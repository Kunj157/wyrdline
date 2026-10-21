//! S3-compatible object storage for bundles (MinIO locally).
//!
//! A small path-style S3 client: AWS Signature Version 4 over `ureq` (the
//! workspace's HTTP client, rustls with ring), run on the blocking pool.
//! The Hub needs only bucket and object put/get/head/delete, so a full SDK
//! (and its TLS stack) is not worth its weight.

use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::{digest, hmac};

use crate::config::S3Config;

/// SHA-256 of the empty string, the payload hash of bodiless requests.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Payload hash for streamed uploads (allowed by S3 over any transport).
const UNSIGNED: &str = "UNSIGNED-PAYLOAD";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("object storage is unreachable: {0}")]
    Unreachable(String),
    #[error("object storage answered {status} to {op}")]
    Status { op: &'static str, status: u16 },
    #[error("object not found")]
    NotFound,
    #[error("local file: {0}")]
    Io(#[from] std::io::Error),
}

/// The bucket the Hub stores into.
#[derive(Debug, Clone)]
pub struct Storage {
    cfg: S3Config,
    agent: ureq::Agent,
}

/// A downloaded object as a blocking reader, with its length.
pub struct Object {
    pub len: Option<u64>,
    pub reader: Box<dyn Read + Send>,
}

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Object")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Storage {
    pub fn new(cfg: S3Config) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Storage { cfg, agent }
    }

    pub fn bucket(&self) -> &str {
        &self.cfg.bucket
    }

    /// Create the bucket unless it exists.
    pub async fn ensure_bucket(&self) -> Result<(), StorageError> {
        if self.ping().await.is_ok() {
            return Ok(());
        }
        let s = self.clone();
        blocking(move || {
            let res = s.call("PUT", "")?;
            match res.status().as_u16() {
                200 | 409 => Ok(()),
                status => Err(StorageError::Status {
                    op: "create bucket",
                    status,
                }),
            }
        })
        .await
    }

    /// The bucket is reachable and ours (`HEAD` bucket).
    pub async fn ping(&self) -> Result<(), StorageError> {
        let s = self.clone();
        blocking(move || {
            let res = s.call("HEAD", "")?;
            expect(res.status().as_u16(), 200, "head bucket")
        })
        .await
    }

    /// Upload a local file to `key`, streamed with its length.
    pub async fn put_file(&self, key: &str, path: PathBuf) -> Result<(), StorageError> {
        let (s, key) = (self.clone(), key.to_string());
        blocking(move || {
            let file = std::fs::File::open(&path)?;
            let (url, headers) = s.sign("PUT", &key, UNSIGNED);
            let res = with_headers(s.agent.put(&url), &headers)
                .header("content-type", "application/octet-stream")
                .send(file)
                .map_err(|e| StorageError::Unreachable(e.to_string()))?;
            expect(res.status().as_u16(), 200, "put object")
        })
        .await
    }

    /// Open `key` for reading.
    pub async fn get(&self, key: &str) -> Result<Object, StorageError> {
        let (s, key) = (self.clone(), key.to_string());
        blocking(move || {
            let res = s.call("GET", &key)?;
            match res.status().as_u16() {
                200 => {
                    let len = res.body().content_length();
                    Ok(Object {
                        len,
                        reader: Box::new(res.into_body().into_reader()),
                    })
                }
                404 => Err(StorageError::NotFound),
                status => Err(StorageError::Status {
                    op: "get object",
                    status,
                }),
            }
        })
        .await
    }

    /// Delete `key` (deleting a missing object succeeds).
    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let (s, key) = (self.clone(), key.to_string());
        blocking(move || {
            let res = s.call("DELETE", &key)?;
            match res.status().as_u16() {
                200 | 204 | 404 => Ok(()),
                status => Err(StorageError::Status {
                    op: "delete object",
                    status,
                }),
            }
        })
        .await
    }

    /// URL and signed headers of one request to `/{bucket}/{key}`.
    fn sign(
        &self,
        method: &str,
        key: &str,
        payload_hash: &str,
    ) -> (String, [(&'static str, String); 3]) {
        let path = if key.is_empty() {
            format!("/{}", self.cfg.bucket)
        } else {
            format!("/{}/{}", self.cfg.bucket, uri_encode(key, false))
        };
        let host = host_of(&self.cfg.endpoint);
        let date = amz_date(SystemTime::now());
        let auth = authorization(
            &SigningKey {
                access_key_id: &self.cfg.access_key_id,
                secret: self.cfg.secret_access_key.expose(),
                region: &self.cfg.region,
            },
            method,
            &path,
            &[
                ("host", host.as_str()),
                ("x-amz-content-sha256", payload_hash),
                ("x-amz-date", date.as_str()),
            ],
            payload_hash,
        );
        (
            format!("{}{path}", self.cfg.endpoint),
            [
                ("x-amz-content-sha256", payload_hash.to_string()),
                ("x-amz-date", date),
                ("authorization", auth),
            ],
        )
    }

    /// A bodiless request (blocking).
    fn call(&self, method: &str, key: &str) -> Result<Response, StorageError> {
        let (url, headers) = self.sign(method, key, EMPTY_SHA256);
        let res = match method {
            "PUT" => with_headers(self.agent.put(&url), &headers).send_empty(),
            "HEAD" => with_headers(self.agent.head(&url), &headers).call(),
            "DELETE" => with_headers(self.agent.delete(&url), &headers).call(),
            _ => with_headers(self.agent.get(&url), &headers).call(),
        };
        res.map_err(|e| StorageError::Unreachable(e.to_string()))
    }
}

type Response = ureq::http::Response<ureq::Body>;

fn with_headers<B>(
    mut req: ureq::RequestBuilder<B>,
    headers: &[(&'static str, String)],
) -> ureq::RequestBuilder<B> {
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    req
}

fn expect(status: u16, want: u16, op: &'static str) -> Result<(), StorageError> {
    if status == want {
        Ok(())
    } else if status == 404 {
        Err(StorageError::NotFound)
    } else {
        Err(StorageError::Status { op, status })
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, StorageError> + Send + 'static,
) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| StorageError::Unreachable(format!("storage task failed: {e}")))?
}

/// `host[:port]` of a base URL.
fn host_of(endpoint: &str) -> String {
    let rest = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Credentials and scope of a signature.
#[derive(Debug, Clone, Copy)]
pub struct SigningKey<'a> {
    pub access_key_id: &'a str,
    pub secret: &'a str,
    pub region: &'a str,
}

/// The `Authorization` header of AWS Signature Version 4 (service `s3`,
/// no query string). `headers` must include `host` and `x-amz-date`
/// (`YYYYMMDDTHHMMSSZ`); every header given is signed.
pub fn authorization(
    key: &SigningKey<'_>,
    method: &str,
    canonical_path: &str,
    headers: &[(&str, &str)],
    payload_hash: &str,
) -> String {
    let mut sorted: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    sorted.sort();
    let amz_date = sorted
        .iter()
        .find(|(k, _)| k == "x-amz-date")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let date = amz_date.get(..8).unwrap_or_default();
    let mut canonical_headers = String::new();
    for (k, v) in &sorted {
        let _ = writeln!(canonical_headers, "{k}:{v}");
    }
    let signed: Vec<&str> = sorted.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical =
        format!("{method}\n{canonical_path}\n\n{canonical_headers}\n{signed}\n{payload_hash}");
    let scope = format!("{date}/{}/s3/aws4_request", key.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(digest::digest(&digest::SHA256, canonical.as_bytes()))
    );
    let mut k = hmac_sha256(format!("AWS4{}", key.secret).as_bytes(), date.as_bytes());
    for part in [key.region, "s3", "aws4_request"] {
        k = hmac_sha256(&k, part.as_bytes());
    }
    let signature = hex::encode(hmac_sha256(&k, to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
        key.access_key_id
    )
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), msg)
        .as_ref()
        .to_vec()
}

/// RFC 3986 percent-encoding as SigV4 wants it (`/` kept unless `slash`).
fn uri_encode(s: &str, slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (b == b'/' && !slash)
        {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `YYYYMMDDTHHMMSSZ` in UTC.
pub fn amz_date(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (H. Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "GET Object" example of the AWS SigV4 documentation for S3
    /// (header-based auth). The example credentials are AWS's published
    /// placeholders, assembled here so no key-shaped literal is committed.
    #[test]
    fn matches_the_published_example() {
        let access = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let secret = format!("wJalrXUtnFEMI/K7MDENG/{}", "bPxRfiCYEXAMPLEKEY");
        let key = SigningKey {
            access_key_id: &access,
            secret: &secret,
            region: "us-east-1",
        };
        let auth = authorization(
            &key,
            "GET",
            "/test.txt",
            &[
                ("Host", "examplebucket.s3.amazonaws.com"),
                ("Range", "bytes=0-9"),
                ("x-amz-content-sha256", EMPTY_SHA256),
                ("x-amz-date", "20130524T000000Z"),
            ],
            EMPTY_SHA256,
        );
        assert_eq!(
            auth,
            format!(
                "AWS4-HMAC-SHA256 Credential={access}/20130524/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
                 Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            )
        );
    }

    #[test]
    fn dates_and_encoding() {
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_369_353_600);
        assert_eq!(amz_date(t), "20130524T000000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_799);
        assert_eq!(amz_date(leap), "20240228T235959Z");
        assert_eq!(uri_encode("a b/c+d~", false), "a%20b/c%2Bd~");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(host_of("http://127.0.0.1:7733"), "127.0.0.1:7733");
        assert_eq!(host_of("https://s3.example.com/base"), "s3.example.com");
    }
}

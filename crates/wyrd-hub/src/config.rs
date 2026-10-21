//! Hub configuration, read from the environment (`docs/architecture/hub.md`).

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Default listen address: loopback, so a bare `wyrd-hub` is never exposed
/// by accident; containers set `WYRD_HUB_LISTEN=0.0.0.0:7730`.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7730";

/// A value that must never reach a log line: `Debug` prints a placeholder.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// S3-compatible object storage (MinIO locally).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// Base URL, e.g. `http://127.0.0.1:7733`. Path-style addressing.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: Secret,
}

/// The OpenID Connect provider the Hub logs users in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: SocketAddr,
    /// The URL users reach the Hub at (OIDC redirects, cookie security).
    pub public_url: String,
    pub database_url: Secret,
    pub db_max_connections: u32,
    pub s3: S3Config,
    /// `None`: login is disabled (API tokens only).
    pub oidc: Option<OidcConfig>,
    /// Local scratch: upload spooling and analytics projections.
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

impl Config {
    /// Read the process environment.
    pub fn from_env() -> Result<Config, ConfigError> {
        Config::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read configuration through `get` (a test passes a map).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let var = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let required = |k: &'static str| var(k).ok_or(ConfigError::Missing(k));

        let listen_raw = var("WYRD_HUB_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.into());
        let listen =
            listen_raw
                .parse()
                .map_err(|e: std::net::AddrParseError| ConfigError::Invalid {
                    name: "WYRD_HUB_LISTEN",
                    reason: e.to_string(),
                })?;
        let public_url = var("WYRD_HUB_PUBLIC_URL")
            .unwrap_or_else(|| format!("http://{listen}"))
            .trim_end_matches('/')
            .to_string();
        check_url("WYRD_HUB_PUBLIC_URL", &public_url)?;
        let db_max_connections = match var("WYRD_HUB_DB_MAX_CONNECTIONS") {
            None => 10,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=1000).contains(n))
                .ok_or(ConfigError::Invalid {
                    name: "WYRD_HUB_DB_MAX_CONNECTIONS",
                    reason: "expected a number from 1 to 1000".into(),
                })?,
        };
        let endpoint = required("WYRD_HUB_S3_ENDPOINT")?
            .trim_end_matches('/')
            .to_string();
        check_url("WYRD_HUB_S3_ENDPOINT", &endpoint)?;
        let bucket = required("WYRD_HUB_S3_BUCKET")?;
        if !valid_bucket(&bucket) {
            return Err(ConfigError::Invalid {
                name: "WYRD_HUB_S3_BUCKET",
                reason: "3-63 lowercase letters, digits, dots, or hyphens".into(),
            });
        }
        let s3 = S3Config {
            endpoint,
            bucket,
            region: var("WYRD_HUB_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            access_key_id: required("WYRD_HUB_S3_ACCESS_KEY_ID")?,
            secret_access_key: Secret::new(required("WYRD_HUB_S3_SECRET_ACCESS_KEY")?),
        };
        let oidc = match var("WYRD_HUB_OIDC_ISSUER") {
            None => None,
            Some(issuer) => {
                let issuer = issuer.trim_end_matches('/').to_string();
                check_url("WYRD_HUB_OIDC_ISSUER", &issuer)?;
                Some(OidcConfig {
                    issuer,
                    client_id: required("WYRD_HUB_OIDC_CLIENT_ID")?,
                    client_secret: Secret::new(required("WYRD_HUB_OIDC_CLIENT_SECRET")?),
                })
            }
        };
        Ok(Config {
            listen,
            public_url,
            database_url: Secret::new(required("WYRD_HUB_DATABASE_URL")?),
            db_max_connections,
            s3,
            oidc,
            data_dir: var("WYRD_HUB_DATA_DIR")
                .map_or_else(|| "wyrd-hub-data".into(), PathBuf::from),
        })
    }
}

fn check_url(name: &'static str, url: &str) -> Result<(), ConfigError> {
    let ok = (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() > "https://".len()
        && !url.contains(char::is_whitespace);
    if ok {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            name,
            reason: "expected an http:// or https:// URL".into(),
        })
    }
}

fn valid_bucket(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
        && b.as_bytes()[0].is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn base() -> HashMap<&'static str, String> {
        [
            ("WYRD_HUB_DATABASE_URL", "postgres://hub@localhost/hub"),
            ("WYRD_HUB_S3_ENDPOINT", "http://127.0.0.1:7733/"),
            ("WYRD_HUB_S3_BUCKET", "wyrd-bundles"),
            ("WYRD_HUB_S3_ACCESS_KEY_ID", "dev"),
            ("WYRD_HUB_S3_SECRET_ACCESS_KEY", "dev-placeholder"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect()
    }

    fn load(m: &HashMap<&'static str, String>) -> Result<Config, ConfigError> {
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_and_trimming() {
        let c = load(&base()).unwrap();
        assert_eq!(c.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(c.public_url, "http://127.0.0.1:7730");
        assert_eq!(c.s3.endpoint, "http://127.0.0.1:7733");
        assert_eq!(c.s3.region, "us-east-1");
        assert_eq!(c.db_max_connections, 10);
        assert!(c.oidc.is_none());
    }

    #[test]
    fn missing_and_invalid_values_are_named() {
        for k in [
            "WYRD_HUB_DATABASE_URL",
            "WYRD_HUB_S3_ENDPOINT",
            "WYRD_HUB_S3_BUCKET",
            "WYRD_HUB_S3_ACCESS_KEY_ID",
            "WYRD_HUB_S3_SECRET_ACCESS_KEY",
        ] {
            let mut m = base();
            m.insert(k, "  ".into());
            assert_eq!(load(&m), Err(ConfigError::Missing(k)));
        }
        for (k, v) in [
            ("WYRD_HUB_LISTEN", "nope"),
            ("WYRD_HUB_S3_BUCKET", "Bad_Bucket"),
            ("WYRD_HUB_S3_ENDPOINT", "minio:9000"),
            ("WYRD_HUB_DB_MAX_CONNECTIONS", "0"),
            ("WYRD_HUB_OIDC_ISSUER", "ftp://idp"),
        ] {
            let mut m = base();
            m.insert(k, v.into());
            assert!(
                matches!(load(&m), Err(ConfigError::Invalid { name, .. }) if name == k),
                "{k}={v}"
            );
        }
    }

    #[test]
    fn oidc_needs_a_client() {
        let mut m = base();
        m.insert("WYRD_HUB_OIDC_ISSUER", "http://127.0.0.1:7735/dex/".into());
        assert_eq!(
            load(&m),
            Err(ConfigError::Missing("WYRD_HUB_OIDC_CLIENT_ID"))
        );
        m.insert("WYRD_HUB_OIDC_CLIENT_ID", "wyrd-hub".into());
        m.insert("WYRD_HUB_OIDC_CLIENT_SECRET", "dev-placeholder".into());
        let c = load(&m).unwrap();
        assert_eq!(c.oidc.unwrap().issuer, "http://127.0.0.1:7735/dex");
    }

    #[test]
    fn secrets_never_print() {
        let c = load(&base()).unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("dev-placeholder"), "{shown}");
        assert!(!shown.contains("postgres://"), "{shown}");
    }
}

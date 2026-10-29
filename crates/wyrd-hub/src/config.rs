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

/// The GitHub Actions OIDC issuer whose tokens `POST /api/v1/ci/token`
/// exchanges for upload tokens (`docs/architecture/hub.md`).
pub const DEFAULT_CI_ISSUER: &str = "https://token.actions.githubusercontent.com";
/// Default lifetime of a CI upload token, in seconds.
pub const DEFAULT_CI_TOKEN_TTL_SECS: i64 = 900;

/// CI token exchange: which issuer is trusted, which audience its tokens
/// must name, and how long the upload tokens live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiConfig {
    /// `https://token.actions.githubusercontent.com`, or a GitHub
    /// Enterprise Server's `https://HOST/_services/token`.
    pub issuer: String,
    /// The `aud` claim workflows request (default: the public URL).
    pub audience: String,
    /// 60–3600.
    pub token_ttl_secs: i64,
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
    pub ci: CiConfig,
    /// Local scratch: upload spooling and analytics projections.
    pub data_dir: PathBuf,
    pub limits: Limits,
    /// A built Studio to serve at `/` (Hub mode pages), when set.
    pub studio_dir: Option<PathBuf>,
}

/// Upload size, the quota new organisations get, and the retention sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest bundle accepted, in bytes (refused while streaming).
    pub max_upload_bytes: u64,
    pub quota_bytes: i64,
    pub quota_bundles: i64,
    /// Seconds between retention sweeps; 0 disables the sweep.
    pub retention_interval_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_upload_bytes: 256 << 20,
            quota_bytes: 10 << 30,
            quota_bundles: 10_000,
            retention_interval_secs: 3600,
        }
    }
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
    #[allow(clippy::too_many_lines)] // one line per setting, read top to bottom
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
        let number = |name: &'static str, default: u64| -> Result<u64, ConfigError> {
            match var(name) {
                None => Ok(default),
                Some(v) => v.parse::<u64>().map_err(|_| ConfigError::Invalid {
                    name,
                    reason: "expected a whole number".into(),
                }),
            }
        };
        let d = Limits::default();
        let signed = |name: &'static str, v: u64| {
            i64::try_from(v).map_err(|_| ConfigError::Invalid {
                name,
                reason: "too large".into(),
            })
        };
        let limits = Limits {
            max_upload_bytes: number("WYRD_HUB_MAX_UPLOAD_BYTES", d.max_upload_bytes)?,
            quota_bytes: signed(
                "WYRD_HUB_QUOTA_BYTES",
                number("WYRD_HUB_QUOTA_BYTES", d.quota_bytes.unsigned_abs())?,
            )?,
            quota_bundles: signed(
                "WYRD_HUB_QUOTA_BUNDLES",
                number("WYRD_HUB_QUOTA_BUNDLES", d.quota_bundles.unsigned_abs())?,
            )?,
            retention_interval_secs: number(
                "WYRD_HUB_RETENTION_INTERVAL_SECS",
                d.retention_interval_secs,
            )?,
        };
        let ci = load_ci(&var, &public_url)?;
        Ok(Config {
            listen,
            public_url,
            database_url: Secret::new(required("WYRD_HUB_DATABASE_URL")?),
            db_max_connections,
            s3,
            oidc,
            ci,
            data_dir: var("WYRD_HUB_DATA_DIR")
                .map_or_else(|| "wyrd-hub-data".into(), PathBuf::from),
            limits,
            studio_dir: var("WYRD_HUB_STUDIO_DIR").map(PathBuf::from),
        })
    }
}

/// The CI token exchange settings (`WYRD_HUB_CI_*`).
fn load_ci(
    var: &impl Fn(&str) -> Option<String>,
    public_url: &str,
) -> Result<CiConfig, ConfigError> {
    let ci_issuer = var("WYRD_HUB_CI_ISSUER")
        .unwrap_or_else(|| DEFAULT_CI_ISSUER.into())
        .trim_end_matches('/')
        .to_string();
    check_url("WYRD_HUB_CI_ISSUER", &ci_issuer)?;
    let token_ttl_secs = match var("WYRD_HUB_CI_TOKEN_TTL_SECS") {
        None => DEFAULT_CI_TOKEN_TTL_SECS,
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (60..=3600).contains(n))
            .ok_or(ConfigError::Invalid {
                name: "WYRD_HUB_CI_TOKEN_TTL_SECS",
                reason: "expected a number of seconds from 60 to 3600".into(),
            })?,
    };
    Ok(CiConfig {
        issuer: ci_issuer,
        audience: var("WYRD_HUB_CI_AUDIENCE").unwrap_or_else(|| public_url.to_string()),
        token_ttl_secs,
    })
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
        assert_eq!(c.limits, Limits::default());
        assert!(c.oidc.is_none());
        assert_eq!(c.ci.issuer, DEFAULT_CI_ISSUER);
        assert_eq!(c.ci.audience, "http://127.0.0.1:7730");
        assert_eq!(c.ci.token_ttl_secs, DEFAULT_CI_TOKEN_TTL_SECS);
    }

    #[test]
    fn ci_exchange_is_configurable() {
        let mut m = base();
        m.insert(
            "WYRD_HUB_CI_ISSUER",
            "https://ghes.example/_services/token/".into(),
        );
        m.insert("WYRD_HUB_CI_AUDIENCE", "wyrd-hub".into());
        m.insert("WYRD_HUB_CI_TOKEN_TTL_SECS", "120".into());
        let c = load(&m).unwrap();
        assert_eq!(c.ci.issuer, "https://ghes.example/_services/token");
        assert_eq!(c.ci.audience, "wyrd-hub");
        assert_eq!(c.ci.token_ttl_secs, 120);
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
            ("WYRD_HUB_MAX_UPLOAD_BYTES", "-1"),
            ("WYRD_HUB_QUOTA_BYTES", "18446744073709551615"),
            ("WYRD_HUB_CI_ISSUER", "token.actions.githubusercontent.com"),
            ("WYRD_HUB_CI_TOKEN_TTL_SECS", "30"),
            ("WYRD_HUB_CI_TOKEN_TTL_SECS", "86400"),
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

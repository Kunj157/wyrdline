//! The OpenID Connect provider: discovery, JWKS with rotation, and the
//! authorization-code exchange (with PKCE). HTTP calls run on the
//! blocking pool through `ureq`.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::OidcConfig;
use crate::jwt::{self, Identity, Jwks, JwtError};

/// How long discovery and keys are cached.
const CACHE_FOR: Duration = Duration::from_secs(3600);
/// The shortest interval between two key refreshes on an unknown `kid`.
const REFRESH_AT_MOST_EVERY: Duration = Duration::from_secs(30);
/// Largest provider response accepted.
const MAX_BODY: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("identity provider unreachable: {0}")]
    Unreachable(String),
    #[error("identity provider answered {0}")]
    Status(u16),
    #[error("identity provider sent an invalid response: {0}")]
    Invalid(String),
    #[error("ID token rejected: {0}")]
    Token(#[from] JwtError),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

#[derive(Debug, Default)]
struct Cache {
    discovery: Option<(Discovery, Instant)>,
    jwks: Option<(Jwks, Instant)>,
}

/// One configured provider.
#[derive(Debug)]
pub struct Provider {
    pub config: OidcConfig,
    agent: ureq::Agent,
    cache: Mutex<Cache>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

impl Provider {
    pub fn new(config: OidcConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        Provider {
            config,
            agent,
            cache: Mutex::new(Cache::default()),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        url: String,
    ) -> Result<T, OidcError> {
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || {
            let mut res = agent
                .get(&url)
                .header("accept", "application/json")
                .call()
                .map_err(|e| OidcError::Unreachable(e.to_string()))?;
            let status = res.status().as_u16();
            if status != 200 {
                return Err(OidcError::Status(status));
            }
            let body = res
                .body_mut()
                .with_config()
                .limit(MAX_BODY)
                .read_to_vec()
                .map_err(|e| OidcError::Invalid(e.to_string()))?;
            serde_json::from_slice(&body).map_err(|e| OidcError::Invalid(e.to_string()))
        })
        .await
        .map_err(|e| OidcError::Unreachable(e.to_string()))?
    }

    /// The provider metadata (cached; its issuer must equal the configured one).
    pub async fn discovery(&self) -> Result<Discovery, OidcError> {
        let mut cache = self.cache.lock().await;
        if let Some((d, at)) = &cache.discovery
            && at.elapsed() < CACHE_FOR
        {
            return Ok(d.clone());
        }
        let url = format!("{}/.well-known/openid-configuration", self.config.issuer);
        let d: Discovery = self.get_json(url).await?;
        if d.issuer.trim_end_matches('/') != self.config.issuer {
            return Err(OidcError::Invalid(format!(
                "discovery names issuer {:?}",
                d.issuer
            )));
        }
        cache.discovery = Some((d.clone(), Instant::now()));
        Ok(d)
    }

    async fn jwks(&self, refresh: bool) -> Result<Jwks, OidcError> {
        let d = self.discovery().await?;
        let mut cache = self.cache.lock().await;
        if let Some((k, at)) = &cache.jwks {
            let fresh = at.elapsed() < CACHE_FOR;
            let recent = at.elapsed() < REFRESH_AT_MOST_EVERY;
            if (fresh && !refresh) || recent {
                return Ok(k.clone());
            }
        }
        let keys: Jwks = self.get_json(d.jwks_uri).await?;
        cache.jwks = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    /// Verify an ID token; an unknown key triggers one JWKS refresh (key
    /// rotation), rate-limited.
    pub async fn verify_id_token(
        &self,
        token: &str,
        nonce: &str,
        now: i64,
    ) -> Result<Identity, OidcError> {
        let claims = match jwt::verify(token, &self.jwks(false).await?) {
            Err(JwtError::UnknownKey) => jwt::verify(token, &self.jwks(true).await?)?,
            other => other?,
        };
        Ok(jwt::check_id_token(
            &claims,
            &self.config.issuer,
            &self.config.client_id,
            nonce,
            now,
        )?)
    }

    /// The authorization request URL.
    pub async fn authorize_url(
        &self,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        challenge: &str,
    ) -> Result<String, OidcError> {
        let d = self.discovery().await?;
        let sep = if d.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(format!(
            "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}\
             &code_challenge={}&code_challenge_method=S256",
            d.authorization_endpoint,
            enc(&self.config.client_id),
            enc(redirect_uri),
            enc("openid email profile"),
            enc(state),
            enc(nonce),
            enc(challenge),
        ))
    }

    /// Exchange an authorization code for the ID token (client secret
    /// sent with HTTP basic authentication).
    pub async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<String, OidcError> {
        let d = self.discovery().await?;
        let form = format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}",
            enc(code),
            enc(redirect_uri),
            enc(verifier)
        );
        let basic = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            enc(&self.config.client_id),
            enc(self.config.client_secret.expose())
        ));
        let agent = self.agent.clone();
        let res: TokenResponse = tokio::task::spawn_blocking(move || {
            let mut res = agent
                .post(&d.token_endpoint)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("accept", "application/json")
                .header("authorization", &format!("Basic {basic}"))
                .send(form)
                .map_err(|e| OidcError::Unreachable(e.to_string()))?;
            let status = res.status().as_u16();
            if status != 200 {
                return Err(OidcError::Status(status));
            }
            let body = res
                .body_mut()
                .with_config()
                .limit(MAX_BODY)
                .read_to_vec()
                .map_err(|e| OidcError::Invalid(e.to_string()))?;
            serde_json::from_slice(&body).map_err(|e| OidcError::Invalid(e.to_string()))
        })
        .await
        .map_err(|e| OidcError::Unreachable(e.to_string()))??;
        res.id_token
            .ok_or_else(|| OidcError::Invalid("no id_token".into()))
    }
}

/// `S256` code challenge of a PKCE verifier.
pub fn challenge(verifier: &str) -> String {
    B64.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// `application/x-www-form-urlencoded` / query component encoding.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B.
    #[test]
    fn pkce_s256_matches_the_rfc() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn encoding() {
        assert_eq!(enc("openid email"), "openid%20email");
        assert_eq!(enc("http://a/b?c=d"), "http%3A%2F%2Fa%2Fb%3Fc%3Dd");
    }
}

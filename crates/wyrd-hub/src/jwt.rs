//! JSON Web Tokens signed by an identity provider: signature verification
//! against a JWK set (RS256, ES256, EdDSA with ring) and ID-token checks.
//! `none`, HMAC, and any other algorithm are refused, and the key type
//! must match the algorithm (no algorithm confusion).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::signature::{self, UnparsedPublicKey};
use serde::Deserialize;

/// Clock skew tolerated on `exp` and `iat`, in seconds.
pub const LEEWAY_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported algorithm {0:?}")]
    Algorithm(String),
    #[error("no matching key")]
    UnknownKey,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer")]
    Issuer,
    #[error("wrong audience")]
    Audience,
    #[error("expired")]
    Expired,
    #[error("issued in the future")]
    NotYetValid,
    #[error("nonce mismatch")]
    Nonce,
    #[error("missing claim {0}")]
    Missing(&'static str),
}

/// One key of a JWK set (only the fields used for verification).
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

/// The key type an algorithm needs.
fn kty_for(alg: &str) -> Result<&'static str, JwtError> {
    match alg {
        "RS256" => Ok("RSA"),
        "ES256" => Ok("EC"),
        "EdDSA" => Ok("OKP"),
        other => Err(JwtError::Algorithm(other.to_string())),
    }
}

fn b64(s: Option<&String>) -> Result<Vec<u8>, JwtError> {
    s.and_then(|s| B64.decode(s).ok())
        .ok_or(JwtError::UnknownKey)
}

/// Verify `alg` signature `sig` over `msg` with `jwk`.
fn verify_with(alg: &str, jwk: &Jwk, msg: &[u8], sig: &[u8]) -> Result<(), JwtError> {
    let ok = match alg {
        "RS256" => {
            let (n, e) = (b64(jwk.n.as_ref())?, b64(jwk.e.as_ref())?);
            signature::RsaPublicKeyComponents { n: &n, e: &e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                .is_ok()
        }
        "ES256" => {
            if jwk.crv.as_deref() != Some("P-256") {
                return Err(JwtError::UnknownKey);
            }
            let (x, y) = (b64(jwk.x.as_ref())?, b64(jwk.y.as_ref())?);
            if x.len() != 32 || y.len() != 32 {
                return Err(JwtError::UnknownKey);
            }
            let mut point = Vec::with_capacity(65);
            point.push(4);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(msg, sig)
                .is_ok()
        }
        "EdDSA" => {
            if jwk.crv.as_deref() != Some("Ed25519") {
                return Err(JwtError::UnknownKey);
            }
            UnparsedPublicKey::new(&signature::ED25519, b64(jwk.x.as_ref())?)
                .verify(msg, sig)
                .is_ok()
        }
        other => return Err(JwtError::Algorithm(other.to_string())),
    };
    if ok { Ok(()) } else { Err(JwtError::Signature) }
}

/// Check the signature of a compact JWS against `keys` and return its
/// claims. Nothing in the claims is checked here.
pub fn verify(token: &str, keys: &Jwks) -> Result<serde_json::Value, JwtError> {
    if token.len() > 16 * 1024 {
        return Err(JwtError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Header = B64
        .decode(h)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(JwtError::Malformed)?;
    let kty = kty_for(&header.alg)?;
    let sig = B64.decode(s).map_err(|_| JwtError::Malformed)?;
    let candidates: Vec<&Jwk> = keys
        .keys
        .iter()
        .filter(|k| k.kty == kty)
        .filter(|k| k.use_.as_deref().is_none_or(|u| u == "sig"))
        .filter(|k| k.alg.as_deref().is_none_or(|a| a == header.alg))
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    let [jwk] = candidates.as_slice() else {
        return Err(JwtError::UnknownKey);
    };
    let msg_len = h.len() + 1 + p.len();
    verify_with(&header.alg, jwk, &token.as_bytes()[..msg_len], &sig)?;
    B64.decode(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(serde_json::Value::is_object)
        .ok_or(JwtError::Malformed)
}

/// The identity an ID token asserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// OpenID Connect Core §3.1.3.7 checks on verified ID-token claims.
pub fn check_id_token(
    claims: &serde_json::Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<Identity, JwtError> {
    let s = |k: &'static str| claims.get(k).and_then(serde_json::Value::as_str);
    if s("iss") != Some(issuer) {
        return Err(JwtError::Issuer);
    }
    let aud_ok = match claims.get("aud") {
        Some(serde_json::Value::String(a)) => a == client_id,
        Some(serde_json::Value::Array(a)) => {
            a.iter().any(|v| v.as_str() == Some(client_id))
                && (a.len() == 1 || s("azp") == Some(client_id))
        }
        _ => false,
    };
    if !aud_ok {
        return Err(JwtError::Audience);
    }
    let exp = claims
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or(JwtError::Missing("exp"))?;
    if exp + LEEWAY_SECS < now {
        return Err(JwtError::Expired);
    }
    if let Some(iat) = claims.get("iat").and_then(serde_json::Value::as_i64)
        && iat - LEEWAY_SECS > now
    {
        return Err(JwtError::NotYetValid);
    }
    if s("nonce") != Some(nonce) {
        return Err(JwtError::Nonce);
    }
    let subject = s("sub")
        .filter(|v| !v.is_empty() && v.len() <= 255)
        .ok_or(JwtError::Missing("sub"))?;
    let short = |k| s(k).filter(|v| v.len() <= 320).map(str::to_string);
    Ok(Identity {
        issuer: issuer.to_string(),
        subject: subject.to_string(),
        email: short("email"),
        name: short("name").or_else(|| short("preferred_username")),
    })
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

/// The workflow run a GitHub Actions OIDC token asserts
/// (`docs/architecture/hub.md`, "CI upload tokens").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiIdentity {
    /// `owner/name`, as GitHub spells it.
    pub repository: String,
    pub repository_owner: String,
    pub repository_id: Option<String>,
    pub repository_owner_id: Option<String>,
    /// The GitHub user who triggered the run.
    pub actor: Option<String>,
    pub workflow_ref: Option<String>,
    pub run_id: Option<String>,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    pub event_name: Option<String>,
}

/// Checks on verified GitHub Actions token claims: issuer, audience
/// (a string, or a list naming `audience`), expiry, not-before, and the
/// repository claims a trust rule is matched against.
pub fn check_ci_token(
    claims: &serde_json::Value,
    issuer: &str,
    audience: &str,
    now: i64,
) -> Result<CiIdentity, JwtError> {
    let s = |k: &'static str| claims.get(k).and_then(serde_json::Value::as_str);
    if s("iss") != Some(issuer) {
        return Err(JwtError::Issuer);
    }
    let aud_ok = match claims.get("aud") {
        Some(serde_json::Value::String(a)) => a == audience,
        Some(serde_json::Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(audience)),
        _ => false,
    };
    if !aud_ok {
        return Err(JwtError::Audience);
    }
    let exp = claims
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or(JwtError::Missing("exp"))?;
    if exp + LEEWAY_SECS < now {
        return Err(JwtError::Expired);
    }
    for k in ["iat", "nbf"] {
        if let Some(t) = claims.get(k).and_then(serde_json::Value::as_i64)
            && t - LEEWAY_SECS > now
        {
            return Err(JwtError::NotYetValid);
        }
    }
    let short = |k: &'static str| s(k).filter(|v| !v.is_empty() && v.len() <= 320);
    let repository = short("repository").ok_or(JwtError::Missing("repository"))?;
    let repository_owner =
        short("repository_owner").ok_or(JwtError::Missing("repository_owner"))?;
    if !repository
        .strip_prefix(repository_owner)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
    {
        return Err(JwtError::Missing("repository"));
    }
    let opt = |k| short(k).map(str::to_string);
    Ok(CiIdentity {
        repository: repository.to_string(),
        repository_owner: repository_owner.to_string(),
        repository_id: opt("repository_id"),
        repository_owner_id: opt("repository_owner_id"),
        actor: opt("actor"),
        workflow_ref: opt("workflow_ref").or_else(|| opt("job_workflow_ref")),
        run_id: opt("run_id"),
        git_ref: opt("ref"),
        sha: opt("sha"),
        event_name: opt("event_name"),
    })
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, Ed25519KeyPair, KeyPair};
    use serde_json::json;

    use super::*;

    fn enc(v: &serde_json::Value) -> String {
        B64.encode(serde_json::to_vec(v).unwrap())
    }

    fn ed25519() -> (Ed25519KeyPair, Jwk) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let jwk = serde_json::from_value(json!({
            "kty": "OKP", "crv": "Ed25519", "kid": "k1",
            "x": B64.encode(kp.public_key().as_ref())
        }))
        .unwrap();
        (kp, jwk)
    }

    fn sign_ed(
        kp: &Ed25519KeyPair,
        header: &serde_json::Value,
        claims: &serde_json::Value,
    ) -> String {
        let msg = format!("{}.{}", enc(header), enc(claims));
        format!("{msg}.{}", B64.encode(kp.sign(msg.as_bytes()).as_ref()))
    }

    #[test]
    fn eddsa_round_trip_and_tamper() {
        let (kp, jwk) = ed25519();
        let keys = Jwks { keys: vec![jwk] };
        let t = sign_ed(
            &kp,
            &json!({"alg": "EdDSA", "kid": "k1"}),
            &json!({"sub": "a"}),
        );
        assert_eq!(verify(&t, &keys).unwrap()["sub"], "a");
        let mut parts: Vec<&str> = t.split('.').collect();
        let forged = enc(&json!({"sub": "admin"}));
        parts[1] = &forged;
        assert_eq!(verify(&parts.join("."), &keys), Err(JwtError::Signature));
        let unknown = sign_ed(&kp, &json!({"alg": "EdDSA", "kid": "k2"}), &json!({}));
        assert_eq!(verify(&unknown, &keys), Err(JwtError::UnknownKey));
    }

    #[test]
    fn es256_round_trip() {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
        let point = kp.public_key().as_ref();
        let jwk = serde_json::from_value(json!({
            "kty": "EC", "crv": "P-256",
            "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..])
        }))
        .unwrap();
        let msg = format!(
            "{}.{}",
            enc(&json!({"alg": "ES256"})),
            enc(&json!({"sub": "e"}))
        );
        let sig = kp.sign(&rng, msg.as_bytes()).unwrap();
        let t = format!("{msg}.{}", B64.encode(sig.as_ref()));
        assert_eq!(verify(&t, &Jwks { keys: vec![jwk] }).unwrap()["sub"], "e");
    }

    /// RFC 7515 Appendix A.2: an RS256 JWS and its public key.
    #[test]
    fn rs256_rfc7515_example() {
        let jwk = serde_json::from_value(json!({
            "kty": "RSA",
            "n": "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp-Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-pCgNMsD1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xsmtdVMTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ",
            "e": "AQAB"
        }))
        .unwrap();
        // Assembled from its parts so no token-shaped literal is committed.
        let t = &[
            "eyJhbGciOiJSUzI1NiJ9",
            "eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ",
            "cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw",
        ]
        .join(".");
        let claims = verify(t, &Jwks { keys: vec![jwk] }).unwrap();
        assert_eq!(claims["iss"], "joe");
    }

    #[test]
    fn refuses_none_hmac_and_key_type_confusion() {
        let (kp, jwk) = ed25519();
        let keys = Jwks { keys: vec![jwk] };
        for alg in ["none", "HS256", "RS512"] {
            let t = format!("{}.{}.", enc(&json!({"alg": alg})), enc(&json!({})));
            assert!(
                matches!(verify(&t, &keys), Err(JwtError::Algorithm(_))),
                "{alg}"
            );
        }
        // An Ed25519 signature presented as RS256 finds no RSA key.
        let t = sign_ed(&kp, &json!({"alg": "RS256"}), &json!({}));
        assert_eq!(verify(&t, &keys), Err(JwtError::UnknownKey));
        assert_eq!(verify("a.b", &keys), Err(JwtError::Malformed));
        assert_eq!(verify("a.b.c.d", &keys), Err(JwtError::Malformed));
    }

    #[test]
    fn id_token_checks() {
        let now = 1_000_000;
        let ok = json!({
            "iss": "https://idp", "aud": "hub", "sub": "u1", "nonce": "n",
            "exp": now + 300, "iat": now, "email": "a@example.com", "name": "A"
        });
        let id = check_id_token(&ok, "https://idp", "hub", "n", now).unwrap();
        assert_eq!(id.subject, "u1");
        assert_eq!(id.email.as_deref(), Some("a@example.com"));
        let with = |k: &str, v: serde_json::Value| {
            let mut c = ok.clone();
            c[k] = v;
            check_id_token(&c, "https://idp", "hub", "n", now)
        };
        assert_eq!(with("iss", json!("https://evil")), Err(JwtError::Issuer));
        assert_eq!(with("aud", json!("other")), Err(JwtError::Audience));
        assert_eq!(
            with("aud", json!(["hub", "other"])),
            Err(JwtError::Audience)
        );
        assert!(with("aud", json!(["hub"])).is_ok());
        assert_eq!(with("exp", json!(now - 61)), Err(JwtError::Expired));
        assert!(with("exp", json!(now - 59)).is_ok(), "leeway");
        assert_eq!(with("iat", json!(now + 120)), Err(JwtError::NotYetValid));
        assert_eq!(with("nonce", json!("other")), Err(JwtError::Nonce));
        assert_eq!(with("sub", json!("")), Err(JwtError::Missing("sub")));
    }

    #[test]
    fn ci_token_checks() {
        let now = 1_000_000;
        let iss = "https://token.actions.githubusercontent.com";
        let ok = json!({
            "iss": iss, "aud": "https://hub.example", "sub": "repo:acme/app:ref:refs/heads/main",
            "exp": now + 300, "iat": now, "nbf": now - 5,
            "repository": "acme/App", "repository_owner": "acme",
            "repository_id": "123", "repository_owner_id": "77",
            "actor": "octocat", "workflow_ref": "acme/App/.github/workflows/ci.yml@refs/heads/main",
            "run_id": "42", "ref": "refs/heads/main", "sha": "abc", "event_name": "pull_request"
        });
        let id = check_ci_token(&ok, iss, "https://hub.example", now).unwrap();
        assert_eq!(id.repository, "acme/App");
        assert_eq!(id.repository_owner, "acme");
        assert_eq!(id.repository_id.as_deref(), Some("123"));
        assert_eq!(id.actor.as_deref(), Some("octocat"));
        assert_eq!(id.run_id.as_deref(), Some("42"));
        let with = |k: &str, v: serde_json::Value| {
            let mut c = ok.clone();
            c[k] = v;
            check_ci_token(&c, iss, "https://hub.example", now)
        };
        assert_eq!(with("iss", json!("https://evil")), Err(JwtError::Issuer));
        assert_eq!(with("aud", json!("https://other")), Err(JwtError::Audience));
        assert!(with("aud", json!(["x", "https://hub.example"])).is_ok());
        assert_eq!(with("aud", json!(7)), Err(JwtError::Audience));
        assert_eq!(with("exp", json!(now - 61)), Err(JwtError::Expired));
        assert_eq!(with("exp", json!(null)), Err(JwtError::Missing("exp")));
        assert_eq!(with("nbf", json!(now + 120)), Err(JwtError::NotYetValid));
        assert_eq!(with("iat", json!(now + 120)), Err(JwtError::NotYetValid));
        assert_eq!(
            with("repository", json!(null)),
            Err(JwtError::Missing("repository"))
        );
        assert_eq!(
            with("repository_owner", json!("")),
            Err(JwtError::Missing("repository_owner"))
        );
        // The repository must belong to the owner claim.
        assert_eq!(
            with("repository", json!("other/App")),
            Err(JwtError::Missing("repository"))
        );
        assert_eq!(
            with("repository", json!("acme")),
            Err(JwtError::Missing("repository"))
        );
        // Optional claims may be absent.
        let mut bare = ok.clone();
        for k in [
            "actor",
            "workflow_ref",
            "run_id",
            "ref",
            "sha",
            "event_name",
        ] {
            bare.as_object_mut().unwrap().remove(k);
        }
        let id = check_ci_token(&bare, iss, "https://hub.example", now).unwrap();
        assert_eq!(id.actor, None);
        assert_eq!(id.workflow_ref, None);
    }
}

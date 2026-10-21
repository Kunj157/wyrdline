//! A stand-in for GitHub's Actions token issuer: discovery (no login
//! endpoints, like the real one), an Ed25519 JWKS that can rotate or go
//! down, and workflow tokens with the claims GitHub puts in them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;

/// How a token is spoiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spoil {
    #[default]
    None,
    Audience,
    Issuer,
    Expired,
    NotYetValid,
    /// Signed by a key the JWKS never served.
    Signature,
    /// A repository claim that does not belong to the owner claim.
    ForeignRepository,
}

pub struct Actions {
    pub issuer: String,
    pub audience: String,
    key: Mutex<(String, Ed25519KeyPair)>,
    other_key: Ed25519KeyPair,
    /// The JWKS endpoint answers 500.
    pub keys_down: AtomicBool,
}

impl std::fmt::Debug for Actions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Actions")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

fn keypair() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

impl Actions {
    /// Replace the signing key (and its `kid`), as GitHub does periodically.
    pub fn rotate(&self) {
        let mut k = self.key.lock().unwrap();
        *k = (super::throwaway(), keypair());
    }

    /// A token for a workflow run of `repository` (`owner/name`).
    pub fn token(&self, repository: &str, spoil: Spoil) -> String {
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let owner = repository.split('/').next().unwrap();
        let claims = json!({
            "iss": if spoil == Spoil::Issuer { "https://evil.test" } else { self.issuer.as_str() },
            "aud": if spoil == Spoil::Audience { "https://someone-else.test" } else { self.audience.as_str() },
            "sub": format!("repo:{repository}:ref:refs/heads/main"),
            "exp": if spoil == Spoil::Expired { now - 3600 } else { now + 300 },
            "iat": now,
            "nbf": if spoil == Spoil::NotYetValid { now + 3600 } else { now - 5 },
            "jti": super::throwaway(),
            "repository": if spoil == Spoil::ForeignRepository { "someone/else".to_string() } else { repository.to_string() },
            "repository_owner": owner,
            "repository_id": "123456",
            "repository_owner_id": "7890",
            "actor": "octocat",
            "workflow_ref": format!("{repository}/.github/workflows/agent.yml@refs/heads/main"),
            "run_id": "987654321",
            "ref": "refs/heads/main",
            "sha": "0123456789abcdef0123456789abcdef01234567",
            "event_name": "pull_request",
        });
        let key = self.key.lock().unwrap();
        let header = json!({"alg": "EdDSA", "kid": key.0, "typ": "JWT"});
        let msg = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header).unwrap()),
            B64.encode(serde_json::to_vec(&claims).unwrap())
        );
        let signer = if spoil == Spoil::Signature {
            &self.other_key
        } else {
            &key.1
        };
        format!("{msg}.{}", B64.encode(signer.sign(msg.as_bytes()).as_ref()))
    }
}

async fn discovery(State(a): State<Arc<Actions>>) -> axum::Json<serde_json::Value> {
    axum::Json(json!({
        "issuer": a.issuer,
        "jwks_uri": format!("{}/.well-known/jwks", a.issuer),
        "subject_types_supported": ["public", "pairwise"],
        "response_types_supported": ["id_token"],
        "id_token_signing_alg_values_supported": ["EdDSA"],
    }))
}

async fn jwks(State(a): State<Arc<Actions>>) -> Response {
    if a.keys_down.load(Ordering::SeqCst) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let key = a.key.lock().unwrap();
    axum::Json(json!({"keys": [{
        "kty": "OKP", "crv": "Ed25519", "kid": key.0, "use": "sig", "alg": "EdDSA",
        "x": B64.encode(key.1.public_key().as_ref())
    }]}))
    .into_response()
}

/// Start the issuer on a loopback port; its tokens name `audience`.
pub async fn start(audience: &str) -> Arc<Actions> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let actions = Arc::new(Actions {
        issuer,
        audience: audience.to_string(),
        key: Mutex::new((super::throwaway(), keypair())),
        other_key: keypair(),
        keys_down: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/.well-known/jwks", get(jwks))
        .with_state(actions.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    actions
}

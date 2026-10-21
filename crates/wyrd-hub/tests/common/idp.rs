//! A minimal OpenID Connect provider for tests: discovery, an Ed25519
//! JWKS, an authorization endpoint that approves at once, and a token
//! endpoint that checks the client secret, the code, and PKCE.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::json;

/// How the next ID token is spoiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spoil {
    #[default]
    None,
    Nonce,
    Audience,
    Issuer,
    Expired,
    /// Signed by a key that is not in the JWKS (same `kid`).
    Signature,
}

#[derive(Debug)]
struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    subject: String,
}

pub struct Idp {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    key: Ed25519KeyPair,
    other_key: Ed25519KeyPair,
    grants: Mutex<HashMap<String, Grant>>,
    pub subject: Mutex<String>,
    pub spoil: Mutex<Spoil>,
}

impl std::fmt::Debug for Idp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Idp")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

fn keypair() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

impl Idp {
    fn id_token(&self, g: &Grant) -> String {
        let spoil = *self.spoil.lock().unwrap();
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let claims = json!({
            "iss": if spoil == Spoil::Issuer { "http://evil.test" } else { self.issuer.as_str() },
            "aud": if spoil == Spoil::Audience { "someone-else" } else { self.client_id.as_str() },
            "sub": g.subject,
            "nonce": if spoil == Spoil::Nonce { "wrong" } else { g.nonce.as_str() },
            "exp": if spoil == Spoil::Expired { now - 3600 } else { now + 300 },
            "iat": now,
            "email": format!("{}@example.test", g.subject),
            "name": format!("User {}", g.subject),
        });
        let header = json!({"alg": "EdDSA", "kid": "test-key", "typ": "JWT"});
        let msg = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header).unwrap()),
            B64.encode(serde_json::to_vec(&claims).unwrap())
        );
        let key = if spoil == Spoil::Signature {
            &self.other_key
        } else {
            &self.key
        };
        format!("{msg}.{}", B64.encode(key.sign(msg.as_bytes()).as_ref()))
    }
}

async fn discovery(State(idp): State<Arc<Idp>>) -> Json<serde_json::Value> {
    Json(json!({
        "issuer": idp.issuer,
        "authorization_endpoint": format!("{}/auth", idp.issuer),
        "token_endpoint": format!("{}/token", idp.issuer),
        "jwks_uri": format!("{}/keys", idp.issuer),
    }))
}

async fn keys(State(idp): State<Arc<Idp>>) -> Json<serde_json::Value> {
    Json(json!({"keys": [{
        "kty": "OKP", "crv": "Ed25519", "kid": "test-key", "use": "sig", "alg": "EdDSA",
        "x": B64.encode(idp.key.public_key().as_ref())
    }]}))
}

async fn authorize(
    State(idp): State<Arc<Idp>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], idp.client_id);
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["scope"].split(' ').any(|s| s == "openid"));
    let code = super::throwaway();
    let subject = idp.subject.lock().unwrap().clone();
    idp.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            nonce: q["nonce"].clone(),
            challenge: q["code_challenge"].clone(),
            redirect_uri: q["redirect_uri"].clone(),
            subject,
        },
    );
    let location = format!("{}?code={code}&state={}", q["redirect_uri"], q["state"]);
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

async fn token(
    State(idp): State<Arc<Idp>>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let basic = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", idp.client_id, idp.client_secret));
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(format!("Basic {basic}").as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    let Some(g) = idp.grants.lock().unwrap().remove(&f["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    let challenge = B64.encode(ring::digest::digest(
        &ring::digest::SHA256,
        f["code_verifier"].as_bytes(),
    ));
    if challenge != g.challenge || f["redirect_uri"] != g.redirect_uri {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    Json(json!({"access_token": "x", "token_type": "Bearer", "id_token": idp.id_token(&g)}))
        .into_response()
}

/// Start the provider on a loopback port.
pub async fn start() -> Arc<Idp> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let idp = Arc::new(Idp {
        issuer,
        client_id: "wyrd-hub".into(),
        client_secret: super::throwaway(),
        key: keypair(),
        other_key: keypair(),
        grants: Mutex::new(HashMap::new()),
        subject: Mutex::new("alice".into()),
        spoil: Mutex::new(Spoil::None),
    });
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/keys", get(keys))
        .route("/auth", get(authorize))
        .route("/token", post(token))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    idp
}

/// The router, for driving `/auth` in-process like a browser would.
pub fn router(idp: &Arc<Idp>) -> Router {
    Router::new()
        .route("/auth", get(authorize))
        .with_state(idp.clone())
}

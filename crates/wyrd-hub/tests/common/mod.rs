//! Postgres and MinIO in containers (Testcontainers) and a Hub over them.
//! Each test gets its own containers; they are removed when it ends.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod bundles;
pub mod idp;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use testcontainers::core::{IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tower::ServiceExt;
use wyrd_hub::config::{Config, OidcConfig, S3Config, Secret};
use wyrd_hub::db::{self, PgPool};
use wyrd_hub::{AppState, Storage, app};

pub const POSTGRES: (&str, &str) = ("postgres", "17-alpine");
/// Upstream MinIO no longer publishes images; this is the pinned
/// community-maintained build of the same server.
pub const MINIO: (&str, &str) = ("pgsty/minio", "RELEASE.2026-08-04T00-00-00Z");

/// A throwaway credential, made at run time.
pub fn throwaway() -> String {
    format!("t{}", ulid::Ulid::generate()).to_lowercase()
}

pub struct Postgres {
    pub url: String,
    _container: ContainerAsync<GenericImage>,
}

pub async fn postgres() -> Postgres {
    let password = throwaway();
    let container = GenericImage::new(POSTGRES.0, POSTGRES.1)
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "hub")
        .with_env_var("POSTGRES_PASSWORD", &password)
        .with_env_var("POSTGRES_DB", "hub")
        .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
        .start()
        .await
        .expect("start postgres (is Docker running?)");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://hub:{password}@127.0.0.1:{port}/hub");
    // The image restarts the server once after initdb: wait until it
    // answers for real.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(pool) = db::connect(&url, 1).await
            && db::query_scalar::<i32>("SELECT 1")
                .fetch_one(&pool)
                .await
                .is_ok()
        {
            pool.close().await;
            break;
        }
        assert!(Instant::now() < deadline, "postgres never became ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Postgres {
        url,
        _container: container,
    }
}

pub struct Minio {
    pub s3: S3Config,
    _container: ContainerAsync<GenericImage>,
}

pub async fn minio() -> Minio {
    let (user, password) = (throwaway(), throwaway());
    let container = GenericImage::new(MINIO.0, MINIO.1)
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::message_on_stderr("API:"))
        .with_env_var("MINIO_ROOT_USER", &user)
        .with_env_var("MINIO_ROOT_PASSWORD", &password)
        .with_cmd(["server", "/data"])
        .with_mount(Mount::tmpfs_mount("/data"))
        .start()
        .await
        .expect("start minio (is Docker running?)");
    let port = container.get_host_port_ipv4(9000).await.unwrap();
    Minio {
        s3: S3Config {
            endpoint: format!("http://127.0.0.1:{port}"),
            bucket: "wyrd-test".into(),
            region: "us-east-1".into(),
            access_key_id: user,
            secret_access_key: Secret::new(password),
        },
        _container: container,
    }
}

/// Configuration for a Hub over these services.
pub fn config(database_url: &str, s3: S3Config) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://hub.test".into(),
        database_url: Secret::new(database_url),
        db_max_connections: 5,
        s3,
        oidc: None,
        data_dir: std::env::temp_dir().join("wyrd-hub-tests"),
        limits: wyrd_hub::config::Limits::default(),
    }
}

/// A migrated Hub with an existing bucket.
pub struct Hub {
    pub state: Arc<AppState>,
    pub data: tempfile::TempDir,
    pub pool: PgPool,
    pub pg: Postgres,
    pub minio: Minio,
}

pub async fn hub() -> Hub {
    hub_with(None).await
}

/// A Hub logging in through `idp`.
pub async fn hub_with_idp(idp: &idp::Idp) -> Hub {
    hub_with(Some(OidcConfig {
        issuer: idp.issuer.clone(),
        client_id: idp.client_id.clone(),
        client_secret: Secret::new(idp.client_secret.clone()),
    }))
    .await
}

pub async fn hub_with(oidc: Option<OidcConfig>) -> Hub {
    let (pg, minio) = tokio::join!(postgres(), minio());
    let storage = Storage::new(minio.s3.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Err(e) = storage.ensure_bucket().await {
        assert!(Instant::now() < deadline, "minio never became ready: {e}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let pool = db::connect(&pg.url, 5).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut cfg = config(&pg.url, minio.s3.clone());
    cfg.oidc = oidc;
    cfg.data_dir = data.path().to_path_buf();
    let state = Arc::new(AppState::new(cfg, storage));
    state.set_db(pool.clone());
    Hub {
        state,
        data,
        pool,
        pg,
        minio,
    }
}

impl Hub {
    pub fn app(&self) -> Router {
        app(self.state.clone())
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub async fn send(app: &Router, req: Request<Body>) -> Reply {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body,
    }
}

pub async fn get(app: &Router, uri: &str) -> Reply {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

/// `name=value` of the `Set-Cookie` header naming `name`.
pub fn set_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(|v| v.split(';').next().unwrap_or_default().to_string())
}

pub fn location(r: &Reply) -> String {
    r.headers[axum::http::header::LOCATION]
        .to_str()
        .unwrap()
        .to_string()
}

/// A started login: the provider redirect and the browser-binding cookie.
pub struct Started {
    pub authorize: String,
    pub login_cookie: String,
}

pub async fn start_login(app: &Router, return_to: &str) -> Started {
    let r = get(
        app,
        &format!("/api/v1/auth/oidc/login?return_to={return_to}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    Started {
        authorize: location(&r),
        login_cookie: set_cookie(&r.headers, "wyrd_login").unwrap(),
    }
}

/// Let the provider approve: returns the hub callback path and query.
pub async fn approve(idp: &std::sync::Arc<idp::Idp>, authorize: &str) -> String {
    let path = authorize.strip_prefix(&idp.issuer).unwrap();
    let r = get(&idp::router(idp), path).await;
    assert_eq!(r.status, StatusCode::FOUND);
    location(&r)
        .strip_prefix("http://hub.test")
        .unwrap()
        .to_string()
}

pub async fn callback(app: &Router, path: &str, cookie: &str) -> Reply {
    send(
        app,
        Request::get(path)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// Log in as `subject`; returns the session cookie (`wyrd_session=…`).
pub async fn login(app: &Router, idp: &std::sync::Arc<idp::Idp>, subject: &str) -> String {
    *idp.subject.lock().unwrap() = subject.to_string();
    let s = start_login(app, "/").await;
    let cb = approve(idp, &s.authorize).await;
    let r = callback(app, &cb, &s.login_cookie).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    set_cookie(&r.headers, "wyrd_session").unwrap()
}

/// A request with the session cookie (and the Hub's own Origin, as a
/// browser sends on mutations).
pub fn as_user(
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header("origin", "http://hub.test");
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

/// Sessions of the people in `acme` (admin, member, viewer) and of the
/// admin of another org, `other`.
pub struct Team {
    pub admin: String,
    pub member: String,
    pub viewer: String,
    pub outsider: String,
}

pub async fn user_id(app: &Router, cookie: &str) -> String {
    let r = send(app, as_user("GET", "/api/v1/me", cookie, None)).await;
    r.json()["id"].as_str().unwrap().to_string()
}

pub async fn team(app: &Router, idp: &std::sync::Arc<idp::Idp>) -> Team {
    let t = Team {
        admin: login(app, idp, "admin").await,
        member: login(app, idp, "member").await,
        viewer: login(app, idp, "viewer").await,
        outsider: login(app, idp, "outsider").await,
    };
    for (cookie, slug) in [(&t.admin, "acme"), (&t.outsider, "other")] {
        let r = send(
            app,
            as_user(
                "POST",
                "/api/v1/orgs",
                cookie,
                Some(serde_json::json!({"slug": slug, "name": slug})),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    for (cookie, role) in [(&t.member, "member"), (&t.viewer, "viewer")] {
        let id = user_id(app, cookie).await;
        let r = send(
            app,
            as_user(
                "PUT",
                &format!("/api/v1/orgs/acme/members/{id}"),
                &t.admin,
                Some(serde_json::json!({ "role": role })),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    t
}

/// Upload `bytes` to `org` with the session `cookie`.
pub async fn upload(
    app: &Router,
    cookie: &str,
    org: &str,
    bytes: &[u8],
    repo: Option<&str>,
) -> Reply {
    let (content_type, body) = bundles::multipart(bytes, repo);
    send(
        app,
        Request::post(format!("/api/v1/orgs/{org}/bundles"))
            .header("cookie", cookie)
            .header("origin", "http://hub.test")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

/// Percent-encode a query value.
pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

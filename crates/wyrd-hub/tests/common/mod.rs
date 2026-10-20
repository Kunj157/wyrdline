//! Postgres and MinIO in containers (Testcontainers) and a Hub over them.
//! Each test gets its own containers; they are removed when it ends.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
use wyrd_hub::config::{Config, S3Config, Secret};
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
        data_dir: std::env::temp_dir(),
    }
}

/// A migrated Hub with an existing bucket.
pub struct Hub {
    pub state: Arc<AppState>,
    pub pool: PgPool,
    pub pg: Postgres,
    pub minio: Minio,
}

pub async fn hub() -> Hub {
    let (pg, minio) = tokio::join!(postgres(), minio());
    let storage = Storage::new(minio.s3.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Err(e) = storage.ensure_bucket().await {
        assert!(Instant::now() < deadline, "minio never became ready: {e}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let pool = db::connect(&pg.url, 5).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    let state = Arc::new(AppState::new(config(&pg.url, minio.s3.clone()), storage));
    state.set_db(pool.clone());
    Hub {
        state,
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

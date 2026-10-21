//! Schema, migrations, readiness, and object storage against real
//! Postgres 17 and MinIO (Testcontainers; `make test-hub`).

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use wyrd_hub::db::{self, PgPool};
use wyrd_hub::storage::StorageError;

const TABLES: [&str; 5] = ["orgs", "users", "memberships", "bundles", "audit_logs"];

async fn tables(pool: &PgPool) -> Vec<String> {
    let mut present = Vec::new();
    for t in TABLES {
        let found: Option<String> = db::query_scalar("SELECT to_regclass($1)::text")
            .bind(t)
            .fetch_one(pool)
            .await
            .unwrap();
        present.extend(found);
    }
    present
}

#[tokio::test]
async fn migrations_go_up_down_and_up_again() {
    let pg = common::postgres().await;
    let pool = db::connect(&pg.url, 2).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(tables(&pool).await, TABLES);
    // Idempotent.
    db::MIGRATOR.run(&pool).await.unwrap();
    db::MIGRATOR.undo(&pool, 0).await.unwrap();
    assert!(tables(&pool).await.is_empty(), "down removes every table");
    let applied: i64 = db::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 0);
    db::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(tables(&pool).await, TABLES);
}

#[tokio::test]
async fn schema_constraints_hold() {
    let hub = common::hub().await;
    let p = &hub.pool;
    let bad_slug = db::query("INSERT INTO orgs (id, slug, name) VALUES ('o1', 'Bad Slug', 'x')")
        .execute(p)
        .await;
    assert!(bad_slug.is_err());
    db::query("INSERT INTO orgs (id, slug, name) VALUES ('o1', 'acme', 'Acme')")
        .execute(p)
        .await
        .unwrap();
    db::query("INSERT INTO users (id, issuer, subject) VALUES ('u1', 'https://idp', 's1')")
        .execute(p)
        .await
        .unwrap();
    let bad_role =
        db::query("INSERT INTO memberships (org_id, user_id, role) VALUES ('o1', 'u1', 'owner')")
            .execute(p)
            .await;
    assert!(bad_role.is_err());
    let dup_user =
        db::query("INSERT INTO users (id, issuer, subject) VALUES ('u2', 'https://idp', 's1')")
            .execute(p)
            .await;
    assert!(dup_user.is_err(), "issuer + subject identify a user");
}

#[tokio::test]
async fn audit_log_is_append_only() {
    let hub = common::hub().await;
    let p = &hub.pool;
    db::query("INSERT INTO audit_logs (actor, action, target) VALUES ('system', 'test', 'x')")
        .execute(p)
        .await
        .unwrap();
    for stmt in [
        "UPDATE audit_logs SET action = 'changed'",
        "DELETE FROM audit_logs",
        "TRUNCATE audit_logs",
    ] {
        let err = db::query(stmt).execute(p).await.unwrap_err().to_string();
        assert!(err.contains("append-only"), "{stmt}: {err}");
    }
}

#[tokio::test]
async fn readyz_is_200_with_database_and_bucket() {
    let hub = common::hub().await;
    let r = common::get(&hub.app(), "/readyz").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["ready"], true);
    let m = common::get(&hub.app(), "/metrics").await.text();
    assert!(m.contains("wyrd_hub_ready 1"), "{m}");
    assert!(
        m.contains("wyrd_hub_db_pool_connections{state=\"idle\"}"),
        "{m}"
    );
}

#[tokio::test]
async fn storage_round_trip() {
    let hub = common::hub().await;
    let s = &hub.state.storage;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.wyrd");
    std::fs::write(&file, b"bundle bytes").unwrap();
    s.put_file("bundles/o1/b1.wyrd", file).await.unwrap();
    let obj = s.get("bundles/o1/b1.wyrd").await.unwrap();
    assert_eq!(obj.len, Some(12));
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut { obj.reader }, &mut body).unwrap();
    assert_eq!(body, b"bundle bytes");
    s.delete("bundles/o1/b1.wyrd").await.unwrap();
    assert!(matches!(
        s.get("bundles/o1/b1.wyrd").await,
        Err(StorageError::NotFound)
    ));
    // Deleting twice is fine.
    s.delete("bundles/o1/b1.wyrd").await.unwrap();
}

#[tokio::test]
async fn wrong_credentials_are_not_ready() {
    let hub = common::hub().await;
    let mut s3 = hub.minio.s3.clone();
    s3.secret_access_key = wyrd_hub::config::Secret::new(common::throwaway());
    let storage = wyrd_hub::Storage::new(s3);
    assert!(matches!(
        storage.ping().await,
        Err(StorageError::Status { status: 403, .. })
    ));
}

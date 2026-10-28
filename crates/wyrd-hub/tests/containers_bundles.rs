//! Bundle upload, verification, storage, quotas, and retention against
//! Postgres and MinIO in containers.

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::http::StatusCode;
use common::bundles::{bundle, tampered, unsigned};
use common::{as_user, idp, send, upload};
use serde_json::json;
use wyrd_hub::db;

async fn count(hub: &common::Hub, sql: &'static str) -> i64 {
    db::query_scalar(sql).fetch_one(&hub.pool).await.unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one flow, read top to bottom
async fn upload_list_show_download() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let bytes = bundle("cart", &["D001", "D003", "D001"]);
    let r = upload(&app, &t.member, "acme", &bytes, Some("Kunj157/shop")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let b = r.json()["bundle"].clone();
    assert_eq!(b["repo"], "Kunj157/shop");
    assert_eq!(b["rules"], json!(["D001", "D003"]));
    assert_eq!(b["session"]["name"], "cart");
    assert_eq!(b["session"]["agent"], "fakeagent");
    assert_eq!(b["size_bytes"], bytes.len());
    let id = b["id"].as_str().unwrap().to_string();

    // A second, different session in another repo.
    let r = upload(
        &app,
        &t.admin,
        "acme",
        &bundle("login", &["D002"]),
        Some("Kunj157/auth"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);

    let list = |q: &'static str| {
        let app = app.clone();
        let cookie = t.viewer.clone();
        async move {
            let r = send(
                &app,
                as_user(
                    "GET",
                    &format!("/api/v1/orgs/acme/bundles{q}"),
                    &cookie,
                    None,
                ),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.text());
            r.json()["bundles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["session"]["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(list("").await, ["login", "cart"], "newest first");
    assert_eq!(list("?repo=Kunj157/shop").await, ["cart"]);
    assert_eq!(list("?rule=D002").await, ["login"]);
    assert_eq!(list("?since=2000-01-01").await.len(), 2);
    assert_eq!(list("?since=2999-01-01T00:00:00Z").await.len(), 0);
    assert_eq!(list("?limit=1").await, ["login"]);
    let bad = send(
        &app,
        as_user(
            "GET",
            "/api/v1/orgs/acme/bundles?since=not-a-date",
            &t.viewer,
            None,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let show = send(
        &app,
        as_user("GET", &format!("/api/v1/bundles/{id}"), &t.viewer, None),
    )
    .await;
    assert_eq!(show.status, StatusCode::OK);
    assert_eq!(show.json()["manifest_hash"], b["manifest_hash"]);
    let dl = send(
        &app,
        as_user(
            "GET",
            &format!("/api/v1/bundles/{id}/download"),
            &t.viewer,
            None,
        ),
    )
    .await;
    assert_eq!(dl.status, StatusCode::OK);
    assert_eq!(dl.body.as_ref(), bytes.as_slice(), "byte for byte");
    assert!(
        dl.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("cart.wyrd")
    );

    // Same bundle again: the existing one, no new row.
    let again = upload(&app, &t.member, "acme", &bytes, None).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["duplicate"], true);
    assert_eq!(again.json()["bundle"]["id"], id.as_str());
    assert_eq!(count(&hub, "SELECT count(*) FROM bundles").await, 2);
    let audits = count(
        &hub,
        "SELECT count(*) FROM audit_logs WHERE action = 'bundle.upload'",
    )
    .await;
    assert_eq!(audits, 2);
    let m = common::get(&app, "/metrics").await.text();
    assert!(
        m.contains("wyrd_hub_uploads_total{outcome=\"accepted\"} 2"),
        "{m}"
    );
    assert!(m.contains("wyrd_hub_bundle_verify_seconds_count 3"), "{m}");
}

#[tokio::test]
async fn unverifiable_bundles_are_422_naming_the_check() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    for (bytes, check) in [
        (tampered("t"), "file hashes"),
        (unsigned("u"), "signature over the manifest"),
        (
            b"not a bundle at all".to_vec(),
            "archive structure and limits",
        ),
    ] {
        let r = upload(&app, &t.member, "acme", &bytes, None).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
        let p = r.json();
        assert_eq!(p["type"], "bundle-rejected");
        assert_eq!(p["failed_check"], check, "{p}");
        assert!(p["detail"].as_str().unwrap().starts_with(check));
        assert!(
            p["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["status"] == "failed")
        );
    }
    assert_eq!(count(&hub, "SELECT count(*) FROM bundles").await, 0);
    let spooled = std::fs::read_dir(hub.data.path().join("uploads"))
        .unwrap()
        .count();
    assert_eq!(spooled, 0, "spool files are removed");
}

#[tokio::test]
async fn bad_requests() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let r = upload(
        &app,
        &t.member,
        "acme",
        &bundle("x", &[]),
        Some("bad repo name"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &app,
        as_user(
            "POST",
            "/api/v1/orgs/acme/bundles",
            &t.member,
            Some(json!({"no": "multipart"})),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn size_limit_and_quotas() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let small = {
        let mut cfg = hub.state.config.clone();
        cfg.limits.max_upload_bytes = 1024;
        let st = std::sync::Arc::new(wyrd_hub::AppState::new(cfg, hub.state.storage.clone()));
        st.set_db(hub.pool.clone());
        st
    };
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let big = vec![7u8; 4096];
    let r = upload(&wyrd_hub::app(small), &t.member, "acme", &big, None).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());

    db::query("UPDATE orgs SET quota_bundles = 1 WHERE slug = 'acme'")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(
        upload(&app, &t.member, "acme", &bundle("a", &[]), None)
            .await
            .status,
        StatusCode::CREATED
    );
    let r = upload(&app, &t.member, "acme", &bundle("b", &[]), None).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text());
    assert_eq!(r.json()["type"], "quota-bundles");

    db::query("UPDATE orgs SET quota_bundles = 100, quota_bytes = 10 WHERE slug = 'acme'")
        .execute(&hub.pool)
        .await
        .unwrap();
    let r = upload(&app, &t.member, "acme", &bundle("c", &[]), None).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(r.json()["type"], "quota-bytes");
    assert_eq!(count(&hub, "SELECT count(*) FROM bundles").await, 1);
}

#[tokio::test]
async fn storage_down_is_503_without_orphan_rows() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut s3 = hub.minio.s3.clone();
    s3.endpoint = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let st = std::sync::Arc::new(wyrd_hub::AppState::new(
        hub.state.config.clone(),
        wyrd_hub::Storage::new(s3),
    ));
    st.set_db(hub.pool.clone());
    let r = upload(
        &wyrd_hub::app(st),
        &t.member,
        "acme",
        &bundle("s", &[]),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(count(&hub, "SELECT count(*) FROM bundles").await, 0);
    assert_eq!(
        count(
            &hub,
            "SELECT count(*) FROM audit_logs WHERE action = 'bundle.upload'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn delete_is_admin_only_and_audited() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let r = upload(&app, &t.member, "acme", &bundle("d", &[]), None).await;
    let id = r.json()["bundle"]["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/bundles/{id}");
    for (who, want) in [
        (&t.outsider, StatusCode::NOT_FOUND),
        (&t.viewer, StatusCode::FORBIDDEN),
        (&t.member, StatusCode::FORBIDDEN),
        (&t.admin, StatusCode::NO_CONTENT),
    ] {
        assert_eq!(
            send(&app, as_user("DELETE", &uri, who, None)).await.status,
            want
        );
    }
    assert_eq!(
        send(&app, as_user("GET", &uri, &t.admin, None))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let key = format!("bundles/{}/{id}.wyrd", {
        let org: String = db::query_scalar("SELECT id FROM orgs WHERE slug = 'acme'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
        org
    });
    assert!(matches!(
        hub.state.storage.get(&key).await,
        Err(wyrd_hub::storage::StorageError::NotFound)
    ));
    let (actor, target): (String, String) =
        db::query_as("SELECT actor, target FROM audit_logs WHERE action = 'bundle.delete'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(actor, common::user_id(&app, &t.admin).await);
    assert_eq!(target, id);
}

#[tokio::test]
async fn retention_deletes_old_bundles_and_audits_as_system() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let set =
        |cookie: &str, v: serde_json::Value| as_user("PATCH", "/api/v1/orgs/acme", cookie, Some(v));
    assert_eq!(
        send(&app, set(&t.member, json!({"retention_days": 1})))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, set(&t.admin, json!({"retention_days": 0})))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&app, set(&t.admin, json!({"quota_bytes": 1})))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let ok = send(&app, set(&t.admin, json!({"retention_days": 30}))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());

    let old = upload(&app, &t.member, "acme", &bundle("old", &[]), None).await;
    let old_id = old.json()["bundle"]["id"].as_str().unwrap().to_string();
    upload(&app, &t.member, "acme", &bundle("new", &[]), None).await;
    // The other org keeps everything (no retention there).
    upload(&app, &t.outsider, "other", &bundle("theirs", &[]), None).await;
    db::query("UPDATE bundles SET created_at = now() - interval '31 days' WHERE session_name IN ('old', 'theirs')")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(wyrd_hub::retention::sweep(&hub.state).await.unwrap(), 1);
    assert_eq!(wyrd_hub::retention::sweep(&hub.state).await.unwrap(), 0);
    let left: Vec<String> =
        db::query_scalar("SELECT session_name FROM bundles ORDER BY session_name")
            .fetch_all(&hub.pool)
            .await
            .unwrap();
    assert_eq!(left, ["new", "theirs"]);
    let (actor, target): (String, String) =
        db::query_as("SELECT actor, target FROM audit_logs WHERE action = 'bundle.expire'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(
        (actor.as_str(), target.as_str()),
        ("system", old_id.as_str())
    );
    // Clearing retention.
    let r = send(&app, set(&t.admin, json!({"retention_days": null}))).await;
    assert_eq!(r.json()["retention_days"], serde_json::Value::Null);
}

#[tokio::test]
async fn bundle_authz_and_cross_org() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let app = hub.app();
    let t = common::team(&app, &idp).await;
    let r = upload(&app, &t.member, "acme", &bundle("z", &[]), None).await;
    let id = r.json()["bundle"]["id"].as_str().unwrap().to_string();
    // Upload needs member.
    assert_eq!(
        upload(&app, &t.viewer, "acme", &bundle("v", &[]), None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        upload(&app, &t.outsider, "acme", &bundle("o", &[]), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Reads: viewers yes, outsiders 404 exactly like a missing bundle.
    for uri in [
        "/api/v1/orgs/acme/bundles".to_string(),
        format!("/api/v1/bundles/{id}"),
        format!("/api/v1/bundles/{id}/download"),
    ] {
        assert_eq!(
            send(&app, as_user("GET", &uri, &t.viewer, None))
                .await
                .status,
            StatusCode::OK,
            "{uri}"
        );
        let theirs = send(&app, as_user("GET", &uri, &t.outsider, None)).await;
        assert_eq!(theirs.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(
            common::get(&app, &uri).await.status,
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
    let missing = send(
        &app,
        as_user("GET", "/api/v1/bundles/01NOSUCHBUNDLE", &t.outsider, None),
    )
    .await;
    let foreign = send(
        &app,
        as_user("GET", &format!("/api/v1/bundles/{id}"), &t.outsider, None),
    )
    .await;
    assert_eq!(missing.body, foreign.body);
}

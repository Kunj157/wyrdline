//! The authorisation matrix: every endpoint for every role, plus
//! cross-organisation access, which must look exactly like a missing
//! resource (404, never 403).

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::idp;
use serde_json::{Value, json};

/// Who is calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    Anonymous,
    /// Admin of another organisation only.
    Outsider,
    Viewer,
    Member,
    Admin,
}

const ALL: [Who; 5] = [
    Who::Anonymous,
    Who::Outsider,
    Who::Viewer,
    Who::Member,
    Who::Admin,
];

struct World {
    app: Router,
    cookies: Vec<(Who, String)>,
    target_user: String,
}

impl World {
    fn cookie(&self, who: Who) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(w, _)| *w == who)
            .map(|(_, c)| c.as_str())
    }

    async fn call(&self, who: Who, method: &str, uri: &str, body: Option<Value>) -> common::Reply {
        let req = if let Some(c) = self.cookie(who) {
            common::as_user(method, uri, c, body)
        } else {
            let b = Request::builder().method(method).uri(uri);
            match body {
                Some(v) => b
                    .header("content-type", "application/json")
                    .body(Body::from(v.to_string()))
                    .unwrap(),
                None => b.body(Body::empty()).unwrap(),
            }
        };
        common::send(&self.app, req).await
    }
}

async fn me(app: &Router, cookie: &str) -> String {
    let r = common::send(app, common::as_user("GET", "/api/v1/me", cookie, None)).await;
    r.json()["id"].as_str().unwrap().to_string()
}

/// acme: admin, member, viewer; other: outsider (admin); a target user
/// with no membership.
async fn world(hub: &common::Hub, idp: &std::sync::Arc<idp::Idp>) -> World {
    let app = hub.app();
    let mut cookies = Vec::new();
    for (who, sub) in [
        (Who::Admin, "admin"),
        (Who::Member, "member"),
        (Who::Viewer, "viewer"),
        (Who::Outsider, "outsider"),
    ] {
        cookies.push((who, common::login(&app, idp, sub).await));
    }
    let target = common::login(&app, idp, "target").await;
    let target_user = me(&app, &target).await;
    let w = World {
        app,
        cookies,
        target_user,
    };
    let r = w
        .call(
            Who::Admin,
            "POST",
            "/api/v1/orgs",
            Some(json!({"slug": "acme", "name": "Acme"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = w
        .call(
            Who::Outsider,
            "POST",
            "/api/v1/orgs",
            Some(json!({"slug": "other", "name": "Other"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    for (who, role) in [(Who::Member, "member"), (Who::Viewer, "viewer")] {
        let id = me(&w.app, w.cookie(who).unwrap()).await;
        let r = w
            .call(
                Who::Admin,
                "PUT",
                &format!("/api/v1/orgs/acme/members/{id}"),
                Some(json!({ "role": role })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    w
}

/// Expected status per caller, in `ALL` order.
type Expect = [StatusCode; 5];

const OK: StatusCode = StatusCode::OK;
const CREATED: StatusCode = StatusCode::CREATED;
const NO_CONTENT: StatusCode = StatusCode::NO_CONTENT;
const UNAUTH: StatusCode = StatusCode::UNAUTHORIZED;
const FORBIDDEN: StatusCode = StatusCode::FORBIDDEN;
const NOT_FOUND: StatusCode = StatusCode::NOT_FOUND;

#[tokio::test]
async fn every_endpoint_for_every_role() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let w = world(&hub, &idp).await;
    let target = w.target_user.clone();
    let viewer_read: Expect = [UNAUTH, NOT_FOUND, OK, OK, OK];
    let admin_only: Expect = [UNAUTH, NOT_FOUND, FORBIDDEN, FORBIDDEN, OK];
    let signed_in: Expect = [UNAUTH, OK, OK, OK, OK];
    let cases: Vec<(&str, String, Option<Value>, Expect)> = vec![
        ("GET", "/api/v1/me".into(), None, signed_in),
        ("GET", "/api/v1/tokens".into(), None, signed_in),
        ("GET", "/api/v1/orgs/acme".into(), None, viewer_read),
        ("GET", "/api/v1/orgs/acme/members".into(), None, viewer_read),
        (
            "PUT",
            format!("/api/v1/orgs/acme/members/{target}"),
            Some(json!({"role": "viewer"})),
            admin_only,
        ),
        (
            "GET",
            "/api/v1/audit-logs?org=acme".into(),
            None,
            admin_only,
        ),
        (
            "DELETE",
            format!("/api/v1/orgs/acme/members/{target}"),
            None,
            [UNAUTH, NOT_FOUND, FORBIDDEN, FORBIDDEN, NO_CONTENT],
        ),
        (
            "POST",
            "/api/v1/tokens".into(),
            Some(json!({"name": "ci", "org": "acme"})),
            [UNAUTH, NOT_FOUND, CREATED, CREATED, CREATED],
        ),
    ];
    let mut failures = Vec::new();
    for (method, uri, body, expect) in &cases {
        for (who, want) in ALL.iter().zip(expect) {
            let r = w.call(*who, method, uri, body.clone()).await;
            if r.status != *want {
                failures.push(format!(
                    "{method} {uri} as {who:?}: {} (want {want}) {}",
                    r.status,
                    r.text()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn cross_org_looks_like_missing() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let w = world(&hub, &idp).await;
    let foreign = w.call(Who::Admin, "GET", "/api/v1/orgs/other", None).await;
    let missing = w
        .call(Who::Admin, "GET", "/api/v1/orgs/no-such-org", None)
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.status, missing.status);
    assert_eq!(
        foreign.body, missing.body,
        "no difference in the body either"
    );
    for uri in ["/api/v1/orgs/other/members", "/api/v1/audit-logs?org=other"] {
        let r = w.call(Who::Admin, "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let r = w
        .call(
            Who::Admin,
            "PUT",
            &format!("/api/v1/orgs/other/members/{}", w.target_user),
            Some(json!({"role": "admin"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tokens_act_as_their_user_within_their_org() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let w = world(&hub, &idp).await;
    let r = w
        .call(
            Who::Member,
            "POST",
            "/api/v1/tokens",
            Some(json!({"name": "ci", "org": "acme"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let token = r.json()["token"].as_str().unwrap().to_string();
    let bearer = |uri: &str, method: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    let ok = common::send(&w.app, bearer("/api/v1/orgs/acme", "GET")).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.json()["role"], "member");
    // A token cannot mint tokens, and a token restricted to acme does
    // not see other organisations of its user.
    let mint = common::send(
        &w.app,
        Request::post("/api/v1/tokens")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "more"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(mint.status, StatusCode::FORBIDDEN);
    let foreign = common::send(&w.app, bearer("/api/v1/orgs/other", "GET")).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    // Tokens are stored hashed.
    let stored: i64 =
        wyrd_hub::db::query_scalar("SELECT count(*) FROM api_tokens WHERE token_hash = $1")
            .bind(&token)
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(stored, 0);
    // Revoked tokens stop working.
    let list = w.call(Who::Member, "GET", "/api/v1/tokens", None).await;
    let id = list.json()["tokens"][0]["id"].as_str().unwrap().to_string();
    let r = w
        .call(Who::Member, "DELETE", &format!("/api/v1/tokens/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let gone = common::send(&w.app, bearer("/api/v1/orgs/acme", "GET")).await;
    assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
    // Someone else's token id is not found.
    let r = w
        .call(Who::Admin, "DELETE", &format!("/api/v1/tokens/{id}"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_last_admin_stays_and_every_change_is_audited() {
    let idp = idp::start().await;
    let hub = common::hub_with_idp(&idp).await;
    let w = world(&hub, &idp).await;
    let admin = me(&w.app, w.cookie(Who::Admin).unwrap()).await;
    let demote = w
        .call(
            Who::Admin,
            "PUT",
            &format!("/api/v1/orgs/acme/members/{admin}"),
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(demote.status, StatusCode::CONFLICT);
    let leave = w
        .call(
            Who::Admin,
            "DELETE",
            &format!("/api/v1/orgs/acme/members/{admin}"),
            None,
        )
        .await;
    assert_eq!(leave.status, StatusCode::CONFLICT);
    // By email too.
    let r = w
        .call(
            Who::Admin,
            "PUT",
            "/api/v1/orgs/acme/members/target@example.test",
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let log = w
        .call(Who::Admin, "GET", "/api/v1/audit-logs?org=acme", None)
        .await;
    let entries = log.json();
    let actions: Vec<&str> = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    let first = &entries["entries"][0];
    assert_eq!(first["action"], "org.member.set");
    assert_eq!(first["actor"], admin.as_str());
    assert_eq!(first["details"]["role"], "member");
    assert!(first["request_id"].is_string());
    assert_eq!(
        actions,
        [
            "org.member.set",
            "org.member.set",
            "org.member.set",
            "org.create"
        ],
        "newest first; failed attempts write nothing"
    );
    // Bad input.
    let r = w
        .call(
            Who::Admin,
            "POST",
            "/api/v1/orgs",
            Some(json!({"slug": "Bad Slug", "name": "x"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = w
        .call(
            Who::Admin,
            "POST",
            "/api/v1/orgs",
            Some(json!({"slug": "acme", "name": "dup"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = w
        .call(
            Who::Admin,
            "PUT",
            "/api/v1/orgs/acme/members/nobody@example.test",
            Some(json!({"role": "member"})),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

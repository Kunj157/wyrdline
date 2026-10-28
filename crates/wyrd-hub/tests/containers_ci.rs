//! CI trust rules and the OIDC token exchange against real Postgres and
//! MinIO (Testcontainers; `make test-hub`).

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::actions::{self, Actions, Spoil};
use common::idp;
use serde_json::json;
use wyrd_hub::db;

const PUBLIC_URL: &str = "http://hub.test";

struct Setup {
    hub: common::Hub,
    gh: Arc<Actions>,
    /// Session cookies.
    admin: String,
    member: String,
    stranger: String,
}

/// Org `acme` (alice admin, bob member), org `other` (nobody), carol in
/// no org, and a GitHub Actions issuer the Hub trusts.
async fn setup() -> Setup {
    let idp = idp::start().await;
    let gh = actions::start(PUBLIC_URL).await;
    let hub = common::hub_with(Some(common::idp_config(&idp)), Some(gh.issuer.clone())).await;
    let app = hub.app();
    for (id, slug) in [("o1", "acme"), ("o2", "other")] {
        db::query("INSERT INTO orgs (id, slug, name) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(slug)
            .execute(&hub.pool)
            .await
            .unwrap();
    }
    let mut cookies = Vec::new();
    for (who, role) in [
        ("alice", Some("admin")),
        ("bob", Some("member")),
        ("carol", None),
    ] {
        let cookie = common::login(&app, &idp, who).await;
        if let Some(role) = role {
            let me = common::send(&app, common::as_user("GET", "/api/v1/me", &cookie, None)).await;
            db::query("INSERT INTO memberships (org_id, user_id, role) VALUES ('o1', $1, $2)")
                .bind(me.json()["id"].as_str().unwrap())
                .bind(role)
                .execute(&hub.pool)
                .await
                .unwrap();
        }
        cookies.push(cookie);
    }
    let stranger = cookies.pop().unwrap();
    let member = cookies.pop().unwrap();
    let admin = cookies.pop().unwrap();
    Setup {
        hub,
        gh,
        admin,
        member,
        stranger,
    }
}

async fn add_rule(
    app: &axum::Router,
    cookie: &str,
    org: &str,
    rule: serde_json::Value,
) -> common::Reply {
    common::send(
        app,
        common::as_user(
            "POST",
            &format!("/api/v1/orgs/{org}/ci-trust"),
            cookie,
            Some(rule),
        ),
    )
    .await
}

async fn exchange(app: &axum::Router, token: &str, org: &str) -> common::Reply {
    common::send(
        app,
        Request::post("/api/v1/ci/token")
            .header("content-type", "application/json")
            .body(Body::from(json!({"token": token, "org": org}).to_string()))
            .unwrap(),
    )
    .await
}

fn bearer(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn trust_rules_are_admin_only_and_validated() {
    let s = setup().await;
    let app = s.hub.app();
    let rule = json!({"repository_owner": "octo", "repository": "octo/app"});
    let r = add_rule(&app, &s.member, "acme", rule.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = add_rule(&app, &s.stranger, "acme", rule.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "a stranger sees no org");
    let r = add_rule(&app, &s.admin, "nope", rule.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = add_rule(&app, &s.admin, "other", rule.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "admin of acme only");
    for bad in [
        json!({"repository_owner": "-octo"}),
        json!({"repository_owner": "octo", "repository": "octo"}),
        json!({"repository_owner": "octo", "repository": "someone/app"}),
        json!({"repository": "octo/app"}),
        json!({"repository_owner": "octo", "repository": "octo/app", "wildcard": true}),
        json!({"repository_owner": "octo", "repository": "octo/app", "git_ref": "refs/heads/*"}),
        json!({"repository_owner": "octo", "repository": "octo/app", "git_ref": "main"}),
    ] {
        let r = add_rule(&app, &s.admin, "acme", bad.clone()).await;
        assert!(
            r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = add_rule(&app, &s.admin, "acme", rule.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["repository"], "octo/app");
    let rule_id = r.json()["id"].as_str().unwrap().to_string();
    let r = add_rule(&app, &s.admin, "acme", rule).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // An owner-wide rule is refused without the explicit opt-in.
    let r = add_rule(&app, &s.admin, "acme", json!({"repository_owner": "wide"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "wide", "wildcard": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["repository"], serde_json::Value::Null);
    assert_eq!(r.json()["wildcard"], true);

    let list = common::send(
        &app,
        common::as_user("GET", "/api/v1/orgs/acme/ci-trust", &s.admin, None),
    )
    .await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json()["rules"].as_array().unwrap().len(), 2);
    let list = common::send(
        &app,
        common::as_user("GET", "/api/v1/orgs/acme/ci-trust", &s.member, None),
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN);

    let r = common::send(
        &app,
        common::as_user(
            "DELETE",
            "/api/v1/orgs/acme/ci-trust/unknown",
            &s.admin,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = common::send(
        &app,
        common::as_user(
            "DELETE",
            &format!("/api/v1/orgs/acme/ci-trust/{rule_id}"),
            &s.admin,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let actions: Vec<String> = db::query_scalar(
        "SELECT action FROM audit_logs WHERE org_id = 'o1' AND action LIKE 'ci.trust.%' ORDER BY id",
    )
    .fetch_all(&s.hub.pool)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "ci.trust.add",
            "ci.trust.add",
            "ci.trust.wildcard",
            "ci.trust.remove"
        ]
    );
}

#[tokio::test]
async fn exchange_mints_an_upload_only_token_of_the_trusting_org() {
    let s = setup().await;
    let app = s.hub.app();
    add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "octo", "repository": "octo/app"}),
    )
    .await;
    add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "wide", "wildcard": true}),
    )
    .await;

    // Untrusted: another repository of the owner, another org, no org.
    for (repo, org) in [
        ("octo/other", "acme"),
        ("octo/app", "other"),
        ("octo/app", "nope"),
    ] {
        let r = exchange(&app, &s.gh.token(repo, Spoil::None), org).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{repo} → {org}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "ci-not-trusted");
    }
    // Trusted, case-insensitively; owner-wide rules cover every repository.
    for repo in ["octo/app", "Octo/App", "wide/anything"] {
        let r = exchange(&app, &s.gh.token(repo, Spoil::None), "acme").await;
        assert_eq!(r.status, StatusCode::OK, "{repo}: {}", r.text());
    }
    let r = exchange(&app, &s.gh.token("octo/app", Spoil::None), "acme").await;
    let body = r.json();
    let token = body["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("wyrdci_"), "{token}");
    assert_eq!(body["scope"], "upload");
    assert_eq!(body["org"], "acme");
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 900);
    assert!(body["expires_at"].as_str().unwrap().ends_with('Z'));

    // Who the token is: the repository's CI identity, a member of acme only.
    let me = common::send(&app, bearer("GET", "/api/v1/me", &token)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text());
    assert_eq!(me.json()["name"], "octo/app (GitHub Actions)");
    assert_eq!(
        me.json()["orgs"],
        json!([{"slug": "acme", "name": "acme", "role": "member"}])
    );
    let ci_user = me.json()["id"].as_str().unwrap().to_string();
    // Upload-only: everything else is refused, including admin reads and
    // the session routes.
    for (method, uri) in [
        ("GET", "/api/v1/orgs/acme/ci-trust"),
        ("POST", "/api/v1/auth/logout"),
        ("POST", "/api/v1/orgs/acme/ci-trust"),
    ] {
        let r = common::send(&app, bearer(method, uri, &token)).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{method} {uri}: {}",
            r.text()
        );
        assert_eq!(r.json()["type"], "insufficient-scope");
    }
    // Stored hashed, scoped, expiring.
    let stored: Vec<(String, Option<String>)> = db::query_as(
        "SELECT scope, (expires_at > now())::text FROM api_tokens WHERE user_id = $1 AND token_hash = $2",
    )
    .bind(&ci_user)
    .bind(wyrd_hub::auth::hash(&token))
    .fetch_all(&s.hub.pool)
    .await
    .unwrap();
    assert_eq!(stored, [("upload".to_string(), Some("true".to_string()))]);
    let plain: i64 = db::query_scalar("SELECT count(*) FROM api_tokens WHERE token_hash = $1")
        .bind(&token)
        .fetch_one(&s.hub.pool)
        .await
        .unwrap();
    assert_eq!(plain, 0);
    // Audited with the run's provenance; counted.
    let details: Vec<serde_json::Value> = db::query_scalar::<String>(
        "SELECT details::text FROM audit_logs \
         WHERE action = 'ci.token.exchange' AND actor = $1 ORDER BY id",
    )
    .bind(&ci_user)
    .fetch_all(&s.hub.pool)
    .await
    .unwrap()
    .iter()
    .map(|j| serde_json::from_str(j).unwrap())
    .collect();
    assert_eq!(details.len(), 3, "octo/app twice plus Octo/App");
    assert_eq!(details[0]["actor"], "octocat");
    assert_eq!(details[0]["run_id"], "987654321");
    assert_eq!(details[0]["event_name"], "pull_request");
    let m = common::get(&app, "/metrics").await.text();
    assert!(
        m.contains("wyrd_hub_ci_token_exchanges_total{outcome=\"success\"} 4"),
        "{m}"
    );
    assert!(
        m.contains("wyrd_hub_ci_token_exchanges_total{outcome=\"failure\"} 3"),
        "{m}"
    );
}

#[tokio::test]
async fn ci_token_uploads_to_its_own_org_only() {
    let s = setup().await;
    let app = s.hub.app();
    add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "octo", "repository": "octo/app"}),
    )
    .await;
    let r = exchange(&app, &s.gh.token("octo/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let token = r.json()["token"].as_str().unwrap().to_string();

    let (content_type, body) =
        common::bundles::multipart(&common::bundles::bundle("ci", &["D001"]), Some("octo/app"));
    let up = common::send(
        &app,
        Request::post("/api/v1/orgs/acme/bundles")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.text());
    assert_eq!(up.json()["bundle"]["repo"], "octo/app");

    // The same token is refused on another organisation's uploads.
    let (content_type, body) =
        common::bundles::multipart(&common::bundles::bundle("ci", &["D001"]), Some("octo/app"));
    let other = common::send(
        &app,
        Request::post("/api/v1/orgs/other/bundles")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.text());
    assert_eq!(other.json()["type"], "insufficient-scope");
}

#[tokio::test]
async fn removing_the_rule_revokes_ci_access() {
    let s = setup().await;
    let app = s.hub.app();
    let r = add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "octo", "repository": "octo/app"}),
    )
    .await;
    let rule_id = r.json()["id"].as_str().unwrap().to_string();
    let token = exchange(&app, &s.gh.token("octo/app", Spoil::None), "acme")
        .await
        .json()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let me = common::send(&app, bearer("GET", "/api/v1/me", &token)).await;
    assert_eq!(me.status, StatusCode::OK);
    let ci_user = me.json()["id"].as_str().unwrap().to_string();

    let r = common::send(
        &app,
        common::as_user(
            "DELETE",
            &format!("/api/v1/orgs/acme/ci-trust/{rule_id}"),
            &s.admin,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let me = common::send(&app, bearer("GET", "/api/v1/me", &token)).await;
    assert_eq!(
        me.status,
        StatusCode::UNAUTHORIZED,
        "the live token is gone"
    );
    let memberships: i64 = db::query_scalar("SELECT count(*) FROM memberships WHERE user_id = $1")
        .bind(&ci_user)
        .fetch_one(&s.hub.pool)
        .await
        .unwrap();
    assert_eq!(memberships, 0);
    let r = exchange(&app, &s.gh.token("octo/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn ci_migration_goes_down_and_up() {
    const TABLE: &str = "SELECT to_regclass('ci_trust_rules')::text";
    const COLUMN: &str = "SELECT column_name::text FROM information_schema.columns \
                          WHERE table_name = 'api_tokens' AND column_name = 'scope'";
    let pg = common::postgres().await;
    let pool = db::connect(&pg.url, 2).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    let present = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            db::query_scalar::<Option<String>>(sql)
                .fetch_optional(&pool)
                .await
                .unwrap()
                .flatten()
                .is_some()
        }
    };
    assert!(present(TABLE).await && present(COLUMN).await);
    db::MIGRATOR.undo(&pool, 20_261_020_000_002).await.unwrap();
    assert!(!present(TABLE).await && !present(COLUMN).await);
    db::MIGRATOR.run(&pool).await.unwrap();
    assert!(present(TABLE).await && present(COLUMN).await);
}

#[tokio::test]
async fn exact_rules_refuse_other_repositories_and_owner_wide_rules_are_audited() {
    let s = setup().await;
    let app = s.hub.app();
    add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "octo", "repository": "octo/app"}),
    )
    .await;
    // A different repository of the same owner: refused, with the same body
    // as a repository nobody trusts.
    let other = exchange(&app, &s.gh.token("octo/other", Spoil::None), "acme").await;
    let unknown = exchange(&app, &s.gh.token("elsewhere/app", Spoil::None), "acme").await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.text());
    assert_eq!(other.json(), unknown.json());
    assert_eq!(other.json()["type"], "ci-not-trusted");

    // The explicit owner-wide opt-in: every repository of the owner works,
    // the opt-in is audited as dangerous, and the exchange records it.
    let r = add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "octo", "wildcard": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ok = exchange(&app, &s.gh.token("octo/other", Spoil::None), "acme").await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let dangerous: Vec<String> = db::query_scalar(
        "SELECT details::text FROM audit_logs WHERE org_id = 'o1' AND action = 'ci.trust.wildcard'",
    )
    .fetch_all(&s.hub.pool)
    .await
    .unwrap();
    assert_eq!(dangerous.len(), 1);
    let details: serde_json::Value = serde_json::from_str(&dangerous[0]).unwrap();
    assert_eq!(details["dangerous"], true);
    let exchanged: Vec<String> = db::query_scalar(
        "SELECT details::text FROM audit_logs WHERE org_id = 'o1' AND action = 'ci.token.exchange' \
         AND details->>'repository' = 'octo/other'",
    )
    .fetch_all(&s.hub.pool)
    .await
    .unwrap();
    assert_eq!(exchanged.len(), 1);
    let details: serde_json::Value = serde_json::from_str(&exchanged[0]).unwrap();
    assert_eq!(details["wildcard"], true);

    // A rule pinned to a ref: the test's runs are on refs/heads/main.
    let r = add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "pin", "repository": "pin/app", "git_ref": "refs/heads/release"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = exchange(&app, &s.gh.token("pin/app", Spoil::None), "acme").await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a run on another ref: {}",
        r.text()
    );
    let r = add_rule(
        &app,
        &s.admin,
        "acme",
        json!({"repository_owner": "main", "repository": "main/app", "git_ref": "refs/heads/main"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = exchange(&app, &s.gh.token("main/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // A disabled rule trusts nothing.
    db::query("UPDATE ci_trust_rules SET enabled = false WHERE repository = 'main/app'")
        .execute(&s.hub.pool)
        .await
        .unwrap();
    let r = exchange(&app, &s.gh.token("main/app", Spoil::None), "acme").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test]
async fn owner_wide_rules_from_before_the_opt_in_are_disabled_and_their_access_revoked() {
    let pg = common::postgres().await;
    let pool = db::connect(&pg.url, 2).await.unwrap();
    db::MIGRATOR.run(&pool).await.unwrap();
    // The schema and access as they were before the opt-in.
    db::MIGRATOR.undo(&pool, 20_261_021_120_000).await.unwrap();
    for sql in [
        "INSERT INTO orgs (id, slug, name) VALUES ('o1', 'acme', 'acme'), ('o2', 'other', 'other')",
        "INSERT INTO users (id, issuer, subject, name) VALUES \
           ('ci1', 'https://token.actions.githubusercontent.com', 'repo:octo/app', 'octo/app'), \
           ('ci2', 'https://token.actions.githubusercontent.com', 'repo:octo/shop', 'octo/shop')",
        "INSERT INTO ci_trust_rules (id, org_id, repository_owner, repository) VALUES \
           ('r1', 'o1', 'octo', NULL), ('r2', 'o2', 'octo', 'octo/shop')",
        "INSERT INTO memberships (org_id, user_id, role) VALUES ('o1', 'ci1', 'member'), ('o2', 'ci2', 'member')",
        "INSERT INTO api_tokens (id, token_hash, user_id, org_id, name, scope, expires_at) VALUES \
           ('t1', 'h1', 'ci1', 'o1', 'ci', 'upload', now() + interval '1 day'), \
           ('t2', 'h2', 'ci2', 'o2', 'ci', 'upload', now() + interval '1 day')",
    ] {
        db::query(sql).execute(&pool).await.unwrap();
    }
    db::MIGRATOR.run(&pool).await.unwrap();

    // The owner-wide rule is kept as a record, disabled, and no longer trusts.
    let r1: (bool, bool) =
        db::query_as("SELECT wildcard, enabled FROM ci_trust_rules WHERE id = 'r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(r1, (true, false));
    // The exact rule is untouched.
    let r2: (bool, bool) =
        db::query_as("SELECT wildcard, enabled FROM ci_trust_rules WHERE id = 'r2'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(r2, (false, true));
    // What the owner-wide rule granted is gone; the exact rule's access stays.
    let members: i64 = db::query_scalar("SELECT count(*) FROM memberships WHERE user_id = 'ci1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(members, 0);
    let kept: i64 = db::query_scalar("SELECT count(*) FROM memberships WHERE user_id = 'ci2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
    let tokens: Vec<String> = db::query_scalar("SELECT id FROM api_tokens ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(tokens, ["t2"]);
}

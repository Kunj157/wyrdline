//! Annotation sync over a real WebSocket: roles, authorisation before the
//! upgrade, persistence in Postgres, and limits (Postgres and MinIO in
//! containers).

#![cfg(feature = "container-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use automerge::sync::{Message as SyncMessage, State, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, Automerge, ROOT, ReadDoc};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use wyrd_hub::auth;
use wyrd_hub::db::{self, PgPool};

/// Who exists: organisation `o1` with a member, a viewer and an admin, and
/// organisation `o2` with its own member; bundle `b1` in `o1`, `b2` in `o2`.
struct Seed {
    member: String,
    viewer: String,
    admin: String,
    other: String,
    /// A member's token restricted to `o2`.
    wrong_org: String,
    /// The member's browser session cookie.
    cookie: String,
}

async fn seed(pool: &PgPool) -> Seed {
    for (id, slug) in [("o1", "acme"), ("o2", "other")] {
        db::query("INSERT INTO orgs (id, slug, name) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(slug)
            .execute(pool)
            .await
            .unwrap();
    }
    for (user, org, role) in [
        ("member", "o1", "member"),
        ("viewer", "o1", "viewer"),
        ("admin", "o1", "admin"),
        ("other", "o2", "member"),
    ] {
        db::query("INSERT INTO users (id, issuer, subject) VALUES ($1, 'https://idp', $1)")
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
        db::query("INSERT INTO memberships (org_id, user_id, role) VALUES ($1, $2, $3)")
            .bind(org)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .unwrap();
    }
    for (bid, org) in [("b1", "o1"), ("b2", "o2")] {
        db::query(
            "INSERT INTO bundles (id, org_id, manifest_hash, signer, session_id, agent, events, \
             size_bytes, storage_key) VALUES ($1, $2, $1, 'k', 's', 'fake', 1, 1, $1)",
        )
        .bind(bid)
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    }
    let token = |user: &'static str, org: Option<&'static str>| async move {
        let t = auth::random_secret(auth::TOKEN_PREFIX).unwrap();
        db::query(
            "INSERT INTO api_tokens (id, token_hash, user_id, org_id, name) \
             VALUES ($1, $2, $3, $4, 'test')",
        )
        .bind(common::throwaway())
        .bind(auth::hash(&t))
        .bind(user)
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
        t
    };
    let session = auth::random_secret("").unwrap();
    db::query(
        "INSERT INTO sessions (id_hash, user_id, expires_at) \
         VALUES ($1, 'member', now() + interval '1 hour')",
    )
    .bind(auth::hash(&session))
    .execute(pool)
    .await
    .unwrap();
    Seed {
        member: token("member", None).await,
        viewer: token("viewer", None).await,
        admin: token("admin", None).await,
        other: token("other", None).await,
        wrong_org: token("member", Some("o2")).await,
        cookie: format!("wyrd_session={session}"),
    }
}

/// Serve the Hub on a loopback port; returns its `ws://` base.
async fn serve(hub: &common::Hub) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, hub.app()).into_future());
    format!("ws://{addr}")
}

/// How a test authenticates a connection.
enum Auth<'a> {
    Token(&'a str),
    Cookie {
        cookie: &'a str,
        origin: Option<&'a str>,
    },
    None,
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// One client: an Automerge document and its sync conversation with the
/// Hub over the socket.
struct Client {
    socket: Socket,
    doc: AutoCommit,
    sync: State,
    /// Heads the Hub last said it has.
    remote_heads: Vec<automerge::ChangeHash>,
}

/// The status the Hub answers an upgrade with (101 when it accepts).
async fn status(base: &str, bundle: &str, auth: Auth<'_>) -> u16 {
    match connect(base, bundle, auth).await {
        Ok(_) => 101,
        Err(status) => status,
    }
}

/// Connect; `Err` is the HTTP status the Hub refused the upgrade with.
async fn connect(base: &str, bundle: &str, auth: Auth<'_>) -> Result<Client, u16> {
    let mut req = format!("{base}/api/v1/sync?bundle={bundle}")
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    match auth {
        Auth::Token(t) => {
            h.insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        Auth::Cookie { cookie, origin } => {
            h.insert("cookie", cookie.parse().unwrap());
            if let Some(o) = origin {
                h.insert("origin", o.parse().unwrap());
            }
        }
        Auth::None => {}
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((socket, _)) => Ok(Client {
            socket,
            doc: AutoCommit::new(),
            sync: State::new(),
            remote_heads: Vec::new(),
        }),
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status().as_u16()),
        Err(e) => panic!("connect: {e}"),
    }
}

impl Client {
    /// The role the Hub announced in its first frame.
    async fn hello(&mut self) -> String {
        match self.next().await {
            Some(Message::Text(t)) => {
                serde_json::from_str::<serde_json::Value>(&t).unwrap()["role"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }
            other => panic!("expected the hello frame, got {other:?}"),
        }
    }

    async fn next(&mut self) -> Option<Message> {
        tokio::time::timeout(Duration::from_secs(10), self.socket.next())
            .await
            .expect("a frame within 10 s")
            .map(|r| r.expect("a readable frame"))
    }

    fn put(&mut self, key: &str, value: i64) {
        self.doc.put(ROOT, key, value).unwrap();
    }

    /// Send whatever the Hub is missing.
    async fn push(&mut self) {
        while let Some(m) = self.doc.sync().generate_sync_message(&mut self.sync) {
            self.socket
                .send(Message::Binary(m.encode().into()))
                .await
                .unwrap();
        }
    }

    /// Apply one binary frame; `false` for anything else (returned to the
    /// caller through `last`).
    fn apply(&mut self, frame: &Message) -> bool {
        match frame {
            Message::Binary(b) => {
                let m = SyncMessage::decode(b).unwrap();
                self.remote_heads.clone_from(&m.heads);
                self.doc
                    .sync()
                    .receive_sync_message(&mut self.sync, m)
                    .unwrap();
                true
            }
            _ => false,
        }
    }

    /// Exchange frames until the Hub holds everything this client has:
    /// the protocol sends changes only once the other side has said what
    /// it is missing, so a bare `push` is not enough.
    async fn sync_up(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.push().await;
            let mut mine = self.doc.get_heads();
            let mut theirs = self.remote_heads.clone();
            mine.sort();
            theirs.sort();
            if mine == theirs {
                return;
            }
            assert!(Instant::now() < deadline, "the Hub never caught up");
            let frame = self.next().await.expect("the socket stays open");
            assert!(self.apply(&frame), "unexpected frame {frame:?}");
        }
    }

    /// Exchange frames until `key` is in the document (or 10 s pass).
    async fn wait_for(&mut self, key: &str) -> i64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some((v, _)) = self.doc.get(ROOT, key).unwrap() {
                return v.as_i64().unwrap();
            }
            assert!(Instant::now() < deadline, "{key} never arrived");
            self.push().await;
            let frame = self.next().await.expect("the socket stays open");
            assert!(self.apply(&frame), "unexpected frame {frame:?}");
        }
    }

    /// The next frames: everything until the socket closes.
    async fn drain(&mut self) -> Vec<Message> {
        let mut out = Vec::new();
        while let Some(f) = self.next().await {
            if !self.apply(&f) {
                out.push(f);
            }
        }
        out
    }
}

fn keys(doc: &Automerge) -> Vec<String> {
    doc.keys(ROOT).collect()
}

#[tokio::test]
async fn members_sync_through_the_hub_and_the_document_persists() {
    let hub = common::hub().await;
    let who = seed(&hub.pool).await;
    let base = serve(&hub).await;

    let mut member = connect(&base, "b1", Auth::Token(&who.member))
        .await
        .unwrap();
    assert_eq!(member.hello().await, "member");
    member.put("from_member", 1);
    member.sync_up().await;

    let mut admin = connect(&base, "b1", Auth::Token(&who.admin)).await.unwrap();
    assert_eq!(admin.hello().await, "admin");
    assert_eq!(admin.wait_for("from_member").await, 1);
    admin.put("from_admin", 2);
    admin.sync_up().await;
    assert_eq!(member.wait_for("from_admin").await, 2);

    // Persisted per bundle, loadable as a document holding both edits.
    let deadline = Instant::now() + Duration::from_secs(10);
    let stored = loop {
        let row: Option<Vec<u8>> =
            db::query_scalar("SELECT doc FROM bundle_annotations WHERE bundle_id = 'b1'")
                .fetch_optional(&hub.pool)
                .await
                .unwrap();
        if let Some(bytes) = row
            && let Ok(doc) = Automerge::load(&bytes)
            && keys(&doc).len() == 2
        {
            break doc;
        }
        assert!(Instant::now() < deadline, "the document was never saved");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(keys(&stored), ["from_admin", "from_member"]);

    let metrics = common::get(&hub.app(), "/metrics").await.text();
    assert!(
        metrics.contains("wyrd_hub_sync_connections_total{role=\"member\"} 1")
            && metrics.contains("wyrd_hub_sync_connections_total{role=\"admin\"} 1")
            && metrics.contains("wyrd_hub_sync_clients 2")
            && metrics.contains("wyrd_hub_sync_documents 1")
            && metrics.contains("wyrd_hub_sync_messages_total{direction=\"in\",outcome=\"ok\"}"),
        "{metrics}"
    );
    drop((member, admin));

    // After every client left, the room is gone and a new client is served
    // from the stored document.
    let mut fresh = connect(&base, "b1", Auth::Token(&who.member))
        .await
        .unwrap();
    fresh.hello().await;
    assert_eq!(fresh.wait_for("from_admin").await, 2);
    assert_eq!(fresh.wait_for("from_member").await, 1);
}

#[tokio::test]
async fn viewers_follow_but_their_changes_are_rejected() {
    let hub = common::hub().await;
    let s = seed(&hub.pool).await;
    let base = serve(&hub).await;

    let mut m = connect(&base, "b1", Auth::Token(&s.member)).await.unwrap();
    m.hello().await;
    m.put("shared", 7);
    m.sync_up().await;

    let mut v = connect(&base, "b1", Auth::Token(&s.viewer)).await.unwrap();
    assert_eq!(v.hello().await, "viewer");
    assert_eq!(v.wait_for("shared").await, 7);

    v.put("mine", 1);
    v.push().await;
    let frames = v.drain().await;
    let text = frames
        .iter()
        .find_map(|f| match f {
            Message::Text(t) => Some(t.to_string()),
            _ => None,
        })
        .expect("an error frame");
    let note: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(note["error"], "read_only");
    assert_eq!(note["message"], "viewers cannot change annotations");
    let close = frames.iter().find_map(|f| match f {
        Message::Close(Some(c)) => Some(c.clone()),
        _ => None,
    });
    assert_eq!(close.map(|c| u16::from(c.code)), Some(1008));

    // The viewer's edit never reached anyone.
    m.put("later", 2);
    m.sync_up().await;
    assert!(m.doc.get(ROOT, "mine").unwrap().is_none());
    let metrics = common::get(&hub.app(), "/metrics").await.text();
    assert!(
        metrics.contains("wyrd_hub_sync_messages_total{direction=\"in\",outcome=\"read_only\"} 1"),
        "{metrics}"
    );
}

#[tokio::test]
async fn authorisation_happens_before_the_upgrade() {
    let hub = common::hub().await;
    let s = seed(&hub.pool).await;
    let base = serve(&hub).await;
    assert_eq!(status(&base, "b1", Auth::None).await, 401);
    assert_eq!(status(&base, "b1", Auth::Token("wyrdhub_nope")).await, 401);
    // Another organisation's bundle looks like a missing one.
    assert_eq!(status(&base, "b1", Auth::Token(&s.other)).await, 404);
    assert_eq!(status(&base, "b2", Auth::Token(&s.member)).await, 404);
    assert_eq!(status(&base, "nope", Auth::Token(&s.member)).await, 404);
    // A token restricted to one organisation cannot reach another's bundle.
    assert_eq!(status(&base, "b1", Auth::Token(&s.wrong_org)).await, 404);
    assert_eq!(status(&base, "..%2Fetc", Auth::Token(&s.member)).await, 400);
    assert_eq!(status(&base, "", Auth::Token(&s.member)).await, 400);
    // A browser session needs the Hub's own origin on the upgrade.
    assert_eq!(
        status(
            &base,
            "b1",
            Auth::Cookie {
                cookie: &s.cookie,
                origin: Some("http://evil.test"),
            }
        )
        .await,
        403
    );
    assert_eq!(
        status(
            &base,
            "b1",
            Auth::Cookie {
                cookie: &s.cookie,
                origin: None,
            }
        )
        .await,
        403
    );
    assert_eq!(
        status(
            &base,
            "b1",
            Auth::Cookie {
                cookie: &s.cookie,
                origin: Some("http://hub.test"),
            }
        )
        .await,
        101
    );
}

#[tokio::test]
async fn frames_over_the_limit_end_the_connection() {
    let hub = common::hub().await;
    let s = seed(&hub.pool).await;
    let base = serve(&hub).await;
    let mut c = connect(&base, "b1", Auth::Token(&s.member)).await.unwrap();
    c.hello().await;
    let huge = vec![0u8; wyrd_hub::sync::limits().max_message + 1];
    c.socket.send(Message::Binary(huge.into())).await.unwrap();
    // The socket closes (a close frame or a dropped connection), and
    // nothing was stored.
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match c.socket.next().await {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the connection stayed open");
    let rows: i64 = db::query_scalar("SELECT count(*) FROM bundle_annotations")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    // A garbage frame within the limit is refused with a reason.
    let mut d = connect(&base, "b1", Auth::Token(&s.member)).await.unwrap();
    d.hello().await;
    d.socket
        .send(Message::Binary(vec![0x42; 64].into()))
        .await
        .unwrap();
    let frames = d.drain().await;
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, Message::Text(t) if t.contains("\"invalid\""))),
        "{frames:?}"
    );
}

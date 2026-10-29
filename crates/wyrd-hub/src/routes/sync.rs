//! `GET /api/v1/sync?bundle={bid}`: the Automerge sync protocol over a
//! WebSocket for one bundle's annotation document (plan §14.4, #159).
//!
//! Authentication and authorisation happen before the upgrade: the caller
//! must belong to the bundle's organisation (else 404, like any resource
//! in another organisation). Viewers follow the document but every change
//! they send is rejected and the socket closed with a reason; members and
//! admins write. Binary frames carry sync messages; the server sends one
//! text frame first (`{"role":…}`) and, on a rejection, a last one
//! (`{"error":…,"message":…}`) before closing.

use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use wyrd_sync::{Connection, JoinError, Reject, Role};

use crate::auth::{self, Caller, Via};
use crate::sync::PgStore;
use crate::{AppState, Problem, db};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/sync", get(sync))
}

#[derive(Debug, Deserialize)]
struct SyncQuery {
    bundle: Option<String>,
}

/// WebSocket close code for a policy violation (RFC 6455 §7.4.1).
const POLICY_VIOLATION: u16 = 1008;
/// Close code for a message the server cannot accept.
const UNSUPPORTED_DATA: u16 = 1003;
/// Close code for a client that fell too far behind (try again later).
const TRY_AGAIN_LATER: u16 = 1013;

fn role_of(role: auth::Role) -> Role {
    match role {
        auth::Role::Viewer => Role::Viewer,
        auth::Role::Member => Role::Member,
        auth::Role::Admin => Role::Admin,
    }
}

async fn sync(
    State(st): State<Arc<AppState>>,
    caller: Caller,
    Query(q): Query<SyncQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Problem> {
    // A WebSocket upgrade is a GET, which the cookie extractor does not
    // guard; a cross-site page must not be able to open one with the
    // user's session (cross-site WebSocket hijacking).
    if caller.via == Via::Session && !auth::same_origin(&st, &headers) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "cross-site",
            "Cross-site request refused",
        ));
    }
    let bid = q
        .bundle
        .filter(|b| wyrd_sync::valid_doc_id(b))
        .ok_or_else(|| Problem::bad_request("`bundle` must be a bundle id"))?;
    let pool = st.db()?;
    let found: Option<(String, String)> = db::query_as(
        "SELECT b.org_id, m.role FROM bundles b \
         JOIN memberships m ON m.org_id = b.org_id AND m.user_id = $2 \
         WHERE b.id = $1",
    )
    .bind(&bid)
    .bind(&caller.user_id)
    .fetch_optional(pool)
    .await?;
    let role = match found {
        Some((org, role)) if caller.token_org.as_deref().is_none_or(|t| t == org) => {
            auth::Role::parse(&role).map(role_of)
        }
        _ => None,
    }
    .ok_or_else(Problem::not_found)?;
    let conn = st.sync.join(&bid, role).await.map_err(|e| match e {
        JoinError::Full => Problem::unavailable("too many clients on this bundle"),
        JoinError::Storage(reason) => Problem::internal(&reason),
    })?;
    let limits = st.sync.limits();
    Ok(ws
        .max_message_size(limits.max_message)
        .max_frame_size(limits.max_message)
        .on_upgrade(move |socket| session(socket, conn, st)))
}

fn count(st: &AppState, direction: &str, outcome: &str) {
    st.metrics.inc(
        "wyrd_hub_sync_messages_total",
        &[("direction", direction), ("outcome", outcome)],
    );
}

/// How long a closing socket keeps reading the client's frames.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Send a close, then read until the client has answered it (or the grace
/// period ends), discarding what it still sends. Dropping a socket with
/// unread inbound data makes the kernel reset the connection, and a client
/// can then lose the close and the reason it carries.
async fn close(socket: &mut WebSocket, code: u16, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while socket.recv().await.is_some() {}
    })
    .await;
}

/// Tell the client why, then close. The client rolls back what it sent.
async fn refuse(socket: &mut WebSocket, reject: Reject) {
    let note = serde_json::json!({ "error": reject.code(), "message": reject.to_string() });
    let _ = socket.send(Message::Text(note.to_string().into())).await;
    close(socket, POLICY_VIOLATION, reject.code()).await;
}

/// Pump frames between the socket and the relay until either side ends.
async fn session(mut socket: WebSocket, mut conn: Connection<PgStore>, st: Arc<AppState>) {
    st.metrics.inc(
        "wyrd_hub_sync_connections_total",
        &[("role", role_name(conn.role()))],
    );
    let hello = serde_json::json!({ "role": role_name(conn.role()) });
    if socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(raw))) => match conn.receive(&raw).await {
                    Ok(()) => count(&st, "in", "ok"),
                    Err(reject) => {
                        count(&st, "in", reject.code());
                        refuse(&mut socket, reject).await;
                        return;
                    }
                },
                Some(Ok(Message::Text(_))) => {
                    count(&st, "in", Reject::Invalid.code());
                    close(&mut socket, UNSUPPORTED_DATA, "binary sync frames only").await;
                    return;
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            },
            outgoing = conn.next() => {
                let Some(raw) = outgoing else {
                    close(&mut socket, TRY_AGAIN_LATER, "too far behind; reconnect").await;
                    return;
                };
                if socket.send(Message::Binary(raw.into())).await.is_err() {
                    return;
                }
                count(&st, "out", "ok");
            }
        }
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Admin => "admin",
    }
}

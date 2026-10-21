//! The audit log: one append-only row per mutation.

use serde_json::Value;
use sqlx_core::executor::Executor;

use crate::db::{self, Postgres};

/// What happened, to what, by whom.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub org_id: Option<&'a str>,
    /// A user id, or `system` for jobs.
    pub actor: &'a str,
    /// Dotted verb, e.g. `org.member.set`.
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub request_id: &'a str,
}

/// Write `entry` with `details` (inside the caller's transaction when
/// `exec` is one, so the change and its audit row commit together).
pub async fn record<'c, E>(
    exec: E,
    entry: Entry<'_>,
    details: Value,
) -> Result<(), sqlx_core::Error>
where
    E: Executor<'c, Database = Postgres>,
{
    db::query(
        "INSERT INTO audit_logs (org_id, actor, action, target, request_id, details) \
         VALUES ($1, $2, $3, $4, NULLIF($5, ''), $6)",
    )
    .bind(entry.org_id)
    .bind(entry.actor)
    .bind(entry.action)
    .bind(entry.target)
    .bind(entry.request_id)
    .bind(sqlx_core::types::Json(details))
    .execute(exec)
    .await?;
    Ok(())
}

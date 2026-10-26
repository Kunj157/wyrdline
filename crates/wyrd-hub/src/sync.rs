//! Annotation documents in Postgres: the [`DocStore`] behind the sync
//! relay mounted at `/api/v1/sync` (`routes::sync`), one row per bundle.

use std::io;
use std::sync::{Arc, OnceLock};

use wyrd_sync::{DocStore, Limits, SyncHub};

use crate::db::{self, PgPool};

/// Saved Automerge documents in `bundle_annotations`.
#[derive(Debug, Clone)]
pub struct PgStore {
    db: Arc<OnceLock<PgPool>>,
}

impl PgStore {
    pub fn new(db: Arc<OnceLock<PgPool>>) -> Self {
        PgStore { db }
    }

    fn pool(&self) -> io::Result<&PgPool> {
        self.db
            .get()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "database not connected"))
    }
}

fn storage(e: &sqlx_core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

impl DocStore for PgStore {
    async fn load(&self, doc: &str) -> io::Result<Option<Vec<u8>>> {
        db::query_scalar("SELECT doc FROM bundle_annotations WHERE bundle_id = $1")
            .bind(doc)
            .fetch_optional(self.pool()?)
            .await
            .map_err(|e| storage(&e))
    }

    async fn save(&self, doc: &str, bytes: Vec<u8>) -> io::Result<()> {
        db::query(
            "INSERT INTO bundle_annotations (bundle_id, doc) VALUES ($1, $2) \
             ON CONFLICT (bundle_id) DO UPDATE SET doc = EXCLUDED.doc, updated_at = now()",
        )
        .bind(doc)
        .bind(bytes)
        .execute(self.pool()?)
        .await
        .map(|_| ())
        .map_err(|e| storage(&e))
    }
}

/// The relay over [`PgStore`] with the Hub's limits.
pub type Relay = SyncHub<PgStore>;

/// Bounds for Hub clients: frames up to 1 MiB, documents up to 8 MiB,
/// 64 clients per bundle.
pub fn limits() -> Limits {
    Limits::default()
}

pub fn relay(db: Arc<OnceLock<PgPool>>) -> Arc<Relay> {
    SyncHub::new(PgStore::new(db), limits())
}

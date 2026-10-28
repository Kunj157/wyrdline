//! The retention job: delete bundles older than their organisation's
//! `retention_days`, each deletion audited as `system`.

use std::sync::Arc;
use std::time::Duration;

use crate::db;
use crate::routes::bundles::{self, BundleRow};
use crate::{AppState, Problem};

/// Bundles handled per sweep (the next sweep continues).
const BATCH: i64 = 500;

/// Delete every expired bundle (up to one batch); returns how many.
pub async fn sweep(st: &AppState) -> Result<u64, Problem> {
    let pool = st.db()?;
    let expired: Vec<BundleRow> = db::query_as(bundles::select!(
        " WHERE o.retention_days IS NOT NULL \
          AND b.created_at < now() - make_interval(days => o.retention_days) \
          ORDER BY b.created_at LIMIT $1"
    ))
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    let mut n = 0;
    for row in &expired {
        let request_id = format!("retention-{}", ulid::Ulid::generate());
        match bundles::delete_bundle(st, row, "system", &request_id, "bundle.expire").await {
            Ok(()) => n += 1,
            Err(p) if p.status == 404 => {}
            Err(p) => return Err(p),
        }
    }
    if n > 0 {
        tracing::info!(deleted = n, "retention sweep");
    }
    st.metrics.inc("wyrd_hub_retention_sweeps_total", &[]);
    Ok(n)
}

/// Run [`sweep`] every `every` once the database is up.
pub fn spawn(st: Arc<AppState>, every: Duration) {
    if every.is_zero() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if st.db().is_err() {
                continue;
            }
            if let Err(p) = sweep(&st).await {
                tracing::error!(problem = ?p, "retention sweep failed");
            }
        }
    });
}

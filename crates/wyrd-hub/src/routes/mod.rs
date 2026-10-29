//! HTTP routes.

mod auth;
pub(crate) mod bundles;
mod health;
mod orgs;
mod sync;

use std::sync::Arc;

use axum::Router;

use crate::AppState;

pub fn router(max_upload_bytes: u64) -> Router<Arc<AppState>> {
    health::router()
        .merge(bundles::router(max_upload_bytes))
        .merge(auth::router())
        .merge(orgs::router())
        .merge(sync::router())
}

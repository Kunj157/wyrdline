//! HTTP routes.

mod auth;
mod health;
mod sync;

use std::sync::Arc;

use axum::Router;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    health::router().merge(auth::router()).merge(sync::router())
}

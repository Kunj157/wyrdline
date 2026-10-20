//! HTTP routes.

mod auth;
mod health;
mod orgs;

use std::sync::Arc;

use axum::Router;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    health::router().merge(auth::router()).merge(orgs::router())
}

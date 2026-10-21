//! `wyrd-hub`: serve the Hub API. Configuration comes from the
//! environment (`docs/architecture/hub.md`).

use std::process::ExitCode;
use std::sync::Arc;

use wyrd_hub::{AppState, Config, Storage, app, db, init_logging};

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let storage = Storage::new(config.s3.clone());
    let state = Arc::new(AppState::new(config.clone(), storage));
    // Listen first: /healthz answers and /readyz says what is missing
    // while the database and the bucket come up.
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, public_url = %config.public_url, "wyrd-hub listening");
    let background = state.clone();
    tokio::spawn(async move {
        let pool = db::connect_with_retry(
            background.config.database_url.expose(),
            background.config.db_max_connections,
        )
        .await;
        background.set_db(pool);
    });
    wyrd_hub::retention::spawn(
        state.clone(),
        std::time::Duration::from_secs(config.limits.retention_interval_secs),
    );
    let bucket = state.clone();
    tokio::spawn(async move {
        let mut attempt = 0u32;
        while let Err(e) = bucket.storage.ensure_bucket().await {
            let wait = db::backoff(attempt);
            tracing::warn!(error = %e, bucket = bucket.storage.bucket(), "object storage unavailable");
            tokio::time::sleep(wait).await;
            attempt = attempt.saturating_add(1);
        }
        tracing::info!(bucket = bucket.storage.bucket(), "object storage ready");
    });
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "invalid configuration");
            return ExitCode::from(2);
        }
    };
    match serve(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "wyrd-hub failed");
            ExitCode::FAILURE
        }
    }
}

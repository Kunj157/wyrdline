//! PostgreSQL: the pool, migrations, and the start-up retry loop.

use std::time::Duration;

use std::borrow::Cow;
use std::sync::LazyLock;

use sqlx_core::migrate::{Migration, MigrationType, Migrator};
use sqlx_core::query::Query;
use sqlx_core::query_as::QueryAs;
use sqlx_core::query_scalar::QueryScalar;
pub use sqlx_postgres::{PgArguments, PgPool, PgPoolOptions, PgRow, Postgres};

/// Embed a reversible migration pair from `crates/wyrd-hub/migrations`.
macro_rules! migration {
    ($version:literal, $name:literal) => {
        [
            Migration::new(
                $version,
                Cow::Borrowed($name),
                MigrationType::ReversibleUp,
                sqlx_core::sql_str::SqlSafeStr::into_sql_str(include_str!(concat!(
                    "../migrations/",
                    stringify!($version),
                    "_",
                    $name,
                    ".up.sql"
                ))),
                false,
            ),
            Migration::new(
                $version,
                Cow::Borrowed($name),
                MigrationType::ReversibleDown,
                sqlx_core::sql_str::SqlSafeStr::into_sql_str(include_str!(concat!(
                    "../migrations/",
                    stringify!($version),
                    "_",
                    $name,
                    ".down.sql"
                ))),
                false,
            ),
        ]
    };
}

/// The schema migrations, each reversible (an `up` and a `down` file),
/// embedded at build time. Versions are dates, so migrations written on
/// parallel branches do not collide.
#[allow(clippy::unreadable_literal)] // versions are timestamps
pub static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| {
    Migrator::with_migrations(
        [migration!(20261020000001, "core")]
            .into_iter()
            .flatten()
            .collect(),
    )
});

/// A statement (sqlx without its compile-time macros, which would pull
/// SQLite into the workspace).
pub fn query(sql: &'static str) -> Query<'static, Postgres, PgArguments> {
    sqlx_core::query::query(sql)
}

pub fn query_as<O>(sql: &'static str) -> QueryAs<'static, Postgres, O, PgArguments>
where
    O: for<'r> sqlx_core::from_row::FromRow<'r, PgRow>,
{
    sqlx_core::query_as::query_as(sql)
}

pub fn query_scalar<O>(sql: &'static str) -> QueryScalar<'static, Postgres, O, PgArguments>
where
    (O,): for<'r> sqlx_core::from_row::FromRow<'r, PgRow>,
{
    sqlx_core::query_scalar::query_scalar(sql)
}

/// Open a pool (no retry).
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, sqlx_core::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
}

/// Delay before attempt `n + 1` (exponential from 500 ms, capped at 30 s).
pub fn backoff(n: u32) -> Duration {
    let ms = 500u64.saturating_mul(1u64 << n.min(16));
    Duration::from_millis(ms.min(30_000))
}

/// Connect and migrate, retrying with [`backoff`] until it works. Every
/// failure is logged with its reason, so a Hub waiting for its database
/// says why instead of crash-looping.
pub async fn connect_with_retry(url: &str, max_connections: u32) -> PgPool {
    let mut attempt = 0u32;
    loop {
        let result = match connect(url, max_connections).await {
            Ok(pool) => MIGRATOR.run(&pool).await.map(|()| pool).map_err(|e| {
                tracing::error!(error = %e, "database migration failed");
                e.to_string()
            }),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(pool) => {
                tracing::info!(attempt, "database connected and migrated");
                return pool;
            }
            Err(error) => {
                let wait = backoff(attempt);
                tracing::warn!(
                    attempt,
                    %error,
                    retry_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                    "database unavailable"
                );
                tokio::time::sleep(wait).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(5), Duration::from_secs(16));
        assert_eq!(backoff(6), Duration::from_secs(30));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn every_migration_is_reversible() {
        let migrations = MIGRATOR.iter().collect::<Vec<_>>();
        assert!(migrations.len() >= 2, "an up and a down per migration");
        for m in &migrations {
            assert!(
                m.migration_type.is_reversible(),
                "{} has no down migration",
                m.description
            );
        }
    }
}

//! PostgreSQL connection and migrations.

use std::time::Duration;

use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};
use tracing::{info, warn};

/// Migrations embedded at compile time from `crates/control-plane/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Connects with bounded retries so containers can start before PostgreSQL is ready.
pub async fn connect(url: &str, max_connections: u32, attempts: u32) -> anyhow::Result<PgPool> {
    let mut delay = Duration::from_millis(500);
    let mut last = None;
    for attempt in 1..=attempts.max(1) {
        match PgPoolOptions::new()
            .max_connections(max_connections)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Duration::from_secs(300))
            .connect(url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) => {
                warn!(attempt, error = %e, "database not reachable yet");
                last = Some(e);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
    Err(anyhow::anyhow!(
        "cannot connect to PostgreSQL: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Applies pending migrations (idempotent; serialized by an advisory lock).
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    info!(
        migrations = MIGRATOR.iter().count(),
        "database schema up to date"
    );
    Ok(())
}

/// Readiness: the database answers and every embedded migration has been applied.
/// Services do not migrate on their own; `aionix-admin migrate` does (ADR-0011).
pub async fn check_ready(pool: &PgPool) -> Result<(), String> {
    let applied: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM _sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("42P01") => {
            "schema not migrated (run aionix-admin migrate)".to_owned()
        }
        _ => format!("database unavailable: {e}"),
    })?;
    let missing: Vec<i64> = MIGRATOR
        .iter()
        .map(|m| m.version)
        .filter(|v| !applied.contains(v))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("pending migrations: {missing:?}"))
    }
}

/// True when `e` is a unique-constraint violation (optionally of `constraint`).
pub fn is_unique_violation(e: &sqlx::Error, constraint: Option<&str>) -> bool {
    match e {
        sqlx::Error::Database(d) => {
            d.is_unique_violation() && constraint.is_none_or(|c| d.constraint() == Some(c))
        }
        _ => false,
    }
}

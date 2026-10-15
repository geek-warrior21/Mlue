pub mod migrations;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Database connection error: {0}")]
    Connection(#[from] sqlx::Error),
    #[error("Database migration error: {0}")]
    Migration(#[from] migrations::MigrationError),
    #[error("Filesystem error creating database directory: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn init_pool_at_path(path: &Path) -> Result<SqlitePool, StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let path_str = path.to_string_lossy();
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path_str))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;

    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

pub async fn init_memory_pool() -> Result<SqlitePool, StorageError> {
    // Shared memory database allows multiple connections to access the same in-memory DB during tests
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;

    migrations::run_migrations(&pool).await?;

    Ok(pool)
}

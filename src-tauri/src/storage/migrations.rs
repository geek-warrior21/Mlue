use sqlx::SqlitePool;
use std::fmt;

#[derive(Debug)]
pub enum MigrationError {
    Sqlx(sqlx::Error),
    Failed(String),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlx(e) => write!(f, "Migration SQL error: {}", e),
            Self::Failed(msg) => write!(f, "Migration failed: {}", msg),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<sqlx::Error> for MigrationError {
    fn from(err: sqlx::Error) -> Self {
        Self::Sqlx(err)
    }
}

struct Migration {
    version: i64,
    description: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    description: "create_projects_api_keys_and_settings_tables",
    sql: r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS api_keys (
                id TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                access_policy TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                last_used_at TEXT,
                revoked_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_api_keys_project_id ON api_keys(project_id);
            CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash ON api_keys(key_hash);

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
        "#,
}];

pub async fn run_migrations(pool: &SqlitePool) -> Result<(), MigrationError> {
    // Enable foreign keys
    sqlx::query("PRAGMA foreign_keys = ON;")
        .execute(pool)
        .await?;

    // Create migrations tracker table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS _schema_migrations (
            version INTEGER PRIMARY KEY NOT NULL,
            description TEXT NOT NULL,
            applied_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    for migration in MIGRATIONS {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT version FROM _schema_migrations WHERE version = ?;")
                .bind(migration.version)
                .fetch_optional(pool)
                .await?;

        if exists.is_none() {
            let mut tx = pool.begin().await?;

            // Execute migration statements
            for statement in migration.sql.split(';') {
                let stmt = statement.trim();
                if !stmt.is_empty() {
                    sqlx::query(stmt).execute(&mut *tx).await?;
                }
            }

            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO _schema_migrations (version, description, applied_at) VALUES (?, ?, ?);",
            )
            .bind(migration.version)
            .bind(migration.description)
            .bind(now)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
        }
    }

    Ok(())
}

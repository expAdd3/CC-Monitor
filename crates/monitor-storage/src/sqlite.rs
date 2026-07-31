use sqlx::{
    migrate::{MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub const MAX_CONNECTIONS: u32 = 4;
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
pub const BUSY_RETRY_DELAYS_MS: [u64; 4] = [1, 2, 4, 8];
pub const BUSY_RETRY_BUDGET: Duration = Duration::from_millis(250);
pub const SUPPORTED_SCHEMA_VERSION: i64 = 13;
// Only the desktop-owned migration entry point embeds this v1-v13 sequence.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum StorageMigrationError {
    #[error(
        "unsupported_future_schema: database version {found} is newer than supported version {supported}"
    )]
    UnsupportedFutureSchema { found: i64, supported: i64 },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Migration(#[from] MigrateError),
}

/// Shared SQLx-only classification for retryable SQLite lock contention.
/// The short-lived rusqlite Hook deliberately keeps its own bounded policy.
pub fn is_sqlite_busy(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(database) = error else {
        return false;
    };
    matches!(
        database.code().as_deref(),
        Some("5" | "6" | "261" | "262" | "517")
    ) || database.message().contains("database is locked")
}

pub fn connect_options(database_path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
}

pub async fn connect(database_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    SqlitePoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .connect_with(connect_options(database_path))
        .await
}

fn unsupported_future_schema(found: i64) -> StorageMigrationError {
    StorageMigrationError::UnsupportedFutureSchema {
        found,
        supported: SUPPORTED_SCHEMA_VERSION,
    }
}

async fn applied_schema_version(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let has_migration_table: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
              WHERE type='table' AND name='_sqlx_migrations'
         )",
    )
    .fetch_one(pool)
    .await?;
    if has_migration_table == 0 {
        return Ok(0);
    }
    sqlx::query_scalar("SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations WHERE success=1")
        .fetch_one(pool)
        .await
}

pub async fn migrate(pool: &SqlitePool) -> Result<(), StorageMigrationError> {
    let found = applied_schema_version(pool).await?;
    if found > SUPPORTED_SCHEMA_VERSION {
        return Err(unsupported_future_schema(found));
    }
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Opens an application database only after a read-only compatibility check.
///
/// The preflight deliberately avoids the normal WAL and synchronous pragmas so
/// an unsupported future database is rejected without modifying it or creating
/// SQLite sidecar files.
pub async fn connect_and_migrate(
    database_path: &Path,
) -> Result<SqlitePool, StorageMigrationError> {
    if database_path.exists() {
        let mut wal_name = database_path.as_os_str().to_owned();
        wal_name.push("-wal");
        let wal_path = PathBuf::from(wal_name);
        let wal_has_frames = std::fs::metadata(&wal_path)
            .map(|metadata| metadata.len() > 32)
            .unwrap_or(false);
        let inspection = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database_path)
                    .read_only(true)
                    // WAL-aware read-only mode may create empty sidecars. When
                    // no WAL frames exist, immutable mode is both accurate and
                    // genuinely zero-write. A WAL containing frames must remain
                    // visible so a newer migration committed there is not missed.
                    .immutable(!wal_has_frames),
            )
            .await?;
        let found = applied_schema_version(&inspection).await?;
        inspection.close().await;
        if found > SUPPORTED_SCHEMA_VERSION {
            return Err(unsupported_future_schema(found));
        }
    }

    let pool = connect(database_path).await?;
    if let Err(error) = migrate(&pool).await {
        pool.close().await;
        return Err(error);
    }
    Ok(pool)
}

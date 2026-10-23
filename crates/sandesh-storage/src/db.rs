//! Database setup, pooling and migrations.

use crate::StorageError;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::time::Duration;

/// Migrations, embedded in the binary at compile time.
///
/// The test suite runs against these same migrations rather than a
/// hand-constructed schema, so a migration that does not produce the expected
/// schema fails the tests rather than being discovered in production.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// An open database, with migrations applied.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Opens (creating if absent) the database at `path` and migrates it.
    ///
    /// # Errors
    /// Fails if the file cannot be opened or a migration fails.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let options = base_options(SqliteConnectOptions::new().filename(path));
        Self::connect(options).await
    }

    /// Opens a private in-memory database, for tests.
    ///
    /// Each call produces an independent database, so tests never share state
    /// and are safe to run in parallel.
    ///
    /// # Errors
    /// Fails if the database cannot be created or a migration fails.
    pub async fn open_in_memory() -> Result<Self, StorageError> {
        // max_connections(1) is required, not merely tidy: each new connection
        // to `:memory:` would otherwise get its own empty database.
        let options = base_options(SqliteConnectOptions::new().in_memory(true));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .map_err(StorageError::from)?;

        MIGRATOR.run(&pool).await.map_err(StorageError::migration)?;
        Ok(Self { pool })
    }

    async fn connect(options: SqliteConnectOptions) -> Result<Self, StorageError> {
        // One writer, several readers. SQLite permits a single writer; letting
        // the pool serialise them avoids SQLITE_BUSY under concurrent delivery
        // and gives per-mailbox UID assignment its serialisation point
        // (CORE_SPEC §11).
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(StorageError::from)?;

        MIGRATOR.run(&pool).await.map_err(StorageError::migration)?;
        Ok(Self { pool })
    }

    /// The connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Closes the pool.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// Applies the pragmas from `STORAGE_DESIGN.md` §3.
///
/// `foreign_keys` is the important one: `SQLite` does **not** enforce foreign
/// keys by default, and it is a per-connection setting, so it must be part of
/// the connect options rather than executed once.
fn base_options(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .pragma("temp_store", "MEMORY")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let db = Database::open_in_memory().await.expect("open");

        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(db.pool())
                .await
                .expect("query tables");

        let names: Vec<&str> = tables.iter().map(|(n,)| n.as_str()).collect();
        for expected in [
            "accounts",
            "attachments",
            "deliveries",
            "delivery_recipients",
            "email_identities",
            "entry_keywords",
            "mailboxes",
            "message_entries",
            "message_headers",
            "messages",
            "mime_parts",
        ] {
            assert!(names.contains(&expected), "missing table {expected}");
        }
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        // SQLite leaves these off by default; the whole relational integrity
        // story depends on this pragma actually being applied.
        let db = Database::open_in_memory().await.expect("open");
        let (enabled,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .expect("pragma");
        assert_eq!(enabled, 1, "foreign key enforcement must be on");
    }

    #[tokio::test]
    async fn tables_are_strict() {
        let db = Database::open_in_memory().await.expect("open");
        let (sql,): (String,) =
            sqlx::query_as("SELECT sql FROM sqlite_master WHERE name = 'messages'")
                .fetch_one(db.pool())
                .await
                .expect("schema");
        assert!(sql.contains("STRICT"), "type enforcement must be on");
    }

    #[tokio::test]
    async fn in_memory_databases_are_isolated() {
        // Parallel tests depend on this.
        let a = Database::open_in_memory().await.expect("open a");
        let b = Database::open_in_memory().await.expect("open b");

        sqlx::query("INSERT INTO meta (key, value) VALUES ('k', 'v')")
            .execute(a.pool())
            .await
            .expect("insert");

        let found: Option<(String,)> = sqlx::query_as("SELECT value FROM meta WHERE key = 'k'")
            .fetch_optional(b.pool())
            .await
            .expect("query");
        assert!(found.is_none(), "databases must not share state");
    }
}

/// Whether the database answers a trivial query.
///
/// Used by the API's readiness endpoint. Deliberately the cheapest statement
/// `SQLite` has: readiness is polled often, so it must cost nothing, and the
/// question it answers is "can we reach the database at all", not "is the
/// schema right".
///
/// Lives here rather than in the API crate because it is the only place that
/// may name a `sqlx` type — the API sees storage only through core ports.
pub async fn ping(pool: &SqlitePool) -> bool {
    sqlx::query("SELECT 1").fetch_one(pool).await.is_ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod ping_tests {
    use super::*;

    #[tokio::test]
    async fn an_open_database_is_reachable() {
        let db = Database::open_in_memory().await.expect("open");
        assert!(ping(db.pool()).await);
    }

    #[tokio::test]
    async fn a_closed_database_is_not_reachable() {
        // Readiness must report the truth after the pool is gone, or a
        // shutting-down instance would keep claiming to be ready.
        let db = Database::open_in_memory().await.expect("open");
        let pool = db.pool().clone();
        db.close().await;
        assert!(!ping(&pool).await);
    }
}

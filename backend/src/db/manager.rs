//! Database manager implementation
//!
//! This module provides database connection management with:
//! - SQLite connection pool using r2d2
//! - Async wrapper for database operations
//! - Transaction support
//! - Database backup functionality
//! - Error handling integration with TingError

use crate::core::app::error::TaskErrorKind;
use crate::core::app::error::{Result, TingError};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task;

/// Database manager with connection pool
pub struct DatabaseManager {
    pool: Pool<SqliteConnectionManager>,
    db_path: PathBuf,
}

impl DatabaseManager {
    fn pool_busy_error(error: impl std::fmt::Display) -> TingError {
        let error = error.to_string();
        tracing::warn!(
            error = %error,
            message_key = "database.connection.failed",
            message_params = %serde_json::json!({ "error": error }),
            "Failed to get database connection"
        );
        TingError::DatabaseError(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("Database connection pool is busy; please retry later".to_string()),
        ))
    }

    /// Create a new DatabaseManager with the specified database path and pool size
    pub fn new(db_path: &Path, pool_size: u32, busy_timeout: Duration) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|_e| {
                TingError::DatabaseError(rusqlite::Error::InvalidPath(parent.to_path_buf()))
            })?;
        }

        crate::db::backup::prepare_database(db_path)?;

        // Create connection manager
        let manager = SqliteConnectionManager::file(db_path).with_init(move |conn| {
            // Enable foreign keys
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            // Set busy timeout
            conn.busy_timeout(busy_timeout)?;
            // Enable WAL mode for better concurrency
            conn.execute_batch("PRAGMA journal_mode = WAL;")?;
            // Optimize for concurrent access
            conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
            conn.execute_batch("PRAGMA cache_size = -64000;")?; // 64MB cache
            conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
            Ok(())
        });

        // Build connection pool
        let pool = Pool::builder()
            .max_size(pool_size)
            .connection_timeout(Duration::from_secs(30))
            .build(manager)
            .map_err(|_e| TingError::DatabaseError(rusqlite::Error::InvalidQuery))?;

        let manager = Self {
            pool,
            db_path: db_path.to_path_buf(),
        };

        // Run migrations on initialization
        manager.migrate()?;

        Ok(manager)
    }

    /// Create a new DatabaseManager with an in-memory database for testing
    pub fn new_in_memory() -> Result<Self> {
        // Create connection manager for in-memory database
        let manager = SqliteConnectionManager::memory().with_init(|conn| {
            // Enable foreign keys
            conn.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(())
        });

        // Build connection pool
        let pool = Pool::builder()
            .max_size(1) // In-memory databases should use a single connection
            .connection_timeout(Duration::from_secs(30))
            .build(manager)
            .map_err(|_e| TingError::DatabaseError(rusqlite::Error::InvalidQuery))?;

        let manager = Self {
            pool,
            db_path: PathBuf::from(":memory:"),
        };

        // Run migrations on initialization
        manager.migrate()?;

        Ok(manager)
    }

    /// Get a connection from the pool
    pub fn get_connection(&self) -> Result<PooledConnection<SqliteConnectionManager>> {
        self.pool.get().map_err(Self::pool_busy_error)
    }

    /// Execute a database operation asynchronously
    ///
    /// This wraps synchronous database operations in tokio::task::spawn_blocking
    /// to avoid blocking the async runtime.
    pub async fn execute<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let conn = pool.get().map_err(Self::pool_busy_error)?;
            f(&conn)
        })
        .await
        .map_err(|e| {
            let message = format!("数据库任务执行失败: {}", e);
            TingError::task(TaskErrorKind::Join, "execute::task", message)
                .with_source(anyhow::anyhow!(e))
        })?
    }

    /// Execute a database operation within a transaction
    ///
    /// The transaction is automatically committed if the closure returns Ok,
    /// or rolled back if it returns Err.
    pub async fn transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred, f)
            .await
    }

    pub async fn transaction_immediate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate, f)
            .await
    }

    async fn transaction_with_behavior<F, T>(
        &self,
        behavior: rusqlite::TransactionBehavior,
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(Self::pool_busy_error)?;

            let tx = conn
                .transaction_with_behavior(behavior)
                .map_err(TingError::DatabaseError)?;
            let result = f(&tx)?;
            tx.commit().map_err(TingError::DatabaseError)?;

            Ok(result)
        })
        .await
        .map_err(|e| {
            let message = format!("事务执行失败: {}", e);
            TingError::task(TaskErrorKind::Join, "transaction::task", message)
                .with_source(anyhow::anyhow!(e))
        })?
    }

    /// Execute database migrations
    pub fn migrate(&self) -> Result<()> {
        let mut conn = self.get_connection()?;
        crate::db::migrations::run_migrations(&mut conn)
    }

    /// Execute database migrations with automatic backup
    ///
    /// This creates a backup before applying migrations.
    /// If any migration fails, it automatically restores from the backup.
    pub fn migrate_with_backup(&self) -> Result<()> {
        crate::db::migrations::run_migrations_with_backup(&self.db_path)
    }

    /// Backup the database to the specified path
    ///
    /// This creates a consistent backup of the database using SQLite's backup API.
    pub fn backup(&self, backup_path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = backup_path.parent() {
            std::fs::create_dir_all(parent).map_err(TingError::IoError)?;
        }

        let src_conn = self.get_connection()?;

        // Open destination database
        let mut dst_conn = Connection::open(backup_path).map_err(TingError::DatabaseError)?;

        // Perform backup
        let backup = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)
            .map_err(TingError::DatabaseError)?;

        backup
            .run_to_completion(5, Duration::from_millis(250), None)
            .map_err(TingError::DatabaseError)?;

        Ok(())
    }

    /// Backup the database asynchronously
    pub async fn backup_async(&self, backup_path: PathBuf) -> Result<()> {
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            // Ensure parent directory exists
            if let Some(parent) = backup_path.parent() {
                std::fs::create_dir_all(parent).map_err(TingError::IoError)?;
            }

            let src_conn = pool.get().map_err(Self::pool_busy_error)?;

            // Open destination database
            let mut dst_conn = Connection::open(&backup_path).map_err(TingError::DatabaseError)?;

            // Perform backup
            let backup = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)
                .map_err(TingError::DatabaseError)?;

            backup
                .run_to_completion(5, Duration::from_millis(250), None)
                .map_err(TingError::DatabaseError)?;

            Ok(())
        })
        .await
        .map_err(|e| {
            let message = format!("Backup task panicked: {}", e);
            TingError::task(TaskErrorKind::Join, "backup_async::task", message)
                .with_source(anyhow::anyhow!(e))
        })?
    }

    /// Get the database file path
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Get the current pool size
    pub fn pool_size(&self) -> u32 {
        self.pool.max_size()
    }

    /// Get a reference to the connection pool
    pub fn get_pool(&self) -> Arc<Pool<SqliteConnectionManager>> {
        Arc::new(self.pool.clone())
    }

    /// Get the number of idle connections in the pool
    pub fn idle_connections(&self) -> u32 {
        self.pool.state().idle_connections
    }

    /// Get the number of active connections in the pool
    pub fn active_connections(&self) -> u32 {
        self.pool.state().connections - self.pool.state().idle_connections
    }
}

impl Clone for DatabaseManager {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            db_path: self.db_path.clone(),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/db/manager.rs"]
mod tests;

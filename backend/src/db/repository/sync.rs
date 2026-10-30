use crate::core::app::error::Result;
use crate::db::manager::DatabaseManager;
use serde::Serialize;
use std::sync::{Arc, LazyLock};

static INSTANCE: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

#[derive(Debug, Serialize)]
pub struct SyncState {
    pub instance: String,
    pub content_revision: i64,
    pub reading_revision: i64,
}

#[cfg(test)]
#[path = "../../../tests/unit/db/repository/sync.rs"]
mod tests;

#[derive(Debug, Serialize)]
pub struct BookRevision {
    pub instance: String,
    pub content_revision: i64,
    pub reading_revision: i64,
    pub progress_revision: i64,
}

pub struct SyncRepository {
    db: Arc<DatabaseManager>,
}

impl SyncRepository {
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    pub async fn state(&self, user: &str) -> Result<SyncState> {
        let user = user.to_owned();
        self.db.execute(move |conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(MAX(CASE WHEN kind='catalog' THEN revision END),0),
                    COALESCE(MAX(CASE WHEN user_id=?1 AND kind IN ('reading','settings') THEN revision END),0)
                 FROM sync_versions WHERE kind='catalog' OR user_id=?1", [&user], |row| Ok(SyncState {
                    instance: INSTANCE.clone(), content_revision: row.get(0)?, reading_revision: row.get(1)?,
                }),
            )?)
        }).await
    }

    pub async fn book(&self, user: &str, book: &str) -> Result<BookRevision> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.execute(move |conn| {
            Ok(conn.query_row(
                "SELECT
                  COALESCE((SELECT revision FROM sync_versions WHERE kind='content' AND resource_id=?2 AND user_id=''),0),
                  COALESCE((SELECT MAX(revision) FROM sync_versions WHERE user_id=?1 AND
                    ((kind='reading' AND resource_id=?2) OR kind='settings')),0),
                  COALESCE((SELECT revision FROM progress_heads WHERE user_id=?1 AND book_id=?2),0)",
                [&user, &book], |row| Ok(BookRevision { instance: INSTANCE.clone(),
                    content_revision: row.get(0)?, reading_revision: row.get(1)?, progress_revision: row.get(2)?,
                }),
            )?)
        }).await
    }
}

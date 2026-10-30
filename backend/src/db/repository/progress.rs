use crate::core::app::error::{Result, TingError};
use crate::db::{manager::DatabaseManager, models::Progress};
use rusqlite::{OptionalExtension, ToSql, params_from_iter};
use std::sync::Arc;

/// Number of days retained in the daily listening activity table.
pub const LISTENING_EVENTS_RETENTION_DAYS: i64 = 90;
/// Hidden playback progress is kept for this long before it is reclaimed.
pub const HIDDEN_PROGRESS_RETENTION_DAYS: i64 = 180;

/// Repository for Progress entities
pub struct ProgressRepository {
    db: Arc<DatabaseManager>,
}

impl ProgressRepository {
    /// Create a new ProgressRepository
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    /// Get recent visible progress for a user
    pub async fn get_recent(&self, user_id: &str, limit: Option<i32>) -> Result<Vec<Progress>> {
        let user_id = user_id.to_string();
        self.db
            .execute(move |conn| {
                let mut sql = "SELECT p.id, p.user_id, p.book_id, p.chapter_id, p.position, p.duration, p.updated_at \
                  FROM progress p \
                  WHERE p.user_id = ? \
                  AND p.chapter_id IS NOT NULL \
                  AND p.history_hidden_at IS NULL \
                  ORDER BY p.updated_at DESC"
                    .to_string();
                if limit.is_some() {
                    sql.push_str(" LIMIT ?");
                }

                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(TingError::DatabaseError)?;

                let mut args: Vec<&dyn ToSql> = vec![&user_id];
                if let Some(ref limit) = limit {
                    args.push(limit);
                }

                let progress = stmt
                    .query_map(params_from_iter(args), |row| {
                        Ok(Progress {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            book_id: row.get(2)?,
                            chapter_id: row.get(3)?,
                            position: row.get(4)?,
                            duration: row.get(5)?,
                            updated_at: row.get(6)?,
                        })
                    })
                    .map_err(TingError::DatabaseError)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(TingError::DatabaseError)?;

                Ok(progress)
            })
            .await
    }

    /// Get recent visible progress enriched with book and chapter details
    pub async fn get_recent_enriched(
        &self,
        user_id: &str,
        limit: Option<i32>,
    ) -> Result<
        Vec<(
            Progress,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i32>,
        )>,
    > {
        let user_id = user_id.to_string();
        self.db.execute(move |conn| {
            let mut sql = "SELECT p.id, p.user_id, p.book_id, p.chapter_id, p.position, p.duration, p.updated_at, \
                 b.title as book_title, b.cover_url, b.library_id, c.title as chapter_title, c.duration as chapter_duration \
                  FROM progress p \
                  JOIN books b ON p.book_id = b.id \
                  LEFT JOIN chapters c ON p.chapter_id = c.id \
                  WHERE p.user_id = ? \
                  AND p.chapter_id IS NOT NULL \
                  AND p.history_hidden_at IS NULL \
                  ORDER BY p.updated_at DESC"
                .to_string();
            if limit.is_some() {
                sql.push_str(" LIMIT ?");
            }

            let mut stmt = conn
                .prepare(&sql)
                .map_err(TingError::DatabaseError)?;

            let mut args: Vec<&dyn ToSql> = vec![&user_id];
            if let Some(ref limit) = limit {
                args.push(limit);
            }

            let progress = stmt.query_map(params_from_iter(args), |row| {
                let progress = Progress {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    book_id: row.get(2)?,
                    chapter_id: row.get(3)?,
                    position: row.get(4)?,
                    duration: row.get(5)?,
                    updated_at: row.get(6)?,
                };
                let book_title: Option<String> = row.get(7)?;
                let cover_url: Option<String> = row.get(8)?;
                let library_id: Option<String> = row.get(9)?;
                let chapter_title: Option<String> = row.get(10)?;
                let chapter_duration: Option<i32> = row.get(11)?;

                Ok((progress, book_title, cover_url, library_id, chapter_title, chapter_duration))
            }).map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(progress)
        }).await
    }

    /// Get progress for a specific book
    pub async fn get_by_book(&self, user_id: &str, book_id: &str) -> Result<Option<Progress>> {
        let user_id = user_id.to_string();
        let book_id = book_id.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT id, user_id, book_id, chapter_id, position, duration, updated_at \
                 FROM progress WHERE user_id = ? AND book_id = ? \
                 ORDER BY updated_at DESC LIMIT 1",
                    rusqlite::params![&user_id, &book_id],
                    |row| {
                        Ok(Progress {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            book_id: row.get(2)?,
                            chapter_id: row.get(3)?,
                            position: row.get(4)?,
                            duration: row.get(5)?,
                            updated_at: row.get(6)?,
                        })
                    },
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    /// Get progress for a specific book enriched with chapter details
    pub async fn get_by_book_enriched(
        &self,
        user_id: &str,
        book_id: &str,
    ) -> Result<Option<(Progress, Option<String>, Option<i32>)>> {
        let user_id = user_id.to_string();
        let book_id = book_id.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT p.id, p.user_id, p.book_id, p.chapter_id, p.position, p.duration, p.updated_at, \
                        c.title as chapter_title, c.duration as chapter_duration \
                     FROM progress p \
                     LEFT JOIN chapters c ON p.chapter_id = c.id \
                     WHERE p.user_id = ? AND p.book_id = ? \
                     ORDER BY p.updated_at DESC LIMIT 1",
                    rusqlite::params![&user_id, &book_id],
                    |row| {
                        let progress = Progress {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            book_id: row.get(2)?,
                            chapter_id: row.get(3)?,
                            position: row.get(4)?,
                            duration: row.get(5)?,
                            updated_at: row.get(6)?,
                        };
                        let chapter_title: Option<String> = row.get(7)?;
                        let chapter_duration: Option<i32> = row.get(8)?;
                        Ok((progress, chapter_title, chapter_duration))
                    },
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    pub async fn exists_for_chapter(
        &self,
        user_id: &str,
        book_id: &str,
        chapter_id: Option<&str>,
    ) -> Result<bool> {
        let user_id = user_id.to_string();
        let book_id = book_id.to_string();
        let chapter_id = chapter_id.map(str::to_string);
        self.db
            .execute(move |conn| {
                let count: i64 = if let Some(chapter_id) = chapter_id {
                    conn.query_row(
                        "SELECT COUNT(*) FROM progress WHERE user_id = ? AND book_id = ? AND chapter_id = ?",
                        rusqlite::params![&user_id, &book_id, &chapter_id],
                        |row| row.get(0),
                    )
                } else {
                    conn.query_row(
                        "SELECT COUNT(*) FROM progress WHERE user_id = ? AND book_id = ? AND chapter_id IS NULL",
                        rusqlite::params![&user_id, &book_id],
                        |row| row.get(0),
                    )
                }
                .map_err(TingError::DatabaseError)?;

                Ok(count > 0)
            })
            .await
    }

    /// Hide all visible playback history for a user without deleting chapter progress.
    pub async fn clear_by_user(&self, user_id: &str) -> Result<usize> {
        let user_id = user_id.to_string();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "UPDATE progress \
                     SET history_hidden_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE user_id = ? AND history_hidden_at IS NULL",
                    [&user_id],
                )
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    /// Hide selected playback history rows without deleting chapter progress.
    pub async fn hide_history(
        &self,
        user_id: &str,
        progress_ids: &[String],
        chapter_ids: &[String],
    ) -> Result<usize> {
        let user_id = user_id.to_string();
        let progress_ids = progress_ids.to_vec();
        let chapter_ids = chapter_ids.to_vec();
        self.db
            .execute(move |conn| {
                let mut affected = 0;

                if !progress_ids.is_empty() {
                    let placeholders = std::iter::repeat_n("?", progress_ids.len())
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!(
                        "UPDATE progress \
                         SET history_hidden_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                         WHERE user_id = ? \
                         AND history_hidden_at IS NULL \
                         AND id IN ({})",
                        placeholders
                    );
                    let params = std::iter::once(user_id.as_str())
                        .chain(progress_ids.iter().map(String::as_str));
                    affected += conn
                        .execute(&sql, params_from_iter(params))
                        .map_err(TingError::DatabaseError)?;
                }

                if !chapter_ids.is_empty() {
                    let placeholders = std::iter::repeat_n("?", chapter_ids.len())
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!(
                        "UPDATE progress \
                         SET history_hidden_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                         WHERE user_id = ? \
                         AND history_hidden_at IS NULL \
                         AND chapter_id IN ({})",
                        placeholders
                    );
                    let params = std::iter::once(user_id.as_str())
                        .chain(chapter_ids.iter().map(String::as_str));
                    affected += conn
                        .execute(&sql, params_from_iter(params))
                        .map_err(TingError::DatabaseError)?;
                }

                Ok(affected)
            })
            .await
    }

    /// Remove daily listening activity and long-hidden progress outside their
    /// retention windows.
    ///
    /// The database trigger keeps this table tidy during normal writes, while
    /// this explicit maintenance path also handles idle installations.
    pub async fn cleanup_expired_records(&self) -> Result<usize> {
        let event_cutoff_modifier = format!("-{} days", LISTENING_EVENTS_RETENTION_DAYS);
        let hidden_progress_cutoff_modifier = format!("-{} days", HIDDEN_PROGRESS_RETENTION_DAYS);
        self.db
            .transaction(move |tx| {
                let removed_events = tx
                    .execute(
                        "DELETE FROM listening_events WHERE activity_date < DATE('now', ?1)",
                        [&event_cutoff_modifier],
                    )
                    .map_err(TingError::DatabaseError)?;
                let removed_progress = tx
                    .execute(
                        "DELETE FROM progress \
                         WHERE history_hidden_at IS NOT NULL \
                           AND julianday(history_hidden_at) < julianday('now', ?1)",
                        [&hidden_progress_cutoff_modifier],
                    )
                    .map_err(TingError::DatabaseError)?;
                Ok(removed_events + removed_progress)
            })
            .await
    }

    /// Upsert progress (insert or update)
    pub async fn upsert(&self, progress: &Progress) -> Result<()> {
        self.upsert_checked(progress, None, None).await?;
        Ok(())
    }

    pub async fn snapshot(&self, user: &str, book: &str) -> Result<(Option<Progress>, i64)> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db
            .execute(move |conn| {
                let result = conn
                    .query_row(
                        "SELECT p.id,p.chapter_id,p.position,p.duration,p.updated_at,h.revision
                 FROM progress_heads h LEFT JOIN progress p ON p.id=COALESCE(h.progress_id,(
                    SELECT id FROM progress WHERE user_id=h.user_id AND book_id=h.book_id
                    ORDER BY updated_at DESC,id DESC LIMIT 1))
                 WHERE h.user_id=?1 AND h.book_id=?2",
                        [&user, &book],
                        |row| {
                            let id: Option<String> = row.get(0)?;
                            let progress = if let Some(id) = id {
                                Some(Progress {
                                    id,
                                    user_id: user.clone(),
                                    book_id: book.clone(),
                                    chapter_id: row.get(1)?,
                                    position: row.get(2)?,
                                    duration: row.get(3)?,
                                    updated_at: row.get(4)?,
                                })
                            } else {
                                None
                            };
                            Ok((progress, row.get(5)?))
                        },
                    )
                    .optional()?;
                Ok(result.unwrap_or((None, 0)))
            })
            .await
    }

    pub async fn upsert_checked(
        &self,
        progress: &Progress,
        expected: Option<i64>,
        device: Option<String>,
    ) -> Result<Option<i64>> {
        let progress = progress.clone();
        self.db.transaction_immediate(move |conn| {
            let revision: i64 = conn.query_row(
                "SELECT COALESCE((SELECT revision FROM progress_heads WHERE user_id=?1 AND book_id=?2),0)",
                [&progress.user_id,&progress.book_id], |row| row.get(0),
            )?;
            if expected.is_some_and(|value| value != revision) { return Ok(None); }
            let progress_position: Option<f64> = if progress.chapter_id.is_none() {
                conn.query_row(
                    "SELECT position FROM progress WHERE user_id = ? AND book_id = ? AND chapter_id IS NULL",
                    rusqlite::params![&progress.user_id, &progress.book_id],
                    |row| row.get(0),
                ).optional().map_err(TingError::DatabaseError)?
            } else {
                conn.query_row(
                    "SELECT position FROM progress WHERE user_id = ? AND book_id = ? AND chapter_id = ?",
                    rusqlite::params![&progress.user_id, &progress.book_id, &progress.chapter_id],
                    |row| row.get(0),
                ).optional().map_err(TingError::DatabaseError)?
            };
            let previous_position: Option<f64> = if progress_position.is_some() {
                progress_position
            } else if progress.chapter_id.is_none() {
                conn.query_row(
                    "SELECT position FROM listening_events WHERE user_id = ? AND book_id = ? AND chapter_id IS NULL ORDER BY created_at DESC LIMIT 1",
                    rusqlite::params![&progress.user_id, &progress.book_id],
                    |row| row.get(0),
                ).optional().map_err(TingError::DatabaseError)?
            } else {
                conn.query_row(
                    "SELECT position FROM listening_events WHERE user_id = ? AND book_id = ? AND chapter_id = ? ORDER BY created_at DESC LIMIT 1",
                    rusqlite::params![&progress.user_id, &progress.book_id, &progress.chapter_id],
                    |row| row.get(0),
                ).optional().map_err(TingError::DatabaseError)?
            };

            let listen_seconds = if !progress.position.is_finite() || progress.position <= 0.0 {
                0.0
            } else if let Some(previous) = previous_position {
                (progress.position - previous).max(0.0)
            } else {
                progress.position
            };

            conn.execute(
                "INSERT INTO listening_events (\
                    id, aggregate_key, user_id, book_id, chapter_id, activity_date, position, duration, \
                    listen_seconds, progress_updates, created_at, last_active_at\
                 ) VALUES (\
                    ?, ? || CHAR(31) || ? || CHAR(31) || COALESCE(?, '') || CHAR(31) || STRFTIME('%Y-%m-%d', 'now'), \
                    ?, ?, ?, STRFTIME('%Y-%m-%d', 'now'), ?, ?, ?, 1, \
                    STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 ) ON CONFLICT(aggregate_key) DO UPDATE SET \
                    position = excluded.position, \
                    duration = excluded.duration, \
                    listen_seconds = listening_events.listen_seconds + excluded.listen_seconds, \
                    last_active_at = excluded.last_active_at",
                rusqlite::params![
                    &progress.id,
                    &progress.user_id,
                    &progress.book_id,
                    &progress.chapter_id,
                    &progress.user_id,
                    &progress.book_id,
                    &progress.chapter_id,
                    progress.position,
                    progress.duration,
                    listen_seconds,
                ],
            )
            .map_err(TingError::DatabaseError)?;

            // Keep the all-time aggregate focused on durable listening seconds.
            // `progress_updates` is legacy data; rolling update counts come from
            // the retained daily table instead of growing this row on every tick.
            conn.execute(
                "INSERT INTO listening_totals (\
                    aggregate_key, user_id, book_id, chapter_id, listen_seconds, progress_updates, \
                    first_active_at, last_active_at\
                 ) VALUES (\
                    ? || CHAR(31) || ? || CHAR(31) || COALESCE(?, ''), ?, ?, ?, ?, 0, \
                    STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now')\
                 ) ON CONFLICT(aggregate_key) DO UPDATE SET \
                    listen_seconds = listening_totals.listen_seconds + excluded.listen_seconds, \
                    last_active_at = excluded.last_active_at",
                rusqlite::params![
                    &progress.user_id,
                    &progress.book_id,
                    &progress.chapter_id,
                    &progress.user_id,
                    &progress.book_id,
                    &progress.chapter_id,
                    listen_seconds,
                ],
            )
            .map_err(TingError::DatabaseError)?;

            // Handle NULL chapter_id carefully since SQLite UNIQUE constraint treats NULLs as distinct
            if progress.chapter_id.is_none() {
                // If chapter_id is NULL, we just insert or update based on user_id and book_id
                // This is a fallback case, normally chapter_id should be provided
                let existing_id: Option<String> = conn.query_row(
                    "SELECT id FROM progress WHERE user_id = ? AND book_id = ? AND chapter_id IS NULL",
                    rusqlite::params![&progress.user_id, &progress.book_id],
                    |row| row.get(0)
                ).optional().unwrap_or(None);

                if let Some(id) = existing_id {
                    conn.execute(
                        "UPDATE progress \
                         SET position = ?, duration = ?, updated_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), history_hidden_at = NULL \
                         WHERE id = ?",
                        rusqlite::params![progress.position, progress.duration, id],
                    ).map_err(TingError::DatabaseError)?;
                } else {
                    conn.execute(
                        "INSERT INTO progress (id, user_id, book_id, chapter_id, position, duration, updated_at, history_hidden_at) \
                         VALUES (?, ?, ?, ?, ?, ?, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL)",
                        rusqlite::params![
                            &progress.id,
                            &progress.user_id,
                            &progress.book_id,
                            &progress.chapter_id,
                            progress.position,
                            progress.duration,
                        ],
                    ).map_err(TingError::DatabaseError)?;
                }
            } else {
                conn.execute(
                    "INSERT INTO progress (id, user_id, book_id, chapter_id, position, duration, updated_at, history_hidden_at) \
                     VALUES (?, ?, ?, ?, ?, ?, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL) \
                     ON CONFLICT(user_id, book_id, chapter_id) DO UPDATE SET \
                     position = excluded.position, \
                     duration = excluded.duration, \
                     updated_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                     history_hidden_at = NULL",
                    rusqlite::params![
                        &progress.id,
                        &progress.user_id,
                        &progress.book_id,
                        &progress.chapter_id,
                        progress.position,
                        progress.duration,
                    ],
                ).map_err(TingError::DatabaseError)?;
            }
            conn.execute(
                "INSERT INTO progress_heads(user_id,book_id,revision,device_id,progress_id)
                 VALUES(?1,?2,?3,?4,(SELECT id FROM progress WHERE user_id=?1 AND book_id=?2 AND chapter_id IS ?5 LIMIT 1))
                 ON CONFLICT(user_id,book_id) DO UPDATE SET revision=excluded.revision,device_id=excluded.device_id,progress_id=excluded.progress_id",
                rusqlite::params![progress.user_id,progress.book_id,revision+1,device,progress.chapter_id],
            )?;
            Ok(Some(revision+1))
        }).await
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/db/repository/progress.rs"]
mod tests;

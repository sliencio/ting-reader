use crate::core::app::error::{Result, TingError};
use crate::core::books::series_title::series_identity;
use crate::db::manager::DatabaseManager;
use crate::db::models::{Book, Series, SeriesBook};
use crate::db::repository::base::Repository;
use crate::db::repository::book::map_book_row;
use async_trait::async_trait;
use rusqlite::{OptionalExtension, Row};
use std::sync::Arc;

fn map_series_row(row: &Row<'_>) -> rusqlite::Result<Series> {
    Ok(Series {
        id: row.get(0)?,
        library_id: row.get(1)?,
        title: row.get(2)?,
        author: row.get(3)?,
        narrator: row.get(4)?,
        cover_url: row.get(5)?,
        description: row.get(6)?,
        // Older databases may contain NULL timestamps even though the current
        // schema declares defaults. Keep those rows readable through the API.
        created_at: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
    })
}

/// Repository for Series entities
pub struct SeriesRepository {
    db: Arc<DatabaseManager>,
}

impl SeriesRepository {
    /// Create a new SeriesRepository
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    /// Find series by library ID
    pub async fn find_by_library(&self, library_id: &str) -> Result<Vec<Series>> {
        let library_id = library_id.to_string();
        self.db.execute(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                 FROM series WHERE library_id = ? ORDER BY created_at DESC"
            ).map_err(TingError::DatabaseError)?;

            let series = stmt.query_map([&library_id], map_series_row)
                .map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(series)
        }).await
    }

    /// Find or create a series atomically within one library.
    ///
    /// Identity is `title_key`, not the raw title: `斗破苍穹 第2卷` and
    /// `斗破苍穹` are the same collection no matter which source wrote it.
    /// The write lock is taken up front (IMMEDIATE) so two concurrent scans
    /// cannot both read "missing" and race into the unique index.
    pub async fn find_or_create_by_title(&self, new_series: Series) -> Result<Series> {
        let identity = series_identity(&new_series.title);
        let mut new_series = new_series;
        new_series.title = identity.title;
        let search_key = identity.key;
        self.db
            .transaction_immediate(move |tx| {
                let existing = tx.query_row(
                    "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                     FROM series WHERE title_key = ? AND library_id = ?",
                    rusqlite::params![&search_key, &new_series.library_id],
                    map_series_row,
                ).optional().map_err(TingError::DatabaseError)?;

                if let Some(s) = existing {
                    Ok(s)
                } else {
                    tx.execute(
                        "INSERT INTO series (id, library_id, title, title_key, author, narrator, cover_url, description, created_at, updated_at) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        rusqlite::params![
                            &new_series.id,
                            &new_series.library_id,
                            &new_series.title,
                            &search_key,
                            &new_series.author,
                            &new_series.narrator,
                            &new_series.cover_url,
                            &new_series.description,
                            &new_series.created_at,
                            &new_series.updated_at,
                        ],
                    ).map_err(TingError::DatabaseError)?;
                    Ok(new_series)
                }
            })
            .await
    }

    /// Look up a series by its identity key inside one library.
    pub async fn find_by_key(&self, library_id: &str, title_key: &str) -> Result<Option<Series>> {
        let library_id = library_id.to_string();
        let title_key = title_key.to_string();
        self.db
            .execute(move |conn| {
                conn.query_row(
                    "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                     FROM series WHERE title_key = ? AND library_id = ?",
                    rusqlite::params![&title_key, &library_id],
                    map_series_row,
                )
                .optional()
                .map_err(TingError::DatabaseError)
            })
            .await
    }

    /// Find series by title globally (across all libraries)
    pub async fn find_by_title(&self, title: &str) -> Result<Option<Series>> {
        let key = series_identity(title).key;
        self.db.execute(move |conn| {
            conn.query_row(
                "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                 FROM series WHERE title_key = ?",
                rusqlite::params![&key],
                map_series_row
            ).optional()
            .map_err(TingError::DatabaseError)
        }).await
    }

    /// Find series by title and library
    pub async fn find_by_title_and_library(
        &self,
        title: &str,
        library_id: &str,
    ) -> Result<Option<Series>> {
        let key = series_identity(title).key;
        let library_id = library_id.to_string();
        self.db.execute(move |conn| {
            conn.query_row(
                "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                 FROM series WHERE title_key = ? AND library_id = ?",
                rusqlite::params![&key, &library_id],
                map_series_row
            ).optional()
            .map_err(TingError::DatabaseError)
        }).await
    }

    /// Find series for a book
    pub async fn find_series_by_book(&self, book_id: &str) -> Result<Vec<Series>> {
        let book_id = book_id.to_string();
        self.db.execute(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT s.id, s.library_id, s.title, s.author, s.narrator, s.cover_url, s.description, s.created_at, s.updated_at \
                 FROM series s \
                 JOIN series_books sb ON s.id = sb.series_id \
                 WHERE sb.book_id = ?"
            ).map_err(TingError::DatabaseError)?;

            let series = stmt.query_map([&book_id], map_series_row)
                .map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(series)
        }).await
    }

    /// Find series with filters and access control
    pub async fn find_with_filters(
        &self,
        user_id: &str,
        is_admin: bool,
        library_id: Option<String>,
    ) -> Result<Vec<Series>> {
        let user_id = user_id.to_string();
        self.db.execute(move |conn| {
            let mut query = "SELECT s.id, s.library_id, s.title, s.author, s.narrator, s.cover_url, s.description, s.created_at, s.updated_at \
                             FROM series s".to_string();
            let mut params: Vec<String> = Vec::new();
            let mut conditions: Vec<String> = Vec::new();

            if let Some(lid) = library_id {
                conditions.push("s.library_id = ?".to_string());
                params.push(lid);
            }

            // Access Control
            if !is_admin {
                conditions.push("(
                    s.library_id IN (SELECT library_id FROM user_library_access WHERE user_id = ?)
                    OR
                    s.id IN (SELECT series_id FROM series_books WHERE book_id IN (SELECT book_id FROM user_book_access WHERE user_id = ?))
                )".to_string());
                params.push(user_id.clone());
                params.push(user_id.clone());
            }

            if !conditions.is_empty() {
                query += " WHERE ";
                query += &conditions.join(" AND ");
            }

            query += " ORDER BY s.created_at DESC";

            let mut stmt = conn.prepare(&query).map_err(TingError::DatabaseError)?;

            let series = stmt.query_map(rusqlite::params_from_iter(params.iter()), map_series_row)
                .map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(series)
        }).await
    }

    /// Check if a user has access to a series
    pub async fn check_access(
        &self,
        series_id: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<bool> {
        if is_admin {
            return Ok(true);
        }

        let series_id = series_id.to_string();
        let user_id = user_id.to_string();

        self.db.execute(move |conn| {
            let has_access: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM series s
                    WHERE s.id = ? AND (
                        s.library_id IN (SELECT library_id FROM user_library_access WHERE user_id = ?)
                        OR
                        s.id IN (SELECT series_id FROM series_books WHERE book_id IN (SELECT book_id FROM user_book_access WHERE user_id = ?))
                    )
                )",
                rusqlite::params![&series_id, &user_id, &user_id],
                |row| row.get(0),
            ).unwrap_or(false);

            Ok(has_access)
        }).await
    }

    /// Find books in a series
    pub async fn find_books_by_series(&self, series_id: &str) -> Result<Vec<(Book, i32)>> {
        let series_id = series_id.to_string();
        self.db.execute(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT b.id, b.library_id, b.title, b.author, b.narrator, b.cover_url, b.theme_color, \
                 b.description, b.skip_intro, b.skip_outro, b.path, b.hash, b.tags, b.genre, b.year, b.created_at, \
                 b.manual_corrected, b.match_pattern, b.chapter_regex, sb.book_order \
                 FROM books b \
                 JOIN series_books sb ON b.id = sb.book_id \
                 WHERE sb.series_id = ? \
                 ORDER BY sb.book_order ASC"
            ).map_err(TingError::DatabaseError)?;

            let books = stmt.query_map([&series_id], |row| {
                let book = map_book_row(row)?;
                let order: i32 = row.get(19)?;
                Ok((book, order))
            }).map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(books)
        }).await
    }

    /// Find books in a series with filters and access control
    pub async fn find_books_by_series_with_filters(
        &self,
        series_id: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<(Book, i32)>> {
        let series_id = series_id.to_string();
        let user_id = user_id.to_string();
        self.db.execute(move |conn| {
            let mut query = "SELECT b.id, b.library_id, b.title, b.author, b.narrator, b.cover_url, b.theme_color, \
                             b.description, b.skip_intro, b.skip_outro, b.path, b.hash, b.tags, b.genre, b.year, b.created_at, \
                             b.manual_corrected, b.match_pattern, b.chapter_regex, sb.book_order \
                             FROM books b \
                             JOIN series_books sb ON b.id = sb.book_id \
                             WHERE sb.series_id = ?".to_string();
            let mut params: Vec<String> = vec![series_id];

            if !is_admin {
                query += " AND (
                    b.library_id IN (SELECT library_id FROM user_library_access WHERE user_id = ?)
                    OR
                    b.id IN (SELECT book_id FROM user_book_access WHERE user_id = ?)
                )";
                params.push(user_id.clone());
                params.push(user_id.clone());
            }

            query += " ORDER BY sb.book_order ASC";

            let mut stmt = conn.prepare(&query).map_err(TingError::DatabaseError)?;

            let books = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
                let book = map_book_row(row)?;
                let order: i32 = row.get(19)?;
                Ok((book, order))
            }).map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(books)
        }).await
    }

    /// Add a book to a series.
    ///
    /// Callers here are explicit (sidecar declarations and admin curation), so
    /// the link is upgraded away from `inferred` — otherwise a later rescan
    /// would be allowed to drop a membership the user or the sidecar asked for.
    pub async fn add_book(&self, series_book: SeriesBook) -> Result<()> {
        self.db
            .execute(move |conn| {
                conn.execute(
                    "INSERT INTO series_books (series_id, book_id, book_order, source) \
                     VALUES (?, ?, ?, '') \
                     ON CONFLICT(series_id, book_id) DO UPDATE SET \
                     book_order = excluded.book_order, source = ''",
                    rusqlite::params![
                        &series_book.series_id,
                        &series_book.book_id,
                        series_book.book_order
                    ],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    /// Attach a book to a directory-inferred series, replacing any previous
    /// inferred membership in the same library.
    ///
    /// A book belongs to at most one inferred series. Without the delete, a
    /// book that moves from `书名 第1卷` to `书名` (or whose parent directory
    /// is renamed) stayed in both, which is what made series look overlapped.
    ///
    /// Two limits keep this from fighting explicit curation:
    /// * it never *adds* a guessed series to a book that already has a curated
    ///   one (`source = ''`), so a sidecar declaration stays authoritative;
    /// * it does still refresh the order of a link that already exists, so
    ///   legacy links keep tracking their directory instead of going stale.
    pub async fn link_inferred_book(
        &self,
        library_id: &str,
        series_id: &str,
        book_id: &str,
        book_order: i32,
    ) -> Result<()> {
        let library_id = library_id.to_string();
        let series_id = series_id.to_string();
        let book_id = book_id.to_string();
        self.db
            .transaction(move |tx| {
                let explicit: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM series_books WHERE book_id = ? AND source = '')",
                        rusqlite::params![&book_id],
                        |row| row.get(0),
                    )
                    .map_err(TingError::DatabaseError)?;
                let already_linked: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM series_books WHERE series_id = ? AND book_id = ?)",
                        rusqlite::params![&series_id, &book_id],
                        |row| row.get(0),
                    )
                    .map_err(TingError::DatabaseError)?;
                if explicit && !already_linked {
                    return Ok(());
                }
                tx.execute(
                    "DELETE FROM series_books WHERE book_id = ? AND source = 'inferred' \
                     AND series_id IN (SELECT id FROM series WHERE library_id = ?) \
                     AND series_id <> ?",
                    rusqlite::params![&book_id, &library_id, &series_id],
                )
                .map_err(TingError::DatabaseError)?;
                // Keep an existing row's owner: a curated link that happens to
                // match the guessed series must not become inference-owned.
                tx.execute(
                    "INSERT INTO series_books (series_id, book_id, book_order, source) \
                     VALUES (?, ?, ?, 'inferred') \
                     ON CONFLICT(series_id, book_id) DO UPDATE SET \
                     book_order = excluded.book_order",
                    rusqlite::params![&series_id, &book_id, book_order],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    /// Release inferred memberships in one library that an explicit sidecar
    /// declaration is replacing.
    ///
    /// A book with `series` entries in `metadata.json` is explicitly placed by
    /// the user, so a directory guess must not keep it in a second collection.
    /// Only rows inference created are dropped; memberships the user curated
    /// by hand have `source = ''` and survive.
    pub async fn release_inferred_memberships(
        &self,
        library_id: &str,
        book_id: &str,
        keep_series_ids: Vec<String>,
    ) -> Result<()> {
        let library_id = library_id.to_string();
        let book_id = book_id.to_string();
        self.db
            .transaction(move |tx| {
                for series_id in &keep_series_ids {
                    tx.execute(
                        "UPDATE series_books SET source = '' WHERE series_id = ? AND book_id = ?",
                        rusqlite::params![series_id, &book_id],
                    )
                    .map_err(TingError::DatabaseError)?;
                }
                tx.execute(
                    "DELETE FROM series_books WHERE book_id = ? AND source = 'inferred' \
                     AND series_id IN (SELECT id FROM series WHERE library_id = ?)",
                    rusqlite::params![&book_id, &library_id],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    /// Remove a book from a series
    pub async fn remove_book(&self, series_id: &str, book_id: &str) -> Result<()> {
        let series_id = series_id.to_string();
        let book_id = book_id.to_string();
        self.db
            .execute(move |conn| {
                conn.execute(
                    "DELETE FROM series_books WHERE series_id = ? AND book_id = ?",
                    [&series_id, &book_id],
                )
                .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }

    /// Update book orders in a series
    pub async fn update_book_orders(
        &self,
        series_id: &str,
        orders: Vec<(String, i32)>,
    ) -> Result<()> {
        let series_id = series_id.to_string();
        self.db
            .transaction(move |tx| {
                for (book_id, order) in orders {
                    tx.execute(
                    "UPDATE series_books SET book_order = ? WHERE series_id = ? AND book_id = ?",
                    rusqlite::params![order, &series_id, &book_id],
                ).map_err(TingError::DatabaseError)?;
                }
                Ok(())
            })
            .await
    }
}

#[async_trait]
impl Repository<Series> for SeriesRepository {
    async fn find_by_id(&self, id: &str) -> Result<Option<Series>> {
        let id = id.to_string();
        self.db.execute(move |conn| {
            conn.query_row(
                "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                 FROM series WHERE id = ?",
                [&id],
                map_series_row
            ).optional()
            .map_err(TingError::DatabaseError)
        }).await
    }

    async fn find_all(&self) -> Result<Vec<Series>> {
        self.db.execute(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, library_id, title, author, narrator, cover_url, description, created_at, updated_at \
                 FROM series ORDER BY created_at DESC"
            ).map_err(TingError::DatabaseError)?;

            let series = stmt.query_map([], map_series_row)
                .map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;

            Ok(series)
        }).await
    }

    async fn create(&self, series: &Series) -> Result<()> {
        let series = series.clone();
        let identity = series_identity(&series.title);
        self.db.execute(move |conn| {
            conn.execute(
                "INSERT INTO series (id, library_id, title, title_key, author, narrator, cover_url, description, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    &series.id,
                    &series.library_id,
                    &identity.title,
                    &identity.key,
                    &series.author,
                    &series.narrator,
                    &series.cover_url,
                    &series.description,
                    &series.created_at,
                    &series.updated_at,
                ],
            ).map_err(TingError::DatabaseError)?;
            Ok(())
        }).await
    }

    async fn update(&self, series: &Series) -> Result<()> {
        let series = series.clone();
        let identity = series_identity(&series.title);
        self.db.execute(move |conn| {
            conn.execute(
                "UPDATE series SET library_id = ?, title = ?, title_key = ?, author = ?, narrator = ?, \
                 cover_url = ?, description = ?, updated_at = STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
                rusqlite::params![
                    &series.library_id,
                    &identity.title,
                    &identity.key,
                    &series.author,
                    &series.narrator,
                    &series.cover_url,
                    &series.description,
                    &series.id,
                ],
            ).map_err(TingError::DatabaseError)?;
            Ok(())
        }).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.db
            .execute(move |conn| {
                conn.execute("DELETE FROM series WHERE id = ?", [&id])
                    .map_err(TingError::DatabaseError)?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/db/repository/series.rs"]
mod tests;

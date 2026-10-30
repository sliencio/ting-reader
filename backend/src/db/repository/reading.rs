use crate::core::app::error::{InputErrorKind, Result, TingError};
use crate::db::manager::DatabaseManager;
use rusqlite::{OptionalExtension, named_params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Serialize)]
pub struct ActivityBook {
    pub book_id: String,
    pub book_title: Option<String>,
    pub cover_url: Option<String>,
    pub library_id: String,
    pub chapter_count: usize,
    pub updated_at: String,
    pub latest_chapter_id: Option<String>,
    pub latest_chapter_title: Option<String>,
    pub latest_position: f64,
    pub latest_duration: f64,
    pub latest_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Bookmark {
    pub id: String,
    pub book_id: String,
    pub chapter_id: String,
    pub chapter_title: Option<String>,
    pub chapter_duration: f64,
    pub position: f64,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct HistorySummary {
    pub books: usize,
    pub chapters: usize,
    pub position_seconds: f64,
}

pub struct ReadingRepository {
    db: Arc<DatabaseManager>,
}

// Apply access rules in the paginated query, so revoked books neither appear
// in results nor distort totals. User ID always scopes the underlying records.
const ACCESS: &str = "(:admin OR b.library_id IN
    (SELECT library_id FROM user_library_access WHERE user_id = :user)
    OR b.id IN (SELECT book_id FROM user_book_access WHERE user_id = :user))";

impl ReadingRepository {
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    pub async fn history_summary(&self, user: &str, admin: bool) -> Result<HistorySummary> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                Ok(conn.query_row(
                    &format!(
                        "SELECT COUNT(DISTINCT b.id),COUNT(*),COALESCE(SUM(MAX(0,r.position)),0)
                 FROM progress r JOIN books b ON b.id=r.book_id
                 WHERE r.user_id=:user AND r.chapter_id IS NOT NULL
                 AND r.history_hidden_at IS NULL AND {ACCESS}"
                    ),
                    named_params! {":user":user,":admin":admin},
                    |row| {
                        Ok(HistorySummary {
                            books: row.get(0)?,
                            chapters: row.get(1)?,
                            position_seconds: row.get(2)?,
                        })
                    },
                )?)
            })
            .await
    }

    pub async fn percentages(&self, user: &str) -> Result<HashMap<String, f64>> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.book_id, 100.0 * SUM(CASE
                    WHEN m.chapter_id IS NOT NULL THEN 1.0
                    WHEN COALESCE(NULLIF(c.duration, 0), NULLIF(p.duration, 0), 0) > 0
                    THEN MIN(1.0, MAX(0.0, p.position) /
                         COALESCE(NULLIF(c.duration, 0), NULLIF(p.duration, 0)))
                    ELSE 0 END) / (SELECT COUNT(*) FROM chapters WHERE book_id=c.book_id)
                 FROM (SELECT chapter_id FROM progress WHERE user_id=?1
                       UNION SELECT chapter_id FROM chapter_read_marks WHERE user_id=?1) r
                 JOIN chapters c ON c.id=r.chapter_id
                 LEFT JOIN progress p ON p.chapter_id=c.id AND p.book_id=c.book_id AND p.user_id=?1
                 LEFT JOIN chapter_read_marks m ON m.chapter_id=c.id AND m.book_id=c.book_id AND m.user_id=?1
                 GROUP BY c.book_id",
                )?;
                let mut values = stmt
                    .query_map([&user], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                    })?
                    .collect::<std::result::Result<HashMap<_, _>, _>>()?;
                let mut marks =
                    conn.prepare("SELECT book_id FROM book_read_marks WHERE user_id = ?")?;
                for book in marks.query_map([&user], |row| row.get::<_, String>(0))? {
                    values.insert(book?, 100.0);
                }
                Ok(values)
            })
            .await
    }

    pub async fn mark_books(&self, user: &str, books: Vec<String>, read: bool) -> Result<()> {
        let user = user.to_owned();
        self.db
            .transaction(move |conn| {
                for book in books {
                    if read {
                        conn.execute(
                            "INSERT OR IGNORE INTO book_read_marks(user_id,book_id) VALUES(?,?)",
                            [&user, &book],
                        )?;
                    } else {
                        conn.execute(
                            "DELETE FROM chapter_read_marks WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                        conn.execute(
                            "DELETE FROM progress WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                        conn.execute(
                            "DELETE FROM book_read_marks WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                    }
                }
                Ok(())
            })
            .await
    }

    pub async fn chapter_read_marks(&self, user: &str, book: &str) -> Result<HashSet<String>> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db
            .execute(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT chapter_id FROM chapter_read_marks WHERE user_id=? AND book_id=?",
                )?;
                Ok(stmt
                    .query_map([user, book], |row| row.get(0))?
                    .collect::<std::result::Result<HashSet<_>, _>>()?)
            })
            .await
    }

    pub async fn mark_chapters(
        &self,
        user: &str,
        book: &str,
        chapters: Vec<String>,
        read: bool,
    ) -> Result<()> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.transaction(move |conn| {
            // 先校验整批章节，再写入；错误章节不能造成部分成功。
            for chapter in &chapters {
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chapters WHERE id=? AND book_id=?)",
                    [chapter, &book], |row| row.get(0),
                )?;
                if !exists {
                    return Err(TingError::invalid_request(
                        InputErrorKind::InvalidValue,
                        "mark_chapters::invalid_chapter",
                        "章节不存在或不属于当前书籍".into(),
                    ));
                }
            }
            for chapter in chapters {
                if read {
                    conn.execute(
                        "INSERT OR IGNORE INTO chapter_read_marks(user_id,book_id,chapter_id) VALUES(?,?,?)",
                        [&user, &book, &chapter],
                    )?;
                } else {
                    conn.execute(
                        "DELETE FROM chapter_read_marks WHERE user_id=? AND book_id=? AND chapter_id=?",
                        [&user, &book, &chapter],
                    )?;
                }
            }
            Ok(())
        }).await
    }

    pub async fn activity_books(
        &self,
        user: &str,
        admin: bool,
        bookmarks: bool,
        page: usize,
        page_size: usize,
    ) -> Result<Page<ActivityBook>> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                let (table, visible) = if bookmarks {
                    ("bookmarks", "")
                } else {
                    (
                        "progress",
                        "AND r.history_hidden_at IS NULL AND r.chapter_id IS NOT NULL",
                    )
                };
                let from = format!(
                    "FROM {table} r JOIN books b ON b.id=r.book_id
                WHERE r.user_id=:user {visible} AND {ACCESS}"
                );
                let total: usize = conn.query_row(
                    &format!("SELECT COUNT(DISTINCT b.id) {from}"),
                    named_params! {":user": user, ":admin": admin},
                    |row| row.get(0),
                )?;
                let duration = if bookmarks {
                    "COALESCE(c.duration,0)"
                } else {
                    "COALESCE(NULLIF(c.duration,0),latest.duration,0)"
                };
                let note = if bookmarks { "latest.note" } else { "NULL" };
                let mut stmt = conn.prepare(&format!(
                    "WITH activity AS (
                        SELECT b.id AS book_id,COUNT(*) AS count,MAX(r.updated_at) AS updated_at
                        {from} GROUP BY b.id ORDER BY MAX(r.updated_at) DESC,b.id
                        LIMIT :limit OFFSET :offset
                    )
                    SELECT b.id,b.title,b.cover_url,b.library_id,a.count,a.updated_at,
                           latest.chapter_id,c.title,latest.position,{duration},{note}
                    FROM activity a JOIN books b ON b.id=a.book_id
                    JOIN {table} latest ON latest.id=(
                        SELECT r.id FROM {table} r
                        WHERE r.user_id=:user AND r.book_id=a.book_id {visible}
                        ORDER BY r.updated_at DESC,r.id LIMIT 1
                    )
                    LEFT JOIN chapters c ON c.id=latest.chapter_id
                    ORDER BY a.updated_at DESC,b.id"
                ))?;
                let items = stmt.query_map(named_params!{
                ":user":user, ":admin":admin, ":limit":page_size, ":offset":(page-1)*page_size
            }, |row| Ok(ActivityBook {
                book_id: row.get(0)?, book_title: row.get(1)?, cover_url: row.get(2)?,
                library_id: row.get(3)?, chapter_count: row.get(4)?, updated_at: row.get(5)?,
                latest_chapter_id:row.get(6)?,latest_chapter_title:row.get(7)?,
                latest_position:row.get(8)?,latest_duration:row.get(9)?,latest_note:row.get(10)?,
            }))?.collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(Page {
                    items,
                    total,
                    page,
                    page_size,
                })
            })
            .await
    }

    pub async fn history_chapters(
        &self,
        user: &str,
        book: &str,
        page: usize,
        page_size: usize,
    ) -> Result<Page<crate::api::models::ProgressResponse>> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.execute(move |conn| {
            let total = conn.query_row("SELECT COUNT(*) FROM progress
                WHERE user_id=? AND book_id=? AND history_hidden_at IS NULL AND chapter_id IS NOT NULL",
                [&user,&book], |row| row.get(0))?;
            let mut stmt = conn.prepare("SELECT p.id,p.chapter_id,p.position,p.duration,p.updated_at,c.title,c.duration
                FROM progress p JOIN chapters c ON c.id=p.chapter_id
                WHERE p.user_id=?1 AND p.book_id=?2 AND p.history_hidden_at IS NULL
                ORDER BY p.updated_at DESC,p.id LIMIT ?3 OFFSET ?4")?;
            let items = stmt.query_map(rusqlite::params![user,book,page_size,(page-1)*page_size], |row| {
                Ok(crate::api::models::ProgressResponse {
                    id:row.get(0)?, user_id:user.clone(),book_id:book.clone(),chapter_id:row.get(1)?,
                    position:row.get(2)?,duration:row.get(3)?,updated_at:row.get(4)?,
                    chapter_title:row.get(5)?,chapter_duration:row.get(6)?,
                    book_title:None,cover_url:None,library_id:None,
                })
            })?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(Page {items,total,page,page_size})
        }).await
    }

    pub async fn clear_history(
        &self,
        user: &str,
        all: bool,
        books: Vec<String>,
        ids: Vec<String>,
        chapters: Vec<String>,
        clear_progress: bool,
    ) -> Result<usize> {
        let user = user.to_owned();
        self.db
            .transaction(move |conn| {
                // Select each row once, regardless of overlapping book/chapter selections.
                let mut parameters = vec![rusqlite::types::Value::from(user.clone())];
                let mut clauses = Vec::new();
                let explicitly_selected_books = books.clone();
                if clear_progress {
                    if all {
                        conn.execute("DELETE FROM chapter_read_marks WHERE user_id=?", [&user])?;
                    } else {
                        for book in &books {
                            conn.execute(
                                "DELETE FROM chapter_read_marks WHERE user_id=? AND book_id=?",
                                [&user, book],
                            )?;
                        }
                        for chapter in &chapters {
                            conn.execute(
                                "DELETE FROM chapter_read_marks WHERE user_id=? AND chapter_id=?",
                                [&user, chapter],
                            )?;
                        }
                        for id in &ids {
                            conn.execute(
                                "DELETE FROM chapter_read_marks WHERE user_id=?1 AND chapter_id IN
                                (SELECT chapter_id FROM progress WHERE user_id=?1 AND id=?2)",
                                [&user, id],
                            )?;
                        }
                    }
                }
                for (column, values) in [("book_id", books), ("id", ids), ("chapter_id", chapters)]
                {
                    if !values.is_empty() {
                        let slots = std::iter::repeat_n("?", values.len())
                            .collect::<Vec<_>>()
                            .join(",");
                        clauses.push(format!("{column} IN ({slots})"));
                        parameters.extend(values.into_iter().map(rusqlite::types::Value::from));
                    }
                }
                if !all && clauses.is_empty() {
                    return Ok(0);
                }
                let scope = if all {
                    "1".to_string()
                } else {
                    clauses.join(" OR ")
                };
                if clear_progress {
                    let sql = format!(
                        "SELECT DISTINCT book_id FROM progress WHERE user_id=? AND ({scope})"
                    );
                    let affected_books = conn
                        .prepare(&sql)?
                        .query_map(rusqlite::params_from_iter(&parameters), |row| {
                            row.get::<_, String>(0)
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    for book in affected_books.into_iter().chain(explicitly_selected_books) {
                        conn.execute(
                            "DELETE FROM book_read_marks WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                    }
                    if all {
                        conn.execute("DELETE FROM book_read_marks WHERE user_id=?", [&user])?;
                    }
                }
                let action = if clear_progress {
                    "DELETE FROM progress"
                } else {
                    "UPDATE progress SET history_hidden_at=STRFTIME('%Y-%m-%dT%H:%M:%fZ','now')"
                };
                let visible = if clear_progress {
                    ""
                } else {
                    "AND history_hidden_at IS NULL"
                };
                Ok(conn.execute(
                    &format!("{action} WHERE user_id=? AND ({scope}) {visible}"),
                    rusqlite::params_from_iter(parameters),
                )?)
            })
            .await
    }

    pub async fn bookmarks(
        &self,
        user: &str,
        book: &str,
        page: usize,
        page_size: usize,
    ) -> Result<Page<Bookmark>> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.execute(move |conn| {
            let total = conn.query_row("SELECT COUNT(*) FROM bookmarks WHERE user_id=? AND book_id=?",
                [&user,&book], |row| row.get(0))?;
            let mut stmt = conn.prepare("SELECT m.id,m.book_id,m.chapter_id,c.title,m.position,m.note,m.created_at,m.updated_at,
                COALESCE(NULLIF(c.duration,0),p.duration,0)
                FROM bookmarks m JOIN chapters c ON c.id=m.chapter_id
                LEFT JOIN progress p ON p.user_id=m.user_id AND p.book_id=m.book_id AND p.chapter_id=m.chapter_id
                WHERE m.user_id=?1 AND m.book_id=?2 ORDER BY m.updated_at DESC,m.id LIMIT ?3 OFFSET ?4")?;
            let items = stmt.query_map(rusqlite::params![user,book,page_size,(page-1)*page_size], |r| Ok(Bookmark {
                id:r.get(0)?, book_id:r.get(1)?, chapter_id:r.get(2)?, chapter_title:r.get(3)?,
                position:r.get(4)?,note:r.get(5)?,created_at:r.get(6)?,updated_at:r.get(7)?,
                chapter_duration:r.get(8)?,
            }))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(Page {items,total,page,page_size})
        }).await
    }

    pub async fn bookmark(&self, user: &str, id: &str) -> Result<Option<Bookmark>> {
        let user = user.to_owned();
        let id = id.to_owned();
        self.db.execute(move |conn| {
            Ok(conn.query_row(
                "SELECT m.id,m.book_id,m.chapter_id,c.title,m.position,m.note,m.created_at,m.updated_at,
                        COALESCE(NULLIF(c.duration,0),p.duration,0)
                 FROM bookmarks m JOIN chapters c ON c.id=m.chapter_id
                 LEFT JOIN progress p ON p.user_id=m.user_id AND p.book_id=m.book_id AND p.chapter_id=m.chapter_id
                 WHERE m.user_id=?1 AND m.id=?2",
                [&user, &id],
                |row| Ok(Bookmark {
                    id: row.get(0)?, book_id: row.get(1)?, chapter_id: row.get(2)?,
                    chapter_title: row.get(3)?, position: row.get(4)?, note: row.get(5)?,
                    created_at: row.get(6)?, updated_at: row.get(7)?, chapter_duration: row.get(8)?,
                }),
            ).optional()?)
        }).await
    }

    pub async fn add_bookmark(
        &self,
        user: &str,
        book: String,
        chapter: String,
        position: f64,
        note: String,
    ) -> Result<Bookmark> {
        let user = user.to_owned();
        self.db.execute(move |conn| {
            let now = chrono::Utc::now().to_rfc3339(); let id = uuid::Uuid::new_v4().to_string();
            conn.execute("INSERT INTO bookmarks(id,user_id,book_id,chapter_id,position,note,created_at,updated_at)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?7)", rusqlite::params![id,user,book,chapter,position,note,now])?;
            let (chapter_title, chapter_duration) = conn.query_row(
                "SELECT c.title,COALESCE(NULLIF(c.duration,0),p.duration,0) FROM chapters c
                 LEFT JOIN progress p ON p.chapter_id=c.id AND p.book_id=c.book_id AND p.user_id=?2 WHERE c.id=?1",
                [&chapter, &user], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(Bookmark {id,book_id:book,chapter_id:chapter,chapter_title,chapter_duration,position,note,created_at:now.clone(),updated_at:now})
        }).await
    }

    pub async fn change_bookmark(
        &self,
        user: &str,
        id: &str,
        note: Option<String>,
    ) -> Result<usize> {
        let user = user.to_owned();
        let id = id.to_owned();
        self.db
            .execute(move |c| {
                Ok(if let Some(note) = note {
                    c.execute(
                        "UPDATE bookmarks SET note=?,updated_at=? WHERE user_id=? AND id=?",
                        [note, chrono::Utc::now().to_rfc3339(), user, id],
                    )?
                } else {
                    c.execute("DELETE FROM bookmarks WHERE user_id=? AND id=?", [user, id])?
                })
            })
            .await
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/db/repository/reading.rs"]
mod tests;

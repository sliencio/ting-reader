use super::*;
use crate::db::models::Progress;
use crate::db::repository::ProgressRepository;

async fn fixture(chapters: usize) -> (Arc<DatabaseManager>, ReadingRepository) {
    let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
    db.transaction(move |conn| {
        conn.execute_batch(
            "INSERT INTO users(id,username,password_hash) VALUES('u1','one','hash'),('u2','two','hash');
             INSERT INTO libraries(id,name,type,url) VALUES('library','Test','local','');
             INSERT INTO books(id,library_id,title,path,hash) VALUES('book','library','Book','/book','hash');
             INSERT INTO user_library_access(user_id,library_id) VALUES('u1','library');",
        )?;
        for index in 0..chapters {
            let id = format!("c{index:05}");
            conn.execute(
                "INSERT INTO chapters(id,book_id,title,path,duration) VALUES(?1,'book',?1,?1,100)",
                [&id],
            )?;
        }
        Ok(())
    }).await.unwrap();
    (db.clone(), ReadingRepository::new(db))
}

async fn add_progress(db: &Arc<DatabaseManager>, user: &str, chapter: &str, position: f64) {
    ProgressRepository::new(db.clone())
        .upsert(&Progress {
            id: format!("{user}:{chapter}"),
            user_id: user.into(),
            book_id: "book".into(),
            chapter_id: Some(chapter.into()),
            position,
            duration: Some(100.0),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn chapter_read_marks_preserve_resume_activity_and_user_isolation() {
    let (db, repo) = fixture(3).await;
    add_progress(&db, "u1", "c00000", 30.0).await;
    let progress = ProgressRepository::new(db.clone());
    let before = progress.get_by_book("u1", "book").await.unwrap();
    repo.mark_chapters(
        "u1",
        "book",
        vec!["c00001".into(), "c00002".into(), "c00001".into()],
        true,
    )
    .await
    .unwrap();
    let after = progress.get_by_book("u1", "book").await.unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert!((repo.percentages("u1").await.unwrap()["book"] - 230.0 / 3.0).abs() < 0.001);
    assert!(
        repo.chapter_read_marks("u2", "book")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repo.history_chapters("u1", "book", 1, 40)
            .await
            .unwrap()
            .total,
        1
    );
    let total: f64 = db
        .execute(|conn| {
            Ok(conn.query_row(
                "SELECT SUM(listen_seconds) FROM listening_totals WHERE user_id='u1'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(total, 30.0);
    repo.mark_chapters("u1", "book", vec!["c00001".into()], false)
        .await
        .unwrap();
    assert_eq!(
        repo.chapter_read_marks("u1", "book").await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn chapter_read_batch_is_atomic_and_unknown_duration_is_supported() {
    let (db, repo) = fixture(2).await;
    db.execute(|conn| {
        conn.execute("UPDATE chapters SET duration=0", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        repo.mark_chapters("u1", "book", vec!["c00000".into(), "missing".into()], true)
            .await
            .is_err()
    );
    assert!(
        repo.chapter_read_marks("u1", "book")
            .await
            .unwrap()
            .is_empty()
    );
    repo.mark_chapters("u1", "book", vec!["c00000".into()], true)
        .await
        .unwrap();
    assert_eq!(repo.percentages("u1").await.unwrap()["book"], 50.0);
    assert_eq!(repo.history_summary("u1", false).await.unwrap().chapters, 0);
    repo.clear_history("u1", false, vec!["book".into()], vec![], vec![], true)
        .await
        .unwrap();
    assert!(
        repo.chapter_read_marks("u1", "book")
            .await
            .unwrap()
            .is_empty()
    );
    repo.mark_chapters("u1", "book", vec!["c00001".into()], true)
        .await
        .unwrap();
    repo.mark_books("u1", vec!["book".into()], false)
        .await
        .unwrap();
    assert!(
        repo.chapter_read_marks("u1", "book")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn read_marks_are_user_scoped_and_do_not_invent_listening_activity() {
    let (db, repo) = fixture(2).await;
    add_progress(&db, "u1", "c00000", 50.0).await;
    add_progress(&db, "u2", "c00000", 100.0).await;
    assert_eq!(repo.percentages("u1").await.unwrap()["book"], 25.0);
    repo.mark_books("u1", vec!["book".into()], true)
        .await
        .unwrap();
    assert_eq!(repo.percentages("u1").await.unwrap()["book"], 100.0);
    assert_eq!(repo.percentages("u2").await.unwrap()["book"], 50.0);
    let totals_before: f64 = db
        .execute(|conn| {
            Ok(conn.query_row(
                "SELECT SUM(listen_seconds) FROM listening_totals",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    repo.mark_books("u1", vec!["book".into()], false)
        .await
        .unwrap();
    assert!(!repo.percentages("u1").await.unwrap().contains_key("book"));
    assert_eq!(
        repo.history_chapters("u1", "book", 1, 40)
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        repo.history_chapters("u2", "book", 1, 40)
            .await
            .unwrap()
            .total,
        1
    );
    let totals_after: f64 = db
        .execute(|conn| {
            Ok(conn.query_row(
                "SELECT SUM(listen_seconds) FROM listening_totals",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(totals_before, totals_after);
}

#[tokio::test]
async fn hiding_history_preserves_resume_and_clear_progress_removes_hidden_chapters() {
    let (db, repo) = fixture(2).await;
    add_progress(&db, "u1", "c00000", 50.0).await;
    add_progress(&db, "u1", "c00001", 100.0).await;
    let hidden = repo
        .clear_history(
            "u1",
            false,
            vec!["book".into()],
            vec!["u1:c00000".into()],
            vec![],
            false,
        )
        .await
        .unwrap();
    assert_eq!(hidden, 2);
    assert_eq!(
        repo.activity_books("u1", false, false, 1, 40)
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(repo.percentages("u1").await.unwrap()["book"], 75.0);
    let cleared = repo
        .clear_history("u1", false, vec!["book".into()], vec![], vec![], true)
        .await
        .unwrap();
    assert_eq!(cleared, 2);
    assert!(repo.percentages("u1").await.unwrap().is_empty());
}

#[tokio::test]
async fn clear_progress_removes_manual_read_marks_even_without_history() {
    let (_, repo) = fixture(1).await;
    repo.mark_books("u1", vec!["book".into()], true)
        .await
        .unwrap();
    repo.clear_history("u1", false, vec!["book".into()], vec![], vec![], true)
        .await
        .unwrap();
    assert!(repo.percentages("u1").await.unwrap().is_empty());
}

#[tokio::test]
async fn history_pages_are_bounded_stable_and_access_filtered() {
    let (db, repo) = fixture(1500).await;
    db.transaction(|conn| {
        conn.execute(
            "INSERT INTO progress(id,user_id,book_id,chapter_id,position,duration,updated_at)
            SELECT id,'u1','book',id,50,100,'2026-09-30T00:00:00Z' FROM chapters",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let first = repo.history_chapters("u1", "book", 1, 50).await.unwrap();
    let second = repo.history_chapters("u1", "book", 2, 50).await.unwrap();
    assert_eq!(first.total, 1500);
    assert_eq!(first.items.len(), 50);
    assert_eq!(second.items.len(), 50);
    assert!(
        first
            .items
            .iter()
            .all(|item| second.items.iter().all(|other| item.id != other.id))
    );
    let books = repo
        .activity_books("u1", false, false, 1, 40)
        .await
        .unwrap();
    assert_eq!(books.items[0].chapter_count, 1500);
    assert_eq!(books.items[0].latest_chapter_id.as_deref(), Some("c00000"));
    assert_eq!(books.items[0].latest_position, 50.0);
    assert_eq!(books.items[0].latest_duration, 100.0);
    db.execute(|conn| {
        conn.execute("DELETE FROM user_library_access WHERE user_id='u1'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        repo.activity_books("u1", false, false, 1, 40)
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        repo.activity_books("u1", true, false, 1, 40)
            .await
            .unwrap()
            .total,
        1
    );
}

#[tokio::test]
async fn activity_summary_uses_latest_visible_entry_and_bookmark_note() {
    let (db, repo) = fixture(2).await;
    add_progress(&db, "u1", "c00000", 75.0).await;
    add_progress(&db, "u1", "c00001", 40.0).await;
    db.execute(|conn| {
        conn.execute_batch(
            "UPDATE progress SET updated_at='2026-09-29T00:00:00Z' WHERE chapter_id='c00000';
             UPDATE progress SET updated_at='2026-09-30T00:00:00Z' WHERE chapter_id='c00001';",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let summary = repo
        .activity_books("u1", false, false, 1, 40)
        .await
        .unwrap();
    assert_eq!(
        summary.items[0].latest_chapter_title.as_deref(),
        Some("c00001")
    );
    assert_eq!(summary.items[0].latest_position, 40.0);
    repo.clear_history("u1", false, vec![], vec![], vec!["c00001".into()], false)
        .await
        .unwrap();
    let visible = repo
        .activity_books("u1", false, false, 1, 40)
        .await
        .unwrap();
    assert_eq!(visible.items[0].chapter_count, 1);
    assert_eq!(visible.items[0].latest_position, 75.0);
    let bookmark = repo
        .add_bookmark("u1", "book".into(), "c00001".into(), 12.5, "Note".into())
        .await
        .unwrap();
    repo.change_bookmark("u1", &bookmark.id, Some("Edited note".into()))
        .await
        .unwrap();
    let bookmarks = repo.activity_books("u1", false, true, 1, 40).await.unwrap();
    assert_eq!(
        bookmarks.items[0].latest_note.as_deref(),
        Some("Edited note")
    );
    assert_eq!(bookmarks.items[0].latest_position, 12.5);
    assert_eq!(bookmarks.items[0].latest_duration, 100.0);
}

#[tokio::test]
async fn bookmark_duration_falls_back_to_owners_progress_when_metadata_is_unknown() {
    let (db, repo) = fixture(1).await;
    add_progress(&db, "u1", "c00000", 50.0).await;
    add_progress(&db, "u2", "c00000", 50.0).await;
    db.execute(|conn| {
        conn.execute_batch(
            "UPDATE chapters SET duration=0;
             UPDATE progress SET duration=200 WHERE user_id='u2';",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let bookmark = repo
        .add_bookmark("u1", "book".into(), "c00000".into(), 25.0, "".into())
        .await
        .unwrap();
    assert_eq!(bookmark.chapter_duration, 100.0);
    assert_eq!(
        repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
        100.0
    );
    db.execute(|conn| {
        conn.execute("DELETE FROM progress WHERE user_id='u1'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
        0.0
    );
}

#[tokio::test]
async fn bookmarks_enforce_ownership_and_cascade_when_chapters_are_deleted() {
    let (db, repo) = fixture(2).await;
    let bookmark = repo
        .add_bookmark("u1", "book".into(), "c00000".into(), 12.5, "备注".into())
        .await
        .unwrap();
    assert_eq!(bookmark.chapter_duration, 100.0);
    assert_eq!(
        repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].position,
        12.5
    );
    assert_eq!(
        repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
        100.0
    );
    assert!(repo.bookmark("u2", &bookmark.id).await.unwrap().is_none());
    let fetched = repo.bookmark("u1", &bookmark.id).await.unwrap().unwrap();
    assert_eq!(fetched.book_id, "book");
    assert_eq!(fetched.position, 12.5);
    assert_eq!(fetched.chapter_duration, 100.0);
    assert_eq!(
        repo.change_bookmark("u2", &bookmark.id, None)
            .await
            .unwrap(),
        0
    );
    repo.change_bookmark("u1", &bookmark.id, Some("new note".into()))
        .await
        .unwrap();
    assert_eq!(
        repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].note,
        "new note"
    );
    assert_eq!(
        repo.activity_books("u1", false, true, 1, 40)
            .await
            .unwrap()
            .total,
        1
    );
    db.execute(|conn| {
        conn.execute("DELETE FROM chapters WHERE id='c00000'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(repo.bookmarks("u1", "book", 1, 40).await.unwrap().total, 0);
    assert!(repo.bookmark("u1", &bookmark.id).await.unwrap().is_none());
}

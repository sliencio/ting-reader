use super::*;

async fn repository() -> (Arc<DatabaseManager>, SeriesRepository) {
    let db = Arc::new(DatabaseManager::new_in_memory().expect("create test database"));
    db.execute(|conn| {
        conn.execute(
            "INSERT INTO libraries (id, name, type, url)
             VALUES ('library-1', 'Test', 'local', '/media')",
            [],
        )
        .map_err(TingError::DatabaseError)?;
        for id in ["book-1", "book-2"] {
            conn.execute(
                "INSERT INTO books (id, library_id, path, hash) VALUES (?, 'library-1', ?, ?)",
                rusqlite::params![id, format!("/media/{id}"), format!("hash-{id}")],
            )
            .map_err(TingError::DatabaseError)?;
        }
        Ok(())
    })
    .await
    .expect("insert fixtures");
    let repo = SeriesRepository::new(db.clone());
    (db, repo)
}

fn series(id: &str, title: &str) -> Series {
    Series {
        id: id.into(),
        library_id: "library-1".into(),
        title: title.into(),
        author: None,
        narrator: None,
        cover_url: None,
        description: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    }
}

async fn memberships(db: &DatabaseManager, book_id: &str) -> Vec<String> {
    let book_id = book_id.to_string();
    db.execute(move |conn| {
        let mut stmt = conn
            .prepare("SELECT series_id FROM series_books WHERE book_id = ? ORDER BY series_id")
            .map_err(TingError::DatabaseError)?;
        let rows = stmt
            .query_map([&book_id], |row| row.get::<_, String>(0))
            .map_err(TingError::DatabaseError)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(TingError::DatabaseError)?;
        Ok(rows)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn titles_differing_only_in_volume_suffix_share_one_series() {
    let (_db, repo) = repository().await;
    let first = repo
        .find_or_create_by_title(series("s1", "斗破苍穹"))
        .await
        .unwrap();
    let second = repo
        .find_or_create_by_title(series("s2", "斗破苍穹 第2卷"))
        .await
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.title, "斗破苍穹");
}

#[tokio::test]
async fn re_linking_replaces_the_previous_inferred_membership() {
    let (db, repo) = repository().await;
    let old = repo
        .find_or_create_by_title(series("s1", "书名 第1卷"))
        .await
        .unwrap();
    let new = repo
        .find_or_create_by_title(series("s2", "书名 第2卷"))
        .await
        .unwrap();
    repo.link_inferred_book("library-1", &old.id, "book-1", 1)
        .await
        .unwrap();
    repo.link_inferred_book("library-1", &new.id, "book-1", 2)
        .await
        .unwrap();
    assert_eq!(memberships(&db, "book-1").await, vec![new.id.clone()]);
}

#[tokio::test]
async fn inference_never_adds_a_second_series_next_to_an_explicit_one() {
    let (db, repo) = repository().await;
    let declared = repo
        .find_or_create_by_title(series("s1", "声明系列"))
        .await
        .unwrap();
    let inferred = repo
        .find_or_create_by_title(series("s2", "目录推断系列"))
        .await
        .unwrap();
    repo.add_book(SeriesBook {
        series_id: declared.id.clone(),
        book_id: "book-1".into(),
        book_order: 1,
    })
    .await
    .unwrap();
    repo.link_inferred_book("library-1", &inferred.id, "book-1", 1)
        .await
        .unwrap();
    assert_eq!(memberships(&db, "book-1").await, vec![declared.id.clone()]);
}

#[tokio::test]
async fn releasing_inferred_memberships_keeps_explicit_ones() {
    let (db, repo) = repository().await;
    let declared = repo
        .find_or_create_by_title(series("s1", "声明系列"))
        .await
        .unwrap();
    let inferred = repo
        .find_or_create_by_title(series("s2", "目录推断系列"))
        .await
        .unwrap();
    repo.add_book(SeriesBook {
        series_id: inferred.id.clone(),
        book_id: "book-1".into(),
        book_order: 1,
    })
    .await
    .unwrap();
    repo.link_inferred_book("library-1", &inferred.id, "book-1", 1)
        .await
        .unwrap();
    repo.release_inferred_memberships("library-1", "book-1", vec![declared.id.clone()])
        .await
        .unwrap();
    // `book-1` was curated into `inferred` by hand, but `link_inferred_book`
    // skipped the explicit book, so the hand-made link survives the release.
    assert_eq!(memberships(&db, "book-1").await, vec![inferred.id.clone()]);
}

#[tokio::test]
async fn merging_moves_members_and_drops_duplicates() {
    let (db, repo) = repository().await;
    let target = repo
        .find_or_create_by_title(series("s1", "斗破苍穹"))
        .await
        .unwrap();
    let duplicate = repo
        .find_or_create_by_title(series("s2", "斗破苍穹 少年篇"))
        .await
        .unwrap();
    // The naming rules miss this variant, so two rows really do exist — which
    // is the situation the merge exists to resolve.
    assert_ne!(target.id, duplicate.id);

    repo.add_book(SeriesBook {
        series_id: target.id.clone(),
        book_id: "book-1".into(),
        book_order: 1,
    })
    .await
    .unwrap();
    // The book also sits in the duplicate, which is exactly the overlap the
    // merge has to resolve without violating the unique index.
    repo.add_book(SeriesBook {
        series_id: duplicate.id.clone(),
        book_id: "book-1".into(),
        book_order: 1,
    })
    .await
    .unwrap();
    repo.add_book(SeriesBook {
        series_id: duplicate.id.clone(),
        book_id: "book-2".into(),
        book_order: 2,
    })
    .await
    .unwrap();

    let moved = repo
        .merge_into(&target.id, vec![duplicate.id.clone()])
        .await
        .unwrap();

    assert_eq!(moved, 2);
    assert!(repo.find_by_id(&duplicate.id).await.unwrap().is_none());
    assert_eq!(memberships(&db, "book-1").await, vec![target.id.clone()]);
    assert_eq!(memberships(&db, "book-2").await, vec![target.id.clone()]);
    // Renumbered into one sequence rather than keeping two `1`s.
    let books = repo.find_books_by_series(&target.id).await.unwrap();
    let orders: Vec<i32> = books.iter().map(|(_, order)| *order).collect();
    assert_eq!(orders, vec![1, 2]);
}

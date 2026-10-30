use super::*;

#[tokio::test]
async fn metadata_and_reading_revisions_are_separate_and_user_scoped() {
    let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
    db.execute(|conn| {conn.execute_batch("INSERT INTO users(id,username,password_hash) VALUES('one','one','hash'),('two','two','hash');
        INSERT INTO libraries(id,name,type,url) VALUES('library','Test','local','');
        INSERT INTO books(id,library_id,path,hash) VALUES('book','library','/book','hash');
        INSERT INTO chapters(id,book_id,path) VALUES('chapter','book','/chapter');")?;Ok(())}).await.unwrap();
    let repo = SyncRepository::new(db.clone());
    let before = repo.book("one", "book").await.unwrap();
    db.execute(|conn| {
        conn.execute("UPDATE books SET title='Changed' WHERE id='book'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let changed = repo.book("one", "book").await.unwrap();
    assert!(changed.content_revision > before.content_revision);
    assert_eq!(changed.reading_revision, before.reading_revision);
    db.execute(|conn| {
        conn.execute(
            "INSERT INTO chapter_read_marks VALUES('one','book','chapter')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(repo.book("one", "book").await.unwrap().reading_revision > changed.reading_revision);
    assert_eq!(repo.book("two", "book").await.unwrap().reading_revision, 0);
    assert_eq!(
        repo.book("one", "book").await.unwrap().content_revision,
        changed.content_revision
    );
}

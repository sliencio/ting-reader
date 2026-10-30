use super::*;

fn database(path: &Path) {
    let mut conn = Connection::open(path).unwrap();
    crate::db::migrations::run_migrations(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO users(id,username,password_hash) VALUES('user','reader','hash')",
        [],
    )
    .unwrap();
}

#[test]
fn consistent_backup_includes_wal_and_restore_waits_for_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("main.db");
    database(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    conn.execute("UPDATE users SET username='before'", [])
        .unwrap();
    let saved = new_path(&path, "manual").unwrap();
    copy_database(&path, &saved).unwrap();
    validate(&saved).unwrap();
    conn.execute("UPDATE users SET username='after'", [])
        .unwrap();
    stage_restore(&path, saved.file_name().unwrap().to_str().unwrap()).unwrap();
    assert_eq!(
        conn.query_row("SELECT username FROM users", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "after"
    );
    drop(conn);
    prepare_database(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT username FROM users", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "before"
    );
    assert!(!directory(&path).join("pending-restore.db").exists());
    assert!(
        list(&path)
            .unwrap()
            .iter()
            .any(|item| item.name.starts_with("before-restore-"))
    );
}

#[test]
fn invalid_and_future_backups_cannot_be_staged() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("main.db");
    database(&path);
    assert!(resolve(&path, "../main.db").is_err());
    assert!(resolve(&path, "pending-restore.db").is_err());
    let invalid_file = new_path(&path, "invalid").unwrap();
    std::fs::write(&invalid_file, b"invalid").unwrap();
    assert!(validate(&invalid_file).is_err());
    let newer = new_path(&path, "newer").unwrap();
    copy_database(&path, &newer).unwrap();
    Connection::open(&newer)
        .unwrap()
        .execute(
            "INSERT INTO schema_migrations VALUES(999,CURRENT_TIMESTAMP)",
            [],
        )
        .unwrap();
    assert!(stage_restore(&path, newer.file_name().unwrap().to_str().unwrap()).is_err());
    assert!(!directory(&path).join("pending-restore.db").exists());
}

#[test]
fn upgrade_backup_is_created_before_schema_changes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("main.db");
    database(&path);
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM schema_migrations WHERE version=32", [])
        .unwrap();
    prepare_database(&path).unwrap();
    let files = list(&path).unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].name.starts_with("before-upgrade-"));
    let saved = Connection::open(resolve(&path, &files[0].name).unwrap()).unwrap();
    assert_eq!(
        saved
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        31
    );
}

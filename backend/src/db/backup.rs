use crate::core::app::error::{InputErrorKind, Result, TingError};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct BackupFile {
    pub name: String,
    pub bytes: u64,
    pub created_at: String,
}

pub struct TemporaryBackup(pub PathBuf);
impl Drop for TemporaryBackup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn directory(database: &Path) -> PathBuf {
    let parent = database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    parent.join(format!(
        "{}.backups",
        database.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn invalid(message: &str) -> TingError {
    TingError::invalid_request(
        InputErrorKind::InvalidValue,
        "database.backup",
        message.into(),
    )
}

pub fn resolve(database: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name.len() > 150
        || !name.ends_with(".db")
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || name == "pending-restore.db"
    {
        return Err(invalid("备份文件名无效"));
    }
    let path = directory(database).join(name);
    if std::fs::symlink_metadata(&path).is_ok_and(|info| info.file_type().is_symlink()) {
        return Err(invalid("备份不能是符号链接"));
    }
    Ok(path)
}

pub fn validate(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA trusted_schema=OFF;")?;
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(invalid("备份数据库完整性校验失败"));
    }
    for table in [
        "users",
        "books",
        "chapters",
        "progress",
        "schema_migrations",
    ] {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(invalid("文件不是完整的听悦数据库备份"));
        }
    }
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    if version > super::migrations::LATEST_SCHEMA_VERSION {
        return Err(invalid("备份来自较新的服务端，请先升级再恢复"));
    }
    Ok(())
}

pub fn copy_database(source: &Path, destination: &Path) -> Result<()> {
    let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    src.busy_timeout(Duration::from_secs(10))?;
    let _snapshot = src.unchecked_transaction()?;
    src.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })?;
    let mut dst = Connection::open(destination)?;
    dst.busy_timeout(Duration::from_secs(10))?;
    rusqlite::backup::Backup::new(&src, &mut dst)?.run_to_completion(
        256,
        Duration::from_millis(1),
        None,
    )?;
    Ok(())
}

pub fn new_path(database: &Path, kind: &str) -> Result<PathBuf> {
    let folder = directory(database);
    std::fs::create_dir_all(&folder).map_err(TingError::IoError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
            .map_err(TingError::IoError)?;
    }
    Ok(folder.join(format!(
        "{kind}-{}-{}.db",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        uuid::Uuid::new_v4().simple()
    )))
}

pub fn list(database: &Path) -> Result<Vec<BackupFile>> {
    let folder = directory(database);
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).map_err(TingError::IoError)? {
        let entry = entry.map_err(TingError::IoError)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if resolve(database, &name).is_err() {
            continue;
        }
        let info = entry.metadata().map_err(TingError::IoError)?;
        if !info.is_file() {
            continue;
        }
        files.push(BackupFile {
            name,
            bytes: info.len(),
            created_at: info
                .modified()
                .map(chrono::DateTime::<chrono::Utc>::from)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
        });
    }
    files.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(files)
}

pub fn stage_restore(database: &Path, name: &str) -> Result<()> {
    let source = resolve(database, name)?;
    validate(&source)?;
    let pending = directory(database).join("pending-restore.db");
    if pending.exists() {
        return Err(invalid("已有待恢复备份，请先取消或重启应用"));
    }
    let staging = directory(database).join(format!("restore-{}.partial", uuid::Uuid::new_v4()));
    let _guard = TemporaryBackup(staging.clone());
    copy_database(&source, &staging)?;
    validate(&staging)?;
    std::fs::rename(&staging, pending).map_err(TingError::IoError)?;
    Ok(())
}

pub fn prepare_database(database: &Path) -> Result<()> {
    let pending = directory(database).join("pending-restore.db");
    if pending.exists() {
        validate(&pending)?;
        if database.exists() {
            copy_database(database, &new_path(database, "before-restore")?)?;
        }
        // 在连接池、定时任务和监听端口创建之前恢复，避免旧连接继续写入。
        copy_database(&pending, database)?;
        validate(database)?;
        std::fs::remove_file(pending).map_err(TingError::IoError)?;
    }
    if database.exists() {
        let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let has_migrations: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='schema_migrations')",
            [],
            |row| row.get(0),
        )?;
        let version = if has_migrations {
            conn.query_row(
                "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                [],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            0
        };
        if version < super::migrations::LATEST_SCHEMA_VERSION {
            copy_database(database, &new_path(database, "before-upgrade")?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/db/backup.rs"]
mod tests;

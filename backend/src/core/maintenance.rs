use crate::core::app::error::{InputErrorKind, Result, TingError};
use crate::db::{backup, manager::DatabaseManager, repository::SystemSettingsRepository};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const SETTINGS_KEY: &str = "server_maintenance";
pub static BACKUP_LOCK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MaintenanceSettings {
    pub automatic_backup: bool,
    pub backup_interval_hours: u64,
    pub backup_retention: usize,
    pub preload_window_mib: usize,
    pub preload_memory_mib: usize,
    pub preload_idle_seconds: u64,
    pub disk_cache_chapters: usize,
    pub disk_cache_mib: Option<usize>,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        Self {
            automatic_backup: true,
            backup_interval_hours: 24,
            backup_retention: 7,
            preload_window_mib: 4,
            preload_memory_mib: 32,
            preload_idle_seconds: 60,
            disk_cache_chapters: 50,
            disk_cache_mib: None,
        }
    }
}

impl MaintenanceSettings {
    pub fn validate(&self) -> Result<()> {
        if !(1..=168).contains(&self.backup_interval_hours)
            || !(1..=60).contains(&self.backup_retention)
            || !(1..=16).contains(&self.preload_window_mib)
            || !(4..=256).contains(&self.preload_memory_mib)
            || self.preload_window_mib > self.preload_memory_mib
            || !(5..=3600).contains(&self.preload_idle_seconds)
            || !(1..=500).contains(&self.disk_cache_chapters)
            || self
                .disk_cache_mib
                .is_some_and(|value| !(64..=65536).contains(&value))
        {
            return Err(TingError::invalid_request(
                InputErrorKind::OutOfRange,
                "maintenance.settings",
                "备份或缓存参数超出允许范围".into(),
            ));
        }
        Ok(())
    }
}

pub async fn load_settings(repo: &SystemSettingsRepository) -> Result<MaintenanceSettings> {
    let value = repo.get(SETTINGS_KEY).await?;
    let settings = match value {
        Some(value) => serde_json::from_str(&value).map_err(|e| {
            TingError::deserialization(
                crate::core::app::error::CodecErrorKind::InvalidData,
                "maintenance.settings",
                e.to_string(),
            )
        })?,
        None => MaintenanceSettings::default(),
    };
    settings.validate()?;
    Ok(settings)
}

pub async fn create_backup(db: Arc<DatabaseManager>, kind: &str) -> Result<backup::BackupFile> {
    let permit = BACKUP_LOCK
        .acquire()
        .await
        .map_err(|_| TingError::DatabaseError(rusqlite::Error::InvalidQuery))?;
    let destination = backup::new_path(db.db_path(), kind)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let temporary = destination.with_extension("partial");
        let _guard = backup::TemporaryBackup(temporary.clone());
        backup::copy_database(db.db_path(), &temporary)?;
        backup::validate(&temporary)?;
        std::fs::rename(&temporary, &destination).map_err(TingError::IoError)?;
        let info = std::fs::metadata(&destination).map_err(TingError::IoError)?;
        Ok(backup::BackupFile {
            name: destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            bytes: info.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    })
    .await
    .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))?
}

#[cfg(test)]
#[path = "../../tests/unit/core/maintenance.rs"]
mod tests;

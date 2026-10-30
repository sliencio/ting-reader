use super::*;

#[test]
fn validates_bounds_and_rejects_preload_larger_than_budget() {
    assert!(MaintenanceSettings::default().validate().is_ok());
    assert!(
        MaintenanceSettings {
            preload_window_mib: 16,
            preload_memory_mib: 4,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        MaintenanceSettings {
            backup_interval_hours: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        MaintenanceSettings {
            disk_cache_mib: Some(63),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        MaintenanceSettings {
            preload_window_mib: 16,
            preload_memory_mib: 256,
            preload_idle_seconds: 3600,
            disk_cache_mib: Some(65536),
            ..Default::default()
        }
        .validate()
        .is_ok()
    );
}

#[tokio::test]
async fn stored_policy_is_reloaded_without_restart() {
    let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
    let repo = SystemSettingsRepository::new(db);
    assert_eq!(load_settings(&repo).await.unwrap().preload_window_mib, 4);
    let settings = MaintenanceSettings {
        preload_window_mib: 8,
        automatic_backup: false,
        ..Default::default()
    };
    repo.set(SETTINGS_KEY, &serde_json::to_string(&settings).unwrap())
        .await
        .unwrap();
    let loaded = load_settings(&repo).await.unwrap();
    assert_eq!(loaded.preload_window_mib, 8);
    assert!(!loaded.automatic_backup);
}

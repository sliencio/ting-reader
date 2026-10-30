use crate::core::app::error::{ExternalErrorKind, InputErrorKind};
mod diagnostics;
pub(crate) mod maintenance;
pub use maintenance::{
    book_revision, cancel_restore, create_backup, delete_backup, diagnose_library, diagnostics,
    download_backup, get_maintenance, import_backup, list_backups, restore_backup, sync_state,
    update_maintenance,
};
mod statistics;
mod tasks;
pub use diagnostics::{get_metrics, health_check};
pub use statistics::get_admin_statistics;
pub use tasks::{batch_delete_tasks, cancel_task, clear_tasks, delete_task, get_task, list_tasks};

use crate::api::models::{
    ApplicationTimeZoneResponse, ConfigResponse, DatabaseConfigResponse, LoggingConfigResponse,
    PluginSystemConfigResponse, SecurityConfigResponse, ServerConfigResponse,
    StorageConfigResponse, TaskQueueConfigResponse, UpdateApplicationTimeZoneRequest,
    UpdateConfigRequest, UpdateConfigResponse,
};
use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use axum::{Json, extract::State, response::IntoResponse};
use serde_json::Value;
use std::path::PathBuf;

mod logs;

pub use logs::{
    ClearSystemLogsResponse, LogsQuery, LogsResponse, clear_system_logs, export_system_logs,
    get_system_logs,
};

/// Return the application-wide time zone. Reading is available to every
/// authenticated user so clients can render timestamps consistently.
pub async fn get_application_time_zone(
    State(state): State<AppState>,
    _user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let time_zone = state
        .system_settings_repo
        .application_time_zone_or_default()
        .await?;

    Ok(Json(ApplicationTimeZoneResponse { time_zone }))
}

/// Update the application-wide time zone. Only administrators may change it.
pub async fn update_application_time_zone(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(request): Json<UpdateApplicationTimeZoneRequest>,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    state
        .system_settings_repo
        .set_application_time_zone(&request.time_zone)
        .await?;
    let time_zone = state
        .system_settings_repo
        .application_time_zone_or_default()
        .await?;
    Ok(Json(ApplicationTimeZoneResponse { time_zone }))
}

/// Handler for GET /api/v1/system/check-update - Check for updates via backend proxy
pub async fn check_update(State(_state): State<AppState>) -> Result<impl IntoResponse> {
    let client = reqwest::Client::new();
    let response = build_update_check_request(&client)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            let message = format!("Failed to check update: {}", e);
            TingError::external_service(
                ExternalErrorKind::Response,
                "check_update::external_service",
                message,
            )
            .with_source(anyhow::anyhow!(e))
        })?;

    let update_info: Value = response.json().await.map_err(|e| {
        let message = format!("Failed to parse update info: {}", e);
        TingError::external_service(
            ExternalErrorKind::Response,
            "check_update::external_service",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })?;

    Ok((
        [
            (
                "Cache-Control",
                "no-store, no-cache, max-age=0, must-revalidate",
            ),
            ("Pragma", "no-cache"),
            ("Expires", "0"),
        ],
        Json(update_info),
    ))
}

fn build_update_check_request(client: &reqwest::Client) -> reqwest::RequestBuilder {
    client
        .get("https://www.tingreader.cn/api/fpk/docker")
        .query(&[("fresh", "1")])
        .header("Cache-Control", "no-cache")
        .header("Pragma", "no-cache")
}

/// Handler for GET /api/v1/config - Get system configuration
pub async fn get_config(State(state): State<AppState>) -> Result<impl IntoResponse> {
    let config = state.config.read().await;

    let api_key_masked = if config.security.api_key.is_empty() {
        String::new()
    } else {
        "***".to_string()
    };

    let response = ConfigResponse {
        server: ServerConfigResponse {
            host: config.server.host.clone(),
            port: config.server.port,
            max_connections: config.server.max_connections,
            request_timeout: config.server.request_timeout,
        },
        database: DatabaseConfigResponse {
            path: config.database.path.display().to_string(),
            connection_pool_size: config.database.connection_pool_size,
            busy_timeout: config.database.busy_timeout,
        },
        plugins: PluginSystemConfigResponse {
            plugin_dir: config.plugins.plugin_dir.display().to_string(),
            enable_hot_reload: config.plugins.enable_hot_reload,
            max_memory_per_plugin: config.plugins.max_memory_per_plugin,
            max_execution_time: config.plugins.max_execution_time,
        },
        task_queue: TaskQueueConfigResponse {
            max_concurrent_tasks: config.task_queue.max_concurrent_tasks,
            default_retry_count: config.task_queue.default_retry_count,
            task_timeout: config.task_queue.task_timeout,
        },
        logging: LoggingConfigResponse {
            level: config.logging.level.clone(),
            format: config.logging.format.clone(),
            output: config.logging.output.clone(),
            log_file: config
                .logging
                .log_file
                .as_ref()
                .map(|p| p.display().to_string()),
            max_file_size: config.logging.max_file_size,
            max_backups: config.logging.max_backups,
        },
        security: SecurityConfigResponse {
            enable_auth: config.security.enable_auth,
            api_key: Some(api_key_masked),
            allowed_origins: config.security.allowed_origins.clone(),
            rate_limit_requests: config.security.rate_limit_requests,
            rate_limit_window: config.security.rate_limit_window,
            enable_hsts: config.security.enable_hsts,
            hsts_max_age: config.security.hsts_max_age,
        },
        storage: StorageConfigResponse {
            data_dir: config.storage.data_dir.display().to_string(),
            temp_dir: config.storage.temp_dir.display().to_string(),
            local_storage_root: config.storage.local_storage_root.display().to_string(),
            local_library_roots: config
                .storage
                .local_library_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            max_disk_usage: config.storage.max_disk_usage,
        },
    };

    Ok(Json(response))
}

/// Handler for PUT /api/v1/config - Update system configuration
pub async fn update_config(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpdateConfigRequest>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;

    let original_config = state.config.read().await.clone();
    let mut new_config = original_config.clone();

    let mut updated_fields = Vec::new();
    let mut requires_restart = Vec::new();

    if let Some(server_update) = req.server {
        if let Some(host) = server_update.host {
            new_config.server.host = host;
            updated_fields.push("server.host".to_string());
            requires_restart.push("server.host".to_string());
        }
        if let Some(port) = server_update.port {
            new_config.server.port = port;
            updated_fields.push("server.port".to_string());
            requires_restart.push("server.port".to_string());
        }
        if let Some(max_connections) = server_update.max_connections {
            new_config.server.max_connections = max_connections;
            updated_fields.push("server.max_connections".to_string());
            requires_restart.push("server.max_connections".to_string());
        }
        if let Some(request_timeout) = server_update.request_timeout {
            new_config.server.request_timeout = request_timeout;
            updated_fields.push("server.request_timeout".to_string());
            requires_restart.push("server.request_timeout".to_string());
        }
    }

    if let Some(database_update) = req.database {
        if let Some(path) = database_update.path {
            new_config.database.path = PathBuf::from(path);
            updated_fields.push("database.path".to_string());
            requires_restart.push("database.path".to_string());
        }
        if let Some(connection_pool_size) = database_update.connection_pool_size {
            new_config.database.connection_pool_size = connection_pool_size;
            updated_fields.push("database.connection_pool_size".to_string());
            requires_restart.push("database.connection_pool_size".to_string());
        }
        if let Some(busy_timeout) = database_update.busy_timeout {
            new_config.database.busy_timeout = busy_timeout;
            updated_fields.push("database.busy_timeout".to_string());
            requires_restart.push("database.busy_timeout".to_string());
        }
    }

    if let Some(plugins_update) = req.plugins {
        if let Some(plugin_dir) = plugins_update.plugin_dir {
            new_config.plugins.plugin_dir = PathBuf::from(plugin_dir);
            updated_fields.push("plugins.plugin_dir".to_string());
            requires_restart.push("plugins.plugin_dir".to_string());
        }
        if let Some(enable_hot_reload) = plugins_update.enable_hot_reload {
            new_config.plugins.enable_hot_reload = enable_hot_reload;
            updated_fields.push("plugins.enable_hot_reload".to_string());
        }
        if let Some(max_memory_per_plugin) = plugins_update.max_memory_per_plugin {
            new_config.plugins.max_memory_per_plugin = max_memory_per_plugin;
            updated_fields.push("plugins.max_memory_per_plugin".to_string());
        }
        if let Some(max_execution_time) = plugins_update.max_execution_time {
            new_config.plugins.max_execution_time = max_execution_time;
            updated_fields.push("plugins.max_execution_time".to_string());
        }
    }

    if let Some(task_queue_update) = req.task_queue {
        if let Some(max_concurrent_tasks) = task_queue_update.max_concurrent_tasks {
            new_config.task_queue.max_concurrent_tasks = max_concurrent_tasks;
            updated_fields.push("task_queue.max_concurrent_tasks".to_string());
        }
        if let Some(default_retry_count) = task_queue_update.default_retry_count {
            new_config.task_queue.default_retry_count = default_retry_count;
            updated_fields.push("task_queue.default_retry_count".to_string());
        }
        if let Some(task_timeout) = task_queue_update.task_timeout {
            new_config.task_queue.task_timeout = task_timeout;
            updated_fields.push("task_queue.task_timeout".to_string());
        }
    }

    if let Some(logging_update) = req.logging {
        if let Some(level) = logging_update.level {
            new_config.logging.level = level;
            updated_fields.push("logging.level".to_string());
        }
        if let Some(format) = logging_update.format {
            new_config.logging.format = format;
            updated_fields.push("logging.format".to_string());
            requires_restart.push("logging.format".to_string());
        }
        if let Some(output) = logging_update.output {
            new_config.logging.output = output;
            updated_fields.push("logging.output".to_string());
            requires_restart.push("logging.output".to_string());
        }
        if let Some(log_file) = logging_update.log_file {
            new_config.logging.log_file = Some(PathBuf::from(log_file));
            updated_fields.push("logging.log_file".to_string());
            requires_restart.push("logging.log_file".to_string());
        }
        if let Some(max_file_size) = logging_update.max_file_size {
            new_config.logging.max_file_size = max_file_size;
            updated_fields.push("logging.max_file_size".to_string());
            requires_restart.push("logging.max_file_size".to_string());
        }
        if let Some(max_backups) = logging_update.max_backups {
            new_config.logging.max_backups = max_backups;
            updated_fields.push("logging.max_backups".to_string());
            requires_restart.push("logging.max_backups".to_string());
        }
    }

    if let Some(security_update) = req.security {
        if let Some(enable_auth) = security_update.enable_auth {
            new_config.security.enable_auth = enable_auth;
            updated_fields.push("security.enable_auth".to_string());
            requires_restart.push("security.enable_auth".to_string());
        }
        if let Some(api_key) = security_update.api_key {
            new_config.security.api_key = api_key;
            updated_fields.push("security.api_key".to_string());
            requires_restart.push("security.api_key".to_string());
        }
        if let Some(allowed_origins) = security_update.allowed_origins {
            new_config.security.allowed_origins = allowed_origins;
            updated_fields.push("security.allowed_origins".to_string());
            requires_restart.push("security.allowed_origins".to_string());
        }
        if let Some(rate_limit_requests) = security_update.rate_limit_requests {
            new_config.security.rate_limit_requests = rate_limit_requests;
            updated_fields.push("security.rate_limit_requests".to_string());
        }
        if let Some(rate_limit_window) = security_update.rate_limit_window {
            new_config.security.rate_limit_window = rate_limit_window;
            updated_fields.push("security.rate_limit_window".to_string());
        }
        if let Some(enable_hsts) = security_update.enable_hsts {
            new_config.security.enable_hsts = enable_hsts;
            updated_fields.push("security.enable_hsts".to_string());
            requires_restart.push("security.enable_hsts".to_string());
        }
        if let Some(hsts_max_age) = security_update.hsts_max_age {
            new_config.security.hsts_max_age = hsts_max_age;
            updated_fields.push("security.hsts_max_age".to_string());
            requires_restart.push("security.hsts_max_age".to_string());
        }
    }

    if let Some(storage_update) = req.storage {
        if let Some(data_dir) = storage_update.data_dir {
            new_config.storage.data_dir = PathBuf::from(data_dir);
            updated_fields.push("storage.data_dir".to_string());
            requires_restart.push("storage.data_dir".to_string());
        }
        if let Some(temp_dir) = storage_update.temp_dir {
            new_config.storage.temp_dir = PathBuf::from(temp_dir);
            updated_fields.push("storage.temp_dir".to_string());
            requires_restart.push("storage.temp_dir".to_string());
        }
        if let Some(local_library_roots) = storage_update.local_library_roots {
            new_config.storage.local_library_roots =
                local_library_roots.into_iter().map(PathBuf::from).collect();
            updated_fields.push("storage.local_library_roots".to_string());
            requires_restart.push("storage.local_library_roots".to_string());
        }
        if let Some(max_disk_usage) = storage_update.max_disk_usage {
            new_config.storage.max_disk_usage = max_disk_usage;
            updated_fields.push("storage.max_disk_usage".to_string());
            requires_restart.push("storage.max_disk_usage".to_string());
        }
    }

    new_config.validate().map_err(|e| {
        tracing::error!(
            error = %e,
            message_key = "config.validation_failed",
            message_params = %serde_json::json!({ "error": e.to_string() }),
            "Configuration validation failed"
        );
        {
            let message = format!("Invalid configuration: {}", e);
            TingError::invalid_request(
                InputErrorKind::InvalidValue,
                "update_config::invalid_request",
                message,
            )
            .with_source(anyhow::anyhow!(e))
        }
    })?;

    let mut config = state.config.write().await;
    *config = new_config;
    drop(config);

    let message = if requires_restart.is_empty() {
        "Configuration updated successfully. Changes are now in effect.".to_string()
    } else {
        format!(
            "Configuration updated successfully. {} parameter(s) require system restart to take effect.",
            requires_restart.len()
        )
    };

    Ok(Json(UpdateConfigResponse {
        message,
        updated_fields,
        requires_restart,
    }))
}

#[cfg(test)]
#[path = "../../../../tests/unit/api/handlers/system.rs"]
mod tests;

//! Component health and runtime metrics in JSON/Prometheus formats.

use crate::api::models::{
    ComponentHealth, ComponentStatus, ComponentsHealth, DatabaseMetrics, HealthResponse,
    HealthStatus, MetricsResponse, PluginMetrics, SystemMetrics, TaskQueueMetrics,
};
use crate::api::state::AppState;
use crate::core::app::error::Result;
use axum::{Json, extract::State, response::IntoResponse};
use chrono::Utc;

/// Health check endpoint
pub async fn health_check(State(state): State<AppState>) -> Result<impl IntoResponse> {
    let db_health = check_database_health(&state).await;
    let plugin_health = check_plugin_system_health(&state).await;

    let overall_status = if db_health.status == ComponentStatus::Healthy
        && plugin_health.status == ComponentStatus::Healthy
    {
        HealthStatus::Healthy
    } else {
        HealthStatus::Unhealthy
    };

    let response = HealthResponse {
        status: overall_status,
        components: ComponentsHealth {
            database: db_health,
            plugin_system: plugin_health,
        },
        timestamp: Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    Ok(Json(response))
}

async fn check_database_health(state: &AppState) -> ComponentHealth {
    match state
        .book_repo
        .db()
        .execute(|conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
            Ok(())
        })
        .await
    {
        Ok(_) => ComponentHealth {
            status: ComponentStatus::Healthy,
            message: Some("Database is operational".to_string()),
            details: Some(serde_json::json!({
                "status": "connected",
            })),
        },
        Err(e) => ComponentHealth {
            status: ComponentStatus::Unhealthy,
            message: Some(format!("Database error: {}", e)),
            details: None,
        },
    }
}

async fn check_plugin_system_health(state: &AppState) -> ComponentHealth {
    use crate::plugin::types::PluginState;

    let plugins = state.plugin_manager.list_plugins().await;
    let total_plugins = plugins.len();

    let active_plugins = plugins
        .iter()
        .filter(|p| matches!(p.state, PluginState::Active))
        .count();

    let failed_plugins = plugins
        .iter()
        .filter(|p| matches!(p.state, PluginState::Failed))
        .count();

    let status = if failed_plugins == 0 {
        ComponentStatus::Healthy
    } else {
        ComponentStatus::Unhealthy
    };

    let message = if failed_plugins > 0 {
        Some(format!("{} plugin(s) in failed state", failed_plugins))
    } else {
        Some("Plugin system is operational".to_string())
    };

    ComponentHealth {
        status,
        message,
        details: Some(serde_json::json!({
            "total_plugins": total_plugins,
            "active_plugins": active_plugins,
            "failed_plugins": failed_plugins,
        })),
    }
}

/// Metrics endpoint
pub async fn get_metrics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse> {
    let accept_header = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    let is_prometheus = accept_header.contains("text/plain")
        || accept_header.contains("application/openmetrics-text");

    let system_metrics = collect_system_metrics(&state);
    let plugin_metrics = collect_plugin_metrics(&state).await;
    let task_queue_metrics = collect_task_queue_metrics(&state).await;
    let database_metrics = collect_database_metrics(&state);

    let timestamp = Utc::now().to_rfc3339();

    if is_prometheus {
        let prometheus_output = format_prometheus_metrics(
            &system_metrics,
            &plugin_metrics,
            &task_queue_metrics,
            &database_metrics,
        );

        Ok((
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            prometheus_output,
        )
            .into_response())
    } else {
        let response = MetricsResponse {
            system: system_metrics,
            plugins: plugin_metrics,
            task_queue: task_queue_metrics,
            database: database_metrics,
            timestamp,
        };

        Ok(Json(response).into_response())
    }
}

fn collect_system_metrics(_state: &AppState) -> SystemMetrics {
    SystemMetrics {
        total_requests: 0,
        avg_response_time_ms: 0.0,
        total_errors: 0,
        error_rate: 0.0,
        uptime_seconds: 0,
    }
}

async fn collect_plugin_metrics(state: &AppState) -> Vec<PluginMetrics> {
    let plugins = state.plugin_manager.list_plugins().await;

    plugins
        .into_iter()
        .map(|info| {
            let success_rate = if info.total_calls > 0 {
                info.successful_calls as f64 / info.total_calls as f64
            } else {
                0.0
            };

            PluginMetrics {
                plugin_id: info.id.clone(),
                plugin_name: info.name.clone(),
                total_calls: info.total_calls,
                successful_calls: info.successful_calls,
                failed_calls: info.failed_calls,
                success_rate,
                min_execution_time_ms: None,
                max_execution_time_ms: None,
                avg_execution_time_ms: None,
                p95_execution_time_ms: None,
                memory_usage_bytes: None,
                peak_memory_bytes: None,
                error_distribution: std::collections::HashMap::new(),
            }
        })
        .collect()
}

async fn collect_task_queue_metrics(state: &AppState) -> TaskQueueMetrics {
    let all_tasks = state.task_queue.list_tasks().await.unwrap_or_default();

    let queued_tasks = all_tasks.iter().filter(|t| t.status == "queued").count();
    let running_tasks = all_tasks.iter().filter(|t| t.status == "running").count();
    let completed_tasks = all_tasks.iter().filter(|t| t.status == "completed").count();
    let failed_tasks = all_tasks.iter().filter(|t| t.status == "failed").count();
    let cancelled_tasks = all_tasks.iter().filter(|t| t.status == "cancelled").count();
    let total_tasks = all_tasks.len();

    let failure_rate = if total_tasks > 0 {
        failed_tasks as f64 / total_tasks as f64
    } else {
        0.0
    };

    TaskQueueMetrics {
        queued_tasks,
        running_tasks,
        completed_tasks,
        failed_tasks,
        cancelled_tasks,
        total_tasks,
        avg_processing_time_ms: 0.0,
        failure_rate,
    }
}

fn collect_database_metrics(_state: &AppState) -> DatabaseMetrics {
    DatabaseMetrics {
        active_connections: 0,
        idle_connections: 0,
        total_queries: 0,
        avg_query_time_ms: 0.0,
    }
}

fn format_prometheus_metrics(
    system: &SystemMetrics,
    plugins: &[PluginMetrics],
    task_queue: &TaskQueueMetrics,
    database: &DatabaseMetrics,
) -> String {
    let mut output = String::new();

    // System metrics
    output.push_str("# HELP ting_reader_requests_total Total number of HTTP requests\n");
    output.push_str("# TYPE ting_reader_requests_total counter\n");
    output.push_str(&format!(
        "ting_reader_requests_total {}\n",
        system.total_requests
    ));
    output.push('\n');

    output.push_str("# HELP ting_reader_response_time_ms Average response time in milliseconds\n");
    output.push_str("# TYPE ting_reader_response_time_ms gauge\n");
    output.push_str(&format!(
        "ting_reader_response_time_ms {}\n",
        system.avg_response_time_ms
    ));
    output.push('\n');

    output.push_str("# HELP ting_reader_errors_total Total number of errors\n");
    output.push_str("# TYPE ting_reader_errors_total counter\n");
    output.push_str(&format!(
        "ting_reader_errors_total {}\n",
        system.total_errors
    ));
    output.push('\n');

    output.push_str("# HELP ting_reader_error_rate Error rate (errors / total requests)\n");
    output.push_str("# TYPE ting_reader_error_rate gauge\n");
    output.push_str(&format!("ting_reader_error_rate {}\n", system.error_rate));
    output.push('\n');

    output.push_str("# HELP ting_reader_uptime_seconds System uptime in seconds\n");
    output.push_str("# TYPE ting_reader_uptime_seconds counter\n");
    output.push_str(&format!(
        "ting_reader_uptime_seconds {}\n",
        system.uptime_seconds
    ));
    output.push('\n');

    // Plugin metrics
    output.push_str("# HELP ting_reader_plugin_calls_total Total number of plugin calls\n");
    output.push_str("# TYPE ting_reader_plugin_calls_total counter\n");
    for plugin in plugins {
        output.push_str(&format!(
            "ting_reader_plugin_calls_total{{plugin_id=\"{}\",plugin_name=\"{}\"}} {}\n",
            plugin.plugin_id, plugin.plugin_name, plugin.total_calls
        ));
    }
    output.push('\n');

    output.push_str("# HELP ting_reader_plugin_success_rate Plugin success rate\n");
    output.push_str("# TYPE ting_reader_plugin_success_rate gauge\n");
    for plugin in plugins {
        output.push_str(&format!(
            "ting_reader_plugin_success_rate{{plugin_id=\"{}\",plugin_name=\"{}\"}} {}\n",
            plugin.plugin_id, plugin.plugin_name, plugin.success_rate
        ));
    }
    output.push('\n');

    output.push_str(
        "# HELP ting_reader_plugin_execution_time_ms Plugin execution time in milliseconds\n",
    );
    output.push_str("# TYPE ting_reader_plugin_execution_time_ms summary\n");
    for plugin in plugins {
        if let Some(min) = plugin.min_execution_time_ms {
            output.push_str(&format!(
                "ting_reader_plugin_execution_time_ms{{plugin_id=\"{}\",plugin_name=\"{}\",quantile=\"0.0\"}} {}\n",
                plugin.plugin_id, plugin.plugin_name, min
            ));
        }
        if let Some(avg) = plugin.avg_execution_time_ms {
            output.push_str(&format!(
                "ting_reader_plugin_execution_time_ms{{plugin_id=\"{}\",plugin_name=\"{}\",quantile=\"0.5\"}} {}\n",
                plugin.plugin_id, plugin.plugin_name, avg
            ));
        }
        if let Some(p95) = plugin.p95_execution_time_ms {
            output.push_str(&format!(
                "ting_reader_plugin_execution_time_ms{{plugin_id=\"{}\",plugin_name=\"{}\",quantile=\"0.95\"}} {}\n",
                plugin.plugin_id, plugin.plugin_name, p95
            ));
        }
        if let Some(max) = plugin.max_execution_time_ms {
            output.push_str(&format!(
                "ting_reader_plugin_execution_time_ms{{plugin_id=\"{}\",plugin_name=\"{}\",quantile=\"1.0\"}} {}\n",
                plugin.plugin_id, plugin.plugin_name, max
            ));
        }
    }
    output.push('\n');

    output.push_str("# HELP ting_reader_plugin_memory_bytes Plugin memory usage in bytes\n");
    output.push_str("# TYPE ting_reader_plugin_memory_bytes gauge\n");
    for plugin in plugins {
        if let Some(memory) = plugin.memory_usage_bytes {
            output.push_str(&format!(
                "ting_reader_plugin_memory_bytes{{plugin_id=\"{}\",plugin_name=\"{}\"}} {}\n",
                plugin.plugin_id, plugin.plugin_name, memory
            ));
        }
    }
    output.push('\n');

    // Task queue metrics
    output.push_str("# HELP ting_reader_tasks_total Total number of tasks by status\n");
    output.push_str("# TYPE ting_reader_tasks_total gauge\n");
    output.push_str(&format!(
        "ting_reader_tasks_total{{status=\"queued\"}} {}\n",
        task_queue.queued_tasks
    ));
    output.push_str(&format!(
        "ting_reader_tasks_total{{status=\"running\"}} {}\n",
        task_queue.running_tasks
    ));
    output.push_str(&format!(
        "ting_reader_tasks_total{{status=\"completed\"}} {}\n",
        task_queue.completed_tasks
    ));
    output.push_str(&format!(
        "ting_reader_tasks_total{{status=\"failed\"}} {}\n",
        task_queue.failed_tasks
    ));
    output.push_str(&format!(
        "ting_reader_tasks_total{{status=\"cancelled\"}} {}\n",
        task_queue.cancelled_tasks
    ));
    output.push('\n');

    output.push_str("# HELP ting_reader_task_failure_rate Task failure rate\n");
    output.push_str("# TYPE ting_reader_task_failure_rate gauge\n");
    output.push_str(&format!(
        "ting_reader_task_failure_rate {}\n",
        task_queue.failure_rate
    ));
    output.push('\n');

    output.push_str(
        "# HELP ting_reader_task_processing_time_ms Average task processing time in milliseconds\n",
    );
    output.push_str("# TYPE ting_reader_task_processing_time_ms gauge\n");
    output.push_str(&format!(
        "ting_reader_task_processing_time_ms {}\n",
        task_queue.avg_processing_time_ms
    ));
    output.push('\n');

    // Database metrics
    output.push_str("# HELP ting_reader_db_connections Database connections\n");
    output.push_str("# TYPE ting_reader_db_connections gauge\n");
    output.push_str(&format!(
        "ting_reader_db_connections{{state=\"active\"}} {}\n",
        database.active_connections
    ));
    output.push_str(&format!(
        "ting_reader_db_connections{{state=\"idle\"}} {}\n",
        database.idle_connections
    ));
    output.push('\n');

    output.push_str("# HELP ting_reader_db_queries_total Total number of database queries\n");
    output.push_str("# TYPE ting_reader_db_queries_total counter\n");
    output.push_str(&format!(
        "ting_reader_db_queries_total {}\n",
        database.total_queries
    ));
    output.push('\n');

    output.push_str(
        "# HELP ting_reader_db_query_time_ms Average database query time in milliseconds\n",
    );
    output.push_str("# TYPE ting_reader_db_query_time_ms gauge\n");
    output.push_str(&format!(
        "ting_reader_db_query_time_ms {}\n",
        database.avg_query_time_ms
    ));
    output.push('\n');

    output
}

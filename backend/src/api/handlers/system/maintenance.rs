use crate::api::{require_admin, state::AppState};
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{InputErrorKind, LookupErrorKind, Result, TingError};
use crate::core::maintenance::{self, MaintenanceSettings};
use crate::db::{
    backup,
    repository::{Repository, sync::SyncRepository},
};
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static BACKUP_ERROR: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

fn invalid(message: &str) -> TingError {
    TingError::invalid_request(
        InputErrorKind::InvalidValue,
        "system.maintenance",
        message.into(),
    )
}

pub async fn get_maintenance(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    Ok(Json(
        maintenance::load_settings(&state.system_settings_repo).await?,
    ))
}

pub async fn update_maintenance(
    State(state): State<AppState>,
    user: AuthUser,
    Json(settings): Json<MaintenanceSettings>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    settings.validate()?;
    let previous = maintenance::load_settings(&state.system_settings_repo).await?;
    state
        .system_settings_repo
        .set(maintenance::SETTINGS_KEY, &json!(settings).to_string())
        .await?;
    let limit = settings
        .disk_cache_mib
        .map(|value| value as u64 * 1024 * 1024)
        .unwrap_or(state.config.read().await.storage.max_disk_usage);
    if previous.preload_window_mib != settings.preload_window_mib
        || previous.preload_memory_mib != settings.preload_memory_mib
        || previous.preload_idle_seconds != settings.preload_idle_seconds
    {
        for (_, (_, task)) in state.active_preload_tasks.lock().await.drain() {
            task.abort();
        }
        state.preload_cache.write().await.clear();
    }
    state
        .cache_manager
        .enforce_limits(settings.disk_cache_chapters, limit)
        .await
        .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))?;
    Ok(Json(settings))
}

pub async fn list_backups(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let path = state.book_repo.db().db_path().to_path_buf();
    let (files, pending) = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok((
            backup::list(&path)?,
            backup::directory(&path).join("pending-restore.db").exists(),
        ))
    })
    .await
    .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))??;
    Ok(Json(
        json!({"files":files,"pending_restore":pending,"last_error":BACKUP_ERROR.lock().unwrap().clone()}),
    ))
}

pub async fn create_backup(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    Ok((
        StatusCode::CREATED,
        Json(maintenance::create_backup(state.book_repo.db().clone(), "manual").await?),
    ))
}

pub async fn download_backup(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
) -> Result<Response> {
    require_admin(&user)?;
    let path = backup::resolve(state.book_repo.db().db_path(), &name)?;
    let file = tokio::fs::File::open(path).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.sqlite3".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

pub async fn delete_backup(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let _permit = maintenance::BACKUP_LOCK
        .acquire()
        .await
        .map_err(|_| invalid("备份服务不可用"))?;
    tokio::fs::remove_file(backup::resolve(state.book_repo.db().db_path(), &name)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    pub confirmation: String,
}

pub async fn restore_backup(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    Json(request): Json<RestoreRequest>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    if request.confirmation != "恢复备份" {
        return Err(invalid("请输入“恢复备份”确认此操作"));
    }
    let permit = maintenance::BACKUP_LOCK
        .acquire()
        .await
        .map_err(|_| invalid("备份服务不可用"))?;
    let database = state.book_repo.db().db_path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        backup::stage_restore(&database, &name)
    })
    .await
    .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))??;
    Ok(Json(
        json!({"pending_restore":true,"requires_restart":true,"message":"备份已校验并暂存，请重启服务端应用恢复；重启前可取消。"}),
    ))
}

pub async fn cancel_restore(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let _permit = maintenance::BACKUP_LOCK
        .acquire()
        .await
        .map_err(|_| invalid("备份服务不可用"))?;
    let pending = backup::directory(state.book_repo.db().db_path()).join("pending-restore.db");
    match tokio::fs::remove_file(pending).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn import_backup(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let _permit = maintenance::BACKUP_LOCK
        .acquire()
        .await
        .map_err(|_| invalid("备份服务不可用"))?;
    let path = backup::new_path(state.book_repo.db().db_path(), "imported")?;
    let temporary = path.with_extension("partial");
    let _temporary_guard = backup::TemporaryBackup(temporary.clone());
    let result = async {
        let mut field = multipart
            .next_field()
            .await
            .map_err(|_| invalid("备份上传数据无效"))?
            .ok_or_else(|| invalid("请选择数据库备份文件"))?;
        let mut file = tokio::fs::File::create(&temporary).await?;
        let mut size = 0usize;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| invalid("读取备份文件失败"))?
        {
            size += chunk.len();
            if size > 512 * 1024 * 1024 {
                return Err(invalid("备份文件不能超过 512 MiB"));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        let checked = temporary.clone();
        tokio::task::spawn_blocking(move || backup::validate(&checked))
            .await
            .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))??;
        tokio::fs::rename(&temporary, &path).await?;
        Ok(Json(
            json!({"name":path.file_name().unwrap_or_default().to_string_lossy(),"bytes":size}),
        ))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(temporary).await;
    }
    result
}

pub async fn sync_state(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    Ok(Json(
        SyncRepository::new(state.book_repo.db().clone())
            .state(&user.id)
            .await?,
    ))
}

pub async fn book_revision(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
) -> Result<impl IntoResponse> {
    if state.book_repo.find_by_id(&book).await?.is_none() {
        return Err(TingError::not_found(
            LookupErrorKind::Book,
            "book.revision",
            "书籍不存在".into(),
        ));
    }
    if !state
        .book_repo
        .check_access(&book, &user.id, user.role == "admin")
        .await?
    {
        return Err(TingError::permission(
            crate::core::app::error::AccessErrorKind::ResourceDenied,
            "book.revision",
            "无权访问这本书".into(),
        ));
    }
    Ok(Json(
        SyncRepository::new(state.book_repo.db().clone())
            .book(&user.id, &book)
            .await?,
    ))
}

pub async fn diagnostics(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let started = Instant::now();
    state
        .book_repo
        .db()
        .execute(|conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
            Ok(())
        })
        .await?;
    let db_ms = started.elapsed().as_millis();
    let config = state.config.read().await.clone();
    let mut directories = Vec::new();
    for (name, path) in [
        ("数据目录", config.storage.data_dir),
        ("临时目录", config.storage.temp_dir),
        (
            "备份目录",
            backup::directory(state.book_repo.db().db_path()),
        ),
    ] {
        let probe = path.join(format!(".write-check-{}", uuid::Uuid::new_v4()));
        let check = async {
            tokio::fs::create_dir_all(&path).await?;
            let file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&probe)
                .await?;
            drop(file);
            tokio::fs::remove_file(&probe).await
        }
        .await;
        #[cfg(unix)]
        let free = match tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new("df")
                .args(["-Pk"])
                .arg(&path)
                .env("LC_ALL", "C")
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .nth(1)
                .and_then(|line| line.split_whitespace().nth(3))
                .and_then(|n| n.parse::<u64>().ok())
                .map(|n| n.saturating_mul(1024)),
            _ => None,
        };
        #[cfg(not(unix))]
        let free: Option<u64> = None;
        directories.push(json!({"name":name,"writable":check.is_ok(),"free_bytes":free,"error":check.err().map(|e|e.to_string())}));
    }
    let mut tools = Vec::new();
    for name in ["ffmpeg", "ffprobe"] {
        let command = if name == "ffmpeg" {
            crate::core::audio::AudioService::ffmpeg_command()
        } else {
            crate::core::audio::AudioService::ffprobe_command()
        };
        let check = match command {
            Ok(mut cmd) => {
                cmd.arg("-version").kill_on_drop(true);
                match tokio::time::timeout(Duration::from_secs(5), cmd.output()).await {
                    Ok(Ok(output)) if output.status.success() => {
                        Ok(String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string())
                    }
                    _ => Err("工具无法运行或检查超时".to_string()),
                }
            }
            Err(error) => Err(error.to_string()),
        };
        tools.push(json!({"name":name,"available":check.is_ok(),"version":check.as_ref().ok(),"error":check.as_ref().err()}));
    }
    let cache = state
        .cache_manager
        .list_cached()
        .await
        .map_err(|e| TingError::IoError(std::io::Error::other(e.to_string())))?;
    let memory = crate::api::handlers::media::stream::preload::cache_stats(&state).await;
    Ok(Json(
        json!({"database_ms":db_ms,"directories":directories,"tools":tools,
        "cache":{"files":cache.len(),"disk_bytes":cache.iter().map(|item|item.file_size).sum::<u64>(),"memory":memory},
        "last_backup_error":BACKUP_ERROR.lock().unwrap().clone()}),
    ))
}

pub async fn diagnose_library(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;
    let library = state.library_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Library,
            "library.diagnose",
            "存储库不存在".into(),
        )
    })?;
    let started = Instant::now();
    let check = async {
        if library.library_type == "webdav" {
            let password = library
                .password
                .as_deref()
                .map(|value| {
                    crate::core::security::crypto::decrypt(value, state.encryption_key.as_ref())
                })
                .transpose()?;
            let client = crate::core::storage::WebDavClient::new(1, 0)?;
            client
                .propfind(
                    &library.url,
                    "0",
                    library.username.as_deref(),
                    password.as_deref(),
                )
                .await?;
        } else if library.library_type == "local" {
            let root = if library.root_path.is_empty() {
                &library.url
            } else {
                &library.root_path
            };
            let _directory = tokio::fs::read_dir(root).await?;
        } else {
            return Ok(json!({"status":"unsupported","message":"请通过具体章节检查此类来源"}));
        }
        let chapter_id = state.book_repo.db().execute(move |conn| {
            use rusqlite::OptionalExtension;
            Ok(conn.query_row("SELECT c.id FROM chapters c JOIN books b ON b.id=c.book_id WHERE b.library_id=? LIMIT 1",[id],|row|row.get::<_,String>(0)).optional()?)
        }).await?;
        let mut read_bytes = 0usize;
        if let Some(chapter_id) = chapter_id
            && let Some(chapter) = state.chapter_repo.find_by_id(&chapter_id).await?
        {
            let mut reader: Box<dyn tokio::io::AsyncRead + Send + Unpin> =
                if library.library_type == "local" {
                    Box::new(
                        state
                            .storage_service
                            .get_local_reader(std::path::Path::new(&chapter.path), Some((0, 65535)))
                            .await?
                            .0,
                    )
                } else {
                    state
                        .storage_service
                        .get_webdav_reader(
                            &library,
                            &chapter.path,
                            Some((0, 65535)),
                            state.encryption_key.as_ref(),
                        )
                        .await?
                        .0
                };
            let mut buffer = vec![0u8; 65536];
            read_bytes = reader.read(&mut buffer).await?;
            if read_bytes == 0 {
                return Err(invalid("抽样章节为空或无法读取"));
            }
        }
        Ok::<_, TingError>(
            json!({"status":"ok","sample_bytes":read_bytes,"message":if read_bytes>0 {"目录及抽样章节可读取"} else {"目录可访问，暂无可检查的章节"}}),
        )
    };
    let result = match tokio::time::timeout(Duration::from_secs(12), check).await {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => Err("连接或读取超过 12 秒".to_string()),
    };
    Ok(Json(
        json!({"library_id":library.id,"elapsed_ms":started.elapsed().as_millis(),"result":result.as_ref().ok(),"error":result.as_ref().err()}),
    ))
}

pub(crate) fn start_services(state: &AppState) {
    let database = std::sync::Arc::downgrade(state.book_repo.db());
    let sockets = std::sync::Arc::downgrade(&state.ws_manager);
    let settings = std::sync::Arc::downgrade(&state.system_settings_repo);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut previous = HashMap::<String, String>::new();
        let mut backup_check = Instant::now();
        let mut automatic_backup: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            interval.tick().await;
            let (Some(db), Some(ws), Some(settings)) =
                (database.upgrade(), sockets.upgrade(), settings.upgrade())
            else {
                break;
            };
            let users = ws.connected_users().await;
            previous.retain(|key, _| users.contains(key));
            let repo = SyncRepository::new(db.clone());
            for user in users {
                if let Ok(version) = repo.state(&user).await {
                    let message = json!({"type":"server_changed","state":version}).to_string();
                    if previous.get(&user) != Some(&message) {
                        ws.broadcast(&user, &message).await;
                        previous.insert(user, message);
                    }
                }
            }
            if backup_check.elapsed() < Duration::from_secs(60)
                || automatic_backup
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
            {
                continue;
            }
            backup_check = Instant::now();
            automatic_backup = Some(tokio::spawn(async move {
                let result = async {
                    let policy = maintenance::load_settings(&settings).await?;
                    if !policy.automatic_backup {
                        return Ok::<_, TingError>(());
                    }
                    let files = backup::list(db.db_path())?;
                    let last = files
                        .iter()
                        .filter(|file| file.name.starts_with("automatic-"))
                        .filter_map(|file| {
                            chrono::DateTime::parse_from_rfc3339(&file.created_at).ok()
                        })
                        .max();
                    if last.is_some_and(|last| {
                        chrono::Utc::now().signed_duration_since(last).num_seconds()
                            < policy.backup_interval_hours as i64 * 3600
                    }) {
                        return Ok(());
                    }
                    maintenance::create_backup(db.clone(), "automatic").await?;
                    for file in backup::list(db.db_path())?
                        .into_iter()
                        .filter(|file| file.name.starts_with("automatic-"))
                        .skip(policy.backup_retention)
                    {
                        tokio::fs::remove_file(backup::resolve(db.db_path(), &file.name)?).await?;
                    }
                    Ok(())
                }
                .await;
                *BACKUP_ERROR.lock().unwrap() = result.err().map(|e| e.to_string());
            }));
        }
    });
}

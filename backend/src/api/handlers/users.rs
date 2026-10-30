use crate::api::models::{
    CreateUserRequest, DeleteProgressHistoryRequest, DeleteProgressHistoryResponse,
    FavoriteActionResponse, ProgressResponse, UpdateProgressRequest, UpdateUserRequest,
    UpdateUserSettingsRequest, UserActionResponse, UserInfoResponse, UserSettingsResponse,
};
use crate::api::require_admin;
use crate::api::state::AppState;
use crate::core::app::error::{AccessErrorKind, InputErrorKind, LookupErrorKind, PluginErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;

fn mask_restricted_settings(response: &mut UserSettingsResponse, is_admin: bool) {
    if is_admin {
        return;
    }

    response.auto_cache = false;
    response.widget_css = None;

    if let Some(settings_json) = response.settings_json.as_mut()
        && let Some(obj) = settings_json.as_object_mut()
    {
        obj.remove("auto_cache");
        obj.remove("widget_css");
    }
}

/// Handler for GET /api/users - Get all users (admin only)
pub async fn list_users(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    require_admin(&user)?;

    let users_list = state.user_repo.find_all().await?;
    let mut users: Vec<UserInfoResponse> = Vec::new();

    for user_model in users_list {
        let mut user_resp = UserInfoResponse::from(user_model.clone());
        user_resp.libraries_accessible = state
            .user_repo
            .get_accessible_libraries(&user_model.id)
            .await?;
        user_resp.books_accessible = state.user_repo.get_accessible_books(&user_model.id).await?;
        users.push(user_resp);
    }

    Ok(Json(users))
}

/// Handler for POST /api/users - Create new user (admin only)
pub async fn create_user(
    State(state): State<AppState>,
    admin: crate::auth::middleware::AuthUser,
    Json(req): Json<CreateUserRequest>,
) -> Result<impl IntoResponse> {
    if admin.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "create_user::permission",
            "Admin access required".to_string(),
        ));
    }

    if state
        .user_repo
        .find_by_username(&req.username)
        .await?
        .is_some()
    {
        return Err(TingError::validation(
            InputErrorKind::Conflict,
            "create_user::validation",
            format!("Username '{}' already exists", req.username),
        ));
    }

    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST).map_err(|e| {
        let message = format!("Failed to hash password: {}", e);
        TingError::plugin_execution(
            PluginErrorKind::Runtime,
            "create_user::plugin_execution",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })?;

    let new_user = crate::db::models::User {
        id: Uuid::new_v4().to_string(),
        username: req.username.clone(),
        password_hash,
        role: req.role.unwrap_or_else(|| "user".to_string()),
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    state.user_repo.create(&new_user).await?;

    state
        .user_repo
        .update_permissions(
            &new_user.id,
            req.libraries_accessible.clone(),
            req.books_accessible.clone(),
        )
        .await?;

    let mut user_resp = UserInfoResponse::from(new_user);
    user_resp.libraries_accessible = req.libraries_accessible.unwrap_or_default();
    user_resp.books_accessible = req.books_accessible.unwrap_or_default();

    Ok((
        StatusCode::CREATED,
        Json(UserActionResponse {
            message: "User created successfully".to_string(),
            user: user_resp,
        }),
    ))
}

/// Handler for PATCH or PUT /api/users/:id - Update user (admin only)
pub async fn update_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    admin: crate::auth::middleware::AuthUser,
    Json(req): Json<UpdateUserRequest>,
) -> Result<impl IntoResponse> {
    if admin.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "update_user::permission",
            "Admin access required".to_string(),
        ));
    }

    let mut user = state.user_repo.find_by_id(&user_id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::User,
            "update_user::not_found",
            format!("User {} not found", user_id),
        )
    })?;

    if let Some(username) = req.username {
        if let Some(existing) = state.user_repo.find_by_username(&username).await?
            && existing.id != user_id
        {
            return Err(TingError::validation(
                InputErrorKind::Conflict,
                "update_user::validation",
                format!("Username '{}' already exists", username),
            ));
        }
        user.username = username;
    }

    if let Some(password) = req.password {
        user.password_hash = bcrypt::hash(&password, bcrypt::DEFAULT_COST).map_err(|e| {
            let message = format!("Failed to hash password: {}", e);
            TingError::plugin_execution(
                PluginErrorKind::Runtime,
                "update_user::plugin_execution",
                message,
            )
            .with_source(anyhow::anyhow!(e))
        })?;
    }

    if let Some(role) = req.role {
        user.role = role;
    }

    state.user_repo.update(&user).await?;

    if req.libraries_accessible.is_some() || req.books_accessible.is_some() {
        state
            .user_repo
            .update_permissions(
                &user.id,
                req.libraries_accessible.clone(),
                req.books_accessible.clone(),
            )
            .await?;
    }

    let mut user_resp = UserInfoResponse::from(user);
    user_resp.libraries_accessible = state.user_repo.get_accessible_libraries(&user_id).await?;
    user_resp.books_accessible = state.user_repo.get_accessible_books(&user_id).await?;

    Ok(Json(UserActionResponse {
        message: "User updated successfully".to_string(),
        user: user_resp,
    }))
}

/// Handler for DELETE /api/users/:id - Delete user (admin only)
pub async fn delete_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    admin: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    if admin.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "delete_user::permission",
            "Admin access required".to_string(),
        ));
    }

    if admin.id == user_id {
        return Err(TingError::validation(
            InputErrorKind::InvalidValue,
            "delete_user::validation",
            "Cannot delete your own account".to_string(),
        ));
    }

    state.user_repo.find_by_id(&user_id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::User,
            "delete_user::not_found",
            format!("User {} not found", user_id),
        )
    })?;

    state.user_repo.delete(&user_id).await?;

    Ok(Json(serde_json::json!({
        "message": "User deleted successfully"
    })))
}

/// Handler for GET /api/settings - Get user settings
pub async fn get_user_settings(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    match state.settings_repo.get_by_user(&user.id).await? {
        Some(settings) => {
            let mut response = UserSettingsResponse::from(settings);

            if let Some(ref json_val) = response.settings_json {
                if let Some(val) = json_val.get("sleep_timer_default")
                    && let Some(i) = val.as_i64()
                {
                    response.sleep_timer_default = i as i32;
                }
                if let Some(val) = json_val.get("auto_preload")
                    && let Some(b) = val.as_bool()
                {
                    response.auto_preload = b;
                }
                if let Some(val) = json_val.get("auto_cache")
                    && let Some(b) = val.as_bool()
                {
                    response.auto_cache = b;
                }
                if let Some(val) = json_val.get("widget_css")
                    && let Some(s) = val.as_str()
                {
                    response.widget_css = Some(s.to_string());
                }
            }

            mask_restricted_settings(&mut response, user.role == "admin");

            Ok(Json(response))
        }
        None => {
            let default_settings = crate::db::models::UserSettings {
                user_id: user.id.clone(),
                playback_speed: 1.0,
                theme: "auto".to_string(),
                auto_play: 1,
                skip_intro: 0,
                skip_outro: 0,
                settings_json: None,
                updated_at: chrono::Utc::now().to_rfc3339(),
            };
            let mut response = UserSettingsResponse::from(default_settings);
            mask_restricted_settings(&mut response, user.role == "admin");

            Ok(Json(response))
        }
    }
}

/// Handler for POST /api/settings - Update user settings (UPSERT)
pub async fn update_user_settings(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<UpdateUserSettingsRequest>,
) -> Result<impl IntoResponse> {
    if let Some(speed) = req.playback_speed
        && (!speed.is_finite() || !(0.5..=3.0).contains(&speed))
    {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "update_user_settings::invalid_request",
            "Playback speed must be between 0.5 and 3.0".into(),
        ));
    }
    let existing = state.settings_repo.get_by_user(&user.id).await?;

    let mut settings_obj = if let Some(ref s) = existing {
        if let Some(ref json_str) = s.settings_json {
            serde_json::from_str(json_str).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        }
    } else {
        serde_json::json!({})
    };
    let previous_preload = settings_obj["auto_preload"].as_bool().unwrap_or(false);
    let previous_cache =
        user.role == "admin" && settings_obj["auto_cache"].as_bool().unwrap_or(false);

    if let Some(sleep_timer) = req.sleep_timer_default {
        settings_obj["sleep_timer_default"] = serde_json::json!(sleep_timer);
    }
    if let Some(auto_preload) = req.auto_preload {
        settings_obj["auto_preload"] = serde_json::json!(auto_preload);
    }

    // Restricted settings (admin only)
    if user.role == "admin" {
        if let Some(auto_cache) = req.auto_cache {
            settings_obj["auto_cache"] = serde_json::json!(auto_cache);
        }
        if let Some(widget_css) = &req.widget_css {
            settings_obj["widget_css"] = serde_json::json!(widget_css);
        }
    } else {
        // If non-admin tries to update these, we silently ignore them or log a warning
        if req.widget_css.is_some() || req.auto_cache.is_some() {
            tracing::warn!(
                user_id = %user.id,
                message_key = "user.settings.restricted_update_ignored",
                message_params = %serde_json::json!({ "user_id": user.id }),
                "Non-admin attempted to update restricted settings"
            );
        }
    }

    for (k, v) in req.extra {
        // Prevent recursion: Do not merge settings_json back into itself
        // Also skip system fields that shouldn't be in the settings JSON
        if k != "settings_json" && k != "user_id" && k != "updated_at" {
            settings_obj[k] = v;
        }
    }

    // Clean up any potential recursive nesting from previous bugs in the existing object
    if let Some(obj) = settings_obj.as_object_mut() {
        obj.remove("settings_json");
        obj.remove("user_id");
        obj.remove("updated_at");
    }

    let settings = crate::db::models::UserSettings {
        user_id: user.id.clone(),
        playback_speed: req
            .playback_speed
            .unwrap_or_else(|| existing.as_ref().map(|s| s.playback_speed).unwrap_or(1.0)),
        theme: req.theme.unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|s| s.theme.clone())
                .unwrap_or_else(|| "auto".to_string())
        }),
        auto_play: req
            .auto_play
            .map(|v| if v { 1 } else { 0 })
            .unwrap_or_else(|| existing.as_ref().map(|s| s.auto_play).unwrap_or(1)),
        skip_intro: req
            .skip_intro
            .unwrap_or_else(|| existing.as_ref().map(|s| s.skip_intro).unwrap_or(0)),
        skip_outro: req
            .skip_outro
            .unwrap_or_else(|| existing.as_ref().map(|s| s.skip_outro).unwrap_or(0)),
        settings_json: Some(settings_obj.to_string()),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    state.settings_repo.upsert(&settings).await?;

    let preload_enabled = settings_obj["auto_preload"].as_bool().unwrap_or(false);
    let cache_enabled =
        user.role == "admin" && settings_obj["auto_cache"].as_bool().unwrap_or(false);
    if ((!preload_enabled && !cache_enabled)
        || preload_enabled != previous_preload
        || cache_enabled != previous_cache)
        && let Some((_, task)) = state.active_preload_tasks.lock().await.remove(&user.id)
    {
        task.abort();
    }

    let mut response = UserSettingsResponse::from(settings);
    if let Some(ref json_val) = response.settings_json {
        if let Some(val) = json_val.get("sleep_timer_default")
            && let Some(i) = val.as_i64()
        {
            response.sleep_timer_default = i as i32;
        }
        if let Some(val) = json_val.get("auto_preload")
            && let Some(b) = val.as_bool()
        {
            response.auto_preload = b;
        }
        if let Some(val) = json_val.get("auto_cache")
            && let Some(b) = val.as_bool()
        {
            response.auto_cache = b;
        }
        if let Some(val) = json_val.get("widget_css")
            && let Some(s) = val.as_str()
        {
            response.widget_css = Some(s.to_string());
        }
    }

    mask_restricted_settings(&mut response, user.role == "admin");

    Ok(Json(response))
}

/// Handler for GET /api/favorites - Get user's favorites
pub async fn get_favorites(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let favorites_list = state.favorite_repo.get_by_user(&user.id).await?;

    let mut books = Vec::new();
    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;

    for fav in favorites_list {
        if let Some(book) = state.book_repo.find_by_id(&fav.book_id).await? {
            let mut book_resp = crate::api::models::BookResponse::from(book);
            book_resp.is_favorite = true;
            book_resp.progress_percent = percentages.get(&book_resp.id).copied().unwrap_or(0.0);
            books.push(book_resp);
        }
    }

    Ok(Json(books))
}

/// Handler for POST /api/favorites/:bookId - Add book to favorites
pub async fn add_favorite(
    State(state): State<AppState>,
    Path(book_id): Path<String>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    state.book_repo.find_by_id(&book_id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Book,
            "add_favorite::not_found",
            format!("Book {} not found", book_id),
        )
    })?;

    if state.favorite_repo.is_favorited(&user.id, &book_id).await? {
        return Ok((
            StatusCode::OK,
            Json(FavoriteActionResponse {
                message: "Book is already in favorites".to_string(),
            }),
        ));
    }

    let favorite = crate::db::models::Favorite {
        id: Uuid::new_v4().to_string(),
        user_id: user.id.clone(),
        book_id: book_id.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    state.favorite_repo.add(&favorite).await?;

    Ok((
        StatusCode::CREATED,
        Json(FavoriteActionResponse {
            message: "Book added to favorites".to_string(),
        }),
    ))
}

/// Handler for DELETE /api/favorites/:bookId - Remove book from favorites
pub async fn remove_favorite(
    State(state): State<AppState>,
    Path(book_id): Path<String>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    state.favorite_repo.remove(&user.id, &book_id).await?;

    Ok(Json(FavoriteActionResponse {
        message: "Book removed from favorites".to_string(),
    }))
}

/// Handler for GET /api/progress/recent - Get recent playback progress
pub async fn get_recent_progress(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    let progress_list = state
        .progress_repo
        .get_recent_enriched(&user.id, None)
        .await?;
    let progress: Vec<ProgressResponse> = progress_list
        .into_iter()
        .map(|(p, b_title, c_url, l_id, c_title, c_dur)| {
            let mut response = ProgressResponse::from(p);
            response.book_title = b_title;
            response.cover_url = c_url;
            response.library_id = l_id;
            response.chapter_title = c_title;
            response.chapter_duration = c_dur;
            response
        })
        .collect();

    Ok(Json(progress))
}

/// Handler for DELETE /api/progress/recent - Clear playback history
pub async fn clear_recent_progress(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    state.progress_repo.clear_by_user(&user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Handler for POST /api/progress/recent/delete - Hide selected playback history
pub async fn delete_progress_history(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<DeleteProgressHistoryRequest>,
) -> Result<impl IntoResponse> {
    if req.book_ids.len() + req.progress_ids.len() + req.chapter_ids.len() > 500 {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "delete_progress_history::invalid_request",
            "Too many history selections".into(),
        ));
    }
    let deleted =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .clear_history(
                &user.id,
                req.all,
                req.book_ids,
                req.progress_ids,
                req.chapter_ids,
                req.clear_progress,
            )
            .await?;
    Ok(Json(DeleteProgressHistoryResponse { deleted }))
}

/// Handler for GET /api/progress/:bookId - Get book playback progress
pub async fn get_book_progress(
    State(state): State<AppState>,
    Path(book_id): Path<String>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    if !state
        .book_repo
        .check_access(&book_id, &user.id, user.role == "admin")
        .await?
    {
        return Err(TingError::permission(
            AccessErrorKind::ResourceDenied,
            "get_book_progress::permission",
            "无权读取这本书的进度".into(),
        ));
    }
    let (snapshot, revision) = state.progress_repo.snapshot(&user.id, &book_id).await?;
    match snapshot {
        Some(progress) => {
            let chapter = if let Some(id) = progress.chapter_id.as_deref() {
                state.chapter_repo.find_by_id(id).await?
            } else {
                None
            };
            let mut response = ProgressResponse::from(progress);
            response.chapter_title = chapter.as_ref().and_then(|item| item.title.clone());
            response.chapter_duration = chapter.and_then(|item| item.duration);
            let mut value = serde_json::to_value(response).map_err(|e| {
                TingError::serialization(
                    crate::core::app::error::CodecErrorKind::InvalidData,
                    "progress.response",
                    e.to_string(),
                )
            })?;
            value["revision"] = revision.into();
            Ok(Json(value))
        }
        None => Ok(Json(
            serde_json::json!({"book_id":book_id,"revision":revision,"position":0}),
        )),
    }
}

/// Handler for POST /api/progress - Update playback progress (UPSERT)
pub async fn update_progress(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
    Json(req): Json<UpdateProgressRequest>,
) -> Result<impl IntoResponse> {
    let book = state
        .book_repo
        .find_by_id(&req.book_id)
        .await?
        .ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Book,
                "update_progress::not_found",
                format!("Book {} not found", req.book_id),
            )
        })?;

    if !state
        .book_repo
        .check_access(&book.id, &user.id, user.role == "admin")
        .await?
    {
        return Err(TingError::permission(
            AccessErrorKind::ResourceDenied,
            "update_progress::permission",
            "Book access denied".into(),
        ));
    }
    if !req.position.is_finite()
        || req.position < 0.0
        || req.base_revision.is_some_and(|value| value < 0)
        || req
            .device_id
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || req
            .duration
            .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "update_progress::invalid_request",
            "Invalid playback progress".into(),
        ));
    }
    if let Some(ref chapter_id) = req.chapter_id {
        let chapter = state
            .chapter_repo
            .find_by_id(chapter_id)
            .await?
            .ok_or_else(|| {
                TingError::not_found(
                    LookupErrorKind::Chapter,
                    "update_progress::not_found",
                    format!("Chapter {} not found", chapter_id),
                )
            })?;

        if chapter.book_id != book.id {
            return Err(TingError::validation(
                InputErrorKind::OutOfRange,
                "update_progress::validation",
                "Chapter does not belong to the specified book".to_string(),
            ));
        }
    }

    let progress = crate::db::models::Progress {
        id: Uuid::new_v4().to_string(),
        user_id: user.id.clone(),
        book_id: req.book_id.clone(),
        chapter_id: req.chapter_id.clone(),
        position: req.position,
        duration: req.duration,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    let Some(revision) = state
        .progress_repo
        .upsert_checked(&progress, req.base_revision, req.device_id.clone())
        .await?
    else {
        let (current, revision) = state.progress_repo.snapshot(&user.id, &req.book_id).await?;
        let mut current = current
            .map(|item| serde_json::json!(item))
            .unwrap_or_else(|| serde_json::json!({"book_id":req.book_id,"position":0}));
        current["revision"] = revision.into();
        return Ok((StatusCode::CONFLICT, Json(serde_json::json!({"code":"PROGRESS_CONFLICT","message":"其他设备已更新进度，请选择续播位置","current":current}))).into_response());
    };

    if let Some(playback_start) = req.playback_start {
        crate::api::handlers::media::audit::record_playback_start(
            &state,
            &user.id,
            &req.book_id,
            req.chapter_id.as_deref(),
            playback_start,
        )
        .await;
    }

    state.ws_manager.broadcast(&user.id, &serde_json::json!({"type":"progress_updated","book_id":req.book_id,"chapter_id":req.chapter_id,"position":req.position,"updated_at":progress.updated_at,"revision":revision,"device_id":req.device_id}).to_string()).await;
    let mut value = serde_json::json!(ProgressResponse::from(progress));
    value["revision"] = revision.into();
    Ok((StatusCode::OK, Json(value)).into_response())
}

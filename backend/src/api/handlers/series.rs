use crate::api::models::{
    BookResponse, CreateSeriesRequest, MergeSeriesRequest, SeriesResponse, UpdateSeriesRequest,
};
use crate::api::state::AppState;
use crate::core::app::error::{AccessErrorKind, InputErrorKind, LookupErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::db::models::{Series, SeriesBook};
use crate::db::repository::Repository;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;

use crate::auth::middleware::AuthUser;

/// Handler for GET /api/v1/series - List all series
pub async fn list_series(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<impl IntoResponse> {
    let library_id = params.get("library_id").cloned();
    let is_admin = user.role == "admin";

    let series_list = state
        .series_repo
        .find_with_filters(&user.id, is_admin, library_id)
        .await?;

    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    let mut response = Vec::new();
    for series in series_list {
        let mut s_res = SeriesResponse::from(series.clone());
        let books = state
            .series_repo
            .find_books_by_series_with_filters(&series.id, &user.id, is_admin)
            .await?;

        s_res.books = books
            .into_iter()
            .map(|(b, _)| {
                let mut response = BookResponse::from(b);
                response.progress_percent = percentages.get(&response.id).copied().unwrap_or(0.0);
                response
            })
            .collect();
        response.push(s_res);
    }

    Ok(Json(response))
}

/// Handler for GET /api/v1/series/:id - Get series by ID
pub async fn get_series(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let series = state.series_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Series,
            "get_series::not_found",
            format!("Series with id {} not found", id),
        )
    })?;

    let is_admin = user.role == "admin";

    // Check if user has access to this series
    if !state
        .series_repo
        .check_access(&series.id, &user.id, is_admin)
        .await?
    {
        return Err(TingError::permission(
            AccessErrorKind::ResourceDenied,
            "get_series::permission",
            "No access to this series".to_string(),
        ));
    }

    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    let mut response = SeriesResponse::from(series.clone());
    let books = state
        .series_repo
        .find_books_by_series_with_filters(&series.id, &user.id, is_admin)
        .await?;
    let library_types: std::collections::HashMap<String, String> = state
        .library_repo
        .find_all()
        .await?
        .into_iter()
        .map(|library| (library.id, library.library_type))
        .collect();
    response.books = books
        .into_iter()
        .map(|(b, _)| {
            let mut response = BookResponse::from(b);
            response.library_type = library_types.get(&response.library_id).cloned();
            response.progress_percent = percentages.get(&response.id).copied().unwrap_or(0.0);
            response
        })
        .collect();

    Ok(Json(response))
}

/// Handler for POST /api/v1/series - Create a new series
pub async fn create_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateSeriesRequest>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "create_series::permission",
            "Only admin can create series".to_string(),
        ));
    }

    let series_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // The DB enforces one series per identity per library. Report the clash
    // instead of surfacing a raw constraint failure.
    if state
        .series_repo
        .find_by_title_and_library(&req.title, &req.library_id)
        .await?
        .is_some()
    {
        return Err(TingError::validation(
            InputErrorKind::Conflict,
            "create_series::duplicate",
            format!(
                "Series '{}' already exists in this library",
                req.title.trim()
            ),
        ));
    }

    // Default metadata from first book if not provided
    let mut author = req.author;
    let mut narrator = req.narrator;
    let mut cover_url = req.cover_url;
    let mut description = req.description;

    if !req.book_ids.is_empty()
        && let Some(first_book) = state.book_repo.find_by_id(&req.book_ids[0]).await?
    {
        if author.is_none() {
            author = first_book.author;
        }
        if narrator.is_none() {
            narrator = first_book.narrator;
        }
        if cover_url.is_none() {
            cover_url = first_book.cover_url;
        }
        if description.is_none() {
            description = first_book.description;
        }
    }

    let series = Series {
        id: series_id.clone(),
        library_id: req.library_id,
        title: req.title.trim().to_string(),
        author,
        narrator,
        cover_url,
        description,
        created_at: now.clone(),
        updated_at: now,
    };

    state.series_repo.create(&series).await?;

    // Add books
    let mut books_to_add = Vec::new();
    for book_id in &req.book_ids {
        if let Some(book) = state.book_repo.find_by_id(book_id).await? {
            books_to_add.push(book);
        }
    }

    // Sort naturally by title for new series creation
    books_to_add.sort_by(|a, b| {
        let t1 = a.title.as_deref().unwrap_or("");
        let t2 = b.title.as_deref().unwrap_or("");
        natord::compare(t1, t2)
    });

    for (idx, book) in books_to_add.iter().enumerate() {
        state
            .series_repo
            .add_book(SeriesBook {
                series_id: series_id.clone(),
                book_id: book.id.clone(),
                book_order: (idx + 1) as i32,
            })
            .await?;

        // Update metadata.json
        if let Err(e) = update_book_metadata_series(&state, &book.id).await {
            tracing::warn!(
                book_id = %book.id,
                error = %e,
                message_key = "metadata.json.update_failed",
                message_params = %serde_json::json!({
                    "book_id": book.id,
                    "error": e.to_string(),
                }),
                "Failed to update book metadata.json"
            );
        }
    }

    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    let mut response = SeriesResponse::from(series);
    // Fetch added books to return full response
    let books = state.series_repo.find_books_by_series(&series_id).await?;
    response.books = books
        .into_iter()
        .map(|(b, _)| {
            let mut response = BookResponse::from(b);
            response.progress_percent = percentages.get(&response.id).copied().unwrap_or(0.0);
            response
        })
        .collect();

    Ok((StatusCode::CREATED, Json(response)))
}

/// Handler for PUT /api/v1/series/:id - Update a series
pub async fn update_series(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateSeriesRequest>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "update_series::permission",
            "Only admin can update series".to_string(),
        ));
    }

    let existing_series = state.series_repo.find_by_id(&id).await?.ok_or_else(|| {
        TingError::not_found(
            LookupErrorKind::Series,
            "update_series::not_found",
            format!("Series with id {} not found", id),
        )
    })?;

    // Renaming onto another existing series would merge two collections by
    // accident; the user has to do that deliberately in the UI.
    if let Some(title) = req.title.as_deref()
        && let Some(other) = state
            .series_repo
            .find_by_title_and_library(title, &existing_series.library_id)
            .await?
        && other.id != id
    {
        return Err(TingError::validation(
            InputErrorKind::Conflict,
            "update_series::duplicate",
            format!("Series '{title}' already exists in this library"),
        ));
    }

    let updated_series = Series {
        id: existing_series.id,
        library_id: existing_series.library_id,
        title: req
            .title
            .as_ref()
            .map(|t| t.trim().to_string())
            .unwrap_or(existing_series.title),
        author: if req.author.is_some() {
            req.author
        } else {
            existing_series.author
        },
        narrator: if req.narrator.is_some() {
            req.narrator
        } else {
            existing_series.narrator
        },
        cover_url: if req.cover_url.is_some() {
            req.cover_url
        } else {
            existing_series.cover_url
        },
        description: if req.description.is_some() {
            req.description
        } else {
            existing_series.description
        },
        created_at: existing_series.created_at,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    state.series_repo.update(&updated_series).await?;

    // Update books if provided
    if let Some(book_ids) = req.book_ids {
        // We need to sync the books list.
        // Simplest way: remove all and re-add.
        // But SeriesRepository doesn't have "remove all books".
        // We can get current books, find diff.

        let current_books = state.series_repo.find_books_by_series(&id).await?;
        let current_ids: Vec<String> = current_books.iter().map(|(b, _)| b.id.clone()).collect();
        let mut affected_books = std::collections::HashSet::new();

        // Remove books that are not in new list
        for book_id in &current_ids {
            if !book_ids.contains(book_id) {
                state.series_repo.remove_book(&id, book_id).await?;
                affected_books.insert(book_id.clone());
            }
        }

        // Add/Update books
        for (idx, book_id) in book_ids.iter().enumerate() {
            state
                .series_repo
                .add_book(SeriesBook {
                    series_id: id.clone(),
                    book_id: book_id.clone(),
                    book_order: (idx + 1) as i32,
                })
                .await?;
            affected_books.insert(book_id.clone());
        }

        // Update metadata.json for all affected books
        for book_id in affected_books {
            if let Err(e) = update_book_metadata_series(&state, &book_id).await {
                tracing::warn!(
                    book_id = %book_id,
                    error = %e,
                    message_key = "metadata.json.update_failed",
                    message_params = %serde_json::json!({
                        "book_id": book_id,
                        "error": e.to_string(),
                    }),
                    "Failed to update book metadata.json"
                );
            }
        }
    } else {
        // If only title changed, we should update metadata for all books in series
        if req.title.is_some() {
            let books = state.series_repo.find_books_by_series(&id).await?;
            for (book, _) in books {
                if let Err(e) = update_book_metadata_series(&state, &book.id).await {
                    tracing::warn!(
                        book_id = %book.id,
                        error = %e,
                        message_key = "metadata.json.update_failed",
                        message_params = %serde_json::json!({
                            "book_id": book.id,
                            "error": e.to_string(),
                        }),
                        "Failed to update book metadata.json"
                    );
                }
            }
        }
    }

    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    let mut response = SeriesResponse::from(updated_series);
    let books = state.series_repo.find_books_by_series(&id).await?;
    response.books = books
        .into_iter()
        .map(|(b, _)| {
            let mut response = BookResponse::from(b);
            response.progress_percent = percentages.get(&response.id).copied().unwrap_or(0.0);
            response
        })
        .collect();

    Ok(Json(response))
}

/// Handler for DELETE /api/v1/series/:id - Delete a series
pub async fn delete_series(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "delete_series::permission",
            "Only admin can delete series".to_string(),
        ));
    }

    if state.series_repo.find_by_id(&id).await?.is_none() {
        return Err(TingError::not_found(
            LookupErrorKind::Series,
            "delete_series::not_found",
            format!("Series with id {} not found", id),
        ));
    }

    // Get books before deletion to update metadata
    let books = state.series_repo.find_books_by_series(&id).await?;

    state.series_repo.delete(&id).await?;

    // Update metadata.json for affected books
    for (book, _) in books {
        if let Err(e) = update_book_metadata_series(&state, &book.id).await {
            tracing::warn!(
                book_id = %book.id,
                error = %e,
                message_key = "metadata.json.update_failed",
                message_params = %serde_json::json!({
                    "book_id": book.id,
                    "error": e.to_string(),
                }),
                "Failed to update book metadata.json"
            );
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn update_book_metadata_series(state: &AppState, book_id: &str) -> Result<()> {
    if let Some(book) = state.book_repo.find_by_id(book_id).await? {
        state
            .metadata_service()
            .sync(
                &book,
                crate::core::books::metadata::MetadataChange::Series,
                &Default::default(),
            )
            .await?;
    }
    Ok(())
}

/// Handler for POST /api/v1/series/merge - Fold several series into one
///
/// Series identity is inferred, so duplicates appear whenever the naming
/// rules miss a variant. Merging lets the user resolve that directly instead
/// of waiting for the heuristic to be taught a new pattern.
pub async fn merge_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MergeSeriesRequest>,
) -> Result<impl IntoResponse> {
    if user.role != "admin" {
        return Err(TingError::permission(
            AccessErrorKind::AdministratorRequired,
            "merge_series::permission",
            "Only admin can merge series".to_string(),
        ));
    }

    let target = state
        .series_repo
        .find_by_id(&req.target_id)
        .await?
        .ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Series,
                "merge_series::target_not_found",
                format!("Series with id {} not found", req.target_id),
            )
        })?;

    let mut source_ids = Vec::new();
    for id in req.source_ids {
        if id == target.id || source_ids.contains(&id) {
            continue;
        }
        let source = state.series_repo.find_by_id(&id).await?.ok_or_else(|| {
            TingError::not_found(
                LookupErrorKind::Series,
                "merge_series::source_not_found",
                format!("Series with id {id} not found"),
            )
        })?;
        // Memberships are library-scoped; crossing libraries would move books
        // into a collection they no longer belong to.
        if source.library_id != target.library_id {
            return Err(TingError::validation(
                InputErrorKind::InvalidValue,
                "merge_series::library_mismatch",
                "Cannot merge series from different libraries".to_string(),
            ));
        }
        source_ids.push(id);
    }

    if source_ids.is_empty() {
        return Err(TingError::validation(
            InputErrorKind::MissingField,
            "merge_series::no_sources",
            "At least one other series is required".to_string(),
        ));
    }

    let books_before = state.series_repo.find_books_by_series(&target.id).await?;
    let moved = state.series_repo.merge_into(&target.id, source_ids).await?;
    let books_after = state.series_repo.find_books_by_series(&target.id).await?;

    // Sidecar files list every series a book belongs to; the absorbed rows are
    // gone now, so any book whose memberships changed needs a rewrite.
    let mut affected: Vec<String> = books_before.into_iter().map(|(b, _)| b.id).collect();
    affected.extend(books_after.iter().map(|(b, _)| b.id.clone()));
    affected.sort();
    affected.dedup();
    for book_id in affected {
        if let Err(e) = update_book_metadata_series(&state, &book_id).await {
            tracing::warn!(
                book_id = %book_id,
                error = %e,
                message_key = "metadata.json.update_failed",
                message_params = %serde_json::json!({
                    "book_id": book_id,
                    "error": e.to_string(),
                }),
                "Failed to update book metadata.json"
            );
        }
    }

    let percentages =
        crate::db::repository::reading::ReadingRepository::new(state.book_repo.db().clone())
            .percentages(&user.id)
            .await?;
    let merged = state
        .series_repo
        .find_by_id(&target.id)
        .await?
        .unwrap_or(target);
    let mut response = SeriesResponse::from(merged);
    response.books = books_after
        .into_iter()
        .map(|(b, _)| {
            let mut book = BookResponse::from(b);
            book.progress_percent = percentages.get(&book.id).copied().unwrap_or(0.0);
            book
        })
        .collect();
    tracing::info!(
        series_id = %response.id,
        members = moved,
        message_key = "series.merged",
        message_params = %serde_json::json!({ "title": response.title, "members": moved }),
        "Series merged"
    );
    Ok(Json(response))
}

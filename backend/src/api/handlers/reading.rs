use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{AccessErrorKind, InputErrorKind, LookupErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::core::books::{BookmarkService, CreateBookmark};
use crate::db::repository::{Repository, reading::ReadingRepository};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

#[derive(Default, Deserialize)]
pub struct ReadingPageQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}
impl ReadingPageQuery {
    fn bounds(&self) -> (usize, usize) {
        (
            self.page.unwrap_or(1).clamp(1, 1_000_000),
            self.page_size.unwrap_or(40).clamp(1, 100),
        )
    }
}
fn repo(state: &AppState) -> ReadingRepository {
    ReadingRepository::new(state.book_repo.db().clone())
}
fn bookmarks<'a>(state: &'a AppState, user: &'a AuthUser) -> BookmarkService<'a> {
    BookmarkService::new(
        &state.book_repo,
        &state.chapter_repo,
        &user.id,
        user.role == "admin",
    )
}
async fn ensure_access(state: &AppState, user: &AuthUser, book: &str) -> Result<()> {
    if state.book_repo.find_by_id(book).await?.is_none() {
        return Err(TingError::not_found(
            LookupErrorKind::Book,
            "ensure_access::not_found",
            "Book not found".into(),
        ));
    }
    if !state
        .book_repo
        .check_access(book, &user.id, user.role == "admin")
        .await?
    {
        return Err(TingError::permission(
            AccessErrorKind::ResourceDenied,
            "ensure_access::permission",
            "Book access denied".into(),
        ));
    }
    Ok(())
}
pub async fn history_books(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state)
            .activity_books(&user.id, user.role == "admin", false, page, size)
            .await?,
    ))
}

pub async fn history_summary(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<impl IntoResponse> {
    Ok(Json(
        repo(&state)
            .history_summary(&user.id, user.role == "admin")
            .await?,
    ))
}
pub async fn history_chapters(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    ensure_access(&state, &user, &book).await?;
    let (page, size) = query.bounds();
    Ok(Json(
        repo(&state)
            .history_chapters(&user.id, &book, page, size)
            .await?,
    ))
}
pub async fn bookmark_books(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    let (page, size) = query.bounds();
    Ok(Json(bookmarks(&state, &user).books(page, size).await?))
}
pub async fn list_bookmarks(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
    Query(query): Query<ReadingPageQuery>,
) -> Result<impl IntoResponse> {
    let (page, size) = query.bounds();
    Ok(Json(
        bookmarks(&state, &user).list(&book, page, size).await?,
    ))
}
#[derive(Deserialize)]
pub struct MarkBooksRequest {
    pub book_ids: Vec<String>,
    pub read: bool,
}
pub async fn mark_books(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<MarkBooksRequest>,
) -> Result<impl IntoResponse> {
    if request.book_ids.is_empty() || request.book_ids.len() > 200 {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "mark_books::invalid_request",
            "Select between 1 and 200 books".into(),
        ));
    }
    for book in &request.book_ids {
        ensure_access(&state, &user, book).await?;
    }
    repo(&state)
        .mark_books(&user.id, request.book_ids, request.read)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub type AddBookmarkRequest = CreateBookmark;

#[derive(Deserialize)]
pub struct MarkChaptersRequest {
    pub chapter_ids: Vec<String>,
    pub read: bool,
}

pub async fn mark_chapters(
    State(state): State<AppState>,
    user: AuthUser,
    Path(book): Path<String>,
    Json(request): Json<MarkChaptersRequest>,
) -> Result<impl IntoResponse> {
    if request.chapter_ids.is_empty() || request.chapter_ids.len() > 200 {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "mark_chapters::invalid_request",
            "请选择 1 至 200 个章节".into(),
        ));
    }
    ensure_access(&state, &user, &book).await?;
    repo(&state)
        .mark_chapters(&user.id, &book, request.chapter_ids, request.read)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn add_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<AddBookmarkRequest>,
) -> Result<impl IntoResponse> {
    Ok((
        StatusCode::CREATED,
        Json(bookmarks(&state, &user).create(request).await?),
    ))
}
#[derive(Deserialize)]
pub struct EditBookmarkRequest {
    pub note: String,
}
pub async fn edit_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(request): Json<EditBookmarkRequest>,
) -> Result<impl IntoResponse> {
    bookmarks(&state, &user).update(&id, request.note).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn delete_bookmark(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    bookmarks(&state, &user).delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

//! WebSocket handler for real-time progress sync

use crate::api::state::AppState;
use crate::api::ws::manager::WsSessionManager;
use crate::auth::jwt;
use crate::core::app::error::{AuthenticationErrorKind, CodecErrorKind, ErrorResponse, TingError};
use crate::db::models::Progress;
use crate::db::repository::Repository;
use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

/// WebSocket query parameters (token auth)
#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// Client → Server message
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "progress_update")]
    ProgressUpdate {
        book_id: String,
        chapter_id: Option<String>,
        position: f64,
        playback_start: Option<f64>,
        base_revision: Option<i64>,
        device_id: Option<String>,
    },
    #[serde(rename = "ping")]
    Ping,
}

/// Server → Client message
#[derive(Serialize, Debug)]
#[serde(tag = "type")]
enum ServerMessage {
    #[serde(rename = "progress_updated")]
    ProgressUpdated {
        book_id: String,
        chapter_id: Option<String>,
        position: f64,
        updated_at: String,
        revision: i64,
        device_id: Option<String>,
    },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "error")]
    Error {
        #[serde(flatten)]
        error: ErrorResponse,
    },
}

/// Handle WebSocket upgrade request
pub async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = params.token;

    // Authenticate via token
    let user_id = match authenticate_ws_token(&state, token).await {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };

    // Upgrade and pass state + user_id to the handler
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state, user_id))
}

/// Validate the WebSocket token and return the user_id
async fn authenticate_ws_token(
    state: &AppState,
    token: Option<String>,
) -> Result<String, TingError> {
    let token = token.ok_or_else(|| {
        TingError::authentication(
            AuthenticationErrorKind::MissingCredentials,
            "authenticate_ws_token::authentication",
            "Missing token parameter".to_string(),
        )
    })?;

    let claims = if let Some(key_manager) = &state.jwt_key_manager {
        let secrets = key_manager.get_validation_secrets().await;
        jwt::validate_token_with_secrets(&token, &secrets)?
    } else {
        jwt::validate_token(&token, &state.jwt_secret)?
    };

    // Verify user exists in DB
    let user = state.user_repo.find_by_id(&claims.user_id).await?;

    match user {
        Some(_) => Ok(claims.user_id),
        None => Err(TingError::authentication(
            AuthenticationErrorKind::UserNotFound,
            "authenticate_ws_token::authentication",
            "User not found".to_string(),
        )),
    }
}

/// Handle an established WebSocket connection
async fn handle_ws_connection(socket: WebSocket, state: AppState, user_id: String) {
    debug!("WebSocket connection established: user={}", &user_id);
    let (mut sender, mut receiver) = socket.split();
    let ws_manager = state.ws_manager.clone();

    // Subscribe to broadcast channel for this user's progress updates
    let mut broadcast_rx = ws_manager.subscribe(&user_id).await;

    let user_id_send = user_id.clone();
    let user_id_recv = user_id.clone();

    // Task: forward broadcast messages to the WebSocket client
    let send_task: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(msg) => {
                    if sender.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    debug!(
                        "WS broadcast lagged by {} messages for user {}",
                        n, &user_id_send
                    );
                }
                Err(RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // Task: handle incoming messages from the WebSocket client
    let recv_task: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    handle_client_message(&state, &ws_manager, &user_id_recv, &text).await;
                }
                Ok(Message::Close(_)) => {
                    break;
                }
                Err(e) => {
                    debug!("WS receive error for user {}: {}", &user_id_recv, e);
                    break;
                }
                _ => {}
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    debug!("WebSocket connection closed: user={}", &user_id);
}

/// Process an incoming message from the client
async fn handle_client_message(
    state: &AppState,
    ws_manager: &Arc<WsSessionManager>,
    user_id: &str,
    text: &str,
) {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            warn!("Invalid WS message from user {}: {}", user_id, e);
            let failure = TingError::deserialization(
                CodecErrorKind::InvalidData,
                "websocket.receive",
                format!("Invalid message: {e}"),
            )
            .with_source(e.into());
            let error = serde_json::to_string(&ServerMessage::Error {
                error: ErrorResponse::from_error(&failure),
            })
            .unwrap_or_default();
            ws_manager.broadcast(user_id, &error).await;
            return;
        }
    };

    match msg {
        ClientMessage::ProgressUpdate {
            book_id,
            chapter_id,
            position,
            playback_start,
            base_revision,
            device_id,
        } => {
            let user = state.user_repo.find_by_id(user_id).await.ok().flatten();
            let allowed = match user.as_ref() {
                Some(user) => state
                    .book_repo
                    .check_access(&book_id, user_id, user.role == "admin")
                    .await
                    .unwrap_or(false),
                None => false,
            };
            let chapter_valid = match chapter_id.as_deref() {
                Some(id) => state
                    .chapter_repo
                    .find_by_id(id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|chapter| chapter.book_id == book_id),
                None => true,
            };
            if !allowed
                || !chapter_valid
                || !position.is_finite()
                || position < 0.0
                || base_revision.is_some_and(|value| value < 0)
                || device_id.as_ref().is_some_and(|value| value.len() > 128)
            {
                ws_manager.broadcast(user_id,&serde_json::json!({"type":"error","code":"INVALID_PROGRESS","message":"无权访问或进度参数无效"}).to_string()).await;
                return;
            }
            // Save progress to database
            let progress = Progress {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: user_id.to_string(),
                book_id: book_id.clone(),
                chapter_id: chapter_id.clone(),
                position,
                duration: None,
                updated_at: chrono::Utc::now().to_rfc3339(),
            };

            debug!(
                "WS received progress: user={} book={} ch={} pos={}s",
                &user_id,
                &book_id,
                chapter_id.as_deref().unwrap_or("-"),
                position
            );

            let saved = state
                .progress_repo
                .upsert_checked(&progress, base_revision, device_id.clone())
                .await;
            if let Err(e) = &saved {
                warn!(
                    user_id = %user_id,
                    error = %e,
                    message_key = "websocket.progress_save_failed",
                    message_params = %serde_json::json!({
                        "user_id": user_id.to_string(),
                        "error": e.to_string(),
                    }),
                    "WebSocket progress save failed"
                );
                if let Ok(error) = serde_json::to_string(&ServerMessage::Error {
                    error: ErrorResponse::from_error(e),
                }) {
                    ws_manager.broadcast(user_id, &error).await;
                }
                return;
            }
            let Some(revision) = saved.ok().flatten() else {
                if let Ok((current, revision)) =
                    state.progress_repo.snapshot(user_id, &book_id).await
                {
                    let mut current = current
                        .map(|p| serde_json::json!(p))
                        .unwrap_or_else(|| serde_json::json!({"book_id":book_id,"position":0}));
                    current["revision"] = revision.into();
                    ws_manager.broadcast(user_id,&serde_json::json!({"type":"progress_conflict","book_id":book_id,"device_id":device_id,"current":current}).to_string()).await;
                }
                return;
            };

            if let Some(playback_start) = playback_start {
                crate::api::handlers::media::audit::record_playback_start(
                    state,
                    user_id,
                    &book_id,
                    chapter_id.as_deref(),
                    playback_start,
                )
                .await;
            }

            // Broadcast to all sessions of this user (including other devices)
            let broadcast_msg = serde_json::to_string(&ServerMessage::ProgressUpdated {
                book_id,
                chapter_id,
                position,
                updated_at: progress.updated_at,
                revision,
                device_id,
            })
            .unwrap_or_default();

            ws_manager.broadcast(user_id, &broadcast_msg).await;
        }
        ClientMessage::Ping => {
            let pong = serde_json::to_string(&ServerMessage::Pong).unwrap_or_default();
            ws_manager.broadcast(user_id, &pong).await;
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api/ws.rs"]
mod tests;

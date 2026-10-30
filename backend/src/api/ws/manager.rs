//! WebSocket session manager
//!
//! Tracks connected WebSocket sessions per user and broadcasts progress updates.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

/// Maximum number of queued messages per user's broadcast channel
const BROADCAST_CAPACITY: usize = 64;

/// Manages WebSocket sessions grouped by user ID
pub struct WsSessionManager {
    /// Map of user_id → broadcast sender for progress updates
    channels: RwLock<HashMap<String, broadcast::Sender<String>>>,
}

impl WsSessionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            channels: RwLock::new(HashMap::new()),
        })
    }

    /// Subscribe to progress updates for a user.
    /// Returns a receiver that will get all progress update messages for this user.
    pub async fn subscribe(&self, user_id: &str) -> broadcast::Receiver<String> {
        let mut channels = self.channels.write().await;
        channels
            .entry(user_id.to_string())
            .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
            .subscribe()
    }

    pub async fn connected_users(&self) -> Vec<String> {
        self.channels
            .read()
            .await
            .iter()
            .filter(|(_, sender)| sender.receiver_count() > 0)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Broadcast a progress update to all sessions of a user.
    /// The message is a JSON string that will be sent to all connected clients.
    pub async fn broadcast(&self, user_id: &str, message: &str) {
        let channels = self.channels.read().await;
        if let Some(sender) = channels.get(user_id) {
            let _ = sender.send(message.to_string());
        }
    }

    /// Remove a user's channel (e.g., when all sessions disconnect).
    pub async fn unsubscribe(&self, user_id: &str) {
        self.channels.write().await.remove(user_id);
    }

    /// Clean up channels with no active receivers.
    /// This can be called periodically to free memory from inactive users.
    pub async fn cleanup_inactive_channels(&self) {
        let mut channels = self.channels.write().await;
        channels.retain(|_, sender| sender.receiver_count() > 0);
    }

    /// Get statistics about active channels
    pub async fn get_stats(&self) -> (usize, usize) {
        let channels = self.channels.read().await;
        let total_channels = channels.len();
        let active_receivers: usize = channels
            .values()
            .map(|sender| sender.receiver_count())
            .sum();
        (total_channels, active_receivers)
    }
}

use crate::core::app::error::{CodecErrorKind, NetworkErrorKind, PluginErrorKind};
use crate::core::app::error::{Result, TingError};
use futures::StreamExt;
use reqwest::{Url, header::LOCATION};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

use super::types::{LocalizedText, Permission, PluginCapability};

const MAX_PLUGIN_DOWNLOAD_BYTES: u64 = 50 * 1024 * 1024;
const MAX_PLUGIN_DOWNLOAD_REDIRECTS: usize = 5;

/// Cache entry for store plugins
#[derive(Debug, Clone)]
struct CacheEntry {
    key: String,
    plugins: Vec<StorePlugin>,
    timestamp: Instant,
}

/// Cache for store plugins with 1 hour TTL
pub struct PluginCache {
    cache: Arc<RwLock<Option<CacheEntry>>>,
    ttl: Duration,
}

impl PluginCache {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(None)),
            ttl: Duration::from_secs(3600), // 1 hour
        }
    }

    pub async fn get(&self, key: &str) -> Option<Vec<StorePlugin>> {
        let cache = self.cache.read().await;
        if let Some(entry) = cache.as_ref() {
            if entry.key != key {
                info!("Plugin cache miss for key {}", key);
                return None;
            }
            if entry.timestamp.elapsed() < self.ttl {
                info!("Plugin cache hit for key {}", key);
                return Some(entry.plugins.clone());
            }
            info!("Plugin cache expired for key {}", key);
        }
        None
    }

    pub async fn set(&self, key: String, plugins: Vec<StorePlugin>) {
        let mut cache = self.cache.write().await;
        *cache = Some(CacheEntry {
            key,
            plugins,
            timestamp: Instant::now(),
        });
        info!("Plugin cache updated");
    }

    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        *cache = None;
        info!("Plugin cache cleared");
    }
}

impl Default for PluginCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Plugin information from the store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub long_description: Option<String>,
    pub icon: Option<String>,
    pub repo: Option<String>,
    pub version: String,
    pub download_url: serde_json::Value, // String or Map<String, String>
    pub size: Option<serde_json::Value>, // String or Map<String, String>
    pub date: Option<String>,
    pub downloads: Option<Vec<StoreDownload>>,
    pub dependencies: Option<Vec<String>>,
    /// Runtime type: "wasm", "javascript", or "native"
    #[serde(default)]
    pub runtime: Option<String>,
    /// License identifier (e.g., "MIT")
    #[serde(default)]
    pub license: Option<String>,
    /// Plugin author
    #[serde(default)]
    pub author: Option<String>,
    /// Localized descriptions keyed by locale, e.g. zh/en/ja
    #[serde(default)]
    pub description_i18n: LocalizedText,
    /// Required permissions
    #[serde(default)]
    pub permissions: Option<Vec<Permission>>,
    /// Configuration schema (JSON Schema format)
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// Minimum core version required
    #[serde(default)]
    pub min_core_version: Option<String>,
    /// Minimum Flutter client version required for client-facing plugins
    #[serde(default)]
    pub min_flutter_version: Option<String>,
    /// Whether this plugin should only be shown and used by admin users
    #[serde(default)]
    pub admin_only: bool,
    /// Capability declarations used by the plugin base.
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
}

impl StorePlugin {
    pub fn normalize_i18n(&mut self) {
        self.description_i18n
            .entry("zh".to_string())
            .or_insert_with(|| self.description.clone());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreDownload {
    pub name: String,
    pub url: String,
}

/// Parse the plugin-store provider response into store plugins.
pub fn parse_store_plugins_response(value: serde_json::Value) -> Result<Vec<StorePlugin>> {
    let payload = value.get("plugins").cloned().unwrap_or(value);
    let mut plugins: Vec<StorePlugin> = serde_json::from_value(payload).map_err(|e| {
        let message = format!("Failed to parse store provider response: {}", e);
        TingError::serialization(
            CodecErrorKind::InvalidData,
            "parse_store_plugins_response::serialization",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })?;
    for plugin in &mut plugins {
        plugin.normalize_i18n();
    }
    Ok(plugins)
}

/// Get the download URL for the current platform
pub fn get_download_url(plugin: &StorePlugin) -> Result<String> {
    // Check if download_url is a string (universal or direct package plugin)
    if let Some(url) = plugin.download_url.as_str() {
        return Ok(url.to_string());
    }

    // Check if it's a map (platform specific for native plugins)
    if let Some(map) = plugin.download_url.as_object() {
        let platform_key = get_platform_key();

        if let Some(url) = map.get(platform_key).and_then(|v| v.as_str()) {
            return Ok(url.to_string());
        }

        // Direct package plugins may not have a repo, so provide a clearer error message.
        if plugin.repo.as_ref().is_none_or(|r| r.is_empty()) {
            return Err(TingError::plugin_load(
                PluginErrorKind::Loading,
                "get_download_url::plugin_load",
                format!(
                    "Plugin {} is not available for platform '{}'. This plugin uses direct package downloads with limited platform support.",
                    plugin.id, platform_key
                ),
            ));
        }

        return Err(TingError::plugin_load(
            PluginErrorKind::Loading,
            "get_download_url::plugin_load",
            format!(
                "No download URL found for platform '{}' for plugin {}",
                platform_key, plugin.id
            ),
        ));
    }

    Err(TingError::plugin_load(
        PluginErrorKind::InvalidOutput,
        "get_download_url::plugin_load",
        format!("Invalid download_url format for plugin {}", plugin.id),
    ))
}

/// Get the platform key for the current system
fn get_platform_key() -> &'static str {
    #[cfg(target_os = "windows")]
    return "windows-x86_64";

    #[cfg(target_os = "linux")]
    {
        #[cfg(target_arch = "aarch64")]
        return "linux-aarch64";

        #[cfg(not(target_arch = "aarch64"))]
        return "linux-x86_64";
    }

    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        return "macos-aarch64";

        #[cfg(not(target_arch = "aarch64"))]
        return "macos-x86_64";
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    "unknown"
}

/// Download a plugin to a temporary file
pub async fn download_plugin(
    client: &reqwest::Client,
    url: &str,
    temp_dir: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let mut current_url = Url::parse(url).map_err(|error| {
        let message = format!("Invalid absolute plugin download URL: {error}");
        TingError::network(
            NetworkErrorKind::InvalidResponse,
            "download_plugin::network",
            message,
        )
        .with_source(anyhow::anyhow!(error))
    })?;
    let mut redirect_count = 0_usize;
    let response = loop {
        let resolved = validate_plugin_download_target(&current_url).await?;
        // Bind the request to the addresses validated above. The shared client resolves the
        // host on its own and may reuse a pooled connection opened for an earlier answer, so
        // comparing its remote address against this lookup fails whenever the two DNS answers
        // differ (rotating CDN records, short TTLs).
        let hop_client = build_pinned_client(client, &current_url, &resolved).map_err(|e| {
            let message = format!("Failed to prepare plugin download client: {e}");
            TingError::network(
                NetworkErrorKind::Transport,
                "download_plugin::network",
                message,
            )
            .with_source(anyhow::anyhow!(e))
        })?;
        let response = hop_client
            .get(current_url.clone())
            .send()
            .await
            .map_err(|e| {
                let e = e.without_url();
                let message = format!("Failed to download plugin: {e}");
                TingError::network(NetworkErrorKind::Transport, "download_plugin", message)
                    .with_source(anyhow::anyhow!(e))
            })?;
        let remote = response.remote_addr().ok_or_else(|| {
            TingError::network(
                NetworkErrorKind::Transport,
                "download_plugin::network",
                "Plugin download did not expose its remote address".to_string(),
            )
        })?;
        if !resolved.iter().any(|address| address.ip() == remote.ip()) {
            let expected = resolved
                .iter()
                .map(|address| address.ip().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                url = %redacted_download_url(current_url.as_str()),
                connected = %remote.ip(),
                expected = %expected,
                "Plugin download connected to an unexpected address"
            );
            return Err(TingError::network(
                NetworkErrorKind::Transport,
                "download_plugin::network",
                format!(
                    "Plugin download connected to an unexpected address: {} (expected {})",
                    remote.ip(),
                    expected
                ),
            ));
        }

        if !response.status().is_redirection() {
            break response;
        }
        if response.url() != &current_url {
            return Err(TingError::network(
                NetworkErrorKind::Redirect,
                "download_plugin::network",
                "Plugin download client followed an unvalidated redirect".to_string(),
            ));
        }
        if redirect_count >= MAX_PLUGIN_DOWNLOAD_REDIRECTS {
            return Err(TingError::network(
                NetworkErrorKind::Redirect,
                "download_plugin::network",
                "Plugin download exceeded the redirect limit".to_string(),
            ));
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                TingError::network(
                    NetworkErrorKind::Redirect,
                    "download_plugin::network",
                    "Plugin download redirect omitted Location".to_string(),
                )
            })?;
        let next_url = current_url.join(location).map_err(|error| {
            let message = format!("Invalid plugin download redirect: {error}");
            TingError::network(
                NetworkErrorKind::Redirect,
                "download_plugin::network",
                message,
            )
            .with_source(anyhow::anyhow!(error))
        })?;
        redirect_count += 1;
        current_url = next_url;
    };

    if !response.status().is_success() {
        return Err(TingError::network(
            NetworkErrorKind::HttpStatus,
            "download_plugin::network",
            format!("Download returned status: {}", response.status()),
        ));
    }

    if response
        .content_length()
        .is_some_and(|length| length > MAX_PLUGIN_DOWNLOAD_BYTES)
    {
        return Err(TingError::network(
            NetworkErrorKind::Transport,
            "download_plugin::network",
            format!(
                "Plugin download exceeds {} bytes",
                MAX_PLUGIN_DOWNLOAD_BYTES
            ),
        ));
    }

    tokio::fs::create_dir_all(temp_dir)
        .await
        .map_err(TingError::IoError)?;
    let canonical_temp_dir = tokio::fs::canonicalize(temp_dir)
        .await
        .map_err(TingError::IoError)?;
    let temp_path = canonical_temp_dir.join(format!("plugin-{}.tr", Uuid::new_v4()));

    let write_result: Result<()> = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(TingError::IoError)?;
        let mut downloaded = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                let error = error.without_url();
                {
                    let message = format!("Failed to read download content: {}", error);
                    TingError::network(
                        NetworkErrorKind::Transport,
                        "download_plugin::network",
                        message,
                    )
                    .with_source(anyhow::anyhow!(error))
                }
            })?;
            downloaded = downloaded.checked_add(chunk.len() as u64).ok_or_else(|| {
                TingError::network(
                    NetworkErrorKind::Transport,
                    "download_plugin::network",
                    "Plugin download is too large".to_string(),
                )
            })?;
            if downloaded > MAX_PLUGIN_DOWNLOAD_BYTES {
                return Err(TingError::network(
                    NetworkErrorKind::Transport,
                    "download_plugin::network",
                    format!(
                        "Plugin download exceeds {} bytes",
                        MAX_PLUGIN_DOWNLOAD_BYTES
                    ),
                ));
            }
            file.write_all(&chunk).await.map_err(TingError::IoError)?;
        }
        file.flush().await.map_err(TingError::IoError)
    }
    .await;

    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }

    Ok(temp_path)
}

/// Build a client whose connections are restricted to `resolved`.
///
/// Hostnames are pinned with `resolve_to_addrs` so the request cannot connect to an address
/// that was never validated, and cannot reuse a pooled connection from an earlier lookup.
/// IP-literal URLs have nothing to resolve, so the shared client is reused as is.
fn build_pinned_client(
    base: &reqwest::Client,
    url: &Url,
    resolved: &[std::net::SocketAddr],
) -> std::result::Result<reqwest::Client, reqwest::Error> {
    let Some(host) = url.host_str() else {
        return Ok(base.clone());
    };
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized_host.parse::<IpAddr>().is_ok() {
        return Ok(base.clone());
    }
    reqwest::Client::builder()
        .user_agent("TingReader/1.0")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .no_proxy()
        .resolve_to_addrs(&normalized_host, resolved)
        .build()
}

async fn validate_plugin_download_target(url: &Url) -> Result<Vec<std::net::SocketAddr>> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(TingError::network(
            NetworkErrorKind::Transport,
            "validate_plugin_download_target::network",
            "Plugin downloads require HTTP or HTTPS URLs".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(TingError::network(
            NetworkErrorKind::Transport,
            "validate_plugin_download_target::network",
            "Plugin download URLs cannot contain credentials".to_string(),
        ));
    }

    let host = url.host_str().ok_or_else(|| {
        TingError::network(
            NetworkErrorKind::Transport,
            "validate_plugin_download_target::network",
            "Plugin download URL is missing a host".to_string(),
        )
    })?;
    let normalized_host = host.trim_end_matches('.').to_ascii_lowercase();
    let port = url.port_or_known_default().ok_or_else(|| {
        TingError::network(
            NetworkErrorKind::Transport,
            "validate_plugin_download_target::network",
            "Plugin download URL uses an unknown port".to_string(),
        )
    })?;
    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        return Ok(vec![std::net::SocketAddr::new(ip, port)]);
    }

    let resolved = tokio::net::lookup_host((normalized_host.as_str(), port))
        .await
        .map_err(|error| {
            let message = format!("Failed to resolve plugin download host: {error}");
            TingError::network(
                NetworkErrorKind::Transport,
                "validate_plugin_download_target::network",
                message,
            )
            .with_source(anyhow::anyhow!(error))
        })?
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return Err(TingError::network(
            NetworkErrorKind::Transport,
            "validate_plugin_download_target::network",
            "Plugin download host did not resolve to an address".to_string(),
        ));
    }
    Ok(resolved)
}

pub fn redacted_download_url(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return "<invalid-url>".to_string();
    };
    if parsed.host_str().is_none() {
        return "<invalid-url>".to_string();
    }
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return "<invalid-url>".to_string();
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

#[cfg(test)]
#[path = "../../tests/unit/plugin/store.rs"]
mod tests;

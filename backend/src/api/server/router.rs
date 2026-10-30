//! Route graph, application dependencies and middleware wiring.

use super::*;

impl ApiServer {
    /// Build the Axum router with all routes and middleware
    pub(super) fn build_router(
        config: Config,
        db: Arc<DatabaseManager>,
        plugin_manager: Arc<crate::plugin::manager::PluginManager>,
        config_manager: Arc<crate::plugin::config::PluginConfigManager>,
        encryption_key: [u8; 32],
    ) -> anyhow::Result<Router> {
        // Create API key configuration for authentication
        let api_key = ApiKey::new(config.security.enable_auth, config.security.api_key.clone());

        // Create security headers configuration
        let security_headers_config =
            SecurityHeadersConfig::new(config.security.enable_hsts, config.security.hsts_max_age);

        // Create repositories
        let book_repo = Arc::new(BookRepository::new(db.clone()));
        let user_repo = Arc::new(crate::db::repository::UserRepository::new(db.clone()));
        let progress_repo = Arc::new(crate::db::repository::ProgressRepository::new(db.clone()));
        let favorite_repo = Arc::new(crate::db::repository::FavoriteRepository::new(db.clone()));

        // Keep rolling activity and long-hidden progress bounded even when playback is idle.
        let progress_cleanup_repo = progress_repo.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            loop {
                interval.tick().await;
                match progress_cleanup_repo.cleanup_expired_records().await {
                    Ok(removed) if removed > 0 => {
                        tracing::info!(
                            removed,
                            message_key = "progress.activity_cleanup.completed",
                            "Expired playback records removed"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            message_key = "progress.activity_cleanup.failed",
                            message_params = %serde_json::json!({ "error": error.to_string() }),
                            "Expired playback record cleanup failed"
                        );
                    }
                }
            }
        });

        let settings_repo = Arc::new(crate::db::repository::UserSettingsRepository::new(
            db.clone(),
        ));
        let system_settings_repo = Arc::new(crate::db::repository::SystemSettingsRepository::new(
            db.clone(),
        ));
        let library_repo = Arc::new(crate::db::repository::LibraryRepository::new(db.clone()));
        let chapter_repo = Arc::new(crate::db::repository::ChapterRepository::new(db.clone()));
        let series_repo = Arc::new(crate::db::repository::SeriesRepository::new(db.clone()));
        let playlist_repo = Arc::new(crate::db::repository::PlaylistRepository::new(db.clone()));
        let notification_repo = Arc::new(
            crate::db::repository::NotificationWebhookRepository::new(db.clone()),
        );

        // Initialize JWT key manager (auto-generates and rotates keys)
        let jwt_key_manager = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                match crate::auth::JwtKeyManager::new(db.get_pool(), encryption_key).await {
                    Ok(manager) => {
                        tracing::info!(
                            message_key = "system.jwt_key.initialized",
                            "JWT key manager initialized"
                        );
                        let manager_arc = Arc::new(manager);
                        // 启动后台轮换任务
                        manager_arc.clone().start_rotation_task();
                        Some(manager_arc)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            message_key = "system.jwt_key.init_failed",
                            message_params = %serde_json::json!({ "error": e.to_string() }),
                            "JWT key manager initialization failed; using configured secret"
                        );
                        None
                    }
                }
            })
        });

        // Get JWT secret from config (fallback)
        let jwt_secret = Arc::new(config.security.jwt_secret.clone());

        // Keep API state and plugin lifecycle on the same config manager instance.
        plugin_manager.set_config_manager(config_manager.clone());

        // Create services
        let book_service = Arc::new(crate::core::books::BookService::new(book_repo.clone()));
        let scraper_service = Arc::new(crate::core::books::ScraperService::new(
            plugin_manager.clone(),
        ));
        let merge_service = Arc::new(crate::core::books::merge_service::MergeService::new(
            book_repo.clone(),
            chapter_repo.clone(),
        ));

        // Create helpers
        let cleaner_config = crate::core::books::text_cleaner::CleanerConfig::default();
        let text_cleaner = Arc::new(crate::core::books::text_cleaner::TextCleaner::new(
            cleaner_config,
        ));

        let nfo_manager = Arc::new(crate::core::books::nfo_manager::NfoManager::new(
            config.storage.data_dir.clone(),
        ));

        // Create audio streamer with configuration
        let streamer_config = crate::core::audio::StreamerConfig {
            cache_enabled: config.audio.cache_enabled,
            cache_size: config.audio.cache_size,
            buffer_size: config.audio.buffer_size,
            supported_formats: vec![
                crate::core::audio::AudioFormat::Mp3,
                crate::core::audio::AudioFormat::M4a,
                crate::core::audio::AudioFormat::Aac,
                crate::core::audio::AudioFormat::Flac,
                crate::core::audio::AudioFormat::Wma,
            ],
        };
        let audio_streamer = Arc::new(crate::core::audio::AudioStreamer::new(streamer_config));

        // Create StorageService
        let storage_service = Arc::new(crate::core::StorageService::new());

        // Create Preload Cache
        let preload_cache = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let preload_cache_for_expiration = Arc::downgrade(&preload_cache);
        let preload_settings = system_settings_repo.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let Some(cache_lock) = preload_cache_for_expiration.upgrade() else {
                    break;
                };
                let policy = crate::core::maintenance::load_settings(&preload_settings)
                    .await
                    .unwrap_or_default();
                let mut cache = cache_lock.write().await;
                crate::api::handlers::media::stream::preload::evict_expired_cache_with(
                    &mut cache,
                    std::time::Instant::now(),
                    std::time::Duration::from_secs(policy.preload_idle_seconds),
                );
            }
        });

        // Create task queue
        let task_queue = Arc::new(
            crate::core::task_queue::TaskQueue::new(
                config.task_queue.clone(),
                db.clone(),
                config.storage.temp_dir.clone(),
            )
            .with_repositories(book_repo.clone(), chapter_repo.clone(), series_repo.clone())
            .with_library_repo(library_repo.clone())
            .with_scraper_service(scraper_service.clone())
            .with_text_cleaner(text_cleaner.clone())
            .with_nfo_manager(nfo_manager.clone())
            .with_audio_streamer(audio_streamer.clone())
            .with_plugin_manager(plugin_manager.clone())
            .with_storage_service(storage_service.clone())
            .with_merge_service(merge_service.clone())
            .with_notification_repo(notification_repo.clone())
            .with_encryption_key(Arc::new(encryption_key)),
        );

        // Start task queue executor
        let task_queue_clone = task_queue.clone();
        tokio::spawn(async move {
            if let Err(e) = task_queue_clone.recover_tasks().await {
                tracing::error!(
                    error = %e,
                    message_key = "task.recovery.failed",
                    message_params = %serde_json::json!({ "error": e.to_string() }),
                    "Task recovery failed"
                );
            }
            task_queue_clone.start().await;
        });

        let sync_scheduler = Arc::new(
            crate::core::library_scanner::scheduler::LibrarySyncScheduler::new(
                library_repo.clone(),
                task_queue.clone(),
            ),
        );
        tokio::spawn(sync_scheduler.start());

        // Wrap config in Arc<RwLock> for shared mutable access
        let config_arc = Arc::new(tokio::sync::RwLock::new(config.clone()));

        // Create cache manager
        let cache_manager = Arc::new(
            crate::cache::CacheManager::new(config.storage.temp_dir.clone())
                .map_err(|e| anyhow::anyhow!("Failed to create cache manager: {}", e))?,
        );
        let plugin_cache = Arc::new(
            crate::plugin::PluginCache::new(config.storage.data_dir.join("plugin-cache"))
                .map_err(|e| anyhow::anyhow!("Failed to create plugin cache: {}", e))?,
        );
        let plugin_route_revocations = Arc::new(
            crate::core::security::signing::PluginRouteRevocations::new(
                config
                    .storage
                    .data_dir
                    .join("plugin-route-revocations.json"),
            )
            .map_err(|e| anyhow::anyhow!("Failed to create plugin route revocations: {}", e))?,
        );
        let plugin_host_gateway = Arc::new(crate::plugin::PluginHostGateway::new(
            crate::plugin::PluginHostGatewayDependencies {
                book_repo: book_repo.clone(),
                library_repo: library_repo.clone(),
                chapter_repo: chapter_repo.clone(),
                progress_repo: progress_repo.clone(),
                playlist_repo: playlist_repo.clone(),
                favorite_repo: favorite_repo.clone(),
                settings_repo: settings_repo.clone(),
                task_queue: task_queue.clone(),
                plugin_manager: plugin_manager.clone(),
                plugin_cache: plugin_cache.clone(),
                plugin_storage: Arc::new(
                    crate::plugin::PluginCache::new(config.storage.data_dir.join("plugin-storage"))
                        .map_err(|e| anyhow::anyhow!("Failed to create plugin storage: {}", e))?,
                ),
                config_manager: config_manager.clone(),
                plugin_route_revocations: plugin_route_revocations.clone(),
                encryption_key: Arc::new(encryption_key),
                config: config.clone(),
            },
        ));
        plugin_manager.set_host_gateway(&plugin_host_gateway);

        // Create library watcher
        let library_watcher = Arc::new(crate::core::library_scanner::watcher::LibraryWatcher::new(
            library_repo.clone(),
            Arc::new(crate::db::repository::LibraryScanStateRepository::new(
                db.clone(),
            )),
            task_queue.clone(),
            config.clone(),
        ));

        // Start watching all local libraries
        let watcher_clone = library_watcher.clone();
        tokio::spawn(async move {
            if let Err(e) = watcher_clone.start_all().await {
                tracing::warn!(
                    error = %e,
                    message_key = "library.watcher.start_failed",
                    message_params = %serde_json::json!({ "error": e.to_string() }),
                    "Library watcher failed to start"
                );
            }
        });

        // Create WebSocket session manager
        let ws_manager = crate::api::ws::manager::WsSessionManager::new();

        // Create HLS session manager
        let hls_temp_dir = config.storage.temp_dir.join("ting_hls_sessions");
        std::fs::create_dir_all(&hls_temp_dir)
            .map_err(|e| anyhow::anyhow!("Failed to create HLS temp directory: {}", e))?;
        let hls_session_manager = Arc::new(
            crate::api::handlers::media::stream::HlsSessionManager::new(hls_temp_dir),
        );

        // Explicit player teardown is backed by frequent idle-session cleanup.
        let hls_manager_clone = hls_session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                hls_manager_clone.cleanup_expired().await;
            }
        });

        // Create application state
        let app_state = AppState {
            book_repo,
            user_repo,
            progress_repo,
            favorite_repo,
            settings_repo,
            system_settings_repo,
            library_repo,
            chapter_repo,
            series_repo,
            playlist_repo,
            notification_repo,
            book_service,
            scraper_service,
            plugin_manager,
            plugin_cache,
            plugin_host_gateway,
            plugin_route_revocations,
            config_manager,
            task_queue,
            config: config_arc,
            jwt_secret,
            jwt_key_manager, // 新增密钥管理器
            cache_manager,
            encryption_key: Arc::new(encryption_key),
            storage_service,
            preload_cache,
            audio_streamer,
            merge_service,
            nfo_manager,
            active_preload_tasks: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            library_watcher,
            ws_manager,
            hls_session_manager,
        };

        // Authentication has its own budget; media and static requests do not.
        let auth_router = Router::new()
            .route(
                "/api/auth/login",
                axum::routing::post(crate::auth::handlers::login),
            )
            .route(
                "/api/auth/token-login",
                axum::routing::post(crate::auth::handlers::token_login),
            )
            .route(
                "/api/auth/session-restore",
                axum::routing::post(crate::auth::handlers::session_restore),
            )
            .route(
                "/api/v1/auth/token-login",
                axum::routing::post(crate::auth::handlers::token_login),
            )
            .route(
                "/api/v1/auth/session-restore",
                axum::routing::post(crate::auth::handlers::session_restore),
            )
            .route(
                "/api/auth/register",
                axum::routing::post(crate::auth::handlers::register),
            );
        let auth_router = if config.security.rate_limit_enabled {
            auth_router.layer(
                ServiceBuilder::new()
                    .layer(axum::Extension(RateLimiter::from_config(&config.security)))
                    .layer(middleware::from_fn(rate_limit_middleware)),
            )
        } else {
            auth_router
        };
        // Create public routes (no authentication required)
        let public_router = Router::new()
            .merge(auth_router)
            .route("/health", get(health_check))
            // WebSocket endpoint — handles auth internally via query param token
            .route("/api/ws", get(crate::api::ws::handler::ws_handler))
            .route("/api/v1/ws", get(crate::api::ws::handler::ws_handler))
            .with_state(app_state.clone());

        // Create protected routes (authentication required)
        crate::api::handlers::system::maintenance::start_services(&app_state);
        let protected_router = build_api_routes(app_state.clone()).layer(middleware::from_fn(
            move |mut req: Request, next: Next| {
                let api_key = api_key.clone();
                async move {
                    // Inject API key into request extensions
                    req.extensions_mut().insert(api_key);
                    // Call auth middleware
                    auth_middleware(req, next).await
                }
            },
        ));

        // Combine public and protected routes
        let api_router = Router::new().merge(public_router).merge(protected_router);

        // Static file serving for SPA
        let static_dir = std::env::var("STATIC_DIR").unwrap_or_else(|_| "static".to_string());
        let index_path = std::path::PathBuf::from(&static_dir).join("index.html");
        let manifest_path = std::path::PathBuf::from(&static_dir).join("manifest.webmanifest");
        let serve_manifest = ServeFile::new(manifest_path);
        let serve_dir = ServeDir::new(&static_dir).fallback(ServeFile::new(index_path));

        // Apply global middleware layers
        let router = api_router
            .route_service("/manifest.webmanifest", serve_manifest)
            .fallback_service(serve_dir);
        let router = router.layer(middleware::from_fn(ensure_manifest_link));
        let router = router.layer(
            ServiceBuilder::new()
                // Add security headers middleware
                .layer(middleware::from_fn(move |mut req: Request, next: Next| {
                    let config = security_headers_config.clone();
                    async move {
                        req.extensions_mut().insert(config);
                        security_headers_middleware(req, next).await
                    }
                }))
                // Add trace ID middleware for request tracking
                .layer(middleware::from_fn(trace_id_middleware))
                // Add tracing for all requests
                .layer(TraceLayer::new_for_http().on_failure(
                    |classification: ServerErrorsFailureClass,
                     latency: Duration,
                     span: &tracing::Span| {
                        let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
                        match classification {
                            ServerErrorsFailureClass::StatusCode(status_code) => {
                                tracing::debug!(
                                    parent: span,
                                    status_code = status_code.as_u16(),
                                    latency_ms = latency_ms,
                                    message_key = "http.response.status_failed",
                                    message_params = %serde_json::json!({
                                        "status_code": status_code.as_u16(),
                                        "latency_ms": latency_ms,
                                    }),
                                    "HTTP response returned failure status"
                                );
                            }
                            ServerErrorsFailureClass::Error(error) => {
                                tracing::error!(
                                    parent: span,
                                    error = %error,
                                    latency_ms = latency_ms,
                                    message_key = "http.response.service_failed",
                                    message_params = %serde_json::json!({
                                        "latency_ms": latency_ms,
                                    }),
                                    "HTTP service failed"
                                );
                            }
                        }
                    },
                ))
                // Add CORS support
                .layer(Self::build_cors_layer(&config.security.allowed_origins)),
        );

        // The same route tree serves both the direct TCP endpoint and the
        // fnOS gateway endpoint. The gateway forwards the registered prefix
        // to this router, so the existing direct URLs remain unchanged.
        let router = if let Some(prefix) =
            normalize_gateway_prefix(config.server.gateway_prefix.as_deref())
        {
            let gateway_state = GatewayProxyState {
                router: router.clone(),
                prefix: prefix.clone(),
            };
            let gateway_path = prefix.clone();
            let gateway_wildcard_path = format!("{prefix}/*path");

            Router::new()
                .route(&gateway_path, any(gateway_proxy))
                .route(&gateway_wildcard_path, any(gateway_proxy))
                .with_state(gateway_state)
                .merge(router)
        } else {
            router
        };

        Ok(router)
    }

    /// Build CORS layer from allowed origins configuration
    fn build_cors_layer(allowed_origins: &[String]) -> CorsLayer {
        use tower_http::cors::Any;

        let cors = CorsLayer::new();

        // If allowed_origins contains "*", allow any origin
        if allowed_origins.contains(&"*".to_string()) {
            cors.allow_origin(Any).allow_methods(Any).allow_headers(Any)
        } else {
            // Parse allowed origins
            let origins: Vec<_> = allowed_origins
                .iter()
                .filter_map(|origin| origin.parse().ok())
                .collect();

            cors.allow_origin(origins)
                .allow_methods(Any)
                .allow_headers(Any)
        }
    }
}

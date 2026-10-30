//! Core business logic module
//!
//! This module provides the core application layer including:
//! - Business logic services
//! - Task queue and scheduling
//! - Event bus for pub/sub messaging
//! - Configuration management
//! - Structured logging system
//! - Error handling and type system
//! - Text cleaning and normalization
//! - Audio streaming and metadata reading

pub mod app;
pub mod audio;
pub mod books;
pub mod common;
pub mod event_bus;
pub mod library_scanner;
pub mod maintenance;
pub mod notifications;
pub mod security;
pub mod storage;
pub mod task_queue;

pub use app::{Config, ErrorContext, ErrorResponse, Logger, Result, TingError};
pub use audio::{AudioFormat, AudioMetadata, AudioService, AudioStreamer, StreamerConfig};
pub use books::{
    BookMetadata, BookService, ChapterMetadata, CleanerConfig, CleaningResult, CleaningRule,
    MergeService, NfoManager, ScraperService, TextCleaner,
};
pub use common::{LruCache, release_memory};
pub use event_bus::{Event, EventBus, EventType};
pub use library_scanner::{LibraryScanner, ScanResult};
pub use security::{CacheStats, DecryptionCacheConfig, DecryptionCacheService};
pub use storage::{StorageService, WebDavClient};
pub use task_queue::{Task, TaskQueue, TaskStatus};

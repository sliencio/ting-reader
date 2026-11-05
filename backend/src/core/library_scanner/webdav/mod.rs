use crate::core::app::error::ConfigurationErrorKind;
mod chapters;
mod extraction;
mod listing;
mod processing;

use super::{LibraryScanner, ScanMode, ScanResult, ScanStatus};
use crate::core::app::error::{Result, TingError};
use crate::core::library_scanner::shared::{
    ChapterRangeDir, CoalescedRangeDirs, SeriesDirectoryCandidate, infer_series_directories,
    parse_chapter_range_dir_name, select_mergeable_range_groups,
};
use crate::db::repository::Repository;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

type WebDavFileEntry = (
    String,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
);

impl LibraryScanner {
    /// Scan a WebDAV library
    pub(crate) async fn scan_webdav_library(
        &self,
        library: &crate::db::models::Library,
        task_id: Option<&str>,
        scraper_config: &crate::db::models::ScraperConfig,
        mode: ScanMode,
    ) -> Result<ScanResult> {
        if self.storage_service.is_none() {
            return Err(TingError::configuration(
                ConfigurationErrorKind::Encoding,
                "scan_webdav_library::configuration",
                "Storage service not configured for WebDAV scan".to_string(),
            ));
        }

        let mut scan_result = ScanResult {
            start_time: Some(std::time::Instant::now()),
            ..Default::default()
        };
        self.update_progress_key(task_id, "scan.webdav.scanning", serde_json::json!({}))
            .await;

        // 1. List files recursively
        let files = self.list_webdav_files(library, task_id, mode).await?;

        let supported_extensions = self.get_supported_extensions().await;

        // Group by directory URL (parent URL)
        // Key: Parent URL (String), Value: List of (File URL, Last Modified)
        let mut dir_groups: HashMap<String, Vec<WebDavFileEntry>> = HashMap::new();

        // Metadata/sidecar file extensions that should be grouped alongside audio files
        // so that cover images, metadata.json, book.nfo etc. are available during processing.
        const METADATA_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "json", "nfo"];

        for (file_url, last_mod, validator) in files {
            // Check extension
            if let Some(ext_pos) = file_url.rfind('.') {
                let ext = file_url[ext_pos + 1..].to_lowercase();
                if supported_extensions.contains(&ext)
                    || METADATA_EXTENSIONS.contains(&ext.as_str())
                {
                    // Get parent URL
                    if let Some(last_slash) = file_url.rfind('/') {
                        let parent = file_url[0..last_slash].to_string();
                        dir_groups
                            .entry(parent)
                            .or_default()
                            .push((file_url, last_mod, validator));
                    }
                }
            }
        }

        self.update_progress_key(
            task_id,
            "scan.audio_dirs.found",
            serde_json::json!({ "count": dir_groups.len() }),
        )
        .await;

        let (dir_groups, coalesced_range_dirs) =
            self.coalesce_webdav_range_directory_groups(dir_groups);
        let inferred_series = if scraper_config.auto_generate_series {
            self.infer_webdav_series_directories(dir_groups.keys())
        } else {
            HashMap::new()
        };
        let total_groups = dir_groups.len();
        let mut processed_count = 0;

        // Pre-fetch all books for lookup and deletion handling
        let prefetched = self.prefetch_books(&library.id).await;
        let chapter_counts = if mode.is_full() {
            HashMap::new()
        } else {
            self.chapter_repo
                .count_by_library(&library.id)
                .await
                .unwrap_or_default()
        };
        let cached_book_states = self
            .scan_state_repo
            .find_by_library_kind(&library.id, "webdav_book")
            .await
            .unwrap_or_default();
        let scan_config_fingerprint = webdav_scan_config_fingerprint(scraper_config);

        let mut book_path_map: HashMap<String, (String, i32, Option<String>)> = HashMap::new();
        let mut book_hash_map: HashMap<String, (String, i32, Option<String>)> = HashMap::new();
        for (id, path, hash, manual_corrected, match_pattern) in &prefetched.all_books {
            book_path_map.insert(
                path.clone(),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
            book_hash_map.insert(
                hash.clone(),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
        }

        let mut found_book_ids: HashSet<String> = HashSet::new();
        let mut live_book_state_paths: HashSet<String> = HashSet::new();
        let mut absorbed_range_book_ids: HashMap<String, String> = HashMap::new();
        let mut pending_book_states = Vec::new();
        let last_scanned = if mode.is_full() {
            None
        } else if let Some(ref date_str) = library.last_scanned_at {
            chrono::DateTime::parse_from_rfc3339(date_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok()
        } else {
            None
        };

        for (dir_url, file_entries) in dir_groups {
            processed_count += 1;
            let report_progress = processed_count == 1
                || processed_count % 16 == 0
                || processed_count == total_groups;
            if report_progress {
                self.check_cancellation(task_id).await?;
            }
            // Extract directory name from URL for logging
            let dir_name = self.webdav_url_name(&dir_url);

            if report_progress {
                self.update_progress_key(
                    task_id,
                    "scan.item.processing",
                    serde_json::json!({
                        "current": processed_count,
                        "total": total_groups,
                        "remaining": total_groups.saturating_sub(processed_count),
                        "name": dir_name,
                    }),
                )
                .await;
            }

            // Extract just URLs for processing
            let mut file_urls: Vec<String> = Vec::new();
            let mut metadata_files: Vec<String> = Vec::new();

            for (url, _, _) in file_entries.iter() {
                let ext = url
                    .split('.')
                    .next_back()
                    .unwrap_or_default()
                    .to_lowercase();
                if ["json", "nfo", "jpg", "png", "jpeg", "webp"].contains(&ext.as_str()) {
                    metadata_files.push(url.clone());
                } else {
                    file_urls.push(url.clone());
                }
            }

            // Skip directories with no audio files (only metadata/sidecar files)
            if file_urls.is_empty() {
                continue;
            }
            live_book_state_paths.insert(dir_url.clone());

            // Calculate directory hash for lookup
            let mut hasher = Sha256::new();
            hasher.update(dir_url.as_bytes());
            let dir_hash = format!("{:x}", hasher.finalize());

            // Optimization: Find existing book to avoid DB lookup
            let mut existing_info = book_path_map.get(&dir_url).cloned();
            if existing_info.is_none() {
                existing_info = book_hash_map.get(&dir_hash).cloned();
            }
            if existing_info.is_none()
                && let Some(child_dirs) = coalesced_range_dirs.get(&dir_url)
            {
                for child_dir in &child_dirs.child_dirs {
                    if let Some(info) = book_path_map.get(child_dir).cloned() {
                        existing_info = Some(info);
                        break;
                    }
                }
            }

            let existing_lock_state = existing_info
                .as_ref()
                .map(|(_, manual_corrected, _)| *manual_corrected)
                .unwrap_or(0);
            let state_config_fingerprint =
                format!("{}:lock={}", scan_config_fingerprint, existing_lock_state);
            let directory_fingerprint = webdav_directory_fingerprint(&file_entries);
            let cache_dir = crate::core::books::metadata::remote_metadata_dir(&dir_url)?;
            let cache_complete = cache_dir.join("metadata.json").is_file()
                && cache_dir.join("book.nfo").is_file()
                && metadata_files
                    .iter()
                    .find(|url| {
                        [
                            "cover.jpg",
                            "cover.jpeg",
                            "cover.png",
                            "cover.webp",
                            "folder.jpg",
                        ]
                        .iter()
                        .any(|filename| url.to_lowercase().ends_with(filename))
                    })
                    .is_none_or(|url| {
                        let ext = url.rsplit('.').next().unwrap_or("jpg");
                        cache_dir.join(format!("cover.{ext}")).is_file()
                    });
            if let (Some((book_id, _, _)), Some(directory_fingerprint)) =
                (existing_info.as_ref(), directory_fingerprint.as_ref())
                && !mode.is_full()
                && cache_complete
                && cached_book_states.get(&dir_url).is_some_and(|state| {
                    state.fingerprint == *directory_fingerprint
                        && state.config_fingerprint.as_deref()
                            == Some(state_config_fingerprint.as_str())
                })
            {
                scan_result.total_books += 1;
                scan_result.books_skipped += 1;
                found_book_ids.insert(book_id.clone());
                continue;
            }

            // Natural ordering is needed only when this book will enter title
            // checks or chapter reconciliation. Cache hits above stay linear.
            file_urls.sort_by_cached_key(|path| {
                crate::core::books::text_cleaner::chapter_order_key(path)
            });
            metadata_files.sort_by(|a, b| natord::compare(a, b));

            // Incremental Check: Skip if book exists and no files modified since last scan
            if let (Some((id, _, _)), Some(last_scan_time)) = (&existing_info, last_scanned)
                && cache_complete
                && cached_book_states.get(&dir_url).is_some_and(|state| {
                    state.config_fingerprint.as_deref() == Some(state_config_fingerprint.as_str())
                })
            {
                // Check if file count changed (new files added or removed)
                let current_file_count = file_urls.len();
                let existing_chapter_count = chapter_counts
                    .get(id)
                    .map(|counts| counts.total)
                    .unwrap_or_default();

                // Determine latest modification time in this directory
                let max_mtime = file_entries.iter().filter_map(|(_, mtime, _)| *mtime).max();

                let mtime_count = file_entries
                    .iter()
                    .filter(|(_, mtime, _)| mtime.is_some())
                    .count();

                info!(
                    book_id = %id,
                    dir = %dir_name,
                    total_files = file_entries.len(),
                    files_with_mtime = mtime_count,
                    current_file_count = current_file_count,
                    existing_chapter_count = existing_chapter_count,
                    "WebDAV 扫描：「{dir_name}」已有记录，核对是否需要更新（现有 {existing_chapter_count} 集，磁盘上 {current_file_count} 集）"
                );

                // Skip only if:
                // 1. File count hasn't changed AND
                // 2. No files have been modified since last scan
                if current_file_count == existing_chapter_count {
                    if let Some(latest) = max_mtime {
                        if latest <= last_scan_time {
                            // Only load chapter rows when the cheap timestamp/count
                            // checks say the book is otherwise unchanged and a
                            // title-rule recheck may actually be needed.
                            let existing_chapters =
                                self.chapter_repo.find_by_book(id).await.unwrap_or_default();
                            let should_reprocess_chapter_titles = self
                                .book_repo
                                .find_by_id(id)
                                .await?
                                .as_ref()
                                .map(|book| {
                                    self.webdav_chapter_title_rules_need_reprocess(
                                        book,
                                        &existing_chapters,
                                        &file_urls,
                                        scraper_config,
                                    )
                                })
                                .unwrap_or(false);

                            if should_reprocess_chapter_titles {
                                info!(
                                    book_id = %id,
                                    dir = %dir_name,
                                    "WebDAV 扫描：「{dir_name}」内容没变，但章节命名规则改了，重新处理章节"
                                );
                            } else {
                                // Book exists and is up to date
                                scan_result.total_books += 1;
                                scan_result.books_skipped += 1;
                                found_book_ids.insert(id.clone());
                                if let Some(directory_fingerprint) = directory_fingerprint.as_ref()
                                {
                                    let mut state = crate::db::repository::LibraryScanState::new(
                                        &library.id,
                                        &dir_url,
                                        "webdav_book",
                                        directory_fingerprint,
                                    );
                                    state.config_fingerprint =
                                        Some(state_config_fingerprint.clone());
                                    pending_book_states.push(state);
                                }
                                info!(
                                    book_id = %id,
                                    dir = %dir_name,
                                    "WebDAV 扫描：「{dir_name}」内容未变，跳过"
                                );
                                continue;
                            }
                        } else {
                            info!(
                                book_id = %id,
                                dir = %dir_name,
                                "WebDAV 扫描：「{dir_name}」有文件比上次扫描更新，重新处理"
                            );
                        }
                    } else {
                        // No modification times available, force update
                        info!(
                            book_id = %id,
                            dir = %dir_name,
                            "WebDAV 扫描：「{dir_name}」拿不到修改时间，按需重新处理"
                        );
                    }
                } else {
                    info!(
                        book_id = %id,
                        dir = %dir_name,
                        "WebDAV 扫描：「{dir_name}」集数变化（{existing_chapter_count} → {current_file_count}），重新处理"
                    );
                }
            }

            match self
                .process_webdav_book(processing::WebDavBookContext {
                    library,
                    dir_url: &dir_url,
                    file_urls: &file_urls,
                    metadata_files: &metadata_files,
                    scraper_config,
                    full_scan: mode.is_full(),
                    existing_info,
                    fallback_title_override: coalesced_range_dirs
                        .get(&dir_url)
                        .and_then(|range_dirs| range_dirs.title_override.as_deref()),
                })
                .await
            {
                Ok((book_id, status)) => {
                    scan_result.total_books += 1;
                    match status {
                        ScanStatus::Created => scan_result.books_created += 1,
                        ScanStatus::Updated => scan_result.books_updated += 1,
                        ScanStatus::Skipped => scan_result.books_skipped += 1,
                    }
                    if status != ScanStatus::Skipped {
                        scan_result.changed_book_ids.insert(book_id.clone());
                    }
                    found_book_ids.insert(book_id.clone());
                    if let Some(directory_fingerprint) = directory_fingerprint {
                        let mut state = crate::db::repository::LibraryScanState::new(
                            &library.id,
                            &dir_url,
                            "webdav_book",
                            directory_fingerprint,
                        );
                        state.config_fingerprint = Some(state_config_fingerprint);
                        pending_book_states.push(state);
                    }
                    if status != ScanStatus::Skipped
                        && let Some(series_info) = inferred_series.get(&dir_url)
                        && let Err(e) = self
                            .link_book_to_inferred_series(&library.id, &book_id, series_info)
                            .await
                    {
                        warn!(
                            url = %dir_url,
                            book_id = %book_id,
                            error = %e,
                            "Failed to link WebDAV book to inferred series"
                        );
                    }
                    if status != ScanStatus::Skipped
                        && let Some(child_dirs) = coalesced_range_dirs.get(&dir_url)
                    {
                        for child_dir in &child_dirs.child_dirs {
                            if let Some((child_book_id, manual_corrected, _)) =
                                book_path_map.get(child_dir)
                                && child_book_id != &book_id
                                && *manual_corrected == 0
                            {
                                absorbed_range_book_ids
                                    .insert(child_book_id.clone(), book_id.clone());
                            }
                        }
                    }
                    let status_label = match status {
                        ScanStatus::Created => "新增书籍",
                        ScanStatus::Updated => "更新书籍",
                        ScanStatus::Skipped => "内容未变，跳过",
                    };
                    info!(
                        book_id = %book_id,
                        dir = %dir_name,
                        status = status_label,
                        chapters = file_urls.len(),
                        "WebDAV 扫描：{status_label}「{dir_name}」（{} 集）",
                        file_urls.len()
                    );
                }
                Err(e) => {
                    scan_result.failed_count += 1;
                    warn!(
                        dir = %dir_name,
                        url = %dir_url,
                        error = %e,
                        "WebDAV 扫描：处理「{dir_name}」失败"
                    );
                    scan_result
                        .errors
                        .push(format!("Failed to process {}: {}", dir_url, e));
                }
            }

            if processed_count % 25 == 0 {
                self.plugin_manager.garbage_collect_all().await;
            }
        }

        if let Some(merge_service) = &self.merge_service {
            for (source_id, target_id) in absorbed_range_book_ids {
                if found_book_ids.contains(&source_id) {
                    continue;
                }

                if let Err(e) = merge_service
                    .absorb_scanned_book(&target_id, &source_id)
                    .await
                {
                    warn!(
                        "Failed to absorb WebDAV range-segment book {} into {}: {}",
                        source_id, target_id, e
                    );
                } else {
                    scan_result.books_deleted += 1;
                    found_book_ids.insert(source_id);
                }
            }
        }

        // The listing helper returns only after it has reconstructed an
        // authoritative complete view (from sync deltas, cached ETag subtrees,
        // or a full traversal). Deletions are therefore safe in either mode.
        self.handle_book_deletions(&mut scan_result, &prefetched, &found_book_ids, false)
            .await;
        let deleted_book_states = cached_book_states
            .keys()
            .filter(|path| !live_book_state_paths.contains(*path))
            .cloned()
            .collect();
        self.scan_state_repo
            .delete_many(&library.id, "webdav_book", deleted_book_states)
            .await?;
        self.scan_state_repo
            .upsert_many(pending_book_states)
            .await?;

        // Final garbage collection after scan
        self.plugin_manager.garbage_collect_all().await;

        Ok(scan_result)
    }

    fn webdav_chapter_title_rules_need_reprocess(
        &self,
        book: &crate::db::models::Book,
        existing_chapters: &[crate::db::models::Chapter],
        file_urls: &[String],
        scraper_config: &crate::db::models::ScraperConfig,
    ) -> bool {
        let chapter_regex = book
            .chapter_regex
            .as_ref()
            .filter(|pattern| !pattern.trim().is_empty())
            .and_then(|pattern| regex::Regex::new(pattern).ok());

        let filename_titles_first = scraper_config.metadata_rank("filename") == 0;
        if chapter_regex.is_none() && !filename_titles_first {
            return existing_chapters.iter().any(|chapter| {
                if chapter.manual_corrected != 0 {
                    return false;
                }

                let Some(title) = chapter.title.as_deref() else {
                    return false;
                };

                let (_, detected_as_extra) = self
                    .text_cleaner
                    .clean_chapter_title(title, book.title.as_deref());
                let is_extra = scraper_config.extract_extra_chapters && detected_as_extra;
                chapter.is_extra != if is_extra { 1 } else { 0 }
            });
        }

        let mut chapter_by_hash = HashMap::new();
        let mut chapter_by_path = HashMap::new();
        for chapter in existing_chapters {
            if let Some(hash) = &chapter.hash {
                chapter_by_hash.insert(hash.as_str(), chapter);
            }
            chapter_by_path.insert(chapter.path.as_str(), chapter);
        }

        let mut main_counter = 0;
        let mut extra_counter = 0;

        for file_url in file_urls {
            let mut hasher = Sha256::new();
            hasher.update(file_url.as_bytes());
            let chapter_hash = format!("{:x}", hasher.finalize());

            let Some(chapter) = chapter_by_hash
                .get(chapter_hash.as_str())
                .or_else(|| chapter_by_path.get(file_url.as_str()))
            else {
                return true;
            };

            let decoded_file_url = self.decode_url_path(file_url);
            let filename = decoded_file_url
                .split('/')
                .next_back()
                .unwrap_or("chapter")
                .to_string();

            let mut regex_idx = None;
            let mut regex_title = None;
            if let Some(re) = &chapter_regex
                && let Some(caps) = re.captures(&filename)
            {
                if let Some(m) = caps.get(1)
                    && let Ok(idx) = m.as_str().parse::<i32>()
                {
                    regex_idx = Some(idx);
                }
                if let Some(m) = caps.get(2) {
                    regex_title = Some(m.as_str().to_string());
                }
            }

            let title_override = if let Some(rt) = regex_title {
                let (cleaned, is_extra) = self
                    .text_cleaner
                    .clean_chapter_title(&rt, book.title.as_deref());
                Some((cleaned, is_extra))
            } else if filename_titles_first {
                let (cleaned, is_extra) = self
                    .text_cleaner
                    .clean_chapter_title(&filename, book.title.as_deref());
                Some((cleaned, is_extra))
            } else {
                None
            };

            let counter_is_extra = if chapter.manual_corrected != 0 {
                chapter.is_extra == 1
            } else {
                scraper_config.extract_extra_chapters
                    && title_override
                        .as_ref()
                        .map(|(_, is_extra)| *is_extra)
                        .unwrap_or(chapter.is_extra == 1)
            };
            let counter_idx = if counter_is_extra {
                extra_counter += 1;
                extra_counter
            } else {
                main_counter += 1;
                main_counter
            };
            let target_idx = regex_idx.unwrap_or(counter_idx);

            if chapter.manual_corrected != 0 {
                continue;
            }

            if !scraper_config.extract_extra_chapters && chapter.is_extra != 0 {
                return true;
            }

            if chapter.chapter_index != Some(target_idx) {
                return true;
            }

            if let Some((target_title, target_is_extra)) = title_override {
                if chapter.title.as_deref() != Some(target_title.as_str()) {
                    return true;
                }

                let target_is_extra = if scraper_config.extract_extra_chapters && target_is_extra {
                    1
                } else {
                    0
                };
                if chapter.is_extra != target_is_extra {
                    return true;
                }
            }
        }

        false
    }

    /// 把「纯区间目录」直接摊平到父目录。
    ///
    /// 形如 `1-500` 的目录只是分段容器，不是一本书。直接把里面的音频文件
    /// 并进父目录，由父目录统一建立章节；这样书里的集数就是 1~500，
    /// 而不是出现一本名叫「1-500」的书。
    fn coalesce_webdav_range_directory_groups(
        &self,
        mut dir_groups: HashMap<String, Vec<WebDavFileEntry>>,
    ) -> (
        HashMap<String, Vec<WebDavFileEntry>>,
        HashMap<String, CoalescedRangeDirs<String>>,
    ) {
        let mut candidates: HashMap<String, Vec<(String, ChapterRangeDir)>> = HashMap::new();

        for dir_url in dir_groups.keys() {
            let Some(parent_url) = webdav_parent_url(dir_url) else {
                continue;
            };
            let dir_name = self.webdav_url_name(dir_url);
            let Some(range_dir) = parse_chapter_range_dir_name(&dir_name) else {
                continue;
            };

            candidates
                .entry(parent_url)
                .or_default()
                .push((dir_url.clone(), range_dir));
        }

        let mut coalesced_range_dirs = HashMap::new();

        for (parent_url, entries) in candidates {
            let parent_name = self.webdav_url_name(&parent_url);
            let ranges: Vec<ChapterRangeDir> =
                entries.iter().map(|(_, range)| range.clone()).collect();

            for group in select_mergeable_range_groups(&parent_name, &ranges) {
                let mut selected: Vec<(String, ChapterRangeDir)> = group
                    .indices
                    .into_iter()
                    .map(|index| entries[index].clone())
                    .collect();
                selected.sort_by_key(|(_, range)| (range.start, range.end));

                let Some(first_child_dir) =
                    selected.first().map(|(child_dir, _)| child_dir.clone())
                else {
                    continue;
                };

                let target_dir = if group.merge_into_parent {
                    parent_url.clone()
                } else {
                    first_child_dir
                };

                let child_dirs: Vec<String> = selected
                    .iter()
                    .map(|(child_dir, _)| child_dir.clone())
                    .collect();

                for child_dir in &child_dirs {
                    if let Some(mut child_files) = dir_groups.remove(child_dir) {
                        dir_groups
                            .entry(target_dir.clone())
                            .or_default()
                            .append(&mut child_files);
                    }
                }

                if !child_dirs.is_empty() {
                    // 多个分组可能都并进同一个父目录。旧写法直接 insert，
                    // 后一个分组会把前一个的 child_dirs 覆盖掉，导致先前那些
                    // 区间目录下的旧书不会被吸收，扫描后仍留在书架上。
                    let entry =
                        coalesced_range_dirs
                            .entry(target_dir)
                            .or_insert(CoalescedRangeDirs {
                                child_dirs: Vec::new(),
                                title_override: None,
                            });
                    entry.child_dirs.extend(child_dirs);
                    if entry.title_override.is_none() {
                        entry.title_override = group.title;
                    }
                }
            }
        }

        (dir_groups, coalesced_range_dirs)
    }

    fn webdav_url_name(&self, url: &str) -> String {
        let raw_name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("");
        self.decode_url_path(raw_name)
    }

    fn infer_webdav_series_directories<'a>(
        &self,
        dirs: impl Iterator<Item = &'a String>,
    ) -> HashMap<String, crate::core::library_scanner::shared::InferredSeriesInfo> {
        let candidates: Vec<SeriesDirectoryCandidate<String>> = dirs
            .filter_map(|dir_url| {
                let parent_url = webdav_parent_url(dir_url)?;
                let dir_name = self.webdav_url_name(dir_url);
                let parent_name = self.webdav_url_name(&parent_url);
                Some(SeriesDirectoryCandidate {
                    key: dir_url.clone(),
                    parent_key: parent_url,
                    parent_name,
                    name: dir_name,
                })
            })
            .collect();

        infer_series_directories(&candidates)
    }
}

fn webdav_directory_fingerprint(entries: &[WebDavFileEntry]) -> Option<String> {
    if entries
        .iter()
        .any(|(_, modified, validator)| modified.is_none() && validator.is_none())
    {
        return None;
    }

    // Each entry is hashed independently, then combined commutatively. URLs
    // are unique within a directory, so this is a stable multiset digest in
    // O(F) without sorting every unchanged remote book.
    let mut aggregate = [0u8; 32];
    for (url, modified, validator) in entries {
        let mut entry_hasher = Sha256::new();
        entry_hasher.update(url.as_bytes());
        entry_hasher.update([0]);
        entry_hasher.update(
            modified
                .map(|value| value.timestamp_nanos_opt().unwrap_or_default())
                .unwrap_or_default()
                .to_le_bytes(),
        );
        entry_hasher.update(validator.as_deref().unwrap_or_default().as_bytes());
        let digest = entry_hasher.finalize();
        for (target, value) in aggregate.iter_mut().zip(digest) {
            *target ^= value;
        }
    }

    let mut hasher = Sha256::new();
    hasher.update((entries.len() as u64).to_le_bytes());
    hasher.update(aggregate);
    Some(format!("{:x}", hasher.finalize()))
}

fn webdav_scan_config_fingerprint(config: &crate::db::models::ScraperConfig) -> String {
    let serialized = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized);
    format!("{:x}", hasher.finalize())
}

fn webdav_parent_url(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/');
    let slash_index = trimmed.rfind('/')?;
    if slash_index == 0 {
        return None;
    }
    Some(trimmed[..slash_index].to_string())
}

#[cfg(test)]
#[path = "../../../../tests/unit/core/library_scanner/webdav.rs"]
mod tests;

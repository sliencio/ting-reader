use super::super::{LibraryScanner, ScanStatus};
use super::chapters::WebDavChapterContext;
use crate::core::app::error::Result;
use crate::core::library_scanner::shared::directory_book_name;
use crate::db::repository::Repository;
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

fn choose_metadata_value<T>(
    local: Option<T>,
    audio: Option<T>,
    local_rank: usize,
    audio_rank: usize,
) -> (Option<T>, usize) {
    match (local, audio) {
        (Some(_), Some(audio)) if audio_rank < local_rank => (Some(audio), audio_rank),
        (Some(local), Some(_)) => (Some(local), local_rank),
        (Some(local), None) => (Some(local), local_rank),
        (None, Some(audio)) => (Some(audio), audio_rank),
        (None, None) => (None, usize::MAX),
    }
}

pub(super) struct WebDavBookContext<'a> {
    pub(super) library: &'a crate::db::models::Library,
    pub(super) dir_url: &'a str,
    pub(super) file_urls: &'a [String],
    pub(super) metadata_files: &'a [String],
    pub(super) scraper_config: &'a crate::db::models::ScraperConfig,
    pub(super) full_scan: bool,
    pub(super) existing_info: Option<(String, i32, Option<String>)>,
    pub(super) fallback_title_override: Option<&'a str>,
}

impl LibraryScanner {
    pub(super) async fn process_webdav_book(
        &self,
        context: WebDavBookContext<'_>,
    ) -> Result<(String, ScanStatus)> {
        let WebDavBookContext {
            library,
            dir_url,
            file_urls,
            metadata_files,
            scraper_config,
            full_scan,
            existing_info,
            fallback_title_override,
        } = context;
        // Derive title from directory name
        // Decode URL to handle percent-encoded characters (e.g. Chinese)
        let decoded_url = self.decode_url_path(dir_url);
        let dir_name_title = decoded_url
            .trim_end_matches('/')
            .split('/')
            .next_back()
            .unwrap_or("Unknown Book")
            .to_string();
        let directory_name = fallback_title_override
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&dir_name_title);
        let directory_title = self.text_cleaner.clean_book_directory_title(directory_name);
        let parent_name = super::webdav_parent_url(dir_url)
            .map(|parent| self.webdav_url_name(&parent))
            .unwrap_or_default();
        let directory_metadata = directory_book_name(directory_name, &parent_name, scraper_config);

        // No local path, use URL as path
        // We use the original URL as path to ensure connectivity, but StorageService needs to handle it correctly
        let path = dir_url.to_string();

        // Check if book exists
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        let path_hash = format!("{:x}", hasher.finalize());
        let (book_id, manual_corrected) = if let Some((ref id, mc, _)) = existing_info {
            (id.clone(), mc == 1)
        } else if let Some(book) = self.book_repo.find_by_hash(&path_hash).await? {
            (book.id, book.manual_corrected == 1)
        } else {
            (Uuid::new_v4().to_string(), false)
        };

        // Prepare temp directory for WebDAV book metadata and cover
        // Structure: temp/{book_hash}/
        let temp_book_dir = crate::core::books::metadata::remote_metadata_dir(&path)?;
        if !temp_book_dir.exists() {
            std::fs::create_dir_all(&temp_book_dir).ok();
        }

        let mut published_year: Option<String> = None;
        let mut json_tags: Vec<String> = Vec::new();
        let mut json_series: Vec<String> = Vec::new();
        let mut chapter_title_template: Option<String> = None;
        let mut ai_chapter_titles: Vec<String> = Vec::new();
        let is_cloud_mode = scraper_config.cloud_mode;

        // Extract metadata from WebDAV files (try multiple files if needed)
        let (
            meta_album,
            mut _meta_chapter_title,
            mut meta_author,
            mut meta_narrator,
            mut meta_cover_url,
            _meta_duration,
        ) = if !is_cloud_mode && !manual_corrected && !file_urls.is_empty() {
            // 尝试多个文件，直到找到完整的元数据（包括封面）
            let mut album = String::new();
            let mut title = String::new();
            let mut author = None;
            let mut narrator = None;
            let mut cover_url = None;
            let mut duration = 0;

            let max_files = std::cmp::min(file_urls.len(), 3); // 最多尝试 3 个文件

            for (index, file_url) in file_urls.iter().take(max_files).enumerate() {
                let (a, t, au, n, c, d) = self
                    .extract_webdav_metadata(
                        library,
                        file_url,
                        if index == 0 {
                            Some(&temp_book_dir)
                        } else {
                            None
                        }, // 只在第一个文件时保存封面到目录
                        scraper_config.extract_audio_cover,
                    )
                    .await;

                if index == 0 {
                    tracing::debug!("Extracting metadata from the first WebDAV file");
                } else {
                    tracing::debug!("Supplementing metadata from WebDAV file #{}", index + 1);
                }

                // 只在第一个文件或缺失时提取基本元数据
                if (index == 0 || album.is_empty()) && !a.is_empty() {
                    album = a;
                }
                if (index == 0 || title.is_empty()) && !t.is_empty() {
                    title = t;
                }
                if (index == 0 || author.is_none()) && au.is_some() {
                    author = au;
                }
                if (index == 0 || narrator.is_none()) && n.is_some() {
                    narrator = n;
                }
                if index == 0 {
                    duration = d;
                }

                // 封面：如果还没有找到，继续尝试
                if scraper_config.extract_audio_cover && cover_url.is_none() && c.is_some() {
                    cover_url = c;
                    if index > 0 {
                        tracing::info!("Found cover in WebDAV file #{}", index + 1);
                    }
                }

                // 如果已经找到了所有需要的元数据，就停止
                let has_basic_metadata = !album.is_empty() || author.is_some();
                let has_cover_if_needed =
                    !scraper_config.extract_audio_cover || cover_url.is_some();

                if has_basic_metadata && has_cover_if_needed {
                    tracing::debug!("Complete WebDAV metadata found; stopping file attempts");
                    break;
                }
            }

            (album, title, author, narrator, cover_url, duration)
        } else {
            (String::new(), String::new(), None, None, None, 0)
        };
        let audio_album = (!meta_album.trim().is_empty()).then_some(meta_album.clone());
        let audio_author = meta_author.clone();
        let audio_narrator = meta_narrator.clone();
        let audio_cover_url = meta_cover_url.clone();
        let mut local_album: Option<String> = None;
        let mut local_author: Option<String> = None;
        let mut local_narrator: Option<String> = None;
        let mut local_cover_url: Option<String> = None;
        let mut local_description: Option<String> = None;
        let mut local_genre: Option<String> = None;

        let has_metadata_json = metadata_files
            .iter()
            .any(|url| url.split('/').next_back().unwrap_or_default() == "metadata.json")
            || manual_corrected && temp_book_dir.join("metadata.json").is_file();
        let has_nfo = metadata_files.iter().any(|url| url.ends_with("/book.nfo"))
            || manual_corrected && temp_book_dir.join("book.nfo").is_file();

        // Try to fetch and parse metadata.json and book.nfo from WebDAV
        // We do this by downloading them to temp_book_dir
        if let Some(storage) = &self.storage_service {
            // Decryption key
            let key = self.encryption_key.as_deref().unwrap_or(&[0u8; 32]);

            for meta_url in metadata_files {
                let filename = meta_url.split('/').next_back().unwrap_or_default();
                if filename == "metadata.json" || filename == "book.nfo" {
                    let temp_path = temp_book_dir.join(filename);
                    if manual_corrected && temp_path.is_file() {
                        continue;
                    }
                    if let Err(error) = crate::core::books::metadata::cache_remote_file(
                        storage,
                        library,
                        meta_url,
                        &temp_path,
                        key,
                        4 * 1024 * 1024,
                    )
                    .await
                    {
                        warn!(file = %filename, error = %error, "Failed to cache WebDAV metadata");
                    }
                }
            }
        }

        // Read metadata.json if downloaded
        let mut json_chapters: Option<Vec<crate::core::books::metadata::AudiobookshelfChapter>> =
            None;
        if has_metadata_json
            && let Ok(Some(json_meta)) =
                crate::core::books::metadata::read_metadata_json(&temp_book_dir)
        {
            let metadata = crate::core::books::metadata::BookMetadataFields::from(json_meta);
            local_album = metadata.title.filter(|title| !title.trim().is_empty());
            local_author = metadata.author;
            local_narrator = metadata.narrator;
            json_series = metadata.json_series;
            json_tags = metadata.json_tags;
            local_genre = metadata.genre;
            local_description = metadata.description;
            json_chapters = metadata.json_chapters;
            published_year = metadata.published_year;
            local_cover_url = metadata.cover_url;
        }

        if has_nfo
            && let Ok(nfo) = self
                .nfo_manager
                .read_book_nfo(&temp_book_dir.join("book.nfo"))
        {
            let metadata = crate::core::books::metadata::BookMetadataFields::from(nfo);
            local_album = local_album.or(metadata.title);
            local_author = local_author.or(metadata.author);
            local_narrator = local_narrator.or(metadata.narrator);
            local_cover_url = local_cover_url.or(metadata.cover_url);
            local_description = local_description.or(metadata.description);
            local_genre = local_genre.or(metadata.genre.filter(|value| !value.is_empty()));
        }

        // Also check if there's a local cover image directly in the webdav folder
        // Download it to temp_book_dir so the proxy can serve it as a local file.
        // Storing the raw WebDAV URL doesn't work because the frontend/proxy lacks WebDAV auth.
        if let Some(storage) = &self.storage_service {
            let key = self.encryption_key.as_deref().unwrap_or(&[0u8; 32]);
            for meta_url in metadata_files {
                let filename = meta_url
                    .split('/')
                    .next_back()
                    .unwrap_or_default()
                    .to_lowercase();
                if [
                    "cover.jpg",
                    "cover.png",
                    "cover.jpeg",
                    "cover.webp",
                    "folder.jpg",
                ]
                .contains(&filename.as_str())
                {
                    // Download cover image to temp_book_dir
                    let original_ext = meta_url.split('.').next_back().unwrap_or("jpg");
                    let temp_cover_path = temp_book_dir.join(format!("cover.{}", original_ext));
                    if !(manual_corrected && temp_cover_path.is_file())
                        && let Err(error) = crate::core::books::metadata::cache_remote_file(
                            storage,
                            library,
                            meta_url,
                            &temp_cover_path,
                            key,
                            10 * 1024 * 1024,
                        )
                        .await
                    {
                        warn!(error = %error, "Failed to cache WebDAV cover");
                    }
                    if temp_cover_path.is_file()
                        && local_cover_url.as_deref().is_none_or(|cover| {
                            !crate::core::books::metadata::is_cover_url(cover) || cover == meta_url
                        })
                    {
                        local_cover_url =
                            Some(temp_cover_path.to_string_lossy().replace('\\', "/"));
                    }
                    break;
                }
            }
        }

        let local_rank = scraper_config.metadata_rank("local_metadata");
        let audio_rank = scraper_config.metadata_rank("audio_metadata");
        let scraper_rank = scraper_config.metadata_rank("scraper");
        let (selected_title, title_source) = scraper_config.select_title(
            &directory_title,
            local_album.as_deref(),
            audio_album.as_deref(),
            None,
        );
        let title_rank = scraper_config.metadata_rank(title_source);
        let book_title = self.text_cleaner.clean_filename(selected_title);
        let (selected_author, author_rank) =
            choose_metadata_value(local_author, audio_author, local_rank, audio_rank);
        let (selected_narrator, narrator_rank) =
            choose_metadata_value(local_narrator, audio_narrator, local_rank, audio_rank);
        let (selected_cover, cover_rank) =
            choose_metadata_value(local_cover_url, audio_cover_url, local_rank, audio_rank);
        meta_author = selected_author;
        meta_narrator = selected_narrator;
        meta_cover_url = selected_cover;
        let description_rank = if local_description.is_some() {
            local_rank
        } else {
            usize::MAX
        };
        let tags_rank = if json_tags.is_empty() {
            usize::MAX
        } else {
            local_rank
        };

        // Create or Update book
        let mut book = crate::db::models::Book {
            id: book_id.clone(),
            library_id: library.id.clone(),
            title: Some(book_title.clone()),
            author: meta_author.or_else(|| directory_metadata.author.map(str::to_owned)),
            narrator: meta_narrator.or_else(|| directory_metadata.narrator.map(str::to_owned)),
            cover_url: meta_cover_url,
            description: local_description,
            created_at: chrono::Utc::now().to_rfc3339(),
            path: path.clone(),
            hash: path_hash.clone(),
            theme_color: None,
            skip_intro: 0,
            skip_outro: 0,
            tags: (!json_tags.is_empty()).then(|| json_tags.join(",")),
            genre: local_genre,
            year: published_year.as_ref().and_then(|y| y.parse::<i32>().ok()),
            manual_corrected: if manual_corrected { 1 } else { 0 },
            match_pattern: None,
            chapter_regex: None,
        };

        // If manual corrected, we should preserve existing fields.
        // We need to fetch the existing book to do that properly if we are updating.
        if manual_corrected
            && let Ok(Some(existing_book)) = self.book_repo.find_by_id(&book_id).await
        {
            book.title = existing_book.title;
            book.author = existing_book.author;
            book.narrator = existing_book.narrator;
            book.description = existing_book.description;
            book.tags = existing_book.tags;
            book.cover_url = existing_book.cover_url;
            book.theme_color = existing_book.theme_color;
            book.chapter_regex = existing_book.chapter_regex;
        }

        // Run scraper if enabled and NOT manual corrected
        if !manual_corrected
            && scraper_config.has_scraper_sources()
            && let Some(scraper_service) = &self.scraper_service
        {
            let chapter_candidates = file_urls
                .iter()
                .enumerate()
                .map(|(index, file_url)| {
                    let decoded_file_url = self.decode_url_path(file_url);
                    let filename = decoded_file_url
                        .split('/')
                        .next_back()
                        .unwrap_or("chapter")
                        .to_string();
                    let title = filename
                        .rsplit_once('.')
                        .map(|(stem, _)| stem)
                        .filter(|stem| !stem.is_empty())
                        .unwrap_or(&filename)
                        .to_string();

                    serde_json::json!({
                        "index": index + 1,
                        "filename": filename,
                        "title": title,
                        "path": file_url,
                    })
                })
                .collect::<Vec<_>>();
            let scrape_context = serde_json::json!({
                "library_type": "webdav",
                "directory": dir_url,
                "directory_name": fallback_title_override
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&dir_name_title),
                "chapters": chapter_candidates,
                "current_metadata": {
                    "title": book.title,
                    "author": book.author,
                    "narrator": book.narrator,
                    "cover_url": book.cover_url,
                    "description": book.description,
                    "tags": book.tags,
                },
            });
            match scraper_service
                .scrape_book_metadata_with_context(
                    &book_title,
                    scraper_config,
                    Some(scrape_context),
                )
                .await
            {
                Ok(detail) => {
                    if !detail.title.trim().is_empty() && scraper_rank < title_rank {
                        book.title = Some(detail.title);
                    }

                    if !detail.author.is_empty()
                        && (book.author.as_deref() == Some("Unknown")
                            || book.author.is_none()
                            || scraper_rank < author_rank)
                    {
                        book.author = Some(detail.author);
                    }

                    if !detail.intro.is_empty()
                        && (book.description.is_none() || scraper_rank < description_rank)
                    {
                        book.description = Some(detail.intro);
                    }

                    if detail.cover_url.is_some()
                        && (book.cover_url.is_none() || scraper_rank < cover_rank)
                    {
                        book.cover_url = detail.cover_url;
                    }

                    if detail.narrator.is_some()
                        && (book.narrator.is_none() || scraper_rank < narrator_rank)
                    {
                        book.narrator = detail.narrator;
                    }

                    if !detail.tags.is_empty() && (book.tags.is_none() || scraper_rank < tags_rank)
                    {
                        book.tags = Some(detail.tags.join(","));
                    }

                    // Fill the year from the scraper only when local metadata
                    // is missing or the configured priority prefers scraping.
                    if let Some(year) = detail
                        .published_year
                        .as_deref()
                        .and_then(|value| value.parse::<i32>().ok())
                        && (book.year.is_none() || scraper_rank < local_rank)
                    {
                        book.year = Some(year);
                    }
                    chapter_title_template = detail.chapter_title_template;
                    if !detail.chapter_titles.is_empty() {
                        ai_chapter_titles = detail.chapter_titles;
                    }
                }
                Err(e) => {
                    warn!("Scraper failed for WebDAV book {}: {}", book_title, e);
                }
            }
        }

        // Calculate theme color if cover exists
        // If cover is from scraper (http), we fetch it.
        // If cover is local (relative), we fetch it from WebDAV.
        // Currently scraper returns HTTP URLs usually.
        // But if we want to support cover.jpg in WebDAV folder:
        // We need to implement find_cover_image for WebDAV.

        // For now, if scraper provided cover_url, we try to calculate color.
        if !manual_corrected && let Some(ref url) = book.cover_url {
            let cover_path = if url.starts_with("//") {
                format!("https:{}", url)
            } else {
                url.clone()
            };
            if let Ok(Some(color)) = crate::core::books::color::calculate_theme_color_with_client(
                &cover_path,
                &self.http_client,
            )
            .await
            {
                book.theme_color = Some(color);
            }
        }

        let mut status = ScanStatus::Created;
        // Check if existing book (by ID check above)
        if existing_info.is_some() {
            if !manual_corrected {
                // Preserve chapter_regex from existing book if not set in metadata
                if book.chapter_regex.is_none()
                    && let Ok(Some(existing)) = self.book_repo.find_by_id(&book_id).await
                {
                    book.chapter_regex = existing.chapter_regex;
                }
                self.book_repo.update(&book).await?;
                status = ScanStatus::Updated;
            } else {
                status = ScanStatus::Skipped;
            }
        } else if let Ok(Some(existing)) = self.book_repo.find_by_id(&book_id).await {
            if !manual_corrected {
                // Preserve chapter_regex from existing book if not set in metadata
                if book.chapter_regex.is_none() {
                    book.chapter_regex = existing.chapter_regex;
                }
                self.book_repo.update(&book).await?;
                status = ScanStatus::Updated;
            } else {
                status = ScanStatus::Skipped;
            }
        } else {
            self.book_repo.create(&book).await?;
        }

        let chapters_changed = self
            .process_webdav_chapters(WebDavChapterContext {
                library,
                book: &book,
                file_urls,
                scraper_config,
                full_scan,
                manual_corrected,
                json_chapters,
                chapter_title_template: chapter_title_template.as_deref(),
                ai_chapter_titles: &ai_chapter_titles,
            })
            .await?;

        self.metadata_service()
            .import_series(&book, &json_series)
            .await?;

        // Scans populate the cache only; remote uploads are reserved for explicit edits.
        self.metadata_service().ensure_remote_cache(&book).await?;

        let final_status = match status {
            ScanStatus::Created => ScanStatus::Created,
            _ => {
                if chapters_changed {
                    ScanStatus::Updated
                } else {
                    status
                }
            }
        };

        Ok((book_id, final_status))
    }
}

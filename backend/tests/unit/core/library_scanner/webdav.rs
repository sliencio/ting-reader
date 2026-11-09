use super::webdav_directory_fingerprint;

struct WebDavTestServer(tokio::task::JoinHandle<()>);

struct WebDavTestCache(std::path::PathBuf);

impl Drop for WebDavTestCache {
    fn drop(&mut self) {
        let root = std::env::current_dir().unwrap().join("temp");
        if self.0.parent() == Some(root.as_path()) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
async fn webdav_series_inherit_parent_people_and_sort_decoded_chinese_chapters() {
    use crate::core::library_scanner::{ScanMode, test_support};
    use axum::{extract::State, http::StatusCode, response::IntoResponse};
    use std::{collections::HashMap, sync::Arc};
    type Listings = Arc<HashMap<String, Vec<(String, bool)>>>;
    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let mut library = test_support::library(&scanner, directory.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    library.url = format!("{origin}/dav");
    library.library_type = "webdav".into();
    let mut config = crate::db::models::ScraperConfig {
        cloud_mode: true,
        extract_audio_cover: false,
        metadata_priority: vec!["local_metadata".into()],
        ..Default::default()
    };
    library.scraper_config = Some(serde_json::to_string(&config).unwrap());
    scanner.library_repo.update(&library).await.unwrap();
    let child = |parent: &str, name: &str| {
        let mut url = url::Url::parse(parent).unwrap();
        url.path_segments_mut().unwrap().push(name);
        url.to_string()
    };
    let parent = child(&library.url, "示例系列 - 示例作者 ／ 示例演播");
    let mut listing: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    let mut add = |parent: &str, url: &str, is_dir| {
        listing
            .entry(url::Url::parse(parent).unwrap().path().into())
            .or_default()
            .push((url::Url::parse(url).unwrap().path().into(), is_dir));
    };
    add(&library.url, &parent, true);
    let mut caches = Vec::new();
    for name in ["示例系列第一部", "示例系列第二部", "示例系列第三部"] {
        let volume = child(&parent, name);
        let cache = crate::core::books::metadata::remote_metadata_dir(&volume).unwrap();
        assert!(!cache.exists());
        caches.push(WebDavTestCache(cache));
        add(&parent, &volume, true);
        for (range, files) in [
            ("1-50", vec!["10", "2", "1"]),
            (
                "51-100任意说明",
                vec!["第四百七十章 示例", "第十章 示例", "第三章 示例"],
            ),
        ] {
            let range = child(&volume, range);
            add(&volume, &range, true);
            for file in files {
                add(&range, &child(&range, &format!("{file}.mp3")), false);
            }
        }
    }
    let app = axum::Router::new().fallback(
        |State(listing): State<Listings>,
         request: axum::extract::Request| async move {
            if request.method().as_str() != "PROPFIND" {
                return StatusCode::NOT_FOUND.into_response();
            }
            let Some(entries) = listing.get(request.uri().path().trim_end_matches('/')) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let responses: String = entries.iter().map(|(path, is_dir)| format!(
                "<d:response><d:href>{path}</d:href><d:propstat><d:prop><d:resourcetype>{}</d:resourcetype><d:getetag>v1</d:getetag><d:getlastmodified>Thu, 15 Oct 2026 00:00:00 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                if *is_dir { "<d:collection/>" } else { "" },
            )).collect();
            (StatusCode::MULTI_STATUS, [("content-type", "application/xml")],
             format!("<d:multistatus xmlns:d=\"DAV:\">{responses}</d:multistatus>")).into_response()
        }
    ).with_state(Arc::new(listing));
    let _server = WebDavTestServer(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    for (enabled, mode, expected_series) in [
        (false, ScanMode::Full, 0),
        (false, ScanMode::Incremental, 0),
        (true, ScanMode::Incremental, 1),
        (false, ScanMode::Full, 1),
    ] {
        config.auto_generate_series = enabled;
        let result = scanner
            .scan_webdav_library(&library, None, &config, mode, None, false)
            .await
            .unwrap();
        assert_eq!(result.failed_count, 0, "{:?}", result.errors);
        let books = scanner
            .book_repo
            .find_by_library(&library.id)
            .await
            .unwrap();
        assert_eq!(books.len(), 3);
        for book in books {
            assert!(book.title.as_deref().unwrap().starts_with("示例系列第"));
            assert_eq!(book.author.as_deref(), Some("示例作者"));
            assert_eq!(book.narrator.as_deref(), Some("示例演播"));
            let chapters = scanner.chapter_repo.find_by_book(&book.id).await.unwrap();
            let names: Vec<_> = chapters
                .iter()
                .map(|chapter| {
                    let url = url::Url::parse(&chapter.path).unwrap();
                    urlencoding::decode(url.path_segments().unwrap().next_back().unwrap())
                        .unwrap()
                        .into_owned()
                })
                .collect();
            assert_eq!(
                names,
                [
                    "1.mp3",
                    "2.mp3",
                    "10.mp3",
                    "第三章 示例.mp3",
                    "第十章 示例.mp3",
                    "第四百七十章 示例.mp3"
                ]
            );
            let series = scanner
                .series_repo
                .find_series_by_book(&book.id)
                .await
                .unwrap();
            assert_eq!(series.len(), expected_series);
            if let Some(series) = series.first() {
                assert_eq!(series.title, "示例系列");
            }
        }
    }
    drop(caches);
}

#[tokio::test]
async fn webdav_metadata_json_series_import_ignores_directory_series_setting() {
    use crate::core::library_scanner::test_support;
    use axum::{http::StatusCode, response::IntoResponse};
    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let mut library = test_support::library(&scanner, directory.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    library.url = format!("http://{}/dav", listener.local_addr().unwrap());
    library.library_type = "webdav".into();
    let dir_url = format!("{}/book", library.url);
    let _cache =
        WebDavTestCache(crate::core::books::metadata::remote_metadata_dir(&dir_url).unwrap());
    let app = axum::Router::new().fallback(|request: axum::extract::Request| async move {
        if request.uri().path().ends_with("/metadata.json") {
            (
                StatusCode::OK,
                r#"{"title":"元数据书名","series":["元数据系列 #2"],"chapters":[{"id":0,"start":0,"end":10,"title":"元数据章节"}]}"#,
            )
                .into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    });
    let _server = WebDavTestServer(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    let files = vec![format!("{dir_url}/001.mp3")];
    let metadata_files = vec![format!("{dir_url}/metadata.json")];
    let mut config = crate::db::models::ScraperConfig {
        cloud_mode: true,
        extract_audio_cover: false,
        metadata_priority: vec!["local_metadata".into()],
        ..Default::default()
    };
    for (enabled, priority, expected_book, expected_chapter) in [
        (false, vec!["filename", "local_metadata"], "book", "1"),
        (
            true,
            vec!["local_metadata", "filename"],
            "元数据书名",
            "元数据章节",
        ),
        (false, vec!["filename", "local_metadata"], "book", "1"),
    ] {
        config.auto_generate_series = enabled;
        config.metadata_priority = priority.into_iter().map(str::to_owned).collect();
        let (id, _) = scanner
            .process_webdav_book(super::processing::WebDavBookContext {
                library: &library,
                dir_url: &dir_url,
                file_urls: &files,
                metadata_files: &metadata_files,
                scraper_config: &config,
                full_scan: true,
                existing_info: None,
                fallback_title_override: None,
            })
            .await
            .unwrap();
        use crate::db::repository::Repository;
        let book = scanner.book_repo.find_by_id(&id).await.unwrap().unwrap();
        assert_eq!(book.title.as_deref(), Some(expected_book));
        let chapters = scanner.chapter_repo.find_by_book(&id).await.unwrap();
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title.as_deref(), Some(expected_chapter));
        assert_eq!(chapters[0].duration, Some(10));
        let series = scanner.series_repo.find_series_by_book(&id).await.unwrap();
        assert_eq!(series.len(), 1);
        if let Some(series) = series.first() {
            assert_eq!(series.title, "元数据系列");
            let members = scanner
                .series_repo
                .find_books_by_series(&series.id)
                .await
                .unwrap();
            assert_eq!(members[0].1, 2);
        }
    }
}

#[tokio::test]
async fn webdav_saves_clean_titles_and_merges_completed_range_directories() {
    use crate::core::library_scanner::test_support;
    use crate::db::repository::Repository;
    use std::collections::HashMap;

    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let mut library = test_support::library(&scanner, directory.path()).await;
    library.library_type = "webdav".into();
    library.url = format!("https://example.test/{}", uuid::Uuid::new_v4());
    let config = crate::db::models::ScraperConfig {
        metadata_priority: vec!["local_metadata".into()],
        extract_audio_cover: false,
        cloud_mode: true,
        ..Default::default()
    };
    library.scraper_config = Some(serde_json::to_string(&config).unwrap());
    scanner.library_repo.update(&library).await.unwrap();

    for (name, title_override, author, narrator) in [
        ("书名-作者／演播", None, Some("作者"), Some("演播")),
        ("书名-作者", None, Some("作者"), None),
        ("书名-示例演播", None, Some("示例演播"), None),
        (
            "书名 | 简介",
            Some("合并书名-作者／演播 | 全本"),
            Some("作者"),
            Some("演播"),
        ),
    ] {
        let mut url = url::Url::parse(&library.url).unwrap();
        url.path_segments_mut().unwrap().push(name);
        let parent = url.to_string();
        let cache = crate::core::books::metadata::remote_metadata_dir(&parent).unwrap();
        assert!(!cache.exists());
        let _cache = WebDavTestCache(cache);
        let groups = ["1-50", "51-100", "101-150集（已完结，感谢收听）"]
            .into_iter()
            .map(|segment| {
                let mut url = url::Url::parse(&parent).unwrap();
                url.path_segments_mut().unwrap().push(segment);
                let group_url = url.to_string();
                url.path_segments_mut().unwrap().push("chapter.mp3");
                (group_url, vec![(url.to_string(), None, None)])
            })
            .collect::<HashMap<_, Vec<super::WebDavFileEntry>>>();
        let (groups, coalesced) = scanner.coalesce_webdav_range_directory_groups(groups);
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key(&parent));
        let files: Vec<_> = groups[&parent]
            .iter()
            .map(|entry| entry.0.clone())
            .collect();
        assert_eq!(files.len(), 3);
        assert_eq!(coalesced[&parent].child_dirs.len(), 3);
        let (id, _) = scanner
            .process_webdav_book(super::processing::WebDavBookContext {
                library: &library,
                dir_url: &parent,
                file_urls: &files,
                metadata_files: &[],
                scraper_config: &config,
                full_scan: true,
                existing_info: None,
                fallback_title_override: title_override,
            })
            .await
            .unwrap();
        let book = scanner.book_repo.find_by_id(&id).await.unwrap().unwrap();
        assert_eq!(
            book.title.as_deref(),
            Some(if title_override.is_some() {
                "合并书名"
            } else {
                "书名"
            })
        );
        assert_eq!(book.author.as_deref(), author);
        assert_eq!(book.narrator.as_deref(), narrator);
        assert_eq!(
            scanner.chapter_repo.find_by_book(&id).await.unwrap().len(),
            3
        );
    }
}

impl Drop for WebDavTestServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn cloud_library() -> crate::db::models::Library {
    crate::db::models::Library {
        id: "cloud".into(),
        name: "cloud".into(),
        library_type: "webdav".into(),
        url: "http://127.0.0.1:1/dav".into(),
        username: None,
        password: None,
        root_path: "/".into(),
        last_scanned_at: None,
        created_at: String::new(),
        scraper_config: None,
    }
}

#[test]
fn cloud_mode_disables_all_audio_probes_independently_of_cover_and_write_settings() {
    let mut library = cloud_library();
    assert!(super::extraction::audio_metadata_enabled(&library));
    for cloud_mode in [true, false] {
        let config = crate::db::models::ScraperConfig {
            cloud_mode,
            extract_audio_cover: true,
            webdav_metadata_writing_enabled: true,
            ..Default::default()
        };
        library.scraper_config = Some(serde_json::to_string(&config).unwrap());
        assert_eq!(
            super::extraction::audio_metadata_enabled(&library),
            !cloud_mode
        );
        assert!(library.can_write_metadata_files());
    }
}

#[tokio::test]
async fn cloud_mode_extraction_never_requests_remote_audio() {
    use crate::core::library_scanner::test_support;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let scanner = test_support::scanner(directory.path());
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let app = axum::Router::new().fallback(axum::routing::get(move || {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::NOT_FOUND
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut library = cloud_library();
    library.url = format!("http://{}/dav", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = WebDavTestServer(server);
    for extract_cover in [false, true] {
        for allow_writes in [false, true] {
            library.scraper_config = Some(
                serde_json::to_string(&crate::db::models::ScraperConfig {
                    cloud_mode: true,
                    extract_audio_cover: extract_cover,
                    webdav_metadata_writing_enabled: allow_writes,
                    ..Default::default()
                })
                .unwrap(),
            );
            for extension in ["m4a", "wma", "xm", "strm"] {
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    scanner.extract_webdav_metadata(
                        &library,
                        &format!("{}/001.{extension}", library.url),
                        Some(directory.path()),
                        extract_cover,
                    ),
                )
                .await
                .unwrap();
                assert_eq!(result, (String::new(), "001".into(), None, None, None, 0));
                assert_eq!(library.can_write_metadata_files(), allow_writes);
            }
        }
    }
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(!directory.path().join("cover.jpg").exists());
    drop(server);
}

#[test]
fn directory_fingerprint_is_order_independent() {
    let first = vec![
        (
            "https://example.test/book/001.mp3".to_string(),
            None,
            Some("etag-1".to_string()),
        ),
        (
            "https://example.test/book/002.mp3".to_string(),
            None,
            Some("etag-2".to_string()),
        ),
    ];
    let mut reversed = first.clone();
    reversed.reverse();

    assert_eq!(
        webdav_directory_fingerprint(&first),
        webdav_directory_fingerprint(&reversed)
    );
}

#[test]
fn directory_fingerprint_changes_with_validator() {
    let before = vec![(
        "https://example.test/book/001.mp3".to_string(),
        None,
        Some("etag-1".to_string()),
    )];
    let after = vec![(
        "https://example.test/book/001.mp3".to_string(),
        None,
        Some("etag-2".to_string()),
    )];

    assert_ne!(
        webdav_directory_fingerprint(&before),
        webdav_directory_fingerprint(&after)
    );
}

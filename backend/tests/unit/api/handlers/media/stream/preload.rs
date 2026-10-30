use super::*;
use axum::http::{StatusCode, header};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn custom_window_memory_budget_and_idle_timeout_are_applied() {
    let prefix = read_preload_prefix_with_limit(b"abcdef".as_slice(), 6, 2)
        .await
        .unwrap();
    assert_eq!(prefix.data.as_ref(), b"ab");
    assert_eq!(prefix.total_size, Some(6));
    let mut cache = std::collections::HashMap::new();
    let idle = std::time::Duration::from_secs(5);
    insert_preload_cache_with(&mut cache, "old".into(), prefix.clone(), 3, idle);
    insert_preload_cache_with(&mut cache, "new".into(), prefix, 3, idle);
    assert_eq!(cache.len(), 1);
    assert!(cache.contains_key("new"));
    evict_expired_cache_with(&mut cache, std::time::Instant::now() + idle, idle);
    assert!(cache.is_empty());
}

fn entry(data: bytes::Bytes, total_size: Option<u64>) -> PreloadedChapter {
    PreloadedChapter {
        data,
        total_size,
        last_access: std::time::Instant::now(),
    }
}

#[test]
fn cache_evicts_oldest_and_never_exceeds_byte_limit() {
    let mut cache = std::collections::HashMap::new();
    for index in 0..9 {
        insert_preload_cache(
            &mut cache,
            index.to_string(),
            entry(
                bytes::Bytes::from(vec![0; PRELOAD_WINDOW_BYTES]),
                Some(100 * PRELOAD_WINDOW_BYTES as u64),
            ),
        );
    }
    assert_eq!(cache.len(), 8);
    assert!(!cache.contains_key("0"));
    assert_eq!(
        cache.values().map(|entry| entry.data.len()).sum::<usize>(),
        MAX_CACHE_BYTES
    );
}

#[test]
fn cache_expires_after_idle_timeout() {
    let mut cache = std::collections::HashMap::new();
    let now = std::time::Instant::now();
    cache.insert(
        "old".into(),
        PreloadedChapter {
            data: bytes::Bytes::from_static(b"a"),
            total_size: Some(1),
            last_access: now - CACHE_IDLE_TIMEOUT,
        },
    );
    cache.insert(
        "new".into(),
        entry(bytes::Bytes::from_static(b"b"), Some(1)),
    );
    evict_expired_cache(&mut cache, now);
    assert!(!cache.contains_key("old"));
    assert!(cache.contains_key("new"));
}

#[tokio::test]
async fn preload_keeps_large_chapter_prefix_without_overreading() {
    let data = vec![7; PRELOAD_WINDOW_BYTES + 10];
    for reported_size in [0, data.len() as u64] {
        let mut reader = data.as_slice();
        let cached = read_preload_prefix(&mut reader, reported_size)
            .await
            .unwrap();
        assert_eq!(cached.data.as_ref(), &data[..PRELOAD_WINDOW_BYTES]);
        assert_eq!(reader.len(), 10);
        assert_eq!(
            cached.total_size,
            (reported_size > 0).then_some(reported_size)
        );
    }
}

#[tokio::test]
async fn preload_bounds_source_read_buffers() {
    struct BoundedReader {
        remaining: usize,
    }
    impl AsyncRead for BoundedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            assert!(buffer.remaining() <= PRELOAD_READ_BYTES);
            let count = buffer.remaining().min(self.remaining);
            buffer.initialize_unfilled()[..count].fill(7);
            buffer.advance(count);
            self.remaining -= count;
            std::task::Poll::Ready(Ok(()))
        }
    }

    let mut reader = BoundedReader {
        remaining: PRELOAD_WINDOW_BYTES + 10,
    };
    let cached = read_preload_prefix(&mut reader, 0).await.unwrap();
    assert_eq!(cached.data.len(), PRELOAD_WINDOW_BYTES);
    assert!(cached.data.iter().all(|byte| *byte == 7));
    assert_eq!(reader.remaining, 10);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn mapped_prefix_releases_pages_after_last_response_or_cancellation() {
    // Isolate address checks from mappings created by other concurrent tests.
    const CHILD_ENV: &str = "TING_PRELOAD_MAPPING_TEST";
    if std::env::var_os(CHILD_ENV).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "api::handlers::media::stream::preload::tests::mapped_prefix_releases_pages_after_last_response_or_cancellation",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }

    fn mapped(address: usize) -> bool {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .any(|line| {
                let range = line.split_whitespace().next().unwrap();
                let (start, end) = range.split_once('-').unwrap();
                let start = usize::from_str_radix(start, 16).unwrap();
                let end = usize::from_str_radix(end, 16).unwrap();
                start <= address && address < end
            })
    }

    let source = vec![7; PRELOAD_WINDOW_BYTES];
    let cached = read_preload_prefix(source.as_slice(), source.len() as u64)
        .await
        .unwrap();
    let address = cached.data.as_ptr() as usize;
    let response = cached.data.slice(1024..2048);
    let mut cache = std::collections::HashMap::new();
    cache.insert("chapter".into(), cached);
    evict_expired_cache(&mut cache, std::time::Instant::now() + CACHE_IDLE_TIMEOUT);
    assert!(cache.is_empty());
    assert!(mapped(address));
    assert!(response.iter().all(|byte| *byte == 7));
    drop(response);
    assert!(!mapped(address));

    struct PendingReader(Arc<std::sync::atomic::AtomicUsize>);
    impl AsyncRead for PendingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            self.0.store(
                buffer.initialize_unfilled().as_ptr() as usize,
                Ordering::SeqCst,
            );
            std::task::Poll::Pending
        }
    }
    let address = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut pending = Box::pin(read_preload_prefix(PendingReader(address.clone()), 0));
    let waker = futures::task::noop_waker();
    let mut context = std::task::Context::from_waker(&waker);
    assert!(std::future::Future::poll(pending.as_mut(), &mut context).is_pending());
    let address = address.load(Ordering::SeqCst);
    assert!(mapped(address));
    drop(pending);
    assert!(!mapped(address));
}

#[tokio::test]
async fn preload_distinguishes_complete_files_from_unknown_length_windows() {
    let empty = read_preload_prefix(b"".as_slice(), 0).await.unwrap();
    assert!(empty.data.is_empty());
    assert_eq!(empty.total_size, Some(0));
    for reported_size in [0, 3] {
        let cached = read_preload_prefix(b"abc".as_slice(), reported_size)
            .await
            .unwrap();
        assert_eq!(cached.data, "abc");
        assert_eq!(cached.total_size, Some(3));
    }
    let data = vec![0; PRELOAD_WINDOW_BYTES];
    let cached = read_preload_prefix(data.as_slice(), data.len() as u64)
        .await
        .unwrap();
    assert_eq!(cached.total_size, Some(data.len() as u64));
    let cached = read_preload_prefix(data.as_slice(), 0).await.unwrap();
    assert_eq!(cached.total_size, None);
    assert_eq!(cached.data.len(), PRELOAD_WINDOW_BYTES);
    assert_eq!(
        read_preload_prefix(b"abc".as_slice(), 10)
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof,
    );
}

#[tokio::test]
async fn prefix_playback_preserves_full_bytes_and_range_headers() {
    let source = bytes::Bytes::from_static(b"abcdefghijklmnop");
    let streamer = crate::core::audio::AudioStreamer::new(Default::default());
    for (range, expected, tail_offset) in [
        (None, 0..16, Some(5)),
        (Some("bytes=0-2"), 0..3, None),
        (Some("bytes=3-8"), 3..9, Some(5)),
        (Some("bytes=5-7"), 5..8, Some(5)),
        (Some("bytes=9-12"), 9..13, Some(9)),
        (Some("bytes=4-"), 4..16, Some(5)),
        (Some("bytes=-3"), 13..16, Some(13)),
        (Some("bytes=-100"), 0..16, Some(5)),
        (Some("bytes=0-18446744073709551615"), 0..16, Some(5)),
    ] {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let tail = source.clone();
        let response = prefix_response(
            entry(source.slice(..5), Some(16)),
            &streamer,
            range,
            "audio/mpeg".into(),
            false,
            move |start, end| async move {
                recorded.lock().unwrap().push((start, end));
                Ok((
                    Box::new(std::io::Cursor::new(tail.slice(start as usize..)))
                        as Box<dyn AsyncRead + Send + Unpin>,
                    16,
                ))
            },
        )
        .unwrap()
        .unwrap();
        assert!(calls.lock().unwrap().is_empty(), "tail must open lazily");
        assert_eq!(
            response.status(),
            if range.is_some() {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            },
        );
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            (expected.end - expected.start).to_string(),
        );
        if range.is_some() {
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                format!("bytes {}-{}/16", expected.start, expected.end - 1),
            );
        }
        let body = axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap();
        assert_eq!(body, source.slice(expected.clone()), "{range:?}");
        let expected_calls = tail_offset
            .map(|start| vec![(start, expected.end as u64)])
            .unwrap_or_default();
        assert_eq!(*calls.lock().unwrap(), expected_calls, "{range:?}");
    }
}

#[tokio::test]
async fn dropping_prefix_response_and_head_never_start_tail_download() {
    use futures::StreamExt;
    let streamer = crate::core::audio::AudioStreamer::new(Default::default());
    for is_head in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let recorded = calls.clone();
        let response = prefix_response(
            entry(bytes::Bytes::from_static(b"abc"), Some(10)),
            &streamer,
            None,
            "audio/mpeg".into(),
            is_head,
            move |_, _| async move {
                recorded.fetch_add(1, Ordering::SeqCst);
                Ok((
                    Box::new(tokio::io::empty()) as Box<dyn AsyncRead + Send + Unpin>,
                    10,
                ))
            },
        )
        .unwrap()
        .unwrap();
        let mut stream = response.into_body().into_data_stream();
        if is_head {
            assert!(stream.next().await.is_none());
        } else {
            assert_eq!(stream.next().await.unwrap().unwrap(), "abc");
        }
        drop(stream);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn unknown_length_prefix_streams_remainder_without_false_content_length() {
    let streamer = crate::core::audio::AudioStreamer::new(Default::default());
    for tail in [b"def".as_slice(), b"".as_slice()] {
        let response = prefix_response(
            entry(bytes::Bytes::from_static(b"abc"), None),
            &streamer,
            None,
            "audio/mpeg".into(),
            false,
            move |start, end| async move {
                assert_eq!((start, end), (3, 0));
                Ok((Box::new(tail) as Box<dyn AsyncRead + Send + Unpin>, 0))
            },
        )
        .unwrap()
        .unwrap();
        assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        let body = axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), [b"abc".as_slice(), tail].concat());
    }
}

#[tokio::test]
async fn changed_source_length_fails_instead_of_splicing_a_different_file() {
    let streamer = crate::core::audio::AudioStreamer::new(Default::default());
    let response = prefix_response(
        entry(bytes::Bytes::from_static(b"abc"), Some(10)),
        &streamer,
        None,
        "audio/mpeg".into(),
        false,
        |_, _| async {
            Ok((
                Box::new(b"other".as_slice()) as Box<dyn AsyncRead + Send + Unpin>,
                8,
            ))
        },
    )
    .unwrap()
    .unwrap();
    assert!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .is_err()
    );
}

#[test]
fn invalid_cached_ranges_return_416_without_reading_the_source() {
    let streamer = crate::core::audio::AudioStreamer::new(Default::default());
    for range in ["bytes=16-", "bytes=8-7", "bytes=-0", "bytes=0-1,3-4"] {
        let response = prefix_response(
            entry(bytes::Bytes::from_static(b"abcde"), Some(16)),
            &streamer,
            Some(range),
            "audio/mpeg".into(),
            false,
            |_, _| async { panic!("invalid range must not read the source") },
        )
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */16");
    }
}

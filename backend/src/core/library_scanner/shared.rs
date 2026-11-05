//! Shared scan utilities used by both local and WebDAV scanners

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::OnceLock;
use tracing::{info, warn};

use super::{LibraryScanner, ScanResult};
use crate::core::books::text_cleaner::{BookName, parse_ordinal};
use crate::db::repository::Repository;
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChapterRangeDir {
    pub start: u32,
    pub end: u32,
    pub context: String,
    pub context_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChapterRangeMergeGroup {
    pub indices: Vec<usize>,
    pub title: Option<String>,
    pub merge_into_parent: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CoalescedRangeDirs<K> {
    pub child_dirs: Vec<K>,
    pub title_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InferredSeriesInfo {
    pub title: String,
    pub order: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct SeriesDirectoryCandidate<K> {
    pub key: K,
    pub parent_key: String,
    pub parent_name: String,
    pub name: String,
}

#[derive(Debug, Clone)]
struct ParsedSeriesDirectory {
    base_title: Option<String>,
    order: Option<i32>,
}

/// Pre-fetched book lookup data
pub struct PrefetchedBooks {
    /// Map: path -> (id, manual_corrected, match_pattern)
    pub path_map: HashMap<String, (String, i32, Option<String>)>,
    /// Map: hash -> (id, manual_corrected, match_pattern)
    pub hash_map: HashMap<String, (String, i32, Option<String>)>,
    /// All minimal book records
    pub all_books: Vec<(String, String, String, i32, Option<String>)>,
}

impl LibraryScanner {
    /// Pre-fetch all existing books for a library and build lookup maps
    pub(crate) async fn prefetch_books(&self, library_id: &str) -> PrefetchedBooks {
        let all_books = self
            .book_repo
            .find_all_minimal_by_library(library_id)
            .await
            .unwrap_or_default();

        let mut path_map = HashMap::new();
        let mut hash_map = HashMap::new();

        for (id, path, hash, manual_corrected, match_pattern) in &all_books {
            path_map.insert(
                path.clone(),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
            hash_map.insert(
                hash.clone(),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
        }

        PrefetchedBooks {
            path_map,
            hash_map,
            all_books,
        }
    }

    /// Handle deletion of books not found during scan
    pub(crate) async fn handle_book_deletions(
        &self,
        scan_result: &mut ScanResult,
        prefetched: &PrefetchedBooks,
        found_book_ids: &HashSet<String>,
        check_path_exists: bool,
    ) {
        for (id, path_str, _, _, _) in &prefetched.all_books {
            if found_book_ids.contains(id) {
                continue;
            }

            if check_path_exists {
                let path = std::path::Path::new(path_str);
                if path.exists() {
                    continue;
                }
            }

            info!("Book missing, deleting record: {}", path_str);
            if let Err(e) = self.book_repo.delete(id).await {
                warn!("Failed to delete missing book {}: {}", id, e);
            } else {
                scan_result.books_deleted += 1;
                if let Err(e) = self.chapter_repo.delete_by_book(id).await {
                    warn!("Failed to delete chapters for missing book {}: {}", id, e);
                }
            }
        }
    }

    pub(crate) async fn link_book_to_inferred_series(
        &self,
        library_id: &str,
        book_id: &str,
        series_info: &InferredSeriesInfo,
    ) -> crate::core::app::error::Result<()> {
        let title = series_info.title.trim();
        if title.is_empty() {
            return Ok(());
        }

        let Some(book) = self.book_repo.find_by_id(book_id).await? else {
            return Ok(());
        };

        let new_series = crate::db::models::Series {
            id: uuid::Uuid::new_v4().to_string(),
            library_id: library_id.to_string(),
            title: title.to_string(),
            author: book.author.clone(),
            narrator: book.narrator.clone(),
            cover_url: book.cover_url.clone(),
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        // Series identity is scoped by library in the repository, so identical
        // titles in separate libraries remain independent collections.
        let series = self.series_repo.find_or_create_by_title(new_series).await?;
        let target_order = series_info.order.max(1);
        self.series_repo
            .add_book(crate::db::models::SeriesBook {
                series_id: series.id,
                book_id: book_id.to_string(),
                book_order: target_order,
            })
            .await?;

        Ok(())
    }
}

type SeriesDirectoryGroup<'a, K> = Vec<(
    &'a SeriesDirectoryCandidate<K>,
    ParsedSeriesDirectory,
    String,
)>;

pub(crate) fn infer_series_directories<K>(
    candidates: &[SeriesDirectoryCandidate<K>],
) -> HashMap<K, InferredSeriesInfo>
where
    K: Clone + Eq + Hash,
{
    let mut grouped: HashMap<(String, String), SeriesDirectoryGroup<'_, K>> = HashMap::new();

    for candidate in candidates {
        let Some(parsed) = parse_series_directory_name(&candidate.name) else {
            continue;
        };

        let title = parsed
            .base_title
            .as_deref()
            .map(clean_series_base_title)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                clean_series_base_title(BookName::parse(&candidate.parent_name).title)
            });

        if title.is_empty() {
            continue;
        }

        let key = (
            candidate.parent_key.clone(),
            canonicalize_series_title_key(&title),
        );
        grouped
            .entry(key)
            .or_default()
            .push((candidate, parsed, title));
    }

    let mut inferred = HashMap::new();

    for (_, mut entries) in grouped {
        if entries.len() < 2 {
            continue;
        }

        entries.sort_by(|a, b| match (a.1.order, b.1.order) {
            (Some(left), Some(right)) => left
                .cmp(&right)
                .then_with(|| natord::compare(&a.0.name, &b.0.name)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => natord::compare(&a.0.name, &b.0.name),
        });

        let series_title = entries
            .iter()
            .find_map(|(_, parsed, title)| {
                parsed
                    .base_title
                    .as_ref()
                    .filter(|base| !base.trim().is_empty())
                    .map(|_| title.clone())
            })
            .unwrap_or_else(|| entries[0].2.clone());

        for (index, (candidate, parsed, _)) in entries.iter().enumerate() {
            let order = parsed.order.unwrap_or((index + 1) as i32).max(1);
            inferred.insert(
                candidate.key.clone(),
                InferredSeriesInfo {
                    title: series_title.clone(),
                    order,
                },
            );
        }
    }

    inferred
}

fn parse_series_directory_name(name: &str) -> Option<ParsedSeriesDirectory> {
    let normalized = normalize_series_dir_name(BookName::parse(name).title);

    if let Some(caps) = series_volume_regex().captures(&normalized) {
        let base = caps
            .name("base")
            .map(|m| clean_series_base_title(m.as_str()))
            .filter(|value| !value.is_empty());
        let num = caps
            .name("num2")
            .or_else(|| caps.name("num"))
            .and_then(|m| parse_series_number(m.as_str()));
        return Some(ParsedSeriesDirectory {
            base_title: base,
            order: num,
        });
    }

    if let Some(caps) = series_s_regex().captures(&normalized) {
        let base = caps
            .name("base")
            .map(|m| clean_series_base_title(m.as_str()))
            .filter(|value| !value.is_empty());
        let num = caps
            .name("num")
            .and_then(|m| parse_series_number(m.as_str()));
        return Some(ParsedSeriesDirectory {
            base_title: base,
            order: num,
        });
    }

    if let Some(caps) = series_word_regex().captures(&normalized) {
        let base = caps
            .name("base")
            .map(|m| clean_series_base_title(m.as_str()))
            .filter(|value| !value.is_empty());
        let num = caps
            .name("num")
            .and_then(|m| parse_series_number(m.as_str()));
        return Some(ParsedSeriesDirectory {
            base_title: base,
            order: num,
        });
    }

    if let Some((base, suffix)) = normalized.split_once('之') {
        let base = clean_series_base_title(base);
        if !base.is_empty() && !suffix.trim().is_empty() {
            return Some(ParsedSeriesDirectory {
                base_title: Some(base),
                order: parse_leading_series_number(suffix),
            });
        }
    }

    None
}

/// Inherit only from the immediate parent of the same book/series. A library
/// or unrelated collection directory must not supply another book's people.
pub(crate) fn directory_book_name<'a>(
    name: &'a str,
    parent: &'a str,
    config: &crate::db::models::ScraperConfig,
) -> BookName<'a> {
    let layout =
        crate::core::books::text_cleaner::BookNameLayout::from_config(&config.book_name_layout);
    let mut book = BookName::parse_with_layout(name, layout, &config.book_name_regex);
    let parent = BookName::parse_with_layout(parent, layout, &config.book_name_regex);
    let same_title =
        canonicalize_series_title_key(book.title) == canonicalize_series_title_key(parent.title);
    let same_series = parse_series_directory_name(book.title).is_some_and(|series| {
        series.base_title.is_none_or(|title| {
            canonicalize_series_title_key(&title) == canonicalize_series_title_key(parent.title)
        })
    });
    if !parent.title.is_empty() && (same_title || same_series) {
        book.author = book.author.or(parent.author);
        book.narrator = book.narrator.or(parent.narrator);
    }
    book
}

fn series_volume_regex() -> &'static Regex {
    static VOLUME_RE: OnceLock<Regex> = OnceLock::new();
    VOLUME_RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?P<base>.*?)\s*(?:第\s*)?(?P<num>[0-9]+|[零〇一二两三四五六七八九十百千万]+)(?:\s*[\(（]\s*(?P<num2>[0-9]+|[零〇一二两三四五六七八九十百千万]+)\s*[\)）])?\s*(?:卷|季|部|册|輯|辑)\s*$",
        )
        .unwrap()
    })
}

fn series_s_regex() -> &'static Regex {
    static S_RE: OnceLock<Regex> = OnceLock::new();
    S_RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?P<base>.*?)\s*[-_. ]*s(?P<num>[0-9]{1,3})\s*$").unwrap()
    })
}

fn series_word_regex() -> &'static Regex {
    static WORD_RE: OnceLock<Regex> = OnceLock::new();
    WORD_RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?P<base>.+?)\s*[-_. ]*(?:vol(?:ume)?|season|book)\s*\.?\s*(?P<num>[0-9]{1,3})\s*$")
            .unwrap()
    })
}

fn normalize_series_dir_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '０'..='９' => {
                let digit = (ch as u32 - '０' as u32) as u8 + b'0';
                normalized.push(digit as char);
            }
            '（' => normalized.push('('),
            '）' => normalized.push(')'),
            '　' => normalized.push(' '),
            _ => normalized.push(ch),
        }
    }
    normalized.trim().to_string()
}

fn clean_series_base_title(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '-' | '_' | '.' | '·' | ':' | '：' | '|' | '/' | '\\' | '(' | ')' | '（' | '）'
                )
        })
        .trim()
        .to_string()
}

fn canonicalize_series_title_key(value: &str) -> String {
    normalize_series_dir_name(value)
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '-' | '_' | '.' | '·' | ':' | '：'))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn parse_leading_series_number(value: &str) -> Option<i32> {
    let trimmed = value.trim_start();
    let mut token = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() || "零〇一二两三四五六七八九十百千万".contains(ch) {
            token.push(ch);
        } else if token.is_empty() && ch == '第' {
            continue;
        } else {
            break;
        }
    }

    if token.is_empty() {
        None
    } else {
        parse_series_number(&token)
    }
}

fn parse_series_number(value: &str) -> Option<i32> {
    parse_ordinal(value).filter(|number| *number > 0)
}

pub(crate) fn parse_chapter_range_dir_name(name: &str) -> Option<ChapterRangeDir> {
    let normalized = normalize_range_marks(name);
    let caps = range_regex().captures(&normalized)?;

    let full_match = caps.get(0)?;
    let start = caps.get(1)?.as_str().parse::<u32>().ok()?;
    let end = caps.get(2)?.as_str().parse::<u32>().ok()?;

    if end <= start || end > 999_999 {
        return None;
    }

    // Avoid common date folder names such as "2024-2025".
    if (1900..=2100).contains(&start) && (1900..=2100).contains(&end) && end - start <= 20 {
        return None;
    }

    let suffix = &normalized[full_match.end()..];
    if starts_with_numeric_range_tail(suffix) {
        return None;
    }
    // Only the title before the first range identifies the book. Everything
    // after the range is a description of this segment and does not affect merging.
    let prefix = &normalized[..full_match.start()];
    let context = canonicalize_range_context(prefix);
    let context_title = clean_range_context_title(prefix);

    Some(ChapterRangeDir {
        start,
        end,
        context,
        context_title,
    })
}

/// 把「名字本身就是一个集数区间」的目录直通到底。
///
/// 形如 `1-500`、`第1-500集`、`1-500章` 的目录只是分段容器，没有书名语义。
/// 这类目录不该被当成独立书目，也不该走「合并成系列」的分支——直接把它的
/// 文件摊平到父目录，由父目录统一做章节映射。
///
/// 返回 `Some((start, end))` 表示这是纯区间目录。
pub(crate) fn parse_pure_range_dir_name(name: &str) -> Option<(u32, u32)> {
    let range = parse_chapter_range_dir_name(name)?;
    // 有书名上下文说明这是「书名 1-500」这类分段，交给合并逻辑处理。
    if !range.context.is_empty() {
        return None;
    }
    Some((range.start, range.end))
}

pub(crate) fn select_mergeable_range_groups(
    parent_name: &str,
    ranges: &[ChapterRangeDir],
) -> Vec<ChapterRangeMergeGroup> {
    let mut by_context: HashMap<&str, Vec<usize>> = HashMap::new();

    for (index, range) in ranges.iter().enumerate() {
        by_context
            .entry(range.context.as_str())
            .or_default()
            .push(index);
    }

    let mut parent_groups = Vec::new();
    let mut contextual_groups = Vec::new();

    for indices in by_context.values() {
        let group: Vec<ChapterRangeDir> =
            indices.iter().map(|index| ranges[*index].clone()).collect();
        if group.is_empty() {
            continue;
        }

        // 目录名没有书名（`1-50`），或书名正好就是父目录名（`完美世界 1-50`
        // 放在 `完美世界` 目录下），都属于父目录这本书。
        let belongs_to_parent = group.iter().all(|range| {
            range.context.is_empty() || range_context_matches_parent(parent_name, &range.context)
        });
        if belongs_to_parent {
            // 单个目录天然「有序」；多个目录才需要确认区间不重叠、不倒退。
            if group.len() > 1 && !is_sequential_range_group(&group) {
                continue;
            }
            parent_groups.push(ChapterRangeMergeGroup {
                indices: indices.clone(),
                title: None,
                merge_into_parent: true,
            });
            continue;
        }

        if !is_sequential_range_group(&group) {
            continue;
        }

        // 目录名里带了别的书名，说明这是一本书被拆到了若干区间目录
        // （如「甲书 1-50」「甲书 51-100」）。保留各自的 context 作为书名，
        // 便于后续合并成一本。
        let context = group[0].context.as_str();
        let title = group
            .iter()
            .find_map(|range| range.context_title.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| context.to_string());
        contextual_groups.push(ChapterRangeMergeGroup {
            indices: indices.clone(),
            title: Some(title),
            merge_into_parent: false,
        });
    }

    // 同一本书的区间目录常因书名写法不一致被拆成多个 context 键。只要各组区间
    // 彼此不重叠，就可以安全地一起并进父目录。以前条件写成「恰好一个分组」，
    // 多个分组时全部丢弃：这本书只合并了一部分，剩下的区间目录变成独立书目，
    // 书名还是从区间目录名硬猜出来的。
    if parent_spans_do_not_overlap(&parent_groups, ranges) {
        contextual_groups.append(&mut parent_groups);
    }

    contextual_groups.sort_by(|left, right| {
        let left_start = left
            .indices
            .iter()
            .filter_map(|index| ranges.get(*index))
            .map(|range| range.start)
            .min()
            .unwrap_or(0);
        let right_start = right
            .indices
            .iter()
            .filter_map(|index| ranges.get(*index))
            .map(|range| range.start)
            .min()
            .unwrap_or(0);
        left_start
            .cmp(&right_start)
            .then_with(|| left.title.cmp(&right.title))
    });

    contextual_groups
}

/// 多个「并入父目录」的分组是否可以安全地一起并进去。
///
/// 只要各组的区间彼此不重叠，答案就是可以：它们描述的都是父目录这本书里
/// 不同区间的集数，合并后章节序号依然唯一。真正危险的是区间互相重叠——
/// 那更可能是两本书被放进了同一个目录，此时保留分组、按各自的 context
/// 处理更合适。
fn parent_spans_do_not_overlap(
    groups: &[ChapterRangeMergeGroup],
    ranges: &[ChapterRangeDir],
) -> bool {
    if groups.is_empty() {
        return false;
    }
    let mut spans = groups
        .iter()
        .flat_map(|group| group.indices.iter())
        .filter_map(|index| ranges.get(*index))
        .map(|range| (range.start, range.end))
        .collect::<Vec<_>>();
    if spans.is_empty() {
        return false;
    }
    spans.sort_unstable();
    spans.windows(2).all(|pair| pair[1].0 >= pair[0].1)
}

fn is_sequential_range_group(ranges: &[ChapterRangeDir]) -> bool {
    if ranges.len() < 2 {
        return false;
    }

    let context = ranges[0].context.as_str();
    if ranges.iter().any(|range| range.context != context) {
        return false;
    }

    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| (range.start, range.end));

    for pair in sorted.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];

        if current.start <= previous.start {
            return false;
        }
        if current.end <= previous.end {
            return false;
        }
        if current.start < previous.end {
            return false;
        }
    }

    true
}

fn range_context_matches_parent(parent_name: &str, context: &str) -> bool {
    let parent_context = canonicalize_range_context(parent_name);
    !parent_context.is_empty()
        && (parent_context.contains(context) || context.contains(&parent_context))
}

fn range_regex() -> &'static Regex {
    static RANGE_RE: OnceLock<Regex> = OnceLock::new();
    RANGE_RE.get_or_init(|| Regex::new(r"(\d{1,6})\s*-\s*(\d{1,6})").unwrap())
}

fn normalize_range_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());

    for ch in name.chars() {
        match ch {
            '０'..='９' => {
                let digit = (ch as u32 - '０' as u32) as u8 + b'0';
                normalized.push(digit as char);
            }
            '－' | '‐' | '‑' | '‒' | '–' | '—' | '―' | '~' | '～' | '至' | '到' => {
                normalized.push('-');
            }
            _ => {
                for lower in ch.to_lowercase() {
                    normalized.push(lower);
                }
            }
        }
    }

    normalized
}

fn normalize_range_marks(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());

    for ch in name.chars() {
        match ch {
            '０'..='９' => {
                let digit = (ch as u32 - '０' as u32) as u8 + b'0';
                normalized.push(digit as char);
            }
            '－' | '‐' | '‑' | '‒' | '–' | '—' | '―' | '~' | '～' | '至' | '到' => {
                normalized.push('-');
            }
            _ => normalized.push(ch),
        }
    }

    normalized
}

fn canonicalize_range_context(text: &str) -> String {
    let mut normalized = normalize_range_name(text);
    for token in [
        "episodes", "episode", "chapters", "chapter", "parts", "part", "tracks", "track", "volume",
        "vol", "chap", "ep", "pt", "第", "集", "章", "回", "话", "節", "节", "卷", "部", "篇",
        "讲", "講", "播",
    ] {
        normalized = normalized.replace(token, "");
    }

    normalized
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn clean_range_context_title(text: &str) -> Option<String> {
    let mut cleaned = normalize_range_marks(text);
    for token in [
        "episodes", "episode", "chapters", "chapter", "parts", "part", "tracks", "track", "volume",
        "vol", "chap", "ep", "pt", "第", "集", "章", "回", "话", "節", "节", "卷", "部", "篇",
        "讲", "講", "播",
    ] {
        cleaned = cleaned.replace(token, "");
    }

    let cleaned = cleaned
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '-' | '_'
                        | '.'
                        | '·'
                        | ':'
                        | '：'
                        | '|'
                        | '/'
                        | '\\'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '['
                        | ']'
                        | '【'
                        | '】'
                        | '{'
                        | '}'
                        | '《'
                        | '》'
                        | '<'
                        | '>'
                )
        })
        .trim()
        .to_string();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn starts_with_numeric_range_tail(text: &str) -> bool {
    let trimmed = text.trim_start();
    let Some(after_dash) = trimmed.strip_prefix('-') else {
        return false;
    };

    after_dash
        .trim_start()
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
}

pub(crate) fn chapter_title_template_preserves_raw(template: Option<&str>) -> bool {
    let Some(template) = template.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };

    let normalized = template.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "raw" | "preserve-raw" | "preserve_raw" | "no-clean" | "no_clean"
    ) || normalized.starts_with("raw:")
        || normalized.starts_with("preserve-raw:")
        || normalized.starts_with("preserve_raw:")
        || normalized.starts_with("no-clean:")
        || normalized.starts_with("no_clean:")
}

fn strip_raw_chapter_title_template_marker(template: &str) -> &str {
    let trimmed = template.trim();
    let normalized = trimmed.to_ascii_lowercase();

    for prefix in [
        "raw:",
        "preserve-raw:",
        "preserve_raw:",
        "no-clean:",
        "no_clean:",
    ] {
        if normalized.starts_with(prefix) {
            return trimmed[prefix.len()..].trim();
        }
    }

    if matches!(
        normalized.as_str(),
        "raw" | "preserve-raw" | "preserve_raw" | "no-clean" | "no_clean"
    ) {
        return "{chapter_title}";
    }

    trimmed
}

pub(crate) fn strip_likely_file_extension(value: &str) -> &str {
    if let Some(idx) = value.rfind('.') {
        let ext = &value[idx + 1..];
        if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return &value[..idx];
        }
    }

    value
}

pub(crate) fn clean_or_preserve_chapter_title(
    text_cleaner: &crate::core::books::text_cleaner::TextCleaner,
    title: &str,
    book_title: Option<&str>,
    preserve_raw: bool,
) -> (String, bool) {
    if preserve_raw {
        let (_, is_extra) = text_cleaner.clean_chapter_title(title, book_title);
        return (
            strip_likely_file_extension(title).trim().to_string(),
            is_extra,
        );
    }

    text_cleaner.clean_chapter_title(title, book_title)
}

pub(crate) fn resolve_chapter_title(
    cleaner: &crate::core::books::text_cleaner::TextCleaner,
    raw: &str,
    book_title: Option<&str>,
    ai_title: Option<&str>,
    clean: bool,
    preserve_raw: bool,
    extract_extra: bool,
) -> (String, bool) {
    let (title, extra) = if let Some(ai_title) = ai_title {
        let (_, extra) = cleaner.clean_chapter_title(ai_title, book_title);
        (ai_title.to_string(), extra)
    } else if clean {
        clean_or_preserve_chapter_title(cleaner, raw, book_title, preserve_raw)
    } else {
        let (_, extra) = cleaner.clean_chapter_title(raw, book_title);
        (raw.to_string(), extra)
    };
    (title, extract_extra && extra)
}

pub(crate) fn apply_chapter_title_template(
    template: Option<&str>,
    book_title: Option<&str>,
    chapter_number: i32,
    chapter_title: &str,
) -> String {
    let Some(template) = template.map(str::trim).filter(|value| !value.is_empty()) else {
        return chapter_title.to_string();
    };
    let template = strip_raw_chapter_title_template_marker(template);
    if template.is_empty() {
        return chapter_title.to_string();
    }

    let normalized = template
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    let template = match normalized.as_str() {
        "chapter-title" | "chapter-name" => "{chapter_title}",
        "book-chapter-title" | "book-chapter-number-chapter-title" => {
            "{book_title}-{chapter_number}-{chapter_title}"
        }
        "chapter-number-chapter-title" => "{chapter_number}-{chapter_title}",
        _ if template == "章节名" => "{chapter_title}",
        _ if template == "书名-章节号-章节名" => {
            "{book_title}-{chapter_number}-{chapter_title}"
        }
        _ if template == "章节号-章节名" => "{chapter_number}-{chapter_title}",
        _ => template,
    };

    let number = chapter_number.max(0).to_string();
    let padded_number = format!("{:04}", chapter_number.max(0));
    let book_title = book_title.unwrap_or("").trim();
    let chapter_title = chapter_title.trim();

    let rendered = template
        .replace("{book}", book_title)
        .replace("{book_title}", book_title)
        .replace("{title}", chapter_title)
        .replace("{chapter_title}", chapter_title)
        .replace("{chapter_name}", chapter_title)
        .replace("{chapter}", chapter_title)
        .replace("{chapter_number_padded}", &padded_number)
        .replace("{chapter_index_padded}", &padded_number)
        .replace("{chapter_number}", &number)
        .replace("{chapter_index}", &number);

    rendered
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(ch, '-' | '_' | '.' | '·' | ':' | '：' | '|' | '/' | '\\')
        })
        .trim()
        .to_string()
}

#[cfg(test)]
#[path = "../../../tests/unit/core/library_scanner/shared.rs"]
mod tests;

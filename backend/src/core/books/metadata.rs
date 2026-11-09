//! Book sidecar metadata: JSON/NFO conversion, persistence and remote synchronization.
//!
//! Callers supply the changed scope; field preservation, relation ordering and write
//! settings are applied here for scans, manual edits and scraper results alike.

use crate::core::StorageService;
use crate::core::app::error::{AccessErrorKind, CodecErrorKind, InputErrorKind};
use crate::core::app::error::{Result, TingError};
use crate::core::books::nfo_manager::{BookMetadata, NfoManager};
use crate::db::models::Chapter;
use crate::db::models::{Book, Library, ScraperConfig};
use crate::db::repository::{ChapterRepository, LibraryRepository, SeriesRepository};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AudiobookshelfChapter {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AudiobookshelfMetadata {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<AudiobookshelfChapter>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub series: Vec<String>,
    pub genres: Vec<String>,
    pub published_year: Option<String>,
    pub published_date: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub abridged: bool,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Normalized fields acquired from sidecars, embedded tags or scraper results.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub(crate) struct BookMetadataFields {
    pub(crate) title: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) narrator: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) tags: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) cover_url: Option<String>,
    pub(crate) subtitle: Option<String>,
    pub(crate) published_year: Option<String>,
    pub(crate) published_date: Option<String>,
    pub(crate) publisher: Option<String>,
    pub(crate) isbn: Option<String>,
    pub(crate) asin: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) explicit: bool,
    pub(crate) abridged: bool,
    pub(crate) json_tags: Vec<String>,
    pub(crate) json_series: Vec<String>,
    pub(crate) json_chapters: Option<Vec<crate::core::books::metadata::AudiobookshelfChapter>>,
    pub(crate) chapter_title_template: Option<String>,
    pub(crate) chapter_titles: Vec<String>,
}

impl BookMetadataFields {
    pub(crate) fn merge(&mut self, other: BookMetadataFields) {
        if let Some(t) = other.title
            && !t.trim().is_empty()
        {
            self.title = Some(t);
        }
        if other.author.is_some() {
            self.author = other.author;
        }
        if other.narrator.is_some() {
            self.narrator = other.narrator;
        }
        if other.description.is_some() {
            self.description = other.description;
        }
        if other.tags.is_some() {
            self.tags = other.tags;
        }
        if other.genre.is_some() {
            self.genre = other.genre;
        }
        if let Some(c) = other.cover_url
            && !c.trim().is_empty()
        {
            self.cover_url = Some(c);
        }
        if other.subtitle.is_some() {
            self.subtitle = other.subtitle;
        }
        if other.published_year.is_some() {
            self.published_year = other.published_year;
        }
        if other.published_date.is_some() {
            self.published_date = other.published_date;
        }
        if other.publisher.is_some() {
            self.publisher = other.publisher;
        }
        if other.isbn.is_some() {
            self.isbn = other.isbn;
        }
        if other.asin.is_some() {
            self.asin = other.asin;
        }
        if other.language.is_some() {
            self.language = other.language;
        }
        if other.explicit {
            self.explicit = true;
        }
        if other.abridged {
            self.abridged = true;
        }
        if !other.json_tags.is_empty() {
            self.json_tags = other.json_tags;
        }
        if !other.json_series.is_empty() {
            self.json_series = other.json_series;
        }
        if other.json_chapters.is_some() {
            self.json_chapters = other.json_chapters;
        }
        if other.chapter_title_template.is_some() {
            self.chapter_title_template = other.chapter_title_template;
        }
        if !other.chapter_titles.is_empty() {
            self.chapter_titles = other.chapter_titles;
        }
    }
}

impl From<BookMetadata> for BookMetadataFields {
    fn from(metadata: BookMetadata) -> Self {
        Self {
            title: (!metadata.title.is_empty()).then_some(metadata.title),
            author: metadata.author,
            narrator: metadata.narrator,
            subtitle: metadata.subtitle,
            description: metadata.intro,
            tags: Some(metadata.tags.items.join(",")),
            genre: Some(metadata.genre.items.join(",")),
            cover_url: metadata.cover_url,
            ..Default::default()
        }
    }
}

impl From<AudiobookshelfMetadata> for BookMetadataFields {
    fn from(metadata: AudiobookshelfMetadata) -> Self {
        let cover_url = ["cover", "coverUrl", "cover_url"].iter().find_map(|field| {
            metadata
                .extra
                .get(*field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        });
        Self {
            title: metadata.title,
            author: metadata.authors.into_iter().next(),
            narrator: metadata.narrators.into_iter().next(),
            description: metadata.description,
            tags: (!metadata.tags.is_empty()).then(|| metadata.tags.join(",")),
            genre: (!metadata.genres.is_empty()).then(|| metadata.genres.join(",")),
            cover_url,
            subtitle: metadata.subtitle,
            published_year: metadata.published_year,
            published_date: metadata.published_date,
            publisher: metadata.publisher,
            isbn: metadata.isbn,
            asin: metadata.asin,
            language: metadata.language,
            explicit: metadata.explicit,
            abridged: metadata.abridged,
            json_tags: metadata.tags,
            json_series: metadata.series,
            json_chapters: (!metadata.chapters.is_empty()).then_some(metadata.chapters),
            ..Default::default()
        }
    }
}

/// Optional fields distinguish an unselected scrape field from an explicit false.
#[derive(Debug, Clone, Default)]
pub struct MetadataPatch {
    pub subtitle: Option<String>,
    pub published_year: Option<String>,
    pub published_date: Option<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
    pub explicit: Option<bool>,
    pub abridged: Option<bool>,
    pub duration: Option<u64>,
}

impl MetadataPatch {
    fn apply(&self, metadata: &mut AudiobookshelfMetadata) {
        macro_rules! update {
            ($($field:ident),+ $(,)?) => {
                $(if self.$field.is_some() {
                    metadata.$field = self.$field.clone();
                })+
            };
        }
        update!(
            subtitle,
            published_year,
            published_date,
            publisher,
            isbn,
            asin,
            language
        );
        if let Some(value) = self.explicit {
            metadata.explicit = value;
        }
        if let Some(value) = self.abridged {
            metadata.abridged = value;
        }
        if let Some(value) = self.duration {
            metadata.extra.insert("duration".into(), value.into());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataChange {
    Book,
    Chapters,
    Series,
}

pub struct MetadataRemote<'a> {
    pub storage: &'a StorageService,
    pub key: &'a [u8; 32],
}

/// One persistence policy for manual edits, scrape results and scanning.
/// Repositories and transport are borrowed; this service owns no tasks or cache.
pub struct BookMetadataService<'a> {
    pub chapters: &'a ChapterRepository,
    pub series: &'a SeriesRepository,
    pub libraries: &'a LibraryRepository,
    pub nfo: &'a NfoManager,
    pub remote: Option<MetadataRemote<'a>>,
}

impl BookMetadataService<'_> {
    /// Sidecar declarations win over directory inference.
    ///
    /// Once `metadata.json` states the series, a guess derived from folder
    /// names must not keep the same book in a second collection.
    async fn release_inferred_series(&self, book: &Book, declarations: &[String]) -> Result<()> {
        if declarations.iter().all(|value| value.trim().is_empty()) {
            return Ok(());
        }
        let mut keep = Vec::new();
        for declaration in declarations {
            let Some((title, _)) =
                crate::core::books::series_title::parse_series_declaration(declaration)
            else {
                continue;
            };
            if let Some(series) = self
                .series
                .find_by_title_and_library(&title, &book.library_id)
                .await?
            {
                keep.push(series.id);
            }
        }
        self.series
            .release_inferred_memberships(&book.library_id, &book.id, keep)
            .await
    }

    /// Import sidecar series declarations with identical behavior for local and
    /// remote scans. A manually owned book keeps its existing relationships.
    pub async fn import_series(&self, book: &Book, declarations: &[String]) -> Result<()> {
        if book.manual_corrected != 0 {
            return Ok(());
        }
        self.release_inferred_series(book, declarations).await?;
        for declaration in declarations {
            let Some((title, explicit_order)) =
                crate::core::books::series_title::parse_series_declaration(declaration)
            else {
                continue;
            };
            let series = self
                .series
                .find_or_create_by_title(crate::db::models::Series {
                    id: uuid::Uuid::new_v4().to_string(),
                    library_id: book.library_id.clone(),
                    title,
                    author: book.author.clone(),
                    narrator: book.narrator.clone(),
                    cover_url: book.cover_url.clone(),
                    description: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                })
                .await?;
            let books = self.series.find_books_by_series(&series.id).await?;
            let current_order = books
                .iter()
                .find(|(candidate, _)| candidate.id == book.id)
                .map(|(_, order)| *order);
            if let Some(current) = current_order
                && explicit_order.is_none_or(|order| order == current)
            {
                continue;
            }
            self.series
                .add_book(crate::db::models::SeriesBook {
                    series_id: series.id.clone(),
                    book_id: book.id.clone(),
                    book_order: explicit_order.unwrap_or(books.len() as i32 + 1),
                })
                .await?;
            if current_order.is_none() && explicit_order.is_none() {
                let mut books = self.series.find_books_by_series(&series.id).await?;
                books.sort_by(|a, b| {
                    natord::compare(
                        a.0.title.as_deref().unwrap_or_default(),
                        b.0.title.as_deref().unwrap_or_default(),
                    )
                    .then_with(|| a.0.id.cmp(&b.0.id))
                });
                let orders = books
                    .into_iter()
                    .enumerate()
                    .map(|(index, (book, _))| (book.id, index as i32 + 1))
                    .collect();
                self.series.update_book_orders(&series.id, orders).await?;
            }
        }
        Ok(())
    }

    pub async fn series_titles(&self, book_id: &str) -> Result<Vec<String>> {
        let mut titles = Vec::new();
        for series in self.series.find_series_by_book(book_id).await? {
            let books = self.series.find_books_by_series(&series.id).await?;
            let title = books
                .iter()
                .find(|(book, _)| book.id == book_id)
                .map(|(_, order)| format!("{} #{}", series.title, order))
                .unwrap_or(series.title);
            if !titles.contains(&title) {
                titles.push(title);
            }
        }
        Ok(titles)
    }

    pub async fn snapshot(
        &self,
        book: &Book,
        dir: &Path,
        change: MetadataChange,
        patch: &MetadataPatch,
    ) -> Result<AudiobookshelfMetadata> {
        // A corrupt or unreadable sidecar must survive, rather than being
        // overwritten with defaults and silently losing its extended fields.
        let mut metadata = match read_metadata_json(dir)? {
            Some(metadata) => metadata,
            None if dir.join("book.nfo").is_file() => {
                AudiobookshelfMetadata::from_nfo(self.nfo.read_book_nfo(&dir.join("book.nfo"))?)
            }
            None => AudiobookshelfMetadata::default(),
        };
        if change == MetadataChange::Book {
            metadata.update_book_fields(book);
        }
        if change != MetadataChange::Series {
            metadata.chapters =
                build_audiobookshelf_chapters(self.chapters.find_by_book(&book.id).await?);
        }
        metadata.series = self.series_titles(&book.id).await?;
        patch.apply(&mut metadata);
        Ok(metadata)
    }

    pub async fn sync(
        &self,
        book: &Book,
        change: MetadataChange,
        patch: &MetadataPatch,
    ) -> Result<()> {
        let Some(library) = self.libraries.find_by_id(&book.library_id).await? else {
            return Ok(());
        };
        let config: ScraperConfig = library
            .scraper_config
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default();
        match library.library_type.as_str() {
            "webdav" => {
                let dir = remote_metadata_dir(&book.path)?;
                let metadata = self
                    .snapshot(book, &dir, MetadataChange::Book, patch)
                    .await?;
                write_cached_sidecars(book, &metadata, self.nfo)?;
                if let Some(remote) = &self.remote {
                    sync_to_remote(remote.storage, &library, book, remote.key).await?;
                }
            }
            "local" => {
                let write_json = config.metadata_writing_enabled;
                let write_nfo = change == MetadataChange::Book && config.nfo_writing_enabled;
                if !write_json && !write_nfo {
                    return Ok(());
                }
                let dir = Path::new(&book.path);
                if change == MetadataChange::Series && !dir.join("metadata.json").is_file() {
                    return Ok(());
                }
                let metadata = self.snapshot(book, dir, change, patch).await?;
                if write_nfo && dir.join("book.nfo").is_file() {
                    self.nfo.read_book_nfo(&dir.join("book.nfo"))?;
                }
                if write_json {
                    write_metadata_json(dir, &metadata)?;
                }
                if write_nfo {
                    write_nfo_sidecar(book, &metadata, self.nfo, dir, book.cover_url.clone())?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Scans fill the remote cache without uploading anything to the drive.
    pub async fn ensure_remote_cache(&self, book: &Book) -> Result<()> {
        let dir = remote_metadata_dir(&book.path)?;
        if !dir.join("metadata.json").is_file() || !dir.join("book.nfo").is_file() {
            let metadata = self
                .snapshot(book, &dir, MetadataChange::Book, &MetadataPatch::default())
                .await?;
            ensure_cached_sidecars(book, &metadata, self.nfo)?;
        }
        Ok(())
    }
}

impl AudiobookshelfMetadata {
    pub fn from_nfo(nfo: BookMetadata) -> Self {
        Self {
            title: (!nfo.title.is_empty()).then_some(nfo.title),
            subtitle: nfo.subtitle,
            authors: nfo.author.into_iter().collect(),
            narrators: nfo.narrator.into_iter().collect(),
            description: nfo.intro,
            tags: nfo.tags.items,
            genres: nfo.genre.items,
            extra: nfo
                .total_duration
                .map(|duration| serde_json::Map::from_iter([("duration".into(), duration.into())]))
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    pub fn update_book_fields(&mut self, book: &crate::db::models::Book) {
        self.title = book.title.clone();
        self.authors = book
            .author
            .clone()
            .map(|value| vec![value])
            .unwrap_or_default();
        self.narrators = book
            .narrator
            .clone()
            .map(|value| vec![value])
            .unwrap_or_default();
        self.description = book.description.clone();
        let split_values = |value: Option<&str>| -> Vec<String> {
            value
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        };
        self.tags = split_values(book.tags.as_deref());
        self.genres = split_values(book.genre.as_deref());
        self.published_year = book
            .year
            .map(|year| year.to_string())
            .or(self.published_year.take());
    }
}

/// Align sidecar chapters with the scanned files when every file stem has an
/// exact chapter-title match. Older Ting Reader versions could write main and
/// extra chapters interleaved by their independent display indexes, so their
/// array positions are not reliable.
pub fn align_chapters_to_file_stems(
    chapters: Vec<AudiobookshelfChapter>,
    file_stems: &[String],
) -> (Vec<AudiobookshelfChapter>, bool) {
    if chapters.len() != file_stems.len() || chapters.is_empty() {
        return (chapters, false);
    }

    let mut indexes_by_title: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, chapter) in chapters.iter().enumerate() {
        indexes_by_title
            .entry(chapter_match_key(&chapter.title))
            .or_default()
            .push_back(index);
    }

    let mut matched_indexes = Vec::with_capacity(file_stems.len());
    for file_stem in file_stems {
        let Some(index) = indexes_by_title
            .get_mut(&chapter_match_key(file_stem))
            .and_then(VecDeque::pop_front)
        else {
            return (chapters, false);
        };
        matched_indexes.push(index);
    }

    let aligned = matched_indexes
        .into_iter()
        .map(|index| chapters[index].clone())
        .collect();
    (aligned, true)
}

/// Audiobookshelf chapter offsets must follow media-file order. Main and extra
/// chapter indexes are separate UI sequences and cannot be used for this.
pub fn build_audiobookshelf_chapters(mut chapters: Vec<Chapter>) -> Vec<AudiobookshelfChapter> {
    chapters.sort_by_cached_key(|chapter| {
        (
            super::text_cleaner::chapter_order_key(&chapter.path),
            chapter.id.clone(),
        )
    });

    let mut current_time = 0.0;
    chapters
        .into_iter()
        .enumerate()
        .map(|(index, chapter)| {
            let duration = chapter.duration.unwrap_or(0).max(0) as f64;
            let result = AudiobookshelfChapter {
                id: index as u32,
                start: current_time,
                end: current_time + duration,
                title: chapter.title.unwrap_or_default(),
            };
            current_time += duration;
            result
        })
        .collect()
}

fn chapter_match_key(value: &str) -> String {
    value.trim().to_lowercase()
}

pub fn remote_metadata_dir(book_path: &str) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let hash = format!("{:x}", Sha256::digest(book_path.as_bytes()));
    Ok(std::env::current_dir()?.join("temp").join(hash))
}

pub fn write_metadata_json(dir: &Path, metadata: &AudiobookshelfMetadata) -> Result<()> {
    let path = dir.join("metadata.json");
    let file = std::fs::File::create(&path).map_err(TingError::IoError)?;
    serde_json::to_writer_pretty(file, metadata).map_err(|e| {
        let message = e.to_string();
        TingError::serialization(
            CodecErrorKind::InvalidData,
            "write_metadata_json::serialization",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })?;
    tracing::info!(
        target: "audit::metadata",
        message_key = "metadata.json.write_succeeded",
        message_params = %serde_json::json!({ "path": dir.display().to_string() }),
        path = %dir.display(),
        "Metadata JSON written"
    );
    Ok(())
}

pub fn read_metadata_json(dir: &Path) -> Result<Option<AudiobookshelfMetadata>> {
    let path = dir.join("metadata.json");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata: AudiobookshelfMetadata = serde_json::from_reader(file).map_err(|e| {
        let message = e.to_string();
        TingError::deserialization(
            CodecErrorKind::InvalidData,
            "read_metadata_json::deserialization",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })?;
    Ok(Some(metadata))
}

pub fn is_cover_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://") || value.starts_with("//")
}

/// Download completely before replacing a cached file, so a failed scan retains it.
pub async fn cache_remote_file(
    storage: &StorageService,
    library: &Library,
    remote_url: &str,
    target: &Path,
    key: &[u8; 32],
    limit: u64,
) -> Result<()> {
    let (reader, length) = storage
        .get_webdav_reader(library, remote_url, None, key)
        .await?;
    if length > limit {
        return Err(TingError::invalid_request(
            InputErrorKind::OutOfRange,
            "cache_remote_file::invalid_request",
            "WebDAV metadata file is too large".into(),
        ));
    }
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit || (length > 0 && bytes.len() as u64 != length) {
        return Err(TingError::invalid_request(
            InputErrorKind::InvalidValue,
            "cache_remote_file::invalid_request",
            "Incomplete or oversized WebDAV metadata file".into(),
        ));
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let staging = target.with_file_name(format!(".metadata-{}.tmp", uuid::Uuid::new_v4()));
    let result = async {
        tokio::fs::write(&staging, bytes).await?;
        tokio::fs::rename(&staging, target).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(error.into());
    }
    Ok(())
}

pub fn write_cached_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
) -> Result<PathBuf> {
    write_sidecars(book, metadata, nfo_manager, false)
}

pub fn ensure_cached_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
) -> Result<PathBuf> {
    write_sidecars(book, metadata, nfo_manager, true)
}

fn write_sidecars(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
    only_missing: bool,
) -> Result<PathBuf> {
    let dir = remote_metadata_dir(&book.path)?;
    std::fs::create_dir_all(&dir)?;
    if !only_missing && dir.join("book.nfo").is_file() {
        nfo_manager.read_book_nfo(&dir.join("book.nfo"))?;
    }
    let cover = portable_cover(book, &dir)?;
    if !only_missing || !dir.join("metadata.json").is_file() {
        let mut metadata = metadata.clone();
        for field in ["cover", "coverUrl", "cover_url"] {
            if metadata.extra.contains_key(field) {
                metadata
                    .extra
                    .insert(field.to_string(), serde_json::json!(cover));
            }
        }
        write_metadata_json(&dir, &metadata)?;
    }
    if only_missing && dir.join("book.nfo").is_file() {
        return Ok(dir);
    }
    write_nfo_sidecar(book, metadata, nfo_manager, &dir, cover)?;
    Ok(dir)
}

fn write_nfo_sidecar(
    book: &Book,
    metadata: &AudiobookshelfMetadata,
    nfo_manager: &NfoManager,
    dir: &Path,
    cover: Option<String>,
) -> Result<()> {
    let nfo_path = dir.join("book.nfo");
    let mut nfo = if nfo_path.is_file() {
        nfo_manager.read_book_nfo(&nfo_path)?
    } else {
        BookMetadata::new(
            book.title.clone().unwrap_or_default(),
            "ting-reader".into(),
            book.id.clone(),
            0,
        )
    };
    nfo.title = book.title.clone().unwrap_or_default();
    nfo.author = book.author.clone();
    nfo.narrator = book.narrator.clone();
    nfo.intro = book.description.clone();
    nfo.subtitle = metadata.subtitle.clone();
    nfo.tags.items = metadata.tags.clone();
    nfo.genre.items = metadata.genres.clone();
    nfo.chapter_count = metadata.chapters.len() as u32;
    nfo.total_duration = metadata
        .extra
        .get("duration")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            metadata
                .chapters
                .last()
                .map(|chapter| chapter.end.max(0.0).round() as u64)
        });
    nfo.cover_url = cover;
    nfo.touch();
    nfo_manager.write_book_nfo_to_dir(dir, &nfo)?;
    Ok(())
}

/// File covers must belong to this book's cache. URL covers remain references.
fn portable_cover(book: &Book, dir: &Path) -> Result<Option<String>> {
    let Some(cover) = book.cover_url.as_deref().filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if is_cover_url(cover) {
        return Ok(Some(cover.to_string()));
    }
    let path = Path::new(cover);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    if !path.is_file() {
        return Ok(None);
    }
    let path = path.canonicalize()?;
    if path.parent() != Some(dir.canonicalize()?.as_path()) {
        return Err(TingError::permission(
            AccessErrorKind::ResourceDenied,
            "portable_cover::permission",
            "Cover file is outside the book metadata cache".into(),
        ));
    }
    Ok(path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned))
}

pub async fn sync_to_remote(
    storage: &StorageService,
    library: &Library,
    book: &Book,
    key: &[u8; 32],
) -> Result<()> {
    let config: ScraperConfig = library
        .scraper_config
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    if library.library_type != "webdav" || !config.webdav_metadata_writing_enabled {
        return Ok(());
    }
    let dir = remote_metadata_dir(&book.path)?;
    // Upload a file cover first, then the sidecars that reference it. Switching
    // to a URL neither downloads that URL nor removes the old remote image.
    if let Some(cover) = portable_cover(book, &dir)?
        && !is_cover_url(&cover)
    {
        storage
            .put_webdav_sidecar(
                library,
                &book.path,
                &cover,
                tokio::fs::read(dir.join(&cover)).await?,
                key,
            )
            .await?;
    }
    for filename in ["metadata.json", "book.nfo"] {
        storage
            .put_webdav_sidecar(
                library,
                &book.path,
                filename,
                tokio::fs::read(dir.join(filename)).await?,
                key,
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/core/books/metadata.rs"]
mod tests;

//! Series titles arrive from three places: directory inference, sidecar
//! declarations and the admin UI. They must all agree on what counts as "the
//! same series" — otherwise one collection is stored as several rows with
//! overlapping members.

use super::text_cleaner::parse_ordinal;
use regex::Regex;
use std::sync::OnceLock;

/// What a series name means: the title to show and the identity to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeriesIdentity {
    pub title: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSeriesTitle {
    pub base_title: Option<String>,
    pub order: Option<i32>,
}

/// Split a sidecar declaration such as `Saga #3` into title and explicit order.
pub(crate) fn parse_series_declaration(declaration: &str) -> Option<(String, Option<i32>)> {
    let declaration = declaration.trim();
    if declaration.is_empty() {
        return None;
    }
    let (title, order) = declaration
        .rsplit_once(" #")
        .and_then(|(title, order)| {
            order
                .trim()
                .parse::<i32>()
                .ok()
                .map(|order| (title.trim(), Some(order)))
        })
        .unwrap_or((declaration, None));
    (!title.is_empty()).then(|| (title.to_string(), order))
}

/// Canonical title and identity key for a series name.
///
/// This is deliberately a pure normalization: callers that hold a directory
/// name strip the author suffix before calling. `斗破苍穹 第2卷`, `斗破苍穹`
/// and `斗破苍穹 S02` share one identity, so whichever source writes last
/// lands on the same row.
pub(crate) fn series_identity(name: &str) -> SeriesIdentity {
    let normalized = normalize_series_name(name);
    let cleaned = clean_series_base_title(&normalized);
    let title = parse_series_title(&cleaned)
        .and_then(|parsed| parsed.base_title)
        .filter(|value| !value.is_empty())
        .unwrap_or(cleaned);
    // A name made only of separators still needs a stable identity; fall back
    // to the raw text rather than collapsing everything into one empty key.
    let title = if title.is_empty() {
        normalized.trim().to_string()
    } else {
        title
    };
    SeriesIdentity {
        key: canonicalize_series_title_key(&title),
        title,
    }
}

/// Split a series name into its base title and the volume number it carries.
///
/// Input is expected to be a title, not a full directory name — callers strip
/// author/narrator suffixes first.
pub(crate) fn parse_series_title(name: &str) -> Option<ParsedSeriesTitle> {
    let normalized = normalize_series_name(name);

    if let Some(caps) = series_volume_regex().captures(&normalized) {
        return Some(ParsedSeriesTitle {
            base_title: caps
                .name("base")
                .map(|m| clean_series_base_title(m.as_str()))
                .filter(|value| !value.is_empty()),
            order: caps
                .name("num2")
                .or_else(|| caps.name("num"))
                .and_then(|m| parse_series_number(m.as_str())),
        });
    }

    if let Some(caps) = series_s_regex().captures(&normalized) {
        return Some(ParsedSeriesTitle {
            base_title: caps
                .name("base")
                .map(|m| clean_series_base_title(m.as_str()))
                .filter(|value| !value.is_empty()),
            order: caps
                .name("num")
                .and_then(|m| parse_series_number(m.as_str())),
        });
    }

    if let Some(caps) = series_word_regex().captures(&normalized) {
        return Some(ParsedSeriesTitle {
            base_title: caps
                .name("base")
                .map(|m| clean_series_base_title(m.as_str()))
                .filter(|value| !value.is_empty()),
            order: caps
                .name("num")
                .and_then(|m| parse_series_number(m.as_str())),
        });
    }

    if let Some((base, suffix)) = normalized.split_once('之') {
        let base = clean_series_base_title(base);
        if !base.is_empty() && !suffix.trim().is_empty() {
            return Some(ParsedSeriesTitle {
                base_title: Some(base),
                order: parse_leading_series_number(suffix),
            });
        }
    }

    None
}

/// Fold full-width digits and punctuation so look-alike names compare equal.
pub(crate) fn normalize_series_name(name: &str) -> String {
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

pub(crate) fn clean_series_base_title(value: &str) -> String {
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

/// Identity key stored in `series.title_key`.
///
/// Separators that people type inconsistently are dropped so `Saga-Part 2`,
/// `Saga Part 2` and `Saga：Part 2` share one series.
pub(crate) fn canonicalize_series_title_key(value: &str) -> String {
    let key: String = normalize_series_name(value)
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '-' | '_' | '.' | '·' | ':' | '：'))
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    if key.is_empty() {
        value.trim().to_lowercase()
    } else {
        key
    }
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

#[cfg(test)]
mod tests {
    use super::{canonicalize_series_title_key, parse_series_declaration, series_identity};

    #[test]
    fn volume_suffixes_share_one_identity() {
        for name in [
            "斗破苍穹",
            "斗破苍穹 第2卷",
            "斗破苍穹 第二季",
            "斗破苍穹 S02",
        ] {
            let identity = series_identity(name);
            assert_eq!(identity.title, "斗破苍穹", "{name}");
            assert_eq!(identity.key, canonicalize_series_title_key("斗破苍穹"));
        }
    }

    #[test]
    fn separators_do_not_split_a_series() {
        let base = series_identity("Saga Part 2").key;
        for name in ["Saga：Part 2", "Saga·Part 2", "Saga_Part 2", "Saga.Part 2"] {
            assert_eq!(series_identity(name).key, base, "{name}");
        }
    }

    #[test]
    fn plain_titles_keep_their_display_form() {
        let identity = series_identity("  三体  ");
        assert_eq!(identity.title, "三体");
        assert_eq!(identity.key, "三体");
    }

    #[test]
    fn declarations_split_their_explicit_order() {
        assert_eq!(
            parse_series_declaration("Saga #3"),
            Some(("Saga".to_string(), Some(3)))
        );
        assert_eq!(
            parse_series_declaration("  Saga  "),
            Some(("Saga".to_string(), None))
        );
        assert_eq!(parse_series_declaration("   "), None);
    }
}

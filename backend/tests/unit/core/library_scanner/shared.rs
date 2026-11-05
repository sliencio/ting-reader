use super::*;

#[test]
fn respects_configured_book_name_layouts() {
    use crate::core::books::text_cleaner::{BookName, BookNameLayout};
    let name = "示例书名 - 张三 - 李四";
    for (layout, title, author, narrator) in [
        (BookNameLayout::Auto, "示例书名", Some("张三"), Some("李四")),
        (
            BookNameLayout::TitleAuthorNarrator,
            "示例书名",
            Some("张三"),
            Some("李四"),
        ),
        (
            BookNameLayout::TitleNarratorAuthor,
            "示例书名",
            Some("李四"),
            Some("张三"),
        ),
        (BookNameLayout::TitleAuthor, "示例书名", Some("张三"), None),
        (
            BookNameLayout::TitleNarrator,
            "示例书名",
            None,
            Some("张三"),
        ),
        (
            BookNameLayout::TitleOnly,
            "示例书名 - 张三 - 李四",
            None,
            None,
        ),
    ] {
        let parsed = BookName::parse_with_layout(name, layout, "");
        assert_eq!(
            (parsed.title, parsed.author, parsed.narrator),
            (title, author, narrator),
            "{layout:?}"
        );
    }

    // 带标签的写法始终按标签识别，不受位置样式影响。
    let labelled = BookName::parse_with_layout(
        "示例书名 - 演播：李四 - 作者：张三",
        BookNameLayout::TitleAuthorNarrator,
        "",
    );
    assert_eq!(labelled.author, Some("张三"));
    assert_eq!(labelled.narrator, Some("李四"));

    // 自定义正则：命名分组优先，其次按位置。
    // 这条正则的顺序是「书名_演播_作者」，所以李四是演播、张三是作者。
    let named = BookName::parse_with_layout(
        "示例书名_张三_李四",
        BookNameLayout::Custom,
        r"^(?P<title>[^_]+)_(?P<narrator>[^_]+)_(?P<author>[^_]+)$",
    );
    assert_eq!(
        (named.title, named.author, named.narrator),
        ("示例书名", Some("李四"), Some("张三"))
    );
    let positional = BookName::parse_with_layout(
        "示例书名_张三_李四",
        BookNameLayout::Custom,
        r"^([^_]+)_([^_]+)_([^_]+)$",
    );
    assert_eq!(
        (positional.title, positional.author, positional.narrator),
        ("示例书名", Some("张三"), Some("李四"))
    );
    // 正则写错时退回内置规则，而不是把整本书的名字弄丢。
    let invalid =
        BookName::parse_with_layout("示例书名-张三", BookNameLayout::Custom, "([unclosed");
    assert_eq!(invalid.title, "示例书名");
    assert_eq!(invalid.author, Some("张三"));
}

#[test]
fn parses_author_and_narrator_roles_and_empty_slots_from_book_names() {
    use crate::core::books::text_cleaner::BookName;
    for (name, title, author, narrator) in [
        (
            "示例书名-示例作者/示例演播",
            "示例书名",
            Some("示例作者"),
            Some("示例演播"),
        ),
        (
            " 示例书名 - 示例作者 ／ 示例演播 ",
            "示例书名",
            Some("示例作者"),
            Some("示例演播"),
        ),
        (
            "示例书名-示例作者-示例演播 | 宣传",
            "示例书名",
            Some("示例作者"),
            Some("示例演播"),
        ),
        ("示例书名-示例作者", "示例书名", Some("示例作者"), None),
        ("示例书名-示例演播", "示例书名", Some("示例演播"), None),
        ("示例书名-示例作者/", "示例书名", Some("示例作者"), None),
        ("示例书名-/示例演播", "示例书名", None, Some("示例演播")),
        (
            "示例书名-作者：示例作者／演播：示例演播",
            "示例书名",
            Some("示例作者"),
            Some("示例演播"),
        ),
        (
            "示例书名-演播：示例演播",
            "示例书名",
            None,
            Some("示例演播"),
        ),
        ("示例书名 | 示例作者／示例演播", "示例书名", None, None),
        ("示例书名-2", "示例书名-2", None, None),
        ("示例书名-S01", "示例书名-S01", None, None),
        ("示例书名-第一部", "示例书名-第一部", None, None),
        (
            "示例书名-S01-示例作者",
            "示例书名-S01",
            Some("示例作者"),
            None,
        ),
        (
            "示例书名 - 第一部 - 示例作者 ／ 示例演播",
            "示例书名 - 第一部",
            Some("示例作者"),
            Some("示例演播"),
        ),
        ("示例书名", "示例书名", None, None),
    ] {
        let parsed = BookName::parse(name);
        assert_eq!(
            parsed,
            BookName {
                title,
                author,
                narrator
            },
            "{name}"
        );
    }
}

#[test]
fn chapters_sort_by_numeric_ordinals_for_local_and_encoded_webdav_names() {
    use crate::core::books::text_cleaner::{chapter_order_key, parse_ordinal};
    let cleaner = crate::core::books::TextCleaner::new(Default::default());
    for (name, expected) in [
        ("1.mp3", 1),
        ("10.mp3", 10),
        ("第十章 示例", 10),
        ("第四百七十章 示例", 470),
        ("第４７１章 示例", 471),
        ("第两百零三集 示例", 203),
        ("第〇一二章 示例", 12),
        ("第一万零一章 示例", 10001),
        ("Chapter 21 Sample", 21),
        ("0 开场", 0),
    ] {
        assert_eq!(
            cleaner.extract_chapter_number(name),
            Some(expected),
            "{name}"
        );
    }
    assert!(parse_ordinal("99999999999999999999999").is_none());
    assert!(parse_ordinal("千万万万万").is_none());
    assert!(parse_ordinal("随机文字").is_none());
    for (files, expected) in [
        (
            vec!["10.mp3", "2.mp3", "1.mp3", "4.mp3", "3.mp3"],
            vec!["1.mp3", "2.mp3", "3.mp3", "4.mp3", "10.mp3"],
        ),
        (
            vec![
                "第四百七十章 示例.mp3",
                "第二章 示例.mp3",
                "第十章 示例.mp3",
                "第一章 示例.mp3",
            ],
            vec![
                "第一章 示例.mp3",
                "第二章 示例.mp3",
                "第十章 示例.mp3",
                "第四百七十章 示例.mp3",
            ],
        ),
        (
            vec!["第十章 示例.mp3", "2.mp3", "第一章 示例.mp3", "11.mp3"],
            vec!["第一章 示例.mp3", "2.mp3", "第十章 示例.mp3", "11.mp3"],
        ),
    ] {
        let mut local: Vec<_> = files
            .iter()
            .map(|file| format!(r"C:\books\{file}"))
            .collect();
        local.sort_by_cached_key(|path| chapter_order_key(path));
        assert_eq!(
            local,
            expected
                .iter()
                .map(|file| format!(r"C:\books\{file}"))
                .collect::<Vec<_>>()
        );
        let url_for = |file: &str| {
            let mut url = url::Url::parse("https://example.test/book/").unwrap();
            url.path_segments_mut().unwrap().push(file);
            url.to_string()
        };
        let mut remote: Vec<_> = files.iter().map(|file| url_for(file)).collect();
        remote.sort_by_cached_key(|path| chapter_order_key(path));
        assert_eq!(
            remote,
            expected
                .iter()
                .map(|file| url_for(file))
                .collect::<Vec<_>>()
        );
    }
    let mut ranges = vec!["51-100/1.mp3", "1-50/10.mp3", "1-50/2.mp3"];
    ranges.sort_by_cached_key(|path| chapter_order_key(path));
    assert_eq!(ranges, vec!["1-50/2.mp3", "1-50/10.mp3", "51-100/1.mp3"]);
}

#[test]
fn series_members_inherit_parent_people_without_misreading_volume_codes() {
    use crate::core::library_scanner::shared::directory_book_name;
    for (child, parent, title, author, narrator) in [
        (
            "示例第一部",
            "示例-作者甲／演播甲",
            "示例第一部",
            Some("作者甲"),
            Some("演播甲"),
        ),
        ("示例-S01", "示例-作者甲", "示例-S01", Some("作者甲"), None),
        (
            "S02",
            "示例-作者甲／演播甲",
            "S02",
            Some("作者甲"),
            Some("演播甲"),
        ),
        (
            "示例第一部-作者乙",
            "示例-作者甲／演播甲",
            "示例第一部",
            Some("作者乙"),
            Some("演播甲"),
        ),
        ("其他书第一部", "示例-作者甲", "其他书第一部", None, None),
        ("独立书", "合集-作者甲", "独立书", None, None),
    ] {
        let actual = directory_book_name(child, parent, &Default::default());
        assert_eq!(
            (actual.title, actual.author, actual.narrator),
            (title, author, narrator)
        );
    }
    for names in [
        ["示例第一部", "示例第二部", "示例第三部"],
        [
            "示例-第一部-作者乙",
            "示例-第二部",
            "示例-第三部-演播：演播乙",
        ],
        ["示例-S01", "示例-S02", "示例-S03"],
        ["S01", "S02", "S03"],
    ] {
        let candidates: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, name)| SeriesDirectoryCandidate {
                key: index,
                parent_key: "parent".into(),
                parent_name: "示例 - 作者甲 ／ 演播甲".into(),
                name: (*name).into(),
            })
            .collect();
        let series = infer_series_directories(&candidates);
        assert_eq!(series.len(), 3, "{names:?}");
        for index in 0..3 {
            assert_eq!(series[&index].title, "示例");
            assert_eq!(series[&index].order, index as i32 + 1);
        }
    }
}

#[test]
fn directory_titles_and_search_queries_share_the_same_name_convention() {
    use crate::core::books::text_cleaner::book_title_from_name;
    let cleaner = crate::core::books::TextCleaner::new(Default::default());
    for (directory, expected) in [
        ("书名-演播", "书名"),
        ("书名 - 作者", "书名"),
        ("书名－演播", "书名"),
        ("书名 | 宣传介绍", "书名"),
        ("书名丨作者", "书名"),
        (" 书名｜演播｜全集 ", "书名"),
        ("书名-演播 | 全集", "书名"),
        ("书名", "书名"),
        ("示例书名-2", "示例书名-2"),
        ("书名-", "书名-"),
    ] {
        assert_eq!(book_title_from_name(directory), expected);
        assert_eq!(cleaner.clean_book_directory_title(directory), expected);
    }
}

#[test]
fn shared_chapter_policy_preserves_ai_sidecar_and_raw_titles() {
    let cleaner = crate::core::books::TextCleaner::new(Default::default());
    let raw = "Original title.mp3";
    let (title, extra) =
        resolve_chapter_title(&cleaner, raw, None, Some("AI title"), true, false, false);
    assert_eq!(title, "AI title");
    assert!(!extra);
    let (title, _) = resolve_chapter_title(&cleaner, raw, None, None, false, false, true);
    assert_eq!(title, raw);
    let (title, _) = resolve_chapter_title(&cleaner, raw, None, None, true, true, true);
    assert_eq!(title, "Original title");
}

fn select_mergeable_range_group(
    parent_name: &str,
    ranges: &[ChapterRangeDir],
) -> Option<Vec<usize>> {
    let groups = select_mergeable_range_groups(parent_name, ranges);
    if groups.len() == 1 {
        Some(groups[0].indices.clone())
    } else {
        None
    }
}

mod series {

    use super::{SeriesDirectoryCandidate, infer_series_directories};

    #[test]
    fn infers_series_from_sibling_volume_directories() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/season1".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "示例书名\u{7b2c}\u{4e00}\u{5b63}".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/season2".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "示例书名\u{7b2c}\u{4e8c}\u{5b63}".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(inferred["root/book/season1"].title, "示例书名");
        assert_eq!(inferred["root/book/season1"].order, 1);
        assert_eq!(inferred["root/book/season2"].order, 2);
    }

    #[test]
    fn infers_series_from_zhi_separator() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/part1".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "测试系列\u{4e4b}第一故事".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/part2".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "测试系列\u{4e4b}第二故事".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(inferred["root/book/part1"].title, "测试系列");
        let mut orders = vec![
            inferred["root/book/part1"].order,
            inferred["root/book/part2"].order,
        ];
        orders.sort();
        assert_eq!(orders, vec![1, 2]);
    }

    #[test]
    fn infers_series_from_parent_when_only_season_code_is_present() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/s01".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "示例书名".to_string(),
                name: "S01".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/s02".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "示例书名".to_string(),
                name: "S02".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(inferred["root/book/s01"].title, "示例书名");
        assert_eq!(inferred["root/book/s01"].order, 1);
        assert_eq!(inferred["root/book/s02"].order, 2);
    }
}

mod titles_and_ranges {

    use super::{
        ChapterRangeDir, apply_chapter_title_template, chapter_title_template_preserves_raw,
        parse_chapter_range_dir_name, parse_pure_range_dir_name, select_mergeable_range_group,
        select_mergeable_range_groups, strip_likely_file_extension,
    };

    #[test]
    fn applies_builtin_chapter_title_templates() {
        assert_eq!(
            apply_chapter_title_template(Some("章节名"), Some("书名"), 7, "正文"),
            "正文"
        );
        assert_eq!(
            apply_chapter_title_template(Some("书名-章节号-章节名"), Some("书名"), 7, "正文"),
            "书名-7-正文"
        );
        assert_eq!(
            apply_chapter_title_template(
                Some("{book_title} 第{chapter_number_padded}章 {chapter_title}"),
                Some("书名"),
                7,
                "正文"
            ),
            "书名 第0007章 正文"
        );
    }

    #[test]
    fn applies_raw_chapter_title_template_marker() {
        assert!(chapter_title_template_preserves_raw(Some(
            "raw:{chapter_title}"
        )));
        assert_eq!(
            apply_chapter_title_template(
                Some("raw:{chapter_number}-{chapter_title}"),
                None,
                7,
                "001.正文"
            ),
            "7-001.正文"
        );
    }

    #[test]
    fn strips_likely_file_extension_only() {
        assert_eq!(strip_likely_file_extension("001.mp3"), "001");
        assert_eq!(strip_likely_file_extension("Chapter.1"), "Chapter");
        assert_eq!(strip_likely_file_extension("第一章.开端"), "第一章.开端");
    }

    #[test]
    fn parses_common_chapter_range_directory_names() {
        let simple = parse_chapter_range_dir_name("1- 500").unwrap();
        assert_eq!(simple.start, 1);
        assert_eq!(simple.end, 500);
        assert_eq!(simple.context, "");

        let decorated = parse_chapter_range_dir_name("第001 - 050集").unwrap();
        assert_eq!(decorated.start, 1);
        assert_eq!(decorated.end, 50);
        assert_eq!(decorated.context, "");

        let tilde = parse_chapter_range_dir_name("[0501～1000]").unwrap();
        assert_eq!(tilde.start, 501);
        assert_eq!(tilde.end, 1000);

        let titled = parse_chapter_range_dir_name("【书名】001-050").unwrap();
        assert_eq!(titled.context, "书名");
        assert_eq!(titled.context_title.as_deref(), Some("书名"));

        let titled_no_space = parse_chapter_range_dir_name("[书名]101-150").unwrap();
        assert_eq!(titled_no_space.start, 101);
        assert_eq!(titled_no_space.end, 150);
        assert_eq!(titled_no_space.context, "书名");
        assert_eq!(titled_no_space.context_title.as_deref(), Some("书名"));

        let plain_no_space = parse_chapter_range_dir_name("书名001-050").unwrap();
        assert_eq!(plain_no_space.start, 1);
        assert_eq!(plain_no_space.end, 50);
        assert_eq!(plain_no_space.context, "书名");
        assert_eq!(plain_no_space.context_title.as_deref(), Some("书名"));
    }

    #[test]
    fn ignores_non_chapter_ranges() {
        assert!(parse_chapter_range_dir_name("2024-2025").is_none());
        assert!(parse_chapter_range_dir_name("500-1").is_none());
        assert!(parse_chapter_range_dir_name("1-50-100").is_none());
    }

    #[test]
    fn recognizes_pure_range_directories_only() {
        // 只有区间的目录是分段容器，可以被摊平到父目录。
        assert_eq!(parse_pure_range_dir_name("1~500"), Some((1, 500)));
        assert_eq!(parse_pure_range_dir_name("1-500"), Some((1, 500)));
        assert_eq!(parse_pure_range_dir_name("第1-500集"), Some((1, 500)));
        assert_eq!(parse_pure_range_dir_name("[0501～1000]"), Some((501, 1000)));

        // 带书名上下文的目录属于「书名+分段」，交给合并逻辑处理。
        assert!(parse_pure_range_dir_name("书名 1-500").is_none());
        assert!(parse_pure_range_dir_name("【书名】001-050").is_none());

        // 普通书名、日期目录、顺序错误的区间都不算。
        assert!(parse_pure_range_dir_name("完美世界").is_none());
        assert!(parse_pure_range_dir_name("2024-2025").is_none());
        assert!(parse_pure_range_dir_name("500-1").is_none());
    }

    #[test]
    fn selects_only_safe_sibling_range_groups() {
        let ranges = vec![
            parse_chapter_range_dir_name("001-050").unwrap(),
            parse_chapter_range_dir_name("051-100").unwrap(),
        ];
        assert_eq!(
            select_mergeable_range_group("书名", &ranges),
            Some(vec![0, 1])
        );

        let prefixed = vec![
            parse_chapter_range_dir_name("书名 001-050").unwrap(),
            parse_chapter_range_dir_name("书名 051-100").unwrap(),
        ];
        assert_eq!(
            select_mergeable_range_group("书名", &prefixed),
            Some(vec![0, 1])
        );

        let different_books = vec![
            parse_chapter_range_dir_name("书A 001-050").unwrap(),
            parse_chapter_range_dir_name("书B 051-100").unwrap(),
        ];
        assert_eq!(select_mergeable_range_group("合集", &different_books), None);
    }

    #[test]
    fn merges_every_non_overlapping_range_group_into_the_parent() {
        // 同一本书的区间目录，因为书名区域写法不同被拆成多个 context 键。
        // 这些分组彼此不重叠，应当全部并进父目录，而不是只并其中一组。
        let ranges: Vec<_> = ["1-100", "完美世界 101-200", "完美世界续集 201-300"]
            .into_iter()
            .map(|name| parse_chapter_range_dir_name(name).unwrap())
            .collect();
        let groups = select_mergeable_range_groups("完美世界", &ranges);
        assert_eq!(groups.len(), 3, "{groups:?}");
        assert!(groups.iter().all(|group| group.merge_into_parent));
        let mut merged: Vec<usize> = groups
            .iter()
            .flat_map(|group| group.indices.clone())
            .collect();
        merged.sort_unstable();
        assert_eq!(merged, vec![0, 1, 2]);
    }

    #[test]
    fn keeps_overlapping_range_groups_apart() {
        // 区间互相重叠，更像是两本书放进了同一个目录，不能硬并成一本。
        let ranges: Vec<_> = ["1-100", "50-150"]
            .into_iter()
            .map(|name| parse_chapter_range_dir_name(name).unwrap())
            .collect();
        let groups = select_mergeable_range_groups("合集", &ranges);
        assert!(
            groups.iter().all(|group| !group.merge_into_parent),
            "{groups:?}"
        );
    }

    #[test]
    fn arbitrary_suffixes_do_not_change_range_group_identity() {
        for suffix in [
            "完结",
            "完结撒花",
            "完结！感谢支持",
            "集（已完结，感谢收听）",
            "大结局【完结撒花】",
            "全书完，感谢陪伴",
            "（完）撒花",
            "全集（全本完结）",
            "集（完结）",
            "【完本】",
            " 全集",
            "-已完結",
            " completed",
            " end thanks for listening",
            "任意说明",
            "后续内容请看151-200",
            "xyz随便写",
            "完美世界",
        ] {
            for prefix in ["", "书名 "] {
                let ranges: Vec<_> = [
                    "1-50".to_string(),
                    "51-100".to_string(),
                    format!("101-150{suffix}"),
                ]
                .iter()
                .map(|range| parse_chapter_range_dir_name(&format!("{prefix}{range}")).unwrap())
                .collect();
                assert_eq!(
                    select_mergeable_range_group("书名", &ranges),
                    Some(vec![0, 1, 2]),
                    "{prefix}{suffix}"
                );
            }
        }
        let ranges: Vec<_> = ["甲书1-50", "甲书51-100", "乙书101-150完结"]
            .into_iter()
            .map(|name| parse_chapter_range_dir_name(name).unwrap())
            .collect();
        let groups = select_mergeable_range_groups("合集", &ranges);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert_eq!(groups[0].title.as_deref(), Some("甲书"));
        assert_eq!(
            parse_chapter_range_dir_name("完结故事101-150")
                .unwrap()
                .context_title
                .as_deref(),
            Some("完结故事")
        );
        assert_eq!(
            parse_chapter_range_dir_name("101-150完美世界")
                .unwrap()
                .context_title
                .as_deref(),
            None
        );
    }

    #[test]
    fn selects_titled_range_groups_under_collection_parent() {
        let ranges = vec![
            parse_chapter_range_dir_name("【书名A】001-050").unwrap(),
            parse_chapter_range_dir_name("[书名A]051-100").unwrap(),
            parse_chapter_range_dir_name("书名B001-050").unwrap(),
            parse_chapter_range_dir_name("书名B 051-100").unwrap(),
        ];

        let groups = select_mergeable_range_groups("合集", &ranges);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title.as_deref(), Some("书名A"));
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert!(!groups[0].merge_into_parent);
        assert_eq!(groups[1].title.as_deref(), Some("书名B"));
        assert_eq!(groups[1].indices, vec![2, 3]);
        assert!(!groups[1].merge_into_parent);
    }

    #[test]
    fn rejects_overlapping_ranges() {
        let ranges = vec![
            ChapterRangeDir {
                start: 1,
                end: 100,
                context: String::new(),
                context_title: None,
            },
            ChapterRangeDir {
                start: 80,
                end: 150,
                context: String::new(),
                context_title: None,
            },
        ];

        assert_eq!(select_mergeable_range_group("书名", &ranges), None);
    }
}

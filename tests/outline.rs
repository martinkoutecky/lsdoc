use lsdoc::ast::Block;
use lsdoc::{parse, parse_outline, OutlineHeader, OutlineHeaderKind, SourceRange};

fn headers(input: &str, format: &str) -> Vec<OutlineHeader> {
    parse_outline(input, format)
        .expect("v2 owns the test input")
        .headers
}

fn range(start: usize, end: usize) -> SourceRange {
    SourceRange { start, end }
}

fn assert_slices_are_exact(input: &str, headers: &[OutlineHeader]) {
    let mut previous_header_start = 0;
    for (index, header) in headers.iter().enumerate() {
        assert!(header.structural_prefix.start <= header.structural_prefix.end);
        assert!(header.line_content.start <= header.line_content.end);
        assert!(header.line.start <= header.line.end);
        assert_eq!(header.line_content.start, header.line.start);
        assert!(header.line.start <= header.structural_prefix.start);
        assert!(header.line.start <= header.header_start);
        assert!(header.header_start <= header.line_content.end);
        assert!(header.structural_prefix.end <= header.line_content.end);
        assert!(header.line_content.end <= header.line.end);
        assert!(header.structural_prefix.slice(input).is_some());
        assert!(header.line_content.slice(input).is_some());
        assert!(header.line.slice(input).is_some());
        if index > 0 {
            assert!(previous_header_start <= header.header_start);
        }
        previous_header_start = header.header_start;
    }
}

fn ast_root_outline(input: &str, format: &str) -> Vec<(OutlineHeaderKind, u32)> {
    parse(input, format)
        .iter()
        .filter_map(|block| match (format, block) {
            ("org", Block::Bullet { level, .. }) => Some((OutlineHeaderKind::OrgHeadline, *level)),
            (_, Block::Heading { level, .. }) => {
                Some((OutlineHeaderKind::MarkdownUnbulletedAtxHeading, *level))
            }
            (_, Block::Bullet { level, .. }) => {
                Some((OutlineHeaderKind::MarkdownDashBullet, *level))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn markdown_projects_root_headers_and_preserves_storage_body_prefixes() {
    let input = concat!(
        "ordinary preamble\n",
        "# Root ATX\n",
        "between\n",
        "  - TODO [#A] task café\n",
        "- # Embedded ATX\n",
        "#\n",
        "-\n",
    );
    let got = headers(input, "md");
    assert_eq!(got.len(), 5);
    assert_eq!(
        got.iter().map(|h| (h.kind, h.level)).collect::<Vec<_>>(),
        vec![
            (OutlineHeaderKind::MarkdownUnbulletedAtxHeading, 1),
            (OutlineHeaderKind::MarkdownDashBullet, 3),
            (OutlineHeaderKind::MarkdownDashBullet, 1),
            (OutlineHeaderKind::MarkdownUnbulletedAtxHeading, 1),
            (OutlineHeaderKind::MarkdownDashBullet, 1),
        ]
    );
    assert_eq!(got[0].structural_prefix.slice(input), Some(""));
    assert_eq!(got[0].line_content.slice(input), Some("# Root ATX"));
    assert_eq!(
        &input[got[0].structural_prefix.end..got[0].line_content.end],
        "# Root ATX"
    );
    assert_eq!(got[1].structural_prefix.slice(input), Some("  - "));
    assert_eq!(
        &input[got[1].structural_prefix.end..got[1].line_content.end],
        "TODO [#A] task café"
    );
    assert_eq!(got[2].structural_prefix.slice(input), Some("- "));
    assert_eq!(
        &input[got[2].structural_prefix.end..got[2].line_content.end],
        "# Embedded ATX"
    );
    assert_eq!(got[3].line_content.slice(input), Some("#"));
    assert_eq!(got[4].structural_prefix.slice(input), Some("-"));
    assert_slices_are_exact(input, &got);
}

#[test]
fn org_projects_root_headlines_and_preserves_markers_and_priorities() {
    let input = concat!(
        "ordinary preamble\n",
        "* Root\n",
        "body\n",
        "*** TODO [#A] café :tag:\n",
        "**\n",
        "*\n",
    );
    let got = headers(input, "org");
    assert_eq!(
        got.iter().map(|h| (h.kind, h.level)).collect::<Vec<_>>(),
        vec![
            (OutlineHeaderKind::OrgHeadline, 1),
            (OutlineHeaderKind::OrgHeadline, 3),
            (OutlineHeaderKind::OrgHeadline, 2),
            (OutlineHeaderKind::OrgHeadline, 1),
        ]
    );
    assert_eq!(got[0].structural_prefix.slice(input), Some("* "));
    assert_eq!(got[1].structural_prefix.slice(input), Some("*** "));
    assert_eq!(
        &input[got[1].structural_prefix.end..got[1].line_content.end],
        "TODO [#A] café :tag:"
    );
    assert_eq!(got[2].structural_prefix.slice(input), Some("**"));
    assert_eq!(got[3].structural_prefix.slice(input), Some("*"));
    assert_slices_are_exact(input, &got);
}

#[test]
fn conventional_separator_is_exactly_one_ascii_space() {
    let md = "-  two\n-\ttab\n";
    let got = headers(md, "md");
    assert_eq!(got[0].structural_prefix.slice(md), Some("- "));
    assert_eq!(
        &md[got[0].structural_prefix.end..got[0].line_content.end],
        " two"
    );
    assert_eq!(got[1].structural_prefix.slice(md), Some("-"));
    assert_eq!(
        &md[got[1].structural_prefix.end..got[1].line_content.end],
        "\ttab"
    );

    let org = "*  two\n*\ttab\n";
    let got = headers(org, "org");
    assert_eq!(got[0].structural_prefix.slice(org), Some("* "));
    assert_eq!(
        &org[got[0].structural_prefix.end..got[0].line_content.end],
        " two"
    );
    assert_eq!(got[1].structural_prefix.slice(org), Some("*"));
    assert_eq!(
        &org[got[1].structural_prefix.end..got[1].line_content.end],
        "\ttab"
    );
}

#[test]
fn parser_accepted_same_line_suffix_headers_keep_the_physical_line() {
    let input = "- $$x$$ # #+BEGIN_NOTE\r\nx\r\n#+END_NOTE";
    let got = headers(input, "md");
    assert_eq!(got.len(), 2);
    assert_eq!(
        got.iter()
            .map(|header| (header.kind, header.level))
            .collect::<Vec<_>>(),
        vec![
            (OutlineHeaderKind::MarkdownDashBullet, 1),
            (OutlineHeaderKind::MarkdownUnbulletedAtxHeading, 2),
        ]
    );
    let first_line_end = input.find('\r').unwrap();
    let nested_start = input.find(" # #+BEGIN_NOTE").unwrap();
    assert_eq!(got[0].structural_prefix, range(0, 2));
    assert_eq!(got[1].header_start, nested_start);
    assert_eq!(got[1].structural_prefix, range(0, 0));
    assert_eq!(got[0].line_content, range(0, first_line_end));
    assert_eq!(got[1].line_content, range(0, first_line_end));
    assert_eq!(got[0].line, range(0, first_line_end + 2));
    assert_eq!(got[1].line, range(0, first_line_end + 2));
    assert_slices_are_exact(input, &got);
}

#[test]
fn markdown_suppresses_headers_inside_non_root_bodies() {
    let input = concat!(
        "# Root\n",
        "```md\n",
        "# fenced fake\n",
        "- fenced fake\n",
        "```\n",
        "#+BEGIN_NOTE\n",
        "# custom fake\n",
        "- custom fake\n",
        "#+END_NOTE\n",
        "> # quote fake\n",
        "> - quote fake\n",
        "* regular list\n",
        "  # list-body fake\n",
        "+ another regular list\n",
        "  # continuation fake\n",
        "- Real after bodies\n",
    );
    let got = headers(input, "md");
    assert_eq!(
        got.iter()
            .map(|h| h.line_content.slice(input).unwrap())
            .collect::<Vec<_>>(),
        vec!["# Root", "- Real after bodies"]
    );
    assert_slices_are_exact(input, &got);
}

#[test]
fn org_suppresses_headlines_inside_literal_custom_quote_and_list_bodies() {
    let input = concat!(
        "* Root\n",
        "#+BEGIN_SRC rust\n",
        "* src fake\n",
        "#+END_SRC\n",
        "#+BEGIN_EXAMPLE\n",
        "** example fake\n",
        "#+END_EXAMPLE\n",
        "#+BEGIN_NOTE\n",
        "* custom fake\n",
        "#+END_NOTE\n",
        "#+BEGIN_QUOTE\n",
        "** quote fake\n",
        "#+END_QUOTE\n",
        "- regular list\n",
        "  * nested list fake\n",
        "** Real after bodies\n",
    );
    let got = headers(input, "org");
    assert_eq!(
        got.iter()
            .map(|h| h.line_content.slice(input).unwrap())
            .collect::<Vec<_>>(),
        vec!["* Root", "** Real after bodies"]
    );
    assert_slices_are_exact(input, &got);
}

#[test]
fn full_physical_line_spans_distinguish_all_terminators_and_eof() {
    let input = "# lf\n- crlf\r\n  - cr\r# eof";
    let got = headers(input, "md");
    assert_eq!(got.len(), 4);
    assert_eq!(got[0].line_content.slice(input), Some("# lf"));
    assert_eq!(got[0].line.slice(input), Some("# lf\n"));
    assert_eq!(got[1].line_content.slice(input), Some("- crlf"));
    assert_eq!(got[1].line.slice(input), Some("- crlf\r\n"));
    assert_eq!(got[2].line_content.slice(input), Some("  - cr"));
    assert_eq!(got[2].line.slice(input), Some("  - cr\r"));
    assert_eq!(got[3].line_content.slice(input), Some("# eof"));
    assert_eq!(got[3].line.slice(input), Some("# eof"));
    assert_eq!(got[3].line, got[3].line_content);
    assert_slices_are_exact(input, &got);
}

#[test]
fn front_matter_offsets_remain_absolute_and_utf8_safe() {
    let input = "---\r\ntitle: café\r\n---\r\n# Héading\r\n- TODO résumé";
    let got = headers(input, "md");
    let atx_start = input.find("# Héading").unwrap();
    let bullet_start = input.find("- TODO résumé").unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].line_content,
        range(atx_start, atx_start + "# Héading".len())
    );
    assert_eq!(got[0].line.end, got[0].line_content.end + 2);
    assert_eq!(got[1].line_content, range(bullet_start, input.len()));
    assert_eq!(
        got[1].structural_prefix,
        range(bullet_start, bullet_start + 2)
    );
    assert_slices_are_exact(input, &got);
}

#[test]
fn org_front_matter_offsets_remain_absolute() {
    let input = "---\ntitle: page\n---\n* Org root\n** Child";
    let got = headers(input, "org");
    let root_start = input.find("* Org root").unwrap();
    let child_start = input.find("** Child").unwrap();
    assert_eq!(
        got.iter()
            .map(|header| header.line.start)
            .collect::<Vec<_>>(),
        vec![root_start, child_start]
    );
    assert_eq!(got[0].structural_prefix.slice(input), Some("* "));
    assert_eq!(got[1].structural_prefix.slice(input), Some("** "));
    assert_slices_are_exact(input, &got);
}

#[test]
fn outline_kinds_and_levels_match_public_ast_root_acceptance() {
    for (format, input) in [
        (
            "md",
            "preamble\n# One\n  - Two\n- # Three\n* ordinary list\n# Four",
        ),
        (
            "org",
            "preamble\n* One\n*** TODO [#B] Two\n- ordinary list\n** Three",
        ),
    ] {
        let events = headers(input, format);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.kind, event.level))
                .collect::<Vec<_>>(),
            ast_root_outline(input, format)
        );
    }
}

#[test]
fn property_folding_discards_same_line_suffix_outline_events_with_the_ast() {
    for (format, input) in [
        (
            "md",
            ":PROPERTIES:\n:key: value\n:END: # Discarded\nnext:: property\n",
        ),
        (
            "org",
            ":PROPERTIES:\n:key: value\n:END:* Discarded\n#+NEXT: property\n",
        ),
    ] {
        assert!(ast_root_outline(input, format).is_empty(), "{format}");
        assert!(headers(input, format).is_empty(), "{format}");
    }
}

#[test]
fn property_folding_retries_same_line_suffix_outline_events_exactly_once() {
    for (format, input, expected_kind, expected_level) in [
        (
            "md",
            "first:: value\n:PROPERTIES:\n:key: value\n:END: # Retried\n",
            OutlineHeaderKind::MarkdownUnbulletedAtxHeading,
            2,
        ),
        (
            "org",
            "#+FIRST: value\n:PROPERTIES:\n:key: value\n:END:* Retried\n",
            OutlineHeaderKind::OrgHeadline,
            1,
        ),
    ] {
        let ast = ast_root_outline(input, format);
        assert_eq!(ast, vec![(expected_kind, expected_level)], "{format}");
        let outline = headers(input, format);
        assert_eq!(
            outline
                .iter()
                .map(|header| (header.kind, header.level))
                .collect::<Vec<_>>(),
            ast,
            "{format}"
        );
        assert_eq!(outline.len(), 1, "{format}");
    }
}

#[test]
fn parse_outline_disposes_deep_quote_ast_on_one_mib_stack() {
    // Unique inner callout names make the v2 frame parser build the entire
    // 20,000-node spine iteratively; the outer QUOTE owns that hidden tree.
    let depth = 20_000;
    let mut input = String::from("#+BEGIN_QUOTE\n");
    for level in 1..depth {
        input.push_str(&format!("#+BEGIN_q{level}\n"));
    }
    input.push_str("x\n");
    for level in (1..depth).rev() {
        input.push_str(&format!("#+END_q{level}\n"));
    }
    input.push_str("#+END_QUOTE\n");
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            let outline = parse_outline(&input, "md").expect("v2 owns deep Markdown quote input");
            assert!(outline.headers.is_empty());
        })
        .expect("spawn small-stack outline thread")
        .join()
        .expect("parse_outline recursively dropped its hidden deep quote AST");
}

#[test]
fn source_range_slice_is_non_panicking_for_invalid_ranges() {
    let input = "é";
    assert_eq!(range(0, 2).slice(input), Some("é"));
    assert_eq!(range(1, 2).slice(input), None);
    assert_eq!(range(0, 3).slice(input), None);
    assert_eq!(range(2, 1).slice(input), None);
    assert_eq!(range(0, 2).slice("xy"), Some("xy"));
}

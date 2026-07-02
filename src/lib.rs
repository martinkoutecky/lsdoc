//! lsdoc — a native-Rust parser for Logseq-flavored Markdown and Org into a typed,
//! `serde`-serializable AST, behavior-equivalent to Logseq's `mldoc` at the
//! granularity that matters for **indexing and rendering** (verified differentially
//! against `mldoc@1.5.7`; see `README.md`/`DECISIONS.md`).
//!
//! # Public API
//!
//! The stable surface is [`mod@ast`] (the AST types) plus four entry points:
//!
//! - [`parse`]`(input, format) -> Vec<ast::Block>` — the **render** path: the block
//!   tree a renderer consumes. `format` is `"org"` for Org, anything else Markdown.
//! - [`refs`]`(input, format) -> ast::Refs` — the **index** path: the OG-faithful
//!   inline ref set (`[[page]]`, `((block))`).
//! - [`parse_format`]/[`parse_to_projection`]/[`parse_org_to_projection`] — both at
//!   once, as an [`ast::Projection`] `{ blocks, refs }`.
//!
//! The AST is the **integration contract** for Tine, which mirrors its serde
//! encoding 1:1 in TypeScript — see [`mod@ast`] and `AST.md` for the field-by-field
//! map. The library depends only on `serde` + `serde_json`; the `bin/` driver and
//! `harness/` oracle are not part of it.

pub(crate) mod block_common;
pub(crate) mod entities;
pub(crate) mod inline;
pub(crate) mod inline_driver;
pub(crate) mod lexer;
pub(crate) mod metrics;
/// Debug-only complexity probe: read+reset the "scan work" counter (bytes examined by
/// re-scanning ops). Used by `tests/complexity.rs`; compiled out in release. See `metrics`.
#[cfg(debug_assertions)]
pub use metrics::__scan_work_take;
pub(crate) mod org;
pub(crate) mod org_resolver;
pub(crate) mod parse;
pub(crate) mod projection;
pub(crate) mod refs;
pub mod render;
pub(crate) mod resolver;
pub(crate) mod source_map;

/// The render contract: the stable, `serde`-serializable AST. **This IS lsdoc's AST**
/// (the projection that was once described as "comparison-only" — that framing is
/// retired; it is render-complete and frozen as the integration surface).
///
/// ## serde encoding (the wire format Tine mirrors in TypeScript)
///
/// Enums are **internally tagged**, with a per-enum discriminant key:
/// - [`Block`](ast::Block) → `"kind"`, [`Inline`](ast::Inline) → `"k"`,
///   [`Url`](ast::Url) → `"type"`.
///
/// Every `Option` / `false` `bool` / empty `Vec` / empty `String` field is **omitted**
/// (`skip_serializing_if`), except [`Block::Table`](ast::Block::Table)`.aligns`, which
/// is always serialized and may be `[]`. A consumer treats an absent key as the default
/// for omitted fields. Field names and `rename` values are part of the contract — see
/// `AST.md` for the exhaustive construct→variant table and the per-variant field list.
///
/// Two fields are intentionally opaque, carried as mldoc's raw JSON so the contract
/// need not commit to their sub-schema (both are render-complete as-is):
/// - [`Inline::Timestamp`](ast::Inline)`.date` — a date / range record (the `ts` tag
///   distinguishes `Date`/`Range`/`Scheduled`/`Deadline`/`Closed`/`Clock`).
/// - [`Inline::Email`](ast::Inline)`.text` — mldoc's address record.
pub mod ast {
    pub use crate::projection::{Align, Block, Inline, ListItem, Projection, Refs, Span, Url};
}

pub use render::{render_html, Format, RenderOpts};

use projection::Projection;

/// Parse `input` into the block AST (the render path). `format == "org"` selects Org;
/// anything else is Markdown. Equivalent to [`parse_format`]`(…).blocks`.
pub fn parse(input: &str, format: &str) -> Vec<ast::Block> {
    parse_format(input, format).blocks
}

/// Parse `input` into the OG-faithful inline ref set (the index path). Equivalent to
/// [`parse_format`]`(…).refs`.
pub fn refs(input: &str, format: &str) -> ast::Refs {
    parse_format(input, format).refs
}

/// Parse a single run of **inline** markup — no block-opener / table / list / heading
/// detection. The analogue of mldoc's `inline->edn` (OG's `inline-text`): for property
/// values, breadcrumbs, ref/query previews, query-table cells — any context that renders
/// inline markup but is NOT a full block body. Leading `>` / `|` / `---` / `#` / `[^1]:` /
/// `$$` are literal text here (they only open blocks in the block grammar). `format == "org"`
/// selects Org; anything else is Markdown.
pub fn inline(input: &str, format: &str) -> Vec<ast::Inline> {
    if format == "org" {
        org_resolver::parse_inline_org(input, 0)
    } else {
        resolver::parse_inline(input, 0)
    }
}

/// Test/perf hook (NOT stable API): the Org block parser returning the raw `Vec<Block>` (no
/// projection). Used by `tests/perf.rs` to time the PARSER alone (typically `+ mem::forget`, so
/// the recursive-AST consumer's drop doesn't pollute a deep-nesting / deep-`>` O(n) measure).
#[doc(hidden)]
pub fn __parse_org_streaming(input: &str) -> Vec<ast::Block> {
    org::parse_streaming_root(input)
}

/// Parse Markdown into the full [`ast::Projection`] (`{ blocks, refs }`).
pub fn parse_to_projection(input: &str) -> Projection {
    let blocks = parse::parse(input);
    let refs = refs::extract_refs(&blocks, "md");
    Projection { blocks, refs }
}

/// Parse Org into the full [`ast::Projection`]. Same ref extraction (the AST is
/// format-agnostic once built).
pub fn parse_org_to_projection(input: &str) -> Projection {
    let blocks = org::parse(input);
    let refs = refs::extract_refs(&blocks, "org");
    Projection { blocks, refs }
}

/// Parse into the full [`ast::Projection`], dispatching by format string
/// (`"org"` → Org, anything else → Markdown).
pub fn parse_format(input: &str, format: &str) -> Projection {
    if format == "org" {
        parse_org_to_projection(input)
    } else {
        parse_to_projection(input)
    }
}

#[cfg(test)]
mod table_align_tests {
    use crate::ast::{Align, Block};

    fn table_aligns(input: &str, format: &str) -> Vec<Option<Align>> {
        let blocks = crate::parse(input, format);
        match blocks.first() {
            Some(Block::Table { aligns, .. }) => aligns.clone(),
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn markdown_table_aligns_follow_separator_contract() {
        assert_eq!(
            table_aligns("|a|b|c|d|\n|:---|---:|:--:|---|\n|1|2|3|4|", "md"),
            vec![Some(Align::Left), Some(Align::Right), Some(Align::Center), None]
        );
        assert_eq!(table_aligns("|a|b|\n|---|---|\n|1|2|", "md"), vec![None, None]);
        assert_eq!(table_aligns("|a|b|\n|1|2|", "md"), Vec::<Option<Align>>::new());
        assert_eq!(table_aligns("|a|b|\n|:--|--:|", "md"), Vec::<Option<Align>>::new());
        assert_eq!(table_aligns("|a|b|\n|:|::|\n|1|2|", "md"), vec![None, None]);

        let ragged = table_aligns("|a|b|c|\n|:--|--:|\n|1|2|3|", "md");
        assert_eq!(ragged, vec![Some(Align::Left), Some(Align::Right)]);
        assert_eq!(ragged.len(), 2);
    }

    #[test]
    fn org_tables_emit_no_aligns() {
        assert_eq!(
            table_aligns("|a|b|\n|---+---|\n|1|2|", "org"),
            Vec::<Option<Align>>::new()
        );
    }

    #[test]
    fn table_aligns_serializes_even_when_empty() {
        let blocks = crate::parse("|a|b|\n|1|2|", "md");
        let table = serde_json::to_value(&blocks[0]).expect("table serializes");
        assert_eq!(table.get("aligns"), Some(&serde_json::json!([])));
    }
}

#[cfg(test)]
mod span_tests {
    use crate::projection::{Block, Inline, Span, SpanMapSegment};

    fn parse_md(s: &str) -> Vec<Inline> {
        crate::resolver::parse_inline(s, 0)
    }
    fn parse_org(s: &str) -> Vec<Inline> {
        crate::org_resolver::parse_inline_org(s, 0)
    }

    fn assert_plain(
        node: &Inline,
        expected_text: &str,
        expected_span: Span,
        expected_map: Option<&[SpanMapSegment]>,
    ) {
        let Inline::Plain {
            text,
            span,
            span_map,
        } = node else {
            panic!("expected plain, got {node:?}");
        };
        assert_eq!(text, expected_text);
        assert_eq!(*span, Some(expected_span));
        assert_eq!(span_map.as_deref(), expected_map);
    }

    fn assert_break(node: &Inline, expected_span: Span) {
        let Inline::Break { span } = node else {
            panic!("expected break, got {node:?}");
        };
        assert_eq!(*span, Some(expected_span));
    }

    fn quote_paragraph(input: &str, format: &str) -> Vec<Inline> {
        match &crate::parse(input, format)[0] {
            Block::Quote { children, .. } => match &children[0] {
                Block::Paragraph { inline, .. } => inline.clone(),
                b => panic!("expected quote paragraph, got {b:?}"),
            },
            b => panic!("expected quote, got {b:?}"),
        }
    }

    fn first_list_paragraph(input: &str) -> Vec<Inline> {
        match &crate::parse(input, "md")[0] {
            Block::List { items, .. } => match &items[0].content[0] {
                Block::Paragraph { inline, .. } => inline.clone(),
                b => panic!("expected list paragraph, got {b:?}"),
            },
            b => panic!("expected list, got {b:?}"),
        }
    }

    /// S5: `block_body[span] == text` for every `plain` node without `span_map`.
    fn assert_s5(input: &str, inlines: &[Inline]) {
        for n in inlines {
            check_s5_node(input, n);
        }
    }
    fn check_s5_node(input: &str, n: &Inline) {
        match n {
            Inline::Plain { text, span, span_map } => {
                if span_map.is_none() {
                    if let Some(Span(s, e)) = span {
                        assert_eq!(
                            &input.as_bytes()[*s..*e],
                            text.as_bytes(),
                            "S5 fail: plain '{text}' has span [{s},{e}) but source is '{}'",
                            &input[*s..*e]
                        );
                    }
                }
            }
            Inline::Emphasis { children, .. }
            | Inline::Subscript { children, .. }
            | Inline::Superscript { children, .. }
            | Inline::Tag { children, .. } => {
                for c in children {
                    check_s5_node(input, c);
                }
            }
            Inline::Link { label, .. } => {
                for c in label {
                    check_s5_node(input, c);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn test_plain_span() {
        let s = "hello world";
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Inline::Plain { text, span, .. }
            if text == "hello world" && *span == Some(Span(0, 11))));
        assert_s5(s, &out);
    }

    #[test]
    fn test_emphasis_worked_example() {
        // from spec: "a **b** c" (9 bytes)
        let s = "a **b** c";
        let out = parse_md(s);
        assert_eq!(out.len(), 3);
        if let Inline::Plain { text, span, .. } = &out[0] {
            assert_eq!(text, "a ");
            assert_eq!(*span, Some(Span(0, 2)));
        } else {
            panic!("out[0] not plain");
        }
        if let Inline::Emphasis { emph, children, span } = &out[1] {
            assert_eq!(emph, "Bold");
            assert_eq!(*span, Some(Span(2, 7)));
            assert_eq!(children.len(), 1);
            if let Inline::Plain { text, span, .. } = &children[0] {
                assert_eq!(text, "b");
                assert_eq!(*span, Some(Span(4, 5)));
            } else {
                panic!("emphasis child not plain");
            }
        } else {
            panic!("out[1] not emphasis");
        }
        if let Inline::Plain { text, span, .. } = &out[2] {
            assert_eq!(text, " c");
            assert_eq!(*span, Some(Span(7, 9)));
        } else {
            panic!("out[2] not plain");
        }
        assert_s5(s, &out);
    }

    #[test]
    fn test_nested_emphasis() {
        // ***x*** → Italic[Bold[plain "x"]]; the inner plain span is the innermost slice.
        let s = "***x***";
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        let Inline::Emphasis { emph, children, span } = &out[0] else { panic!("not emphasis") };
        assert_eq!(emph, "Italic");
        assert_eq!(*span, Some(Span(0, 7)));
        assert_eq!(children.len(), 1);
        let Inline::Emphasis { emph, children, span: inner_span } = &children[0] else {
            panic!("inner not emphasis")
        };
        assert_eq!(emph, "Bold");
        // S3: inner ⊆ outer.
        let (Some(Span(is, ie)), Some(Span(os, oe))) = (*inner_span, *span) else {
            panic!("missing spans")
        };
        assert!(os <= is && ie <= oe, "inner {inner_span:?} not contained in outer {span:?}");
        assert_eq!(children.len(), 1);
        if let Inline::Plain { text, span, .. } = &children[0] {
            assert_eq!(text, "x");
            assert_eq!(*span, Some(Span(3, 4)));
        } else {
            panic!("innermost not plain");
        }
        assert_s5(s, &out);
    }

    #[test]
    fn test_link_label_span() {
        // [t](u) → Link [0,6) with label plain "t" at [1,2)
        let s = "[t](u)";
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        let Inline::Link { span, label, .. } = &out[0] else { panic!("not link") };
        assert_eq!(*span, Some(Span(0, 6)));
        assert_eq!(label.len(), 1);
        if let Inline::Plain { text, span, .. } = &label[0] {
            assert_eq!(text, "t");
            assert_eq!(*span, Some(Span(1, 2)));
        } else {
            panic!("label[0] not plain");
        }
        assert_s5(s, &out);
    }

    #[test]
    fn test_link_multi_node_label() {
        // [**a**~~b~~](u): the label fully decomposes into two emphasis nodes with absolute
        // spans into the block body (S2), each containing its own plain child.
        let s = "[**a**~~b~~](u)";
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        let Inline::Link { label, span, .. } = &out[0] else { panic!("not link") };
        assert_eq!(*span, Some(Span(0, 15)));
        assert_eq!(label.len(), 2);
        if let Inline::Emphasis { emph, span, .. } = &label[0] {
            assert_eq!(emph, "Bold");
            assert_eq!(*span, Some(Span(1, 6)));
        } else {
            panic!("label[0] not emphasis");
        }
        if let Inline::Emphasis { emph, span, .. } = &label[1] {
            assert_eq!(emph, "Strike_through");
            assert_eq!(*span, Some(Span(6, 11)));
        } else {
            panic!("label[1] not emphasis");
        }
        assert_s5(s, &out);
    }

    #[test]
    fn test_inline_code_span() {
        // `x` → code atom over [0,3) (delimiters included)
        let s = "`x`";
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        if let Inline::Code { text, span } = &out[0] {
            assert_eq!(text, "x");
            assert_eq!(*span, Some(Span(0, 3)));
        } else {
            panic!("not code");
        }
    }

    #[test]
    fn test_non_ascii_plain() {
        // "café" — é is 2 bytes (U+00E9), so offsets are BYTES.
        let s = "café";
        assert_eq!(s.len(), 5);
        let out = parse_md(s);
        assert_eq!(out.len(), 1);
        if let Inline::Plain { text, span, .. } = &out[0] {
            assert_eq!(text, "café");
            assert_eq!(*span, Some(Span(0, 5)));
        } else {
            panic!("not plain");
        }
        assert_s5(s, &out);
    }

    fn plain_concat(inlines: &[Inline]) -> String {
        let mut out = String::new();
        for node in inlines {
            if let Inline::Plain { text, .. } = node {
                out.push_str(text);
            }
        }
        out
    }

    fn assert_known_inline_html(inlines: &[Inline], expected: &str) {
        assert!(matches!(
            inlines,
            [
                Inline::Plain { text: a, .. },
                Inline::InlineHtml { text: h, .. },
                Inline::Plain { text: b, .. },
            ] if a == "x " && h == expected && b == " y"
        ));
    }

    #[test]
    fn test_raw_html_inline_shared_parser() {
        for parse in [
            crate::resolver::parse_inline as fn(&str, usize) -> Vec<Inline>,
            crate::org_resolver::parse_inline_org as fn(&str, usize) -> Vec<Inline>,
        ] {
            let unknown = "x <foo>y</foo> z";
            let out = parse(unknown, 0);
            assert!(!out.iter().any(|n| matches!(n, Inline::InlineHtml { .. })));
            assert_eq!(plain_concat(&out), unknown);

            let br = "x <br/> y";
            let out = parse(br, 0);
            assert!(!out.iter().any(|n| matches!(n, Inline::InlineHtml { .. })));
            assert_eq!(plain_concat(&out), br);

            assert_known_inline_html(&parse("x <b>a</b> y", 0), "<b>a</b>");
            assert_known_inline_html(
                &parse("x <figcaption>a</figcaption> y", 0),
                "<figcaption>a</figcaption>",
            );
            assert_known_inline_html(
                &parse("x <blockquote>a</blockquote> y", 0),
                "<blockquote>a</blockquote>",
            );
        }
    }

    #[test]
    fn test_c4_email_optional_closer_and_suffix() {
        let out = parse_md("x <a@b co>");
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], Inline::Plain { text, .. } if text == "x "));
        let Inline::Email { text, span } = &out[1] else { panic!("expected email") };
        assert_eq!(text.get("local_part").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(text.get("domain").and_then(|v| v.as_str()), Some("b"));
        assert_eq!(*span, Some(Span(2, 6)));
        assert!(matches!(&out[2], Inline::Plain { text, span, .. } if text == " co>" && *span == Some(Span(6, 10))));

        let out = parse_md("x <a@b.co");
        assert!(matches!(&out[..], [
            Inline::Plain { text: a, .. },
            Inline::Email { text, .. },
        ] if a == "x " && text.get("domain").and_then(|v| v.as_str()) == Some("b.co")));
    }

    #[test]
    fn test_c4_org_target_shadows_radio_target() {
        let out = parse_org("x <<<r>>>");
        assert!(matches!(&out[..], [
            Inline::Plain { text: a, .. },
            Inline::Target { text: target, .. },
            Inline::Plain { text: tail, .. },
        ] if a == "x " && target == "<r" && tail == ">"));
    }

    #[test]
    fn test_c4_statistics_cookie_scanf_prefix() {
        let out = parse_md("[1/2%] [50%%] [1//2]");
        assert!(matches!(&out[0],
            Inline::Cookie { kind, value: 1, total: Some(2), .. } if kind == "Absolute"));
        assert!(matches!(&out[2],
            Inline::Cookie { kind, value: 50, total: None, .. } if kind == "Percent"));
        assert!(matches!(&out[3], Inline::Plain { text, .. } if text == " [1//2]"));
    }

    #[test]
    fn test_tag_non_ascii() {
        // "#škola" in org mode — š is 2 bytes; Tag [0,7), plain "škola" [1,7).
        let s = "#škola";
        assert_eq!(s.len(), 7);
        let out = crate::org_resolver::parse_inline_org(s, 0);
        let tag = out.iter().find(|n| matches!(n, Inline::Tag { .. }));
        let Some(Inline::Tag { children, span }) = tag else { panic!("expected Tag node") };
        assert_eq!(*span, Some(Span(0, 7)));
        let plain = children.iter().find(|n| matches!(n, Inline::Plain { .. }));
        if let Some(Inline::Plain { text, span, .. }) = plain {
            assert_eq!(text, "škola");
            assert_eq!(*span, Some(Span(1, 7)));
        } else {
            panic!("expected a plain child");
        }
        assert_s5(s, &out);
    }

    #[test]
    fn test_table_cell_base() {
        // A table cell's inline spans are ABSOLUTE block-body offsets (S2/S5).
        let input = "| hello | world |";
        let blocks = crate::parse::parse(input);
        let Some(crate::projection::Block::Table { rows, header, .. }) = blocks.first() else {
            panic!("expected a Table");
        };
        // "| hello | world |" is a header-only table (no separator/body), so the cells live
        // in `header`; either way, check whichever holds the row.
        let row = header.as_ref().map(|h| h.as_slice()).or_else(|| rows.first().map(|r| r.as_slice()));
        let row = row.expect("a header or body row");
        let cell = row.first().expect("a first cell");
        if let Some(Inline::Plain { text, span, .. }) = cell.first() {
            assert_eq!(text, "hello");
            if let Some(Span(s, e)) = span {
                assert_eq!(&input.as_bytes()[*s..*e], text.as_bytes(), "S5 in table cell");
            } else {
                panic!("table cell plain has no span");
            }
        } else {
            panic!("first cell not a plain");
        }
    }

    #[test]
    fn inline_spans_v2_worked_quote() {
        let inline = quote_paragraph("> b c\n> d", "md");
        assert_eq!(inline.len(), 4);
        assert_plain(&inline[0], "b c", Span(2, 5), None);
        assert_break(&inline[1], Span(5, 6));
        assert_plain(&inline[2], "d", Span(8, 9), None);
        assert_break(&inline[3], Span(9, 9));
    }

    #[test]
    fn inline_spans_v2_folded_constructs() {
        let quote_body = quote_paragraph("#+BEGIN_QUOTE\n  hi\n#+END_QUOTE", "org");
        assert_plain(&quote_body[0], "hi", Span(16, 18), None);
        assert_break(&quote_body[1], Span(18, 19));

        assert_plain(&first_list_paragraph("* item")[0], "item", Span(2, 6), None);
        assert_plain(&first_list_paragraph("1. item")[0], "item", Span(3, 7), None);

        let blocks = crate::parse("* > b c\n  > d", "md");
        let Block::List { items, .. } = &blocks[0] else { panic!("expected list") };
        let Block::Quote { children, .. } = &items[0].content[0] else { panic!("expected quote") };
        let Block::Paragraph { inline, .. } = &children[0] else { panic!("expected paragraph") };
        assert_plain(&inline[0], "b c", Span(4, 7), None);
        assert_break(&inline[1], Span(7, 8));
        assert_plain(&inline[2], "d", Span(12, 13), None);
        assert_break(&inline[3], Span(13, 13));
    }

    #[test]
    fn inline_spans_v2_non_ascii_and_crlf_quote() {
        let inline = quote_paragraph("> café", "md");
        assert_plain(&inline[0], "café", Span(2, 7), None);
        assert_break(&inline[1], Span(7, 7));

        let inline = quote_paragraph("> b c\r\n> d", "md");
        assert_plain(&inline[0], "b c", Span(2, 5), None);
        assert_break(&inline[1], Span(5, 7));
        assert_plain(&inline[2], "d", Span(9, 10), None);
        assert_break(&inline[3], Span(10, 10));
    }

    #[test]
    fn inline_spans_v2_non_folded_transformed_plains() {
        assert_plain(
            &parse_md("a\\*b")[0],
            "a*b",
            Span(0, 4),
            Some(&[SpanMapSegment(0, 0, 1), SpanMapSegment(1, 2, 2)]),
        );

        let quick = parse_md("<https://a\\*b>");
        let Inline::Link { label, .. } = &quick[0] else { panic!("expected quick link") };
        assert_plain(
            &label[0],
            "https://a*b",
            Span(1, 13),
            Some(&[SpanMapSegment(0, 1, 9), SpanMapSegment(9, 11, 2)]),
        );

        let blocks = crate::parse("* TODO task with :tag1:tag2:", "org");
        let Block::Bullet { inline, htags, .. } = &blocks[0] else { panic!("expected org headline") };
        assert_eq!(htags, &vec!["tag1".to_string(), "tag2".to_string()]);
        assert_plain(&inline[0], "task with ", Span(7, 17), None);
    }

    #[test]
    fn inline_spans_v2_cr_and_unknown_entity_maps() {
        let md = parse_md("*a\rb*");
        let Inline::Emphasis { children, .. } = &md[0] else { panic!("expected emphasis") };
        assert_plain(
            &children[0],
            "a\nb",
            Span(1, 4),
            Some(&[SpanMapSegment(0, 1, 1), SpanMapSegment(2, 3, 1)]),
        );

        assert_plain(
            &parse_org("\\doesnotexist")[0],
            "doesnotexist",
            Span(0, 13),
            Some(&[SpanMapSegment(0, 1, 12)]),
        );
    }
}

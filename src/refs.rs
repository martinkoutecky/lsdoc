//! OG-faithful reference extraction over the projection block tree — the Rust
//! mirror of `harness/lib/refs.mjs` (a port of graph-parser `block.cljs`):
//!
//!   page refs:  OG `get-page-reference`: Page_ref, Search, Org Search,
//!               File label, Nested_link, Tag, and embed-macro arg.
//!   block refs: OG `get-block-reference`: Block_ref, id:// links, UUID-ish link
//!               targets, and embed-macro ((uuid)) arg — UUID-gated.
//!
//! These walk the inline tree the parser emits (links/tags/macros) and produce the
//! real page/block ref sets; they are gated against mldoc's refs by `harness/`.

use std::borrow::Cow;

use crate::projection::{Block, Inline, ListItem, Property, Refs, Url};

pub fn extract_refs(blocks: &[Block], format: &str) -> Refs {
    let org = format == "org";
    let mut page = Vec::new();
    let mut block = Vec::new();
    let mut stack = Vec::new();
    // scan-owner: (a2) refs block traversal — explicit stack, each block pushed once.
    for b in blocks.iter().rev() {
        stack.push(RefFrame::Block(b));
    }
    // scan-owner: (a2) refs block traversal — stack pop owns recursive block/list walk.
    while let Some(frame) = stack.pop() {
        match frame {
            RefFrame::Block(b) => walk_block_iter(b, &mut stack, &mut page, &mut block, org),
            RefFrame::ListItem(it) => {
                walk_inlines(&it.name, &mut page, &mut block, org);
                // scan-owner: (a2) refs list traversal — child items are pushed once.
                for sub in it.items.iter().rev() {
                    stack.push(RefFrame::ListItem(sub));
                }
                // scan-owner: (a2) refs list traversal — item content blocks are pushed once.
                for c in it.content.iter().rev() {
                    stack.push(RefFrame::Block(c));
                }
            }
        }
    }
    page.sort_unstable();
    page.dedup();
    block.sort_unstable();
    block.dedup();
    Refs { page, block }
}

enum RefFrame<'a> {
    Block(&'a Block),
    ListItem(&'a ListItem),
}

fn walk_block_iter<'a>(
    b: &'a Block,
    stack: &mut Vec<RefFrame<'a>>,
    page: &mut Vec<String>,
    block: &mut Vec<String>,
    org: bool,
) {
    match b {
        Block::Paragraph { inline, .. }
        | Block::Heading { inline, .. }
        | Block::Bullet { inline, .. }
        | Block::FootnoteDef { inline, .. } => walk_inlines(inline, page, block, org),
        Block::Export { .. } | Block::CommentBlock { .. } => {}
        Block::Quote { children, .. } | Block::Custom { children, .. } => {
            // scan-owner: (a2) refs container traversal — children are pushed once.
            for c in children.iter().rev() {
                stack.push(RefFrame::Block(c));
            }
        }
        Block::List { items, .. } => {
            // scan-owner: (a2) refs list traversal — top-level items are pushed once.
            for it in items.iter().rev() {
                stack.push(RefFrame::ListItem(it));
            }
        }
        Block::Table { header, rows, .. } => {
            if let Some(h) = header {
                // scan-owner: (a2) refs table traversal — header cells are visited once.
                for cell in h {
                    walk_inlines(cell, page, block, org);
                }
            }
            // scan-owner: (a2) refs table traversal — rows are visited once.
            for row in rows {
                // scan-owner: (a2) refs table traversal — row cells are visited once.
                for cell in row {
                    walk_inlines(cell, page, block, org);
                }
            }
        }
        Block::Properties { props, .. } => {
            // mldoc stores each parse1 property's value with a third tuple field built by
            // `Property.property_references`: parse with `inline_skip_macro = true`, then
            // keep only top-level Tag/Link/Nested_link nodes for property page refs and
            // postwalk those precomputed nodes for property block refs. Do not use normal inline
            // parsing here: `{{query [[Page]]}}` is macro-opaque in ordinary inlines, but
            // property refs deliberately see the inner page link because macro parsing is
            // disabled.
            // scan-owner: (a2) refs property traversal — each property value is checked once.
            for prop in props {
                if !property_value_refs_enabled(prop) {
                    continue;
                }
                let value = prop.value();
                if property_value_is_unparsed(value) || !property_value_may_have_refs(value) {
                    continue;
                }
                let inl = if org {
                    crate::org_resolver::parse_property_reference_inlines_org(value, 0)
                } else {
                    crate::resolver::parse_property_reference_inlines(value, 0)
                };
                walk_property_reference_inlines(&inl, page, block);
            }
        }
        Block::Src { .. }
        | Block::Hr { .. }
        | Block::RawHtml { .. }
        | Block::DisplayedMath { .. }
        | Block::LatexEnv { .. }
        | Block::Drawer { .. }
        | Block::Directive { .. }
        | Block::Comment { .. }
        | Block::Example { .. }
        // Hiccup payload is the raw unparsed bracket text — mldoc extracts no refs from
        // it (the `[[…]]` inside `[:div [[Foo]]]` is not a ref).
        | Block::Hiccup { .. }
        | Block::Results { .. } => {}
    }
}

/// Drawer.parse2 (`#+NAME: value`) hardcodes the property's refs list to `[]`
/// in mldoc `lib/syntax/drawer.ml:74`; only parse1-derived entries contribute
/// value refs.
fn property_value_refs_enabled(prop: &Property) -> bool {
    !prop.is_parse2()
}

fn property_value_is_unparsed(value: &str) -> bool {
    value.is_empty()
        || (value.as_bytes().first() == Some(&b'"') && value.as_bytes().last() == Some(&b'"'))
}

fn property_value_may_have_refs(value: &str) -> bool {
    // scan-owner: (a2) property-ref prefilter — one bounded pass over the property value
    // before deciding whether to run the heavier inline-skip-macro parser.
    value
        .as_bytes()
        .iter()
        .any(|&b| matches!(b, b'[' | b'#' | b'(' | b'<' | b':' | b'{'))
}

fn walk_property_reference_inlines(
    inlines: &[Inline],
    page: &mut Vec<String>,
    block: &mut Vec<String>,
) {
    // scan-owner: (a2) property-reference inline filter — mldoc keeps only top-level
    // Tag/Link/Nested_link results from `Property.property_references` for page refs;
    // OG then postwalks that precomputed property AST for block refs.
    for seg in inlines {
        if let Some(p) = property_page_ref_from_inline(seg) {
            push_property_page_ref(page, p);
        }
    }
    walk_property_block_refs(inlines, block);
}

fn property_page_ref_from_inline(seg: &Inline) -> Option<Cow<'_, str>> {
    match seg {
        Inline::Link { url, .. } => match url {
            Url::PageRef { v } | Url::Search { v } => Some(Cow::Borrowed(v)),
            _ => None,
        },
        Inline::NestedLink { content, .. } => page_name(content).map(Cow::Borrowed),
        Inline::Tag { children, .. } => {
            let first = children.first()?;
            match first {
                Inline::Plain { text, .. } => Some(Cow::Borrowed(text)),
                other => property_page_ref_from_inline(other),
            }
        }
        _ => None,
    }
}

fn push_property_page_ref(page: &mut Vec<String>, raw: Cow<'_, str>) {
    // scan-owner: (a2) property page-ref trim — bounded to the current emitted ref.
    let p = raw.trim();
    if !p.is_empty() {
        // scan-owner: (o) property page-ref copy — output-size reference materialization.
        page.push(p.to_string());
    }
}

fn walk_property_block_refs(inlines: &[Inline], block: &mut Vec<String>) {
    // scan-owner: (a2) property block-ref postwalk — each property-ref inline slice is pushed once.
    let mut stack = Vec::new();
    let mut current = inlines;
    loop {
        for seg in current {
            match seg {
                Inline::Link { url, label, .. } => {
                    if let Some(id) = block_ref_from_link(url) {
                        block.push(id);
                    }
                    if !label.is_empty() {
                        stack.push(label);
                    }
                }
                Inline::Tag { children, .. }
                | Inline::Emphasis { children, .. }
                | Inline::Subscript { children, .. }
                | Inline::Superscript { children, .. } => {
                    if !children.is_empty() {
                        stack.push(children);
                    }
                }
                Inline::Macro { name, args, .. } if name == "embed" => {
                    if let Some(id) = args
                        .first()
                        .and_then(|arg| block_ref_id(arg))
                        .and_then(parse_uuid)
                    {
                        block.push(id);
                    }
                }
                Inline::NestedLink { .. } | Inline::ExportSnippet { .. } => {}
                _ => {}
            }
        }
        let Some(next) = stack.pop() else {
            break;
        };
        current = next;
    }
}

fn walk_inlines(inlines: &[Inline], page: &mut Vec<String>, block: &mut Vec<String>, org: bool) {
    let mut stack = Vec::new();
    let mut current = inlines;
    // scan-owner: (a2) refs inline traversal — explicit stack, each inline slice pushed once.
    loop {
        // scan-owner: (a2) refs inline traversal — nodes in the current slice are visited once.
        for seg in current {
            match seg {
                Inline::Link { url, label, .. } => {
                    add_link_refs(url, label, page, block, org);
                    if !label.is_empty() {
                        stack.push(label);
                    }
                }
                Inline::NestedLink { content, .. } => push_nested_link_pages(page, content),
                Inline::Tag { children, .. } => {
                    push_page_ref(page, tag_text(children).as_ref());
                    if !children.is_empty() {
                        stack.push(children);
                    }
                }
                Inline::Macro { name, args, .. } if name == "embed" => {
                    push_embed_page_ref(page, args);
                    if let Some(id) = args
                        .first()
                        .and_then(|arg| block_ref_id(arg))
                        .and_then(parse_uuid)
                    {
                        block.push(id); // {{embed ((uuid))}}
                    }
                }
                Inline::Emphasis { children, .. }
                | Inline::Subscript { children, .. }
                | Inline::Superscript { children, .. } => {
                    if !children.is_empty() {
                        stack.push(children);
                    }
                }
                Inline::ExportSnippet { .. } => {}
                _ => {}
            }
        }
        let Some(next) = stack.pop() else {
            break;
        };
        current = next;
    }
}

fn add_link_refs(
    url: &Url,
    label: &[Inline],
    page: &mut Vec<String>,
    block: &mut Vec<String>,
    org: bool,
) {
    if let Some(p) = page_ref_from_link(url, label, org) {
        push_page_ref(page, p.as_ref());
    }
    if let Some(id) = block_ref_from_link(url) {
        block.push(id);
    }
}

fn page_ref_from_link<'a>(url: &'a Url, label: &'a [Inline], org: bool) -> Option<Cow<'a, str>> {
    match url {
        Url::PageRef { v } if !local_asset(v) && !draw_path(v) => Some(Cow::Borrowed(v)),
        Url::Search { v } if page_ref_shaped(v) => Some(Cow::Borrowed(unbracket(v))),
        Url::Search { v } if org && !local_asset(v) => Some(Cow::Borrowed(v)),
        Url::File { .. } => first_label_value(label),
        _ => None,
    }
}

fn block_ref_from_link(url: &Url) -> Option<String> {
    match url {
        Url::BlockRef { v } => parse_uuid(v),
        Url::Complex {
            protocol: Some(protocol),
            link: Some(link),
        } if protocol == "id" => parse_uuid(link),
        Url::PageRef { v } | Url::Search { v } | Url::File { v } | Url::EmbedData { v } => {
            // scan-owner: (a2) link block-ref shaping — bounded to the current URL string.
            let id = block_ref_id(v).unwrap_or_else(|| v.trim());
            parse_uuid(id)
        }
        _ => None,
    }
}

fn first_label_value(label: &[Inline]) -> Option<Cow<'_, str>> {
    match label.first()? {
        Inline::Plain { text, .. } | Inline::Code { text, .. } | Inline::Verbatim { text, .. } => {
            Some(Cow::Borrowed(text))
        }
        Inline::NestedLink { content, .. } => Some(Cow::Borrowed(content)),
        Inline::Tag { children, .. } => Some(tag_text(children)),
        Inline::Entity { unicode, .. } => Some(Cow::Borrowed(unicode)),
        _ => None,
    }
}

fn push_nested_link_pages(page: &mut Vec<String>, content: &str) {
    let mut stack = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    // scan-owner: (a2) nested-link ref extraction — scans the already-emitted
    // nested-link content once and records every balanced `[[...]]` pair.
    while i + 1 < bytes.len() {
        crate::metrics::scan_work(1);
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            stack.push(i);
            i += 2;
        } else if bytes[i] == b']' && bytes[i + 1] == b']' {
            if let Some(start) = stack.pop() {
                // scan-owner: (o) nested-link ref copy — copies one emitted ref string.
                push_page_ref(page, &content[start..i + 2]);
            }
            i += 2;
        } else {
            i += crate::inline::char_len(bytes[i]);
        }
    }
}

fn push_page_ref(page: &mut Vec<String>, raw: &str) {
    let p = unbracket(raw);
    // scan-owner: (o) page-ref copy — final output materialization for one emitted ref.
    page.push(block_ref_id(p).unwrap_or(p).to_string());
}

fn push_embed_page_ref(page: &mut Vec<String>, args: &[String]) {
    match args {
        [] => push_page_ref(page, ""),
        [only] => push_page_ref(page, only),
        _ => {
            let mut joined = String::new();
            // scan-owner: (o) embed macro arg join — output-size materialization of one ref.
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    joined.push_str(", ");
                }
                joined.push_str(arg);
            }
            push_page_ref(page, &joined);
        }
    }
}

fn local_asset(s: &str) -> bool {
    let trimmed = s.trim_start_matches(['.', '/']);
    trimmed.starts_with("assets")
}

fn draw_path(s: &str) -> bool {
    s.starts_with("draws")
}

fn page_ref_shaped(s: &str) -> bool {
    // scan-owner: (a2) page-ref shape trim — bounded to the current URL string.
    let t = s.trim();
    t.starts_with("[[") && t.ends_with("]]")
}

fn page_name(s: &str) -> Option<&str> {
    // scan-owner: (a2) property nested-link trim — bounded to the current nested-link content.
    let t = s.trim();
    // scan-owner: (a2) property nested-link bracket strip — bounded to the current content.
    let inner = t.strip_prefix("[[")?;
    let inner = inner.strip_suffix("]]")?;
    Some(inner)
}

/// Concatenate a tag's inline content the way block.cljs get-tag does.
fn tag_text(children: &[Inline]) -> Cow<'_, str> {
    match children {
        [Inline::Plain { text, .. }] => return Cow::Borrowed(text),
        [Inline::Link { full, .. }] => return Cow::Borrowed(full),
        [Inline::NestedLink { content, .. }] => return Cow::Borrowed(content),
        _ => {}
    }

    let mut s = String::new();
    // scan-owner: (a2) tag text materialization — each tag child is copied/ignored once.
    for seg in children {
        match seg {
            Inline::Plain { text, .. } => s.push_str(text),
            Inline::Link { full, .. } => s.push_str(full),
            Inline::NestedLink { content, .. } => s.push_str(content),
            _ => {}
        }
    }
    Cow::Owned(s)
}

/// Strip a surrounding `[[ ]]` if present (page-ref-un-brackets!).
fn unbracket(s: &str) -> &str {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
        inner
    } else {
        s
    }
}

/// Extract `uuid` from `((uuid))` if shaped that way.
fn block_ref_id(s: &str) -> Option<&str> {
    let t = s.trim();
    t.strip_prefix("((")
        .and_then(|x| x.strip_suffix("))"))
        .map(|x| x.trim())
}

/// UUID gate (canonical 8-4-4-4-12 hex), matching block.cljs parse-uuid intent.
pub fn parse_uuid(s: &str) -> Option<String> {
    let t = s.trim();
    let bytes = t.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    for (i, &c) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        };
        if !ok {
            return None;
        }
    }
    Some(t.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(props: Vec<Property>) -> Vec<Block> {
        vec![Block::Properties { props, span: None }]
    }

    #[test]
    fn parse1_property_refs_skip_macro_parser() {
        let blocks = properties(vec![Property::parse1((
            "querkey".to_string(),
            "valuey macro (page): {{query [[Some Page]]}}".to_string(),
        ))]);

        assert_eq!(extract_refs(&blocks, "md").page, ["Some Page"]);
    }

    #[test]
    fn property_refs_are_top_level_only_and_parse2_is_empty() {
        let blocks = properties(vec![
            Property::parse1((
                "visible".to_string(),
                "**[[Hidden]]** [[Shown]] #tag".to_string(),
            )),
            Property::parse2(("ignored".to_string(), "[[Parse2]] #skip".to_string())),
        ]);

        assert_eq!(extract_refs(&blocks, "md").page, ["Shown", "tag"]);
    }

    #[test]
    fn og_reference_extraction_link_variants() {
        assert_eq!(
            crate::parse_format("[[outer [[Inner]]]]", "md").refs.page,
            ["Inner", "outer [[Inner]]"]
        );
        assert_eq!(
            crate::parse_format("[[plain-search][label]]", "org")
                .refs
                .page,
            ["plain-search"]
        );
        assert_eq!(
            crate::parse_format("[Some Page](file:../pages/some_page.md)", "md")
                .refs
                .page,
            ["Some Page"]
        );
        assert_eq!(
            crate::parse_format("[x](id://11111111-1111-1111-1111-111111111111)", "md")
                .refs
                .block,
            ["11111111-1111-1111-1111-111111111111"]
        );
        assert_eq!(
            crate::parse_format("((AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA))", "md")
                .refs
                .block,
            ["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"]
        );
        assert_eq!(
            crate::parse_format("{{embed Plain Page}}", "md").refs.page,
            ["Plain Page"]
        );
        assert_eq!(
            crate::parse_format("{{embed ((AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA))}}", "md",).refs,
            Refs {
                page: vec!["AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".into()],
                block: vec!["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into()],
            }
        );
        assert_eq!(
            crate::parse_format("p #tag[[P]] q", "md").refs.page,
            ["P", "tag[[P]]"]
        );
        assert_eq!(
            crate::parse_format("p #tag[[url][label]] q", "org")
                .refs
                .page,
            ["tag[[url][label]]", "url"]
        );
    }

    #[test]
    fn og_reference_extraction_property_variants() {
        assert_eq!(
            crate::parse_format("k:: [[outer [[Inner]]]]", "md")
                .refs
                .page,
            ["outer [[Inner]]"]
        );
        assert_eq!(
            crate::parse_format("k:: [label](foo)", "md").refs.page,
            ["foo"]
        );
        assert!(crate::parse_format("k:: [Some](file:../x.md)", "md")
            .refs
            .page
            .is_empty());
        assert_eq!(
            crate::parse_format("k:: [x](id://11111111-1111-1111-1111-111111111111)", "md")
                .refs
                .block,
            ["11111111-1111-1111-1111-111111111111"]
        );
    }
}

//! OG-faithful reference extraction over the projection block tree — the Rust
//! mirror of `harness/lib/refs.mjs` (a port of graph-parser `block.cljs`):
//!
//!   page refs:  Link Page_ref value; Tag (un-bracketed); embed-macro arg
//!               (un-bracketed) — ONLY name == "embed".
//!   block refs: Link Block_ref id; embed-macro ((uuid)) arg — both UUID-gated.
//!
//! These walk the inline tree the parser emits (links/tags/macros) and produce the
//! real page/block ref sets; they are gated against mldoc's refs by `harness/`.

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
                walk_inlines(&it.name, &mut page, &mut block);
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
    page.sort();
    page.dedup();
    block.sort();
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
        | Block::FootnoteDef { inline, .. } => walk_inlines(inline, page, block),
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
                    walk_inlines(cell, page, block);
                }
            }
            // scan-owner: (a2) refs table traversal — rows are visited once.
            for row in rows {
                // scan-owner: (a2) refs table traversal — row cells are visited once.
                for cell in row {
                    walk_inlines(cell, page, block);
                }
            }
        }
        Block::Properties { props, .. } => {
            // mldoc stores each parse1 property's value with a third tuple field built by
            // `Property.property_references`: parse with `inline_skip_macro = true`, then
            // keep only top-level Tag/Link/Nested_link nodes. Do not use normal inline
            // parsing here: `{{query [[Page]]}}` is macro-opaque in ordinary inlines, but
            // property refs deliberately see the inner page link because macro parsing is
            // disabled.
            // scan-owner: (a2) refs property traversal — each property value is checked once.
            for prop in props {
                if !property_value_refs_enabled(prop) {
                    continue;
                }
                let value = prop.value();
                if property_value_is_unparsed(value) {
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

fn walk_property_reference_inlines(
    inlines: &[Inline],
    page: &mut Vec<String>,
    block: &mut Vec<String>,
) {
    // scan-owner: (a2) property-reference inline filter — mldoc keeps only top-level
    // Tag/Link/Nested_link results from `Property.property_references`.
    for seg in inlines {
        match seg {
            Inline::Link { url, .. } => match url {
                Url::PageRef { v } => page.push(v.clone()),
                Url::BlockRef { v } => {
                    if let Some(id) = parse_uuid(v) {
                        block.push(id);
                    }
                }
                _ => {}
            },
            Inline::Tag { children, .. } => page.push(unbracket(&tag_text(children))),
            Inline::NestedLink { .. } => {}
            _ => {}
        }
    }
}

fn walk_inlines(inlines: &[Inline], page: &mut Vec<String>, block: &mut Vec<String>) {
    let mut stack = vec![inlines];
    // scan-owner: (a2) refs inline traversal — explicit stack, each inline slice pushed once.
    while let Some(inlines) = stack.pop() {
        // scan-owner: (a2) refs inline traversal — nodes in the current slice are visited once.
        for seg in inlines {
            match seg {
                Inline::Link { url, label, .. } => {
                    match url {
                        Url::PageRef { v } => page.push(v.clone()),
                        Url::BlockRef { v } => {
                            if let Some(id) = parse_uuid(v) {
                                block.push(id);
                            }
                        }
                        _ => {}
                    }
                    stack.push(label);
                }
                Inline::Tag { children, .. } => page.push(unbracket(&tag_text(children))),
                Inline::Macro { name, args, .. } if name == "embed" => {
                    let joined = args.join(", ");
                    if let Some(id) = block_ref_id(&joined).and_then(|s| parse_uuid(&s)) {
                        block.push(id); // {{embed ((uuid))}}
                    } else {
                        let p = unbracket(&joined);
                        if p != joined || joined.trim_start().starts_with("[[") {
                            page.push(p); // {{embed [[Foo]]}}
                        }
                    }
                }
                Inline::Emphasis { children, .. }
                | Inline::Subscript { children, .. }
                | Inline::Superscript { children, .. } => stack.push(children),
                Inline::ExportSnippet { .. } => {}
                _ => {}
            }
        }
    }
}

/// Concatenate a tag's inline content the way block.cljs get-tag does.
fn tag_text(children: &[Inline]) -> String {
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
    s
}

/// Strip a surrounding `[[ ]]` if present (page-ref-un-brackets!).
fn unbracket(s: &str) -> String {
    let t = s.trim();
    if let Some(inner) = t.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
        inner.to_string()
    } else {
        s.to_string()
    }
}

/// Extract `uuid` from `((uuid))` if shaped that way.
fn block_ref_id(s: &str) -> Option<String> {
    let t = s.trim();
    t.strip_prefix("((")
        .and_then(|x| x.strip_suffix("))"))
        .map(|x| x.trim().to_string())
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
    Some(t.to_string())
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
}

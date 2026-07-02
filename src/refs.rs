//! OG-faithful reference extraction over the projection block tree — the Rust
//! mirror of `harness/lib/refs.mjs` (a port of graph-parser `block.cljs`):
//!
//!   page refs:  Link Page_ref value; Tag (un-bracketed); embed-macro arg
//!               (un-bracketed) — ONLY name == "embed".
//!   block refs: Link Block_ref id; embed-macro ((uuid)) arg — both UUID-gated.
//!
//! These walk the inline tree the parser emits (links/tags/macros) and produce the
//! real page/block ref sets; they are gated against mldoc's refs by `harness/`.

use crate::projection::{Block, Inline, ListItem, Refs, Url};

pub fn extract_refs(blocks: &[Block], format: &str) -> Refs {
    let org = format == "org";
    let mut page = Vec::new();
    let mut block = Vec::new();
    for b in blocks {
        walk_block(b, &mut page, &mut block, org);
    }
    page.sort();
    page.dedup();
    block.sort();
    block.dedup();
    Refs { page, block }
}

fn walk_block(b: &Block, page: &mut Vec<String>, block: &mut Vec<String>, org: bool) {
    match b {
        Block::Paragraph { inline, .. }
        | Block::Heading { inline, .. }
        | Block::Bullet { inline, .. }
        | Block::FootnoteDef { inline, .. } => walk_inlines(inline, page, block),
        Block::Quote { children, .. } | Block::Custom { children, .. } => {
            for c in children {
                walk_block(c, page, block, org);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                walk_list_item(it, page, block, org);
            }
        }
        Block::Table { header, rows, .. } => {
            if let Some(h) = header {
                for cell in h {
                    walk_inlines(cell, page, block);
                }
            }
            for row in rows {
                for cell in row {
                    walk_inlines(cell, page, block);
                }
            }
        }
        Block::Properties { props, .. } => {
            // mldoc stores each property's value as a parsed inline list (the AST's
            // 3rd tuple element), which OG's block.cljs walks for refs. Re-parse each
            // value to recover those page/block refs (e.g. `tags:: [[Foo]], Bar`).
            // mldoc's `Property.property_references` first bails (empty refs) when the
            // trimmed value is empty or fully wrapped in double quotes (`desc:: "..."`
            // is "unparsed"), so we must NOT extract refs from those.
            for (_k, v) in props {
                let vt = v.trim();
                if vt.is_empty() || (vt.starts_with('"') && vt.ends_with('"')) {
                    continue;
                }
                // Re-parse with the FORMAT's inline parser: mldoc parses the property
                // value's inline list per format, and the two differ (e.g. org `[[x][y]]`
                // → Search link [no ref]; md `[[x][y]]` → Page_ref "x][y"). C6.
                let inl = if org {
                    crate::org_resolver::parse_inline_org(v, 0)
                } else {
                    crate::resolver::parse_inline(v, 0)
                };
                walk_inlines(&inl, page, block);
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
        | Block::Hiccup { .. } => {}
    }
}

/// Walk a list item's def-list term `name`, its block content, and (recursively) its
/// nested child items — mirroring the oracle's generic deep walk over the AST (which
/// recurses into every `name`/`content`/`items` field). See `harness/lib/refs.mjs`.
fn walk_list_item(it: &ListItem, page: &mut Vec<String>, block: &mut Vec<String>, org: bool) {
    walk_inlines(&it.name, page, block);
    for c in &it.content {
        walk_block(c, page, block, org);
    }
    for sub in &it.items {
        walk_list_item(sub, page, block, org);
    }
}

fn walk_inlines(inlines: &[Inline], page: &mut Vec<String>, block: &mut Vec<String>) {
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
                walk_inlines(label, page, block);
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
            | Inline::Superscript { children, .. } => walk_inlines(children, page, block),
            Inline::ExportSnippet { .. } => {}
            _ => {}
        }
    }
}

/// Concatenate a tag's inline content the way block.cljs get-tag does.
fn tag_text(children: &[Inline]) -> String {
    let mut s = String::new();
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

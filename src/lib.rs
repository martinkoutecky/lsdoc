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

pub(crate) mod entities;
pub(crate) mod inline;
pub(crate) mod org;
pub(crate) mod parse;
pub(crate) mod projection;
pub(crate) mod refs;

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
/// (`skip_serializing_if`); a consumer treats an absent key as the default. Field names
/// and `rename` values are part of the contract — see `AST.md` for the exhaustive
/// construct→variant table and the per-variant field list.
///
/// Two fields are intentionally opaque, carried as mldoc's raw JSON so the contract
/// need not commit to their sub-schema (both are render-complete as-is):
/// - [`Inline::Timestamp`](ast::Inline)`.date` — a date / range record (the `ts` tag
///   distinguishes `Date`/`Range`/`Scheduled`/`Deadline`/`Closed`).
/// - [`Inline::Email`](ast::Inline)`.text` — mldoc's address record.
pub mod ast {
    pub use crate::projection::{Block, Inline, ListItem, Projection, Refs, Span, Url};
}

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

/// Parse Markdown into the full [`ast::Projection`] (`{ blocks, refs }`).
pub fn parse_to_projection(input: &str) -> Projection {
    let blocks = parse::parse(input);
    let refs = refs::extract_refs(&blocks);
    Projection { blocks, refs }
}

/// Parse Org into the full [`ast::Projection`]. Same ref extraction (the AST is
/// format-agnostic once built).
pub fn parse_org_to_projection(input: &str) -> Projection {
    let blocks = org::parse(input);
    let refs = refs::extract_refs(&blocks);
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

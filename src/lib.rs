//! lsdoc — a native-Rust parser for Logseq-flavored Markdown (and, later, Org)
//! into a typed, serde-serializable AST with source spans, behavior-equivalent to
//! Logseq's `mldoc` at the granularity that matters for indexing and rendering.
//!
//! See `SPEC.md` for the brief, `DECISIONS.md` for the design log, and `README.md`
//! for the oracle/harness. Built milestone by milestone; modules grow as each
//! construct lands (block structure, inline core, dialect inline, …).

pub mod entities;
pub mod inline;
pub mod org;
pub mod parse;
pub mod projection;
pub mod refs;

use projection::Projection;

/// Parse Markdown into the normalized observable projection diffed against mldoc.
pub fn parse_to_projection(input: &str) -> Projection {
    let blocks = parse::parse(input);
    let refs = refs::extract_refs(&blocks);
    Projection { blocks, refs }
}

/// Parse Org into the same projection (M6). Same ref extraction (the projection is
/// format-agnostic once built).
pub fn parse_org_to_projection(input: &str) -> Projection {
    let blocks = org::parse(input);
    let refs = refs::extract_refs(&blocks);
    Projection { blocks, refs }
}

/// Dispatch by format string ("org" → Org, anything else → Markdown).
pub fn parse_format(input: &str, format: &str) -> Projection {
    if format == "org" {
        parse_org_to_projection(input)
    } else {
        parse_to_projection(input)
    }
}

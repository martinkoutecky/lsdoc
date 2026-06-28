//! lsdoc — a native-Rust parser for Logseq-flavored Markdown (and, later, Org)
//! into a typed, serde-serializable AST with source spans, behavior-equivalent to
//! Logseq's `mldoc` at the granularity that matters for indexing and rendering.
//!
//! See `SPEC.md` for the brief, `DECISIONS.md` for the design log, and `README.md`
//! for the oracle/harness. Built milestone by milestone; modules grow as each
//! construct lands (block structure, inline core, dialect inline, …).

pub mod inline;
pub mod parse;
pub mod projection;
pub mod refs;

use projection::Projection;

/// Parse an input string into the normalized observable projection that is diffed
/// against mldoc (the oracle). Convenience over `parse::parse` + `refs::extract_refs`.
pub fn parse_to_projection(input: &str) -> Projection {
    let blocks = parse::parse(input);
    let refs = refs::extract_refs(&blocks);
    Projection { blocks, refs }
}

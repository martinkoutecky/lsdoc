//! Source-transcribed parser used by the public lsdoc entry points.
//!
//! The first invariant is zero behavior drift against the public contract: the normal
//! parser, `LSDOC_ENGINE=v2`, and the strict/report harness paths must produce the same
//! projection for every input v2 owns. The legacy parser remains only as an explicit
//! harness engine.

pub(crate) mod block;
pub(crate) mod source;

use crate::projection::Projection;
use crate::{ast, org_resolver, refs, resolver};

pub(crate) fn try_parse_format(input: &str, format: &str) -> Option<Projection> {
    let blocks = block::try_parse(input, format)?;
    let refs = if format == "org" {
        refs::extract_refs(&blocks, "org")
    } else {
        refs::extract_refs(&blocks, "md")
    };
    Some(Projection { blocks, refs })
}

pub(crate) fn parse_format(input: &str, format: &str) -> Projection {
    try_parse_format(input, format)
        .unwrap_or_else(|| panic!("lsdoc v2 parser does not yet own {format:?} input"))
}

pub(crate) fn parse_blocks(input: &str, format: &str) -> Vec<ast::Block> {
    block::try_parse(input, format)
        .unwrap_or_else(|| panic!("lsdoc v2 parser does not yet own {format:?} input"))
}

pub(crate) fn inline(input: &str, format: &str) -> Vec<ast::Inline> {
    if format != "org" {
        crate::lexer::reset_markdown_code_span_state();
    }
    inline_at(input, format, 0)
}

pub(crate) fn inline_at(input: &str, format: &str, base: usize) -> Vec<ast::Inline> {
    if format == "org" {
        org_resolver::parse_inline_org(input, base)
    } else {
        resolver::parse_inline(input, base)
    }
}

//! Parser entry point.
//!
//! MILESTONE 1 STUB: emits the whole input as a single paragraph of plain text so
//! the differential harness runs end-to-end. Real block/inline parsing lands in
//! M2+ and replaces this. Until then, expect large oracle diffs — that is the
//! intended starting state of the oracle-driven loop.

use crate::projection::{Block, Inline, Span};

pub fn parse(input: &str) -> Vec<Block> {
    if input.is_empty() {
        return vec![];
    }
    vec![Block::Paragraph {
        inline: vec![Inline::Plain {
            text: input.to_string(),
        }],
        span: Some(Span(0, input.len())),
    }]
}

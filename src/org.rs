//! Org-mode parser (M6).
//!
//! STUB until the Org block segmenter + inline parser land — emits the whole input
//! as one paragraph so the differential loop runs end-to-end on Org inputs (the
//! same "infra-before-implementation" pattern used for the Markdown M1 stub). Org
//! corpus inputs will diff until this is replaced.
//!
//! Org differs from Markdown: headlines are `*`-prefixed (mldoc `Heading{unordered}`)
//! with markers/priority/`:tags:`; emphasis is `*b* /i/ _u_ +s+ ~code~ =verb=`;
//! links are `[[target]]` / `[[target][label]]`; `#+KEY:` directives; `#+BEGIN_X`
//! blocks; `:PROPERTIES:`/drawers; `[fn:1]` footnotes.

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

//! Stable source-oriented projection of parser-accepted document outline headers.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::Range;

/// A half-open UTF-8 byte range (`start..end`) in the original parse input.
///
/// Ranges emitted by [`crate::parse_outline`] always lie on UTF-8 character
/// boundaries. [`SourceRange::slice`] provides a non-panicking way to apply a
/// range to the input that produced it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}

impl SourceRange {
    pub(crate) const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Return this range as a standard Rust range.
    pub const fn as_range(self) -> Range<usize> {
        self.start..self.end
    }

    /// Safely slice `input`, returning `None` for an out-of-bounds range or a
    /// range whose endpoints are not UTF-8 character boundaries.
    ///
    /// A range carries offsets, not input provenance. Passing a different
    /// string can therefore return `Some` when the same offsets are valid for
    /// that string.
    pub fn slice<'a>(self, input: &'a str) -> Option<&'a str> {
        input.get(self.as_range())
    }
}

/// The source syntax whose parser-owned acceptance produced an outline header.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineHeaderKind {
    /// An unbulleted Markdown ATX heading such as `# Heading`.
    MarkdownUnbulletedAtxHeading,
    /// A Markdown dash outline bullet such as `  - TODO Heading`.
    MarkdownDashBullet,
    /// An Org headline such as `** TODO [#A] Heading`.
    OrgHeadline,
}

/// One parser-accepted document-root outline header in source order.
///
/// All offsets are exact half-open UTF-8 byte offsets into the original input:
///
/// - [`header_start`](Self::header_start) is the first byte of the
///   parser-accepted structural header marker (including accepted indentation
///   for Markdown).
/// - [`structural_prefix`](Self::structural_prefix) is the source prefix Tine
///   removes to obtain the block raw. It includes indentation plus `-` for a
///   Markdown bullet, or the leading `*` run for an Org headline, followed by
///   exactly one ASCII space when present. It is empty for an unbulleted
///   Markdown ATX heading, whose entire heading line remains block raw.
/// - [`line_content`](Self::line_content) is the complete physical line without
///   its terminator.
/// - [`line`](Self::line) is the complete physical line including its `\n`,
///   `\r\n`, or lone `\r` terminator when present; at EOF it equals
///   `line_content`.
///
/// `structural_prefix.end` is deliberately earlier than lsdoc's semantic
/// heading-title start. Task markers, priorities, and an embedded ATX heading
/// after a Markdown dash therefore remain in the consumer-owned block raw.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutlineHeader {
    pub kind: OutlineHeaderKind,
    /// The parser's structural outline level.
    pub level: u32,
    pub header_start: usize,
    pub structural_prefix: SourceRange,
    pub line_content: SourceRange,
    pub line: SourceRange,
}

/// Source-oriented projection returned by [`crate::parse_outline`].
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentOutline {
    /// Parser-accepted document-root outline headers in source order.
    pub headers: Vec<OutlineHeader>,
}

/// The v2 parser could not take ownership of the input.
///
/// The production parser is intended to own all Markdown and Org input, so this
/// error indicates an internal parser-coverage defect rather than malformed
/// user syntax. The outline API returns an error instead of panicking so a
/// storage-oriented caller can fail closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OutlineParseError {
    ParserOwnership,
}

impl fmt::Display for OutlineParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParserOwnership => f.write_str("lsdoc v2 parser did not take ownership of input"),
        }
    }
}

impl std::error::Error for OutlineParseError {}

//! Shared top-level inline driver facts.
//!
//! Fresh/swallow model: mldoc has no explicit `fresh` flag. It runs the
//! format-specific `inline_choices` arm (`syntax/inline.ml:1342-1410`) and then
//! falls back to `plain` (`syntax/inline.ml:193-236, 1412`). `plain` either
//! consumes one plain delimiter byte or consumes a whole non-delimiter run. That
//! means a failed marker delimiter such as `[` or `#` leaves the next byte at a
//! fresh dispatch point, while a failed/swallowed opener or closer such as `(`
//! can absorb following ordinary bytes into the same plain run. lsdoc encodes
//! that same state as `fresh`: construct openers in the swallow families are
//! attempted only when `fresh`, and fallback updates `fresh` from the delimiter
//! tables below. These are the C6 defect-5 equivalence constraints documented in
//! `subagent-tasks/notes/c6-speccheck.md`: direct bare URLs and keyword
//! timestamps are tried only at mldoc iteration starts, not inside a swallowed
//! ordinary plain run.

/// mldoc `markdown_plain_delims` bytes, excluding whitespace
/// (`syntax/inline.ml:197-198`; whitespace is handled by
/// [`crate::inline::is_ws`]).
pub(crate) const MARKDOWN_PLAIN_DELIMITER_BYTES: &[u8] =
    &[b'\\', b'_', b'^', b'[', b'*', b'~', b'`', b'=', b'$', b'#'];

/// Markdown bytes whose failed dispatch falls through to a plain run that can
/// swallow following non-delimiters. This is the complement of explicit
/// marker-delimiter one-byte fallback for the punctuation bytes lsdoc lexes.
pub(crate) const MARKDOWN_SWALLOW_BYTES: &[u8] =
    &[b'!', b'(', b')', b'{', b'}', b'<', b'>', b']', b'@'];

/// mldoc `org_plain_delims` bytes, excluding whitespace
/// (`syntax/inline.ml:194-195`; the duplicate `^` is intentionally de-duped).
pub(crate) const ORG_PLAIN_DELIMITER_BYTES: &[u8] =
    &[b'\\', b'_', b'^', b'[', b'*', b'/', b'+', b'$', b'#'];

/// Org punctuation bytes that do not use marker-delimiter fallback in lsdoc's
/// token stream. `!` is included because the current behavior-preserving Org
/// driver has no image parser on that upstream arm.
pub(crate) const ORG_SWALLOW_BYTES: &[u8] = &[
    b'~', b'=', b'<', b'{', b'(', b'@', b']', b')', b'}', b'>', b'!',
];

#[inline]
pub(crate) fn markdown_plain_delimiter(c: u8) -> bool {
    MARKDOWN_PLAIN_DELIMITER_BYTES.contains(&c) || crate::inline::is_ws(c)
}

#[inline]
pub(crate) fn markdown_swallow_byte(c: u8) -> bool {
    MARKDOWN_SWALLOW_BYTES.contains(&c)
}

#[inline]
pub(crate) fn org_plain_delimiter(c: u8) -> bool {
    ORG_PLAIN_DELIMITER_BYTES.contains(&c) || crate::inline::is_ws(c)
}

#[inline]
pub(crate) fn org_swallow_byte(c: u8) -> bool {
    ORG_SWALLOW_BYTES.contains(&c)
}

//! Shared block-layer leaf predicates + infrastructure for the two block drivers.
//!
//! `parse.rs` (Markdown) and `org.rs` (Org) are intentionally PARALLEL implementations —
//! they encode genuinely different grammars (md `-`-bullets / def-lists / `>`-quotes /
//! hiccup-in-list vs org headlines / verbatim `:`-lines / stateful list-collapse / `:tags:`),
//! so their dispatch ladders and driver loops must NOT be merged. What they DO share is a set
//! of byte-identical leaf predicates, data structures, and infrastructure (line splitting, the
//! `#+END_` closer trie, fence/drawer index lookups, the task-marker table, the callout
//! `Builder`, the residual-recursion depth cap). Those live here once, `pub(crate)`, and both
//! drivers `use crate::block_common::…`. Each item below was verified byte-identical between the
//! two drivers before being lifted (modulo a comment or a `std::` path); the format-specific
//! near-twins (`fence_marker`, `split_markers`, `drawer_begin`, `flush_para`) stay per-file.

use crate::projection::{Block, Span};
use std::cell::Cell;

/// Anti-SIGABRT recursion floor on the ONE remaining native re-dispatch: the de-`>`'d reparse
/// FALLBACK for a `>`-quote body that contains a fenced-code / `#+BEGIN_X` callout / LaTeX env /
/// block hiccup — the four constructs whose recognizers use raw-input global closer indexes or
/// raw byte scans that literal `>`s defeat, so (unlike a whitespace strip, which every predicate
/// `trim_start`s through) they can't be recognized copy-free on the frame view and take a one-shot
/// de-`>` reparse (org `streaming_reparse` / md `reparse_block_content`, non-`in_item` branch).
///
/// This floor NO LONGER bounds any realistic parse. The former block/quote nesting cap is gone:
/// `#+BEGIN_X` bodies are zero-copy strip-view frames (P1/P2) and the `>`-quote staircase is
/// iterative `>`-container frames (P3) — both uncapped and O(n). The only thing that still
/// native-recurses is the fallback above, and only for *construct-in-`>`-quote* nesting, which
/// needs ~quadratic input for linear depth (each level costs a `>` AND a fenced/callout construct),
/// never occurs in real content, and which mldoc itself only handles by stack-overflowing (~1000).
/// lsdoc degrades it gracefully to a flat Paragraph at 64 rather than SIGABRT-ing at parse time —
/// a parser Tine embeds must not crash on malformed input. Each driver keeps its OWN thread-local
/// depth counter; only this constant is shared.
pub(crate) const GT_FALLBACK_NEST_CAP: usize = 64;

/// One source line: byte window `[start, end)` (end is just past the trailing terminator, or
/// EOF) plus the content text WITHOUT the trailing `\n`/`\r\n`.
pub(crate) struct Line<'a> {
    pub(crate) start: usize, // byte offset of line start
    pub(crate) end: usize,   // byte offset just past the trailing '\n' (or EOF)
    pub(crate) text: &'a str, // line content WITHOUT the trailing '\n'
}

/// The Logseq task markers (mldoc `marker` set), matched as a leading whole word on a
/// heading/bullet/headline title.
pub(crate) const MARKERS: &[&str] = &[
    "TODO",
    "DOING",
    "WAITING",
    "WAIT",
    "DONE",
    "CANCELED",
    "CANCELLED",
    "STARTED",
    "IN-PROGRESS",
    "NOW",
    "LATER",
];

/// Split `input` into lines, each carrying its byte window and terminator-stripped text.
/// Terminators: `\r\n` consumed as a unit, else a lone `\r`/`\n`.
pub(crate) fn split_lines(input: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let bytes = input.as_bytes();
    let n = input.len();
    let mut i = 0;
    while i < n {
        let start = i;
        let mut j = i;
        while j < n && bytes[j] != b'\n' && bytes[j] != b'\r' {
            j += 1;
        }
        let content_end = j;
        let end = if j < n {
            // consume the terminator: `\r\n` as a unit, else a lone `\r`/`\n`.
            if bytes[j] == b'\r' && j + 1 < n && bytes[j + 1] == b'\n' {
                j + 2
            } else {
                j + 1
            }
        } else {
            j
        };
        lines.push(Line { start, end, text: &input[start..content_end] });
        i = end;
    }
    lines
}

/// Count of leading spaces/tabs in `s`.
pub(crate) fn leading_ws(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

/// Is the open paragraph byte-window all whitespace (so it emits no Paragraph)?
pub(crate) fn para_ws_only(para: &Option<(usize, usize)>, input: &str) -> bool {
    match para {
        Some((s, e)) => input.as_bytes()[*s..*e]
            .iter()
            .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')),
        None => false,
    }
}

/// Split a leading list/heading checkbox (`[ ]`/`[x]`/`[X]`) off `s`; returns the checked
/// state (if any) and the trimmed remainder.
pub(crate) fn split_checkbox(s: &str) -> (Option<bool>, &str) {
    if let Some(r) = s.strip_prefix("[ ]") {
        (Some(false), r.trim_start())
    } else if let Some(r) = s.strip_prefix("[x]").or_else(|| s.strip_prefix("[X]")) {
        (Some(true), r.trim_start())
    } else {
        (None, s)
    }
}

/// Parse a drawer/property line `:KEY: value` → `(key, value)`. `None` for `:END:`, an empty
/// key, or a key containing whitespace.
pub(crate) fn drawer_property(s: &str) -> Option<(String, String)> {
    let t = s.trim_start().strip_prefix(':')?;
    let pos = t.find(':')?;
    let key = &t[..pos];
    if key.is_empty() || key.contains(' ') || key.contains('\t') || key.eq_ignore_ascii_case("end") {
        return None;
    }
    // value = rest of line after the key's closing `:`, trimmed (drops a leading space
    // and a trailing CR from CRLF inputs).
    let value = t[pos + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

/// Byte offset of a block-level `$$` opener in a line view: arbitrary leading
/// spaces/tabs are indentation, but trailing bytes after the closing delimiter
/// are a separate block.
pub(crate) fn displayed_math_opener(s: &str) -> Option<usize> {
    let off = leading_ws(s);
    s[off..].starts_with("$$").then_some(off)
}

/// First `$$` after a block-level opener, bounded to this block body's byte
/// window. This is intentionally a single monotone byte walk: no per-line
/// restart and no backtracking.
pub(crate) fn find_displayed_math_close(input: &str, opener: usize, body_end: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut p = opener + 2;
    let mut scanned = 0usize;
    while p + 1 < body_end {
        scanned += 1;
        if bytes[p] == b'$' && bytes[p + 1] == b'$' {
            crate::metrics::scan_work(scanned + 1);
            return Some(p);
        }
        p += 1;
    }
    crate::metrics::scan_work(scanned);
    None
}

/// State for raw-HTML callers. It is intentionally parse-pass local: when a known-tag
/// opener scans to this body's end and finds neither a matching `</tag>` nor a source-compatible
/// fallback `/>`, later same-tag openers in the same or a smaller remaining body can fail without
/// re-scanning to EOF. The grammar parser below is cache-free; callers own this advance-only memo.
pub(crate) struct RawHtmlScan {
    no_tag_end_until: Vec<usize>,
    no_special_until: [usize; 4],
}

impl RawHtmlScan {
    pub(crate) fn new() -> Self {
        Self {
            no_tag_end_until: vec![0; crate::inline::HICCUP_TAGS.len()],
            no_special_until: [0; 4],
        }
    }
}

pub(crate) struct RawHtmlCapture {
    pub(crate) text: String,
    pub(crate) span_start: usize,
    pub(crate) span_end: usize,
    pub(crate) next: usize,
    pub(crate) rewrite: Option<(usize, usize, usize)>, // line index, new start, content end
}

#[derive(Clone, Copy, Debug)]
enum RawHtmlHead<'a> {
    Tag { tag: &'a str, index: usize },
    Special { opener: &'static str, closer: &'static str, miss: usize },
}

const MAX_HTML_TAG_LEN: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RawHtmlExtent {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawHtmlMiss {
    NoGrammar,
    MissingTagCloser { index: usize },
    MissingSpecialCloser { miss: usize },
    UnbalancedTag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawHtmlAttempt {
    Match(RawHtmlExtent),
    Miss(RawHtmlMiss),
}

fn mldoc_is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | 0x0c | 0x1a)
}

fn raw_html_head_at(input: &str, at: usize, limit: usize, require_peek: bool) -> Option<RawHtmlHead<'_>> {
    if at >= limit || limit > input.len() || input.as_bytes().get(at) != Some(&b'<') {
        return None;
    }
    debug_assert!(crate::inline::HICCUP_TAGS.iter().all(|t| t.len() <= MAX_HTML_TAG_LEN));
    // mldoc Raw_html.parse begins with `peek_string 10`; Angstrom fails before dispatch when
    // fewer than ten bytes remain. This is the source of `<b>ab</b>` (9 bytes) being plain.
    if require_peek && limit.saturating_sub(at) < 10 {
        return None;
    }
    if starts_with_at(input.as_bytes(), at, b"<?", limit) {
        return Some(RawHtmlHead::Special { opener: "<?", closer: "?>", miss: 0 });
    }
    if starts_with_at(input.as_bytes(), at, b"<!--", limit) {
        return Some(RawHtmlHead::Special { opener: "<!--", closer: "-->", miss: 1 });
    }
    if starts_with_at(input.as_bytes(), at, b"<![CDATA[", limit) {
        // Source exact: mldoc 1.5.7 uses "]]" as the strict wrapper closer here.
        return Some(RawHtmlHead::Special { opener: "<![CDATA[", closer: "]]", miss: 2 });
    }
    if starts_with_at(input.as_bytes(), at, b"<!", limit) {
        return Some(RawHtmlHead::Special { opener: "<!", closer: ">", miss: 3 });
    }

    // mldoc raw_html.ml: after `<`, `take_till1 (is_space || (=) '>')` is the tag token.
    // Therefore `<br/>` has token `br/` and is NOT a known tag; `<br />` has token `br`
    // but still needs the later `peek_string 10` gate before Raw_html.parse can accept it.
    let b = input.as_bytes();
    let token_start = at + 1;
    let mut j = token_start;
    let mut scanned = 0usize;
    while j < limit && j - token_start < MAX_HTML_TAG_LEN {
        scanned += 1;
        if mldoc_is_space(b[j]) || b[j] == b'>' {
            break;
        }
        j += 1;
    }
    let overlong = j == token_start + MAX_HTML_TAG_LEN && j < limit && {
        scanned += 1;
        !mldoc_is_space(b[j]) && b[j] != b'>'
    };
    crate::metrics::scan_work(scanned);
    if overlong {
        return None;
    }
    if j == token_start {
        return None;
    }
    let tag = input.get(token_start..j)?;
    let index = crate::inline::known_html_tag_index(tag)?;
    Some(RawHtmlHead::Tag { tag, index })
}

fn raw_html_head_prefix(s: &str) -> Option<(usize, RawHtmlHead<'_>)> {
    let off = leading_ws(s);
    Some((off, raw_html_head_at(s, off, s.len(), false)?))
}

pub(crate) fn raw_html_block_start(s: &str) -> bool {
    raw_html_head_prefix(s).is_some()
}

fn starts_with_ci_at(bytes: &[u8], at: usize, needle: &[u8], end: usize) -> bool {
    at + needle.len() <= end
        && bytes[at..at + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn starts_with_at(bytes: &[u8], at: usize, needle: &[u8], end: usize) -> bool {
    at + needle.len() <= end && &bytes[at..at + needle.len()] == needle
}

fn find_exact_bounded(bytes: &[u8], from: usize, end: usize, needle: &[u8]) -> (Option<usize>, usize) {
    if needle.is_empty() || from > end || needle.len() > end.saturating_sub(from) {
        return (None, 0);
    }
    let mut p = from;
    let mut scanned = 0usize;
    while p + needle.len() <= end {
        scanned += 1;
        if &bytes[p..p + needle.len()] == needle {
            return (Some(p), scanned + needle.len().saturating_sub(1));
        }
        p += 1;
    }
    (None, scanned)
}

fn find_end_string_bounded(bytes: &[u8], from: usize, end: usize, closer: &[u8]) -> (Option<usize>, usize) {
    let (found, scanned) = find_exact_bounded(bytes, from, end, closer);
    if closer.len() == 1 && found == Some(from) {
        return (None, scanned);
    }
    (found, scanned)
}

fn count_tag_opens_in_chunk(
    bytes: &[u8],
    from: usize,
    end: usize,
    open_plain: &[u8],
    open_with_attrs: &[u8],
) -> (usize, usize) {
    let mut count = 0usize;
    let mut q = from;
    let mut scanned = 0usize;
    while q < end {
        scanned += 1;
        if starts_with_at(bytes, q, open_plain, end) {
            count += 1;
            q += open_plain.len();
        } else if starts_with_at(bytes, q, open_with_attrs, end) {
            count += 1;
            q += open_with_attrs.len();
        } else {
            q += 1;
        }
    }
    (count, scanned)
}

fn parse_raw_html_impl(input: &str, opener: usize, body_end: usize) -> RawHtmlAttempt {
    let Some(head) = raw_html_head_at(input, opener, body_end, true) else {
        return RawHtmlAttempt::Miss(RawHtmlMiss::NoGrammar);
    };
    match head {
        RawHtmlHead::Special { opener: open, closer, miss } => {
            let from = opener + open.len();
            let (found, scanned) =
                find_end_string_bounded(input.as_bytes(), from, body_end, closer.as_bytes());
            crate::metrics::scan_work(scanned);
            match found {
                Some(pos) => RawHtmlAttempt::Match(RawHtmlExtent { start: opener, end: pos + closer.len() }),
                None => RawHtmlAttempt::Miss(RawHtmlMiss::MissingSpecialCloser { miss }),
            }
        }
        RawHtmlHead::Tag { tag, index } => {
            let after_tag = opener + 1 + tag.len();
            let bytes = input.as_bytes();
            let close_tag = format!("</{}>", tag);
            let open_tag = format!("<{}>", tag);
            let open_attr = format!("<{} ", tag);
            let close = close_tag.as_bytes();
            let open_plain = open_tag.as_bytes();
            let open_with_attrs = open_attr.as_bytes();

            let mut level = 1isize;
            let mut p = after_tag;
            let mut chunk_start = after_tag;
            let mut scanned = 0usize;
            let mut saw_close = false;
            let mut first_self_close = None;
            while p < body_end {
                scanned += 1;
                if first_self_close.is_none() && starts_with_at(bytes, p, b"/>", body_end) {
                    first_self_close = Some(p + 2);
                }
                if starts_with_ci_at(bytes, p, close, body_end) {
                    let (opens, chunk_scanned) =
                        count_tag_opens_in_chunk(bytes, chunk_start, p, open_plain, open_with_attrs);
                    scanned += chunk_scanned;
                    saw_close = true;
                    level += opens as isize;
                    level -= 1;
                    p += close.len();
                    chunk_start = p;
                    if level <= 0 {
                        crate::metrics::scan_work(scanned);
                        return RawHtmlAttempt::Match(RawHtmlExtent { start: opener, end: p });
                    }
                    continue;
                }
                p += 1;
            }
            crate::metrics::scan_work(scanned);
            if let Some(end) = first_self_close {
                return RawHtmlAttempt::Match(RawHtmlExtent { start: opener, end });
            }
            if saw_close {
                RawHtmlAttempt::Miss(RawHtmlMiss::UnbalancedTag)
            } else {
                RawHtmlAttempt::Miss(RawHtmlMiss::MissingTagCloser { index })
            }
        }
    }
}

pub(crate) fn parse_raw_html_at(input: &str, opener: usize, body_end: usize) -> Option<RawHtmlExtent> {
    if opener > body_end || body_end > input.len() {
        return None;
    }
    match parse_raw_html_impl(input, opener, body_end) {
        RawHtmlAttempt::Match(extent) => Some(extent),
        RawHtmlAttempt::Miss(_) => None,
    }
}

pub(crate) fn parse_raw_html_at_cached(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
) -> Option<RawHtmlExtent> {
    if opener > body_end || body_end > input.len() {
        return None;
    }
    let head = raw_html_head_at(input, opener, body_end, true)?;
    if let Some(s) = state.as_ref() {
        match head {
            RawHtmlHead::Tag { index, .. } if s.no_tag_end_until[index] >= body_end => return None,
            RawHtmlHead::Special { miss, .. } if s.no_special_until[miss] >= body_end => return None,
            _ => {}
        }
    }
    match parse_raw_html_impl(input, opener, body_end) {
        RawHtmlAttempt::Match(extent) => Some(extent),
        RawHtmlAttempt::Miss(RawHtmlMiss::MissingTagCloser { index }) => {
            if let Some(s) = state {
                s.no_tag_end_until[index] = body_end;
            }
            None
        }
        RawHtmlAttempt::Miss(RawHtmlMiss::MissingSpecialCloser { miss }) => {
            if let Some(s) = state {
                s.no_special_until[miss] = body_end;
            }
            None
        }
        RawHtmlAttempt::Miss(_) => None,
    }
}

pub(crate) fn raw_html_end_at(
    input: &str,
    opener: usize,
    body_end: usize,
    state: &mut RawHtmlScan,
) -> Option<usize> {
    if opener > body_end {
        return None;
    }
    let off = leading_ws(input.get(opener..body_end)?);
    parse_raw_html_at_cached(input, opener + off, body_end, Some(state)).map(|e| e.end)
}

fn line_view_abs_start(line: &Line<'_>, view: &str) -> usize {
    debug_assert!(line.text.ends_with(view));
    line.start + line.text.len() - view.len()
}

pub(crate) fn raw_html_raw_capture<'a>(
    lines: &[Line<'a>],
    cur: usize,
    hi: usize,
    body_end: usize,
    input: &'a str,
    first_view: &str,
    state: &mut RawHtmlScan,
) -> Option<RawHtmlCapture> {
    let (opener_off, _) = raw_html_head_prefix(first_view)?;
    let opener = line_view_abs_start(&lines[cur], first_view) + opener_off;
    let close_end = parse_raw_html_at_cached(input, opener, body_end, Some(state))?.end;
    let mut close_line = cur;
    while close_line < hi && lines[close_line].start + lines[close_line].text.len() < close_end {
        close_line += 1;
    }
    if close_line >= hi {
        return None;
    }
    let content_end = lines[close_line].start + lines[close_line].text.len();
    let text = input[opener..close_end].to_string();
    if close_end < content_end {
        Some(RawHtmlCapture {
            text,
            span_start: opener,
            span_end: close_end,
            next: close_line,
            rewrite: Some((close_line, close_end, content_end)),
        })
    } else {
        let mut next = close_line + 1;
        let mut span_end = lines[close_line].end;
        while next < hi && lines[next].text.is_empty() {
            span_end = lines[next].end;
            next += 1;
        }
        Some(RawHtmlCapture { text, span_start: opener, span_end, next, rewrite: None })
    }
}

pub(crate) fn raw_html_view_capture<'a>(
    lines: &[Line<'a>],
    cur: usize,
    hi: usize,
    strip: usize,
    first_view: &str,
) -> Option<RawHtmlCapture> {
    let (opener_off, _) = raw_html_head_prefix(first_view)?;
    let mut body = String::new();
    let mut line_starts = Vec::new();
    let mut k = cur;
    loop {
        line_starts.push(body.len());
        if k == cur {
            body.push_str(first_view);
        } else {
            body.push_str(crate::org::strip_view(lines[k].text, strip));
        }
        k += 1;
        if k >= hi {
            break;
        }
        body.push('\n');
    }
    let close_end_view = parse_raw_html_at(&body, opener_off, body.len())?.end;
    let mut close_line = cur;
    let mut close_line_view_start = 0usize;
    for (idx, start) in line_starts.iter().enumerate() {
        let line_idx = cur + idx;
        let view_len = if line_idx == cur {
            first_view.len()
        } else {
            crate::org::strip_view(lines[line_idx].text, strip).len()
        };
        if close_end_view <= start + view_len {
            close_line = line_idx;
            close_line_view_start = *start;
            break;
        }
    }
    let close_view = if close_line == cur {
        first_view
    } else {
        crate::org::strip_view(lines[close_line].text, strip)
    };
    let close_end_in_line = close_end_view - close_line_view_start;
    let close_abs = line_view_abs_start(&lines[close_line], close_view) + close_end_in_line;
    let content_end = lines[close_line].start + lines[close_line].text.len();
    let text = body[opener_off..close_end_view].to_string();
    let span_start = line_view_abs_start(&lines[cur], first_view) + opener_off;
    if close_end_in_line < close_view.len() {
        Some(RawHtmlCapture {
            text,
            span_start,
            span_end: close_abs,
            next: close_line,
            rewrite: Some((close_line, close_abs, content_end)),
        })
    } else {
        let mut next = close_line + 1;
        let mut span_end = lines[close_line].end;
        while next < hi && lines[next].text.is_empty() {
            span_end = lines[next].end;
            next += 1;
        }
        Some(RawHtmlCapture { text, span_start, span_end, next, rewrite: None })
    }
}

/// Next fence-marker line at/after `from`, advancing the monotone `cursor` (the drivers reach
/// fence openers in increasing `from`, so the cursor only advances — O(1) amortized).
pub(crate) fn find_matching_fence(fence_lines: &[usize], cursor: &mut usize, from: usize) -> Option<usize> {
    // the main loop reaches fence openers in increasing `from`, so the cursor only advances.
    while *cursor < fence_lines.len() && fence_lines[*cursor] <= from {
        *cursor += 1;
    }
    fence_lines.get(*cursor).copied()
}

/// First `:END:` drawer-closer line strictly after `from`, via a monotone `cursor` (advance-only,
/// like [`find_matching_fence`]): the drivers reach drawer openers in increasing `from`, so the
/// cursor only advances ⇒ O(1) amortized, not the O(log n) of a per-opener binary search. The
/// cursor stops AT (does not consume) the first closer `> from`, so a repeated/equal `from` is
/// idempotent.
pub(crate) fn find_drawer_end(drawer_end_idxs: &[usize], cursor: &mut usize, from: usize) -> Option<usize> {
    while *cursor < drawer_end_idxs.len() && drawer_end_idxs[*cursor] <= from {
        *cursor += 1;
    }
    drawer_end_idxs.get(*cursor).copied()
}

/// A `#+END_<name>` closer trie: index every closer line under the lowercased leading run of
/// its name, so the drivers find the first closer after a given line whose name prefix-matches
/// an opener in O(|name|) with no EOF scan.
///
/// Child links are a small sorted-by-insertion `Vec<(byte, node)>` linear-scanned, NOT a
/// `HashMap<u8, _>` — the fan-out per node is tiny (the distinct next-letters of `#+END_` names)
/// and a byte is a perfect array key, so hashing it (SipHash!) is strictly more work than a
/// handful of byte compares. See lsdoc/CLAUDE.md "avoid hashes if an array would do".
pub(crate) struct EndTrie {
    kids: Vec<Vec<(u8, u32)>>, // node → child links (byte → child node), tiny fan-out → linear scan
    ends: Vec<Vec<usize>>,     // node → `#+END_` line indexes with this prefix (ascending)
    cursor: Vec<Cell<usize>>,  // node → monotone read cursor into `ends` (advance-only)
}
impl EndTrie {
    pub(crate) fn new() -> Self {
        EndTrie { kids: vec![Vec::new()], ends: vec![Vec::new()], cursor: vec![Cell::new(0)] }
    }
    /// Index `#+END_` line `idx` under the leading non-ws run of `suffix` (the text after
    /// `#+END_`), lowercased. The empty prefix (root) matches any opener name (incl. `""`).
    pub(crate) fn insert(&mut self, suffix: &str, idx: usize) {
        let mut node = 0usize;
        self.ends[node].push(idx);
        for &b in suffix.as_bytes() {
            if b == b' ' || b == b'\t' {
                break;
            }
            let lb = b.to_ascii_lowercase();
            node = match self.kids[node].iter().find(|&&(k, _)| k == lb) {
                Some(&(_, c)) => c as usize,
                None => {
                    let c = self.kids.len();
                    self.kids.push(Vec::new());
                    self.ends.push(Vec::new());
                    self.cursor.push(Cell::new(0));
                    self.kids[node].push((lb, c as u32));
                    c
                }
            };
            self.ends[node].push(idx);
        }
    }
    /// First `#+END_` line after `from` whose name starts with `name` (case-insensitive), or
    /// `None` (unclosed/mismatched — O(|name|), no EOF scan). Byte-exact to the old prefix scan.
    ///
    /// Successor lookup via a per-node MONOTONE CURSOR (advance-only), not `partition_point`:
    /// the block drivers query each node with non-decreasing `from` (the main-loop line index `i`
    /// only advances; the headline-split lookahead re-asks at the SAME `i` — idempotent), so
    /// skipping ends `<= from` is O(1) amortized and the whole closer phase is O(n), not O(n log n).
    /// Correct across demotion/nesting: a demoted/inner opener still only asks "first end > from",
    /// and equal/repeated `from` never rewinds the cursor. A fresh `EndTrie` (hence fresh cursors)
    /// is built per parse pass, so recursive sub-parses don't share cursor state.
    pub(crate) fn find(&self, name: &str, from: usize) -> Option<usize> {
        let mut node = 0usize;
        for &b in name.as_bytes() {
            let lb = b.to_ascii_lowercase();
            node = self.kids[node].iter().find(|&&(k, _)| k == lb).map(|&(_, c)| c as usize)?;
        }
        let v = &self.ends[node];
        let cur = &self.cursor[node];
        let mut c = cur.get();
        while c < v.len() && v[c] <= from {
            c += 1;
        }
        cur.set(c);
        v.get(c).copied()
    }
}

/// A captured callout opener (`#+BEGIN_QUOTE` / `#+BEGIN_<custom>`): emitted as the right
/// block once its body children are known.
pub(crate) enum Builder {
    Quote,
    Custom(String),
}
impl Builder {
    pub(crate) fn finish(self, children: Vec<Block>, span: Option<Span>) -> Block {
        match self {
            Builder::Quote => Block::Quote { children, span },
            Builder::Custom(name) => Block::Custom { name, children, span },
        }
    }
}

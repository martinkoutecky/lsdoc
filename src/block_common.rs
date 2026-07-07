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
    pub(crate) start: usize,  // byte offset of line start
    pub(crate) end: usize,    // byte offset just past the trailing '\n' (or EOF)
    pub(crate) text: &'a str, // line content WITHOUT the trailing '\n'
    /// Synthetic mid-line remainders have already passed the frame's clear-indent point.
    /// Re-dispatch must see them verbatim, not through the enclosing strip view.
    pub(crate) no_strip: bool,
}

#[inline]
pub(crate) fn mldoc_ltrim_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\x0c' | b'\n' | b'\r' | b'\t')
}

#[inline]
pub(crate) fn mldoc_ltrim_prefix_at_most(text: &str, limit: usize) -> usize {
    let mut n = 0;
    for &b in text.as_bytes() {
        if n == limit || !mldoc_ltrim_byte(b) {
            break;
        }
        n += 1;
    }
    crate::metrics::scan_work(n);
    n
}

#[inline]
fn strip_view_from_prefix(text: &str, strip: usize, prefix: usize) -> &str {
    if prefix >= strip {
        if text.len() > strip {
            &text[strip..]
        } else {
            text
        }
    } else if prefix == text.len() {
        text
    } else {
        &text[prefix..]
    }
}

/// Per-line de-indent view: equivalent to mldoc's clear-indents map on ONE line, with a
/// bounded prefix scan and no alloc. `strip` = the cumulative first-line indent cleared by
/// ancestor frames. Branch tests use mldoc's ltrim byte set (`' '`, `'\f'`, `'\n'`, `'\r'`,
/// `'\t'`), while the branch-1 removal blindly strips `strip` bytes. Matches mldoc's
/// `safe_sub` quirk: a line whose length is exactly `strip` is returned unchanged rather than
/// sliced to `""`.
pub(crate) fn strip_view(text: &str, strip: usize) -> &str {
    if strip == 0 {
        return text;
    }
    let prefix = mldoc_ltrim_prefix_at_most(text, strip);
    strip_view_from_prefix(text, strip, prefix)
}

const STRIP_INF: usize = usize::MAX;

/// Exact sequential all-whitespace clear-indent emulator for active positive `#+BEGIN_X`
/// frame increments. The stack is bottom-to-top (outermost first); zero increments are excluded
/// by `push`, which keeps query charging tied to successful positive subtractions.
pub(crate) struct StripSeqTree {
    values: Vec<usize>,
    tree: Vec<usize>,
    base: usize,
}

impl StripSeqTree {
    pub(crate) fn new() -> Self {
        Self {
            values: Vec::new(),
            tree: Vec::new(),
            base: 0,
        }
    }

    #[inline]
    pub(crate) fn active_positive_len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn push(&mut self, increment: usize) -> bool {
        if increment == 0 {
            return false;
        }
        self.ensure_capacity_for_push();
        let idx = self.values.len();
        self.values.push(increment);
        self.set_leaf(idx, increment);
        true
    }

    pub(crate) fn pop_positive(&mut self) {
        let idx = self
            .values
            .len()
            .checked_sub(1)
            .expect("strip tree pop underflow");
        self.values.pop();
        self.set_leaf(idx, STRIP_INF);
    }

    fn ensure_capacity_for_push(&mut self) {
        if self.values.len() < self.base {
            return;
        }
        let new_base = if self.base == 0 { 1 } else { self.base * 2 };
        let mut tree = vec![STRIP_INF; new_base * 2];
        crate::metrics::scan_work(tree.len());
        for (idx, &v) in self.values.iter().enumerate() {
            tree[new_base + idx] = v;
            crate::metrics::scan_work(1);
        }
        for node in (1..new_base).rev() {
            tree[node] = tree[node * 2].min(tree[node * 2 + 1]);
            crate::metrics::scan_work(1);
        }
        self.base = new_base;
        self.tree = tree;
    }

    fn set_leaf(&mut self, idx: usize, value: usize) {
        debug_assert!(idx < self.base);
        let mut node = self.base + idx;
        self.tree[node] = value;
        crate::metrics::scan_work(1);
        while node > 1 {
            node /= 2;
            self.tree[node] = self.tree[node * 2].min(self.tree[node * 2 + 1]);
            crate::metrics::scan_work(1);
        }
    }

    pub(crate) fn seq_ws_len(&self, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let mut v = len;
        let mut p = 0usize;
        while let Some(q) = self.first_lt_from(p, v) {
            let s = self.values[q];
            debug_assert!(s > 0 && s < v);
            v -= s;
            p = q + 1;
        }
        v
    }

    fn first_lt_from(&self, start: usize, threshold: usize) -> Option<usize> {
        if start >= self.values.len() || self.base == 0 {
            return None;
        }
        self.first_lt_node(1, 0, self.base, start, threshold)
    }

    fn first_lt_node(
        &self,
        node: usize,
        lo: usize,
        hi: usize,
        start: usize,
        threshold: usize,
    ) -> Option<usize> {
        crate::metrics::scan_work(1);
        if hi <= start || lo >= self.values.len() || self.tree[node] >= threshold {
            return None;
        }
        if hi - lo == 1 {
            return Some(lo);
        }
        let mid = lo + (hi - lo) / 2;
        self.first_lt_node(node * 2, lo, mid, start, threshold)
            .or_else(|| self.first_lt_node(node * 2 + 1, mid, hi, start, threshold))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StripCtx<'a> {
    total: usize,
    seq: Option<&'a StripSeqTree>,
}

impl<'a> StripCtx<'a> {
    pub(crate) fn new(total: usize, seq: &'a StripSeqTree) -> Self {
        Self {
            total,
            seq: (total > 0 && seq.active_positive_len() >= 2).then_some(seq),
        }
    }

    pub(crate) fn view_text<'t>(self, text: &'t str) -> &'t str {
        let Some(seq) = self.seq else {
            return strip_view(text, self.total);
        };
        let prefix = mldoc_ltrim_prefix_at_most(text, self.total);
        // The capped prefix scan can prove "whole line is mldoc whitespace" only when it reaches
        // the physical line end. That is sufficient: sequential and cumulative clearing can differ
        // only for all-whitespace lines with `len <= total`. If `len > total`, every positive
        // stage subtracts and the old cumulative `strip_view` result is exact.
        if prefix == text.len() {
            let keep = seq.seq_ws_len(text.len());
            return &text[text.len() - keep..];
        }
        strip_view_from_prefix(text, self.total, prefix)
    }
}

impl<'a> Line<'a> {
    pub(crate) fn viewed_text(&self, ctx: StripCtx<'_>) -> &'a str {
        if self.no_strip {
            self.text
        } else {
            ctx.view_text(self.text)
        }
    }

    fn has_eol(&self) -> bool {
        self.end > self.start + self.text.len()
    }
}

#[cfg(test)]
mod strip_seq_tests {
    use super::StripSeqTree;

    fn brute_seq_len(stack: &[usize], len: usize) -> usize {
        let mut v = len;
        for &s in stack {
            if s > 0 && v > s {
                v -= s;
            }
        }
        v
    }

    fn next_rand(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state
    }

    #[test]
    fn strip_seq_matches_bruteforce_random_stacks() {
        for seed in [1u64, 7, 99, 20260703] {
            let mut rng = seed;
            for _ in 0..200 {
                let depth = (next_rand(&mut rng) % 32) as usize;
                let mut tree = StripSeqTree::new();
                let mut stack = Vec::new();
                for _ in 0..depth {
                    let s = (next_rand(&mut rng) % 12) as usize;
                    if tree.push(s) {
                        stack.push(s);
                    }
                }
                for len in 0..80 {
                    assert_eq!(tree.seq_ws_len(len), brute_seq_len(&stack, len));
                }
            }
        }
    }

    #[test]
    fn strip_seq_pop_then_sibling_reuses_leaf_exactly() {
        let mut tree = StripSeqTree::new();
        let mut stack = Vec::new();
        for s in [2, 3, 4] {
            assert!(tree.push(s));
            stack.push(s);
        }
        assert!(tree.push(1));
        stack.push(1);
        for len in 0..20 {
            assert_eq!(tree.seq_ws_len(len), brute_seq_len(&stack, len));
        }
        tree.pop_positive();
        stack.pop();
        assert!(tree.push(6));
        stack.push(6);
        for len in 0..30 {
            assert_eq!(tree.seq_ws_len(len), brute_seq_len(&stack, len));
        }
    }

    #[test]
    fn strip_seq_excludes_zero_increments() {
        let mut tree = StripSeqTree::new();
        assert!(!tree.push(0));
        assert_eq!(tree.active_positive_len(), 0);
        assert!(tree.push(2));
        assert!(!tree.push(0));
        assert!(tree.push(1));
        assert_eq!(tree.active_positive_len(), 2);
        assert_eq!(tree.seq_ws_len(2), 1);
    }
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
// scan-owner: (b) monotone cursor — initial input-to-Line table split
pub(crate) fn split_lines(input: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let bytes = input.as_bytes();
    let n = input.len();
    crate::metrics::scan_work(n);
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
        lines.push(Line {
            start,
            end,
            text: &input[start..content_end],
            no_strip: false,
        });
        i = end;
    }
    lines
}

/// Count of leading spaces/tabs in `s`.
// scan-owner: (a2) caller-owned slice helper — leading whitespace scan over caller-owned line/view slice
pub(crate) fn leading_ws(s: &str) -> usize {
    let n = s.bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    crate::metrics::scan_work(n + usize::from(n < s.len()));
    n
}

/// mldoc `Parsers.is_space` (`lib/parsers.ml`): space, tab, SUB, FF.
pub(crate) fn mldoc_is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | 0x1a | 0x0c)
}

/// Count leading mldoc parser spaces (`Parsers.spaces` / `tabs_or_ws`).
// scan-owner: (a2) caller-owned slice helper — trim/space scan over caller-owned line/view slice
pub(crate) fn mldoc_spaces_len(s: &str) -> usize {
    let n = s
        .as_bytes()
        .iter()
        .take_while(|&&b| mldoc_is_space(b))
        .count();
    crate::metrics::scan_work(n + usize::from(n < s.len()));
    n
}

pub(crate) fn mldoc_trim_spaces_start(s: &str) -> &str {
    &s[mldoc_spaces_len(s)..]
}

/// Byte length after trimming trailing mldoc parser spaces.
// scan-owner: (a2) caller-owned slice helper — trim/space scan over caller-owned line/view slice
pub(crate) fn mldoc_trim_spaces_end_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut k = b.len();
    while k > 0 && mldoc_is_space(b[k - 1]) {
        k -= 1;
    }
    crate::metrics::scan_work(b.len() - k + usize::from(k > 0));
    k
}

pub(crate) fn mldoc_trim_spaces(s: &str) -> &str {
    let s = mldoc_trim_spaces_start(s);
    &s[..mldoc_trim_spaces_end_len(s)]
}

/// In-line projection of mldoc `whitespace_chars` for heading marker boundaries
/// (`lib/parsers.ml`, `lib/syntax/heading0.ml`): space, tab, FF, or line end.
/// CR/LF are absent from `Line::text`; SUB is intentionally not accepted.
pub(crate) fn mldoc_heading_boundary(s: &str) -> bool {
    s.is_empty() || matches!(s.as_bytes()[0], b' ' | b'\t' | 0x0c)
}

#[inline]
pub(crate) fn ocaml_trim_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// OCaml `String.trim` byte set: space, tab, LF, CR, FF. SUB is not trimmed.
// scan-owner: (a2) caller-owned slice helper — trim/space scan over caller-owned line/view slice
pub(crate) fn ocaml_trim(s: &str) -> &str {
    let b = s.as_bytes();
    let mut start = 0usize;
    let mut end = b.len();
    while start < end && ocaml_trim_byte(b[start]) {
        start += 1;
    }
    let leading_work = start + usize::from(start < b.len());
    while end > start && ocaml_trim_byte(b[end - 1]) {
        end -= 1;
    }
    let trailing_work = b.len() - end + usize::from(end > start);
    crate::metrics::scan_work(leading_work + trailing_work);
    &s[start..end]
}

// scan-owner: (a2) caller-owned slice helper — trim/space scan over caller-owned line/view slice
pub(crate) fn ocaml_trim_end(s: &str) -> &str {
    let b = s.as_bytes();
    let mut end = b.len();
    while end > 0 && ocaml_trim_byte(b[end - 1]) {
        end -= 1;
    }
    crate::metrics::scan_work(b.len() - end + usize::from(end > 0));
    &s[..end]
}

/// mldoc `get_indent` for the first body line of a `#+BEGIN_X` frame.
pub(crate) fn first_body_indent(s: &str) -> usize {
    let indent = leading_ws(s);
    if indent == s.len() {
        0
    } else {
        indent
    }
}

#[cfg(test)]
mod first_body_indent_tests {
    use super::first_body_indent;

    #[test]
    fn matches_mldoc_get_indent_first_line_rule() {
        assert_eq!(first_body_indent(""), 0);
        assert_eq!(first_body_indent(" \t"), 0);
        assert_eq!(first_body_indent("\x0c  "), 0);
        assert_eq!(first_body_indent(" \tx"), 2);
        assert_eq!(first_body_indent("\t  x"), 3);
    }
}

/// Is the open paragraph byte-window all whitespace (so it emits no Paragraph)?
// scan-owner: (a2) caller-owned slice helper — whitespace check over current paragraph window
pub(crate) fn para_ws_only(para: &Option<(usize, usize)>, input: &str) -> bool {
    match para {
        Some((s, e)) => {
            crate::metrics::scan_work(e.saturating_sub(*s));
            input.as_bytes()[*s..*e]
                .iter()
                .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        }
        None => false,
    }
}

/// Split a leading list/heading checkbox (`[ ]`/`[x]`/`[X]`) off `s`; returns the checked
/// state (if any) and the trimmed remainder.
pub(crate) fn split_checkbox(s: &str) -> (Option<bool>, &str) {
    if let Some(r) = s.strip_prefix("[ ]") {
        (Some(false), mldoc_trim_spaces_start(r))
    } else if let Some(r) = s.strip_prefix("[x]").or_else(|| s.strip_prefix("[X]")) {
        (Some(true), mldoc_trim_spaces_start(r))
    } else {
        (None, s)
    }
}

/// Parse a drawer/property line `:KEY: value` → `(key, value)`.
/// mldoc `drawer.ml:25-41`: leading/value `spaces` are MSPACE; key rejects
/// `:` and literal space only (tab is a legal key byte), plus eol and `end`.
// scan-owner: (a2) caller-owned line helper — drawer/property search and copy on one line
pub(crate) fn drawer_property(s: &str) -> Option<(String, String)> {
    let t = mldoc_trim_spaces_start(s).strip_prefix(':')?;
    let pos = match t.find(':') {
        Some(pos) => {
            crate::metrics::scan_work(pos + 1);
            pos
        }
        None => {
            crate::metrics::scan_work(t.len());
            return None;
        }
    };
    let key = &t[..pos];
    crate::metrics::scan_work(key.len());
    if key.is_empty()
        || key
            .bytes()
            .any(|b| b == b':' || b == b' ' || b == b'\n' || b == b'\r')
        || key.eq_ignore_ascii_case("end")
    {
        return None;
    }
    let value = mldoc_trim_spaces_start(&t[pos + 1..]);
    crate::metrics::scan_work(key.len() + value.len());
    Some((key.to_string(), value.to_string()))
}

/// Byte offset of a block-level `$$` opener in a line view: arbitrary leading
/// spaces/tabs are indentation, but trailing bytes after the closing delimiter
/// are a separate block.
pub(crate) fn displayed_math_opener(s: &str) -> Option<usize> {
    let off = mldoc_spaces_len(s);
    s[off..].starts_with("$$").then_some(off)
}

/// First `$$` after a block-level opener, bounded to this block body's byte
/// window. This is intentionally a single monotone byte walk: no per-line
/// restart and no backtracking.
pub(crate) fn find_displayed_math_close(
    input: &str,
    opener: usize,
    body_end: usize,
) -> Option<usize> {
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
    tag_indexes: Vec<RawHtmlTagCache>,
    #[cfg(debug_assertions)]
    input_id: Option<(usize, usize)>,
}

impl RawHtmlScan {
    pub(crate) fn new() -> Self {
        Self {
            no_tag_end_until: vec![0; crate::inline::HICCUP_TAGS.len()],
            no_special_until: [0; 4],
            tag_indexes: Vec::new(),
            #[cfg(debug_assertions)]
            input_id: None,
        }
    }

    fn guard_input(&mut self, _input: &str) {
        #[cfg(debug_assertions)]
        {
            let id = (_input.as_ptr() as usize, _input.len());
            match self.input_id {
                Some(prev) => {
                    debug_assert_eq!(prev, id, "RawHtmlScan reused with a different input string")
                }
                None => self.input_id = Some(id),
            }
        }
    }

    fn tag_index<'a>(&'a mut self, input: &str, tag: &str) -> &'a mut RawHtmlTagIndex {
        self.guard_input(input);
        for pos in 0..self.tag_indexes.len() {
            crate::metrics::scan_work(1);
            if self.tag_indexes[pos].tag == tag {
                return &mut self.tag_indexes[pos].index;
            }
        }
        crate::metrics::scan_work(1);
        self.tag_indexes.push(RawHtmlTagCache {
            tag: tag.to_string(),
            index: RawHtmlTagIndex::build(input.as_bytes(), tag),
        });
        let last = self.tag_indexes.len() - 1;
        &mut self.tag_indexes[last].index
    }
}

struct RawHtmlTagCache {
    tag: String,
    index: RawHtmlTagIndex,
}

#[derive(Clone, Copy)]
struct RawHtmlQueryRanks {
    event_rank: usize,
    close_rank: usize,
    self_rank: usize,
}

const RAW_HTML_NO_NSE: usize = usize::MAX;

struct RawHtmlTagIndex {
    input_len: usize,
    close_len: usize,
    event_pos: Vec<usize>,
    #[cfg(test)]
    event_prefix_after: Vec<isize>,
    next_strict_below_close: Vec<usize>,
    virtual_next_strict_below_close: usize,
    close_pos: Vec<usize>,
    self_close_pos: Vec<usize>,
    last_after_tag: usize,
    event_cursor: usize,
    close_cursor: usize,
    self_close_cursor: usize,
}

impl RawHtmlTagIndex {
    fn build(bytes: &[u8], tag: &str) -> Self {
        // scan-owner: (b) global precompute table + monotone cursors, bounded tag universe — raw-HTML per-tag index build
        let close_tag = format!("</{}>", tag);
        let open_tag = format!("<{}>", tag);
        let open_attr = format!("<{} ", tag);
        let close = close_tag.as_bytes();
        let open_plain = open_tag.as_bytes();
        let open_with_attrs = open_attr.as_bytes();
        let input_len = bytes.len();

        let mut events: Vec<(usize, isize)> = Vec::new();
        let mut close_pos = Vec::new();
        let mut self_close_pos = Vec::new();
        let mut open_cursor = 0usize;
        let mut close_cursor = 0usize;
        let mut self_cursor = 0usize;
        let mut scanned = 0usize;

        // scan-owner: (b) global precompute table + monotone cursors, bounded tag universe — raw-HTML per-tag event scan
        for pos in 0..input_len {
            scanned += 1;
            if pos == open_cursor {
                if starts_with_at(bytes, pos, open_plain, input_len) {
                    events.push((pos, 1));
                    open_cursor += open_plain.len();
                } else if starts_with_at(bytes, pos, open_with_attrs, input_len) {
                    events.push((pos, 1));
                    open_cursor += open_with_attrs.len();
                } else {
                    open_cursor += 1;
                }
            }
            if pos == close_cursor {
                if close_cursor + close.len() <= input_len {
                    if starts_with_ci_at(bytes, pos, close, input_len) {
                        close_pos.push(pos);
                        events.push((pos, -1));
                        close_cursor += close.len();
                    } else {
                        close_cursor += 1;
                    }
                } else {
                    close_cursor += 1;
                }
            }
            if pos == self_cursor {
                if self_cursor + 1 < input_len {
                    if starts_with_at(bytes, pos, b"/>", input_len) {
                        self_close_pos.push(pos);
                        self_cursor += 2;
                    } else {
                        self_cursor += 1;
                    }
                } else {
                    self_cursor += 1;
                }
            }
        }
        crate::metrics::scan_work(scanned);

        let mut event_pos = Vec::with_capacity(events.len());
        let mut event_delta = Vec::with_capacity(events.len());
        let mut event_prefix_after = Vec::with_capacity(events.len());
        let mut prefix = 0isize;
        // scan-owner: (b) global precompute table + monotone cursors, bounded tag universe — raw-HTML event prefix table
        for (pos, delta) in events {
            prefix += delta;
            event_pos.push(pos);
            event_delta.push(delta);
            event_prefix_after.push(prefix);
        }
        let (next_strict_below_close, virtual_next_strict_below_close) =
            Self::build_next_strict_below_close(&event_delta, &event_prefix_after);

        Self {
            input_len,
            close_len: close.len(),
            event_pos,
            #[cfg(test)]
            event_prefix_after,
            next_strict_below_close,
            virtual_next_strict_below_close,
            close_pos,
            self_close_pos,
            last_after_tag: 0,
            event_cursor: 0,
            close_cursor: 0,
            self_close_cursor: 0,
        }
    }

    fn build_next_strict_below_close(
        event_delta: &[isize],
        event_prefix_after: &[isize],
    ) -> (Vec<usize>, usize) {
        let _ = event_delta;
        let mut next = vec![RAW_HTML_NO_NSE; event_prefix_after.len()];
        let mut stack: Vec<usize> = Vec::new();
        for i in (0..event_prefix_after.len()).rev() {
            crate::metrics::scan_work(1);
            while let Some(&candidate) = stack.last() {
                crate::metrics::scan_work(1);
                if event_prefix_after[candidate] < event_prefix_after[i] {
                    break;
                }
                stack.pop();
            }
            next[i] = stack.last().copied().unwrap_or(RAW_HTML_NO_NSE);
            debug_assert!(
                next[i] == RAW_HTML_NO_NSE || event_delta[next[i]] < 0,
                "first strict-below event must be a close"
            );
            stack.push(i);
        }
        while let Some(&candidate) = stack.last() {
            crate::metrics::scan_work(1);
            if event_prefix_after[candidate] < 0 {
                break;
            }
            stack.pop();
        }
        let virtual_next = stack.last().copied().unwrap_or(RAW_HTML_NO_NSE);
        debug_assert!(
            virtual_next == RAW_HTML_NO_NSE || event_delta[virtual_next] < 0,
            "first strict-below virtual event must be a close"
        );
        (next, virtual_next)
    }

    fn rank_from_start_charged(slice: &[usize], target: usize) -> usize {
        let mut idx = 0usize;
        while idx < slice.len() {
            crate::metrics::scan_work(1);
            if slice[idx] >= target {
                break;
            }
            idx += 1;
        }
        idx
    }

    fn matching_close_event(&self, event_rank: usize) -> usize {
        if event_rank == 0 {
            self.virtual_next_strict_below_close
        } else {
            self.next_strict_below_close[event_rank - 1]
        }
    }

    fn close_fits(&self, close_pos: usize, body_end: usize) -> bool {
        close_pos + self.close_len <= body_end
    }

    fn self_close_fits(self_close_pos: usize, body_end: usize) -> bool {
        self_close_pos + 2 <= body_end
    }

    fn query_ranks(&mut self, after_tag: usize) -> RawHtmlQueryRanks {
        if after_tag < self.last_after_tag {
            let event_rank = Self::rank_from_start_charged(&self.event_pos, after_tag);
            return RawHtmlQueryRanks {
                event_rank,
                close_rank: Self::rank_from_start_charged(&self.close_pos, after_tag),
                self_rank: Self::rank_from_start_charged(&self.self_close_pos, after_tag),
            };
        }

        while self.event_cursor < self.event_pos.len()
            && self.event_pos[self.event_cursor] < after_tag
        {
            crate::metrics::scan_work(1);
            self.event_cursor += 1;
        }
        while self.close_cursor < self.close_pos.len()
            && self.close_pos[self.close_cursor] < after_tag
        {
            crate::metrics::scan_work(1);
            self.close_cursor += 1;
        }
        while self.self_close_cursor < self.self_close_pos.len()
            && self.self_close_pos[self.self_close_cursor] < after_tag
        {
            crate::metrics::scan_work(1);
            self.self_close_cursor += 1;
        }
        self.last_after_tag = after_tag;
        RawHtmlQueryRanks {
            event_rank: self.event_cursor,
            close_rank: self.close_cursor,
            self_rank: self.self_close_cursor,
        }
    }

    fn match_from(
        &mut self,
        opener: usize,
        after_tag: usize,
        body_end: usize,
        tag_index: usize,
    ) -> RawHtmlAttempt {
        if body_end > self.input_len {
            debug_assert!(false, "raw HTML body_end exceeds indexed input length");
            return RawHtmlAttempt::Miss(RawHtmlMiss::NoGrammar);
        }
        let query = self.query_ranks(after_tag);
        let close_event = self.matching_close_event(query.event_rank);
        if close_event != RAW_HTML_NO_NSE {
            let close_pos = self.event_pos[close_event];
            if self.close_fits(close_pos, body_end) {
                return RawHtmlAttempt::Match(RawHtmlExtent {
                    start: opener,
                    end: close_pos + self.close_len,
                });
            }
        }
        if query.self_rank < self.self_close_pos.len() {
            let self_close = self.self_close_pos[query.self_rank];
            if Self::self_close_fits(self_close, body_end) {
                return RawHtmlAttempt::Match(RawHtmlExtent {
                    start: opener,
                    end: self_close + 2,
                });
            }
        }
        if query.close_rank < self.close_pos.len()
            && self.close_fits(self.close_pos[query.close_rank], body_end)
        {
            RawHtmlAttempt::Miss(RawHtmlMiss::UnbalancedTag)
        } else {
            RawHtmlAttempt::Miss(RawHtmlMiss::MissingTagCloser { index: tag_index })
        }
    }
}

#[cfg(test)]
mod raw_html_index_tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct LegacyEvents {
        event_pos: Vec<usize>,
        event_prefix_after: Vec<isize>,
        close_pos: Vec<usize>,
        self_close_pos: Vec<usize>,
    }

    fn legacy_events(bytes: &[u8], tag: &str, body_end: usize) -> LegacyEvents {
        let close_tag = format!("</{}>", tag);
        let open_tag = format!("<{}>", tag);
        let open_attr = format!("<{} ", tag);
        let close = close_tag.as_bytes();
        let open_plain = open_tag.as_bytes();
        let open_with_attrs = open_attr.as_bytes();

        let mut events: Vec<(usize, isize)> = Vec::new();
        let mut q = 0usize;
        while q < body_end {
            if starts_with_at(bytes, q, open_plain, body_end) {
                events.push((q, 1));
                q += open_plain.len();
            } else if starts_with_at(bytes, q, open_with_attrs, body_end) {
                events.push((q, 1));
                q += open_with_attrs.len();
            } else {
                q += 1;
            }
        }

        let mut close_pos = Vec::new();
        let mut p = 0usize;
        while p + close.len() <= body_end {
            if starts_with_ci_at(bytes, p, close, body_end) {
                close_pos.push(p);
                events.push((p, -1));
                p += close.len();
            } else {
                p += 1;
            }
        }

        let mut self_close_pos = Vec::new();
        let mut r = 0usize;
        while r + 1 < body_end {
            if starts_with_at(bytes, r, b"/>", body_end) {
                self_close_pos.push(r);
                r += 2;
            } else {
                r += 1;
            }
        }

        events.sort_unstable_by_key(|&(pos, _)| pos);
        let mut event_pos = Vec::with_capacity(events.len());
        let mut event_prefix_after = Vec::with_capacity(events.len());
        let mut prefix = 0isize;
        for (pos, delta) in events {
            prefix += delta;
            event_pos.push(pos);
            event_prefix_after.push(prefix);
        }

        LegacyEvents {
            event_pos,
            event_prefix_after,
            close_pos,
            self_close_pos,
        }
    }

    fn legacy_attempt(input: &str, opener: usize, body_end: usize) -> RawHtmlAttempt {
        parse_raw_html_impl(input, opener, body_end, None, true)
    }

    fn assert_cached_matches_legacy(input: &str, openers: &[usize], body_ends: &[usize]) {
        let mut state = RawHtmlScan::new();
        for &opener in openers {
            for &body_end in body_ends {
                if opener >= body_end || body_end > input.len() {
                    continue;
                }
                let cached = parse_raw_html_impl(input, opener, body_end, Some(&mut state), true);
                let legacy = legacy_attempt(input, opener, body_end);
                assert_eq!(
                    cached, legacy,
                    "input={input:?} opener={opener} body_end={body_end}"
                );
            }
        }
    }

    #[test]
    fn raw_html_combined_build_matches_legacy_event_sets() {
        let cases = [
            ("<br /><br></BR><BR x/>", "br"),
            ("<Div><div></DIV></div><Div /></DIV>", "Div"),
            ("x</div><div data=\"/>\"></div><div />", "div"),
            ("<span></SPAN><span class=x></span></span>", "span"),
        ];
        for (input, tag) in cases {
            let idx = RawHtmlTagIndex::build(input.as_bytes(), tag);
            let legacy = legacy_events(input.as_bytes(), tag, input.len());
            assert_eq!(idx.event_pos, legacy.event_pos, "{input:?} {tag}");
            assert_eq!(
                idx.event_prefix_after, legacy.event_prefix_after,
                "{input:?} {tag}"
            );
            assert_eq!(idx.close_pos, legacy.close_pos, "{input:?} {tag}");
            assert_eq!(idx.self_close_pos, legacy.self_close_pos, "{input:?} {tag}");
        }
    }

    #[test]
    fn raw_html_nse_is_strict_below_and_includes_adjacent_close() {
        let equal_prefix = "<div><div></div><div></div>";
        let mut idx = RawHtmlTagIndex::build(equal_prefix.as_bytes(), "div");
        let outer = idx.query_ranks(4);
        assert_eq!(idx.matching_close_event(outer.event_rank), RAW_HTML_NO_NSE);
        let mut state = RawHtmlScan::new();
        assert_eq!(
            parse_raw_html_impl(equal_prefix, 0, equal_prefix.len(), Some(&mut state), true),
            RawHtmlAttempt::Miss(RawHtmlMiss::UnbalancedTag)
        );

        let adjacent = "<div><div></div></div>";
        let mut idx = RawHtmlTagIndex::build(adjacent.as_bytes(), "div");
        let inner_opener = adjacent[1..].find("<div").unwrap() + 1;
        let inner = idx.query_ranks(inner_opener + 4);
        let close_event = idx.matching_close_event(inner.event_rank);
        assert_ne!(close_event, RAW_HTML_NO_NSE);
        assert_eq!(idx.event_pos[close_event], inner_opener + "<div>".len());
    }

    #[test]
    fn raw_html_global_index_matches_legacy_window_queries() {
        let input = "<div><div>x</div>\n<div />\n<div><span></span></div>\n</div>tail</div>";
        let openers: Vec<usize> = input.match_indices("<div").map(|(pos, _)| pos).collect();
        let body_ends = [18usize, 27, 52, input.len() - 3, input.len()];
        assert_cached_matches_legacy(input, &openers, &body_ends);

        let mut reverse_openers = openers.clone();
        reverse_openers.reverse();
        assert_cached_matches_legacy(input, &reverse_openers, &[input.len()]);

        let boundary_cases = [
            "<div>abc</DIV>tail",
            "<div>abc<div>tail</div>",
            "<div>abc<div tail</DIV>",
            "<div>abc/>tail</div>",
            "<div>abc<div /></DIV>",
        ];
        for input in boundary_cases {
            let openers: Vec<usize> = input.match_indices("<div").map(|(pos, _)| pos).collect();
            let body_ends: Vec<usize> = (1..=input.len()).collect();
            assert_cached_matches_legacy(input, &openers, &body_ends);
        }

        let blockquote = "<blockquote>x</BLOCKQUOTE>tail";
        let openers = [0usize];
        let body_ends: Vec<usize> = (1..=blockquote.len()).collect();
        assert_cached_matches_legacy(blockquote, &openers, &body_ends);
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
    Tag {
        tag: &'a str,
        index: usize,
    },
    Special {
        opener: &'static str,
        closer: &'static str,
        miss: usize,
    },
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

fn raw_html_head_at(
    input: &str,
    at: usize,
    limit: usize,
    require_peek: bool,
) -> Option<RawHtmlHead<'_>> {
    if at >= limit || limit > input.len() || input.as_bytes().get(at) != Some(&b'<') {
        return None;
    }
    debug_assert!(crate::inline::HICCUP_TAGS
        .iter()
        .all(|t| t.len() <= MAX_HTML_TAG_LEN));
    // mldoc Raw_html.parse begins with `peek_string 10`; Angstrom fails before dispatch when
    // fewer than ten bytes remain. This is the source of `<b>ab</b>` (9 bytes) being plain.
    if require_peek && limit.saturating_sub(at) < 10 {
        return None;
    }
    if starts_with_at(input.as_bytes(), at, b"<?", limit) {
        return Some(RawHtmlHead::Special {
            opener: "<?",
            closer: "?>",
            miss: 0,
        });
    }
    if starts_with_at(input.as_bytes(), at, b"<!--", limit) {
        return Some(RawHtmlHead::Special {
            opener: "<!--",
            closer: "-->",
            miss: 1,
        });
    }
    if starts_with_at(input.as_bytes(), at, b"<![CDATA[", limit) {
        // Source exact: mldoc 1.5.7 uses "]]" as the strict wrapper closer here.
        return Some(RawHtmlHead::Special {
            opener: "<![CDATA[",
            closer: "]]",
            miss: 2,
        });
    }
    if starts_with_at(input.as_bytes(), at, b"<!", limit) {
        return Some(RawHtmlHead::Special {
            opener: "<!",
            closer: ">",
            miss: 3,
        });
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
    let off = mldoc_spaces_len(s);
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

fn find_exact_bounded(
    bytes: &[u8],
    from: usize,
    end: usize,
    needle: &[u8],
) -> (Option<usize>, usize) {
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

fn find_end_string_bounded(
    bytes: &[u8],
    from: usize,
    end: usize,
    closer: &[u8],
) -> (Option<usize>, usize) {
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

fn parse_raw_html_impl(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
    require_peek: bool,
) -> RawHtmlAttempt {
    let Some(head) = raw_html_head_at(input, opener, body_end, require_peek) else {
        return RawHtmlAttempt::Miss(RawHtmlMiss::NoGrammar);
    };
    match head {
        RawHtmlHead::Special {
            opener: open,
            closer,
            miss,
        } => {
            let from = opener + open.len();
            let (found, scanned) =
                find_end_string_bounded(input.as_bytes(), from, body_end, closer.as_bytes());
            crate::metrics::scan_work(scanned);
            match found {
                Some(pos) => RawHtmlAttempt::Match(RawHtmlExtent {
                    start: opener,
                    end: pos + closer.len(),
                }),
                None => RawHtmlAttempt::Miss(RawHtmlMiss::MissingSpecialCloser { miss }),
            }
        }
        RawHtmlHead::Tag { tag, index } => {
            let after_tag = opener + 1 + tag.len();
            if let Some(state) = state {
                return state
                    .tag_index(input, tag)
                    .match_from(opener, after_tag, body_end, index);
            }
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
                    let (opens, chunk_scanned) = count_tag_opens_in_chunk(
                        bytes,
                        chunk_start,
                        p,
                        open_plain,
                        open_with_attrs,
                    );
                    scanned += chunk_scanned;
                    saw_close = true;
                    level += opens as isize;
                    level -= 1;
                    p += close.len();
                    chunk_start = p;
                    if level <= 0 {
                        crate::metrics::scan_work(scanned);
                        return RawHtmlAttempt::Match(RawHtmlExtent {
                            start: opener,
                            end: p,
                        });
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

fn parse_raw_html_at_cached_with_gate(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
    require_peek: bool,
) -> Option<RawHtmlExtent> {
    if opener > body_end || body_end > input.len() {
        return None;
    }
    let mut state = state;
    let head = raw_html_head_at(input, opener, body_end, require_peek)?;
    if let Some(s) = state.as_deref() {
        match head {
            RawHtmlHead::Tag { index, .. } if s.no_tag_end_until[index] >= body_end => return None,
            RawHtmlHead::Special { miss, .. } if s.no_special_until[miss] >= body_end => {
                return None
            }
            _ => {}
        }
    }
    match parse_raw_html_impl(input, opener, body_end, state.as_deref_mut(), require_peek) {
        RawHtmlAttempt::Match(extent) => Some(extent),
        RawHtmlAttempt::Miss(RawHtmlMiss::MissingTagCloser { index }) => {
            if let Some(s) = state.as_deref_mut() {
                s.no_tag_end_until[index] = body_end;
            }
            None
        }
        RawHtmlAttempt::Miss(RawHtmlMiss::MissingSpecialCloser { miss }) => {
            if let Some(s) = state.as_deref_mut() {
                s.no_special_until[miss] = body_end;
            }
            None
        }
        RawHtmlAttempt::Miss(_) => None,
    }
}

pub(crate) fn parse_raw_html_at_cached(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
) -> Option<RawHtmlExtent> {
    parse_raw_html_at_cached_with_gate(input, opener, body_end, state, true)
}

fn parse_raw_html_at_cached_after_view_peek(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
) -> Option<RawHtmlExtent> {
    parse_raw_html_at_cached_with_gate(input, opener, body_end, state, false)
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

fn copy_capture_text(s: &str) -> String {
    crate::metrics::scan_work(s.len());
    s.to_string()
}

// scan-owner: (a) consumed-on-match accepted copy — raw-HTML canonical close construction
fn raw_html_canonical_close(
    input: &str,
    opener: usize,
    close_end: usize,
) -> Option<(usize, String)> {
    if opener >= close_end || close_end > input.len() {
        return None;
    }
    let RawHtmlHead::Tag { tag, .. } = raw_html_head_at(input, opener, close_end, false)? else {
        return None;
    };
    let close_tag = format!("</{}>", tag);
    crate::metrics::scan_work(close_tag.len());
    let close = close_tag.as_bytes();
    if close_end < close.len()
        || !starts_with_ci_at(input.as_bytes(), close_end - close.len(), close, close_end)
    {
        return None;
    }
    Some((1 + tag.len(), close_tag))
}

fn rebuild_capture_from_mismatch(s: &str, first_mismatch: usize, close_tag: &str) -> String {
    crate::metrics::scan_work(s.len());
    let close = close_tag.as_bytes();
    let bytes = s.as_bytes();
    let end = s.len();
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..first_mismatch]);
    out.push_str(close_tag);

    let mut p = first_mismatch + close.len();
    let mut chunk_start = p;
    while p + close.len() <= end {
        if starts_with_ci_at(bytes, p, close, end) {
            out.push_str(&s[chunk_start..p]);
            out.push_str(close_tag);
            p += close.len();
            chunk_start = p;
        } else {
            p += 1;
        }
    }
    out.push_str(&s[chunk_start..]);
    out
}

fn normalize_ci_matched_closes(s: &str, scan_start: usize, close_tag: &str) -> Option<String> {
    // mldoc's `end_string_2 close_tag ~ci:true` consumes exactly the sequential leftmost
    // non-overlapping case-insensitive `</tag>` occurrences after the opener tag token. The
    // scanner already proved this extent ends on the same stream, so this single pass rewrites
    // the same consumed closes without changing spans or extents.
    crate::metrics::scan_work(s.len());
    let close = close_tag.as_bytes();
    let bytes = s.as_bytes();
    let end = s.len();
    let mut p = scan_start.min(end);
    while p + close.len() <= end {
        if starts_with_ci_at(bytes, p, close, end) {
            if &bytes[p..p + close.len()] != close {
                return Some(rebuild_capture_from_mismatch(s, p, close_tag));
            }
            p += close.len();
        } else {
            p += 1;
        }
    }
    None
}

pub(crate) fn raw_html_capture_text(input: &str, opener: usize, close_end: usize) -> String {
    let s = &input[opener..close_end];
    let Some((scan_start, close_tag)) = raw_html_canonical_close(input, opener, close_end) else {
        return copy_capture_text(s);
    };
    normalize_ci_matched_closes(s, scan_start, &close_tag).unwrap_or_else(|| copy_capture_text(s))
}

fn normalize_raw_html_view_text(
    text: String,
    input: &str,
    opener: usize,
    close_end: usize,
) -> String {
    let Some((scan_start, close_tag)) = raw_html_canonical_close(input, opener, close_end) else {
        return text;
    };
    normalize_ci_matched_closes(&text, scan_start, &close_tag).unwrap_or(text)
}

fn push_capture_str(out: &mut String, s: &str) {
    crate::metrics::scan_work(s.len());
    out.push_str(s);
}

fn push_capture_joiner(out: &mut String) {
    crate::metrics::scan_work(1);
    out.push('\n');
}

// scan-owner: (a) consumed-on-match accepted copy — raw-HTML view peek over accepted capture window
fn view_tail_has_peek(
    lines: &[Line<'_>],
    cur: usize,
    hi: usize,
    ctx: StripCtx<'_>,
    first_view: &str,
    opener_off: usize,
) -> bool {
    let mut seen = first_view.len().saturating_sub(opener_off).min(10);
    crate::metrics::scan_work(seen);
    if seen < 10 && lines[cur].has_eol() {
        seen += 1;
        crate::metrics::scan_work(1);
    }
    let mut k = cur + 1;
    while seen < 10 && k < hi {
        let before = seen;
        seen = (seen + lines[k].viewed_text(ctx).len()).min(10);
        crate::metrics::scan_work(seen - before);
        if seen < 10 && lines[k].has_eol() {
            seen += 1;
            crate::metrics::scan_work(1);
        }
        k += 1;
    }
    seen >= 10
}

// scan-owner: (a) consumed-on-match accepted copy — raw-HTML accepted raw capture mapping/copy
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
        crate::metrics::scan_work(1);
        close_line += 1;
    }
    if close_line >= hi {
        return None;
    }
    let content_end = lines[close_line].start + lines[close_line].text.len();
    let text = raw_html_capture_text(input, opener, close_end);
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
            crate::metrics::scan_work(1);
            span_end = lines[next].end;
            next += 1;
        }
        Some(RawHtmlCapture {
            text,
            span_start: opener,
            span_end,
            next,
            rewrite: None,
        })
    }
}

// scan-owner: (a) consumed-on-match accepted copy — raw-HTML accepted view capture mapping/copy
pub(crate) fn raw_html_view_capture<'a>(
    lines: &[Line<'a>],
    cur: usize,
    hi: usize,
    ctx: StripCtx<'_>,
    first_view: &str,
    body_end: usize,
    input: &'a str,
    state: &mut RawHtmlScan,
) -> Option<RawHtmlCapture> {
    if cur >= hi {
        return None;
    }
    let (opener_off, _) = raw_html_head_prefix(first_view)?;
    if !view_tail_has_peek(lines, cur, hi, ctx, first_view, opener_off) {
        return None;
    }
    let opener = line_view_abs_start(&lines[cur], first_view) + opener_off;
    let view_body_raw_end = lines[hi - 1].start + lines[hi - 1].text.len();
    debug_assert!(view_body_raw_end <= body_end);
    let close_end =
        parse_raw_html_at_cached_after_view_peek(input, opener, view_body_raw_end, Some(state))?
            .end;
    let mut close_line = cur;
    while close_line < hi && lines[close_line].start + lines[close_line].text.len() < close_end {
        crate::metrics::scan_work(1);
        close_line += 1;
    }
    if close_line >= hi {
        return None;
    }
    let close_view = if close_line == cur {
        first_view
    } else {
        lines[close_line].viewed_text(ctx)
    };
    let close_view_start = line_view_abs_start(&lines[close_line], close_view);
    if close_end < close_view_start || close_end > close_view_start + close_view.len() {
        return None;
    }
    let close_end_in_line = close_end - close_view_start;
    let close_abs = close_end;
    let content_end = lines[close_line].start + lines[close_line].text.len();
    let span_start = line_view_abs_start(&lines[cur], first_view) + opener_off;
    let mut text = String::with_capacity(close_end.saturating_sub(opener));
    if close_line == cur {
        push_capture_str(&mut text, &first_view[opener_off..close_end_in_line]);
    } else {
        push_capture_str(&mut text, &first_view[opener_off..]);
        for line_idx in cur + 1..close_line {
            crate::metrics::scan_work(1);
            push_capture_joiner(&mut text);
            push_capture_str(&mut text, lines[line_idx].viewed_text(ctx));
        }
        push_capture_joiner(&mut text);
        push_capture_str(&mut text, &close_view[..close_end_in_line]);
    }
    let text = normalize_raw_html_view_text(text, input, opener, close_end);
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
            crate::metrics::scan_work(1);
            span_end = lines[next].end;
            next += 1;
        }
        Some(RawHtmlCapture {
            text,
            span_start,
            span_end,
            next,
            rewrite: None,
        })
    }
}

#[cfg(test)]
mod raw_html_capture_tests {
    use super::*;
    use crate::projection::Inline;

    fn raw_html_text(blocks: &[Block], idx: usize) -> &str {
        match &blocks[idx] {
            Block::RawHtml { text, .. } => text,
            other => panic!("expected raw_html at {idx}, got {other:?}"),
        }
    }

    fn inline_html_text(inlines: &[Inline]) -> &str {
        inlines
            .iter()
            .find_map(|node| match node {
                Inline::InlineHtml { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .expect("expected inline_html")
    }

    #[test]
    fn raw_html_raw_capture_normalizes_final_close() {
        let blocks = crate::parse::parse("<div>x</DIV>");
        assert_eq!(raw_html_text(&blocks, 0), "<div>x</div>");
    }

    #[test]
    fn raw_html_raw_capture_normalizes_intermediate_closes() {
        let blocks = crate::parse::parse("<b>a<b>c</B>d</B> tail here");
        assert_eq!(raw_html_text(&blocks, 0), "<b>a<b>c</b>d</b>");
    }

    #[test]
    fn raw_html_raw_capture_preserves_opener_case() {
        let blocks = crate::parse::parse("<DIV>a</div>");
        assert_eq!(raw_html_text(&blocks, 0), "<DIV>a</DIV>");
    }

    #[test]
    fn raw_html_view_capture_normalizes_close_in_stripped_view() {
        let blocks = crate::org::parse("#+BEGIN_QUOTE\n  <DIV>x\n  </div>\n#+END_QUOTE");
        match &blocks[..] {
            [Block::Quote { children, .. }] => {
                assert_eq!(raw_html_text(children, 0), "<DIV>x\n</DIV>");
            }
            other => panic!("expected quote with raw_html child, got {other:?}"),
        }
    }

    #[test]
    fn raw_html_inline_capture_normalizes_close() {
        let md = crate::resolver::parse_inline("a <b>x</B> y", 0);
        assert_eq!(inline_html_text(&md), "<b>x</b>");

        let org = crate::org_resolver::parse_inline_org("a <b>x</B> y", 0);
        assert_eq!(inline_html_text(&org), "<b>x</b>");
    }
}

/// Next fence-marker line at/after `from`, advancing the monotone `cursor` (the drivers reach
/// fence openers in increasing `from`, so the cursor only advances — O(1) amortized).
// scan-owner: (b) monotone cursor — fence closer index cursor
pub(crate) fn find_matching_fence(
    fence_lines: &[usize],
    cursor: &mut usize,
    from: usize,
) -> Option<usize> {
    // the main loop reaches fence openers in increasing `from`, so the cursor only advances.
    while *cursor < fence_lines.len() && fence_lines[*cursor] <= from {
        crate::metrics::scan_work(1);
        *cursor += 1;
    }
    fence_lines.get(*cursor).copied()
}

/// First `:END:` drawer-closer line strictly after `from`, via a monotone `cursor` (advance-only,
/// like [`find_matching_fence`]): the drivers reach drawer openers in increasing `from`, so the
/// cursor only advances ⇒ O(1) amortized, not the O(log n) of a per-opener binary search. The
/// cursor stops AT (does not consume) the first closer `> from`, so a repeated/equal `from` is
/// idempotent.
// scan-owner: (b) monotone cursor — drawer closer index cursor
pub(crate) fn find_drawer_end(
    drawer_end_idxs: &[usize],
    cursor: &mut usize,
    from: usize,
) -> Option<usize> {
    while *cursor < drawer_end_idxs.len() && drawer_end_idxs[*cursor] <= from {
        crate::metrics::scan_work(1);
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
        EndTrie {
            kids: vec![Vec::new()],
            ends: vec![Vec::new()],
            cursor: vec![Cell::new(0)],
        }
    }
    /// Index `#+END_` line `idx` under the leading non-ws run of `suffix` (the text after
    /// `#+END_`), lowercased. The empty prefix (root) matches any opener name (incl. `""`).
    pub(crate) fn insert(&mut self, suffix: &str, idx: usize) {
        let mut node = 0usize;
        self.ends[node].push(idx);
        for &b in suffix.as_bytes() {
            crate::metrics::scan_work(1);
            if b == b' ' || b == b'\t' {
                break;
            }
            let lb = b.to_ascii_lowercase();
            let mut found = None;
            for &(k, c) in &self.kids[node] {
                crate::metrics::scan_work(1);
                if k == lb {
                    found = Some(c as usize);
                    break;
                }
            }
            node = match found {
                Some(c) => c,
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
            crate::metrics::scan_work(1);
            let lb = b.to_ascii_lowercase();
            let mut found = None;
            for &(k, c) in &self.kids[node] {
                crate::metrics::scan_work(1);
                if k == lb {
                    found = Some(c as usize);
                    break;
                }
            }
            node = found?;
        }
        let v = &self.ends[node];
        let cur = &self.cursor[node];
        let mut c = cur.get();
        while c < v.len() && v[c] <= from {
            crate::metrics::scan_work(1);
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
            Builder::Custom(name) => Block::Custom {
                name,
                children,
                span,
            },
        }
    }
}

/// `#+BEGIN_EXPORT <name> <options...>` fields from mldoc's
/// `block_name_options_parser` + `separate_name_options`.
// scan-owner: (a2) caller-owned line helper — export opener option split scans only current line tail
pub(crate) fn begin_export_fields(s: &str) -> (String, Option<Vec<String>>) {
    let Some(t) = mldoc_trim_spaces_start(s).get(8..) else {
        return (String::new(), None);
    };
    let name_len = t.bytes().take_while(|&b| !mldoc_is_space(b)).count();
    crate::metrics::scan_work(name_len + usize::from(name_len < t.len()));
    let rest = &t[name_len..];
    let ws = mldoc_spaces_len(rest);
    let options = &rest[ws..];
    if options.is_empty() {
        return (String::new(), None);
    }
    crate::metrics::scan_work(options.len());
    // scan-owner: (a2) caller-owned line helper — split the already charged export option tail
    let mut parts = options.split(' ');
    let name = parts.next().unwrap_or("").to_string();
    let options: Vec<String> = parts.map(str::to_string).collect();
    let options = if options.is_empty() {
        None
    } else {
        Some(options)
    };
    (name, options)
}

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
                Some(prev) => debug_assert_eq!(
                    prev, id,
                    "RawHtmlScan reused with a different input string"
                ),
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
    event_prefix: isize,
    close_rank: usize,
    self_rank: usize,
}

#[derive(Clone, Copy)]
struct RawHtmlWindowRanks {
    tail_start: usize,
    event_tail_start: usize,
    close_tail_start: usize,
    close_fit_end: usize,
    self_fit_end: usize,
}

// Per-tag body windows are stack-like plus the full-input resolver window. Keep only the most
// recent windows so lookup work is deterministic; eviction only recomputes pure rank metadata.
const RAW_HTML_BODY_RANK_CACHE_CAP: usize = 16;

struct RawHtmlTagIndex {
    input_len: usize,
    close_len: usize,
    max_event_len: usize,
    event_pos: Vec<usize>,
    event_end: Vec<usize>,
    event_delta: Vec<isize>,
    event_prefix_after: Vec<isize>,
    close_pos: Vec<usize>,
    close_min_tree: Vec<isize>,
    close_tree_base: usize,
    self_close_pos: Vec<usize>,
    body_ranks: Vec<(usize, RawHtmlWindowRanks)>,
    last_after_tag: usize,
    event_cursor: usize,
    event_cursor_prefix: isize,
    close_cursor: usize,
    self_close_cursor: usize,
}

impl RawHtmlTagIndex {
    fn build(bytes: &[u8], tag: &str) -> Self {
        let close_tag = format!("</{}>", tag);
        let open_tag = format!("<{}>", tag);
        let open_attr = format!("<{} ", tag);
        let close = close_tag.as_bytes();
        let open_plain = open_tag.as_bytes();
        let open_with_attrs = open_attr.as_bytes();
        let input_len = bytes.len();
        let max_event_len = close.len().max(open_plain.len()).max(open_with_attrs.len()).max(2);

        let mut events: Vec<(usize, usize, isize)> = Vec::new();
        let mut close_pos = Vec::new();
        let mut self_close_pos = Vec::new();
        let mut open_cursor = 0usize;
        let mut close_cursor = 0usize;
        let mut self_cursor = 0usize;
        let mut scanned = 0usize;

        for pos in 0..input_len {
            scanned += 1;
            if pos == open_cursor {
                if starts_with_at(bytes, pos, open_plain, input_len) {
                    events.push((pos, pos + open_plain.len(), 1));
                    open_cursor += open_plain.len();
                } else if starts_with_at(bytes, pos, open_with_attrs, input_len) {
                    events.push((pos, pos + open_with_attrs.len(), 1));
                    open_cursor += open_with_attrs.len();
                } else {
                    open_cursor += 1;
                }
            }
            if pos == close_cursor {
                if close_cursor + close.len() <= input_len {
                    if starts_with_ci_at(bytes, pos, close, input_len) {
                        close_pos.push(pos);
                        events.push((pos, pos + close.len(), -1));
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
        let mut event_end = Vec::with_capacity(events.len());
        let mut event_delta = Vec::with_capacity(events.len());
        let mut event_prefix_after = Vec::with_capacity(events.len());
        let mut close_prefix_after = Vec::with_capacity(close_pos.len());
        let mut prefix = 0isize;
        for (pos, end, delta) in events {
            prefix += delta;
            event_pos.push(pos);
            event_end.push(end);
            event_delta.push(delta);
            event_prefix_after.push(prefix);
            if delta < 0 {
                close_prefix_after.push(prefix);
            }
        }
        debug_assert_eq!(close_prefix_after.len(), close_pos.len());

        let mut close_tree_base = 1usize;
        while close_tree_base < close_prefix_after.len().max(1) {
            close_tree_base *= 2;
        }
        let mut close_min_tree = vec![isize::MAX; close_tree_base * 2];
        for (i, &v) in close_prefix_after.iter().enumerate() {
            close_min_tree[close_tree_base + i] = v;
        }
        for i in (1..close_tree_base).rev() {
            close_min_tree[i] = close_min_tree[i * 2].min(close_min_tree[i * 2 + 1]);
        }

        Self {
            input_len,
            close_len: close.len(),
            max_event_len,
            event_pos,
            event_end,
            event_delta,
            event_prefix_after,
            close_pos,
            close_min_tree,
            close_tree_base,
            self_close_pos,
            body_ranks: Vec::with_capacity(RAW_HTML_BODY_RANK_CACHE_CAP),
            last_after_tag: 0,
            event_cursor: 0,
            event_cursor_prefix: 0,
            close_cursor: 0,
            self_close_cursor: 0,
        }
    }

    fn lower_bound_charged(slice: &[usize], target: usize) -> usize {
        let mut lo = 0usize;
        let mut hi = slice.len();
        while lo < hi {
            crate::metrics::scan_work(1);
            let mid = lo + (hi - lo) / 2;
            if slice[mid] < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn query_ranks(&mut self, after_tag: usize) -> RawHtmlQueryRanks {
        let monotone = after_tag >= self.last_after_tag;
        debug_assert!(
            monotone,
            "RawHtmlTagIndex queried backwards: previous after_tag={}, current after_tag={}",
            self.last_after_tag,
            after_tag
        );
        if !monotone {
            let event_rank = Self::lower_bound_charged(&self.event_pos, after_tag);
            let event_prefix = if event_rank == 0 {
                0
            } else {
                self.event_prefix_after[event_rank - 1]
            };
            return RawHtmlQueryRanks {
                event_rank,
                event_prefix,
                close_rank: Self::lower_bound_charged(&self.close_pos, after_tag),
                self_rank: Self::lower_bound_charged(&self.self_close_pos, after_tag),
            };
        }

        while self.event_cursor < self.event_pos.len() && self.event_pos[self.event_cursor] < after_tag {
            crate::metrics::scan_work(1);
            self.event_cursor_prefix += self.event_delta[self.event_cursor];
            self.event_cursor += 1;
        }
        while self.close_cursor < self.close_pos.len() && self.close_pos[self.close_cursor] < after_tag {
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
            event_prefix: self.event_cursor_prefix,
            close_rank: self.close_cursor,
            self_rank: self.self_close_cursor,
        }
    }

    fn window_ranks(&mut self, body_end: usize) -> RawHtmlWindowRanks {
        debug_assert!(body_end <= self.input_len);
        let mut hit = None;
        for pos in 0..self.body_ranks.len() {
            crate::metrics::scan_work(1);
            if self.body_ranks[pos].0 == body_end {
                hit = Some(pos);
                break;
            }
        }
        if let Some(pos) = hit {
            let ranks = self.body_ranks[pos].1;
            if pos != 0 {
                crate::metrics::scan_work(pos);
                let entry = self.body_ranks.remove(pos);
                self.body_ranks.insert(0, entry);
            }
            return ranks;
        }
        let tail_start = body_end.saturating_sub(self.max_event_len);
        let close_fit_end = if body_end >= self.close_len {
            Self::lower_bound_charged(&self.close_pos, body_end - self.close_len + 1)
        } else {
            0
        };
        let self_fit_end = if body_end >= 2 {
            Self::lower_bound_charged(&self.self_close_pos, body_end - 1)
        } else {
            0
        };
        let ranks = RawHtmlWindowRanks {
            tail_start,
            event_tail_start: Self::lower_bound_charged(&self.event_pos, tail_start),
            close_tail_start: Self::lower_bound_charged(&self.close_pos, tail_start),
            close_fit_end,
            self_fit_end,
        };
        if self.body_ranks.len() == RAW_HTML_BODY_RANK_CACHE_CAP {
            self.body_ranks.pop();
        }
        crate::metrics::scan_work(self.body_ranks.len());
        self.body_ranks.insert(0, (body_end, ranks));
        ranks
    }

    fn prefix_before_window(
        &self,
        pos: usize,
        body_end: usize,
        query: RawHtmlQueryRanks,
        window: RawHtmlWindowRanks,
    ) -> isize {
        if pos <= window.tail_start {
            return query.event_prefix;
        }
        let mut prefix = query.event_prefix;
        let mut idx = window.event_tail_start;
        while idx < query.event_rank {
            crate::metrics::scan_work(1);
            if self.event_end[idx] > body_end {
                prefix -= self.event_delta[idx];
            }
            idx += 1;
        }
        prefix
    }

    fn first_tail_close_below(
        &self,
        body_end: usize,
        after_tag: usize,
        threshold: isize,
        window: RawHtmlWindowRanks,
    ) -> Option<usize> {
        let mut prefix = if window.event_tail_start == 0 {
            0
        } else {
            self.event_prefix_after[window.event_tail_start - 1]
        };
        let mut idx = window.event_tail_start;
        while idx < self.event_pos.len() {
            let pos = self.event_pos[idx];
            if pos >= body_end {
                break;
            }
            crate::metrics::scan_work(1);
            if self.event_end[idx] <= body_end {
                prefix += self.event_delta[idx];
                if self.event_delta[idx] < 0 && pos >= after_tag && prefix < threshold {
                    return Some(pos);
                }
            }
            idx += 1;
        }
        None
    }

    fn first_close_below_range(&self, start_idx: usize, end_idx: usize, threshold: isize) -> Option<usize> {
        if start_idx >= end_idx || self.close_pos.is_empty() {
            return None;
        }
        self.first_close_below_node(1, 0, self.close_tree_base, start_idx, end_idx, threshold)
    }

    fn first_close_below_node(
        &self,
        node: usize,
        lo: usize,
        hi: usize,
        start_idx: usize,
        end_idx: usize,
        threshold: isize,
    ) -> Option<usize> {
        crate::metrics::scan_work(1);
        if hi <= start_idx || lo >= end_idx || self.close_min_tree[node] >= threshold {
            return None;
        }
        if hi - lo == 1 {
            return (lo < self.close_pos.len()).then_some(lo);
        }
        let mid = (lo + hi) / 2;
        self.first_close_below_node(node * 2, lo, mid, start_idx, end_idx, threshold)
            .or_else(|| {
                self.first_close_below_node(node * 2 + 1, mid, hi, start_idx, end_idx, threshold)
            })
    }

    fn match_from(
        &mut self,
        opener: usize,
        after_tag: usize,
        body_end: usize,
        tag_index: usize,
    ) -> RawHtmlAttempt {
        let query = self.query_ranks(after_tag);
        let window = self.window_ranks(body_end);
        let threshold = self.prefix_before_window(after_tag, body_end, query, window);
        if after_tag < window.tail_start {
            let non_tail_end = window.close_tail_start.min(window.close_fit_end);
            if let Some(close_idx) = self.first_close_below_range(query.close_rank, non_tail_end, threshold) {
                return RawHtmlAttempt::Match(RawHtmlExtent {
                    start: opener,
                    end: self.close_pos[close_idx] + self.close_len,
                });
            }
        }
        if let Some(close_pos) = self.first_tail_close_below(body_end, after_tag, threshold, window) {
            return RawHtmlAttempt::Match(RawHtmlExtent {
                start: opener,
                end: close_pos + self.close_len,
            });
        }
        if query.self_rank < window.self_fit_end {
            let self_close = self.self_close_pos[query.self_rank];
            return RawHtmlAttempt::Match(RawHtmlExtent {
                start: opener,
                end: self_close + 2,
            });
        }
        if query.close_rank < window.close_fit_end {
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

        LegacyEvents { event_pos, event_prefix_after, close_pos, self_close_pos }
    }

    fn legacy_attempt(input: &str, opener: usize, body_end: usize) -> RawHtmlAttempt {
        parse_raw_html_impl(input, opener, body_end, None)
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
            assert_eq!(idx.event_prefix_after, legacy.event_prefix_after, "{input:?} {tag}");
            assert_eq!(idx.close_pos, legacy.close_pos, "{input:?} {tag}");
            assert_eq!(idx.self_close_pos, legacy.self_close_pos, "{input:?} {tag}");
        }
    }

    #[test]
    fn raw_html_global_index_matches_legacy_window_queries() {
        let input = "<div><div>x</div>\n<div />\n<div><span></span></div>\n</div>tail</div>";
        let mut state = RawHtmlScan::new();
        let openers: Vec<usize> = input.match_indices("<div").map(|(pos, _)| pos).collect();
        let body_ends = [18usize, 27, 52, input.len() - 3, input.len()];
        for opener in openers {
            for body_end in body_ends {
                if opener >= body_end || body_end > input.len() {
                    continue;
                }
                let cached = parse_raw_html_impl(input, opener, body_end, Some(&mut state));
                let legacy = legacy_attempt(input, opener, body_end);
                assert_eq!(cached, legacy, "opener={opener} body_end={body_end}");
            }
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

fn parse_raw_html_impl(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
) -> RawHtmlAttempt {
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

pub(crate) fn parse_raw_html_at_cached(
    input: &str,
    opener: usize,
    body_end: usize,
    state: Option<&mut RawHtmlScan>,
) -> Option<RawHtmlExtent> {
    if opener > body_end || body_end > input.len() {
        return None;
    }
    let mut state = state;
    let head = raw_html_head_at(input, opener, body_end, true)?;
    if let Some(s) = state.as_deref() {
        match head {
            RawHtmlHead::Tag { index, .. } if s.no_tag_end_until[index] >= body_end => return None,
            RawHtmlHead::Special { miss, .. } if s.no_special_until[miss] >= body_end => return None,
            _ => {}
        }
    }
    match parse_raw_html_impl(input, opener, body_end, state.as_deref_mut()) {
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

fn push_capture_str(out: &mut String, s: &str) {
    crate::metrics::scan_work(s.len());
    out.push_str(s);
}

fn push_capture_joiner(out: &mut String) {
    crate::metrics::scan_work(1);
    out.push('\n');
}

fn view_tail_has_peek(
    lines: &[Line<'_>],
    cur: usize,
    hi: usize,
    strip: usize,
    first_view: &str,
    opener_off: usize,
) -> bool {
    let mut seen = first_view.len().saturating_sub(opener_off).min(10);
    let mut k = cur + 1;
    while seen < 10 && k < hi {
        seen += 1; // view line join
        if seen >= 10 {
            break;
        }
        seen = (seen + crate::org::strip_view(lines[k].text, strip).len()).min(10);
        k += 1;
    }
    seen >= 10
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
    let text = copy_capture_text(&input[opener..close_end]);
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
    body_end: usize,
    input: &'a str,
    state: &mut RawHtmlScan,
) -> Option<RawHtmlCapture> {
    if cur >= hi {
        return None;
    }
    let (opener_off, _) = raw_html_head_prefix(first_view)?;
    if !view_tail_has_peek(lines, cur, hi, strip, first_view, opener_off) {
        return None;
    }
    let opener = line_view_abs_start(&lines[cur], first_view) + opener_off;
    let view_body_raw_end = lines[hi - 1].start + lines[hi - 1].text.len();
    debug_assert!(view_body_raw_end <= body_end);
    let close_end = parse_raw_html_at_cached(input, opener, view_body_raw_end, Some(state))?.end;
    let mut close_line = cur;
    while close_line < hi && lines[close_line].start + lines[close_line].text.len() < close_end {
        close_line += 1;
    }
    if close_line >= hi {
        return None;
    }
    let close_view = if close_line == cur {
        first_view
    } else {
        crate::org::strip_view(lines[close_line].text, strip)
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
            push_capture_joiner(&mut text);
            push_capture_str(&mut text, crate::org::strip_view(lines[line_idx].text, strip));
        }
        push_capture_joiner(&mut text);
        push_capture_str(&mut text, &close_view[..close_end_in_line]);
    }
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

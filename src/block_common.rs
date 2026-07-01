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

/// `$$…$$` displayed-math line → inner text, when the line is exactly a `$$`-delimited block.
pub(crate) fn displayed_math(s: &str) -> Option<String> {
    let t = s.trim();
    if t.len() >= 4 {
        t.strip_prefix("$$")?.strip_suffix("$$").map(str::to_string)
    } else {
        None
    }
}

/// Is `s` a raw-HTML block line — `<tag …>…</tag>`, a real HTML element, NOT an autolink
/// `<https://…>` and NOT an incomplete tag? mldoc is strict: a bare `<div>` or `<note this>`
/// is a paragraph; only a line with an opening tag AND a closing `</…>` is Raw_Html.
pub(crate) fn is_raw_html(s: &str) -> bool {
    let t = s.trim_start();
    let b = t.as_bytes();
    if b.len() < 2 || b[0] != b'<' {
        return false;
    }
    let mut k = 1;
    if b[k] == b'/' {
        k += 1;
    }
    let name_start = k;
    while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'-') {
        k += 1;
    }
    if k == name_start || !b[name_start].is_ascii_alphabetic() {
        return false;
    }
    if !matches!(b.get(k), Some(b'>' | b'/' | b' ' | b'\t')) {
        return false;
    }
    // require a closing tag on the line (approximates mldoc's complete-element rule).
    t.contains("</")
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

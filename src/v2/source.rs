//! v2 source pass: one byte walk to build line windows and deterministic block-event indexes.
//!
//! This is intentionally small and independent of the current block parser. Later v2 block
//! code should consume this pass instead of re-splitting lines or building ad hoc closer
//! tables.

#![allow(dead_code)]

use std::cell::Cell;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Eol {
    Lf,
    CrLf,
    Cr,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Line<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) text: &'a str,
    pub(crate) eol: Eol,
    pub(crate) mldoc_spaces: usize,
}

pub(crate) struct Source<'a> {
    #[allow(dead_code)]
    pub(crate) input: &'a str,
    pub(crate) lines: Vec<Line<'a>>,
    pub(crate) events: Events,
}

pub(crate) struct Events {
    pub(crate) fence_lines: Vec<usize>,
    pub(crate) drawer_end_lines: Vec<usize>,
    pub(crate) property_end_lines: Vec<usize>,
    pub(crate) callout_ends: EndTrie,
    pub(crate) hiccup_close: HiccupClosers,
}

impl<'a> Source<'a> {
    pub(crate) fn scan(input: &'a str) -> Source<'a> {
        let mut scanner = SourceScanner::new(input);
        scanner.scan();
        let SourceScanner { lines, events, .. } = scanner;
        Source {
            input,
            lines,
            events,
        }
    }
}

impl Events {
    fn new() -> Events {
        Events {
            fence_lines: Vec::new(),
            drawer_end_lines: Vec::new(),
            property_end_lines: Vec::new(),
            callout_ends: EndTrie::new(),
            hiccup_close: HiccupClosers::new(),
        }
    }

    fn observe_line(&mut self, idx: usize, text: &str, ocaml_start: usize, mldoc_start: usize) {
        let bytes = text.as_bytes();
        let Some(first) = bytes.get(ocaml_start).copied() else {
            return;
        };
        let trimmed = &text[ocaml_start..];
        match first {
            b'`' | b'~' => {
                if is_fence_marker_line(trimmed) {
                    self.fence_lines.push(idx);
                }
            }
            b':' => {
                if is_property_end_line_at(text, mldoc_start) {
                    self.property_end_lines.push(idx);
                }
                if is_drawer_end_line_after_trim_start(trimmed) {
                    self.drawer_end_lines.push(idx);
                }
            }
            b'#' => {
                if let Some(suffix) = ci_strip_prefix(trimmed, "#+END_") {
                    self.callout_ends.insert(suffix, idx);
                }
            }
            0x1a => {
                if is_property_end_line_at(text, mldoc_start) {
                    self.property_end_lines.push(idx);
                }
            }
            _ => {}
        }
    }
}

struct SourceScanner<'a> {
    input: &'a str,
    lines: Vec<Line<'a>>,
    events: Events,
    start: usize,
    i: usize,
    hiccup_stack: Vec<usize>,
    hiccup_in_string: bool,
}

impl<'a> SourceScanner<'a> {
    fn new(input: &'a str) -> SourceScanner<'a> {
        SourceScanner {
            input,
            lines: Vec::with_capacity(initial_line_capacity(input.len())),
            events: Events::new(),
            start: 0,
            i: 0,
            hiccup_stack: Vec::new(),
            hiccup_in_string: false,
        }
    }

    // scan-owner: (a) consumed source pass — `i` advances monotonically over the input,
    // the hiccup close table is filled during this same cursor walk, and line-event
    // helpers own only the just-completed current line.
    fn scan(&mut self) {
        let bytes = self.input.as_bytes();
        while self.i < bytes.len() {
            let next = next_source_special(bytes, self.i, !self.hiccup_stack.is_empty());
            if next > self.i {
                crate::metrics::scan_work(next - self.i);
                self.i = next;
                continue;
            }
            match bytes[self.i] {
                b'\n' => {
                    crate::metrics::scan_work(1);
                    self.push_line(self.i + 1, self.i, Eol::Lf);
                    self.i += 1;
                    self.start = self.i;
                }
                b'\r' if bytes.get(self.i + 1) == Some(&b'\n') => {
                    crate::metrics::scan_work(2);
                    self.push_line(self.i + 2, self.i, Eol::CrLf);
                    self.i += 2;
                    self.start = self.i;
                }
                b'\r' => {
                    crate::metrics::scan_work(1);
                    self.push_line(self.i + 1, self.i, Eol::Cr);
                    self.i += 1;
                    self.start = self.i;
                }
                b'[' if bytes.get(self.i + 1) == Some(&b':') => {
                    crate::metrics::scan_work(2);
                    if self.hiccup_stack.is_empty() {
                        self.hiccup_in_string = false;
                    }
                    // Reserve the opener slot now (open order = increasing position,
                    // so the index is opener-sorted); the stack holds its pair index.
                    let idx = self.events.hiccup_close.open(self.i);
                    self.hiccup_stack.push(idx);
                    self.i += 2;
                }
                b'[' => {
                    crate::metrics::scan_work(1);
                    self.i += 1;
                }
                b']' => {
                    crate::metrics::scan_work(1);
                    if !self.hiccup_in_string {
                        if let Some(idx) = self.hiccup_stack.pop() {
                            self.events.hiccup_close.close(idx, self.i + 1);
                            if self.hiccup_stack.is_empty() {
                                self.hiccup_in_string = false;
                            }
                        }
                    }
                    self.i += 1;
                }
                b'"' => {
                    crate::metrics::scan_work(1);
                    if !self.hiccup_stack.is_empty() && (self.i == 0 || bytes[self.i - 1] != b'\\')
                    {
                        self.hiccup_in_string = !self.hiccup_in_string;
                    }
                    self.i += 1;
                }
                _ => {
                    crate::metrics::scan_work(1);
                    self.i += 1;
                }
            }
        }
        if self.start < self.input.len() {
            self.push_line(self.input.len(), self.input.len(), Eol::Eof);
        }
    }

    fn push_line(&mut self, end: usize, text_end: usize, eol: Eol) {
        let text = &self.input[self.start..text_end];
        let (mldoc_spaces, ocaml_start) = line_prefixes(text);
        let idx = self.lines.len();
        self.events
            .observe_line(idx, text, ocaml_start, mldoc_spaces);
        self.lines.push(Line {
            start: self.start,
            end,
            text,
            eol,
            mldoc_spaces,
        });
    }
}

fn initial_line_capacity(input_len: usize) -> usize {
    (input_len / 64 + 1).clamp(1, 4096)
}

fn line_prefixes(text: &str) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut mldoc_spaces = 0usize;
    let mut ocaml_start = 0usize;
    let mut mldoc_open = true;
    let mut ocaml_open = true;
    while i < bytes.len() && (mldoc_open || ocaml_open) {
        let b = bytes[i];
        let is_mldoc = mldoc_space_byte(b);
        let is_ocaml = ocaml_trim_byte(b);
        crate::metrics::scan_work(1);
        if mldoc_open {
            if is_mldoc {
                mldoc_spaces += 1;
            } else {
                mldoc_open = false;
            }
        }
        if ocaml_open {
            if is_ocaml {
                ocaml_start += 1;
            } else {
                ocaml_open = false;
            }
        }
        if !is_mldoc && !is_ocaml {
            break;
        }
        i += 1;
    }
    (mldoc_spaces, ocaml_start)
}

fn next_source_special(bytes: &[u8], start: usize, in_hiccup: bool) -> usize {
    let hay = &bytes[start..];
    let first = memchr::memchr3(b'\n', b'\r', b'[', hay);
    let rel = if in_hiccup {
        let primary = first.unwrap_or(hay.len());
        // scan-owner: (a2) bounded secondary hiccup scan — `]`/`"` only need to
        // be searched before the next primary source event. Searching the whole
        // suffix from every `[:` opener makes `[:div ` x n + `]` quadratic.
        let close_or_quote = memchr::memchr2(b']', b'"', &hay[..primary]);
        crate::metrics::scan_work(primary);
        min_option(first, close_or_quote)
    } else {
        first
    };
    start + rel.unwrap_or(hay.len())
}

fn min_option(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn ocaml_trim_start(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() && ocaml_trim_byte(bytes[start]) {
        crate::metrics::scan_work(1);
        start += 1;
    }
    &s[start..]
}

#[inline]
fn ocaml_trim_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

#[inline]
fn mldoc_name_space_or_eol(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x1a)
}

#[inline]
fn mldoc_space_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | 0x0c | 0x1a)
}

fn is_fence_marker_line(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn is_drawer_end_line_after_trim_start(trimmed_start: &str) -> bool {
    let Some(rest) = trimmed_start.get(5..) else {
        return false;
    };
    if !trimmed_start[..5].eq_ignore_ascii_case(":END:") {
        return false;
    }
    // scan-owner: (a) drawer-end suffix trim — only exact `:END:` candidates scan
    // their trailing trim bytes; non-candidates stop after the constant prefix check.
    rest.as_bytes().iter().all(|&b| {
        crate::metrics::scan_work(1);
        ocaml_trim_byte(b)
    })
}

fn is_property_end_line_at(text: &str, start: usize) -> bool {
    text[start..]
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(":END:"))
}

fn ci_strip_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let p = prefix.as_bytes();
    let b = s.as_bytes();
    crate::metrics::scan_work(p.len().min(b.len()));
    if b.len() < p.len() || !b[..p.len()].eq_ignore_ascii_case(p) {
        return None;
    }
    Some(&s[p.len()..])
}

/// Sparse `[:` opener → `]` close-end index. Records are appended at OPEN time
/// (`open`) and their close filled on the matching `]` (`close`). Because the scanner
/// visits openers in strictly increasing byte position, `pairs` is sorted by opener
/// BY CONSTRUCTION — no sort pass (audit4 F9 removed an O(H log H) `sort_unstable`).
/// Lookups use a monotone cursor: the sole parser caller queries openers
/// outer-then-inner, i.e. in increasing position, so the cursor makes `at` amortized
/// O(1); a rare out-of-order query falls back to a binary search over the sorted vec,
/// so correctness never depends on the monotonicity assumption.
pub(crate) struct HiccupClosers {
    pairs: Vec<(usize, usize)>,
    cursor: Cell<usize>,
}

const HICCUP_UNCLOSED: usize = usize::MAX;

impl HiccupClosers {
    fn new() -> HiccupClosers {
        HiccupClosers {
            pairs: Vec::new(),
            cursor: Cell::new(0),
        }
    }

    /// Reserve a slot for an opener; returns its index for the matching `close`.
    fn open(&mut self, opener: usize) -> usize {
        let idx = self.pairs.len();
        self.pairs.push((opener, HICCUP_UNCLOSED));
        idx
    }

    fn close(&mut self, idx: usize, close_end: usize) {
        self.pairs[idx].1 = close_end;
    }

    fn resolve(&self, idx: usize) -> Option<usize> {
        match self.pairs.get(idx) {
            Some(&(_, HICCUP_UNCLOSED)) | None => None,
            Some(&(_, close)) => Some(close),
        }
    }

    pub(crate) fn at(&self, opener: usize) -> Option<usize> {
        let cur = self.cursor.get();
        if let Some(&(at, _)) = self.pairs.get(cur) {
            if at == opener {
                self.cursor.set(cur + 1);
                return self.resolve(cur);
            }
        }
        // Out-of-order (or first) query: binary search the opener-sorted vec, then
        // re-seat the cursor just past it so a resumed monotone run stays O(1).
        let idx = self
            .pairs
            .binary_search_by_key(&opener, |&(at, _)| at)
            .ok()?;
        self.cursor.set(idx + 1);
        self.resolve(idx)
    }
}

/// Prefix trie for `#+END_<name>` closers. A `#+END_FOOX` closes opener `FOO`, so each
/// trie node stores every closer line whose name has that prefix.
pub(crate) struct EndTrie {
    kids: Vec<Vec<(u8, u32)>>,
    ends: Vec<Vec<usize>>,
    cursor: Vec<Cell<usize>>,
}

impl EndTrie {
    pub(crate) fn new() -> EndTrie {
        EndTrie {
            kids: vec![Vec::new()],
            ends: vec![Vec::new()],
            cursor: vec![Cell::new(0)],
        }
    }

    // scan-owner: (a) source-index build — each closer suffix byte is inserted once
    // during the single source pass.
    pub(crate) fn insert(&mut self, suffix: &str, line_idx: usize) {
        let mut node = 0usize;
        self.ends[node].push(line_idx);
        for &raw in suffix.as_bytes() {
            crate::metrics::scan_work(1);
            if mldoc_name_space_or_eol(raw) {
                break;
            }
            let b = raw.to_ascii_lowercase();
            let mut found = None;
            for &(key, child) in &self.kids[node] {
                crate::metrics::scan_work(1);
                if key == b {
                    found = Some(child as usize);
                    break;
                }
            }
            node = match found {
                Some(child) => child,
                None => {
                    let child = self.kids.len();
                    self.kids.push(Vec::new());
                    self.ends.push(Vec::new());
                    self.cursor.push(Cell::new(0));
                    self.kids[node].push((b, child as u32));
                    child
                }
            };
            self.ends[node].push(line_idx);
        }
    }

    // scan-owner: (b) monotone closer cursor — opener-name bytes are caller-owned,
    // and each trie-node closer cursor advances only forward.
    pub(crate) fn first_after(&self, opener_name: &str, from_line: usize) -> Option<usize> {
        let mut node = 0usize;
        for &raw in opener_name.as_bytes() {
            crate::metrics::scan_work(1);
            if mldoc_name_space_or_eol(raw) {
                break;
            }
            let b = raw.to_ascii_lowercase();
            let mut found = None;
            for &(key, child) in &self.kids[node] {
                crate::metrics::scan_work(1);
                if key == b {
                    found = Some(child as usize);
                    break;
                }
            }
            node = found?;
        }
        let ends = &self.ends[node];
        let cursor = &self.cursor[node];
        let mut i = cursor.get();
        while i < ends.len() && ends[i] <= from_line {
            crate::metrics::scan_work(1);
            i += 1;
        }
        cursor.set(i);
        ends.get(i).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::{Eol, Source};

    #[test]
    fn source_pass_splits_lines_and_indexes_events() {
        let src = Source::scan("a\r\n```rs\nx\n```\n#+END_QUOTEX\n:END:\r  :END:tail\nlast");
        assert_eq!(src.lines.len(), 8);
        assert_eq!(src.lines[0].text, "a");
        assert_eq!(src.lines[0].eol, Eol::CrLf);
        assert_eq!(src.lines[5].eol, Eol::Cr);
        assert_eq!(src.events.fence_lines, vec![1, 3]);
        assert_eq!(src.events.drawer_end_lines, vec![5]);
        assert_eq!(src.events.property_end_lines, vec![5, 6]);
        assert_eq!(src.events.callout_ends.first_after("quote", 0), Some(4));
    }

    #[test]
    fn source_pass_pairs_hiccup_vectors() {
        let src = Source::scan("[:div [:span x]]\n[:div \"x]\"]\n[:div [:\n]");
        assert_eq!(src.events.hiccup_close.at(0), Some(16));
        assert_eq!(src.events.hiccup_close.at(6), Some(15));
        assert_eq!(src.events.hiccup_close.at(17), Some(28));
        assert_eq!(src.events.hiccup_close.at(29), None);

        let src = Source::scan("\" before\n[:div]");
        assert_eq!(src.events.hiccup_close.at(9), Some(15));
    }

    #[test]
    fn callout_end_trie_is_prefix_and_monotone() {
        let src = Source::scan("#+END_NOTE\n#+END_NOTEX\n#+END_WARNING\n");
        let trie = &src.events.callout_ends;
        assert_eq!(trie.first_after("note", 0), Some(1));
        assert_eq!(trie.first_after("note", 1), None);
        assert_eq!(trie.first_after("warning", 0), Some(2));
        assert_eq!(trie.first_after("missing", 0), None);
    }
}

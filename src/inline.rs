//! Inline leaf-parser library — the shared, context-free building blocks of inline
//! parsing, behavior-equivalent to mldoc 1.5.7's inline grammar (`lib/syntax/inline.ml`,
//! verified against the live oracle).
//!
//! The top-level ctx-aware inline pass (lexer → one-pass resolve) lives in the two format
//! resolvers — `crate::resolver` (Markdown) and `crate::org_resolver` (Org). THIS module is
//! their shared leaf kit: byte-class predicates (`is_ws`, `is_ws_or_nl`, …), the
//! bracket/close pre-pair builders (`build_hiccup_close`, `build_nested_close`, …), and the
//! per-construct leaf parsers both resolvers call — page refs (`parse_page_ref`), nested links
//! (`parse_nested_link`), md links/images (`md_link`), autolinks, bare URLs, timestamps, latex
//! spans, hiccup, entities, escapes. Each returns `(node, end)` (or `None`) and does no
//! delimiter pairing of its own; the resolver drives them.
//!
//! Markdown link/image LABELS are re-parsed through the C1 Markdown emphasis port plus
//! mldoc's label-only latex/entity/code/script choices (`resolver::parse_inline_ctx_md_label`).
//! The old standalone v1 `Scanner` inline engine that used to live here was retired once both
//! resolvers shipped.
//!
//! BYTE-SAFETY: all `&str` slicing is at ASCII delimiter / run boundaries or via
//! `char_indices`, never mid-codepoint.

use crate::projection::{Inline, Span, Url};
use crate::source_map::OriginSegment;
use std::ops::Range;

/// Byte offset of `sub` within `parent` (both must share the same backing buffer — `sub`
/// is a sub-slice of `parent`). Used to recover the absolute base for a sub-slice inline
/// parse. Debug-asserts the sub-slice invariant.
#[inline]
pub(crate) fn ptr_base(sub: &str, parent: &str) -> usize {
    debug_assert!(
        sub.as_ptr() as usize >= parent.as_ptr() as usize
            && sub.as_ptr() as usize + sub.len() <= parent.as_ptr() as usize + parent.len(),
        "ptr_base: sub is not a sub-slice of parent"
    );
    sub.as_ptr() as usize - parent.as_ptr() as usize
}

// ---- byte classes ---------------------------------------------------------

#[inline]
pub(crate) fn is_ws(c: u8) -> bool {
    // `\r` is an EOL (handled as `Break`, like `\n`), NOT whitespace — so a whitespace
    // run stops at `\r` and lets the break dispatch fire (C5: CRLF / lone-CR endings).
    // Form feed is part of mldoc's `whitespace_chars` and the npm oracle treats it as a
    // C6 tag/bare-url delimiter. SUB (`0x1a`) is intentionally not included here.
    c == b' ' || c == b'\t' || c == 0x0c
}
#[inline]
pub(crate) fn is_ws_or_nl(c: u8) -> bool {
    is_ws(c) || c == b'\n' || c == b'\r'
}

#[inline]
fn is_tag_url_space_or_eol(c: u8) -> bool {
    // C6 boundary set: source `space_chars @ eol_chars` as enforced by the local
    // npm oracle for actual SUB (`0x1a`). `U+0016` remains ordinary text.
    is_ws_or_nl(c) || c == 0x1a
}

#[inline]
fn email_local_forbidden(c: u8) -> bool {
    matches!(c, b'<' | b'>' | b'@' | b',') || is_ws_or_nl(c)
}

#[inline]
fn email_domain_forbidden(c: u8) -> bool {
    email_local_forbidden(c) || matches!(c, b'\'' | b'"')
}
// ---- shared helpers -------------------------------------------------------

#[inline]
pub(crate) fn char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        1 // continuation/invalid byte: advance one to stay safe
    }
}

/// Monotone current-line floor: answers whether `byte` occurs at/after `from`
/// before the next CR/LF. Used by callers that otherwise would retry a
/// delimiter-to-EOL scan from every opener on the same line.
pub(crate) struct ByteBeforeEolScan {
    byte: u8,
    hit: usize,
    eol: usize,
    initialized: bool,
}

impl ByteBeforeEolScan {
    pub(crate) fn new(byte: u8) -> Self {
        Self { byte, hit: 0, eol: 0, initialized: false }
    }

    pub(crate) fn first_before_eol(&mut self, bb: &[u8], from: usize) -> Option<usize> {
        if !self.initialized || from > self.eol {
            let (hit, eol) = first_byte_or_crlf_for_scan(bb, from, self.byte);
            self.hit = hit;
            self.eol = eol;
            self.initialized = true;
        } else if self.hit < from {
            let (hit, eol) = first_byte_or_crlf_for_scan(bb, from, self.byte);
            self.hit = hit;
            self.eol = eol;
        }
        (self.hit < self.eol).then_some(self.hit)
    }

    pub(crate) fn has_before_eol(&mut self, bb: &[u8], from: usize) -> bool {
        self.first_before_eol(bb, from).is_some()
    }
}

fn first_byte_or_crlf_for_scan(bb: &[u8], from: usize, byte: u8) -> (usize, usize) {
    let mut p = from;
    let mut scanned = 0usize;
    while p < bb.len() && bb[p] != byte && bb[p] != b'\n' && bb[p] != b'\r' {
        scanned += 1;
        p += 1;
    }
    if p < bb.len() {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if p < bb.len() && bb[p] == byte {
        (p, p + 1)
    } else {
        (p, p)
    }
}

/// First index of `needle` in `b[from..]`, or None. (No newline restriction.)
pub(crate) fn find_sub(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > b.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Like `find_sub` but stops at a newline (returns None if a `\n` precedes needle).
#[allow(dead_code)]
pub(crate) fn find_sub_line(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= b.len() {
        if b[i] == b'\n' {
            return None;
        }
        if &b[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[derive(Default)]
pub(crate) struct AngleBoundaryScan {
    next: usize,
    initialized: bool,
}

impl AngleBoundaryScan {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn first_from(&mut self, b: &[u8], from: usize) -> usize {
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len() && b[p] != b'>' && !is_ws_or_nl(b[p]) {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

pub(crate) struct AutolinkScan {
    boundary: AngleBoundaryScan,
}

impl AutolinkScan {
    pub(crate) fn new() -> Self {
        Self {
            boundary: AngleBoundaryScan::new(),
        }
    }
}

pub(crate) struct EmailAutolinkScan {
    no_at_from: usize,
    domain_boundary: EmailDomainBoundaryScan,
}

impl EmailAutolinkScan {
    pub(crate) fn new() -> Self {
        Self {
            no_at_from: usize::MAX,
            domain_boundary: EmailDomainBoundaryScan::new(),
        }
    }
}

#[derive(Default)]
struct EmailDomainBoundaryScan {
    next: usize,
    initialized: bool,
}

impl EmailDomainBoundaryScan {
    fn new() -> Self {
        Self::default()
    }

    fn first_from(&mut self, b: &[u8], from: usize) -> usize {
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len() && !email_domain_forbidden(b[p]) {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

#[derive(Clone, Default)]
struct TimestampCloseCursor {
    next: usize,
    initialized: bool,
    no_close_from: usize,
}

impl TimestampCloseCursor {
    fn new() -> Self {
        Self {
            next: 0,
            initialized: false,
            no_close_from: usize::MAX,
        }
    }

    fn first_close_or_lf(&mut self, b: &[u8], from: usize, close: u8) -> usize {
        if from >= self.no_close_from {
            return b.len();
        }
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len() && b[p] != close && b[p] != b'\n' {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            } else {
                self.no_close_from = self.no_close_from.min(from);
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

#[derive(Clone)]
pub(crate) struct TimestampCloseScan {
    angle: TimestampCloseCursor,
    bracket: TimestampCloseCursor,
    angle_token: TimestampTokenCursor,
    bracket_token: TimestampTokenCursor,
}

impl TimestampCloseScan {
    pub(crate) fn new() -> Self {
        Self {
            angle: TimestampCloseCursor::new(),
            bracket: TimestampCloseCursor::new(),
            angle_token: TimestampTokenCursor::new(),
            bracket_token: TimestampTokenCursor::new(),
        }
    }

    fn first_close_or_lf(&mut self, b: &[u8], from: usize, close: u8) -> usize {
        match close {
            b'>' => self.angle.first_close_or_lf(b, from, close),
            b']' => self.bracket.first_close_or_lf(b, from, close),
            _ => b.len(),
        }
    }

    fn first_token_boundary_or_lf(&mut self, b: &[u8], from: usize, close: u8) -> usize {
        match close {
            b'>' => self.angle_token.first_boundary_or_lf(b, from, close),
            b']' => self.bracket_token.first_boundary_or_lf(b, from, close),
            _ => b.len(),
        }
    }
}

#[derive(Clone, Default)]
struct TimestampTokenCursor {
    next: usize,
    initialized: bool,
    no_boundary_from: usize,
}

impl TimestampTokenCursor {
    fn new() -> Self {
        Self {
            next: 0,
            initialized: false,
            no_boundary_from: usize::MAX,
        }
    }

    fn first_boundary_or_lf(&mut self, b: &[u8], from: usize, close: u8) -> usize {
        if from >= self.no_boundary_from {
            return b.len();
        }
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len()
                && b[p] != close
                && b[p] != b'\n'
                && !is_mldoc_timestamp_space(b[p])
            {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            } else {
                self.no_boundary_from = self.no_boundary_from.min(from);
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

pub(crate) struct BareUrlScan {
    no_scheme_from: usize,
}

impl BareUrlScan {
    pub(crate) fn new() -> Self {
        Self {
            no_scheme_from: usize::MAX,
        }
    }
}

/// Boundary-run map for hash tags: for every byte in a run of tag delimiters, `true`
/// means the run suffix is followed by whitespace/eol/EOF and therefore terminates the
/// tag. The resolver builds this once per inline string so `#`×n does not re-scan the
/// same delimiter suffix at every failed tag dispatch.
pub(crate) fn build_tag_boundary_runs(s: &str) -> Vec<bool> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = vec![false; n];
    let mut i = 0usize;
    while i < n {
        if !TAG_DELIMS.contains(&b[i]) {
            i += char_len(b[i]);
            continue;
        }
        let start = i;
        while i < n && TAG_DELIMS.contains(&b[i]) {
            i += 1;
        }
        let boundary = i >= n || is_tag_url_space_or_eol(b[i]);
        if boundary {
            for slot in &mut out[start..i] {
                *slot = true;
            }
        }
    }
    crate::metrics::scan_work(n);
    out
}

// ---- page ref / nested link -----------------------------------------------

const PR_MEMO_UNSEEN: usize = usize::MAX;
const PR_MEMO_NONE: usize = usize::MAX - 1;

pub(crate) struct PageRefScan {
    source_len: Option<usize>,
    page_ref_walk_memo: Vec<usize>,
    org_link2_walk_memo: Vec<usize>,
    next_rr_or_nl_memo: Vec<usize>,
    nested_link_memo: Vec<usize>,
    next_crlf_initialized: bool,
    next_crlf_last_query: usize,
    next_crlf_hit: usize,
}

impl PageRefScan {
    pub(crate) fn new() -> Self {
        Self {
            source_len: None,
            page_ref_walk_memo: Vec::new(),
            org_link2_walk_memo: Vec::new(),
            next_rr_or_nl_memo: Vec::new(),
            nested_link_memo: Vec::new(),
            next_crlf_initialized: false,
            next_crlf_last_query: 0,
            next_crlf_hit: 0,
        }
    }

    fn check_source(&mut self, len: usize) {
        match self.source_len {
            Some(existing) => debug_assert_eq!(existing, len),
            None => self.source_len = Some(len),
        }
    }

    fn page_ref_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.page_ref_walk_memo.is_empty() {
            self.page_ref_walk_memo = vec![PR_MEMO_UNSEEN; len + 1];
        }
        &mut self.page_ref_walk_memo
    }

    fn org_link2_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.org_link2_walk_memo.is_empty() {
            self.org_link2_walk_memo = vec![PR_MEMO_UNSEEN; len + 1];
        }
        &mut self.org_link2_walk_memo
    }

    fn rr_or_nl_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.next_rr_or_nl_memo.is_empty() {
            self.next_rr_or_nl_memo = vec![PR_MEMO_UNSEEN; len + 1];
        }
        &mut self.next_rr_or_nl_memo
    }

    fn nested_link_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.nested_link_memo.is_empty() {
            self.nested_link_memo = vec![PR_MEMO_UNSEEN; len + 1];
        }
        &mut self.nested_link_memo
    }

    fn page_ref_close(&mut self, bb: &[u8], start: usize) -> usize {
        let mut j = start;
        let mut visited: Vec<usize> = Vec::new();
        let result = loop {
            if j >= bb.len() {
                break PR_MEMO_NONE;
            }
            let memo = self
                .page_ref_memo(bb.len())
                .get(j)
                .copied()
                .unwrap_or(PR_MEMO_UNSEEN);
            if memo != PR_MEMO_UNSEEN {
                break memo;
            }
            visited.push(j);
            match bb[j] {
                b'\n' | b'\r' => {
                    crate::metrics::scan_work(1);
                    break PR_MEMO_NONE;
                }
                b']' if j + 1 < bb.len() && bb[j + 1] == b']' => {
                    crate::metrics::scan_work(2);
                    break j;
                }
                b'\\' if j + 1 < bb.len() => {
                    crate::metrics::scan_work(2);
                    j += 2;
                }
                c => {
                    let w = char_len(c);
                    crate::metrics::scan_work(w);
                    j += w;
                }
            }
        };
        if !visited.is_empty() {
            let memo = self.page_ref_memo(bb.len());
            for pos in visited {
                memo[pos] = result;
            }
        }
        result
    }

    pub(crate) fn org_link2_close(&mut self, bb: &[u8], at: usize) -> Option<usize> {
        self.check_source(bb.len());
        if bb.get(at) != Some(&b'[') || bb.get(at + 1) != Some(&b'[') {
            return None;
        }
        let name_start = at + 2;
        let close = self.org_link2_walk(bb, name_start);
        if close == PR_MEMO_NONE
            || close == name_start
            || close + 1 >= bb.len()
            || bb[close] != b']'
            || bb[close + 1] != b']'
        {
            return None;
        }
        Some(close)
    }

    fn org_link2_walk(&mut self, bb: &[u8], start: usize) -> usize {
        let mut j = start;
        let mut visited: Vec<usize> = Vec::new();
        let result = loop {
            if j >= bb.len() {
                break PR_MEMO_NONE;
            }
            let memo = self
                .org_link2_memo(bb.len())
                .get(j)
                .copied()
                .unwrap_or(PR_MEMO_UNSEEN);
            if memo != PR_MEMO_UNSEEN {
                break memo;
            }
            visited.push(j);
            match bb[j] {
                b'\n' | b'\r' => {
                    crate::metrics::scan_work(1);
                    break PR_MEMO_NONE;
                }
                b'\\' if j + 1 < bb.len() => {
                    let w = char_len(bb[j + 1]);
                    crate::metrics::scan_work(1 + w);
                    j += 1 + w;
                }
                b']' if j + 1 < bb.len() && bb[j + 1] == b']' => {
                    crate::metrics::scan_work(2);
                    break j;
                }
                b']' => {
                    crate::metrics::scan_work(1);
                    j += 1;
                }
                c => {
                    let w = char_len(c);
                    crate::metrics::scan_work(w);
                    j += w;
                }
            }
        };
        if !visited.is_empty() {
            let memo = self.org_link2_memo(bb.len());
            for pos in visited {
                memo[pos] = result;
            }
        }
        result
    }

    fn next_rr_or_nl(&mut self, bb: &[u8], from: usize) -> Option<usize> {
        self.check_source(bb.len());
        let mut j = from;
        let mut visited: Vec<usize> = Vec::new();
        let result = loop {
            if j + 1 >= bb.len() {
                break PR_MEMO_NONE;
            }
            let memo = self
                .rr_or_nl_memo(bb.len())
                .get(j)
                .copied()
                .unwrap_or(PR_MEMO_UNSEEN);
            if memo != PR_MEMO_UNSEEN {
                break memo;
            }
            visited.push(j);
            if bb[j] == b'\n' {
                crate::metrics::scan_work(1);
                break PR_MEMO_NONE;
            }
            if bb[j] == b']' && bb[j + 1] == b']' {
                crate::metrics::scan_work(2);
                break j;
            }
            crate::metrics::scan_work(1);
            j += 1;
        };
        if !visited.is_empty() {
            let memo = self.rr_or_nl_memo(bb.len());
            for pos in visited {
                memo[pos] = result;
            }
        }
        (result != PR_MEMO_NONE).then_some(result)
    }

    pub(crate) fn next_crlf_at_or_after(&mut self, bb: &[u8], from: usize) -> usize {
        self.check_source(bb.len());
        if !self.next_crlf_initialized || from > self.next_crlf_hit || from < self.next_crlf_last_query
        {
            self.next_crlf_hit = first_crlf_for_page_ref_scan(bb, from);
            self.next_crlf_initialized = true;
        }
        self.next_crlf_last_query = from;
        self.next_crlf_hit
    }

    pub(crate) fn parse_nested_link(&mut self, s: &str, at: usize) -> Option<(usize, String)> {
        self.check_source(s.len());
        if !s[at..].starts_with("[[") {
            return None;
        }
        let cached = self
            .nested_link_memo(s.len())
            .get(at)
            .copied()
            .unwrap_or(PR_MEMO_UNSEEN);
        if cached != PR_MEMO_UNSEEN {
            return (cached != PR_MEMO_NONE).then(|| (cached, s[at..cached].to_string()));
        }
        let end = match_brackets_end_with_scan(s, at, self).and_then(|end| {
            let inner = &s[at + 2..end - 2];
            (nested_children_count(inner) > 1).then_some(end)
        });
        {
            let memo = self.nested_link_memo(s.len());
            memo[at] = end.unwrap_or(PR_MEMO_NONE);
        }
        end.map(|end| (end, s[at..end].to_string()))
    }
}

fn first_crlf_for_page_ref_scan(bb: &[u8], from: usize) -> usize {
    let mut p = from;
    let mut scanned = 0usize;
    while p < bb.len() && bb[p] != b'\n' && bb[p] != b'\r' {
        scanned += 1;
        p += 1;
    }
    if p < bb.len() {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    p
}

/// `[[ name ]]` where name is non-empty, contains no newline, and ends at the first
/// `]]` (single `]` allowed inside). Returns (end_index, name, full_text).
pub(crate) fn parse_page_ref(s: &str, at: usize) -> Option<(usize, String, String)> {
    let b = s.as_bytes();
    if !s[at..].starts_with("[[") {
        return None;
    }
    let name_start = at + 2;
    finish_page_ref(s, at, page_ref_close_raw(b, name_start))
}

pub(crate) fn parse_page_ref_end_with_scan(
    s: &str,
    at: usize,
    scan: &mut PageRefScan,
) -> Option<usize> {
    let b = s.as_bytes();
    scan.check_source(b.len());
    if !s[at..].starts_with("[[") {
        return None;
    }
    let name_start = at + 2;
    finish_page_ref_end(s, at, scan.page_ref_close(b, name_start))
}

pub(crate) fn parse_page_ref_with_scan(
    s: &str,
    at: usize,
    scan: &mut PageRefScan,
) -> Option<(usize, String, String)> {
    let end = parse_page_ref_end_with_scan(s, at, scan)?;
    Some(build_page_ref(s, at, end))
}

fn finish_page_ref(s: &str, at: usize, close: usize) -> Option<(usize, String, String)> {
    let end = finish_page_ref_end(s, at, close)?;
    Some(build_page_ref(s, at, end))
}

fn finish_page_ref_end(s: &str, at: usize, close: usize) -> Option<usize> {
    if close == PR_MEMO_NONE {
        return None;
    }
    let b = s.as_bytes();
    let name_start = at + 2;
    if close + 1 >= b.len() || b[close] != b']' || b[close + 1] != b']' {
        return None;
    }
    if close == name_start {
        return None; // empty name
    }
    Some(close + 2)
}

fn build_page_ref(s: &str, at: usize, end: usize) -> (usize, String, String) {
    let name_start = at + 2;
    let close = end - 2;
    let name = unescape(&s[name_start..close]); // value is unescaped; full stays raw
    let full = s[at..end].to_string();
    (end, name, full)
}

fn page_ref_close_raw(b: &[u8], name_start: usize) -> usize {
    let n = b.len();
    let mut j = name_start;
    while j < n {
        let c = b[j];
        if c == b'\n' || c == b'\r' {
            crate::metrics::scan_work(1);
            return PR_MEMO_NONE;
        }
        if c == b']' {
            if j + 1 < n && b[j + 1] == b']' {
                crate::metrics::scan_work(2);
                break; // closing "]]"
            }
            // single ']' allowed in name
            crate::metrics::scan_work(1);
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < n {
            crate::metrics::scan_work(2);
            j += 2; // backslash escapes next char inside page name
            continue;
        }
        let w = char_len(c);
        crate::metrics::scan_work(w);
        j += w;
    }
    if j + 1 >= n || b[j] != b']' || b[j + 1] != b']' {
        return PR_MEMO_NONE;
    }
    j
}

/// nested link `[[ ... ]]` whose inner text parses into >1 (label | nested) child.
pub(crate) fn parse_nested_link(s: &str, at: usize) -> Option<(usize, String)> {
    let end = match_brackets_end_raw(s, at)?;
    let content = &s[at..end];
    let inner = &content[2..content.len() - 2];
    if nested_children_count(inner) > 1 {
        Some((end, content.to_string()))
    } else {
        None
    }
}

pub(crate) fn parse_nested_link_with_scan(
    s: &str,
    at: usize,
    scan: &mut PageRefScan,
) -> Option<(usize, String)> {
    scan.parse_nested_link(s, at)
}

/// Bracket matcher: from `[[`, count levels using `]]` chunks (mldoc match_brackets).
/// Returns (end_index, matched_string). Stops at a newline (returns None).
fn match_brackets(s: &str, at: usize) -> Option<(usize, String)> {
    let end = match_brackets_end_raw(s, at)?;
    Some((end, s[at..end].to_string()))
}

fn match_brackets_end_raw(s: &str, at: usize) -> Option<usize> {
    let b = s.as_bytes();
    if !s[at..].starts_with("[[") {
        return None;
    }
    let mut level: i32 = 1;
    let mut pos = at + 2;
    loop {
        let idx = next_rr_or_nl_raw(b, pos)?;
        let chunk = &s[pos..idx];
        level += count_occurrences(chunk, "[[") as i32 - 1;
        pos = idx + 2;
        if level <= 0 {
            return Some(pos);
        }
    }
}

fn match_brackets_end_with_scan(s: &str, at: usize, scan: &mut PageRefScan) -> Option<usize> {
    let b = s.as_bytes();
    scan.check_source(b.len());
    if !s[at..].starts_with("[[") {
        return None;
    }
    let mut level: i32 = 1;
    let mut pos = at + 2;
    loop {
        let idx = scan.next_rr_or_nl(b, pos)?;
        let chunk = &s[pos..idx];
        level += count_occurrences(chunk, "[[") as i32 - 1;
        pos = idx + 2;
        if level <= 0 {
            return Some(pos);
        }
    }
}

fn next_rr_or_nl_raw(b: &[u8], from: usize) -> Option<usize> {
    let mut k = from;
    while k + 1 < b.len() {
        if b[k] == b'\n' {
            crate::metrics::scan_work(1);
            return None;
        }
        if b[k] == b']' && b[k + 1] == b']' {
            crate::metrics::scan_work(2);
            return Some(k);
        }
        crate::metrics::scan_work(1);
        k += 1;
    }
    None
}

fn count_occurrences(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    crate::metrics::scan_work(hay.len());
    let hb = hay.as_bytes();
    let nb = needle.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + nb.len() <= hb.len() {
        if &hb[i..i + nb.len()] == nb {
            count += 1;
            i += nb.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Count the (label | inner-nested-link) children of a nested-link's inner text.
/// Returns 1 if the inner text doesn't fully decompose (mldoc: single Label fallback).
fn nested_children_count(inner: &str) -> usize {
    let b = inner.as_bytes();
    let n = b.len();
    let mut j = 0;
    let mut count = 0;
    while j < n {
        if b[j] != b'[' {
            // label run until next '['
            while j < n && b[j] != b'[' {
                j += char_len(b[j]);
            }
            count += 1;
        } else {
            match match_brackets(inner, j) {
                Some((end, _)) => {
                    count += 1;
                    j = end;
                }
                None => {
                    // '[' not starting a valid match -> label_parse fails here;
                    // mldoc's consume:All then fails -> whole inner is one Label.
                    return 1;
                }
            }
        }
    }
    count
}

// ---- tags -----------------------------------------------------------------

const TAG_DELIMS: &[u8] = &[b',', b';', b'.', b'!', b'?', b'\'', b'"', b':', b'#'];
const TAG_STOP: &[u8] = &[b'#', b',', b'!', b'?', b'\'', b'"', b':'];

#[derive(Clone, Copy)]
pub(crate) enum TagReparse {
    Markdown,
    Org,
}

/// Parse a tag name starting at `start` (just after '#'). Returns (end_index,
/// children). This mirrors mldoc's two-stage `hash_tag`: first capture a raw
/// `Hash_tag.hashtag_name` string (where `[[...]]` is admitted by `page_ref`),
/// then reparse that captured string with the format's `nested_link_or_link`.
/// `unescape_plain`: Markdown unescapes the plain runs (`#ab\|` → `ab|`); Org keeps
/// backslashes literal (`#ab\|` → `ab\|`), matching its no-unescape invariant (C4).
pub(crate) fn parse_tag_name(
    s: &str,
    start: usize,
    unescape_plain: bool,
    base: usize,
    format: TagReparse,
    boundary_runs: Option<&[bool]>,
    scan: &mut PageRefScan,
) -> (usize, Vec<Inline>) {
    let end = capture_tag_name_end(s, start, boundary_runs, scan);
    if end == start {
        return (start, Vec::new());
    }
    let children = reparse_tag_name(s, start, end, unescape_plain, base, format);
    (end, children)
}

fn capture_tag_name_end(
    s: &str,
    start: usize,
    boundary_runs: Option<&[bool]>,
    scan: &mut PageRefScan,
) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = start;
    let mut consumed = false;
    loop {
        // `hashtag_name_part` case 1: a non-empty run of non-space/eol chars
        // that are not tag delimiters and not `[`.
        let run_start = i;
        while i < n {
            let c = b[i];
            if is_tag_url_space_or_eol(c) || TAG_DELIMS.contains(&c) || c == b'[' {
                break;
            }
            i += char_len(c);
        }
        if i > run_start {
            consumed = true;
            continue;
        }
        if i >= n {
            break;
        }
        let c = b[i];
        if is_tag_url_space_or_eol(c) {
            break;
        }
        if c == b'[' {
            // `hashtag_name_part` case 2: raw `page_ref` capture. Nested-link
            // recognition happens only in the second-stage reparse. The source
            // `page_ref` capture is EOL-bounded, even when a backslash precedes
            // the newline.
            debug_assert!(!b[start..i].iter().any(|&c| c == b'\n' || c == b'\r'));
            if let Some(end) = parse_page_ref_end_with_scan(s, i, scan) {
                if scan.next_crlf_at_or_after(b, i) >= end {
                    i = end;
                    consumed = true;
                    continue;
                }
            }
        }

        // `hashtag_name_part` case 3a: if the whole delimiter run is followed
        // by space/eol/EOF, stop before the entire run.
        if TAG_DELIMS.contains(&c) {
            let boundary_run = boundary_runs
                .and_then(|runs| runs.get(i))
                .copied()
                .unwrap_or_else(|| {
                    let mut k = i;
                    let mut scanned = 0usize;
                    while k < n && TAG_DELIMS.contains(&b[k]) {
                        scanned += 1;
                        k += 1;
                    }
                    crate::metrics::scan_work(scanned);
                    k > i && (k >= n || is_tag_url_space_or_eol(b[k]))
                });
            if boundary_run {
                break;
            }
        }

        // `hashtag_name_part` case 3b: otherwise consume exactly one char unless
        // it is a hard stop. This is what lets `.` and `;` continue when they are
        // not a trailing delimiter run, and lets a lone invalid `[` be literal.
        if TAG_STOP.contains(&c) {
            break;
        }
        i += char_len(c);
        consumed = true;
    }
    if consumed { i } else { start }
}

fn reparse_tag_name(
    s: &str,
    start: usize,
    end: usize,
    unescape_plain: bool,
    base: usize,
    format: TagReparse,
) -> Vec<Inline> {
    let tag = &s[start..end];
    let b = tag.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let tag_base = base + start;
    let mut children = Vec::new();
    let mut plain = String::new();
    let mut plain_start = 0usize;
    let mut plain_end = 0usize;
    let mut md_link_scan = MdLinkScan::new();
    let mut org_inline_scan = crate::org_resolver::OrgInlineScan::new();

    macro_rules! push_plain_raw {
        ($local_start:expr, $local_end:expr) => {{
            if plain.is_empty() {
                plain_start = $local_start;
            }
            plain_end = $local_end;
            plain.push_str(&tag[$local_start..$local_end]);
        }};
    }
    macro_rules! flush_plain {
        () => {{
            if !plain.is_empty() {
                let raw = std::mem::take(&mut plain);
                children.push(tag_plain(
                    &raw,
                    tag_base + plain_start,
                    tag_base + plain_end,
                    unescape_plain,
                ));
            }
        }};
    }

    while i < n {
        // mldoc `hash_tag` stage 2: plain run is non-space/eol and not `[`.
        let run_start = i;
        while i < n && !is_tag_url_space_or_eol(b[i]) && b[i] != b'[' {
            i += char_len(b[i]);
        }
        if i > run_start {
            push_plain_raw!(run_start, i);
            continue;
        }
        if i >= n {
            break;
        }
        if b[i] == b'[' {
            let parsed = match format {
                TagReparse::Markdown => {
                    try_nested_link_or_link_md_tag(tag, i, tag_base, &mut md_link_scan)
                }
                TagReparse::Org => {
                    crate::org_resolver::try_nested_link_or_link_org(
                        tag,
                        b,
                        i,
                        tag_base,
                        &mut org_inline_scan,
                    )
                }
            };
            if let Some((node, next)) = parsed {
                flush_plain!();
                children.push(node);
                i = next;
                continue;
            }
        }
        let next = i + char_len(b[i]);
        push_plain_raw!(i, next);
        i = next;
    }
    flush_plain!();

    concat_plains(children)
}

fn tag_plain(raw: &str, abs_start: usize, abs_end: usize, unescape_plain: bool) -> Inline {
    let text = if unescape_plain { unescape(raw) } else { raw.to_string() };
    if unescape_plain && raw.contains('\\') {
        crate::source_map::make_plain(
            text,
            Span(abs_start, abs_end),
            unescape_origins(raw, abs_start),
            raw,
            abs_start,
        )
    } else {
        Inline::Plain {
            text,
            span: Some(Span(abs_start, abs_end)),
            span_map: None,
        }
    }
}

fn try_nested_link_or_link_md_tag(
    s: &str,
    at: usize,
    base: usize,
    scan: &mut MdLinkScan,
) -> Option<(Inline, usize)> {
    if s[at..].starts_with("[[") {
        if let Some((end, content)) = parse_nested_link_with_scan(s, at, scan.page_ref_scan()) {
            return Some((Inline::NestedLink { content, span: Some(Span(base + at, base + end)) }, end));
        }
        if let Some((end, name, full)) = parse_page_ref_with_scan(s, at, scan.page_ref_scan()) {
            return Some((
                Inline::Link {
                    url: Url::PageRef { v: name },
                    label: vec![],
                    full,
                    image: false,
                    metadata: String::new(),
                    title: None,
                    span: Some(Span(base + at, base + end)),
                },
                end,
            ));
        }
    }
    let (mut node, end) = md_link_with_scan(s, at, false, base, scan)?;
    crate::projection::set_inline_span(&mut node, Some(Span(base + at, base + end)));
    Some((node, end))
}

fn concat_plains(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for node in nodes {
        match (out.last_mut(), node) {
            (
                Some(Inline::Plain {
                    text: prev,
                    span: prev_span,
                    span_map: prev_map,
                }),
                Inline::Plain {
                    text,
                    span,
                    span_map,
                },
            ) => {
                let shift = prev.len();
                if prev_map.is_some() || span_map.is_some() {
                    let map = prev_map.get_or_insert_with(Vec::new);
                    if map.is_empty() {
                        if let Some(Span(start, end)) = *prev_span {
                            crate::source_map::push_wire_segment(map, 0, start, end - start);
                        }
                    }
                    match span_map {
                        Some(mut segments) => {
                            for seg in &mut segments {
                                seg.0 += shift;
                            }
                            map.extend(segments);
                        }
                        None => {
                            if let Some(Span(start, end)) = span {
                                crate::source_map::push_wire_segment(map, shift, start, end - start);
                            }
                        }
                    }
                }
                prev.push_str(&text);
                *prev_span = match (*prev_span, span) {
                    (Some(Span(start, _)), Some(Span(_, end))) => Some(Span(start, end)),
                    _ => None,
                };
            }
            (_, node) => out.push(node),
        }
    }
    out
}

// ---- macros ---------------------------------------------------------------

/// Parse a macro inner string into (name, args). Returns None if arg splitting
/// doesn't consume the whole arg string with valid macro_args (mldoc consume:All).
pub(crate) fn parse_macro(inner: &str) -> Option<(String, Vec<String>)> {
    let b = inner.as_bytes();
    let n = b.len();
    // name = chars until '}' / '(' / ' '
    let mut j = 0;
    while j < n && b[j] != b'}' && b[j] != b'(' && b[j] != b' ' {
        j += char_len(b[j]);
    }
    if j == 0 {
        return None;
    }
    let name = inner[..j].to_string();
    let args_str = &inner[j..];
    if args_str.is_empty() {
        return Some((name, vec![]));
    }
    let mut scan = PageRefScan::new();
    let args = parse_macro_args(args_str, &mut scan)?;
    Some((name, args))
}

/// mldoc macro_args: `optional spaces *> sep_by ',' (spaces *> macro_arg <* spaces)`
/// with consume:All. Returns None if any arg can't be cleanly consumed.
fn parse_macro_args(s: &str, scan: &mut PageRefScan) -> Option<Vec<String>> {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    let skip_sp = |b: &[u8], mut i: usize| {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        i
    };
    i = skip_sp(b, i);
    let mut args = Vec::new();
    // empty after spaces -> a single empty arg? mldoc sep_by on "" yields []. But
    // args_str begins with the separator content after the name; treat all-space as [].
    if i >= n {
        return Some(vec![]);
    }
    loop {
        i = skip_sp(b, i);
        let (arg, ni) = parse_macro_arg(s, i, scan)?;
        i = skip_sp(b, ni);
        args.push(arg);
        if i >= n {
            break;
        }
        if b[i] == b',' {
            i += 1;
            continue;
        }
        // leftover that isn't a separator -> consume:All fails.
        return None;
    }
    Some(args)
}

/// One macro arg: nested-link content | page-ref | `(( .. ))` | `"..."` | until ','.
/// The plain fallback is mldoc `take_while1 (c <> ',')` and keeps trailing spaces
/// (`lib/syntax/inline.ml:979-988`).
fn parse_macro_arg(s: &str, at: usize, scan: &mut PageRefScan) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if at >= n {
        return None;
    }
    // nested link content
    if s[at..].starts_with("[[") {
        if let Some((end, content)) = parse_nested_link_with_scan(s, at, scan) {
            return Some((content, end));
        }
        if let Some((end, _name, full)) = parse_page_ref_with_scan(s, at, scan) {
            return Some((full, end));
        }
    }
    // (( ... ))
    if s[at..].starts_with("((") {
        let inner_start = at + 2;
        let mut j = inner_start;
        while j < n && b[j] != b')' {
            j += 1;
        }
        if j > inner_start && j + 1 < n && b[j] == b')' && b[j + 1] == b')' {
            return Some((s[at..j + 2].to_string(), j + 2));
        }
    }
    // quoted "..."
    if b[at] == b'"' {
        let mut j = at + 1;
        while j < n && b[j] != b'"' {
            if b[j] == b'\\' && j + 1 < n {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j < n && b[j] == b'"' {
            return Some((s[at..j + 1].to_string(), j + 1));
        }
    }
    // until ','
    let mut j = at;
    while j < n && b[j] != b',' {
        j += char_len(b[j]);
    }
    if j == at {
        return None;
    }
    Some((s[at..j].to_string(), j))
}

// ---- footnote ref ---------------------------------------------------------

pub(crate) fn parse_footnote_ref(s: &str, at: usize) -> Option<(usize, String)> {
    // `[^ id ]` : id non-empty, no ']' / whitespace.
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("[^") {
        return None;
    }
    let id_start = at + 2;
    let mut j = id_start;
    while j < n && b[j] != b']' && !is_ws_or_nl(b[j]) {
        j += char_len(b[j]);
    }
    if j == id_start || j >= n || b[j] != b']' {
        return None;
    }
    Some((j + 1, s[id_start..j].to_string()))
}

// ---- markdown link / image ------------------------------------------------

struct MdLink {
    node: Inline,
    end: usize,
}

const MD_LINK_NONE: usize = usize::MAX;

pub(crate) struct MdLinkScan {
    source_len: Option<usize>,
    label_brackets: Option<BalancedEndTable>,
    url_parens: Option<BalancedEndTable>,
    page_refs: Option<Vec<usize>>,
    code_spans: Option<Vec<usize>>,
    page_ref_scan: PageRefScan,
    metadata_rbrace: ByteBeforeEolScan,
}

impl MdLinkScan {
    pub(crate) fn new() -> Self {
        Self {
            source_len: None,
            label_brackets: None,
            url_parens: None,
            page_refs: None,
            code_spans: None,
            page_ref_scan: PageRefScan::new(),
            metadata_rbrace: ByteBeforeEolScan::new(b'}'),
        }
    }

    fn check_source(&mut self, s: &str) {
        match self.source_len {
            Some(len) => debug_assert_eq!(len, s.len()),
            None => self.source_len = Some(s.len()),
        }
    }

    fn label_bracket_end(&mut self, s: &str, at: usize) -> usize {
        self.check_source(s);
        self.label_brackets
            .get_or_insert_with(|| BalancedEndTable::build(s, b'[', b']', b"[]", b"", b""))
            .end(at)
    }

    fn url_paren_end(&mut self, s: &str, at: usize) -> usize {
        self.check_source(s);
        self.url_parens
            .get_or_insert_with(|| BalancedEndTable::build(s, b'(', b')', b"()", b"\r\n", b""))
            .end(at)
    }

    fn page_ref_end(&mut self, s: &str, at: usize) -> Option<usize> {
        self.check_source(s);
        let ends = self.page_refs.get_or_insert_with(|| build_page_ref_ends(s));
        let end = ends.get(at).copied().unwrap_or(MD_LINK_NONE);
        (end != MD_LINK_NONE).then_some(end)
    }

    fn code_span_end(&mut self, s: &str, at: usize) -> Option<usize> {
        self.check_source(s);
        let ends = self.code_spans.get_or_insert_with(|| build_code_span_ends(s));
        let end = ends.get(at).copied().unwrap_or(MD_LINK_NONE);
        (end != MD_LINK_NONE).then_some(end)
    }

    pub(crate) fn page_ref_scan(&mut self) -> &mut PageRefScan {
        &mut self.page_ref_scan
    }

    fn metadata_close(&mut self, b: &[u8], from: usize) -> Option<usize> {
        self.metadata_rbrace.first_before_eol(b, from)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BalancedToken {
    Other,
    Stop,
    Left,
    Right,
}

struct BalancedEndTable {
    end: Vec<usize>,
}

impl BalancedEndTable {
    fn build(
        s: &str,
        left: u8,
        right: u8,
        escape_chars: &[u8],
        other_delims: &[u8],
        excluded_ending_chars: &[u8],
    ) -> Self {
        let b = s.as_bytes();
        let n = b.len();
        let mut kind = vec![BalancedToken::Other; n];
        let mut next = vec![n; n];

        for i in 0..n {
            let c = b[i];
            if other_delims.contains(&c) {
                kind[i] = BalancedToken::Stop;
                next[i] = (i + 1).min(n);
                continue;
            }
            if excluded_ending_chars.contains(&c) {
                let remain = n - i;
                if remain < 2 || b.get(i + 1).is_some_and(|c2| other_delims.contains(c2)) {
                    kind[i] = BalancedToken::Stop;
                    next[i] = (i + 1).min(n);
                } else {
                    next[i] = i + 1;
                }
                continue;
            }
            if c == left {
                kind[i] = BalancedToken::Left;
                next[i] = i + 1;
                continue;
            }
            if c == right {
                kind[i] = BalancedToken::Right;
                next[i] = i + 1;
                continue;
            }
            if c == b'\\' {
                let mut ni = i + 1;
                if ni < n {
                    let escaped = b[ni];
                    let next_plain = !other_delims.contains(&escaped)
                        && !excluded_ending_chars.contains(&escaped)
                        && escaped != left
                        && escaped != right;
                    if escape_chars.contains(&escaped) || next_plain {
                        ni = (ni + char_len(escaped)).min(n);
                    }
                }
                next[i] = ni;
                continue;
            }
            next[i] = (i + char_len(c)).min(n);
        }

        let mut end = vec![n; n + 1];
        for i in (0..n).rev() {
            end[i] = match kind[i] {
                BalancedToken::Stop | BalancedToken::Right => i,
                BalancedToken::Other => end[next[i]],
                BalancedToken::Left => {
                    let r = end[next[i]];
                    if r < n && kind[r] == BalancedToken::Right {
                        end[next[r]]
                    } else {
                        r
                    }
                }
            };
        }
        crate::metrics::scan_work(n);
        Self { end }
    }

    fn end(&self, at: usize) -> usize {
        crate::metrics::scan_work(1);
        self.end.get(at).copied().unwrap_or_else(|| self.end.len().saturating_sub(1))
    }
}

fn build_page_ref_ends(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut first_close_or_stop = vec![n; n + 2];
    for i in (0..n).rev() {
        let c = b[i];
        first_close_or_stop[i] = if c == b'\n' || c == b'\r' {
            i
        } else if c == b'\\' && i + 1 < n {
            first_close_or_stop[(i + 2).min(n)]
        } else if c == b']' && i + 1 < n && b[i + 1] == b']' {
            i
        } else {
            first_close_or_stop[(i + char_len(c)).min(n)]
        };
    }

    let mut out = vec![MD_LINK_NONE; n];
    for i in 0..n.saturating_sub(1) {
        if b[i] != b'[' || b[i + 1] != b'[' {
            continue;
        }
        let name_start = i + 2;
        let close = first_close_or_stop[name_start.min(n)];
        if close > name_start && close + 1 < n && b[close] == b']' && b[close + 1] == b']' {
            out[i] = close + 2;
        }
    }
    crate::metrics::scan_work(n);
    out
}

fn build_code_span_ends(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut next_backtick = vec![n; n + 1];
    let mut next_eol = vec![n; n + 1];
    let mut next_double = vec![n; n + 1];
    for i in (0..n).rev() {
        next_backtick[i] = if b[i] == b'`' { i } else { next_backtick[i + 1] };
        next_eol[i] = if b[i] == b'\n' || b[i] == b'\r' { i } else { next_eol[i + 1] };
        next_double[i] = if i + 1 < n && b[i] == b'`' && b[i + 1] == b'`' {
            i
        } else {
            next_double[i + 1]
        };
    }

    let mut out = vec![MD_LINK_NONE; n];
    for pos in 0..n {
        if b[pos] != b'`' {
            continue;
        }
        if b.get(pos + 1) == Some(&b'`') {
            let start = (pos + 2).min(n);
            let close = next_double[start];
            if close < n {
                out[pos] = close + 2;
            }
        } else {
            let start = (pos + 1).min(n);
            let close = next_backtick[start];
            if close > start && close < next_eol[start] {
                out[pos] = close + 1;
            }
        }
    }
    crate::metrics::scan_work(n);
    out
}

pub(crate) fn md_link_with_scan(
    s: &str,
    at: usize,
    image: bool,
    base: usize,
    scan: &mut MdLinkScan,
) -> Option<(Inline, usize)> {
    parse_md_link(s, at, image, base, scan).map(|l| (l.node, l.end))
}

fn parse_md_link(
    s: &str,
    at: usize,
    image: bool,
    base: usize,
    scan: &mut MdLinkScan,
) -> Option<MdLink> {
    if image {
        if let Some(link) = markdown_embed_image(s, at, base, scan) {
            return Some(link);
        }
    }
    markdown_link(s, at, image, base, scan)
}

/// mldoc `markdown_embed_image` (`syntax/inline.ml:1138-1152`): this branch is
/// first and separate from `markdown_link`, so `data:` payloads do not go through
/// URL-piece parsing or title parsing.
fn markdown_embed_image(s: &str, at: usize, base: usize, scan: &mut MdLinkScan) -> Option<MdLink> {
    let label = label_part(s, at, base, false, scan)?;
    let b = s.as_bytes();
    let data_start = label.url_start;
    if !s[data_start..].starts_with("data:") {
        return None;
    }
    let mut j = data_start + "data:".len();
    if j >= b.len() || b[j] == b')' {
        return None;
    }
    while j < b.len() && b[j] != b')' {
        j += char_len(b[j]);
    }
    if j >= b.len() || b[j] != b')' {
        return None;
    }
    let data = s[data_start..j].to_string();
    crate::metrics::scan_work(data.len());
    let mut end = j + 1;
    let metadata = read_metadata(s, b, &mut end, scan);
    let full = format!("![{}]({}){}", label.label_text, data, metadata);
    Some(MdLink {
        node: Inline::Link {
            url: Url::EmbedData { v: data },
            label: label.label,
            full,
            image: true,
            metadata,
            title: None,
            span: None,
        },
        end,
    })
}

/// mldoc `markdown_link` (`syntax/inline.ml:822-890`).
fn markdown_link(
    s: &str,
    at: usize,
    image: bool,
    base: usize,
    scan: &mut MdLinkScan,
) -> Option<MdLink> {
    let label = label_part(s, at, base, true, scan)?;
    let (url_range, after_url) = link_url_part_range(s, label.url_start, scan)?;
    let parsed_url = link_url_part_inner(s, url_range.clone(), scan);
    let url_text = s[url_range.clone()].to_string();
    crate::metrics::scan_work(url_range.len());
    let mut end = after_url;
    let metadata = read_metadata(s, s.as_bytes(), &mut end, scan);
    let (link_type, url_value, title) = parsed_url.unwrap_or((MdUrlType::Other, url_text.clone(), None));
    let trimmed = url_value.trim();
    let unescaped;
    let url_value = if link_type == MdUrlType::Other {
        unescaped = unescape(trimmed);
        unescaped.as_str()
    } else {
        trimmed
    };
    let url = classify_markdown_url(link_type, url_value);
    let prefix = if image { "!" } else { "" };
    let full = format!("{}[{}]({}){}", prefix, label.label_text, url_text, metadata);
    Some(MdLink {
        node: Inline::Link { url, label: label.label, full, image, metadata, title, span: None },
        end,
    })
}

struct MdLabelPart {
    label: Vec<Inline>,
    label_text: String,
    url_start: usize,
}

/// mldoc `label_part` / `label_part_choices` (`syntax/inline.ml:735-770`).
fn label_part(
    s: &str,
    at: usize,
    base: usize,
    reparse_plain: bool,
    scan: &mut MdLinkScan,
) -> Option<MdLabelPart> {
    let url_start = label_part_url_start(s, at, scan)?;
    let delimiter = url_start.checked_sub(2)?;
    let raw_nodes = materialize_label_part(s, at, delimiter, base, scan)?;
    let label_text = label_text_for_full(&raw_nodes);
    let label = finish_markdown_label(raw_nodes, reparse_plain);
    Some(MdLabelPart { label, label_text, url_start })
}

fn label_part_url_start(s: &str, at: usize, scan: &mut MdLinkScan) -> Option<usize> {
    let b = s.as_bytes();
    let n = b.len();
    if s[at..].starts_with("[](") {
        return Some(at + 3);
    }
    let mut j = at + 1;
    while j < n {
        if s[j..].starts_with("](") {
            return Some(j + 2);
        }
        let c = b[j];
        if let Some(end) = take_while1_include_backslash_len(s, j, b"[]", |c| {
            c != b'\n' && c != b'\r' && !matches!(c, b'`' | b'[' | b']')
        }) {
            j = end;
            continue;
        }
        if c == b'`' {
            if let Some(end) = scan.code_span_end(s, j) {
                j = end;
                continue;
            }
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < n {
            j = j + 1 + char_len(b[j + 1]);
            continue;
        }
        if c == b'\\' {
            j += 1;
            continue;
        }
        if c == b'[' {
            if let Some(end) = scan.page_ref_end(s, j) {
                j = end;
                continue;
            }
            let end = scan.label_bracket_end(s, j);
            if end > j {
                j = end;
                continue;
            }
        }
        if c == b']' || c == b'\n' || c == b'\r' {
            return None;
        }
        j += char_len(c);
    }
    None
}

fn materialize_label_part(
    s: &str,
    at: usize,
    delimiter: usize,
    base: usize,
    scan: &mut MdLinkScan,
) -> Option<Vec<Inline>> {
    let b = s.as_bytes();
    let mut j = at + 1;
    let mut raw_nodes: Vec<Inline> = Vec::new();
    while j < delimiter {
        let c = b[j];
        if let Some(end) = take_while1_include_backslash_len(s, j, b"[]", |c| {
            c != b'\n' && c != b'\r' && !matches!(c, b'`' | b'[' | b']')
        }) {
            let end = end.min(delimiter);
            push_label_plain(&mut raw_nodes, &s[j..end], base + j);
            j = end;
            continue;
        }
        if c == b'`' {
            if let Some(end) = scan.code_span_end(s, j).filter(|&end| end <= delimiter) {
                raw_nodes.push(Inline::Code {
                    text: code_inner(&s[j..end]),
                    span: Some(Span(base + j, base + end)),
                });
                j = end;
                continue;
            }
            push_label_plain(&mut raw_nodes, "`", base + j);
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < b.len() {
            let end = (j + 1 + char_len(b[j + 1])).min(delimiter);
            push_label_plain(&mut raw_nodes, &s[j..end], base + j);
            j = end;
            continue;
        }
        if c == b'\\' {
            push_label_plain(&mut raw_nodes, "\\", base + j);
            j += 1;
            continue;
        }
        if c == b'[' {
            if let Some(end) = scan.page_ref_end(s, j).filter(|&end| end <= delimiter) {
                push_label_plain(&mut raw_nodes, &s[j..end], base + j);
                j = end;
                continue;
            }
            let end = scan.label_bracket_end(s, j);
            if end > j && end <= delimiter {
                push_label_plain(&mut raw_nodes, &s[j..end], base + j);
                j = end;
                continue;
            }
        }
        if c == b']' || c == b'\n' || c == b'\r' {
            return None;
        }
        let end = (j + char_len(c)).min(delimiter);
        push_label_plain(&mut raw_nodes, &s[j..end], base + j);
        j = end;
    }
    (j == delimiter).then_some(raw_nodes)
}

fn finish_markdown_label(nodes: Vec<Inline>, reparse_plain: bool) -> Vec<Inline> {
    if reparse_plain {
        reparse_markdown_label(nodes)
    } else {
        unescape_markdown_label(nodes)
    }
}

fn unescape_markdown_label(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Inline::Plain { text, span, .. } => {
                let value = unescape(&text);
                if value.len() == text.len() {
                    out.push(Inline::Plain {
                        text: value,
                        span,
                        span_map: None,
                    });
                } else if let Some(span) = span {
                    out.push(crate::source_map::make_plain(
                        value,
                        span,
                        unescape_origins(&text, span.0),
                        &text,
                        span.0,
                    ));
                } else {
                    out.push(Inline::Plain {
                        text: value,
                        span: None,
                        span_map: None,
                    });
                }
            }
            other => out.push(other),
        }
    }
    concat_label_plains(out)
}

fn push_label_plain(nodes: &mut Vec<Inline>, raw: &str, abs_start: usize) {
    if raw.is_empty() {
        return;
    }
    crate::metrics::scan_work(raw.len());
    match nodes.last_mut() {
        Some(Inline::Plain { text, span, span_map }) => {
            text.push_str(raw);
            *span = span.map(|Span(start, _)| Span(start, abs_start + raw.len()));
            *span_map = None;
        }
        _ => nodes.push(Inline::Plain {
            text: raw.to_string(),
            span: Some(Span(abs_start, abs_start + raw.len())),
            span_map: None,
        }),
    }
}

fn label_text_for_full(nodes: &[Inline]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Inline::Plain { text, .. } => out.push_str(text),
            Inline::Code { text, .. } => {
                out.push('`');
                out.push_str(text);
                out.push('`');
            }
            _ => {}
        }
    }
    out
}

fn reparse_markdown_label(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Inline::Plain { text, span, .. } => {
                let base = span.map(|Span(start, _)| start).unwrap_or(0);
                if let Some(nodes) = crate::resolver::parse_inline_ctx_md_label(&text, base) {
                    out.extend(nodes);
                } else {
                    let value = unescape(&text);
                    if value.len() == text.len() {
                        out.push(Inline::Plain {
                            text: value,
                            span,
                            span_map: None,
                        });
                    } else if let Some(span) = span {
                        out.push(crate::source_map::make_plain(
                            value,
                            span,
                            unescape_origins(&text, span.0),
                            &text,
                            span.0,
                        ));
                    } else {
                        out.push(Inline::Plain {
                            text: value,
                            span: None,
                            span_map: None,
                        });
                    }
                }
            }
            other => out.push(other),
        }
    }
    concat_label_plains(out)
}

fn concat_label_plains(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for node in nodes {
        match (out.last_mut(), node) {
            (
                Some(Inline::Plain {
                    text: prev,
                    span: prev_span,
                    span_map: prev_map,
                }),
                Inline::Plain {
                    text,
                    span,
                    span_map,
                },
            ) => {
                let shift = prev.len();
                if prev_map.is_some() || span_map.is_some() {
                    let map = prev_map.get_or_insert_with(Vec::new);
                    if map.is_empty() {
                        if let Some(Span(start, end)) = *prev_span {
                            crate::source_map::push_wire_segment(map, 0, start, end - start);
                        }
                    }
                    match span_map {
                        Some(mut segments) => {
                            for seg in &mut segments {
                                seg.0 += shift;
                            }
                            map.extend(segments);
                        }
                        None => {
                            if let Some(Span(start, end)) = span {
                                crate::source_map::push_wire_segment(map, shift, start, end - start);
                            }
                        }
                    }
                }
                prev.push_str(&text);
                *prev_span = match (*prev_span, span) {
                    (Some(Span(start, _)), Some(Span(_, end))) => Some(Span(start, end)),
                    _ => None,
                };
            }
            (_, node) => out.push(node),
        }
    }
    out
}

pub(crate) fn take_while1_include_backslash_len<F>(
    s: &str,
    at: usize,
    chars_can_escape: &[u8],
    mut pred: F,
) -> Option<usize>
where
    F: FnMut(u8) -> bool,
{
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut last_backslash = false;
    let mut examined_stop = false;
    while j < n {
        let c = b[j];
        let take = if last_backslash && chars_can_escape.contains(&c) {
            last_backslash = false;
            true
        } else if last_backslash {
            last_backslash = false;
            pred(c)
        } else if c == b'\\' {
            last_backslash = true;
            true
        } else {
            pred(c)
        };
        if !take {
            examined_stop = true;
            break;
        }
        j += char_len(c);
    }
    crate::metrics::scan_work(j.saturating_sub(at) + usize::from(examined_stop));
    (j > at).then_some(j)
}

fn code_inner(span: &str) -> String {
    if span.starts_with("``") {
        span[2..span.len() - 2].to_string()
    } else {
        span[1..span.len() - 1].to_string()
    }
}

/// mldoc `link_url_part` (`syntax/inline.ml:723-733`).
fn link_url_part_range(
    s: &str,
    at: usize,
    scan: &mut MdLinkScan,
) -> Option<(Range<usize>, usize)> {
    let end = scan.url_paren_end(s, at);
    if s.as_bytes().get(end) == Some(&b')') {
        return Some((at..end, end + 1));
    }
    if end > at && s.as_bytes().get(end - 1) == Some(&b')') {
        return Some((at..end - 1, end));
    }
    None
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MdUrlType {
    BlockRef,
    PageRef,
    Other,
    Other1,
    Other2,
}

/// mldoc `link_url_part_inner` (`syntax/inline.ml:772-813`).
fn link_url_part_inner(
    s: &str,
    url_range: Range<usize>,
    scan: &mut MdLinkScan,
) -> Option<(MdUrlType, String, Option<String>)> {
    let b = s.as_bytes();
    let n = url_range.end;
    let mut j = url_range.start;
    let mut parts: Vec<(MdUrlType, String)> = Vec::new();
    while j < n {
        if let Some((kind, value, end)) = url_part_piece(s, j, n, scan.page_ref_scan()) {
            parts.push((kind, value));
            j = end;
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let (kind, value) = if parts.len() == 1 {
        let (kind, value) = parts.pop().unwrap();
        match kind {
            MdUrlType::Other1 | MdUrlType::Other2 => (MdUrlType::Other, value),
            _ => (kind, value),
        }
    } else {
        if parts.iter().any(|(kind, _)| *kind == MdUrlType::Other1) {
            return None;
        }
        (MdUrlType::Other, parts.into_iter().map(|(_, value)| value).collect())
    };
    while j < n && matches!(b[j], b' ' | b'\t' | 0x16 | 0x0c) {
        j += 1;
    }
    let title = if j >= n {
        None
    } else if b[j] == b'"' {
        let start = j + 1;
        let end = take_while1_include_backslash_len(&s[..n], start, b"\"", |c| c != b'"')?;
        if end >= n || b[end] != b'"' {
            return None;
        }
        j = end + 1;
        if j != n {
            return None;
        }
        Some(s[start..end].to_string())
    } else {
        return None;
    };
    Some((kind, value, title))
}

fn url_part_piece(
    s: &str,
    at: usize,
    limit: usize,
    scan: &mut PageRefScan,
) -> Option<(MdUrlType, String, usize)> {
    let b = s.as_bytes();
    if at >= limit {
        return None;
    }
    if s[at..].starts_with("((") {
        let mut j = at + 2;
        while j < limit && b[j] != b')' {
            j += char_len(b[j]);
        }
        if j > at + 2 && j + 1 < limit && b[j] == b')' && b[j + 1] == b')' {
            return Some((MdUrlType::BlockRef, s[at..j + 2].to_string(), j + 2));
        }
    }
    if b[at] == b'<' {
        let start = at + 1;
        let end = take_while1_include_backslash_len(&s[..limit], start, b"<>", |c| {
            c != b'<' && c != b'>'
        })?;
        if end < limit && b[end] == b'>' {
            return Some((MdUrlType::Other1, s[start..end].to_string(), end + 1));
        }
    }
    if b[at] != b'[' && !is_ws_or_nl(b[at]) {
        let mut j = at;
        while j < limit && !is_ws_or_nl(b[j]) && b[j] != b'[' {
            j += char_len(b[j]);
        }
        if j > at {
            return Some((MdUrlType::Other2, s[at..j].to_string(), j));
        }
    }
    if s[at..].starts_with("[[") {
        if let Some(end) = parse_page_ref_end_with_scan(s, at, scan).filter(|&end| end <= limit) {
            let full = s[at..end].to_string();
            return Some((MdUrlType::PageRef, full, end));
        }
    }
    if b[at] == b' ' {
        return None;
    }
    let w = char_len(b[at]);
    Some((MdUrlType::Other2, s[at..at + w].to_string(), at + w))
}

fn classify_markdown_url(link_type: MdUrlType, url: &str) -> Url {
    match link_type {
        MdUrlType::BlockRef => Url::BlockRef { v: url[2..url.len().saturating_sub(2)].to_string() },
        MdUrlType::PageRef => Url::PageRef { v: url[2..url.len().saturating_sub(2)].to_string() },
        MdUrlType::Other => {
            if let Some(idx) = url.find(':') {
                let protocol = &url[..idx];
                if !protocol.is_empty() && url[idx..].starts_with("://") {
                    let mut link = &url[idx + 3..];
                    if let Some(stripped) = link.strip_prefix("//") {
                        link = stripped;
                    }
                    return Url::Complex {
                        protocol: Some(protocol.to_string()),
                        link: Some(link.to_string()),
                    };
                }
            }
            let lower = url.to_ascii_lowercase();
            if url.len() > 3 && (lower.ends_with(".md") || lower.ends_with(".markdown")) {
                Url::File { v: url.to_string() }
            } else {
                Url::Search { v: url.to_string() }
            }
        }
        MdUrlType::Other1 | MdUrlType::Other2 => unreachable!("normalized before classification"),
    }
}

/// Shared mldoc `metadata` (`syntax/inline.ml:562-566`).
fn read_metadata(s: &str, b: &[u8], end: &mut usize, scan: &mut MdLinkScan) -> String {
    if b.get(*end) == Some(&b'{') {
        if let Some(close) = scan.metadata_close(b, *end + 1) {
            let meta = s[*end..close + 1].to_string();
            *end = close + 1;
            return meta;
        }
    }
    String::new()
}

// ---- autolink / email / inline html ---------------------------------------

/// mldoc `quick_link`: `<protocol:optional//link>`, where `link` is nonempty
/// and stops before whitespace or `>`. Returns (end, node).
pub(crate) fn parse_quick_link(s: &str, at: usize) -> Option<(usize, Inline)> {
    parse_quick_link_with_mode(s, at, false, 0)
}

/// Markdown quick links in the compiled mldoc 1.5.7 artifact unescape the
/// synthetic label and url link, while published `quick_link_aux` cannot produce
/// label != full_text. The npm oracle is the compatibility target here.
pub(crate) fn parse_quick_link_md(s: &str, at: usize, base: usize) -> Option<(usize, Inline)> {
    parse_quick_link_with_mode(s, at, true, base)
}

fn parse_quick_link_with_mode(
    s: &str,
    at: usize,
    md_unescape: bool,
    base: usize,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    // protocol = letters/digits, then ':'
    let mut j = at + 1;
    let p0 = j;
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
    if j == p0 || j >= n || b[j] != b':' {
        return None;
    }
    let protocol = s[p0..j].to_string();
    j += 1; // past ':'
    let mut slashes = String::new();
    if s[j..].starts_with("//") {
        slashes = "//".to_string();
        j += 2;
    }
    let link_start = j;
    let mut scanned = 0usize;
    while j < n && !is_ws_or_nl(b[j]) && b[j] != b'>' {
        scanned += 1;
        j += char_len(b[j]);
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j >= n || b[j] != b'>' || j == link_start {
        return None;
    }
    let raw_link = &s[link_start..j];
    let link = if md_unescape {
        unescape(raw_link)
    } else {
        raw_link.to_string()
    };
    let full = format!("{}:{}{}", protocol, slashes, raw_link);
    let raw_label = &full;
    let label = if md_unescape {
        unescape(&full)
    } else {
        full.clone()
    };
    let label_node = if label.as_bytes() == raw_label.as_bytes() {
        Inline::Plain {
            text: label,
            span: Some(Span(base + at + 1, base + j)),
            span_map: None,
        }
    } else {
        crate::source_map::make_plain(
            label,
            Span(base + at + 1, base + j),
            unescape_origins(raw_label, base + at + 1),
            raw_label,
            base + at + 1,
        )
    };
    let node = Inline::Link {
        url: Url::Complex {
            protocol: Some(protocol),
            link: Some(link),
        },
        label: vec![label_node],
        full,
        image: false,
        metadata: String::new(),
        title: None,
        span: None,
    };
    Some((j + 1, node))
}

/// Dispatch-owner guard for `<scheme:...>`: the unbounded part of autolink parsing is
/// the search for the first `>`/whitespace boundary after the scheme. A single monotone
/// boundary cursor owns that scan; EOF is a suffix-absence miss and whitespace-before-`>`
/// is an invalidating token until the dispatch cursor passes it.
pub(crate) fn autolink_has_closing_boundary(s: &str, at: usize, scan: &mut AutolinkScan) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return false;
    }
    let mut j = at + 1;
    let p0 = j;
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
    if j == p0 || j >= n || b[j] != b':' {
        return false;
    }
    j += 1;
    if s[j..].starts_with("//") {
        j += 2;
    }
    let link_start = j;
    let boundary = scan.boundary.first_from(b, link_start);
    boundary < n && b[boundary] == b'>' && boundary > link_start
}

/// mldoc `email_address.email`, dispatched only from the `<` arm by the resolvers:
/// optional `<`, address, optional `>`. On success without `>`, only the address is
/// consumed and the suffix remains plain.
pub(crate) fn parse_email_autolink_cached(
    s: &str,
    at: usize,
    scan: &mut EmailAutolinkScan,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    if b.get(j) == Some(&b'<') {
        j += 1;
    }
    let local_start = j;
    if local_start >= scan.no_at_from {
        return None;
    }
    let mut scanned = 0usize;
    while j < n && !email_local_forbidden(b[j]) {
        scanned += 1;
        j += char_len(b[j]);
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j >= n {
        scan.no_at_from = scan.no_at_from.min(local_start);
        return None;
    }
    if b[j] != b'@' || j == local_start {
        return None;
    }
    let local = s[local_start..j].to_string();
    j += 1;
    let dom_start = j;
    let boundary = scan.domain_boundary.first_from(b, dom_start);
    if boundary == dom_start {
        return None;
    }
    let domain = s[dom_start..boundary].to_string();
    let val = serde_json::json!({ "local_part": local, "domain": domain });
    let end = if boundary < n && b[boundary] == b'>' {
        boundary + 1
    } else {
        boundary
    };
    Some((end, Inline::Email { text: val, span: None }))
}

/// mldoc `statistics_cookie`: `[digits/slashes/percents]`, then Scanf-prefix
/// parsing as either `%d/%d` or `%d%%`.
pub(crate) fn parse_statistics_cookie(s: &str, at: usize) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    if b.get(at) != Some(&b'[') {
        return None;
    }
    let mut j = at + 1;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'/' || b[j] == b'%') {
        j += 1;
    }
    if j == at + 1 || b.get(j) != Some(&b']') {
        return None;
    }
    let body = &s[at + 1..j];
    if let Some((value, total)) = scan_absolute_cookie(body) {
        return Some((
            j + 1,
            Inline::Cookie {
                kind: "Absolute".to_string(),
                value,
                total: Some(total),
                span: None,
            },
        ));
    }
    if let Some(value) = scan_percent_cookie(body) {
        return Some((
            j + 1,
            Inline::Cookie {
                kind: "Percent".to_string(),
                value,
                total: None,
                span: None,
            },
        ));
    }
    None
}

fn scan_cookie_int(body: &str, mut i: usize) -> Option<(i64, usize)> {
    let start = i;
    let b = body.as_bytes();
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    body[start..i].parse::<i64>().ok().map(|n| (n, i))
}

fn scan_absolute_cookie(body: &str) -> Option<(i64, i64)> {
    let (value, i) = scan_cookie_int(body, 0)?;
    if body.as_bytes().get(i) != Some(&b'/') {
        return None;
    }
    let (total, _) = scan_cookie_int(body, i + 1)?;
    Some((value, total))
}

fn scan_percent_cookie(body: &str) -> Option<i64> {
    let (value, i) = scan_cookie_int(body, 0)?;
    (body.as_bytes().get(i) == Some(&b'%')).then_some(value)
}

/// Block-level LaTeX environment `\begin{NAME} … \end{NAME}` (mldoc `latex_env.ml`,
/// shared by the Markdown and Org block segmenters). The opener must be at the start
/// of the line at `line_start` after optional leading mldoc spaces (`spaces *>`); text
/// before `\begin` disqualifies it. mldoc grammar:
///   `spaces *> "\begin{" *> take_while1(≠'}') <* '}' <* spaces_or_eols`,
///   content = all chars until a case-insensitive `\end{NAME}` (or EOF); the node
///   name is lowercased. `line_end` is the byte offset of the opener line's content
///   end (the `\n` or EOF) — the name is taken within that line (realistic envs name
///   the environment on the begin line). Returns `(name, content, consumed_end)`
///   where `consumed_end` is the byte offset just past `\end{NAME}` (or EOF).
pub(crate) fn parse_latex_env(
    input: &str,
    line_start: usize,
    line_end: usize,
) -> Option<(String, String, usize)> {
    let b = input.as_bytes();
    let mut p = line_start;
    while p < line_end && crate::block_common::mldoc_is_space(b[p]) {
        p += 1;
    }
    if !input[p..].starts_with("\\begin{") {
        return None;
    }
    let name_start = p + 7; // past "\begin{"
    let mut j = name_start;
    while j < line_end && b[j] != b'}' {
        j += 1;
    }
    if j >= line_end || b[j] != b'}' || j == name_start {
        return None;
    }
    let name = &input[name_start..j];
    // spaces_or_eols after `\begin{NAME}` (mldoc spaces plus CR/LF).
    let mut cs = j + 1;
    while cs < input.len()
        && (crate::block_common::mldoc_is_space(b[cs]) || matches!(b[cs], b'\n' | b'\r'))
    {
        cs += 1;
    }
    let ending = format!("\\end{{{}}}", name);
    match find_ci(input, cs, &ending) {
        Some(e) => Some((name.to_ascii_lowercase(), input[cs..e].to_string(), e + ending.len())),
        None => Some((name.to_ascii_lowercase(), input[cs..].to_string(), input.len())),
    }
}

pub(crate) fn find_ci(s: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = s.as_bytes();
    let nb = needle.as_bytes();
    let n = hay.len();
    if nb.is_empty() || from > n {
        return None;
    }
    let mut i = from;
    while i + nb.len() <= n {
        if hay[i..i + nb.len()]
            .iter()
            .zip(nb.iter())
            .all(|(a, c)| a.eq_ignore_ascii_case(c))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---- bare urls ------------------------------------------------------------

pub(crate) fn parse_bare_url_with_scan(
    s: &str,
    at: usize,
    scan: &mut BareUrlScan,
    base: usize,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if at >= scan.no_scheme_from {
        return None;
    }
    // protocol
    let mut j = at;
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
    if j == at || !s[j..].starts_with("://") {
        if j >= n {
            scan.no_scheme_from = scan.no_scheme_from.min(at);
        }
        return None;
    }
    let protocol = s[at..j].to_string();
    j += 3; // past "://"
    let path_start = j;
    // before_path: until space/eol / '/' / '?' / '#' / inline_link_delims ([]<>{}()).
    // Trailing punctuation is NOT excluded here; mldoc only applies that rule to
    // the optional `/ ? #` tail.
    let mut path_scanned = 0usize;
    while j < n {
        let c = b[j];
        if is_tag_url_space_or_eol(c)
            || c == b'/'
            || c == b'?'
            || c == b'#'
            || matches!(c, b'[' | b']' | b'<' | b'>' | b'{' | b'}' | b'(' | b')')
        {
            break;
        }
        path_scanned += 1;
        j += char_len(c);
    }
    if j < n {
        path_scanned += 1;
    }
    crate::metrics::scan_work(path_scanned);
    let before_path_end = j;
    if before_path_end == path_start {
        return None; // before_path is take_while1 in mldoc: must be non-empty
    }
    // remaining_part: mldoc consumes the optional '/' | '?' | '#' opener before
    // entering the balanced-bracket scan. That opener is always kept; the
    // trailing ,;.!? exclusion applies only to bytes after it.
    let mut remain_end = before_path_end;
    if j < n && matches!(b[j], b'/' | b'?' | b'#') {
        remain_end = read_url_balanced(s, j + 1);
    }
    let end = remain_end;
    let raw = &s[path_start..end];
    let link = unescape(raw);
    let full = format!("{}://{}", protocol, raw);
    let label_text = format!("{}://{}", protocol, link);
    let label = if label_text.as_bytes() == full.as_bytes() {
        Inline::Plain {
            text: label_text,
            span: Some(Span(base + at, base + end)),
            span_map: None,
        }
    } else {
        crate::source_map::make_plain(
            label_text,
            Span(base + at, base + end),
            unescape_origins(&full, base + at),
            &full,
            base + at,
        )
    };
    let node = Inline::Link {
        url: Url::Complex {
            protocol: Some(protocol),
            link: Some(link),
        },
        label: vec![label],
        full,
        image: false,
        metadata: String::new(),
        title: None,
        span: None,
    };
    Some((end, node))
}

/// Read the remaining URL path after the already-consumed `/`, `?`, or `#`
/// opener (mldoc `string_contains_balanced_brackets`): balances `()` and `[]`,
/// stops at whitespace or an unmatched `)`/`]`, and excludes a trailing
/// `, ; . ! ?` that precedes whitespace or end-of-input. Does NOT stop at
/// `< > { }` (mldoc keeps those in the tail).
fn read_url_balanced(s: &str, at: usize) -> usize {
    string_contains_balanced_brackets_multi_end(
        s,
        at,
        &[(b'(', b')'), (b'[', b']')],
        b" \t\r\n\x0c\x1a",
        b",;.!?",
    )
}

fn string_contains_balanced_brackets_multi_end(
    s: &str,
    at: usize,
    bracket_pairs: &[(u8, u8)],
    other_delims: &[u8],
    excluded_ending_chars: &[u8],
) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut stack: Vec<u8> = Vec::new();
    let mut scanned = 0usize;
    while j < n {
        let c = b[j];
        scanned += 1;
        if other_delims.contains(&c) {
            stack.clear();
            break;
        }
        if let Some(&expected) = stack.last() {
            if c == expected {
                stack.pop();
                j += 1;
                continue;
            }
        }
        if bracket_pairs.iter().any(|&(_, right)| c == right) {
            if stack.pop().is_some() {
                continue;
            } else {
                break;
            }
        }
        if excluded_ending_chars.contains(&c) {
            if j + 1 >= n || other_delims.contains(&b[j + 1]) {
                stack.clear();
                break;
            }
            j += 1;
            continue;
        }
        if let Some(&(_, right)) = bracket_pairs.iter().find(|&&(left, _)| left == c) {
            stack.push(right);
            j += 1;
            continue;
        }

        let mut plain_end = j;
        while plain_end < n {
            let pc = b[plain_end];
            if other_delims.contains(&pc)
                || excluded_ending_chars.contains(&pc)
                || bracket_pairs.iter().any(|&(left, right)| pc == left || pc == right)
            {
                break;
            }
            plain_end += char_len(pc);
        }
        if plain_end == j {
            break;
        }
        scanned += plain_end - j - 1;
        j = plain_end;
    }
    while !stack.is_empty() {
        stack.pop();
    }
    crate::metrics::scan_work(scanned);
    j
}

/// Remove a backslash that escapes an ASCII-punctuation char (mldoc unescapes such
/// sequences in extracted string *values* — ref names, tag text, url links — while
/// leaving `full_text` raw). `\<punct>` → `<punct>`, `\\` → `\`; other `\` kept.
pub(crate) fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if b[i] == b'\\' && i + 1 < n && b[i + 1].is_ascii_punctuation() {
            out.push(b[i + 1] as char);
            i += 2;
        } else {
            let w = char_len(b[i]);
            out.push_str(&s[i..i + w]);
            i += w;
        }
    }
    out
}

fn unescape_origins(raw: &str, base: usize) -> Vec<OriginSegment> {
    let b = raw.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut text_off = 0usize;
    let mut i = 0usize;
    while i < n {
        if b[i] == b'\\' && i + 1 < n && b[i + 1].is_ascii_punctuation() {
            out.push(OriginSegment::new(text_off, base + i + 1, 1, 1));
            text_off += 1;
            i += 2;
        } else {
            let w = char_len(b[i]);
            out.push(OriginSegment::new(text_off, base + i, w, w));
            text_off += w;
            i += w;
        }
    }
    out
}

// ---- hiccup ---------------------------------------------------------------

/// mldoc's Clojure-hiccup HTML-element allowlist (110 names, lowercase, **byte-sorted**
/// for binary search). A `[:name …]` vector is a `Hiccup` (block) / `Inline_Hiccup`
/// (inline) iff `name` (case-insensitively) is one of these. Derived from mldoc 1.5.7's
/// source tag set (`Qz`) and cross-checked against the live oracle (every name in / every
/// HTML5 element not listed out). Keep sorted — `is_hiccup_tag` binary-searches it.
pub(crate) static HICCUP_TAGS: &[&str] = &[
    "a", "abbr", "address", "area", "article", "aside", "audio", "b", "base", "bdi", "bdo",
    "blockquote", "body", "br", "button", "canvas", "caption", "cite", "code", "col", "colgroup", "data",
    "datalist", "dd", "del", "details", "dfn", "div", "dl", "dt", "em", "embed", "fieldset",
    "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "head",
    "header", "hr", "html", "i", "iframe", "img", "input", "ins", "kbd", "keygen", "label",
    "legend", "li", "link", "main", "map", "mark", "meta", "meter", "nav", "noscript", "object",
    "ol", "optgroup", "option", "output", "p", "param", "pre", "progress", "q", "rb", "rp",
    "rt", "rtc", "ruby", "s", "samp", "script", "section", "select", "small", "source", "span",
    "strong", "style", "sub", "summary", "sup", "table", "tbody", "td", "template", "textarea", "tfoot",
    "th", "thead", "time", "title", "tr", "track", "u", "ul", "var", "video", "wbr",
];

/// Case-insensitive membership test against `HICCUP_TAGS`. `name` is ASCII alphanumeric
/// (the only thing the caller passes), so a lowercased fixed buffer + binary search is
/// allocation-free. The longest allowed tag is 10 bytes (`blockquote`/`figcaption`).
pub(crate) fn known_html_tag_index(name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    let mut buf = [0u8; 10];
    for (k, &c) in bytes.iter().enumerate() {
        buf[k] = c.to_ascii_lowercase();
    }
    let lower = &buf[..bytes.len()];
    HICCUP_TAGS.binary_search_by(|t| t.as_bytes().cmp(lower)).ok()
}

/// Case-insensitive membership test against the shared mldoc HTML-element allowlist.
pub(crate) fn is_known_html_tag(name: &str) -> bool {
    known_html_tag_index(name).is_some()
}

fn is_hiccup_tag(name: &str) -> bool {
    is_known_html_tag(name)
}

/// Recognize + capture a Clojure-hiccup vector `[:tag …]` starting at byte `at` (which
/// must be the `[`). Returns the byte index just past the matching `]` (so the captured
/// raw text is `s[at..end]`), or `None` if it isn't a hiccup. Rules verified vs mldoc
/// 1.5.7:
///   1. `[:` then a non-empty `[A-Za-z0-9]+` element name whose lowercase ∈ `HICCUP_TAGS`;
///   2. the char immediately after the name is the keyword boundary — one of
///      `]`, space, tab, `.`, `#` (a CSS-selector start / separator). Anything else
///      (`{ [ " ( , : / -` … or a newline) → NOT a hiccup;
///   3. a string-aware, `[:`-nested balanced scan to the matching `]`: depth starts at 1
///      on the outer `[`, a NESTED `[:` opens a level (+1), a `]` closes (−1, end at 0),
///      a `]` inside a `"…"` string is ignored, but `[:` inside the string still opens
///      a level. A `"` toggles the string state unless the previous byte is `\`
///      (naive one-byte look-back, not escape pairing). A lone `[` (not `[:`) and any
///      `{ }` are literal (NOT balanced). Reaching EOF unbalanced — including an
///      unterminated string — → `None`.
/// Linear in the captured length.
/// Steps (1)+(2) of hiccup recognition — element-name allowlist + keyword boundary —
/// in O(1)+name, with NO balanced scan. Split out so the inline scanner can pair
/// `[:`…`]` once up front (`build_hiccup_close`) and then validate each opener's head
/// against the precomputed close, instead of re-scanning to the closer per opener.
pub(crate) fn hiccup_head_ok(s: &str, at: usize) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'[') || b.get(at + 1) != Some(&b':') {
        return false;
    }
    // (1) element name = maximal [A-Za-z0-9]+ after `[:`, lowercase ∈ HICCUP_TAGS.
    let name_start = at + 2;
    let mut j = name_start;
    while j < n && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == name_start || !is_hiccup_tag(&s[name_start..j]) {
        return false;
    }
    // (2) keyword boundary: `]`, mldoc `is_space`, `.` or `#` (CSS-selector start / end).
    matches!(b.get(j), Some(b']') | Some(b'.') | Some(b'#'))
        || b.get(j).is_some_and(|&c| crate::block_common::mldoc_is_space(c))
}

/// Pair EVERY `[:`…`]` hiccup vector in `s` in one linear pass (a delimiter stack:
/// a `[:` always pushes, a `]` pops only outside a `"…"` string, and `"` toggles
/// string state unless the previous byte is `\`). Returns a
/// position-indexed `Vec` (length `|s|`): `close[opener-`[`-byte]` = index-just-past-the-
/// matching-`]`, and `usize::MAX` where no hiccup vector opens (NOT a `HashMap<usize,_>` —
/// the key is a byte position, a perfect array index; see lsdoc/CLAUDE.md). This is the
/// structural replacement for the per-opener balanced re-scan (and the `rbracket` absence
/// cache): the inline dispatch does an O(1) array lookup instead. The balance counts every
/// `[:` regardless of tag validity (tag-validity is applied separately at lookup via
/// [`hiccup_head_ok`]) — matching mldoc's depth scan exactly.
pub(crate) fn build_hiccup_close(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut close = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut p = 0;
    while p < n {
        match b[p] {
            b'[' if p + 1 < n && b[p + 1] == b':' => {
                stack.push(p);
                p += 2;
            }
            b']' if !in_string => {
                if let Some(o) = stack.pop() {
                    close[o] = p + 1;
                }
                p += 1;
            }
            b'"' => {
                if p == 0 || b[p - 1] != b'\\' {
                    in_string = !in_string;
                }
                p += 1;
            }
            c => p += char_len(c),
        }
    }
    close
}

/// Pair `[[`…`]]` the way `match_brackets` (nested-link) balances them — a delimiter
/// stack: `[[` pushes, `]]` pops, a `\n` clears the stack (mldoc returns `None` across a
/// newline). Returns a position-indexed `Vec` (length `|s|`): `close[opener-`[[`-byte]` =
/// index-just-past-the-matching-`]]`, `usize::MAX` where none (a byte position is a perfect
/// array index, not a `HashMap` key). Escape-FREE, because `match_brackets` does not treat
/// `\` specially. Gating nested-link on this means an unbalanced `[[`-run can't trigger a
/// to-EOF level-scan per opener.
pub(crate) fn build_nested_close(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut close = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut p = 0;
    while p < n {
        match b[p] {
            b'\n' => {
                stack.clear();
                p += 1;
            }
            b'[' if p + 1 < n && b[p + 1] == b'[' => {
                stack.push(p);
                p += 2;
            }
            b']' if p + 1 < n && b[p + 1] == b']' => {
                if let Some(o) = stack.pop() {
                    close[o] = p + 2;
                }
                p += 2;
            }
            c => p += char_len(c),
        }
    }
    close
}

/// Sorted positions of escape-aware "real" `]]` — a `]` reached at an UNescaped position
/// whose next byte is also `]`. These are the closers `parse_page_ref` actually breaks on
/// (a `\` consumes the next byte, so `\]]` has no real `]]`; a single `]` is content). One
/// left-to-right pass tracking escape (globally consistent because the scanner dispatches
/// `[[` only at unescaped positions, matching the per-opener name scan's fresh start).
/// Page-ref then closes at the next real `]]` in O(1) amortized via a monotone cursor,
/// never fail-scanning to the eol per opener.
pub(crate) fn build_real_dbl(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut p = 0;
    while p < n {
        let c = b[p];
        if c == b'\\' && p + 1 < n {
            p += 2; // `\` escapes the next byte (both consumed), as in parse_page_ref
        } else if c == b']' && p + 1 < n && b[p + 1] == b']' {
            out.push(p);
            p += 1; // a later opener could still see `]]` starting one byte on (`]]]`)
        } else {
            p += char_len(c);
        }
    }
    out
}

// ---- resolver (v0.2) leaf entry points ------------------------------------
// Free fns returning (node, end) that the resolvers call at each dispatch point; they do no
// no-closer caching of their own — the resolver's `fresh` flag keeps no-closer runs linear
// (only the first opener of a run scans).

/// `\( … \)` (Inline) / `\[ … \]` (Displayed) latex span.
pub(crate) fn parse_latex_backslash_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let (close, mode): (&str, &str) = match b.get(at + 1).copied()? {
        b'(' => ("\\)", "Inline"),
        b'[' => ("\\]", "Displayed"),
        _ => return None,
    };
    let body_start = at + 2;
    let end = find_sub(b, body_start, close.as_bytes())?;
    Some((
        Inline::Latex { mode: mode.to_string(), body: s[body_start..end].to_string(), span: None },
        end + 2,
    ))
}

/// `$$ … $$` (Displayed) / `$ … $` (Inline) latex span.
///
/// mldoc `lib/syntax/inline.ml:534-541`: after `$$`, displayed math is
/// `take_while (c <> '$' && c <> '\r' && c <> '\n') <* string "$$"`.
/// A lone `$` in the body fails the displayed arm; there is no `\$` escape.
///
/// mldoc's inline `$...$` grammar reads the first body byte separately: only an
/// immediate ASCII space is rejected at the start, and the end reject checks the
/// tail after that first byte. Thus `$($`, `$[$`, `${$`, and `$\n$` are valid,
/// while `$x($`, `$x[$`, `$x{$`, and `$x $` are not.
pub(crate) fn parse_latex_dollar_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let after = *b.get(at + 1)?;
    if after == b'$' {
        let body_start = at + 2;
        let end = latex_display_body_end(b, body_start)?;
        return Some((
            Inline::Latex { mode: "Displayed".to_string(), body: s[body_start..end].to_string(), span: None },
            end + 2,
        ));
    }
    if after == b' ' {
        return None;
    }
    let tail_start = at + 2;
    let mut j = tail_start;
    while j < n && b[j] != b'$' && b[j] != b'\n' && b[j] != b'\r' {
        j += 1;
    }
    if j >= n || b[j] != b'$' {
        return None;
    }
    if j > tail_start && matches!(b[j - 1], b' ' | b'(' | b'[' | b'{') {
        return None;
    }
    Some((
        Inline::Latex { mode: "Inline".to_string(), body: s[at + 1..j].to_string(), span: None },
        j + 1,
    ))
}

fn latex_display_body_end(b: &[u8], body_start: usize) -> Option<usize> {
    let mut j = body_start;
    let mut scanned = 0usize;
    while j < b.len() {
        scanned += 1;
        match b[j] {
            b'$' => {
                crate::metrics::scan_work(scanned);
                return (j + 1 < b.len() && b[j + 1] == b'$').then_some(j);
            }
            b'\n' | b'\r' => {
                crate::metrics::scan_work(scanned);
                return None;
            }
            _ => j += 1,
        }
    }
    crate::metrics::scan_work(scanned);
    None
}

/// `(( … ))` block ref (inner has no `)`; value and `full` are raw).
pub(crate) fn parse_block_ref_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("((") {
        return None;
    }
    let inner_start = at + 2;
    let mut j = inner_start;
    let mut scanned = 0usize;
    while j < n && b[j] != b')' {
        scanned += 1;
        j += 1;
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j == inner_start || j + 1 >= n || b[j] != b')' || b[j + 1] != b')' {
        return None;
    }
    Some((
        Inline::Link {
            url: Url::BlockRef {
                v: s[inner_start..j].to_string(),
            },
            label: vec![],
            full: s[at..j + 2].to_string(),
            image: false,
            metadata: String::new(),
            title: None,
            span: None,
        },
        j + 2,
    ))
}

/// `{{{ … }}}` / `{{ … }}` macro (triple tried first); args raw via `parse_macro`.
pub(crate) fn parse_macro_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    if !s[at..].starts_with("{{") {
        return None;
    }
    let b = s.as_bytes();
    let n = b.len();
    let candidates: &[(&str, &str)] = if s[at..].starts_with("{{{") {
        &[("{{{", "}}}"), ("{{", "}}")]
    } else {
        &[("{{", "}}")]
    };
    for &(open, close) in candidates {
        let inner_start = at + open.len();
        let mut j = inner_start;
        let mut scanned = 0usize;
        while j < n && b[j] != b'}' && b[j] != b'\n' && b[j] != b'\r' {
            scanned += 1;
            j += 1;
        }
        if j < n {
            scanned += 1;
        }
        crate::metrics::scan_work(scanned);
        if j == inner_start || !s[j..].starts_with(close) {
            continue;
        }
        if let Some((name, args)) = parse_macro(&s[inner_start..j]) {
            return Some((Inline::Macro { name, args, span: None }, j + close.len()));
        }
    }
    None
}

// ---- export snippet -------------------------------------------------------

#[inline]
fn is_export_name_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0c | 0x1a)
}

/// `@@name: content@@` export snippet. Mirrors mldoc `Export_Snippet`:
/// nonempty name excluding space/EOL/`:`, literal `": "`, nonempty content
/// excluding every `@`/EOL, then `@@`.
pub(crate) fn parse_export_snippet_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    if !s[at..].starts_with("@@") {
        return None;
    }
    let b = s.as_bytes();
    let n = b.len();
    let name_start = at + 2;
    let mut j = name_start;
    let mut scanned = 0usize;
    while j < n
        && b[j] != b':'
        && b[j] != b'\n'
        && b[j] != b'\r'
        && !is_export_name_space(b[j])
    {
        scanned += 1;
        j += char_len(b[j]);
    }
    if j < n {
        scanned += 1;
    }
    if j == name_start || j + 1 >= n || b[j] != b':' || b[j + 1] != b' ' {
        crate::metrics::scan_work(scanned);
        return None;
    }
    let content_start = j + 2;
    let mut k = content_start;
    while k < n && b[k] != b'@' && b[k] != b'\n' && b[k] != b'\r' {
        scanned += 1;
        k += char_len(b[k]);
    }
    if k < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if k == content_start || k + 1 >= n || b[k] != b'@' || b[k + 1] != b'@' {
        return None;
    }
    Some((
        Inline::ExportSnippet {
            name: s[name_start..j].to_string(),
            content: s[content_start..k].to_string(),
            span: None,
        },
        k + 2,
    ))
}

// ---- timestamps -----------------------------------------------------------

pub(crate) fn parse_angle_timestamp_with_scan(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, Inline)> {
    if s.as_bytes().get(at) != Some(&b'<') {
        return None;
    }
    parse_timestamp_at_with_scan(s, at, scan)
}

/// Text-arm timestamp dispatch for S/C/D/s/c/d. This must try `range` before
/// keyword forms so `SCHEDULED: <a>--<b>` becomes a plain Range, while a missing
/// second half backtracks to the first keyword timestamp.
pub(crate) fn parse_keyword_timestamp_with_scan(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, Inline)> {
    if !matches!(s.as_bytes().get(at), Some(b'S' | b'C' | b'D' | b's' | b'c' | b'd')) {
        return None;
    }
    parse_timestamp_at_with_scan(s, at, scan)
}

/// `[...]` timestamp dispatch, used by both Markdown and Org bracket arms after
/// their link/reference alternatives have failed.
pub(crate) fn parse_bracket_timestamp_with_scan(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, Inline)> {
    if s.as_bytes().get(at) != Some(&b'[') {
        return None;
    }
    parse_timestamp_at_with_scan(s, at, scan)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimestampKind {
    Date,
    Scheduled,
    Deadline,
    Closed,
    Clock,
}

impl TimestampKind {
    fn label(self) -> &'static str {
        match self {
            TimestampKind::Date => "Date",
            TimestampKind::Scheduled => "Scheduled",
            TimestampKind::Deadline => "Deadline",
            TimestampKind::Closed => "Closed",
            TimestampKind::Clock => "Clock",
        }
    }
}

fn parse_timestamp_at_with_scan(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, Inline)> {
    if let Some(range) = parse_range_at(s, at, scan) {
        return Some(range);
    }
    let (end, kind, point) = parse_general_timestamp_at(s, at, scan)?;
    Some((end, timestamp_node(kind, point)))
}

fn parse_range_at(s: &str, at: usize, scan: &mut TimestampCloseScan) -> Option<(usize, Inline)> {
    let mut range_scan = scan.clone();
    let b = s.as_bytes();
    let mut i = skip_mldoc_spaces(b, at);
    let prefix_start = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let clock = if i > prefix_start && b.get(i) == Some(&b':') {
        let prefix = &s[prefix_start..i];
        i += 1;
        Some(prefix == "CLOCK")
    } else {
        i = prefix_start;
        None
    };
    i = skip_mldoc_spaces(b, i);

    let (end1, _kind1, start) = parse_general_timestamp_at(s, i, &mut range_scan)?;
    if !s.get(end1..).is_some_and(|rest| rest.starts_with("--")) {
        return None;
    }
    let (end2, _kind2, stop) = parse_general_timestamp_at(s, end1 + 2, &mut range_scan)?;
    *scan = range_scan;
    if clock == Some(true) {
        Some((end2, Inline::Timestamp {
            ts: "Clock".to_string(),
            date: serde_json::json!(["Stopped", { "start": start, "stop": stop }]),
            span: None,
        }))
    } else {
        Some((end2, Inline::Timestamp {
            ts: "Range".to_string(),
            date: serde_json::json!({ "start": start, "stop": stop }),
            span: None,
        }))
    }
}

fn parse_general_timestamp_at(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, TimestampKind, serde_json::Value)> {
    let b = s.as_bytes();
    let i = skip_mldoc_spaces(b, at);
    let c = *b.get(i)?;
    match c.to_ascii_uppercase() {
        b'<' => parse_date_time_at(s, i, b'<', b'>', true, scan)
            .map(|(end, point)| (end, TimestampKind::Date, point)),
        b'[' => parse_date_time_at(s, i, b'[', b']', false, scan)
            .map(|(end, point)| (end, TimestampKind::Date, point)),
        b'S' => parse_keyword_body(s, i + 1, b"CHEDULED:", TimestampKind::Scheduled, scan),
        b'D' => parse_keyword_body(s, i + 1, b"EADLINE:", TimestampKind::Deadline, scan),
        b'C' => {
            let rest = i + 1;
            if b.get(rest..rest + 3) == Some(b"LOS") {
                parse_keyword_body(s, rest + 3, b"ED:", TimestampKind::Closed, scan)
            } else if b.get(rest..rest + 3) == Some(b"LOC") {
                parse_keyword_body(s, rest + 3, b"K:", TimestampKind::Clock, scan)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_keyword_body(
    s: &str,
    rest_at: usize,
    rest: &[u8],
    kind: TimestampKind,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, TimestampKind, serde_json::Value)> {
    let b = s.as_bytes();
    if !ascii_ci_starts_with(b, rest_at, rest) {
        return None;
    }
    let after_keyword = rest_at + rest.len();
    let opener = take_mldoc_ws1(b, after_keyword)?;
    match b.get(opener).copied()? {
        b'<' => parse_date_time_at(s, opener, b'<', b'>', true, scan)
            .map(|(end, point)| (end, kind, point)),
        b'[' => parse_date_time_at(s, opener, b'[', b']', false, scan)
            .map(|(end, point)| (end, kind, point)),
        _ => None,
    }
}

/// Port of mldoc `date_time close_char ~active typ`
/// (`lib/syntax/inline.ml:1022-1066`).
fn parse_date_time_at(
    s: &str,
    at: usize,
    open: u8,
    close: u8,
    active: bool,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, serde_json::Value)> {
    let b = s.as_bytes();
    if b.get(at) != Some(&open) || !timestamp_body_has_close_before_lf(s, at, close, scan) {
        return None;
    }
    let mut i = at + 1;
    let (date_start, date_end) = take_timestamp_non_spaces(s, i, close, scan)?;
    i = date_end;
    if !b.get(i).is_some_and(|&c| is_mldoc_timestamp_space(c)) {
        return None;
    }
    let (year, month, day) = parse_date_scanf(&b[date_start..date_end])?;
    i += 1;

    let wday_start = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == wday_start {
        return None;
    }
    let wday = &s[wday_start..i];

    let mut slot1 = None;
    let mut slot2 = None;
    if b.get(i).is_some_and(|&c| is_mldoc_timestamp_space(c)) {
        i += 1;
        let (start, end) = take_timestamp_non_spaces(s, i, close, scan)?;
        slot1 = Some(&s[start..end]);
        i = end;
        if b.get(i).is_some_and(|&c| is_mldoc_timestamp_space(c)) {
            i += 1;
            let (start, end) = take_timestamp_non_spaces(s, i, close, scan)?;
            slot2 = Some(&s[start..end]);
            i = end;
        }
    }
    if b.get(i) != Some(&close) {
        return None;
    }

    let (time, repetition) = match (slot1, slot2) {
        (None, None) => (None, None),
        (Some(s1), None) => match s1.as_bytes().first().copied() {
            Some(c @ (b'+' | b'.')) => (None, repetition_parser(s1, c)),
            Some(_) => (parse_time_scanf(s1.as_bytes()), None),
            None => (None, None),
        },
        (Some(s1), Some(s2)) => (
            parse_time_scanf(s1.as_bytes()),
            s2.as_bytes()
                .first()
                .copied()
                .and_then(|c| repetition_parser(s2, c)),
        ),
        (None, Some(_)) => unreachable!(),
    };

    Some((i + 1, timestamp_point(year, month, day, wday, time, repetition, active)))
}

fn timestamp_body_has_close_before_lf(
    s: &str,
    at: usize,
    close: u8,
    scan: &mut TimestampCloseScan,
) -> bool {
    let b = s.as_bytes();
    let boundary = scan.first_close_or_lf(b, at + 1, close);
    boundary < b.len() && b[boundary] == close
}

fn timestamp_node(kind: TimestampKind, point: serde_json::Value) -> Inline {
    let date = if kind == TimestampKind::Clock {
        serde_json::json!(["Started", point])
    } else {
        point
    };
    Inline::Timestamp {
        ts: kind.label().to_string(),
        date,
        span: None,
    }
}

fn timestamp_point(
    year: i64,
    month: i64,
    day: i64,
    wday: &str,
    time: Option<(i64, i64)>,
    repetition: Option<serde_json::Value>,
    active: bool,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "date".to_string(),
        serde_json::json!({ "year": year, "month": month, "day": day }),
    );
    obj.insert("wday".to_string(), serde_json::json!(wday));
    if let Some((hour, min)) = time {
        obj.insert("time".to_string(), serde_json::json!({ "hour": hour, "min": min }));
    }
    if let Some(rep) = repetition {
        obj.insert("repetition".to_string(), rep);
    }
    obj.insert("active".to_string(), serde_json::json!(active));
    serde_json::Value::Object(obj)
}

#[inline]
fn is_mldoc_timestamp_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x1a | 0x0c)
}

fn skip_mldoc_spaces(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_mldoc_timestamp_space(b[i]) {
        i += 1;
    }
    i
}

fn take_mldoc_ws1(b: &[u8], i: usize) -> Option<usize> {
    if !b.get(i).is_some_and(|&c| is_mldoc_timestamp_space(c)) {
        return None;
    }
    Some(skip_mldoc_spaces(b, i))
}

fn ascii_ci_starts_with(b: &[u8], at: usize, pat: &[u8]) -> bool {
    b.get(at..at + pat.len()).is_some_and(|got| {
        got.iter()
            .zip(pat)
            .all(|(&g, &p)| g.to_ascii_uppercase() == p.to_ascii_uppercase())
    })
}

fn take_timestamp_non_spaces(
    s: &str,
    i: usize,
    close: u8,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let start = i;
    let end = scan.first_token_boundary_or_lf(b, i, close);
    (end > start && end < b.len() && b[end] != b'\n').then_some((start, end))
}

fn scan_i64_prefix(b: &[u8], mut i: usize) -> Option<(i64, usize)> {
    let start = i;
    if matches!(b.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let digit_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start {
        return None;
    }
    let n = std::str::from_utf8(&b[start..i]).ok()?.parse::<i64>().ok()?;
    Some((n, i))
}

/// Port of `Timestamp.parse_date`: `Scanf.sscanf s "%d-%d-%d"` with prefix
/// scanning and no width/range validation.
fn parse_date_scanf(b: &[u8]) -> Option<(i64, i64, i64)> {
    let (year, mut i) = scan_i64_prefix(b, 0)?;
    if b.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let (month, mut i) = scan_i64_prefix(b, i)?;
    if b.get(i) != Some(&b'-') {
        return None;
    }
    i += 1;
    let (day, _i) = scan_i64_prefix(b, i)?;
    Some((year, month, day))
}

/// Port of `Timestamp.parse_time`: `Scanf.sscanf s "%d:%d"` with prefix
/// scanning and no hour/minute validation.
fn parse_time_scanf(b: &[u8]) -> Option<(i64, i64)> {
    let (hour, mut i) = scan_i64_prefix(b, 0)?;
    if b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let (min, _i) = scan_i64_prefix(b, i)?;
    Some((hour, min))
}

fn parse_repetition_marker(kind: &'static str, b: &[u8]) -> Option<serde_json::Value> {
    let (n, i) = scan_i64_prefix(b, 0)?;
    let dur = match b.get(i).copied()? {
        b'h' => "Hour",
        b'd' => "Day",
        b'w' => "Week",
        b'm' => "Month",
        b'y' => "Year",
        _ => return None,
    };
    Some(serde_json::json!([[kind], [dur], n]))
}

/// Port of `Timestamp.repetition_parser` (`lib/syntax/timestamp.ml:120-136`),
/// including the byte-drop quirks for `.1d`, `x1d`, `z+1d`, signed counts, and
/// ignored suffixes after the unit char.
fn repetition_parser(tok: &str, first: u8) -> Option<serde_json::Value> {
    let b = tok.as_bytes();
    if b.len() < 2 {
        return None;
    }
    if b[1] != b'+' {
        parse_repetition_marker("Plus", &b[1..])
    } else {
        let kind = if first == b'+' { "DoublePlus" } else { "Dotted" };
        parse_repetition_marker(kind, &b[2..])
    }
}

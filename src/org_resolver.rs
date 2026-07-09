//! lsdoc ORG inline lexer+resolver (v0.2) — the Org-grammar twin of [`crate::lexer`] +
//! [`crate::resolver`]. Separate from markdown because Org's inline grammar differs in three
//! deep ways (so a shared lexer/resolver can't be byte-exact):
//!
//! 1. **Markers** are `* / + _ ^` (Bold / Italic / Strike_through / Underline / `^^`
//!    Highlight) — NOT md's `* _ ~ ^ =`. `~ … ~` is Code and `= … = ` is Verbatim (raw,
//!    ctx-gated). `_`/`^` are *dual-purpose* (emphasis vs sub/superscript).
//! 2. **Emphasis is STATEFUL**: `/ + _` gate on the *preceding* plain char (`use_state` +
//!    `last_plain_char`) — `a/b/c` stays literal, `/a/` is italic. md has no backward gate.
//! 3. **Escape is non-destructive**: Org keeps `\X` literally (`\*` → `"\\*"`), md unescapes.
//!
//! Byte-exact to mldoc, validated over the differential harness gate. **M6-core** here: text /
//! break / escape / entity; markers + specials are emitted as deferred tokens (rendered
//! literally until the emphasis / leaf / bracket sub-steps refine them).

use crate::inline::{char_len, is_ws, is_ws_or_nl};
use crate::lexer::{Kind, Token};
use crate::projection::{Inline, Span};
use crate::source_map::OriginSegment;

/// Active Org constructs (mirrors `crate::org::Ctx`; the variants below match mldoc's
/// top / nested-emphasis / link-label re-parse contexts exactly). Fields
/// are read as each construct family lands in later M6 sub-steps.
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
    /// Backward emphasis gate active (top level only). Off in every re-parse.
    pub use_state: bool,
    pub tags: bool,
    pub block_refs: bool,
    pub macros: bool,
    pub export_snippets: bool,
    pub latex: bool,
    pub urls: bool,
    pub timestamps: bool,
    pub angle: bool,
    pub code: bool,
    pub verbatim: bool,
    pub nested_emphasis: bool,
    pub breaks: bool,
    pub entity: bool,
    pub footnotes: bool,
    pub scripts: bool,
    pub links: bool,
    pub hiccup: bool,
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx {
            use_state: true,
            tags: true,
            block_refs: true,
            macros: true,
            export_snippets: true,
            latex: true,
            urls: true,
            timestamps: true,
            angle: true,
            code: true,
            verbatim: true,
            nested_emphasis: true,
            breaks: true,
            entity: true,
            footnotes: true,
            scripts: true,
            links: true,
            hiccup: true,
        }
    }
    /// `[[url][label]]` label re-parse (`org_link_1`): latex/code/entity/scripts/emphasis,
    /// NO verbatim, nested-emphasis repair, nested links, or tags.
    fn label() -> Ctx {
        Ctx {
            use_state: false,
            latex: true,
            code: true,
            verbatim: false,
            nested_emphasis: false,
            entity: true,
            scripts: true,
            links: false,
            tags: false,
            block_refs: false,
            macros: false,
            export_snippets: false,
            urls: false,
            timestamps: false,
            angle: false,
            breaks: false,
            footnotes: false,
            hiccup: false,
        }
    }
}

const ORG_MEMO_UNSEEN: usize = usize::MAX;
const ORG_MEMO_NONE: usize = usize::MAX - 1;

pub(crate) struct OrgInlineScan {
    source_len: Option<usize>,
    url_rbracket_memo: Vec<usize>,
    label_end_memo: Vec<usize>,
    chunk_end_memo: Vec<usize>,
    page_ref_scan: crate::inline::PageRefScan,
    footnote_rbracket: crate::inline::ByteBeforeEolScan,
    metadata_rbrace: crate::inline::ByteBeforeEolScan,
    target_gt: crate::inline::ByteBeforeEolScan,
}

impl OrgInlineScan {
    pub(crate) fn new() -> Self {
        Self {
            source_len: None,
            url_rbracket_memo: Vec::new(),
            label_end_memo: Vec::new(),
            chunk_end_memo: Vec::new(),
            page_ref_scan: crate::inline::PageRefScan::new(),
            footnote_rbracket: crate::inline::ByteBeforeEolScan::new(b']'),
            metadata_rbrace: crate::inline::ByteBeforeEolScan::new(b'}'),
            target_gt: crate::inline::ByteBeforeEolScan::new(b'>'),
        }
    }

    fn check_source(&mut self, len: usize) {
        match self.source_len {
            Some(existing) => debug_assert_eq!(existing, len),
            None => self.source_len = Some(len),
        }
    }

    fn label_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.label_end_memo.is_empty() {
            self.label_end_memo = vec![ORG_MEMO_UNSEEN; len + 1];
        }
        &mut self.label_end_memo
    }

    fn url_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.url_rbracket_memo.is_empty() {
            self.url_rbracket_memo = vec![ORG_MEMO_UNSEEN; len + 1];
        }
        &mut self.url_rbracket_memo
    }

    fn chunk_memo(&mut self, len: usize) -> &mut Vec<usize> {
        self.check_source(len);
        if self.chunk_end_memo.is_empty() {
            self.chunk_end_memo = vec![ORG_MEMO_UNSEEN; len + 1];
        }
        &mut self.chunk_end_memo
    }

    fn footnote_close(&mut self, bb: &[u8], from: usize) -> Option<usize> {
        self.check_source(bb.len());
        self.footnote_rbracket.first_before_eol(bb, from)
    }

    fn page_ref_scan(&mut self) -> &mut crate::inline::PageRefScan {
        &mut self.page_ref_scan
    }

    fn metadata_close(&mut self, bb: &[u8], from: usize) -> Option<usize> {
        self.check_source(bb.len());
        self.metadata_rbrace.first_before_eol(bb, from)
    }

    fn target_gt_close(&mut self, bb: &[u8], from: usize) -> Option<usize> {
        self.check_source(bb.len());
        self.target_gt.first_before_eol(bb, from)
    }
}

/// Org emphasis markers grouped into `Delim` runs. `^` (Highlight `^^` / superscript `^x`)
/// and `_` (Underline / subscript `_x`) are dual-purpose — disambiguated by the resolver.
#[inline]
fn is_marker(c: u8) -> bool {
    matches!(c, b'*' | b'/' | b'+' | b'_' | b'^')
}

/// Bytes the Org lexer treats specially (stop a plain run). `~`/`=` (code/verbatim) and the
/// brackets / `$` / `#` / `<` / `{` / `(` / `!` / `@` become deferred `Punct` tokens; the resolver
/// decides per-ctx. (Org has no backtick code span.)
#[inline]
fn is_special(c: u8) -> bool {
    c == b'\\'
        || is_marker(c)
        || matches!(
            c,
            b'~' | b'='
                | b'$'
                | b'['
                | b']'
                | b'('
                | b')'
                | b'{'
                | b'}'
                | b'<'
                | b'>'
                | b'#'
                | b'!'
                | b'@'
        )
}

#[inline]
fn is_plain_stop(c: u8) -> bool {
    is_ws_or_nl(c) || is_special(c)
}

/// Lex `s` as Org inline. Ctx-free; the resolver applies context.
pub(crate) fn org_lex(s: &str) -> Vec<Token> {
    let b = s.as_bytes();
    let n = b.len();
    let mut toks: Vec<Token> = Vec::new();
    let mut i = 0usize;
    let mut pending_start: Option<usize> = None;
    let mut pending_end = 0usize;
    macro_rules! flush {
        () => {
            if let Some(start) = pending_start.take() {
                debug_assert!(pending_end > start);
                toks.push(Token {
                    off: start,
                    kind: Kind::Text { end: pending_end },
                });
            }
        };
    }
    macro_rules! push_pending {
        ($off:expr, $end:expr) => {{
            let off = $off;
            let end = $end;
            if pending_start.is_none() {
                pending_start = Some(off);
            } else {
                debug_assert_eq!(pending_end, off);
            }
            crate::metrics::scan_work(end - off); // A1: charge scanned plain bytes (O(n))
            pending_end = end;
        }};
    }

    while i < n {
        let c = b[i];
        match c {
            b'\n' | b'\r' => {
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Newline(c),
                });
                i += 1;
            }
            b' ' | b'\t' | 0x0c => {
                flush!();
                let start = i;
                while i < n && is_ws(b[i]) {
                    i += 1;
                }
                crate::metrics::scan_work(i - start); // A1: charge copied ws bytes
                toks.push(Token {
                    off: start,
                    kind: Kind::Text { end: i },
                });
            }
            b'\\' => {
                // ALL of `\`-handling is ctx-gated (hard-break / latex / entity all hang off
                // `ctx.entity`, then escape) — so defer the whole thing: emit `Punct(\)` and
                // let the resolver run the ctx-aware `backslash()` on the raw bytes. (A `\X`
                // consumed mid-Text straddle is handled by `resync_straddle`; a `\#` inside a
                // tag leaves the `#` as its own `Punct` for a fresh tag dispatch.)
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Punct(b'\\'),
                });
                i += 1;
            }
            _ if is_marker(c) => {
                // ONE `Delim{ch,1}` token per marker BYTE — Org emphasis is byte-position based
                // (fixed k per marker; `^^` reads 2 raw bytes itself), tried at each marker, NOT
                // run-grouped like md. The resolver works off byte offsets + raw bytes.
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Delim { ch: c, len: 1 },
                });
                i += 1;
            }
            _ if is_special(c) => {
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Punct(c),
                });
                i += 1;
            }
            _ => {
                // All stop bytes are ASCII, so byte-wise scanning stays on UTF-8 boundaries:
                // continuation bytes cannot be mistaken for whitespace or a construct opener.
                let start = i;
                i += 1;
                while i < n {
                    if is_plain_stop(b[i]) {
                        break;
                    }
                    i += 1;
                }
                push_pending!(start, i);
            }
        }
    }
    flush!();
    toks
}

/// Parse an Org inline run at top level. `base` is the absolute byte offset of `text[0]` in
/// the block body — every emitted node's `span` is absolute (S2).
pub(crate) fn parse_inline_org(text: &str, base: usize) -> Vec<Inline> {
    if let Some(nodes) = crate::inline::plain_fast_path_org(text, base) {
        return nodes;
    }
    parse_ctx(text, Ctx::top(), base)
}

/// mldoc `Property.property_references`: parse property values with
/// `inline_skip_macro = true`, then keep only top-level reference-shaped inlines.
pub(crate) fn parse_property_reference_inlines_org(text: &str, base: usize) -> Vec<Inline> {
    let mut ctx = Ctx::top();
    ctx.macros = false;
    parse_ctx(text, ctx, base)
}

pub(crate) fn org_terminal_odd_backslash(text: &str) -> bool {
    ends_with_odd_backslash_run(text.as_bytes())
}

pub(crate) fn try_org_nested_emphasis_at_cached(
    text: &str,
    at: usize,
    base: usize,
    state_char: Option<u8>,
    no_closer: &mut [[bool; 2]; 5],
    terminal_odd_backslash: bool,
) -> Option<(Inline, usize)> {
    nested_emphasis_at_org(
        text,
        at,
        state_char,
        no_closer,
        base,
        terminal_odd_backslash,
    )
    .ok()
    .map(|hit| (hit.node, hit.end))
}

pub(crate) fn try_org_code_or_verbatim_at(
    text: &str,
    at: usize,
    base: usize,
) -> Option<(Inline, usize)> {
    let bb = text.as_bytes();
    let marker = *bb.get(at)?;
    if !matches!(marker, b'=' | b'~') {
        return None;
    }
    let (mut node, end) = try_code_verbatim_at(text, bb, at, marker)?;
    crate::projection::set_inline_span(&mut node, Some(Span(base + at, base + end)));
    Some((node, end))
}

fn parse_ctx(text: &str, ctx: Ctx, base: usize) -> Vec<Inline> {
    let mut toks = org_lex(text);
    resolve(text, &mut toks, ctx, base)
}

/// Flush the pending plain run as a `Plain` node (see the md resolver's `flush` for the
/// `plain_start`/`plain_end` contract — identical here).
fn flush(
    out: &mut Vec<Inline>,
    pending: &mut String,
    plain_start: &mut Option<usize>,
    plain_end: usize,
    plain_extent_start: &mut Option<usize>,
    plain_extent_end: usize,
    plain_origins: &mut Vec<OriginSegment>,
    source: &str,
    source_base: usize,
) {
    if !pending.is_empty() {
        let span = plain_start
            .take()
            .map(|s| Span(s, plain_end))
            .unwrap_or_else(|| Span((*plain_extent_start).unwrap_or(plain_end), plain_extent_end));
        let origins = std::mem::take(plain_origins);
        out.push(crate::source_map::make_plain(
            std::mem::take(pending),
            span,
            origins,
            source,
            source_base,
        ));
    } else {
        plain_start.take();
        plain_origins.clear();
    }
    *plain_extent_start = None;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmFail {
    NotMatch,
    NoCloser,
}

struct EmParsed {
    node: Inline,
    end: usize,
}

/// ARTIFACT-1.5.7 deviation from published `inline.ml`: compiled npm mldoc 1.5.7
/// backs an emphasis close off to a provenance-tracked pattern-as-plain absorb.
/// See `subagent-tasks/constructs/d19-emphasis-close-guard-spec.md` and
/// `subagent-tasks/notes/d19-diagnosis.md`.
#[derive(Clone, Copy)]
struct EmBackoffCandidate {
    closer_start: usize,
    body_len: usize,
}

/// Port of mldoc `Parsers.whitespace_chars` (`lib/parsers.ml:4`).
#[inline]
fn mldoc_whitespace_char(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Port of mldoc `underline_emphasis_delims` (`lib/syntax/inline.ml:259-293`).
#[inline]
fn underline_emphasis_delim(c: u8) -> bool {
    c.is_ascii_punctuation() || mldoc_whitespace_char(c)
}

/// Port of mldoc `underline_emphasis_delims_backward`
/// (`lib/syntax/inline.ml:390-399`).
#[inline]
fn underline_emphasis_delims_backward(state_char: Option<u8>) -> bool {
    state_char.map(underline_emphasis_delim).unwrap_or(true)
}

/// Port of mldoc `underline_emphasis_delims_lookahead`
/// (`lib/syntax/inline.ml:381-383`).
#[inline]
fn underline_emphasis_delims_lookahead(s: &str, at: usize) -> bool {
    s.as_bytes()
        .get(at)
        .copied()
        .map(underline_emphasis_delim)
        .unwrap_or(true)
}

/// Port of mldoc `is_left_flanking_delimiter_run`
/// (`lib/syntax/inline.ml:295-296`).
#[inline]
fn is_left_flanking_delimiter_run(s: &str, at: usize, pattern: &[u8]) -> bool {
    let bb = s.as_bytes();
    bb.get(at..at + pattern.len()) == Some(pattern)
        && bb
            .get(at + pattern.len())
            .is_some_and(|&c| !mldoc_whitespace_char(c))
}

fn ends_with_odd_backslash_run(bb: &[u8]) -> bool {
    if bb.last() != Some(&b'\\') {
        return false;
    }
    let mut i = bb.len();
    while i > 0 && bb[i - 1] == b'\\' {
        i -= 1;
    }
    let run = bb.len() - i;
    crate::metrics::scan_work(run);
    run % 2 == 1
}

#[inline]
fn early_escape_close_at(bb: &[u8], i: usize, pattern_c: u8, terminal_odd_backslash: bool) -> bool {
    if !terminal_odd_backslash || bb.get(i) != Some(&b'\\') || bb.get(i + 1) != Some(&pattern_c) {
        return false;
    }
    let Some(&following) = bb.get(i + 2) else {
        return false;
    };
    if following == pattern_c {
        return false;
    }
    if pattern_c == b'*' {
        !mldoc_whitespace_char(following)
    } else {
        !mldoc_whitespace_char(following) && underline_emphasis_delim(following)
    }
}

/// Port of mldoc `take_while1_include_backslash`
/// (`lib/parsers.ml:236-248`).
fn take_while1_include_backslash(
    s: &str,
    mut i: usize,
    chars_can_escape: &[u8],
    early_pattern: Option<u8>,
    terminal_odd_backslash: bool,
    mut pred: impl FnMut(u8) -> bool,
) -> Option<usize> {
    let bb = s.as_bytes();
    let start = i;
    let mut last_backslash = false;
    let mut only_backslashes = true;
    let mut backslashes = 0usize;
    while i < bb.len() {
        if only_backslashes && backslashes % 2 == 0 {
            if let Some(pattern_c) = early_pattern {
                if early_escape_close_at(bb, i, pattern_c, terminal_odd_backslash) {
                    break;
                }
            }
        }
        let c = bb[i];
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
            break;
        }
        i += char_len(c);
        if only_backslashes {
            if c == b'\\' {
                backslashes += 1;
            } else {
                only_backslashes = false;
            }
        }
    }
    if i > start {
        crate::metrics::scan_work(i - start);
        Some(i)
    } else {
        None
    }
}

fn push_plain_node(out: &mut Vec<Inline>, text: &str, start: usize, end: usize, base: usize) {
    crate::metrics::scan_work(text.len());
    if text.as_bytes().contains(&b'\r') {
        crate::metrics::scan_work(text.len());
        out.push(crate::source_map::make_plain(
            text.replace('\r', "\n"),
            Span(base + start, base + end),
            cr_plain_origins(text, base + start),
            text,
            base + start,
        ));
    } else {
        crate::metrics::scan_work(text.len());
        out.push(Inline::Plain {
            text: text.to_string(),
            span: Some(Span(base + start, base + end)),
            span_map: None,
        });
    }
}

fn cr_plain_origins(raw: &str, base: usize) -> Vec<OriginSegment> {
    crate::metrics::scan_work(raw.len()); // A1: one pass over this (disjoint) plain slice
    let bb = raw.as_bytes();
    let mut origins = Vec::new();
    let mut text_off = 0usize;
    let mut i = 0usize;
    while i < bb.len() {
        let len = char_len(bb[i]);
        origins.push(OriginSegment::new(
            text_off,
            base + i,
            if bb[i] == b'\r' { 1 } else { len },
            len,
        ));
        text_off += if bb[i] == b'\r' { 1 } else { len };
        i += len;
    }
    origins
}

fn set_char_before_pattern_from_node(node: &Inline, char_before_pattern: &mut Option<u8>) {
    match node {
        Inline::Plain { text, .. } => *char_before_pattern = text.as_bytes().last().copied(),
        Inline::Code { .. } => *char_before_pattern = Some(b'`'),
        _ => *char_before_pattern = None,
    }
}

/// Port of mldoc `md_em_parser` as `org_em_parser = md_em_parser ~include_md_code:false`
/// (`lib/syntax/inline.ml:298-375`).
fn org_md_em_parser_at(
    s: &str,
    at: usize,
    pattern: &str,
    typ: &str,
    base: usize,
    terminal_odd_backslash: bool,
) -> Result<EmParsed, EmFail> {
    let bb = s.as_bytes();
    let pat = pattern.as_bytes();
    let pattern_c = pat[0];
    if !is_left_flanking_delimiter_run(s, at, pat) {
        return Err(EmFail::NotMatch);
    }

    let mut i = at + pat.len();
    let mut body: Vec<Inline> = Vec::new();
    let mut char_before_pattern: Option<u8> = None;
    let mut saw_non_ws = false;
    let mut backoff: Option<EmBackoffCandidate> = None;

    let close_at = |closer_start: usize, body: Vec<Inline>| {
        let close_end = closer_start + pat.len();
        let full = Some(Span(base + at, base + close_end));
        EmParsed {
            node: Inline::Emphasis {
                emph: typ.to_string(),
                children: concat_plains_without_pos(body),
                span: full,
            },
            end: close_end,
        }
    };

    let early_pattern = (pat.len() == 1).then_some(pattern_c);
    let parse_non_ws = |i: usize,
                        body: &mut Vec<Inline>,
                        char_before_pattern: &mut Option<u8>,
                        backoff: &mut Option<EmBackoffCandidate>|
     -> Option<usize> {
        let stop_chars = |c: u8| c == pattern_c || mldoc_whitespace_char(c);
        let escape_chars = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c];
        if let Some(end) = take_while1_include_backslash(
            s,
            i,
            &escape_chars,
            early_pattern,
            terminal_odd_backslash,
            |c| !stop_chars(c),
        ) {
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            *backoff = None;
            return Some(end);
        }
        if bb.get(i..i + pat.len()) != Some(pat) {
            if i < bb.len() {
                let preserves_backoff = bb[i] == pattern_c;
                let end = i + char_len(bb[i]);
                push_plain_node(body, &s[i..end], i, end, base);
                set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
                if !preserves_backoff {
                    *backoff = None;
                }
                return Some(end);
            }
            return None;
        }
        let following = bb.get(i + pat.len()).copied()?;
        let pattern_as_plain = match (pattern_c, *char_before_pattern, following) {
            (b'_', Some(c), _) if mldoc_whitespace_char(c) => true,
            (b'_', _, fc) if !underline_emphasis_delim(fc) => true,
            (_, Some(c), _) if mldoc_whitespace_char(c) => true,
            _ => false,
        };
        if pattern_as_plain {
            let end = i + pat.len();
            let body_len = body.len();
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            *backoff = Some(EmBackoffCandidate {
                closer_start: i,
                body_len,
            });
            return Some(end);
        }
        None
    };

    loop {
        if i >= bb.len() {
            if let Some(candidate) = backoff {
                body.truncate(candidate.body_len);
                return Ok(close_at(candidate.closer_start, body));
            }
            return Err(EmFail::NoCloser);
        }
        if mldoc_whitespace_char(bb[i]) {
            let ws_start = i;
            while i < bb.len() && mldoc_whitespace_char(bb[i]) {
                i += 1;
            }
            crate::metrics::scan_work(i - ws_start);
            push_plain_node(&mut body, &s[ws_start..i], ws_start, i, base);
            set_char_before_pattern_from_node(body.last().unwrap(), &mut char_before_pattern);
            backoff = None;
            let before = i;
            match parse_non_ws(i, &mut body, &mut char_before_pattern, &mut backoff) {
                Some(end) => {
                    i = end;
                    saw_non_ws = true;
                    continue;
                }
                None if before >= bb.len() => {
                    if let Some(candidate) = backoff {
                        body.truncate(candidate.body_len);
                        return Ok(close_at(candidate.closer_start, body));
                    }
                    return Err(EmFail::NoCloser);
                }
                None if bb.get(before..before + pat.len()) == Some(pat) => {
                    if let Some(candidate) = backoff {
                        body.truncate(candidate.body_len);
                        return Ok(close_at(candidate.closer_start, body));
                    }
                    if saw_non_ws {
                        return Ok(close_at(before, body));
                    }
                    return Err(EmFail::NotMatch);
                }
                None => return Err(EmFail::NoCloser),
            }
        }
        match parse_non_ws(i, &mut body, &mut char_before_pattern, &mut backoff) {
            Some(end) => {
                i = end;
                saw_non_ws = true;
            }
            None if bb.get(i..i + pat.len()) == Some(pat) => {
                if let Some(candidate) = backoff {
                    body.truncate(candidate.body_len);
                    return Ok(close_at(candidate.closer_start, body));
                }
                if saw_non_ws {
                    return Ok(close_at(i, body));
                }
                return Err(EmFail::NotMatch);
            }
            None => return Err(EmFail::NoCloser),
        }
    }
}

/// Port of mldoc `org_emphasis` dispatch (`lib/syntax/inline.ml:429-449`).
fn org_emphasis_at(
    s: &str,
    at: usize,
    state_char: Option<u8>,
    no_closer: &mut [[bool; 2]; 5],
    base: usize,
    terminal_odd_backslash: bool,
) -> Result<EmParsed, EmFail> {
    let Some(&ch) = s.as_bytes().get(at) else {
        return Err(EmFail::NotMatch);
    };
    let mut parse = |pattern: &str, typ: &str, k: usize, lookahead: bool| {
        crate::metrics::scan_work(pattern.len());
        let cls = class_idx(ch);
        if no_closer[cls][k - 1] {
            return Err(EmFail::NotMatch);
        }
        match org_md_em_parser_at(s, at, pattern, typ, base, terminal_odd_backslash) {
            Ok(hit) if !lookahead || underline_emphasis_delims_lookahead(s, hit.end) => Ok(hit),
            Ok(_) => Err(EmFail::NotMatch),
            Err(EmFail::NoCloser) => {
                no_closer[cls][k - 1] = true;
                Err(EmFail::NotMatch)
            }
            Err(e) => Err(e),
        }
    };
    match ch {
        b'*' => parse("*", "Bold", 1, false),
        b'_' if underline_emphasis_delims_backward(state_char) => parse("_", "Underline", 1, true),
        b'/' if underline_emphasis_delims_backward(state_char) => parse("/", "Italic", 1, true),
        b'+' if underline_emphasis_delims_backward(state_char) => {
            parse("+", "Strike_through", 1, true)
        }
        b'^' => parse("^^", "Highlight", 2, false),
        _ => Err(EmFail::NotMatch),
    }
}

/// Port of mldoc `nested_emphasis` entry (`lib/syntax/inline.ml:919-954`).
fn nested_emphasis_at_org(
    s: &str,
    at: usize,
    state_char: Option<u8>,
    no_closer: &mut [[bool; 2]; 5],
    base: usize,
    terminal_odd_backslash: bool,
) -> Result<EmParsed, EmFail> {
    let mut hit = org_emphasis_at(s, at, state_char, no_closer, base, terminal_odd_backslash)?;
    hit.node = aux_nested_emphasis_org(hit.node);
    Ok(hit)
}

/// Port of mldoc `nested_emphasis` / `aux_nested_emphasis`
/// (`lib/syntax/inline.ml:922-947`).
fn aux_nested_emphasis_org(node: Inline) -> Inline {
    if is_synthetic_nested_emphasis(&node) {
        return node;
    }
    match node {
        Inline::Emphasis {
            emph,
            children,
            span,
        } => {
            let mut reparsed = Vec::new();
            // scan-owner: (b) suffix-absence miss-cache / accepted subtree — Org nested-emphasis child repair
            for child in children {
                crate::metrics::scan_work(1);
                match child {
                    Inline::Plain {
                        text,
                        span: plain_span,
                        span_map,
                    } => {
                        match parse_nested_plain_org(&text, plain_span.map(|s| s.0).unwrap_or(0)) {
                            Ok(result)
                                if result.len() == 1
                                    && matches!(result[0], Inline::Plain { .. }) =>
                            {
                                reparsed.push(Inline::Plain {
                                    text,
                                    span: plain_span,
                                    span_map,
                                });
                            }
                            Ok(result) => {
                                reparsed.extend(result.into_iter().map(aux_nested_emphasis_org));
                            }
                            Err(()) => reparsed.push(Inline::Plain {
                                text,
                                span: plain_span,
                                span_map: None,
                            }),
                        }
                    }
                    other => reparsed.push(other),
                }
            }
            Inline::Emphasis {
                emph,
                children: concat_plains_without_pos(reparsed),
                span,
            }
        }
        other => other,
    }
}

#[inline]
fn is_synthetic_nested_emphasis(node: &Inline) -> bool {
    matches!(
        node,
        Inline::Emphasis {
            emph,
            children,
            ..
        } if emph == "Italic"
            && children.len() == 1
            && matches!(&children[0], Inline::Emphasis { emph: inner, .. } if inner == "Bold")
    )
}

/// Port of the phase-2 parser inside mldoc `nested_emphasis`
/// (`lib/syntax/inline.ml:927-934`) for Org.
fn parse_nested_plain_org(text: &str, base: usize) -> Result<Vec<Inline>, ()> {
    let bb = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut no_closer = [[false; 2]; 5];
    let terminal_odd_backslash = ends_with_odd_backslash_run(bb);
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');
    let mut org_inline_scan = OrgInlineScan::new();
    // scan-owner: (a) consumed-on-match — Org nested plain reparse cursor
    while i < bb.len() {
        crate::metrics::scan_work(1);
        if matches!(bb[i], b'*' | b'_' | b'/' | b'+' | b'^') {
            if let Ok(hit) =
                org_emphasis_at(text, i, None, &mut no_closer, base, terminal_odd_backslash)
            {
                out.push(hit.node);
                i = hit.end;
                continue;
            }
        }
        if matches!(bb[i], b'_' | b'^') {
            let braced_close =
                bb.get(i + 1) == Some(&b'{') && script_rbrace_scan.has_before_eol(bb, i + 2);
            if let Some((mut node, end)) = try_script(text, bb, i, bb[i], braced_close, base) {
                crate::projection::set_inline_span(&mut node, Some(Span(base + i, base + end)));
                out.push(node);
                i = end;
                continue;
            }
        }
        if bb[i] == b'[' {
            if let Some((node, end)) =
                try_nested_link_or_link_org(text, bb, i, base, &mut org_inline_scan)
            {
                out.push(node);
                i = end;
                continue;
            }
        }
        let (node, end) = org_plain_at(text, i, base).ok_or(())?;
        out.push(node);
        i = end;
    }
    Ok(concat_plains_without_pos(out))
}

/// Port of mldoc Org `plain` fallback as used by `nested_emphasis`
/// (`lib/syntax/inline.ml:211-236`).
fn org_plain_at(s: &str, i: usize, base: usize) -> Option<(Inline, usize)> {
    let bb = s.as_bytes();
    if i >= bb.len() {
        return None;
    }
    let in_plain_delims = |c: u8| {
        matches!(
            c,
            b'\\' | b'_' | b'^' | b'[' | b'*' | b'/' | b'+' | b'$' | b'#'
        ) || mldoc_whitespace_char(c)
    };
    if bb[i] != b'\n' && bb[i] != b'\r' && !in_plain_delims(bb[i]) {
        let mut end = i + char_len(bb[i]);
        while end < bb.len() && bb[end] != b'\n' && bb[end] != b'\r' && !in_plain_delims(bb[end]) {
            end += char_len(bb[end]);
        }
        crate::metrics::scan_work(end - i);
        return Some((
            Inline::Plain {
                text: s[i..end].to_string(),
                span: Some(Span(base + i, base + end)),
                span_map: None,
            },
            end,
        ));
    }
    if matches!(bb[i], b' ' | b'\t' | 0x0c) {
        let mut end = i + 1;
        while end < bb.len() && matches!(bb[end], b' ' | b'\t' | 0x1a | 0x0c) {
            end += 1;
        }
        crate::metrics::scan_work(end - i);
        return Some((
            Inline::Plain {
                text: s[i..end].to_string(),
                span: Some(Span(base + i, base + end)),
                span_map: None,
            },
            end,
        ));
    }
    if bb[i] == b'\\' {
        if let Some(&next) = bb.get(i + 1) {
            if next.is_ascii_punctuation() {
                let end = i + 1 + char_len(next);
                return Some((
                    Inline::Plain {
                        text: s[i..end].to_string(),
                        span: Some(Span(base + i, base + end)),
                        span_map: None,
                    },
                    end,
                ));
            }
        }
    }
    if in_plain_delims(bb[i]) {
        let end = i + char_len(bb[i]);
        return Some((
            Inline::Plain {
                text: s[i..end].to_string(),
                span: Some(Span(base + i, base + end)),
                span_map: None,
            },
            end,
        ));
    }
    None
}

/// Port of mldoc Org `nested_link_or_link`
/// (`lib/syntax/inline.ml:915-917`) for phase-2 emphasis reparsing.
pub(crate) fn try_nested_link_or_link_org(
    s: &str,
    bb: &[u8],
    at: usize,
    base: usize,
    scan: &mut OrgInlineScan,
) -> Option<(Inline, usize)> {
    crate::metrics::scan_work(2);
    if s[at..].starts_with("[[") {
        if let Some((end, node)) = org_link_1_at(s, bb, at, base, scan) {
            return Some((node, end));
        }
        if let Some((end, content)) =
            crate::inline::parse_nested_link_with_scan(s, at, scan.page_ref_scan())
        {
            return Some((
                Inline::NestedLink {
                    content,
                    span: Some(Span(base + at, base + end)),
                },
                end,
            ));
        }
        if let Some((end, node)) = org_link_2_at(s, bb, at, base, scan) {
            return Some((node, end));
        }
    }
    None
}

fn concat_plains_without_pos(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    // scan-owner: (a2) caller-owned accepted range — Org plain-node concatenation
    for node in nodes {
        crate::metrics::scan_work(1);
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
                                crate::source_map::push_wire_segment(
                                    map,
                                    shift,
                                    start,
                                    end - start,
                                );
                            }
                        }
                    }
                }
                crate::metrics::scan_work(text.len());
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

/// Resolver: ONE ctx-aware pass over the Org tokens. M6 emphasis ports mldoc's
/// `org_emphasis` dispatch and `org_em_parser = md_em_parser ~include_md_code:false`,
/// followed by phase-2 `nested_emphasis`; sub/superscript remains the fallback for `_`/`^`.
/// `last_plain_char` mirrors mldoc `push_plain`: updated on EVERY plain append and PERSISTS
/// across nodes/flush (an emphasis node does NOT reset it).
#[allow(unused_assignments)] // last_plain_char / fresh are running state; final writes may be unread
fn resolve(s: &str, toks: &mut [Token], ctx: Ctx, base: usize) -> Vec<Inline> {
    let bb = s.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    let mut last_plain_char: Option<u8> = None;
    // Span tracking for the pending plain run (see the md resolver): `plain_start` is the
    // S5 fast path. `plain_extent_*` covers transformed source bytes too, and
    // `plain_origins` records exact copied bytes for `span_map` when S5 fails.
    let mut plain_start: Option<usize> = None;
    let mut plain_end: usize = 0;
    let mut plain_extent_start: Option<usize> = None;
    let mut plain_extent_end: usize = 0;
    let mut plain_origins: Vec<OriginSegment> = Vec::new();
    let mut no_closer = [[false; 2]; 5];

    // Bracket-pairing maps (shared with md; computed once when `[` is present) + monotone
    // closer cursors (the v1 `seq_present`/`has_rbracket`/`next_real_dbl`/`next_crlf` floors,
    // expressed as forward cursors — keep the gated `[`×n / `{{ `×n / `(( `×n runs linear).
    // scan-owner: (b) monotone cursor + per-buffer memos — Org bracket precompute gate
    crate::metrics::scan_work(bb.len());
    let has_brk = bb.contains(&b'[');
    let nested_close = if has_brk {
        crate::inline::build_nested_close(s)
    } else {
        Vec::new()
    };
    let hiccup_close = if has_brk {
        crate::inline::build_hiccup_close(s)
    } else {
        Vec::new()
    };
    let real_dbl = if has_brk {
        crate::inline::build_real_dbl(s)
    } else {
        Vec::new()
    };
    let mut real_dbl_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);
    let mut rbracket = first_byte(bb, 0, b']');
    let mut sq_rb_lb = first_seq(bb, b']', b'[', 0); // ][
    let mut sq_rr = first_seq(bb, b')', b')', 0); // ))
    let mut sq_rbrace = first_seq(bb, b'}', b'}', 0); // }}
    let mut sq_at = first_seq(bb, b'@', b'@', 0); // @@
    let mut raw_html_scan = crate::block_common::RawHtmlScan::new();
    let mut autolink_scan = crate::inline::AutolinkScan::new();
    let mut timestamp_scan = crate::inline::TimestampCloseScan::new();
    let mut email_scan = crate::inline::EmailAutolinkScan::new();
    let mut bare_url_scan = crate::inline::BareUrlScan::new();
    let mut org_inline_scan = OrgInlineScan::new();
    let terminal_odd_backslash = ends_with_odd_backslash_run(bb);
    // scan-owner: (b) monotone cursor + per-buffer memos — Org tag boundary precompute gate
    crate::metrics::scan_work(bb.len());
    let tag_boundary_runs = if ctx.tags && bb.contains(&b'#') {
        crate::inline::build_tag_boundary_runs(s)
    } else {
        Vec::new()
    };
    let tag_boundary_runs = (!tag_boundary_runs.is_empty()).then_some(tag_boundary_runs);
    // latex-backslash closer floors: only attempt `\(`/`\[` when a `\)`/`\]` exists ahead, so
    // a `\(`×n run (no closer) stays O(n) instead of an EOF re-scan per `\(` (mirrors resolver.rs).
    let mut bs_paren = first_seq(bb, b'\\', b')', 0); // \)
    let mut bs_brack = first_seq(bb, b'\\', b']', 0); // \]
    let mut dollar_scan = crate::inline::ByteBeforeEolScan::new(b'$');
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');
    // `fresh` = a dispatch point (mldoc `plain_run` stops at PLAIN_DELIMS `\ _ ^ [ * / + $ #`
    // + ws/eol). The SWALLOW openers `~ = < { ( @` fire only when fresh; mid-plain-run they are
    // absorbed as literal text.
    let mut fresh = true;

    // Update the plain-run span for a push of `$len` source bytes starting at `$off` (a
    // byte offset within `s`). Must be evaluated BEFORE the push (reads `pending.is_empty()`).
    macro_rules! track {
        ($off:expr, $len:expr) => {{
            if pending.is_empty() {
                plain_start = Some(base + $off);
                plain_extent_start = Some(base + $off);
            }
            if plain_start.is_some() {
                plain_end = base + $off + $len;
            }
            plain_extent_end = base + $off + $len;
            plain_origins.push(OriginSegment::new(pending.len(), base + $off, $len, $len));
        }};
    }
    macro_rules! append_transformed {
        ($extent_off:expr, $extent_len:expr, $src_off:expr, $txt:expr) => {{
            let txt: &str = $txt;
            if pending.is_empty() {
                plain_extent_start = Some(base + $extent_off);
            }
            plain_start = None;
            plain_extent_end = base + $extent_off + $extent_len;
            plain_origins.push(OriginSegment::new(
                pending.len(),
                base + $src_off,
                txt.len(),
                txt.len(),
            ));
            if let Some(b) = txt.bytes().next_back() {
                last_plain_char = Some(b);
            }
            crate::metrics::scan_work(txt.len()); // A1: charge copied pending bytes (O(n))
            pending.push_str(txt);
        }};
    }
    macro_rules! append {
        ($off:expr, $seg:expr) => {{
            let seg: &str = $seg;
            track!($off, seg.len());
            if let Some(b) = seg.bytes().next_back() {
                last_plain_char = Some(b);
            }
            crate::metrics::scan_work(seg.len()); // A1: charge copied pending bytes (O(n))
            pending.push_str(seg);
        }};
    }
    macro_rules! push_byte {
        ($off:expr, $c:expr) => {{
            let c: u8 = $c;
            track!($off, 1usize);
            crate::metrics::scan_work(1); // A1: charge copied pending byte
            pending.push(c as char);
            last_plain_char = Some(c);
        }};
    }
    macro_rules! flush_pending {
        () => {{
            flush(
                &mut out,
                &mut pending,
                &mut plain_start,
                plain_end,
                &mut plain_extent_start,
                plain_extent_end,
                &mut plain_origins,
                s,
                base,
            );
        }};
    }
    /// monotone: advance `$cur` to the first `$a$b`-seq at/after `$off`, return presence.
    macro_rules! present {
        ($cur:expr, $a:expr, $b:expr, $off:expr) => {{
            if $off > $cur {
                $cur = first_seq(bb, $a, $b, $off);
            }
            $cur < bb.len()
        }};
    }
    macro_rules! resync_straddle_here {
        ($t:ident, $end:expr) => {{
            $t = resync_straddle(
                s,
                toks,
                $t,
                $end,
                &mut pending,
                &mut last_plain_char,
                &mut fresh,
                &mut plain_start,
                &mut plain_end,
                &mut plain_extent_start,
                &mut plain_extent_end,
                &mut plain_origins,
                base,
                ctx,
                &mut bare_url_scan,
                &mut timestamp_scan,
            );
        }};
    }
    macro_rules! dispatch_org_text {
        ($t:ident, $off:expr, $keyword_ts:expr) => {{
            let txt = toks[$t].text(s).expect("Text token must slice source");
            let is_ws = txt.bytes().all(crate::inline::is_ws);
            if fresh && !is_ws {
                let leaf = (if $keyword_ts && ctx.timestamps {
                    crate::inline::parse_keyword_timestamp_with_scan(s, $off, &mut timestamp_scan)
                } else {
                    None
                })
                .or_else(|| {
                    if ctx.urls {
                        crate::inline::parse_bare_url_with_scan(s, $off, &mut bare_url_scan, base)
                    } else {
                        None
                    }
                });
                if let Some((end, mut node)) = leaf {
                    flush_pending!();
                    crate::projection::set_inline_span(
                        &mut node,
                        Some(Span(base + $off, base + end)),
                    );
                    out.push(node);
                    resync_straddle_here!($t, end);
                    continue;
                }
            }
            append!($off, txt);
            fresh = is_ws;
        }};
    }
    macro_rules! dispatch_org_delim {
        ($t:ident, $off:expr) => {{
            let ch = match &toks[$t].kind {
                Kind::Delim { ch, .. } => *ch,
                _ => unreachable!(),
            };
            let state_char = if ctx.use_state { last_plain_char } else { None };
            let emphasis_hit = if ctx.nested_emphasis {
                nested_emphasis_at_org(
                    s,
                    $off,
                    state_char,
                    &mut no_closer,
                    base,
                    terminal_odd_backslash,
                )
            } else {
                org_emphasis_at(
                    s,
                    $off,
                    state_char,
                    &mut no_closer,
                    base,
                    terminal_odd_backslash,
                )
            };
            if let Ok(hit) = emphasis_hit {
                flush_pending!();
                out.push(hit.node);
                fresh = true;
                $t = resync(toks, $t, hit.end);
                continue;
            }
            if (ch == b'_' || ch == b'^') && ctx.scripts {
                let braced_close = bb.get($off + 1) == Some(&b'{')
                    && script_rbrace_scan.has_before_eol(bb, $off + 2);
                if let Some((mut node, end)) = try_script(s, bb, $off, ch, braced_close, base) {
                    flush_pending!();
                    crate::projection::set_inline_span(
                        &mut node,
                        Some(Span(base + $off, base + end)),
                    );
                    out.push(node);
                    fresh = true;
                    $t = resync(toks, $t, end);
                    continue;
                }
            }
            push_byte!($off, ch);
            fresh = true;
        }};
    }

    let mut t = 0usize;
    // scan-owner: (b) monotone cursor + per-buffer memos — Org resolver token loop
    while t < toks.len() {
        crate::metrics::scan_work(1);
        let off = toks[t].off;
        match org_dispatch_byte(s, &toks[t]) {
            // inline.ml:1376 — `| '\n' -> breakline`
            b'\n' | b'\r' => {
                let c = match &toks[t].kind {
                    Kind::Newline(c) => *c,
                    _ => unreachable!(),
                };
                if ctx.breaks {
                    flush_pending!();
                    out.push(Inline::Break {
                        span: Some(Span(base + off, base + off + 1)),
                    });
                } else if c == b'\r' {
                    append_transformed!(off, 1usize, off, "\n");
                } else {
                    append!(off, "\n");
                }
                fresh = true;
            }
            // inline.ml:1377 — `| '#' -> hash_tag config`
            b'#' => {
                let mut hit = None;
                if ctx.tags {
                    let (e, children) = crate::inline::parse_tag_name(
                        s,
                        off + 1,
                        false,
                        base,
                        crate::inline::TagReparse::Org,
                        tag_boundary_runs.as_deref(),
                        org_inline_scan.page_ref_scan(),
                    );
                    if e > off + 1 && !children.is_empty() {
                        flush_pending!();
                        out.push(Inline::Tag {
                            children,
                            span: Some(Span(base + off, base + e)),
                        });
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'#');
                fresh = true;
            }
            // inline.ml:1378-1381 — `| '*' | '/' | '+' -> nested_emphasis ~state config`
            b'*' | b'/' | b'+' => dispatch_org_delim!(t, off),
            // inline.ml:1382 — `| '_' -> nested_emphasis ~state config <|> subscript config`
            b'_' => dispatch_org_delim!(t, off),
            // inline.ml:1383 — `| '^' -> nested_emphasis config <|> superscript config`
            b'^' => dispatch_org_delim!(t, off),
            // inline.ml:1384 — `| '$' -> latex_fragment config`
            b'$' => {
                let mut hit = None;
                if ctx.latex && dollar_scan.has_before_eol(bb, off + 2) {
                    if let Some((mut node, e)) = crate::inline::parse_latex_dollar_at(s, off) {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'$');
                fresh = true;
            }
            // inline.ml:1385-1388 — `| '\\' -> org_hard_breakline <|> latex_fragment <|> entity`
            b'\\' => match &toks[t].kind {
                Kind::Punct(b'\\') => {
                    let latex_ok = match bb.get(off + 1) {
                        Some(b'(') => present!(bs_paren, b'\\', b')', off),
                        Some(b'[') => present!(bs_brack, b'\\', b']', off),
                        _ => false,
                    };
                    let (bs, end) = org_backslash_at(s, bb, off, ctx, latex_ok);
                    match bs {
                        Bs::Node(mut node) => {
                            flush_pending!();
                            crate::projection::set_inline_span(
                                &mut node,
                                Some(Span(base + off, base + end)),
                            );
                            out.push(node);
                        }
                        Bs::Plain(text) => {
                            let one_to_one = text.len() == end - off;
                            if one_to_one {
                                track!(off, text.len());
                                if let Some(b) = text.bytes().next_back() {
                                    last_plain_char = Some(b);
                                }
                                pending.push_str(&text);
                            } else {
                                let src_off = if off + 1 + text.len() <= end
                                    && s.as_bytes()[off + 1..off + 1 + text.len()]
                                        == *text.as_bytes()
                                {
                                    off + 1
                                } else {
                                    off
                                };
                                append_transformed!(off, end - off, src_off, text.as_str());
                            }
                        }
                    }
                    resync_straddle_here!(t, end);
                    continue;
                }
                Kind::LatexBs(c) => {
                    push_byte!(off, b'\\');
                    push_byte!(off + 1, *c);
                    fresh = false;
                }
                Kind::Escape(x) => {
                    append_transformed!(off, 1 + x.len(), off + 1, x.as_str());
                    fresh = false;
                }
                Kind::Leaf(node) => {
                    flush_pending!();
                    let tok_end_val = if t + 1 < toks.len() {
                        toks[t + 1].off
                    } else {
                        s.len()
                    };
                    let mut node = node.clone();
                    crate::projection::set_inline_span(
                        &mut node,
                        Some(Span(base + off, base + tok_end_val)),
                    );
                    out.push(node);
                    fresh = true;
                }
                _ => unreachable!(),
            },
            // inline.ml:1389-1393 — `[` link/nested → timestamp → footnote → cookie → hiccup
            b'[' => {
                let mut hit = None;
                if ctx.links {
                    if rbracket < off {
                        rbracket = first_byte(bb, off, b']');
                    }
                    if rbracket < bb.len() {
                        let rb_lb = present!(sq_rb_lb, b']', b'[', off);
                        if let Some((mut node, e)) = try_bracket_at(
                            s,
                            bb,
                            off,
                            ctx,
                            &hiccup_close,
                            &nested_close,
                            &real_dbl,
                            &mut real_dbl_cur,
                            &mut crlf,
                            rb_lb,
                            base,
                            &mut timestamp_scan,
                            &mut org_inline_scan,
                        ) {
                            flush_pending!();
                            crate::projection::set_inline_span(
                                &mut node,
                                Some(Span(base + off, base + e)),
                            );
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'[');
                fresh = true;
            }
            // inline.ml:1394-1396 — `<` quick_link → target → radio_target → timestamp → html → email
            b'<' => {
                let mut hit = None;
                if ctx.angle && fresh {
                    if let Some((mut node, e)) = try_target_angle_at(
                        s,
                        bb,
                        off,
                        base,
                        ctx,
                        &mut org_inline_scan,
                        &mut raw_html_scan,
                        &mut autolink_scan,
                        &mut timestamp_scan,
                        &mut email_scan,
                    ) {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'<');
                fresh = false;
            }
            // inline.ml:1397 — `| '{' -> macro config`
            b'{' => {
                let mut hit = None;
                if ctx.macros && fresh && present!(sq_rbrace, b'}', b'}', off) {
                    if let Some((mut node, e)) = try_macro_at(s, bb, off) {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'{');
                fresh = false;
            }
            // inline.ml:1398 — `| '!' -> markdown_image config`
            b'!' => {
                // Current Org behavior keeps `!` as a swallowed plain byte; no behavior change in C8.
                push_byte!(off, b'!');
                fresh = false;
            }
            // inline.ml:1399 — `| '@' -> export_snippet`
            b'@' => {
                let mut hit = None;
                if ctx.export_snippets && fresh && present!(sq_at, b'@', b'@', off + 2) {
                    if let Some((mut node, e)) = crate::inline::parse_export_snippet_at(s, off) {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'@');
                fresh = false;
            }
            // inline.ml:1400 — `| '=' -> code config <|> verbatim`
            b'=' => {
                let mut hit = None;
                if ctx.verbatim && fresh {
                    if let Some((mut node, e)) = try_code_verbatim_at(s, bb, off, b'=') {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'=');
                fresh = false;
            }
            // inline.ml:1401 — `| '~' -> code config`
            b'~' => {
                let mut hit = None;
                if ctx.code && fresh {
                    if let Some((mut node, e)) = try_code_verbatim_at(s, bb, off, b'~') {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'~');
                fresh = false;
            }
            // inline.ml:1402-1408 — `| 'S' | 'C' | 'D' | 's' | 'c' | 'd' -> timestamp`
            b'S' | b'C' | b'D' | b's' | b'c' | b'd' => dispatch_org_text!(t, off, true),
            // inline.ml:1409 — `| '(' -> block_reference config`
            b'(' => {
                let mut hit = None;
                if ctx.block_refs && fresh && present!(sq_rr, b')', b')', off) {
                    if let Some((mut node, e)) = try_block_ref_at(s, bb, off) {
                        flush_pending!();
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        hit = Some(e);
                    }
                }
                if let Some(e) = hit {
                    resync_straddle_here!(t, e);
                    continue;
                }
                push_byte!(off, b'(');
                fresh = false;
            }
            // inline.ml:1410 — `| _ -> link_inline`, then `p <|> plain` at line 1412.
            _ => {
                let c = match &toks[t].kind {
                    Kind::Text { .. } => {
                        dispatch_org_text!(t, off, false);
                        t += 1;
                        continue;
                    }
                    Kind::Punct(c) => *c,
                    _ => unreachable!(),
                };
                push_byte!(off, c);
                fresh = if crate::inline_driver::org_swallow_byte(c) {
                    false
                } else {
                    crate::inline_driver::org_plain_delimiter(c)
                };
            }
        }
        t += 1;
    }
    flush_pending!();
    out
}

fn org_dispatch_byte(s: &str, tok: &Token) -> u8 {
    match &tok.kind {
        Kind::Text { .. } => s.as_bytes().get(tok.off).copied().unwrap_or(0),
        Kind::Newline(c) => *c,
        Kind::Leaf(_) | Kind::Escape(_) | Kind::LatexBs(_) => b'\\',
        Kind::Delim { ch, .. } | Kind::Punct(ch) => *ch,
    }
}

/// `_x`/`_{x}` → Subscript, `^x`/`^{x}` → Superscript (mldoc `gen_script`). Returns the node
/// and the consumed byte extent; `None` if no valid script body.
fn try_script(
    s: &str,
    bb: &[u8],
    i: usize,
    c: u8,
    braced_close: bool,
    base: usize,
) -> Option<(Inline, usize)> {
    let n = bb.len();
    let after = *bb.get(i + 1)?;
    let braced = if after == b'{' && braced_close {
        let body_start = i + 2;
        let mut j = body_start;
        while j < n && bb[j] != b'}' && bb[j] != b'\n' && bb[j] != b'\r' {
            j += 1;
        }
        crate::metrics::scan_work(j - body_start + usize::from(j < n));
        if j < n && bb[j] == b'}' && j > body_start {
            crate::metrics::scan_work(j - body_start);
            Some((s[body_start..j].to_string(), body_start, j + 1))
        } else {
            None
        }
    } else {
        None
    };
    let (content, content_start, end) = if let Some(braced) = braced {
        braced
    } else {
        if is_org_space(after) {
            return None;
        }
        let start = i + 1;
        let mut j = start;
        while j < n && !is_org_space(bb[j]) {
            j += char_len(bb[j]);
        }
        crate::metrics::scan_work(j - start + usize::from(j < n));
        crate::metrics::scan_work(j - start);
        (s[start..j].to_string(), start, j)
    };
    let children = parse_org_script_body(&content, base + content_start);
    // span set by the caller over [i, end).
    let node = if c == b'_' {
        Inline::Subscript {
            children,
            span: None,
        }
    } else {
        Inline::Superscript {
            children,
            span: None,
        }
    };
    Some((node, end))
}

/// Port of the body parser inside mldoc `gen_script`:
/// `many1 (choice [ emphasis; plain; whitespaces; entity ])`.
fn parse_org_script_body(text: &str, base: usize) -> Vec<Inline> {
    let bb = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut no_closer = [[false; 2]; 5];
    let terminal_odd_backslash = ends_with_odd_backslash_run(bb);
    // scan-owner: (a) consumed-on-match / caller-gated — Org script body cursor
    while i < bb.len() {
        crate::metrics::scan_work(1);
        if matches!(bb[i], b'*' | b'_' | b'/' | b'+' | b'^') {
            if let Ok(hit) =
                org_emphasis_at(text, i, None, &mut no_closer, base, terminal_odd_backslash)
            {
                out.push(hit.node);
                i = hit.end;
                continue;
            }
        }
        if let Some((node, end)) = org_plain_at(text, i, base) {
            out.push(node);
            i = end;
            continue;
        }
        if bb[i] == b'\\' {
            if let Some((node, end)) = org_entity_at(text, bb, i, base) {
                out.push(node);
                i = end;
                continue;
            }
        }
        return vec![Inline::Plain {
            text: {
                crate::metrics::scan_work(text.len());
                text.to_string()
            },
            span: Some(Span(base, base + text.len())),
            span_map: None,
        }];
    }
    concat_plains_without_pos(out)
}

fn org_entity_at(s: &str, bb: &[u8], i: usize, base: usize) -> Option<(Inline, usize)> {
    if bb.get(i) != Some(&b'\\') || !bb.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let start = i + 1;
    let mut end = start;
    while end < bb.len() && bb[end].is_ascii_alphabetic() {
        end += 1;
    }
    crate::metrics::scan_work(end - start + usize::from(end < bb.len()));
    let name = &s[start..end];
    if s[end..].starts_with("{}") {
        crate::metrics::scan_work(2);
        end += 2;
    }
    match crate::entities::find(name) {
        Some(e) => {
            crate::metrics::scan_work(
                e.name.len() + e.latex.len() + e.html.len() + e.ascii.len() + e.unicode.len(),
            );
            Some((
                Inline::Entity {
                    name: e.name.to_string(),
                    latex: e.latex.to_string(),
                    latex_mathp: e.latex_mathp,
                    html: e.html.to_string(),
                    ascii: e.ascii.to_string(),
                    unicode: e.unicode.to_string(),
                    span: Some(Span(base + i, base + end)),
                },
                end,
            ))
        }
        None => {
            crate::metrics::scan_work(name.len());
            Some((
                crate::source_map::make_plain(
                    name.to_string(),
                    Span(base + i, base + end),
                    vec![OriginSegment::new(0, base + i + 1, name.len(), name.len())],
                    s,
                    base,
                ),
                end,
            ))
        }
    }
}

fn is_org_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x1a | 0x0c)
}

fn class_idx(c: u8) -> usize {
    match c {
        b'*' => 0,
        b'/' => 1,
        b'+' => 2,
        b'_' => 3,
        _ => 4, // '^'
    }
}

/// Consume-on-match owner for token-boundary constructs: advance the token cursor to the first
/// token at/after byte `end`. Emphasis/script ends land on a token boundary; tag/bare-url
/// straddles are handled by `resync_straddle`.
fn resync(toks: &[Token], mut t: usize, end: usize) -> usize {
    // scan-owner: (a2) caller-owned accepted range — Org token resync cursor
    while t < toks.len() && toks[t].off < end {
        crate::metrics::scan_work(1);
        t += 1;
    }
    t
}

/// Result of the ctx-aware Org `\`-dispatch.
enum Bs {
    Node(Inline),
    Plain(String),
}

/// mldoc Org `backslash()` on raw bytes at `i` (the `\`). ctx-gated: hard-break / latex /
/// entity all hang off `ctx.entity` (then `ctx.latex`); otherwise `\X`-punct stays literal
/// (Org never unescapes) and a lone `\` is kept. Returns the action + consumed byte extent.
fn org_backslash_at(s: &str, bb: &[u8], i: usize, ctx: Ctx, latex_ok: bool) -> (Bs, usize) {
    let n = bb.len();
    if ctx.entity {
        match bb.get(i + 1) {
            None => return (Bs::Plain("\\".to_string()), i + 1),
            Some(b'\n') | Some(b'\r') => {
                // mldoc `org_hard_breakline = string "\\" <* eol`
                // (`lib/syntax/inline.ml:456`): consume the backslash plus the
                // EOL byte this resolver matched. CRLF intentionally leaves the
                // following LF for normal break dispatch in this byte path.
                return (Bs::Node(Inline::HardBreak { span: None }), i + 2);
            }
            _ => {}
        }
        // `latex_ok` is the caller's closer-floor verdict (a `\)`/`\]` exists ahead). When it
        // is false the `find_sub` scan would fail anyway, so skip it — that is what keeps a
        // `\(`×n run linear; the result (fall through to the punct-escape `\(` below) is
        // identical to attempting and failing.
        if ctx.latex && latex_ok {
            if let Some((node, end)) = crate::inline::parse_latex_backslash_at(s, i) {
                return (Bs::Node(node), end);
            }
        }
        if bb.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            let start = i + 1;
            let mut j = start;
            while j < n && bb[j].is_ascii_alphabetic() {
                j += 1;
            }
            crate::metrics::scan_work(j - start + usize::from(j < n));
            crate::metrics::scan_work(j - start);
            let name = s[start..j].to_string();
            if s[j..].starts_with("{}") {
                crate::metrics::scan_work(2);
                j += 2;
            }
            return match crate::entities::find(&name) {
                Some(e) => {
                    crate::metrics::scan_work(
                        e.name.len()
                            + e.latex.len()
                            + e.html.len()
                            + e.ascii.len()
                            + e.unicode.len(),
                    );
                    (
                        Bs::Node(Inline::Entity {
                            name: e.name.to_string(),
                            latex: e.latex.to_string(),
                            latex_mathp: e.latex_mathp,
                            html: e.html.to_string(),
                            ascii: e.ascii.to_string(),
                            unicode: e.unicode.to_string(),
                            span: None,
                        }),
                        j,
                    )
                }
                None => (Bs::Plain(name), j),
            };
        }
    }
    match bb.get(i + 1) {
        Some(&c) if c.is_ascii_punctuation() => {
            let w = char_len(c);
            crate::metrics::scan_work(1 + w);
            (Bs::Plain(s[i..i + 1 + w].to_string()), i + 1 + w)
        }
        _ => {
            crate::metrics::scan_work(1);
            (Bs::Plain("\\".to_string()), i + 1)
        }
    }
}

/// Consume-on-match owner for constructs whose raw `end` may land MID a Text token (an Org
/// `\X`-escape / entity that consumed into the following ordinary run): advance past `end`,
/// and if it lands strictly inside a token, either re-dispatch the bounded split tail (when it
/// starts a keyword timestamp / bare URL) or push `s[end..tok_end]` raw. Org never unescapes;
/// the straddled token is always ordinary Text.
///
/// FAST PATH (Phase C, audit bug 2b): the outer `org_lex(s)` already tokenized `[end, n)`, and
/// Org's lexer has NO non-local construct (backticks are plain; there are no Code/Entity
/// `Leaf`s — entities are resolver-level), so `toks[t+1..]` is ALWAYS the correct tail. On the
/// `leads` (keyword-ts / bare-url) case that used to re-lex the whole suffix, re-lex ONLY the
/// O(1) split token's tail `[end, te)` → one `Text` token, overwrite `toks[t]`, and re-dispatch
/// via the loop → O(n), no native stack. (The straddled token is always ordinary `Text`.)
#[allow(clippy::too_many_arguments)]
fn resync_straddle(
    s: &str,
    toks: &mut [Token],
    mut t: usize,
    end: usize,
    pending: &mut String,
    last_plain_char: &mut Option<u8>,
    fresh: &mut bool,
    plain_start: &mut Option<usize>,
    plain_end: &mut usize,
    plain_extent_start: &mut Option<usize>,
    plain_extent_end: &mut usize,
    plain_origins: &mut Vec<OriginSegment>,
    base: usize,
    ctx: Ctx,
    bare_url_scan: &mut crate::inline::BareUrlScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
) -> usize {
    let n = s.len();
    // scan-owner: (a2) caller-owned accepted range — Org straddle token resync cursor
    while t < toks.len()
        && (if t + 1 < toks.len() {
            toks[t + 1].off
        } else {
            n
        }) <= end
    {
        crate::metrics::scan_work(1);
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        // straddle: an entity/escape consumed into the following ordinary Text run. The tail
        // is `end`'s fresh dispatch point — if it LEADS a no-opener construct (keyword-ts /
        // bare-url), re-dispatch the tail from `end`; else push the plain tail raw.
        let te = if t + 1 < toks.len() {
            toks[t + 1].off
        } else {
            n
        };
        let leads = (ctx.timestamps
            && matches!(s.as_bytes()[end], b'S' | b'C' | b'D' | b's' | b'c' | b'd')
            && crate::inline::parse_keyword_timestamp_with_scan(s, end, timestamp_scan).is_some())
            || (ctx.urls
                && crate::inline::parse_bare_url_with_scan(s, end, bare_url_scan, base).is_some());
        if leads {
            // Re-lex ONLY the split token's tail (Org is fully local — see fn doc), overwrite
            // `toks[t]`, re-dispatch. Because the caller straddled an ordinary Text token, the
            // tail lexes to one local Text/Punct token; the old suffix reparse fallback was
            // unreachable after C1-C7 moved entities/code to resolver-local dispatch.
            let mut retok = org_lex(&s[end..te]);
            debug_assert!(
                retok.len() == 1 && matches!(retok[0].kind, Kind::Text { .. } | Kind::Punct(_))
            );
            crate::metrics::scan_work(te - end); // O(1): ONLY the split token re-lexed
            retok[0].rebase(end); // local → absolute
            toks[t] = retok
                .pop()
                .expect("org split text tail must re-lex to one token");
            *fresh = true; // `end` is a fresh dispatch point
            return t; // re-dispatch the corrected token in the same loop
        }
        // the tail is pushed RAW (org never unescapes) → 1:1 with source from `end`. pending
        // is empty here (the caller flushed before pushing its node), so this is a fresh run.
        let tail = &s[end..te];
        if let Some(b) = tail.bytes().next_back() {
            *last_plain_char = Some(b);
        }
        if pending.is_empty() {
            *plain_start = Some(base + end);
            *plain_extent_start = Some(base + end);
        }
        if plain_start.is_some() {
            *plain_end = base + te;
        }
        *plain_extent_end = base + te;
        plain_origins.push(OriginSegment::new(
            pending.len(),
            base + end,
            tail.len(),
            tail.len(),
        ));
        *fresh = !tail.is_empty() && tail.bytes().all(crate::inline::is_ws);
        crate::metrics::scan_work(tail.len());
        pending.push_str(tail);
        t += 1;
    } else {
        // clean construct end → fresh dispatch point.
        *fresh = true;
    }
    t
}

// ---- leaf / bracket constructs (byte-based;
// shared free predicates reused from `crate::inline` / `crate::org`) -----------------------

/// `~ … ~` Code / `= … = ` Verbatim (non-empty, no marker / eol inside) — v1 try_code/verbatim.
fn try_code_verbatim_at(s: &str, bb: &[u8], i: usize, marker: u8) -> Option<(Inline, usize)> {
    let n = bb.len();
    let start = i + 1;
    let mut j = start;
    while j < n && bb[j] != marker && bb[j] != b'\n' && bb[j] != b'\r' {
        j += 1;
    }
    crate::metrics::scan_work(j - start + usize::from(j < n));
    if j > start && j < n && bb[j] == marker {
        crate::metrics::scan_work(j - start);
        let body = s[start..j].to_string();
        let node = if marker == b'~' {
            Inline::Code {
                text: body,
                span: None,
            }
        } else {
            Inline::Verbatim {
                text: body,
                span: None,
            }
        };
        Some((node, j + 1))
    } else {
        None
    }
}

/// Org `<` arm: quick_link → target → radio_target → timestamp → inline_html → email.
pub(crate) fn try_target_angle_at(
    s: &str,
    bb: &[u8],
    i: usize,
    base: usize,
    ctx: Ctx,
    org_inline_scan: &mut OrgInlineScan,
    raw_html_scan: &mut crate::block_common::RawHtmlScan,
    autolink_scan: &mut crate::inline::AutolinkScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
    email_scan: &mut crate::inline::EmailAutolinkScan,
) -> Option<(Inline, usize)> {
    let n = bb.len();
    if crate::inline::autolink_has_closing_boundary(s, i, autolink_scan) {
        if let Some((end, node)) = crate::inline::parse_quick_link_org(s, i, base) {
            return Some((node, end));
        }
    }
    if s[i..].starts_with("<<") {
        let inner_start = i + 2;
        if let Some(j) = org_inline_scan.target_gt_close(bb, inner_start) {
            if j > inner_start && j + 1 < n && bb[j] == b'>' && bb[j + 1] == b'>' {
                crate::metrics::scan_work(j - inner_start);
                return Some((
                    Inline::Target {
                        text: s[inner_start..j].to_string(),
                        span: None,
                    },
                    j + 2,
                ));
            }
        }
    }
    if s[i..].starts_with("<<<") {
        let inner_start = i + 3;
        if let Some(j) = org_inline_scan.target_gt_close(bb, inner_start) {
            if j > inner_start
                && j + 2 < n
                && bb[j] == b'>'
                && bb[j + 1] == b'>'
                && bb[j + 2] == b'>'
            {
                crate::metrics::scan_work(j - inner_start);
                return Some((
                    Inline::Target {
                        text: s[inner_start..j].to_string(),
                        span: None,
                    },
                    j + 3,
                ));
            }
        }
    }
    if ctx.timestamps {
        if let Some((end, node)) =
            crate::inline::parse_angle_timestamp_with_scan(s, i, timestamp_scan)
        {
            return Some((node, end));
        }
    }
    if let Some(extent) =
        crate::block_common::parse_raw_html_at_cached(s, i, s.len(), Some(raw_html_scan))
    {
        return Some((
            Inline::InlineHtml {
                text: crate::block_common::raw_html_capture_text(s, i, extent.end),
                span: None,
            },
            extent.end,
        ));
    }
    if let Some((end, node)) = crate::inline::parse_email_autolink_cached(s, i, email_scan) {
        return Some((node, end));
    }
    None
}

/// `{{ … }}` / `{{{ … }}}` macro — v1 try_macro (caller guarantees a `}}` exists ahead).
fn try_macro_at(s: &str, bb: &[u8], i: usize) -> Option<(Inline, usize)> {
    let n = bb.len();
    if !s[i..].starts_with("{{") {
        return None;
    }
    let candidates: &[(&str, &str)] = if s[i..].starts_with("{{{") {
        &[("{{{", "}}}"), ("{{", "}}")]
    } else {
        &[("{{", "}}")]
    };
    for &(open, close) in candidates {
        let inner_start = i + open.len();
        let mut j = inner_start;
        while j < n && bb[j] != b'}' && bb[j] != b'\n' && bb[j] != b'\r' {
            j += 1;
        }
        crate::metrics::scan_work(j - inner_start + usize::from(j < n));
        if j == inner_start || !s[j..].starts_with(close) {
            continue;
        }
        if let Some((name, args)) = crate::inline::parse_macro(&s[inner_start..j]) {
            return Some((
                Inline::Macro {
                    name,
                    args,
                    span: None,
                },
                j + close.len(),
            ));
        }
    }
    None
}

/// `(( … ))` block ref — v1 try_block_ref (caller guarantees a `))` exists ahead).
fn try_block_ref_at(s: &str, bb: &[u8], i: usize) -> Option<(Inline, usize)> {
    let n = bb.len();
    if !s[i..].starts_with("((") {
        return None;
    }
    let inner_start = i + 2;
    let mut j = inner_start;
    while j < n && bb[j] != b')' {
        j += 1;
    }
    crate::metrics::scan_work(j - inner_start + usize::from(j < n));
    if j == inner_start {
        return None;
    }
    if j + 1 < n && bb[j] == b')' && bb[j + 1] == b')' {
        crate::metrics::scan_work(j - inner_start);
        crate::metrics::scan_work(j + 2 - i);
        let inner = s[inner_start..j].to_string();
        let full = s[i..j + 2].to_string();
        return Some((
            Inline::Link {
                url: crate::projection::Url::BlockRef { v: inner },
                label: vec![],
                full,
                image: false,
                metadata: String::new(),
                title: None,
                span: None,
            },
            j + 2,
        ));
    }
    None
}

/// `[` bracket dispatch — mldoc Org order: nested/link → timestamp → footnote →
/// statistics-cookie → hiccup. Maps + cursors mirror md's
/// `[[…]]` linearity devices.
#[allow(clippy::too_many_arguments)]
fn try_bracket_at(
    s: &str,
    bb: &[u8],
    off: usize,
    ctx: Ctx,
    hiccup_close: &[usize],
    nested_close: &[usize],
    real_dbl: &[usize],
    real_dbl_cur: &mut usize,
    crlf: &mut usize,
    rb_lb_present: bool,
    base: usize,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
    org_inline_scan: &mut OrgInlineScan,
) -> Option<(Inline, usize)> {
    if s[off..].starts_with("[[") {
        if rb_lb_present {
            if let Some((end, node)) = org_link_1_at(s, bb, off, base, org_inline_scan) {
                return Some((node, end));
            }
        }
        if nested_close.get(off).is_some_and(|&e| e != usize::MAX) {
            if let Some((end, content)) = crate::inline::parse_nested_link(s, off) {
                return Some((
                    Inline::NestedLink {
                        content,
                        span: None,
                    },
                    end,
                ));
            }
        }
        // scan-owner: (b) OrgInlineScan owner / (a) accepted copy — Org real-dbl cursor
        while real_dbl.get(*real_dbl_cur).is_some_and(|&p| p < off + 2) {
            crate::metrics::scan_work(1);
            *real_dbl_cur += 1;
        }
        if let Some(&d) = real_dbl.get(*real_dbl_cur) {
            if off > *crlf {
                *crlf = first_crlf(bb, off);
            }
            if d > off + 2 && *crlf > d {
                if let Some((end, node)) = org_link_2_at(s, bb, off, base, org_inline_scan) {
                    return Some((node, end));
                }
            }
        }
    }
    if ctx.timestamps {
        if let Some((end, node)) =
            crate::inline::parse_bracket_timestamp_with_scan(s, off, timestamp_scan)
        {
            return Some((node, end));
        }
    }
    if ctx.footnotes {
        if let Some((end, name)) = org_footnote_at(s, off, org_inline_scan) {
            return Some((Inline::Fnref { name, span: None }, end));
        }
    }
    if let Some((end, node)) = crate::inline::parse_statistics_cookie(s, off) {
        return Some((node, end));
    }
    if ctx.hiccup && bb.get(off + 1) == Some(&b':') && crate::inline::hiccup_head_ok(s, off) {
        if let Some(end) = hiccup_close.get(off).copied().filter(|&e| e != usize::MAX) {
            crate::metrics::scan_work(end - off);
            return Some((
                Inline::Hiccup {
                    v: s[off..end].to_string(),
                    span: None,
                },
                end,
            ));
        }
    }
    None
}

/// `[[url][label]]` — port of mldoc `org_link_1`
/// (`syntax/inline.ml:617-696`).
fn org_link_1_at(
    s: &str,
    bb: &[u8],
    at: usize,
    base: usize,
    scan: &mut OrgInlineScan,
) -> Option<(usize, Inline)> {
    let url_start = at + 2;
    let j = find_org_link_url_rbracket(bb, url_start, scan);
    if j == url_start || j >= bb.len() {
        return None;
    }
    if !s[j..].starts_with("][") {
        return None;
    }
    let label_start = j + 2;
    let close = find_org_label_end(bb, label_start, scan)?;
    crate::metrics::scan_work(j - url_start);
    let url_text = s[url_start..j].to_string();
    crate::metrics::scan_work(close - label_start);
    let label_text = s[label_start..close].to_string();
    let mut end = close + 2;
    let metadata = read_metadata(s, bb, &mut end, scan);
    let url = crate::org::classify_org_link_1(&url_text, &label_text);
    // label_text is a raw slice of `s` starting at `label_start` → children index off that.
    let label = parse_ctx(&label_text, Ctx::label(), base + label_start);
    let label_first = match label.first() {
        Some(Inline::Plain { text, .. }) => {
            crate::metrics::scan_work(text.len());
            text.clone()
        }
        _ => String::new(),
    };
    crate::metrics::scan_work(url_text.len() + label_first.len() + metadata.len() + 6);
    let full = format!("[[{}][{}]]{}", url_text, label_first, metadata);
    // span set by the caller over [at, end).
    Some((
        end,
        Inline::Link {
            url,
            label,
            full,
            image: false,
            metadata,
            title: None,
            span: None,
        },
    ))
}

fn find_org_link_url_rbracket(bb: &[u8], start: usize, scan: &mut OrgInlineScan) -> usize {
    let mut j = start;
    let mut visited: Vec<usize> = Vec::new();
    let result = loop {
        if j >= bb.len() {
            break bb.len();
        }
        let memo = scan
            .url_memo(bb.len())
            .get(j)
            .copied()
            .unwrap_or(ORG_MEMO_UNSEEN);
        if memo != ORG_MEMO_UNSEEN {
            break memo;
        }
        visited.push(j);
        match bb[j] {
            b']' => {
                crate::metrics::scan_work(1);
                break j;
            }
            b'\\' => {
                crate::metrics::scan_work(1);
                j += 1;
                if j < bb.len() {
                    let w = char_len(bb[j]);
                    crate::metrics::scan_work(w);
                    j += w;
                }
            }
            _ => {
                let w = char_len(bb[j]);
                crate::metrics::scan_work(w);
                j += w;
            }
        }
    };
    if !visited.is_empty() {
        let memo = scan.url_memo(bb.len());
        for pos in visited {
            memo[pos] = result;
        }
    }
    result
}

/// `[[url]]` — v1 org_link_2 (single `]` allowed, non-empty, no eol).
fn org_link_2_at(
    s: &str,
    bb: &[u8],
    at: usize,
    base: usize,
    scan: &mut OrgInlineScan,
) -> Option<(usize, Inline)> {
    let name_start = at + 2;
    scan.check_source(bb.len());
    let close = scan.page_ref_scan().org_link2_close(bb, at)?;
    crate::metrics::scan_work(close - name_start);
    let name = s[name_start..close].to_string();
    let url = crate::org::classify_org_link_2(&name);
    crate::metrics::scan_work(name.len() + 4);
    let full = format!("[[{}]]", name);
    // the synthetic label (== name) is a raw slice of `s` at `name_start` → span it.
    let label = match &url {
        crate::projection::Url::PageRef { .. } => vec![],
        _ => vec![Inline::Plain {
            text: {
                crate::metrics::scan_work(name.len());
                name.clone()
            },
            span: Some(Span(base + name_start, base + close)),
            span_map: None,
        }],
    };
    // span set by the caller over [at, j + 2).
    Some((
        close + 2,
        Inline::Link {
            url,
            label,
            full,
            image: false,
            metadata: String::new(),
            title: None,
            span: None,
        },
    ))
}

/// End of the `org_link_1` label. Mirrors `label_part_choices`
/// (`syntax/inline.ml:621-642`): a single `]` is label text unless it starts the
/// final `]]`.
fn find_org_label_end(bb: &[u8], start: usize, scan: &mut OrgInlineScan) -> Option<usize> {
    let mut j = start;
    let mut visited: Vec<usize> = Vec::new();
    let result = loop {
        if j >= bb.len() {
            break ORG_MEMO_NONE;
        }
        let memo = scan
            .label_memo(bb.len())
            .get(j)
            .copied()
            .unwrap_or(ORG_MEMO_NONE);
        if memo != ORG_MEMO_UNSEEN {
            break memo;
        }
        visited.push(j);
        if bb[j] == b']' && bb.get(j + 1) == Some(&b']') {
            crate::metrics::scan_work(2);
            break j;
        }
        if let Some(end) = take_org_label_plain(bb, j) {
            j = end;
            continue;
        }
        match bb[j] {
            b'[' => {
                j = org_balanced_label_chunk(bb, j, scan);
            }
            b']' => {
                crate::metrics::scan_work(1);
                j += 1;
            }
            _ => {
                crate::metrics::scan_work(char_len(bb[j]));
                j += char_len(bb[j]);
            }
        }
    };
    if !visited.is_empty() {
        let memo = scan.label_memo(bb.len());
        for pos in visited {
            memo[pos] = result;
        }
    }
    (result != ORG_MEMO_NONE).then_some(result)
}

fn take_org_label_plain(bb: &[u8], at: usize) -> Option<usize> {
    let mut j = at;
    let mut last_backslash = false;
    while j < bb.len() {
        let c = bb[j];
        let take = if last_backslash && matches!(c, b'[' | b']') {
            last_backslash = false;
            true
        } else if last_backslash {
            last_backslash = false;
            c != b'\n' && c != b'\r' && !matches!(c, b'[' | b']')
        } else if c == b'\\' {
            last_backslash = true;
            true
        } else {
            c != b'\n' && c != b'\r' && !matches!(c, b'[' | b']')
        };
        if !take {
            break;
        }
        j += char_len(c);
    }
    if j > at {
        crate::metrics::scan_work(j - at + usize::from(j < bb.len()));
    }
    (j > at).then_some(j)
}

fn org_balanced_label_chunk(bb: &[u8], at: usize, scan: &mut OrgInlineScan) -> usize {
    let cached = scan
        .chunk_memo(bb.len())
        .get(at)
        .copied()
        .unwrap_or(ORG_MEMO_UNSEEN);
    if cached != ORG_MEMO_UNSEEN {
        return cached;
    }
    let mut j = at;
    let mut depth = 0usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut scanned = 0usize;
    while j < bb.len() {
        match bb[j] {
            b'\\' => {
                scanned += 1;
                j += 1;
                if j < bb.len() {
                    let next = bb[j];
                    if matches!(next, b'[' | b']') || !matches!(next, b'[' | b']') {
                        scanned += char_len(next);
                        j += char_len(next);
                    }
                }
            }
            b'[' => {
                depth += 1;
                stack.push(j);
                scanned += 1;
                j += 1;
            }
            b']' if depth == 0 => break,
            b']' => {
                depth -= 1;
                scanned += 1;
                j += 1;
                if let Some(open) = stack.pop() {
                    let memo = scan.chunk_memo(bb.len());
                    if memo[open] == ORG_MEMO_UNSEEN {
                        memo[open] = j;
                    }
                }
                if depth == 0 {
                    break;
                }
            }
            _ => {
                let w = char_len(bb[j]);
                scanned += w;
                j += w;
            }
        }
    }
    if j >= bb.len() {
        let memo = scan.chunk_memo(bb.len());
        for open in stack {
            if memo[open] == ORG_MEMO_UNSEEN {
                memo[open] = bb.len();
            }
        }
    }
    crate::metrics::scan_work(scanned);
    let memo = scan.chunk_memo(bb.len());
    if memo[at] == ORG_MEMO_UNSEEN {
        memo[at] = j;
    }
    j
}

/// Optional `{ … }` metadata after a link; advances `end` and returns it (incl. braces) or "".
fn read_metadata(s: &str, bb: &[u8], end: &mut usize, scan: &mut OrgInlineScan) -> String {
    if bb.get(*end) == Some(&b'{') {
        if let Some(close) = scan.metadata_close(bb, *end + 1) {
            crate::metrics::scan_work(close + 1 - *end);
            let meta = s[*end..close + 1].to_string();
            *end = close + 1;
            return meta;
        }
    }
    String::new()
}

/// `[fn:name]` / `[fn:name:def]` / `[fn::def]` → name — v1 org_footnote_ref.
pub(crate) fn org_footnote_at(
    s: &str,
    i: usize,
    scan: &mut OrgInlineScan,
) -> Option<(usize, String)> {
    let rest = s[i..].strip_prefix("[fn:")?;
    if rest.strip_prefix(':').is_some() {
        let def_start = i + 5;
        let close = scan.footnote_close(s.as_bytes(), def_start)?;
        if close == def_start {
            return None;
        }
        return Some((close + 1, String::new()));
    }
    let rb = rest.as_bytes();
    let mut j = 0;
    while j < rb.len() && rb[j] != b':' && rb[j] != b']' && rb[j] != b'\n' && rb[j] != b'\r' {
        j += 1;
    }
    crate::metrics::scan_work(j + usize::from(j < rb.len()));
    if j == 0 {
        return None;
    }
    crate::metrics::scan_work(j);
    let name = rest[..j].to_string();
    let after = &rest[j..];
    if after.starts_with(':') {
        let def_start = i + 4 + j + 1;
        let close = scan.footnote_close(s.as_bytes(), def_start)?;
        Some((close + 1, name))
    } else {
        after.strip_prefix(']')?;
        Some((i + 4 + j + 1, name))
    }
}

/// First byte `c` at/after `from`, else `bb.len()` (monotone-cursor helper).
fn first_byte(bb: &[u8], from: usize, c: u8) -> usize {
    let mut p = from;
    let mut scanned = 0usize;
    while p < bb.len() && bb[p] != c {
        scanned += 1;
        p += 1;
    }
    if p < bb.len() {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    p
}

/// First `a b` 2-byte sequence at/after `from`, else `bb.len()` (monotone).
fn first_seq(bb: &[u8], a: u8, b: u8, from: usize) -> usize {
    let mut p = from;
    let mut scanned = 0usize;
    while p + 1 < bb.len() {
        scanned += 1;
        if bb[p] == a && bb[p + 1] == b {
            crate::metrics::scan_work(scanned + 1);
            return p;
        }
        p += 1;
    }
    crate::metrics::scan_work(scanned);
    bb.len()
}

/// First `\n`/`\r` at/after `from`, else `bb.len()` (page-ref eol boundary).
fn first_crlf(bb: &[u8], from: usize) -> usize {
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

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
            breaks: true,
            entity: true,
            footnotes: true,
            scripts: true,
            links: true,
            hiccup: true,
        }
    }
    /// `[[url][label]]` label re-parse (`org_link_1`): latex/code/entity/scripts/emphasis,
    /// NO nested links, NO tags.
    fn label() -> Ctx {
        Ctx {
            use_state: false,
            latex: true,
            code: true,
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

/// Lex `s` as Org inline. Ctx-free; the resolver applies context.
pub(crate) fn org_lex(s: &str) -> Vec<Token> {
    let b = s.as_bytes();
    let n = b.len();
    let mut toks: Vec<Token> = Vec::new();
    let mut i = 0usize;
    let mut pending = String::new();
    let mut pending_off = 0usize;
    macro_rules! flush {
        () => {
            if !pending.is_empty() {
                toks.push(Token {
                    off: pending_off,
                    kind: Kind::Text(std::mem::take(&mut pending)),
                });
            }
        };
    }
    macro_rules! push_pending {
        ($off:expr, $seg:expr) => {{
            if pending.is_empty() {
                pending_off = $off;
            }
            pending.push_str($seg);
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
                toks.push(Token {
                    off: start,
                    kind: Kind::Text(s[start..i].to_string()),
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
                let start = i;
                i += char_len(c);
                while i < n {
                    let d = b[i];
                    if is_ws_or_nl(d) || is_special(d) {
                        break;
                    }
                    i += char_len(d);
                }
                push_pending!(start, &s[start..i]);
            }
        }
    }
    flush!();
    toks
}

/// Parse an Org inline run at top level. `base` is the absolute byte offset of `text[0]` in
/// the block body — every emitted node's `span` is absolute (S2).
pub(crate) fn parse_inline_org(text: &str, base: usize) -> Vec<Inline> {
    parse_ctx(text, Ctx::top(), base)
}

fn parse_ctx(text: &str, ctx: Ctx, base: usize) -> Vec<Inline> {
    if !ctx.breaks && text.as_bytes().contains(&b'\r') {
        let text = text.replace('\r', "\n");
        let mut toks = org_lex(&text);
        return resolve(&text, &mut toks, ctx, base);
    }
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
) {
    if !pending.is_empty() {
        let span = plain_start.take().map(|s| Span(s, plain_end));
        out.push(Inline::Plain {
            text: std::mem::take(pending),
            span,
        });
    } else {
        plain_start.take();
    }
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

/// Port of mldoc `take_while1_include_backslash`
/// (`lib/parsers.ml:236-248`).
fn take_while1_include_backslash(
    s: &str,
    mut i: usize,
    chars_can_escape: &[u8],
    mut pred: impl FnMut(u8) -> bool,
) -> Option<usize> {
    let bb = s.as_bytes();
    let start = i;
    let mut last_backslash = false;
    while i < bb.len() {
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
    }
    if i > start {
        crate::metrics::scan_work(i - start);
        Some(i)
    } else {
        None
    }
}

fn push_plain_node(out: &mut Vec<Inline>, text: &str, start: usize, end: usize, base: usize) {
    let text = if text.as_bytes().contains(&b'\r') {
        text.replace('\r', "\n")
    } else {
        text.to_string()
    };
    out.push(Inline::Plain {
        text,
        span: Some(Span(base + start, base + end)),
    });
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

    let parse_non_ws = |i: usize,
                        body: &mut Vec<Inline>,
                        char_before_pattern: &mut Option<u8>,
                        backoff: &mut Option<EmBackoffCandidate>|
     -> Option<usize> {
        let stop_chars = |c: u8| c == pattern_c || mldoc_whitespace_char(c);
        let escape_chars = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c];
        if let Some(end) = take_while1_include_backslash(s, i, &escape_chars, |c| !stop_chars(c)) {
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
) -> Result<EmParsed, EmFail> {
    let Some(&ch) = s.as_bytes().get(at) else {
        return Err(EmFail::NotMatch);
    };
    let mut parse = |pattern: &str, typ: &str, k: usize, lookahead: bool| {
        let cls = class_idx(ch);
        if no_closer[cls][k - 1] {
            return Err(EmFail::NotMatch);
        }
        match org_md_em_parser_at(s, at, pattern, typ, base) {
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
) -> Result<EmParsed, EmFail> {
    let mut hit = org_emphasis_at(s, at, state_char, no_closer, base)?;
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
            for child in children {
                match child {
                    Inline::Plain {
                        text,
                        span: plain_span,
                    } => {
                        match parse_nested_plain_org(&text, plain_span.map(|s| s.0).unwrap_or(0)) {
                            Ok(result)
                                if result.len() == 1
                                    && matches!(result[0], Inline::Plain { .. }) =>
                            {
                                reparsed.push(Inline::Plain {
                                    text,
                                    span: plain_span,
                                });
                            }
                            Ok(result) => {
                                reparsed.extend(result.into_iter().map(aux_nested_emphasis_org));
                            }
                            Err(()) => reparsed.push(Inline::Plain {
                                text,
                                span: plain_span,
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
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'/' | b'+' | b'^') {
            if let Ok(hit) = org_emphasis_at(text, i, None, &mut no_closer, base) {
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
            if let Some((node, end)) = try_nested_link_or_link_org(text, bb, i, base) {
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
) -> Option<(Inline, usize)> {
    if s[at..].starts_with("[[") {
        if let Some((end, node)) = org_link_1_at(s, bb, at, base) {
            return Some((node, end));
        }
        if let Some((end, content)) = crate::inline::parse_nested_link(s, at) {
            return Some((
                Inline::NestedLink {
                    content,
                    span: Some(Span(base + at, base + end)),
                },
                end,
            ));
        }
        if let Some((end, node)) = org_link_2_at(s, bb, at, base) {
            return Some((node, end));
        }
    }
    None
}

fn concat_plains_without_pos(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for node in nodes {
        match (out.last_mut(), node) {
            (
                Some(Inline::Plain {
                    text: prev,
                    span: prev_span,
                }),
                Inline::Plain { text, span },
            ) => {
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
    // ABSOLUTE start (None once a `\`-transform makes it non-1:1), `plain_end` the end.
    let mut plain_start: Option<usize> = None;
    let mut plain_end: usize = 0;
    let mut no_closer = [[false; 2]; 5];

    // Bracket-pairing maps (shared with md; computed once when `[` is present) + monotone
    // closer cursors (the v1 `seq_present`/`has_rbracket`/`next_real_dbl`/`next_crlf` floors,
    // expressed as forward cursors — keep the gated `[`×n / `{{ `×n / `(( `×n runs linear).
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
            }
            if plain_start.is_some() {
                plain_end = base + $off + $len;
            }
        }};
    }
    macro_rules! append {
        ($off:expr, $seg:expr) => {{
            let seg: &str = $seg;
            track!($off, seg.len());
            if let Some(b) = seg.bytes().next_back() {
                last_plain_char = Some(b);
            }
            pending.push_str(seg);
        }};
    }
    macro_rules! push_byte {
        ($off:expr, $c:expr) => {{
            let c: u8 = $c;
            track!($off, 1usize);
            pending.push(c as char);
            last_plain_char = Some(c);
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
                base,
                ctx,
                &mut bare_url_scan,
                &mut timestamp_scan,
            );
        }};
    }
    macro_rules! dispatch_org_text {
        ($t:ident, $off:expr, $keyword_ts:expr) => {{
            let txt = match &toks[$t].kind {
                Kind::Text(txt) => txt.as_str(),
                _ => unreachable!(),
            };
            let is_ws = txt.bytes().all(crate::inline::is_ws);
            if fresh && !is_ws {
                let leaf = (if $keyword_ts && ctx.timestamps {
                    crate::inline::parse_keyword_timestamp_with_scan(s, $off, &mut timestamp_scan)
                } else {
                    None
                })
                .or_else(|| {
                    if ctx.urls {
                        crate::inline::parse_bare_url_with_scan(s, $off, &mut bare_url_scan)
                    } else {
                        None
                    }
                });
                if let Some((end, mut node)) = leaf {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
            if let Ok(hit) = nested_emphasis_at_org(s, $off, state_char, &mut no_closer, base) {
                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                out.push(hit.node);
                fresh = true;
                $t = resync(toks, $t, hit.end);
                continue;
            }
            if (ch == b'_' || ch == b'^') && ctx.scripts {
                let braced_close = bb.get($off + 1) == Some(&b'{')
                    && script_rbrace_scan.has_before_eol(bb, $off + 2);
                if let Some((mut node, end)) = try_script(s, bb, $off, ch, braced_close, base) {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
    while t < toks.len() {
        let off = toks[t].off;
        match org_dispatch_byte(&toks[t].kind) {
            // inline.ml:1376 — `| '\n' -> breakline`
            b'\n' | b'\r' => {
                let c = match &toks[t].kind {
                    Kind::Newline(c) => *c,
                    _ => unreachable!(),
                };
                if ctx.breaks {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    out.push(Inline::Break {
                        span: Some(Span(base + off, base + off + 1)),
                    });
                } else {
                    append!(off, if c == b'\n' { "\n" } else { "\r" });
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
                    );
                    if e > off + 1 && !children.is_empty() {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            crate::projection::set_inline_span(
                                &mut node,
                                Some(Span(base + off, base + end)),
                            );
                            out.push(node);
                        }
                        Bs::Plain(text) => {
                            if let Some(b) = text.bytes().next_back() {
                                last_plain_char = Some(b);
                            }
                            let one_to_one = text.len() == end - off;
                            if one_to_one {
                                track!(off, text.len());
                            } else {
                                plain_start = None;
                            }
                            pending.push_str(&text);
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
                    append!(off, x.as_str());
                    plain_start = None;
                    fresh = false;
                }
                Kind::Leaf(node) => {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        ) {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        ctx,
                        &mut raw_html_scan,
                        &mut autolink_scan,
                        &mut timestamp_scan,
                        &mut email_scan,
                    ) {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                if ctx.code && fresh {
                    if let Some((mut node, e)) = try_code_verbatim_at(s, bb, off, b'=') {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
                    Kind::Text(_) => {
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
    flush(&mut out, &mut pending, &mut plain_start, plain_end);
    out
}

fn org_dispatch_byte(kind: &Kind) -> u8 {
    match kind {
        Kind::Text(s) => s.as_bytes().first().copied().unwrap_or(0),
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
        if j < n && bb[j] == b'}' && j > body_start {
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
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'/' | b'+' | b'^') {
            if let Ok(hit) = org_emphasis_at(text, i, None, &mut no_closer, base) {
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
            text: text.to_string(),
            span: Some(Span(base, base + text.len())),
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
    let name = &s[start..end];
    if s[end..].starts_with("{}") {
        end += 2;
    }
    match crate::entities::find(name) {
        Some(e) => Some((
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
        )),
        None => Some((
            Inline::Plain {
                text: name.to_string(),
                span: None,
            },
            end,
        )),
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
    while t < toks.len() && toks[t].off < end {
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
                return (Bs::Node(Inline::HardBreak { span: None }), i + 2)
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
            let name = s[start..j].to_string();
            if s[j..].starts_with("{}") {
                j += 2;
            }
            return match crate::entities::find(&name) {
                Some(e) => (
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
                ),
                None => (Bs::Plain(name), j),
            };
        }
    }
    match bb.get(i + 1) {
        Some(&c) if c.is_ascii_punctuation() => {
            let w = char_len(c);
            (Bs::Plain(s[i..i + 1 + w].to_string()), i + 1 + w)
        }
        _ => (Bs::Plain("\\".to_string()), i + 1),
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
    base: usize,
    ctx: Ctx,
    bare_url_scan: &mut crate::inline::BareUrlScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
) -> usize {
    let n = s.len();
    while t < toks.len()
        && (if t + 1 < toks.len() {
            toks[t + 1].off
        } else {
            n
        }) <= end
    {
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
                && crate::inline::parse_bare_url_with_scan(s, end, bare_url_scan).is_some());
        if leads {
            // Re-lex ONLY the split token's tail (Org is fully local — see fn doc), overwrite
            // `toks[t]`, re-dispatch. Because the caller straddled an ordinary Text token, the
            // tail lexes to one local Text/Punct token; the old suffix reparse fallback was
            // unreachable after C1-C7 moved entities/code to resolver-local dispatch.
            let mut retok = org_lex(&s[end..te]);
            debug_assert!(
                retok.len() == 1 && matches!(retok[0].kind, Kind::Text(_) | Kind::Punct(_))
            );
            crate::metrics::scan_work(te - end); // O(1): ONLY the split token re-lexed
            retok[0].off += end; // local → absolute
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
        *plain_start = Some(base + end);
        *plain_end = base + te;
        *fresh = !tail.is_empty() && tail.bytes().all(crate::inline::is_ws);
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
    if j > start && j < n && bb[j] == marker {
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
fn try_target_angle_at(
    s: &str,
    bb: &[u8],
    i: usize,
    ctx: Ctx,
    raw_html_scan: &mut crate::block_common::RawHtmlScan,
    autolink_scan: &mut crate::inline::AutolinkScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
    email_scan: &mut crate::inline::EmailAutolinkScan,
) -> Option<(Inline, usize)> {
    let n = bb.len();
    if crate::inline::autolink_has_closing_boundary(s, i, autolink_scan) {
        if let Some((end, node)) = crate::inline::parse_quick_link(s, i) {
            return Some((node, end));
        }
    }
    if s[i..].starts_with("<<") {
        let inner_start = i + 2;
        let mut j = inner_start;
        while j < n {
            let c = bb[j];
            if c == b'>' || c == b'\n' || c == b'\r' {
                break;
            }
            j += char_len(c);
        }
        if j > inner_start && j + 1 < n && bb[j] == b'>' && bb[j + 1] == b'>' {
            return Some((
                Inline::Target {
                    text: s[inner_start..j].to_string(),
                    span: None,
                },
                j + 2,
            ));
        }
    }
    if s[i..].starts_with("<<<") {
        let inner_start = i + 3;
        let mut j = inner_start;
        while j < n {
            let c = bb[j];
            if c == b'>' || c == b'\n' || c == b'\r' {
                break;
            }
            j += char_len(c);
        }
        if j > inner_start && j + 2 < n && bb[j] == b'>' && bb[j + 1] == b'>' && bb[j + 2] == b'>' {
            return Some((
                Inline::Target {
                    text: s[inner_start..j].to_string(),
                    span: None,
                },
                j + 3,
            ));
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
                text: s[i..extent.end].to_string(),
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
    if j == inner_start {
        return None;
    }
    if j + 1 < n && bb[j] == b')' && bb[j + 1] == b')' {
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
) -> Option<(Inline, usize)> {
    if s[off..].starts_with("[[") {
        if rb_lb_present {
            if let Some((end, node)) = org_link_1_at(s, bb, off, base) {
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
        while real_dbl.get(*real_dbl_cur).is_some_and(|&p| p < off + 2) {
            *real_dbl_cur += 1;
        }
        if let Some(&d) = real_dbl.get(*real_dbl_cur) {
            if off > *crlf {
                *crlf = first_crlf(bb, off);
            }
            if d > off + 2 && *crlf > d {
                if let Some((end, node)) = org_link_2_at(s, bb, off, base) {
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
        if let Some((end, name)) = org_footnote_at(s, off) {
            return Some((Inline::Fnref { name, span: None }, end));
        }
    }
    if let Some((end, node)) = crate::inline::parse_statistics_cookie(s, off) {
        return Some((node, end));
    }
    if ctx.hiccup && bb.get(off + 1) == Some(&b':') && crate::inline::hiccup_head_ok(s, off) {
        if let Some(end) = hiccup_close.get(off).copied().filter(|&e| e != usize::MAX) {
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
fn org_link_1_at(s: &str, bb: &[u8], at: usize, base: usize) -> Option<(usize, Inline)> {
    let url_start = at + 2;
    let j = crate::inline::take_while1_include_backslash_len(s, url_start, b"]", |c| c != b']')?;
    if !s[j..].starts_with("][") {
        return None;
    }
    let url_text = s[url_start..j].to_string();
    let label_start = j + 2;
    let close = find_org_label_end(bb, label_start)?;
    let label_text = s[label_start..close].to_string();
    let mut end = close + 2;
    let metadata = read_metadata(s, bb, &mut end);
    let url = crate::org::classify_org_link_1(&url_text, &label_text);
    // label_text is a raw slice of `s` starting at `label_start` → children index off that.
    let label = parse_ctx(&label_text, Ctx::label(), base + label_start);
    let label_first = match label.first() {
        Some(Inline::Plain { text, .. }) => text.clone(),
        _ => String::new(),
    };
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

/// `[[url]]` — v1 org_link_2 (single `]` allowed, non-empty, no eol).
fn org_link_2_at(s: &str, bb: &[u8], at: usize, base: usize) -> Option<(usize, Inline)> {
    let n = bb.len();
    let name_start = at + 2;
    let mut j = name_start;
    while j < n {
        let c = bb[j];
        if c == b'\n' || c == b'\r' {
            return None;
        }
        if c == b'\\' && j + 1 < n {
            j += 1 + char_len(bb[j + 1]);
            continue;
        }
        if c == b']' {
            if j + 1 < n && bb[j + 1] == b']' {
                break;
            }
            j += 1;
            continue;
        }
        j += char_len(c);
    }
    if j + 1 >= n || bb[j] != b']' || bb[j + 1] != b']' || j == name_start {
        return None;
    }
    let name = s[name_start..j].to_string();
    let url = crate::org::classify_org_link_2(&name);
    let full = format!("[[{}]]", name);
    // the synthetic label (== name) is a raw slice of `s` at `name_start` → span it.
    let label = match &url {
        crate::projection::Url::PageRef { .. } => vec![],
        _ => vec![Inline::Plain {
            text: name.clone(),
            span: Some(Span(base + name_start, base + j)),
        }],
    };
    // span set by the caller over [at, j + 2).
    Some((
        j + 2,
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
fn find_org_label_end(bb: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bb.len() {
        if bb[j] == b']' && bb.get(j + 1) == Some(&b']') {
            return Some(j);
        }
        if let Some(end) = take_org_label_plain(bb, j) {
            j = end;
            continue;
        }
        match bb[j] {
            b'[' => {
                j = org_balanced_label_chunk(bb, j);
            }
            b']' => {
                j += 1;
            }
            _ => {
                j += char_len(bb[j]);
            }
        }
    }
    None
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
    (j > at).then_some(j)
}

fn org_balanced_label_chunk(bb: &[u8], at: usize) -> usize {
    let mut j = at;
    let mut depth = 0usize;
    while j < bb.len() {
        match bb[j] {
            b'\\' => {
                j += 1;
                if j < bb.len() {
                    let next = bb[j];
                    if matches!(next, b'[' | b']') || !matches!(next, b'[' | b']') {
                        j += char_len(next);
                    }
                }
            }
            b'[' => {
                depth += 1;
                j += 1;
            }
            b']' if depth == 0 => break,
            b']' => {
                depth -= 1;
                j += 1;
                if depth == 0 {
                    break;
                }
            }
            _ => j += char_len(bb[j]),
        }
    }
    j
}

/// Optional `{ … }` metadata after a link; advances `end` and returns it (incl. braces) or "".
fn read_metadata(s: &str, bb: &[u8], end: &mut usize) -> String {
    if bb.get(*end) == Some(&b'{') {
        if let Some(close) = crate::inline::find_sub_line(bb, *end + 1, b"}") {
            let meta = s[*end..close + 1].to_string();
            *end = close + 1;
            return meta;
        }
    }
    String::new()
}

/// `[fn:name]` / `[fn:name:def]` / `[fn::def]` → name — v1 org_footnote_ref.
fn org_footnote_at(s: &str, i: usize) -> Option<(usize, String)> {
    let rest = s[i..].strip_prefix("[fn:")?;
    if let Some(def) = rest.strip_prefix(':') {
        let close = def.find(']')?;
        if close == 0 || def[..close].contains('\n') || def[..close].contains('\r') {
            return None;
        }
        return Some((i + 5 + close + 1, String::new()));
    }
    let rb = rest.as_bytes();
    let mut j = 0;
    while j < rb.len() && rb[j] != b':' && rb[j] != b']' && rb[j] != b'\n' && rb[j] != b'\r' {
        j += 1;
    }
    if j == 0 {
        return None;
    }
    let name = rest[..j].to_string();
    let after = &rest[j..];
    if after.starts_with(':') {
        let def = &after[1..];
        let close = def.find(']')?;
        if def[..close].contains('\n') || def[..close].contains('\r') {
            return None;
        }
        Some((i + 4 + j + 1 + close + 1, name))
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

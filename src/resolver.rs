//! lsdoc inline resolver (v0.2) — ONE ctx-aware pass over the lexer's tokens → `Vec<Inline>`.
//!
//! Byte-offset-driven and leftmost-greedy: walks the token stream once, applying context and
//! pairing emphasis/brackets. Byte-exact to mldoc, validated over the differential harness
//! gate (`harness/run.mjs`: 1039-input corpus + inlinegate).
//!
//! **M0** core (text/break/escape/entity/code). **M1** emphasis ports mldoc's
//! `md_em_parser` dispatch/body combinators plus phase-2 `nested_emphasis` reparsing.
//! It is NOT a CommonMark backward `openers_bottom` stack. Linear via bounded body
//! consumption plus a per-(marker,len) `no_closer` forward floor.

use crate::lexer::{lex, Kind, Token};
use crate::projection::{Inline, Span};
use crate::source_map::OriginSegment;

/// Active constructs (mirrors v1's `Ctx`; grows as families migrate). Page-ref / nested-link
/// / md-link / code / emphasis / escapes are ALWAYS on (no flag); these gate the constructs
/// mldoc's `Ctx::emph` disables.
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
    /// mldoc inline state is only supplied to top-level Markdown `_` emphasis.
    pub use_state: bool,
    /// Whether a `\n` is a `Break` node (true) or literal text (false — emphasis content).
    pub breaks: bool,
    pub hiccup: bool,
    pub footnotes: bool,
    pub images: bool,
    pub latex: bool,
    pub tags: bool,
    pub macros: bool,
    pub export_snippets: bool,
    pub block_refs: bool,
    pub urls: bool,
    pub timestamps: bool,
    pub autolinks: bool,
    pub html: bool,
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx {
            use_state: true,
            breaks: true,
            hiccup: true,
            footnotes: true,
            images: true,
            latex: true,
            tags: true,
            macros: true,
            export_snippets: true,
            block_refs: true,
            urls: true,
            timestamps: true,
            autolinks: true,
            html: true,
        }
    }
}

/// Parse a run of inline markup (top-level Markdown context). `base` is the absolute byte
/// offset of `text[0]` in the block body — every emitted node's `span` is absolute (S2).
pub(crate) fn parse_inline(text: &str, base: usize) -> Vec<Inline> {
    parse_ctx(text, Ctx::top(), base)
}

/// Markdown link-label Plain chunk reparse, porting
/// `syntax/inline.ml:862-884`: `many1 (choice [emphasis; latex_fragment; entity;
/// code; subscript; superscript])` with `consume:All`. This deliberately has no
/// `plain` or whitespace fallback; callers keep the original Plain on `None`.
pub(crate) fn parse_inline_ctx_md_label(text: &str, base: usize) -> Option<Vec<Inline>> {
    let bb = text.as_bytes();
    if bb.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut no_closer = [[false; 3]; 5];
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'~' | b'^' | b'=') {
            if let Ok(hit) = nested_emphasis_at_md(text, i, None, &mut no_closer, base) {
                out.push(hit.node);
                i = hit.end;
                continue;
            }
        }
        if let Some((node, end)) = markdown_label_latex_at(text, bb, i, base) {
            out.push(node);
            i = end;
            continue;
        }
        if let Some((node, end)) = markdown_label_entity_at(text, bb, i, base) {
            out.push(node);
            i = end;
            continue;
        }
        if let Some((node, end)) = try_code_span(text, i, base) {
            out.push(node);
            i = end;
            continue;
        }
        if matches!(bb[i], b'_' | b'^') {
            if bb.get(i + 1) == Some(&b'{') && script_rbrace_scan.has_before_eol(bb, i + 2) {
                if let Some((node, end)) = try_markdown_script_at(text, bb, i, base) {
                    out.push(node);
                    i = end;
                    continue;
                }
            }
        }
        return None;
    }
    Some(concat_plains_without_pos(out))
}

fn markdown_label_latex_at(s: &str, bb: &[u8], i: usize, base: usize) -> Option<(Inline, usize)> {
    let (mut node, end) = if bb.get(i) == Some(&b'$') {
        crate::inline::parse_latex_dollar_at(s, i)?
    } else if bb.get(i) == Some(&b'\\') && matches!(bb.get(i + 1), Some(b'(' | b'[')) {
        crate::inline::parse_latex_backslash_at(s, i)?
    } else {
        return None;
    };
    crate::projection::set_inline_span(&mut node, Some(Span(base + i, base + end)));
    Some((node, end))
}

fn markdown_label_entity_at(s: &str, bb: &[u8], i: usize, base: usize) -> Option<(Inline, usize)> {
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
    let e = crate::entities::find(name)?;
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

fn parse_ctx(text: &str, ctx: Ctx, base: usize) -> Vec<Inline> {
    let mut toks = lex(text);
    resolve(text, &mut toks, ctx, base)
}

fn class_idx(ch: u8) -> usize {
    match ch {
        b'*' => 0,
        b'_' => 1,
        b'~' => 2,
        b'^' => 3,
        _ => 4, // '='
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
    closer_start: usize,
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
        i += char_len_at(bb, i);
    }
    if i > start {
        crate::metrics::scan_work(i - start);
        Some(i)
    } else {
        None
    }
}

#[inline]
fn char_len_at(bb: &[u8], i: usize) -> usize {
    crate::inline::char_len(bb[i])
}

fn push_plain_node(out: &mut Vec<Inline>, text: &str, start: usize, end: usize, base: usize) {
    let raw = text;
    let (text, clean) = normalize_cr_plain_text(raw);
    if clean {
        out.push(Inline::Plain {
            text,
            span: Some(Span(base + start, base + end)),
            span_map: None,
        });
    } else {
        out.push(crate::source_map::make_plain(
            text,
            Span(base + start, base + end),
            cr_plain_origins(raw, base + start),
            raw,
            base + start,
        ));
    }
}

fn normalize_cr_plain_text(text: &str) -> (String, bool) {
    if text.as_bytes().contains(&b'\r') {
        (text.replace('\r', "\n"), false)
    } else {
        (text.to_string(), true)
    }
}

fn markdown_plain_text(text: &str) -> (String, bool) {
    crate::metrics::scan_work(text.len()); // A1: one pass over this (disjoint) plain slice
    let bb = text.as_bytes();
    if !bb.contains(&b'\\') && !bb.contains(&b'\r') {
        return (text.to_string(), true);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut clean = true;
    while i < bb.len() {
        if bb[i] == b'\r' {
            out.push('\n');
            clean = false;
            i += 1;
        } else if bb[i] == b'\\' && bb.get(i + 1).is_some_and(|c| c.is_ascii_punctuation()) {
            let next = i + 1;
            let end = next + char_len_at(bb, next);
            out.push_str(&text[next..end]);
            clean = false;
            i = end;
        } else {
            let end = i + char_len_at(bb, i);
            out.push_str(&text[i..end]);
            i = end;
        }
    }
    (out, clean)
}

fn cr_plain_origins(raw: &str, base: usize) -> Vec<OriginSegment> {
    crate::metrics::scan_work(raw.len()); // A1: one pass over this (disjoint) plain slice
    let bb = raw.as_bytes();
    let mut origins = Vec::new();
    let mut text_off = 0usize;
    let mut i = 0usize;
    while i < bb.len() {
        let len = char_len_at(bb, i);
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

fn markdown_plain_origins(raw: &str, base: usize) -> Vec<OriginSegment> {
    let bb = raw.as_bytes();
    let mut origins = Vec::new();
    let mut text_off = 0usize;
    let mut i = 0usize;
    while i < bb.len() {
        if bb[i] == b'\\' && bb.get(i + 1).is_some_and(|c| c.is_ascii_punctuation()) {
            let next = i + 1;
            let len = char_len_at(bb, next);
            origins.push(OriginSegment::new(text_off, base + next, len, len));
            text_off += len;
            i = next + len;
        } else {
            let len = char_len_at(bb, i);
            origins.push(OriginSegment::new(text_off, base + i, len, len));
            text_off += if bb[i] == b'\r' { 1 } else { len };
            i += len;
        }
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

/// Port of mldoc `md_em_parser` (`lib/syntax/inline.ml:298-374`).
#[allow(clippy::too_many_arguments)]
fn md_em_parser_at(
    s: &str,
    at: usize,
    pattern: &str,
    typ: &str,
    nested: bool,
    include_md_code: bool,
    base: usize,
) -> Result<EmParsed, EmFail> {
    let bb = s.as_bytes();
    let pat = pattern.as_bytes();
    let pattern_c = pat[0];
    if !is_left_flanking_delimiter_run(s, at, pat) {
        return Err(EmFail::NotMatch);
    }

    let content_start = at + pat.len();
    let mut i = content_start;
    let mut body: Vec<Inline> = Vec::new();
    let mut char_before_pattern: Option<u8> = None;
    let mut saw_non_ws = false;
    let mut backoff: Option<EmBackoffCandidate> = None;

    let close_at = |closer_start: usize, body: Vec<Inline>| {
        let close_end = closer_start + pat.len();
        let children = concat_plains_without_pos(body);
        let full = Some(Span(base + at, base + close_end));
        let node = if nested {
            Inline::Emphasis {
                emph: "Italic".to_string(),
                children: vec![Inline::Emphasis {
                    emph: "Bold".to_string(),
                    children,
                    span: full,
                }],
                span: full,
            }
        } else {
            Inline::Emphasis {
                emph: typ.to_string(),
                children,
                span: full,
            }
        };
        EmParsed {
            node,
            end: close_end,
            closer_start,
        }
    };

    let parse_non_ws = |i: usize,
                        body: &mut Vec<Inline>,
                        char_before_pattern: &mut Option<u8>,
                        backoff: &mut Option<EmBackoffCandidate>|
     -> Option<usize> {
        let stop_chars = |c: u8| c == pattern_c || mldoc_whitespace_char(c);
        let stop_chars_with_code =
            |c: u8| c == pattern_c || (include_md_code && c == b'`') || mldoc_whitespace_char(c);

        // Alternative 1: non-whitespace run, stopping at pattern/whitespace/code.
        let escape_chars_with_code = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c, b'`'];
        let escape_chars_without_code = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c];
        let escape_chars = if include_md_code {
            &escape_chars_with_code[..]
        } else {
            &escape_chars_without_code[..]
        };
        if let Some(end) =
            take_while1_include_backslash(s, i, escape_chars, |c| !stop_chars_with_code(c))
        {
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            *backoff = None;
            return Some(end);
        }

        // Alternative 2: Markdown inline code has precedence inside md emphasis.
        if include_md_code && bb.get(i) == Some(&b'`') {
            if let Some((mut node, end)) = crate::lexer::code_span(s, i) {
                if let Inline::Code { text, .. } = &mut node {
                    if text.as_bytes().contains(&b'\r') {
                        *text = text.replace('\r', "\n");
                    }
                }
                crate::projection::set_inline_span(&mut node, Some(Span(base + i, base + end)));
                set_char_before_pattern_from_node(&node, char_before_pattern);
                body.push(node);
                *backoff = None;
                return Some(end);
            }
        }

        // Alternative 3: non-whitespace run, allowing invalid backticks as plain.
        let escape_chars = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c];
        if let Some(end) = take_while1_include_backslash(s, i, &escape_chars, |c| !stop_chars(c)) {
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            *backoff = None;
            return Some(end);
        }

        // Alternative 4: not the same full pattern, so consume one Angstrom `any_char`.
        if bb.get(i..i + pat.len()) != Some(pat) {
            if i < bb.len() {
                let preserves_backoff = bb[i] == pattern_c;
                let end = i + char_len_at(bb, i);
                push_plain_node(body, &s[i..end], i, end, base);
                set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
                if !preserves_backoff {
                    *backoff = None;
                }
                return Some(end);
            }
            return None;
        }

        // Alternative 5: pattern-as-plain.
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

/// Port of mldoc `markdown_emphasis` dispatch
/// (`lib/syntax/inline.ml:406-427`).
fn markdown_emphasis_at(
    s: &str,
    at: usize,
    state_char: Option<u8>,
    no_closer: &mut [[bool; 3]; 5],
    base: usize,
) -> Result<EmParsed, EmFail> {
    let Some(&ch) = s.as_bytes().get(at) else {
        return Err(EmFail::NotMatch);
    };
    match ch {
        b'*' => {
            for &(pattern, typ, nested, k) in &[
                ("***", "Bold", true, 3usize),
                ("**", "Bold", false, 2usize),
                ("*", "Italic", false, 1usize),
            ] {
                let cls = class_idx(ch);
                if no_closer[cls][k - 1] {
                    continue;
                }
                match md_em_parser_at(s, at, pattern, typ, nested, true, base) {
                    Ok(hit) => return Ok(hit),
                    Err(EmFail::NoCloser) => no_closer[cls][k - 1] = true,
                    Err(EmFail::NotMatch) => {}
                }
            }
            Err(EmFail::NotMatch)
        }
        b'_' => {
            if !underline_emphasis_delims_backward(state_char) {
                return Err(EmFail::NotMatch);
            }
            for &(pattern, typ, nested, k) in &[
                ("___", "Bold", true, 3usize),
                ("__", "Bold", false, 2usize),
                ("_", "Italic", false, 1usize),
            ] {
                let cls = class_idx(ch);
                if no_closer[cls][k - 1] {
                    continue;
                }
                match md_em_parser_at(s, at, pattern, typ, nested, true, base) {
                    Ok(hit) if underline_emphasis_delims_lookahead(s, hit.end) => return Ok(hit),
                    Ok(_) => {}
                    Err(EmFail::NoCloser) => no_closer[cls][k - 1] = true,
                    Err(EmFail::NotMatch) => {}
                }
            }
            Err(EmFail::NotMatch)
        }
        b'~' => {
            let cls = class_idx(ch);
            if no_closer[cls][1] {
                return Err(EmFail::NotMatch);
            }
            match md_em_parser_at(s, at, "~~", "Strike_through", false, true, base) {
                Ok(hit) => Ok(hit),
                Err(EmFail::NoCloser) => {
                    no_closer[cls][1] = true;
                    Err(EmFail::NotMatch)
                }
                Err(e) => Err(e),
            }
        }
        b'^' => {
            let cls = class_idx(ch);
            if no_closer[cls][1] {
                return Err(EmFail::NotMatch);
            }
            match md_em_parser_at(s, at, "^^", "Highlight", false, true, base) {
                Ok(hit) => Ok(hit),
                Err(EmFail::NoCloser) => {
                    no_closer[cls][1] = true;
                    Err(EmFail::NotMatch)
                }
                Err(e) => Err(e),
            }
        }
        b'=' => {
            let cls = class_idx(ch);
            if no_closer[cls][1] {
                return Err(EmFail::NotMatch);
            }
            match md_em_parser_at(s, at, "==", "Highlight", false, true, base) {
                Ok(hit) => Ok(hit),
                Err(EmFail::NoCloser) => {
                    no_closer[cls][1] = true;
                    Err(EmFail::NotMatch)
                }
                Err(e) => Err(e),
            }
        }
        _ => Err(EmFail::NotMatch),
    }
}

/// Port of mldoc `nested_emphasis` entry (`lib/syntax/inline.ml:919-954`).
fn nested_emphasis_at_md(
    s: &str,
    at: usize,
    state_char: Option<u8>,
    no_closer: &mut [[bool; 3]; 5],
    base: usize,
) -> Result<EmParsed, EmFail> {
    let mut hit = markdown_emphasis_at(s, at, state_char, no_closer, base)?;
    hit.node = aux_nested_emphasis_md(hit.node, s, base);
    Ok(hit)
}

/// Port of mldoc `nested_emphasis` / `aux_nested_emphasis`
/// (`lib/syntax/inline.ml:922-947`).
fn aux_nested_emphasis_md(node: Inline, source: &str, source_base: usize) -> Inline {
    if is_synthetic_nested_emphasis(&node) {
        return unescape_synthetic_nested_emphasis_md(node, source, source_base);
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
                        span_map: plain_map,
                    } => match parse_nested_plain_md(&text, plain_span.map(|s| s.0).unwrap_or(0)) {
                        Ok(result)
                            if result.len() == 1 && matches!(result[0], Inline::Plain { .. }) =>
                        {
                            if plain_map.is_some() {
                                reparsed.push(Inline::Plain {
                                    text,
                                    span: plain_span,
                                    span_map: plain_map,
                                });
                            } else if let Some(span) = plain_span {
                                let raw = text;
                                let (text, clean) = markdown_plain_text(&raw);
                                if clean {
                                    reparsed.push(Inline::Plain {
                                        text,
                                        span: Some(span),
                                        span_map: None,
                                    });
                                } else {
                                    reparsed.push(crate::source_map::make_plain(
                                        text,
                                        span,
                                        markdown_plain_origins(&raw, span.0),
                                        &raw,
                                        span.0,
                                    ));
                                }
                            } else {
                                reparsed.push(crate::source_map::make_plain(
                                    text,
                                    Span(0, 0),
                                    Vec::new(),
                                    "",
                                    0,
                                ));
                            }
                        }
                        Ok(mut result) => {
                            let child_base = plain_span.map(|s| s.0).unwrap_or(0);
                            if plain_span.is_none() {
                                for node in &mut result {
                                    clear_inline_spans(node);
                                }
                            }
                            reparsed.extend(
                                result
                                    .into_iter()
                                    .map(|node| aux_nested_emphasis_md(node, &text, child_base)),
                            );
                        }
                        Err(()) => reparsed.push(Inline::Plain {
                            text,
                            span: plain_span,
                            span_map: None,
                        }),
                    },
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

fn unescape_synthetic_nested_emphasis_md(
    node: Inline,
    source: &str,
    source_base: usize,
) -> Inline {
    match node {
        Inline::Emphasis {
            emph,
            mut children,
            span,
        } if emph == "Italic" && children.len() == 1 => {
            match children.pop().unwrap() {
                Inline::Emphasis {
                    emph: inner_emph,
                    children: inner_children,
                    span: inner_span,
                } if inner_emph == "Bold" => {
                    let children = inner_children
                        .into_iter()
                        .map(|node| unescape_synthetic_plain_md(node, source, source_base))
                        .collect();
                    Inline::Emphasis {
                        emph,
                        children: vec![Inline::Emphasis {
                            emph: inner_emph,
                            children: concat_plains_without_pos(children),
                            span: inner_span,
                        }],
                        span,
                    }
                }
                child => Inline::Emphasis {
                    emph,
                    children: vec![child],
                    span,
                },
            }
        }
        other => other,
    }
}

fn unescape_synthetic_plain_md(node: Inline, source: &str, source_base: usize) -> Inline {
    match node {
        Inline::Plain {
            text,
            span,
            span_map: _,
        } => {
            if let Some(span) = span {
                let raw = source_slice_for_span(source, source_base, span).unwrap_or(&text);
                let (text, clean) = markdown_plain_text(&raw);
                if clean {
                    Inline::Plain {
                        text,
                        span: Some(span),
                        span_map: None,
                    }
                } else {
                    crate::source_map::make_plain(
                        text,
                        span,
                        markdown_plain_origins(&raw, span.0),
                        &raw,
                        span.0,
                    )
                }
            } else {
                let raw = text;
                let (text, clean) = markdown_plain_text(&raw);
                if clean {
                    Inline::Plain {
                        text,
                        span: None,
                        span_map: None,
                    }
                } else {
                    crate::source_map::make_plain(text, Span(0, 0), Vec::new(), "", 0)
                }
            }
        }
        other => other,
    }
}

fn source_slice_for_span(source: &str, source_base: usize, span: Span) -> Option<&str> {
    let start = span.0.checked_sub(source_base)?;
    let end = start + (span.1 - span.0);
    source.get(start..end)
}

fn clear_inline_spans(node: &mut Inline) {
    match node {
        Inline::Plain { span, span_map, .. } => {
            *span = None;
            *span_map = None;
        }
        Inline::Code { span, .. }
        | Inline::Verbatim { span, .. }
        | Inline::Break { span }
        | Inline::HardBreak { span }
        | Inline::NestedLink { span, .. }
        | Inline::Target { span, .. }
        | Inline::Macro { span, .. }
        | Inline::ExportSnippet { span, .. }
        | Inline::Latex { span, .. }
        | Inline::Timestamp { span, .. }
        | Inline::Cookie { span, .. }
        | Inline::Fnref { span, .. }
        | Inline::InlineHtml { span, .. }
        | Inline::Email { span, .. }
        | Inline::Entity { span, .. }
        | Inline::Hiccup { span, .. } => *span = None,
        Inline::Emphasis { children, span, .. }
        | Inline::Subscript { children, span }
        | Inline::Superscript { children, span }
        | Inline::Tag { children, span } => {
            *span = None;
            for child in children {
                clear_inline_spans(child);
            }
        }
        Inline::Link { label, span, .. } => {
            *span = None;
            for child in label {
                clear_inline_spans(child);
            }
        }
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
/// (`lib/syntax/inline.ml:927-934`) for Markdown.
fn parse_nested_plain_md(text: &str, base: usize) -> Result<Vec<Inline>, ()> {
    let bb = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut no_closer = [[false; 3]; 5];
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'~' | b'^' | b'=') {
            if let Ok(hit) = markdown_emphasis_at(text, i, None, &mut no_closer, base) {
                out.push(hit.node);
                i = hit.end;
                continue;
            }
        }
        if matches!(bb[i], b'_' | b'^') {
            if bb.get(i + 1) == Some(&b'{') && script_rbrace_scan.has_before_eol(bb, i + 2) {
                if let Some((node, end)) = try_markdown_script_at(text, bb, i, base) {
                    out.push(node);
                    i = end;
                    continue;
                }
            }
        }
        if bb[i] == b'[' {
            if let Some((node, end)) = try_nested_link_or_link_md(text, i, base) {
                out.push(node);
                i = end;
                continue;
            }
        }
        let (node, end) = markdown_plain_at(text, i, base).ok_or(())?;
        out.push(node);
        i = end;
    }
    Ok(concat_plains_without_pos(out))
}

/// Port of mldoc Markdown `plain` fallback as used by `nested_emphasis`
/// (`lib/syntax/inline.ml:211-236`).
fn markdown_plain_at(s: &str, i: usize, base: usize) -> Option<(Inline, usize)> {
    let bb = s.as_bytes();
    if i >= bb.len() {
        return None;
    }
    let in_plain_delims = |c: u8| {
        matches!(
            c,
            b'\\' | b'_' | b'^' | b'[' | b'*' | b'~' | b'`' | b'=' | b'$' | b'#'
        ) || mldoc_whitespace_char(c)
    };
    if !mldoc_whitespace_char(bb[i]) && bb[i] != b'\n' && bb[i] != b'\r' && !in_plain_delims(bb[i])
    {
        let mut end = i + char_len_at(bb, i);
        while end < bb.len() && bb[end] != b'\n' && bb[end] != b'\r' && !in_plain_delims(bb[end]) {
            end += char_len_at(bb, end);
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
                let end = i + 1 + char_len_at(bb, i + 1);
                return Some((
                    crate::source_map::make_plain(
                        s[i + 1..end].to_string(),
                        Span(base + i, base + end),
                        vec![OriginSegment::new(0, base + i + 1, end - i - 1, end - i - 1)],
                        s,
                        base,
                    ),
                    end,
                ));
            }
        }
    }
    if in_plain_delims(bb[i]) {
        let end = i + char_len_at(bb, i);
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

/// Port of mldoc Markdown `nested_link_or_link`
/// (`lib/syntax/inline.ml:915-917`) for phase-2 emphasis reparsing.
fn try_nested_link_or_link_md(s: &str, at: usize, base: usize) -> Option<(Inline, usize)> {
    if s[at..].starts_with("[[") {
        if let Some((end, content)) = crate::inline::parse_nested_link(s, at) {
            return Some((
                Inline::NestedLink {
                    content,
                    span: Some(Span(base + at, base + end)),
                },
                end,
            ));
        }
        if let Some((end, name, full)) = crate::inline::parse_page_ref(s, at) {
            return Some((
                Inline::Link {
                    url: crate::projection::Url::PageRef { v: name },
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
    let (mut node, end) = crate::inline::md_link(s, at, false, base)?;
    crate::projection::set_inline_span(&mut node, Some(Span(base + at, base + end)));
    Some((node, end))
}

/// Port of mldoc `gen_script` for Markdown braced script bodies
/// (`lib/syntax/inline.ml:492-514`).
fn try_markdown_script_at(s: &str, bb: &[u8], i: usize, base: usize) -> Option<(Inline, usize)> {
    let marker = bb[i];
    if !matches!(marker, b'_' | b'^') || bb.get(i + 1) != Some(&b'{') {
        return None;
    }
    let body_start = i + 2;
    let mut j = body_start;
    while j < bb.len() && bb[j] != b'}' && bb[j] != b'\n' && bb[j] != b'\r' {
        j += char_len_at(bb, j);
    }
    if j == body_start || bb.get(j) != Some(&b'}') {
        return None;
    }
    let children = parse_markdown_script_body(&s[body_start..j], base + body_start);
    let span = Some(Span(base + i, base + j + 1));
    let node = if marker == b'_' {
        Inline::Subscript { children, span }
    } else {
        Inline::Superscript { children, span }
    };
    Some((node, j + 1))
}

/// Port of the inner parser in mldoc `gen_script`
/// (`lib/syntax/inline.ml:503-510`) for Markdown braced script bodies.
fn parse_markdown_script_body(text: &str, base: usize) -> Vec<Inline> {
    let bb = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut no_closer = [[false; 3]; 5];
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'~' | b'^' | b'=') {
            if let Ok(hit) = markdown_emphasis_at(text, i, None, &mut no_closer, base) {
                out.push(aux_nested_emphasis_md(hit.node, text, base));
                i = hit.end;
                continue;
            }
        }
        if let Some((node, end)) = markdown_plain_at(text, i, base) {
            out.push(node);
            i = end;
            continue;
        }
        if bb[i] == b'\\' {
            if let Some((node, end)) = markdown_entity_or_plain_at(text, i, base) {
                out.push(node);
                i = end;
                continue;
            }
        }
        break;
    }
    concat_plains_without_pos(out)
}

/// Port of mldoc `entity` fallback used by `gen_script`
/// (`lib/syntax/inline.ml:481-488`).
fn markdown_entity_or_plain_at(s: &str, i: usize, base: usize) -> Option<(Inline, usize)> {
    let bb = s.as_bytes();
    if bb.get(i) != Some(&b'\\') {
        return None;
    }
    let start = i + 1;
    if !bb.get(start).is_some_and(|c| c.is_ascii_alphabetic()) {
        return markdown_plain_at(s, i, base);
    }
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
            crate::source_map::make_plain(
                name.to_string(),
                Span(base + i, base + end),
                vec![OriginSegment::new(0, base + i + 1, name.len(), name.len())],
                s,
                base,
            ),
            end,
        )),
    }
}

fn concat_plains_without_pos(nodes: Vec<Inline>) -> Vec<Inline> {
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

fn last_plain_char_after_append(s: &str, last_plain_char: &mut Option<u8>) {
    if let Some(b) = s.as_bytes().last().copied() {
        *last_plain_char = Some(b);
    }
}

fn find_delim_token_containing(
    toks: &[Token],
    mut t: usize,
    start: usize,
    end: usize,
    ch: u8,
) -> Option<usize> {
    while t < toks.len() {
        match toks[t].kind {
            Kind::Delim { ch: dch, len }
                if dch == ch && toks[t].off <= start && start < toks[t].off + len =>
            {
                return Some(t);
            }
            _ if toks[t].off >= end => return None,
            _ => t += 1,
        }
    }
    None
}

/// Dispatch-time code span (Phase D): reuse the lexer's byte-exact `code_span` builder and set the
/// absolute span, so a `` ` `` is recognized at resolve time (like tags/links) instead of pre-built
/// as a multi-byte `Leaf`. A backtick consumed by a construct is then never dispatched → no straddle.
fn try_code_span(s: &str, off: usize, base: usize) -> Option<(Inline, usize)> {
    if s.as_bytes().get(off) != Some(&b'`') {
        return None;
    }
    let (mut node, end) = crate::lexer::code_span(s, off)?;
    if let Inline::Code { text, .. } = &mut node {
        if text.as_bytes().contains(&b'\r') {
            *text = text.replace('\r', "\n");
        }
    }
    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + end)));
    Some((node, end))
}

fn resolve(s: &str, toks: &mut [Token], ctx: Ctx, base: usize) -> Vec<Inline> {
    let bb = s.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    let mut last_plain_char: Option<u8> = None;
    // Span tracking for the pending plain run. `plain_start/plain_end` remain the S5 fast path.
    // `plain_extent_*` covers transformed source bytes too, and `plain_origins` records the
    // copied bytes that can become `span_map` if S5 fails.
    let mut plain_start: Option<usize> = None;
    let mut plain_end: usize = 0;
    let mut plain_extent_start: Option<usize> = None;
    let mut plain_extent_end: usize = 0;
    let mut plain_origins: Vec<OriginSegment> = Vec::new();
    // no_closer[class][k-1]: once an opener of (marker,len) finds no forward closer, every
    // later opener of that class skips the search (monotone forward floor — the mldoc
    // emphasis linearity device; NOT a CommonMark backward openers_bottom).
    let mut no_closer = [[false; 3]; 5];

    // Bracket-pairing disciplines (KEPT — Goal 3): nested-link escape-FREE balance, page-ref
    // escape-AWARE real `]]`. Computed once; consulted by the [[…]] dispatch in O(1). `crlf`
    // is the monotone next-`\n`/`\r` (page-ref eol boundary).
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
    let lbp = if has_brk {
        seq_positions(bb, b']', b'(')
    } else {
        Vec::new()
    };
    let mut real_dbl_cur = 0usize;
    let mut lbp_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);
    let mut rparen = first_byte(bb, 0, b')');
    // Caller-owned raw-HTML miss cache: a `<tag>`×n run with no closer stays linear.
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
    let mut sq_rr = first_seq(bb, b')', b')', 0);
    let mut sq_rbrace = first_seq(bb, b'}', b'}', 0);
    let mut sq_at = first_seq(bb, b'@', b'@', 0);
    let mut block_rparen = first_byte(bb, 0, b')');
    let mut macro_rbrace = first_byte(bb, 0, b'}');
    // monotone next-`\)` / `\]` (latex-backslash closer floors: a `\(`×n run stays linear).
    let mut bs_paren = first_seq(bb, b'\\', b')', 0);
    let mut bs_brack = first_seq(bb, b'\\', b']', 0);
    let mut dollar_scan = crate::inline::ByteBeforeEolScan::new(b'$');
    let mut script_rbrace_scan = crate::inline::ByteBeforeEolScan::new(b'}');

    let mut fresh = true;
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
            plain_origins.push(OriginSegment::new(
                pending.len(),
                base + $off,
                $len,
                $len,
            ));
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
            crate::metrics::scan_work(txt.len()); // A1: charge copied pending bytes (O(n))
            pending.push_str(txt);
            last_plain_char_after_append(txt, &mut last_plain_char);
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
    macro_rules! append_text {
        ($off:expr, $txt:expr) => {{
            let txt: &str = $txt;
            track!($off, txt.len());
            crate::metrics::scan_work(txt.len()); // A1: charge copied pending bytes (O(n))
            pending.push_str(txt);
            last_plain_char_after_append(txt, &mut last_plain_char);
        }};
    }
    macro_rules! resync_here {
        ($t:ident, $end:expr) => {{
            $t = resync(
                s,
                toks,
                $t,
                $end,
                &mut out,
                &mut pending,
                &mut fresh,
                ctx,
                &mut plain_start,
                &mut plain_end,
                &mut plain_extent_start,
                &mut plain_extent_end,
                &mut plain_origins,
                base,
                &mut bare_url_scan,
                &mut timestamp_scan,
            );
        }};
    }
    macro_rules! dispatch_text {
        ($t:ident, $off:expr, $keyword_ts:expr) => {{
            let txt = match &toks[$t].kind {
                Kind::Text(x) => x.as_str(),
                _ => unreachable!(),
            };
            if fresh {
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
                if let Some((e, mut node)) = leaf {
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
                    crate::projection::set_inline_span(
                        &mut node,
                        Some(Span(base + $off, base + e)),
                    );
                    out.push(node);
                    resync_here!($t, e);
                    continue;
                }
            }
            append_text!($off, txt);
            fresh = trailing_dispatch_ws(txt) > 0;
            $t += 1;
            continue;
        }};
    }
    macro_rules! dispatch_swallow_byte {
        ($t:ident, $off:expr, $c:expr) => {{
            push_byte!($off, $c);
            fresh = false;
            $t += 1;
            continue;
        }};
    }
    macro_rules! dispatch_markdown_delim {
        ($t:ident, $off:expr) => {{
            let (ch, len) = match &toks[$t].kind {
                Kind::Delim { ch, len } => (*ch, *len),
                _ => unreachable!(),
            };
            let state_char = if ctx.use_state {
                pending.as_bytes().last().copied().or(last_plain_char)
            } else {
                None
            };
            if let Ok(hit) = nested_emphasis_at_md(s, $off, state_char, &mut no_closer, base) {
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
                out.push(hit.node);
                if let Some(closer_t) =
                    find_delim_token_containing(toks, $t, hit.closer_start, hit.end, ch)
                {
                    let closer_end = match toks[closer_t].kind {
                        Kind::Delim { len, .. } => toks[closer_t].off + len,
                        _ => unreachable!(),
                    };
                    if closer_end > hit.end {
                        toks[closer_t] = Token {
                            off: hit.end,
                            kind: Kind::Delim {
                                ch,
                                len: closer_end - hit.end,
                            },
                        };
                        $t = closer_t;
                    } else {
                        $t = closer_t + 1;
                    }
                } else {
                    resync_here!($t, hit.end);
                }
                fresh = true;
                continue;
            }
            if matches!(ch, b'_' | b'^')
                && bb.get($off + 1) == Some(&b'{')
                && script_rbrace_scan.has_before_eol(bb, $off + 2)
            {
                if let Some((node, end)) = try_markdown_script_at(s, bb, $off, base) {
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
                    out.push(node);
                    resync_here!($t, end);
                    fresh = true;
                    continue;
                }
            }
            push_byte!($off, ch);
            if len > 1 {
                toks[$t] = Token {
                    off: $off + 1,
                    kind: Kind::Delim { ch, len: len - 1 },
                };
            } else {
                $t += 1;
            }
            fresh = true;
            continue;
        }};
    }

    let mut t = 0usize;
    while t < toks.len() {
        let off = toks[t].off;
        match md_dispatch_byte(&toks[t].kind) {
            // inline.ml:1344 — `| '\n' -> breakline`
            b'\n' | b'\r' => {
                let c = match &toks[t].kind {
                    Kind::Newline(c) => *c,
                    _ => unreachable!(),
                };
                if ctx.breaks {
                    if let Some(hardbreak_start) = markdown_hardbreak_start(&pending) {
                        let kept_source_end =
                            plain_origin_boundary(&plain_origins, hardbreak_start, plain_extent_end);
                        if plain_start.is_some() {
                            plain_end = kept_source_end;
                        }
                        pending.truncate(hardbreak_start);
                        plain_extent_end = kept_source_end;
                        truncate_plain_origins(&mut plain_origins, pending.len());
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
                        out.push(Inline::HardBreak {
                            span: Some(Span(base + off, base + off + 1)),
                        });
                    } else {
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
                        out.push(Inline::Break {
                            span: Some(Span(base + off, base + off + 1)),
                        });
                    }
                } else if c == b'\r' {
                    append_transformed!(off, 1usize, off, "\n");
                } else {
                    push_byte!(off, b'\n');
                }
                fresh = true;
                t += 1;
            }
            // inline.ml:1345 — `| '#' -> hash_tag config`
            b'#' => {
                let mut end = None;
                if ctx.tags {
                    let (e, children) = crate::inline::parse_tag_name(
                        s,
                        off + 1,
                        true,
                        base,
                        crate::inline::TagReparse::Markdown,
                        tag_boundary_runs.as_deref(),
                    );
                    if e > off + 1 && !children.is_empty() {
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
                        out.push(Inline::Tag {
                            children,
                            span: Some(Span(base + off, base + e)),
                        });
                        end = Some(e);
                    }
                }
                if let Some(e) = end {
                    resync_here!(t, e);
                } else {
                    push_byte!(off, b'#');
                    fresh = true;
                    t += 1;
                }
            }
            // inline.ml:1346-1348 — `| '*' | '~' -> nested_emphasis config`
            b'*' | b'~' => dispatch_markdown_delim!(t, off),
            // inline.ml:1349 — `| '_' -> nested_emphasis ~state config <|> subscript config`
            b'_' => dispatch_markdown_delim!(t, off),
            // inline.ml:1350 — `| '^' -> nested_emphasis config <|> superscript config`
            b'^' => dispatch_markdown_delim!(t, off),
            // inline.ml:1351 — `| '=' -> nested_emphasis config`
            b'=' => dispatch_markdown_delim!(t, off),
            // inline.ml:1352 — `| '$' -> latex_fragment config`
            b'$' => {
                let mut end = None;
                if ctx.latex && dollar_scan.has_before_eol(bb, off + 2) {
                    if let Some((mut node, e)) = crate::inline::parse_latex_dollar_at(s, off) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        end = Some(e);
                    }
                }
                if let Some(e) = end {
                    resync_here!(t, e);
                } else {
                    push_byte!(off, b'$');
                    fresh = true;
                    t += 1;
                }
            }
            // inline.ml:1353 — `| '\\' -> latex_fragment config <|> entity`
            b'\\' => match &toks[t].kind {
                Kind::LatexBs(c) => {
                    let c = *c;
                    let mut end = None;
                    if ctx.latex {
                        let closer = if c == b'(' {
                            if off > bs_paren {
                                bs_paren = first_seq(bb, b'\\', b')', off);
                            }
                            bs_paren < bb.len()
                        } else {
                            if off > bs_brack {
                                bs_brack = first_seq(bb, b'\\', b']', off);
                            }
                            bs_brack < bb.len()
                        };
                        if closer {
                            if let Some((mut node, e)) =
                                crate::inline::parse_latex_backslash_at(s, off)
                            {
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
                                crate::projection::set_inline_span(
                                    &mut node,
                                    Some(Span(base + off, base + e)),
                                );
                                out.push(node);
                                end = Some(e);
                            }
                        }
                    }
                    if let Some(e) = end {
                        resync_here!(t, e);
                    } else {
                        let text = (c as char).to_string();
                        append_transformed!(off, 2usize, off + 1, text.as_str());
                        fresh = true;
                        t += 1;
                    }
                }
                Kind::Leaf(node) => {
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
                    t += 1;
                }
                Kind::Escape(x) => {
                    append_transformed!(off, 1usize + x.len(), off + 1, x.as_str());
                    fresh = true;
                    t += 1;
                }
                _ => unreachable!(),
            },
            // inline.ml:1354-1358 — `[` footnote/ref → nested/link → timestamp → cookie → hiccup
            b'[' => {
                let mut end = None;
                if ctx.footnotes && bb.get(off + 1) == Some(&b'^') {
                    if let Some((e, name)) = crate::inline::parse_footnote_ref(s, off) {
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
                        out.push(Inline::Fnref {
                            name,
                            span: Some(Span(base + off, base + e)),
                        });
                        end = Some(e);
                    }
                }
                if end.is_none() && s[off..].starts_with("[[") {
                    if nested_close.get(off).is_some_and(|&e| e != usize::MAX) {
                        if let Some((e, content)) = crate::inline::parse_nested_link(s, off) {
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
                            out.push(Inline::NestedLink {
                                content,
                                span: Some(Span(base + off, base + e)),
                            });
                            end = Some(e);
                        }
                    }
                    if end.is_none() {
                        while real_dbl.get(real_dbl_cur).is_some_and(|&p| p < off + 2) {
                            real_dbl_cur += 1;
                        }
                        if let Some(&d) = real_dbl.get(real_dbl_cur) {
                            if off > crlf {
                                crlf = first_crlf(bb, off);
                            }
                            if d > off + 2 && crlf > d {
                                if let Some((e, name, full)) = crate::inline::parse_page_ref(s, off)
                                {
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
                                    out.push(Inline::Link {
                                        url: crate::projection::Url::PageRef { v: name },
                                        label: vec![],
                                        full,
                                        image: false,
                                        metadata: String::new(),
                                        title: None,
                                        span: Some(Span(base + off, base + e)),
                                    });
                                    end = Some(e);
                                }
                            }
                        }
                    }
                }
                if end.is_none() {
                    if let Some((mut node, e)) = try_md_link(
                        s,
                        bb,
                        off,
                        false,
                        &lbp,
                        &mut lbp_cur,
                        &mut crlf,
                        &mut rparen,
                        base,
                    ) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        end = Some(e);
                    }
                }
                if end.is_none() && ctx.timestamps {
                    if let Some((e, mut node)) = crate::inline::parse_bracket_timestamp_with_scan(
                        s,
                        off,
                        &mut timestamp_scan,
                    ) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        end = Some(e);
                    }
                }
                if end.is_none() {
                    if let Some((e, mut node)) = crate::inline::parse_statistics_cookie(s, off) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        end = Some(e);
                    }
                }
                if end.is_none()
                    && ctx.hiccup
                    && bb.get(off + 1) == Some(&b':')
                    && crate::inline::hiccup_head_ok(s, off)
                {
                    if let Some(e) = hiccup_close.get(off).copied().filter(|&e| e != usize::MAX) {
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
                        out.push(Inline::Hiccup {
                            v: s[off..e].to_string(),
                            span: Some(Span(base + off, base + e)),
                        });
                        end = Some(e);
                    }
                }
                if let Some(e) = end {
                    resync_here!(t, e);
                } else {
                    push_byte!(off, b'[');
                    fresh = true;
                    t += 1;
                }
            }
            // inline.ml:1359 — `| '<' -> quick_link <|> timestamp <|> inline_html <|> email`
            b'<' => {
                if fresh && (ctx.autolinks || ctx.timestamps || ctx.html) {
                    if let Some((mut node, e)) = try_angle(
                        s,
                        off,
                        ctx,
                        &mut raw_html_scan,
                        &mut autolink_scan,
                        &mut timestamp_scan,
                        &mut email_scan,
                        base,
                    ) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        resync_here!(t, e);
                        continue;
                    }
                }
                dispatch_swallow_byte!(t, off, b'<');
            }
            // inline.ml:1360 — `| '{' -> macro config`
            b'{' => {
                if fresh
                    && ctx.macros
                    && macro_close_is_viable(bb, off, &mut sq_rbrace, &mut macro_rbrace)
                {
                    if let Some((mut node, e)) = crate::inline::parse_macro_at(s, off) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        resync_here!(t, e);
                        continue;
                    }
                }
                dispatch_swallow_byte!(t, off, b'{');
            }
            // inline.ml:1361 — `| '!' -> markdown_image config`
            b'!' => {
                if fresh && ctx.images && bb.get(off + 1) == Some(&b'[') {
                    if let Some((mut node, e)) = try_md_link(
                        s,
                        bb,
                        off + 1,
                        true,
                        &lbp,
                        &mut lbp_cur,
                        &mut crlf,
                        &mut rparen,
                        base,
                    ) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        resync_here!(t, e);
                        continue;
                    }
                }
                dispatch_swallow_byte!(t, off, b'!');
            }
            // inline.ml:1362 — `| '@' -> export_snippet`
            b'@' => {
                if fresh
                    && ctx.export_snippets
                    && export_snippet_close_is_viable(bb, off, &mut sq_at)
                {
                    if let Some((mut node, e)) = crate::inline::parse_export_snippet_at(s, off) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        resync_here!(t, e);
                        continue;
                    }
                }
                dispatch_swallow_byte!(t, off, b'@');
            }
            // inline.ml:1363 — `| '`' -> code config`
            b'`' => {
                if let Some((node, e)) = try_code_span(s, off, base) {
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
                    out.push(node);
                    resync_here!(t, e);
                } else {
                    push_byte!(off, b'`');
                    fresh = true;
                    t += 1;
                }
            }
            // inline.ml:1364-1370 — `| 'S' | 'C' | 'D' | 's' | 'c' | 'd' -> timestamp`
            b'S' | b'C' | b'D' | b's' | b'c' | b'd' => dispatch_text!(t, off, true),
            // inline.ml:1371 — `| '(' -> block_reference config`
            b'(' => {
                if fresh
                    && ctx.block_refs
                    && block_ref_close_is_viable(bb, off, &mut sq_rr, &mut block_rparen)
                {
                    if let Some((mut node, e)) = crate::inline::parse_block_ref_at(s, off) {
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
                        crate::projection::set_inline_span(
                            &mut node,
                            Some(Span(base + off, base + e)),
                        );
                        out.push(node);
                        resync_here!(t, e);
                        continue;
                    }
                }
                dispatch_swallow_byte!(t, off, b'(');
            }
            // inline.ml:1372 — `| ' ' -> Markdown_line_breaks.parse >>| Hard_Break_Line`
            b' ' | b'\t' | 0x0c => dispatch_text!(t, off, false),
            // inline.ml:1373 — `| _ -> link_inline`, then `p <|> plain` at line 1412.
            _ => {
                if let Kind::Text(_) = &toks[t].kind {
                    dispatch_text!(t, off, false);
                }
                let c = match &toks[t].kind {
                    Kind::Punct(c) => *c,
                    _ => unreachable!(),
                };
                if crate::inline_driver::markdown_swallow_byte(c) {
                    dispatch_swallow_byte!(t, off, c);
                }
                push_byte!(off, c);
                fresh = crate::inline_driver::markdown_plain_delimiter(c);
                t += 1;
            }
        }
    }
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
    out
}

/// Flush the pending plain run as a `Plain` node.
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

fn truncate_plain_origins(origins: &mut Vec<OriginSegment>, len: usize) {
    crate::metrics::scan_work(origins.len()); // A1: bounded by this node's segment count
    let mut keep = 0usize;
    while keep < origins.len() {
        let seg = origins[keep];
        if seg.text_off >= len {
            break;
        }
        if seg.text_off + seg.text_len > len {
            let kept = len - seg.text_off;
            origins[keep].text_len = kept;
            if seg.text_len == seg.src_len {
                origins[keep].src_len = kept;
            }
            keep += 1;
            break;
        }
        keep += 1;
    }
    origins.truncate(keep);
}

/// mldoc markdown hard-break dispatch rule (`inline.ml` + `markdown_line_breaks.ml`):
/// the parser is reached only when the inline dispatcher lands on a literal space, but
/// the consumed run counts `space_chars` (`' '`, tab, SUB, form feed). Leading SUB bytes
/// in the trailing run belong to the preceding plain word, so dispatch advances past
/// them before checking for the literal-space arm.
fn markdown_hardbreak_start(s: &str) -> Option<usize> {
    let bb = s.as_bytes();
    let mut start = bb.len();
    let mut scanned = 0usize;
    while start > 0 {
        scanned += 1;
        if !matches!(bb[start - 1], b' ' | b'\t' | 0x0c | 0x1a) {
            break;
        }
        start -= 1;
    }
    crate::metrics::scan_work(scanned);

    let mut q = start;
    let mut sub_scanned = 0usize;
    while q < bb.len() && bb[q] == 0x1a {
        q += 1;
        sub_scanned += 1;
    }
    crate::metrics::scan_work(sub_scanned);

    (q < bb.len() && bb[q] == b' ' && bb.len() - q >= 2).then_some(q)
}

fn plain_origin_boundary(origins: &[OriginSegment], len: usize, fallback: usize) -> usize {
    let mut result = fallback;
    let mut scanned = 0usize;
    for seg in origins {
        scanned += 1;
        if len < seg.text_off {
            crate::metrics::scan_work(scanned);
            return result;
        }
        if len <= seg.text_off + seg.text_len {
            crate::metrics::scan_work(scanned);
            return if seg.text_len == seg.src_len {
                seg.src_off + (len - seg.text_off).min(seg.src_len)
            } else if len == seg.text_off {
                seg.src_off
            } else {
                seg.src_off + seg.src_len
            };
        }
        result = seg.src_off + seg.src_len;
    }
    crate::metrics::scan_work(scanned);
    result
}

/// Count of trailing mldoc whitespace bytes that make the next byte a fresh
/// dispatch point. Unlike hard breaks, this includes form feed.
fn trailing_dispatch_ws(s: &str) -> usize {
    s.bytes()
        .rev()
        .take_while(|&b| matches!(b, b' ' | b'\t' | 0x0c))
        .count()
}

fn md_dispatch_byte(kind: &Kind) -> u8 {
    match kind {
        Kind::Text(s) => s.as_bytes().first().copied().unwrap_or(0),
        Kind::Newline(c) => *c,
        Kind::Leaf(_) | Kind::Escape(_) | Kind::LatexBs(_) => b'\\',
        Kind::Delim { ch, .. } | Kind::Punct(ch) => *ch,
    }
}

/// First `\n`/`\r` byte at/after `from`, or `bb.len()` (page-ref eol boundary).
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

/// After consuming a construct's byte extent `[_, end)`, advance the token cursor past it
/// (leftmost-greedy resync — interior tokens discarded). Most constructs end at a clean
/// token boundary; tag / bare-url end mid-Text (at a ws / tag-delim), so when `end` lands
/// strictly inside a straddling token, recover the tail `s[end..token_end]` and re-dispatch.
///
/// FAST PATH (Phase C/D, audit bug 2b): the outer `lex(s)` ALREADY tokenized `[end, n)`. Since
/// Phase D the md lexer has NO non-local construct a straddle can invalidate — code spans are
/// recognized LAZILY at dispatch (a backtick is a one-byte `Punct`), so a freed backtick simply
/// re-dispatches and pairs on the lazy scan. The only remaining pre-built multi-byte `Leaf` is a
/// `\name` Entity (which does not straddle from a tag). So when the straddled boundary token is
/// NOT a `Leaf`, `toks[t+1..]` IS the correct tail: re-lex ONLY the O(1) split token's tail
/// `[end, te)` → one `Punct`/`Text` token, overwrite `toks[t]`, and re-dispatch via the loop (no
/// recursion, no suffix re-lex). Escape (`#a\`), freed-backtick, plain-tail, keyword-timestamp and
/// bare-url straddles all land here → O(n), no native stack. A residual Entity `Leaf` straddle
/// (rare, non-chaining) falls through to the byte-exact RECURSE below. (The `#a\`code\`` code-leaf
/// O(n²)/SIGABRT family no longer exists — see subagent-tasks/notes/lsdoc-inline-delimstack-design.md.)
#[allow(clippy::too_many_arguments)]
fn resync(
    s: &str,
    toks: &mut [Token],
    mut t: usize,
    end: usize,
    out: &mut Vec<Inline>,
    pending: &mut String,
    fresh: &mut bool,
    ctx: Ctx,
    plain_start: &mut Option<usize>,
    plain_end: &mut usize,
    plain_extent_start: &mut Option<usize>,
    plain_extent_end: &mut usize,
    plain_origins: &mut Vec<OriginSegment>,
    base: usize,
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
        // `end` lands strictly inside a straddled token (a tag / bare-url / latex / page-ref
        // whose raw end falls mid-Text or mid-Escape — escape is CONSTRUCT-LOCAL, so the
        // token boundaries needn't align).
        let bb = s.as_bytes();
        let te = if t + 1 < toks.len() {
            toks[t + 1].off
        } else {
            n
        };
        // FAST PATH — reuse the outer tail (see the fn doc). Excludes only `Leaf` (a `\name`
        // Entity, the sole remaining pre-built multi-byte token — code spans are dispatch-time
        // since Phase D, so a freed backtick is a one-byte `Punct` that re-dispatches + pairs
        // lazily, no longer a non-local hazard).
        if !matches!(toks[t].kind, Kind::Leaf(_)) {
            let mut retok = lex(&s[end..te]);
            if retok.len() == 1 && matches!(retok[0].kind, Kind::Text(_) | Kind::Punct(_)) {
                crate::metrics::scan_work(te - end); // O(1): ONLY the split token re-lexed
                retok[0].off += end; // local → absolute
                toks[t] = retok.pop().unwrap();
                *fresh = true; // `end` is a fresh dispatch point (mldoc post-construct)
                return t; // re-dispatch the corrected token in the same loop
            }
        }
        // RECURSE (byte-exact): a `\name` Entity `Leaf` whose opener a construct consumed, or an
        // exotic multi-token tail. Rare and non-chaining (code spans are dispatch-time since D, so
        // the chaining `#a\`code\`` family is gone); kept as the byte-exact fallback.
        let recurse = matches!(toks[t].kind, Kind::Leaf(_))
            || bb.get(end).is_some_and(|&c| is_special_lead(c))
            || (ctx.timestamps
                && crate::inline::parse_keyword_timestamp_with_scan(s, end, timestamp_scan)
                    .is_some())
            || (ctx.urls
                && crate::inline::parse_bare_url_with_scan(s, end, bare_url_scan, base).is_some());
        if recurse {
            flush(
                out,
                pending,
                plain_start,
                *plain_end,
                plain_extent_start,
                *plain_extent_end,
                plain_origins,
                s,
                base,
            );
            // the remainder is re-parsed with its own absolute base `base + end`.
            crate::metrics::scan_work(s.len() - end); // resync re-lexes the whole suffix
            out.extend(parse_ctx(&s[end..], ctx, base + end));
            return toks.len(); // recursion handled the remainder — stop the outer walk
        }
        // the tail bytes are pushed RAW (no unescape) → they map 1:1 to source from `end`.
        let tail = &s[end..te];
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
        pending.push_str(tail);
        *fresh = trailing_dispatch_ws(tail) > 0;
        t += 1;
    } else {
        // clean construct end → fresh dispatch point.
        *fresh = true;
    }
    t
}

/// A byte that LEADS an inline construct opener (so a straddle tail starting here must be
/// re-dispatched, not pushed as plain). Closers `] ) } >` are excluded (they `plain_run`).
fn is_special_lead(c: u8) -> bool {
    matches!(
        c,
        b'#' | b'$'
            | b'['
            | b'('
            | b'{'
            | b'<'
            | b'!'
            | b'*'
            | b'_'
            | b'~'
            | b'^'
            | b'='
            | b'`'
            | b'\\'
            | b'@'
    )
}

/// `<…>` angle dispatch (mldoc order): quick_link → timestamp → inline_html → email.
fn try_angle(
    s: &str,
    at: usize,
    ctx: Ctx,
    raw_html_scan: &mut crate::block_common::RawHtmlScan,
    autolink_scan: &mut crate::inline::AutolinkScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
    email_scan: &mut crate::inline::EmailAutolinkScan,
    base: usize,
) -> Option<(Inline, usize)> {
    if ctx.autolinks {
        if crate::inline::autolink_has_closing_boundary(s, at, autolink_scan) {
            if let Some((e, node)) = crate::inline::parse_quick_link_md(s, at, base) {
                return Some((node, e));
            }
        }
    }
    if ctx.timestamps {
        if let Some((e, node)) =
            crate::inline::parse_angle_timestamp_with_scan(s, at, timestamp_scan)
        {
            return Some((node, e));
        }
    }
    if ctx.html {
        if let Some(extent) =
            crate::block_common::parse_raw_html_at_cached(s, at, s.len(), Some(raw_html_scan))
        {
            return Some((
                Inline::InlineHtml {
                    text: crate::block_common::raw_html_capture_text(s, at, extent.end),
                    span: None,
                },
                extent.end,
            ));
        }
    }
    if ctx.autolinks {
        if let Some((e, node)) = crate::inline::parse_email_autolink_cached(s, at, email_scan) {
            return Some((node, e));
        }
    }
    None
}

fn macro_close_is_viable(bb: &[u8], off: usize, sq_rbrace: &mut usize, rbrace: &mut usize) -> bool {
    let inner = off + 2;
    if *sq_rbrace < inner {
        *sq_rbrace = first_seq(bb, b'}', b'}', inner);
    }
    if *sq_rbrace >= bb.len() {
        return false;
    }
    if *rbrace < inner {
        *rbrace = first_byte(bb, inner, b'}');
    }
    *rbrace + 1 < bb.len() && bb[*rbrace + 1] == b'}'
}

fn export_snippet_close_is_viable(bb: &[u8], off: usize, sq_at: &mut usize) -> bool {
    let inner = off + 2;
    if *sq_at < inner {
        *sq_at = first_seq(bb, b'@', b'@', inner);
    }
    *sq_at < bb.len()
}

fn block_ref_close_is_viable(bb: &[u8], off: usize, sq_rr: &mut usize, rparen: &mut usize) -> bool {
    let inner = off + 2;
    if *sq_rr < inner {
        *sq_rr = first_seq(bb, b')', b')', inner);
    }
    if *sq_rr >= bb.len() {
        return false;
    }
    if *rparen < inner {
        *rparen = first_byte(bb, inner, b')');
    }
    *rparen + 1 < bb.len() && bb[*rparen + 1] == b')'
}

/// First byte `c` at/after `from`, or `bb.len()` if none (monotone-cursor helper).
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

/// First position of the 2-byte sequence `a b` at/after `from`, or `bb.len()` (monotone).
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

/// Sorted positions of the 2-byte sequence `a b` in `bb` (e.g. `](` for markdown links).
fn seq_positions(bb: &[u8], a: u8, b: u8) -> Vec<usize> {
    let mut v = Vec::new();
    let mut i = 0usize;
    while i + 1 < bb.len() {
        if bb[i] == a && bb[i + 1] == b {
            v.push(i);
        }
        i += 1;
    }
    crate::metrics::scan_work(bb.len());
    v
}

/// Markdown link / image at `at`: needs a `](` before the next eol (the label can't cross a
/// newline) and a closing `)` ahead — the monotone floors that make a `[`×n run linear — then
/// the v1 parser validates fully. `lbp`/`crlf`/`rparen` are monotone cursors (kept state).
#[allow(clippy::too_many_arguments)]
fn try_md_link(
    s: &str,
    bb: &[u8],
    at: usize,
    image: bool,
    lbp: &[usize],
    lbp_cur: &mut usize,
    crlf: &mut usize,
    rparen: &mut usize,
    base: usize,
) -> Option<(Inline, usize)> {
    while lbp.get(*lbp_cur).is_some_and(|&p| p < at) {
        *lbp_cur += 1;
    }
    let rb = *lbp.get(*lbp_cur)?;
    if at > *crlf {
        *crlf = first_crlf(bb, at);
    }
    if rb >= *crlf {
        return None; // the `](` is not before the next eol
    }
    if at > *rparen {
        *rparen = first_byte(bb, at, b')');
    }
    if *rparen >= bb.len() {
        return None; // no closing `)` ahead
    }
    crate::inline::md_link(s, at, image, base)
}

#[cfg(test)]
mod tests {
    use super::markdown_hardbreak_start;

    #[test]
    fn markdown_hardbreak_dispatch_truth_table() {
        let cases: &[(&str, Option<usize>)] = &[
            ("x  ", Some(1)),
            ("x \t", Some(1)),
            ("x \t ", Some(1)),
            ("x \x0c", Some(1)),
            ("x \x1a", Some(1)),
            ("x\x1a  ", Some(2)),
            ("x\t ", None),
            ("x\t  ", None),
            ("x\x0c  ", None),
            ("\t ", None),
            ("\t  ", None),
            ("\x0c  ", None),
            (" \t", Some(0)),
            ("x \x1a\t", Some(1)),
            ("\x1a  ", Some(1)),
            ("x\x1a\x1a ", None),
            ("x  ", Some(1)),
            ("x\t  ", None),
        ];

        for (input, expected) in cases {
            assert_eq!(
                markdown_hardbreak_start(input),
                *expected,
                "pending input {input:?}"
            );
        }
    }
}

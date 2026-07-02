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
            block_refs: true,
            urls: true,
            timestamps: true,
            autolinks: true,
            html: true,
        }
    }
    /// Restricted emphasis-content context (mldoc `aux_nested_emphasis`): breaks become
    /// literal; tags/macros/latex/images/hiccup/footnotes/block-refs off; links/code/
    /// emphasis on.
    fn emph() -> Ctx {
        Ctx {
            use_state: false,
            breaks: false,
            hiccup: false,
            footnotes: false,
            images: false,
            latex: false,
            tags: false,
            macros: false,
            block_refs: false,
            urls: false,
            timestamps: false,
            autolinks: false,
            html: false,
        }
    }
}

/// Parse a run of inline markup (top-level Markdown context). `base` is the absolute byte
/// offset of `text[0]` in the block body — every emitted node's `span` is absolute (S2).
pub(crate) fn parse_inline(text: &str, base: usize) -> Vec<Inline> {
    parse_ctx(text, Ctx::top(), base)
}

/// Re-parse a markdown link/image LABEL with the restricted emphasis-content context
/// (mldoc `aux_nested_emphasis`): the same `Ctx::emph()` the resolver already applies to
/// emphasis *content*. Used by `inline::reparse_label_text` so md label reparse runs on the
/// v0.2 resolver (matching how Org labels go through `org_resolver::parse_ctx(_, Ctx::label())`).
pub(crate) fn parse_inline_ctx_emph(text: &str, base: usize) -> Vec<Inline> {
    parse_ctx(text, Ctx::emph(), base)
}

fn parse_ctx(text: &str, ctx: Ctx, base: usize) -> Vec<Inline> {
    if !ctx.breaks && text.as_bytes().contains(&b'\r') {
        let text = text.replace('\r', "\n");
        let mut toks = lex(&text);
        return resolve(&text, &mut toks, ctx, base);
    }
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
    s.as_bytes().get(at).copied().map(underline_emphasis_delim).unwrap_or(true)
}

/// Port of mldoc `is_left_flanking_delimiter_run`
/// (`lib/syntax/inline.ml:295-296`).
#[inline]
fn is_left_flanking_delimiter_run(s: &str, at: usize, pattern: &[u8]) -> bool {
    let bb = s.as_bytes();
    bb.get(at..at + pattern.len()) == Some(pattern)
        && bb.get(at + pattern.len()).is_some_and(|&c| !mldoc_whitespace_char(c))
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
    let (text, clean) = normalize_cr_plain_text(text);
    out.push(Inline::Plain {
        text,
        span: clean.then_some(Span(base + start, base + end)),
    });
}

fn normalize_cr_plain_text(text: &str) -> (String, bool) {
    if text.as_bytes().contains(&b'\r') {
        (text.replace('\r', "\n"), false)
    } else {
        (text.to_string(), true)
    }
}

fn markdown_plain_text(text: &str) -> (String, bool) {
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

    let parse_non_ws = |i: usize,
                        body: &mut Vec<Inline>,
                        char_before_pattern: &mut Option<u8>|
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
        if let Some(end) = take_while1_include_backslash(
            s,
            i,
            escape_chars,
            |c| !stop_chars_with_code(c),
        ) {
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
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
                return Some(end);
            }
        }

        // Alternative 3: non-whitespace run, allowing invalid backticks as plain.
        let escape_chars = [pattern_c, b' ', b'\t', b'\n', b'\r', 0x0c];
        if let Some(end) =
            take_while1_include_backslash(s, i, &escape_chars, |c| !stop_chars(c))
        {
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            return Some(end);
        }

        // Alternative 4: not the same full pattern, so consume one Angstrom `any_char`.
        if bb.get(i..i + pat.len()) != Some(pat) {
            if i < bb.len() {
                let end = i + char_len_at(bb, i);
                push_plain_node(body, &s[i..end], i, end, base);
                set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
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
            push_plain_node(body, &s[i..end], i, end, base);
            set_char_before_pattern_from_node(body.last().unwrap(), char_before_pattern);
            return Some(end);
        }
        None
    };

    loop {
        if i >= bb.len() {
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
            let before = i;
            match parse_non_ws(i, &mut body, &mut char_before_pattern) {
                Some(end) => {
                    i = end;
                    saw_non_ws = true;
                    continue;
                }
                None if before >= bb.len() => return Err(EmFail::NoCloser),
                None if bb.get(before..before + pat.len()) == Some(pat) && saw_non_ws => {
                    let close_end = before + pat.len();
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
                        Inline::Emphasis { emph: typ.to_string(), children, span: full }
                    };
                    return Ok(EmParsed { node, end: close_end, closer_start: before });
                }
                None if bb.get(before..before + pat.len()) == Some(pat) => return Err(EmFail::NotMatch),
                None => return Err(EmFail::NoCloser),
            }
        }

        match parse_non_ws(i, &mut body, &mut char_before_pattern) {
            Some(end) => {
                i = end;
                saw_non_ws = true;
            }
            None if bb.get(i..i + pat.len()) == Some(pat) && saw_non_ws => {
                let close_end = i + pat.len();
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
                    Inline::Emphasis { emph: typ.to_string(), children, span: full }
                };
                return Ok(EmParsed { node, end: close_end, closer_start: i });
            }
            None if bb.get(i..i + pat.len()) == Some(pat) => return Err(EmFail::NotMatch),
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
    hit.node = aux_nested_emphasis_md(hit.node);
    Ok(hit)
}

/// Port of mldoc `nested_emphasis` / `aux_nested_emphasis`
/// (`lib/syntax/inline.ml:922-947`).
fn aux_nested_emphasis_md(node: Inline) -> Inline {
    if is_synthetic_nested_emphasis(&node) {
        return node;
    }
    match node {
        Inline::Emphasis { emph, children, span } => {
            let mut reparsed = Vec::new();
            for child in children {
                match child {
                    Inline::Plain { text, span: plain_span } => {
                        match parse_nested_plain_md(&text, plain_span.map(|s| s.0).unwrap_or(0)) {
                            Ok(result) if result.len() == 1 && matches!(result[0], Inline::Plain { .. }) => {
                                let (text, clean) = markdown_plain_text(&text);
                                reparsed.push(Inline::Plain {
                                    text,
                                    span: if clean { plain_span } else { None },
                                });
                            }
                            Ok(mut result) => {
                                if plain_span.is_none() {
                                    for node in &mut result {
                                        clear_inline_spans(node);
                                    }
                                }
                                reparsed.extend(result.into_iter().map(aux_nested_emphasis_md));
                            }
                            Err(()) => reparsed.push(Inline::Plain { text, span: plain_span }),
                        }
                    }
                    other => reparsed.push(other),
                }
            }
            Inline::Emphasis { emph, children: concat_plains_without_pos(reparsed), span }
        }
        other => other,
    }
}

fn clear_inline_spans(node: &mut Inline) {
    match node {
        Inline::Plain { span, .. }
        | Inline::Code { span, .. }
        | Inline::Verbatim { span, .. }
        | Inline::Break { span }
        | Inline::HardBreak { span }
        | Inline::NestedLink { span, .. }
        | Inline::Target { span, .. }
        | Inline::Macro { span, .. }
        | Inline::Latex { span, .. }
        | Inline::Timestamp { span, .. }
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
    while i < bb.len() {
        if matches!(bb[i], b'*' | b'_' | b'~' | b'^' | b'=') {
            if let Ok(hit) = markdown_emphasis_at(text, i, None, &mut no_closer, base) {
                out.push(hit.node);
                i = hit.end;
                continue;
            }
        }
        if matches!(bb[i], b'_' | b'^') {
            if let Some((node, end)) = try_markdown_script_at(text, bb, i, base) {
                out.push(node);
                i = end;
                continue;
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
        matches!(c, b'\\' | b'_' | b'^' | b'[' | b'*' | b'~' | b'`' | b'=' | b'$' | b'#')
            || mldoc_whitespace_char(c)
    };
    if !mldoc_whitespace_char(bb[i]) && bb[i] != b'\n' && bb[i] != b'\r' && !in_plain_delims(bb[i]) {
        let mut end = i + char_len_at(bb, i);
        while end < bb.len()
            && bb[end] != b'\n'
            && bb[end] != b'\r'
            && !in_plain_delims(bb[end])
        {
            end += char_len_at(bb, end);
        }
        crate::metrics::scan_work(end - i);
        return Some((
            Inline::Plain { text: s[i..end].to_string(), span: Some(Span(base + i, base + end)) },
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
            Inline::Plain { text: s[i..end].to_string(), span: Some(Span(base + i, base + end)) },
            end,
        ));
    }
    if bb[i] == b'\\' {
        if let Some(&next) = bb.get(i + 1) {
            if next.is_ascii_punctuation() {
                let end = i + 1 + char_len_at(bb, i + 1);
                return Some((
                    Inline::Plain { text: s[i + 1..end].to_string(), span: None },
                    end,
                ));
            }
        }
    }
    if in_plain_delims(bb[i]) {
        let end = i + char_len_at(bb, i);
        return Some((
            Inline::Plain { text: s[i..end].to_string(), span: Some(Span(base + i, base + end)) },
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
            return Some((Inline::NestedLink { content, span: Some(Span(base + at, base + end)) }, end));
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
                out.push(aux_nested_emphasis_md(hit.node));
                i = hit.end;
                continue;
            }
        }
        if bb[i] == b'\\' {
            if let Some((node, end)) = markdown_entity_or_plain_at(text, i, base) {
                out.push(node);
                i = end;
                continue;
            }
        }
        let Some((node, end)) = markdown_plain_at(text, i, base) else {
            break;
        };
        out.push(node);
        i = end;
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
            Inline::Plain { text: name.to_string(), span: None },
            end,
        )),
    }
}

fn concat_plains_without_pos(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for node in nodes {
        match (out.last_mut(), node) {
            (Some(Inline::Plain { text: prev, span: prev_span }), Inline::Plain { text, span }) => {
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

fn find_delim_token_containing(toks: &[Token], mut t: usize, start: usize, end: usize, ch: u8) -> Option<usize> {
    while t < toks.len() {
        match toks[t].kind {
            Kind::Delim { ch: dch, len } if dch == ch && toks[t].off <= start && start < toks[t].off + len => {
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
    // Span tracking for the pending plain run: `plain_start` is the ABSOLUTE byte offset of
    // the run's first byte (None once a `\`-transform makes it non-1:1 → S5 can't hold),
    // `plain_end` its absolute end. `flush` turns them into the `Plain.span`.
    let mut plain_start: Option<usize> = None;
    let mut plain_end: usize = 0;
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
    let real_dbl = if has_brk { crate::inline::build_real_dbl(s) } else { Vec::new() };
    let lbp = if has_brk { seq_positions(bb, b']', b'(') } else { Vec::new() };
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
    let mut block_rparen = first_byte(bb, 0, b')');
    let mut macro_rbrace = first_byte(bb, 0, b'}');
    // monotone next-`\)` / `\]` (latex-backslash closer floors: a `\(`×n run stays linear).
    let mut bs_paren = first_seq(bb, b'\\', b')', 0);
    let mut bs_brack = first_seq(bb, b'\\', b']', 0);

    // `fresh` = at a fresh dispatch point (BOL, or after ws / a marker-delim / a construct /
    // a Break). A SWALLOW opener (`! ( { <`) tries its construct only when `fresh`; mid-plain-
    // run (after ordinary non-ws text) it is swallowed as plain (mldoc `plain_run` semantics).
    let mut fresh = true;
    let mut t = 0usize;
    while t < toks.len() {
        // `[` dispatch (M2a/M2b): mldoc's try_bracket order — hiccup `[:` → footnote `[^` →
        // nested-link / page-ref `[[…]]` → markdown link `[…](…)`. Leftmost-greedy with
        // byte-offset resync; the kept pairing disciplines + monotone floors keep it linear.
        if matches!(toks[t].kind, Kind::Punct(b'[')) {
            let off = toks[t].off;
            let mut end = None;
            // 1. inline hiccup `[:tag …]` (ctx-gated — off in emphasis content).
            if ctx.hiccup && bb.get(off + 1) == Some(&b':') && crate::inline::hiccup_head_ok(s, off)
            {
                if let Some(e) = hiccup_close.get(off).copied().filter(|&e| e != usize::MAX) {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    out.push(Inline::Hiccup { v: s[off..e].to_string(), span: Some(Span(base + off, base + e)) });
                    end = Some(e);
                }
            }
            // 2. footnote `[^id]` (ctx-gated).
            if end.is_none() && ctx.footnotes && bb.get(off + 1) == Some(&b'^') {
                if let Some((e, name)) = crate::inline::parse_footnote_ref(s, off) {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    out.push(Inline::Fnref { name, span: Some(Span(base + off, base + e)) });
                    end = Some(e);
                }
            }
            // 3. nested-link (escape-free balance) then page-ref (escape-aware first `]]`).
            if end.is_none() && s[off..].starts_with("[[") {
                if nested_close.get(off).is_some_and(|&e| e != usize::MAX) {
                    if let Some((e, content)) = crate::inline::parse_nested_link(s, off) {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
                        out.push(Inline::NestedLink { content, span: Some(Span(base + off, base + e)) });
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
                            if let Some((e, name, full)) = crate::inline::parse_page_ref(s, off) {
                                flush(&mut out, &mut pending, &mut plain_start, plain_end);
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
            // 4. markdown link `[label](url)` — needs a `](` before the next eol and a `)`.
            if end.is_none() {
                if let Some((mut node, e)) =
                    try_md_link(s, bb, off, false, &lbp, &mut lbp_cur, &mut crlf, &mut rparen, base)
                {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                    out.push(node);
                    end = Some(e);
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan),
                None => {
                    if pending.is_empty() { plain_start = Some(base + off); }
                    if plain_start.is_some() { plain_end = base + off + 1; }
                    pending.push('[');
                    last_plain_char = Some(b'[');
                    t += 1;
                    fresh = true; // `[` is a marker-delim → fresh point
                }
            }
            continue;
        }

        // `$` latex / `#` tag — marker-delim openers: a single literal char on failure.
        let md_open = match &toks[t].kind {
            Kind::Punct(c @ (b'$' | b'#')) => Some(*c),
            _ => None,
        };
        if let Some(c) = md_open {
            let off = toks[t].off;
            let mut end = None;
            if c == b'$' && ctx.latex {
                if let Some((mut node, e)) = crate::inline::parse_latex_dollar_at(s, off) {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                    out.push(node);
                    end = Some(e);
                }
            } else if c == b'#' && ctx.tags {
                let (e, children) = crate::inline::parse_tag_name(
                    s,
                    off + 1,
                    true,
                    base,
                    tag_boundary_runs.as_deref(),
                );
                if e > off + 1 && !children.is_empty() {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    out.push(Inline::Tag { children, span: Some(Span(base + off, base + e)) });
                    end = Some(e);
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan),
                None => {
                    if pending.is_empty() { plain_start = Some(base + off); }
                    if plain_start.is_some() { plain_end = base + off + 1; }
                    pending.push(c as char);
                    last_plain_char = Some(c);
                    t += 1;
                    fresh = true; // `$`/`#` are marker-delims → fresh point
                }
            }
            continue;
        }

        // `` ` `` code span (Phase D) — recognized LAZILY here (was a pre-built lexer `Leaf`). On
        // success emit the Code node + `resync` past its extent (the closer `` ` `` and content are
        // consumed tokens); else the backtick is a literal marker-delim → fresh point (`` `((uuid))
        // `` → `` ` `` + block-ref). Greedy left-to-right: a backtick a construct already consumed
        // is never dispatched here, so a tag eating a `` ` `` needs no re-lex (bug 2b, code-leaf).
        if matches!(toks[t].kind, Kind::Punct(b'`')) {
            let off = toks[t].off;
            if let Some((node, e)) = try_code_span(s, off, base) {
                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                out.push(node);
                t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan);
            } else {
                if pending.is_empty() { plain_start = Some(base + off); }
                if plain_start.is_some() { plain_end = base + off + 1; }
                pending.push('`');
                last_plain_char = Some(b'`');
                t += 1;
                fresh = true; // `` ` `` is a marker-delim → fresh point
            }
            continue;
        }

        // `\(` / `\[` latex-backslash (ctx-dependent): a Latex span when `ctx.latex` and a
        // `\)`/`\]` closer exists ahead, else an escape (the `(`/`[` literal). The monotone
        // closer floor keeps a `\(`×n run linear.
        let latex_bs = match &toks[t].kind {
            Kind::LatexBs(c) => Some(*c),
            _ => None,
        };
        if let Some(c) = latex_bs {
            let off = toks[t].off;
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
                    if let Some((mut node, e)) = crate::inline::parse_latex_backslash_at(s, off) {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
                        crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                        out.push(node);
                        end = Some(e);
                    }
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan),
                None => {
                    // escape: the `\` is DROPPED, only `(`/`[` kept → the plain run is no
                    // longer 1:1 with source, so S5 can't hold for it.
                    plain_start = None;
                    pending.push(c as char);
                    last_plain_char = Some(c);
                    t += 1;
                    fresh = true;
                }
            }
            continue;
        }

        // Swallow bytes `! ( { < ] ) } >`: openers try their construct (M2b: `!` image;
        // `( { <` land in M3), then ALL fall back to a plain_run that swallows following
        // non-marker-delim bytes — so a following `!`/special isn't re-dispatched
        // (`!![a](b)` → plain `![a](b)`; `]]![a](b)` → plain `]]!` + `[a](b)`).
        let swallow = match &toks[t].kind {
            Kind::Punct(c) if is_swallow_byte(*c) => Some(*c),
            _ => None,
        };
        if let Some(c) = swallow {
            let off = toks[t].off;
            // Opener construct, only at a fresh dispatch point. `!` image, `{` macro, `(`
            // block-ref (M3); `<` angle constructs land in M3b. `] ) } >` never open.
            if fresh {
                let opened = match c {
                    b'!' if ctx.images && bb.get(off + 1) == Some(&b'[') => {
                        try_md_link(s, bb, off + 1, true, &lbp, &mut lbp_cur, &mut crlf, &mut rparen, base)
                    }
                    b'{' if ctx.macros
                        && macro_close_is_viable(bb, off, &mut sq_rbrace, &mut macro_rbrace) =>
                    {
                        crate::inline::parse_macro_at(s, off)
                    }
                    b'(' if ctx.block_refs
                        && block_ref_close_is_viable(bb, off, &mut sq_rr, &mut block_rparen) =>
                    {
                        crate::inline::parse_block_ref_at(s, off)
                    }
                    b'<' if ctx.autolinks || ctx.timestamps || ctx.html => {
                        try_angle(
                            s,
                            off,
                            ctx,
                            &mut raw_html_scan,
                            &mut autolink_scan,
                            &mut timestamp_scan,
                            &mut email_scan,
                        )
                    }
                    _ => None,
                };
                if let Some((mut node, e)) = opened {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    // span starts at the dispatch byte (`!`/`{`/`(`/`<`), which for `!` is one
                    // before the `[` that `try_md_link` was handed — the image extent includes it.
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                    out.push(node);
                    t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan);
                    continue;
                }
            }
            // not consumed (failed opener, or mid-plain-run) → render as plain; now mid-run, so
            // a following swallow byte won't be re-dispatched.
            if pending.is_empty() { plain_start = Some(base + off); }
            if plain_start.is_some() { plain_end = base + off + 1; }
            pending.push(c as char);
            last_plain_char = Some(c);
            fresh = false;
            t += 1;
            continue;
        }

        // Text — at a fresh dispatch point try the no-opener leaves (keyword timestamp then
        // bare URL), exactly where mldoc's default arm does; otherwise plain.
        if let Kind::Text(_) = &toks[t].kind {
            let off = toks[t].off;
            if fresh {
                let leaf = (if ctx.timestamps {
                    crate::inline::parse_keyword_timestamp(s, off)
                } else {
                    None
                })
                .or_else(|| {
                    if ctx.urls {
                        crate::inline::parse_bare_url_with_scan(s, off, &mut bare_url_scan)
                    } else {
                        None
                    }
                });
                if let Some((e, mut node)) = leaf {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                    out.push(node);
                    t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan);
                    continue;
                }
            }
            let txt = match &toks[t].kind {
                Kind::Text(x) => x,
                _ => unreachable!(),
            };
            let txt_len = txt.len();
            if pending.is_empty() { plain_start = Some(base + off); }
            if plain_start.is_some() { plain_end = base + off + txt_len; }
            pending.push_str(txt);
            last_plain_char_after_append(txt, &mut last_plain_char);
            fresh = trailing_ws(txt) > 0;
            t += 1;
            continue;
        }

        // Non-delimiter tokens pass straight through (Text is handled by its own block above).
        if !matches!(toks[t].kind, Kind::Delim { .. }) {
            let off = toks[t].off;
            match &toks[t].kind {
                Kind::Newline(c) => {
                    let c = *c;
                    if ctx.breaks {
                        // hard break: `\n` (not `\r`) immediately preceded by >=2 spaces/tabs
                        // in the pending run — the spaces are consumed (mldoc).
                        let tw = trailing_ws(&pending);
                        if c == b'\n' && tw >= 2 {
                            // the consumed spaces leave the plain run; drop them from its end.
                            if plain_start.is_some() { plain_end -= tw; }
                            pending.truncate(pending.len() - tw);
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            out.push(Inline::HardBreak { span: Some(Span(base + off, base + off + 1)) });
                        } else {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            out.push(Inline::Break { span: Some(Span(base + off, base + off + 1)) });
                        }
                    } else {
                        if pending.is_empty() { plain_start = Some(base + off); }
                        if plain_start.is_some() { plain_end = base + off + 1; }
                        pending.push(c as char);
                        last_plain_char = Some(c);
                    }
                    fresh = true;
                }
                // Phase D: a `Leaf` is now only a `\name` Entity (code spans are dispatched lazily
                // at the `` ` `` Punct branch above). It ends at the next token's byte offset (or EOF).
                Kind::Leaf(node) => {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    let tok_end_val = if t + 1 < toks.len() { toks[t + 1].off } else { s.len() };
                    let mut node = node.clone();
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + tok_end_val)));
                    out.push(node);
                    fresh = true;
                }
                // resolved escape / lone `\` / unknown entity letters — the position right
                // after is a fresh dispatch point in mldoc.
                Kind::Escape(x) => {
                    // the backslash is dropped from the text → S5 can't hold for this run.
                    plain_start = None;
                    pending.push_str(x.as_str());
                    last_plain_char_after_append(x, &mut last_plain_char);
                    fresh = true;
                }
                // `$`/`#` (M3 markers) render literally for now; they are marker-delims → fresh.
                Kind::Punct(c) => {
                    let c = *c;
                    if pending.is_empty() { plain_start = Some(base + off); }
                    if plain_start.is_some() { plain_end = base + off + 1; }
                    pending.push(c as char);
                    last_plain_char = Some(c);
                    fresh = true;
                }
                // Text/Delim/LatexBs are handled by dedicated blocks above.
                Kind::Text(_) | Kind::Delim { .. } | Kind::LatexBs(_) => unreachable!(),
            }
            t += 1;
            continue;
        }

        // Emphasis delimiter run.
        let (ch, len, off) = match &toks[t].kind {
            Kind::Delim { ch, len } => (*ch, *len, toks[t].off),
            _ => unreachable!(),
        };
        let state_char = if ctx.use_state {
            pending.as_bytes().last().copied().or(last_plain_char)
        } else {
            None
        };
        if let Ok(hit) = nested_emphasis_at_md(s, off, state_char, &mut no_closer, base) {
            flush(&mut out, &mut pending, &mut plain_start, plain_end);
            out.push(hit.node);
            if let Some(closer_t) = find_delim_token_containing(toks, t, hit.closer_start, hit.end, ch) {
                let closer_end = match toks[closer_t].kind {
                    Kind::Delim { len, .. } => toks[closer_t].off + len,
                    _ => unreachable!(),
                };
                if closer_end > hit.end {
                    toks[closer_t] =
                        Token { off: hit.end, kind: Kind::Delim { ch, len: closer_end - hit.end } };
                    t = closer_t;
                } else {
                    t = closer_t + 1;
                }
            } else {
                t = resync(s, toks, t, hit.end, &mut out, &mut pending, &mut fresh, ctx, &mut plain_start, &mut plain_end, base, &mut bare_url_scan);
            }
            fresh = true;
            continue;
        }
        if pending.is_empty() { plain_start = Some(base + off); }
        if plain_start.is_some() { plain_end = base + off + 1; }
        pending.push(ch as char);
        last_plain_char = Some(ch);
        if len > 1 {
            toks[t] = Token { off: off + 1, kind: Kind::Delim { ch, len: len - 1 } };
        } else {
            t += 1;
        }
        fresh = true;
    }
    flush(&mut out, &mut pending, &mut plain_start, plain_end);
    out
}

/// Flush the pending plain run as a `Plain` node. `plain_start` is the absolute byte offset
/// of the run's first byte (None if a `\`-transform in the run made the source non-1:1, so
/// S5 can't hold — the Plain then carries no span); `plain_end` is the run's absolute end.
fn flush(
    out: &mut Vec<Inline>,
    pending: &mut String,
    plain_start: &mut Option<usize>,
    plain_end: usize,
) {
    if !pending.is_empty() {
        let span = plain_start.take().map(|s| Span(s, plain_end));
        out.push(Inline::Plain { text: std::mem::take(pending), span });
    } else {
        plain_start.take();
    }
}

/// Count of trailing space/tab bytes in `s` (for hard-break detection).
fn trailing_ws(s: &str) -> usize {
    s.bytes().rev().take_while(|&b| b == b' ' || b == b'\t').count()
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
    base: usize,
    bare_url_scan: &mut crate::inline::BareUrlScan,
) -> usize {
    let n = s.len();
    while t < toks.len() && (if t + 1 < toks.len() { toks[t + 1].off } else { n }) <= end {
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        // `end` lands strictly inside a straddled token (a tag / bare-url / latex / page-ref
        // whose raw end falls mid-Text or mid-Escape — escape is CONSTRUCT-LOCAL, so the
        // token boundaries needn't align).
        let bb = s.as_bytes();
        let te = if t + 1 < toks.len() { toks[t + 1].off } else { n };
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
            || (ctx.timestamps && crate::inline::parse_keyword_timestamp(s, end).is_some())
            || (ctx.urls
                && crate::inline::parse_bare_url_with_scan(s, end, bare_url_scan).is_some());
        if recurse {
            flush(out, pending, plain_start, *plain_end);
            // the remainder is re-parsed with its own absolute base `base + end`.
            crate::metrics::scan_work(s.len() - end); // resync re-lexes the whole suffix
            out.extend(parse_ctx(&s[end..], ctx, base + end));
            return toks.len(); // recursion handled the remainder — stop the outer walk
        }
        // the tail bytes are pushed RAW (no unescape) → they map 1:1 to source from `end`.
        let tail = &s[end..te];
        *plain_start = Some(base + end);
        *plain_end = base + te;
        pending.push_str(tail);
        *fresh = trailing_ws(tail) > 0;
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
        b'#' | b'$' | b'[' | b'(' | b'{' | b'<' | b'!' | b'*' | b'_' | b'~' | b'^' | b'=' | b'`'
            | b'\\'
    )
}

/// Is `c` a SWALLOW byte — `mldoc` dispatches it but a failure runs `plain_run` (rather than
/// emitting a single literal char like a marker-delim). Openers `! ( { <` and the closers
/// `] ) } >` (which never open an inline construct at top level).
fn is_swallow_byte(c: u8) -> bool {
    matches!(c, b'!' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b']')
}

/// `<…>` angle dispatch (mldoc try_angle order): autolink → timestamp → email → inline-html.
fn try_angle(
    s: &str,
    at: usize,
    ctx: Ctx,
    raw_html_scan: &mut crate::block_common::RawHtmlScan,
    autolink_scan: &mut crate::inline::AutolinkScan,
    timestamp_scan: &mut crate::inline::TimestampCloseScan,
    email_scan: &mut crate::inline::EmailAutolinkScan,
) -> Option<(Inline, usize)> {
    if ctx.autolinks {
        if crate::inline::autolink_has_closing_boundary(s, at, autolink_scan) {
            if let Some((e, node)) = crate::inline::parse_autolink(s, at) {
                return Some((node, e));
            }
        }
    }
    if ctx.timestamps {
        if let Some((e, node)) = crate::inline::parse_angle_timestamp_with_scan(s, at, timestamp_scan) {
            return Some((node, e));
        }
    }
    if ctx.autolinks {
        if let Some((e, node)) = crate::inline::parse_email_autolink_cached(s, at, email_scan) {
            return Some((node, e));
        }
    }
    if ctx.html {
        if let Some(extent) =
            crate::block_common::parse_raw_html_at_cached(s, at, s.len(), Some(raw_html_scan))
        {
            return Some((Inline::InlineHtml { text: s[at..extent.end].to_string(), span: None }, extent.end));
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

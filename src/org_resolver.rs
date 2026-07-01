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

use crate::inline::{char_len, is_underscore_delim, is_ws, is_ws_or_nl};
use crate::lexer::{Kind, Token};
use crate::projection::{Inline, Span};

/// Active Org constructs (mirrors `crate::org::Ctx`; the 4 variants below match mldoc's
/// top / nested-emphasis / link-label / sub-superscript re-parse contexts exactly). Fields
/// are read as each construct family lands in later M6 sub-steps.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct Ctx {
    /// Backward emphasis gate active (top level only). Off in every re-parse.
    pub use_state: bool,
    pub tags: bool,
    pub block_refs: bool,
    pub macros: bool,
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
    /// Emphasis body re-parse (`nested_emphasis`): emphasis + scripts + links only.
    #[allow(dead_code)]
    fn emph() -> Ctx {
        Ctx {
            use_state: false,
            scripts: true,
            links: true,
            tags: false,
            block_refs: false,
            macros: false,
            latex: false,
            urls: false,
            timestamps: false,
            angle: false,
            code: false,
            breaks: false,
            entity: false,
            footnotes: false,
            hiccup: false,
        }
    }
    /// `[[url][label]]` label re-parse (`org_link_1`): latex/code/entity/scripts/emphasis,
    /// NO nested links, NO tags.
    #[allow(dead_code)]
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
            urls: false,
            timestamps: false,
            angle: false,
            breaks: false,
            footnotes: false,
            hiccup: false,
        }
    }
    /// Sub/superscript body re-parse (`gen_script`): emphasis + entity only.
    #[allow(dead_code)]
    fn script() -> Ctx {
        Ctx {
            use_state: false,
            entity: true,
            scripts: false,
            links: false,
            tags: false,
            block_refs: false,
            macros: false,
            latex: false,
            urls: false,
            timestamps: false,
            angle: false,
            code: false,
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
/// brackets / `$` / `#` / `<` / `{` / `(` / `!` become deferred `Punct` tokens; the resolver
/// decides per-ctx. (Org has no backtick code span.)
#[inline]
fn is_special(c: u8) -> bool {
    c == b'\\'
        || is_marker(c)
        || matches!(
            c,
            b'~' | b'=' | b'$' | b'[' | b']' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b'#' | b'!'
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
                toks.push(Token { off: pending_off, kind: Kind::Text(std::mem::take(&mut pending)) });
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
                toks.push(Token { off: i, kind: Kind::Newline(c) });
                i += 1;
            }
            b' ' | b'\t' => {
                flush!();
                let start = i;
                while i < n && is_ws(b[i]) {
                    i += 1;
                }
                toks.push(Token { off: start, kind: Kind::Text(s[start..i].to_string()) });
            }
            b'\\' => {
                // ALL of `\`-handling is ctx-gated (hard-break / latex / entity all hang off
                // `ctx.entity`, then escape) — so defer the whole thing: emit `Punct(\)` and
                // let the resolver run the ctx-aware `backslash()` on the raw bytes. (A `\X`
                // consumed mid-Text straddle is handled by `resync_straddle`; a `\#` inside a
                // tag leaves the `#` as its own `Punct` for a fresh tag dispatch.)
                flush!();
                toks.push(Token { off: i, kind: Kind::Punct(b'\\') });
                i += 1;
            }
            _ if is_marker(c) => {
                // ONE `Delim{ch,1}` token per marker BYTE — Org emphasis is byte-position based
                // (fixed k per marker; `^^` reads 2 raw bytes itself), tried at each marker, NOT
                // run-grouped like md. The resolver works off byte offsets + raw bytes.
                flush!();
                toks.push(Token { off: i, kind: Kind::Delim { ch: c, len: 1 } });
                i += 1;
            }
            _ if is_special(c) => {
                flush!();
                toks.push(Token { off: i, kind: Kind::Punct(c) });
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
        out.push(Inline::Plain { text: std::mem::take(pending), span });
    } else {
        plain_start.take();
    }
}

/// Resolver: ONE ctx-aware pass + stack over the Org tokens. M6 emphasis sub-step: real
/// emphasis (the stateful backward gate + forward gate / `continue_search`) and sub/super-
/// script; the remaining specials (`Punct`/`LatexBs`) still render literally (refined by the
/// leaf / bracket sub-steps). `last_plain_char` mirrors mldoc `push_plain`: updated on EVERY
/// plain append and PERSISTS across nodes/flush (an emphasis node does NOT reset it).
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
    let real_dbl = if has_brk { crate::inline::build_real_dbl(s) } else { Vec::new() };
    let mut real_dbl_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);
    let mut rbracket = first_byte(bb, 0, b']');
    let mut sq_rb_lb = first_seq(bb, b']', b'[', 0); // ][
    let mut sq_rr = first_seq(bb, b')', b')', 0); // ))
    let mut sq_rbrace = first_seq(bb, b'}', b'}', 0); // }}
    let mut sq_lt_sl = first_seq(bb, b'<', b'/', 0); // </
    // latex-backslash closer floors: only attempt `\(`/`\[` when a `\)`/`\]` exists ahead, so
    // a `\(`×n run (no closer) stays O(n) instead of an EOF re-scan per `\(` (mirrors resolver.rs).
    let mut bs_paren = first_seq(bb, b'\\', b')', 0); // \)
    let mut bs_brack = first_seq(bb, b'\\', b']', 0); // \]
    // `fresh` = a dispatch point (mldoc `plain_run` stops at PLAIN_DELIMS `\ _ ^ [ * / + $ #`
    // + ws/eol). The SWALLOW openers `~ = < { (` fire only when fresh; mid-plain-run they are
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

    let mut t = 0usize;
    while t < toks.len() {
        let off = toks[t].off;
        match &toks[t].kind {
            Kind::Text(txt) => {
                // default arm: keyword timestamp (S/C/D…) then bare URL at a fresh ordinary run.
                let txt = txt.clone();
                // org Text is all-ws or all-ordinary (ws is lexed separately).
                let is_ws = txt.bytes().all(|b| b == b' ' || b == b'\t');
                if fresh && !is_ws {
                    let leaf = (if ctx.timestamps && matches!(bb[off], b'S' | b'C' | b'D' | b's' | b'c' | b'd') {
                        crate::inline::parse_keyword_timestamp(s, off)
                    } else {
                        None
                    })
                    .or_else(|| if ctx.urls { crate::inline::parse_bare_url(s, off) } else { None });
                    if let Some((end, mut node)) = leaf {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
                        crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + end)));
                        out.push(node);
                        t = resync_straddle(s, toks, t, end, &mut out, &mut pending, &mut last_plain_char, &mut fresh, &mut plain_start, &mut plain_end, base, ctx);
                        continue;
                    }
                }
                append!(off, &txt);
                fresh = is_ws;
            }
            Kind::Escape(x) => {
                // (dead in org — org_lex emits Punct(`\`), not Escape — but a `\`-escape drops
                // bytes, so mark the run non-1:1.)
                append!(off, x.as_str());
                plain_start = None;
                fresh = false;
            }
            Kind::Newline(c) => {
                if ctx.breaks {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    out.push(Inline::Break { span: Some(Span(base + off, base + off + 1)) });
                } else {
                    append!(off, if *c == b'\n' { "\n" } else { "\r" });
                }
                fresh = true;
            }
            Kind::Leaf(node) => {
                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                let tok_end_val = if t + 1 < toks.len() { toks[t + 1].off } else { s.len() };
                let mut node = node.clone();
                crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + tok_end_val)));
                out.push(node);
                fresh = true;
            }
            Kind::Delim { ch, .. } => {
                let ch = *ch;
                let (k, kind, fwd_gate, bwd_gate, continue_search) = match ch {
                    b'*' => (1, "Bold", false, false, false),
                    b'/' => (1, "Italic", true, true, false),
                    b'+' => (1, "Strike_through", true, true, false),
                    b'_' => (1, "Underline", true, true, true),
                    b'^' => (2, "Highlight", false, false, false),
                    _ => unreachable!(),
                };
                if let Some((mut node, end)) = parse_emphasis(
                    s, bb, off, k, kind, fwd_gate, bwd_gate, continue_search, ctx,
                    last_plain_char, &mut no_closer, base,
                ) {
                    flush(&mut out, &mut pending, &mut plain_start, plain_end);
                    crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + end)));
                    out.push(node);
                    fresh = true;
                    t = resync(toks, t, end);
                    continue;
                }
                if (ch == b'_' || ch == b'^') && ctx.scripts {
                    if let Some((mut node, end)) = try_script(s, bb, off, ch, base) {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
                        crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + end)));
                        out.push(node);
                        fresh = true;
                        t = resync(toks, t, end);
                        continue;
                    }
                }
                push_byte!(off, ch); // plain_one; marker is a PLAIN_DELIM → fresh
                fresh = true;
            }
            Kind::Punct(b'\\') => {
                // latex closer floor: only let `\(`/`\[` attempt a latex span when its closer
                // exists ahead (monotone cursor) — keeps a `\(`×n run linear.
                let latex_ok = match bb.get(off + 1) {
                    Some(b'(') => present!(bs_paren, b'\\', b')', off),
                    Some(b'[') => present!(bs_brack, b'\\', b']', off),
                    _ => false,
                };
                let (bs, end) = org_backslash_at(s, bb, off, ctx, latex_ok);
                match bs {
                    Bs::Node(mut node) => {
                        flush(&mut out, &mut pending, &mut plain_start, plain_end);
                        crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + end)));
                        out.push(node);
                    }
                    Bs::Plain(text) => {
                        if let Some(b) = text.bytes().next_back() {
                            last_plain_char = Some(b);
                        }
                        // 1:1 with source iff the pushed text spans the whole consumed extent
                        // (`\<punct>` and lone `\` keep the `\`); an unknown entity `\foo`
                        // drops the `\` (text shorter than extent) → S5 can't hold.
                        let one_to_one = text.len() == end - off;
                        if one_to_one {
                            track!(off, text.len());
                        } else {
                            plain_start = None;
                        }
                        pending.push_str(&text);
                    }
                }
                t = resync_straddle(s, toks, t, end, &mut out, &mut pending, &mut last_plain_char, &mut fresh, &mut plain_start, &mut plain_end, base, ctx);
                continue;
            }
            Kind::Punct(c) => {
                let c = *c;
                // PLAIN_DELIMS `# $ [` always dispatch; SWALLOW `~ = < { (` only when fresh.
                let mut hit: Option<usize> = None;
                match c {
                    b'#' if ctx.tags => {
                        let (e, children) = crate::inline::parse_tag_name(s, off + 1, false, base);
                        if e > off + 1 && !children.is_empty() {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            out.push(Inline::Tag { children, span: Some(Span(base + off, base + e)) });
                            hit = Some(e);
                        }
                    }
                    b'$' if ctx.latex => {
                        if let Some((mut node, e)) = try_latex_dollar_at(s, bb, off) {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'[' if ctx.links => {
                        if rbracket < off {
                            rbracket = first_byte(bb, off, b']');
                        }
                        if rbracket < bb.len() {
                            let rb_lb = present!(sq_rb_lb, b']', b'[', off);
                            if let Some((mut node, e)) = try_bracket_at(
                                s, bb, off, ctx, &hiccup_close, &nested_close, &real_dbl,
                                &mut real_dbl_cur, &mut crlf, rb_lb, base,
                            ) {
                                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                                crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    b'~' if ctx.code && fresh => {
                        if let Some((mut node, e)) = try_code_verbatim_at(s, bb, off, b'~') {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'=' if ctx.code && fresh => {
                        if let Some((mut node, e)) = try_code_verbatim_at(s, bb, off, b'=') {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'<' if ctx.angle && fresh => {
                        let html_closer = present!(sq_lt_sl, b'<', b'/', off);
                        if let Some((mut node, e)) = try_target_angle_at(s, bb, off, ctx, html_closer) {
                            flush(&mut out, &mut pending, &mut plain_start, plain_end);
                            crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'{' if ctx.macros && fresh => {
                        if present!(sq_rbrace, b'}', b'}', off) {
                            if let Some((mut node, e)) = try_macro_at(s, bb, off) {
                                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                                crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    b'(' if ctx.block_refs && fresh => {
                        if present!(sq_rr, b')', b')', off) {
                            if let Some((mut node, e)) = try_block_ref_at(s, bb, off) {
                                flush(&mut out, &mut pending, &mut plain_start, plain_end);
                                crate::projection::set_inline_span(&mut node, Some(Span(base + off, base + e)));
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(e) = hit {
                    t = resync_straddle(s, toks, t, e, &mut out, &mut pending, &mut last_plain_char, &mut fresh, &mut plain_start, &mut plain_end, base, ctx);
                    continue;
                }
                // not consumed: plain. `# $ [` are PLAIN_DELIMS (fresh); the swallow bytes
                // and non-openers (`] ) } > !` …) are absorbed mid-run (not fresh).
                push_byte!(off, c);
                fresh = matches!(c, b'#' | b'$' | b'[');
            }
            Kind::LatexBs(c) => {
                // both bytes (`\` + c) are kept → 1:1 with source (2 bytes at `off`).
                let c = *c;
                push_byte!(off, b'\\');
                push_byte!(off + 1, c);
                fresh = false;
            }
        }
        t += 1;
    }
    flush(&mut out, &mut pending, &mut plain_start, plain_end);
    out
}

#[allow(clippy::too_many_arguments)]
fn parse_emphasis(
    s: &str,
    bb: &[u8],
    open_start: usize,
    k: usize,
    kind: &str,
    fwd_gate: bool,
    bwd_gate: bool,
    continue_search: bool,
    ctx: Ctx,
    before: Option<u8>,
    no_closer: &mut [[bool; 2]; 5],
    base: usize,
) -> Option<(Inline, usize)> {
    let n = bb.len();
    let c = bb[open_start];
    let content_start = open_start + k;
    if content_start > n || bb[open_start..content_start].iter().any(|&x| x != c) {
        return None;
    }
    // left-flanking: opener followed by non-whitespace.
    let after = *bb.get(content_start)?;
    if is_ws_or_nl(after) {
        return None;
    }
    // empty content: the next k bytes are themselves the closer pattern.
    if content_start + k <= n && bb[content_start..content_start + k].iter().all(|&x| x == c) {
        return None;
    }
    // backward gate (top level only): char before opener ∈ punct/whitespace.
    if bwd_gate && ctx.use_state {
        let ok = match before {
            Some(ch) => is_underscore_delim(ch),
            None => true,
        };
        if !ok {
            return None;
        }
    }
    let ki = k - 1;
    let ci = class_idx(c);
    if no_closer[ci][ki] {
        return None;
    }
    let closer = match find_closer(bb, c, k, content_start, fwd_gate, continue_search) {
        Some(q) => q,
        None => {
            no_closer[ci][ki] = true;
            return None;
        }
    };
    let content = s[content_start..closer].to_string();
    let children = parse_ctx(&content, Ctx::emph(), base + content_start);
    // span set by the caller over [open_start, closer + k).
    Some((Inline::Emphasis { emph: kind.to_string(), children, span: None }, closer + k))
}

/// First closing run (len ≥ k) of `c` with a non-ws byte before it (escapes skipped); the
/// forward gate / `continue_search` exactly as v1 `find_closer`.
fn find_closer(bb: &[u8], c: u8, k: usize, from: usize, fwd_gate: bool, continue_search: bool) -> Option<usize> {
    let n = bb.len();
    let mut j = from;
    while j < n {
        let cur = bb[j];
        if cur == b'\\' {
            j += 1;
            if j < n {
                j += char_len(bb[j]);
            }
            continue;
        }
        if cur == c {
            let rl = run_len(bb, j, c);
            if rl >= k {
                let before = bb[j - 1];
                if !is_ws_or_nl(before) {
                    if fwd_gate {
                        let fwd_ok = match bb.get(j + k) {
                            None => true,
                            Some(&a) => is_underscore_delim(a),
                        };
                        if fwd_ok {
                            return Some(j);
                        }
                        if !continue_search {
                            return None;
                        }
                        j += k;
                        continue;
                    }
                    return Some(j);
                }
                j += rl;
                continue;
            }
            j += rl;
            continue;
        }
        j += char_len(cur);
    }
    None
}

/// `_x`/`_{x}` → Subscript, `^x`/`^{x}` → Superscript (mldoc `gen_script`). Returns the node
/// and the consumed byte extent; `None` if no valid script body.
fn try_script(s: &str, bb: &[u8], i: usize, c: u8, base: usize) -> Option<(Inline, usize)> {
    let n = bb.len();
    let after = *bb.get(i + 1)?;
    let (content, content_start, end) = if after == b'{' {
        let body_start = i + 2;
        let mut j = body_start;
        while j < n && bb[j] != b'}' && bb[j] != b'\n' && bb[j] != b'\r' {
            j += 1;
        }
        if j >= n || bb[j] != b'}' || j == body_start {
            return None;
        }
        (s[body_start..j].to_string(), body_start, j + 1)
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
    let children = parse_ctx(&content, Ctx::script(), base + content_start);
    // span set by the caller over [i, end).
    let node = if c == b'_' {
        Inline::Subscript { children, span: None }
    } else {
        Inline::Superscript { children, span: None }
    };
    Some((node, end))
}

fn run_len(b: &[u8], pos: usize, c: u8) -> usize {
    let mut k = pos;
    while k < b.len() && b[k] == c {
        k += 1;
    }
    k - pos
}

fn is_org_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0c | 0x1a)
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

/// Advance the token cursor to the first token at/after byte `end` (emphasis/script ends land
/// on a token boundary; tag/bare-url straddles are handled when those sub-steps land).
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
            Some(b'\n') | Some(b'\r') => return (Bs::Node(Inline::HardBreak { span: None }), i + 1),
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

/// Resync after a construct whose raw `end` may land MID a Text token (an Org `\X`-escape /
/// entity that consumed into the following ordinary run): advance past `end`, and if it lands
/// strictly inside a token, push that token's plain tail `s[end..tok_end]` raw (Org never
/// unescapes; the straddled token is always ordinary Text).
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
    out: &mut Vec<Inline>,
    pending: &mut String,
    last_plain_char: &mut Option<u8>,
    fresh: &mut bool,
    plain_start: &mut Option<usize>,
    plain_end: &mut usize,
    base: usize,
    ctx: Ctx,
) -> usize {
    let n = s.len();
    while t < toks.len() && (if t + 1 < toks.len() { toks[t + 1].off } else { n }) <= end {
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        // straddle: an entity/escape consumed into the following ordinary Text run. The tail
        // is `end`'s fresh dispatch point — if it LEADS a no-opener construct (keyword-ts /
        // bare-url), re-dispatch the tail from `end`; else push the plain tail raw.
        let te = if t + 1 < toks.len() { toks[t + 1].off } else { n };
        let leads = (ctx.timestamps
            && matches!(s.as_bytes()[end], b'S' | b'C' | b'D' | b's' | b'c' | b'd')
            && crate::inline::parse_keyword_timestamp(s, end).is_some())
            || (ctx.urls && crate::inline::parse_bare_url(s, end).is_some());
        if leads {
            // FAST PATH: re-lex ONLY the split token's tail (Org is fully local — see fn doc),
            // overwrite `toks[t]`, re-dispatch. Falls back to the suffix re-lex only if the
            // tail is not a single Text/Punct token (defensive; unreachable for Org straddles).
            let mut retok = org_lex(&s[end..te]);
            if retok.len() == 1 && matches!(retok[0].kind, Kind::Text(_) | Kind::Punct(_)) {
                crate::metrics::scan_work(te - end); // O(1): ONLY the split token re-lexed
                retok[0].off += end; // local → absolute
                toks[t] = retok.pop().unwrap();
                *fresh = true; // `end` is a fresh dispatch point
                return t; // re-dispatch the corrected token in the same loop
            }
            flush(out, pending, plain_start, *plain_end);
            crate::metrics::scan_work(s.len() - end); // resync re-lexes the whole suffix
            out.extend(parse_ctx(&s[end..], ctx, base + end));
            return toks.len(); // recursion handled the remainder
        }
        // the tail is pushed RAW (org never unescapes) → 1:1 with source from `end`. pending
        // is empty here (the caller flushed before pushing its node), so this is a fresh run.
        let tail = &s[end..te];
        if let Some(b) = tail.bytes().next_back() {
            *last_plain_char = Some(b);
        }
        *plain_start = Some(base + end);
        *plain_end = base + te;
        *fresh = !tail.is_empty() && tail.bytes().all(|b| b == b' ' || b == b'\t');
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

/// `$ … $` (Inline) / `$$ … $$` (Displayed) — v1 `try_latex_dollar`.
fn try_latex_dollar_at(s: &str, bb: &[u8], i: usize) -> Option<(Inline, usize)> {
    let n = bb.len();
    let after = *bb.get(i + 1)?;
    if after == b'$' {
        let body_start = i + 2;
        let end = crate::inline::find_sub_line(bb, body_start, b"$$")?;
        return Some((Inline::Latex { mode: "Displayed".to_string(), body: s[body_start..end].to_string(), span: None }, end + 2));
    }
    if after == b' ' {
        return None;
    }
    let body_start = i + 1;
    let mut j = body_start;
    while j < n && bb[j] != b'$' && bb[j] != b'\n' && bb[j] != b'\r' {
        j += 1;
    }
    if j >= n || bb[j] != b'$' {
        return None;
    }
    if matches!(bb[j - 1], b' ' | b'(' | b'[' | b'{') {
        return None;
    }
    Some((Inline::Latex { mode: "Inline".to_string(), body: s[body_start..j].to_string(), span: None }, j + 1))
}

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
            Inline::Code { text: body, span: None }
        } else {
            Inline::Verbatim { text: body, span: None }
        };
        Some((node, j + 1))
    } else {
        None
    }
}

/// `<<target>>` then `<…>` angle (autolink → timestamp → inline-html → email) — v1
/// try_target + try_angle. `html_closer` = a `</` exists ahead.
fn try_target_angle_at(s: &str, bb: &[u8], i: usize, ctx: Ctx, html_closer: bool) -> Option<(Inline, usize)> {
    let n = bb.len();
    if s[i..].starts_with("<<") {
        let inner_start = i + 2;
        let mut j = inner_start;
        while j < n {
            let c = bb[j];
            if c == b'<' || c == b'>' || c == b'\n' || c == b'\r' {
                break;
            }
            j += char_len(c);
        }
        if j > inner_start && j + 1 < n && bb[j] == b'>' && bb[j + 1] == b'>' {
            return Some((Inline::Target { text: s[inner_start..j].to_string(), span: None }, j + 2));
        }
    }
    if let Some((end, node)) = crate::org::parse_org_autolink(s, i) {
        return Some((node, end));
    }
    if ctx.timestamps {
        if let Some((end, node)) = crate::inline::parse_angle_timestamp(s, i) {
            return Some((node, end));
        }
    }
    if let Some((end, text)) = crate::inline::parse_inline_html_cached(s, i, html_closer) {
        return Some((Inline::InlineHtml { text, span: None }, end));
    }
    if let Some((end, node)) = crate::inline::parse_email_autolink(s, i) {
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
            return Some((Inline::Macro { name, args, span: None }, j + close.len()));
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
    while j < n && bb[j] != b')' && bb[j] != b'\n' && bb[j] != b'\r' {
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

/// `[` bracket dispatch — v1 try_bracket: hiccup → org_link_1 → nested → org_link_2 (page-ref)
/// → inactive timestamp → footnote. Maps + cursors mirror md's `[[…]]` linearity devices.
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
) -> Option<(Inline, usize)> {
    if ctx.hiccup && bb.get(off + 1) == Some(&b':') && crate::inline::hiccup_head_ok(s, off) {
        if let Some(end) = hiccup_close.get(off).copied().filter(|&e| e != usize::MAX) {
            return Some((Inline::Hiccup { v: s[off..end].to_string(), span: None }, end));
        }
    }
    if s[off..].starts_with("[[") {
        if rb_lb_present {
            if let Some((end, node)) = org_link_1_at(s, bb, off, base) {
                return Some((node, end));
            }
        }
        if nested_close.get(off).is_some_and(|&e| e != usize::MAX) {
            if let Some((end, content)) = crate::inline::parse_nested_link(s, off) {
                return Some((Inline::NestedLink { content, span: None }, end));
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
        if let Some((end, node)) = org_inactive_ts_at(s, bb, off) {
            return Some((node, end));
        }
    }
    if ctx.footnotes {
        if let Some((end, name)) = org_footnote_at(s, off) {
            return Some((Inline::Fnref { name, span: None }, end));
        }
    }
    None
}

/// `[[url][label]]` — v1 org_link_1.
fn org_link_1_at(s: &str, bb: &[u8], at: usize, base: usize) -> Option<(usize, Inline)> {
    let n = bb.len();
    let url_start = at + 2;
    let mut j = url_start;
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
            break;
        }
        j += char_len(c);
    }
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
    Some((end, Inline::Link { url, label, full, image: false, metadata, title: None, span: None }))
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
        _ => vec![Inline::Plain { text: name.clone(), span: Some(Span(base + name_start, base + j)) }],
    };
    // span set by the caller over [at, j + 2).
    Some((j + 2, Inline::Link { url, label, full, image: false, metadata: String::new(), title: None, span: None }))
}

/// Closing `]]` of an org-link label, balancing single `[ ]` pairs — v1 find_org_label_end.
fn find_org_label_end(bb: &[u8], start: usize) -> Option<usize> {
    let n = bb.len();
    let mut j = start;
    let mut depth: i32 = 0;
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
            if depth == 0 {
                if j + 1 < n && bb[j + 1] == b']' {
                    return Some(j);
                }
                return None;
            }
            depth -= 1;
            j += 1;
            continue;
        }
        if c == b'[' {
            depth += 1;
            j += 1;
            continue;
        }
        j += char_len(c);
    }
    None
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

/// `[date]` / `[date]--[date]` inactive timestamp — v1 org_inactive_timestamp.
fn org_inactive_ts_at(s: &str, bb: &[u8], i: usize) -> Option<(usize, Inline)> {
    if !bb.get(i + 1).is_some_and(|c| c.is_ascii_digit() || c.is_ascii_whitespace()) {
        return None;
    }
    let (end1, ts1) = crate::inline::parse_bracket_date(s, i, b'[', b']')?;
    if s[end1..].starts_with("--") {
        if let Some((end2, ts2)) = crate::inline::parse_bracket_date(s, end1 + 2, b'[', b']') {
            let val = serde_json::json!({ "start": ts1, "stop": ts2 });
            return Some((end2, Inline::Timestamp { ts: "Range".to_string(), date: val, span: None }));
        }
    }
    Some((end1, Inline::Timestamp { ts: "Date".to_string(), date: ts1, span: None }))
}

/// `[fn:name]` / `[fn:name:def]` / `[fn::def]` → name — v1 org_footnote_ref.
fn org_footnote_at(s: &str, i: usize) -> Option<(usize, String)> {
    let rest = s[i..].strip_prefix("[fn:")?;
    let rb = rest.as_bytes();
    let mut j = 0;
    while j < rb.len() && rb[j] != b':' && rb[j] != b']' && rb[j] != b'\n' && rb[j] != b'\r' {
        j += 1;
    }
    let name = rest[..j].to_string();
    let after = &rest[j..];
    let close = after.find(']')?;
    if after[..close].contains('\n') || after[..close].contains('\r') {
        return None;
    }
    Some((i + 4 + j + close + 1, name))
}

/// First byte `c` at/after `from`, else `bb.len()` (monotone-cursor helper).
fn first_byte(bb: &[u8], from: usize, c: u8) -> usize {
    let mut p = from;
    while p < bb.len() && bb[p] != c {
        p += 1;
    }
    p
}

/// First `a b` 2-byte sequence at/after `from`, else `bb.len()` (monotone).
fn first_seq(bb: &[u8], a: u8, b: u8, from: usize) -> usize {
    let mut p = from;
    while p + 1 < bb.len() && !(bb[p] == a && bb[p + 1] == b) {
        p += 1;
    }
    if p + 1 < bb.len() {
        p
    } else {
        bb.len()
    }
}

/// First `\n`/`\r` at/after `from`, else `bb.len()` (page-ref eol boundary).
fn first_crlf(bb: &[u8], from: usize) -> usize {
    let mut p = from;
    while p < bb.len() && bb[p] != b'\n' && bb[p] != b'\r' {
        p += 1;
    }
    p
}

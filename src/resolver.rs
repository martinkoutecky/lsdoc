//! lsdoc inline resolver (v0.2) — ONE ctx-aware pass over the lexer's tokens → `Vec<Inline>`.
//!
//! Byte-offset-driven and leftmost-greedy: walks the token stream once, applying context and
//! pairing emphasis/brackets. Byte-exact to mldoc, validated over the differential harness
//! gate (`harness/run.mjs`: 1039-input corpus + inlinegate).
//!
//! **M0** core (text/break/escape/entity/code). **M1** emphasis: mldoc's *leftmost opener →
//! first FORWARD valid closer, flat content reparsed* — NOT a CommonMark backward
//! `openers_bottom` stack (that gives a different tree). Linear via a per-(marker,len)
//! `no_closer` forward floor. Deferred `Punct` tokens still render literally (M2/M3).

use crate::inline::{is_underscore_delim, is_ws_or_nl};
use crate::lexer::{lex, Kind, Token};
use crate::projection::Inline;

/// Active constructs (mirrors v1's `Ctx`; grows as families migrate). Page-ref / nested-link
/// / md-link / code / emphasis / escapes are ALWAYS on (no flag); these gate the constructs
/// mldoc's `Ctx::emph` disables.
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
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

/// Parse a run of inline markup (top-level Markdown context).
pub(crate) fn parse_inline(text: &str) -> Vec<Inline> {
    parse_ctx(text, Ctx::top())
}

/// Re-parse a markdown link/image LABEL with the restricted emphasis-content context
/// (mldoc `aux_nested_emphasis`): the same `Ctx::emph()` the resolver already applies to
/// emphasis *content*. Used by `inline::reparse_label_text` so md label reparse runs on the
/// v0.2 resolver (matching how Org labels go through `org_resolver::parse_ctx(_, Ctx::label())`).
pub(crate) fn parse_inline_ctx_emph(text: &str) -> Vec<Inline> {
    parse_ctx(text, Ctx::emph())
}

fn parse_ctx(text: &str, ctx: Ctx) -> Vec<Inline> {
    let mut toks = lex(text);
    resolve(text, &mut toks, ctx)
}

/// Emphasis candidate patterns for a marker, longest-first (mldoc dispatch order).
/// `(k, kind, nested)`: `nested` is the `***`/`___` form → `Italic[Bold[…]]`.
fn patterns(ch: u8) -> &'static [(usize, &'static str, bool)] {
    match ch {
        b'*' | b'_' => &[(3, "Bold", true), (2, "Bold", false), (1, "Italic", false)],
        b'~' => &[(2, "Strike_through", false)],
        b'^' => &[(2, "Highlight", false)],
        b'=' => &[(2, "Highlight", false)],
        _ => &[],
    }
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

fn resolve(s: &str, toks: &mut [Token], ctx: Ctx) -> Vec<Inline> {
    let bb = s.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
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
        std::collections::HashMap::new()
    };
    let hiccup_close = if has_brk {
        crate::inline::build_hiccup_close(s)
    } else {
        std::collections::HashMap::new()
    };
    let real_dbl = if has_brk { crate::inline::build_real_dbl(s) } else { Vec::new() };
    let lbp = if has_brk { seq_positions(bb, b']', b'(') } else { Vec::new() };
    let mut real_dbl_cur = 0usize;
    let mut lbp_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);
    let mut rparen = first_byte(bb, 0, b')');
    // monotone next-`</` (inline-html name-keyed closer floor: a `<tag>`×n run stays linear).
    let mut lt_slash = first_seq(bb, b'<', b'/', 0);
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
                if let Some(&e) = hiccup_close.get(&off) {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Hiccup { v: s[off..e].to_string() });
                    end = Some(e);
                }
            }
            // 2. footnote `[^id]` (ctx-gated).
            if end.is_none() && ctx.footnotes && bb.get(off + 1) == Some(&b'^') {
                if let Some((e, name)) = crate::inline::parse_footnote_ref(s, off) {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Fnref { name });
                    end = Some(e);
                }
            }
            // 3. nested-link (escape-free balance) then page-ref (escape-aware first `]]`).
            if end.is_none() && s[off..].starts_with("[[") {
                if nested_close.contains_key(&off) {
                    if let Some((e, content)) = crate::inline::parse_nested_link(s, off) {
                        flush(&mut out, &mut pending);
                        out.push(Inline::NestedLink { content });
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
                                flush(&mut out, &mut pending);
                                out.push(Inline::Link {
                                    url: crate::projection::Url::PageRef { v: name },
                                    label: vec![],
                                    full,
                                    image: false,
                                    metadata: String::new(),
                                    title: None,
                                });
                                end = Some(e);
                            }
                        }
                    }
                }
            }
            // 4. markdown link `[label](url)` — needs a `](` before the next eol and a `)`.
            if end.is_none() {
                if let Some((node, e)) =
                    try_md_link(s, bb, off, false, &lbp, &mut lbp_cur, &mut crlf, &mut rparen)
                {
                    flush(&mut out, &mut pending);
                    out.push(node);
                    end = Some(e);
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx),
                None => {
                    pending.push('[');
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
                if let Some((node, e)) = crate::inline::parse_latex_dollar_at(s, off) {
                    flush(&mut out, &mut pending);
                    out.push(node);
                    end = Some(e);
                }
            } else if c == b'#' && ctx.tags {
                let (e, children) = crate::inline::parse_tag_name(s, off + 1, true);
                if e > off + 1 && !children.is_empty() {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Tag { children });
                    end = Some(e);
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx),
                None => {
                    pending.push(c as char);
                    t += 1;
                    fresh = true; // `$`/`#` are marker-delims → fresh point
                }
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
                    if let Some((node, e)) = crate::inline::parse_latex_backslash_at(s, off) {
                        flush(&mut out, &mut pending);
                        out.push(node);
                        end = Some(e);
                    }
                }
            }
            match end {
                Some(e) => t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx),
                None => {
                    pending.push(c as char); // escape: drop `\`, keep `(`/`[`
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
                        try_md_link(s, bb, off + 1, true, &lbp, &mut lbp_cur, &mut crlf, &mut rparen)
                    }
                    b'{' if ctx.macros => crate::inline::parse_macro_at(s, off),
                    b'(' if ctx.block_refs => crate::inline::parse_block_ref_at(s, off),
                    b'<' if ctx.autolinks || ctx.timestamps || ctx.html => {
                        if off > lt_slash {
                            lt_slash = first_seq(bb, b'<', b'/', off);
                        }
                        try_angle(s, off, ctx, lt_slash < bb.len())
                    }
                    _ => None,
                };
                if let Some((node, e)) = opened {
                    flush(&mut out, &mut pending);
                    out.push(node);
                    t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx);
                    continue;
                }
            }
            // not consumed (failed opener, or mid-plain-run) → render as plain; now mid-run, so
            // a following swallow byte won't be re-dispatched.
            pending.push(c as char);
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
                .or_else(|| if ctx.urls { crate::inline::parse_bare_url(s, off) } else { None });
                if let Some((e, node)) = leaf {
                    flush(&mut out, &mut pending);
                    out.push(node);
                    t = resync(s, toks, t, e, &mut out, &mut pending, &mut fresh, ctx);
                    continue;
                }
            }
            let txt = match &toks[t].kind {
                Kind::Text(x) => x,
                _ => unreachable!(),
            };
            pending.push_str(txt);
            fresh = trailing_ws(txt) > 0;
            t += 1;
            continue;
        }

        // Non-delimiter tokens pass straight through (Text is handled by its own block above).
        if !matches!(toks[t].kind, Kind::Delim { .. }) {
            match &toks[t].kind {
                Kind::Newline(c) => {
                    if ctx.breaks {
                        // hard break: `\n` (not `\r`) immediately preceded by >=2 spaces/tabs
                        // in the pending run — the spaces are consumed (mldoc).
                        let tw = trailing_ws(&pending);
                        if *c == b'\n' && tw >= 2 {
                            pending.truncate(pending.len() - tw);
                            flush(&mut out, &mut pending);
                            out.push(Inline::HardBreak);
                        } else {
                            flush(&mut out, &mut pending);
                            out.push(Inline::Break);
                        }
                    } else {
                        pending.push(*c as char);
                    }
                    fresh = true;
                }
                Kind::Leaf(node) => {
                    flush(&mut out, &mut pending);
                    out.push(node.clone());
                    fresh = true;
                }
                // resolved escape / lone `\` / unknown entity letters — the position right
                // after is a fresh dispatch point in mldoc.
                Kind::Escape(x) => {
                    pending.push_str(x);
                    fresh = true;
                }
                // `$`/`#` (M3 markers) render literally for now; they are marker-delims → fresh.
                Kind::Punct(c) => {
                    pending.push(*c as char);
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
        // `_` open gate (md): the char before the opener must be an underscore-delim (or BOL).
        let before_ok = ch != b'_' || off == 0 || is_underscore_delim(bb[off - 1]);
        let mut matched = false;
        if before_ok {
            for &(k, kind, nested) in patterns(ch) {
                if len < k {
                    continue;
                }
                let content_start = off + k;
                // left-flank: the char after the k opener markers must be non-ws.
                match bb.get(content_start) {
                    Some(&a) if !is_ws_or_nl(a) => {}
                    _ => continue,
                }
                // empty content: opener immediately followed by its closing pattern.
                if content_start + k <= bb.len()
                    && bb[content_start..content_start + k].iter().all(|&x| x == ch)
                {
                    continue;
                }
                let cls = class_idx(ch);
                if no_closer[cls][k - 1] {
                    continue;
                }
                let Some((closer_t, closer_off)) = find_closer(bb, toks, t, ch, k) else {
                    no_closer[cls][k - 1] = true;
                    continue;
                };
                // `_` close gate (post-check on the FIRST closer): char after the k closing
                // markers must be an underscore-delim (or EOF) — else this pattern fails.
                if ch == b'_' && bb.get(closer_off + k).is_some_and(|&a| !is_underscore_delim(a)) {
                    continue;
                }
                let children = parse_ctx(&s[content_start..closer_off], Ctx::emph());
                let node = if nested {
                    Inline::Emphasis {
                        emph: "Italic".to_string(),
                        children: vec![Inline::Emphasis { emph: kind.to_string(), children }],
                    }
                } else {
                    Inline::Emphasis { emph: kind.to_string(), children }
                };
                flush(&mut out, &mut pending);
                out.push(node);
                // Resume past the k closing markers; closer-run surplus re-enters as a fresh
                // Delim (mldoc re-dispatches at byte closer_off + k).
                let closer_len = match &toks[closer_t].kind {
                    Kind::Delim { len, .. } => *len,
                    _ => unreachable!(),
                };
                if closer_len > k {
                    toks[closer_t] = Token {
                        off: closer_off + k,
                        kind: Kind::Delim { ch, len: closer_len - k },
                    };
                    t = closer_t;
                } else {
                    t = closer_t + 1;
                }
                matched = true;
                break;
            }
        }
        if !matched {
            // failed opener: emit ONE marker char, re-dispatch the rest of the run at off+1.
            pending.push(ch as char);
            if len > 1 {
                toks[t] = Token { off: off + 1, kind: Kind::Delim { ch, len: len - 1 } };
            } else {
                t += 1;
            }
        }
        // a marker run (matched emphasis or a literal marker char) is a marker-delim → fresh.
        fresh = true;
    }
    flush(&mut out, &mut pending);
    out
}

/// First valid closer for an opener of pattern `(ch, k)` at token `open_t`: the first later
/// `Delim` token of the same `ch`, len ≥ k, right-flanking (the byte before its start is
/// non-ws). Code/escapes are already collapsed into Leaf/Text tokens, so scanning `Delim`
/// tokens reproduces v1's byte scan that skips them. The `_` forward gate is the caller's
/// post-check. Returns `(closer_token_index, closer_byte_offset)`.
fn find_closer(bb: &[u8], toks: &[Token], open_t: usize, ch: u8, k: usize) -> Option<(usize, usize)> {
    let mut q = open_t + 1;
    while q < toks.len() {
        if let Kind::Delim { ch: dch, len } = &toks[q].kind {
            let qoff = toks[q].off;
            if *dch == ch && *len >= k && qoff > 0 && !is_ws_or_nl(bb[qoff - 1]) {
                return Some((q, qoff));
            }
        }
        q += 1;
    }
    None
}

fn flush(out: &mut Vec<Inline>, pending: &mut String) {
    if !pending.is_empty() {
        out.push(Inline::Plain { text: std::mem::take(pending) });
    }
}

/// Count of trailing space/tab bytes in `s` (for hard-break detection).
fn trailing_ws(s: &str) -> usize {
    s.bytes().rev().take_while(|&b| b == b' ' || b == b'\t').count()
}

/// First `\n`/`\r` byte at/after `from`, or `bb.len()` (page-ref eol boundary).
fn first_crlf(bb: &[u8], from: usize) -> usize {
    let mut p = from;
    while p < bb.len() && bb[p] != b'\n' && bb[p] != b'\r' {
        p += 1;
    }
    p
}

/// After consuming a construct's byte extent `[_, end)`, advance the token cursor past it
/// (leftmost-greedy resync — interior tokens discarded). Most constructs end at a clean
/// token boundary; tag / bare-url end mid-Text (at a ws / tag-delim), so when `end` lands
/// strictly inside a straddling Text token, re-lex its tail `s[end..token_end]` (re-resolving
/// escapes) into `pending`. (`end` never lands inside a Punct/Delim/Leaf token.)
#[allow(clippy::too_many_arguments)]
fn resync(
    s: &str,
    toks: &[Token],
    mut t: usize,
    end: usize,
    out: &mut Vec<Inline>,
    pending: &mut String,
    fresh: &mut bool,
    ctx: Ctx,
) -> usize {
    let n = s.len();
    let tok_end = |i: usize| if i + 1 < toks.len() { toks[i + 1].off } else { n };
    while t < toks.len() && tok_end(t) <= end {
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        // `end` lands strictly inside a straddled token (a tag / bare-url / latex / page-ref
        // whose raw end falls mid-Text or mid-Escape — escape is CONSTRUCT-LOCAL, so the
        // token boundaries needn't align). If the tail STARTS a fresh construct (e.g. a tag
        // consumed `\` and left a `#`, or a `\)`-split left a `(`), re-lex+resolve the rest
        // from `end` (a fresh dispatch point); otherwise the tail is plain — push it raw.
        let bb = s.as_bytes();
        // Straddling a Leaf (a code span the lexer eagerly built but a tag consumed `` ` ``
        // as a literal char) means the tail spans a region with its own constructs → must
        // re-dispatch. A plain Text/Escape tail re-dispatches only if it leads a construct.
        let recurse = matches!(toks[t].kind, Kind::Leaf(_))
            || bb.get(end).is_some_and(|&c| is_special_lead(c))
            || (ctx.timestamps && crate::inline::parse_keyword_timestamp(s, end).is_some())
            || (ctx.urls && crate::inline::parse_bare_url(s, end).is_some());
        if recurse {
            flush(out, pending);
            out.extend(parse_ctx(&s[end..], ctx));
            return toks.len(); // recursion handled the remainder — stop the outer walk
        }
        let tail = &s[end..tok_end(t)];
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
/// `html_closer` says whether a `</` exists ahead (so the name-keyed closer scan can be
/// skipped — the by-construction floor that keeps a `<tag>`×n run linear).
fn try_angle(s: &str, at: usize, ctx: Ctx, html_closer: bool) -> Option<(Inline, usize)> {
    if ctx.autolinks {
        if let Some((e, node)) = crate::inline::parse_autolink(s, at) {
            return Some((node, e));
        }
    }
    if ctx.timestamps {
        if let Some((e, node)) = crate::inline::parse_angle_timestamp(s, at) {
            return Some((node, e));
        }
    }
    if ctx.autolinks {
        if let Some((e, node)) = crate::inline::parse_email_autolink(s, at) {
            return Some((node, e));
        }
    }
    if ctx.html {
        if let Some((e, text)) = crate::inline::parse_inline_html_cached(s, at, html_closer) {
            return Some((Inline::InlineHtml { text }, e));
        }
    }
    None
}

/// First byte `c` at/after `from`, or `bb.len()` if none (monotone-cursor helper).
fn first_byte(bb: &[u8], from: usize, c: u8) -> usize {
    let mut p = from;
    while p < bb.len() && bb[p] != c {
        p += 1;
    }
    p
}

/// First position of the 2-byte sequence `a b` at/after `from`, or `bb.len()` (monotone).
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
    crate::inline::md_link(s, at, image)
}

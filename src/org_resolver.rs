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
//! Built milestone-by-milestone behind the `LSDOC_ORG_INLINE_V2` seam, validated byte-exact
//! against `crate::org::parse_inline_org_top` over fuzzed Org inputs. **M6-core** here: text /
//! break / escape / entity; markers + specials are emitted as deferred tokens (rendered
//! literally until the emphasis / leaf / bracket sub-steps refine them).

use crate::inline::{char_len, is_underscore_delim, is_ws, is_ws_or_nl};
use crate::lexer::{Kind, Token};
use crate::projection::Inline;

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


/// Parse an Org inline run at top level.
pub(crate) fn parse_inline_org(text: &str) -> Vec<Inline> {
    parse_ctx(text, Ctx::top())
}

fn parse_ctx(text: &str, ctx: Ctx) -> Vec<Inline> {
    let mut toks = org_lex(text);
    resolve(text, &mut toks, ctx)
}

fn flush(out: &mut Vec<Inline>, pending: &mut String) {
    if !pending.is_empty() {
        out.push(Inline::Plain { text: std::mem::take(pending) });
    }
}

/// Resolver: ONE ctx-aware pass + stack over the Org tokens. M6 emphasis sub-step: real
/// emphasis (the stateful backward gate + forward gate / `continue_search`) and sub/super-
/// script; the remaining specials (`Punct`/`LatexBs`) still render literally (refined by the
/// leaf / bracket sub-steps). `last_plain_char` mirrors mldoc `push_plain`: updated on EVERY
/// plain append and PERSISTS across nodes/flush (an emphasis node does NOT reset it).
#[allow(unused_assignments)] // last_plain_char / fresh are running state; final writes may be unread
fn resolve(s: &str, toks: &mut [Token], ctx: Ctx) -> Vec<Inline> {
    let bb = s.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    let mut last_plain_char: Option<u8> = None;
    let mut no_closer = [[false; 2]; 5];

    // Bracket-pairing maps (shared with md; computed once when `[` is present) + monotone
    // closer cursors (the v1 `seq_present`/`has_rbracket`/`next_real_dbl`/`next_crlf` floors,
    // expressed as forward cursors — keep the gated `[`×n / `{{ `×n / `(( `×n runs linear).
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
    let mut real_dbl_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);
    let mut rbracket = first_byte(bb, 0, b']');
    let mut sq_rb_lb = first_seq(bb, b']', b'[', 0); // ][
    let mut sq_rr = first_seq(bb, b')', b')', 0); // ))
    let mut sq_rbrace = first_seq(bb, b'}', b'}', 0); // }}
    let mut sq_lt_sl = first_seq(bb, b'<', b'/', 0); // </
    // `fresh` = a dispatch point (mldoc `plain_run` stops at PLAIN_DELIMS `\ _ ^ [ * / + $ #`
    // + ws/eol). The SWALLOW openers `~ = < { (` fire only when fresh; mid-plain-run they are
    // absorbed as literal text.
    let mut fresh = true;

    macro_rules! append {
        ($seg:expr) => {{
            let seg: &str = $seg;
            if let Some(b) = seg.bytes().next_back() {
                last_plain_char = Some(b);
            }
            pending.push_str(seg);
        }};
    }
    macro_rules! push_byte {
        ($c:expr) => {{
            let c: u8 = $c;
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
                    if let Some((end, node)) = leaf {
                        flush(&mut out, &mut pending);
                        out.push(node);
                        t = resync_straddle(s, toks, t, end, &mut out, &mut pending, &mut last_plain_char, &mut fresh, ctx);
                        continue;
                    }
                }
                append!(&txt);
                fresh = is_ws;
            }
            Kind::Escape(x) => {
                append!(x.as_str());
                fresh = false;
            }
            Kind::Newline(c) => {
                if ctx.breaks {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Break);
                } else {
                    append!(if *c == b'\n' { "\n" } else { "\r" });
                }
                fresh = true;
            }
            Kind::Leaf(node) => {
                flush(&mut out, &mut pending);
                out.push(node.clone());
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
                if let Some((node, end)) = parse_emphasis(
                    s, bb, off, k, kind, fwd_gate, bwd_gate, continue_search, ctx,
                    last_plain_char, &mut no_closer,
                ) {
                    flush(&mut out, &mut pending);
                    out.push(node);
                    fresh = true;
                    t = resync(toks, t, end);
                    continue;
                }
                if (ch == b'_' || ch == b'^') && ctx.scripts {
                    if let Some((node, end)) = try_script(s, bb, off, ch) {
                        flush(&mut out, &mut pending);
                        out.push(node);
                        fresh = true;
                        t = resync(toks, t, end);
                        continue;
                    }
                }
                push_byte!(ch); // plain_one; marker is a PLAIN_DELIM → fresh
                fresh = true;
            }
            Kind::Punct(b'\\') => {
                let (bs, end) = org_backslash_at(s, bb, off, ctx);
                match bs {
                    Bs::Node(node) => {
                        flush(&mut out, &mut pending);
                        out.push(node);
                    }
                    Bs::Plain(text) => {
                        if let Some(b) = text.bytes().next_back() {
                            last_plain_char = Some(b);
                        }
                        pending.push_str(&text);
                    }
                }
                t = resync_straddle(s, toks, t, end, &mut out, &mut pending, &mut last_plain_char, &mut fresh, ctx);
                continue;
            }
            Kind::Punct(c) => {
                let c = *c;
                // PLAIN_DELIMS `# $ [` always dispatch; SWALLOW `~ = < { (` only when fresh.
                let mut hit: Option<usize> = None;
                match c {
                    b'#' if ctx.tags => {
                        let (e, children) = crate::inline::parse_tag_name(s, off + 1, false);
                        if e > off + 1 && !children.is_empty() {
                            flush(&mut out, &mut pending);
                            out.push(Inline::Tag { children });
                            hit = Some(e);
                        }
                    }
                    b'$' if ctx.latex => {
                        if let Some((node, e)) = try_latex_dollar_at(s, bb, off) {
                            flush(&mut out, &mut pending);
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
                            if let Some((node, e)) = try_bracket_at(
                                s, bb, off, ctx, &hiccup_close, &nested_close, &real_dbl,
                                &mut real_dbl_cur, &mut crlf, rb_lb,
                            ) {
                                flush(&mut out, &mut pending);
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    b'~' if ctx.code && fresh => {
                        if let Some((node, e)) = try_code_verbatim_at(s, bb, off, b'~') {
                            flush(&mut out, &mut pending);
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'=' if ctx.code && fresh => {
                        if let Some((node, e)) = try_code_verbatim_at(s, bb, off, b'=') {
                            flush(&mut out, &mut pending);
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'<' if ctx.angle && fresh => {
                        let html_closer = present!(sq_lt_sl, b'<', b'/', off);
                        if let Some((node, e)) = try_target_angle_at(s, bb, off, ctx, html_closer) {
                            flush(&mut out, &mut pending);
                            out.push(node);
                            hit = Some(e);
                        }
                    }
                    b'{' if ctx.macros && fresh => {
                        if present!(sq_rbrace, b'}', b'}', off) {
                            if let Some((node, e)) = try_macro_at(s, bb, off) {
                                flush(&mut out, &mut pending);
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    b'(' if ctx.block_refs && fresh => {
                        if present!(sq_rr, b')', b')', off) {
                            if let Some((node, e)) = try_block_ref_at(s, bb, off) {
                                flush(&mut out, &mut pending);
                                out.push(node);
                                hit = Some(e);
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(e) = hit {
                    t = resync_straddle(s, toks, t, e, &mut out, &mut pending, &mut last_plain_char, &mut fresh, ctx);
                    continue;
                }
                // not consumed: plain. `# $ [` are PLAIN_DELIMS (fresh); the swallow bytes
                // and non-openers (`] ) } > !` …) are absorbed mid-run (not fresh).
                push_byte!(c);
                fresh = matches!(c, b'#' | b'$' | b'[');
            }
            Kind::LatexBs(c) => {
                push_byte!(b'\\');
                push_byte!(*c);
                fresh = false;
            }
        }
        t += 1;
    }
    flush(&mut out, &mut pending);
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
    let children = parse_ctx(&content, Ctx::emph());
    Some((Inline::Emphasis { emph: kind.to_string(), children }, closer + k))
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
fn try_script(s: &str, bb: &[u8], i: usize, c: u8) -> Option<(Inline, usize)> {
    let n = bb.len();
    let after = *bb.get(i + 1)?;
    let (content, end) = if after == b'{' {
        let body_start = i + 2;
        let mut j = body_start;
        while j < n && bb[j] != b'}' && bb[j] != b'\n' && bb[j] != b'\r' {
            j += 1;
        }
        if j >= n || bb[j] != b'}' || j == body_start {
            return None;
        }
        (s[body_start..j].to_string(), j + 1)
    } else {
        if is_org_space(after) {
            return None;
        }
        let start = i + 1;
        let mut j = start;
        while j < n && !is_org_space(bb[j]) {
            j += char_len(bb[j]);
        }
        (s[start..j].to_string(), j)
    };
    let children = parse_ctx(&content, Ctx::script());
    let node = if c == b'_' {
        Inline::Subscript { children }
    } else {
        Inline::Superscript { children }
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
fn org_backslash_at(s: &str, bb: &[u8], i: usize, ctx: Ctx) -> (Bs, usize) {
    let n = bb.len();
    if ctx.entity {
        match bb.get(i + 1) {
            None => return (Bs::Plain("\\".to_string()), i + 1),
            Some(b'\n') | Some(b'\r') => return (Bs::Node(Inline::HardBreak), i + 1),
            _ => {}
        }
        if ctx.latex {
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
#[allow(clippy::too_many_arguments)]
fn resync_straddle(
    s: &str,
    toks: &[Token],
    mut t: usize,
    end: usize,
    out: &mut Vec<Inline>,
    pending: &mut String,
    last_plain_char: &mut Option<u8>,
    fresh: &mut bool,
    ctx: Ctx,
) -> usize {
    let n = s.len();
    let tok_end = |i: usize| if i + 1 < toks.len() { toks[i + 1].off } else { n };
    while t < toks.len() && tok_end(t) <= end {
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        // straddle: an entity/escape consumed into the following ordinary Text run. The tail
        // is `end`'s fresh dispatch point — if it LEADS a no-opener construct (keyword-ts /
        // bare-url), re-resolve the remainder from `end`; else push the plain tail raw.
        let leads = (ctx.timestamps
            && matches!(s.as_bytes()[end], b'S' | b'C' | b'D' | b's' | b'c' | b'd')
            && crate::inline::parse_keyword_timestamp(s, end).is_some())
            || (ctx.urls && crate::inline::parse_bare_url(s, end).is_some());
        if leads {
            flush(out, pending);
            out.extend(parse_ctx(&s[end..], ctx));
            return toks.len(); // recursion handled the remainder
        }
        let tail = &s[end..tok_end(t)];
        if let Some(b) = tail.bytes().next_back() {
            *last_plain_char = Some(b);
        }
        *fresh = !tail.is_empty() && tail.bytes().all(|b| b == b' ' || b == b'\t');
        pending.push_str(tail);
        t += 1;
    } else {
        // clean construct end → fresh dispatch point.
        *fresh = true;
    }
    t
}

// ---- leaf / bracket constructs (reimplemented from the v1 `OrgScanner` methods, byte-based;
// shared free predicates reused from `crate::inline` / `crate::org`) -----------------------

/// `$ … $` (Inline) / `$$ … $$` (Displayed) — v1 `try_latex_dollar`.
fn try_latex_dollar_at(s: &str, bb: &[u8], i: usize) -> Option<(Inline, usize)> {
    let n = bb.len();
    let after = *bb.get(i + 1)?;
    if after == b'$' {
        let body_start = i + 2;
        let end = crate::inline::find_sub_line(bb, body_start, b"$$")?;
        return Some((Inline::Latex { mode: "Displayed".to_string(), body: s[body_start..end].to_string() }, end + 2));
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
    Some((Inline::Latex { mode: "Inline".to_string(), body: s[body_start..j].to_string() }, j + 1))
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
            Inline::Code { text: body }
        } else {
            Inline::Verbatim { text: body }
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
            return Some((Inline::Target { text: s[inner_start..j].to_string() }, j + 2));
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
        return Some((Inline::InlineHtml { text }, end));
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
            return Some((Inline::Macro { name, args }, j + close.len()));
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
    hiccup_close: &std::collections::HashMap<usize, usize>,
    nested_close: &std::collections::HashMap<usize, usize>,
    real_dbl: &[usize],
    real_dbl_cur: &mut usize,
    crlf: &mut usize,
    rb_lb_present: bool,
) -> Option<(Inline, usize)> {
    if ctx.hiccup && bb.get(off + 1) == Some(&b':') && crate::inline::hiccup_head_ok(s, off) {
        if let Some(&end) = hiccup_close.get(&off) {
            return Some((Inline::Hiccup { v: s[off..end].to_string() }, end));
        }
    }
    if s[off..].starts_with("[[") {
        if rb_lb_present {
            if let Some((end, node)) = org_link_1_at(s, bb, off) {
                return Some((node, end));
            }
        }
        if nested_close.contains_key(&off) {
            if let Some((end, content)) = crate::inline::parse_nested_link(s, off) {
                return Some((Inline::NestedLink { content }, end));
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
                if let Some((end, node)) = org_link_2_at(s, bb, off) {
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
            return Some((Inline::Fnref { name }, end));
        }
    }
    None
}

/// `[[url][label]]` — v1 org_link_1.
fn org_link_1_at(s: &str, bb: &[u8], at: usize) -> Option<(usize, Inline)> {
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
    let label = parse_ctx(&label_text, Ctx::label());
    let label_first = match label.first() {
        Some(Inline::Plain { text }) => text.clone(),
        _ => String::new(),
    };
    let full = format!("[[{}][{}]]{}", url_text, label_first, metadata);
    Some((end, Inline::Link { url, label, full, image: false, metadata, title: None }))
}

/// `[[url]]` — v1 org_link_2 (single `]` allowed, non-empty, no eol).
fn org_link_2_at(s: &str, bb: &[u8], at: usize) -> Option<(usize, Inline)> {
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
    let label = match &url {
        crate::projection::Url::PageRef { .. } => vec![],
        _ => vec![Inline::Plain { text: name.clone() }],
    };
    Some((j + 2, Inline::Link { url, label, full, image: false, metadata: String::new(), title: None }))
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
            return Some((end2, Inline::Timestamp { ts: "Range".to_string(), date: val }));
        }
    }
    Some((end1, Inline::Timestamp { ts: "Date".to_string(), date: ts1 }))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Differential: `resolve(org_lex(s)) == crate::org::parse_inline_org_top(s)` over fuzzed
    /// inputs built from `tokens` (growing per sub-step). `crate::org` is byte-exact to mldoc.
    fn diff_count(tokens: &[&str], iters: usize, seed0: u64) -> usize {
        let mut diffs = 0usize;
        let mut state = seed0 | 1;
        let mut rng = || {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..iters {
            let len = (rng() % 7) as usize;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(tokens[(rng() as usize) % tokens.len()]);
            }
            let v2 = parse_inline_org(&s);
            let v1 = crate::org::parse_inline_org_top(&s);
            if format!("{v2:?}") != format!("{v1:?}") {
                if diffs < 10 {
                    eprintln!("ORG DIFF {s:?}\n   v1={v1:?}\n   v2={v2:?}\n");
                }
                diffs += 1;
            }
        }
        diffs
    }

    /// M6-core: plain / ws / break / Org escape (non-destructive) / entity. NO markers or
    /// constructs yet (those render literally in the core stub).
    #[test]
    fn org_v2_matches_v1_m6_core() {
        const TOKENS: &[&str] =
            &["a", "b", " ", "\n", "word", "café", "\\,", "\\;", "\\.", "\\Delta", "\\alpha", "x"];
        assert_eq!(diff_count(TOKENS, 300_000, 0x6094), 0);
    }

    /// M6 emphasis: `*` Bold / `/` Italic / `+` Strike / `_` Underline / `^^` Highlight (the
    /// stateful backward gate + forward-gate/`continue_search`) and `_x`/`^x` sub/superscript.
    #[test]
    fn org_v2_matches_v1_m6_emphasis() {
        const TOKENS: &[&str] = &[
            "a", "b", " ", "\n", "x", "word", ".", ",",
            "*", "/", "+", "_", "^", "^^", "*b*", "/i/", "+s+", "_u_", "^^h^^",
            "_{sub}", "^{sup}", "\\,", "\\Delta",
        ];
        assert_eq!(diff_count(TOKENS, 500_000, 0x5E11), 0);
    }

    /// Exhaustive small Org emphasis strings over `{* / + _ ^ a space}` (lengths 1..=6) — the
    /// gate-heavy corner (backward state, `_` continue-search, `^^`/`^x` dual use).
    #[test]
    fn org_v2_emphasis_exhaustive() {
        let alpha = [b'*', b'/', b'+', b'_', b'^', b'a', b' '];
        let mut diffs = 0;
        let mut buf = Vec::new();
        fn rec(alpha: &[u8], buf: &mut Vec<u8>, depth: usize, diffs: &mut usize) {
            if depth > 0 {
                let s = std::str::from_utf8(buf).unwrap();
                let v2 = parse_inline_org(s);
                let v1 = crate::org::parse_inline_org_top(s);
                if format!("{v2:?}") != format!("{v1:?}") {
                    if *diffs < 12 {
                        eprintln!("ORG EX DIFF {s:?}\n   v1={v1:?}\n   v2={v2:?}\n");
                    }
                    *diffs += 1;
                }
            }
            if depth == 6 {
                return;
            }
            for &ch in alpha {
                buf.push(ch);
                rec(alpha, buf, depth + 1, diffs);
                buf.pop();
            }
        }
        rec(&alpha, &mut buf, 0, &mut diffs);
        assert_eq!(diffs, 0, "{diffs} divergences");
    }

    /// Diagnostic: the inline-relevant subset of the node-fuzz `TOKENS_ORG`, to surface any
    /// remaining v2-vs-v1 org divergences the curated alphabets missed.
    #[test]
    #[ignore = "diagnostic; run explicitly"]
    fn org_v2_matches_v1_nodefuzz() {
        const TOKENS: &[&str] = &[
            "* ", "** ", "*** ", "*", "/", "_", "+", "~", "=", "^", "^^", "[[", "]]", "][",
            "[[target]]", "[[t][l]]", "[fn:1]", "<2026-06-26 Fri>", "[2026-06-20 Sat]",
            "SCHEDULED: ", "DEADLINE: ", "[#A] ", ":tag1:tag2:", "\\", "a", "b", " ", "  ", "\n",
            "café", "中文", "😀", ".", "/", "_x", "^y", "word",
        ];
        let n = diff_count(TOKENS, 200_000, 0x7A03);
        assert_eq!(n, 0, "{n} divergences");
    }

    /// Diagnostic: full `parse_org_to_projection` v1-vs-v2 over the COMPLETE node-fuzz
    /// `TOKENS_ORG` (block + inline), to find block-body inline divergences.
    #[test]
    #[ignore = "diagnostic; run explicitly"]
    fn org_v2_block_projection() {
        const TOKENS: &[&str] = &[
            "* ", "** ", "*** ", "*", "/", "_", "+", "~", "=", "^", "^^", "[[", "]]", "][",
            "[[target]]", "[[t][l]]", "[fn:1]", "<2026-06-26 Fri>", "[2026-06-20 Sat]",
            "#+TITLE: ", "#+BEGIN_SRC ", "#+END_SRC", "#+BEGIN_QUOTE", "#+END_QUOTE", "#+NAME: ",
            ":PROPERTIES:", ":key: value", ":END:", "SCHEDULED: ", "DEADLINE: ", "TODO ",
            "DONE ", "[#A] ", ":tag1:tag2:", "- ", "+ ", "1. ", "| a | b |", "\\", "a", "b",
            " ", "  ", "\n", "café", "中文", "😀", ".", "/", "_x", "^y", "word",
        ];
        let mut diffs = 0;
        let mut state = 0x5151u64 | 1;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..150_000 {
            let len = (rng() % 7) as usize;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(TOKENS[(rng() as usize) % TOKENS.len()]);
            }
            std::env::remove_var("LSDOC_ORG_INLINE_V2");
            let v1 = format!("{:?}", crate::parse_org_to_projection(&s));
            std::env::set_var("LSDOC_ORG_INLINE_V2", "1");
            let v2 = format!("{:?}", crate::parse_org_to_projection(&s));
            if v1 != v2 {
                if diffs < 10 {
                    eprintln!("ORG BLK DIFF {s:?}\n   v1={v1}\n   v2={v2}\n");
                }
                diffs += 1;
            }
        }
        std::env::remove_var("LSDOC_ORG_INLINE_V2");
        assert_eq!(diffs, 0, "{diffs} divergences");
    }

    /// M6 leaves: every Org leaf / bracket family + the swallow/fresh interactions.
    #[test]
    fn org_v2_matches_v1_m6_leaves() {
        const TOKENS: &[&str] = &[
            "a", "b", " ", "\n", "x", "word", ".", ",",
            "*b*", "/i/", "_u_", "^^h^^", "_{s}", "^{s}",
            "#tag", "$x$", "$$d$$", "~code~", "=verb=", "<<tg>>",
            "{{m}}", "{{{q}}}", "((11111111-1111-1111-1111-111111111111))",
            "[[Foo]]", "[[u][l]]", "[:div ]", "[fn:1]", "[2024-01-01 Mon]",
            "<https://z.io>", "<a@b.com>", "<2026-06-20 Sat>", "<div>", "</div>",
            "SCHEDULED: <2004-12-25 Sat>", "http://x.com/a", "\\,", "\\Delta", "\\(e\\)",
            "~", "=", "<", "{", "(", "[", "]", ")", "}", ">", "!",
        ];
        assert_eq!(diff_count(TOKENS, 600_000, 0x10F6), 0);
    }
}

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
                // let the resolver run the ctx-aware `backslash()` on the raw bytes.
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
fn resolve(s: &str, toks: &mut [Token], ctx: Ctx) -> Vec<Inline> {
    let bb = s.as_bytes();
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    let mut last_plain_char: Option<u8> = None;
    // no_closer[class][k-1]: a failed opener of (marker,k) never re-scans (mldoc bail).
    let mut no_closer = [[false; 2]; 5];

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
            pending.push(c as char); // markers / deferred specials are ASCII
            last_plain_char = Some(c);
        }};
    }

    let mut t = 0usize;
    while t < toks.len() {
        let off = toks[t].off;
        match &toks[t].kind {
            Kind::Text(txt) => append!(txt.as_str()),
            Kind::Escape(x) => append!(x.as_str()),
            Kind::Newline(c) => {
                if ctx.breaks {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Break);
                } else {
                    append!(if *c == b'\n' { "\n" } else { "\r" });
                }
            }
            Kind::Leaf(node) => {
                flush(&mut out, &mut pending);
                out.push(node.clone());
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
                    t = resync(toks, t, end);
                    continue;
                }
                if (ch == b'_' || ch == b'^') && ctx.scripts {
                    if let Some((node, end)) = try_script(s, bb, off, ch) {
                        flush(&mut out, &mut pending);
                        out.push(node);
                        t = resync(toks, t, end);
                        continue;
                    }
                }
                push_byte!(ch);
            }
            // `\`-dispatch (ctx-aware backslash: hard-break / latex / entity / escape).
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
                t = resync_straddle(s, toks, t, end, &mut pending, &mut last_plain_char);
                continue;
            }
            // Deferred specials — literal for now (leaf / bracket sub-steps refine).
            Kind::Punct(c) => push_byte!(*c),
            Kind::LatexBs(c) => {
                push_byte!(b'\\');
                push_byte!(*c);
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
fn resync_straddle(
    s: &str,
    toks: &[Token],
    mut t: usize,
    end: usize,
    pending: &mut String,
    last_plain_char: &mut Option<u8>,
) -> usize {
    let n = s.len();
    let tok_end = |i: usize| if i + 1 < toks.len() { toks[i + 1].off } else { n };
    while t < toks.len() && tok_end(t) <= end {
        t += 1;
    }
    if t < toks.len() && toks[t].off < end {
        let tail = &s[end..tok_end(t)];
        if let Some(b) = tail.bytes().next_back() {
            *last_plain_char = Some(b);
        }
        pending.push_str(tail);
        t += 1;
    }
    t
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
}

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

use crate::inline::{char_len, is_ws, is_ws_or_nl};
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
            b'\\' => org_backslash(s, &mut i, &mut pending, &mut pending_off, &mut toks),
            _ if is_marker(c) => {
                flush!();
                let mut j = i;
                while j < n && b[j] == c {
                    j += 1;
                }
                toks.push(Token { off: i, kind: Kind::Delim { ch: c, len: j - i } });
                i = j;
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

/// Org `\`-dispatch: `\(`/`\[` latex (deferred, ctx-gated → `LatexBs`); `\name` entity (known
/// → `Leaf`, unknown → the bare letters, no backslash); `\`+eol hard-break (deferred via a
/// `Punct(\)` before the `Newline`, ctx-gated in the resolver); `\`+punct kept LITERAL as
/// `"\X"` (Org never unescapes); lone `\` kept. All are fresh-making.
fn org_backslash(
    s: &str,
    i: &mut usize,
    pending: &mut String,
    pending_off: &mut usize,
    toks: &mut Vec<Token>,
) {
    let b = s.as_bytes();
    let n = b.len();
    let at = *i;
    macro_rules! flush_into {
        () => {
            if !pending.is_empty() {
                toks.push(Token { off: *pending_off, kind: Kind::Text(std::mem::take(pending)) });
            }
        };
    }
    match b.get(at + 1).copied() {
        Some(ch @ (b'(' | b'[')) => {
            flush_into!();
            toks.push(Token { off: at, kind: Kind::LatexBs(ch) });
            *i = at + 2;
        }
        Some(ch) if ch.is_ascii_alphabetic() => {
            // entity `\name` (+ optional `{}`): known → Leaf, unknown → bare letters (Escape).
            let mut j = at + 1;
            while j < n && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let name = &s[at + 1..j];
            if j + 1 < n && b[j] == b'{' && b[j + 1] == b'}' {
                j += 2;
            }
            match crate::entities::find(name) {
                Some(e) => {
                    flush_into!();
                    toks.push(Token {
                        off: at,
                        kind: Kind::Leaf(Inline::Entity {
                            name: e.name.to_string(),
                            latex: e.latex.to_string(),
                            latex_mathp: e.latex_mathp,
                            html: e.html.to_string(),
                            ascii: e.ascii.to_string(),
                            unicode: e.unicode.to_string(),
                        }),
                    });
                }
                None => {
                    flush_into!();
                    toks.push(Token { off: at, kind: Kind::Escape(name.to_string()) });
                }
            }
            *i = j;
        }
        Some(b'\n') | Some(b'\r') => {
            // `\`+eol → hard-break candidate; the resolver pairs it with the next Newline
            // (ctx.breaks) or renders `\` literally.
            flush_into!();
            toks.push(Token { off: at, kind: Kind::Punct(b'\\') });
            *i = at + 1;
        }
        Some(ch) if ch.is_ascii_punctuation() => {
            // Org escape: keep BOTH chars literally (no unescape).
            let w = char_len(ch);
            flush_into!();
            toks.push(Token { off: at, kind: Kind::Escape(s[at..at + 1 + w].to_string()) });
            *i = at + 1 + w;
        }
        _ => {
            // lone `\` (before digit / space / EOF): kept.
            flush_into!();
            toks.push(Token { off: at, kind: Kind::Escape("\\".to_string()) });
            *i = at + 1;
        }
    }
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

/// Trailing whitespace byte count of `s` (for the hard-break / fresh logic).
fn trailing_ws(s: &str) -> usize {
    s.bytes().rev().take_while(|&b| b == b' ' || b == b'\t').count()
}

/// M6-core resolver: text / break / escape / entity. Markers (`Delim`) and deferred specials
/// (`Punct` / `LatexBs`) render as their literal bytes for now (the emphasis / leaf / bracket
/// sub-steps refine them). `last_plain_char` is threaded already so the stateful emphasis gate
/// can read it once emphasis lands.
fn resolve(s: &str, toks: &mut [Token], ctx: Ctx) -> Vec<Inline> {
    let _ = s;
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    // last byte of the most recently FLUSHED Plain node (mldoc `last_plain_char`) — the
    // backward emphasis gate reads it; `None` = start of input / no plain yet.
    let mut last_plain_char: Option<u8> = None;
    let _ = &mut last_plain_char;

    let mut t = 0usize;
    while t < toks.len() {
        match &toks[t].kind {
            Kind::Text(txt) => {
                pending.push_str(txt);
            }
            Kind::Escape(x) => {
                pending.push_str(x);
            }
            Kind::Newline(c) => {
                if ctx.breaks {
                    flush_plain(&mut out, &mut pending, &mut last_plain_char);
                    out.push(Inline::Break);
                } else {
                    pending.push(*c as char);
                }
            }
            Kind::Leaf(node) => {
                flush_plain(&mut out, &mut pending, &mut last_plain_char);
                out.push(node.clone());
            }
            // M6-core stubs: render the literal byte(s); refined by later sub-steps.
            Kind::Delim { ch, len } => {
                for _ in 0..*len {
                    pending.push(*ch as char);
                }
            }
            Kind::Punct(c) => {
                pending.push(*c as char);
            }
            Kind::LatexBs(c) => {
                // unrefined: `\(`/`\[` literal (latex lands in a later sub-step).
                pending.push('\\');
                pending.push(*c as char);
            }
        }
        t += 1;
    }
    flush_plain(&mut out, &mut pending, &mut last_plain_char);
    out
}

/// Flush pending plain text, updating `last_plain_char` to its final byte (mldoc updates the
/// backward-gate state only when a Plain node is emitted).
fn flush_plain(out: &mut Vec<Inline>, pending: &mut String, last_plain_char: &mut Option<u8>) {
    if !pending.is_empty() {
        *last_plain_char = pending.bytes().next_back();
        flush(out, pending);
    }
    let _ = trailing_ws; // used once emphasis lands
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
}

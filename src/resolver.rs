//! lsdoc inline resolver (v0.2) — ONE ctx-aware pass over the lexer's tokens → `Vec<Inline>`.
//!
//! Byte-offset-driven and leftmost-greedy: walks the token stream once, applying context and
//! pairing emphasis/brackets. Built milestone-by-milestone alongside the v1 scanner behind
//! the `LSDOC_INLINE_V2` seam; validated by diffing `resolve(lex(s))` against
//! `crate::inline::parse_inline` (byte-exact to mldoc) over fuzzed inputs.
//!
//! **M0** core (text/break/escape/entity/code). **M1** emphasis: mldoc's *leftmost opener →
//! first FORWARD valid closer, flat content reparsed* — NOT a CommonMark backward
//! `openers_bottom` stack (that gives a different tree). Linear via a per-(marker,len)
//! `no_closer` forward floor. Deferred `Punct` tokens still render literally (M2/M3).

use crate::inline::{is_underscore_delim, is_ws_or_nl};
use crate::lexer::{lex, Kind, Token};
use crate::projection::Inline;

/// Active constructs (mirrors v1's `Ctx`; grows as families migrate).
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
    /// Whether a `\n` is a `Break` node (true) or literal text (false — emphasis content).
    pub breaks: bool,
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx { breaks: true }
    }
    /// Restricted emphasis-content context (mldoc `aux_nested_emphasis`): breaks become
    /// literal; emphasis/links/code/escapes stay on (the latter handled as M2/M3 land).
    fn emph() -> Ctx {
        Ctx { breaks: false }
    }
}

/// Parse a run of inline markup (top-level Markdown context).
pub(crate) fn parse_inline(text: &str) -> Vec<Inline> {
    parse_ctx(text, Ctx::top())
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
    let real_dbl = if has_brk { crate::inline::build_real_dbl(s) } else { Vec::new() };
    let mut real_dbl_cur = 0usize;
    let mut crlf = first_crlf(bb, 0);

    let mut t = 0usize;
    while t < toks.len() {
        // `[[…]]` dispatch (M2a): nested-link then page-ref, leftmost-greedy with byte-offset
        // resync. Other `[` uses (md-link / hiccup / footnote) land in M2b — for now a `[`
        // that doesn't open a `[[…]]` renders literally.
        if matches!(toks[t].kind, Kind::Punct(b'[')) {
            let off = toks[t].off;
            let mut consumed = false;
            if s[off..].starts_with("[[") {
                if nested_close.contains_key(&off) {
                    if let Some((end, content)) = crate::inline::parse_nested_link(s, off) {
                        flush(&mut out, &mut pending);
                        out.push(Inline::NestedLink { content });
                        t = resync(toks, t, end);
                        consumed = true;
                    }
                }
                if !consumed {
                    while real_dbl.get(real_dbl_cur).is_some_and(|&p| p < off + 2) {
                        real_dbl_cur += 1;
                    }
                    if let Some(&d) = real_dbl.get(real_dbl_cur) {
                        if off > crlf {
                            crlf = first_crlf(bb, off);
                        }
                        if d > off + 2 && crlf > d {
                            if let Some((end, name, full)) = crate::inline::parse_page_ref(s, off) {
                                flush(&mut out, &mut pending);
                                out.push(Inline::Link {
                                    url: crate::projection::Url::PageRef { v: name },
                                    label: vec![],
                                    full,
                                    image: false,
                                    metadata: String::new(),
                                    title: None,
                                });
                                t = resync(toks, t, end);
                                consumed = true;
                            }
                        }
                    }
                }
            }
            if !consumed {
                pending.push('[');
                t += 1;
            }
            continue;
        }

        // Non-delimiter tokens pass straight through.
        if !matches!(toks[t].kind, Kind::Delim { .. }) {
            match &toks[t].kind {
                Kind::Text(txt) => pending.push_str(txt),
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
                }
                Kind::Leaf(node) => {
                    flush(&mut out, &mut pending);
                    out.push(node.clone());
                }
                Kind::Punct(c) => pending.push(*c as char),
                Kind::Delim { .. } => unreachable!(),
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

/// After consuming a construct's byte extent `[_, end)`, advance the token cursor to the
/// first token at/after `end` (the leftmost-greedy resync — interior tokens are discarded).
fn resync(toks: &[Token], mut t: usize, end: usize) -> usize {
    while t < toks.len() && toks[t].off < end {
        t += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_count(tokens: &[&str], iters: usize, seed0: u64) -> usize {
        let mut seed = seed0;
        let mut rng = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let mut fails = 0usize;
        let mut shown = 0usize;
        for _ in 0..iters {
            let len = 1 + rng() % 10;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(tokens[rng() % tokens.len()]);
            }
            let a = crate::inline::parse_inline(&s);
            let b = parse_inline(&s);
            if a != b {
                fails += 1;
                if shown < 25 {
                    shown += 1;
                    eprintln!("DIFF {:?}\n   v1={:?}\n   v2={:?}\n", s, a, b);
                }
            }
        }
        fails
    }

    /// M0 core families (text/break/escape/entity/code) — no markers/brackets.
    #[test]
    fn v2_matches_v1_m0_core() {
        const TOKENS: &[&str] = &[
            "a", "b", "c", "1", " ", "  ", "\n", "word", "café", "中",
            "\\!", "\\,", "\\\\", "\\;", "\\Delta", "\\AA", "\\foo", "`co`", "``d``", "`x", "z",
        ];
        assert_eq!(diff_count(TOKENS, 300_000, 0xC0FFEE_1234_5678), 0);
    }

    /// M1 emphasis: markers + the core. Exercises run-length, flanking, nesting, surplus.
    #[test]
    fn v2_matches_v1_m1_emphasis() {
        const TOKENS: &[&str] = &[
            "a", "b", "c", " ", "\n", "word", "x",
            "*", "**", "***", "****", "_", "__", "___", "~~", "^^", "==",
            "*a*", "_b_", "**c**", "`co`", "\\*", "\\_",
        ];
        assert_eq!(diff_count(TOKENS, 500_000, 0xE3_4F_19), 0);
    }

    /// M2a `[[…]]`: page-ref + nested-link (with the dual escape disciplines) + core +
    /// emphasis. NO `](`/`[:`/`[^` yet (those are M2b) — so no md-link/hiccup/footnote.
    #[test]
    fn v2_matches_v1_m2a_dblbracket() {
        // NOTE: no `\[`/`\]`/`\(`/`$` — those are latex (M3), not escapes, at top level.
        const TOKENS: &[&str] = &[
            "a", "b", " ", "\n", "word", "x",
            "[[", "]]", "[", "]", "[[Foo]]", "[[a b]]", "[[a[[b]]c]]", "[[x]",
            "\\]", "\\!", "*", "**", "_", "~~", "`co`",
        ];
        assert_eq!(diff_count(TOKENS, 500_000, 0x5A_2B_71), 0);
    }

    /// Exhaustive small enumeration: every short string over {`*`,`_`,`a`,space} — covers
    /// the surplus / empty-content / flanking corners the review flagged are fuzz-sparse.
    #[test]
    fn v2_matches_v1_emphasis_exhaustive() {
        let alpha = [b'*', b'_', b'a', b' '];
        let mut fails = 0usize;
        let mut shown = 0usize;
        for length in 1..=7usize {
            let total = alpha.len().pow(length as u32);
            for mut idx in 0..total {
                let mut bytes = Vec::with_capacity(length);
                for _ in 0..length {
                    bytes.push(alpha[idx % alpha.len()]);
                    idx /= alpha.len();
                }
                let s = std::str::from_utf8(&bytes).unwrap();
                let a = crate::inline::parse_inline(s);
                let b = parse_inline(s);
                if a != b {
                    fails += 1;
                    if shown < 30 {
                        shown += 1;
                        eprintln!("DIFF {:?}\n   v1={:?}\n   v2={:?}\n", s, a, b);
                    }
                }
            }
        }
        assert_eq!(fails, 0, "{fails} exhaustive emphasis divergences");
    }
}

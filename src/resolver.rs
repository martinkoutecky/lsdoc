//! lsdoc inline resolver (v0.2) — ONE ctx-aware pass over the lexer's tokens → `Vec<Inline>`.
//!
//! The resolver is byte-offset-driven and leftmost-greedy: it walks the token stream once,
//! applying context (which constructs are live) and pairing brackets/emphasis with a stack.
//! Built milestone-by-milestone alongside the v1 scanner behind the `LSDOC_INLINE_V2` seam;
//! validated by diffing `resolve(lex(s))` against `crate::inline::parse_inline` (which is
//! byte-exact to mldoc) over fuzzed inputs. See DESIGN-lsdoc-v2.md / the plan.
//!
//! **M0** resolves the core families (text / break / hardbreak / escape / entity / code) and
//! renders deferred `Punct` tokens literally. M1 adds emphasis (forward first-valid-closer +
//! `no_closer` floor — NOT a backward stack), M2 brackets, M3 the leaves.

use crate::lexer::{lex, Kind, Token};
use crate::projection::Inline;

/// Active constructs (mirrors v1's `Ctx`; grows as families migrate). M0 only consults
/// `breaks` (whether a `\n` is a `Break` node or literal text — off in emphasis content).
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
    pub breaks: bool,
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx { breaks: true }
    }
}

/// Parse a run of inline markup (top-level Markdown context).
pub(crate) fn parse_inline(text: &str) -> Vec<Inline> {
    resolve(&lex(text), Ctx::top())
}

fn resolve(toks: &[Token], ctx: Ctx) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut pending = String::new();
    for t in toks {
        match &t.kind {
            Kind::Text(s) => pending.push_str(s),
            Kind::Newline(c) => {
                if ctx.breaks {
                    flush(&mut out, &mut pending);
                    out.push(Inline::Break);
                } else {
                    pending.push(*c as char);
                }
            }
            Kind::HardBreak => {
                flush(&mut out, &mut pending);
                out.push(Inline::HardBreak);
            }
            Kind::Leaf(node) => {
                flush(&mut out, &mut pending);
                out.push(node.clone());
            }
            // M0: deferred special bytes render as their literal char (all are ASCII).
            Kind::Punct(c) => pending.push(*c as char),
        }
    }
    flush(&mut out, &mut pending);
    out
}

fn flush(out: &mut Vec<Inline>, pending: &mut String) {
    if !pending.is_empty() {
        out.push(Inline::Plain { text: std::mem::take(pending) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The differential oracle: `resolve(lex(s))` must equal v1 `inline::parse_inline(s)`
    /// over the construct families the resolver currently handles. M0 alphabet = the core
    /// only (NO markers/brackets/`$`/`#` — those are deferred to M1+ and would form
    /// constructs in v1 that M0 renders as literal text). The alphabet grows per milestone.
    #[test]
    fn v2_matches_v1_m0_core() {
        const TOKENS: &[&str] = &[
            "a", "b", "c", "1", " ", "  ", "\n", "word", "café", "中",
            "\\!", "\\,", "\\\\", "\\;", "\\Delta", "\\AA", "\\foo",
            "`co`", "``d``", "`x", "z",
        ];
        let mut seed: u64 = 0xC0FFEE_1234_5678;
        let mut rng = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let mut fails = 0usize;
        let mut shown = 0usize;
        for _ in 0..300_000 {
            let len = 1 + rng() % 9;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(TOKENS[rng() % TOKENS.len()]);
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
        assert_eq!(fails, 0, "{fails} resolver-vs-v1 divergences (M0 core)");
    }
}

//! lsdoc inline lexer (v0.2) — ONE ctx-free pass, bytes → `Vec<Token>` with byte offsets.
//!
//! The lexer resolves ONLY the unconditional, always-on raw/leaf constructs (escapes,
//! entities, code spans — their content is raw so inner markers vanish), and **marks /
//! emits everything else as typed tokens** for the ctx-aware [`crate::resolver`]. It never
//! rewrites bytes for ctx-dependent constructs (latex/tags/macros/brackets/emphasis) and
//! never forward-scans for a closer — each byte is classified once.
//!
//! Built milestone-by-milestone: **M0** handles the core (text / break / hardbreak /
//! escape / entity / code) and emits every other special byte as a deferred [`Kind::Punct`]
//! (the resolver renders it literally for now). M1 refines emphasis markers into delimiter
//! runs, M2 the brackets, etc. — the token model is designed to absorb those without a
//! redesign (every token already carries its byte offset).

use crate::inline::{char_len, is_ws, is_ws_or_nl};
use crate::projection::Inline;

/// A classified unit of the inline stream, tagged with its start byte offset (the resolver
/// is byte-offset-driven: leaf predicates return a byte extent and it resyncs by offset).
pub(crate) struct Token {
    /// Byte offset of this token's start. Unused at M0; the resolver becomes byte-offset-
    /// driven at M2 (leaf predicates return a byte extent → resync to the token at that
    /// offset), so it's recorded from the start to avoid a later token-model change.
    #[allow(dead_code)]
    pub off: usize,
    pub kind: Kind,
}

impl Token {
    #[inline]
    pub(crate) fn rebase(&mut self, delta: usize) {
        self.off += delta;
        if let Kind::Text { end } = &mut self.kind {
            *end += delta;
        }
    }

    #[inline]
    pub(crate) fn text<'a>(&self, source: &'a str) -> Option<&'a str> {
        match self.kind {
            Kind::Text { end } => Some(&source[self.off..end]),
            _ => None,
        }
    }
}

pub(crate) enum Kind {
    /// Literal source-identical text run; `Token::off..end` slices the original input.
    Text { end: usize },
    /// A bare `\n` or `\r`. The resolver decides `Break` / `HardBreak` / literal — hard-break
    /// detection (`>=2` trailing spaces before a `\n`) is CTX-DEPENDENT (off in emphasis
    /// content), so it lives in the resolver, not here.
    Newline(u8),
    /// A fully-resolved self-contained leaf (Code, Entity) — passes straight through.
    Leaf(Inline),
    /// A resolved escape `\X` (the literal char(s), backslash dropped) / lone `\` / an
    /// unknown `\letters` entity (the bare letters). A SEPARATE token (not merged into Text)
    /// because the position right after it is a FRESH dispatch point in mldoc.
    Escape(String),
    /// An emphasis delimiter run: `len` copies of `ch` (`* _ ~ ^ =`). All flanking / empty-
    /// content / `_`-gate validity is evaluated per-pattern by the resolver against the raw
    /// bytes (it needs the char after the *k* opener markers, not the whole run).
    Delim { ch: u8, len: usize },
    /// A latex-backslash opener `\(` / `\[` (the byte is `(` or `[`). CTX-dependent: the
    /// resolver makes a Latex span when `ctx.latex` + a closer exists, else an escape (the
    /// `(`/`[` literal, backslash dropped). Deferred here because the lexer is ctx-free.
    LatexBs(u8),
    /// A single special byte deferred to a later milestone's resolver logic (`$ [ ] ( ) { }
    /// < > # ! @`). M0/M1 render it as its literal char; M2/M3 reclassify into bracket/leaf.
    Punct(u8),
}

/// Emphasis delimiter markers (grouped into `Delim` runs).
#[inline]
fn is_marker(c: u8) -> bool {
    matches!(c, b'*' | b'_' | b'~' | b'^' | b'=')
}

/// Is `c` a byte the lexer must treat specially (stops a plain run)? Backslash, backtick and
/// markers have dedicated handling; the rest become deferred `Punct` tokens for now.
#[inline]
fn is_special(c: u8) -> bool {
    matches!(c, b'\\' | b'`')
        || is_marker(c)
        || matches!(
            c,
            b'$' | b'[' | b']' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b'#' | b'!' | b'@'
        )
}

#[inline]
fn is_plain_stop(c: u8) -> bool {
    is_ws_or_nl(c) || is_special(c)
}

/// mldoc `md_escape_chars`: every ASCII punctuation char.
#[inline]
fn is_escape_char(c: u8) -> bool {
    c.is_ascii_punctuation()
}

/// Lex `s` (markdown) into tokens. Ctx-free: the same bytes always lex the same way; the
/// resolver applies context (e.g. whether `Newline` is a Break) afterwards.
pub(crate) fn lex(s: &str) -> Vec<Token> {
    let b = s.as_bytes();
    let n = b.len();
    let mut toks: Vec<Token> = Vec::new();
    let mut i = 0usize;
    // pending source-identical plain text, flushed lazily into one Text token.
    let mut pending_start: Option<usize> = None;
    let mut pending_end = 0usize;
    macro_rules! flush {
        () => {
            if let Some(start) = pending_start.take() {
                debug_assert!(pending_end > start);
                toks.push(Token {
                    off: start,
                    kind: Kind::Text { end: pending_end },
                });
            }
        };
    }
    macro_rules! push_pending {
        ($off:expr, $end:expr) => {{
            let off = $off;
            let end = $end;
            if pending_start.is_none() {
                pending_start = Some(off);
            } else {
                debug_assert_eq!(pending_end, off);
            }
            // A1: charge the scanned plain bytes (each input byte enters a Text span at most once).
            crate::metrics::scan_work(end - off);
            pending_end = end;
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
                // whitespace run → its OWN Text token (not merged into ordinary text), so
                // construct ends at a ws align with a token boundary and the position right
                // after ws is a fresh dispatch point (bare-url / keyword-timestamp detection).
                // Hard-break (>=2 trailing spaces before a `\n`) is decided by the resolver.
                flush!();
                let start = i;
                while i < n && is_ws(b[i]) {
                    i += 1;
                }
                crate::metrics::scan_work(i - start); // A1: charge the copied ws bytes
                toks.push(Token {
                    off: start,
                    kind: Kind::Text { end: i },
                });
            }
            b'\\' => lex_backslash(s, &mut i, &mut pending_start, &mut pending_end, &mut toks),
            b'`' => {
                // Phase D: emit a backtick as a ONE-BYTE `Punct`; the resolver recognizes code
                // spans LAZILY at dispatch (like tags/links), reusing `code_span`. Pre-building a
                // multi-byte code `Leaf` here is what let a tag consuming a backtick force a
                // non-local `resync` re-lex (bug 2b, code-leaf O(n²)). A `` ` `` is a marker-delim
                // (fresh-making); the position after it is a fresh dispatch point (`` `((uuid)) ``
                // → `` ` `` + block-ref).
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Punct(b'`'),
                });
                i += 1;
            }
            _ if is_marker(c) => {
                // group a run of the same emphasis marker into one Delim token.
                flush!();
                let mut j = i;
                while j < n && b[j] == c {
                    j += 1;
                }
                toks.push(Token {
                    off: i,
                    kind: Kind::Delim { ch: c, len: j - i },
                });
                i = j;
            }
            _ if is_special(c) => {
                // deferred special byte (brackets / $ / # / !) — render literally for now.
                flush!();
                toks.push(Token {
                    off: i,
                    kind: Kind::Punct(c),
                });
                i += 1;
            }
            _ => {
                // ordinary plain run: until a special / ws / nl byte.
                // All stop bytes are ASCII, so walking UTF-8 bytes one at a time cannot
                // stop inside a multibyte scalar; continuation bytes never match.
                let start = i;
                i += 1;
                while i < n {
                    if is_plain_stop(b[i]) {
                        break;
                    }
                    i += 1;
                }
                push_pending!(start, i);
            }
        }
    }
    flush!();
    toks
}

/// `\`-dispatch (M0): entity `\letters` (+ optional `{}`), escape `\punct`, lone `\`.
/// NOTE latex `\(`/`\[` is ctx-dependent (latex vs escape-to-`(`); M0 treats it as escape
/// (rendered `(`/`[`) — correct only outside the latex contexts M3 will add. The M0 oracle
/// alphabet excludes `\(`/`\[`/`$`, so this placeholder is never exercised wrongly.
fn lex_backslash(
    s: &str,
    i: &mut usize,
    pending_start: &mut Option<usize>,
    pending_end: &mut usize,
    toks: &mut Vec<Token>,
) {
    let b = s.as_bytes();
    let n = b.len();
    let at = *i;
    match b.get(at + 1).copied() {
        Some(ch @ (b'(' | b'[')) => {
            // `\(` / `\[` — defer to the resolver (latex vs escape is ctx-dependent).
            if let Some(start) = pending_start.take() {
                toks.push(Token {
                    off: start,
                    kind: Kind::Text { end: *pending_end },
                });
            }
            toks.push(Token {
                off: at,
                kind: Kind::LatexBs(ch),
            });
            *i = at + 2;
        }
        Some(ch) if ch.is_ascii_alphabetic() => {
            // entity `\letters` (+ optional `{}`): known name → Entity, else bare letters.
            let start = at + 1;
            let mut j = start;
            while j < n && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            crate::metrics::scan_work(j - start + usize::from(j < n));
            let name = &s[start..j];
            if s[j..].starts_with("{}") {
                crate::metrics::scan_work(2);
                j += 2;
            }
            match crate::entities::find(name) {
                Some(e) => {
                    if let Some(start) = pending_start.take() {
                        toks.push(Token {
                            off: start,
                            kind: Kind::Text { end: *pending_end },
                        });
                    }
                    toks.push(Token {
                        off: at,
                        kind: Kind::Leaf(Inline::Entity {
                            // scan-owner: (a) consumed-on-match — Markdown entity strings copied after run is consumed
                            name: e.name.to_string(),
                            latex: e.latex.to_string(),
                            latex_mathp: e.latex_mathp,
                            html: e.html.to_string(),
                            ascii: e.ascii.to_string(),
                            unicode: e.unicode.to_string(),
                            span: None,
                        }),
                    });
                }
                None => {
                    // unknown entity → the bare letters, as a fresh-making Escape token.
                    flush_into(pending_start, pending_end, toks);
                    crate::metrics::scan_work(name.len());
                    toks.push(Token {
                        off: at,
                        kind: Kind::Escape(name.to_string()),
                    });
                }
            }
            *i = j;
        }
        Some(ch) if is_escape_char(ch) => {
            // escape: drop the backslash, keep the punctuation literally (Escape token).
            let w = char_len(ch);
            flush_into(pending_start, pending_end, toks);
            crate::metrics::scan_work(w);
            toks.push(Token {
                off: at,
                kind: Kind::Escape(s[at + 1..at + 1 + w].to_string()),
            });
            *i = at + 1 + w;
        }
        _ => {
            // lone backslash (before digit / space / eol / EOF): kept (Escape token).
            flush_into(pending_start, pending_end, toks);
            toks.push(Token {
                off: at,
                kind: Kind::Escape("\\".to_string()),
            });
            *i = at + 1;
        }
    }
}

/// Flush the lexer's pending text run into a `Text` token (if non-empty).
fn flush_into(pending_start: &mut Option<usize>, pending_end: &mut usize, toks: &mut Vec<Token>) {
    if let Some(start) = pending_start.take() {
        toks.push(Token {
            off: start,
            kind: Kind::Text { end: *pending_end },
        });
    }
}

/// `` `…` `` (single) / ``` ``…`` ``` (double-backtick) code span → (Code node, end). The
/// content is raw (a lexer mode: no inner token recognition), so emphasis/brackets inside
/// code never become tokens. `pub(crate)` so the resolver can recognize code spans LAZILY at
/// dispatch time (Phase D) instead of the lexer pre-building them as multi-byte `Leaf`s.
pub(crate) fn code_span(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at + 1) != Some(&b'`') {
        let start = at + 1;
        let mut j = start;
        while j < n && b[j] != b'`' && b[j] != b'\n' && b[j] != b'\r' {
            j += 1;
        }
        crate::metrics::scan_work(j - start + usize::from(j < n));
        if j > start && j < n && b[j] == b'`' {
            crate::metrics::scan_work(j - start);
            return Some((
                Inline::Code {
                    text: s[start..j].to_string(),
                    span: None,
                },
                j + 1,
            ));
        }
        return None;
    }
    let start = at + 2;
    let end = crate::inline::find_sub(b, start, b"``")?;
    crate::metrics::scan_work(end - start);
    Some((
        Inline::Code {
            text: s[start..end].to_string(),
            span: None,
        },
        end + 2,
    ))
}

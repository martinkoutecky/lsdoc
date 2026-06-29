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

pub(crate) enum Kind {
    /// Literal text run; escapes already resolved into it (top-level md-escape semantics).
    Text(String),
    /// A bare `\n` or `\r` (the resolver emits `Break` when `ctx.breaks`, else literal).
    Newline(u8),
    /// `>=2` spaces/tabs immediately before a `\n`.
    HardBreak,
    /// A fully-resolved self-contained leaf (Code, Entity) — passes straight through.
    Leaf(Inline),
    /// A single special byte deferred to a later milestone's resolver logic. M0 renders it
    /// as its literal char; M1+ reclassify these (`* _ ~ ^ = $ [ ] ( ) { } < > # !`) into
    /// delimiter/bracket handling.
    Punct(u8),
}

/// Is `c` a byte the lexer must treat specially (stops a plain run)? Backslash and backtick
/// are handled by dedicated arms; the rest become deferred `Punct` tokens for now.
#[inline]
fn is_special(c: u8) -> bool {
    matches!(
        c,
        b'\\' | b'`'
            | b'*' | b'_' | b'~' | b'^' | b'='
            | b'$' | b'[' | b']' | b'(' | b')' | b'{' | b'}'
            | b'<' | b'>' | b'#' | b'!'
    )
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
    // pending plain text, flushed lazily into one Text token (mldoc concat_plains).
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
                // hard break: >=2 spaces/tabs immediately before a '\n'.
                let mut j = i;
                while j < n && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                if j - i >= 2 && j < n && b[j] == b'\n' {
                    flush!();
                    toks.push(Token { off: i, kind: Kind::HardBreak });
                    i = j + 1;
                    continue;
                }
                let start = i;
                while i < n && is_ws(b[i]) {
                    i += 1;
                }
                push_pending!(start, &s[start..i]);
            }
            b'\\' => lex_backslash(s, &mut i, &mut pending, &mut pending_off, &mut toks),
            b'`' => {
                if let Some((node, end)) = code_span(s, i) {
                    flush!();
                    toks.push(Token { off: i, kind: Kind::Leaf(node) });
                    i = end;
                } else {
                    push_pending!(i, "`");
                    i += 1;
                }
            }
            _ if is_special(c) => {
                // deferred special byte (markers / brackets / $ / # / !) — M0 keeps it as a
                // single Punct token (rendered literally); M1+ reclassify.
                flush!();
                toks.push(Token { off: i, kind: Kind::Punct(c) });
                i += 1;
            }
            _ => {
                // ordinary plain run: until a special / ws / nl byte.
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

/// `\`-dispatch (M0): entity `\letters` (+ optional `{}`), escape `\punct`, lone `\`.
/// NOTE latex `\(`/`\[` is ctx-dependent (latex vs escape-to-`(`); M0 treats it as escape
/// (rendered `(`/`[`) — correct only outside the latex contexts M3 will add. The M0 oracle
/// alphabet excludes `\(`/`\[`/`$`, so this placeholder is never exercised wrongly.
fn lex_backslash(
    s: &str,
    i: &mut usize,
    pending: &mut String,
    pending_off: &mut usize,
    toks: &mut Vec<Token>,
) {
    let b = s.as_bytes();
    let n = b.len();
    let at = *i;
    let set_off = |pending: &str, pending_off: &mut usize| {
        if pending.is_empty() {
            *pending_off = at;
        }
    };
    match b.get(at + 1).copied() {
        Some(ch) if ch.is_ascii_alphabetic() => {
            // entity `\letters` (+ optional `{}`): known name → Entity, else bare letters.
            let start = at + 1;
            let mut j = start;
            while j < n && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let name = &s[start..j];
            if s[j..].starts_with("{}") {
                j += 2;
            }
            match crate::entities::find(name) {
                Some(e) => {
                    if !pending.is_empty() {
                        toks.push(Token {
                            off: *pending_off,
                            kind: Kind::Text(std::mem::take(pending)),
                        });
                    }
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
                    set_off(pending, pending_off);
                    pending.push_str(name);
                }
            }
            *i = j;
        }
        Some(ch) if is_escape_char(ch) => {
            // escape: drop the backslash, keep the punctuation literally.
            let w = char_len(ch);
            set_off(pending, pending_off);
            pending.push_str(&s[at + 1..at + 1 + w]);
            *i = at + 1 + w;
        }
        _ => {
            // lone backslash (before digit / space / eol / EOF): kept.
            set_off(pending, pending_off);
            pending.push('\\');
            *i = at + 1;
        }
    }
}

/// `` `…` `` (single) / ``` ``…`` ``` (double-backtick) code span → (Code node, end). The
/// content is raw (a lexer mode: no inner token recognition), so emphasis/brackets inside
/// code never become tokens.
fn code_span(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at + 1) != Some(&b'`') {
        let start = at + 1;
        let mut j = start;
        while j < n && b[j] != b'`' && b[j] != b'\n' && b[j] != b'\r' {
            j += 1;
        }
        if j > start && j < n && b[j] == b'`' {
            return Some((Inline::Code { text: s[start..j].to_string() }, j + 1));
        }
        return None;
    }
    let start = at + 2;
    let end = crate::inline::find_sub(b, start, b"``")?;
    Some((Inline::Code { text: s[start..end].to_string() }, end + 2))
}

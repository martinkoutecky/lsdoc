//! Inline parser — milestones M3 (inline core) + M4 (Logseq dialect inline).
//!
//! A single left-to-right byte scanner (`Scanner`) that turns a block's raw inline
//! text into `Vec<Inline>`, behavior-equivalent to mldoc 1.5.7's inline grammar
//! (`lib/syntax/inline.ml`, verified against the live oracle).
//!
//! ## Dispatch model (mirrors mldoc's `inline_choices`)
//! At each iteration we dispatch on the current byte to the one parser mldoc would
//! try; on failure we fall back to a *plain run* (greedy run of "ordinary" bytes,
//! stopping at a marker delimiter or whitespace), exactly as mldoc's `plain` does.
//! A marker byte (`* _ ^ [ ~ ` = $ #`) whose construct fails is emitted as a single
//! literal char; an ordinary dispatch byte (`< { ! @ (`) whose construct fails is
//! swallowed into the following plain run (it is not a `plain` delimiter in mldoc),
//! which is why e.g. `(https://a.com)` stays plain but `see https://a.com` links.
//!
//! ## Emphasis (the hard part — SPEC §4)
//! mldoc's emphasis is recursive-descent `between_string` (NOT a CommonMark
//! delimiter stack): an opener matches the *first* later valid closer of the same
//! delimiter, content is flat, then re-parsed for nesting. We replicate the *output*
//! with a linear scan: a forward closer search per opener, plus a per-line
//! "no-closer" cache (`no_closer`) so a failed opener of pattern P never re-scans —
//! once we prove P has no closer before the next newline, later P openers on that
//! line short-circuit. Net: O(n) over the line (each pattern's failing scan happens
//! at most once per line; successful emphases consume their content). The content of
//! a matched emphasis is parsed recursively in a restricted context (`Ctx::emph`).
//! See DECISIONS.md.
//!
//! Complexity: the whole inline pass is O(n) amortized (code/page-ref/url/bracket
//! scans are over disjoint regions; emphasis is bounded by the no-closer cache).
//! BYTE-SAFETY: all `&str` slicing is at ASCII delimiter / run boundaries or via
//! `char_indices`, never mid-codepoint.

use crate::projection::{Inline, Url};
use std::collections::HashMap;

/// Parse a block's inline text into the observable inline projection.
pub fn parse_inline(text: &str) -> Vec<Inline> {
    let mut sc = Scanner::new(text, Ctx::top());
    sc.run();
    sc.finish()
}

/// Which constructs are active. Code, emphasis, links/page-refs/nested-links,
/// sub/superscript, escapes and plain are ALWAYS on; the rest are gated so the
/// restricted emphasis-content / label contexts match mldoc's re-parse grammars.
#[derive(Clone, Copy)]
struct Ctx {
    tags: bool,
    block_refs: bool,
    macros: bool,
    latex: bool,
    urls: bool,
    images: bool,
    timestamps: bool,
    footnotes: bool,
    breaks: bool,
    html: bool,
    autolinks: bool,
}

impl Ctx {
    fn top() -> Ctx {
        Ctx {
            tags: true,
            block_refs: true,
            macros: true,
            latex: true,
            urls: true,
            images: true,
            timestamps: true,
            footnotes: true,
            breaks: true,
            html: true,
            autolinks: true,
        }
    }
    /// Restricted context for emphasis content: only emphasis, links, sub/sup, code,
    /// plain (matches mldoc's `aux_nested_emphasis` re-parse, which does NOT see
    /// tags, block-refs, macros, latex, bare URLs, images, timestamps, footnotes).
    fn emph() -> Ctx {
        Ctx {
            tags: false,
            block_refs: false,
            macros: false,
            latex: false,
            urls: false,
            images: false,
            timestamps: false,
            footnotes: false,
            // mldoc's whitespace_chars include '\n', so emphasis SPANS newlines, but
            // the `\n` is captured as literal plain text inside the emphasis (the
            // re-parse has no breakline rule) — NOT a Break node. So breaks stays off.
            breaks: false,
            html: false,
            autolinks: false,
        }
    }
}

struct Scanner<'a> {
    s: &'a str,
    b: &'a [u8],
    n: usize,
    i: usize,
    ctx: Ctx,
    out: Vec<Inline>,
    pending: String, // accumulated plain text, flushed lazily (mldoc concat_plains)
    /// Cache: emphasis patterns (marker,len) proven to have no valid closer ahead.
    no_closer: HashMap<(u8, usize), bool>,
    /// Cache: 2-byte closer sequences proven absent from `self.i` onward. Absence is
    /// monotone (the scan window only shrinks), so this makes runs of unmatched
    /// openers (`[[[[…`, `((((…`, `{{{{…`) linear instead of O(n²).
    absent: std::collections::HashSet<[u8; 2]>,
}

// ---- byte classes ---------------------------------------------------------

#[inline]
pub(crate) fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r'
}
#[inline]
pub(crate) fn is_ws_or_nl(c: u8) -> bool {
    is_ws(c) || c == b'\n'
}
/// `plain` delimiters in mldoc (`markdown_plain_delims`, minus whitespace which we
/// test separately). A plain run stops at these (and at whitespace / newline).
#[inline]
fn is_marker_delim(c: u8) -> bool {
    matches!(
        c,
        b'\\' | b'_' | b'^' | b'[' | b'*' | b'~' | b'`' | b'=' | b'$' | b'#'
    )
}
/// mldoc `md_escape_chars`: every ASCII punctuation char (backslash included).
#[inline]
fn is_md_escape_char(c: u8) -> bool {
    c.is_ascii_punctuation()
}
/// mldoc `underline_emphasis_delims`: ASCII punctuation + whitespace (NOT letters/
/// digits, NOT non-ASCII). Used for `_`/`__` open-backward and close-forward gates.
#[inline]
pub(crate) fn is_underscore_delim(c: u8) -> bool {
    c.is_ascii_punctuation() || is_ws_or_nl(c)
}

impl<'a> Scanner<'a> {
    fn new(s: &'a str, ctx: Ctx) -> Scanner<'a> {
        Scanner {
            s,
            b: s.as_bytes(),
            n: s.len(),
            i: 0,
            ctx,
            out: Vec::new(),
            pending: String::new(),
            no_closer: HashMap::new(),
            absent: std::collections::HashSet::new(),
        }
    }

    /// Is the 2-byte sequence `needle` present at/after `self.i`? Caches absence so
    /// a run of unmatched openers doesn't re-scan to EOF each time.
    fn seq_present(&mut self, needle: [u8; 2]) -> bool {
        if self.absent.contains(&needle) {
            return false;
        }
        if find_sub(self.b, self.i, &needle).is_some() {
            true
        } else {
            self.absent.insert(needle);
            false
        }
    }

    fn finish(mut self) -> Vec<Inline> {
        self.flush();
        self.out
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            let t = std::mem::take(&mut self.pending);
            self.out.push(Inline::Plain { text: t });
        }
    }

    fn push(&mut self, node: Inline) {
        self.flush();
        self.out.push(node);
    }

    fn push_plain(&mut self, s: &str) {
        self.pending.push_str(s);
    }

    fn run(&mut self) {
        while self.i < self.n {
            let start = self.i;
            self.step();
            // Safety net: every step must make progress.
            if self.i == start {
                let c = self.b[self.i];
                let w = char_len(c);
                self.push_plain(&self.s[self.i..self.i + w]);
                self.i += w;
            }
        }
    }

    fn step(&mut self) {
        let c = self.b[self.i];
        match c {
            b'\n' => {
                if self.ctx.breaks {
                    self.push(Inline::Break);
                } else {
                    self.push_plain("\n");
                }
                self.i += 1;
            }
            b' ' | b'\t' | b'\r' => self.whitespace(),
            b'#' if self.ctx.tags => {
                if !self.try_tag() {
                    self.push_plain("#");
                    self.i += 1;
                }
            }
            b'*' | b'~' | b'^' | b'=' => {
                if !self.try_emphasis(c) {
                    self.plain_one();
                }
            }
            b'_' => {
                if !self.try_emphasis(b'_') && !self.try_subscript() {
                    self.plain_one();
                }
            }
            b'$' if self.ctx.latex => {
                if !self.try_latex_dollar() {
                    self.plain_one();
                }
            }
            b'\\' => self.backslash(),
            b'`' => {
                if !self.try_code() {
                    self.push_plain("`");
                    self.i += 1;
                }
            }
            b'[' => {
                if !self.try_bracket() {
                    self.push_plain("[");
                    self.i += 1;
                }
            }
            b'<' => {
                if !self.try_angle() {
                    self.plain_run(); // '<' is not a plain delim -> swallow run
                }
            }
            b'{' if self.ctx.macros => {
                if !self.try_macro() {
                    self.plain_run();
                }
            }
            b'!' if self.ctx.images => {
                if !self.try_image() {
                    self.plain_run();
                }
            }
            b'(' if self.ctx.block_refs => {
                if !self.try_block_ref() {
                    self.plain_run();
                }
            }
            _ => {
                // S C D s c d -> timestamp; else bare URL; else plain run.
                if self.ctx.timestamps && matches!(c, b'S' | b'C' | b'D' | b's' | b'c' | b'd') {
                    if self.try_timestamp_keyword() {
                        return;
                    }
                }
                if self.ctx.urls && self.try_bare_url() {
                    return;
                }
                self.plain_run();
            }
        }
    }

    /// Emit a single literal char (a failed marker delimiter), advancing by 1.
    fn plain_one(&mut self) {
        let w = char_len(self.b[self.i]);
        let seg = &self.s[self.i..self.i + w];
        self.pending.push_str(seg);
        self.i += w;
    }

    /// Greedy plain run: ordinary bytes until a marker delim, whitespace or newline.
    fn plain_run(&mut self) {
        let start = self.i;
        // always consume at least the first byte's char (may be a non-delim dispatch
        // char like '<','{','!','(' whose construct just failed).
        self.i += char_len(self.b[self.i]);
        while self.i < self.n {
            let c = self.b[self.i];
            if is_ws_or_nl(c) || is_marker_delim(c) {
                break;
            }
            self.i += char_len(c);
        }
        let seg = &self.s[start..self.i];
        self.pending.push_str(seg);
    }

    fn whitespace(&mut self) {
        // Hard break: a run of >=2 spaces/tabs immediately followed by '\n'.
        if self.ctx.breaks {
            let mut j = self.i;
            while j < self.n && (self.b[j] == b' ' || self.b[j] == b'\t') {
                j += 1;
            }
            if j - self.i >= 2 && j < self.n && self.b[j] == b'\n' {
                self.push(Inline::HardBreak);
                self.i = j + 1;
                return;
            }
        }
        // plain whitespace run (space/tab/\r), not crossing '\n'.
        let start = self.i;
        while self.i < self.n && is_ws(self.b[self.i]) {
            self.i += 1;
        }
        let seg = &self.s[start..self.i];
        self.pending.push_str(seg);
    }

    // ---- escapes / entities / latex backslash -----------------------------

    fn backslash(&mut self) {
        // mldoc dispatch at '\\': latex `\(`,`\[`  ;  entity `\letters`  ;  escape.
        if self.ctx.latex {
            if let Some(node) = self.parse_latex_backslash() {
                self.push(node);
                return;
            }
        }
        let next = self.b.get(self.i + 1).copied();
        match next {
            Some(c) if c.is_ascii_alphabetic() => {
                // entity: `\` + letters (+ optional `{}`). A name in the LaTeX entity
                // table → `Entity`; otherwise the bare letters as plain (backslash
                // dropped). The `{}` is consumed either way (mldoc: `\Delta{}G`→Entity
                // +"G", `\foo{}G`→"fooG").
                let start = self.i + 1;
                let mut j = start;
                while j < self.n && self.b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let name = &self.s[start..j];
                if self.s[j..].starts_with("{}") {
                    j += 2;
                }
                match crate::entities::find(name) {
                    Some(e) => self.push(Inline::Entity {
                        name: e.name.to_string(),
                        latex: e.latex.to_string(),
                        latex_mathp: e.latex_mathp,
                        html: e.html.to_string(),
                        ascii: e.ascii.to_string(),
                        unicode: e.unicode.to_string(),
                    }),
                    None => self.pending.push_str(name),
                }
                self.i = j;
            }
            Some(c) if is_md_escape_char(c) => {
                // escape: drop the backslash, emit the punctuation literally.
                let w = char_len(c);
                let seg = &self.s[self.i + 1..self.i + 1 + w];
                self.pending.push_str(seg);
                self.i += 1 + w;
            }
            _ => {
                // lone backslash (before digit / space / eol / EOF): keep it.
                self.push_plain("\\");
                self.i += 1;
            }
        }
    }

    fn parse_latex_backslash(&mut self) -> Option<Inline> {
        // `\( ... \)` inline ; `\[ ... \]` displayed.
        let open = self.b.get(self.i + 1).copied()?;
        let (close, mode) = match open {
            b'(' => ("\\)", "Inline"),
            b'[' => ("\\]", "Displayed"),
            _ => return None,
        };
        let body_start = self.i + 2;
        let end = find_sub(self.b, body_start, close.as_bytes())?;
        let body = self.s[body_start..end].to_string();
        self.i = end + 2;
        Some(Inline::Latex {
            mode: mode.to_string(),
            body,
        })
    }

    fn try_latex_dollar(&mut self) -> bool {
        // `$$ ... $$` displayed ; `$ ... $` inline. Content has no '$' / newline.
        let after = match self.b.get(self.i + 1) {
            Some(&c) => c,
            None => return false,
        };
        if after == b'$' {
            // displayed
            let body_start = self.i + 2;
            let end = find_sub_line(self.b, body_start, b"$$");
            if let Some(end) = end {
                let body = self.s[body_start..end].to_string();
                self.push(Inline::Latex {
                    mode: "Displayed".to_string(),
                    body,
                });
                self.i = end + 2;
                return true;
            }
            return false;
        }
        if after == b' ' {
            return false; // inline math can't start with space
        }
        // inline: from i+1, read until next '$' (no newline). Body must not end with
        // a space / '(' / '[' / '{'.
        let body_start = self.i + 1;
        let mut j = body_start;
        while j < self.n && self.b[j] != b'$' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j >= self.n || self.b[j] != b'$' {
            return false;
        }
        let last = self.b[j - 1];
        if matches!(last, b' ' | b'(' | b'[' | b'{') {
            return false;
        }
        let body = self.s[body_start..j].to_string();
        self.push(Inline::Latex {
            mode: "Inline".to_string(),
            body,
        });
        self.i = j + 1;
        true
    }

    // ---- code spans -------------------------------------------------------

    fn try_code(&mut self) -> bool {
        let second = self.b.get(self.i + 1).copied();
        // Single-backtick is tried first (mldoc `md_code = code_aux_p "`" <|> ...`):
        // it only applies when the next char is not itself a backtick.
        if second != Some(b'`') {
            let body_start = self.i + 1;
            let mut j = body_start;
            while j < self.n && self.b[j] != b'`' && self.b[j] != b'\n' && self.b[j] != b'\r' {
                j += 1;
            }
            if j > body_start && j < self.n && self.b[j] == b'`' {
                let body = self.s[body_start..j].to_string();
                self.push(Inline::Code { text: body });
                self.i = j + 1;
                return true;
            }
            return false;
        }
        // Double-backtick escape code: `` ... `` (content may include single `, no
        // newline restriction in mldoc's end_string; content MAY be empty, e.g.
        // ``````→ Code "" + `).
        let body_start = self.i + 2;
        if let Some(end) = find_sub(self.b, body_start, b"``") {
            {
                let body = self.s[body_start..end].to_string();
                self.push(Inline::Code { text: body });
                self.i = end + 2;
                return true;
            }
        }
        false
    }

    // ---- emphasis ---------------------------------------------------------

    /// Try to parse an emphasis starting at `self.i` (current byte == marker `c`).
    fn try_emphasis(&mut self, c: u8) -> bool {
        // Determine the candidate patterns (longest first), per mldoc dispatch.
        // '*'/'_': try ***/**/* (the *** form yields Italic[Bold]); ~/^/= : try ** only.
        // Cap the run measurement at 3 (the longest pattern) so a huge marker run
        // (`***…`) stays O(1) per position rather than O(run).
        let run = {
            let mut k = self.i;
            while k < self.n && self.b[k] == c && k - self.i < 3 {
                k += 1;
            }
            k - self.i
        };
        let candidates: &[(usize, &str, bool)] = match c {
            b'*' | b'_' => &[
                (3, "Bold", true),    // nested -> Italic[Bold]
                (2, "Bold", false),
                (1, "Italic", false),
            ],
            b'~' => &[(2, "Strike_through", false)],
            b'^' => &[(2, "Highlight", false)],
            b'=' => &[(2, "Highlight", false)],
            _ => return false,
        };
        for &(k, kind, nested) in candidates {
            if run < k {
                continue;
            }
            if let Some(node) = self.parse_emphasis_pattern(c, k, kind, nested) {
                self.push(node);
                return true;
            }
        }
        false
    }

    fn run_len(&self, pos: usize, c: u8) -> usize {
        let mut k = pos;
        while k < self.n && self.b[k] == c {
            k += 1;
        }
        k - pos
    }

    fn parse_emphasis_pattern(
        &mut self,
        c: u8,
        k: usize,
        kind: &str,
        nested: bool,
    ) -> Option<Inline> {
        let open_start = self.i;
        let content_start = open_start + k;
        // Left-flanking: the pattern must be followed by a non-whitespace char.
        let after = *self.b.get(content_start)?;
        if is_ws_or_nl(after) {
            return None;
        }
        // Empty content is invalid: if the next `k` bytes are the full closing
        // pattern, the closer sits at content_start (mldoc's content take_while1
        // needs >=1 char). A single marker byte that does NOT complete the pattern
        // (e.g. the 3rd `*` of `***.`) is literal content, not an empty closer.
        if content_start + k <= self.n
            && self.b[content_start..content_start + k].iter().all(|&x| x == c)
        {
            return None;
        }
        // For `_`/`__`/`___`: char before the opener must be an underscore-delim
        // (whitespace / punctuation / start-of-input). (markdown_underline backward.)
        if c == b'_' {
            if let Some(prev) = self.last_char_before(open_start) {
                if !is_underscore_delim(prev) {
                    return None;
                }
            }
        }
        // Per-line no-closer cache: skip the scan if we've already proven there is
        // no valid closer for this pattern before the next newline.
        let key = (c, k);
        if *self.no_closer.get(&key).unwrap_or(&false) {
            return None;
        }
        let closer = self.find_emphasis_closer(c, k, content_start);
        let closer = match closer {
            Some(q) => q,
            None => {
                self.no_closer.insert(key, true);
                return None;
            }
        };
        // For `_` close: char after the closing run must be an underscore-delim.
        if c == b'_' {
            let after_close = self.b.get(closer + k).copied();
            match after_close {
                None => {}
                Some(ac) => {
                    if !is_underscore_delim(ac) {
                        // not a valid `_` close here; treat whole pattern as failing.
                        return None;
                    }
                }
            }
        }
        let content = &self.s[content_start..closer];
        self.i = closer + k;
        let children = parse_inline_ctx(content, Ctx::emph());
        if nested {
            // *** -> Italic[Bold[content]]
            let inner = Inline::Emphasis {
                emph: kind.to_string(),
                children,
            };
            Some(Inline::Emphasis {
                emph: "Italic".to_string(),
                children: vec![inner],
            })
        } else {
            Some(Inline::Emphasis {
                emph: kind.to_string(),
                children,
            })
        }
    }

    /// Find the first valid closer of pattern (`c` repeated `k`) at position
    /// `> from`, scanning to end of input (mldoc's `whitespace_chars` include `\n`,
    /// so emphasis spans newlines). A closer at q is valid iff the byte before q is
    /// non-whitespace (right-flanking) and q is not inside a code span. Code spans in
    /// the content are skipped (markers in them can't close).
    fn find_emphasis_closer(&self, c: u8, k: usize, from: usize) -> Option<usize> {
        let mut j = from;
        while j < self.n {
            let cur = self.b[j];
            if cur == b'\\' {
                // a backslash-escaped char (incl. `\*`, `\_`, `` \` ``) can't close
                // an emphasis (mldoc treats it as literal content). Skip both bytes.
                j += 1;
                if j < self.n {
                    j += char_len(self.b[j]);
                }
                continue;
            }
            if cur == b'`' {
                // skip over a code span so a marker inside it can't close.
                if let Some(end) = self.code_span_end(j) {
                    j = end;
                    continue;
                }
            }
            if cur == c {
                // candidate run; ensure it is exactly the closing run of length k
                let rl = self.run_len(j, c);
                if rl >= k {
                    let before = self.b[j - 1]; // j > from >= content_start > 0
                    if !is_ws_or_nl(before) {
                        return Some(j);
                    }
                    // can't close here (whitespace before) -> skip the whole run.
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

    /// If a code span starts at `pos` (a backtick), return the byte index just past
    /// its closing delimiter; else None. (Used to skip code while seeking a closer.)
    fn code_span_end(&self, pos: usize) -> Option<usize> {
        let second = self.b.get(pos + 1).copied();
        if second != Some(b'`') {
            let start = pos + 1;
            let mut j = start;
            while j < self.n && self.b[j] != b'`' && self.b[j] != b'\n' && self.b[j] != b'\r' {
                j += 1;
            }
            if j > start && j < self.n && self.b[j] == b'`' {
                return Some(j + 1);
            }
            return None;
        }
        let start = pos + 2;
        if let Some(end) = find_sub(self.b, start, b"``") {
            return Some(end + 2); // double-backtick code may be empty
        }
        None
    }

    fn last_char_before(&self, pos: usize) -> Option<u8> {
        if pos == 0 {
            None
        } else {
            Some(self.b[pos - 1])
        }
    }

    fn try_subscript(&mut self) -> bool {
        // mldoc markdown subscript only matches `_{ ... }`. Rare; emit as plain-ish.
        if self.b.get(self.i + 1) != Some(&b'{') {
            return false;
        }
        // Keep it simple: do not model subscript nodes (not in the corpus); fall
        // through so `_` is handled as a literal delimiter.
        false
    }

    // ---- tags -------------------------------------------------------------

    fn try_tag(&mut self) -> bool {
        // self.i at '#'. Parse the tag name (mldoc Hash_tag.hashtag_name), splitting
        // into Plain runs and page-ref children.
        let name_start = self.i + 1;
        let (end, children) = parse_tag_name(self.s, name_start);
        if end == name_start || children.is_empty() {
            return false;
        }
        self.push(Inline::Tag { children });
        self.i = end;
        true
    }

    // ---- block refs -------------------------------------------------------

    fn try_block_ref(&mut self) -> bool {
        // `(( <non-')' ...> ))`
        if !self.s[self.i..].starts_with("((") {
            return false;
        }
        if !self.seq_present(*b"))") {
            return false; // no closing `))` ahead — keep `((((…` runs linear
        }
        let inner_start = self.i + 2;
        let mut j = inner_start;
        while j < self.n && self.b[j] != b')' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j == inner_start {
            return false; // empty
        }
        if j + 1 < self.n && self.b[j] == b')' && self.b[j + 1] == b')' {
            let inner = unescape(&self.s[inner_start..j]);
            let full = self.s[self.i..j + 2].to_string();
            self.push(Inline::Link {
                url: Url::BlockRef { v: inner },
                label: vec![],
                full,
            });
            self.i = j + 2;
            return true;
        }
        false
    }

    // ---- macros -----------------------------------------------------------

    fn try_macro(&mut self) -> bool {
        if !self.s[self.i..].starts_with("{{") {
            return false;
        }
        if !self.seq_present(*b"}}") {
            return false; // no closing `}}` ahead — keep `{{{{…` runs linear
        }
        // mldoc: `between "{{{" "}}}" <|> between "{{" "}}"` — try triple, then double.
        let candidates: &[(&str, &str)] = if self.s[self.i..].starts_with("{{{") {
            &[("{{{", "}}}"), ("{{", "}}")]
        } else {
            &[("{{", "}}")]
        };
        for &(open, close) in candidates {
            let inner_start = self.i + open.len();
            let mut j = inner_start;
            while j < self.n && self.b[j] != b'}' && self.b[j] != b'\n' && self.b[j] != b'\r' {
                j += 1;
            }
            if j == inner_start || !self.s[j..].starts_with(close) {
                continue;
            }
            let inner = &self.s[inner_start..j];
            if let Some((name, args)) = parse_macro(inner) {
                self.push(Inline::Macro { name, args });
                self.i = j + close.len();
                return true;
            }
        }
        false
    }

    // ---- bracket: footnote / nested-link / page-ref / markdown link -------

    fn try_bracket(&mut self) -> bool {
        // footnote `[^id]`
        if self.ctx.footnotes && self.b.get(self.i + 1) == Some(&b'^') {
            if let Some((end, name)) = parse_footnote_ref(self.s, self.i) {
                self.push(Inline::Fnref { name });
                self.i = end;
                return true;
            }
        }
        // Fast reject: page/nested links need `]]`, markdown links need `](`. If
        // neither is ahead, no `[`-construct can match (keeps `[[[[…` runs linear).
        if !self.seq_present(*b"]]") && !self.seq_present(*b"](") {
            return false;
        }
        // nested link `[[ ... [[..]] ... ]]`
        if self.s[self.i..].starts_with("[[") {
            if let Some((end, content)) = parse_nested_link(self.s, self.i) {
                self.push(Inline::NestedLink { content });
                self.i = end;
                return true;
            }
            // page ref `[[name]]`
            if let Some((end, name, full)) = parse_page_ref(self.s, self.i) {
                self.push(Inline::Link {
                    url: Url::PageRef { v: name },
                    label: vec![],
                    full,
                });
                self.i = end;
                return true;
            }
        }
        // markdown link `[label](url)`
        if let Some(link) = self.parse_markdown_link(self.i, false) {
            self.push(link.node);
            self.i = link.end;
            return true;
        }
        false
    }

    fn try_image(&mut self) -> bool {
        // `![label](url)` (image) — reuse the markdown link parser with a '!' prefix.
        if !self.seq_present(*b"](") {
            return false;
        }
        if let Some(link) = self.parse_markdown_link(self.i + 1, true) {
            self.push(link.node);
            self.i = link.end;
            return true;
        }
        false
    }

    fn parse_markdown_link(&self, at: usize, image: bool) -> Option<MdLink> {
        parse_md_link(self.s, at, image)
    }

    // ---- angle: autolink / timestamp / inline html / email ----------------

    fn try_angle(&mut self) -> bool {
        // `<scheme:...>` autolink (must have a ':')
        if self.ctx.autolinks {
            if let Some((end, node)) = parse_autolink(self.s, self.i) {
                self.push(node);
                self.i = end;
                return true;
            }
        }
        if self.ctx.timestamps {
            if let Some((end, node)) = parse_angle_timestamp(self.s, self.i) {
                self.push(node);
                self.i = end;
                return true;
            }
        }
        if self.ctx.autolinks {
            if let Some((end, node)) = parse_email_autolink(self.s, self.i) {
                self.push(node);
                self.i = end;
                return true;
            }
        }
        if self.ctx.html {
            if let Some((end, text)) = parse_inline_html(self.s, self.i) {
                self.push(Inline::InlineHtml { text });
                self.i = end;
                return true;
            }
        }
        false
    }

    // ---- bare urls --------------------------------------------------------

    fn try_bare_url(&mut self) -> bool {
        if let Some((end, node)) = parse_bare_url(self.s, self.i) {
            self.push(node);
            self.i = end;
            return true;
        }
        false
    }

    // ---- timestamps keywords ----------------------------------------------

    fn try_timestamp_keyword(&mut self) -> bool {
        if let Some((end, node)) = parse_keyword_timestamp(self.s, self.i) {
            self.push(node);
            self.i = end;
            return true;
        }
        false
    }
}

fn parse_inline_ctx(text: &str, ctx: Ctx) -> Vec<Inline> {
    let mut sc = Scanner::new(text, ctx);
    sc.run();
    sc.finish()
}

// ---- shared helpers -------------------------------------------------------

#[inline]
pub(crate) fn char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        1 // continuation/invalid byte: advance one to stay safe
    }
}

/// First index of `needle` in `b[from..]`, or None. (No newline restriction.)
pub(crate) fn find_sub(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > b.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Like `find_sub` but stops at a newline (returns None if a `\n` precedes needle).
pub(crate) fn find_sub_line(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= b.len() {
        if b[i] == b'\n' {
            return None;
        }
        if &b[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---- page ref / nested link -----------------------------------------------

/// `[[ name ]]` where name is non-empty, contains no newline, and ends at the first
/// `]]` (single `]` allowed inside). Returns (end_index, name, full_text).
pub(crate) fn parse_page_ref(s: &str, at: usize) -> Option<(usize, String, String)> {
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("[[") {
        return None;
    }
    let name_start = at + 2;
    let mut j = name_start;
    while j < n {
        let c = b[j];
        if c == b'\n' || c == b'\r' {
            return None;
        }
        if c == b']' {
            if j + 1 < n && b[j + 1] == b']' {
                break; // closing "]]"
            }
            // single ']' allowed in name
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < n {
            j += 2; // backslash escapes next char inside page name
            continue;
        }
        j += char_len(c);
    }
    if j + 1 >= n || b[j] != b']' || b[j + 1] != b']' {
        return None;
    }
    if j == name_start {
        return None; // empty name
    }
    let name = unescape(&s[name_start..j]); // value is unescaped; full stays raw
    let full = s[at..j + 2].to_string();
    Some((j + 2, name, full))
}

/// nested link `[[ ... ]]` whose inner text parses into >1 (label | nested) child.
pub(crate) fn parse_nested_link(s: &str, at: usize) -> Option<(usize, String)> {
    let (end, content) = match_brackets(s, at)?;
    let inner = &content[2..content.len() - 2];
    if nested_children_count(inner) > 1 {
        Some((end, content))
    } else {
        None
    }
}

/// Bracket matcher: from `[[`, count levels using `]]` chunks (mldoc match_brackets).
/// Returns (end_index, matched_string). Stops at a newline (returns None).
fn match_brackets(s: &str, at: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("[[") {
        return None;
    }
    let mut level: i32 = 1;
    let mut pos = at + 2;
    loop {
        // find next "]]" before a newline
        let mut k = pos;
        let mut found = None;
        while k + 1 < n {
            if b[k] == b'\n' {
                return None;
            }
            if b[k] == b']' && b[k + 1] == b']' {
                found = Some(k);
                break;
            }
            k += 1;
        }
        let idx = found?;
        let chunk = &s[pos..idx];
        level += count_occurrences(chunk, "[[") as i32 - 1;
        pos = idx + 2;
        if level <= 0 {
            return Some((pos, s[at..pos].to_string()));
        }
    }
}

fn count_occurrences(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let hb = hay.as_bytes();
    let nb = needle.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + nb.len() <= hb.len() {
        if &hb[i..i + nb.len()] == nb {
            count += 1;
            i += nb.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Count the (label | inner-nested-link) children of a nested-link's inner text.
/// Returns 1 if the inner text doesn't fully decompose (mldoc: single Label fallback).
fn nested_children_count(inner: &str) -> usize {
    let b = inner.as_bytes();
    let n = b.len();
    let mut j = 0;
    let mut count = 0;
    while j < n {
        if b[j] != b'[' {
            // label run until next '['
            while j < n && b[j] != b'[' {
                j += char_len(b[j]);
            }
            count += 1;
        } else {
            match match_brackets(inner, j) {
                Some((end, _)) => {
                    count += 1;
                    j = end;
                }
                None => {
                    // '[' not starting a valid match -> label_parse fails here;
                    // mldoc's consume:All then fails -> whole inner is one Label.
                    return 1;
                }
            }
        }
    }
    count
}

// ---- tags -----------------------------------------------------------------

const TAG_DELIMS: &[u8] = &[b',', b';', b'.', b'!', b'?', b'\'', b'"', b':', b'#'];
const TAG_STOP: &[u8] = &[b'#', b',', b'!', b'?', b'\'', b'"', b':'];

/// Parse a tag name starting at `start` (just after '#'). Returns (end_index,
/// children) where children are Plain runs and page-ref Links (mldoc Hash_tag).
pub(crate) fn parse_tag_name(s: &str, start: usize) -> (usize, Vec<Inline>) {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = start;
    let mut children: Vec<Inline> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, children: &mut Vec<Inline>| {
        if !plain.is_empty() {
            children.push(Inline::Plain {
                text: unescape(&std::mem::take(plain)),
            });
        }
    };
    loop {
        // (a) main run: non-space/eol, not a tag delim, not '['
        let run_start = i;
        while i < n {
            let c = b[i];
            if is_ws_or_nl(c) || TAG_DELIMS.contains(&c) || c == b'[' {
                break;
            }
            i += char_len(c);
        }
        if i > run_start {
            plain.push_str(&s[run_start..i]);
            continue;
        }
        if i >= n {
            break;
        }
        let c = b[i];
        if is_ws_or_nl(c) {
            break;
        }
        if c == b'[' {
            // (b) nested link `[[ …[[ ]]… ]]` (tried before page-ref, mirroring the
            // top-level bracket dispatch) then page ref.
            if let Some((end, content)) = parse_nested_link(s, i) {
                flush(&mut plain, &mut children);
                children.push(Inline::NestedLink { content });
                i = end;
                continue;
            }
            if let Some((end, name, full)) = parse_page_ref(s, i) {
                flush(&mut plain, &mut children);
                children.push(Inline::Link {
                    url: Url::PageRef { v: name },
                    label: vec![],
                    full,
                });
                i = end;
                continue;
            }
            // else '[' is an ordinary tag char (c2)
            plain.push('[');
            i += 1;
            continue;
        }
        // (c1) lookahead: a run of tag delims followed by space/eol/EOF -> stop.
        let mut k = i;
        while k < n && TAG_DELIMS.contains(&b[k]) {
            k += 1;
        }
        if k > i && (k >= n || is_ws_or_nl(b[k])) {
            break;
        }
        // (c2) consume one char if it isn't a hard tag-stop char.
        if TAG_STOP.contains(&c) {
            break;
        }
        plain.push_str(&s[i..i + char_len(c)]);
        i += char_len(c);
    }
    flush(&mut plain, &mut children);
    (i, children)
}

// ---- macros ---------------------------------------------------------------

/// Parse a macro inner string into (name, args). Returns None if arg splitting
/// doesn't consume the whole arg string with valid macro_args (mldoc consume:All).
pub(crate) fn parse_macro(inner: &str) -> Option<(String, Vec<String>)> {
    let b = inner.as_bytes();
    let n = b.len();
    // name = chars until '}' / '(' / ' '
    let mut j = 0;
    while j < n && b[j] != b'}' && b[j] != b'(' && b[j] != b' ' {
        j += char_len(b[j]);
    }
    if j == 0 {
        return None;
    }
    let name = inner[..j].to_string();
    let args_str = &inner[j..];
    if args_str.is_empty() {
        return Some((name, vec![]));
    }
    let args = parse_macro_args(args_str)?;
    Some((name, args))
}

/// mldoc macro_args: `optional spaces *> sep_by ',' (spaces *> macro_arg <* spaces)`
/// with consume:All. Returns None if any arg can't be cleanly consumed.
fn parse_macro_args(s: &str) -> Option<Vec<String>> {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    let skip_sp = |b: &[u8], mut i: usize| {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        i
    };
    i = skip_sp(b, i);
    let mut args = Vec::new();
    // empty after spaces -> a single empty arg? mldoc sep_by on "" yields []. But
    // args_str begins with the separator content after the name; treat all-space as [].
    if i >= n {
        return Some(vec![]);
    }
    loop {
        i = skip_sp(b, i);
        let (arg, ni) = parse_macro_arg(s, i)?;
        i = skip_sp(b, ni);
        args.push(arg);
        if i >= n {
            break;
        }
        if b[i] == b',' {
            i += 1;
            continue;
        }
        // leftover that isn't a separator -> consume:All fails.
        return None;
    }
    Some(args)
}

/// One macro arg: nested-link content | page-ref | `(( .. ))` | `"..."` | until ','.
fn parse_macro_arg(s: &str, at: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if at >= n {
        return Some((String::new(), at));
    }
    // nested link content
    if s[at..].starts_with("[[") {
        if let Some((end, content)) = parse_nested_link(s, at) {
            return Some((content, end));
        }
        if let Some((end, _name, full)) = parse_page_ref(s, at) {
            return Some((full, end));
        }
    }
    // (( ... ))
    if s[at..].starts_with("((") {
        let inner_start = at + 2;
        let mut j = inner_start;
        while j < n && b[j] != b')' {
            j += 1;
        }
        if j > inner_start && j + 1 < n && b[j] == b')' && b[j + 1] == b')' {
            return Some((s[at..j + 2].to_string(), j + 2));
        }
    }
    // quoted "..."
    if b[at] == b'"' {
        let mut j = at + 1;
        while j < n && b[j] != b'"' {
            if b[j] == b'\\' && j + 1 < n {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j < n && b[j] == b'"' {
            return Some((s[at..j + 1].to_string(), j + 1));
        }
    }
    // until ','
    let mut j = at;
    while j < n && b[j] != b',' {
        j += char_len(b[j]);
    }
    if j == at {
        return None;
    }
    Some((s[at..j].trim_end().to_string(), j))
}

// ---- footnote ref ---------------------------------------------------------

fn parse_footnote_ref(s: &str, at: usize) -> Option<(usize, String)> {
    // `[^ id ]` : id non-empty, no ']' / whitespace.
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("[^") {
        return None;
    }
    let id_start = at + 2;
    let mut j = id_start;
    while j < n && b[j] != b']' && !is_ws_or_nl(b[j]) {
        j += char_len(b[j]);
    }
    if j == id_start || j >= n || b[j] != b']' {
        return None;
    }
    Some((j + 1, s[id_start..j].to_string()))
}

// ---- markdown link / image ------------------------------------------------

struct MdLink {
    node: Inline,
    end: usize,
}

/// `[label](url)` markdown link/image starting at `at` (the '['). `image` controls
/// the `!`-prefixed full_text. Returns None if it isn't a well-formed link.
fn parse_md_link(s: &str, at: usize, image: bool) -> Option<MdLink> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'[') {
        return None;
    }
    // label between '[' and '](' (bracket-balanced; '[[..]]'/refs kept as text).
    let (label_text, after_label) = parse_label(s, at)?; // after_label points at '('
    if after_label >= n || b[after_label] != b'(' {
        return None;
    }
    let url_start = after_label + 1;
    let (url_text, after_url) = read_link_url(s, url_start)?; // after_url past ')'
    // optional metadata `{...}`
    let mut end = after_url;
    let mut metadata = String::new();
    if end < n && b[end] == b'{' {
        if let Some(close) = find_sub_line(b, end + 1, b"}") {
            metadata = s[end..close + 1].to_string();
            end = close + 1;
        }
    }
    // mldoc re-parses the raw between-parens text into a destination + optional
    // `"title"` (link_url_part_inner); the title is dropped, the destination value is
    // unescaped (full_text keeps the raw url_text). See DECISIONS.md.
    let dest = link_destination(&url_text);
    let url = classify_url(&dest);
    let label = parse_label_inline(&label_text);
    let prefix = if image { "!" } else { "" };
    let full = format!("{}[{}]({}){}", prefix, label_text, url_text, metadata);
    Some(MdLink {
        node: Inline::Link { url, label, full },
        end,
    })
}

/// Read the label text between `[` (at `at`) and the `](`, bracket-balanced.
/// Returns (label_raw, index_of_'(' ). Code spans inside are rendered into the raw
/// label text with surrounding backticks (mldoc label_part).
fn parse_label(s: &str, at: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    // special-case empty label "[]("
    if s[at..].starts_with("[](") {
        return Some((String::new(), at + 2));
    }
    let mut j = at + 1;
    let mut out = String::new();
    while j < n {
        let c = b[j];
        if c == b'\n' || c == b'\r' {
            return None;
        }
        if c == b']' {
            // must be followed by '(' to be a link
            if j + 1 < n && b[j + 1] == b'(' {
                return Some((out, j + 1));
            }
            return None;
        }
        if c == b'`' {
            // code span (kept as `...` text) or single backtick
            if let Some(end) = code_span_end_str(s, j) {
                out.push_str(&s[j..end]);
                j = end;
                continue;
            }
            out.push('`');
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < n {
            out.push('\\');
            out.push_str(&s[j + 1..j + 1 + char_len(b[j + 1])]);
            j += 1 + char_len(b[j + 1]);
            continue;
        }
        if c == b'[' {
            // page-ref, then single-bracket-balanced `[…]` (mldoc label_part_choices:
            // page_ref <|> string_contains_balanced_brackets [('[',']')]), kept raw.
            if let Some((end, _name, full)) = parse_page_ref(s, j) {
                out.push_str(&full);
                j = end;
                continue;
            }
            if let Some((end, content)) = match_single_brackets(s, j) {
                out.push_str(&content);
                j = end;
                continue;
            }
            out.push('[');
            j += 1;
            continue;
        }
        out.push_str(&s[j..j + char_len(c)]);
        j += char_len(c);
    }
    None
}

fn code_span_end_str(s: &str, pos: usize) -> Option<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let second = b.get(pos + 1).copied();
    if second != Some(b'`') {
        let start = pos + 1;
        let mut j = start;
        while j < n && b[j] != b'`' && b[j] != b'\n' && b[j] != b'\r' {
            j += 1;
        }
        if j > start && j < n && b[j] == b'`' {
            return Some(j + 1);
        }
        return None;
    }
    let start = pos + 2;
    if let Some(end) = find_sub(b, start, b"``") {
        return Some(end + 2); // double-backtick code may be empty
    }
    None
}

/// Read the URL inside a markdown link's parens, paren/bracket-balanced, stopping
/// at the unmatched ')' (the link closer). Returns (url_text, index_past_')').
fn read_link_url(s: &str, at: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut pd = 0i32;
    let mut bd = 0i32;
    while j < n {
        let c = b[j];
        if c == b'\n' || c == b'\r' {
            return None;
        }
        match c {
            b'(' => {
                pd += 1;
                j += 1;
            }
            b'[' => {
                bd += 1;
                j += 1;
            }
            b')' => {
                if pd == 0 {
                    // link closer
                    let url = s[at..j].to_string();
                    return Some((url, j + 1));
                }
                pd -= 1;
                j += 1;
            }
            b']' => {
                if bd > 0 {
                    bd -= 1;
                }
                j += 1;
            }
            b'\\' if j + 1 < n => {
                j += 2;
            }
            _ => j += char_len(c),
        }
    }
    None
}

/// Returns (label_text, index_past_')') with the URL string between (label_text, ...).
/// Wrapper name kept for clarity in callers.
fn parse_label_inline(label_text: &str) -> Vec<Inline> {
    // mldoc re-parses each Plain label segment with {emphasis,latex,entity,code,
    // sub/sup}, consume:All-or-keep-original. For our corpus, labels are plain text
    // (or contain code spans). We reproduce: try the restricted parse; if it fully
    // decomposes into non-plain-only nodes, use it, else keep the plain text.
    if label_text.is_empty() {
        return vec![];
    }
    // First split off code spans (label_part already turned them into `...`): we
    // re-segment on backticks so code is preserved, and re-parse the rest for
    // emphasis only.
    let segs = split_label_segments(label_text);
    let mut out = Vec::new();
    for seg in segs {
        match seg {
            LabelSeg::Code(t) => out.push(Inline::Code { text: t }),
            LabelSeg::Text(t) => {
                if let Some(nodes) = reparse_label_text(&t) {
                    out.extend(nodes);
                } else {
                    // label value is unescaped (`\]`→`]`, `\*`→`*`, …) while full_text
                    // keeps the raw backslash (mldoc). Mirrors page-ref value unescape.
                    out.push(Inline::Plain { text: unescape(&t) });
                }
            }
        }
    }
    out
}

enum LabelSeg {
    Text(String),
    Code(String),
}

fn split_label_segments(s: &str) -> Vec<LabelSeg> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < n {
        if b[i] == b'`' {
            if let Some(end) = code_span_end_str(s, i) {
                if !buf.is_empty() {
                    out.push(LabelSeg::Text(std::mem::take(&mut buf)));
                }
                // strip surrounding backticks for Code content
                let inner = code_inner(&s[i..end]);
                out.push(LabelSeg::Code(inner));
                i = end;
                continue;
            }
        }
        buf.push_str(&s[i..i + char_len(b[i])]);
        i += char_len(b[i]);
    }
    if !buf.is_empty() {
        out.push(LabelSeg::Text(buf));
    }
    out
}

fn code_inner(span: &str) -> String {
    // span is `x` or ``x``
    if span.starts_with("``") {
        span[2..span.len() - 2].to_string()
    } else {
        span[1..span.len() - 1].to_string()
    }
}

/// Re-parse a label text segment with emphasis-only (consume:All). Returns Some if
/// the whole segment decomposes into emphasis nodes; None to keep it as plain.
fn reparse_label_text(t: &str) -> Option<Vec<Inline>> {
    // Only emphasis (and the chars it consumes) are honored in labels; a label that
    // isn't pure-emphasis is kept verbatim (matches mldoc keeping Plain on failure).
    if !t.contains(['*', '_', '~', '^', '=']) {
        return None;
    }
    let nodes = parse_inline_ctx(t, Ctx::emph());
    // accept only if it produced at least one emphasis and no bare plain leftovers
    let has_emph = nodes
        .iter()
        .any(|x| matches!(x, Inline::Emphasis { .. }));
    let only_one_plain = nodes.len() == 1 && matches!(nodes[0], Inline::Plain { .. });
    if has_emph && !only_one_plain {
        // ensure no stray Plain that would indicate the consume:All failed
        // (mldoc keeps original if any non-emphasis text remains).
        let all_ok = nodes.iter().all(|x| {
            matches!(
                x,
                Inline::Emphasis { .. } | Inline::Code { .. }
            )
        });
        if all_ok {
            return Some(nodes);
        }
    }
    None
}

/// From a `[` at `at`, match the balancing `]` counting nested single brackets
/// (mldoc `string_contains_balanced_brackets [('[',']')]`). `\[`/`\]` are escaped.
/// Returns (end_index, matched_string incl. the outer brackets). Stops at a newline.
fn match_single_brackets(s: &str, at: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'[') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut j = at;
    while j < n {
        let c = b[j];
        if c == b'\n' || c == b'\r' {
            return None;
        }
        if c == b'\\' && j + 1 < n {
            j += 1 + char_len(b[j + 1]);
            continue;
        }
        if c == b'[' {
            depth += 1;
            j += 1;
        } else if c == b']' {
            depth -= 1;
            j += 1;
            if depth == 0 {
                return Some((j, s[at..j].to_string()));
            }
        } else {
            j += char_len(c);
        }
    }
    None
}

/// Extract a markdown link's *destination* from the raw between-parens text,
/// dropping an optional trailing ` "title"` and unescaping the value — a port of
/// mldoc `link_url_part_inner`. The url-parts are: `((block-ref))`, `<…>` (angles
/// stripped, inner spaces kept), `[[page-ref]]` (inner spaces kept), runs of
/// non-space/non-`[` chars, or a lone `[`; they stop at the first space outside
/// those. After the parts, optional spaces then a `"…"` title-to-end is allowed;
/// anything else fails and the *whole* raw text becomes the destination.
fn link_destination(url_text: &str) -> String {
    let b = url_text.as_bytes();
    let n = b.len();
    let mut j = 0usize;
    let mut dest = String::new();
    let mut part_count = 0usize;
    let mut had_angle = false;
    while j < n {
        let c = b[j];
        if c == b' ' {
            break;
        }
        // block ref ((...))
        if url_text[j..].starts_with("((") {
            if let Some(end) = find_sub(b, j + 2, b"))") {
                dest.push_str(&url_text[j..end + 2]);
                j = end + 2;
                part_count += 1;
                continue;
            }
        }
        // <...> (angles stripped from the value; inner spaces allowed)
        if c == b'<' {
            let mut k = j + 1;
            while k < n && b[k] != b'<' && b[k] != b'>' {
                k += 1;
            }
            if k < n && b[k] == b'>' && k > j + 1 {
                dest.push_str(&url_text[j + 1..k]);
                j = k + 1;
                part_count += 1;
                had_angle = true;
                continue;
            }
        }
        // [[page ref]] (inner spaces allowed)
        if url_text[j..].starts_with("[[") {
            if let Some((end, _name, full)) = parse_page_ref(url_text, j) {
                dest.push_str(&full);
                j = end;
                part_count += 1;
                continue;
            }
        }
        // run of non-space, non-'[' chars
        if c != b'[' {
            let run_start = j;
            while j < n {
                let cc = b[j];
                if cc == b' ' || cc == b'\n' || cc == b'\r' || cc == b'[' {
                    break;
                }
                j += char_len(cc);
            }
            if j > run_start {
                dest.push_str(&url_text[run_start..j]);
                part_count += 1;
                continue;
            }
        }
        // lone '[' (did not start '[[')
        dest.push_str(&url_text[j..j + char_len(c)]);
        j += char_len(c);
        part_count += 1;
    }
    // after the url-parts: optional spaces, then end-of-string or a `"…"` title.
    let rest = url_text[j..].trim_start();
    let title_ok = rest.is_empty() || is_quoted_title(rest);
    if (had_angle && part_count > 1) || !title_ok {
        // consume:All failed → the whole raw text is the destination.
        unescape(url_text.trim())
    } else {
        unescape(dest.trim())
    }
}

/// Is `s` exactly a `"…"` title (non-empty content, no unescaped `"` before the end)?
fn is_quoted_title(s: &str) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    if n < 3 || b[0] != b'"' || b[n - 1] != b'"' {
        return false;
    }
    let mut i = 1;
    while i < n - 1 {
        if b[i] == b'\\' && i + 1 < n - 1 {
            i += 2;
            continue;
        }
        if b[i] == b'"' {
            return false; // a bare `"` before the closing quote
        }
        i += 1;
    }
    true
}

fn classify_url(url_text: &str) -> Url {
    let t = url_text.trim();
    // block ref `(( x ))` exactly
    if t.starts_with("((") && t.ends_with("))") && t.len() >= 4 {
        let inner = &t[2..t.len() - 2];
        if !inner.contains(')') && !inner.is_empty() {
            return Url::BlockRef {
                v: inner.to_string(),
            };
        }
    }
    // page ref `[[ x ]]`
    if t.starts_with("[[") && t.ends_with("]]") && t.len() >= 4 {
        let inner = &t[2..t.len() - 2];
        if !inner.contains("]]") {
            return Url::PageRef {
                v: inner.to_string(),
            };
        }
    }
    // `<...>` strip
    let t2 = if t.starts_with('<') && t.ends_with('>') && t.len() >= 2 {
        &t[1..t.len() - 1]
    } else {
        t
    };
    // protocol://rest -> Complex
    if let Some(idx) = t2.find("://") {
        let protocol = &t2[..idx];
        if !protocol.is_empty() && protocol.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Url::Complex {
                protocol: Some(protocol.to_string()),
                link: Some(t2[idx + 3..].to_string()),
            };
        }
    }
    // .md / .markdown -> File
    let lower = t2.to_ascii_lowercase();
    if t2.len() > 3 && (lower.ends_with(".md") || lower.ends_with(".markdown")) {
        return Url::File { v: t2.to_string() };
    }
    Url::Search { v: t2.to_string() }
}

// ---- autolink / email / inline html ---------------------------------------

/// `<scheme:rest>` autolink (rest has no whitespace / '>'). Returns (end, node).
pub(crate) fn parse_autolink(s: &str, at: usize) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    // protocol = letters/digits, then ':'
    let mut j = at + 1;
    let p0 = j;
    while j < n && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == p0 || j >= n || b[j] != b':' {
        return None;
    }
    let protocol = s[p0..j].to_string();
    j += 1; // past ':'
    let mut slashes = String::new();
    if s[j..].starts_with("//") {
        slashes = "//".to_string();
        j += 2;
    }
    let link_start = j;
    while j < n && !is_ws_or_nl(b[j]) && b[j] != b'>' {
        j += char_len(b[j]);
    }
    if j >= n || b[j] != b'>' || j == link_start {
        return None;
    }
    let link = s[link_start..j].to_string();
    let full = format!("{}:{}{}", protocol, slashes, link);
    let node = Inline::Link {
        url: Url::Complex {
            protocol: Some(protocol),
            link: Some(link),
        },
        label: vec![Inline::Plain { text: full.clone() }],
        full,
    };
    Some((j + 1, node))
}

/// `<a@b.com>` email autolink. Returns (end, node) with the address object.
pub(crate) fn parse_email_autolink(s: &str, at: usize) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    let mut j = at + 1;
    let local_start = j;
    while j < n && b[j] != b'@' && b[j] != b'>' && !is_ws_or_nl(b[j]) {
        j += 1;
    }
    if j >= n || b[j] != b'@' || j == local_start {
        return None;
    }
    let local = s[local_start..j].to_string();
    j += 1;
    let dom_start = j;
    while j < n && b[j] != b'>' && !is_ws_or_nl(b[j]) {
        j += 1;
    }
    if j >= n || b[j] != b'>' || j == dom_start {
        return None;
    }
    let domain = s[dom_start..j].to_string();
    let val = serde_json::json!({ "local_part": local, "domain": domain });
    Some((j + 1, Inline::Email { text: val }))
}

/// Inline raw HTML `<tag ...> ... </tag>` (or self-contained). We capture the same
/// extent mldoc's Raw_html does for inline: a single tag region. For paired tags we
/// take up to the matching close; otherwise a single `<...>`.
pub(crate) fn parse_inline_html(s: &str, at: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    // tag name
    let mut j = at + 1;
    if j < n && b[j] == b'/' {
        j += 1;
    }
    let name_start = j;
    while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
        j += 1;
    }
    if j == name_start || !b[name_start].is_ascii_alphabetic() {
        return None;
    }
    let name = s[name_start..j].to_ascii_lowercase();
    // find end of the opening tag '>'
    let open_end = find_sub_line(b, j, b">")?;
    let self_closing = open_end > 0 && b[open_end - 1] == b'/';
    if self_closing {
        return Some((open_end + 1, s[at..open_end + 1].to_string()));
    }
    // look for matching </name>
    let close_tag = format!("</{}>", name);
    if let Some(cidx) = find_ci(s, open_end + 1, &close_tag) {
        let end = cidx + close_tag.len();
        return Some((end, s[at..end].to_string()));
    }
    // no closer: just the opening tag
    Some((open_end + 1, s[at..open_end + 1].to_string()))
}

/// Block-level LaTeX environment `\begin{NAME} … \end{NAME}` (mldoc `latex_env.ml`,
/// shared by the Markdown and Org block segmenters). The opener must be at the start
/// of the line at `line_start` after optional leading spaces/tabs (`spaces *>`); text
/// before `\begin` disqualifies it. mldoc grammar:
///   `spaces *> "\begin{" *> take_while1(≠'}') <* '}' <* spaces_or_eols`,
///   content = all chars until a case-insensitive `\end{NAME}` (or EOF); the node
///   name is lowercased. `line_end` is the byte offset of the opener line's content
///   end (the `\n` or EOF) — the name is taken within that line (realistic envs name
///   the environment on the begin line). Returns `(name, content, consumed_end)`
///   where `consumed_end` is the byte offset just past `\end{NAME}` (or EOF).
pub(crate) fn parse_latex_env(
    input: &str,
    line_start: usize,
    line_end: usize,
) -> Option<(String, String, usize)> {
    let b = input.as_bytes();
    let mut p = line_start;
    while p < line_end && (b[p] == b' ' || b[p] == b'\t') {
        p += 1;
    }
    if !input[p..].starts_with("\\begin{") {
        return None;
    }
    let name_start = p + 7; // past "\begin{"
    let mut j = name_start;
    while j < line_end && b[j] != b'}' {
        j += 1;
    }
    if j >= line_end || b[j] != b'}' || j == name_start {
        return None;
    }
    let name = &input[name_start..j];
    // spaces_or_eols after `\begin{NAME}` (spaces, tabs, newlines, CR).
    let mut cs = j + 1;
    while cs < input.len() && matches!(b[cs], b' ' | b'\t' | b'\n' | b'\r') {
        cs += 1;
    }
    let ending = format!("\\end{{{}}}", name);
    match find_ci(input, cs, &ending) {
        Some(e) => Some((name.to_ascii_lowercase(), input[cs..e].to_string(), e + ending.len())),
        None => Some((name.to_ascii_lowercase(), input[cs..].to_string(), input.len())),
    }
}

pub(crate) fn find_ci(s: &str, from: usize, needle: &str) -> Option<usize> {
    let hay = s.as_bytes();
    let nb = needle.as_bytes();
    let n = hay.len();
    if nb.is_empty() || from > n {
        return None;
    }
    let mut i = from;
    while i + nb.len() <= n {
        if hay[i..i + nb.len()]
            .iter()
            .zip(nb.iter())
            .all(|(a, c)| a.eq_ignore_ascii_case(c))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---- bare urls ------------------------------------------------------------

/// Bare URL `proto://...` (mldoc link_inline). proto = letters/digits. The path is
/// read until whitespace / `< > { } ( ) [ ]`-imbalance, with balanced parens/brackets
/// and `,;.!?` allowed only when not trailing before a delimiter.
pub(crate) fn parse_bare_url(s: &str, at: usize) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    // protocol
    let mut j = at;
    while j < n && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == at || !s[j..].starts_with("://") {
        return None;
    }
    let protocol = s[at..j].to_string();
    j += 3; // past "://"
    let path_start = j;
    // before_path: until space / '/' / '?' / '#' / inline_link_delims ([]<>{}())
    while j < n {
        let c = b[j];
        if is_ws_or_nl(c)
            || c == b'/'
            || c == b'?'
            || c == b'#'
            || matches!(c, b'[' | b']' | b'<' | b'>' | b'{' | b'}' | b'(' | b')')
        {
            break;
        }
        j += char_len(c);
    }
    let before_path_end = j;
    if before_path_end == path_start {
        return None; // before_path is take_while1 in mldoc: must be non-empty
    }
    // remaining_part: optional ('/' | '?' | '#') then balanced-bracket run that stops
    // only at whitespace / unmatched ')'|']' (NOT at < > { }) with trailing ,;.!?
    // before whitespace/EOL excluded.
    let mut remain_end = before_path_end;
    if j < n && matches!(b[j], b'/' | b'?' | b'#') {
        remain_end = read_url_balanced(s, j);
    }
    let end = remain_end;
    let raw = &s[path_start..end];
    let link = unescape(raw);
    let full = format!("{}://{}", protocol, raw);
    let label_text = format!("{}://{}", protocol, link);
    let node = Inline::Link {
        url: Url::Complex {
            protocol: Some(protocol),
            link: Some(link),
        },
        label: vec![Inline::Plain { text: label_text }],
        full,
    };
    Some((end, node))
}

/// Read the remaining URL path (mldoc `string_contains_balanced_brackets` over the
/// `/`,`?`,`#`-prefixed tail): balances `()` and `[]`, stops at whitespace or an
/// unmatched `)`/`]`, and excludes a trailing `, ; . ! ?` that precedes whitespace
/// or end-of-input. Does NOT stop at `< > { }` (mldoc keeps those in the tail).
fn read_url_balanced(s: &str, at: usize) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut pd = 0i32;
    let mut bd = 0i32;
    while j < n {
        let c = b[j];
        if is_ws_or_nl(c) {
            break;
        }
        match c {
            b'(' => {
                pd += 1;
                j += 1;
            }
            b'[' => {
                bd += 1;
                j += 1;
            }
            b')' => {
                if pd == 0 {
                    break;
                }
                pd -= 1;
                j += 1;
            }
            b']' => {
                if bd == 0 {
                    break;
                }
                bd -= 1;
                j += 1;
            }
            b',' | b';' | b'.' | b'!' | b'?' => {
                // excluded ending char: drop it (and stop) if it's the last char or
                // is followed by whitespace/EOL; otherwise keep and continue.
                if j + 1 >= n || is_ws_or_nl(b[j + 1]) {
                    break;
                }
                j += 1;
            }
            _ => j += char_len(c),
        }
    }
    j
}

/// Remove a backslash that escapes an ASCII-punctuation char (mldoc unescapes such
/// sequences in extracted string *values* — ref names, tag text, url links — while
/// leaving `full_text` raw). `\<punct>` → `<punct>`, `\\` → `\`; other `\` kept.
pub(crate) fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if b[i] == b'\\' && i + 1 < n && b[i + 1].is_ascii_punctuation() {
            out.push(b[i + 1] as char);
            i += 2;
        } else {
            let w = char_len(b[i]);
            out.push_str(&s[i..i + w]);
            i += w;
        }
    }
    out
}

// ---- timestamps -----------------------------------------------------------

/// `<YYYY-MM-DD WDAY [HH:MM]>` and ranges `<..>--<..>`. active = true (angle).
pub(crate) fn parse_angle_timestamp(s: &str, at: usize) -> Option<(usize, Inline)> {
    let (end1, ts1) = parse_bracket_date(s, at, b'<', b'>')?;
    // range?
    if s[end1..].starts_with("--") {
        let r = at + 0;
        let _ = r;
        if let Some((end2, ts2)) = parse_bracket_date(s, end1 + 2, b'<', b'>') {
            let val = serde_json::json!({ "start": ts1, "stop": ts2 });
            return Some((end2, Inline::Timestamp {
                ts: "Range".to_string(),
                date: val,
            }));
        }
    }
    Some((end1, Inline::Timestamp {
        ts: "Date".to_string(),
        date: ts1,
    }))
}

/// `SCHEDULED:`/`DEADLINE:`/`CLOSED:` `<DATE>`.
pub(crate) fn parse_keyword_timestamp(s: &str, at: usize) -> Option<(usize, Inline)> {
    let keywords = [
        ("SCHEDULED:", "Scheduled"),
        ("DEADLINE:", "Deadline"),
        ("CLOSED:", "Closed"),
    ];
    for (kw, ty) in keywords {
        if s[at..].starts_with(kw) {
            let mut j = at + kw.len();
            let b = s.as_bytes();
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if let Some((end, ts)) = parse_bracket_date(s, j, b'<', b'>') {
                return Some((end, Inline::Timestamp {
                    ts: ty.to_string(),
                    date: ts,
                }));
            }
            return None;
        }
    }
    None
}

/// Parse `<YYYY-MM-DD WDAY [HH:MM]>` (or with `[`/`]`), returning (end, date_obj).
pub(crate) fn parse_bracket_date(
    s: &str,
    at: usize,
    open: u8,
    close: u8,
) -> Option<(usize, serde_json::Value)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&open) {
        return None;
    }
    let inner_start = at + 1;
    let mut j = inner_start;
    while j < n && b[j] != close && b[j] != b'\n' {
        j += 1;
    }
    if j >= n || b[j] != close {
        return None;
    }
    let inner = &s[inner_start..j];
    let obj = parse_date_inner(inner, open == b'<')?;
    Some((j + 1, obj))
}

/// Parse an org timestamp repeater token (`+1m`, `++2w`, `.+1d`) into mldoc's JSON
/// shape `[[kind],[duration],n]` (e.g. `[["Plus"],["Month"],1]`); None if not one.
fn parse_repetition(tok: &str) -> Option<serde_json::Value> {
    let (kind, rest) = if let Some(r) = tok.strip_prefix(".+") {
        ("Dotted", r)
    } else if let Some(r) = tok.strip_prefix("++") {
        ("DoublePlus", r)
    } else if let Some(r) = tok.strip_prefix('+') {
        ("Plus", r)
    } else {
        return None;
    };
    let rb = rest.as_bytes();
    if rb.is_empty() {
        return None;
    }
    let dur = match rb[rb.len() - 1] {
        b'h' => "Hour",
        b'd' => "Day",
        b'w' => "Week",
        b'm' => "Month",
        b'y' => "Year",
        _ => return None,
    };
    let n: i64 = rest[..rest.len() - 1].parse().ok()?; // unit is ASCII → boundary safe
    Some(serde_json::json!([[kind], [dur], n]))
}

pub(crate) fn parse_date_inner(inner: &str, active: bool) -> Option<serde_json::Value> {
    // "YYYY-MM-DD WDAY [HH:MM] [repeat...]"
    let mut parts = inner.split_whitespace();
    let date_str = parts.next()?;
    let date_b: Vec<&str> = date_str.split('-').collect();
    if date_b.len() != 3 {
        return None;
    }
    let year: i64 = date_b[0].parse().ok()?;
    let month: i64 = date_b[1].parse().ok()?;
    let day: i64 = date_b[2].parse().ok()?;
    let wday = parts.next();
    // require a weekday made of letters (mldoc day_name_parser)
    let wday = match wday {
        Some(w) if w.chars().all(|c| c.is_alphabetic()) => w,
        _ => return None,
    };
    let mut obj = serde_json::Map::new();
    obj.insert(
        "date".to_string(),
        serde_json::json!({ "year": year, "month": month, "day": day }),
    );
    obj.insert("wday".to_string(), serde_json::json!(wday));
    // optional time `HH:MM` and/or repeater `+1m`/`++2w`/`.+1d` (mldoc timestamp.ml).
    for tok in parts {
        if let Some((h, m)) = tok.split_once(':') {
            if let (Ok(hour), Ok(min)) = (h.parse::<i64>(), m.parse::<i64>()) {
                obj.insert(
                    "time".to_string(),
                    serde_json::json!({ "hour": hour, "min": min }),
                );
                continue;
            }
        }
        if let Some(rep) = parse_repetition(tok) {
            obj.insert("repetition".to_string(), rep);
        }
    }
    obj.insert("active".to_string(), serde_json::json!(active));
    Some(serde_json::Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(s: &str) -> Vec<Inline> {
        parse_inline(s)
    }

    fn kinds(s: &str) -> Vec<String> {
        pi(s).iter().map(kind_of).collect()
    }
    fn kind_of(i: &Inline) -> String {
        match i {
            Inline::Plain { text } => format!("plain({text})"),
            Inline::Code { text } => format!("code({text})"),
            Inline::Emphasis { emph, .. } => format!("em({emph})"),
            Inline::Link { url, .. } => format!("link({})", url_kind(url)),
            Inline::Tag { children } => format!("tag({})", tag_text_dbg(children)),
            Inline::Macro { name, args } => format!("macro({name};{})", args.join("|")),
            Inline::NestedLink { content } => format!("nested({content})"),
            Inline::Break => "break".into(),
            Inline::HardBreak => "hardbreak".into(),
            Inline::Latex { mode, body } => format!("latex({mode}:{body})"),
            Inline::Fnref { name } => format!("fn({name})"),
            Inline::Timestamp { ts, .. } => format!("ts({ts})"),
            Inline::InlineHtml { text } => format!("html({text})"),
            Inline::Email { .. } => "email".into(),
            Inline::Verbatim { text } => format!("verb({text})"),
            Inline::Subscript { .. } => "sub".into(),
            Inline::Superscript { .. } => "sup".into(),
            Inline::Entity { unicode, .. } => format!("entity({unicode})"),
        }
    }
    fn url_kind(u: &Url) -> String {
        match u {
            Url::PageRef { v } => format!("page:{v}"),
            Url::BlockRef { v } => format!("block:{v}"),
            Url::Search { v } => format!("search:{v}"),
            Url::File { v } => format!("file:{v}"),
            Url::Complex { protocol, link } => format!(
                "complex:{}:{}",
                protocol.clone().unwrap_or_default(),
                link.clone().unwrap_or_default()
            ),
        }
    }
    fn tag_text_dbg(c: &[Inline]) -> String {
        c.iter()
            .map(|x| match x {
                Inline::Plain { text } => text.clone(),
                Inline::Link { full, .. } => full.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn plain_and_breaks() {
        assert_eq!(kinds("hello world"), ["plain(hello world)"]);
        assert_eq!(kinds("a\nb"), ["plain(a)", "break", "plain(b)"]);
        assert_eq!(kinds("x  \ny"), ["plain(x)", "hardbreak", "plain(y)"]);
    }

    #[test]
    fn emphasis_basic() {
        assert_eq!(kinds("*a*"), ["em(Italic)"]);
        assert_eq!(kinds("**a**"), ["em(Bold)"]);
        assert_eq!(kinds("~~a~~"), ["em(Strike_through)"]);
        assert_eq!(kinds("==a=="), ["em(Highlight)"]);
        assert_eq!(kinds("^^a^^"), ["em(Highlight)"]);
        assert_eq!(kinds("__a__"), ["em(Bold)"]);
        assert_eq!(kinds("_a_"), ["em(Italic)"]);
    }

    #[test]
    fn emphasis_nesting_and_flanking() {
        // *** -> Italic[Bold]
        match &pi("***a***")[0] {
            Inline::Emphasis { emph, children } => {
                assert_eq!(emph, "Italic");
                assert!(matches!(&children[0], Inline::Emphasis { emph, .. } if emph == "Bold"));
            }
            _ => panic!(),
        }
        // outer-first matching: *a *b* c* -> Italic["a *b"] + " c*"
        assert_eq!(kinds("*a *b* c*"), ["em(Italic)", "plain( c*)"]);
        // word-internal underscores do not emphasize
        assert_eq!(kinds("snake_case_word"), ["plain(snake_case_word)"]);
        assert_eq!(kinds("foo__bar__baz"), ["plain(foo_)", "em(Italic)", "plain(_baz)"]);
        // space-adjacent markers do not form emphasis
        assert_eq!(kinds("a* b *c"), ["plain(a* b *c)"]);
        assert_eq!(kinds("~~a~~ ~~ b ~~"), ["em(Strike_through)", "plain( ~~ b ~~)"]);
    }

    #[test]
    fn emphasis_nested_links() {
        assert_eq!(kinds("**[[Foo]]**"), ["em(Bold)"]);
        // tag inside emphasis stays plain
        assert_eq!(kinds("==#tag=="), ["em(Highlight)"]);
        match &pi("**x [[Y]] z**")[0] {
            Inline::Emphasis { children, .. } => {
                assert!(children.iter().any(|c| matches!(c, Inline::Link { .. })));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn code_spans() {
        assert_eq!(kinds("`x`"), ["code(x)"]);
        assert_eq!(kinds("``[[Foo]]``"), ["code([[Foo]])"]);
        assert_eq!(kinds("```[[Foo]]```"), ["code(`[[Foo]])", "plain(`)"]);
        assert_eq!(kinds("`unterminated [[Foo]]"), ["plain(`unterminated )", "link(page:Foo)"]);
        // refs never leak out of code
        assert_eq!(kinds("`[[Foo]]`"), ["code([[Foo]])"]);
    }

    #[test]
    fn escapes() {
        assert_eq!(kinds("\\[[a]]"), ["plain([[a]])"]);
        assert_eq!(kinds("\\#tag"), ["plain(#tag)"]);
        assert_eq!(kinds("a \\[[b]] c"), ["plain(a [[b]] c)"]);
        assert_eq!(kinds("\\\\[[a]]"), ["plain(\\)", "link(page:a)"]);
        assert_eq!(kinds("\\`[[a]]\\`"), ["plain(`)", "link(page:a)", "plain(`)"]);
    }

    #[test]
    fn page_and_block_refs() {
        assert_eq!(kinds("[[Foo]]"), ["link(page:Foo)"]);
        assert_eq!(kinds("[[Foo](bar)]]"), ["link(page:Foo](bar))"]);
        assert_eq!(kinds("[[]]"), ["plain([[]])"]);
        assert_eq!(
            kinds("((11111111-1111-1111-1111-111111111111))"),
            ["link(block:11111111-1111-1111-1111-111111111111)"]
        );
        // labeled block ref: triple paren
        assert_eq!(
            kinds("[L](((11111111-1111-1111-1111-111111111111)))"),
            ["link(block:11111111-1111-1111-1111-111111111111)"]
        );
        // nested link
        assert_eq!(kinds("[[a[[b]]c]]"), ["nested([[a[[b]]c]])"]);
    }

    #[test]
    fn tags_charset() {
        assert_eq!(kinds("#café"), ["tag(café)"]);
        assert_eq!(kinds("#中文"), ["tag(中文)"]);
        assert_eq!(kinds("#😀"), ["tag(😀)"]);
        assert_eq!(kinds("#a.b"), ["tag(a.b)"]);
        assert_eq!(kinds("c#sharp"), ["plain(c)", "tag(sharp)"]);
        assert_eq!(kinds("#t."), ["tag(t)", "plain(.)"]);
        assert_eq!(kinds("#[[a b]]"), ["tag([[a b]])"]);
        assert_eq!(kinds("(#tag)"), ["plain(()", "tag(tag))"]);
    }

    #[test]
    fn macros_and_links() {
        assert_eq!(kinds("{{embed [[Foo]]}}"), ["macro(embed;[[Foo]])"]);
        assert_eq!(kinds("{{query [[Foo]]}}"), ["macro(query;[[Foo]])"]);
        assert_eq!(kinds("{{renderer :x, [[Foo]]}}"), ["macro(renderer;:x|[[Foo]])"]);
        assert_eq!(kinds("[text](https://ex.com/a)"), ["link(complex:https:ex.com/a)"]);
        assert_eq!(kinds("see https://ex.com/p then x"),
            ["plain(see )", "link(complex:https:ex.com/p)", "plain( then x)"]);
    }

    #[test]
    fn latex_and_timestamp() {
        assert_eq!(kinds("$e=mc^2$"), ["latex(Inline:e=mc^2)"]);
        assert_eq!(kinds("text $$x$$ more"), ["plain(text )", "latex(Displayed:x)", "plain( more)"]);
        assert_eq!(kinds("<2026-06-20 Sat>"), ["ts(Date)"]);
    }

    #[test]
    fn unicode_no_panic() {
        for s in [
            "café 中文 😀",
            "[[café]] #naïve",
            "a\u{200b}b #tag\u{200b}suf",
            "***中文***",
            "`中`",
            "**café**",
            "😀#tag",
        ] {
            let _ = pi(s);
        }
    }

    #[test]
    fn escaped_marker_in_emphasis() {
        // a backslash-escaped marker inside emphasis is literal, not a closer (M5).
        match &pi("*a\\*b*")[0] {
            Inline::Emphasis { emph, children } => {
                assert_eq!(emph, "Italic");
                assert_eq!(children, &vec![Inline::Plain { text: "a*b".into() }]);
            }
            _ => panic!(),
        }
        // _a*b\*_  -> Italic["a*b*"] (inner * has no unescaped closer)
        match &pi("_a*b\\*_")[0] {
            Inline::Emphasis { children, .. } => {
                assert_eq!(children, &vec![Inline::Plain { text: "a*b*".into() }]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn link_title_and_url_unescape() {
        // trailing "title" dropped from the url value; full_text keeps it (M5).
        match &pi("[a](u \"t\")")[0] {
            Inline::Link { url, full, .. } => {
                assert!(matches!(url, Url::Search { v } if v == "u"));
                assert_eq!(full, "[a](u \"t\")");
            }
            _ => panic!(),
        }
        // <bbb> angle-stripped + title dropped.
        assert_eq!(kinds("[a](<bbb> \"cc\")"), ["link(search:bbb)"]);
        // page-ref kept inside a destination, with its inner space.
        assert_eq!(kinds("[a](bbb[[ccc \"dd\"]] \"e f\")"), ["link(search:bbb[[ccc \"dd\"]])"]);
        // url value is unescaped (\)→)), full keeps the backslash.
        match &pi("[x](a\\)b)")[0] {
            Inline::Link { url, full, .. } => {
                assert!(matches!(url, Url::Search { v } if v == "a)b"));
                assert_eq!(full, "[x](a\\)b)");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn link_label_brackets_and_unescape() {
        // single [..] balanced inside an image label (M5).
        match &pi("![lab[el]](u)")[0] {
            Inline::Link { label, .. } => {
                assert_eq!(label, &vec![Inline::Plain { text: "lab[el]".into() }]);
            }
            _ => panic!(),
        }
        // escaped ] in a label: value unescaped, full raw.
        match &pi("[label\\](x)](xxx)")[0] {
            Inline::Link { label, full, url } => {
                assert_eq!(label, &vec![Inline::Plain { text: "label](x)".into() }]);
                assert!(matches!(url, Url::Search { v } if v == "xxx"));
                assert_eq!(full, "[label\\](x)](xxx)");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn tag_with_nested_link() {
        // #[[nested [[tag]]]] -> Tag[Nested_link] (M5).
        match &pi("#[[nested [[tag]]]]")[0] {
            Inline::Tag { children } => {
                assert_eq!(children, &vec![Inline::NestedLink { content: "[[nested [[tag]]]]".into() }]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn timestamp_with_repeater() {
        match &pi("SCHEDULED: <2004-12-25 Sat +1m>")[0] {
            Inline::Timestamp { ts, date } => {
                assert_eq!(ts, "Scheduled");
                assert_eq!(date["repetition"], serde_json::json!([["Plus"], ["Month"], 1]));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn latex_entities() {
        // a known entity name → Entity (backslash + letters), with " G" plain after.
        match &pi("\\Delta G")[0] {
            Inline::Entity { name, latex, latex_mathp, html, ascii, unicode } => {
                assert_eq!(name, "Delta");
                assert_eq!(latex, "\\Delta");
                assert!(*latex_mathp);
                assert_eq!(html, "&Delta;");
                assert_eq!(ascii, "Delta");
                assert_eq!(unicode, "Δ");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(kinds("\\Delta G"), ["entity(Δ)", "plain( G)"]);
        // optional `{}` consumed after the name (entity or not).
        assert_eq!(kinds("\\Delta{}G"), ["entity(Δ)", "plain(G)"]);
        assert_eq!(kinds("\\foo{}G"), ["plain(fooG)"]); // unknown → bare letters
        assert_eq!(kinds("\\foo G"), ["plain(foo G)"]);
        // inside `$…$` the backslash stays a Latex_Fragment (entity path not taken).
        assert_eq!(kinds("$\\Delta$"), ["latex(Inline:\\Delta)"]);
        // case-sensitive table (AA vs aa are distinct entities).
        assert_eq!(kinds("\\AA"), ["entity(Å)"]);
    }

    #[test]
    fn adversarial_runs_terminate() {
        // long marker runs must not hang / overflow (linear no-closer cache).
        let stars = "*a ".repeat(20000);
        let _ = pi(&stars);
        let opens = "[[".repeat(20000);
        let _ = pi(&opens);
        let parens = "((".repeat(20000);
        let _ = pi(&parens);
        let deep = "*".repeat(50000);
        let _ = pi(&deep);
    }
}

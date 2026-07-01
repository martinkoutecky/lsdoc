//! Inline leaf-parser library — the shared, context-free building blocks of inline
//! parsing, behavior-equivalent to mldoc 1.5.7's inline grammar (`lib/syntax/inline.ml`,
//! verified against the live oracle).
//!
//! The top-level ctx-aware inline pass (lexer → one-pass resolve) lives in the two format
//! resolvers — `crate::resolver` (Markdown) and `crate::org_resolver` (Org). THIS module is
//! their shared leaf kit: byte-class predicates (`is_ws`, `is_underscore_delim`, …), the
//! bracket/close pre-pair builders (`build_hiccup_close`, `build_nested_close`, …), and the
//! per-construct leaf parsers both resolvers call — page refs (`parse_page_ref`), nested links
//! (`parse_nested_link`), md links/images (`md_link`), autolinks, bare URLs, timestamps, latex
//! spans, hiccup, entities, escapes. Each returns `(node, end)` (or `None`) and does no
//! delimiter pairing of its own; the resolver drives them.
//!
//! Markdown link/image LABELS are re-parsed with the restricted emphasis-content grammar
//! (mldoc `aux_nested_emphasis`) via `crate::resolver::parse_inline_ctx_emph`
//! (`reparse_label_text` below) — the same `Ctx::emph()` path the resolver uses for emphasis
//! content, so md labels go through the v0.2 resolver just as Org labels do. (The old standalone
//! v1 `Scanner` inline engine that used to live here was retired once both resolvers shipped.)
//!
//! BYTE-SAFETY: all `&str` slicing is at ASCII delimiter / run boundaries or via
//! `char_indices`, never mid-codepoint.

use crate::projection::{Inline, Span, Url};

/// Byte offset of `sub` within `parent` (both must share the same backing buffer — `sub`
/// is a sub-slice of `parent`). Used to recover the absolute base for a sub-slice inline
/// parse. Debug-asserts the sub-slice invariant.
#[inline]
pub(crate) fn ptr_base(sub: &str, parent: &str) -> usize {
    debug_assert!(
        sub.as_ptr() as usize >= parent.as_ptr() as usize
            && sub.as_ptr() as usize + sub.len() <= parent.as_ptr() as usize + parent.len(),
        "ptr_base: sub is not a sub-slice of parent"
    );
    sub.as_ptr() as usize - parent.as_ptr() as usize
}

// ---- byte classes ---------------------------------------------------------

#[inline]
pub(crate) fn is_ws(c: u8) -> bool {
    // `\r` is an EOL (handled as `Break`, like `\n`), NOT whitespace — so a whitespace
    // run stops at `\r` and lets the break dispatch fire (C5: CRLF / lone-CR endings).
    c == b' ' || c == b'\t'
}
#[inline]
pub(crate) fn is_ws_or_nl(c: u8) -> bool {
    is_ws(c) || c == b'\n' || c == b'\r'
}
/// mldoc `underline_emphasis_delims`: ASCII punctuation + whitespace (NOT letters/
/// digits, NOT non-ASCII). Used for `_`/`__` open-backward and close-forward gates.
#[inline]
pub(crate) fn is_underscore_delim(c: u8) -> bool {
    c.is_ascii_punctuation() || is_ws_or_nl(c)
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
/// `unescape_plain`: Markdown unescapes the plain runs (`#ab\|` → `ab|`); Org keeps
/// backslashes literal (`#ab\|` → `ab\|`), matching its no-unescape invariant (C4).
pub(crate) fn parse_tag_name(
    s: &str,
    start: usize,
    unescape_plain: bool,
    base: usize,
) -> (usize, Vec<Inline>) {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = start;
    let mut children: Vec<Inline> = Vec::new();
    let mut plain = String::new();
    // Byte offset (within `s`) where the current plain run began. The plain buffer only
    // accumulates a CONTIGUOUS source range (each push is at the current `i` and advances
    // it), so the run is `s[plain_buf_start..i]` — UNLESS `unescape` shortened it (md).
    let mut plain_buf_start = start;
    macro_rules! flush {
        () => {{
            if !plain.is_empty() {
                let raw = std::mem::take(&mut plain);
                // A `\`-unescape transform (md) can shorten the text vs. source, so S5
                // can't hold — drop the span. Org keeps `\` literal → always trackable.
                let span = if !unescape_plain || !raw.contains('\\') {
                    Some(Span(base + plain_buf_start, base + i))
                } else {
                    None
                };
                let text = if unescape_plain { unescape(&raw) } else { raw };
                children.push(Inline::Plain { text, span });
            }
        }};
    }
    macro_rules! push_plain {
        ($pos:expr, $seg:expr) => {{
            if plain.is_empty() {
                plain_buf_start = $pos;
            }
            plain.push_str($seg);
        }};
    }
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
            push_plain!(run_start, &s[run_start..i]);
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
                flush!();
                children.push(Inline::NestedLink { content, span: Some(Span(base + i, base + end)) });
                i = end;
                continue;
            }
            if let Some((end, name, full)) = parse_page_ref(s, i) {
                flush!();
                children.push(Inline::Link {
                    url: Url::PageRef { v: name },
                    label: vec![],
                    full,
                    image: false,
                    metadata: String::new(),
                    title: None,
                    span: Some(Span(base + i, base + end)),
                });
                i = end;
                continue;
            }
            // else '[' is an ordinary tag char (c2)
            push_plain!(i, "[");
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
        push_plain!(i, &s[i..i + char_len(c)]);
        i += char_len(c);
    }
    flush!();
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

pub(crate) fn parse_footnote_ref(s: &str, at: usize) -> Option<(usize, String)> {
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

/// Resolver (v0.2) entry: `[label](url)` / `![…](…)` → (node, end). Thin wrapper over the
/// v1 `parse_md_link` so the resolver reuses its exact label/url/title/metadata semantics.
/// `base` is the absolute byte offset of `s` in the block body (for label-child spans).
pub(crate) fn md_link(s: &str, at: usize, image: bool, base: usize) -> Option<(Inline, usize)> {
    parse_md_link(s, at, image, base).map(|l| (l.node, l.end))
}

/// `[label](url)` markdown link/image starting at `at` (the '['). `image` controls
/// the `!`-prefixed full_text. Returns None if it isn't a well-formed link.
fn parse_md_link(s: &str, at: usize, image: bool, base: usize) -> Option<MdLink> {
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
    let (dest, title) = link_destination(&url_text);
    let url = classify_url(&dest);
    // The label text is byte-identical to the source slice `s[at+1 .. close]` (parse_label
    // copies every byte verbatim), so its children index off `base + at + 1`. The Link's own
    // `span` is set by the resolver (set_inline_span) over the full `[label](url)` extent.
    let label = parse_label_inline(&label_text, base + at + 1);
    let prefix = if image { "!" } else { "" };
    let full = format!("{}[{}]({}){}", prefix, label_text, url_text, metadata);
    Some(MdLink {
        node: Inline::Link { url, label, full, image, metadata, title, span: None },
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

/// Re-parse the raw `label_text` (byte-identical to its source slice) into inline nodes.
/// `label_base` = the absolute byte offset of `label_text[0]` in the block body, so each
/// segment's children index correctly (S2). mldoc re-parses each Plain label segment with
/// {emphasis,latex,entity,code,sub/sup}, consume:All-or-keep-original. For our corpus,
/// labels are plain text (or contain code spans). We reproduce: try the restricted parse;
/// if it fully decomposes into non-plain-only nodes, use it, else keep the plain text.
fn parse_label_inline(label_text: &str, label_base: usize) -> Vec<Inline> {
    if label_text.is_empty() {
        return vec![];
    }
    // First split off code spans (label_part already turned them into `...`): we
    // re-segment on backticks so code is preserved, and re-parse the rest for
    // emphasis only. Each segment carries its byte offset within `label_text`.
    let segs = split_label_segments(label_text);
    let mut out = Vec::new();
    for (seg_start, seg) in segs {
        match seg {
            LabelSeg::Code { text, full_len } => out.push(Inline::Code {
                text,
                span: Some(Span(label_base + seg_start, label_base + seg_start + full_len)),
            }),
            LabelSeg::Text(t) => {
                if let Some(nodes) = reparse_label_text(&t, label_base + seg_start) {
                    out.extend(nodes);
                } else {
                    // label value is unescaped (`\]`→`]`, `\*`→`*`, …) while full_text keeps
                    // the raw backslash (mldoc). The unescape can shorten the text vs. source;
                    // keep a span only when it stayed 1:1 (length preserved ⟹ no `\` removed).
                    let text = unescape(&t);
                    let span = if text.len() == t.len() {
                        Some(Span(label_base + seg_start, label_base + seg_start + t.len()))
                    } else {
                        None
                    };
                    out.push(Inline::Plain { text, span });
                }
            }
        }
    }
    out
}

enum LabelSeg {
    Text(String),
    /// `text` = the code span's inner content (backticks stripped); `full_len` = the code
    /// span's FULL source byte length (backticks included), for the atom's span extent.
    Code { text: String, full_len: usize },
}

/// Split `s` into `(byte_offset_in_s, segment)` pairs: code spans (`` `…` ``) become
/// `Code`, the rest coalesces into `Text`. Each segment's source range is contiguous, so
/// `byte_offset_in_s` is the start of `Text` runs / the opening backtick of `Code`.
fn split_label_segments(s: &str) -> Vec<(usize, LabelSeg)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_start = 0usize;
    let mut i = 0;
    while i < n {
        if b[i] == b'`' {
            if let Some(end) = code_span_end_str(s, i) {
                if !buf.is_empty() {
                    out.push((buf_start, LabelSeg::Text(std::mem::take(&mut buf))));
                }
                // strip surrounding backticks for Code content; keep the full length.
                let inner = code_inner(&s[i..end]);
                out.push((i, LabelSeg::Code { text: inner, full_len: end - i }));
                i = end;
                continue;
            }
        }
        if buf.is_empty() {
            buf_start = i;
        }
        buf.push_str(&s[i..i + char_len(b[i])]);
        i += char_len(b[i]);
    }
    if !buf.is_empty() {
        out.push((buf_start, LabelSeg::Text(buf)));
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
/// the whole segment decomposes into emphasis nodes; None to keep it as plain. `base` is
/// the absolute byte offset of `t[0]` in the block body (so the reparsed nodes' spans are
/// absolute, S2).
fn reparse_label_text(t: &str, base: usize) -> Option<Vec<Inline>> {
    // Only emphasis (and the chars it consumes) are honored in labels; a label that
    // isn't pure-emphasis is kept verbatim (matches mldoc keeping Plain on failure).
    if !t.contains(['*', '_', '~', '^', '=']) {
        return None;
    }
    let nodes = crate::resolver::parse_inline_ctx_emph(t, base);
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
/// Returns `(destination, title)`: the title is the raw inner of a trailing `"…"`
/// (no quotes, NOT unescaped — matching mldoc's `Link.title`), or `None`.
fn link_destination(url_text: &str) -> (String, Option<String>) {
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
        // consume:All failed → the whole raw text is the destination, no title.
        (unescape(url_text.trim()), None)
    } else {
        // `rest` is either empty or exactly a `"…"` title (quotes ASCII → byte-safe).
        let title = is_quoted_title(rest).then(|| rest[1..rest.len() - 1].to_string());
        (unescape(dest.trim()), title)
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
        // synthetic label (== full, no `<>`): no clean source slice → no span.
        label: vec![Inline::Plain { text: full.clone(), span: None }],
        full,
        image: false,
        metadata: String::new(),
        title: None,
        span: None,
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
    Some((j + 1, Inline::Email { text: val, span: None }))
}

/// Inline raw HTML `<tag ...> ... </tag>` (or self-contained). We capture the same
/// extent mldoc's Raw_html does for inline: a single tag region. For paired tags we
/// take up to the matching close; otherwise a single `<...>`.
/// Parse inline raw HTML `<tag …>…</tag>`. `closer_possible == false` asserts (from a caller's
/// monotone absence cache) that no `</` exists at/after `at`, so the `</name>` search
/// is skipped. Every closing tag begins with the literal bytes `</`, so when those are
/// absent the closer scan can only fail — the result (the bare opening tag) is
/// byte-identical to the full scan, just O(1). This keeps a run of unclosed `<tag>`s
/// linear instead of O(n²) (each would otherwise re-scan to EOF for its closer).
pub(crate) fn parse_inline_html_cached(s: &str, at: usize, closer_possible: bool) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    // Raw_html.parse starts with `peek_string 10`; short remaining inputs fail before the
    // tag/special-form dispatch. This is why bare `<br />` is plain but longer self-closes parse.
    if n.saturating_sub(at) < 10 {
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
    let raw_name = &s[name_start..j];
    let name = raw_name.to_ascii_lowercase();
    // find end of the opening tag '>'
    let open_end = find_sub_line(b, j, b">")?;
    let self_closing = open_end > 0 && b[open_end - 1] == b'/';
    if self_closing {
        return Some((open_end + 1, s[at..open_end + 1].to_string()));
    }
    // look for matching </name> (skipped when no `</` exists ahead at all).
    if closer_possible {
        if is_known_html_tag(&name) {
            let (end, saw_close, self_close) = match_known_inline_html(s, j, raw_name);
            if let Some(end) = end.or(self_close) {
                return Some((end, s[at..end].to_string()));
            }
            if saw_close {
                return None;
            }
        } else {
            let close_tag = format!("</{}>", name);
            if let Some(cidx) = find_ci(s, open_end + 1, &close_tag) {
                let end = cidx + close_tag.len();
                return Some((end, s[at..end].to_string()));
            }
        }
    }
    // no closer: just the opening tag
    Some((open_end + 1, s[at..open_end + 1].to_string()))
}

fn starts_with_bytes_at(bytes: &[u8], at: usize, needle: &[u8], end: usize) -> bool {
    at + needle.len() <= end && &bytes[at..at + needle.len()] == needle
}

fn count_inline_tag_opens(bytes: &[u8], from: usize, end: usize, plain: &[u8], attrs: &[u8]) -> usize {
    let mut count = 0usize;
    let mut q = from;
    while q < end {
        if starts_with_bytes_at(bytes, q, plain, end) {
            count += 1;
            q += plain.len();
        } else if starts_with_bytes_at(bytes, q, attrs, end) {
            count += 1;
            q += attrs.len();
        } else {
            q += 1;
        }
    }
    count
}

fn match_known_inline_html(s: &str, scan_start: usize, raw_name: &str) -> (Option<usize>, bool, Option<usize>) {
    let bytes = s.as_bytes();
    let close_tag = format!("</{}>", raw_name);
    let open_plain = format!("<{}>", raw_name);
    let open_attrs = format!("<{} ", raw_name);
    let mut first_self_close = None;
    let mut p = scan_start;
    while first_self_close.is_none() {
        match find_sub(bytes, p, b"/>") {
            Some(end) => first_self_close = Some(end + 2),
            None => break,
        }
    }

    let mut level = 1isize;
    let mut chunk_start = scan_start;
    let mut saw_close = false;
    while let Some(close_at) = find_ci(s, p, &close_tag) {
        saw_close = true;
        level += count_inline_tag_opens(
            bytes,
            chunk_start,
            close_at,
            open_plain.as_bytes(),
            open_attrs.as_bytes(),
        ) as isize;
        level -= 1;
        let end = close_at + close_tag.len();
        if level <= 0 {
            return (Some(end), saw_close, first_self_close);
        }
        p = end;
        chunk_start = end;
    }
    (None, saw_close, first_self_close)
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
        // synthetic label (unescaped url): may differ from source → no span.
        label: vec![Inline::Plain { text: label_text, span: None }],
        full,
        image: false,
        metadata: String::new(),
        title: None,
        span: None,
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

// ---- hiccup ---------------------------------------------------------------

/// mldoc's Clojure-hiccup HTML-element allowlist (110 names, lowercase, **byte-sorted**
/// for binary search). A `[:name …]` vector is a `Hiccup` (block) / `Inline_Hiccup`
/// (inline) iff `name` (case-insensitively) is one of these. Derived from mldoc 1.5.7's
/// source tag set (`Qz`) and cross-checked against the live oracle (every name in / every
/// HTML5 element not listed out). Keep sorted — `is_hiccup_tag` binary-searches it.
pub(crate) static HICCUP_TAGS: &[&str] = &[
    "a", "abbr", "address", "area", "article", "aside", "audio", "b", "base", "bdi", "bdo",
    "blockquote", "body", "br", "button", "canvas", "caption", "cite", "code", "col", "colgroup", "data",
    "datalist", "dd", "del", "details", "dfn", "div", "dl", "dt", "em", "embed", "fieldset",
    "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "head",
    "header", "hr", "html", "i", "iframe", "img", "input", "ins", "kbd", "keygen", "label",
    "legend", "li", "link", "main", "map", "mark", "meta", "meter", "nav", "noscript", "object",
    "ol", "optgroup", "option", "output", "p", "param", "pre", "progress", "q", "rb", "rp",
    "rt", "rtc", "ruby", "s", "samp", "script", "section", "select", "small", "source", "span",
    "strong", "style", "sub", "summary", "sup", "table", "tbody", "td", "template", "textarea", "tfoot",
    "th", "thead", "time", "title", "tr", "track", "u", "ul", "var", "video", "wbr",
];

/// Case-insensitive membership test against `HICCUP_TAGS`. `name` is ASCII alphanumeric
/// (the only thing the caller passes), so a lowercased fixed buffer + binary search is
/// allocation-free. The longest allowed tag is 10 bytes (`blockquote`/`figcaption`).
pub(crate) fn known_html_tag_index(name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    let mut buf = [0u8; 10];
    for (k, &c) in bytes.iter().enumerate() {
        buf[k] = c.to_ascii_lowercase();
    }
    let lower = &buf[..bytes.len()];
    HICCUP_TAGS.binary_search_by(|t| t.as_bytes().cmp(lower)).ok()
}

/// Case-insensitive membership test against the shared mldoc HTML-element allowlist.
pub(crate) fn is_known_html_tag(name: &str) -> bool {
    known_html_tag_index(name).is_some()
}

fn is_hiccup_tag(name: &str) -> bool {
    is_known_html_tag(name)
}

/// Recognize + capture a Clojure-hiccup vector `[:tag …]` starting at byte `at` (which
/// must be the `[`). Returns the byte index just past the matching `]` (so the captured
/// raw text is `s[at..end]`), or `None` if it isn't a hiccup. Rules verified vs mldoc
/// 1.5.7:
///   1. `[:` then a non-empty `[A-Za-z0-9]+` element name whose lowercase ∈ `HICCUP_TAGS`;
///   2. the char immediately after the name is the keyword boundary — one of
///      `]`, space, tab, `.`, `#` (a CSS-selector start / separator). Anything else
///      (`{ [ " ( , : / -` … or a newline) → NOT a hiccup;
///   3. a string-aware, `[:`-nested balanced scan to the matching `]`: depth starts at 1
///      on the outer `[`, a NESTED `[:` opens a level (+1), a `]` closes (−1, end at 0),
///      a `]` inside a `"…"` string is ignored, but `[:` inside the string still opens
///      a level. A `"` toggles the string state unless the previous byte is `\`
///      (naive one-byte look-back, not escape pairing). A lone `[` (not `[:`) and any
///      `{ }` are literal (NOT balanced). Reaching EOF unbalanced — including an
///      unterminated string — → `None`.
/// Linear in the captured length.
/// Steps (1)+(2) of hiccup recognition — element-name allowlist + keyword boundary —
/// in O(1)+name, with NO balanced scan. Split out so the inline scanner can pair
/// `[:`…`]` once up front (`build_hiccup_close`) and then validate each opener's head
/// against the precomputed close, instead of re-scanning to the closer per opener.
pub(crate) fn hiccup_head_ok(s: &str, at: usize) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'[') || b.get(at + 1) != Some(&b':') {
        return false;
    }
    // (1) element name = maximal [A-Za-z0-9]+ after `[:`, lowercase ∈ HICCUP_TAGS.
    let name_start = at + 2;
    let mut j = name_start;
    while j < n && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == name_start || !is_hiccup_tag(&s[name_start..j]) {
        return false;
    }
    // (2) keyword boundary: `]`, space, tab, `.` or `#` (CSS-selector start / end).
    matches!(b.get(j), Some(b']') | Some(b' ') | Some(b'\t') | Some(b'.') | Some(b'#'))
}

/// Pair EVERY `[:`…`]` hiccup vector in `s` in one linear pass (a delimiter stack:
/// a `[:` always pushes, a `]` pops only outside a `"…"` string, and `"` toggles
/// string state unless the previous byte is `\`). Returns a
/// position-indexed `Vec` (length `|s|`): `close[opener-`[`-byte]` = index-just-past-the-
/// matching-`]`, and `usize::MAX` where no hiccup vector opens (NOT a `HashMap<usize,_>` —
/// the key is a byte position, a perfect array index; see lsdoc/CLAUDE.md). This is the
/// structural replacement for the per-opener balanced re-scan (and the `rbracket` absence
/// cache): the inline dispatch does an O(1) array lookup instead. The balance counts every
/// `[:` regardless of tag validity (tag-validity is applied separately at lookup via
/// [`hiccup_head_ok`]) — matching mldoc's depth scan exactly.
pub(crate) fn build_hiccup_close(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut close = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut p = 0;
    while p < n {
        match b[p] {
            b'[' if p + 1 < n && b[p + 1] == b':' => {
                stack.push(p);
                p += 2;
            }
            b']' if !in_string => {
                if let Some(o) = stack.pop() {
                    close[o] = p + 1;
                }
                p += 1;
            }
            b'"' => {
                if p == 0 || b[p - 1] != b'\\' {
                    in_string = !in_string;
                }
                p += 1;
            }
            c => p += char_len(c),
        }
    }
    close
}

/// Pair `[[`…`]]` the way `match_brackets` (nested-link) balances them — a delimiter
/// stack: `[[` pushes, `]]` pops, a `\n` clears the stack (mldoc returns `None` across a
/// newline). Returns a position-indexed `Vec` (length `|s|`): `close[opener-`[[`-byte]` =
/// index-just-past-the-matching-`]]`, `usize::MAX` where none (a byte position is a perfect
/// array index, not a `HashMap` key). Escape-FREE, because `match_brackets` does not treat
/// `\` specially. Gating nested-link on this means an unbalanced `[[`-run can't trigger a
/// to-EOF level-scan per opener.
pub(crate) fn build_nested_close(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut close = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut p = 0;
    while p < n {
        match b[p] {
            b'\n' => {
                stack.clear();
                p += 1;
            }
            b'[' if p + 1 < n && b[p + 1] == b'[' => {
                stack.push(p);
                p += 2;
            }
            b']' if p + 1 < n && b[p + 1] == b']' => {
                if let Some(o) = stack.pop() {
                    close[o] = p + 2;
                }
                p += 2;
            }
            c => p += char_len(c),
        }
    }
    close
}

/// Sorted positions of escape-aware "real" `]]` — a `]` reached at an UNescaped position
/// whose next byte is also `]`. These are the closers `parse_page_ref` actually breaks on
/// (a `\` consumes the next byte, so `\]]` has no real `]]`; a single `]` is content). One
/// left-to-right pass tracking escape (globally consistent because the scanner dispatches
/// `[[` only at unescaped positions, matching the per-opener name scan's fresh start).
/// Page-ref then closes at the next real `]]` in O(1) amortized via a monotone cursor,
/// never fail-scanning to the eol per opener.
pub(crate) fn build_real_dbl(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut p = 0;
    while p < n {
        let c = b[p];
        if c == b'\\' && p + 1 < n {
            p += 2; // `\` escapes the next byte (both consumed), as in parse_page_ref
        } else if c == b']' && p + 1 < n && b[p + 1] == b']' {
            out.push(p);
            p += 1; // a later opener could still see `]]` starting one byte on (`]]]`)
        } else {
            p += char_len(c);
        }
    }
    out
}

// ---- resolver (v0.2) leaf entry points ------------------------------------
// Free fns returning (node, end) that the resolvers call at each dispatch point; they do no
// no-closer caching of their own — the resolver's `fresh` flag keeps no-closer runs linear
// (only the first opener of a run scans).

/// `\( … \)` (Inline) / `\[ … \]` (Displayed) latex span.
pub(crate) fn parse_latex_backslash_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let (close, mode): (&str, &str) = match b.get(at + 1).copied()? {
        b'(' => ("\\)", "Inline"),
        b'[' => ("\\]", "Displayed"),
        _ => return None,
    };
    let body_start = at + 2;
    let end = find_sub(b, body_start, close.as_bytes())?;
    Some((
        Inline::Latex { mode: mode.to_string(), body: s[body_start..end].to_string(), span: None },
        end + 2,
    ))
}

/// `$$ … $$` (Displayed) / `$ … $` (Inline) latex span (no `$`/newline in the body; the
/// inline form can't start with a space nor end with ` ( [ {`).
pub(crate) fn parse_latex_dollar_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let after = *b.get(at + 1)?;
    if after == b'$' {
        let body_start = at + 2;
        let end = find_sub_line(b, body_start, b"$$")?;
        return Some((
            Inline::Latex { mode: "Displayed".to_string(), body: s[body_start..end].to_string(), span: None },
            end + 2,
        ));
    }
    if after == b' ' {
        return None;
    }
    let body_start = at + 1;
    let mut j = body_start;
    while j < n && b[j] != b'$' && b[j] != b'\n' && b[j] != b'\r' {
        j += 1;
    }
    if j >= n || b[j] != b'$' {
        return None;
    }
    if matches!(b[j - 1], b' ' | b'(' | b'[' | b'{') {
        return None;
    }
    Some((
        Inline::Latex { mode: "Inline".to_string(), body: s[body_start..j].to_string(), span: None },
        j + 1,
    ))
}

/// `(( … ))` block ref (inner has no `)`/newline; value unescaped, `full` raw).
pub(crate) fn parse_block_ref_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("((") {
        return None;
    }
    let inner_start = at + 2;
    let mut j = inner_start;
    while j < n && b[j] != b')' && b[j] != b'\n' && b[j] != b'\r' {
        j += 1;
    }
    if j == inner_start || j + 1 >= n || b[j] != b')' || b[j + 1] != b')' {
        return None;
    }
    Some((
        Inline::Link {
            url: Url::BlockRef { v: unescape(&s[inner_start..j]) },
            label: vec![],
            full: s[at..j + 2].to_string(),
            image: false,
            metadata: String::new(),
            title: None,
            span: None,
        },
        j + 2,
    ))
}

/// `{{{ … }}}` / `{{ … }}` macro (triple tried first); args raw via `parse_macro`.
pub(crate) fn parse_macro_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    if !s[at..].starts_with("{{") {
        return None;
    }
    let b = s.as_bytes();
    let n = b.len();
    let candidates: &[(&str, &str)] = if s[at..].starts_with("{{{") {
        &[("{{{", "}}}"), ("{{", "}}")]
    } else {
        &[("{{", "}}")]
    };
    for &(open, close) in candidates {
        let inner_start = at + open.len();
        let mut j = inner_start;
        while j < n && b[j] != b'}' && b[j] != b'\n' && b[j] != b'\r' {
            j += 1;
        }
        if j == inner_start || !s[j..].starts_with(close) {
            continue;
        }
        if let Some((name, args)) = parse_macro(&s[inner_start..j]) {
            return Some((Inline::Macro { name, args, span: None }, j + close.len()));
        }
    }
    None
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
                span: None,
            }));
        }
    }
    Some((end1, Inline::Timestamp {
        ts: "Date".to_string(),
        date: ts1,
        span: None,
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
                    span: None,
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

//! Inline leaf-parser library — the shared, context-free building blocks of inline
//! parsing, behavior-equivalent to mldoc 1.5.7's inline grammar (`lib/syntax/inline.ml`,
//! verified against the live oracle).
//!
//! The top-level ctx-aware inline pass (lexer → one-pass resolve) lives in the two format
//! resolvers — `crate::resolver` (Markdown) and `crate::org_resolver` (Org). THIS module is
//! their shared leaf kit: byte-class predicates (`is_ws`, `is_ws_or_nl`, …), the
//! bracket/close pre-pair builders (`build_hiccup_close`, `build_nested_close`, …), and the
//! per-construct leaf parsers both resolvers call — page refs (`parse_page_ref`), nested links
//! (`parse_nested_link`), md links/images (`md_link`), autolinks, bare URLs, timestamps, latex
//! spans, hiccup, entities, escapes. Each returns `(node, end)` (or `None`) and does no
//! delimiter pairing of its own; the resolver drives them.
//!
//! Markdown link/image LABELS are re-parsed through the C1 Markdown emphasis port plus
//! mldoc's label-only latex/entity/code/script choices (`resolver::parse_inline_ctx_md_label`).
//! The old standalone v1 `Scanner` inline engine that used to live here was retired once both
//! resolvers shipped.
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

#[derive(Default)]
pub(crate) struct AngleBoundaryScan {
    next: usize,
    initialized: bool,
}

impl AngleBoundaryScan {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn first_from(&mut self, b: &[u8], from: usize) -> usize {
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len() && b[p] != b'>' && !is_ws_or_nl(b[p]) {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

pub(crate) struct AutolinkScan {
    boundary: AngleBoundaryScan,
}

impl AutolinkScan {
    pub(crate) fn new() -> Self {
        Self {
            boundary: AngleBoundaryScan::new(),
        }
    }
}

pub(crate) struct EmailAutolinkScan {
    no_at_from: usize,
    domain_boundary: AngleBoundaryScan,
}

impl EmailAutolinkScan {
    pub(crate) fn new() -> Self {
        Self {
            no_at_from: usize::MAX,
            domain_boundary: AngleBoundaryScan::new(),
        }
    }
}

#[derive(Default)]
pub(crate) struct TimestampCloseScan {
    next: usize,
    initialized: bool,
    no_close_from: usize,
}

impl TimestampCloseScan {
    pub(crate) fn new() -> Self {
        Self {
            next: 0,
            initialized: false,
            no_close_from: usize::MAX,
        }
    }

    fn first_close_or_lf(&mut self, b: &[u8], from: usize, close: u8) -> usize {
        if from >= self.no_close_from {
            return b.len();
        }
        if !self.initialized || self.next < from {
            let mut p = from;
            let mut scanned = 0usize;
            while p < b.len() && b[p] != close && b[p] != b'\n' {
                scanned += 1;
                p += char_len(b[p]);
            }
            if p < b.len() {
                scanned += 1;
            } else {
                self.no_close_from = self.no_close_from.min(from);
            }
            crate::metrics::scan_work(scanned);
            self.next = p;
            self.initialized = true;
        }
        self.next
    }
}

pub(crate) struct BareUrlScan {
    no_scheme_from: usize,
}

impl BareUrlScan {
    pub(crate) fn new() -> Self {
        Self {
            no_scheme_from: usize::MAX,
        }
    }
}

/// Boundary-run map for hash tags: for every byte in a run of tag delimiters, `true`
/// means the run suffix is followed by whitespace/eol/EOF and therefore terminates the
/// tag. The resolver builds this once per inline string so `#`×n does not re-scan the
/// same delimiter suffix at every failed tag dispatch.
pub(crate) fn build_tag_boundary_runs(s: &str) -> Vec<bool> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = vec![false; n];
    let mut i = 0usize;
    while i < n {
        if !TAG_DELIMS.contains(&b[i]) {
            i += char_len(b[i]);
            continue;
        }
        let start = i;
        while i < n && TAG_DELIMS.contains(&b[i]) {
            i += 1;
        }
        let boundary = i >= n || is_ws_or_nl(b[i]);
        if boundary {
            for slot in &mut out[start..i] {
                *slot = true;
            }
        }
    }
    crate::metrics::scan_work(n);
    out
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
    boundary_runs: Option<&[bool]>,
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
        let boundary_run = boundary_runs
            .and_then(|runs| runs.get(i))
            .copied()
            .unwrap_or_else(|| {
                let mut k = i;
                let mut scanned = 0usize;
                while k < n && TAG_DELIMS.contains(&b[k]) {
                    scanned += 1;
                    k += 1;
                }
                crate::metrics::scan_work(scanned);
                k > i && (k >= n || is_ws_or_nl(b[k]))
            });
        if boundary_run {
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

/// Resolver entry for mldoc `markdown_link` / `markdown_image`
/// (`lib/syntax/inline.ml:723-890,1138-1160`). `at` points at the `[`; when
/// `image` is true the caller consumed the leading `!` at `at - 1`.
/// `base` is the absolute byte offset of `s` in the block body (for label-child spans).
pub(crate) fn md_link(s: &str, at: usize, image: bool, base: usize) -> Option<(Inline, usize)> {
    parse_md_link(s, at, image, base).map(|l| (l.node, l.end))
}

fn parse_md_link(s: &str, at: usize, image: bool, base: usize) -> Option<MdLink> {
    if image {
        if let Some(link) = markdown_embed_image(s, at, base) {
            return Some(link);
        }
    }
    markdown_link(s, at, image, base)
}

/// mldoc `markdown_embed_image` (`syntax/inline.ml:1138-1152`): this branch is
/// first and separate from `markdown_link`, so `data:` payloads do not go through
/// URL-piece parsing or title parsing.
fn markdown_embed_image(s: &str, at: usize, base: usize) -> Option<MdLink> {
    let label = label_part(s, at, base, false)?;
    let b = s.as_bytes();
    let data_start = label.url_start;
    if !s[data_start..].starts_with("data:") {
        return None;
    }
    let mut j = data_start + "data:".len();
    if j >= b.len() || b[j] == b')' {
        return None;
    }
    while j < b.len() && b[j] != b')' {
        j += char_len(b[j]);
    }
    if j >= b.len() || b[j] != b')' {
        return None;
    }
    let data = s[data_start..j].to_string();
    let mut end = j + 1;
    let metadata = read_metadata(s, b, &mut end);
    let full = format!("![{}]({}){}", label.label_text, data, metadata);
    Some(MdLink {
        node: Inline::Link {
            url: Url::EmbedData { v: data },
            label: label.label,
            full,
            image: true,
            metadata,
            title: None,
            span: None,
        },
        end,
    })
}

/// mldoc `markdown_link` (`syntax/inline.ml:822-890`).
fn markdown_link(s: &str, at: usize, image: bool, base: usize) -> Option<MdLink> {
    let label = label_part(s, at, base, true)?;
    let (url_text, after_url) = link_url_part(s, label.url_start)?;
    let mut end = after_url;
    let metadata = read_metadata(s, s.as_bytes(), &mut end);
    let (link_type, url_value, title) = link_url_part_inner(&url_text)
        .unwrap_or((MdUrlType::Other, url_text.clone(), None));
    let trimmed = url_value.trim();
    let unescaped;
    let url_value = if link_type == MdUrlType::Other {
        unescaped = unescape(trimmed);
        unescaped.as_str()
    } else {
        trimmed
    };
    let url = classify_markdown_url(link_type, url_value);
    let prefix = if image { "!" } else { "" };
    let full = format!("{}[{}]({}){}", prefix, label.label_text, url_text, metadata);
    Some(MdLink {
        node: Inline::Link { url, label: label.label, full, image, metadata, title, span: None },
        end,
    })
}

struct MdLabelPart {
    label: Vec<Inline>,
    label_text: String,
    url_start: usize,
}

/// mldoc `label_part` / `label_part_choices` (`syntax/inline.ml:735-770`).
fn label_part(s: &str, at: usize, base: usize, reparse_plain: bool) -> Option<MdLabelPart> {
    let b = s.as_bytes();
    let n = b.len();
    if s[at..].starts_with("[](") {
        return Some(MdLabelPart { label: vec![], label_text: String::new(), url_start: at + 3 });
    }
    let mut j = at + 1;
    let mut raw_nodes: Vec<Inline> = Vec::new();
    while j < n {
        if s[j..].starts_with("](") {
            let label_text = label_text_for_full(&raw_nodes);
            let label = finish_markdown_label(raw_nodes, reparse_plain);
            return Some(MdLabelPart { label, label_text, url_start: j + 2 });
        }
        let c = b[j];
        if let Some(end) = take_while1_include_backslash_len(s, j, b"[]", |c| {
            c != b'\n' && c != b'\r' && !matches!(c, b'`' | b'[' | b']')
        }) {
            push_label_plain(&mut raw_nodes, &s[j..end], base + j);
            j = end;
            continue;
        }
        if c == b'`' {
            if let Some(end) = code_span_end_str(s, j) {
                raw_nodes.push(Inline::Code {
                    text: code_inner(&s[j..end]),
                    span: Some(Span(base + j, base + end)),
                });
                j = end;
                continue;
            }
            push_label_plain(&mut raw_nodes, "`", base + j);
            j += 1;
            continue;
        }
        if c == b'\\' && j + 1 < n {
            let end = j + 1 + char_len(b[j + 1]);
            push_label_plain(&mut raw_nodes, &s[j..end], base + j);
            j = end;
            continue;
        }
        if c == b'\\' {
            push_label_plain(&mut raw_nodes, "\\", base + j);
            j += 1;
            continue;
        }
        if c == b'[' {
            if let Some((end, _name, full)) = parse_page_ref(s, j) {
                push_label_plain(&mut raw_nodes, &full, base + j);
                j = end;
                continue;
            }
            let (text, end) = string_contains_balanced_brackets_single(s, j, b'[', b']', b"[]", b"", b"");
            if end > j {
                push_label_plain(&mut raw_nodes, &text, base + j);
                j = end;
                continue;
            }
        }
        if c == b']' || c == b'\n' || c == b'\r' {
            return None;
        }
        push_label_plain(&mut raw_nodes, &s[j..j + char_len(c)], base + j);
        j += char_len(c);
    }
    None
}

fn finish_markdown_label(nodes: Vec<Inline>, reparse_plain: bool) -> Vec<Inline> {
    if reparse_plain {
        reparse_markdown_label(nodes)
    } else {
        unescape_markdown_label(nodes)
    }
}

fn unescape_markdown_label(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Inline::Plain { text, span } => {
                let value = unescape(&text);
                let span = if value.len() == text.len() { span } else { None };
                out.push(Inline::Plain { text: value, span });
            }
            other => out.push(other),
        }
    }
    concat_label_plains(out)
}

fn push_label_plain(nodes: &mut Vec<Inline>, raw: &str, abs_start: usize) {
    if raw.is_empty() {
        return;
    }
    match nodes.last_mut() {
        Some(Inline::Plain { text, span }) => {
            text.push_str(raw);
            *span = span.map(|Span(start, _)| Span(start, abs_start + raw.len()));
        }
        _ => nodes.push(Inline::Plain {
            text: raw.to_string(),
            span: Some(Span(abs_start, abs_start + raw.len())),
        }),
    }
}

fn label_text_for_full(nodes: &[Inline]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Inline::Plain { text, .. } => out.push_str(text),
            Inline::Code { text, .. } => {
                out.push('`');
                out.push_str(text);
                out.push('`');
            }
            _ => {}
        }
    }
    out
}

fn reparse_markdown_label(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Inline::Plain { text, span } => {
                let base = span.map(|Span(start, _)| start).unwrap_or(0);
                if let Some(nodes) = crate::resolver::parse_inline_ctx_md_label(&text, base) {
                    out.extend(nodes);
                } else {
                    let value = unescape(&text);
                    let span = if value.len() == text.len() { span } else { None };
                    out.push(Inline::Plain { text: value, span });
                }
            }
            other => out.push(other),
        }
    }
    concat_label_plains(out)
}

fn concat_label_plains(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for node in nodes {
        match (out.last_mut(), node) {
            (Some(Inline::Plain { text: prev, span: prev_span }), Inline::Plain { text, span }) => {
                prev.push_str(&text);
                *prev_span = match (*prev_span, span) {
                    (Some(Span(start, _)), Some(Span(_, end))) => Some(Span(start, end)),
                    _ => None,
                };
            }
            (_, node) => out.push(node),
        }
    }
    out
}

pub(crate) fn take_while1_include_backslash_len<F>(
    s: &str,
    at: usize,
    chars_can_escape: &[u8],
    mut pred: F,
) -> Option<usize>
where
    F: FnMut(u8) -> bool,
{
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut last_backslash = false;
    while j < n {
        let c = b[j];
        let take = if last_backslash && chars_can_escape.contains(&c) {
            last_backslash = false;
            true
        } else if last_backslash {
            last_backslash = false;
            pred(c)
        } else if c == b'\\' {
            last_backslash = true;
            true
        } else {
            pred(c)
        };
        if !take {
            break;
        }
        j += char_len(c);
    }
    (j > at).then_some(j)
}

/// Iterative single-pair port of `Parsers.string_contains_balanced_brackets`
/// (`parsers.ml:293-332`) for the C2 call sites. It keeps the source helper's
/// empty success, escape handling, excluded-ending rule, and unmatched-left
/// fallback while avoiding a deep Rust call stack.
fn string_contains_balanced_brackets_single(
    s: &str,
    at: usize,
    left: u8,
    right: u8,
    escape_chars: &[u8],
    other_delims: &[u8],
    excluded_ending_chars: &[u8],
) -> (String, usize) {
    let b = s.as_bytes();
    let n = b.len();
    let mut j = at;
    let mut depth = 0usize;
    let mut out = String::new();
    while j < n {
        let c = b[j];
        if other_delims.contains(&c) {
            break;
        }
        if excluded_ending_chars.contains(&c) {
            let remain = n - j;
            if remain < 2 || b.get(j + 1).is_some_and(|c2| other_delims.contains(c2)) {
                break;
            }
            out.push(c as char);
            j += 1;
            continue;
        }
        if c == left {
            out.push(c as char);
            depth += 1;
            j += 1;
            continue;
        }
        if c == right {
            if depth == 0 {
                break;
            }
            out.push(c as char);
            depth -= 1;
            j += 1;
            continue;
        }
        if c == b'\\' {
            out.push('\\');
            j += 1;
            if j < n {
                let next = b[j];
                let next_plain = !other_delims.contains(&next)
                    && !excluded_ending_chars.contains(&next)
                    && next != left
                    && next != right;
                if escape_chars.contains(&next) || next_plain {
                    let w = char_len(next);
                    out.push_str(&s[j..j + w]);
                    j += w;
                }
            }
            continue;
        }
        let w = char_len(c);
        out.push_str(&s[j..j + w]);
        j += w;
    }
    (out, j)
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
        return Some(end + 2);
    }
    None
}

fn code_inner(span: &str) -> String {
    if span.starts_with("``") {
        span[2..span.len() - 2].to_string()
    } else {
        span[1..span.len() - 1].to_string()
    }
}

/// mldoc `link_url_part` (`syntax/inline.ml:723-733`).
fn link_url_part(s: &str, at: usize) -> Option<(String, usize)> {
    let (mut text, end) =
        string_contains_balanced_brackets_single(s, at, b'(', b')', b"()", b"\r\n", b"");
    if s.as_bytes().get(end) == Some(&b')') {
        return Some((text, end + 1));
    }
    if text.ends_with(')') {
        text.pop();
        return Some((text, end));
    }
    None
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum MdUrlType {
    BlockRef,
    PageRef,
    Other,
    Other1,
    Other2,
}

/// mldoc `link_url_part_inner` (`syntax/inline.ml:772-813`).
fn link_url_part_inner(url_text: &str) -> Option<(MdUrlType, String, Option<String>)> {
    let b = url_text.as_bytes();
    let n = b.len();
    let mut j = 0usize;
    let mut parts: Vec<(MdUrlType, String)> = Vec::new();
    while j < n {
        if let Some((kind, value, end)) = url_part_piece(url_text, j) {
            parts.push((kind, value));
            j = end;
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let (kind, value) = if parts.len() == 1 {
        let (kind, value) = parts.pop().unwrap();
        match kind {
            MdUrlType::Other1 | MdUrlType::Other2 => (MdUrlType::Other, value),
            _ => (kind, value),
        }
    } else {
        if parts.iter().any(|(kind, _)| *kind == MdUrlType::Other1) {
            return None;
        }
        (MdUrlType::Other, parts.into_iter().map(|(_, value)| value).collect())
    };
    while j < n && matches!(b[j], b' ' | b'\t' | 0x16 | 0x0c) {
        j += 1;
    }
    let title = if j >= n {
        None
    } else if b[j] == b'"' {
        let start = j + 1;
        let end = take_while1_include_backslash_len(url_text, start, b"\"", |c| c != b'"')?;
        if end >= n || b[end] != b'"' {
            return None;
        }
        j = end + 1;
        if j != n {
            return None;
        }
        Some(url_text[start..end].to_string())
    } else {
        return None;
    };
    Some((kind, value, title))
}

fn url_part_piece(url_text: &str, at: usize) -> Option<(MdUrlType, String, usize)> {
    let b = url_text.as_bytes();
    let n = b.len();
    if at >= n {
        return None;
    }
    if url_text[at..].starts_with("((") {
        let mut j = at + 2;
        while j < n && b[j] != b')' {
            j += char_len(b[j]);
        }
        if j > at + 2 && j + 1 < n && b[j] == b')' && b[j + 1] == b')' {
            return Some((MdUrlType::BlockRef, url_text[at..j + 2].to_string(), j + 2));
        }
    }
    if b[at] == b'<' {
        let start = at + 1;
        let end = take_while1_include_backslash_len(url_text, start, b"<>", |c| {
            c != b'<' && c != b'>'
        })?;
        if end < n && b[end] == b'>' {
            return Some((MdUrlType::Other1, url_text[start..end].to_string(), end + 1));
        }
    }
    if b[at] != b'[' && !is_ws_or_nl(b[at]) {
        let mut j = at;
        while j < n && !is_ws_or_nl(b[j]) && b[j] != b'[' {
            j += char_len(b[j]);
        }
        if j > at {
            return Some((MdUrlType::Other2, url_text[at..j].to_string(), j));
        }
    }
    if url_text[at..].starts_with("[[") {
        if let Some((end, _name, full)) = parse_page_ref(url_text, at) {
            return Some((MdUrlType::PageRef, full, end));
        }
    }
    if b[at] == b' ' {
        return None;
    }
    let w = char_len(b[at]);
    Some((MdUrlType::Other2, url_text[at..at + w].to_string(), at + w))
}

fn classify_markdown_url(link_type: MdUrlType, url: &str) -> Url {
    match link_type {
        MdUrlType::BlockRef => Url::BlockRef { v: url[2..url.len().saturating_sub(2)].to_string() },
        MdUrlType::PageRef => Url::PageRef { v: url[2..url.len().saturating_sub(2)].to_string() },
        MdUrlType::Other => {
            if let Some(idx) = url.find(':') {
                let protocol = &url[..idx];
                if !protocol.is_empty() && url[idx..].starts_with("://") {
                    let mut link = &url[idx + 3..];
                    if let Some(stripped) = link.strip_prefix("//") {
                        link = stripped;
                    }
                    return Url::Complex {
                        protocol: Some(protocol.to_string()),
                        link: Some(link.to_string()),
                    };
                }
            }
            let lower = url.to_ascii_lowercase();
            if url.len() > 3 && (lower.ends_with(".md") || lower.ends_with(".markdown")) {
                Url::File { v: url.to_string() }
            } else {
                Url::Search { v: url.to_string() }
            }
        }
        MdUrlType::Other1 | MdUrlType::Other2 => unreachable!("normalized before classification"),
    }
}

/// Shared mldoc `metadata` (`syntax/inline.ml:562-566`).
fn read_metadata(s: &str, b: &[u8], end: &mut usize) -> String {
    if b.get(*end) == Some(&b'{') {
        if let Some(close) = find_sub_line(b, *end + 1, b"}") {
            let meta = s[*end..close + 1].to_string();
            *end = close + 1;
            return meta;
        }
    }
    String::new()
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
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
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
    let mut scanned = 0usize;
    while j < n && !is_ws_or_nl(b[j]) && b[j] != b'>' {
        scanned += 1;
        j += char_len(b[j]);
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
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

/// Dispatch-owner guard for `<scheme:...>`: the unbounded part of autolink parsing is
/// the search for the first `>`/whitespace boundary after the scheme. A single monotone
/// boundary cursor owns that scan; EOF is a suffix-absence miss and whitespace-before-`>`
/// is an invalidating token until the dispatch cursor passes it.
pub(crate) fn autolink_has_closing_boundary(s: &str, at: usize, scan: &mut AutolinkScan) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return false;
    }
    let mut j = at + 1;
    let p0 = j;
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
    if j == p0 || j >= n || b[j] != b':' {
        return false;
    }
    j += 1;
    if s[j..].starts_with("//") {
        j += 2;
    }
    let link_start = j;
    let boundary = scan.boundary.first_from(b, link_start);
    boundary < n && b[boundary] == b'>' && boundary > link_start
}

/// `<a@b.com>` email autolink. Returns (end, node) with the address object.
#[allow(dead_code)]
pub(crate) fn parse_email_autolink_with_no_at_floor(
    s: &str,
    at: usize,
    no_at_from: &mut usize,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    let mut j = at + 1;
    let local_start = j;
    if local_start >= *no_at_from {
        return None;
    }
    let mut scanned = 0usize;
    while j < n && b[j] != b'@' && b[j] != b'>' && !is_ws_or_nl(b[j]) {
        scanned += 1;
        j += 1;
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j >= n {
        *no_at_from = (*no_at_from).min(local_start);
        return None;
    }
    if j >= n || b[j] != b'@' || j == local_start {
        return None;
    }
    let local = s[local_start..j].to_string();
    j += 1;
    let dom_start = j;
    let mut domain_scanned = 0usize;
    while j < n && b[j] != b'>' && !is_ws_or_nl(b[j]) {
        domain_scanned += 1;
        j += 1;
    }
    if j < n {
        domain_scanned += 1;
    }
    crate::metrics::scan_work(domain_scanned);
    if j >= n || b[j] != b'>' || j == dom_start {
        return None;
    }
    let domain = s[dom_start..j].to_string();
    let val = serde_json::json!({ "local_part": local, "domain": domain });
    Some((j + 1, Inline::Email { text: val, span: None }))
}

pub(crate) fn parse_email_autolink_cached(
    s: &str,
    at: usize,
    scan: &mut EmailAutolinkScan,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    let mut j = at + 1;
    let local_start = j;
    if local_start >= scan.no_at_from {
        return None;
    }
    let mut scanned = 0usize;
    while j < n && b[j] != b'@' && b[j] != b'>' && !is_ws_or_nl(b[j]) {
        scanned += 1;
        j += 1;
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j >= n {
        scan.no_at_from = scan.no_at_from.min(local_start);
        return None;
    }
    if b[j] != b'@' || j == local_start {
        return None;
    }
    let local = s[local_start..j].to_string();
    j += 1;
    let dom_start = j;
    let boundary = scan.domain_boundary.first_from(b, dom_start);
    if boundary >= n || b[boundary] != b'>' || boundary == dom_start {
        return None;
    }
    let domain = s[dom_start..boundary].to_string();
    let val = serde_json::json!({ "local_part": local, "domain": domain });
    Some((boundary + 1, Inline::Email { text: val, span: None }))
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
#[allow(dead_code)]
pub(crate) fn parse_bare_url(s: &str, at: usize) -> Option<(usize, Inline)> {
    let mut scan = BareUrlScan::new();
    parse_bare_url_with_scan(s, at, &mut scan)
}

pub(crate) fn parse_bare_url_with_scan(
    s: &str,
    at: usize,
    scan: &mut BareUrlScan,
) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if at >= scan.no_scheme_from {
        return None;
    }
    // protocol
    let mut j = at;
    let mut proto_scanned = 0usize;
    while j < n && b[j].is_ascii_alphanumeric() {
        proto_scanned += 1;
        j += 1;
    }
    if j < n {
        proto_scanned += 1;
    }
    crate::metrics::scan_work(proto_scanned);
    if j == at || !s[j..].starts_with("://") {
        if j >= n {
            scan.no_scheme_from = scan.no_scheme_from.min(at);
        }
        return None;
    }
    let protocol = s[at..j].to_string();
    j += 3; // past "://"
    let path_start = j;
    // before_path: until space / '/' / '?' / '#' / inline_link_delims ([]<>{}())
    let mut path_scanned = 0usize;
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
        path_scanned += 1;
        j += char_len(c);
    }
    if j < n {
        path_scanned += 1;
    }
    crate::metrics::scan_work(path_scanned);
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
    let mut scanned = 0usize;
    while j < n {
        let c = b[j];
        scanned += 1;
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
    crate::metrics::scan_work(scanned);
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

/// `(( … ))` block ref (inner has no `)`; value unescaped, `full` raw).
pub(crate) fn parse_block_ref_at(s: &str, at: usize) -> Option<(Inline, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    if !s[at..].starts_with("((") {
        return None;
    }
    let inner_start = at + 2;
    let mut j = inner_start;
    let mut scanned = 0usize;
    while j < n && b[j] != b')' {
        scanned += 1;
        j += 1;
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
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
        let mut scanned = 0usize;
        while j < n && b[j] != b'}' && b[j] != b'\n' && b[j] != b'\r' {
            scanned += 1;
            j += 1;
        }
        if j < n {
            scanned += 1;
        }
        crate::metrics::scan_work(scanned);
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
#[allow(dead_code)]
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

pub(crate) fn parse_angle_timestamp_with_scan(
    s: &str,
    at: usize,
    scan: &mut TimestampCloseScan,
) -> Option<(usize, Inline)> {
    if !bracket_date_has_close_before_lf(s, at, b'<', b'>', scan) {
        return None;
    }
    let (end1, ts1) = parse_bracket_date(s, at, b'<', b'>')?;
    if s[end1..].starts_with("--")
        && bracket_date_has_close_before_lf(s, end1 + 2, b'<', b'>', scan)
    {
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
    match b.get(inner_start) {
        Some(c) if c.is_ascii_digit() || *c == b'+' || is_ws(*c) => {}
        Some(_) => {
            crate::metrics::scan_work(1);
            return None;
        }
        None => return None,
    }
    let mut j = inner_start;
    let mut scanned = 0usize;
    while j < n && b[j] != close && b[j] != b'\n' {
        scanned += 1;
        j += 1;
    }
    if j < n {
        scanned += 1;
    }
    crate::metrics::scan_work(scanned);
    if j >= n || b[j] != close {
        return None;
    }
    let inner = &s[inner_start..j];
    let obj = parse_date_inner(inner, open == b'<')?;
    Some((j + 1, obj))
}

fn bracket_date_has_close_before_lf(
    s: &str,
    at: usize,
    open: u8,
    close: u8,
    scan: &mut TimestampCloseScan,
) -> bool {
    let b = s.as_bytes();
    if b.get(at) != Some(&open) {
        return false;
    }
    let inner_start = at + 1;
    match b.get(inner_start) {
        Some(c) if c.is_ascii_digit() || *c == b'+' || is_ws(*c) => {}
        _ => return false,
    }
    let boundary = scan.first_close_or_lf(b, inner_start, close);
    boundary < b.len() && b[boundary] == close
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

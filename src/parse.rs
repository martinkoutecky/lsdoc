//! Block segmentation — milestone 2.
//!
//! A single-pass, line-based scanner that splits input into mldoc-equivalent
//! blocks. Inline content is still a stub (the whole block text as one Plain);
//! real inline parsing lands in M3/M4. The differential gate for this milestone
//! is `block-struct` (kind/level/nesting/properties), which ignores inline content
//! and spans.
//!
//! Complexity: O(n). Each line is classified in O(line length); fenced code
//! regions are pre-paired in a single forward pass (see `pair_fences`) so an
//! unclosed/￼adversarial run of ``` markers can't trigger O(n²) re-scanning.
//!
//! mldoc quirks replicated (see DECISIONS.md / the block probe):
//! - only `-` bullets become `Bullet` (mldoc `Heading{unordered}`); `*`/`+` and
//!   `N.` become `List` nodes; `N)` is NOT a list.
//! - heading `level` is always 1 with `size` = `#`-count (uncapped); a space must
//!   follow the hashes.
//! - consecutive non-block lines (incl. blank lines) coalesce into ONE paragraph.
//! - unclosed fences and 4-space indents are paragraphs, not code.

use crate::projection::{Block, Inline, ListItem, Span};
use std::collections::HashMap;

struct Line<'a> {
    start: usize, // byte offset of line start
    end: usize,   // byte offset just past the trailing '\n' (or EOF)
    text: &'a str, // line content WITHOUT the trailing '\n'
}

pub fn parse(input: &str) -> Vec<Block> {
    let lines = split_lines(input);
    let fences = pair_fences(&lines); // open-line-idx -> (close-line-idx, lang)

    let mut out: Vec<Block> = Vec::new();
    let mut para: Option<(usize, usize)> = None;
    let mut i = 0;

    while i < lines.len() {
        let line = &lines[i];
        let t = line.text;

        // 1. fenced code (Src) — pre-paired, so this is the open line.
        if let Some((close, lang)) = fences.get(&i) {
            flush_para(&mut out, &mut para, input);
            let code = if *close > i + 1 {
                input[lines[i + 1].start..lines[*close - 1].end].to_string()
            } else {
                String::new()
            };
            out.push(Block::Src {
                lang: lang.clone(),
                code,
                span: Some(Span(line.start, lines[*close].end)),
            });
            i = *close + 1;
            continue;
        }

        // 2. callout #+BEGIN_X … #+END_X
        if let Some(name) = callout_begin(t) {
            if let Some(close) = find_callout_end(&lines, i, &name) {
                flush_para(&mut out, &mut para, input);
                let inner = if close > i + 1 {
                    input[lines[i + 1].start..lines[close - 1].end].to_string()
                } else {
                    String::new()
                };
                let children = parse(&inner);
                let span = Some(Span(line.start, lines[close].end));
                if name.eq_ignore_ascii_case("QUOTE") {
                    out.push(Block::Quote { children, span });
                } else {
                    out.push(Block::Custom { name: name.to_ascii_lowercase(), children, span });
                }
                i = close + 1;
                continue;
            }
            // no matching END → fall through (treat as paragraph text).
        }

        // 3. heading
        if let Some(size) = heading_size(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::Heading {
                level: 1,
                size: Some(size),
                inline: stub_inline(t),
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }

        // 4. horizontal rule (before dash bullet / list)
        if is_hr(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::Hr { span: Some(Span(line.start, line.end)) });
            i += 1;
            continue;
        }

        // 5. `-` bullet (mldoc Heading{unordered})
        if let Some(level) = dash_bullet_level(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::Bullet {
                level,
                inline: stub_inline(t),
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }

        // 6. footnote definition
        if let Some(fname) = footnote_def_name(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::FootnoteDef {
                name: fname,
                inline: stub_inline(t),
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }

        // 7. table (group of consecutive `|` lines)
        if t.trim_start().starts_with('|') {
            flush_para(&mut out, &mut para, input);
            let start = i;
            while i < lines.len() && lines[i].text.trim_start().starts_with('|') {
                i += 1;
            }
            out.push(build_table(&lines[start..i], lines[start].start, lines[i - 1].end));
            continue;
        }

        // 8. property drawer (group of consecutive `key:: value` lines)
        if property(t).is_some() {
            flush_para(&mut out, &mut para, input);
            let start = i;
            let mut props = Vec::new();
            while i < lines.len() {
                if let Some(kv) = property(lines[i].text) {
                    props.push(kv);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Block::Properties {
                props,
                span: Some(Span(lines[start].start, lines[i - 1].end)),
            });
            continue;
        }

        // 9. list (group of consecutive `*`/`+`/`N.` items)
        if let Some(item) = list_item(t) {
            flush_para(&mut out, &mut para, input);
            let start = i;
            let mut items = vec![item];
            i += 1;
            while i < lines.len() {
                if let Some(it) = list_item(lines[i].text) {
                    items.push(it);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Block::List {
                items,
                span: Some(Span(lines[start].start, lines[i - 1].end)),
            });
            continue;
        }

        // 10. quote (group of consecutive `>` lines, indentation tolerated)
        if t.trim_start().starts_with('>') {
            flush_para(&mut out, &mut para, input);
            let start = i;
            while i < lines.len() && lines[i].text.trim_start().starts_with('>') {
                i += 1;
            }
            let body = input[lines[start].start..lines[i - 1].end].to_string();
            out.push(Block::Quote {
                children: vec![Block::Paragraph {
                    inline: stub_inline(&body),
                    span: None,
                }],
                span: Some(Span(lines[start].start, lines[i - 1].end)),
            });
            continue;
        }

        // 11. raw HTML (single-line, minimal)
        if is_raw_html(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::RawHtml {
                text: t.to_string(),
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }

        // 11b. block-level displayed math: a line that is just `$$ … $$`.
        if let Some(math) = displayed_math(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::DisplayedMath {
                text: math,
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }

        // 11c. org-style drawer `:NAME: … :END:` (e.g. :LOGBOOK:).
        if let Some(name) = drawer_begin(t) {
            if let Some(close) = find_drawer_end(&lines, i) {
                flush_para(&mut out, &mut para, input);
                out.push(Block::Drawer {
                    name,
                    span: Some(Span(line.start, lines[close].end)),
                });
                i = close + 1;
                continue;
            }
            // no :END: → fall through to paragraph.
        }

        // 12. plain line — accumulate into the current paragraph.
        para = Some(match para {
            Some((s, _)) => (s, line.end),
            None => (line.start, line.end),
        });
        i += 1;
    }

    flush_para(&mut out, &mut para, input);
    out
}

// ---- helpers --------------------------------------------------------------

fn flush_para(out: &mut Vec<Block>, para: &mut Option<(usize, usize)>, input: &str) {
    if let Some((s, e)) = para.take() {
        out.push(Block::Paragraph {
            inline: stub_inline(&input[s..e]),
            span: Some(Span(s, e)),
        });
    }
}

fn stub_inline(s: &str) -> Vec<Inline> {
    // M2 placeholder; M3 replaces with the real inline parser. (Ignored by the
    // block-struct gate.)
    if s.is_empty() {
        vec![]
    } else {
        vec![Inline::Plain { text: s.to_string() }]
    }
}

fn split_lines(input: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let bytes = input.as_bytes();
    let n = input.len();
    let mut i = 0;
    while i < n {
        let start = i;
        let mut j = i;
        while j < n && bytes[j] != b'\n' {
            j += 1;
        }
        let content_end = j;
        let end = if j < n { j + 1 } else { j };
        lines.push(Line { start, end, text: &input[start..content_end] });
        i = end;
    }
    lines
}

/// Greedy left-to-right pairing of fenced code markers in one pass → O(n).
/// Returns open-line-idx -> (close-line-idx, language). Unpaired markers are not
/// fences (so an unclosed fence falls through to paragraph text).
fn pair_fences(lines: &[Line]) -> HashMap<usize, (usize, String)> {
    let mut out = HashMap::new();
    let mut open: Option<(usize, u8)> = None;
    for (idx, l) in lines.iter().enumerate() {
        if let Some((c, _len)) = fence_marker(l.text) {
            match open {
                None => open = Some((idx, c)),
                Some((oidx, oc)) => {
                    if c == oc {
                        let (_, mend) = fence_marker(lines[oidx].text).unwrap();
                        let lang = lines[oidx].text[mend..].trim().to_string();
                        out.insert(oidx, (idx, lang));
                        open = None;
                    }
                    // different marker while open → it's code content, ignore.
                }
            }
        }
    }
    out
}

/// A code-fence marker line: 3+ ` or ~ after optional leading whitespace (Logseq
/// indents fences under bullets). Returns (marker char, byte offset just past the
/// run — i.e. where the language tag begins).
fn fence_marker(s: &str) -> Option<(u8, usize)> {
    let b = s.as_bytes();
    let ws = leading_ws(s);
    let c = *b.get(ws)?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut k = ws;
    while k < b.len() && b[k] == c {
        k += 1;
    }
    if k - ws >= 3 {
        Some((c, k))
    } else {
        None
    }
}

/// `#{1,n}` followed by a space ⇒ heading of `size` n (level always 1).
fn heading_size(s: &str) -> Option<u32> {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 {
        return None;
    }
    let rest = &s[hashes..];
    // a space/tab must follow the hashes — or the line is just the hashes ("#").
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(hashes as u32)
    } else {
        None
    }
}

fn is_hr(s: &str) -> bool {
    let t = s.trim();
    if t.len() < 3 {
        return false;
    }
    let c = t.as_bytes()[0];
    (c == b'-' || c == b'*' || c == b'_') && t.bytes().all(|b| b == c)
}

fn leading_ws(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

/// `(ws)- ` ⇒ bullet of level `1 + ws` (each space/tab counts 1).
fn dash_bullet_level(s: &str) -> Option<u32> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    let after = rest.strip_prefix('-')?;
    if after.starts_with(' ') || after.starts_with('\t') {
        Some(1 + ws as u32)
    } else {
        None
    }
}

fn list_item(s: &str) -> Option<ListItem> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    // unordered * or +
    if let Some(after) = rest.strip_prefix('*').or_else(|| rest.strip_prefix('+')) {
        if after.starts_with(' ') || after.starts_with('\t') {
            return Some(ListItem {
                ordered: false,
                number: None,
                indent: ws as u32,
                content: vec![Block::Paragraph {
                    inline: stub_inline(rest),
                    span: None,
                }],
                items: vec![],
            });
        }
    }
    // ordered N.  (NOT N))
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        let after = &rest[digits..];
        if let Some(after2) = after.strip_prefix('.') {
            if after2.starts_with(' ') || after2.starts_with('\t') {
                if let Ok(number) = rest[..digits].parse::<u32>() {
                    return Some(ListItem {
                        ordered: true,
                        number: Some(number),
                        indent: ws as u32,
                        content: vec![Block::Paragraph {
                            inline: stub_inline(rest),
                            span: None,
                        }],
                        items: vec![],
                    });
                }
            }
        }
    }
    None
}

fn property(s: &str) -> Option<(String, String)> {
    let s = s.trim_start(); // property lines may be indented under a block
    let pos = s.find("::")?;
    let key = &s[..pos];
    if key.is_empty() || key.contains(' ') || key.contains('\t') {
        return None;
    }
    let rest = &s[pos + 2..];
    // `::` must be followed by a space or end-of-line ("a::b mid line" is prose).
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let value = rest.strip_prefix(' ').unwrap_or(rest);
    Some((key.to_string(), value.to_string()))
}

fn footnote_def_name(s: &str) -> Option<String> {
    let rest = s.trim_start().strip_prefix("[^")?;
    let end = rest.find(']')?;
    let name = &rest[..end];
    if rest[end + 1..].starts_with(':') {
        Some(name.to_string())
    } else {
        None
    }
}

fn callout_begin(s: &str) -> Option<String> {
    let t = s.trim_start();
    // `get(..8)` is char-boundary-safe (returns None on a multibyte split).
    if t.get(..8)?.eq_ignore_ascii_case("#+BEGIN_") {
        Some(t[8..].split_whitespace().next().unwrap_or("").to_string())
    } else {
        None
    }
}

fn find_callout_end(lines: &[Line], from: usize, name: &str) -> Option<usize> {
    let needle = format!("#+END_{}", name);
    for (off, l) in lines[from + 1..].iter().enumerate() {
        let t = l.text.trim_start();
        if t.get(..needle.len()).is_some_and(|p| p.eq_ignore_ascii_case(&needle)) {
            return Some(from + 1 + off);
        }
    }
    None
}

/// A line that is exactly `$$ … $$` (after trimming) ⇒ block-level displayed math.
fn displayed_math(s: &str) -> Option<String> {
    let t = s.trim();
    if t.len() >= 4 {
        t.strip_prefix("$$")?.strip_suffix("$$").map(str::to_string)
    } else {
        None
    }
}

/// `:NAME:` (alone on a line, NAME != END) ⇒ opens a drawer.
fn drawer_begin(s: &str) -> Option<String> {
    let inner = s.trim().strip_prefix(':')?.strip_suffix(':')?;
    if inner.is_empty() || inner.eq_ignore_ascii_case("END") {
        return None;
    }
    if inner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        Some(inner.to_ascii_lowercase())
    } else {
        None
    }
}

fn find_drawer_end(lines: &[Line], from: usize) -> Option<usize> {
    lines[from + 1..]
        .iter()
        .position(|l| l.text.trim().eq_ignore_ascii_case(":END:"))
        .map(|off| from + 1 + off)
}

fn is_raw_html(s: &str) -> bool {
    // `<tag …>` / `</tag>` — a real HTML tag name, NOT an autolink like
    // `<https://…>` (whose ':' breaks the tag-name rule → stays inline/paragraph).
    let t = s.trim_start();
    let b = t.as_bytes();
    if b.len() < 2 || b[0] != b'<' {
        return false;
    }
    let mut k = 1;
    if b[k] == b'/' {
        k += 1;
    }
    let name_start = k;
    while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'-') {
        k += 1;
    }
    if k == name_start || !b[name_start].is_ascii_alphabetic() {
        return false;
    }
    k < b.len() && matches!(b[k], b'>' | b'/' | b' ' | b'\t')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Block;

    fn kinds(input: &str) -> Vec<&'static str> {
        parse(input).iter().map(|b| match b {
            Block::Paragraph { .. } => "paragraph",
            Block::Heading { .. } => "heading",
            Block::Bullet { .. } => "bullet",
            Block::List { .. } => "list",
            Block::Src { .. } => "src",
            Block::Quote { .. } => "quote",
            Block::Custom { .. } => "custom",
            Block::Properties { .. } => "properties",
            Block::Hr { .. } => "hr",
            Block::Table { .. } => "table",
            Block::FootnoteDef { .. } => "footnote_def",
            Block::RawHtml { .. } => "raw_html",
            Block::DisplayedMath { .. } => "displayed_math",
            Block::Drawer { .. } => "drawer",
        }).collect()
    }

    #[test]
    fn block_kinds() {
        assert_eq!(kinds("# h"), ["heading"]);
        assert_eq!(kinds("#nospace"), ["paragraph"]);
        assert_eq!(kinds("#"), ["heading"]); // bare hashes are a heading
        assert_eq!(kinds("- a\n- b"), ["bullet", "bullet"]);
        assert_eq!(kinds("* a\n+ b"), ["list"]); // *,+ → one List
        assert_eq!(kinds("1. a\n2. b"), ["list"]);
        assert_eq!(kinds("1) a"), ["paragraph"]); // N) is not a list
        assert_eq!(kinds("```js\nx\n```"), ["src"]);
        assert_eq!(kinds("```\nunclosed"), ["paragraph"]); // unclosed fence
        assert_eq!(kinds("key:: v\nk2:: w"), ["properties"]);
        assert_eq!(kinds("a::b mid"), ["paragraph"]); // needs space after ::
        assert_eq!(kinds("> q\n  > more"), ["quote"]); // indented continuation
        assert_eq!(kinds("---"), ["hr"]);
        assert_eq!(kinds("| a | b |\n|-|-|\n| 1 | 2 |"), ["table"]);
        assert_eq!(kinds("[^1]: note"), ["footnote_def"]);
        assert_eq!(kinds(":LOGBOOK:\nx\n:END:"), ["drawer"]);
        assert_eq!(kinds("$$x$$"), ["displayed_math"]);
        assert_eq!(kinds("<div>x</div>"), ["raw_html"]);
        assert_eq!(kinds("<https://x.com>"), ["paragraph"]); // autolink, not html
        assert_eq!(kinds("a\nb\n\nc"), ["paragraph"]); // text coalesces across blanks
        assert_eq!(kinds(""), Vec::<&str>::new());
    }

    #[test]
    fn heading_size_and_bullet_level() {
        match &parse("### h")[0] {
            Block::Heading { level, size, .. } => { assert_eq!(*level, 1); assert_eq!(*size, Some(3)); }
            _ => panic!(),
        }
        match &parse("  - x")[0] {
            Block::Bullet { level, .. } => assert_eq!(*level, 3), // 1 + 2 leading spaces
            _ => panic!(),
        }
    }

    #[test]
    fn spans_tile_contiguously() {
        // Spans aren't oracle-checked, so verify them here: each top-level block's
        // span is contiguous and covers the whole input.
        let input = "# Title\n- a\n- b";
        let blocks = parse(input);
        let mut prev_end = 0;
        for b in &blocks {
            let span = block_span(b).expect("top-level block has a span");
            assert_eq!(span.0, prev_end, "spans must be contiguous");
            prev_end = span.1;
        }
        assert_eq!(prev_end, input.len(), "spans cover the whole input");
    }

    fn block_span(b: &Block) -> Option<Span> {
        match b {
            Block::Paragraph { span, .. } | Block::Heading { span, .. }
            | Block::Bullet { span, .. } | Block::List { span, .. }
            | Block::Src { span, .. } | Block::Quote { span, .. }
            | Block::Custom { span, .. } | Block::Properties { span, .. }
            | Block::Hr { span, .. } | Block::Table { span, .. }
            | Block::FootnoteDef { span, .. } | Block::RawHtml { span, .. }
            | Block::DisplayedMath { span, .. } | Block::Drawer { span, .. } => *span,
        }
    }

    #[test]
    fn unicode_does_not_panic() {
        // Real content has multibyte chars; byte-slicing must stay on boundaries.
        for s in ["#+BEGIN_QUOTE\ncafé 中文 😀\n#+END_QUOTE", "café", "中文 #tag", "😀 [[page]]"] {
            let _ = parse(s);
        }
    }
}

fn build_table(rows: &[Line], start: usize, end: usize) -> Block {
    let split_cells = |s: &str| -> Vec<Vec<Inline>> {
        let t = s.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        t.split('|').map(|c| stub_inline(c.trim())).collect()
    };
    let is_sep = |s: &str| -> bool {
        let t = s.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        !t.is_empty()
            && t.split('|').all(|c| {
                let c = c.trim();
                !c.is_empty() && c.bytes().all(|b| b == b'-' || b == b':')
            })
    };

    let header = rows.first().map(|l| split_cells(l.text));
    let mut data_start = 1;
    if rows.len() > 1 && is_sep(rows[1].text) {
        data_start = 2;
    }
    let body: Vec<Vec<Vec<Inline>>> =
        rows[data_start.min(rows.len())..].iter().map(|l| split_cells(l.text)).collect();

    Block::Table {
        header,
        rows: body,
        span: Some(Span(start, end)),
    }
}

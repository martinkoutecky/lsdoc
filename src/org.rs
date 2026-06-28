//! Org-mode parser (M6).
//!
//! A from-scratch Org parser, behavior-equivalent to mldoc 1.5.7's Org config
//! (`format:"Org"`), verified against the live oracle. Two phases like the Markdown
//! side: a line-based block segmenter (`parse`) and a single-pass inline scanner
//! (`OrgScanner`). Shared, format-agnostic helpers (timestamps, autolink/email/html,
//! nested links, macros, bare urls, page-ref/tag scanning) are reused from
//! `crate::inline`; Org-specific grammar (emphasis, verbatim/code, sub/superscript,
//! `[[…]]`/`[[…][…]]` links, plain-run delimiters) lives here.
//!
//! Key Org-vs-Markdown differences (all probed against mldoc, see DECISIONS.md):
//! - Headlines `*{n} ` → `Bullet{level:n}` with marker/priority/`:tags:`; a `*` line
//!   inside `#+BEGIN_SRC` is code, not a headline.
//! - Emphasis: `*`Bold `/`Italic `_`Underline `+`Strike `~`Code `=`Verbatim `^^`Highlight.
//!   `/`,`_`,`+` carry a backward (char-before-opener ∈ punct/ws) AND forward
//!   (char-after-closer ∈ punct/ws/eoi) delimiter gate; `*`,`^^` carry neither. So
//!   `2*3*4`→Bold, `a/b/c`→literal, `snake_case_var`→Subscript.
//! - `_x`/`_{x}`→Subscript, `^x`/`^{x}`→Superscript.
//! - Plain runs stop only at `\ _ ^ [ * / + $ #` + whitespace (NOT `~ = ( < { ! @`),
//!   so verbatim/code/block-refs/autolinks/macros only fire at a run boundary.
//! - Org does NOT unescape values (backslashes are kept literal).

use crate::inline::{
    char_len, find_sub, find_sub_line, is_underscore_delim, is_ws, is_ws_or_nl, parse_angle_timestamp,
    parse_bare_url, parse_bracket_date, parse_email_autolink, parse_inline_html,
    parse_keyword_timestamp, parse_macro, parse_nested_link,
};
use crate::projection::{Block, Inline, ListItem, Span, Url};

// ===========================================================================
// Block segmentation
// ===========================================================================

struct Line<'a> {
    start: usize,
    end: usize, // just past the trailing '\n' (or EOF)
    text: &'a str,
}

/// Org task markers (mldoc `Heading0.marker`), stripped from a headline title.
const MARKERS: &[&str] = &[
    "TODO",
    "DOING",
    "WAITING",
    "WAIT",
    "DONE",
    "CANCELED",
    "CANCELLED",
    "STARTED",
    "IN-PROGRESS",
    "NOW",
    "LATER",
];

pub fn parse(input: &str) -> Vec<Block> {
    let lines = split_lines(input);
    let fences = pair_fences(&lines);
    let mut out: Vec<Block> = Vec::new();
    let mut para: Option<(usize, usize)> = None;
    // After an "absorbing" block (Directive/Comment/Block/List/Footnote) mldoc's
    // `<* optional eols` swallows the following blank lines; Heading/Table/Drawer do
    // not, so a blank line there becomes a (leading-Break) paragraph.
    let mut absorb = false;
    let mut i = 0;

    while i < lines.len() {
        let line = &lines[i];
        let t = line.text;

        // blank line: extend an open paragraph, else swallow (if absorbing) or start one.
        if t.trim().is_empty() {
            if let Some((s, _)) = para {
                para = Some((s, line.end));
            } else if absorb {
                // swallowed by the preceding block.
            } else {
                para = Some((line.start, line.end));
            }
            i += 1;
            continue;
        }

        // 1. directive `#+KEY: value` (KEY != BEGIN_…)
        if let Some((name, value)) = directive(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::Directive { name, value, span: Some(Span(line.start, line.end)) });
            absorb = true;
            i += 1;
            continue;
        }

        // 2. drawer `:PROPERTIES:`/`:NAME:` … `:END:`
        if let Some(name) = drawer_begin(t) {
            if let Some(close) = find_drawer_end(&lines, i) {
                flush_para(&mut out, &mut para, input);
                if name == "properties" {
                    let mut props: Vec<(String, String)> = lines[i + 1..close]
                        .iter()
                        .filter_map(|l| drawer_property(l.text))
                        .collect();
                    // mldoc `Drawer.parse` is `many1 (parse1 <|> parse2)`: a run of
                    // `#+NAME: value` directives immediately following the drawer folds
                    // into the same Property_Drawer (parse2 absorbs trailing eols).
                    let mut j = close + 1;
                    let mut folded = false;
                    while j < lines.len() {
                        if let Some(kv) = directive(lines[j].text) {
                            props.push(kv);
                            folded = true;
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    let end = lines[j - 1].end;
                    out.push(Block::Properties { props, span: Some(Span(line.start, end)) });
                    absorb = folded;
                    i = j;
                    continue;
                }
                out.push(Block::Drawer { name, span: Some(Span(line.start, lines[close].end)) });
                absorb = false;
                i = close + 1;
                continue;
            }
        }

        // 3. headline `*{n} `
        if let Some(level) = headline_level(t) {
            flush_para(&mut out, &mut para, input);
            let (marker, priority, inline, htags) = headline_parts(t);
            let empty_title = inline.is_empty() && htags.is_empty();
            out.push(Block::Bullet {
                level,
                inline,
                marker,
                priority,
                htags,
                span: Some(Span(line.start, line.end)),
            });
            absorb = false;
            // mldoc quirk: an EMPTY-title headline that still has trailing whitespace
            // (`*** `, `* TODO `) emits the empty bullet, then the leftover whitespace
            // begins a fresh paragraph that absorbs the following lines (`* \nx` →
            // Bullet + Paragraph[" ", Break, "x"]). A *block-construct* remainder
            // (`* :x`/`* #+K:v`/`* | a |`) is left as documented adversarial noise.
            if empty_title {
                let content_len = t.trim_end_matches([' ', '\t']).len();
                if content_len < t.len() {
                    para = Some((line.start + content_len, line.end));
                }
            }
            i += 1;
            continue;
        }

        // 4. table (group of consecutive well-formed `|…|` rows)
        if is_table_row(t) {
            flush_para(&mut out, &mut para, input);
            let start = i;
            while i < lines.len() && is_table_row(lines[i].text) {
                i += 1;
            }
            out.push(build_table(&lines[start..i], lines[start].start, lines[i - 1].end));
            absorb = false;
            continue;
        }

        // 4b. LaTeX environment `\begin{X} … \end{X}` (mldoc Latex_env, before Block).
        let line_content_end = line.start + t.len();
        if let Some((name, content, consumed_end)) =
            crate::inline::parse_latex_env(input, line.start, line_content_end)
        {
            flush_para(&mut out, &mut para, input);
            out.push(Block::LatexEnv { name, content, span: Some(Span(line.start, consumed_end)) });
            absorb = false;
            let mut ni = i + 1;
            while ni < lines.len() && lines[ni].start < consumed_end {
                ni += 1;
            }
            i = ni;
            continue;
        }

        // 5. fenced code block (```/~~~) — markdown fences work in Org too.
        if let Some((close, lang)) = fences.get(&i) {
            flush_para(&mut out, &mut para, input);
            let code = if *close > i + 1 {
                input[lines[i + 1].start..lines[*close - 1].end].to_string()
            } else {
                String::new()
            };
            out.push(Block::Src { lang: lang.clone(), code, span: Some(Span(line.start, lines[*close].end)) });
            absorb = true;
            i = *close + 1;
            continue;
        }

        // 6. `#+BEGIN_X` … `#+END_X` block
        if let Some(name) = block_begin(t) {
            if let Some(close) = find_block_end(&lines, i, &name) {
                flush_para(&mut out, &mut para, input);
                let inner = block_code(&lines[i + 1..close]);
                let span = Some(Span(line.start, lines[close].end));
                let lname = name.to_ascii_lowercase();
                match lname.as_str() {
                    "src" => {
                        let lang = begin_lang(t);
                        out.push(Block::Src { lang, code: inner, span });
                    }
                    "example" => out.push(Block::Example { code: inner, span }),
                    "quote" => out.push(Block::Quote { children: parse(&inner), span }),
                    _ => out.push(Block::Custom { name: lname, children: parse(&inner), span }),
                }
                absorb = true;
                i = close + 1;
                continue;
            }
        }

        // 7. verbatim block (Org): consecutive lines starting with `:` → Example.
        if is_verbatim_line(t) {
            flush_para(&mut out, &mut para, input);
            let start = i;
            let mut code = String::new();
            while i < lines.len() && is_verbatim_line(lines[i].text) {
                code.push_str(verbatim_content(lines[i].text));
                code.push('\n');
                i += 1;
            }
            out.push(Block::Example { code, span: Some(Span(lines[start].start, lines[i - 1].end)) });
            absorb = true;
            continue;
        }

        // 8. markdown blockquote (`>` …) — also recognised in Org.
        if quote_opens(t) {
            flush_para(&mut out, &mut para, input);
            let start = i;
            let mut body = String::new();
            if let Some(c) = quote_line_content(lines[i].text) {
                body.push_str(&c);
                body.push('\n');
            }
            i += 1;
            while i < lines.len() {
                match quote_line_content(lines[i].text) {
                    Some(c) => {
                        body.push_str(&c);
                        body.push('\n');
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push(Block::Quote {
                children: parse(&body),
                span: Some(Span(lines[start].start, lines[i - 1].end)),
            });
            absorb = true;
            continue;
        }

        // 9. block-level displayed math `$$ … $$`.
        if let Some(math) = displayed_math(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::DisplayedMath { text: math, span: Some(Span(line.start, line.end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 10. raw HTML (single line, complete element).
        if is_raw_html(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::RawHtml { text: t.to_string(), span: Some(Span(line.start, line.end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 11. footnote definition `[fn:name] text`.
        if let Some((name, content)) = footnote_def(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::FootnoteDef {
                name,
                inline: parse_inline_org_top(content),
                span: Some(Span(line.start, line.end)),
            });
            absorb = true;
            i += 1;
            continue;
        }

        // 12. list (group of consecutive `- `/`+ `/`N. ` items at indent 0)
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
                items: crate::projection::nest_items(items),
                span: Some(Span(lines[start].start, lines[i - 1].end)),
            });
            absorb = true;
            continue;
        }

        // 13. horizontal rule (exactly 5 dashes).
        if is_org_hr(t) {
            flush_para(&mut out, &mut para, input);
            out.push(Block::Hr { span: Some(Span(line.start, line.end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 14. plain line → accumulate into the current paragraph.
        para = Some(match para {
            Some((s, _)) => (s, line.end),
            None => (line.start, line.end),
        });
        absorb = false;
        i += 1;
    }

    flush_para(&mut out, &mut para, input);
    out
}

fn flush_para(out: &mut Vec<Block>, para: &mut Option<(usize, usize)>, input: &str) {
    if let Some((s, e)) = para.take() {
        out.push(Block::Paragraph {
            inline: parse_inline_org_top(&input[s..e]),
            span: Some(Span(s, e)),
        });
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

fn leading_ws(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

// ---- directive ------------------------------------------------------------

/// `#+KEY: value` where KEY is non-empty and not `BEGIN_…`. Returns (key, value).
/// Leading whitespace is allowed (mldoc: `  #+KEY: v` is a directive). The value is
/// **left-trimmed only** — mldoc keeps trailing whitespace (`#+TITLE: x  ` → `x  `).
fn directive(s: &str) -> Option<(String, String)> {
    let rest = s.trim_start().strip_prefix("#+")?;
    let pos = rest.find(':')?;
    let key = &rest[..pos];
    if key.is_empty() || key.bytes().any(|b| b == b'\n' || b == b'\r') {
        return None;
    }
    if key.len() >= 6 && key[..6].eq_ignore_ascii_case("begin_") {
        return None;
    }
    let value = rest[pos + 1..].trim_start();
    Some((key.to_string(), value.to_string()))
}

// ---- drawers --------------------------------------------------------------

/// `:NAME:` alone on a line (NAME != END) → opens a drawer. Lowercased name.
fn drawer_begin(s: &str) -> Option<String> {
    let inner = s.trim().strip_prefix(':')?.strip_suffix(':')?;
    if inner.is_empty() || inner.eq_ignore_ascii_case("END") {
        return None;
    }
    if inner.bytes().any(|b| b == b':' || b == b' ' || b == b'\t') {
        return None;
    }
    Some(inner.to_ascii_lowercase())
}

fn find_drawer_end(lines: &[Line], from: usize) -> Option<usize> {
    lines[from + 1..]
        .iter()
        .position(|l| l.text.trim().eq_ignore_ascii_case(":END:"))
        .map(|off| from + 1 + off)
}

/// One `:key: value` line of a `:PROPERTIES:` drawer (mldoc drawer.ml `property`).
fn drawer_property(s: &str) -> Option<(String, String)> {
    let t = s.trim_start().strip_prefix(':')?;
    let pos = t.find(':')?;
    let key = &t[..pos];
    if key.is_empty() || key.contains(' ') || key.contains('\t') || key.eq_ignore_ascii_case("end") {
        return None;
    }
    let value = t[pos + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

// ---- headline -------------------------------------------------------------

/// `*{n}` at column 0 followed by a space/tab or end-of-line ⇒ headline level n.
fn headline_level(s: &str) -> Option<u32> {
    if !s.starts_with('*') {
        return None;
    }
    let stars = s.bytes().take_while(|&b| b == b'*').count();
    let rest = &s[stars..];
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some(stars as u32)
    } else {
        None
    }
}

/// Returns (marker, priority, title inlines, htags) for a headline line.
fn headline_parts(t: &str) -> (Option<String>, Option<String>, Vec<Inline>, Vec<String>) {
    let stars = t.bytes().take_while(|&b| b == b'*').count();
    let after = t[stars..].trim_start();
    let (marker, priority, title_text) = split_markers(after);
    let mut inline = parse_inline_org_top(title_text);
    let htags = extract_htags(&mut inline);
    (marker, priority, inline, htags)
}

/// Strip a leading task marker (followed by a space) and priority `[#X]`.
fn split_markers(s: &str) -> (Option<String>, Option<String>, &str) {
    let mut marker = None;
    let mut s = s;
    for m in MARKERS {
        if let Some(rest) = s.strip_prefix(m) {
            // mldoc accepts a marker followed by a space OR end-of-line.
            if rest.is_empty() || rest.starts_with(' ') {
                marker = Some((*m).to_string());
                s = rest.trim_start();
                break;
            }
        }
    }
    let b = s.as_bytes();
    let priority = if b.len() >= 4 && b[0] == b'[' && b[1] == b'#' && b[2] < 0x80 && b[3] == b']' {
        let p = (b[2] as char).to_string();
        s = s[4..].trim_start();
        Some(p)
    } else {
        None
    };
    (marker, priority, s)
}

/// Org headline tag extraction: if the last title inline is a `Plain` whose trimmed
/// text ends with `:` (len > 1), split off a trailing `:tag1:tag2:` run (mldoc
/// `heading0.ml`). Mutates `title` in place; returns the tag list.
fn extract_htags(title: &mut Vec<Inline>) -> Vec<String> {
    let Some(Inline::Plain { text }) = title.last() else {
        return Vec::new();
    };
    let s = text.trim().to_string();
    if s.len() <= 1 || !s.ends_with(':') {
        return Vec::new();
    }
    // splitr at the last space: prefix includes the trailing space, suffix = last run.
    let (prefix, maybe_tags): (String, &str) = match s.rfind(' ') {
        Some(p) => (s[..p + 1].to_string(), &s[p + 1..]),
        None => (String::new(), s.as_str()),
    };
    let Some(tags) = parse_org_tags(maybe_tags) else {
        return Vec::new();
    };
    // title2 = drop_last 1 title (then append [Plain prefix] if prefix != "")
    title.pop();
    if !prefix.is_empty() {
        title.push(Inline::Plain { text: prefix });
    }
    // last_plain: if the (new) last inline is Plain, rtrim it and add one trailing space.
    if let Some(Inline::Plain { text }) = title.last_mut() {
        let trimmed = text.trim_end();
        *text = format!("{} ", trimmed);
    }
    tags
}

/// `:a:b:` → ["a","b"]; None if not a valid `:`-wrapped tag run. Empty tokens are
/// dropped (mldoc `remove is_blank`); any token containing a space invalidates it.
fn parse_org_tags(s: &str) -> Option<Vec<String>> {
    if s.len() < 2 || !s.starts_with(':') || !s.ends_with(':') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut out = Vec::new();
    for tok in inner.split(':') {
        if tok.is_empty() {
            continue; // dropped as blank
        }
        if tok.bytes().any(|b| b == b' ' || b == b'\t') {
            return None;
        }
        out.push(tok.to_string());
    }
    if out.is_empty() { None } else { Some(out) }
}

// ---- blocks (#+BEGIN_X / fences / verbatim / quote / math / html) ---------

fn block_begin(s: &str) -> Option<String> {
    let t = s.trim_start();
    if t.get(..8)?.eq_ignore_ascii_case("#+BEGIN_") {
        Some(t[8..].split_whitespace().next().unwrap_or("").to_string())
    } else {
        None
    }
}

fn find_block_end(lines: &[Line], from: usize, name: &str) -> Option<usize> {
    let needle = format!("#+END_{}", name);
    for (off, l) in lines[from + 1..].iter().enumerate() {
        let t = l.text.trim_start();
        if t.get(..needle.len()).is_some_and(|p| p.eq_ignore_ascii_case(&needle)) {
            return Some(from + 1 + off);
        }
    }
    None
}

/// Language token from a `#+BEGIN_SRC <lang> …` line (first whitespace word).
fn begin_lang(s: &str) -> String {
    let t = s.trim_start();
    t[8..].split_whitespace().nth(1).unwrap_or("").to_string()
}

/// Inner code/content of a `#+BEGIN_X … #+END_X` block, joined with one `\n` per
/// line plus a trailing `\n`, with the common indent (the first line's leading
/// whitespace) stripped from each line (mldoc `block0.ml` "clear indents").
fn block_code(inner: &[Line]) -> String {
    if inner.is_empty() {
        return String::new();
    }
    let indent = leading_ws(inner[0].text);
    let mut out = String::new();
    for l in inner {
        let t = l.text;
        let lw = leading_ws(t);
        let cleared = if lw >= indent {
            &t[indent..] // leading ws are ASCII (space/tab) ⇒ byte-safe
        } else if t.trim().is_empty() {
            t
        } else {
            t.trim_start()
        };
        out.push_str(cleared);
        out.push('\n');
    }
    out
}

/// A code-fence marker line: 3+ `` ` `` or `~` after optional leading whitespace.
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
    if k - ws >= 3 { Some((c, k)) } else { None }
}

fn pair_fences(lines: &[Line]) -> std::collections::HashMap<usize, (usize, String)> {
    let mut out = std::collections::HashMap::new();
    let mut open: Option<(usize, u8)> = None;
    for (idx, l) in lines.iter().enumerate() {
        if let Some((c, _)) = fence_marker(l.text) {
            match open {
                None => open = Some((idx, c)),
                Some((oidx, oc)) => {
                    if c == oc {
                        let (_, mend) = fence_marker(lines[oidx].text).unwrap();
                        let lang = lines[oidx].text[mend..].trim().to_string();
                        out.insert(oidx, (idx, lang));
                        open = None;
                    }
                }
            }
        }
    }
    out
}

/// A line that is part of an Org fixed-width block: starts (after optional ws) with a
/// `:`. mldoc maps ANY `:`-prefixed line that is NOT part of a recognized
/// `:NAME: … :END:` drawer (tried first in `parse`) to a verbatim `Example` — incl.
/// `: text`, `:text`, `:key: value`, `:tag1:tag2:`, a bare `:END:`/`:PROPERTIES:`.
fn is_verbatim_line(s: &str) -> bool {
    s[leading_ws(s)..].starts_with(':')
}

/// Fixed-width line content (mldoc): drop the leading ws, the `:`, then any following
/// ASCII space/tab (`:    x` → `x`); trailing/internal ws kept (`: a b  ` → `a b  `).
fn verbatim_content(s: &str) -> &str {
    let t = &s[leading_ws(s)..];
    let rest = t.strip_prefix(':').unwrap_or(t);
    &rest[leading_ws(rest)..]
}

fn quote_opens(s: &str) -> bool {
    match s.trim_start().strip_prefix('>') {
        Some(rest) => !rest.trim().is_empty(),
        None => false,
    }
}

fn quote_line_content(s: &str) -> Option<String> {
    let t = s.trim_start();
    let had_gt = t.starts_with('>');
    let rest = if had_gt { t[1..].trim_start() } else { t };
    if rest.is_empty() {
        return if had_gt { Some(String::new()) } else { None };
    }
    if rest.starts_with("- ")
        || rest.starts_with("# ")
        || rest.starts_with("id:: ")
        || rest == "-"
        || rest == "#"
    {
        return None;
    }
    Some(rest.to_string())
}

fn displayed_math(s: &str) -> Option<String> {
    let t = s.trim();
    if t.len() >= 4 {
        t.strip_prefix("$$")?.strip_suffix("$$").map(str::to_string)
    } else {
        None
    }
}

fn is_raw_html(s: &str) -> bool {
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
    if !matches!(b.get(k), Some(b'>' | b'/' | b' ' | b'\t')) {
        return false;
    }
    t.contains("</")
}

/// Org footnote definition `[fn:name] text`. Returns (name, content). Leading
/// whitespace is allowed (mldoc). mldoc requires a **non-empty body whose first char
/// does not begin a block construct** (`* # [ -`): `[fn:1] text`/`[fn:1]:x` →
/// Footnote_Definition, but a bare `[fn:1]` (or `[fn:1]*x`/`[fn:1]-x`/`[fn:1]#x`/
/// `[fn:1][x`) is an inline footnote ref inside a Paragraph.
fn footnote_def(s: &str) -> Option<(String, &str)> {
    let rest = s.trim_start().strip_prefix("[fn:")?;
    let end = rest.find(']')?;
    let name = &rest[..end];
    if name.is_empty() || name.contains('\n') || name.contains('\r') {
        return None;
    }
    let content = rest[end + 1..].trim_start();
    let first = content.bytes().next()?;
    if matches!(first, b'*' | b'#' | b'[' | b'-') {
        return None;
    }
    Some((name.to_string(), content))
}

/// Org list item at indent 0: `- `/`+ ` (unordered) or `N. ` (ordered). A `* `
/// at column 0 is a headline (handled earlier), so only `-`/`+`/`N.` here.
fn list_item(s: &str) -> Option<ListItem> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    let mk_item = |ordered, number, content: &str| {
        let (checkbox, body) = split_checkbox(content);
        ListItem {
            ordered,
            number,
            indent: ws as u32,
            content: vec![Block::Paragraph {
                inline: parse_inline_org_top(body),
                span: None,
            }],
            items: vec![],
            name: vec![],
            checkbox,
        }
    };
    // mldoc requires non-empty content after the marker (and after any checkbox): a
    // bare `- `/`+ `/`1. ` (or `- [ ]`) is a Paragraph, only `- x` is a List.
    // Marker quirks (mldoc lists.ml): `-` is a bullet ONLY at column 0 (an indented
    // `  - x` is a Paragraph); `*` is the OPPOSITE — a list item ONLY when indented
    // (a column-0 `* x` is a headline, handled earlier); `+`/`1.` are lists at any
    // indent. So: dash at col 0, star when indented, plus `+`.
    let dash = if ws == 0 { rest.strip_prefix('-') } else { None };
    let star = if ws > 0 { rest.strip_prefix('*') } else { None };
    if let Some(after) = dash.or(star).or_else(|| rest.strip_prefix('+')) {
        if after.starts_with(' ') || after.starts_with('\t') {
            let content = after.trim_start();
            if split_checkbox(content).1.trim().is_empty() {
                return None;
            }
            return Some(mk_item(false, None, content));
        }
    }
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        if let Some(after) = rest[digits..].strip_prefix('.') {
            if after.starts_with(' ') || after.starts_with('\t') {
                if let Ok(number) = rest[..digits].parse::<u32>() {
                    let content = after.trim_start();
                    if split_checkbox(content).1.trim().is_empty() {
                        return None;
                    }
                    return Some(mk_item(true, Some(number), content));
                }
            }
        }
    }
    None
}

/// Split a leading list checkbox `[ ]`/`[x]`/`[X]` (+ following spaces) off `s`,
/// returning (state, rest). See `parse::split_checkbox` (md sibling).
fn split_checkbox(s: &str) -> (Option<bool>, &str) {
    if let Some(r) = s.strip_prefix("[ ]") {
        (Some(false), r.trim_start())
    } else if let Some(r) = s.strip_prefix("[x]").or_else(|| s.strip_prefix("[X]")) {
        (Some(true), r.trim_start())
    } else {
        (None, s)
    }
}

/// Org horizontal rule: exactly 5 `-` (optionally surrounded by whitespace).
fn is_org_hr(s: &str) -> bool {
    s.trim() == "-----"
}

// ---- table ----------------------------------------------------------------

/// An Org table row: the trimmed line both starts AND ends with `|` and is at least 2
/// bytes (`||`/`| a |`/`|---+---|` are rows; `|`, `|a`, `| a | b` are not — mldoc
/// makes those Paragraphs and breaks the table group at the first non-row line).
fn is_table_row(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 2 && t.starts_with('|') && t.ends_with('|')
}

fn build_table(rows: &[Line], start: usize, end: usize) -> Block {
    let split_cells = |s: &str| -> Vec<Vec<Inline>> {
        let t = s.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        t.split('|').map(|c| parse_inline_org_top(c.trim())).collect()
    };
    // Org separator line: between the outer pipes only `-`, `+`, `|`, `:`, space.
    let is_sep = |s: &str| -> bool {
        let t = s.trim();
        let inner = t.strip_prefix('|').unwrap_or(t);
        !inner.is_empty()
            && inner
                .bytes()
                .all(|b| matches!(b, b'-' | b'+' | b'|' | b':' | b' '))
    };

    let header = rows.first().map(|l| split_cells(l.text));
    // data rows = all non-separator rows after the first.
    let body: Vec<Vec<Vec<Inline>>> = rows[1.min(rows.len())..]
        .iter()
        .filter(|l| !is_sep(l.text))
        .map(|l| split_cells(l.text))
        .collect();

    Block::Table { header, rows: body, span: Some(Span(start, end)) }
}

// ===========================================================================
// Inline parsing
// ===========================================================================

#[derive(Clone, Copy)]
struct Ctx {
    /// Backward delim gate for `_`/`/`/`+` is active only with state (top level);
    /// inside an emphasis re-parse mldoc calls `emphasis` without state.
    use_state: bool,
    tags: bool,
    block_refs: bool,
    macros: bool,
    latex: bool,
    urls: bool,
    timestamps: bool,
    angle: bool,
    code: bool,
    breaks: bool,
    entity: bool,
    footnotes: bool,
    scripts: bool,
    links: bool,
}

impl Ctx {
    fn top() -> Ctx {
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
        }
    }
    /// Emphasis content / link-label re-parse (mldoc `nested_emphasis`): emphasis,
    /// sub/superscript, links and plain; no state ⇒ backward gate always passes.
    fn emph() -> Ctx {
        Ctx {
            use_state: false,
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
            scripts: true,
            links: true,
        }
    }
    /// `[[url][label]]` label re-parse (mldoc `org_link_1`): emphasis, latex, entity,
    /// code, sub/superscript, plain — NO links, NO tags (so `[[x]]` in a label stays
    /// literal).
    fn label() -> Ctx {
        Ctx {
            use_state: false,
            tags: false,
            block_refs: false,
            macros: false,
            latex: true,
            urls: false,
            timestamps: false,
            angle: false,
            code: true,
            breaks: false,
            entity: true,
            footnotes: false,
            scripts: true,
            links: false,
        }
    }
    /// Sub/superscript content (mldoc `gen_script`): emphasis, plain, ws, entity —
    /// NO nested sub/superscript, NO links.
    fn script() -> Ctx {
        Ctx {
            use_state: false,
            tags: false,
            block_refs: false,
            macros: false,
            latex: false,
            urls: false,
            timestamps: false,
            angle: false,
            code: false,
            breaks: false,
            entity: true,
            footnotes: false,
            scripts: false,
            links: false,
        }
    }
}

pub fn parse_inline_org_top(text: &str) -> Vec<Inline> {
    parse_inline_org(text, Ctx::top())
}

fn parse_inline_org(text: &str, ctx: Ctx) -> Vec<Inline> {
    let mut sc = OrgScanner::new(text, ctx);
    sc.run();
    sc.finish()
}

struct OrgScanner<'a> {
    s: &'a str,
    b: &'a [u8],
    n: usize,
    i: usize,
    ctx: Ctx,
    out: Vec<Inline>,
    pending: String,
    /// mldoc `state.last_plain_char`: last char of the most recent Plain run, used by
    /// the `_`/`/`/`+` backward delimiter gate. Updated only on plain emission.
    last_plain_char: Option<u8>,
    no_closer: std::collections::HashMap<(u8, usize), bool>,
    absent: std::collections::HashSet<[u8; 2]>,
    /// Once `]` is absent from a position it is absent from every later one (the scan
    /// window only shrinks) — keeps `[[[[…`-style runs linear (no bracket construct
    /// can match without a `]`).
    rbracket_absent: bool,
}

impl<'a> OrgScanner<'a> {
    fn new(s: &'a str, ctx: Ctx) -> OrgScanner<'a> {
        OrgScanner {
            s,
            b: s.as_bytes(),
            n: s.len(),
            i: 0,
            ctx,
            out: Vec::new(),
            pending: String::new(),
            last_plain_char: None,
            no_closer: std::collections::HashMap::new(),
            absent: std::collections::HashSet::new(),
            rbracket_absent: false,
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

    /// Append plain text and remember its last byte (mldoc `set_last_char`).
    fn push_plain(&mut self, seg: &str) {
        if let Some(&last) = seg.as_bytes().last() {
            self.last_plain_char = Some(last);
        }
        self.pending.push_str(seg);
    }

    /// Is there any `]` at/after `self.i`? Caches absence (monotone).
    fn has_rbracket(&mut self) -> bool {
        if self.rbracket_absent {
            return false;
        }
        if self.b[self.i..].contains(&b']') {
            true
        } else {
            self.rbracket_absent = true;
            false
        }
    }

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

    fn run(&mut self) {
        while self.i < self.n {
            let start = self.i;
            self.step();
            if self.i == start {
                let w = char_len(self.b[self.i]);
                let seg = self.s[self.i..self.i + w].to_string();
                self.push_plain(&seg);
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
            b'*' | b'/' | b'+' => {
                if !self.try_emphasis(c) {
                    self.plain_one();
                }
            }
            b'_' => {
                if !self.try_emphasis(b'_') && !(self.ctx.scripts && self.try_script(b'_')) {
                    self.plain_one();
                }
            }
            b'^' => {
                if !self.try_emphasis(b'^') && !(self.ctx.scripts && self.try_script(b'^')) {
                    self.plain_one();
                }
            }
            b'\\' => self.backslash(),
            b'$' if self.ctx.latex => {
                if !self.try_latex_dollar() {
                    self.plain_one();
                }
            }
            b'[' if self.ctx.links => {
                if !self.try_bracket() {
                    self.push_plain("[");
                    self.i += 1;
                }
            }
            b'[' => {
                // links disabled (sub/superscript content): `[` is a plain delimiter.
                self.push_plain("[");
                self.i += 1;
            }
            b'=' if self.ctx.code => {
                if !self.try_verbatim() {
                    self.plain_run();
                }
            }
            b'~' if self.ctx.code => {
                if !self.try_code() {
                    self.plain_run();
                }
            }
            b'<' if self.ctx.angle => {
                if !self.try_target() && !self.try_angle() {
                    self.plain_run();
                }
            }
            b'{' if self.ctx.macros => {
                if !self.try_macro() {
                    self.plain_run();
                }
            }
            b'(' if self.ctx.block_refs => {
                if !self.try_block_ref() {
                    self.plain_run();
                }
            }
            _ => {
                if self.ctx.timestamps && matches!(c, b'S' | b'C' | b'D' | b's' | b'c' | b'd') {
                    if let Some((end, node)) = parse_keyword_timestamp(self.s, self.i) {
                        self.push(node);
                        self.i = end;
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

    /// Emit a single literal char (a failed marker delimiter), advancing by one char.
    fn plain_one(&mut self) {
        let w = char_len(self.b[self.i]);
        let seg = self.s[self.i..self.i + w].to_string();
        self.push_plain(&seg);
        self.i += w;
    }

    /// Greedy plain run: ordinary bytes until an Org plain-delim, whitespace or eol.
    fn plain_run(&mut self) {
        let start = self.i;
        self.i += char_len(self.b[self.i]);
        while self.i < self.n {
            let c = self.b[self.i];
            if is_ws_or_nl(c) || is_org_marker_delim(c) {
                break;
            }
            self.i += char_len(c);
        }
        let seg = self.s[start..self.i].to_string();
        self.push_plain(&seg);
    }

    fn whitespace(&mut self) {
        let start = self.i;
        while self.i < self.n && is_ws(self.b[self.i]) {
            self.i += 1;
        }
        let seg = self.s[start..self.i].to_string();
        self.push_plain(&seg);
    }

    // ---- backslash: hard break / latex / entity / escape ------------------

    fn backslash(&mut self) {
        if self.ctx.entity {
            // org hard break: `\` immediately before end-of-line.
            match self.b.get(self.i + 1) {
                None => {
                    self.push_plain("\\");
                    self.i += 1;
                    return;
                }
                Some(b'\n') | Some(b'\r') => {
                    self.push(Inline::HardBreak);
                    self.i += 1;
                    return;
                }
                _ => {}
            }
            if self.ctx.latex {
                if let Some(node) = self.parse_latex_backslash() {
                    self.push(node);
                    return;
                }
            }
            // entity `\letters` (+ optional `{}`): a name in the LaTeX entity table →
            // `Entity`; otherwise the bare letters (backslash dropped). The `{}` is
            // consumed either way (same as Markdown).
            if self.b.get(self.i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
                let start = self.i + 1;
                let mut j = start;
                while j < self.n && self.b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let name = self.s[start..j].to_string();
                if self.s[j..].starts_with("{}") {
                    j += 2;
                }
                match crate::entities::find(&name) {
                    Some(e) => self.push(Inline::Entity {
                        name: e.name.to_string(),
                        latex: e.latex.to_string(),
                        latex_mathp: e.latex_mathp,
                        html: e.html.to_string(),
                        ascii: e.ascii.to_string(),
                        unicode: e.unicode.to_string(),
                    }),
                    None => self.push_plain(&name),
                }
                self.i = j;
                return;
            }
        }
        // escape: `\` + ASCII punctuation → keep BOTH chars literally (Org does not
        // unescape). Anything else → a lone backslash.
        match self.b.get(self.i + 1) {
            Some(&c) if c.is_ascii_punctuation() => {
                let w = char_len(c);
                let seg = self.s[self.i..self.i + 1 + w].to_string();
                self.push_plain(&seg);
                self.i += 1 + w;
            }
            _ => {
                self.push_plain("\\");
                self.i += 1;
            }
        }
    }

    fn parse_latex_backslash(&mut self) -> Option<Inline> {
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
        Some(Inline::Latex { mode: mode.to_string(), body })
    }

    fn try_latex_dollar(&mut self) -> bool {
        let after = match self.b.get(self.i + 1) {
            Some(&c) => c,
            None => return false,
        };
        if after == b'$' {
            let body_start = self.i + 2;
            if let Some(end) = find_sub_line(self.b, body_start, b"$$") {
                let body = self.s[body_start..end].to_string();
                self.push(Inline::Latex { mode: "Displayed".to_string(), body });
                self.i = end + 2;
                return true;
            }
            return false;
        }
        if after == b' ' {
            return false;
        }
        let body_start = self.i + 1;
        let mut j = body_start;
        while j < self.n && self.b[j] != b'$' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j >= self.n || self.b[j] != b'$' {
            return false;
        }
        if matches!(self.b[j - 1], b' ' | b'(' | b'[' | b'{') {
            return false;
        }
        let body = self.s[body_start..j].to_string();
        self.push(Inline::Latex { mode: "Inline".to_string(), body });
        self.i = j + 1;
        true
    }

    // ---- code / verbatim --------------------------------------------------

    /// Org inline code `~ … ~` (non-empty, no `~`/CR/NL inside).
    fn try_code(&mut self) -> bool {
        let start = self.i + 1;
        let mut j = start;
        while j < self.n && self.b[j] != b'~' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j > start && j < self.n && self.b[j] == b'~' {
            let body = self.s[start..j].to_string();
            self.push(Inline::Code { text: body });
            self.i = j + 1;
            true
        } else {
            false
        }
    }

    /// Org verbatim `= … =` (non-empty, no `=`/CR/NL inside).
    fn try_verbatim(&mut self) -> bool {
        let start = self.i + 1;
        let mut j = start;
        while j < self.n && self.b[j] != b'=' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j > start && j < self.n && self.b[j] == b'=' {
            let body = self.s[start..j].to_string();
            self.push(Inline::Verbatim { text: body });
            self.i = j + 1;
            true
        } else {
            false
        }
    }

    // ---- emphasis ---------------------------------------------------------

    /// Try Org emphasis at the current marker byte `c`.
    /// `*`→Bold, `/`→Italic, `+`→Strike, `_`→Underline (all single char), `^^`→Highlight.
    fn try_emphasis(&mut self, c: u8) -> bool {
        let (k, kind, fwd_gate, bwd_gate, continue_search) = match c {
            b'*' => (1, "Bold", false, false, false),
            b'/' => (1, "Italic", true, true, false),
            b'+' => (1, "Strike_through", true, true, false),
            b'_' => (1, "Underline", true, true, true),
            b'^' => (2, "Highlight", false, false, false),
            _ => return false,
        };
        if let Some(node) = self.parse_emphasis(c, k, kind, fwd_gate, bwd_gate, continue_search) {
            self.push(node);
            true
        } else {
            false
        }
    }

    fn parse_emphasis(
        &mut self,
        c: u8,
        k: usize,
        kind: &str,
        fwd_gate: bool,
        bwd_gate: bool,
        continue_search: bool,
    ) -> Option<Inline> {
        let open_start = self.i;
        let content_start = open_start + k;
        // need the full opener pattern present.
        if content_start > self.n || self.b[open_start..content_start].iter().any(|&x| x != c) {
            return None;
        }
        // left-flanking: opener followed by non-whitespace.
        let after = *self.b.get(content_start)?;
        if is_ws_or_nl(after) {
            return None;
        }
        // empty content: the next k bytes are themselves the closing pattern.
        if content_start + k <= self.n && self.b[content_start..content_start + k].iter().all(|&x| x == c) {
            return None;
        }
        // backward gate (top level only): char before opener ∈ punct/whitespace.
        if bwd_gate && self.ctx.use_state {
            let ok = match self.last_plain_char {
                Some(ch) => is_underscore_delim(ch),
                None => true,
            };
            if !ok {
                return None;
            }
        }
        let key = (c, k);
        if *self.no_closer.get(&key).unwrap_or(&false) {
            return None;
        }
        let closer = match self.find_closer(c, k, content_start, fwd_gate, continue_search) {
            Some(q) => q,
            None => {
                self.no_closer.insert(key, true);
                return None;
            }
        };
        let content = self.s[content_start..closer].to_string();
        self.i = closer + k;
        let children = parse_inline_org(&content, Ctx::emph());
        Some(Inline::Emphasis { emph: kind.to_string(), children })
    }

    /// Find the closing pattern. A candidate is a run (len ≥ k) of `c` whose preceding
    /// byte is non-whitespace (right-flanking); backslash-escaped chars are skipped.
    /// With `fwd_gate`, the byte after the closer must be a punct/ws delim or eoi; if
    /// it isn't, `continue_search` (true for `_`) skips to the next candidate, else
    /// (`/`,`+`) the whole emphasis fails.
    fn find_closer(&self, c: u8, k: usize, from: usize, fwd_gate: bool, continue_search: bool) -> Option<usize> {
        let mut j = from;
        while j < self.n {
            let cur = self.b[j];
            if cur == b'\\' {
                j += 1;
                if j < self.n {
                    j += char_len(self.b[j]);
                }
                continue;
            }
            if cur == c {
                let rl = run_len(self.b, j, c);
                if rl >= k {
                    let before = self.b[j - 1]; // j > from >= content_start > 0
                    if !is_ws_or_nl(before) {
                        if fwd_gate {
                            let fwd_ok = match self.b.get(j + k) {
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

    // ---- subscript / superscript ------------------------------------------

    /// `_x`/`_{x}` → Subscript, `^x`/`^{x}` → Superscript. Content (a non-space run or
    /// a `{ … }` group) is re-parsed for nested emphasis/links.
    fn try_script(&mut self, c: u8) -> bool {
        let after = match self.b.get(self.i + 1) {
            Some(&x) => x,
            None => return false,
        };
        let (content, end) = if after == b'{' {
            // `_{ … }` / `^{ … }`: up to the closing `}` on the same line.
            let body_start = self.i + 2;
            let mut j = body_start;
            while j < self.n && self.b[j] != b'}' && self.b[j] != b'\n' && self.b[j] != b'\r' {
                j += 1;
            }
            if j >= self.n || self.b[j] != b'}' || j == body_start {
                return false;
            }
            (self.s[body_start..j].to_string(), j + 1)
        } else {
            // `_x` / `^x`: a run of non-space chars (mldoc `take_while1 non_space`).
            if is_org_space(after) {
                return false;
            }
            let start = self.i + 1;
            let mut j = start;
            while j < self.n && !is_org_space(self.b[j]) {
                j += char_len(self.b[j]);
            }
            (self.s[start..j].to_string(), j)
        };
        let children = parse_inline_org(&content, Ctx::script());
        let node = if c == b'_' {
            Inline::Subscript { children }
        } else {
            Inline::Superscript { children }
        };
        self.push(node);
        self.i = end;
        true
    }

    // ---- tags -------------------------------------------------------------

    fn try_tag(&mut self) -> bool {
        let name_start = self.i + 1;
        let (end, children) = crate::inline::parse_tag_name(self.s, name_start);
        if end == name_start || children.is_empty() {
            return false;
        }
        self.push(Inline::Tag { children });
        self.i = end;
        true
    }

    // ---- block refs `(( … ))` --------------------------------------------

    fn try_block_ref(&mut self) -> bool {
        if !self.s[self.i..].starts_with("((") {
            return false;
        }
        if !self.seq_present(*b"))") {
            return false;
        }
        let inner_start = self.i + 2;
        let mut j = inner_start;
        while j < self.n && self.b[j] != b')' && self.b[j] != b'\n' && self.b[j] != b'\r' {
            j += 1;
        }
        if j == inner_start {
            return false;
        }
        if j + 1 < self.n && self.b[j] == b')' && self.b[j + 1] == b')' {
            let inner = self.s[inner_start..j].to_string();
            let full = self.s[self.i..j + 2].to_string();
            self.push(Inline::Link { url: Url::BlockRef { v: inner }, label: vec![], full, image: false, metadata: String::new(), title: None });
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
            return false;
        }
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

    // ---- bracket: org link / nested link / timestamp / footnote -----------

    fn try_bracket(&mut self) -> bool {
        // No `]` anywhere ahead ⇒ no bracket construct can match (keeps `[[[[…` linear).
        if !self.has_rbracket() {
            return false;
        }
        // org_link_1 `[[url][label]]` (needs `][`), then nested link / org_link_2
        // `[[url]]` (need `]]`). The seq guards keep `[[`-with-no-closer runs linear.
        if self.s[self.i..].starts_with("[[") {
            if self.seq_present(*b"][") {
                if let Some((end, node)) = self.org_link_1() {
                    self.push(node);
                    self.i = end;
                    return true;
                }
            }
            if self.seq_present(*b"]]") {
                if let Some((end, content)) = parse_nested_link(self.s, self.i) {
                    self.push(Inline::NestedLink { content });
                    self.i = end;
                    return true;
                }
                if let Some((end, node)) = self.org_link_2() {
                    self.push(node);
                    self.i = end;
                    return true;
                }
            }
        }
        // inactive timestamp `[date]` (+ optional range).
        if self.ctx.timestamps {
            if let Some((end, node)) = self.org_inactive_timestamp() {
                self.push(node);
                self.i = end;
                return true;
            }
        }
        // footnote reference `[fn:name]`.
        if self.ctx.footnotes {
            if let Some((end, name)) = self.org_footnote_ref() {
                self.push(Inline::Fnref { name });
                self.i = end;
                return true;
            }
        }
        false
    }

    /// `[[url][label]]` (mldoc `org_link_1`).
    fn org_link_1(&self) -> Option<(usize, Inline)> {
        let at = self.i;
        // url part: `[[` then chars (≠ ']', `\]` escaped, no eol) then `][`.
        let url_start = at + 2;
        let mut j = url_start;
        while j < self.n {
            let c = self.b[j];
            if c == b'\n' || c == b'\r' {
                return None;
            }
            if c == b'\\' && j + 1 < self.n {
                j += 1 + char_len(self.b[j + 1]);
                continue;
            }
            if c == b']' {
                break;
            }
            j += char_len(c);
        }
        if !self.s[j..].starts_with("][") {
            return None;
        }
        let url_text = self.s[url_start..j].to_string();
        let label_start = j + 2;
        // label: balanced single brackets until the closing `]]`.
        let close = self.find_org_label_end(label_start)?;
        let label_text = self.s[label_start..close].to_string();
        let mut end = close + 2;
        let metadata = self.read_metadata(&mut end);

        let url = classify_org_link_1(&url_text, &label_text);
        let label = parse_inline_org(&label_text, Ctx::label());
        let label_first = match label.first() {
            Some(Inline::Plain { text }) => text.clone(),
            _ => String::new(),
        };
        let full = format!("[[{}][{}]]{}", url_text, label_first, metadata);
        // org_link_1 carries Logseq media metadata `{:width …}` (mldoc's `metadata`);
        // org has no `![…]` image syntax (image=false) nor CommonMark titles.
        Some((end, Inline::Link { url, label, full, image: false, metadata, title: None }))
    }

    /// `[[url]]` (mldoc `org_link_2`). Single `]` allowed inside, non-empty, no eol.
    fn org_link_2(&self) -> Option<(usize, Inline)> {
        let at = self.i;
        let name_start = at + 2;
        let mut j = name_start;
        while j < self.n {
            let c = self.b[j];
            if c == b'\n' || c == b'\r' {
                return None;
            }
            if c == b'\\' && j + 1 < self.n {
                j += 1 + char_len(self.b[j + 1]);
                continue;
            }
            if c == b']' {
                if j + 1 < self.n && self.b[j + 1] == b']' {
                    break;
                }
                j += 1;
                continue;
            }
            j += char_len(c);
        }
        if j + 1 >= self.n || self.b[j] != b']' || self.b[j + 1] != b']' || j == name_start {
            return None;
        }
        let name = self.s[name_start..j].to_string();
        let url = classify_org_link_2(&name);
        let full = format!("[[{}]]", name);
        let label = match &url {
            Url::PageRef { .. } => vec![],
            _ => vec![Inline::Plain { text: name.clone() }],
        };
        Some((j + 2, Inline::Link { url, label, full, image: false, metadata: String::new(), title: None }))
    }

    /// Find the closing `]]` of an org-link label, balancing single `[ ]` pairs.
    fn find_org_label_end(&self, start: usize) -> Option<usize> {
        let mut j = start;
        let mut depth: i32 = 0;
        while j < self.n {
            let c = self.b[j];
            if c == b'\n' || c == b'\r' {
                return None;
            }
            if c == b'\\' && j + 1 < self.n {
                j += 1 + char_len(self.b[j + 1]);
                continue;
            }
            if c == b']' {
                if depth == 0 {
                    if j + 1 < self.n && self.b[j + 1] == b']' {
                        return Some(j);
                    }
                    return None;
                }
                depth -= 1;
                j += 1;
                continue;
            }
            if c == b'[' {
                depth += 1;
                j += 1;
                continue;
            }
            j += char_len(c);
        }
        None
    }

    /// Optional `{ … }` metadata after a link; advances `end` past it. Returns the raw
    /// metadata string (incl. braces) or "".
    fn read_metadata(&self, end: &mut usize) -> String {
        if self.b.get(*end) == Some(&b'{') {
            if let Some(close) = find_sub_line(self.b, *end + 1, b"}") {
                let meta = self.s[*end..close + 1].to_string();
                *end = close + 1;
                return meta;
            }
        }
        String::new()
    }

    fn org_inactive_timestamp(&self) -> Option<(usize, Inline)> {
        let (end1, ts1) = parse_bracket_date(self.s, self.i, b'[', b']')?;
        if self.s[end1..].starts_with("--") {
            if let Some((end2, ts2)) = parse_bracket_date(self.s, end1 + 2, b'[', b']') {
                let val = serde_json::json!({ "start": ts1, "stop": ts2 });
                return Some((end2, Inline::Timestamp { ts: "Range".to_string(), date: val }));
            }
        }
        Some((end1, Inline::Timestamp { ts: "Date".to_string(), date: ts1 }))
    }

    /// `[fn:name]` / `[fn:name:def]` / `[fn::def]` reference → name.
    fn org_footnote_ref(&self) -> Option<(usize, String)> {
        let rest = self.s[self.i..].strip_prefix("[fn:")?;
        let mut j = 0;
        let rb = rest.as_bytes();
        while j < rb.len() && rb[j] != b':' && rb[j] != b']' && rb[j] != b'\n' && rb[j] != b'\r' {
            j += 1;
        }
        let name = rest[..j].to_string();
        // optional `:def` then `]`.
        let after = &rest[j..];
        let close = after.find(']')?;
        if after[..close].contains('\n') || after[..close].contains('\r') {
            return None;
        }
        let end = self.i + 4 + j + close + 1;
        Some((end, name))
    }

    // ---- angle: autolink / timestamp / inline html / email ----------------

    /// Org dedicated/radio target `<<name>>` (mldoc `Target`): `<<`, non-empty inner
    /// (no `<`/`>`/eol), then `>>`. Inner taken raw (matching mldoc).
    fn try_target(&mut self) -> bool {
        if !self.s[self.i..].starts_with("<<") {
            return false;
        }
        let inner_start = self.i + 2;
        let mut j = inner_start;
        while j < self.n {
            let c = self.b[j];
            if c == b'<' || c == b'>' || c == b'\n' || c == b'\r' {
                break;
            }
            j += char_len(c);
        }
        if j > inner_start && j + 1 < self.n && self.b[j] == b'>' && self.b[j + 1] == b'>' {
            let text = self.s[inner_start..j].to_string();
            self.push(Inline::Target { text });
            self.i = j + 2;
            return true;
        }
        false
    }

    fn try_angle(&mut self) -> bool {
        if let Some((end, node)) = parse_org_autolink(self.s, self.i) {
            self.push(node);
            self.i = end;
            return true;
        }
        if self.ctx.timestamps {
            if let Some((end, node)) = parse_angle_timestamp(self.s, self.i) {
                self.push(node);
                self.i = end;
                return true;
            }
        }
        if let Some((end, text)) = parse_inline_html(self.s, self.i) {
            self.push(Inline::InlineHtml { text });
            self.i = end;
            return true;
        }
        if let Some((end, node)) = parse_email_autolink(self.s, self.i) {
            self.push(node);
            self.i = end;
            return true;
        }
        false
    }

    fn try_bare_url(&mut self) -> bool {
        if let Some((end, node)) = parse_bare_url(self.s, self.i) {
            self.push(node);
            self.i = end;
            return true;
        }
        false
    }
}

// ---- inline helpers -------------------------------------------------------

/// Org `plain` delimiters (`org_plain_delims`, minus whitespace): a plain run stops
/// at these (and at whitespace / newline). NOT `~ = ( < { ! @ ] )`.
#[inline]
fn is_org_marker_delim(c: u8) -> bool {
    matches!(c, b'\\' | b'_' | b'^' | b'[' | b'*' | b'/' | b'+' | b'$' | b'#')
}

/// mldoc `is_space` (used by sub/superscript `non_space`): space, tab, \012, \026.
#[inline]
fn is_org_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0c | 0x1a)
}

fn run_len(b: &[u8], pos: usize, c: u8) -> usize {
    let mut k = pos;
    while k < b.len() && b[k] == c {
        k += 1;
    }
    k - pos
}

/// `<scheme:rest>` autolink (mldoc `quick_link`): scheme letters/digits, `:`, optional
/// `//`, then non-space rest; ANY `:` makes it a link (so `<a:b>` works).
fn parse_org_autolink(s: &str, at: usize) -> Option<(usize, Inline)> {
    let b = s.as_bytes();
    let n = b.len();
    if b.get(at) != Some(&b'<') {
        return None;
    }
    let p0 = at + 1;
    let mut j = p0;
    while j < n && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == p0 || j >= n || b[j] != b':' {
        return None;
    }
    let protocol = s[p0..j].to_string();
    j += 1;
    let mut slashes = "";
    if s[j..].starts_with("//") {
        slashes = "//";
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
        url: Url::Complex { protocol: Some(protocol), link: Some(link) },
        label: vec![Inline::Plain { text: full.clone() }],
        full,
        image: false,
        metadata: String::new(),
        title: None,
    };
    Some((j + 1, node))
}

/// Classify an `[[url][label]]` destination (mldoc `org_link_1`): `file:` → File;
/// empty label → Search; `proto:link` (single colon, strip leading `//`) → Complex;
/// else Search.
fn classify_org_link_1(url_text: &str, label_text: &str) -> Url {
    if url_text.len() > 5 && url_text.starts_with("file:") {
        return Url::File { v: url_text.to_string() };
    }
    if label_text.is_empty() {
        return Url::Search { v: url_text.to_string() };
    }
    if let Some(idx) = url_text.find(':') {
        let protocol = &url_text[..idx];
        if !protocol.is_empty() {
            let mut link = &url_text[idx + 1..];
            if let Some(stripped) = link.strip_prefix("//") {
                link = stripped;
            }
            return Url::Complex { protocol: Some(protocol.to_string()), link: Some(link.to_string()) };
        }
    }
    Url::Search { v: url_text.to_string() }
}

/// Classify a `[[url]]` destination (mldoc `org_link_2`): `file:` → File;
/// `proto://link` → Complex; else Page_ref.
fn classify_org_link_2(name: &str) -> Url {
    if name.len() > 5 && name.starts_with("file:") {
        return Url::File { v: name.to_string() };
    }
    if let Some(idx) = name.find("://") {
        let protocol = &name[..idx];
        if !protocol.is_empty() {
            return Url::Complex {
                protocol: Some(protocol.to_string()),
                link: Some(name[idx + 3..].to_string()),
            };
        }
    }
    Url::PageRef { v: name.to_string() }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(s: &str) -> Vec<Inline> {
        parse_inline_org_top(s)
    }
    fn ik(i: &Inline) -> String {
        match i {
            Inline::Plain { text } => format!("plain({text})"),
            Inline::Code { text } => format!("code({text})"),
            Inline::Verbatim { text } => format!("verb({text})"),
            Inline::Emphasis { emph, .. } => format!("em({emph})"),
            Inline::Subscript { .. } => "sub".into(),
            Inline::Superscript { .. } => "sup".into(),
            Inline::Link { url, .. } => format!("link({})", uk(url)),
            Inline::Tag { children } => format!("tag({})", txt(children)),
            Inline::Macro { name, args } => format!("macro({name};{})", args.join("|")),
            Inline::NestedLink { content } => format!("nested({content})"),
            Inline::Target { text } => format!("target({text})"),
            Inline::Break => "break".into(),
            Inline::HardBreak => "hardbreak".into(),
            Inline::Latex { mode, body } => format!("latex({mode}:{body})"),
            Inline::Fnref { name } => format!("fn({name})"),
            Inline::Timestamp { ts, .. } => format!("ts({ts})"),
            Inline::InlineHtml { text } => format!("html({text})"),
            Inline::Email { .. } => "email".into(),
            Inline::Entity { unicode, .. } => format!("entity({unicode})"),
        }
    }
    fn uk(u: &Url) -> String {
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
    fn txt(c: &[Inline]) -> String {
        c.iter()
            .map(|x| match x {
                Inline::Plain { text } => text.clone(),
                Inline::Link { full, .. } => full.clone(),
                Inline::NestedLink { content } => content.clone(),
                _ => String::new(),
            })
            .collect()
    }
    fn ks(s: &str) -> Vec<String> {
        pi(s).iter().map(ik).collect()
    }
    fn bkinds(s: &str) -> Vec<&'static str> {
        parse(s)
            .iter()
            .map(|b| match b {
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
                Block::Directive { .. } => "directive",
                Block::Example { .. } => "example",
                Block::LatexEnv { .. } => "latex_env",
            })
            .collect()
    }

    // ---- headlines --------------------------------------------------------

    #[test]
    fn render_target_checkbox_orglink_metadata() {
        // dedicated/radio target `<<name>>` (raw inner, internal spaces kept).
        match &pi("see <<my target>> here")[1] {
            Inline::Target { text } => assert_eq!(text, "my target"),
            _ => panic!("expected Target"),
        }
        assert_eq!(pi("<<>>").len(), 1); // empty `<<>>` is not a target (stays plain)
        match &pi("<<>>")[0] {
            Inline::Plain { .. } => {}
            _ => panic!("empty target should be plain"),
        }
        // list checkboxes: `[ ]`→Some(false), `[x]`/`[X]`→Some(true), none→None.
        let item0 = |s: &str| match &parse(s)[0] {
            Block::List { items, .. } => items[0].clone(),
            _ => panic!("expected List"),
        };
        assert_eq!(item0("- [ ] todo").checkbox, Some(false));
        assert_eq!(item0("- [x] done").checkbox, Some(true));
        assert_eq!(item0("- [X] done").checkbox, Some(true));
        assert_eq!(item0("- plain").checkbox, None);
        assert_eq!(item0("1. [x] num").checkbox, Some(true));
        // org_link_1 carries media metadata `{:…}`; org_link_2 (no label) does not.
        match &pi("[[../a.png][img]]{:width 100}")[0] {
            Inline::Link { metadata, image, title, .. } => {
                assert_eq!(metadata, "{:width 100}");
                assert!(!*image);
                assert_eq!(title, &None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn headline_levels_and_space() {
        assert_eq!(bkinds("* Heading"), ["bullet"]);
        assert_eq!(bkinds("** Sub"), ["bullet"]);
        assert_eq!(bkinds("*** Deep"), ["bullet"]);
        assert_eq!(bkinds("*no space"), ["paragraph"]); // no space ⇒ not a headline
        match &parse("** Sub")[0] {
            Block::Bullet { level, .. } => assert_eq!(*level, 2),
            _ => panic!(),
        }
    }

    #[test]
    fn headline_marker_priority_tags() {
        match &parse("* TODO [#A] task with :tag1:tag2:")[0] {
            Block::Bullet { marker, priority, htags, inline, .. } => {
                assert_eq!(marker.as_deref(), Some("TODO"));
                assert_eq!(priority.as_deref(), Some("A"));
                assert_eq!(htags, &vec!["tag1".to_string(), "tag2".to_string()]);
                assert_eq!(inline, &vec![Inline::Plain { text: "task with ".into() }]);
            }
            _ => panic!(),
        }
        // marker at end-of-line (no title).
        match &parse("* TODO")[0] {
            Block::Bullet { marker, inline, .. } => {
                assert_eq!(marker.as_deref(), Some("TODO"));
                assert!(inline.is_empty());
            }
            _ => panic!(),
        }
        // tags only (empty title).
        match &parse("* plain :only:tags:")[0] {
            Block::Bullet { htags, inline, .. } => {
                assert_eq!(htags, &vec!["only".to_string(), "tags".to_string()]);
                assert_eq!(inline, &vec![Inline::Plain { text: "plain ".into() }]);
            }
            _ => panic!(),
        }
    }

    // ---- emphasis ---------------------------------------------------------

    #[test]
    fn emphasis_six_kinds() {
        assert_eq!(ks("*bold*"), ["em(Bold)"]);
        assert_eq!(ks("/italic/"), ["em(Italic)"]);
        assert_eq!(ks("_under_"), ["em(Underline)"]);
        assert_eq!(ks("+strike+"), ["em(Strike_through)"]);
        assert_eq!(ks("~code~"), ["code(code)"]);
        assert_eq!(ks("=verb="), ["verb(verb)"]);
        assert_eq!(ks("^^hl^^"), ["em(Highlight)"]);
    }

    #[test]
    fn emphasis_boundary_literals() {
        // these must stay LITERAL (the gates kill them)
        assert_eq!(ks("a/b/c"), ["plain(a/b/c)"]);
        assert_eq!(ks("/a/b/"), ["plain(/a/b/)"]);
        assert_eq!(ks("+a+b+"), ["plain(+a+b+)"]);
        // but bold has no gates, so it fires even between digits/letters
        assert_eq!(ks("2*3*4"), ["plain(2)", "em(Bold)", "plain(4)"]);
        assert_eq!(ks("a*b*c"), ["plain(a)", "em(Bold)", "plain(c)"]);
        // verbatim/code only at a run boundary (sticky plain run)
        assert_eq!(ks("a~code~"), ["plain(a~code~)"]);
        assert_eq!(ks("x ~code~"), ["plain(x )", "code(code)"]);
    }

    #[test]
    fn emphasis_nesting_and_newline() {
        // /it *bo* it/ → Italic[plain, Bold, plain]
        match &pi("nested /it *bo* it/")[1] {
            Inline::Emphasis { emph, children } => {
                assert_eq!(emph, "Italic");
                assert!(children.iter().any(|c| matches!(c, Inline::Emphasis { emph, .. } if emph == "Bold")));
            }
            _ => panic!(),
        }
        // bold spans a newline (kept as literal plain)
        match &pi("*bold spanning\nnewline*")[0] {
            Inline::Emphasis { children, .. } => {
                assert_eq!(children, &vec![Inline::Plain { text: "bold spanning\nnewline".into() }]);
            }
            _ => panic!(),
        }
    }

    // ---- subscript / superscript ------------------------------------------

    #[test]
    fn sub_superscript() {
        assert_eq!(ks("snake_case_var"), ["plain(snake)", "sub"]);
        assert_eq!(ks("a^b^c"), ["plain(a)", "sup"]);
        assert_eq!(ks("x_{i+1}"), ["plain(x)", "sub"]);
        // sub content does NOT nest further sub/sup
        match &pi("snake_case_var")[1] {
            Inline::Subscript { children } => {
                assert_eq!(children, &vec![Inline::Plain { text: "case_var".into() }]);
            }
            _ => panic!(),
        }
    }

    // ---- links ------------------------------------------------------------

    #[test]
    fn links() {
        assert_eq!(ks("[[target]]"), ["link(page:target)"]);
        assert_eq!(ks("[[target][label]]"), ["link(search:target)"]);
        assert_eq!(ks("[[https://x.org][site]]"), ["link(complex:https:x.org)"]);
        assert_eq!(ks("[[https://x.org]]"), ["link(complex:https:x.org)"]);
        assert_eq!(ks("[[file:foo.org][bar]]"), ["link(file:file:foo.org)"]);
        assert_eq!(ks("[[exam:ple]]"), ["link(page:exam:ple)"]); // no // ⇒ page ref
        assert_eq!(ks("[[a[[b]]c]]"), ["nested([[a[[b]]c]])"]);
        // page ref produces a ref; labelled link does not over-extract
        let r = crate::refs::extract_refs(&parse("[[target]] and [[b][c]]"));
        assert_eq!(r.page, vec!["target".to_string()]);
    }

    // ---- timestamps -------------------------------------------------------

    #[test]
    fn timestamps() {
        assert_eq!(ks("<2026-06-26 Fri>"), ["ts(Date)"]);
        assert_eq!(ks("[2026-06-20 Sat]"), ["ts(Date)"]);
        assert_eq!(
            ks("<2026-06-26 Fri>--<2026-06-28 Sun>"),
            ["ts(Range)"]
        );
        match &parse("* h\nSCHEDULED: <2026-06-26 Fri>")[1] {
            Block::Paragraph { inline, .. } => {
                assert!(matches!(&inline[0], Inline::Timestamp { ts, .. } if ts == "Scheduled"));
            }
            _ => panic!(),
        }
    }

    // ---- directives / blocks / drawers ------------------------------------

    #[test]
    fn directives_and_blocks() {
        assert_eq!(bkinds("#+TITLE: my title"), ["directive"]);
        match &parse("#+TITLE: my title")[0] {
            Block::Directive { name, value, .. } => {
                assert_eq!(name, "TITLE");
                assert_eq!(value, "my title");
            }
            _ => panic!(),
        }
        // #+BEGIN_X blocks
        assert_eq!(bkinds("#+BEGIN_SRC clojure\n(defn x [])\n#+END_SRC"), ["src"]);
        assert_eq!(bkinds("#+BEGIN_QUOTE\nq\n#+END_QUOTE"), ["quote"]);
        assert_eq!(bkinds("#+BEGIN_EXAMPLE\nlit\n#+END_EXAMPLE"), ["example"]);
        // a `*` line inside SRC stays code, not a headline.
        match &parse("#+BEGIN_SRC\n* star line\n#+END_SRC")[0] {
            Block::Src { code, .. } => assert_eq!(code, "* star line\n"),
            _ => panic!(),
        }
    }

    #[test]
    fn drawers_and_properties() {
        match &parse(":PROPERTIES:\n:key: value\n:another: 2\n:END:")[0] {
            Block::Properties { props, .. } => {
                assert_eq!(props, &vec![("key".into(), "value".into()), ("another".into(), "2".into())]);
            }
            _ => panic!(),
        }
        assert_eq!(bkinds(":LOGBOOK:\nCLOCK: x\n:END:"), ["drawer"]);
        // #+NAME directives fold into a preceding property drawer.
        match &parse(":PROPERTIES:\n:a: 1\n:END:\n#+b: 2")[0] {
            Block::Properties { props, .. } => {
                assert_eq!(props, &vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
            }
            _ => panic!(),
        }
    }

    // ---- lists / tables / hr / footnotes ----------------------------------

    #[test]
    fn lists_tables_hr_footnotes() {
        assert_eq!(bkinds("- milk\n- eggs\n+ also"), ["list"]);
        assert_eq!(bkinds("1. first\n2. second"), ["list"]);
        assert_eq!(bkinds("| a | b |\n|---+---|\n| 1 | 2 |"), ["table"]);
        assert_eq!(bkinds("-----"), ["hr"]); // exactly 5 dashes
        assert_eq!(bkinds("------"), ["paragraph"]); // 6 ⇒ not hr
        assert_eq!(bkinds("[fn:1] the definition"), ["footnote_def"]);
        assert_eq!(ks("see [fn:1] ref"), ["plain(see )", "fn(1)", "plain( ref)"]);
        // table header/data
        match &parse("| a | b |\n|---+---|\n| 1 | 2 |")[0] {
            Block::Table { header, rows, .. } => {
                assert_eq!(header.as_ref().unwrap().len(), 2);
                assert_eq!(rows.len(), 1); // separator row dropped
            }
            _ => panic!(),
        }
    }

    #[test]
    fn paragraph_breaks_and_blank_absorption() {
        // a directive absorbs the following blank line (no break paragraph).
        assert_eq!(bkinds("#+TITLE: x\n\n* H"), ["directive", "bullet"]);
        // a heading does NOT: the blank becomes a Paragraph[Break].
        assert_eq!(bkinds("* A\n\n* B"), ["bullet", "paragraph", "bullet"]);
        // multi-line paragraph coalesces with Break_Line.
        match &parse("a plain paragraph\nsecond line")[0] {
            Block::Paragraph { inline, .. } => assert_eq!(
                inline,
                &vec![
                    Inline::Plain { text: "a plain paragraph".into() },
                    Inline::Break,
                    Inline::Plain { text: "second line".into() },
                ]
            ),
            _ => panic!(),
        }
    }

    // ---- tags / macros / block refs ---------------------------------------

    #[test]
    fn tags_macros_blockrefs() {
        assert_eq!(ks("a #tag here"), ["plain(a )", "tag(tag)", "plain( here)"]);
        assert_eq!(ks("{{namespace Formula1}}"), ["macro(namespace;Formula1)"]);
        assert_eq!(ks("{{embed [[Foo]]}}"), ["macro(embed;[[Foo]])"]);
        let u = "11111111-1111-1111-1111-111111111111";
        assert_eq!(ks(&format!("x (({}))", u)), [format!("plain(x )"), format!("link(block:{})", u)]);
    }

    // ---- backslash (Org does NOT unescape) --------------------------------

    #[test]
    fn org_backslash_kept() {
        assert_eq!(ks("a\\*b"), ["plain(a\\*b)"]);
        assert_eq!(ks("x\\\\y"), ["plain(x\\\\y)"]);
        match &pi("a\\\nb")[1] {
            Inline::HardBreak => {}
            _ => panic!("expected hard break"),
        }
    }

    // ---- robustness -------------------------------------------------------

    #[test]
    fn latex_entities_and_environment_org() {
        // Org resolves the same LaTeX entity table as Markdown.
        match &pi("\\Delta G")[0] {
            Inline::Entity { name, unicode, .. } => { assert_eq!(name, "Delta"); assert_eq!(unicode, "Δ"); }
            other => panic!("{other:?}"),
        }
        assert_eq!(ks("\\Delta{}G"), ["entity(Δ)", "plain(G)"]);
        assert_eq!(ks("\\foo G"), ["plain(foo G)"]); // unknown → bare letters (bksl kept? no — dropped)
        // block-level LaTeX environment in Org.
        match &parse("\\begin{equation}\nx=1\n\\end{equation}")[0] {
            Block::LatexEnv { name, content, .. } => { assert_eq!(name, "equation"); assert_eq!(content, "x=1\n"); }
            _ => panic!(),
        }
        assert_eq!(bkinds("\\begin{eq}a b\\end{eq}"), ["latex_env"]);
        assert_eq!(bkinds("hi \\begin{eq}x\\end{eq}"), ["paragraph"]); // text before ⇒ not env
    }

    #[test]
    fn unicode_does_not_panic() {
        for s in [
            "* café 中文 😀 :tag:",
            "/中文/ and _naïve_",
            "[[café]] and #naïve",
            "snake_café_var",
            "#+BEGIN_SRC\ncafé 中文 😀\n#+END_SRC",
            "* TODO [#A] 中文 :标签:",
            "a\u{200b}b ^中^ _下_",
            "[fn:abc] 中文",
            "~中文~ =café=",
        ] {
            let _ = parse(s);
        }
    }

    // ---- M6 fuzz-hardening (block over/under-detection vs mldoc) ----------

    #[test]
    fn verbatim_colon_lines() {
        // ANY `:`-prefixed line that isn't a recognized drawer → fixed-width Example.
        assert_eq!(bkinds(": text"), ["example"]);
        assert_eq!(bkinds(":text"), ["example"]);
        assert_eq!(bkinds(":key: value"), ["example"]); // standalone "property"
        assert_eq!(bkinds(":tag1:tag2:"), ["example"]);
        assert_eq!(bkinds(":END:"), ["example"]); // bare :END:
        assert_eq!(bkinds(":PROPERTIES:"), ["example"]); // unclosed drawer head
        assert_eq!(bkinds("  : indented"), ["example"]);
        // content: leading ws after `:` stripped, trailing kept.
        match &parse(":    x")[0] {
            Block::Example { code, .. } => assert_eq!(code, "x\n"),
            _ => panic!(),
        }
        match &parse(": a b  ")[0] {
            Block::Example { code, .. } => assert_eq!(code, "a b  \n"),
            _ => panic!(),
        }
        // consecutive `:` lines coalesce into ONE Example.
        match &parse(": line1\n: line2\n: line3")[0] {
            Block::Example { code, .. } => assert_eq!(code, "line1\nline2\nline3\n"),
            _ => panic!(),
        }
        // valid drawers must STAY drawers, not verbatim.
        assert_eq!(bkinds(":PROPERTIES:\n:k: v\n:END:"), ["properties"]);
        assert_eq!(bkinds(":LOGBOOK:\nCLOCK: x\n:END:"), ["drawer"]);
        // properties drawer followed by a `:`-line → drawer + Example.
        assert_eq!(bkinds(":PROPERTIES:\n:k: v\n:END:\n:more: stuff"), ["properties", "example"]);
        // verbatim run swallows an embedded `:NAME:` (drawer not re-tried mid-run).
        assert_eq!(bkinds(": text\n:NAME:\ncontent\n:END:"), ["example", "paragraph", "example"]);
    }

    #[test]
    fn footnote_def_needs_body() {
        assert_eq!(bkinds("[fn:1]"), ["paragraph"]); // bare ref
        assert_eq!(bkinds("[fn:1]   "), ["paragraph"]); // no body
        assert_eq!(bkinds("[fn:1] body"), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1]body"), ["footnote_def"]); // no space ok
        assert_eq!(bkinds("[fn:1]:x"), ["footnote_def"]);
        assert_eq!(bkinds(" [fn:1] body"), ["footnote_def"]); // leading ws ok
        // forbidden first char (`* # [ -`) → inline ref in a paragraph.
        for s in ["[fn:1]*x", "[fn:1]#x", "[fn:1][x", "[fn:1]-x"] {
            assert_eq!(bkinds(s), ["paragraph"], "{s}");
        }
    }

    #[test]
    fn empty_list_marker_is_paragraph() {
        for s in ["+ ", "- ", "1. ", "- [ ]", "- [ ]   "] {
            assert_eq!(bkinds(s), ["paragraph"], "{s}");
        }
        for s in ["+ x", "- x", "1. x", "- [ ] x", "+ [X] done"] {
            assert_eq!(bkinds(s), ["list"], "{s}");
        }
    }

    #[test]
    fn indented_dash_is_paragraph() {
        // `-` is a bullet only at column 0; indented `-` is prose (mldoc quirk).
        assert_eq!(bkinds("  - x"), ["paragraph"]);
        assert_eq!(bkinds("\t- x"), ["paragraph"]);
        // but indented `+`/`N.` stay lists.
        assert_eq!(bkinds("  + y"), ["list"]);
        assert_eq!(bkinds("  1. z"), ["list"]);
        match &parse("  + y")[0] {
            Block::List { items, .. } => assert_eq!(items[0].indent, 2),
            _ => panic!(),
        }
    }

    #[test]
    fn nested_org_lists() {
        // Compact tree shape "a[b,c]" (a with children b,c); see `nest_items`. Org
        // `-` nests only as a col-0 sibling/parent; `+` and `N.` nest via indent.
        fn label(it: &ListItem) -> String {
            match &it.content[0] {
                Block::Paragraph { inline, .. } => match inline.first() {
                    Some(Inline::Plain { text }) => text.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            }
        }
        fn shape(items: &[ListItem]) -> String {
            items
                .iter()
                .map(|it| {
                    if it.items.is_empty() {
                        label(it)
                    } else {
                        format!("{}[{}]", label(it), shape(&it.items))
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        }
        let items = |input: &str| -> Vec<ListItem> {
            match &parse(input)[0] {
                Block::List { items, .. } => items.clone(),
                b => panic!("not a list: {b:?}"),
            }
        };
        assert_eq!(shape(&items("+ a\n  + b")), "a[b]");
        assert_eq!(shape(&items("+ a\n  + b\n    + c")), "a[b[c]]");
        assert_eq!(shape(&items("+ a\n + b")), "a[b]");
        assert_eq!(shape(&items("+ a\n+ b")), "a,b");
        assert_eq!(shape(&items("1. a\n   2. b\n   3. c")), "a[b,c]");
        assert_eq!(shape(&items("- a\n  1. b")), "a[b]"); // col-0 `-` parent + numbered child
        assert_eq!(shape(&items("+ a\n    + deep\n  + mid")), "a[deep],mid");
    }

    #[test]
    fn malformed_table_is_paragraph() {
        // a row must start AND end with `|`.
        for s in ["| a | b", "|a", "|", "| a |\\"] {
            assert_eq!(bkinds(s), ["paragraph"], "{s}");
        }
        for s in ["| a | b |", "||", "| a |", "| a |   "] {
            assert_eq!(bkinds(s), ["table"], "{s}");
        }
        // a non-row line breaks the table group.
        assert_eq!(bkinds("| a | b |\n| c | d"), ["table", "paragraph"]);
    }

    #[test]
    fn directive_leading_ws_and_value_trim() {
        assert_eq!(bkinds("  #+TODO: x"), ["directive"]); // leading ws allowed
        // value is left-trimmed only (mldoc keeps trailing whitespace).
        match &parse("#+TITLE: hello  ")[0] {
            Block::Directive { name, value, .. } => {
                assert_eq!(name, "TITLE");
                assert_eq!(value, "hello  ");
            }
            _ => panic!(),
        }
        match &parse("#+a:b:c")[0] {
            Block::Directive { name, value, .. } => {
                assert_eq!(name, "a");
                assert_eq!(value, "b:c");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn empty_headline_trailing_ws_splits() {
        // empty-title headline + trailing ws → Bullet + Paragraph(leftover ws).
        assert_eq!(bkinds("*** "), ["bullet", "paragraph"]);
        assert_eq!(bkinds("* TODO "), ["bullet", "paragraph"]);
        assert_eq!(bkinds("*   "), ["bullet", "paragraph"]);
        // no trailing ws → no split.
        assert_eq!(bkinds("*"), ["bullet"]);
        // a real title (even with trailing ws) is NOT split.
        assert_eq!(bkinds("* title "), ["bullet"]);
        // the leftover-ws paragraph absorbs following lines.
        match &parse("* \nreal content")[1] {
            Block::Paragraph { inline, .. } => assert_eq!(
                inline,
                &vec![
                    Inline::Plain { text: " ".into() },
                    Inline::Break,
                    Inline::Plain { text: "real content".into() },
                ]
            ),
            _ => panic!(),
        }
        assert_eq!(bkinds("*** \n* B"), ["bullet", "paragraph", "bullet"]);
    }

    #[test]
    fn adversarial_runs_terminate() {
        let _ = pi(&"*a ".repeat(20000));
        let _ = pi(&"/a ".repeat(20000));
        let _ = pi(&"[[".repeat(20000));
        let _ = pi(&"((".repeat(20000));
        let _ = pi(&"_".repeat(50000));
        let _ = parse(&"* h\n".repeat(20000));
    }
}

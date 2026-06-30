//! Org-mode parser (M6).
//!
//! A from-scratch Org parser, behavior-equivalent to mldoc 1.5.7's Org config
//! (`format:"Org"`), verified against the live oracle. This module is the line-based
//! block segmenter (`parse`); inline markup is resolved by the lexer+resolver in
//! [`crate::org_resolver`]. A few Org-specific inline leaf predicates (autolink,
//! `[[…]]`/`[[…][…]]` link classification) live here and are reused by the resolver;
//! other format-agnostic leaf helpers come from `crate::inline`.
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

use crate::inline::{char_len, is_ws_or_nl};
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
/// Max nested-`Quote` depth (whether the `>`s sit on one line or recurse across lines).
/// mldoc itself stack-overflows on deep `>` (it errors out ≈1000 `>`), so no comparable
/// output exists to match past a modest depth; this cap only bounds the recursive
/// build/ref-walk/serialize/drop of pathological `>`×N input so it can't SIGABRT — kept
/// low enough that even a debug build survives on a 1 MiB stack. Far above any real /
/// corpus / fuzz-reachable nesting (a handful of `>`), so it never affects real output.
const QUOTE_NEST_CAP: usize = 64;

std::thread_local! {
    /// Current Org blockquote nesting depth across recursive `parse` of multi-line quote
    /// bodies (see `build_org_quote`); bounds pathological deep `>` so it can't SIGABRT.
    static ORG_QUOTE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Are we re-parsing the BODY of a blockquote (`>` or `#+BEGIN_QUOTE`)? Inside a
    /// quote mldoc does NOT recognize Org headlines (`* x` → Paragraph, not Heading), so
    /// headline detection is suppressed while this is set. C2.
    static ORG_IN_QUOTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Re-parse `inner` as the body of a blockquote with headline detection suppressed
/// (mldoc treats `* x` inside a quote as a Paragraph). Restores the previous flag, so
/// nested quotes stay suppressed. C2.
fn parse_quote_inner(inner: &str) -> Vec<Block> {
    let prev = ORG_IN_QUOTE.with(|c| c.replace(true));
    let r = parse(inner);
    ORG_IN_QUOTE.with(|c| c.set(prev));
    r
}

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
    parse_doc(input, false)
}

/// Block segmenter. `in_item` = re-parsing the **content** of an Org list item.
/// mldoc parses list-item content with `list_content_parsers` (mldoc_parser.ml),
/// whose choice set does NOT include Directive/Drawer/Heading/Footnote/List — so
/// inside an item those constructs stay paragraphs (`#+K: v`), verbatim (`:x` →
/// Example) or inline (`[fn:1] x`), never their own block. Everything else (Table,
/// `#+BEGIN`/fences/`:`-verbatim/`>`-quote/`$$`/`<html>`, Latex_env, Hr, Paragraph)
/// is recognised in both contexts. Quote/Custom children re-enter via `parse`
/// (`in_item = false`), matching mldoc's `block_content_parsers` for those.
fn parse_doc(input: &str, in_item: bool) -> Vec<Block> {
    let mut lines = split_lines(input);
    // Byte offset of the last `]` (None if none): a block-level hiccup `[:tag …]` needs a
    // closing `]`, so a `[:` line starting at/after it is skipped O(1) (see step 13b).
    let last_rbracket = input.rfind(']');
    // Sparse, sorted closer-line INDEXES so a `#+BEGIN_X` block / `:NAME:` drawer / fence
    // opener finds its matching closer ON-DEMAND at the dispatch point (binary-searching
    // only candidate lines, never an EOF re-scan per opener — kills the O(n²) class, audit
    // R2-P4/P6). Exact mldoc semantics: the first matching closer line after the opener.
    // On-demand finding is also context-aware — a closer inside a block/drawer body (which
    // the loop jumps past) can never pair with an opener outside it (the fence-straddle
    // bug). Computed on the original lines; the headline-split rewrite never creates an
    // `#+END_`/`:END:`/fence OPENER line, so the indexes stay valid through it + re-enter.
    let mut end_idxs: Vec<usize> = Vec::new(); // `#+END_…` lines (block closers)
    let mut drawer_end_idxs: Vec<usize> = Vec::new(); // `:END:` lines (drawer closers)
    let mut fence_line_idxs: std::collections::HashMap<u8, Vec<usize>> =
        std::collections::HashMap::new(); // per-char whole-line ```/~~~ marker lines
    for (idx, l) in lines.iter().enumerate() {
        if l.text.trim_start().get(..6).is_some_and(|p| p.eq_ignore_ascii_case("#+END_")) {
            end_idxs.push(idx);
        }
        if l.text.trim().eq_ignore_ascii_case(":END:") {
            drawer_end_idxs.push(idx);
        }
        if let Some((c, _)) = fence_marker(l.text) {
            fence_line_idxs.entry(c).or_default().push(idx);
        }
    }
    let mut out: Vec<Block> = Vec::new();
    let mut para: Option<(usize, usize)> = None;
    // After an "absorbing" block (Directive/Comment/Block/Footnote) mldoc's
    // `<* optional eols` swallows the following blank lines; Heading/Table/Drawer/List
    // do not (a List only consumes the single blank that ends its last item, via
    // `two_eols`), so a further blank there becomes a (leading-Break) paragraph.
    let mut absorb = false;
    // Memoised collapse floor: once a list starting at line `s` collapses with its
    // trigger at line `e`, every list-start in `[s, e)` collapses the same way (the
    // suffix is identical). Skipping the collector for those lines keeps repeated
    // collapses linear instead of O(n²) re-scanning.
    let mut collapse_floor = 0usize;
    // Memo for the headline block-opener split (see `headline_split_opener`): block-names
    // already known to have NO `#+END_` ahead, so repeated unclosed `* #+BEGIN_X` headlines
    // and `#+BEGIN_X` openers don't each re-scan to EOF. (Fences need no such memo —
    // `find_matching_fence` is a monotone-cursor lookup, not a re-scan.)
    let mut no_block_end: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fence_cursor: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
    let mut i = 0;

    while i < lines.len() {
        // `t`/`line_start`/`line_end` are copied out (a `&'a str` + two `usize`s, none
        // borrowing the `lines` Vec) so the headline block-opener split can REWRITE
        // `lines[i]` in place (see step 3) without a borrow conflict.
        let line = &lines[i];
        let t = line.text;
        let line_start = line.start;
        let line_end = line.end;

        // blank line: extend an open paragraph, else swallow (if absorbing) or start one.
        if t.trim().is_empty() {
            if let Some((s, _)) = para {
                para = Some((s, line_end));
            } else if absorb {
                // swallowed by the preceding block.
            } else {
                para = Some((line_start, line_end));
            }
            i += 1;
            continue;
        }

        // 1. directive `#+KEY: value` (KEY != BEGIN_…) — not a list-item content block.
        if let Some((name, value)) = directive(t).filter(|_| !in_item) {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::Directive { name, value, span: Some(Span(line_start, line_end)) });
            absorb = true;
            i += 1;
            continue;
        }

        // 1b. comment `# text` (mldoc Comment). Unlike Directive this IS a valid
        // list-item content block (mldoc `- a\n  # c` → item content [Paragraph, Comment]),
        // so it is NOT gated on `in_item`. `#+…` is a directive (handled above);
        // `#c`/`# `/`##` are paragraphs. Absorbs a following blank line.
        if let Some(text) = org_comment(t) {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::Comment {
                text: text.to_string(),
                span: Some(Span(line_start, line_end)),
            });
            absorb = true;
            i += 1;
            continue;
        }

        // 2. drawer `:PROPERTIES:`/`:NAME:` … `:END:` — not a list-item content block
        // (inside an item a `:`-line is verbatim/Example via step 7 instead).
        if let Some(name) = drawer_begin(t).filter(|_| !in_item) {
            if let Some(close) = find_drawer_end(&drawer_end_idxs, i) {
                flush_para(&mut out, &mut para, input, in_item);
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
                    out.push(Block::Properties { props, span: Some(Span(line_start, end)) });
                    absorb = folded;
                    i = j;
                    continue;
                }
                out.push(Block::Drawer { name, span: Some(Span(line_start, lines[close].end)) });
                absorb = false;
                i = close + 1;
                continue;
            }
        }

        // 3. headline `*{n} ` — not a list-item content block (stays a paragraph line),
        // and NOT inside a blockquote body (mldoc: `* x` in a quote is a Paragraph). C2.
        if let Some(level) =
            headline_level(t).filter(|_| !in_item && !ORG_IN_QUOTE.with(|c| c.get()))
        {
            let stars = t.bytes().take_while(|&b| b == b'*').count();
            let after = t[stars..].trim_start();
            let (marker, priority, content) = split_markers(after);
            // `content` is a (left-trimmed) suffix of the line, so its byte offset is
            // recoverable from the lengths.
            let content_off = line_start + (t.len() - content.len());

            // SPLIT: the post-marker CONTENT begins a block-construct opener ⇒ emit an
            // empty bullet (keeping level/marker/priority) and reparse CONTENT as the
            // following block, exactly like mldoc's heading-title lookahead (heading0.ml).
            if !content.is_empty()
                && headline_split_opener(
                    content,
                    input,
                    content_off,
                    &lines,
                    i,
                    &end_idxs,
                    &mut no_block_end,
                    &fence_line_idxs,
                    &mut fence_cursor,
                )
            {
                flush_para(&mut out, &mut para, input, in_item);
                out.push(Block::Bullet {
                    level,
                    size: None,
                    inline: vec![],
                    marker,
                    priority,
                    htags: vec![],
                    span: Some(Span(line_start, content_off)),
                });
                // Markdown ``` / ~~~ fence → Src: the `* ```` headline line is not itself a
                // whole-line fence marker (so it isn't in `fence_line_idxs`); its closer is
                // the first same-char whole-line fence after it. The predicate only lets a
                // fence reach here when it CLOSES, so this is `Some`; the `if` is a
                // belt-and-braces guard (an unclosed fence stays the heading title and
                // never enters this branch).
                if let Some((fchar, frun)) = fence_marker(content) {
                    if let Some(close) = find_matching_fence(&fence_line_idxs, &mut fence_cursor, i, fchar) {
                        let code = if close > i + 1 {
                            input[lines[i + 1].start..lines[close - 1].end].to_string()
                        } else {
                            String::new()
                        };
                        let lang = content[frun..].trim().to_string();
                        out.push(Block::Src {
                            lang,
                            code,
                            span: Some(Span(content_off, lines[close].end)),
                        });
                        absorb = true;
                        i = close + 1;
                        continue;
                    }
                }
                // Generic reparse: REWRITE this line to its CONTENT slice and re-enter the
                // loop WITHOUT advancing `i`, so the column-0 block parsers (and their
                // multi-line consumption of the following real lines) handle it exactly as
                // mldoc does. Terminates: `content` begins a non-`*` opener, so the headline
                // branch can't re-fire on it and every other branch advances `i`.
                lines[i] = Line { start: content_off, end: line_end, text: content };
                absorb = false;
                continue;
            }

            flush_para(&mut out, &mut para, input, in_item);
            let mut inline = org_inline(content);
            let htags = extract_htags(&mut inline);
            let empty_title = inline.is_empty() && htags.is_empty();
            out.push(Block::Bullet {
                level,
                size: None, // org headlines carry no `#`-size (mldoc Heading.size = null)
                inline,
                marker,
                priority,
                htags,
                span: Some(Span(line_start, line_end)),
            });
            absorb = false;
            // mldoc quirk: an EMPTY-title headline that still has trailing whitespace
            // (`*** `, `* TODO `) emits the empty bullet, then the leftover whitespace
            // begins a fresh paragraph that absorbs the following lines (`* \nx` →
            // Bullet + Paragraph[" ", Break, "x"]).
            if empty_title {
                let content_len = t.trim_end_matches([' ', '\t']).len();
                if content_len < t.len() {
                    para = Some((line_start + content_len, line_end));
                }
            }
            i += 1;
            continue;
        }

        // 4. table (group of consecutive well-formed `|…|` rows)
        if is_table_row(t) {
            flush_para(&mut out, &mut para, input, in_item);
            let start = i;
            while i < lines.len() && is_table_row(lines[i].text) {
                i += 1;
            }
            out.push(build_table(&lines[start..i], lines[start].start, lines[i - 1].end));
            absorb = false;
            continue;
        }

        // 4b. LaTeX environment `\begin{X} … \end{X}` (mldoc Latex_env, before Block).
        let line_content_end = line_start + t.len();
        if let Some((name, content, consumed_end)) =
            crate::inline::parse_latex_env(input, line_start, line_content_end)
        {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::LatexEnv { name, content, span: Some(Span(line_start, consumed_end)) });
            absorb = false;
            let mut ni = i + 1;
            while ni < lines.len() && lines[ni].start < consumed_end {
                ni += 1;
            }
            i = ni;
            continue;
        }

        // 5. fenced code block (```/~~~) — markdown fences work in Org too. ON-DEMAND
        // (context-aware): a fence-marker line the loop reaches is a top-level opener
        // (block/drawer bodies are jumped past), so its closer = the first same-char
        // whole-line fence after it — it can never pair across a body boundary the way the
        // old global `pair_fences` pre-pass did (the fence-straddle bug).
        if let Some((c, mend)) = fence_marker(t) {
            if let Some(close) = find_matching_fence(&fence_line_idxs, &mut fence_cursor, i, c) {
                flush_para(&mut out, &mut para, input, in_item);
                let code = if close > i + 1 {
                    input[lines[i + 1].start..lines[close - 1].end].to_string()
                } else {
                    String::new()
                };
                let lang = t[mend..].trim().to_string();
                out.push(Block::Src { lang, code, span: Some(Span(line_start, lines[close].end)) });
                absorb = true;
                i = close + 1;
                continue;
            }
        }

        // 6. `#+BEGIN_X` … `#+END_X` block
        if let Some(name) = block_begin(t) {
            if let Some(close) = find_block_end(&end_idxs, &lines, &mut no_block_end, i, &name) {
                flush_para(&mut out, &mut para, input, in_item);
                let inner = block_code(&lines[i + 1..close]);
                let span = Some(Span(line_start, lines[close].end));
                let lname = name.to_ascii_lowercase();
                match lname.as_str() {
                    "src" => {
                        let lang = begin_lang(t);
                        out.push(Block::Src { lang, code: inner, span });
                    }
                    "example" => out.push(Block::Example { code: inner, span }),
                    "quote" => out.push(Block::Quote { children: parse_quote_inner(&inner), span }),
                    _ => out.push(Block::Custom { name: lname, children: parse(&inner), span }),
                }
                absorb = true;
                i = close + 1;
                continue;
            }
        }

        // 7. verbatim block (Org): consecutive lines starting with `:` → Example.
        if is_verbatim_line(t) {
            flush_para(&mut out, &mut para, input, in_item);
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
        if let Some(first_content) = quote_first_line(t) {
            flush_para(&mut out, &mut para, input, in_item);
            let start = i;
            // First line: mldoc strips up to TWO leading `>` (enter the quote, then the
            // remainder is itself a body line that drops one more `>`); continuation
            // lines drop one. The de-`>`'d body is then re-parsed (a leading `>` body
            // line ⇒ a nested quote), so N leading `>` nest ⌈N/2⌉ Quotes.
            let mut body = String::new();
            body.push_str(&first_content);
            body.push('\n');
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
            // Build the (possibly nested) Quote. A body that is a SINGLE line which
            // itself opens a quote is peeled ITERATIVELY (so `>`×d on one line can't
            // stack-overflow via recursive `parse`); other bodies parse normally
            // (mldoc's recursion, shallow for real multi-line quotes).
            let span = Some(Span(lines[start].start, lines[i - 1].end));
            out.push(build_org_quote(body, span));
            absorb = true;
            continue;
        }

        // 9. block-level displayed math `$$ … $$`.
        if let Some(math) = displayed_math(t) {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::DisplayedMath { text: math, span: Some(Span(line_start, line_end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 10. raw HTML (single line, complete element).
        if is_raw_html(t) {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::RawHtml { text: t.to_string(), span: Some(Span(line_start, line_end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 11. footnote definition `[fn:name] text` — not a list-item content block
        // (inside an item it stays an inline footnote ref in a paragraph). mldoc's
        // `footnote_definition = many1 l` absorbs the following continuation lines into
        // the def's inline body (joined with Break_Line, de-indented) until a
        // footnote-body terminator (`footnote_cont`); the first line's body comes from
        // `footnote_def` (which is exactly mldoc's first `l`).
        if let Some((name, content)) = footnote_def(t).filter(|_| !in_item) {
            flush_para(&mut out, &mut para, input, in_item);
            // First body line: mldoc `line = take_till1 is_eol` drops a CRLF `\r`.
            let mut body = strip_cr_eol(content, line_has_nl(input, &lines[i])).to_string();
            let mut j = i + 1;
            while let Some(next) = lines.get(j) {
                match footnote_cont(next.text, line_has_nl(input, next)) {
                    Some(c) => {
                        body.push('\n');
                        body.push_str(c);
                        j += 1;
                    }
                    None => break,
                }
            }
            out.push(Block::FootnoteDef {
                name,
                inline: org_inline(&body),
                span: Some(Span(line_start, lines[j - 1].end)),
            });
            absorb = true;
            i = j;
            continue;
        }

        // 12. list — mldoc Org list parser (lists0.ml): multi-line item-continuation
        // folding + the indented-`-` collapse quirk (see `collect_list`). Disabled in
        // list-item content. `collapse_floor` skips list-starts inside a region that
        // already collapsed (linearity). On collapse the region falls through to the
        // paragraph fallback below, which reproduces mldoc's failed-list Paragraph.
        if !in_item && i >= collapse_floor && list_marker(t).is_some() {
            match collect_list(&lines, i) {
                Ok((block, next)) => {
                    flush_para(&mut out, &mut para, input, in_item);
                    out.push(block);
                    absorb = false;
                    i = next;
                    continue;
                }
                Err(Collapse { kept, resume, trigger }) => {
                    collapse_floor = trigger;
                    if let Some(block) = kept {
                        // partial collapse: emit the surviving prefix List, then resume
                        // (the failing item onward falls through to the paragraph path).
                        flush_para(&mut out, &mut para, input, in_item);
                        out.push(block);
                        absorb = false;
                        i = resume;
                        continue;
                    }
                    // full collapse (resume == i == start): fall through to paragraph.
                }
            }
        }

        // 13. horizontal rule (exactly 5 dashes).
        if is_org_hr(t) {
            flush_para(&mut out, &mut para, input, in_item);
            out.push(Block::Hr { span: Some(Span(line_start, line_end)) });
            absorb = false;
            i += 1;
            continue;
        }

        // 13b. block-level Clojure-hiccup `[:tag …]` at BOL (after leading ws). Emitted at
        // the document level AND inside list-item content (mldoc yields a `Hiccup` block in
        // both). The string-aware balanced capture may span lines; the remainder past the
        // `]` re-enters block parsing at BOL (`[:div]x` → [Hiccup, Paragraph x]).
        {
            let lw = leading_ws(t);
            let rec = line_start + lw;
            if last_rbracket.is_some_and(|last| rec <= last) && input[rec..].starts_with("[:") {
                if let Some(cap_end) = crate::inline::parse_hiccup(input, rec) {
                    // A preceding paragraph drops its trailing Break before a Hiccup inside
                    // a blockquote body (mldoc: `> a\n> [:div]` → Quote[Para "a", Hiccup]),
                    // but keeps it at the document level (`a\n[:div]` → Para[a, Break]).
                    let trim = in_item || ORG_IN_QUOTE.with(|c| c.get());
                    flush_para(&mut out, &mut para, input, trim);
                    out.push(Block::Hiccup {
                        v: input[rec..cap_end].to_string(),
                        span: Some(Span(line_start, cap_end)),
                    });
                    absorb = false;
                    // Resume after the `]`, absorbing consecutive eols (mldoc `<* optional
                    // eols`: `[:div]\n\nx` → [Hiccup, Para "x"]). A same-line remainder
                    // (`[:div]x`) keeps its following blanks (only `\n`/`\r` bytes skipped).
                    let bytes = input.as_bytes();
                    let mut resume = cap_end;
                    while resume < bytes.len() && matches!(bytes[resume], b'\n' | b'\r') {
                        resume += 1;
                    }
                    if resume >= bytes.len() {
                        break; // captured to EOF (+ trailing eols)
                    }
                    let mut ri = i;
                    while ri < lines.len() && lines[ri].end <= resume {
                        ri += 1;
                    }
                    if ri >= lines.len() {
                        break; // defensive (resume < len ⇒ unreachable)
                    }
                    if resume > lines[ri].start {
                        let content_end = lines[ri].start + lines[ri].text.len();
                        lines[ri] = Line {
                            start: resume,
                            end: lines[ri].end,
                            text: &input[resume..content_end],
                        };
                    }
                    i = ri;
                    continue;
                }
            }
        }

        // 14. plain line → accumulate into the current paragraph.
        para = Some(match para {
            Some((s, _)) => (s, line_end),
            None => (line_start, line_end),
        });
        absorb = false;
        i += 1;
    }

    flush_para(&mut out, &mut para, input, false);
    out
}

/// Flush the open paragraph. `trim_eol` drops trailing newline(s) from the slice
/// (so no trailing `Break_Line`): in list-item content (`in_item`) a *following block*
/// absorbs the paragraph's trailing eols via mldoc's `between_eols` (its block parsers
/// are tried before `Paragraph.sep`), whereas at the document level `Paragraph.sep`
/// claims the eol first and it stays a Break. EOF / end-of-content flushes pass `false`.
fn flush_para(out: &mut Vec<Block>, para: &mut Option<(usize, usize)>, input: &str, trim_eol: bool) {
    if let Some((s, mut e)) = para.take() {
        if trim_eol {
            while e > s && matches!(input.as_bytes()[e - 1], b'\n' | b'\r') {
                e -= 1;
            }
        }
        out.push(Block::Paragraph {
            inline: org_inline(&input[s..e]),
            span: Some(Span(s, e)),
        });
    }
}

/// Split into lines on any of `\r\n`, lone `\n`, or lone `\r` (mldoc `is_eol` treats
/// `\r` and `\n` each as a terminator; CRLF is consumed as ONE). The `text` excludes
/// the terminator, so no trailing `\r` reaches block content; paragraph bodies are
/// re-extracted from the raw span and the inline parser restores per-eol breaks.
fn split_lines(input: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let bytes = input.as_bytes();
    let n = input.len();
    let mut i = 0;
    while i < n {
        let start = i;
        let mut j = i;
        while j < n && bytes[j] != b'\n' && bytes[j] != b'\r' {
            j += 1;
        }
        let content_end = j;
        let end = if j < n {
            if bytes[j] == b'\r' && j + 1 < n && bytes[j + 1] == b'\n' {
                j + 2
            } else {
                j + 1
            }
        } else {
            j
        };
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
    // `key.get(..6)` not `key[..6]`: a directive key is user text, so a multibyte char
    // straddling byte 6 (`#+END_中:`) would panic on a raw slice. char-boundary-safe.
    if key.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("begin_")) {
        return None;
    }
    let value = rest[pos + 1..].trim_start();
    Some((key.to_string(), value.to_string()))
}

/// Org comment `# text` (mldoc `Comment`): optional leading ws, a single `#`, then
/// ≥1 space/tab, then non-empty content (leading spaces stripped, **trailing kept**).
/// `#c` (no space), `# ` (empty), `##…` (two hashes), `#+…` (directive) are NOT comments.
fn org_comment(s: &str) -> Option<&str> {
    let rest = s.trim_start().strip_prefix('#')?;
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None; // `##…`, `#+…`, `#c` — second char must be a space/tab
    }
    let content = rest.trim_start_matches([' ', '\t']);
    if content.is_empty() {
        return None; // `# ` with nothing after
    }
    Some(content)
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

/// First `:END:` line after `from`, via the sparse `:END:` index (binary search ⇒ O(log n)).
fn find_drawer_end(drawer_end_idxs: &[usize], from: usize) -> Option<usize> {
    drawer_end_idxs.get(drawer_end_idxs.partition_point(|&x| x <= from)).copied()
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

/// Does an org headline whose post-marker CONTENT is `content` (a non-empty,
/// left-trimmed suffix of the headline line at byte `content_off` in `input`, the line
/// being `lines[i]`) split into `[empty bullet, block]`? True iff reparsing CONTENT (+
/// the following lines) as a column-0 block yields a *real block* — i.e. anything other
/// than the Paragraph / Comment / List / Heading fallbacks that mldoc keeps as (or after)
/// the heading title. Mirrors mldoc's Org heading-title lookahead (heading0.ml).
///
/// Single-line / always-terminating openers (`#+KEY:` directive, any `:`-line → Drawer
/// or Example, `| … |` table, `\begin{}` latex env — which consumes to EOF when unclosed,
/// `> ` quote, `$$…$$`, complete `<tag>…</tag>` html, valid `[fn:n] body`, `-----` hr)
/// always produce their block, so they split unconditionally. A `#+BEGIN_X` block or a
/// markdown ```/~~~ fence only becomes a block when it CLOSES; an unclosed one reparses
/// as a Paragraph, so mldoc keeps it as the title (`* #+BEGIN_SRC\nx` → Heading titled
/// `#+BEGIN_SRC`) — hence the explicit close gate (`find_block_end` for blocks, the fence
/// cursor for fences). Comment (`# x`), list (`- `/`+ `/`N. `) and nested-headline content
/// match none of these.
fn headline_split_opener(
    content: &str,
    input: &str,
    content_off: usize,
    lines: &[Line],
    i: usize,
    end_idxs: &[usize],
    no_block_end: &mut std::collections::HashSet<String>,
    fence_line_idxs: &std::collections::HashMap<u8, Vec<usize>>,
    fence_cursor: &mut std::collections::HashMap<u8, usize>,
) -> bool {
    if directive(content).is_some()
        || is_verbatim_line(content)
        || is_table_row(content)
        || crate::inline::parse_latex_env(input, content_off, content_off + content.len()).is_some()
        || quote_opens(content)
        || displayed_math(content).is_some()
        || is_raw_html(content)
        || footnote_def(content).is_some()
        || is_org_hr(content)
    {
        return true;
    }
    // A `#+BEGIN_X` block / ```|~~~ fence only splits when it CLOSES. The block-name search
    // (`find_block_end`) scans `lines[i+1..]`, so it carries a per-name "no `#+END_` ahead"
    // memo to keep a run of repeated UNCLOSED openers linear; the fence test is the
    // monotone-cursor finder.
    if let Some(name) = block_begin(content) {
        return find_block_end(end_idxs, lines, no_block_end, i, &name).is_some();
    }
    if let Some((ch, _)) = fence_marker(content) {
        return find_matching_fence(fence_line_idxs, fence_cursor, i, ch).is_some();
    }
    false
}

/// First whole-line fence-marker of char `fchar` strictly after `from`, via the per-char
/// sorted index + a MONOTONE per-char cursor (O(1) amortized, never an EOF re-scan). The
/// single closer-finder for BOTH a top-level fence opener (step 5) and one in headline content
/// (step 3, e.g. `* ```` — not itself an index entry). On-demand at the dispatch point, so it
/// is context-aware (it never pairs across a block/drawer body the loop already jumped). The
/// loop reaches fence openers in increasing `from`, so the cursor only advances → O(n) total.
fn find_matching_fence(
    fence_line_idxs: &std::collections::HashMap<u8, Vec<usize>>,
    cursor: &mut std::collections::HashMap<u8, usize>,
    from: usize,
    fchar: u8,
) -> Option<usize> {
    let v = fence_line_idxs.get(&fchar)?;
    let cur = cursor.entry(fchar).or_insert(0);
    while *cur < v.len() && v[*cur] <= from {
        *cur += 1;
    }
    v.get(*cur).copied()
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

/// First line after `from` whose trimmed start is `#+END_<name>` (prefix match — mldoc-
/// exact, so trailing junk after the name still closes), via the sparse `#+END_` index +
/// a per-name "absent from here on" memo. A run of unclosed / `#+END_`-mismatched openers
/// stays linear (audit R2-P4) instead of re-scanning to EOF per opener. Shared by the main
/// loop (step 6) and the headline block-opener split.
fn find_block_end(
    end_idxs: &[usize],
    lines: &[Line],
    no_block_end: &mut std::collections::HashSet<String>,
    from: usize,
    name: &str,
) -> Option<usize> {
    let key = name.to_ascii_lowercase();
    if no_block_end.contains(&key) {
        return None;
    }
    let needle = format!("#+END_{}", name);
    let start = end_idxs.partition_point(|&x| x <= from);
    for &idx in &end_idxs[start..] {
        let t = lines[idx].text.trim_start();
        if t.get(..needle.len()).is_some_and(|p| p.eq_ignore_ascii_case(&needle)) {
            return Some(idx);
        }
    }
    no_block_end.insert(key);
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
    quote_first_line(s).is_some()
}

/// A de-`>`'d line content that ENDS an Org blockquote run (it starts a new block:
/// list / heading / `id::`). On the FIRST line such content also makes mldoc reject
/// the quote outright (→ Paragraph), not just stop the run.
fn quote_line_breaker(s: &str) -> bool {
    s.starts_with("- ")
        || s.starts_with("# ")
        || s.starts_with("id:: ")
        || s == "-"
        || s == "#"
}

/// First line of an Org blockquote. mldoc enters the quote by stripping one leading `>`
/// (+ws); the remainder is itself a body line that drops one MORE `>` (+ws) — i.e. up
/// to TWO `>` on the opener (so N leading `>` ultimately nest ⌈N/2⌉ Quotes). The quote
/// OPENS only if the result is non-empty and does not start a block construct (a
/// list/heading/`id::` marker makes mldoc reject the quote entirely, leaving the raw
/// line a Paragraph). Returns the first body-line content, else None.
fn quote_first_line(s: &str) -> Option<String> {
    let r1 = s.trim_start().strip_prefix('>')?.trim_start();
    let content = match r1.strip_prefix('>') {
        Some(r2) => r2.trim_start(),
        None => r1,
    };
    if content.is_empty() || quote_line_breaker(content) {
        return None;
    }
    Some(content.to_string())
}

/// One CONTINUATION line of an Org blockquote body (mldoc strips ONE `>` + ws, lazy:
/// a non-`>` line still continues). Returns None to STOP the run (blank line, or a line
/// that — after stripping one `>` — starts a new block construct).
fn quote_line_content(s: &str) -> Option<String> {
    let t = s.trim_start();
    let had_gt = t.starts_with('>');
    let rest = if had_gt { t[1..].trim_start() } else { t };
    if rest.is_empty() {
        return if had_gt { Some(String::new()) } else { None };
    }
    if quote_line_breaker(rest) {
        return None;
    }
    Some(rest.to_string())
}

/// Build a (possibly nested) Org Quote from an already de-`>`'d body. When the body is
/// a SINGLE line that itself opens a quote, peel levels ITERATIVELY — so `>`×d on one
/// line nests ⌈d/2⌉ Quotes WITHOUT recursing `parse` (no stack overflow). Other bodies
/// parse normally (mldoc's recursion, shallow for real multi-line quotes; any deep
/// single-line quote nested inside is again caught by this peel).
fn build_org_quote(body: String, span: Option<Span>) -> Block {
    // `base` = how deeply we are ALREADY nested (across recursive `parse` of multi-line
    // quote bodies). Combined with the single-line peel below, this bounds TOTAL nesting
    // at `QUOTE_NEST_CAP` regardless of how the `>`s are split across lines.
    let base = ORG_QUOTE_DEPTH.with(|c| c.get());
    let mut depth = 1usize;
    let mut cur = body;
    // The innermost children. Filled either by peeling out (then `parse`d once) or by
    // hitting the depth cap (then the remaining text is emitted as a plain Paragraph).
    let children = loop {
        let trimmed = cur.strip_suffix('\n').unwrap_or(&cur);
        if base + depth >= QUOTE_NEST_CAP {
            // Beyond this depth mldoc itself stack-overflows (no comparable output
            // exists), so stop nesting and keep the rest as one Paragraph — purely to
            // avoid a deep recursive walk/serialize/drop of the result, which would
            // SIGABRT. Real / fuzz inputs never reach this.
            break vec![Block::Paragraph {
                inline: org_inline(trimmed),
                span,
            }];
        }
        if trimmed.contains('\n') {
            // Multi-line body: parse normally (mldoc's recursion), but tracking depth so
            // a deep `>` first line that re-enters here stays bounded.
            break parse_nested_quote_body(&cur, base + depth);
        }
        match quote_first_line(trimmed) {
            Some(inner) => {
                cur = inner + "\n";
                depth += 1;
            }
            None => break parse_nested_quote_body(&cur, base + depth),
        }
    };
    let mut block = Block::Quote { children, span };
    for _ in 1..depth {
        block = Block::Quote { children: vec![block], span };
    }
    block
}

/// Parse a multi-line blockquote body, recording the current nesting depth so a deep
/// `>` line inside it re-enters `build_org_quote` already aware of how deep we are.
fn parse_nested_quote_body(body: &str, depth: usize) -> Vec<Block> {
    let prev = ORG_QUOTE_DEPTH.with(|c| c.replace(depth));
    let prev_q = ORG_IN_QUOTE.with(|c| c.replace(true)); // suppress headlines (C2)
    let r = parse(body);
    ORG_IN_QUOTE.with(|c| c.set(prev_q));
    ORG_QUOTE_DEPTH.with(|c| c.set(prev));
    r
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
/// whitespace is allowed (mldoc). mldoc (`footnote.ml`): after `[fn:name]` + spaces,
/// the body is `many1 l` where `l = spaces *> satisfy non_eol >>= fun c -> line` —
/// i.e. (1) the first body char must NOT begin a block construct (`* # [ -`, also
/// `\r \n`), and (2) `line = take_till1 is_eol` requires **≥1 more char** after that
/// first char, so a single-byte body fails. So `[fn:1] ab`/`[fn:1]:x`/`[fn:1]/x` →
/// Footnote_Definition, but `[fn:1] a` (1-byte body), `[fn:1]` (bare ref), `[fn:1]  a`
/// (still 1-byte after spaces) and `[fn:1]*x`/`[fn:1]-x`/`[fn:1]#x`/`[fn:1][x` (bad
/// first char) are inline footnote refs inside a Paragraph.
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
    // mldoc `satisfy non_eol` (1 byte) then `take_till1 is_eol` (≥1 byte): the body
    // (after leading spaces) needs at least 2 bytes, else it is just an inline ref.
    if content.len() < 2 {
        return None;
    }
    Some((name.to_string(), content))
}

/// Was this `Line` terminated by a `\n` in the source (vs. ending at EOF)? `Line.end`
/// points just past the trailing `\n` when present, so the last byte of the span is the
/// newline. Used to tell a CRLF `\r\n` ending (drop the `\r`) from a dangling `\r`.
fn line_has_nl(input: &str, line: &Line) -> bool {
    line.end > line.start && input.as_bytes()[line.end - 1] == b'\n'
}

/// mldoc `line = take_till1 is_eol`: the body stops at the first `\r`/`\n`. The line text
/// has no `\n` (split on it), so this only drops a trailing CRLF `\r` (present iff the
/// line ended in `\r\n`, i.e. `followed_by_nl`). A lone trailing `\r` with no `\n` can't
/// reach here from a matched `footnote_def` first line.
fn strip_cr_eol(s: &str, followed_by_nl: bool) -> &str {
    if followed_by_nl {
        s.strip_suffix('\r').unwrap_or(s)
    } else {
        s
    }
}

/// A continuation line of an Org footnote-definition body — mldoc's `footnote_definition`
/// `l = spaces *> satisfy non_eol >>= fun c -> line <* (end_of_input <|> end_of_line)`,
/// where this `non_eol` is the footnote-SPECIFIC predicate (`\r \n - * # [` all false).
/// Returns the de-indented body slice (leading `space_chars` stripped, trailing CRLF `\r`
/// dropped) iff the line is absorbed into the body, else `None` for a terminator:
///   - blank / whitespace-only line             → `satisfy` hits the eol/EOF
///   - first non-space byte in `- * # [`         → footnote `non_eol` rejects it
///   - < 2 bytes before the eol (1-byte body)    → `line = take_till1` needs ≥1 more byte
///   - an embedded/lone `\r` not ending `\r\n`   → `end_of_input <|> end_of_line` fails
/// All checks are byte-oriented (angstrom is byte-oriented), matching `footnote_def`.
/// `followed_by_nl` marks a real `\r\n` ending vs. a dangling `\r`.
fn footnote_cont(text: &str, followed_by_nl: bool) -> Option<&str> {
    let b = text.as_bytes();
    // mldoc `spaces` = skip `space_chars` [' '; '\t'; '\026'; '\012'].
    let mut s = 0;
    while s < b.len() && matches!(b[s], b' ' | b'\t' | 0x0C | 0x1A) {
        s += 1;
    }
    let rest = &b[s..];
    // `satisfy non_eol`: a first byte must exist and not be in the terminator set (which
    // also excludes `\r`/`\n`, so a blank / whitespace-only line is rejected here).
    let first = *rest.first()?;
    if matches!(first, b'-' | b'*' | b'#' | b'[' | b'\r' | b'\n') {
        return None;
    }
    // `line = take_till1 is_eol`: content runs to the first `\r` (no `\n` in line text).
    let cr = rest.iter().position(|&c| c == b'\r');
    let core_len = cr.unwrap_or(rest.len());
    if core_len < 2 {
        return None; // 1-byte body: `take_till1` fails after the satisfy'd char.
    }
    // `<* (end_of_input <|> end_of_line)`: an interior `\r` must be the final byte AND a
    // real `\r\n` ending; a mid-line or dangling `\r` makes `end_of_line` fail.
    if let Some(p) = cr {
        if p != rest.len() - 1 || !followed_by_nl {
            return None;
        }
    }
    // Byte-safe: `s` and `s + core_len` fall on ASCII boundaries (space_chars / `\r`).
    Some(&text[s..s + core_len])
}

/// A parsed Org list marker (mldoc `format_checkbox_parser` + the first content line).
struct Marker {
    ordered: bool,
    number: Option<u32>,
    checkbox: Option<bool>,
    indent: usize,
    /// The raw content after marker + ws + checkbox + spaces (trim_start'd), i.e. the
    /// first item-content line BEFORE the final `String.trim` mldoc applies at join.
    body: String,
}

/// Parse an Org list marker at the line's own indent (mldoc `format_checkbox_parser`,
/// indent-aware): col-0 → `- `/`+ `/`N. `, indent>0 → `* `/`+ `/`N. ` (`-` is a
/// bullet ONLY at column 0; `*` ONLY when indented — a col-0 `* x` is a headline).
/// Requires a marker + ≥1 space and **non-empty content** after any checkbox (mldoc's
/// `take_till1` needs ≥1 char) — a bare `- `/`+ `/`1. `/`- [ ]` yields None.
fn list_marker(s: &str) -> Option<Marker> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    let mk = |ordered, number, content: &str| {
        let (checkbox, body) = split_checkbox(content);
        if body.trim().is_empty() {
            return None;
        }
        Some(Marker { ordered, number, checkbox, indent: ws, body: body.to_string() })
    };
    let dash = if ws == 0 { rest.strip_prefix('-') } else { None };
    let star = if ws > 0 { rest.strip_prefix('*') } else { None };
    if let Some(after) = dash.or(star).or_else(|| rest.strip_prefix('+')) {
        if after.starts_with(' ') || after.starts_with('\t') {
            return mk(false, None, after.trim_start());
        }
    }
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 {
        if let Some(after) = rest[digits..].strip_prefix('.') {
            if after.starts_with(' ') || after.starts_with('\t') {
                if let Ok(number) = rest[..digits].parse::<u32>() {
                    return mk(true, Some(number), after.trim_start());
                }
            }
        }
    }
    None
}

/// mldoc `check_listitem` (Org): `(indent, is_item)`. `is_item` marks a line as a
/// *list-item shape* for the continuation logic — NOTE this is broader than a
/// parseable marker: a leading integer (`Scanf "%d"`, even `12abc`/`-5`) is `is_item`
/// regardless of a following `.`, and `- ` is `is_item` at ANY indent. The mismatch
/// between this and `list_marker` (which fails on `-` at indent>0, on `N` without `.`,
/// and on empty content) is exactly what drives the collapse. (is_heading is folded
/// into the caller's col-0 / `headline_level` handling, so not returned.)
fn check_listitem(line: &str) -> (usize, bool) {
    let indent = leading_ws(line);
    if scan_leading_int(line.trim()) {
        return (indent, true);
    }
    let b = line.as_bytes();
    if b.len() - indent >= 2 {
        let (p0, p1) = (b[indent], b[indent + 1]);
        let is_item = (p0 == b'+' && p1 == b' ')
            || (p0 == b'-' && p1 == b' ')
            || (indent != 0 && p0 == b'*' && p1 == b' ');
        (indent, is_item)
    } else {
        (indent, false)
    }
}

/// mldoc `Scanf.sscanf (String.trim line) "%d"`: does the (already-trimmed) string
/// begin with an integer (optional `+`/`-` then ≥1 digit)?
fn scan_leading_int(t: &str) -> bool {
    let b = t.as_bytes();
    let i = if matches!(b.first(), Some(b'+' | b'-')) { 1 } else { 0 };
    b.get(i).is_some_and(u8::is_ascii_digit)
}

/// A (possibly partial) list collapse: mldoc's recursive list parser failed on a bad
/// continuation. `kept` is the `List` of items parsed before the failing item (None if
/// none survive — a full collapse); `resume` is the line the document parser resumes at
/// (the failing item's marker, re-parsed as a Paragraph); `trigger` memoises the
/// collapse region for the caller (linearity).
struct Collapse {
    kept: Option<Block>,
    resume: usize,
    trigger: usize,
}

/// Collect an Org list starting at line `start` (faithful port of mldoc lists0.ml).
/// Each item folds its indented multi-line continuation (de-indented via `String.trim`,
/// re-parsed with the list-item content parser, `parse_doc(.., true)`); deeper
/// is-item lines become children via the flat sequence + `nest_items`.
///
/// COLLAPSE: an indented continuation that is a list-item shape (`check_listitem`)
/// deeper than the current item but NOT a parseable marker there (`list_marker` None —
/// an indented `- `, a `N`-no-`.`, or an empty marker) makes the item's child
/// `list_parser` fail. In mldoc that failure bubbles up the recursion through every
/// item that is *first at its level*, terminating at (and keeping) the first ancestor
/// level that has a prior sibling; the failing item onward re-parses as a Paragraph.
/// `collapse_resume` reproduces that bubble from the flat indent sequence.
fn collect_list(lines: &[Line], start: usize) -> Result<(Block, usize), Collapse> {
    let mut flat: Vec<ListItem> = Vec::new();
    let mut flat_lines: Vec<usize> = Vec::new();
    let mut flat_indents: Vec<u32> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let t = lines[i].text;
        // terminators at a would-be marker position: blank line, a col-0 headline, or
        // any non-marker line (mldoc heading-lookahead / `format_checkbox` failure).
        if t.is_empty() || headline_level(t).is_some() {
            break;
        }
        let marker = match list_marker(t) {
            Some(m) => m,
            None => break,
        };
        let cur_indent = marker.indent;
        // content = first line (after marker) + folded indented continuation lines.
        let mut content_lines: Vec<String> = vec![marker.body.trim().to_string()];
        let mut j = i + 1;
        let mut trigger: Option<usize> = None;
        loop {
            if j >= lines.len() {
                break; // EOF ends this item's content
            }
            let cl = lines[j].text;
            if cl.is_empty() {
                j += 1; // mldoc `two_eols`: a blank ends the content AND is consumed
                break;
            }
            let (ci, is_item) = check_listitem(cl);
            if ci == 0 {
                break; // a col-0 line ends the content (left for the outer loop)
            }
            if is_item {
                if ci > cur_indent && list_marker(cl).is_none() {
                    trigger = Some(j); // COLLAPSE trigger (deeper unparseable marker)
                }
                break; // child / breakout / collapse — handled below
            }
            content_lines.push(cl.trim().to_string()); // fold (de-indented)
            j += 1;
        }
        if let Some(trigger) = trigger {
            // The failing item P is the one at line `i` (indent `cur_indent`), NOT pushed.
            let r = collapse_resume(&flat_indents, cur_indent as u32);
            let resume = if r < flat_lines.len() { flat_lines[r] } else { i };
            flat.truncate(r);
            let kept = if flat.is_empty() {
                None
            } else {
                let items = std::mem::take(&mut flat);
                Some(Block::List {
                    items: crate::projection::nest_items(items),
                    span: Some(Span(lines[start].start, lines[resume - 1].end)),
                })
            };
            return Err(Collapse { kept, resume, trigger });
        }
        flat.push(ListItem {
            ordered: marker.ordered,
            number: marker.number,
            indent: cur_indent as u32,
            content: parse_doc(&content_lines.join("\n"), true),
            items: vec![],
            name: vec![],
            checkbox: marker.checkbox,
        });
        flat_lines.push(i);
        flat_indents.push(cur_indent as u32);
        i = j;
    }
    if flat.is_empty() {
        // defensive: caller gates on `list_marker`, so unreachable.
        return Err(Collapse { kept: None, resume: start, trigger: start });
    }
    let span = Some(Span(lines[start].start, lines[i - 1].end));
    Ok((Block::List { items: crate::projection::nest_items(flat), span }, i))
}

/// Given the indents of the successfully-collected list items and the indent of the
/// failing item P (conceptually at index `flat_indents.len()`), return the flat index
/// `r` such that items `[0, r)` are kept and the resume point is item `r`'s marker
/// (or P's marker if `r == flat_indents.len()`). Walks up while the current item is the
/// *first at its level* (its nearest shallower-or-equal predecessor is strictly
/// shallower — a parent, not a prior sibling), matching mldoc's failure bubble-up.
fn collapse_resume(flat_indents: &[u32], p_indent: u32) -> usize {
    let mut cur_indent = p_indent;
    let mut cur_index = flat_indents.len();
    loop {
        // nearest earlier item with indent <= cur_indent.
        let q = (0..cur_index).rev().find(|&j| flat_indents[j] <= cur_indent);
        match q {
            None => return cur_index,                              // first item overall
            Some(j) if flat_indents[j] == cur_indent => return cur_index, // prior sibling
            Some(j) => {
                cur_index = j; // a parent → bubble up
                cur_indent = flat_indents[j];
            }
        }
    }
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
        t.split('|').map(|c| org_inline(c.trim())).collect()
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

/// Block-body inline seam: the v0.2 `org_resolver`. Name kept for the block call sites.
pub(crate) fn org_inline(text: &str) -> Vec<Inline> {
    crate::org_resolver::parse_inline_org(text)
}


// ---- inline helpers -------------------------------------------------------

/// `<scheme:rest>` autolink (mldoc `quick_link`): scheme letters/digits, `:`, optional
/// `//`, then non-space rest; ANY `:` makes it a link (so `<a:b>` works).
pub(crate) fn parse_org_autolink(s: &str, at: usize) -> Option<(usize, Inline)> {
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
pub(crate) fn classify_org_link_1(url_text: &str, label_text: &str) -> Url {
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
pub(crate) fn classify_org_link_2(name: &str) -> Url {
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
            Inline::Hiccup { v } => format!("hiccup({v})"),
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
                Block::Comment { .. } => "comment",
                Block::Example { .. } => "example",
                Block::LatexEnv { .. } => "latex_env",
                Block::Hiccup { .. } => "hiccup",
            })
            .collect()
    }

    #[test]
    fn org_comment_block() {
        assert_eq!(bkinds("# c"), ["comment"]);
        assert_eq!(bkinds("  # indented"), ["comment"]);
        assert_eq!(bkinds("#c"), ["paragraph"]); // no space after #
        assert_eq!(bkinds("# "), ["paragraph"]); // empty content
        assert_eq!(bkinds("##  two"), ["paragraph"]); // two hashes
        assert_eq!(bkinds("#+TITLE: x"), ["directive"]); // #+ is a directive
        assert_eq!(bkinds("# a\n# b"), ["comment", "comment"]);
        assert_eq!(bkinds("- a\n# c"), ["list", "comment"]); // col-0 # terminates the list
        // content: leading spaces stripped, trailing kept; not inline-parsed.
        match &parse("   # x  ")[0] {
            Block::Comment { text, .. } => assert_eq!(text, "x  "),
            _ => panic!("expected Comment"),
        }
    }

    #[test]
    fn org_hiccup() {
        // block-level (whole line) and not-a-tag.
        assert_eq!(bkinds("[:div]"), ["hiccup"]);
        assert_eq!(bkinds("[:foo]"), ["paragraph"]);
        assert_eq!(bkinds("[:div]x"), ["hiccup", "paragraph"]);
        assert_eq!(bkinds("[:div][:span]"), ["hiccup", "hiccup"]);
        // shielded constructs win; recognized inside list-item content.
        assert_eq!(bkinds("* [:div]"), ["bullet"]); // headline (inline-hiccup title)
        assert_eq!(bkinds("#+BEGIN_SRC\n[:div]\n#+END_SRC"), ["src"]);
        match &parse("- [:div]")[0] {
            Block::List { items, .. } => assert!(matches!(items[0].content[0], Block::Hiccup { .. })),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn org_hiccup_runs_terminate() {
        let _ = parse(&"[:div ".repeat(20000));
        let _ = parse(&"[:a]".repeat(20000));
    }

    // ---- headlines --------------------------------------------------------

    #[test]
    fn render_target_checkbox_orglink_metadata() {
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

    // ---- headline block-opener split (mldoc heading-title lookahead) -------

    #[test]
    fn headline_split_openers() {
        // a headline whose post-marker CONTENT is a block construct splits into
        // [empty bullet, <block>] — the org analog of the md `-` bullet-opener split.
        assert_eq!(bkinds("* #+TITLE: x"), ["bullet", "directive"]);
        assert_eq!(bkinds("* :PROPERTIES:\n:a: b\n:END:"), ["bullet", "properties"]);
        assert_eq!(bkinds("* :LOGBOOK:\nx\n:END:"), ["bullet", "drawer"]);
        assert_eq!(bkinds("* :NAME:"), ["bullet", "example"]); // bare drawer → verbatim
        assert_eq!(bkinds("* : text"), ["bullet", "example"]);
        assert_eq!(bkinds("* #+BEGIN_SRC\ncode\n#+END_SRC"), ["bullet", "src"]);
        assert_eq!(bkinds("* #+BEGIN_QUOTE\nq\n#+END_QUOTE"), ["bullet", "quote"]);
        assert_eq!(bkinds("* #+BEGIN_FOO\nf\n#+END_FOO"), ["bullet", "custom"]);
        assert_eq!(bkinds("* | a | b |"), ["bullet", "table"]);
        assert_eq!(bkinds("* | a | b |\n| c | d |"), ["bullet", "table"]);
        assert_eq!(bkinds("* > quote"), ["bullet", "quote"]);
        assert_eq!(bkinds("* $$x$$"), ["bullet", "displayed_math"]);
        assert_eq!(bkinds("* <div>x</div>"), ["bullet", "raw_html"]);
        assert_eq!(bkinds("* [fn:1] body"), ["bullet", "footnote_def"]);
        assert_eq!(bkinds("* -----"), ["bullet", "hr"]);
        assert_eq!(bkinds("* \\begin{x}\ny\n\\end{x}"), ["bullet", "latex_env"]);
        assert_eq!(bkinds("* \\begin{x}"), ["bullet", "latex_env"]); // latex consumes to EOF
        assert_eq!(bkinds("* ```\ncode\n```"), ["bullet", "src"]); // markdown fence
        assert_eq!(bkinds("* ~~~\nx\n~~~"), ["bullet", "src"]);
    }

    #[test]
    fn headline_split_keeps_marker_priority_level_empty_title() {
        // the empty bullet KEEPS level/marker/priority but has an empty title + no htags.
        match &parse("*** TODO [#A] #+TITLE: x")[0] {
            Block::Bullet { level, marker, priority, inline, htags, .. } => {
                assert_eq!(*level, 3);
                assert_eq!(marker.as_deref(), Some("TODO"));
                assert_eq!(priority.as_deref(), Some("A"));
                assert!(inline.is_empty());
                assert!(htags.is_empty());
            }
            _ => panic!("expected empty Bullet"),
        }
        // trailing `:tag:` folds into the directive value (no htags on the bullet).
        match &parse("* #+TITLE: x :a:b:")[1] {
            Block::Directive { name, value, .. } => {
                assert_eq!(name, "TITLE");
                assert_eq!(value, "x :a:b:");
            }
            _ => panic!("expected Directive"),
        }
    }

    #[test]
    fn headline_split_non_splitters() {
        // comment / list / nested headline / plain / tag / bare-marker content stays a
        // single (non-split) headline.
        assert_eq!(bkinds("* # comment"), ["bullet"]);
        assert_eq!(bkinds("* TODO task"), ["bullet"]);
        assert_eq!(bkinds("* #tag x"), ["bullet"]);
        assert_eq!(bkinds("* - item"), ["bullet"]);
        assert_eq!(bkinds("* 1. item"), ["bullet"]);
        assert_eq!(bkinds("* ** x"), ["bullet"]);
        assert_eq!(bkinds("* plain title"), ["bullet"]);
        // an UNCLOSED #+BEGIN / fence is NOT a block ⇒ stays the heading title.
        assert_eq!(bkinds("* #+BEGIN_SRC\ncode"), ["bullet", "paragraph"]);
        assert_eq!(bkinds("* ```\nx"), ["bullet", "paragraph"]);
        // a short/invalid footnote body is an inline ref, not a definition.
        assert_eq!(bkinds("* [fn:1] a"), ["bullet"]);
        // bare empty headline (no split, existing behavior).
        assert_eq!(bkinds("*"), ["bullet"]);
    }

    #[test]
    fn headline_split_following_blocks() {
        // the split block absorbs following blanks / continues paragraphs like a col-0
        // block, and adjacent headlines are unaffected.
        assert_eq!(bkinds("* #+TITLE: x\n\ny"), ["bullet", "directive", "paragraph"]);
        assert_eq!(bkinds("* #+TITLE: x\n* Second"), ["bullet", "directive", "bullet"]);
        assert_eq!(bkinds("* :PROPERTIES:\n:a: b\n:END:\n#+FOO: bar"), ["bullet", "properties"]);
    }

    // ---- links ------------------------------------------------------------

    #[test]
    fn links() {
        // page ref produces a ref; labelled link does not over-extract
        let r = crate::refs::extract_refs(&parse("[[target]] and [[b][c]]"), "org");
        assert_eq!(r.page, vec!["target".to_string()]);
    }

    // ---- timestamps -------------------------------------------------------

    #[test]
    fn timestamps() {
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

    // ---- robustness -------------------------------------------------------

    #[test]
    fn latex_entities_and_environment_org() {
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

    // ---- multi-line list continuation + collapse (mldoc lists0.ml) -----------

    /// Block kinds of a single list item's `content`.
    fn item_content_kinds(s: &str) -> Vec<&'static str> {
        match &parse(s)[0] {
            Block::List { items, .. } => items[0]
                .content
                .iter()
                .map(|b| match b {
                    Block::Paragraph { .. } => "paragraph",
                    Block::Quote { .. } => "quote",
                    Block::Example { .. } => "example",
                    Block::Table { .. } => "table",
                    Block::Hr { .. } => "hr",
                    Block::DisplayedMath { .. } => "displayed_math",
                    Block::Src { .. } => "src",
                    _ => "other",
                })
                .collect(),
            b => panic!("not a list: {b:?}"),
        }
    }

    #[test]
    fn list_item_continuation_folds() {
        // an indented (>=1 space / tab) non-marker line folds into the item content,
        // de-indented (String.trim) and joined with Break_Line.
        let para_inline = |s: &str| match &parse(s)[0] {
            Block::List { items, .. } => match &items[0].content[0] {
                Block::Paragraph { inline, .. } => inline.clone(),
                b => panic!("not a paragraph: {b:?}"),
            },
            b => panic!("not a list: {b:?}"),
        };
        let plains: Vec<String> = para_inline("- a\n  more")
            .iter()
            .filter_map(|i| match i {
                Inline::Plain { text } => Some(text.clone()),
                Inline::Break => Some("⏎".into()),
                _ => None,
            })
            .collect();
        assert_eq!(plains, ["a", "⏎", "more"]);
        // fold predicate: >=1-space indent folds; col-0 does NOT.
        assert_eq!(bkinds("- a\n  more"), ["list"]);
        assert_eq!(bkinds("- a\n more"), ["list"]);
        assert_eq!(bkinds("- a\nmore"), ["list", "paragraph"]);
        assert_eq!(bkinds("- a\n\tmore"), ["list"]); // tab indent
        assert_eq!(bkinds("- a\n  m1\n  m2"), ["list"]);
        assert_eq!(bkinds("+ a\n  more"), ["list"]);
        assert_eq!(bkinds("1. a\n   more"), ["list"]);
        assert_eq!(bkinds("- [ ] a\n  more"), ["list"]);
        assert_eq!(bkinds("  + x\n    more"), ["list"]); // list starting at indent>0
        // blank-line handling (mldoc two_eols): one blank between items is absorbed;
        // a blank right after the marker breaks the fold.
        assert_eq!(bkinds("- a\n  more\n\n- b"), ["list"]);
        assert_eq!(bkinds("- a\n\n  more"), ["list", "paragraph"]);
        assert_eq!(bkinds("- a\n b\nc"), ["list", "paragraph"]);
        assert_eq!(bkinds("- a\n\n\nb"), ["list", "paragraph"]);
        // col-0 terminators end the list, the next block re-parses normally.
        assert_eq!(bkinds("- a\n  more\n* head"), ["list", "bullet"]);
        assert_eq!(bkinds("- a\n  more\n#+TITLE: x"), ["list", "directive"]);
        assert_eq!(bkinds("- a\n  more\n-----"), ["list", "hr"]);
    }

    #[test]
    fn list_item_content_reparses_blocks() {
        // indented constructs fold as the item's content BLOCKS, re-parsed with the
        // list-item content parser (no Directive/Drawer/Heading/Footnote/List).
        assert_eq!(item_content_kinds("- a\n  > quote"), ["paragraph", "quote"]);
        assert_eq!(item_content_kinds("- a\n  : ex"), ["paragraph", "example"]);
        assert_eq!(item_content_kinds("- a\n  | t |"), ["paragraph", "table"]);
        assert_eq!(item_content_kinds("- a\n  -----"), ["paragraph", "hr"]);
        assert_eq!(item_content_kinds("- a\n  $$x$$"), ["paragraph", "displayed_math"]);
        assert_eq!(
            item_content_kinds("- a\n  #+BEGIN_SRC\n  x\n  #+END_SRC"),
            ["paragraph", "src"]
        );
        // drawer → verbatim Example (drawer parser not in item content); directive,
        // headline, footnote, indented `---` stay inside the paragraph.
        assert_eq!(
            item_content_kinds("- a\n  :PROPERTIES:\n  :p: 1\n  :END:"),
            ["paragraph", "example"]
        );
        assert_eq!(item_content_kinds("- a\n  #+TITLE: x"), ["paragraph"]);
        assert_eq!(item_content_kinds("- a\n  [fn:1] body"), ["paragraph"]);
        assert_eq!(item_content_kinds("- a\n  ---"), ["paragraph"]);
        // a marker body that itself looks like a marker is plain content (no nesting).
        assert_eq!(item_content_kinds("- - x"), ["paragraph"]);
        assert_eq!(item_content_kinds("- * x"), ["paragraph"]);
    }

    #[test]
    fn list_indented_dash_collapses() {
        // an indented `-` (or other deeper-but-unparseable marker) deeper than the
        // current item makes mldoc's list parser fail → the whole region is a Paragraph.
        for s in [
            "- a\n  - nested",
            "+ a\n  - nested",
            "1. a\n   more\n   - x",
            "- a\n  - x\n  more",
            "- a\n  more\n  - x",
            "- a\n  + ",       // empty deeper marker
            "- a\n  12abc",    // integer-prefixed, no `.`
            "- a\n  -5",       // `-5` is is_item but unparseable
            "+ a\n  + b\n    - c", // collapse propagates from a grandchild
        ] {
            assert_eq!(bkinds(s), ["paragraph"], "should collapse: {s:?}");
        }
        // collapse then a col-0 terminator still re-parses the terminator.
        assert_eq!(bkinds("- a\n  - x\n* h"), ["paragraph", "bullet"]);
        assert_eq!(bkinds("- a\n  - x\n\n- b"), ["paragraph", "list"]);
        // breakout (NOT collapse): an indented `-` at indent <= the current item.
        assert_eq!(bkinds("+ a\n  + b\n  - c"), ["list", "paragraph"]);
        assert_eq!(bkinds("- a\n- "), ["list", "paragraph"]); // empty trailing marker
        // PARTIAL collapse: items before the failing item survive as a List; the
        // failing item onward is a Paragraph (mldoc bubbles up only through
        // first-at-level items).
        let kept_len = |s: &str| match &parse(s)[0] {
            Block::List { items, .. } => items.len(),
            b => panic!("not a list: {b:?}"),
        };
        assert_eq!(bkinds("- a\n- b\n  - z"), ["list", "paragraph"]);
        assert_eq!(kept_len("- a\n- b\n  - z"), 1); // only `a` survives
        assert_eq!(bkinds("- a\n- b\n- c\n  - z"), ["list", "paragraph"]);
        assert_eq!(kept_len("- a\n- b\n- c\n  - z"), 2); // a, b survive
        assert_eq!(bkinds("+ a\n  + b\n  + c\n    - d"), ["list", "paragraph"]);
        assert_eq!(kept_len("+ a\n  + b\n  + c\n    - d"), 1); // a (with child b) survives
        assert_eq!(bkinds("+ p\n+ a\n  + b\n    - c"), ["list", "paragraph"]);
        assert_eq!(bkinds("1. a\n2. b\n   - z"), ["list", "paragraph"]);
        // two independent first-item collapses ⇒ one merged Paragraph.
        assert_eq!(bkinds("- a\n  - z\n- y\n  - w"), ["paragraph"]);
        // repeated collapses stay linear (collapse-floor memoisation).
        let big = format!("{}  - z", "- a\n".repeat(40_000));
        let _ = parse(&big);
    }

    #[test]
    fn footnote_def_minimum_body() {
        // mldoc: footnote def body needs >=2 bytes after the spaces (satisfy + take_till1).
        assert_eq!(bkinds("[fn:1] a"), ["paragraph"]); // 1-byte body
        assert_eq!(bkinds("[fn:1]  a"), ["paragraph"]); // still 1 byte after spaces
        assert_eq!(bkinds("[fn:1]"), ["paragraph"]); // bare ref
        assert_eq!(bkinds("[fn:1] ab"), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1] a."), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1] a b"), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1]:x"), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1]/x"), ["footnote_def"]);
        assert_eq!(bkinds("[fn:1] é"), ["footnote_def"]); // 2 bytes
        // bad first char stays a paragraph regardless of length.
        assert_eq!(bkinds("[fn:1]-x"), ["paragraph"]);
        assert_eq!(bkinds("[fn:1]*x"), ["paragraph"]);
        assert_eq!(bkinds("[fn:1]#x"), ["paragraph"]);
        assert_eq!(bkinds("[fn:1][x"), ["paragraph"]);
    }

    #[test]
    fn footnote_body_continuation() {
        // mldoc `footnote_definition = many1 l`: the body absorbs following continuation
        // lines (joined with Break_Line, de-indented) until a footnote-specific
        // terminator. `fnbody` renders the (sole) FootnoteDef body, marking Break_Line
        // with `⏎` (robust to plain-node merging).
        let fnbody = |s: &str| -> String {
            match &parse(s)[0] {
                Block::FootnoteDef { inline, .. } => inline
                    .iter()
                    .map(|i| match i {
                        Inline::Plain { text } => text.clone(),
                        Inline::Break => "\u{23ce}".into(),
                        other => format!("<{}>", ik(other)),
                    })
                    .collect(),
                b => panic!("expected FootnoteDef, got {b:?}"),
            }
        };
        // absorbed: de-indented, joined with Break_Line, trailing spaces kept.
        assert_eq!(fnbody("[fn:1] body\ncont"), "body\u{23ce}cont");
        assert_eq!(fnbody("[fn:1] body\ncont\nmore"), "body\u{23ce}cont\u{23ce}more");
        assert_eq!(fnbody("[fn:1] body\n  indented"), "body\u{23ce}indented");
        assert_eq!(fnbody("[fn:1] body\n\tcont"), "body\u{23ce}cont");
        assert_eq!(fnbody("[fn:1] body\ncont  "), "body\u{23ce}cont  ");
        // `+`/`N.` lists and `:`-lines fold as TEXT (footnote non_eol allows them);
        // an indented `+` is de-indented like other content.
        assert_eq!(fnbody("[fn:1] body\n+ x"), "body\u{23ce}+ x");
        assert_eq!(fnbody("[fn:1] body\n1. x"), "body\u{23ce}1. x");
        assert_eq!(fnbody("[fn:1] body\n  + x"), "body\u{23ce}+ x");
        assert_eq!(fnbody("[fn:1] body\n: ex"), "body\u{23ce}: ex");
        // CRLF: a `\r\n` ending drops the `\r` on first AND continuation lines.
        assert_eq!(fnbody("[fn:1] body\r\ncont"), "body\u{23ce}cont");
        // single-line def unchanged; a trailing newline is swallowed.
        assert_eq!(fnbody("[fn:1] body"), "body");
        assert_eq!(fnbody("[fn:1] body\ncont\n"), "body\u{23ce}cont");

        // TERMINATORS: the body stops; the next line is its own block, and the body is
        // exactly the def's own line.
        assert_eq!(bkinds("[fn:1] body\n\ncont"), ["footnote_def", "paragraph"]); // blank
        assert_eq!(bkinds("[fn:1] body\n* h"), ["footnote_def", "bullet"]); // headline
        assert_eq!(bkinds("[fn:1] body\n- x"), ["footnote_def", "list"]); // col-0 `-`
        assert_eq!(bkinds("[fn:1] body\n#+TITLE: x"), ["footnote_def", "directive"]);
        assert_eq!(bkinds("[fn:1] body\n#+BEGIN_SRC\nx\n#+END_SRC"), ["footnote_def", "src"]);
        assert_eq!(bkinds("[fn:1] body\n-----"), ["footnote_def", "hr"]); // `-` hr
        assert_eq!(bkinds("[fn:1] ab\n[fn:2] cd"), ["footnote_def", "footnote_def"]);
        assert_eq!(bkinds("[fn:1] body\n[fn:2] b"), ["footnote_def", "paragraph"]); // `[`
        assert_eq!(bkinds("[fn:1] body\nx"), ["footnote_def", "paragraph"]); // 1-byte cont
        assert_eq!(bkinds("[fn:1] body\n  * x"), ["footnote_def", "list"]); // indented `*`
        assert_eq!(fnbody("[fn:1] body\n- x"), "body");
    }

    #[test]
    fn footnote_cont_predicate() {
        // unit-level: the footnote-body continuation predicate (mldoc `l`).
        assert_eq!(footnote_cont("cont", false), Some("cont")); // EOF line ok
        assert_eq!(footnote_cont("  cont", true), Some("cont")); // de-indent
        assert_eq!(footnote_cont("\tcont", true), Some("cont")); // tab de-indent
        assert_eq!(footnote_cont("cont  ", true), Some("cont  ")); // trailing kept
        assert_eq!(footnote_cont("+ x", true), Some("+ x")); // `+` folds as text
        assert_eq!(footnote_cont("cont\r", true), Some("cont")); // CRLF `\r` dropped
        // terminators → None
        assert_eq!(footnote_cont("", true), None); // blank
        assert_eq!(footnote_cont("   ", true), None); // whitespace-only
        assert_eq!(footnote_cont("x", true), None); // 1-byte body
        for s in ["- x", "* x", "# x", "[x", "  - x", "  # x"] {
            assert_eq!(footnote_cont(s, true), None, "{s}"); // forbidden first char
        }
        assert_eq!(footnote_cont("cont\r", false), None); // dangling `\r`, no `\n`
        assert_eq!(footnote_cont("co\rnt", true), None); // mid `\r` breaks end_of_line
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
        let _ = parse(&"* h\n".repeat(20000));
    }
}

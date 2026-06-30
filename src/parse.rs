//! Block segmentation — milestone 2.
//!
//! A single-pass, line-based scanner that splits input into mldoc-equivalent
//! blocks. Inline content is still a stub (the whole block text as one Plain);
//! real inline parsing lands in M3/M4. The differential gate for this milestone
//! is `block-struct` (kind/level/nesting/properties), which ignores inline content
//! and spans.
//!
//! Complexity: O(n·log n) typical. Each line is classified in O(line length); container
//! closers are found ON-DEMAND at the dispatch point (never eagerly pre-paired). Callout
//! (`#+END_…`) and drawer (`:END:`) closers use per-construct sorted closer-line indexes
//! (`partition_point` ⇒ O(log n)) plus a per-name "no closer ahead" absence memo; fences
//! (where the opener and closer tokens are identical) use a monotone per-char cursor (O(1)
//! amortized). On-demand finding is context-aware: a fence/closer inside a callout or drawer
//! body (which the main loop jumps past) can never pair with one outside it.
//!
//! mldoc quirks replicated (see DECISIONS.md / the block probe):
//! - only `-` bullets become `Bullet` (mldoc `Heading{unordered}`); `*`/`+` and
//!   `N.` become `List` nodes; `N)` is NOT a list. A `*`/`+`/`N.` marker with an
//!   empty title (`1. `, `* `, `* [ ]`) is NOT a list — it falls through to a
//!   Paragraph (mldoc requires non-empty list content).
//! - heading `size` = `#`-count (uncapped); a space/tab (or end-of-line) must
//!   follow the hashes. `level` = 1 + leading-whitespace count: mldoc allows
//!   leading whitespace before the `#`-run and bumps `level` per space/tab
//!   (uncapped — NOT CommonMark's ≤3 rule). The same leading-ws `level` applies to
//!   `-` bullets.
//! - an EMPTY heading/bullet whose line has trailing whitespace after the
//!   marker/size/task-marker/priority prefix splits into [heading|bullet,
//!   paragraph(trailing ws)] — mldoc emits the node for the prefix, then the
//!   leftover whitespace starts a paragraph (`## ` → [heading, paragraph]; a bare
//!   `##` / `-` with no trailing ws stays a single node).
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
    // Single-pass streaming block driver: O(n) time, O(depth) HEAP (the explicit container
    // stack), NO native recursion and NO depth cap. Byte-exact to mldoc (gated by `harness/`).
    // (The deep-nesting recurse-on-body — mldoc's O(n²) + stack-overflow — is gone.)
    parse_impl(input)
}

/// The outcome of classifying ONE line (`dispatch_md_line`): either advance to line
/// `Next(ni)`, or recognize a container opener (`Open`) whose body is `[i+1, close)` and
/// whose closer is line `close`. `Open` defers the body handling to the driver: the
/// legacy `parse_impl` recurses on the body slice; `parse_streaming_impl` pushes a stack
/// frame and keeps scanning the same line array. (The dispatch helper never flushes the
/// paragraph for `Open` — the driver does, just before recursing/pushing.)
enum Step {
    Next(usize),
    Open { close: usize, builder: Builder },
}

/// Captures a callout opener's identity so the driver can emit the right block once its
/// body children are known (`Quote` for `#+BEGIN_QUOTE`, else `Custom{name}`).
enum Builder {
    Quote,
    Custom(String),
}
impl Builder {
    fn finish(self, children: Vec<Block>, span: Option<Span>) -> Block {
        match self {
            Builder::Quote => Block::Quote { children, span },
            Builder::Custom(name) => Block::Custom { name, children, span },
        }
    }
}

/// One open container on the streaming driver's explicit stack. Only re-dispatched
/// callout bodies become frames; everything else writes into the top frame's `out`/`para`.
/// md needs no context flags and no `absorb` (its blank handling is unconditional) — the
/// frame is exactly `hi` + the body accumulators + the captured opener (root has none).
struct Frame {
    hi: usize,                       // EXCLUSIVE closer line index; line `hi` is the closer.
    out: Vec<Block>,                 // children of THIS body.
    para: Option<(usize, usize)>,    // the open paragraph byte-window for THIS body.
    builder: Option<Builder>,        // the opener → emitted on pop (None for the root).
    open_span_start: usize,          // byte offset of the opener line start (for the span).
}

/// The shared precompute over the whole input (O(n), built ONCE): the `#+END_<name>`
/// closer trie, the `:END:` drawer-closer index, and the whole-line fence-marker index.
/// Both drivers query these with a `closer < hi` bound (the streaming driver) — at the
/// top level `hi == lines.len()` so the bound is a no-op, identical to legacy.
fn build_indexes(lines: &[Line]) -> (EndTrie, Vec<usize>, Vec<usize>) {
    // Callout closer index: a trie of `#+END_<name>` line names (see `EndTrie`). A `#+BEGIN_X`
    // opener finds its closer by an O(|X|) trie walk (mldoc's prefix match: `#+END_QUOTEX` closes
    // `QUOTE`), absent ⇒ O(1) — no EOF re-scan, no absence memo. O(n) build / O(n) total, where
    // mldoc's own `take_until` is O(n²) on unclosed-opener runs. Drawers/fences keep their indexes.
    let mut end_trie = EndTrie::new();
    let mut drawer_end_idxs: Vec<usize> = Vec::new(); // `:END:` lines (drawer closers)
    // ALL whole-line fence markers (` ``` `/`~~~`), ascending — mldoc closes a fence at the first
    // later 3+ run of EITHER char (length/info-agnostic), so closing is char-AGNOSTIC: one list.
    let mut fence_lines: Vec<usize> = Vec::new();
    for (idx, l) in lines.iter().enumerate() {
        let t = l.text.trim_start();
        if t.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("#+END_")) {
            end_trie.insert(&t[6..], idx);
        }
        if l.text.trim().eq_ignore_ascii_case(":END:") {
            drawer_end_idxs.push(idx);
        }
        if fence_marker(l.text).is_some() {
            fence_lines.push(idx);
        }
    }
    (end_trie, drawer_end_idxs, fence_lines)
}

/// The md block driver: ONE left-to-right pass over an explicit container-frame stack. Each input
/// line is classified ONCE (`dispatch_md_line`); a callout body is a contiguous line *window*
/// pushed as a `Frame` (`Step::Open`) rather than copied + re-lexed (the old recurse-on-body, the
/// removed source of O(n²) + stack-overflow). Correctness: every closer-search in `dispatch_md_line`
/// is bounded by the frame's `hi`/`body_end`, so a closer/`\end{}`/`]`/run-line belongs to THIS
/// body, never the enclosing one. O(n) time, O(depth) HEAP — no native recursion, no depth cap.
fn parse_impl(input: &str) -> Vec<Block> {
    let mut lines = split_lines(input);
    let last_rbracket = input.rfind(']');
    let (end_trie, drawer_end_idxs, fence_lines) = build_indexes(&lines);
    let n = lines.len();

    // Root frame spans the whole input; its `out`/`para` are the document's. Non-root frames
    // are callout bodies, popped (and emitted via `builder.finish`) when `i` reaches their `hi`.
    let mut stack: Vec<Frame> = vec![Frame {
        hi: n,
        out: Vec::new(),
        para: None,
        builder: None,
        open_span_start: 0,
    }];
    let mut fence_cursor: usize = 0; // monotone & shared across the whole pass (`i` is monotone).
    let mut i = 0;

    loop {
        // Close every container ending at line `i` (consuming the closer line). Non-root frames
        // have `hi == close <= n-1 < n`, so the root (hi == n) is never popped here.
        while stack.len() > 1 && stack.last().unwrap().hi == i {
            let mut f = stack.pop().unwrap();
            flush_para(&mut f.out, &mut f.para, input);
            let span = Some(Span(f.open_span_start, lines[i].end));
            let block = f.builder.unwrap().finish(f.out, span);
            stack.last_mut().unwrap().out.push(block);
            i += 1; // CONSUME the closer line.
        }
        if i >= n {
            break;
        }
        let line_start = lines[i].start; // copied before the `&mut` dispatch borrows.
        let step = {
            let top = stack.last_mut().unwrap();
            let hi = top.hi;
            dispatch_md_line(
                i,
                &mut lines,
                &mut top.out,
                &mut top.para,
                hi,
                &end_trie,
                &drawer_end_idxs,
                &fence_lines,
                &mut fence_cursor,
                last_rbracket,
                input,
            )
        };
        match step {
            Step::Next(ni) => i = ni,
            Step::Open { close, builder } => {
                // The dispatch helper did NOT flush para for `Open` — flush the parent's, then
                // push the body frame and step past the opener line.
                {
                    let top = stack.last_mut().unwrap();
                    flush_para(&mut top.out, &mut top.para, input);
                }
                stack.push(Frame {
                    hi: close,
                    out: Vec::new(),
                    para: None,
                    builder: Some(builder),
                    open_span_start: line_start,
                });
                i += 1;
            }
        }
    }

    // Only the root remains (all callout bodies closed before EOF); flush its paragraph.
    let mut root = stack.pop().unwrap();
    flush_para(&mut root.out, &mut root.para, input);
    root.out
}

/// Classify ONE md line `i` in the body bounded by `hi` (EXCLUSIVE closer line index), writing
/// any completed block into `out` / accumulating into `para`, and return a `Step`. This is the
/// single per-line dispatch ladder shared by BOTH drivers (legacy recurses on `Open`; streaming
/// pushes a frame). The whole streaming-correctness story lives here: every forward closer-search
/// is bounded by `hi` / `body_end`, so a closer/`\end{}`/`]`/run-line BELONGS to this body and
/// never the enclosing one. At the top level `hi == lines.len()` (and `body_end == input.len()`),
/// so all bounds are no-ops and the behavior is identical to the pre-refactor inline ladder.
#[allow(clippy::too_many_arguments)]
fn dispatch_md_line<'a>(
    i: usize,
    lines: &mut [Line<'a>],
    out: &mut Vec<Block>,
    para: &mut Option<(usize, usize)>,
    hi: usize,
    end_trie: &EndTrie,
    drawer_end_idxs: &[usize],
    fence_lines: &[usize],
    fence_cursor: &mut usize,
    last_rbracket: Option<usize>,
    input: &'a str,
) -> Step {
    // Copy the line's fields out (a `&'a str` + two `usize`s, none borrowing the `lines`
    // slice) so the block-hiccup remainder split (step 11d') can REWRITE `lines[ri]` in place.
    let t = lines[i].text;
    let line_start = lines[i].start;
    let line_end = lines[i].end;
    // Byte offset where THIS body ends (the closer line's start, or EOF at the root). Used to
    // CLAMP the to-end-of-input forward-scanners (`parse_latex_env`, `parse_hiccup`).
    let body_end = if hi < lines.len() { lines[hi].start } else { input.len() };

    // 1. fenced code (Src) — ON-DEMAND, context-aware. A fence-marker line the loop REACHES is
    // an opener at THIS level. Its closer = the first whole-line fence marker after it of EITHER
    // char (`find_matching_fence`). The closer must lie inside THIS body (`< hi`); a match `>= hi`
    // belongs to an enclosing body, so the fence here is unclosed → fall through to paragraph.
    if let Some((_c, mend)) = fence_marker(t) {
        if let Some(close) = find_matching_fence(fence_lines, fence_cursor, i) {
            if close < hi {
                flush_para(out, para, input);
                let lang = fence_lang(&t[mend..]);
                let code = if close > i + 1 {
                    input[lines[i + 1].start..lines[close - 1].end].to_string()
                } else {
                    String::new()
                };
                // mldoc's Src swallows trailing blank lines (so they don't become a leading
                // break on the following paragraph). Bounded by `hi` (the closer is non-blank).
                let mut ni = close + 1;
                let mut end = lines[close].end;
                while ni < hi && lines[ni].text.is_empty() {
                    end = lines[ni].end;
                    ni += 1;
                }
                out.push(Block::Src { lang, code, span: Some(Span(line_start, end)) });
                return Step::Next(ni);
            }
            // closer is outside this body → unclosed here → fall through.
        }
        // unclosed fence → fall through (treat as paragraph text).
    }

    // 2. callout #+BEGIN_X … #+END_X → an Open container (the driver handles the body). The
    // closer must lie inside THIS body (`< hi`); otherwise it belongs to an enclosing callout →
    // this `#+BEGIN_X` is unclosed here → fall through to paragraph.
    if let Some(name) = callout_begin(t) {
        if let Some(close) = end_trie.find(&name, i) {
            if close < hi {
                let builder = if name.eq_ignore_ascii_case("QUOTE") {
                    Builder::Quote
                } else {
                    Builder::Custom(name.to_ascii_lowercase())
                };
                return Step::Open { close, builder };
            }
            // closer is outside this body → fall through.
        }
        // no matching END → fall through (treat as paragraph text).
    }

    // 2b. LaTeX environment `\begin{X} … \end{X}` (mldoc Latex_env, before Block). CLAMP the
    // `\end{}` search to `&input[..body_end]` so an `\end{X}` outside this body is not captured
    // (verified load-bearing: `#+BEGIN_QUOTE\n\begin{eq}\n#+END_QUOTE\n\end{eq}`).
    let line_content_end = line_start + t.len();
    if let Some((name, content, consumed_end)) =
        crate::inline::parse_latex_env(&input[..body_end], line_start, line_content_end)
    {
        flush_para(out, para, input);
        out.push(Block::LatexEnv { name, content, span: Some(Span(line_start, consumed_end)) });
        // resume at the first line starting at/after consumed_end (always > i, and <= hi since
        // consumed_end <= body_end == lines[hi].start).
        let mut ni = i + 1;
        while ni < lines.len() && lines[ni].start < consumed_end {
            ni += 1;
        }
        return Step::Next(ni);
    }

    // 3. heading. `level` = 1 + leading-ws (mldoc bumps level per leading
    // space/tab, uncapped); `size` = `#`-count. An empty heading whose line has
    // trailing whitespace splits into [heading, paragraph(trailing ws)].
    if let Some((level, size, hend)) = heading_at(t) {
        flush_para(out, para, input);
        let (marker, priority, title) = split_markers(t[hend..].trim_start());
        let trail = trim_end_ws_len(t);
        if title.is_empty() && trail < t.len() {
            out.push(Block::Heading {
                level,
                size: Some(size),
                inline: vec![],
                marker,
                priority,
                htags: vec![],
                span: Some(Span(line_start, line_start + trail)),
            });
            *para = Some((line_start + trail, line_end));
            return Step::Next(i + 1);
        }
        out.push(Block::Heading {
            level,
            size: Some(size),
            inline: stub_inline(title),
            marker,
            priority,
            htags: vec![],
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 4. horizontal rule (before dash bullet / list)
    if is_hr(t) {
        flush_para(out, para, input);
        out.push(Block::Hr { span: Some(Span(line_start, line_end)) });
        return Step::Next(i + 1);
    }

    // 5. `-` bullet (mldoc Heading{unordered}).
    if let Some(level) = dash_bullet_level(t) {
        // mldoc's bullet title is a lookahead (heading0.ml `title_aux_p`): if the
        // text after the bullet prefix parses as a block construct, the bullet gets
        // an EMPTY title and the construct is parsed as the next block. We replicate
        // the two openers that occur in real outlines: a fenced code block and a
        // markdown blockquote (only on `-` bullets; `*`/`+` are Lists, untouched).
        let dw = leading_ws(t);
        let after = t[dw + 1..].trim_start(); // after '-' + spaces
        let (size, content) = atx_size(after); // heading `#{1,n}` size + the rest
        let content_off = line_start + (t.len() - content.len());
        // emit the empty (title-less) bullet that precedes a split-off sibling block.
        macro_rules! empty_bullet {
            () => {
                out.push(Block::Bullet {
                    level,
                    size,
                    inline: vec![],
                    marker: None,
                    priority: None,
                    htags: vec![],
                    span: Some(Span(line_start, content_off)),
                });
            };
        }
        // (a) fenced code opener on the bullet line — only splits if it CLOSES inside this body
        // (an unclosed/out-of-body `- ``` ` stays a normal bullet titled "```", per mldoc).
        if let Some((_fchar, frun)) = fence_marker(content) {
            // `content` is already leading-ws-stripped, so `fence_marker` matched at
            // its very start (a true fence opener).
            if let Some(close) = find_matching_fence(fence_lines, fence_cursor, i) {
                if close < hi {
                    flush_para(out, para, input);
                    empty_bullet!();
                    let lang = fence_lang(&content[frun..]);
                    let code = if close > i + 1 {
                        input[lines[i + 1].start..lines[close - 1].end].to_string()
                    } else {
                        String::new()
                    };
                    let mut end = lines[close].end;
                    let mut ni = close + 1;
                    while ni < hi && lines[ni].text.is_empty() {
                        end = lines[ni].end;
                        ni += 1;
                    }
                    out.push(Block::Src { lang, code, span: Some(Span(content_off, end)) });
                    return Step::Next(ni);
                }
            }
        }
        // (b) markdown blockquote opener on the bullet line (lazy continuation). The run is
        // bounded by `hi` — the closer line is never a quote-continuation, so absorbing it
        // would wrongly swallow the frame's closer (verified load-bearing).
        if quote_opens(content) {
            flush_para(out, para, input);
            empty_bullet!();
            let mut body_lines: Vec<String> = Vec::new();
            if let Some(c) = quote_line_content(content, true) {
                body_lines.push(c);
            }
            let mut ni = i + 1;
            while ni < hi {
                match quote_line_content(lines[ni].text, false) {
                    Some(c) => {
                        body_lines.push(c);
                        ni += 1;
                    }
                    None => break,
                }
            }
            out.push(Block::Quote {
                children: parse_quote_body(&body_lines),
                span: Some(Span(content_off, lines[ni - 1].end)),
            });
            return Step::Next(ni);
        }
        // (c) property line on the bullet line (mldoc heading0.ml: the title is a
        // lookahead, and `markdown_property` is one of the constructs tried — so
        // `- key:: value` yields an EMPTY bullet then a Property_Drawer that BEGINS
        // at the bullet content and folds in subsequent property/directive lines
        // (exactly like step 8). `content` is post-`#{1,n}`-strip, matching the size
        // run; the property `key` rejects bullet prefixes via its space check.
        if let Some(kv) = property(content) {
            flush_para(out, para, input);
            empty_bullet!();
            let mut props = vec![kv];
            let mut end = line_end;
            let mut ni = i + 1;
            while ni < hi {
                if let Some(kv) = property(lines[ni].text) {
                    props.push(kv);
                } else if let Some(kv) = directive_property(lines[ni].text) {
                    props.push(kv);
                } else {
                    break;
                }
                end = lines[ni].end;
                ni += 1;
            }
            out.push(Block::Properties { props, span: Some(Span(content_off, end)) });
            return Step::Next(ni);
        }
        // (d) horizontal rule opener (`---`/`***`/`___`).
        if is_hr(content) {
            flush_para(out, para, input);
            empty_bullet!();
            out.push(Block::Hr { span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (e) block displayed-math opener `$$ … $$` (single line).
        if let Some(math) = displayed_math(content) {
            flush_para(out, para, input);
            empty_bullet!();
            out.push(Block::DisplayedMath { text: math, span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (f) raw-HTML opener.
        if is_raw_html(content) {
            flush_para(out, para, input);
            empty_bullet!();
            out.push(Block::RawHtml { text: content.to_string(), span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (g) LaTeX environment opener `\begin{X} … \end{X}` (may span lines). CLAMP as in 2b.
        if let Some((name, lc, consumed_end)) =
            crate::inline::parse_latex_env(&input[..body_end], content_off, line_start + t.len())
        {
            flush_para(out, para, input);
            empty_bullet!();
            out.push(Block::LatexEnv { name, content: lc, span: Some(Span(content_off, consumed_end)) });
            let mut ni = i + 1;
            while ni < lines.len() && lines[ni].start < consumed_end {
                ni += 1;
            }
            return Step::Next(ni);
        }
        // (h) table opener `| … |` (consumes following table-row lines, bounded by `hi`).
        if md_table_row(content) {
            flush_para(out, para, input);
            empty_bullet!();
            let mut texts: Vec<&str> = vec![content];
            let mut ni = i + 1;
            while ni < hi && md_table_row(lines[ni].text) {
                texts.push(lines[ni].text);
                ni += 1;
            }
            out.push(build_table_from_texts(&texts, content_off, lines[ni - 1].end));
            return Step::Next(ni);
        }
        // (i) footnote-definition opener — only WITHOUT a `#` prefix (with a `#`,
        // `[^id]` is an inline footnote ref in the heading title, per mldoc heading0).
        if size.is_none() {
            if let Some((fname, fbody)) = footnote_def(content) {
                flush_para(out, para, input);
                empty_bullet!();
                out.push(Block::FootnoteDef {
                    name: fname,
                    inline: stub_inline(fbody),
                    span: Some(Span(content_off, line_end)),
                });
                return Step::Next(i + 1);
            }
        }
        // normal bullet — or, when the title is empty and the line has trailing
        // whitespace after the marker/size/task-marker/priority prefix, an empty
        // bullet followed by a paragraph of that trailing whitespace (mldoc emits
        // the bullet for the prefix, then the leftover ws starts a paragraph:
        // `- ` / `-   ` / `- ## ` / `- TODO ` → [bullet, paragraph]; a bare `-`
        // / `- ##` / `- TODO` with no trailing ws stays a single empty bullet).
        flush_para(out, para, input);
        let (marker, priority, title) = split_markers(content);
        let trail = trim_end_ws_len(t);
        if title.is_empty() && trail < t.len() {
            out.push(Block::Bullet {
                level,
                size,
                inline: vec![],
                marker,
                priority,
                htags: vec![],
                span: Some(Span(line_start, line_start + trail)),
            });
            *para = Some((line_start + trail, line_end));
            return Step::Next(i + 1);
        }
        out.push(Block::Bullet {
            level,
            size,
            inline: stub_inline(title),
            marker,
            priority,
            htags: vec![],
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 6. footnote definition
    if let Some((fname, content)) = footnote_def(t) {
        flush_para(out, para, input);
        out.push(Block::FootnoteDef {
            name: fname,
            inline: stub_inline(content),
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 7. table (group of consecutive table-row lines, bounded by `hi`)
    if md_table_row(t) {
        flush_para(out, para, input);
        let start = i;
        let mut ni = i;
        while ni < hi && md_table_row(lines[ni].text) {
            ni += 1;
        }
        out.push(build_table(&lines[start..ni], lines[start].start, lines[ni - 1].end));
        return Step::Next(ni);
    }

    // 8. property drawer (group of consecutive `key:: value` lines, bounded by `hi`). mldoc folds
    // trailing `#+name: value` org directives into the same drawer (drawer.ml
    // `many1 (parse1 <|> parse2)`), so `a:: 1\n#+b: 2` → props a, b.
    if property(t).is_some() {
        flush_para(out, para, input);
        let start = i;
        let mut props = Vec::new();
        let mut ni = i;
        while ni < hi {
            if let Some(kv) = property(lines[ni].text) {
                props.push(kv);
                ni += 1;
            } else if let Some(kv) = directive_property(lines[ni].text) {
                props.push(kv);
                ni += 1;
            } else {
                break;
            }
        }
        out.push(Block::Properties {
            props,
            span: Some(Span(lines[start].start, lines[ni - 1].end)),
        });
        return Step::Next(ni);
    }

    // 9. list (group of consecutive `*`/`+`/`N.` items, bounded by `hi`)
    if let Some(item) = list_item(t) {
        flush_para(out, para, input);
        let start = i;
        let mut items = vec![item];
        let mut last = i; // last line index that belongs to the list
        let mut ni = i + 1;
        while ni < hi {
            if let Some(it) = list_item(lines[ni].text) {
                items.push(it);
                last = ni;
                ni += 1;
            } else if lines[ni].text.trim().is_empty() {
                // mldoc's list absorbs ONE blank line (two_eols): the list span
                // extends through it. If the next line is another item the list
                // continues; otherwise the list ends here (the blank consumed, so it
                // never becomes its own paragraph). A SECOND consecutive blank is not
                // absorbed — it ends the list and becomes a paragraph.
                let next_is_item = ni + 1 < hi
                    && !lines[ni + 1].text.trim().is_empty()
                    && list_item(lines[ni + 1].text).is_some();
                last = ni;
                ni += 1;
                if !next_is_item {
                    break;
                }
            } else {
                break;
            }
        }
        out.push(Block::List {
            items: crate::projection::nest_items(items),
            span: Some(Span(lines[start].start, lines[last].end)),
        });
        return Step::Next(ni);
    }

    // 10. markdown blockquote (mldoc block0.ml `md_blockquote`): a `>` line opens
    // a quote whose body is the de-`>`'d lines PLUS lazy continuation lines (no
    // `>` needed) until a blank line or a line that starts a new block
    // (`- `/`# `/`id:: `/bare `-`/`#`). The body is parsed as block-content — for
    // markdown prose that is a single Paragraph (with keep_line_break breaks); the
    // property/heading/bullet parsers are NOT applied inside a quote.
    // A quote OPENS only if there's non-whitespace after the `>` (mldoc: lone
    // `>` / `> ` are paragraphs; `>x` / `> x` are quotes). The run is bounded by `hi`
    // (the closer line would otherwise be lazily absorbed — verified load-bearing).
    if quote_opens(t) {
        flush_para(out, para, input);
        let start = i;
        let mut body_lines: Vec<String> = Vec::new();
        // first line: strip the opening `>` then process its remainder like a
        // continuation (mldoc consumes one `>` then runs lines_while on the rest).
        if let Some(c) = quote_line_content(lines[i].text, true) {
            body_lines.push(c);
        }
        let mut ni = i + 1;
        while ni < hi {
            match quote_line_content(lines[ni].text, false) {
                Some(c) => {
                    body_lines.push(c);
                    ni += 1;
                }
                None => break,
            }
        }
        out.push(Block::Quote {
            children: parse_quote_body(&body_lines),
            span: Some(Span(lines[start].start, lines[ni - 1].end)),
        });
        return Step::Next(ni);
    }

    // 11. raw HTML (single-line, minimal)
    if is_raw_html(t) {
        flush_para(out, para, input);
        out.push(Block::RawHtml {
            text: t.to_string(),
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 11b. block-level displayed math: a line that is just `$$ … $$`.
    if let Some(math) = displayed_math(t) {
        flush_para(out, para, input);
        out.push(Block::DisplayedMath {
            text: math,
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 11c. org-style drawer `:NAME: … :END:` (e.g. :LOGBOOK:). The special
    // `:PROPERTIES:` drawer becomes a Property_Drawer even in Markdown (mldoc
    // drawer.ml), with `:key: value` lines parsed as properties. The `:END:` must lie
    // inside THIS body (`< hi`); else it belongs to an enclosing body → fall through.
    if let Some(name) = drawer_begin(t) {
        if let Some(close) = find_drawer_end(drawer_end_idxs, i) {
            if close < hi {
                flush_para(out, para, input);
                let span = Some(Span(line_start, lines[close].end));
                if name == "properties" {
                    let props = lines[i + 1..close]
                        .iter()
                        .filter_map(|l| drawer_property(l.text))
                        .collect();
                    out.push(Block::Properties { props, span });
                } else {
                    out.push(Block::Drawer { name, span });
                }
                return Step::Next(close + 1);
            }
            // :END: is outside this body → fall through.
        }
        // no :END: → fall through to paragraph.
    }

    // 11d'. block-level Clojure-hiccup `[:tag …]` at BOL (after leading ws). mldoc
    // emits a `Hiccup` block when a line (no shielding construct claimed it) starts
    // with a balanced hiccup vector; the balanced capture is string-aware and MAY
    // span lines, and the remainder past the `]` re-enters block parsing at BOL
    // (`[:div]x` → [Hiccup, Paragraph x]; `[:a][:b]` → two Hiccups). Tried before the
    // def-list / paragraph fallbacks (a hiccup wins: `[:div]\n: def` → [Hiccup, Para]).
    {
        let lw = leading_ws(t);
        let rec = line_start + lw;
        if last_rbracket.is_some_and(|last| rec <= last) && input[rec..].starts_with("[:") {
            // CLAMP the (to-end-of-input) balanced capture to `&input[..body_end]` so a `]`
            // outside this body is not captured (verified load-bearing). `rec < body_end`.
            if let Some(cap_end) = crate::inline::parse_hiccup(&input[..body_end], rec) {
                flush_para(out, para, input);
                out.push(Block::Hiccup {
                    v: input[rec..cap_end].to_string(),
                    span: Some(Span(line_start, cap_end)),
                });
                // Resume after the `]`, first absorbing consecutive eols (mldoc's
                // `<* optional eols`: `[:div]\n\nx` → [Hiccup, Para "x"], i.e. blank
                // lines after a whole-line hiccup are swallowed — but a same-line
                // remainder `[:div]x\n\ny` is NOT, so skip only `\n`/`\r` bytes). The eol
                // run stops at `body_end` (the closer line starts with `#`/`:`, non-eol),
                // so it never crosses into the enclosing body.
                let bytes = input.as_bytes();
                let mut resume = cap_end;
                while resume < bytes.len() && matches!(bytes[resume], b'\n' | b'\r') {
                    resume += 1;
                }
                if resume >= bytes.len() {
                    return Step::Next(lines.len()); // captured to EOF (+ trailing eols)
                }
                // Find the line containing `resume`; process it as-is when `resume` is
                // at its start, else rewrite it to the remainder slice.
                let mut ri = i;
                while ri < lines.len() && lines[ri].end <= resume {
                    ri += 1;
                }
                if ri >= lines.len() {
                    return Step::Next(lines.len()); // defensive (resume < len ⇒ unreachable)
                }
                if resume > lines[ri].start {
                    let content_end = lines[ri].start + lines[ri].text.len();
                    lines[ri] = Line {
                        start: resume,
                        end: lines[ri].end,
                        text: &input[resume..content_end],
                    };
                }
                return Step::Next(ri);
            }
        }
    }

    // 11d. markdown definition list (mldoc `lists0.ml` `md_definition`, the Lists
    // fallback, tried just above paragraph): a (would-be paragraph) term line
    // immediately followed by a `: <def>` line. mldoc pulls the term out of a
    // running paragraph (`intro\nterm\n: def` → Paragraph[intro] + def-list), so
    // we check it here at the paragraph point, after every other block construct.
    // The term peek + `build_def_list`'s item/continuation/blank scans are bounded by `hi`.
    if !t.trim_start().is_empty()
        && i + 1 < hi
        && is_def_opener(lines[i + 1].text)
    {
        flush_para(out, para, input);
        let (item, ni) = build_def_list(lines, i, hi);
        out.push(Block::List {
            items: vec![item],
            span: Some(Span(line_start, lines[ni - 1].end)),
        });
        return Step::Next(ni);
    }

    // 12. plain line — accumulate into the current paragraph.
    *para = Some(match *para {
        Some((s, _)) => (s, line_end),
        None => (line_start, line_end),
    });
    Step::Next(i + 1)
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
    // The real inline parser. Name kept for the existing call sites.
    crate::resolver::parse_inline(s)
}

/// mldoc heading/bullet task markers (`Heading0.marker`), stripped from the title.
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

/// Title text of an ATX-ish bullet/heading content: strip a leading `#{1,n} ` run
/// (mldoc parses a heading inside a bullet, e.g. `- ## X` → bullet titled `X`), then
/// a leading task marker (`TODO `…) and priority (`[#A]`), matching mldoc's
/// `level *> marker *> priority *> title` order.
/// Split a leading ATX `#`-run that forms a heading size (uncapped `#`-count, must be
/// followed by a space/tab or end-of-line) off `s`. Returns (size, rest-after-the-run,
/// leading-ws-trimmed). `None` when there is no valid `#`-run (`#nospace`, `text`).
fn atx_size(s: &str) -> (Option<u32>, &str) {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    if hashes > 0 {
        let after = &s[hashes..];
        if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
            return (Some(hashes as u32), after.trim_start());
        }
    }
    (None, s)
}

/// Extract a leading task marker (`TODO `…) and priority `[#X]`, in mldoc's
/// `marker *> priority *> title` order. Returns (marker, priority, remaining title).
fn split_markers(s: &str) -> (Option<String>, Option<String>, &str) {
    let mut marker = None;
    let mut s = s;
    for m in MARKERS {
        if let Some(rest) = s.strip_prefix(m) {
            if rest.starts_with(' ') {
                marker = Some((*m).to_string());
                s = rest.trim_start();
                break;
            }
        }
    }
    // priority `[#X]` (exactly "[#", one ASCII char, "]")
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

/// Split a leading list checkbox `[ ]` / `[x]` / `[X]` (+ following spaces) off `s`,
/// returning (state, rest): `[ ]`→`Some(false)`, `[x]`/`[X]`→`Some(true)`, none→`(None, s)`.
/// mldoc records this only for `*`/`+`/`N.` lists (lists0), NOT for `-` bullets (heading0).
fn split_checkbox(s: &str) -> (Option<bool>, &str) {
    if let Some(r) = s.strip_prefix("[ ]") {
        (Some(false), r.trim_start())
    } else if let Some(r) = s.strip_prefix("[x]").or_else(|| s.strip_prefix("[X]")) {
        (Some(true), r.trim_start())
    } else {
        (None, s)
    }
}


/// Split into lines on any of `\r\n`, lone `\n`, or lone `\r` (mldoc's `is_eol`
/// treats `\r` and `\n` each as a line terminator; a CRLF is consumed as ONE
/// terminator). The returned `text` excludes the terminator, so a trailing `\r` is
/// never carried into block content. Paragraph bodies are re-extracted from the raw
/// byte span, so the inline parser (which treats `\r` AND `\n` as `Break`) restores
/// the per-eol break count (`a\r\nb` → [a, Break, Break, b]).
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
            // consume the terminator: `\r\n` as a unit, else a lone `\r`/`\n`.
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

/// Language of a fence from its info string (the text after the ``` run): mldoc's
/// `language` is the FIRST whitespace-delimited token (`clj :results` → `clj`, the
/// `:results` is a separate `options` field we don't model).
fn fence_lang(info: &str) -> String {
    info.split_whitespace().next().unwrap_or("").to_string()
}

/// First whole-line fence-marker (` ``` ` OR `~~~`, char-AGNOSTIC — mldoc closes a fence at the
/// first later 3+ run of either char) strictly after `from`, via the ascending `fence_lines`
/// list + a MONOTONE cursor (O(1) amortized, never an EOF re-scan). The single closer-finder for
/// BOTH a top-level fence opener (step 1) and one opened on a `-` bullet line (step 5a):
/// on-demand at the dispatch point, so it can't pair across a body the main loop jumped past.
fn find_matching_fence(fence_lines: &[usize], cursor: &mut usize, from: usize) -> Option<usize> {
    // the main loop reaches fence openers in increasing `from`, so the cursor only advances.
    while *cursor < fence_lines.len() && fence_lines[*cursor] <= from {
        *cursor += 1;
    }
    fence_lines.get(*cursor).copied()
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
    // mldoc's fence marker is EXACTLY 3 chars: a 3+ run opens/closes, but only the first 3 are
    // the marker — extra run chars (and the rest of the line) are the info/lang string. So the
    // info begins at `ws + 3`, not past the whole run (`####js` → lang "`js", not "js").
    if k - ws >= 3 {
        Some((c, ws + 3))
    } else {
        None
    }
}

/// Heading detection, allowing leading whitespace. Returns `(level, size, hend)`:
/// `level` = 1 + leading-ws count (mldoc bumps `level` per leading space/tab,
/// uncapped — it does NOT apply CommonMark's ≤3-space rule), `size` = the `#`-count
/// (uncapped), and `hend` = the within-line byte index just past the `#`-run. A
/// space/tab must follow the hashes — or the line is just (ws +) the hashes. `None`
/// when the first non-ws char isn't such a `#`-run.
fn heading_at(s: &str) -> Option<(u32, u32, usize)> {
    let lw = leading_ws(s);
    let rest0 = &s[lw..];
    let hashes = rest0.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 {
        return None;
    }
    let rest = &rest0[hashes..];
    if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
        Some((1 + lw as u32, hashes as u32, lw + hashes))
    } else {
        None
    }
}

/// Byte length of `s` with trailing spaces/tabs removed (the index just past the
/// last char that is not a space/tab). Used to locate the trailing-whitespace run
/// that mldoc splits into a paragraph after an empty heading/bullet.
fn trim_end_ws_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut k = b.len();
    while k > 0 && (b[k - 1] == b' ' || b[k - 1] == b'\t') {
        k -= 1;
    }
    k
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

/// `(ws)- ` (or a lone `(ws)-` at end-of-line) ⇒ bullet of level `1 + ws` (each
/// space/tab counts 1). mldoc (heading0) accepts `-` followed by a space/tab OR
/// end-of-line (`- ` and a bare `-` are both empty bullets), OR directly by an ATX
/// heading run (`-## x` → bullet with size 2, no space needed; but `-#x`/`-x` are not).
fn dash_bullet_level(s: &str) -> Option<u32> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    let after = rest.strip_prefix('-')?;
    if after.is_empty()
        || after.starts_with(' ')
        || after.starts_with('\t')
        || atx_size(after).0.is_some()
    {
        Some(1 + ws as u32)
    } else {
        None
    }
}

/// Build a markdown `*`/`+`/`N.` list item's content from its raw body. mldoc block-parses
/// list-item content with a restricted set that recognizes block-Hiccups (`[:tag …]`) but
/// not headings/etc.: a body beginning with one or more `[:tag …]` vectors yields those
/// `Hiccup` blocks, then the remainder (if any) as one Paragraph; anything else is a single
/// inline-parsed Paragraph (`* [:div]x` → [Hiccup, Para "x"]; `* a [:div] b` → [Para]).
fn list_item_content(body: &str) -> Vec<Block> {
    let mut pos = 0;
    let mut blocks: Vec<Block> = Vec::new();
    while body[pos..].starts_with("[:") {
        match crate::inline::parse_hiccup(body, pos) {
            Some(end) => {
                blocks.push(Block::Hiccup { v: body[pos..end].to_string(), span: None });
                pos = end;
            }
            None => break,
        }
    }
    if blocks.is_empty() {
        return vec![Block::Paragraph { inline: stub_inline(body), span: None }];
    }
    if pos < body.len() {
        blocks.push(Block::Paragraph { inline: stub_inline(&body[pos..]), span: None });
    }
    blocks
}

fn list_item(s: &str) -> Option<ListItem> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    // unordered * or +
    if let Some(after) = rest.strip_prefix('*').or_else(|| rest.strip_prefix('+')) {
        if after.starts_with(' ') || after.starts_with('\t') {
            let (checkbox, body) = split_checkbox(after.trim_start());
            // mldoc requires non-empty list content: `* ` / `*  ` / `* [ ]` (an
            // empty marker, optionally just a checkbox) is a Paragraph, not a List.
            if body.trim().is_empty() {
                return None;
            }
            return Some(ListItem {
                ordered: false,
                number: None,
                indent: ws as u32,
                // `*`/`+`/`N.` list content is RAW after the marker+checkbox — mldoc does
                // NOT strip ATX `#`/task-markers here (unlike `-` bullets): `* # h` → "# h".
                // It IS block-parsed for leading hiccups (`* [:div]` → item content [Hiccup]).
                content: list_item_content(body),
                items: vec![],
                name: vec![],
                checkbox,
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
                    let (checkbox, body) = split_checkbox(after2.trim_start());
                    // mldoc requires non-empty content: `1. ` / `1.  ` / `1. [ ]`
                    // (an empty ordered marker) is a Paragraph, not a List.
                    if body.trim().is_empty() {
                        return None;
                    }
                    return Some(ListItem {
                        ordered: true,
                        number: Some(number),
                        indent: ws as u32,
                        content: list_item_content(body), // raw + leading-hiccup blocks
                        items: vec![],
                        name: vec![],
                        checkbox,
                    });
                }
            }
        }
    }
    None
}

/// A post-`>` core that makes the WHOLE `>` line a plain Paragraph (mldoc does not open
/// a blockquote for these): a lone `-`/`#` outline marker (`- x`/`# x`/bare `-`/`#`) or
/// an `id::` property. NOTE `##`/`-x`/`* `/`+ `/`N.` are NOT triggers (those DO open a
/// quote; `*`/`+`/`N.` then parse as a List inside it). C2.
fn quote_para_trigger(core: &str) -> bool {
    core == "#"
        || core.starts_with("# ")
        || core == "-"
        || core.starts_with("- ")
        || core.starts_with("id:: ")
}

/// Strip the leading `>` (and recursively any nested `>`) from a quote line, returning
/// the de-`>`'d body content — or `None` when the line is a paragraph-trigger (so it is
/// NOT a quote). A lone `>` (+ws) yields `Some("")` (an empty quote-body line). `t` must
/// be already `trim_start`-ed and begin with `>`. mldoc strips nested `>` to a single
/// flattened Quote, and a trigger after any number of `>` makes the whole line a
/// Paragraph (`> > - x` → Paragraph). C2.
fn quote_strip(t: &str) -> Option<String> {
    let rest = &t[1..]; // drop the leading '>'
    let core = rest.trim_start();
    if core.is_empty() {
        return Some(String::new());
    }
    if quote_para_trigger(core) {
        return None;
    }
    if core.starts_with('>') {
        return quote_strip(core);
    }
    Some(core.to_string())
}

/// Does this line OPEN a blockquote? A `>` line opens one unless its post-`>` core is
/// empty (lone `>`/`> ` → Paragraph) or a paragraph-trigger (`- `/`# `/`id:: ` → the
/// whole line is a Paragraph, NOT an empty quote). C2.
fn quote_opens(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('>') && matches!(quote_strip(t), Some(c) if !c.is_empty())
}

fn property(s: &str) -> Option<(String, String)> {
    let s = s.trim_start(); // property lines may be indented under a block
    let pos = s.find("::")?;
    let key = &s[..pos];
    // key has no whitespace and no `:` — the latter rejects URLs like
    // `http://x.com:: y` (mldoc: prose, not a property), since `http:` has a colon.
    if key.is_empty() || key.contains(' ') || key.contains('\t') || key.contains(':') {
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

fn footnote_def(s: &str) -> Option<(String, &str)> {
    let rest = s.trim_start().strip_prefix("[^")?;
    let end = rest.find(']')?;
    let name = &rest[..end];
    let after = rest[end + 1..].strip_prefix(':')?;
    Some((name.to_string(), after.trim_start()))
}

/// One blockquote body line (mldoc `md_blockquote` `lines_while`): from a raw line,
/// strip leading ws, an optional `>` (required only on the first line — handled by
/// the caller passing `first=true`, which still strips it), and following ws, and
/// return the remaining content. Returns `None` to STOP the quote: on a blank line
/// (no `>`, empty after ws) or a line that opens a new block (`- `/`# `/`id:: ` or a
/// bare `-`/`#`). A line that is just `>`(+ws) yields `Some("")` (an empty quote line).
fn quote_line_content(s: &str, first: bool) -> Option<String> {
    let _ = first; // first vs continuation differ only in that the first always has `>`
    let t = s.trim_start();
    if t.starts_with('>') {
        return quote_strip(t);
    }
    // lazy continuation (no `>`): a blank line, or a line that starts a new block
    // (`- `/`# `/`id:: `/bare `-`/`#`), ends the quote. `*`/`+`/`N.` markers are NOT
    // breakers — they are absorbed and parsed as a List inside the quote body.
    if t.is_empty() || quote_para_trigger(t) {
        return None;
    }
    Some(t.to_string())
}

/// Parse a Markdown blockquote body (de-`>`'d content lines) into the inner block
/// sequence. mldoc recognizes Lists (`*`/`+`/`N.`), block-Hiccups (`[:tag …]`) and
/// Paragraphs inside a quote (no headings/tables). A paragraph run joins its lines with
/// `keep_line_break` `Break`s and a trailing `Break` UNLESS it is immediately followed by
/// a List or a Hiccup (mldoc drops that break). C2.
fn parse_quote_body(lines: &[String]) -> Vec<Block> {
    // Expand each (de-`>`'d, trim_start'd) body line into block-hiccup pieces + the text
    // remainder: a `[:tag …]` at a body-line BOL becomes its own `Hiccup` (mldoc emits one
    // inside a quote body too). A hiccup that starts mid-line (`a [:div]`) stays in the
    // Text piece and is handled by the inline parser. Hiccups are captured WITHIN a single
    // body line (a quote hiccup spanning `>` lines is not modeled — vanishingly rare).
    enum Q {
        Hic(String),
        Text(String),
    }
    let mut items: Vec<Q> = Vec::new();
    for s in lines {
        let mut pos = 0;
        while s[pos..].starts_with("[:") {
            match crate::inline::parse_hiccup(s, pos) {
                Some(end) => {
                    items.push(Q::Hic(s[pos..end].to_string()));
                    pos = end;
                }
                None => break,
            }
        }
        if pos == 0 {
            items.push(Q::Text(s.clone())); // whole line (no leading hiccup)
        } else if pos < s.len() {
            items.push(Q::Text(s[pos..].to_string())); // remainder after the hiccup(s)
        }
        // pos == s.len() with peeled hiccup(s): line fully consumed, no Text piece.
    }

    let list_at = |q: &Q| -> Option<ListItem> {
        match q {
            Q::Text(t) => list_item(t),
            Q::Hic(_) => None,
        }
    };
    let mut out = Vec::new();
    let n = items.len();
    let mut i = 0;
    while i < n {
        match &items[i] {
            Q::Hic(v) => {
                out.push(Block::Hiccup { v: v.clone(), span: None });
                i += 1;
            }
            _ if list_at(&items[i]).is_some() => {
                let mut litems = Vec::new();
                while i < n {
                    match list_at(&items[i]) {
                        Some(it) => {
                            litems.push(it);
                            i += 1;
                        }
                        None => break,
                    }
                }
                out.push(Block::List { items: crate::projection::nest_items(litems), span: None });
            }
            _ => {
                // paragraph run: consecutive non-list Text pieces.
                let mut texts: Vec<&str> = Vec::new();
                while i < n {
                    match &items[i] {
                        Q::Text(t) if list_item(t).is_none() => {
                            texts.push(t.as_str());
                            i += 1;
                        }
                        _ => break,
                    }
                }
                // a following List or Hiccup drops the paragraph's trailing Break (same
                // rule mldoc applies before a List in the no-hiccup case).
                let followed = i < n;
                let mut text = texts.join("\n");
                if !followed {
                    text.push('\n');
                }
                out.push(Block::Paragraph { inline: stub_inline(&text), span: None });
            }
        }
    }
    out
}

/// `#+name: value` org directive line, folded into an adjacent markdown property
/// drawer (mldoc drawer.ml `parse2`). Returns (name, value).
fn directive_property(s: &str) -> Option<(String, String)> {
    let t = s.trim_start().strip_prefix("#+")?;
    let pos = t.find(':')?;
    let key = &t[..pos];
    if key.is_empty() || key.contains(' ') || key.contains('\t') {
        return None;
    }
    let value = t[pos + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

/// One `:key: value` line of a `:PROPERTIES:` drawer (mldoc drawer.ml `property`):
/// `:` key `:` value (key has no `:`/space). Returns None for non-property lines.
fn drawer_property(s: &str) -> Option<(String, String)> {
    let t = s.trim_start().strip_prefix(':')?;
    let pos = t.find(':')?;
    let key = &t[..pos];
    if key.is_empty() || key.contains(' ') || key.contains('\t') || key.eq_ignore_ascii_case("end") {
        return None;
    }
    // value = rest of line after the key's closing `:`, trimmed (drops a leading space
    // and a trailing CR from CRLF inputs).
    let value = t[pos + 1..].trim();
    Some((key.to_string(), value.to_string()))
}

/// Does line `s` OPEN a markdown definition (a `: <def>` line)? mldoc
/// `markdown_definition.ml`: `spaces *> ':' *> ws(≥1) *> term_definition` where the
/// first `l` is `spaces *> satisfy(∉ ':' '#' eol) *> line(take_till1 eol)` — so after
/// the `:` and its required whitespace there must be ≥2 non-eol chars whose first is
/// not `:`/`#` (the quirky take_till1-after-satisfy gives the ≥2 rule, e.g. `: a` is
/// NOT a def but `: ab` is).
fn is_def_opener(s: &str) -> bool {
    let rest = match s.trim_start().strip_prefix(':') {
        Some(r) => r,
        None => return false,
    };
    // ws = take_while1 is_space: ≥1 space/tab required (`:nospace` is not a def).
    if !(rest.starts_with(' ') || rest.starts_with('\t')) {
        return false;
    }
    def_line_content_ok(rest.trim_start())
}

/// A definition continuation line (mldoc term_definition `l`): after leading spaces,
/// the same ≥2-chars / first-∉`:`#` rule (`: ab\nx` stops at `ab`; `: ab\ncc` joins).
fn is_def_continuation(s: &str) -> bool {
    def_line_content_ok(s.trim_start())
}

/// The shared `satisfy(∉ ':' '#' eol) *> take_till1 eol` test on the content (already
/// leading-space-stripped): first char ∉ {`:`,`#`,CR}, and ≥1 more non-CR char.
fn def_line_content_ok(content: &str) -> bool {
    let mut it = content.chars();
    match it.next() {
        Some(c0) if c0 != ':' && c0 != '#' && c0 != '\r' => {
            matches!(it.next(), Some(c) if c != '\r')
        }
        _ => false,
    }
}

/// Build the markdown definition list whose term is `lines[i]` and whose `:` items
/// follow. Returns the single `ListItem` and the next line index (after the items and
/// any trailing blank lines mldoc's `<* optional eols` absorbs).
fn build_def_list(lines: &[Line], i: usize, hi: usize) -> (ListItem, usize) {
    // All scans are bounded by `hi`: at the top level `hi == lines.len()` (identical to
    // before); inside a callout body the closer line (`#+END_X`) is never a def
    // opener/continuation/blank, so the bound matches the legacy body-local scan exactly.
    let name = stub_inline(lines[i].text.trim_start()); // mldoc name = `spaces *> line`
    let mut content: Vec<Block> = Vec::new();
    let mut j = i + 1;
    while j < hi && is_def_opener(lines[j].text) {
        // item first line: drop `<spaces>:` then the required ws (and any more spaces).
        let first = lines[j].text.trim_start().strip_prefix(':').unwrap_or("").trim_start();
        let mut item_lines = vec![first.to_string()];
        j += 1;
        // continuation lines (non-`:`-leading, same ≥2 rule), joined with '\n'.
        while j < hi && is_def_continuation(lines[j].text) {
            item_lines.push(lines[j].text.trim_start().to_string());
            j += 1;
        }
        // mldoc inline-parses `String.trim`-ed of the joined item.
        let item_text = item_lines.join("\n");
        content.push(Block::Paragraph {
            inline: stub_inline(item_text.trim()),
            span: None,
        });
    }
    // absorb trailing blank lines (mldoc `definition_parse <* optional eols`).
    while j < hi && lines[j].text.trim().is_empty() {
        j += 1;
    }
    let item = ListItem {
        ordered: false,
        number: None,
        indent: 0,
        content,
        items: vec![],
        name,
        checkbox: None,
    };
    (item, j)
}

fn callout_begin(s: &str) -> Option<String> {
    let t = s.trim_start();
    // `get(..8)` is char-boundary-safe (returns None on a multibyte split).
    if t.get(..8)?.eq_ignore_ascii_case("#+BEGIN_") {
        // mldoc's block name is `take_while1(non-space)` IMMEDIATELY after `#+BEGIN_`: the name
        // is the leading non-ws run, and an empty one (`#+BEGIN_`, or `#+BEGIN_ X` where ws
        // leads) is NOT a block — a plain paragraph. (Do NOT `split_whitespace`-skip leading ws.)
        let rest = &t[8..];
        let n = rest.bytes().take_while(|&b| b != b' ' && b != b'\t').count();
        (n > 0).then(|| rest[..n].to_string())
    } else {
        None
    }
}

/// A trie over the (lowercased) names of `#+END_<name>` lines. Each node holds the line indexes
/// of every `#+END_` line whose name PASSES THROUGH it (i.e. has the node's path as a prefix),
/// in ascending line order. A callout opener named X then finds its closer — the first `#+END_`
/// line after `from` whose name starts with X (mldoc's case-insensitive prefix match:
/// `#+END_QUOTEX` closes `QUOTE`) — by walking X (O(|X|)) and `partition_point`-ing the node's
/// index list. Build is O(Σ |name|) = O(n) and shares prefixes (so a single long name is O(name),
/// not O(name²) like a per-prefix hashmap). This is O(n) over a parse level even on adversarial
/// unclosed-opener runs — where mldoc's own `take_until` is O(n²) (measured: 4000 openers = 68s).
#[derive(Default)]
struct EndTrie {
    kids: Vec<HashMap<u8, u32>>, // node → byte → child node
    ends: Vec<Vec<usize>>,       // node → `#+END_` line indexes with this prefix (ascending)
}
impl EndTrie {
    fn new() -> Self {
        EndTrie { kids: vec![HashMap::new()], ends: vec![Vec::new()] }
    }
    /// Index `#+END_` line `idx` under the leading non-ws run of `suffix` (the text after
    /// `#+END_`), lowercased. The empty prefix (root) matches any opener name (incl. `""`).
    fn insert(&mut self, suffix: &str, idx: usize) {
        let mut node = 0usize;
        self.ends[node].push(idx);
        for &b in suffix.as_bytes() {
            if b == b' ' || b == b'\t' {
                break;
            }
            let lb = b.to_ascii_lowercase();
            node = match self.kids[node].get(&lb) {
                Some(&c) => c as usize,
                None => {
                    let c = self.kids.len();
                    self.kids.push(HashMap::new());
                    self.ends.push(Vec::new());
                    self.kids[node].insert(lb, c as u32);
                    c
                }
            };
            self.ends[node].push(idx);
        }
    }
    /// First `#+END_` line after `from` whose name starts with `name` (case-insensitive), or
    /// `None` (unclosed/mismatched — O(|name|), no EOF scan). Byte-exact to the old prefix scan.
    fn find(&self, name: &str, from: usize) -> Option<usize> {
        let mut node = 0usize;
        for &b in name.as_bytes() {
            node = *self.kids[node].get(&b.to_ascii_lowercase())? as usize;
        }
        let v = &self.ends[node];
        v.get(v.partition_point(|&x| x <= from)).copied()
    }
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

/// First `:END:` line after `from`, via the sparse `:END:` index (binary search ⇒ O(log n)).
fn find_drawer_end(drawer_end_idxs: &[usize], from: usize) -> Option<usize> {
    drawer_end_idxs.get(drawer_end_idxs.partition_point(|&x| x <= from)).copied()
}

fn is_raw_html(s: &str) -> bool {
    // `<tag …>…</tag>` — a real HTML element, NOT an autolink `<https://…>` and NOT
    // an incomplete tag. mldoc is strict: a bare `<div>` or `<note this>` is a
    // paragraph; only a line with an opening tag AND a closing `</…>` is Raw_Html.
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
    // require a closing tag on the line (approximates mldoc's complete-element rule).
    t.contains("</")
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
            Block::Directive { .. } => "directive",
            Block::Comment { .. } => "comment",
            Block::Example { .. } => "example",
            Block::LatexEnv { .. } => "latex_env",
            Block::Hiccup { .. } => "hiccup",
        }).collect()
    }

    #[test]
    fn bullet_heading_size_and_openers() {
        // Gap 1: `- ## Title` carries the heading level as Bullet.size (uncapped).
        let size = |s: &str| match &parse(s)[0] {
            Block::Bullet { size, .. } => *size,
            _ => panic!("expected Bullet"),
        };
        assert_eq!(size("- # h"), Some(1));
        assert_eq!(size("- ###### h"), Some(6));
        assert_eq!(size("- ####### h"), Some(7)); // uncapped
        assert_eq!(size("- # TODO x"), Some(1)); // size then marker
        assert_eq!(size("- #nospace"), None); // no space ⇒ not a heading
        assert_eq!(size("- plain"), None);
        // Gap 2: post-marker block openers split into [empty bullet, sibling block].
        assert_eq!(kinds("- ---"), ["bullet", "hr"]);
        assert_eq!(kinds("- $$ x $$"), ["bullet", "displayed_math"]);
        assert_eq!(kinds("- [^1]: body"), ["bullet", "footnote_def"]);
        assert_eq!(kinds("- <div>x</div>"), ["bullet", "raw_html"]);
        assert_eq!(kinds("- \\begin{eq}a\\end{eq}"), ["bullet", "latex_env"]);
        assert_eq!(kinds("- | a | b |"), ["bullet", "table"]);
        // size + opener combine; but `[^id]:` after a `#` is an inline ref (no split).
        assert_eq!(kinds("- # ---"), ["bullet", "hr"]);
        assert_eq!(kinds("- # [^1]: b"), ["bullet"]);
    }

    #[test]
    fn block_hiccup() {
        // whole-line hiccup → Hiccup block; not-a-tag → paragraph.
        assert_eq!(kinds("[:div]"), ["hiccup"]);
        assert_eq!(kinds("  [:div]"), ["hiccup"]); // leading ws absorbed
        assert_eq!(kinds("[:foo]"), ["paragraph"]);
        // remainder past the `]` re-enters block parsing at BOL.
        assert_eq!(kinds("[:div]x"), ["hiccup", "paragraph"]);
        assert_eq!(kinds("[:div]# h"), ["hiccup", "heading"]);
        assert_eq!(kinds("[:div]- x"), ["hiccup", "bullet"]);
        assert_eq!(kinds("[:div][:span]"), ["hiccup", "hiccup"]);
        assert_eq!(kinds("[:div]\n: def"), ["hiccup", "paragraph"]); // hiccup beats def-list
        // shielded by a fenced code block / breaks a paragraph.
        assert_eq!(kinds("```\n[:div]\n```"), ["src"]);
        assert_eq!(kinds("foo\n[:div]\nbar"), ["paragraph", "hiccup", "paragraph"]);
        // payload is the raw bracket text.
        match &parse("[:div.cls {:a 1}]")[0] {
            Block::Hiccup { v, .. } => assert_eq!(v, "[:div.cls {:a 1}]"),
            _ => panic!("expected Hiccup"),
        }
        // list-item content is block-parsed for leading hiccups.
        match &parse("* [:div]")[0] {
            Block::List { items, .. } => {
                assert!(matches!(items[0].content[0], Block::Hiccup { .. }));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn block_hiccup_runs_terminate() {
        let _ = parse(&"[:div ".repeat(20000)); // unclosed block-hiccup lines
        let _ = parse(&"[:a]".repeat(20000)); // consecutive whole-line hiccups
    }

    #[test]
    fn realmut_tracked_edges() {
        // table header+separator, no body → the separator stays a body row.
        match &parse("| a | b |\n|---|---|")[0] {
            Block::Table { header, rows, .. } => {
                assert!(header.is_some());
                assert_eq!(rows.len(), 1); // the `|---|` row, kept
            }
            _ => panic!("expected Table"),
        }
        match &parse("| a | b |\n|---|---|\n| 1 | 2 |")[0] {
            Block::Table { rows, .. } => assert_eq!(rows.len(), 1), // sep dropped, 1 body row
            _ => panic!(),
        }
        // `*`/`N.` list content is raw (no `#`/marker strip).
        let item0_text = |s: &str| match &parse(s)[0] {
            Block::List { items, .. } => match &items[0].content[0] {
                Block::Paragraph { inline, .. } => match &inline[0] {
                    Inline::Plain { text } => text.clone(),
                    _ => panic!(),
                },
                _ => panic!(),
            },
            _ => panic!("expected List"),
        };
        assert_eq!(item0_text("* # heading"), "# heading");
        assert_eq!(item0_text("* TODO task"), "TODO task");
        assert_eq!(item0_text("1. # h"), "# h");
        // a single blank between items is absorbed; 2+ blanks split.
        assert_eq!(kinds("* a\n\n* b"), ["list"]);
        assert_eq!(kinds("* a\n\n\n* b"), ["list", "paragraph", "list"]);
        assert_eq!(kinds("* a\n\n# h"), ["list", "heading"]); // list absorbs the trailing blank
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
    fn fuzz_surfaced_block_edges() {
        // quote opens only with non-whitespace after `>`
        assert_eq!(kinds(">"), ["paragraph"]);
        assert_eq!(kinds("> "), ["paragraph"]);
        assert_eq!(kinds(">x"), ["quote"]);
        assert_eq!(kinds("> x"), ["quote"]);
        // property key must not contain `:` (URLs are prose, not properties)
        assert_eq!(kinds("http://x.com:: y"), ["paragraph"]);
        assert_eq!(kinds("a/b:: c"), ["properties"]);
        assert_eq!(kinds("a.b:: c"), ["properties"]);
        // raw html needs a closing tag; a bare/incomplete tag is a paragraph
        assert_eq!(kinds("<div>"), ["paragraph"]);
        assert_eq!(kinds("<note this>"), ["paragraph"]);
        assert_eq!(kinds("<div>x</div>"), ["raw_html"]);
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

    #[test]
    fn block_span_round_trips_source() {
        // FOR-TINE wire contract (point 1): a block's `span` [s,e] is its byte-range in the input,
        // so a consumer can slice the raw text by span (Tine: search-text = raw MINUS the
        // Properties/Drawer spans). Spans must stay EMITTED (the `Block` enum keeps
        // `skip_serializing_if = "Option::is_none"`, NOT skip-always) and round-trip exactly.
        let input = "- foo\nkey:: val"; // [Bullet("foo"), Properties{key}]
        let blocks = parse(input);
        let props = blocks
            .iter()
            .find(|b| matches!(b, Block::Properties { .. }))
            .expect("trailing `key::` ⇒ a Properties block");
        let Span(s, e) = block_span(props).expect("the Properties block carries a span");
        assert_eq!(&input[s..e], "key:: val", "the span must round-trip to the block's source");
    }

    fn block_span(b: &Block) -> Option<Span> {
        match b {
            Block::Paragraph { span, .. } | Block::Heading { span, .. }
            | Block::Bullet { span, .. } | Block::List { span, .. }
            | Block::Src { span, .. } | Block::Quote { span, .. }
            | Block::Custom { span, .. } | Block::Properties { span, .. }
            | Block::Hr { span, .. } | Block::Table { span, .. }
            | Block::FootnoteDef { span, .. } | Block::RawHtml { span, .. }
            | Block::DisplayedMath { span, .. } | Block::Drawer { span, .. }
            | Block::Directive { span, .. } | Block::Comment { span, .. }
            | Block::Example { span, .. }
            | Block::LatexEnv { span, .. }
            | Block::Hiccup { span, .. } => *span,
        }
    }

    #[test]
    fn properties_drawer_and_directive_fold() {
        // :PROPERTIES: drawer parses to a Property_Drawer even in Markdown (M5).
        match &parse(":PROPERTIES:\n:type: x\n:creator: y\n:END:")[0] {
            Block::Properties { props, .. } => {
                assert_eq!(props, &vec![("type".into(), "x".into()), ("creator".into(), "y".into())]);
            }
            _ => panic!(),
        }
        // a #+name: value directive is folded into an adjacent property drawer.
        match &parse("a:: 1\n#+b: 2")[0] {
            Block::Properties { props, .. } => {
                assert_eq!(props, &vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
            }
            _ => panic!(),
        }
        assert_eq!(kinds("a:: 1\n#+b: 2"), ["properties"]);
    }

    #[test]
    fn empty_and_eol_bullets() {
        // `- ##` is a bullet whose heading-prefix leaves an empty title (M5).
        assert_eq!(kinds("- ##"), ["bullet"]);
        match &parse("- ##")[0] {
            Block::Bullet { inline, .. } => assert!(inline.is_empty()),
            _ => panic!(),
        }
        // a lone `-` at end-of-line is an (empty) bullet.
        assert_eq!(kinds("+ x\n  -"), ["list", "bullet"]);
    }

    #[test]
    fn empty_marker_and_leading_ws_segmentation() {
        // Helpers to read heading/bullet level + the first paragraph's plain text.
        let hlevel = |s: &str| match &parse(s)[0] {
            Block::Heading { level, .. } | Block::Bullet { level, .. } => *level,
            b => panic!("expected heading/bullet, got {b:?}"),
        };
        let para_text = |b: &Block| match b {
            Block::Paragraph { inline, .. } => match inline.first() {
                Some(Inline::Plain { text }) => text.clone(),
                _ => String::new(),
            },
            _ => panic!("expected paragraph"),
        };

        // (1) empty ordered / `*` / `+` markers are Paragraphs, not Lists.
        assert_eq!(kinds("1. "), ["paragraph"]);
        assert_eq!(kinds("3. "), ["paragraph"]);
        assert_eq!(kinds("1."), ["paragraph"]); // no space at all
        assert_eq!(kinds("1.  "), ["paragraph"]);
        assert_eq!(kinds("1. \t"), ["paragraph"]);
        assert_eq!(kinds("* "), ["paragraph"]);
        assert_eq!(kinds("+ "), ["paragraph"]);
        assert_eq!(kinds("* [ ]"), ["paragraph"]); // checkbox but no title
        assert_eq!(kinds("1. [ ]"), ["paragraph"]);
        assert_eq!(kinds("1. x"), ["list"]); // non-empty ⇒ still a list
        assert_eq!(kinds("* x"), ["list"]);
        assert_eq!(kinds("* [ ] x"), ["list"]);
        // a trailing empty marker after a real item ends the list (separate paragraph).
        assert_eq!(kinds("1. x\n2. "), ["list", "paragraph"]);

        // (2) empty ATX heading + trailing ws ⇒ [heading, paragraph(trailing ws)].
        assert_eq!(kinds("## "), ["heading", "paragraph"]);
        assert_eq!(kinds("# "), ["heading", "paragraph"]);
        assert_eq!(kinds("##"), ["heading"]); // bare hashes ⇒ single heading
        assert_eq!(para_text(&parse("##  ")[1]), "  "); // both spaces in the paragraph
        assert_eq!(para_text(&parse("# TODO ")[1]), " "); // marker on heading, ws split
        match &parse("# TODO ")[0] {
            Block::Heading { marker, inline, .. } => {
                assert_eq!(marker.as_deref(), Some("TODO"));
                assert!(inline.is_empty());
            }
            _ => panic!(),
        }

        // (3) empty `-` bullet + trailing ws ⇒ [bullet, paragraph(trailing ws)].
        assert_eq!(kinds("- "), ["bullet", "paragraph"]);
        assert_eq!(kinds("-"), ["bullet"]); // bare dash ⇒ single bullet
        assert_eq!(kinds("- ## "), ["bullet", "paragraph"]); // size kept, ws split
        assert_eq!(kinds("- ##"), ["bullet"]); // no trailing ws ⇒ single bullet
        assert_eq!(para_text(&parse("-   ")[1]), "   ");
        assert_eq!(para_text(&parse("- \t ")[1]), " \t ");
        match &parse("- TODO ")[0] {
            Block::Bullet { marker, inline, .. } => {
                assert_eq!(marker.as_deref(), Some("TODO"));
                assert!(inline.is_empty());
            }
            _ => panic!(),
        }
        // trailing ws starts a paragraph that absorbs following lines (lazy).
        assert_eq!(kinds("- \nfoo"), ["bullet", "paragraph"]);
        assert_eq!(kinds("## \nfoo"), ["heading", "paragraph"]);

        // (4) leading whitespace before `#` ⇒ heading; level = 1 + ws (uncapped, tab=1).
        assert_eq!(kinds("  # heading"), ["heading"]);
        assert_eq!(hlevel("  # heading"), 3); // 1 + 2 spaces
        assert_eq!(hlevel("   # heading"), 4);
        assert_eq!(hlevel("    # heading"), 5); // 4 spaces still a heading (no ≤3 rule)
        assert_eq!(hlevel("\t# heading"), 2); // tab counts 1
        assert_eq!(hlevel(" \t # heading"), 4); // mixed ws
        // a heading interrupts a running paragraph.
        assert_eq!(kinds("foo\n  # bar"), ["paragraph", "heading"]);

        // (5) combinations: leading-ws + empty heading/bullet trailing-ws split.
        assert_eq!(kinds("  ## "), ["heading", "paragraph"]);
        assert_eq!(hlevel("  ## "), 3);
        assert_eq!(kinds("  - "), ["bullet", "paragraph"]);
        assert_eq!(hlevel("  - "), 3);
        // HR with leading ws is still an HR (heading check yields to it).
        assert_eq!(kinds("   ---"), ["hr"]);
    }

    #[test]
    fn blockquote_flat_paragraph_and_lazy_continuation() {
        // quote body is a flat paragraph, NOT re-segmented into a property drawer (M5).
        match &parse("> a:: b")[0] {
            Block::Quote { children, .. } => assert!(matches!(children[0], Block::Paragraph { .. })),
            _ => panic!(),
        }
        // a following non-`>` line lazily continues the quote (one quote block).
        assert_eq!(kinds(">foo\nbar"), ["quote"]);
        // a `- ` line ends the quote (new block).
        assert_eq!(kinds("> q\n- item"), ["quote", "bullet"]);
    }

    #[test]
    fn latex_environment_block() {
        // multi-line env: content is everything between `\begin{X}` (after one eol) and
        // `\end{X}`; the node name is lowercased.
        match &parse("\\begin{equation}\nx=1\ny=2\n\\end{equation}")[0] {
            Block::LatexEnv { name, content, .. } => {
                assert_eq!(name, "equation");
                assert_eq!(content, "x=1\ny=2\n");
            }
            _ => panic!(),
        }
        // single-line env.
        match &parse("\\begin{eq}a b\\end{eq}")[0] {
            Block::LatexEnv { name, content, .. } => {
                assert_eq!(name, "eq");
                assert_eq!(content, "a b");
            }
            _ => panic!(),
        }
        assert_eq!(kinds("  \\begin{eq}a\\end{eq}"), ["latex_env"]); // leading indent
        // an unclosed `\begin` still becomes an env to EOF (mldoc).
        match &parse("\\begin{eq}\nx=1")[0] {
            Block::LatexEnv { content, .. } => assert_eq!(content, "x=1"),
            _ => panic!(),
        }
        // text before `\begin` ⇒ NOT an env (it's a paragraph).
        assert_eq!(kinds("hi \\begin{eq}x\\end{eq}"), ["paragraph"]);
        // case-insensitive end match.
        match &parse("\\begin{Eq}x\\END{eq}")[0] {
            Block::LatexEnv { name, content, .. } => { assert_eq!(name, "eq"); assert_eq!(content, "x"); }
            _ => panic!(),
        }
    }

    #[test]
    fn markdown_definition_list() {
        // term + `: def` → a List whose item carries the term as `name`.
        match &parse("term\n: definition")[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].name, vec![Inline::Plain { text: "term".into() }]);
                assert!(matches!(items[0].content[0], Block::Paragraph { .. }));
            }
            _ => panic!(),
        }
        assert_eq!(kinds("term\n: definition"), ["list"]);
        // multi-def: one item, two content paragraphs.
        match &parse("term\n: def1\n: def2")[0] {
            Block::List { items, .. } => assert_eq!(items[0].content.len(), 2),
            _ => panic!(),
        }
        // two terms (continuation): item1 content joins `d1`+`t2` across a Break.
        match &parse("t1\n: d1\nt2\n: d2")[0] {
            Block::List { items, .. } => {
                assert_eq!(items[0].name, vec![Inline::Plain { text: "t1".into() }]);
                match &items[0].content[0] {
                    Block::Paragraph { inline, .. } => assert_eq!(inline, &vec![
                        Inline::Plain { text: "d1".into() }, Inline::Break,
                        Inline::Plain { text: "t2".into() },
                    ]),
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
        // quirks: no space after `:`, single-char def, and a `:`/`#`-leading def all fail.
        assert_eq!(kinds("term\n:nospace"), ["paragraph"]);
        assert_eq!(kinds("term\n: a"), ["paragraph"]);      // <2 chars after `: `
        assert_eq!(kinds("term\n: #x"), ["paragraph"]);
        // a running paragraph keeps all but the last line; that becomes the term.
        assert_eq!(kinds("intro\nterm\n: definition"), ["paragraph", "list"]);
    }

    #[test]
    fn block_construct_on_bullet_line() {
        // a `-` bullet whose content opens a fence → empty bullet + Src.
        assert_eq!(kinds("- ```\ncode\n```"), ["bullet", "src"]);
        match &parse("- ```\ncode\n```")[0] {
            Block::Bullet { inline, .. } => assert!(inline.is_empty()),
            _ => panic!(),
        }
        match &parse("- ``` clj :results\n(inc 2)\n```")[1] {
            Block::Src { lang, code, .. } => { assert_eq!(lang, "clj"); assert_eq!(code, "(inc 2)\n"); }
            _ => panic!(),
        }
        // a `-` bullet opening a blockquote (with lazy continuation).
        assert_eq!(kinds("- > q"), ["bullet", "quote"]);
        assert_eq!(kinds("  - > line3\n    > line4"), ["bullet", "quote"]);
        // `*`/`+` are Lists — NOT split (the ``` is item content).
        assert_eq!(kinds("* ```\nx\n```"), ["list", "paragraph"]);
        // an UNCLOSED fence stays a normal bullet (title "```"); a lone `-`/`> ` too.
        assert_eq!(kinds("- ```\nnoclose"), ["bullet", "paragraph"]);
        assert_eq!(kinds("- normal text"), ["bullet"]);
        match &parse("- >")[0] {
            Block::Bullet { inline, .. } => assert_eq!(inline, &vec![Inline::Plain { text: ">".into() }]),
            _ => panic!(),
        }
    }

    #[test]
    fn nested_md_lists() {
        // Compact tree shape: "a[b,c]" = a with children b,c. Label = the item's
        // first plain inline. Verifies mldoc's indent-folding (see `nest_items`).
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
        assert_eq!(shape(&items("* a\n  * b")), "a[b]");
        assert_eq!(shape(&items("* a\n  * b\n    * c")), "a[b[c]]");
        assert_eq!(shape(&items("* a\n * b")), "a[b]"); // any greater indent nests
        assert_eq!(shape(&items("* a\n* b")), "a,b"); // equal indent → siblings
        assert_eq!(shape(&items("+ a\n  + b")), "a[b]");
        assert_eq!(shape(&items("1. a\n   2. b\n   3. c")), "a[b,c]"); // b,c siblings under a
        assert_eq!(shape(&items("* a\n  1. b")), "a[b]"); // mixed un/ordered nests
        assert_eq!(shape(&items("1. a\n   1. nested")), "a[nested]"); // the former b021
        assert_eq!(shape(&items("* a\n  * b\n  * b2\n    * c")), "a[b,b2[c]]");
        // mid (indent 2) unwinds past deep's run floor (4) → TOP sibling of a, not a child.
        assert_eq!(shape(&items("* a\n    * deep\n  * mid")), "a[deep],mid");
    }

    #[test]
    fn unicode_does_not_panic() {
        // Real content has multibyte chars; byte-slicing must stay on boundaries.
        for s in ["#+BEGIN_QUOTE\ncafé 中文 😀\n#+END_QUOTE", "café", "中文 #tag", "😀 [[page]]"] {
            let _ = parse(s);
        }
    }
}

/// A Markdown table row: after trimming, starts AND ends with `|` (≥2 bytes). mldoc
/// (and org's `is_table_row`) require BOTH ends — a bare leading `|` (`|a`, `| a | b`)
/// is a Paragraph, not a Table (C3: prevents table over-detection + phantom refs).
fn md_table_row(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 2 && t.starts_with('|') && t.ends_with('|')
}

fn build_table(rows: &[Line], start: usize, end: usize) -> Block {
    let texts: Vec<&str> = rows.iter().map(|l| l.text).collect();
    build_table_from_texts(&texts, start, end)
}

/// Build a `Table` from raw row strings (used by both the top-level table block and the
/// `- | … |` bullet-opener split, whose first row is a mid-line bullet body, not a `Line`).
fn build_table_from_texts(rows: &[&str], start: usize, end: usize) -> Block {
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

    let header = rows.first().map(|l| split_cells(l));
    let mut data_start = 1;
    // The `|---|` separator row is dropped ONLY when a body row follows it; a table that
    // is just header+separator (no body) keeps the separator as a body row (mldoc quirk).
    if rows.len() > 2 && is_sep(rows[1]) {
        data_start = 2;
    }
    let body: Vec<Vec<Vec<Inline>>> =
        rows[data_start.min(rows.len())..].iter().map(|l| split_cells(l)).collect();

    Block::Table {
        header,
        rows: body,
        span: Some(Span(start, end)),
    }
}


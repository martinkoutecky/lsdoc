//! Block segmentation — the markdown block parser.
//!
//! A single-pass, line-based streaming scanner that splits input into
//! mldoc-equivalent blocks. Each block's inline content is fully parsed (see
//! `inline.rs`); this file owns only the block layer (segmentation + nesting).
//! The driver (`parse_md_streaming`) keeps an explicit container stack instead of
//! recursing on block bodies, so it is O(n) time / O(depth) heap with no native
//! recursion. Correctness is gated byte-exact against mldoc 1.5.7 by `harness/`
//! (the full corpus differential + `blockgate`/`inlinegate` + the fuzz tripwires),
//! comparing the whole projection — block tree AND inline content — modulo `span`.
//!
//! Complexity: O(n). Each line is classified in O(line length); container closers are found
//! ON-DEMAND at the dispatch point (never eagerly pre-paired). Callout (`#+END_…`), drawer
//! (`:END:`) and fence closers all use MONOTONE CURSORS (advance-only) over per-construct
//! sorted closer-line indexes — the drivers query each in non-decreasing line order, so a
//! lookup is O(1) amortized, not the O(log n) of a binary search (see `EndTrie::find`,
//! `find_drawer_end`, `find_matching_fence`). On-demand finding is context-aware: a
//! fence/closer inside a callout or drawer body (which the main loop jumps past) can never
//! pair with one outside it.
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

// The md (`parse.rs`) and org (`org.rs`) block loops are intentionally PARALLEL implementations
// (different grammars); the leaf predicates + infrastructure they both use — `split_lines`,
// `EndTrie`, fence/drawer lookups, the task-marker table, `Builder`, `GT_FALLBACK_NEST_CAP` — live once
// in `crate::block_common`. The dispatch ladders and driver loops below stay per-format.
use crate::block_common::{
    displayed_math, drawer_property, find_drawer_end, find_matching_fence, is_raw_html, leading_ws,
    para_ws_only, split_checkbox, split_lines, Builder, EndTrie, Line, GT_FALLBACK_NEST_CAP, MARKERS,
};
use crate::projection::{Block, Inline, ListItem, Span};

// Depth guard for the ONE md re-dispatch that still native-recurses: the §3 `>`-quote fallback
// (`reparse_block_content`). The `>`-quote staircase and re-bulleted `#+BEGIN` bodies are now
// frames (P2/P3c) and never reach it. Only a `>`-quote body containing a fence / `#+BEGIN` /
// LaTeX env / hiccup is de-`>`'d and reparsed, and construct-in-`>`-quote nesting recurses one
// level per such body (O(d²) INPUT, fuzz-unreachable, where mldoc itself SIGABRTs). Counts that
// depth against the shared `block_common::GT_FALLBACK_NEST_CAP`, which degrades gracefully to a
// flat Paragraph rather than SIGABRT-ing. Unreachable by any gated / realistic / fuzz input.
std::thread_local! {
    static MD_BLOCK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn parse(input: &str) -> Vec<Block> {
    // Single-pass streaming block driver: O(n) time, O(depth) HEAP (the explicit container
    // stack), NO native recursion and NO depth cap. Byte-exact to mldoc (gated by `harness/`).
    // (The deep-nesting recurse-on-body — mldoc's O(n²) + stack-overflow — is gone.)
    parse_md_streaming(input, false, false)
}

/// Per-line de-indent view reused from org.rs: `pub(crate) strip_view` strips `strip` bytes
/// of leading ASCII whitespace from `text`. See `crate::org::strip_view` for the full spec and
/// the composition proof (strip_view(strip_view(t,A),B) == strip_view(t,A+B)).
#[inline]
fn line_text<'a>(lines: &[crate::block_common::Line<'a>], k: usize, strip: usize) -> &'a str {
    crate::org::strip_view(lines[k].text, strip)
}

/// Peel `n` blockquote `>`-levels off `s` (CONTINUATION semantics: `trim_start` then strip one
/// leading `>` per level; a level with no `>` stops early — the lazy case). O(min(n, #`>`)) =
/// O(len). The md twin of org's `gt_peel`; composes the cumulative `>`-strip of a stack of
/// `>`-frames, the `>`-analogue of `strip_view`'s indent strip.
fn gt_peel(s: &str, n: usize) -> &str {
    let mut cur = s;
    for _ in 0..n {
        let t = cur.trim_start();
        match t.strip_prefix('>') {
            Some(rest) => cur = rest, // next iteration's trim_start handles the ws
            None => return t,         // lazy: no `>` at this level ⇒ stop
        }
    }
    cur
}

/// The view of a `>`-frame CONTINUATION line: peel `gt_level` `>`s off the strip-viewed raw line
/// (`gt_peel` the first `gt_level-1`, the FINAL peel via `md_quote_cont_slice` so the breaker /
/// `>`-blank / blank boundary is honored). `None` ⇒ the line ends the run (bare blank or a de-`>`'d
/// breaker) ⇒ the `>`-frame closes. `gt_level >= 1` (only `>`-frames call this).
fn gt_cont_view(raw: &str, strip: usize, gt_level: usize) -> Option<&str> {
    md_quote_cont_slice(gt_peel(crate::org::strip_view(raw, strip), gt_level - 1))
}

/// Null the inline spans of the DIRECT leaf blocks WITHOUT recursing into container children
/// (`Quote`/`Custom`/`List`). Used at a `>`-frame's close: that frame's nested `>`-quote children
/// and its `Step::GtFallback` reparse children are ALREADY fully null'd, so re-descending them is
/// redundant AND — on a deep (uncapped) `>`-staircase — an O(depth) native-recursion stack
/// overflow. The md twin of org's `none_out_frame_leaves`.
fn none_out_frame_leaves(blocks: &mut [Block]) {
    for b in blocks.iter_mut() {
        match b {
            Block::Paragraph { inline, .. }
            | Block::Heading { inline, .. }
            | Block::Bullet { inline, .. }
            | Block::FootnoteDef { inline, .. } => none_out_inlines(inline),
            Block::Table { header, rows, .. } => {
                if let Some(h) = header {
                    for cell in h.iter_mut() {
                        none_out_inlines(cell);
                    }
                }
                for row in rows.iter_mut() {
                    for cell in row.iter_mut() {
                        none_out_inlines(cell);
                    }
                }
            }
            // Container children are already fully null'd — do NOT recurse (the overflow).
            _ => {}
        }
    }
}

/// The outcome of classifying ONE line (`dispatch_md_line`): either advance to line
/// `Next(ni)`, or recognize a container opener (`Open`) whose body is `[i+1, close)` and
/// whose closer is line `close`. `Open` defers the body handling to the driver:
/// `parse_md_streaming` pushes a stack frame for the body and keeps scanning the same
/// line array (no recursion on the body). (The dispatch helper never flushes the
/// paragraph for `Open` — the driver does, just before pushing the frame.)
///
/// `indent_strip` = the leading-ws count of the VIEWED first body line (0 for the bare
/// `#+BEGIN_X` form; first-body-line leading ws for re-bulleted bodies). The main loop
/// adds it to the parent's `strip` to get `child_strip`. `span_start` = byte offset of
/// the block's span start (line_start for the bare form; content_off for re-bulleted).
enum Step<'a> {
    Next(usize),
    Open { close: usize, builder: Builder, indent_strip: usize, span_start: usize },
    /// A markdown `>`-blockquote opener recognized at line `i` (document root, the bullet-lazy
    /// path, OR a NESTED opener inside a `>`-frame). The driver pushes a Quote `Frame` at
    /// `gt_level+1` whose OPENER line is `i` — re-dispatched, so `i` does NOT advance — bounded
    /// DYNAMICALLY by `md_quote_cont_slice` (closes on the first continuation `None`). Mirrors
    /// org's `Step::OpenQuote`; replaces `build_md_quote` (no `String`, no residual recursion, so a
    /// `>`-staircase is iterative frames — each line viewed once at its own depth ⇒ O(n)).
    OpenQuote { opener_content: &'a str, span_start: usize },
    /// A `>`-frame body line whose de-`>`'d view opens a construct that can't be classified
    /// copy-free against the global raw-input indexes/scanners (fenced code / `#+BEGIN_X` callout /
    /// LaTeX env / block hiccup / directive with raw eol-swallow) or needs a raw-input multi-line
    /// builder (table / list / def-list). The driver reparses the frame's REMAINING body `[i, end)`
    /// — prefixed by any pending copy-free paragraph so a degraded construct coalesces / a real
    /// block trims its preceding Break — ONCE via `reparse_block_content`, then jumps to `end`.
    GtFallback,
}

/// One open container on the streaming driver's explicit stack. Every re-dispatched
/// `#+BEGIN_X` callout body (Quote/Custom) — clean-window (indent-0 body) or strip-view
/// (indented body, `null_spans = true`) — lives here as a heap `Frame`. `in_quote` marks
/// the "in block content" context; `in_item` marks list-item content.
struct Frame<'a> {
    hi: usize,                       // EXCLUSIVE closer line index; line `hi` is the closer.
    in_quote: bool,                  // is THIS an in-block-content body (`>`-quote OR `#+BEGIN_X`
                                     // callout)? (suppresses heading/bullet/property/footnote/
                                     // drawer, trims para breaks before blocks — mldoc
                                     // `block_content_parsers`).
    in_item: bool,                   // is THIS a markdown list-item content body? (mldoc's
                                     // `list_content_parsers`: the in-block-content grammar
                                     // MINUS Lists AND — at the document level — Directive. So
                                     // it suppresses everything `in_quote` does, PLUS list/
                                     // def-list, PLUS Directive unless ALSO inside a quote.)
    out: Vec<Block>,                 // children of THIS body.
    para: Option<(usize, usize)>,    // the open paragraph byte-window for THIS body.
    // In a `null_spans` (re-bulleted / strip>0) frame the paragraph's raw byte-window keeps
    // the per-line indent (only the first line is de-indented). Instead we accumulate the
    // VIEWED (`line_text`) line texts joined with `\n`, which normalizes the cumulative
    // indent (via `strip`). Active iff a paragraph is open in a null_spans frame (clean
    // frames keep `para`'s `(start,end)` fast path).
    para_buf: Option<String>,
    builder: Option<Builder>,        // the opener → emitted on pop (None for the root).
    open_span_start: usize,          // byte offset of the opener line start (for the span).
    strip: usize,       // cumulative de-indent applied to every body-line view (0 = root/clean).
    null_spans: bool,   // body was re-bulleted (strip>0) → null inline spans on pop.
    // P3c `>`-blockquote container frame (`gt_level == 0` for the root / `#+BEGIN_X` callout
    // frames). `gt_level` = the cumulative `>`-peel applied to CONTINUATION lines (composed on
    // top of the indent `strip`); the OPENER line (`open_line`) is instead viewed via
    // `opener_content` (the up-to-2 `>` peel → the opener-2/continuation-1 asymmetry). A
    // `>`-frame's extent is DYNAMIC: it closes when a continuation view is `None`, bounded above
    // by the inherited `hi`. `null_spans` is always true for a `>`-frame (a `>`-body is transformed).
    gt_level: usize,
    open_line: usize,
    opener_content: &'a str,
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
/// `root_in_quote = false` at the document level; `true` when re-dispatching a `>`-blockquote
/// body (F1: the body is parsed with the full block grammar MINUS heading/bullet/property/
/// footnote/drawer, and a paragraph's trailing Break is trimmed before a following block).
fn parse_md_streaming<'a>(input: &'a str, root_in_quote: bool, root_in_item: bool) -> Vec<Block> {
    let mut lines = split_lines(input);
    let last_rbracket = input.rfind(']');
    let (end_trie, drawer_end_idxs, fence_lines) = build_indexes(&lines);
    let n = lines.len();

    // Root frame spans the whole input; its `out`/`para` are the document's. Non-root frames
    // are callout bodies, popped (and emitted via `builder.finish`) when `i` reaches their `hi`.
    let mut stack: Vec<Frame<'a>> = vec![Frame {
        hi: n,
        in_quote: root_in_quote,
        in_item: root_in_item,
        out: Vec::new(),
        para: None,
        para_buf: None,
        builder: None,
        open_span_start: 0,
        strip: 0,
        null_spans: false,
        gt_level: 0,
        open_line: 0,
        opener_content: "",
    }];
    let mut fence_cursor: usize = 0; // monotone & shared across the whole pass (`i` is monotone).
    let mut drawer_cursor: usize = 0; // ditto, for `:END:` lookups (find_drawer_end).
    // The list-collapse memo (mldoc's recursive list-parser failure bubble): when a list item's
    // deeper continuation is an unparseable list-item shape, the list collapses; `collapse_floor`
    // marks the trigger line so the collapsed region is NOT re-scanned as a list (linearity). One
    // per streaming pass, shared across frames — `i` is monotone, so a past floor never leaks
    // into a later body. See `collect_list_md` / `collapse_resume`.
    let mut collapse_floor: usize = 0;
    // F4/M3: set by an empty heading/bullet marker to the marker line's END offset (the boundary
    // between the marker's trailing-ws `" \n"` and any following blank lines). Consumed by the
    // NEXT line's dispatch: a drop-trigger block drops the marker portion (keeping intervening
    // blank breaks as their own paragraph — M3), a truly-empty line carries the flag forward, and
    // any non-empty line clears it (the marker ws is then kept).
    let mut ws_drop: Option<usize> = None;
    let mut i = 0;

    loop {
        // Close frames ending at line `i`. A HARD frame (root / `#+BEGIN_X` callout, `gt_level==0`)
        // closes when `i` reaches its EXCLUSIVE closer `hi`, CONSUMING that `#+END_` line (`i+=1`)
        // and swallowing trailing blanks (F6). A `>`-frame (`gt_level>0`) has DYNAMIC extent: it
        // closes when `i` reaches the inherited `hi` OR — past the opener — its continuation view
        // is `None`; it does NOT consume a closer (`i` unchanged; the line belongs to the parent).
        // Each `>`-frame line is thus viewed once at its own depth ⇒ O(n) (no per-frame run re-scan).
        let mut gt_closed = false;
        while stack.len() > 1 {
            let (close, consume) = {
                let top = stack.last().unwrap();
                if top.gt_level > 0 {
                    if i >= top.hi {
                        (true, false)
                    } else if i <= top.open_line {
                        (false, false) // the opener line is always dispatched, never closes here
                    } else {
                        (gt_cont_view(lines[i].text, top.strip, top.gt_level).is_none(), false)
                    }
                } else {
                    (top.hi == i, true)
                }
            };
            if !close {
                break;
            }
            let mut f = stack.pop().unwrap();
            flush_para(&mut f.out, &mut f.para, &mut f.para_buf, input, false);
            // Transformed body (strip > 0 or a `>`-frame): inline spans don't map to global
            // byte-ranges → null them. A `>`-frame's container children are already null'd
            // (bottom-up), so null only its OWN leaves NON-recursively — a deep (uncapped)
            // `>`-staircase would overflow the native stack under recursive `none_out_blocks`.
            if f.null_spans {
                if f.gt_level > 0 {
                    none_out_frame_leaves(&mut f.out);
                } else {
                    none_out_blocks(&mut f.out);
                }
            }
            // Hard frame: line `i` is the `#+END_` closer → span ends at `lines[i].end`. `>`-frame:
            // line `i` is NOT in the run → the last body line is `i-1` (a `>`-frame closes only for
            // `i > open_line`, so `i >= 1`).
            let span_end = if consume { lines[i].end } else { lines[i - 1].end };
            let span = Some(Span(f.open_span_start, span_end));
            let block = f.builder.unwrap().finish(f.out, span);
            stack.last_mut().unwrap().out.push(block);
            if consume {
                i += 1; // CONSUME the closer line.
                // mldoc ends a `#+BEGIN_X` callout with `<* optional eols` (F6): swallow following
                // blank lines. Bounded by the parent's `hi`; whitespace-only lines are NOT eols.
                let top_hi = stack.last().unwrap().hi;
                while i < top_hi && lines[i].text.is_empty() {
                    i += 1;
                }
            } else {
                gt_closed = true;
            }
        }
        // F6 for `>`-frames: mldoc's `md_blockquote = … <* optional eols` swallows the trailing
        // blank(s) AFTER the whole quote nest closes. Done ONCE here (not per-frame) so an inner
        // close can't advance `i` past the blank and make an OUTER frame lazily absorb what follows.
        // ALSO reproduces org's `block_absorbs`: when a NESTED `>`-quote closes and its parent
        // `>`-frame continues, the parent absorbs a following `>`-blank continuation (`"> "` views to
        // "" but is NOT raw-empty, so the raw F6 loop misses it) so it doesn't become an empty
        // paragraph — mirroring what the old `build_md_quote`→reparse path did via a de-`>`'d "".
        if gt_closed {
            let (top_hi, top_gt, top_strip) = {
                let t = stack.last().unwrap();
                (t.hi, t.gt_level, t.strip)
            };
            while i < top_hi
                && (lines[i].text.is_empty()
                    || (top_gt > 0 && gt_cont_view(lines[i].text, top_strip, top_gt) == Some("")))
            {
                i += 1;
            }
        }
        if i >= n {
            break;
        }
        let step = {
            let top = stack.last_mut().unwrap();
            let hi = top.hi;
            let in_quote = top.in_quote;
            let in_item = top.in_item;
            let strip = top.strip;
            let null_spans = top.null_spans;
            let gt_level = top.gt_level;
            // `>`-frame OPENER line ⇒ the up-to-2-`>` peel view (`opener_content`); else `None`
            // and dispatch computes the `gt_level`-peel continuation view itself.
            let gt_opener = if gt_level > 0 && i == top.open_line {
                Some(top.opener_content)
            } else {
                None
            };
            dispatch_md_line(
                i,
                &mut lines,
                &mut top.out,
                &mut top.para,
                &mut top.para_buf,
                in_quote,
                in_item,
                &mut ws_drop,
                &mut collapse_floor,
                hi,
                &end_trie,
                &drawer_end_idxs,
                &fence_lines,
                &mut fence_cursor,
                &mut drawer_cursor,
                last_rbracket,
                input,
                strip,
                null_spans,
                gt_level,
                gt_opener,
            )
        };
        match step {
            Step::Next(ni) => i = ni,
            Step::Open { close, builder, indent_strip, span_start } => {
                // The dispatch helper did NOT flush para for `Open` (but it DID apply the F4
                // ws-drop) — flush the parent's para (trim if the parent is a quote body), then
                // push the body frame. M1: a `#+BEGIN_X` callout body (Quote OR Custom) is
                // block-parsed with the SAME in-block-content grammar as a `>`-blockquote body —
                // mldoc reparses both with `block_content_parsers` (suppress heading/bullet/
                // property/footnote/drawer → text; trim a paragraph's trailing Break before a
                // following block). So the child frame is ALWAYS `in_quote = true`, regardless of
                // the parent context (the md mirror of org's F2 custom child_ctx).
                let top_strip = stack.last().unwrap().strip;
                {
                    let top = stack.last_mut().unwrap();
                    let pq = top.in_quote;
                    flush_para(&mut top.out, &mut top.para, &mut top.para_buf, input, pq);
                }
                // child_strip = parent.strip + indent_strip (cumulative composition). A re-bulleted
                // body (indent_strip > 0) → null_spans so inline positions are de-indented on pop.
                let child_strip = top_strip + indent_strip;
                let null_spans = child_strip > 0;
                stack.push(Frame {
                    hi: close,
                    in_quote: true,
                    in_item: false, // a `#+BEGIN_X` callout body is in-block-content, NOT list-item content
                    out: Vec::new(),
                    para: None,
                    para_buf: None,
                    builder: Some(builder),
                    open_span_start: span_start,
                    strip: child_strip,
                    null_spans,
                    gt_level: 0,
                    open_line: 0,
                    opener_content: "",
                });
                i += 1;
            }
            Step::OpenQuote { opener_content, span_start } => {
                // Push a `>`-Quote container frame (P3c): the opener line `i` is RE-DISPATCHED inside
                // it (`i` unchanged), giving the opener-2 peel via `opener_content` and — if that
                // still opens a quote — the single-line `⌈N/2⌉` and the multi-line staircase, all as
                // iterative frames (no `build_md_quote`, no String, no residual recursion).
                let (p_hi, p_strip, p_gt) = {
                    let t = stack.last().unwrap();
                    (t.hi, t.strip, t.gt_level)
                };
                {
                    let top = stack.last_mut().unwrap();
                    // A preceding paragraph drops its trailing Break before this (nested) quote when
                    // already inside a block-content / list-item body (`between_eols`) — same as
                    // `Step::Open`. (The dispatch already applied the F4 marker ws-drop.)
                    let trim = top.in_quote || top.in_item;
                    flush_para(&mut top.out, &mut top.para, &mut top.para_buf, input, trim);
                }
                stack.push(Frame {
                    hi: p_hi, // inherit the enclosing hard bound (a `>`-quote can't cross a callout closer)
                    in_quote: true,
                    in_item: false,
                    out: Vec::new(),
                    para: None,
                    para_buf: None,
                    builder: Some(Builder::Quote),
                    open_span_start: span_start,
                    strip: p_strip,   // inherit the ancestor indent strip
                    null_spans: true, // a `>`-body is transformed ⇒ null inline spans on pop
                    gt_level: p_gt + 1,
                    open_line: i,
                    opener_content,
                });
                // i unchanged: the opener line is re-dispatched inside the new frame.
            }
            Step::GtFallback => {
                // §3 (md): the top `>`-frame's remaining body opens a construct that can't be
                // classified copy-free. Reparse `[i, end)` de-`>`'d ONCE via `reparse_block_content`,
                // PREFIXED by any pending copy-free paragraph (`para_buf`) so a degraded construct
                // coalesces with it and a real block's preceding Break is trimmed — byte-identical to
                // a whole-body reparse across the seam.
                let (p_hi, p_strip, p_gt, p_open, p_oc) = {
                    let t = stack.last().unwrap();
                    (t.hi, t.strip, t.gt_level, t.open_line, t.opener_content)
                };
                let mut de_gt = {
                    let top = stack.last_mut().unwrap();
                    top.para = None;
                    top.para_buf.take().unwrap_or_default()
                };
                let vi = if i == p_open {
                    p_oc
                } else {
                    gt_cont_view(lines[i].text, p_strip, p_gt).unwrap_or("")
                };
                de_gt.push_str(vi);
                de_gt.push('\n');
                let mut end = i + 1;
                while end < p_hi {
                    match gt_cont_view(lines[end].text, p_strip, p_gt) {
                        Some(v) => {
                            de_gt.push_str(v);
                            de_gt.push('\n');
                            end += 1;
                        }
                        None => break,
                    }
                }
                let children = reparse_block_content(&de_gt);
                stack.last_mut().unwrap().out.extend(children);
                i = end; // the frame closes next iteration (i == end ⇒ continuation `None` or `hi`)
            }
        }
    }

    // Only the root remains (all callout bodies closed before EOF); flush its paragraph.
    let mut root = stack.pop().unwrap();
    flush_para(&mut root.out, &mut root.para, &mut root.para_buf, input, false);
    root.out
}

/// Classify ONE md line `i` in the body bounded by `hi` (EXCLUSIVE closer line index), writing
/// any completed block into `out` / accumulating into `para`, and return a `Step`. This is the
/// single per-line dispatch ladder used by the streaming driver (which pushes a frame on `Open`).
/// The whole streaming-correctness story lives here: every forward closer-search
/// is bounded by `hi` / `body_end`, so a closer/`\end{}`/`]`/run-line BELONGS to this body and
/// never the enclosing one. At the top level `hi == lines.len()` (and `body_end == input.len()`),
/// so all bounds are no-ops and the behavior is identical to the pre-refactor inline ladder.
#[allow(clippy::too_many_arguments)]
fn dispatch_md_line<'a>(
    i: usize,
    lines: &mut [Line<'a>],
    out: &mut Vec<Block>,
    para: &mut Option<(usize, usize)>,
    para_buf: &mut Option<String>,
    in_quote: bool,
    in_item: bool,
    ws_drop: &mut Option<usize>,
    collapse_floor: &mut usize,
    hi: usize,
    end_trie: &EndTrie,
    drawer_end_idxs: &[usize],
    fence_lines: &[usize],
    fence_cursor: &mut usize,
    drawer_cursor: &mut usize,
    last_rbracket: Option<usize>,
    input: &'a str,
    strip: usize,
    null_spans: bool,
    // P3c `>`-frame context: `gt_level == 0` for a hard (root / callout) frame — behavior is
    // identical to before. `gt_level > 0` ⇒ the current line is viewed at the frame's cumulative
    // `>`-peel; `gt_opener` is `Some(opener_content)` on the frame's OPENER line (the up-to-2 peel),
    // `None` on a continuation (dispatch computes the `gt_level`-peel view itself).
    gt_level: usize,
    gt_opener: Option<&'a str>,
) -> Step<'a> {
    // Copy the line's fields out (a `&'a str` + two `usize`s, none borrowing the `lines`
    // slice) so the block-hiccup remainder split (step 11d') can REWRITE `lines[ri]` in place.
    // `t` is the line's VIEW: the strip-viewed text for a hard frame, or — inside a `>`-frame —
    // `opener_content` (opener) / the `gt_level`-peel continuation view. `line_content_end_orig`
    // uses the original text length so `parse_latex_env`'s `line_end` bound is correct (hard frames
    // only — a `>`-frame routes latex to the §3 fallback before `parse_latex_env` is reached).
    let t = if gt_level == 0 {
        line_text(lines, i, strip)
    } else if let Some(oc) = gt_opener {
        oc
    } else {
        // Continuation view; the driver's close phase already ensured `Some` (else it closed the
        // frame instead of dispatching), so `unwrap_or("")` is just a defensive fallback.
        gt_cont_view(lines[i].text, strip, gt_level).unwrap_or("")
    };
    let line_start = lines[i].start;
    let line_end = lines[i].end;
    let line_content_end_orig = line_start + lines[i].text.len();
    // "in block content" = a `>`-quote / `#+BEGIN_X` callout body (`in_quote`) OR a markdown
    // list-item content body (`in_item`). Both trim a paragraph's trailing Break before a
    // following block and suppress heading/bullet/property/footnote/drawer (mldoc's
    // `block_content_parsers` / `list_content_parsers` omit those leaf parsers). F1/M-item.
    let in_block_content = in_quote || in_item;
    let trim = in_block_content;
    // F4/M3: read + clear the empty-marker ws-drop flag (the marker line's END offset) set by a
    // PREVIOUS line. A drop-trigger block (fence/callout/hr/table/`>`-quote/`$$`/raw-html) drops
    // the marker's `" \n"` portion (keeping intervening blank breaks — M3); a truly-empty line
    // re-arms it below (step 12); any other line leaves it cleared (marker ws kept).
    let was_ws_drop = ws_drop.take();
    // Byte offset where THIS body ends (the closer line's start, or EOF at the root). Used to
    // CLAMP the to-end-of-input forward-scanners (`parse_latex_env`, `parse_hiccup`).
    let body_end = if hi < lines.len() { lines[hi].start } else { input.len() };

    // P3c §3: inside a `>`-frame, a de-`>`'d view opening a construct whose recognition needs the
    // literal `>`s stripped from what the GLOBAL raw-input indexes/scanners see — fenced code /
    // `#+BEGIN_X` callout (`fence_lines`/`EndTrie` never record a `>`-prefixed closer), a LaTeX env
    // / block hiccup (`parse_latex_env`/`parse_hiccup` scan raw bytes), a directive (its `<* eols`
    // swallow reads raw lines and would cross the `>`-run boundary), or a raw-input multi-line
    // BUILDER (table cells / list / def-list read raw `input`) — cannot be handled copy-free. Hand
    // the frame's remaining body to the bounded de-`>`'d reparse (`Step::GtFallback`). The
    // single-line leaves (`$$`/raw-html/hr), paragraphs, and NESTED `>`-quotes stay copy-free below,
    // so a pure-quote staircase never reaches here. (Over-routing is only a perf cost — the fallback
    // runs the identical ladder — so these tells may be conservative.)
    if gt_level > 0
        && (fence_marker(t).is_some()
            || callout_begin(t).is_some()
            || crate::org::directive(t).is_some()
            || t.trim_start().starts_with("\\begin{")
            || t.trim_start().starts_with("[:")
            || md_table_row(t)
            || md_marker(t).is_some()
            || (!t.trim_start().is_empty()
                && i + 1 < hi
                && gt_cont_view(lines[i + 1].text, strip, gt_level).is_some_and(is_def_opener)))
    {
        return Step::GtFallback;
    }

    // 1. fenced code (Src) — ON-DEMAND, context-aware. A fence-marker line the loop REACHES is
    // an opener at THIS level. Its closer = the first whole-line fence marker after it of EITHER
    // char (`find_matching_fence`). The closer must lie inside THIS body (`< hi`); a match `>= hi`
    // belongs to an enclosing body, so the fence here is unclosed → fall through to paragraph.
    if let Some((_c, mend)) = fence_marker(t) {
        if let Some(close) = find_matching_fence(fence_lines, fence_cursor, i) {
            if close < hi {
                drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blanks.
                flush_para(out, para, para_buf, input, trim);
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
                drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blanks.
                // B: SRC/EXAMPLE are RAW-body blocks consumed in place — NOT re-dispatched
                // containers. mldoc's markdown block parser (block0.ml, shared with org) maps
                // `#+BEGIN_SRC`→Src{lang, code} and `#+BEGIN_EXAMPLE`→Example{code}, with the
                // body indent-cleared (`block_code_texts`) and the lang the first token after
                // the name (`begin_lang`) — MIRRORING org.rs exactly. Trailing truly-empty
                // lines are swallowed (mldoc `<* optional eols`, like the fence handler), so a
                // following paragraph gets no leading Break. EXPORT/COMMENT are DEFERRED (they
                // need new projection kinds and diverge in both formats) → stay Custom.
                if name.eq_ignore_ascii_case("SRC") || name.eq_ignore_ascii_case("EXAMPLE") {
                    flush_para(out, para, para_buf, input, trim);
                    let (block, ni) = raw_callout_block(&name, t, lines, i, close, hi, line_start);
                    out.push(block);
                    return Step::Next(ni);
                }
                return Step::Open { close, builder: callout_builder(&name), indent_strip: 0, span_start: line_start };
            }
            // closer is outside this body → fall through.
        }
        // no matching END → fall through (treat as paragraph text).
    }

    // 2c. standalone directive `#+KEY: value` (KEY non-empty, not `BEGIN_…`). The md driver had
    // NO standalone-directive parser, so a bare `#+TITLE: x` was mis-classified as a Paragraph
    // whose `#+name` became a phantom `+name` page-tag. mldoc — and lsdoc's OWN org driver —
    // parse it as a `Block::Directive{name, value}` with a RAW value (no inline parse, no ref
    // walk), identically in BOTH formats. So we MIRROR the org classifier byte-for-byte by reusing
    // `crate::org::directive` (leading-ws tolerant key, value LEFT-trimmed only, `BEGIN_…`
    // excluded). NOT gated on `in_quote`: Directive IS in mldoc's `block_content_parsers`, so it
    // fires inside a `>`-quote / `#+BEGIN_X` body too (org gates only `in_item`, never the quote
    // body). Placed AFTER the `#+BEGIN_X` callout opener (so block markers aren't swallowed: a
    // `#+BEGIN_…` is rejected by `directive`'s `BEGIN_` guard, and a colon-free `#+END_X` has no
    // `:` so it stays a paragraph — matching mldoc). mldoc `Directive.parse` = `… <* optional
    // eols`, swallowing following truly-empty lines (the md mirror of org's `*absorb = true`; the
    // span extends over them). A directive is ALSO a drop-trigger block: an empty heading/bullet
    // marker's trailing-ws paragraph is dropped before it (F4/M3, e.g. `## \n#+a: 1`).
    // Suppressed ONLY in DOCUMENT-level list-item content (`in_item && !in_quote`): mldoc's
    // top-level `list_content_parsers` (mldoc_parser.ml) OMITS Directive — so a folded
    // `#+TITLE: x` stays paragraph text with a `#+name` inline tag — whereas inside a `>`-quote /
    // callout the WITH-Directive `list_content_parsers` (block0.ml) keeps it (verified vs oracle).
    if let Some((name, value)) = crate::org::directive(t).filter(|_| in_quote || !in_item) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop the marker `" \n"`, keep blanks.
        flush_para(out, para, para_buf, input, trim);
        let mut ni = i + 1;
        let mut end = line_end;
        while ni < hi && lines[ni].text.is_empty() {
            end = lines[ni].end;
            ni += 1;
        }
        out.push(Block::Directive { name, value, span: Some(Span(line_start, end)) });
        return Step::Next(ni);
    }

    // 2b. LaTeX environment `\begin{X} … \end{X}` (mldoc Latex_env, before Block). CLAMP the
    // `\end{}` search to `&input[..body_end]` so an `\end{X}` outside this body is not captured
    // (verified load-bearing: `#+BEGIN_QUOTE\n\begin{eq}\n#+END_QUOTE\n\end{eq}`).
    // `line_content_end_orig` (set above from the original text length) keeps the closing-brace
    // search in-bounds even when `t` is a strip-view shorter than the original line.
    if let Some((name, content, consumed_end)) =
        crate::inline::parse_latex_env(&input[..body_end], line_start, line_content_end_orig)
    {
        // latex_env is the ONLY block_content construct that does NOT consume the preceding
        // eol (no `optional eols`/`between_eols`), so inside a `>`-quote body a paragraph KEEPS
        // its trailing Break before it, and the eol AFTER it becomes a `Paragraph_Sep` →
        // a Break-paragraph (mldoc's `Paragraph.sep`-last ordering). Never trim. F1.
        flush_para(out, para, para_buf, input, false);
        let mut ni = i + 1;
        while ni < lines.len() && lines[ni].start < consumed_end {
            ni += 1;
        }
        // In a null_spans frame `content` sliced from raw `input` keeps the per-line indent.
        // Re-run parse_latex_env over the VIEWED (de-indented) body window to get the correct
        // content; the STRUCTURE (name / consumed_end / ni) stays from the raw pass. O(n): each
        // body line belongs to exactly one leaf construct.
        let content = if null_spans {
            let mut s = String::new();
            for k in i..ni {
                s.push_str(line_text(lines, k, strip));
                s.push('\n');
            }
            let first_len = line_text(lines, i, strip).len();
            crate::inline::parse_latex_env(&s, 0, first_len)
                .map(|(_, c, _)| c)
                .unwrap_or(content)
        } else {
            content
        };
        out.push(Block::LatexEnv { name, content, span: Some(Span(line_start, consumed_end)) });
        // resume at the first line starting at/after consumed_end (always > i, and <= hi since
        // consumed_end <= body_end == lines[hi].start).
        if in_quote {
            // The trailing eol(s) between the `\end{}` and the next line start a Break-paragraph.
            let trail_end = if ni < lines.len() { lines[ni].start } else { body_end };
            if consumed_end < trail_end {
                *para = Some((consumed_end, trail_end));
                // Keep para/para_buf in lockstep in a null_spans frame: the trailing region is a
                // line terminator (a single Break); the raw `\n` normalizes to `\n`.
                if null_spans {
                    *para_buf = Some("\n".to_string());
                }
            }
        }
        return Step::Next(ni);
    }

    // 3. heading. `level` = 1 + leading-ws (mldoc bumps level per leading
    // space/tab, uncapped); `size` = `#`-count. An empty heading whose line has
    // trailing whitespace splits into [heading, paragraph(trailing ws)].
    // (suppressed inside a `>`-blockquote body — mldoc `block_content_parsers` omits Heading,
    // so `# h` / a `-` bullet there stay paragraph text. F1.)
    if let Some((level, size, hend)) = heading_at(t).filter(|_| !in_block_content) {
        flush_para(out, para, para_buf, input, trim);
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
            *ws_drop = Some(line_end); // F4/M3: marker line-end boundary; droppable before a block.
            return Step::Next(i + 1);
        }
        out.push(Block::Heading {
            level,
            size: Some(size),
            inline: stub_inline(title, crate::inline::ptr_base(title, input)),
            marker,
            priority,
            htags: vec![],
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 4. horizontal rule (before dash bullet / list)
    if is_hr(t) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blank breaks.
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::Hr { span: Some(Span(line_start, line_end)) });
        return Step::Next(i + 1);
    }

    // 5. `-` bullet (mldoc Heading{unordered}) — suppressed inside a `>`-blockquote body
    // (mldoc `block_content_parsers` omits Heading, so `- x` there stays a paragraph). F1.
    if let Some(level) = dash_bullet_level(t).filter(|_| !in_block_content) {
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
                    flush_para(out, para, para_buf, input, trim);
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
        // (a2) `#+BEGIN_<TYPE>` block opener on the bullet line (B): the title lookahead reparses
        // the bullet content as a block, so `- #+BEGIN_<TYPE> … #+END_<TYPE>` → [empty bullet,
        // <block>] where <block> is dispatched IDENTICALLY to the bare `#+BEGIN_<TYPE>` form (step
        // 2): SRC→Src / EXAMPLE→Example (raw-body, consumed in place, `raw_callout_block`), QUOTE→
        // Quote / anything-else→Custom{name lowercased} (a re-dispatched container via `Step::Open`,
        // body block-parsed by the driver with the in-block-content grammar using a strip-view
        // frame instead of block_code + reparse_block_content). Only splits when the block CLOSES
        // inside this body (`< hi`); otherwise the bullet content has no matching END here and it
        // stays a normal bullet titled `#+BEGIN_<TYPE> …` (mldoc).
        if let Some(bname) = callout_begin(content) {
            if let Some(close) = end_trie.find(&bname, i).filter(|&c| c < hi) {
                flush_para(out, para, para_buf, input, trim);
                empty_bullet!();
                if bname.eq_ignore_ascii_case("SRC") || bname.eq_ignore_ascii_case("EXAMPLE") {
                    let (block, ni) = raw_callout_block(&bname, content, lines, i, close, hi, content_off);
                    out.push(block);
                    return Step::Next(ni);
                }
                // QUOTE→Quote / anything-else→Custom: zero-copy strip-view frame (P2). The body
                // carries the bullet continuation indent; `indent_strip` = leading ws of the
                // VIEWED first body line (parent strip already applied via line_text;
                // child_strip = strip + indent_strip in the Step::Open handler). The empty
                // bullet is already in `out` before we return Open, so ordering is preserved.
                // SRC/EXAMPLE stay raw (raw_callout_block above); only QUOTE/Custom become frames.
                let indent_strip =
                    if close > i + 1 { leading_ws(line_text(lines, i + 1, strip)) } else { 0 };
                return Step::Open { close, builder: callout_builder(&bname), indent_strip, span_start: content_off };
            }
        }
        // (b) markdown blockquote opener on the bullet line (P3c, lazy continuation). Emit the
        // empty bullet FIRST, then hand the driver a `Step::OpenQuote` that pushes a `>`-Quote frame
        // whose opener line is `i` (re-dispatched; the bullet content `content` becomes the frame's
        // view). The run is bounded DYNAMICALLY by the continuation predicate — itself bounded by the
        // frame's inherited `hi` (the closer line is never a quote-continuation, so absorbing it
        // would wrongly swallow the frame's closer — verified load-bearing). This path fires only at
        // the document root (bullets suppressed in every in-block-content body), so `strip == 0`.
        if let Some(inner) = md_quote_first_slice(content) {
            flush_para(out, para, para_buf, input, trim);
            empty_bullet!();
            return Step::OpenQuote { opener_content: inner, span_start: content_off };
        }
        // (c) property line on the bullet line (mldoc heading0.ml: the title is a
        // lookahead, and `markdown_property` is one of the constructs tried — so
        // `- key:: value` yields an EMPTY bullet then a Property_Drawer that BEGINS
        // at the bullet content and folds in subsequent property/directive lines
        // (exactly like step 8). `content` is post-`#{1,n}`-strip, matching the size
        // run; the property `key` rejects bullet prefixes via its space check.
        if let Some(kv) = property(content) {
            flush_para(out, para, para_buf, input, trim);
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
            flush_para(out, para, para_buf, input, trim);
            empty_bullet!();
            out.push(Block::Hr { span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (e) block displayed-math opener `$$ … $$` (single line).
        if let Some(math) = displayed_math(content) {
            flush_para(out, para, para_buf, input, trim);
            empty_bullet!();
            out.push(Block::DisplayedMath { text: math, span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (f) raw-HTML opener.
        if is_raw_html(content) {
            flush_para(out, para, para_buf, input, trim);
            empty_bullet!();
            out.push(Block::RawHtml { text: content.to_string(), span: Some(Span(content_off, line_end)) });
            return Step::Next(i + 1);
        }
        // (g) LaTeX environment opener `\begin{X} … \end{X}` (may span lines). CLAMP as in 2b.
        if let Some((name, lc, consumed_end)) =
            crate::inline::parse_latex_env(&input[..body_end], content_off, line_start + t.len())
        {
            flush_para(out, para, para_buf, input, trim);
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
            flush_para(out, para, para_buf, input, trim);
            empty_bullet!();
            let mut texts: Vec<&str> = vec![content];
            let mut ni = i + 1;
            while ni < hi && md_table_row(lines[ni].text) {
                texts.push(lines[ni].text);
                ni += 1;
            }
            out.push(build_table_from_texts(&texts, content_off, lines[ni - 1].end, input));
            return Step::Next(ni);
        }
        // (i) footnote-definition opener — only WITHOUT a `#` prefix (with a `#`,
        // `[^id]` is an inline footnote ref in the heading title, per mldoc heading0).
        if size.is_none() {
            if let Some((fname, fbody)) = footnote_def(content) {
                flush_para(out, para, para_buf, input, trim);
                empty_bullet!();
                out.push(Block::FootnoteDef {
                    name: fname,
                    inline: stub_inline(fbody, crate::inline::ptr_base(fbody, input)),
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
        flush_para(out, para, para_buf, input, trim);
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
            *ws_drop = Some(line_end); // F4/M3: marker line-end boundary; droppable before a block.
            return Step::Next(i + 1);
        }
        out.push(Block::Bullet {
            level,
            size,
            inline: stub_inline(title, crate::inline::ptr_base(title, input)),
            marker,
            priority,
            htags: vec![],
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 6. footnote definition — suppressed inside a `>`-blockquote body (mldoc
    // `block_content_parsers` omits Footnote, so `[^id]: …` stays paragraph text). F1.
    if let Some((fname, content)) = footnote_def(t).filter(|_| !in_block_content) {
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::FootnoteDef {
            name: fname,
            inline: stub_inline(content, crate::inline::ptr_base(content, input)),
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 7. table (group of consecutive table-row lines, bounded by `hi`)
    if md_table_row(t) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blank breaks.
        flush_para(out, para, para_buf, input, trim);
        let start = i;
        let mut ni = i;
        while ni < hi && md_table_row(lines[ni].text) {
            ni += 1;
        }
        out.push(build_table(&lines[start..ni], lines[start].start, lines[ni - 1].end, input));
        return Step::Next(ni);
    }

    // 8. property drawer (group of consecutive `key:: value` lines, bounded by `hi`). mldoc folds
    // trailing `#+name: value` org directives into the same drawer (drawer.ml
    // `many1 (parse1 <|> parse2)`), so `a:: 1\n#+b: 2` → props a, b. Suppressed inside a
    // `>`-blockquote body (mldoc omits the markdown property from `block_content_parsers`). F1.
    if property(t).is_some() && !in_block_content {
        flush_para(out, para, para_buf, input, trim);
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

    // 9. list (`*`/`+`/`N.` items, bounded by `hi`) — a faithful port of mldoc's shared
    // `lists0.ml` list parser (markdown branch). Each item folds its indented multi-line
    // continuation (de-indented via per-line `String.trim`) into its content, which is re-parsed
    // with the list-item content grammar (`reparse_item_content`, the `in_item` driver). Deeper
    // is-item lines become children (flat collection + `nest_items`); an unparseable deeper
    // is-item shape COLLAPSES the list (the recursive-parser failure bubble — `collapse_resume` +
    // `collapse_floor`). Disabled inside list-item content (`!in_item`: `list_content_parsers`
    // omits `Lists.parse`); `collapse_floor` skips list-starts inside an already-collapsed region.
    if !in_item && i >= *collapse_floor && md_marker(t).is_some() {
        match collect_list_md(lines, i, hi, in_quote) {
            Ok((block, next)) => {
                flush_para(out, para, para_buf, input, trim);
                out.push(block);
                return Step::Next(next);
            }
            Err(Collapse { kept, resume, trigger }) => {
                *collapse_floor = trigger;
                if let Some(block) = kept {
                    flush_para(out, para, para_buf, input, trim);
                    out.push(block);
                    return Step::Next(resume);
                }
                // full collapse (resume == i == start): fall through to paragraph.
            }
        }
    }

    // 10. markdown blockquote (mldoc block0.ml `md_blockquote`): a `>` line opens a quote
    // whose body is the de-`>`'d lines PLUS lazy continuation lines (no `>` needed) until a
    // blank line or a line that starts a new block (`- `/`# `/`id:: `/bare `-`/`#`). The
    // de-`>`'d body is re-parsed with the FULL md block grammar MINUS {heading, bullet,
    // property, footnote, drawer} (mldoc `block_content_parsers`), and a `>`-on-continuation
    // nests a child Quote (one `>` stripped per continuation line) — see `build_md_quote`. A
    // quote OPENS only if the de-`>`'d content is non-empty and non-breaker (mldoc: lone
    // `>` / `> ` / `> - x` are paragraphs). The run is bounded by `hi` (the closer line would
    // otherwise be lazily absorbed — verified load-bearing). F1/F5/F6.
    // P3c: the VIEW `t` opening a quote (opener strips up to 2 `>`) hands the driver a
    // `Step::OpenQuote`: it pushes a `>`-Quote container `Frame` (`gt_level+1`) whose opener line is
    // `i`, re-dispatched (`i` unchanged). The run is bounded DYNAMICALLY by `md_quote_cont_slice`
    // (the frame closes on the first `None`), itself bounded by the inherited `hi` (else the closer
    // line would be lazily absorbed). The open paragraph is flushed by the driver (like
    // `Step::Open`). Fires at the root (`gt_level==0`, `t = line_text`) AND for a NESTED opener
    // inside a `>`-frame (`t` = the frame's view) — the single-line `⌈N/2⌉` and the staircase.
    if let Some(inner) = md_quote_first_slice(t) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blank breaks.
        return Step::OpenQuote { opener_content: inner, span_start: line_start };
    }

    // 11. raw HTML (single-line, minimal)
    if is_raw_html(t) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blank breaks.
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::RawHtml {
            text: t.to_string(),
            span: Some(Span(line_start, line_end)),
        });
        return Step::Next(i + 1);
    }

    // 11b. block-level displayed math: a line that is just `$$ … $$`.
    if let Some(math) = displayed_math(t) {
        drop_marker_ws(para, was_ws_drop, input); // F4/M3: drop marker `" \n"`, keep blank breaks.
        flush_para(out, para, para_buf, input, trim);
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
    // Suppressed inside a `>`-blockquote body (mldoc omits Drawer from `block_content_parsers`,
    // so `:NAME: … :END:` there is paragraph text). F1.
    if let Some(name) = drawer_begin(t).filter(|_| !in_block_content) {
        if let Some(close) = find_drawer_end(drawer_end_idxs, drawer_cursor, i) {
            if close < hi {
                flush_para(out, para, para_buf, input, trim);
                let span = Some(Span(line_start, lines[close].end));
                // mldoc (`drawer.ml`) emits a `Property_Drawer` ONLY when the WHOLE body
                // parses as `many1 property` — every body line a valid `:key: value` (an
                // empty body is allowed). If ANY body line fails (plain text, a blank line,
                // a markdown `key:: v`), `parse1` can't reach `:END:` → it falls back to
                // `drawer_parse` → a generic `Drawer{name:"properties"}` (NO props, NO
                // value ref-walking). C3.
                if name == "properties"
                    && lines[i + 1..close]
                        .iter()
                        .all(|l| drawer_property(l.text).is_some())
                {
                    let mut props: Vec<(String, String)> = lines[i + 1..close]
                        .iter()
                        .filter_map(|l| drawer_property(l.text))
                        .collect();
                    // M2: mldoc (`drawer.ml`) continues a `Property_Drawer` with a
                    // `many (parse1 <|> parse2)` AFTER the `:END:`, folding following lines into
                    // the SAME props: `parse1` = a markdown `key:: value` property (consumes one
                    // eol, no blank absorption); `parse2` = a `#+name: value` directive, which
                    // ALSO swallows surrounding blank lines (`optional eols`, so blank-then-
                    // directive and directive-then-blank both fold). Bounded by `hi` (never cross
                    // the frame closer). A `:PROPERTIES:` with a stray body line is a generic
                    // `Drawer` (above) and does NOT fold (F3 unchanged).
                    let mut j = close + 1;
                    loop {
                        if j < hi {
                            if let Some(kv) = property(lines[j].text) {
                                props.push(kv);
                                j += 1;
                                continue;
                            }
                            // directive with leading + trailing truly-empty-line absorption.
                            let mut k = j;
                            while k < hi && lines[k].text.is_empty() {
                                k += 1;
                            }
                            if k < hi {
                                if let Some(kv) = directive_property(lines[k].text) {
                                    props.push(kv);
                                    j = k + 1;
                                    while j < hi && lines[j].text.is_empty() {
                                        j += 1;
                                    }
                                    continue;
                                }
                            }
                        }
                        break;
                    }
                    let end = lines[j - 1].end;
                    out.push(Block::Properties { props, span: Some(Span(line_start, end)) });
                    return Step::Next(j);
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
    // A run of consecutive block hiccups (`[:a][:b]…`, or one that spills onto later lines) is
    // consumed in ONE LOCAL LOOP here, NOT by re-dispatching the whole shrinking remainder line
    // through the full ladder once per vector. The old per-vector `Step::Next(remainder)` re-ran
    // every earlier ladder predicate on the tail each time — and `property` (step 8) does an
    // O(line) `find("::")` — so N vectors cost Σ O(remaining) = O(n²). Capturing them locally
    // makes each ladder predicate run O(1) times per source line ⇒ O(n). We hand control back to
    // the main loop exactly ONCE: at the frame boundary (so it pops + absorbs trailing eols) or
    // for the first NON-hiccup remainder (so it goes through def-list / paragraph normally).
    {
        let mut cur = i; // line index whose leading `[:…]` we are trying to consume
        let mut captured = false;
        loop {
            // At/after the frame's closer line: defer to the main loop (frame pop + eol absorb).
            // `i < hi` always holds on entry, so the first iteration never trips this.
            if cur >= hi {
                return Step::Next(cur);
            }
            let cur_start = lines[cur].start;
            let cur_text = lines[cur].text;
            let lw = leading_ws(cur_text);
            let rec = cur_start + lw;
            if !(last_rbracket.is_some_and(|last| rec <= last) && input[rec..].starts_with("[:")) {
                break;
            }
            // CLAMP the (to-end-of-input) balanced capture to `&input[..body_end]` so a `]`
            // outside this body is not captured (verified load-bearing). `rec < body_end`.
            let Some(cap_end) = crate::inline::parse_hiccup(&input[..body_end], rec) else {
                break;
            };
            flush_para(out, para, para_buf, input, trim); // no-op after the first (para already flushed)
            out.push(Block::Hiccup {
                v: input[rec..cap_end].to_string(),
                span: Some(Span(cur_start, cap_end)),
            });
            captured = true;
            // Resume after the `]`, first absorbing consecutive eols (mldoc's `<* optional eols`:
            // `[:div]\n\nx` → [Hiccup, Para "x"] — blank lines after a whole-line hiccup are
            // swallowed — but a same-line remainder `[:div]x\n\ny` is NOT, so skip only `\n`/`\r`).
            // The eol run stops at `body_end` (the closer line starts with `#`/`:`), never crossing
            // into the enclosing body.
            let bytes = input.as_bytes();
            let mut resume = cap_end;
            while resume < bytes.len() && matches!(bytes[resume], b'\n' | b'\r') {
                resume += 1;
            }
            if resume >= bytes.len() {
                return Step::Next(lines.len()); // captured to EOF (+ trailing eols)
            }
            // Find the line containing `resume`; leave it as-is when `resume` is at its start,
            // else rewrite it to the remainder slice — then loop to try the next vector at it.
            let mut ri = cur;
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
            cur = ri;
        }
        if captured {
            // First non-hiccup remainder (or frame boundary handled above): re-dispatch it ONCE
            // through the full ladder — identical to the old per-vector hand-off, minus the
            // O(n²) repetition.
            return Step::Next(cur);
        }
        // Not a block hiccup at all → fall through to def-list / paragraph.
    }

    // 11d. markdown definition list (mldoc `lists0.ml` `md_definition`, the Lists
    // fallback, tried just above paragraph): a (would-be paragraph) term line
    // immediately followed by a `: <def>` line. mldoc pulls the term out of a
    // running paragraph (`intro\nterm\n: def` → Paragraph[intro] + def-list), so
    // we check it here at the paragraph point, after every other block construct.
    // The term peek + `build_def_list`'s item/continuation/blank scans are bounded by `hi`.
    // Suppressed in list-item content (`in_item`): mldoc's `list_content_parsers` omits the WHOLE
    // `Lists.parse` (= regular list <|> `md_definition`), so a def-list never nests inside an item.
    if !in_item
        && !t.trim_start().is_empty()
        && i + 1 < hi
        && is_def_opener(lines[i + 1].text)
    {
        flush_para(out, para, para_buf, input, trim);
        let (item, ni) = build_def_list(lines, i, hi, input);
        out.push(Block::List {
            items: vec![item],
            span: Some(Span(line_start, lines[ni - 1].end)),
        });
        return Step::Next(ni);
    }

    // 12. plain line — accumulate into the current paragraph.
    // In null_spans (strip>0) frames, the content lives in para_buf (viewed, de-indented).
    // Each viewed line is appended with its trailing `\n` so that flush_para's trim_eol=false
    // (body-final flush at frame close) preserves the trailing Break, while trim_eol=true
    // (mid-body flush before a following block, in_quote=true) trims it — exactly matching
    // what the old reparse_block_content path produced via block_code_texts("x\n").
    if null_spans {
        let viewed = t; // already line_text(lines, i, strip)
        let buf = para_buf.get_or_insert_with(String::new);
        buf.push_str(viewed);
        buf.push('\n'); // trailing \n preserved until flush_para trims or keeps based on trim_eol
        // Keep para in lockstep so flush_para and ws_drop still work.
        *para = Some(match *para {
            Some((s, _)) => (s, line_end),
            None => (line_start, line_end),
        });
    } else {
        *para = Some(match *para {
            Some((s, _)) => (s, line_end),
            None => (line_start, line_end),
        });
    }
    // M3: an empty-marker ws-drop survives across a TRULY-EMPTY line (mldoc `optional eols`), so a
    // block opener after intervening blank line(s) still drops the marker's `" \n"` (keeping the
    // blanks as a break-paragraph). A non-empty line — even whitespace-only `"  "` — clears it, so
    // the marker ws is kept (`## \n  \n```` → marker ws kept; `## \n\n```` → marker ws dropped).
    if was_ws_drop.is_some() && t.is_empty() {
        *ws_drop = was_ws_drop;
    }
    Step::Next(i + 1)
}

// ---- helpers --------------------------------------------------------------

/// Flush the open paragraph. `trim_eol` drops trailing newline(s) from the slice (so no
/// trailing `Break_Line`): inside a `>`-blockquote body (`in_quote`) a *following block*
/// absorbs the paragraph's trailing eol via mldoc's `between_eols`/`concat_paragraph_lines`,
/// whereas at the document level the eol stays a Break. Body-final / EOF flushes pass `false`.
///
/// `para_buf`: in a `null_spans` (strip>0 / re-bulleted) frame, the paragraph content lives
/// in `para_buf` (viewed line texts joined by `\n`), already de-indented. Parse THAT and
/// null the span (the byte-window doesn't map to de-indented content; `none_out_blocks` on
/// the frame's pop will null the inline spans). Keep `para` in lockstep by clearing it too.
fn flush_para(
    out: &mut Vec<Block>,
    para: &mut Option<(usize, usize)>,
    para_buf: &mut Option<String>,
    input: &str,
    trim_eol: bool,
) {
    if let Some(mut buf) = para_buf.take() {
        *para = None;
        if trim_eol {
            while buf.ends_with('\n') || buf.ends_with('\r') {
                buf.pop();
            }
        }
        // Base offset 0: inline spans are relative to `buf` and get nulled by none_out_blocks
        // on the frame's pop (every null_spans frame runs it), so they never reach the output.
        out.push(Block::Paragraph { inline: stub_inline(&buf, 0), span: None });
        return;
    }
    if let Some((s, mut e)) = para.take() {
        if trim_eol {
            while e > s && matches!(input.as_bytes()[e - 1], b'\n' | b'\r') {
                e -= 1;
            }
        }
        out.push(Block::Paragraph {
            inline: stub_inline(&input[s..e], s),
            span: Some(Span(s, e)),
        });
    }
}

/// F4/M3: a drop-trigger block follows an empty heading/bullet marker whose trailing-ws
/// paragraph is open (`was_ws_drop == Some(boundary)`, where `boundary` is the marker line's
/// end offset). Drop the marker's `" \n"` portion `[para.start, boundary)`; KEEP any intervening
/// blank lines `[boundary, para.end)` as their own break-paragraph (M3 — the no-blank F4 case is
/// `boundary == para.end`, dropping the whole para). A no-op unless the para is whitespace-only.
fn drop_marker_ws(para: &mut Option<(usize, usize)>, was_ws_drop: Option<usize>, input: &str) {
    if let Some(boundary) = was_ws_drop {
        if let Some((_, e)) = *para {
            if para_ws_only(para, input) {
                *para = if boundary < e { Some((boundary, e)) } else { None };
            }
        }
    }
}

fn stub_inline(s: &str, base: usize) -> Vec<Inline> {
    // The real inline parser. `base` = the absolute byte offset of `s` in the block body.
    crate::resolver::parse_inline(s, base)
}

/// Null out the `span` of `n` and, recursively, of its inline children. Used when the inline
/// text was parsed from a FOLDED (joined) buffer whose positions don't map to the block body,
/// so no meaningful source span exists (spans must be absent, not wrong).
fn none_out_inline(n: &mut Inline) {
    crate::projection::set_inline_span(n, None);
    match n {
        Inline::Emphasis { children, .. }
        | Inline::Subscript { children, .. }
        | Inline::Superscript { children, .. }
        | Inline::Tag { children, .. } => {
            for c in children.iter_mut() {
                none_out_inline(c);
            }
        }
        Inline::Link { label, .. } => {
            for c in label.iter_mut() {
                none_out_inline(c);
            }
        }
        _ => {}
    }
}

/// `pub(crate)` so the org driver can null the inline spans of its FOLDED reparse buffers.
pub(crate) fn none_out_inlines(inlines: &mut [Inline]) {
    for n in inlines.iter_mut() {
        none_out_inline(n);
    }
}

fn none_out_list_items(items: &mut [ListItem]) {
    for item in items.iter_mut() {
        none_out_inlines(&mut item.name);
        none_out_blocks(&mut item.content);
        none_out_list_items(&mut item.items);
    }
}

/// Recursively null out every INLINE span in `blocks` (block-level spans are untouched —
/// they are excluded from the gate and not part of this task's inline-span contract).
/// `pub(crate)` so the org driver reuses it for its FOLDED `streaming_reparse` bodies.
pub(crate) fn none_out_blocks(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        match block {
            Block::Paragraph { inline, .. }
            | Block::Heading { inline, .. }
            | Block::Bullet { inline, .. }
            | Block::FootnoteDef { inline, .. } => {
                none_out_inlines(inline);
            }
            Block::Table { header, rows, .. } => {
                if let Some(h) = header {
                    for cell in h.iter_mut() {
                        none_out_inlines(cell);
                    }
                }
                for row in rows.iter_mut() {
                    for cell in row.iter_mut() {
                        none_out_inlines(cell);
                    }
                }
            }
            Block::List { items, .. } => {
                none_out_list_items(items);
            }
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                none_out_blocks(children);
            }
            _ => {}
        }
    }
}

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


/// Language of a fence from its info string (the text after the ``` run): mldoc's
/// `language` is the FIRST whitespace-delimited token (`clj :results` → `clj`, the
/// `:results` is a separate `options` field we don't model).
fn fence_lang(info: &str) -> String {
    info.split_whitespace().next().unwrap_or("").to_string()
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

/// A parsed markdown list marker (mldoc `format_checkbox_parser` + the first content line),
/// PLUS the raw body after `marker + ws + checkbox + spaces` (NOT yet trimmed — `collect_list_md`
/// applies the per-line `String.trim` mldoc does at content join).
struct MdMarker {
    ordered: bool,
    number: Option<u32>,
    checkbox: Option<bool>,
    indent: u32,
    body: String,
}

/// Parse a markdown list marker (`*`/`+` then ws, or `N.` then ws; mldoc `format_parser` for
/// `is_markdown`: `+`/`*` always, plus ordered `digits '.'`). Requires non-empty content after
/// any checkbox (mldoc's `take_till1` needs ≥1 char) — a bare `* `/`+ `/`1. `/`* [ ]` yields None
/// (those fall through to a Paragraph). `-` is a Bullet in markdown, never a list marker; `N)` is
/// not a list. The body is the raw rest after the marker+ws+checkbox+spaces.
fn md_marker(s: &str) -> Option<MdMarker> {
    let ws = leading_ws(s);
    let rest = &s[ws..];
    let mk = |ordered, number, content: &str| {
        let (checkbox, body) = split_checkbox(content);
        if body.trim().is_empty() {
            return None;
        }
        Some(MdMarker { ordered, number, checkbox, indent: ws as u32, body: body.to_string() })
    };
    // unordered * or +
    if let Some(after) = rest.strip_prefix('*').or_else(|| rest.strip_prefix('+')) {
        if after.starts_with(' ') || after.starts_with('\t') {
            return mk(false, None, after.trim_start());
        }
    }
    // ordered N.  (NOT N))
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

/// mldoc `check_listitem` (markdown branch): `(indent, is_item, is_heading)`. `is_item` marks a
/// line as a *list-item shape* for the continuation logic — BROADER than a parseable marker: a
/// leading integer (`Scanf "%d"` on the trimmed line, even `12abc`/`-5`) is `is_item` regardless
/// of a following `.`; `+ ` and `* ` (at ANY indent in markdown) are `is_item`. `is_heading` is a
/// `-`-bullet line (`"- "`, or a lone `-`) — it ENDS an item's content (the `-` becomes a Bullet),
/// it does NOT fold. The mismatch between `is_item` and the parseable `md_marker` (which fails on
/// a leading int that is not `N. `) is exactly what drives the collapse.
fn check_listitem_md(line: &str) -> (u32, bool, bool) {
    let indent = leading_ws(line);
    if scan_leading_int(line.trim()) {
        return (indent as u32, true, false);
    }
    let b = line.as_bytes();
    if b.len() >= indent + 2 {
        let (p0, p1) = (b[indent], b[indent + 1]);
        let is_item = (p0 == b'+' && p1 == b' ') || (p0 == b'*' && p1 == b' ');
        let is_heading = p0 == b'-' && p1 == b' ';
        (indent as u32, is_item, is_heading)
    } else if b.len() >= indent + 1 {
        (indent as u32, false, b[indent] == b'-')
    } else {
        (indent as u32, false, false)
    }
}

/// mldoc `Scanf.sscanf (String.trim line) "%d"`: does the (already-trimmed) string begin with an
/// integer (optional `+`/`-` then ≥1 digit)? (Mirrors org's identical helper.)
fn scan_leading_int(t: &str) -> bool {
    let b = t.as_bytes();
    let i = if matches!(b.first(), Some(b'+' | b'-')) { 1 } else { 0 };
    b.get(i).is_some_and(u8::is_ascii_digit)
}

/// mldoc `lists0.ml` `definition` (markdown, UNORDERED items only): a `name :: ` item splits its
/// trimmed-joined content into (`name` inline, description). `end_string " ::"` matched with
/// `consume:All` only succeeds when the FIRST " ::" is at the very END of the content — so it
/// fires iff content == `<name> ::` with `<name>` non-empty and containing no earlier " ::"; the
/// description is then always "" (the `>= l+1` branch is dead under `consume:All`). `* term :: x`
/// / `* term ::\n…` do NOT fire (something follows the first " ::"); `* term ::` → name "term",
/// content "". Returns `(name_inline, stripped_content)`; `None` = no split (content unchanged).
fn md_definition_split(content: &str) -> Option<(Vec<Inline>, String)> {
    let pos = content.find(" ::")?;
    if pos + 3 != content.len() {
        return None; // the first " ::" is not at the end ⇒ consume:All fails ⇒ no definition
    }
    let name = &content[..pos];
    if name.is_empty() {
        return None; // `take_while1` needs ≥1 char before " ::"
    }
    // `content` is a FOLDED join → its positions don't map to the block body; drop the spans.
    let mut inl = stub_inline(name, 0);
    none_out_inlines(&mut inl);
    Some((inl, String::new()))
}

/// A (possibly partial) list collapse — mldoc's recursive list parser failed on a deeper
/// continuation that is a list-item shape but NOT a parseable marker. `kept` is the `List` of
/// items parsed before the failing item (None if none survive — a full collapse → the start
/// line re-parses as a Paragraph); `resume` is the line the document parser resumes at (the
/// failing/first-unkept item's marker); `trigger` memoises the collapse region (`collapse_floor`,
/// for linearity). Mirrors org's identical struct.
struct Collapse {
    kept: Option<Block>,
    resume: usize,
    trigger: usize,
}

/// Collect a markdown list starting at line `start` — a faithful port of mldoc's shared
/// `lists0.ml` list parser (markdown branch). Each item folds its indented multi-line
/// continuation (de-indented via per-line `String.trim`) into its content, re-parsed with the
/// list-item content grammar (`reparse_item_content`, inheriting `in_quote`); deeper is-item
/// lines become children via the flat sequence + `nest_items`. UNORDERED items run `md_definition_split`.
///
/// COLLAPSE: a continuation that is a list-item shape (`check_listitem_md`) DEEPER than the
/// current item but NOT a parseable marker there (`md_marker` None — a leading int that is not
/// `N. `) makes the item's child `list_parser` fail. In mldoc that failure bubbles up through
/// every item that is *first at its level*, terminating at (and keeping) the first ancestor level
/// with a prior sibling; the failing item onward re-parses as a Paragraph. `collapse_resume`
/// reproduces that bubble from the flat indent sequence.
///
/// `hi` bounds every scan to THIS body (a list inside a callout window must not absorb the
/// `#+END_…` closer); at the top level `hi == lines.len()` (no-op).
fn collect_list_md(
    lines: &[Line],
    start: usize,
    hi: usize,
    in_quote: bool,
) -> Result<(Block, usize), Collapse> {
    let mut flat: Vec<ListItem> = Vec::new();
    let mut flat_lines: Vec<usize> = Vec::new();
    let mut flat_indents: Vec<u32> = Vec::new();
    let mut i = start;
    while i < hi {
        let t = lines[i].text;
        // terminator at a would-be marker position: a blank line, or any non-marker line (a
        // `#` heading / `-` bullet is the mldoc `Heading.parse` breakout — never an `md_marker`).
        if t.is_empty() {
            break;
        }
        let marker = match md_marker(t) {
            Some(m) => m,
            None => break,
        };
        let cur_indent = marker.indent;
        // content = first line (marker body) + folded indented continuation lines, each trimmed.
        let mut content_lines: Vec<String> = vec![marker.body.trim().to_string()];
        let mut j = i + 1;
        let mut trigger: Option<usize> = None;
        loop {
            if j >= hi {
                break; // EOF / body boundary ends this item's content
            }
            let cl = lines[j].text;
            if cl.is_empty() {
                j += 1; // mldoc `two_eols`: a (truly) blank line ends the content AND is consumed
                break;
            }
            let (ci, is_item, is_heading) = check_listitem_md(cl);
            if ci == 0 {
                break; // a col-0 line ends the content (left for the outer loop)
            }
            if is_heading {
                break; // a `-` bullet line ends the content (left to become a Bullet)
            }
            if is_item {
                if ci > cur_indent && md_marker(cl).is_none() {
                    trigger = Some(j); // COLLAPSE trigger (deeper unparseable list-item shape)
                }
                break; // child / breakout / collapse — handled below
            }
            content_lines.push(cl.trim().to_string()); // fold (de-indented)
            j += 1;
        }
        if let Some(trigger) = trigger {
            // The failing item P is the one at line `i` (indent `cur_indent`), NOT pushed.
            let r = collapse_resume(&flat_indents, cur_indent);
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
        // mldoc: `content = List.map String.trim content |> concat "\n"`, then UNORDERED items
        // run `definition` (which may strip a trailing `name ::` and empty the content).
        let joined = content_lines.join("\n");
        let (name, content_str) = if marker.ordered {
            (Vec::new(), joined)
        } else {
            match md_definition_split(&joined) {
                Some((name, stripped)) => (name, stripped),
                None => (Vec::new(), joined),
            }
        };
        flat.push(ListItem {
            ordered: marker.ordered,
            number: marker.number,
            indent: cur_indent,
            content: reparse_item_content(&content_str, in_quote),
            items: vec![],
            name,
            checkbox: marker.checkbox,
        });
        flat_lines.push(i);
        flat_indents.push(cur_indent);
        i = j;
    }
    if flat.is_empty() {
        // defensive: the caller gates on `md_marker`, so unreachable.
        return Err(Collapse { kept: None, resume: start, trigger: start });
    }
    let span = Some(Span(lines[start].start, lines[i - 1].end));
    Ok((Block::List { items: crate::projection::nest_items(flat), span }, i))
}

/// Given the indents of the successfully-collected list items and the indent of the failing item
/// P (conceptually at index `flat_indents.len()`), return the flat index `r` such that items
/// `[0, r)` are kept and the resume point is item `r`'s marker (or P's marker if `r ==
/// flat_indents.len()`). Walks up while the current item is the *first at its level* (its nearest
/// shallower-or-equal predecessor is strictly shallower — a parent, not a prior sibling), matching
/// mldoc's failure bubble-up. Mirrors org's identical helper.
fn collapse_resume(flat_indents: &[u32], p_indent: u32) -> usize {
    let mut cur_indent = p_indent;
    let mut cur_index = flat_indents.len();
    loop {
        let q = (0..cur_index).rev().find(|&j| flat_indents[j] <= cur_indent);
        match q {
            None => return cur_index,                                       // first item overall
            Some(j) if flat_indents[j] == cur_indent => return cur_index,    // prior sibling
            Some(j) => {
                cur_index = j; // a parent → bubble up
                cur_indent = flat_indents[j];
            }
        }
    }
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

/// First line of a markdown blockquote (mldoc `md_blockquote = char '>' *> lines_while …`):
/// the opener consumes one `>` (`char '>'`) and `lines_while`'s `optional (char '>')` strips a
/// SECOND — so up to TWO `>` on the opener (N leading `>` nest ⌈N/2⌉ Quotes). Opens only if the
/// de-`>`'d content is non-empty and does NOT start a new block (`- `/`# `/`id:: `/bare `-`/`#`).
/// Returns the content as a SUFFIX slice (no alloc) so `build_md_quote` peels in place. F1/F5.
fn md_quote_first_slice(s: &str) -> Option<&str> {
    let r1 = s.trim_start().strip_prefix('>')?.trim_start();
    let content = match r1.strip_prefix('>') {
        Some(r2) => r2.trim_start(),
        None => r1,
    };
    if content.is_empty() || quote_para_trigger(content) {
        return None;
    }
    Some(content)
}

/// One CONTINUATION line of a markdown blockquote body (mldoc `lines_while`): a `>`(+ws)-only
/// line → `Some("")` (an empty body line); else strip ONE optional `>` (+ws) and keep the rest
/// (lazy — a non-`>` line still continues). `None` STOPS the run: a blank line (no `>`), or a
/// de-`>`'d line that starts a new block. Strips exactly ONE `>` (the F5 fix vs the old
/// recursive flatten). Borrowing suffix slice. F1/F5.
fn md_quote_cont_slice(s: &str) -> Option<&str> {
    let t = s.trim_start();
    let had_gt = t.starts_with('>');
    let rest = if had_gt { t[1..].trim_start() } else { t };
    if rest.is_empty() {
        return if had_gt { Some("") } else { None };
    }
    if quote_para_trigger(rest) {
        return None;
    }
    Some(rest)
}

/// The §3 `>`-quote fallback (org `streaming_reparse` analog): a `>`-quote body containing a
/// fence / `#+BEGIN_X` callout / LaTeX env / block hiccup — constructs whose recognizers don't
/// tolerate literal `>`s — is de-`>`'d and reparsed once through the md driver with
/// `in_quote = true`. Re-bulleted `#+BEGIN_X` bodies (P2) and the `>`-quote staircase (P3c) are
/// now frames and no longer reach here. Guarded by `GT_FALLBACK_NEST_CAP` so construct-in-`>`-quote
/// nesting degrades gracefully (flat Paragraph past 64) instead of a parse-time SIGABRT — mldoc
/// overflows on the same shape. See the const's doc.
fn reparse_block_content(residual: &str) -> Vec<Block> {
    let depth = MD_BLOCK_DEPTH.with(|c| c.get());
    let mut out = if depth >= GT_FALLBACK_NEST_CAP {
        if residual.is_empty() {
            Vec::new()
        } else {
            vec![Block::Paragraph { inline: stub_inline(residual, 0), span: Some(Span(0, residual.len())) }]
        }
    } else {
        MD_BLOCK_DEPTH.with(|c| c.set(depth + 1));
        let o = parse_md_streaming(residual, true, false);
        MD_BLOCK_DEPTH.with(|c| c.set(depth));
        o
    };
    // `residual` is a FOLDED (de-`>`'d / dedented) buffer → inline spans don't map to the
    // block body; drop them (block-level spans are gate-excluded and left as-is).
    none_out_blocks(&mut out);
    out
}

/// Re-parse a markdown list item's folded content (de-indented continuation lines, joined
/// with `\n`) through the md driver with the list-item content grammar (mldoc's
/// `list_content_parsers`: in-block-content MINUS Lists, MINUS Directive at the document
/// level). `in_quote` is inherited from the list's enclosing frame (a list inside a `>`-quote
/// keeps Directive in its item content — mldoc instantiates `Lists.parse` with the WITH-Directive
/// `list_content_parsers` inside `block_content_parsers`). List re-entry is disabled by `in_item`,
/// so this is depth-1 (any nested callout/quote inside the content takes the guarded
/// `reparse_block_content` path) — no extra depth guard needed, mirroring org's `streaming_reparse`.
/// mldoc's content reparse falls back to a single empty Paragraph on an empty content string
/// (`definition` may strip a `name ::` item to ""), so an empty reparse yields `[Paragraph []]`.
fn reparse_item_content(content: &str, in_quote: bool) -> Vec<Block> {
    if content.is_empty() {
        return vec![Block::Paragraph { inline: Vec::new(), span: None }];
    }
    // `content` is a FOLDED (de-indented, `\n`-joined) buffer → inline spans don't map to the
    // block body; drop them.
    let mut out = parse_md_streaming(content, in_quote, true);
    none_out_blocks(&mut out);
    out
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
fn build_def_list(lines: &[Line], i: usize, hi: usize, input: &str) -> (ListItem, usize) {
    // All scans are bounded by `hi`: at the top level `hi == lines.len()` (identical to
    // before); inside a callout body the closer line (`#+END_X`) is never a def
    // opener/continuation/blank, so the bound matches the legacy body-local scan exactly.
    // The term is a raw sub-slice of `lines[i].text` → its inline spans are absolute (S2).
    let term = lines[i].text.trim_start(); // mldoc name = `spaces *> line`
    let name = stub_inline(term, crate::inline::ptr_base(term, input));
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
        // mldoc inline-parses `String.trim`-ed of the joined item — a FOLDED buffer, so its
        // inline spans don't map to the block body; drop them.
        let item_text = item_lines.join("\n");
        let mut inl = stub_inline(item_text.trim(), 0);
        none_out_inlines(&mut inl);
        content.push(Block::Paragraph {
            inline: inl,
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

/// The shared `#+BEGIN_<TYPE>` container dispatch (the md mirror of org): `QUOTE`→`Quote`,
/// anything-else→`Custom{name lowercased}`. SRC/EXAMPLE never reach here — they are raw-body
/// blocks consumed in place (`raw_callout_block`). Reused by BOTH the bare `#+BEGIN_X` opener
/// (step 2) and the re-bulleted `- #+BEGIN_X` opener (step 5 a2) so the two can't drift.
fn callout_builder(name: &str) -> Builder {
    if name.eq_ignore_ascii_case("QUOTE") {
        Builder::Quote
    } else {
        Builder::Custom(name.to_ascii_lowercase())
    }
}

/// Build a raw-body `#+BEGIN_SRC`/`#+BEGIN_EXAMPLE` block: the body indent is cleared by mldoc's
/// `block_code_texts`, and `Src`'s lang is the first token after the name (`begin_lang`, read from
/// `name_src` — the opener line / bullet content). Trailing truly-empty lines are swallowed (mldoc
/// `<* optional eols`), so the returned resume index `ni` skips them. `span_start` is the block's
/// span start (the line start for the bare form, the bullet CONTENT for the re-bulleted form).
/// Shared by step 2 (bare) and step 5 a2 (re-bulleted) so the SRC/EXAMPLE handling can't drift.
fn raw_callout_block(
    name: &str,
    name_src: &str,
    lines: &[Line<'_>],
    i: usize,
    close: usize,
    hi: usize,
    span_start: usize,
) -> (Block, usize) {
    let texts: Vec<&str> = lines[i + 1..close].iter().map(|l| l.text).collect();
    let code = crate::org::block_code_texts(&texts);
    let mut ni = close + 1;
    let mut end = lines[close].end;
    while ni < hi && lines[ni].text.is_empty() {
        end = lines[ni].end;
        ni += 1;
    }
    let block = if name.eq_ignore_ascii_case("SRC") {
        Block::Src { lang: crate::org::begin_lang(name_src), code, span: Some(Span(span_start, end)) }
    } else {
        Block::Example { code, span: Some(Span(span_start, end)) }
    };
    (block, ni)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Block;

    /// Strip inline spans (added by the source-span feature) so structural `assert_eq!`s over
    /// inline vecs stay span-agnostic — the span invariants are checked separately (lib.rs).
    fn ns(v: &[Inline]) -> Vec<Inline> {
        let mut v = v.to_vec();
        none_out_inlines(&mut v);
        v
    }

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
                    Inline::Plain { text, .. } => text.clone(),
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
                Some(Inline::Plain { text, .. }) => text.clone(),
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
                assert_eq!(ns(&items[0].name), vec![Inline::Plain { text: "term".into(), span: None }]);
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
                assert_eq!(ns(&items[0].name), vec![Inline::Plain { text: "t1".into(), span: None }]);
                match &items[0].content[0] {
                    Block::Paragraph { inline, .. } => assert_eq!(ns(inline), vec![
                        Inline::Plain { text: "d1".into(), span: None }, Inline::Break { span: None },
                        Inline::Plain { text: "t2".into(), span: None },
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
            Block::Bullet { inline, .. } => assert_eq!(ns(inline), vec![Inline::Plain { text: ">".into(), span: None }]),
            _ => panic!(),
        }
    }

    #[test]
    fn begin_callout_on_bullet_line() {
        // A `-` bullet whose content is `#+BEGIN_<TYPE> … #+END_<TYPE>` splits into
        // [empty bullet, <block>], dispatched IDENTICALLY to the bare form: QUOTE→Quote,
        // SRC→Src, EXAMPLE→Example, anything-else→Custom{name lowercased}.
        let name2 = |s: &str| match &parse(s)[1] {
            Block::Custom { name, .. } => name.clone(),
            b => panic!("expected Custom, got {b:?}"),
        };
        // first plain text of the 2nd block's first child paragraph (verifies body dedent).
        let body2 = |s: &str| {
            let kids = match &parse(s)[1] {
                Block::Custom { children, .. } | Block::Quote { children, .. } => children.clone(),
                b => panic!("expected Custom/Quote, got {b:?}"),
            };
            match &kids[0] {
                Block::Paragraph { inline, .. } => match &inline[0] {
                    Inline::Plain { text, .. } => text.clone(),
                    i => panic!("expected Plain, got {i:?}"),
                },
                b => panic!("expected Paragraph, got {b:?}"),
            }
        };
        // NOTE / TIP / WARNING → Custom{name}; the empty bullet precedes the block.
        assert_eq!(kinds("- #+BEGIN_NOTE\n  x\n  #+END_NOTE"), ["bullet", "custom"]);
        assert_eq!(name2("- #+BEGIN_NOTE\n  x\n  #+END_NOTE"), "note");
        assert_eq!(name2("- #+BEGIN_TIP\n  t\n  #+END_TIP"), "tip");
        assert_eq!(name2("- #+BEGIN_WARNING\n  w\n  #+END_WARNING"), "warning");
        match &parse("- #+BEGIN_NOTE\n  x\n  #+END_NOTE")[0] {
            Block::Bullet { inline, .. } => assert!(inline.is_empty()),
            _ => panic!("expected empty bullet"),
        }
        // QUOTE → Quote; unknown TYPE → Custom{type lowercased}.
        assert_eq!(kinds("- #+BEGIN_QUOTE\n  q\n  #+END_QUOTE"), ["bullet", "quote"]);
        assert_eq!(name2("- #+BEGIN_FOO\n  f\n  #+END_FOO"), "foo");
        // case-insensitive BEGIN/END/name.
        assert_eq!(name2("- #+begin_note\n  x\n  #+END_NOTE"), "note");
        // body INDENT-CLEARED (block0.ml): the 2-space continuation indent is stripped.
        assert_eq!(body2("- #+BEGIN_NOTE\n  x\n  #+END_NOTE"), "x");
        assert_eq!(body2("- #+BEGIN_QUOTE\n  q\n  #+END_QUOTE"), "q");
        // mismatched / unterminated END → NOT split: a normal bullet + following paragraph.
        assert_eq!(kinds("- #+BEGIN_TIP\n  x"), ["bullet", "paragraph"]);
        assert_eq!(kinds("- #+BEGIN_TIP\n  x\n  #+END_OTHER"), ["bullet", "paragraph"]);
        // SRC/EXAMPLE stay raw-body blocks (non-regression — the v0.2.3 B fix).
        assert_eq!(kinds("- #+BEGIN_SRC\n  x\n  #+END_SRC"), ["bullet", "src"]);
        assert_eq!(kinds("- #+BEGIN_EXAMPLE\n  x\n  #+END_EXAMPLE"), ["bullet", "example"]);
        // bare forms unchanged (no bullet split).
        assert_eq!(kinds("#+BEGIN_NOTE\nx\n#+END_NOTE"), ["custom"]);
        assert_eq!(kinds("#+BEGIN_QUOTE\nq\n#+END_QUOTE"), ["quote"]);
    }

    #[test]
    fn nested_md_lists() {
        // Compact tree shape: "a[b,c]" = a with children b,c. Label = the item's
        // first plain inline. Verifies mldoc's indent-folding (see `nest_items`).
        fn label(it: &ListItem) -> String {
            match &it.content[0] {
                Block::Paragraph { inline, .. } => match inline.first() {
                    Some(Inline::Plain { text, .. }) => text.clone(),
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

fn build_table(rows: &[Line], start: usize, end: usize, input: &str) -> Block {
    let texts: Vec<&str> = rows.iter().map(|l| l.text).collect();
    build_table_from_texts(&texts, start, end, input)
}

/// Build a `Table` from raw row strings (used by both the top-level table block and the
/// `- | … |` bullet-opener split, whose first row is a mid-line bullet body, not a `Line`).
/// Each `rows[k]` is a sub-slice of `input` (a real row line, or a bullet content line), so
/// each cell's byte offset into the block body is recovered by pointer arithmetic (S2).
fn build_table_from_texts(rows: &[&str], start: usize, end: usize, input: &str) -> Block {
    let split_cells = |s: &str| -> Vec<Vec<Inline>> {
        let t = s.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        t.split('|')
            .map(|c| {
                let c = c.trim();
                stub_inline(c, crate::inline::ptr_base(c, input))
            })
            .collect()
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

    // lsdoc-only render enrichment (gate-dropped): when the separator row is dropped,
    // retain its `:--`/`--:`/`:-:` per-column alignment for `data-align`. Keep only if
    // at least one column is actually aligned, so plain `|---|` tables emit nothing.
    let aligns = (data_start == 2)
        .then(|| crate::projection::parse_separator_aligns(rows[1]))
        .filter(|a| a.iter().any(Option::is_some));

    Block::Table {
        header,
        rows: body,
        aligns,
        span: Some(Span(start, end)),
    }
}


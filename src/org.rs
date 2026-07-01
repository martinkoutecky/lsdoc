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

// The org (`org.rs`) and md (`parse.rs`) block loops are intentionally PARALLEL implementations
// (different grammars); the leaf predicates + infrastructure they both use — `split_lines`,
// `EndTrie`, fence/drawer lookups, the task-marker table, `Builder`, `GT_FALLBACK_NEST_CAP` — live once
// in `crate::block_common`. The dispatch ladders and driver loops below stay per-format.
use crate::block_common::{
    displayed_math, drawer_property, find_drawer_end, find_matching_fence, is_raw_html, leading_ws,
    para_ws_only, split_checkbox, split_lines, Builder, EndTrie, Line, GT_FALLBACK_NEST_CAP, MARKERS,
};
use crate::inline::{char_len, is_ws_or_nl};
use crate::projection::{Block, Inline, ListItem, Span, Url};

// ===========================================================================
// Block segmentation
// ===========================================================================

// Graceful anti-SIGABRT guard on `streaming_reparse`'s ONE remaining native re-dispatch — the
// §3 `>`-quote fallback. This is **NOT a parity cap**: every gated / fuzz-reachable / realistic
// Org shape parses UNCAPPED in O(n). Indented / `\r\n` `#+BEGIN_X`/quote bodies are zero-copy
// strip-view `Frame`s (P1) and the `>`-quote staircase is iterative `>`-container frames (P3) —
// none touch this cap. It bounds ONLY the fuzz-unreachable residual re-dispatch that remains: a
// `>`-quote body containing a fence / `#+BEGIN` / LaTeX env / hiccup (constructs whose recognizers
// can't see through literal `>`s) is de-`>`'d and reparsed once, and construct-in-`>`-quote nesting
// recurses one level per such body — reaching depth d needs O(d²) input bytes (each level costs a
// `>` AND a construct), so the fuzz/corpus never gets there. That shape is an mldoc-stack-overflow
// shape with no defined byte-target past a modest depth; lsdoc degrades it to a flat Paragraph at
// `GT_FALLBACK_NEST_CAP` (= 64) rather than SIGABRT-ing at parse time. This thread-local counts
// that residual Org re-dispatch depth against the cap.
std::thread_local! {
    static BLOCK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn parse(input: &str) -> Vec<Block> {
    // Single-pass streaming Org block driver: O(n) time, O(depth) HEAP (the explicit container
    // `Frame` stack — `#+BEGIN_X` strip-view frames + `>`-container quote frames), NO native
    // top-level recursion and NO parity cap. Byte-exact to mldoc (gated by `harness/`). The legacy
    // recurse-on-body driver — mldoc's O(n²) + stack-overflow on deep callouts/`>` — is gone; the
    // depth guard now lives ONLY inside `streaming_reparse`'s §3 fallback (`GT_FALLBACK_NEST_CAP`).
    parse_org_streaming(input, Ctx { in_item: false, in_quote: false })
}

/// The streaming Org block driver at the document root — identical to `parse`, but re-exported
/// `#[doc(hidden)]` as `lsdoc::__parse_org_streaming` so the perf / overflow gates can name the
/// streaming entry point directly (and `forget` its result on a small stack). Not stable API.
pub(crate) fn parse_streaming_root(input: &str) -> Vec<Block> {
    parse_org_streaming(input, Ctx { in_item: false, in_quote: false })
}

/// The bounded, L-attributed Org block context — exactly two booleans (the linearity
/// premise: no input-derived state). Set by the ENCLOSING container on push (root =
/// `{false,false}`), and carried through each `streaming_reparse` of a transformed body.
/// `in_item` suppresses Directive/Drawer/Headline/Footnote/List; `in_quote` suppresses
/// Headline (and trims a paragraph's trailing break before a hiccup).
#[derive(Clone, Copy)]
struct Ctx {
    in_item: bool,
    in_quote: bool,
}

/// The outcome of classifying ONE Org line (`dispatch_org_line`): advance to `Next(ni)`,
/// or recognize a re-dispatched callout opener (`Open`) whose body is `[i+1, close)` and
/// whose closer is line `close`. Only `#+BEGIN_QUOTE` / `#+BEGIN_<custom>` are `Open`
/// (SRC/EXAMPLE/fence/latex/drawer/`>`-quote are consumed/recursed in place). The driver
/// handles the body: it pushes a WINDOW frame when the body is a clean `\n`/indent-0 window,
/// else falls back to a transformed sub-recursion (`block_code` + `streaming_reparse`) so a
/// de-indented body stays local.
enum Step {
    Next(usize),
    Open { close: usize, builder: Builder, child_ctx: Ctx, indent_strip: usize },
    /// A `>`-frame body line whose de-`>`'d view opens a construct that CANNOT be classified
    /// copy-free against the global raw-input indexes/scanners (fenced code / `#+BEGIN_X` callout /
    /// LaTeX env / block hiccup) or needs a raw-input multi-line builder (table / verbatim / list).
    /// The driver reparses the frame's REMAINING body `[i, end)` — plus any pending copy-free
    /// paragraph as the reparse PREFIX (so a degraded construct coalesces / a real block trims the
    /// preceding Break) — ONCE via `streaming_reparse`, then jumps to `end`. Lazy: a pure-quote
    /// body never trips it, so the staircase stays copy-free.
    GtFallback,
}

/// Does this freshly-finished block swallow a following blank line (mldoc's
/// `<* optional eols`)? Only `Quote`/`Custom` reach this (they are the sole callout
/// frames / sub-recursions); both absorb, matching mldoc's `absorb = true` after a
/// `#+BEGIN_X` block.
fn block_absorbs(b: &Block) -> bool {
    matches!(b, Block::Quote { .. } | Block::Custom { .. } | Block::Example { .. })
}

/// Per-line de-indent view: equivalent to `block_code_texts` applied to ONE line, O(1), no
/// alloc. `strip` = the cumulative first-line indent cleared by all ancestor frames. Leading
/// ws are ASCII (space/tab) ⇒ byte-safe. Composition:
/// `strip_view(strip_view(t, A), B) == strip_view(t, A+B)` for all t, A, B (see spec).
pub(crate) fn strip_view(text: &str, strip: usize) -> &str {
    if strip == 0 {
        return text;
    }
    let lw = leading_ws(text);
    if lw >= strip {
        &text[strip..]
    } else if text.trim().is_empty() {
        text
    } else {
        text.trim_start()
    }
}

/// View line `k` through the cumulative de-indent `strip` (no-op fast path when strip == 0).
#[inline]
fn line_text<'a>(lines: &[Line<'a>], k: usize, strip: usize) -> &'a str {
    strip_view(lines[k].text, strip)
}

/// Peel `n` blockquote `>`-levels off `s` (CONTINUATION semantics: each level is `trim_start`
/// then strip one leading `>`; a level with no `>` stops early — the lazy case). O(min(n, #`>`))
/// = O(len), a single scan. This is the `>`-analogue of `strip_view`'s cumulative indent strip
/// and composes the same way: `gt_peel` `n` levels == `n` applications of `quote_line_content`'s
/// peel (minus the breaker/blank `None` checks, which apply only at the FINAL level).
fn gt_peel(s: &str, n: usize) -> &str {
    let mut cur = s;
    for _ in 0..n {
        let t = cur.trim_start();
        match t.strip_prefix('>') {
            Some(rest) => cur = rest, // next iteration's trim_start handles the ws
            None => {
                crate::metrics::scan_work(s.len() - t.len()); // `>`-prefix bytes examined
                return t; // lazy: no `>` at this level ⇒ stop
            }
        }
    }
    crate::metrics::scan_work(s.len() - cur.len());
    cur
}

/// Null the inline spans of the DIRECT leaf blocks in `blocks` WITHOUT recursing into container
/// children (`Quote`/`Custom`/`List`). Used at a `>`-frame's close: that frame's nested `>`-quote
/// children and its `Step::GtFallback` reparse children are ALREADY fully null'd (each container
/// nulls its own subtree at close / `streaming_reparse` nulls its output), so re-descending them
/// is redundant AND — on a deep `>`-staircase (now uncapped ⇒ O(depth) nesting) — an O(depth)
/// NATIVE-RECURSION stack overflow. Nulling only THIS frame's copy-free leaves keeps the
/// whole-tree null invariant with O(1) native stack per close ⇒ O(n) total, no overflow.
fn none_out_frame_leaves(blocks: &mut [Block]) {
    use crate::parse::none_out_inlines;
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

/// The view of a `>`-frame CONTINUATION line: peel `gt_level` `>`s off the strip-viewed raw line
/// (`gt_peel` the first `gt_level-1`, then the FINAL peel via `quote_line_content_slice` so the
/// breaker/`>`-blank/blank boundary is honored). `None` ⇒ the line ends the run (bare blank or a
/// de-`>`'d breaker) ⇒ the `>`-frame closes. `gt_level >= 1`. Now used ONLY by the §3 fallback
/// (`Step::GtFallback`) — the per-line close/open walk uses `scan_gt_prefix` + `offs[·]` slices.
fn gt_cont_view(raw: &str, strip: usize, gt_level: usize) -> Option<&str> {
    quote_line_content_slice(gt_peel(strip_view(raw, strip), gt_level - 1))
}

/// A-org single prefix consume: walk `line2`'s leading `>`-prefix ONCE (the enclosing indent
/// already removed via `strip_view`), recording `offs[j]` = the byte offset into `line2` where
/// `gt_peel(line2, j)` begins, for j = 0..=g. So `&line2[offs[j]..] == gt_peel(line2, j)`, and
/// `quote_line_content_slice` / `quote_first_line_slice` on those slices reproduce the per-level
/// continuation / opener views with NO re-scan (each peels only 1 / ≤2 `>` — O(1)). Returns `g`
/// (the `>`-count) and charges `crate::metrics::scan_work` EXACTLY once (the `>`-prefix bytes) —
/// the single walk that replaces every per-frame `gt_cont_view` re-peel and every `Step::OpenQuote`
/// re-dispatch. `offs` is a reused scratch (cleared, not realloc'd).
fn scan_gt_prefix(line2: &str, offs: &mut Vec<usize>) -> usize {
    offs.clear();
    offs.push(0); // offs[0] = gt_peel(line2, 0) = line2 itself
    let mut cur = line2;
    loop {
        let t = cur.trim_start();
        match t.strip_prefix('>') {
            Some(rest) => {
                cur = rest;
                offs.push(line2.len() - rest.len());
            }
            None => break,
        }
    }
    let g = offs.len() - 1;
    // Bytes examined = the whole prefix up to the (trimmed) content — the same charge a full
    // `gt_peel(line2, g)` would make, but once per line instead of once per frame / re-dispatch.
    let content = line2[offs[g]..].trim_start();
    crate::metrics::scan_work(line2.len() - content.len());
    g
}

/// Pop the top frame and emit its block into the parent (`flush_para` → null spans → `finish` →
/// `parent.absorb`). `consume` (a HARD frame's `#+END_` closer at line `i`) makes the span end at
/// `lines[i].end`; a `>`-frame (`consume == false`) ends at the last body line `lines[i-1].end`.
/// The driver adjusts `i` (only a consume advances it). Shared by the hard-bound close (phase 1)
/// and the `>`-continuation close (phase 2a).
fn close_top(stack: &mut Vec<Frame>, lines: &[Line], input: &str, i: usize, consume: bool) {
    let mut f = stack.pop().unwrap();
    flush_para(&mut f.out, &mut f.para, &mut f.para_buf, input, false);
    // Transformed body (strip>0, nonstd eol, or a `>`-frame): null inline spans. A `>`-frame's
    // container children are already null'd (bottom-up) ⇒ non-recursive leaf null (deep-staircase
    // overflow guard); a callout body is shallow ⇒ recurse.
    if f.null_spans {
        if f.gt_level > 0 {
            none_out_frame_leaves(&mut f.out);
        } else {
            crate::parse::none_out_blocks(&mut f.out);
        }
    }
    let span_end = if consume {
        lines[i].end
    } else {
        // `>`-frame: line `i` (the continuation-fail / hard-bound line) is NOT in the run, so the
        // last body line is `i-1`; a `>`-frame is opened AND its content dispatched in one pass, so
        // it always survives ≥1 line past its opener before a LATER line closes it.
        debug_assert!(f.open_line < i);
        lines[i - 1].end
    };
    let block = f.builder.unwrap().finish(f.out, Some(Span(f.open_span_start, span_end)));
    let absorbs = block_absorbs(&block);
    let parent = stack.last_mut().unwrap();
    parent.out.push(block);
    parent.absorb = absorbs; // mldoc: a Quote/Custom swallows a following blank.
}

/// One open callout container on the streaming driver's explicit stack. Every re-dispatched
/// `#+BEGIN_QUOTE`/custom body — whether indent-0+plain-`\n` (clean window) or indented /
/// `\r`-terminated (strip-view frame) — lives here as a heap `Frame`. `ctx` is the child
/// context the PARENT set on push; `absorb` is this body's blank-swallow flag.
struct Frame {
    hi: usize,                    // EXCLUSIVE closer line index; line `hi` is the closer.
    ctx: Ctx,                     // child context (set by the parent on push).
    out: Vec<Block>,              // children of THIS body.
    para: Option<(usize, usize)>, // the open paragraph byte-window for THIS body.
    // In a `null_spans` (transformed) frame the paragraph's raw byte-window would keep the
    // per-line indent (only the first line is de-indented) AND any `\r`; so instead we
    // accumulate the VIEWED (`line_text`) line texts joined with `\n`, which normalizes BOTH
    // the cumulative indent (via `strip`) and `\r\n`→`\n` in one move. Some IFF a paragraph is
    // open in a null_spans frame (clean frames keep `para`'s `(start,end)` fast path).
    para_buf: Option<String>,
    absorb: bool,                 // did this body's last child swallow a following blank?
    builder: Option<Builder>,     // the opener → emitted on pop (None for the root).
    open_span_start: usize,       // byte offset of the opener line start (for the span).
    strip: usize,       // cumulative de-indent applied to every body-line view (0 = root/clean).
    null_spans: bool,   // body was transformed (strip>0 or nonstd eol) → null inline spans on pop.
    // A-org `>`-blockquote container frame (`gt_level == 0` for the root / `#+BEGIN_X` callout
    // frames). `gt_level` = the cumulative `>`-peel applied to CONTINUATION lines (composes on top
    // of the indent `strip`). The per-line `scan_gt_prefix` walk decides close/open/content for the
    // WHOLE `>`-stack in one pass — no per-frame re-peel, no opener re-dispatch. `open_line` is the
    // opener line index (kept for the pop invariant / span reasoning). `null_spans` is always true
    // for a `>`-frame (a `>`-body is transformed). Frame no longer borrows input (the deleted
    // `opener_content` field held the only `&'a str`) ⇒ no lifetime parameter.
    gt_level: usize,
    open_line: usize,
}

/// Re-parse a transformed (folded) body: routes back into `parse_org_streaming` with the child
/// `ctx`. Two callers remain (callouts and the `>`-quote staircase are now frames — P1/P3):
///   - **list-item content** (`in_item: true`): depth-1 — list re-entry is disabled by `in_item`,
///     so it can't nest into itself; skips the guard, uncapped.
///   - **the §3 `>`-quote fallback** (`in_item: false`): a `>`-body containing a fence / `#+BEGIN` /
///     LaTeX env / hiccup, de-`>`'d and reparsed once (those recognizers don't tolerate literal
///     `>`s). Guarded by `GT_FALLBACK_NEST_CAP` so construct-in-`>`-quote nesting can't SIGABRT
///     (graceful flat-Paragraph degradation past 64; see the const's doc).
fn streaming_reparse(input: &str, ctx: Ctx) -> Vec<Block> {
    // Every `streaming_reparse` input is a FOLDED buffer (a `\n`-joined list-item content or a
    // de-`>`'d quote-fallback body) whose byte positions don't map to the enclosing block body,
    // so the reparsed INLINE spans are meaningless → null them.
    let mut out = if ctx.in_item {
        parse_org_streaming(input, ctx)
    } else {
        let depth = BLOCK_DEPTH.with(|c| c.get());
        if depth >= GT_FALLBACK_NEST_CAP {
            if input.is_empty() {
                Vec::new()
            } else {
                vec![Block::Paragraph { inline: org_inline(input, 0), span: Some(Span(0, input.len())) }]
            }
        } else {
            BLOCK_DEPTH.with(|c| c.set(depth + 1));
            let o = parse_org_streaming(input, ctx);
            BLOCK_DEPTH.with(|c| c.set(depth));
            o
        }
    };
    crate::parse::none_out_blocks(&mut out);
    out
}

/// The shared per-construct closer/marker indexes over the WHOLE input (built ONCE, O(n)):
/// the `#+END_<name>` callout-closer trie, the `:END:` drawer-closer index, the whole-line
/// fence-marker index, and the list of lines whose terminator is NOT a plain single `\n`
/// (`\r\n` / lone `\r` / EOF). Both drivers query the first three with a `closer < hi`
/// bound; the streaming driver uses the last to decide when a callout body is a clean
/// WINDOW (for all-`\n` input it is empty ⇒ every indent-0 callout is a window frame).
fn build_org_indexes(
    lines: &[Line],
    input: &str,
) -> (EndTrie, Vec<usize>, Vec<usize>, Vec<usize>) {
    let mut end_trie = EndTrie::new();
    let mut drawer_end_idxs: Vec<usize> = Vec::new(); // `:END:` lines (drawer closers)
    let mut fence_lines: Vec<usize> = Vec::new();
    let mut nonstd_eol_lines: Vec<usize> = Vec::new();
    let bytes = input.as_bytes();
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
        // A plain single-`\n` terminator (so the line's [start,end) byte-range equals
        // `text` + `\n`, i.e. mldoc's `block_code` is a no-op on it). Anything else
        // (`\r\n` normalised to `\n`, a lone `\r` normalised, or the EOF line with no
        // terminator) makes the de-indented reparse string differ from the byte-window.
        let content_end = l.start + l.text.len();
        let plain_nl = l.end == content_end + 1 && bytes.get(content_end) == Some(&b'\n');
        if !plain_nl {
            nonstd_eol_lines.push(idx);
        }
    }
    (end_trie, drawer_end_idxs, fence_lines, nonstd_eol_lines)
}

/// The Org block driver: ONE
/// left-to-right pass over an explicit container-frame stack: a `#+BEGIN_QUOTE`/custom body
/// that is a CLEAN WINDOW (indent-0, plain-`\n` lines ⇒ the byte-range equals mldoc's
/// de-indented reparse string) is pushed as a `Frame` and scanned in place — never copied
/// or re-lexed (the removed recurse-on-body O(n²)). A de-indented / `\r`-terminated body is
/// a TRANSFORMED sub-recursion (`block_code` + `streaming_reparse`), byte-exact to mldoc
/// (local spans; gated by `harness/`). The `>`-quote and list-item content stay sub-recursions
/// routed through the SAME driver via `streaming_reparse`. `root_ctx` is the document
/// default `{false,false}` at the top level, or the child ctx of a transformed re-parse.
fn parse_org_streaming<'a>(input: &'a str, root_ctx: Ctx) -> Vec<Block> {
    let mut lines = split_lines(input);
    let last_rbracket = input.rfind(']');
    let (end_trie, drawer_end_idxs, fence_lines, nonstd_eol_lines) =
        build_org_indexes(&lines, input);
    let n = lines.len();

    let mut stack: Vec<Frame> = vec![Frame {
        hi: n,
        ctx: root_ctx,
        out: Vec::new(),
        para: None,
        para_buf: None,
        absorb: false,
        builder: None,
        open_span_start: 0,
        strip: 0,
        null_spans: false,
        gt_level: 0,
        open_line: 0,
    }];
    let mut collapse_floor = 0usize; // shared & monotone (i is monotone across frames).
    let mut fence_cursor: usize = 0;
    let mut drawer_cursor: usize = 0; // monotone `:END:` cursor (find_drawer_end), shared across the pass.
    let mut nonstd_cursor: usize = 0; // monotone nonstd-eol cursor (body_is_clean_window), ditto.
    let mut offs: Vec<usize> = Vec::new(); // reused `>`-prefix offset scratch (scan_gt_prefix)
    // F4: set by an empty headline marker (`* ` trailing-ws para), consumed by the NEXT line's
    // dispatch — a drop-trigger block drops the para, anything else clears the flag.
    let mut ws_drop = false;
    let mut i = 0;

    loop {
        // --- Phase 1: close at the HARD bound. Any frame with `hi <= i` closes: a HARD frame
        // (root / `#+BEGIN_X` callout, `gt_level==0`) CONSUMES its `#+END_` closer (`i += 1`); a
        // `>`-frame that lazily continued up to the enclosing closer closes WITHOUT consuming. (A
        // `>`-frame's DYNAMIC continuation-close, at `i < hi`, is phase 2a — so phase 1 never needs
        // `offs`, and after it every open frame has `hi > i`.)
        while stack.len() > 1 && stack.last().unwrap().hi <= i {
            let consume = stack.last().unwrap().gt_level == 0;
            close_top(&mut stack, &lines, input, i, consume);
            if consume {
                i += 1;
            }
        }
        if i >= n {
            break;
        }

        // --- Phase 2: the single `>`-container prefix consume. `strip` is the enclosing hard frame's
        // indent (shared by every `>`-frame in the contiguous stack above it); `line2` the de-indented
        // line. Run the walk iff there are open `>`-frames OR the line might open one; a plain non-`>`
        // line at a hard frame skips to a normal dispatch (`view = None`).
        let strip = stack.last().unwrap().strip;
        let line2 = strip_view(lines[i].text, strip);
        let scanned = stack.last().unwrap().gt_level > 0 || line2.trim_start().starts_with('>');

        let (dispatch_view, gt_level_disp): (Option<&str>, usize) = if scanned {
            let g = scan_gt_prefix(line2, &mut offs); // ONE `>`-prefix walk; charges scan_work once

            // Phase 2a: close `>`-frames whose continuation view is `None` (all `i < hi` now ⇒ no
            // `i`-advance). `offs[min(L-1, g)]` is the pre-scanned `gt_peel(line2, L-1)` slice, so
            // `quote_line_content_slice` on it is byte-identical to `gt_cont_view`, at O(1) per pop.
            while stack.len() > 1 && stack.last().unwrap().gt_level > 0 {
                let l = stack.last().unwrap().gt_level;
                if quote_line_content_slice(&line2[offs[(l - 1).min(g)]..]).is_some() {
                    break;
                }
                close_top(&mut stack, &lines, input, i, false);
            }

            // Phase 2b: open new `>`-frames. `cur` starts at the surviving top's level-`H` view
            // (`H == 0` ⇒ the hard frame ⇒ the whole de-indented line). `quote_first_line_slice`
            // peels ≤2 `>` per step (opener-2; the ⌈N/2⌉ + reject-on-first-line breaker rules live
            // INSIDE it), advancing the slice — no re-dispatch, O(1) per opened frame. Flush the
            // parent (+F4 ws-drop) ONCE, before the nested-quote chain.
            let h = stack.last().unwrap().gt_level;
            let mut cur = if h == 0 {
                line2
            } else {
                quote_line_content_slice(&line2[offs[(h - 1).min(g)]..]).unwrap_or("")
            };
            let mut opened_any = false;
            while let Some(inner) = quote_first_line_slice(cur) {
                let (p_hi, p_strip, p_gt) = {
                    let top = stack.last_mut().unwrap();
                    if !opened_any {
                        // F4: drop the empty `* ` trailing-ws para before this quote opener, then
                        // flush the parent's paragraph (`between_eols`) before the nested quote.
                        let was = std::mem::replace(&mut ws_drop, false);
                        if was && para_ws_only(&top.para, input) {
                            top.para = None;
                        }
                        flush_para(&mut top.out, &mut top.para, &mut top.para_buf, input, top.ctx.in_item || top.ctx.in_quote);
                        opened_any = true;
                    }
                    (top.hi, top.strip, top.gt_level)
                };
                stack.push(Frame {
                    hi: p_hi, // inherit the enclosing hard bound (a `>`-quote can't cross a callout closer)
                    ctx: Ctx { in_item: false, in_quote: true },
                    out: Vec::new(),
                    para: None,
                    para_buf: None,
                    absorb: false,
                    builder: Some(Builder::Quote),
                    open_span_start: lines[i].start,
                    strip: p_strip,   // inherit the ancestor indent strip
                    null_spans: true, // a `>`-body is transformed ⇒ null inline spans on pop
                    gt_level: p_gt + 1,
                    open_line: i,
                });
                cur = inner;
            }
            (Some(cur), stack.last().unwrap().gt_level)
        } else {
            (None, 0)
        };

        // Phase 2c: dispatch the (final) view ONCE, at the deepest level. `dispatch_view` is the
        // `>`-view for a `>`-line (the ladder's quote-step is deleted — opening is phase 2b) / `None`
        // for a plain hard-frame line (⇒ `line_text`). A `>`-frame dispatch returns Next / GtFallback;
        // a hard-frame line may return Open (a `#+BEGIN_X` callout).
        let step = {
            let top = stack.last_mut().unwrap();
            let hi = top.hi;
            let ctx = top.ctx;
            let null_spans = top.null_spans;
            dispatch_org_line(
                i,
                &mut lines,
                &mut top.out,
                &mut top.para,
                &mut top.para_buf,
                &mut top.absorb,
                &mut collapse_floor,
                &mut fence_cursor,
                &mut drawer_cursor,
                &mut ws_drop,
                ctx,
                hi,
                &end_trie,
                &drawer_end_idxs,
                &fence_lines,
                last_rbracket,
                input,
                strip,
                null_spans,
                gt_level_disp,
                dispatch_view,
            )
        };
        match step {
            Step::Next(ni) => i = ni,
            Step::Open { close, builder, child_ctx, indent_strip } => {
                let top_strip = stack.last().unwrap().strip;
                {
                    let top = stack.last_mut().unwrap();
                    // Preceding paragraph drops its trailing Break before this container opener
                    // when already inside a list-item / blockquote body (`between_eols`).
                    flush_para(&mut top.out, &mut top.para, &mut top.para_buf, input, top.ctx.in_item || top.ctx.in_quote);
                }
                // A `#+BEGIN_X` callout body — clean-window (spans global) or strip-view / nonstd
                // (spans nulled on pop). Callouts only open at `gt_level==0` (inside a `>`-frame they
                // are a §3 fallback tell), so a callout frame's `gt_level` is always 0.
                let child_strip = top_strip + indent_strip;
                let null_spans = child_strip > 0
                    || !body_is_clean_window(&nonstd_eol_lines, &mut nonstd_cursor, i + 1, close);
                stack.push(Frame {
                    hi: close,
                    ctx: child_ctx,
                    out: Vec::new(),
                    para: None,
                    para_buf: None,
                    absorb: false,
                    builder: Some(builder),
                    open_span_start: lines[i].start,
                    strip: child_strip,
                    null_spans,
                    gt_level: 0,
                    open_line: 0,
                });
                i += 1;
            }
            Step::GtFallback => {
                // §3: the top `>`-frame's content (`cur`) opens a construct that can't be classified
                // copy-free. Reparse `[i, end)` de-`>`'d ONCE via `streaming_reparse`, PREFIXED by
                // any pending copy-free paragraph (`para_buf`) so a degraded construct coalesces /
                // a real block's preceding Break is trimmed. Line `i`'s view is exactly `cur`.
                let (p_hi, p_gt) = {
                    let t = stack.last().unwrap();
                    (t.hi, t.gt_level)
                };
                let mut de_gt = {
                    let top = stack.last_mut().unwrap();
                    top.para = None;
                    top.para_buf.take().unwrap_or_default()
                };
                de_gt.push_str(dispatch_view.unwrap_or(""));
                de_gt.push('\n');
                let mut end = i + 1;
                while end < p_hi {
                    match gt_cont_view(lines[end].text, strip, p_gt) {
                        Some(v) => {
                            de_gt.push_str(v);
                            de_gt.push('\n');
                            end += 1;
                        }
                        None => break,
                    }
                }
                let children = streaming_reparse(&de_gt, Ctx { in_item: false, in_quote: true });
                stack.last_mut().unwrap().out.extend(children);
                i = end; // the frame closes next iteration (i == end ⇒ continuation `None` or `hi`)
            }
        }
    }

    let mut root = stack.pop().unwrap();
    flush_para(&mut root.out, &mut root.para, &mut root.para_buf, input, false);
    root.out
}

/// No non-plain-`\n` body line in `[lo, hi)` (the caller has already applied the indent-0
/// guard) ⇒ the callout body's byte-range equals mldoc's de-indented `block_code` reparse
/// string, so it can be a streaming WINDOW frame. For all-`\n` input `nonstd_eol_lines` is
/// empty ⇒ always true (the common path: every indent-0 callout is a window frame).
///
/// Uses a monotone `cursor` (advance-only) instead of `partition_point`: callout openers are
/// reached with non-decreasing `lo`, so the first nonstd-eol `>= lo` only moves forward ⇒ O(1)
/// amortized, O(n) total. The cursor stops AT (does not consume) that line, so the `< hi`
/// emptiness test is unaffected and a later opener (larger `lo`) advances past lines now behind it.
fn body_is_clean_window(nonstd_eol_lines: &[usize], cursor: &mut usize, lo: usize, hi: usize) -> bool {
    while *cursor < nonstd_eol_lines.len() && nonstd_eol_lines[*cursor] < lo {
        *cursor += 1;
    }
    !(*cursor < nonstd_eol_lines.len() && nonstd_eol_lines[*cursor] < hi)
}

/// Classify ONE Org line `i` in the body bounded by `hi` (EXCLUSIVE closer-line index),
/// writing any completed block into `out` / accumulating into `para`, threading `absorb`
/// (blank-swallow) + `collapse_floor` (list-collapse memo) + `fence_cursor`, and return a
/// `Step`. This is the per-line dispatch ladder for the streaming driver (on `Open` it
/// pushes a window frame or sub-recurses the de-indented body). Every forward closer-search
/// is bounded by `hi` / `body_end` so a closer / `\end{}` / `]` / run-line BELONGS to this
/// body, never the enclosing one; at the top level `hi == lines.len()` (`body_end ==
/// input.len()`) so all bounds are no-ops. The `>`-quote and list-item sub-recursions
/// re-enter via `streaming_reparse`. `ctx.in_item`/`ctx.in_quote`
/// gate the context-restricted constructs.
#[allow(clippy::too_many_arguments)]
fn dispatch_org_line<'a>(
    i: usize,
    lines: &mut [Line<'a>],
    out: &mut Vec<Block>,
    para: &mut Option<(usize, usize)>,
    para_buf: &mut Option<String>,
    absorb: &mut bool,
    collapse_floor: &mut usize,
    fence_cursor: &mut usize,
    drawer_cursor: &mut usize,
    ws_drop: &mut bool,
    ctx: Ctx,
    hi: usize,
    end_trie: &EndTrie,
    drawer_end_idxs: &[usize],
    fence_lines: &[usize],
    last_rbracket: Option<usize>,
    input: &'a str,
    strip: usize,
    null_spans: bool,
    // A-org: `gt_level` is the FINAL `>`-depth the driver's prefix consume settled this line at
    // (0 for a hard-frame / plain line — behavior identical to before; `> 0` for a `>`-frame). `view`
    // is the pre-computed view the driver already de-`>`'d (`cur`) — `None` only for a plain line,
    // where `t` falls back to `line_text`. The dispatch NO LONGER opens quotes (the prefix consume
    // did) — its former step-8 is deleted; a `>`-line reaches here only for its leaf/blank/§3-tell.
    gt_level: usize,
    view: Option<&'a str>,
) -> Step {
    // Copy out the line fields (a `&'a str` + two `usize`s, none borrowing `lines`) so the
    // headline / hiccup splits can REWRITE `lines[i]`/`lines[ri]` in place (steps 3, 13b).
    // `t` is the line's VIEW: the driver's pre-de-`>`'d `view` (`>`-frame or `>`-line at the root),
    // else the strip-viewed `line_text` (plain line). `line_content_end` uses the original text
    // length so `parse_latex_env`'s `line_end` bound is correct (hard frames only — a `>`-frame
    // routes latex to the §3 fallback before `parse_latex_env` is reached).
    let t = match view {
        Some(v) => v,
        None => line_text(lines, i, strip),
    };
    let line_start = lines[i].start;
    let line_end = lines[i].end;
    let line_content_end_orig = line_start + lines[i].text.len(); // for parse_latex_env
    let in_item = ctx.in_item;
    // F4: read + clear the empty-marker ws-drop flag set by the PREVIOUS line. A drop-trigger
    // block (drawer/table/fence/`#+BEGIN`/verbatim/`>`-quote/`$$`/raw-html/hr) drops the empty
    // `* ` headline's trailing-ws paragraph; any other line leaves the flag cleared.
    let was_ws_drop = std::mem::replace(ws_drop, false);
    // A paragraph flushed because a following BLOCK begins drops its trailing `Break_Line` when
    // that block parser claims the eol first (mldoc `between_eols`) — true in BOTH list-item
    // content (`in_item`) AND a blockquote body (`in_quote`); at the document level
    // `Paragraph.sep` claims the eol and the Break stays. (EOF flushes pass `false` explicitly.)
    let trim = in_item || ctx.in_quote;
    // Byte offset where THIS body ends (the closer line's start, or EOF at the root). CLAMPs
    // the to-end-of-input forward-scanners (`parse_latex_env`, `parse_hiccup`).
    let body_end = if hi < lines.len() { lines[hi].start } else { input.len() };

    // blank line: extend an open paragraph, else swallow (if absorbing) or start one.
    if t.trim().is_empty() {
        // Is a paragraph open OR being started (not swallowed by a preceding block)?
        let open_para = para.is_some() || !*absorb;
        if let Some((s, _)) = *para {
            *para = Some((s, line_end));
        } else if *absorb {
            // swallowed by the preceding block.
        } else {
            *para = Some((line_start, line_end));
        }
        // null_spans frame: mirror into the de-indented buffer (blank line ⇒ empty content +
        // the `\n` Break delimiter; keeps para/para_buf in lockstep).
        if null_spans && open_para {
            let b = para_buf.get_or_insert_with(String::new);
            b.push_str(t); // "" for a truly-blank line; strip-viewed otherwise
            b.push('\n');
        }
        return Step::Next(i + 1);
    }

    // P3 §3: inside a `>`-frame, a de-`>`'d view opening a construct whose recognition needs the
    // literal `>`s stripped from what the GLOBAL raw-input indexes/scanners see — fenced code /
    // `#+BEGIN_X` callout (`fence_lines`/`EndTrie` never record a `>`-prefixed closer), a LaTeX env
    // or block hiccup (`parse_latex_env`/`parse_hiccup` scan raw bytes), or a raw-input multi-line
    // BUILDER (table cells / verbatim / list content read raw `input`) — cannot be handled copy-free.
    // Hand the frame's remaining body to the bounded de-`>`'d reparse (`Step::GtFallback`). The
    // single-line leaves (directive/comment/`$$`/raw-html/hr), paragraphs, and NESTED `>`-quotes
    // stay copy-free below, so a pure-quote staircase never reaches here. (Over-routing is only a
    // perf cost — the fallback runs the identical ladder — so these tells may be conservative.)
    if gt_level > 0
        && (fence_marker(t).is_some()
            || block_begin(t).is_some()
            || t.trim_start().starts_with("\\begin{")
            || t.trim_start().starts_with("[:")
            || is_table_row(t)
            || is_verbatim_line(t)
            || list_marker(t).is_some())
    {
        return Step::GtFallback;
    }

    // 1. directive `#+KEY: value` (KEY != BEGIN_…) — not a list-item content block.
    if let Some((name, value)) = directive(t).filter(|_| !in_item) {
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::Directive { name, value, span: Some(Span(line_start, line_end)) });
        *absorb = true;
        return Step::Next(i + 1);
    }

    // 1b. comment `# text` (mldoc Comment) — IS a valid list-item content block (not gated).
    if let Some(text) = org_comment(t) {
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::Comment { text: text.to_string(), span: Some(Span(line_start, line_end)) });
        *absorb = true;
        return Step::Next(i + 1);
    }

    // 2. drawer `:PROPERTIES:`/`:NAME:` … `:END:` — not a list-item content block, and NOT
    // inside a `#+BEGIN_X` / `>`-quote body (mldoc's `block_content_parsers` omits `Drawer`,
    // so a `:NAME:` there is verbatim/text). The `:END:` must lie inside THIS body (`< hi`);
    // else it belongs to an enclosing body. F2.
    if let Some(name) = drawer_begin(t).filter(|_| !in_item && !ctx.in_quote) {
        if let Some(close) = find_drawer_end(drawer_end_idxs, drawer_cursor, i) {
            if close < hi {
                if was_ws_drop && para_ws_only(para, input) {
                    *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
                }
                flush_para(out, para, para_buf, input, trim);
                // mldoc (`drawer.ml`) makes a `Property_Drawer` ONLY when the WHOLE body
                // parses as `many1 property` — every body line a valid `:key: value` (empty
                // body allowed). If ANY line fails (plain text, blank, markdown `key:: v`),
                // `parse1` can't reach `:END:` → it falls back to `drawer_parse` → a generic
                // `Drawer{name:"properties"}` (no props, no directive folding, no value
                // ref-walking). C3.
                if name == "properties"
                    && (i + 1..close)
                        .all(|k| drawer_property(line_text(lines, k, strip)).is_some())
                {
                    let mut props: Vec<(String, String)> = (i + 1..close)
                        .filter_map(|k| drawer_property(line_text(lines, k, strip)))
                        .collect();
                    // mldoc `Drawer.parse` folds trailing `#+NAME: value` directives into the
                    // same Property_Drawer. Bounded by `hi` (don't cross the frame closer).
                    let mut j = close + 1;
                    let mut folded = false;
                    while j < hi {
                        if let Some(kv) = directive(line_text(lines, j, strip)) {
                            props.push(kv);
                            folded = true;
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    let end = lines[j - 1].end;
                    out.push(Block::Properties { props, span: Some(Span(line_start, end)) });
                    *absorb = folded;
                    return Step::Next(j);
                }
                out.push(Block::Drawer { name, span: Some(Span(line_start, lines[close].end)) });
                *absorb = false;
                return Step::Next(close + 1);
            }
            // :END: is outside this body → fall through.
        }
    }

    // 3. headline `*{n} ` — not a list-item content block, and NOT inside a blockquote body
    // (mldoc: `* x` in a quote is a Paragraph). C2.
    if let Some(level) = headline_level(t).filter(|_| !in_item && !ctx.in_quote) {
        let stars = t.bytes().take_while(|&b| b == b'*').count();
        let after = t[stars..].trim_start();
        let (marker, priority, content) = split_markers(after);
        let content_off = line_start + (t.len() - content.len());

        // SPLIT: the post-marker CONTENT begins a block-construct opener ⇒ emit an empty
        // bullet (keeping level/marker/priority) and reparse CONTENT as the following block.
        // The split lookahead is bounded by `hi`/`body_end` (a `#+BEGIN`/fence/`\end{}` that
        // closes OUTSIDE this body does not split — it belongs to an enclosing body).
        if !content.is_empty()
            && headline_split_opener(
                content,
                input,
                content_off,
                i,
                hi,
                body_end,
                end_trie,
                fence_lines,
                fence_cursor,
            )
        {
            flush_para(out, para, para_buf, input, trim);
            out.push(Block::Bullet {
                level,
                size: None,
                inline: vec![],
                marker,
                priority,
                htags: vec![],
                span: Some(Span(line_start, content_off)),
            });
            // markdown ```/~~~ fence → Src (the closer is `< hi`, ensured by the predicate).
            if let Some((_fchar, frun)) = fence_marker(content) {
                if let Some(close) = find_matching_fence(fence_lines, fence_cursor, i) {
                    let code = if close > i + 1 {
                        input[lines[i + 1].start..lines[close - 1].end].to_string()
                    } else {
                        String::new()
                    };
                    let lang = content[frun..].trim().to_string();
                    out.push(Block::Src { lang, code, span: Some(Span(content_off, lines[close].end)) });
                    *absorb = true;
                    return Step::Next(close + 1);
                }
            }
            // Generic reparse: REWRITE this line to its CONTENT slice and re-enter WITHOUT
            // advancing `i` (the rewrite never creates an END/fence/drawer opener, so the
            // precompute + open frames' `hi` stay valid). Terminates: `content` begins a
            // non-`*` opener, so the headline branch can't re-fire.
            lines[i] = Line { start: content_off, end: line_end, text: content };
            *absorb = false;
            return Step::Next(i);
        }

        flush_para(out, para, para_buf, input, trim);
        let mut inline = org_inline(content, crate::inline::ptr_base(content, input));
        let htags = extract_htags(&mut inline);
        let empty_title = inline.is_empty() && htags.is_empty();
        out.push(Block::Bullet {
            level,
            size: None,
            inline,
            marker,
            priority,
            htags,
            span: Some(Span(line_start, line_end)),
        });
        *absorb = false;
        // mldoc quirk: an empty-title headline with trailing whitespace begins a paragraph
        // from that whitespace (`* \nx` → Bullet + Paragraph[" ", Break, "x"]).
        if empty_title {
            let content_len = t.trim_end_matches([' ', '\t']).len();
            if content_len < t.len() {
                *para = Some((line_start + content_len, line_end));
                *ws_drop = true; // F4: droppable if the next line opens a block.
            }
        }
        return Step::Next(i + 1);
    }

    // 4. table (group of consecutive well-formed `|…|` rows), bounded by `hi`.
    if is_table_row(t) {
        if was_ws_drop && para_ws_only(para, input) {
            *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
        }
        flush_para(out, para, para_buf, input, trim);
        let start = i;
        let mut ni = i;
        while ni < hi && is_table_row(line_text(lines, ni, strip)) {
            ni += 1;
        }
        out.push(build_table(&lines[start..ni], lines[start].start, lines[ni - 1].end, input));
        *absorb = false;
        return Step::Next(ni);
    }

    // 4b. LaTeX environment `\begin{X} … \end{X}` (mldoc Latex_env, before Block). CLAMP the
    // `\end{}` search to `&input[..body_end]` so an `\end{X}` outside this body is not taken.
    // `line_content_end_orig` (set above from the original text length) keeps the closing-brace
    // search in-bounds even when `t` is a strip-view shorter than the original line.
    if let Some((name, content, consumed_end)) =
        crate::inline::parse_latex_env(&input[..body_end], line_start, line_content_end_orig)
    {
        // latex_env is the ONLY block_content construct that does NOT consume the preceding eol,
        // so inside a `#+BEGIN_X` / `>`-quote body a paragraph KEEPS its trailing Break before it
        // and the eol AFTER it becomes a Break-paragraph (mldoc `Paragraph.sep`-last). Never trim.
        flush_para(out, para, para_buf, input, false);
        let mut ni = i + 1;
        while ni < lines.len() && lines[ni].start < consumed_end {
            ni += 1;
        }
        // In a null_spans frame `content` sliced from raw `input` keeps the per-line indent (and
        // any `\r`). Re-run parse_latex_env over the VIEWED (de-indented, `\r`-free) body window
        // to get the reparse-faithful content; the STRUCTURE (name / consumed_end / ni) stays
        // from the raw pass. O(n): each body line belongs to exactly one leaf construct.
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
        *absorb = false;
        if ctx.in_quote {
            let trail_end = if ni < lines.len() { lines[ni].start } else { body_end };
            if consumed_end < trail_end {
                *para = Some((consumed_end, trail_end));
                // Keep para/para_buf in lockstep in a null_spans frame: the trailing region is a
                // line terminator (a single Break); normalize `\r\n`→`\n`.
                if null_spans {
                    *para_buf = Some("\n".to_string());
                }
            }
        }
        return Step::Next(ni);
    }

    // 5. fenced code block (```/~~~). ON-DEMAND; the closer must lie inside THIS body
    // (`< hi`), else this fence is unclosed here → fall through to a later classifier.
    if let Some((_c, mend)) = fence_marker(t) {
        if let Some(close) = find_matching_fence(fence_lines, fence_cursor, i) {
            if close < hi {
                if was_ws_drop && para_ws_only(para, input) {
                    *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
                }
                flush_para(out, para, para_buf, input, trim);
                // Body lines via line_text: strip-view drops the cumulative indent (= block_code
                // semantics for strip>0) and is identical to `input[start..end]` for strip==0
                // (clean-window bodies have only plain-`\n` lines, so text+"\n" == raw slice).
                let code = if close > i + 1 {
                    let mut s = String::new();
                    for k in i + 1..close {
                        s.push_str(line_text(lines, k, strip));
                        s.push('\n');
                    }
                    s
                } else {
                    String::new()
                };
                let lang = t[mend..].trim().to_string();
                out.push(Block::Src { lang, code, span: Some(Span(line_start, lines[close].end)) });
                *absorb = true;
                return Step::Next(close + 1);
            }
            // closer is outside this body → fall through.
        }
    }

    // 6. `#+BEGIN_X` … `#+END_X` block. The closer must lie inside THIS body (`< hi`).
    // QUOTE/custom become re-dispatched `Open` containers (the driver handles the body);
    // SRC/EXAMPLE are raw bodies consumed in place (`block_code`).
    if let Some(name) = block_begin(t) {
        if let Some(close) = end_trie.find(&name, i) {
            if close < hi {
                if was_ws_drop && para_ws_only(para, input) {
                    *para = None; // F4: drop the empty `* ` trailing-ws para (driver flushes
                                  // for QUOTE/custom Open; src/example flush in place below).
                }
                let lname = name.to_ascii_lowercase();
                match lname.as_str() {
                    "src" => {
                        flush_para(out, para, para_buf, input, trim);
                        let lang = begin_lang(t);
                        // Body via line_text: applies the cumulative strip (matching block_code
                        // semantics for nested indented bodies; no-op for strip==0).
                        let texts: Vec<&str> = (i + 1..close).map(|k| line_text(lines, k, strip)).collect();
                        let inner = block_code_texts(&texts);
                        out.push(Block::Src { lang, code: inner, span: Some(Span(line_start, lines[close].end)) });
                        *absorb = true;
                        return Step::Next(close + 1);
                    }
                    "example" => {
                        flush_para(out, para, para_buf, input, trim);
                        let texts: Vec<&str> = (i + 1..close).map(|k| line_text(lines, k, strip)).collect();
                        let inner = block_code_texts(&texts);
                        out.push(Block::Example { code: inner, span: Some(Span(line_start, lines[close].end)) });
                        *absorb = true;
                        return Step::Next(close + 1);
                    }
                    "quote" => {
                        // indent_strip: leading ws of the VIEWED first body line (parent strip
                        // already applied via line_text; child_strip = strip + indent_strip in
                        // the Step::Open handler).
                        let indent_strip =
                            if close > i + 1 { leading_ws(line_text(lines, i + 1, strip)) } else { 0 };
                        return Step::Open {
                            close,
                            builder: Builder::Quote,
                            child_ctx: Ctx { in_item: false, in_quote: true },
                            indent_strip,
                        };
                    }
                    _ => {
                        let indent_strip =
                            if close > i + 1 { leading_ws(line_text(lines, i + 1, strip)) } else { 0 };
                        // mldoc reparses a custom `#+BEGIN_X` body with `block_content_parsers`
                        // — the SAME grammar as a QUOTE body (omits headline/drawer/footnote)
                        // — so the child context is "in block content" (`in_quote: true`),
                        // which also drops a paragraph's trailing Break before a following
                        // block (mldoc `between_eols`/`concat`). F2.
                        return Step::Open {
                            close,
                            builder: Builder::Custom(lname),
                            child_ctx: Ctx { in_item: false, in_quote: true },
                            indent_strip,
                        };
                    }
                }
            }
            // closer is outside this body → fall through.
        }
    }

    // 7. verbatim block (Org): consecutive lines starting with `:` → Example. Bounded by `hi`.
    if is_verbatim_line(t) {
        if was_ws_drop && para_ws_only(para, input) {
            *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
        }
        flush_para(out, para, para_buf, input, trim);
        let start = i;
        let mut code = String::new();
        let mut ni = i;
        while ni < hi && is_verbatim_line(line_text(lines, ni, strip)) {
            code.push_str(verbatim_content(line_text(lines, ni, strip)));
            code.push('\n');
            ni += 1;
        }
        out.push(Block::Example { code, span: Some(Span(lines[start].start, lines[ni - 1].end)) });
        *absorb = true;
        return Step::Next(ni);
    }

    // 8. markdown blockquote — DELETED (A-org). Opening a `>`-quote is now the driver's
    // `scan_gt_prefix` prefix consume (close/open/content in ONE walk, no re-dispatch); `t` here is
    // already fully de-`>`'d content, so it never opens a quote. The F4 ws-drop before a quote moved
    // to the driver's open loop.

    // 9. block-level displayed math `$$ … $$`.
    if let Some(math) = displayed_math(t) {
        if was_ws_drop && para_ws_only(para, input) {
            *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
        }
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::DisplayedMath { text: math, span: Some(Span(line_start, line_end)) });
        *absorb = false;
        return Step::Next(i + 1);
    }

    // 10. raw HTML (single line, complete element).
    if is_raw_html(t) {
        if was_ws_drop && para_ws_only(para, input) {
            *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
        }
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::RawHtml { text: t.to_string(), span: Some(Span(line_start, line_end)) });
        *absorb = false;
        return Step::Next(i + 1);
    }

    // 11. footnote definition `[fn:name] text` — not a list-item content block, and NOT
    // inside a `#+BEGIN_X` / `>`-quote body (mldoc's `block_content_parsers` omits
    // `Footnote`, so `[fn:n] …` there stays a paragraph with an inline footnote ref). The
    // body absorbs following continuation lines (mldoc `many1 l`); bounded by `hi`. F2.
    if let Some((name, content)) = footnote_def(t).filter(|_| !in_item && !ctx.in_quote) {
        flush_para(out, para, para_buf, input, trim);
        let mut body = strip_cr_eol(content, line_has_nl(input, &lines[i])).to_string();
        let mut j = i + 1;
        while j < hi {
            match footnote_cont(line_text(lines, j, strip), line_has_nl(input, &lines[j])) {
                Some(c) => {
                    body.push('\n');
                    body.push_str(c);
                    j += 1;
                }
                None => break,
            }
        }
        // `body` is a FOLDED (possibly multi-line joined) buffer → inline spans don't map to
        // the block body; drop them.
        let mut inl = org_inline(&body, 0);
        crate::parse::none_out_inlines(&mut inl);
        out.push(Block::FootnoteDef {
            name,
            inline: inl,
            span: Some(Span(line_start, lines[j - 1].end)),
        });
        *absorb = true;
        return Step::Next(j);
    }

    // 12. list — bounded by `hi`; item content re-parsed via `streaming_reparse`. Disabled in
    // list-item content; `collapse_floor` skips list-starts inside an already-collapsed region.
    if !in_item && i >= *collapse_floor && list_marker(t).is_some() {
        match collect_list(lines, i, hi, Ctx { in_item: true, in_quote: ctx.in_quote }, strip) {
            Ok((block, next)) => {
                flush_para(out, para, para_buf, input, trim);
                out.push(block);
                *absorb = false;
                return Step::Next(next);
            }
            Err(Collapse { kept, resume, trigger }) => {
                *collapse_floor = trigger;
                if let Some(block) = kept {
                    flush_para(out, para, para_buf, input, trim);
                    out.push(block);
                    *absorb = false;
                    return Step::Next(resume);
                }
                // full collapse (resume == i == start): fall through to paragraph.
            }
        }
    }

    // 13. horizontal rule (exactly 5 dashes).
    if is_org_hr(t) {
        if was_ws_drop && para_ws_only(para, input) {
            *para = None; // F4: drop the empty `* ` trailing-ws para before a block.
        }
        flush_para(out, para, para_buf, input, trim);
        out.push(Block::Hr { span: Some(Span(line_start, line_end)) });
        *absorb = false;
        return Step::Next(i + 1);
    }

    // 13b. block-level Clojure-hiccup `[:tag …]` at BOL. The balanced capture is CLAMPed to
    // `&input[..body_end]`; the remainder past the `]` re-enters block parsing at BOL.
    // Consecutive block hiccups (`[:a][:b]…`) are consumed in ONE LOCAL LOOP, not by
    // re-dispatching the whole shrinking remainder line through the full ladder per vector (which
    // re-ran every earlier predicate on the tail each time → O(n²); the md twin is parse.rs 11d').
    // Control returns to the main loop exactly ONCE: at the frame boundary, or for the first
    // non-hiccup remainder.
    {
        let mut cur = i;
        let mut captured = false;
        loop {
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
            let Some(cap_end) = crate::inline::parse_hiccup(&input[..body_end], rec) else {
                break;
            };
            // A preceding paragraph drops its trailing Break before a Hiccup inside a blockquote
            // body / list item, but keeps it at the document level.
            flush_para(out, para, para_buf, input, trim); // no-op after the first
            out.push(Block::Hiccup {
                v: input[rec..cap_end].to_string(),
                span: Some(Span(cur_start, cap_end)),
            });
            *absorb = false;
            captured = true;
            // Resume after the `]`, absorbing consecutive eols (mldoc `<* optional eols`). The eol
            // run stops at the closer line (`#+END_…` is non-eol), so it never crosses the body.
            let bytes = input.as_bytes();
            let mut resume = cap_end;
            while resume < bytes.len() && matches!(bytes[resume], b'\n' | b'\r') {
                resume += 1;
            }
            if resume >= bytes.len() {
                return Step::Next(lines.len()); // captured to EOF (+ trailing eols)
            }
            let mut ri = cur;
            while ri < lines.len() && lines[ri].end <= resume {
                ri += 1;
            }
            if ri >= lines.len() {
                return Step::Next(lines.len()); // defensive (resume < len ⇒ unreachable)
            }
            if resume > lines[ri].start {
                let content_end = lines[ri].start + lines[ri].text.len();
                lines[ri] = Line { start: resume, end: lines[ri].end, text: &input[resume..content_end] };
            }
            cur = ri;
        }
        if captured {
            return Step::Next(cur);
        }
    }

    // 14. plain line → accumulate into the current paragraph.
    *para = Some(match *para {
        Some((s, _)) => (s, line_end),
        None => (line_start, line_end),
    });
    // null_spans frame: accumulate the VIEWED line text (de-indented, `\r`-free) into the
    // paragraph buffer, joined with `\n` (the Break delimiter). This is what flush_para parses
    // instead of the raw byte-window, so continuation lines are de-indented too — not just the
    // first line — and a `\r\n` body never yields a stray extra Break.
    if null_spans {
        let b = para_buf.get_or_insert_with(String::new);
        b.push_str(t);
        b.push('\n');
    }
    *absorb = false;
    Step::Next(i + 1)
}

/// Flush the open paragraph. `trim_eol` drops trailing newline(s) from the slice
/// (so no trailing `Break_Line`): in list-item content (`in_item`) a *following block*
/// absorbs the paragraph's trailing eols via mldoc's `between_eols` (its block parsers
/// are tried before `Paragraph.sep`), whereas at the document level `Paragraph.sep`
/// claims the eol first and it stays a Break. EOF / end-of-content flushes pass `false`.
fn flush_para(
    out: &mut Vec<Block>,
    para: &mut Option<(usize, usize)>,
    para_buf: &mut Option<String>,
    input: &str,
    trim_eol: bool,
) {
    // null_spans frame: the paragraph content lives in `para_buf` (viewed line texts joined by
    // `\n`), already de-indented and `\r`-free. Parse THAT (span None — the byte-window doesn't
    // map to the de-indented content, mirroring the old streaming_reparse's nulled spans). Keep
    // `para` in lockstep by clearing it too.
    if let Some(mut buf) = para_buf.take() {
        *para = None;
        if trim_eol {
            while buf.ends_with('\n') || buf.ends_with('\r') {
                buf.pop();
            }
        }
        // Base offset 0: the inline spans are relative to `buf` and get nulled by
        // none_out_blocks on the frame's pop (every null_spans frame runs it), so they never
        // reach the output.
        out.push(Block::Paragraph { inline: org_inline(&buf, 0), span: None });
        return;
    }
    if let Some((s, mut e)) = para.take() {
        if trim_eol {
            while e > s && matches!(input.as_bytes()[e - 1], b'\n' | b'\r') {
                e -= 1;
            }
        }
        out.push(Block::Paragraph {
            inline: org_inline(&input[s..e], s),
            span: Some(Span(s, e)),
        });
    }
}

// ---- directive ------------------------------------------------------------

/// `#+KEY: value` where KEY is non-empty and not `BEGIN_…`. Returns (key, value).
/// Leading whitespace is allowed (mldoc: `  #+KEY: v` is a directive). The value is
/// **left-trimmed only** — mldoc keeps trailing whitespace (`#+TITLE: x  ` → `x  `).
/// `pub(crate)` so the markdown driver (`parse.rs`) mirrors this EXACT classifier for a
/// standalone `#+name: value` directive line — mldoc parses it identically in both formats.
pub(crate) fn directive(s: &str) -> Option<(String, String)> {
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
    i: usize,
    hi: usize,
    body_end: usize,
    end_trie: &EndTrie,
    fence_lines: &[usize],
    fence_cursor: &mut usize,
) -> bool {
    if directive(content).is_some()
        || is_verbatim_line(content)
        || is_table_row(content)
        // CLAMP the `\end{}` scan to THIS body (streaming): an env that closes outside the
        // frame is not a split. At the top level `body_end == input.len()` (no-op there).
        || crate::inline::parse_latex_env(&input[..body_end], content_off, content_off + content.len()).is_some()
        || quote_opens(content)
        || displayed_math(content).is_some()
        || is_raw_html(content)
        || footnote_def(content).is_some()
        || is_org_hr(content)
    {
        return true;
    }
    // A `#+BEGIN_X` block / ```|~~~ fence only splits when it CLOSES INSIDE this body
    // (`< hi`) — the block-name close is an O(1) `end_by_prefix` lookup, the fence test the
    // monotone-cursor finder. At the top level `hi == lines.len()` (always true there).
    if let Some(name) = block_begin(content) {
        return end_trie.find(&name, i).is_some_and(|c| c < hi);
    }
    if fence_marker(content).is_some() {
        return find_matching_fence(fence_lines, fence_cursor, i).is_some_and(|c| c < hi);
    }
    false
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
    let Some(Inline::Plain { text, .. }) = title.last() else {
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
        // reformatted (trimmed) text → no source span.
        title.push(Inline::Plain { text: prefix, span: None });
    }
    // last_plain: if the (new) last inline is Plain, rtrim it and add one trailing space.
    if let Some(Inline::Plain { text, span }) = title.last_mut() {
        let trimmed = text.trim_end();
        *text = format!("{} ", trimmed);
        *span = None; // text reformatted → span no longer matches source
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
        // mldoc's block name = `take_while1(non-space)` immediately after `#+BEGIN_`; an empty
        // name (`#+BEGIN_` / `#+BEGIN_ X`) is NOT a block (a plain paragraph). C: don't skip ws.
        let rest = &t[8..];
        let n = rest.bytes().take_while(|&b| b != b' ' && b != b'\t').count();
        (n > 0).then(|| rest[..n].to_string())
    } else {
        None
    }
}

/// Language token from a `#+BEGIN_SRC <lang> …` line (first whitespace word).
/// `pub(crate)`: the md driver (`crate::parse`) reuses it for markdown `#+BEGIN_SRC`
/// (mldoc parses the SRC lang identically in both formats — see fix B).
pub(crate) fn begin_lang(s: &str) -> String {
    let t = s.trim_start();
    t[8..].split_whitespace().nth(1).unwrap_or("").to_string()
}

/// The raw-body builder for a `#+BEGIN_SRC`/`#+BEGIN_EXAMPLE` block, over the body lines'
/// text (the common indent — the first line's leading ws — cleared from each, joined with
/// one `\n` per line plus a trailing `\n`; mldoc `block0.ml` "clear indents"). `pub(crate)`:
/// the md driver reuses this VERBATIM so markdown `#+BEGIN_SRC`/`EXAMPLE` mirror org exactly
/// (fix B — mldoc's markdown block parser is the same `block_content` grammar).
pub(crate) fn block_code_texts(texts: &[&str]) -> String {
    if texts.is_empty() {
        return String::new();
    }
    let indent = leading_ws(texts[0]);
    let mut out = String::new();
    for &t in texts {
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
    // mldoc's fence marker is EXACTLY 3 chars; extra run chars + the rest of the line are the
    // info/lang (so `~~~~` → lang "~"). Info begins at `ws + 3`, not past the whole run.
    if k - ws >= 3 { Some((c, ws + 3)) } else { None }
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
    quote_first_line_slice(s).is_some()
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

/// First line of an Org blockquote — the de-`>`'d opener content as a SUFFIX slice of `s` (no
/// allocation). mldoc enters the quote by stripping one leading `>` (+ws); the remainder is itself
/// a body line that drops one MORE `>` (+ws) — i.e. up to TWO `>` on the opener (so N leading `>`
/// on ONE line ultimately nest ⌈N/2⌉ Quotes). The quote OPENS only if the result is non-empty and
/// does not start a block construct (a list/heading/`id::` marker makes mldoc reject the quote
/// entirely, leaving the raw line a Paragraph). This slice is the P3 `>`-frame's `opener_content`.
fn quote_first_line_slice(s: &str) -> Option<&str> {
    let r1 = s.trim_start().strip_prefix('>')?.trim_start();
    let content = match r1.strip_prefix('>') {
        Some(r2) => r2.trim_start(),
        None => r1,
    };
    if content.is_empty() || quote_line_breaker(content) {
        return None;
    }
    Some(content)
}

/// One CONTINUATION line of an Org blockquote body — the de-`>`'d content as a SUFFIX slice of `s`
/// (mldoc strips ONE `>` + ws, lazy: a non-`>` line still continues). The `>`-blank case is the
/// empty slice `Some("")`; a non-`>` blank / a de-`>`'d breaker is `None` (STOP the run — the P3
/// `>`-frame closes). Composed by `gt_cont_view` to view a `>`-frame's continuation lines.
fn quote_line_content_slice(s: &str) -> Option<&str> {
    let t = s.trim_start();
    let had_gt = t.starts_with('>');
    let rest = if had_gt { t[1..].trim_start() } else { t };
    if rest.is_empty() {
        return if had_gt { Some("") } else { None };
    }
    if quote_line_breaker(rest) {
        return None;
    }
    Some(rest)
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
/// re-parsed with the list-item content parser via `streaming_reparse` + `in_item:true`); deeper
/// is-item lines become children via the flat sequence + `nest_items`.
///
/// COLLAPSE: an indented continuation that is a list-item shape (`check_listitem`)
/// deeper than the current item but NOT a parseable marker there (`list_marker` None —
/// an indented `- `, a `N`-no-`.`, or an empty marker) makes the item's child
/// `list_parser` fail. In mldoc that failure bubbles up the recursion through every
/// item that is *first at its level*, terminating at (and keeping) the first ancestor
/// level that has a prior sibling; the failing item onward re-parses as a Paragraph.
/// `collapse_resume` reproduces that bubble from the flat indent sequence.
///
/// `hi` bounds every line scan to THIS body (streaming: a list inside a callout window
/// must not absorb the `#+END_…` closer — an INDENTED closer is `is_item`-false and would
/// otherwise fold as content); at the top level `hi == lines.len()` (no-op there).
/// `item_ctx` + `streaming_reparse` re-parse each item's content via the streaming driver (`in_item`).
fn collect_list(
    lines: &[Line],
    start: usize,
    hi: usize,
    item_ctx: Ctx,
    strip: usize,
) -> Result<(Block, usize), Collapse> {
    let mut flat: Vec<ListItem> = Vec::new();
    let mut flat_lines: Vec<usize> = Vec::new();
    let mut flat_indents: Vec<u32> = Vec::new();
    let mut i = start;
    while i < hi {
        let t = line_text(lines, i, strip);
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
            if j >= hi {
                break; // EOF / body boundary ends this item's content
            }
            let cl = line_text(lines, j, strip);
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
            content: streaming_reparse(&content_lines.join("\n"), item_ctx),
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

fn build_table(rows: &[Line], start: usize, end: usize, input: &str) -> Block {
    let split_cells = |s: &str| -> Vec<Vec<Inline>> {
        let t = s.trim();
        let t = t.strip_prefix('|').unwrap_or(t);
        let t = t.strip_suffix('|').unwrap_or(t);
        t.split('|')
            .map(|c| {
                let c = c.trim();
                org_inline(c, crate::inline::ptr_base(c, input))
            })
            .collect()
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

    // Fix C: org tables emit NO `aligns`. Org's real column alignment is a `<l>/<c>/<r>`
    // cookie row (using `+` junctions), NOT a markdown `:--` separator, so reusing the
    // markdown separator parser here produced WRONG alignment. mldoc discards org alignment
    // entirely (no oracle truth), so `data-align` is markdown-only.
    Block::Table { header, rows: body, aligns: None, span: Some(Span(start, end)) }
}

// ===========================================================================
// Inline parsing
// ===========================================================================

/// Block-body inline seam: the v0.2 `org_resolver`. `base` = the absolute byte offset of
/// `text` in the block body. Name kept for the block call sites.
pub(crate) fn org_inline(text: &str, base: usize) -> Vec<Inline> {
    crate::org_resolver::parse_inline_org(text, base)
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

    /// Strip inline spans so structural `assert_eq!`s over inline vecs stay span-agnostic
    /// (span invariants are checked separately in lib.rs).
    fn ns(v: &[Inline]) -> Vec<Inline> {
        let mut v = v.to_vec();
        crate::parse::none_out_inlines(&mut v);
        v
    }

    fn ik(i: &Inline) -> String {
        match i {
            Inline::Plain { text, .. } => format!("plain({text})"),
            Inline::Code { text, .. } => format!("code({text})"),
            Inline::Verbatim { text, .. } => format!("verb({text})"),
            Inline::Emphasis { emph, .. } => format!("em({emph})"),
            Inline::Subscript { .. } => "sub".into(),
            Inline::Superscript { .. } => "sup".into(),
            Inline::Link { url, .. } => format!("link({})", uk(url)),
            Inline::Tag { children, .. } => format!("tag({})", txt(children)),
            Inline::Macro { name, args, .. } => format!("macro({name};{})", args.join("|")),
            Inline::NestedLink { content, .. } => format!("nested({content})"),
            Inline::Target { text, .. } => format!("target({text})"),
            Inline::Break { .. } => "break".into(),
            Inline::HardBreak { .. } => "hardbreak".into(),
            Inline::Latex { mode, body, .. } => format!("latex({mode}:{body})"),
            Inline::Fnref { name, .. } => format!("fn({name})"),
            Inline::Timestamp { ts, .. } => format!("ts({ts})"),
            Inline::InlineHtml { text, .. } => format!("html({text})"),
            Inline::Email { .. } => "email".into(),
            Inline::Entity { unicode, .. } => format!("entity({unicode})"),
            Inline::Hiccup { v, .. } => format!("hiccup({v})"),
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
                Inline::Plain { text, .. } => text.clone(),
                Inline::Link { full, .. } => full.clone(),
                Inline::NestedLink { content, .. } => content.clone(),
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
                assert_eq!(ns(inline), vec![Inline::Plain { text: "task with ".into(), span: None }]);
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
                assert_eq!(ns(inline), vec![Inline::Plain { text: "plain ".into(), span: None }]);
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
                ns(inline),
                vec![
                    Inline::Plain { text: "a plain paragraph".into(), span: None },
                    Inline::Break { span: None },
                    Inline::Plain { text: "second line".into(), span: None },
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
                Inline::Plain { text, .. } => Some(text.clone()),
                Inline::Break { .. } => Some("⏎".into()),
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
                        Inline::Plain { text, .. } => text.clone(),
                        Inline::Break { .. } => "\u{23ce}".into(),
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
                ns(inline),
                vec![
                    Inline::Plain { text: " ".into(), span: None },
                    Inline::Break { span: None },
                    Inline::Plain { text: "real content".into(), span: None },
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

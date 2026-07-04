//! Complexity gate — the structural guard the byte-exact parity gate cannot be.
//!
//! `src/metrics.rs` counts "scan work": parser-owned byte scans plus index builds, cache lookups,
//! cursor advances, search probes, and tree visits. A single-pass parser keeps that total
//! O(input length). This gate parses adversarial families at n / 2n / 4n and asserts the count
//! grows ~linearly (ratio < 1.6×). Because the count is **deterministic** (not timed), small inputs
//! give a clean signal and there is no machine-noise flakiness — the weakness that let O(n²)
//! families hide behind byte-exact parity.
//!
//! Debug-only (the counter compiles out in release): run with `cargo test --test complexity`.
#![cfg(debug_assertions)]

use std::fmt::Write;

/// Scan-work for one parse. The result is `forget`-ted: an adversarial family builds a deep AST
/// whose recursive DROP would overflow — we measure only the (iterative, bounded-stack) parse.
fn work(input: &str, fmt: &str) -> u64 {
    lsdoc::__scan_work_take(); // reset
    std::mem::forget(lsdoc::parse(input, fmt));
    lsdoc::__scan_work_take()
}

fn assert_linear(label: &str, f: impl Fn(usize) -> String, base: usize, fmt: &str) {
    // Normalize by INPUT LENGTH — some families (e.g. the `>`-staircase) have O(depth²) bytes, so
    // scan-work must be judged PER BYTE, not per `base`. A single-pass parser examines each byte
    // O(1) times ⇒ scan-work/byte is ~constant across sizes; an O(n²) re-scan makes it grow ∝ n.
    // The count is deterministic, so linear ⇒ growth ≈1×, O(n²) ⇒ ≈2× per size step; 1.6 separates.
    const CAP: f64 = 1.6;
    let q = |n: usize| -> f64 {
        let s = f(n);
        work(&s, fmt).max(1) as f64 / s.len().max(1) as f64
    };
    let (q1, q2, q4) = (q(base), q(2 * base), q(4 * base));
    let (r1, r2) = (q2 / q1, q4 / q2);
    assert!(
        r1 < CAP && r2 < CAP,
        "{label} [{fmt}]: scan-work/byte {q1:.3} → {q2:.3} → {q4:.3} (base={base}), growth \
         {r1:.2}×/{r2:.2}× — >{CAP}× means a super-linear re-scan (single-pass invariant violated)"
    );
}

// ---- adversarial families -------------------------------------------------

/// Single-line collapsed `>`-nest (`>`×n + x). O(n) via a single prefix consume; O(n²) if the
/// line is re-dispatched/re-scanned per opened frame. (Bug 1a — MD only, `property` re-scan.)
fn gt_spine(n: usize) -> String {
    format!("{}x", ">".repeat(n))
}
/// A deep opener line then an interior breaker-dedent that closes many frames at once. O(n²) if
/// the close loop re-peels the breaker's `>`-prefix per popped frame. (Bug 1b — both formats.)
fn gt_breaker(d: usize) -> String {
    format!("{}y\n{}- x\n", ">".repeat(2 * d), ">".repeat(d / 2))
}
/// Many unclosed hiccup heads + one far `]`. O(n²) if each `[:` line scans to EOF (weak
/// `last_rbracket` floor). (Bug 2a — both formats.)
fn hiccup_unclosed(m: usize) -> String {
    let mut s = String::new();
    for _ in 0..m {
        s.push_str("[:div [:\n");
    }
    s.push(']');
    s
}
/// Tag straddling into a `\` escape, ×n. O(n²) + O(n) native stack if `resync` recurses over the
/// whole remaining suffix per unit. FIXED (C): the fast path reuses the outer tokens (the split
/// escape's tail re-lexes to a single Punct/Text token — no non-local backtick pairing), so it
/// re-dispatches in the loop → O(n), O(1) native stack. (Bug 2b — inline resolver.)
fn resync(n: usize) -> String {
    "#a\\".repeat(n)
}
/// Tag straddling into a `` `code` `` LEAF, ×n. FIXED (D): the lexer no longer pre-builds code spans
/// as multi-byte `Leaf`s — a backtick is a one-byte `Punct` and the resolver recognizes code spans
/// LAZILY at dispatch. So a tag consuming a backtick lands on a clean 1-byte boundary (no straddle,
/// no re-lex) and the consumed backtick is simply never dispatched as a code opener (mldoc's
/// behavior). O(n). See subagent-tasks/notes/lsdoc-inline-delimstack-design.md. (Bug 2b, residual.)
fn resync_leaf(n: usize) -> String {
    "#a`#`".repeat(n)
}
/// Flat multi-line quote where one de-`>` fold buffer has O(n) origin segments and O(n) inline
/// nodes. The remap pass must walk the origin segments monotonically, not from segment zero per
/// inline node.
fn flat_gt_quote_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(&mut s, "> line {i}").unwrap();
    }
    s
}
/// Flat `>` fallback whose reparse emits many table cell remaps against one parent OriginMap.
fn gt_flat_table_rows(n: usize) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str("> | a | b |\n");
    }
    s
}
/// Markdown definition-list fallback: direct term remaps plus per-item child map construction.
fn gt_flat_def_list(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(&mut s, "> Term{i}").unwrap();
        s.push_str("> : value\n");
    }
    s
}
/// Flat `>` fallback containing adjacent LaTeX environments.
fn gt_flat_latex_envs(n: usize) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str("> \\begin{equation}\n> x\n> \\end{equation}\n");
    }
    s
}
/// Org `#+BEGIN_QUOTE` with an indented body: the strip-view paragraph buffer is another
/// transformed fold site that remaps O(n) inline nodes through O(n) origin segments.
fn org_begin_quote_indented_body(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for i in 0..n {
        writeln!(&mut s, "  line {i}").unwrap();
    }
    s.push_str("#+END_QUOTE\n");
    s
}
/// Precise transformed-frame cell: an indented Org quote body containing table rows.
fn org_indented_quote_table_rows(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for _ in 0..n {
        s.push_str("  | a | b |\n");
    }
    s.push_str("#+END_QUOTE\n");
    s
}
/// Precise transformed-frame cell: a markdown re-bulleted callout containing a def-list.
fn md_rebulleted_def_list(n: usize) -> String {
    let mut s = String::from("- #+BEGIN_NOTE\n");
    for i in 0..n {
        writeln!(&mut s, "  Term{i}").unwrap();
        s.push_str("  : value\n");
    }
    s.push_str("  #+END_NOTE\n");
    s
}

fn d35_open_chain(increments: &[usize], body: impl FnOnce(&mut String, usize)) -> String {
    let mut s = String::new();
    if increments.is_empty() {
        body(&mut s, 0);
        return s;
    }
    s.push_str("#+BEGIN_D35_0\n");
    let mut cum = 0usize;
    for i in 1..increments.len() {
        cum += increments[i - 1];
        writeln!(&mut s, "{}#+BEGIN_D35_{i}", " ".repeat(cum)).unwrap();
    }
    cum += increments[increments.len() - 1];
    writeln!(&mut s, "{}seed", " ".repeat(cum)).unwrap();
    body(&mut s, cum);
    for i in (0..increments.len()).rev() {
        writeln!(&mut s, "#+END_D35_{i}").unwrap();
    }
    s
}

/// D35 adversary (i): parent strips 2..d+1, then many strip-1 sibling frames. Pop must reset
/// leaves, and later sibling pushes must not see stale entries.
fn d35_rollback_siblings(d: usize) -> String {
    let increments: Vec<usize> = (2..=d + 1).collect();
    d35_open_chain(&increments, |s, parent_cum| {
        for q in 0..d * d {
            writeln!(&mut *s, "{}#+BEGIN_D35_S{q}", " ".repeat(parent_cum)).unwrap();
            writeln!(&mut *s, "{}x", " ".repeat(parent_cum + 1)).unwrap();
            s.push_str("  \n");
            writeln!(&mut *s, "{}#+END_D35_S{q}", " ".repeat(parent_cum)).unwrap();
        }
    })
}

/// D35 adversary (ii): strips d..1 and many length-2 all-ws lines. A naive walk crosses d-1
/// no-op stages per line before the final strip-1 subtraction.
fn d35_query_noop_chain(d: usize) -> String {
    let increments: Vec<usize> = (1..=d).rev().collect();
    d35_open_chain(&increments, |s, _| {
        for _ in 0..d * d {
            s.push_str("  \n");
        }
    })
}

/// D35 adversary (iii): many all-ws lines under deep positive nesting.
fn d35_many_ws_deep(d: usize) -> String {
    let increments = vec![1usize; d];
    d35_open_chain(&increments, |s, _| {
        for _ in 0..d * d {
            s.push_str("  \n");
        }
    })
}

// ---- linear controls (must stay linear) -----------------------------------

fn plain(n: usize) -> String {
    "word ".repeat(n)
}
/// Multi-line `>`-staircase (one level per line) — the container work IS single-pass here.
fn staircase(d: usize) -> String {
    let mut s = String::new();
    for k in 1..=d {
        for _ in 0..k {
            s.push_str("> ");
        }
        s.push_str("x\n");
    }
    s
}
/// Alternating emphasis delimiters — the historical inline O(n²) that the `no_closer` floor fixed.
fn emph_alt(n: usize) -> String {
    format!("{}x{}", "*_".repeat(n), "_*".repeat(n))
}
/// Adjacent display-math blocks on one physical line. O(n²) if each remainder re-runs
/// the whole block ladder on a shrinking suffix instead of being consumed locally.
fn display_math_adjacent(n: usize) -> String {
    "$$x$$".repeat(n)
}
/// One block-level `$$` opener with no close. O(n²) if failed openers rescan EOF per line.
fn display_math_unclosed_tail(n: usize) -> String {
    format!("$$x\n{}", "tail\n".repeat(n))
}
/// Adjacent closed raw-HTML blocks on one physical line. O(n²) if each consumed block
/// re-runs the block ladder over the whole remaining suffix.
fn raw_html_adjacent(n: usize) -> String {
    "<kbd>x</kbd>".repeat(n)
}
/// One known-tag raw-HTML opener with no close. O(n²) if the failed opener rescans EOF
/// from each following line instead of failing once.
fn raw_html_unclosed_tail(n: usize) -> String {
    format!("<kbd>x\n{}", "tail\n".repeat(n))
}
/// Many unclosed known-tag raw-HTML openers. O(n²) without the block-phase no-close memo.
fn raw_html_repeated_unclosed(n: usize) -> String {
    "<kbd>\n".repeat(n)
}
/// Org `#+BEGIN_QUOTE` with indented adjacent raw-HTML siblings. This is the transformed-view
/// twin of `raw_html_adjacent`: O(n²) if each sibling materializes the remaining strip-view suffix.
fn org_indented_quote_raw_html_adjacent(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for _ in 0..n {
        s.push_str("  <kbd>x</kbd>\n");
    }
    s.push_str("#+END_QUOTE\n");
    s
}
/// Markdown re-bulleted callout body with indented adjacent raw-HTML siblings. It reaches the same
/// strip-view raw-HTML capture path through the markdown driver.
fn md_rebulleted_raw_html_adjacent(n: usize) -> String {
    let mut s = String::from("- #+BEGIN_NOTE\n");
    for _ in 0..n {
        s.push_str("  <kbd>x</kbd>\n");
    }
    s.push_str("  #+END_NOTE\n");
    s
}
/// Rejected transformed-view raw-HTML openers. Missing closers must be floored through the shared
/// raw-coordinate absence cache instead of re-scanning the remaining body for every line.
fn org_indented_quote_raw_html_rejected(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for _ in 0..n {
        s.push_str("  <kbd>x\n");
    }
    s.push_str("#+END_QUOTE\n");
    s
}
fn base36(mut n: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[n % 36]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}
fn nested_callout_raw_html(k: usize) -> String {
    let width = base36(k + 1).len();
    let mut s = String::new();
    for d in 0..k {
        let name = format!("{:0>width$}", base36(d), width = width);
        writeln!(&mut s, "#+BEGIN_A{name}").unwrap();
        s.push_str("<div>x</div>\n");
    }
    for d in (0..k).rev() {
        let name = format!("{:0>width$}", base36(d), width = width);
        writeln!(&mut s, "#+END_A{name}").unwrap();
    }
    s
}
fn nested_reuse_after_child_raw_html(k: usize) -> String {
    let width = base36(k + 1).len();
    let mut s = String::new();
    for d in 0..k {
        let name = format!("{:0>width$}", base36(d), width = width);
        writeln!(&mut s, "#+BEGIN_A{name}").unwrap();
        s.push_str("<div>before</div>\n");
    }
    for d in (0..k).rev() {
        s.push_str("<div>after</div>\n");
        let name = format!("{:0>width$}", base36(d), width = width);
        writeln!(&mut s, "#+END_A{name}").unwrap();
    }
    s
}
fn raw_html_sibling_alternation(k: usize) -> String {
    let width = base36(k + 1).len();
    let mut s = String::new();
    for d in 0..k {
        s.push_str("<div>p</div>\n");
        let name = format!("{:0>width$}", base36(d), width = width);
        writeln!(&mut s, "#+BEGIN_A{name}").unwrap();
        s.push_str("<div>c</div>\n");
        writeln!(&mut s, "#+END_A{name}").unwrap();
    }
    s.push_str("<div>p</div>\n");
    s
}
fn raw_html_sibling_pairs(n: usize) -> String {
    let mut s = String::from("<div>");
    for _ in 0..n {
        s.push_str("<div></div>");
    }
    s.push_str("</div>");
    s
}
fn raw_html_closes_then_opens(n: usize) -> String {
    format!("<div>{}{}</div>", "</div>".repeat(n), "<div>".repeat(n))
}
fn raw_html_unbalanced_retry_interleave(n: usize) -> String {
    "*a*<div><div>x</div>\n".repeat(n)
}
fn raw_html_org_unbalanced_retry_interleave(n: usize) -> String {
    "/a/<div><div>x</div>\n".repeat(n)
}
/// Bare `<` run: control for raw-HTML tokenizer batching when no inline construct resets `fresh`.
fn raw_html_lt_bare(n: usize) -> String {
    "<".repeat(n)
}
/// Emphasis + `<` interleave: exercises raw-HTML tokenizer dispatch after `fresh` reset.
fn raw_html_emph_lt(n: usize) -> String {
    "*a*<".repeat(n / 4)
}
/// Page-ref + `<` interleave: exercises raw-HTML tokenizer dispatch after bracket construct reset.
fn raw_html_pageref_lt(n: usize) -> String {
    "[[a]]<".repeat(n / 6)
}
/// Org emphasis + `<` twin for the same construct-interleaved raw-HTML tokenizer shape.
fn raw_html_org_emph_lt(n: usize) -> String {
    "/a/<".repeat(n / 4)
}
fn email_domain_interleave(n: usize) -> String {
    "*a*<x@".repeat(n / 6)
}
fn org_email_domain_interleave(n: usize) -> String {
    "/a/<x@".repeat(n / 6)
}
fn timestamp_angle_interleave(n: usize) -> String {
    "*a*<20".repeat(n / 6)
}
fn org_timestamp_angle_interleave(n: usize) -> String {
    "/a/<20".repeat(n / 6)
}
fn autolink_interleave(n: usize) -> String {
    "*a*<a:".repeat(n / 6)
}
fn macro_interleave(n: usize) -> String {
    "*a*{{".repeat(n / 5)
}
fn org_macro_interleave(n: usize) -> String {
    "/a/{{".repeat(n / 5)
}
fn export_snippet_interleave(n: usize) -> String {
    "*a*@@a: b\n".repeat(n / 10)
}
fn org_export_snippet_interleave(n: usize) -> String {
    "/a/@@a: b\n".repeat(n / 10)
}
fn blockref_interleave(n: usize) -> String {
    "*a*((".repeat(n / 5)
}
fn org_blockref_interleave(n: usize) -> String {
    "/a/((".repeat(n / 5)
}
fn raw_html_unbalanced_interleave(n: usize) -> String {
    "*a*<div><div>x</div>".repeat(n / 20)
}
fn tag_hash_run(n: usize) -> String {
    "#".repeat(n)
}
fn tag_word_interleave(n: usize) -> String {
    "x #a".repeat(n / 4)
}
fn bare_url_interleave(n: usize) -> String {
    "*a*httpx".repeat(n / 8)
}
fn latex_dollar_failure_interleave(n: usize) -> String {
    "*a*$$x$z ".repeat(n / 9)
}
fn org_latex_dollar_failure_interleave(n: usize) -> String {
    "/a/$$x$z ".repeat(n / 9)
}
fn md_link_many_openers_one_close(n: usize) -> String {
    format!("{}](u)", "[".repeat(n))
}
fn md_link_code_interleave(n: usize) -> String {
    format!("{}x`](u)", "[`".repeat(n))
}
fn md_link_counter_nested_bracket(n: usize) -> String {
    format!("{}](u)", "[[".repeat(n))
}
fn md_link_counter_escaped_bracket(n: usize) -> String {
    format!("{}](u)", "[[\\[".repeat(n))
}
fn md_link_counter_page_ref(n: usize) -> String {
    format!("{}](u)", "[[[p]]".repeat(n))
}
fn md_link_counter_code_interleave(n: usize) -> String {
    format!("{}[`x`](u)", "[`".repeat(n))
}
fn org_many_openers_one_close(n: usize) -> String {
    format!("{}](u)", "[".repeat(n))
}
fn md_html_comment_unclosed(n: usize) -> String {
    "<!--\n".repeat(n)
}
fn md_html_comment_quote(n: usize) -> String {
    "> <!--\n".repeat(n)
}
fn md_html_comment_indented_begin(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for _ in 0..n {
        s.push_str("  <!--\n");
    }
    s.push_str("#+END_QUOTE\n");
    s
}
fn md_html_comment_list_item(n: usize) -> String {
    let mut s = String::from("- seed\n");
    for _ in 0..n {
        s.push_str("  <!--\n");
    }
    s
}
fn org_fn_anon_newline_tail(n: usize) -> String {
    "/a/[fn::x\n".repeat(n) + "]"
}
fn org_fn_named_newline_tail(n: usize) -> String {
    "/a/[fn:a:x\n".repeat(n) + "]"
}
fn org_fn_anon_no_close_line(n: usize) -> String {
    "/a/[fn::x".repeat(n)
}
fn org_fn_named_no_close_line(n: usize) -> String {
    "/a/[fn:n:x".repeat(n)
}
fn org_link1_missing_label_close(n: usize) -> String {
    "/a/[[a".repeat(n) + "][x"
}
fn org_link1_overlapping_chunks(n: usize) -> String {
    "/a/[[u][".repeat(n)
}
fn org_link1_balanced_tail_present(n: usize) -> String {
    "/a/[[u][[a".repeat(n) + "]]"
}
fn md_link_metadata_missing_close(n: usize) -> String {
    "*a*[a](u){".repeat(n)
}
fn md_embed_data_metadata_missing_close(n: usize) -> String {
    "*a*![a](data:x){".repeat(n)
}
fn org_link_metadata_missing_close(n: usize) -> String {
    "/a/[[u][l]]{".repeat(n)
}
fn md_tag_link_metadata_missing_close(n: usize) -> String {
    format!("#t{}", "[a](u){".repeat(n))
}
fn org_tag_link_metadata_missing_close(n: usize) -> String {
    format!("#t{}", "[[u][l]]{".repeat(n))
}
fn f5_tag_pageref_simple(n: usize) -> String {
    format!("#t{}", "[[ul]]".repeat(n))
}
fn f5_org_tag_pageref_link1_shape(n: usize) -> String {
    format!("#t{}", "[[u][l]]".repeat(n))
}
fn f5_tag_pageref_fail_lf(n: usize) -> String {
    format!("#t{}\n]]", "[[a".repeat(n))
}
fn f5_tag_pageref_cross_call(n: usize) -> String {
    "#t[[a ".repeat(n)
}
fn f5_org_tag_pageref_bs_lf_hop(n: usize) -> String {
    format!("#t{}\\\n]]x", "[[a".repeat(n))
}
fn f5_control_separate_tags(n: usize) -> String {
    "#t[[ul]] ".repeat(n)
}
fn f5_control_single_brackets(n: usize) -> String {
    format!("#t{}", "[u]".repeat(n))
}
fn f5_control_plain_tag(n: usize) -> String {
    format!("#t{}", "a".repeat(n))
}
fn audit3_md_footnote_no_close(n: usize) -> String {
    "[^a".repeat(n)
}
fn audit3_md_footnote_success(n: usize) -> String {
    "[^a] ".repeat(n)
}
fn audit3_org_target_no_close(n: usize) -> String {
    "<<a ".repeat(n)
}
fn audit3_org_radio_no_close(n: usize) -> String {
    "<<<a ".repeat(n)
}
fn audit3_org_target_success(n: usize) -> String {
    "<<a>> ".repeat(n)
}
fn audit3_macro_blockref_arg_no_close(n: usize) -> String {
    format!("{{{{m {}z}}}}", "((a,".repeat(n))
}
fn audit3_macro_blockref_arg_success(n: usize) -> String {
    format!("{{{{m {}z}}}}", "((a)),".repeat(n))
}
fn f5_w_tag_reparse(n: usize) -> String {
    format!("#t{}", "[[a".repeat(n))
}
fn f5_w_md_emphasis_reparse(n: usize) -> String {
    format!("*{}*", "[[a".repeat(n))
}
fn f5_w_md_url_piece(n: usize) -> String {
    format!("[x]({})", "[[a".repeat(n))
}
fn f5_w_macro_args(n: usize) -> String {
    format!("{{{{m {}z}}}}", "[[a,".repeat(n))
}
fn f5_control_md_deep_imbalance(n: usize) -> String {
    format!("*{}{}*", "[[".repeat(n), "x]]".repeat(n / 2))
}
fn f5_control_org_deep_imbalance(n: usize) -> String {
    format!("#t{}{}", "[[".repeat(n), "x]]".repeat(n / 2))
}
fn f5_control_md_balanced_nested(n: usize) -> String {
    format!("*{}{}*", "[[".repeat(n), "x]]".repeat(n))
}

fn big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// The green gate: families that are single-pass today stay single-pass. Runs by default.
#[test]
fn complexity_gate() {
    big_stack(|| {
        assert_linear("plain", plain, 5000, "md");
        assert_linear("plain", plain, 5000, "org");
        assert_linear("staircase", staircase, 700, "md");
        assert_linear("staircase", staircase, 700, "org");
        assert_linear("emph_alt", emph_alt, 3000, "md");
        assert_linear("emph_alt", emph_alt, 3000, "org");
        assert_linear("display_math_adjacent", display_math_adjacent, 3000, "md");
        assert_linear("display_math_adjacent", display_math_adjacent, 3000, "org");
        assert_linear("display_math_unclosed_tail", display_math_unclosed_tail, 3000, "md");
        assert_linear("display_math_unclosed_tail", display_math_unclosed_tail, 3000, "org");
        assert_linear("raw_html_adjacent", raw_html_adjacent, 3000, "md");
        assert_linear("raw_html_adjacent", raw_html_adjacent, 3000, "org");
        assert_linear("raw_html_unclosed_tail", raw_html_unclosed_tail, 3000, "md");
        assert_linear("raw_html_unclosed_tail", raw_html_unclosed_tail, 3000, "org");
        assert_linear("raw_html_repeated_unclosed", raw_html_repeated_unclosed, 3000, "md");
        assert_linear("raw_html_repeated_unclosed", raw_html_repeated_unclosed, 3000, "org");
        assert_linear(
            "org_indented_quote_raw_html_adjacent",
            org_indented_quote_raw_html_adjacent,
            1000,
            "org",
        );
        assert_linear(
            "md_rebulleted_raw_html_adjacent",
            md_rebulleted_raw_html_adjacent,
            1000,
            "md",
        );
        assert_linear(
            "org_indented_quote_raw_html_rejected",
            org_indented_quote_raw_html_rejected,
            1000,
            "org",
        );
        assert_linear("nested_callout_raw_html", nested_callout_raw_html, 50, "md");
        assert_linear("nested_callout_raw_html", nested_callout_raw_html, 50, "org");
        assert_linear("nested_reuse_after_child_raw_html", nested_reuse_after_child_raw_html, 50, "md");
        assert_linear("nested_reuse_after_child_raw_html", nested_reuse_after_child_raw_html, 50, "org");
        assert_linear("raw_html_sibling_alternation", raw_html_sibling_alternation, 50, "md");
        assert_linear("raw_html_sibling_alternation", raw_html_sibling_alternation, 50, "org");
        assert_linear("raw_html_sibling_pairs", raw_html_sibling_pairs, 500, "md");
        assert_linear("raw_html_sibling_pairs", raw_html_sibling_pairs, 500, "org");
        assert_linear("raw_html_closes_then_opens", raw_html_closes_then_opens, 500, "md");
        assert_linear("raw_html_closes_then_opens", raw_html_closes_then_opens, 500, "org");
        assert_linear(
            "raw_html_unbalanced_retry_interleave",
            raw_html_unbalanced_retry_interleave,
            500,
            "md",
        );
        assert_linear(
            "raw_html_org_unbalanced_retry_interleave",
            raw_html_org_unbalanced_retry_interleave,
            500,
            "org",
        );
        assert_linear("raw_html_lt_bare", raw_html_lt_bare, 6000, "md");
        assert_linear("raw_html_lt_bare", raw_html_lt_bare, 6000, "org");
        assert_linear("raw_html_emph_lt", raw_html_emph_lt, 6000, "md");
        assert_linear("raw_html_pageref_lt", raw_html_pageref_lt, 6000, "md");
        assert_linear("raw_html_org_emph_lt", raw_html_org_emph_lt, 6000, "org");
        assert_linear("raw_html_org_pageref_lt", raw_html_pageref_lt, 6000, "org");
        assert_linear("email_domain_interleave", email_domain_interleave, 6000, "md");
        assert_linear("org_email_domain_interleave", org_email_domain_interleave, 6000, "org");
        assert_linear("timestamp_angle_interleave", timestamp_angle_interleave, 6000, "md");
        assert_linear("org_timestamp_angle_interleave", org_timestamp_angle_interleave, 6000, "org");
        assert_linear("autolink_interleave", autolink_interleave, 6000, "md");
        assert_linear("macro_interleave", macro_interleave, 6000, "md");
        assert_linear("org_macro_interleave", org_macro_interleave, 6000, "org");
        assert_linear("export_snippet_interleave", export_snippet_interleave, 6000, "md");
        assert_linear("org_export_snippet_interleave", org_export_snippet_interleave, 6000, "org");
        assert_linear("blockref_interleave", blockref_interleave, 6000, "md");
        assert_linear("org_blockref_interleave", org_blockref_interleave, 6000, "org");
        assert_linear("raw_html_unbalanced_interleave", raw_html_unbalanced_interleave, 6000, "md");
        assert_linear("tag_hash_run", tag_hash_run, 6000, "md");
        assert_linear("tag_word_interleave", tag_word_interleave, 6000, "md");
        assert_linear("bare_url_interleave", bare_url_interleave, 6000, "md");
        assert_linear("latex_dollar_failure_interleave", latex_dollar_failure_interleave, 6000, "md");
        assert_linear("org_latex_dollar_failure_interleave", org_latex_dollar_failure_interleave, 6000, "org");
        assert_linear("md_link_many_openers_one_close", md_link_many_openers_one_close, 1000, "md");
        assert_linear("md_link_code_interleave", md_link_code_interleave, 1000, "md");
        assert_linear("md_link_counter_nested_bracket", md_link_counter_nested_bracket, 1000, "md");
        assert_linear("md_link_counter_escaped_bracket", md_link_counter_escaped_bracket, 1000, "md");
        assert_linear("md_link_counter_page_ref", md_link_counter_page_ref, 1000, "md");
        assert_linear("md_link_counter_code_interleave", md_link_counter_code_interleave, 1000, "md");
        assert_linear("org_many_openers_one_close", org_many_openers_one_close, 1000, "org");
        assert_linear("md_html_comment_unclosed", md_html_comment_unclosed, 1000, "md");
        assert_linear("md_html_comment_quote", md_html_comment_quote, 1000, "md");
        assert_linear(
            "md_html_comment_indented_begin",
            md_html_comment_indented_begin,
            1000,
            "md",
        );
        assert_linear("md_html_comment_list_item", md_html_comment_list_item, 1000, "md");
        assert_linear("org_fn_anon_newline_tail", org_fn_anon_newline_tail, 1000, "org");
        assert_linear("org_fn_named_newline_tail", org_fn_named_newline_tail, 1000, "org");
        assert_linear("org_fn_anon_no_close_line", org_fn_anon_no_close_line, 1000, "org");
        assert_linear("org_fn_named_no_close_line", org_fn_named_no_close_line, 1000, "org");
        assert_linear("org_link1_missing_label_close", org_link1_missing_label_close, 1000, "org");
        assert_linear("org_link1_overlapping_chunks", org_link1_overlapping_chunks, 1000, "org");
        assert_linear(
            "org_link1_balanced_tail_present",
            org_link1_balanced_tail_present,
            1000,
            "org",
        );
        assert_linear(
            "md_link_metadata_missing_close",
            md_link_metadata_missing_close,
            1000,
            "md",
        );
        assert_linear(
            "md_embed_data_metadata_missing_close",
            md_embed_data_metadata_missing_close,
            1000,
            "md",
        );
        assert_linear(
            "org_link_metadata_missing_close",
            org_link_metadata_missing_close,
            1000,
            "org",
        );
        assert_linear(
            "md_tag_link_metadata_missing_close",
            md_tag_link_metadata_missing_close,
            1000,
            "md",
        );
        assert_linear(
            "org_tag_link_metadata_missing_close",
            org_tag_link_metadata_missing_close,
            1000,
            "org",
        );
        assert_linear("f5_tag_pageref_simple", f5_tag_pageref_simple, 1000, "md");
        assert_linear("f5_tag_pageref_simple", f5_tag_pageref_simple, 1000, "org");
        assert_linear(
            "f5_org_tag_pageref_link1_shape",
            f5_org_tag_pageref_link1_shape,
            1000,
            "org",
        );
        assert_linear("f5_tag_pageref_fail_lf", f5_tag_pageref_fail_lf, 1000, "md");
        assert_linear("f5_tag_pageref_fail_lf", f5_tag_pageref_fail_lf, 1000, "org");
        assert_linear(
            "f5_tag_pageref_cross_call",
            f5_tag_pageref_cross_call,
            1000,
            "md",
        );
        assert_linear(
            "f5_tag_pageref_cross_call",
            f5_tag_pageref_cross_call,
            1000,
            "org",
        );
        assert_linear(
            "f5_org_tag_pageref_bs_lf_hop",
            f5_org_tag_pageref_bs_lf_hop,
            1000,
            "org",
        );
        assert_linear("f5_control_separate_tags", f5_control_separate_tags, 1000, "md");
        assert_linear("f5_control_separate_tags", f5_control_separate_tags, 1000, "org");
        assert_linear("f5_control_single_brackets", f5_control_single_brackets, 1000, "md");
        assert_linear("f5_control_single_brackets", f5_control_single_brackets, 1000, "org");
        assert_linear("f5_control_plain_tag", f5_control_plain_tag, 1000, "md");
        assert_linear("f5_control_plain_tag", f5_control_plain_tag, 1000, "org");
        assert_linear("audit3_md_footnote_no_close", audit3_md_footnote_no_close, 1000, "md");
        assert_linear("audit3_md_footnote_success", audit3_md_footnote_success, 1000, "md");
        assert_linear("audit3_org_target_no_close", audit3_org_target_no_close, 1000, "org");
        assert_linear("audit3_org_radio_no_close", audit3_org_radio_no_close, 1000, "org");
        assert_linear("audit3_org_target_success", audit3_org_target_success, 1000, "org");
        assert_linear(
            "audit3_macro_blockref_arg_no_close",
            audit3_macro_blockref_arg_no_close,
            1000,
            "md",
        );
        assert_linear(
            "audit3_macro_blockref_arg_no_close",
            audit3_macro_blockref_arg_no_close,
            1000,
            "org",
        );
        assert_linear(
            "audit3_macro_blockref_arg_success",
            audit3_macro_blockref_arg_success,
            1000,
            "md",
        );
        assert_linear(
            "audit3_macro_blockref_arg_success",
            audit3_macro_blockref_arg_success,
            1000,
            "org",
        );
        assert_linear("f5_w_tag_reparse", f5_w_tag_reparse, 1000, "md");
        assert_linear("f5_w_tag_reparse", f5_w_tag_reparse, 1000, "org");
        assert_linear("f5_w_md_emphasis_reparse", f5_w_md_emphasis_reparse, 1000, "md");
        assert_linear("f5_w_md_url_piece", f5_w_md_url_piece, 1000, "md");
        assert_linear("f5_w_macro_args", f5_w_macro_args, 1000, "md");
        assert_linear("f5_w_macro_args", f5_w_macro_args, 1000, "org");
        assert_linear(
            "f5_control_md_deep_imbalance",
            f5_control_md_deep_imbalance,
            1000,
            "md",
        );
        assert_linear(
            "f5_control_org_deep_imbalance",
            f5_control_org_deep_imbalance,
            1000,
            "org",
        );
        assert_linear(
            "f5_control_md_balanced_nested",
            f5_control_md_balanced_nested,
            1000,
            "md",
        );
        // A (container-prefix consume): both formats now dispatch a `>`-line's content ONCE at the
        // final depth (no per-re-dispatch `property` re-scan — 1a) and close many frames at one
        // interior breaker in O(closed) (no per-frame `gt_cont_view` re-peel — 1b).
        assert_linear("gt_spine", gt_spine, 3000, "org");
        assert_linear("gt_spine", gt_spine, 3000, "md"); // A-md (1a)
        assert_linear("gt_breaker", gt_breaker, 1500, "org"); // A-org (1b)
        assert_linear("gt_breaker", gt_breaker, 1500, "md"); // A-md (1b)
        // B (2a): the block-hiccup capture is a precomputed `[:`…`]`-balance array lookup +
        // `close <= body_end` clamp, not a per-opener `parse_hiccup` re-scan to `body_end`.
        assert_linear("hiccup_unclosed", hiccup_unclosed, 3000, "md");
        assert_linear("hiccup_unclosed", hiccup_unclosed, 3000, "org");
        // C (2b): the inline escape-straddle resync reuses the outer tokens (re-lexes only the
        // O(1) split boundary token, then re-dispatches in the loop) instead of recursing over the
        // whole remaining suffix → LINEAR.
        assert_linear("resync", resync, 1500, "md");
        // D (2b, code-leaf): code spans are recognized LAZILY at dispatch (one-byte backtick
        // `Punct`, no pre-built `Leaf`), so a tag consuming a backtick lands on a clean boundary —
        // the straddle cannot exist, no re-lex → LINEAR.
        assert_linear("resync_leaf", resync_leaf, 1500, "md");
        // Inline-spans v2 Round 2: transformed flat quote buffers have O(n) origin segments and
        // O(n) inline nodes. Source-map remap scans are counted by `scan_work`, so a segment-zero
        // re-scan per inline node fails this deterministic gate.
        assert_linear("flat_gt_quote_lines", flat_gt_quote_lines, 1000, "md");
        assert_linear("org_flat_gt_quote_lines", flat_gt_quote_lines, 1000, "org");
        assert_linear("gt_flat_table_rows", gt_flat_table_rows, 1000, "md");
        assert_linear("org_gt_flat_table_rows", gt_flat_table_rows, 1000, "org");
        assert_linear("gt_flat_def_list", gt_flat_def_list, 1000, "md");
        assert_linear("gt_flat_latex_envs", gt_flat_latex_envs, 700, "md");
        assert_linear("org_gt_flat_latex_envs", gt_flat_latex_envs, 700, "org");
        assert_linear(
            "org_begin_quote_indented_body",
            org_begin_quote_indented_body,
            1000,
            "org",
        );
        assert_linear(
            "org_indented_quote_table_rows",
            org_indented_quote_table_rows,
            1000,
            "org",
        );
        assert_linear("md_rebulleted_def_list", md_rebulleted_def_list, 1000, "md");
        assert_linear("d35_rollback_siblings", d35_rollback_siblings, 14, "md");
        assert_linear("d35_rollback_siblings", d35_rollback_siblings, 14, "org");
        assert_linear("d35_query_noop_chain", d35_query_noop_chain, 45, "md");
        assert_linear("d35_query_noop_chain", d35_query_noop_chain, 45, "org");
        assert_linear("d35_many_ws_deep", d35_many_ws_deep, 45, "md");
        assert_linear("d35_many_ws_deep", d35_many_ws_deep, 45, "org");
    });
    // (2b) no-SIGABRT on a SMALL (4 MiB) stack, where the old per-unit suffix-recurse overflowed at
    // ~24 KB on the default stack: both the escape (C) and code-leaf (D) straddle families now parse
    // ~64 KB with O(1) native stack. `forget` the flat AST — this guards only the PARSE stack.
    std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            std::mem::forget(lsdoc::parse(&resync(22_000), "md")); // escape ×22k ≈ 66 KB
            std::mem::forget(lsdoc::parse(&resync_leaf(13_000), "md")); // code-leaf ×13k ≈ 65 KB
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The audit's not-yet-fixed O(n²) families. EMPTY: all four (`gt_spine`/`gt_breaker` 1a/1b → A,
/// `hiccup_unclosed` 2a → B, `resync` + `resync_leaf` 2b → C/D) are now single-pass and live in
/// `complexity_gate`. Kept as a shell so a future re-scan regression has an obvious home.
#[test]
#[ignore = "empty — all audit O(n^2) families are single-pass (in complexity_gate)"]
fn complexity_gate_targets() {}

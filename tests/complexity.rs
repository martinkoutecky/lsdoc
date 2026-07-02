//! Complexity gate — the structural guard the byte-exact parity gate cannot be.
//!
//! `src/metrics.rs` counts "scan work": bytes examined by the parser's re-scanning operations
//! (the `>`-prefix peel, `property`'s `::` search, the hiccup balanced-bracket scan, the inline
//! `resync` re-lex). A single-pass parser examines each byte O(1) times, so scan-work MUST be
//! O(input length). This gate parses adversarial families at n / 2n / 4n and asserts the count
//! grows ~linearly (ratio < 3×). Because the count is **deterministic** (not timed), small inputs
//! give a clean signal and there is no machine-noise flakiness — the weakness that let four O(n²)
//! families hide behind 1321/1321 byte-exact.
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
        assert_linear(
            "org_begin_quote_indented_body",
            org_begin_quote_indented_body,
            1000,
            "org",
        );
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

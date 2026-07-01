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
/// whole remaining suffix per unit. (Bug 2b — inline resolver.)
fn resync(n: usize) -> String {
    "#a\\".repeat(n)
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
        // A (container-prefix consume): both formats now dispatch a `>`-line's content ONCE at the
        // final depth (no per-re-dispatch `property` re-scan — 1a) and close many frames at one
        // interior breaker in O(closed) (no per-frame `gt_cont_view` re-peel — 1b).
        assert_linear("gt_spine", gt_spine, 3000, "org");
        assert_linear("gt_spine", gt_spine, 3000, "md"); // A-md (1a)
        assert_linear("gt_breaker", gt_breaker, 1500, "org"); // A-org (1b)
        assert_linear("gt_breaker", gt_breaker, 1500, "md"); // A-md (1b)
    });
}

/// The audit's remaining O(n²) families. FAILS today (that is the point — it proves the gate catches
/// what 1321/1321 byte-exact missed). Each `assert_linear` moves up into `complexity_gate` as its
/// phase lands: A (container-prefix walk) DONE; 2a → B (hiccup index), 2b → C (delimiter stack).
#[test]
#[ignore = "audit O(n^2) targets — un-ignore/move to complexity_gate as A/B/C fix each"]
fn complexity_gate_targets() {
    big_stack(|| {
        assert_linear("hiccup_unclosed", hiccup_unclosed, 3000, "md"); // B (2a)
        assert_linear("hiccup_unclosed", hiccup_unclosed, 3000, "org"); // B (2a)
        assert_linear("resync", resync, 1500, "md"); // C (2b)
    });
}

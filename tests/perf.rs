//! Performance + robustness gate (SPEC §4: correctness is necessary, not
//! sufficient). Catches *catastrophic* regressions — accidental O(n²) emphasis
//! backtracking, per-token re-scans, unbounded recursion — not constant factors.
//!
//! `perf_smoke` runs in the normal (debug) `cargo test` and is sized to stay fast.
//! The full-scale gate (`*_heavy`) is `#[ignore]`d so it doesn't slow the dev loop;
//! run it explicitly: `cargo test --release -- --ignored` (see README).

use std::time::Instant;

fn parse(s: &str) {
    std::hint::black_box(lsdoc::parse_to_projection(std::hint::black_box(s)));
}

/// Inputs that would explode under super-linear scanning: long single-marker runs,
/// repeated constructs, and mixed-delimiter soup.
fn linear_cases(n: usize) -> Vec<(&'static str, String)> {
    vec![
        ("stars", "*".repeat(n)),
        ("underscores", "_".repeat(n)),
        ("open_brackets", "[".repeat(n)),
        ("open_parens", "(".repeat(n)),
        ("braces", "{".repeat(n)),
        ("backticks", "`".repeat(n)),
        ("hashes", "#".repeat(n)),
        ("emph_words", "*a ".repeat(n / 3)),
        ("page_open", "[[".repeat(n / 2)),
        ("block_open", "((".repeat(n / 2)),
        ("macro_open", "{{".repeat(n / 2)),
        ("tags", "#tag ".repeat(n / 5)),
        ("refs", "[[a]] ".repeat(n / 6)),
        ("mixed_delims", "a*b_c~`d[e(f".repeat(n / 11)),
        ("many_lines", "x\n".repeat(n / 2)),
    ]
}

/// Deeply-nested inputs: if any parse phase recursed O(depth), these overflow a
/// small stack; parsed on a 1 MiB-stack thread to prove bounded-depth.
fn deep_cases(d: usize) -> Vec<String> {
    vec![
        format!("{}x{}", "*".repeat(d), "*".repeat(d)),
        format!("{}x{}", "[".repeat(d), "]".repeat(d)),
        format!("{}x{}", "((".repeat(d), "))".repeat(d)),
        format!("{}x{}", "{{".repeat(d), "}}".repeat(d)),
        format!("{}x{}", "#[[".repeat(d), "]]".repeat(d)),
        "> x\n".repeat(d / 10),
        format!("{}x", "#+BEGIN_QUOTE\n".repeat(d / 20)),
    ]
}

fn assert_linear(n: usize, budget_ms: u128) {
    for (name, input) in &linear_cases(n) {
        let t = Instant::now();
        parse(input);
        let ms = t.elapsed().as_millis();
        assert!(
            ms < budget_ms,
            "'{name}' ({} bytes) took {ms}ms (> {budget_ms}ms) — possible O(n^2)/backtracking",
            input.len()
        );
    }
}

fn assert_no_overflow(d: usize) {
    let inputs = deep_cases(d);
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || inputs.iter().for_each(|s| parse(s)))
        .expect("spawn parse thread")
        .join()
        .expect("deep nesting overflowed a 1 MiB stack — parser is not bounded-depth");
}

#[test]
fn perf_smoke() {
    // Fast enough for the default loop; a catastrophic regression still blows the
    // budget by orders of magnitude at this size.
    assert_linear(20_000, 1500);
    assert_no_overflow(40_000);
}

#[test]
#[ignore = "full-scale perf gate; run with: cargo test --release -- --ignored"]
fn pathological_inputs_stay_linear_heavy() {
    assert_linear(100_000, 3000);
}

#[test]
#[ignore = "full-scale stack gate; run with: cargo test --release -- --ignored"]
fn deep_nesting_does_not_overflow_the_stack_heavy() {
    assert_no_overflow(200_000);
}

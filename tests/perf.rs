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

fn parse_org(s: &str) {
    std::hint::black_box(lsdoc::parse_org_to_projection(std::hint::black_box(s)));
}

/// Org-specific pathological inputs (a fixed O(n²) regression lived here): long
/// emphasis-marker runs, headline runs, `[[`/`[[a][b]]` runs.
fn org_linear_cases(n: usize) -> Vec<(&'static str, String)> {
    vec![
        ("o_stars", "*".repeat(n)),
        ("o_slash", "/".repeat(n)),
        ("o_under", "_".repeat(n)),
        ("o_plus", "+".repeat(n)),
        ("o_headlines", "* x\n".repeat(n / 4)),
        ("o_emph_words", "*a* ".repeat(n / 4)),
        ("o_page_open", "[[".repeat(n / 2)),
        ("o_links", "[[a][b]] ".repeat(n / 9)),
        ("o_deep_emph", format!("{}x{}", "*".repeat(n / 2), "*".repeat(n / 2))),
        // Org multi-line list: long sibling run, long single-item continuation fold,
        // and the indented-`-` COLLAPSE (a memoised collapse-floor keeps repeated
        // collapse attempts linear instead of O(n²) suffix re-scanning).
        ("o_list_siblings", "- a\n".repeat(n / 4)),
        ("o_list_fold", format!("- a{}", "\n  cont".repeat(n / 4))),
        ("o_list_collapse", format!("{}  - z", "- a\n".repeat(n / 4))),
        // Org footnote-definition body absorbing a long continuation-line run
        // (mldoc `footnote_definition = many1 l`): must be single-pass / linear.
        ("o_fn_fold", format!("[fn:1] body{}", "\ncont".repeat(n / 4))),
        // Org unclosed-opener families (audit P3/P5/P8/P9): a run of openers with NO
        // closer ahead must NOT re-scan to EOF per opener (was O(n²) / O(n³)).
        ("o_block_open", "#+BEGIN_FOO\n".repeat(n / 4)), // P3: no #+END
        // P3 with UNIQUE names — a name-keyed memo alone would still miss every time;
        // the name-INDEPENDENT "no #+END ahead" floor keeps it linear.
        (
            "o_block_open_uniq",
            (0..n / 4).map(|k| format!("#+BEGIN_B{k}\n")).collect(),
        ),
        ("o_drawer_open", ":a:\nx\n".repeat(n / 8)), // P5: no :END:
        ("o_inline_html", "<tag>".repeat(n / 5)),    // P8: no </tag>
        ("o_inline_latex", "\\(".repeat(n / 2)),     // P9: no \)
        // C7 hiccup (org): unclosed-opener run + consecutive block hiccups, both linear.
        ("o_hiccup_open", "[:div ".repeat(n / 6)),
        ("o_hiccup_blocks", "[:a]".repeat(n / 4)),
        ("o_hiccup_inline", "x [:a] ".repeat(n / 7)),
    ]
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
        // Markdown unclosed-opener families (audit P1/P4/P6/P7/P8/P9): a run of openers
        // with NO closer ahead must not re-scan to EOF per opener.
        ("md_link_bait", format!("{}](", "[".repeat(n))), // P1: `[`×m + `](` (was O(n³))
        ("md_callout_open", "#+BEGIN_FOO\n".repeat(n / 4)), // P4: no #+END
        ("md_drawer_open", ":a:\nx\n".repeat(n / 8)),     // P6: no :END:
        ("md_drawer_consec", ":a:\n".repeat(n / 4)),      // P6: consecutive `:a:`
        ("md_dash_fence", "- ``` \n".repeat(n / 6)),      // P7: unclosed dash-bullet fence
        ("md_inline_html", "<tag>".repeat(n / 5)),        // P8: no </tag>
        ("md_inline_latex", "\\(".repeat(n / 2)),         // P9: no \)
        // C7 hiccup: a run of UNCLOSED `[:tag ` (no `]` anywhere) must bail O(1) per
        // occurrence via the `]`-absence cache (block) / `rbracket_present` (inline).
        ("md_hiccup_open", "[:div ".repeat(n / 6)),
        // consecutive whole-line block hiccups: the in-place remainder split keeps this
        // linear (no per-hiccup re-precomputation / recursion).
        ("md_hiccup_blocks", "[:a]".repeat(n / 4)),
        ("md_hiccup_inline", "x [:a] ".repeat(n / 7)),    // inline hiccups in a paragraph
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

/// Org-only deep inputs. A single line of `>`×d nests Org `Quote`s (md's quote path is
/// flat); the handler must peel iteratively AND bound nesting depth so the parse, ref
/// walk, and drop of the result can't overflow (audit P2: this aborted ~7.5k `>`).
fn org_deep_cases(d: usize) -> Vec<String> {
    vec![
        format!("{}x", ">".repeat(d)),       // P2: `>`×d on ONE line
        format!("x\n{}y", ">".repeat(d)),    // deep `>` line below a paragraph (recursion path)
        format!("{}x\n> y", ">".repeat(d)),  // deep `>` first line + continuation (recursion path)
        "> x\n".repeat(d / 10),              // wide (single quote, many body lines)
    ]
}

fn assert_no_overflow_org(d: usize) {
    let inputs = org_deep_cases(d);
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || inputs.iter().for_each(|s| parse_org(s)))
        .expect("spawn parse thread")
        .join()
        .expect("deep org `>` nesting overflowed a 1 MiB stack — quote handler not bounded");
}

fn assert_linear_org(n: usize, budget_ms: u128) {
    for (name, input) in &org_linear_cases(n) {
        let t = Instant::now();
        parse_org(input);
        let ms = t.elapsed().as_millis();
        assert!(
            ms < budget_ms,
            "org '{name}' ({} bytes) took {ms}ms (> {budget_ms}ms) — possible O(n^2)/backtracking",
            input.len()
        );
    }
}

#[test]
fn perf_smoke() {
    // Fast enough for the default loop; a catastrophic regression still blows the
    // budget by orders of magnitude at this size.
    assert_linear(20_000, 1500);
    assert_linear_org(20_000, 1500);
    assert_no_overflow(40_000);
    assert_no_overflow_org(40_000);
}

#[test]
#[ignore = "full-scale perf gate; run with: cargo test --release -- --ignored"]
fn pathological_inputs_stay_linear_heavy() {
    assert_linear(100_000, 3000);
    assert_linear_org(100_000, 3000);
}

#[test]
#[ignore = "full-scale stack gate; run with: cargo test --release -- --ignored"]
fn deep_nesting_does_not_overflow_the_stack_heavy() {
    assert_no_overflow(200_000);
    assert_no_overflow_org(200_000);
}

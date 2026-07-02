//! Performance + robustness gate (SPEC §4: correctness is necessary, not
//! sufficient). Catches *catastrophic* regressions — accidental O(n²) emphasis
//! backtracking, per-token re-scans, unbounded recursion — not constant factors.
//!
//! `perf_smoke` runs in the normal (debug) `cargo test` and is sized to stay fast.
//! The full-scale gate (`*_heavy`) is `#[ignore]`d so it doesn't slow the dev loop;
//! run it explicitly: `cargo test --release -- --ignored` (see README).

use std::{fmt::Write, time::Instant};

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
        ("o_lt_bare", "<".repeat(n)),
        ("o_emph_lt", "/a/<".repeat(n / 4)),
        ("o_email_domain_interleave", "/a/<x@".repeat(n / 6)),
        ("o_timestamp_angle_interleave", "/a/<20".repeat(n / 6)),
        ("o_pageref_lt", "[[a]]<".repeat(n / 6)),
        ("o_page_open", "[[".repeat(n / 2)),
        ("o_macro_interleave", "/a/{{".repeat(n / 5)),
        ("o_export_snippet_interleave", "/a/@@a: b\n".repeat(n / 10)),
        ("o_blockref_interleave", "/a/((".repeat(n / 5)),
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
        ("lt_bare", "<".repeat(n)),
        ("emph_lt", "*a*<".repeat(n / 4)),
        ("email_domain_interleave", "*a*<x@".repeat(n / 6)),
        ("timestamp_angle_interleave", "*a*<20".repeat(n / 6)),
        ("autolink_interleave", "*a*<a:".repeat(n / 6)),
        ("pageref_lt", "[[a]]<".repeat(n / 6)),
        ("page_open", "[[".repeat(n / 2)),
        ("block_open", "((".repeat(n / 2)),
        ("macro_open", "{{".repeat(n / 2)),
        ("macro_interleave", "*a*{{".repeat(n / 5)),
        ("export_snippet_interleave", "*a*@@a: b\n".repeat(n / 10)),
        ("blockref_interleave", "*a*((".repeat(n / 5)),
        ("raw_html_unbalanced_interleave", "*a*<div><div>x</div>".repeat(n / 20)),
        ("tags", "#tag ".repeat(n / 5)),
        ("tag_hash_run", "#".repeat(n)),
        ("tag_word_interleave", "x #a".repeat(n / 4)),
        ("bare_url_interleave", "*a*httpx".repeat(n / 8)),
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
        // Markdown multi-line list (mirrors the org cases): a long sibling run, a single item
        // with a long continuation-fold tail, and the deeper-unparseable-shape COLLAPSE (the
        // memoised `collapse_floor` must keep repeated collapse attempts linear, not O(n^2)
        // suffix re-scanning). Each item's content is re-parsed once over its own folded lines,
        // so the total content-reparse work stays O(n).
        ("md_list_siblings", "* a\n".repeat(n / 4)),
        ("md_list_fold", format!("* a{}", "\n  cont".repeat(n / 4))),
        ("md_list_collapse", format!("{}  5z", "* a\n".repeat(n / 4))),
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
        // RECURSING nested callouts (distinct names that actually CLOSE → the body was
        // re-parsed by the OLD recursion, unlike the unclosed `#+BEGIN_QUOTE` run above). The
        // streaming driver opens each as a HEAP frame (no native recursion), so the PARSE uses
        // O(1) native stack at any depth `d/20` — `assert_no_overflow` forgets the (deep) result
        // so its recursive drop doesn't overflow the 1 MiB test stack (see that fn).
        {
            let dd = d / 20;
            let mut s = String::new();
            for k in 0..dd {
                s.push_str(&format!("#+BEGIN_a{k}\n"));
            }
            s.push_str("x\n");
            for k in (0..dd).rev() {
                s.push_str(&format!("#+END_a{k}\n"));
            }
            s
        },
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
        // Assert the streaming PARSER is bounded-depth: it builds the `Block` tree ITERATIVELY (the
        // container stack lives on the HEAP), so it never overflows the native stack regardless of
        // nesting depth. We call the raw block parser (`lsdoc::parse`, not `parse_to_projection`) and
        // `forget` the result on purpose: DROPPING a deeply-nested `Block` tree recurses and would
        // overflow the 1 MiB test stack — but that recursive drop (and the recursive project/serialize
        // a consumer does) is a DOWNSTREAM property of a recursive AST, not a parser one, inherent and
        // bounded by the consumer's stack (mldoc overflows far earlier, at PARSE time ~1000).
        .spawn(move || inputs.iter().for_each(|s| std::mem::forget(lsdoc::parse(s, "md"))))
        .expect("spawn parse thread")
        .join()
        .expect("deep nesting overflowed a 1 MiB stack — the streaming parser is not bounded-depth");
}

/// Org-only deep inputs under the STREAMING driver. `>`×d on one line nests ⌈d/2⌉ Org
/// `Quote`s; the streaming `gt_strip` peel (`build_org_quote_streaming`) builds that spine
/// ITERATIVELY — single-line peel, multi-line tail-peel, lazy-absorbed continuation — with
/// NO native recursion and NO `>`-depth cap. Includes the MULTI-LINE `>`-nests (a deep
/// opener + a continuation that rides into the deepest level; a deep `>` line below a
/// paragraph), the case the old recurse-on-body would stack-overflow uncapped.
fn org_deep_cases(d: usize) -> Vec<String> {
    vec![
        format!("{}x", ">".repeat(d)),       // `>`×d on ONE line (single-line peel)
        format!("x\n{}y", ">".repeat(d)),    // deep `>` line below a paragraph (single-line peel)
        format!("{}x\n> y", ">".repeat(d)),  // MULTI-LINE: deep opener + a lazily-absorbed cont
        "> x\n".repeat(d / 10),              // wide (single quote, many body lines)
    ]
}

fn assert_no_overflow_org(d: usize) {
    let inputs = org_deep_cases(d);
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        // Assert the STREAMING Org parser is bounded-depth: `build_org_quote_streaming` peels
        // the `>`-spine ITERATIVELY (the wrappers are a loop, the residual a single re-dispatch),
        // so a deep `>`-nest never grows the native stack — no cap needed. `forget` the result,
        // exactly like md's `assert_no_overflow`: DROPPING the deep `Quote` tree recurses and
        // would overflow the 1 MiB test stack, but that recursive drop is a DOWNSTREAM property
        // of a recursive AST (the consumer's stack), not a parser one. We call the streaming
        // root entry point directly (`__parse_org_streaming`, identical to the public `parse`,
        // which is now the streaming driver).
        .spawn(move || inputs.iter().for_each(|s| std::mem::forget(lsdoc::__parse_org_streaming(s))))
        .expect("spawn parse thread")
        .join()
        .expect("deep org `>` nesting overflowed a 1 MiB stack — streaming quote peel not bounded");
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

/// Time a single parse in microseconds (release-only signal; debug is noisy).
fn time_us(input: &str, is_org: bool) -> u128 {
    let t = Instant::now();
    if is_org {
        parse_org(input);
    } else {
        parse(input);
    }
    t.elapsed().as_micros()
}

/// Best-of-N timing with a warmup pass — the ratio gate measures STEADY-STATE asymptotics,
/// not allocator/cache cold-start (the v2 resolver allocates a token Vec, so a cold first
/// touch of the larger 2n buffer otherwise inflates a single-shot ratio). `min` is the right
/// statistic for "how fast can it go": it strips one-off scheduling / page-fault / cache-evict
/// noise without hiding a true O(n²) (which inflates EVERY run, min included).
fn best_us(input: &str, is_org: bool, runs: usize) -> u128 {
    time_us(input, is_org); // warmup: prime allocator + caches
    (0..runs).map(|_| time_us(input, is_org)).min().unwrap()
}

/// Round-2 audit class: a closer/END that exists "elsewhere" (a later line, a different
/// block name) defeated v1's per-construct caches/floors, re-enabling O(n²)/O(n³). These
/// generators are NOT in `linear_cases` because v1's budget test would hang on them at
/// 100k. Each carries its OWN base size: the cubic uses a SMALL base so a regression fails
/// *cleanly* (ratio check) instead of hanging; the quadratics use a large base for signal.
/// (name, is_org, base_n, build(n) -> input)
fn scaling_pairs() -> Vec<(&'static str, bool, usize, fn(usize) -> String)> {
    vec![
        // R2-P1: `[`×m + a markdown link tail on a LATER line → was O(n³). Small base.
        ("md_link_nl", false, 1500, |n| format!("{}\n](x)", "[".repeat(n))),
        // R2-P2: `[`×m + `]]` on a later line → O(n²) (md + org).
        ("md_pageref_nl", false, 25_000, |n| format!("{}\n]]", "[".repeat(n))),
        ("org_pageref_nl", true, 25_000, |n| format!("{}\n]]", "[".repeat(n))),
        // R2-P3: org inline present-closer — macro `{{`, block-ref `((` (later closer).
        ("md_macro_closer", false, 25_000, |n| "{{x ".repeat(n) + "}}"),
        ("org_macro_closer", true, 25_000, |n| "{{x ".repeat(n) + "}}"),
        ("md_blockref_closer", false, 25_000, |n| "((x ".repeat(n) + "))"),
        ("org_blockref_closer", true, 25_000, |n| "((x ".repeat(n) + "))"),
        // R2-P4: name-independent floor defeated by ONE non-matching `#+END_BAR` (md + org).
        ("md_block_mismatch", false, 25_000, |n| {
            "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"
        }),
        ("org_block_mismatch", true, 25_000, |n| {
            "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"
        }),
        // R2-P5: hiccup `[:div `×n + a single trailing `]` defeated the rbracket caches.
        ("md_hiccup_present", false, 25_000, |n| "[:div ".repeat(n) + "]"),
        ("org_hiccup_present", true, 25_000, |n| "[:div ".repeat(n) + "]"),
        // F2: consecutive CLOSED, nested hiccup vectors — the org 13b remainder loop, which (like
        // md 11d') re-dispatched the whole shrinking line per vector and re-ran `property`-style
        // O(len) predicates on the tail → O(n²). Now consumed in one local pass. (md is locked
        // separately in `md_hiccup_nested_scales_linearly_heavy`.)
        ("org_hiccup_nested", true, 8_000, |n| "[:div [:span x] [:b y]] ".repeat(n)),
        // Nested-emphasis reparse guard (design-review concern): content is re-scanned on a
        // shrinking substring. mldoc's first-valid-closer pairs the NEAREST closer, so nesting
        // depth is bounded (~5 distinct markers) → this is O(n), measured ≈2×/doubling. The
        // probe locks that in so the lexer/resolver rewrite can't reintroduce O(n²) here.
        ("md_emph_alt", false, 25_000, |n| "*_".repeat(n) + "x" + &"_*".repeat(n)),
        ("org_emph_alt", true, 25_000, |n| "*/".repeat(n) + "x" + &"/*".repeat(n)),
        // Latex `\(`×n with NO `\)` closer. Was O(n²) in Org (a `find_sub` EOF re-scan per
        // `\(`); the monotone closer floor (resolver.rs + org_resolver.rs) makes it linear.
        // Ratio-gated because the absolute-budget `o_inline_latex` case masked the quadratic
        // (it sat just under budget at low load until n grew). md was already floored.
        ("md_latex_open", false, 25_000, |n| "\\(".repeat(n)),
        ("org_latex_open", true, 25_000, |n| "\\(".repeat(n)),
        // Phase B leaf-linearity: construct-interleaved inline LEAF misses. Homogeneous
        // opener runs were already covered; these force a fresh dispatch before each opener.
        ("md_email_domain_interleave", false, 25_000, |n| "*a*<x@".repeat(n)),
        ("org_email_domain_interleave", true, 25_000, |n| "/a/<x@".repeat(n)),
        ("md_timestamp_angle_interleave", false, 25_000, |n| "*a*<20".repeat(n)),
        ("org_timestamp_angle_interleave", true, 25_000, |n| "/a/<20".repeat(n)),
        ("md_autolink_interleave", false, 25_000, |n| "*a*<a:".repeat(n)),
        ("md_macro_interleave", false, 25_000, |n| "*a*{{".repeat(n)),
        ("org_macro_interleave", true, 25_000, |n| "/a/{{".repeat(n)),
        ("md_blockref_interleave", false, 25_000, |n| "*a*((".repeat(n)),
        ("org_blockref_interleave", true, 25_000, |n| "/a/((".repeat(n)),
        ("md_tag_hash_run", false, 25_000, |n| "#".repeat(n)),
        ("md_tag_word_interleave", false, 25_000, |n| "x #a".repeat(n)),
        ("md_bare_url_interleave", false, 25_000, |n| "*a*httpx".repeat(n)),
        ("md_raw_html_unbalanced_interleave", false, 8_000, |n| {
            "*a*<div><div>x</div>".repeat(n)
        }),
        // Callout closer-finding adversarial cases. The on-demand dispatch (correct: only
        // top-level openers are reached) finds `#+END_<name>` via the by-all-prefixes index — an
        // O(1) bucket lookup, no EOF scan. So even these stay ~linear, where mldoc's own
        // `take_until` is O(n²) (measured: 4000 unclosed openers = 68s in mldoc):
        //  - UNIQUE-name openers each with a non-matching `#+END_` (absent bucket ⇒ O(1)/opener):
        ("md_callout_uniq", false, 8_000, |n| {
            (0..n).map(|k| format!("#+BEGIN_A{k}\n#+END_Z{k}\n")).collect::<String>()
        }),
        ("org_callout_uniq", true, 8_000, |n| {
            (0..n).map(|k| format!("#+BEGIN_A{k}\n#+END_Z{k}\n")).collect::<String>()
        }),
        //  - a validly-closed callout with a LONG NAME (index build is O(name), lookup O(1)):
        ("md_callout_longname", false, 8_000, |n| format!("#+BEGIN_{0}\n#+END_{0}x", "b".repeat(n))),
        ("org_callout_longname", true, 8_000, |n| format!("#+BEGIN_{0}\n#+END_{0}x", "b".repeat(n))),
        // DEEPLY-NESTED distinct callouts that close → the OLD recurse-on-body (mldoc is itself
        // O(n²) here AND stack-overflows). Both streaming drivers open each as a HEAP frame —
        // each line classified once → genuine O(n), NO cap (ratio ≈2×/doubling). Both bases are
        // kept drop-safe (max 4× = 4000-deep, whose recursive drop fits the ratio test's 8 MiB
        // main stack): org now runs the streaming default (uncapped, deep result), so its base
        // drops 2000 → 1000 to match md.
        ("md_nested_callout", false, 1_000, |n| nested_callout(n)),
        ("org_nested_callout", true, 1_000, |n| nested_callout(n)),
        // Inline-spans v2 Round 2: one transformed quote buffer with O(n) origin segments and
        // O(n) inline nodes. The source-map remap cursor must keep these near 2× per doubling.
        ("md_flat_gt_quote_lines", false, 4_000, flat_gt_quote_lines),
        ("org_flat_gt_quote_lines", true, 4_000, flat_gt_quote_lines),
        (
            "org_begin_quote_indented_body",
            true,
            4_000,
            org_begin_quote_indented_body,
        ),
    ]
}

fn flat_gt_quote_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(&mut s, "> line {i}").unwrap();
    }
    s
}

fn org_begin_quote_indented_body(n: usize) -> String {
    let mut s = String::from("#+BEGIN_QUOTE\n");
    for i in 0..n {
        writeln!(&mut s, "  line {i}").unwrap();
    }
    s.push_str("#+END_QUOTE\n");
    s
}

/// `#+BEGIN_a0 … #+BEGIN_a{n-1}` / `x` / `#+END_a{n-1} … #+END_a0` — n distinct-name callouts
/// nested `n` deep that all close (so each body is re-parsed).
fn nested_callout(n: usize) -> String {
    let mut s = String::new();
    for k in 0..n {
        s.push_str(&format!("#+BEGIN_a{k:06}\n"));
    }
    s.push_str("x\n");
    for k in (0..n).rev() {
        s.push_str(&format!("#+END_a{k:06}\n"));
    }
    s
}

/// Assert each round-2 generator scales ~linearly. This NFS/shared box is too noisy for a
/// single n→2n ratio: linear cases spike to ~3.4× on an individual cold doubling (a memory/
/// cache boundary), so a tight single-doubling cap false-fails (verified across 4 doublings in
/// `scaling_probe`: every generator is ~2×/doubling with a lone spike). The robust
/// discriminator: an O(n²)/O(n³) regression inflates **every** doubling (~4×/~8×), whereas a
/// linear case always has at least one clean ~2× doubling. So we measure two consecutive
/// doublings (n→2n, 2n→4n) and gate on the MIN: linear ⇒ min ≈2×; O(n²) ⇒ min ≈4×;
/// O(n³) ⇒ min ≈8×. CAP=3.0 separates them with margin and is immune to single-point spikes.
/// Both v1 and the v2 resolver pass (v2's token-Vec allocation lifts the constant, not the
/// class). FLOOR_US guards sub-ms jitter.
fn assert_linear_scaling() {
    const CAP: f64 = 3.0;
    const FLOOR_US: f64 = 20_000.0; // 20ms — below this a ratio is just measurement noise
    for (name, is_org, base, build) in scaling_pairs() {
        let tn = best_us(&build(base), is_org, 3) as f64;
        let t2n = best_us(&build(2 * base), is_org, 3) as f64;
        let t4n = best_us(&build(4 * base), is_org, 3) as f64;
        let r1 = t2n / tn.max(FLOOR_US);
        let r2 = t4n / t2n.max(FLOOR_US);
        let ratio = r1.min(r2);
        assert!(
            ratio < CAP,
            "scaling '{name}': n={base} {:.0}ms → 2n {:.0}ms → 4n {:.0}ms; doublings {r1:.1}×, \
             {r2:.1}× — MIN {ratio:.1}× (linear ≈2×; >{CAP}× both ⇒ O(n²)/O(n³) regression)",
            tn / 1000.0,
            t2n / 1000.0,
            t4n / 1000.0,
        );
    }
}

/// Best-of-N time (µs) to PARSE a deep `>`-nest via the STREAMING driver, FORGETTING the
/// result. The deep `Quote` tree's recursive DROP would dominate the measurement (and
/// overflow the main stack) — a downstream property of a recursive AST, not the parser; we
/// measure only the bounded-depth, iterative parse. Mirrors `best_us`'s warmup + min.
fn best_us_org_deep_quote(input: &str, runs: usize) -> u128 {
    let once = |s: &str| {
        let t = Instant::now();
        std::mem::forget(lsdoc::__parse_org_streaming(std::hint::black_box(s)));
        t.elapsed().as_micros()
    };
    once(input); // warmup: prime allocator + caches
    (0..runs).map(|_| once(input)).min().unwrap()
}

/// M4 O(n) gate: a deep MULTI-LINE `>`-nest scales ~linearly under the streaming `gt_strip`
/// peel — each line is stripped to its depth ONCE (Σ `>`-counts = O(n)), with NO per-level
/// body copy and NO native recursion, so the `QUOTE_NEST_CAP` is GONE. The legacy
/// recurse-on-body was O(n²) + stack-overflow on this exact shape (a `>`×d opener that peels
/// ⌈d/2⌉ deep, plus a continuation lazily absorbed into the deepest level). Two doublings,
/// gate on the MIN (immune to a single cold spike), like `assert_linear_scaling`.
#[test]
#[ignore = "M4 deep-`>` O(n) gate; run with: cargo test --release --test perf -- --ignored"]
fn org_deep_quote_scales_linearly_heavy() {
    const CAP: f64 = 3.0;
    const FLOOR_US: f64 = 20_000.0; // 20ms — below this a ratio is just measurement noise
    let build = |d: usize| format!("{}x\n> y", ">".repeat(d));
    let base = 100_000usize;
    let tn = best_us_org_deep_quote(&build(base), 3) as f64;
    let t2n = best_us_org_deep_quote(&build(2 * base), 3) as f64;
    let t4n = best_us_org_deep_quote(&build(4 * base), 3) as f64;
    let r1 = t2n / tn.max(FLOOR_US);
    let r2 = t4n / t2n.max(FLOOR_US);
    let ratio = r1.min(r2);
    assert!(
        ratio < CAP,
        "org deep `>` (gt_strip peel): d={base} {:.1}ms → 2d {:.1}ms → 4d {:.1}ms; doublings \
         {r1:.1}×, {r2:.1}× — MIN {ratio:.1}× (linear ≈2×; >{CAP}× ⇒ O(n²)/recurse-on-body regression)",
        tn / 1000.0,
        t2n / 1000.0,
        t4n / 1000.0,
    );
}

/// Regression lock: a run of properly-closed, consecutive+nested hiccup vectors
/// (`[:div [:span x] [:b y]]`×n) must scale ~linearly. This WAS O(n²) — and NOT in the inline
/// resolver (a `[:…]` line becomes a RAW `Block::Hiccup`, never inline-parsed): the block parser's
/// step 11d' re-dispatched the whole SHRINKING remainder line once per vector, re-running every
/// earlier ladder predicate on the tail each time — notably `property`'s O(line) `find("::")` —
/// so N vectors cost Σ O(remaining) = O(n²) (measured ≈4×/doubling: 5k→10k→20k→40k reps =
/// 241→948→3781→13635 ms). FIXED by consuming consecutive block hiccups in ONE local loop in 11d'
/// (each ladder predicate now runs O(1)× per source line). The gate originally MISSED it because
/// the other hiccup cases (`md_hiccup_present` = `[:div `×n + one `]`) are FLAT unclosed runs that
/// never trigger the remainder loop. Guards against reintroducing the per-vector re-dispatch.
#[test]
#[ignore = "hiccup-nested O(n) regression lock; run with: cargo test --release --test perf -- --ignored"]
fn md_hiccup_nested_scales_linearly_heavy() {
    const CAP: f64 = 3.0;
    const FLOOR_US: f64 = 20_000.0; // 20ms — below this a ratio is just measurement noise
    let build = |n: usize| "[:div [:span x] [:b y]] ".repeat(n);
    let base = 25_000usize;
    let tn = best_us(&build(base), false, 3) as f64;
    let t2n = best_us(&build(2 * base), false, 3) as f64;
    let t4n = best_us(&build(4 * base), false, 3) as f64;
    let r1 = t2n / tn.max(FLOOR_US);
    let r2 = t4n / t2n.max(FLOOR_US);
    let ratio = r1.min(r2);
    assert!(
        ratio < CAP,
        "nested hiccup: n={base} {:.1}ms → 2n {:.1}ms → 4n {:.1}ms; doublings {r1:.1}×, {r2:.1}× \
         — MIN {ratio:.1}× (linear ≈2×; >{CAP}× ⇒ O(n²)) [KNOWN BUG, not yet fixed — see analysis]",
        tn / 1000.0,
        t2n / 1000.0,
        t4n / 1000.0,
    );
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

/// The structural guarantee v1's point-caches lacked: every round-1 + round-2 pathological
/// generator must scale ~linearly (ratio < 3×), so the optimistic-scan O(n²)/O(n³) class can
/// never silently return. This FAILS on pre-v2 code (the round-2 findings are still
/// super-linear) and is the acceptance gate the scanner redesign drives to green.
#[test]
#[ignore = "linear-scaling gate; run with: cargo test --release -- --ignored"]
fn pathological_inputs_scale_linearly_heavy() {
    assert_linear_scaling();
}

/// `render_html` regression guard. The `> [!TYPE]` callout path once rebuilt the body via
/// `children[1..].iter().cloned()` and recursed — deep-cloning the whole subtree once per
/// nesting level → O(n²) time + memory on nested callouts (audit HIGH: ~3.2s at depth 4000,
/// OOM at 16000). The md parser caps nesting at `BLOCK_NEST_CAP=64`, so we build the nested
/// `[!NOTE]` AST DIRECTLY to reach the regime. Linear render of ~4000 callout blocks is low-ms;
/// the cloning regression was ~3.2s. A 500ms bound separates them with >6× margin. Big stack
/// because `render_html` recurses with AST depth; build iteratively, drop on the same thread.
#[test]
#[ignore = "render_html linear gate; run with: cargo test --release -- --ignored"]
fn render_html_nested_callout_is_linear_heavy() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            use lsdoc::ast::{Block, Inline};
            fn build(depth: usize) -> Vec<Block> {
                let lead = || {
                    vec![
                        Inline::Plain {
                            text: "[!NOTE] t".to_string(),
                            span: None,
                            span_map: None,
                        },
                        Inline::Break { span: None },
                        Inline::Plain {
                            text: "x".to_string(),
                            span: None,
                            span_map: None,
                        },
                    ]
                };
                let mut node = Block::Quote {
                    children: vec![Block::Paragraph { inline: lead(), span: None }],
                    span: None,
                };
                for _ in 1..depth {
                    node = Block::Quote {
                        children: vec![Block::Paragraph { inline: lead(), span: None }, node],
                        span: None,
                    };
                }
                vec![node]
            }
            let opts = lsdoc::RenderOpts { format: lsdoc::Format::Md };
            let blocks = build(4000);
            let render_us = || -> u128 {
                let t = Instant::now();
                let s = lsdoc::render_html(std::hint::black_box(&blocks), &opts);
                std::hint::black_box(&s);
                t.elapsed().as_micros()
            };
            render_us(); // warmup
            let us = (0..3).map(|_| render_us()).min().unwrap();
            assert!(
                us < 500_000,
                "render_html nested [!TYPE] callout took {}ms at depth 4000 — expected <500ms \
                 (linear); the quote() O(n²) subtree clone is likely back",
                us / 1000
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// P4 cap-removal gate: the `>`-quote STAIRCASE (`> x\n> > x\n> > > x…`) — the exact shape the
/// former `BLOCK_NEST_CAP=64` bounded — is now iterative `>`-container frames (P3), so it parses
/// to FULL depth uncapped. A staircase far past 64 must nest that many `Quote`s, NOT degrade to a
/// flat Paragraph at 64. The parse itself is O(depth) HEAP (no native recursion); we build/walk/
/// drop the deep tree on a big stack because the AST is recursive (a downstream consumer property,
/// not the parser's). Locks that the staircase can never silently re-acquire a depth cap.
#[test]
#[ignore = "P4 staircase-uncapped gate; run with: cargo test --release --test perf -- --ignored"]
fn quote_staircase_uncapped_heavy() {
    use lsdoc::ast::Block;
    fn quote_depth(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                Block::Quote { children, .. } => 1 + quote_depth(children),
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }
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
    let d = 256usize; // 4× past the old cap (64)
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            for fmt in ["md", "org"] {
                let got = quote_depth(&lsdoc::parse(&staircase(d), fmt));
                assert!(
                    got > 64,
                    "{fmt}: `>`-staircase nested only {got} deep at input depth {d} — the old \
                     BLOCK_NEST_CAP=64 (now GT_FALLBACK_NEST_CAP, §3-fallback only) must NOT bound \
                     the staircase; it is iterative `>`-container frames (P3)"
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

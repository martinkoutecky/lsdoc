//! THROWAWAY probe: print n→2n scaling ratios for every round-2 pair (no asserts).
//! Run: cargo test --release --test scaling_probe -- --nocapture ; deleted before commit.
use std::time::Instant;

fn t_us(s: &str, org: bool) -> u128 {
    let t = Instant::now();
    if org {
        std::hint::black_box(lsdoc::parse_org_to_projection(std::hint::black_box(s)));
    } else {
        std::hint::black_box(lsdoc::parse_to_projection(std::hint::black_box(s)));
    }
    t.elapsed().as_micros()
}

fn pairs() -> Vec<(&'static str, bool, usize, fn(usize) -> String)> {
    vec![
        ("md_link_nl", false, 1500, |n| format!("{}\n](x)", "[".repeat(n))),
        ("md_pageref_nl", false, 25_000, |n| format!("{}\n]]", "[".repeat(n))),
        ("org_pageref_nl", true, 25_000, |n| format!("{}\n]]", "[".repeat(n))),
        ("md_macro_closer", false, 25_000, |n| "{{x ".repeat(n) + "}}"),
        ("org_macro_closer", true, 25_000, |n| "{{x ".repeat(n) + "}}"),
        ("md_blockref_closer", false, 25_000, |n| "((x ".repeat(n) + "))"),
        ("org_blockref_closer", true, 25_000, |n| "((x ".repeat(n) + "))"),
        ("md_block_mismatch", false, 25_000, |n| "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"),
        ("org_block_mismatch", true, 25_000, |n| "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"),
        ("md_hiccup_present", false, 25_000, |n| "[:div ".repeat(n) + "]"),
        ("org_hiccup_present", true, 25_000, |n| "[:div ".repeat(n) + "]"),
    ]
}

fn t_inline(s: &str, org: bool) -> u128 {
    let t = Instant::now();
    std::hint::black_box(lsdoc::inline(std::hint::black_box(s), if org { "org" } else { "md" }));
    t.elapsed().as_micros()
}

#[test]
fn iso() {
    // isolate inline-only vs full projection for the two hiccup pathologies.
    for (name, org) in [("md_hiccup", false), ("org_hiccup", true)] {
        let b1 = "[:div ".repeat(25_000) + "]";
        let b2 = "[:div ".repeat(50_000) + "]";
        let i1 = t_inline(&b1, org) as f64;
        let i2 = t_inline(&b2, org) as f64;
        println!("{name:<12} INLINE-only {:.1}ms -> {:.1}ms = {:.2}x", i1/1000.0, i2/1000.0, i2/i1.max(20_000.0));
    }
}

/// Measure v1's nested-emphasis exponent (design-review concern: reparse re-scans a
/// shrinking substring → possible O(n·depth)=O(n²)). Print ratios across n,2n,4n so the
/// exponent is visible (O(n)≈2×, O(n²)≈4× per doubling).
#[test]
fn emph_nest() {
    let cases: &[(&str, bool, fn(usize) -> String)] = &[
        // review's generator: alternating */_ markers around a center.
        ("md_emph_alt", false, |n| "*_".repeat(n) + "x" + &"_*".repeat(n)),
        ("org_emph_alt", true, |n| "*/".repeat(n) + "x" + &"/*".repeat(n)),
        // distinct 2-char markers genuinely nest (bounded depth ~5); long body to stress reparse.
        ("md_emph_distinct", false, |n| {
            format!("~~=={}==~~", "^^".to_string() + &"a".repeat(n) + "^^")
        }),
        // many small adjacent emphases (sequence, should be linear).
        ("md_emph_seq", false, |n| "*a* ".repeat(n)),
        // same-marker long run (caps at 3; should be linear/trivial).
        ("md_emph_run", false, |n| "*".repeat(n) + "a" + &"*".repeat(n)),
        // block name-mismatch (classify: real O(n²) vs memory-bound-linear).
        ("org_block_mismatch", true, |n| "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"),
        ("md_block_mismatch", false, |n| "#+BEGIN_FOO\n".repeat(n) + "#+END_BAR\n"),
    ];
    for &(name, org, build) in cases {
        let base = 25000usize;
        let t1 = t_us(&build(base), org) as f64;
        let t2 = t_us(&build(2 * base), org) as f64;
        let t4 = t_us(&build(4 * base), org) as f64;
        let t8 = t_us(&build(8 * base), org) as f64;
        println!(
            "{name:<18} n {:.1}ms -> 2n {:.1}ms ({:.2}x) -> 4n {:.1}ms ({:.2}x) -> 8n {:.1}ms ({:.2}x)",
            t1 / 1000.0,
            t2 / 1000.0,
            t2 / t1.max(2000.0),
            t4 / 1000.0,
            t4 / t2.max(2000.0),
            t8 / 1000.0,
            t8 / t4.max(2000.0),
        );
    }
}

#[test]
fn probe() {
    for (name, org, base, build) in pairs() {
        let tn = t_us(&build(base), org) as f64;
        let t2n = t_us(&build(2 * base), org) as f64;
        let ratio = t2n / tn.max(20_000.0);
        let flag = if ratio >= 3.0 { " <== RED" } else { "" };
        println!(
            "{name:<22} n={base:<7} {:.1}ms -> 2n {:.1}ms = {ratio:.2}x{flag}",
            tn / 1000.0,
            t2n / 1000.0
        );
    }
}

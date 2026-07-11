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

fn work_v2(input: &str, fmt: &str) -> u64 {
    lsdoc::__scan_work_take(); // reset
    std::mem::forget(lsdoc::__parse_format_v2(input, fmt));
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

fn assert_linear_v2(label: &str, f: impl Fn(usize) -> String, base: usize, fmt: &str) {
    const CAP: f64 = 1.6;
    let q = |n: usize| -> f64 {
        let s = f(n);
        work_v2(&s, fmt).max(1) as f64 / s.len().max(1) as f64
    };
    let (q1, q2, q4) = (q(base), q(2 * base), q(4 * base));
    let (r1, r2) = (q2 / q1, q4 / q2);
    assert!(
        r1 < CAP && r2 < CAP,
        "{label} [{fmt} v2]: scan-work/byte {q1:.3} → {q2:.3} → {q4:.3} (base={base}), growth \
         {r1:.2}×/{r2:.2}× — >{CAP}× means a super-linear v2 scan"
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
fn v2_md_hr_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(match i % 3 {
            0 => "---\n",
            1 => "***\n",
            _ => "___\n",
        });
    }
    s
}
fn v2_org_hr_lines(n: usize) -> String {
    "-----\n".repeat(n)
}
fn v2_md_leaf_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("alpha beta\n"),
            1 => s.push('\n'),
            2 => s.push_str("---\n"),
            3 => s.push_str("plain *emph* [[Page]]\r\n"),
            _ => s.push_str("tail\r"),
        }
    }
    s
}
fn v2_org_leaf_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("alpha beta\n"),
            1 => s.push('\n'),
            2 => s.push_str("-----\n"),
            3 => s.push_str("plain *bold* [[Page]]\r\n"),
            _ => s.push_str("---\r"),
        }
    }
    s
}
fn v2_directive_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 4 {
            0 => s.push_str("#+TITLE: alpha\n"),
            1 => s.push_str("  #+A B: \t\x1av  \n\n"),
            2 => s.push_str("plain text\n"),
            _ => s.push_str("tail text\n"),
        }
    }
    s
}
fn v2_front_matter_directives(n: usize) -> String {
    let mut s = String::from("---\n");
    for i in 0..n {
        writeln!(&mut s, "key{i}: value {i}").unwrap();
    }
    s.push_str("---\nplain\n");
    s
}
fn v2_md_comment_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("[//]: # alpha\n"),
            1 => s.push_str("  [//]: #   beta  \n\n"),
            2 => s.push_str("plain text\n"),
            3 => s.push_str("---\n"),
            _ => s.push_str("[//]: # tail"),
        }
    }
    s
}
fn v2_org_comment_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("# alpha\n"),
            1 => s.push_str("  #   beta  \n\n"),
            2 => s.push_str("plain text\n"),
            3 => s.push_str("-----\n"),
            _ => s.push_str("#c\n"),
        }
    }
    s
}
fn v2_md_heading_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 47 {
            0 => s.push_str("# alpha\n"),
            1 => s.push_str("  ## beta\n"),
            2 => s.push_str("- bullet\n"),
            3 => s.push_str("- ## Section\n"),
            4 => s.push_str("# \n"),
            5 => s.push_str("plain text\n"),
            6 => s.push_str("# ---\n"),
            7 => s.push_str("# #+BEGIN_EXPORT html\nx\n#+END_EXPORT\n"),
            8 => s.push_str("- #+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n"),
            9 => s.push_str("- ```\nx\n```\n"),
            10 => s.push_str("# $$x$$\n"),
            11 => s.push_str("# \\begin{eq}x\\end{eq}\n"),
            12 => s.push_str("# $$x$$tail\n"),
            13 => s.push_str("# \\begin{eq}x\\end{eq}tail\n"),
            14 => s.push_str("# <div>x</div>\n"),
            15 => s.push_str("# <div>x</div>tail\n"),
            16 => s.push_str("# <div>x</div><span>y</span>\n"),
            17 => s.push_str("# [:div]\n"),
            18 => s.push_str("# [:div][:span]\n"),
            19 => s.push_str("# | a |\n"),
            20 => s.push_str("# | a |\n|---|\n| b |\n"),
            21 => s.push_str("# $$x$$#+BEGIN_SRC\nx\n#+END_SRC\n"),
            22 => s.push_str("# [^1]: body\n"),
            23 => s.push_str("# > quote\n"),
            24 => s.push_str("# $$x$$> quote\n"),
            25 => s.push_str("# key:: value\n"),
            26 => s.push_str("# $$x$$key:: value\n"),
            27 => s.push_str("# #+TITLE: x\n"),
            28 => s.push_str("# #+BEGIN_NOTE\n#+END_NOTE\n"),
            29 => s.push_str("# <foo>x</foo>\n"),
            30 => s.push_str("# $$unclosed\n"),
            31 => s.push_str("# ```\nx\n"),
            32 => s.push_str("# > - x\n"),
            33 => s.push_str("# \\begin{}x\\end{}\n"),
            34 => s.push_str("# #+BEGIN_SRC\nx\n"),
            35 => s.push_str("# | a | b\n"),
            36 => s.push_str("# |---\n"),
            37 => s.push_str("# a::b\n"),
            38 => s.push_str("# #+END_NOTE\n"),
            39 => s.push_str("- # [^1]: b\n"),
            40 => s.push_str("#\t:\n"),
            41 => s.push_str("# :END:\n"),
            42 => s.push_str("# :PROPERTIES:\n"),
            43 => s.push_str("# h\rplain\n"),
            44 => s.push_str("# $$x$$\rplain\n"),
            _ => s.push_str("---\n"),
        }
    }
    s
}
fn v2_org_heading_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 37 {
            0 => s.push_str("* alpha\n"),
            1 => s.push_str("** TODO [#A] beta :tag:\n"),
            2 => s.push_str("* \n"),
            3 => s.push_str("plain text\n"),
            4 => s.push_str("-----\n"),
            5 => s.push_str("* -----\n"),
            6 => s.push_str("* #+BEGIN_SRC\nx\n#+END_SRC\n"),
            7 => s.push_str("* #+BEGIN_EXPORT html\nx\n#+END_EXPORT\n"),
            8 => s.push_str("* ```\nx\n```\n"),
            9 => s.push_str("* $$x$$\n"),
            10 => s.push_str("* \\begin{eq}x\\end{eq}\n"),
            11 => s.push_str("* $$x$$tail\n"),
            12 => s.push_str("* \\begin{eq}x\\end{eq}tail\n"),
            13 => s.push_str("* <div>x</div>\n"),
            14 => s.push_str("* <div>x</div>tail\n"),
            15 => s.push_str("* <div>x</div><span>y</span>\n"),
            16 => s.push_str("* [:div]\n"),
            17 => s.push_str("* [:div][:span]\n"),
            18 => s.push_str("* | a |\n"),
            19 => s.push_str("* $$x$$#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n"),
            20 => s.push_str("* [fn:1] body\n"),
            21 => s.push_str("* > quote\n"),
            22 => s.push_str("* $$x$$> quote\n"),
            23 => s.push_str("* :PROPERTIES:\n:k: v\n:END:\n"),
            24 => s.push_str("* $$x$$:PROPERTIES:\n:END:\n"),
            25 => s.push_str("* #+TITLE: x\n"),
            26 => s.push_str("* #+BEGIN_NOTE\n#+END_NOTE\n"),
            27 => s.push_str("* <foo>x</foo>\n"),
            28 => s.push_str("* $$unclosed\n"),
            29 => s.push_str("* > - x\n"),
            30 => s.push_str("* \\begin{}x\\end{}\n"),
            31 => s.push_str("* #+BEGIN_EXAMPLE\nx\n"),
            32 => s.push_str("* | a | b\n"),
            33 => s.push_str("* |---\n"),
            34 => s.push_str("* h\rplain\n"),
            35 => s.push_str("* $$x$$\rplain\n"),
            _ => s.push_str("# alpha\n"),
        }
    }
    s
}
fn v2_md_property_drawer_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 40 {
            0 => s.push_str("key:: value\n"),
            1 => s.push_str("a:: 1\nb:: 2\n#+c: 3\n"),
            2 => s.push_str(":PROPERTIES:\n:a: 1\n:END:\n"),
            3 => s.push_str(":PROPERTIES:\n:a: 1\n:END:tail\n"),
            4 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<div>x</div>\n"),
            5 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_SRC\nx\n#+END_SRC\n"),
            6 => s.push_str(":PROPERTIES:\n:a: 1\n:END:[^1]: body\n"),
            7 => s.push_str(":PROPERTIES:\n:a: 1\n:END:> quote\n"),
            8 => s.push_str(":PROPERTIES:\n:a: 1\n:END:$$x$$\n"),
            9 => s.push_str(":LOGBOOK:\nCLOCK: x\n:END:\n"),
            10 => s.push_str("#+BEGIN_x: no\n"),
            11 => s.push_str(":LOGBOOK:\nCLOCK: x\n"),
            12 => s.push_str("key:: v\rtail\n"),
            13 => s.push_str("a:: 1\nb:: 2\rtail\n"),
            14 => s.push_str("a:: 1\n#+b: 2\rtail\n"),
            15 => s.push_str("#+BEGIN_x: no\rtail\n"),
            16 => s.push_str(":PROPERTIES:\n:k: v\r:END:\n"),
            17 => s.push_str(":PROPERTIES:\n:a: 1\n:END:[//]: # c\nnext\n"),
            18 => s.push_str(":PROPERTIES:\n:a: 1\n:END:# h\n"),
            19 => s.push_str(":PROPERTIES:\n:a: 1\n:END:- x\n"),
            20 => s.push_str(":PROPERTIES:\n:a: 1\n:END:key:: v\rtail\n"),
            21 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<foo>x</foo>\n"),
            22 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<br />\n"),
            23 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<div>x\n"),
            24 => s.push_str(":PROPERTIES:\n:a: 1\n:END:$$unclosed\n"),
            25 => s.push_str(":PROPERTIES:\n:a: 1\n:END:```\nx\n"),
            26 => s.push_str(":PROPERTIES:\n:a: 1\n:END:> - x\n"),
            27 => s.push_str(":PROPERTIES:\n:a: 1\n:END:\\begin{}x\\end{}\n"),
            28 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_SRC\nx\n"),
            29 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_NOTE\n#+END_NOTE\n"),
            30 => s.push_str(":PROPERTIES:\n:a: 1\n:END:| a | b\n"),
            31 => s.push_str(":PROPERTIES:\n:a: 1\n:END:|---\n"),
            32 => s.push_str(":PROPERTIES:\n:a: 1\n:END:1. \n"),
            33 => s.push_str(":PROPERTIES:\n:a: 1\n:END:+ \n"),
            34 => s.push_str(":PROPERTIES:\n:a: 1\n:END:# <foo>x</foo>\n"),
            35 => s.push_str(":PROPERTIES:\n:a: 1\n:END:# | a | b\n"),
            36 => s.push_str(":PROPERTIES:\n:a: 1\n:END:a::b\n"),
            37 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+END_NOTE\n"),
            38 => s.push_str(":PROPERTIES:\n:a: 1\n:END::END:\n"),
            39 => s.push_str("a::b mid line\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_property_drawer_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 33 {
            0 => s.push_str(":PROPERTIES:\n:key: value\n:END:\n"),
            1 => s.push_str(":PROPERTIES:\n:key: value\n:END:\n\n#+NEXT: ok\n"),
            2 => s.push_str(":PROPERTIES:\n:a: 1\n:END:tail\n"),
            3 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n"),
            4 => s.push_str(":PROPERTIES:\n:a: 1\n:END:[fn:1] body\n"),
            5 => s.push_str(":PROPERTIES:\n:a: 1\n:END:> quote\n"),
            6 => s.push_str(":PROPERTIES:\n:a: 1\n:END:$$x$$\n"),
            7 => s.push_str(":LOGBOOK:\nCLOCK: x\n:END:\n"),
            8 => s.push_str("* heading\n"),
            9 => s.push_str("#+Begin_x: yes\n"),
            10 => s.push_str(":PROPERTIES:\n:k: v\r:END:\n"),
            11 => s.push_str(":LOGBOOK:\nx\r:END:\n"),
            12 => s.push_str(":PROPERTIES:\n:a: 1\n:END:\n:PROPERTIES:\r:k: v\r:END:\n"),
            13 => s.push_str("#+b: 2\rtail\n"),
            14 => s.push_str(":PROPERTIES:\n:a: 1\n:END:# c\n"),
            15 => s.push_str(":PROPERTIES:\n:a: 1\n:END:* h\n"),
            16 => s.push_str(":PROPERTIES:\n:a: 1\n:END:- x\n"),
            17 => s.push_str(":PROPERTIES:\n:a: 1\n:END:: x\n"),
            18 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<foo>x</foo>\n"),
            19 => s.push_str(":PROPERTIES:\n:a: 1\n:END:<div>x\n"),
            20 => s.push_str(":PROPERTIES:\n:a: 1\n:END:$$unclosed\n"),
            21 => s.push_str(":PROPERTIES:\n:a: 1\n:END:~~~\nx\n"),
            22 => s.push_str(":PROPERTIES:\n:a: 1\n:END:> - x\n"),
            23 => s.push_str(":PROPERTIES:\n:a: 1\n:END:\\begin{}x\\end{}\n"),
            24 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_EXAMPLE\nx\n"),
            25 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_NOTE\n#+END_NOTE\n"),
            26 => s.push_str(":PROPERTIES:\n:a: 1\n:END:| a | b\n"),
            27 => s.push_str(":PROPERTIES:\n:a: 1\n:END:|---\n"),
            28 => s.push_str(":PROPERTIES:\n:a: 1\n:END:1. \n"),
            29 => s.push_str(":PROPERTIES:\n:a: 1\n:END:- \n"),
            30 => s.push_str(":PROPERTIES:\n:a: 1\n:END:* <foo>x</foo>\n"),
            31 => s.push_str(":PROPERTIES:\n:a: 1\n:END:* | a | b\n"),
            32 => s.push_str(":PROPERTIES:\n:a: 1\n:END:#+bad\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_displayed_math_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 31 {
            0 => s.push_str("$$x$$\n"),
            1 => s.push_str("$$a\nb$$\n"),
            2 => s.push_str("$$a$$ $$b$$\n"),
            3 => s.push_str("$$x$$tail\n"),
            4 => s.push_str("$$x$$---\n"),
            5 => s.push_str("$$x$$#+BEGIN_SRC\ncode\n#+END_SRC\n"),
            6 => s.push_str("$$x$$<div>x</div>\n"),
            7 => s.push_str("$$x$$\\begin{eq}a\\end{eq}\n"),
            8 => s.push_str("$$x$$| a |\n"),
            9 => s.push_str("$$x$$[^1]: body\n"),
            10 => s.push_str("$$x$$> quote\n"),
            11 => s.push_str("$$x$$key:: value\n"),
            12 => s.push_str("$$unclosed\n"),
            13 => s.push_str("$$x$$$$unclosed\n"),
            14 => s.push_str("$$x$$<foo>x</foo>\n"),
            15 => s.push_str("$$x$$```\ny\n"),
            16 => s.push_str("$$x$$> - x\n"),
            17 => s.push_str("$$x$$\\begin{}x\\end{}\n"),
            18 => s.push_str("$$x$$#+BEGIN_SRC\ny\n"),
            19 => s.push_str("$$x$$#+BEGIN_NOTE\n#+END_NOTE\n"),
            20 => s.push_str("$$x$$| a | b\n"),
            21 => s.push_str("$$x$$|---\n"),
            22 => s.push_str("$$x$$1. \n"),
            23 => s.push_str("$$x$$+ \n"),
            24 => s.push_str("$$x$$# <foo>x</foo>\n"),
            25 => s.push_str("$$x$$# | a | b\n"),
            26 => s.push_str("$$x$$a::b\n"),
            27 => s.push_str("$$x$$#+END_NOTE\n"),
            28 => s.push_str("$$x$$[^1]: b\n"),
            29 => s.push_str("$$x$$[:div]\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_displayed_math_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 29 {
            0 => s.push_str("$$x$$\n"),
            1 => s.push_str("$$a\nb$$\n"),
            2 => s.push_str("$$a$$ $$b$$\n"),
            3 => s.push_str("$$x$$tail\n"),
            4 => s.push_str("$$x$$-----\n"),
            5 => s.push_str("$$x$$#+BEGIN_EXAMPLE\ncode\n#+END_EXAMPLE\n"),
            6 => s.push_str("$$x$$<div>x</div>\n"),
            7 => s.push_str("$$x$$\\begin{eq}a\\end{eq}\n"),
            8 => s.push_str("$$x$$[fn:1] body\n"),
            9 => s.push_str("$$x$$> quote\n"),
            10 => s.push_str("$$x$$:PROPERTIES:\n:END:\n"),
            11 => s.push_str("$$unclosed\n"),
            12 => s.push_str("$$x$$$$unclosed\n"),
            13 => s.push_str("$$x$$<foo>x</foo>\n"),
            14 => s.push_str("$$x$$~~~\ny\n"),
            15 => s.push_str("$$x$$> - x\n"),
            16 => s.push_str("$$x$$\\begin{}x\\end{}\n"),
            17 => s.push_str("$$x$$#+BEGIN_NOTE\n#+END_NOTE\n"),
            18 => s.push_str("$$x$$| a | b\n"),
            19 => s.push_str("$$x$$|---\n"),
            20 => s.push_str("$$x$$1. \n"),
            21 => s.push_str("$$x$$- \n"),
            22 => s.push_str("$$x$$* <foo>x</foo>\n"),
            23 => s.push_str("$$x$$* | a | b\n"),
            24 => s.push_str("$$x$$#+bad\n"),
            25 => s.push_str("$$x$$:END:\n"),
            26 => s.push_str("$$x$$:PROPERTIES:\n"),
            27 => s.push_str("$$x$$[:div]\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_latex_env_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 8 {
            0 => s.push_str("\\begin{eq}x\\end{eq}\n"),
            1 => s.push_str("\\begin{eq}\nx\n\\end{eq}\n"),
            2 => s.push_str("\\begin{Eq}x\\END{eq}tail\n"),
            3 => s.push_str("\\begin{e q} \n x \n\\end{e q}\n"),
            4 => s.push_str("\\begin{eq}x\\end{eq}\n---\n"),
            5 => s.push_str("\\begin{}x\\end{}\n"),
            6 => s.push_str("\\begin{eq\nx\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_latex_env_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 6 {
            0 => s.push_str("\\begin{eq}x\\end{eq}\n"),
            1 => s.push_str("\\begin{eq}\nx\n\\end{eq}\n"),
            2 => s.push_str("\\begin{Eq}x\\END{eq}tail\n"),
            3 => s.push_str("\\begin{e q} \n x \n\\end{e q}\n"),
            4 => s.push_str("\\begin{eq}x\\end{eq}\n-----\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_latex_long_name_backslashes(n: usize) -> String {
    let name = "A".repeat(n);
    let body = "\\".repeat(n);
    format!("\\begin{{{name}}}{body}\\end{{{name}}}")
}
fn v2_md_table_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 7 {
            0 => s.push_str("| a | b |\n| c | d |\n"),
            1 => s.push_str("| a | b |\n|---|---|\n| 1 | 2 |\n"),
            2 => s.push_str("|---\n| a |\n"),
            3 => s.push_str("| a |\rplain\n"),
            4 => s.push_str("# \n| a |\n"),
            5 => s.push_str("| a | b\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_table_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 6 {
            0 => s.push_str("| a | b |\n|---+---|\n| 1 | 2 |\n"),
            1 => s.push_str("| a |\n#+TBLFM: x\nplain\n"),
            2 => s.push_str("| h | h |\n|---+---|\n| / | > |\n| a | b |\n"),
            3 => s.push_str("* \n| a |\n"),
            4 => s.push_str("|---\n| a |\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_fence_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 9 {
            0 => s.push_str("```js\nx\n```\n"),
            1 => s.push_str("```\nx\n```\n\nplain\n"),
            2 => s.push_str("````clj\n(code)\n~~~\n"),
            3 => s.push_str("  ``` rust opts\nfn main() {}\n```\n"),
            4 => s.push_str("# \n```\nx\n```\n"),
            5 => s.push_str("```\nx\n"),
            6 => s.push_str("```\r\nx\r\n```\n"),
            7 => s.push_str("```\rx\r```\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_fence_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 9 {
            0 => s.push_str("```js\nx\n```\n"),
            1 => s.push_str("```\nx\n```\n\nplain\n"),
            2 => s.push_str("````clj\n(code)\n~~~\n"),
            3 => s.push_str("  ``` rust opts\nfn main() {}\n```\n"),
            4 => s.push_str("* \n```\nx\n```\n"),
            5 => s.push_str("~~~\nx\n"),
            6 => s.push_str("```\r\nx\r\n```\n"),
            7 => s.push_str("```\rx\r```\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_src_example_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 10 {
            0 => s.push_str("#+BEGIN_SRC clojure\n  (x)\n#+END_SRC\n"),
            1 => s.push_str("#+begin_src js\nx\n#+end_src\n"),
            2 => s.push_str("#+BEGIN_SRC\nx\n#+END_SRC_EXTRA\n"),
            3 => s.push_str("#+BEGIN_EXAMPLE\n  x\n#+END_EXAMPLE\n"),
            4 => s.push_str("# \n#+BEGIN_SRC\nx\n#+END_SRC\n"),
            5 => s.push_str("#+BEGIN_EXPORT html opt\n  x\n#+END_EXPORT\n"),
            6 => s.push_str("#+BEGIN_COMMENT\n  hidden\n#+END_COMMENT\n"),
            7 => s.push_str("#+BEGIN_SRC\nx\n"),
            8 => s.push_str("#+BEGIN_ \nx\n#+END_\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_src_example_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 10 {
            0 => s.push_str("#+BEGIN_SRC clojure\n  (x)\n#+END_SRC\n"),
            1 => s.push_str("#+begin_src js\nx\n#+end_src\n"),
            2 => s.push_str("#+BEGIN_SRC\nx\n#+END_SRC_EXTRA\n"),
            3 => s.push_str("#+BEGIN_EXAMPLE\n  x\n#+END_EXAMPLE\n"),
            4 => s.push_str("* \n#+BEGIN_SRC\nx\n#+END_SRC\n"),
            5 => s.push_str("#+BEGIN_EXPORT html opt\n  x\n#+END_EXPORT\n"),
            6 => s.push_str("#+BEGIN_COMMENT\n  hidden\n#+END_COMMENT\n"),
            7 => s.push_str("#+BEGIN_EXAMPLE\nx\n"),
            8 => s.push_str("#+BEGIN_ \nx\n#+END_\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_empty_callout_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("#+BEGIN_QUOTE\n#+END_QUOTE\n"),
            1 => s.push_str("#+BEGIN_NOTE\n#+END_NOTE\n"),
            2 => s.push_str("  #+begin_TIP arg\n#+end_TIP_EXTRA\n\n"),
            3 => s.push_str("#+BEGIN_QUOTE\r\n#+END_QUOTE\r\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_empty_callout_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => s.push_str("#+BEGIN_QUOTE\n#+END_QUOTE\n"),
            1 => s.push_str("#+BEGIN_NOTE\n#+END_NOTE\n"),
            2 => s.push_str("  #+begin_TIP arg\n#+end_TIP_EXTRA\n\n"),
            3 => s.push_str("#+BEGIN_QUOTE\r\n#+END_QUOTE\r\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_plain_callout_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 17 {
            0 => s.push_str("#+BEGIN_QUOTE\nplain body\n#+END_QUOTE\n"),
            1 => s.push_str("#+BEGIN_NOTE\n  stripped\n  body\n#+END_NOTE\n"),
            2 => s.push_str("#+BEGIN_TIP\n*bold* [[Page]]\n#+END_TIP\n"),
            3 => s.push_str("#+BEGIN_QUOTE\nline one\nline two\n#+END_QUOTE\n\n"),
            4 => s.push_str("#+BEGIN_FOO\n| a | b |\n#+END_FOO\n"),
            5 => s.push_str("#+BEGIN_FOO\n```\ncode\n```\n#+END_FOO\n"),
            6 => s.push_str("#+BEGIN_FOO\nintro\n```\ncode\n```\n#+END_FOO\n"),
            7 => s.push_str("#+BEGIN_FOO\n  text\n  \\begin{eq}\n  x\n  \\end{eq}\n#+END_FOO\n"),
            8 => s.push_str("#+BEGIN_FOO\n# h\n#+END_FOO\n"),
            9 => s.push_str("#+BEGIN_FOO\n- x\n#+END_FOO\n"),
            10 => s.push_str("#+BEGIN_FOO\n[^1]: body\n#+END_FOO\n"),
            11 => s.push_str("#+BEGIN_FOO\n:NAME:\nx\n:END:\n#+END_FOO\n"),
            12 => s.push_str("#+BEGIN_FOO\n:PROPERTIES:\n:k: v\n:END:\n#+END_FOO\n"),
            13 => s.push_str("#+BEGIN_FOO\nk:: v\n#+b: 2\n#+END_FOO\n"),
            14 => s.push_str("#+BEGIN_FOO\n:PROPERTIES:\n:k: v\n:END:\n#+b: 2\n#+END_FOO\n"),
            15 => s.push_str("#+BEGIN_FOO\n>>>>key:: val\n#+END_FOO\n"),
            16 => {
                s.push_str("#+BEGIN_QUOTE\n- #+BEGIN_NOTE\n  nested\n  #+END_NOTE\n#+END_QUOTE\n")
            }
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_plain_callout_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 24 {
            0 => s.push_str("#+BEGIN_QUOTE\nplain body\n#+END_QUOTE\n"),
            1 => s.push_str("#+BEGIN_NOTE\n  stripped\n  body\n#+END_NOTE\n"),
            2 => s.push_str("#+BEGIN_TIP\n/bold/ [[Page]]\n#+END_TIP\n"),
            3 => s.push_str("#+BEGIN_QUOTE\nline one\nline two\n#+END_QUOTE\n\n"),
            4 => s.push_str("#+BEGIN_FOO\n| a | b |\n#+END_FOO\n"),
            5 => s.push_str("#+BEGIN_FOO\n```\ncode\n```\n#+END_FOO\n"),
            6 => s.push_str("#+BEGIN_FOO\nintro\n```\ncode\n```\n#+END_FOO\n"),
            7 => s.push_str("#+BEGIN_FOO\n  text\n  \\begin{eq}\n  x\n  \\end{eq}\n#+END_FOO\n"),
            8 => s.push_str("#+BEGIN_FOO\n* h\n#+END_FOO\n"),
            9 => s.push_str("#+BEGIN_FOO\n[fn:1] body\n#+END_FOO\n"),
            10 => s.push_str("#+BEGIN_FOO\n:PROPERTIES:\n:k: v\n:END:\n#+END_FOO\n"),
            11 => s.push_str("#+BEGIN_FOO\n:LOGBOOK:\nx\n:END:\n#+END_FOO\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_fixed_width_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 7 {
            0 => s.push_str(": alpha\n"),
            1 => s.push_str("  :    beta  \n"),
            2 => s.push_str(":PROPERTIES:\n"),
            3 => s.push_str(":LOGBOOK:\r"),
            4 => s.push_str("* \n: under heading\n"),
            5 => s.push_str(":LOGBOOK:\nCLOCK: x\n:END:\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_md_footnote_def_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 5 {
            0 => writeln!(&mut s, "[^a{i}]: body {i}").unwrap(),
            1 => s.push_str("[^b]: body\ncont line\n"),
            2 => s.push_str("[^c]: body\n\nplain\n"),
            3 => s.push_str("# \n[^d]: ab\n"),
            _ => s.push_str("[^e]: body\n---\n"),
        }
    }
    s
}
fn v2_org_footnote_def_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 6 {
            0 => writeln!(&mut s, "[fn:a{i}] body {i}").unwrap(),
            1 => s.push_str("[fn:b] body\ncont line\n"),
            2 => s.push_str("[fn:c] body\n\nplain\n"),
            3 => s.push_str("* \n[fn:d] ab\n"),
            4 => s.push_str("[fn:e] body\n-----\n"),
            _ => s.push_str("[fn:f] body\n  - x\n"),
        }
    }
    s
}
fn v2_md_definition_list_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 6 {
            0 => writeln!(&mut s, "term{i}\n: definition {i}").unwrap(),
            1 => s.push_str("term\n: one\n: two\n"),
            2 => s.push_str("term\n: one\ncontinued\n"),
            3 => s.push_str("term\n: one\n\n# h\n: two\n"),
            4 => s.push_str("---\n: rule term\n"),
            _ => s.push_str("plain\nterm\n: definition\n"),
        }
    }
    s
}
fn v2_md_regular_list_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 24 {
            0 => writeln!(&mut s, "* item {i}").unwrap(),
            1 => s.push_str("+ [x] done\n+ [ ] todo\n"),
            2 => s.push_str("1. one\n2. two\n"),
            3 => s.push_str("* parent\n  * child\n  * child2\n"),
            4 => s.push_str("* folded\n  continuation\n"),
            5 => s.push_str("* term ::\n"),
            6 => s.push_str("* cr\rplain\n"),
            7 => s.push_str("* before\n  ---\n"),
            8 => s.push_str("* | a | b |\n  | c | d |\n"),
            9 => s.push_str("* ```\n  code\n  ```\n"),
            10 => s.push_str("* <div>raw</div>\n"),
            11 => s.push_str("* $$x$$\n"),
            12 => s.push_str("* \\begin{eq}x\\end{eq}\n"),
            13 => s.push_str("* before\n  # heading text\n"),
            14 => s.push_str("* before\n  [^1]: body\n"),
            15 => s.push_str("* before\n  key:: value\n"),
            16 => s.push_str("* before\n  :PROPERTIES:\n  :k: v\n  :END:\n"),
            17 => s.push_str("* before\n  12bad\n"),
            18 => s.push_str("* before\n  #+A: b\n"),
            19 => s.push_str("* before\n  term\n  : def\n"),
            20 => s.push_str("1. \n"),
            21 => s.push_str("+ #+RESULTS:\n"),
            22 => s.push_str("+ #+RESULTS:x\n"),
            23 => s.push_str("+ #+RESULTS:\n  next\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_org_regular_list_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 22 {
            0 => writeln!(&mut s, "- item {i}").unwrap(),
            1 => s.push_str("+ [x] done\n+ [ ] todo\n"),
            2 => s.push_str("1. one\n2. two\n"),
            3 => s.push_str("- parent\n  + child\n  + child2\n"),
            4 => s.push_str("- parent\n  * child\n"),
            5 => s.push_str("- folded\n  continuation\n"),
            6 => s.push_str("- cr\rplain\n"),
            7 => s.push_str("- before\n  -----\n"),
            8 => s.push_str("- | a | b |\n  | c | d |\n"),
            9 => s.push_str("- #+BEGIN_SRC\n  code\n  #+END_SRC\n"),
            10 => s.push_str("- * heading text\n"),
            11 => s.push_str("- before\n  [fn:1] body\n"),
            12 => s.push_str("- before\n  :PROPERTIES:\n  :k: v\n  :END:\n"),
            13 => s.push_str("- before\n  :LOGBOOK:\n  x\n  :END:\n"),
            14 => s.push_str("- before\n  12bad\n"),
            15 => s.push_str("- before\n  #+A: b\n"),
            16 => s.push_str("+ before\n  + child\n    - reject\n"),
            17 => s.push_str("- - x\n"),
            18 => s.push_str("- #+RESULTS:\n"),
            19 => s.push_str("- #+RESULTS:x\n"),
            20 => s.push_str("- #+RESULTS:\n  next\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_markdown_blockquote_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 10 {
            0 => writeln!(&mut s, "> quote {i}").unwrap(),
            1 => s.push_str("> x\ny\n"),
            2 => s.push_str("> > nested-looking\n"),
            3 => s.push_str("> + item\n"),
            4 => s.push_str("> # suppressed\n"),
            5 => s.push_str("> - rejected\n"),
            6 => s.push_str("> cr\rplain\n"),
            7 => s.push_str("> :LOGBOOK:\n> x\n> :END:\n"),
            8 => s.push_str("> q\n+ #+RESULTS:\n"),
            _ => s.push_str("plain text\n"),
        }
    }
    s
}
fn v2_markdown_empty_quote_blank_continuations(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(&mut s, "> quote {i}\n>\n\nlazy {i}\n- stop {i}").unwrap();
    }
    s
}
fn markdown_balanced_label_after_eol(n: usize) -> String {
    let mut s = String::from("://");
    for _ in 0..n {
        s.push_str("[[]\n]() ");
    }
    s
}
fn v2_md_hiccup_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 43 {
            0 => s.push_str("[:div]\n"),
            1 => s.push_str("  [:span]tail\n"),
            2 => s.push_str("[:div][:span]\n"),
            3 => s.push_str("[:div]\n\nplain\n"),
            4 => s.push_str("[:div]---\n"),
            5 => s.push_str("[:nope]\n: def\n"),
            6 => s.push_str("[:div]#+BEGIN_SRC\nx\n#+END_SRC\n"),
            7 => s.push_str("[:div]#+BEGIN_EXPORT html\nx\n#+END_EXPORT\n"),
            8 => s.push_str("[:div]```\nx\n```\n"),
            9 => s.push_str("[:div]$$x$$\n"),
            10 => s.push_str("[:div]\\begin{eq}x\\end{eq}\n"),
            11 => s.push_str("[:div]$$x$$tail\n"),
            12 => s.push_str("[:div]\\begin{eq}x\\end{eq}tail\n"),
            13 => s.push_str("[:div]<kbd>x</kbd>\n"),
            14 => s.push_str("[:div]<kbd>x</kbd>tail\n"),
            15 => s.push_str("[:div]<kbd>x</kbd><span>y</span>\n"),
            16 => s.push_str("[:div]$$x$$#+BEGIN_SRC\nx\n#+END_SRC\n"),
            17 => s.push_str("[:div][^1]: body\n"),
            18 => s.push_str("[:div]> quote\n"),
            19 => s.push_str("[:div]$$x$$> quote\n"),
            20 => s.push_str("[:div]key:: value\n"),
            21 => s.push_str("[:div]$$x$$key:: value\n"),
            22 => s.push_str("[:div]<foo>x</foo>\n"),
            23 => s.push_str("[:div]$$unclosed\n"),
            24 => s.push_str("[:div]```\nx\n"),
            25 => s.push_str("[:div]> - x\n"),
            26 => s.push_str("[:div]\\begin{}x\\end{}\n"),
            27 => s.push_str("[:div]#+BEGIN_SRC\nx\n"),
            28 => s.push_str("[:div][//]: # c\nnext\n"),
            29 => s.push_str("[:div]# h\n"),
            30 => s.push_str("[:div]- x\n"),
            31 => s.push_str("[:div]#+BEGIN_NOTE\n#+END_NOTE\n"),
            32 => s.push_str("[:div]| a | b\n"),
            33 => s.push_str("[:div]|---\n"),
            34 => s.push_str("[:div]1. \n"),
            35 => s.push_str("[:div]+ \n"),
            36 => s.push_str("[:div]# <foo>x</foo>\n"),
            37 => s.push_str("[:div]# | a | b\n"),
            38 => s.push_str("[:div]a::b\n"),
            39 => s.push_str("[:div]#+END_NOTE\n"),
            40 => s.push_str("[:div][^1]: b\n"),
            41 => s.push_str("[:div]:END:\n"),
            42 => s.push_str("[:div]:PROPERTIES:\n"),
            _ => writeln!(&mut s, "[:div [:span {i}]]").unwrap(),
        }
    }
    s
}
fn v2_org_hiccup_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 39 {
            0 => s.push_str("[:div]\n"),
            1 => s.push_str("  [:span]tail\n"),
            2 => s.push_str("[:div][:span]\n"),
            3 => s.push_str("[:div]\n\nplain\n"),
            4 => s.push_str("[:div]-----\n"),
            5 => s.push_str("[:div]\n: def\n"),
            6 => s.push_str("[:div]#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n"),
            7 => s.push_str("[:div]#+BEGIN_COMMENT\nx\n#+END_COMMENT\n"),
            8 => s.push_str("[:div]~~~clj\nx\n```\n"),
            9 => s.push_str("[:div]$$x$$\n"),
            10 => s.push_str("[:div]\\begin{eq}x\\end{eq}\n"),
            11 => s.push_str("[:div]$$x$$tail\n"),
            12 => s.push_str("[:div]\\begin{eq}x\\end{eq}tail\n"),
            13 => s.push_str("[:div]<kbd>x</kbd>\n"),
            14 => s.push_str("[:div]<kbd>x</kbd>tail\n"),
            15 => s.push_str("[:div]<kbd>x</kbd><span>y</span>\n"),
            16 => s.push_str("[:div]$$x$$#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n"),
            17 => s.push_str("[:div][fn:1] body\n"),
            18 => s.push_str("[:div]> quote\n"),
            19 => s.push_str("[:div]$$x$$> quote\n"),
            20 => s.push_str("[:div]:PROPERTIES:\n:END:\n"),
            21 => s.push_str("[:div]$$x$$:PROPERTIES:\n:END:\n"),
            22 => s.push_str("[:div]- x\n"),
            23 => s.push_str("[:div]<foo>x</foo>\n"),
            24 => s.push_str("[:div]$$unclosed\n"),
            25 => s.push_str("[:div]~~~\nx\n"),
            26 => s.push_str("[:div]> - x\n"),
            27 => s.push_str("[:div]\\begin{}x\\end{}\n"),
            28 => s.push_str("[:div]#+BEGIN_EXAMPLE\nx\n"),
            29 => s.push_str("[:div]# c\n"),
            30 => s.push_str("[:div]: x\n"),
            31 => s.push_str("[:div]#+BEGIN_NOTE\n#+END_NOTE\n"),
            32 => s.push_str("[:div]| a | b\n"),
            33 => s.push_str("[:div]|---\n"),
            34 => s.push_str("[:div]1. \n"),
            35 => s.push_str("[:div]- \n"),
            36 => s.push_str("[:div]* <foo>x</foo>\n"),
            37 => s.push_str("[:div]* | a | b\n"),
            38 => s.push_str("[:div]#+bad\n"),
            _ => writeln!(&mut s, "[:div [:span {i}]]").unwrap(),
        }
    }
    s
}
fn v2_md_raw_html_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 29 {
            0 => s.push_str("<div>x</div>\n"),
            1 => s.push_str("  <span>tail</span>x\n"),
            2 => s.push_str("<div>x</div><span>y</span>\n"),
            3 => s.push_str("<div>\na\n</div>\n\nplain\n"),
            4 => s.push_str("<foo>x</foo>\n: def\n"),
            5 => s.push_str("<b>a\nb</b>\n"),
            6 => s.push_str("<div>x</div><foo>x</foo>\n"),
            7 => s.push_str("<div>x</div>$$unclosed\n"),
            8 => s.push_str("<div>x</div>```\nx\n"),
            9 => s.push_str("<div>x</div>> - x\n"),
            10 => s.push_str("<div>x</div>\\begin{}x\\end{}\n"),
            11 => s.push_str("<div>x</div>#+BEGIN_SRC\nx\n"),
            12 => s.push_str("<div>x</div>[//]: # c\nnext\n"),
            13 => s.push_str("<div>x</div># h\n"),
            14 => s.push_str("<div>x</div>- x\n"),
            15 => s.push_str("<div>x</div>#+BEGIN_NOTE\n#+END_NOTE\n"),
            16 => s.push_str("<div>x</div>| a | b\n"),
            17 => s.push_str("<div>x</div>|---\n"),
            18 => s.push_str("<div>x</div>1. \n"),
            19 => s.push_str("<div>x</div>+ \n"),
            20 => s.push_str("<div>x</div># <foo>x</foo>\n"),
            21 => s.push_str("<div>x</div># | a | b\n"),
            22 => s.push_str("<div>x</div>a::b\n"),
            23 => s.push_str("<div>x</div>#+END_NOTE\n"),
            24 => s.push_str("<div>x</div>[^1]: b\n"),
            25 => s.push_str("<div>x</div>:END:\n"),
            26 => s.push_str("<div>x</div>:PROPERTIES:\n"),
            27 => s.push_str("<div>x</div>[:div]\n"),
            _ => writeln!(&mut s, "<div><span>{i}</span></div>").unwrap(),
        }
    }
    s
}
fn v2_org_raw_html_lines(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        match i % 24 {
            0 => s.push_str("<div>x</div>\n"),
            1 => s.push_str("  <span>tail</span>x\n"),
            2 => s.push_str("<div>x</div><span>y</span>\n"),
            3 => s.push_str("<div>\na\n</div>\n\nplain\n"),
            4 => s.push_str("<foo>x</foo>\n: def\n"),
            5 => s.push_str("<b>a\nb</b>\n"),
            6 => s.push_str("<div>x</div><foo>x</foo>\n"),
            7 => s.push_str("<div>x</div>$$unclosed\n"),
            8 => s.push_str("<div>x</div>~~~\nx\n"),
            9 => s.push_str("<div>x</div>> - x\n"),
            10 => s.push_str("<div>x</div>\\begin{}x\\end{}\n"),
            11 => s.push_str("<div>x</div>#+BEGIN_EXAMPLE\nx\n"),
            12 => s.push_str("<div>x</div># c\n"),
            13 => s.push_str("<div>x</div>* h\n"),
            14 => s.push_str("<div>x</div>: x\n"),
            15 => s.push_str("<div>x</div>#+BEGIN_NOTE\n#+END_NOTE\n"),
            16 => s.push_str("<div>x</div>| a | b\n"),
            17 => s.push_str("<div>x</div>|---\n"),
            18 => s.push_str("<div>x</div>1. \n"),
            19 => s.push_str("<div>x</div>- \n"),
            20 => s.push_str("<div>x</div>* <foo>x</foo>\n"),
            21 => s.push_str("<div>x</div>* | a | b\n"),
            22 => s.push_str("<div>x</div>#+bad\n"),
            23 => s.push_str("<div>x</div>[:div]\n"),
            _ => writeln!(&mut s, "<div><span>{i}</span></div>").unwrap(),
        }
    }
    s
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
        assert_linear(
            "display_math_unclosed_tail",
            display_math_unclosed_tail,
            3000,
            "md",
        );
        assert_linear(
            "display_math_unclosed_tail",
            display_math_unclosed_tail,
            3000,
            "org",
        );
        assert_linear("raw_html_adjacent", raw_html_adjacent, 3000, "md");
        assert_linear("raw_html_adjacent", raw_html_adjacent, 3000, "org");
        assert_linear("raw_html_unclosed_tail", raw_html_unclosed_tail, 3000, "md");
        assert_linear(
            "raw_html_unclosed_tail",
            raw_html_unclosed_tail,
            3000,
            "org",
        );
        assert_linear(
            "raw_html_repeated_unclosed",
            raw_html_repeated_unclosed,
            3000,
            "md",
        );
        assert_linear(
            "raw_html_repeated_unclosed",
            raw_html_repeated_unclosed,
            3000,
            "org",
        );
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
        assert_linear(
            "nested_callout_raw_html",
            nested_callout_raw_html,
            50,
            "org",
        );
        assert_linear(
            "nested_reuse_after_child_raw_html",
            nested_reuse_after_child_raw_html,
            50,
            "md",
        );
        assert_linear(
            "nested_reuse_after_child_raw_html",
            nested_reuse_after_child_raw_html,
            50,
            "org",
        );
        assert_linear(
            "raw_html_sibling_alternation",
            raw_html_sibling_alternation,
            50,
            "md",
        );
        assert_linear(
            "raw_html_sibling_alternation",
            raw_html_sibling_alternation,
            50,
            "org",
        );
        assert_linear("raw_html_sibling_pairs", raw_html_sibling_pairs, 500, "md");
        assert_linear("raw_html_sibling_pairs", raw_html_sibling_pairs, 500, "org");
        assert_linear(
            "raw_html_closes_then_opens",
            raw_html_closes_then_opens,
            500,
            "md",
        );
        assert_linear(
            "raw_html_closes_then_opens",
            raw_html_closes_then_opens,
            500,
            "org",
        );
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
        assert_linear(
            "email_domain_interleave",
            email_domain_interleave,
            6000,
            "md",
        );
        assert_linear(
            "org_email_domain_interleave",
            org_email_domain_interleave,
            6000,
            "org",
        );
        assert_linear(
            "timestamp_angle_interleave",
            timestamp_angle_interleave,
            6000,
            "md",
        );
        assert_linear(
            "org_timestamp_angle_interleave",
            org_timestamp_angle_interleave,
            6000,
            "org",
        );
        assert_linear("autolink_interleave", autolink_interleave, 6000, "md");
        assert_linear("macro_interleave", macro_interleave, 6000, "md");
        assert_linear("org_macro_interleave", org_macro_interleave, 6000, "org");
        assert_linear(
            "export_snippet_interleave",
            export_snippet_interleave,
            6000,
            "md",
        );
        assert_linear(
            "org_export_snippet_interleave",
            org_export_snippet_interleave,
            6000,
            "org",
        );
        assert_linear("blockref_interleave", blockref_interleave, 6000, "md");
        assert_linear(
            "org_blockref_interleave",
            org_blockref_interleave,
            6000,
            "org",
        );
        assert_linear(
            "raw_html_unbalanced_interleave",
            raw_html_unbalanced_interleave,
            6000,
            "md",
        );
        assert_linear("tag_hash_run", tag_hash_run, 6000, "md");
        assert_linear("tag_word_interleave", tag_word_interleave, 6000, "md");
        assert_linear("bare_url_interleave", bare_url_interleave, 6000, "md");
        assert_linear(
            "latex_dollar_failure_interleave",
            latex_dollar_failure_interleave,
            6000,
            "md",
        );
        assert_linear(
            "org_latex_dollar_failure_interleave",
            org_latex_dollar_failure_interleave,
            6000,
            "org",
        );
        assert_linear(
            "md_link_many_openers_one_close",
            md_link_many_openers_one_close,
            1000,
            "md",
        );
        assert_linear(
            "md_link_code_interleave",
            md_link_code_interleave,
            1000,
            "md",
        );
        assert_linear(
            "md_link_counter_nested_bracket",
            md_link_counter_nested_bracket,
            1000,
            "md",
        );
        assert_linear(
            "md_link_counter_escaped_bracket",
            md_link_counter_escaped_bracket,
            1000,
            "md",
        );
        assert_linear(
            "md_link_counter_page_ref",
            md_link_counter_page_ref,
            1000,
            "md",
        );
        assert_linear(
            "md_link_counter_code_interleave",
            md_link_counter_code_interleave,
            1000,
            "md",
        );
        assert_linear(
            "org_many_openers_one_close",
            org_many_openers_one_close,
            1000,
            "org",
        );
        assert_linear(
            "md_html_comment_unclosed",
            md_html_comment_unclosed,
            1000,
            "md",
        );
        assert_linear("md_html_comment_quote", md_html_comment_quote, 1000, "md");
        assert_linear(
            "md_html_comment_indented_begin",
            md_html_comment_indented_begin,
            1000,
            "md",
        );
        assert_linear(
            "md_html_comment_list_item",
            md_html_comment_list_item,
            1000,
            "md",
        );
        assert_linear(
            "org_fn_anon_newline_tail",
            org_fn_anon_newline_tail,
            1000,
            "org",
        );
        assert_linear(
            "org_fn_named_newline_tail",
            org_fn_named_newline_tail,
            1000,
            "org",
        );
        assert_linear(
            "org_fn_anon_no_close_line",
            org_fn_anon_no_close_line,
            1000,
            "org",
        );
        assert_linear(
            "org_fn_named_no_close_line",
            org_fn_named_no_close_line,
            1000,
            "org",
        );
        assert_linear(
            "org_link1_missing_label_close",
            org_link1_missing_label_close,
            1000,
            "org",
        );
        assert_linear(
            "org_link1_overlapping_chunks",
            org_link1_overlapping_chunks,
            1000,
            "org",
        );
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
        assert_linear(
            "f5_tag_pageref_fail_lf",
            f5_tag_pageref_fail_lf,
            1000,
            "org",
        );
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
        assert_linear(
            "f5_control_separate_tags",
            f5_control_separate_tags,
            1000,
            "md",
        );
        assert_linear(
            "f5_control_separate_tags",
            f5_control_separate_tags,
            1000,
            "org",
        );
        assert_linear(
            "f5_control_single_brackets",
            f5_control_single_brackets,
            1000,
            "md",
        );
        assert_linear(
            "f5_control_single_brackets",
            f5_control_single_brackets,
            1000,
            "org",
        );
        assert_linear("f5_control_plain_tag", f5_control_plain_tag, 1000, "md");
        assert_linear("f5_control_plain_tag", f5_control_plain_tag, 1000, "org");
        assert_linear(
            "audit3_md_footnote_no_close",
            audit3_md_footnote_no_close,
            1000,
            "md",
        );
        assert_linear(
            "audit3_md_footnote_success",
            audit3_md_footnote_success,
            1000,
            "md",
        );
        assert_linear(
            "audit3_org_target_no_close",
            audit3_org_target_no_close,
            1000,
            "org",
        );
        assert_linear(
            "audit3_org_radio_no_close",
            audit3_org_radio_no_close,
            1000,
            "org",
        );
        assert_linear(
            "audit3_org_target_success",
            audit3_org_target_success,
            1000,
            "org",
        );
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
        assert_linear(
            "f5_w_md_emphasis_reparse",
            f5_w_md_emphasis_reparse,
            1000,
            "md",
        );
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

#[test]
fn v2_leaf_complexity_gate() {
    assert_linear_v2("v2_md_hr_lines", v2_md_hr_lines, 1000, "md");
    assert_linear_v2("v2_org_hr_lines", v2_org_hr_lines, 1000, "org");
    assert_linear_v2("v2_md_leaf_lines", v2_md_leaf_lines, 1000, "md");
    assert_linear_v2("v2_org_leaf_lines", v2_org_leaf_lines, 1000, "org");
    assert_linear_v2("v2_directive_lines", v2_directive_lines, 1000, "md");
    assert_linear_v2("v2_directive_lines", v2_directive_lines, 1000, "org");
    assert_linear_v2(
        "v2_front_matter_directives",
        v2_front_matter_directives,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_front_matter_directives",
        v2_front_matter_directives,
        1000,
        "org",
    );
    assert_linear_v2("v2_md_comment_lines", v2_md_comment_lines, 1000, "md");
    assert_linear_v2("v2_org_comment_lines", v2_org_comment_lines, 1000, "org");
    assert_linear_v2("v2_md_heading_lines", v2_md_heading_lines, 1000, "md");
    assert_linear_v2("v2_org_heading_lines", v2_org_heading_lines, 1000, "org");
    assert_linear_v2(
        "v2_md_property_drawer_lines",
        v2_md_property_drawer_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_property_drawer_lines",
        v2_org_property_drawer_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_md_displayed_math_lines",
        v2_md_displayed_math_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_displayed_math_lines",
        v2_org_displayed_math_lines,
        1000,
        "org",
    );
    assert_linear_v2("v2_md_latex_env_lines", v2_md_latex_env_lines, 1000, "md");
    assert_linear_v2(
        "v2_org_latex_env_lines",
        v2_org_latex_env_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_latex_long_name_backslashes",
        v2_latex_long_name_backslashes,
        1000,
        "md",
    );
    assert_linear_v2("v2_md_table_lines", v2_md_table_lines, 1000, "md");
    assert_linear_v2("v2_org_table_lines", v2_org_table_lines, 1000, "org");
    assert_linear_v2("v2_md_fence_lines", v2_md_fence_lines, 1000, "md");
    assert_linear_v2("v2_org_fence_lines", v2_org_fence_lines, 1000, "org");
    assert_linear_v2(
        "v2_md_src_example_lines",
        v2_md_src_example_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_src_example_lines",
        v2_org_src_example_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_md_empty_callout_lines",
        v2_md_empty_callout_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_empty_callout_lines",
        v2_org_empty_callout_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_md_plain_callout_lines",
        v2_md_plain_callout_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_plain_callout_lines",
        v2_org_plain_callout_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_org_fixed_width_lines",
        v2_org_fixed_width_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_md_footnote_def_lines",
        v2_md_footnote_def_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_footnote_def_lines",
        v2_org_footnote_def_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_md_definition_list_lines",
        v2_md_definition_list_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_md_regular_list_lines",
        v2_md_regular_list_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_regular_list_lines",
        v2_org_regular_list_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_markdown_blockquote_lines",
        v2_markdown_blockquote_lines,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_markdown_blockquote_lines",
        v2_markdown_blockquote_lines,
        1000,
        "org",
    );
    assert_linear_v2(
        "v2_markdown_empty_quote_blank_continuations",
        v2_markdown_empty_quote_blank_continuations,
        1000,
        "md",
    );
    assert_linear_v2(
        "v2_org_empty_quote_blank_continuations",
        v2_markdown_empty_quote_blank_continuations,
        1000,
        "org",
    );
    assert_linear_v2(
        "markdown_balanced_label_after_eol",
        markdown_balanced_label_after_eol,
        1000,
        "md",
    );
    assert_linear_v2("v2_md_hiccup_lines", v2_md_hiccup_lines, 1000, "md");
    assert_linear_v2("v2_org_hiccup_lines", v2_org_hiccup_lines, 1000, "org");
    assert_linear_v2("v2_md_raw_html_lines", v2_md_raw_html_lines, 1000, "md");
    assert_linear_v2("v2_org_raw_html_lines", v2_org_raw_html_lines, 1000, "org");
}

/// The audit's not-yet-fixed O(n²) families. EMPTY: all four (`gt_spine`/`gt_breaker` 1a/1b → A,
/// `hiccup_unclosed` 2a → B, `resync` + `resync_leaf` 2b → C/D) are now single-pass and live in
/// `complexity_gate`. Kept as a shell so a future re-scan regression has an obvious home.
#[test]
#[ignore = "empty — all audit O(n^2) families are single-pass (in complexity_gate)"]
fn complexity_gate_targets() {}

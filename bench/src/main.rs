//! lsdoc throughput bench vs well-engineered third-party parsers.
//!
//! Feeds the SAME bytes to lsdoc and to established parsers and reports absolute
//! throughput (MB/s, ns/byte) plus the ratio to lsdoc. This is a *throughput* comparison
//! ("bytes -> tree, how fast"), NOT a semantic one — see the fairness notes in README.md.
//!
//!   md  peers: comrak (builds a full AST — the fair peer), pulldown-cmark (event
//!              stream, builds NO owned tree — a *ceiling*, not a peer).
//!   org peer : orgize (builds a syntax tree — the fair peer).
//!
//! Usage:
//!   cargo run --release -- --graph <dir> [--format md|org] [--iters N] [--scale]
//!   cargo run --release -- --files a.md b.md ... [--format md|org] [--iters N]
//!   cargo run --release -- --synthetic refs --synth-size 50000
//!
//! `--graph` walks the dir (honoring pages/ + journals/ if present) and dispatches each
//! file to md/org by extension unless `--format` forces one. I/O happens before timing.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut graph: Option<String> = None;
    let mut files: Vec<String> = Vec::new();
    let mut format_override: Option<String> = None;
    let mut iters: usize = 5;
    let mut report_path = "report.md".to_string();
    let mut scale = false;
    let mut synthetic: Option<String> = None;
    let mut synthetic_size: usize = 20_000;
    let mut gate_sota: Option<f64> = None;
    let mut gate_min_bytes: usize = 100_000;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--graph" => {
                graph = Some(args[i + 1].clone());
                i += 2;
            }
            "--format" => {
                format_override = Some(args[i + 1].clone());
                i += 2;
            }
            "--iters" => {
                iters = args[i + 1].parse().expect("--iters N");
                i += 2;
            }
            "--report" => {
                report_path = args[i + 1].clone();
                i += 2;
            }
            "--scale" => {
                scale = true;
                i += 1;
            }
            "--synthetic" => {
                synthetic = Some(args[i + 1].clone());
                i += 2;
            }
            "--synth-size" | "--synthetic-size" => {
                synthetic_size = args[i + 1].parse().expect("--synth-size N");
                i += 2;
            }
            "--gate-sota" => {
                gate_sota = Some(args[i + 1].parse().expect("--gate-sota RATIO"));
                i += 2;
            }
            "--gate-min-bytes" => {
                gate_min_bytes = args[i + 1].parse().expect("--gate-min-bytes N");
                i += 2;
            }
            "--files" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") {
                    files.push(args[i].clone());
                    i += 1;
                }
            }
            "-h" | "--help" => {
                eprintln!("{}", USAGE);
                return;
            }
            other => {
                eprintln!("unknown arg: {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    // Collect (path, format) pairs.
    let mut paths: Vec<(PathBuf, Fmt)> = Vec::new();
    if let Some(g) = &graph {
        collect_graph(Path::new(g), format_override.as_deref(), &mut paths);
    }
    for f in &files {
        let fmt = format_of(Path::new(f), format_override.as_deref());
        paths.push((PathBuf::from(f), fmt));
    }
    if paths.is_empty() && synthetic.is_none() {
        eprintln!("no input files found\n{USAGE}");
        std::process::exit(2);
    }

    // Read everything into memory (I/O excluded from timing). Split by format.
    let mut md: Vec<String> = Vec::new();
    let mut org: Vec<String> = Vec::new();
    let (mut n_md_files, mut n_org_files) = (0usize, 0usize);
    for (p, fmt) in &paths {
        let bytes = match fs::read(p) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skip {}: {e}", p.display());
                continue;
            }
        };
        let s = String::from_utf8_lossy(&bytes).into_owned();
        match fmt {
            Fmt::Md => {
                md.push(s);
                n_md_files += 1;
            }
            Fmt::Org => {
                org.push(s);
                n_org_files += 1;
            }
        }
    }
    if let Some(name) = &synthetic {
        let fmt = format_of(Path::new("synthetic.md"), format_override.as_deref());
        let docs = synthetic_inputs(name, fmt, synthetic_size);
        match fmt {
            Fmt::Md => {
                n_md_files += docs.len();
                md.extend(docs);
            }
            Fmt::Org => {
                n_org_files += docs.len();
                org.extend(docs);
            }
        }
    }

    let md_bytes: usize = md.iter().map(|s| s.len()).sum();
    let org_bytes: usize = org.iter().map(|s| s.len()).sum();

    let mut out = String::new();
    out.push_str("# lsdoc throughput vs third-party parsers\n\n");
    if let Some(g) = &graph {
        out.push_str(&format!("Corpus: `{g}`\n\n"));
    }
    if let Some(name) = &synthetic {
        out.push_str(&format!("Synthetic: `{name}` ({synthetic_size} units)\n\n"));
    }
    out.push_str(&format!(
        "- Markdown: {n_md_files} files, {} ({} bytes)\n- Org: {n_org_files} files, {} ({} bytes)\n- Timing: min of {iters} iterations, whole-corpus per-file document parse, I/O excluded.\n\n",
        human(md_bytes), md_bytes, human(org_bytes), org_bytes,
    ));

    print!("\nCorpus: md {n_md_files} files / {}  |  org {n_org_files} files / {}  |  min-of-{iters}\n",
           human(md_bytes), human(org_bytes));

    let mut gate_failures = Vec::new();
    let gate_enabled = gate_sota.filter(|_| synthetic.is_none());

    if !md.is_empty() {
        let parse = measure("lsdoc::parse (AST)", md_bytes, iters, "", &md, |s| {
            black_box(lsdoc::parse(s, "md"));
        });
        let sota_rows = vec![
            parse.clone(),
            measure(
                "comrak (CommonMark AST)",
                md_bytes,
                iters,
                "fair peer: builds a tree",
                &md,
                |s| {
                    let arena = comrak::Arena::new();
                    let opts = comrak::Options::default();
                    black_box(comrak::parse_document(&arena, s, &opts));
                },
            ),
            measure(
                "pulldown-cmark (events)",
                md_bytes,
                iters,
                "CEILING: no owned tree",
                &md,
                |s| {
                    black_box(pulldown_cmark::Parser::new(s).count());
                },
            ),
        ];
        emit_table(&mut out, "Markdown SOTA", &sota_rows);
        if let Some(cap) = gate_enabled {
            check_sota_gate(
                "Markdown/comrak",
                &sota_rows[0],
                &sota_rows[1],
                cap,
                gate_min_bytes,
                &mut out,
                &mut gate_failures,
            );
        }

        let rendered: Vec<Vec<lsdoc::ast::Block>> =
            md.iter().map(|s| lsdoc::parse(s, "md")).collect();
        let phase_rows = vec![
            parse,
            measure(
                "lsdoc::parse_format (+refs)",
                md_bytes,
                iters,
                "full projection",
                &md,
                |s| {
                    black_box(lsdoc::parse_format(s, "md"));
                },
            ),
            measure(
                "lsdoc::refs",
                md_bytes,
                iters,
                "public refs-only API",
                &md,
                |s| {
                    black_box(lsdoc::refs(s, "md"));
                },
            ),
            measure_render(
                "lsdoc::render_html",
                md_bytes,
                iters,
                "AST -> HTML only",
                &rendered,
                lsdoc::Format::Md,
            ),
        ];
        emit_table(&mut out, "Markdown Phase Costs", &phase_rows);
    }

    if !org.is_empty() {
        let parse = measure("lsdoc::parse (AST)", org_bytes, iters, "", &org, |s| {
            black_box(lsdoc::parse(s, "org"));
        });
        let sota_rows = vec![
            parse.clone(),
            measure(
                "orgize (syntax tree)",
                org_bytes,
                iters,
                "fair peer: builds a tree",
                &org,
                |s| {
                    black_box(orgize::Org::parse(s));
                },
            ),
        ];
        emit_table(&mut out, "Org SOTA", &sota_rows);
        if let Some(cap) = gate_enabled {
            check_sota_gate(
                "Org/orgize",
                &sota_rows[0],
                &sota_rows[1],
                cap,
                gate_min_bytes,
                &mut out,
                &mut gate_failures,
            );
        }

        let rendered: Vec<Vec<lsdoc::ast::Block>> =
            org.iter().map(|s| lsdoc::parse(s, "org")).collect();
        let phase_rows = vec![
            parse,
            measure(
                "lsdoc::parse_format (+refs)",
                org_bytes,
                iters,
                "full projection",
                &org,
                |s| {
                    black_box(lsdoc::parse_format(s, "org"));
                },
            ),
            measure(
                "lsdoc::refs",
                org_bytes,
                iters,
                "public refs-only API",
                &org,
                |s| {
                    black_box(lsdoc::refs(s, "org"));
                },
            ),
            measure_render(
                "lsdoc::render_html",
                org_bytes,
                iters,
                "AST -> HTML only",
                &rendered,
                lsdoc::Format::Org,
            ),
        ];
        emit_table(&mut out, "Org Phase Costs", &phase_rows);
    }

    if scale {
        out.push_str("## Scaling (real-content O(n²) guard)\n\n");
        out.push_str("Single concatenated input at 1×/2×/4×. Linear ⇒ each doubling ≈ 2.0×. ");
        out.push_str("If lsdoc's ratio outruns comrak's, that's real-content super-linearity.\n\n");
        if !md.is_empty() {
            let base: String = md.join("\n\n");
            scale_report(
                &mut out,
                "Markdown",
                &base,
                |s| {
                    black_box(lsdoc::parse(s, "md"));
                },
                |s| {
                    let a = comrak::Arena::new();
                    black_box(comrak::parse_document(&a, s, &comrak::Options::default()));
                },
            );
        }
        if !org.is_empty() {
            let base: String = org.join("\n\n");
            scale_report(
                &mut out,
                "Org",
                &base,
                |s| {
                    black_box(lsdoc::parse(s, "org"));
                },
                |s| {
                    black_box(orgize::Org::parse(s));
                },
            );
        }
    }

    out.push_str("\n---\n_Throughput only, not semantic parity. comrak/orgize parse the same bytes into their own trees; pulldown-cmark builds no owned tree (a ceiling). See README.md for the full fairness notes._\n");

    if !gate_failures.is_empty() {
        out.push_str("## SOTA Gate Failures\n\n");
        for failure in &gate_failures {
            out.push_str("- ");
            out.push_str(failure);
            out.push('\n');
        }
        out.push('\n');
    }

    if let Err(e) = fs::write(&report_path, &out) {
        eprintln!("could not write {report_path}: {e}");
    } else {
        println!("\nWrote {report_path}");
    }
    if !gate_failures.is_empty() {
        for failure in gate_failures {
            eprintln!("SOTA gate failed: {failure}");
        }
        std::process::exit(1);
    }
}

#[derive(Clone, Copy)]
enum Fmt {
    Md,
    Org,
}

#[derive(Clone)]
struct Row {
    name: &'static str,
    note: &'static str,
    dur: Duration,
    bytes: usize,
}

/// Warm up once, then take the min over `iters` full passes.
fn measure<F: Fn(&str)>(
    name: &'static str,
    bytes: usize,
    iters: usize,
    note: &'static str,
    files: &[String],
    f: F,
) -> Row {
    for s in files {
        f(s);
    } // warmup
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let start = Instant::now();
        for s in files {
            f(black_box(s));
        }
        best = best.min(start.elapsed());
    }
    Row {
        name,
        note,
        dur: best,
        bytes,
    }
}

fn measure_render(
    name: &'static str,
    bytes: usize,
    iters: usize,
    note: &'static str,
    docs: &[Vec<lsdoc::ast::Block>],
    format: lsdoc::Format,
) -> Row {
    let opts = lsdoc::RenderOpts { format };
    for blocks in docs {
        black_box(lsdoc::render_html(blocks, &opts));
    } // warmup
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let start = Instant::now();
        for blocks in docs {
            black_box(lsdoc::render_html(black_box(blocks), &opts));
        }
        best = best.min(start.elapsed());
    }
    Row {
        name,
        note,
        dur: best,
        bytes,
    }
}

fn mbps(bytes: usize, d: Duration) -> f64 {
    (bytes as f64) / d.as_secs_f64() / 1.0e6
}
fn ns_per_byte(bytes: usize, d: Duration) -> f64 {
    d.as_nanos() as f64 / bytes as f64
}

fn check_sota_gate(
    label: &str,
    lsdoc: &Row,
    peer: &Row,
    cap: f64,
    min_bytes: usize,
    out: &mut String,
    failures: &mut Vec<String>,
) {
    if lsdoc.bytes < min_bytes {
        let msg = format!(
            "{label}: skipped gate on {} input bytes (< {} minimum)",
            lsdoc.bytes, min_bytes
        );
        println!("SOTA gate skipped: {msg}");
        out.push_str(&format!("_SOTA gate skipped: {msg}._\n\n"));
        return;
    }
    let ratio = mbps(peer.bytes, peer.dur) / mbps(lsdoc.bytes, lsdoc.dur);
    if ratio > cap {
        failures.push(format!(
            "{label}: peer/lsdoc ratio {ratio:.3}x exceeds {cap:.3}x"
        ));
    }
}

fn emit_table(out: &mut String, title: &str, rows: &[Row]) {
    // lsdoc::parse (first row) is the baseline.
    let base = mbps(rows[0].bytes, rows[0].dur);
    println!("\n== {title} ==");
    println!(
        "  {:<30} {:>9}  {:>9}  {:>10}  {}",
        "parser", "MB/s", "ns/byte", "vs lsdoc", "note"
    );
    out.push_str(&format!("## {title}\n\n"));
    out.push_str("| parser | MB/s | ns/byte | vs lsdoc | note |\n|---|---:|---:|---:|---|\n");
    for (idx, r) in rows.iter().enumerate() {
        let m = mbps(r.bytes, r.dur);
        let npb = ns_per_byte(r.bytes, r.dur);
        let (vs, verdict) = if idx == 0 {
            ("1.00×".to_string(), "baseline".to_string())
        } else {
            let ratio = m / base; // peer speed as a multiple of lsdoc's
            let verdict = if m >= base {
                format!("lsdoc {:.0}% slower", (1.0 - base / m) * 100.0)
            } else {
                format!("lsdoc {:.0}% faster", (base / m - 1.0) * 100.0)
            };
            (format!("{ratio:.2}×"), verdict)
        };
        let note = if r.note.is_empty() {
            verdict.clone()
        } else {
            format!("{} — {}", r.note, verdict)
        };
        println!(
            "  {:<30} {:>9.1}  {:>9.2}  {:>10}  {}",
            r.name, m, npb, vs, note
        );
        out.push_str(&format!(
            "| {} | {:.1} | {:.2} | {} | {} |\n",
            r.name, m, npb, vs, note
        ));
    }
    out.push('\n');
}

fn scale_report<L: Fn(&str), C: Fn(&str)>(
    out: &mut String,
    title: &str,
    base: &str,
    lsd: L,
    peer: C,
) {
    out.push_str(&format!(
        "### {title}\n\n| size | lsdoc ms | ratio | peer ms | ratio |\n|---|---:|---:|---:|---:|\n"
    ));
    println!("\n== {title} scaling ==");
    let mut prev_l: Option<f64> = None;
    let mut prev_p: Option<f64> = None;
    for k in [1usize, 2, 4] {
        let input: String = base.repeat(k);
        let tl = min_time(3, || lsd(&input));
        let tp = min_time(3, || peer(&input));
        let (lms, pms) = (tl.as_secs_f64() * 1e3, tp.as_secs_f64() * 1e3);
        let lr = prev_l.map(|p| lms / p).unwrap_or(1.0);
        let pr = prev_p.map(|p| pms / p).unwrap_or(1.0);
        println!("  {k}×  lsdoc {lms:8.2}ms ({lr:.2}×)   peer {pms:8.2}ms ({pr:.2}×)");
        out.push_str(&format!(
            "| {k}× | {lms:.2} | {lr:.2}× | {pms:.2} | {pr:.2}× |\n"
        ));
        prev_l = Some(lms);
        prev_p = Some(pms);
    }
    out.push('\n');
}

fn min_time<F: Fn()>(iters: usize, f: F) -> Duration {
    f(); // warmup
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let s = Instant::now();
        f();
        best = best.min(s.elapsed());
    }
    best
}

fn format_of(p: &Path, override_: Option<&str>) -> Fmt {
    match override_ {
        Some("org") => Fmt::Org,
        Some(_) => Fmt::Md,
        None => match p.extension().and_then(|e| e.to_str()) {
            Some("org") => Fmt::Org,
            _ => Fmt::Md,
        },
    }
}

/// Walk a graph dir. If pages/ and/or journals/ exist, restrict to those; else walk all.
fn collect_graph(root: &Path, override_: Option<&str>, out: &mut Vec<(PathBuf, Fmt)>) {
    let subdirs = ["pages", "journals"];
    let has_structure = subdirs.iter().any(|d| root.join(d).is_dir());
    if has_structure {
        for d in subdirs {
            walk(&root.join(d), override_, out);
        }
    } else {
        walk(root, override_, out);
    }
}

fn walk(dir: &Path, override_: Option<&str>, out: &mut Vec<(PathBuf, Fmt)>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }
            walk(&p, override_, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if ext == "md" || ext == "org" || ext == "markdown" {
                out.push((p.clone(), format_of(&p, override_)));
            }
        }
    }
}

fn human(bytes: usize) -> String {
    let b = bytes as f64;
    if b >= 1e6 {
        format!("{:.2} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

fn synthetic_inputs(name: &str, fmt: Fmt, n: usize) -> Vec<String> {
    if name == "all" {
        return [
            "prose",
            "refs",
            "refs-dup",
            "properties",
            "markup",
            "hiccup",
            "hiccup-nested",
            "render-heavy",
        ]
        .into_iter()
        .map(|case| synthetic_input(case, fmt, n))
        .collect();
    }
    vec![synthetic_input(name, fmt, n)]
}

fn synthetic_input(name: &str, fmt: Fmt, n: usize) -> String {
    let mut s = String::new();
    match (name, fmt) {
        ("refs", Fmt::Md)
        | ("refs", Fmt::Org)
        | ("refs-unique", Fmt::Md)
        | ("refs-unique", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!(
                    "[[page-{i:06}]] ((11111111-1111-1111-1111-{i:012x}))\n"
                ));
            }
        }
        ("refs-dup", Fmt::Md)
        | ("refs-dup", Fmt::Org)
        | ("refs-dupe", Fmt::Md)
        | ("refs-dupe", Fmt::Org) => {
            for _ in 0..n {
                s.push_str("[[same-page]] ((11111111-1111-1111-1111-111111111111)) #same-tag\n");
            }
        }
        ("properties", Fmt::Md) | ("property-refs", Fmt::Md) => {
            for i in 0..n {
                s.push_str(&format!("k{i:06}:: {{{{query [[page-{i:06}]]}}}} [id](id://11111111-1111-1111-1111-{i:012x})\n"));
            }
        }
        ("properties", Fmt::Org) | ("property-refs", Fmt::Org) => {
            s.push_str(":PROPERTIES:\n");
            for i in 0..n {
                s.push_str(&format!(":k{i:06}: {{{{query [[page-{i:06}]]}}}} [[id://11111111-1111-1111-1111-{i:012x}][id]]\n"));
            }
            s.push_str(":END:\n");
        }
        ("prose", Fmt::Md) | ("prose", Fmt::Org) | ("plain", Fmt::Md) | ("plain", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!(
                    "This is ordinary prose line {i}, with words and punctuation but no markup.\n"
                ));
            }
        }
        ("markup", Fmt::Md) => {
            for i in 0..n {
                s.push_str(&format!("line {i} **strong** _em_ [[page-{i:06}]] #tag-{i} [site](https://example.com/{i}) `code`\n"));
            }
        }
        ("markup", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!(
                    "line {i} *strong* /em/ [[page-{i:06}][label]] #tag-{i} ~code~\n"
                ));
            }
        }
        ("hiccup", Fmt::Md) | ("hiccup", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!("[:div [:span \"row-{i}\"]]\n"));
            }
        }
        ("hiccup-nested", Fmt::Md) | ("hiccup-nested", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!(
                    "[:div [:span \"row-{i}\"] [:b [:i \"nested\"]]]\n"
                ));
            }
        }
        ("render-heavy", Fmt::Md) => {
            for i in 0..n {
                s.push_str(&format!("k{i:06}:: value [[page-{i:06}]] **bold**\n#tag-{i} ![alt {i}](assets/img-{i}.png) [paper {i}](assets/doc-{i}.pdf)\n"));
            }
        }
        ("render-heavy", Fmt::Org) => {
            for i in 0..n {
                s.push_str(&format!(":k{i:06}: value [[page-{i:06}]] *bold*\n#tag-{i} [[file:assets/doc-{i}.pdf][paper {i}]]\n"));
            }
        }
        _ => {
            eprintln!("unknown synthetic workload: {name}");
            std::process::exit(2);
        }
    }
    s
}

const USAGE: &str = "lsdoc-bench — throughput vs comrak/pulldown (md) and orgize (org)\n\
  --graph <dir>       walk a Logseq graph dir (pages/ + journals/ if present)\n\
  --files <f> [f...]  explicit files\n\
  --format md|org     force a format (else inferred per-file by extension)\n\
  --iters N           timing passes, min taken (default 5)\n\
  --scale             also run the 1x/2x/4x single-input O(n^2) guard\n\
  --gate-sota RATIO   fail real-corpus fair-peer/lsdoc parse ratios above RATIO\n\
  --gate-min-bytes N  minimum bytes for a format-specific SOTA gate (default 100000)\n\
  --synthetic NAME    generate all|prose|refs|refs-dup|properties|markup|hiccup|hiccup-nested|render-heavy\n\
  --synth-size N      synthetic repetition count (default 20000)\n\
  --report <path>     report file (default report.md)";

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

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--graph" => { graph = Some(args[i + 1].clone()); i += 2; }
            "--format" => { format_override = Some(args[i + 1].clone()); i += 2; }
            "--iters" => { iters = args[i + 1].parse().expect("--iters N"); i += 2; }
            "--report" => { report_path = args[i + 1].clone(); i += 2; }
            "--scale" => { scale = true; i += 1; }
            "--files" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") { files.push(args[i].clone()); i += 1; }
            }
            "-h" | "--help" => { eprintln!("{}", USAGE); return; }
            other => { eprintln!("unknown arg: {other}\n{USAGE}"); std::process::exit(2); }
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
    if paths.is_empty() {
        eprintln!("no input files found\n{USAGE}");
        std::process::exit(2);
    }

    // Read everything into memory (I/O excluded from timing). Split by format.
    let mut md: Vec<String> = Vec::new();
    let mut org: Vec<String> = Vec::new();
    let (mut n_md_files, mut n_org_files) = (0usize, 0usize);
    for (p, fmt) in &paths {
        let bytes = match fs::read(p) { Ok(b) => b, Err(e) => { eprintln!("skip {}: {e}", p.display()); continue; } };
        let s = String::from_utf8_lossy(&bytes).into_owned();
        match fmt { Fmt::Md => { md.push(s); n_md_files += 1; } Fmt::Org => { org.push(s); n_org_files += 1; } }
    }

    let md_bytes: usize = md.iter().map(|s| s.len()).sum();
    let org_bytes: usize = org.iter().map(|s| s.len()).sum();

    let mut out = String::new();
    out.push_str("# lsdoc throughput vs third-party parsers\n\n");
    if let Some(g) = &graph { out.push_str(&format!("Corpus: `{g}`\n\n")); }
    out.push_str(&format!(
        "- Markdown: {n_md_files} files, {} ({} bytes)\n- Org: {n_org_files} files, {} ({} bytes)\n- Timing: min of {iters} iterations, whole-corpus per-file document parse, I/O excluded.\n\n",
        human(md_bytes), md_bytes, human(org_bytes), org_bytes,
    ));

    print!("\nCorpus: md {n_md_files} files / {}  |  org {n_org_files} files / {}  |  min-of-{iters}\n",
           human(md_bytes), human(org_bytes));

    if !md.is_empty() {
        let mut rows = Vec::new();
        rows.push(measure("lsdoc::parse (AST)", md_bytes, iters, "", &md, |s| { black_box(lsdoc::parse(s, "md")); }));
        rows.push(measure("lsdoc::parse_format (+refs)", md_bytes, iters, "Logseq index tax", &md, |s| { black_box(lsdoc::parse_format(s, "md")); }));
        rows.push(measure("comrak (CommonMark AST)", md_bytes, iters, "fair peer: builds a tree", &md, |s| {
            let arena = comrak::Arena::new();
            let opts = comrak::Options::default();
            black_box(comrak::parse_document(&arena, s, &opts));
        }));
        rows.push(measure("pulldown-cmark (events)", md_bytes, iters, "CEILING: no owned tree", &md, |s| {
            black_box(pulldown_cmark::Parser::new(s).count());
        }));
        emit_table(&mut out, "Markdown", &rows);
    }

    if !org.is_empty() {
        let mut rows = Vec::new();
        rows.push(measure("lsdoc::parse (AST)", org_bytes, iters, "", &org, |s| { black_box(lsdoc::parse(s, "org")); }));
        rows.push(measure("lsdoc::parse_format (+refs)", org_bytes, iters, "Logseq index tax", &org, |s| { black_box(lsdoc::parse_format(s, "org")); }));
        rows.push(measure("orgize (syntax tree)", org_bytes, iters, "fair peer: builds a tree", &org, |s| {
            black_box(orgize::Org::parse(s));
        }));
        emit_table(&mut out, "Org", &rows);
    }

    if scale {
        out.push_str("## Scaling (real-content O(n²) guard)\n\n");
        out.push_str("Single concatenated input at 1×/2×/4×. Linear ⇒ each doubling ≈ 2.0×. ");
        out.push_str("If lsdoc's ratio outruns comrak's, that's real-content super-linearity.\n\n");
        if !md.is_empty() {
            let base: String = md.join("\n\n");
            scale_report(&mut out, "Markdown", &base,
                |s| { black_box(lsdoc::parse(s, "md")); },
                |s| { let a = comrak::Arena::new(); black_box(comrak::parse_document(&a, s, &comrak::Options::default())); });
        }
        if !org.is_empty() {
            let base: String = org.join("\n\n");
            scale_report(&mut out, "Org", &base,
                |s| { black_box(lsdoc::parse(s, "org")); },
                |s| { black_box(orgize::Org::parse(s)); });
        }
    }

    out.push_str("\n---\n_Throughput only, not semantic parity. comrak/orgize parse the same bytes into their own trees; pulldown-cmark builds no owned tree (a ceiling). See README.md for the full fairness notes._\n");

    if let Err(e) = fs::write(&report_path, &out) { eprintln!("could not write {report_path}: {e}"); }
    else { println!("\nWrote {report_path}"); }
}

#[derive(Clone, Copy)]
enum Fmt { Md, Org }

struct Row { name: &'static str, note: &'static str, dur: Duration, bytes: usize }

/// Warm up once, then take the min over `iters` full passes.
fn measure<F: Fn(&str)>(name: &'static str, bytes: usize, iters: usize, note: &'static str, files: &[String], f: F) -> Row {
    for s in files { f(s); } // warmup
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let start = Instant::now();
        for s in files { f(black_box(s)); }
        best = best.min(start.elapsed());
    }
    Row { name, note, dur: best, bytes }
}

fn mbps(bytes: usize, d: Duration) -> f64 { (bytes as f64) / d.as_secs_f64() / 1.0e6 }
fn ns_per_byte(bytes: usize, d: Duration) -> f64 { d.as_nanos() as f64 / bytes as f64 }

fn emit_table(out: &mut String, title: &str, rows: &[Row]) {
    // lsdoc::parse (first row) is the baseline.
    let base = mbps(rows[0].bytes, rows[0].dur);
    println!("\n== {title} ==");
    println!("  {:<30} {:>9}  {:>9}  {:>10}  {}", "parser", "MB/s", "ns/byte", "vs lsdoc", "note");
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
        let note = if r.note.is_empty() { verdict.clone() } else { format!("{} — {}", r.note, verdict) };
        println!("  {:<30} {:>9.1}  {:>9.2}  {:>10}  {}", r.name, m, npb, vs, note);
        out.push_str(&format!("| {} | {:.1} | {:.2} | {} | {} |\n", r.name, m, npb, vs, note));
    }
    out.push('\n');
}

fn scale_report<L: Fn(&str), C: Fn(&str)>(out: &mut String, title: &str, base: &str, lsd: L, peer: C) {
    out.push_str(&format!("### {title}\n\n| size | lsdoc ms | ratio | peer ms | ratio |\n|---|---:|---:|---:|---:|\n"));
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
        out.push_str(&format!("| {k}× | {lms:.2} | {lr:.2}× | {pms:.2} | {pr:.2}× |\n"));
        prev_l = Some(lms); prev_p = Some(pms);
    }
    out.push('\n');
}

fn min_time<F: Fn()>(iters: usize, f: F) -> Duration {
    f(); // warmup
    let mut best = Duration::MAX;
    for _ in 0..iters { let s = Instant::now(); f(); best = best.min(s.elapsed()); }
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
        for d in subdirs { walk(&root.join(d), override_, out); }
    } else {
        walk(root, override_, out);
    }
}

fn walk(dir: &Path, override_: Option<&str>, out: &mut Vec<(PathBuf, Fmt)>) {
    let rd = match fs::read_dir(dir) { Ok(r) => r, Err(_) => return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().and_then(|n| n.to_str()) == Some(".git") { continue; }
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
    if b >= 1e6 { format!("{:.2} MB", b / 1e6) }
    else if b >= 1e3 { format!("{:.1} KB", b / 1e3) }
    else { format!("{bytes} B") }
}

const USAGE: &str = "lsdoc-bench — throughput vs comrak/pulldown (md) and orgize (org)\n\
  --graph <dir>       walk a Logseq graph dir (pages/ + journals/ if present)\n\
  --files <f> [f...]  explicit files\n\
  --format md|org     force a format (else inferred per-file by extension)\n\
  --iters N           timing passes, min taken (default 5)\n\
  --scale             also run the 1x/2x/4x single-input O(n^2) guard\n\
  --report <path>     report file (default report.md)";

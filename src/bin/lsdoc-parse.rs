//! Differential harness driver (lsdoc side): read a corpus of `[{id, input}]`,
//! parse each into the observable projection, and write `[{id, input, projection}]`
//! so `harness/compare.mjs` can diff it against the mldoc oracle's output.
//!
//! Usage: lsdoc-parse [--timings-no-input] [--engine current|legacy|v2|v2-strict|v2-report] [CORPUS_JSON] [OUTPUT_JSON]
//!   defaults: harness/corpus.json  →  harness/lsdoc-out.json

use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Instant;

#[derive(Deserialize)]
struct CorpusItem {
    id: String,
    input: String,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Serialize)]
struct OutItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    projection: lsdoc::ast::Projection,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_micros: Option<u128>,
}

#[derive(Serialize)]
struct InlineOutItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    inline: Vec<lsdoc::ast::Inline>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_micros: Option<u128>,
}

fn main() {
    let mut positional = Vec::new();
    let mut timings_no_input = false;
    let mut engine = std::env::var("LSDOC_ENGINE").unwrap_or_else(|_| "current".to_string());
    let mut argv = std::env::args().skip(1).peekable();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--timings-no-input" => timings_no_input = true,
            "--engine" => {
                engine = argv
                    .next()
                    .unwrap_or_else(|| panic!("--engine needs current|legacy|v2"));
            }
            _ => positional.push(arg),
        }
    }
    if engine != "current"
        && engine != "legacy"
        && engine != "v2"
        && engine != "v2-strict"
        && engine != "v2-report"
    {
        panic!("unknown lsdoc engine '{engine}' (expected current|legacy|v2|v2-strict|v2-report)");
    }
    let mut args = positional.into_iter();
    let corpus_path = args
        .next()
        .unwrap_or_else(|| "harness/corpus.json".to_string());
    let out_path = args
        .next()
        .unwrap_or_else(|| "harness/lsdoc-out.json".to_string());

    let raw =
        fs::read_to_string(&corpus_path).unwrap_or_else(|e| panic!("read {corpus_path}: {e}"));
    let corpus: Vec<CorpusItem> =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {corpus_path}: {e}"));

    // LSDOC_INLINE=1 → exercise the inline-only entrypoint (`lsdoc::inline`) instead of the
    // full projection, for the inline differential gate against mldoc `parseInlineJson`.
    if std::env::var("LSDOC_INLINE").is_ok() {
        let out: Vec<InlineOutItem> = corpus
            .into_iter()
            .map(|c| {
                let start = Instant::now();
                let format = c.format.as_deref().unwrap_or("md");
                let inline = if engine == "v2" || engine == "v2-report" {
                    lsdoc::__inline_v2(&c.input, format)
                } else {
                    lsdoc::inline(&c.input, format)
                };
                let parse_micros = start.elapsed().as_micros();
                InlineOutItem {
                    inline,
                    id: c.id,
                    input: if timings_no_input {
                        None
                    } else {
                        Some(c.input)
                    },
                    parse_micros: if timings_no_input {
                        Some(parse_micros)
                    } else {
                        None
                    },
                }
            })
            .collect();
        // Compact, NOT to_string_pretty: the pretty-printer indents each JSON line by the
        // current nesting depth, so a depth-k nested projection serializes to Σdepth = O(k²)
        // bytes (measured: an 83KB depth-3200 callout nest → 205MB pretty vs 210KB compact).
        // Every consumer JSON.parses this file; formatting carries no information.
        let json = serde_json::to_string(&out).expect("serialize output");
        fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
        println!(
            "lsdoc[{engine}]: wrote {} inline runs to {out_path}",
            out.len()
        );
        return;
    }

    let mut unowned = Vec::new();
    let mut out = Vec::with_capacity(corpus.len());
    for c in corpus {
        let start = Instant::now();
        let format = c.format.as_deref().unwrap_or("md");
        let projection = match engine.as_str() {
            "legacy" => lsdoc::__parse_format_legacy(&c.input, format),
            "v2" => lsdoc::__parse_format_v2(&c.input, format),
            "v2-strict" => lsdoc::__try_parse_format_v2(&c.input, format)
                .unwrap_or_else(|| panic!("v2-strict unowned corpus item {}", c.id)),
            "v2-report" => match lsdoc::__try_parse_format_v2(&c.input, format) {
                Some(projection) => projection,
                None => {
                    unowned.push(c.id.clone());
                    lsdoc::__parse_format_legacy(&c.input, format)
                }
            },
            _ => lsdoc::parse_format(&c.input, format),
        };
        let parse_micros = start.elapsed().as_micros();
        out.push(OutItem {
            projection,
            id: c.id,
            input: if timings_no_input {
                None
            } else {
                Some(c.input)
            },
            parse_micros: if timings_no_input {
                Some(parse_micros)
            } else {
                None
            },
        });
    }

    // Compact for the same O(k²)-pretty-indentation reason as the inline path above.
    let json = serde_json::to_string(&out).expect("serialize output");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    println!(
        "lsdoc[{engine}]: wrote {} projections to {out_path}",
        out.len()
    );
    if engine == "v2-report" {
        eprintln!(
            "lsdoc[v2-report]: owned {}/{}",
            out.len() - unowned.len(),
            out.len()
        );
        for id in &unowned {
            eprintln!("lsdoc[v2-report]: unowned {id}");
        }
        if !unowned.is_empty() {
            std::process::exit(1);
        }
    }
}

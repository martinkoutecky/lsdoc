//! Differential harness driver (lsdoc side): read a corpus of `[{id, input}]`,
//! parse each into the observable projection, and write `[{id, input, projection}]`
//! so `harness/compare.mjs` can diff it against the mldoc oracle's output.
//!
//! Usage: lsdoc-parse [CORPUS_JSON] [OUTPUT_JSON]
//!   defaults: harness/corpus.json  →  harness/lsdoc-out.json

use serde::{Deserialize, Serialize};
use std::fs;

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
    input: String,
    projection: lsdoc::ast::Projection,
}

#[derive(Serialize)]
struct InlineOutItem {
    id: String,
    input: String,
    inline: Vec<lsdoc::ast::Inline>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_path = args.next().unwrap_or_else(|| "harness/corpus.json".to_string());
    let out_path = args.next().unwrap_or_else(|| "harness/lsdoc-out.json".to_string());

    let raw = fs::read_to_string(&corpus_path)
        .unwrap_or_else(|e| panic!("read {corpus_path}: {e}"));
    let corpus: Vec<CorpusItem> =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {corpus_path}: {e}"));

    // LSDOC_INLINE=1 → exercise the inline-only entrypoint (`lsdoc::inline`) instead of the
    // full projection, for the inline differential gate against mldoc `parseInlineJson`.
    if std::env::var("LSDOC_INLINE").is_ok() {
        let out: Vec<InlineOutItem> = corpus
            .into_iter()
            .map(|c| InlineOutItem {
                inline: lsdoc::inline(&c.input, c.format.as_deref().unwrap_or("md")),
                id: c.id,
                input: c.input,
            })
            .collect();
        // Compact, NOT to_string_pretty: the pretty-printer indents each JSON line by the
        // current nesting depth, so a depth-k nested projection serializes to Σdepth = O(k²)
        // bytes (measured: an 83KB depth-3200 callout nest → 205MB pretty vs 210KB compact).
        // Every consumer JSON.parses this file; formatting carries no information.
        let json = serde_json::to_string(&out).expect("serialize output");
        fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
        println!("lsdoc: wrote {} inline runs to {out_path}", out.len());
        return;
    }

    let out: Vec<OutItem> = corpus
        .into_iter()
        .map(|c| OutItem {
            projection: lsdoc::parse_format(&c.input, c.format.as_deref().unwrap_or("md")),
            id: c.id,
            input: c.input,
        })
        .collect();

    // Compact for the same O(k²)-pretty-indentation reason as the inline path above.
    let json = serde_json::to_string(&out).expect("serialize output");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    println!("lsdoc: wrote {} projections to {out_path}", out.len());
}

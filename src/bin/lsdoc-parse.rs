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
    projection: lsdoc::projection::Projection,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_path = args.next().unwrap_or_else(|| "harness/corpus.json".to_string());
    let out_path = args.next().unwrap_or_else(|| "harness/lsdoc-out.json".to_string());

    let raw = fs::read_to_string(&corpus_path)
        .unwrap_or_else(|e| panic!("read {corpus_path}: {e}"));
    let corpus: Vec<CorpusItem> =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {corpus_path}: {e}"));

    let out: Vec<OutItem> = corpus
        .into_iter()
        .map(|c| OutItem {
            projection: lsdoc::parse_format(&c.input, c.format.as_deref().unwrap_or("md")),
            id: c.id,
            input: c.input,
        })
        .collect();

    let json = serde_json::to_string_pretty(&out).expect("serialize output");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    println!("lsdoc: wrote {} projections to {out_path}", out.len());
}

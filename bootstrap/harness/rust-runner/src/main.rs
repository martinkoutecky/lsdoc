// Standalone runner: read corpus.json, run the Tine Rust ref-extractor over every
// input, write rust-out.json. Uses tine-core as a path dependency (repo untouched).
use serde_json::{json, Value};
use std::fs;
use tine_core::refs::{block_ref_ids, block_refs, page_refs};

const CORPUS: &str = "/tmp/claude-3042/-aux-koutecky-logseq/2e921412-0c07-49c5-87de-46be358044a0/scratchpad/parser-divergence/corpus.json";
const OUT: &str = "/tmp/claude-3042/-aux-koutecky-logseq/2e921412-0c07-49c5-87de-46be358044a0/scratchpad/parser-divergence/rust-out.json";

fn main() {
    let raw = fs::read_to_string(CORPUS).expect("read corpus.json");
    let corpus: Vec<Value> = serde_json::from_str(&raw).expect("parse corpus.json");
    let results: Vec<Value> = corpus
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap();
            let input = c["input"].as_str().unwrap();
            json!({
                "id": id,
                "page": page_refs(input),
                "block_refs": block_refs(input),     // non-uuid-gated bare ((..))
                "block_ids": block_ref_ids(input),   // uuid-gated, deduped, all forms
            })
        })
        .collect();
    fs::write(OUT, serde_json::to_string_pretty(&results).unwrap()).expect("write rust-out.json");
    println!("wrote {} rust results", results.len());
}

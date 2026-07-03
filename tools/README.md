# Graph Check

`graph-check.mjs` benchmarks `lsdoc` and Logseq's shipped `mldoc` npm build on a local Logseq
graph, and can scan for parser divergences without uploading anything.

## Setup

From this repository:

```bash
cd harness
npm ci
cd ..
source scripts/env.sh
cargo build --release --bin lsdoc-parse
```

The tool will build `target/release/lsdoc-parse` if it is missing or older than the bin wrapper.

## Run

```bash
node tools/graph-check.mjs /path/to/logseq-graph --mode both --format auto --out report.md
```

Useful flags:

- `--mode bench|diff|both`: benchmark only, diff scan only, or both.
- `--format md|org|auto`: scan Markdown, Org, or both by extension.
- `--journals` / `--no-journals`: include or exclude `journals/**`; journals are included by default.
- `--jobs N`: parallelism for diff-mode mldoc subprocesses. Default is `1`.
- `--timeout-ms N`: per-file parser timeout. Default is `10000`.
- `--fast`: non-authoritative diff scan using a warm mldoc process. It can both invent and mask
  divergences; default diff mode uses one fresh mldoc subprocess per file.

The scan looks only under `pages/**` and, by default, `journals/**`. Files over 8 MB are skipped and
listed in the report.

## Privacy

Nothing is uploaded. The tool does not write graph content anywhere except a private `mkdtemp`
directory with mode `0700`, removed on exit, and the final `--out` report.

The report contains paths, graph-level timing statistics, and only anonymized snippets that were
re-parsed and still reproduced a divergence. If anonymization does not preserve the divergence, the
report lists only the file path and line range so you can inspect it locally.

An anonymized snippet is a fresh reproducible divergence derived from your page, not the original
page text. Inspect the report before sharing it.


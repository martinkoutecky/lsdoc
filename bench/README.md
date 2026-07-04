# lsdoc/bench — throughput vs well-engineered third-party parsers

The differential oracle answers "is lsdoc *correct*?" This crate answers a different
question: **"is lsdoc *fast enough* — competitive with a well-written, non-exotic parser,
not just faster than mldoc?"** Beating mldoc (the exotic parser lsdoc replaces) is a low
bar; this measures lsdoc against parsers people actually reach for.

It feeds the **same bytes** to lsdoc and to established parsers and reports absolute
throughput (MB/s, ns/byte) plus the ratio to lsdoc:

| format | fair peer (builds a tree) | ceiling (builds no owned tree) |
|---|---|---|
| Markdown | **comrak** (CommonMark AST) | **pulldown-cmark** (event stream) |
| Org | **orgize** (rowan syntax tree) | — |

It is a **standalone crate with its own `[workspace]`** (see `Cargo.toml`): comrak /
pulldown-cmark / orgize compile *only* when you build `bench/`. They never enter lsdoc's
own dependency graph (which stays `serde`-only) and never propagate to Tine. `orgize` is
here purely as a benchmark peer — **not** a re-addition to lsdoc/Tine at runtime.

## Run

```sh
source ../scripts/env.sh
bash fetch-corpus.sh                                   # clones logseq/docs (md) + worg (org), gitignored
cargo build --release

./target/release/lsdoc-bench --graph corpus/logseq-docs           # md
./target/release/lsdoc-bench --graph corpus/worg --format org     # org
./target/release/lsdoc-bench --files ../../mldoc-upstream/examples/doc.org --format org
./target/release/lsdoc-bench --graph corpus/logseq-docs --scale   # 1x/2x/4x O(n^2) guard

# Martin only — the honest number on the real graph (I can't access it):
./target/release/lsdoc-bench --graph ~/research/brain
```

`--graph` walks `pages/` + `journals/` (if present) and dispatches md/org by extension.
All file I/O happens before timing; each parser gets a warm-up pass then the **min of N**
full passes (default `--iters 5`).

## Results (2026-07-04, /aux dev box, `cargo 1.96`, release + thin-LTO)

**Representative regime — per-file / per-block, i.e. how lsdoc and Tine actually parse:**

| corpus | lsdoc MB/s | peer MB/s | verdict |
|---|---:|---:|---|
| logseq/docs (md, 313 files, 543 KB) | 6.5 | comrak 24.3 | **lsdoc 3.8× slower** (pulldown ceiling 61 → 9.4×) |
| worg (org, 293 files, 7.6 MB) | 13.5 | orgize 44.0 | **lsdoc 3.3× slower** |
| doc.org (org, 1 file, 1.2 MB) | 13.4 | orgize 29.1 | lsdoc 2.2× slower |

`lsdoc::parse` and `lsdoc::parse_format` (+refs) come out ~equal, so the Logseq **ref
index is not the cost** — block+inline construction is.

**Scaling (1×/2×/4× on one concatenated input) — the real-content O(n²) guard:**

| corpus | lsdoc per-doubling | peer per-doubling |
|---|---|---|
| md (logseq/docs concat) | 2.02×, 2.03× | comrak 2.02×, 1.99× |
| org (worg concat) | 1.79×, 2.02× | orgize 1.90×, 2.16× |

**lsdoc is cleanly linear on real content** — the divergence-fixing grind has *not* left a
real-content quadratic. (Note: on a *single* multi-MB document lsdoc's per-byte constant is
much higher than in the per-file regime — an allocation/working-set effect on giant owned
ASTs, not super-linearity, and not representative of block-based use.)

## Bottom line

lsdoc is **~3–4× slower than best-in-class** on realistic per-file input — it does **not**
currently clear the "within ~50% of comrak/orgize" bar. What it *does* clear: it is
comfortably linear, and it stays well ahead of mldoc. The gap is real headroom, not a
crisis, and part of it is legitimate (below).

## Fairness caveats — read before quoting a number

- **Throughput, not semantics.** comrak/orgize parse the same bytes into *their* trees.
  Valid "bytes → tree, how fast"; **not** apples-to-apples parity.
- **pulldown-cmark is a ceiling, not a peer** — a pull parser that builds *no owned tree*,
  so it does strictly less work than lsdoc. Never read "9× vs pulldown" as a fair gap.
- **Logseq tax.** lsdoc resolves `[[page]]`, `#tag`, `((block))`, `{{macro}}`, timestamps,
  math; comrak (default CommonMark) and orgize treat most of these as plain text. Some of
  the 3–4× is legitimate extra work lsdoc *must* do and the peers skip. comrak is run with
  **default options** (fastest, no GFM) — the most conservative (hardest-on-lsdoc) peer.
- **Allocator / tree representation.** comrak uses a `typed-arena`; orgize a rowan green
  tree with interning — both cache-friendly, few allocations. lsdoc builds owned
  `Vec<Block>` / `Vec<Inline>` / `String`. This is the most likely dominant lever if the
  gap is worth closing (it shows up hardest in the giant-single-document regime).
- Small-file corpora fold per-file fixed costs into MB/s (why absolute numbers look low);
  the *ratio* is the robust part and is stable across regimes.

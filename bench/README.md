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

# Local/private real graph, when mounted:
./target/release/lsdoc-bench --graph ~/research/brain
```

`--graph` walks `pages/` + `journals/` (if present) and dispatches md/org by extension.
All file I/O happens before timing; each parser gets a warm-up pass then the **min of N**
full passes (default `--iters 5`).

## Results (2026-07-07, /aux dev box, `cargo 1.96`, release + thin-LTO, v2 public parser)

**Representative regime — per-file / per-block, i.e. how lsdoc and Tine actually parse:**

| corpus | lsdoc MB/s | peer MB/s | verdict |
|---|---:|---:|---|
| logseq/docs (md, 313 files, 542.5 KB) | 67.5 | comrak 99.8 | **lsdoc 1.48× slower** (pulldown ceiling 238.4 → 3.53×) |
| worg (org, 293 files, 7.64 MB) | 113.2 | orgize 163.9 | **lsdoc 1.45× slower** |
| private brain graph (md, 261 files, 3.51 MB) | 128.0 | comrak 189.3 | **lsdoc 1.48× slower** (pulldown ceiling 490.2 → 3.83×) |

`lsdoc::parse` is now block-only; `lsdoc::parse_format` (+refs) is ~3–15% slower on these
corpora, so the Logseq **ref index is a small tax, not the main cost** — block+inline
construction is.

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

lsdoc v2 is now **~1.45-1.48× slower than best-in-class fair peers** on the representative
public and private corpora, clearing the "within ~50% of comrak/orgize" target in this
per-file/block regime. It remains comfortably linear in the dedicated perf/complexity gates
and stays well ahead of mldoc. The remaining gap is real headroom, and part of it is
legitimate (below).

## Fairness caveats — read before quoting a number

- **Throughput, not semantics.** comrak/orgize parse the same bytes into *their* trees.
  Valid "bytes → tree, how fast"; **not** apples-to-apples parity.
- **pulldown-cmark is a ceiling, not a peer** — a pull parser that builds *no owned tree*,
  so it does strictly less work than lsdoc. Never read "9× vs pulldown" as a fair gap.
- **Logseq tax.** lsdoc resolves `[[page]]`, `#tag`, `((block))`, `{{macro}}`, timestamps,
  math; comrak (default CommonMark) and orgize treat most of these as plain text. Some of
  the 3–4× is legitimate extra work lsdoc *must* do and the peers skip. comrak is run with
  **default options** (fastest, no GFM) — the most conservative (hardest-on-lsdoc) peer.
- **Where the gap actually lives (probed 2026-07-04).** A structure-light vs structure-heavy
  probe splits the hypothesis space: on near-plain prose (few AST nodes → few allocations)
  lsdoc is **15.5×** behind comrak (90 vs 5.8 ns/byte); on markup-dense input (allocation-heavy
  for both) only **2×**. So final-AST allocation (owned Vec/String vs comrak's arena) is NOT
  the dominant cost — the **plain-text path** is: ~5 byte-scanning passes (split_lines,
  build_indexes, block dispatch, lex, resolve) and every plain byte copied 2–3× through
  intermediate Strings (lexer `Text` token → resolver `pending` → `Plain` node), vs comrak's
  ~2 passes with a memchr-style skip-to-next-special fast path over borrowed slices. Real
  graphs are prose-heavy, hence the original ~3.8× aggregate. The 2026-07-07 top-level
  plain-inline/page-ref/Markdown-bracket/Markdown-link/image/tag/common-URL/macro/block-ref/keyword-timestamp/LaTeX/Markdown-angle/Markdown-escape/entity/identifier-underscore/single-equals/braced-script/code-span/Markdown-delimiter-state/Markdown-emphasis/Org-bracket/Org-angle/Org-slash-plus-emphasis/Org-code-verbatim/Org-emphasis fast path, direct plain/break-only inline builder, Markdown link raw-delimiter floor, borrowed paragraph runs, ordinary-paragraph dispatch guard, sparse source-event indexing, bounded source hiccup delimiter scan, cached line prefixes, and allocation pre-sizing
  block-only `parse` moved the representative md/org numbers from 25.1/48.1 MB/s to
  67.1/114.1 MB/s, clearing the 50% target in the representative per-file regime. The private brain graph improved
  to 129.9 MB/s vs comrak's 188.5 MB/s after Markdown `_`/`^` delimiter fallback,
  bracket fallback, and conservative backslash escape/entity handling joined the shared fast path,
  confirming that prose-heavy mixed inline buffers are
  still the dominant gap even after generated identifiers like `FLUSH_ERROR` and ordinary
  underscore prose stay on the fast path. The remaining lever is deeper
  copy/scanner elimination in mixed-markup inline resolution, with arena ownership still
  secondary unless a new profile says otherwise.
- Small-file corpora fold per-file fixed costs into MB/s (why absolute numbers look low);
  the *ratio* is the robust part and is stable across regimes.

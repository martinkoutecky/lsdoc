# lsdoc — repo-local working notes

lsdoc is a from-scratch native-Rust reimplementation of Logseq's `mldoc@1.5.7`
(Markdown + Org → typed AST), **byte-exact** to mldoc and gated by a differential
oracle harness. Consumed by Tine as a public AGPL git-dependency.

## Build & gate (run from the repo root)

```sh
source scripts/env.sh                 # shared /aux toolchain — REQUIRED before cargo
cd harness && node run.mjs            # corpus + blockgate + inlinegate, exits non-zero on any diff
node fuzz.mjs 40000 99                # append `org` for org. FLOOR IS ZERO (both formats, blocks+refs,
                                      # any seed) — ANY mismatch = regression or new divergence (file a
                                      # D-entry in DIVERGENCES.md); adjudicate with vdiff_iso (isolated)
cargo test --lib                      # unit tests
cargo test --test render              # render_html tests
cargo test --release --test perf -- --ignored   # perf ratio + linearity + stack gates
cargo test --test complexity          # DEBUG op-count gate — the structural O(n) guard (see below)
```

**The complexity gate is the load-bearing O(n) check** — NOT the perf/parity gates. `src/metrics.rs`
counts "scan work" (bytes examined by re-scanning ops: the `>`-peel, `property`'s `::` search, the
hiccup scan, the inline `resync` re-lex); `tests/complexity.rs` asserts scan-work-PER-BYTE stays
~constant across n/2n/4n. It is **deterministic** (not timed → no flakiness) and shape-independent,
so it catches O(n²) re-scans that the byte-exact parity gate is structurally blind to (the 2026-07
audit found four O(n²) families at 1321/1321 green). The counter is `#[cfg(debug_assertions)]` —
zero cost in release / Tine. `complexity_gate_targets` (`#[ignore]`) holds the audit's not-yet-fixed
O(n²) families; each moves into `complexity_gate` as its single-pass fix lands. **Any new re-scan
must be gated here** — add the family to `complexity_gate`.

The `--ignored` perf tests also pass (`quote_staircase_uncapped_heavy` locks the `>`-staircase
uncapped after the container-frame rewrite).

The gate compares a **normalized projection** (`harness/lib/normalize.mjs` ↔ `src/projection.rs`);
the two emitters MUST stay in sync. Any divergence = a real behavior bug, never "rounding".

**⚠ The oracle leaks global state across parses in ONE process.** `oracle.mjs` loads `mldoc` once
and calls `Mldoc.parseJson` per input, so a prior parse can contaminate a later one — e.g. `$$$`
before `$$$$` flips `$$$$` from `displayed_math("")` (its true, isolated value) to `paragraph`.
`run.mjs`/`fuzz.mjs` parse batched, so the curated corpus is kept **contamination-clean** (that's
what 1362/1362 verifies) — but a hand-written probe run through `oracle.mjs` batched can show FALSE
divergences (and mask real ones). lsdoc is stateless (fresh per parse) and must match mldoc's
**isolated** behavior. So VERIFY probes with **`node harness/vdiff_iso.mjs <probe.json>`**, which
parses each input in a fresh oracle process (no leak) and diffs vs lsdoc. Use it for any ad-hoc
byte-exactness check; never trust a batched probe for a contamination-prone construct.

## Performance principle — avoid hashes if an array would do

**Prefer a deterministic array / direct-index** (or a small sorted `Vec`, a perfect hash for a
fixed set, or a monotone cursor) over a general `HashMap` / `HashSet` **whenever the key is a
small integer, a byte, a position, or a fixed dictionary.**

- Rust's std `HashMap` is **SipHash** — DoS-resistant but slow for small keys, and only O(1)
  *expected* (O(n) worst-case on collisions). A parser eats arbitrary input, so "expected" is a
  real liability, not a formality.
- Real O(n) parsers use deterministic structures on the hot path, never a general hash: cmark
  (char dispatch + delimiter/block stacks), tree-sitter (table DFA), keyword/closer matching via
  `gperf` perfect hash or a trie/DFA, simdjson (SIMD + memcmp).
- **Cautionary tale:** lsdoc once hashed a `u8` (the `EndTrie` children map) — hashing a byte is
  strictly more work than indexing by it. Don't.

This is a *constant-factor + determinism* principle, not an asymptotic one: on real input the log
factor is invisible. The reason to follow it is a measurably faster, collision-proof hot path —
**not** a dramatic speedup. Profile before claiming a magnitude.

## O(n) by construction

The block phase classifies each line once (monotone `i`, no re-scan); closer/fence/drawer lookups
use **monotone cursors** (advance-only), not binary search. Container bodies are **frames on an
explicit heap stack**, never copied or re-lexed: `#+BEGIN_X` bodies are zero-copy strip-view frames
(the de-indent is a lazy per-line view; non-all-ws lines use the cumulative fast path, while nested
all-ws lines use the exact D35 segment-tree exception below), and `>`-quotes are `>`-container frames
(the staircase unrolls iteratively, each line viewed once at its own `gt_level`). So deep nesting is
O(depth) HEAP + O(n) time except for the documented D35 log-factor all-ws path, with NO depth cap on
any realistic shape. Verify O(n) by a *structural* audit (no `.sort` / `partition_point` /
`binary_search` / `HashMap` left on the hot path, no per-level body copy, no unbounded native
re-dispatch), not by a perf ratio — the gate's ~2×/doubling can't distinguish O(n) from O(n log n).

Three deliberate exceptions, all documented and none a regression:
- **`refs.rs` sort+dedup** — O(R log R), R = ref occurrences ≤ n; also the canonical output order the
  order-sensitive gate requires.
- **`GT_FALLBACK_NEST_CAP` (= 64)** — an anti-SIGABRT recursion floor on the SOLE remaining native
  re-dispatch: the de-`>` reparse of a `>`-quote body containing a fence / `#+BEGIN` / LaTeX env /
  hiccup (recognizers that can't see through literal `>`s). It bounds ONLY construct-in-`>`-quote
  nesting (needs ~quadratic input for linear depth, fuzz-unreachable, where mldoc itself SIGABRTs);
  lsdoc degrades it to a flat Paragraph rather than crashing. It does NOT bound any realistic parse.
- **D35 sequential all-whitespace clear-indent tree** — O((frames + all-ws bytes) log depth), exact
  mldoc semantics for nested positive `#+BEGIN_X` indent frames where all-whitespace `safe_sub`
  no-ops do not compose cumulatively. The min segment tree is Vec-backed, excludes zero increments,
  and charges push/pop/query descent nodes via `scan_work`.
- **inline hiccup closer index** (`build_hiccup_close_sparse`) — the sparse `[:`→`]` pairs are
  filled at open time (opener-sorted by construction, no sort), but the inline-context lookup
  (`hiccup_sparse_close_at`) is still a `binary_search`: O(H log H) over H = hiccup openers ≤ n, on
  the inline-only path (property values / previews). The BLOCK path (`HiccupClosers`) is fully O(n) —
  a monotone cursor, no sort, no binary search. Both had an O(H log H) `sort_unstable` until audit4 F9.

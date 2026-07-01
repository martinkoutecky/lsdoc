# lsdoc — repo-local working notes

lsdoc is a from-scratch native-Rust reimplementation of Logseq's `mldoc@1.5.7`
(Markdown + Org → typed AST), **byte-exact** to mldoc and gated by a differential
oracle harness. Consumed by Tine as a public AGPL git-dependency.

## Build & gate (run from the repo root)

```sh
source scripts/env.sh                 # shared /aux toolchain — REQUIRED before cargo
cd harness && node run.mjs            # corpus + blockgate + inlinegate, exits non-zero on any diff
node fuzz.mjs 40000 99                # md fuzz floor 555;  append `org` for the org floor 1522
cargo test --lib                      # unit tests
cargo test --test render              # render_html tests
cargo test --release --test perf -- --ignored   # perf ratio + linearity + stack gates
```

All perf tests pass. (`md_hiccup_nested_scales_linearly_heavy` / `org_hiccup_nested` lock the
block-hiccup remainder loop at O(n) — it was once O(n²) via a per-vector re-dispatch that re-ran
`property`'s O(line) `find("::")` on the shrinking tail; fixed in parse.rs 11d' / org.rs 13b.)

The gate compares a **normalized projection** (`harness/lib/normalize.mjs` ↔ `src/projection.rs`);
the two emitters MUST stay in sync. Any divergence = a real behavior bug, never "rounding".

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
use **monotone cursors** (advance-only), not binary search. Verify O(n) by a *structural* audit
(no `.sort` / `partition_point` / `binary_search` / `HashMap` left on the hot path), not by a perf
ratio — the perf gate's ~2×/doubling can't distinguish O(n) from O(n log n). The one allowed
O(R log R) corner is `refs.rs`'s sort+dedup (R = ref occurrences ≤ n; also the canonical output
order the order-sensitive gate requires) — documented, not a regression.

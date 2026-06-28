# lsdoc

A from-scratch, native-Rust parser for **Logseq-flavored Markdown** (and, later,
Org) → a typed, `serde`-serializable **AST with source spans**, behavior-equivalent
to Logseq's [`mldoc`](https://github.com/logseq/mldoc) at the granularity that
matters for indexing and rendering.

It is the intended single source of truth for parsing in **Tine** (sibling
outliner), replacing Tine's two divergent parsers (`refs.rs` + `parseInline.ts`).
See [`SPEC.md`](SPEC.md) for the full brief and [`DECISIONS.md`](DECISIONS.md) for
the design log (mldoc quirks, intentional deviations, complexity decisions).

## Status

**Markdown AND Org complete.** One differential gate over **516 inputs** (adversarial
+ mined mldoc/OG test suites + real Markdown graph + real Org graph), both formats:
**refs 516/516, block-struct & blocks-full 0 diffs** (11 documented allowlist
deviations, all Markdown); real content — `~/research/tine-test` (md) AND
`~/research/org-graph` (org) — is 0-diff; fuzzing is panic-free over 60k+ inputs; the
perf suite is linear and stack-bounded for both formats. Milestone order (each gated
by "0 oracle diffs on its slice + perf budgets hold"):

1. ✅ Harness / oracle / corpus / normalization + regression loop
2. ✅ Block structure (paragraphs, headings, lists, code fences, properties, quotes, hr, tables)
3. ✅ Inline core (text, emphasis, code, links/images, autolinks, escapes)
4. ✅ Logseq dialect inline (`[[page]]`, `#tag`, `((uuid))`, `{{macros}}`, math, timestamps)
5. ✅ Hardening (differential fuzz, perf + stack-overflow gate, real-graph diff)
6. ✅ Org mode (headlines, markers/priority/tags, org emphasis, `[[t][l]]` links, `#+` directives/blocks, drawers)

Remaining (future, not built): Tine integration — cross the DTO boundary with the
serde AST, delete `parseInline.ts`, repoint `refs.rs`.

## The oracle

Correctness is checked **differentially against real mldoc** (`mldoc@1.5.7`, the
version OG pins), run under Node:

```
input string
  → mldoc Mldoc.parseJson (JSON AST)        # harness/, the reference
  → normalized "observable" projection      # block structure + inline tree + ref set
  ↕ compared against
  → lsdoc parse → same normalized projection
```

We do **not** bind to mldoc's exact internal node identity (some of it is
legacy/quirky). We compare on a normalized projection: block kind/level/nesting/
properties, the ordered inline tree (kind + payload), and the OG-faithful ref set
(page/block/tag/embed, UUID-gated as `block.cljs` does it). **Spans are excluded**
from the comparison (mldoc emits no inline spans and its block spans are quirky);
lsdoc tracks spans internally and verifies them with its own unit tests. Intentional
deviations live on a small, documented allowlist in `DECISIONS.md`.

Correctness is necessary but **not sufficient**: a separate adversarial **perf**
suite and a **fuzz** loop guard against `O(n²)`/`O(2^n)`/stack-overflow behavior
that passes every correctness diff. No parser phase is worse than `O(n log n)`
without a written justification in `DECISIONS.md`.

## Running

```sh
source scripts/env.sh                  # shared Rust toolchain on /aux (cargo 1.96)
cargo test                             # unit tests + fast perf/stack smoke
cargo test --release -- --ignored      # full-scale perf + stack-overflow gate

# Oracle harness (Node 20):
cd harness && npm install              # installs mldoc@1.5.7 (once)

# One-command differential regression loop (the dev gate):
#   corpus (inline + block + mined + real) → mldoc oracle → lsdoc → compare → report
node run.mjs                           # exits non-zero on any divergence
node run.mjs --no-gen                  # skip corpus regeneration
node fuzz.mjs [count] [seed]           # differential fuzz (panic + oracle-mismatch)
node fuzz-triage.mjs [count] [seed]    # bucket fuzz mismatches by structure
#   run.mjs writes divergences.json (drill-down); probe.mjs dumps raw mldoc AST.
```

## Layout

- `src/` — the parser crate (standalone; no dependency on Tine).
- `harness/` — the live Node oracle + differential regression loop.
- `bootstrap/` — the 2026-06-28 divergence-spike output: seed corpus, the
  `block.cljs`-faithful mldoc oracle, and `FINDINGS.md` (read it). Treated as the
  seed for `harness/`, not rebuilt.
- `scripts/env.sh` — sources the shared Rust toolchain.

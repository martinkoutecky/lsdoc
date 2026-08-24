# lsdoc backlog current-state audit — 2026-08-24

Base: `c79cb059da5b4360ebde2e5fd953fa1f43ddabc3` (`v0.5.5`).

The backlog items dispatched as a parity/single-pass lane were historical. The
current source already contains their implementations, so this audit made no
parser behavior change.

## Closed entries and evidence

| Backlog entry | Current evidence |
|---|---|
| D10-D12 raw-HTML unification | `b85f4d7` routes block and inline HTML through the shared source-faithful parser. `harness/raw-html-probe.json`: 56/56 isolated cases match. |
| D13 Markdown link-label reparsing | `2c77af8`; `harness/c2-links-probe.json`: 41/41 isolated cases match. |
| D14 timestamp ordering | `187ecbf`; `harness/c3-my-probe.json`: 31/31 isolated cases match. |
| D15 punctuated drawer names | `931a2a5`; the D15 controls in `harness/c7-my-probe.json` match (70/70 for the full probe). |
| GH #5 / reported real-graph divergences | The permanent isolated gate passes all 404 cases in `harness/reported-divergences.json`, including `issue5_01` through `issue5_04`. |
| Replace the optimistic scanner | `5924d4d` introduced the source-transcribed v2 parser; `92655d9` made the two-phase v2 parser the released public path. `src/v2/source.rs` owns the monotone source/event pass and `src/v2/block.rs` owns block construction. |
| M11 old inline scanners | The named `Scanner` and `OrgScanner` types are absent. Public parsing has no legacy fallback; the explicit legacy harness hook remains intentionally available for differential provenance. |
| Org checkbox parse coverage | Markdown and Org checkbox cases are present in the generated block corpora, v2 list tests, legacy parity tests, render tests, fuzz vocabulary, and transformed-context probes. |
| Plain-text fast path and copy elimination | v2 shipped broad conservative plain/construct fast paths; `0001292` removed lexer text-token copies and `e283e2d` added byte-run scanning. The representative public benchmark remains inside the approved 1.5x fair-peer target. |

## Verification receipt

- `harness/reported-divergences.mjs`: 404/404 match.
- Focused isolated D10-D15 probes: 56/56, 41/41, 31/31, and 70/70 match.
- `harness/run.mjs`: 1402/1402 refs and block projections match; real block
  gate 99/99; inline 37/37; spans zero violations; shortcut audit 7533/7533.
- `harness/fuzz.mjs 40000 99`: zero Markdown mismatches.
- `harness/fuzz.mjs 40000 99 org`: zero Org mismatches.
- `cargo test --lib`: 196 passed.
- `cargo test --test render`: 47 passed.
- `cargo test --test complexity`: both active gates passed; the target gate is
  intentionally empty because all audited quadratic families are in the green
  gate.
- `cargo test --release --test perf -- --ignored`: 7/7 passed.
- Public benchmark, min of five on the existing corpora:
  - logseq/docs Markdown: lsdoc 70.9 MB/s, comrak 101.2 MB/s — lsdoc is
    1.43x slower;
  - worg Org: lsdoc 125.1 MB/s, orgize 164.9 MB/s — lsdoc is 1.32x slower.

The plain-text performance trigger therefore does not justify another parser
rewrite: the previously approved implementation already clears the <=1.5x
representative acceptance boundary.

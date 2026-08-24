# lsdoc — Backlog & Triage

Not-yet-done work for lsdoc (the Logseq/mldoc-compatible parser). Kept separate from Tine's
backlog because lsdoc is a separate repo. Detailed design/decisions live in `DESIGN-lsdoc-v2.md`,
`DECISIONS.md`, and `DIVERGENCES.md`; this is the prioritized index.

Categories: **In flight** / **P1** / **P2** / **Deferred** (genuinely-later, not WONTFIX).

---

## In flight

None.

The 2026-08-24 current-state audit removed stale entries for the v2 single-pass
rebuild, raw-HTML unification, D10-D15, the old inline scanner cleanup, Org
checkbox coverage, and the plain-text fast path. They were already implemented
and released; the proof receipt is
`docs/audits/lsdoc-backlog-current-state-2026-08-24.md`.

---

## P1 — divergences to close (byte-exact vs mldoc)

None currently known. `DIVERGENCES.md` and the permanent reported-divergence
gate remain authoritative if a new mismatch is found.

---

## P2 — cleanup & analysis (behavior-preserving)

| Item | Notes |
|---|---|
| **P2 unification opportunities** | Analysis-only, behavior-preserving: list / display-math / quote-helper / bracket-scan dedup; inline-ctx boolean-bags. From the lsdoc-vs-mldoc audit. Needs Martin's approval before applying. |

---

## Deferred — genuinely later, no slot yet (NOT WONTFIX)

| Item | Notes |
|---|---|
| **M7 — explicit `lex_lines` line-lexer** | Would be dead code after the M8/M9 block rewrite already hit O(n); a large lateral rewire for stylistic uniformity, zero perf/correctness gain. Only if a focused clarity pass is wanted. |
| **Consumer-recursion → iterative project/serialize** | The deep Block tree's recursive drop/project/serialize is bounded by ~6k stack frames (strictly better than mldoc's ~1000; adversarial-only input). Making it iterative removes the ceiling; explainer owed. |
| **Hiccup `[:tag …]` → HTML render** | Clojure hiccup renders as literal text, not HTML. Low-priority parity gap. |
| **audit4 F10 — raw-HTML per-distinct-tag full-input scan** | `RawHtmlScan::tag_index` builds one `RawHtmlTagIndex` per DISTINCT tag lazily, and each `RawHtmlTagIndex::build` (`block_common.rs`) walks the whole input once. A block with K distinct allowlisted tags therefore pays K full passes — **measured 25.6 work/byte at 20 distinct tags vs 6.2 for one repeated tag** (verified 2026-07-21). BOUNDED (≤110, the allowlist size) so it is not an asymptotic violation, but a real constant-factor cliff on paste/import blocks with varied HTML. **Fix direction:** build raw-HTML events in ONE source pass, dispatching each recognized `<tag>`/`</tag>`/`<tag/>` to a direct-indexed per-tag event vector, then keep the existing monotone-cursor query model — removes the `distinct_tags × input_len` multiplier. **Deferred, not WONTFIX:** the raw-HTML matcher is the parser's most intricate, regression-prone component (repo history has many raw-HTML O(n²)/parity fixes); a single-pass rewrite there is disproportionate risk for a bounded, rare constant-factor. Do it behind the full gate + a new 20-distinct-tag complexity family only when raw-HTML-heavy import is actually felt. |

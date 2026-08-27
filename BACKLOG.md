# lsdoc — Backlog & Triage

Not-yet-done work for lsdoc (the Logseq/mldoc-compatible parser). Kept separate from Tine's
backlog because lsdoc is a separate repo. Detailed design/decisions live in `DESIGN-lsdoc-v2.md`,
`DECISIONS.md`, and `DIVERGENCES.md`; this is the prioritized index.

Categories: **In flight** / **P1** / **P2** / **Deferred** (genuinely-later, not WONTFIX).

---

## In flight

None currently.

## Recently completed

| Item | Receipt |
|---|---|
| <a id="f10-raw-html-index"></a>**F10 — single-pass raw-HTML tag index** | One source pass now dispatches recognized opens/closes into direct-indexed tag families while retaining exact-case opener variants and monotone query cursors. The deterministic 20-distinct-tag family is below the fixed work/byte ceiling. |
| <a id="hiccup-html"></a>**Hiccup `[:tag …]` → HTML render** | The bundled renderer now reads block and inline hiccup into allowlisted markup, retaining the parsed wire AST and rejecting unsafe tags, event attributes, and script URLs. |
| <a id="iterative-consumers"></a>**Consumer recursion → iterative project/serialize** | `projection_to_json` / `blocks_to_json` use explicit work stacks; the differential CLI uses the projection path, and a 20,000-level small-stack regression passes. |
| <a id="m7-lex-lines"></a>**M7 — explicit `lex_lines` line lexer** | The v2 source boundary is explicit without adding a second scan or changing line/event semantics. |

Detailed implementation and gate evidence: `docs/audits/lsdoc-implement-queue-2026-08-27.md`.

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
None currently.

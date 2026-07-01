# lsdoc — Backlog & Triage

Not-yet-done work for lsdoc (the Logseq/mldoc-compatible parser). Kept separate from Tine's
backlog because lsdoc is a separate repo. Detailed design/decisions live in `DESIGN-lsdoc-v2.md`,
`DECISIONS.md`, and `DIVERGENCES.md`; this is the prioritized index.

Categories: **In flight** / **P1** / **P2** / **Deferred** (genuinely-later, not WONTFIX).

---

## In flight

| Item | Detail |
|---|---|
| **Single-pass rebuild** — replace the optimistic scanner with a real two-phase lexer: (A) container-prefix walk → (B) hiccup index → (C) inline delimiter stack. **Acceptance = the op-count gate** (`cargo test --test complexity`). | `DESIGN-lsdoc-v2.md`, `PLAN-v0.3.0-deterministic-O-n.md`; the single-pass-audit found the optimistic scanner recreated Tine's O(n²) flaw. |
| **Raw-HTML unification (D10/D11/D12)** — one source-faithful port of mldoc's `Raw_html.parse`, routed from both block + inline; de-dups 4 look-alike scanners. Approved, awaiting codex dispatch. | `subagent-tasks/raw-html-unification-plan.md` |

---

## P1 — divergences to close (byte-exact vs mldoc)

Tracked in `DIVERGENCES.md`; fold into the divergence loop.

| Item | Example |
|---|---|
| **D13 — md link-label doesn't reparse entities/latex** | `[\alpha](u)` → Entity, `[$x$](u)` → Latex in mldoc, not in lsdoc |
| **D14 — timestamp order-permissive** | `<… +1d 12:00>` |
| **D15 — md drawer name rejects punctuation** | org drawer name `:LOG@BOOK:` |

---

## P2 — cleanup & analysis (behavior-preserving)

| Item | Notes |
|---|---|
| **M11 — delete both v1 inline scanners** (`inline.rs` Scanner + `org.rs` OrgScanner) + the cache zoo they justified | After the single-pass rebuild lands (they stay as differential oracles until then). Fold keeper tests into `perf.rs`; rewrite `DESIGN-lsdoc-v2.md`. Pure cleanup. |
| **P2 unification opportunities** | Analysis-only, behavior-preserving: list / display-math / quote-helper / bracket-scan dedup; inline-ctx boolean-bags. From the lsdoc-vs-mldoc audit. Needs Martin's approval before applying. |
| **Org-checkbox parse coverage** | Tine's org-page checkbox *toggle* is done + data-safe (Tine side); the residual is lsdoc's org-checkbox *parse* coverage. Low. |

---

## Deferred — genuinely later, no slot yet (NOT WONTFIX)

| Item | Notes |
|---|---|
| **M7 — explicit `lex_lines` line-lexer** | Would be dead code after the M8/M9 block rewrite already hit O(n); a large lateral rewire for stylistic uniformity, zero perf/correctness gain. Only if a focused clarity pass is wanted. |
| **Consumer-recursion → iterative project/serialize** | The deep Block tree's recursive drop/project/serialize is bounded by ~6k stack frames (strictly better than mldoc's ~1000; adversarial-only input). Making it iterative removes the ceiling; explainer owed. |
| **Hiccup `[:tag …]` → HTML render** | Clojure hiccup renders as literal text, not HTML. Low-priority parity gap. |

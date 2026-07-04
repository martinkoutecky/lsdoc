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
| **Plain-text fast path + copy elimination** (close the comrak throughput gap) | Jul 4 2026 `bench/` result (public corpora logseq/docs + worg): lsdoc ~3.3–3.8×/byte behind comrak (md) / orgize (org), but cleanly linear. A plain-vs-dense probe *located* the gap: **15.5×** behind comrak on near-plain prose (90 vs 5.8 ns/byte — almost nothing to allocate) yet only **2×** on markup-dense input ⇒ the cost is the **plain-text path**, NOT final-AST allocation (an arena rewrite is NOT the lever; that hypothesis was tested and refuted). Anatomy: ~5 byte-scanning passes (`split_lines` → `build_indexes` → block dispatch → `lex` → `resolve`) and every plain byte copied 2–3× through intermediate Strings (lexer `Text` token `lexer.rs:101,124` → resolver `pending` via `append_text!` `resolver.rs:1557` → `Plain` node; + `para_buf` `parse.rs:2398` in remap frames). **The fix:** a memchr-style skip-to-next-special-byte scan for plain runs in the lexer, carrying borrowed slices/spans instead of owned Strings until the final `Plain` node (merge the lexer/resolver plain handling to cut 2 copies → ≤1; escape/entity/CR slow paths unchanged). Expected ≈2–3× overall on real graphs (prose-heavy); the dense path is already within 2× of comrak. **Gates that must stay green:** oracle 0-diff (`run.mjs`), fuzz floor 0 both formats, `cargo test --test complexity` (keep charging `scan_work` on the new path), span invariants S1–S5 (slice-carrying actually *aligns* with spans). **Acceptance:** `bench/` ratio vs comrak on the logseq-docs corpus from 3.8× → ≤1.5×. **Trigger:** only if whole-graph parse ever becomes felt in Tine (graph-open warm is background-paced; ~1–2 s per 10 MB today) — not urgent, well-understood, bounded. |
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

# bootstrap/ — spike output (read before writing parser code)

This is the output of the parser-divergence spike (2026-06-28). It is your **oracle
skeleton** and your **first regression corpus**. Don't rebuild it — extend it.

## What's here
- `FINDINGS.md` — the full spike report: ranked divergence tables (Prong A = Tine's two
  parsers vs each other; Prong B = Tine vs OG/mldoc), root-cause analysis, and the oracle
  verdict. **Read this first.**
- `harness/corpus.gen.mjs` → `corpus.json` — 157 adversarial inputs across 12 categories
  (brackets, parens, code, link, escape, emph, prop, tag, url, macro, unicode, misc). This
  is the seed regression set; grow it.
- `harness/mldoc/` — throwaway npm project pinning `mldoc@1.5.7`. `mldoc-runner.mjs` is the
  **faithful oracle**: it walks `Mldoc.parseJson` and ports OG's `block.cljs` reference
  extraction (NOT the shallow `Mldoc.getReferences`, which is wrong — see FINDINGS §oracle).
  Run `npm install` here first (node_modules was not copied).
- `harness/rust-runner/` — standalone cargo project with `tine-core = { path = … }` calling
  `refs.rs`. **The path in `Cargo.toml` points at the original scratchpad/Tine location —
  fix it** when you adopt this.
- `harness/ts-runner/` — standalone vitest project importing the real `parseInline.ts` by
  absolute path. node_modules was a symlink to Tine's; re-point or `npm i`.
- `harness/{rust,ts,mldoc}-out.json`, `divergences.json` — reference outputs from the spike
  run, so you can diff your reproduction against known-good.

## How the oracle works (the design you inherit)
input string → mldoc `parseJson` (JSON AST) → **OG `block.cljs` extraction** (page refs
from `Link Page_ref` + `Tag`/`get-tag` + **embed-macro args only**; block refs from
`Link Block_ref` + embed, **both `parse-uuid`-gated**) → normalized ref set. Compare *that*
against your Rust parser's ref set. mldoc raw ≠ OG: mldoc keeps `((non-uuid))` as a
Block_ref; OG drops it. So the oracle must encode OG's post-mldoc rules, not mldoc raw.

## Reproduce (from the original scratchpad; adapt paths)
```
node corpus.gen.mjs
( cd mldoc && npm install && node mldoc-runner.mjs )
( source <tine>/scripts/env.sh && cd rust-runner && cargo run -q )   # fix Cargo.toml path
<tine>/node_modules/.bin/vitest run --config ./ts-runner/vitest.config.ts
node compare.mjs
```

## Caveats to carry forward (from FINDINGS, don't lose these)
1. **Oracle granularity:** compare on the normalized observable ref set, not mldoc's raw
   AST node identity.
2. **Tag boundary is unresolved ground truth:** mldoc 1.5.7 + OG `block.cljs` treat
   `word#tag` / `c#sharp` as tags; Tine's `refs.rs` deliberately rejects them (word-boundary
   rule, traces to a real past decision about not tagging `…#fragment` in URLs). This needs
   a human spot-check against the **real OG app** before it's encoded as ground truth — and
   it may be a deliberate Tine-better-than-OG deviation, not a bug.
3. **Layer mismatch:** `parseInline` is inline-only; Tine's block layer handles fenced code
   separately. Multi-line-fence "divergences" must be compared at the right layer or they're
   false positives.
4. **Random fuzzing is weak for refs** (~0 signal on normal text); bias the generator toward
   the adversarial categories.

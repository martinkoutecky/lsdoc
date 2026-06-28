# DECISIONS

Design log: mldoc quirks discovered, intentional deviations chosen, and
performance decisions. The "why" lives here. Newest entries at the bottom of each
section.

## Environment / infra

- **Toolchain.** Shared Rust toolchain on the persistent `/aux` mount
  (`/aux/koutecky/logseq/.toolchain/{cargo,rustup}`), sourced via
  `scripts/env.sh`. lsdoc is standalone (no dependency on Tine's `env.sh` or its
  browser tooling) but reuses the same toolchain. cargo 1.96, edition 2024.
- **Oracle = `mldoc@1.5.7` under Node** (the version OG pins). Installed in
  `harness/`. `package-lock.json` is committed for a reproducible oracle;
  `node_modules` is git-ignored.
- **Harness relationship to `bootstrap/`.** `bootstrap/` is the immutable record
  of the 2026-06-28 divergence spike (seed corpus, `block.cljs`-faithful oracle,
  `FINDINGS.md`). The live, maintained oracle lives in `harness/`, seeded from it
  (paths fixed, projection enriched) — extended, not rebuilt.

## Markdown realism corpus (Martin, 2026-06-28)

- The §8 Markdown DoD names `~/research/org-graph` as the real-graph gate, but
  that graph is **all Org** (16 `.org`, 0 `.md`). So for the **Markdown**
  milestone the realism gate is **`~/research/tine-test` (7 `.md`) +
  `kitchen-sink.md`**; `~/research/org-graph` becomes the **Org**-milestone gate.
  Martin's personal `~/research/brain` (232 `.md`) is deliberately **left out** of
  the loop.
- **Scope of the current run:** loop milestones 1–5 to the §8 first-cut DoD, then
  stop and report. Org (milestone 6) is deferred to a later session.

## Oracle granularity / normalized projection

We compare a **normalized "observable" projection**, not mldoc's raw AST node
identity. Findings from the AST-shape probe (`harness/probe.mjs`, mldoc 1.5.7,
`format:"Markdown"`):

- **Top-level shape:** each block is `[node, {start_pos, end_pos}]`. Block nodes
  carry source spans; **inline segments do NOT** (except `Src`, which has
  `pos_meta`). ⇒ The oracle can diff **block spans**, but inline comparison must
  be on **kind + payload + order + nesting only** — mldoc gives no inline spans to
  diff against. lsdoc still *preserves* inline spans (its own design-for-Tine
  requirement); it just can't validate them against this oracle.
- **Lists are `Heading` nodes**, not a distinct list node: a bullet `- x` parses
  to `["Heading", {unordered:true, level:<indent-derived>, size:null, title:[…]}]`;
  ordered items have `unordered:false, size:<n>`. The normalized projection must
  map these to a list/heading distinction Tine cares about (TBD in milestone 2).
- **Block node kinds seen:** `Paragraph [inline…]`, `Heading {title,tags,level,
  anchor,meta,unordered,size}`, `Property_Drawer [[k,v,[]]…]`, `Quote [block…]`
  (nests blocks), `Src {lines,language,pos_meta}`.
- **Inline node kinds seen:** `Plain str`, `Emphasis [[kind],[inline…]]` (kind ∈
  Bold/Italic/Strike_through/Highlight/…), `Code str`, `Link {url,label,full_text,
  metadata}` (url ∈ `Page_ref name` / `Block_ref id` / `Complex {protocol,link}`),
  `Tag [inline…]` (inline content; `#[[x]]` → `Tag [Link Page_ref…]`),
  `Macro {name,arguments}`, `Latex_Fragment ["Inline"|"Displayed", str]`,
  `Break_Line`.
- **Escaping** is honored by mldoc: `\[[escaped]]` → `Plain "[[escaped]]"` (no
  ref), `\#nottag` → plain text. lsdoc must implement `\` escaping (the single
  biggest bug class in current Tine — neither Tine parser does it).

## Reference semantics (OG-faithful, from `block.cljs`)

- Page refs = `Link Page_ref` value + `Tag` (un-bracketed) + **`embed`-macro arg
  only** (not `query`/`renderer`). Block refs = `Link Block_ref` id + `embed`
  arg, **both `parse-uuid`-gated** (OG drops non-UUID block refs; raw mldoc keeps
  them). So "match mldoc raw" ≠ "match OG" — the oracle encodes OG's post-mldoc
  rules. (Use the `block.cljs` port in `bootstrap/harness/mldoc/mldoc-runner.mjs`,
  NOT the shallow `Mldoc.getReferences`.)

## Tags (RESOLVED — Martin, 2026-06-28; matches OG exactly, no deviation)

- lsdoc tags `#…` exactly like OG/mldoc, **including** glued `c#sharp`→`sharp` and
  accented/CJK/emoji/dotted tags (`#café`, `#中文`, `#😀`, `#a.b`). The URL-fragment
  worry is handled by **tokenizing URLs/autolinks/link-targets first** (a `#frag`
  inside a URL is consumed into the link, never a tag) — NOT by a word-boundary
  rule. Do **not** port `refs.rs`'s word-boundary rule.

## Intentional deviations from mldoc (allowlist)

_(none yet — to be populated as the differential loop finds mldoc quirks we
deliberately don't replicate.)_

## Complexity decisions

_(per-phase complexity recorded here as phases land; target: nothing worse than
O(n log n) without justification. Emphasis resolution uses a single-pass
delimiter-stack algorithm, not recursive backtracking.)_

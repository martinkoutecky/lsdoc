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
- **Spans are UTF-8 byte offsets** (verified: `#café` end=6 bytes not 5 chars;
  `#中文` end=7). This matches Rust `&str` byte indexing exactly — block spans
  compare directly, no char/byte conversion needed on either side.
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

## Spans excluded from comparison (granularity decision)

Block/inline **spans are not part of the differential contract** and are excluded
from the oracle comparison (`compare.mjs` IGNORE_KEYS):
- mldoc emits **no inline spans** (only block-level `start_pos/end_pos`), so inline
  spans can't be diffed at all.
- mldoc's **block spans are quirky/inconsistent**: a `Src` swallows trailing blank
  lines into its span while a `Property_Drawer` doesn't; a lone blank line between
  two block constructs becomes its own `paragraph` block. Binding to that exact
  byte arithmetic is binding to mldoc's internal identity, which SPEC §5 says not to
  do.
lsdoc still **tracks spans internally** (needed by Tine for rendering/click
targets) and verifies them with its own unit tests — just not against this oracle.

## Intentional deviations from mldoc (allowlist)

Tracked in `harness/allowlist.json` (id + reason); `compare.mjs` excludes these
from diff counts but still reports them. Current entries:

- **`c047`** — a fenced code block opened *on a bullet line* (`` - ```calc ``):
  mldoc splits an empty bullet `- ` then a `Src`; lsdoc keeps it as one bullet.
  Adversarial — Logseq writes code under a bullet as a nested child block, not on
  the bullet line. Revisit if a real graph needs it.
- **`b021`** — a nested ordered list via markdown indentation
  (`1. a\n   1. nested`): mldoc folds the nested item into the parent's sub-items
  (normalizes to a `raw` node); lsdoc emits two flat items. Logseq nests via
  outline bullets, not markdown list indentation. Revisit if a real graph needs it.

## M2 block-structure rules (replicated from the oracle)

Single-pass line scanner (`src/parse.rs`, O(n); fences pre-paired to avoid O(n²)):
heading `#{1,n}` + space/EOL (level always 1, size=n); only `-` → `Bullet`
(level = 1 + leading-ws), `*`/`+`/`N.` → `List` (`N)` is not a list); `key:: ` (+
space/EOL, indentation tolerated) → `Property_Drawer`; ` ``` `/`~~~` fences (must
close, else paragraph) → `Src`; `>` → `Quote`; `#+BEGIN_X…#+END_X` → `Quote`
(QUOTE) or `Custom`; `---/***/___` → `Hr`; `|…` → `Table`; `[^n]:` →
`Footnote_Definition`; `<tag…>` (not `<autolink>`) → raw HTML; everything else
(incl. blank lines) coalesces into one `Paragraph`. M2 gate = `block-struct`
(kind/level/nesting/props), which ignores inline content + spans.

## Complexity decisions

_(per-phase complexity recorded here as phases land; target: nothing worse than
O(n log n) without justification. Emphasis resolution uses a single-pass
delimiter-stack algorithm, not recursive backtracking.)_

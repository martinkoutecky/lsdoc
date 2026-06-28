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
- **`m096`/`m097`** — a fenced code block opened *after a bullet prefix*
  (`- ```\ncode\n``` `): mldoc splits an empty bullet `- ` then a `Src`; lsdoc keeps
  the ``` on the bullet line. Same class as `c047` — Logseq nests code under a bullet
  as a child block, not on the bullet line.
- **`m114`/`m115`/`m116`** — a markdown blockquote (and a tab-indented property/fence
  tail) opened *after a bullet prefix* (`  - > line3`): mldoc splits an empty bullet
  then a `Quote` (lazy-joining the next `> ` line across indentation); lsdoc keeps
  `> line3` as the bullet title. Same family as `c047`/`b021` (a block construct
  starting after the bullet prefix); not how Logseq writes outlines.
- **`m135`** — a markdown **definition list** (`term\n: definition`): mldoc emits a
  `List` whose item carries a `name` (the term) with the definition as content; lsdoc
  emits a paragraph. Def-list syntax is niche in Logseq (outline bullets / `key:: value`)
  and the list-item `name` field is outside the modeled projection.
- **`m054`/`m056`** — a LaTeX **named entity** (`\Delta`, `\Delta{}`): mldoc maps it
  to an `Entity` node via a ~2k-entry LaTeX entity table; lsdoc renders an unknown
  `\word` as the bare letters (correct for non-entities). Porting the entity table is
  disproportionate for a math/org construct rare in Logseq Markdown.
- **`m089`** — a LaTeX **environment** block (`\begin{equation}…\end{equation}`):
  mldoc emits a `Latex_Environment` block; lsdoc emits a paragraph. Not modeled in the
  projection; a math/org construct rare in Logseq Markdown.

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

## M3/M4 inline rules (replicated from the oracle + mldoc 1.5.7 source)

The inline parser (`src/inline.rs`) is a single left-to-right byte scanner whose
dispatch mirrors mldoc's `inline_choices` (`lib/syntax/inline.ml`, verified against
the live oracle). On the first byte we pick the one construct mldoc would try; on
failure we fall back to a *plain run*. A marker byte (`* _ ^ [ ~ \` = $ #`) whose
construct fails is emitted as one literal char; an ordinary dispatch byte
(`< { ! @ (`) whose construct fails is swallowed into the following plain run
(they are not `plain` delimiters in mldoc) — this is why `(https://a.com)` stays
plain but `see https://a.com` links.

Constructs handled (parity verified): plain, break (`\n`), hard break (`>=2`
trailing spaces + `\n`), inline code (single `` ` `` and double `` `` `` incl.
*empty* `` `````` ``), emphasis, page refs `[[…]]`, nested links `[[ …[[ ]]… ]]`,
markdown links/images `[l](u)` / `![l](u)` (incl. block-ref `[l](((uuid)))`,
page-ref, file `.md`/`.markdown`, complex `proto://`, search), bare URLs,
autolinks `<scheme:…>`, email `<a@b>`, inline HTML, tags `#…` / `#[[…]]`, block
refs `((…))`, macros `{{…}}` / `{{{…}}}`, latex `$…$` / `$$…$$` / `\(…\)` /
`\[…\]`, timestamps (`<date>`, ranges, `SCHEDULED:`/`DEADLINE:`/`CLOSED:`),
footnote refs `[^id]`, escapes.

Quirks worth knowing (all matched):
- **Emphasis is NOT a CommonMark delimiter stack.** mldoc is recursive-descent
  `between_string`: an opener matches the *first later* valid closer of the same
  marker; content is flat, then re-parsed for nesting. Dispatch tries `***`/`**`/`*`
  (and `___`/`__`/`_`) longest-first; `***x***` → `Italic[Bold[x]]`. Left-flank =
  marker followed by non-whitespace; close = byte before non-whitespace; `_`/`__`
  additionally require the byte *before the opener* and *after the closer* to be an
  ASCII-punct/whitespace delimiter (so `snake_case`, `a_b_c` are NOT italic). The
  first-opener-wins rule gives `*a *b* c*` → `Italic["a *b"] + " c*"` (not the
  CommonMark inner pairing). Empty content is rejected (`d**e` stays plain).
- **Emphasis spans newlines** (mldoc `whitespace_chars` include `\n`), but the `\n`
  is captured as literal plain text inside the emphasis (no `Break` node).
- **Inside emphasis** only emphasis, links/page-refs, sub/sup, code and plain are
  recognized — NOT tags, block-refs, macros, latex, bare URLs, images (`==#tag==`
  keeps `#tag` plain; `**[[Foo]]**` keeps the ref).
- **Code precedence:** at `` ` `` mldoc tries single-backtick first, then double
  (`` `` ``). `` ```[[Foo]]``` `` → `Code "`[[Foo]]"` + `` ` `` (double consumes 2,
  the 3rd is content). Refs never leak out of code; the emphasis closer-search skips
  code spans.
- **Backslash escapes** drop the backslash and make the char literal: `\[[a]]` →
  `[[a]]` (no ref), `\#tag`, `\((u))`, `` \` `` are plain; `\\` → one `\`;
  `\<letter>+` (entity) → the letters; a `\` before a non-escapable char is kept.
  Extracted **values** (page/block-ref names, tag text, URL links) are additionally
  *unescaped* (`\X`→`X` for ASCII punct) while `full_text` stays raw — matching
  mldoc's transform; this affects the OG ref set.
- **Page ref** `[[…]]`: ends at the first `]]`, single `]` allowed inside, no
  newline, non-empty (`[[]]` is plain). `[[name]]` precedence is page-ref *before*
  markdown link, so `[[Foo](bar)]]` → Page_ref "Foo](bar)".
- **Link labels** are parsed by a restricted grammar (emphasis/code/latex only,
  consume-all-or-keep-plain): `[a *b* c](u)` keeps `a *b* c` plain, `[**b**](u)`
  bolds, `[#tag](u)`/`[[[x]]](u)` keep the tag/ref as plain label text.
- **Bare URL tail** (after `/`?`#`) stops only at whitespace or an unmatched `)`/`]`
  (balancing `()`/`[]`), keeps `< > { }`, and drops a trailing `,;.!?` that precedes
  whitespace/EOL. The host part stops at the inline-link delimiters `[]<>{}()`.
- **Tags:** charset is byte-wise non-space, excluding `,;.!?'":#` and `[` (which
  starts a `[[page]]` child); `.`/`;` are kept mid-name but stripped when trailing
  before whitespace/EOL. Unicode/emoji/glued (`c#sharp`→`sharp`) all tag.
- **Macros:** name = up to `}`/`(`/space; args split on `,` with each arg being a
  nested-link / page-ref / `((…))` / `"…"` / run-to-comma; if the args don't fully
  consume, the whole macro fails and re-parses as plain + inner refs
  (`{{embed [[Foo]] ((uuid))}}` → plain + page ref + block ref).
- **Block-layer title stripping** (in `parse.rs`, the only block changes besides
  calling `parse_inline`): heading/bullet titles strip a leading `#{1,n} ` (heading
  in a bullet), then a task **marker** (`TODO `/`DOING `/… ) and **priority**
  (`[#A]`); `*`/`+`/`N.` *list* items also strip a leading checkbox `[ ]`/`[x]`
  (mldoc lists0) — but `-` bullets do NOT. Quote `>` lines are de-prefixed and
  re-parsed as nested blocks; a `Src` swallows trailing blank lines.

## Complexity decisions

- **Block segmentation** (`parse.rs`): O(n), one line scan; fences pre-paired.
- **Inline parse** (`inline.rs`): O(n) amortized. Plain runs, code spans, page/block
  refs, bare URLs and bracket-balanced scans each cover disjoint regions. Emphasis
  uses a forward closer-search per opener bounded by a per-pattern **no-closer
  cache** (once a marker is proven to have no closer ahead, later openers of that
  marker short-circuit), and the open-run length is measured capped at 3 — so even
  `*`×10⁵ or `*a `×n is linear, not O(n²). Runs of unmatched `[` / `(` / `{` are kept
  linear by a monotone **closer-absent cache** (`]]`/`](`/`))`/`}}`), since a 2-byte
  closer absent from position p is absent from every later position. Adversarial
  perf is unit-tested (`inline::tests::adversarial_runs_terminate`, <0.2s). No phase
  is worse than O(n log n).
- Emphasis is single-pass (no recursive trial-and-error / backtracking); matched
  content is re-parsed once on a strictly-smaller substring (bounded by nesting
  depth), never re-scanned on failure.

## Differential fuzzer (M5 seed)

`harness/fuzz.mjs` generates biased-random markdown (adversarial token alphabet),
runs both mldoc and lsdoc, and diffs the projection. No panics over 8k+ inputs
(byte-safety holds on café/中文/😀/zero-width). Residual fuzz-only divergences are
all (a) block-segmenter edge cases deliberately out of M3/M4 scope (lone `>`, bare
`-\n`, loose property/`$$` detection) or (b) deep adversarial bracket/emphasis/macro
nesting in random soup that does not occur in real content (the realism corpus —
`~/research/tine-test` + kitchen-sink — is in the gate and passes 0-diff). Not
allowlisted: they are not gate inputs.

## Mined test corpus (M5)

`harness/corpus.mined.gen.mjs` → `harness/corpus.mined.json` (committed like
`corpus.blocks.json`; generated from the committed generator, self-contained — strings
embedded as data, no clone needed at runtime). Merged into the differential run by
`run.mjs` alongside the inline/block/real corpora (`m###` ids). 202 unique inputs after
dedup against `corpus.json`/`corpus.blocks.json`.

- **Sources.** mldoc's OCaml tests (cloned `logseq/mldoc`, default branch HEAD `bedae99`
  — no `v1.5.7` tag exists, nearest are `v1.5.5`/`v1.5.8`; `test_markdown.ml`/
  `test_outline_markdown.ml` line counts match the 1.5.7-era in SPEC §5): the INPUT is
  the first OCaml string literal after each `check_aux`/`check_aux2` call, decoded with
  full OCaml escape + line-continuation (`\`+newline+blanks) handling — **99**
  (test_markdown), **91** (test_outline_markdown), **14** (test_export_markdown), **0
  skipped**. Plus OG graph-parser cljs tests (`/aux/.../og/.../test/`): markdown input
  strings curated by reading `mldoc_test`/`block_test`/`extract_test`/`text_test`/
  `property_test` — org-format (`:org` config) and org-syntax (`#+TITLE`,
  `[[file:…][…]]`) inputs excluded; a few `(str …)`-built block-ref/timestamp cases
  reconstructed from their literal constants.

- **mldoc behaviors surfaced and replicated** (all real, now matched):
  - **Markdown link destination** (`link_url_part_inner`): the raw between-parens text
    is split into a destination + optional trailing ` "title"`; the title is dropped and
    the destination **value is unescaped** (`\)`→`)`, `\.`→`.`) while `full_text` keeps
    the raw backslash. `<…>` destinations are angle-stripped (inner spaces kept), and
    `[[page-ref]]`/`((block-ref))` parts keep their inner spaces. On a consume-all
    failure (e.g. `((uuid)) extra`) the *whole* raw text is the destination.
  - **Link label** balances single `[…]` brackets (`![lab[el]]…`) and the label **value
    is unescaped** (`\]`→`]`) while `full_text` stays raw.
  - **Emphasis** closer-search skips backslash-escaped chars: `\*`/`` \` `` inside
    emphasis is literal content, not a closer (`*a\*b*` → Italic[`a*b`]).
  - **Tags** parse a nested-link child (`#[[nested [[tag]]]]` → `Tag[Nested_link]`), not
    just a page-ref.
  - **`:PROPERTIES:` drawer** → `Property_Drawer` even in Markdown (mldoc `drawer.ml`),
    with `:key: value` lines as properties (refs walked from the values). A `#+name:
    value` org directive **immediately following a property line is folded into the same
    drawer** (`a:: 1\n#+b: 2` → props a, b) — a standalone `#+…:` is a Directive (not in
    the corpus, left as a paragraph).
  - **Bullets**: `#{1,n}` heading-prefix in a bullet strips at end-of-title too (`- ##`
    → empty bullet), and a lone `-` at end-of-line is an (empty) bullet.
  - **Markdown blockquote** (`md_blockquote`): a `>` line opens a quote whose body is the
    de-`>`'d lines **plus lazy continuation** (following non-`>` lines) until a blank
    line or a new-block line (`- `/`# `/`id:: `/bare `-`/`#`); the body is a **flat
    Paragraph** (with keep_line_break breaks) — the property/heading/bullet parsers are
    NOT applied inside a quote, so `> a:: b` stays a paragraph.
  - **Timestamp repeater** (`+1m`/`++2w`/`.+1d`) parsed into mldoc's
    `repetition:[[kind],[duration],n]` JSON.

  Remaining diffs are allowlisted (see above): LaTeX entity/environment tables (niche),
  markdown definition lists, and block constructs opened after a bullet prefix (the
  `c047`/`b021` adversarial family). No `refs` or `block-struct` diffs remain.

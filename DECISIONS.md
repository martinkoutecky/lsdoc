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
from diff counts but still reports them. **The allowlist is now EMPTY** (`[]`):
the original family was eliminated 2026-06-28 (Martin-approved) — LaTeX
entities/environments (`m054`/`m056`/`m089`), markdown definition lists (`m135`),
and the bullet-line / bullet-prefix block constructs
(`c047`/`m096`/`m097`/`m114`/`m115`/`m116`) — and the last entry, `b021`
(indented-numbered-list re-nesting), was resolved by implementing real list nesting
(rule below). There are no remaining intentional deviations.

## LaTeX entities + environments (Markdown AND Org; replicated from `entity.ml` / `latex_env.ml`)

- **Named entity** (`Inline::Entity`, projection key `entity`): at a `\` + ≥1 ASCII
  letters, the letters are looked up in the 339-entry mldoc table
  (`src/entities.rs`, `find()` over a `OnceLock<HashMap>`, **case-sensitive** —
  `Delta`/`delta`, `AA`/`aa` are distinct). A hit → an `Entity` carrying mldoc's full
  record `{name, latex, latex_mathp, html, ascii, unicode}`; a miss → the bare letters
  as plain (backslash dropped, the prior behavior). An optional `{}` immediately after
  the letters is consumed **either way** (`\Delta{}G`→Entity+"G", `\foo{}G`→"fooG").
  Inside `$…$`/`$$…$$`/`\(…\)`/`\[…\]` the backslash is part of a `Latex_Fragment`
  (the `$`/`\(` dispatch runs first), so the entity path is never reached there.
  Wired in `inline.rs backslash()` (md) and `org.rs backslash()` (org).
- **Environment block** (`Block::LatexEnv`, projection key `latex_env`,
  `["Latex_Environment", name, null, content]`): a line that, after optional leading
  spaces/tabs (`spaces *>` — text before `\begin` disqualifies it), starts with
  `\begin{NAME}`. After the `}` a `spaces_or_eols` run (spaces/tabs/newlines) is
  dropped; `content` is then everything up to a **case-insensitive** `\end{NAME}` (or
  EOF if absent — an unclosed `\begin` still becomes an env to EOF); the node `name` is
  lowercased. Shared helper `inline::parse_latex_env`, called from both block
  segmenters (between Table and the fenced/begin blocks, mirroring mldoc's parser
  order). The block consumes `[line.start, end-of-\end{NAME})`; the line loop resumes
  at the first line at/after that offset. (A `\begin…\end` that ends mid-line leaves a
  small span gap / drops a following-line leading `Break` vs mldoc — fuzz-only, not in
  any gate corpus; envs in real content occupy whole lines.)

## Markdown definition list (Markdown only; replicated from `markdown_definition.ml`)

- `term\n: definition` → a `List` whose single item carries the term as `name`
  (`ListItem.name: Vec<Inline>`, projection key `name`, `skip_serializing_if`-empty;
  `normalize.mjs`/`cleanBlock` likewise drop an empty `name`, so non-def items match).
  The item's `content` is one `Paragraph` per `:`-definition. A definition opens on a
  `(spaces) : (≥1 space) <content>` line and mldoc's `take_till1`-after-`satisfy`
  imposes a quirky **≥2-char** rule: the content's first char must be ∉ `{:`,`#`}` and
  there must be ≥1 more char (`: a` is NOT a def, `: ab` is). Continuation lines (next
  non-`:`/`#`-leading lines, same ≥2 rule) join into the same paragraph across a
  `Break`. Tried just above the paragraph fallback (mldoc's Lists fallback, after every
  other block construct), and it **pulls the term out of a running paragraph**
  (`intro\nterm\n: def` → `Paragraph[intro]` + def-list). Implemented in `parse.rs`
  (`is_def_opener`/`is_def_continuation`/`build_def_list`); Org `: def` stays an
  `Example` (untouched).

## Nested lists (Markdown AND Org; replicated from `lists.ml`)

- A `List` node's items are a **tree**, not a flat sequence: mldoc folds a
  deeper-indented item into the preceding item's `items` sub-array (`ListItem.items:
  Vec<ListItem>`, projection key `items`). This applies to the `List`-node path only
  — md `*`/`+`/`N.` and org `-`(col-0)/`+`/`N.`; md `-` dash bullets stay flat
  `Heading{unordered}` blocks with `level = 1 + indent` (unchanged).
- **Fold rule** (verified against mldoc over 40k random md+org inputs, `nest_items` in
  `projection.rs`): the block segmenter first collects the maximal run of consecutive
  list lines into a flat `(indent, item)` sequence (indentation differences do **not**
  break the group); `nest_items` then folds it. An item's **children are the maximal
  following run whose indent is ≥ the FIRST child's indent**; any strictly-greater
  indent nests (no fixed step — `* a\n * b` with one space nests). A shallower item
  **unwinds the stack fully**: it rejoins the nearest ancestor run it fits, else
  becomes a top-level sibling. The discriminating case is `* a\n    * deep\n  * mid`
  → `deep`(4) is a child of `a`(0), but `mid`(2) is a **top-level sibling of `a`**
  (not a child), because `mid`'s indent is below `deep`'s child-run floor (4) — a
  plain indent-stack would wrongly make `mid` a child of `a`. Equal indents under the
  same parent are siblings; mixed ordered/unordered types nest fine.
- Implemented **iteratively** (explicit frame stack, no recursion), single-pass O(n),
  so a pathological deeply-indented list can't overflow the stack. `normalize.mjs`
  (`normItem`/`cleanItem`), `compare.mjs` (`skelItem`) and `refs.rs`
  (`walk_list_item`, which also walks the def-list `name`) recurse the nested items
  as items — matching the oracle's generic deep AST walk.

## Block construct on a `-` bullet line (Markdown; replicated from `heading0.ml`)

- mldoc's bullet title is a lookahead (`title_aux_p`): if the text after the bullet
  prefix parses as a block construct, the bullet gets an **empty title** and the
  construct becomes the next block. lsdoc replicates the two openers that occur in real
  outlines, on `-` bullets only (`*`/`+` are Lists — their ``` is item content, NOT
  split): a **fenced code** opener (`` - ```lang `` → empty Bullet + `Src`; only when
  the fence actually closes — an unclosed `` - ``` `` stays a normal bullet titled
  ` ``` `; `Src` language is the first info-string token) and a **markdown blockquote**
  opener (`- > q` / indented `  - > l3` → empty Bullet + `Quote` with lazy
  continuation; a lone `- >` stays a normal bullet). Implemented in the `parse.rs`
  dash-bullet branch. (Other lookahead constructs — Hr/Table/Footnote/Latex_env/Drawer
  after a bullet prefix — are not split; none occur in the gate corpus, and a bullet
  carrying a task **marker** before the opener is left unsplit, both being adversarial
  forms Logseq does not produce.)

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
  `\<letter>+` (+ optional `{}`) → an `Entity` if the name is in the LaTeX table, else
  the bare letters (see the LaTeX section); a `\` before a non-escapable char is kept.
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
- **Committed gate** (`tests/perf.rs`): `perf_smoke` runs in the default `cargo test`
  (linear-budget + deep-nesting-on-a-1MiB-stack at moderate size). The full-scale
  versions (100k-char runs, 200k-deep nesting) are `#[ignore]`d — run with
  `cargo test --release -- --ignored`. Measured: 9× 100k-char pathological inputs in
  ~0.2 s release; deep nesting to depth 200k completes on a 1 MiB stack (parser is
  bounded-depth, not O(depth) recursive).

## Differential fuzzer (M5)

`harness/fuzz.mjs` generates biased-random markdown (adversarial token alphabet),
runs both mldoc and lsdoc, and diffs the projection. **No panics/hangs over 60k+
inputs across seeds** (byte-safety holds on café/中文/😀/zero-width) — this is the
robustness guarantee.

`harness/fuzz-triage.mjs` buckets the mismatches by structural signature
(oracle-block-kinds → lsdoc-block-kinds). The triage drove three **block-level
over-detections** that could plausibly trip on semi-realistic content, now FIXED
(probed against mldoc, unit-tested in `parse.rs::fuzz_surfaced_block_edges`):
- **quote**: opens only with non-whitespace after `>` (`>` / `> ` are paragraphs).
- **property**: key must contain no `:` — so `http://x.com:: y` is prose, not a
  property (the `http:` colon disqualifies the key).
- **raw HTML**: needs a closing `</…>` on the line — a bare `<div>` / `<note this>`
  is a paragraph (mldoc only emits Raw_Html for a complete element).

Remaining fuzz-only mismatches (after the fixes) are all **same-block-kind**: inline
tokenization differences on pure mixed-delimiter token-soup (e.g.
`#[[$}_](url)tagword`), plus mldoc's odd `$$x$$<trailing>` mid-line split (a displayed
-math block followed by junk on the same line — lsdoc keeps the cleaner whole-line
behavior). None occur in real content: the realism corpus (`~/research/tine-test` +
kitchen-sink) AND the 202 mined upstream-test inputs are all in the gate at 0-diff.
These are not gate inputs and are not allowlisted — exact bug-for-bug parity on random
garbage would mean binding to mldoc's combinator internals, which SPEC §5 forbids.

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

  The LaTeX entity/environment, markdown definition-list, bullet-line block
  constructs, and indented-numbered-list re-nesting (`b021`) that were once
  allowlisted here are now matched (see the dedicated rule sections above). The
  **allowlist is empty** — no markdown deviations remain. No `refs` or `block-struct`
  diffs remain.

## M6 Org-mode (replicated from the oracle + mldoc 1.5.7 source)

The Org parser (`src/org.rs`) is a second line-based block segmenter + single-pass
inline scanner, behavior-equivalent to mldoc's `format:"Org"`. Format-agnostic
helpers (timestamps, autolink/email/html, nested links, macros, bare urls, page-ref
& tag scanning, `char_len`/`find_sub`/`unescape`) are reused from `src/inline.rs`
(made `pub(crate)`); Org-specific grammar lives in `org.rs`. The Markdown parser is
untouched (md gate stays 0-diff). Two inline nodes were added in lockstep to
`projection.rs` + `harness/lib/normalize.mjs`: `Subscript`, `Superscript`.

**Doc-level block order** (mldoc `mldoc_parser.ml`, `Org` config): directive →
drawer → headline → table → latex-env → fenced/`#+BEGIN`/verbatim/quote/`$$`/raw-html
block → footnote → list → hr → paragraph. Org `~/research/org-graph` (16 real `.org`) +
53 hand-written + 25 mined `test_org.ml` inputs all reach **0 diffs** (refs +
block-struct + blocks-full); **the allowlist is empty** (`b021` was resolved by the
nested-list rule). Org `+`/`N.` lists nest via indentation like Markdown; org `-`
nests only as a column-0 sibling/parent (an indented `  - x` is not a list line).

### Block rules
- **Headline** `*{n}` at column 0 + space/EOL → `Bullet{level:n}` (mldoc
  `Heading{unordered:true, level:n}`). `*nospace` is a paragraph; an indented `  * x`
  is a *list* item, not a headline. Title text = after stars, then a leading task
  **marker** (`TODO`/`DOING`/`WAITING`/`WAIT`/`DONE`/`CANCELED`/`CANCELLED`/`STARTED`/
  `IN-PROGRESS`/`NOW`/`LATER`, followed by a space OR end-of-line) and **priority**
  `[#X]`. **`:tag1:tag2:` extraction** (`heading0.ml`): if the last title inline is a
  `Plain` whose trimmed text ends with `:` (len > 1), `splitr` at the last space; the
  suffix is parsed as `:`-wrapped tags (empty tokens dropped, a space invalidates),
  and the title's last Plain is rebuilt as `rtrim(prefix) + " "` (or dropped if the
  whole plain was tags). A `*` line **inside `#+BEGIN_SRC` is code**, not a headline.
- **Directive** `#+KEY: value` (KEY has no `:`, not `BEGIN_…`) → `Directive`.
- **Drawer** `:PROPERTIES: … :END:` → `Property_Drawer`; any other `:NAME: … :END:`
  → `Drawer` (name lowercased, content opaque). A run of `#+NAME: value` lines
  **immediately following a `:PROPERTIES:` drawer folds into it** (mldoc `Drawer.parse`
  `many1 (parse1 <|> parse2)`), e.g. `:PROPERTIES:…:END:\n#+ZZZ: 3` → props incl ZZZ.
- **Blocks** `#+BEGIN_X … #+END_X`: `SRC`→`Src` (first token after `_SRC` is the
  language), `EXAMPLE`→`Example`, `QUOTE`→`Quote` (content re-parsed as blocks), else
  →`Custom` (name lowercased). Content gets **indent-cleared** by the first line's
  leading whitespace (`block0.ml`), so `#+BEGIN_QUOTE\n aaa\nbbb` → `aaa\nbbb`.
  Markdown `` ``` ``/`~~~` fences and `$$…$$` and `<html>…</html>` and `>`-blockquotes
  also work in Org. A run of `:`-prefixed lines (`: foo`) is an Org **verbatim block**
  → `Example` (drawer `:NAME:` is tried first).
- **List** at indent 0: `- `/`+ ` (unordered) / `N. ` (ordered) → `List` (mldoc
  `List`, NOT a bullet — only `*` at col 0 is a headline). Leading `[ ]`/`[x]`
  checkbox stripped from item content.
- **HR** = exactly 5 dashes `-----` (`count 5 (char '-')`); `----`/`------` are prose.
- **Footnote def** `[fn:name] text` → `Footnote_Definition`.
- **Paragraph** accumulation matches the Markdown segmenter (span incl. trailing
  newlines → `Break_Line` per `\n`). **Blank-line absorption differs by predecessor**:
  Directive/Comment/`#+BEGIN`-block/verbatim/List/Footnote (mldoc `<* optional eols`)
  swallow following blank lines; Heading/Table/Drawer/Property-drawer/HR do not, so a
  blank there becomes a `Paragraph[Break_Line]` (e.g. `* A\n\n* B`).

### Inline rules (`OrgScanner`, mldoc `inline.ml` Org branch)
- **Plain-run delimiters** = `\ _ ^ [ * / + $ #` + whitespace (`org_plain_delims`).
  Notably NOT `~ = ( < { ! @ ] )` — so `~code~`/`=verb=`/`((ref))`/`<url>`/`{{macro}}`
  fire **only at a run boundary**: `text ~code~`→Code but `a~code~`→literal, `x ((u))`→
  block-ref but `a((u))`→literal. (Same dual `plain_one`/`plain_run` model as Markdown,
  different delimiter set.)
- **Emphasis**: `*`→Bold, `/`→Italic, `_`→Underline, `+`→Strike_through, `^^`→Highlight
  (single char except `^^`); `~`→Code, `=`→Verbatim (literal, non-empty, no marker/eol
  inside). Gates (mldoc `org_emphasis` + `md_em_parser`):
  - `*` and `^^`: **no** boundary gate. ⇒ `2*3*4`→Bold[3], `a*b*c`→Bold[b].
  - `/`, `+`, `_`: **backward** gate (char before opener ∈ ASCII-punct/whitespace, via
    mldoc `state.last_plain_char`, default true) AND **forward** gate (char after the
    closer ∈ punct/whitespace/eoi). ⇒ `a/b/c`/`snake_case_var`/`word+x` stay literal.
  - The forward gate differs for `_` vs `/`,`+`: `_` **continues** to the next
    candidate closer if the forward char fails (`_a_b_`→Underline[`a_b`]), whereas
    `/`,`+` **fail outright** (`/a/b/`→literal). `*`/`^^` close at the first
    right-flanking run.
  - The **backward gate is active only at top level** (state); inside an emphasis
    re-parse mldoc calls `emphasis` without state, so only the forward gate applies.
    `last_plain_char` is tracked precisely (updated only on plain emission), so
    `word[[x]]_y_` → Subscript (the `d` before `_` kills Underline), not Underline.
  - Emphasis content is re-parsed with **emphasis/sub-superscript/links/plain**
    (`nested_emphasis`); `*a/b/c*`→Bold[`a/b/c`] (the `/` italic fails its forward gate).
- **Subscript/Superscript**: `_x`/`^x` (a `non_space` run) or `_{x}`/`^{x}`. Content is
  re-parsed with **emphasis/plain/entity only — NO nested sub/sup, NO links**
  (`gen_script`): `snake_case_var`→`snake` + Subscript[`case_var`] (not nested).
- **Links** (`org_link`): `[[url][label]]` (`org_link_1`), nested `[[…[[…]]…]]`, then
  `[[url]]` (`org_link_2`). Classification: `file:…`→File; `org_link_2` `proto://link`
  →Complex else Page_ref (`[[id:uuid]]`→Page_ref `id:uuid`, no `://`); `org_link_1`
  empty-label→Search, `proto:link` (single colon, strip leading `//`)→Complex else
  Search. Label re-parse uses **emphasis/latex/code/sub-sup/plain — NO links** (so
  `[[…][[[x]] …]]` keeps `[[x]]` literal, no spurious page ref). `full_text` for
  `org_link_1` uses only the first label inline's plain text (mldoc quirk).
- **Tags/macros/block-refs/bare-urls/timestamps/latex/autolink/email/html** reuse the
  shared `inline.rs` parsers. `<…>` tries autolink → `<date>` timestamp → html → email;
  `[…]` tries org-link → inactive `[date]` timestamp → `[fn:…]` footnote ref.
- **Escapes**: Org does **NOT** unescape (`md_unescaped` is Markdown-only), so `a\*b`
  → Plain `a\*b` (backslash kept), `\\`→`\\`. `\`+eol → `Hard_Break_Line`
  (`org_hard_breakline`); `\(…\)`/`\[…\]` → latex; `\letters` (+ optional `{}`) → an
  `Entity` if the letters are in the 339-entry table (`entities.rs`, same path as
  Markdown), else the bare letters. Block-level `\begin{X}…\end{X}` is a
  `Latex_Environment` in Org too (see the dedicated LaTeX section). NOTE: the reused
  page-ref/tag/bare-url value
  scanners *do* call `unescape`, which is a no-op on real Org content (no backslashes
  in those positions across the whole corpus); a synthetic `[[a\]b]]` would
  technically under-keep the backslash in the extracted *value* only.

### Mined Org corpus (M6)
`harness/corpus.org.mined.gen.mjs` → `harness/corpus.org.mined.json` (committed,
self-contained, `om###` ids). The INPUT (first OCaml string literal after each
`check_aux`/`check_aux2`) of every `test/test_org.ml` test (logseq/mldoc HEAD
`bedae99`), full OCaml escape decoding — **25** inputs. `test_org.ml` uses
`keep_line_break:false`; we reuse only the input strings (re-run through our
`keep_line_break:true` oracle). Surfaced + replicated: org-link-1 label without links,
`#+BEGIN` indent-clearing, `:PROPERTIES:`+`#+NAME` folding (the three fixes above).

### Complexity
Block segmentation O(n) (one line scan; fences pre-paired). Inline O(n) amortised: the
emphasis no-closer cache and the 2-byte `seq_present`/single-`]` `has_rbracket` absent
caches keep `*`×n / `[[`×n / `((`×n / `_`×n runs linear (unit-tested,
`org::tests::adversarial_runs_terminate`, <0.3 s).

### M6 Org fuzz-hardening
Differential fuzzing against mldoc-Org (`node fuzz.mjs N seed org`, biased token-soup)
surfaced a **~21.6% block-mismatch** rate (vs the ~1.4% Markdown floor) — real
edge-case gaps the curated corpus missed. `node fuzz-split.mjs` separates **structural**
(different block-kind sequence — genuine over/under-detection) from **inline-soup**
(same block-kinds, inline tokenization noise on garbage). Six block-rule fixes (all
probed against mldoc, unit-tested in `org::tests`, added as `o###` regressions in
`corpus.org.gen.mjs`) drove block-mismatch **21.6% → ~7.3%** (structural 752 → 323 per
20k; refs 6% → 2.1%); the md fuzz floor is unchanged (only `org.rs` changed):

- **Fixed-width `:` block.** ANY line that (after optional ws) starts with `:` and is
  NOT part of a recognized `:NAME: … :END:` drawer (tried first) → a verbatim
  `Example` (mldoc maps `: text`/`:text`/`:key: value`/`:tag1:tag2:`/bare `:END:`/
  `:PROPERTIES:` all to `Example`). Consecutive `:`-lines coalesce into one `Example`;
  content = after the `:`, leading ws stripped, trailing/internal kept (`:  x` → `x`,
  `: a b  ` → `a b  `). A valid `:PROPERTIES:…:END:`/`:LOGBOOK:…` stays Property_Drawer/
  Drawer; once a fixed-width run starts, an embedded `:NAME:` is swallowed as text (mldoc
  does not re-try the drawer mid-run). (Was: lsdoc only matched `: `/`:`-then-space → the
  biggest bucket, ~1200/20k.)
- **Footnote definition needs a body.** `[fn:1]` (or `[fn:1]   `) is an inline footnote
  ref in a Paragraph; only `[fn:1] body` is a `Footnote_Definition`. mldoc additionally
  rejects a body whose first non-ws char **begins a block construct** (`* # [ -`):
  `[fn:1]:x`/`[fn:1]/x` → def, `[fn:1]*x`/`[fn:1]-x`/`[fn:1]#x`/`[fn:1][x` → Paragraph.
  Leading ws before `[fn:` is allowed.
- **Empty list marker → Paragraph.** `- `/`+ `/`1. ` (and `- [ ]` with a checkbox but no
  content) → Paragraph; only a non-empty item (`- x`, `- [ ] x`) is a `List` (mirrors the
  md "quote needs content" rule). Also: **`-` is a bullet only at column 0** — an indented
  `  - x` is a Paragraph, while indented `  + x`/`  1. x` stay Lists (mldoc quirk).
- **Malformed table → Paragraph.** An Org table row's trimmed line must start AND end
  with `|` (≥ 2 bytes): `| a |`/`||`/`|---+---|` are rows, `| a | b`/`|a`/`|` are not;
  a non-row line breaks the table group. (Was: lsdoc accepted any `|`-prefixed line.)
- **Directive.** Leading whitespace is allowed (`  #+K: v`); the value is **left-trimmed
  only** — mldoc keeps trailing whitespace (`#+TITLE: x  ` → `x  `). (Was: `.trim()` ⇒
  the largest same-kind bucket, 302/20k.)
- **Empty-title headline with trailing whitespace.** `*** `/`* TODO ` emit the empty
  bullet, then the leftover whitespace begins a fresh paragraph that absorbs following
  lines (`* \nx` → Bullet + Paragraph[" ", Break, "x"]). A *block-construct* remainder
  (`* :x` → Example, `* #+K: v` → Directive, `* | a |` → Table) is left as adversarial
  noise (see below).

Also extended `normalize.mjs` `cleanBlock` to strip the cosmetic empty-`Plain ""` from
**table cells** (mldoc emits `[Plain ""]` for an empty cell `||`; lsdoc emits `[]`) — the
same cleaning already applied to inline arrays, now applied to both sides' cells.

**Residual fuzz situation (analogous to the md fuzzer note).** After the fixes the
remaining ~7.3% block-mismatch is dominated by **same-block-kind inline-soup** (~5.6%):
inline tokenization differences on pure mixed-delimiter garbage (denser than md because
Org has more single-char delimiters `* / _ + ~ = ^`). The residual **structural** chunk
(~1.6%) is all mldoc combinator quirks on adversarial input, NOT realistic content, so —
per SPEC §5 — it is documented, not chased and not allowlisted:
- **block construct glued onto an empty headline** (`* :PROPERTIES:`/`* #+K: v`/
  `* | a |` on ONE line): mldoc emits the empty bullet + the re-parsed block; a real
  headline always has a title, and a real heading+drawer is on separate lines (already
  matched). lsdoc keeps the remainder as the headline title.
- **`#+BEGIN_X: v` with a colon and no matching `#+END_X`** → mldoc `Property_Drawer`
  (a `#+key:value` Drawer.parse2 fallback). Real `#+BEGIN_…` blocks have no colon.
- **`:END:word` (drawer end with trailing junk)** → mldoc closes the drawer and re-parses
  `word`; a real `:END:` is alone on its line.
- **multi-line footnote-definition continuation**: mldoc's footnote body greedily absorbs
  following lines as plain text — but with footnote-specific terminators (stops at
  headline/list/directive/footnote/blank, yet absorbs a table/quote/`:`-line *as text*,
  unlike a Paragraph). Matching that exact predicate is binding to mldoc internals; lsdoc
  keeps the single-line footnote def. (~15/20k.)

No allowlist entries (the allowlist is empty); the real org graph + hand-written +
mined corpora stay at 0-diff with 49 new `o###` fuzz regressions added.
`node fuzz-split.mjs N seed org` is the kept diagnostic (structural vs inline-soup).

## Render-level parity (RENDER-PARITY-AND-INTEGRATION.md §1; Martin, 2026-06-28)

The original 583/583 parity covered **indexing**: refs + block structure + inline
kind/payload/order/nesting. It did NOT cover several **render-only** fields mldoc
carries that the projection silently dropped. Method (per the spec): `harness/delta.mjs`
walks raw mldoc ASTs over the full corpus + a render-focused extra set and auto-diffs
*every* payload key mldoc emits against what `normalize.mjs` keeps; `harness/probe-render.mjs`
nails down the uncertain mechanics. Each render-relevant field was added to BOTH the AST
(`projection.rs`) and `normalize.mjs`, then gated to 0-diff with new corpus cases. The
gate is now **render-level**: 621 inputs, refs + block-struct + blocks-full all 0-diff.

**Carried + gated (the render fields):**
- **Image-ness** — `Inline::Link.image: bool`. mldoc carries **no native image flag**;
  the *only* difference between `![a](x)` and `[a](x)` is `full`'s leading `!`. Both sides
  derive `image` from that (`normalize.mjs`: `full_text.startsWith("!")`; lsdoc: the `!`
  image path). Omitted when false. (md only — org `[[…]]` never starts with `!`.)
- **Link `metadata`** — `Inline::Link.metadata: String`, the raw Logseq media dims
  `{:width … :height …}` (braces included), mldoc's `metadata`. md links (after `)`) and
  org_link_1 `[[u][l]]{…}` carry it; org_link_2 `[[u]]{…}` does NOT (mldoc leaves the `{…}`
  as plain text — matched). Omitted when empty. lsdoc already computed it (folded into
  `full`); now also exposed.
- **Link `title`** — `Inline::Link.title: Option<String>`, the raw inner of a trailing
  `"…"` (no quotes, **not** unescaped — mldoc keeps `a \"b\" c` verbatim). Empty `""` is
  not a title (the whole between-parens becomes the URL). md only.
- **List `checkbox`** — `ListItem.checkbox: Option<bool>`: `[ ]`→`Some(false)`,
  `[x]`/`[X]`→`Some(true)`, none→`None`. mldoc records it on `*`/`+`/`N.` (md) and
  `-`/`+`/`N.` (org) list items. md `-` bullets are `Heading{unordered}` (a `Bullet`), so
  `- [ ] x` is literal title text `[ ] x` with NO checkbox (matched). `[-]` is literal, not
  a checkbox. lsdoc already stripped the checkbox from content; now also records the state.
- **Org `Target`** — `Inline::Target { text }` for `<<name>>` (mldoc `Target`). Inner taken
  raw; `<<>>` (empty) and unterminated `<<x` stay plain. (`normalize.mjs` already emitted
  `{k:"target"}`; lsdoc previously produced soup — fixed.)

**Justified non-carries (render-relevant but deliberately NOT added):**
- **Table column alignment** — mldoc **1.5.7 discards it**: `col_groups` is just
  `[column_count]` (`[3]` for both `|:--|:-:|--:|` and `|---|---|`), there is no
  per-column align anywhere. Logseq (via mldoc) does not render aligned tables, so
  matching it means dropping alignment too. (The spec's audit assumed mldoc kept it in
  `col_groups`; it does not.) Nothing to gate against. **If Martin wants aligned tables as
  a beyond-OG feature, lsdoc would have to parse the separator row itself and carry an
  un-gateable `align` field — flagged for his call; not done, to preserve zero-allowlist.**
- **`Inline_Hiccup`** (`@@hiccup:…@@`) — a Logseq-internal HTML-export construct, never
  user-authored; absent from all real graphs (the gate would already fail if any gated
  input produced it). mldoc's own handling is a degenerate split (`@@hiccup:` + inner +
  `@@` as three nodes). lsdoc emits plain text for `@@…@@`. `normalize.mjs` still has a
  `hiccup` case (harmless; never fires on gated inputs) — a future corpus case would force
  the decision. Additive if ever needed.
- **`Heading.meta`** (`{timestamps, properties}`) — always `{timestamps:[], properties:[]}`
  under OG's config; SCHEDULED/DEADLINE/ranges flow through the **`Timestamp` inline**
  instead (already kept, with `ts` + the full opaque `date` object — render-complete).
- **`Heading.anchor`** — TOC slug, not visually rendered by Tine.
- **`Footnote_Reference.id`** — mldoc's auto-increment int; `name` suffices to link ref↔def.
- **`Nested_link.children`** — the parsed decomposition of `[[a [[b]] c]]`; `content` (the
  raw inner string) is kept and suffices to render a nested page-ref. (`children` would also
  drag in a `Label` inline tag the enum lacks.) Additive if fidelity ever needs it.
- **`Src.options`** (org header-args `:results …`) — affects babel execution, not display;
  Tine renders read-only code by language. **`Src.pos_meta`** is a span (spans out of scope).

## Fuzz-reachability audit + the org bugs it found (2026-06-28)

After render parity, a differential **fuzz-reachability** analysis (100k inputs, 50k md +
50k org) classified the residual structural-mismatch "floor": **20/22 buckets (840/885) are
unreachable** from realistic input (each provably needs an adversarial feature — two
block-openers glued with no newline, a stray/dirty `:END:`/`#+END_*`, a construct glued onto
a headline, md `$$` glued to text; all match once segmented one-block-per-line), and the real
graphs are structurally **0-diff**. But **2 buckets were genuine parity bugs on valid Org**
(a block body is arbitrary multi-line content, so this matters regardless of how Logseq stores
blocks). Both fixed:

1. **Indented `*` list item** (`  * x` → `List`, not `Paragraph`) — contradicted the spec
   above (`### Block rules`). `*` is now an unordered marker when indented (col-0 `*` is a
   headline). Only `*` was broken; `-`/`+`/`N.` were already correct.

## Org multi-line list continuation + indented-`-` collapse (FIXED — port of `lists0.ml`)

lsdoc's org list segmenter previously treated each line independently. It now ports mldoc's
recursive list parser (`collect_list` in `org.rs`):
- **Continuation fold.** An indented (≥1 space) non-marker line folds into the current item's
  content (de-indented via `String.trim`, joined with `\n`, re-parsed). `- a\n  more` →
  `List` item content `a⏎more`; `- a\nmore` (no indent) → `List` + `Paragraph` (not folded);
  a blank line ends the item (mldoc `two_eols`).
- **Restricted item content.** Item content uses mldoc's `list_content_parsers` set: NO
  Directive/Drawer/Heading/Footnote/List inside an item (`#+K: v` stays a paragraph,
  `:PROPERTIES:`→Example, `[fn:1] x`→inline ref), but `> q`/`: ex`/`| t |`/`-----`/`$$`/
  `#+BEGIN_…`/`<html>` are real blocks.
- **Indented-`-` collapse (PARTIAL).** An indented `-` line (not a valid marker) makes mldoc
  fail the list; the failure **bubbles up only through first-at-level items**, so a surviving
  prefix stays a `List` and only the failing item onward becomes a `Paragraph`:
  `- a\n  - z` → `Paragraph`; `- a\n- b\n  - z` → `List(a,b)` + `Paragraph`;
  `- a\n  - z\n- b` → `Paragraph` + `List(b)`. A `collapse_floor` memo keeps repeated
  collapses linear (no O(n²)); verified by a 40k-line perf case.

Verified: gate 0-diff (56 new `o###` cases), 28 fresh hand-probed cases all match mldoc,
org fuzz block-mismatch **7.29% → 4.94%** (panic-free), md unchanged, perf/stack pass.

## Org footnote-definition predicate + multi-line body continuation (both FIXED)

**Def-vs-paragraph predicate.** `[fn:LABEL] body` is a `Footnote_Definition` only if the body
(after leading spaces) is **≥ 2 bytes** AND its first char doesn't begin a block construct
(`* # [ -`); else it is an inline footnote *ref* in a `Paragraph` (mldoc `satisfy non_eol` +
`take_till1`). So `[fn:1] a`/`[fn:1]  a` → `Paragraph`, `[fn:1] ab`/`[fn:1]:x`/`[fn:1] é`
(2 bytes) → def. (6B initially mischaracterized this as "continuation folding" — corrected
after direct probing; verify-the-prover.)

**Body continuation (FIXED — port of `footnote.ml`).** A footnote def absorbs following
continuation lines into its inline body (joined with `Break_Line`, de-indented). mldoc's body
is `many1 l`, `l = spaces *> satisfy non_eol >>= line` with a footnote-specific `non_eol`
(excludes `\r \n - * # [`). A continuation line is **absorbed** iff (after stripping leading
spaces) it is non-empty, its first byte ∉ `{- * # [}`, it has ≥2 bytes before any eol, and any
interior `\r` is a real `\r\n`; else it **terminates** (blank/ws-only line, col-0 `* - # [`,
directive, `#+BEGIN_X`, hr, another `[fn:N]`, a 1-byte line). Notably `+`/`N.`/tables/quotes/
`:`-lines/`<<target>>` **fold as text**; indented `+` is de-indented while indented `* - #`
terminate — all probe-confirmed. Linear in body length (100k-line perf case). 713/713 gate.

**Two pre-existing gaps this work EXPOSED (footnote-unrelated, newly tracked):**
- **Org `# comment` blocks.** `# c` / `  # indented` → mldoc `Comment` block; lsdoc emits a
  `Paragraph` and `normalize.mjs` has no `Comment` case (→ `block:Comment` fallback). Standard
  org syntax, so reachable — but fixing it ADDS a `Block::Comment` AST variant, the one
  change-type that needs Tine-contract coordination (v0.1.0 is handed off). Pending a
  fix-now-vs-defer decision; not yet in the gated corpus.
- **Whitespace-only continuation line** (`[fn:1] body\n   \ncont`): the *downstream* paragraph
  keeps `"   ",Break` in mldoc; lsdoc drops the ws-only line. Footnote body itself is correct;
  general `absorb`/ws-line issue, value-only.

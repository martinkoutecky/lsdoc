# v2 systematic parity audit

## Objective

Restore the meaning of "mldoc-equivalent by construction" for `lsdoc` v2.
The current v2 implementation is close, but the real-graph divergences in
`../V2-REAL-GRAPH-DIVERGENCES.md` show that the construction proof is incomplete:
some optimized or derived paths bypass source-transcribed behavior.

The target state is:

- public `parse`, `parse_format`, and Tine's WASM path produce the same observable
  projection as the latest published `mldoc` oracle, currently `mldoc@1.5.9`;
- intentional divergences from `mldoc` bugs are isolated by dedicated probes and
  documented in `V2-TRANSCRIPTION.md`;
- parser time is linear by construction, with documented exceptions only;
- representative throughput is at most 50% slower than the relevant state-of-the-art
  parser family on the existing benchmark suite.

## Closure status, 2026-07-09

The first failure was not that v2 lacked tests. It was that several optimized paths
were accepted as "obviously equivalent" without a local proof against the exact
mldoc source context. Those paths were by-construction only for a wider or different
grammar than the one mldoc actually runs.

The closure pass therefore changed the rule from "match the oracle on sampled output"
to "name the mldoc parser, parser group, and fallback rule for every shortcut." The
known GH/reported and real-graph divergences are now covered by permanent differential
gates:

- `harness/reported-divergences.mjs`: 147 minimized/reported cases, including the four
  v1-vs-mldoc GH families, now match with `LSDOC_ENGINE=v2`;
- `harness/audit-v2-shortcuts.mjs`: 7158 source-context boundary cases over inline,
  block, transformed-body, suffix, and ref/properties shortcuts now match;
- `harness/realmut.mjs`: 39040 mutated real-graph inputs now have zero reference or
  block projection mismatches;
- `harness/run.mjs`: the ordinary corpus, blockgate, inlinegate, spans gate, and shortcut
  audit all pass under `LSDOC_ENGINE=v2`;
- release ignored perf/complexity gates pass, and current representative benchmarks
  remain within the at-most-50%-slower gate against comrak/orgize.

## Initial triggering failures

The real graph scan found two non-version-skew parity holes:

1. Markdown bold fast path reparses `$...$` inside `**...**` as LaTeX, while mldoc
   keeps it as plain text inside nested emphasis repair.

   Minimal reproducer:

   ```md
   **x $a$**
   ```

2. Markdown blockquote materialization keeps quoted blank separators before a quoted
   horizontal rule as trailing paragraph breaks, while mldoc consumes those separators
   as block separation.

   Minimal reproducer:

   ```md
   > a
   >
   > ---
   ```

These are not oracle-version skew: both reproduce against the local pinned
`mldoc@1.5.9` harness.

The later reported-divergence and shortcut-audit pass found the same pattern in
additional contexts: Org link labels used the wrong inline config, top-level paragraph
separator behavior leaked into block/list-content contexts, malformed timestamp and
Markdown-link-label candidates committed unsafe scan state, and empty heading/list marker
tails treated unclosed fences as if `Fenced_code_block.parse` had accepted them. Those
were all fixed by source rules, not by adding one-off output rewrites.

## Principle

Every non-literal v2 path must be either:

- a source transcription of the corresponding mldoc parser state machine; or
- a conservative refinement that accepts only a proven subset of that source
  behavior and otherwise declines without consuming input.

If a proof depends on "mldoc probably reparses this the same way", the path is not
acceptable. It must decline to the source-transcribed path or be rewritten so the
equivalence is local and checkable.

## Scope

Audit every path that can emit AST without directly following the source ledger:

- inline fast paths and fused direct builders;
- simple emphasis, code, link, image, entity, timestamp, URL, macro, block-ref, page-ref,
  script, LaTeX, and hiccup shortcuts;
- direct paragraph/plain builders;
- quote, callout, drawer, list, table, definition-list, and source/body frame parsers;
- split-title suffix parsing after headings, bullets, hiccups, displayed math, raw HTML,
  and properties/drawers;
- body materialization and origin-map remapping;
- merge/trim/rewrite post-processors;
- ref extraction shortcuts and property-value reparsing;
- any benchmark-motivated shortcut added during the performance pass.

## Proof obligation template

For each audited path, record or encode the following:

- **mldoc source**: exact source function(s), parser order, and config context.
- **entry context**: where the path is allowed to run and which earlier alternatives
  have already failed.
- **accepted language**: the byte-level predicate for inputs the shortcut may accept.
- **subset proof**: why every accepted input is also accepted by mldoc's corresponding
  parser in that context.
- **output proof**: why emitted `Block`/`Inline`/refs equal the normalized oracle output.
- **failure proof**: why rejected inputs consume nothing and fall through to the same
  next parser mldoc would try.
- **state proof**: freshness, delimiter state, paragraph separator state, quote/callout
  body state, origin mapping, and post-processing effects.
- **complexity proof**: scan owner, monotonicity/cache argument, and any exception.
- **tests**: minimal positive cases, minimal negative cases, and an enumerator/fuzzer
  covering the construct's boundary alphabet.

## Work plan

1. Add the real-graph minimal reproducers to the harness before changing code.

2. Fix the known parity holes:

   - make `markdown_fast_simple_bold` accept only the mldoc-safe subset and decline to
     the source-transcribed nested-emphasis child grammar for context-sensitive body
     constructs;
   - make blockquote/block-content separator trimming match mldoc for blank runs
     before non-paragraph child blocks such as `Hr`.

3. Build a fast-path inventory.

   Search for direct builders, shortcut names, post-processors, and comments such as
   `fast`, `direct`, `simple`, `shortcut`, `fused`, `rewrite`, `trim`, `merge`,
   `remap`, `safe`, and `bounded`. Convert the result into an audit checklist in this
   document or a generated appendix.

4. Audit inline paths first.

   Inline shortcuts are high risk because mldoc's parser groups differ by context:
   top-level inline, Markdown link labels, nested emphasis repair, scripts, property
   values, and Org inline each have different allowed alternatives. Any shortcut that
   reparses a child buffer must use the exact context grammar or decline.

5. Audit transformed-body paths next.

   Quote, callout, list, drawer/property, fixed-width, and special body parsing all
   materialize views of source text. Verify separator consumption, parser suppression,
   lazy continuation, origin mapping, span clearing, and child post-processing against
   the relevant mldoc source path.

6. Audit split suffixes and post-processors.

   Heading/bullet/hiccup/property/math/raw-HTML suffix parsing is vulnerable because it
   re-enters the block parser in unusual contexts. Merge/trim/rewrite functions must
   have their own proof obligations, not be treated as harmless normalization.

7. Add construct-specific differential enumerators.

   Required enumerators:

   - Markdown emphasis bodies over `$`, backticks, escapes, `_`, `^`, `[`, `]`,
     whitespace, and nested markers;
   - blockquote bodies over blank runs before HR, comment, list, heading/bullet-looking
     paragraph suppression, raw HTML, fences, displayed math, and ordinary paragraphs;
   - callout/list block-content separators and suppressed parser families;
   - heading/bullet split suffix candidates;
   - invalid candidate fallthrough cases for every shortcut.

8. Re-run the full parity suite.

   Required gates:

   ```bash
   rtk cargo fmt -- --check
   rtk cargo check
   rtk cargo test
   rtk cargo test --test complexity
   rtk cargo test --release -- --ignored
   rtk env LSDOC_ENGINE=v2 node harness/run.mjs
   rtk env LSDOC_ENGINE=v2 node harness/realmut.mjs
   rtk env LSDOC_ENGINE=v2 node harness/fuzz.mjs 40000 4242
   rtk git diff --check
   ```

   If real Tine block exports are available, `harness/run.mjs` must also exercise
   `blockgate` instead of skipping it.

9. Re-clear the performance gate.

   Run the benchmark suite in release mode on the same corpora used for the existing
   `bench/README.md` numbers. The accepted state is:

   - `lsdoc` is at most 1.5x slower than `comrak` on representative Markdown graph/doc
     parsing;
   - `lsdoc` is at most 1.5x slower than `orgize` on representative Org graph/doc
     parsing;
   - any remaining larger gap against event-stream parsers such as `pulldown-cmark`
     is documented separately and not treated as the primary gate.

   If a correctness fix regresses throughput beyond the gate, profile and either
   recover the performance with a proven-equivalent shortcut or keep the source path
   and document why the gate cannot be met. Do not add unproven shortcuts to recover
   speed.

## Acceptance criteria

- The two real-graph minimal reproducers are committed as regression tests.
- Every shortcut/post-processor has either a written proof obligation or has been
  removed in favor of the source-transcribed path.
- No known `V2-REAL-GRAPH-DIVERGENCES.md` case diverges from `mldoc@1.5.9`.
- Full harness, fuzz, realmut, complexity, ignored release, and formatting gates pass.
- Real blockgate passes when Tine block exports are present.
- `docs/V2-TRANSCRIPTION.md` and `docs/LINEARITY.md` are updated for any changed
  parser ownership or complexity exception.
- Benchmarks satisfy the at-most-50%-slower gate against `comrak`/`orgize`, with
  updated numbers in `bench/README.md`.

## Audit Inventory

Status legend:

- **Audited**: a local proof obligation is documented here and backed by tests or a
  deterministic differential enumerator.
- **Needs audit**: still requires a proof obligation or removal/decline rewrite before
  this plan is complete.

| Area | Status | Proof / action |
|---|---:|---|
| Markdown `markdown_fast_simple_bold` | Audited | The shortcut now accepts only the mldoc-safe subset: the body must start with non-mldoc-whitespace, may keep common same-output bodies such as plain text, `[[page]]`/nested links, code spans, `#` text, ordinary punctuation, and safe nested emphasis/script cases on the fast path, and must decline for top-level-only or state-sensitive body constructs whose fast output would differ from mldoc nested-emphasis repair (`$...$`, `![...]`, single-bracket fnref/cookie/timestamp/hiccup/link families, raw angle/autolink/html, bare `://` URLs, alphabetic entities, and `_` delimiter-state cases). Otherwise it declines to `resolver::try_markdown_nested_emphasis_at_cached`, the source-transcribed path. Regressions cover `**x $a$**`, `** x**`, `**[[Page]]**`, and `**x_u_**`; `harness/audit-v2-shortcuts.mjs` enumerates Markdown bold bodies over LaTeX, links, cookies, timestamps, hiccup, tags, macros, block refs, HTML/autolinks, URLs, scripts, entities, and whitespace. |
| Markdown top-level inline fast path | Audited | `plain_fast_path_markdown` is allowed only in top-level inline context before `resolver::parse_ctx`. `harness/audit-v2-shortcuts.mjs` enumerates top-level Markdown inline pairs/triples across the construct-start alphabet and compares against `mldoc@1.5.9`. Any declined case falls back to the resolver. |
| Org top-level inline fast path | Audited | `plain_fast_path_org` is allowed only in top-level Org inline context before `org_resolver::parse_ctx`. `harness/audit-v2-shortcuts.mjs` enumerates Org code/verbatim/emphasis, links, cookies, timestamps, footnotes, hiccup, tags, macros, block refs, HTML/autolinks, URLs, entities, and LaTeX-looking bytes against `mldoc@1.5.9`. |
| Markdown blockquote quote-only fast frame | Audited | `try_parse_quote_only_body` is a conservative quote-spine/paragraph shortcut. `quote_fast_paragraph_safe` now declines on every known block-content starter, including `*` and `_` so `***`/`___` HR lines re-enter the source-transcribed block-content path. `trim_paragraph_breaks_before_blocks` removes the full blank separator run before structural child blocks while preserving the Markdown comment exception. Regressions cover `> a\n>\n> ---` and `> a\n> ***`; the shortcut audit enumerates blank runs before HR/comment/list/property/raw/math/fence/callout/hiccup/plain children. |
| Markdown `#+BEGIN_QUOTE` / custom callout fast frame | Audited | `callout_fast_plain_line_safe` uses the same conservative block-content starter guard as quote frames, including `*` and `_`. The post-close trim uses the shared full separator-run trimming. Regression covers `#+BEGIN_QUOTE\na\n***\n#+END_QUOTE`; the shortcut audit enumerates blank runs before the same structural child set as blockquotes. |
| Markdown regular-list item content boundary handling | Audited | The shortcut audit enumerates list item body boundaries and blank runs before HR/comment/list/property/raw/math/fence/callout/hiccup/plain children. Current output matches `mldoc@1.5.9`; no code change was required in this pass. |
| Heading/bullet split suffixes | Audited | The shortcut audit enumerates Markdown heading and bullet title suffixes over HR/comment/list/property/raw/math/fence/callout/hiccup/table/footnote/plain candidates. Current output matches `mldoc@1.5.9`; no code change was required in this pass. |
| `rewrite_callout_suppressed_blocks` and `rewrite_list_item_suppressed_blocks` | Audited | `harness/audit-v2-shortcuts.mjs` enumerates document, quote, callout, list, and list-contained quote contexts over directives, `#+RESULTS`, parse2-looking `#+BEGIN_*:`, parse1 properties, `id::`, property drawers, generic drawers, footnote definitions, and Markdown comments. The full projection matches `mldoc@1.5.9`; context-specific suppressions remain covered by existing unit tests. |
| Origin remapping and span clearing for transformed bodies | Audited | The transformed-body contract is now explicit in `V2-TRANSCRIPTION.md` and `LINEARITY.md`: clean callout-frame children keep remapped public block spans, regular transformed quote/callout children remap inline and leaf-block spans through `OriginMap`, transformed paragraph block spans are cleared to match the current public projection, and the direct Org generic-drawer rewrite preserves local child block spans while remapping only inline spans. `harness/spans.mjs` remains the source-span invariant gate over the full differential corpus, and `harness/audit-v2-shortcuts.mjs` exercises quote, callout, list, suppression, bounded suffix, and property-ref remap contexts against `mldoc@1.5.9`. |
| Bounded same-line suffix helpers after raw HTML, displayed math, hiccup, properties/drawers | Audited | `harness/audit-v2-shortcuts.mjs` enumerates accepted displayed-math, hiccup, raw-HTML, property-drawer, and parse1-property prefixes followed by same-line, one-blank, and two-blank tails over paragraph, HR, comment, heading/bullet-looking text, list-looking text, properties, raw HTML, malformed raw HTML, displayed math, fences, callouts, hiccup, tables, and footnotes. Current output matches `mldoc@1.5.9`. |
| Ref extraction/property-value reparsing shortcuts | Audited | `harness/audit-v2-shortcuts.mjs` includes parse1 properties, parse2 properties, and list-contained properties over page refs, tags, quoted values, query/embed macros, block refs, Markdown `Search` links, `File` links, nested links, duplicates, and empty values. Ordinary block/title/body refs use OG `get-page-reference`/`get-block-reference`; property page refs use OG `text/extract-refs-from-mldoc-ast` over mldoc's precomputed property refs, and property block refs postwalk that same precomputed AST through `get-block-reference`. The full projection, including refs, is compared against `mldoc@1.5.9` plus the shared OG-faithful refs oracle. |
| Reported divergence corpus | Audited | `harness/reported-divergences.mjs` freezes 147 cases from the GH issue families, CRLF/lone-CR probes, minimized real-graph reproducers, refs-audit reproducers, and follow-up source-rule cases. It is a permanent `LSDOC_ENGINE=v2` gate and currently has zero diffs. |
| Top-level vs block/list-content paragraph separators | Audited | `mldoc_parser.ml` top-level order runs `Paragraph.sep` before later real block alternatives, so a paragraph followed by a heading/list/table-like block keeps its visible separator. `block0.ml` block-content and `lists0.ml` list-content put leading optional EOL consumption inside the child block parsers before `Paragraph.sep`, so nested structural children consume the separator. v2 now has explicit flush modes plus documented exceptions for Markdown comments, properties/drawers, directives, list-content `#+RESULTS`, and LaTeX environments. |
| Org link labels and `org_link_2` classification | Audited | `inline.ml` reparses `org_link_1` labels with a label config that allows `~code~` but not `=verbatim=`, and does not use the nested-emphasis repair context. `org_link_2` follows the first-colon `://` scan shape: empty protocol/link can classify as a URL, while `a:b://x` stays page-like. `src/org_resolver.rs` and `src/org.rs` now encode those rules directly, and the shortcut audit enumerates Org label/link2 cases. |
| Empty heading/list marker tails and fences | Audited | `block0.ml` `fenced_code_block` backtracks when no later matching fence close exists. Empty-marker tail dropping is therefore allowed only for an actually accepted following fence, not for a mere marker-looking line. The audit now distinguishes `- \n```\n` paragraph tails from closed fence tails. |
| Timestamp scanner rollback | Audited | `inline.ml` timestamp parsing tries ranges and general timestamps transactionally; `timestamp.ml` date parsing starts with an integer-shaped date token. v2 now commits only safe absence facts from failed general timestamp scans and prefilters non-date starts, preserving linearity while avoiding poisoned fallback state. |
| Markdown link label `[` branch | Audited | `inline.ml` `label_part_choices` has no plain-character fallback for `[` inside a Markdown link label: it must be a page ref/nested link or a balanced bracket group. v2 now declines malformed openers at that point, and the audit covers many-openers/one-close cases that previously exposed both parity and complexity risk. |
| Macro argument mldoc-space trim | Audited | Macro arguments skip mldoc spaces, not just ASCII space, so tab/form-feed/SUB around atoms are trimmed the same way the source parser trims them. The audit includes tabbed namespace macro arguments. |
| Markdown bullet-title simple strong | Audited | The whitespace-before-closer rule from mldoc nested emphasis matters inside bullet/heading title parsing as well as paragraph inline parsing. `markdown_fast_simple_bold` now declines closers preceded by mldoc whitespace and falls back to the source-transcribed resolver path. |

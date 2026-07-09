# lsdoc correctness audit, 2026-07-09

Scope: broad source audit of `lsdoc` parser, parity harness, and selected
documentation for correctness risks. This audit did not run the harness or test
suite because the delegation allowed only this report write.

## Findings

### 1. OG reference extraction cases are missing from `lsdoc::refs`

Severity: High

File/line refs:

- `src/refs.rs:143-165`
- `src/refs.rs:168-207`
- `src/inline.rs:3538`
- `src/inline.rs:5214`
- `src/inline.rs:5226`
- `src/org.rs:3588`
- `src/org.rs:3607`
- `src/org.rs:3613`
- `src/org.rs:3633`
- `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs:37-83`
- `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs:86-119`

Why this is suspect:

`src/refs.rs` only records page refs from `Link` URLs classified as
`PageRef`, block refs from `Link` URLs classified as `BlockRef`, tags, and
`embed` macros. The upstream Logseq extractor handles more cases:

- page refs from `Nested_link`
- page refs from `Search` links, including Org search links that are treated as
  page refs
- page refs from `File` links via the link label
- block refs from complex/generic links whose target is an `id` URL or a UUID

The Rust parser can emit these URL variants (`Search`, `File`, and `Complex`)
and nested links, but the reference walker drops them. Property reference
walking has the same issue: its comment says the top-level candidate set
includes `Nested_link`, but the match arm for `Inline::NestedLink` is empty.

That means downstream indexing can lose page or block references even when the
parser preserved the inline syntax in the AST.

Minimal repro/test idea:

- Markdown nested page link: `[[outer [[Inner]]]]`
- Org search/page link: `[[plain-search][label]]`
- ID block link: `[x](id://11111111-1111-1111-1111-111111111111)`
- File link whose label should become a page ref under the OG rules

Parse each example and compare `lsdoc::refs` output against Logseq's
`graph_parser.block/get-page-reference` and `get-block-reference` behavior.

Confidence: High. The mismatch is visible directly in the Rust and ClojureScript
source. Exact AST spelling of each repro should still be confirmed with a
non-mutating parser probe or a normal test run.

### 2. The JavaScript refs oracle omits the same OG branches

Severity: High

File/line refs:

- `harness/lib/refs.mjs:1-10`
- `harness/lib/refs.mjs:34-52`
- `harness/oracle.mjs:28-32`
- `docs/V2-TRANSCRIPTION.md:48`
- `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs:37-83`
- `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs:86-119`

Why this is suspect:

`harness/lib/refs.mjs` says it is an OG-faithful reference projection, but its
`Link` handling only checks `Page_ref` and `Block_ref`, then returns. It does
not port the OG `Search`, `File`, `Nested_link`, complex `id`, or generic UUID
link branches. `harness/oracle.mjs` uses this projection when producing oracle
data, so a refs parity run can be green while both Rust and the oracle omit the
same references.

This is especially risky because the README and v2 documents present the parity
harness as the guardrail for behavioral equivalence. A shared omission in the
oracle undermines that claim for reference extraction.

Minimal repro/test idea:

Add the examples from finding 1 to a small oracle corpus. First update
`harness/lib/refs.mjs` to port every branch in OG
`get-page-reference`/`get-block-reference`; then compare current Rust output
against that corrected projection. The current oracle is expected to miss at
least the nested-link, Org search-link, and `id://` cases.

Confidence: High. The harness code mirrors the Rust omissions and diverges from
the OG extractor.

### 3. Several v2 unit tests compare the implementation to itself

Severity: Medium

File/line refs:

- `src/lib.rs:73-74`
- `src/v2/mod.rs:24-31`
- `src/v2/block.rs:8226`
- `src/v2/block.rs:8278`
- `src/v2/block.rs:8299-8300`
- `src/v2/block.rs:8507-8508`
- `src/v2/block.rs:8862-8863`
- `src/v2/block.rs:9149-9150`
- `src/v2/block.rs:9317-9318`

Why this is suspect:

Many v2 tests assert `try_parse(...) == Some(crate::parse(...))`. Public
`crate::parse` now routes to `v2::parse_blocks`, and `v2::parse_blocks` wraps
the same `try_parse` path. After that ownership change, these tests are no
longer comparing v2 against an independent oracle, legacy parser, mldoc output,
or fixed expected AST. They mostly assert that one public wrapper agrees with
the internal implementation it wraps.

This can hide parser regressions in exactly the places where the test names
suggest parity or ownership behavior is being protected.

Minimal repro/test idea:

Temporarily change one v2 parsing branch covered by a
`Some(crate::parse(...))` expectation. The expected value will move with the
implementation. Replace representative self-comparisons with explicit AST
fixtures, mldoc golden JSON, or a separate legacy parser entry point if one is
still intended to exist.

Confidence: High for the test weakness. This does not prove a current runtime
bug, but it does reduce confidence in the current correctness gates.

### 4. UUID reference casing may not match OG identity semantics

Severity: Low

File/line refs:

- `src/refs.rs:243-259`
- `harness/lib/refs.mjs:13-14`
- `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs:86-119`

Why this is suspect:

`parse_uuid` validates UUID shape but returns the original string unchanged.
The JavaScript oracle does the same. The OG extractor calls into UUID parsing
before storing block refs, which likely canonicalizes UUID identity rather than
preserving input casing. If so, uppercase and lowercase spellings of the same
block UUID could be treated as different reference keys by `lsdoc`.

This is lower severity because the exact downstream expected representation
needs confirmation, but the current Rust/oracle pair would not catch a casing
semantic mismatch.

Minimal repro/test idea:

Compare reference extraction for `((AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA))`
and `((aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa))` against OG/Tine storage behavior.
If OG canonicalizes, assert that `lsdoc` returns the canonical lowercase UUID or
otherwise normalizes before identity comparison.

Confidence: Medium-low. The risk follows from source shape, but confirmation of
OG runtime UUID representation is needed.

### 5. Parity status documents disagree about whether known divergences remain

Severity: Low

File/line refs:

- `docs/V2-PARITY-CLOSURE-EXECUTION-PLAN.md:23-43`
- `docs/V2-SYSTEMATIC-PARITY-AUDIT.md:20-38`
- `docs/V2-SYSTEMATIC-PARITY-AUDIT.md:247`

Why this is suspect:

One document lists current known v2 failures and a closure plan, while another
states that the reported-divergences gate has zero diffs and that parity closure
is complete. These files appear to represent different snapshots, but both are
present in the tree. A future maintainer could trust the wrong status and skip
rechecking known parser edge cases.

Minimal repro/test idea:

Run the reported-divergences gate from a clean worktree and update or archive
the stale document so there is a single current source of truth for v2 parity
status.

Confidence: Medium for the documentation inconsistency, low for any direct
runtime impact.

## Areas inspected

- Public parser dispatch and v2 ownership boundaries in `src/lib.rs` and
  `src/v2/mod.rs`
- v2 block parser tests and representative parser logic in `src/v2/block.rs`
- Reference extraction in `src/refs.rs`
- JavaScript parity reference projection in `harness/lib/refs.mjs` and oracle
  wiring in `harness/oracle.mjs`
- Relevant OG reference extraction source in
  `../og/deps/graph-parser/src/logseq/graph_parser/block.cljs`
- Org and Markdown link classification paths in `src/org.rs`, `src/inline.rs`,
  and selected upstream mldoc parser code
- Selected v2 parity and design documents, including README, design notes,
  divergence reports, and transcription notes

## Areas not inspected

- I did not run `cargo test`, fuzzers, the reported-divergences harness, or the
  mldoc oracle because this audit was constrained to one report write.
- I did not fully audit `render_html`, entity-table internals, every inline
  scanner branch, raw HTML parsing, or every v2 block parser branch.
- I did not inspect all generated corpus outputs or performance artifacts.
- I did not verify the suspected UUID casing behavior against a live OG/Tine
  runtime.

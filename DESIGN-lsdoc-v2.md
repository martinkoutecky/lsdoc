# lsdoc v2 architecture (as built)

lsdoc parses Logseq-flavored Markdown and Org into the public typed AST. The
public `parse`, `parse_format`, and outline entry points all use the v2 parser;
there is no silent fallback to the legacy parser. An input outside v2's proven
ownership boundary fails closed. The detailed source ledger lives in
`docs/V2-TRANSCRIPTION.md`; scan ownership and sanctioned complexity exceptions
live in `docs/LINEARITY.md` and `CLAUDE.md`.

## Phase A: one source pass

`src/v2/source.rs::Source::scan` walks every source byte monotonically. It emits
borrowed line windows with physical offsets and builds the deterministic event
indexes used by block parsing:

- fence line positions;
- drawer and property closer positions;
- a prefix-aware `#+END_` trie for callout/special bodies;
- sparse hiccup opener/closer pairs.

The source pass does not construct AST nodes and does not decide block grammar.
It supplies bounded, monotone lookup owners so later parser choices never need
to rescan the document to EOF.

## Phase B: source-transcribed block machine

`src/v2/block.rs` advances a single line cursor through the source windows in
mldoc parser order. Each branch owns only inputs for which it can reproduce the
source parser's acceptance, output, rollback, and context rules. A branch that
cannot prove ownership declines without committing state.

Document, block-content, and list-content contexts are distinct because mldoc
enables different parser families and separator behavior in each. Quotes,
callouts, list items, drawers, and split-title suffixes use bounded body views or
origin maps; accepted source bytes are copied/remapped once and all closer/event
queries are monotone or direct-indexed. Deep structural assembly uses explicit
stacks where native recursion would make depth a correctness or safety limit.

## Phase C: inline delimiter machine

Markdown and Org share the source-transcribed inline helpers in `src/inline.rs`,
`src/resolver.rs`, and `src/org_resolver.rs`. A conservative fused fast path owns
plain runs and locally proven common constructs. The first unowned construct
causes it to decline to the full lexer/resolver path; it never guesses an AST.
Delimiter, closer, bracket, hiccup, URL, raw-HTML, and timestamp scans use one
owner per inline buffer, with monotone cursors or position-indexed memo tables.

The legacy block entry points remain available only through the explicit
`__parse_format_legacy` harness hook and as shared helper provenance. Public
parsing never falls back to them. The obsolete `Scanner`/`OrgScanner` inline
types named by the old M11 backlog entry no longer exist.

## Executable proof

The architecture is enforced by complementary gates:

- `harness/run.mjs`: ordinary, mined, real-graph, inline, span, and shortcut
  differential corpora;
- `harness/reported-divergences.mjs`: isolated-oracle reports from real users and
  GitHub issues;
- `harness/fuzz.mjs`: zero-diff floor for Markdown and Org;
- `tests/complexity.rs`: deterministic scan-work-per-byte bounds (the
  load-bearing asymptotic proof);
- `tests/perf.rs`: stack-safety and timed scaling guards;
- `bench/`: representative throughput against comrak and orgize.

The isolated oracle is authoritative for focused probes because mldoc leaks
process-global state across parses.

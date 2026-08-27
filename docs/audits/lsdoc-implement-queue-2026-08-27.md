# lsdoc Implement queue receipt — 2026-08-27

This receipt closes the four private routing cards whose source of truth is
`BACKLOG.md`. The wire AST and parser parity contract remain unchanged.

## F10 — single-pass raw-HTML tag index

`RawHtmlScan` now discovers every recognized raw-HTML opening, closing, and
self-closing event in one monotone source walk. Events are dispatched to the
allowlist's direct tag index. Exact-case opening variants remain separate, while
case-insensitive closing behavior remains shared, preserving mldoc behavior.
The existing per-tag next-strict-below tables and monotone query cursors are
retained.

The deterministic complexity suite now includes a 20-distinct-tag family and a
fixed scan-work/byte ceiling, guarding the constant-factor regression that the
ordinary asymptotic ratio gate could not detect.

## Hiccup HTML rendering

The AST continues to carry raw `hiccup.v` text. At render time, the bundled HTML
renderer reads the Clojure vector/map/string subset used by hiccup, renders
allowlisted nested tags, selector ids/classes, attributes, and text, and escapes
all text/attribute content. Dangerous element families, event attributes,
`dangerouslySetInnerHTML`, and script-bearing URL attributes are declined. This
mirrors OG's safe-reader then sanitize boundary without treating the raw vector as
trusted HTML.

## Iterative projection serialization

The public `projection_to_json` and `blocks_to_json` APIs serialize recursive
block, list-item, and inline children through an explicit heap work stack. The
differential CLI now uses this path. A one-MiB-stack regression serializes a
20,000-level quote tree, then disposes of it through the existing iterative drop
path. The full differential corpus also passes through the iterative serializer,
proving its JSON is semantically identical to the serde contract.

Downstream WASM consumers should use these APIs when they next pin this lsdoc
release; direct `serde_json::to_string` remains available for ordinary shallow
values but is inherently recursive.

## Explicit line lexer boundary

`v2::source::lex_lines` now names the single boundary that produces both physical
line windows and line-owned source events. It delegates to the existing monotone
scanner, so the clarity change adds no source pass, allocation regime, or parser
behavior.

## Acceptance evidence

- `harness/run.mjs`: 1402/1402 refs, block structure, and full block projections;
  0 real-block diffs; 37/37 inline parity; 0 span violations.
- `cargo test --lib`: 199 passed.
- `cargo test --test render`: 47 passed.
- `cargo test --test complexity`: active gates passed, including the new
  20-distinct-tag family.
- `cargo test --release --test perf -- --ignored`: 7 passed.
- `node fuzz.mjs 40000 99` and its Org counterpart: 0 ref mismatches and 0 block
  mismatches across 40,000 cases in each format.

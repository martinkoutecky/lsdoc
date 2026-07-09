# lsdoc performance audit - 2026-07-09

Scope: broad source audit of `/aux/koutecky/logseq/lsdoc` for nonlinear behavior, repeated scans, avoidable copies, allocation-heavy paths, and obvious speedups. I did not implement fixes.

## Findings

### 1. Index-only `refs()` pays for the full render AST

Severity: Medium

Refs:
- `src/lib.rs:77-80`
- `src/v2/mod.rs:14-21`
- `src/refs.rs:13-43`

Why suspect: The public `refs(input, format)` API is documented as the index path, but it calls `parse_format(...).refs`, and `parse_format` first builds the complete block/inline render tree, then runs `refs::extract_refs` as a separate tree walk. This is linear, but it makes the index-only path allocate the full AST and then traverse it again. For a graph indexer that only needs refs, this is a large fixed tax and defeats a natural "parse less for indexing" expectation.

Complexity/benchmark idea: Compare `parse(input, fmt)`, `parse_format(input, fmt)`, and `refs(input, fmt)` on a prose-heavy graph corpus and on a ref-heavy synthetic corpus. Track wall time and allocations with `heaptrack`/`valgrind massif` or a custom allocator counter. Expected shape today: `refs()` is approximately `parse_format`, not a lightweight pass; cost is `O(N + AST + R log R)` rather than a dedicated `O(N)` scanner with reduced allocation.

Confidence: High

### 2. Reference extraction clones/materializes every hit, reparses property values, then sorts/dedups

Severity: Medium

Refs:
- `src/refs.rs:39-42`
- `src/refs.rs:91-113`
- `src/refs.rs:153-161`
- `src/refs.rs:175-195`
- `src/refs.rs:209-230`
- `src/refs.rs:234-258`

Why suspect: Ref extraction pushes owned `String`s for page/block refs, materializes tag text into a new `String`, joins macro args with `args.join(", ")`, reparses every eligible property value for property-reference semantics, and finally sorts/dedups both ref vectors. The sort is a documented canonical-order tradeoff, but it is still the obvious superlinear component in the public projection path. The property reparse also means some bytes are parsed as normal inline content during block parsing and parsed again for refs.

Complexity/benchmark idea: Generate `N` unique refs (`[[p000000]] ... [[pN]]`) and `N` duplicate refs, plus `N` properties such as `k:: {{query [[P_i]]}}`. Measure `parse_format` time and allocation counts at `N, 2N, 4N`. Unique refs should show the `R log R` sort cost more clearly; duplicate refs should show clone/materialization pressure even when the final set is small.

Confidence: High

### 3. Plain/prose-heavy parsing is still multi-pass and copy-heavy

Severity: Medium

Refs:
- `src/v2/source.rs:43-52`
- `src/v2/source.rs:125-198`
- `src/v2/block.rs:78-91`
- `src/v2/block.rs:783-790`
- `src/inline.rs:68-76`
- `src/inline.rs:102-105`
- `src/inline.rs:730-768`
- `src/inline.rs:834-843`
- `src/lexer.rs:81-107`
- `src/lexer.rs:122-136`
- `src/resolver.rs:219-221`
- `src/resolver.rs:423-430`

Why suspect: The v2 source pass walks the input to build line/event indexes, the block loop classifies those lines, paragraph flushing calls `inline_at`, and the inline fast path walks the bytes again before copying accepted plain slices into `Inline::Plain`. If the fast path declines, the fallback lexer builds owned `Text(String)` tokens and the resolver then materializes plain strings again. This is not a nonlinear bug, and the code has explicit scan ownership, but it is the main visible throughput/copy tax on prose-heavy documents.

Complexity/benchmark idea: Use the existing `bench/` harness with near-plain prose, markup-dense text, and mixed Logseq pages. Add allocation counts and bytes-copied counters around `Source::scan`, `plain_fast_path_*`, `lexer::lex`, and `Inline::Plain` creation. Expected shape: linear time, but high ns/byte and allocation/copy count on plain prose compared with a borrowed-slice or fused lexer/resolver design.

Confidence: High

### 4. HTML rendering reparses property values and repeatedly flattens labels into temporary strings

Severity: Low/Medium

Refs:
- `src/render.rs:236-250`
- `src/render.rs:506-507`
- `src/render.rs:581-586`
- `src/render.rs:591-600`
- `src/render.rs:626-630`
- `src/render.rs:731-752`
- `src/render.rs:755-784`

Why suspect: `render_html` parses each property value as inline markup at render time, even if the caller just parsed the document. It also calls `flatten_text` for tags, block-ref labels, PDF labels, and image alt text; `flatten_text` allocates a fresh `String`, and nested empty-label links call `url_dest`, which can allocate again. This is linear, but render-heavy callers with many tags/images/properties will pay repeated tree walks and temporary allocations.

Complexity/benchmark idea: Build an AST with many properties containing inline markup, many tags with nested children, and many image/PDF links with long labels. Benchmark `render_html` alone at `N, 2N, 4N`, with allocation counts. A useful regression test would assert linear scaling and a budget for allocations per rendered inline node.

Confidence: High

### 5. Sparse hiccup closer index introduces a documented sort/log factor

Severity: Low

Refs:
- `src/v2/source.rs:340-365`

Why suspect: `HiccupClosers` stores matched opener/close pairs, sorts them at the end of the source pass, and uses binary search for lookups. This is an intentional sparse-index tradeoff, but it is one of the few visible `O(H log H)` components in v2 block parsing, where `H` is the number of matched hiccup openers. It should remain visible in performance documentation because a dense or monotone alternative would have a different memory/time shape.

Complexity/benchmark idea: Compare flat hiccups (`[:a]` repeated) and deeply nested hiccups (`[:a` repeated then `]` repeated) at `N, 2N, 4N`. Measure source-pass time separately from full parse. Expected shape: mostly linear input scanning plus `H log H` sorting; lookup cost should remain small unless hiccup-heavy documents dominate.

Confidence: Medium

## Areas inspected

- Public API dispatch and parse/projection entry points: `src/lib.rs`, `src/v2/mod.rs`.
- Reference extraction: `src/refs.rs`.
- v2 block/source pipeline: `src/v2/source.rs`, `src/v2/block.rs`.
- Inline parser hot paths and fallback lexer/resolver: `src/inline.rs`, `src/lexer.rs`, `src/resolver.rs`, with spot checks in `src/org_resolver.rs`.
- HTML renderer: `src/render.rs`.
- Performance/complexity harnesses: `bench/src/main.rs`, `tests/perf.rs`, `tests/complexity.rs`.
- Project performance notes: `README.md`, `CLAUDE.md`, `BACKLOG.md`, `DESIGN-lsdoc-v2.md`, `DIVERGENCES.md`.

## Areas not inspected

- Full line-by-line audit of the legacy parsers `src/parse.rs` and `src/org.rs`; I sampled them mainly to distinguish legacy-only code from the v2 public path.
- Generated/static entity data in `src/entities.rs`.
- Harness JavaScript and corpus generation beyond quick context checks.
- Running fresh benchmarks or profilers; this report is source-audit based.
- Downstream consumers of the recursive AST, except for noting parser/drop comments in existing tests and backlog docs.

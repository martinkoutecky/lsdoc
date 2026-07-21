# Investigation brief — lsdoc v2 panics on a real `.md` page (`does not yet own "md" input`)

**Status:** OPEN, needs investigation. **Filed:** 2026-07-21 (by Claude, from the Tine side).
**lsdoc at time of filing:** v0.5.3 (`96e9096`), mldoc oracle `1.5.9`.
**Not a data-loss emergency** (Tine already contains the blast radius — see §7), but a
real real-graph coverage gap in the parser.

---

## 1. TL;DR
A real Logseq **Markdown** page — in a user's 12,698-page graph — makes lsdoc v2's
`block::try_parse` return `None`, which trips the deliberate fail-safe panic
`lsdoc v2 parser does not yet own "md" input`. That is **a genuine transcription
gap**: some Markdown construct that `mldoc@1.5.x` parses is not yet owned by lsdoc
v2, so v2 (correctly, by design) refuses to guess and panics instead of
mis-parsing. We do **not** have the offending page's content. Your job: find which
construct it is (or, more valuably, close the remaining un-owned surfaces) and make
`try_parse` return the correct blocks instead of `None`.

## 2. Where this came from (provenance)
- Reported inside **Tine GitHub issue #209** ("Ctrl+K Search Can Silently Omit
  Large Parts of a Graph"), by contributor **EllisMorrow**, in a detailed 0.6.2
  search-reliability audit. Track "who said what": this is the reporter's finding,
  not Martin's.
- Empirical fact from that audit: directly loading all 12,698 page files succeeded
  for 12,697 and **consistently, deterministically panicked for exactly one** with
  `lsdoc v2 parser does not yet own "md" input`. Format is **`md`**, not org.
- We do NOT have that page (it is in the reporter's private graph). Do **not** try
  to obtain or read any private graph (Martin's `~/research/brain` is hard-off-limits;
  the reporter's graph is not ours either). Work from a minimal input the reporter
  supplies via #209, plus the surface audit below.

## 3. Exact mechanism (source-precise)
- `parse_format` / `parse_blocks` (`src/v2/mod.rs:26` and `:31`) call
  `block::try_parse(input, "md")` and `.unwrap_or_else(|| panic!("lsdoc v2 parser
  does not yet own {format:?} input"))`. So the panic == `try_parse` returned `None`.
  This is the intended fail-fast guard (see `DESIGN-lsdoc-v2.md`, `CLAUDE.md`:
  "designed to fail safe on un-transcribed input rather than mis-parse").
- `block::try_parse` (`src/v2/block.rs:19`) → `try_parse_leaf_blocks(_in)`.
  `None` originates at the **`*Decision::Delegate => return None`** bail-outs — the
  places where v2 recognizes it is looking at a construct it has NOT fully
  transcribed and deliberately punts. As of `96e9096` these are:

  | line | surface (construct) |
  |---|---|
  | `block.rs:156` | `PropertyDrawerDecision::Delegate` |
  | `block.rs:349` | `BlockquoteDecision::Delegate` |
  | `block.rs:374` | `DisplayedMathDecision::Delegate` |
  | `block.rs:404` | `RawHtmlDecision::Delegate` |
  | `block.rs:429` | `HiccupDecision::Delegate` |
  | `block.rs:460` | `ListDecision::Delegate` |
  | `block.rs:255`, `block.rs:535` | other guarded `return None` |

  **Exactly one of these fires on the reporter's page.** (There are more `Decision`
  enums — Table/Fence/RawSrcExample/CalloutContainer/LatexEnv/Heading around
  block.rs:1673/2004/2096/2101/5000/5915 — grep `Delegate,` under each `enum
  *Decision` to get the current full list; the set may have shifted since filing.)

## 4. Why the fuzzer didn't catch it
lsdoc's fuzz floor is **0** (`node fuzz.mjs` matches mldoc byte-for-byte on
generated input, both formats, blocks+refs, any seed). But the fuzz grammar does
not generate whatever this construct is — real Logseq graphs contain constructs
the generator doesn't. So "fuzz floor 0" ≠ "owns all real input". This case is the
proof.

## 5. Investigation plan (two tracks — run both)

### 5a. Get the minimal input (fastest, needs the reporter)
The Tine-side #209 reporter comment already asks EllisMorrow for the **minimal
block text** that triggers the panic. To make it easy for them to identify it
WITHOUT sharing private content, ship/suggest a **diagnostic build**: replace the
two generic `panic!` messages in `src/v2/mod.rs` (temporarily) — or, better, give
each `Delegate` a static reason string — so the message names the exact surface and
byte offset, e.g. `does not yet own "md" input [RawHtmlDecision::Delegate @
block.rs:404, offset 812]`. The reporter can run the portable graph-check tool
(`graph-check.mjs`, repo `martinkoutecky/lsdoc`) with that build on their own graph
and report just the surface + the (redactable) construct. Then you know precisely
which decision to complete, and can build a minimal fixture from it.

### 5b. Audit the un-owned surfaces (proactive, needs NO external input)
This is the higher-value track and can start immediately. For **each**
`*Decision::Delegate => return None` site:
1. Read the decision function to learn the exact Markdown that reaches `Delegate`
   (the condition under which v2 gives up).
2. Construct a **minimal input** that hits that `Delegate`.
3. Run it through mldoc via **`node harness/vdiff_iso.mjs <probe.json>`** (isolated
   oracle — one fresh process per input; do NOT use a batched `oracle.mjs` probe:
   mldoc leaks global state across parses in one process and will show false
   divergences — see `CLAUDE.md`).
4. Transcribe mldoc's behavior so `try_parse` returns the correct blocks instead of
   `None`, and add the case to the curated corpus.
Prioritize by real-Logseq likelihood: **raw-HTML blocks, blockquotes containing
fences/`#+BEGIN`, property drawers, hiccup `[:tag ...]`, deeply/irregularly nested
lists, displayed math `$$…$$`**. Closing each `Delegate` shrinks the set of real
pages that can ever trip the panic — independent of whether we ever get PAGE-001.

## 6. Validation gates (all must stay green after any change)
From repo root, `source scripts/env.sh` first:
- `cd harness && node run.mjs` — corpus + blockgate + inlinegate (exits non-zero on
  any diff). Add a regression case for every `Delegate` you close.
- `node fuzz.mjs 40000 99` (append `org` for org) — **floor must stay 0**, both
  formats, blocks+refs, any seed.
- `cargo test --lib` and `cargo test --test render`.
- `cargo test --test complexity` — the load-bearing O(n) structural guard. Any new
  re-scan you introduce must be gated here (`complexity_gate`).
- Keep the projection-sync invariant (`harness/lib/normalize.mjs` ↔
  `src/projection.rs`).
Adjudicate any ad-hoc byte-exactness probe with `harness/vdiff_iso.mjs`, never a
batched probe.

## 7. Impact & urgency (why this is not a fire, but matters)
Tine already fixed the **downstream catastrophe** (GH #209 finding 5.1): its
whole-graph search cache used to run parsing across worker threads and join with
`join().unwrap_or_default()`, so ONE panicking page turned that worker's entire
~1/8 shard into an empty result — silently dropping many unrelated pages from
search. Tine now isolates each page's parse in `catch_unwind`, skips only the bad
page, and reports it (`Graph::page_index_failures`). So **the panic no longer harms
other pages**. BUT the offending page (and any real page with the same construct)
stays unparseable → invisible to search/refs until this lsdoc gap is closed. Every
`Delegate` you complete directly widens real-graph coverage.

## 8. Boundaries / rules
- Reimplementation rule: **TRANSCRIBE the corresponding mldoc function**, do not
  reverse-engineer behavior empirically or "make something up" to stop the panic.
  The `Delegate` panic is a feature — replace it with a faithful transcription, not
  a lenient fallback that guesses.
- Accept any mldoc ≥ 1.5.7 as oracle; don't replicate upstream-fixed 1.5.7 bugs
  (1.5.7 is the default gate; current bundle is 1.5.9).
- Do not read any private graph. Minimal inputs come from GH #209 or your own
  Delegate-audit fixtures only.

## 9. Handy references
- Panic: `src/v2/mod.rs:26,31`. Gap surfaces: `src/v2/block.rs` `*Decision::Delegate`.
- Design/rationale: `DESIGN-lsdoc-v2.md`, `CLAUDE.md`, `DECISIONS.md`.
- Prior real-graph gaps: `V2-REAL-GRAPH-DIVERGENCES.md`, `DIVERGENCES.md`.
- Oracle/verify: `harness/run.mjs`, `harness/fuzz.mjs`, `harness/vdiff_iso.mjs`,
  `harness/oracle.mjs`.
- Upstream source of truth: `mldoc@1.5.9` (`frontend/format/*`, block/markdown).
- Reporter channel: Tine GitHub issue **#209** (`martinkoutecky/tine`).

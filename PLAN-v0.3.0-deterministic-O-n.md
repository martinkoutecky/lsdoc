# PLAN v0.3.0 — deterministic O(n) lsdoc

**Status:** approved by Martin (2026-06-30), DEFERRED to a fresh session. Point me here to execute.
**Goal (3 items, in order):** (1) a repo-local process note "avoid hashes if an array would do";
(2) replace the HashMaps (most → arrays, the entity dict → a perfect hash); (3) turn the parser's
**O(n log n) into deterministic O(n)**. Ship as **v0.3.0**.

Companion memory: [[lsdoc-on-optimization]] (the full analysis — read it). This plan is the executable
version. Current head: v0.2.5.

---

## The current state (verified 2026-06-30 against the code)
lsdoc's parse is **O(n log n)**, not O(n) (disregarding the guarded `BLOCK_NEST_CAP` O(n²) residuals).
Two log-factor sources + a determinism gap:
- **L1 — binary-search closer lookups** (`partition_point`, O(log n) per block/drawer opener):
  `EndTrie::find` (`#+END_<name>`, `block_common.rs`), `find_drawer_end` (`:END:`, `block_common.rs`),
  org `nonstd_eol_lines` (`org.rs`). Fences are already O(1) via a monotone `fence_cursor` — the model.
- **L2 — `refs.rs` `page.sort()`/`block.sort()`** (each followed by `.dedup()`) — the sort is for dedup.
- **Determinism gap — 17 std HashMaps = SipHash** (Rust's slow DoS-resistant default; O(1) *expected*,
  O(n) worst-case on collisions). Three buckets:
  - EndTrie `kids: HashMap<u8,u32>` — keyed on a BYTE.
  - bracket-match `hiccup_close`/`nested_close: HashMap<usize,usize>` (`inline.rs`,`resolver.rs`,`org_resolver.rs`) — keyed on a POSITION.
  - entity table: `OnceLock<HashMap<&str,Entity>>` built from the static `ENTITIES` slice (`entities.rs`, ~339 entries).

## HONEST framing of the value (do NOT oversell — Martin's anti-handwave rule)
The asymptotic O(n) vs O(n log n) is **practically invisible on real input** (log n is tiny). The real
wins are: **(a) removing SipHash** (a genuine constant-factor speedup — measurable, the main practical
gain), and **(b) determinism** (no adversarial hash-collision worst-case — a parser eats arbitrary
input). The clean O(n) bound is the third, smaller, mostly-guarantee win. Frame v0.3.0 as
"deterministic + SipHash-free + O(n)," not "dramatically faster."

## Anti-drift invariants (re-read EVERY step)
1. **ZERO behavior change** — these are perf refactors. After EVERY step: `cd harness && node run.mjs`
   = **1230/1230 + blockgate 99 + inlinegate 37, 0 diffs**; `node fuzz.mjs 40000 99` md=**555** + `… org`
   =**1522** (unchanged — a CHANGE means a behavior bug); `cargo test --lib` + `--test render` (44) +
   `--release --test perf -- --ignored` (green, no O(n²)). Commit per step.
2. **O(n) is proven BY CONSTRUCTION, not by the perf ratio.** The perf gate's CAP=3.0 ratio CANNOT
   distinguish O(n) from O(n log n) (both ≈2×/doubling). Verification = a structural audit: no
   `.sort`/`partition_point`/`binary_search`/`HashMap` left on the hot path. A large-n block-opener-heavy
   benchmark is a WEAK secondary signal (O(n log n) is ~2.1×/doubling vs ~2.0× — ~5%, near noise).
3. **PROFILE FIRST and AFTER.** Don't state a speedup magnitude you haven't measured.
4. The guiding principle is item 1: **prefer a deterministic array/index to a hash whenever the key is
   (or can be) a small integer / position / fixed set.**

---

## Step 0 — process note (item 1)  [lsdoc has NO CLAUDE.md — CREATE it]
Create `/aux/koutecky/logseq/lsdoc/CLAUDE.md` with the principle:
> **Avoid hashes if an array would do.** Prefer a deterministic array/direct-index (or a small sorted
> Vec, a perfect hash for a fixed set, a monotone cursor) over a general `HashMap`/`HashSet` whenever
> the key is a small integer, a byte, a position, or a fixed dictionary. Rust's std HashMap is SipHash
> (slow for small keys, only O(1) *expected*). Real O(n) parsers (cmark, tree-sitter, gperf, simdjson)
> use deterministic structures, not general hashes, on the hot path. Cautionary tale: lsdoc once hashed
> a `u8` (EndTrie children) — hashing a byte is strictly more work than indexing by it.
Plus the O(n)-by-construction reminder (already in spirit from the block rewrite). Keep it short.

## Step 1 — profile baseline
`cargo build --release`; profile (`valgrind --tool=callgrind` or `perf record`) over: the corpus
(`harness/corpus*.json` concatenated), the fuzz inputs (`fuzz.mjs` dumps them), and TWO synthetic
worst-cases — block-opener-heavy (`#+BEGIN_a{k}\n…#+END_a{k}` × N, distinct names) and ref-heavy
(`[[a{k}]]` × N). Record the % of parse time in: EndTrie, drawer/eol lookups, the two bracket maps,
`refs.rs` sort, entity lookups. This calibrates which steps are worth the constant + gives before/after.

## Step 2 — hashes → arrays / perfect hash (item 2)  [each: gate 0-diff after]
- **2a. EndTrie `kids: HashMap<u8,u32>`** → a deterministic child map. Small fan-out per node ⇒ a sorted
  `Vec<(u8,u32)>` linear-scanned is likely best (tiny, cache-friendly); `[u32;256]`/`[Option<NonZeroU32>;256]`
  if you want O(1) (≈1–2 KB/node, fine for few nodes). No SipHash. **Combine with 3a** (same struct).
- **2b. `hiccup_close`/`nested_close: HashMap<usize,usize>`** → `Vec<usize>` (sentinel) or
  `Vec<Option<usize>>` indexed by position, length = the scanned string. Deterministic, no hash. 3 sites:
  `inline.rs` (`build_hiccup_close`/`build_nested_close`), and the `resolver.rs`/`org_resolver.rs` callers.
- **2c. entity table** → the `ENTITIES` static slice is already an array; the HashMap is just its index.
  Replace with EITHER (i) **`phf`** (compile-time perfect hash — what Martin asked for; adds the `phf`
  dep) OR (ii) **assert `ENTITIES` is sorted by name + binary search** (no new dep, O(log 339)≈O(1)).
  This is a FIXED dict (constant size) → it's about determinism + SipHash, NOT the asymptotic. Recommend
  (ii) unless the profile shows entity lookup hot enough to want phf's true O(1).

## Step 3 — O(n log n) → O(n) (item 3)  [each: gate 0-diff after]
- **3a. `EndTrie::find` `partition_point` → per-node monotone cursor.** TRICKIEST. The query is
  "first `#+END_<name>` line `> from`"; `from` (the opener line) is **monotone-increasing per trie node**
  because the driver processes openers in line order. So a per-node two-pointer cursor (advance while
  `ends[c] ≤ from`) is O(1) amortized, total O(n). The query is a monotone function of `from`, so the
  cursor is correct **regardless of demotion/nesting** (a demoted opener still only asks "first end >
  from"). VERIFY hard against the byte-exact gate + targeted nested/demotion tests; the prefix-trie shape
  (a node's `ends` = all `#+END` with that prefix) is the subtle bit.
- **3b. `find_drawer_end` `partition_point` → a single monotone cursor** over the flat `:END:` list (drawer
  openers also process in line order). Easy.
- **3c. org `nonstd_eol_lines` `partition_point` → cursor** (same pattern; check it's monotone-queried).
- **3d. `refs.rs` sort+dedup — the genuinely-hard piece (string dedup is inherently sort-or-hash).**
  FIRST investigate: does `harness/compare.mjs` compare refs ordered or as a set, and does mldoc emit refs
  sorted or in document order? (If the gate sorts both sides, lsdoc's `.sort` is only for its own output
  and the sole need is DEDUP.) Then choose:
  - (a) **keep the comparison sort, O(n log n)** — refs are usually few; document it as the one O(n log n)
    component (the *parse* is still O(n)). Simplest, honest.
  - (b) **MSD/radix sort the ref strings, O(total ref chars)=O(n) deterministic** — strict goal, bigger
    constant. Then dedup adjacent.
  - (c) **intern ref strings via a trie during the walk → dedup on integer ids (bitset), O(n)** — complex.
  Decide from the profile (is `refs.sort` even hot?). Default recommendation: (a) + an honest note, unless
  the profile says refs dominate, then (b). DO NOT introduce a HashSet here (violates item 1 + determinism).

## Step 4 — verify (the real proof)
- Full gate 0-diff + floors + perf + lib/render, after each step AND at the end.
- **Structural O(n) audit:** `grep -rn 'partition_point|binary_search|\.sort|HashMap' src/` ⇒ only the
  refs sort may remain (if 3d option (a)); nothing else on the hot path. This — not a perf ratio — is the
  proof of O(n).
- Profile-after vs the Step-1 baseline; report the real before/after (likely a modest constant win from
  killing SipHash + the binary searches).

## Step 5 — ship v0.3.0
Bump `0.2.5 → 0.3.0` (Cargo.toml + Cargo.lock), tag `v0.3.0` with notes (deterministic, SipHash-free,
O(n) parse; refs caveat if 3d-(a)), push. Update [[lsdoc-on-optimization]] + `DESIGN-lsdoc-v2.md`'s
complexity claim. No Tine impact (perf-only, byte-exact) — Tine bumps the pin at leisure.

## Execution notes
- Mechanical + delegable (gate-verify each): 2a, 2b, 2c, 3b, 3c.
- Do-myself / verify-closely: Step 1 (profile), **3a (the EndTrie cursor)**, **3d (refs)** — the subtle/
  judgment ones.
- One commit per step (or per coherent pair, e.g. 2a+3a on the EndTrie).

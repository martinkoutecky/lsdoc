# Spec: a from-scratch Rust reimplementation of Logseq's `mldoc` parser

> **Historical — initial greenfield brief.** This is the original kickoff spec, not the current state of the parser (Markdown + Org are complete and gated). For what shipped, see README.md and DESIGN-lsdoc-v2.md.

**Name:** `lsdoc`. Repo sits next to Tine at `/aux/koutecky/logseq/lsdoc/` (Martin is
creating a private GitHub repo for it).

**Status:** greenfield. You are the first agent on this. `git init`, set up the Cargo
crate, build the verification harness *before* the parser. Work as autonomously as you can.

---

## 0. Mission (one paragraph)

Build **one** real, native-Rust parser that turns Logseq-flavored Markdown — and
eventually Org — into a typed **AST**, behavior-equivalent to Logseq's `mldoc`. It will
become the single source of truth for parsing in **Tine** (the sibling outliner),
replacing Tine's *two* divergent parsers. Correctness is checked **differentially against
real mldoc** (which is runnable). The non-obvious hard part: passing every correctness
diff does **not** protect you from dragging in `O(n²)` or `O(2^n)` behavior while
producing correct output. Complexity discipline is a first-class, separately-tested
requirement.

---

## 1. Context — why this exists

**Tine** (`/aux/koutecky/logseq/logseq-claude`, sibling repo, **read-only** for you) is a
Tauri + SolidJS Logseq-compatible outliner. Today it parses markdown **twice**:

- **Rust** (`crates/tine-core/src/`): `refs.rs`, `doc.rs`, `org.rs` — hand-written
  character scanning, **zero regex**. Parses the block tree and **extracts references**
  for indexing (backlinks, queries, ref-count badges).
- **TypeScript** (`src/render/parseInline.ts`): a *second, independent* inline parser
  (~65% hand-scan, ~35% small sticky regexes) that re-parses the raw block string into a
  `Seg[]` for **rendering**.
- The DTO boundary between backend and frontend carries the **raw markdown string**, not
  an AST. So inline markdown is parsed twice, in two languages, by two codebases that
  must agree.

**These two parsers drift.** Concrete example already hit and fixed: the labeled block ref
`[label](((uuid)))` (triple paren) was mis-parsed and had to be corrected in *both*
`refs.rs` and `parseInline.ts`. That dual-parser divergence is the entire bug class we are
killing — and it's exactly the kind of "rare input → silent wrong result → painful to
hunt" failure mode Martin cares about.

**OG (Logseq itself)** uses `mldoc`: OCaml + Angstrom parser combinators, compiled to JS
via `js_of_ocaml`, running on the frontend JS thread. mldoc is a **real parser to an AST**
— architecturally the *right* design. (OG is slow for *other* reasons — DataScript,
reactivity, parsing on the JS main thread — not because mldoc is crude. Tine is faster
mainly because it parses structurally in native Rust off the JS thread, **not** because
Tine's parser is better; Tine's inline parser is actually *less* principled than mldoc's.)

**Decision (Martin, 2026-06-28):** build a from-scratch Rust parser-to-AST,
mldoc-equivalent, as an independent repo. Rejected alternatives, so you don't relitigate:
- **comrak / pulldown-cmark + extend** — they implement **CommonMark**, a *different
  dialect*. They are robust toward the wrong target and would diverge from mldoc precisely
  on the strange inputs we care about; and the Logseq constructs (`((…))`, `[[…]]`, `::`
  properties, `#tags`, `{{macros}}`, org-isms) aren't CommonMark at all. Use them as
  **architectural prior art only** (see §3), not as the engine.
- **Embed / FFI real mldoc** — perfect parity for free, but drags the OCaml (or a JS)
  runtime into a native Rust app and uglifies cross-platform CI. No.

---

## 2. The goal (end state)

- A **standalone Rust crate** (no dependency on Tine) that parses Logseq **Markdown AND
  Org** into a well-typed, **serde-serializable** AST with **source spans** preserved.
- **Behavior-equivalent to mldoc at the granularity that matters to Tine** (see §5
  "oracle granularity"): the set and spans of references (page refs, block refs, tags,
  embeds, macros), block structure, and the inline constructs needed to render identically
  to OG — **not** necessarily byte-identical to mldoc's internal node identity.
- **Org-mode is explicitly in scope for the end state.** But scope **Markdown first** to a
  high bar, then Org. Do not let Org inflate the Markdown milestone.
- **Eventual consumer = Tine** (future, *out of scope for the first cut* but design for
  it): the AST crosses Tine's DTO boundary; both indexing and rendering consume the *one*
  AST; `parseInline.ts` is deleted and `refs.rs`'s inline scanning folds in. Implication
  for your design now: the AST must be cheap to serialize and send to a frontend, and
  spans/offsets must survive (used for rendering and click targets). Don't implement the
  integration; just don't paint yourself into a corner.

---

## 3. Study these first (learn good decisions; don't copy bugs)

**Tine's Rust parsers** — read for hard-won edge cases:
- `crates/tine-core/src/doc.rs` — block tree: stack-based build, indentation levels, code-
  fence detection.
- `crates/tine-core/src/refs.rs` — ref extraction; `code_ranges`/`in_code` fence-awareness;
  `read_bracket_link` paren-balancing; **`block_ref_ids` precedence**: it consumes a labeled
  link `[lbl](((uuid)))` *before* the bare-`((` scan, or the triple paren gets mangled.
  That precedence rule is hard-won — preserve the lesson.
- `crates/tine-core/src/org.rs` — org headline scanning, `#+BEGIN/#+END` depth tracking.
- `crates/tine-core/src/publish.rs` — export-side ref helpers that had to agree with
  `refs.rs` (more evidence of the dual-parser tax).
- `src/render/parseInline.ts` — the inline `Seg` model: what constructs Tine actually
  renders (emphasis, org emphasis, inline code, links/images, math, macros, footnotes,
  bare URLs with trailing-punctuation handling, timestamps).

Where Tine's current behavior diverges from mldoc, **mldoc wins** (it's the oracle) — but
Tine's edge cases (trailing-quote-in-URL, tag word boundary, fence-awareness) are real and
worth keeping as **test inputs**.

**mldoc itself** — `github.com/logseq/mldoc` (OCaml/Angstrom). Read its shape:
`lib/syntax/inline.ml` (~1454 LOC — the inline grammar), `block0.ml`, `heading0.ml`,
`lists0.ml`, `document.ml`, `parsers.ml`. Learn its AST taxonomy (the JSON it emits:
block-level `Paragraph/Heading/List/Src/Quote/Table/Properties/Drawer/Footnote_Definition/
Displayed_Math/Raw_Html/Horizontal_Rule/…`; inline `Emphasis(Bold/Italic/Underline/
Strikethrough/Highlight)/Code/Verbatim/Link(File/Page_ref/Block_ref/Embed)/Nested_link/
Tag/Timestamp/Macro/Latex_Fragment/Break_Line/Footnote_Reference/…`).

**How OG consumes mldoc** (this *defines* what "a ref" means):
`/aux/koutecky/logseq/og/deps/graph-parser/src/logseq/graph_parser/mldoc.cljs` (config +
the parse call) and `…/block.cljs` (how refs are pulled out of the AST — e.g. the embed
arg is parsed as a block ref).

**Architectural prior art (Rust, read for design — do NOT vendor):** `pulldown-cmark` and
`comrak`. Specifically their two-phase **block-then-inline** structure, their **delimiter-
stack** emphasis algorithm (the key to *linear* emphasis resolution — see §4), their span
bookkeeping, and their fuzz setups. The CommonMark spec's phased model and emphasis
algorithm are worth understanding even though our dialect differs.

You are **encouraged to do web research**: mldoc GitHub issues/discussions, the CommonMark
spec, Angstrom docs, articles on markdown-parser design and on avoiding catastrophic
backtracking.

---

## 4. The hard part — correctness is necessary, not sufficient

The oracle gives you a **correctness** signal, never a **performance** one. You can pass
every differential test while smuggling in:
- `O(n²)`: re-scanning from the start per token; recomputing code/fence ranges per inline;
  building the AST via repeated substring allocation/concatenation.
- `O(2^n)`: **catastrophic backtracking** in emphasis/delimiter resolution (recursive
  trial-and-error over nested `*`/`_`/`(`/`[`/`{`), e.g. inputs like `*a *b *c *d …` or
  long runs of `(((((…`.
- **Stack overflow**: deep nesting (`[[[[…`, deep list indentation) via unbounded
  recursion.

**Requirements (separately tested, gated):**
1. Use a **single-pass / delimiter-stack** algorithm for emphasis (the CommonMark
   approach), not recursive backtracking. Compute code/fence ranges **once per block**.
   Prefer **byte-index / span** scanning over allocating substrings.
2. Document the **complexity of each phase**. No phase worse than `O(n log n)` without a
   written justification in `DECISIONS.md`.
3. Ship an **adversarial performance suite** distinct from the correctness suite: inputs
   engineered to trigger backtracking and pathological allocation (long `*`/`_`/`(`/`[`/`{`
   runs, deep nesting, huge property blocks, very long single lines, many refs). Assert a
   wall-clock / iteration **budget** (e.g. a 10⁵-char pathological input parses in < a few
   ms; deep nesting doesn't overflow). A correctness pass is **not** enough — these gate.
4. Run a **fuzzer** (`cargo-fuzz`/libFuzzer or a `proptest`/`arbitrary` harness) that
   checks **both** (a) no panic/hang and (b) output matches the oracle. Differential
   fuzzing against the live mldoc reference is the strongest tool you have — lean on it.

---

## 5. Verification — the oracle (this is what makes autonomy credible)

**PRIMARY ORACLE — real mldoc as a differential reference.** mldoc is published to npm
(the version OG pins is **1.5.7** — confirm the exact package name from OG's
`package.json`; it resolves as `mldoc` in OG's lockfile, possibly published as `mldoc` or
`@logseq/mldoc`). It's a `js_of_ocaml` build that **runs under plain Node** and exposes
`Mldoc.parse` / `parseJson` / `parseInlineJson` returning a **JSON AST**. Loop:
1. Stand up a tiny Node harness that installs mldoc and exposes "string in → JSON AST out".
2. For the mldoc **config**, copy what OG uses (read
   `…/og/deps/graph-parser/src/logseq/graph_parser/mldoc.cljs`; format `"Markdown"` /
   `"Org"`).
3. Feed an input corpus through mldoc → **golden** JSON AST; feed the same through your
   Rust parser → your AST; **normalize both** to a common comparison projection (next
   point) and diff. Every diff is a bug in yours — until proven a mldoc quirk you
   *deliberately* don't replicate, which you log on an explicit allowlist.
4. It's **unlimited and fuzzable**: generate random + adversarial markdown, run both,
   compare. This is the gold-standard autonomous verification basis.

**ORACLE GRANULARITY (a real design decision — get it right).** Do **not** bind to
mldoc's exact internal node identity; some of its AST is legacy/quirky and you may not want
bug-for-bug parity. Define a **normalized "observable" projection** that captures what Tine
needs, and compare on *that*:
- ordered inline segments: `kind + source span + key payload` (ref target, URL, tag name,
  macro name+args, emphasis kind, code content);
- block structure: `kind, level, nesting, properties`;
- the **ref set** (page / block / tag / embed) with spans.
Where mldoc differs in ways that don't affect Tine's rendering or indexing, record an
**intentional deviation** (documented, justified, *small* allowlist) — not a test failure.

**SECONDARY ORACLES / corpora:**
- mldoc's own OCaml tests (`test/test_markdown.ml` ~1063 LOC, `test_outline_markdown.ml`
  ~886, `test_org.ml` ~294, export tests) — **not** runnable against Rust (OCaml/Alcotest-
  bound), but a rich source of **input cases** to mine for differential/golden tests.
- OG graph-parser tests: `…/og/deps/graph-parser/test/` (`mldoc_test.cljs`,
  `block_test.cljs`, `cli_test.cljs`) — what "a ref"/"a property" means in practice; one
  integration test parses a whole docs repo.
- Tine's `src/fixtures/kitchen-sink.md` — covers most constructs.
- **Real graph for realism:** `~/research/org-graph` (a real Logseq graph). Parse every
  block, diff vs mldoc. Expect ~0 divergence on real content — so any divergence there is a
  **high-value** bug.

**Bootstrap:** a spike has already prototyped (a) a Tine-internal divergence finder
(`refs.rs` vs `parseInline.ts`), (b) an adversarial input corpus, and (c) — if mldoc
installed cleanly — the mldoc-under-Node golden harness. Those assets + findings will be
dropped into `bootstrap/` in this repo. **Start by reading `bootstrap/`** — the harness is
your oracle skeleton, the corpus is your first regression set, and the Tine-internal
divergences are concrete adversarial cases the new parser must get right.

---

## 6. How to work (autonomy + milestone order)

**Infra before implementation.** Build the differential harness + normalization/comparison
layer + corpus + a one-command regression run **first**, then implement against it
continuously. Core loop: implement a construct → diff vs mldoc on the corpus → fix
divergences → add adversarial + perf tests → fuzz → fix → expand corpus → repeat until
**0 diffs (modulo the allowlist) + perf budgets hold**, then next construct.

**Milestone order** (each gated by "0 oracle diffs on its slice + perf tests pass"):
1. **Harness/oracle/corpus/normalization + CI loop.** (Infra.)
2. **Block structure:** paragraphs, headings, lists + indentation, code fences,
   properties, quotes/callouts, hr, tables.
3. **Inline core:** text; emphasis (bold/italic/strike/highlight) via **delimiter stack**;
   inline code; links/images with **paren-balanced** URLs; autolinks/bare URLs (trailing-
   punct rules); escapes.
4. **Logseq dialect inline:** `[[page]]`, `#tag` / `#[[…]]`, `((uuid))`,
   `[label](((uuid)))`, `{{macros}}` incl. `{{embed ((uuid))}}`, **block/page-ref gating to
   UUID shape**, math `$…$` / `$$…$$`, timestamps / SCHEDULED / DEADLINE.
5. **Hardening:** fuzz to convergence; perf adversarial suite; real-graph diff
   (`~/research/org-graph`); finalize the intentional-deviation allowlist.
6. **ORG MODE:** org block structure (headlines-as-blocks, `#+BEGIN/#+END`), org emphasis,
   org links/timestamps, org properties/drawers — same oracle (mldoc Org config), same
   discipline.
7. **(Future, out of scope for first cut — design for it, don't build it)** Tine
   integration: serde AST across the DTO boundary; Tine deletes `parseInline.ts` and
   re-points `refs.rs`.

**Hygiene:** `git init`; commit per construct/milestone; `README.md` (oracle + how to run
the harness + per-phase complexity); keep the crate dependency-light; serde-derive the AST
from day one; maintain **`DECISIONS.md`** logging mldoc quirks found, deviations chosen,
and perf decisions (the "why" lives here).

---

## 7. Bootstrap assets & spike findings

A divergence spike ran (2026-06-28); both prongs produced verified output (mldoc installed
fine). **All assets are in `bootstrap/` — read `bootstrap/FINDINGS.md` and
`bootstrap/README.md` before writing any parser code.** The harness (`bootstrap/harness/`)
is your oracle skeleton: a 157-input adversarial corpus generator, a vitest runner over the
real `parseInline.ts`, a cargo runner over `refs.rs`, and a **faithful mldoc oracle** that
ports OG's `block.cljs` extraction over `Mldoc.parseJson` (NOT the shallow
`Mldoc.getReferences`, which is wrong). Reference outputs + `divergences.json` are included.

**Headline results (157 inputs):** Prong A (Tine's two parsers vs each other): 27 page-ref
+ 2 block-ref divergences. Prong B (Tine vs OG/mldoc): 41 page + 4 block; of these, **16
where BOTH Tine parsers agree yet OG differs** — the prize, invisible to any Tine-internal
check.

**The bug classes the new parser must get right** (these are your first targets — they're
where the existing parsers are wrong, so they double as a spec for "don't repeat this"):
1. **Escape handling — the biggest find.** *Neither* Tine parser honors markdown `\`
   escaping. `\[[a]]`, `\#tag`, `\((uuid))` are wrongly indexed as references; an escaped
   backtick `` \` `` wrongly suppresses a real ref inside. OG honors all of these. Common in
   code/how-to notes. The new parser MUST implement `\` escaping.
2. **Tag charset + boundary** (≈11 of 27 Prong-A cases). `refs.rs` allows Unicode + `.` and
   enforces a left word-boundary; `parseInline` uses ASCII `\w/_-` with no boundary. They
   disagree on every accented tag (`#café`→`café` vs `caf`), CJK/emoji tag, dotted tag
   (`#a.b`), and word-glued tag (`c#sharp`). **`refs.rs` matches OG on accented/dotted;
   `parseInline` matches OG on word-glued.** Resolved below: match OG on **both**.
3. **Macros.** `parseInline` treats `{{…}}` as opaque; `refs.rs` scans inside. OG indexes
   refs **only** inside `{{embed …}}`, not `{{query …}}` / `{{renderer …}}`. So
   `{{embed [[Foo]]}}`→`Foo` (refs.rs right), but `{{query [[Foo]]}}`→ no ref (parseInline
   right). The new parser needs macro-aware, embed-only ref extraction.
4. **Code spans.** Different models for double-backtick inline code and fences cause refs to
   leak out of (or get wrongly swallowed by) code. Model N-backtick inline spans correctly.
5. **`[[name]]` vs `[label](url)` precedence** (`[[a](b)]]`, `[#tag](u)`) and **empty/odd
   refs** (`[[]]` currently makes an *empty-named* page ref; OG rejects it; OG also rejects
   nested-bracket `[[a[[b]]c]]`).

**✅ RESOLVED — tags & URL fragments (Martin, 2026-06-28, confirmed by an mldoc 1.5.7 probe).**
**lsdoc matches OG exactly on tags; NO word-boundary deviation.** The probe showed the two
worries are *independent* in mldoc:
- A `#fragment` inside a URL is **never** a tag, because mldoc tokenizes the whole URL as a
  single `Link` first (the `#` is consumed into the link). Verified: `https://x.com/p#frag`
  → no tag; `http://x.com/p?q=1#frag and #realtag` → only `realtag` tags; markdown-link and
  `<autolink>` targets likewise. **This — not a word boundary — is the real protection for
  the URL case**, and lsdoc tokenizes URLs anyway, so it's immune by construction.
- Glued tags ARE tagged by mldoc: `c#sharp`→`sharp`, `word#tag`→`tag`.

So lsdoc: **(a)** tokenize URLs / autolinks / link-targets first (fragments never tag);
**(b)** otherwise tag `#…` exactly like OG/mldoc, **including** glued `c#sharp` and
accented/CJK/emoji/dotted tags (`#café`, `#中文`, `#😀`, `#a.b`). **Do NOT port `refs.rs`'s
word-boundary rule** — it was a blunt URL-safety proxy (Tine decision #83) that proper URL
tokenization supersedes, and as a side effect it wrongly dropped legitimate glued tags.
(`refs.rs` was already right on accented/dotted; `parseInline` was already right on glued —
matching OG gives you both, with no deviation to maintain.)

**Oracle verdict (from the spike):** a golden-corpus + adversarially-biased fuzz loop IS a
credible autonomous regression gate — it already found ~16 real OG-parity bugs in current
Tine. But only as a *differential* oracle against a **`block.cljs`-faithful extractor**
(not `mldoc.getReferences`). The one ground-truth judgment call it surfaced — the tag
boundary — is now **resolved** (see above: match OG, URL fragments safe via tokenization),
so mldoc + `block.cljs` can serve as ground truth and the loop runs autonomously.

> Note: most of these are *current* Tine bugs too, but do **not** patch Tine's two parsers
> now — the rewrite subsumes them. (The tag-boundary question that needed a human call is
> resolved above.)

---

## 8. Definition of done (first cut, pre-Org)

- Rust crate parses Logseq **Markdown** to a serde AST with spans.
- Differential harness shows **0 diffs** (modulo a small, documented deviation allowlist)
  on: the adversarial corpus, mined mldoc/OG test inputs, kitchen-sink, and the real graph
  (`~/research/org-graph`).
- Fuzzing runs **clean** (no panic/hang/oracle-mismatch) for a sustained budget.
- Perf suite: every adversarial input within budget; per-phase complexity documented,
  nothing worse than `O(n log n)` unjustified.
- `README.md` + `DECISIONS.md` present. **Org-mode** tracked as the next milestone.

---

## 9. Path quick-reference

- **Tine** (sibling, read-only): `/aux/koutecky/logseq/logseq-claude`
  - `crates/tine-core/src/{doc,refs,org,publish,model}.rs`
  - `src/render/parseInline.ts`, `src/render/block.ts`
  - `src/fixtures/kitchen-sink.md`
- **OG (Logseq source):** `/aux/koutecky/logseq/og`
  - `deps/graph-parser/src/logseq/graph_parser/mldoc.cljs` (config + consumption)
  - `deps/graph-parser/src/logseq/graph_parser/block.cljs` (what counts as a ref)
  - `deps/graph-parser/test/` (parser tests)
- **mldoc upstream:** `github.com/logseq/mldoc` (OCaml; `test/` for input cases)
- **mldoc runnable:** npm `mldoc@1.5.7` (js_of_ocaml; runs under Node)
- **Real graph:** `~/research/org-graph`
- Note: building Tine's `tine-core` (only if you want to cross-check) needs
  `source scripts/env.sh` first — a Tine quirk; your crate is standalone.

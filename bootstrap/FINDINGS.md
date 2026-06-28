# Parser-divergence spike — findings (Tine)

Date: 2026-06-28. Both prongs RAN and produced verified output (mldoc installed OK).

## What was built (reusable, all in scratchpad — repo tree left clean)

`scratchpad/parser-divergence/`
- `corpus.gen.mjs` → `corpus.json` — 157 adversarial inputs across 12 categories
  (brackets 17, parens 17, code 18, link 14, escape 6, emph 11, prop 7, tag 29,
  url 9, macro 11, unicode 10, misc 8).
- `ts-runner/` — standalone vitest project (plain-object config, `node_modules`
  symlinked to the repo). `runner.test.ts` imports the REAL
  `/aux/.../src/render/parseInline.ts` by absolute path, runs it over the corpus,
  recursively collects `pageref`+`tag` (page), `blockref` (block), `macro` bodies,
  writes `ts-out.json`. Run: `node_modules/.bin/vitest run --config ts-runner/vitest.config.ts`
  from the repo root (or with abs path to the repo's vitest binary).
- `rust-runner/` — standalone cargo project, `tine-core = { path = … }`. Calls
  `refs::page_refs`, `refs::block_refs`, `refs::block_ref_ids`; writes `rust-out.json`.
  Run: `source scripts/env.sh && cargo run` in `rust-runner/`.
- `mldoc/` — throwaway npm project (`mldoc@1.5.7`). `mldoc-runner.mjs` ports OG's
  `block.cljs` ref-extraction over `Mldoc.parseJson` (NOT the shallow
  `getReferences`), writes `mldoc-out.json` (`og_page`, `og_block`).
- `compare.mjs` → `divergences.json` + console tables. Prong A (Rust vs TS) and
  Prong B (each vs OG/mldoc, classified).

All three runners compile/run; numbers below are real output, not hypothetical.

## Headline numbers

| Comparison | Divergent inputs (of 157) |
|---|---|
| Prong A — page refs: refs.rs vs parseInline | **27** |
| Prong A — block refs (uuid-gated): block_ref_ids vs blockref | **2** |
| Prong B — page refs: Tine vs OG(mldoc) | **41** |
| Prong B — block refs: Tine vs OG(mldoc) | **4** |
| Prong B — **BOTH Tine parsers agree but OG differs** (page) | **14** |
| Prong B — BOTH Tine parsers agree but OG differs (block) | **2** |

The 16 "both-agree-but-wrong" cases are the prize: Prong A's dual-parser cross-check
can NEVER surface them (no internal disagreement), yet they silently diverge from OG.

---

## Prong A — refs.rs vs parseInline (the dual-parser bug class)

Page-ref divergences, ranked most-innocent first (looks like normal content → a
user would never suspect it; the bug hides in plain sight).

| Rank | id | input | refs.rs (Rust) | parseInline (TS) | why it's innocent |
|--|--|--|--|--|--|
| 1 | c094 | `#café` | `café` | `caf` | accented tag — TS `\w` stops at `é`, truncates the page name |
| 2 | c117 | `#naïve` | `naïve` | `na` | same; TS silently points at the wrong page `na` |
| 3 | c093 | `#中文` | `中文` | *(none)* | CJK tag — TS emits no tag at all |
| 4 | c144 | `#étag` | `étag` | *(none)* | leading accent — TS drops the whole tag |
| 5 | c097 | `#tag.foo` | `tag.foo` | `tag` | dotted tag (`#v1.2`, `#a.b`) — TS truncates at `.` |
| 6 | c099 | `#a.b.c` | `a.b.c` | `a` | same |
| 7 | c090 | `#t.` | `t.` | `t` | trailing dot kept by Rust, dropped by TS |
| 8 | c118 | `pre #tag.next` | `tag.next` | `tag` | same, mid-sentence |
| 9 | c100 | `word#tag` | *(none)* | `tag` | tag glued to a word — TS tags it, Rust's word-boundary rule rejects it |
| 10 | c102 | `c#sharp` | *(none)* | `sharp` | classic `C#`/`F#` case |
| 11 | c101 | `a#b` | *(none)* | `b` | same |
| 12 | c113 | `email#fragment` | *(none)* | `fragment` | same |
| 13 | c132 | `{{embed [[Foo]]}}` | `Foo` | *(none)* | the everyday embed macro — Rust mines the page ref, TS treats it as an opaque macro |
| 14 | c131 | `{{query [[Foo]]}}` | `Foo` | *(none)* | query macro with a page ref inside |
| 15 | c136 | `{{renderer :x, [[Foo]]}}` | `Foo` | *(none)* | renderer macro |
| 16 | c037 | `` ``[[Foo]]`` `` | *(none)* | `Foo` | double-backtick inline code — TS treats `` `` `` as empty code, then parses `[[Foo]]` as a live ref (leaks out of code); Rust is N-backtick-aware |
| 17 | c051 | `` `one` two ``[[Foo]]`` end `` | *(none)* | `Foo` | same, mid-line |
| 18 | c038 | `` ``a [[Foo]] b`` `` | *(none)* | `Foo` | same |
| 19 | c009 | `[[Foo](bar)]]` | `Foo](bar)` | *(none)* | a `[[name]]` whose name contains `](` — TS's `[label](url)` link rule hijacks it (emits a link, no page ref); Rust keeps the outer `[[…]]` |
| 20 | c011 | `[[a](b)]] and [[c]]` | `a](b)`,`c` | `c` | same; TS loses the first ref |
| 21 | c059 | `[#tag](u)` | `tag` | *(none)* | `#tag` inside a link label — Rust scans raw and tags it; TS consumes the whole link |
| 22 | c077 | `_#tag_` | *(none)* | `tag` | `_` is a tag-char so Rust's boundary rejects; TS parses italic then the tag |
| 23 | c048 | `` ```\ntext [[Foo]] `` | *(none)* | `Foo` | UNCLOSED fence — Rust treats rest-of-text as code; TS (inline-only) parses the ref |
| 24 | c049 | `` ```js\n[[A]] and #B `` | *(none)* | `A`,`B` | same |
| 25 | c050 | closed+stray fences | `B` | `B`,`C` | odd fence count; TS's backtick-pairing differs from Rust's fence tracking |
| 26 | c092 | `#t#t` | `t` | `t`,`t` | second `#t` follows a tag-char, Rust rejects; TS tags both |
| 27 | c000 | `[[a[[b]]c]]` | `a[[b` | `a[[b` | (agree — listed for completeness; both take the first `]]`) |

Block-ref divergences (uuid-gated, block_ref_ids vs blockref): only **2**, both the
embed macro (`{{embed ((uuid))}}` c128, c133) — Rust mines the uuid, TS emits a
macro seg and never sees a block ref.

### Root causes (the recurring asymmetries)
- **Tag character set + boundary.** refs.rs `is_tag_char` = `alphanumeric | -_/.` with
  Unicode `is_alphanumeric()` AND a left word-boundary check. parseInline `RE_TAG =
  /#([\w/_-]+)/` — ASCII-only `\w`, NO `.`, NO boundary check. ⇒ disagree on every
  accented/CJK tag, every dotted tag, and every word-glued tag. (≈11 of 27.)
- **Macros are opaque to TS but transparent to Rust.** parseInline consumes
  `{{…}}` whole; refs.rs has no macro concept and scans inside. ⇒ any `[[ref]]`/
  `((uuid))` inside a macro diverges.
- **Code-span models differ.** refs.rs models N-backtick inline spans AND multi-line
  fences. parseInline only matches single-backtick inline code and has NO fence
  awareness (it is an *inline* parser; the block layer separates fenced code — so
  the multi-line cases here are partly architectural, see caveat). ⇒ double-backtick
  and unclosed-fence leaks.
- **`[label](url)` vs `[[name]]` precedence.** parseInline tries the `[..](..)` link
  rule before `[[..]]`; refs.rs has no link rule in `page_refs`. ⇒ `[[name](x)]]`
  and `[#tag](u)` diverge.

---

## Prong B — Tine vs OG (mldoc 1.5.7), classified

Oracle = port of OG `graph-parser/block.cljs` over `Mldoc.parseJson` (see oracle
verdict for why the convenience `getReferences` is the WRONG thing to copy).

### A. BOTH Tine parsers agree, OG disagrees → silent OG-parity bugs (14 page + 2 block)
These are invisible to Prong A and to any Tine-internal cross-check.

| id | input | Tine (both) | OG | class |
|--|--|--|--|--|
| c066 | `\[[a]]` | `a` | *(none)* | **escape ignored** — Tine indexes an escaped link |
| c068 | `\#tag` | `tag` | *(none)* | **escape ignored** |
| c069 | `a \[[b]] c` | `b` | *(none)* | **escape ignored** |
| c067 | `\((uuid))` (block) | uuid | *(none)* | **escape ignored** (block ref) |
| c071 | `` \`[[a]]\` `` | *(none)* | `a` | **escaped backtick** — Tine treats `` ` `` as code, OG honors `\`` so `[[a]]` is a real ref |
| c004 | `[[]]` | `""` (empty!) | *(none)* | Tine creates an **empty-named page ref** |
| c013 | `[[a\nb]]` | `a\nb` | *(none)* | Tine lets a page ref span a newline; OG doesn't |
| c000 | `[[a[[b]]c]]` | `a[[b` | *(none)* | OG rejects nested-bracket page refs entirely |
| c055 | `![[[Foo]]](u)` | `[Foo` | *(none)* | adversarial image/link nest |
| c022 | `[label]:((uuid))` (block) | uuid | *(none)* | OG reads `[label]:` as a link-ref-definition → whole line Plain |
| c079 | `==#tag==` | `tag` | *(none)* | OG does NOT tokenize `#tag` inside emphasis/highlight (it DOES keep `[[..]]`) |
| c105 | `(#tag)` | `tag` | `tag)` | OG's tag is greedy and **includes the `)`**; Tine strips it |
| c110 | `#[[unclosed` | *(none)* | `[[unclosed` | OG tags the unterminated `#[[…`; Tine requires `]]` |
| c116 | `x#中文` | *(none)* | `中文` | word-glued + CJK — both Tine miss it (diff reasons), OG tags it |
| c142 | `#tag​suffix` | `tag` | `tag​suffix` | OG treats zero-width space as a tag char |
| c147 | `#😀` | *(none)* | `😀` | OG allows emoji tags; Tine doesn't |

The **escape class (c066–c069, c071, c067)** is the most important: neither Tine
parser honors markdown `\` escaping, so escaped `\[[a]]` / `\#tag` / `\((uuid))` are
wrongly indexed as references (and an escaped-backtick ref is wrongly suppressed).
Common in code-heavy / how-to notes. Cheap to suspect once you know; impossible to
catch by comparing Tine-to-Tine.

### B. Parsers disagree AND one matches OG (tells you which one to trust)
- **RUST = OG, TS wrong (15):** all accented/dotted tags (`#café`→`café`, `#naïve`,
  `#tag.foo`, `#a.b.c`, `#étag`, `pre #tag.next`), the `[[name](x)]]` cases, the
  double-backtick code leaks, and the embed macros (`{{embed [[Foo]]}}`→`Foo`).
  ⇒ For these, **parseInline is the OG-parity bug** (truncates/drops refs, leaks
  refs out of code, misses embed refs). Block refs: c128/c133 embed → RUST=OG.
- **TS = OG, RUST wrong (12):** the word-boundary tags (`word#tag`, `c#sharp`,
  `a#b`, `email#fragment`, `#t#t`, `#t.`→`t`), the unclosed-fence cases,
  `[#tag](u)`, and the non-embed macros (`{{query …}}`, `{{renderer …}}` → no ref).
  ⇒ For these, **refs.rs is the OG-parity bug**.

The single biggest RUST-vs-OG conflict: refs.rs's **tag word-boundary rule**
(`tag_boundary`, used to reject `word#tag`/`c#sharp`) is contradicted by mldoc
1.5.7 + OG `block.cljs:get-tag`, which accept `word#tag` as a tag. refs.rs's own
comment ("`word#x` … are NOT tags — matching OG") and the `hash_needs_a_word_boundary`
test assert the opposite of what mldoc does. ONE of them is wrong about OG. ⚠️ This
needs a real-OG spot check (graph-parser indexing vs editor rendering may differ);
flagged, not auto-trusted. Note: refs.rs is ALSO right that block refs are
UUID-gated (OG `get-block-reference` has a `parse-uuid` gate) — raw mldoc Block_ref
keeps non-UUIDs, so a naive mldoc oracle would falsely flag refs.rs there.

---

## Oracle verdict — is a golden-corpus + fuzz loop a credible *autonomous*
## verification basis for a from-scratch Rust mldoc-equivalent?

**Standing mldoc up was easy; using it correctly as an oracle was NOT.**

1. **Install/API: trivial.** `npm install mldoc@1.5.7` succeeded offline-friendly
   (56 pkgs, ~3s). `require("mldoc").Mldoc` exposes `parseJson`, `parseInlineJson`,
   `getReferences`. OG's config (`mldoc.cljc default-config`) is a 7-key JSON blob;
   copied verbatim. Both `parseJson` and `getReferences` return JSON **strings**
   (must `JSON.parse`), `parseInlineJson` returns a live array — a small gotcha.

2. **The oracle is NOT one mldoc call — it's a layered pipeline, and the obvious
   call is the wrong one.** Three traps, each of which would have produced false
   divergences if trusted blindly:
   - `Mldoc.getReferences` (the function literally named for this) is **shallow**:
     it misses refs inside emphasis (`*[[Foo]]*` → `[]`) and **over-reports** macro
     refs (returns `Foo` from `{{query [[Foo]]}}` and `{{renderer …}}`, which OG
     does NOT index). It is NOT what OG uses for `:block/refs`.
   - The faithful path is **OG's `block.cljs` walking `parseJson`**: page refs from
     `Link Page_ref` + `Tag` (`get-tag`) + **`embed`-macro args only**; block refs
     from `Link Block_ref` + embed macro, **both `parse-uuid`-gated**. You must port
     ~60 lines of Clojure (get-page-reference / get-block-reference / get-tag) to
     get the reference set OG actually stores.
   - mldoc's AST and OG's graph layer **disagree with each other**: mldoc emits
     `Block_ref` for `((not-a-uuid))`; OG drops it. So "match mldoc" ≠ "match OG".

3. **Verdict: credible, but only as a *differential* oracle with a hand-curated
   adversarial corpus and a faithful block.cljs port — NOT as a thin "diff against
   mldoc.getReferences" loop.** Concretely:
   - **Feasible & high-value:** a fixed golden corpus (this one, grown over time)
     run through `parseJson` + a block.cljs-faithful extractor, compared to the
     Rust rewrite, IS a credible autonomous gate. It already found ~16 real
     OG-parity bugs in the *current* Tine that no Tine-internal check could.
   - **Random fuzzing alone is weak** for refs: ~0 divergence on normal text; all
     the signal is in a narrow adversarial band (escapes, tag boundaries/charset,
     code/macro nesting, bracket/paren pathologies). A generator biased toward
     those categories (like `corpus.gen.mjs`) is what produces signal. Pure random
     ASCII would mostly miss them.
   - **Caveats that block full autonomy:** (a) mldoc ≠ OG at the block-ref UUID gate
     and macro layer — the oracle must encode OG's post-mldoc rules, not mldoc raw;
     (b) `parseInline` is an *inline* parser and Tine's block layer handles fenced
     code separately, so multi-line fence "divergences" must be compared at the
     right layer or they're false positives; (c) mldoc tag tokenization (Unicode,
     word-glued, trailing-`)` greediness) contradicts refs.rs's stated OG-intent —
     a human must adjudicate the tag-boundary question against the real OG app
     before encoding it as ground truth. Net: the loop is a strong bug-finder and
     regression gate, but "ground truth" needs one human calibration pass on the
     tag-boundary + macro classes, after which it can run autonomously.

## Reproduce
```
cd scratchpad/parser-divergence
node corpus.gen.mjs
( source /aux/koutecky/logseq/logseq-claude/scripts/env.sh && cd rust-runner && cargo run -q )
/aux/koutecky/logseq/logseq-claude/node_modules/.bin/vitest run --config ./ts-runner/vitest.config.ts
( cd mldoc && node mldoc-runner.mjs )
node compare.mjs           # prints all tables; writes divergences.json
```

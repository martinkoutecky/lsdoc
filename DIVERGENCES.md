# Known byte-exact divergences from mldoc@1.5.7 — status

Cases where lsdoc's AST differs from mldoc's. All were pre-existing (verified — none introduced by
the container-frame rewrite / O(n) audit; the audit merely *surfaced* them). **Fixing each LOWERS
the fuzz floor** (more mldoc parity) — how each fix was verified.

## Status (Jul 2 2026)
| # | divergence | status | commit |
|---|---|---|---|
| D1 | trailing bare `>` at EOF → paragraph | **FIXED** | `46baefa` |
| D2 | def-list-after-para keeps the break (block content) | **FIXED** | `43c8c6d` |
| D3 | count `[:` inside hiccup strings (naive quote escape) | **FIXED** | `413f996` |
| D4 | CR→LF in inline literal-break + code text | **FIXED** | `c26b8ca` |
| D5 | multi-line `$$…$$` → display-math block | **FIXED** | `912b1bd` |
| D6 | multi-line HTML → raw_html block | **FIXED** | `2fa647a` |
| D7 | org `[[url][label]]` label spans a newline | **FIXED** | `65df867` |
| D8 | org `^{…}`/`_{…}` script non-space fallback | **FIXED** | `65df867` |
| D9 | block-ref `((…))` content spans a newline | **FIXED** | `65df867` |
| D10 | inline_html accepts UNKNOWN tags (mldoc → plain) | **FIXED** | `b85f4d7` |
| D11 | `<br/>` no-space → inline_html (mldoc → plain) | **FIXED** | `b85f4d7` |
| D12 | single-line `<b>`/`<i>` phrasing tags → raw_html (mldoc → plain) | **FIXED** | `b85f4d7` |
| D13 | md link-label doesn't reparse entities/latex (`[\alpha](u)`, `[$x$](u)`) | **FIXED** | `2c77af8` |
| D14 | timestamp token order-permissive (`<… +1d 12:00>` accepts both vs mldoc date-only) | **FIXED** | `187ecbf` |
| D15 | md drawer name rejects punctuation (`:LOG@BOOK:` → paragraph vs mldoc drawer) | **FIXED** | `931a2a5` |
| D16 | email requires closing `>` (`<a@b.co` → plain; mldoc → email, `<`/`>` both optional per `syntax/email_address.ml:33-34`; ditto `<a@b co>` → email `a@b` + plain) | **FIXED** | `86d4d33` |
| D17 | md `data:` image parses as Search instead of `Embed_data` | **FIXED** | `2c77af8` |
| D18 | org `[[u][a]b]]` treats single `]` as terminator instead of label text | **FIXED** | `2c77af8` |
| D19 | emphasis close-guard: 1.5.7 artifact never closes right after an ABSORBED marker (`*a **` → It("a ")+`*`, not It("a *")); provenance-tracked back-off to the run start; published source (1.5.5==1.5.8) lacks the guard — oracle wins | **FIXED** | `30d0842` |
| D20 | inline displayed `$$…$$` body allows lone `$` + `\$` escape (mldoc: `take_while(∉{$,CR,LF})` then literal `$$` or fail → plain-`$` fallback: `$$$a$$` → `$`+Displayed("a"), `$$a$b$$` → `$`+Inline("a")+plain; `inline.ml:534-541`) | **FIXED** | `d32a88b` |
| D21 | md property value kept raw after one `":: "` space (mldoc: `Parsers.spaces` skip {sp,tab,SUB,FF} + `String.trim` both ends {sp,tab,LF,CR,FF}; only_key `k::`+spaces+EOL → `""`; `markdown_property.ml`) | **FIXED** | `d32a88b` |
| D22 | org block phase: `#+X: v` classification family — Directive rejects `BEGIN_` ci (ARTIFACT: source is case-sensitive — oracle wins), Drawer.parse2 `#+NoSpaceName: v` → Property_Drawer, many1 fold (blank-line-tolerant, `:END:` spill re-entry, `:end:`-key = closer), heading0.ml title lookahead (Drawer-not-Directive) | **FIXED** | `5c8ea6a` |
| D23 | macro plain args trimmed (mldoc: greedy `take_while1(≠',')` keeps trailing spaces — `{{embed ] }}` → `["] "]`; empty/space-only slots invalidate the macro; `inline.ml:977-1021`) | **FIXED** | `7d835c3` |
| D24 | org hard-break `\`+EOL emitted extra Break (mldoc `string "\\" <* eol` consumes ONE EOL byte — CRLF keeps the `\n` break; `inline.ml:456`) | **FIXED** | `7d835c3` |
| D25 | org heading tags accepted empty segments (mldoc: `:seg(:seg)*:` consume-all — interior `::` kills ALL tags, all-empty `::` = empty tags + title rewrite; `heading0.ml:79-82`) | **FIXED** | `7d835c3` |
| D26 | org non-braced `^`-script at `\`+EOL — floor row 10 was reclassified: it was D24 (hardbreak) + D22 (directive) compounds; the minimal script case matches | **CLOSED (not a divergence)** | — |
| D28 | org `#+NAME: v` (Drawer.parse2) property values ref-scanned — mldoc hardcodes refs `[]` for parse2 entries, per-entry provenance within a folded drawer (`drawer.ml:74`); also made `vdiff_iso` refs-aware (was blocks-only) | **FIXED** | `76df562` |
| D29 | md block-ref `((…))` inner text unescaped (mldoc: verbatim `String.sub` slice — `\`` kept; `inline.ml` block_reference) | **FIXED** | `a53d35f` |
| D30 | md `<quick_link>` kept `\X` in label/url (1.5.7 ARTIFACT unescapes label + url.link, keeps full_text + protocol raw; published quick_link_aux can't produce label≠full — oracle wins; org quick links stay raw) | **FIXED** | `a53d35f` |
| D31 | org `[[url][label]]` with `:`-target → Search (mldoc Scanf `%[^:]:%[^\n]`: EMPTY protocol ok, link truncated at LF, `//` stripped, file-first, Search only when no colon; `inline.ml:647-664`) | **FIXED** | `a53d35f` |
| D32 | org hash-tag captured `[[…]]` across a newline (mldoc tag capture is EOL-bounded via `page_ref`'s `non_eol`; top-level link targets/labels MAY span newlines; script bodies never parse links) | **FIXED** | `a53d35f` |
| D33 | ci-matched raw-html closes case-normalized to the opener's token (match_tag rebuilds with canonical `</tag>` — intermediate closes too, attr-value quirk, `/>` fallback verbatim; block raw/view + inline_html). Was mis-filed as a second "D9" during the Jul 2026 raw-html index audit | **FIXED** | `cf3d2be` |
| D34 | `#+BEGIN_X` frame-body re-parse parity: peek-10 misses the virtual final `\n` (both gates), post-close remainder wrongly indent-stripped, `strip_view` lacks mldoc's `safe_sub` exact-indent no-op (all-ws line == indent survives verbatim), spurious empty para on blank first body line. The 9 viewcap probes + 5 new shapes | **FIXED** | `ea0cb31` |
| D35 | NESTED frames × all-ws line: mldoc clears indents SEQUENTIALLY per frame level, lsdoc folds strips cumulatively — `safe_sub`'s no-op breaks the composition lemma (`strip_view(t,A+B) ≠ strip_view(strip_view(t,A),B)` for all-ws `t`, e.g. 3-ws line under strips 2+1 → mldoc `" "`, lsdoc `"   "`). Only all-ws lines in ≥2 nested indent-bearing frames diverge (non-ws lines compose exactly). Pre-existing (pre-D34 lsdoc emptied such lines — mldoc NEVER empties an all-ws line), fuzz-unreachable. Probes `harness/d35-probe.json` (x01/x02, x03 = single-frame control OK). Fixing exactly needs sequential per-frame semantics for all-ws views — an O(depth)-per-line walk is Θ(n²)-able (frames ≈11 bytes each, ws lines ≈1 byte), so an O(n)-preserving design (NSE-style jump structure / piecewise map / bounded fallback) needs a decision. Design round verdict (`subagent-tasks/notes/d35-design-review.md`): BOTH candidate designs refuted (rollback-sibling + query-chain adversaries; piece-count 2^k family); no O(n) design known, no lower bound either; exact log-factor fallback exists (min segment tree, `O((frames + all-ws bytes)·log depth)`) — Martin approved the exact log fallback (α): StripSeqTree min segment tree, `O((frames + all-ws bytes)·log depth)`, the THIRD sanctioned exception (LINEARITY.md + CLAUDE.md) | **FIXED** | `13a77a7` |
| D37 | frame indent derivation: an ALL-space/tab FIRST body line makes mldoc `get_indent` return 0 (the try/with never raises — `prelude.ml:199`) ⇒ NO stripping for the whole body; lsdoc derives indent = leading_ws = len and strips (`#+BEGIN_QUOTE\n  \n  a\n#+END_QUOTE`: mldoc para `  a`, lsdoc `a`). Pre-existing (confirmed on pre-D35 HEAD). Also predicted at `block_code_texts` (`indent = leading_ws(texts[0])` — same quirk for SRC/EXAMPLE, verify). Probes s02/s03 in `harness/d35-verify-my-probe.json` + `harness/d37-probe.json` (14 rows) | **FIXED** | `5f6c836` |
| D36 | clear-indents whitespace-SET mismatch: mldoc's branch test uses `ltrim` over `{' ','\f','\n','\r','\t'}` then `safe_sub` strips `indent` BYTES blindly (a `\f`-led ws line under an indented frame: mldoc `"\f  "` → `" "` — the `\f` itself is stripped); lsdoc's `strip_view` gates branch 1 on space/tab `leading_ws` and branch 2 on Unicode `trim()`. Depth-1 reachable (ff01 in `harness/d35-wsset-probe.json`); VT/NBSP unaffected (not in mldoc's set). Found by the D35 design round's side probe | **FIXED** | `9f8899a` |

| D38 | md hard-break rule is DISPATCH-scoped, not "2 trailing ws": mldoc fires `Markdown_line_breaks.parse` only from the `' '` peek-arm (`inline.ml:1372`), counting `ws = take_while1 {' ','\t','\x1a','\f'}` ≥ 2 to eol; tab/FF-led runs are consumed whole by the plain/ws fallback (no inner dispatch) while leading `\x1a`s are absorbed into the word. lsdoc used trailing `{' ','\t'}` ≥ 2 — wrong in BOTH directions (`"x\t \n"` → falsely hardbreak; `"x \f\n"` → falsely plain). Found by the close-out ENUMERATION (427,814 frame-body cases: 64,010 diffs = 3 classes = this ONE mechanism at depths 1-3; org 100% clean; report `harness/_enum-report.json`); top-level reachable both directions. Transcription-first per Martin's directive; the effective rule survived adversarial spec-check refutation; CRLF/lone-CR eol + EOF-plain rows oracle-pinned; acceptance = enumeration 427,814 → 0 (verified twice) | **FIXED** | `741dcf4` |
| D39 | UMBRELLA (triage DONE Jul 3 via the block-opener TRANSCRIPTION AUDIT `subagent-tasks/notes/block-opener-transcription.md` + oracle confirmation `harness/d39-transcription-probe.json`, 38 probes / 31 REAL): the extended-fuzz findings (`harness/d39-fuzz-findings.txt`) decompose into D40 (SUB/FF ws-set opener class — covers ALL the org fuzz findings) + D41 (md front matter) + D42 (md footnote-def body rule) + a REMAINING md-inline family NOT explained by block openers: `***\\\`***` emphasis backslash-code (mldoc keeps `` ` ``, lsdoc `` \` ``) and `[-\\\\\`](url)` link-label (mldoc plain, lsdoc code) — needs an inline transcription pass (emphasis body + link-label reparse). Oracle REFUTED 5 audit predictions on their minimal inputs: `#+RESULTS:` both formats (likely post-1.5.7 in the ~1.5.9 checkout), md HTML comments, CR-run eols (`a\r\r# b`), SUB-before-fence-closer, deflist-SUB — do NOT fix these against the checkout | **OPEN (md-inline residue only)** | — |
| D40 | SUB/FF whitespace-SET class at BLOCK OPENERS (one mechanism, ~20 sites, BOTH formats): mldoc `spaces`/`ws` skip `{' ','\t','\x1a','\f'}` (`parsers.ml:8`; `is_tab_or_space` is aliased to `is_space`) at every `between_eols`-wrapped opener + list indent/marker-sep, heading post-marker = `whitespace_chars` (FF yes SUB NO), fence CLOSERS = OCaml `String.trim` set (FF yes SUB no), list continuation = PEEK-not-consume (folded SUB stays in content), plus the propdrawer body-key rule (tab is a legal key byte) — while lsdoc gated on space/tab `leading_ws` or Rust-trim. Spec-check round added 6 sites + re-bucketed 2 probes + corrected 3 stale audit-table rows (RESULTS + md HTML comments = no divergence, both sides directive/raw_html). Acceptance: 29 probes (`harness/d39-transcription-probe.json`) + 11 (`harness/d40x-probe.json`) all flipped, 8 guards stayed OK, my 20 fresh adversarial probes (`harness/d40-verify-my-probe.json`, mixed SUB+FF runs, sites×bytes outside both files) all match. **Org EXTENDED-vocab fuzz floor → 0/0 (seeds 99/31337/271828)**; md extended floor 65 = unchanged D39 inline residue. Fix = shared `mldoc_is_space`/`mldoc_trim_spaces*`/`ocaml_trim*`/`mldoc_heading_boundary` helper family in `block_common.rs`, routed per-site with each site's EXACT set | **FIXED** | `6d91a7f` |
| D41 | md FRONT MATTER not implemented: at absolute parser start `---\n…\n---` parses as `key: value` directives in mldoc (`markdown_front_matter.ml:24-30`, no leading-ws tolerance, tried before the whole document `many`); lsdoc parses the fences as HR. Oracle-confirmed (`d39p-md-frontmatter`) on default config | **OPEN** | — |
| D42 | md footnote-def body rule: `[^1]: a` — mldoc's shared `footnote_definition` requires each body line to have ≥2 bytes after the leading-ws skip (`footnote.ml:57-73` via `line` after one non-special byte), so a 1-byte body fails → paragraph; lsdoc's `footnote_def` accepts any nonempty body. Oracle-confirmed (`d39p-md-fndef-short`). Full body/continuation rules need the transcription treatment when fixing | **OPEN** | — |

**FLOOR = 0 (Jul 2 2026):** after D19–D32, `node fuzz.mjs 40000 <seed>` (+` org`) is **0/0 (blocks AND
refs) on every tested seed** (99, 7, 42, 12345, 31337, 271828, 2718, 555555 × both formats). The fuzz
floor is now an INVARIANT: any nonzero fuzz result on any seed is either a REGRESSION or a NEW
divergence — file a D-entry, never a ratchet.

D16 surfaced during Phase B verification (pre-existing — fuzz floors held exactly across the perf-only
change); fix belongs to the `<`-family construct port (inline-restructure-SPEC Phase C4).

D14–D15 were surfaced by the **lsdoc-vs-mldoc structural audit** (`subagent-tasks/notes/lsdoc-vs-mldoc-audit.md`,
Jul 1) — behavioral drifts confirmed vs the isolated oracle (D14 verified; D15 codex-probed). D13 was fixed by
the C2 links port, together with D17/D18 from `subagent-tasks/constructs/links-spec.md` rev 2. That
report also lists structural UNIFICATION opportunities (raw-html one-parser, hiccup quote-parity port,
list/display-math/bracket-scan dedup) — those are refactors awaiting Martin's approval, not divergences.

D10–D12 were fixed by routing block and inline raw-HTML dispatch through one source-faithful
`Raw_html.parse` port. D3/D4 were RE-AUDITED faithful vs source.
Verify any divergence probe with `node harness/vdiff_iso.mjs` (ISOLATED — mldoc leaks batched state).
The prose entries below are the original diagnoses (D1–D9 now fixed; their commits have the exact ports).

Status legend: `OPEN` = diagnosed, not yet fixed. Each has a root cause in real code and a fix
direction; none is blocked on understanding.

---

## D1 — a TRAILING bare `>` is absorbed into the quote instead of split off

**Trigger:** a `>`-blockquote whose LAST line is a bare `>` (or `> ` with only trailing space),
with nothing after it. **Both formats.**

```
input:  "> a\n>"            (also "> a\n> ")
mldoc:  Quote[ Para["a", break] ] , Para[">"]        ← the trailing `>` is a SEPARATE paragraph
lsdoc:  Quote[ Para["a", break, break] ]             ← the trailing `>` absorbed as an extra break
```

**Precise scope (verified):** ONLY the *trailing* position diverges. A *middle* `>`-blank
(`"> a\n>\n> b"`) is **byte-exact** (both keep it inside the quote). So it is specifically "a bare
`>` that is the last line of the run."

**Root cause:** `quote_line_content_slice(">")` (org) / `md_quote_cont_slice(">")` returns
`Some("")` — a bare-`>` line is treated as an empty-content *continuation* of the quote (lazy),
contributing an extra `break`. mldoc instead ends the quote at a trailing bare `>` and re-emits
that `>` as its own paragraph. The lazy-continuation predicate doesn't distinguish a *trailing*
bare `>` (nothing meaningful follows) from a *mid-run* one.

**Reachability:** a quote a user ends with a stray `>` line. Unusual but not adversarial-only.

**Fix direction:** in the quote close / prefix-consume, a bare-`>` line that turns out to be the
LAST line of the run must terminate the run and emit the `>` as a paragraph, rather than absorb it
(needs the "is anything meaningful after this?" distinction). MEDIUM (touches the close boundary).

Status: **OPEN.**

---

## D2 — a def-list preceded by a paragraph line drops the paragraph's trailing break (MD, block content)

**Trigger:** a def-list (`term` / `: definition`) **preceded by a paragraph line**, **inside block
content** (a `>`-quote or a `#+BEGIN_X` callout), in **Markdown**.

```
input:  "> intro\n> term\n> : def\n"    (also the callout form "#+BEGIN_QUOTE\nintro\nterm\n: def\n#+END_QUOTE")
mldoc:  Quote[ Para["intro", break] , List[<def-item>] ]     ← keeps the "intro" para's trailing break
lsdoc:  Quote[ Para["intro"]        , List[<def-item>] ]     ← drops it
```

**Precise scope (verified):** **MD-only** — the identical ORG input (`"> intro\n> term\n> : def"`
org) is **byte-exact**. And it needs the preceding paragraph + block content: the top-level form
(`"term\n: def"`) and the bare-in-quote form (`"> term\n> : def"`, no preceding para) are both
byte-exact. So the trigger is narrow: MD + block content + def-list-after-a-paragraph.

**Root cause:** MD's def-list step (step 11d) does `flush_para(trim = in_block_content)` before
`build_def_list`; in block content that trims the preceding paragraph's trailing `break`. mldoc
keeps it (the def-list *term* is pulled from the running paragraph, so the break is internal to
it). Org's def-list path does not trim here, which is why org matches. So this is an MD-vs-org
inconsistency in the `flush_para` trim policy at the def-list seam.

**Reachability:** a def-list written right under a paragraph inside a quote/callout, in a `.md`
graph. Plausible in real notes.

**Fix direction:** MD's step-11d should not trim the preceding paragraph's break when the def-list
term comes from a running paragraph in block content (match org's non-trimming behavior at this
seam). SMALL–MEDIUM, localized to the md def-list path; verify it doesn't disturb the other
`between_eols` cases. Status: **OPEN.**

---

## D3 — a `[:` opener INSIDE a hiccup string is ignored (→ hiccup) instead of counted (→ paragraph)

**Trigger:** a block or inline hiccup with a `[:` **inside a `"…"` string**. **Both formats.**

```
input:  "[:a \"[:x\" ]"
mldoc:  Para["[:a \"[:x\" ]"]        ← counts the inner `[:` → unbalanced → NOT a hiccup → paragraph
lsdoc:  Hiccup(v="[:a \"[:x\" ]")    ← treats the string as fully opaque → balanced → hiccup
```

**Precise scope (verified):** specifically a `[:` *opener* inside a string. A `]` inside a string
(`"[:a \"]\" x]"`) is **byte-exact** (both correctly treat the string as opaque *for `]`*). So the
one difference is whether a `[:` inside a string increments hiccup depth.

**Root cause:** lsdoc's hiccup bracket-matcher (`inline.rs::build_hiccup_close`, and formerly the
now-removed `parse_hiccup`) skips a `"…"` string **entirely** — opaque for both `[:` and `]`.
mldoc's matcher treats a string as opaque **only for `]`**, still counting a `[:` inside it. So a
`[:` in a string: lsdoc ignores it (stays balanced), mldoc counts it (goes unbalanced → paragraph.)
The scanner is **shared**, so this affects both the block-hiccup path (via Phase B's precompute)
and the inline-hiccup path identically.

**Reachability:** a hiccup literally containing `[:` inside a quoted string. Very unusual.

**Fix direction:** in `build_hiccup_close`'s string-skip, keep skipping for `]` but still scan for
`[:` (count openers inside strings). SMALL, localized to `inline.rs`; re-run the block + inline
hiccup gates and confirm the fuzz floors improve. Status: **OPEN.**

---

## D4 — a `\r\n` inside a DOUBLE-backtick code span is kept raw instead of normalized

**Trigger:** a double-backtick code span (``` ``…`` ```) that spans a `\r\n` line ending. **MD.**

```
input:  "``a\r\nb``"
mldoc:  Code(text="a\n\nb")   ← normalizes the CRLF inside the code content
lsdoc:  Code(text="a\r\nb")   ← keeps the raw CRLF
```

**Root cause:** `inline.rs::code_span` extracts the code content as a raw byte-slice; mldoc
normalizes `\r\n`→`\n\n` (line-ending normalization) inside double-code content. Purely a
content-extraction quirk in `code_span` — **pre-existing** (verified byte-identical on `bb35b6e`,
before the Phase-D lazy-code-span refactor, which reuses `code_span` verbatim).

**Reachability:** a CRLF file with a multi-line double-backtick code span. Uncommon (most graphs
are LF; single-backtick code stops at the newline so it's double-code-only).

**Status: FIXED — commit `c26b8ca`.** The real scope was BROADER than "double-code": mldoc
normalizes `\r`→`\n` (1:1) during INLINE parsing, so it also hits emphasis / link-label /
sub-superscript content (breaks-off reparse contexts), not just code. Fix = normalize `\r`→`\n`
at `parse_ctx` entry when `!ctx.breaks` (resolver.rs + org_resolver.rs) + on code-span text in
`try_code_span`. NOT global (block-level raw slices keep `\r` — that's what D5/D6 expose). Span-safe.

---

## D5 — a multi-line `$$…$$` is inline latex, not a `displayed_math` block

**Trigger:** `$$…$$` whose content spans a newline. **MD.** (Single-line `$$ab$$` is byte-exact.)

```
input:  "$$a\nb$$"            (or the block form "$$\na\nb\n$$")
mldoc:  displayed_math{text:"a\nb"}                          ← a BLOCK, content raw
lsdoc:  paragraph[ plain "$$a", break, plain "b$$" ]         ← unrecognized → paragraph text
```

**Root cause:** lsdoc has no block-level multi-line `$$…$$` recognizer; inline `$…$`/`$$…$$` is
single-line, so a newline inside breaks it and it falls to paragraph. mldoc recognizes a
display-math BLOCK spanning lines. **Reachable** (a Logseq block body is multi-line; display math
across lines is normal). **Fix direction:** add a multi-line `$$…$$` display-math block recognizer
in the block phase — MUST stay single-pass O(n) (scan to the closing `$$` with a monotone cursor,
no per-line rescan) and add NO cap. Status: **OPEN.**

## D6 — multi-line inline HTML is `inline_html` in a paragraph, not a `raw_html` block

**Trigger:** an HTML tag whose content spans a newline (`<kbd>a\nb</kbd>`, `<div>a\nb</div>`).
**MD.** (Single-line `<kbd>ab</kbd>` is byte-exact — also inline there, but that matches.)

```
input:  "<div>a\nb</div>"
mldoc:  raw_html{text:"<div>a\nb</div>"}                     ← a BLOCK
lsdoc:  paragraph[ inline_html "<div>a\nb</div>" ]           ← inline in a paragraph
```

**Root cause:** lsdoc recognizes inline HTML but not a multi-line `raw_html` BLOCK. mldoc promotes
multi-line HTML to a block. **Reachable** (multi-line HTML in a block body). **Fix direction:**
block-level multi-line HTML recognizer — single-pass O(n), no cap. Status: **OPEN.**

## D7 — an org `[[url][label]]` link with a newline in the label isn't recognized

**Trigger:** org bracket link whose label spans a newline. **ORG.**

```
input:  "[[http://x][a\nb]]"
mldoc:  link{ url:http://x, label:[plain "a\nb"] }           ← recognized; label CR→LF normalized
lsdoc:  plain "[[", bare-url(http://x), plain "][a", break, plain "b]]"   ← not recognized
```

**Root cause:** lsdoc's org `[[url][label]]` recognizer requires the whole link on one line; a
newline in the label breaks it. mldoc allows a multi-line label. **Reachability:** uncommon (org,
multi-line link label). **Fix direction:** allow a newline inside the org link label scan — stay
O(n) (no per-link rescan), no cap; once recognized the label reparse already normalizes CR (D4).
Status: **OPEN.**

## D8 — an org `^{…}` / `_{…}` script with a newline in the body isn't recognized

**Trigger:** org sub/superscript `^{…}`/`_{…}` whose body spans a newline. **ORG.**

```
input:  "a^{b\nc}"
mldoc:  plain "a", superscript[plain "{b\nc}"]               ← recognized; body CR→LF normalized
lsdoc:  plain "a^{b", break, plain "c}"                      ← not recognized
```

**Root cause:** lsdoc's org `^{…}`/`_{…}` recognizer is single-line. mldoc allows a newline in the
braced body. **Reachability:** rare (org, multi-line script body). **Fix direction:** allow a
newline inside the braced-script scan — O(n), no cap. Status: **OPEN.**

## D33 (originally mis-filed as a second "D9") — a case-mismatched raw-HTML close keeps its source case instead of the opener's

**Trigger:** raw HTML where the matching `</tag>` differs in ASCII case from the opener. **MD + ORG.**

```
input:  "<DIV>a</div><div>b</DIV>"
mldoc:  raw_html "<DIV>a</DIV>", raw_html "<div>b</div>"    ← close REWRITTEN to the opener's case
lsdoc:  raw_html "<DIV>a</div>", raw_html "<div>b</DIV>"    ← close kept verbatim from source
```

**Root cause:** mldoc's `Raw_html.parse` matches the close case-insensitively but RECONSTRUCTS the
consumed text with the opener's tag token, so the emitted close is case-normalized to the opener.
lsdoc matches case-insensitively too (extent identical — block boundaries match) but copies the
source bytes verbatim. Found by an adversarial probe during the Jul 2026 raw-html index
verification (`harness/rawhtml-fix-my-probe.json` p03/p04/p13); pre-existing, NOT introduced by
the index rewrite (unmodified-HEAD output identical). **Reachability:** rare (mixed-case HTML tag
pairs). **Status: FIXED — commit `cf3d2be`.** The real scope was BROADER than first filed: the
normalization also applies to INTERMEDIATE closes at nesting level > 0 (`<b>a<b>c</B>d</B>` →
`<b>a<b>c</b>d</b>`) and to `inline_html` (same `Raw_html.parse`), and a `</tag>` inside the
opener's own attribute region counts as the first consumed close (mldoc's `end_string_2` scans
from right after the tag token). The `/>` self-close fallback and the special forms stay
byte-exact. Fix = one charged ci scan per captured extent at text-build time (spans untouched);
spec + full diagnosis in `subagent-tasks/d33-case-normalized-close-spec.md` and
`subagent-tasks/notes/d33-d34-diagnosis.md`.

---

## Not on this list (for contrast)
These are **sanctioned**, not divergences to fix — see the O(n) audit spec's E1/E2:
- **`refs.rs` sort/dedup** — O(R log R), the one deliberate super-linear place (canonical ref order).
- **`GT_FALLBACK_NEST_CAP`** — the bounded §3 `>`-quote-fallback guard; a `[64,~1000]` parity gap on
  adversarial construct-in-`>`-quote nesting that needs ~quadratic input for linear depth (never in
  real content; mldoc stack-overflows there too).

# Known byte-exact divergences from mldoc@1.5.7 — to be dealt with

Cases where lsdoc's AST differs from mldoc's. **All three are pre-existing** (verified
byte-identical on the pre-change tree — none was introduced by the 2026-07 container-frame
rewrite or the O(n) audit; the audit/rewrite merely *surfaced* them via targeted probes). All
three are currently absorbed by the fuzz floors (`node fuzz.mjs … ` md=555 / org=1522) — i.e.
they are among the "known non-matching" adversarial inputs the floor tolerates, so they do **not**
fail the gate. **Fixing each LOWERS the floor** (more mldoc parity), which is how we'll verify a fix.

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

**Fix direction:** normalize `\r\n`→`\n\n` (confirm the exact mldoc rule — `\n\n` vs `\n`) in
`code_span`'s content extraction for the double-backtick case. SMALL, localized to `inline.rs`.
Status: **OPEN.** (Lowest priority — the most exotic of the four.)

---

## Not on this list (for contrast)
These are **sanctioned**, not divergences to fix — see the O(n) audit spec's E1/E2:
- **`refs.rs` sort/dedup** — O(R log R), the one deliberate super-linear place (canonical ref order).
- **`GT_FALLBACK_NEST_CAP`** — the bounded §3 `>`-quote-fallback guard; a `[64,~1000]` parity gap on
  adversarial construct-in-`>`-quote nesting that needs ~quadratic input for linear depth (never in
  real content; mldoc stack-overflows there too).

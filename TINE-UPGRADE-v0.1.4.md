# lsdoc v0.1.4 — upgrade note for the Tine integration

Bump the git dependency to **`tag = "v0.1.4"`**. One small thing to handle, the rest is free.

## ⚠️ Action required: two new AST variants (`hiccup`)

v0.1.4 adds Clojure-hiccup `[:tag …]` support (mldoc `Hiccup` / `Inline_Hiccup`). That means
**two new variants** on the public AST — if you `match` exhaustively on `Block`/`Inline` in the
BlockDto mapping or the renderer, you must add arms for them (otherwise: a Rust non-exhaustive-match
compile error, or an unhandled-node gap in the TS renderer).

**Rust (`lsdoc::ast`):**
- `Block::Hiccup { v: String, span: Option<Span> }`
- `Inline::Hiccup { v: String }`

**Serde / TS shape (what reaches the renderer):**
- block: `{ "kind": "hiccup", "v": "[:div.foo \"hi\"]" }` (+ optional `span`)
- inline: `{ "k": "hiccup", "v": "[:span \"x\"]" }`

`v` is the **raw bracket text verbatim** — mldoc does NOT parse the children, and neither does
lsdoc; it's an opaque string. **No refs** are extracted from hiccup (your ref index is unaffected).

### How to render it
- **Minimum (do this now):** render `v` as literal text (or a `<code>`-style span). This is safe,
  never crashes, and is fine — hiccup is **absent from every real graph** in our test corpus
  (blockgate 99/99 has none), so it's an edge construct.
- **OG-faithful (optional follow-up):** Logseq renders hiccup as actual HTML (the `[:tag attrs …]`
  vector → the corresponding HTML element). A faithful renderer would transform the hiccup vector
  to HTML. Low priority given its rarity; the raw-text fallback is acceptable until then. If/when
  you do it, the recognition rule is mldoc's: `[:` + an HTML-element name from a 110-tag allowlist +
  optional `.class`/`#id`/`{attrs}`/children. (lsdoc has already done the *recognition*; you'd only
  need the hiccup→HTML *rendering*.)

## Free wins (no code change on your side — just the version bump)

These all fixed parsing on arbitrary block content, which is exactly what you feed lsdoc:
- **Two DoS classes gone** (worth the bump alone): a single block line of `[`×n was O(n³)
  (multi-second hangs); a line of nested `>` could **stack-overflow and abort the process**. Both
  now linear/bounded. Plus 7 more O(n²) classes (`#+BEGIN`/drawers/unclosed-fence/inline-HTML/
  inline-latex) linearized.
- **A crash fixed:** a multibyte org directive key (`#+END_<non-ASCII>:`) used to panic.
- **5 correctness fixes** (output now matches Logseq): blockquotes whose first line is a list/
  heading marker no longer silently lose their text; markdown tables are no longer over-detected
  from a single leading `|` (which also produced phantom block-refs); org tags keep backslashes;
  **CRLF / lone-CR** line endings are handled (was leaving `\r` in content — affects any
  Windows/pasted text); org property values no longer emit a false page-ref.

## TL;DR
Bump to `v0.1.4`; add the `Block::Hiccup`/`Inline::Hiccup` match arms (render `v` as text for now);
everything else is a transparent correctness/perf/safety upgrade.

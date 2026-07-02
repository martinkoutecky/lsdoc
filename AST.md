# lsdoc AST — the render contract

This maps **every renderable construct → its AST variant + exact payload fields**, so a
consumer (Tine) can render exhaustively from the AST alone and extract refs, with no
re-parsing of `full`/raw text. It is the renderer's checklist and the proof that the
mldoc-vs-projection delta is closed: every field here is gated 0-diff against
`mldoc@1.5.7` (`harness/run.mjs`, 621 inputs), except the explicitly-marked
derived/excluded ones.

The types live in [`src/projection.rs`](src/projection.rs) (`lsdoc::ast`); the JS mirror
that proves parity is [`harness/lib/normalize.mjs`](harness/lib/normalize.mjs). Both
Markdown and Org produce the **same** AST.

## Serde encoding (the wire contract)

- A parse is `Projection { blocks: Block[], refs: Refs }`. The render path wants `blocks`;
  the index path wants `refs` (see `lsdoc::refs`).
- Enums are **internally tagged**; the discriminant key differs per enum:
  - `Block` → **`"kind"`**, `Inline` → **`"k"`**, `Url` → **`"type"`**.
- **Omitted = default.** Every `Option`/`bool false`/empty-`Vec`/empty-`String` field is
  omitted (`skip_serializing_if`). A consumer must treat an absent key as the default
  (`None` / `false` / `[]` / `""`). The non-omitted (always-present) fields are noted below.
- `span` (block byte-offset `[start,end]`) is emitted but is **out of the render contract**
  (excluded from the oracle diff; Tine renders read-only). Inline nodes carry **no** span.

---

## Block (`"kind"`)

| `kind` | Always-present fields | Optional fields | Construct |
|---|---|---|---|
| `paragraph` | `inline: Inline[]` | `span` | a text paragraph |
| `heading` | `level: u32`, `size: u32\|null`, `inline: Inline[]` | `marker`, `priority`, `htags: string[]`, `span` | ATX `#…`/setext heading; org `*` headline with a `#`-level (md). `size` = setext/ATX size |
| `bullet` | `level: u32`, `inline: Inline[]` | `size: u32`, `marker`, `priority`, `htags: string[]`, `span` | outline bullet (md `-`) / org headline (`*`) — mldoc `Heading{unordered}`. `size` = heading level when the body is an ATX heading (`- ## T` → 2), else absent |
| `list` | `items: ListItem[]` | `span` | `*`/`+`/`N.` (md) and `-`/`+`/`N.` (org) list |
| `src` | `lang: string`, `code: string` | `span` | fenced/`#+BEGIN_SRC` code block. `lang` may be `""` |
| `quote` | `children: Block[]` | `span` | `>` blockquote / `#+BEGIN_QUOTE` |
| `custom` | `name: string`, `children: Block[]` | `span` | `#+BEGIN_X … #+END_X`, X≠QUOTE (NOTE/TIP/WARNING/…) |
| `raw_html` | `text: string` | `span` | block-level raw HTML |
| `displayed_math` | `text: string` | `span` | block `$$…$$` (mldoc `Displayed_Math`) |
| `drawer` | `name: string` | `span` | org `:NAME: … :END:` (content opaque — name only) |
| `directive` | `name: string`, `value: string` | `span` | org `#+KEY: value` |
| `comment` | `text: string` | `span` | org `# text` comment line (not rendered) |
| `example` | `code: string` | `span` | org `#+BEGIN_EXAMPLE` / fixed-width `:` lines |
| `latex_env` | `name: string`, `content: string` | `span` | `\begin{X}…\end{X}` (name lowercased) |
| `properties` | `props: [string, string][]` | `span` | `key:: value` block / org `:PROPERTIES:` |
| `hr` | — | `span` | horizontal rule |
| `table` | `header: Inline[][] \| null`, `rows: Inline[][][]` | `span` | table. **No column alignment** (see Notes) |
| `footnote_def` | `name: string`, `inline: Inline[]` | `span` | `[^id]: body` / org `[fn:id] body` |
| `hiccup` | `v: string` | `span` | block-level Clojure-hiccup vector `[:tag …]` occupying a whole line (mldoc `Hiccup`). `v` = the RAW bracket text verbatim (children NOT parsed). md + org |

`marker` = task marker (`TODO`/`DOING`/`DONE`/…). `priority` = org `[#A]` → `"A"`.
`htags` = org headline `:tag1:tag2:`.

### ListItem (an element of `list.items`)

| Field | Type | Notes |
|---|---|---|
| `ordered` | `bool` | always present |
| `number` | `u32?` | present for `N.` items |
| `indent` | `u32` | always present; leading-whitespace columns |
| `content` | `Block[]` | the item body (usually one `paragraph`) |
| `items` | `ListItem[]` | nested child items (always present, may be `[]`) |
| `name` | `Inline[]?` | markdown definition-list term (`term\n: def`); absent otherwise |
| `checkbox` | `bool?` | `[ ]`→`false`, `[x]`/`[X]`→`true`; absent = no checkbox |

---

## Inline (`"k"`)

| `k` | Fields | Construct |
|---|---|---|
| `plain` | `text: string` | literal text |
| `code` | `text: string` | `` `code` `` / org `~code~` |
| `verbatim` | `text: string` | org `=verbatim=` |
| `break` | — | soft line break (`keep_line_break`) |
| `hardbreak` | — | hard break (trailing `\` / two spaces) |
| `emphasis` | `emph: string`, `children: Inline[]` | see **emph vocabulary** below |
| `subscript` | `children: Inline[]` | org `_{x}` / `a_b` |
| `superscript` | `children: Inline[]` | org `^{x}` / `a^b` |
| `link` | `url: Url`, `full: string`, **opt** `label: Inline[]`, `image: bool`, `metadata: string`, `title: string` | links, images, page/block refs, autolinks — see **Link** |
| `nested_link` | `content: string` | Logseq `[[a [[b]] c]]` (raw inner kept) |
| `target` | `text: string` | org dedicated/radio target `<<name>>` |
| `tag` | `children: Inline[]` | `#tag` / `#[[bracket tag]]` |
| `macro` | `name: string`, `args: string[]` | `{{name arg1, arg2}}` (incl. `{{embed …}}`, `{{query …}}`) |
| `export_snippet` | `name: string`, `content: string` | `@@name: content@@`; render exposes only `name == "html"` as raw HTML |
| `latex` | `mode: string`, `body: string` | `mode` ∈ {`"Inline"`, `"Displayed"`}; `$x$` / `$$x$$` / `\(x\)` / `\[x\]` |
| `timestamp` | `ts: string`, `date: Value` | `ts` ∈ {`"Date"`,`"Range"`,`"Scheduled"`,`"Deadline"`,`"Closed"`,`"Clock"`}; `date` = opaque (see below) |
| `cookie` | `kind: string`, `value: number`, **opt** `total: number` | statistics cookie; `kind` ∈ {`"Absolute"`,`"Percent"`}; `total` only for absolute cookies |
| `fnref` | `name: string` | footnote reference `[^id]` / `[fn:id]`; inline definitions are parsed by mldoc but projection-invisible |
| `inline_html` | `text: string` | inline raw HTML `<span>…` |
| `email` | `text: Value` | `<a@b.com>` autolink; `text` opaque (see below) |
| `entity` | `name, latex, html, ascii, unicode: string`, `latex_mathp: bool` | LaTeX entity `\Delta` → resolved record (see `src/entities.rs`) |
| `hiccup` | `v: string` | inline Clojure-hiccup vector `[:tag …]` mixed with text (mldoc `Inline_Hiccup`). `v` = the RAW bracket text verbatim (children NOT parsed) |

### Link (`k:"link"`) — the render-critical fields

| Field | Present when | Meaning |
|---|---|---|
| `url` | always | the destination — a `Url` (below) |
| `full` | always | the raw source text of the link (incl. leading `!`/metadata) |
| `label` | non-empty | the rendered label (inline children). Absent for bare refs |
| `image` | `true` only | `![…](…)` image. **Derived** from `full`'s leading `!` (mldoc has no native bit); md only |
| `metadata` | non-empty | raw Logseq media dims `{:width … :height …}` (braces incl.) |
| `title` | present | CommonMark `[l](u "title")` — raw inner (no quotes, not unescaped) |

A renderer decides "image vs link" from `image` (no need to sniff `full`); sizes the image
from `metadata`; shows `title` as a tooltip; renders the destination from `url`.

### Url (`"type"`)

| `type` | Fields | Source |
|---|---|---|
| `page_ref` | `v: string` | `[[Page]]` |
| `block_ref` | `v: string` | `((uuid))` |
| `search` | `v: string` | a bare/relative destination (no protocol) — incl. most image paths |
| `file` | `v: string` | org `file:…` |
| `complex` | `protocol: string?`, `link: string?` | `proto://…` (http(s), etc.) |

---

## Vocabularies (exact strings)

- **`emphasis.emph`**: `"Bold"`, `"Italic"`, `"Strike_through"`, `"Highlight"`, `"Underline"`.
  (md emits Bold/Italic/Strike_through/Highlight; org adds Underline. `~~`→Strike_through;
  md `^^…^^` / `==…==` and org `_…_` map as Logseq does.) Nesting: `***x***` →
  `Italic[Bold[x]]`.
- **`latex.mode`**: `"Inline"` | `"Displayed"`.
- **`timestamp.ts`**: `"Date"` | `"Range"` | `"Scheduled"` | `"Deadline"` | `"Closed"` | `"Clock"`.

### Opaque `Value` payloads

`timestamp.date` and `email.text` are passed through as **mldoc's raw JSON** (not re-shaped),
so they are render-complete without lsdoc committing to a sub-schema:

- `timestamp.date` — for a single date: `{date:{year,month,day}, wday, active, time?:{hour,min},
  repetition?}`; for `ts:"Range"`: `{start:{…}, stop:{…}}`; for `ts:"Clock"`:
  `["Started",{…}]` or `["Stopped",{start:{…},stop:{…}}]`. SCHEDULED/DEADLINE/CLOSED/CLOCK and
  date ranges all flow through here (the `ts` tag distinguishes), so heading-level planner badges
  render from the `Timestamp` inline — there is no separate heading-meta field.
- `email.text` — mldoc's address record.

A consumer that needs a typed view of these should mirror the shapes above but may treat them
as opaque for display.

---

## Notes / deliberate gaps (render-relevant, NOT carried — see DECISIONS.md §"Render-level parity")

- **Table column alignment is not available.** mldoc 1.5.7 discards it (`col_groups` is just
  the column count); Logseq does not render aligned tables, so neither does this AST. If you
  need it, it must be re-derived from the source separator row — it is not in the AST.
- **Clojure-hiccup `[:tag …]`** (mldoc `Hiccup` / `Inline_Hiccup`) IS carried — as the
  `hiccup` block + inline variants above. `v` is the raw bracket text verbatim (mldoc does
  NOT parse the children, so a renderer treats it opaquely; no refs are extracted from it).
  Recognition matches mldoc exactly: `[:` + an HTML-element name from mldoc's 110-tag
  allowlist (case-insensitive) + a keyword boundary (`]`/space/tab/`.`/`#`) + a string-aware,
  `[:`-nested balanced `]`. A whole-line vector → a `hiccup` block (the remainder past the
  `]` re-enters block parsing); a vector mixed with text → an inline `hiccup`.
- **No inline spans.** Block `span` (byte `[start,end]`) is present but excluded from the
  render contract; inline nodes have none. Source-rewriting features (media-resize, checkbox
  toggle) operate on the block's raw text, not the AST.
- Dropped mldoc internals with no render impact: `Heading.anchor`/`meta`,
  `Footnote_Reference.id` and inline `definition`, `Nested_link.children` (use `content`),
  `Src.options`/`pos_meta`.

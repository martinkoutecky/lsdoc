# lsdoc → Tine integration: prerequisite work (lsdoc side)

**Audience:** the lsdoc session (this repo).
**Purpose:** make lsdoc cleanly consumable by Tine as its single parser. This file
lists ONLY the lsdoc-side work that must land *before* Tine can integrate. The
Tine-side work (adding the dependency, deleting `parseInline.ts`, repointing
`refs.rs`, rendering from the AST) happens in the Tine session and is summarized at
the end for context, so you can shape the AST to fit the consumer.

Written 2026-06-28 after auditing both sides. lsdoc is otherwise complete (M1–M6,
exact mldoc parity, 583/583, zero allowlist). The blockers below are small but real.

---

## How Tine will use lsdoc (the contract you're designing for)

Tine already owns the outline/file layer. It parses a page file into an outline
tree of blocks (`doc.rs`), each block carrying a **`raw: String`** = that block's
full body (first line + de-indented continuation lines + its `id::`/`collapsed::`/
other properties), and persists it back to disk itself. Tine also owns rename,
alias resolution, namespaces, and `tags::`/`alias::` property semantics.

So lsdoc stays a **pure, read-only content parser**. Tine will call it two ways:

1. **Render path:** `lsdoc::parse(block.raw, format) -> Vec<Block>` per outline
   block. The block's `raw` is fed as a mini-document; lsdoc segments it into
   paragraph/heading/code/table/quote/properties/list/… blocks with inline trees.
   Tine serializes that AST across the Tauri DTO boundary and renders it in
   SolidJS, **replacing** `parseInline.ts` + `body.tsx`'s TS segmentation.
2. **Index path:** `lsdoc::refs(block.raw, format) -> Refs` (page + block refs),
   replacing the inline-ref half of Tine's `refs.rs`.

What lsdoc does **NOT** need to do (stays in Tine — do not build these):

- No outline segmentation, no `id::`/`collapsed::` handling, no file I/O, no
  serialization-back-to-disk. Tine keeps `raw` and round-trips it byte-for-byte.
- No rename / ref-rewriting. Tine rewrites raw text in `refs.rs`.
- No `tags::` / `alias::` / namespace-parent ref semantics. Those are Logseq
  *app-layer* rules layered on top of mldoc (mldoc itself doesn't know them);
  Tine keeps that layer. lsdoc only extracts the mldoc-faithful inline ref set
  (`[[…]]`, `#tag`, `((uuid))`, `[l](((uuid)))`, `{{embed …}}`).
- **No inline source spans.** Tine renders read-only; the two features that rewrite
  source (media-resize width, list checkbox toggle) operate on the block's `raw`,
  not the AST. Block-level spans already exist and are enough. Do not build inline
  spans for this.

---

## Prereq 1 — Render-grade AST (the only substantial item)

The `projection` types were built for the **differential oracle**, which normalizes
away anything that doesn't change the ref set or block structure. Two such
normalizations are **lossy for rendering** and must be fixed:

### 1a. Images are indistinguishable from links

`![alt](url)` is parsed (`try_image` → `parse_md_link(.., image=true)`), but the
projection `Inline::Link { url, label, full }` carries **no image flag**. An image
currently differs from a link only by `full` starting with `!`. The renderer must
render `<img>`/video/audio/PDF embeds vs `<a>` and cannot rely on string-sniffing
`full`.

- **Do:** expose image-ness as a first-class signal — either a distinct
  `Inline::Image { … }` variant, or an `image: bool` on the link node.
- **Accept:** `![a](x.png)` and `[a](x.png)` produce different, machine-checkable
  AST tags; a TS consumer can branch on the tag without inspecting `full`.

### 1b. Link/image metadata `{:width … :height …}` is dropped

`parse_md_link` captures the trailing `{…}` into a local `metadata` string, then
**discards it** into `full`. Tine renders media at a stored width/height
(`![a](v.mp4){:width "40%"}`) and needs it structured.

- **Do:** expose the metadata on the link/image node. Raw metadata string is
  acceptable; parsed `width`/`height` is better.
- **Accept:** `![a](x.png){:width "40%"}` exposes the width without the consumer
  re-parsing `full`.

### 1c. Lossless-for-rendering audit + a construct→variant table

You own the parser; do one pass over every place the projection is normalized for
the oracle and confirm nothing **else** a renderer needs is dropped. Produce a short
table (in `AST.md` or doc-comments) mapping each renderable construct to its AST
variant + payload, so the Tine session can render exhaustively. Known-sufficient
already (just confirm + document the exact payload names/values):

- emphasis discriminator `Emphasis.emph` — list the exact strings emitted
  (bold/italic/underline/strikethrough/highlight/…) so TS can map them.
- `Macro { name, args }`, `Latex { mode, body }`, `Timestamp { ts, date }`,
  `Code`/`Verbatim`, `Tag`, `Link.url` variants (`page_ref`/`block_ref`/`search`/
  `file`/`complex`), `Entity` (resolved unicode), `InlineHtml`, `Email`, `Fnref`,
  `Subscript`/`Superscript`, `Break`/`HardBreak`.
- block variants: `Paragraph/Heading/Bullet/List/Src/Quote/Custom/RawHtml/
  DisplayedMath/Drawer/Directive/Example/LatexEnv/Properties/Hr/Table/FootnoteDef`
  (a superset of what Tine's `body.tsx` renders today — good).

**Oracle note:** mldoc *does* carry image-ness and link metadata, so you may either
keep the new fields out of the comparison, or (preferred) fold them into
normalize.mjs and strengthen parity. Either way, do not regress 583/583.

---

## Prereq 2 — Bless + freeze the public API and serde contract

Tine will depend on these types as a **stable, serialized contract** mirrored 1:1 by
hand-written TypeScript types. Today they live in `projection.rs` and are documented
as "comparison-only, NOT lsdoc's real AST." That framing is now wrong — they are the
integration AST.

- **Do:** update that doc comment; bless `projection::{Block, Inline, Url, ListItem,
  Span, Refs, Projection}` as public API (re-exporting under a stable path such as
  `lsdoc::ast` is fine). Commit to the serde representation: the internally-tagged
  enums (`tag = "kind"` / `"k"` / `"type"`), every field name, and every `rename`
  value. Document them so the TS union can mirror them exactly.
- **Do:** firm up or explicitly document the two loose `serde_json::Value` fields —
  `Timestamp.date` and `Email.text`. Either give them a defined shape, or declare
  them opaque/optional (the renderer can treat them as such).
- **Do:** provide ergonomic entry points alongside `parse_format`:
  `parse(input, format) -> Vec<Block>` (render path) and a refs-only
  `refs(input, format) -> Refs` (index path), so Tine isn't forced to compute both
  when it needs one.
- **Accept:** a TS consumer can deserialize a `Vec<Block>` JSON and exhaustively
  switch on every tag; blocks-only and refs-only entry points exist.

---

## Prereq 3 — Vendor-clean library crate

Tine pulls lsdoc into its Cargo workspace. The mechanism is Martin's call
(recommended: **vendor `src/` as `crates/lsdoc/` inside the Tine repo**, synced from
this repo and pinned to a recorded commit — simplest CI, and AGPL means lsdoc source
ships with Tine anyway). Whatever the mechanism, ensure:

- The **library** (`src/lib.rs` + modules) builds standalone depending on **only**
  `serde` + `serde_json`. The `bin/` driver and `harness/` (Node oracle) are not
  needed by Tine — confirm nothing the lib exports pulls them in.
- **Toolchain:** lsdoc is `edition = "2024"` (needs Rust ≥ 1.85). Tine's CI must use
  a new-enough toolchain. If dropping to `edition = "2021"` is cheap, consider it to
  reduce Tine-CI friction — otherwise just flag the minimum Rust version here.
- **Pin:** tag a release or record the commit Tine should vendor from.
- **Accept:** `cargo build -p lsdoc` from a fresh checkout (serde from crates.io,
  nothing else) succeeds and exposes the blessed API.

---

## Out of scope (do NOT build — prevents over-engineering)

- Inline source spans (see contract above).
- Any rewriting / rename / serialization.
- `tags::` / `alias::` / namespace / page-existence semantics (Tine app-layer).
- Async resolution (block-ref preview text, asset blob URLs, KaTeX, highlight.js,
  emoji): all stay frontend-side in Tine; the AST only needs to carry the id / path
  / tex / lang / macro body, which it already does.

## Definition of done

A fresh Tine session can: add lsdoc to the workspace; call `parse(raw, format)` and
render **every** construct from the AST alone (no re-parsing of `full` or raw); and
call `refs(raw, format)` for the inline ref set — with the construct→variant table
(1c) as the renderer's checklist.

---

## Appendix — Tine-side plan (for context; not your work)

After the above lands, the Tine session will: (1) add the lsdoc dep; (2) add a
`parse_block(raw, format) -> Vec<Block>` command and embed `lsdoc::ast::Block` in the
`BlockDto` (or a sibling field); (3) hand-write the TS mirror of the AST and rewrite
`render/inline.tsx` + `render/body.tsx` to render from it; (4) delete
`parseInline.ts` + `parseInline.test.ts`; (5) repoint `refs.rs`'s inline extraction
to `lsdoc::refs`, keeping Tine's property/namespace/alias/rename layer on top;
(6) keep media-resize + checkbox-toggle operating on `raw`. The async render
dependencies (block-ref preview, asset blobs, KaTeX, hljs, emoji, journal-title
routing) are unaffected — they consume ids/paths from the AST exactly as they
consume them from today's tokens.

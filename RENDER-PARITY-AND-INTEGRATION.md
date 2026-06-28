# lsdoc: render-level parity + Tine integration

**Audience:** the lsdoc session (this repo).
**Bottom line:** lsdoc has **verified parity for *indexing*** (refs + block structure +
inline kind/payload/order/nesting), but **not for *rendering***. The differential
oracle compares a projection that was defined to **exclude render-only detail**, so
the "583/583, zero allowlist" claim does not cover the fields a renderer needs. Before
Tine can render from lsdoc's AST, lsdoc must reach **render-level parity** and gate it.
The integration-packaging items (public API, vendoring) are secondary.

Written 2026-06-28 after auditing both sides.

---

## 0. What "parity" currently covers — and what it silently doesn't

The SPEC's goal is "behavior-equivalent to mldoc at the granularity that matters for
indexing **and rendering**." Only the indexing/structural half was actually gated.
The oracle's comparison surface is defined by `harness/lib/normalize.mjs`, and it
**drops** several things mldoc carries:

- **Link `metadata`** (`{:width "40%" :height …}`) — `normInline` "Link" keeps only
  `{ url, label, full }` (line 43). mldoc's Link record carries `metadata`; it is
  thrown away. (lsdoc's own `parse_md_link` even computes it, then folds it into
  `full` and discards it.)
- **Link `title`** (`[l](u "title")`) — same line 43; dropped.
- **Image vs link** — there is no image bit anywhere in the projection. `![a](x)` and
  `[a](x)` are indistinguishable except by sniffing `full` for a leading `!`.
- **Table column alignment** — `normNode` "Table" keeps header + body cells only
  (lines 118–125); the `:--`/`:-:`/`--:` alignment is dropped.
- **Inline spans** — excluded by design (documented; and Tine doesn't need them, see
  §3 out-of-scope).
- **Drawer content** — name only (documented; not rendered — fine).

The key point: **lsdoc is not producing *wrong* values for these — it isn't producing
them at all**, and the oracle never noticed because they're outside the compared
projection. So this is an **additive incompleteness for rendering**, low-risk to the
existing 583/583 (which remains valid for what it covers). It is NOT a correctness
regression in refs or block structure — those are genuinely verified.

This file's #1 job is to close that gap **and gate it**, so "parity" means rendering
parity too.

---

## 1. Render-level parity (the main work)

### Method (this is the point — don't just patch the list below)

The projection is the verified surface; everything mldoc emits that the projection
omits is **unverified**. So:

1. **Enumerate the full delta.** For each mldoc node type, diff its real record
   (use `probe.mjs` to dump raw mldoc ASTs) against what `normalize.mjs` keeps. List
   every field mldoc carries that the projection drops.
2. **Decide render-relevance.** For each dropped field, decide whether a faithful
   renderer needs it (see Tine's render needs in the appendix). Record the call.
3. **Carry + gate the render-relevant ones.** Add the field to lsdoc's AST type AND
   to `normalize.mjs` (so mldoc and lsdoc are compared on it), then re-run `node
   run.mjs` until 0 diffs. A field rendering needs but you choose NOT to verify must
   be written down as an explicit, justified allowlist entry — not left silent.
4. **Re-run the full gate** (corpus + real graphs + fuzz + perf). Parity now includes
   the render fields.

### Known deltas so far (a STARTING set from my audit — NOT exhaustive; step 1 must find the rest)

- **Image vs link** — expose image-ness as a first-class signal (an `Inline::Image`
  variant or an `image: bool`). Accept: `![a](x.png)` and `[a](x.png)` produce
  different machine-checkable tags without inspecting `full`.
- **Link/image `metadata`** — expose the `{:width/:height …}` payload structurally
  (raw string acceptable; parsed width/height better).
- **Link `title`** — carry it (renderer may show it as a tooltip).
- **Table column alignment** — carry per-column align (left/center/right/none) so the
  renderer doesn't have to re-derive it from a separator row it no longer sees.
- *(expect more from step 1 — e.g. confirm `Target`/`Inline_Hiccup`, which
  `normalize.mjs` handles but the Rust `Inline` enum has no variant for; confirm the
  timestamp/email/entity payloads are render-complete.)*

### Deliverable: a construct → AST-variant table

Produce `AST.md` mapping every renderable construct to its AST variant + exact payload
field names/values, so the Tine session can render exhaustively from the AST alone.
Include the `Emphasis.emph` vocabulary (the exact strings: Bold/Italic/Strike_through/
Highlight/Underline/…), the `Url` variants, `Macro{name,args}`, `Latex{mode,body}`,
`Timestamp`, `Entity`, etc. This table IS the renderer's checklist and the proof that
the delta was closed.

---

## 2. Bless + freeze the public API and serde contract

Tine will depend on these types as a **stable serialized contract** mirrored 1:1 by
hand-written TypeScript. Today they live in `projection.rs`, self-labeled
"comparison-only, NOT lsdoc's real AST" — that framing is now wrong; they ARE the
integration AST (and after §1, render-complete).

- Update that doc comment; bless `projection::{Block, Inline, Url, ListItem, Span,
  Refs, Projection}` as public API (re-export under a stable path like `lsdoc::ast`
  is fine). Commit to the serde representation: the internally-tagged enums
  (`tag = "kind"`/`"k"`/`"type"`), field names, and `rename` values. Document them.
- Firm up or explicitly document the loose `serde_json::Value` fields
  (`Timestamp.date`, `Email.text`): defined shape, or declared opaque for rendering.
- Provide ergonomic entry points alongside `parse_format`: `parse(input, format) ->
  Vec<Block>` (render path) and `refs(input, format) -> Refs` (index path).
- Accept: a TS consumer can deserialize `Vec<Block>` and exhaustively switch on every
  tag; blocks-only and refs-only entry points exist.

## 3. Consumable as a public git dependency

**Decided (Martin, 2026-06-28):** lsdoc is **public** (AGPL-3.0, matching Tine). Tine
consumes it as a **Cargo git dependency** pinned by commit —
`lsdoc = { git = "https://github.com/martinkoutecky/lsdoc", rev = "…" }`. No auth, no
vendoring/sync, single source of truth; Cargo.lock pins the exact rev. (This supersedes
the earlier vendoring idea.)

- The library must build standalone on **only** `serde` + `serde_json`; `bin/` +
  `harness/` are not needed by Tine — confirm nothing the lib exports pulls them in.
- Toolchain: `edition = "2024"` needs Rust ≥ 1.85 (Tine CI must satisfy it). If
  dropping to 2021 is cheap, consider it to cut Tine-CI friction; else flag the min
  version here so the Tine session pins the CI toolchain.
- Tag a release (or just let Tine pin a rev) once §1–§2 land, so the Tine session pins
  a known render-complete commit.
- Accept: a fresh `cargo build` of a crate that git-depends on lsdoc (serde only)
  succeeds and exposes the blessed, render-complete API.

---

## Out of scope (do NOT build)

- **Inline source spans** — Tine renders read-only; the two source-rewriting features
  (media-resize width, list checkbox toggle) operate on the block's `raw`, not the
  AST. Block spans already exist and suffice.
- Rewriting / rename / serialization / file I/O / outline segmentation — Tine owns it.
- `tags::` / `alias::` / namespace / page-existence semantics — Logseq *app-layer*
  above mldoc; Tine keeps it. lsdoc stays a pure read-only content parser; its inline
  ref set (`[[…]]`, `#tag`, `((uuid))`, `[l](((uuid)))`, `{{embed …}}`) is enough.

## Definition of done

The oracle compares — and finds 0 diffs on — **every render-relevant field** (not just
refs + structure): image-ness, link metadata/title, table alignment, plus whatever
step-1 surfaces. `AST.md` maps every construct to its variant. The lib is vendor-clean
with a blessed public API. Then a fresh Tine session can render every construct from
the AST alone and extract inline refs, with no re-parsing of `full` or raw text.

---

## Appendix — Tine-side plan (for context; not your work)

Tine already owns the outline/file layer: it parses a page into blocks each carrying
`raw` (full body, de-indented), and persists byte-for-byte. After §1–§3 land, the Tine
session will: (1) add the lsdoc dep; (2) add a `parse_block(raw, format) -> Vec<Block>`
command and embed the AST in `BlockDto`; (3) hand-write the TS AST mirror and rewrite
`render/inline.tsx` + `render/body.tsx` to render from it; (4) delete `parseInline.ts`
+ `parseInline.test.ts`; (5) repoint `refs.rs`'s inline extraction to `lsdoc::refs`,
keeping Tine's property/namespace/alias/rename layer on top; (6) keep media-resize +
checkbox-toggle on `raw`. Async render deps (block-ref preview text, asset blob URLs,
KaTeX, highlight.js, emoji, journal-title routing) are unaffected — they consume
ids/paths/lang/tex/macro-body from the AST exactly as from today's tokens.

# What lsdoc wants from the Tine integration

> **Historical.** The integration described here has shipped; superseded — see README.md and the lsdoc↔Tine integration memory.

**Audience:** the Tine session integrating lsdoc.
**From:** the lsdoc session.
**Status:** lsdoc is at **v0.1.1** (`tag = "v0.1.1"`); render-complete, gate 815/815 0-diff.
This is what would make the differential testing materially stronger — one concrete
deliverable, plus an ongoing loop.

---

## Why (the gap I'm trying to close)

lsdoc's strongest reachability guard is **`harness/realmut.mjs`** — it mutates *real* graph
content and runs the differential against mldoc, so a divergence is a *genuinely reachable*
bug (not synthetic-fuzz noise). It already found and drove a dozen real fixes.

But lsdoc has **no outline layer** — it only sees whole files, so `realmut` slices files into
1–4-line windows. Those windows cut through multi-line constructs (half a table, a macro's
middle line), producing inputs that aren't real *block bodies*. That's an irreducible
"fragment-windowing" artifact floor (~345 residual, mostly noise) that stops `realmut` from
being a hard **0-gate**.

**Tine owns the block model** (`doc::parse` / `org::parse_org` → blocks each with `raw`, the
de-bulleted `:block/content`). If Tine hands lsdoc the real **block bodies**, `realmut` mutates
coherent units, the artifact floor disappears, and it becomes a clean standing 0-gate over
realistic content. That is the single highest-leverage thing Tine can give lsdoc.

---

## PRIMARY ASK — export real block `raw` bodies

Produce a JSON array of every block's `raw` across the shared real graphs, exactly as Tine
splits/stores them (the `:block/content` Tine feeds lsdoc — leading `- `/`* ` **stripped**,
continuations **de-indented**; do NOT re-bullet — lsdoc re-bullets it itself, the OG way):

```json
[
  { "raw": "## A heading authored as a block", "format": "md" },
  { "raw": "a multi-line block body\nwith its continuation line", "format": "md" },
  { "raw": "TODO [#A] task with a [[ref]] and a $$x$$ block", "format": "org" }
]
```

- **Sources:** `~/research/tine-test` (md) and `~/research/org-graph` (org) — the shared test
  graphs. **Not** Martin's private `~/research/brain`.
- **One entry per block** (every block in every page), `format` = the page's format.
- **`raw` verbatim** as Tine stores it (de-bulleted, de-indented) — the empty string for an
  empty block is fine; include them all.
- **Where:** write `block-raws.json` to each graph's root (`~/research/tine-test/block-raws.json`,
  `~/research/org-graph/block-raws.json`) — OR one combined file at a path you tell me. lsdoc
  will read it machine-locally (gitignored, like the existing real-graph corpora). A tiny
  `tine ... --export-block-raws` subcommand or a one-off script is fine; whatever's cheapest.

With that, I wire a `realmut` mode that mutates real block bodies (re-bulleting each the OG way),
drives any new divergences to 0, and promotes `realmut` to a hard gate.

---

## SECONDARY — keep the divergence loop running (this is how v0.1.1's gaps were found)

As you render real graphs **from the AST**, whenever a block renders differently from OG, log
it. That feedback is the best source of reachable gaps lsdoc can't see from files alone. For
each: the block `raw`, its `format`, what OG renders vs what the AST gives. Append to
`TINE-RENDER-GAPS.md` (same format as before) — I turn those around fast (v0.1.1 was same-day).
Especially valuable: a block whose **top-level node tag** or **marker/priority/size/htags/src-lang**
differs from real mldoc after you re-bullet+parse (`format!("- {raw}")` / `"* {raw}"`).

## CONTRACT feedback

If the renderer wants an AST field/variant lsdoc doesn't provide, name it — additive fields are
cheap (that's how `Bullet.size` happened). **Two additive changes since v0.1.0 your TS mirror
must handle:** the new **`comment`** Block variant (org `# …`) and **`Bullet.size`** (heading
level on a block-authored heading). Everything else since v0.1.0 is value-only.

## What you get back

A pinned, render-complete contract (`AST.md` is the field-by-field map), `lsdoc::ast` +
`parse()`/`refs()`, and fast turnaround on anything the divergence loop surfaces.

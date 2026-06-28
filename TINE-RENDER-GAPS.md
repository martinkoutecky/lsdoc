# lsdoc render gaps found by the Tine integration (→ v0.1.1)

**Audience:** the lsdoc session.
**Source:** the Tine session, integrating lsdoc v0.1.0 (`59ccd46`) as the renderer.
**Status:** two gaps found by feeding **real Tine blocks** through lsdoc the way OG
feeds mldoc. Both are to be fixed for OG parity (one a missing field, one a segmentation
divergence). All evidence below is from **real mldoc 1.5.7** (your `harness/` oracle) vs
**lsdoc v0.1.0**, not inferred.

---

## How Tine feeds lsdoc (why these only show up now)

Tine owns the outline layer: a page is split into blocks, each carrying `raw` = the
block body **with the leading `- `/`* ` stripped and continuations de-indented** (this is
Logseq's `:block/content`). To render, Tine does **exactly what OG does**
(`frontend/format/block.cljs:94-100`, `parse-title-and-body`): it **re-prepends the block
pattern** and parses —

```
lsdoc::parse(format!("{pattern} {}", raw.trim_start()), fmt)   // pattern = "-" (md) / "*" (org)
```

— then reads `marker`/`priority`/heading-`size` off the first (bullet/heading) node and
renders the rest as body, with marker/priority as chrome. This re-bulleted, per-block
input form is what the existing 623/623 gate did **not** exercise (it parses whole files /
de-bulleted block content), so these gaps slipped through.

**Method to reproduce:** mine every block's `raw` from a real graph via Tine's
`doc::parse`/`org::parse_org`, re-bullet as above, and diff a block-skeleton (top-level
node tags + heading/bullet `marker`/`priority`/`size`/`htags`, + src lang) between real
mldoc and lsdoc. Over a 111-block corpus (a constructs kitchen-sink + the `tine-test` md
graph + the `org-graph` org graph): **103 match, 8 differ**, in exactly the two classes
below. (Inline parity is not re-checked here — it's already gated.)

---

## Gap 1 — `Block::Bullet` drops the heading `size` (MUST-FIX, common)

A markdown heading authored as a block is stored de-bulleted as `## Title`; Tine
re-bullets to `- ## Title`. mldoc puts the heading level in `size` on the (unordered)
Heading; lsdoc's `Bullet` variant has no `size` field, so **every `#`…`######` heading
block loses its level.**

Evidence (5/8 diffs — every heading-block in the corpus):

| input | mldoc | lsdoc v0.1.0 |
|---|---|---|
| `- # Heading one` | `Heading{unordered:true, size:1, level:1}` | `Bullet{level:1}` — **no size** |
| `- ## Heading two` | `Heading{unordered:true, size:2, level:1}` | `Bullet{level:1}` — **no size** |
| `- ###### Heading six` | `Heading{unordered:true, size:6, level:1}` | `Bullet{level:1}` — **no size** |

**Target:** `Block::Bullet` carries `size: Option<u32>` = mldoc's `size` (the `#` count,
1–6; `None` when the bullet is not a heading), mirroring `Block::Heading.size`. A renderer
then does: `if let Some(n) = bullet.size { render as h{n} } else { normal }` — exactly how
OG reads `size` off the heading node.

**Validate — and mind the trap:** the gate almost certainly passes today *because*
`normalize.mjs` also drops `size` on unordered headings (so mldoc and lsdoc agree at
"absent"). Fixing this means **adding `size` to the compared projection** (normalize.mjs +
projection.rs) so the gate actually verifies it — otherwise it stays green while still
dropping the field (the "additive incompleteness" trap RENDER-PARITY §1 warned about). Add
a `- #`…`- ######` set to the corpus and require 0-diff with `size` present.

**AST-shape note:** adding an optional `size` to `Bullet` is **additive** (a new
`skip_serializing_if` field), not a rename/removal — consistent with the v0.1.0 handoff
rule. Tine consumes it on the version bump with no code change beyond reading it.

---

## Gap 2 — bullet-line block-openers fold into the bullet inline (fix: match mldoc)

When the post-marker content of a bullet line is itself a **block-level** construct,
mldoc emits `[empty bullet, <that block>]` (splits); lsdoc keeps a single `Bullet` and
folds the construct into the bullet's **inline**. Only three openers diverge — lsdoc
splits `- ```code```, `- |table|`, `- > quote`, `- * list` correctly.

Evidence (3/8 diffs):

| input | mldoc | lsdoc v0.1.0 | render impact |
|---|---|---|---|
| `- $$ E = mc^2 $$` | `[Heading(empty), Displayed_Math]` | `[Bullet{inline:[Latex mode:Displayed]}]` | **benign** — renders as display math either way |
| `- ---` | `[Heading(empty), Horizontal_Rule]` | `[Bullet{inline:[Plain "---"]}]` | **wrong** — literal `---` text, not a rule |
| `- [^1]: the footnote body` | `[Heading(empty), Footnote_Definition]` | `[Bullet{inline:[Fnref "1", Plain ": the footnote body"]}]` | **wrong** — footnote *ref*+text, not a *definition* |

Root cause: mldoc treats `$$` / `---` / `[^id]:` in immediate post-marker position as
sibling block openers (bullet title empty); lsdoc consumes the rest of the bullet line as
inline.

**Decision: fix it — match mldoc (= OG).** Default is OG parity unless something is a bug
or explicitly out of scope; none of these qualifies. `- ---` in particular is a common,
deliberate divider (easy to type, visually useful), so the literal-`---` rendering is a
real defect, not an edge case. When the bullet-line tail is a block-level opener, emit it
as a sibling block exactly as mldoc does — for **all** such openers: at least `$$…$$`,
`---`, `[^id]:`, plus whatever else an audit of post-marker block-openers turns up (the
already-correct `code`/`table`/`quote`/`list` cases show the split machinery exists; these
three are the ones that slip through). Match mldoc's split (empty bullet title + sibling
block) so OG's `parse-title-and-body` "node 0 = title, rest = body" maps 1:1. (`$$` already
renders acceptably via the folded display-latex, so it's the lowest *severity* of the
three — but still fix it for parity; don't special-case it out.)

---

## Definition of done (v0.1.1)

- Gap 1 fixed: `Bullet.size` carried **and gated** (the projection compares it; corpus
  includes `- #`…`- ######`; 0-diff).
- Gap 2 fixed: post-marker block-openers (`$$…$$`, `---`, `[^id]:`, + any others found)
  split into a sibling block matching mldoc, gated 0-diff (corpus includes `- ---`,
  `- $$ x $$`, `- [^1]: body`).
- Re-run the full gate (corpus + real graphs + fuzz + perf) green; cut **v0.1.1**.
- Tine then bumps the pin `tag = "v0.1.1"` and proceeds to the render cutover.

The AST *shape* stays compatible (only an additive `Bullet.size`); no renamed tags or
removed variants, so this is a value+one-additive-field bump, not a breaking change.

// Differential comparison: diff lsdoc's projection against the mldoc oracle's,
// per corpus input. Reports two independent signals — the OG-faithful ref set and
// the full block tree — and writes divergences.json for drill-down.
//
// Object-key order is irrelevant (serde vs JS emit different orders), so we
// compare a key-sorted canonical JSON string. Usage: node compare.mjs
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { canonJSON } from "./lib/compare.mjs";
const __dir = dirname(fileURLToPath(import.meta.url));

// Intentional-deviation allowlist: ids we knowingly don't match (documented in
// DECISIONS.md). Excluded from diff counts but still reported. { id, reason }.
const allowPath = join(__dir, "allowlist.json");
const allow = existsSync(allowPath)
  ? Object.fromEntries(JSON.parse(readFileSync(allowPath, "utf8")).map((a) => [a.id, a.reason]))
  : {};

// Canonical stringify (key-sorted, drops span/aligns) — see lib/compare.mjs for
// the shared definition and the rationale for the ignored keys.
const s = canonJSON;

// Structural skeleton: a block's shape WITHOUT inline content, so block structure
// (M2) can be gated independently of inline parsing (M3/M4). Keeps kind, level,
// ordered/size, src lang+code, properties, span, and nesting; drops `inline`
// arrays and reduces table cells to dimensions.
function skel(b) {
  if (!b || typeof b !== "object") return b;
  const o = { kind: b.kind };
  for (const k of ["level", "size", "lang", "code", "props", "span", "name", "htags", "text", "marker", "priority", "value", "content"]) {
    if (k in b) o[k] = b[k];
  }
  if (b.children) o.children = b.children.map(skel);
  if (b.items) o.items = b.items.map(skelItem);
  if (b.kind === "table") {
    o.header = b.header ? b.header.length : null;
    o.rows = (b.rows ?? []).map((r) => r.length);
  }
  return o;
}
// A list item's structural skeleton: ordered/number/indent + block-shaped `content`
// + recursively-nested `items` (list-items, recurse via skelItem — NOT skel). The
// def-list term `name` is kept by the parent skel's key list when present.
function skelItem(it) {
  return {
    ordered: it.ordered, number: it.number, indent: it.indent,
    content: (it.content ?? []).map(skel), items: (it.items ?? []).map(skelItem),
  };
}
const skels = (blocks) => (blocks ?? []).map(skel);

const oracle = JSON.parse(readFileSync(join(__dir, "oracle-out.json"), "utf8"));
const lsdoc = JSON.parse(readFileSync(join(__dir, "lsdoc-out.json"), "utf8"));
const byId = Object.fromEntries(lsdoc.map((x) => [x.id, x]));

let refsOk = 0, structOk = 0, blocksOk = 0, missing = 0;
const refDiffs = [], structDiffs = [], blockDiffs = [];
const allowedHit = []; // allowlisted ids that did diverge (expected)

// Optional filter: `node compare.mjs --cat=tag` limits the shown diffs to a corpus
// category (ids are stable; category lookup via the corpus file would need a join,
// so we filter on a substring of the input instead via --grep).
const grep = (process.argv.find((a) => a.startsWith("--grep=")) || "").slice(7);

const oracleErrs = [];
for (const o of oracle) {
  const l = byId[o.id];
  if (!l) { missing++; continue; }
  // Oracle parse error — FAIL CLOSED (audit4 C4: skipping shrank the denominator
  // and the run exited green while lsdoc owned the input with an unchecked result).
  if (o.err || !o.projection) { oracleErrs.push({ id: o.id, input: o.input, err: o.err }); continue; }
  const op = o.projection, lp = l.projection;

  const allowed = o.id in allow;

  if (s(op.refs) === s(lp.refs)) refsOk++;
  else if (!allowed) refDiffs.push({ id: o.id, input: o.input, oracle: op.refs, lsdoc: lp.refs });

  const structDiff = s(skels(op.blocks)) !== s(skels(lp.blocks));
  const blockDiff = s(op.blocks) !== s(lp.blocks);
  if (!structDiff) structOk++;
  else if (!allowed) structDiffs.push({ id: o.id, input: o.input, oracle: skels(op.blocks), lsdoc: skels(lp.blocks) });
  if (!blockDiff) blocksOk++;
  else if (!allowed) blockDiffs.push({ id: o.id, input: o.input, oracle: op.blocks, lsdoc: lp.blocks });

  if (allowed && (structDiff || blockDiff || s(op.refs) !== s(lp.refs))) {
    allowedHit.push({ id: o.id, reason: allow[o.id] });
  }
}

const total = oracle.filter((o) => !o.err && o.projection).length;
writeFileSync(join(__dir, "divergences.json"),
  JSON.stringify({ summary: { total, refsOk, structOk, blocksOk, missing }, refDiffs, structDiffs, blockDiffs }, null, 1));

console.log(`\n=== differential summary (${total} inputs) ===`);
console.log(`  refs       match: ${refsOk}/${total}   (${refDiffs.length} diffs)`);
console.log(`  block-struct match: ${structOk}/${total}   (${structDiffs.length} diffs)   [M2 gate]`);
console.log(`  blocks-full  match: ${blocksOk}/${total}   (${blockDiffs.length} diffs)   [M3/M4 gate]`);
if (missing) console.log(`  MISSING from lsdoc output: ${missing}`);
if (oracleErrs.length) {
  console.log(`  ORACLE ERRORS (fail-closed): ${oracleErrs.length}`);
  for (const e of oracleErrs.slice(0, 12)) console.log(`    ${e.id}  ${JSON.stringify(e.input).slice(0, 160)}  — ${e.err}`);
}
if (allowedHit.length) {
  console.log(`  allowlisted deviations (excluded): ${allowedHit.length}`);
  for (const a of allowedHit) console.log(`    ${a.id} — ${a.reason}`);
}

const show = (label, arr, fmt) => {
  const f = grep ? arr.filter((d) => d.input.includes(grep)) : arr;
  if (!f.length) return;
  console.log(`\n--- first ${Math.min(12, f.length)} ${label} diffs${grep ? ` (grep ${JSON.stringify(grep)})` : ""} ---`);
  for (const d of f.slice(0, 12)) {
    console.log(`  ${d.id}  ${JSON.stringify(d.input)}`);
    console.log(`    oracle: ${fmt(d.oracle)}`);
    console.log(`    lsdoc : ${fmt(d.lsdoc)}`);
  }
};
// Default view focuses on the current gate: structure first, then refs.
show("block-struct", structDiffs, (b) => JSON.stringify(b).slice(0, 240));
show("ref", refDiffs, (r) => JSON.stringify(r));

// Exit non-zero if anything diverges — including oracle errors, which are a
// gate defect (unverifiable input), never a pass.
process.exit(refDiffs.length + blockDiffs.length + missing + oracleErrs.length === 0 ? 0 : 1);

// Differential comparison: diff lsdoc's projection against the mldoc oracle's,
// per corpus input. Reports two independent signals — the OG-faithful ref set and
// the full block tree — and writes divergences.json for drill-down.
//
// Object-key order is irrelevant (serde vs JS emit different orders), so we
// compare a key-sorted canonical JSON string. Usage: node compare.mjs
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
const __dir = dirname(fileURLToPath(import.meta.url));

// Stable stringify: recursively sort object keys so comparison is order-insensitive.
function canon(v) {
  if (Array.isArray(v)) return v.map(canon);
  if (v && typeof v === "object") {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = canon(v[k]);
    return o;
  }
  return v;
}
const s = (v) => JSON.stringify(canon(v));

const oracle = JSON.parse(readFileSync(join(__dir, "oracle-out.json"), "utf8"));
const lsdoc = JSON.parse(readFileSync(join(__dir, "lsdoc-out.json"), "utf8"));
const byId = Object.fromEntries(lsdoc.map((x) => [x.id, x]));

let refsOk = 0, blocksOk = 0, missing = 0;
const refDiffs = [], blockDiffs = [];

for (const o of oracle) {
  const l = byId[o.id];
  if (!l) { missing++; continue; }
  if (o.err || !o.projection) continue; // oracle parse error — skip
  const op = o.projection, lp = l.projection;

  const refMatch = s(op.refs) === s(lp.refs);
  const blockMatch = s(op.blocks) === s(lp.blocks);
  if (refMatch) refsOk++;
  else refDiffs.push({ id: o.id, input: o.input, oracle: op.refs, lsdoc: lp.refs });
  if (blockMatch) blocksOk++;
  else blockDiffs.push({ id: o.id, input: o.input, oracle: op.blocks, lsdoc: lp.blocks });
}

const total = oracle.filter((o) => !o.err && o.projection).length;
writeFileSync(join(__dir, "divergences.json"),
  JSON.stringify({ summary: { total, refsOk, blocksOk, missing }, refDiffs, blockDiffs }, null, 1));

console.log(`\n=== differential summary (${total} inputs) ===`);
console.log(`  refs   match: ${refsOk}/${total}   (${refDiffs.length} diffs)`);
console.log(`  blocks match: ${blocksOk}/${total}   (${blockDiffs.length} diffs)`);
if (missing) console.log(`  MISSING from lsdoc output: ${missing}`);

const show = (label, arr, fmt) => {
  if (!arr.length) return;
  console.log(`\n--- first ${Math.min(10, arr.length)} ${label} diffs ---`);
  for (const d of arr.slice(0, 10)) {
    console.log(`  ${d.id}  ${JSON.stringify(d.input)}`);
    console.log(`    oracle: ${fmt(d.oracle)}`);
    console.log(`    lsdoc : ${fmt(d.lsdoc)}`);
  }
};
show("ref", refDiffs, (r) => JSON.stringify(r));
show("block", blockDiffs, (b) => JSON.stringify(b).slice(0, 200));

// Exit non-zero if anything diverges, so the runner can gate on it.
process.exit(refDiffs.length + blockDiffs.length + missing === 0 ? 0 : 1);

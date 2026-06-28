// Compare Rust (refs.rs) vs TS (parseInline) [Prong A] and each vs mldoc 1.5.7
// [Prong B] over the corpus. Emits classified divergence tables + divergences.json.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dir = dirname(fileURLToPath(import.meta.url));
const r = (f) => JSON.parse(readFileSync(join(__dir, f), "utf8"));
const corpus = r("corpus.json");
const ts = Object.fromEntries(r("ts-out.json").map((x) => [x.id, x]));
const rust = Object.fromEntries(r("rust-out.json").map((x) => [x.id, x]));
const hasMldoc = existsSync(join(__dir, "mldoc-out.json"));
const mldoc = hasMldoc ? Object.fromEntries(r("mldoc-out.json").map((x) => [x.id, x])) : null;

const UUID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const isUuid = (s) => UUID.test(s.trim());
const trimAll = (a) => a.map((x) => x.trim());
const eqList = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
const dedupe = (a) => [...new Set(a)];
const esc = (s) => JSON.stringify(s);

// ---------- Prong A: Rust vs TS ----------
const pageDiv = [], blockDiv = [];
for (const c of corpus) {
  const t = ts[c.id], u = rust[c.id];
  if (!eqList(u.page, t.page)) pageDiv.push({ id: c.id, cat: c.cat, input: c.input, rust: u.page, ts: t.page });
  const tsBlockGated = dedupe(trimAll(t.block).filter(isUuid));
  if (!eqList(u.block_ids, tsBlockGated))
    blockDiv.push({ id: c.id, cat: c.cat, input: c.input, rust: u.block_ids, ts: tsBlockGated, ts_raw: t.block, macro: t.macro });
}

// ---------- Prong B: Tine vs mldoc (OG-faithful oracle) ----------
let pageVsMldoc = [], blockVsMldoc = [];
const classify = (u, t, m) => {
  const rustOk = eqList(u, m), tsOk = eqList(t, m);
  if (rustOk && tsOk) return null;
  if (eqList(u, t)) return "BOTH_TINE_AGREE_≠_OG"; // silent OG-parity bug (both wrong)
  if (tsOk) return "TS=OG, RUST_WRONG";
  if (rustOk) return "RUST=OG, TS_WRONG";
  return "ALL_THREE_DIFFER";
};
if (hasMldoc) {
  for (const c of corpus) {
    const u = rust[c.id].page, t = ts[c.id].page, m = mldoc[c.id].og_page;
    const cls = classify(u, t, m);
    if (cls) pageVsMldoc.push({ id: c.id, cat: c.cat, input: c.input, rust: u, ts: t, mldoc: m, cls });
    // block refs: rust block_ids vs TS blockref(uuid-gated,dedupe) vs OG og_block
    const ub = rust[c.id].block_ids, tb = dedupe(trimAll(ts[c.id].block).filter(isUuid)), mb = mldoc[c.id].og_block;
    const bcls = classify(ub, tb, mb);
    if (bcls) blockVsMldoc.push({ id: c.id, cat: c.cat, input: c.input, rust: ub, ts: tb, mldoc: mb, cls: bcls });
  }
}

const summary = {
  total: corpus.length,
  prongA_pageDivergences: pageDiv.length,
  prongA_blockDivergences_uuidGated: blockDiv.length,
  prongB_ran: hasMldoc,
  prongB_pageVsMldoc: pageVsMldoc.length,
  prongB_blockVsMldoc: blockVsMldoc.length,
};
console.log(JSON.stringify(summary, null, 2));
writeFileSync(join(__dir, "divergences.json"), JSON.stringify({ summary, pageDiv, blockDiv, pageVsMldoc, blockVsMldoc }, null, 1));

console.log("\n=== PRONG A: PAGE-REF DIVERGENCES (Rust refs.rs vs TS parseInline) ===");
for (const d of pageDiv) console.log(`${d.id} [${d.cat}] ${esc(d.input)}  RUST=${esc(d.rust)} TS=${esc(d.ts)}`);
console.log("\n=== PRONG A: BLOCK-REF (uuid-gated) DIVERGENCES ===");
for (const d of blockDiv) console.log(`${d.id} [${d.cat}] ${esc(d.input)}  RUST=${esc(d.rust)} TS=${esc(d.ts)} macro=${esc(d.macro)}`);

if (hasMldoc) {
  const ORDER = ["BOTH_TINE_AGREE_≠_OG", "TS=OG, RUST_WRONG", "RUST=OG, TS_WRONG", "ALL_THREE_DIFFER"];
  const dump = (title, rows) => {
    console.log(`\n=== ${title} ===`);
    for (const cls of ORDER) {
      const rs = rows.filter((d) => d.cls === cls);
      if (!rs.length) continue;
      console.log(`\n-- ${cls} (${rs.length}) --`);
      for (const d of rs) console.log(`${d.id} [${d.cat}] ${esc(d.input)}  RUST=${esc(d.rust)} TS=${esc(d.ts)} OG=${esc(d.mldoc)}`);
    }
  };
  dump("PRONG B: PAGE-REF Tine vs OG(mldoc) — classified", pageVsMldoc);
  dump("PRONG B: BLOCK-REF Tine vs OG(mldoc) — classified", blockVsMldoc);
}

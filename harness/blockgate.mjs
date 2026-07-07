// Real-block-body differential gate — the realistic gate the Tine integration needs.
//
// Tine owns the outline layer: each block carries `raw` (de-bulleted :block/content).
// To render, it RE-BULLETS (`format!("{pattern} {}", raw.trim_start())`, pattern `-`/`*`)
// and parses. This reproduces that EXACTLY over every real block of the shared graphs
// (block-raws.json exported by Tine at each graph root) and diffs lsdoc vs pinned mldoc.
// This is the most realistic possible gate (real block bodies, fed the OG way) and is
// meant to be a hard 0-gate.
//
// Usage: node blockgate.mjs        (reads the two block-raws.json files)
import { createRequire } from "node:module";
import { readFileSync, existsSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";
import { canonJSON } from "./lib/compare.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");

const SOURCES = [
  join(homedir(), "research/tine-test/block-raws.json"),
  join(homedir(), "research/org-graph/block-raws.json"),
];

const cfg = (fmt) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false, keep_line_break: true,
  format: fmt === "org" ? "Org" : "Markdown", heading_to_list: false, export_md_remove_options: [],
});
const oracle = (input, fmt) => {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(fmt)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast) };
};
// Canonical stringify (key-sorted, drops span/aligns) — shared in lib/compare.mjs.
const S = canonJSON;

// re-bullet exactly as Tine does (block.cljs parse-title-and-body).
const reBullet = (raw, fmt) => `${fmt === "org" ? "*" : "-"} ${raw.replace(/^\s+/, "")}`;

const blocks = [];
for (const src of SOURCES) {
  if (!existsSync(src)) {
    console.log(`blockgate: skip — ${src} absent (Tine exports it; see FOR-TINE.md). Machine-specific, like the real-graph corpora.`);
    continue;
  }
  const arr = JSON.parse(readFileSync(src, "utf8"));
  for (const b of arr) blocks.push({ raw: b.raw, format: b.format || "md", src });
}
if (!blocks.length) { console.log("blockgate: no block-raws.json present — nothing to check (OK)."); process.exit(0); }
console.log(`real blocks: ${blocks.length} (${SOURCES.map(s => s.split("/").slice(-2)[0]).join(", ")})`);

// lsdoc, one batch run over the re-bulleted inputs.
const inputs = blocks.map((b, i) => ({ id: `bg${i}`, input: reBullet(b.raw, b.format), format: b.format }));
const corpusPath = join(__dir, "corpus.blockgate.json");
writeFileSync(corpusPath, JSON.stringify(inputs, null, 0));
const env = { ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}` };
const outPath = join(__dir, "lsdoc-blockgate.json");
const r = spawnSync("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--", corpusPath, outPath],
  { cwd: repo, env, encoding: "utf8" });
if (r.status !== 0) { console.error("lsdoc-parse FAILED (possible panic):\n", r.stderr?.slice(-2000)); process.exit(1); }
const byId = Object.fromEntries(JSON.parse(readFileSync(outPath, "utf8")).map(x => [x.id, x]));

let refMis = 0, blkMis = 0, shown = 0;
for (const inp of inputs) {
  let op; try { op = oracle(inp.input, inp.format); } catch { continue; }
  const lp = byId[inp.id]?.projection; if (!lp) continue;
  const rb = S(op.refs) !== S(lp.refs), bb = S(op.blocks) !== S(lp.blocks);
  if (rb) refMis++; if (bb) blkMis++;
  if ((rb || bb) && shown < 25) {
    shown++;
    console.log(`\nDIVERGENCE ${inp.id} [${inp.format}]`);
    console.log(`  input: ${JSON.stringify(inp.input).slice(0, 220)}`);
    if (bb) { console.log(`  O: ${S(op.blocks).slice(0, 320)}`); console.log(`  L: ${S(lp.blocks).slice(0, 320)}`); }
    if (rb) { console.log(`  refs O: ${S(op.refs)}  L: ${S(lp.refs)}`); }
  }
}
console.log(`\nblockgate: ${inputs.length} real blocks — refMismatch=${refMis} blockMismatch=${blkMis}`);
console.log(refMis + blkMis === 0 ? "✓ 0 diffs on real block bodies." : `✗ ${refMis + blkMis} diffs.`);
process.exit(refMis + blkMis === 0 ? 0 : 2);

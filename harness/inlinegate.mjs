// Inline differential gate: lsdoc `inline()` vs mldoc `parseInlineJson` (the `inline->edn` /
// OG `inline-text` path) over corpus.inline.json. This gates the public `lsdoc::inline`
// entrypoint to byte-exact mldoc INLINE parity — distinct from run.mjs/blockgate (which gate
// the BLOCK parser). Reads corpus.inline.json, runs lsdoc with LSDOC_INLINE=1, runs mldoc
// parseInlineJson, normalizes both via normalize.mjs, compares. Non-zero exit on any diff.
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { normInline, cleanInlines } from "./lib/normalize.mjs";
import { canonJSON } from "./lib/compare.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");

const corpusPath = join(__dir, "corpus.inline.json");
if (!existsSync(corpusPath)) {
  console.log("inlinegate: corpus.inline.json absent — skipping (run corpus.inline.gen.mjs).");
  process.exit(0);
}
const corpus = JSON.parse(readFileSync(corpusPath, "utf8"));

const cfg = (f) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false, keep_line_break: true,
  format: f === "org" ? "Org" : "Markdown", heading_to_list: false, export_md_remove_options: [],
});

// lsdoc side: build+run the inline entrypoint (LSDOC_INLINE=1). Toolchain on /aux (mirror run.mjs).
const outPath = join(__dir, "inline-lsdoc-out.json");
const cargoEnv = {
  ...process.env,
  LSDOC_INLINE: "1",
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
const r = spawnSync("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--", corpusPath, outPath],
  { cwd: repo, env: cargoEnv, stdio: ["ignore", "ignore", "inherit"] });
if (r.status !== 0) { console.error(`inlinegate: lsdoc run failed (exit ${r.status})`); process.exit(r.status ?? 1); }
const lsd = Object.fromEntries(JSON.parse(readFileSync(outPath, "utf8")).map((x) => [x.id, x.inline]));

// Canonical stringify (key-sorted, drops span/aligns) — shared in lib/compare.mjs.
// Inline nodes carry no span/aligns, so the ignore filter is a harmless no-op here.
const S = canonJSON;

let ok = 0;
const diffs = [];
for (const it of corpus) {
  const mldoc = cleanInlines(JSON.parse(Mldoc.parseInlineJson(it.input, cfg(it.format))).map(normInline));
  const ours = cleanInlines(lsd[it.id] ?? []);
  if (S(mldoc) === S(ours)) ok++;
  else diffs.push({ id: it.id, input: it.input, format: it.format, mldoc: S(mldoc), lsdoc: S(ours) });
}

console.log(`inlinegate: ${ok}/${corpus.length} inline runs match mldoc parseInlineJson  (${diffs.length} diffs)`);
if (diffs.length) {
  writeFileSync(join(__dir, "inline-divergences.json"), JSON.stringify(diffs, null, 1));
  for (const d of diffs.slice(0, 20)) {
    console.log(`\nDIFF ${d.id} [${d.format}] ${JSON.stringify(d.input)}`);
    console.log(`  mldoc: ${d.mldoc.slice(0, 200)}`);
    console.log(`  lsdoc: ${d.lsdoc.slice(0, 200)}`);
  }
  process.exit(1);
}
console.log("✓ inline() == mldoc inline->edn on the inline corpus.");

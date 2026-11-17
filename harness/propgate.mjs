// Property-reference gate: lsdoc `property_references` vs mldoc `getReferences`
// (`Property.property_references`: a property value parsed with macros disabled, keeping
// its top-level Tag / Link / Nested_link nodes) over corpus.property.json. mldoc leaks
// state across parses in one process (CLAUDE.md), so every value is read in a FRESH
// oracle process. lsdoc's top-level reference nodes are compared after the inline
// normalizer both gates share. Non-zero exit on any unregistered diff, or on a registered
// one that now matches (drop it from KNOWN).
import { spawnSync } from "node:child_process";
import { readFileSync, existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { normInline, cleanInlines } from "./lib/normalize.mjs";
import { canonJSON } from "./lib/compare.mjs";

const cfg = (f) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false, keep_line_break: true,
  format: f === "org" ? "Org" : "Markdown", heading_to_list: false, export_md_remove_options: [],
});

if (process.argv[2] === "one") {
  const { Mldoc } = createRequire(import.meta.url)("mldoc");
  const [format, input] = JSON.parse(process.argv[3]);
  process.stdout.write(Mldoc.getReferences(input, cfg(format)) || "[]");
  process.exit(0);
}

// Known lsdoc differences, each with the mldoc function lsdoc does not transcribe.
const ESCAPE = "md escape: mldoc's property parse keeps `\\_` (Type_op.md_unescaped runs only in "
  + "Mldoc_parser.parse); lsdoc's Markdown inline resolver unescapes while parsing";
// Keyed by `format:input`.
const KNOWN = new Map([
  ["md:#a\\_b", ESCAPE], ["md:[[a\\_b]]", ESCAPE], ["md:#[[a\\_b]]", ESCAPE],
  ["org:![x](../assets/a.png)", "org `!`: mldoc inline.ml `'!' -> markdown_image config`; "
    + "lsdoc's Org resolver swallows `!` as plain"],
  // Not a reference difference (the top-level node is the same https link): inside its label.
  ["md:[**[[a]]**](https://a.example)", "md link label: mldoc inline.ml `markdown_link` "
    + "re-reads label Plains with emphasis/latex/entity/code/sub/superscript only, so `[[a]]` "
    + "inside the label's emphasis stays Plain; lsdoc resolves it as a page ref (doc and inline "
    + "parses alike)"],
]);

const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");
const corpusPath = join(__dir, "corpus.property.json");
if (!existsSync(corpusPath)) {
  console.error("propgate: corpus.property.json absent (run corpus.property.gen.mjs)");
  process.exit(1);
}
const corpus = JSON.parse(readFileSync(corpusPath, "utf8"));

const outPath = join(mkdtempSync(join(tmpdir(), "propgate-")), "lsdoc.json");
const cargoEnv = {
  ...process.env,
  LSDOC_INLINE: "property",
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
const r = spawnSync("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--", corpusPath, outPath],
  { cwd: repo, env: cargoEnv, stdio: ["ignore", "ignore", "inherit"] });
if (r.status !== 0) { console.error(`propgate: lsdoc run failed (exit ${r.status})`); process.exit(1); }
const lsd = Object.fromEntries(JSON.parse(readFileSync(outPath, "utf8")).map((x) => [x.id, x.inline]));

const REFERENCE = new Set(["tag", "link", "nested_link"]);
let ok = 0;
const diffs = [];
const stale = [];
for (const it of corpus) {
  const one = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "one",
    JSON.stringify([it.format, it.input])], { encoding: "utf8" });
  if (one.status !== 0) { diffs.push({ ...it, mldoc: `ORACLE-ERROR ${one.stderr}`, lsdoc: "" }); continue; }
  const mldoc = canonJSON(cleanInlines(JSON.parse(one.stdout).map(normInline)));
  const ours = canonJSON(cleanInlines(structuredClone((lsd[it.id] ?? []).filter((n) => REFERENCE.has(n.k)))));
  const same = mldoc === ours;
  const key = `${it.format}:${it.input}`;
  if (KNOWN.has(key)) { if (same) stale.push(key); else ok++; continue; }
  if (same) ok++;
  else diffs.push({ ...it, mldoc, lsdoc: ours });
}
console.log(`propgate: ${ok}/${corpus.length} accepted; known=${KNOWN.size} unclassified=${diffs.length} stale=${stale.length}`);
for (const [id, why] of KNOWN) console.log(`  ${id} — ${why}`);
for (const d of diffs.slice(0, 20)) {
  console.log(`\nDIFF ${d.id} [${d.format}] ${JSON.stringify(d.input)}`);
  console.log(`  mldoc: ${d.mldoc.slice(0, 240)}`);
  console.log(`  lsdoc: ${d.lsdoc.slice(0, 240)}`);
}
if (diffs.length) {
  writeFileSync(join(__dir, "property-divergences.json"), JSON.stringify(diffs, null, 1));
  process.exit(1);
}
if (stale.length) { console.error(`propgate: now matching, drop from KNOWN: ${stale.join(", ")}`); process.exit(1); }
console.log("✓ property-reference corpus has zero unclassified diffs.");

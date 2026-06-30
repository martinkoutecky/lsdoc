// Quick differential fuzz: generate biased-random markdown, run both mldoc (oracle)
// and lsdoc, compare the observable projection. Reports mismatches + any panics.
// Usage: node fuzz.mjs [count] [seed]
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { writeFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";
import { canonJSON } from "./lib/compare.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
// Format from argv[4] ("md" default, "org"): `node fuzz.mjs 20000 1 org`.
const FORMAT = process.argv[4] === "org" ? "org" : "md";
const MLDOC_CFG = JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: FORMAT === "org" ? "Org" : "Markdown",
  heading_to_list: false, export_md_remove_options: [],
});
const parseToProjection = (input) => {
  const ast = JSON.parse(Mldoc.parseJson(input, MLDOC_CFG));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast) };
};

const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");
const N = parseInt(process.argv[2] || "3000", 10);
let seed = parseInt(process.argv[3] || "12345", 10);
const rng = () => {
  seed = (seed * 1103515245 + 12345) & 0x7fffffff;
  return seed / 0x7fffffff;
};
const pick = (a) => a[Math.floor(rng() * a.length)];

// token alphabet biased toward the adversarial inline band, per format.
const TOKENS_MD = [
  "*", "**", "***", "_", "__", "~~", "==", "^^", "`", "``",
  "[[", "]]", "((", "))", "{{", "}}", "[", "]", "(", ")", "{", "}",
  "#", "#tag", "[[Foo]]", "((11111111-1111-1111-1111-111111111111))",
  "[label]", "](url)", "{{embed ", "{{query ", "https://x.com/a", "http://y.org",
  "\\", "\\[", "\\#", "\\`", "$", "$x$", "$$", "!", "![a]", "<", ">", "<https://z.io>",
  "a", "b", " ", "  ", "\n", "café", "中文", "😀", ".", ",", "!", ":", "-", "/",
  "TODO ", "[#A] ", "[ ] ", "\t", "word", "x", "#[[", "tag", "::",
];
const TOKENS_ORG = [
  "* ", "** ", "*** ", "*", "/", "_", "+", "~", "=", "^", "^^",
  "[[", "]]", "][", "[[target]]", "[[t][l]]", "[fn:1]", "<2026-06-26 Fri>", "[2026-06-20 Sat]",
  "#+TITLE: ", "#+BEGIN_SRC ", "#+END_SRC", "#+BEGIN_QUOTE", "#+END_QUOTE", "#+NAME: ",
  ":PROPERTIES:", ":key: value", ":END:", "SCHEDULED: ", "DEADLINE: ",
  "TODO ", "DONE ", "[#A] ", ":tag1:tag2:", "- ", "+ ", "1. ", "| a | b |",
  "\\", "a", "b", " ", "  ", "\n", "café", "中文", "😀", ".", "/", "_x", "^y", "word",
];
const TOKENS = FORMAT === "org" ? TOKENS_ORG : TOKENS_MD;

function genInput() {
  const len = 1 + Math.floor(rng() * 14);
  let s = "";
  for (let i = 0; i < len; i++) s += pick(TOKENS);
  return s;
}

// Canonical stringify (key-sorted, drops span/aligns) — shared in lib/compare.mjs.
const S = canonJSON;

// build the corpus, run lsdoc once over all of it.
const inputs = [];
for (let i = 0; i < N; i++) inputs.push({ id: `f${i}`, input: genInput(), format: FORMAT });
const corpusPath = join(__dir, "corpus.fuzz.json");
writeFileSync(corpusPath, JSON.stringify(inputs, null, 0));

const env = {
  ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
const outPath = join(__dir, "lsdoc-fuzz.json");
const r = spawnSync("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--", corpusPath, outPath],
  { cwd: repo, env, encoding: "utf8" });
if (r.status !== 0) {
  console.error("lsdoc-parse FAILED (possible panic):\n", r.stderr?.slice(-2000));
  process.exit(1);
}
const lsdoc = JSON.parse(readFileSync(outPath, "utf8"));
const byId = Object.fromEntries(lsdoc.map((x) => [x.id, x]));

let refMismatch = 0, blockMismatch = 0, shown = 0;
for (const c of inputs) {
  let op;
  try { op = parseToProjection(c.input); } catch { continue; }
  const lp = byId[c.id]?.projection;
  if (!lp) continue;
  const rb = S(op.refs) !== S(lp.refs);
  const bb = S(op.blocks) !== S(lp.blocks);
  if (rb) refMismatch++;
  if (bb) blockMismatch++;
  if ((rb || bb) && shown < 25) {
    shown++;
    console.log(`MISMATCH ${c.id} ${JSON.stringify(c.input)}`);
    if (rb) console.log(`  refs   O:${S(op.refs)}\n         L:${S(lp.refs)}`);
    if (bb) console.log(`  blocks O:${S(op.blocks).slice(0, 400)}\n         L:${S(lp.blocks).slice(0, 400)}`);
  }
}
console.log(`\nfuzz ${N}: refMismatch=${refMismatch} blockMismatch=${blockMismatch}`);
process.exit(refMismatch + blockMismatch === 0 ? 0 : 2);

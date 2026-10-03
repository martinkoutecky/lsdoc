// Short-string sweep over v2 ownership and mldoc agreement.
//
// Generates every string (exhaustive) or random strings (seeded) over a small alphabet that
// includes `\r`, runs them through `lsdoc-parse --engine v2-try` (never aborts: reports
// `none` / `panic` for ownership gaps) and the mldoc oracle, and prints:
//   - UNOWNED: v2 returned None or panicked (the Tine-reachable abort),
//   - DIFF:    v2 owns it but its projection differs from mldoc's.
// Oracle results are re-verified in a FRESH process per case (mldoc leaks global state across
// parses; see CLAUDE.md), so every printed finding is an isolated-oracle finding.
//
// Usage: node sweep.mjs exhaustive <maxLen> [alphabet]   (alphabet: default | cr)
//        node sweep.mjs random <count> <maxLen> <seed>
//        node sweep.mjs lines <count> <maxLines> <seed>
//        env SWEEP_FORMATS=md,org (default both)
import { spawnSync, execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { writeFileSync, readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";
import { canonJSON } from "./lib/compare.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");
const BIN = join(repo, "target/release/lsdoc-parse");
const cfg = (format) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: format === "org" ? "Org" : "Markdown",
  heading_to_list: false, export_md_remove_options: [],
});
const oracle = (input, format) => {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(format)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast, format) };
};

const ALPHA = ["\r", "\n", "\t", "-", ":", "#", "*", "[", "]", "(", ")", "{", "}", "`", "$", ">", "|", " "];
const ALPHA_CR = ["\r", "\n", "-", ":", "#", "*", "a", " ", "`", "["];
const FORMATS = (process.env.SWEEP_FORMATS || "md,org").split(",");

function lsdocBatch(cases) {
  const work = mkdtempSync(join(tmpdir(), "sweep-"));
  const inP = join(work, "in.json"), outP = join(work, "out.json");
  writeFileSync(inP, JSON.stringify(cases));
  const r = spawnSync(BIN, ["--engine", "v2-try", inP, outP], { stdio: ["ignore", "ignore", "inherit"] });
  if (r.status !== 0) throw new Error("lsdoc-parse failed");
  const owned = new Map(JSON.parse(readFileSync(outP, "utf8")).map((x) => [x.id, x.projection]));
  const unowned = new Map(JSON.parse(readFileSync(outP + ".unowned.json", "utf8")).map((x) => [x.id, x.status]));
  rmSync(work, { recursive: true, force: true });
  return { owned, unowned };
}

// Fresh-process oracle: returns canonical JSON or "ERR:<msg>".
function isoOracle(input, format) {
  const code = `import {createRequire} from "node:module";const require=createRequire(${JSON.stringify(import.meta.url)});` +
    `const {Mldoc}=require("mldoc");` +
    `import {normalizeAst} from ${JSON.stringify(join(__dir, "lib/normalize.mjs"))};` +
    `import {extractRefs} from ${JSON.stringify(join(__dir, "lib/refs.mjs"))};` +
    `import {canonJSON} from ${JSON.stringify(join(__dir, "lib/compare.mjs"))};` +
    `const [i,f]=JSON.parse(process.argv[1]);const cfg=JSON.stringify({toc:false,parse_outline_only:false,heading_number:false,keep_line_break:true,format:f==="org"?"Org":"Markdown",heading_to_list:false,export_md_remove_options:[]});` +
    `try{const ast=JSON.parse(Mldoc.parseJson(i,cfg));console.log(canonJSON({blocks:normalizeAst(ast),refs:extractRefs(ast,f)}))}catch(e){console.log("ERR:"+e)}`;
  return execFileSync("node", ["--input-type=module", "-e", code, JSON.stringify([input, format])],
    { encoding: "utf8", maxBuffer: 1 << 28 }).trim();
}

function* exhaustive(alpha, maxLen) {
  for (let len = 1; len <= maxLen; len++) {
    const idx = new Array(len).fill(0);
    for (;;) {
      yield idx.map((k) => alpha[k]).join("");
      let p = len - 1;
      while (p >= 0 && ++idx[p] === alpha.length) idx[p--] = 0;
      if (p < 0) break;
    }
  }
}
function* random(count, maxLen, seed) {
  let s = seed;
  const rng = () => { s = (s * 1103515245 + 12345) & 0x7fffffff; return s / 0x7fffffff; };
  for (let n = 0; n < count; n++) {
    const len = 1 + Math.floor(rng() * maxLen);
    let t = "";
    for (let i = 0; i < len; i++) t += ALPHA[Math.floor(rng() * ALPHA.length)];
    yield t;
  }
}

// Line-level generator: 1..maxLines lines drawn from a vocabulary of block openers / property
// lines / fences / drawers / list items, each followed by a random eol from {LF, CR, CRLF, none}
// (CR over-weighted). Reaches the block-level interactions short char strings cannot.
const LINES = [
  "- ", "- a", "- s::", "- s:: v", "s::", "s:: v", "  s:: v", "tags:: x", "id:: ::}}", "a", "b c", "",
  "  ", "# h", "## h", "* h", "** h", "*", "-", "> q", ">", ">>", "- [ ] t", "- TODO x", "1. a", "+ a",
  "```", "```js", "~~~", "    code", "\t- a", "  - b", "#+BEGIN_QUOTE", "#+END_QUOTE", "#+BEGIN_SRC js",
  "#+END_SRC", "#+TITLE: t", "#+a: b", ":PROPERTIES:", ":END:", ":id: x", ":LOGBOOK:", ":CLOCK:", ": ex",
  "SCHEDULED: <2024-01-01 Mon>", "DEADLINE: <2024-01-01 Mon>", "| a | b |", "|---|---|", "|-", "---", "***",
  "<div>", "</div>", "<!--", "-->", "$$", "$$x$$", "\\begin{a}", "\\end{a}", "[:div", "]", "{{embed [[p]]}}",
  "#+BEGIN_::", "#+BEGIN_a:: b", "#+begin_::", "#+BEGIN_: b", "> #+BEGIN_::", ">#+BEGIN_::", "- #+BEGIN_::", "a:: b", "k::", ":: v",
  "[^1]: n", "[fn:1] n", "term", ": def", "[[p]]", "((x))", "<<t>>", "%%", "-- c", "# c", "::}}", "}}",
];
const EOLS = ["\n", "\r", "\r\n", "\r", ""];
function* lineMode(count, maxLines, seed) {
  let s = seed;
  const rng = () => { s = (s * 1103515245 + 12345) & 0x7fffffff; return s / 0x7fffffff; };
  const pickL = (a) => a[Math.floor(rng() * a.length)];
  for (let n = 0; n < count; n++) {
    const k = 1 + Math.floor(rng() * maxLines);
    let t = "";
    for (let i = 0; i < k; i++) t += pickL(LINES) + (i === k - 1 && rng() < 0.3 ? "" : pickL(EOLS));
    yield t;
  }
}

const mode = process.argv[2];
let gen;
if (mode === "exhaustive") gen = exhaustive(process.argv[4] === "cr" ? ALPHA_CR : ALPHA, +process.argv[3]);
else if (mode === "random") gen = random(+process.argv[3], +process.argv[4], +process.argv[5]);
else if (mode === "lines") gen = lineMode(+process.argv[3], +process.argv[4], +process.argv[5]);
else { console.error("usage: see header"); process.exit(2); }

if (process.env.SWEEP_DUMP) { // dump the generated inputs (JSON array) and exit; for bisecting a hard abort
  writeFileSync(process.env.SWEEP_DUMP, JSON.stringify([...gen]));
  process.exit(0);
}
const BATCH = 20000;
let total = 0;
const findings = { unowned: new Map(), diff: new Map() }; // key `${fmt}\0${input}`
function flush(inputs) {
  for (const format of FORMATS) {
    const cases = inputs.map((input, i) => ({ id: String(i), input, format }));
    const { owned, unowned } = lsdocBatch(cases);
    for (const [id, st] of unowned) findings.unowned.set(`${format}\0${inputs[+id]}`, st);
    for (const [id, proj] of owned) {
      const input = inputs[+id];
      let o;
      try { o = canonJSON(oracle(input, format)); } catch (e) { continue; }
      if (o === canonJSON(proj)) continue;
      // re-verify isolated
      if (isoOracle(input, format) !== canonJSON(proj)) findings.diff.set(`${format}\0${input}`, "diff");
    }
  }
  total += inputs.length;
}
let buf = [];
for (const s of gen) { buf.push(s); if (buf.length >= BATCH) { flush(buf); buf = []; } }
if (buf.length) flush(buf);

// ---- minimization: repeatedly delete one char while the same failure kind persists ----
function failsKind(inputs, format, kind) {
  const cases = inputs.map((input, i) => ({ id: String(i), input, format }));
  const { owned, unowned } = lsdocBatch(cases);
  return inputs.map((input, i) => {
    if (kind === "unowned") return unowned.has(String(i));
    if (!owned.has(String(i))) return false;
    try { return canonJSON(oracle(input, format)) !== canonJSON(owned.get(String(i))); } catch { return false; }
  });
}
function minimize(input, format, kind) {
  let cur = input;
  for (let changed = true; changed; ) {
    changed = false;
    const chars = [...cur];
    const variants = chars.map((_, i) => chars.slice(0, i).concat(chars.slice(i + 1)).join("")).filter((v) => v.length > 0);
    const f = failsKind(variants, format, kind);
    const k = f.indexOf(true);
    if (k >= 0) { cur = variants[k]; changed = true; }
  }
  return cur;
}
const MAXMIN = +(process.env.SWEEP_MINIMIZE || 150);
function report(name, m, kind) {
  const keys = [...m.keys()];
  const sample = keys.length > MAXMIN ? keys.filter((_, i) => i % Math.ceil(keys.length / MAXMIN) === 0) : keys;
  const mins = new Map();
  for (const k of sample) {
    const f = k.slice(0, k.indexOf("\0")), inp = k.slice(k.indexOf("\0") + 1);
    const mi = minimize(inp, f, kind);
    if (kind === "diff" && isoOracle(mi, f) === canonJSON(lsdocBatch([{ id: "0", input: mi, format: f }]).owned.get("0") ?? {})) continue;
    mins.set(`${f} ${JSON.stringify(mi)}`, (mins.get(`${f} ${JSON.stringify(mi)}`) || 0) + 1);
  }
  console.log(`${name}: ${keys.length} failing inputs (${sample.length} minimized) -> ${mins.size} distinct minimal`);
  for (const [k] of [...mins].sort((a, b) => a[0].length - b[0].length).slice(0, +(process.env.SWEEP_SHOW || 60))) console.log("  " + k);
}
console.log(`sweep ${mode}: ${total} inputs x ${FORMATS.join("+")}`);
report("UNOWNED", findings.unowned, "unowned");
report("DIFF", findings.diff, "diff");

// Phase 2 retool: sweep (batched) -> persist raw diffs -> group by divergence SIGNATURE ->
// isolated-verify a few representatives per class -> report class table.
// Usage: node enum-clearindents2.mjs [depthMax=3] [repsPerClass=3]
// Progress goes to stderr line-by-line (safe to redirect to a file).
import { readFileSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { canonJSON } from "./lib/compare.mjs";

const depthMax = parseInt(process.argv[2] || "3", 10);
const REPS = parseInt(process.argv[3] || "3", 10);

const WS = [" ", "\t", "\f"];
const wsRuns = [""];
for (const a of WS) {
  wsRuns.push(a);
  for (const b of WS) {
    wsRuns.push(a + b);
    for (const c of WS) wsRuns.push(a + b + c);
  }
}
const LINES = [];
for (const r of wsRuns) {
  LINES.push(r);
  LINES.push(r + "a");
}
const LINES3 = [...wsRuns, "a", " a", "  a"];
const PREFIXES = ["", " ", "  ", "\t"];

function* bodies(nLines, vocab = LINES) {
  if (nLines === 1) {
    for (const a of vocab) yield [a];
    return;
  }
  for (const rest of bodies(nLines - 1, vocab)) for (const a of vocab) yield [a, ...rest];
}
function wrap(depth, prefixes, bodyLines) {
  let openers = "",
    closers = "";
  for (let i = 0; i < depth; i++) {
    openers += prefixes[i] + "#+BEGIN_E" + i + "\n";
    closers = prefixes[i] + "#+END_E" + i + "\n" + closers;
  }
  return openers + bodyLines.map((l) => l + "\n").join("") + closers;
}
function* cases() {
  let id = 0;
  for (let depth = 1; depth <= depthMax; depth++) {
    const tuples = depth === 1 ? [[""]] : depth === 2 ? PREFIXES.map((p) => ["", p]) : [];
    if (depth === 3) for (const p of PREFIXES) for (const q of PREFIXES) tuples.push(["", p, q]);
    const sizes = depth === 1 ? [2, 3] : [2];
    for (const tuple of tuples)
      for (const n of sizes)
        for (const body of bodies(n, n === 3 ? LINES3 : LINES))
          for (const format of ["markdown", "org"])
            yield { id: "n" + id++, format, input: wrap(depth, tuple, body), depth, tuple };
  }
}

const strip = (o) => canonJSON({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });
const CHUNK = 20000;
let all = 0;
const rawDiffs = [];
let chunk = [];
function flush() {
  if (!chunk.length) return;
  writeFileSync("_enum-in.json", JSON.stringify(chunk.map(({ id, format, input }) => ({ id, format, input }))));
  execSync("cargo run -q --release --bin lsdoc-parse -- _enum-in.json _enum-ls.json", { stdio: "ignore" });
  execSync("node oracle.mjs _enum-in.json", { stdio: "ignore" });
  const L = Object.fromEntries(JSON.parse(readFileSync("_enum-ls.json", "utf8")).map((x) => [x.id, x]));
  const O = JSON.parse(readFileSync("oracle-out.json", "utf8"));
  const byId = Object.fromEntries(chunk.map((c) => [c.id, c]));
  for (const o of O) {
    const lm = strip(L[o.id].projection),
      om = strip(o.projection);
    if (lm !== om) rawDiffs.push({ ...byId[o.id], mldoc: om, lsdoc: lm });
  }
  all += chunk.length;
  process.stderr.write(`${all} enumerated, ${rawDiffs.length} raw diffs\n`);
  chunk = [];
}
for (const c of cases()) {
  chunk.push(c);
  if (chunk.length >= CHUNK) flush();
}
flush();
writeFileSync("_enum-raw.json", JSON.stringify(rawDiffs));

// signature: format + depth + a normalized "shape" of the two projections (block kinds + first
// difference position class), so identical divergence mechanics group together.
function kinds(j) {
  return (j.match(/"kind":"[a-z_]+"/g) || []).join(",");
}
function sig(d) {
  return [d.format, d.depth, kinds(d.mldoc), "||", kinds(d.lsdoc)].join(" ");
}
const classes = new Map();
for (const d of rawDiffs) {
  const k = sig(d);
  if (!classes.has(k)) classes.set(k, []);
  classes.get(k).push(d);
}
process.stderr.write(`classes: ${classes.size}\n`);

// isolated verification of representatives per class
const report = [];
for (const [k, members] of [...classes.entries()].sort((a, b) => b[1].length - a[1].length)) {
  const reps = [members[0], members[Math.floor(members.length / 2)], members[members.length - 1]]
    .filter((v, i, a) => a.indexOf(v) === i)
    .slice(0, REPS);
  let confirmed = 0,
    leak = 0;
  const samples = [];
  for (const r of reps) {
    writeFileSync("_enum-one.json", JSON.stringify([{ id: r.id, format: r.format, input: r.input }]));
    execSync("node oracle.mjs _enum-one.json", { stdio: "ignore" });
    const o = JSON.parse(readFileSync("oracle-out.json", "utf8"))[0];
    const om = strip(o.projection);
    if (om !== r.lsdoc) {
      confirmed++;
      samples.push({ input: r.input, format: r.format, mldoc: om, lsdoc: r.lsdoc });
    } else leak++;
  }
  report.push({ class: k, size: members.length, confirmed, leak, samples: samples.slice(0, 2) });
  process.stderr.write(`class(${members.length}) confirmed=${confirmed} leak=${leak}: ${k.slice(0, 110)}\n`);
}
writeFileSync("_enum-report.json", JSON.stringify(report, null, 1));
const conf = report.filter((r) => r.confirmed).reduce((a, r) => a + r.size, 0);
console.log(
  `${all} cases; ${rawDiffs.length} raw diffs in ${classes.size} classes; ~${conf} in isolated-CONFIRMED classes -> _enum-report.json`
);

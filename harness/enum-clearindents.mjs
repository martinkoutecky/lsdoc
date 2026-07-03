// D33-D37 close-out: EXHAUSTIVE differential enumeration of the frame clear-indents area.
//
// Not a sampler: enumerates every combination of a semantically complete line vocabulary
// (mldoc-ws prefixes over {' ','\t','\f'} x optional text tail) as 2-3-line bodies inside
// 1-3 nested #+BEGIN frames with every small nesting prefix, in BOTH formats. The vocabulary
// is complete for the clear-indents semantics because the per-line behavior depends only on
// (mldoc-ltrim prefix length, whole-line-ws?, length, byte content of the prefix) and the
// frame behavior on the first line's space/tab prefix — all realized within length <= 4.
//
// Method: run lsdoc + the BATCHED oracle over big chunks (fast), then RE-VERIFY every diff
// with the ISOLATED oracle (vdiff_iso semantics) to filter the known batched-state leak.
// Usage:  node enum-clearindents.mjs [depthMax=3] [outPrefix=_enum]
// Output: <outPrefix>-diffs.json (isolated-confirmed diffs, empty = area exact),
//         progress + counts on stdout. Exit 1 if any confirmed diff.
import { readFileSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { canonJSON } from "./lib/compare.mjs";

const depthMax = parseInt(process.argv[2] || "3", 10);
const outPrefix = process.argv[3] || "_enum";

// Line vocabulary: all strings of length 0..3 over {' ','\t','\f'} plus each with a text tail.
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
  LINES.push(r); // all-ws (or empty) line
  LINES.push(r + "a"); // ws prefix + text
}
// nesting prefixes for inner BEGIN/END lines
const PREFIXES = ["", " ", "  ", "\t"];

// 3-line bodies use a reduced-but-role-complete vocabulary (all ws runs + short text lines):
// the third line exists to catch downstream assembly effects (paragraph joins after blanks,
// h20-class), for which the full ws-prefix x text cross-product of LINES is redundant.
const LINES3 = [...wsRuns, "a", " a", "  a"];

function* bodies(nLines, vocab = LINES) {
  if (nLines === 1) {
    for (const a of vocab) yield [a];
    return;
  }
  for (const rest of bodies(nLines - 1, vocab)) for (const a of vocab) yield [a, ...rest];
}

function wrap(depth, prefixes, bodyLines) {
  // innermost body sits inside frames A0..A<depth-1>; frame i's BEGIN/END carry prefixes[i]
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
    // prefix tuples: frame 0 always unprefixed; inner frames take every prefix
    const tuples = depth === 1 ? [[""]] : depth === 2 ? PREFIXES.map((p) => ["", p]) : [];
    if (depth === 3) for (const p of PREFIXES) for (const q of PREFIXES) tuples.push(["", p, q]);
    // body size: 2 lines everywhere; 3 lines only at depth 1 (keeps the count tractable
    // while covering first/middle/last-line roles; depth>=2 first-line roles come from the
    // inner BEGIN line itself plus the 2-line body)
    const sizes = depth === 1 ? [2, 3] : [2];
    for (const tuple of tuples)
      for (const n of sizes)
        for (const body of bodies(n, n === 3 ? LINES3 : LINES))
          for (const format of ["markdown", "org"])
            yield { id: "n" + id++, format, input: wrap(depth, tuple, body) };
  }
}

const CHUNK = 20000;
let all = 0,
  rawDiffs = [];
let chunk = [];
const strip = (o) => canonJSON({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });

function flush() {
  if (!chunk.length) return;
  writeFileSync(outPrefix + "-in.json", JSON.stringify(chunk));
  execSync(`cargo run -q --release --bin lsdoc-parse -- ${outPrefix}-in.json ${outPrefix}-ls.json`, {
    stdio: "ignore",
  });
  execSync(`node oracle.mjs ${outPrefix}-in.json`, { stdio: "ignore" }); // writes oracle-out.json (fixed name)
  const L = Object.fromEntries(
    JSON.parse(readFileSync(outPrefix + "-ls.json", "utf8")).map((x) => [x.id, x])
  );
  const O = JSON.parse(readFileSync("oracle-out.json", "utf8"));
  for (const o of O) {
    const lm = strip(L[o.id].projection),
      om = strip(o.projection);
    if (lm !== om) rawDiffs.push(chunk.find((c) => c.id === o.id));
  }
  all += chunk.length;
  process.stdout.write(`\r${all} enumerated, ${rawDiffs.length} raw diffs`);
  chunk = [];
}

for (const c of cases()) {
  chunk.push(c);
  if (chunk.length >= CHUNK) flush();
}
flush();
console.log("");

// Isolated re-verification of raw diffs (filters the batched-oracle state leak).
const confirmed = [];
for (const c of rawDiffs) {
  writeFileSync(outPrefix + "-one.json", JSON.stringify([c]));
  execSync(`node oracle.mjs ${outPrefix}-one.json`, { stdio: "ignore" });
  const o = JSON.parse(readFileSync("oracle-out.json", "utf8"))[0];
  execSync(`cargo run -q --release --bin lsdoc-parse -- ${outPrefix}-one.json ${outPrefix}-onels.json`, {
    stdio: "ignore",
  });
  const l = JSON.parse(readFileSync(outPrefix + "-onels.json", "utf8"))[0];
  if (strip(o.projection) !== strip(l.projection))
    confirmed.push({ ...c, mldoc: strip(o.projection), lsdoc: strip(l.projection) });
}
writeFileSync(outPrefix + "-diffs.json", JSON.stringify(confirmed, null, 1));
console.log(
  `${all} cases; raw diffs ${rawDiffs.length}; ISOLATED-CONFIRMED diffs ${confirmed.length}` +
    (confirmed.length ? ` -> ${outPrefix}-diffs.json` : " — area exact under this vocabulary")
);
process.exit(confirmed.length ? 1 : 0);

// Permanent regression gate for divergences reported from real use.
//
// mldoc has process-global parser state for some constructs, so run the oracle in
// a fresh process for each case. lsdoc is stateless and can parse the corpus in a
// single batch.
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { canonJSON } from "./lib/compare.mjs";

const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");
const corpusPath = resolve(process.argv[2] || join(__dir, "reported-divergences.json"));
const outPath = join(__dir, "reported-divergences-out.json");
const onePath = join(__dir, "_reported_one.json");

const env = {
  ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};

function projectionString(o) {
  const p = o.projection || o;
  return canonJSON({ blocks: p.blocks || p, refs: p.refs || { page: [], block: [] } });
}

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { encoding: "utf8", ...opts });
  if (r.status !== 0) {
    const tail = `${r.stderr || ""}${r.stdout || ""}`.slice(-4000);
    throw new Error(`${cmd} ${args.join(" ")} failed\n${tail}`);
  }
  return r;
}

const cases = JSON.parse(readFileSync(corpusPath, "utf8"));

run(
  "cargo",
  ["run", "-q", "--bin", "lsdoc-parse", "--", "--engine", "v2", corpusPath, outPath],
  { cwd: repo, env },
);

const lsdoc = new Map(
  JSON.parse(readFileSync(outPath, "utf8")).map((x) => [x.id, x]),
);

let bad = 0;
const failures = [];
for (const c of cases) {
  writeFileSync(onePath, JSON.stringify([c]));
  run(process.execPath, ["oracle.mjs", onePath], { cwd: __dir });
  const oracle = JSON.parse(readFileSync(join(__dir, "oracle-out.json"), "utf8"))[0];
  const actual = lsdoc.get(c.id);
  if (!actual) {
    bad++;
    failures.push({ c, reason: "missing lsdoc output" });
    continue;
  }
  const expected = projectionString(oracle);
  const got = projectionString(actual);
  if (expected !== got) {
    bad++;
    failures.push({ c, expected, got });
  }
}

if (bad) {
  console.log(`*** ${bad} reported divergence gate diffs / ${cases.length} cases ***`);
  for (const f of failures.slice(0, 25)) {
    console.log(`\nDIFF ${f.c.id} ${f.c.format || "md"} ${f.c.source || ""}`);
    if (f.c.issue) console.log(`issue: #${f.c.issue}`);
    if (f.c.heading) console.log(`heading: ${f.c.heading}`);
    if (f.reason) {
      console.log(f.reason);
      continue;
    }
    console.log(`input: ${JSON.stringify(f.c.input).slice(0, 600)}`);
    console.log(`mldoc: ${f.expected.slice(0, 800)}`);
    console.log(`lsdoc: ${f.got.slice(0, 800)}`);
  }
  process.exit(2);
}

console.log(`*** all ${cases.length} reported divergence cases match ***`);

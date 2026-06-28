// One-command differential regression runner (SPEC §6 "one-command regression
// run"). Orchestrates the full loop:
//   1. (re)generate the corpus           -> corpus.json
//   2. run the mldoc oracle              -> oracle-out.json
//   3. build+run lsdoc over the corpus   -> lsdoc-out.json
//   4. compare projections, print report -> divergences.json
//
// Exits non-zero if any divergence remains, so it gates CI / the dev loop.
// Usage: node run.mjs            (full loop)
//        node run.mjs --no-gen   (skip corpus regen)
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (r.status !== 0 && !opts.allowFail) {
    console.error(`\n✗ step failed: ${cmd} ${args.join(" ")} (exit ${r.status})`);
    process.exit(r.status ?? 1);
  }
  return r;
}

// 1. corpus — generate all sources, then merge into corpus.all.json (derived).
if (!process.argv.includes("--no-gen")) {
  run("node", [join(__dir, "corpus.gen.mjs")]);
  run("node", [join(__dir, "corpus.blocks.gen.mjs")]);
  run("node", [join(__dir, "corpus.mined.gen.mjs")]); // mined mldoc/OG test inputs
  run("node", [join(__dir, "corpus.real.gen.mjs")]); // real files (machine-specific; [] if absent)
}
const load = (f) => JSON.parse(readFileSync(join(__dir, f), "utf8"));
const inline = load("corpus.json");
const blocks = load("corpus.blocks.json");
const mined = load("corpus.mined.json");
const real = load("corpus.real.json").map((r) => ({ id: r.id, input: r.input })); // drop `file`
const all = [...inline, ...blocks, ...mined, ...real];
const allPath = join(__dir, "corpus.all.json");
writeFileSync(allPath, JSON.stringify(all, null, 1));
console.log(`corpus: ${inline.length} inline + ${blocks.length} block + ${mined.length} mined + ${real.length} real = ${all.length} total`);

// 2. oracle
run("node", [join(__dir, "oracle.mjs"), allPath]);
// 3. lsdoc — build+run via cargo. Toolchain lives on /aux; source env.sh.
//    spawnSync can't `source`, so we set the env vars cargo needs directly.
const cargoEnv = {
  ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
run("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--",
  allPath, join(__dir, "lsdoc-out.json")],
  { cwd: repo, env: cargoEnv });
// 4. compare (gates: non-zero exit on any divergence)
const cmp = run("node", [join(__dir, "compare.mjs")], { allowFail: true });
process.exit(cmp.status ?? 0);

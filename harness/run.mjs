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

// 1. corpus
if (!process.argv.includes("--no-gen")) {
  run("node", [join(__dir, "corpus.gen.mjs")]);
}
// 2. oracle
run("node", [join(__dir, "oracle.mjs")]);
// 3. lsdoc — build+run via cargo. Toolchain lives on /aux; source env.sh.
//    spawnSync can't `source`, so we set the env vars cargo needs directly.
const cargoEnv = {
  ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
run("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--",
  join(__dir, "corpus.json"), join(__dir, "lsdoc-out.json")],
  { cwd: repo, env: cargoEnv });
// 4. compare (gates: non-zero exit on any divergence)
const cmp = run("node", [join(__dir, "compare.mjs")], { allowFail: true });
process.exit(cmp.status ?? 0);

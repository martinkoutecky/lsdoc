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
//    Markdown sources are untagged (default "md"); Org sources carry format:"org".
if (!process.argv.includes("--no-gen")) {
  run("node", [join(__dir, "corpus.gen.mjs")]);
  run("node", [join(__dir, "corpus.blocks.gen.mjs")]);
  run("node", [join(__dir, "corpus.mined.gen.mjs")]);     // mined mldoc/OG md test inputs
  run("node", [join(__dir, "corpus.real.gen.mjs")]);      // real md files (machine-specific)
  run("node", [join(__dir, "corpus.org.gen.mjs")]);       // hand-written org adversarial
  run("node", [join(__dir, "corpus.org.mined.gen.mjs")]); // mined mldoc test_org inputs
  run("node", [join(__dir, "corpus.org.real.gen.mjs")]);  // real org graph (machine-specific)
  run("node", [join(__dir, "corpus.inline.gen.mjs")]);    // inline-only entrypoint corpus
}
const load = (f) => JSON.parse(readFileSync(join(__dir, f), "utf8"));
const strip = (a) => a.map(({ id, input, format }) => ({ id, input, format })); // drop `file`/`cat`
const inline = load("corpus.json");
const blocks = load("corpus.blocks.json");
const mined = load("corpus.mined.json");
const real = strip(load("corpus.real.json"));
const org = load("corpus.org.json");
const orgMined = load("corpus.org.mined.json");
const orgReal = strip(load("corpus.org.real.json"));
const all = [...inline, ...blocks, ...mined, ...real, ...org, ...orgMined, ...orgReal];
const allPath = join(__dir, "corpus.all.json");
writeFileSync(allPath, JSON.stringify(all, null, 1));
console.log(`corpus: ${inline.length} inline + ${blocks.length} block + ${mined.length} mined + ${real.length} real + ${org.length} org + ${orgMined.length} org-mined + ${orgReal.length} org-real = ${all.length} total`);

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
// 5. real-block-body gate (Tine feeds lsdoc per-block, re-bulleted): runs over the
//    machine-specific block-raws.json exports if present, else skips. See FOR-TINE.md.
const bg = run("node", [join(__dir, "blockgate.mjs")], { allowFail: true });
// 6. inline-entrypoint gate: lsdoc `inline()` vs mldoc `parseInlineJson` (the inline->edn /
//    OG inline-text path Tine uses for property values, breadcrumbs, ref previews, cells).
const ig = run("node", [join(__dir, "inlinegate.mjs")], { allowFail: true });
// 7. inline source-span invariant gate (S1–S5) over lsdoc-out.json (the projection output
//    written in step 3; inlinegate/blockgate write their own files, so it's still intact).
const sg = run("node", [join(__dir, "spans.mjs")], { allowFail: true });
console.log("spans gate:", sg.status === 0 ? "ok" : "FAIL");
// 8. v2 shortcut/post-processor audit: deterministic boundary alphabets for the
//    optimized paths whose proof obligation is "strict subset or decline".
const ag = run("node", [join(__dir, "audit-v2-shortcuts.mjs")], { allowFail: true });
console.log("shortcut audit:", ag.status === 0 ? "ok" : "FAIL");
process.exit(
  (cmp.status ?? 0) || (bg.status ?? 0) || (ig.status ?? 0) || (sg.status ?? 0) || (ag.status ?? 0)
);

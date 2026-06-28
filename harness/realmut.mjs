// B4/B5 — REAL-CORPUS MUTATION FUZZ: the data-grounded reachability test.
//
// Instead of asking an analyzer "can this happen in realistic input?" (a claim biased
// toward "no" = less work), we GROUND "realistic" in real data: take the actual real
// graphs (tine-test md + org-graph org), cut them into block-sized fragments, apply
// small realistic mutations, and run the differential gate on the result. A divergence
// here is, by construction, reachable from real content (it IS mutated real content) —
// a found bug with a real-derived reproducer. Zero divergences over a large run is
// strong, falsifiable, data-grounded evidence the floor is not reachable from realistic
// input. This also serves as the standing tripwire (run it in CI).
//
// Usage: node realmut.mjs [mutationsPerFragment=8] [seed=1] [mode=realistic|aggressive]
//   realistic (default): only edits a user actually makes to a block body — insert a
//     token, delete a char, dup/split/join a line, indent/dedent. A divergence here is
//     a genuinely reachable bug. This is the standing-tripwire signal.
//   aggressive: also wrap a whole fragment in emphasis / concat two fragments — these
//     produce derived-from-real-but-not-realistic inputs (the bulk of the noisy floor).
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { writeFileSync, readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");

const K = parseInt(process.argv[2] || "8", 10);   // mutations per fragment
let seed = parseInt(process.argv[3] || "1", 10);
const AGGRESSIVE = process.argv[4] === "aggressive";
const N_OPS = AGGRESSIVE ? 9 : 7; // realistic = ops 0–6; aggressive adds wrap(7)+concat(8)
const rng = () => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed / 0x7fffffff; };
const pick = (a) => a[Math.floor(rng() * a.length)];
const ri = (n) => Math.floor(rng() * n);

const cfg = (fmt) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false, keep_line_break: true,
  format: fmt === "org" ? "Org" : "Markdown", heading_to_list: false, export_md_remove_options: [],
});
const oracle = (input, fmt) => {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(fmt)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast) };
};
const IGNORE = new Set(["span"]);
const canon = (v) => Array.isArray(v) ? v.map(canon)
  : (v && typeof v === "object")
    ? Object.fromEntries(Object.keys(v).sort().filter(k => !IGNORE.has(k)).map(k => [k, canon(v[k])]))
    : v;
const S = (v) => JSON.stringify(canon(v));

// --- load the real graphs (whole files) ------------------------------------
const load = (f) => existsSync(join(__dir, f)) ? JSON.parse(readFileSync(join(__dir, f), "utf8")) : [];
const realFiles = [
  ...load("corpus.real.json").map(c => ({ input: c.input, fmt: "md" })),
  ...load("corpus.org.real.json").map(c => ({ input: c.input, fmt: c.format || "org" })),
];
if (!realFiles.length) {
  console.error("no real corpora found — run `node run.mjs` first to generate corpus.real.json / corpus.org.real.json");
  process.exit(1);
}

// --- fragments = whole files + every 1..4-line consecutive window ----------
// (a block body is arbitrary multi-line content, so realistic units are line windows
// of the real files, not just whole files.)
const fragments = [];
for (const { input, fmt } of realFiles) {
  fragments.push({ text: input, fmt });
  const lines = input.split("\n");
  for (let w = 1; w <= 4; w++)
    for (let i = 0; i + w <= lines.length; i++)
      fragments.push({ text: lines.slice(i, i + w).join("\n"), fmt });
}
// realistic mutation tokens (NOT adversarial soup): real Logseq/markdown/org constructs.
const MUT_TOK = {
  md: ["**bold**", "*it*", "`code`", "[[Page]]", "#tag", "[l](http://x)", "![i](a.png)",
       "- ", "1. ", "> ", "## ", "TODO ", "[ ] ", "[x] ", "((11111111-1111-1111-1111-111111111111))",
       "{{embed [[A]]}}", "$x$", "key:: value", " ", "  ", "\t", "x", "\n"],
  org: ["*bold*", "/it/", "~code~", "=v=", "[[Page]]", "[[t][l]]", "#tag", "- ", "+ ", "1. ",
        "* ", "** ", "# comment", "#+TITLE: x", ":PROPERTIES:", ":END:", "[fn:1] note",
        "<<target>>", "<2026-06-26 Fri>", "SCHEDULED: ", " ", "  ", "\t", "x", "\n"],
};
// small, realistic, structure-preserving-ish mutations of a fragment's text.
const mutate = (text, fmt) => {
  const toks = MUT_TOK[fmt];
  const op = ri(N_OPS);
  const lines = text.split("\n");
  if (text.length === 0) return pick(toks);
  switch (op) {
    case 0: { const p = ri(text.length + 1); return text.slice(0, p) + pick(toks) + text.slice(p); } // insert token
    case 1: { const p = ri(text.length); return text.slice(0, p) + text.slice(p + 1); }              // delete char
    case 2: { const i = ri(lines.length); lines.splice(i, 0, lines[i] ?? ""); return lines.join("\n"); } // dup line
    case 3: { const i = ri(lines.length); const l = lines[i] ?? ""; const p = ri(l.length + 1);
              lines[i] = l.slice(0, p) + "\n" + l.slice(p); return lines.join("\n"); }                // split line
    case 4: { if (lines.length < 2) return text; const i = ri(lines.length - 1);
              lines.splice(i, 2, (lines[i] ?? "") + (lines[i + 1] ?? "")); return lines.join("\n"); } // join lines
    case 5: { const i = ri(lines.length); lines[i] = pick(["  ", "    ", "\t"]) + (lines[i] ?? ""); return lines.join("\n"); } // indent
    case 6: { const i = ri(lines.length); lines[i] = (lines[i] ?? "").replace(/^\s+/, ""); return lines.join("\n"); } // dedent
    case 7: { const m = pick(fmt === "org" ? ["*", "/", "_", "~", "="] : ["**", "*", "`", "~~"]);
              return m + text + m; }                                                                  // wrap emphasis
    case 8: return text + "\n" + pick(fragments).text;                                                // concat another real fragment
  }
  return text;
};

// --- build the mutated batch ------------------------------------------------
// char-level mutations can split an emoji's UTF-16 surrogate pair, yielding a lone
// surrogate (invalid UTF-8, can't occur in a real file) — strip those so we only test
// genuinely realistic, valid-UTF-8 inputs.
const stripLoneSurrogates = (s) =>
  s.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "");

const inputs = [];
let id = 0;
for (const fr of fragments) {
  for (let k = 0; k < K; k++) {
    let t = fr.text;
    const rounds = 1 + ri(2);
    for (let r = 0; r < rounds; r++) t = mutate(t, fr.fmt);
    if (t.length > 20000) t = t.slice(0, 20000);
    t = stripLoneSurrogates(t);
    inputs.push({ id: `r${id++}`, input: t, format: fr.fmt, _src: fr.text.slice(0, 60) });
  }
}
console.log(`mode=${AGGRESSIVE ? "aggressive" : "realistic"}; real fragments: ${fragments.length}; mutated inputs: ${inputs.length} (K=${K})`);

const corpusPath = join(__dir, "corpus.realmut.json");
writeFileSync(corpusPath, JSON.stringify(inputs.map(({ id, input, format }) => ({ id, input, format })), null, 0));

const env = { ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}` };
const outPath = join(__dir, "lsdoc-realmut.json");
const r = spawnSync("cargo", ["run", "-q", "--bin", "lsdoc-parse", "--", corpusPath, outPath],
  { cwd: repo, env, encoding: "utf8" });
if (r.status !== 0) { console.error("lsdoc-parse FAILED (possible panic):\n", r.stderr?.slice(-2000)); process.exit(1); }
const byId = Object.fromEntries(JSON.parse(readFileSync(outPath, "utf8")).map(x => [x.id, x]));

let refMis = 0, blkMis = 0, shown = 0;
const diffs = [];
for (const c of inputs) {
  let op; try { op = oracle(c.input, c.format); } catch { continue; }
  const lp = byId[c.id]?.projection; if (!lp) continue;
  const rb = S(op.refs) !== S(lp.refs), bb = S(op.blocks) !== S(lp.blocks);
  if (rb) refMis++; if (bb) blkMis++;
  if (rb || bb) {
    diffs.push({ id: c.id, input: c.input, src: c._src, fmt: c.format,
      oracle: bb ? S(op.blocks) : S(op.refs), lsdoc: bb ? S(lp.blocks) : S(lp.refs) });
    if (shown < 20) { shown++;
      console.log(`\nDIVERGENCE ${c.id} [${c.format}] from real src ${JSON.stringify(c._src)}`);
      console.log(`  input: ${JSON.stringify(c.input).slice(0, 200)}`);
      if (bb) { console.log(`  O: ${S(op.blocks).slice(0, 300)}`); console.log(`  L: ${S(lp.blocks).slice(0, 300)}`); }
      if (rb) { console.log(`  refs O: ${S(op.refs)}  L: ${S(lp.refs)}`); }
    }
  }
}
writeFileSync(join(__dir, "realmut-divergences.json"), JSON.stringify(diffs, null, 1));
console.log(`\nrealmut: ${inputs.length} mutated-real inputs — refMismatch=${refMis} blockMismatch=${blkMis}`);
console.log(diffs.length === 0
  ? `✓ 0 divergences (${AGGRESSIVE ? "aggressive" : "realistic"}): no mutated-real input reaches a parity gap.`
  : `✗ ${diffs.length} divergences = REACHABLE bugs with real-derived reproducers (see realmut-divergences.json).`);
process.exit(diffs.length === 0 ? 0 : 2);

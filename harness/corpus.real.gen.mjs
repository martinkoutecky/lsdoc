// Real-content corpus: every .md file from the realism gate (tine-test + the Tine
// kitchen-sink) as one input each. Used for the structural smoke test now and the
// M5 real-graph diff. Output: corpus.real.json.
import { readFileSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { homedir } from "node:os";

function mdFiles(dir) {
  const out = [];
  let ents;
  try { ents = readdirSync(dir); } catch { return out; }
  for (const e of ents) {
    const p = join(dir, e);
    const st = statSync(p);
    if (st.isDirectory()) out.push(...mdFiles(p));
    else if (e.endsWith(".md")) out.push(p);
  }
  return out;
}

const roots = [
  join(homedir(), "research/tine-test"),
  "/aux/koutecky/logseq/logseq-claude/src/fixtures",
];
const files = roots.flatMap(mdFiles).filter((f) => f.endsWith(".md"));
const out = files.map((f, i) => ({ id: `r${String(i).padStart(3, "0")}`, file: f, input: readFileSync(f, "utf8") }));

const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.real.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} real files:`);
for (const o of out) console.log(`  ${o.id}  ${o.file}  (${o.input.length} bytes)`);

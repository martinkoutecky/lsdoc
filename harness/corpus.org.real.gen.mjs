// Real Org content: every .org file under ~/research/org-graph (the real Logseq Org
// graph) as one input each, tagged format:"org". The Org-milestone realism gate.
// Output: corpus.org.real.json.
import { readFileSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { homedir } from "node:os";

function orgFiles(dir) {
  const out = [];
  let ents;
  try { ents = readdirSync(dir); } catch { return out; }
  for (const e of ents) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) out.push(...orgFiles(p));
    else if (e.endsWith(".org")) out.push(p);
  }
  return out;
}

const root = join(homedir(), "research/org-graph");
const files = orgFiles(root);
const out = files.map((f, i) => ({
  id: `or${String(i).padStart(3, "0")}`, file: f, format: "org", input: readFileSync(f, "utf8"),
}));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.org.real.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} real org files`);
for (const o of out) console.log(`  ${o.id}  ${o.file}  (${o.input.length} bytes)`);

// Isolated differential: parse each probe input in a FRESH oracle process, so mldoc's
// cross-parse global-state leak (e.g. `$$$` before `$$$$`) can't contaminate the result.
// lsdoc is stateless so a single batch run is fine. Usage: node vdiff_iso.mjs <probe.json>
import { readFileSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { canonJSON } from "./lib/compare.mjs";
const path = process.argv[2];
const cases = JSON.parse(readFileSync(path, "utf8"));
execSync(`cargo run -q --bin lsdoc-parse -- ${path} _iso_lsdoc.json`, { stdio: "ignore" });
const L = Object.fromEntries(JSON.parse(readFileSync("_iso_lsdoc.json", "utf8")).map((x) => [x.id, x]));
// canonJSON = the main gate's canonical comparator (key-sorted, span-dropped) — raw stringify
// gave FALSE diffs on key order (email/timestamp objects).
const strip = (o) => canonJSON({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });
let bad = 0;
for (const c of cases) {
  writeFileSync("_iso_one.json", JSON.stringify([c]));
  execSync("node oracle.mjs _iso_one.json", { stdio: "ignore" });
  const o = JSON.parse(readFileSync("oracle-out.json", "utf8"))[0];
  const om = strip(o.projection), lm = strip(L[c.id].projection);
  if (om !== lm) { bad++; console.log("DIFF " + c.id + "  " + JSON.stringify(c.input)); console.log("   mldoc:", om.slice(0, 140)); console.log("   lsdoc:", lm.slice(0, 140)); }
  else console.log("OK   " + c.id);
}
console.log(bad ? `*** ${bad} REAL diffs (isolated oracle) ***` : `*** all ${cases.length} match (isolated oracle) ***`);
writeFileSync("_iso_done", "");

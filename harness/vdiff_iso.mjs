// Isolated differential: parse each probe input in a FRESH oracle process, so mldoc's
// cross-parse global-state leak (e.g. `$$$` before `$$$$`) can't contaminate the result.
// lsdoc is stateless so a single batch run is fine. Usage: node vdiff_iso.mjs <probe.json>
//
// audit4 C5 hardening: every invocation works in a private mkdtemp directory (two
// concurrent runs used to cross-wire each other's results through checkout-global
// _iso_*.json/oracle-out.json and still exit 0); a null oracle projection is reported
// as ORACLE-ERROR instead of crashing; and the process exit code is nonzero on any
// diff, oracle error, or missing case, so callers can gate on it.
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "fs";
import { execSync } from "child_process";
import { tmpdir } from "os";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { canonJSON } from "./lib/compare.mjs";
const __dir = dirname(fileURLToPath(import.meta.url));
const path = process.argv[2];
const cases = JSON.parse(readFileSync(path, "utf8"));
const work = mkdtempSync(join(tmpdir(), "vdiff-iso-"));
const lsdocOut = join(work, "lsdoc.json");
execSync(`cargo run -q --bin lsdoc-parse -- ${JSON.stringify(path)} ${JSON.stringify(lsdocOut)}`, {
  stdio: ["ignore", "inherit", "inherit"],
  cwd: join(__dir, ".."),
});
const L = Object.fromEntries(JSON.parse(readFileSync(lsdocOut, "utf8")).map((x) => [x.id, x]));
// canonJSON = the main gate's canonical comparator (key-sorted, span-dropped) — raw stringify
// gave FALSE diffs on key order (email/timestamp objects).
const strip = (o) => canonJSON({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });
let bad = 0;
for (const c of cases) {
  const onePath = join(work, "one.json");
  const oneOut = join(work, "one-oracle.json");
  writeFileSync(onePath, JSON.stringify([c]));
  execSync(`node ${JSON.stringify(join(__dir, "oracle.mjs"))} ${JSON.stringify(onePath)} ${JSON.stringify(oneOut)}`, { stdio: "ignore" });
  const o = JSON.parse(readFileSync(oneOut, "utf8"))[0];
  if (!L[c.id]) { bad++; console.log("MISSING lsdoc result for " + c.id); continue; }
  if (o.err || !o.projection) { bad++; console.log("ORACLE-ERROR " + c.id + "  " + JSON.stringify(c.input).slice(0, 160) + "  — " + o.err); continue; }
  const om = strip(o.projection), lm = strip(L[c.id].projection);
  if (om !== lm) { bad++; console.log("DIFF " + c.id + "  " + JSON.stringify(c.input)); console.log("   mldoc:", om.slice(0, 140)); console.log("   lsdoc:", lm.slice(0, 140)); }
  else console.log("OK   " + c.id);
}
rmSync(work, { recursive: true, force: true });
console.log(bad ? `*** ${bad} REAL diffs/errors (isolated oracle) ***` : `*** all ${cases.length} match (isolated oracle) ***`);
process.exitCode = bad ? 1 : 0;

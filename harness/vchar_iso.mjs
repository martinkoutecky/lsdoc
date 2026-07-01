// Isolated characterization: fresh oracle process per input (no mldoc state leak),
// print mldoc + lsdoc block structure for EVERY case. Usage: node vchar_iso.mjs <probe.json>
import { readFileSync, writeFileSync } from "fs";
import { execSync } from "child_process";
const path = process.argv[2];
const cases = JSON.parse(readFileSync(path, "utf8"));
execSync(`cargo run -q --bin lsdoc-parse -- ${path} _iso_lsdoc.json`, { stdio: "ignore" });
const L = Object.fromEntries(JSON.parse(readFileSync("_iso_lsdoc.json", "utf8")).map((x) => [x.id, x]));
const rend = (bs) =>
  (bs.blocks || bs)
    .map((b) => {
      if (b.kind === "paragraph")
        return "P[" + b.inline.map((i) => (i.k === "plain" ? JSON.stringify(i.text) : i.k + (i.text !== undefined ? ":" + JSON.stringify(i.text).slice(0, 30) : ""))).join(",") + "]";
      if (b.kind === "raw_html" || b.kind === "displayed_math") return b.kind.toUpperCase() + "(" + JSON.stringify(b.text) + ")";
      if (b.kind === "quote" || b.kind === "custom") return b.kind[0].toUpperCase() + "{" + rend(b.children) + "}";
      return b.kind;
    })
    .join(" ");
for (const c of cases) {
  writeFileSync("_iso_one.json", JSON.stringify([c]));
  execSync("node oracle.mjs _iso_one.json", { stdio: "ignore" });
  const o = JSON.parse(readFileSync("oracle-out.json", "utf8"))[0];
  const om = rend(o.projection), lm = rend(L[c.id].projection);
  console.log((om === lm ? "OK   " : "DIFF ") + c.id + "  " + JSON.stringify(c.input));
  console.log("   mldoc:", om);
  if (om !== lm) console.log("   lsdoc:", lm);
}

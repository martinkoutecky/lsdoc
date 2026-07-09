// The live oracle: run every corpus input through the pinned latest mldoc package
// (currently mldoc@1.5.9) and emit the
// normalized observable projection { blocks, refs } that lsdoc's Rust side also
// produces. Output: oracle-out.json = [{ id, input, projection }].
//
// Usage: node oracle.mjs            (reads corpus.json)
//        node oracle.mjs <file>     (reads a custom corpus json: [{id,input}])
import { createRequire } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));

// mldoc config = OG's default (graph-parser mldoc.cljc), per format. Each corpus
// entry may carry `format: "md" | "org"` (default "md"); the Org milestone (M6)
// runs Org inputs through the Org config in the SAME differential loop.
const cfg = (format) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: format === "org" ? "Org" : "Markdown",
  heading_to_list: false, export_md_remove_options: [],
});
export const MLDOC_CFG = cfg("md");

export function parseToProjection(input, format = "md") {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(format)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast, format) };
}

function main() {
  const corpusPath = process.argv[2] || join(__dir, "corpus.json");
  const corpus = JSON.parse(readFileSync(corpusPath, "utf8"));
  const out = corpus.map((c) => {
    let projection, err = null;
    try { projection = parseToProjection(c.input, c.format); }
    catch (e) { err = String(e); projection = null; }
    return { id: c.id, input: c.input, format: c.format || "md", projection, err };
  });
  writeFileSync(join(__dir, "oracle-out.json"), JSON.stringify(out, null, 1));
  const errs = out.filter((o) => o.err).length;
  console.log(`oracle: wrote ${out.length} projections (${errs} errors)`);
}

main();

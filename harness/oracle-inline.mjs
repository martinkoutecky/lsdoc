// Fresh-process inline oracle used by the intentional-divergence classifier.
import { createRequire } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import { cleanInlines, normInline } from "./lib/normalize.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const [inputPath, outputPath] = process.argv.slice(2);
const item = JSON.parse(readFileSync(inputPath, "utf8"));
const cfg = JSON.stringify({
  toc: false,
  parse_outline_only: false,
  heading_number: false,
  keep_line_break: true,
  format: item.format === "org" ? "Org" : "Markdown",
  heading_to_list: false,
  export_md_remove_options: [],
});

let inline = null;
let err = null;
try {
  inline = cleanInlines(JSON.parse(Mldoc.parseInlineJson(item.input, cfg)).map(normInline));
} catch (error) {
  err = String(error);
}
writeFileSync(outputPath, JSON.stringify({ inline, err }));

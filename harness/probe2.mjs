// Second probe: block shapes not covered in probe.mjs, to finalize the projection.
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const cfg = JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: "Markdown", heading_to_list: false,
  export_md_remove_options: [],
});
const inputs = [
  "1. first\n2. second",
  "---",
  "| a | b |\n| - | - |\n| 1 | 2 |",
  "> outer\n> - inner bullet",
  "![alt](img.png)",
  "[file](../x.pdf)",
  "TODO finish this\nDEADLINE: <2026-07-01 Wed>",
  "line one\nline two",
  "## H2 with #tag and [[Link]]",
  "- [ ] task\n- [x] done",
  "#+BEGIN_QUOTE\nhi\n#+END_QUOTE",
  "a footnote[^1]\n\n[^1]: the note",
];
for (const inp of inputs) {
  console.log("INPUT:", JSON.stringify(inp));
  try { console.log(JSON.stringify(JSON.parse(Mldoc.parseJson(inp, cfg)))); }
  catch (e) { console.log("ERR", String(e)); }
  console.log("-".repeat(60));
}

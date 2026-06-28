// Focused probe round 2: table alignment representation + checkbox edge cases +
// Target/Inline_Hiccup (inline tags with no Rust variant yet).
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const cfg = (f) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: f === "org" ? "Org" : "Markdown",
  heading_to_list: false, export_md_remove_options: [],
});
const show = (inp, f = "md") => {
  console.log(`\n=== [${f}] ${JSON.stringify(inp)}`);
  console.log(JSON.stringify(JSON.parse(Mldoc.parseJson(inp, cfg(f)))));
};
console.log("###### TABLE alignment (col_groups? per-cell?)");
show("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |");
show("| a | b |\n|---|---|\n| 1 | 2 |");
show("| a | b | c | d |\n|:-|:-:|-:|-|\n| 1 | 2 | 3 | 4 |");
show("| a |\n|:-:|\n| x |\n| y |");
console.log("\n###### CHECKBOX edge cases (org)");
show("- plain item", "org");
show("- [ ] unchecked", "org");
show("- [x] checked", "org");
show("- [X] checked caps", "org");
show("1. [ ] ordered checkbox", "org");
show("- [-] partial", "org");
console.log("\n###### CHECKBOX (md ordered list — any checkbox?)");
show("1. [ ] x\n2. [x] y");
console.log("\n###### Target / Inline_Hiccup");
show("see <<my target>> here", "org");
show("@@html:<b>x</b>@@", "org");
show("@@hiccup:[:div]@@", "org");

// Probe mldoc's Org-format AST shape (format:"Org") to design the Org parser +
// extend normalize.mjs. Run from harness/ after npm install.
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const cfg = JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: "Org", heading_to_list: false,
  export_md_remove_options: [],
});
const inputs = [
  "* Heading one",
  "** Sub two",
  "* TODO [#A] task with :tag1:tag2:",
  "* DONE finished",
  "text *bold* /italic/ _under_ +strike+ ~code~ =verb= ^^hl^^",
  "[[target]] and [[target][label]] and [[https://x.org][site]]",
  "met <2026-06-26 Fri> and [2026-06-20 Sat]",
  "* h\nSCHEDULED: <2026-06-26 Fri>",
  "#+TITLE: my title",
  "#+FILETAGS: :a:b:",
  "#+BEGIN_SRC clojure\n(defn x [])\n#+END_SRC",
  "#+BEGIN_QUOTE\nquoted\n#+END_QUOTE",
  ":PROPERTIES:\n:key: value\n:another: 2\n:END:",
  "- milk\n- eggs\n+ also",
  "| a | b |\n|---+---|\n| 1 | 2 |",
  "a plain paragraph\nsecond line",
  "[fn:1] a footnote def",
  "see [fn:1] ref",
  "#+BEGIN_EXAMPLE\nliteral\n#+END_EXAMPLE",
];
for (const inp of inputs) {
  console.log("INPUT:", JSON.stringify(inp));
  try { console.log(JSON.stringify(JSON.parse(Mldoc.parseJson(inp, cfg)))); }
  catch (e) { console.log("ERR", String(e)); }
  console.log("-".repeat(60));
}

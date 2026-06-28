// One-off AST-shape probe: dump mldoc 1.5.7 parseJson for representative inputs
// so we can design the normalized comparison projection against the real shape,
// not a guess. Run from harness/ after `npm install`.
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");

const cfg = JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: "Markdown", heading_to_list: false,
  export_md_remove_options: [],
});

const inputs = [
  "hello **bold** *italic* ~~strike~~ ^^hl^^ `code`",
  "# Heading one",
  "- item 1\n- item 2\n  - nested",
  "[[Page]] and #tag and ((11111111-1111-1111-1111-111111111111))",
  "[label](https://example.com) and https://bare.url/x",
  "key:: value\nother:: 1",
  "> a quote",
  "```js\nlet x = [[NotARef]]\n```",
  "{{embed [[Foo]]}} {{query [[Bar]]}}",
  "$x^2$ and $$y$$",
  "\\[[escaped]] and \\#nottag",
  "#[[bracket tag]] and #a.b",
];

for (const inp of inputs) {
  const ast = JSON.parse(Mldoc.parseJson(inp, cfg));
  console.log("INPUT:", JSON.stringify(inp));
  console.log(JSON.stringify(ast, null, 1));
  console.log("=".repeat(72));
}

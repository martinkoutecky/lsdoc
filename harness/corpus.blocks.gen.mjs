// Block-structure corpus (multi-line) — complements the inline-focused corpus.json.
// Targets mldoc's block segmentation: headings, bullets/lists + indentation, code
// fences, properties, quotes/callouts, hr, tables, footnote defs, paragraph
// wrapping + blank-line separation, and mixed documents. Output: corpus.blocks.json
// = [{ id: b###, cat, input }].
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const cases = [];
const add = (cat, input) => cases.push({ cat, input });

// headings
add("heading", "# h1");
add("heading", "## h2");
add("heading", "###### h6");
add("heading", "####### seven hashes");
add("heading", "#nospace");
add("heading", "#  extra spaces");
add("heading", "## trailing ##");
add("heading", "Title\n===");
add("heading", "Title\n---");

// unordered bullets (mldoc → Heading{unordered:true})
add("bullet", "- a");
add("bullet", "* b");
add("bullet", "+ c");
add("bullet", "- a\n- b");
add("bullet", "- a\n  - nested");
add("bullet", "- a\n    - deeper");
add("bullet", "- a\n\t- tab nested");
add("bullet", "-nospace");
add("bullet", "- a\n- b\n- c");

// ordered lists (mldoc → List node)
add("list", "1. a\n2. b");
add("list", "1) a\n2) b");
add("list", "3. starts at three");
add("list", "1. a\n   1. nested ordered");
add("list", "1. a\n- mixed bullet");

// nested lists (mldoc folds a deeper-indented item into the preceding item's
// `items` sub-array; any strictly-greater indent nests, no fixed step).
add("nest", "* a\n  * b");                 // b nested under a
add("nest", "* a\n  * b\n    * c");        // a > b > c (3 levels)
add("nest", "* a\n * b");                  // 1-space indent still nests
add("nest", "* a\n* b");                   // equal indent → siblings
add("nest", "+ a\n  + b");                 // `+` nests too
add("nest", "1. a\n   2. b\n   3. c");     // b,c siblings, both children of a
add("nest", "* a\n  1. b");               // mixed unordered/ordered nests
add("nest", "1. a\n   1. nested");        // the former b021 (indented numbered)
add("nest", "* a\n  * b\n  * b2\n    * c"); // a > [b, b2 > [c]]
add("nest", "* a\n    * deep\n  * mid");   // mid (indent 2) is a TOP sibling of a, not a child

// code fences (mldoc → Src)
add("code", "```\ncode\n```");
add("code", "```js\nlet x = 1\n```");
add("code", "~~~\ntilde fence\n~~~");
add("code", "```\nunclosed code");
add("code", "```python\na\nb\nc\n```");
add("code", "```\n[[NotARef]] #notag\n```");
add("code", "    four space indent");

// properties
add("prop", "key:: value");
add("prop", "key:: value\nother:: 1");
add("prop", "alias:: a, b, c");
add("prop", "tags:: [[Foo]], [[Bar]]");
add("prop", "key:: value\n\nnot a prop");

// quotes / callouts
add("quote", "> a quote");
add("quote", "> line one\n> line two");
add("quote", "#+BEGIN_QUOTE\nquoted\n#+END_QUOTE");
add("quote", "#+BEGIN_NOTE\nnote body\n#+END_NOTE");

// horizontal rules
add("hr", "---");
add("hr", "***");
add("hr", "___");

// tables
add("table", "| a | b |\n| - | - |\n| 1 | 2 |");
add("table", "| h1 | h2 | h3 |\n|---|---|---|\n| x | y | z |");

// footnotes
add("footnote", "text[^1]\n\n[^1]: the definition");

// paragraph wrapping + blank-line separation
add("para", "line one\nline two");
add("para", "para one\n\npara two");
add("para", "a\nb\n\nc\nd");

// mixed documents
add("mixed", "# Title\n- bullet one\n- bullet two");
add("mixed", "intro para\n\n## Section\n\n```js\ncode()\n```\n\nclosing");
add("mixed", "- task\n  key:: value");

// list checkboxes (md: `*`/`+`/`N.` lists carry checkbox; `-` bullets do NOT —
// `- [ ] x` is a Heading{unordered} with literal title "[ ] x", per mldoc).
add("checkbox", "* [ ] unchecked");
add("checkbox", "* [x] checked");
add("checkbox", "* [X] checked caps");
add("checkbox", "+ [ ] plus checkbox");
add("checkbox", "1. [ ] ordered todo\n2. [x] ordered done");
add("checkbox", "* plain no checkbox");
add("checkbox", "- [ ] dash is NOT a checkbox");   // → Heading, literal "[ ] …"
add("checkbox", "* [-] not a checkbox marker");    // `[-]` is literal text
add("checkbox", "* [ ] a\n  * [x] nested done");   // checkbox survives nesting

// edge / empty
add("edge", "");
add("edge", "   ");
add("edge", "\n\n");
add("edge", "<div>raw html</div>");

const out = cases.map((c, idx) => ({ id: `b${String(idx).padStart(3, "0")}`, cat: c.cat, input: c.input }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.blocks.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} block corpus inputs`);

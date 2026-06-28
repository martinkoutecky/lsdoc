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

// bullet heading-size (Gap 1: a `- ## Title` block carries the heading level as `size`,
// mirroring Heading.size; mldoc Heading{unordered, size}). Found by the Tine integration.
add("bullet-size", "- # Heading one");
add("bullet-size", "- ## Heading two");
add("bullet-size", "- ###### six");
add("bullet-size", "- ####### seven");      // uncapped
add("bullet-size", "- # TODO task");        // size + marker
add("bullet-size", "- ## [#A] prioritised");
add("bullet-size", "- #nospace");           // not a heading (no space) → size none
add("bullet-size", "- plain bullet");       // size none

// bullet-line block-opener splits (Gap 2: post-marker block construct ⇒ empty bullet +
// sibling block, matching mldoc). Found by the Tine integration + a post-marker audit.
add("bullet-open", "- ---");                 // → bullet + hr (common divider)
add("bullet-open", "- ***");
add("bullet-open", "- ___");
add("bullet-open", "- $$ E = mc^2 $$");      // → bullet + displayed_math
add("bullet-open", "- [^1]: the body");      // → bullet + footnote_def
add("bullet-open", "- <div>x</div>");        // → bullet + raw_html
add("bullet-open", "- \\begin{eq}a\\end{eq}"); // → bullet + latex_env
add("bullet-open", "- | a | b |");           // → bullet + table (single row)
add("bullet-open", "- | a | b |\n| 1 | 2 |"); // → bullet + table (multi-row)
add("bullet-open", "- # ---");               // size 1 + hr
add("bullet-open", "- # $$ x $$");           // size 1 + displayed_math
add("bullet-open", "- # | a |");             // size 1 + table
add("bullet-open", "- # [^1]: b");           // size 1, NO footnote split (inline ref)

// real-corpus mutation-fuzz (realmut) structural fixes: empty markers, empty
// heading/bullet trailing-whitespace splits, and leading whitespace before `#`.
// mldoc requires non-empty list content; an empty ordered/`*`/`+` marker is a
// Paragraph, not a List.
add("realmut-empty-list", "1. ");           // → paragraph (empty ordered marker)
add("realmut-empty-list", "3. ");
add("realmut-empty-list", "1.");            // no space at all → paragraph
add("realmut-empty-list", "1.  ");          // marker + only ws → paragraph
add("realmut-empty-list", "* ");            // → paragraph
add("realmut-empty-list", "+ ");
add("realmut-empty-list", "* [ ]");         // checkbox but no title → paragraph
add("realmut-empty-list", "1. [ ]");
add("realmut-empty-list", "1. x\n2. ");     // trailing empty marker ends the list
add("realmut-empty-list", "* a\n* ");
// empty ATX heading with trailing whitespace → [heading, paragraph(trailing ws)].
add("realmut-empty-head", "## ");
add("realmut-empty-head", "# ");
add("realmut-empty-head", "##  ");          // two trailing spaces in the paragraph
add("realmut-empty-head", "# TODO ");       // marker on heading + ws split
add("realmut-empty-head", "# [#A] ");       // priority on heading + ws split
add("realmut-empty-head", "## \nfoo");      // trailing ws absorbs the next line
// empty `-` bullet with trailing whitespace → [bullet, paragraph(trailing ws)].
add("realmut-empty-bullet", "- ");
add("realmut-empty-bullet", "-   ");
add("realmut-empty-bullet", "- \t ");
add("realmut-empty-bullet", "- ## ");       // size kept on the bullet, ws split
add("realmut-empty-bullet", "- # ");
add("realmut-empty-bullet", "- TODO ");     // marker on bullet + ws split
add("realmut-empty-bullet", "- \nfoo");     // trailing ws absorbs the next line
add("realmut-empty-bullet", "foo\n- ");     // paragraph + empty bullet + paragraph
// leading whitespace before `#` is a heading (level = 1 + ws, uncapped, tab = 1).
add("realmut-ws-heading", "  # heading");   // 2 spaces → level 3
add("realmut-ws-heading", "   # heading");  // 3 spaces → level 4
add("realmut-ws-heading", "    # heading"); // 4 spaces still a heading (no ≤3 rule)
add("realmut-ws-heading", "\t# heading");   // tab → level 2
add("realmut-ws-heading", " \t # heading"); // mixed ws → level 4
add("realmut-ws-heading", "  ## x");        // leading ws + multi-hash
add("realmut-ws-heading", "  ## ");         // leading ws + empty + trailing ws split
add("realmut-ws-heading", "  - ");          // leading ws + empty bullet trailing split
add("realmut-ws-heading", "foo\n  # bar");  // heading interrupts a paragraph

// edge / empty
add("edge", "");
add("edge", "   ");
add("edge", "\n\n");
add("edge", "<div>raw html</div>");

const out = cases.map((c, idx) => ({ id: `b${String(idx).padStart(3, "0")}`, cat: c.cat, input: c.input }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.blocks.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} block corpus inputs`);

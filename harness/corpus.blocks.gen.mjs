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

// list-item multi-line continuation (mldoc `lists0.ml` folds an indented non-item line into the
// preceding item's content, de-indented via per-line `String.trim`; re-parsed with the item-content
// grammar). org already folds; this is the markdown port. Verified byte-exact vs the mldoc oracle.
add("cont", "* a\n  b");                       // plain continuation → paragraph["a", Break, "b"]
add("cont", "* a\n  b\n  c");                  // multi-line continuation joins with Break
add("cont", "+ a\n  b");                       // `+` item continuation
add("cont", "1. a\n   b");                     // ordered item continuation (own indent width)
add("cont", "10. a\n  b");                     // ordered, continuation shallower than marker width
add("cont", "* [ ] a\n  b");                   // checkbox item continuation
add("cont", "* a\n\tb\n\tc");                  // tab-indented continuation
add("cont", "* a\n\n  b");                     // a BLANK line ENDS the continuation (separate para)
add("cont", "* a\nb");                         // LAZY (col-0) line does NOT continue
add("cont", "* a\n  b\nc");                    // fold b, then col-0 c ends list → separate para
add("cont", "* a\n  cont\n* b");               // cont folds into item 1; `* b` is item 2
add("cont", "* a\n  b\n  c\n* d\n  e");        // two items, each with continuation
add("cont", "* a\n  # h");                     // `#` folds (heading suppressed in item content)
add("cont", "* a\n  -x");                      // `-x` (no space) folds; `- ` would NOT (a Bullet)
add("cont", "* a\n   \n  b");                  // whitespace-only line folds as an empty Break
add("cont-block", "* #+BEGIN_TIP\n  this is a tip\n  #+END_TIP"); // `*` admonition folds → custom{tip}
add("cont-block", "+ #+BEGIN_NOTE\n  n\n  #+END_NOTE");           // `+` admonition folds → custom{note}
add("cont-block", "* a\n  ```\n  code\n  ```"); // fenced code folds (de-indented body)
add("cont-block", "* a\n  | t |");             // table folds into item content
add("cont-block", "* a\n  > q\n  c");          // blockquote folds (lazy continuation inside)
add("cont-block", "* a\n  #+TITLE: x");        // doc-level item content has NO Directive → inline #tag
add("cont-child", "* a\n  b\n  * c\n    d");   // continuation then a nested child with its own continuation
add("cont-child", "+ a\n  + b\n    c\n  d");   // child folds deeper-AND-same-indent non-item lines
add("cont-collapse", "* a\n  b\n  5x");        // deeper unparseable list-shape → FULL collapse to paragraph
add("cont-collapse", "* a\n* b\n  5x");        // partial collapse: keep item a, `* b\n  5x` → paragraph
add("cont-def", "* term ::");                  // unordered `name ::` definition → item name, empty content
add("cont-def", "* term :: desc");             // trailing text after `::` ⇒ NOT a definition (plain content)

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

// block/drawer pairing semantics (v2 pre-pairing must reproduce mldoc's greedy
// first-closer-of-name, non-overlapping, prefix-match behavior — see DESIGN-lsdoc-v2).
add("pairing", "#+BEGIN_FOO\n#+BEGIN_FOO\nx\n#+END_FOO\n#+END_FOO"); // nested same name: outer grabs FIRST #+END_FOO, inner is content
add("pairing", "#+BEGIN_FOO\nx\n#+END_BAR");                          // mismatched END → no close → paragraph
add("pairing", "#+BEGIN_FOO\n#+END_BAR\n#+END_FOO");                  // skip non-matching #+END_BAR, close at #+END_FOO
add("pairing", "#+BEGIN_FOO\nx\n#+END_FOObar");                       // prefix-match: #+END_FOObar closes FOO
// NB: md `#+END_SRC trailing` (junk after END_SRC) is a PRE-EXISTING divergence (mldoc keeps
// the Src; lsdoc md renders Custom{src}) — adversarial, unrelated to pairing; tracked, not gated.
add("pairing", ":A:\n:B:\nx\n:END:\n:END:");                          // nested drawers: first :END: closes :A:
add("pairing", ":A:\nx\n:END:\nmore\n:END:");                         // first :END: closes; second is stray
add("pairing", "- ```\ncode\n```");                                   // dash-bullet fence that CLOSES → bullet + src
add("pairing", "- ```\nunclosed code");                              // dash-bullet fence no close → bullet "```"

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
// dash directly followed by an ATX run is a bullet (no space needed); `-#x`/`-x` are not.
add("bullet-size", "-## HEADINGS");         // → bullet size 2 (realmut-found)
add("bullet-size", "-# x");                 // → bullet size 1
add("bullet-size", "-#x");                  // → paragraph (no space after #)
add("bullet-size", "-#### ");               // → bullet size 4 + trailing-ws paragraph

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

// realmut tracked-edge fixes (B): table header+sep no body; list content kept raw
// (no `#`/marker strip); a single blank line between list items is absorbed.
add("edge-table", "| a | b |\n|---|---|");        // header+sep, no body → sep stays a body row
add("edge-table", "| a | b |\n|---|---|\n| 1 | 2 |"); // body present → sep dropped (unchanged)
add("edge-listraw", "* # heading");                // list content "# heading" (NOT stripped)
add("edge-listraw", "* TODO task");                // "TODO task" (marker kept)
add("edge-listraw", "1. # h");
add("edge-listraw", "* ## TODO x");                // "## TODO x"
add("edge-listraw", "* [ ] # x");                  // checkbox stripped, "# x" kept
add("edge-listblank", "* a\n\n* b");               // one blank absorbed → List(a,b)
add("edge-listblank", "* a\n\n\n* b");             // two blanks → List + para + List
add("edge-listblank", "1. a\n\n2. b");
add("edge-listblank", "* a\n\nplain");             // blank then non-item → list ends
add("edge-listblank", "* a\n\n* b\n\n* c");        // multiple single-blanks absorbed
add("edge-listblank", "* a\n\n# h");               // blank then heading → list ends

// edge / empty
add("edge", "");
add("edge", "   ");
add("edge", "\n\n");
add("edge", "<div>raw html</div>");

// C2 — markdown blockquote marker-line rules (audit C2): `>`+`- `/`# `/`id:: ` is a
// plain Paragraph (NOT an empty quote — was silent content loss); `>`+`*`/`+`/`N.` is a
// Quote containing a List. Multi-line + nested + break-before-list edges included.
const QU = "550e8400-e29b-41d4-a716-446655440000";
add("c2-quote", "> - x");                          // → Paragraph "> - x"
add("c2-quote", "> # x");                          // → Paragraph "> # x"
add("c2-quote", "> #");                            // bare # → Paragraph
add("c2-quote", "> - ");                           // → Paragraph "> - "
add("c2-quote", "> id:: b");                       // id:: trigger → Paragraph
add("c2-quote", "> key:: b");                      // non-id property → Quote
add("c2-quote", "> * x");                          // → Quote[List]
add("c2-quote", "> + x");                          // → Quote[List]
add("c2-quote", "> 1. x");                         // → Quote[List ordered]
add("c2-quote", "> 12. x");                        // multi-digit ordered
add("c2-quote", "> 1) x");                         // `1)` not a list marker → Quote[para]
add("c2-quote", "> a");                            // plain → Quote[Paragraph]
add("c2-quote", "> #x");                           // `#x` tag (no space) → Quote
add("c2-quote", "> ## x");                         // `##` (not single) → Quote[para "## x"]
add("c2-quote", "> -x");                           // `-x` (no space) → Quote[para]
add("c2-quote", ">");                              // lone > → Paragraph
add("c2-quote", "> ");                             // > + ws → Paragraph
add("c2-quote", `> - ((${QU}))`);                  // marker line keeps the block ref
add("c2-quote", `> ((${QU}))`);                    // quote keeps the block ref
add("c2-quote", "> [[Foo]]");                      // quote keeps the page ref
add("c2-quote", "> * [[Foo]]");                    // Quote[List] with page ref
add("c2-quote", "> a\n> - b");                     // Quote then Paragraph "> - b"
add("c2-quote", "> - a\n> b");                     // Paragraph then Quote
add("c2-quote", "> a\n> - b\n> c");                // Quote, Paragraph, Quote
add("c2-quote", "> * a\n> * b");                   // Quote[List a,b]
add("c2-quote", "> a\n> * b");                     // Quote[Para a (no trailing break), List]
add("c2-quote", "> * a\n> b");                     // Quote[List, Para b]
add("c2-quote", "> * a\n> ## b");                  // Quote[List, Para "## b"]
add("c2-quote", "> a\n> b");                       // Quote[Para a,break,b,break]
add("c2-quote", "> a\n>\n> b");                    // lone > continues (blank break)
add("c2-quote", "> a\nb");                         // lazy continuation
add("c2-quote", "> a\n- b");                       // lazy `- ` stops the quote
add("c2-quote", "> a\n* b");                       // lazy `* ` absorbed → List in quote
add("c2-quote", "> a\nid:: b");                    // lazy id:: → property after quote
add("c2-quote", "> > x");                          // nested > flattens (strip both)
add("c2-quote", "> > - x");                        // nested trigger → Paragraph
add("c2-quote", "> a\n> b\n> * c");                // Para run [a,b] (no trailing break)+List
add("c2-quote", "> a\n> * b\n> c");                // Para, List, Para
add("c2-quote", "> * ");                           // empty list marker → Quote[Para "* "]

// C3 — markdown table over-detection (audit C3): a table row must (after trimming) start
// AND end with `|`; a bare leading `|` is a Paragraph (and emits no phantom refs).
add("c3-table", "|a");                             // → Paragraph (not Table)
add("c3-table", "| a | b");                        // → Paragraph (not Table)
add("c3-table", `|((${QU}))x`);                    // → Paragraph, NO false block ref
add("c3-table", "|a|");                            // → Table (control)
add("c3-table", "|a|b|");                          // → Table (control)
add("c3-table", "  |a|b|  ");                      // trimmed → Table
add("c3-table", "a|b");                            // → Paragraph (control)
add("c3-table", "|a|\n|b");                        // Table (header) + Paragraph "|b"
add("c3-table", "|a|\n|b|");                       // Table with a body row

// C5 — CRLF / lone-CR line endings (audit C5): `\r\n` is one terminator (and `\r`, `\n`
// each yield a Break); a trailing `\r` never leaks into block content.
add("c5-eol", "# A\r\nB");                         // heading "A" + paragraph "B"
add("c5-eol", "a\rb");                             // [a, Break, b]
add("c5-eol", "a\r\nb");                           // [a, Break, Break, b]
add("c5-eol", "a\r");                              // [a, Break]
add("c5-eol", "line1\r\nline2\r\n");               // both lines + trailing breaks
add("c5-eol", "- a\r\n- b");                       // bullets across CRLF
add("c5-eol", "# H\r\n");                          // heading with CRLF terminator

// C7 — block-level Clojure-hiccup `[:tag …]` (md). A whole-line hiccup → Hiccup block;
// the remainder past the `]` re-enters block parsing at BOL; other constructs shield it.
add("c7-hiccup", "[:div]");                        // whole line → Hiccup block
add("c7-hiccup", "[:span]");
add("c7-hiccup", "[:foo]");                        // not a tag → Paragraph
add("c7-hiccup", "  [:div]");                      // leading ws absorbed → Hiccup
add("c7-hiccup", "\t[:div]");                      // leading tab → Hiccup
add("c7-hiccup", "    [:div]");                    // 4 spaces still → Hiccup (no CM code)
add("c7-hiccup", "[:div]x");                       // Hiccup + Paragraph "x"
add("c7-hiccup", "[:div] x");                      // Hiccup + Paragraph " x"
add("c7-hiccup", "[:div]# h");                     // Hiccup + Heading "h" (remainder at BOL)
add("c7-hiccup", "[:div]- x");                     // Hiccup + Bullet "x"
add("c7-hiccup", "[:div]* x");                     // Hiccup + List
add("c7-hiccup", "[:div]> q");                     // Hiccup + Quote
add("c7-hiccup", "[:div]key:: v");                 // Hiccup + Properties
add("c7-hiccup", "[:div][:span]");                 // two Hiccup blocks
add("c7-hiccup", "[:div][:span]x");                // Hiccup, Hiccup, Paragraph
add("c7-hiccup", "[:div]\nmore");                  // Hiccup + Paragraph "more"
add("c7-hiccup", "[:div]\n: def");                 // Hiccup + Paragraph (NOT def-list)
add("c7-hiccup", "[:div [:span\n]]");              // multi-line balanced capture → one Hiccup
add("c7-hiccup", "[:div\n]");                      // newline after name → Paragraph (gate fail)
add("c7-hiccup", "foo\n[:div]\nbar");              // Para, Hiccup, Para (breaks paragraph)
add("c7-hiccup", "foo\nbar\n[:div]");              // Para[foo,bar], Hiccup
add("c7-hiccup", "```\n[:div]\n```");              // fenced → Src (hiccup shielded)
add("c7-hiccup", "#+BEGIN_QUOTE\n[:div]\n#+END_QUOTE"); // Quote[Hiccup] via recursive parse
add("c7-hiccup", "> [:div]");                      // Quote[Hiccup] (quote body)
add("c7-hiccup", "> a\n> [:div]");                 // Quote[Para a, Hiccup]
add("c7-hiccup", "> [:div]\n> b");                 // Quote[Hiccup, Para b]
add("c7-hiccup", "> [:div]x");                     // Quote[Hiccup, Para x]
add("c7-hiccup", "- [:div]");                      // Bullet (hiccup is the title, inline)
add("c7-hiccup", "* [:div]");                      // List (item content → Hiccup block)
add("c7-hiccup", "[:div]\n\nx");                   // hiccup absorbs blank line → Para[x]
add("c7-hiccup", "[:div]\n\n\nx");                 // absorbs multiple blank lines
add("c7-hiccup", "[:div]x\n\ny");                  // same-line remainder keeps the blank
add("c7-hiccup", "[:div]\n  \nx");                 // whitespace-only line NOT absorbed
add("c7-hiccup", "[:div]\n\n# h");                 // absorb then heading
add("c7-hiccup", "* [:div]x");                     // list item content [Hiccup, Para x]
add("c7-hiccup", "* a [:div] b");                  // list item inline hiccup

// fence/container STRADDLE: a ``` inside a callout/drawer body must NOT pair with a ```
// outside it (the v0.1.3 global-pair_fences bug: `quote, paragraph` instead of `quote, src`).
// On-demand context-aware fence pairing (the block rewrite) fixes this.
add("fence-straddle", "#+BEGIN_QUOTE\n```\n#+END_QUOTE\n```\nx\n```");   // quote, src
add("fence-straddle", ":LOGBOOK:\n```\n:END:\n```\ny\n```");            // drawer, src
add("fence-straddle", "#+BEGIN_NOTE\n```\n#+END_NOTE\n```\nz\n```");    // custom callout, src
add("fence-straddle", "```\n#+BEGIN_QUOTE\n```\n#+END_QUOTE");          // src(body has #+BEGIN), then para
add("fence-straddle", "#+BEGIN_QUOTE\n```\n```\n#+END_QUOTE\n```\nx\n```"); // quote[src], then src

// phantom-opener regression: a `#+BEGIN_X`/`:NAME:` lexically INSIDE a fenced (opaque)
// body is CONTENT, not an opener — it must NOT steal the closer of a genuine container
// that follows the fence. The buggy global pending-opener pre-pass registered it and
// dropped the real container (`src, paragraph` instead of `src, quote` / `src, properties`).
add("phantom-opener", "```\n#+BEGIN_QUOTE\n```\n#+BEGIN_QUOTE\nx\n#+END_QUOTE");  // src, quote
add("phantom-opener", "```\n:LOGBOOK:\n```\n:PROPERTIES:\n:ID: a\n:END:");        // src, properties

// audit-r2 closer-finding divergences (all confirmed vs mldoc):
// (1) empty-name `#+BEGIN_` (or leading-ws `#+BEGIN_ X`) is NOT a block — mldoc → paragraphs,
//     NOT a phantom `custom name:""` block that swallows an intervening drawer.
add("empty-name", "#+BEGIN_\nkey:: val\n#+END_QUOTE");   // paragraph, properties, paragraph
add("empty-name", "#+BEGIN_ QUOTE\nx\n#+END_QUOTE");      // paragraph (leading-ws name)
// (2) a fence closes on the first later 3+ run of EITHER char (length/info-agnostic), not same-char.
add("mixed-fence", "~~~\na\n```\nb");                     // src("a"), paragraph
add("mixed-fence", "```\na\n~~~~~ info\nb");              // src("a"), paragraph
// (3) the fence marker is EXACTLY 3 chars; extra run chars belong to the lang/info.
add("fence-lang", "````js\na\n````");                     // src lang="`js"
add("fence-lang", "~~~~\na\n~~~~");                        // src lang="~"

// FOR-TINE wire contract (md): facets Tine reads off the one lsdoc parse per block.
// (2) trailing `key:: value` ⇒ Properties, fence-aware (key:: inside src is CODE, not a prop).
add("tine-props", "- foo\nkey:: val");                    // bullet, properties{key}
add("tine-props", "```\nkey:: val\n```");                 // src (key:: is code, NOT a property)
// (3) Timestamp ONLY for a standalone planning line — NEVER inside inline code / a fence.
add("tine-ts", "SCHEDULED: <2026-01-01 Thu>");            // paragraph[Timestamp{Scheduled}]
add("tine-ts", "DEADLINE: <2026-01-01 Thu>");             // paragraph[Timestamp{Deadline}]
add("tine-ts", "`SCHEDULED: <2026-01-01 Thu>`");          // paragraph[Code] — NOT a timestamp
add("tine-ts", "```\nSCHEDULED: <2026-01-01 Thu>\n```");  // src — NOT a timestamp

// === Audit correctness fixes (subagent-tasks/notes/audit-correctness-opus-rewrite.md) ===
// F6: md Quote/Custom absorb the trailing blank line (mldoc `<* optional eols`).
add("absorb-quote", "> q\n\ntext");                       // quote, paragraph["text"]
add("absorb-quote", "> q\n\n# h");                        // quote, heading
add("absorb-quote", "> q\nlazy\n\nafter");                // quote, paragraph["after"]
add("absorb-callout", "#+BEGIN_QUOTE\nx\n#+END_QUOTE\n\ntext"); // quote, paragraph["text"]
add("absorb-callout", "#+BEGIN_FOO\nx\n#+END_FOO\n\ntext");     // custom, paragraph["text"]
// F3: `:PROPERTIES:` is all-or-nothing — any non-`:key: value` body line → generic Drawer.
add("props-allornothing", ":PROPERTIES:\nfoo\n:END:");          // drawer "properties"
add("props-allornothing", ":PROPERTIES:\n:k: v\nfoo\n:END:");   // drawer (one bad line)
add("props-allornothing", ":PROPERTIES:\n:k: v\n\n:END:");      // drawer (blank body line)
add("props-allornothing", ":PROPERTIES:\nkey:: v\n:END:");      // drawer (md `key::` ≠ `:key:`)
add("props-allornothing", ":PROPERTIES:\n:k: v\n:m: w\n:END:"); // properties (all valid)
// F1: md blockquote body uses the FULL block grammar MINUS {heading,bullet,property,footnote,drawer}.
add("quote-body", "> q\n---");                            // quote[ P[q], hr ]
add("quote-body", "> q\n| a | b |");                      // quote[ P[q], table ]
add("quote-body", "> q\n$$x$$");                          // quote[ P[q], displayed_math ]
add("quote-body", "> q\n<div>x</div>");                   // quote[ P[q], raw_html ]
add("quote-body", "> q\n```\ncode\n```");                 // quote[ P[q], src ]
add("quote-body", "> q\n\\begin{eq}a\\end{eq}");          // quote[ P[q,Break], latex_env, P[Break] ]
add("quote-body", "> cont\n#+BEGIN_QUOTE\n#+END_QUOTE");  // quote[ P[cont], quote[] ] — no phantom refs
add("quote-body", "> a:: b");                             // quote[ P ] — property stays text
add("quote-body", "> # h");                               // quote[ P ] — heading stays text
add("quote-body", "> - x");                               // quote[ P ] — dash bullet stays text
// F5: a `>` on a CONTINUATION line nests a child Quote (one `>` stripped/line, not flattened).
add("quote-nest", "> a\n> > b");                          // quote[ P[a], quote[ P[b] ] ]
add("quote-nest", "> a\n> > b\n> > > c");                 // quote[ P[a], quote[ P[b], quote[ P[c] ] ] ]
// F4: an empty `## `/`- ` marker's trailing-ws paragraph is DROPPED before a following block.
add("empty-marker-drop", "## \n```\ncode\n```");          // heading, src (no spurious paragraph)
add("empty-marker-drop", "## \n---");                     // heading, hr
add("empty-marker-drop", "## \n| a | b |");               // heading, table
add("empty-marker-drop", "- \n#+BEGIN_QUOTE\n#+END_QUOTE"); // bullet, quote
add("empty-marker-drop", "## \nplain");                   // heading, paragraph (real content KEPT)
add("empty-marker-drop", "## \n* x");                     // heading, paragraph[" "], list (KEPT before list)

// === 3 more md grammar divergences (subagent-tasks/fix-3-more.md) ===
// M1: a md `#+BEGIN_X` callout body (Quote OR Custom) is block-parsed with the SAME
// in-block-content grammar as a `>`-blockquote body — suppress {heading,bullet,property,
// footnote,drawer} (→ paragraph text) and trim a paragraph's trailing Break before a block.
add("m1-callout-suppress", "#+BEGIN_FOO\n# h\n#+END_FOO");         // custom[ P "# h" ] (heading→text)
add("m1-callout-suppress", "#+BEGIN_FOO\n[^1]: b\n#+END_FOO");     // custom[ P[fnref,": b"] ] (footnote→text)
add("m1-callout-suppress", "#+BEGIN_FOO\n- x\n#+END_FOO");         // custom[ P "- x" ] (bullet→text)
add("m1-callout-suppress", "#+BEGIN_FOO\nk:: v\n#+END_FOO");       // custom[ P "k:: v" ] (property→text)
add("m1-callout-suppress", "#+BEGIN_FOO\n:NAME:\nx\n:END:\n#+END_FOO"); // custom[ P ] (drawer→text)
add("m1-callout-suppress", "#+BEGIN_QUOTE\n# h\n#+END_QUOTE");     // quote[ P "# h" ] (same grammar)
add("m1-callout-suppress", "#+BEGIN_QUOTE\n- x\n#+END_QUOTE");     // quote[ P "- x" ]
add("m1-callout-keep", "#+BEGIN_FOO\n* x\n#+END_FOO");             // custom[ list ] — list still recognized
add("m1-callout-keep", "#+BEGIN_FOO\n| a | b |\n#+END_FOO");       // custom[ table ] — table still recognized
add("m1-callout-keep", "#+BEGIN_FOO\n```\ncode\n```\n#+END_FOO");  // custom[ src ] — fence still recognized
add("m1-callout-keep", "#+BEGIN_FOO\n---\n#+END_FOO");             // custom[ hr ] — hr still recognized
add("m1-callout-trim", "#+BEGIN_FOO\nintro\n```\ncode\n```\n#+END_FOO"); // custom[ P[intro] (no break), src ]
add("m1-callout-trim", "#+BEGIN_FOO\nintro\n* item\n#+END_FOO");   // custom[ P[intro] (no break), list ]
// M2: a valid md `:PROPERTIES:` drawer folds a following `many(property | directive)` run into
// the SAME props — directives also swallow surrounding blank lines (`optional eols`).
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\n#+b: 2");        // properties[[k,v],[b,2]]
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\n#+b: 2\n#+c: 3"); // properties[[k,v],[b,2],[c,3]]
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\nx:: 1\n#+b: 2"); // properties[[k,v],[x,1],[b,2]]
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\nx:: 1\ny:: 2");  // properties[[k,v],[x,1],[y,2]]
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\n\n#+b: 2");      // properties[[k,v],[b,2]] (blank absorbed)
add("m2-props-fold", ":PROPERTIES:\n:k: v\n:END:\n#+b: 2\n\nplain"); // properties[[k,v],[b,2]], paragraph[plain]
add("m2-props-nofold", ":PROPERTIES:\n:k: v\n:END:\nplain");      // properties[[k,v]], paragraph[plain] (plain stops)
add("m2-props-nofold", ":PROPERTIES:\n:k: v\n:END:\nx:: 1\n\ny:: 2"); // props[[k,v],[x,1]], P[break], props[[y,2]]
// M3: the empty-marker ws-drop survives across a TRULY-EMPTY line — the marker's `" \n"` is
// dropped before a block, but intervening blank line(s) become their own break-paragraph.
add("m3-drop-across-blank", "## \n\n```\nx\n```");        // heading, paragraph[Break], src
add("m3-drop-across-blank", "## \n\n---");                // heading, paragraph[Break], hr
add("m3-drop-across-blank", "## \n\n| a | b |");          // heading, paragraph[Break], table
add("m3-drop-across-blank", "## \n\n#+BEGIN_QUOTE\n#+END_QUOTE"); // heading, paragraph[Break], quote
add("m3-drop-across-blank", "## \n\n> q");                // heading, paragraph[Break], quote
add("m3-drop-across-blank", "## \n\n$$x$$");              // heading, paragraph[Break], displayed_math
add("m3-drop-across-blank", "## \n\n<div>x</div>");       // heading, paragraph[Break], raw_html
add("m3-drop-across-blank", "- \n\n> q");                 // bullet, paragraph[Break], quote
add("m3-drop-across-blank", "## \n\n\n```\nx\n```");      // heading, paragraph[Break,Break], src (two blanks)
add("m3-keep-ws", "## \n\nplain");                        // heading, paragraph[" ",Break,Break,plain] (plain keeps ws)
add("m3-keep-ws", "## \n  \n```\nx\n```");                // heading, paragraph[" ",Break,Hardbreak], src (ws-line keeps ws)

// === md standalone `#+name: value` directive (subagent-tasks/fix-md-directive.md) ===
// A bare `#+KEY: value` line is a `Block::Directive{name, value}` with a RAW value (no inline
// parse, no ref walk) — identical to mldoc and to lsdoc's OWN org driver. The md driver had no
// standalone-directive parser, so it mis-classified the line as a Paragraph whose `#+name`
// became a phantom `+name` page-tag. Mirrors `crate::org::directive` byte-for-byte.
add("md-directive", "#+TITLE: my title");                 // directive{TITLE,"my title"}
add("md-directive", "#+b: 2");                            // directive{b,"2"}
add("md-directive", "#+key:value");                       // directive{key,"value"} (no space after `:`)
add("md-directive", "#+key:");                            // directive{key,""} (empty value)
add("md-directive", "#+CAPTION: a *bold* b");             // directive{CAPTION,"a *bold* b"} (emphasis NOT parsed)
add("md-directive", "#+TITLE: x  ");                      // directive{TITLE,"x  "} (trailing ws KEPT, left-trim only)
add("md-directive-ref", "#+a: [[Page]]");                 // directive{a,"[[Page]]"}, refs {} — NO phantom ref (the bug)
add("md-directive-ws", "  #+a: 1");                       // directive{a,"1"} (leading ws tolerated)
add("md-directive-ws", "\t#+a: 1");                       // directive{a,"1"} (leading tab tolerated)
add("md-directive-seq", "#+a: 1\n#+b: 2");                // [directive{a,1}, directive{b,2}] (each line independent)
add("md-directive-seq", "#+a: 1\nplain");                 // [directive{a,1}, paragraph["plain"]] (does NOT absorb text)
add("md-directive-seq", "#+a: 1\n\nplain");               // [directive{a,1}, paragraph["plain"]] (blank absorbed)
add("md-directive-seq", "#+a: 1\n\n\nplain");             // [directive{a,1}, paragraph["plain"]] (two blanks absorbed)
add("md-directive-seq", "#+a: 1\nmore\n#+b: 2");          // [directive, paragraph[more,Break], directive]
add("md-directive-seq", "#+a: 1\nb:: 2");                 // [directive{a,1}, properties[[b,2]]] (property starts after)
// NEGATIVES: text before `#+`, and colon-free `#+BEGIN_X`/`#+END_X` stay non-directive (the block path).
add("md-directive-neg", "not #+a: 1");                    // paragraph (phantom `+a` tag EXPECTED here — mldoc too)
add("md-directive-neg", "#+END_X");                       // paragraph (no `:` → tag `+END_X`, mldoc parity)
add("md-directive-neg", "#+BEGIN_X");                     // paragraph (no `:` → tag `+BEGIN_X`, mldoc parity)
add("md-directive-neg", "#+END_X: foo");                  // directive{END_X,"foo"} (has `:`, not BEGIN_ — mldoc too)
// drop-trigger: a directive drops a preceding empty heading/bullet marker's trailing-ws paragraph.
add("md-directive-drop", "## \n#+a: 1");                  // [heading[], directive{a,1}] (marker ws dropped, no para)
add("md-directive-drop", "- \n#+a: 1");                   // [bullet[], directive{a,1}]
// directive IS in `block_content_parsers` — it fires inside a `>`-quote / `#+BEGIN_X` body too.
add("md-directive-inbody", "#+BEGIN_QUOTE\n#+a: 1\n#+END_QUOTE"); // quote[ directive{a,1} ]
add("md-directive-inbody", "#+BEGIN_X\n#+a: 1\n#+END_X");         // custom[ directive{a,1} ]
add("md-directive-inbody", "> #+a: 1");                          // quote[ directive{a,1} ]
// M2/F3 interaction: M2 still folds a directive into a VALID props drawer; F3 (generic drawer)
// leaves the trailing directive STANDALONE → [drawer, directive] (the new classifier handles it).
add("md-directive-m2", ":PROPERTIES:\n:k: v\n:END:\n#+b: 2");    // properties[[k,v],[b,2]] (one block — M2 fold)
add("md-directive-f3", ":PROPERTIES:\nfoo\n:END:\n#+b: 2");      // [drawer{properties}, directive{b,2}]

// === md `#+BEGIN_SRC` / `#+BEGIN_EXAMPLE` raw-body blocks (audit fix B) ===
// mldoc's markdown block parser (block0.ml, shared with org) maps `#+BEGIN_SRC`→Src{lang,
// code} and `#+BEGIN_EXAMPLE`→Example{code} (body indent-cleared, lang = the first token
// after the name) — NOT a generic Custom. The bullet title-lookahead splits `- #+BEGIN_SRC`
// into [empty bullet, Src/Example]. Trailing blank lines are swallowed. QUOTE/NOTE stay
// Quote/Custom (non-regression). EXPORT/COMMENT are DEFERRED (new projection kinds) — not here.
add("begin-src", "#+BEGIN_SRC python\nx=1\n#+END_SRC");          // src{lang:python, code:"x=1\n"}
add("begin-src", "#+BEGIN_SRC\nx=1\n#+END_SRC");                 // src{lang:"", code:"x=1\n"} (no lang)
add("begin-src", "#+BEGIN_SRC\n#+END_SRC");                      // src{lang:"", code:""} (empty body)
add("begin-src", "#+BEGIN_SRC js\nlet a = 1\nlet b = 2\n#+END_SRC"); // src multiline
add("begin-src", "#+BEGIN_SRC python\n    x=1\n    y=2\n#+END_SRC");  // body common-indent cleared
add("begin-src", "#+BEGIN_SRC clojure :results\n(inc 2)\n#+END_SRC"); // lang = first token only
add("begin-src", "#+begin_src python\nx=1\n#+end_src");          // case-insensitive name
add("begin-src", "text before\n#+BEGIN_SRC\nx\n#+END_SRC");      // paragraph then src
add("begin-src", "#+BEGIN_SRC\nx\n#+END_SRC\nafter");            // src then paragraph (no blank)
add("begin-src", "#+BEGIN_SRC\nx\n#+END_SRC\n\ny");              // src then paragraph (blank swallowed)
add("begin-example", "#+BEGIN_EXAMPLE\nhello\n#+END_EXAMPLE");   // example{code:"hello\n"}
add("begin-example", "#+BEGIN_EXAMPLE\nline1\nline2\n#+END_EXAMPLE"); // example multiline
add("begin-example", "#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE\n\ny");  // example then paragraph (blank swallowed)
add("begin-bullet", "- #+BEGIN_SRC python\n  x=1\n  #+END_SRC"); // [bullet, src] (re-bulleted Tine form)
add("begin-bullet", "- #+BEGIN_EXAMPLE\n  hi\n  #+END_EXAMPLE"); // [bullet, example]
// Re-bulleted `- #+BEGIN_<TYPE>` Custom/Quote openers (the general admonition fix): the bullet
// title-lookahead splits into [empty bullet, custom/quote], dispatched IDENTICALLY to the bare
// form (QUOTE→Quote, else→Custom{name lowercased}). The body is INDENT-CLEARED (mldoc block0.ml,
// the same first-line-indent rule SRC/EXAMPLE use) then reparsed with the block grammar.
add("begin-bullet-callout", "- #+BEGIN_NOTE\n  x\n  #+END_NOTE");          // [bullet, custom{note}]
add("begin-bullet-callout", "- #+BEGIN_TIP\n  this is a tip\n  #+END_TIP"); // [bullet, custom{tip}]
add("begin-bullet-callout", "- #+BEGIN_WARNING\n  w\n  #+END_WARNING");    // [bullet, custom{warning}]
add("begin-bullet-callout", "- #+BEGIN_QUOTE\n  q\n  #+END_QUOTE");        // [bullet, quote]
add("begin-bullet-callout", "- #+BEGIN_FOO\n  f\n  #+END_FOO");            // [bullet, custom{foo}] (unknown→Custom)
add("begin-bullet-callout", "- #+begin_note\n  x\n  #+END_NOTE");          // case-insensitive BEGIN + name
add("begin-bullet-callout", "- #+BEGIN_NOTE\nx\n#+END_NOTE");              // Tine real form (indent-0 continuation)
add("begin-bullet-callout", "- #+BEGIN_NOTE\n  a\n  b\n  #+END_NOTE");     // multi-line body, common indent cleared
add("begin-bullet-callout", "- #+BEGIN_TIP\n  x");                         // no matching END → normal bullet titled "#+BEGIN_TIP"
add("begin-nonreg", "#+BEGIN_QUOTE\nquoted\n#+END_QUOTE");       // quote (unchanged)
add("begin-nonreg", "#+BEGIN_NOTE\nnote body\n#+END_NOTE");      // custom{note} (unchanged)
add("begin-nest", "#+BEGIN_QUOTE\n#+BEGIN_SRC\nx\n#+END_SRC\n#+END_QUOTE"); // quote[ src ]

// viewframe: re-bulleted `- #+BEGIN_X` transformed bodies parsed via the zero-copy strip-view
// frame (P2 — no `block_code_texts` copy, no `reparse_block_content` recursion). The fuzz never
// generates leading indent or `\r\n` inside a re-bulleted block body, so these lock the de-indent
// + eol-normalization (leaf content built from the VIEWED, `\n`-joined lines). See lsdoc-viewframe-P2.
add("viewframe", "- #+BEGIN_QUOTE\n  hello\n  #+END_QUOTE");                          // basic indented
add("viewframe", "- #+BEGIN_NOTE\n  multi\n  line paragraph\n  #+END_NOTE");          // multi-line para
add("viewframe", "- #+BEGIN_WARNING\n  first\n\n  second\n  #+END_WARNING");          // blank in para run
add("viewframe", "- #+BEGIN_QUOTE\n  #+END_QUOTE");                                   // empty body
add("viewframe", "- #+BEGIN_QUOTE\n  - #+BEGIN_NOTE\n    nested\n    #+END_NOTE\n  #+END_QUOTE"); // nested re-bulleted
add("viewframe", "- #+BEGIN_FOO\n  a\n  #+END_FOO\n- #+BEGIN_BAR\n  b\n  #+END_BAR");  // siblings
add("viewframe", "text before\n- #+BEGIN_NOTE\n  body\n  #+END_NOTE\ntext after");    // surrounded
add("viewframe", "- #+BEGIN_QUOTE\n  \\begin{equation}\n  x^2\n  \\end{equation}\n  #+END_QUOTE"); // latex-env
add("viewframe", "- #+BEGIN_QUOTE\n  | a | b |\n  | 1 | 2 |\n  #+END_QUOTE");          // table
add("viewframe", "- #+BEGIN_QUOTE\n  line with [[link]] and #tag\n  #+END_QUOTE");    // refs de-indented
add("viewframe", "- #+BEGIN_QUOTE\r\n  hello\r\n  #+END_QUOTE\r\n");                   // CRLF
add("viewframe", "- #+BEGIN_QUOTE\r\n  aaa\r\n  bbb\r\n  #+END_QUOTE\r\n");             // CRLF multi-line
add("viewframe", "- #+BEGIN_QUOTE\n  - #+BEGIN_NOTE\n    - #+BEGIN_TIP\n      deep\n      body\n      #+END_TIP\n    #+END_NOTE\n  #+END_QUOTE"); // 3-deep
add("viewframe", "- #+BEGIN_QUOTE\n  aaa\nbbb\n  #+END_QUOTE");                        // under-indented continuation
add("viewframe", "- #+BEGIN_QUOTE\n  \\begin{eq}\n  a\n  \\end{eq}\n  after\n  #+END_QUOTE"); // latex then para
add("viewframe", "- #+BEGIN_QUOTE\n  [:div x]\n  para\n  #+END_QUOTE");                // hiccup + para
add("viewframe", "- #+BEGIN_QUOTE\r\n  - #+BEGIN_NOTE\r\n    x\r\n    #+END_NOTE\r\n  #+END_QUOTE\r\n"); // CRLF nested
add("viewframe", "- #+BEGIN_QUOTE\n  \\begin{a}\n  x\n  \\end{a}\n  \\begin{b}\n  y\n  \\end{b}\n  #+END_QUOTE"); // two latex
add("viewframe", "- #+BEGIN_QUOTE\n  a\n\n  #+END_QUOTE");                             // trailing blank
add("viewframe", "- #+BEGIN_QUOTE\r\n  \\begin{eq}\r\n  a\r\n  \\end{eq}\r\n  #+END_QUOTE\r\n"); // CRLF latex
add("viewframe", "- #+BEGIN_A\n  - #+BEGIN_B\n    - #+BEGIN_C\n      - #+BEGIN_D\n        z\n        #+END_D\n      #+END_C\n    #+END_B\n  #+END_A"); // 4-deep
add("viewframe", "- #+BEGIN_QUOTE\r\n  aaa\r\nbbb\r\n  #+END_QUOTE\r\n");               // CRLF + under-indent

// quoteframe: md `>`-blockquotes parsed as first-class `>`-container stack frames (P3c — no
// build_md_quote, no residual reparse for the staircase; both the normal and bullet-lazy entry
// points push a frame). Locks the opener-2/continuation-1 asymmetry, single-line ⌈N/2⌉, dynamic
// staircase, and the §3 lazy de-`>` reparse fallback. Fuzz never generates these shapes.
add("quoteframe", "> > > > x\n");                                   // single-line ⌈4/2⌉ = 2 quotes
add("quoteframe", "> x\n> y\n");                                    // lazy tail coalesces (one para)
add("quoteframe", "> - list\n");                                    // breaker first line → Paragraph
add("quoteframe", "> > a\n> > > b\n> > > > c\n");                   // staircase from depth 2
add("quoteframe", "> > ```\n> > code\n> > ```\n");                  // §3 fence at depth 2 (both-`>`)
add("quoteframe", "> #+BEGIN_NOTE\n> x\n> #+END_NOTE\n");           // §3 callout in quote
add("quoteframe", "> a\n> \\begin{eq}\n> x\n> \\end{eq}\n> b\n");   // §3 latex mid-quote
add("quoteframe", "> [:div [:span x]]\n");                         // §3 nested hiccup
add("quoteframe", "> a\n```\ncode\n```\n");                         // §3 lazy-no-`>` fence (global index)
add("quoteframe", "- > a\n  > > b\n");                             // bullet-lazy nested quote
add("quoteframe", "> a\r\n> b\r\n");                               // CRLF quote
add("quoteframe", "> café 中文 😀 x\n");                            // multibyte in quote
add("quoteframe", "> a\n\n> b\n");                                 // blank stops run → siblings
add("quoteframe", "> a\n\nplain\n");                              // blank then plain (swallow parity)
add("quoteframe", "> term\n> : def\n");                           // bare def-list in quote (byte-exact)
add("quoteframe", "- > ```\n  > code\n  > ```\n");                 // §3 fence in a bullet-lazy quote
add("quoteframe", "> > > a\n> > b\nplain\n");                      // deep staircase then plain
add("quoteframe", "> a\n>\n> b\n");                                // middle `>`-blank breaks tail
add("quoteframe", "> [:a][:b]\n");                                 // §3 consecutive block hiccups

// prefix-consume (A-md): md `>`-quotes via ONE per-line container-prefix walk (no Step::OpenQuote
// re-dispatch → `property` runs once, killing 1a; O(1) close → killing 1b). Locks the property/quote
// boundary (step-8 property BEFORE step-10 blockquote at the doc root) + the close/open collapse.
// See lsdoc-viewframe-A-design / lsdoc-single-pass-audit.
add("prefixconsume", ">>>>key:: val\n");                            // property (no-space key), NOT a quote
add("prefixconsume", "> key:: val\n");                             // quote (`> key` has a space → not a property)
add("prefixconsume", ">key:: val\n");                              // property (1 `>`, no space)
add("prefixconsume", "#+BEGIN_QUOTE\n>>>>key:: val\n#+END_QUOTE\n"); // in block content → property suppressed → quote
add("prefixconsume", "> a\n>>>>key:: val\n");                      // property line after a quote line
add("prefixconsume", ">http://x.com:: y\n");                       // colon-in-key ⇒ not a property ⇒ quote
add("prefixconsume", "> a\n> > b\n> c\n> > d\n");                  // de-nest then re-nest
add("prefixconsume", ">>>>>>>>y\n>>- x\n");                        // interior breaker closes many frames
add("prefixconsume", "> a\n>\n>\n> b\n");                          // double `>`-blank (F6 swallow)
add("prefixconsume", ">>x\n>>>>y\n>>>>>>z\n");                     // depth increases per line
add("prefixconsume", "- > > a\n  > > b\n");                        // bullet-lazy nested quote
add("prefixconsume", ">>>>x\n> y\n");                              // single-line ⌈4/2⌉ then a continuation

const out = cases.map((c, idx) => ({ id: `b${String(idx).padStart(3, "0")}`, cat: c.cat, input: c.input }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.blocks.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} block corpus inputs`);

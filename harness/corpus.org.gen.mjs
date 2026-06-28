// Hand-written adversarial Org corpus (format:"org"). Targets Org-specific syntax
// and boundary rules. Output: corpus.org.json = [{id:o###, cat, input, format:"org"}].
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const U1 = "11111111-1111-1111-1111-111111111111";
const cases = [];
const add = (cat, input) => cases.push({ cat, input, format: "org" });

// headlines (levels, markers, priority, tags)
add("head", "* Heading");
add("head", "** Sub");
add("head", "*** Deep");
add("head", "*no space");          // not a headline (no space)
add("head", "* TODO task");
add("head", "* DONE done");
add("head", "* DOING [#A] urgent");
add("head", "* TODO [#B] with :tag1:tag2:");
add("head", "* plain :only:tags:");
add("head", "*  extra spaces");
add("head", "* TODO");             // marker, empty title

// emphasis (org markers + boundary rules)
add("emph", "*bold* and /italic/");
add("emph", "_underline_ +strike+");
add("emph", "~code~ and =verbatim=");
add("emph", "^^highlight^^");
add("emph", "a/b/c paths stay literal");   // no italic
add("emph", "snake_case_var literal");      // no underline
add("emph", "2*3*4 literal");               // no bold
add("emph", "nested /it *bo* it/");
add("emph", "*bold spanning\nnewline*");

// links
add("link", "[[target]]");
add("link", "[[target][label]]");
add("link", "[[https://orgmode.org][site]]");
add("link", "[[https://x.org]]");
add("link", `[[id:${U1}]]`);
add("link", "[[a]] and [[b][c]]");

// timestamps
add("ts", "met <2026-06-26 Fri>");
add("ts", "logged [2026-06-20 Sat]");
add("ts", "* h\nSCHEDULED: <2026-06-26 Fri>");
add("ts", "* h\nDEADLINE: <2026-07-01 Wed>");
add("ts", "range <2026-06-26 Fri>--<2026-06-28 Sun>");

// directives / keywords
add("kw", "#+TITLE: my title");
add("kw", "#+FILETAGS: :a:b:");
add("kw", "#+ICON: 🚀");
add("kw", "#+AUTHOR: someone");

// blocks
add("block", "#+BEGIN_SRC clojure\n(defn x [])\n#+END_SRC");
add("block", "#+BEGIN_QUOTE\nquoted text\n#+END_QUOTE");
add("block", "#+BEGIN_EXAMPLE\nliteral\n#+END_EXAMPLE");
add("block", "#+BEGIN_SRC\n* star line stays code\n#+END_SRC");

// drawers / properties
add("drawer", ":PROPERTIES:\n:key: value\n:END:");
add("drawer", ":PROPERTIES:\n:key: value\n:another: 2\n:END:");
add("drawer", ":LOGBOOK:\nCLOCK: [2026-06-14]\n:END:");
add("drawer", "* h\n:PROPERTIES:\n:id: x\n:END:");

// lists / tables
add("list", "- milk\n- eggs");
add("list", "+ plus item");
add("list", "1. first\n2. second");

// nested org lists. NB: org `-` is a list item only at column 0, so an indented
// `  - x` is NOT a list line (it can't nest); `+` and `N.` DO nest via indent.
add("nest", "+ a\n  + b");                 // b nested under a
add("nest", "+ a\n  + b\n    + c");        // a > b > c (3 levels)
add("nest", "+ a\n + b");                  // 1-space indent still nests
add("nest", "+ a\n+ b");                   // equal indent → siblings
add("nest", "1. a\n   2. b");             // numbered nests
add("nest", "1. a\n   2. b\n   3. c");     // b,c siblings under a
add("nest", "- a\n  1. b");               // `-` parent (col0) + numbered child
add("nest", "+ a\n    + deep\n  + mid");   // mid (indent 2) is a TOP sibling of a
add("table", "| a | b |\n|---+---|\n| 1 | 2 |");

// footnotes
add("fn", "see [fn:1] ref");
add("fn", "[fn:1] the definition");

// plain / boundary
add("misc", "a plain paragraph\nsecond line");
add("misc", "* parent\n** child with /em/ and [[link]]");
add("misc", "");
add("misc", "   ");

// --- M6 fuzz-hardening regressions (probed against mldoc format:"Org") ---------
// (1) `:`-prefixed lines are Org fixed-width Example blocks (NOT a recognized
//     `:NAME: … :END:` drawer). content = after the `:`, leading ws stripped.
add("verbatim", ": text");                 // Example "text"
add("verbatim", ":text");                  // Example "text" (no space)
add("verbatim", ": ");                      // Example ""
add("verbatim", ":");                       // Example ""
add("verbatim", ":  double");              // Example "double" (all leading ws)
add("verbatim", ": a b  ");                // Example "a b  " (trailing kept)
add("verbatim", ":key: value");            // standalone "property" → Example
add("verbatim", ":tag1:tag2:");            // Example
add("verbatim", ":END:");                   // bare :END: → Example
add("verbatim", ":PROPERTIES:");           // unclosed drawer head → Example
add("verbatim", ": line1\n: line2\n: line3"); // one Example, 3 lines
add("verbatim", "  : indented");           // leading ws before `:` → Example
add("verbatim", ": text\n:NAME:\ncontent\n:END:"); // Example[text,NAME:] + para + Example[END:]
// these must STAY drawers (not verbatim):
add("verbatim", ":PROPERTIES:\n:key: value\n:END:");          // Property_Drawer
add("verbatim", ":LOGBOOK:\nCLOCK: x\n:END:");                // Drawer
add("verbatim", ":PROPERTIES:\n:a: 1\n:END:\n:more: stuff");  // drawer + Example

// (2) footnote definition needs a non-empty body whose first char doesn't begin a
//     block construct (`* # [ -`); else it's an inline ref in a Paragraph.
add("fndef", "[fn:1]");                     // Paragraph (bare ref)
add("fndef", "[fn:1]   ");                 // Paragraph (no body)
add("fndef", "[fn:1] body");               // Footnote_Definition
add("fndef", "[fn:1]body");                // Footnote_Definition (no space)
add("fndef", "[fn:1]:x");                  // Footnote_Definition
add("fndef", "[fn:1]*x");                  // Paragraph (forbidden first char)
add("fndef", "[fn:1]#x");                  // Paragraph
add("fndef", "[fn:1][x");                  // Paragraph
add("fndef", "[fn:1]-x");                  // Paragraph
add("fndef", " [fn:1] body");              // Footnote_Definition (leading ws ok)

// (3) empty list marker → Paragraph; `- [ ]` (checkbox, no content) → Paragraph.
add("list-empty", "+ ");                    // Paragraph
add("list-empty", "- ");                    // Paragraph
add("list-empty", "1. ");                   // Paragraph
add("list-empty", "- [ ]");                // Paragraph
add("list-empty", "- [ ] x");              // List
add("list-empty", "+ x");                   // List
// `-` is a bullet only at column 0; indented `-` is a Paragraph (mldoc quirk),
// while indented `+`/`N.` stay Lists.
add("list-indent", "  - x");               // Paragraph
add("list-indent", "  + y");               // List (indent 2)
add("list-indent", "  1. z");              // List (indent 2)

// (4) malformed table (row must start AND end with `|`) → Paragraph.
add("table-bad", "| a | b");               // Paragraph (no closing pipe)
add("table-bad", "|a");                     // Paragraph
add("table-bad", "|");                      // Paragraph (single pipe)
add("table-bad", "| a | b |");             // Table
add("table-bad", "||");                     // Table (one empty cell)
add("table-bad", "| a | b |\n| c | d");    // Table + Paragraph

// (5) directive: leading whitespace allowed; value is LEFT-trimmed only (mldoc keeps
//     trailing whitespace).
add("directive", "#+TITLE: hello  ");      // value "hello  "
add("directive", "  #+TODO: x");           // directive (leading ws)
add("directive", "#+a:b:c");               // key "a", value "b:c"

// (6) empty-title headline with trailing whitespace → Bullet + Paragraph(leftover ws).
add("head-ws", "*** ");                     // Bullet + Paragraph[" "]
add("head-ws", "* TODO ");                  // Bullet(TODO) + Paragraph[" "]
add("head-ws", "*   ");                     // Bullet + Paragraph["   "]
add("head-ws", "* \nreal content");        // Bullet + Paragraph[" ", Break, "real content"]
add("head-ws", "*** \n* B");               // Bullet + Paragraph[" ", Break] + Bullet

// (7) render-level fields (§ render parity): org list checkboxes (-, +, N.),
// dedicated targets `<<…>>`, and org-link media metadata `{:width …}`.
add("checkbox", "- [ ] unchecked");
add("checkbox", "- [x] checked");
add("checkbox", "- [X] checked caps");
add("checkbox", "+ [ ] plus");
add("checkbox", "1. [ ] ordered\n2. [x] ordered done");
add("checkbox", "- plain item");
add("checkbox", "- [-] partial is literal");      // `[-]` is NOT a checkbox
// NOTE: org *nested* list cases (`- a\n  - b`) are intentionally NOT here — they hit
// a separate, pre-existing org multi-line list-continuation gap (see DECISIONS.md
// "Org multi-line list continuation"), unrelated to checkbox/render parity.
add("target", "see <<my target>> here");
add("target", "<<target>>");
add("target", "<<a>> and <<b>>");
add("target", "<<>>");                             // empty → not a target (literal)
add("target", "<< spaced >>");                     // inner spaces kept raw
add("target", "text <<no close here");             // unterminated → literal
add("link-meta", "[[../a.png][img]]{:width 100}"); // org_link_1 metadata
add("link-meta", "[[file:x.png][cap]]{:width 50, :height 20}");
add("link-meta", "[[../a.png]]{:height 40}");      // org_link_2: metadata NOT consumed
// (8) indented `*` is a LIST item (col-0 `*` is a headline) — mldoc lists.ml; the
// opposite of `-` (bullet only at col 0). Found by the 6B fuzz-reachability check
// as a real parity bug vs DECISIONS.md:393. `+`/`N.` were already correct.
add("istar", "  * x");                             // indented star → list (not paragraph)
add("istar", "    * deep");
add("istar", "* h\n  * a\n  * b");                  // headline + indented-star list
add("istar", "  * a\n    * b");                     // nesting via indent
add("istar", "  * [ ] task");                       // indented star + checkbox
add("istar", "  * ");                               // empty → paragraph (needs content)
add("istar", "* TODO task\n  * sub star");          // the 6B in-context repro

const out = cases.map((c, i) => ({ id: `o${String(i).padStart(3, "0")}`, cat: c.cat, input: c.input, format: c.format }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.org.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} org corpus inputs`);

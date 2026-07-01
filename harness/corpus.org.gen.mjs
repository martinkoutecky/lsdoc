// Hand-written adversarial Org corpus (format:"org"). Targets Org-specific syntax
// and boundary rules. Output: corpus.org.json = [{id:o###, cat, input, format:"org"}].
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const U1 = "11111111-1111-1111-1111-111111111111";
const cases = [];
const add = (cat, input) => cases.push({ cat, input, format: "org" });

// block/drawer pairing semantics (v2 pre-pairing must reproduce mldoc's greedy
// first-closer-of-name, non-overlapping, prefix-match behavior — see DESIGN-lsdoc-v2).
add("pairing", "#+BEGIN_FOO\n#+BEGIN_FOO\nx\n#+END_FOO\n#+END_FOO"); // nested same name: outer grabs FIRST #+END_FOO
add("pairing", "#+BEGIN_FOO\nx\n#+END_BAR");                          // mismatched END → no close
add("pairing", "#+BEGIN_FOO\n#+END_BAR\n#+END_FOO");                  // skip non-matching, close at #+END_FOO
add("pairing", "#+BEGIN_SRC\ncode\n#+END_SRC trailing");             // prefix-match: trailing junk after END name
add("pairing", ":A:\n:B:\nx\n:END:\n:END:");                          // nested drawers: first :END: closes :A:
add("pairing", "* #+BEGIN_FOO\nx\n#+END_FOO");                        // headline-split block opener (closes)
add("pairing", "* #+BEGIN_FOO\nx\n#+END_BAR");                        // headline-split block opener (no close)

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
add("kw", "#+END_中:");             // multibyte key after `#+END_` — must NOT panic (was a crash)
add("kw", "#+中: value");           // multibyte directive key
// NB: `#+begin_中:` deliberately NOT here — `#+begin…:` (a begin_-excluded key + colon, no
// `#+END`) hits mldoc's `#+key:value` Property_Drawer fallback, a documented adversarial
// residual (see DECISIONS.md M6 org fuzz-hardening); the panic fix is covered by `#+END_中:`.

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

// (2b) footnote body absorbs continuation lines (mldoc `footnote_definition = many1 l`)
//      until a footnote-specific terminator; absorbed lines join with Break_Line,
//      de-indented; trailing whitespace kept.
add("fncont", "[fn:1] body\ncont");                 // absorbed (basic continuation)
add("fncont", "[fn:1] body\ncont\nmore");           // absorbed (multi-line)
add("fncont", "[fn:1] body\n  indented");           // absorbed (de-indented)
add("fncont", "[fn:1] body\n\tcont");               // absorbed (tab de-indented)
add("fncont", "[fn:1] body\ncont  ");               // trailing spaces kept
add("fncont", "[fn:1] body\n+ x");                  // absorbed (`+` list folds as text)
add("fncont", "[fn:1] body\n1. x");                 // absorbed (`N.` folds as text)
add("fncont", "[fn:1] body\n| t |");                // absorbed (table as text)
add("fncont", "[fn:1] body\n> q");                  // absorbed (quote as text)
add("fncont", "[fn:1] body\n: ex");                 // absorbed (`:`-line as text)
add("fncont", "[fn:1] body\n<<target>>");           // absorbed (inline target)
add("fncont", "[fn:1] body\n:PROPERTIES:\n:k: v\n:END:"); // absorbed (drawer as text)
add("fncont", "[fn:1] body\n  + x");                // absorbed (indented `+` de-indented)
add("fncont", "[fn:1] body\ncont\n");               // absorbed, trailing newline swallowed
add("fncont", "[fn:1] body\n\ncont");               // TERMINATE: blank line → Paragraph
add("fncont", "[fn:1] body\n* h");                  // TERMINATE: headline
add("fncont", "[fn:1] body\n- x");                  // TERMINATE: col-0 `-` list
add("fncont", "[fn:1] body\n#+TITLE: x");           // TERMINATE: directive
add("fncont", "[fn:1] body\n#+BEGIN_SRC\nx\n#+END_SRC"); // TERMINATE: block opener
add("fncont", "[fn:1] body\n-----");                // TERMINATE: hr (`-` first char)
add("fncont", "[fn:1] body\n[fn:2] b");             // TERMINATE: `[` → inline ref Paragraph
add("fncont", "[fn:1] ab\n[fn:2] cd");              // TERMINATE: second Footnote_Definition
add("fncont", "[fn:1] body\nx");                    // TERMINATE: 1-byte continuation
add("fncont", "[fn:1] body\n  * x");                // TERMINATE: indented `*`
add("fncont", "[fn:1] body\n  - x");                // TERMINATE: indented `-`
// NOTE: indented `#` and a whitespace-only line also TERMINATE the footnote body, but
// the leftover line then hits PRE-EXISTING, footnote-unrelated divergences (indented-`#`
// comment classification; the absorb/whitespace-only-line swallow at the blank-line
// handler — same for directives), so they are documented in notes, not asserted here.
add("fncont", "[fn:1] body\ncont\n\n[fn:2] b");     // absorb cont, blank, then inline ref
add("fncont", "[fn:1] body\n  next\n+ keep\n- stop"); // absorb next/+keep, stop at col-0 `-`

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
// NOTE: org multi-line list continuation + collapse is now implemented; the regression
// cases live under the "list-cont"/"list-collapse" categories below.
// (9) Org list multi-line item continuation + indented-`-` collapse (mldoc lists0.ml).
//     Fold: an indented (>=1 space) non-marker continuation line de-indents (String.trim)
//     and joins the item's content (re-parsed with the list-item content parser, which
//     excludes Directive/Drawer/Heading/Footnote/List).
add("list-cont", "- a\n  more");                 // fold one continuation line
add("list-cont", "- a\n more");                  // 1-space indent still folds
add("list-cont", "- a\nmore");                   // col-0 ⇒ NOT folded (List + Paragraph)
add("list-cont", "- a\n  m1\n  m2");             // multi-line fold
add("list-cont", "- a\n  more\n- b");            // 2 items (a+fold, then b)
add("list-cont", "- a\n  more\n  more2\n- b");   // 2 items, multi-line fold
add("list-cont", "- a\n  more\n\n- b");          // blank between items absorbed (single List)
add("list-cont", "- a\n\n  more");               // blank then indent ⇒ List + Paragraph("  more")
add("list-cont", "- a\n b\nc");                  // List(a+b) + Paragraph(c)
add("list-cont", "+ a\n  more");                 // `+` folds
add("list-cont", "1. a\n   more");               // ordered folds
add("list-cont", "- [ ] a\n  more");             // checkbox + fold
add("list-cont", "- a\n    deep4\n  mid2");      // any indent>=1 folds (de-indented)
add("list-cont", "- a\n  more\n    deeper");     // deeper indent still folds
add("list-cont", "- a\n  more\n  ");             // trailing whitespace-only line folds (Break)
add("list-cont", "- a\n\tmore");                 // tab indent folds
add("list-cont", "- a\nb");                      // col-0 non-marker terminates (List + Para)
add("list-cont", "- a\n\nb");                    // blank consumed, then Paragraph(b)
add("list-cont", "- a\n\n\nb");                  // 2nd blank ⇒ Paragraph(Break, b)
add("list-cont", "  + x\n    more");             // list starting at indent>0 folds
// indented constructs fold as item content blocks (re-parsed without lists/drawers):
add("list-cont", "- a\n  > quote");              // → item content [Para a, Quote]
add("list-cont", "- a\n  : ex");                 // → [Para a, Example]  (NOT a drawer)
add("list-cont", "- a\n  | t |");                // → [Para a, Table]
add("list-cont", "- a\n  -----");                // → [Para a, Hr]
add("list-cont", "- a\n  ---");                  // indented `---` folds as text (List)
add("list-cont", "- a\n  #+TITLE: x");           // directive NOT split inside an item (one Para)
add("list-cont", "- a\n  :PROPERTIES:\n  :p: 1\n  :END:"); // drawer→verbatim Example in item
add("list-cont", "- a\n  [fn:1] body");          // footnote stays inline ref in item
add("list-cont", "- a\n  $$x$$");                // → [Para a, displayed_math]
add("list-cont", "- a\n  #+BEGIN_SRC\n  x\n  #+END_SRC"); // → [Para a, Src]
// col-0 terminators end the List (next block re-parsed normally):
add("list-cont", "- a\n  more\n* head");         // → List + Heading
add("list-cont", "- a\n  more\n#+TITLE: x");     // → List + Directive
add("list-cont", "- a\n  more\n-----");          // → List + Hr
add("list-cont", "- - x");                       // body "- x" is item content (no nested list)
add("list-cont", "- * x");                       // body "* x" is item content (not a heading)
add("list-cont", "1. - x");                      // ordered, body "- x" content

// (9b) indented-`-` (and unparseable deeper marker) COLLAPSE: whole region → Paragraph.
add("list-collapse", "- a\n  - nested");         // indented `-` ⇒ Paragraph
add("list-collapse", "+ a\n  - nested");         // collapse even for `+`
add("list-collapse", "1. a\n   more\n   - x");   // collapse mid-continuation (ordered)
add("list-collapse", "- a\n  - x\n  more");      // collapse (indented `-`)
add("list-collapse", "- a\n  more\n  - x");      // collapse after folding
add("list-collapse", "- a\n  + ");               // empty deeper marker ⇒ collapse
add("list-collapse", "- a\n  12abc");            // integer-prefixed (no `.`) ⇒ collapse
add("list-collapse", "- a\n  -5");               // `-5` is is_item but unparseable ⇒ collapse
add("list-collapse", "+ a\n  + b\n    - c");     // collapse propagates from a grandchild
add("list-collapse", "- a\n  - x\n* h");         // collapse Paragraph + Heading
add("list-collapse", "- a\n  - x\n\n- b");       // collapse Paragraph + (blanks) + List
// breakout (NOT collapse): an indented `-` at indent <= current item ⇒ List + Paragraph.
add("list-collapse", "+ a\n  + b\n  - c");       // → List(a[b]) + Paragraph("  - c")
add("list-collapse", "- a\n- ");                 // empty trailing marker ⇒ List + Paragraph
// PARTIAL collapse: items before the failing item survive as a List, the failing item
// onward becomes a Paragraph (mldoc's failure bubbles up only through first-at-level items).
add("list-collapse", "- a\n- b\n  - z");         // → List(a,b? no: List(a) + Para) ; kept=[a]
add("list-collapse", "- a\n- b\n- c\n  - z");     // → List(a,b) + Paragraph(c + trigger)
add("list-collapse", "+ a\n  + b\n  + c\n    - d"); // → List(a[b]) + Paragraph(c + trigger)
add("list-collapse", "+ p\n+ a\n  + b\n    - c"); // → List(p) + Paragraph(a..trigger)
add("list-collapse", "- a\n  more\n- b\n  - z");  // → List(a+more) + Paragraph(b + trigger)
add("list-collapse", "- a\n  - z\n- y\n  - w");   // two independent collapses ⇒ one Paragraph
add("list-collapse", "1. a\n2. b\n   - z");       // ordered partial collapse

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

// (9) org comments `# text` (mldoc Comment). Single `#` + ≥1 space + non-empty
// content (leading stripped, trailing kept). `#c`/`# `/`##`/`#+…` are NOT comments.
add("comment", "# c");                              // Comment "c"
add("comment", "# a comment");                      // Comment "a comment"
add("comment", "  # indented");                     // Comment "indented" (leading ws ok)
add("comment", "#  two spaces");                    // Comment "two spaces"
add("comment", "   # x  ");                         // Comment "x  " (trailing kept)
add("comment", "#c");                               // Paragraph (no space)
add("comment", "# ");                               // Paragraph (empty content)
add("comment", "##  two");                          // Paragraph (two hashes)
add("comment", "# a\n# b");                         // two Comment blocks
add("comment", "# a\nplain");                       // Comment + Paragraph
add("comment", "# note\n\nafter");                  // Comment absorbs the blank
add("comment", "- a\n# c");                         // List + Comment (col-0 terminates)
add("comment", "- a\n  # c");                       // Comment is in-item content
add("comment", "[fn:1] body\n# c");                 // footnote def + Comment (terminates)
add("comment", "[fn:1] body\n  # x");               // was the indented-# footnote residual

// (10) headline block-opener split (mldoc heading0.ml title lookahead): a headline
// whose post-marker CONTENT begins a block construct splits into [empty bullet, block]
// (the org analog of the md `-` bullet-opener split). The empty bullet KEEPS
// level/marker/priority, with an empty title and no htags. The 12 real-graph
// divergences were all `* #+TITLE: x` namespace pages (blockgate.mjs).
add("hlsplit", "* #+TITLE: x");                     // → [bullet, directive]
add("hlsplit", "* #+FOO:bar");                      // directive, no space after colon
add("hlsplit", "* #+KEY:");                         // directive, empty value
add("hlsplit", "** #+TITLE: x");                    // level-2 empty bullet
add("hlsplit", "*** #+TITLE: x");                   // level-3 empty bullet
add("hlsplit", "* TODO #+TITLE: x");                // empty bullet KEEPS marker TODO
add("hlsplit", "* TODO [#A] #+TITLE: x");           // KEEPS marker + priority
add("hlsplit", "* [#A] #+TITLE: x");                // KEEPS priority only
add("hlsplit", "* #+TITLE: x :a:b:");               // no htags — tags fold into the value
add("hlsplit", "* :PROPERTIES:\n:a: b\n:END:");     // → [bullet, properties]
add("hlsplit", "* :PROPERTIES:\n:a: b\n:END:\n#+FOO: bar"); // property folds directive
add("hlsplit", "* :LOGBOOK:\nx\n:END:");            // → [bullet, drawer]
add("hlsplit", "* :NAME:");                         // bare drawer → [bullet, example]
add("hlsplit", "* : text");                         // verbatim `:`-line → [bullet, example]
add("hlsplit", "* #+BEGIN_SRC\ncode\n#+END_SRC");   // → [bullet, src]
add("hlsplit", "* #+BEGIN_SRC js\ncode\n#+END_SRC");
add("hlsplit", "* #+BEGIN_QUOTE\nq\n#+END_QUOTE");  // → [bullet, quote]
add("hlsplit", "* #+BEGIN_FOO\nf\n#+END_FOO");      // → [bullet, custom]
add("hlsplit", "* | a | b |");                      // → [bullet, table]
add("hlsplit", "* | a | b |\n| c | d |");           // multi-row table
add("hlsplit", "* > quote");                        // md blockquote → [bullet, quote]
add("hlsplit", "* $$x$$");                          // → [bullet, displayed_math]
add("hlsplit", "* <div>x</div>");                   // → [bullet, raw_html]
add("hlsplit", "* [fn:1] body");                    // → [bullet, footnote_def]
add("hlsplit", "* -----");                          // org hr → [bullet, hr]
add("hlsplit", "* \\begin{x}\ny\n\\end{x}");        // → [bullet, latex_env]
add("hlsplit", "* \\begin{x}");                     // latex env consumes to EOF (splits)
add("hlsplit", "* ```\ncode\n```");                 // markdown fence → [bullet, src]
add("hlsplit", "* ~~~\nx\n~~~");                     // tilde fence → [bullet, src]
add("hlsplit", "* #+TITLE: x\n\ny");                // directive absorbs blank, then para
add("hlsplit", "* #+TITLE: x\n* Second");           // adjacent headline unaffected
// NON-splitters: content stays the heading title.
add("hlsplit", "* # comment");                      // comment is NOT a split
add("hlsplit", "* TODO task");                      // bare marker
add("hlsplit", "* #tag x");                         // a tag is not a directive
add("hlsplit", "* - item");                         // a list is not a split
add("hlsplit", "* ** x");                           // nested-headline content
add("hlsplit", "* #+BEGIN_SRC\ncode");              // UNCLOSED block ⇒ title, no split
add("hlsplit", "* ```\nx");                         // UNCLOSED fence ⇒ title, no split
add("hlsplit", "* [fn:1] a");                       // 1-byte footnote body ⇒ inline ref

// C2 — Org blockquote marker-line rules (audit C2): `>`+`- `/`# `/`id:: ` is a plain
// Paragraph; `>`+plain is a Quote[Paragraph]; `*` is NOT a headline inside a quote body
// (mldoc emits a Paragraph), while `-`/`+`/`N.` lists ARE parsed. Same for `#+BEGIN_QUOTE`.
add("c2q", "> - x");                               // → Paragraph "> - x"
add("c2q", "> # x");                               // → Paragraph "> # x"
add("c2q", "> a");                                 // → Quote[Paragraph]
add("c2q", "> * x");                               // → Quote[Paragraph "* x"] (NOT a headline)
add("c2q", "> ** x");                              // → Quote[Paragraph "** x"]
add("c2q", "> + x");                               // → Quote[List]
add("c2q", "> 1. x");                              // → Quote[List ordered]
add("c2q", "> a\n> b");                            // → Quote[Para a,break,b,break]
add("c2q", `> - ((${U1}))`);                       // marker line → Paragraph keeps the ref
add("c2q", "#+BEGIN_QUOTE\n* x\n#+END_QUOTE");     // headline suppressed → Paragraph "* x"
add("c2q", "#+BEGIN_QUOTE\n** y\n#+END_QUOTE");    // → Paragraph "** y"

// C4 — Org tags are NOT markdown-unescaped (audit C4): `#ab\|` keeps the backslash
// (tag/ref `ab\|`), matching Org's no-unescape invariant (md DOES unescape).
add("c4tag", "#ab\\|");                            // tag "ab\\|" (backslash kept)
add("c4tag", "#tag\\=x");                          // tag "tag\\=x"
add("c4tag", "#tag\\+x");                          // tag "tag\\+x"
add("c4tag", "#a\\b");                             // `\\`+letter kept by both (control)

// C5 — CRLF / lone-CR line endings (audit C5), Org side.
add("c5eol", "# A\r\nB");                          // `# A` is an Org comment + paragraph "B"
add("c5eol", "a\rb");                              // [a, Break, b]
add("c5eol", "a\r\nb");                            // [a, Break, Break, b]
add("c5eol", "a\r");                               // [a, Break]
add("c5eol", "* H\r\nbody");                       // headline across CRLF

// C6 — Org property-value refs use the ORG inline parser (audit C6): a malformed
// `[[x][y]]` value yields NO ref (Org Search link), while `[[Foo]]` yields ref Foo.
add("c6prop", `:PROPERTIES:\n:a: [[x][y]]\n:END:`);        // NO ref (was false ref "x][y")
add("c6prop", `:PROPERTIES:\n:a: [[Foo]]\n:END:`);         // ref Foo
add("c6prop", `:PROPERTIES:\n:tags: [[A]], [[B]]\n:END:`); // refs A, B

// C7 — Clojure-hiccup `[:tag …]` (org). Block at BOL & inline; allowlist; remainder;
// recognized inside list-item content too.
add("c7hiccup", "[:div]");                   // whole line → Hiccup block
add("c7hiccup", "[:foo]");                   // not a tag → Paragraph
add("c7hiccup", "x [:div] y");               // inline Hiccup
add("c7hiccup", "x [:foo] y");               // not a tag → plain
add("c7hiccup", "[:div]x");                  // Hiccup + Paragraph x
add("c7hiccup", "[:div][:span]");            // two Hiccups
add("c7hiccup", "[:DIV]");                   // case-insensitive → Hiccup
add("c7hiccup", 'x [:div "a]b"] y');         // string-protected `]`
add("c7hiccup", "[:div [:span]]");           // nested
add("c7hiccup", "/[:div]/");                 // inside emphasis → NOT hiccup (plain)
add("c7hiccup", "* [:div]");                 // headline → bullet (inline hiccup title)
add("c7hiccup", "- [:div]");                 // list → item content (Hiccup block)
add("c7hiccup", "- a\n  [:div]");            // item content [Para a, Hiccup]
add("c7hiccup", "> [:div]");                 // Quote[Hiccup]
add("c7hiccup", "#+BEGIN_SRC\n[:div]\n#+END_SRC"); // Src (shielded)
add("c7hiccup", ": [:div]");                 // verbatim Example (shielded)
add("c7hiccup", "[:div]\n\nx");              // hiccup absorbs blank line → Para[x]
add("c7hiccup", "[:div]\n  \nx");            // whitespace-only line NOT absorbed
add("c7hiccup", "> a\n> [:div]");            // Quote[Para a (no break), Hiccup]
add("c7hiccup", "> a\n> b\n> [:div]");       // Quote[Para a,b, Hiccup]
add("c7hiccup", "> a\n> [:div]\n> c");       // Quote[Para a, Hiccup, Para c]
add("c7hiccup", "- [:div]x");                // item content [Hiccup, Para x]

// fence/container STRADDLE (org): a ``` inside a #+BEGIN_X / :drawer: body must NOT pair
// with a ``` outside it (the old global-pair_fences `quote, paragraph` bug; should be
// `quote, src`). Context-aware on-demand fence pairing fixes it.
add("fence-straddle", "#+BEGIN_QUOTE\n```\n#+END_QUOTE\n```\nx\n```");  // quote, src
add("fence-straddle", ":LOGBOOK:\n```\n:END:\n```\ny\n```");           // drawer, src

// phantom-opener regression (org): a `:NAME:` lexically INSIDE a `#+BEGIN_SRC` body is
// opaque CONTENT, not a drawer opener — it must NOT steal the `:END:` of a genuine
// `:PROPERTIES:` drawer that follows. The buggy global pending-opener pre-pass registered
// the body `:LOGBOOK:` and dropped the real drawer (`src, example` instead of `src, properties`).
add("phantom-opener", "#+BEGIN_SRC org\n:LOGBOOK:\n#+END_SRC\n:PROPERTIES:\n:ID: abc\n:END:"); // src, properties

// audit-r2 closer-finding divergences (confirmed vs mldoc Org):
add("empty-name", "#+BEGIN_\n#+TITLE: x\n#+END_SRC");  // paragraph, directive, paragraph (empty name ≠ block)
add("mixed-fence", "~~~\na\n```\nb");                  // src("a"), paragraph (either-char close)
add("fence-lang", "~~~~\na\n~~~~");                     // src lang="~" (marker exactly 3 chars)

// FOR-TINE wire contract (org): org properties come from a `:PROPERTIES:` drawer, NOT `key::`.
add("tine-props", "* foo\n:PROPERTIES:\n:key: val\n:END:"); // bullet, properties{key}
add("tine-props", "* foo\nkey:: val");                      // bullet, paragraph (key:: is NOT a property in org)
add("tine-props", "#+BEGIN_SRC\nkey:: val\n#+END_SRC");     // src (key:: is code, NOT a property)

// quote-body paragraph drops its trailing Break before a FOLLOWING block (mldoc `between_eols`):
// the para keeps it only at the document level / as the body's LAST block. (Confirmed vs mldoc.)
add("quote-break", ">>a\n>>b");                  // Q{ P[a], Q{ P[b,Break] } } — NOT P[a,Break]
add("quote-break", "> a\n>> b\n>> c");           // Q{ P[a], Q{ P[b,Break,c,Break] } }
add("quote-break", "> a\n> #+BEGIN_SRC\n> x\n> #+END_SRC"); // Q{ P[a], src } — para before a block in a quote
add("quote-break", "> a\n> -----");             // Q{ P[a], hr }

// === Audit correctness fixes (subagent-tasks/notes/audit-correctness-opus-rewrite.md) ===
// F2: `#+BEGIN_X` / `>`-quote bodies suppress {headline, drawer, footnote-def} (→ text), while
// list/table/directive/comment/src/hr/example stay; a para drops its trailing Break before a block.
add("body-suppress", "#+BEGIN_FOO\n** head\n#+END_FOO");          // custom[ P["** head"] ]
add("body-suppress", "#+BEGIN_FOO\n* head\n#+END_FOO");           // custom[ P["* head"] ]
add("body-suppress", "#+BEGIN_FOO\n[fn:1] body\n#+END_FOO");      // custom[ P[fnref," body"] ]
add("body-suppress", "#+BEGIN_FOO\n:LOGBOOK:\ny\n:END:\n#+END_FOO");      // custom[ example, P, example ]
add("body-suppress", "#+BEGIN_QUOTE\n:LOGBOOK:\ny\n:END:\n#+END_QUOTE");  // quote[ example, P, example ]
add("body-suppress", "#+BEGIN_QUOTE\n[fn:1] body\n#+END_QUOTE");  // quote[ P[fnref," body"] ]
add("body-suppress", "> q\n:LOGBOOK:\nx\n:END:");                 // quote[ P[q], example, P, example ]
add("body-suppress", "> [fn:1] body");                           // quote[ P[fnref," body"] ]
add("body-suppress", "#+BEGIN_FOO\n- item\n#+END_FOO");          // custom[ list ] — list KEPT
add("body-suppress", "#+BEGIN_FOO\n#+TITLE: x\n#+END_FOO");       // custom[ directive ] — directive KEPT
add("body-suppress", "#+BEGIN_FOO\ntext\n- item\n#+END_FOO");    // custom[ P[text], list ] — Break trimmed
// F3: `:PROPERTIES:` all-or-nothing — any non-property body line → generic Drawer (no value refs).
add("props-allornothing", ":PROPERTIES:\nfoo\n:END:");           // drawer "properties"
add("props-allornothing", ":PROPERTIES:\nfoo\n:k: v\n:END:");    // drawer (bad first line)
add("props-allornothing", ":PROPERTIES:\n:k: v\n\n:END:");       // drawer (blank body line)
add("props-allornothing", ":PROPERTIES:\nfoo\n:k: [[Page]]\n:END:"); // drawer — NO phantom page ref
add("props-allornothing", ":PROPERTIES:\n:k: v\n:END:\n#+FOO: bar"); // properties (valid + directive fold)
// F4: an empty `* ` headline marker's trailing-ws paragraph is DROPPED before a following block.
add("empty-marker-drop", "* \n#+BEGIN_SRC\ncode\n#+END_SRC");    // bullet, src
add("empty-marker-drop", "* \n-----");                           // bullet, hr
add("empty-marker-drop", "* \n:LOGBOOK:\nx\n:END:");             // bullet, drawer
add("empty-marker-drop", "* \n| a | b |");                       // bullet, table
add("empty-marker-drop", "* \nplain");                           // bullet, paragraph (real content KEPT)

// viewframe: transformed `#+BEGIN` bodies (indented / `\r\n` / nested) parsed via the
// zero-copy strip-view frame (no `block_code` copy, no `streaming_reparse`). The fuzz tokens
// never generate leading indent or `\r\n` inside a block body, so these lock the de-indent /
// eol-normalization the view path must reproduce byte-for-byte (leaf content built from the
// VIEWED, `\n`-joined lines, not a raw `input` slice). See lsdoc-viewframe-P1.
add("viewframe", "#+BEGIN_QUOTE\n  aaa\n  bbb\n#+END_QUOTE\n");                       // indented multi-line para
add("viewframe", "#+BEGIN_QUOTE\n  [:div hello]\n#+END_QUOTE\n");                    // indented hiccup (lw-offset de-indent)
add("viewframe", "#+BEGIN_QUOTE\n  x\n  #+BEGIN_QUOTE\n    y\n  #+END_QUOTE\n#+END_QUOTE\n"); // nested indent
add("viewframe", "#+BEGIN_QUOTE\n  \\begin{eq}\n  a\n  \\end{eq}\n#+END_QUOTE\n");    // indented latex-env content
add("viewframe", "#+BEGIN_QUOTE\n  text here\n  \\begin{eq}\n  a\n  \\end{eq}\n#+END_QUOTE\n"); // para then latex
add("viewframe", "#+BEGIN_QUOTE\n  \\begin{eq}\n  a\n  \\end{eq}\n  after\n#+END_QUOTE\n"); // latex then para
add("viewframe", "#+BEGIN_QUOTE\n  | a | b |\n#+END_QUOTE\n");                        // indented table
add("viewframe", "#+BEGIN_QUOTE\n  #+BEGIN_SRC\n  code\n  #+END_SRC\n#+END_QUOTE\n"); // indented src
add("viewframe", "#+BEGIN_QUOTE\r\naaa\r\n#+END_QUOTE\r\n");                          // crlf, no indent
add("viewframe", "#+BEGIN_QUOTE\r\n  aaa\r\n  bbb\r\n#+END_QUOTE\r\n");                // crlf + indent, multi-line
add("viewframe", "#+BEGIN_QUOTE\n  - a\n  - b\n#+END_QUOTE\n");                       // indented list
add("viewframe", "#+BEGIN_QUOTE\n  :PROPERTIES:\n  :k: v\n  :END:\n#+END_QUOTE\n");   // indented drawer/props
add("viewframe", "#+BEGIN_QUOTE\n x\n  #+BEGIN_QUOTE\n  y\n  #+END_QUOTE\n#+END_QUOTE\n"); // under-indent then nest
add("viewframe", "#+BEGIN_QUOTE\na\n #+BEGIN_QUOTE\n b\n  #+BEGIN_QUOTE\n  c\n  #+END_QUOTE\n #+END_QUOTE\n#+END_QUOTE\n"); // increasing-indent depth (cumulative strip)
add("viewframe", "#+BEGIN_QUOTE\n\ta\n\tb\n#+END_QUOTE\n");                           // tab indent
add("viewframe", "#+BEGIN_QUOTE\n  *bold* x\n  /italic/ y\n#+END_QUOTE\n");           // inline markup de-indented
add("viewframe", "#+BEGIN_QUOTE\n  a\n  b\n\n  c\n#+END_QUOTE\n");                     // blank line inside para run
add("viewframe", "#+BEGIN_QUOTE\n  p1\n  #+BEGIN_QUOTE\n    p2\n    #+BEGIN_QUOTE\n      p3\n      q3\n    #+END_QUOTE\n  #+END_QUOTE\n#+END_QUOTE\n"); // 3-deep, para at each level
add("viewframe", "#+BEGIN_FOO\n  line1\n  line2\n#+END_FOO\n");                       // indented custom
add("viewframe", "#+BEGIN_QUOTE\n  a\n\n#+END_QUOTE\n");                              // trailing blank
add("viewframe", "#+BEGIN_QUOTE\n  \\begin{eq}\n  a\n\n  b\n  \\end{eq}\n#+END_QUOTE\n"); // latex content with blank line
add("viewframe", "#+BEGIN_QUOTE\n  \\begin{a}\n  x\n  \\end{a}\n  \\begin{b}\n  y\n  \\end{b}\n#+END_QUOTE\n"); // two latex envs
add("viewframe", "#+BEGIN_QUOTE\n  [:div x]\n  para\n#+END_QUOTE\n");                 // hiccup then para
add("viewframe", "#+BEGIN_QUOTE\n  aaa\nbbb\n#+END_QUOTE\n");                         // under-indented continuation (trim_start)
add("viewframe", "#+BEGIN_QUOTE\n  [fn:1] note\n  cont\n#+END_QUOTE\n");              // indented footnote fold
add("viewframe", "#+BEGIN_FOO\n  [:span x]\n  para\n#+END_FOO\n");                    // custom + hiccup + para
add("viewframe", "#+BEGIN_QUOTE\n  : verb\n#+END_QUOTE\n");                           // indented verbatim `:` line
add("viewframe", "#+BEGIN_QUOTE\r\n  x\r\n  #+BEGIN_QUOTE\r\n  y\r\n  #+END_QUOTE\r\n#+END_QUOTE\r\n"); // nested crlf

const out = cases.map((c, i) => ({ id: `o${String(i).padStart(3, "0")}`, cat: c.cat, input: c.input, format: c.format }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.org.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} org corpus inputs`);

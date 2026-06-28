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
add("table", "| a | b |\n|---+---|\n| 1 | 2 |");

// footnotes
add("fn", "see [fn:1] ref");
add("fn", "[fn:1] the definition");

// plain / boundary
add("misc", "a plain paragraph\nsecond line");
add("misc", "* parent\n** child with /em/ and [[link]]");
add("misc", "");
add("misc", "   ");

const out = cases.map((c, i) => ({ id: `o${String(i).padStart(3, "0")}`, cat: c.cat, input: c.input, format: c.format }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.org.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} org corpus inputs`);

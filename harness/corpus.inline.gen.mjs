// Inline-only corpus for the `lsdoc::inline` entrypoint gate (the analogue of mldoc's
// `inline->edn` / OG `inline-text`). Diffed against mldoc `parseInlineJson`, NOT the block
// parser — so these inputs exercise inline parsing with NO block-opener/table/list detection.
// Crucially includes lines whose leading token opens a BLOCK in the block grammar (`>`/`|`/
// `---`/`#`/`[^1]:`/`$$`/`*`/`N.`/`:drawer:`): in inline mode those are literal text + the
// rest is a full inline run. Output: corpus.inline.json = [{ id: il###, input, format }].
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const cases = [];
const add = (format, input) => cases.push({ format, input });

// --- normal inline (md): must equal what the block path puts on a Paragraph/Bullet ---
add("md", "x **b** and *i*");
add("md", "a `code` b");
add("md", "[[Page]] and #tag");
add("md", "((11111111-1111-1111-1111-111111111111))");
add("md", "text [[Foo]] more #bar ((22222222-2222-2222-2222-222222222222))");
add("md", "![alt](img.png) and [label](http://x.com)");
add("md", "***bold italic*** ~~strike~~ ==hl==");
add("md", "{{embed [[Foo]]}} {{query (and [[a]] [[b]])}}");
add("md", "$x^2$ inline math and $$y$$");
add("md", "a #[[bracket tag]] z");
add("md", "[[café]] θ #naïve 😀");
add("md", "nested **a *b* c** end");

// --- §2 fix: lines whose leading token is a BLOCK opener → literal + full inline run ---
add("md", "> quote with **bold** and [[Link]]");
add("md", "a | b | c");                 // pipe is NOT a table inline
add("md", "--- **after hr**");
add("md", "[^1]: a footnote **body** [[L]]");
add("md", "$$E=mc^2$$ **trailing**");
add("md", "# **not a heading** [[L]]");
add("md", "* list-ish **b**");
add("md", "1. ordered-ish **b**");
add("md", ":LOGBOOK: **b**");           // drawer-ish
add("md", "#+BEGIN_X **b**");           // directive/block-ish

// --- multi-line tolerance (Tine feeds single lines, but inline must match mldoc on \n) ---
add("md", "a\nb");
add("md", "a **x**\n> q");

// --- org inline ---
add("org", "/italic/ =verbatim= ~code~ _under_ *bold*");
add("org", "[[target][alias]] and [[Page]]");
add("org", "a #tag and #[[bracket]]");
add("org", "<<radio target>> text");
add("org", "{{{macro(arg)}}} and call");
add("org", "\\Delta and \\alpha entities");
add("org", "[2024-01-02 Tue] timestamp");
add("org", "> leading gt /italic/");    // org: leading > literal
add("org", "a | b | c");                // org: pipe literal inline
add("org", "x /i/\nnext line");         // org multi-line
add("org", "src_python{print(1)} inline");

// --- hiccup inline (v0.1.4) on both formats ---
add("md", "before [:div.cls \"hi\"] after");
add("org", "before [:span] after");

// emit
const out = cases.map((c, i) => ({ id: `il${String(i).padStart(3, "0")}`, ...c }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.inline.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} inline corpus inputs`);

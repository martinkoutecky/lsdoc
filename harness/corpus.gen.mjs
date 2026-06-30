// Generate an adversarial markdown corpus for the Tine parser-divergence spike.
// Output: corpus.json = [{ id, cat, input }]. Single-line inputs unless a category
// is explicitly about multi-line (fences) — parseInline is an *inline* parser, so
// multi-line fence cases are flagged as architectural in the report, not bugs.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const U1 = "11111111-1111-1111-1111-111111111111"; // real-shaped uuids
const U2 = "22222222-2222-2222-2222-222222222222";
const U3 = "33333333-3333-3333-3333-333333333333";

const cases = [];
const add = (cat, input) => cases.push({ cat, input });

// --- unbalanced / nested brackets ---
add("brackets", "[[a[[b]]c]]");
add("brackets", "[[");
add("brackets", "]]");
add("brackets", "[[ ]]");
add("brackets", "[[]]");
add("brackets", "[[a]");
add("brackets", "a]]b");
add("brackets", "[[a]]b]]");
add("brackets", "[[[[a]]]]");
add("brackets", "[[Foo](bar)]]");      // first ] immediately followed by ( -> TS link hijack
add("brackets", "[[Foo]](bar)");       // ]] then (bar)
add("brackets", "[[a](b)]] and [[c]]");
add("brackets", "x[[y]z](w)]]");
add("brackets", "[[a\nb]]");           // newline inside [[ ]]
add("brackets", "[[a]]]");
add("brackets", "[[[a]]]");
add("brackets", "pre [[Foo]] mid [[Bar]] post");

// --- triple / quad parens & block refs ---
add("parens", "((((x))))");
add("parens", "(((");
add("parens", `(( ${U1} ))`);          // spaces inside block ref
add("parens", `((${U1}))`);
add("parens", `((((${U1}))))`);
add("parens", `[label]:((${U1}))`);
add("parens", `[L](((${U1}))) tail`);  // labeled block ref
add("parens", `[L](( ${U1} ))`);
add("parens", `((not-a-uuid))`);
add("parens", `((Related Work))`);
add("parens", `(see (${U1}))`);
add("parens", `((${U1}`);
add("parens", `${U1}))`);
add("parens", `text ((${U1})) more`);
add("parens", `((${U1})) ((${U2}))`);
add("parens", `((${U1})) and ((${U1}))`); // dup
add("parens", `nested ((a((${U1}))b))`);

// --- refs inside inline / fenced code ---
add("code", "`[[Foo]]`");
add("code", "`#tag`");
add("code", `\`((${U1}))\``);
add("code", "``[[Foo]]``");            // double-backtick: TS treats as empty code + ref
add("code", "``a [[Foo]] b``");
add("code", "```[[Foo]]```");          // triple inline backtick
add("code", "`a` [[Foo]] `b`");
add("code", "`unterminated [[Foo]]");
add("code", "pre `code [[X]]` [[Foo]]");
add("code", "text with `nested ` [[Foo]] ` codes`");
add("code", "```\n[[Foo]]\n```");      // fenced (multi-line) -- architectural
add("code", `\`\`\`\n((${U1}))\n\`\`\``);
add("code", "before\n```\n[[In]]\n```\nafter [[Out]]");
add("code", "- ```calc\n  1+2\n  ```\n- [[After]]"); // bulleted fence
add("code", "```\ntext [[Foo]]");      // UNCLOSED fence: Rust=code(none), TS parses ref
add("code", "```js\n[[A]] and #B");    // unclosed fence w/ lang
add("code", "```\n[[A]]\n```\n[[B]]\n```\n[[C]]"); // odd number of fences
add("code", "`one` two ``[[Foo]]`` end"); // double-backtick mid-line

// --- refs inside links / images ---
add("link", "[((x))](y)");
add("link", `[${"alt"}]((${U1}))`);
add("link", `![a]((${U1}))`);
add("link", `![[[Foo]]](u)`);
add("link", "[[[Foo]]](u)");
add("link", `[text](https://ex.com/a)`);
add("link", `[text](/Foo_(bar))`);
add("link", `[#tag](u)`);
add("link", `[x](y) [[Foo]]`);
add("link", `[a](b)(c)`);
add("link", `[a]()`);                  // empty url
add("link", `[]()`);
add("link", `![alt](img.png){:width 200}`);
add("link", `[label](((${U1})) extra)`); // block-ref-ish but trailing text

// --- link-label emphasis reparse (M1: md labels re-parsed with Ctx::emph) ---
add("label", "[**b**](u)");                 // bold label
add("label", "[*i* and `c`](u)");           // italic + code span in a label
add("label", "[a [[P]] **b**](u)");         // page-ref + bold inside a label
add("label", "[~~s~~ ^sup^ =hl=](u)");      // strike + superscript + highlight
add("label", "![**alt** x](pic.png)");      // emphasis in an IMAGE alt label
add("label", "[a * b](u)");                 // marker present but NOT emphasis → kept PLAIN

// --- escapes ---
add("escape", "\\[[a]]");
add("escape", `\\((${U1}))`);
add("escape", "\\#tag");
add("escape", "a \\[[b]] c");
add("escape", "\\\\[[a]]");
add("escape", "\\`[[a]]\\`");

// --- mixed emphasis ---
add("emph", "**a*b**c*");
add("emph", "*_a_*");
add("emph", "***a***");
add("emph", "**[[Foo]]**");
add("emph", "*[[Foo]]*");
add("emph", "_#tag_");
add("emph", "~~[[Foo]]~~");
add("emph", "==#tag==");
add("emph", "a*b*c [[Foo]] d**e");
add("emph", "**((nope))**");
add("emph", "__[[Foo]]__");

// --- properties edge ---
add("prop", "a::b mid line");
add("prop", "::");
add("prop", "k:: v :: w");
add("prop", "tags:: [[Foo]], Bar");
add("prop", "alias:: a, b");
add("prop", "key:: ((not))");
add("prop", "x:: #tag");

// --- tags ---
add("tag", "#t.");
add("tag", "#[[a b]]");
add("tag", "#t#t");
add("tag", "#中文");
add("tag", "#café");
add("tag", "#123");
add("tag", "#t,");
add("tag", "#tag.foo");
add("tag", "#a/b/c");
add("tag", "#a.b.c");
add("tag", "word#tag");
add("tag", "a#b");
add("tag", "c#sharp");
add("tag", "#tag-name_2");
add("tag", "#tag!");
add("tag", "(#tag)");
add("tag", "]#tag");
add("tag", "#");
add("tag", "# notatag");
add("tag", "#[[a]b]]");
add("tag", "#[[unclosed");
add("tag", "##double");
add("tag", "#tag's");
add("tag", "email#fragment");
add("tag", "#_underscore");
add("tag", "#-dash");
add("tag", "x#中文");
add("tag", "#naïve");
add("tag", "pre #tag.next");

// --- urls with parens / trailing punct ---
add("url", "(see http://x.com/a(b))");
add("url", "http://x.com.");
add("url", "https://en.wikipedia.org/wiki/Foo_(bar)");
add("url", "see https://ex.com/p#Old then #real");
add("url", "<https://ex.com/path>");
add("url", "https://x.com/#frag");
add("url", "visit (https://a.com) and #tag");
add("url", "https://x.com/a)b");
add("url", "url: https://x.com, next");

// --- macros ---
add("macro", `{{embed ((${U1}))}}`);
add("macro", "{{{n}}}");
add("macro", "{{m {{n}}}}");
add("macro", "{{query [[Foo]]}}");
add("macro", "{{embed [[Foo]]}}");
add("macro", `{{embed [[Foo]] ((${U1}))}}`);
add("macro", "{{");
add("macro", "{{}}");
add("macro", "{{renderer :x, [[Foo]]}}");
add("macro", "a {{b}} #tag");
add("macro", "{{video https://x.com/(v)}}");

// --- unicode / zero-width / combining ---
add("unicode", "[[café]]");
add("unicode", "[[naïve]] θ #tag");
add("unicode", "[[a​b]]");       // zero-width space inside
add("unicode", "#tag​suffix");   // zwsp after tag
add("unicode", "[[é]]");        // combining acute
add("unicode", "#étag");
add("unicode", "[[＃fullwidth]]");
add("unicode", "[[😀emoji]]");
add("unicode", "#😀");
add("unicode", "[[a\tb]]");           // tab inside

// --- misc / very long / deep ---
add("misc", "[[" + "a".repeat(500) + "]]");
add("misc", "#" + "a".repeat(500));
add("misc", "[[a]] ".repeat(50).trim());
add("misc", "((" + U1 + ")) ".repeat(10).trim());
add("misc", "");
add("misc", "   ");
add("misc", "plain text no refs at all");
add("misc", "[[Foo]] #bar ((" + U1 + ")) {{q}} `c` **b**");

// --- render-level link fields: image-ness, title, metadata (§ render parity) ---
add("render", "![alt](img.png)");                 // image=true
add("render", "[alt](img.png)");                  // image=false (same url)
add("render", "![](x.png)");                      // empty-label image
add("render", "![a](../assets/x.png)");           // relative asset image
add("render", "[label](http://x.com \"a title\")"); // title
add("render", "![alt](img.png \"cap\")");          // image + title
add("render", "[l](u \"a \\\"b\\\" c\")");          // title with escaped quotes (raw)
add("render", "[l](u \"\")");                      // empty "" → no title, url keeps it
add("render", "[t](u 'single')");                 // single-quotes are NOT a title
add("render", "![a](../assets/x.png){:height 40, :width 100}"); // image + metadata
add("render", "[a](u){:width 50}");               // metadata on a non-image link
add("render", "![a](x.png \"cap\"){:width 10}");   // image + title + metadata
add("render", "text ![i](a.png) and [l](b) end"); // mixed image/link in a line

// --- Clojure-hiccup `[:tag …]` (C7) — inline & boundary cases (md) ---
// allowlist membership (the #1 correctness risk): in vs out.
add("hiccup", "x [:div] y");                  // inline Hiccup between plain
add("hiccup", "x [:span] y");
add("hiccup", "a [:a] b");                    // single-letter allowed tag
add("hiccup", "a [:h1] b");                   // digit in name (allowed)
add("hiccup", "a [:foo] b");                  // NOT a tag → plain
add("hiccup", "a [:aa] b");                   // NOT a tag → plain
add("hiccup", "a [:svg] b");                  // NOT a tag → plain
add("hiccup", "a [:h7] b");                   // NOT a tag → plain
add("hiccup", "a [:div2] b");                 // alnum continues name → not a tag
add("hiccup", "a [:DIV] b");                  // case-insensitive allowlist → Hiccup
add("hiccup", "a [:Section] b");              // mixed case → Hiccup
// gate boundary: char after the name.
add("hiccup", "x [:div.cls] y");              // `.` selector
add("hiccup", "x [:div#id] y");               // `#` selector
add("hiccup", "x [:div{:a 1}] y");            // `{` immediately → NOT hiccup
add("hiccup", "x [:div-x] y");                // `-` in name → NOT hiccup
// balanced capture: strings, nesting, braces, escapes.
add("hiccup", 'x [:div "a]b"] y');            // `]` inside a string is protected
add("hiccup", 'x [:div "a\\"]b"] y');         // escaped quote inside string
add("hiccup", "x [:div [:span]] y");          // nested `[:` increments depth
add("hiccup", "x [:div [x]] y");              // lone `[` does NOT nest → stops at first `]`
add("hiccup", "x [:div {:a]}] y");            // `{}` not balanced; first `]` closes
add("hiccup", "x [:div.cls {:a 1} \"hi\" [:span \"y\"]] y"); // full hiccup
// unclosed / fallback.
add("hiccup", "x [:div y");                   // no `]` → plain
add("hiccup", "x [:div [:span] y");           // outer unclosed, inner inline Hiccup
add("hiccup", 'x [:div "abc] y');             // unterminated string → plain
add("hiccup", "x[:div]");                     // hiccup not at BOL → inline
add("hiccup", "[:div]x and [:span]");         // BOL handled by block gen; here mid-text

// emit
const out = cases.map((c, idx) => ({ id: `c${String(idx).padStart(3, "0")}`, cat: c.cat, input: c.input }));
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} corpus inputs`);

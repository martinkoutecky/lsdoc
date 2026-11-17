// Property-value corpus for propgate.mjs: `lsdoc::property_references` vs mldoc
// `getReferences` (`Property.property_references`). Every value is read in both formats.
// Shapes: block openers and other first-document-node forms, links of every kind,
// emphasis/sub/superscript/label nesting, nested links, tags, macros (including markup and
// links crossing a macro's boundary), escapes. Writes corpus.property.json.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const values = [
  "# x", "## x", "* x", "- x", "+ x", "1. x", "1) x", "> x", "| x |", "---", "-----", "***",
  "___", "* * *", "```x", "~~~", "$$x$$", "#+TITLE: x", "#+RESULTS: x", "#+BEGIN_SRC",
  "#+BEGIN_QUOTE x", ":PROPERTIES:", ":END:", ": x", ":x:", "<!-- x -->", "<div>x</div>",
  "[:div \"x\"]", "[fn:x] y", "[^x]: y", "CLOCK: x", "SCHEDULED: <2020-01-01 Wed>", "#", "-",
  "*", ">", "|", "1.", "# [[a]]", "- [[a]], b", "+ [[a]], b", "> #x, b", "| [[a]] |",
  "1. a, b", "* a, b", "#+TITLE: [[a]]", "$$[[a]]$$", "<div>[[a]]</div>", "# x, [[a]]",
  "\\#x", "\\# x", "\\- x", "\\> x", "[[a", "a]] b", "a\\[b", "a, # x", "x # y", "#x, # y",
  "[[a]], b", "a ## b", "x, \"y\"", "\"x\"", "[[a]]", "#x", "#[[a b]]", "[[a [[b]]]]",
  "[[a]][[b]]", "[x](A.md)", "[x](A.org)", "[x](../pages/A.md)", "[x](https://a.example)",
  "[[file:A.org][x]]", "[[A][x]]", "[x]([[a]])", "[[https://a.example]]",
  "<https://a.example>", "https://a.example", "![x](../assets/a.png)", "[[../assets/a.png]]",
  "[[draws/a.excalidraw]]", "((6539a1b2-0000-4000-8000-000000000000))", "{{embed [[a]]}}",
  "**[[a]]**", "`[[a]]`", "[a [[b]]](A.md)", "[[a]], [x](A.md), c", "*[[a]]*", "_[[a]]_",
  "~~[[a]]~~", "^^[[a]]^^", "==[[a]]==", "**#a**", "*#[[a b]]*", "x **[[a]]**, [[b]]",
  "**[[a]]**, b", "/[[a]]/", "+[[a]]+", "a_{[[b]]}", "a^{[[b]]}", "H_2O, [[b]]", "a_[[b]]",
  "[[[a]]]([[b]])", "[x [[a]]]([[b]])", "[**[[a]]**](https://a.example)", "[[b][[[a]]]]",
  "[[https://a.example][[[a]]]]", "[#a](https://a.example)", "[[a [[b [[c]]]]]]",
  "#[[a [[b]]]]", "[[a]] [[b [[c]]]]", "[[a [[b]]]], c", "{{embed [[a]] [[b]]}}",
  "{{embed ((6578a1b0-0000-4000-8000-000000000000))}}", "{{renderer [[macro]]}}",
  "{{renderer :x, [[a]]}}", "{{query [[a]]}}", "{{cloze [[a]]}}", "{{x #a}}",
  "{{x **[[a]]**}}", "{{embed [[a [[b]]]]}}", "x, {{renderer [[a]]}}, y", "{{a {{b [[c]]}}}}",
  "{{embed [[a]]}}, b", "{{x}}", "{{x [[a}} b]]", "#a", "#a, #b", "#a]]", "#[[a]]b",
  "[[a]], #b, c", "#[[a]]", "{{x **[[a]]}}**", "**{{x [[a]]}}**", "{{x **[[a]]}} [[b]]**",
  "{{x [[a]]}} **[[b]]**", "*{{x [[a]]*}}", "_{{x [[a]]}}_", "/{{x [[a]]}}/",
  "**a{{x}}** [[b]]", "{{x [[a]]}}}", "{{{x [[a]]}}}", "{{embed [[a]]}}**",
  "**{{embed [[a]]}}**", "[[a {{x}} b]]", "{{x #[[a}} b]]", "#a{{x}}", "{{x [[a]], b}}, c",
  "{{x [[a]]}} [[b}}", "{{query (and [[a]] [[b]])}}", "{{x [[a]]}}, {{y [[b}} c]]",
  "~~{{x [[a]]}}~~ [[b]]", "{{x **[[a]]}}** [[b]]", "{{x ==[[a]]}}==", "{{x ^^[[a]]}}^^",
  "a, **{{x [[b]]}}**, c", "#a\\_b", "[[a\\_b]]", "#[[a\\_b]]",
  "valuey macro (page): {{query [[Some Page]]}}", "[[a]] {{x [[b]]}} c [[d}}",
  "{{{{x [[a]]}}}}", "\\{{x [[a]]}}", "{{x [[a]] #b}} #c", "{{x ((6539a1b2-0000-4000-8000-000000000000))}}",
];

const corpus = [];
for (const format of ["md", "org"]) {
  values.forEach((input, i) => {
    corpus.push({ id: `pr-${format}-${String(i).padStart(3, "0")}`, format, input });
  });
}
const __dir = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(__dir, "corpus.property.json"), JSON.stringify(corpus, null, 1) + "\n");
console.log(`corpus.property: ${corpus.length} values (${values.length} × md, org)`);

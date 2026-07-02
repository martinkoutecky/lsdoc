// §1a delta-enumeration probe (RENDER-PARITY-AND-INTEGRATION.md step 1).
//
// For every mldoc node/inline TAG, collect the union of payload-object keys mldoc
// actually emits across a comprehensive corpus (corpus.all.json + a render-focused
// extra set), with a sample value per key, and diff that against the keys the
// observable projection KEEPS (normalize.mjs). Anything in `mldoc` but not in
// `kept` is a candidate render delta to triage.
//
// Usage: node delta.mjs            (corpus.all.json + built-in render extras)
import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));

const cfg = (format) => JSON.stringify({
  toc: false, parse_outline_only: false, heading_number: false,
  keep_line_break: true, format: format === "org" ? "Org" : "Markdown",
  heading_to_list: false, export_md_remove_options: [],
});

// What normalize.mjs keeps, per mldoc tag. (Hand-mirrored from normalize.mjs so the
// diff is meaningful — if you change normalize.mjs, update this too.)
const KEPT = {
  // inline
  Plain: ["*scalar*"], Code: ["*scalar*"], Verbatim: ["*scalar*"],
  Break_Line: [], Hard_Break_Line: [],
  Emphasis: ["*array*"],                                  // [[kind],[children]]
  Link: ["url", "label", "full_text"],
  Subscript: ["*array*"], Superscript: ["*array*"],
  Nested_link: ["content"],
  Tag: ["*array*"],
  Macro: ["name", "arguments"],
  Latex_Fragment: ["*array*"],                            // [mode, body]
  Timestamp: ["*array*"],                                 // [ts, date]
  Cookie: ["*array*"],                                    // [Absolute,n,m] | [Percent,n]
  Footnote_Reference: ["name"],
  Target: ["*scalar*"],
  Radio_Target: ["*scalar*"],
  Email: ["*whole*"],                                     // carried opaque
  Inline_Hiccup: [], Inline_Html: ["*scalar*"],
  Entity: ["name", "latex", "latex_mathp", "html", "ascii", "unicode"],
  // block
  Paragraph: ["*array*"],
  Heading: ["level", "size", "title", "tags", "marker", "priority", "unordered"],
  List: ["*items*"],                                       // ordered,number,indent,content,items,name
  Src: ["language", "lines"],
  Quote: ["*array*"],
  Custom: ["*name+children*"],
  Property_Drawer: ["*pairs*"],
  Horizontal_Rule: [],
  Table: ["header", "groups"],
  Footnote_Definition: ["*name+inlines*"],
  Raw_Html: ["*scalar*"], Displayed_Math: ["*scalar*"],
  Latex_Environment: ["*name+content*"],
  Directive: ["*key+value*"],
  Example: ["*lines*"],
  Drawer: ["*name-only*"],
};
// List-item object keys the projection keeps (normItem).
const KEPT_ITEM = ["ordered", "number", "indent", "content", "items", "name"];

// Collect, per tag, the union of object-payload keys + a sample value.
const seen = {};            // tag -> { key -> sampleValue }
const itemKeys = {};        // "·item·" -> { key -> sampleValue }
const sample = (store, tag, key, val) => {
  store[tag] ??= {};
  if (!(key in store[tag])) store[tag][key] = val;
};

// Heuristic: a node array is "tagged" if its first element is a CapitalizedString.
function isTagged(a) {
  return Array.isArray(a) && typeof a[0] === "string" && /^[A-Z]/.test(a[0]);
}
// Is this object a mldoc list-item? (has the item shape, not a tagged node)
function looksLikeItem(o) {
  return o && typeof o === "object" && !Array.isArray(o) &&
    ("content" in o) && ("items" in o);
}

function walk(node) {
  if (Array.isArray(node)) {
    if (isTagged(node)) {
      const tag = node[0];
      for (let i = 1; i < node.length; i++) {
        const el = node[i];
        if (el && typeof el === "object" && !Array.isArray(el)) {
          for (const [k, v] of Object.entries(el)) sample(seen, tag, k, v);
        }
      }
    }
    for (const el of node) walk(el);
  } else if (node && typeof node === "object") {
    if (looksLikeItem(node)) {
      for (const [k, v] of Object.entries(node)) sample(itemKeys, "·item·", k, v);
    }
    for (const v of Object.values(node)) walk(v);
  }
}

// --- corpus -----------------------------------------------------------------
const corpus = [];
const allPath = join(__dir, "corpus.all.json");
if (existsSync(allPath)) corpus.push(...JSON.parse(readFileSync(allPath, "utf8")));

// Render-focused extras: exercise the fields the audit flagged + likely neighbours.
const extra = [
  // images vs links, titles, metadata
  ["![alt](img.png)", "md"],
  ["[label](img.png)", "md"],
  ['![alt](img.png "a title")', "md"],
  ['[label](http://x.com "a title")', "md"],
  ["![alt](../assets/x.png){:height 40, :width 100}", "md"],
  ["[[Page]] inline", "md"],
  ["[label](((11111111-1111-1111-1111-111111111111)))", "md"],
  ["![img.png](../assets/photo.jpg)", "md"],
  // task checkboxes (markdown + org)
  ["- [ ] todo task", "md"],
  ["- [x] done task", "md"],
  ["- [ ] a\n- [x] b", "md"],
  ["* TODO heading task", "org"],
  ["* DONE finished", "org"],
  ["- [ ] org checkbox", "org"],
  // tables with alignment
  ["| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |", "md"],
  ["| a | b |\n|---|---|\n| 1 | 2 |", "md"],
  // scheduled / deadline / logbook on a heading
  ["* TODO task\nSCHEDULED: <2026-06-28 Sun>", "org"],
  ["* TODO task\nDEADLINE: <2026-06-28 Sun>", "org"],
  ["TODO write\nSCHEDULED: <2026-06-28 Sun>", "md"],
  // src with options/results
  ["#+BEGIN_SRC python :results output\nprint(1)\n#+END_SRC", "org"],
  ["```python {.line-numbers}\nx=1\n```", "md"],
  // headings with size / setext
  ["# H1 {#custom-id}", "md"],
  ["Title\n=====", "md"],
  // org priority + tags
  ["* TODO [#A] important :work:urgent:", "org"],
  // footnote
  ["text[^1]\n\n[^1]: def body", "md"],
  // timestamps
  ["<2026-06-28 Sun>", "org"],
  ["<2026-06-28 Sun 10:00-11:00>", "org"],
  ["SCHEDULED: <2026-06-28 Sun .+1d>", "org"],
  // org targets / radio
  ["<<my target>>", "org"],
  // org-mode emphasis variants
  ["*bold* /italic/ _under_ +strike+ =verb= ~code~", "org"],
  // hiccup
  ["@@html:<b>x</b>@@", "org"],
];
for (const [input, format] of extra) corpus.push({ id: "x", input, format });

let errs = 0;
for (const c of corpus) {
  try {
    const ast = JSON.parse(Mldoc.parseJson(c.input, cfg(c.format)));
    walk(ast);
  } catch { errs++; }
}

// --- report -----------------------------------------------------------------
const trunc = (v) => {
  let s; try { s = JSON.stringify(v); } catch { s = String(v); }
  return s.length > 80 ? s.slice(0, 77) + "…" : s;
};

console.log(`# mldoc payload-key delta  (${corpus.length} inputs, ${errs} parse errors)\n`);
const tags = Object.keys(seen).sort();
for (const tag of tags) {
  const kept = KEPT[tag];
  const keys = Object.keys(seen[tag]).sort();
  const known = kept && kept[0]?.startsWith("*");      // structural keep (whole/array/...)
  console.log(`## ${tag}${kept ? "" : "   ⚠ TAG NOT IN normalize.mjs"}`);
  for (const k of keys) {
    let mark = "  DROPPED";
    if (!kept) mark = "  (tag unhandled)";
    else if (known) mark = "  (kept structurally)";
    else if (kept.includes(k)) mark = "  kept";
    console.log(`   ${mark.padEnd(22)} ${k.padEnd(16)} e.g. ${trunc(seen[tag][k])}`);
  }
  console.log();
}

console.log(`## ·list-item· object`);
for (const k of Object.keys(itemKeys["·item·"] ?? {}).sort()) {
  const mark = KEPT_ITEM.includes(k) ? "  kept" : "  DROPPED";
  console.log(`   ${mark.padEnd(22)} ${k.padEnd(16)} e.g. ${trunc(itemKeys["·item·"][k])}`);
}

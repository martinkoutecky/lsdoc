// Mined Org test INPUTS for the differential corpus (SPEC §5 "mined mldoc/OG
// test inputs"), Org milestone M6. Self-contained: every string is embedded as
// data (no clone needed at runtime), like corpus.mined.gen.mjs.
//
// Provenance: the INPUT (first OCaml string literal after each `check_aux`/
// `check_aux2`) of every test in logseq/mldoc `test/test_org.ml` (default branch
// HEAD bedae99), full OCaml escape + line-continuation decoding. Org-format only.
// Expected ASTs are NOT inputs (and test_org.ml uses keep_line_break:false, a
// different config; we only reuse the input strings, re-run through our oracle).
// The last 2 entries are the `.org` whole-file fixtures from OG's
// graph_parser_test.cljs (`parse-file conn "<f>.org" "<content>" {}`).
//
// Output: corpus.org.mined.json = [{ id:"om###", input, format:"org" }] — deduped
// against the other Org corpora. Committed (NOT gitignored).
import { writeFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const INPUTS = [
  "*a b c*",
  "a*b*c",
  "/a b c/",
  "_a b c_",
  "a * b*",
  "a_b_c",
  "_a _ a_",
  "*a * a*",
  "hello_world_",
  "hello,_world_",
  "[[http://example.com][[example] website]]",
  "[[http://example.com][[[example]] website]]",
  "[[http://example.com]]",
  "[[example]]",
  "[[exam:ple]]",
  "[[exam:ple][label]]",
  "[fn:abc] 中文",
  "#+BEGIN_QUOTE\nfoo\nbar\n#+END_QUOTE",
  "#+BEGIN_QUOTE\n aaa\nbbb\n#+END_QUOTE",
  "#+BEGIN_EXAMPLE\nfoo\nbar\n#+END_EXAMPLE",
  ":PROPERTIES:\n:XXX: 1\n:yyy: 2\n:END:\n#+ZZZ: 3\n#+UUU: 4",
  "#+BEGIN_QUOTE\na:: b\n#+END_QUOTE",
  "#+BEGIN_SRC haskell :results silent :exports code :var n=0\nfac 0 = 1\nfac n = n * fac (n-1)\n#+END_SRC",
  "* aaa     :bb:cc:",
  "* aaa [[link][label]]     :bb:cc:",
  ":PROPERTIES:\n:ID:       72289d9a-eb2f-427b-ad97-b605a4b8c59b\n:END:\n#+tItLe: Well parsed!",
  "* [[bar][title]]\n* [[https://example.com][example]]\n* [[../assets/conga_parrot.gif][conga]]",
];

const __dir = dirname(fileURLToPath(import.meta.url));
const existing = new Set();
for (const f of ["corpus.org.json"]) {
  try { for (const c of JSON.parse(readFileSync(join(__dir, f), "utf8"))) existing.add(c.input); }
  catch { /* not generated yet */ }
}
const seen = new Set();
const out = [];
let idx = 0;
for (const input of INPUTS) {
  if (input === "" || existing.has(input) || seen.has(input)) continue;
  seen.add(input);
  out.push({ id: `om${String(idx).padStart(3, "0")}`, input, format: "org" });
  idx++;
}
writeFileSync(join(__dir, "corpus.org.mined.json"), JSON.stringify(out, null, 1));
console.log(`wrote ${out.length} mined org corpus inputs`);

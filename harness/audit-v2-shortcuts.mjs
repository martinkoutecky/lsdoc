// Differential audit for v2 shortcuts and transformed-body post-processors.
//
// This is intentionally not random fuzz: each family enumerates boundary alphabets
// around a shortcut whose correctness depends on accepting only a locally provable
// subset of mldoc behavior. If a future optimization widens a grammar, this gate
// should fail before a real graph finds it.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";
import { canon, canonJSON } from "./lib/compare.mjs";

const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const __dir = dirname(fileURLToPath(import.meta.url));
const repo = join(__dir, "..");

function cfg(format) {
  return JSON.stringify({
    toc: false,
    parse_outline_only: false,
    heading_number: false,
    keep_line_break: true,
    format: format === "org" ? "Org" : "Markdown",
    heading_to_list: false,
    export_md_remove_options: [],
  });
}

function oracle(input, format) {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(format)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast, format) };
}

const cases = [];
let seq = 0;
function add(cat, input, format = "md") {
  cases.push({ id: `a${seq++}`, cat, input, format });
}

function addMarkdownBoldBodyCases() {
  const atoms = [
    "x",
    " ",
    "  x",
    "$a$",
    "$$a$$",
    "`c`",
    "[[P]]",
    "[l](u)",
    "[50%]",
    "[2026-01-01 Thu]",
    "[:div]",
    "#tag",
    "{{foo}}",
    "{{embed [[P]]}}",
    "((11111111-1111-1111-1111-111111111111))",
    "<a@b.com>",
    "<http://x>",
    "<span>x</span>",
    "http://x",
    "_u_",
    "^{x}",
    "\\amp",
    "\\(x\\)",
  ];
  const bodies = new Set();
  for (const a of atoms) bodies.add(a);
  for (const a of atoms) {
    for (const b of atoms) {
      bodies.add(a + b);
      bodies.add(`${a} ${b}`);
    }
  }
  for (const body of bodies) add("md-bold-body", `**${body}**`);
}

function addTopInlineCases() {
  const atoms = [
    "a",
    " ",
    ".",
    ",",
    ":",
    ";",
    "!",
    "?",
    "$x$",
    "$$x$$",
    "`c`",
    "**b**",
    "*i*",
    "_u_",
    "~~s~~",
    "==h==",
    "^{x}",
    "_{x}",
    "[[P]]",
    "[l](u)",
    "![alt](pic.png)",
    "[50%]",
    "[2026-01-01 Thu]",
    "[^note]",
    "[:div]",
    "#tag",
    "{{embed [[P]]}}",
    "{{foo}}",
    "{{namespace \tFormula1}}",
    "((11111111-1111-1111-1111-111111111111))",
    "<http://x>",
    "<a@b.com>",
    "<span>x</span>",
    "http://x",
    "src://x",
    "SCHEDULED: <2026-01-01 Thu>",
    "\\amp",
    "\\*",
    "\\(x\\)",
  ];
  const inputs = new Set();
  for (const a of atoms) inputs.add(`p ${a} q`);
  for (const a of atoms) {
    for (const b of atoms) {
      inputs.add(`p ${a}${b} q`);
      inputs.add(`p ${a} ${b} q`);
    }
  }
  for (const input of inputs) add("md-top-inline", input);
}

function addOrgTopInlineCases() {
  const atoms = [
    "a",
    " ",
    ".",
    ":",
    "$x$",
    "`c`",
    "=code=",
    "~raw~",
    "*b*",
    "/i/",
    "+s+",
    "_{x}",
    "^{x}",
    "[[P]]",
    "[[url][label]]",
    "[50%]",
    "[2026-01-01 Thu]",
    "[fn:note]",
    "[:div]",
    "#tag",
    "{{embed [[P]]}}",
    "{{namespace \tFormula1}}",
    "((11111111-1111-1111-1111-111111111111))",
    "<http://x>",
    "<a@b.com>",
    "<span>x</span>",
    "http://x",
    "src://x",
    "SCHEDULED: <2026-01-01 Thu>",
    "\\alpha",
    "\\(x\\)",
  ];
  const inputs = new Set();
  for (const a of atoms) inputs.add(`p ${a} q`);
  for (const a of atoms) {
    for (const b of atoms) {
      inputs.add(`p ${a}${b} q`);
      inputs.add(`p ${a} ${b} q`);
    }
  }
  for (const input of inputs) add("org-top-inline", input, "org");
}

function addOrgLinkLabelCases() {
  const urls = [
    "Page",
    "http://example.com/a?b=c&d=e",
    "file:notes/page.org",
    "aaaaaa:aa.aaaa#aaaaa://aa.aaaa/?aaaa_aaaa=aaaaaaa&a=99999",
  ];
  const labels = [
    "label",
    "=verbatim=",
    "~raw~",
    "_{sub}",
    "^{sup}",
    "*bold*",
    "/italic/",
    "+strike+",
    "/a _{sub}/",
    "/a _sub/",
    "*a ^{sup}*",
    "a =verbatim= b",
    "a ~raw~ b",
    "a [bracket] b",
    "a \\] escaped b",
    "[[Nested Page]]",
    "http://label.example/path",
    "[2026-01-01 Thu]",
    "#tag",
    "žluťoučký 中",
  ];
  for (const url of urls) {
    for (const label of labels) {
      add("org-link-label", `pre [[${url}][${label}]] post`, "org");
    }
  }
}

function addOrgLink2ClassificationCases() {
  const names = [
    "Page",
    "Page With Spaces",
    "file:notes/page.org",
    "http://example.com",
    "proto://host/path?x=1&y=2",
    "://missing-protocol",
    "proto://",
    "a:b",
    "a:b://x",
    "#frag://x",
    "page#frag://x",
    "aaaaaa:aa.aaaa#aaaaa://aa.aaaa/?aaaa_aaaa=aaaaaaa&a=99999",
    "plain:colon:page",
    "escaped\\]bracket",
    "žluťoučký 中",
  ];
  for (const name of names) add("org-link2-classifier", `pre [[${name}]] post`, "org");
}

function addBlockquoteBoundaryCases() {
  const nexts = [
    "---",
    "***",
    "___",
    "[//]: # c",
    "# h",
    "- b",
    "+ b",
    "1. b",
    "id:: x",
    "key:: value",
    "<div>x</div>",
    "<!-- c -->",
    "$$x$$",
    "```\nx\n```",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "[:div]",
    "plain",
  ];
  const blanks = ["", ">\n", ">\n>\n", "> \n", "> \n> \n"];
  const prefixes = ["> a", "> a\n> b"];
  for (const prefix of prefixes) {
    for (const blank of blanks) {
      for (const next of nexts) {
        const quotedNext = next
          .split("\n")
          .map((line) => `> ${line}`)
          .join("\n");
        add("md-blockquote-boundary", `${prefix}\n${blank}${quotedNext}`);
      }
    }
  }
}

function addMarkdownHardbreakBoundaryCases() {
  const bodies = [
    "x  \n9. y",
    "x   \n9. y",
    "x\n  \n9. y",
    "x\n   \n9. y",
    "x  \n- y",
    "x\n  \n- y",
    "x  \n[//]: # c",
    "x  \nkey:: value",
    "x  \n---",
    "x\n  \n---",
  ];
  const wrappers = [
    ["md-hardbreak-doc", (body) => body],
    ["md-hardbreak-quote", (body) => body.split("\n").map((line) => `> ${line}`).join("\n")],
    ["md-hardbreak-callout", (body) => `#+BEGIN_QUOTE\n${body}\n#+END_QUOTE`],
    [
      "md-hardbreak-list-callout",
      (body) => `* #+BEGIN_QUOTE\n  ${body.split("\n").join("\n  ")}\n  #+END_QUOTE`,
    ],
    [
      "md-hardbreak-callout-list",
      (body) => `#+BEGIN_QUOTE\n* ${body.split("\n").join("\n  ")}\n#+END_QUOTE`,
    ],
  ];
  for (const [cat, wrap] of wrappers) {
    for (const body of bodies) add(cat, wrap(body));
  }
}

function addLatexBoundaryCases() {
  const cases = [
    ["> q\n\\begin{eq}a\\end{eq}", "md"],
    ["> a\n> \\begin{eq}\n> x\n> \\end{eq}\n> b\n", "md"],
    ["#+BEGIN_QUOTE\ntext here\n\\begin{eq}\na\n\\end{eq}\n#+END_QUOTE\n", "md"],
    ["#+BEGIN_QUOTE\n\\begin{a}\nx\n\\end{a}\n\\begin{b}\ny\n\\end{b}\n#+END_QUOTE", "md"],
    ["- #+BEGIN_QUOTE\n  \\begin{a}\n  x\n  \\end{a}\n  \\begin{b}\n  y\n  \\end{b}\n  #+END_QUOTE", "md"],
    ["#+BEGIN_QUOTE\n  text here\n  \\begin{eq}\n  a\n  \\end{eq}\n#+END_QUOTE\n", "org"],
    ["#+BEGIN_QUOTE\n  \\begin{a}\n  x\n  \\end{a}\n  \\begin{b}\n  y\n  \\end{b}\n#+END_QUOTE\n", "org"],
  ];
  for (const [input, format] of cases) add("latex-boundary", input, format);

  // GH #209 audit4 F2: a block-starter placed SAME-LINE after `\end{name}` is a
  // following block in mldoc, not paragraph text. The cases above only cover
  // next-line composition; enumerate the same-line-tail product here.
  const heads = ["", "- "];
  const tails = [
    "$$y$$",
    "# H",
    "|a|b|\n|---|---|",
    "#+BEGIN_NOTE\nx\n#+END_NOTE",
    "---",
    "<div>x</div>",
    "\\begin{b}\nw\n\\end{b}",
    "[:span]",
    "plain",
    "", // empty tail: keep_line_break parity
  ];
  for (const head of heads)
    for (const tail of tails)
      for (const fmt of ["md", "org"])
        add("latex-same-line-tail", `${head}\\begin{a}\nz\n\\end{a} ${tail}`, fmt);
}

function addCalloutBoundaryCases() {
  const nexts = [
    "---",
    "***",
    "___",
    "[//]: # c",
    "# h",
    "- b",
    "+ b",
    "1. b",
    "id:: x",
    "key:: value",
    "<div>x</div>",
    "<!-- c -->",
    "$$x$$",
    "```\nx\n```",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "[:div]",
    "plain",
  ];
  const blanks = ["", "\n", "\n\n"];
  const prefixes = ["a", "a\nb"];
  for (const prefix of prefixes) {
    for (const blank of blanks) {
      for (const next of nexts) {
        add("md-callout-boundary", `#+BEGIN_QUOTE\n${prefix}\n${blank}${next}\n#+END_QUOTE`);
      }
    }
  }
}

function addListItemBoundaryCases() {
  const nexts = [
    "---",
    "***",
    "___",
    "[//]: # c",
    "# h",
    "- b",
    "+ b",
    "1. b",
    "id:: x",
    "key:: value",
    "<div>x</div>",
    "<!-- c -->",
    "$$x$$",
    "```\nx\n```",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "[:div]",
    "plain",
  ];
  const blanks = ["", "\n  ", "\n  \n  "];
  const prefixes = ["* a", "* a\n  b"];
  for (const prefix of prefixes) {
    for (const blank of blanks) {
      for (const next of nexts) {
        const indentedNext = next
          .split("\n")
          .map((line, j) => (j === 0 && blank === "" ? "" : "  ") + line)
          .join("\n");
        add("md-list-boundary", `${prefix}\n${blank}${indentedNext}`);
      }
    }
  }
}

function addHeadingSuffixCases() {
  const suffixes = [
    "---",
    "***",
    "___",
    "[//]: # c",
    "# h",
    "- b",
    "+ b",
    "1. b",
    "id:: x",
    "key:: value",
    "<div>x</div>",
    "<!-- c -->",
    "$$x$$",
    "```\nx\n```",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "[:div]",
    "|a|b|\n|---|---|",
    "[^1]: body",
    "plain",
  ];
  for (const prefix of ["# ", "- ", "## ", "- TODO "]) {
    for (const suffix of suffixes) add("md-heading-suffix", prefix + suffix);
  }
}

function addMarkdownBulletTitleEmphasisCases() {
  const titles = [
    "**Ctrl1. ** and",
    "**CSV import.> ** Drop",
    "**math stays TeX by design **",
    "**t ** toggles the right sidebar; **Shift-click** any",
    "**ok** then **next**",
  ];
  for (const title of titles) {
    add("md-bullet-title-emphasis", `- ${title}`);
    add("md-bullet-title-emphasis", `\t- ${title}`);
  }
}

function addTimestampBacktrackCases() {
  const fragments = [
    "x [2026-07-07 Tue 12:22:19--[2026-07-07 Tue 12:22:20] y",
    "CLOCK: [2026-07-07 Tue 12:22:19--[2026-07-07 Tue 12:22:20] =>  00:00:01",
  ];
  for (const fragment of fragments) {
    add("timestamp-backtrack-md", fragment, "md");
    add("timestamp-backtrack-org", fragment, "org");
  }
  add(
    "timestamp-backtrack-after-properties",
    "shipped:: false\n\t  stray:: this column is undeclared\n\t  :LOGBOOK:\n\t  CLOCK: [2026-07-07 Tue 12:22:19--[2026-07-07 Tue 12:22:20] =>  00:00:01",
    "md",
  );
}

function addMarkdownLinkLabelBracketCases() {
  const labels = [
    "[[](u)",
    "[[[](u)",
    "[[[[](u)",
    "[[Page]](u)",
    "[a [b] c](u)",
    "[a [[Page]] c](u)",
    "[a \\[ b](u)",
  ];
  for (const input of labels) add("md-link-label-bracket-source", input, "md");
}

function addEmptyMarkerFenceCases() {
  const fence = "`".repeat(3);
  const prefixes = ["- ", "# ", "\t- "];
  const suffixes = [
    `${fence}rust`,
    `${fence}\nx`,
    `${fence}\nx\n${fence}`,
    `${fence} rust\nx\n${fence}`,
  ];
  for (const prefix of prefixes) {
    for (const suffix of suffixes) {
      add("md-empty-marker-fence", `${prefix}\n${suffix}`, "md");
    }
  }
}

function addBoundedSuffixCases() {
  const prefixes = [
    "$$x$$",
    "[:div]",
    "<div>x</div>",
    ":PROPERTIES:\n:k: v\n:END:",
    "k:: v",
  ];
  const tails = [
    "",
    "plain",
    "---",
    "***",
    "___",
    "[//]: # c",
    "# h",
    "- b",
    "+ b",
    "1. b",
    "id:: x",
    "key:: value",
    "<span>y</span>",
    "<bad",
    "$$y$$",
    "```\ny\n```",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "[:span]",
    "|a|b|\n|---|---|",
    "[^1]: body",
  ];
  for (const prefix of prefixes) {
    for (const sep of ["", "\n", "\n\n"]) {
      for (const tail of tails) add("md-bounded-suffix", prefix + sep + tail);
    }
  }
}

// GH #209: split-title suffix CHAINS. mldoc's heading-title lookahead reuses the
// FULL block alternation (heading0.ml title_aux_p → mldoc_parser.ml parsers), so
// under a heading/bullet head every same-line block prefix composes with every
// block suffix. v2 realizes that alternation as bounded_split_suffix_blocks +
// callout_container_split_at behind the total split_suffix_blocks combinator;
// this product locks the combinator's totality at every recursive edge. The
// audit engine panics on any unowned case, so this matrix is also an
// ownership gate, not only a parity gate.
function addSplitSuffixCompositionCases() {
  const heads = ["- ", "# ", "- TODO [#A] "];
  const mids = [
    "$$m$$",
    "<i>h</i>",
    "[:div]",
    "#",
    ":PROPERTIES:\n:k: v\n:END:",
  ];
  const tails = [
    "#+BEGIN_NOTE\nn\n#+END_NOTE",
    "#+BEGIN_QUOTE\nq\n#+END_QUOTE",
    "#+BEGIN_NOTE\nunclosed",
    "#+BEGIN_SRC c\ns\n#+END_SRC",
    "```\nf\n```",
    "$$y$$",
    "<b>t</b>",
    "[:span]",
    "# h",
    "- b",
    "---",
    "|a|b|\n|---|---|",
    "[^1]: body",
    "\\begin{eq}\nx\n\\end{eq}",
    ":PROPERTIES:\n:p: q\n:END:",
    "plain",
  ];
  for (const head of heads)
    for (const mid of mids)
      for (const tail of tails) add("md-split-suffix-chain", `${head}${mid} ${tail}`);
  // two same-line block prefixes before the tail
  const mids2 = ["$$m$$", "<i>h</i>", "[:div]", "#"];
  const tails2 = ["#+BEGIN_NOTE\nn\n#+END_NOTE", "$$y$$", "<b>t</b>", "plain"];
  for (const m1 of mids2)
    for (const m2 of mids2)
      for (const tail of tails2) add("md-split-suffix-chain2", `- ${m1} ${m2} ${tail}`);
}

function addRefExtractionCases() {
  const values = [
    "[[Page]]",
    "#tag",
    "[[Page]] #tag",
    "\"[[Page]]\"",
    "{{query [[Page]]}}",
    "{{embed [[Page]]}}",
    "{{embed ((11111111-1111-1111-1111-111111111111))}}",
    "((11111111-1111-1111-1111-111111111111))",
    "[label](page.md)",
    "[Some](file:../x.md)",
    "[x](id://11111111-1111-1111-1111-111111111111)",
    "[[outer [[Inner]]]]",
    "[[A]] [[A]] #b",
    "",
  ];
  for (const value of values) add("md-ref-property-parse1", `key:: ${value}`);
  for (const value of values) add("md-ref-property-parse2", `#+KEY: ${value}`);
  for (const value of values) add("md-ref-list-property", `- item\n  key:: ${value}`);
}

function addSuppressedRewriteCases() {
  const bodies = [
    "#+TITLE: x",
    "#+RESULTS: ok",
    "#+BEGIN_x: no",
    "key:: value",
    "id:: 11111111-1111-1111-1111-111111111111",
    ":PROPERTIES:\n:k: v\n:END:",
    ":DRAWER:\nx\n:END:",
    ":NAME:\nx\n:END:",
    "[^1]: body",
    "[//]: # c",
  ];
  const wrappers = [
    ["md-rewrite-doc", (body) => body],
    ["md-rewrite-quote", (body) => body.split("\n").map((line) => `> ${line}`).join("\n")],
    ["md-rewrite-callout", (body) => `#+BEGIN_QUOTE\n${body}\n#+END_QUOTE`],
    ["md-rewrite-list", (body) => `* item\n  ${body.split("\n").join("\n  ")}`],
    ["md-rewrite-list-quote", (body) => `* item\n  > ${body.split("\n").join("\n  > ")}`],
  ];
  for (const [cat, wrap] of wrappers) {
    for (const body of bodies) add(cat, wrap(body));
  }
}

function addOrgListContentCases() {
  const bodies = [
    "#+TITLE: x",
    "#+RESULTS: ok",
    "#+BEGIN_x: no",
    "key:: value",
    ":PROPERTIES:\n:k: v\n:END:",
    ":DRAWER:\nx\n:END:",
    "[fn:1] body",
    "[//]: # c",
  ];
  for (const body of bodies) {
    add("org-list-content", `- a\n  ${body.split("\n").join("\n  ")}`, "org");
    add("org-list-content", `- a\n  more\n  ${body.split("\n").join("\n  ")}`, "org");
  }
}

addMarkdownBoldBodyCases();
addTopInlineCases();
addOrgTopInlineCases();
addOrgLinkLabelCases();
addOrgLink2ClassificationCases();
addBlockquoteBoundaryCases();
addMarkdownHardbreakBoundaryCases();
addLatexBoundaryCases();
addCalloutBoundaryCases();
addListItemBoundaryCases();
addHeadingSuffixCases();
addMarkdownBulletTitleEmphasisCases();
addTimestampBacktrackCases();
addMarkdownLinkLabelBracketCases();
addEmptyMarkerFenceCases();
addBoundedSuffixCases();
addSplitSuffixCompositionCases();
addRefExtractionCases();
addSuppressedRewriteCases();
addOrgListContentCases();

const corpusPath = join(__dir, "corpus.audit-v2-shortcuts.json");
const outPath = join(__dir, "audit-v2-shortcuts-out.json");
writeFileSync(
  corpusPath,
  JSON.stringify(cases.map(({ id, input, format }) => ({ id, input, format })), null, 0),
);

const env = {
  ...process.env,
  CARGO_HOME: "/aux/koutecky/logseq/.toolchain/cargo",
  RUSTUP_HOME: "/aux/koutecky/logseq/.toolchain/rustup",
  PATH: `/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}`,
};
const r = spawnSync(
  "cargo",
  ["run", "-q", "--bin", "lsdoc-parse", "--", "--engine", "v2", corpusPath, outPath],
  { cwd: repo, env, encoding: "utf8" },
);
if (r.status !== 0) {
  console.error("lsdoc-parse failed:\n" + (r.stderr || r.stdout || "").slice(-4000));
  process.exit(r.status ?? 1);
}

const byId = Object.fromEntries(JSON.parse(readFileSync(outPath, "utf8")).map((x) => [x.id, x]));
const diffs = [];
for (const c of cases) {
  let expected;
  try {
    expected = canon(oracle(c.input, c.format));
  } catch {
    continue;
  }
  const actual = canon(byId[c.id]?.projection);
  if (canonJSON(expected) !== canonJSON(actual)) {
    diffs.push({ ...c, expected, actual });
  }
}

console.log(`audit-v2-shortcuts: ${cases.length} cases, ${diffs.length} diffs`);
for (const d of diffs.slice(0, 25)) {
  console.log(`\nDIFF ${d.id} [${d.cat}] ${JSON.stringify(d.input)}`);
  console.log("  mldoc:", JSON.stringify(d.expected.blocks).slice(0, 900));
  console.log("  lsdoc:", JSON.stringify(d.actual.blocks).slice(0, 900));
}
if (diffs.length > 25) console.log(`\n... ${diffs.length - 25} more diff(s) hidden`);
process.exit(diffs.length ? 2 : 0);

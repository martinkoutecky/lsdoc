// Construct-seam cross-product enumerator.
// Batched lsdoc/oracle scan -> signature class grouping -> isolated representative confirmation.
import { HARNESS_DIR, outputPath, parseCommonArgs, runEnumeration, unique } from "./lib/enum-runner.mjs";

const MD_LINES = unique([
  "# h",
  "## h",
  "#",
  "## ",
  "- a",
  "-",
  "* a",
  "+ a",
  "1. a",
  "1. ",
  "> q",
  ">",
  "```",
  "```rust",
  "~~~",
  "#+BEGIN_QUOTE",
  "#+END_QUOTE",
  "#+BEGIN_SRC",
  "#+END_SRC",
  "#+BEGIN_EXAMPLE",
  "#+END_EXAMPLE",
  "#+TITLE: x",
  "#+NAME: v",
  "a:: 1",
  "id:: 1",
  ":PROPERTIES:",
  ":a:: 1",
  ":a: 1",
  ":END:",
  ":LOGBOOK:",
  "| a |",
  "|-|",
  "---",
  "***",
  "$$x$$",
  "$$",
  "<div>x</div>",
  "<!--",
  "-->",
  "[:div]",
  "[^1]: ab",
  "term",
  ": def x",
  "\\begin{x}",
  "\\end{x}",
  "a  ",
  "\f- a",
  "\x1a# h",
  "  a",
  "a",
  " ",
  "",
]);

const ORG_LINES = unique([
  "* h",
  "** h",
  "*",
  "* ",
  "* TODO [#A] x :tag:",
  "- a",
  "+ a",
  "1. a",
  "> q",
  ">",
  "```",
  "~~~",
  "#+BEGIN_QUOTE",
  "#+END_QUOTE",
  "#+BEGIN_SRC",
  "#+END_SRC",
  "#+BEGIN_EXAMPLE",
  "#+END_EXAMPLE",
  "#+BEGIN_NOTE",
  "#+END_NOTE",
  "#+TITLE: x",
  "#+NAME: v",
  ":PROPERTIES:",
  ":a: 1",
  ":END:",
  ":LOGBOOK:",
  ":abc",
  "# c",
  "#",
  "| a |",
  "-----",
  "$$x$$",
  "$$",
  "<div>x</div>",
  "[:div]",
  "[fn:1] ab",
  "\\begin{x}",
  "\\end{x}",
  "\f* h",
  "\x1a- a",
  "  a",
  "  #+TITLE: x",
  "  :abc",
  "a",
  " ",
  "",
]);

function* tuples(vocab, arity) {
  if (arity === 1) {
    for (const v of vocab) yield [v];
    return;
  }
  for (const rest of tuples(vocab, arity - 1)) {
    for (const v of vocab) yield [...rest, v];
  }
}

function* seamCases(format, vocab) {
  let n = 0;
  for (const parts of tuples(vocab, 2)) {
    yield {
      id: `${format}-lf2-${n++}`,
      format,
      input: parts.join("\n"),
      meta: { eol: "lf", arity: 2, lines: parts },
    };
  }
  n = 0;
  for (const parts of tuples(vocab, 3)) {
    yield {
      id: `${format}-lf3-${n++}`,
      format,
      input: parts.join("\n"),
      meta: { eol: "lf", arity: 3, lines: parts },
    };
  }
  n = 0;
  for (const parts of tuples(vocab, 2)) {
    yield {
      id: `${format}-crlf2-${n++}`,
      format,
      input: parts.join("\r\n"),
      meta: { eol: "crlf", arity: 2, lines: parts },
    };
  }
}

function main() {
  const options = parseCommonArgs(process.argv.slice(2));
  const findingsPath =
    options.findings || outputPath(options.smoke ? "/tmp/enum-seams-smoke-findings.json" : `${HARNESS_DIR}/enum-seams-findings.json`);
  runEnumeration({
    name: "enum-seams",
    findingsPath,
    options,
    groups: [
      { label: "markdown", cases: () => seamCases("markdown", MD_LINES) },
      { label: "org", cases: () => seamCases("org", ORG_LINES) },
    ],
  });
}

main();

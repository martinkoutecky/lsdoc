// Small-string total exhaustion enumerator.
// Batched lsdoc/oracle scan -> signature class grouping -> isolated representative confirmation.
import { HARNESS_DIR, outputPath, parseCommonArgs, runEnumeration } from "./lib/enum-runner.mjs";

const DEFAULTS = {
  markdown: ["-", "*", "#", ">", "`", "[", "]", "$", "\\", ":", "\n", " "],
  org: ["*", "-", "+", ":", "#", "[", "]", "_", "^", "~", "\n", " "],
};

function decodeAlphabet(s) {
  return [...s.replaceAll("\\n", "\n").replaceAll("\\t", "\t").replaceAll("\\s", " ")];
}

function parseArgs(argv) {
  const enumArgs = [];
  const local = { maxLen: 5, alphabet: null };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const readValue = (prefix) => {
      if (arg.includes("=")) return arg.slice(prefix.length + 1);
      if (i + 1 >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[++i];
    };
    if (arg.startsWith("--max-len")) {
      local.maxLen = parseInt(readValue("--max-len"), 10);
    } else if (arg.startsWith("--alphabet")) {
      local.alphabet = decodeAlphabet(readValue("--alphabet"));
    } else {
      enumArgs.push(arg);
    }
  }
  if (!Number.isFinite(local.maxLen) || local.maxLen < 1) throw new Error("--max-len must be >= 1");
  return { local, options: parseCommonArgs(enumArgs) };
}

function* stringsOfLength(alphabet, len, prefix = "") {
  if (len === 0) {
    yield prefix;
    return;
  }
  for (const ch of alphabet) yield* stringsOfLength(alphabet, len - 1, prefix + ch);
}

function* smallCases(format, alphabet, maxLen) {
  let n = 0;
  for (let len = 1; len <= maxLen; len++) {
    for (const input of stringsOfLength(alphabet, len)) {
      yield {
        id: `${format}-small-${n++}`,
        format,
        input,
        meta: { len, alphabet: alphabet.map((ch) => (ch === "\n" ? "\\n" : ch)) },
      };
    }
  }
}

function main() {
  const { local, options } = parseArgs(process.argv.slice(2));
  const findingsPath =
    options.findings || outputPath(options.smoke ? "/tmp/enum-small-smoke-findings.json" : `${HARNESS_DIR}/enum-small-findings.json`);
  const alphabetFor = (format) => local.alphabet || DEFAULTS[format];
  runEnumeration({
    name: "enum-small",
    findingsPath,
    options,
    groups: [
      { label: "markdown", cases: () => smallCases("markdown", alphabetFor("markdown"), local.maxLen) },
      { label: "org", cases: () => smallCases("org", alphabetFor("org"), local.maxLen) },
    ],
  });
}

main();

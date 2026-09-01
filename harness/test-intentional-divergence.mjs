import assert from "node:assert/strict";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";
import {
  classifyNestedDollar,
  createFreshParsers,
  NESTED_DOLLAR_KIND,
} from "./lib/intentional-divergence.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const parsers = createFreshParsers({
  lsdocPath: process.env.LSDOC_PARSE || join(here, "..", "target", "debug", "lsdoc-parse"),
});

const classify = (input, entrypoint = "inline", projection = null, format = "md") => {
  const lsdocOriginal = projection ?? parsers.lsdoc(input, format, entrypoint);
  const before = JSON.stringify(lsdocOriginal);
  const result = classifyNestedDollar({ input, format, entrypoint, lsdocOriginal, parsers });
  assert.equal(JSON.stringify(lsdocOriginal), before, "classifier mutated its input projection");
  return { result, projection: lsdocOriginal };
};

try {
  for (const input of [
    "*$x$*",
    "*a $x$ b $y$ c*",
    "[*$x$*](u)",
    "_[*$x$*](u)_",
    "*$x_{i}$*",
    "*$a[[Foo]]b$*",
    "*_a_$x$*",
  ]) {
    const { result } = classify(input);
    assert.equal(result.status, "intentional", input);
    assert.equal(result.kind, NESTED_DOLLAR_KIND, input);
  }

  const block = classify("*$a[[Foo]]b$*", "block");
  assert.equal(block.result.status, "intentional");
  assert.deepEqual(block.projection.refs.page, [], "opaque math must not retain the page ref");
  assert.deepEqual(
    parsers.oracleBlock("*$a[[Foo]]b$*", "md").projection.refs.page,
    ["Foo"],
    "the accepted ref delta must be a proven removal",
  );

  assert.equal(classify("$x$").result.status, "unclassified");
  assert.equal(classify("*[$x$](u)*").result.status, "unclassified");
  assert.equal(classify("*$x$*", "inline", null, "org").result.status, "unclassified");

  const valid = parsers.lsdoc("*$x$*", "md", "inline");
  const malformedSpan = structuredClone(valid);
  malformedSpan[0].children[0].span = [1, 999];
  assert.equal(classify("*$x$*", "inline", malformedSpan).result.status, "unclassified");

  const unknownMode = structuredClone(valid);
  unknownMode[0].children[0].mode = "Unknown";
  assert.equal(classify("*$x$*", "inline", unknownMode).result.status, "unclassified");

  const overlap = structuredClone(valid);
  overlap[0].children.push(structuredClone(overlap[0].children[0]));
  assert.equal(classify("*$x$*", "inline", overlap).result.status, "unclassified");

  const siblingChange = structuredClone(valid);
  siblingChange.push({ k: "plain", text: "unrelated", span: [5, 5] });
  assert.equal(classify("*$x$*", "inline", siblingChange).result.status, "unclassified");

  const linked = parsers.lsdoc("[*$x$*](u)", "md", "inline");
  const badFull = structuredClone(linked);
  badFull[0].full = "not-source-identical";
  assert.equal(classify("[*$x$*](u)", "inline", badFull).result.status, "unclassified");

  const refAddition = parsers.lsdoc("*$a[[Foo]]b$*", "md", "block");
  refAddition.refs.page.push("Invented");
  assert.equal(
    classify("*$a[[Foo]]b$*", "block", refAddition).result.status,
    "unclassified",
  );

  const invalidStandalone = [{
    k: "emphasis",
    emph: "Italic",
    children: [{ k: "latex", mode: "Inline", body: " x", span: [1, 5] }],
    span: [0, 6],
  }];
  assert.equal(
    classify("*$ x$*", "inline", invalidStandalone).result.reason,
    "standalone-shape",
  );

  const backslashDelimited = [{
    k: "emphasis",
    emph: "Italic",
    children: [{ k: "latex", mode: "Inline", body: "x", span: [1, 6] }],
    span: [0, 7],
  }];
  assert.equal(
    classify("*\\(x\\)*", "inline", backslashDelimited).result.reason,
    "source",
  );

  const leakProjection = parsers.lsdoc("*$x$*", "md", "inline");
  const leakParsers = {
    ...parsers,
    oracleInline(input, format) {
      if (input === "*$x$*") return { inline: structuredClone(leakProjection), err: null };
      return parsers.oracleInline(input, format);
    },
  };
  assert.deepEqual(
    classifyNestedDollar({
      input: "*$x$*",
      format: "md",
      entrypoint: "inline",
      lsdocOriginal: leakProjection,
      parsers: leakParsers,
    }),
    { status: "oracle_leak", reason: "isolated-match" },
  );

  const structuralProjection = parsers.lsdoc("*$x$*", "md", "block");
  const structuralParsers = {
    ...parsers,
    oracleBlock(input, format) {
      const result = parsers.oracleBlock(input, format);
      result.projection.blocks.push({ kind: "hr" });
      return result;
    },
  };
  assert.equal(
    classifyNestedDollar({
      input: "*$x$*",
      format: "md",
      entrypoint: "block",
      lsdocOriginal: structuralProjection,
      parsers: structuralParsers,
    }).reason,
    "structure",
  );

  console.log("intentional-divergence classifier: focused fail-closed tests passed");
} finally {
  parsers.close();
}

import { readFileSync, writeFileSync } from "node:fs";
import { test, expect } from "vitest";
// Import the REAL Tine inline parser by absolute path (repo untouched).
import { parseInline, type Seg } from "/aux/koutecky/logseq/logseq-claude/src/render/parseInline.ts";

const CORPUS = "/tmp/claude-3042/-aux-koutecky-logseq/2e921412-0c07-49c5-87de-46be358044a0/scratchpad/parser-divergence/corpus.json";
const OUT = "/tmp/claude-3042/-aux-koutecky-logseq/2e921412-0c07-49c5-87de-46be358044a0/scratchpad/parser-divergence/ts-out.json";

type Corpus = { id: string; cat: string; input: string }[];

// Recursively walk segments (emphasis nests parsed sub-segments) collecting refs.
function walk(segs: Seg[], acc: { page: string[]; block: string[]; macro: string[] }) {
  for (const s of segs) {
    switch (s.t) {
      case "pageref": acc.page.push(s.name); break;
      case "tag": acc.page.push(s.name); break;
      case "blockref": acc.block.push(s.id); break;
      case "macro": acc.macro.push(s.body); break;
      case "bold": case "italic": case "underline": case "strike": case "highlight":
        walk(s.v, acc); break;
      default: break;
    }
  }
}

test("run parseInline over corpus", () => {
  const corpus: Corpus = JSON.parse(readFileSync(CORPUS, "utf8"));
  const results = corpus.map((c) => {
    const acc = { page: [] as string[], block: [] as string[], macro: [] as string[] };
    walk(parseInline(c.input, "md"), acc);
    return { id: c.id, page: acc.page, block: acc.block, macro: acc.macro };
  });
  writeFileSync(OUT, JSON.stringify(results, null, 1));
  expect(results.length).toBe(corpus.length);
});

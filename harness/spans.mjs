// Inline source-span invariant pass over the lsdoc differential corpus.
//
// lsdoc parses the WHOLE corpus `input` into blocks, and every inline node's `span` is an
// ABSOLUTE `[start, end)` byte range into that same `input` (the base a paragraph/heading/
// table-cell was sliced from). So the "block body" the spec's S1/S5 index into is `entry.input`.
//
// Checks, for every inline node that carries a non-null span (FOLDED-buffer nodes carry NO
// span and are skipped):
//   S1  in bounds:      0 <= start <= end <= byte_len(input)
//   S3  containment:    parent.start <= child.start <= child.end <= parent.end
//   S4  ordered/no-overlap siblings within one run: a.end <= b.start
//   S5  plain fidelity: input[start..end] == plain.text  (byte-for-byte)
//
// Exits 0 if no violations, non-zero otherwise (a gate).
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dir = dirname(fileURLToPath(import.meta.url));
const data = JSON.parse(readFileSync(join(__dir, "lsdoc-out.json"), "utf8"));

let violations = 0;
const MAX_REPORT = 40;
function report(msg) {
  if (violations < MAX_REPORT) console.error(msg);
  violations++;
}

function checkSpan(label, span, bodyLen) {
  if (span == null) return;
  const [s, e] = span;
  if (!(0 <= s && s <= e && e <= bodyLen)) {
    report(`S1 violation at ${label}: span [${s},${e}) body_len=${bodyLen}`);
  }
}

function checkPlain(label, text, span, bodyBytes) {
  if (span == null) return;
  const [s, e] = span;
  const slice = bodyBytes.subarray(s, e);
  const textBytes = Buffer.from(text, "utf8");
  if (!slice.equals(textBytes)) {
    report(`S5 violation at ${label}: plain '${text}' span [${s},${e}) source '${slice.toString("utf8")}'`);
  }
}

function checkSiblings(label, nodes) {
  let prevEnd = -1;
  for (const n of nodes) {
    if (n.span == null) continue;
    const [s, e] = n.span;
    if (s < prevEnd) report(`S4 violation at ${label}: sibling overlap prev_end=${prevEnd} start=${s}`);
    prevEnd = e;
  }
}

function checkContainment(parentLabel, parentSpan, childLabel, childSpan) {
  if (parentSpan == null || childSpan == null) return;
  const [ps, pe] = parentSpan;
  const [cs, ce] = childSpan;
  if (!(ps <= cs && ce <= pe)) {
    report(`S3 violation: parent ${parentLabel} [${ps},${pe}) vs child ${childLabel} [${cs},${ce})`);
  }
}

function walkInlines(nodes, bodyBytes, bodyLen, ctx, parentSpan) {
  checkSiblings(ctx, nodes);
  for (const n of nodes) {
    const nl = `${ctx}/${n.k}`;
    checkSpan(nl, n.span, bodyLen);
    if (parentSpan != null) checkContainment(ctx, parentSpan, nl, n.span);
    if (n.k === "plain") checkPlain(nl, n.text, n.span, bodyBytes);
    // Inline children live under `children` (emphasis/sub/sup/tag) or `label` (link).
    const kids = n.children || n.label || [];
    if (kids.length > 0) walkInlines(kids, bodyBytes, bodyLen, nl, n.span);
  }
}

function walkListItem(item, bodyBytes, bodyLen, p) {
  if (Array.isArray(item.name)) walkInlines(item.name, bodyBytes, bodyLen, `${p}.name`, null);
  if (Array.isArray(item.content)) {
    item.content.forEach((b, i) => walkBlock(b, bodyBytes, bodyLen, `${p}.content[${i}]`));
  }
  if (Array.isArray(item.items)) {
    item.items.forEach((it, i) => walkListItem(it, bodyBytes, bodyLen, `${p}.items[${i}]`));
  }
}

function walkBlock(block, bodyBytes, bodyLen, p) {
  if (Array.isArray(block.inline)) {
    walkInlines(block.inline, bodyBytes, bodyLen, `${p}.inline`, null);
  }
  if (Array.isArray(block.header)) {
    block.header.forEach((cell, ci) => walkInlines(cell, bodyBytes, bodyLen, `${p}.header[${ci}]`, null));
  }
  if (Array.isArray(block.rows)) {
    block.rows.forEach((row, ri) =>
      row.forEach((cell, ci) => walkInlines(cell, bodyBytes, bodyLen, `${p}.rows[${ri}][${ci}]`, null)));
  }
  // Quote/Custom nest child BLOCKS under `children`.
  if (Array.isArray(block.children)) {
    block.children.forEach((c, i) => walkBlock(c, bodyBytes, bodyLen, `${p}.children[${i}]`));
  }
  if (Array.isArray(block.items)) {
    block.items.forEach((it, i) => walkListItem(it, bodyBytes, bodyLen, `${p}.items[${i}]`));
  }
}

for (const entry of data) {
  const body = entry.input ?? "";
  const bodyBytes = Buffer.from(body, "utf8");
  const bodyLen = bodyBytes.length;
  const blocks = entry.projection?.blocks ?? [];
  blocks.forEach((b, i) => walkBlock(b, bodyBytes, bodyLen, `entry[${entry.id}].blocks[${i}]`));
}

if (violations > 0) {
  console.error(`\nSpan validation: ${violations} violation(s)`);
  process.exit(1);
} else {
  console.log(`Span validation: 0 violations`);
}

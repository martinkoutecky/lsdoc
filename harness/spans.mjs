// Inline source-span invariant pass over the lsdoc differential corpus.
//
// lsdoc parses the WHOLE corpus `input` into blocks, and every inline node's `span` is an
// ABSOLUTE `[start, end)` byte range into that same `input` (the base a paragraph/heading/
// table-cell was sliced from). So the "block body" the spec's S1/S5 index into is `entry.input`.
//
// Checks, for every inline node:
//   S1  in bounds:      0 <= start <= end <= byte_len(input)
//   S3  containment:    parent.start <= child.start <= child.end <= parent.end
//   S4  ordered/no-overlap siblings within one run: a.end <= b.start
//   S5  plain fidelity: input[start..end] == plain.text  (byte-for-byte) when no span_map
//   S6  mapped plain fidelity: strict, in-span, byte-equal span_map segments when S5 fails
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

function validSpan(label, span, bodyLen) {
  if (!Array.isArray(span) || span.length !== 2) {
    report(`T1 violation at ${label}: missing inline span`);
    return null;
  }
  const [s, e] = span;
  if (!Number.isInteger(s) || !Number.isInteger(e) || !(0 <= s && s <= e && e <= bodyLen)) {
    report(`S1 violation at ${label}: span [${s},${e}) body_len=${bodyLen}`);
    return null;
  }
  return [s, e];
}

function checkPlain(label, text, checkedSpan, spanMap, bodyBytes) {
  if (checkedSpan == null) return;
  const [s, e] = checkedSpan;
  const slice = bodyBytes.subarray(s, e);
  const textBytes = Buffer.from(text, "utf8");
  const s5 = slice.equals(textBytes);
  if (spanMap == null) {
    if (!s5) {
      report(`S5 violation at ${label}: plain '${text}' span [${s},${e}) source '${slice.toString("utf8")}'`);
    }
    return;
  }
  if (s5) {
    report(`S6 violation at ${label}: gratuitous span_map on S5 plain '${text}' span [${s},${e})`);
  }
  if (!Array.isArray(spanMap)) {
    report(`S6 violation at ${label}: span_map is not an array`);
    return;
  }
  let prevTextStart = -1;
  let prevTextEnd = 0;
  let prevSrcStart = -1;
  let prevSrcEnd = 0;
  for (let i = 0; i < spanMap.length; i++) {
    const seg = spanMap[i];
    if (!Array.isArray(seg) || seg.length !== 3) {
      report(`S6 violation at ${label}: span_map[${i}] is not [text_off,src_off,len]`);
      continue;
    }
    const [textOff, srcOff, len] = seg;
    if (![textOff, srcOff, len].every(Number.isInteger) || textOff < 0 || srcOff < 0 || len <= 0) {
      report(`S6 violation at ${label}: invalid segment ${JSON.stringify(seg)}`);
      continue;
    }
    if (textOff <= prevTextStart || srcOff <= prevSrcStart || textOff < prevTextEnd || srcOff < prevSrcEnd) {
      report(`S6 violation at ${label}: non-increasing/overlapping segment ${JSON.stringify(seg)}`);
    }
    if (textOff + len > textBytes.length) {
      report(`S6 violation at ${label}: segment ${JSON.stringify(seg)} exceeds plain text length ${textBytes.length}`);
      continue;
    }
    if (!(s <= srcOff && srcOff + len <= e)) {
      report(`S6 violation at ${label}: segment ${JSON.stringify(seg)} outside node span [${s},${e})`);
      continue;
    }
    if (srcOff + len > bodyBytes.length) {
      report(`S6 violation at ${label}: segment ${JSON.stringify(seg)} exceeds body length ${bodyBytes.length}`);
      continue;
    }
    const textSlice = textBytes.subarray(textOff, textOff + len);
    const sourceSlice = bodyBytes.subarray(srcOff, srcOff + len);
    if (!sourceSlice.equals(textSlice)) {
      report(`S6 violation at ${label}: segment ${JSON.stringify(seg)} text '${textSlice.toString("utf8")}' source '${sourceSlice.toString("utf8")}'`);
    }
    prevTextStart = textOff;
    prevTextEnd = textOff + len;
    prevSrcStart = srcOff;
    prevSrcEnd = srcOff + len;
  }
}

function spanOrNull(n) {
  return Array.isArray(n.span) && n.span.length === 2 ? n.span : null;
}

function checkSiblings(label, nodes) {
  let prevEnd = -1;
  for (const n of nodes) {
    const sp = spanOrNull(n);
    if (sp == null) continue;
    const [s, e] = sp;
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
    const checked = validSpan(nl, n.span, bodyLen);
    if (parentSpan != null) checkContainment(ctx, parentSpan, nl, checked);
    if (n.k === "plain") {
      checkPlain(nl, n.text, checked, n.span_map, bodyBytes);
    } else if (n.span_map != null) {
      report(`S6 violation at ${nl}: span_map is only valid on plain nodes`);
    }
    // Inline children live under `children` (emphasis/sub/sup/tag) or `label` (link).
    const kids = n.children || n.label || [];
    if (kids.length > 0) walkInlines(kids, bodyBytes, bodyLen, nl, checked);
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

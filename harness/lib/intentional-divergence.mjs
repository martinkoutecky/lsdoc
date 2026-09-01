import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { canonJSON } from "./compare.mjs";

export const NESTED_DOLLAR_KIND = "md-nested-dollar-latex";
const HERE = dirname(fileURLToPath(import.meta.url));
const HARNESS = dirname(HERE);
const REPO = dirname(HARNESS);
const ORACLE_BLOCK = join(HARNESS, "oracle.mjs");
const ORACLE_INLINE = join(HARNESS, "oracle-inline.mjs");

const clone = (value) => structuredClone(value);

export function blockSkeleton(blocks) {
  const block = (b) => {
    if (!b || typeof b !== "object") return b;
    const out = { kind: b.kind };
    for (const key of [
      "level", "size", "lang", "code", "props", "span", "name", "htags",
      "text", "marker", "priority", "value", "content",
    ]) if (key in b) out[key] = b[key];
    if (b.children) out.children = b.children.map(block);
    if (b.items) out.items = b.items.map(item);
    if (b.kind === "table") {
      out.header = b.header ? b.header.length : null;
      out.rows = (b.rows ?? []).map((row) => row.length);
    }
    return out;
  };
  const item = (it) => ({
    ordered: it.ordered,
    number: it.number,
    indent: it.indent,
    content: (it.content ?? []).map(block),
    items: (it.items ?? []).map(item),
  });
  return (blocks ?? []).map(block);
}

function discoverCandidates(root) {
  const candidates = [];
  const walk = (value, path, links) => {
    if (Array.isArray(value)) {
      value.forEach((child, index) => walk(child, [...path, index], links));
      return;
    }
    if (!value || typeof value !== "object") return;
    const nextLinks = value.k === "link" ? [...links, { node: value, path }] : links;
    if (value.k === "emphasis" && Array.isArray(value.children)) {
      value.children.forEach((child, index) => {
        const childPath = [...path, "children", index];
        if (child?.k === "latex") {
          candidates.push({
            node: child,
            path: childPath,
            parentPath: [...path, "children"],
            links: nextLinks,
          });
        }
        walk(child, childPath, nextLinks);
      });
      for (const [key, child] of Object.entries(value)) {
        if (key !== "children") walk(child, [...path, key], nextLinks);
      }
      return;
    }
    for (const [key, child] of Object.entries(value)) {
      walk(child, [...path, key], nextLinks);
    }
  };
  walk(root, [], []);
  return candidates;
}

function atPath(root, path) {
  let value = root;
  for (const part of path) value = value?.[part];
  return value;
}

function maskBuffer(source, candidates, byte) {
  const masked = Buffer.from(source);
  for (const candidate of candidates) masked.fill(byte, candidate.start, candidate.end);
  return masked;
}

function maskText(length, byte) {
  return String.fromCharCode(byte).repeat(length);
}

function coalescePlains(children) {
  const out = [];
  for (const child of children) {
    const previous = out.at(-1);
    if (previous?.k === "plain" && child?.k === "plain") {
      previous.text += child.text;
      if (Array.isArray(previous.span) && Array.isArray(child.span)) {
        previous.span = [previous.span[0], child.span[1]];
      } else {
        delete previous.span;
      }
      delete previous.span_map;
    } else {
      out.push(child);
    }
  }
  return out;
}

function validateAndDescribe(source, format, candidates, parseOracleInline) {
  if (format !== "md" || candidates.length === 0) return { ok: false, reason: "scope" };
  const sourceBytes = Buffer.from(source);
  const ranges = [];
  for (const candidate of candidates) {
    const { mode, body, span } = candidate.node;
    if (!Array.isArray(span) || span.length !== 2 || !span.every(Number.isInteger)) {
      return { ok: false, reason: "span" };
    }
    const [start, end] = span;
    if (start < 0 || start >= end || end > sourceBytes.length) {
      return { ok: false, reason: "bounds" };
    }
    const delimiter = mode === "Inline" ? "$" : mode === "Displayed" ? "$$" : null;
    if (!delimiter || typeof body !== "string") return { ok: false, reason: "mode" };
    const expected = Buffer.from(`${delimiter}${body}${delimiter}`);
    if (!sourceBytes.subarray(start, end).equals(expected)) {
      return { ok: false, reason: "source" };
    }
    const standalone = parseOracleInline(sourceBytes.subarray(start, end).toString("utf8"), format);
    if (standalone.err || standalone.inline?.length !== 1) {
      return { ok: false, reason: "standalone" };
    }
    const only = standalone.inline[0];
    if (only?.k !== "latex" || only.mode !== mode || only.body !== body) {
      return { ok: false, reason: "standalone-shape" };
    }
    candidate.start = start;
    candidate.end = end;
    ranges.push([start, end]);
  }
  ranges.sort((a, b) => a[0] - b[0]);
  for (let i = 1; i < ranges.length; i++) {
    if (ranges[i][0] < ranges[i - 1][1]) return { ok: false, reason: "overlap" };
  }
  return { ok: true, sourceBytes };
}

function transformedOriginal(original, sourceBytes, candidates, byte) {
  const transformed = clone(original);
  const parentPaths = new Map();
  const linkMasks = new Map();
  for (const candidate of candidates) {
    const node = atPath(transformed, candidate.path);
    if (!node || node.k !== "latex") return null;
    const replacement = {
      k: "plain",
      text: maskText(candidate.end - candidate.start, byte),
    };
    if (Array.isArray(node.span)) replacement.span = node.span;
    const parent = atPath(transformed, candidate.parentPath);
    const index = candidate.path.at(-1);
    parent[index] = replacement;
    parentPaths.set(JSON.stringify(candidate.parentPath), candidate.parentPath);

    for (const link of candidate.links) {
      const clonedLink = atPath(transformed, link.path);
      if (!Array.isArray(clonedLink?.span) || typeof clonedLink.full !== "string") return null;
      const [linkStart, linkEnd] = clonedLink.span;
      if (candidate.start < linkStart || candidate.end > linkEnd) return null;
      const sourceSlice = sourceBytes.subarray(linkStart, linkEnd);
      if (!sourceSlice.equals(Buffer.from(clonedLink.full))) return null;
      const key = JSON.stringify(link.path);
      if (!linkMasks.has(key)) linkMasks.set(key, { path: link.path, start: linkStart, bytes: Buffer.from(sourceSlice) });
      const entry = linkMasks.get(key);
      entry.bytes.fill(byte, candidate.start - linkStart, candidate.end - linkStart);
    }
  }
  for (const path of parentPaths.values()) {
    const children = atPath(transformed, path);
    const owner = atPath(transformed, path.slice(0, -1));
    owner[path.at(-1)] = coalescePlains(children);
  }
  for (const entry of linkMasks.values()) {
    atPath(transformed, entry.path).full = entry.bytes.toString("utf8");
  }
  return transformed;
}

function refsSubset(lsdocRefs, oracleRefs) {
  for (const key of ["page", "block"]) {
    const superset = new Set(oracleRefs?.[key] ?? []);
    for (const value of lsdocRefs?.[key] ?? []) if (!superset.has(value)) return false;
  }
  return true;
}

export function createFreshParsers({ lsdocPath, lsdocArgs = [] }) {
  const temp = mkdtempSync(join(tmpdir(), "lsdoc-intentional-"));
  let serial = 0;
  const paths = () => {
    serial++;
    return {
      input: join(temp, `input-${serial}.json`),
      output: join(temp, `output-${serial}.json`),
    };
  };
  const oracleBlock = (input, format) => {
    const p = paths();
    writeFileSync(p.input, JSON.stringify([{ id: "one", input, format }]));
    execFileSync("node", [ORACLE_BLOCK, p.input, p.output], { cwd: HARNESS, stdio: "ignore" });
    return JSON.parse(readFileSync(p.output, "utf8"))[0];
  };
  const oracleInline = (input, format) => {
    const p = paths();
    writeFileSync(p.input, JSON.stringify({ input, format }));
    execFileSync("node", [ORACLE_INLINE, p.input, p.output], { cwd: HARNESS, stdio: "ignore" });
    return JSON.parse(readFileSync(p.output, "utf8"));
  };
  const lsdoc = (input, format, entrypoint) => {
    const p = paths();
    writeFileSync(p.input, JSON.stringify([{ id: "one", input, format }]));
    const env = { ...process.env };
    if (entrypoint === "inline") env.LSDOC_INLINE = "1";
    execFileSync(lsdocPath, [...lsdocArgs, p.input, p.output], {
      cwd: REPO,
      env,
      stdio: "ignore",
    });
    const result = JSON.parse(readFileSync(p.output, "utf8"))[0];
    return entrypoint === "inline" ? result.inline : result.projection;
  };
  return {
    oracleBlock,
    oracleInline,
    lsdoc,
    close: () => rmSync(temp, { recursive: true, force: true }),
  };
}

export function classifyNestedDollar({
  input,
  format,
  entrypoint,
  lsdocOriginal,
  parsers,
}) {
  if (format !== "md") return { status: "unclassified", reason: "format" };
  const candidates = discoverCandidates(lsdocOriginal);
  const valid = validateAndDescribe(input, format, candidates, parsers.oracleInline);
  if (!valid.ok) return { status: "unclassified", reason: valid.reason };

  const isolatedOracleResult = entrypoint === "inline"
    ? parsers.oracleInline(input, format)
    : parsers.oracleBlock(input, format);
  if (isolatedOracleResult.err) return { status: "unclassified", reason: "oracle-error" };
  const isolatedOracle = entrypoint === "inline"
    ? isolatedOracleResult.inline
    : isolatedOracleResult.projection;
  if (canonJSON(isolatedOracle) === canonJSON(lsdocOriginal)) {
    return { status: "oracle_leak", reason: "isolated-match" };
  }
  if (entrypoint === "block"
      && canonJSON(blockSkeleton(isolatedOracle.blocks))
        !== canonJSON(blockSkeleton(lsdocOriginal.blocks))) {
    return { status: "unclassified", reason: "structure" };
  }
  if (entrypoint === "block" && !refsSubset(lsdocOriginal.refs, isolatedOracle.refs)) {
    return { status: "unclassified", reason: "ref-addition" };
  }

  for (const byte of [",".charCodeAt(0), "q".charCodeAt(0)]) {
    const maskedInput = maskBuffer(valid.sourceBytes, candidates, byte).toString("utf8");
    const maskedOracleResult = entrypoint === "inline"
      ? parsers.oracleInline(maskedInput, format)
      : parsers.oracleBlock(maskedInput, format);
    if (maskedOracleResult.err) continue;
    const maskedOracle = entrypoint === "inline"
      ? maskedOracleResult.inline
      : maskedOracleResult.projection;
    const maskedLsdoc = parsers.lsdoc(maskedInput, format, entrypoint);
    if (canonJSON(maskedOracle) !== canonJSON(maskedLsdoc)) continue;
    const transformed = transformedOriginal(lsdocOriginal, valid.sourceBytes, candidates, byte);
    if (!transformed || canonJSON(transformed) !== canonJSON(maskedLsdoc)) continue;
    if (entrypoint === "block"
        && canonJSON(lsdocOriginal.refs) !== canonJSON(maskedLsdoc.refs)) continue;
    return {
      status: "intentional",
      kind: NESTED_DOLLAR_KIND,
      mask: String.fromCharCode(byte),
      candidates: candidates.length,
    };
  }
  return { status: "unclassified", reason: "mask-proof" };
}

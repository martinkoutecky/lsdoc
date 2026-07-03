import { execFileSync } from "child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { dirname, isAbsolute, join, resolve } from "path";
import { fileURLToPath } from "url";
import { canon, canonJSON } from "./compare.mjs";

export const HARNESS_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
export const REPO_DIR = dirname(HARNESS_DIR);
const ORACLE = join(HARNESS_DIR, "oracle.mjs");
const DEFAULT_LSDOC = join(REPO_DIR, "target", "release", "lsdoc-parse");

export function parseCommonArgs(argv) {
  const args = {
    chunk: 20000,
    reps: 3,
    limit: Infinity,
    formats: null,
    smoke: false,
    findings: null,
    lsdoc: process.env.LSDOC_PARSE || DEFAULT_LSDOC,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const readValue = (prefix) => {
      if (arg.includes("=")) return arg.slice(prefix.length + 1);
      if (i + 1 >= argv.length) throw new Error(`missing value for ${arg}`);
      return argv[++i];
    };
    if (arg === "--smoke") {
      args.smoke = true;
      args.limit = Math.min(args.limit, 100);
    } else if (arg.startsWith("--limit")) {
      args.limit = parseInt(readValue("--limit"), 10);
    } else if (arg.startsWith("--chunk")) {
      args.chunk = parseInt(readValue("--chunk"), 10);
    } else if (arg.startsWith("--reps")) {
      args.reps = parseInt(readValue("--reps"), 10);
    } else if (arg.startsWith("--formats")) {
      args.formats = readValue("--formats")
        .split(",")
        .map((s) => normalizeFormat(s.trim()))
        .filter(Boolean);
    } else if (arg.startsWith("--findings")) {
      args.findings = outputPath(readValue("--findings"));
    } else if (arg.startsWith("--lsdoc")) {
      args.lsdoc = outputPath(readValue("--lsdoc"));
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (!Number.isFinite(args.limit)) args.limit = Infinity;
  if (!Number.isFinite(args.chunk) || args.chunk < 1 || args.chunk > 50000) {
    throw new Error("--chunk must be in the range 1..50000");
  }
  if (!Number.isFinite(args.reps) || args.reps < 1) {
    throw new Error("--reps must be >= 1");
  }
  if (!existsSync(args.lsdoc)) {
    throw new Error(`release lsdoc-parse binary not found: ${args.lsdoc}`);
  }
  return args;
}

export function outputPath(path) {
  return isAbsolute(path) ? path : resolve(process.cwd(), path);
}

export function normalizeFormat(format) {
  if (format === "md" || format === "markdown") return "markdown";
  if (format === "org") return "org";
  return format;
}

export function projection(o) {
  if (o == null) return null;
  return canon({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });
}

function projectionKey(o) {
  if (o == null) return "null";
  return canonJSON({ blocks: o.blocks || o, refs: o.refs || { page: [], block: [] } });
}

function stableJSON(v) {
  if (Array.isArray(v)) return `[${v.map(stableJSON).join(",")}]`;
  if (v && typeof v === "object") {
    return `{${Object.keys(v)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableJSON(v[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(v);
}

function shape(v, key = "") {
  if (Array.isArray(v)) return v.map((x) => shape(x));
  if (v && typeof v === "object") {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = shape(v[k], k);
    return o;
  }
  if (typeof v === "string") {
    if (key === "kind" || key === "mode") return v;
    return "S";
  }
  if (typeof v === "number") return "N";
  if (typeof v === "boolean") return v;
  return v === null ? null : typeof v;
}

function signature(diff) {
  return [
    diff.format,
    stableJSON(shape(diff.mldoc)),
    "||",
    stableJSON(shape(diff.lsdoc)),
  ].join(" ");
}

function representatives(members, reps) {
  const indexes = [0, Math.floor(members.length / 2), members.length - 1];
  const out = [];
  const seen = new Set();
  for (const idx of indexes) {
    const v = members[idx];
    if (!v || seen.has(v.id)) continue;
    seen.add(v.id);
    out.push(v);
    if (out.length >= reps) break;
  }
  return out;
}

function caseForWire(c) {
  return { id: c.id, format: c.format, input: c.input };
}

function runLsdoc(lsdoc, inPath, outPath) {
  execFileSync(lsdoc, [inPath, outPath], { cwd: REPO_DIR, stdio: "ignore" });
  return JSON.parse(readFileSync(outPath, "utf8"));
}

function runOracle(inPath) {
  execFileSync("node", [ORACLE, inPath], { cwd: HARNESS_DIR, stdio: "ignore" });
  return JSON.parse(readFileSync(join(HARNESS_DIR, "oracle-out.json"), "utf8"));
}

function statFor(stats, key) {
  if (!stats.has(key)) stats.set(key, { cases: 0, rawDiffs: 0, classes: 0, confirmedClasses: 0 });
  return stats.get(key);
}

function sortClassEntries(classes) {
  return [...classes.entries()].sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0]));
}

export function runEnumeration({ name, groups, findingsPath, options }) {
  const started = Date.now();
  const temp = mkdtempSync(join(tmpdir(), `lsdoc-${name}-`));
  const inPath = join(temp, "in.json");
  const lsPath = join(temp, "lsdoc.json");
  const stats = new Map();
  const rawDiffs = [];
  let total = 0;
  let emitted = 0;
  let chunk = [];

  const flush = () => {
    if (!chunk.length) return;
    writeFileSync(inPath, JSON.stringify(chunk.map(caseForWire)));
    const lsdoc = Object.fromEntries(runLsdoc(options.lsdoc, inPath, lsPath).map((x) => [x.id, x]));
    const oracle = runOracle(inPath);
    const byId = Object.fromEntries(chunk.map((c) => [c.id, c]));
    for (const o of oracle) {
      const c = byId[o.id];
      const l = lsdoc[o.id];
      if (!l) throw new Error(`missing lsdoc projection for ${o.id}`);
      const lm = projection(l.projection);
      const om = projection(o.projection);
      const lk = projectionKey(l.projection);
      const ok = projectionKey(o.projection);
      if (lk !== ok) {
        rawDiffs.push({ ...c, mldoc: om, lsdoc: lm, oracleError: o.err || null });
        statFor(stats, c.format).rawDiffs++;
      }
    }
    total += chunk.length;
    process.stderr.write(`${total} enumerated, ${rawDiffs.length} raw diffs\n`);
    chunk = [];
  };

  try {
    outer: for (const group of groups) {
      for (const c of group.cases()) {
        if (emitted >= options.limit) break outer;
        const format = normalizeFormat(c.format);
        if (options.formats && !options.formats.includes(format)) continue;
        chunk.push({ ...c, format });
        statFor(stats, format).cases++;
        emitted++;
        if (chunk.length >= options.chunk) flush();
      }
      flush();
    }
    flush();

    const classes = new Map();
    for (const d of rawDiffs) {
      const k = signature(d);
      if (!classes.has(k)) classes.set(k, []);
      classes.get(k).push(d);
    }
    for (const [k, members] of classes) {
      const fmt = k.split(" ", 1)[0];
      statFor(stats, fmt).classes++;
    }
    process.stderr.write(`classes: ${classes.size}\n`);

    const report = [];
    for (const [k, members] of sortClassEntries(classes)) {
      const samples = [];
      let confirmed = 0;
      let leak = 0;
      for (const r of representatives(members, options.reps)) {
        writeFileSync(inPath, JSON.stringify([caseForWire(r)]));
        const l = runLsdoc(options.lsdoc, inPath, lsPath)[0];
        const o = runOracle(inPath)[0];
        const lm = projection(l.projection);
        const om = projection(o.projection);
        if (projectionKey(l.projection) !== projectionKey(o.projection)) {
          confirmed++;
          samples.push({
            id: r.id,
            format: r.format,
            input: r.input,
            meta: r.meta || {},
            mldoc: om,
            lsdoc: lm,
            oracleError: o.err || null,
          });
        } else {
          leak++;
        }
      }
      if (confirmed > 0) statFor(stats, members[0].format).confirmedClasses++;
      report.push({
        class: k,
        size: members.length,
        confirmed,
        leak,
        representatives: samples.slice(0, options.reps),
      });
      process.stderr.write(`class(${members.length}) confirmed=${confirmed} leak=${leak}: ${k.slice(0, 140)}\n`);
    }

    const findings = report.filter((r) => r.confirmed > 0);
    writeFileSync(findingsPath, JSON.stringify(findings, null, 1));
    const elapsedMs = Date.now() - started;
    for (const [format, s] of [...stats.entries()].sort()) {
      console.log(
        `${name}/${format}: ${s.cases} cases; ${s.rawDiffs} raw diffs in ${s.classes} classes; ${s.confirmedClasses} isolated-CONFIRMED classes`
      );
    }
    console.log(
      `${total} cases; ${rawDiffs.length} raw diffs in ${classes.size} classes; ${findings.length} isolated-CONFIRMED classes -> ${findingsPath} (${(elapsedMs / 1000).toFixed(1)}s)`
    );
    return { total, rawDiffs: rawDiffs.length, classes: classes.size, findings, stats, elapsedMs };
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

export function unique(items) {
  return [...new Set(items)];
}

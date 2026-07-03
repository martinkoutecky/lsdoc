#!/usr/bin/env node
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import {
  accessSync,
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { readdir, readFile, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import {
  basename,
  dirname,
  extname,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { canonJSON } from "../harness/lib/compare.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO = resolve(__dirname, "..");
const HARNESS = join(REPO, "harness");
const RELEASE_BIN = join(REPO, "target", "release", process.platform === "win32" ? "lsdoc-parse.exe" : "lsdoc-parse");
const DEFAULT_TIMEOUT_MS = 10_000;
const MAX_FILE_BYTES = 8 * 1024 * 1024;
const MAX_CHUNK_FILES = 500;
const MAX_CHUNK_BYTES = 32 * 1024 * 1024;
const require = createRequire(import.meta.url);

let tempDir = null;

function usage(exitCode = 0) {
  const out = exitCode === 0 ? console.log : console.error;
  out(`Usage:
  node tools/graph-check.mjs <graph-dir> [--mode bench|diff|both] [--format md|org|auto]
                             [--out report.md] [--journals] [--no-journals]
                             [--jobs N] [--timeout-ms N] [--fast]

  node tools/graph-check.mjs --self-test`);
  process.exit(exitCode);
}

function parseArgs(argv) {
  const opts = {
    mode: "both",
    format: "auto",
    out: "graph-check-report.md",
    journals: true,
    jobs: 1,
    timeoutMs: DEFAULT_TIMEOUT_MS,
    fast: false,
    pathological: true,
    selfTest: false,
  };
  const positional = [];
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") usage(0);
    else if (arg === "--self-test") opts.selfTest = true;
    else if (arg === "--mode") opts.mode = needValue(argv, ++i, arg);
    else if (arg.startsWith("--mode=")) opts.mode = arg.slice("--mode=".length);
    else if (arg === "--format") opts.format = needValue(argv, ++i, arg);
    else if (arg.startsWith("--format=")) opts.format = arg.slice("--format=".length);
    else if (arg === "--out") opts.out = needValue(argv, ++i, arg);
    else if (arg.startsWith("--out=")) opts.out = arg.slice("--out=".length);
    else if (arg === "--journals") opts.journals = true;
    else if (arg === "--no-journals") opts.journals = false;
    else if (arg === "--jobs") opts.jobs = Number(needValue(argv, ++i, arg));
    else if (arg.startsWith("--jobs=")) opts.jobs = Number(arg.slice("--jobs=".length));
    else if (arg === "--timeout-ms") opts.timeoutMs = Number(needValue(argv, ++i, arg));
    else if (arg.startsWith("--timeout-ms=")) opts.timeoutMs = Number(arg.slice("--timeout-ms=".length));
    else if (arg === "--fast") opts.fast = true;
    else if (arg === "--no-pathological") opts.pathological = false;
    else if (arg.startsWith("-")) throw new Error(`unknown flag: ${arg}`);
    else positional.push(arg);
  }

  if (!["bench", "diff", "both"].includes(opts.mode)) throw new Error("--mode must be bench, diff, or both");
  if (!["md", "org", "auto"].includes(opts.format)) throw new Error("--format must be md, org, or auto");
  if (!Number.isInteger(opts.jobs) || opts.jobs < 1) throw new Error("--jobs must be a positive integer");
  if (!Number.isInteger(opts.timeoutMs) || opts.timeoutMs < 100) throw new Error("--timeout-ms must be an integer >= 100");
  if (!opts.selfTest && positional.length !== 1) usage(1);
  opts.graphDir = positional[0] ? resolve(positional[0]) : null;
  opts.out = resolve(process.cwd(), opts.out);
  return opts;
}

function needValue(argv, index, flag) {
  if (index >= argv.length) throw new Error(`${flag} needs a value`);
  return argv[index];
}

function ensureTempDir() {
  if (!tempDir) {
    tempDir = mkdtempSync(join(tmpdir(), "lsdoc-graph-check-"));
    chmodSync(tempDir, 0o700);
  }
  return tempDir;
}

function cleanup() {
  if (tempDir) {
    rmSync(tempDir, { recursive: true, force: true });
    tempDir = null;
  }
}

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.once(sig, () => {
    cleanup();
    process.kill(process.pid, sig);
  });
}
process.once("exit", cleanup);

function runProcess(cmd, args, { cwd = REPO, env = {}, input = null, timeoutMs = DEFAULT_TIMEOUT_MS, inherit = false } = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, {
      cwd,
      env: { ...process.env, ...env },
      stdio: inherit ? "inherit" : ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    const timer = setTimeout(() => {
      settled = true;
      child.kill("SIGKILL");
      resolve({ ok: false, timeout: true, status: null, signal: "SIGKILL", stdout, stderr });
    }, timeoutMs);

    if (!inherit) {
      child.stdout.on("data", (d) => {
        if (stdout.length < 2_000_000) stdout += d.toString("utf8");
      });
      child.stderr.on("data", (d) => {
        if (stderr.length < 2_000_000) stderr += d.toString("utf8");
      });
      if (input != null) child.stdin.end(input);
      else child.stdin.end();
    }

    child.on("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ ok: false, error, status: null, signal: null, stdout, stderr });
    });
    child.on("close", (status, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ ok: status === 0, timeout: false, status, signal, stdout, stderr });
    });
  });
}

async function ensureReleaseBinary() {
  const src = join(REPO, "src", "bin", "lsdoc-parse.rs");
  let needsBuild = false;
  try {
    accessSync(RELEASE_BIN);
    needsBuild = statSync(src).mtimeMs > statSync(RELEASE_BIN).mtimeMs;
  } catch {
    needsBuild = true;
  }
  if (!needsBuild) return;
  if (!existsSync(RELEASE_BIN)) {
    console.error("lsdoc release binary missing; building with `source scripts/env.sh && cargo build --release --bin lsdoc-parse`");
  } else {
    console.error("lsdoc release binary is older than src/bin/lsdoc-parse.rs; rebuilding");
  }
  const result = await runProcess("bash", ["-lc", "source scripts/env.sh && cargo build --release --bin lsdoc-parse"], {
    cwd: REPO,
    timeoutMs: 10 * 60_000,
    inherit: true,
  });
  if (!result.ok) throw new Error("cargo build --release --bin lsdoc-parse failed");
}

async function scanGraph(graphDir, opts) {
  const roots = ["pages"];
  if (opts.journals) roots.push("journals");
  const files = [];
  const skipped = [];
  for (const root of roots) {
    const absRoot = join(graphDir, root);
    if (!existsSync(absRoot)) continue;
    await walk(absRoot, async (abs) => {
      const ext = extname(abs).toLowerCase();
      if (opts.format === "md" && ext !== ".md") return;
      if (opts.format === "org" && ext !== ".org") return;
      if (opts.format === "auto" && ext !== ".md" && ext !== ".org") return;
      const st = await stat(abs);
      const rel = slash(relative(graphDir, abs));
      if (st.size > MAX_FILE_BYTES) {
        skipped.push({ rel, bytes: st.size, reason: "larger than 8 MB" });
        return;
      }
      files.push({
        id: `f${files.length}`,
        abs,
        rel,
        bytes: st.size,
        format: ext === ".org" ? "org" : "md",
      });
    });
  }
  files.sort((a, b) => a.rel.localeCompare(b.rel));
  files.forEach((f, i) => { f.id = `f${i}`; });
  return { files, skipped };
}

async function walk(dir, onFile) {
  const entries = await readdir(dir, { withFileTypes: true });
  entries.sort((a, b) => a.name.localeCompare(b.name));
  for (const ent of entries) {
    const abs = join(dir, ent.name);
    if (ent.isDirectory()) await walk(abs, onFile);
    else if (ent.isFile()) await onFile(abs);
  }
}

function slash(path) {
  return path.split(sep).join("/");
}

function chunkFiles(files) {
  const chunks = [];
  let cur = [];
  let curBytes = 0;
  for (const file of files) {
    if (cur.length && (cur.length >= MAX_CHUNK_FILES || curBytes + file.bytes > MAX_CHUNK_BYTES)) {
      chunks.push(cur);
      cur = [];
      curBytes = 0;
    }
    cur.push(file);
    curBytes += file.bytes;
  }
  if (cur.length) chunks.push(cur);
  return chunks;
}

async function readUtf8(file) {
  return (await readFile(file.abs)).toString("utf8");
}

async function runLsdocFiles(files, opts) {
  const results = new Map();
  const chunks = chunkFiles(files);
  for (let i = 0; i < chunks.length; i++) {
    const chunk = chunks[i];
    const res = await runLsdocChunk(chunk, opts, { allowFallback: true });
    for (const [id, item] of res) results.set(id, item);
    console.error(`lsdoc batch ${i + 1}/${chunks.length}: ${chunk.length} files`);
  }
  return results;
}

async function runLsdocChunk(files, opts, { allowFallback }) {
  const dir = ensureTempDir();
  const corpusPath = join(dir, `lsdoc-corpus-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.json`);
  const outPath = join(dir, `lsdoc-out-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.json`);
  const corpus = [];
  for (const file of files) {
    corpus.push({ id: file.id, input: await readUtf8(file), format: file.format });
  }
  writeFileSync(corpusPath, JSON.stringify(corpus));
  const timeoutMs = opts.timeoutMs + 1_000;
  const proc = await runProcess(RELEASE_BIN, ["--timings-no-input", corpusPath, outPath], { timeoutMs });
  rmSync(corpusPath, { force: true });
  if (!proc.ok) {
    rmSync(outPath, { force: true });
    if (allowFallback && files.length > 1) {
      const out = new Map();
      for (const file of files) {
        const single = await runLsdocChunk([file], opts, { allowFallback: false });
        for (const [id, item] of single) out.set(id, item);
      }
      return out;
    }
    return new Map(files.map((f) => [f.id, {
      ok: false,
      parser: "lsdoc",
      status: proc.timeout ? "timeout" : "crash",
      detail: proc.timeout ? `timeout after ${opts.timeoutMs}ms` : exitDetail(proc),
    }]));
  }
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(outPath, "utf8"));
  } catch {
    rmSync(outPath, { force: true });
    return new Map(files.map((f) => [f.id, {
      ok: false,
      parser: "lsdoc",
      status: "bad-output",
      detail: "lsdoc output was not valid JSON",
    }]));
  }
  rmSync(outPath, { force: true });
  const out = new Map();
  for (const item of parsed) {
    delete item.input;
    out.set(item.id, {
      ok: true,
      projection: item.projection,
      parseMicros: Number(item.parse_micros ?? 0),
      overTimeout: Number(item.parse_micros ?? 0) / 1000 > opts.timeoutMs,
    });
  }
  for (const file of files) {
    if (!out.has(file.id)) {
      out.set(file.id, {
        ok: false,
        parser: "lsdoc",
        status: "missing-output",
        detail: "lsdoc did not emit a result for this file",
      });
    }
  }
  return out;
}

async function runLsdocInput(input, format, opts) {
  const file = {
    id: "probe",
    abs: join(ensureTempDir(), `probe-${Date.now()}-${Math.random().toString(16).slice(2)}.${format}`),
    rel: "probe",
    bytes: Buffer.byteLength(input),
    format,
  };
  writeFileSync(file.abs, input);
  const res = await runLsdocChunk([file], opts, { allowFallback: false });
  rmSync(file.abs, { force: true });
  return res.get(file.id);
}

function exitDetail(proc) {
  if (proc.signal) return `terminated by signal ${proc.signal}`;
  if (proc.status != null) return `exit ${proc.status}`;
  return "process failed";
}

function writeMldocWorkerScript() {
  const script = join(ensureTempDir(), "mldoc-worker.mjs");
  if (existsSync(script)) return script;
  writeFileSync(script, `import { createInterface } from "node:readline";
import { createRequire } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const repo = process.env.LSDOC_REPO;
const require = createRequire(pathToFileURL(join(repo, "harness", "oracle.mjs")).href);
const { Mldoc } = require("mldoc");
const { normalizeAst } = await import(pathToFileURL(join(repo, "harness", "lib", "normalize.mjs")).href);
const { extractRefs } = await import(pathToFileURL(join(repo, "harness", "lib", "refs.mjs")).href);

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

function parseToProjection(input, format = "md") {
  const ast = JSON.parse(Mldoc.parseJson(input, cfg(format)));
  return { blocks: normalizeAst(ast), refs: extractRefs(ast) };
}

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of rl) {
  if (!line) continue;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    process.stdout.write(JSON.stringify({ ok: false, status: "bad-request" }) + "\\n");
    continue;
  }
  const start = process.hrtime.bigint();
  try {
    const projection = parseToProjection(msg.input, msg.format || "md");
    const parse_micros = Number((process.hrtime.bigint() - start) / 1000n);
    process.stdout.write(JSON.stringify({ id: msg.id, ok: true, projection, parse_micros }) + "\\n");
  } catch {
    process.stdout.write(JSON.stringify({ id: msg.id, ok: false, status: "parse-error" }) + "\\n");
  }
}`);
  chmodSync(script, 0o700);
  return script;
}

class MldocWorker {
  constructor(opts) {
    this.opts = opts;
    this.child = null;
    this.buf = "";
    this.current = null;
  }

  start() {
    const script = writeMldocWorkerScript();
    this.child = spawn(process.execPath, [script], {
      cwd: REPO,
      env: { ...process.env, LSDOC_REPO: REPO },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stdout.on("data", (d) => this.onStdout(d));
    this.child.stderr.on("data", () => {});
    this.child.on("close", (status, signal) => {
      if (this.current) {
        const cur = this.current;
        this.current = null;
        clearTimeout(cur.timer);
        cur.resolve({
          ok: false,
          parser: "mldoc",
          status: "crash",
          detail: signal ? `terminated by signal ${signal}` : `exit ${status}`,
        });
      }
    });
  }

  onStdout(data) {
    this.buf += data.toString("utf8");
    for (;;) {
      const idx = this.buf.indexOf("\n");
      if (idx === -1) break;
      const line = this.buf.slice(0, idx);
      this.buf = this.buf.slice(idx + 1);
      if (!this.current) continue;
      const cur = this.current;
      this.current = null;
      clearTimeout(cur.timer);
      try {
        const msg = JSON.parse(line);
        if (msg.ok) {
          cur.resolve({ ok: true, projection: msg.projection, parseMicros: Number(msg.parse_micros ?? 0) });
        } else {
          cur.resolve({ ok: false, parser: "mldoc", status: msg.status || "parse-error", detail: "mldoc parse failed" });
        }
      } catch {
        cur.resolve({ ok: false, parser: "mldoc", status: "bad-output", detail: "mldoc worker emitted invalid JSON" });
      }
    }
  }

  async parse(input, format, timeoutMs = this.opts.timeoutMs) {
    if (!this.child || this.child.killed || this.child.exitCode != null) this.start();
    return await new Promise((resolve) => {
      const timer = setTimeout(() => {
        if (this.current) this.current = null;
        this.child.kill("SIGKILL");
        resolve({ ok: false, parser: "mldoc", status: "timeout", detail: `timeout after ${timeoutMs}ms` });
      }, timeoutMs);
      this.current = { resolve, timer };
      this.child.stdin.write(JSON.stringify({ id: "probe", input, format }) + "\n");
    });
  }

  close() {
    if (!this.child) return;
    this.child.stdin.end();
    setTimeout(() => {
      if (this.child && this.child.exitCode == null) this.child.kill("SIGKILL");
    }, 50).unref();
    this.child = null;
  }
}

async function runMldocFresh(input, format, opts) {
  const worker = new MldocWorker(opts);
  const res = await worker.parse(input, format, opts.timeoutMs);
  worker.close();
  return res;
}

async function parseBothFresh(input, format, opts) {
  const [lsdoc, mldoc] = await Promise.all([
    runLsdocInput(input, format, opts),
    runMldocFresh(input, format, opts),
  ]);
  if (!lsdoc?.ok || !mldoc?.ok) return { ok: false, lsdoc, mldoc };
  const lsdocCanon = projectionKey(lsdoc.projection);
  const mldocCanon = projectionKey(mldoc.projection);
  return {
    ok: true,
    diverges: lsdocCanon !== mldocCanon,
    lsdoc,
    mldoc,
    lsdocCanon,
    mldocCanon,
  };
}

function projectionKey(projection) {
  return canonJSON({ blocks: projection?.blocks || projection || [], refs: projection?.refs || { page: [], block: [] } });
}

async function runDiff(files, lsdocResults, opts) {
  const findings = [];
  if (opts.fast) {
    console.error("WARNING: --fast uses a warm mldoc process. It can both invent and mask divergences; findings and absences are non-authoritative until re-verified.");
  } else {
    const eta = (files.length * opts.timeoutMs) / Math.max(1, opts.jobs) / 1000;
    console.error(`mldoc isolated scan: ${files.length} files, one fresh subprocess per file, worst-case ETA about ${formatDuration(eta * 1000)}`);
  }

  let done = 0;
  const start = Date.now();
  const warmWorkers = opts.fast ? Array.from({ length: opts.jobs }, () => new MldocWorker(opts)) : [];
  await mapLimit(files, opts.jobs, async (file, idx, workerIndex) => {
    const lsdoc = lsdocResults.get(file.id);
    if (!lsdoc?.ok) {
      findings.push({ type: "lsdoc-failure", file, status: lsdoc?.status || "failed", detail: lsdoc?.detail || "lsdoc failed" });
      progress();
      return;
    }
    if (lsdoc.overTimeout) {
      findings.push({ type: "lsdoc-timeout", file, status: "timeout", detail: `lsdoc parse_micros exceeded ${opts.timeoutMs}ms` });
    }
    const input = await readUtf8(file);
    const mldoc = opts.fast
      ? await warmWorkers[workerIndex].parse(input, file.format, opts.timeoutMs)
      : await runMldocFresh(input, file.format, opts);
    if (!mldoc.ok) {
      findings.push({ type: "mldoc-failure", file, status: mldoc.status, detail: mldoc.detail, lsdocMs: microsToMs(lsdoc.parseMicros) });
      progress();
      return;
    }
    const lKey = projectionKey(lsdoc.projection);
    const mKey = projectionKey(mldoc.projection);
    if (lKey !== mKey) {
      const original = await parseBothFresh(input, file.format, opts);
      if (original.ok && original.diverges) {
        const minimized = await minimize(file, Buffer.from(input, "utf8"), file.format, opts);
        const anon = await anonymizeAndVerify(minimized.input, file.format, opts);
        findings.push({
          type: "divergence",
          file,
          lineStart: minimized.lineStart,
          lineEnd: minimized.lineEnd,
          contextDependent: minimized.contextDependent,
          originalBytes: minimized.inputBytes,
          anonymized: anon,
        });
      } else {
        findings.push({ type: "unstable-divergence", file, status: "not-reverified", detail: "scan mismatch did not reproduce in fresh parser processes" });
      }
    }
    progress();

    function progress() {
      done++;
      const elapsed = Date.now() - start;
      const eta = done ? (elapsed / done) * (files.length - done) : 0;
      if (done === files.length || done <= 5 || done % 10 === 0) {
        console.error(`diff scan ${done}/${files.length}, eta ${formatDuration(eta)}: ${file.rel}`);
      }
    }
  });
  for (const w of warmWorkers) w.close();
  findings.sort((a, b) => a.file.rel.localeCompare(b.file.rel));
  return findings;
}

async function mapLimit(items, limit, fn) {
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length || 1) }, async (_, workerIndex) => {
    while (next < items.length) {
      const idx = next++;
      await fn(items[idx], idx, workerIndex);
    }
  });
  await Promise.all(workers);
}

async function minimize(file, buffer, format, opts) {
  const whole = await parseBothFresh(buffer.toString("utf8"), format, opts);
  if (!whole.ok || !whole.diverges) {
    return {
      input: buffer.toString("utf8"),
      inputBytes: buffer.length,
      lineStart: 1,
      lineEnd: lineNumberForOffset(buffer, buffer.length),
      contextDependent: true,
    };
  }

  const ranges = chunkRanges(buffer, format);
  const candidate = await findDivergentRange(buffer, ranges, format, opts);
  const chosen = candidate || { start: 0, end: buffer.length, contextDependent: true };
  const snippet = buffer.slice(chosen.start, chosen.end).toString("utf8");
  return {
    input: snippet,
    inputBytes: Buffer.byteLength(snippet),
    lineStart: lineNumberForOffset(buffer, chosen.start),
    lineEnd: lineNumberForOffset(buffer, Math.max(chosen.start, chosen.end - 1)),
    contextDependent: Boolean(chosen.contextDependent),
  };
}

async function findDivergentRange(buffer, ranges, format, opts) {
  if (ranges.length <= 1) return null;
  let lo = 0;
  let hi = ranges.length;
  while (hi - lo > 1) {
    const mid = lo + Math.floor((hi - lo) / 2);
    const left = rangeFromChunks(ranges, lo, mid);
    if (await rangeDiverges(buffer, left, format, opts)) {
      hi = mid;
      continue;
    }
    const right = rangeFromChunks(ranges, mid, hi);
    if (await rangeDiverges(buffer, right, format, opts)) {
      lo = mid;
      continue;
    }
    break;
  }
  const scoped = ranges.slice(lo, hi);
  const base = lo;
  const singles = scoped
    .map((r, i) => ({ ...r, i: base + i }))
    .sort((a, b) => (a.end - a.start) - (b.end - b.start));
  for (const r of singles) {
    if (await rangeDiverges(buffer, r, format, opts)) return r;
  }
  const maxTests = 2_000;
  let tests = 0;
  for (let len = 2; len <= scoped.length; len++) {
    for (let start = 0; start + len <= scoped.length; start++) {
      if (++tests > maxTests) return null;
      const r = rangeFromChunks(ranges, base + start, base + start + len);
      if (await rangeDiverges(buffer, r, format, opts)) return r;
    }
  }
  return null;
}

function rangeFromChunks(ranges, lo, hi) {
  return { start: ranges[lo].start, end: ranges[hi - 1].end };
}

async function rangeDiverges(buffer, range, format, opts) {
  if (range.end <= range.start) return false;
  const parsed = await parseBothFresh(buffer.slice(range.start, range.end).toString("utf8"), format, opts);
  return parsed.ok && parsed.diverges;
}

export function chunkRanges(buffer, format) {
  if (buffer.length === 0) return [{ start: 0, end: 0 }];
  const lines = splitLineRanges(buffer);
  const boundaries = new Set([0, buffer.length]);
  for (let i = 0; i < lines.length; i++) {
    const line = buffer.slice(lines[i].start, lines[i].contentEnd).toString("utf8");
    if (i > 0 && isBoundaryLine(line, format)) boundaries.add(lines[i].start);
    if (i > 0 && isBlankLine(buffer.slice(lines[i - 1].start, lines[i - 1].contentEnd))) boundaries.add(lines[i].start);
  }
  const sorted = [...boundaries].sort((a, b) => a - b);
  const out = [];
  for (let i = 0; i + 1 < sorted.length; i++) {
    if (sorted[i] !== sorted[i + 1]) out.push({ start: sorted[i], end: sorted[i + 1] });
  }
  return out.length ? out : [{ start: 0, end: buffer.length }];
}

function splitLineRanges(buffer) {
  const lines = [];
  let start = 0;
  for (let i = 0; i < buffer.length; i++) {
    if (buffer[i] === 0x0a) {
      const contentEnd = i > start && buffer[i - 1] === 0x0d ? i - 1 : i;
      lines.push({ start, contentEnd, end: i + 1 });
      start = i + 1;
    } else if (buffer[i] === 0x0d) {
      lines.push({ start, contentEnd: i, end: i + 1 });
      start = i + 1;
    }
  }
  if (start < buffer.length) lines.push({ start, contentEnd: buffer.length, end: buffer.length });
  return lines;
}

function isBoundaryLine(line, format) {
  if (format === "org" && /^\*+\s/.test(line)) return true;
  return /^([-*+]\s|\d+\.\s)/.test(line);
}

function isBlankLine(buf) {
  for (const b of buf) {
    if (b !== 0x20 && b !== 0x09 && b !== 0x0c) return false;
  }
  return true;
}

function lineNumberForOffset(buffer, offset) {
  let line = 1;
  const end = Math.min(offset, buffer.length);
  for (let i = 0; i < end; i++) {
    if (buffer[i] === 0x0a || buffer[i] === 0x0d) {
      line++;
      if (buffer[i] === 0x0d && buffer[i + 1] === 0x0a && i + 1 < end) i++;
    }
  }
  return line;
}

async function anonymizeAndVerify(input, format, opts) {
  const attempts = [
    ["tier 1", () => anonymizeTier1(input, [])],
    ["tier 2", () => anonymizeTier2(input, [])],
    ["tier 1 + protected keywords", () => anonymizeTier1(input, protectedSpans(input))],
    ["tier 2 + protected keywords", () => anonymizeTier2(input, protectedSpans(input))],
  ];
  for (const [tier, make] of attempts) {
    const candidate = make();
    const parsed = await parseBothFresh(candidate, format, opts);
    if (parsed.ok && parsed.diverges) {
      return {
        ok: true,
        tier,
        input: candidate,
        visible: JSON.stringify(candidate),
        lsdocProjection: parsed.lsdoc.projection,
        mldocProjection: parsed.mldoc.projection,
      };
    }
  }
  return { ok: false };
}

function anonymizeTier1(input, protectedRanges = []) {
  return transformCodepoints(input, protectedRanges, (ch) => {
    const cp = ch.codePointAt(0);
    if (cp >= 0x41 && cp <= 0x5a) return "A";
    if (cp >= 0x61 && cp <= 0x7a) return "a";
    if (cp >= 0x30 && cp <= 0x39) return "9";
    if (cp > 0x7f) return replacementForUtf8Len(Buffer.byteLength(ch, "utf8"));
    return ch;
  });
}

function anonymizeTier2(input, protectedRanges = []) {
  return transformCodepoints(input, protectedRanges, (ch) => {
    const cp = ch.codePointAt(0);
    if (cp >= 0x41 && cp <= 0x5a) return String.fromCharCode(((cp - 0x41 + 1) % 26) + 0x41);
    if (cp >= 0x61 && cp <= 0x7a) return String.fromCharCode(((cp - 0x61 + 1) % 26) + 0x61);
    return ch;
  });
}

function transformCodepoints(input, protectedRanges, fn) {
  let out = "";
  for (let i = 0; i < input.length;) {
    const cp = input.codePointAt(i);
    const ch = String.fromCodePoint(cp);
    const next = i + ch.length;
    out += inProtectedRange(i, protectedRanges) ? ch : fn(ch);
    i = next;
  }
  return out;
}

function inProtectedRange(index, ranges) {
  return ranges.some((r) => index >= r.start && index < r.end);
}

function replacementForUtf8Len(len) {
  if (len === 2) return "ä";
  if (len === 3) return "中";
  if (len === 4) return "😀";
  return "中";
}

function protectedSpans(input) {
  const ranges = [];
  const patterns = [
    /https?:\/\/[^\s<>"'`)\]]+/gi,
    /#\+(?:BEGIN|END)_[A-Z0-9_+-]+/gi,
    /#\+[A-Z0-9_+-]+:/gi,
    /:PROPERTIES:|:END:/gi,
    /\b(?:TODO|DOING|DONE|NOW|LATER|WAITING|WAIT|CANCELED|CANCELLED|SCHEDULED:|DEADLINE:|CLOSED:)\b/gi,
    /\b(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday)\b/gi,
    /\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec|January|February|March|April|June|July|August|September|October|November|December)\b/gi,
  ];
  for (const re of patterns) {
    for (const m of input.matchAll(re)) ranges.push({ start: m.index, end: m.index + m[0].length });
  }
  ranges.sort((a, b) => a.start - b.start || a.end - b.end);
  return ranges;
}

async function runBench(files, opts) {
  const lsdocRuns = [];
  const mldocRuns = [];
  for (let i = 0; i < 3; i++) {
    console.error(`bench lsdoc run ${i + 1}/3`);
    lsdocRuns.push(await benchLsdoc(files, opts));
  }
  for (let i = 0; i < 3; i++) {
    console.error(`bench mldoc run ${i + 1}/3`);
    mldocRuns.push(await benchMldoc(files, opts));
  }
  return {
    lsdoc: summarizeBenchRuns(lsdocRuns),
    mldoc: summarizeBenchRuns(mldocRuns),
    pathological: opts.pathological ? await runPathologicalAppendix(opts) : [],
  };
}

async function benchLsdoc(files, opts) {
  const results = await runLsdocFiles(files, opts);
  return benchFromResults(files, results);
}

async function benchMldoc(files, opts) {
  const worker = new MldocWorker(opts);
  const results = new Map();
  for (const file of files) {
    const input = await readUtf8(file);
    const res = await worker.parse(input, file.format, opts.timeoutMs);
    results.set(file.id, res);
    if (!res.ok) {
      worker.close();
    }
  }
  worker.close();
  return benchFromResults(files, results);
}

function benchFromResults(files, results) {
  const samples = [];
  const failures = [];
  for (const file of files) {
    const res = results.get(file.id);
    if (res?.ok && !res.overTimeout) {
      samples.push({ rel: file.rel, micros: Number(res.parseMicros || 0) });
    } else {
      failures.push({
        rel: file.rel,
        status: res?.status || (res?.overTimeout ? "timeout" : "failed"),
        detail: res?.detail || (res?.overTimeout ? "parse time exceeded timeout" : "parser failed"),
      });
    }
  }
  return { samples, failures, totalMicros: sum(samples.map((s) => s.micros)) };
}

function summarizeBenchRuns(runs) {
  const best = runs.slice().sort((a, b) => a.totalMicros - b.totalMicros)[0] || { samples: [], failures: [], totalMicros: 0 };
  const values = best.samples.map((s) => s.micros).sort((a, b) => a - b);
  const slowest = best.samples.slice().sort((a, b) => b.micros - a.micros).slice(0, 5);
  return {
    totalMs: microsToMs(best.totalMicros),
    fileCount: best.samples.length,
    p50Ms: microsToMs(percentile(values, 0.50)),
    p95Ms: microsToMs(percentile(values, 0.95)),
    maxMs: microsToMs(values[values.length - 1] || 0),
    slowest: slowest.map((s) => ({ rel: s.rel, ms: microsToMs(s.micros) })),
    failures: best.failures,
  };
}

function percentile(sorted, p) {
  if (!sorted.length) return 0;
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1))];
}

function sum(values) {
  return values.reduce((a, b) => a + b, 0);
}

async function runPathologicalAppendix(opts) {
  const cases = [
    { name: "org construct-in-> quote nesting", format: "org", sizes: [64, 256, 512], make: makeConstructGtQuoteNesting },
    { name: "org sequential raw-html quote frames", format: "org", sizes: [32, 128, 512], make: (n) => Array.from({ length: n }, () => "#+BEGIN_QUOTE\n<div>x</div>\n#+END_QUOTE\n").join("") },
    { name: "org deep #+BEGIN nesting", format: "org", sizes: [32, 128, 512], make: (n) => `${Array.from({ length: n }, (_, i) => `#+BEGIN_A${i}\n`).join("")}x\n${Array.from({ length: n }, (_, i) => `#+END_A${n - i - 1}\n`).join("")}` },
  ];
  const rows = [];
  for (const c of cases) {
    for (const n of c.sizes) {
      const input = c.make(n);
      const [lsdoc, mldoc] = await Promise.all([
        runLsdocInput(input, c.format, opts),
        runMldocFresh(input, c.format, opts),
      ]);
      rows.push({
        name: c.name,
        size: n,
        bytes: Buffer.byteLength(input),
        lsdoc: lsdoc.ok ? `${microsToMs(lsdoc.parseMicros).toFixed(3)} ms` : lsdoc.status,
        mldoc: mldoc.ok ? `${microsToMs(mldoc.parseMicros).toFixed(3)} ms` : mldoc.status,
      });
    }
  }
  return rows;
}

function makeConstructGtQuoteNesting(n) {
  const lines = [];
  for (let i = 1; i <= n; i++) lines.push(`${">".repeat(i)} #+BEGIN_QUOTE`);
  lines.push(`${">".repeat(n + 1)} x`);
  for (let i = n; i >= 1; i--) lines.push(`${">".repeat(i)} #+END_QUOTE`);
  return `${lines.join("\n")}\n`;
}

function graphStats(files, skipped) {
  const bytes = sum(files.map((f) => f.bytes));
  const largest = files.slice().sort((a, b) => b.bytes - a.bytes)[0];
  return { files: files.length, totalBytes: bytes, largest, skipped };
}

async function versions() {
  let lsdoc = "unknown";
  const git = await runProcess("git", ["describe", "--always", "--dirty"], { cwd: REPO, timeoutMs: 2_000 });
  if (git.ok) lsdoc = git.stdout.trim();
  let mldoc = "unknown";
  try {
    mldoc = JSON.parse(readFileSync(join(HARNESS, "node_modules", "mldoc", "package.json"), "utf8")).version || "unknown";
  } catch {}
  return { lsdoc, mldoc };
}

function renderReport({ graphDir, opts, stats, versions, bench, findings, zeroFiles }) {
  const lines = [];
  lines.push("# lsdoc graph check report", "");
  lines.push(`Generated: ${new Date().toISOString()}`);
  lines.push(`Graph: \`${graphDir || "(none)"}\``);
  lines.push(`Mode: \`${opts.mode}\`, format: \`${opts.format}\`, journals: \`${opts.journals ? "on" : "off"}\`, jobs: \`${opts.jobs}\`, timeout: \`${opts.timeoutMs}ms\``);
  if (opts.fast) lines.push("WARNING: `--fast` was used. The mldoc scan used a warm process and can both invent and mask divergences; findings and absences are non-authoritative except for entries re-verified below.");
  lines.push(`lsdoc version: \`${versions.lsdoc}\``);
  lines.push(`mldoc npm version: \`${versions.mldoc}\``, "");
  lines.push("Privacy: nothing was uploaded. Temporary parser inputs were kept in a fresh mode-0700 temp directory and removed on exit. This report is the only persistent output; snippets below are anonymized and re-verified, or omitted.", "");

  if (zeroFiles) {
    lines.push("## Graph stats", "");
    lines.push("0 files matched. No statistics were computed.", "");
    return lines.join("\n");
  }

  lines.push("## Graph stats", "");
  lines.push(`- Matched files: ${stats.files}`);
  lines.push(`- Total bytes: ${stats.totalBytes}`);
  lines.push(`- Largest file: ${stats.largest ? `\`${stats.largest.rel}\` (${stats.largest.bytes} bytes)` : "n/a"}`);
  lines.push(`- Skipped files over 8 MB: ${stats.skipped.length}`);
  for (const s of stats.skipped) lines.push(`  - \`${s.rel}\` (${s.bytes} bytes): ${s.reason}`);
  lines.push("");

  if (bench) renderBench(lines, bench);
  if (findings) renderFindings(lines, findings);
  return lines.join("\n");
}

function renderBench(lines, bench) {
  lines.push("## Bench", "");
  lines.push("Fairness notes: mldoc here is the npm js_of_ocaml build used by Logseq/Electron, so it is the real-world shipped comparison, but it is not native OCaml. Each side ran 3 times; totals below are best-of-3 parse time sums, excluding crashed/timed-out files. Per-file values are from parser-reported in-process parse timings.", "");
  for (const [name, b] of [["lsdoc", bench.lsdoc], ["mldoc", bench.mldoc]]) {
    lines.push(`### ${name}`, "");
    lines.push(`- Parsed files in aggregate: ${b.fileCount}`);
    lines.push(`- Best total: ${b.totalMs.toFixed(3)} ms`);
    lines.push(`- p50 / p95 / max: ${b.p50Ms.toFixed(3)} / ${b.p95Ms.toFixed(3)} / ${b.maxMs.toFixed(3)} ms`);
    lines.push("- 5 slowest files:");
    if (b.slowest.length) for (const s of b.slowest) lines.push(`  - \`${s.rel}\` (${s.ms.toFixed(3)} ms)`);
    else lines.push("  - none");
    if (b.failures.length) {
      lines.push("- Excluded crash/timeout files:");
      for (const f of b.failures) lines.push(`  - \`${f.rel}\`: ${f.status}`);
    }
    lines.push("");
  }
  if (bench.pathological?.length) {
    lines.push("### Pathological synthetic appendix", "");
    lines.push("Static generated inputs; no graph content is used. mldoc is subprocess-guarded with the same timeout, so crashes/timeouts do not stop the report.", "");
    lines.push("| Case | Size | Bytes | lsdoc | mldoc |");
    lines.push("|---|---:|---:|---:|---:|");
    for (const r of bench.pathological) lines.push(`| ${r.name} | ${r.size} | ${r.bytes} | ${r.lsdoc} | ${r.mldoc} |`);
    lines.push("");
  }
}

function renderFindings(lines, findings) {
  lines.push("## Diff findings", "");
  if (!findings.length) {
    lines.push("No divergences, crashes, or timeouts found.", "");
    return;
  }
  lines.push(`${findings.length} finding(s). File paths are relative to the graph root.`, "");
  findings.forEach((f, i) => {
    lines.push(`### Finding ${i + 1}: ${f.type}`, "");
    lines.push(`File: \`${f.file.rel}\``);
    if (f.lineStart) lines.push(`Local range: lines ${f.lineStart}-${f.lineEnd}${f.contextDependent ? " (context-dependent whole-page fallback)" : ""}`);
    if (f.type === "mldoc-failure") {
      lines.push(`mldoc crashed/timed out (${f.status}); lsdoc parsed in ${Number(f.lsdocMs || 0).toFixed(3)} ms.`);
      lines.push("");
      return;
    }
    if (f.type !== "divergence") {
      lines.push(`${f.status || "failed"}: ${f.detail || ""}`, "");
      return;
    }
    if (!f.anonymized.ok) {
      lines.push("Divergence found but not auto-anonymizable; please extract manually. No page content is included.");
      lines.push("");
      return;
    }
    lines.push(`Snippet status: fresh reproducible divergence derived from your page via ${f.anonymized.tier}. This is the anonymized input's own parser output, not the original page projection.`);
    lines.push("");
    lines.push("Anonymized snippet:");
    lines.push(fenced(f.anonymized.input));
    lines.push("");
    lines.push(`Visible JSON string: \`${escapeBackticks(f.anonymized.visible)}\``);
    lines.push("");
    lines.push(`mldoc projection: \`${escapeBackticks(truncate(projectionKey(f.anonymized.mldocProjection), 400))}\``);
    lines.push(`lsdoc projection: \`${escapeBackticks(truncate(projectionKey(f.anonymized.lsdocProjection), 400))}\``);
    lines.push("");
    lines.push("Post this anonymized, re-verified snippet to https://github.com/martinkoutecky/lsdoc/issues");
    lines.push("");
  });
}

function fenced(text) {
  let fence = "```";
  while (text.includes(fence)) fence += "`";
  return `${fence}\n${text}\n${fence}`;
}

function escapeBackticks(s) {
  return String(s).replaceAll("`", "\\`");
}

function truncate(s, max) {
  return s.length <= max ? s : `${s.slice(0, max)}...`;
}

function microsToMs(micros) {
  return Number(micros || 0) / 1000;
}

function formatDuration(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "0s";
  const s = Math.ceil(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${String(s % 60).padStart(2, "0")}s`;
}

function assert(cond, msg) {
  if (!cond) throw new Error(msg);
}

async function selfTest() {
  const original = "Ab9é中😀!\n";
  const t1 = anonymizeTier1(original);
  assert(Buffer.byteLength(t1) === Buffer.byteLength(original), "tier1 byte length changed");
  assert(t1.startsWith("Aa9"), "tier1 did not preserve case/digit classes");
  const chars = [...t1];
  assert(Buffer.byteLength(chars[3]) === 2 && Buffer.byteLength(chars[4]) === 3 && Buffer.byteLength(chars[5]) === 4, "tier1 UTF-8 class changed");

  const t2 = anonymizeTier2("Azaz09");
  assert(t2 === "Baba09", "tier2 rotation/digit preservation failed");

  const fallback = await anonymizeWithFakeVerifier("Ab1", async (candidate) => candidate === "Bc1");
  assert(fallback.ok && fallback.tier === "tier 2", "tier2 fallback was not selected");
  const rejected = await anonymizeWithFakeVerifier("Ab1", async () => false);
  assert(!rejected.ok, "reverify rejection path failed");

  const crlf = Buffer.from("- one\r\nbody\r\n\r\n- two\r\n", "utf8");
  const ranges = chunkRanges(crlf, "md");
  assert(Buffer.concat(ranges.map((r) => crlf.slice(r.start, r.end))).equals(crlf), "chunker did not round-trip CRLF bytes");
  assert(crlf.slice(ranges[0].start, ranges[0].end).includes(Buffer.from("\r\n")), "first chunk lost CRLF");
  console.log("graph-check self-test: ok");
}

async function anonymizeWithFakeVerifier(input, verifier) {
  const attempts = [
    ["tier 1", () => anonymizeTier1(input, [])],
    ["tier 2", () => anonymizeTier2(input, [])],
    ["tier 1 + protected keywords", () => anonymizeTier1(input, protectedSpans(input))],
    ["tier 2 + protected keywords", () => anonymizeTier2(input, protectedSpans(input))],
  ];
  for (const [tier, make] of attempts) {
    const candidate = make();
    if (await verifier(candidate)) return { ok: true, tier, input: candidate };
  }
  return { ok: false };
}

async function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (e) {
    console.error(e.message);
    usage(1);
  }
  if (opts.selfTest) {
    await selfTest();
    return;
  }

  const graphDir = opts.graphDir;
  const st = statSync(graphDir);
  if (!st.isDirectory()) throw new Error(`${graphDir} is not a directory`);
  ensureTempDir();
  await ensureReleaseBinary();

  const { files, skipped } = await scanGraph(graphDir, opts);
  const stats = graphStats(files, skipped);
  const vers = await versions();
  if (files.length === 0) {
    const report = renderReport({ graphDir, opts, stats, versions: vers, zeroFiles: true });
    writeFileSync(opts.out, report);
    console.error(`0 files matched; wrote ${opts.out}`);
    return;
  }

  let bench = null;
  let findings = null;
  let lsdocResults = null;
  if (opts.mode === "bench" || opts.mode === "both") bench = await runBench(files, opts);
  if (opts.mode === "diff" || opts.mode === "both") {
    lsdocResults = await runLsdocFiles(files, opts);
    findings = await runDiff(files, lsdocResults, opts);
  }

  const report = renderReport({ graphDir, opts, stats, versions: vers, bench, findings, zeroFiles: false });
  writeFileSync(opts.out, report);
  console.error(`wrote ${opts.out}`);
}

main().catch((e) => {
  cleanup();
  console.error(`graph-check failed: ${e.message}`);
  process.exit(1);
});

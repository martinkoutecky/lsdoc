// One-off triage: run the differential fuzzer, bucket mismatches by a structural
// signature (oracle block-kinds -> lsdoc block-kinds), and show an example per
// bucket so we can tell adversarial-soup classes from real bugs. Not a gate.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { writeFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { normalizeAst } from "./lib/normalize.mjs";
import { extractRefs } from "./lib/refs.mjs";
const require = createRequire(import.meta.url);
const { Mldoc } = require("mldoc");
const CFG = JSON.stringify({ toc:false, parse_outline_only:false, heading_number:false, keep_line_break:true, format:"Markdown", heading_to_list:false, export_md_remove_options:[] });
const proj = (s) => { const a = JSON.parse(Mldoc.parseJson(s, CFG)); return { blocks: normalizeAst(a), refs: extractRefs(a) }; };
const __dir = dirname(fileURLToPath(import.meta.url));

const N = parseInt(process.argv[2] || "30000", 10);
let seed = parseInt(process.argv[3] || "7", 10);
const rng = () => { seed = (seed*1103515245+12345)&0x7fffffff; return seed/0x7fffffff; };
const pick = (a) => a[Math.floor(rng()*a.length)];
const TOKENS = ["*","**","***","_","__","~~","==","^^","`","``","[[","]]","((","))","{{","}}","[","]","(",")","{","}","#","#tag","[[Foo]]","((11111111-1111-1111-1111-111111111111))","[label]","](url)","{{embed ","{{query ","https://x.com/a","http://y.org","\\","\\[","\\#","\\`","$","$x$","$$","!","![a]","<",">","<https://z.io>","a","b"," ","  ","\n","café","中文","😀",".",",","!",":","-","/","TODO ","[#A] ","[ ] ","\t","word","x","#[[","tag","::"];
const gen = () => { const len=1+Math.floor(rng()*14); let s=""; for(let i=0;i<len;i++) s+=pick(TOKENS); return s; };

const inputs = []; for (let i=0;i<N;i++) inputs.push({ id:`t${i}`, input: gen() });
writeFileSync(join(__dir,"corpus.fuzz.json"), JSON.stringify(inputs));
const env = { ...process.env, CARGO_HOME:"/aux/koutecky/logseq/.toolchain/cargo", RUSTUP_HOME:"/aux/koutecky/logseq/.toolchain/rustup", PATH:`/aux/koutecky/logseq/.toolchain/cargo/bin:${process.env.PATH}` };
spawnSync("cargo",["run","-q","--bin","lsdoc-parse","--",join(__dir,"corpus.fuzz.json"),join(__dir,"lsdoc-fuzz.json")],{cwd:join(__dir,".."),env});
const ls = Object.fromEntries(JSON.parse(readFileSync(join(__dir,"lsdoc-fuzz.json"),"utf8")).map(x=>[x.id,x]));

const IGN=new Set(["span"]);
const canon=(v)=>Array.isArray(v)?v.map(canon):(v&&typeof v==="object"?Object.fromEntries(Object.keys(v).sort().filter(k=>!IGN.has(k)).map(k=>[k,canon(v[k])])):v);
const S=(v)=>JSON.stringify(canon(v));
const kinds=(bs)=>(bs||[]).map(b=>b.kind).join(",");

const buckets = new Map();
for (const c of inputs) {
  let op; try { op = proj(c.input); } catch { continue; }
  const lp = ls[c.id]?.projection; if (!lp) continue;
  const bb = S(op.blocks)!==S(lp.blocks), rb = S(op.refs)!==S(lp.refs);
  if (!bb && !rb) continue;
  const sig = `[${kinds(op.blocks)}] -> [${kinds(lp.blocks)}]` + (rb?" +REF":"");
  if (!buckets.has(sig)) buckets.set(sig, { n:0, ex:c.input });
  buckets.get(sig).n++;
}
const sorted = [...buckets.entries()].sort((a,b)=>b[1].n-a[1].n);
console.log(`mismatch buckets (${sorted.length}) over ${N} inputs:\n`);
for (const [sig,{n,ex}] of sorted.slice(0,25)) console.log(`${String(n).padStart(4)}  ${sig}\n      e.g. ${JSON.stringify(ex)}`);

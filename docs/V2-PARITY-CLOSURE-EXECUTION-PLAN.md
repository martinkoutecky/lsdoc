# v2 parity closure execution plan

Status: executed on 2026-07-09. The "current known failures" section below is
historical seed evidence for the closure pass, not the current parity status. The
current status is tracked in `docs/V2-SYSTEMATIC-PARITY-AUDIT.md` and the permanent
reported-divergence gate.

## Objective

Close the gap between the v2 claim and the v2 reality:

- no known GitHub issue, real-graph, fuzz, or enumerated probe diverges from
  `mldoc@1.5.9` under `LSDOC_ENGINE=v2`;
- every v2 shortcut has an executable equivalence boundary, not only prose;
- public `parse` / `parse_format` still use v2 and remain linear;
- representative throughput remains at most **1.5x slower** than the fair
  state-of-the-art peers: `comrak` for Markdown and `orgize` for Org.

This plan is intentionally stricter than "fix the four current failures". The
four failures are the seed evidence; the real fix is making remaining shortcut
ownership executable.

## Historical seed failures

The 2026-07-09 GitHub issue sweep found 94 issue repros. With
`LSDOC_ENGINE=v2 node harness/vdiff_iso.mjs`, 90 match and 4 remain real diffs:

1. `issue2_04`, Markdown, `pages/tSC-Tool Regeltermin.md (lines 1-42)`.
   A regular list item contains `#+BEGIN_QUOTE`; inside the transformed quote body,
   v2 emits a Markdown `HardBreak` for a whitespace-only line where mldoc keeps a
   plain `"  "` segment.

2. `issue3_10`, Org, `20240821T132227--anonafilename-manual__anonafilename.org`.
   Org `[[url][label]]` label parsing reparses `=...=` as Verbatim and reconstructs
   `full` from the first parsed label child; mldoc keeps the label text/plain
   shape differently and preserves the raw full link.

3. `issue3_12`, Org, `20241215T203930--anonafilename.org`.
   Org link-label parsing handles `_...` / script-emphasis interaction differently
   from mldoc inside a link label.

4. `issue3_13`, Org, `20250712T125325--anonafilename.org`.
   Org `[[url]]` classification treats any non-empty prefix before `://` as a
   complex protocol. mldoc classifies
   `[[aaaaaa:aa.aaaa#aaaaa://aa.aaaa/?aaaa_aaaa=aaaaaaa&a=99999]]` as a page ref.

The issue #4 Android/CRLF report is cleared in v2: all 42 LF issue snippets and
all 42 CRLF-converted variants match the isolated oracle.

## Root cause classes

1. **Context matrix holes.** Inline equivalence was checked for top-level Markdown
   and Org, but not for every mldoc reparse context. Org link labels, nested
   emphasis repair, property values, and macro/ref reparsing are separate parser
   groups and cannot inherit the top-level proof.

2. **Transformed-body state holes.** Quote/callout/list body materialization was
   tested for many structural separators, but not for all combinations of
   whitespace-only lines, Markdown hardbreak state, list-content suppression, and
   transformed origins.

3. **Shortcut accepted-language drift.** Some helpers accept a convenient superset
   of mldoc's language. `classify_org_link_2` accepting any `://` is the minimal
   example.

4. **Prose proof without executable fence.** Existing audit rows can say
   "Audited" while the exact accepted language is not exercised by an oracle
   enumerator in every context.

## Invariants to enforce

- A shortcut may emit AST only if its accepted language is either source-transcribed
  or covered by a committed deterministic oracle enumerator for that exact context.
- If a shortcut cannot prove equivalence locally, it must decline before consuming
  input and fall through to the source-transcribed path.
- Performance recovery may not add an unproven shortcut. Correctness first; speed
  only via a proven-equivalent subset or by improving the source-transcribed path.
- "Audited" in docs means: source function named, accepted language stated, failure
  behavior stated, and an executable oracle gate committed.

## Phase 1 - make the failures permanent gates

1. Add a committed reported-divergence corpus, e.g.
   `harness/reported-divergences.json`, containing:
   - the 94 extracted GitHub issue repros;
   - CRLF variants for issue #4 code-block reports;
   - all entries from `V2-REAL-GRAPH-DIVERGENCES.md`;
   - the four current minimized failure cases with stable IDs.

2. Add `harness/reported-divergences.mjs` that runs the corpus through the same
   isolated oracle discipline as `vdiff_iso.mjs`, with `--engine v2` explicitly.

3. Wire the reported-divergence gate into `harness/run.mjs` or document it as a
   required standalone gate until runtime is acceptable.

Acceptance:

```bash
rtk proxy bash -lc 'source scripts/env.sh && cd harness && LSDOC_ENGINE=v2 node reported-divergences.mjs'
```

The gate must fail before fixes and pass after fixes.

## Phase 2 - fix the four known diffs by narrowing ownership

1. Markdown transformed quote/list hardbreak:
   - locate the clean-frame/list-content path that reparses the `#+BEGIN_QUOTE`
     body;
   - either carry the exact mldoc paragraph/hardbreak state into inline parsing, or
     decline the clean shortcut for whitespace-only/hardbreak-sensitive transformed
     lines;
   - add minimal probes for `"  "` line, `"   "` line, two trailing spaces before
     newline, blank line, list-contained quote, and quote-contained list.

2. Org `[[url][label]]` labels:
   - source-check mldoc `org_link_1` label parser and label reparse grammar;
   - fix `Ctx::label()` / `org_link_1_at` so label children and `full` match mldoc;
   - preserve raw `full` from the accepted source slice unless mldoc truly rebuilds
     it from parsed children;
   - enumerate labels containing `=...=`, `~...~`, `_...`, `^...`, `*...*`,
     `/.../`, `+...+`, brackets, escaped brackets, URLs, timestamps, tags, and
     non-ASCII.

3. Org `[[url]]` classification:
   - source-check mldoc `org_link_2` URL classification;
   - replace the current "any `://`" rule with the source grammar;
   - enumerate `file:`, `proto://x`, empty protocol, colon-before-protocol,
     `#...://`, multiple colons, query strings, `&`, escaped brackets, and plain
     page refs.

Acceptance:

```bash
rtk proxy bash -lc 'source scripts/env.sh && cd harness && LSDOC_ENGINE=v2 node reported-divergences.mjs'
rtk proxy bash -lc 'source scripts/env.sh && cd harness && LSDOC_ENGINE=v2 node vdiff_iso.mjs reported-current-fixes.json'
```

## Phase 3 - add the context-matrix inline oracle

Create or extend `harness/audit-v2-shortcuts.mjs` so the same generated inline
atoms are tested in all relevant mldoc contexts:

- Markdown top-level inline;
- Markdown nested-emphasis child;
- Markdown link label / title / URL-adjacent contexts where applicable;
- Org top-level inline;
- Org nested-emphasis child;
- Org `[[url][label]]` label;
- Org `[[url]]` classification;
- parse1 property values with inline-skip-macro refs;
- macro argument parsing where page refs and block refs are special.

The alphabet should include at least:

- whitespace, LF, CRLF, trailing-space hardbreak candidates;
- `*`, `_`, `^`, `/`, `+`, `~`, `=`, backtick, `$`;
- `[`, `]`, `[[...]]`, `[[url][label]]`, `[label](url)`, images;
- `#tag`, `{{macro}}`, `((uuid))`, timestamps, cookies;
- `<...>`, URLs, entities, escaped punctuation, non-ASCII.

Acceptance:

```bash
rtk proxy bash -lc 'source scripts/env.sh && LSDOC_ENGINE=v2 node harness/audit-v2-shortcuts.mjs'
```

Any new diff from this matrix must be fixed or narrowed before moving on.

## Phase 4 - add transformed-body block enumerators

Add a deterministic block-context enumerator covering the same small body fragments
inside:

- top-level document;
- Markdown regular-list item;
- Markdown blockquote;
- Markdown `#+BEGIN_QUOTE` / custom callout;
- list-contained quote/callout;
- quote-contained list/callout.

Fragment families:

- whitespace-only lines with 0, 1, 2, 3 spaces/tabs/form-feed;
- lines ending in two spaces before LF/CRLF;
- blank runs before paragraph, HR, list, heading/bullet-looking line, property,
  drawer, fence, raw HTML, displayed math, hiccup, footnote, and comment;
- lazy continuation lines and suppressed parser families.

Acceptance:

```bash
rtk proxy bash -lc 'source scripts/env.sh && LSDOC_ENGINE=v2 node harness/audit-v2-shortcuts.mjs --block-contexts'
```

If this is implemented as a separate script, use that script name and record it in
`docs/V2-SYSTEMATIC-PARITY-AUDIT.md`.

## Phase 5 - update proof documents to match executable reality

Update:

- `docs/V2-SYSTEMATIC-PARITY-AUDIT.md`;
- `docs/V2-TRANSCRIPTION.md`;
- `docs/LINEARITY.md` if ownership or scan-work changes;
- `bench/README.md` after the performance gate.

Audit rows may be marked **Audited** only when the relevant enumerator/probe is
committed and named. Otherwise mark **Needs audit**.

## Phase 6 - full correctness gates

Run and clear:

```bash
rtk proxy bash -lc 'source scripts/env.sh && cargo fmt -- --check'
rtk proxy bash -lc 'source scripts/env.sh && cargo check'
rtk proxy bash -lc 'source scripts/env.sh && cargo test'
rtk proxy bash -lc 'source scripts/env.sh && cargo test --test complexity'
rtk proxy bash -lc 'source scripts/env.sh && cargo test --release -- --ignored'
rtk proxy bash -lc 'source scripts/env.sh && LSDOC_ENGINE=v2 node harness/run.mjs'
rtk proxy bash -lc 'source scripts/env.sh && LSDOC_ENGINE=v2 node harness/realmut.mjs'
rtk proxy bash -lc 'source scripts/env.sh && cd harness && LSDOC_ENGINE=v2 node fuzz.mjs 40000 4242'
rtk git diff --check
```

If machine-specific real block exports are present, ensure `blockgate` runs and
passes rather than silently skipping.

## Phase 7 - performance gate

Rebuild and run the benchmark suite in release mode using the same representative
regime as `bench/README.md`.

Required public gates:

- Markdown `logseq/docs`: lsdoc must be at most 1.5x slower than `comrak`;
- Org `worg`: lsdoc must be at most 1.5x slower than `orgize`;
- scaling mode must remain approximately linear.

Required private gate when `/nfs/home/koutecky/research/brain` is mounted:

- private brain graph Markdown: lsdoc must be at most 1.5x slower than `comrak`.

Commands:

```bash
rtk proxy bash -lc 'source scripts/env.sh && cd bench && cargo build --release'
rtk proxy bash -lc 'source scripts/env.sh && cd bench && ./target/release/lsdoc-bench --graph corpus/logseq-docs'
rtk proxy bash -lc 'source scripts/env.sh && cd bench && ./target/release/lsdoc-bench --graph corpus/worg --format org'
rtk proxy bash -lc 'source scripts/env.sh && cd bench && ./target/release/lsdoc-bench --graph corpus/logseq-docs --scale'
rtk proxy bash -lc 'source scripts/env.sh && cd bench && ./target/release/lsdoc-bench --graph /nfs/home/koutecky/research/brain'
```

If public corpora are missing, run `bench/fetch-corpus.sh` only with network
approval, then repeat the benchmark. Do not waive the performance gate because the
corpus is absent; mark the run blocked until the corpus is available.

If the 1.5x gate fails:

1. keep the correctness fix;
2. profile the regression;
3. recover speed only through a source-transcribed path improvement or a newly
   enumerated conservative shortcut;
4. rerun all parity gates before accepting the speed fix.

## Final acceptance checklist

- `harness/reported-divergences.mjs` passes for all known GitHub and real-graph
  repros.
- Context-matrix inline enumerator passes.
- Transformed-body block-context enumerator passes.
- Existing full parity, realmut, fuzz, complexity, release ignored, and diff-check
  gates pass.
- `bench/README.md` contains fresh numbers and all fair-peer ratios are <= 1.5x.
- Docs no longer claim "Audited" for any shortcut lacking an executable oracle
  boundary.
- No unrelated user changes are reverted.

## Goal command instruction

Use this as the goal text:

```text
In /aux/koutecky/logseq/lsdoc, execute docs/V2-PARITY-CLOSURE-EXECUTION-PLAN.md end to end: add permanent reported-divergence and context enumerator gates, fix all v2/mldoc diffs they expose by narrowing or source-transcribing shortcuts, update proof/performance docs, and do not finish until the full parity suite and the <=1.5x comrak/orgize performance gate pass.
```

# lsdoc architecture (as built)

lsdoc parses Logseq-flavored Markdown + Org into a typed, serde AST, byte-exact to
`mldoc@1.5.7` over the differential harness (`harness/run.mjs`: 1039 corpus + 99 real
block bodies + 37 inline). Two formats, two parallel implementations that share leaf
predicates: **Markdown** = `parse.rs` (block) + `lexer.rs`/`resolver.rs` (inline);
**Org** = `org.rs` (block) + `org_resolver.rs` (inline). The AST/refs/entities and the
whole harness are the frozen oracle.

## Two phases, linear by construction

**Inline** (`resolver.rs` / `org_resolver.rs`): a context-free **lexer** emits offset-
tagged tokens (marking — never rewriting — escape positions), then a one-pass + stack
**resolver** builds `Vec<Inline>`. Emphasis = leftmost opener → first FORWARD valid
closer with a per-(marker,len) `no_closer` floor and flat reparsed content (NOT a
CommonMark backward `openers_bottom` stack — that yields a different tree). Brackets pair
via delimiter-stack maps with the two escape disciplines (nested-link escape-free,
page-ref escape-aware). Closers are found via monotone cursors (`first_seq`) and
closer-presence floors (e.g. `bs_paren`/`bs_brack` so a `\(`×n run stays O(n)) — never an
EOF re-scan per opener. Org adds a stateful backward gate (`last_plain_char`) and
ctx-gated code/verbatim.

**Block** (`parse.rs` / `org.rs`): `split_lines` → a single forward dispatch loop in
mldoc's priority order (fence, callout, latex-env, heading, hr, bullet, footnote, table,
property-drawer, list, quote, raw-html, math, drawer, hiccup, def-list, paragraph).
Container bodies are RE-PARSED recursively (mldoc's `take_until` + recurse-on-body): the
one O(n·depth) carve-out, depth-bounded by the 1 MiB-stack gate.

### Container pairing is O(n) by construction

mldoc's `#+BEGIN`/`#+END` and `:NAME:`/`:END:` are **outermost-first** (NOT LIFO):
`#+BEGIN_QUOTE / #+BEGIN_QUOTE / #+END_QUOTE` → `Quote{ Paragraph("#+BEGIN_QUOTE") }`.
Closer-finding uses NO binary search and NO absence memo:

- **Callouts + drawers** are pre-paired globally in ONE forward **pending-opener-stack**
  pass (`pair_callouts` / `pair_drawers`), reproducing outermost-first. This is sound
  because `#+BEGIN`/`#+END` (and `:NAME:`/`:END:`) are **role-distinct tokens** (a closer
  line is never also an opener line) and their `take_until` closer is **textual** (ignores
  fences/drawers — verified vs mldoc). Callout closers prefix-match
  (`#+END_QUOTEX`/`#+END_QUOTE x` close `QUOTE`; `#+END_QUOT` does not), handled in O(n)
  via a `by_name` stack-position index (`outermost_callout_match`, an O(|suffix|) prefix
  probe) — so even unique-name openers + non-matching `#+END_` stay linear.
- **Fences** (` ``` `/`~~~`) CANNOT be pre-paired: opener and closer are the **same token**,
  so a global greedy pass mis-assigns roles — that was a real shipped bug (a ` ``` ` inside
  a callout/drawer body paired with one outside it, giving `quote,paragraph` where mldoc
  gives `quote,src`). Fences are found ON-DEMAND at the dispatch point (context-correct,
  since the loop jumps past claimed bodies) via a monotone per-char cursor over
  `fence_line_idxs`.

Org wrinkle: the headline split rewrites a line (`* #+BEGIN_X` / `* :PROPERTIES:` → split
+ re-enter its content), so the org pairing passes also recognize a container opener behind
a headline marker (`headline_split_content`), gated `!in_item && !ORG_IN_QUOTE` to match the
dispatch's split condition.

## Performance gate

`tests/perf.rs`: `perf_smoke` (debug, fast) + `#[ignore]`d heavy gates (run
`cargo test --release -- --ignored`). `assert_linear_scaling` (`scaling_pairs`) is the
ratio gate — it measures n→2n→4n and gates on the MIN doubling (linear ≈2×, O(n²) ≈4×,
O(n³) ≈8×; CAP 3.0), robust to this shared NFS box's noise. Every known pathological /
adversarial generator (unclosed-opener runs, unique-name + non-matching closers, `\(`×n,
emphasis soup) is locked in here.

## What is NOT here (deliberate)

An explicit `lex_lines → Vec<LineTok>` line-lexer phase: the block dispatch already
classifies each line inline and the O(n) goal is met via the pairing pre-passes, so a
separate line-token type would be a lateral clarity refactor of already-clean code with
real parity risk and no perf/correctness gain. The pending-opener pre-passes + recursive
dispatch ARE the two-phase structure (closer-precompute + line-driven block build).

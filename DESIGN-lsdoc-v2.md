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

All closers are found **ON-DEMAND at the dispatch point** — i.e. only when the top-down
dispatch *reaches* an opener (having jumped past any earlier container's body). This is
mldoc's own recursive-descent + `take_until` structure, and it is what makes pairing
**correct**: it cleanly separates mldoc's two rules — a closer is **textual** (`take_until`
sees `#+END_`/`:END:` even inside a fence), but an opener is **contextual** (a `#+BEGIN`/
`:NAME:` inside an opaque body — fence code, `#+BEGIN_SRC`/Example/drawer bodies, latex envs
— is content, not an opener). A *global pre-pass* cannot separate these: it registers
phantom openers inside opaque bodies that steal real closers (a shipped data-loss bug we
hit and reverted — see [[lsdoc-architecture-redesign]]). The dispatch gets it for free.

The on-demand finders are O(n) by construction (no per-opener EOF scan):
- **Callouts** (`#+BEGIN_X` … `#+END_X`, outermost-first, prefix-match — `#+END_QUOTEX`
  closes `QUOTE`): an **`EndTrie`** over the (lowercased) `#+END_<name>` names. Each node
  holds the ascending line indexes of every `#+END_` line whose name has that prefix, so an
  opener X walks X (O(|X|)) and `partition_point`s the node's list; absent path ⇒ O(1)
  (unclosed). Build is O(Σ|name|) = O(n), prefixes shared (one long name is O(name), not
  O(name²)). This is O(n) **even on the adversarial unclosed-opener runs mldoc itself is
  O(n²) on** (measured: 4000 openers = 68 s in mldoc).
- **Drawers** (`:NAME:` … `:END:`): a sorted `:END:` index + `partition_point` (first
  `:END:` after the opener).
- **Fences** (` ``` `/`~~~`, opener == closer token): a monotone per-char cursor over
  `fence_line_idxs`. (NB the global *greedy* `pair_fences` that lsdoc once used mis-assigned
  roles across container boundaries — a ` ``` ` inside a callout body paired with one outside
  it, `quote,paragraph` vs mldoc's `quote,src`; on-demand finding fixes that.)

Recurse-on-body is the inherent O(n·depth) carve-out (mldoc re-parses callout/drawer/quote
bodies; depth bounded by the 1 MiB-stack gate). Org wrinkle: the headline split rewrites a
line (`* #+BEGIN_X` / `* :PROPERTIES:` → emit an empty bullet, then re-enter the content),
so the close-gate runs on the rewritten content via the same on-demand finders.

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

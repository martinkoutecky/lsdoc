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

**Block** (`parse.rs` / `org.rs`): `split_lines` → a SINGLE streaming pass over an explicit
container-frame stack, dispatching each line in mldoc's priority order (fence, callout,
latex-env, heading, hr, bullet, footnote, table, property-drawer, list, quote, raw-html,
math, drawer, hiccup, def-list, paragraph). Each input line is classified **once**; a
callout/quote body is a line **window** opened as a heap `Frame` (`Step::Open`), never copied
or re-lexed. Outermost-first pairing falls out of a `closer < frame.hi` check; demotion of an
inner opener whose closer lies outside the body needs no re-parse (it is re-classified in the
same pass). **O(n) time, O(depth) HEAP** — no native recursion, no depth cap. (The old
recurse-on-body — itself mldoc's O(n²) + stack-overflow — is gone; see
[[lsdoc-architecture-redesign]].)

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

Inside a `Frame`, every closer-search is bounded by the frame's `hi` / `body_end` (a no-op
at the top level where `hi == lines.len()`): fence/callout/drawer accept a closer only if
`< hi`; the to-EOF forward-scanners (`parse_latex_env`, `parse_hiccup`) are clamped to
`&input[..body_end]`. So a closer / `\end{}` / `]` / run-line belongs to THIS body, never the
enclosing one — the streaming equivalent of the old recursion's body-slice bound.

The `>`-quote nests via an iterative suffix-view peel (`build_org_quote_streaming`, encoding
mldoc's opener-strips-2 / continuation-strips-1 asymmetry over `&str` views, no per-level
copy), also uncapped. The ONE residual: `BLOCK_NEST_CAP` survives purely as a graceful
anti-SIGABRT guard on two **fuzz-unreachable** re-dispatch shapes — an increasing-`>`-per-line
quote (needs O(d²) input to reach depth d) and deeply-INDENTED / `\r\n` callout bodies — NOT a
parity cap; mldoc is O(n²)+overflows there too, and the recursive AST *consumer* (drop /
project / serialize, ~6k) caps that depth regardless. Org wrinkle: the headline split rewrites
a line (`* #+BEGIN_X` / `* :PROPERTIES:` → emit an empty bullet, then re-enter the content via
`Step::Next(i)` without advancing), so the close-gate runs on the rewritten content via the
same `hi`-bounded finders.

## Performance gate

`tests/perf.rs`: `perf_smoke` (debug, fast) + `#[ignore]`d heavy gates (run
`cargo test --release -- --ignored`). `assert_linear_scaling` (`scaling_pairs`) is the
ratio gate — it measures n→2n→4n and gates on the MIN doubling (linear ≈2×, O(n²) ≈4×,
O(n³) ≈8×; CAP 3.0), robust to this shared NFS box's noise. Every known pathological /
adversarial generator (unclosed-opener runs, unique-name + non-matching closers, `\(`×n,
emphasis soup) is locked in here.

## What is NOT here (deliberate)

An explicit `lex_lines → Vec<LineTok>` line-lexer phase: the streaming dispatch already
classifies each line inline (once), so a separate line-token type would be a lateral clarity
refactor with real parity risk and no perf/correctness gain. The on-demand closer indexes
(`EndTrie` / drawer / fence) + the streaming container-frame stack ARE the two-phase
structure (closer-precompute + single-pass line-driven block build).

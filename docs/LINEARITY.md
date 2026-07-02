# Inline Linearity Contract

This file is the standing O(n) ownership map for inline-phase forward scans. Each scan
must have exactly one owner: constant/local, consume-on-match, suffix-absence miss-cache,
invalidating cursor, precomputed map, or boundary-run map. A new unfloored scan in these
paths is a bug.

C7 export-snippet rows were added against the current tree; older row line numbers retain
their last audit positions.

| scan @ file:line | owner | argument |
|---|---|---|
| Markdown resolver token loop @ `src/resolver.rs:134` | consume-on-match | `t` advances monotonically; successful leaves resync past consumed bytes. |
| Markdown page-ref `]]` lookup @ `src/resolver.rs:216` | precomputed-map | `build_real_dbl` positions are shared with a monotone cursor. |
| Markdown hiccup/nested close lookup @ `src/resolver.rs:199` | precomputed-map | `build_hiccup_close` and `build_nested_close` give O(1) close checks. |
| Markdown md-link `](`/`)` floors @ `src/resolver.rs:1079`, `src/resolver.rs:1227`, `src/resolver.rs:1673` | invalidating-cursor | `lbp_cur`, `crlf`, and `rparen` advance only forward before `md_link`. |
| Markdown emphasis body parser @ `src/resolver.rs:255`, dispatch @ `src/resolver.rs:432` | consume-on-match + suffix-absence miss-cache | mldoc `md_em_parser` consumes each body byte once per bounded nesting phase; `no_closer[class][k]` floors EOF/no-closer failures. |
| Markdown script braced scan @ `src/resolver.rs:825`, body reparse @ `src/resolver.rs:850` | `}`-before-eol invalidating-cursor + bounded body | Braced `_`/`^` dispatch is gated by `ByteBeforeEolScan` at label, nested, and top-level call sites; accepted script bodies are disjoint consumed spans and body reparse is bounded by that span. |
| Markdown entity lexer @ `src/lexer.rs:173` | consume-on-match | `\` plus the maximal ASCII-letter run and optional `{}` are consumed once; known names emit `Entity`, unknown names emit the same consumed bare name as `Plain`. |
| Markdown tag dispatch @ `src/resolver.rs:1156`, `src/inline.rs:394` | boundary-run | delimiter-run termination is precomputed once by `build_tag_boundary_runs`. |
| Markdown macro dispatch @ `src/resolver.rs:846`, `src/inline.rs:1844` | suffix-absence miss-cache + invalidating-cursor | `}}` floor proves close presence; first lone `}` cursor prevents repeated invalid misses. |
| Markdown export-snippet dispatch @ `src/resolver.rs:1278`, parser @ `src/inline.rs:2275` | suffix-absence miss-cache + consume-on-match | `@@` closer floor skips parser calls when no closer exists after the opener; accepted snippets consume through the closing `@@`, and invalid candidates scan only to the first `@`/EOL in that candidate. |
| Markdown block-ref dispatch @ `src/resolver.rs:860`, `src/inline.rs:1809` | suffix-absence miss-cache + invalidating-cursor | `))` floor proves close presence; first lone `)` cursor owns body-invalid failures. |
| Markdown angle autolink @ `src/resolver.rs:810`, `src/inline.rs:1206` | suffix-absence miss-cache + invalidating-cursor | one cursor owns first `>`/ws after `<scheme:`; EOF and ws-before-`>` are cached outcomes. |
| Markdown angle timestamp @ `src/resolver.rs:1253`, `src/inline.rs:2011` | suffix-absence miss-cache + invalidating-cursor | active `<...>` bodies and active range halves are gated by the `>` half of `TimestampCloseScan`. |
| Markdown inactive bracket timestamp @ `src/resolver.rs:1018`, `src/inline.rs:2041` | suffix-absence miss-cache + invalidating-cursor | inactive `[...]` bodies and inactive range halves are gated by the `]` half of `TimestampCloseScan`, after link/reference attempts. |
| Markdown keyword/range timestamp @ `src/resolver.rs:1287`, `src/inline.rs:2027` | suffix-absence miss-cache + invalidating-cursor | keyword active/inactive bodies reuse the delimiter-specific timestamp cursor; range second-half probes are transactional clones, committed only on a successful range so fallback stays O(n). |
| Markdown email local/domain @ `src/resolver.rs:810`, `src/inline.rs:1288` | suffix-absence miss-cache + invalidating-cursor | local-part keeps the `@` absence floor; domain uses a first `>`/ws cursor. |
| Markdown raw HTML angle @ `src/resolver.rs:810`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | missing closers use `RawHtmlScan`; unbalanced tag matching uses a lazy per-tag/body index. |
| Markdown bare URL dispatch @ `src/resolver.rs:1325`, `src/inline.rs:1730` | consume-on-match + suffix-absence miss-cache | accepted URLs consume their span; all-alphanumeric no-scheme suffixes are floored by `BareUrlScan`. |
| Markdown resync lead checks @ `src/resolver.rs:1581` | invalidating-cursor | split-token bare-url probes share `BareUrlScan`; fast path re-lexes only the split token. |
| Org resolver token loop @ `src/org_resolver.rs:270` | consume-on-match | `t` advances monotonically; accepted leaves resync past consumed bytes. |
| Org bracket/page/link floors @ `src/org_resolver.rs:337`, `src/org_resolver.rs:1141` | precomputed-map + invalidating-cursor | bracket close maps plus `rbracket`, `sq_rb_lb`, `real_dbl_cur`, and `crlf` gate scans. |
| Org macro/block-ref dispatch @ `src/org_resolver.rs:550`, `src/org_resolver.rs:558` | suffix-absence miss-cache | `sq_rbrace` and `sq_rr` must be present before parsers run. |
| Org export-snippet dispatch @ `src/org_resolver.rs:1003`, parser @ `src/inline.rs:2275` | suffix-absence miss-cache + consume-on-match | `sq_at` proves a closing `@@` after the opener before the parser runs; accepted snippets consume through the closer, failed candidates are bounded by the first `@`/EOL. |
| Org tag dispatch @ `src/org_resolver.rs:914`, `src/inline.rs:394` | boundary-run | same delimiter-run precompute as Markdown. |
| Org angle target @ `src/org_resolver.rs:975` | constant/local | `<<target>>` stops at `<`, `>`, or EOL and is tried once at the dispatch byte. |
| Org autolink @ `src/org_resolver.rs:975`, `src/org.rs:2210` | suffix-absence miss-cache + invalidating-cursor | shared first `>`/ws cursor gates `parse_org_autolink`. |
| Org angle timestamp @ `src/org_resolver.rs:977`, `src/inline.rs:2011` | suffix-absence miss-cache + invalidating-cursor | active `<...>` bodies and active range halves are gated by the `>` half of `TimestampCloseScan`. |
| Org inactive bracket timestamp @ `src/org_resolver.rs:944`, `src/inline.rs:2041` | suffix-absence miss-cache + invalidating-cursor | inactive `[...]` bodies and inactive range halves are gated by the `]` half of `TimestampCloseScan`, after org link attempts. |
| Org keyword/range timestamp @ `src/org_resolver.rs:808`, `src/inline.rs:2027` | suffix-absence miss-cache + invalidating-cursor | keyword active/inactive bodies reuse the delimiter-specific timestamp cursor; range second-half probes are transactional clones, committed only on a successful range so fallback stays O(n). |
| Org raw HTML angle @ `src/org_resolver.rs:975`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | same `RawHtmlScan` and unbalanced tag index as Markdown. |
| Org email domain @ `src/org_resolver.rs:975`, `src/inline.rs:1288` | suffix-absence miss-cache + invalidating-cursor | same email `@` floor plus domain boundary cursor as Markdown. |
| Org bare URL dispatch/resync @ `src/org_resolver.rs:809`, `src/org_resolver.rs:1273` | consume-on-match + suffix-absence miss-cache | accepted URLs consume; resync lead probes share `BareUrlScan`. |
| Org emphasis body parser @ `src/org_resolver.rs:370`, dispatch @ `src/org_resolver.rs:486` | consume-on-match + suffix-absence miss-cache | Org uses the same mldoc body parser with `include_md_code=false`; `no_closer[class][k]` floors EOF/no-closer failures. |
| Org script braced scan/fallback @ `src/org_resolver.rs:1026`, body reparse @ `src/org_resolver.rs:1075` | `}`-before-eol invalidating-cursor + consume-on-match | Braced `_`/`^` attempts are gated by `ByteBeforeEolScan`; when absent, Org falls straight to the p1 `non_space` run. Accepted braced or p1 spans bound the body reparse. |
| Org entity dispatch @ `src/org_resolver.rs:1108`, top-level @ `src/org_resolver.rs:1170` | consume-on-match | Entity handling consumes the maximal ASCII-letter run and optional `{}` for both known and unknown names; unknown names consume the run and emit bare `Plain(name)`. |
| `parse_tag_name` capture/reparse @ `src/inline.rs:573`, `src/inline.rs:659` | consume-on-match + boundary-run | captured tag bytes are consumed once; delimiter suffixes use the boundary map, then the bounded tag string is reparsed. |
| Tag nested/page/link reparse @ `src/inline.rs:716`, `src/org_resolver.rs:655` | consume-on-match in tag | successful nested/page/Markdown/Org links advance the tag reparse cursor; top-level bracket retries are separately gated by maps. |
| Macro arg scans @ `src/inline.rs:784` | consume-on-match | scans are limited to an already accepted macro body. |
| Markdown `markdown_embed_image` data branch @ `src/inline.rs:918` | consume-on-match under md-link floors | The `data:` scan runs only after `try_md_link` proves `](` and a `)`; success consumes through that `)`, failure is bounded by the same candidate. |
| Markdown `label_part` @ `src/inline.rs:985` | consume-on-match under md-link floors | Reached only after `try_md_link` proves a same-line `](`; label chunks, code spans, page refs, and bracket chunks advance the local label cursor monotonically to that delimiter. |
| Markdown label `string_contains_balanced_brackets` @ `src/inline.rs:1037` | bounded-by-label-candidate | The iterative helper is called only inside the already-floored label span and advances one local cursor; unmatched-left fallback does not scan past the label candidate. |
| Markdown `link_url_part` @ `src/inline.rs:1282` | bounded-by-url-candidate | The balanced-paren scan is called only after the resolver has a forward `)` floor; it advances to that candidate or to an eol stop, with no retry from later bytes. |
| Markdown `link_url_part_inner` URL/title reparse @ `src/inline.rs:1305` | bounded-by-url-candidate | It reparses only the raw string returned by `link_url_part`; URL pieces, quoted-title scans, and parse-failure fallback are one pass over that bounded candidate. |
| Markdown label Plain reparse @ `src/resolver.rs:67` | bounded disjoint label spans | Each Plain label node is reparsed once with the C1 emphasis port plus latex/entity/code/script choices; consume-all failure keeps that one Plain chunk, so chunks are not rescanned. |
| Markdown link metadata @ `src/inline.rs:1430` | consume-on-match/current-line | `{...}` metadata is checked immediately after an accepted link/image and scans only to `}` before eol; absence is constant at the current end byte. |
| Org `org_link_1` URL scan @ `src/org_resolver.rs:1522` | bounded by org bracket floors | `try_bracket_at` reaches this path only with the `][` floor; the URL scan advances once to the candidate `]` and accepts escaped `]` without retrying prior bytes. |
| Org `org_link_1` label scan @ `src/org_resolver.rs:1587` | bounded by org `]]`/eol floors | The label scan advances one cursor to the caller-proved closing `]]`; single `]` is consumed as label text unless it is that final closer. |
| Org label reparse/full reconstruction @ `src/org_resolver.rs:1529` | bounded disjoint label span | The label string is reparsed once with `Ctx::label()`; the full-text first-Plain-only quirk reads only the first produced node. |
| Org link metadata @ `src/org_resolver.rs:1672` | consume-on-match/current-line | Same metadata parser as Markdown in placement: it runs only after an accepted link and scans to `}` before eol. |
| Autolink parser body @ `src/inline.rs:1445` | invalidating-cursor owned by caller | parser may scan to `>`/ws, but dispatch only calls it after the shared boundary cursor succeeds. |
| Email parser body @ `src/inline.rs:1540` | suffix-absence miss-cache + invalidating-cursor | cached entry point owns both local `@` absence and domain boundary. |
| Bare URL path balance @ `src/inline.rs:1816`, `src/inline.rs:1826` | consume-on-match | the balanced tail is part of the emitted URL span and advances a single local cursor. |
| LaTeX backslash/dollar @ `src/inline.rs:2113`, `src/inline.rs:2134` | invalidating-cursor / `$`-before-eol invalidating-cursor | Backslash closers are gated by resolver `\)`/`\]` cursors; top-level dollar dispatch is gated by `ByteBeforeEolScan` and successful dollar spans consume through their closer. Label reparses are bounded by the accepted label span. |
| Timestamp body slots @ `src/inline.rs:2394`, token boundary cursor @ `src/inline.rs:341` | invalidating-cursor owned by caller | after a cursor-owned close candidate, delimiter-specific token-boundary cursors own the date/time/repetition slot scans; exact body spacing and two-slot interpretation are bounded local work with no `split_whitespace` suffix rescans or caps. |
| Raw HTML head @ `src/block_common.rs:406` | constant | known tag token scan is bounded by `MAX_HTML_TAG_LEN = 10`. |
| Raw HTML special closer @ `src/block_common.rs:497`, `src/block_common.rs:530` | suffix-absence miss-cache | missing special closers update `RawHtmlScan.no_special_until`. |
| Raw HTML missing tag closer @ `src/block_common.rs:530`, `src/block_common.rs:614` | suffix-absence miss-cache | no `</tag>` ahead updates `RawHtmlScan.no_tag_end_until[index]`. |
| Raw HTML unbalanced tag @ `src/block_common.rs:200`, `src/block_common.rs:530` | precomputed-map | lazy per `(tag, body_end)` index answers match-tag balance queries without rescanning. |
| Raw HTML self-close fallback @ `src/block_common.rs:200` | precomputed-map | first `/>` after the opener is stored in the same tag/body index and used only after no match. |
| Raw HTML accepted capture mapping @ `src/block_common.rs:668`, `src/block_common.rs:708` | consume-on-match | line/view mapping scans only accepted raw-HTML extents and consumed trailing blanks. |

Documented non-inline exceptions:

| exception @ file:line | bound | note |
|---|---|---|
| refs sort @ `src/refs.rs:20` | `O(R log R)` | reference count `R <= input bytes`; this is outside inline parsing. |
| GT fallback recursion cap @ `src/block_common.rs:33` | capped | `GT_FALLBACK_NEST_CAP = 64` applies only to the residual transformed quote fallback. |

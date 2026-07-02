# Inline Linearity Contract

This file is the standing O(n) ownership map for inline-phase forward scans. Each scan
must have exactly one owner: constant/local, consume-on-match, suffix-absence miss-cache,
invalidating cursor, precomputed map, or boundary-run map. A new unfloored scan in these
paths is a bug. Every new row must state the summation argument, not only name the mechanism.
Audit lesson: counters must charge data-structure work, and every cache key dimension needs a
gate shape that maximizes its cardinality.

C8 refreshed the driver rows after the dispatch loops were rewritten in `inline_choices`
order. Older construct-internal rows are kept where the owner did not change.

| scan @ file:line | owner | argument |
|---|---|---|
| Shared fresh/swallow tables @ `src/inline_driver.rs:17` | constant tables | Markdown/Org plain-delimiter and swallow-byte tables encode mldoc `plain` fallback; no scan is introduced. |
| Markdown resolver token loop @ `src/resolver.rs:1259` | consume-on-match | `t` advances monotonically; successful leaves resync past consumed bytes. |
| Inline OriginMap build/remap @ `src/parse.rs:1717`, `src/org.rs:1766`, `src/source_map.rs:299` | compose-at-build + one monotone remap | Fold buffers append source views and EOL joiners once while constructing text; remap walks each inline run and each origin segment in source order, with no suffix rescans or endpoint-only remapping. |
| Markdown page-ref `]]` lookup @ `src/resolver.rs:1055` | precomputed-map | `build_real_dbl` positions are shared with a monotone cursor. |
| Markdown hiccup/nested close lookup @ `src/resolver.rs:1047` | precomputed-map | `build_hiccup_close` and `build_nested_close` give O(1) close checks. |
| Markdown md-link `](`/`)` floors @ `src/resolver.rs:1477`, `src/resolver.rs:1592`, `src/resolver.rs:1989` | invalidating-cursor | `lbp_cur`, `crlf`, and `rparen` advance only forward before `md_link`. |
| Markdown emphasis body parser @ `src/resolver.rs:255`, dispatch @ `src/resolver.rs:432` | consume-on-match + suffix-absence miss-cache | mldoc `md_em_parser` consumes each body byte once per bounded nesting phase; `no_closer[class][k]` floors EOF/no-closer failures. |
| Markdown script braced scan @ `src/resolver.rs:1229`, body reparse @ `src/resolver.rs:922` | `}`-before-eol invalidating-cursor + bounded body | Braced `_`/`^` dispatch is gated by `ByteBeforeEolScan` at label, nested, and top-level call sites; accepted script bodies are disjoint consumed spans and body reparse is bounded by that span. |
| Markdown entity lexer @ `src/lexer.rs:173` | consume-on-match | `\` plus the maximal ASCII-letter run and optional `{}` are consumed once; known names emit `Entity`, unknown names emit the same consumed bare name as `Plain`. |
| Markdown tag dispatch @ `src/resolver.rs:1293`, `src/inline.rs:573` | boundary-run | delimiter-run termination is precomputed once by `build_tag_boundary_runs`. |
| Markdown macro dispatch @ `src/resolver.rs:1573`, `src/inline.rs:2223` | suffix-absence miss-cache + invalidating-cursor | `}}` floor proves close presence; first lone `}` cursor prevents repeated invalid misses. |
| Markdown export-snippet dispatch @ `src/resolver.rs:1609`, parser @ `src/inline.rs:2266` | suffix-absence miss-cache + consume-on-match | `@@` closer floor skips parser calls when no closer exists after the opener; accepted snippets consume through the closing `@@`, and invalid candidates scan only to the first `@`/EOL. |
| Markdown block-ref dispatch @ `src/resolver.rs:1666`, `src/inline.rs:2188` | suffix-absence miss-cache + invalidating-cursor | `))` floor proves close presence; first lone `)` cursor owns body-invalid failures. |
| Markdown angle autolink @ `src/resolver.rs:1871`, `src/inline.rs:1473` | suffix-absence miss-cache + invalidating-cursor | one cursor owns first `>`/ws after `<scheme:`; EOF and ws-before-`>` are cached outcomes. |
| Markdown angle timestamp @ `src/resolver.rs:1878`, `src/inline.rs:2316` | suffix-absence miss-cache + invalidating-cursor | active `<...>` bodies and active range halves are gated by the `>` half of `TimestampCloseScan`. |
| Markdown inactive bracket timestamp @ `src/resolver.rs:1487`, `src/inline.rs:2343` | suffix-absence miss-cache + invalidating-cursor | inactive `[...]` bodies and inactive range halves are gated by the `]` half of `TimestampCloseScan`, after link/reference attempts. |
| Markdown keyword/range timestamp @ `src/resolver.rs:1155`, `src/inline.rs:2330` | suffix-absence miss-cache + invalidating-cursor | keyword active/inactive bodies reuse the delimiter-specific timestamp cursor; range second-half probes are transactional clones, committed only on a successful range so fallback stays O(n). |
| Markdown email local/domain @ `src/resolver.rs:1890`, `src/inline.rs:1568` | suffix-absence miss-cache + invalidating-cursor | local-part keeps the `@` absence floor; domain uses a first `>`/ws cursor. |
| Markdown raw HTML angle @ `src/resolver.rs:1883`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | Missing closers use `RawHtmlScan`; unbalanced tag matching uses the parse-local raw-HTML tag index below, so total work is the sum of one charged per-tag build plus charged cursor/rank queries. |
| Markdown bare URL dispatch @ `src/resolver.rs:1161`, `src/inline.rs:1749` | consume-on-match + suffix-absence miss-cache | accepted URLs consume their span; all-alphanumeric no-scheme suffixes are floored by `BareUrlScan`. |
| Markdown resync lead checks @ `src/resolver.rs:1812` | invalidating-cursor | split-token bare-url probes share `BareUrlScan`; fast path re-lexes only the split token. |
| Org resolver token loop @ `src/org_resolver.rs:953` | consume-on-match | `t` advances monotonically; accepted leaves resync past consumed bytes. |
| Org bracket/page/link floors @ `src/org_resolver.rs:806`, `src/org_resolver.rs:1083`, `src/org_resolver.rs:1786` | precomputed-map + invalidating-cursor | bracket close maps plus `rbracket`, `sq_rb_lb`, `real_dbl_cur`, and `crlf` gate scans. |
| Org macro/block-ref dispatch @ `src/org_resolver.rs:1167`, `src/org_resolver.rs:1241` | suffix-absence miss-cache | `sq_rbrace` and `sq_rr` must be present before parsers run. |
| Org export-snippet dispatch @ `src/org_resolver.rs:1187`, parser @ `src/inline.rs:2266` | suffix-absence miss-cache + consume-on-match | `sq_at` proves a closing `@@` after the opener before the parser runs; accepted snippets consume through the closer, failed candidates are bounded by the first `@`/EOL. |
| Org tag dispatch @ `src/org_resolver.rs:974`, `src/inline.rs:573` | boundary-run | same delimiter-run precompute as Markdown. |
| Org angle target @ `src/org_resolver.rs:1635` | constant/local | `<<target>>` stops at `<`, `>`, or EOL and is tried once at the dispatch byte. |
| Org autolink @ `src/org_resolver.rs:1647`, `src/org.rs:2210` | suffix-absence miss-cache + invalidating-cursor | shared first `>`/ws cursor gates `parse_org_autolink`. |
| Org angle timestamp @ `src/org_resolver.rs:1693`, `src/inline.rs:2316` | suffix-absence miss-cache + invalidating-cursor | active `<...>` bodies and active range halves are gated by the `>` half of `TimestampCloseScan`. |
| Org inactive bracket timestamp @ `src/org_resolver.rs:1833`, `src/inline.rs:2343` | suffix-absence miss-cache + invalidating-cursor | inactive `[...]` bodies and inactive range halves are gated by the `]` half of `TimestampCloseScan`, after org link attempts. |
| Org keyword/range timestamp @ `src/org_resolver.rs:890`, `src/inline.rs:2330` | suffix-absence miss-cache + invalidating-cursor | keyword active/inactive bodies reuse the delimiter-specific timestamp cursor; range second-half probes are transactional clones, committed only on a successful range so fallback stays O(n). |
| Org raw HTML angle @ `src/org_resolver.rs:1704`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | Same `RawHtmlScan` and unbalanced tag index as Markdown: one charged per-tag build per input string plus charged cursor/rank queries. |
| Org email domain @ `src/org_resolver.rs:1709`, `src/inline.rs:1568` | suffix-absence miss-cache + invalidating-cursor | same email `@` floor plus domain boundary cursor as Markdown. |
| Org bare URL dispatch/resync @ `src/org_resolver.rs:896`, `src/org_resolver.rs:1568`, `src/inline.rs:1749` | consume-on-match + suffix-absence miss-cache | accepted URLs consume; bounded split-token resync probes share `BareUrlScan`. |
| Org emphasis body parser @ `src/org_resolver.rs:370`, dispatch @ `src/org_resolver.rs:486` | consume-on-match + suffix-absence miss-cache | Org uses the same mldoc body parser with `include_md_code=false`; `no_closer[class][k]` floors EOF/no-closer failures. |
| Org script braced scan/fallback @ `src/org_resolver.rs:933`, body reparse @ `src/org_resolver.rs:1360` | `}`-before-eol invalidating-cursor + consume-on-match | Braced `_`/`^` attempts are gated by `ByteBeforeEolScan`; when absent, Org falls straight to the p1 `non_space` run. Accepted braced or p1 spans bound the body reparse. |
| Org entity dispatch @ `src/org_resolver.rs:1380`, top-level @ `src/org_resolver.rs:1477` | consume-on-match | Entity handling consumes the maximal ASCII-letter run and optional `{}` for both known and unknown names; unknown names consume the run and emit bare `Plain(name)`. |
| `parse_tag_name` capture/reparse @ `src/inline.rs:573`, `src/inline.rs:659` | consume-on-match + boundary-run | captured tag bytes are consumed once; delimiter suffixes use the boundary map, then the bounded tag string is reparsed. |
| Tag nested/page/link reparse @ `src/inline.rs:716`, `src/org_resolver.rs:655` | consume-on-match in tag | successful nested/page/Markdown/Org links advance the tag reparse cursor; top-level bracket retries are separately gated by maps. |
| Macro arg scans @ `src/inline.rs:784` | consume-on-match | scans are limited to an already accepted macro body. |
| Markdown `markdown_embed_image` data branch @ `src/inline.rs:946` | consume-on-match under md-link floors | The `data:` scan runs only after `try_md_link` proves `](` and a `)`; success consumes through that `)`, failure is bounded by the same candidate. |
| Markdown `label_part` @ `src/inline.rs:1013` | consume-on-match under md-link floors | Reached only after `try_md_link` proves a same-line `](`; label chunks, code spans, page refs, and bracket chunks advance the local label cursor monotonically to that delimiter. |
| Markdown label `string_contains_balanced_brackets` @ `src/inline.rs:1212`, `src/inline.rs:1847` | bounded-by-label-candidate | The iterative helper is called only inside the already-floored label span and advances one local cursor; unmatched-left fallback does not scan past the label candidate. |
| Markdown `link_url_part` @ `src/inline.rs:1310` | bounded-by-url-candidate | The balanced-paren scan is called only after the resolver has a forward `)` floor; it advances to that candidate or to an eol stop, with no retry from later bytes. |
| Markdown `link_url_part_inner` URL/title reparse @ `src/inline.rs:1333` | bounded-by-url-candidate | It reparses only the raw string returned by `link_url_part`; URL pieces, quoted-title scans, and parse-failure fallback are one pass over that bounded candidate. |
| Markdown label Plain reparse @ `src/resolver.rs:67` | bounded disjoint label spans | Each Plain label node is reparsed once with the C1 emphasis port plus latex/entity/code/script choices; consume-all failure keeps that one Plain chunk, so chunks are not rescanned. |
| Markdown link metadata @ `src/inline.rs:1458` | consume-on-match/current-line | `{...}` metadata is checked immediately after an accepted link/image and scans only to `}` before eol; absence is constant at the current end byte. |
| Org `org_link_1` URL scan @ `src/org_resolver.rs:1862` | bounded by org bracket floors | `try_bracket_at` reaches this path only with the `][` floor; the URL scan advances once to the candidate `]` and accepts escaped `]` without retrying prior bytes. |
| Org `org_link_1` label scan @ `src/org_resolver.rs:1952` | bounded by org `]]`/eol floors | The label scan advances one cursor to the caller-proved closing `]]`; single `]` is consumed as label text unless it is that final closer. |
| Org label reparse/full reconstruction @ `src/org_resolver.rs:1873` | bounded disjoint label span | The label string is reparsed once with `Ctx::label()`; the full-text first-Plain-only quirk reads only the first produced node. |
| Org link metadata @ `src/org_resolver.rs:2035` | consume-on-match/current-line | Same metadata parser as Markdown in placement: it runs only after an accepted link and scans to `}` before eol. |
| Autolink parser body @ `src/inline.rs:1473` | invalidating-cursor owned by caller | parser may scan to `>`/ws, but dispatch only calls it after the shared boundary cursor succeeds. |
| Email parser body @ `src/inline.rs:1568` | suffix-absence miss-cache + invalidating-cursor | cached entry point owns both local `@` absence and domain boundary. |
| Bare URL path balance @ `src/inline.rs:1816`, `src/inline.rs:1826` | consume-on-match | the balanced tail is part of the emitted URL span and advances a single local cursor. |
| LaTeX backslash/dollar @ `src/inline.rs:2134`, `src/inline.rs:2155` | invalidating-cursor / `$`-before-eol invalidating-cursor + charged first-`$` body scan | Backslash closers are gated by resolver `\)`/`\]` cursors; top-level dollar dispatch is gated by `ByteBeforeEolScan`. The displayed `$$` body scan is the mldoc first-`$`/EOL scan, charged with `metrics::scan_work`; success consumes through the literal `$$`, and failure emits one `$` before re-dispatch at the next byte, so the failed displayed scan plus the fallback inline retry is bounded by a constant number of passes over that segment. Label reparses are bounded by the accepted label span. |
| Timestamp body slots @ `src/inline.rs:2375`, token boundary cursor @ `src/inline.rs:300` | invalidating-cursor owned by caller | after a cursor-owned close candidate, delimiter-specific token-boundary cursors own the date/time/repetition slot scans; exact body spacing and two-slot interpretation are bounded local work with no `split_whitespace` suffix rescans or caps. |
| Raw HTML head @ `src/block_common.rs:406` | constant | known tag token scan is bounded by `MAX_HTML_TAG_LEN = 10`. |
| Raw HTML special closer @ `src/block_common.rs:497`, `src/block_common.rs:530` | suffix-absence miss-cache | missing special closers update `RawHtmlScan.no_special_until`. |
| Raw HTML missing tag closer @ `src/block_common.rs:530`, `src/block_common.rs:614` | suffix-absence miss-cache | no `</tag>` ahead updates `RawHtmlScan.no_tag_end_until[index]`. |
| Raw HTML tag-index build @ `src/block_common.rs:200` | precomputed-map + exact-case cache | Each `RawHtmlScan` is tied to one input string. A queried exact-case tag token builds one combined pass over the whole input; opens stay exact-case and closes stay case-insensitive. Summation: `Σ builds <= T * n`, where `T` is the bounded universe of HICCUP-recognized exact-case ASCII tag tokens of length `<= 10`, so `T` is a parse-independent constant. |
| Raw HTML tag-index query @ `src/block_common.rs:200`, `src/block_common.rs:530` | monotone cursor + charged window ranks | `after_tag` queries are non-decreasing per `RawHtmlScan`, so per-tag event/close/self-close cursors advance at most once over their lists: `Σ cursor work <= events + queries`. A `(tag, body_end)` rank entry is cached and built by charged binary probes plus `O(max_tag_pattern_len)` tail correction; `Σ rank work <= distinct queried body windows * O(log events)`, with the nested-frame maximizer covered by the raw-HTML gate and all probes charged to `scan_work`. |
| Raw HTML self-close fallback @ `src/block_common.rs:200` | shared precomputed-map | `/>` positions are stored in the same per-tag index. Query work shares the monotone self-close cursor and the charged body-window fit rank above, so `Σ self-close work <= self-close events + queries + rank probes`. |
| Raw HTML accepted capture mapping @ `src/block_common.rs:668`, `src/block_common.rs:708` | consume-on-match | line/view mapping scans only accepted raw-HTML extents and consumed trailing blanks. |

Documented non-inline exceptions:

| exception @ file:line | bound | note |
|---|---|---|
| refs sort @ `src/refs.rs:20` | `O(R log R)` | reference count `R <= input bytes`; this is outside inline parsing. |
| GT fallback recursion cap @ `src/block_common.rs:33` | capped | `GT_FALLBACK_NEST_CAP = 64` applies only to the residual transformed quote fallback. |

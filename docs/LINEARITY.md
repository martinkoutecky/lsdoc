# Inline Linearity Contract

This file is the standing O(n) ownership map for inline-phase forward scans. Each scan
must have exactly one owner: constant/local, consume-on-match, suffix-absence miss-cache,
invalidating cursor, precomputed map, or boundary-run map. A new unfloored scan in these
paths is a bug.

Line numbers were rechecked against the current tree after Phase B leaf-linearity work.

| scan @ file:line | owner | argument |
|---|---|---|
| Markdown resolver token loop @ `src/resolver.rs:134` | consume-on-match | `t` advances monotonically; successful leaves resync past consumed bytes. |
| Markdown page-ref `]]` lookup @ `src/resolver.rs:216` | precomputed-map | `build_real_dbl` positions are shared with a monotone cursor. |
| Markdown hiccup/nested close lookup @ `src/resolver.rs:199` | precomputed-map | `build_hiccup_close` and `build_nested_close` give O(1) close checks. |
| Markdown md-link `](`/`)` floors @ `src/resolver.rs:906`, `src/resolver.rs:875` | invalidating-cursor | `lbp_cur`, `crlf`, and `rparen` advance only forward before `md_link`. |
| Markdown emphasis body parser @ `src/resolver.rs:255`, dispatch @ `src/resolver.rs:432` | consume-on-match + suffix-absence miss-cache | mldoc `md_em_parser` consumes each body byte once per bounded nesting phase; `no_closer[class][k]` floors EOF/no-closer failures. |
| Markdown tag dispatch @ `src/resolver.rs:292`, `src/inline.rs:219` | boundary-run | delimiter-run termination is precomputed once by `build_tag_boundary_runs`. |
| Markdown macro dispatch @ `src/resolver.rs:846`, `src/inline.rs:1844` | suffix-absence miss-cache + invalidating-cursor | `}}` floor proves close presence; first lone `}` cursor prevents repeated invalid misses. |
| Markdown block-ref dispatch @ `src/resolver.rs:860`, `src/inline.rs:1809` | suffix-absence miss-cache + invalidating-cursor | `))` floor proves close presence; first lone `)` cursor owns body-invalid failures. |
| Markdown angle autolink @ `src/resolver.rs:810`, `src/inline.rs:1206` | suffix-absence miss-cache + invalidating-cursor | one cursor owns first `>`/ws after `<scheme:`; EOF and ws-before-`>` are cached outcomes. |
| Markdown angle timestamp @ `src/resolver.rs:810`, `src/inline.rs:1903` | suffix-absence miss-cache + invalidating-cursor | timestamp date parsing is gated by a monotone first `>`/LF cursor. |
| Markdown email local/domain @ `src/resolver.rs:810`, `src/inline.rs:1288` | suffix-absence miss-cache + invalidating-cursor | local-part keeps the `@` absence floor; domain uses a first `>`/ws cursor. |
| Markdown raw HTML angle @ `src/resolver.rs:810`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | missing closers use `RawHtmlScan`; unbalanced tag matching uses a lazy per-tag/body index. |
| Markdown bare URL dispatch @ `src/resolver.rs:444`, `src/inline.rs:1407` | consume-on-match + suffix-absence miss-cache | accepted URLs consume their span; all-alphanumeric no-scheme suffixes are floored. |
| Markdown resync lead checks @ `src/resolver.rs:725` | invalidating-cursor | split-token bare-url probes share `BareUrlScan`; fast path re-lexes only the split token. |
| Org resolver token loop @ `src/org_resolver.rs:270` | consume-on-match | `t` advances monotonically; accepted leaves resync past consumed bytes. |
| Org bracket/page/link floors @ `src/org_resolver.rs:337`, `src/org_resolver.rs:1141` | precomputed-map + invalidating-cursor | bracket close maps plus `rbracket`, `sq_rb_lb`, `real_dbl_cur`, and `crlf` gate scans. |
| Org macro/block-ref dispatch @ `src/org_resolver.rs:550`, `src/org_resolver.rs:558` | suffix-absence miss-cache | `sq_rbrace` and `sq_rr` must be present before parsers run. |
| Org tag dispatch @ `src/org_resolver.rs:493`, `src/inline.rs:219` | boundary-run | same delimiter-run precompute as Markdown. |
| Org angle target @ `src/org_resolver.rs:975` | constant/local | `<<target>>` stops at `<`, `>`, or EOL and is tried once at the dispatch byte. |
| Org autolink @ `src/org_resolver.rs:975`, `src/org.rs:2210` | suffix-absence miss-cache + invalidating-cursor | shared first `>`/ws cursor gates `parse_org_autolink`. |
| Org timestamp @ `src/org_resolver.rs:975`, `src/inline.rs:1903` | suffix-absence miss-cache + invalidating-cursor | shared timestamp close cursor gates angle timestamps. |
| Org raw HTML angle @ `src/org_resolver.rs:975`, `src/block_common.rs:614` | suffix-absence miss-cache + precomputed-map | same `RawHtmlScan` and unbalanced tag index as Markdown. |
| Org email domain @ `src/org_resolver.rs:975`, `src/inline.rs:1288` | suffix-absence miss-cache + invalidating-cursor | same email `@` floor plus domain boundary cursor as Markdown. |
| Org bare URL dispatch/resync @ `src/org_resolver.rs:367`, `src/org_resolver.rs:860` | consume-on-match + suffix-absence miss-cache | accepted URLs consume; resync lead probes share `BareUrlScan`. |
| Org emphasis body parser @ `src/org_resolver.rs:370`, dispatch @ `src/org_resolver.rs:486` | consume-on-match + suffix-absence miss-cache | Org uses the same mldoc body parser with `include_md_code=false`; `no_closer[class][k]` floors EOF/no-closer failures. |
| `parse_tag_name` body @ `src/inline.rs:390` | consume-on-match + boundary-run | main bytes are consumed into the tag; delimiter suffixes use the boundary map. |
| Tag nested/page refs @ `src/inline.rs:454` | precomputed-map at top level, consume-on-match in tag | successful refs advance tag cursor; top-level bracket retries are gated by maps. |
| Macro arg scans @ `src/inline.rs:569` | consume-on-match | scans are limited to an already accepted macro body. |
| Markdown link label/destination/title @ `src/inline.rs:650` | consume-on-match | reached only after resolver floors prove `](` and `)`; accepted link consumes the tail. |
| Autolink parser body @ `src/inline.rs:1143` | invalidating-cursor owned by caller | parser may scan to `>`/ws, but dispatch only calls it after the shared boundary cursor succeeds. |
| Email parser body @ `src/inline.rs:1288` | suffix-absence miss-cache + invalidating-cursor | cached entry point owns both local `@` absence and domain boundary. |
| Bare URL path balance @ `src/inline.rs:1492` | consume-on-match | the balanced tail is part of the emitted URL span. |
| LaTeX backslash/dollar @ `src/inline.rs:1759`, `src/inline.rs:1776` | invalidating-cursor / current-line | backslash closers are gated by resolver `\)`/`\]` cursors; dollar scans stop at current line and consume on success. |
| Timestamp date body @ `src/inline.rs:1959` | invalidating-cursor owned by caller | angle timestamps are gated by `TimestampCloseScan`; accepted keyword/inactive timestamps consume their date span. |
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

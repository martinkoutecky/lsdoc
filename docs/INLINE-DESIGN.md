# Inline Design

lsdoc's inline phase is a source-mirrored, byte-offset-driven port of mldoc's
`syntax/inline.ml`. The shared leaf parsers live in `src/inline.rs`; the two
format drivers live in `src/resolver.rs` and `src/org_resolver.rs`.

## Driver Model

The top-level drivers read one token stream left to right. Each successful construct emits its
node, then `resync` advances the token cursor past the consumed byte extent. Failed constructs
fall back to plain text exactly like mldoc's `p <|> plain` at `inline.ml:1412`.

The main dispatch loops are ordered to match mldoc's `inline_choices`:

| mldoc source | lsdoc loop |
|---|---|
| Markdown `inline.ml:1342-1373` | `src/resolver.rs`, `match md_dispatch_byte(...)` |
| Org `inline.ml:1375-1410` | `src/org_resolver.rs`, `match org_dispatch_byte(...)` |

Each arm comment cites the corresponding `inline.ml` line. Bodies call the C1-C7 ported
construct functions: emphasis/script, links/images, timestamps, angle family, latex/entity,
hashtag/bare URL, macro/block-ref/footnote/cookie/export-snippet.

## Fresh And Swallow

The authoritative fresh/swallow model is documented once in `src/inline_driver.rs`, next to the
named delimiter data:

| table | role |
|---|---|
| `MARKDOWN_PLAIN_DELIMITER_BYTES` | one-byte Markdown plain fallback delimiters |
| `MARKDOWN_SWALLOW_BYTES` | Markdown failed dispatch bytes that enter a swallowed plain run |
| `ORG_PLAIN_DELIMITER_BYTES` | one-byte Org plain fallback delimiters |
| `ORG_SWALLOW_BYTES` | Org failed dispatch bytes that enter a swallowed plain run |

The design invariant from C6 defect 5 is that bare URLs and keyword timestamps are tried only at
mldoc iteration starts.

## Resync

`resync` is the consume-on-match owner. A construct returns a raw byte end, and the driver advances
from position to token cursor once. When a construct ends inside a token, the driver re-lexes only
the bounded split tail where possible. Markdown keeps the byte-exact fallback for remaining
`Leaf`/special-tail cases that are not proven unreachable; Org's old suffix reparse fallback was
deleted because Org straddles can only split an ordinary `Text` token.

## Linearity Families

Every forward scan has one owner, recorded in `docs/LINEARITY.md`:

| family | owner |
|---|---|
| accepted constructs and token cursor movement | consume-on-match |
| no closer ahead (`}}`, `))`, `@@`, raw HTML closers, bare URL no-scheme suffixes) | suffix-absence miss-cache |
| first invalidating closer/boundary (`)`, `}`, `>`, eol) | invalidating cursor |
| bracket/link/hiccup close maps | precomputed map |
| tag and URL delimiter runs | boundary-run map |

The rule for new inline code is simple: no scan lands without an owner.

# Source-oriented outline API

`lsdoc::parse_outline(input, format)` is the stable source contract for consumers
that need Logseq-flavored Markdown or Org outline structure while preserving the
original bytes. It returns:

```rust
Result<DocumentOutline, OutlineParseError>
```

`format == "org"` selects Org; every other value selects Markdown, matching
`lsdoc::parse`.

The outline is produced as a sidecar of the same v2 block-parser decisions that
build the public AST. It is not a raw-source scanner, it does not infer headers
from AST spans, it does not compute refs, and it does not parse the document a
second time. The block AST is currently built internally and discarded after
the parser-owned events have been collected.

## Header kinds

`OutlineHeaderKind` distinguishes the three source constructs in the contract:

- `MarkdownUnbulletedAtxHeading` — `# Heading` (including accepted indentation)
- `MarkdownDashBullet` — `  - Heading`
- `OrgHeadline` — `** Heading`

Only parser-accepted document-root outline headers are returned. Header-looking
text inside fenced code, literal/src/custom/quote containers, and regular-list
bodies is absent unless the v2 parser itself accepts it as a document-root
outline event.

## Exact source ranges

Every `OutlineHeader` has a parser `level` and three exact half-open UTF-8 byte
ranges into the original input:

| Field | Meaning |
|---|---|
| `header_start` | First byte of the parser-accepted structural marker, including accepted Markdown indentation |
| `structural_prefix` | Bytes a storage adapter removes before retaining block raw |
| `line_content` | Complete physical header line excluding its terminator |
| `line` | Complete physical header line including LF, CRLF, or lone CR; equal to `line_content` at EOF |

`header_start` locates the recognized structural header.
`structural_prefix.end` is the structural content start:

- For an unbulleted Markdown ATX heading `structural_prefix` is empty at the
  physical line start, preserving the entire heading line as block raw.
- For a Markdown dash bullet it contains indentation plus `-`, followed by
  exactly one ASCII space when present.
- For an Org headline it contains the leading stars, followed by exactly one
  ASCII space when present.

This boundary deliberately precedes lsdoc's semantic title boundary. TODO/task
markers, Org priorities, extra whitespace, and an embedded ATX heading such as
`- # Heading` remain in the retained raw bytes.

`SourceRange::slice(input)` applies a range without panicking and returns `None`
for out-of-bounds offsets or invalid UTF-8 boundaries. A `SourceRange` does not
carry input identity or provenance: if the same byte offsets are valid for a
different string, `slice` returns that different string's corresponding slice.

## Example

```rust
use lsdoc::{parse_outline, OutlineHeaderKind};

let input = "# Root\r\n  - TODO [#A] café";
let outline = parse_outline(input, "md")?;

assert_eq!(
    outline.headers[0].kind,
    OutlineHeaderKind::MarkdownUnbulletedAtxHeading
);
assert_eq!(outline.headers[0].line.slice(input), Some("# Root\r\n"));
assert_eq!(
    outline.headers[1].structural_prefix.slice(input),
    Some("  - ")
);
# Ok::<(), lsdoc::OutlineParseError>(())
```

The API returns `OutlineParseError::ParserOwnership` instead of panicking if the
production v2 parser unexpectedly fails to own an input.

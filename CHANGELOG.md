# Changelog

Releases before 0.5.12 are described in their release commits and in `DIVERGENCES.md`.

## [0.5.12]

### Added
- `property_references(value, format)`: mldoc's `Property.property_references` (Logseq's
  `getReferences`), the whole-value inline parse with macros disabled that decides a property
  value's references. Spans index the value. A value without `{{` reads exactly as `inline`.
- `parse_format_without_property_page_refs(input, format)`: `parse_format` whose `refs.page`
  leaves out property-value page references (block references stay), for a consumer that reads
  property values with `property_references` itself.
- Harness: `propgate.mjs`, a property-reference gate against mldoc `getReferences`, one oracle
  process per value, over `corpus.property.json` (165 values × md, org); part of `run.mjs`.

### Unchanged
- Every existing entry point returns what it did in 0.5.11. The gate's three known
  differences are D51 in `DIVERGENCES.md`.

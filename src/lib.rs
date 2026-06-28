//! lsdoc — a native-Rust parser for Logseq-flavored Markdown (and, later, Org)
//! into a typed, serde-serializable AST with source spans, behavior-equivalent to
//! Logseq's `mldoc` at the granularity that matters for indexing and rendering.
//!
//! See `SPEC.md` for the brief, `DECISIONS.md` for the design log, and `README.md`
//! for the oracle/harness. The crate is built milestone by milestone; modules are
//! added as each lands (block structure, inline core, dialect inline, …).

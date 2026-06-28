//! The "observable projection": the lossy, comparison-only view of a parse that
//! is diffed against mldoc (the oracle). Its `serde` output must match
//! `harness/lib/normalize.mjs` exactly (key names + value shapes), so the Node
//! `compare.mjs` can deep-equal the two sides.
//!
//! This is NOT lsdoc's real AST (which is richer and carries inline spans). Once
//! the real parser lands (M2+), a `project()` step maps real AST → these types.
//! For now the stub parser builds these directly.

use serde::Serialize;

/// One input's parse result: block tree + OG-faithful ref set.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Projection {
    pub blocks: Vec<Block>,
    pub refs: Refs,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Refs {
    pub page: Vec<String>,
    pub block: Vec<String>,
}

/// Block source span `[start, end]` (byte offsets). Serializes as a 2-array to
/// match mldoc's `{start_pos,end_pos}` after normalization.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Span(pub usize, pub usize);

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind")]
pub enum Block {
    #[serde(rename = "paragraph")]
    Paragraph {
        inline: Vec<Inline>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "heading")]
    Heading {
        level: u32,
        size: Option<u32>,
        inline: Vec<Inline>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Logseq outline bullet — mldoc emits these as `Heading{unordered:true}`.
    #[serde(rename = "bullet")]
    Bullet {
        level: u32,
        inline: Vec<Inline>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "list")]
    List {
        items: Vec<ListItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "src")]
    Src {
        lang: String,
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "quote")]
    Quote {
        children: Vec<Block>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Callout block `#+BEGIN_X … #+END_X` (X != QUOTE). mldoc emits `Custom`.
    #[serde(rename = "custom")]
    Custom {
        name: String,
        children: Vec<Block>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "raw_html")]
    RawHtml {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Block-level `$$ … $$` (mldoc `Displayed_Math`). Inline `$$…$$` mixed with
    /// text is a `Latex` inline instead.
    #[serde(rename = "displayed_math")]
    DisplayedMath {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Org-style drawer `:NAME: … :END:` (e.g. `:LOGBOOK:`). Content is opaque —
    /// compared on `name` only (see DECISIONS.md).
    #[serde(rename = "drawer")]
    Drawer {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "properties")]
    Properties {
        props: Vec<(String, String)>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "hr")]
    Hr {
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "table")]
    Table {
        header: Option<Vec<Vec<Inline>>>,
        rows: Vec<Vec<Vec<Inline>>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    #[serde(rename = "footnote_def")]
    FootnoteDef {
        name: String,
        inline: Vec<Inline>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ListItem {
    pub ordered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    pub indent: u32,
    pub content: Vec<Block>,
    pub items: Vec<Block>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "k")]
pub enum Inline {
    #[serde(rename = "plain")]
    Plain { text: String },
    #[serde(rename = "code")]
    Code { text: String },
    #[serde(rename = "verbatim")]
    Verbatim { text: String },
    #[serde(rename = "break")]
    Break,
    #[serde(rename = "hardbreak")]
    HardBreak,
    #[serde(rename = "emphasis")]
    Emphasis { emph: String, children: Vec<Inline> },
    #[serde(rename = "link")]
    Link {
        url: Url,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        label: Vec<Inline>,
        full: String,
    },
    #[serde(rename = "nested_link")]
    NestedLink { content: String },
    #[serde(rename = "tag")]
    Tag { children: Vec<Inline> },
    #[serde(rename = "macro")]
    Macro { name: String, args: Vec<String> },
    #[serde(rename = "latex")]
    Latex { mode: String, body: String },
    #[serde(rename = "timestamp")]
    Timestamp { ts: String, date: serde_json::Value },
    #[serde(rename = "fnref")]
    Fnref { name: String },
    /// Inline raw HTML, e.g. `<span class="x">…</span>` (mldoc `Inline_Html`).
    #[serde(rename = "inline_html")]
    InlineHtml { text: String },
    /// Email autolink `<a@b.com>` (mldoc `Email`); payload is the raw address obj.
    #[serde(rename = "email")]
    Email { text: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum Url {
    #[serde(rename = "page_ref")]
    PageRef { v: String },
    #[serde(rename = "block_ref")]
    BlockRef { v: String },
    #[serde(rename = "search")]
    Search { v: String },
    #[serde(rename = "file")]
    File { v: String },
    #[serde(rename = "complex")]
    Complex {
        protocol: Option<String>,
        link: Option<String>,
    },
}

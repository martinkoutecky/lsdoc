//! lsdoc's AST — the render-complete, `serde`-serializable parse tree, re-exported
//! as the public, frozen [`crate::ast`] (see that module + `AST.md` for the wire
//! contract). It is **both** the integration surface Tine renders from AND the
//! "observable projection" diffed against mldoc: its `serde` output matches
//! `harness/lib/normalize.mjs` exactly (key names + value shapes), so the Node
//! `compare.mjs` can deep-equal the two sides, gating every field to 0-diff.
//!
//! (Earlier this was framed as a lossy "comparison-only" view distinct from a
//! richer real AST. That framing is retired: render-relevant fields are carried
//! and gated; the only excluded detail is inline source spans — out of scope for a
//! read-only renderer, verified by lsdoc's own unit tests. See `DECISIONS.md`.)

use serde::{Deserialize, Serialize};

/// One input's parse result: block tree + OG-faithful ref set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Projection {
    pub blocks: Vec<Block>,
    pub refs: Refs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Refs {
    pub page: Vec<String>,
    pub block: Vec<String>,
}

/// Block source span `[start, end]` (byte offsets). Serializes as a 2-array to
/// match mldoc's `{start_pos,end_pos}` after normalization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Span(pub usize, pub usize);

/// `serde` `skip_serializing_if` for `bool` fields that default to `false`.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Per-column table alignment, parsed from the separator row (`:--`/`--:`/`:-:`).
/// An lsdoc-only render enrichment used by [`crate::render_html`]'s `data-align`
/// (mldoc discards table alignment); the differential gate drops it like `span`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Parse a table separator row into per-column alignment. Splits on `|` (the
/// column delimiter for both markdown and org), reading a leading/trailing `:`
/// per cell: `:--`→Left, `--:`→Right, `:-:`→Center, `---`→`None` (unaligned).
/// A cell with no `-` (`:`, `::`) is NOT an alignment → `None` (mldoc/GitHub
/// require a `-` in a separator cell — fix D). Markdown-only now: org's
/// `build_table` no longer calls this (fix C).
pub(crate) fn parse_separator_aligns(sep: &str) -> Vec<Option<Align>> {
    let t = sep.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|')
        .map(|c| {
            let c = c.trim();
            // Fix D: a separator cell must contain at least one `-` to be an alignment
            // (mldoc/GitHub require a `-`); a colon-only cell (`:`, `::`) is NOT an
            // alignment → `None`, not Center.
            if !c.contains('-') {
                return None;
            }
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Some(Align::Center),
                (false, true) => Some(Align::Right),
                (true, false) => Some(Align::Left),
                (false, false) => None,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        /// task marker (TODO/DOING/DONE/…), org `[#A]` priority, org `:tags:`.
        #[serde(skip_serializing_if = "Option::is_none")]
        marker: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        priority: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        htags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Outline bullet (md `-`) / org headline (`*`) — mldoc `Heading{unordered:true}`.
    #[serde(rename = "bullet")]
    Bullet {
        level: u32,
        /// Heading level when the bullet body is an ATX heading (`- ## Title` → 2),
        /// the `#`-count (uncapped); `None` for a non-heading bullet. Mirrors
        /// `Heading.size` so a renderer can show a block-authored heading.
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
        inline: Vec<Inline>,
        #[serde(skip_serializing_if = "Option::is_none")]
        marker: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        priority: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        htags: Vec<String>,
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
    /// Org keyword line `#+KEY: value` (mldoc `Directive`).
    #[serde(rename = "directive")]
    Directive {
        name: String,
        value: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Org comment line `# text` (mldoc `Comment`). `text` is the raw content after
    /// `#` + spaces (leading stripped, trailing kept); not inline-parsed, not rendered.
    #[serde(rename = "comment")]
    Comment {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// Org `#+BEGIN_EXAMPLE … #+END_EXAMPLE` literal block (mldoc `Example`).
    #[serde(rename = "example")]
    Example {
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
    /// LaTeX environment block `\begin{X} … \end{X}` (mldoc `Latex_Environment`).
    /// mldoc shape: `["Latex_Environment", name, null, content]` (name lowercased).
    #[serde(rename = "latex_env")]
    LatexEnv {
        name: String,
        content: String,
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
        /// Per-column alignment parsed from the markdown `:--`/`--:`/`:-:` (or org)
        /// separator row. An **lsdoc-only render enrichment**: mldoc 1.5.7 discards
        /// alignment, so this is excluded from the differential gate exactly like
        /// `span` (`harness/compare.mjs` + `harness/blockgate.mjs` drop the key).
        /// `None` = no separator row / no `:` markers; each column is `Some` only
        /// when that column carried a marker. Modeled on `Span` as a gate-dropped field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aligns: Option<Vec<Option<Align>>>,
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
    /// Block-level Clojure-hiccup vector `[:tag …]` occupying a whole line (mldoc
    /// `Hiccup`). `v` is the RAW bracket text verbatim (mldoc does NOT parse the
    /// children); a renderer treats it opaquely. See `AST.md`.
    #[serde(rename = "hiccup")]
    Hiccup {
        v: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Span>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListItem {
    pub ordered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    pub indent: u32,
    pub content: Vec<Block>,
    /// Nested child items (mldoc nests a deeper-indented item into the preceding
    /// item's `items` sub-array). Built by `nest_items` from the flat line sequence.
    pub items: Vec<ListItem>,
    /// Markdown definition-list term (`term\n: def` → item `name`). Empty for all
    /// other list items (mldoc emits `name: []`, cleaned away on both sides).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub name: Vec<Inline>,
    /// Task checkbox state: `[ ]`→`Some(false)`, `[x]`/`[X]`→`Some(true)`, none→`None`.
    /// mldoc records it on `*`/`+`/`N.` (md) and `-`/`+`/`N.` (org) list items; md `-`
    /// bullets are `Block::Bullet`, never list items, so never carry a checkbox.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkbox: Option<bool>,
}

/// Fold a flat, in-order sequence of list items (each carrying its `indent`, `items`
/// empty) into mldoc's nested tree. Rule, verified against mldoc over 40k random
/// md+org inputs: an item's children are the maximal following run whose indent is
/// **≥ the FIRST child's indent**; a shallower item unwinds the stack fully (it may
/// rejoin an ancestor's run or become a top-level sibling). E.g. `a(0) deep(4) mid(2)`
/// → `mid` is a top-level sibling of `a` (NOT a child), because `mid`'s indent (2) is
/// below `deep`'s child-run floor (4). Iterative (explicit stack), single-pass O(n),
/// no recursion — deep nesting can't overflow the stack.
pub fn nest_items(flat: Vec<ListItem>) -> Vec<ListItem> {
    struct Frame {
        item: ListItem,
        children: Vec<ListItem>,
        child_min_indent: u32,
    }
    let n = flat.len();
    let indents: Vec<u32> = flat.iter().map(|it| it.indent).collect();
    let mut roots: Vec<ListItem> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    let push_done = |item: ListItem, stack: &mut Vec<Frame>, roots: &mut Vec<ListItem>| {
        match stack.last_mut() {
            Some(parent) => parent.children.push(item),
            None => roots.push(item),
        }
    };

    for (i, mut cur) in flat.into_iter().enumerate() {
        // Close frames whose child-run `cur` is too shallow to join.
        while stack.last().is_some_and(|top| cur.indent < top.child_min_indent) {
            let f = stack.pop().unwrap();
            let mut done = f.item;
            done.items = f.children;
            push_done(done, &mut stack, &mut roots);
        }
        cur.items = Vec::new();
        if i + 1 < n && indents[i + 1] > cur.indent {
            // `cur` opens a child run floored at the next item's indent.
            stack.push(Frame { item: cur, children: Vec::new(), child_min_indent: indents[i + 1] });
        } else {
            push_done(cur, &mut stack, &mut roots);
        }
    }
    while let Some(f) = stack.pop() {
        let mut done = f.item;
        done.items = f.children;
        push_done(done, &mut stack, &mut roots);
    }
    roots
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Org `_x_`/`_{x}` subscript and `^x`/`^{x}` superscript (mldoc Subscript/
    /// Superscript). Inline content, re-parsed for nested emphasis/links.
    #[serde(rename = "subscript")]
    Subscript { children: Vec<Inline> },
    #[serde(rename = "superscript")]
    Superscript { children: Vec<Inline> },
    #[serde(rename = "link")]
    Link {
        url: Url,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        label: Vec<Inline>,
        full: String,
        /// `![…](…)` markdown image. mldoc carries **no** native image flag; both
        /// sides derive it from the leading `!` of `full`. Omitted when false.
        #[serde(default, skip_serializing_if = "is_false")]
        image: bool,
        /// Logseq media metadata, the raw `{:width … :height …}` text (mldoc's
        /// `metadata`, braces included). Omitted when empty (the common case).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        metadata: String,
        /// CommonMark link title `[l](url "title")` — the raw inner text (no quotes,
        /// not unescaped, matching mldoc). Omitted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "nested_link")]
    NestedLink { content: String },
    /// Org dedicated/radio target `<<name>>` (mldoc `Target`). The destination
    /// anchor for an internal org link; renders as its text.
    #[serde(rename = "target")]
    Target { text: String },
    #[serde(rename = "tag")]
    Tag { children: Vec<Inline> },
    #[serde(rename = "macro")]
    Macro { name: String, args: Vec<String> },
    #[serde(rename = "latex")]
    Latex { mode: String, body: String },
    /// `ts` ∈ {`Date`,`Range`,`Scheduled`,`Deadline`,`Closed`}; `date` is mldoc's
    /// raw date/range record, **declared opaque** for rendering (shape in `AST.md`).
    #[serde(rename = "timestamp")]
    Timestamp { ts: String, date: serde_json::Value },
    #[serde(rename = "fnref")]
    Fnref { name: String },
    /// Inline raw HTML, e.g. `<span class="x">…</span>` (mldoc `Inline_Html`).
    #[serde(rename = "inline_html")]
    InlineHtml { text: String },
    /// Email autolink `<a@b.com>` (mldoc `Email`); `text` is mldoc's raw address
    /// record, **declared opaque** for rendering (shape in `AST.md`).
    #[serde(rename = "email")]
    Email { text: serde_json::Value },
    /// LaTeX named entity `\Delta` / `\Delta{}` (mldoc `Entity`), resolved from the
    /// 339-entry table in `entities.rs`. Carries mldoc's full entity record.
    #[serde(rename = "entity")]
    Entity {
        name: String,
        latex: String,
        latex_mathp: bool,
        html: String,
        ascii: String,
        unicode: String,
    },
    /// Inline Clojure-hiccup vector `[:tag …]` mixed with text (mldoc `Inline_Hiccup`).
    /// `v` is the RAW bracket text verbatim (children unparsed). See `AST.md`.
    #[serde(rename = "hiccup")]
    Hiccup { v: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

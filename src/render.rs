//! Canonical AST→HTML renderer (M3 render contract).
//!
//! [`render_html`] turns an [`crate::ast::Block`] tree into the **static structural
//! skeleton** the Logseq frontend / Tine's static export both conform to: tags,
//! class names, nesting — and a `data-*` hook + RAW payload for everything that is
//! runtime/consumer-dependent (ref resolution, asset URLs, KaTeX/highlight/emoji,
//! macros, checkbox-toggle, iframe-trust). lsdoc owns **structure + classes +
//! `data-*` + (only) timestamp formatting**; it never resolves refs/assets/math/
//! macros, and it never emits untrusted HTML — `raw_html`/`inline_html` carry the
//! ESCAPED original in `data-raw` for the consumer to (re-)trust.
//!
//! Two in-render bonuses lsdoc *can* own (and does here): callout detection straight
//! from the AST (`Custom` callout blocks + a `Quote` whose first paragraph leads with
//! `[!TYPE]`), and Org timestamp formatting (it owns the date parse).
//!
//! The renderer is exhaustive: the `match` over every [`Block`] / [`Inline`] variant
//! has **no wildcard arm**, so a future AST variant is a compile error, not silent
//! empty output.

use crate::projection::{Align, Block, Inline, ListItem, Url};
use serde_json::Value;

/// Source format of the block tree being rendered. Drives only inline re-parsing of
/// property values (markdown vs org); the skeleton is otherwise format-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Md,
    Org,
}

/// Options for [`render_html`]. Minimal by design (no Tine-specific knobs).
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    pub format: Format,
}

impl RenderOpts {
    fn fmt_str(&self) -> &'static str {
        match self.format {
            Format::Md => "md",
            Format::Org => "org",
        }
    }
}

/// Render a block tree to the canonical HTML skeleton (see module docs).
pub fn render_html(blocks: &[Block], opts: &RenderOpts) -> String {
    let mut r = Renderer { out: String::new(), opts: *opts };
    r.blocks(blocks);
    r.out
}

// ===========================================================================
// Escaping — the security boundary. Every interpolated value routes through one of
// these two helpers. Text escapes `& < >`; attributes also escape `" '`.
// ===========================================================================

/// Append `s` HTML-escaped for **text** content (`& < >`).
fn esc_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// Append `s` HTML-escaped for an **attribute** value (`& < > " '`).
fn esc_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Append ` name="value"` with `value` attribute-escaped.
fn push_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    esc_attr(value, out);
    out.push('"');
}

// ===========================================================================

/// Org `#+BEGIN_X` callout block names that map to a `.callout` skeleton (the rest
/// render their children bare). Matches the frontend's `CALLOUT_TYPES`.
const CALLOUT_TYPES: &[&str] = &["note", "tip", "important", "caution", "warning", "pinned"];

struct Renderer {
    out: String,
    opts: RenderOpts,
}

/// A `paragraph`/`bullet`/`heading` — an inline-flow block. Consecutive inline-flow
/// blocks are `<br>`-joined (the frontend's `renderBlocks` line-stacked look).
fn is_inline_flow(b: &Block) -> bool {
    matches!(b, Block::Paragraph { .. } | Block::Bullet { .. } | Block::Heading { .. })
}

impl Renderer {
    /// Render a list of blocks, `<br>`-joining consecutive inline-flow blocks.
    fn blocks(&mut self, bs: &[Block]) {
        for (i, b) in bs.iter().enumerate() {
            if i > 0 && is_inline_flow(b) && is_inline_flow(&bs[i - 1]) {
                self.out.push_str("<br>");
            }
            self.block(b);
        }
    }

    fn block(&mut self, b: &Block) {
        match b {
            // Inline-flow bodies. A `paragraph` is a bare inline run. A `bullet`'s
            // marker/priority/htags are consumer outline chrome, but its `size` (a
            // block-authored heading, `- ## x`, the COMMON heading form in a bulleted
            // graph) IS rendered like a `heading` — the AST field exists for exactly this.
            Block::Paragraph { inline, .. } => self.inlines(inline),
            Block::Bullet { inline, size, .. } => match size {
                Some(s) => self.heading_text(*s, inline),
                None => self.inlines(inline),
            },
            Block::Heading { level, size, inline, .. } => {
                // Displayed heading level = the ATX/setext `size` (1–6); the AST `level`
                // is mldoc's outline nesting (always 1 for a standalone `#`-heading), so
                // `### x` has level=1, size=3. The catalog confirms the rendered level is
                // the size, not `level` (which the contract's `h{level}` shorthand glosses).
                self.heading_text(size.unwrap_or(*level), inline);
            }
            Block::List { items, .. } => {
                let mut cb = 0usize;
                self.list(items, &mut cb);
            }
            // Code: lsdoc does NOT highlight — escaped raw code + `data-lang`; the
            // consumer runs highlight.js. `example` has no language.
            Block::Src { lang, code, .. } => self.code_block(lang, code),
            Block::Example { code, .. } => self.code_block("", code),
            Block::Quote { children, .. } => self.quote(children),
            Block::Custom { name, children, .. } => self.custom(name, children),
            Block::Properties { props, .. } => self.properties(props),
            Block::Table { header, rows, aligns, .. } => self.table(header.as_deref(), rows, aligns.as_deref()),
            Block::Hr { .. } => self.out.push_str("<hr class=\"md-hr\">"),
            // Block-level math: empty element + raw tex hook (consumer renders KaTeX).
            Block::DisplayedMath { text, .. } => self.math_block(text),
            Block::LatexEnv { name, content, .. } => {
                self.math_block(&format!("\\begin{{{name}}}{content}\\end{{{name}}}"));
            }
            // raw HTML is never emitted live — escaped original in `data-raw`.
            Block::RawHtml { text, .. } => self.raw_html(text),
            Block::FootnoteDef { name, inline, .. } => {
                self.out.push_str("<div class=\"footnote-def\"><sup class=\"footnote-ref\">");
                esc_text(name, &mut self.out);
                self.out.push_str("</sup> ");
                self.inlines(inline);
                self.out.push_str("</div>");
            }
            Block::Hiccup { v, .. } => {
                self.out.push_str("<span class=\"ast-hiccup\">");
                esc_text(v, &mut self.out);
                self.out.push_str("</span>");
            }
            // Not rendered (match the frontend): org drawers / `#+KEY:` keywords /
            // `# comment` lines.
            Block::Drawer { .. } | Block::Directive { .. } | Block::Comment { .. } => {}
        }
    }

    /// A heading body: wrap in `heading-text h{h}` when `h ∈ 1..=6` (the frontend's
    /// `headingLevel` range, `facets.ts`), else a bare inline run — there is no CSS for
    /// `h7+` and the frontend emits no wrapper above 6. Shared by `Heading` and
    /// block-authored bullet headings (`- ## x`).
    fn heading_text(&mut self, h: u32, inline: &[Inline]) {
        if (1..=6).contains(&h) {
            self.out.push_str("<span class=\"heading-text h");
            self.out.push_str(&h.to_string());
            self.out.push_str("\">");
            self.inlines(inline);
            self.out.push_str("</span>");
        } else {
            self.inlines(inline);
        }
    }

    fn code_block(&mut self, lang: &str, code: &str) {
        self.out.push_str("<pre class=\"code-block\"><code class=\"hljs\"");
        push_attr(&mut self.out, "data-lang", lang);
        self.out.push('>');
        esc_text(code, &mut self.out);
        self.out.push_str("</code></pre>");
    }

    fn math_block(&mut self, tex: &str) {
        self.out.push_str("<span class=\"math math-display\"");
        push_attr(&mut self.out, "data-tex", tex);
        self.out.push_str("></span>");
    }

    fn raw_html(&mut self, text: &str) {
        self.out.push_str("<span class=\"raw-html\"");
        push_attr(&mut self.out, "data-raw", text);
        self.out.push_str("></span>");
    }

    fn properties(&mut self, props: &[(String, String)]) {
        if props.is_empty() {
            return;
        }
        self.out.push_str("<span class=\"block-properties\">");
        for (k, v) in props {
            self.out.push_str("<span class=\"block-property\"><span class=\"block-property-key\">");
            esc_text(k, &mut self.out);
            self.out.push_str("</span> <span class=\"block-property-val\">");
            // Property values are inline markup — render via lsdoc's own (format-aware)
            // inline parser. (One parser: this is `lsdoc::inline`, not a second scanner.)
            let inlines = crate::inline(v, self.opts.fmt_str());
            self.inlines(&inlines);
            self.out.push_str("</span></span>");
        }
        self.out.push_str("</span>");
    }

    fn table(&mut self, header: Option<&[Vec<Inline>]>, rows: &[Vec<Vec<Inline>>], aligns: Option<&[Option<Align>]>) {
        let align_attr = |out: &mut String, col: usize| {
            if let Some(Some(a)) = aligns.map(|a| a.get(col).copied().flatten()) {
                let v = match a {
                    Align::Left => "left",
                    Align::Center => "center",
                    Align::Right => "right",
                };
                push_attr(out, "data-align", v);
            }
        };
        self.out.push_str("<table class=\"md-table\">");
        if let Some(h) = header {
            self.out.push_str("<thead><tr>");
            for (col, cell) in h.iter().enumerate() {
                self.out.push_str("<th");
                align_attr(&mut self.out, col);
                self.out.push('>');
                self.inlines(cell);
                self.out.push_str("</th>");
            }
            self.out.push_str("</tr></thead>");
        }
        self.out.push_str("<tbody>");
        for row in rows {
            self.out.push_str("<tr>");
            for (col, cell) in row.iter().enumerate() {
                self.out.push_str("<td");
                align_attr(&mut self.out, col);
                self.out.push('>');
                self.inlines(cell);
                self.out.push_str("</td>");
            }
            self.out.push_str("</tr>");
        }
        self.out.push_str("</tbody></table>");
    }

    /// A `> [!TYPE]`-callout or a plain blockquote (detected from the AST, never by
    /// re-parsing a string: we inspect the first child paragraph's first plain inline).
    fn quote(&mut self, children: &[Block]) {
        let callout = match children.first() {
            Some(Block::Paragraph { inline, .. }) => match inline.first() {
                Some(Inline::Plain { text, .. }) => parse_callout_lead(text).map(|(ty, title)| (ty, title, inline)),
                _ => None,
            },
            _ => None,
        };
        if let Some((ty, title_text, lead_inline)) = callout {
            // Split the lead paragraph at the FIRST soft break: everything before it (the
            // `[!TYPE]` text remainder + any inline markup on the title line) is the TITLE;
            // everything after begins the BODY. (Previously only `[!TYPE]`'s first plain
            // segment was the title, so `[!NOTE] Heads **up**` spilled `**up**` into the body.)
            let break_idx =
                lead_inline[1..].iter().position(|n| matches!(n, Inline::Break { .. })).map(|p| p + 1);
            let (title_markup, body_inlines): (&[Inline], &[Inline]) = match break_idx {
                Some(k) => (&lead_inline[1..k], &lead_inline[k + 1..]),
                None => (&lead_inline[1..], &[]),
            };
            self.open_callout_title(&ty);
            if title_text.is_empty() && title_markup.is_empty() {
                esc_text(&ty.to_uppercase(), &mut self.out);
            } else {
                esc_text(&title_text, &mut self.out);
                self.inlines(title_markup);
            }
            self.out.push_str("</div><div class=\"callout-body\">");
            // Body rendered IN PLACE — NOT via `children[1..].cloned()` (deep-cloning the
            // subtree once per nesting level → O(n^2) on nested callouts). The body inlines
            // are a bare inline run (the lead paragraph's tail after the title break); the
            // remaining children follow with the same `<br>`-join `blocks()` applies (the lead
            // paragraph, when present, is the preceding inline-flow block).
            let lead_is_para = !body_inlines.is_empty();
            if lead_is_para {
                self.inlines(body_inlines);
            }
            let tail = &children[1..];
            for (i, b) in tail.iter().enumerate() {
                let prev_inline_flow = if i == 0 { lead_is_para } else { is_inline_flow(&tail[i - 1]) };
                if prev_inline_flow && is_inline_flow(b) {
                    self.out.push_str("<br>");
                }
                self.block(b);
            }
            self.out.push_str("</div></div>");
        } else {
            self.out.push_str("<blockquote class=\"md-quote\">");
            self.blocks(children);
            self.out.push_str("</blockquote>");
        }
    }

    fn custom(&mut self, name: &str, children: &[Block]) {
        let ty = name.to_ascii_lowercase();
        if CALLOUT_TYPES.contains(&ty.as_str()) {
            self.open_callout(&ty, &ty.to_uppercase());
            if !children.is_empty() {
                self.out.push_str("<div class=\"callout-body\">");
                self.blocks(children);
                self.out.push_str("</div>");
            }
            self.out.push_str("</div>");
        } else if ty == "quote" {
            self.out.push_str("<blockquote class=\"md-quote\">");
            self.blocks(children);
            self.out.push_str("</blockquote>");
        } else {
            // Unknown custom block: no wrapper, children rendered as-is (catalog #8).
            self.blocks(children);
        }
    }

    /// `<div class="callout callout-{ty}"><div class="callout-title">{title}</div>` —
    /// the shared open for Quote `[!TYPE]` and Custom callouts. Caller closes the body.
    /// Emit `<div class="callout callout-{ty}"><div class="callout-title">` — the caller
    /// renders the title content (text and/or inlines) then closes `</div>`.
    fn open_callout_title(&mut self, ty: &str) {
        self.out.push_str("<div class=\"callout callout-");
        esc_attr(ty, &mut self.out);
        self.out.push_str("\"><div class=\"callout-title\">");
    }

    /// A callout with a plain-text title (Custom callouts; `quote()`'s inline-title path
    /// opens it directly via `open_callout_title`).
    fn open_callout(&mut self, ty: &str, title: &str) {
        self.open_callout_title(ty);
        esc_text(title, &mut self.out);
        self.out.push_str("</div>");
    }

    /// A list (`md-list`/`md-list-item`); `cb` is the running depth-first checkbox
    /// ordinal for the whole list block (matches the frontend's per-list `cbItems`).
    fn list(&mut self, items: &[ListItem], cb: &mut usize) {
        let ordered = items.first().map(|i| i.ordered).unwrap_or(false);
        self.out.push_str(if ordered { "<ol class=\"md-list\">" } else { "<ul class=\"md-list\">" });
        for item in items {
            self.out.push_str("<li class=\"md-list-item");
            if item.checkbox.is_some() {
                self.out.push_str(" has-checkbox");
            }
            if !item.name.is_empty() {
                self.out.push_str(" has-term");
            }
            self.out.push_str("\">");
            if let Some(checked) = item.checkbox {
                self.out.push_str(if checked {
                    "<span class=\"block-checkbox checked\""
                } else {
                    "<span class=\"block-checkbox\""
                });
                push_attr(&mut self.out, "data-cb-index", &cb.to_string());
                self.out.push_str("></span> ");
                *cb += 1;
            }
            // Markdown definition-list term (`term\n: def`) — the item's label, rendered
            // (inline) before its definition body. Empty for ordinary list items.
            if !item.name.is_empty() {
                self.out.push_str("<span class=\"md-list-term\">");
                self.inlines(&item.name);
                self.out.push_str("</span> ");
            }
            self.blocks(&item.content);
            if !item.items.is_empty() {
                self.list(&item.items, cb);
            }
            self.out.push_str("</li>");
        }
        self.out.push_str(if ordered { "</ol>" } else { "</ul>" });
    }

    // ---- inline ----------------------------------------------------------

    fn inlines(&mut self, is: &[Inline]) {
        for i in is {
            self.inline(i);
        }
    }

    fn inline(&mut self, i: &Inline) {
        match i {
            Inline::Plain { text, .. } => esc_text(text, &mut self.out),
            Inline::Code { text, .. } | Inline::Verbatim { text, .. } => {
                self.out.push_str("<code class=\"inline-code\">");
                esc_text(text, &mut self.out);
                self.out.push_str("</code>");
            }
            Inline::Break { .. } | Inline::HardBreak { .. } => self.out.push_str("<br>"),
            Inline::Emphasis { emph, children, .. } => {
                let tag = match emph.as_str() {
                    "Bold" => Some("strong"),
                    "Italic" => Some("em"),
                    "Strike_through" => Some("del"),
                    "Highlight" => Some("mark"),
                    "Underline" => Some("u"),
                    _ => None, // unknown emphasis: render children bare (frontend parity)
                };
                if let Some(t) = tag {
                    self.out.push('<');
                    self.out.push_str(t);
                    self.out.push('>');
                    self.inlines(children);
                    self.out.push_str("</");
                    self.out.push_str(t);
                    self.out.push('>');
                } else {
                    self.inlines(children);
                }
            }
            Inline::Subscript { children, .. } => {
                self.out.push_str("<sub>");
                self.inlines(children);
                self.out.push_str("</sub>");
            }
            Inline::Superscript { children, .. } => {
                self.out.push_str("<sup>");
                self.inlines(children);
                self.out.push_str("</sup>");
            }
            Inline::Link { url, label, image, metadata, .. } => self.link(url, label, *image, metadata),
            Inline::NestedLink { content, .. } => {
                // Logseq `[[a [[b]] c]]` — routed as a page ref (catalog), inner kept raw.
                self.out.push_str("<a class=\"page-ref\"");
                push_attr(&mut self.out, "data-page", content);
                self.out.push_str(">[[");
                esc_text(content, &mut self.out);
                self.out.push_str("]]</a>");
            }
            Inline::Target { text, .. } => {
                self.out.push_str("<span class=\"org-target\">");
                esc_text(text, &mut self.out);
                self.out.push_str("</span>");
            }
            Inline::Tag { children, .. } => {
                let name = flatten_text(children);
                self.out.push_str("<a class=\"tag\"");
                push_attr(&mut self.out, "data-page", &name);
                self.out.push_str(">#");
                esc_text(&name, &mut self.out);
                self.out.push_str("</a>");
            }
            Inline::Macro { name, args, .. } => {
                let json = serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string());
                self.out.push_str("<span class=\"macro\"");
                push_attr(&mut self.out, "data-macro", name);
                push_attr(&mut self.out, "data-args", &json);
                self.out.push_str("></span>");
            }
            Inline::Latex { mode, body, .. } => {
                let cls = if mode == "Displayed" { "math math-display" } else { "math" };
                self.out.push_str("<span class=\"");
                self.out.push_str(cls);
                self.out.push('"');
                push_attr(&mut self.out, "data-tex", body);
                self.out.push_str("></span>");
            }
            Inline::Timestamp { ts, date, .. } => self.timestamp(ts, date),
            Inline::Fnref { name, .. } => {
                self.out.push_str("<sup class=\"footnote-ref\">");
                esc_text(name, &mut self.out);
                self.out.push_str("</sup>");
            }
            Inline::InlineHtml { text, .. } => self.raw_html(text),
            Inline::Email { text, .. } => {
                let addr = email_addr(text);
                self.out.push_str("<a class=\"external-link\"");
                push_attr(&mut self.out, "href", &format!("mailto:{addr}"));
                self.out.push('>');
                esc_text(&addr, &mut self.out);
                self.out.push_str("</a>");
            }
            Inline::Entity { unicode, .. } => esc_text(unicode, &mut self.out),
            Inline::Hiccup { v, .. } => esc_text(v, &mut self.out),
        }
    }

    fn link(&mut self, url: &Url, label: &[Inline], image: bool, metadata: &str) {
        match url {
            Url::PageRef { v } => {
                self.out.push_str("<a class=\"page-ref\"");
                push_attr(&mut self.out, "data-page", v);
                self.out.push('>');
                if label.is_empty() {
                    self.out.push_str("[[");
                    esc_text(v, &mut self.out);
                    self.out.push_str("]]");
                } else {
                    self.inlines(label);
                }
                self.out.push_str("</a>");
            }
            Url::BlockRef { v } => {
                self.out.push_str("<span class=\"block-ref\"");
                push_attr(&mut self.out, "data-block", v);
                self.out.push('>');
                if label.is_empty() {
                    let short: String = v.chars().take(8).collect();
                    self.out.push_str("((");
                    esc_text(&short, &mut self.out);
                    self.out.push_str("))");
                } else {
                    esc_text(&flatten_text(label), &mut self.out);
                }
                self.out.push_str("</span>");
            }
            _ => {
                let dest = url_dest(url);
                if image {
                    self.image(&dest, label, metadata);
                } else if dest.to_ascii_lowercase().ends_with(".pdf") {
                    let filename = dest.rsplit('/').next().unwrap_or(&dest);
                    let label_str = if label.is_empty() { filename.to_string() } else { flatten_text(label) };
                    let label_str = if label_str.is_empty() { filename.to_string() } else { label_str };
                    self.out.push_str("<a class=\"external-link pdf-link\"");
                    push_attr(&mut self.out, "href", &dest);
                    self.out.push_str(">\u{1F4C4} ");
                    esc_text(&label_str, &mut self.out);
                    self.out.push_str("</a>");
                } else {
                    self.out.push_str("<a class=\"external-link\"");
                    push_attr(&mut self.out, "href", &dest);
                    self.out.push('>');
                    if label.is_empty() {
                        esc_text(&dest, &mut self.out);
                    } else {
                        self.inlines(label);
                    }
                    self.out.push_str("</a>");
                }
            }
        }
    }

    fn image(&mut self, dest: &str, label: &[Inline], metadata: &str) {
        let alt = if label.is_empty() { String::new() } else { flatten_text(label) };
        match media_kind(dest) {
            Some("video") => {
                self.out.push_str("<span class=\"media-embed-wrap\"><video class=\"media-embed\" controls");
                push_attr(&mut self.out, "data-asset", dest);
                if !metadata.is_empty() {
                    push_attr(&mut self.out, "data-metadata", metadata);
                }
                self.out.push_str("></video></span>");
            }
            Some("audio") => {
                self.out.push_str("<span class=\"media-embed-wrap media-audio-wrap\"><audio class=\"media-embed media-audio\" controls");
                push_attr(&mut self.out, "data-asset", dest);
                if !metadata.is_empty() {
                    push_attr(&mut self.out, "data-metadata", metadata);
                }
                self.out.push_str("></audio></span>");
            }
            // image or unknown extension → an <img> (frontend AssetImage).
            _ => {
                self.out.push_str("<span class=\"inline-image-wrap\"><img class=\"inline-image\"");
                push_attr(&mut self.out, "data-asset", dest);
                push_attr(&mut self.out, "alt", &alt);
                if !metadata.is_empty() {
                    push_attr(&mut self.out, "data-metadata", metadata);
                }
                self.out.push_str("></span>");
            }
        }
    }

    fn timestamp(&mut self, ts: &str, date: &Value) {
        let (active, text) = if ts == "Clock" {
            fmt_clock_timestamp(date)
        } else if ts == "Range" && date.get("start").is_some() && date.get("stop").is_some() {
            fmt_timestamp_range(date)
        } else {
            let active = date.get("active").and_then(Value::as_bool).unwrap_or(true);
            (active, fmt_ts_point(date))
        };
        let cls = if active { "org-timestamp" } else { "org-timestamp inactive" };
        let (open, close) = if active { ("<", ">") } else { ("[", "]") };
        self.out.push_str("<span class=\"");
        self.out.push_str(cls);
        self.out.push_str("\">");
        esc_text(&format!("{open}{text}{close}"), &mut self.out);
        self.out.push_str("</span>");
    }
}

fn fmt_clock_timestamp(date: &Value) -> (bool, String) {
    match (date.get(0).and_then(Value::as_str), date.get(1)) {
        (Some("Started"), Some(point)) => {
            let active = point.get("active").and_then(Value::as_bool).unwrap_or(true);
            (active, fmt_ts_point(point))
        }
        (Some("Stopped"), Some(range)) => fmt_timestamp_range(range),
        _ => (true, fmt_ts_point(date)),
    }
}

fn fmt_timestamp_range(date: &Value) -> (bool, String) {
    let (Some(start), Some(stop)) = (date.get("start"), date.get("stop")) else {
        return (true, fmt_ts_point(date));
    };
    let active = start.get("active").and_then(Value::as_bool).unwrap_or(true);
    (active, format!("{}--{}", fmt_ts_point(start), fmt_ts_point(stop)))
}

// ===========================================================================
// Free helpers
// ===========================================================================

/// `[!TYPE] title…` → `(type_lowercased, title_left_trimmed)`, or `None` if the leading
/// plain text isn't a GitHub-callout marker. Mirrors the frontend regex
/// `^\[!(\w+)\]\s*(.*)$` but reads the already-parsed Plain node (no re-parse).
fn parse_callout_lead(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let ty = &rest[..close];
    if ty.is_empty() || !ty.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    // Left-trim only (mirror the frontend regex `\[!\w+\]\s*(.*)`): keep a trailing space
    // before any inline markup on the title line so `[!NOTE] Heads **up**` joins as
    // "Heads <strong>up</strong>", not "Heads<strong>up</strong>".
    let title = rest[close + 1..].trim_start().to_string();
    Some((ty.to_ascii_lowercase(), title))
}

/// The destination string of a link/image `url` (mirrors the frontend `urlDest`).
fn url_dest(url: &Url) -> String {
    match url {
        Url::PageRef { v }
        | Url::BlockRef { v }
        | Url::Search { v }
        | Url::File { v }
        | Url::EmbedData { v } => v.clone(),
        Url::Complex { protocol, link } => match (protocol, link) {
            (Some(p), Some(l)) => format!("{p}://{l}"),
            (_, l) => l.clone().unwrap_or_default(),
        },
    }
}

/// Flatten an inline run to plain text (tag names, block-ref / image / pdf labels) —
/// mirrors the frontend `astText`. Render-only nodes (break, macro, timestamp, email,
/// inline_html, fnref) contribute nothing, exactly as `astText` omits them.
fn flatten_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    flatten_into(inlines, &mut out);
    out
}

fn flatten_into(inlines: &[Inline], out: &mut String) {
    for s in inlines {
        match s {
            Inline::Plain { text, .. } | Inline::Code { text, .. } | Inline::Verbatim { text, .. } => out.push_str(text),
            Inline::Emphasis { children, .. } | Inline::Subscript { children, .. } | Inline::Superscript { children, .. } => {
                flatten_into(children, out);
            }
            Inline::Tag { children, .. } => {
                out.push('#');
                flatten_into(children, out);
            }
            Inline::Link { url, label, .. } => {
                if label.is_empty() {
                    out.push_str(&url_dest(url));
                } else {
                    flatten_into(label, out);
                }
            }
            Inline::NestedLink { content, .. } => out.push_str(content),
            Inline::Target { text, .. } => out.push_str(text),
            Inline::Entity { unicode, .. } => out.push_str(unicode),
            Inline::Latex { body, .. } => out.push_str(body),
            Inline::Hiccup { v, .. } => out.push_str(v),
            // Not part of `astText`: contribute no text.
            Inline::Break { .. }
            | Inline::HardBreak { .. }
            | Inline::Macro { .. }
            | Inline::Timestamp { .. }
            | Inline::Fnref { .. }
            | Inline::InlineHtml { .. }
            | Inline::Email { .. } => {}
        }
    }
}

/// Reconstruct an email address from mldoc's opaque address record (`{local_part,
/// domain}` object, or a bare string). Mirrors the frontend `renderEmail`.
fn email_addr(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(o) = v.as_object() {
        if let (Some(lp), Some(dom)) = (
            o.get("local_part").and_then(Value::as_str),
            o.get("domain").and_then(Value::as_str),
        ) {
            return format!("{lp}@{dom}");
        }
    }
    String::new()
}

/// Format a single timestamp point `{date:{year,month,day}, wday?, time?}` →
/// `year-MM-dd[ wday][ HH:mm]`. Mirrors the frontend `fmtTsPoint`.
fn fmt_ts_point(p: &Value) -> String {
    let g = |k: &str| p.get("date").and_then(|d| d.get(k)).and_then(Value::as_i64).unwrap_or(0);
    let mut s = format!("{}-{:02}-{:02}", g("year"), g("month"), g("day"));
    if let Some(w) = p.get("wday").and_then(Value::as_str) {
        if !w.is_empty() {
            s.push(' ');
            s.push_str(w);
        }
    }
    if let Some(t) = p.get("time").filter(|t| t.is_object()) {
        let h = t.get("hour").and_then(Value::as_i64).unwrap_or(0);
        let mi = t.get("min").and_then(Value::as_i64).unwrap_or(0);
        s.push_str(&format!(" {h:02}:{mi:02}"));
    }
    s
}

/// Lowercased extension (no dot) of a filename/URL, ignoring a `?`/`#` tail.
fn ext_of(s: &str) -> String {
    let q = s.split(['?', '#']).next().unwrap_or(s);
    match q.rfind('.') {
        Some(i) => q[i + 1..].to_ascii_lowercase(),
        None => String::new(),
    }
}

/// `"image"`/`"video"`/`"audio"`/`None` for a destination — the extension sets Tine
/// shares between insert + render (`src/media.ts`).
fn media_kind(s: &str) -> Option<&'static str> {
    const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif"];
    const VIDEO: &[&str] = &["mp4", "webm", "mov", "flv", "avi", "mkv", "m4v", "ogv"];
    const AUDIO: &[&str] = &["mp3", "ogg", "oga", "wav", "m4a", "flac", "wma", "aac", "opus", "mpeg"];
    let e = ext_of(s);
    if IMAGE.contains(&e.as_str()) {
        Some("image")
    } else if VIDEO.contains(&e.as_str()) {
        Some("video")
    } else if AUDIO.contains(&e.as_str()) {
        Some("audio")
    } else {
        None
    }
}

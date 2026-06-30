//! Exact-HTML-per-kind tests for `lsdoc::render_html` — the canonical AST→HTML
//! skeleton (M3 render contract). Each test drives `lsdoc::parse` → `render_html`
//! and asserts the precise emitted string, so any structural/class/`data-*`/escaping
//! drift is a hard failure. Covers every Block + Inline kind, the escaping security
//! boundary, callout detection, timestamp formatting, table `data-align`, checkbox
//! `data-cb-index`, and recursion (nested list + nested callout).

use lsdoc::ast::{Block, Inline};
use lsdoc::{render_html, Format, RenderOpts};

/// Parse `input` in `fmt`, then render to the canonical HTML skeleton.
fn md(input: &str) -> String {
    render_html(&lsdoc::parse(input, "md"), &RenderOpts { format: Format::Md })
}
fn org(input: &str) -> String {
    render_html(&lsdoc::parse(input, "org"), &RenderOpts { format: Format::Org })
}

// ===========================================================================
// Inline-flow blocks
// ===========================================================================

#[test]
fn paragraph_is_a_bare_inline_run() {
    assert_eq!(md("hello **world** and *em*"), "hello <strong>world</strong> and <em>em</em>");
}

#[test]
fn heading_wraps_in_heading_text_with_level() {
    assert_eq!(md("# Title here"), r#"<span class="heading-text h1">Title here</span>"#);
    assert_eq!(md("### Deep"), r#"<span class="heading-text h3">Deep</span>"#);
}

#[test]
fn bullet_is_a_bare_inline_run() {
    // marker/priority/size are consumer outline chrome, not part of the body skeleton.
    assert_eq!(md("- a bullet body"), "a bullet body");
}

// ===========================================================================
// Emphasis / code / breaks
// ===========================================================================

#[test]
fn emphasis_variants() {
    assert_eq!(md("**b**"), "<strong>b</strong>");
    assert_eq!(md("*i*"), "<em>i</em>");
    assert_eq!(md("~~s~~"), "<del>s</del>");
    assert_eq!(md("==h=="), "<mark>h</mark>");
    assert_eq!(org("org _under_ text"), "org <u>under</u> text");
}

#[test]
fn inline_code_and_verbatim_escape() {
    assert_eq!(md("use `x < y`"), r#"use <code class="inline-code">x &lt; y</code>"#);
    assert_eq!(org("a =v < w= b"), r#"a <code class="inline-code">v &lt; w</code> b"#);
}

#[test]
fn subscript_superscript_org() {
    assert_eq!(org("H_{2}O and x^{2}"), "H<sub>2</sub>O and x<sup>2</sup>");
}

// ===========================================================================
// Links / refs / media
// ===========================================================================

#[test]
fn page_ref_bare_and_labeled() {
    assert_eq!(md("see [[My Page]] ok"), r#"see <a class="page-ref" data-page="My Page">[[My Page]]</a> ok"#);
    assert_eq!(md("[lbl]([[My Page]])"), r#"<a class="page-ref" data-page="My Page">lbl</a>"#);
}

#[test]
fn block_ref_short_id_and_labeled() {
    assert_eq!(
        md("see ((64f8a-uuid-1234-5678)) ok"),
        r#"see <span class="block-ref" data-block="64f8a-uuid-1234-5678">((64f8a-uu))</span> ok"#
    );
    assert_eq!(
        md("[the label](((64f8a-uuid-1234-5678)))"),
        r#"<span class="block-ref" data-block="64f8a-uuid-1234-5678">the label</span>"#
    );
}

#[test]
fn external_pdf_bare_links() {
    assert_eq!(md("see [Google](https://google.com) ok"), r#"see <a class="external-link" href="https://google.com">Google</a> ok"#);
    assert_eq!(md("[doc](file.pdf)"), "<a class=\"external-link pdf-link\" href=\"file.pdf\">\u{1F4C4} doc</a>");
    assert_eq!(md("https://example.com/x"), r#"<a class="external-link" href="https://example.com/x">https://example.com/x</a>"#);
}

#[test]
fn image_video_audio() {
    assert_eq!(
        md("![alt <x>](../assets/x.png){:width 200}"),
        r#"<span class="inline-image-wrap"><img class="inline-image" data-asset="../assets/x.png" alt="alt &lt;x&gt;" data-metadata="{:width 200}"></span>"#
    );
    assert_eq!(md("![v](movie.mp4)"), r#"<span class="media-embed-wrap"><video class="media-embed" controls data-asset="movie.mp4"></video></span>"#);
    assert_eq!(md("![a](sound.mp3)"), r#"<span class="media-embed-wrap media-audio-wrap"><audio class="media-embed media-audio" controls data-asset="sound.mp3"></audio></span>"#);
}

#[test]
fn tags() {
    assert_eq!(md("a #foo b"), r#"a <a class="tag" data-page="foo">#foo</a> b"#);
    assert_eq!(md("a #[[bar baz]] b"), r#"a <a class="tag" data-page="bar baz">#bar baz</a> b"#);
}

#[test]
fn email_entity_target() {
    assert_eq!(md("contact <a.b@c.com> now"), r#"contact <a class="external-link" href="mailto:a.b@c.com">a.b@c.com</a> now"#);
    assert_eq!(md("alpha \\Delta beta"), "alpha \u{0394} beta");
    assert_eq!(org("see <<anchor>> here"), r#"see <span class="org-target">anchor</span> here"#);
}

// ===========================================================================
// Macros / math / hiccup
// ===========================================================================

#[test]
fn macro_emits_hooks_not_output() {
    assert_eq!(
        md("{{query (and [[a]] [[b]])}}"),
        r#"<span class="macro" data-macro="query" data-args="[&quot;(and [[a]] [[b]])&quot;]"></span>"#
    );
}

#[test]
fn inline_and_block_math_are_empty_with_tex_hook() {
    assert_eq!(md("math $a<b$ here"), r#"math <span class="math" data-tex="a&lt;b"></span> here"#);
    assert_eq!(md("$$x^2 < y$$"), r#"<span class="math math-display" data-tex="x^2 &lt; y"></span>"#);
    assert_eq!(
        org("\\begin{align}\na &= b\n\\end{align}"),
        "<span class=\"math math-display\" data-tex=\"\\begin{align}a &amp;= b\n\\end{align}\"></span>"
    );
}

#[test]
fn hiccup_block_and_inline() {
    assert_eq!(md("[:b.foo hi]"), r#"<span class="ast-hiccup">[:b.foo hi]</span>"#);
}

// ===========================================================================
// Timestamps (lsdoc owns the format)
// ===========================================================================

#[test]
fn timestamp_active_inactive_range() {
    assert_eq!(md("do <2026-06-30 Mon 14:30>"), r#"do <span class="org-timestamp">&lt;2026-06-30 Mon 14:30&gt;</span>"#);
    assert_eq!(org("do [2026-06-30 Mon 14:30]"), r#"do <span class="org-timestamp inactive">[2026-06-30 Mon 14:30]</span>"#);
    assert_eq!(
        md("<2026-06-30 Mon 14:30>--<2026-07-02 Wed 16:00>"),
        r#"<span class="org-timestamp">&lt;2026-06-30 Mon 14:30--2026-07-02 Wed 16:00&gt;</span>"#
    );
    // date-only (weekday, no time) — mldoc needs the weekday to recognize a timestamp.
    assert_eq!(org("[2026-06-30 Mon]"), r#"<span class="org-timestamp inactive">[2026-06-30 Mon]</span>"#);
}

// ===========================================================================
// Block-level constructs
// ===========================================================================

#[test]
fn src_and_example_emit_escaped_code_and_data_lang() {
    assert_eq!(
        md("```rust\nlet x = 1 < 2 && 3 > 2;\n```"),
        "<pre class=\"code-block\"><code class=\"hljs\" data-lang=\"rust\">let x = 1 &lt; 2 &amp;&amp; 3 &gt; 2;\n</code></pre>"
    );
    assert_eq!(
        org("#+BEGIN_EXAMPLE\ncode <x> & y\n#+END_EXAMPLE"),
        "<pre class=\"code-block\"><code class=\"hljs\" data-lang=\"\">code &lt;x&gt; &amp; y\n</code></pre>"
    );
}

#[test]
fn hr_and_footnote_and_displayed_math() {
    assert_eq!(md("---"), r#"<hr class="md-hr">"#);
    assert_eq!(md("[^1]: the note body"), r#"<div class="footnote-def"><sup class="footnote-ref">1</sup> the note body</div>"#);
    assert_eq!(md("$$a < b$$"), r#"<span class="math math-display" data-tex="a &lt; b"></span>"#);
}

#[test]
fn latex_env_block_reconstructs_tex() {
    assert_eq!(
        org("\\begin{equation}\nE=mc^2\n\\end{equation}"),
        "<span class=\"math math-display\" data-tex=\"\\begin{equation}E=mc^2\n\\end{equation}\"></span>"
    );
}

#[test]
fn properties_render_all_pairs_value_as_inline() {
    assert_eq!(
        md("key:: value with [[ref]]\nfoo:: bar"),
        concat!(
            r#"<span class="block-properties">"#,
            r#"<span class="block-property"><span class="block-property-key">key</span> "#,
            r#"<span class="block-property-val">value with <a class="page-ref" data-page="ref">[[ref]]</a></span></span>"#,
            r#"<span class="block-property"><span class="block-property-key">foo</span> "#,
            r#"<span class="block-property-val">bar</span></span>"#,
            r#"</span>"#
        )
    );
}

#[test]
fn quote_plain_blockquote() {
    assert_eq!(md("> just a quote\n> more"), r#"<blockquote class="md-quote">just a quote<br>more<br></blockquote>"#);
}

#[test]
fn drawer_directive_comment_render_nothing() {
    assert_eq!(org(":LOGBOOK:\nstuff\n:END:"), "");
    assert_eq!(org("#+TITLE: Hello"), "");
    assert_eq!(org("# a comment line"), "");
}

#[test]
fn raw_html_block_escapes_into_data_raw() {
    assert_eq!(
        md("<div onclick=\"x\">hi & <b>bye</b></div>"),
        r#"<span class="raw-html" data-raw="&lt;div onclick=&quot;x&quot;&gt;hi &amp; &lt;b&gt;bye&lt;/b&gt;&lt;/div&gt;"></span>"#
    );
}

// ===========================================================================
// Callouts (from the AST — no string re-parse)
// ===========================================================================

#[test]
fn quote_callout_with_title_and_body() {
    assert_eq!(
        md("> [!NOTE] Heads up\n> body line"),
        r#"<div class="callout callout-note"><div class="callout-title">Heads up</div><div class="callout-body">body line<br></div></div>"#
    );
}

#[test]
fn quote_callout_without_title_uses_uppercased_type() {
    assert_eq!(
        md("> [!warning]\n> careful"),
        r#"<div class="callout callout-warning"><div class="callout-title">WARNING</div><div class="callout-body">careful<br></div></div>"#
    );
}

#[test]
fn custom_callout_block() {
    assert_eq!(
        org("#+BEGIN_NOTE\nbody\n#+END_NOTE"),
        r#"<div class="callout callout-note"><div class="callout-title">NOTE</div><div class="callout-body">body<br></div></div>"#
    );
}

#[test]
fn nested_callout_recursion() {
    assert_eq!(
        org("#+BEGIN_NOTE\nintro\n#+BEGIN_TIP\ninner tip\n#+END_TIP\n#+END_NOTE"),
        concat!(
            r#"<div class="callout callout-note"><div class="callout-title">NOTE</div><div class="callout-body">"#,
            r#"intro"#,
            r#"<div class="callout callout-tip"><div class="callout-title">TIP</div><div class="callout-body">inner tip<br></div></div>"#,
            r#"</div></div>"#
        )
    );
}

#[test]
fn nested_callout_quote_in_place() {
    // a `> [!TYPE]` callout whose body contains a nested `> > [!TYPE]` callout — renders in
    // place (no subtree clone; the clone was the O(n²) audit HIGH). Lead remainder + the rest
    // of the children keep the same <br>-join blocks() would apply.
    assert_eq!(
        md("> [!NOTE] outer\n> body1\n> > [!TIP] inner\n> > body2"),
        concat!(
            r#"<div class="callout callout-note"><div class="callout-title">outer</div><div class="callout-body">"#,
            r#"body1"#,
            r#"<div class="callout callout-tip"><div class="callout-title">inner</div><div class="callout-body">body2<br></div></div>"#,
            r#"</div></div>"#
        )
    );
}

// ===========================================================================
// Lists
// ===========================================================================

#[test]
fn unordered_and_ordered_list() {
    assert_eq!(md("* a\n* b"), r#"<ul class="md-list"><li class="md-list-item">a</li><li class="md-list-item">b</li></ul>"#);
    assert_eq!(md("1. one\n2. two"), r#"<ol class="md-list"><li class="md-list-item">one</li><li class="md-list-item">two</li></ol>"#);
}

#[test]
fn nested_list_recursion() {
    assert_eq!(
        md("* a\n  * b\n  * c"),
        concat!(
            r#"<ul class="md-list"><li class="md-list-item">a"#,
            r#"<ul class="md-list"><li class="md-list-item">b</li><li class="md-list-item">c</li></ul>"#,
            r#"</li></ul>"#
        )
    );
}

#[test]
fn checkbox_list_depth_first_cb_index() {
    assert_eq!(
        md("* [ ] todo a\n* [x] done b\n  * [ ] nested c"),
        concat!(
            r#"<ul class="md-list">"#,
            r#"<li class="md-list-item has-checkbox"><span class="block-checkbox" data-cb-index="0"></span> todo a</li>"#,
            r#"<li class="md-list-item has-checkbox"><span class="block-checkbox checked" data-cb-index="1"></span> done b"#,
            r#"<ul class="md-list"><li class="md-list-item has-checkbox"><span class="block-checkbox" data-cb-index="2"></span> nested c</li></ul>"#,
            r#"</li></ul>"#
        )
    );
}

#[test]
fn definition_list_term() {
    assert_eq!(
        md("Term\n: the definition"),
        r#"<ul class="md-list"><li class="md-list-item has-term"><span class="md-list-term">Term</span> the definition</li></ul>"#
    );
}

#[test]
fn definition_list_term_renders_inline() {
    // the term is an inline run (here a page ref), rendered like any other inline.
    assert_eq!(
        md("key [[Page]] word\n: the body"),
        concat!(
            r#"<ul class="md-list"><li class="md-list-item has-term">"#,
            r#"<span class="md-list-term">key <a class="page-ref" data-page="Page">[[Page]]</a> word</span> "#,
            r#"the body</li></ul>"#
        )
    );
}

// ===========================================================================
// Tables (data-align bonus)
// ===========================================================================

#[test]
fn table_with_alignment() {
    assert_eq!(
        md("| A | B | C |\n|:--|:-:|--:|\n| 1 | 2 | 3 |"),
        concat!(
            r#"<table class="md-table"><thead><tr>"#,
            r#"<th data-align="left">A</th><th data-align="center">B</th><th data-align="right">C</th>"#,
            r#"</tr></thead><tbody><tr>"#,
            r#"<td data-align="left">1</td><td data-align="center">2</td><td data-align="right">3</td>"#,
            r#"</tr></tbody></table>"#
        )
    );
}

#[test]
fn table_without_alignment_emits_no_data_align() {
    assert_eq!(
        md("| A | B |\n|---|---|\n| 1 | 2 |"),
        r#"<table class="md-table"><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>"#
    );
}

#[test]
fn table_partial_alignment() {
    // only the first column is aligned; the second gets no data-align.
    assert_eq!(
        md("| A | B |\n|:--|---|\n| 1 | 2 |"),
        r#"<table class="md-table"><thead><tr><th data-align="left">A</th><th>B</th></tr></thead><tbody><tr><td data-align="left">1</td><td>2</td></tr></tbody></table>"#
    );
}

// ===========================================================================
// Escaping — the security boundary
// ===========================================================================

#[test]
fn plain_text_escapes_angle_and_amp() {
    // `<` `>` `&` escaped in text; `"` left as-is in text (per the contract).
    assert_eq!(md(r#"5 < 3 & 2 > 1 say "hi""#), r#"5 &lt; 3 &amp; 2 &gt; 1 say "hi""#);
}

#[test]
fn script_in_text_is_never_live() {
    // A `<script>` run lands as inline raw-html: ESCAPED into data-raw, empty element.
    assert_eq!(
        md("a <script>alert(1)</script> b"),
        r#"a <span class="raw-html" data-raw="&lt;script&gt;alert(1)&lt;/script&gt;"></span> b"#
    );
}

#[test]
fn script_and_quote_in_page_name_are_attr_escaped() {
    // `"` and `<`/`>` escaped in the data-page ATTRIBUTE; `<`/`>` escaped in text too.
    assert_eq!(
        md(r#"[[a"<script>]]"#),
        r#"<a class="page-ref" data-page="a&quot;&lt;script&gt;">[[a"&lt;script&gt;]]</a>"#
    );
}

// ===========================================================================
// Block-level <br>-join of consecutive inline-flow blocks (frontend renderBlocks)
// ===========================================================================

fn plain_para(text: &str) -> Block {
    Block::Paragraph { inline: vec![Inline::Plain { text: text.into() }], span: None }
}

#[test]
fn consecutive_inline_flow_blocks_are_br_joined() {
    let blocks = vec![plain_para("a"), plain_para("b"), plain_para("c")];
    assert_eq!(render_html(&blocks, &RenderOpts { format: Format::Md }), "a<br>b<br>c");
}

#[test]
fn non_inline_flow_block_breaks_the_join() {
    let blocks = vec![plain_para("a"), Block::Hr { span: None }, plain_para("b")];
    assert_eq!(render_html(&blocks, &RenderOpts { format: Format::Md }), r#"a<hr class="md-hr">b"#);
}

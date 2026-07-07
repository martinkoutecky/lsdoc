//! v2 block parser.
//!
//! Each construct branch owns only inputs it can parse completely by source-transcribed
//! rules. A `None` return is an ownership failure for strict/report harnesses and a
//! production panic through `v2::parse_format`, not a silent fallback to the legacy parser.

use crate::block_common::{
    begin_export_fields, displayed_math_opener, drawer_property, find_displayed_math_close,
    find_matching_fence, first_body_indent, leading_ws, mldoc_heading_boundary, mldoc_is_space,
    mldoc_ltrim_prefix_at_most, mldoc_spaces_len, mldoc_trim_spaces, mldoc_trim_spaces_start,
    ocaml_trim, ocaml_trim_byte, parse_raw_html_at_cached, raw_html_capture_text, split_checkbox,
    RawHtmlScan, StripCtx, StripSeqTree, MARKERS,
};
use crate::projection::{Align, Block, Inline, ListItem, Property, Span, SpanMapSegment};
use crate::source_map::{OriginCursor, OriginMap};

use super::source::{Eol, Line, Source};

pub(crate) fn try_parse(input: &str, format: &str) -> Option<Vec<Block>> {
    if let Some((mut blocks, rest_start)) = markdown_front_matter_sequence(input) {
        let mut tail = try_parse_leaf_blocks(&input[rest_start..], format)?;
        offset_blocks(&mut tail, rest_start);
        blocks.extend(tail);
        return Some(blocks);
    }
    try_parse_leaf_blocks(input, format)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockParseContext {
    Document,
    BlockContent,
    ListContent(ListContentMode),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListContentMode {
    Document,
    BlockContent,
}

impl BlockParseContext {
    fn is_list_content(self) -> bool {
        matches!(self, BlockParseContext::ListContent(_))
    }

    fn list_content_mode(self) -> Option<ListContentMode> {
        match self {
            BlockParseContext::ListContent(mode) => Some(mode),
            _ => None,
        }
    }
}

fn try_parse_leaf_blocks(input: &str, format: &str) -> Option<Vec<Block>> {
    try_parse_leaf_blocks_in(input, format, BlockParseContext::Document)
}

fn try_parse_leaf_blocks_in(
    input: &str,
    format: &str,
    context: BlockParseContext,
) -> Option<Vec<Block>> {
    let source = Source::scan(input);
    let mut blocks = Vec::with_capacity(source.lines.len());
    let mut para = ParagraphRun::new(source.input);
    let mut i = 0usize;
    let mut drawer_end_cursor = 0usize;
    let mut property_end_cursor = 0usize;
    let mut fence_cursor = 0usize;
    let mut raw_html_scan = RawHtmlScan::new();
    while i < source.lines.len() {
        let line = &source.lines[i];
        if definitely_paragraph_line(&source, i, format, context) {
            para.push_line(line);
            i += 1;
            continue;
        }
        if context.list_content_mode() == Some(ListContentMode::Document) {
            if let Some(prefix_end) = results_prefix_end(line.text) {
                para.flush(&mut blocks, format);
                let span_end = line.start + prefix_end;
                blocks.push(Block::Results {
                    span: Some(Span(line.start, span_end)),
                });
                if span_end < line_text_end(line) {
                    para.push_tail(line, span_end);
                } else {
                    para.push_eol(line);
                }
                i += 1;
                continue;
            }
        }
        if let Some((name, value)) = directive_line(line.text) {
            para.flush(&mut blocks, format);
            let (span_end, next_i) = directive_span_end(&source.lines, i);
            blocks.push(Block::Directive {
                name,
                value,
                span: Some(Span(line.start, span_end)),
            });
            i = next_i;
            continue;
        }
        match property_or_drawer(
            &source,
            i,
            format,
            &mut drawer_end_cursor,
            &mut property_end_cursor,
            &mut fence_cursor,
            &mut raw_html_scan,
        ) {
            PropertyDrawerDecision::Emit {
                block,
                after_blocks,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                blocks.extend(after_blocks);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            PropertyDrawerDecision::Delegate => return None,
            PropertyDrawerDecision::No => {}
        }
        match heading_line(line, format) {
            HeadingDecision::Emit(heading) => {
                para.flush(&mut blocks, format);
                let drop_tail = heading.tail_start.is_some()
                    && empty_marker_tail_drops(&source, i, format, &mut raw_html_scan);
                let mut block = heading.block(format);
                if drop_tail {
                    set_block_span_end(
                        &mut block,
                        if line.eol == Eol::Cr {
                            line_text_end(line)
                        } else {
                            line.end
                        },
                    );
                }
                blocks.push(block);
                if let Some(tail_start) = heading.tail_start.filter(|_| !drop_tail) {
                    para.push_tail(line, tail_start);
                } else if line.eol == Eol::Cr {
                    para.push_eol(line);
                }
                i += 1;
                continue;
            }
            HeadingDecision::Split {
                mut heading,
                title_start,
            } => {
                if context != BlockParseContext::Document {
                    para.push_line(line);
                    i += 1;
                    continue;
                }
                let split = bounded_split_suffix_blocks(
                    &source,
                    i,
                    title_start,
                    format,
                    &mut drawer_end_cursor,
                    &mut property_end_cursor,
                    &mut fence_cursor,
                    &mut raw_html_scan,
                )
                .or_else(|| {
                    (context == BlockParseContext::Document)
                        .then(|| callout_container_split_at(&source, i, title_start, format))
                        .flatten()
                });
                let Some(split) = split else {
                    if context == BlockParseContext::Document
                        && heading_split_title_falls_back_to_inline(
                            &source,
                            i,
                            title_start,
                            format,
                            &mut fence_cursor,
                            &mut raw_html_scan,
                        )
                    {
                        let line = &source.lines[i];
                        heading.title = &source.input[title_start..line_text_end(line)];
                        heading.title_start = title_start;
                        heading.span_end = if line.eol == Eol::Cr {
                            line_text_end(line)
                        } else {
                            line.end
                        };
                        para.flush(&mut blocks, format);
                        blocks.push(heading.block(format));
                        if line.eol == Eol::Cr {
                            para.push_eol(line);
                        }
                        i += 1;
                        continue;
                    }
                    if context != BlockParseContext::Document
                        && heading_split_title_is_suppressed_begin(&source, i, title_start)
                    {
                        let line = &source.lines[i];
                        heading.title = &source.input[title_start..line_text_end(line)];
                        heading.title_start = title_start;
                        heading.span_end = if line.eol == Eol::Cr {
                            line_text_end(line)
                        } else {
                            line.end
                        };
                        para.flush(&mut blocks, format);
                        blocks.push(heading.block(format));
                        if line.eol == Eol::Cr {
                            para.push_eol(line);
                        }
                        i += 1;
                        continue;
                    }
                    return None;
                };
                para.flush(&mut blocks, format);
                blocks.push(heading.block(format));
                blocks.extend(split.blocks);
                if let Some((tail_line, tail_start)) = split.tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = split.next;
                continue;
            }
            HeadingDecision::No => {}
        }
        match table_sequence(&source, i, format) {
            TableDecision::Emit {
                block,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            TableDecision::No => {}
        }
        match latex_env_sequence(&source, i) {
            LatexEnvDecision::Emit {
                block,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            LatexEnvDecision::Paragraph => {
                para.push_line(line);
                i += 1;
                continue;
            }
            LatexEnvDecision::No => {}
        }
        match fence_sequence(&source, i, &mut fence_cursor) {
            FenceDecision::Emit { block, next } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
            FenceDecision::FallthroughParagraph => {
                para.push_line(line);
                i += 1;
                continue;
            }
            FenceDecision::No => {}
        }
        match raw_src_example_sequence(&source, i) {
            RawSrcExampleDecision::Emit { block, next } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
            RawSrcExampleDecision::No => {}
        }
        match callout_container_sequence(&source, i, format) {
            CalloutContainerDecision::Emit { block, next } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
            CalloutContainerDecision::No => {}
        }
        match markdown_blockquote_sequence(&source, i, format) {
            BlockquoteDecision::Emit { block, next } => {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
            BlockquoteDecision::Paragraph => {
                para.push_line(line);
                i += 1;
                continue;
            }
            BlockquoteDecision::Delegate => return None,
            BlockquoteDecision::No => {}
        }
        match displayed_math_sequence(
            &source,
            i,
            format,
            &mut drawer_end_cursor,
            &mut property_end_cursor,
            &mut fence_cursor,
            &mut raw_html_scan,
        ) {
            DisplayedMathDecision::Emit {
                blocks: math_blocks,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.extend(math_blocks);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            DisplayedMathDecision::Delegate => return None,
            DisplayedMathDecision::Paragraph => {
                para.push_line(line);
                i += 1;
                continue;
            }
            DisplayedMathDecision::No => {}
        }
        match raw_html_sequence(
            &source,
            i,
            format,
            &mut drawer_end_cursor,
            &mut property_end_cursor,
            &mut fence_cursor,
            &mut raw_html_scan,
        ) {
            RawHtmlDecision::Emit {
                blocks: html_blocks,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.extend(html_blocks);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            RawHtmlDecision::Delegate => return None,
            RawHtmlDecision::No => {}
        }
        match hiccup_sequence(
            &source,
            i,
            format,
            &mut drawer_end_cursor,
            &mut property_end_cursor,
            &mut fence_cursor,
            &mut raw_html_scan,
        ) {
            HiccupDecision::Emit {
                blocks: hiccup_blocks,
                next,
                tail_start,
            } => {
                para.flush(&mut blocks, format);
                blocks.extend(hiccup_blocks);
                if let Some((tail_line, tail_start)) = tail_start {
                    para.push_tail(&source.lines[tail_line], tail_start);
                }
                i = next;
                continue;
            }
            HiccupDecision::Delegate => return None,
            HiccupDecision::No => {}
        }
        if let Some((block, next)) = org_verbatim_sequence(&source, i, format) {
            para.flush(&mut blocks, format);
            blocks.push(block);
            i = next;
            continue;
        }
        if context == BlockParseContext::Document {
            if let Some((block, next)) = footnote_sequence(&source, i, format) {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
        }
        if context.is_list_content() {
            if list_content_regular_list_candidate_at(&source, i, line.start, format) {
                para.push_line(line);
                i += 1;
                continue;
            }
        } else {
            match regular_list_sequence(&source, i, format, context) {
                ListDecision::Emit { block, next } => {
                    para.flush(&mut blocks, format);
                    blocks.push(block);
                    i = next;
                    continue;
                }
                ListDecision::Delegate => return None,
                ListDecision::Paragraph => {
                    para.push_line(line);
                    i += 1;
                    continue;
                }
                ListDecision::No => {}
            }
        }
        if !context.is_list_content() {
            if let Some((block, next)) = markdown_definition_sequence(&source, i, format) {
                para.flush(&mut blocks, format);
                blocks.push(block);
                i = next;
                continue;
            }
        }
        if hr_accepts_eol(line.eol) && is_hr_line(line.text, format) {
            para.flush(&mut blocks, format);
            blocks.push(Block::Hr {
                span: Some(Span(line.start, line.end)),
            });
            i += 1;
            continue;
        }
        if let Some(comment) = comment_line(line.text, format) {
            para.flush(&mut blocks, format);
            if format == "org" {
                let (span_end, next_i) = directive_span_end(&source.lines, i);
                blocks.push(Block::Comment {
                    text: comment,
                    span: Some(Span(line.start, span_end)),
                });
                i = next_i;
            } else {
                blocks.push(Block::Comment {
                    text: comment,
                    span: Some(Span(line.start, line_text_end(line))),
                });
                para.push_eol(line);
                i += 1;
            }
            continue;
        }
        if begin_line_falls_through_to_paragraph(&source, i) {
            para.push_line(line);
            i += 1;
            continue;
        }
        if could_start_non_paragraph(line.text, format) {
            if line.eol == Eol::Cr && regular_list_marker_text(line.text, format).is_some() {
                para.push_line(line);
                i += 1;
                continue;
            }
            if rejected_regular_list_tail_at(&source, i, line.start, format) {
                para.push_line(line);
                i += 1;
                continue;
            }
            if line.eol == Eol::Cr && format != "org" && markdown_property_line(line.text).is_some()
            {
                para.push_line(line);
                i += 1;
                continue;
            }
            if rejected_markdown_property_tail_at(&source, i, line.start, format) {
                para.push_line(line);
                i += 1;
                continue;
            }
            return None;
        }
        para.push_line(line);
        i += 1;
    }
    para.flush(&mut blocks, format);
    Some(blocks)
}

fn definitely_paragraph_line(
    source: &Source<'_>,
    i: usize,
    format: &str,
    context: BlockParseContext,
) -> bool {
    let line = &source.lines[i];
    let ws = line.mldoc_spaces;
    let text = &line.text[ws..];
    let Some(first) = text.as_bytes().first().copied() else {
        return true;
    };
    let special = matches!(
        first,
        b'#' | b':'
            | b'*'
            | b'-'
            | b'+'
            | b'_'
            | b'|'
            | b'\\'
            | b'`'
            | b'~'
            | b'$'
            | b'>'
            | b'<'
            | b'['
            | b'0'..=b'9'
    );
    if special {
        return false;
    }
    if format != "org" {
        let property_probe = text.contains("::");
        crate::metrics::scan_work(text.len());
        if property_probe {
            return false;
        }
        if !context.is_list_content()
            && i + 1 < source.lines.len()
            && markdown_definition_opener(&source.lines[i + 1]).is_some()
        {
            return false;
        }
    }
    true
}

// mldoc source: lib/mldoc_parser.ml wraps the full parser in
// `Markdown_front_matter.parse` for both Markdown and Org configs.
// lib/syntax/markdown_front_matter.ml parses only an input-start `---` followed
// by LF/CRLF, consumes through the first later literal `---`, and turns a body of
// `key: value` lines into directives. If the body parser fails, the front matter
// still consumes and emits no directives.
// scan-owner: (a) input-prefix front matter — opener check is constant, the
// close search scans the prefix once, body directive parsing walks the consumed
// body once, and suffix offsetting walks only emitted suffix AST nodes.
fn markdown_front_matter_sequence(input: &str) -> Option<(Vec<Block>, usize)> {
    let bytes = input.as_bytes();
    if !input.starts_with("---") {
        return None;
    }
    let body_start = match bytes.get(3) {
        Some(b'\n') => 4,
        Some(b'\r') if bytes.get(4) == Some(&b'\n') => 5,
        _ => return None,
    };
    let search = &input[body_start..];
    let close = match search.find("---") {
        Some(close) => {
            crate::metrics::scan_work(close + 3);
            close
        }
        None => {
            crate::metrics::scan_work(search.len());
            return None;
        }
    };
    let body = &search[..close];
    Some((markdown_front_matter_body(body), body_start + close + 3))
}

fn markdown_front_matter_body(body: &str) -> Vec<Block> {
    if body.is_empty() {
        return Vec::new();
    }
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let key_start = i;
        while i < bytes.len() && bytes[i] != b':' && bytes[i] != b'\n' && bytes[i] != b'\r' {
            crate::metrics::scan_work(1);
            i += 1;
        }
        if i == key_start || i == bytes.len() || bytes[i] != b':' {
            return Vec::new();
        }
        let key = &body[key_start..i];
        i += 1;
        let ws = mldoc_spaces_len(&body[i..]);
        i += ws;
        let value_start = i;
        while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
            crate::metrics::scan_work(1);
            i += 1;
        }
        let value = &body[value_start..i];
        out.push(Block::Directive {
            name: key.to_string(),
            value: value.to_string(),
            span: None,
        });
        if i == bytes.len() {
            break;
        }
        if bytes[i] == b'\n' {
            i += 1;
        } else if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 2;
        } else {
            return Vec::new();
        }
    }
    out
}

// mldoc source: `mldoc_parser.ml` top-level `list_content_parsers` includes
// `Type_parser.Block.results` but excludes `Directive.parse`, so `#+RESULTS:`
// is a `Results` leaf only inside top-level regular-list item content. The
// parser is case-sensitive and leaves same-line suffix text/EOL to following
// list-content parsers.
// scan-owner: (a2) caller-owned line helper — one current-line prefix check.
fn results_prefix_end(s: &str) -> Option<usize> {
    let start = mldoc_spaces_len(s);
    s[start..]
        .starts_with("#+RESULTS:")
        .then_some(start + "#+RESULTS:".len())
}

struct ParagraphRun<'a> {
    source: &'a str,
    start: Option<usize>,
    end: usize,
    direct: bool,
    text: String,
}

impl<'a> ParagraphRun<'a> {
    fn new(source: &'a str) -> ParagraphRun<'a> {
        ParagraphRun {
            source,
            start: None,
            end: 0,
            direct: true,
            text: String::new(),
        }
    }

    // mldoc source: lib/syntax/paragraph.ml `Paragraph_line` keeps line text, while
    // `Paragraph_Sep n` appends `n` literal "\n" strings. Because `Parsers.eols`
    // treats CRLF as two EOL chars, CRLF becomes two breaks inside paragraph text.
    // scan-owner: (a) consumed paragraph run — each accepted line is appended once
    // before the run is flushed at an HR or EOF.
    fn push_line(&mut self, line: &Line<'_>) {
        self.start.get_or_insert(line.start);
        if self.direct
            && matches!(line.eol, Eol::Lf | Eol::Eof)
            && self.can_extend_direct(line.start)
        {
            self.end = line.end;
        } else {
            self.materialize_direct();
            self.end = line.end;
            self.text.push_str(line.text);
            self.push_eol_text(line.eol);
        }
        crate::metrics::scan_work(line.end - line.start);
    }

    fn push_eol(&mut self, line: &Line<'_>) {
        if line.eol == Eol::Eof {
            return;
        }
        self.start.get_or_insert(line_text_end(line));
        if self.direct && line.eol == Eol::Lf && self.can_extend_direct(line_text_end(line)) {
            self.end = line.end;
        } else {
            self.materialize_direct();
            self.end = line.end;
            self.push_eol_text(line.eol);
        }
        crate::metrics::scan_work(line.end - line_text_end(line));
    }

    fn push_tail(&mut self, line: &Line<'_>, tail_start: usize) {
        self.start.get_or_insert(tail_start);
        if self.direct
            && matches!(line.eol, Eol::Lf | Eol::Eof)
            && self.can_extend_direct(tail_start)
        {
            self.end = line.end;
        } else {
            self.materialize_direct();
            self.end = line.end;
            let text_start = tail_start - line.start;
            self.text.push_str(&line.text[text_start..]);
            self.push_eol_text(line.eol);
        }
        crate::metrics::scan_work(line.end - tail_start);
    }

    fn push_eol_text(&mut self, eol: Eol) {
        match eol {
            Eol::Lf | Eol::Cr => self.text.push('\n'),
            Eol::CrLf => self.text.push_str("\n\n"),
            Eol::Eof => {}
        }
    }

    fn flush(&mut self, blocks: &mut Vec<Block>, format: &str) {
        let Some(start) = self.start.take() else {
            return;
        };
        let inline_text = if self.direct {
            &self.source[start..self.end]
        } else {
            &self.text
        };
        let inline = super::inline_at(inline_text, format, start);
        blocks.push(Block::Paragraph {
            inline,
            span: Some(Span(start, self.end)),
        });
        self.text.clear();
        self.direct = true;
        self.end = 0;
    }

    fn materialize_direct(&mut self) {
        if !self.direct {
            return;
        }
        if let Some(start) = self.start {
            if start < self.end {
                // scan-owner: (a) paragraph materialization — copies only the already accepted
                // direct source slice when a later transformed EOL breaks the borrowed path.
                self.text.push_str(&self.source[start..self.end]);
                crate::metrics::scan_work(self.end - start);
            }
        }
        self.direct = false;
    }

    fn can_extend_direct(&self, next_start: usize) -> bool {
        self.end == 0 || self.end == next_start
    }
}

// mldoc source:
// - lib/syntax/hr.ml: optional spaces, parser, optional spaces, line end / EOF
// - lib/syntax/markdown_hr.ml: many1 of '-', '*', '_'; hr.ml then requires
//   length >= 3 and all chars identical.
// - Org branch: count 5 (char '-'), then the same all-identical check.
// scan-owner: (a2) caller-owned line helper — each v2-owned HR line is trimmed and
// marker-scanned once while the outer line cursor advances monotonically.
fn is_hr_line(text: &str, format: &str) -> bool {
    let t = mldoc_trim_spaces(text);
    crate::metrics::scan_work(t.len());
    if format == "org" {
        t == "-----"
    } else {
        let bytes = t.as_bytes();
        if bytes.len() < 3 {
            return false;
        }
        let marker = bytes[0];
        matches!(marker, b'-' | b'*' | b'_') && bytes.iter().all(|&b| b == marker)
    }
}

fn hr_accepts_eol(eol: Eol) -> bool {
    matches!(eol, Eol::Lf | Eol::CrLf | Eol::Eof)
}

// mldoc source: lib/syntax/directive.ml, wrapped by lib/parsers.ml `between_eols`.
// `Prelude.starts_with` is ASCII-case-insensitive, so every case variant of
// `BEGIN_` leaves Directive.parse and is handled by block/property parsing.
// scan-owner: (a2) caller-owned line helper — directive recognition scans only
// the current line text, then `directive_span_end` advances over contiguous EOL
// bytes represented by following empty lines.
fn directive_line(text: &str) -> Option<(String, String)> {
    let ws = mldoc_spaces_len(text);
    let rest = &text[ws..];
    let rest = rest.strip_prefix("#+")?;
    let name_end = rest.as_bytes().iter().position(|&b| b == b':')?;
    crate::metrics::scan_work(name_end + 1);
    if name_end == 0 {
        return None;
    }
    let name = &rest[..name_end];
    if starts_ci(name, "BEGIN_") {
        return None;
    }
    let mut value = &rest[name_end + 1..];
    let vws = mldoc_spaces_len(value);
    value = &value[vws..];
    crate::metrics::scan_work(name.len() + value.len());
    Some((name.to_string(), value.to_string()))
}

fn heading_title_directive_name(text: &str) -> Option<&str> {
    let ws = mldoc_spaces_len(text);
    let rest = text[ws..].strip_prefix("#+")?;
    let name_end = rest.as_bytes().iter().position(|&b| b == b':')?;
    crate::metrics::scan_work(name_end + 1);
    if name_end == 0 {
        return None;
    }
    let name = &rest[..name_end];
    (!name.as_bytes().iter().any(|&b| mldoc_is_space(b))).then_some(name)
}

fn heading_title_directive_line(text: &str) -> Option<(String, String)> {
    let name = heading_title_directive_name(text)?;
    if starts_ci(name, "BEGIN_") {
        return None;
    }
    directive_line(text)
}

fn heading_title_property_or_drawer_start(s: &str, format: &str) -> bool {
    drawer_begin(s).is_some()
        || heading_title_directive_name(s).is_some()
        || (format != "org" && markdown_property_start(mldoc_trim_spaces_start(s)))
}

fn directive_span_end(lines: &[Line<'_>], start: usize) -> (usize, usize) {
    let mut end = lines[start].end;
    let mut next = start + 1;
    while next < lines.len() && lines[next].text.is_empty() && lines[next].eol != Eol::Eof {
        end = lines[next].end;
        next += 1;
        crate::metrics::scan_work(1);
    }
    (end, next)
}

enum PropertyDrawerDecision {
    Emit {
        block: Block,
        after_blocks: Vec<Block>,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    Delegate,
    No,
}

// mldoc source:
// - lib/syntax/drawer.ml `parse`: `many1 (parse1 <|> parse2)` folded to
//   `Property_Drawer`, falling back to `drawer_parse`.
// - lib/syntax/markdown_property.ml for Markdown `key:: value` parse1.
// This v2 slice owns top-level whole-line `:END:` drawers plus bounded same-line
// close tails after property drawers.
// scan-owner: (a2) caller-owned line helper + monotone drawer closer cursor.
fn property_or_drawer(
    source: &Source<'_>,
    i: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> PropertyDrawerDecision {
    property_or_drawer_at(
        source,
        i,
        source.lines[i].start,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    )
}

fn property_or_drawer_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> PropertyDrawerDecision {
    let line = &source.lines[i];
    let Some(rel) = start_abs.checked_sub(line.start) else {
        return PropertyDrawerDecision::No;
    };
    let Some(text) = line.text.get(rel..) else {
        return PropertyDrawerDecision::No;
    };
    if format != "org" {
        if line.eol == Eol::Cr && markdown_property_line(text).is_some() {
            return PropertyDrawerDecision::No;
        }
        if let Some(kv) = markdown_property_line(text) {
            let fold = match fold_markdown_property_group(
                source,
                i + 1,
                vec![Property::parse1(kv)],
                line.end,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                Ok(fold) => fold,
                Err(decision) => return decision,
            };
            return PropertyDrawerDecision::Emit {
                block: Block::Properties {
                    props: fold.props,
                    span: Some(Span(start_abs, fold.span_end)),
                },
                after_blocks: Vec::new(),
                next: fold.next,
                tail_start: None,
            };
        }
    }

    if let Some(kv) = directive_property_line(text) {
        let props = vec![Property::parse2(kv)];
        let fold = if format == "org" {
            match fold_org_property_tail(
                source,
                i + 1,
                props,
                line.end,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                Ok(fold) => fold,
                Err(decision) => return decision,
            }
        } else {
            match fold_markdown_property_group(
                source,
                i + 1,
                props,
                line.end,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                Ok(fold) => fold,
                Err(decision) => return decision,
            }
        };
        return PropertyDrawerDecision::Emit {
            block: Block::Properties {
                props: fold.props,
                span: Some(Span(start_abs, fold.span_end)),
            },
            after_blocks: Vec::new(),
            next: fold.next,
            tail_start: None,
        };
    }

    let Some(name) = drawer_begin(text) else {
        return PropertyDrawerDecision::No;
    };
    if !drawer_opener_accepts_eol(line.eol) {
        return PropertyDrawerDecision::No;
    }
    if name == "properties" {
        let Some(close) = find_property_end(source, i, property_end_cursor) else {
            return generic_drawer_decision_at(
                source,
                i,
                start_abs,
                format,
                name,
                drawer_end_cursor,
            );
        };
        if range_has_lone_cr_before(&source.lines, i, close) {
            return PropertyDrawerDecision::No;
        }
        let mut props = Vec::new();
        for body in &source.lines[i + 1..close] {
            let Some(kv) = drawer_property(body.text) else {
                return generic_drawer_decision(source, i, format, name, drawer_end_cursor);
            };
            props.push(Property::parse1(kv));
        }
        let Ok(close_tail) = property_close_span_and_tail(
            source,
            close,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        ) else {
            return PropertyDrawerDecision::Delegate;
        };
        let (props, span_seed, after_blocks_seed, tail_start_seed) =
            if let Some((tail_props, tail_end)) =
                same_line_property_tail_props(&close_tail.after_blocks, format)
            {
                props.extend(tail_props);
                (props, tail_end, Vec::new(), None)
            } else {
                (
                    props,
                    close_tail.span_end,
                    close_tail.after_blocks,
                    close_tail.tail_start,
                )
            };
        let fold = if format == "org" {
            match fold_org_property_tail(
                source,
                close_tail.next,
                props,
                span_seed,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                Ok(fold) => fold,
                Err(decision) => return decision,
            }
        } else {
            match fold_markdown_property_group(
                source,
                close_tail.next,
                props,
                span_seed,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                Ok(fold) => fold,
                Err(decision) => return decision,
            }
        };
        let (after_blocks, tail_start) = if fold.next == close_tail.next {
            (after_blocks_seed, tail_start_seed)
        } else {
            (Vec::new(), None)
        };
        return PropertyDrawerDecision::Emit {
            block: Block::Properties {
                props: fold.props,
                span: Some(Span(start_abs, fold.span_end)),
            },
            after_blocks,
            next: fold.next,
            tail_start,
        };
    }

    generic_drawer_decision_at(source, i, start_abs, format, name, drawer_end_cursor)
}

fn same_line_property_tail_props(blocks: &[Block], format: &str) -> Option<(Vec<Property>, usize)> {
    match blocks {
        [Block::Directive {
            name,
            value,
            span: Some(Span(_, end)),
        }] => Some((vec![Property::parse2((name.clone(), value.clone()))], *end)),
        [Block::Properties {
            props,
            span: Some(Span(_, end)),
        }]
        // scan-owner: (a2) caller-owned same-line property tail — the split
        // helper can emit at most one property block for this tail, so checking
        // provenance walks only that already-emitted property vector once.
            if format != "org" || props.iter().all(Property::is_parse2) =>
        {
            Some((props.clone(), *end))
        }
        _ => None,
    }
}

fn generic_drawer_decision(
    source: &Source<'_>,
    i: usize,
    format: &str,
    name: String,
    drawer_end_cursor: &mut usize,
) -> PropertyDrawerDecision {
    generic_drawer_decision_at(
        source,
        i,
        source.lines[i].start,
        format,
        name,
        drawer_end_cursor,
    )
}

fn generic_drawer_decision_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    _format: &str,
    name: String,
    drawer_end_cursor: &mut usize,
) -> PropertyDrawerDecision {
    let Some(close) = find_drawer_end(source, i, drawer_end_cursor) else {
        return PropertyDrawerDecision::No;
    };
    if range_has_lone_cr_before(&source.lines, i, close) {
        return PropertyDrawerDecision::No;
    }
    PropertyDrawerDecision::Emit {
        block: Block::Drawer {
            name,
            span: Some(Span(start_abs, source.lines[close].end)),
        },
        after_blocks: Vec::new(),
        next: close + 1,
        tail_start: None,
    }
}

struct PropertyFold {
    props: Vec<Property>,
    next: usize,
    span_end: usize,
}

fn fold_markdown_property_group(
    source: &Source<'_>,
    mut cur: usize,
    mut props: Vec<Property>,
    mut span_end: usize,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Result<PropertyFold, PropertyDrawerDecision> {
    let mut has_parse2 = props.iter().any(Property::is_parse2);
    while cur < source.lines.len() {
        crate::metrics::scan_work(1);
        let line = &source.lines[cur];
        if line.eol == Eol::Cr && markdown_property_line(line.text).is_some() {
            break;
        }
        if let Some(kv) = markdown_property_line(line.text) {
            props.push(Property::parse1(kv));
            span_end = line.end;
            cur += 1;
            continue;
        }
        if let Some(kv) = directive_property_line(line.text) {
            props.push(Property::parse2(kv));
            has_parse2 = true;
            span_end = line.end;
            cur += 1;
            while cur < source.lines.len() && source.lines[cur].text.is_empty() {
                crate::metrics::scan_work(1);
                span_end = source.lines[cur].end;
                cur += 1;
            }
            continue;
        }
        if let Some((drawer_props, next, end)) = fold_adjacent_properties_drawer(
            source,
            cur,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        ) {
            props.extend(drawer_props);
            span_end = end;
            cur = next;
            continue;
        }
        if line.text.is_empty() {
            if has_parse2 {
                while cur < source.lines.len() && source.lines[cur].text.is_empty() {
                    crate::metrics::scan_work(1);
                    span_end = source.lines[cur].end;
                    cur += 1;
                }
                continue;
            }
            let mut k = cur + 1;
            while k < source.lines.len() && source.lines[k].text.is_empty() {
                crate::metrics::scan_work(1);
                k += 1;
            }
            if k < source.lines.len() {
                if let Some(kv) = directive_property_line(source.lines[k].text) {
                    props.push(Property::parse2(kv));
                    has_parse2 = true;
                    span_end = source.lines[k].end;
                    cur = k + 1;
                    while cur < source.lines.len() && source.lines[cur].text.is_empty() {
                        crate::metrics::scan_work(1);
                        span_end = source.lines[cur].end;
                        cur += 1;
                    }
                    continue;
                }
            }
        }
        break;
    }
    Ok(PropertyFold {
        props,
        next: cur,
        span_end,
    })
}

fn fold_org_property_tail(
    source: &Source<'_>,
    mut cur: usize,
    mut props: Vec<Property>,
    mut span_end: usize,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Result<PropertyFold, PropertyDrawerDecision> {
    let mut has_parse2 = props.iter().any(Property::is_parse2);
    loop {
        if cur >= source.lines.len() {
            break;
        }
        crate::metrics::scan_work(1);
        if source.lines[cur].text.is_empty() {
            let mut k = cur;
            while k < source.lines.len() && source.lines[k].text.is_empty() {
                crate::metrics::scan_work(1);
                k += 1;
            }
            if has_parse2
                || (k < source.lines.len()
                    && directive_property_line(source.lines[k].text).is_some())
            {
                while cur < k {
                    crate::metrics::scan_work(1);
                    span_end = source.lines[cur].end;
                    cur += 1;
                }
            } else {
                break;
            }
        }
        if cur >= source.lines.len() {
            break;
        }
        if org_properties_begin(source.lines[cur].text) {
            let Some(close) = find_property_end(source, cur, property_end_cursor) else {
                break;
            };
            if range_has_lone_cr_before(&source.lines, cur, close) {
                return Ok(PropertyFold {
                    props,
                    next: cur,
                    span_end,
                });
            }
            for body in &source.lines[cur + 1..close] {
                let Some(kv) = drawer_property(body.text) else {
                    return Ok(PropertyFold {
                        props,
                        next: cur,
                        span_end,
                    });
                };
                props.push(Property::parse1(kv));
            }
            let Ok(close_tail) = property_close_span_and_tail(
                source,
                close,
                "org",
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) else {
                return Err(PropertyDrawerDecision::Delegate);
            };
            if close_tail.tail_start.is_some() || !close_tail.after_blocks.is_empty() {
                return Ok(PropertyFold {
                    props,
                    next: cur,
                    span_end,
                });
            }
            span_end = close_tail.span_end;
            cur = close_tail.next;
            continue;
        }
        if let Some(kv) = directive_property_line(source.lines[cur].text) {
            props.push(Property::parse2(kv));
            has_parse2 = true;
            span_end = source.lines[cur].end;
            cur += 1;
            continue;
        }
        break;
    }
    Ok(PropertyFold {
        props,
        next: cur,
        span_end,
    })
}

fn fold_adjacent_properties_drawer(
    source: &Source<'_>,
    opener: usize,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Option<(Vec<Property>, usize, usize)> {
    let line = source.lines.get(opener)?;
    if !org_properties_begin(line.text) || !drawer_opener_accepts_eol(line.eol) {
        return None;
    }
    let close = find_property_end(source, opener, property_end_cursor)?;
    if range_has_lone_cr_before(&source.lines, opener, close) {
        return None;
    }
    let mut props = Vec::new();
    for body in &source.lines[opener + 1..close] {
        crate::metrics::scan_work(1);
        props.push(Property::parse1(drawer_property(body.text)?));
    }
    let close_tail = property_close_span_and_tail(
        source,
        close,
        "md",
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    )
    .ok()?;
    if close_tail.tail_start.is_some() || !close_tail.after_blocks.is_empty() {
        return None;
    }
    Some((props, close_tail.next, close_tail.span_end))
}

fn markdown_property_line(s: &str) -> Option<(String, String)> {
    let start = mldoc_spaces_len(s);
    let s = &s[start..];
    let found = s.find("::");
    crate::metrics::scan_work(found.map_or(s.len(), |p| p + 2));
    let pos = found?;
    let key = &s[..pos];
    if key.is_empty()
        || key
            .as_bytes()
            .iter()
            .any(|&b| b == b':' || !markdown_property_non_space_eol(b))
    {
        return None;
    }
    let rest = &s[pos + 2..];
    if let Some(value) = rest.strip_prefix(' ') {
        let value = &value[mldoc_spaces_len(value)..];
        return Some((
            key.to_string(),
            trim_markdown_property_value(value).to_string(),
        ));
    }
    rest.as_bytes()
        .iter()
        .all(|&b| mldoc_is_space(b))
        .then(|| (key.to_string(), String::new()))
}

fn directive_property_line(s: &str) -> Option<(String, String)> {
    let rest = mldoc_trim_spaces_start(s).strip_prefix("#+")?;
    let found = rest.find(':');
    crate::metrics::scan_work(found.map_or(rest.len(), |p| p + 1));
    let pos = found?;
    let key = &rest[..pos];
    crate::metrics::scan_work(key.len());
    if key.is_empty()
        || key
            .bytes()
            .any(|b| b == b':' || b == b'\n' || b == b'\r' || mldoc_is_space(b))
    {
        return None;
    }
    let value = mldoc_trim_spaces_start(&rest[pos + 1..]);
    crate::metrics::scan_work(key.len() + value.len());
    Some((key.to_string(), value.to_string()))
}

fn drawer_begin(s: &str) -> Option<String> {
    let inner = mldoc_trim_spaces_start(s)
        .strip_prefix(':')?
        .strip_suffix(':')?;
    if inner.is_empty() {
        return None;
    }
    crate::metrics::scan_work(inner.len());
    if inner
        .bytes()
        .any(|b| b == b':' || b == b' ' || b == b'\n' || b == b'\r')
    {
        return None;
    }
    crate::metrics::scan_work(inner.len());
    Some(inner.to_ascii_lowercase())
}

fn find_drawer_end(source: &Source<'_>, opener: usize, cursor: &mut usize) -> Option<usize> {
    let ends = &source.events.drawer_end_lines;
    while *cursor < ends.len() && ends[*cursor] <= opener {
        *cursor += 1;
        crate::metrics::scan_work(1);
    }
    ends.get(*cursor).copied()
}

fn find_property_end(source: &Source<'_>, opener: usize, cursor: &mut usize) -> Option<usize> {
    let ends = &source.events.property_end_lines;
    while *cursor < ends.len() && ends[*cursor] <= opener {
        *cursor += 1;
        crate::metrics::scan_work(1);
    }
    ends.get(*cursor).copied()
}

struct PropertyCloseTail {
    span_end: usize,
    next: usize,
    tail_start: Option<(usize, usize)>,
    after_blocks: Vec<Block>,
}

fn property_close_span_and_tail(
    source: &Source<'_>,
    close: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Result<PropertyCloseTail, ()> {
    let line = &source.lines[close];
    let rel_end = property_end_marker_len(line.text).ok_or(())?;
    let abs_end = line.start + rel_end;
    if rel_end < line.text.len() {
        let tail = &line.text[rel_end..];
        if let Some(split) = bounded_split_suffix_blocks(
            source,
            close,
            abs_end,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        ) {
            return Ok(PropertyCloseTail {
                span_end: abs_end,
                next: split.next,
                tail_start: split.tail_start,
                after_blocks: split.blocks,
            });
        }
        if let Some(split) = callout_container_split_at(source, close, abs_end, format) {
            return Ok(PropertyCloseTail {
                span_end: abs_end,
                next: split.next,
                tail_start: split.tail_start,
                after_blocks: split.blocks,
            });
        }
        if line.eol == Eol::Cr && format != "org" && markdown_property_line(tail).is_some() {
            return Ok(PropertyCloseTail {
                span_end: abs_end,
                next: close + 1,
                tail_start: Some((close, abs_end)),
                after_blocks: Vec::new(),
            });
        }
        if invalid_raw_html_tail_at(source, close, abs_end, raw_html_scan) {
            return Ok(PropertyCloseTail {
                span_end: abs_end,
                next: close + 1,
                tail_start: Some((close, abs_end)),
                after_blocks: Vec::new(),
            });
        }
        if unclosed_displayed_math_tail_at(source, close, abs_end)
            || rejected_fence_tail_at(source, close, abs_end, fence_cursor)
            || rejected_table_tail_at(source, close, abs_end, format)
            || rejected_regular_list_tail_at(source, close, abs_end, format)
            || unclosed_markdown_drawer_tail_at(source, close, abs_end, format)
            || rejected_markdown_property_tail_at(source, close, abs_end, format)
            || rejected_directive_property_tail_at(source, close, abs_end)
            || rejected_blockquote_tail_at(source, close, abs_end, format)
            || malformed_latex_tail_at(source, close, abs_end)
            || rejected_begin_tail_at(source, close, abs_end)
        {
            return Ok(PropertyCloseTail {
                span_end: abs_end,
                next: close + 1,
                tail_start: Some((close, abs_end)),
                after_blocks: Vec::new(),
            });
        }
        if directive_line(tail).is_some()
            || comment_line(tail, format).is_some()
            || heading_start(tail, format)
            || property_or_drawer_start(tail, format)
            || raw_html_tail_start(tail)
            || could_start_non_paragraph(tail, format)
        {
            return Err(());
        }
        Ok(PropertyCloseTail {
            span_end: abs_end,
            next: close + 1,
            tail_start: Some((close, abs_end)),
            after_blocks: Vec::new(),
        })
    } else {
        Ok(PropertyCloseTail {
            span_end: line.end,
            next: close + 1,
            tail_start: None,
            after_blocks: Vec::new(),
        })
    }
}

fn property_end_marker_len(text: &str) -> Option<usize> {
    let off = mldoc_spaces_len(text);
    text[off..]
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(":END:"))
        .then_some(off + 5)
}

fn org_properties_begin(s: &str) -> bool {
    mldoc_trim_spaces_start(s).eq_ignore_ascii_case(":PROPERTIES:")
}

fn property_or_drawer_start(s: &str, format: &str) -> bool {
    drawer_begin(s).is_some()
        || directive_property_start(s)
        || (format != "org" && markdown_property_start(mldoc_trim_spaces_start(s)))
}

fn directive_property_start(s: &str) -> bool {
    mldoc_trim_spaces_start(s).starts_with("#+")
}

fn drawer_opener_accepts_eol(eol: Eol) -> bool {
    matches!(eol, Eol::Lf | Eol::CrLf)
}

fn range_has_lone_cr_before(lines: &[Line<'_>], start: usize, end: usize) -> bool {
    lines[start..end].iter().any(|line| line.eol == Eol::Cr)
}

#[inline]
fn markdown_property_non_space_eol(b: u8) -> bool {
    !mldoc_is_space(b) && b != b'\n' && b != b'\r'
}

fn trim_markdown_property_value(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && markdown_property_trim(bytes[start]) {
        start += 1;
    }
    let leading_work = start + usize::from(start < bytes.len());
    while end > start && markdown_property_trim(bytes[end - 1]) {
        end -= 1;
    }
    let trailing_work = bytes.len() - end + usize::from(end > start);
    crate::metrics::scan_work(leading_work + trailing_work);
    &s[start..end]
}

#[inline]
fn markdown_property_trim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

enum TableDecision {
    Emit {
        block: Block,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    No,
}

struct TableRow<'a> {
    cells: Vec<TableCell<'a>>,
    consumed_end: usize,
    can_continue: bool,
}

struct TableCell<'a> {
    text: &'a str,
    start: usize,
}

struct TableSeparator<'a> {
    text: &'a str,
    consumed_end: usize,
}

// mldoc source: lib/syntax/table.ml. A table is repeated groups of row lines,
// where a separator line terminates the current group and is not itself a row.
// A trailing non-empty `#+TBLFM:` line is consumed by `boundaries_spec` but its
// EOL is left for the next paragraph parser.
// scan-owner: (a) consumed table run — the group loop advances `cur` over each
// accepted table/TBLFM line once; row cell splitting and inline parsing are over
// accepted row slices only.
fn table_sequence(source: &Source<'_>, i: usize, format: &str) -> TableDecision {
    let Some((block, next, tail_start)) =
        table_sequence_at(source, i, source.lines[i].start, format)
    else {
        return TableDecision::No;
    };
    TableDecision::Emit {
        block,
        next,
        tail_start,
    }
}

fn table_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
) -> Option<(Block, usize, Option<(usize, usize)>)> {
    let mut cur = i;
    let mut span_end = start_abs;
    let mut groups: Vec<Vec<TableRow<'_>>> = Vec::new();
    let mut aligns: Vec<Option<Align>> = Vec::new();
    let mut tail_start = None;
    let mut stop = false;

    while cur < source.lines.len() && !stop {
        let mut rows = Vec::new();
        let mut consumed_group = false;
        loop {
            let Some(line) = source.lines.get(cur) else {
                break;
            };
            let line_start = if cur == i { start_abs } else { line.start };
            if let Some(separator) = table_separator_line_from(line, line_start) {
                consumed_group = true;
                span_end = separator.consumed_end;
                if format != "org" && aligns.is_empty() {
                    aligns = table_separator_aligns(separator.text);
                }
                cur += 1;
                break;
            }
            let Some(row) = table_row_line_from(line, line_start) else {
                break;
            };
            consumed_group = true;
            span_end = row.consumed_end;
            let can_continue = row.can_continue;
            if !can_continue && row.consumed_end < line.end {
                tail_start = Some((cur, row.consumed_end));
            }
            rows.push(row);
            cur += 1;
            if !can_continue {
                stop = true;
                break;
            }
        }
        if !consumed_group {
            break;
        }
        groups.push(rows);
    }

    if groups.is_empty() {
        return None;
    }

    if !stop {
        if let Some(line) = source.lines.get(cur) {
            if let Some(tblfm_end) = table_tblfm_end(line) {
                span_end = tblfm_end;
                if tblfm_end < line.end {
                    tail_start = Some((cur, tblfm_end));
                }
                cur += 1;
            }
        }
    }

    Some((
        build_table_block(groups, aligns, start_abs, span_end, format),
        cur,
        tail_start,
    ))
}

fn table_line_start(line: &Line<'_>) -> bool {
    table_separator_line(line).is_some() || table_row_line(line).is_some()
}

fn table_separator_line<'a>(line: &Line<'a>) -> Option<TableSeparator<'a>> {
    table_separator_line_from(line, line.start)
}

fn table_separator_line_from<'a>(line: &Line<'a>, start_abs: usize) -> Option<TableSeparator<'a>> {
    if !matches!(line.eol, Eol::Lf | Eol::CrLf) {
        return None;
    }
    let rel = start_abs.checked_sub(line.start)?;
    let text = line.text.get(rel..)?;
    let ws = mldoc_spaces_len(text);
    let rest = text.get(ws..)?;
    let body = rest.strip_prefix('|')?;
    let bytes = body.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0usize;
    while i < bytes.len() && table_separator_body_byte(bytes[i]) {
        crate::metrics::scan_work(1);
        i += 1;
    }
    crate::metrics::scan_work(usize::from(i < bytes.len()));
    (i == bytes.len()).then_some(TableSeparator {
        text,
        consumed_end: line.end,
    })
}

fn table_row_line<'a>(line: &Line<'a>) -> Option<TableRow<'a>> {
    table_row_line_from(line, line.start)
}

fn table_row_line_from<'a>(line: &Line<'a>, start_abs: usize) -> Option<TableRow<'a>> {
    let rel = start_abs.checked_sub(line.start)?;
    let text = line.text.get(rel..)?;
    let ws = mldoc_spaces_len(text);
    let rest = text.get(ws..)?;
    let after_pipe = rest.strip_prefix('|')?;
    if after_pipe.is_empty() {
        return None;
    }
    let after_pipe_start = start_abs + ws + 1;
    let (trimmed, trimmed_start) = ocaml_trim_with_start(after_pipe, after_pipe_start);
    if trimmed.is_empty() || !trimmed.ends_with('|') {
        return None;
    }
    let inner = &trimmed[..trimmed.len() - 1];
    Some(TableRow {
        cells: split_table_cells(inner, trimmed_start),
        consumed_end: if matches!(line.eol, Eol::Lf | Eol::CrLf) {
            line.end
        } else {
            line_text_end(line)
        },
        can_continue: matches!(line.eol, Eol::Lf | Eol::CrLf),
    })
}

fn split_table_cells<'a>(inner: &'a str, inner_start: usize) -> Vec<TableCell<'a>> {
    let bytes = inner.as_bytes();
    let mut cells = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'|' {
            let raw = &inner[start..i];
            let (text, text_start) = ocaml_trim_with_start(raw, inner_start + start);
            cells.push(TableCell {
                text,
                start: text_start,
            });
            start = i + 1;
        }
        if i < bytes.len() {
            crate::metrics::scan_work(1);
        }
        i += 1;
    }
    cells
}

fn table_tblfm_end(line: &Line<'_>) -> Option<usize> {
    let ws = mldoc_spaces_len(line.text);
    let rest = line.text.get(ws..)?;
    let body = rest.strip_prefix("#+TBLFM:")?;
    (!body.is_empty()).then_some(line_text_end(line))
}

fn build_table_block(
    groups: Vec<Vec<TableRow<'_>>>,
    aligns: Vec<Option<Align>>,
    start: usize,
    end: usize,
    format: &str,
) -> Block {
    let mut parsed_groups: Vec<Vec<Vec<Vec<Inline>>>> = groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|row| parse_table_row(row, format))
                .collect()
        })
        .collect();

    let mut body_groups: Vec<Vec<Vec<Vec<Inline>>>> = Vec::new();
    let header = if parsed_groups.first().is_some_and(Vec::is_empty) {
        parsed_groups.remove(0);
        None
    } else {
        let mut first = parsed_groups.remove(0);
        let header = first.remove(0);
        if !first.is_empty() {
            body_groups.push(first);
        }
        Some(header)
    };
    body_groups.extend(parsed_groups);

    if let Some(first_group) = body_groups.first_mut() {
        if first_group.first().is_some_and(|row| is_table_col_row(row)) {
            first_group.remove(0);
        }
    }

    let rows = body_groups.into_iter().flatten().collect();
    Block::Table {
        header,
        rows,
        aligns: if format == "org" { Vec::new() } else { aligns },
        span: Some(Span(start, end)),
    }
}

fn parse_table_row(row: &TableRow<'_>, format: &str) -> Vec<Vec<Inline>> {
    row.cells
        .iter()
        .map(|cell| super::inline_at(cell.text, format, cell.start))
        .collect()
}

fn is_table_col_row(row: &[Vec<Inline>]) -> bool {
    row.iter().all(|cell| match cell.as_slice() {
        [] => true,
        [Inline::Plain { text, .. }] => matches!(text.as_str(), "/" | "<" | "" | ">"),
        _ => false,
    })
}

fn table_separator_aligns(text: &str) -> Vec<Option<Align>> {
    let (trimmed, _) = ocaml_trim_with_start(mldoc_trim_spaces_start(text), 0);
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut aligns = Vec::new();
    for cell in inner.split('|') {
        aligns.push(table_align_from_separator_cell(cell));
    }
    aligns
}

fn table_align_from_separator_cell(cell: &str) -> Option<Align> {
    let (cell, _) = ocaml_trim_with_start(cell, 0);
    let bytes = cell.as_bytes();
    let mut first = None;
    let mut last = None;
    let mut has_dash = false;
    for &b in bytes {
        if b == b' ' {
            continue;
        }
        first.get_or_insert(b);
        last = Some(b);
        has_dash |= b == b'-';
        crate::metrics::scan_work(1);
    }
    if !has_dash {
        return None;
    }
    match (first, last) {
        (Some(b':'), Some(b':')) => Some(Align::Center),
        (Some(b':'), _) => Some(Align::Left),
        (_, Some(b':')) => Some(Align::Right),
        _ => None,
    }
}

fn table_separator_body_byte(b: u8) -> bool {
    matches!(b, b'-' | b'+' | b'|' | b' ' | b':')
}

fn ocaml_trim_with_start(s: &str, abs_start: usize) -> (&str, usize) {
    let bytes = s.as_bytes();
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && ocaml_trim_byte(bytes[start]) {
        start += 1;
    }
    let leading_work = start + usize::from(start < bytes.len());
    while end > start && ocaml_trim_byte(bytes[end - 1]) {
        end -= 1;
    }
    let trailing_work = bytes.len() - end + usize::from(end > start);
    crate::metrics::scan_work(leading_work + trailing_work);
    (&s[start..end], abs_start + start)
}

enum FenceDecision {
    Emit { block: Block, next: usize },
    FallthroughParagraph,
    No,
}

// mldoc source: lib/syntax/block0.ml `fenced_code_block`. The opener consumes
// exactly the first three ```/~~~ bytes after mldoc spaces; extra run bytes are
// part of the info string. The closer is the first later line whose OCaml-trimmed
// text starts with either fence marker.
// scan-owner: (a) consumed-on-match fence block — opener and language inspect
// the opener line once, the monotone source fence cursor skips each indexed
// marker once, body copy covers accepted body bytes once, and trailing blank
// swallow advances the line cursor over each swallowed blank once.
fn fence_sequence(source: &Source<'_>, i: usize, cursor: &mut usize) -> FenceDecision {
    let line = &source.lines[i];
    let Some((_marker, info_start)) = fence_marker(line.text) else {
        return FenceDecision::No;
    };
    if line.eol == Eol::Cr {
        return FenceDecision::FallthroughParagraph;
    }
    let Some(close) = find_matching_fence(&source.events.fence_lines, cursor, i) else {
        return FenceDecision::FallthroughParagraph;
    };
    if lines_have_lone_cr(&source.lines[i + 1..close]) {
        return FenceDecision::FallthroughParagraph;
    }

    let code = if close > i + 1 {
        fenced_code_text(&source.lines[i + 1..close])
    } else {
        String::new()
    };
    let mut next = close + 1;
    let mut span_end = source.lines[close].end;
    while next < source.lines.len() && source.lines[next].text.is_empty() {
        crate::metrics::scan_work(1);
        span_end = source.lines[next].end;
        next += 1;
    }
    FenceDecision::Emit {
        block: Block::Src {
            lang: fence_lang(&line.text[info_start..]),
            code,
            span: Some(Span(line.start, span_end)),
        },
        next,
    }
}

// scan-owner: (a2) caller-owned fence body copy — the caller has already
// accepted a disjoint fence body interval, and each body line is copied once
// with one synthetic LF.
fn fenced_code_text(lines: &[Line<'_>]) -> String {
    let mut code = String::new();
    for line in lines {
        crate::metrics::scan_work(line.text.len() + 1);
        code.push_str(line.text);
        code.push('\n');
    }
    code
}

fn fence_marker(s: &str) -> Option<(u8, usize)> {
    let bytes = s.as_bytes();
    let ws = mldoc_spaces_len(s);
    let marker = *bytes.get(ws)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let mut end = ws;
    while end < bytes.len() && bytes[end] == marker {
        crate::metrics::scan_work(1);
        end += 1;
    }
    crate::metrics::scan_work(usize::from(end < bytes.len()));
    (end - ws >= 3).then_some((marker, ws + 3))
}

fn fence_lang(info: &str) -> String {
    let info = mldoc_trim_spaces_start(info);
    let bytes = info.as_bytes();
    let mut end = 0usize;
    while end < bytes.len() && !mldoc_is_space(bytes[end]) {
        crate::metrics::scan_work(1);
        end += 1;
    }
    crate::metrics::scan_work(usize::from(end < bytes.len()) + end);
    info[..end].to_string()
}

enum RawSrcExampleDecision {
    Emit { block: Block, next: usize },
    No,
}

enum CalloutContainerDecision {
    Emit { block: Block, next: usize },
    No,
}

// mldoc source: lib/syntax/block0.ml `block_name_options_parser` plus the
// `"src"`/`"example"` cases in `block_parse`. The closer is the first later
// OCaml-trimmed `#+END_...` line whose suffix has the opener name as a prefix.
// scan-owner: (a) consumed raw block — opener-name scan owns the current line,
// EndTrie closer lookup is monotone per prefix, body line collection walks the
// accepted body interval once, and trailing blank swallow advances once per blank.
fn raw_src_example_sequence(source: &Source<'_>, i: usize) -> RawSrcExampleDecision {
    let line = &source.lines[i];
    let Some(name) = block_begin_name(line.text) else {
        return RawSrcExampleDecision::No;
    };
    if !special_body_block_name(&name) {
        return RawSrcExampleDecision::No;
    }
    if line.eol == Eol::Cr {
        return RawSrcExampleDecision::No;
    }
    let Some(close) = source.events.callout_ends.first_after(&name, i) else {
        return RawSrcExampleDecision::No;
    };
    if lines_have_lone_cr(&source.lines[i + 1..close]) {
        return RawSrcExampleDecision::No;
    }
    crate::metrics::scan_work(close.saturating_sub(i + 1));
    let texts: Vec<&str> = (i + 1..close)
        .map(|line_idx| source.lines[line_idx].text)
        .collect();
    let code = crate::org::block_code_texts(&texts);
    let mut next = close + 1;
    let mut span_end = source.lines[close].end;
    while next < source.lines.len() && source.lines[next].text.is_empty() {
        crate::metrics::scan_work(1);
        span_end = source.lines[next].end;
        next += 1;
    }
    let span = Some(Span(line.start, span_end));
    let block = if name.eq_ignore_ascii_case("SRC") {
        Block::Src {
            lang: crate::org::begin_lang(line.text),
            code,
            span,
        }
    } else if name.eq_ignore_ascii_case("EXAMPLE") {
        Block::Example { code, span }
    } else if name.eq_ignore_ascii_case("EXPORT") {
        let (name, options) = begin_export_fields(line.text);
        Block::Export {
            name,
            options,
            content: code,
            span,
        }
    } else {
        debug_assert!(name.eq_ignore_ascii_case("COMMENT"));
        Block::CommentBlock {
            content: code,
            span,
        }
    };
    RawSrcExampleDecision::Emit { block, next }
}

fn special_body_block_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("SRC")
        || name.eq_ignore_ascii_case("EXAMPLE")
        || name.eq_ignore_ascii_case("EXPORT")
        || name.eq_ignore_ascii_case("COMMENT")
}

fn begin_line_falls_through_to_paragraph(source: &Source<'_>, i: usize) -> bool {
    rejected_begin_tail_at(source, i, source.lines[i].start)
}

// mldoc source: lib/syntax/block0.ml `block_parse` maps `#+BEGIN_QUOTE`
// to `Quote` and other non-raw callouts to `Custom`, with `src`/`example`,
// `export`, and `comment` handled by separate variants. The body is reparsed
// after `block_code` clear-indent. This v2 slice owns empty bodies and bodies
// whose transformed block-content output contains only constructs v2 already
// owns with the same in-body semantics; richer or suppressed body grammar returns
// ownership failure until it is source-transcribed directly.
// scan-owner: (a) consumed callout — opener-name scan owns the current line,
// EndTrie closer lookup is monotone per prefix, accepted body lines are copied
// and origin-mapped once, the nested paragraph parse owns that disjoint
// transformed body, and trailing blank swallow advances once per blank.
fn callout_container_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
) -> CalloutContainerDecision {
    callout_container_sequence_at(source, i, source.lines[i].start, format)
}

fn callout_container_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
) -> CalloutContainerDecision {
    let line = &source.lines[i];
    let Some(rel) = start_abs.checked_sub(line.start) else {
        return CalloutContainerDecision::No;
    };
    let Some(text) = line.text.get(rel..) else {
        return CalloutContainerDecision::No;
    };
    let Some(name) = block_begin_name(text) else {
        return CalloutContainerDecision::No;
    };
    if special_body_block_name(&name) {
        return CalloutContainerDecision::No;
    }
    if line.eol == Eol::Cr {
        return CalloutContainerDecision::No;
    }
    let Some(close) = source.events.callout_ends.first_after(&name, i) else {
        return CalloutContainerDecision::No;
    };
    if close != i + 1 {
        if lines_have_lone_cr(&source.lines[i + 1..close]) {
            return CalloutContainerDecision::No;
        }
    }

    let children = if close == i + 1 {
        Vec::new()
    } else if let Some(children) = try_parse_unindented_callout_body(source, i + 1, close, format) {
        children
    } else {
        let (body, map, transformed_body) = callout_body_text_and_map(source, i + 1, close);
        let Some(mut children) =
            try_parse_leaf_blocks_in(&body, format, BlockParseContext::BlockContent)
        else {
            return CalloutContainerDecision::No;
        };
        if rewrite_callout_suppressed_blocks(&mut children, &body, format).is_none() {
            return CalloutContainerDecision::No;
        }
        merge_adjacent_paragraph_blocks(&mut children);
        if format == "org" {
            merge_adjacent_org_fixed_width_examples(&mut children, &body);
        }
        if !callout_children_are_safe_block_content(&children) {
            return CalloutContainerDecision::No;
        }
        trim_paragraph_breaks_before_blocks(&mut children, format);
        remap_blocks_from_origin(&mut children, &body, source.input, &map);
        trim_callout_hiccup_spans(&mut children, source.input);
        if transformed_body {
            restore_transformed_leaf_block_span_starts(&mut children, source.input);
            clear_paragraph_block_spans(&mut children);
        }
        children
    };

    let mut next = close + 1;
    let span_end = source.lines[close].end;
    while next < source.lines.len() && source.lines[next].text.is_empty() {
        crate::metrics::scan_work(1);
        next += 1;
    }
    let span = Some(Span(start_abs, span_end));
    let block = if name.eq_ignore_ascii_case("QUOTE") {
        Block::Quote { children, span }
    } else {
        Block::Custom {
            name: name.to_ascii_lowercase(),
            children,
            span,
        }
    };
    CalloutContainerDecision::Emit { block, next }
}

struct CalloutFastFrame {
    name: String,
    start_abs: usize,
    children: Vec<Block>,
    para: QuoteFastParagraph,
    strip_total: usize,
    strip_pushed: bool,
    is_root: bool,
}

impl CalloutFastFrame {
    fn root(strip_total: usize, strip_pushed: bool) -> CalloutFastFrame {
        CalloutFastFrame {
            name: String::new(),
            start_abs: 0,
            children: Vec::new(),
            para: QuoteFastParagraph::new(),
            strip_total,
            strip_pushed,
            is_root: true,
        }
    }

    fn callout(
        name: String,
        start_abs: usize,
        strip_total: usize,
        strip_pushed: bool,
    ) -> CalloutFastFrame {
        CalloutFastFrame {
            name,
            start_abs,
            children: Vec::new(),
            para: QuoteFastParagraph::new(),
            strip_total,
            strip_pushed,
            is_root: false,
        }
    }
}

fn block_end_matches_name(text: &str, name: &str) -> bool {
    let t = mldoc_trim_spaces_start(text);
    let Some(suffix) = t.get(6..) else {
        return false;
    };
    starts_ci(t, "#+END_")
        && suffix.len() >= name.len()
        && suffix.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
}

fn block_end_line_start(text: &str) -> bool {
    starts_ci(mldoc_trim_spaces_start(text), "#+END_")
}

fn callout_fast_plain_line_safe(text: &str, format: &str) -> bool {
    let t = mldoc_trim_spaces_start(text);
    !t.is_empty()
        && !could_start_non_paragraph(text, format)
        && !matches!(
            t.as_bytes().first().copied(),
            Some(b'<' | b'|' | b':' | b'$' | b'`' | b'~' | b'\\' | b'[' | b'#' | b'-' | b'+')
        )
        && !t.as_bytes().first().is_some_and(|b| b.is_ascii_digit())
}

fn close_callout_fast_frame(
    stack: &mut Vec<CalloutFastFrame>,
    strip_seq: &mut StripSeqTree,
    close_line: &Line<'_>,
    source_input: &str,
    format: &str,
) -> Option<()> {
    let mut frame = stack.pop()?;
    if frame.is_root {
        return None;
    }
    if frame.strip_pushed {
        strip_seq.pop_positive();
    }
    frame.para.flush(&mut frame.children, source_input, format);
    trim_paragraph_breaks_before_blocks_shallow(&mut frame.children, format);
    let span = Some(Span(frame.start_abs, close_line.end));
    let block = if frame.name.eq_ignore_ascii_case("QUOTE") {
        Block::Quote {
            children: frame.children,
            span,
        }
    } else {
        Block::Custom {
            name: frame.name.to_ascii_lowercase(),
            children: frame.children,
            span,
        }
    };
    stack.last_mut()?.children.push(block);
    Some(())
}

fn callout_fast_line_view<'a>(
    line: &Line<'a>,
    strip_total: usize,
    strip_seq: &StripSeqTree,
) -> QuoteFastLine<'a> {
    let view = StripCtx::new(strip_total, strip_seq).view_text(line.text);
    let rel = line.text.len() - view.len();
    QuoteFastLine {
        text: view,
        abs: line.start + rel,
        text_end: line_text_end(line),
        line_end: line.end,
        eol: line.eol,
    }
}

// scan-owner: (a2) callout frame body — for clean bodies the source
// line cursor advances once through `[body_start, close)`, nested callout frames
// are pushed/popped without materializing child bodies, and clear-indent is a
// stack of increments queried through `StripSeqTree` rather than rescanning
// every active parent body.
fn try_parse_unindented_callout_body(
    source: &Source<'_>,
    body_start: usize,
    close: usize,
    format: &str,
) -> Option<Vec<Block>> {
    if body_start >= close {
        return Some(Vec::new());
    }

    let mut strip_seq = StripSeqTree::new();
    let root_strip = first_body_indent(source.lines[body_start].text);
    let root_pushed = strip_seq.push(root_strip);
    let mut transformed = root_strip != 0;
    let mut stack = vec![CalloutFastFrame::root(root_strip, root_pushed)];
    let mut i = body_start;
    while i < close {
        let line = &source.lines[i];
        let current_strip = stack.last()?.strip_total;
        let view = callout_fast_line_view(line, current_strip, &strip_seq);
        if stack.len() > 1 && block_end_matches_name(view.text, &stack.last()?.name) {
            close_callout_fast_frame(&mut stack, &mut strip_seq, line, source.input, format)?;
            i += 1;
            continue;
        }
        if block_end_line_start(view.text) {
            return None;
        }

        if let Some(name) = block_begin_name(view.text) {
            if special_body_block_name(&name) || line.eol == Eol::Cr {
                return None;
            }
            let top = stack.last_mut()?;
            top.para.flush(&mut top.children, source.input, format);
            let child_strip = if i + 1 < close {
                let next_view =
                    callout_fast_line_view(&source.lines[i + 1], current_strip, &strip_seq);
                first_body_indent(next_view.text)
            } else {
                0
            };
            let pushed = strip_seq.push(child_strip);
            transformed |= child_strip != 0;
            stack.push(CalloutFastFrame::callout(
                name,
                view.abs,
                current_strip + child_strip,
                pushed,
            ));
            i += 1;
            continue;
        }

        if current_strip == 0 {
            if let Some(block) = callout_fast_raw_html_line(source, i) {
                let top = stack.last_mut()?;
                top.para.flush(&mut top.children, source.input, format);
                top.children.push(block);
                i += 1;
                continue;
            }
        }

        if mldoc_trim_spaces_start(view.text).is_empty() {
            let top = stack.last_mut()?;
            top.para.push_line(view);
            i += 1;
            continue;
        }

        if !callout_fast_plain_line_safe(view.text, format) {
            return None;
        }
        {
            let top = stack.last_mut()?;
            top.para.push_line(view);
        }
        i += 1;
    }

    if stack.len() != 1 {
        return None;
    }
    let root = stack.last_mut()?;
    root.para.flush(&mut root.children, source.input, format);
    trim_paragraph_breaks_before_blocks_shallow(&mut root.children, format);
    if transformed {
        clear_paragraph_block_spans(&mut root.children);
    }
    Some(std::mem::take(&mut root.children))
}

fn callout_fast_raw_html_line(source: &Source<'_>, i: usize) -> Option<Block> {
    let line = &source.lines[i];
    let content_end = line_text_end(line);
    let ws = mldoc_spaces_len(line.text);
    let opener = line.start + ws;
    if opener >= content_end || !source.input[opener..].starts_with('<') {
        return None;
    }
    let extent = parse_raw_html_at_cached(source.input, opener, content_end, None)?;
    if extent.end != content_end {
        return None;
    }
    Some(Block::RawHtml {
        text: raw_html_capture_text(source.input, opener, extent.end),
        span: Some(Span(line.start, line.end)),
    })
}

fn callout_body_text_and_map(
    source: &Source<'_>,
    body_start: usize,
    close: usize,
) -> (String, OriginMap, bool) {
    let mut body = String::new();
    let mut map = OriginMap::new();
    let mut transformed = false;
    if body_start >= close {
        return (body, map, transformed);
    }
    let indent = first_body_indent(source.lines[body_start].text);
    for line_idx in body_start..close {
        let line = &source.lines[line_idx];
        let prefix = mldoc_ltrim_prefix_at_most(line.text, indent);
        let rel = if prefix >= indent {
            if line.text.len() > indent {
                indent
            } else {
                0
            }
        } else if prefix == line.text.len() {
            0
        } else {
            prefix
        };
        transformed |= rel != 0;
        let text = &line.text[rel..];
        let text_off = body.len();
        if !text.is_empty() {
            map.push(text_off, line.start + rel, text.len(), text.len());
            crate::metrics::scan_work(text.len());
            body.push_str(text);
        }
        let eol_start = line.start + line.text.len();
        let eol_len = line.end.saturating_sub(eol_start);
        if eol_len > 0 {
            map.push(body.len(), eol_start, 1, eol_len);
        }
        transformed |= eol_len != 1;
        crate::metrics::scan_work(1);
        body.push('\n');
    }
    (body, map, transformed)
}

enum BlockquoteDecision {
    Emit { block: Block, next: usize },
    Delegate,
    Paragraph,
    No,
}

// mldoc source: lib/syntax/block0.ml `md_blockquote`:
// `char '>' *> lines_while (...)`, followed by `block_content_parsers`.
// The collector strips the opener `>`, then for each accepted body line strips
// mldoc spaces, one optional extra `>`, more spaces, and appends a synthetic "\n".
// scan-owner: (a) consumed blockquote — accepted quote lines advance the source
// line cursor once; each copied body slice and synthetic newline is origin-mapped
// once, then the nested v2 block-content parse owns the disjoint body buffer.
fn markdown_blockquote_sequence(source: &Source<'_>, i: usize, format: &str) -> BlockquoteDecision {
    markdown_blockquote_sequence_at(source, i, source.lines[i].start, format)
}

fn markdown_blockquote_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
) -> BlockquoteDecision {
    let Some(first) = blockquote_line_content_from(&source.lines[i], true, start_abs) else {
        return if blockquote_line_start_from(&source.lines[i], start_abs) {
            BlockquoteDecision::Paragraph
        } else {
            BlockquoteDecision::No
        };
    };

    let mut body = String::new();
    let mut map = OriginMap::new();
    append_blockquote_line(source, i, first, &mut body, &mut map);

    let mut next = i + 1;
    if source.lines[i].eol != Eol::Cr {
        while next < source.lines.len() {
            let Some(content) = blockquote_line_content(&source.lines[next], false) else {
                break;
            };
            append_blockquote_line(source, next, content, &mut body, &mut map);
            if source.lines[next].eol == Eol::Cr {
                next += 1;
                break;
            }
            next += 1;
        }
    }

    let Some(mut children) = try_parse_quote_only_body(&body, format)
        .or_else(|| try_parse_leaf_blocks_in(&body, format, BlockParseContext::BlockContent))
    else {
        return BlockquoteDecision::Delegate;
    };
    let preserve_local_child_spans = format == "org" && blocks_contain_drawer(&children);
    if rewrite_callout_suppressed_blocks(&mut children, &body, format).is_none() {
        return BlockquoteDecision::Delegate;
    }
    merge_adjacent_paragraph_blocks(&mut children);
    if format == "org" {
        merge_adjacent_org_fixed_width_examples(&mut children, &body);
    }
    if !callout_children_are_safe_block_content(&children) {
        return BlockquoteDecision::Delegate;
    }
    trim_paragraph_breaks_before_blocks(&mut children, format);
    if preserve_local_child_spans {
        remap_block_inlines_from_origin_preserve_spans(&mut children, &body, source.input, &map);
    } else {
        remap_blocks_from_origin(&mut children, &body, source.input, &map);
        trim_callout_hiccup_spans(&mut children, source.input);
        clear_paragraph_block_spans(&mut children);
    }

    let span_end = source.lines[next - 1].end;
    while next < source.lines.len() && source.lines[next].text.is_empty() {
        crate::metrics::scan_work(1);
        next += 1;
    }

    BlockquoteDecision::Emit {
        block: Block::Quote {
            children,
            span: Some(Span(start_abs, span_end)),
        },
        next,
    }
}

fn blockquote_line_start(text: &str) -> bool {
    mldoc_trim_spaces_start(text).starts_with('>')
}

fn blockquote_line_start_from(line: &Line<'_>, start_abs: usize) -> bool {
    let Some(rel) = start_abs.checked_sub(line.start) else {
        return false;
    };
    let Some(text) = line.text.get(rel..) else {
        return false;
    };
    blockquote_line_start(text)
}

#[derive(Clone, Copy)]
struct BlockquoteLineContent {
    rel: usize,
    len: usize,
}

fn blockquote_line_content(line: &Line<'_>, first: bool) -> Option<BlockquoteLineContent> {
    blockquote_line_content_from(line, first, line.start)
}

fn blockquote_line_content_from(
    line: &Line<'_>,
    first: bool,
    start_abs: usize,
) -> Option<BlockquoteLineContent> {
    let mut rel = if first {
        let start_rel = start_abs.checked_sub(line.start)?;
        let text = line.text.get(start_rel..)?;
        let ws = mldoc_spaces_len(text);
        let rest = text[ws..].strip_prefix('>')?;
        start_rel + text.len() - rest.len()
    } else {
        0
    };
    let after_outer = &line.text[rel..];
    let ws = mldoc_spaces_len(after_outer);
    let after_ws = &after_outer[ws..];
    if line.eol != Eol::Eof {
        if let Some(after_gt) = after_ws.strip_prefix('>') {
            let after_gt_ws = mldoc_spaces_len(after_gt);
            if after_gt_ws == after_gt.len() {
                return Some(BlockquoteLineContent { rel: 0, len: 0 });
            }
        }
    }

    rel += ws;
    if line.text[rel..].starts_with('>') {
        rel += 1;
    }
    rel += mldoc_spaces_len(&line.text[rel..]);
    if rel >= line.text.len() {
        return None;
    }
    let content = &line.text[rel..];
    if quote_para_trigger(content) {
        return None;
    }
    Some(BlockquoteLineContent {
        rel,
        len: line.text.len() - rel,
    })
}

fn quote_para_trigger(content: &str) -> bool {
    content == "#"
        || content.starts_with("# ")
        || content == "-"
        || content.starts_with("- ")
        || content.starts_with("id:: ")
}

fn append_blockquote_line(
    source: &Source<'_>,
    line_idx: usize,
    content: BlockquoteLineContent,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    if content.len > 0 {
        let text_off = body.len();
        map.push(text_off, line.start + content.rel, content.len, content.len);
        body.push_str(&line.text[content.rel..content.rel + content.len]);
        crate::metrics::scan_work(content.len);
    }
    let text_off = body.len();
    let eol_start = line.start + line.text.len();
    let eol_len = line.end.saturating_sub(eol_start);
    if eol_len > 0 {
        map.push(text_off, eol_start, 1, eol_len);
    }
    crate::metrics::scan_work(1);
    body.push('\n');
}

#[derive(Clone, Copy)]
struct QuoteFastLine<'a> {
    text: &'a str,
    abs: usize,
    text_end: usize,
    line_end: usize,
    eol: Eol,
}

impl<'a> QuoteFastLine<'a> {
    fn from_line(line: &Line<'a>) -> QuoteFastLine<'a> {
        QuoteFastLine {
            text: line.text,
            abs: line.start,
            text_end: line_text_end(line),
            line_end: line.end,
            eol: line.eol,
        }
    }

    fn slice(self, rel: usize) -> QuoteFastLine<'a> {
        QuoteFastLine {
            text: &self.text[rel..],
            abs: self.abs + rel,
            text_end: self.text_end,
            line_end: self.line_end,
            eol: self.eol,
        }
    }
}

struct QuoteFastParagraph {
    start: Option<usize>,
    end: usize,
    text: String,
    map: OriginMap,
}

impl QuoteFastParagraph {
    fn new() -> QuoteFastParagraph {
        QuoteFastParagraph {
            start: None,
            end: 0,
            text: String::new(),
            map: OriginMap::new(),
        }
    }

    fn push_line(&mut self, line: QuoteFastLine<'_>) {
        self.start.get_or_insert(line.abs);
        if !line.text.is_empty() {
            let text_off = self.text.len();
            self.map
                .push(text_off, line.abs, line.text.len(), line.text.len());
            self.text.push_str(line.text);
            crate::metrics::scan_work(line.text.len());
        }
        let eol_len = line.line_end.saturating_sub(line.text_end);
        if eol_len > 0 {
            let text_off = self.text.len();
            self.map.push(text_off, line.text_end, 1, eol_len);
            self.text.push('\n');
            crate::metrics::scan_work(1);
        }
        self.end = line.line_end;
    }

    fn flush(&mut self, out: &mut Vec<Block>, body: &str, format: &str) {
        let Some(start) = self.start.take() else {
            return;
        };
        let mut inline = super::inline(&self.text, format);
        let mut cursor = OriginCursor::new();
        crate::source_map::remap_inlines(&mut inline, &self.text, body, &self.map, &mut cursor);
        out.push(Block::Paragraph {
            inline,
            span: Some(Span(start, self.end)),
        });
        self.end = 0;
        self.text.clear();
        self.map = OriginMap::new();
    }
}

struct QuoteFastFrame {
    children: Vec<Block>,
    para: QuoteFastParagraph,
    span_start: usize,
    span_end: usize,
    is_quote: bool,
}

impl QuoteFastFrame {
    fn root() -> QuoteFastFrame {
        QuoteFastFrame {
            children: Vec::new(),
            para: QuoteFastParagraph::new(),
            span_start: 0,
            span_end: 0,
            is_quote: false,
        }
    }

    fn quote(span_start: usize, span_end: usize) -> QuoteFastFrame {
        QuoteFastFrame {
            children: Vec::new(),
            para: QuoteFastParagraph::new(),
            span_start,
            span_end,
            is_quote: true,
        }
    }
}

// mldoc source: same `md_blockquote` first/continuation stripping as
// `blockquote_line_content_from`, but applied to an already stripped quote-body
// line view. This lets quote-only bodies be parsed as frames instead of by
// recursively materializing one body per nested quote.
fn quote_fast_line_content(line: QuoteFastLine<'_>, first: bool) -> Option<QuoteFastLine<'_>> {
    let mut rel = if first {
        let ws = mldoc_spaces_len(line.text);
        let rest = line.text[ws..].strip_prefix('>')?;
        line.text.len() - rest.len()
    } else {
        0
    };
    let after_outer = &line.text[rel..];
    let ws = mldoc_spaces_len(after_outer);
    let after_ws = &after_outer[ws..];
    if line.eol != Eol::Eof {
        if let Some(after_gt) = after_ws.strip_prefix('>') {
            let after_gt_ws = mldoc_spaces_len(after_gt);
            if after_gt_ws == after_gt.len() {
                return Some(QuoteFastLine {
                    text: "",
                    abs: line.abs,
                    text_end: line.text_end,
                    line_end: line.line_end,
                    eol: line.eol,
                });
            }
        }
    }

    rel += ws;
    if line.text[rel..].starts_with('>') {
        rel += 1;
    }
    rel += mldoc_spaces_len(&line.text[rel..]);
    if rel >= line.text.len() {
        return None;
    }
    let content = &line.text[rel..];
    if quote_para_trigger(content) {
        return None;
    }
    Some(line.slice(rel))
}

fn quote_fast_paragraph_safe(text: &str, format: &str) -> bool {
    let t = mldoc_trim_spaces_start(text);
    if t.is_empty() || could_start_non_paragraph(text, format) {
        return false;
    }
    !matches!(
        t.as_bytes().first().copied(),
        Some(b'<' | b'|' | b':' | b'$' | b'`' | b'~' | b'\\' | b'[' | b'#' | b'-' | b'+')
    ) && !t.as_bytes().first().is_some_and(|b| b.is_ascii_digit())
}

fn close_quote_fast_frame(stack: &mut Vec<QuoteFastFrame>, body: &str, format: &str) -> Option<()> {
    let mut frame = stack.pop()?;
    if !frame.is_quote {
        return None;
    }
    frame.para.flush(&mut frame.children, body, format);
    trim_paragraph_breaks_before_blocks(&mut frame.children, format);
    let block = Block::Quote {
        children: frame.children,
        span: Some(Span(frame.span_start, frame.span_end)),
    };
    stack.last_mut()?.children.push(block);
    Some(())
}

// scan-owner: (a2) consumed quote body — this owns only the already materialized
// blockquote body once. Active quote frames consume each physical body line by
// applying mldoc's continuation peel at most once per open frame that survives
// on that line; accepted paragraph bytes are copied once into paragraph buffers
// and origin-mapped back to the body. Mixed construct bodies return `None` and
// use the general v2 block-content parser.
fn try_parse_quote_only_body(body: &str, format: &str) -> Option<Vec<Block>> {
    let source = Source::scan(body);
    if source.lines.is_empty() {
        return Some(Vec::new());
    }

    let mut stack = vec![QuoteFastFrame::root()];
    for line in &source.lines {
        let full = QuoteFastLine::from_line(line);
        let mut views = Vec::with_capacity(stack.len());
        views.push(full);
        let mut failed_at = None;
        for depth in 1..stack.len() {
            match quote_fast_line_content(*views.last().unwrap(), false) {
                Some(content) => {
                    stack[depth].span_end = views.last().unwrap().line_end;
                    views.push(content);
                }
                None => {
                    failed_at = Some(depth);
                    break;
                }
            }
        }
        if let Some(depth) = failed_at {
            while stack.len() > depth {
                close_quote_fast_frame(&mut stack, body, format)?;
            }
            views.truncate(depth);
        }

        let mut view = *views.last().unwrap();
        loop {
            if let Some(content) = quote_fast_line_content(view, true) {
                if content.text.is_empty() {
                    return None;
                }
                let top = stack.last_mut()?;
                top.para.flush(&mut top.children, body, format);
                stack.push(QuoteFastFrame::quote(view.abs, view.line_end));
                view = content;
                continue;
            }

            if !blockquote_line_start(view.text) && !quote_fast_paragraph_safe(view.text, format) {
                return None;
            }
            stack.last_mut()?.para.push_line(view);
            break;
        }
    }

    while stack.len() > 1 {
        close_quote_fast_frame(&mut stack, body, format)?;
    }
    let root = stack.last_mut()?;
    root.para.flush(&mut root.children, body, format);
    Some(std::mem::take(&mut root.children))
}

fn callout_children_are_safe_block_content(blocks: &[Block]) -> bool {
    for block in blocks {
        match block {
            Block::Heading { .. }
            | Block::Bullet { .. }
            | Block::Drawer { .. }
            | Block::Properties { .. }
            | Block::FootnoteDef { .. } => return false,
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                if !callout_children_are_safe_block_content(children) {
                    return false;
                }
            }
            Block::Paragraph { .. }
            | Block::Src { .. }
            | Block::Export { .. }
            | Block::CommentBlock { .. }
            | Block::RawHtml { .. }
            | Block::DisplayedMath { .. }
            | Block::Directive { .. }
            | Block::Comment { .. }
            | Block::Example { .. }
            | Block::LatexEnv { .. }
            | Block::Hr { .. }
            | Block::Hiccup { .. }
            | Block::Results { .. }
            | Block::Table { .. }
            | Block::List { .. } => {}
        }
    }
    true
}

fn blocks_contain_drawer(blocks: &[Block]) -> bool {
    blocks.iter().any(|block| match block {
        Block::Drawer { .. } => true,
        Block::Quote { children, .. } | Block::Custom { children, .. } => {
            blocks_contain_drawer(children)
        }
        Block::List { items, .. } => list_items_contain_drawer(items),
        _ => false,
    })
}

fn list_items_contain_drawer(items: &[ListItem]) -> bool {
    items
        .iter()
        .any(|item| blocks_contain_drawer(&item.content) || list_items_contain_drawer(&item.items))
}

fn rewrite_callout_suppressed_blocks(
    blocks: &mut Vec<Block>,
    body: &str,
    format: &str,
) -> Option<()> {
    let mut rewritten = Vec::with_capacity(blocks.len());
    for mut block in std::mem::take(blocks) {
        match &mut block {
            Block::Heading { span, .. } | Block::Bullet { span, .. } => {
                rewritten.push(paragraph_from_body_span(body, format, *span)?);
            }
            Block::FootnoteDef { span, .. } => {
                rewritten.extend(suppressed_footnote_blocks(
                    body,
                    *span,
                    format,
                    SuppressedContentContext::BlockContent,
                )?);
            }
            Block::Drawer { span, .. } => {
                if format == "org" {
                    rewritten.extend(org_drawer_content_blocks(
                        body,
                        *span,
                        SuppressedContentContext::BlockContent,
                    )?);
                } else {
                    rewritten.extend(markdown_suppressed_property_span_blocks(
                        body,
                        *span,
                        SuppressedContentContext::BlockContent,
                    )?);
                }
            }
            Block::Properties { props, span } => {
                if props.iter().all(Property::is_parse2) {
                    rewritten.push(paragraph_from_body_span(body, format, *span)?);
                } else if format == "org" {
                    rewritten.extend(org_drawer_content_blocks(
                        body,
                        *span,
                        SuppressedContentContext::BlockContent,
                    )?);
                } else if !span_trimmed_starts_with(body, *span, '>') {
                    rewritten.extend(markdown_suppressed_property_span_blocks(
                        body,
                        *span,
                        SuppressedContentContext::BlockContent,
                    )?);
                } else {
                    rewritten.extend(markdown_block_content_blockquote_blocks(body, *span)?);
                }
            }
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                rewrite_callout_suppressed_blocks(children, body, format)?;
                rewritten.push(block);
            }
            Block::List { items, .. } => {
                for item in items {
                    rewrite_callout_suppressed_blocks(&mut item.content, body, format)?;
                    rewrite_callout_suppressed_list_items(&mut item.items, body, format)?;
                }
                rewritten.push(block);
            }
            _ => rewritten.push(block),
        }
    }
    *blocks = rewritten;
    Some(())
}

fn rewrite_callout_suppressed_list_items(
    items: &mut [ListItem],
    body: &str,
    format: &str,
) -> Option<()> {
    for item in items {
        rewrite_callout_suppressed_blocks(&mut item.content, body, format)?;
        rewrite_callout_suppressed_list_items(&mut item.items, body, format)?;
    }
    Some(())
}

// scan-owner: (a2) caller-owned suppressed footnote span — split the already
// accepted footnote-looking span after its first source line, emit that prefix
// once as paragraph text, and reparse the disjoint suffix once in the caller's
// block/list content context.
fn suppressed_footnote_blocks(
    body: &str,
    span: Option<Span>,
    format: &str,
    context: SuppressedContentContext,
) -> Option<Vec<Block>> {
    let Span(start, end) = span?;
    if start > end || end > body.len() {
        return None;
    }
    let source = Source::scan(&body[start..end]);
    let Some(first) = source.lines.first() else {
        return Some(Vec::new());
    };
    let first_end = start + first.end;
    let mut blocks = vec![paragraph_from_body_span(
        body,
        format,
        Some(Span(start, first_end)),
    )?];
    markdown_suppressed_context_chunk(&mut blocks, body, first_end, end, format, context)?;
    Some(blocks)
}

// scan-owner: (a2) caller-owned suppressed Markdown property/drawer span — the
// accepted span is walked once to split the paragraph prefix from the first
// `#+...` suffix, and the suffix is reparsed once in the caller's block/list
// content context.
fn markdown_suppressed_property_span_blocks(
    body: &str,
    span: Option<Span>,
    context: SuppressedContentContext,
) -> Option<Vec<Block>> {
    let Span(start, end) = span?;
    if start > end || end > body.len() {
        return None;
    }
    let source = Source::scan(&body[start..end]);
    let mut blocks = Vec::new();
    let mut para_start = None;
    let mut para_end = start;
    let mut i = 0usize;
    while i < source.lines.len() {
        let line = &source.lines[i];
        if mldoc_trim_spaces_start(line.text).starts_with("#+") {
            if let Some(paragraph_start) = para_start.take() {
                blocks.push(paragraph_from_body_span(
                    body,
                    "md",
                    Some(Span(paragraph_start, para_end)),
                )?);
            }
            markdown_suppressed_context_chunk(
                &mut blocks,
                body,
                start + line.start,
                end,
                "md",
                context,
            )?;
            return Some(blocks);
        }
        para_start.get_or_insert(start + line.start);
        para_end = start + line.end;
        i += 1;
    }
    if let Some(paragraph_start) = para_start {
        blocks.push(paragraph_from_body_span(
            body,
            "md",
            Some(Span(paragraph_start, para_end)),
        )?);
    }
    Some(blocks)
}

// scan-owner: (a2) caller-owned suppressed-context chunk — the caller passes a
// disjoint suffix from an already accepted suppressed span, and the nested parse
// owns that suffix buffer once.
fn markdown_suppressed_context_chunk(
    blocks: &mut Vec<Block>,
    body: &str,
    start: usize,
    end: usize,
    format: &str,
    context: SuppressedContentContext,
) -> Option<()> {
    if start >= end {
        return Some(());
    }
    let parse_context = match context {
        SuppressedContentContext::BlockContent => BlockParseContext::BlockContent,
        SuppressedContentContext::ListContent(mode) => BlockParseContext::ListContent(mode),
    };
    let mut chunk = try_parse_leaf_blocks_in(&body[start..end], format, parse_context)?;
    offset_blocks(&mut chunk, start);
    match context {
        SuppressedContentContext::BlockContent => {
            rewrite_callout_suppressed_blocks(&mut chunk, body, format)?;
        }
        SuppressedContentContext::ListContent(mode) => {
            rewrite_list_item_suppressed_blocks(&mut chunk, body, format, mode)?;
        }
    }
    blocks.extend(chunk);
    Some(())
}

fn markdown_block_content_blockquote_blocks(body: &str, span: Option<Span>) -> Option<Vec<Block>> {
    let Span(start, end) = span?;
    if start > end || end > body.len() {
        return None;
    }
    let source = Source::scan(&body[start..end]);
    let BlockquoteDecision::Emit { mut block, next } =
        markdown_blockquote_sequence_at(&source, 0, 0, "md")
    else {
        return None;
    };
    if next != source.lines.len() {
        return None;
    }
    offset_block(&mut block, start);
    align_descendant_quote_span_starts(&mut block);
    Some(vec![block])
}

fn align_descendant_quote_span_starts(block: &mut Block) {
    let Block::Quote {
        children,
        span: Some(Span(start, _)),
    } = block
    else {
        return;
    };
    let start = *start;
    for child in children {
        set_quote_span_start_recursive(child, start);
    }
}

fn set_quote_span_start_recursive(block: &mut Block, start: usize) {
    match block {
        Block::Quote { children, span } => {
            if let Some(Span(child_start, _)) = span {
                *child_start = start;
            }
            for child in children {
                set_quote_span_start_recursive(child, start);
            }
        }
        Block::Custom { children, .. } => {
            for child in children {
                set_quote_span_start_recursive(child, start);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for child in &mut item.content {
                    set_quote_span_start_recursive(child, start);
                }
                set_quote_span_start_in_items(&mut item.items, start);
            }
        }
        _ => {}
    }
}

fn set_quote_span_start_in_items(items: &mut [ListItem], start: usize) {
    for item in items {
        for child in &mut item.content {
            set_quote_span_start_recursive(child, start);
        }
        set_quote_span_start_in_items(&mut item.items, start);
    }
}

fn paragraph_from_body_span(body: &str, format: &str, span: Option<Span>) -> Option<Block> {
    let Span(start, end) = span?;
    if start > end || end > body.len() {
        return None;
    }
    let inline = super::inline_at(&body[start..end], format, start);
    Some(Block::Paragraph {
        inline,
        span: Some(Span(start, end)),
    })
}

fn span_trimmed_starts_with(body: &str, span: Option<Span>, needle: char) -> bool {
    let Some(Span(start, end)) = span else {
        return false;
    };
    if start > end || end > body.len() {
        return false;
    }
    mldoc_trim_spaces_start(&body[start..end]).starts_with(needle)
}

fn merge_adjacent_paragraph_blocks(blocks: &mut Vec<Block>) {
    let mut merged = Vec::with_capacity(blocks.len());
    for mut block in std::mem::take(blocks) {
        match (&mut block, merged.last_mut()) {
            (
                Block::Paragraph { inline, span },
                Some(Block::Paragraph {
                    inline: prev_inline,
                    span: prev_span,
                }),
            ) if paragraph_spans_touch(*prev_span, *span) => {
                prev_inline.append(inline);
                if let (Some(Span(_, prev_end)), Some(Span(_, end))) =
                    (prev_span.as_mut(), span.as_ref())
                {
                    *prev_end = *end;
                }
            }
            _ => merged.push(block),
        }
    }
    *blocks = merged;
}

fn merge_adjacent_org_fixed_width_examples(blocks: &mut Vec<Block>, body: &str) {
    for block in blocks.iter_mut() {
        match block {
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                merge_adjacent_org_fixed_width_examples(children, body);
            }
            _ => {}
        }
    }

    let mut merged: Vec<Block> = Vec::with_capacity(blocks.len());
    for block in std::mem::take(blocks) {
        match (merged.last_mut(), &block) {
            (
                Some(Block::Example {
                    code: prev_code,
                    span: prev_span,
                }),
                Block::Example { code, span },
            ) if org_fixed_width_example_spans_touch(body, *prev_span, *span) => {
                prev_code.push_str(code);
                if let (Some(Span(_, prev_end)), Some(Span(_, end))) =
                    (prev_span.as_mut(), span.as_ref())
                {
                    *prev_end = *end;
                }
            }
            _ => merged.push(block),
        }
    }
    *blocks = merged;
}

// mldoc source: lib/syntax/block0.ml block-content parsers omit
// Property_Drawer/Drawer and then parse colon-start physical lines through
// `verbatim`, whose `lines_starts_with (char ':')` loop coalesces adjacent
// fixed-width lines. Suppressed drawer/property rewrites can temporarily create
// separate Example nodes for adjacent colon spans, so coalesce only touching
// spans that both still point at colon-start source. Explicit BEGIN_EXAMPLE
// blocks are not merged by this helper.
// scan-owner: (a2) caller-owned block list — the post-rewrite merge walks each
// emitted node once and checks only the first non-space byte of adjacent spans.
fn org_fixed_width_example_spans_touch(
    body: &str,
    left: Option<Span>,
    right: Option<Span>,
) -> bool {
    let (Some(Span(left_start, left_end)), Some(Span(right_start, right_end))) = (left, right)
    else {
        return false;
    };
    left_start <= left_end
        && right_start <= right_end
        && left_end == right_start
        && span_trimmed_starts_with(body, Some(Span(left_start, left_end)), ':')
        && span_trimmed_starts_with(body, Some(Span(right_start, right_end)), ':')
}

fn paragraph_spans_touch(left: Option<Span>, right: Option<Span>) -> bool {
    match (left, right) {
        (Some(Span(_, left_end)), Some(Span(right_start, _))) => left_end == right_start,
        _ => false,
    }
}

fn trim_paragraph_breaks_before_blocks(blocks: &mut Vec<Block>, format: &str) {
    trim_paragraph_breaks_before_blocks_shallow(blocks, format);
    for block in blocks {
        match block {
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                trim_paragraph_breaks_before_blocks(children, format);
            }
            Block::List { items, .. } => {
                for item in items {
                    trim_paragraph_breaks_before_blocks(&mut item.content, format);
                }
            }
            _ => {}
        }
    }
}

fn trim_paragraph_breaks_before_blocks_shallow(blocks: &mut Vec<Block>, format: &str) {
    if blocks.len() < 2 {
        return;
    }
    let old = std::mem::take(blocks);
    let mut iter = old.into_iter().peekable();
    while let Some(mut block) = iter.next() {
        let trim_before_next_block = iter.peek().is_some_and(|next| !match next {
            Block::Paragraph { .. } | Block::LatexEnv { .. } | Block::Results { .. } => true,
            Block::Comment { .. } => format != "org",
            _ => false,
        });
        let mut drop_block = false;
        if trim_before_next_block {
            if let Block::Paragraph { inline, span } = &mut block {
                if inline.is_empty() {
                    drop_block = true;
                }
                if matches!(inline.last(), Some(Inline::Break { .. })) {
                    inline.pop();
                    if let (Some(Span(start, end)), Some(last_end)) =
                        (span.as_mut(), inline.last().and_then(inline_span_end))
                    {
                        *end = last_end.max(*start);
                    }
                    drop_block = inline.is_empty();
                }
            }
        }
        if !drop_block {
            blocks.push(block);
        }
    }
}

fn inline_span_end(node: &Inline) -> Option<usize> {
    match node {
        Inline::Plain { span, .. }
        | Inline::Emphasis { span, .. }
        | Inline::Code { span, .. }
        | Inline::Verbatim { span, .. }
        | Inline::Break { span }
        | Inline::HardBreak { span }
        | Inline::Link { span, .. }
        | Inline::NestedLink { span, .. }
        | Inline::Target { span, .. }
        | Inline::Macro { span, .. }
        | Inline::ExportSnippet { span, .. }
        | Inline::Latex { span, .. }
        | Inline::Timestamp { span, .. }
        | Inline::Cookie { span, .. }
        | Inline::Fnref { span, .. }
        | Inline::Subscript { span, .. }
        | Inline::Superscript { span, .. }
        | Inline::Tag { span, .. }
        | Inline::InlineHtml { span, .. }
        | Inline::Email { span, .. }
        | Inline::Entity { span, .. }
        | Inline::Hiccup { span, .. } => span.as_ref().map(|span| span.1),
    }
}

fn trim_callout_hiccup_spans(blocks: &mut [Block], source_input: &str) {
    for block in blocks {
        match block {
            Block::Hiccup { v, span } => {
                if let Some(Span(start, end)) = span {
                    if *start <= *end && *end <= source_input.len() {
                        let ws = mldoc_spaces_len(&source_input[*start..*end]);
                        let text_end = start.saturating_add(ws).saturating_add(v.len());
                        if text_end <= *end {
                            *end = text_end;
                        }
                    }
                }
            }
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                trim_callout_hiccup_spans(children, source_input);
            }
            Block::List { items, .. } => {
                for item in items {
                    trim_callout_hiccup_spans(&mut item.content, source_input);
                }
            }
            _ => {}
        }
    }
}

fn restore_transformed_leaf_block_span_starts(blocks: &mut [Block], source_input: &str) {
    for block in blocks {
        match block {
            Block::Src { span, .. }
            | Block::Export { span, .. }
            | Block::CommentBlock { span, .. }
            | Block::RawHtml { span, .. }
            | Block::DisplayedMath { span, .. }
            | Block::Drawer { span, .. }
            | Block::Directive { span, .. }
            | Block::Comment { span, .. }
            | Block::Example { span, .. }
            | Block::LatexEnv { span, .. }
            | Block::Properties { span, .. }
            | Block::Hr { span }
            | Block::Hiccup { span, .. }
            | Block::Table { span, .. }
            | Block::FootnoteDef { span, .. }
            | Block::Heading { span, .. }
            | Block::Bullet { span, .. }
            | Block::Results { span } => restore_span_start_to_line_indent(span, source_input),
            Block::Quote { children, span } | Block::Custom { children, span, .. } => {
                restore_span_start_to_line_indent(span, source_input);
                restore_transformed_leaf_block_span_starts(children, source_input);
            }
            Block::List { items, span } => {
                restore_span_start_to_line_indent(span, source_input);
                for item in items {
                    restore_transformed_leaf_block_span_starts(&mut item.content, source_input);
                    restore_list_item_transformed_span_starts(&mut item.items, source_input);
                }
            }
            Block::Paragraph { .. } => {}
        }
    }
}

fn restore_list_item_transformed_span_starts(items: &mut [ListItem], source_input: &str) {
    for item in items {
        restore_transformed_leaf_block_span_starts(&mut item.content, source_input);
        restore_list_item_transformed_span_starts(&mut item.items, source_input);
    }
}

fn restore_span_start_to_line_indent(span: &mut Option<Span>, source_input: &str) {
    let Some(Span(start, end)) = span else {
        return;
    };
    if *start > *end || *end > source_input.len() {
        return;
    }
    let line_start = source_line_start(source_input, *start);
    if source_input.as_bytes()[line_start..*start]
        .iter()
        .all(|&b| matches!(b, b' ' | b'\t' | b'\x0c'))
    {
        *start = line_start;
    }
}

fn source_line_start(input: &str, pos: usize) -> usize {
    let bytes = input.as_bytes();
    let mut i = pos.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' && bytes[i - 1] != b'\r' {
        i -= 1;
    }
    i
}

fn block_begin_name(s: &str) -> Option<String> {
    let t = mldoc_trim_spaces_start(s);
    if !t.get(..8)?.eq_ignore_ascii_case("#+BEGIN_") {
        return None;
    }
    let rest = &t[8..];
    let mut end = 0usize;
    let bytes = rest.as_bytes();
    while end < bytes.len() && !mldoc_is_space(bytes[end]) {
        crate::metrics::scan_work(1);
        end += 1;
    }
    crate::metrics::scan_work(usize::from(end < bytes.len()) + end);
    (end > 0).then(|| rest[..end].to_string())
}

// mldoc source: lib/syntax/block0.ml `verbatim` uses
// `lines_starts_with (char ':')`, where `lines_starts_with` is
// `lines_while ((spaces *> p <* spaces) *> optional_line)`.
// Thus Org fixed-width blocks strip mldoc spaces before the colon and after it,
// preserve the rest of each physical line, append one `\n` per accepted line,
// and only coalesce lines whose separator was LF/CRLF/EOF-compatible. Lone-CR
// colon lines become separate examples because mldoc `eol` does not accept lone
// CR in the line-loop continuation.
// scan-owner: (a) consumed fixed-width run — the line cursor advances over each
// accepted `:` line once; content copies are over disjoint line suffixes.
fn org_verbatim_sequence(source: &Source<'_>, i: usize, format: &str) -> Option<(Block, usize)> {
    org_verbatim_sequence_at(source, i, source.lines[i].start, format)
}

fn org_verbatim_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
) -> Option<(Block, usize)> {
    if format != "org" {
        return None;
    }
    let mut code = String::new();
    let mut span_end = start_abs;
    let mut cur = i;
    while cur < source.lines.len() {
        let line = &source.lines[cur];
        let text = if cur == i {
            let rel = start_abs.checked_sub(line.start)?;
            line.text.get(rel..)?
        } else {
            line.text
        };
        let Some(content) = org_verbatim_content(text) else {
            break;
        };
        crate::metrics::scan_work(content.len() + 1);
        code.push_str(content);
        code.push('\n');
        span_end = line.end;
        cur += 1;
        if line.eol == Eol::Cr {
            break;
        }
        let mut consumed_blank_separator = false;
        while cur < source.lines.len()
            && source.lines[cur].text.is_empty()
            && source.lines[cur].eol != Eol::Cr
        {
            span_end = source.lines[cur].end;
            cur += 1;
            consumed_blank_separator = true;
            crate::metrics::scan_work(1);
        }
        if consumed_blank_separator {
            break;
        }
    }
    (cur > i).then(|| {
        (
            Block::Example {
                code,
                span: Some(Span(start_abs, span_end)),
            },
            cur,
        )
    })
}

fn org_verbatim_line(s: &str) -> bool {
    org_verbatim_content(s).is_some()
}

fn org_verbatim_content(s: &str) -> Option<&str> {
    let off = mldoc_spaces_len(s);
    crate::metrics::scan_work(usize::from(off < s.len()));
    let rest = s[off..].strip_prefix(':')?;
    crate::metrics::scan_work(1);
    Some(mldoc_trim_spaces_start(rest))
}

// mldoc source: lib/syntax/footnote.ml. Top-level `Footnote.parse` runs after
// generic block parsing and before lists/HR/comment/paragraph; block-content and
// list-content parser groups do not include it, so v2 only calls this sequence
// in `BlockParseContext::Document`. The marker grammar is format-specific, while
// `footnote_definition` is shared: each body line is `spaces *> satisfy non_eol`
// followed by `line`, so the body needs at least two bytes after leading spaces
// and stops before lines whose first non-space byte is `-`, `*`, `#`, or `[`.
// scan-owner: (a) consumed footnote definition — the marker scan owns the first
// line, the body cursor advances over each accepted continuation line once, the
// origin map records each copied body slice and joiner once, and trailing blank
// absorption advances only over the consumed blank suffix.
fn footnote_sequence(source: &Source<'_>, i: usize, format: &str) -> Option<(Block, usize)> {
    footnote_sequence_at(source, i, source.lines[i].start, format)
}

fn footnote_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
) -> Option<(Block, usize)> {
    let line = &source.lines[i];
    let rel = start_abs.checked_sub(line.start)?;
    let text = line.text.get(rel..)?;
    let (name, first_body_rel) = footnote_marker(text, format)?;
    let (content_rel, content_len) = footnote_body_content(&text[first_body_rel..], line.eol)?;

    let mut body = String::new();
    let mut map = OriginMap::new();
    append_footnote_content(
        source,
        i,
        rel + first_body_rel + content_rel,
        content_len,
        &mut body,
        &mut map,
    );

    let mut last_content = i;
    let mut next = i + 1;
    while next < source.lines.len() {
        let line = &source.lines[next];
        let Some((rel, len)) = footnote_body_content(line.text, line.eol) else {
            break;
        };
        append_footnote_joiner(source, last_content, &mut body, &mut map);
        append_footnote_content(source, next, rel, len, &mut body, &mut map);
        last_content = next;
        next += 1;
    }

    let mut span_end = source.lines[last_content].end;
    while next < source.lines.len()
        && source.lines[next].text.is_empty()
        && source.lines[next].eol != Eol::Eof
    {
        crate::metrics::scan_work(1);
        span_end = source.lines[next].end;
        next += 1;
    }

    let mut inline = super::inline(&body, format);
    let mut cursor = OriginCursor::new();
    crate::source_map::remap_inlines(&mut inline, &body, source.input, &map, &mut cursor);

    Some((
        Block::FootnoteDef {
            name,
            inline,
            span: Some(Span(start_abs, span_end)),
        },
        next,
    ))
}

fn footnote_marker(text: &str, format: &str) -> Option<(String, usize)> {
    if format == "org" {
        org_footnote_marker(text)
    } else {
        markdown_footnote_marker(text)
    }
}

fn org_footnote_marker(text: &str) -> Option<(String, usize)> {
    let start = mldoc_spaces_len(text);
    let rest = text[start..].strip_prefix("[fn:")?;
    let end = match rest.find(']') {
        Some(end) => {
            crate::metrics::scan_work(end + 1);
            end
        }
        None => {
            crate::metrics::scan_work(rest.len());
            return None;
        }
    };
    let name = &rest[..end];
    if name.is_empty() || name.contains('\r') || name.contains('\n') {
        return None;
    }
    let mut body_rel = start + 4 + end + 1;
    body_rel += mldoc_spaces_len(&text[body_rel..]);
    Some((name.to_string(), body_rel))
}

fn markdown_footnote_marker(text: &str) -> Option<(String, usize)> {
    let start = mldoc_spaces_len(text);
    let rest = text[start..].strip_prefix("[^")?;
    let bytes = rest.as_bytes();
    let mut end = 0usize;
    while end < bytes.len() {
        let b = bytes[end];
        if b == b']' {
            break;
        }
        if b == b'\r' || b == b'\n' || mldoc_is_space(b) {
            return None;
        }
        crate::metrics::scan_work(1);
        end += 1;
    }
    crate::metrics::scan_work(usize::from(end < bytes.len()));
    if end == 0 || bytes.get(end) != Some(&b']') {
        return None;
    }
    if bytes.get(end + 1) != Some(&b':') {
        return None;
    }
    let mut body_rel = start + 2 + end + 2;
    body_rel += mldoc_spaces_len(&text[body_rel..]);
    Some((rest[..end].to_string(), body_rel))
}

fn footnote_definition_start(text: &str, eol: Eol, format: &str) -> bool {
    let Some((_name, body_rel)) = footnote_marker(text, format) else {
        return false;
    };
    footnote_body_content(&text[body_rel..], eol).is_some()
}

fn footnote_body_content(text: &str, eol: Eol) -> Option<(usize, usize)> {
    if eol == Eol::Cr {
        return None;
    }
    let start = mldoc_spaces_len(text);
    let rest = text[start..].as_bytes();
    let first = *rest.first()?;
    if matches!(first, b'-' | b'*' | b'#' | b'[' | b'\r' | b'\n') {
        return None;
    }
    crate::metrics::scan_work(rest.len());
    if rest.len() < 2 {
        return None;
    }
    Some((start, rest.len()))
}

fn append_footnote_content(
    source: &Source<'_>,
    line_idx: usize,
    rel: usize,
    len: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let text_off = body.len();
    let src_off = line.start + rel;
    map.push(text_off, src_off, len, len);
    body.push_str(&line.text[rel..rel + len]);
    crate::metrics::scan_work(len);
}

fn append_footnote_joiner(
    source: &Source<'_>,
    line_idx: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let text_off = body.len();
    let eol_start = line.start + line.text.len();
    let eol_len = line.end.saturating_sub(eol_start);
    if eol_len > 0 {
        map.push(text_off, eol_start, 1, eol_len);
    }
    crate::metrics::scan_work(1);
    body.push('\n');
}

// mldoc source: lib/syntax/lists0.ml tries regular Markdown lists before
// `Markdown_definition.parse`; lib/syntax/markdown_definition.ml then parses
// `many1 (definition_parse <* optional eols)`. Once this parser has committed,
// later heading/list-looking term lines may become additional definition terms.
// scan-owner: (a) consumed Markdown definition list — the term/body cursor
// advances over each consumed line once; each definition body copy and synthetic
// joiner is recorded once in a local origin map before one inline remap pass.
fn markdown_definition_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
) -> Option<(Block, usize)> {
    if format == "org" || !markdown_definition_can_start(source, i, true) {
        return None;
    }

    let mut items = Vec::new();
    let mut term_idx = i;
    let mut span_end = source.lines[i].start;
    loop {
        let (term_rel, term_text) = markdown_definition_term(&source.lines[term_idx], false)?;
        let term_start = source.lines[term_idx].start + term_rel;
        let name = super::inline_at(term_text, "md", term_start);

        let mut content = Vec::new();
        let mut cur = term_idx + 1;
        while cur < source.lines.len() {
            let Some((rel, len)) = markdown_definition_opener(&source.lines[cur]) else {
                break;
            };
            let mut item_text = String::new();
            let mut item_map = OriginMap::new();
            append_definition_content(source, cur, rel, len, &mut item_text, &mut item_map);
            let mut last_content = cur;
            cur += 1;

            while cur < source.lines.len() {
                let Some((rel, len)) = markdown_definition_continuation(&source.lines[cur]) else {
                    break;
                };
                append_definition_joiner(source, last_content, &mut item_text, &mut item_map);
                append_definition_content(source, cur, rel, len, &mut item_text, &mut item_map);
                last_content = cur;
                cur += 1;
            }
            span_end = source.lines[last_content].end;
            content.push(markdown_definition_paragraph(
                item_text,
                item_map,
                source.input,
            ));
        }

        items.push(ListItem {
            ordered: false,
            number: None,
            indent: 0,
            content,
            items: Vec::new(),
            name,
            checkbox: None,
        });

        while cur < source.lines.len()
            && source.lines[cur].text.is_empty()
            && source.lines[cur].eol != Eol::Eof
        {
            crate::metrics::scan_work(1);
            span_end = source.lines[cur].end;
            cur += 1;
        }
        if markdown_definition_can_start(source, cur, false) {
            term_idx = cur;
        } else {
            return Some((
                Block::List {
                    items,
                    span: Some(Span(source.lines[i].start, span_end)),
                },
                cur,
            ));
        }
    }
}

fn markdown_definition_can_start(source: &Source<'_>, i: usize, initial: bool) -> bool {
    if i + 1 >= source.lines.len() {
        return false;
    }
    markdown_definition_term(&source.lines[i], initial).is_some()
        && markdown_definition_opener(&source.lines[i + 1]).is_some()
}

fn markdown_definition_term<'a>(line: &'a Line<'a>, initial: bool) -> Option<(usize, &'a str)> {
    if !matches!(line.eol, Eol::Lf | Eol::CrLf) {
        return None;
    }
    let rel = mldoc_spaces_len(line.text);
    let text = &line.text[rel..];
    if text.is_empty() {
        return None;
    }
    if initial
        && (markdown_regular_list_start(text)
            || ordered_list_start(text)
            || markdown_definition_unowned_prior_block_start(text))
    {
        return None;
    }
    Some((rel, text))
}

fn markdown_definition_unowned_prior_block_start(t: &str) -> bool {
    starts_ci(t, "#+BEGIN_") || t.starts_with('>')
}

fn markdown_regular_list_start(t: &str) -> bool {
    let bytes = t.as_bytes();
    matches!(bytes.first(), Some(b'*' | b'+')) && bytes.get(1).is_some_and(|&b| mldoc_is_space(b))
}

enum ListDecision {
    Emit { block: Block, next: usize },
    Delegate,
    Paragraph,
    No,
}

struct RegularListMarker {
    ordered: bool,
    number: Option<u32>,
    checkbox: Option<bool>,
    indent: u32,
    body_start: usize,
}

#[derive(Clone, Copy)]
struct ListCollapseFloor {
    flat_len: usize,
    line_idx: usize,
}

#[derive(Clone, Copy)]
struct ListScope {
    indent: u32,
    committed: usize,
    empty_floor: ListCollapseFloor,
}

// mldoc source: lib/syntax/lists0.ml regular list branch. This v2 slice owns
// paragraph-safe list items: marker parsing, checkbox splitting, per-line
// OCaml-trim folding, two-eol nesting boundaries, and the flat-to-nested item
// fold are source-transcribed. Item content that would need a non-paragraph
// block-content reparse returns ownership failure before the list commits.
// scan-owner: (a) consumed regular list — the item cursor advances over each
// marker/continuation/absorbed blank line once; each item body byte is copied
// once into a local origin map before one inline remap pass.
fn regular_list_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
    context: BlockParseContext,
) -> ListDecision {
    regular_list_sequence_at(source, i, source.lines[i].start, format, context)
}

fn regular_list_sequence_at(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
    context: BlockParseContext,
) -> ListDecision {
    if regular_list_marker_at(&source.lines[i], start_abs, format).is_none() {
        if empty_regular_list_marker_line_at(source, i, start_abs, format) {
            return ListDecision::Paragraph;
        }
        return ListDecision::No;
    }

    let mut flat = Vec::new();
    let mut boundaries = Vec::new();
    let mut scopes: Vec<ListScope> = Vec::new();
    let mut last_item: Option<(u32, ListCollapseFloor)> = None;
    let mut after_two_eols = false;
    let mut cur = i;

    while cur < source.lines.len() {
        let line = &source.lines[cur];
        if line.text.is_empty() {
            break;
        }
        let marker_start = if cur == i {
            start_abs
        } else {
            source.lines[cur].start
        };
        let Some(marker) = regular_list_marker_at(line, marker_start, format) else {
            break;
        };

        let boundary_before = after_two_eols;
        after_two_eols = false;
        let cur_indent = marker.indent;
        while scopes.last().is_some_and(|scope| scope.indent > cur_indent) {
            scopes.pop();
        }
        if !scopes
            .last()
            .is_some_and(|scope| scope.indent == cur_indent)
        {
            let empty_floor = match last_item {
                Some((prev_indent, prev_floor)) if cur_indent > prev_indent => prev_floor,
                _ => ListCollapseFloor {
                    flat_len: flat.len(),
                    line_idx: cur,
                },
            };
            scopes.push(ListScope {
                indent: cur_indent,
                committed: 0,
                empty_floor,
            });
        }
        let scope_idx = scopes.len() - 1;
        let fail_floor = if scopes[scope_idx].committed == 0 {
            scopes[scope_idx].empty_floor
        } else {
            ListCollapseFloor {
                flat_len: flat.len(),
                line_idx: cur,
            }
        };

        let mut item_text = String::new();
        let mut item_map = OriginMap::new();
        append_list_content(
            source,
            cur,
            marker.body_start,
            &mut item_text,
            &mut item_map,
        );
        let mut last_content_line = cur;
        let mut next = cur + 1;

        loop {
            if next >= source.lines.len() {
                break;
            }
            let cont = &source.lines[next];
            if cont.text.is_empty() {
                next += 1;
                after_two_eols = true;
                break;
            }

            let (cont_indent, is_item, is_heading) =
                regular_list_continuation_shape(cont.text, format);
            if cont_indent == 0
                && !cont
                    .text
                    .as_bytes()
                    .first()
                    .is_some_and(|&b| mldoc_is_space(b))
            {
                break;
            }
            if is_heading {
                break;
            }
            if is_item {
                if cont_indent > cur_indent && regular_list_marker(cont, format).is_none() {
                    return regular_list_emit_floor(
                        source, start_abs, flat, boundaries, fail_floor,
                    );
                }
                break;
            }

            append_list_joiner(source, last_content_line, &mut item_text, &mut item_map);
            append_list_content(source, next, 0, &mut item_text, &mut item_map);
            last_content_line = next;
            next += 1;
        }

        let (name, content_text, content_map) = if format != "org" && !marker.ordered {
            match markdown_regular_list_definition_split(&item_text, &item_map, source.input) {
                Some((name, stripped, stripped_map)) => (name, stripped, stripped_map),
                None => (Vec::new(), item_text, item_map),
            }
        } else {
            (Vec::new(), item_text, item_map)
        };

        let mode = if context == BlockParseContext::BlockContent {
            ListContentMode::BlockContent
        } else {
            ListContentMode::Document
        };
        let Some(content) =
            list_item_block_content(content_text, content_map, format, source.input, mode)
        else {
            return ListDecision::Delegate;
        };

        flat.push(ListItem {
            ordered: marker.ordered,
            number: marker.number,
            indent: cur_indent,
            content,
            items: Vec::new(),
            name,
            checkbox: marker.checkbox,
        });
        boundaries.push(boundary_before);
        scopes[scope_idx].committed += 1;
        last_item = Some((cur_indent, fail_floor));
        cur = next;
    }

    if flat.is_empty() {
        return ListDecision::No;
    }

    regular_list_emit(source, start_abs, flat, boundaries, cur)
}

fn regular_list_emit_floor(
    source: &Source<'_>,
    start_abs: usize,
    mut flat: Vec<ListItem>,
    mut boundaries: Vec<bool>,
    floor: ListCollapseFloor,
) -> ListDecision {
    if floor.flat_len == 0 {
        return ListDecision::Paragraph;
    }
    flat.truncate(floor.flat_len);
    boundaries.truncate(floor.flat_len);
    regular_list_emit(source, start_abs, flat, boundaries, floor.line_idx)
}

fn regular_list_emit(
    source: &Source<'_>,
    start_abs: usize,
    flat: Vec<ListItem>,
    boundaries: Vec<bool>,
    next: usize,
) -> ListDecision {
    let span = Some(Span(start_abs, source.lines[next - 1].end));
    ListDecision::Emit {
        block: Block::List {
            items: crate::projection::nest_items_with_boundaries(flat, boundaries),
            span,
        },
        next,
    }
}

fn regular_list_marker(line: &Line<'_>, format: &str) -> Option<RegularListMarker> {
    if line.eol == Eol::Cr {
        return None;
    }
    regular_list_marker_at(line, line.start, format)
}

fn regular_list_marker_at(
    line: &Line<'_>,
    start_abs: usize,
    format: &str,
) -> Option<RegularListMarker> {
    if line.eol == Eol::Cr {
        return None;
    }
    let rel = start_abs.checked_sub(line.start)?;
    let text = line.text.get(rel..)?;
    let mut marker = regular_list_marker_text(text, format)?;
    marker.body_start += rel;
    Some(marker)
}

fn regular_list_marker_text(text: &str, format: &str) -> Option<RegularListMarker> {
    if format == "org" {
        org_regular_list_marker(text)
    } else {
        markdown_regular_list_marker(text)
    }
}

fn markdown_regular_list_marker(text: &str) -> Option<RegularListMarker> {
    let ws = mldoc_spaces_len(text);
    let rest = &text[ws..];
    let mk = |ordered, number, content: &str| {
        let (checkbox, body) = split_checkbox(content);
        if ocaml_trim(body).is_empty() {
            return None;
        }
        Some(RegularListMarker {
            ordered,
            number,
            checkbox,
            indent: ws as u32,
            body_start: text.len() - body.len(),
        })
    };

    if let Some(after) = rest.strip_prefix('*').or_else(|| rest.strip_prefix('+')) {
        if after.as_bytes().first().is_some_and(|&b| mldoc_is_space(b)) {
            return mk(false, None, mldoc_trim_spaces_start(after));
        }
    }

    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    crate::metrics::scan_work(digits + usize::from(digits < rest.len()));
    if digits > 0 {
        if let Some(after) = rest[digits..].strip_prefix('.') {
            if after.as_bytes().first().is_some_and(|&b| mldoc_is_space(b)) {
                if let Ok(number) = rest[..digits].parse::<u32>() {
                    return mk(true, Some(number), mldoc_trim_spaces_start(after));
                }
            }
        }
    }

    None
}

fn org_regular_list_marker(text: &str) -> Option<RegularListMarker> {
    let ws = mldoc_spaces_len(text);
    let rest = &text[ws..];
    let mk = |ordered, number, content: &str| {
        let (checkbox, body) = split_checkbox(content);
        if ocaml_trim(body).is_empty() {
            return None;
        }
        Some(RegularListMarker {
            ordered,
            number,
            checkbox,
            indent: ws as u32,
            body_start: text.len() - body.len(),
        })
    };

    let dash = if ws == 0 {
        rest.strip_prefix('-')
    } else {
        None
    };
    let star = if ws > 0 { rest.strip_prefix('*') } else { None };
    if let Some(after) = dash.or(star).or_else(|| rest.strip_prefix('+')) {
        if after.as_bytes().first().is_some_and(|&b| mldoc_is_space(b)) {
            return mk(false, None, mldoc_trim_spaces_start(after));
        }
    }

    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    crate::metrics::scan_work(digits + usize::from(digits < rest.len()));
    if digits > 0 {
        if let Some(after) = rest[digits..].strip_prefix('.') {
            if after.as_bytes().first().is_some_and(|&b| mldoc_is_space(b)) {
                if let Ok(number) = rest[..digits].parse::<u32>() {
                    return mk(true, Some(number), mldoc_trim_spaces_start(after));
                }
            }
        }
    }

    None
}

fn regular_list_continuation_shape(text: &str, format: &str) -> (u32, bool, bool) {
    if format == "org" {
        let (indent, is_item) = org_regular_list_continuation_shape(text);
        (indent, is_item, false)
    } else {
        markdown_regular_list_continuation_shape(text)
    }
}

fn markdown_regular_list_continuation_shape(text: &str) -> (u32, bool, bool) {
    let indent = leading_ws(text);
    if list_scan_leading_int(ocaml_trim(text)) {
        return (indent as u32, true, false);
    }
    let b = text.as_bytes();
    if b.len() >= indent + 2 {
        let p0 = b[indent];
        let p1 = b[indent + 1];
        let is_item = (p0 == b'+' && p1 == b' ') || (p0 == b'*' && p1 == b' ');
        let is_heading = p0 == b'-' && p1 == b' ';
        (indent as u32, is_item, is_heading)
    } else if b.len() >= indent + 1 {
        (indent as u32, false, b[indent] == b'-')
    } else {
        (indent as u32, false, false)
    }
}

fn org_regular_list_continuation_shape(text: &str) -> (u32, bool) {
    let indent = leading_ws(text);
    if list_scan_leading_int(ocaml_trim(text)) {
        return (indent as u32, true);
    }
    let b = text.as_bytes();
    if b.len() >= indent + 2 {
        let p0 = b[indent];
        let p1 = b[indent + 1];
        let is_item = (p0 == b'+' && p1 == b' ')
            || (p0 == b'-' && p1 == b' ')
            || (indent != 0 && p0 == b'*' && p1 == b' ');
        (indent as u32, is_item)
    } else {
        (indent as u32, false)
    }
}

fn list_scan_leading_int(text: &str) -> bool {
    let bytes = text.as_bytes();
    let i = if matches!(bytes.first(), Some(b'+' | b'-')) {
        1
    } else {
        0
    };
    crate::metrics::scan_work(i + usize::from(i < bytes.len()));
    bytes.get(i).is_some_and(u8::is_ascii_digit)
}

fn empty_regular_list_marker_line_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    regular_list_marker_text_allow_empty(text, format)
        .is_some_and(|body| ocaml_trim(body).is_empty())
}

fn regular_list_marker_text_allow_empty<'a>(text: &'a str, format: &str) -> Option<&'a str> {
    let ws = mldoc_spaces_len(text);
    let rest = &text[ws..];

    let unordered_body = |after: &'a str| {
        after
            .as_bytes()
            .first()
            .is_some_and(|&b| mldoc_is_space(b))
            .then(|| {
                let body = mldoc_trim_spaces_start(after);
                let (_checkbox, body) = split_checkbox(body);
                body
            })
    };

    if format == "org" {
        if let Some(after) = (ws == 0)
            .then(|| rest.strip_prefix('-'))
            .flatten()
            .or_else(|| (ws > 0).then(|| rest.strip_prefix('*')).flatten())
            .or_else(|| rest.strip_prefix('+'))
        {
            return unordered_body(after);
        }
    } else if let Some(after) = rest.strip_prefix('*').or_else(|| rest.strip_prefix('+')) {
        return unordered_body(after);
    }

    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    crate::metrics::scan_work(digits + usize::from(digits < rest.len()));
    if digits == 0 || rest[..digits].parse::<u32>().is_err() {
        return None;
    }
    let after = rest[digits..].strip_prefix('.')?;
    if !after.as_bytes().first().is_some_and(|&b| mldoc_is_space(b)) {
        return None;
    }
    let body = mldoc_trim_spaces_start(after);
    let (_checkbox, body) = split_checkbox(body);
    Some(body)
}

fn append_list_content(
    source: &Source<'_>,
    line_idx: usize,
    rel: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let (trimmed, abs_start) = ocaml_trim_with_start(&line.text[rel..], line.start + rel);
    if trimmed.is_empty() {
        return;
    }
    let text_off = body.len();
    map.push(text_off, abs_start, trimmed.len(), trimmed.len());
    body.push_str(trimmed);
    crate::metrics::scan_work(trimmed.len());
}

fn append_list_joiner(
    source: &Source<'_>,
    line_idx: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let text_off = body.len();
    let eol_start = line.start + line.text.len();
    let eol_len = line.end.saturating_sub(eol_start);
    if eol_len > 0 {
        map.push(text_off, eol_start, 1, eol_len);
    }
    crate::metrics::scan_work(1);
    body.push('\n');
}

fn markdown_regular_list_definition_split(
    content: &str,
    map: &OriginMap,
    source_input: &str,
) -> Option<(Vec<Inline>, String, OriginMap)> {
    let found = content.find(" ::");
    crate::metrics::scan_work(found.map_or(content.len(), |pos| pos + 3));
    let pos = found?;
    if pos == 0 || pos + 3 != content.len() {
        return None;
    }
    let mut name = super::inline(&content[..pos], "md");
    let mut cursor = OriginCursor::new();
    crate::source_map::remap_inlines(&mut name, content, source_input, map, &mut cursor);
    Some((name, String::new(), OriginMap::new()))
}

fn list_item_block_content(
    content: String,
    map: OriginMap,
    format: &str,
    source_input: &str,
    mode: ListContentMode,
) -> Option<Vec<Block>> {
    if content.is_empty() {
        return Some(vec![Block::Paragraph {
            inline: Vec::new(),
            span: None,
        }]);
    }

    let mut blocks =
        try_parse_leaf_blocks_in(&content, format, BlockParseContext::ListContent(mode))?;
    rewrite_list_item_suppressed_blocks(&mut blocks, &content, format, mode)?;
    merge_adjacent_paragraph_blocks(&mut blocks);
    if format == "org" {
        merge_adjacent_org_fixed_width_examples(&mut blocks, &content);
    }
    if !list_item_blocks_are_safe_content(&blocks, mode) {
        return None;
    }
    trim_paragraph_breaks_before_blocks(&mut blocks, format);
    remap_block_inlines_from_origin_preserve_spans(&mut blocks, &content, source_input, &map);
    Some(blocks)
}

fn list_item_blocks_are_safe_content(blocks: &[Block], mode: ListContentMode) -> bool {
    for block in blocks {
        match block {
            Block::Heading { .. }
            | Block::Bullet { .. }
            | Block::List { .. }
            | Block::Drawer { .. }
            | Block::Properties { .. }
            | Block::FootnoteDef { .. } => return false,
            Block::Directive { .. } if mode == ListContentMode::Document => return false,
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                if !callout_children_are_safe_block_content(children) {
                    return false;
                }
            }
            Block::Paragraph { .. }
            | Block::Src { .. }
            | Block::Export { .. }
            | Block::CommentBlock { .. }
            | Block::RawHtml { .. }
            | Block::DisplayedMath { .. }
            | Block::Comment { .. }
            | Block::Directive { .. }
            | Block::Example { .. }
            | Block::LatexEnv { .. }
            | Block::Hr { .. }
            | Block::Hiccup { .. }
            | Block::Results { .. }
            | Block::Table { .. } => {}
        }
    }
    true
}

#[derive(Clone, Copy)]
enum SuppressedContentContext {
    BlockContent,
    ListContent(ListContentMode),
}

fn rewrite_list_item_suppressed_blocks(
    blocks: &mut Vec<Block>,
    body: &str,
    format: &str,
    mode: ListContentMode,
) -> Option<()> {
    let mut rewritten = Vec::with_capacity(blocks.len());
    for mut block in std::mem::take(blocks) {
        match &mut block {
            Block::Heading { span, .. } | Block::Bullet { span, .. } => {
                rewritten.push(paragraph_from_body_span(body, format, *span)?);
            }
            Block::FootnoteDef { span, .. } => {
                rewritten.extend(suppressed_footnote_blocks(
                    body,
                    *span,
                    format,
                    SuppressedContentContext::ListContent(mode),
                )?);
            }
            Block::Properties { props, span } => {
                if props.iter().all(Property::is_parse2) {
                    let block = paragraph_from_body_span(body, format, *span)?;
                    rewritten.push(block);
                } else if format == "org" {
                    rewritten.extend(org_drawer_content_blocks(
                        body,
                        *span,
                        SuppressedContentContext::ListContent(mode),
                    )?);
                } else {
                    rewritten.extend(markdown_suppressed_property_span_blocks(
                        body,
                        *span,
                        SuppressedContentContext::ListContent(mode),
                    )?);
                }
            }
            Block::Directive { span, .. } => {
                if mode == ListContentMode::Document {
                    rewritten.push(paragraph_from_body_span(body, format, *span)?);
                } else {
                    rewritten.push(block);
                }
            }
            Block::Drawer { span, .. } => {
                if format == "org" {
                    rewritten.extend(org_drawer_content_blocks(
                        body,
                        *span,
                        SuppressedContentContext::ListContent(mode),
                    )?);
                } else {
                    rewritten.extend(markdown_suppressed_property_span_blocks(
                        body,
                        *span,
                        SuppressedContentContext::ListContent(mode),
                    )?);
                }
            }
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                rewrite_callout_suppressed_blocks(children, body, format)?;
                rewritten.push(block);
            }
            Block::List { items, .. } => {
                for item in items {
                    rewrite_list_item_suppressed_blocks(&mut item.content, body, format, mode)?;
                    rewrite_list_item_suppressed_list_items(&mut item.items, body, format, mode)?;
                }
                rewritten.push(block);
            }
            _ => rewritten.push(block),
        }
    }
    *blocks = rewritten;
    Some(())
}

fn org_drawer_content_blocks(
    body: &str,
    span: Option<Span>,
    context: SuppressedContentContext,
) -> Option<Vec<Block>> {
    let Span(start, end) = span?;
    if start > end || end > body.len() {
        return None;
    }
    let source = Source::scan(&body[start..end]);
    let mut blocks = Vec::new();
    let mut paragraph_start = None;
    let mut i = 0usize;
    while i < source.lines.len() {
        if let Some((mut block, next)) = org_verbatim_sequence(&source, i, "org") {
            if let Some(chunk_start) = paragraph_start.take() {
                org_drawer_non_verbatim_blocks(
                    &mut blocks,
                    body,
                    chunk_start,
                    start + source.lines[i].start,
                    context,
                )?;
            }
            offset_block(&mut block, start);
            blocks.push(block);
            i = next;
        } else {
            paragraph_start.get_or_insert(start + source.lines[i].start);
            i += 1;
        }
    }
    if let Some(chunk_start) = paragraph_start {
        org_drawer_non_verbatim_blocks(&mut blocks, body, chunk_start, end, context)?;
    }
    Some(blocks)
}

fn org_drawer_non_verbatim_blocks(
    blocks: &mut Vec<Block>,
    body: &str,
    start: usize,
    end: usize,
    context: SuppressedContentContext,
) -> Option<()> {
    if start >= end {
        return Some(());
    }
    let parse_context = match context {
        SuppressedContentContext::BlockContent => BlockParseContext::BlockContent,
        SuppressedContentContext::ListContent(mode) => BlockParseContext::ListContent(mode),
    };
    let mut chunk = try_parse_leaf_blocks_in(&body[start..end], "org", parse_context)?;
    offset_blocks(&mut chunk, start);
    match context {
        SuppressedContentContext::BlockContent => {
            rewrite_callout_suppressed_blocks(&mut chunk, body, "org")?;
        }
        SuppressedContentContext::ListContent(mode) => {
            rewrite_list_item_suppressed_blocks(&mut chunk, body, "org", mode)?;
        }
    }
    blocks.extend(chunk);
    Some(())
}

fn rewrite_list_item_suppressed_list_items(
    items: &mut [ListItem],
    body: &str,
    format: &str,
    mode: ListContentMode,
) -> Option<()> {
    for item in items {
        rewrite_list_item_suppressed_blocks(&mut item.content, body, format, mode)?;
        rewrite_list_item_suppressed_list_items(&mut item.items, body, format, mode)?;
    }
    Some(())
}

fn remap_block_inlines_from_origin_preserve_spans(
    blocks: &mut [Block],
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
) {
    let mut cursor = OriginCursor::new();
    for block in blocks {
        remap_block_inlines_preserve_spans(block, current_input, source_input, origin, &mut cursor);
    }
}

fn remap_block_inlines_preserve_spans(
    block: &mut Block,
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) {
    match block {
        Block::Paragraph { inline, .. }
        | Block::Heading { inline, .. }
        | Block::Bullet { inline, .. }
        | Block::FootnoteDef { inline, .. } => {
            crate::source_map::remap_inlines(inline, current_input, source_input, origin, cursor);
        }
        Block::Table { header, rows, .. } => {
            if let Some(header) = header {
                for cell in header {
                    crate::source_map::remap_inlines(
                        cell,
                        current_input,
                        source_input,
                        origin,
                        cursor,
                    );
                }
            }
            for row in rows {
                for cell in row {
                    crate::source_map::remap_inlines(
                        cell,
                        current_input,
                        source_input,
                        origin,
                        cursor,
                    );
                }
            }
        }
        Block::Quote { children, .. } | Block::Custom { children, .. } => {
            for child in children {
                remap_block_inlines_preserve_spans(
                    child,
                    current_input,
                    source_input,
                    origin,
                    cursor,
                );
            }
        }
        Block::List { items, .. } => {
            remap_list_item_inlines_preserve_spans(
                items,
                current_input,
                source_input,
                origin,
                cursor,
            );
        }
        Block::Src { .. }
        | Block::Export { .. }
        | Block::CommentBlock { .. }
        | Block::RawHtml { .. }
        | Block::DisplayedMath { .. }
        | Block::Drawer { .. }
        | Block::Directive { .. }
        | Block::Comment { .. }
        | Block::Example { .. }
        | Block::LatexEnv { .. }
        | Block::Properties { .. }
        | Block::Hr { .. }
        | Block::Hiccup { .. }
        | Block::Results { .. } => {}
    }
}

fn remap_list_item_inlines_preserve_spans(
    items: &mut [ListItem],
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) {
    for item in items {
        crate::source_map::remap_inlines(
            &mut item.name,
            current_input,
            source_input,
            origin,
            cursor,
        );
        for block in &mut item.content {
            remap_block_inlines_preserve_spans(block, current_input, source_input, origin, cursor);
        }
        remap_list_item_inlines_preserve_spans(
            &mut item.items,
            current_input,
            source_input,
            origin,
            cursor,
        );
    }
}

fn markdown_definition_opener(line: &Line<'_>) -> Option<(usize, usize)> {
    if line.eol == Eol::Cr {
        return None;
    }
    let rel = mldoc_spaces_len(line.text);
    let rest = line.text[rel..].strip_prefix(':')?;
    let ws = mldoc_spaces_len(rest);
    if ws == 0 {
        return None;
    }
    markdown_definition_body_at(line.text, rel + 1 + ws)
}

fn markdown_definition_continuation(line: &Line<'_>) -> Option<(usize, usize)> {
    if line.eol == Eol::Cr {
        return None;
    }
    let rel = mldoc_spaces_len(line.text);
    markdown_definition_body_at(line.text, rel)
}

fn markdown_definition_body_at(text: &str, rel: usize) -> Option<(usize, usize)> {
    let rest = text[rel..].as_bytes();
    if rest.len() < 2 || matches!(rest[0], b':' | b'#' | b'\r' | b'\n') {
        return None;
    }
    crate::metrics::scan_work(rest.len());
    Some((rel, rest.len()))
}

fn append_definition_content(
    source: &Source<'_>,
    line_idx: usize,
    rel: usize,
    len: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let text_off = body.len();
    let src_off = line.start + rel;
    map.push(text_off, src_off, len, len);
    body.push_str(&line.text[rel..rel + len]);
    crate::metrics::scan_work(len);
}

fn append_definition_joiner(
    source: &Source<'_>,
    line_idx: usize,
    body: &mut String,
    map: &mut OriginMap,
) {
    let line = &source.lines[line_idx];
    let text_off = body.len();
    let eol_start = line.start + line.text.len();
    let eol_len = line.end.saturating_sub(eol_start);
    if eol_len > 0 {
        map.push(text_off, eol_start, 1, eol_len);
    }
    crate::metrics::scan_work(1);
    body.push('\n');
}

fn markdown_definition_paragraph(
    item_text: String,
    item_map: OriginMap,
    source_input: &str,
) -> Block {
    let (trimmed, trim_start) = ocaml_trim_with_start(&item_text, 0);
    let mut inline = super::inline_at(trimmed, "md", trim_start);
    let mut cursor = OriginCursor::new();
    crate::source_map::remap_inlines(
        &mut inline,
        &item_text,
        source_input,
        &item_map,
        &mut cursor,
    );
    Block::Paragraph { inline, span: None }
}

enum LatexEnvDecision {
    Emit {
        block: Block,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    Paragraph,
    No,
}

struct LatexEnvCapture {
    block: Block,
    next: usize,
    tail_start: Option<(usize, usize)>,
}

// mldoc source: lib/syntax/latex_env.ml. Top-level parser order places
// `Latex_env.parse` before generic `Block.parse`; the opener is `spaces *>
// string_ci "\\begin{" *> take_while1 (fun c -> c <> '}') <* '}' <*
// spaces_or_eols`, and content runs to the first case-insensitive `\end{name}`
// or EOF.
// scan-owner: (a) consumed-on-match accepted environment — opener/name scan,
// KMP closer scan, line cursor advance, and content copy are each over disjoint
// accepted spans; malformed openers decline after one bounded suffix scan.
fn latex_env_sequence(source: &Source<'_>, i: usize) -> LatexEnvDecision {
    let line = &source.lines[i];
    if !latex_env_opener_at(source, i, line.start) {
        return LatexEnvDecision::No;
    }
    let Ok(capture) = latex_env_capture(source, i, line.start) else {
        return LatexEnvDecision::Paragraph;
    };
    LatexEnvDecision::Emit {
        block: capture.block,
        next: capture.next,
        tail_start: capture.tail_start,
    }
}

fn latex_env_opener_at(source: &Source<'_>, line_idx: usize, start_abs: usize) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    if start_abs > content_end {
        return false;
    }
    let text = &source.input[start_abs..content_end];
    let ws = mldoc_spaces_len(text);
    text[ws..].starts_with("\\begin{")
}

fn latex_env_capture(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
) -> Result<LatexEnvCapture, ()> {
    let input = source.input;
    let bytes = input.as_bytes();
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let text = source.input.get(start_abs..content_end).ok_or(())?;
    let ws = mldoc_spaces_len(text);
    if !text[ws..].starts_with("\\begin{") {
        return Err(());
    }

    let name_start = start_abs + ws + 7;
    let mut name_end = name_start;
    while name_end < input.len() && bytes[name_end] != b'}' {
        crate::metrics::scan_work(1);
        name_end += 1;
    }
    crate::metrics::scan_work(usize::from(name_end < input.len()));
    if name_end == input.len() || name_end == name_start {
        return Err(());
    }

    let name = &input[name_start..name_end];
    let mut content_start = name_end + 1;
    while content_start < input.len()
        && (mldoc_is_space(bytes[content_start]) || matches!(bytes[content_start], b'\n' | b'\r'))
    {
        crate::metrics::scan_work(1);
        content_start += 1;
    }
    crate::metrics::scan_work(usize::from(content_start < input.len()));

    let ending = latex_env_ending_pattern(name);
    let (content_end, consumed_end) = match find_latex_env_close_ci(input, content_start, &ending) {
        Some(close) => (close, close + ending.len()),
        None => (input.len(), input.len()),
    };
    let content = &input[content_start..content_end];
    crate::metrics::scan_work(content.len());

    let next = first_line_start_at_or_after(&source.lines, line_idx, consumed_end);
    let trail_end = source
        .lines
        .get(next)
        .map_or(input.len(), |line| line.start);
    let tail_start = (consumed_end < trail_end).then_some((next - 1, consumed_end));
    crate::metrics::scan_work(name.len());

    Ok(LatexEnvCapture {
        block: Block::LatexEnv {
            name: name.to_ascii_lowercase(),
            content: content.to_string(),
            span: Some(Span(start_abs, consumed_end)),
        },
        next,
        tail_start,
    })
}

fn latex_env_ending_pattern(name: &str) -> Vec<u8> {
    let mut ending = Vec::with_capacity(name.len() + 6);
    ending.extend_from_slice(b"\\end{");
    for &b in name.as_bytes() {
        crate::metrics::scan_work(1);
        ending.push(ascii_lower_byte(b));
    }
    ending.push(b'}');
    crate::metrics::scan_work(6);
    ending
}

fn find_latex_env_close_ci(input: &str, from: usize, pattern: &[u8]) -> Option<usize> {
    let prefix = kmp_prefix(pattern);
    let mut matched = 0usize;
    let bytes = input.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        crate::metrics::scan_work(1);
        let b = ascii_lower_byte(bytes[i]);
        while matched > 0 && b != pattern[matched] {
            crate::metrics::scan_work(1);
            matched = prefix[matched - 1];
        }
        if b == pattern[matched] {
            matched += 1;
            if matched == pattern.len() {
                return Some(i + 1 - pattern.len());
            }
        }
        i += 1;
    }
    None
}

fn kmp_prefix(pattern: &[u8]) -> Vec<usize> {
    let mut prefix = vec![0; pattern.len()];
    let mut matched = 0usize;
    let mut i = 1usize;
    while i < pattern.len() {
        crate::metrics::scan_work(1);
        while matched > 0 && pattern[i] != pattern[matched] {
            crate::metrics::scan_work(1);
            matched = prefix[matched - 1];
        }
        if pattern[i] == pattern[matched] {
            matched += 1;
            prefix[i] = matched;
        }
        i += 1;
    }
    prefix
}

#[inline]
fn ascii_lower_byte(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

fn first_line_start_at_or_after(lines: &[Line<'_>], from: usize, pos: usize) -> usize {
    let mut i = from + 1;
    while i < lines.len() && lines[i].start < pos {
        crate::metrics::scan_work(1);
        i += 1;
    }
    i
}

enum DisplayedMathDecision {
    Emit {
        blocks: Vec<Block>,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    Delegate,
    Paragraph,
    No,
}

// mldoc source: lib/syntax/block0.ml `displayed_math`, inside `between_eols`.
// `end_string "$$"` captures until the first following `$$`. This v2 slice owns
// raw top-level captures and same-line tails that are themselves v2-owned block
// suffixes.
// scan-owner: (a) consumed-on-match accepted capture — every emitted math span
// advances to a disjoint close; unowned candidates return `None` before commit
// and unclosed candidates decline immediately.
fn displayed_math_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> DisplayedMathDecision {
    let mut line_idx = i;
    let mut start_abs = source.lines[i].start;
    let mut text = source.lines[i].text;
    if displayed_math_opener(text).is_none() {
        return DisplayedMathDecision::No;
    }

    let mut blocks = Vec::new();
    loop {
        let Some(opener_off) = displayed_math_opener(text) else {
            return displayed_math_tail(
                source,
                line_idx,
                start_abs,
                text,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            );
        };
        let opener = start_abs + opener_off;
        let Some(close) = find_displayed_math_close(source.input, opener, source.input.len())
        else {
            if blocks.is_empty() {
                return DisplayedMathDecision::Paragraph;
            }
            return DisplayedMathDecision::Emit {
                blocks,
                next: line_idx + 1,
                tail_start: Some((line_idx, opener)),
            };
        };
        let close_end = close + 2;
        let Some(close_line) = line_containing_text_pos(&source.lines, line_idx, close_end) else {
            return DisplayedMathDecision::Delegate;
        };
        let content_end = line_text_end(&source.lines[close_line]);
        let math_text = &source.input[opener + 2..close];
        crate::metrics::scan_work(math_text.len());

        if close_end < content_end {
            blocks.push(Block::DisplayedMath {
                text: math_text.to_string(),
                span: Some(Span(start_abs, close_end)),
            });
            line_idx = close_line;
            start_abs = close_end;
            text = &source.input[start_abs..content_end];
            continue;
        }

        let mut next = close_line + 1;
        let mut span_end = source.lines[close_line].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        blocks.push(Block::DisplayedMath {
            text: math_text.to_string(),
            span: Some(Span(start_abs, span_end)),
        });
        return DisplayedMathDecision::Emit {
            blocks,
            next,
            tail_start: None,
        };
    }
}

fn displayed_math_tail(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    text: &str,
    mut blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> DisplayedMathDecision {
    if text.is_empty() {
        return DisplayedMathDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if hr_accepts_eol(source.lines[line_idx].eol) && is_hr_line(text, format) {
        blocks.push(Block::Hr {
            span: Some(Span(start_abs, source.lines[line_idx].end)),
        });
        return DisplayedMathDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if let Some(split) = bounded_split_suffix_blocks(
        source,
        line_idx,
        start_abs,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    ) {
        blocks.extend(split.blocks);
        return DisplayedMathDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if let Some(split) = callout_container_split_at(source, line_idx, start_abs, format) {
        blocks.extend(split.blocks);
        return DisplayedMathDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if invalid_raw_html_tail_at(source, line_idx, start_abs, raw_html_scan)
        || rejected_fence_tail_at(source, line_idx, start_abs, fence_cursor)
        || rejected_table_tail_at(source, line_idx, start_abs, format)
        || rejected_regular_list_tail_at(source, line_idx, start_abs, format)
        || rejected_footnote_tail_at(source, line_idx, start_abs, format)
        || unclosed_markdown_drawer_tail_at(source, line_idx, start_abs, format)
        || rejected_markdown_property_tail_at(source, line_idx, start_abs, format)
        || rejected_directive_property_tail_at(source, line_idx, start_abs)
        || rejected_blockquote_tail_at(source, line_idx, start_abs, format)
        || malformed_latex_tail_at(source, line_idx, start_abs)
        || rejected_begin_tail_at(source, line_idx, start_abs)
    {
        return DisplayedMathDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: Some((line_idx, start_abs)),
        };
    }
    if directive_line(text).is_some()
        || comment_line(text, format).is_some()
        || heading_start(text, format)
        || property_or_drawer_start(text, format)
        || footnote_definition_start(text, source.lines[line_idx].eol, format)
        || raw_html_tail_start(text)
        || could_start_non_paragraph(text, format)
    {
        return DisplayedMathDecision::Delegate;
    }
    DisplayedMathDecision::Emit {
        blocks,
        next: line_idx + 1,
        tail_start: Some((line_idx, start_abs)),
    }
}

enum RawHtmlDecision {
    Emit {
        blocks: Vec<Block>,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    Delegate,
    No,
}

// mldoc source: lib/syntax/block0.ml dispatches `Raw_html.parse` for `<` inside
// `between_eols`; lib/syntax/raw_html.ml handles special wrappers and known HTML
// tags. The lower grammar is implemented by `block_common`'s source-audited
// cached raw-HTML matcher; this v2 owner supplies the top-level block order,
// span/tail handling, and fallthrough behavior.
// scan-owner: (a) consumed raw-HTML sequence — `RawHtmlScan` owns closer misses
// and per-tag indexes; this loop copies only accepted captures and advances to
// disjoint same-line tails or later source lines.
fn raw_html_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    scan: &mut RawHtmlScan,
) -> RawHtmlDecision {
    let mut blocks = Vec::new();
    let mut line_idx = i;
    let mut start_abs = source.lines[i].start;
    let mut same_line_tail = false;

    loop {
        if line_idx >= source.lines.len() {
            return RawHtmlDecision::Emit {
                blocks,
                next: line_idx,
                tail_start: None,
            };
        }
        let line = &source.lines[line_idx];
        let content_end = line_text_end(line);
        if start_abs > content_end {
            return if blocks.is_empty() {
                RawHtmlDecision::No
            } else {
                RawHtmlDecision::Emit {
                    blocks,
                    next: line_idx,
                    tail_start: None,
                }
            };
        }
        let ws = mldoc_spaces_len(&source.input[start_abs..content_end]);
        let opener = start_abs + ws;
        if !source.input[opener..].starts_with('<') {
            return raw_html_non_match(
                source,
                line_idx,
                start_abs,
                same_line_tail,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                scan,
            );
        }

        let Some(extent) =
            parse_raw_html_at_cached(source.input, opener, source.input.len(), Some(scan))
        else {
            return raw_html_non_match(
                source,
                line_idx,
                start_abs,
                same_line_tail,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                scan,
            );
        };
        let close_end = extent.end;
        let Some(close_line) = line_containing_text_pos(&source.lines, line_idx, close_end) else {
            return RawHtmlDecision::Delegate;
        };
        let close_content_end = line_text_end(&source.lines[close_line]);
        let text = raw_html_capture_text(source.input, opener, close_end);

        if close_end < close_content_end {
            blocks.push(Block::RawHtml {
                text,
                span: Some(Span(start_abs, close_end)),
            });
            line_idx = close_line;
            start_abs = close_end;
            same_line_tail = true;
            continue;
        }

        let mut next = close_line + 1;
        let mut span_end = source.lines[close_line].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        blocks.push(Block::RawHtml {
            text,
            span: Some(Span(start_abs, span_end)),
        });

        if next >= source.lines.len() {
            return RawHtmlDecision::Emit {
                blocks,
                next,
                tail_start: None,
            };
        }
        line_idx = next;
        start_abs = source.lines[next].start;
        same_line_tail = false;
    }
}

fn raw_html_non_match(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    same_line_tail: bool,
    blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> RawHtmlDecision {
    if blocks.is_empty() {
        return RawHtmlDecision::No;
    }
    if same_line_tail {
        raw_html_tail(
            source,
            line_idx,
            start_abs,
            blocks,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        )
    } else {
        RawHtmlDecision::Emit {
            blocks,
            next: line_idx,
            tail_start: None,
        }
    }
}

fn raw_html_tail(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    mut blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> RawHtmlDecision {
    let line = &source.lines[line_idx];
    let text = &source.input[start_abs..line_text_end(line)];
    if text.is_empty() {
        return RawHtmlDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if let Some(split) = bounded_split_suffix_blocks(
        source,
        line_idx,
        start_abs,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    ) {
        blocks.extend(split.blocks);
        return RawHtmlDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if let Some(split) = callout_container_split_at(source, line_idx, start_abs, format) {
        blocks.extend(split.blocks);
        return RawHtmlDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if hr_accepts_eol(line.eol) && is_hr_line(text, format) {
        blocks.push(Block::Hr {
            span: Some(Span(start_abs, line.end)),
        });
        return RawHtmlDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if invalid_raw_html_tail_at(source, line_idx, start_abs, raw_html_scan)
        || unclosed_displayed_math_tail_at(source, line_idx, start_abs)
        || rejected_fence_tail_at(source, line_idx, start_abs, fence_cursor)
        || rejected_table_tail_at(source, line_idx, start_abs, format)
        || rejected_regular_list_tail_at(source, line_idx, start_abs, format)
        || rejected_footnote_tail_at(source, line_idx, start_abs, format)
        || unclosed_markdown_drawer_tail_at(source, line_idx, start_abs, format)
        || rejected_markdown_property_tail_at(source, line_idx, start_abs, format)
        || rejected_directive_property_tail_at(source, line_idx, start_abs)
        || rejected_blockquote_tail_at(source, line_idx, start_abs, format)
        || malformed_latex_tail_at(source, line_idx, start_abs)
        || rejected_begin_tail_at(source, line_idx, start_abs)
    {
        return RawHtmlDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: Some((line_idx, start_abs)),
        };
    }
    if directive_line(text).is_some()
        || comment_line(text, format).is_some()
        || heading_start(text, format)
        || property_or_drawer_start(text, format)
        || footnote_definition_start(text, source.lines[line_idx].eol, format)
        || raw_html_tail_start(text)
        || could_start_non_paragraph(text, format)
    {
        return RawHtmlDecision::Delegate;
    }
    RawHtmlDecision::Emit {
        blocks,
        next: line_idx + 1,
        tail_start: Some((line_idx, start_abs)),
    }
}

enum HiccupDecision {
    Emit {
        blocks: Vec<Block>,
        next: usize,
        tail_start: Option<(usize, usize)>,
    },
    Delegate,
    No,
}

// mldoc source: lib/syntax/block0.ml dispatches `Hiccup.parse` for `[` inside
// `between_eols`; lib/syntax/extended/hiccup.ml requires `[:`, a known raw-HTML
// tag, and `match_tag tag "[:" "]"`. The matcher counts nested `[:` even inside
// strings and ignores `]` only while the quote count is odd, which is preserved by
// the v2 source-pass close table.
// scan-owner: (a) consumed block hiccup sequence — the source pass precomputes
// matching close bytes once; this loop copies each accepted `v` once and advances
// to disjoint same-line tails or later source lines.
fn hiccup_sequence(
    source: &Source<'_>,
    i: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> HiccupDecision {
    hiccup_sequence_from(
        source,
        i,
        source.lines[i].start,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    )
}

fn hiccup_sequence_from(
    source: &Source<'_>,
    i: usize,
    start_abs: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> HiccupDecision {
    let mut blocks = Vec::new();
    let mut line_idx = i;
    let mut start_abs = start_abs;
    let mut same_line_tail = false;

    loop {
        if line_idx >= source.lines.len() {
            return HiccupDecision::Emit {
                blocks,
                next: line_idx,
                tail_start: None,
            };
        }
        let line = &source.lines[line_idx];
        let content_end = line_text_end(line);
        if start_abs > content_end {
            return if blocks.is_empty() {
                HiccupDecision::No
            } else {
                HiccupDecision::Emit {
                    blocks,
                    next: line_idx,
                    tail_start: None,
                }
            };
        }
        let ws = mldoc_spaces_len(&source.input[start_abs..content_end]);
        let opener = start_abs + ws;
        let starts_hiccup = source.input[opener..].starts_with("[:");
        if !starts_hiccup || !crate::inline::hiccup_head_ok(source.input, opener) {
            return hiccup_non_match(
                source,
                line_idx,
                start_abs,
                same_line_tail,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            );
        }
        let Some(close_end) = source.events.hiccup_close.at(opener) else {
            return hiccup_non_match(
                source,
                line_idx,
                start_abs,
                same_line_tail,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            );
        };

        let Some(close_line) = line_containing_text_pos(&source.lines, line_idx, close_end) else {
            return HiccupDecision::Delegate;
        };
        let close_content_end = line_text_end(&source.lines[close_line]);
        crate::metrics::scan_work(close_end - opener);

        if close_end < close_content_end {
            blocks.push(Block::Hiccup {
                v: source.input[opener..close_end].to_string(),
                span: Some(Span(start_abs, close_end)),
            });
            line_idx = close_line;
            start_abs = close_end;
            same_line_tail = true;
            continue;
        }

        let mut next = close_line + 1;
        let mut span_end = source.lines[close_line].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        blocks.push(Block::Hiccup {
            v: source.input[opener..close_end].to_string(),
            span: Some(Span(start_abs, span_end)),
        });

        if next >= source.lines.len() {
            return HiccupDecision::Emit {
                blocks,
                next,
                tail_start: None,
            };
        }
        line_idx = next;
        start_abs = source.lines[next].start;
        same_line_tail = false;
    }
}

fn hiccup_non_match(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    same_line_tail: bool,
    blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> HiccupDecision {
    if blocks.is_empty() {
        return HiccupDecision::No;
    }
    if same_line_tail {
        hiccup_tail(
            source,
            line_idx,
            start_abs,
            blocks,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        )
    } else {
        HiccupDecision::Emit {
            blocks,
            next: line_idx,
            tail_start: None,
        }
    }
}

fn hiccup_tail(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    mut blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> HiccupDecision {
    let line = &source.lines[line_idx];
    let text = &source.input[start_abs..line_text_end(line)];
    if text.is_empty() {
        return HiccupDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if let Some(split) = bounded_split_suffix_blocks(
        source,
        line_idx,
        start_abs,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    ) {
        blocks.extend(split.blocks);
        return HiccupDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if let Some(split) = callout_container_split_at(source, line_idx, start_abs, format) {
        blocks.extend(split.blocks);
        return HiccupDecision::Emit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        };
    }
    if hr_accepts_eol(line.eol) && is_hr_line(text, format) {
        blocks.push(Block::Hr {
            span: Some(Span(start_abs, line.end)),
        });
        return HiccupDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        };
    }
    if invalid_raw_html_tail_at(source, line_idx, start_abs, raw_html_scan)
        || unclosed_displayed_math_tail_at(source, line_idx, start_abs)
        || rejected_fence_tail_at(source, line_idx, start_abs, fence_cursor)
        || rejected_table_tail_at(source, line_idx, start_abs, format)
        || rejected_regular_list_tail_at(source, line_idx, start_abs, format)
        || rejected_footnote_tail_at(source, line_idx, start_abs, format)
        || unclosed_markdown_drawer_tail_at(source, line_idx, start_abs, format)
        || rejected_markdown_property_tail_at(source, line_idx, start_abs, format)
        || rejected_directive_property_tail_at(source, line_idx, start_abs)
        || rejected_blockquote_tail_at(source, line_idx, start_abs, format)
        || malformed_latex_tail_at(source, line_idx, start_abs)
        || rejected_begin_tail_at(source, line_idx, start_abs)
    {
        return HiccupDecision::Emit {
            blocks,
            next: line_idx + 1,
            tail_start: Some((line_idx, start_abs)),
        };
    }
    if directive_line(text).is_some()
        || comment_line(text, format).is_some()
        || heading_start(text, format)
        || property_or_drawer_start(text, format)
        || footnote_definition_start(text, source.lines[line_idx].eol, format)
        || raw_html_tail_start(text)
        || could_start_non_paragraph(text, format)
    {
        return HiccupDecision::Delegate;
    }
    HiccupDecision::Emit {
        blocks,
        next: line_idx + 1,
        tail_start: Some((line_idx, start_abs)),
    }
}

fn line_containing_text_pos(lines: &[Line<'_>], from: usize, pos: usize) -> Option<usize> {
    let mut i = from;
    while i < lines.len() && line_text_end(&lines[i]) < pos {
        crate::metrics::scan_work(1);
        i += 1;
    }
    (i < lines.len() && pos <= line_text_end(&lines[i])).then_some(i)
}

enum HeadingDecision<'a> {
    Emit(HeadingEmit<'a>),
    Split {
        heading: HeadingEmit<'a>,
        title_start: usize,
    },
    No,
}

enum HeadingKind {
    Heading,
    Bullet,
}

struct HeadingEmit<'a> {
    kind: HeadingKind,
    level: u32,
    size: Option<u32>,
    marker: Option<String>,
    priority: Option<String>,
    title: &'a str,
    title_start: usize,
    span_start: usize,
    span_end: usize,
    tail_start: Option<usize>,
}

impl HeadingEmit<'_> {
    fn block(&self, format: &str) -> Block {
        let mut inline = super::inline_at(self.title, format, self.title_start);
        let htags = if format == "org" {
            extract_org_htags(&mut inline)
        } else {
            Vec::new()
        };
        let span = Some(Span(self.span_start, self.span_end));
        match self.kind {
            HeadingKind::Heading => Block::Heading {
                level: self.level,
                size: self.size,
                inline,
                marker: self.marker.clone(),
                priority: self.priority.clone(),
                htags,
                span,
            },
            HeadingKind::Bullet => Block::Bullet {
                level: self.level,
                size: self.size,
                inline,
                marker: self.marker.clone(),
                priority: self.priority.clone(),
                htags,
                span,
            },
        }
    }
}

// mldoc source: lib/syntax/heading0.ml. This slice owns headings whose title
// lookahead resolves to paragraph text, plus a narrow source-transcribed subset
// of split title suffixes handled by `bounded_split_suffix_blocks`.
// scan-owner: (a2) caller-owned line helper — current-line marker/title scan.
fn heading_line<'a>(line: &Line<'a>, format: &str) -> HeadingDecision<'a> {
    if format == "org" {
        org_headline(line)
    } else {
        markdown_heading_or_bullet(line)
    }
}

fn heading_start(text: &str, format: &str) -> bool {
    if format == "org" {
        org_headline_level(text).is_some()
    } else {
        markdown_heading_at(text).is_some() || markdown_dash_bullet_level(text).is_some()
    }
}

fn markdown_heading_or_bullet<'a>(line: &Line<'a>) -> HeadingDecision<'a> {
    if let Some((level, size, marker_end)) = markdown_heading_at(line.text) {
        return heading_emit(
            line,
            HeadingKind::Heading,
            level,
            Some(size),
            marker_end,
            "md",
        );
    }
    if let Some(level) = markdown_dash_bullet_level(line.text) {
        let ws = mldoc_spaces_len(line.text);
        let after_dash = ws + 1;
        let (size, content_start) = dash_bullet_size_start(line.text, after_dash);
        return heading_emit(line, HeadingKind::Bullet, level, size, content_start, "md");
    }
    HeadingDecision::No
}

fn org_headline<'a>(line: &Line<'a>) -> HeadingDecision<'a> {
    let Some(level) = org_headline_level(line.text) else {
        return HeadingDecision::No;
    };
    heading_emit(
        line,
        HeadingKind::Bullet,
        level,
        None,
        level as usize,
        "org",
    )
}

fn heading_emit<'a>(
    line: &Line<'a>,
    kind: HeadingKind,
    level: u32,
    size: Option<u32>,
    content_start: usize,
    format: &str,
) -> HeadingDecision<'a> {
    let fields = split_heading_markers(line.text, content_start, line.eol == Eol::Eof);
    let title_start = line.start + fields.title_start;
    let title = fields.title;
    if !title.is_empty() && heading_title_needs_block_split(title, format) {
        return HeadingDecision::Split {
            heading: HeadingEmit {
                kind,
                level,
                size,
                marker: fields.marker,
                priority: fields.priority,
                title: "",
                title_start,
                span_start: line.start,
                span_end: title_start,
                tail_start: None,
            },
            title_start,
        };
    }
    let tail_start = fields.tail_start.map(|rel| line.start + rel);
    let span_end = tail_start.unwrap_or_else(|| {
        if line.eol == Eol::Cr {
            line_text_end(line)
        } else {
            line.end
        }
    });
    HeadingDecision::Emit(HeadingEmit {
        kind,
        level,
        size,
        marker: fields.marker,
        priority: fields.priority,
        title,
        title_start,
        span_start: line.start,
        span_end,
        tail_start,
    })
}

fn markdown_heading_at(s: &str) -> Option<(u32, u32, usize)> {
    let ws = mldoc_spaces_len(s);
    let rest0 = &s[ws..];
    let hashes = rest0.bytes().take_while(|&b| b == b'#').count();
    crate::metrics::scan_work(hashes + usize::from(hashes < rest0.len()));
    if hashes == 0 {
        return None;
    }
    let rest = &rest0[hashes..];
    mldoc_heading_boundary(rest).then_some((1 + ws as u32, hashes as u32, ws + hashes))
}

fn org_headline_level(s: &str) -> Option<u32> {
    if !s.starts_with('*') {
        return None;
    }
    let stars = s.bytes().take_while(|&b| b == b'*').count();
    crate::metrics::scan_work(stars + usize::from(stars < s.len()));
    let rest = &s[stars..];
    mldoc_heading_boundary(rest).then_some(stars as u32)
}

fn markdown_dash_bullet_level(s: &str) -> Option<u32> {
    let ws = mldoc_spaces_len(s);
    let rest = &s[ws..];
    let after = rest.strip_prefix('-')?;
    (mldoc_heading_boundary(after) || atx_size(after).0.is_some()).then_some(1 + ws as u32)
}

fn atx_size(s: &str) -> (Option<u32>, &str) {
    let hashes = s.bytes().take_while(|&b| b == b'#').count();
    crate::metrics::scan_work(hashes + usize::from(hashes < s.len()));
    if hashes > 0 {
        let after = &s[hashes..];
        if mldoc_heading_boundary(after) {
            return (Some(hashes as u32), mldoc_trim_spaces_start(after));
        }
    }
    (None, s)
}

fn dash_bullet_size_start(s: &str, after_dash: usize) -> (Option<u32>, usize) {
    let size_ws = mldoc_spaces_len(&s[after_dash..]);
    let hash_start = after_dash + size_ws;
    let hashes = s[hash_start..].bytes().take_while(|&b| b == b'#').count();
    crate::metrics::scan_work(hashes + usize::from(hash_start + hashes < s.len()));
    if hashes > 0 {
        let after_hashes = hash_start + hashes;
        if mldoc_heading_boundary(&s[after_hashes..]) {
            return (Some(hashes as u32), after_hashes);
        }
    }
    (None, after_dash)
}

struct HeadingMarkerFields<'a> {
    marker: Option<String>,
    priority: Option<String>,
    title: &'a str,
    title_start: usize,
    tail_start: Option<usize>,
}

fn split_heading_markers(
    line_text: &str,
    content_start: usize,
    marker_eof: bool,
) -> HeadingMarkerFields<'_> {
    let mut marker = None;
    let mut priority = None;
    let mut cur = content_start;
    let content_ws = mldoc_spaces_len(&line_text[cur..]);
    let field_start = cur + content_ws;

    for m in MARKERS {
        let Some(rest) = line_text[field_start..].strip_prefix(m) else {
            continue;
        };
        let after_marker = field_start + m.len();
        if rest.as_bytes().first() == Some(&b' ') || (rest.is_empty() && marker_eof) {
            crate::metrics::scan_work(m.len());
            marker = Some((*m).to_string());
            cur = after_marker;
            break;
        }
    }

    let priority_ws = mldoc_spaces_len(&line_text[cur..]);
    if priority_ws > 0 {
        let p = cur + priority_ws;
        let b = line_text[p..].as_bytes();
        if b.len() >= 4 && b[0] == b'[' && b[1] == b'#' && b[2] < 0x80 && b[3] == b']' {
            crate::metrics::scan_work(1);
            priority = Some((b[2] as char).to_string());
            cur = p + 4;
        }
    }

    let title_ws = mldoc_spaces_len(&line_text[cur..]);
    if title_ws > 0 {
        let title_start = cur + title_ws;
        if title_start < line_text.len() {
            return HeadingMarkerFields {
                marker,
                priority,
                title: &line_text[title_start..],
                title_start,
                tail_start: None,
            };
        }
        return HeadingMarkerFields {
            marker,
            priority,
            title: "",
            title_start,
            tail_start: Some(cur),
        };
    }

    if cur < line_text.len() && (marker.is_some() || priority.is_some()) {
        return HeadingMarkerFields {
            marker,
            priority,
            title: "",
            title_start: cur,
            tail_start: Some(cur),
        };
    }

    HeadingMarkerFields {
        marker,
        priority,
        title: if cur < line_text.len() {
            &line_text[field_start..]
        } else {
            ""
        },
        title_start: if cur < line_text.len() {
            field_start
        } else {
            cur
        },
        tail_start: None,
    }
}

struct BoundedSplit {
    blocks: Vec<Block>,
    next: usize,
    tail_start: Option<(usize, usize)>,
}

fn callout_container_split_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> Option<BoundedSplit> {
    match callout_container_sequence_at(source, line_idx, start_abs, format) {
        CalloutContainerDecision::Emit { block, next } => Some(BoundedSplit {
            blocks: vec![block],
            next,
            tail_start: None,
        }),
        CalloutContainerDecision::No => None,
    }
}

// mldoc uses unsafe lookahead in several places where a same-line suffix may be
// parsed as the next block. This helper owns only split suffixes whose extent is
// known without reparsing an arbitrary suffix window.
// scan-owner: (a2) caller-owned split suffix — one property/drawer group,
// same-line comment/heading/HR, one closed table run, displayed-math capture,
// one LaTeX environment, raw-HTML sequence, one v2-safe blockquote, one regular
// list, one Org fixed-width run, one footnote definition, one closed fence, or
// one closed special body block.
fn bounded_split_suffix_blocks(
    source: &Source<'_>,
    line_idx: usize,
    title_start: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Option<BoundedSplit> {
    let line = &source.lines[line_idx];
    let rel = title_start.checked_sub(line.start)?;
    let title = line.text.get(rel..)?;

    if let Some((name, value)) = directive_line(title) {
        let (span_end, next) = directive_span_end(&source.lines, line_idx);
        return Some(BoundedSplit {
            blocks: vec![Block::Directive {
                name,
                value,
                span: Some(Span(title_start, span_end)),
            }],
            next,
            tail_start: None,
        });
    }

    if let Some(text) = comment_line(title, format) {
        if format == "org" {
            let (span_end, next) = directive_span_end(&source.lines, line_idx);
            return Some(BoundedSplit {
                blocks: vec![Block::Comment {
                    text,
                    span: Some(Span(title_start, span_end)),
                }],
                next,
                tail_start: None,
            });
        }
        let content_end = line_text_end(line);
        return Some(BoundedSplit {
            blocks: vec![Block::Comment {
                text,
                span: Some(Span(title_start, content_end)),
            }],
            next: line_idx + 1,
            tail_start: (line.eol != Eol::Eof).then_some((line_idx, content_end)),
        });
    }

    if property_or_drawer_start(title, format) {
        match property_or_drawer_at(
            source,
            line_idx,
            title_start,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        ) {
            PropertyDrawerDecision::Emit {
                block,
                after_blocks,
                next,
                tail_start,
            } => {
                let mut blocks = vec![block];
                blocks.extend(after_blocks);
                return Some(BoundedSplit {
                    blocks,
                    next,
                    tail_start,
                });
            }
            PropertyDrawerDecision::Delegate => return None,
            PropertyDrawerDecision::No => {}
        }
    }

    if heading_start(title, format) {
        let suffix_line = Line {
            start: title_start,
            end: line.end,
            text: title,
            eol: line.eol,
            mldoc_spaces: mldoc_spaces_len(title),
        };
        match heading_line(&suffix_line, format) {
            HeadingDecision::Emit(heading) => {
                let drop_tail = heading.tail_start.is_some()
                    && empty_marker_tail_drops(source, line_idx, format, raw_html_scan);
                let mut block = heading.block(format);
                if drop_tail {
                    set_block_span_end(&mut block, line.end);
                }
                return Some(BoundedSplit {
                    blocks: vec![block],
                    next: line_idx + 1,
                    tail_start: heading
                        .tail_start
                        .filter(|_| !drop_tail)
                        .map(|tail| (line_idx, tail)),
                });
            }
            HeadingDecision::Split {
                mut heading,
                title_start,
            } => {
                if let Some(split) = bounded_split_suffix_blocks(
                    source,
                    line_idx,
                    title_start,
                    format,
                    drawer_end_cursor,
                    property_end_cursor,
                    fence_cursor,
                    raw_html_scan,
                ) {
                    let mut blocks = vec![heading.block(format)];
                    blocks.extend(split.blocks);
                    return Some(BoundedSplit {
                        blocks,
                        next: split.next,
                        tail_start: split.tail_start,
                    });
                }
                if heading_split_title_falls_back_to_inline(
                    source,
                    line_idx,
                    title_start,
                    format,
                    fence_cursor,
                    raw_html_scan,
                ) {
                    heading.title = &source.input[title_start..line_text_end(line)];
                    heading.title_start = title_start;
                    heading.span_end = if line.eol == Eol::Cr {
                        line_text_end(line)
                    } else {
                        line.end
                    };
                    return Some(BoundedSplit {
                        blocks: vec![heading.block(format)],
                        next: line_idx + 1,
                        tail_start: None,
                    });
                }
                return None;
            }
            HeadingDecision::No => return None,
        }
    }

    if hr_accepts_eol(line.eol) && is_hr_line(title, format) {
        return Some(BoundedSplit {
            blocks: vec![Block::Hr {
                span: Some(Span(title_start, line.end)),
            }],
            next: line_idx + 1,
            tail_start: None,
        });
    }

    if mldoc_trim_spaces_start(title).starts_with('|') {
        let (block, next, tail_start) = table_sequence_at(source, line_idx, title_start, format)?;
        return Some(BoundedSplit {
            blocks: vec![block],
            next,
            tail_start,
        });
    }

    if raw_html_tail_start(title) {
        return bounded_raw_html_split(
            source,
            line_idx,
            title_start,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        );
    }

    if mldoc_trim_spaces_start(title).starts_with("[:") {
        match hiccup_sequence_from(
            source,
            line_idx,
            title_start,
            format,
            drawer_end_cursor,
            property_end_cursor,
            fence_cursor,
            raw_html_scan,
        ) {
            HiccupDecision::Emit {
                blocks,
                next,
                tail_start,
            } => {
                return Some(BoundedSplit {
                    blocks,
                    next,
                    tail_start,
                });
            }
            HiccupDecision::Delegate | HiccupDecision::No => {}
        }
    }

    if blockquote_line_start_from(line, title_start) {
        return match markdown_blockquote_sequence_at(source, line_idx, title_start, format) {
            BlockquoteDecision::Emit { block, next } => Some(BoundedSplit {
                blocks: vec![block],
                next,
                tail_start: None,
            }),
            BlockquoteDecision::Delegate
            | BlockquoteDecision::Paragraph
            | BlockquoteDecision::No => None,
        };
    }

    if let Some(opener_off) = displayed_math_opener(title) {
        let opener = title_start + opener_off;
        let Some(close) = find_displayed_math_close(source.input, opener, source.input.len())
        else {
            return None;
        };
        let close_end = close + 2;
        let Some(close_line) = line_containing_text_pos(&source.lines, line_idx, close_end) else {
            return None;
        };
        let content_end = line_text_end(&source.lines[close_line]);
        let math_text = &source.input[opener + 2..close];
        crate::metrics::scan_work(math_text.len());
        if close_end < content_end {
            let math = Block::DisplayedMath {
                text: math_text.to_string(),
                span: Some(Span(title_start, close_end)),
            };
            let tail = &source.input[close_end..content_end];
            if let Some(split) = bounded_split_suffix_blocks(
                source,
                close_line,
                close_end,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                raw_html_scan,
            ) {
                let mut blocks = vec![math];
                blocks.extend(split.blocks);
                return Some(BoundedSplit {
                    blocks,
                    next: split.next,
                    tail_start: split.tail_start,
                });
            }
            if invalid_raw_html_tail_at(source, close_line, close_end, raw_html_scan)
                || rejected_fence_tail_at(source, close_line, close_end, fence_cursor)
                || rejected_table_tail_at(source, close_line, close_end, format)
                || rejected_regular_list_tail_at(source, close_line, close_end, format)
                || rejected_footnote_tail_at(source, close_line, close_end, format)
                || unclosed_markdown_drawer_tail_at(source, close_line, close_end, format)
                || rejected_markdown_property_tail_at(source, close_line, close_end, format)
                || rejected_directive_property_tail_at(source, close_line, close_end)
                || rejected_blockquote_tail_at(source, close_line, close_end, format)
                || malformed_latex_tail_at(source, close_line, close_end)
                || rejected_begin_tail_at(source, close_line, close_end)
            {
                return Some(BoundedSplit {
                    blocks: vec![math],
                    next: close_line + 1,
                    tail_start: Some((close_line, close_end)),
                });
            }
            if directive_line(tail).is_some()
                || comment_line(tail, format).is_some()
                || heading_start(tail, format)
                || property_or_drawer_start(tail, format)
                || footnote_definition_start(tail, source.lines[close_line].eol, format)
                || raw_html_tail_start(tail)
                || could_start_non_paragraph(tail, format)
            {
                return None;
            }
            return Some(BoundedSplit {
                blocks: vec![math],
                next: close_line + 1,
                tail_start: Some((close_line, close_end)),
            });
        }
        let mut next = close_line + 1;
        let mut span_end = source.lines[close_line].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        return Some(BoundedSplit {
            blocks: vec![Block::DisplayedMath {
                text: math_text.to_string(),
                span: Some(Span(title_start, span_end)),
            }],
            next,
            tail_start: None,
        });
    }

    if latex_env_opener_at(source, line_idx, title_start) {
        let capture = latex_env_capture(source, line_idx, title_start).ok()?;
        return Some(BoundedSplit {
            blocks: vec![capture.block],
            next: capture.next,
            tail_start: capture.tail_start,
        });
    }

    if let Some((_marker, info_start)) = fence_marker(title) {
        if line.eol == Eol::Cr {
            return None;
        }
        let close = find_matching_fence(&source.events.fence_lines, fence_cursor, line_idx)?;
        if lines_have_lone_cr(&source.lines[line_idx + 1..close]) {
            return None;
        }
        let code = if close > line_idx + 1 {
            fenced_code_text(&source.lines[line_idx + 1..close])
        } else {
            String::new()
        };
        let mut next = close + 1;
        let mut span_end = source.lines[close].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        return Some(BoundedSplit {
            blocks: vec![Block::Src {
                lang: fence_lang(&title[info_start..]),
                code,
                span: Some(Span(title_start, span_end)),
            }],
            next,
            tail_start: None,
        });
    }

    match regular_list_sequence_at(
        source,
        line_idx,
        title_start,
        format,
        BlockParseContext::Document,
    ) {
        ListDecision::Emit { block, next } => {
            return Some(BoundedSplit {
                blocks: vec![block],
                next,
                tail_start: None,
            });
        }
        ListDecision::Delegate | ListDecision::Paragraph | ListDecision::No => {}
    }

    if let Some((block, next)) = org_verbatim_sequence_at(source, line_idx, title_start, format) {
        return Some(BoundedSplit {
            blocks: vec![block],
            next,
            tail_start: None,
        });
    }

    if let Some((block, next)) = footnote_sequence_at(source, line_idx, title_start, format) {
        return Some(BoundedSplit {
            blocks: vec![block],
            next,
            tail_start: None,
        });
    }

    let name = block_begin_name(title)?;
    if !special_body_block_name(&name) || line.eol == Eol::Cr {
        return None;
    }
    let close = source.events.callout_ends.first_after(&name, line_idx)?;
    if lines_have_lone_cr(&source.lines[line_idx + 1..close]) {
        return None;
    }
    crate::metrics::scan_work(close.saturating_sub(line_idx + 1));
    let texts: Vec<&str> = (line_idx + 1..close)
        .map(|idx| source.lines[idx].text)
        .collect();
    let code = crate::org::block_code_texts(&texts);
    let mut next = close + 1;
    let mut span_end = source.lines[close].end;
    while next < source.lines.len() && source.lines[next].text.is_empty() {
        crate::metrics::scan_work(1);
        span_end = source.lines[next].end;
        next += 1;
    }
    let span = Some(Span(title_start, span_end));
    let block = if name.eq_ignore_ascii_case("SRC") {
        Block::Src {
            lang: crate::org::begin_lang(title),
            code,
            span,
        }
    } else if name.eq_ignore_ascii_case("EXAMPLE") {
        Block::Example { code, span }
    } else if name.eq_ignore_ascii_case("EXPORT") {
        let (name, options) = begin_export_fields(title);
        Block::Export {
            name,
            options,
            content: code,
            span,
        }
    } else {
        debug_assert!(name.eq_ignore_ascii_case("COMMENT"));
        Block::CommentBlock {
            content: code,
            span,
        }
    };
    Some(BoundedSplit {
        blocks: vec![block],
        next,
        tail_start: None,
    })
}

fn bounded_raw_html_split(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    scan: &mut RawHtmlScan,
) -> Option<BoundedSplit> {
    let mut blocks = Vec::new();
    let mut line_idx = line_idx;
    let mut start_abs = start_abs;
    let mut same_line_tail = false;

    loop {
        if line_idx >= source.lines.len() {
            return Some(BoundedSplit {
                blocks,
                next: line_idx,
                tail_start: None,
            });
        }
        let line = &source.lines[line_idx];
        let content_end = line_text_end(line);
        if start_abs > content_end {
            return if blocks.is_empty() {
                None
            } else {
                Some(BoundedSplit {
                    blocks,
                    next: line_idx,
                    tail_start: None,
                })
            };
        }
        let ws = mldoc_spaces_len(&source.input[start_abs..content_end]);
        let opener = start_abs + ws;
        if !source.input[opener..].starts_with('<') {
            return bounded_raw_html_tail(
                source,
                line_idx,
                start_abs,
                same_line_tail,
                blocks,
                format,
                drawer_end_cursor,
                property_end_cursor,
                fence_cursor,
                scan,
            );
        }

        let extent =
            parse_raw_html_at_cached(source.input, opener, source.input.len(), Some(scan))?;
        let close_end = extent.end;
        let close_line = line_containing_text_pos(&source.lines, line_idx, close_end)?;
        let close_content_end = line_text_end(&source.lines[close_line]);
        let text = raw_html_capture_text(source.input, opener, close_end);

        if close_end < close_content_end {
            blocks.push(Block::RawHtml {
                text,
                span: Some(Span(start_abs, close_end)),
            });
            line_idx = close_line;
            start_abs = close_end;
            same_line_tail = true;
            continue;
        }

        let mut next = close_line + 1;
        let mut span_end = source.lines[close_line].end;
        while next < source.lines.len() && source.lines[next].text.is_empty() {
            crate::metrics::scan_work(1);
            span_end = source.lines[next].end;
            next += 1;
        }
        blocks.push(Block::RawHtml {
            text,
            span: Some(Span(start_abs, span_end)),
        });

        if next >= source.lines.len() {
            return Some(BoundedSplit {
                blocks,
                next,
                tail_start: None,
            });
        }
        line_idx = next;
        start_abs = source.lines[next].start;
        same_line_tail = false;
    }
}

fn bounded_raw_html_tail(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    same_line_tail: bool,
    mut blocks: Vec<Block>,
    format: &str,
    drawer_end_cursor: &mut usize,
    property_end_cursor: &mut usize,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> Option<BoundedSplit> {
    if blocks.is_empty() {
        return None;
    }
    if !same_line_tail {
        return Some(BoundedSplit {
            blocks,
            next: line_idx,
            tail_start: None,
        });
    }
    let line = &source.lines[line_idx];
    let text = &source.input[start_abs..line_text_end(line)];
    if text.is_empty() {
        return Some(BoundedSplit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        });
    }
    if let Some(split) = bounded_split_suffix_blocks(
        source,
        line_idx,
        start_abs,
        format,
        drawer_end_cursor,
        property_end_cursor,
        fence_cursor,
        raw_html_scan,
    ) {
        blocks.extend(split.blocks);
        return Some(BoundedSplit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        });
    }
    if let Some(split) = callout_container_split_at(source, line_idx, start_abs, format) {
        blocks.extend(split.blocks);
        return Some(BoundedSplit {
            blocks,
            next: split.next,
            tail_start: split.tail_start,
        });
    }
    if hr_accepts_eol(line.eol) && is_hr_line(text, format) {
        blocks.push(Block::Hr {
            span: Some(Span(start_abs, line.end)),
        });
        return Some(BoundedSplit {
            blocks,
            next: line_idx + 1,
            tail_start: None,
        });
    }
    if invalid_raw_html_tail_at(source, line_idx, start_abs, raw_html_scan)
        || unclosed_displayed_math_tail_at(source, line_idx, start_abs)
        || rejected_fence_tail_at(source, line_idx, start_abs, fence_cursor)
        || rejected_table_tail_at(source, line_idx, start_abs, format)
        || rejected_regular_list_tail_at(source, line_idx, start_abs, format)
        || rejected_footnote_tail_at(source, line_idx, start_abs, format)
        || unclosed_markdown_drawer_tail_at(source, line_idx, start_abs, format)
        || rejected_markdown_property_tail_at(source, line_idx, start_abs, format)
        || rejected_directive_property_tail_at(source, line_idx, start_abs)
        || rejected_blockquote_tail_at(source, line_idx, start_abs, format)
        || malformed_latex_tail_at(source, line_idx, start_abs)
        || rejected_begin_tail_at(source, line_idx, start_abs)
    {
        return Some(BoundedSplit {
            blocks,
            next: line_idx + 1,
            tail_start: Some((line_idx, start_abs)),
        });
    }
    if directive_line(text).is_some()
        || comment_line(text, format).is_some()
        || heading_start(text, format)
        || property_or_drawer_start(text, format)
        || footnote_definition_start(text, source.lines[line_idx].eol, format)
        || raw_html_tail_start(text)
        || could_start_non_paragraph(text, format)
    {
        return None;
    }
    Some(BoundedSplit {
        blocks,
        next: line_idx + 1,
        tail_start: Some((line_idx, start_abs)),
    })
}

fn heading_title_needs_block_split(title: &str, format: &str) -> bool {
    let t = mldoc_trim_spaces_start(title);
    if t.is_empty() {
        return false;
    }
    if heading_title_directive_line(t).is_some()
        || starts_ci(t, "#+BEGIN_")
        || heading_title_property_or_drawer_start(t, format)
        || (format == "org" && t.starts_with(':'))
        || t.starts_with('|')
    {
        return true;
    }
    if t.starts_with("\\begin{")
        || t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with("$$")
        || t.starts_with('>')
        || t.starts_with('<')
        || (format != "org" && t.starts_with("[^"))
    {
        return true;
    }
    if format == "org" && t.starts_with("[fn:") {
        return true;
    }
    is_hr_line(t, format)
}

fn heading_split_title_falls_back_to_inline(
    source: &Source<'_>,
    line_idx: usize,
    title_start: usize,
    format: &str,
    fence_cursor: &mut usize,
    raw_html_scan: &mut RawHtmlScan,
) -> bool {
    invalid_raw_html_tail_at(source, line_idx, title_start, raw_html_scan)
        || unclosed_displayed_math_tail_at(source, line_idx, title_start)
        || rejected_fence_tail_at(source, line_idx, title_start, fence_cursor)
        || rejected_table_tail_at(source, line_idx, title_start, format)
        || rejected_footnote_tail_at(source, line_idx, title_start, format)
        || unclosed_markdown_drawer_tail_at(source, line_idx, title_start, format)
        || rejected_markdown_property_tail_at(source, line_idx, title_start, format)
        || rejected_directive_property_tail_at(source, line_idx, title_start)
        || rejected_blockquote_tail_at(source, line_idx, title_start, format)
        || malformed_latex_tail_at(source, line_idx, title_start)
        || rejected_begin_tail_at(source, line_idx, title_start)
}

fn heading_split_title_is_suppressed_begin(
    source: &Source<'_>,
    line_idx: usize,
    title_start: usize,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(title_start..content_end) else {
        return false;
    };
    starts_ci(mldoc_trim_spaces_start(text), "#+BEGIN_")
}

// mldoc's empty heading/bullet marker opens a whitespace paragraph from the
// marker-line tail. A following drop-trigger block removes the marker-line part;
// true blank lines after it remain for the normal paragraph path.
// scan-owner: (a2) caller-owned line helper — look ahead across contiguous blank
// lines only when an empty marker was just consumed.
fn empty_marker_tail_drops(
    source: &Source<'_>,
    i: usize,
    format: &str,
    raw_html_scan: &mut RawHtmlScan,
) -> bool {
    let lines = &source.lines;
    let mut j = i + 1;
    while j < lines.len() && lines[j].text.is_empty() && lines[j].eol != Eol::Eof {
        crate::metrics::scan_work(1);
        j += 1;
    }
    let Some(line) = lines.get(j) else {
        return false;
    };
    directive_line(line.text).is_some()
        || (format == "org" && drawer_begin(line.text).is_some())
        || (format == "org" && org_verbatim_line(line.text))
        || table_line_start(line)
        || fence_marker(line.text).is_some()
        || block_begin_name(line.text).is_some_and(|name| special_body_block_name(&name))
        || empty_callout_line_matches(source, j)
        || displayed_math_opener(line.text).is_some()
        || raw_html_line_matches(source, j, raw_html_scan)
        || blockquote_line_content(line, true).is_some()
        || (hr_accepts_eol(line.eol) && is_hr_line(line.text, format))
}

fn empty_callout_line_matches(source: &Source<'_>, line_idx: usize) -> bool {
    let line = &source.lines[line_idx];
    let Some(name) = block_begin_name(line.text) else {
        return false;
    };
    if name.eq_ignore_ascii_case("SRC")
        || name.eq_ignore_ascii_case("EXAMPLE")
        || name.eq_ignore_ascii_case("EXPORT")
        || name.eq_ignore_ascii_case("COMMENT")
        || line.eol == Eol::Cr
    {
        return false;
    }
    let Some(close) = source.lines.get(line_idx + 1) else {
        return false;
    };
    let t = mldoc_trim_spaces_start(close.text);
    let Some(suffix) = t.get(6..) else {
        return false;
    };
    starts_ci(t, "#+END_")
        && suffix.len() >= name.len()
        && suffix.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
}

fn raw_html_line_matches(source: &Source<'_>, line_idx: usize, scan: &mut RawHtmlScan) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let ws = mldoc_spaces_len(line.text);
    let opener = line.start + ws;
    opener < content_end
        && source.input[opener..].starts_with('<')
        && parse_raw_html_at_cached(source.input, opener, source.input.len(), Some(scan)).is_some()
}

fn extract_org_htags(title: &mut Vec<Inline>) -> Vec<String> {
    let Some(Inline::Plain {
        text,
        span: old_span,
        span_map: old_map,
    }) = title.last()
    else {
        return Vec::new();
    };
    let old_text = text.clone();
    crate::metrics::scan_work(text.len());
    let old_span = *old_span;
    let old_map = old_map.clone();
    let s = text.trim().to_string();
    crate::metrics::scan_work(text.len() + s.len());
    if s.len() <= 1 || !s.ends_with(':') {
        return Vec::new();
    }
    crate::metrics::scan_work(s.len());
    let (prefix, maybe_tags): (String, &str) = match s.rfind(' ') {
        Some(p) => {
            crate::metrics::scan_work(p + 1);
            (s[..p + 1].to_string(), &s[p + 1..])
        }
        None => (String::new(), s.as_str()),
    };
    let Some(tags) = parse_org_tags(maybe_tags) else {
        return Vec::new();
    };
    title.pop();
    if !prefix.is_empty() {
        let trimmed_start = old_text.len() - old_text.trim_start().len();
        let prefix_start = trimmed_start;
        let prefix_end = prefix_start + prefix.len();
        title.push(plain_from_existing_slice(
            &old_text,
            prefix_start,
            prefix_end,
            prefix,
            old_span,
            old_map.as_deref(),
        ));
    }
    if matches!(title.last(), Some(Inline::Plain { .. })) {
        let Some(Inline::Plain {
            text,
            span,
            span_map,
        }) = title.last().cloned()
        else {
            unreachable!();
        };
        let trimmed = text.trim_end();
        crate::metrics::scan_work(text.len());
        let slice_end = if trimmed.len() < text.len() {
            trimmed.len() + 1
        } else {
            trimmed.len()
        };
        crate::metrics::scan_work(trimmed.len() + 1);
        let replacement = plain_from_existing_slice(
            &text,
            0,
            slice_end,
            format!("{trimmed} "),
            span,
            span_map.as_deref(),
        );
        *title.last_mut().unwrap() = replacement;
    }
    tags
}

fn plain_from_existing_slice(
    old_text: &str,
    slice_start: usize,
    slice_end: usize,
    text: String,
    old_span: Option<Span>,
    old_map: Option<&[SpanMapSegment]>,
) -> Inline {
    let source_slice = &old_text[slice_start..slice_end];
    if let Some(map) = old_map {
        let mut out = Vec::new();
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        // scan-owner: (a) consumed-on-match accepted copy — Org heading tag span-map slice walk.
        for SpanMapSegment(text_off, src_off, len) in map.iter().copied() {
            crate::metrics::scan_work(1);
            let seg_end = text_off + len;
            let a = slice_start.max(text_off);
            let b = slice_end.min(seg_end);
            if a >= b {
                continue;
            }
            let src = src_off + (a - text_off);
            crate::source_map::push_wire_segment(&mut out, a - slice_start, src, b - a);
            lo = lo.min(src);
            hi = hi.max(src + (b - a));
        }
        let span = if lo != usize::MAX {
            Span(lo, hi)
        } else {
            let p = old_span.map(|s| s.0).unwrap_or(0);
            Span(p, p)
        };
        Inline::Plain {
            text,
            span: Some(span),
            span_map: Some(out),
        }
    } else if let Some(Span(start, _)) = old_span {
        let span = Span(start + slice_start, start + slice_end);
        let span_map = if source_slice.as_bytes() == text.as_bytes() {
            None
        } else {
            let mut out = Vec::new();
            let copied_len = source_slice
                .as_bytes()
                .iter()
                .zip(text.as_bytes())
                .take_while(|(a, b)| a == b)
                .count();
            crate::metrics::scan_work(
                copied_len + usize::from(copied_len < source_slice.len().min(text.len())),
            );
            crate::source_map::push_wire_segment(&mut out, 0, span.0, copied_len);
            Some(out)
        };
        Inline::Plain {
            text,
            span: Some(span),
            span_map,
        }
    } else {
        Inline::Plain {
            text,
            span: Some(Span(0, 0)),
            span_map: Some(Vec::new()),
        }
    }
}

fn parse_org_tags(s: &str) -> Option<Vec<String>> {
    if s.len() < 2 || !s.starts_with(':') || !s.ends_with(':') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    // scan-owner: (a2) caller-owned line helper — Org headline tag split and validation.
    for tok in inner.split(':') {
        crate::metrics::scan_work(tok.len() + 1);
        if tok.is_empty() {
            return None;
        }
        crate::metrics::scan_work(tok.len());
        if tok.bytes().any(|b| b == b' ' || b == b'\t') {
            return None;
        }
        crate::metrics::scan_work(tok.len());
        out.push(tok.to_string());
    }
    Some(out)
}

// mldoc source:
// - Org: lib/syntax/comment.ml `between_eols ((char '#' <* ws) *> line)`.
// - Markdown: lib/syntax/markdown_comment.ml line-comment branch, not wrapped in
//   `between_eols`, so the trailing EOL remains a paragraph separator.
// scan-owner: (a2) caller-owned line helper — comment recognition scans only the
// current line text; Org reuses the monotone contiguous-EOL span helper.
fn comment_line(text: &str, format: &str) -> Option<String> {
    if format == "org" {
        org_comment_line(text)
    } else {
        markdown_comment_line(text)
    }
}

fn org_comment_line(text: &str) -> Option<String> {
    let ws = mldoc_spaces_len(text);
    let rest = &text[ws..];
    let rest = rest.strip_prefix('#')?;
    let comment_ws = rest
        .as_bytes()
        .iter()
        .take_while(|&&b| mldoc_is_space(b))
        .count();
    crate::metrics::scan_work(comment_ws + usize::from(comment_ws < rest.len()));
    if comment_ws == 0 || comment_ws == rest.len() {
        return None;
    }
    Some(rest[comment_ws..].to_string())
}

fn markdown_comment_line(text: &str) -> Option<String> {
    let ws = mldoc_spaces_len(text);
    let mut rest = &text[ws..];
    rest = rest.strip_prefix("[//]: #")?;
    let body_ws = rest
        .as_bytes()
        .iter()
        .take_while(|&&b| mldoc_is_space(b))
        .count();
    crate::metrics::scan_work(body_ws + usize::from(body_ws < rest.len()));
    if body_ws == rest.len() {
        return None;
    }
    Some(rest[body_ws..].to_string())
}

fn could_start_non_paragraph(text: &str, format: &str) -> bool {
    let ws = mldoc_spaces_len(text);
    let t = &text[ws..];
    if t.is_empty() {
        return false;
    }

    if starts_ci(t, "#+BEGIN_") || (format == "org" && t.starts_with(':')) {
        return true;
    }
    if t.starts_with("\\begin{")
        || t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with("$$")
        || t.starts_with('>')
    {
        return true;
    }

    if format == "org" {
        org_heading_or_list_start(t, ws) || ordered_list_start(t)
    } else {
        markdown_heading_start(t)
            || markdown_bullet_or_list_start(t)
            || markdown_property_start(t)
            || ordered_list_start(t)
    }
}

fn raw_html_tail_start(text: &str) -> bool {
    let ws = mldoc_spaces_len(text);
    text[ws..].starts_with('<')
}

fn invalid_raw_html_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    scan: &mut RawHtmlScan,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    if start_abs > content_end {
        return false;
    }
    let ws = mldoc_spaces_len(&source.input[start_abs..content_end]);
    let opener = start_abs + ws;
    opener < content_end
        && source.input[opener..].starts_with('<')
        && parse_raw_html_at_cached(source.input, opener, source.input.len(), Some(scan)).is_none()
}

fn unclosed_displayed_math_tail_at(source: &Source<'_>, line_idx: usize, start_abs: usize) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let Some(opener_off) = displayed_math_opener(text) else {
        return false;
    };
    let opener = start_abs + opener_off;
    find_displayed_math_close(source.input, opener, source.input.len()).is_none()
}

// scan-owner: (a2) caller-owned fence rejection helper — current-line marker
// check is local, the fence cursor is monotone, and the optional CR-body scan
// covers only the candidate body interval before it becomes paragraph text.
fn rejected_fence_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    fence_cursor: &mut usize,
) -> bool {
    let line = &source.lines[line_idx];
    let Some(rel) = start_abs.checked_sub(line.start) else {
        return false;
    };
    let Some(text) = line.text.get(rel..) else {
        return false;
    };
    if fence_marker(text).is_none() {
        return false;
    }
    if line.eol == Eol::Cr {
        return true;
    }
    let mut probe = *fence_cursor;
    let Some(close) = find_matching_fence(&source.events.fence_lines, &mut probe, line_idx) else {
        *fence_cursor = probe;
        return true;
    };
    let rejected = lines_have_lone_cr(&source.lines[line_idx + 1..close]);
    if rejected {
        *fence_cursor = probe;
    }
    rejected
}

fn rejected_table_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    if !mldoc_trim_spaces_start(text).starts_with('|') {
        return false;
    }
    table_sequence_at(source, line_idx, start_abs, format).is_none()
}

fn rejected_regular_list_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let ws = mldoc_spaces_len(text);
    let t = &text[ws..];
    if t.is_empty() || !regular_list_candidate_start(t, ws, format) {
        return false;
    }
    regular_list_marker_at(line, start_abs, format).is_none()
}

fn list_content_regular_list_candidate_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let ws = mldoc_spaces_len(text);
    let t = &text[ws..];
    !t.is_empty() && regular_list_candidate_start(t, ws, format)
}

fn rejected_footnote_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let t = mldoc_trim_spaces_start(text);
    let starts = if format == "org" {
        t.starts_with("[fn:")
    } else {
        t.starts_with("[^")
    };
    starts && !footnote_definition_start(text, line.eol, format)
}

fn unclosed_markdown_drawer_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    if format == "org" {
        return false;
    }
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    if drawer_begin(text).is_none() {
        return false;
    }
    let ends = &source.events.drawer_end_lines;
    crate::metrics::scan_work(usize::from(!ends.is_empty()));
    ends.partition_point(|&idx| idx <= line_idx) == ends.len()
}

fn rejected_markdown_property_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    if format == "org" {
        return false;
    }
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let t = mldoc_trim_spaces_start(text);
    markdown_property_start(t) && markdown_property_line(text).is_none()
}

fn rejected_directive_property_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
) -> bool {
    let line = &source.lines[line_idx];
    let content_end = line_text_end(line);
    let Some(text) = source.input.get(start_abs..content_end) else {
        return false;
    };
    let t = mldoc_trim_spaces_start(text);
    t.starts_with("#+") && !starts_ci(t, "#+BEGIN_") && directive_property_line(text).is_none()
}

fn regular_list_candidate_start(t: &str, indent: usize, format: &str) -> bool {
    ordered_list_start(t)
        || if format == "org" {
            let bytes = t.as_bytes();
            match bytes.first().copied() {
                Some(b'+' | b'-') => bytes.get(1).is_some_and(|&b| mldoc_is_space(b)),
                Some(b'*') if indent > 0 => bytes.get(1).is_some_and(|&b| mldoc_is_space(b)),
                _ => false,
            }
        } else {
            let bytes = t.as_bytes();
            matches!(bytes.first(), Some(b'*' | b'+'))
                && bytes.get(1).is_some_and(|&b| mldoc_is_space(b))
        }
}

fn rejected_blockquote_tail_at(
    source: &Source<'_>,
    line_idx: usize,
    start_abs: usize,
    format: &str,
) -> bool {
    matches!(
        markdown_blockquote_sequence_at(source, line_idx, start_abs, format),
        BlockquoteDecision::Paragraph
    )
}

fn malformed_latex_tail_at(source: &Source<'_>, line_idx: usize, start_abs: usize) -> bool {
    latex_env_opener_at(source, line_idx, start_abs)
        && latex_env_capture(source, line_idx, start_abs).is_err()
}

// mldoc source: `Block.parse` only accepts `#+BEGIN_*` containers when the
// opener, body, and closer are separated by LF/CRLF/EOF line structure. Lone CR
// candidates fall through to paragraph text even if a matching `#+END_*` exists.
// scan-owner: (a2) caller-owned begin rejection helper — the current-line start
// check is local, the closer trie cursor is monotone, and the CR-contamination
// walk visits only the candidate body interval before the caller emits it once
// as paragraph text.
fn rejected_begin_tail_at(source: &Source<'_>, line_idx: usize, start_abs: usize) -> bool {
    let line = &source.lines[line_idx];
    let Some(rel) = start_abs.checked_sub(line.start) else {
        return false;
    };
    let Some(text) = line.text.get(rel..) else {
        return false;
    };
    if !starts_ci(mldoc_trim_spaces_start(text), "#+BEGIN_") {
        return false;
    }
    let Some(name) = block_begin_name(text) else {
        return true;
    };
    let Some(close) = source.events.callout_ends.first_after(&name, line_idx) else {
        return true;
    };
    line.eol == Eol::Cr || lines_have_lone_cr(&source.lines[line_idx + 1..close])
}

// scan-owner: (a2) caller-owned line-metadata scan — callers pass a single
// candidate body interval that is either accepted or emitted as paragraph text.
fn lines_have_lone_cr(lines: &[Line<'_>]) -> bool {
    let mut has_cr = false;
    for line in lines {
        crate::metrics::scan_work(1);
        has_cr |= line.eol == Eol::Cr;
    }
    has_cr
}

fn line_text_end(line: &Line<'_>) -> usize {
    line.start + line.text.len()
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    let p = prefix.as_bytes();
    let b = s.as_bytes();
    b.len() >= p.len() && b[..p.len()].eq_ignore_ascii_case(p)
}

fn markdown_heading_start(t: &str) -> bool {
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    crate::metrics::scan_work(hashes + usize::from(hashes < t.len()));
    hashes > 0 && mldoc_heading_boundary(&t[hashes..])
}

fn markdown_bullet_or_list_start(t: &str) -> bool {
    let bytes = t.as_bytes();
    match bytes[0] {
        b'-' => {
            let after = &t[1..];
            mldoc_heading_boundary(after) || markdown_atx_start(after)
        }
        b'*' | b'+' => bytes.get(1).is_some_and(|&b| mldoc_is_space(b)),
        _ => false,
    }
}

fn markdown_atx_start(s: &str) -> bool {
    let ws = mldoc_spaces_len(s);
    let rest = &s[ws..];
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    crate::metrics::scan_work(hashes + usize::from(hashes < rest.len()));
    hashes > 0 && mldoc_heading_boundary(&rest[hashes..])
}

fn org_heading_or_list_start(t: &str, indent: usize) -> bool {
    let bytes = t.as_bytes();
    match bytes[0] {
        b'*' => {
            let stars = bytes.iter().take_while(|&&b| b == b'*').count();
            crate::metrics::scan_work(stars + usize::from(stars < bytes.len()));
            (indent == 0 && mldoc_heading_boundary(&t[stars..]))
                || (indent > 0 && bytes.get(1).is_some_and(|&b| mldoc_is_space(b)))
        }
        b'+' | b'-' => bytes.get(1).is_some_and(|&b| mldoc_is_space(b)),
        _ => false,
    }
}

fn ordered_list_start(t: &str) -> bool {
    let bytes = t.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    crate::metrics::scan_work(digits + usize::from(digits < bytes.len()));
    digits > 0
        && bytes.get(digits) == Some(&b'.')
        && bytes.get(digits + 1).is_some_and(|&b| mldoc_is_space(b))
}

fn markdown_property_start(t: &str) -> bool {
    let bytes = t.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] != b':' && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    crate::metrics::scan_work(i + usize::from(i < bytes.len()));
    i > 0 && bytes.get(i..i + 2) == Some(&b"::"[..])
}

fn offset_blocks(blocks: &mut [Block], delta: usize) {
    if delta == 0 {
        return;
    }
    for block in blocks {
        offset_block(block, delta);
    }
}

fn offset_block(block: &mut Block, delta: usize) {
    match block {
        Block::Paragraph { inline, span } => {
            offset_inlines(inline, delta);
            offset_span(span, delta);
        }
        Block::Heading { inline, span, .. } | Block::Bullet { inline, span, .. } => {
            offset_inlines(inline, delta);
            offset_span(span, delta);
        }
        Block::List { items, span } => {
            offset_list_items(items, delta);
            offset_span(span, delta);
        }
        Block::Src { span, .. }
        | Block::Export { span, .. }
        | Block::CommentBlock { span, .. }
        | Block::RawHtml { span, .. }
        | Block::DisplayedMath { span, .. }
        | Block::Drawer { span, .. }
        | Block::Directive { span, .. }
        | Block::Comment { span, .. }
        | Block::Example { span, .. }
        | Block::LatexEnv { span, .. }
        | Block::Properties { span, .. }
        | Block::Hr { span }
        | Block::Hiccup { span, .. }
        | Block::Results { span } => offset_span(span, delta),
        Block::Quote { children, span } | Block::Custom { children, span, .. } => {
            offset_blocks(children, delta);
            offset_span(span, delta);
        }
        Block::Table {
            header, rows, span, ..
        } => {
            if let Some(header) = header {
                for cell in header {
                    offset_inlines(cell, delta);
                }
            }
            for row in rows {
                for cell in row {
                    offset_inlines(cell, delta);
                }
            }
            offset_span(span, delta);
        }
        Block::FootnoteDef { inline, span, .. } => {
            offset_inlines(inline, delta);
            offset_span(span, delta);
        }
    }
}

fn offset_list_items(items: &mut [ListItem], delta: usize) {
    for item in items {
        offset_blocks(&mut item.content, delta);
        offset_list_items(&mut item.items, delta);
        offset_inlines(&mut item.name, delta);
    }
}

fn offset_span(span: &mut Option<Span>, delta: usize) {
    if let Some(Span(start, end)) = span {
        *start += delta;
        *end += delta;
    }
}

fn remap_blocks_from_origin(
    blocks: &mut [Block],
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
) {
    let mut inline_cursor = OriginCursor::new();
    for block in blocks {
        remap_block_from_origin(
            block,
            current_input,
            source_input,
            origin,
            &mut inline_cursor,
        );
    }
}

fn remap_block_from_origin(
    block: &mut Block,
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
    inline_cursor: &mut OriginCursor,
) {
    match block {
        Block::Paragraph { inline, span } => {
            crate::source_map::remap_inlines(
                inline,
                current_input,
                source_input,
                origin,
                inline_cursor,
            );
            remap_block_span(span, origin);
        }
        Block::Heading { inline, span, .. } | Block::Bullet { inline, span, .. } => {
            crate::source_map::remap_inlines(
                inline,
                current_input,
                source_input,
                origin,
                inline_cursor,
            );
            remap_block_span(span, origin);
        }
        Block::List { items, span } => {
            remap_list_items_from_origin(items, current_input, source_input, origin, inline_cursor);
            remap_block_span(span, origin);
        }
        Block::Src { span, .. }
        | Block::Export { span, .. }
        | Block::CommentBlock { span, .. }
        | Block::RawHtml { span, .. }
        | Block::DisplayedMath { span, .. }
        | Block::Drawer { span, .. }
        | Block::Directive { span, .. }
        | Block::Comment { span, .. }
        | Block::Example { span, .. }
        | Block::LatexEnv { span, .. }
        | Block::Properties { span, .. }
        | Block::Hr { span }
        | Block::Hiccup { span, .. }
        | Block::Results { span } => remap_block_span(span, origin),
        Block::Quote { children, span } | Block::Custom { children, span, .. } => {
            for child in children {
                remap_block_from_origin(child, current_input, source_input, origin, inline_cursor);
            }
            remap_block_span(span, origin);
        }
        Block::Table {
            header, rows, span, ..
        } => {
            if let Some(header) = header {
                for cell in header {
                    crate::source_map::remap_inlines(
                        cell,
                        current_input,
                        source_input,
                        origin,
                        inline_cursor,
                    );
                }
            }
            for row in rows {
                for cell in row {
                    crate::source_map::remap_inlines(
                        cell,
                        current_input,
                        source_input,
                        origin,
                        inline_cursor,
                    );
                }
            }
            remap_block_span(span, origin);
        }
        Block::FootnoteDef { inline, span, .. } => {
            crate::source_map::remap_inlines(
                inline,
                current_input,
                source_input,
                origin,
                inline_cursor,
            );
            remap_block_span(span, origin);
        }
    }
}

fn remap_list_items_from_origin(
    items: &mut [ListItem],
    current_input: &str,
    source_input: &str,
    origin: &OriginMap,
    inline_cursor: &mut OriginCursor,
) {
    for item in items {
        crate::source_map::remap_inlines(
            &mut item.name,
            current_input,
            source_input,
            origin,
            inline_cursor,
        );
        for block in &mut item.content {
            let local_paragraph_span = match block {
                Block::Paragraph { span, .. } => *span,
                _ => None,
            };
            remap_block_from_origin(block, current_input, source_input, origin, inline_cursor);
            if let (Block::Paragraph { span, .. }, Some(local_span)) = (block, local_paragraph_span)
            {
                *span = Some(local_span);
            }
        }
        remap_list_items_from_origin(
            &mut item.items,
            current_input,
            source_input,
            origin,
            inline_cursor,
        );
    }
}

fn remap_block_span(span: &mut Option<Span>, origin: &OriginMap) {
    if let Some(local) = *span {
        *span = Some(origin_span_indexed(local, origin));
    }
}

fn clear_paragraph_block_spans(blocks: &mut [Block]) {
    for block in blocks {
        match block {
            Block::Paragraph { span, .. } => *span = None,
            Block::List { items, .. } => clear_list_item_paragraph_block_spans(items),
            Block::Quote { children, .. } | Block::Custom { children, .. } => {
                clear_paragraph_block_spans(children);
            }
            _ => {}
        }
    }
}

fn clear_list_item_paragraph_block_spans(items: &mut [ListItem]) {
    for item in items {
        clear_paragraph_block_spans(&mut item.content);
        clear_list_item_paragraph_block_spans(&mut item.items);
    }
}

fn origin_span_indexed(span: Span, origin: &OriginMap) -> Span {
    if span.0 >= span.1 {
        let p = origin_boundary_indexed(span.0, origin);
        return Span(p, p);
    }
    let segments = origin.segments();
    let mut idx = first_origin_segment_after_text(segments, span.0);
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    while let Some(seg) = segments.get(idx).copied() {
        crate::metrics::scan_work(1);
        if seg.text_off >= span.1 {
            break;
        }
        let a = span.0.max(seg.text_off);
        let b = span.1.min(seg.text_off + seg.text_len);
        if a < b {
            let (src, src_len) = origin_segment_src_subrange(seg, a, b);
            lo = lo.min(src);
            hi = hi.max(src + src_len);
        }
        idx += 1;
    }
    if lo == usize::MAX {
        let p = origin_boundary_indexed(span.0, origin);
        Span(p, p)
    } else {
        Span(lo, hi)
    }
}

fn origin_boundary_indexed(text_off: usize, origin: &OriginMap) -> usize {
    let segments = origin.segments();
    let idx = first_origin_segment_after_text(segments, text_off);
    if let Some(seg) = segments.get(idx).copied() {
        crate::metrics::scan_work(1);
        if text_off < seg.text_off {
            return idx
                .checked_sub(1)
                .map(|prev| {
                    let prev = segments[prev];
                    prev.src_off + prev.src_len
                })
                .unwrap_or(seg.src_off);
        }
        if seg.text_len == seg.src_len {
            return seg.src_off + (text_off - seg.text_off).min(seg.src_len);
        }
        return if text_off == seg.text_off {
            seg.src_off
        } else {
            seg.src_off + seg.src_len
        };
    }
    segments
        .last()
        .map(|seg| seg.src_off + seg.src_len)
        .unwrap_or(0)
}

fn first_origin_segment_after_text(
    segments: &[crate::source_map::OriginSegment],
    text_off: usize,
) -> usize {
    let mut lo = 0usize;
    let mut hi = segments.len();
    while lo < hi {
        crate::metrics::scan_work(1);
        let mid = lo + (hi - lo) / 2;
        let seg = segments[mid];
        if seg.text_off + seg.text_len <= text_off {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn origin_segment_src_subrange(
    seg: crate::source_map::OriginSegment,
    start: usize,
    end: usize,
) -> (usize, usize) {
    if seg.text_len == seg.src_len {
        let rel = start - seg.text_off;
        (seg.src_off + rel, end - start)
    } else {
        (seg.src_off, seg.src_len)
    }
}

fn set_block_span_end(block: &mut Block, new_end: usize) {
    match block {
        Block::Heading { span, .. } | Block::Bullet { span, .. } => {
            if let Some(Span(_, end)) = span {
                *end = new_end;
            }
        }
        _ => {}
    }
}

fn offset_inlines(inline: &mut [Inline], delta: usize) {
    if delta == 0 {
        return;
    }
    for node in inline {
        offset_inline(node, delta);
    }
}

fn offset_inline(node: &mut Inline, delta: usize) {
    match node {
        Inline::Plain { span, span_map, .. } => {
            offset_span(span, delta);
            if let Some(map) = span_map {
                for SpanMapSegment(_, src, _) in map {
                    *src += delta;
                }
            }
        }
        Inline::Emphasis { children, span, .. }
        | Inline::Subscript { children, span }
        | Inline::Superscript { children, span }
        | Inline::Tag { children, span } => {
            offset_inlines(children, delta);
            offset_span(span, delta);
        }
        Inline::Link { label, span, .. } => {
            offset_inlines(label, delta);
            offset_span(span, delta);
        }
        Inline::Code { span, .. }
        | Inline::Verbatim { span, .. }
        | Inline::Break { span }
        | Inline::HardBreak { span }
        | Inline::NestedLink { span, .. }
        | Inline::Target { span, .. }
        | Inline::Macro { span, .. }
        | Inline::ExportSnippet { span, .. }
        | Inline::Latex { span, .. }
        | Inline::Timestamp { span, .. }
        | Inline::Cookie { span, .. }
        | Inline::Fnref { span, .. }
        | Inline::InlineHtml { span, .. }
        | Inline::Email { span, .. }
        | Inline::Entity { span, .. }
        | Inline::Hiccup { span, .. } => offset_span(span, delta),
    }
}

#[cfg(test)]
mod tests {
    use super::try_parse;
    use crate::projection::{Block, Inline, ListItem, Property, Span};

    fn hr(start: usize, end: usize) -> Block {
        Block::Hr {
            span: Some(Span(start, end)),
        }
    }

    fn heading(level: u32, size: Option<u32>, start: usize, end: usize) -> Block {
        Block::Heading {
            level,
            size,
            inline: Vec::new(),
            marker: None,
            priority: None,
            htags: Vec::new(),
            span: Some(Span(start, end)),
        }
    }

    fn bullet(level: u32, start: usize, end: usize) -> Block {
        Block::Bullet {
            level,
            size: None,
            inline: Vec::new(),
            marker: None,
            priority: None,
            htags: Vec::new(),
            span: Some(Span(start, end)),
        }
    }

    fn heading_text(
        level: u32,
        size: Option<u32>,
        text: &str,
        format: &str,
        title_start: usize,
        start: usize,
        end: usize,
    ) -> Block {
        let mut inline = crate::inline(text, format);
        super::offset_inlines(&mut inline, title_start);
        Block::Heading {
            level,
            size,
            inline,
            marker: None,
            priority: None,
            htags: Vec::new(),
            span: Some(Span(start, end)),
        }
    }

    fn bullet_text(
        level: u32,
        text: &str,
        format: &str,
        title_start: usize,
        start: usize,
        end: usize,
    ) -> Block {
        let mut inline = crate::inline(text, format);
        super::offset_inlines(&mut inline, title_start);
        Block::Bullet {
            level,
            size: None,
            inline,
            marker: None,
            priority: None,
            htags: Vec::new(),
            span: Some(Span(start, end)),
        }
    }

    fn comment(text: &str, start: usize, end: usize) -> Block {
        Block::Comment {
            text: text.into(),
            span: Some(Span(start, end)),
        }
    }

    fn directive(name: &str, value: &str) -> Block {
        Block::Directive {
            name: name.into(),
            value: value.into(),
            span: None,
        }
    }

    fn example(code: &str, start: usize, end: usize) -> Block {
        Block::Example {
            code: code.into(),
            span: Some(Span(start, end)),
        }
    }

    fn export_block(
        name: &str,
        options: Option<Vec<&str>>,
        content: &str,
        start: usize,
        end: usize,
    ) -> Block {
        Block::Export {
            name: name.into(),
            options: options.map(|items| items.into_iter().map(str::to_string).collect()),
            content: content.into(),
            span: Some(Span(start, end)),
        }
    }

    fn comment_block(content: &str, start: usize, end: usize) -> Block {
        Block::CommentBlock {
            content: content.into(),
            span: Some(Span(start, end)),
        }
    }

    fn raw_html(text: &str, start: usize, end: usize) -> Block {
        Block::RawHtml {
            text: text.into(),
            span: Some(Span(start, end)),
        }
    }

    fn hiccup(v: &str, start: usize, end: usize) -> Block {
        Block::Hiccup {
            v: v.into(),
            span: Some(Span(start, end)),
        }
    }

    fn quote(children: Vec<Block>, start: usize, end: usize) -> Block {
        Block::Quote {
            children,
            span: Some(Span(start, end)),
        }
    }

    fn quote_plain_zero_break(
        text: &str,
        start: usize,
        end: usize,
        quote_start: usize,
        quote_end: usize,
    ) -> Block {
        Block::Quote {
            children: vec![Block::Paragraph {
                inline: vec![
                    Inline::Plain {
                        text: text.into(),
                        span: Some(Span(start, end)),
                        span_map: None,
                    },
                    Inline::Break {
                        span: Some(Span(end, end)),
                    },
                ],
                span: None,
            }],
            span: Some(Span(quote_start, quote_end)),
        }
    }

    fn custom(name: &str, children: Vec<Block>, start: usize, end: usize) -> Block {
        Block::Custom {
            name: name.into(),
            children,
            span: Some(Span(start, end)),
        }
    }

    fn footnote(
        name: &str,
        body: &str,
        format: &str,
        body_start: usize,
        start: usize,
        end: usize,
    ) -> Block {
        let mut inline = crate::inline(body, format);
        super::offset_inlines(&mut inline, body_start);
        Block::FootnoteDef {
            name: name.into(),
            inline,
            span: Some(Span(start, end)),
        }
    }

    fn paragraph(text: &str, format: &str, start: usize, end: usize) -> Block {
        let mut inline = crate::inline(text, format);
        super::offset_inlines(&mut inline, start);
        Block::Paragraph {
            inline,
            span: Some(Span(start, end)),
        }
    }

    fn paragraph_no_span(text: &str, format: &str, start: usize) -> Block {
        let mut inline = crate::inline(text, format);
        super::offset_inlines(&mut inline, start);
        Block::Paragraph { inline, span: None }
    }

    #[test]
    fn markdown_hr_only_documents_are_owned_by_v2() {
        assert_eq!(
            try_parse("---\n***\r\n___", "md"),
            Some(vec![hr(0, 4), hr(4, 9), hr(9, 12)])
        );
        assert_eq!(try_parse(" \t---\x1a\n", "md"), Some(vec![hr(0, 7)]));
    }

    #[test]
    fn markdown_leaf_documents_include_paragraphs_and_blank_lines() {
        assert_eq!(
            try_parse("hello\n---\nworld", "md"),
            Some(crate::parse("hello\n---\nworld", "md"))
        );
        assert_eq!(
            try_parse("\n---\n", "md"),
            Some(crate::parse("\n---\n", "md"))
        );
        assert_eq!(
            try_parse("---\r", "md"),
            Some(vec![Block::Paragraph {
                inline: crate::inline("---\n", "md"),
                span: Some(Span(0, 4)),
            }])
        );
        assert_eq!(
            try_parse("a\r\nb", "md"),
            Some(crate::parse("a\r\nb", "md"))
        );
    }

    #[test]
    fn markdown_regular_list_leaf_documents_are_owned_by_v2() {
        assert_eq!(
            try_parse("* item\n", "md"),
            Some(crate::parse("* item\n", "md"))
        );
    }

    #[test]
    fn input_start_front_matter_is_owned_by_v2() {
        for format in ["md", "org"] {
            assert_eq!(
                try_parse("---\na: b\n---\nplain", format),
                Some(vec![
                    directive("a", "b"),
                    paragraph("\nplain", format, 12, 18)
                ]),
                "{format}"
            );
            assert_eq!(
                try_parse("---\na\n---\nplain", format),
                Some(vec![paragraph("\nplain", format, 9, 15)]),
                "{format}"
            );
            assert_eq!(
                try_parse("---\n---\n", format),
                Some(vec![paragraph("\n", format, 7, 8)]),
                "{format}"
            );
        }

        assert_eq!(
            try_parse("---\na: b\n---\n> q", "md"),
            Some(crate::parse("---\na: b\n---\n> q", "md")),
            "front matter suffix blockquotes are now owned"
        );
    }

    #[test]
    fn properties_and_drawers_are_owned_by_v2() {
        for (input, format) in [
            ("key:: value\n", "md"),
            ("key::\n", "md"),
            ("a:: 1\nb:: 2\n#+c: 3", "md"),
            (":PROPERTIES:\n:a: 1\n:END:\n", "md"),
            (":PROPERTIES:\n:a: 1\n:END:\nb:: 2\n#+c: 3", "md"),
            (":PROPERTIES:\n:k: v\n:END:\n#+b: 2\n\nplain", "md"),
            (":LOGBOOK:\nCLOCK: x\n:END:", "md"),
            (":PROPERTIES:\n:key: value\n:END:\n", "org"),
            (":PROPERTIES:\n:key: value\n:END:\n\n#+NEXT: ok", "org"),
            (":PROPERTIES:\nplain\n:END:", "org"),
            (":LOGBOOK:\nCLOCK: x\n:END:", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("a::b mid line", "md"),
            Some(vec![paragraph("a::b mid line", "md", 0, 13)])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:tail", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                paragraph("tail", "md", 24, 28),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:#+A: b", "md"),
            Some(vec![Block::Properties {
                props: vec![
                    Property::parse1(("a".into(), "1".into())),
                    Property::parse2(("A".into(), "b".into())),
                ],
                span: Some(Span(0, 30)),
            }])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:key:: value", "md"),
            Some(vec![Block::Properties {
                props: vec![
                    Property::parse1(("a".into(), "1".into())),
                    Property::parse1(("key".into(), "value".into())),
                ],
                span: Some(Span(0, 35)),
            }])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:<div>x</div>", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                raw_html("<div>x</div>", 24, 36),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_SRC\nx\n#+END_SRC", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(24, 47)),
                },
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:[^1]: body", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                footnote("1", "body", "md", 30, 24, 34),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:> quote", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                quote_plain_zero_break("quote", 26, 31, 24, 31),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:$$x$$", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(24, 29)),
                },
            ])
        );
        assert_eq!(
            try_parse(
                ":PROPERTIES:\n:key: value\n:END:#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE",
                "org"
            ),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("key".into(), "value".into()))],
                    span: Some(Span(0, 30)),
                },
                example("x\n", 30, 61),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                custom("note", Vec::new(), 24, 47),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:key: value\n:END:[fn:1] body", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("key".into(), "value".into()))],
                    span: Some(Span(0, 30)),
                },
                footnote("1", "body", "org", 37, 30, 41),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:key: value\n:END:> quote", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("key".into(), "value".into()))],
                    span: Some(Span(0, 30)),
                },
                quote_plain_zero_break("quote", 32, 37, 30, 37),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:key: value\n:END:$$x$$", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("key".into(), "value".into()))],
                    span: Some(Span(0, 30)),
                },
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(30, 35)),
                },
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:tail", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                paragraph("tail", "org", 24, 28),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:#+BEGIN_X: y", "org"),
            Some(vec![Block::Properties {
                props: vec![
                    Property::parse1(("a".into(), "1".into())),
                    Property::parse2(("BEGIN_X".into(), "y".into())),
                ],
                span: Some(Span(0, 36)),
            }])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:k: v\n:END:\n\nplain", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("k".into(), "v".into()))],
                    span: Some(Span(0, 25)),
                },
                paragraph("\nplain", "org", 25, 31),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:---", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                hr(24, 27),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:-----", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                hr(24, 29),
            ])
        );
    }

    #[test]
    fn unclosed_generic_drawers_fall_through() {
        for (input, format) in [(":LOGBOOK:\nCLOCK: x", "md")] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn property_drawer_lone_cr_edges_follow_latest_mldoc() {
        assert_eq!(
            try_parse("key:: v\rtail", "md"),
            Some(vec![paragraph("key:: v\ntail", "md", 0, 12)])
        );
        assert_eq!(
            try_parse("a:: 1\nb:: 2\rtail", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 6)),
                },
                paragraph("b:: 2\ntail", "md", 6, 16),
            ])
        );
        assert_eq!(
            try_parse("a:: 1\n#+b: 2\rtail", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![
                        Property::parse1(("a".into(), "1".into())),
                        Property::parse2(("b".into(), "2".into())),
                    ],
                    span: Some(Span(0, 13)),
                },
                paragraph("tail", "md", 13, 17),
            ])
        );
        assert_eq!(
            try_parse("#+BEGIN_x: no\rtail", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse2(("BEGIN_x".into(), "no".into()))],
                    span: Some(Span(0, 14)),
                },
                paragraph("tail", "md", 14, 18),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:k: v\r:END:", "md"),
            Some(vec![paragraph(":PROPERTIES:\n:k: v\n:END:", "md", 0, 24)])
        );
        assert_eq!(
            try_parse("a:: 1\n:PROPERTIES:\r:k: v\r:END:", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 6)),
                },
                paragraph(":PROPERTIES:\n:k: v\n:END:", "md", 6, 30),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:k: v\r:END:", "org"),
            Some(vec![
                example("PROPERTIES:\nk: v\n", 0, 19),
                example("END:\n", 19, 24),
            ])
        );
        assert_eq!(
            try_parse(":LOGBOOK:\nx\r:END:", "org"),
            Some(vec![
                example("LOGBOOK:\n", 0, 10),
                paragraph("x\n", "org", 10, 12),
                example("END:\n", 12, 17),
            ])
        );
        assert_eq!(
            try_parse(
                ":PROPERTIES:\n:a: 1\n:END:\n:PROPERTIES:\r:k: v\r:END:",
                "org",
            ),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 25)),
                },
                example("PROPERTIES:\n", 25, 38),
                example("k: v\n", 38, 44),
                example("END:\n", 44, 49),
            ])
        );
    }

    #[test]
    fn property_drawer_same_line_close_tails_are_owned_by_v2() {
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:[//]: # c\nnext", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                comment("c", 24, 33),
                paragraph("\nnext", "md", 33, 38),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:key:: v\rtail", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                paragraph("key:: v\ntail", "md", 24, 36),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:: x", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                example("x\n", 24, 27),
            ])
        );
        for (input, format, tail, end) in [
            (
                ":PROPERTIES:\n:a: 1\n:END:<foo>x</foo>",
                "md",
                "<foo>x</foo>",
                36,
            ),
            (":PROPERTIES:\n:a: 1\n:END:<div>x", "org", "<div>x", 30),
            (
                ":PROPERTIES:\n:a: 1\n:END:$$unclosed",
                "md",
                "$$unclosed",
                34,
            ),
            (":PROPERTIES:\n:a: 1\n:END::END:", "md", ":END:", 29),
            (":PROPERTIES:\n:a: 1\n:END:> - x", "org", "> - x", 29),
            (
                ":PROPERTIES:\n:a: 1\n:END:\\begin{}x\\end{}",
                "md",
                r"\begin{}x\end{}",
                39,
            ),
            (
                ":PROPERTIES:\n:a: 1\n:END:#+BEGIN_SRC\nx",
                "md",
                "#+BEGIN_SRC\nx",
                37,
            ),
            (":PROPERTIES:\n:a: 1\n:END:| a | b", "md", "| a | b", 31),
            (":PROPERTIES:\n:a: 1\n:END:|---", "org", "|---", 28),
            (":PROPERTIES:\n:a: 1\n:END:1. ", "md", "1. ", 27),
            (":PROPERTIES:\n:a: 1\n:END:- ", "org", "- ", 26),
            (":PROPERTIES:\n:a: 1\n:END:a::b", "md", "a::b", 28),
            (
                ":PROPERTIES:\n:a: 1\n:END:#+END_NOTE",
                "md",
                "#+END_NOTE",
                34,
            ),
            (":PROPERTIES:\n:a: 1\n:END:#+bad", "org", "#+bad", 29),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(vec![
                    Block::Properties {
                        props: vec![Property::parse1(("a".into(), "1".into()))],
                        span: Some(Span(0, 24)),
                    },
                    paragraph(tail, format, 24, end),
                ]),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:```\nx", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                paragraph("```\nx", "md", 24, 29),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:# h", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                Block::Heading {
                    level: 1,
                    size: Some(1),
                    inline: vec![Inline::Plain {
                        text: "h".into(),
                        span: Some(Span(26, 27)),
                        span_map: None,
                    }],
                    marker: None,
                    priority: None,
                    htags: Vec::new(),
                    span: Some(Span(24, 27)),
                },
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:- x", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                Block::Bullet {
                    level: 1,
                    size: None,
                    inline: vec![Inline::Plain {
                        text: "x".into(),
                        span: Some(Span(26, 27)),
                        span_map: None,
                    }],
                    marker: None,
                    priority: None,
                    htags: Vec::new(),
                    span: Some(Span(24, 27)),
                },
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:# c", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                comment("c", 24, 27),
            ])
        );
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:* h", "org"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                Block::Bullet {
                    level: 1,
                    size: None,
                    inline: vec![Inline::Plain {
                        text: "h".into(),
                        span: Some(Span(26, 27)),
                        span_map: None,
                    }],
                    marker: None,
                    priority: None,
                    htags: Vec::new(),
                    span: Some(Span(24, 27)),
                },
            ])
        );
        for (input, format) in [
            (":PROPERTIES:\n:a: 1\n:END:* x", "md"),
            (":PROPERTIES:\n:a: 1\n:END:- x", "org"),
        ] {
            let blocks = try_parse(input, format).expect(input);
            assert!(
                matches!(
                    blocks.as_slice(),
                    [
                        Block::Properties {
                            span: Some(Span(0, 24)),
                            ..
                        },
                        Block::List {
                            span: Some(Span(24, 27)),
                            ..
                        }
                    ]
                ),
                "{format} {input:?}: {blocks:?}"
            );
        }
        assert_eq!(
            try_parse(":PROPERTIES:\n:a: 1\n:END:# <foo>x</foo>", "md"),
            Some(vec![
                Block::Properties {
                    props: vec![Property::parse1(("a".into(), "1".into()))],
                    span: Some(Span(0, 24)),
                },
                heading_text(1, Some(1), "<foo>x</foo>", "md", 26, 24, 38),
            ])
        );
    }

    #[test]
    fn displayed_math_is_owned_by_v2() {
        for (input, format) in [
            ("$$x$$", "md"),
            ("  $$a\nb$$", "md"),
            ("$$\na\nb\n$$", "md"),
            ("$$ab$$x", "md"),
            ("$$a$$ $$b$$", "md"),
            ("$$a\nb$$\n\nplain", "md"),
            ("$$x$$---", "md"),
            ("$$x$$| a |", "md"),
            ("$$x$$\\begin{eq}a\\end{eq}", "md"),
            ("$$x$$<div>y</div>", "md"),
            ("$$x$$```\ny\n```", "md"),
            ("$$x$$#+BEGIN_SRC\nx\n#+END_SRC", "md"),
            ("$$x$$[^1]: body", "md"),
            ("$$x$$[:div]", "md"),
            ("$$x$$> quote", "md"),
            ("$$x$$key:: value", "md"),
            ("$$x$$:END:", "md"),
            ("$$x$$:PROPERTIES:", "md"),
            ("$$x$$<foo>x</foo>", "md"),
            ("$$x$$```\ny", "md"),
            ("$$x$$> - x", "md"),
            (r"$$x$$\begin{}x\end{}", "md"),
            ("$$x$$#+BEGIN_SRC\ny", "md"),
            ("$$x$$[^1]: b", "md"),
            ("$$x$$| a | b", "md"),
            ("$$x$$|---", "md"),
            ("$$x$$1. ", "md"),
            ("$$x$$+ ", "md"),
            ("$$x$$a::b", "md"),
            ("$$x$$#+END_NOTE", "md"),
            ("$$x$$# <foo>x</foo>", "md"),
            ("$$x$$", "org"),
            ("* \n$$x$$", "org"),
            ("$$x$$-----", "org"),
            ("$$x$$| a |", "org"),
            ("$$x$$\\begin{eq}a\\end{eq}", "org"),
            ("$$x$$<div>y</div>", "org"),
            ("$$x$$#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE", "org"),
            ("$$x$$[fn:1] body", "org"),
            ("$$x$$[:div]", "org"),
            ("$$x$$> quote", "org"),
            ("$$x$$:END:", "org"),
            ("$$x$$:PROPERTIES:\n:END:", "org"),
            ("$$x$$<foo>x</foo>", "org"),
            ("$$x$$~~~\ny", "org"),
            ("$$x$$> - x", "org"),
            ("$$x$$| a | b", "org"),
            ("$$x$$|---", "org"),
            ("$$x$$1. ", "org"),
            ("$$x$$- ", "org"),
            ("$$x$$#+bad", "org"),
            ("$$x$$* <foo>x</foo>", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("$$x$$#+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(0, 5)),
                },
                custom("note", Vec::new(), 5, 28),
            ])
        );
    }

    #[test]
    fn heading_malformed_split_candidates_stay_inline_titles() {
        assert_eq!(
            try_parse("# <foo>x</foo>", "md"),
            Some(vec![heading_text(
                1,
                Some(1),
                "<foo>x</foo>",
                "md",
                2,
                0,
                14
            )])
        );
        assert_eq!(
            try_parse("# $$unclosed", "md"),
            Some(vec![heading_text(1, Some(1), "$$unclosed", "md", 2, 0, 12)])
        );
        assert_eq!(
            try_parse("# ```\nx", "md"),
            Some(vec![
                heading_text(1, Some(1), "```", "md", 2, 0, 6),
                paragraph("x", "md", 6, 7),
            ])
        );
        assert_eq!(
            try_parse("# > - x", "md"),
            Some(vec![heading_text(1, Some(1), "> - x", "md", 2, 0, 7)])
        );
        assert_eq!(
            try_parse("# a::b", "md"),
            Some(vec![heading_text(1, Some(1), "a::b", "md", 2, 0, 6)])
        );
        assert_eq!(
            try_parse("* [^1]: body", "org"),
            Some(crate::parse("* [^1]: body", "org"))
        );
        assert_eq!(
            try_parse("- # [^1]: b", "md"),
            Some(crate::parse("- # [^1]: b", "md"))
        );
        assert_eq!(
            try_parse("# #+END_NOTE", "md"),
            Some(vec![heading_text(1, Some(1), "#+END_NOTE", "md", 2, 0, 12)])
        );
        assert_eq!(
            try_parse(r"# \begin{}x\end{}", "md"),
            Some(vec![heading_text(
                1,
                Some(1),
                r"\begin{}x\end{}",
                "md",
                2,
                0,
                17
            )])
        );
        assert_eq!(
            try_parse("# #+BEGIN_SRC\nx", "md"),
            Some(vec![
                heading_text(1, Some(1), "#+BEGIN_SRC", "md", 2, 0, 14),
                paragraph("x", "md", 14, 15),
            ])
        );
        assert_eq!(
            try_parse("# | a | b", "md"),
            Some(vec![heading_text(1, Some(1), "| a | b", "md", 2, 0, 9)])
        );
        assert_eq!(
            try_parse("# |---", "md"),
            Some(vec![heading_text(1, Some(1), "|---", "md", 2, 0, 6)])
        );
        assert_eq!(
            try_parse("* <foo>x</foo>", "org"),
            Some(vec![bullet_text(1, "<foo>x</foo>", "org", 2, 0, 14)])
        );
        assert_eq!(
            try_parse("* $$unclosed", "org"),
            Some(vec![bullet_text(1, "$$unclosed", "org", 2, 0, 12)])
        );
        assert_eq!(
            try_parse("* | a | b", "org"),
            Some(vec![bullet_text(1, "| a | b", "org", 2, 0, 9)])
        );
        assert_eq!(
            try_parse("* |---", "org"),
            Some(vec![bullet_text(1, "|---", "org", 2, 0, 6)])
        );
    }

    #[test]
    fn lone_cr_heading_lines_follow_latest_mldoc() {
        assert_eq!(
            try_parse("# h\ry", "md"),
            Some(vec![
                heading_text(1, Some(1), "h", "md", 2, 0, 3),
                paragraph("\ny", "md", 3, 5),
            ])
        );
        assert_eq!(
            try_parse("- h\ry", "md"),
            Some(vec![
                bullet_text(1, "h", "md", 2, 0, 3),
                paragraph("\ny", "md", 3, 5),
            ])
        );
        assert_eq!(
            try_parse("* h\ry", "org"),
            Some(vec![
                bullet_text(1, "h", "org", 2, 0, 3),
                paragraph("\ny", "org", 3, 5),
            ])
        );
        assert_eq!(
            try_parse("#\ry", "md"),
            Some(vec![
                heading(1, Some(1), 0, 1),
                paragraph("\ny", "md", 1, 3)
            ])
        );
        assert_eq!(
            try_parse("# \rplain", "md"),
            Some(vec![
                heading(1, Some(1), 0, 1),
                paragraph(" \nplain", "md", 1, 8),
            ])
        );
        assert_eq!(
            try_parse("# \r---", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                paragraph("\n", "md", 2, 3),
                hr(3, 6),
            ])
        );
        assert_eq!(
            try_parse("* \rplain", "org"),
            Some(vec![bullet(1, 0, 1), paragraph(" \nplain", "org", 1, 8)])
        );
        assert_eq!(
            try_parse("* \r-----", "org"),
            Some(vec![
                bullet(1, 0, 2),
                paragraph("\n", "org", 2, 3),
                hr(3, 8)
            ])
        );
        assert_eq!(
            try_parse("# $$x$$\ry", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(2, 8)),
                },
                paragraph("y", "md", 8, 9),
            ])
        );
        assert_eq!(
            try_parse("* $$x$$\ry", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(2, 8)),
                },
                paragraph("y", "org", 8, 9),
            ])
        );
    }

    #[test]
    fn empty_marker_spans_follow_mldoc() {
        assert_eq!(
            try_parse("# \n$$x$$", "md"),
            Some(vec![
                heading(1, Some(1), 0, 3),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(3, 8)),
                },
            ])
        );
        assert_eq!(
            try_parse("# \n---", "md"),
            Some(vec![heading(1, Some(1), 0, 3), hr(3, 6)])
        );
        assert_eq!(
            try_parse("# \n```\nx\n```", "md"),
            Some(vec![
                heading(1, Some(1), 0, 3),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(3, 12)),
                },
            ])
        );
        assert_eq!(
            try_parse("- \n#+BEGIN_QUOTE\n#+END_QUOTE", "md"),
            Some(vec![bullet(1, 0, 3), quote(Vec::new(), 3, 28)])
        );
        assert_eq!(
            try_parse("# #+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                custom("note", Vec::new(), 2, 25),
            ])
        );
        assert_eq!(
            try_parse("- #+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![bullet(1, 0, 2), custom("note", Vec::new(), 2, 25)])
        );
        assert_eq!(
            try_parse("* #+BEGIN_NOTE\n#+END_NOTE", "org"),
            Some(vec![bullet(1, 0, 2), custom("note", Vec::new(), 2, 25)])
        );
        assert_eq!(
            try_parse("## \n\n#+BEGIN_QUOTE\n#+END_QUOTE", "md"),
            Some(vec![
                heading(1, Some(2), 0, 4),
                paragraph("\n", "md", 4, 5),
                quote(Vec::new(), 5, 30),
            ])
        );
        assert_eq!(
            try_parse("## \n\n<div>x</div>", "md"),
            Some(vec![
                heading(1, Some(2), 0, 4),
                paragraph("\n", "md", 4, 5),
                raw_html("<div>x</div>", 5, 17),
            ])
        );
        assert_eq!(
            try_parse("## \n\n<foo>x</foo>", "md"),
            Some(vec![
                heading(1, Some(2), 0, 2),
                paragraph(" \n\n<foo>x</foo>", "md", 2, 17),
            ])
        );
        assert_eq!(
            try_parse("## \n\n<br />", "md"),
            Some(vec![
                heading(1, Some(2), 0, 2),
                paragraph(" \n\n<br />", "md", 2, 11),
            ])
        );
        assert_eq!(
            try_parse("* ", "org"),
            Some(vec![bullet(1, 0, 1), paragraph(" ", "org", 1, 2)])
        );
        assert_eq!(
            try_parse("* \n# c", "org"),
            Some(vec![
                bullet(1, 0, 1),
                paragraph(" \n", "org", 1, 3),
                comment("c", 3, 6),
            ])
        );
    }

    #[test]
    fn unclosed_displayed_math_falls_through_to_paragraph() {
        for (input, format) in [
            ("$$unclosed", "md"),
            ("$$unclosed", "org"),
            ("$$x$$$$unclosed", "md"),
            ("$$x$$$$unclosed", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn raw_html_blocks_are_owned_by_v2() {
        for (input, format) in [
            ("<div>x</div>", "md"),
            ("<div>x</div>tail", "md"),
            ("<div>x</div><span>y</span>", "md"),
            ("<div>\na\n</div>", "md"),
            ("<div><span>a\nb</span></div>", "md"),
            ("<img src=\"x\" />", "md"),
            ("<!DOCTYPE\nhtml>", "md"),
            ("<!-- c\nd -->", "md"),
            ("<b>a\nb</b>", "md"),
            ("<div>x</div>", "org"),
            ("<div>x</div>tail", "org"),
            ("<div>\na\n</div>", "org"),
            ("<img src=\"x\" />", "org"),
            ("<!-- c\nd -->", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("  <div>x</div>", "md"),
            Some(vec![raw_html("<div>x</div>", 0, 14)])
        );
        assert_eq!(
            try_parse("<div>x</div>\n\nplain", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 14),
                paragraph("plain", "md", 14, 19),
            ])
        );
        assert_eq!(
            try_parse("<DIV>x</div>", "md"),
            Some(vec![raw_html("<DIV>x</DIV>", 0, 12)])
        );
        assert_eq!(
            try_parse("<div>x</div>\n: def", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 13),
                paragraph(": def", "md", 13, 18),
            ])
        );
    }

    #[test]
    fn same_line_raw_html_tails_are_owned_by_v2() {
        assert_eq!(
            try_parse("<div>x</div>[//]: # c\nnext", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                comment("c", 12, 21),
                paragraph("\nnext", "md", 21, 26),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div># h", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                heading_text(1, Some(1), "h", "md", 14, 12, 15),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div>- x", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                bullet_text(1, "x", "md", 14, 12, 15),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div># c", "org"),
            Some(vec![raw_html("<div>x</div>", 0, 12), comment("c", 12, 15)])
        );
        assert_eq!(
            try_parse("<div>x</div>* h", "org"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                bullet_text(1, "h", "org", 14, 12, 15),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div>: x", "org"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                example("x\n", 12, 15),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div>#+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                custom("note", Vec::new(), 12, 35),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div>[:div]", "md"),
            Some(crate::parse("<div>x</div>[:div]", "md"))
        );
        assert_eq!(
            try_parse("<div>x</div>[:div]", "org"),
            Some(crate::parse("<div>x</div>[:div]", "org"))
        );
        assert_eq!(
            try_parse("<div>x</div># <foo>x</foo>", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                heading_text(1, Some(1), "<foo>x</foo>", "md", 14, 12, 26),
            ])
        );

        for (input, tail, end) in [
            ("<div>x</div><foo>x</foo>", "<foo>x</foo>", 24),
            ("<div>x</div>$$unclosed", "$$unclosed", 22),
            ("<div>x</div>```\nx", "```\nx", 17),
            ("<div>x</div>> - x", "> - x", 17),
            (r"<div>x</div>\begin{}x\end{}", r"\begin{}x\end{}", 27),
            ("<div>x</div>#+BEGIN_SRC\nx", "#+BEGIN_SRC\nx", 25),
            ("<div>x</div>[^1]: b", "[^1]: b", 19),
            ("<div>x</div>| a | b", "| a | b", 19),
            ("<div>x</div>|---", "|---", 16),
            ("<div>x</div>1. ", "1. ", 15),
            ("<div>x</div>+ ", "+ ", 14),
            ("<div>x</div>:END:", ":END:", 17),
            ("<div>x</div>:PROPERTIES:", ":PROPERTIES:", 24),
            ("<div>x</div>a::b", "a::b", 16),
            ("<div>x</div>#+END_NOTE", "#+END_NOTE", 22),
        ] {
            assert_eq!(
                try_parse(input, "md"),
                Some(vec![
                    raw_html("<div>x</div>", 0, 12),
                    paragraph(tail, "md", 12, end),
                ]),
                "{input:?}"
            );
        }
    }

    #[test]
    fn malformed_raw_html_candidates_fall_through() {
        for (input, format) in [
            ("<unknown>a\nb</unknown>", "md"),
            ("<foo>bar</foo>", "md"),
            ("<br/>", "md"),
            ("<br />", "md"),
            ("<b>ab</b>", "md"),
            ("<?php\na?>", "md"),
            ("<div>a\nb", "md"),
            ("<foo>x</foo>\n: def", "md"),
            ("<unknown>a\nb</unknown>", "org"),
            ("<foo>bar</foo>", "org"),
            ("<br/>", "org"),
            ("<br />", "org"),
            ("<b>ab</b>", "org"),
            ("<?php\na?>", "org"),
            ("<div>a\nb", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn empty_callout_containers_are_owned_by_v2() {
        for format in ["md", "org"] {
            assert_eq!(
                try_parse("#+BEGIN_QUOTE\n#+END_QUOTE", format),
                Some(vec![quote(Vec::new(), 0, 25)]),
                "{format}"
            );
            assert_eq!(
                try_parse("#+BEGIN_NOTE\n#+END_NOTE", format),
                Some(vec![custom("note", Vec::new(), 0, 23)]),
                "{format}"
            );
            assert_eq!(
                try_parse("#+BEGIN_QUOTE\r\n#+END_QUOTE\r\n", format),
                Some(vec![quote(Vec::new(), 0, 28)]),
                "{format}"
            );
            assert_eq!(
                try_parse("  #+begin_TIP arg\n#+end_TIP_EXTRA\n\nplain", format),
                Some(vec![
                    custom("tip", Vec::new(), 0, 34),
                    paragraph("plain", format, 35, 40),
                ]),
                "{format}"
            );
        }
    }

    #[test]
    fn safe_callout_container_bodies_are_owned_by_v2() {
        assert_eq!(
            try_parse("#+BEGIN_QUOTE\nquoted\n#+END_QUOTE", "md"),
            Some(vec![quote(
                vec![paragraph("quoted\n", "md", 14, 21)],
                0,
                32
            )])
        );
        for (input, format) in [
            ("#+BEGIN_QUOTE\nquoted\n#+END_QUOTE", "org"),
            ("#+BEGIN_NOTE\nplain\n#+END_NOTE", "md"),
            ("#+BEGIN_NOTE\nplain\n#+END_NOTE", "org"),
            ("#+BEGIN_NOTE\n  quoted\n  more\n#+END_NOTE", "md"),
            ("#+BEGIN_NOTE\n  quoted\n  more\n#+END_NOTE", "org"),
            ("#+BEGIN_QUOTE\n*bold* [[Page]]\n#+END_QUOTE", "md"),
            ("#+BEGIN_QUOTE\n*bold* [[Page]]\n#+END_QUOTE", "org"),
            ("#+BEGIN_QUOTE\nplain\n#+END_QUOTE\n\ntext", "md"),
            ("#+BEGIN_QUOTE\nplain\n#+END_QUOTE\n\ntext", "org"),
            ("#+BEGIN_FOO\n| a | b |\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n| a | b |\n#+END_FOO", "org"),
            ("#+BEGIN_FOO\n```\ncode\n```\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n```\ncode\n```\n#+END_FOO", "org"),
            ("#+BEGIN_FOO\n---\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n-----\n#+END_FOO", "org"),
            ("#+BEGIN_FOO\n<div>x</div>\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n[:div]\n#+END_FOO", "org"),
            ("#+BEGIN_FOO\nintro\n```\ncode\n```\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n# h\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n[^1]: b\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n- x\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n* x\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\n1. x\n#+END_FOO", "md"),
            ("#+BEGIN_FOO\nk:: v\n#+END_FOO", "md"),
            ("#+BEGIN_NOTE\nk:: v\n#+b: 2\n#+END_NOTE", "md"),
            ("#+BEGIN_NOTE\n:PROPERTIES:\n:k: v\n:END:\n#+END_NOTE", "md"),
            (
                "#+BEGIN_NOTE\n:PROPERTIES:\n:k: v\n:END:\n#+b: 2\n#+END_NOTE",
                "md",
            ),
            (
                "#+BEGIN_NOTE\n:PROPERTIES:\n:k: v\n:END:\n#+END_NOTE",
                "org",
            ),
            ("#+BEGIN_FOO\n:NAME:\nx\n:END:\n#+END_FOO", "md"),
            ("#+BEGIN_QUOTE\n# h\n#+END_QUOTE", "md"),
            ("#+BEGIN_QUOTE\n- x\n#+END_QUOTE", "md"),
            ("#+BEGIN_QUOTE\n+ x\n#+END_QUOTE", "org"),
            ("#+BEGIN_QUOTE\n1. x\n#+END_QUOTE", "org"),
            ("#+BEGIN_QUOTE\n* x\n#+END_QUOTE", "org"),
            ("#+BEGIN_QUOTE\n** y\n#+END_QUOTE", "org"),
            ("#+BEGIN_QUOTE\n[fn:1] body\n#+END_QUOTE", "org"),
            ("#+BEGIN_NOTE\n:NAME:\nx\n:END:\n#+END_NOTE", "org"),
            ("> :LOGBOOK:\n> x\n> :END:", "org"),
            ("#+BEGIN_QUOTE\n>>>>key:: val\n#+END_QUOTE\n", "md"),
            (
                "#+BEGIN_QUOTE\n  text here\n  \\begin{eq}\n  a\n  \\end{eq}\n#+END_QUOTE\n",
                "org",
            ),
            (
                "#+BEGIN_QUOTE\n  \\begin{a}\n  x\n  \\end{a}\n  \\begin{b}\n  y\n  \\end{b}\n#+END_QUOTE\n",
                "org",
            ),
            (
                "- #+BEGIN_QUOTE\n  - #+BEGIN_NOTE\n    nested\n    #+END_NOTE\n  #+END_QUOTE",
                "md",
            ),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn special_callout_containers_use_special_body_parser() {
        assert_eq!(
            try_parse("#+BEGIN_SRC\n#+END_SRC", "md"),
            Some(crate::parse("#+BEGIN_SRC\n#+END_SRC", "md"))
        );
        assert_eq!(
            try_parse("#+BEGIN_EXAMPLE\n#+END_EXAMPLE", "org"),
            Some(crate::parse("#+BEGIN_EXAMPLE\n#+END_EXAMPLE", "org"))
        );
    }

    #[test]
    fn markdown_blockquotes_are_owned_by_v2() {
        for (input, format) in [
            ("> x", "md"),
            ("> x\n", "md"),
            ("> x\ny", "md"),
            ("> > x\n", "md"),
            ("> x\n- y", "md"),
            ("  > x\n", "md"),
            ("> x\n\np", "md"),
            ("> x\n> y\n\np", "md"),
            ("> x", "org"),
            ("> ## > Third", "md"),
            ("> q\n  ## > Third", "md"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }

        for input in ["> - x", "> # h", "> id:: x"] {
            assert_eq!(
                try_parse(input, "md"),
                Some(vec![paragraph(input, "md", 0, input.len())]),
                "{input:?}"
            );
        }

        let blocks = try_parse("> + x", "md").unwrap();
        assert!(matches!(
            blocks.as_slice(),
            [Block::Quote {
                children,
                span: Some(Span(0, 5))
            }] if matches!(children.as_slice(), [Block::List { items, .. }] if items.len() == 1)
        ));

        assert_eq!(
            try_parse("> x\ry", "md"),
            Some(vec![
                quote(vec![paragraph_no_span("x\n", "md", 2)], 0, 4),
                paragraph("y", "md", 4, 5),
            ])
        );
        assert_eq!(
            try_parse("> x\ry", "org"),
            Some(vec![
                quote(vec![paragraph_no_span("x\n", "org", 2)], 0, 4),
                paragraph("y", "org", 4, 5),
            ])
        );

        let blocks = try_parse("> q\n[//]: # c", "md").unwrap();
        let [Block::Quote { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            children.as_slice(),
            [
                Block::Paragraph { inline, .. },
                Block::Comment { text, .. },
                Block::Paragraph { .. },
            ] if matches!(inline.last(), Some(Inline::Break { .. })) && text == "c"
        ));

        let blocks = try_parse("> ## > Third", "md").unwrap();
        assert!(matches!(
            blocks.as_slice(),
            [Block::Quote { children, .. }]
                if matches!(children.as_slice(), [Block::Paragraph { inline, .. }]
                    if inline.iter().any(|node| matches!(node, Inline::Plain { text, .. } if text == "## > Third")))
        ));
    }

    #[test]
    fn block_hiccups_are_owned_by_v2() {
        for (input, format) in [
            ("[:div]", "md"),
            ("  [:div]", "md"),
            ("[:div]x", "md"),
            ("[:div][:span]", "md"),
            ("[:div]  [:span]", "md"),
            ("[:div \"x]\"]tail", "md"),
            ("[:div \n x]", "md"),
            ("[:div]---", "md"),
            ("[:div]key:: value", "md"),
            ("[:div]", "org"),
            ("[:div]-----", "org"),
            ("[:div]:PROPERTIES:\n:END:", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("  [:div]x", "md"),
            Some(vec![hiccup("[:div]", 0, 8), paragraph("x", "md", 8, 9)])
        );
        assert_eq!(
            try_parse("[:div]\n\n", "md"),
            Some(vec![hiccup("[:div]", 0, 8)])
        );
        assert_eq!(
            try_parse("[:div]\n: def", "md"),
            Some(vec![
                hiccup("[:div]", 0, 7),
                paragraph(": def", "md", 7, 12),
            ])
        );
        assert_eq!(
            try_parse("[:div]\n: def", "org"),
            Some(vec![hiccup("[:div]", 0, 7), example("def\n", 7, 12)])
        );
        assert_eq!(
            try_parse("[:div]\n\nplain", "md"),
            Some(vec![
                hiccup("[:div]", 0, 8),
                paragraph("plain", "md", 8, 13),
            ])
        );
        assert_eq!(
            try_parse("[:div]\n\n[:span]", "md"),
            Some(vec![hiccup("[:div]", 0, 8), hiccup("[:span]", 8, 15)])
        );
        assert_eq!(
            try_parse("[:div]\n\nplain", "org"),
            Some(vec![
                hiccup("[:div]", 0, 8),
                paragraph("plain", "org", 8, 13),
            ])
        );
        assert_eq!(
            try_parse("[:div]  ", "md"),
            Some(vec![hiccup("[:div]", 0, 6), paragraph("  ", "md", 6, 8),])
        );
    }

    #[test]
    fn same_line_hiccup_malformed_tails_are_owned_by_v2() {
        for (input, tail, end) in [
            ("[:div]<foo>x</foo>", "<foo>x</foo>", 18),
            ("[:div]$$unclosed", "$$unclosed", 16),
            ("[:div]```\nx", "```\nx", 11),
            ("[:div]> - x", "> - x", 11),
            (r"[:div]\begin{}x\end{}", r"\begin{}x\end{}", 21),
            ("[:div]#+BEGIN_SRC\nx", "#+BEGIN_SRC\nx", 19),
            ("[:div]:END:", ":END:", 11),
            ("[:div]:PROPERTIES:", ":PROPERTIES:", 18),
            ("[:div]a::b", "a::b", 10),
            ("[:div]#+END_NOTE", "#+END_NOTE", 16),
        ] {
            assert_eq!(
                try_parse(input, "md"),
                Some(vec![hiccup("[:div]", 0, 6), paragraph(tail, "md", 6, end)]),
                "{input:?}"
            );
        }
    }

    #[test]
    fn same_line_hiccup_comment_heading_and_fixed_width_tails_are_owned_by_v2() {
        assert_eq!(
            try_parse("[:div][//]: # c\nnext", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                comment("c", 6, 15),
                paragraph("\nnext", "md", 15, 20),
            ])
        );
        assert_eq!(
            try_parse("[:div]# h", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                heading_text(1, Some(1), "h", "md", 8, 6, 9),
            ])
        );
        assert_eq!(
            try_parse("[:div]- x", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                bullet_text(1, "x", "md", 8, 6, 9),
            ])
        );
        assert_eq!(
            try_parse("[:div]# c", "org"),
            Some(vec![hiccup("[:div]", 0, 6), comment("c", 6, 9)])
        );
        assert_eq!(
            try_parse("[:div]* h", "org"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                bullet_text(1, "h", "org", 8, 6, 9),
            ])
        );
        assert_eq!(
            try_parse("[:div]: x", "org"),
            Some(vec![hiccup("[:div]", 0, 6), example("x\n", 6, 9)])
        );
        assert_eq!(
            try_parse("[:div]#+BEGIN_NOTE\n#+END_NOTE", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                custom("note", Vec::new(), 6, 29),
            ])
        );
        assert_eq!(
            try_parse("[:div]| a | b", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                paragraph("| a | b", "md", 6, 13)
            ])
        );
        assert_eq!(
            try_parse("[:div]|---", "md"),
            Some(vec![hiccup("[:div]", 0, 6), paragraph("|---", "md", 6, 10)])
        );
        assert_eq!(
            try_parse("[:div]1. ", "md"),
            Some(vec![hiccup("[:div]", 0, 6), paragraph("1. ", "md", 6, 9)])
        );
        assert_eq!(
            try_parse("[:div]- ", "org"),
            Some(vec![hiccup("[:div]", 0, 6), paragraph("- ", "org", 6, 8)])
        );
        assert_eq!(
            try_parse("[:div]# <foo>x</foo>", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                heading_text(1, Some(1), "<foo>x</foo>", "md", 8, 6, 20),
            ])
        );
    }

    #[test]
    fn malformed_block_hiccups_fall_through() {
        for (input, format) in [
            ("[:nope]", "md"),
            ("[:div ", "md"),
            ("[:div\n x]", "md"),
            ("[:nope]\n: def", "md"),
            ("[:div \n: def", "md"),
            ("[:nope]", "org"),
            ("[:div ", "org"),
            ("[:div\n x]", "org"),
            ("[:nope]\n: def", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn same_line_hiccup_regular_list_tails_are_owned_by_v2() {
        for (input, format) in [
            ("[:div]- x", "org"),
            ("[:div]+ ", "md"),
            ("[:div]1. ", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn same_line_hiccup_special_body_tails_are_owned_by_v2() {
        assert_eq!(
            try_parse("[:div]#+BEGIN_SRC\nx\n#+END_SRC", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(6, 29)),
                },
            ])
        );
        assert_eq!(
            try_parse("[:div]#+BEGIN_EXPORT html\nx\n#+END_EXPORT", "org"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                export_block("html", None, "x\n", 6, 40),
            ])
        );
        assert_eq!(
            try_parse("[:div]```\nx\n```", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(6, 15)),
                },
            ])
        );
        assert_eq!(
            try_parse("[:div]$$x$$", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(6, 11)),
                },
            ])
        );
        assert_eq!(
            try_parse("[:div]$$x$$tail", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(6, 11)),
                },
                paragraph("tail", "md", 11, 15),
            ])
        );
        assert_eq!(
            try_parse("[:div]$$x$$#+BEGIN_SRC\nx\n#+END_SRC", "md"),
            Some(crate::parse("[:div]$$x$$#+BEGIN_SRC\nx\n#+END_SRC", "md"))
        );
        assert_eq!(
            try_parse("[:div][^1]: body", "md"),
            Some(crate::parse("[:div][^1]: body", "md"))
        );
        assert_eq!(
            try_parse("[:div][^1]: b", "md"),
            Some(crate::parse("[:div][^1]: b", "md"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$[^1]: body", "md"),
            Some(crate::parse("[:div]$$x$$[^1]: body", "md"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$[^1]: b", "md"),
            Some(crate::parse("[:div]$$x$$[^1]: b", "md"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$key:: value", "md"),
            Some(crate::parse("[:div]$$x$$key:: value", "md"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$[fn:1] body", "org"),
            Some(crate::parse("[:div]$$x$$[fn:1] body", "org"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$:PROPERTIES:\n:END:", "org"),
            Some(crate::parse("[:div]$$x$$:PROPERTIES:\n:END:", "org"))
        );
        assert_eq!(
            try_parse("[:div]> quote", "md"),
            Some(crate::parse("[:div]> quote", "md"))
        );
        assert_eq!(
            try_parse("[:div]$$x$$> quote", "md"),
            Some(crate::parse("[:div]$$x$$> quote", "md"))
        );
        assert_eq!(
            try_parse("[:div]<kbd>x</kbd>", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                raw_html("<kbd>x</kbd>", 6, 18)
            ])
        );
        assert_eq!(
            try_parse("[:div]<kbd>x</kbd>tail", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                raw_html("<kbd>x</kbd>", 6, 18),
                paragraph("tail", "md", 18, 22),
            ])
        );
        assert_eq!(
            try_parse("[:div]<kbd>x</kbd><span>y</span>", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                raw_html("<kbd>x</kbd>", 6, 18),
                raw_html("<span>y</span>", 18, 32),
            ])
        );
        assert_eq!(
            try_parse(r"[:div]\begin{eq}a\end{eq}", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::LatexEnv {
                    name: "eq".into(),
                    content: "a".into(),
                    span: Some(Span(6, 25)),
                },
            ])
        );
        assert_eq!(
            try_parse(r"[:div]\begin{eq}a\end{eq}tail", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                Block::LatexEnv {
                    name: "eq".into(),
                    content: "a".into(),
                    span: Some(Span(6, 25)),
                },
                paragraph("tail", "md", 25, 29),
            ])
        );
        for (input, format) in [
            ("[:div]#+BEGIN_COMMENT\nx\n#+END_COMMENT", "md"),
            ("[:div]#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE", "org"),
            ("[:div]~~~clj\nx\n```", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn fenced_code_blocks_are_owned_by_v2() {
        for (input, format) in [
            ("```js\nx\n```", "md"),
            ("```\nx\n```\n\nplain", "md"),
            ("```\nx\n```tail", "md"),
            ("````js\nx\n~~~", "md"),
            ("  ``` clj opts\nx\n```", "md"),
            ("```js\nx\n```", "org"),
            ("* \n```\nx\n```", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn unclosed_fenced_code_falls_through_to_paragraph() {
        for (input, format) in [("```\nx", "md"), ("~~~\nx", "org")] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn fenced_code_lone_cr_and_crlf_follow_latest_mldoc() {
        assert_eq!(
            try_parse("```\r\nx\r\n```", "md"),
            Some(vec![Block::Src {
                lang: String::new(),
                code: "x\n".into(),
                span: Some(Span(0, 11)),
            }])
        );
        assert_eq!(
            try_parse("```\rx\r```", "md"),
            Some(vec![paragraph("```\nx\n```", "md", 0, 9)])
        );
        assert_eq!(
            try_parse("# h\r```\rx\r```", "md"),
            Some(vec![
                heading_text(1, Some(1), "h", "md", 2, 0, 3),
                paragraph("\n```\nx\n```", "md", 3, 13),
            ])
        );
        assert_eq!(
            try_parse("$$x$$```\ry\r```", "md"),
            Some(vec![
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(0, 5)),
                },
                paragraph("```\ny\n```", "md", 5, 14),
            ])
        );
        assert_eq!(
            try_parse("$$x$$```\r\ny\r\n```", "md"),
            Some(vec![
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(0, 5)),
                },
                Block::Src {
                    lang: String::new(),
                    code: "y\n".into(),
                    span: Some(Span(5, 16)),
                },
            ])
        );
    }

    #[test]
    fn src_and_example_blocks_are_owned_by_v2() {
        assert_eq!(
            try_parse("#+BEGIN_EXPORT html opt\n  x\n#+END_EXPORT", "md"),
            Some(vec![export_block("html", Some(vec!["opt"]), "x\n", 0, 40)])
        );
        assert_eq!(
            try_parse("#+BEGIN_COMMENT\n  x\n#+END_COMMENT", "org"),
            Some(vec![comment_block("x\n", 0, 33)])
        );
        for (input, format) in [
            ("#+BEGIN_SRC clojure\n  (x)\n#+END_SRC", "md"),
            ("#+begin_src js\nx\n#+end_src", "md"),
            ("#+BEGIN_SRC\nx\n#+END_SRC_EXTRA", "md"),
            ("#+BEGIN_EXAMPLE\n  x\n#+END_EXAMPLE", "md"),
            ("#+BEGIN_EXPORT html opt\n  x\n#+END_EXPORT", "md"),
            ("#+BEGIN_COMMENT\n  x\n#+END_COMMENT", "md"),
            ("#+BEGIN_SRC\nx\n#+END_SRC\n\nplain", "md"),
            ("#+BEGIN_SRC clojure\n  (x)\n#+END_SRC", "org"),
            ("#+BEGIN_EXPORT html opt\n  x\n#+END_EXPORT", "org"),
            ("#+BEGIN_COMMENT\n  x\n#+END_COMMENT", "org"),
            ("* \n#+BEGIN_SRC\nx\n#+END_SRC", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn unclosed_or_malformed_begin_blocks_fall_through_to_paragraph() {
        for (input, format) in [
            ("#+BEGIN_SRC\nx", "md"),
            ("#+BEGIN_EXAMPLE\nx", "org"),
            ("#+BEGIN_ \nx\n#+END_", "md"),
            ("#+BEGIN_NOTE\nx", "md"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn lone_cr_begin_blocks_follow_latest_mldoc_paragraph_fallthrough() {
        for format in ["md", "org"] {
            assert_eq!(
                try_parse("#+BEGIN_SRC\rx\r#+END_SRC", format),
                Some(vec![paragraph("#+BEGIN_SRC\nx\n#+END_SRC", format, 0, 23)]),
                "{format} src"
            );
            assert_eq!(
                try_parse("#+BEGIN_NOTE\rx\r#+END_NOTE", format),
                Some(vec![paragraph(
                    "#+BEGIN_NOTE\nx\n#+END_NOTE",
                    format,
                    0,
                    25
                )]),
                "{format} note"
            );
        }

        assert_eq!(
            try_parse("# h\r#+BEGIN_SRC\rx\r#+END_SRC", "md"),
            Some(vec![
                heading_text(1, Some(1), "h", "md", 2, 0, 3),
                paragraph("\n#+BEGIN_SRC\nx\n#+END_SRC", "md", 3, 27),
            ])
        );
        assert_eq!(
            try_parse("$$x$$#+BEGIN_SRC\ry\r#+END_SRC", "md"),
            Some(vec![
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(0, 5)),
                },
                paragraph("#+BEGIN_SRC\ny\n#+END_SRC", "md", 5, 28),
            ])
        );
        assert_eq!(
            try_parse("<div>x</div>#+BEGIN_NOTE\ry\r#+END_NOTE", "md"),
            Some(vec![
                raw_html("<div>x</div>", 0, 12),
                paragraph("#+BEGIN_NOTE\ny\n#+END_NOTE", "md", 12, 37),
            ])
        );
        assert_eq!(
            try_parse("[:div]#+BEGIN_SRC\ry\r#+END_SRC", "md"),
            Some(vec![
                hiccup("[:div]", 0, 6),
                paragraph("#+BEGIN_SRC\ny\n#+END_SRC", "md", 6, 29),
            ])
        );
        assert_eq!(
            try_parse("# #+BEGIN_SRC\ry\r#+END_SRC", "md"),
            Some(vec![
                heading_text(1, Some(1), "#+BEGIN_SRC", "md", 2, 0, 13),
                paragraph("\ny\n#+END_SRC", "md", 13, 25),
            ])
        );
    }

    #[test]
    fn org_fixed_width_examples_are_owned_by_v2() {
        for input in [
            ": text",
            "  : indented",
            ": line1\n: line2\nplain",
            ":PROPERTIES:",
            ":LOGBOOK:",
            ":LOGBOOK:\nCLOCK: x\n:END:",
            ": text\n:NAME:\ncontent\n:END:",
            "* \n: x",
        ] {
            assert_eq!(
                try_parse(input, "org"),
                Some(crate::parse(input, "org")),
                "{input:?}"
            );
        }

        assert_eq!(
            try_parse(": text", "md"),
            Some(crate::parse(": text", "md"))
        );
        assert_eq!(try_parse(":    x", "org"), Some(vec![example("x\n", 0, 6)]));
        assert_eq!(
            try_parse(": a b  ", "org"),
            Some(vec![example("a b  \n", 0, 7)])
        );
        assert_eq!(
            try_parse(": a\r: b", "org"),
            Some(vec![example("a\n", 0, 4), example("b\n", 4, 7)])
        );
        assert_eq!(
            try_parse(": a\r\n: b", "org"),
            Some(vec![example("a\nb\n", 0, 8)])
        );
        assert_eq!(
            try_parse(": a\n\n:b", "org"),
            Some(vec![example("a\n", 0, 5), example("b\n", 5, 7)])
        );
        assert_eq!(
            try_parse(": a\n\nplain", "org"),
            Some(vec![example("a\n", 0, 5), paragraph("plain", "org", 5, 10)])
        );
        assert_eq!(
            try_parse("#+TITLE\n: Project/Recap\n\n", "org"),
            Some(vec![
                paragraph("#+TITLE\n", "org", 0, 8),
                example("Project/Recap\n", 8, 25),
            ])
        );
        assert_eq!(
            try_parse("* \n: x", "org"),
            Some(vec![bullet(1, 0, 3), example("x\n", 3, 6)])
        );
    }

    #[test]
    fn footnote_definitions_are_owned_by_v2() {
        for (input, format) in [
            ("[^1]: body", "md"),
            (" [^1]: body", "md"),
            ("[^1]: body\ncont", "md"),
            ("[^1]: ab\n[^2]: cd", "md"),
            ("[fn:1] body", "org"),
            (" [fn:1] body", "org"),
            ("[fn:1] body\ncont", "org"),
            ("[fn:1] ab\n[fn:2] cd", "org"),
            ("[fn:1] body\n#+TITLE: x", "org"),
            ("[fn:1] body\n-----", "org"),
            ("[fn:1] body\n  - x", "org"),
            ("[^1]: body\n---", "md"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("[^1]: body\n\ncont", "md"),
            Some(vec![
                footnote("1", "body", "md", 6, 0, 12),
                paragraph("cont", "md", 12, 16),
            ])
        );
        assert_eq!(
            try_parse("[fn:1] body\n\ncont", "org"),
            Some(vec![
                footnote("1", "body", "org", 7, 0, 13),
                paragraph("cont", "org", 13, 17),
            ])
        );
        assert_eq!(
            try_parse("# \n[^1]: ab", "md"),
            Some(vec![
                heading(1, Some(1), 0, 1),
                paragraph(" \n", "md", 1, 3),
                footnote("1", "ab", "md", 9, 3, 11),
            ])
        );
        assert_eq!(
            try_parse("* \n[fn:1] ab", "org"),
            Some(vec![
                bullet(1, 0, 1),
                paragraph(" \n", "org", 1, 3),
                footnote("1", "ab", "org", 10, 3, 12),
            ])
        );
        assert_eq!(
            try_parse("[^1]: body\ncont\rmore", "md"),
            Some(vec![
                footnote("1", "body", "md", 6, 0, 11),
                paragraph("cont\nmore", "md", 11, 20),
            ])
        );
        assert_eq!(
            try_parse("[fn:1] body\ncont\rmore", "org"),
            Some(vec![
                footnote("1", "body", "org", 7, 0, 12),
                paragraph("cont\nmore", "org", 12, 21),
            ])
        );
    }

    #[test]
    fn malformed_footnote_definition_starts_are_paragraphs() {
        for (input, format) in [
            ("[^1]: a", "md"),
            ("[^1]:-x", "md"),
            ("[^a b]: cd", "md"),
            ("[fn:1] a", "org"),
            ("[fn:1]-x", "org"),
            ("[fn:1][x", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("[^1]: body\rcont", "md"),
            Some(vec![paragraph("[^1]: body\ncont", "md", 0, 15)])
        );
        assert_eq!(
            try_parse("[fn:1] body\rcont", "org"),
            Some(vec![paragraph("[fn:1] body\ncont", "org", 0, 16)])
        );
    }

    #[test]
    fn suppressed_footnote_like_lines_stop_before_fences_in_block_content() {
        let blocks = try_parse("> [^1]: body\n> ```\n> x\n> ```", "md").unwrap();
        let [Block::Quote { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            children.as_slice(),
            [
                Block::Paragraph { .. },
                Block::Src { lang, code, .. },
            ] if lang.is_empty() && code == "x\n"
        ));

        let blocks = try_parse("+ [^1]: body\n  ```\n  x\n  ```", "md").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { content, .. }]
                if matches!(
                    content.as_slice(),
                    [
                        Block::Paragraph { .. },
                        Block::Src { lang, code, .. },
                    ] if lang.is_empty() && code == "x\n"
                )
        ));

        let blocks = try_parse("> [fn:1] body\n> ```\n> x\n> ```", "org").unwrap();
        let [Block::Quote { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            children.as_slice(),
            [
                Block::Paragraph { .. },
                Block::Src { lang, code, .. },
            ] if lang.is_empty() && code == "x\n"
        ));
    }

    #[test]
    fn regular_lists_with_paragraph_content_are_owned_by_v2() {
        for (input, format) in [
            ("* a\n* b", "md"),
            ("+ [x] done\n+ [ ] todo", "md"),
            ("1. one\n2. two", "md"),
            ("* a\n  cont\n* b", "md"),
            ("* a\n  * b\n  * c", "md"),
            ("* term ::", "md"),
            ("- a\n- b", "org"),
            ("- a\n  + b\n  + c", "org"),
            ("- a\n  * b", "org"),
            ("1. one\n2. two", "org"),
            ("* a\n: def", "md"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn regular_lists_with_v2_block_content_are_owned_by_v2() {
        for (input, format) in [
            ("* a\n  # h", "md"),
            ("* a\n  [^1]: body", "md"),
            ("* a\n  key:: value", "md"),
            ("* a\n  :PROPERTIES:\n  :k: v\n  :END:", "md"),
            ("* a\n  ---", "md"),
            ("* | a | b |\n  | c | d |", "md"),
            ("* ```\n  x\n  ```", "md"),
            ("* <div>x</div>", "md"),
            ("* $$x$$", "md"),
            ("* \\begin{eq}x\\end{eq}", "md"),
            ("* #+A: b", "md"),
            ("* a\n  #+A: b", "md"),
            ("* term\n  : def", "md"),
            ("* a\n  term\n  : def", "md"),
            ("- * x", "org"),
            ("- a\n  [fn:1] body", "org"),
            ("- a\n  :PROPERTIES:\n  :k: v\n  :END:", "org"),
            ("- a\n  :LOGBOOK:\n  x\n  :END:", "org"),
            ("- a\n  -----", "org"),
            ("- | a | b |\n  | c | d |", "org"),
            ("- a\n  * h\n  :PROPERTIES:", "org"),
            ("- #+A: b", "org"),
            ("- a\n  #+A: b", "org"),
            ("- - x", "org"),
            ("- * x", "org"),
            ("1. - x", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn regular_list_results_content_follows_latest_mldoc() {
        let blocks = try_parse("+ #+RESULTS:", "md").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem {
                ordered: false,
                indent: 0,
                content,
                items,
                ..
            }] if items.is_empty()
                && matches!(content.as_slice(), [Block::Results { .. }])
        ));

        let blocks = try_parse("- #+RESULTS:", "org").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem {
                ordered: false,
                indent: 0,
                content,
                ..
            }] if matches!(content.as_slice(), [Block::Results { .. }])
        ));

        let blocks = try_parse("+ #+RESULTS:x", "md").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items[0].content.as_slice(),
            [
                Block::Results { .. },
                Block::Paragraph { inline, .. },
            ] if matches!(inline.as_slice(), [Inline::Plain { text, .. }] if text == "x")
        ));

        let blocks = try_parse("+ #+RESULTS:\n  next", "md").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items[0].content.as_slice(),
            [
                Block::Results { .. },
                Block::Paragraph { inline, .. },
            ] if matches!(
                inline.as_slice(),
                [
                    Inline::Break { .. },
                    Inline::Plain { text, .. },
                ] if text == "next"
            )
        ));

        let blocks = try_parse("+ parent\n  + #+RESULTS:", "md").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { items, .. }]
                if matches!(
                    items.as_slice(),
                    [ListItem { content, .. }]
                        if matches!(content.as_slice(), [Block::Results { .. }])
                )
        ));

        for (input, format) in [
            ("> q\n+ #+RESULTS:", "md"),
            ("#+BEGIN_QUOTE\n+ #+RESULTS:\n#+END_QUOTE", "md"),
            ("> q\n+ #+RESULTS:", "org"),
        ] {
            let blocks = try_parse(input, format).unwrap();
            let [Block::Quote { children, .. }] = blocks.as_slice() else {
                panic!("{format} {input:?}: {blocks:?}");
            };
            let list = children
                .iter()
                .find_map(|block| match block {
                    Block::List { items, .. } => Some(items),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{format} {input:?}: {children:?}"));
            assert!(matches!(
                list.as_slice(),
                [ListItem { content, .. }]
                    if matches!(
                        content.as_slice(),
                        [Block::Directive { name, value, .. }]
                            if name == "RESULTS" && value.is_empty()
                    )
            ));
        }

        assert_eq!(
            try_parse("+ #+results:", "md"),
            Some(crate::parse("+ #+results:", "md"))
        );
    }

    #[test]
    fn regular_list_quote_children_preserve_block_content_directives() {
        for (input, format, expected_name, expected_value) in [
            ("+ > q\n  #+A: b", "md", "A", "b"),
            ("+ > q\n  #+RESULTS:", "md", "RESULTS", ""),
            ("+ > q\n  #+A: b", "org", "A", "b"),
            ("+ > q\n  #+RESULTS:", "org", "RESULTS", ""),
        ] {
            let blocks = try_parse(input, format).unwrap();
            let [Block::List { items, .. }] = blocks.as_slice() else {
                panic!("{format} {input:?}: {blocks:?}");
            };
            assert!(matches!(
                items.as_slice(),
                [ListItem { content, .. }]
                    if matches!(
                        content.as_slice(),
                        [Block::Quote { children, .. }]
                            if matches!(
                                children.as_slice(),
                                [
                                    Block::Paragraph { .. },
                                    Block::Directive { name, value, .. },
                                ] if name == expected_name && value == expected_value
                            )
                    )
            ));
        }
    }

    #[test]
    fn begin_parse2_lines_in_block_content_are_paragraphs() {
        for (input, format) in [
            ("> #+BEGIN_X: y", "md"),
            ("#+BEGIN_NOTE\n#+BEGIN_X: y\n#+END_NOTE", "md"),
            ("> #+BEGIN_X: y", "org"),
            ("#+BEGIN_NOTE\n#+BEGIN_X: y\n#+END_NOTE", "org"),
        ] {
            let blocks = try_parse(input, format).unwrap();
            let children = match blocks.as_slice() {
                [Block::Quote { children, .. }] | [Block::Custom { children, .. }] => children,
                _ => panic!("{format} {input:?}: {blocks:?}"),
            };
            assert!(
                matches!(children.as_slice(), [Block::Paragraph { .. }]),
                "{format} {input:?}: {children:?}"
            );
        }
    }

    #[test]
    fn org_property_drawer_suppression_preserves_following_directives_by_context() {
        let example_then_directive = |children: &[Block], name: &str| {
            assert!(
                matches!(
                    children,
                    [
                        Block::Example { code, .. },
                        Block::Directive { name: n, value, .. },
                    ] if code == "PROPERTIES:\nk: v\nEND:\n" && n == name && (value == "b" || value.is_empty())
                ),
                "{children:?}"
            );
        };

        let blocks = try_parse("> :PROPERTIES:\n> :k: v\n> :END:\n> #+A: b", "org").unwrap();
        let [Block::Quote { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        example_then_directive(children, "A");

        let blocks = try_parse(
            "#+BEGIN_NOTE\n:PROPERTIES:\n:k: v\n:END:\n#+RESULTS:\n#+END_NOTE",
            "org",
        )
        .unwrap();
        let [Block::Custom { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        example_then_directive(children, "RESULTS");

        let blocks = try_parse("+ :PROPERTIES:\n  :k: v\n  :END:\n  #+A: b", "org").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { content, .. }]
                if matches!(
                    content.as_slice(),
                    [
                        Block::Example { code, .. },
                        Block::Paragraph { .. },
                    ] if code == "PROPERTIES:\nk: v\nEND:\n"
                )
        ));

        let blocks = try_parse("+ :PROPERTIES:\n  :k: v\n  :END:\n  #+RESULTS:", "org").unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { content, .. }]
                if matches!(
                    content.as_slice(),
                    [
                        Block::Example { code, .. },
                        Block::Results { .. },
                    ] if code == "PROPERTIES:\nk: v\nEND:\n"
                )
        ));

        let blocks = try_parse(
            "#+BEGIN_NOTE\n+ :PROPERTIES:\n  :k: v\n  :END:\n  #+A: b\n#+END_NOTE",
            "org",
        )
        .unwrap();
        let [Block::Custom { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        let [Block::List { items, .. }] = children.as_slice() else {
            panic!("{children:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { content, .. }]
                if matches!(
                    content.as_slice(),
                    [
                        Block::Example { code, .. },
                        Block::Directive { name, value, .. },
                    ] if code == "PROPERTIES:\nk: v\nEND:\n" && name == "A" && value == "b"
                )
        ));
    }

    #[test]
    fn org_suppressed_drawer_examples_coalesce_like_fixed_width() {
        let blocks = try_parse(
            "> :PROPERTIES:\n> :k: v\n> :END:\n> :LOGBOOK:\n> x\n> :END:",
            "org",
        )
        .unwrap();
        let [Block::Quote { children, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            children.as_slice(),
            [
                Block::Example { code: first, .. },
                Block::Paragraph { .. },
                Block::Example { code: last, .. },
            ] if first == "PROPERTIES:\nk: v\nEND:\nLOGBOOK:\n" && last == "END:\n"
        ));

        let blocks = try_parse(
            "+ :LOGBOOK:\n  x\n  :END:\n  :PROPERTIES:\n  :k: v\n  :END:",
            "org",
        )
        .unwrap();
        let [Block::List { items, .. }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(matches!(
            items.as_slice(),
            [ListItem { content, .. }]
                if matches!(
                    content.as_slice(),
                    [
                        Block::Example { code: first, .. },
                        Block::Paragraph { .. },
                        Block::Example { code: last, .. },
                    ] if first == "LOGBOOK:\n" && last == "END:\nPROPERTIES:\nk: v\nEND:\n"
                )
        ));
    }

    #[test]
    fn regular_list_edges_follow_latest_mldoc() {
        assert_eq!(
            try_parse("* a\rb", "md"),
            Some(vec![paragraph("* a\nb", "md", 0, 5)])
        );
        assert_eq!(
            try_parse("- a\rb", "org"),
            Some(vec![paragraph("- a\nb", "org", 0, 5)])
        );
        assert_eq!(
            try_parse("1. ", "md"),
            Some(vec![paragraph("1. ", "md", 0, 3)])
        );
    }

    #[test]
    fn regular_list_loose_child_item_failures_fall_through_to_paragraph() {
        assert_eq!(
            try_parse("* a\n  12bad", "md"),
            Some(crate::parse("* a\n  12bad", "md"))
        );
        assert_eq!(
            try_parse("- a\n  12bad", "org"),
            Some(crate::parse("- a\n  12bad", "org"))
        );
        assert_eq!(
            try_parse("* a\n* b\n  5x", "md"),
            Some(crate::parse("* a\n* b\n  5x", "md"))
        );
        for input in [
            "- a\n  - nested",
            "+ a\n  + b\n    - c",
            "- a\n- b\n  - z",
            "+ a\n  + b\n  + c\n    - d",
            "+ p\n+ a\n  + b\n    - c",
        ] {
            assert_eq!(
                try_parse(input, "org"),
                Some(crate::parse(input, "org")),
                "{input:?}"
            );
        }
    }

    #[test]
    fn markdown_definition_lists_are_owned_by_v2() {
        for input in [
            "term\n: definition",
            "term\n: one\n: two",
            "term\n: one\ncontinued",
            "term\n: one\n\nnext\n: two",
            "---\n: definition",
            ": term\n: definition",
            "| a | b\n: definition",
            "term\n: body\n---",
            "term\n: body\n# h\n: two",
            "term\n: body\n* h\n: two",
            "intro\nterm\n: definition",
        ] {
            assert_eq!(
                try_parse(input, "md"),
                Some(crate::parse(input, "md")),
                "{input:?}"
            );
        }
        assert_eq!(
            try_parse("term\n: a", "md"),
            Some(vec![paragraph("term\n: a", "md", 0, 8)])
        );
        assert_eq!(
            try_parse("term\r: definition", "md"),
            Some(vec![paragraph("term\n: definition", "md", 0, 17)])
        );
        assert_eq!(
            try_parse("term\n: body\rnext", "md"),
            Some(vec![paragraph("term\n: body\nnext", "md", 0, 16)])
        );
        assert_eq!(
            try_parse("term\n:nospace", "md"),
            Some(vec![paragraph("term\n:nospace", "md", 0, 13)])
        );
        assert_eq!(
            try_parse("* a\n: def", "md"),
            Some(crate::parse("* a\n: def", "md")),
            "regular Markdown lists now win before Markdown definition lists"
        );
        assert_eq!(
            try_parse("[:div]\n: def", "md"),
            Some(vec![
                hiccup("[:div]", 0, 7),
                paragraph(": def", "md", 7, 12),
            ]),
            "valid block hiccup now wins before Markdown definition lists"
        );
        assert_eq!(
            try_parse("[:nope]\n: def", "md"),
            Some(crate::parse("[:nope]\n: def", "md")),
            "invalid hiccup heads still fall through to Markdown definition lists"
        );
        assert_eq!(
            try_parse("term\n: definition", "org"),
            Some(crate::parse("term\n: definition", "org"))
        );
    }

    #[test]
    fn tables_are_owned_by_v2() {
        for (input, format) in [
            ("| a | b |", "md"),
            ("| a | b |\n| c | d |", "md"),
            ("|---\n| a |", "md"),
            ("| a | b |\n|---|---|", "md"),
            ("| a | b |\n|---|---|\n| 1 | 2 |", "md"),
            ("| a | b |\n|---+---|\n| 1 | 2 |", "org"),
            ("* \n| a |", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn table_edges_follow_mldoc_source() {
        let blocks = try_parse("| a |\n#+TBLFM: x\nplain", "org").unwrap();
        assert!(matches!(
            blocks.as_slice(),
            [
                Block::Table {
                    span: Some(Span(0, 16)),
                    ..
                },
                Block::Paragraph {
                    span: Some(Span(16, 22)),
                    ..
                }
            ]
        ));

        let blocks = try_parse("| h | h |\n|---+---|\n| / | > |\n| a | b |", "org").unwrap();
        match &blocks[0] {
            Block::Table { rows, span, .. } => {
                assert_eq!(*span, Some(Span(0, 39)));
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }

        let blocks = try_parse("| a |\rplain", "md").unwrap();
        assert!(matches!(
            blocks.as_slice(),
            [
                Block::Table {
                    span: Some(Span(0, 5)),
                    ..
                },
                Block::Paragraph {
                    span: Some(Span(5, 11)),
                    ..
                }
            ]
        ));

        assert_eq!(
            try_parse("# \n| a |", "md"),
            Some(vec![
                heading(1, Some(1), 0, 3),
                Block::Table {
                    header: Some(vec![{
                        let mut inline = crate::inline("a", "md");
                        super::offset_inlines(&mut inline, 5);
                        inline
                    }]),
                    rows: Vec::new(),
                    aligns: Vec::new(),
                    span: Some(Span(3, 8)),
                },
            ])
        );
    }

    #[test]
    fn malformed_table_candidates_fall_through_to_paragraph() {
        for (input, format) in [("| a | b", "md"), ("|---", "md")] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn latex_environments_are_owned_by_v2() {
        for (input, format) in [
            (
                r"\begin{equation}
x=1
y=2
\end{equation}",
                "md",
            ),
            (r"  \begin{eq}a\end{eq}", "md"),
            (r"\begin{eq}a\end{eq}tail", "md"),
            ("\\begin{eq}a\\end{eq}\nplain", "md"),
            ("\\begin{eq}a\\end{eq}\n---", "md"),
            (r"\begin{eq}a\end{eq}\begin{b}c\end{b}", "md"),
            ("\\begin{eq}\nx=1", "md"),
            ("\\begin{eq}   \nx\\end{eq}", "md"),
            (r"\begin{Eq}x\END{eq}", "md"),
            (r"\begin{eq}a\end{eq}tail", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn latex_environment_name_follows_mldoc_source() {
        assert_eq!(
            try_parse("\\begin{eq\n}x\\end{eq\n}", "md"),
            Some(vec![Block::LatexEnv {
                name: "eq\n".into(),
                content: "x".into(),
                span: Some(Span(0, 21)),
            }])
        );
    }

    #[test]
    fn malformed_latex_environment_openers_fall_through_to_paragraph() {
        for (input, format) in [(r"\begin{}x\end{}", "md"), ("\\begin{eq\nx", "md")] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn top_level_headings_and_bullets_are_owned_by_v2() {
        for (input, format) in [
            ("# heading\n", "md"),
            ("  ## indented", "md"),
            ("#", "md"),
            ("# ", "md"),
            ("#\t:", "md"),
            ("# :END:", "md"),
            ("# :PROPERTIES:", "md"),
            ("# TODO ", "md"),
            ("- bullet", "md"),
            ("- ## Section", "md"),
            ("- TODO [#A] done", "md"),
            ("text\n# h\nnext", "md"),
            ("# \nkey:: value", "md"),
            ("# \n:LOGBOOK:\n:END:", "md"),
            ("# [:div]", "md"),
            ("# [:div]tail", "md"),
            ("# [:div][:span]", "md"),
            ("# $$x$$#+BEGIN_SRC\nx\n#+END_SRC", "md"),
            ("# [^1]: body", "md"),
            ("# $$x$$[^1]: body", "md"),
            ("# > quote", "md"),
            ("# $$x$$> quote", "md"),
            ("# key:: value", "md"),
            ("# key:: value\nother:: 1", "md"),
            ("# $$x$$key:: value", "md"),
            ("# #+TITLE: x", "md"),
            ("* Heading", "org"),
            ("** TODO [#A] title :tag:", "org"),
            ("*", "org"),
            ("text\n* h\nnext", "org"),
            ("* \n:LOGBOOK:\n:END:", "org"),
            ("* [:div]", "org"),
            ("* $$x$$#+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE", "org"),
            ("* [fn:1] body", "org"),
            ("* $$x$$[fn:1] body", "org"),
            ("* > quote", "org"),
            ("* $$x$$> quote", "org"),
            ("* :PROPERTIES:\n:END:", "org"),
            ("* :PROPERTIES:\n:k: v\n:END:", "org"),
            ("* $$x$$:PROPERTIES:\n:END:", "org"),
            ("* #+TITLE: x", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn heading_title_directive_property_and_drawer_splits_are_owned_by_v2() {
        for (input, format) in [
            ("# key:: value", "md"),
            ("# key:: value\nother:: 1", "md"),
            ("# $$x$$key:: value", "md"),
            ("# #+TITLE: x", "md"),
            ("* :PROPERTIES:\n:END:", "org"),
            ("* :PROPERTIES:\n:k: v\n:END:", "org"),
            ("* $$x$$:PROPERTIES:\n:END:", "org"),
            ("* #+TITLE: x", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn heading_marker_tail_and_title_split_regressions() {
        assert_eq!(
            try_parse("- ", "md"),
            Some(vec![bullet(1, 0, 1), paragraph(" ", "md", 1, 2)])
        );
        assert_eq!(
            try_parse("- \nnext", "md"),
            Some(vec![bullet(1, 0, 1), paragraph(" \nnext", "md", 1, 7)])
        );
        assert_eq!(
            try_parse("*    \n#+END_EXAMPLE\n", "org"),
            Some(vec![
                bullet(1, 0, 1),
                paragraph("    \n#+END_EXAMPLE\n", "org", 1, 20)
            ])
        );
        assert_eq!(
            try_parse("* TODO [#A][[Page]] high-priority task\n", "org"),
            Some(vec![
                Block::Bullet {
                    level: 1,
                    size: None,
                    inline: Vec::new(),
                    marker: Some("TODO".into()),
                    priority: Some("A".into()),
                    htags: Vec::new(),
                    span: Some(Span(0, 11)),
                },
                paragraph("[[Page]] high-priority task\n", "org", 11, 39),
            ])
        );
    }

    #[test]
    fn heading_title_directives_with_spaces_stay_inline() {
        assert_eq!(
            try_parse(
                "* #+END_QUOTE[2026-06-20 Sat]SCHEDULED: x  #+BEGIN_A\n",
                "org"
            ),
            Some(vec![bullet_text(
                1,
                "#+END_QUOTE[2026-06-20 Sat]SCHEDULED: x  #+BEGIN_A",
                "org",
                2,
                0,
                53
            )])
        );
        assert_eq!(
            try_parse("* #+END_SRC#+BEGIN_SRC | a | b |*** #+NAME: .  \n", "org"),
            Some(vec![bullet_text(
                1,
                "#+END_SRC#+BEGIN_SRC | a | b |*** #+NAME: .  ",
                "org",
                2,
                0,
                48
            )])
        );
        assert_eq!(
            try_parse("* #+TITLE: x\n", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::Directive {
                    name: "TITLE".into(),
                    value: "x".into(),
                    span: Some(Span(2, 13)),
                },
            ])
        );
    }

    #[test]
    fn timestamps_accept_mldoc_non_space_newlines_inside_tokens() {
        let blocks = try_parse("SCHEDULED: <2026-06-20\n Sat>", "md").unwrap();
        let [Block::Paragraph { inline, .. }] = blocks.as_slice() else {
            panic!("expected one paragraph");
        };
        assert!(matches!(
            inline.as_slice(),
            [Inline::Timestamp { ts, .. }] if ts == "Scheduled"
        ));
    }

    #[test]
    fn heading_title_hr_and_special_blocks_are_owned_by_v2() {
        assert_eq!(
            try_parse("# ---", "md"),
            Some(vec![heading(1, Some(1), 0, 2), hr(2, 5)])
        );
        assert_eq!(
            try_parse("* -----", "org"),
            Some(vec![bullet(1, 0, 2), hr(2, 7)])
        );
        assert_eq!(
            try_parse("* #+BEGIN_SRC\nx\n#+END_SRC", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(2, 25)),
                },
            ])
        );
        assert_eq!(
            try_parse("# $$x$$", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(2, 7)),
                },
            ])
        );
        assert_eq!(
            try_parse("# $$x$$tail", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::DisplayedMath {
                    text: "x".into(),
                    span: Some(Span(2, 7)),
                },
                paragraph("tail", "md", 7, 11),
            ])
        );
        assert_eq!(
            try_parse("* $$a\nb$$", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::DisplayedMath {
                    text: "a\nb".into(),
                    span: Some(Span(2, 9)),
                },
            ])
        );
        assert_eq!(
            try_parse(r"# \begin{eq}a\end{eq}", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::LatexEnv {
                    name: "eq".into(),
                    content: "a".into(),
                    span: Some(Span(2, 21)),
                },
            ])
        );
        assert_eq!(
            try_parse(r"# \begin{eq}a\end{eq}tail", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::LatexEnv {
                    name: "eq".into(),
                    content: "a".into(),
                    span: Some(Span(2, 21)),
                },
                paragraph("tail", "md", 21, 25),
            ])
        );
        assert_eq!(
            try_parse("# <div>x</div>", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                raw_html("<div>x</div>", 2, 14),
            ])
        );
        assert_eq!(
            try_parse("# <div>x</div>tail", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                raw_html("<div>x</div>", 2, 14),
                paragraph("tail", "md", 14, 18),
            ])
        );
        assert_eq!(
            try_parse("# <div>x</div><span>y</span>", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                raw_html("<div>x</div>", 2, 14),
                raw_html("<span>y</span>", 14, 28),
            ])
        );
        for (input, format) in [
            ("# | a |", "md"),
            ("# | a |\n|---|\n| b |", "md"),
            ("* | a |", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("* \\begin{Eq}\nx\n\\END{eq}", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::LatexEnv {
                    name: "eq".into(),
                    content: "x\n".into(),
                    span: Some(Span(2, 23)),
                },
            ])
        );
        assert_eq!(
            try_parse("- ```\nx\n```", "md"),
            Some(vec![
                bullet(1, 0, 2),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(2, 11)),
                },
            ])
        );
        assert_eq!(
            try_parse("# ```rust\nx\n```", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                Block::Src {
                    lang: "rust".into(),
                    code: "x\n".into(),
                    span: Some(Span(2, 15)),
                },
            ])
        );
        assert_eq!(
            try_parse("* ```\nx\n```", "org"),
            Some(vec![
                bullet(1, 0, 2),
                Block::Src {
                    lang: String::new(),
                    code: "x\n".into(),
                    span: Some(Span(2, 11)),
                },
            ])
        );
        assert_eq!(
            try_parse("# #+BEGIN_EXPORT html\nx\n#+END_EXPORT", "md"),
            Some(vec![
                heading(1, Some(1), 0, 2),
                export_block("html", None, "x\n", 2, 36),
            ])
        );
        for (input, format) in [
            ("# #+BEGIN_COMMENT\nx\n#+END_COMMENT", "md"),
            ("- #+BEGIN_EXAMPLE\nx\n#+END_EXAMPLE", "md"),
            ("* #+BEGIN_EXPORT html\nx\n#+END_EXPORT", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{format} {input:?}"
            );
        }
    }

    #[test]
    fn directives_are_owned_and_absorb_following_eols() {
        for (input, format) in [
            ("#+TITLE: hello  ", "md"),
            ("  #+TODO: x", "md"),
            ("text\n#+A: b\nnext", "md"),
            ("#+A B: \t\x1av  ", "md"),
            ("#+TITLE: hello  ", "org"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(crate::parse(input, format)),
                "{input:?}"
            );
        }
        assert_eq!(
            try_parse("#+A: b\n\ntext", "md"),
            Some(vec![
                Block::Directive {
                    name: "A".into(),
                    value: "b".into(),
                    span: Some(Span(0, 8)),
                },
                paragraph("text", "md", 8, 12),
            ])
        );
        assert_eq!(
            try_parse("#+A: b\r\n\r\ntext", "md"),
            Some(vec![
                Block::Directive {
                    name: "A".into(),
                    value: "b".into(),
                    span: Some(Span(0, 10)),
                },
                paragraph("text", "md", 10, 14),
            ])
        );
    }

    #[test]
    fn begin_looking_directives_fall_back_to_parse2_properties() {
        for (input, format, key, value) in [
            ("#+BEGIN_x: no", "md", "BEGIN_x", "no"),
            ("#+Begin_x: yes", "org", "Begin_x", "yes"),
            ("#+begin_x: no", "org", "begin_x", "no"),
        ] {
            assert_eq!(
                try_parse(input, format),
                Some(vec![Block::Properties {
                    props: vec![Property::parse2((key.into(), value.into()))],
                    span: Some(Span(0, input.len())),
                }]),
                "{format} {input:?}"
            );
        }
        assert_eq!(
            try_parse("#+: empty", "md"),
            Some(crate::parse("#+: empty", "md"))
        );

        let input = "#+BEGIN_x: no\n\ntext";
        let blocks = try_parse(input, "md").unwrap();
        let text_start = input.find("text").unwrap();
        assert!(matches!(
            blocks.as_slice(),
            [
                Block::Properties { span, .. },
                Block::Paragraph { inline, .. },
            ] if *span == Some(Span(0, text_start))
                && matches!(inline.first(), Some(Inline::Plain { text, .. }) if text == "text")
        ));

        let blocks = try_parse("#+BEGIN_x: no\n\nkey:: value", "md").unwrap();
        let expected = vec![
            Property::parse2(("BEGIN_x".into(), "no".into())),
            Property::parse1(("key".into(), "value".into())),
        ];
        assert!(matches!(
            blocks.as_slice(),
            [Block::Properties { props, .. }] if props == &expected
        ));
    }

    #[test]
    fn comments_are_owned_with_format_specific_eol_behavior() {
        assert_eq!(try_parse("# c", "org"), Some(vec![comment("c", 0, 3)]));
        assert_eq!(
            try_parse("  #   indented", "org"),
            Some(vec![comment("indented", 0, 14)])
        );
        assert_eq!(
            try_parse("# c\n\ntext", "org"),
            Some(vec![comment("c", 0, 5), paragraph("text", "org", 5, 9),])
        );
        assert_eq!(
            try_parse("\n# c", "org"),
            Some(crate::parse("\n# c", "org"))
        );
        for input in ["# ", "#c", "## c"] {
            assert_eq!(try_parse(input, "org"), Some(crate::parse(input, "org")));
        }

        assert_eq!(try_parse("[//]: # c", "md"), Some(vec![comment("c", 0, 9)]));
        assert_eq!(
            try_parse("  [//]: #   c  ", "md"),
            Some(vec![comment("c  ", 0, 15)])
        );
        assert_eq!(
            try_parse("text\n[//]: # c\nnext", "md"),
            Some(vec![
                paragraph("text\n", "md", 0, 5),
                comment("c", 5, 14),
                paragraph("\nnext", "md", 14, 19),
            ])
        );
        assert_eq!(
            try_parse("[//]: # c\n---", "md"),
            Some(vec![
                comment("c", 0, 9),
                paragraph("\n", "md", 9, 10),
                hr(10, 13),
            ])
        );
        assert_eq!(
            try_parse("[//]: #", "md"),
            Some(crate::parse("[//]: #", "md"))
        );
        assert_eq!(
            try_parse("<!--\nx\n-->\ny", "md"),
            Some(vec![
                raw_html("<!--\nx\n-->", 0, 11),
                paragraph("y", "md", 11, 12),
            ])
        );
    }

    #[test]
    fn org_hr_is_exactly_five_dashes() {
        assert_eq!(try_parse("-----\n", "org"), Some(vec![hr(0, 6)]));
        assert_eq!(
            try_parse("---\n", "org"),
            Some(crate::parse("---\n", "org"))
        );
        assert_eq!(
            try_parse("------\n", "org"),
            Some(crate::parse("------\n", "org"))
        );
    }
}

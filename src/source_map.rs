use crate::projection::{Inline, Span, SpanMapSegment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginSegment {
    pub text_off: usize,
    pub src_off: usize,
    pub text_len: usize,
    pub src_len: usize,
}

impl OriginSegment {
    #[inline]
    pub(crate) fn new(text_off: usize, src_off: usize, text_len: usize, src_len: usize) -> Self {
        Self {
            text_off,
            src_off,
            text_len,
            src_len,
        }
    }

    #[inline]
    fn text_end(self) -> usize {
        self.text_off + self.text_len
    }

    #[inline]
    fn src_end(self) -> usize {
        self.src_off + self.src_len
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OriginMap {
    segments: Vec<OriginSegment>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OriginCursor {
    idx: usize,
    #[cfg(debug_assertions)]
    map_id: Option<usize>,
}

impl OriginCursor {
    pub(crate) fn new() -> Self {
        Self {
            idx: 0,
            #[cfg(debug_assertions)]
            map_id: None,
        }
    }

    #[inline]
    fn bind(&mut self, _map: &OriginMap) {
        #[cfg(debug_assertions)]
        {
            let id = _map as *const OriginMap as usize;
            match self.map_id {
                Some(existing) => debug_assert_eq!(
                    existing, id,
                    "OriginCursor reused with a different OriginMap"
                ),
                None => self.map_id = Some(id),
            }
        }
    }
}

impl OriginMap {
    pub(crate) fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.segments.clear();
    }

    pub(crate) fn segments(&self) -> &[OriginSegment] {
        &self.segments
    }

    pub(crate) fn push(
        &mut self,
        text_off: usize,
        src_off: usize,
        text_len: usize,
        src_len: usize,
    ) {
        if text_len == 0 || src_len == 0 {
            return;
        }
        if let Some(last) = self.segments.last_mut() {
            if last.text_end() == text_off
                && last.src_end() == src_off
                && last.text_len == last.src_len
                && text_len == src_len
            {
                last.text_len += text_len;
                last.src_len += src_len;
                return;
            }
        }
        self.segments
            .push(OriginSegment::new(text_off, src_off, text_len, src_len));
    }

    pub(crate) fn push_composed(
        &mut self,
        text_off: usize,
        parent: Option<&OriginMap>,
        parent_text_off: usize,
        text_len: usize,
        parent_cursor: Option<&mut OriginCursor>,
    ) {
        if text_len == 0 {
            return;
        }
        if let Some(parent) = parent {
            if let Some(cursor) = parent_cursor {
                parent.copy_interval_into(self, text_off, parent_text_off, text_len, cursor);
            } else {
                let mut cursor = OriginCursor::new();
                parent.copy_interval_into(self, text_off, parent_text_off, text_len, &mut cursor);
            }
        } else {
            self.push(text_off, parent_text_off, text_len, text_len);
        }
    }

    pub(crate) fn push_composed_eol(
        &mut self,
        text_off: usize,
        parent: Option<&OriginMap>,
        parent_text_off: usize,
        parent_text_len: usize,
        parent_cursor: Option<&mut OriginCursor>,
    ) {
        if parent_text_len == 0 {
            return;
        }
        if let Some(parent) = parent {
            if let Some(cursor) = parent_cursor {
                parent.copy_interval_into(self, text_off, parent_text_off, parent_text_len, cursor);
            } else {
                let mut cursor = OriginCursor::new();
                parent.copy_interval_into(
                    self,
                    text_off,
                    parent_text_off,
                    parent_text_len,
                    &mut cursor,
                );
            }
        } else {
            self.push(text_off, parent_text_off, 1, parent_text_len);
        }
    }

    pub(crate) fn truncate_text_len(&mut self, len: usize) {
        let mut keep = 0usize;
        // scan-owner: (b) structural map cursor — OriginMap segment prefix trim
        while keep < self.segments.len() {
            crate::metrics::scan_work(1);
            let seg = self.segments[keep];
            if seg.text_off >= len {
                break;
            }
            if seg.text_end() > len {
                let kept_text_len = len - seg.text_off;
                if kept_text_len == 0 {
                    break;
                }
                let kept_src_len = if seg.text_len == seg.src_len {
                    kept_text_len
                } else if kept_text_len == seg.text_len {
                    seg.src_len
                } else {
                    break;
                };
                self.segments[keep].text_len = kept_text_len;
                self.segments[keep].src_len = kept_src_len;
                keep += 1;
                break;
            }
            keep += 1;
        }
        self.segments.truncate(keep);
    }

    fn advance_to(&self, cursor: &mut OriginCursor, text_off: usize) {
        cursor.bind(self);
        // Monotonicity tripwire (A2): the cursor never rewinds `idx`, so a query for a
        // `text_off` that lands before the end of an already-skipped segment can no longer see
        // that segment — it would return a wrong/short envelope SILENTLY (the fresh-cursor-per-
        // fragment class of bug fixed three times in the raw-html/spans work). We skip segment
        // `j` only once a prior request passed its end, so correct monotone usage always has
        // `text_off >= segments[idx-1].text_end()`; a violation is a threading bug, not slow.
        #[cfg(debug_assertions)]
        if cursor.idx > 0 {
            debug_assert!(
                text_off >= self.segments[cursor.idx - 1].text_end(),
                "OriginCursor queried backwards: text_off {} < last-skipped segment end {} \
                 (a shared cursor was advanced past this offset — silent wrong span)",
                text_off,
                self.segments[cursor.idx - 1].text_end()
            );
        }
        while cursor.idx < self.segments.len() {
            crate::metrics::scan_work(1);
            if self.segments[cursor.idx].text_end() > text_off {
                break;
            }
            cursor.idx += 1;
        }
    }

    pub(crate) fn envelope(
        &self,
        start: usize,
        end: usize,
        cursor: &mut OriginCursor,
    ) -> Option<Span> {
        if start >= end {
            return None;
        }
        self.advance_to(cursor, start);
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        let mut idx = cursor.idx;
        while let Some(seg) = self.segments.get(idx).copied() {
            crate::metrics::scan_work(1);
            if seg.text_off >= end {
                break;
            }
            let a = start.max(seg.text_off);
            let b = end.min(seg.text_end());
            if a >= b {
                idx += 1;
                continue;
            }
            let (src, src_len) = seg_src_subrange(seg, a, b);
            lo = lo.min(src);
            hi = hi.max(src + src_len);
            idx += 1;
        }
        (lo != usize::MAX).then_some(Span(lo, hi))
    }

    pub(crate) fn boundary_at(&self, text_off: usize, cursor: &mut OriginCursor) -> usize {
        self.advance_to(cursor, text_off);
        if let Some(seg) = self.segments.get(cursor.idx).copied() {
            crate::metrics::scan_work(1);
            if text_off < seg.text_off {
                return cursor
                    .idx
                    .checked_sub(1)
                    .map(|prev| self.segments[prev].src_end())
                    .unwrap_or(seg.src_off);
            }
            if seg.text_len == seg.src_len {
                return seg.src_off + (text_off - seg.text_off).min(seg.src_len);
            }
            return if text_off == seg.text_off {
                seg.src_off
            } else {
                seg.src_end()
            };
        }
        self.segments.last().map(|seg| seg.src_end()).unwrap_or(0)
    }

    fn copy_interval_into(
        &self,
        out: &mut OriginMap,
        out_text_off: usize,
        start: usize,
        len: usize,
        cursor: &mut OriginCursor,
    ) {
        let end = start + len;
        self.advance_to(cursor, start);
        let mut idx = cursor.idx;
        while let Some(seg) = self.segments.get(idx).copied() {
            crate::metrics::scan_work(1);
            if seg.text_off >= end {
                break;
            }
            let a = start.max(seg.text_off);
            let b = end.min(seg.text_end());
            if a >= b {
                idx += 1;
                continue;
            }
            let (src, src_len) = seg_src_subrange(seg, a, b);
            out.push(out_text_off + (a - start), src, b - a, src_len);
            idx += 1;
        }
    }
}

#[inline]
fn seg_src_subrange(seg: OriginSegment, a: usize, b: usize) -> (usize, usize) {
    if seg.text_len == seg.src_len {
        let delta = a - seg.text_off;
        (seg.src_off + delta, b - a)
    } else {
        (seg.src_off, seg.src_len)
    }
}

pub(crate) fn push_wire_segment(
    out: &mut Vec<SpanMapSegment>,
    text_off: usize,
    src_off: usize,
    len: usize,
) {
    if len == 0 {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.0 + last.2 == text_off && last.1 + last.2 == src_off {
            last.2 += len;
            return;
        }
    }
    out.push(SpanMapSegment(text_off, src_off, len));
}

pub(crate) fn wire_map_from_origins(
    text: &str,
    origins: &[OriginSegment],
    source: &str,
    source_base: usize,
) -> Vec<SpanMapSegment> {
    let mut out = Vec::new();
    let text_bytes = text.as_bytes();
    let source_bytes = source.as_bytes();
    for seg in origins {
        crate::metrics::scan_work(1);
        if seg.text_len != seg.src_len || seg.text_len == 0 {
            continue;
        }
        let Some(src_rel) = seg.src_off.checked_sub(source_base) else {
            continue;
        };
        if src_rel + seg.src_len > source_bytes.len()
            || seg.text_off + seg.text_len > text_bytes.len()
        {
            continue;
        }
        let src = &source_bytes[src_rel..src_rel + seg.src_len];
        let txt = &text_bytes[seg.text_off..seg.text_off + seg.text_len];
        crate::metrics::scan_work(seg.text_len);
        if src == txt {
            push_wire_segment(&mut out, seg.text_off, seg.src_off, seg.text_len);
        }
    }
    out
}

fn source_slice_equals(source: &str, span: Span, text: &str) -> bool {
    let b = source.as_bytes();
    if span.1 > b.len() {
        return false;
    }
    crate::metrics::scan_work(text.len());
    &b[span.0..span.1] == text.as_bytes()
}

pub(crate) fn make_plain(
    text: String,
    span: Span,
    origins: Vec<OriginSegment>,
    source: &str,
    source_base: usize,
) -> Inline {
    let s5 = if let Some(start) = span.0.checked_sub(source_base) {
        let end = start + (span.1 - span.0);
        end <= source.len() && &source.as_bytes()[start..end] == text.as_bytes()
    } else {
        false
    };
    let span_map = if s5 {
        None
    } else {
        Some(wire_map_from_origins(&text, &origins, source, source_base))
    };
    Inline::Plain {
        text,
        span: Some(span),
        span_map,
    }
}

pub(crate) fn remap_inlines(
    inlines: &mut [Inline],
    current_input: &str,
    source_body: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) {
    // scan-owner: (b) monotone cursor / (o) span-map output — source-map inline remap walk
    for node in inlines {
        crate::metrics::scan_work(1);
        remap_inline(node, current_input, source_body, origin, cursor);
    }
}

fn remap_inline(
    node: &mut Inline,
    current_input: &str,
    source_body: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) {
    let node_cursor = *cursor;
    match node {
        Inline::Plain {
            text,
            span,
            span_map,
        } => {
            let local_span = span.take();
            let local_map = span_map.take();
            let absolute = remapped_span(local_span, origin, cursor);
            let wire = absolute.and_then(|abs| {
                if source_slice_equals(source_body, abs, text) {
                    None
                } else {
                    Some(match local_map {
                        Some(local) => remap_existing_plain_map(
                            text,
                            &local,
                            current_input,
                            source_body,
                            origin,
                            cursor,
                        ),
                        None => local_span
                            .map(|sp| {
                                remap_plain_from_origin_span(
                                    text,
                                    sp,
                                    current_input,
                                    source_body,
                                    origin,
                                    cursor,
                                )
                            })
                            .unwrap_or_default(),
                    })
                }
            });
            *span = Some(absolute.unwrap_or_else(|| {
                let mut fallback_cursor = OriginCursor::new();
                let p = origin.boundary_at(0, &mut fallback_cursor);
                Span(p, p)
            }));
            *span_map = wire;
        }
        Inline::Emphasis { children, span, .. }
        | Inline::Subscript { children, span }
        | Inline::Superscript { children, span }
        | Inline::Tag { children, span } => {
            let local = span.take();
            *span = Some(remapped_span(local, origin, cursor).unwrap_or_else(|| {
                let mut collapsed_cursor = node_cursor;
                collapsed(local, origin, &mut collapsed_cursor)
            }));
            let mut child_cursor = node_cursor;
            remap_inlines(
                children,
                current_input,
                source_body,
                origin,
                &mut child_cursor,
            );
        }
        Inline::Link { label, span, .. } => {
            let local = span.take();
            *span = Some(remapped_span(local, origin, cursor).unwrap_or_else(|| {
                let mut collapsed_cursor = node_cursor;
                collapsed(local, origin, &mut collapsed_cursor)
            }));
            let mut child_cursor = node_cursor;
            remap_inlines(label, current_input, source_body, origin, &mut child_cursor);
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
        | Inline::Hiccup { span, .. } => {
            let local = span.take();
            *span = Some(remapped_span(local, origin, cursor).unwrap_or_else(|| {
                let mut collapsed_cursor = node_cursor;
                collapsed(local, origin, &mut collapsed_cursor)
            }));
        }
    }
}

fn remapped_span(
    span: Option<Span>,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) -> Option<Span> {
    let span = span?;
    origin.envelope(span.0, span.1, cursor).or_else(|| {
        let p = origin.boundary_at(span.0, cursor);
        Some(Span(p, p))
    })
}

fn collapsed(span: Option<Span>, origin: &OriginMap, cursor: &mut OriginCursor) -> Span {
    let p = origin.boundary_at(span.map(|s| s.0).unwrap_or(0), cursor);
    Span(p, p)
}

fn remap_existing_plain_map(
    text: &str,
    local: &[SpanMapSegment],
    current_input: &str,
    source_body: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) -> Vec<SpanMapSegment> {
    let mut out = Vec::new();
    // scan-owner: (b) monotone cursor / (o) span-map output — existing span-map segment remap
    for SpanMapSegment(text_off, local_src, len) in local.iter().copied() {
        crate::metrics::scan_work(1);
        remap_exact_interval(
            &mut out,
            text,
            text_off,
            local_src,
            len,
            current_input,
            source_body,
            origin,
            cursor,
        );
    }
    out
}

fn remap_plain_from_origin_span(
    text: &str,
    span: Span,
    current_input: &str,
    source_body: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) -> Vec<SpanMapSegment> {
    let mut out = Vec::new();
    origin.advance_to(cursor, span.0);
    let mut idx = cursor.idx;
    while let Some(seg) = origin.segments().get(idx).copied() {
        crate::metrics::scan_work(1);
        if seg.text_off >= span.1 {
            break;
        }
        let a = span.0.max(seg.text_off);
        let b = span.1.min(seg.text_end());
        if a >= b {
            idx += 1;
            continue;
        }
        remap_origin_piece(
            &mut out,
            text,
            a - span.0,
            a,
            b - a,
            current_input,
            source_body,
            seg,
        );
        idx += 1;
    }
    out
}

fn remap_exact_interval(
    out: &mut Vec<SpanMapSegment>,
    text: &str,
    text_off: usize,
    local_src: usize,
    len: usize,
    current_input: &str,
    source_body: &str,
    origin: &OriginMap,
    cursor: &mut OriginCursor,
) {
    let end = local_src + len;
    origin.advance_to(cursor, local_src);
    let mut idx = cursor.idx;
    while let Some(seg) = origin.segments().get(idx).copied() {
        crate::metrics::scan_work(1);
        if seg.text_off >= end {
            break;
        }
        let a = local_src.max(seg.text_off);
        let b = end.min(seg.text_end());
        if a >= b {
            idx += 1;
            continue;
        }
        remap_origin_piece(
            out,
            text,
            text_off + (a - local_src),
            a,
            b - a,
            current_input,
            source_body,
            seg,
        );
        idx += 1;
    }
}

fn remap_origin_piece(
    out: &mut Vec<SpanMapSegment>,
    text: &str,
    text_off: usize,
    local_src: usize,
    len: usize,
    current_input: &str,
    source_body: &str,
    seg: OriginSegment,
) {
    if seg.text_len != seg.src_len {
        return;
    }
    let src_off = seg.src_off + (local_src - seg.text_off);
    if text_off + len > text.len()
        || local_src + len > current_input.len()
        || src_off + len > source_body.len()
    {
        return;
    }
    let text_bytes = &text.as_bytes()[text_off..text_off + len];
    let current_bytes = &current_input.as_bytes()[local_src..local_src + len];
    let source_bytes = &source_body.as_bytes()[src_off..src_off + len];
    crate::metrics::scan_work(len);
    if text_bytes == current_bytes && text_bytes == source_bytes {
        push_wire_segment(out, text_off, src_off, len);
    }
}

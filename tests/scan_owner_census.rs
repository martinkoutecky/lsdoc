//! Scan-owner census enforcement.
//!
//! This source scanner strips comments and string/char literals before detecting broad
//! parser-work candidates. It scans top-level `src/*.rs`, excludes `src/bin`, and blanks
//! `#[cfg(test)] mod ... { ... }` modules before matching. A candidate is satisfied only
//! by a `// scan-owner:` annotation in the preceding five source lines, or by an enclosing
//! function listed in the embedded verified Phase 1 census / single-owner allowlist.

use std::fs;
use std::path::{Path, PathBuf};

const SINGLE_OWNER_FNS: &[(&str, &str)] = &[
    ("src/block_common.rs", "split_lines"),
    ("src/block_common.rs", "leading_ws"),
    ("src/block_common.rs", "mldoc_spaces_len"),
    ("src/block_common.rs", "mldoc_trim_spaces_end_len"),
    ("src/block_common.rs", "ocaml_trim"),
    ("src/block_common.rs", "ocaml_trim_end"),
    ("src/block_common.rs", "para_ws_only"),
    ("src/block_common.rs", "drawer_property"),
    ("src/block_common.rs", "raw_html_canonical_close"),
    ("src/block_common.rs", "view_tail_has_peek"),
    ("src/block_common.rs", "raw_html_raw_capture"),
    ("src/block_common.rs", "raw_html_view_capture"),
    ("src/block_common.rs", "find_matching_fence"),
    ("src/block_common.rs", "find_drawer_end"),
    ("src/inline.rs", "find_sub"),
    ("src/inline.rs", "find_sub_line"),
    ("src/inline.rs", "match_brackets_end_raw"),
    ("src/inline.rs", "next_rr_or_nl_raw"),
    ("src/inline.rs", "count_occurrences"),
    ("src/inline.rs", "nested_children_count"),
    ("src/inline.rs", "parse_macro"),
    ("src/inline.rs", "find_ci"),
    ("src/parse.rs", "view_abs_start"),
    ("src/parse.rs", "md_view_abs_start"),
    ("src/parse.rs", "md_html_comment_opener"),
    ("src/resolver.rs", "parse_inline_ctx_md_label"),
    ("src/resolver.rs", "parse_markdown_script_body"),
    ("src/resolver.rs", "concat_plains_without_pos"),
    ("src/resolver.rs", "find_delim_token_containing"),
    ("src/org.rs", "build_org_indexes"),
    ("src/org.rs", "body_is_clean_window"),
    ("src/org.rs", "org_spaces_len"),
    ("src/org_resolver.rs", "parse_nested_plain_org"),
    ("src/org_resolver.rs", "concat_plains_without_pos"),
    ("src/source_map.rs", "truncate_text_len"),
    ("src/source_map.rs", "remap_inlines"),
    ("src/source_map.rs", "remap_existing_plain_map"),
    ("src/parse.rs", "atx_size"),
    ("src/parse.rs", "heading_at"),
];

const PHASE1_CENSUS: &str = include_str!("../subagent-tasks/notes/scan-loop-census.md");

#[derive(Clone)]
struct FnScope {
    name: String,
    depth: usize,
}

#[test]
fn scan_owner_annotations_cover_candidates() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    for entry in fs::read_dir(&src).expect("read src dir") {
        let entry = entry.expect("src dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    files.sort();

    let mut misses = Vec::new();
    for path in files {
        let rel = rel_path(&root, &path);
        let original = fs::read_to_string(&path).expect("read rust source");
        let stripped = strip_cfg_test_modules(&strip_comments_and_literals(&original));
        scan_file(&rel, &original, &stripped, &mut misses);
    }

    assert!(
        misses.is_empty(),
        "missing scan-owner annotations:\n{}",
        misses.join("\n")
    );
}

fn scan_file(rel: &str, original: &str, stripped: &str, misses: &mut Vec<String>) {
    let original_lines: Vec<&str> = original.lines().collect();
    let stripped_lines: Vec<&str> = stripped.lines().collect();
    let mut fn_stack: Vec<FnScope> = Vec::new();
    let mut pending_fn: Option<(String, usize)> = None;
    let mut brace_depth = 0usize;

    for (idx, line) in stripped_lines.iter().enumerate() {
        let line_no = idx + 1;
        if pending_fn.is_none() {
            if let Some(name) = fn_name(line) {
                pending_fn = Some((name, line_no));
            }
        }

        if is_candidate_line(line) {
            let site_owned = has_scan_owner_near(&original_lines, line_no, 5);
            let fn_owned = fn_stack
                .last()
                .is_some_and(|scope| is_single_owner(rel, &scope.name));
            if !site_owned && !fn_owned {
                let scope = fn_stack
                    .last()
                    .map(|s| s.name.as_str())
                    .unwrap_or("<module>");
                misses.push(format!("{rel}:{line_no} in {scope}: {}", line.trim()));
            }
        }

        let opens = line.as_bytes().iter().filter(|&&b| b == b'{').count();
        let closes = line.as_bytes().iter().filter(|&&b| b == b'}').count();
        if let Some((name, start_line)) = pending_fn.take() {
            if opens > 0 {
                fn_stack.push(FnScope {
                    name,
                    depth: brace_depth + 1,
                });
            } else {
                pending_fn = Some((name, start_line));
            }
        }
        brace_depth = brace_depth.saturating_add(opens).saturating_sub(closes);
        while fn_stack.last().is_some_and(|scope| brace_depth < scope.depth) {
            fn_stack.pop();
        }
    }
}

fn is_single_owner(rel: &str, name: &str) -> bool {
    SINGLE_OWNER_FNS
        .iter()
        .any(|(file, fn_name)| *file == rel && *fn_name == name)
        || PHASE1_CENSUS.lines().any(|line| {
            line.starts_with("| `src/")
                && line.contains(&format!("`{rel}:"))
                && line.contains(&format!("`{name}`"))
        })
}

fn has_scan_owner_near(lines: &[&str], line_no: usize, distance: usize) -> bool {
    let start = line_no.saturating_sub(distance + 1);
    let end = line_no.saturating_sub(1);
    lines
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .any(|line| line.trim_start().starts_with("// scan-owner:"))
}

fn is_candidate_line(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return false;
    }
    let method = [
        ".find(",
        ".rfind(",
        ".position(",
        ".contains(",
        ".any(",
        ".all(",
        ".take_while(",
        ".trim(",
        ".trim_start(",
        ".trim_end(",
        ".strip_prefix(",
        ".strip_suffix(",
        ".split(",
        ".match_indices(",
        ".chars(",
        ".to_string(",
        ".to_owned(",
        ".push_str(",
        ".join(",
        ".collect(",
    ];
    l.starts_with("while ")
        || l.starts_with("while(")
        || l.starts_with("for ")
        || l == "loop {"
        || l.contains("format!(")
        || l.contains("with_capacity(")
        || l.contains("vec![")
        || method.iter().any(|pat| l.contains(pat))
}

fn fn_name(line: &str) -> Option<String> {
    let pos = line.find("fn ")?;
    let after = &line[pos + 3..];
    let mut name = String::new();
    for ch in after.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            name.push(ch);
        } else {
            break;
        }
    }
    (!name.is_empty()).then_some(name)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("source file under manifest dir")
        .to_string_lossy()
        .replace('\\', "/")
}

fn strip_cfg_test_modules(src: &str) -> String {
    let mut out = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut pending_cfg_test = false;
    let mut skipping = false;
    let mut depth = 0isize;

    for line in lines {
        if skipping {
            depth += brace_delta(line);
            out.push(String::new());
            if depth <= 0 {
                skipping = false;
            }
            continue;
        }
        if line.contains("#[cfg(test)]") {
            pending_cfg_test = true;
            out.push(line.to_string());
            continue;
        }
        if pending_cfg_test && line.contains("mod ") && line.contains('{') {
            skipping = true;
            depth = brace_delta(line);
            out.push(String::new());
            if depth <= 0 {
                skipping = false;
            }
            pending_cfg_test = false;
            continue;
        }
        if !line.trim().is_empty() {
            pending_cfg_test = false;
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

fn brace_delta(line: &str) -> isize {
    let opens = line.as_bytes().iter().filter(|&&b| b == b'{').count() as isize;
    let closes = line.as_bytes().iter().filter(|&&b| b == b'}').count() as isize;
    opens - closes
}

fn strip_comments_and_literals(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    let mut block_depth = 0usize;
    while i < bytes.len() {
        if block_depth > 0 {
            if bytes.get(i..i + 2) == Some(b"/*") {
                out.push_str("  ");
                block_depth += 1;
                i += 2;
            } else if bytes.get(i..i + 2) == Some(b"*/") {
                out.push_str("  ");
                block_depth -= 1;
                i += 2;
            } else {
                push_blank_preserve_newline(&mut out, bytes[i]);
                i += 1;
            }
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if bytes.get(i..i + 2) == Some(b"/*") {
            out.push_str("  ");
            block_depth = 1;
            i += 2;
            continue;
        }
        if let Some((end, len)) = raw_string_end(bytes, i) {
            for &b in &bytes[i..end] {
                push_blank_preserve_newline(&mut out, b);
            }
            i += len;
            continue;
        }
        if bytes[i] == b'"' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'"')) {
            let start = i;
            i += if bytes[i] == b'b' { 2 } else { 1 };
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            for &b in &bytes[start..i] {
                push_blank_preserve_newline(&mut out, b);
            }
            continue;
        }
        if bytes[i] == b'\'' && is_lifetime(bytes, i) {
            out.push('\'');
            i += 1;
            continue;
        }
        if bytes[i] == b'\'' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else if bytes[i] == b'\n' {
                    break;
                } else {
                    i += 1;
                }
            }
            for &b in &bytes[start..i] {
                push_blank_preserve_newline(&mut out, b);
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_lifetime(bytes: &[u8], i: usize) -> bool {
    let Some(&next) = bytes.get(i + 1) else {
        return false;
    };
    if !(next == b'_' || next.is_ascii_alphabetic()) {
        return false;
    }
    let mut j = i + 2;
    while bytes
        .get(j)
        .is_some_and(|b| *b == b'_' || b.is_ascii_alphanumeric())
    {
        j += 1;
    }
    bytes.get(j) != Some(&b'\'')
}

fn raw_string_end(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if bytes.get(j) == Some(&b'b') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hash_start = j;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j - hash_start;
    j += 1;
    while j < bytes.len() {
        if bytes[j] == b'"' && bytes.get(j + 1..j + 1 + hashes) == Some(&bytes[hash_start..hash_start + hashes]) {
            let end = j + 1 + hashes;
            return Some((end, end - i));
        }
        j += 1;
    }
    Some((bytes.len(), bytes.len() - i))
}

fn push_blank_preserve_newline(out: &mut String, b: u8) {
    if b == b'\n' {
        out.push('\n');
    } else {
        out.push(' ');
    }
}

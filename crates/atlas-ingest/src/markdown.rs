//! Markdown reader. Ported from graphify `graphify/extractors/markdown.py`.
//!
//! Deterministic (no LLM): emits a doc node for the file, a node per heading
//! (nested by level via `contains` edges), and `references` edges to sibling
//! documents linked by inline `[text](./other.md)`, reference-style
//! `[label]: ./other.md`, and `[[wikilink]]` links. External URLs, in-page
//! anchors, images and non-document targets are skipped. Fenced code blocks are
//! skipped so their contents aren't parsed as headings.

use crate::{edge, node, Extraction};
use atlas_core::ids::{file_stem, make_id};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

fn inline_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [text](target "title") — negative lookbehind for '!' excludes images.
    // Rust `regex` has no lookbehind, so we match an optional leading char and
    // reject when it's '!' in code (see scan below).
    RE.get_or_init(|| Regex::new(r"(?:^|[^!])\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+[^)]*)?\)").unwrap())
}

fn ref_def_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s{0,3}\[[^\]]+\]:\s*<?([^\s>]+)>?").unwrap())
}

fn wikilink_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // [[target]] / [[target|alias]] / [[target#anchor]], not ![[...]].
    RE.get_or_init(|| Regex::new(r"(?:^|[^!])\[\[([^\]|#]+)(?:[#|][^\]]*)?\]\]").unwrap())
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(#{1,6})\s+(.+)").unwrap())
}

const LINKABLE_EXTS: [&str; 6] = ["md", "mdx", "qmd", "markdown", "rst", "txt"];

/// Resolve a markdown link target to the absolute-ish path of a sibling
/// document, or None to skip. Mirrors graphify `_resolve_markdown_link`:
/// strips anchor/query, rejects external URLs and non-doc extensions, and
/// treats extension-less targets (typical of wikilinks) as sibling `.md`.
fn resolve_link(raw: &str, source_dir: &Path) -> Option<String> {
    let mut target = raw.trim();
    if target.is_empty() {
        return None;
    }
    // Drop anchor / query so #section links still resolve to the target doc.
    target = target.split('#').next().unwrap_or("");
    target = target.split('?').next().unwrap_or("");
    target = target.trim();
    if target.is_empty() {
        return None;
    }
    let low = target.to_lowercase();
    if target.contains("://")
        || low.starts_with("mailto:")
        || low.starts_with("tel:")
        || low.starts_with("//")
        || low.starts_with("data:")
    {
        return None;
    }
    let mut owned = target.to_string();
    let suffix = Path::new(target)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let suffix = if suffix.is_empty() {
        owned.push_str(".md");
        "md".to_string()
    } else {
        suffix
    };
    if !LINKABLE_EXTS.contains(&suffix.as_str()) {
        return None;
    }
    let candidate = Path::new(&owned);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        source_dir.join(candidate)
    };
    Some(normpath(&joined))
}

/// Lexical path normalization (collapse `.`/`..`) without touching the
/// filesystem — matches Python's `os.path.normpath` closely enough for node
/// ids; both endpoints go through the same recipe so they merge.
fn normpath(p: &Path) -> String {
    use std::path::Component;
    let mut out: Vec<String> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last().map(String::as_str), Some(s) if s != "..") && !out.is_empty() {
                    out.pop();
                } else {
                    out.push("..".into());
                }
            }
            Component::RootDir => out.push(String::new()),
            other => out.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    let joined = out.join("/");
    if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

/// Extract the structural graph for a single markdown file.
pub fn extract_markdown(path: impl AsRef<Path>) -> Extraction {
    let path = path.as_ref();
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Extraction::default(),
    };
    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);
    let source_dir = path.parent().unwrap_or(Path::new("."));

    let mut out = Extraction::default();
    let mut seen_ids = std::collections::HashSet::new();
    let mut linked_targets = std::collections::HashSet::new();

    let file_nid = make_id([str_path.as_str()]);
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    seen_ids.insert(file_nid.clone());
    out.nodes.push(node(&file_nid, &file_label, "document", &str_path, Some("L1")));

    let add_link = |raw: &str, line: usize, out: &mut Extraction, linked: &mut std::collections::HashSet<String>| {
        if let Some(resolved) = resolve_link(raw, source_dir) {
            let tgt_nid = make_id([resolved.as_str()]);
            if tgt_nid == file_nid || !linked.insert(tgt_nid.clone()) {
                return;
            }
            out.edges
                .push(edge(&file_nid, &tgt_nid, "references", &str_path, Some(&format!("L{line}"))));
        }
    };

    // Heading nesting stack: (level, node_id).
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut in_code_block = false;

    for (i, line_text) in source.lines().enumerate() {
        let line_num = i + 1;
        let stripped = line_text.trim();
        if stripped.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        // Links -> document references, scanned on every non-fenced line.
        for cap in inline_link_re().captures_iter(line_text) {
            add_link(&cap[1], line_num, &mut out, &mut linked_targets);
        }
        for cap in wikilink_re().captures_iter(line_text) {
            add_link(&cap[1], line_num, &mut out, &mut linked_targets);
        }
        if let Some(cap) = ref_def_re().captures(line_text) {
            add_link(&cap[1], line_num, &mut out, &mut linked_targets);
        }

        if let Some(cap) = heading_re().captures(line_text) {
            let level = cap[1].len();
            let title = cap[2].trim();
            let mut h_nid = make_id([stem.as_str(), title]);
            if seen_ids.contains(&h_nid) {
                h_nid = make_id([stem.as_str(), title, &line_num.to_string()]);
            }
            if seen_ids.insert(h_nid.clone()) {
                out.nodes
                    .push(node(&h_nid, title, "document", &str_path, Some(&format!("L{line_num}"))));
            }
            while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                heading_stack.pop();
            }
            let parent = heading_stack
                .last()
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| file_nid.clone());
            out.edges
                .push(edge(&parent, &h_nid, "contains", &str_path, Some(&format!("L{line_num}"))));
            heading_stack.push((level, h_nid));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_skips_external_and_assets() {
        let dir = Path::new("/docs");
        assert!(resolve_link("https://example.com", dir).is_none());
        assert!(resolve_link("#anchor", dir).is_none());
        assert!(resolve_link("mailto:a@b.c", dir).is_none());
        assert!(resolve_link("./logo.png", dir).is_none());
        // extension-less wikilink target -> sibling .md
        assert_eq!(resolve_link("other", dir).unwrap(), "/docs/other.md");
        assert_eq!(resolve_link("./y.md#sec", dir).unwrap(), "/docs/y.md");
    }
}

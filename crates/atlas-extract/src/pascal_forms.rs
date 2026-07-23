//! Pascal form / package extractor — a Rust port of graphify's
//! `extract_delphi_form` / `extract_lazarus_form`
//! (`graphify/extractors/pascal_forms.py`) and `extract_lazarus_package`
//! (`graphify/extract.py`).
//!
//! Dispatched extensions: `.dfm` (Delphi form), `.lfm` (Lazarus form), `.lpk`
//! (Lazarus package, XML).
//!
//! Forms (.dfm/.lfm) are a text tree of `object Name: TClass ... end` blocks.
//! Nodes: the file, each component class, each `OnXxx` event handler. Edges:
//! parent `contains` child class, component `references` handler (context
//! `event`). Binary `.dfm` (FF 0A magic) is skipped gracefully.
//!
//! Packages (.lpk) list a package name, required packages, and member units.
//! Nodes: file, package (name), each required dep, each unit. Edges: file
//! `contains` package, package `imports` dep (context `import`), package
//! `contains` unit.
//!
//! DELTA (single-file scope): graphify's `.lpk` unit resolution rglobs the
//! project tree to map a unit name to its `.pas` file id. atlas resolves a unit
//! to the bare `make_id(unit_name)` (graphify's on-disk-miss fallback) — the
//! cross-file resolution is out of scope, exactly as pascal.rs's `uses` targets
//! stay bare.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    match path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "lpk" => extract_lpk(path, source),
        _ => extract_form(path, source), // .dfm / .lfm
    }
}

// ── .dfm / .lfm forms ───────────────────────────────────────────────────────

fn extract_form(path: &Path, source: &[u8]) -> ExtractResult {
    // Binary DFM streams start with FF 0A — unreadable as text, skipped.
    if source.len() >= 2 && source[0] == 0xff && source[1] == 0x0a {
        return ExtractResult {
            nodes: vec![],
            edges: vec![],
        };
    }
    let text = String::from_utf8_lossy(source);
    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);
    let file_nid = make_id([stem.as_str()]);
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut nodes: Vec<Attrs> = Vec::new();
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen_nodes: HashSet<String> = HashSet::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    let add_node = |nid: &str,
                    label: &str,
                    line: usize,
                    nodes: &mut Vec<Attrs>,
                    seen: &mut HashSet<String>| {
        if seen.insert(nid.to_string()) {
            nodes.push(node_map(nid, label, "code", &str_path, &format!("L{line}")));
        }
    };
    let add_edge = |src: &str,
                    tgt: &str,
                    rel: &str,
                    line: usize,
                    ctx: Option<&str>,
                    edges: &mut Vec<Attrs>,
                    seen: &mut HashSet<(String, String, String)>| {
        let key = (src.to_string(), tgt.to_string(), rel.to_string());
        if seen.insert(key) {
            edges.push(edge_map(src, tgt, rel, ctx, &str_path, &format!("L{line}")));
        }
    };

    add_node(&file_nid, &label, 1, &mut nodes, &mut seen_nodes);

    // (?i) case-insensitive, matching graphify's re.IGNORECASE.
    let obj_re = Regex::new(r"(?i)^\s*object\s+\w+\s*:\s*(\w+)").unwrap();
    let event_re = Regex::new(r"(?i)^\s*On\w+\s*=\s*(\w+)").unwrap();
    let end_re = Regex::new(r"(?i)^\s*end\s*$").unwrap();

    let mut stack: Vec<String> = vec![file_nid.clone()];
    for (idx, line) in text.lines().enumerate() {
        let lineno = idx + 1;
        if let Some(c) = obj_re.captures(line) {
            let class_name = &c[1];
            let nid = make_id([stem.as_str(), class_name]);
            add_node(&nid, class_name, lineno, &mut nodes, &mut seen_nodes);
            add_edge(
                stack.last().unwrap(),
                &nid,
                "contains",
                lineno,
                None,
                &mut edges,
                &mut seen_edges,
            );
            stack.push(nid);
            continue;
        }
        if stack.len() > 1 {
            if let Some(c) = event_re.captures(line) {
                let handler = &c[1];
                let nid = make_id([stem.as_str(), handler]);
                add_node(
                    &nid,
                    &format!("{handler}()"),
                    lineno,
                    &mut nodes,
                    &mut seen_nodes,
                );
                add_edge(
                    stack.last().unwrap(),
                    &nid,
                    "references",
                    lineno,
                    Some("event"),
                    &mut edges,
                    &mut seen_edges,
                );
                continue;
            }
        }
        if end_re.is_match(line) && stack.len() > 1 {
            stack.pop();
        }
    }

    ExtractResult { nodes, edges }
}

// ── .lpk Lazarus package (XML) ──────────────────────────────────────────────

fn extract_lpk(path: &Path, source: &[u8]) -> ExtractResult {
    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);
    let file_nid = make_id([stem.as_str()]);
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut nodes: Vec<Attrs> = Vec::new();
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let add_node = |nid: &str, label: &str, nodes: &mut Vec<Attrs>, seen: &mut HashSet<String>| {
        if seen.insert(nid.to_string()) {
            nodes.push(node_map(nid, label, "code", &str_path, "L1"));
        }
    };

    add_node(&file_nid, &label, &mut nodes, &mut seen);

    let text = String::from_utf8_lossy(source);
    let doc = match roxmltree::Document::parse(&text) {
        Ok(d) => d,
        Err(_) => return ExtractResult { nodes, edges },
    };

    // Package name → package node.
    let pkg_name = doc
        .descendants()
        .find(|n| {
            n.tag_name().name() == "Name"
                && n.parent()
                    .map(|p| p.tag_name().name() == "Package")
                    .unwrap_or(false)
        })
        .and_then(|n| n.attribute("Value"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let pkg_nid = make_id([stem.as_str(), pkg_name.as_str()]);
    add_node(&pkg_nid, &pkg_name, &mut nodes, &mut seen);
    edges.push(edge_map(
        &file_nid, &pkg_nid, "contains", None, &str_path, "L1",
    ));

    // Required packages → imports.
    for pkg_el in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "PackageName")
    {
        if let Some(dep) = pkg_el.attribute("Value") {
            if !dep.is_empty() {
                let dep_nid = make_id([dep]);
                add_node(&dep_nid, dep, &mut nodes, &mut seen);
                edges.push(edge_map(
                    &pkg_nid,
                    &dep_nid,
                    "imports",
                    Some("import"),
                    &str_path,
                    "L1",
                ));
            }
        }
    }

    // Member units → contains (bare-id fallback; on-disk resolution out of scope).
    for unit_el in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "UnitName")
    {
        if let Some(unit) = unit_el.attribute("Value") {
            if !unit.is_empty() {
                let unit_nid = make_id([unit]);
                add_node(&unit_nid, unit, &mut nodes, &mut seen);
                edges.push(edge_map(
                    &pkg_nid, &unit_nid, "contains", None, &str_path, "L1",
                ));
            }
        }
    }

    ExtractResult { nodes, edges }
}

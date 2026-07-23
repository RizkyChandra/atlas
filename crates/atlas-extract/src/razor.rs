//! ASP.NET Razor extractor — a Rust port of graphify `graphify/extractors/razor.py`
//! (pure regex, no grammar). Handles `.razor` and `.cshtml`.
//!
//! Nodes/edges: `@page` route (concept node + `references`, no source_location),
//! `@using`/`@inject` → `imports`, `@inherits` → `inherits`, `@model` →
//! `references`, PascalCase component tags `<Foo …>` → `calls` (HTML tags
//! filtered), and `@code { … }` C# method declarations → `contains` (no
//! source_location on the edge). Symbol ids key off the file stem (FILE).
//!
//! NOT ported (documented in tests/langs.rs): graphify's razor extractor itself
//! only handles `@code` (not `@functions`), so neither do we — matching the
//! oracle.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;

const HTML_TAGS: &[&str] = &[
    "DOCTYPE", "Html", "Head", "Body", "Div", "Span", "Table", "Form", "Input", "Button", "Select",
    "Option", "Label", "Textarea", "Script", "Style", "Link", "Meta", "Title", "Header", "Footer",
    "Nav", "Main", "Section", "Article", "Aside",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source).into_owned();
    let stem = file_stem(path);
    let str_path = path.to_string_lossy().into_owned();
    let file_nid = make_id([stem.as_str()]);
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut ex = Razor {
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
    };
    // File node: source_location null (graphify).
    let mut fnode = node_map(&file_nid, &file_label, "code", &ex.str_path, "");
    fnode.insert("source_location".into(), serde_json::Value::Null);
    ex.nodes.push(fnode);
    ex.seen.insert(file_nid);

    // Directives — anchored at line start, first match per line wins (elif chain).
    let using_re = Regex::new(r"^@using\s+([\w.]+)").unwrap();
    let inject_re = Regex::new(r"^@inject\s+([\w.<>\[\]]+)\s+(\w+)").unwrap();
    let inherits_re = Regex::new(r"^@inherits\s+([\w.<>\[\]]+)").unwrap();
    let model_re = Regex::new(r"^@model\s+([\w.<>\[\]]+)").unwrap();
    let page_re = Regex::new(r#"^@page\s+"([^"]+)""#).unwrap();

    for (idx, line) in src.lines().enumerate() {
        let i = idx + 1;
        if let Some(c) = using_re.captures(line) {
            ex.add_ref(&c[1], "imports", i);
        } else if let Some(c) = inject_re.captures(line) {
            ex.add_ref(&c[1], "imports", i);
        } else if let Some(c) = inherits_re.captures(line) {
            ex.add_ref(&c[1], "inherits", i);
        } else if let Some(c) = model_re.captures(line) {
            ex.add_ref(&c[1], "references", i);
        } else if let Some(c) = page_re.captures(line) {
            ex.add_route(&c[1], i);
        }
    }

    // Component references (PascalCase tags), over the whole source.
    let comp_re = Regex::new(r"<([A-Z][A-Za-z0-9]+)[\s/>]").unwrap();
    for m in comp_re.captures_iter(&src) {
        let comp = m.get(1).unwrap();
        if HTML_TAGS.contains(&comp.as_str()) {
            continue;
        }
        let line = src[..comp.start()].matches('\n').count() + 1;
        ex.add_ref(comp.as_str(), "calls", line);
    }

    // @code { … } blocks → C# method declarations.
    let code_re = Regex::new(r"@code\s*\{").unwrap();
    let method_re = Regex::new(
        r"(?:public|private|protected|internal|static|async|override|virtual|abstract)\s+[\w<>\[\],\s]+\s+(\w+)\s*\(",
    )
    .unwrap();
    for m in code_re.find_iter(&src) {
        let block_start = m.end();
        let block = brace_block(&src, block_start);
        for mm in method_re.captures_iter(block) {
            let name = mm.get(1).unwrap().as_str().to_string();
            let abs_pos = block_start + mm.get(0).unwrap().start();
            let line = src[..abs_pos].matches('\n').count() + 1;
            ex.add_method(&name, line);
        }
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

/// Text from `start` up to the matching close brace (depth started at 1).
fn brace_block(src: &str, start: usize) -> &str {
    let bytes = src.as_bytes();
    let mut depth = 1i32;
    let mut pos = start;
    while pos < bytes.len() && depth > 0 {
        match bytes[pos] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        pos += 1;
    }
    if depth == 0 {
        &src[start..pos - 1]
    } else {
        ""
    }
}

struct Razor {
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
}

impl Razor {
    fn add_ref(&mut self, target: &str, relation: &str, line: usize) {
        let nid = make_id([target]);
        if nid.is_empty() {
            return;
        }
        let loc = format!("L{line}");
        if self.seen.insert(nid.clone()) {
            self.nodes
                .push(node_map(&nid, target, "code", &self.str_path, &loc));
        }
        self.edges.push(edge_map(
            &self.file_nid,
            &nid,
            relation,
            None,
            &self.str_path,
            &loc,
        ));
    }

    /// `@page` route: concept node, and a `references` edge WITHOUT source_location.
    fn add_route(&mut self, route: &str, line: usize) {
        let nid = make_id(["route", route]);
        if nid.is_empty() || !self.seen.insert(nid.clone()) {
            return;
        }
        self.nodes.push(node_map(
            &nid,
            &format!("route:{route}"),
            "concept",
            &self.str_path,
            &format!("L{line}"),
        ));
        let mut e = Attrs::new();
        e.insert("source".into(), json!(self.file_nid));
        e.insert("target".into(), json!(nid));
        e.insert("relation".into(), json!("references"));
        e.insert("confidence".into(), json!("EXTRACTED"));
        e.insert("source_file".into(), json!(self.str_path));
        e.insert("weight".into(), json!(1.0));
        self.edges.push(e);
    }

    /// `@code` method: node with a line, and a `contains` edge WITHOUT source_location.
    fn add_method(&mut self, name: &str, line: usize) {
        let nid = make_id([self.stem.as_str(), name]);
        if nid.is_empty() || !self.seen.insert(nid.clone()) {
            return;
        }
        self.nodes.push(node_map(
            &nid,
            name,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
        let mut e = Attrs::new();
        e.insert("source".into(), json!(self.file_nid));
        e.insert("target".into(), json!(nid));
        e.insert("relation".into(), json!("contains"));
        e.insert("confidence".into(), json!("EXTRACTED"));
        e.insert("source_file".into(), json!(self.str_path));
        e.insert("weight".into(), json!(1.0));
        self.edges.push(e);
    }
}

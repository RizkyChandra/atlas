//! WPF/XAML extractor — a Rust port of graphify's `extract_xaml`
//! (`graphify/extract.py`). Uses `roxmltree` (read-only DOM) in place of Python's
//! ElementTree.
//!
//! Ported (the pure element-tree / markup-parsing core): file → root `contains`,
//! `x:Class` → class node + `references`(context `x_class`), each named element
//! (`x:Name`/`Name`) → `contains` + a `references`(context `type`) to an
//! `xaml_<type>` concept node, `{Binding …}` paths → `references`(context
//! `binding_path` / `binding_command`), `{StaticResource …}` converters →
//! `references`(context `binding_converter`), and the direct `<Binding Path=…
//! Converter=…>` element form. Node ids key off the file stem (FILE) for elements;
//! bindings/converters/types use global `binding`/`binding_converter`/`xaml`
//! prefixes.
//!
//! NOT ported (documented in tests/langs.rs — all require reading OTHER files, out
//! of single-file scope): code-behind (`<file>.xaml.cs`) event-handler wiring,
//! ViewModel inference + whole-project C# scan (`_xaml_csharp_class_nodes`), and
//! CommunityToolkit `[ObservableProperty]`/`[RelayCommand]` member generation.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let text = String::from_utf8_lossy(source).into_owned();
    let doc = match roxmltree::Document::parse(&text) {
        Ok(d) => d,
        Err(_) => {
            return ExtractResult {
                nodes: vec![],
                edges: vec![],
            }
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let stem = file_stem(path);
    let str_path = path.to_string_lossy().into_owned();
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut ex = Xaml {
        str_path,
        lines,
        nodes: Vec::new(),
        edges: Vec::new(),
        seen_nodes: HashSet::new(),
        seen_edges: HashSet::new(),
    };

    let file_nid = make_id([stem.as_str()]);
    let root = doc.root_element();
    let root_type = root.tag_name().name().to_string();
    let root_nid = make_id([stem.as_str(), root_type.as_str()]);

    ex.add_node(&file_nid, &file_label, 1, "code");
    ex.add_node(&root_nid, &root_type, 1, "code");
    ex.add_edge(&file_nid, &root_nid, "contains", 1, None);

    // x:Class → class node (no code-behind lookup — see module docs).
    if let Some(cn) = root
        .attributes()
        .find(|a| a.name() == "Class")
        .map(|a| a.value())
    {
        let cn = cn.trim();
        let class_label = cn.rsplit('.').next().unwrap_or(cn);
        let class_nid = make_id([stem.as_str(), class_label]);
        let line = ex.line_for(cn);
        ex.add_node(&class_nid, class_label, line, "code");
        ex.add_edge(&root_nid, &class_nid, "references", line, Some("x_class"));
    }

    for elem in root.descendants().filter(|n| n.is_element()) {
        let elem_type = elem.tag_name().name();
        let elem_name = elem
            .attributes()
            .find(|a| a.name() == "Name")
            .map(|a| a.value().trim());

        let owner_nid = if let Some(name) = elem_name {
            let owner = make_id([stem.as_str(), name]);
            let line = ex.line_for(name);
            ex.add_node(&owner, name, line, "code");
            ex.add_edge(&root_nid, &owner, "contains", line, None);
            let type_nid = make_id(["xaml", elem_type]);
            ex.add_node(&type_nid, elem_type, line, "concept");
            ex.add_edge(&owner, &type_nid, "references", line, Some("type"));
            owner
        } else {
            root_nid.clone()
        };

        for attr in elem.attributes() {
            let value = attr.value();
            let attr_local = attr.name();
            // Event wiring against code-behind methods is NOT ported (see docs).
            let (binding_path, binding_converter) = binding_refs(value);
            if let Some(bp) = binding_path {
                let bind_nid = make_id(["binding", &bp]);
                let line = ex.line_for(value);
                ex.add_node(&bind_nid, &bp, line, "concept");
                let ctx = if attr_local == "Command" || attr_local.ends_with(".Command") {
                    "binding_command"
                } else {
                    "binding_path"
                };
                ex.add_edge(&owner_nid, &bind_nid, "references", line, Some(ctx));
            }
            if let Some(bc) = binding_converter {
                let conv_nid = make_id(["binding_converter", &bc]);
                let line = ex.line_for(value);
                ex.add_node(&conv_nid, &bc, line, "concept");
                ex.add_edge(
                    &owner_nid,
                    &conv_nid,
                    "references",
                    line,
                    Some("binding_converter"),
                );
            }
            if elem_type == "Binding" && attr_local == "Path" {
                let direct = value.trim();
                if !direct.is_empty() && !direct.contains('{') && !direct.contains('}') {
                    let bind_nid = make_id(["binding", direct]);
                    let line = ex.line_for(value);
                    ex.add_node(&bind_nid, direct, line, "concept");
                    ex.add_edge(
                        &owner_nid,
                        &bind_nid,
                        "references",
                        line,
                        Some("binding_path"),
                    );
                }
            }
            if elem_type == "Binding" && attr_local == "Converter" {
                if let Some(dc) = static_resource_key(value) {
                    let conv_nid = make_id(["binding_converter", &dc]);
                    let line = ex.line_for(value);
                    ex.add_node(&conv_nid, &dc, line, "concept");
                    ex.add_edge(
                        &owner_nid,
                        &conv_nid,
                        "references",
                        line,
                        Some("binding_converter"),
                    );
                }
            }
        }
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Xaml<'a> {
    str_path: String,
    lines: Vec<&'a str>,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen_nodes: HashSet<String>,
    seen_edges: HashSet<(String, String, String, Option<String>)>,
}

impl Xaml<'_> {
    fn line_for(&self, value: &str) -> usize {
        if !value.is_empty() {
            for (i, line) in self.lines.iter().enumerate() {
                if line.contains(value) {
                    return i + 1;
                }
            }
        }
        1
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize, file_type: &str) {
        if self.seen_nodes.insert(nid.to_string()) {
            self.nodes.push(node_map(
                nid,
                label,
                file_type,
                &self.str_path,
                &format!("L{line}"),
            ));
        }
    }

    fn add_edge(
        &mut self,
        src: &str,
        tgt: &str,
        relation: &str,
        line: usize,
        context: Option<&str>,
    ) {
        let key = (
            src.to_string(),
            tgt.to_string(),
            relation.to_string(),
            context.map(str::to_string),
        );
        if self.seen_edges.insert(key) {
            self.edges.push(edge_map(
                src,
                tgt,
                relation,
                context,
                &self.str_path,
                &format!("L{line}"),
            ));
        }
    }
}

/// `{Name args}` → (name, args). Returns None for non-markup or empty inner.
fn markup_extension(value: &str) -> Option<(&str, &str)> {
    let v = value.trim();
    let inner = v.strip_prefix('{')?.strip_suffix('}')?.trim();
    if inner.is_empty() || inner.starts_with('}') {
        return None;
    }
    Some(match inner.split_once(' ') {
        Some((name, args)) => (name, args.trim()),
        None => (inner, ""),
    })
}

/// Split markup args on top-level commas (brace-aware).
fn split_markup_args(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (idx, ch) in args.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            ',' if depth == 0 => {
                parts.push(args[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = args[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn static_resource_key(value: &str) -> Option<String> {
    let (name, args) = markup_extension(value)?;
    if name != "StaticResource" {
        return None;
    }
    for part in split_markup_args(args) {
        match part.split_once('=') {
            None => {
                return if part.is_empty() {
                    None
                } else {
                    Some(part.to_string())
                }
            }
            Some((key, resource)) if key.trim() == "ResourceKey" => {
                let r = resource.trim();
                return if r.is_empty() {
                    None
                } else {
                    Some(r.to_string())
                };
            }
            _ => {}
        }
    }
    None
}

fn binding_refs(value: &str) -> (Option<String>, Option<String>) {
    let Some((name, args)) = markup_extension(value) else {
        return (None, None);
    };
    if name != "Binding" {
        return (None, None);
    }
    let mut path_ref: Option<String> = None;
    let mut converter_ref: Option<String> = None;
    for part in split_markup_args(args) {
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            None => {
                if path_ref.is_none() {
                    path_ref = Some(part.trim().to_string());
                }
            }
            Some((key, raw)) => {
                let key = key.trim();
                let raw = raw.trim();
                if key == "Path" {
                    path_ref = Some(raw.to_string());
                } else if key == "Converter" {
                    converter_ref = static_resource_key(raw);
                }
            }
        }
    }
    if let Some(p) = &path_ref {
        if p.contains('{') || p.contains('}') || p.is_empty() {
            path_ref = None;
        }
    }
    (path_ref, converter_ref)
}

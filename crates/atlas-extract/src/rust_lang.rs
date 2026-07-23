//! Rust extractor — a Rust port of graphify `graphify/extractors/rust.py`.
//!
//! Structs/enums/traits and free functions key off the file stem. `impl` blocks
//! attach their methods to the type node; `impl Trait for T` emits `implements`;
//! trait bounds emit `inherits` (first) / `references` (rest). Struct, tuple-
//! struct and enum-variant field types emit `references`. Call resolution is
//! in-file only, with a trait-method blocklist for cross-file candidates (which
//! we drop in single-file mode).

use crate::{edge_map, is_builtin_global, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(PartialEq, Eq, Clone, Copy)]
enum Role {
    Type,
    Generic,
}

const TYPE_NODES: &[&str] = &[
    "type_identifier", "generic_type", "scoped_type_identifier",
    "reference_type", "primitive_type", "tuple_type", "array_type",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("load rust grammar");
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return ExtractResult { nodes: vec![], edges: vec![] },
    };

    let stem = file_stem(path);
    let mut ex = Rs {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem: stem.clone(),
        file_nid: make_id([stem.as_str()]),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        function_bodies: Vec::new(),
        label_to_nid: HashMap::new(),
        seen_call_pairs: HashSet::new(),
    };

    let root = tree.root_node();
    let label = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let fnid = ex.file_nid.clone();
    ex.add_node(&fnid, &label, 1);
    ex.walk(root, None);

    ex.build_label_map();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (nid, body) in bodies {
        ex.walk_calls(body, &nid);
    }
    ex.clean_dangling();

    ExtractResult { nodes: ex.nodes, edges: ex.edges }
}

struct Rs<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    function_bodies: Vec<(String, Node<'a>)>,
    label_to_nid: HashMap<String, String>,
    seen_call_pairs: HashSet<(String, String)>,
}

impl<'a> Rs<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }
    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if self.seen.insert(nid.to_string()) {
            self.nodes.push(node_map(nid, label, "code", &self.str_path, &format!("L{line}")));
        }
    }
    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, context: Option<&str>, line: usize) {
        self.edges.push(edge_map(src, tgt, relation, context, &self.str_path, &format!("L{line}")));
    }

    fn ensure_named_node(&mut self, name: &str) -> String {
        let nid = make_id([self.stem.as_str(), name]);
        if self.seen.contains(&nid) {
            return nid;
        }
        let nid = make_id([name]);
        if !self.seen.contains(&nid) {
            self.seen.insert(nid.clone());
            self.nodes.push(node_map(&nid, name, "code", "", ""));
        }
        nid
    }

    fn emit_param_return_refs(&mut self, func_node: Node, func_nid: &str, line: usize) {
        if let Some(params) = func_node.child_by_field_name("parameters") {
            for p in kids(params) {
                if p.kind() != "parameter" {
                    continue;
                }
                if let Some(type_node) = p.child_by_field_name("type") {
                    let mut refs = Vec::new();
                    collect_type_refs(self.source, type_node, false, &mut refs);
                    for (name, role) in refs {
                        let ctx = if role == Role::Generic { "generic_arg" } else { "parameter_type" };
                        let tgt = self.ensure_named_node(&name);
                        if tgt != func_nid {
                            self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
                        }
                    }
                }
            }
        }
        if let Some(return_type) = func_node.child_by_field_name("return_type") {
            let mut refs = Vec::new();
            collect_type_refs(self.source, return_type, false, &mut refs);
            for (name, role) in refs {
                let ctx = if role == Role::Generic { "generic_arg" } else { "return_type" };
                let tgt = self.ensure_named_node(&name);
                if tgt != func_nid {
                    self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
                }
            }
        }
    }

    fn walk(&mut self, node: Node<'a>, parent_impl_nid: Option<&str>) {
        match node.kind() {
            "function_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let func_name = self.text(name_node);
                    let line = self.line(node);
                    let func_nid = match parent_impl_nid {
                        Some(p) => {
                            let nid = make_id([p, func_name.as_str()]);
                            self.add_node(&nid, &format!(".{func_name}()"), line);
                            self.add_edge(p, &nid, "method", None, line);
                            nid
                        }
                        None => {
                            let nid = make_id([self.stem.as_str(), func_name.as_str()]);
                            self.add_node(&nid, &format!("{func_name}()"), line);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &nid, "contains", None, line);
                            nid
                        }
                    };
                    self.emit_param_return_refs(node, &func_nid, line);
                    if let Some(body) = node.child_by_field_name("body") {
                        self.function_bodies.push((func_nid, body));
                    }
                }
            }
            "struct_item" | "enum_item" | "trait_item" => {
                let Some(name_node) = node.child_by_field_name("name") else { return };
                let item_name = self.text(name_node);
                let line = self.line(node);
                let item_nid = make_id([self.stem.as_str(), item_name.as_str()]);
                self.add_node(&item_nid, &item_name, line);
                let f = self.file_nid.clone();
                self.add_edge(&f, &item_nid, "contains", None, line);
                match node.kind() {
                    "trait_item" => self.rust_trait_bounds(node, &item_nid, line),
                    "struct_item" => self.rust_struct_fields(node, &item_nid),
                    "enum_item" => self.rust_enum_variants(node, &item_nid),
                    _ => {}
                }
            }
            "impl_item" => {
                let type_node = node.child_by_field_name("type");
                let trait_node = node.child_by_field_name("trait");
                let mut impl_nid: Option<String> = None;
                if let Some(tn) = type_node {
                    let type_name = self.text(tn).trim().to_string();
                    let nid = make_id([self.stem.as_str(), type_name.as_str()]);
                    self.add_node(&nid, &type_name, self.line(node));
                    impl_nid = Some(nid);
                }
                if let (Some(trait_node), Some(inid)) = (trait_node, impl_nid.as_deref()) {
                    let inid = inid.to_string();
                    let mut refs = Vec::new();
                    collect_type_refs(self.source, trait_node, false, &mut refs);
                    for (idx, (ref_name, _)) in refs.into_iter().enumerate() {
                        let tgt = self.ensure_named_node(&ref_name);
                        if tgt == inid {
                            continue;
                        }
                        if idx == 0 {
                            self.add_edge(&inid, &tgt, "implements", None, self.line(node));
                        } else {
                            self.add_edge(&inid, &tgt, "references", Some("generic_arg"), self.line(node));
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    let inid = impl_nid.clone();
                    for child in kids(body) {
                        self.walk(child, inid.as_deref());
                    }
                }
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    let raw = self.text(arg);
                    let clean = raw
                        .split('{')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(':')
                        .trim_end_matches('*')
                        .trim_end_matches(':');
                    let module_name = clean.rsplit("::").next().unwrap_or("").trim();
                    if !module_name.is_empty() {
                        let tgt = make_id([module_name]);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &tgt, "imports_from", Some("import"), self.line(node));
                    }
                }
            }
            _ => {
                for child in kids(node) {
                    self.walk(child, None);
                }
            }
        }
    }

    fn rust_trait_bounds(&mut self, node: Node, item_nid: &str, line: usize) {
        for c in kids(node) {
            if c.kind() != "trait_bounds" {
                continue;
            }
            for sub in kids(c) {
                if !sub.is_named() {
                    continue;
                }
                let mut refs = Vec::new();
                collect_type_refs(self.source, sub, false, &mut refs);
                for (idx, (ref_name, _)) in refs.into_iter().enumerate() {
                    let tgt = self.ensure_named_node(&ref_name);
                    if tgt == item_nid {
                        continue;
                    }
                    if idx == 0 {
                        self.add_edge(item_nid, &tgt, "inherits", None, line);
                    } else {
                        self.add_edge(item_nid, &tgt, "references", Some("generic_arg"), line);
                    }
                }
            }
        }
    }

    fn rust_struct_fields(&mut self, node: Node, item_nid: &str) {
        for c in kids(node) {
            if c.kind() == "field_declaration_list" {
                for field in kids(c) {
                    if field.kind() != "field_declaration" {
                        continue;
                    }
                    let type_node = field.child_by_field_name("type").or_else(|| {
                        kids(field).into_iter().find(|fc| TYPE_NODES.contains(&fc.kind()))
                    });
                    let Some(type_node) = type_node else { continue };
                    let line = self.line(field);
                    self.emit_field_refs(type_node, item_nid, line);
                }
            }
        }
        // Tuple structs: ordered_field_declaration_list.
        for c in kids(node) {
            if c.kind() != "ordered_field_declaration_list" {
                continue;
            }
            let fline = self.line(c);
            for tc in kids(c) {
                if TYPE_NODES.contains(&tc.kind()) {
                    self.emit_field_refs(tc, item_nid, fline);
                }
            }
        }
    }

    fn rust_enum_variants(&mut self, node: Node, item_nid: &str) {
        for c in kids(node) {
            if c.kind() != "enum_variant_list" {
                continue;
            }
            for variant in kids(c) {
                if variant.kind() != "enum_variant" {
                    continue;
                }
                let vline = self.line(variant);
                for vc in kids(variant) {
                    match vc.kind() {
                        "ordered_field_declaration_list" => {
                            for tc in kids(vc) {
                                if TYPE_NODES.contains(&tc.kind()) {
                                    self.emit_field_refs(tc, item_nid, vline);
                                }
                            }
                        }
                        "field_declaration_list" => {
                            for field in kids(vc) {
                                if field.kind() != "field_declaration" {
                                    continue;
                                }
                                if let Some(type_node) = field.child_by_field_name("type") {
                                    self.emit_field_refs(type_node, item_nid, self.line(field));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn emit_field_refs(&mut self, type_node: Node, item_nid: &str, line: usize) {
        let mut refs = Vec::new();
        collect_type_refs(self.source, type_node, false, &mut refs);
        for (ref_name, role) in refs {
            let ctx = if role == Role::Generic { "generic_arg" } else { "field" };
            let tgt = self.ensure_named_node(&ref_name);
            if tgt != item_nid {
                self.add_edge(item_nid, &tgt, "references", Some(ctx), line);
            }
        }
    }

    // ── calls ───────────────────────────────────────────────────────────────
    fn build_label_map(&mut self) {
        for n in &self.nodes {
            let (Some(id), Some(label)) = (n.get("id").and_then(Value::as_str), n.get("label").and_then(Value::as_str)) else { continue };
            let normalised = label.trim_matches(|c| c == '(' || c == ')').trim_start_matches('.');
            self.label_to_nid.insert(normalised.to_string(), id.to_string());
        }
    }

    fn walk_calls(&mut self, node: Node<'a>, caller_nid: &str) {
        if node.kind() == "function_item" {
            return;
        }
        if node.kind() == "call_expression" {
            if let Some(func_node) = node.child_by_field_name("function") {
                let mut callee: Option<String> = None;
                match func_node.kind() {
                    "identifier" => callee = Some(self.text(func_node)),
                    "field_expression" => {
                        if let Some(field) = func_node.child_by_field_name("field") {
                            callee = Some(self.text(field));
                        }
                    }
                    "scoped_identifier" => {
                        if let Some(name) = func_node.child_by_field_name("name") {
                            callee = Some(self.text(name));
                        }
                    }
                    _ => {}
                }
                if let Some(name) = callee {
                    if !name.is_empty() && !is_builtin_global(&name) {
                        if let Some(tgt) = self.label_to_nid.get(&name).cloned() {
                            if tgt != caller_nid && self.seen_call_pairs.insert((caller_nid.to_string(), tgt.clone())) {
                                let line = self.line(node);
                                self.add_edge(caller_nid, &tgt, "calls", Some("call"), line);
                            }
                        }
                    }
                }
            }
        }
        for child in kids(node) {
            self.walk_calls(child, caller_nid);
        }
    }

    fn clean_dangling(&mut self) {
        let valid = &self.seen;
        self.edges.retain(|e| {
            let src = e.get("source").and_then(Value::as_str).unwrap_or("");
            let tgt = e.get("target").and_then(Value::as_str).unwrap_or("");
            let rel = e.get("relation").and_then(Value::as_str).unwrap_or("");
            valid.contains(src) && (valid.contains(tgt) || matches!(rel, "imports" | "imports_from"))
        });
    }
}

fn collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    let role = if generic { Role::Generic } else { Role::Type };
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "primitive_type" => {}
        "type_identifier" => {
            let t = text(node);
            if !t.is_empty() {
                out.push((t, role));
            }
        }
        "scoped_type_identifier" => {
            let full = text(node);
            let t = full.rsplit("::").next().unwrap_or("").to_string();
            if !t.is_empty() {
                out.push((t, role));
            }
        }
        "generic_type" => {
            let name_node = node.child_by_field_name("type").or_else(|| {
                kids(node).into_iter().find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"))
            });
            if let Some(nn) = name_node {
                let full = text(nn);
                let t = full.rsplit("::").next().unwrap_or("").to_string();
                if !t.is_empty() {
                    out.push((t, role));
                }
            }
            for c in kids(node) {
                if c.kind() == "type_arguments" {
                    for arg in kids(c) {
                        if arg.is_named() {
                            collect_type_refs(src, arg, true, out);
                        }
                    }
                }
            }
        }
        "reference_type" | "pointer_type" | "array_type" | "tuple_type" | "slice_type" => {
            for c in kids(node) {
                if c.is_named() {
                    collect_type_refs(src, c, generic, out);
                }
            }
        }
        _ => {
            if node.is_named() {
                for c in kids(node) {
                    if c.is_named() {
                        collect_type_refs(src, c, generic, out);
                    }
                }
            }
        }
    }
}

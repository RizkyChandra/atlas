//! Go extractor — a Rust port of graphify `graphify/extractors/go.py`.
//!
//! Types (structs/interfaces) and their methods are keyed off the *package
//! scope* (the parent directory name), so methods on one type across a package's
//! files share a canonical node; free functions and the file node are keyed off
//! the file stem. Struct fields with a named field emit `references`, embedded
//! (unnamed) fields emit `embeds`; interface embedding emits `embeds`.

use crate::{edge_map, is_builtin_global, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const GO_PREDECLARED: &[&str] = &[
    "bool",
    "byte",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "any",
    "comparable",
];

#[derive(PartialEq, Eq, Clone, Copy)]
enum Role {
    Type,
    Generic,
}

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("load go grammar");
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => {
            return ExtractResult {
                nodes: vec![],
                edges: vec![],
            }
        }
    };

    let stem = file_stem(path);
    // Package scope = parent directory name (graphify: `path.parent.name or stem`).
    let pkg_scope = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| stem.clone());

    let mut ex = Go {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem: stem.clone(),
        pkg_scope,
        file_nid: make_id([stem.as_str()]),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        function_bodies: Vec::new(),
        imported_pkgs: HashSet::new(),
        label_to_nid: HashMap::new(),
        seen_call_pairs: HashSet::new(),
    };

    let root = tree.root_node();
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fnid = ex.file_nid.clone();
    ex.add_node(&fnid, &label, 1);
    ex.walk(root);

    ex.build_label_map();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (nid, body) in bodies {
        ex.walk_calls(body, &nid);
    }
    ex.clean_dangling();

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Go<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    pkg_scope: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    function_bodies: Vec<(String, Node<'a>)>,
    imported_pkgs: HashSet<String>,
    label_to_nid: HashMap<String, String>,
    seen_call_pairs: HashSet<(String, String)>,
}

impl<'a> Go<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }
    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if self.seen.insert(nid.to_string()) {
            self.nodes.push(node_map(
                nid,
                label,
                "code",
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
        context: Option<&str>,
        line: usize,
    ) {
        self.edges.push(edge_map(
            src,
            tgt,
            relation,
            context,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    fn ensure_named_node(&mut self, name: &str) -> String {
        let nid = make_id([self.pkg_scope.as_str(), name]);
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

    fn emit_method_refs(&mut self, func_node: Node, func_nid: &str, line: usize) {
        if let Some(params) = func_node.child_by_field_name("parameters") {
            for p in kids(params) {
                if p.kind() != "parameter_declaration" {
                    continue;
                }
                if let Some(type_node) = p.child_by_field_name("type") {
                    let mut refs = Vec::new();
                    collect_type_refs(self.source, type_node, false, &mut refs);
                    for (name, role) in refs {
                        let ctx = if role == Role::Generic {
                            "generic_arg"
                        } else {
                            "parameter_type"
                        };
                        let tgt = self.ensure_named_node(&name);
                        if tgt != func_nid {
                            self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
                        }
                    }
                }
            }
        }
        if let Some(result) = func_node.child_by_field_name("result") {
            if result.kind() == "parameter_list" {
                for p in kids(result) {
                    if p.kind() != "parameter_declaration" {
                        continue;
                    }
                    let type_node = p
                        .child_by_field_name("type")
                        .or_else(|| kids(p).into_iter().find(|c| c.is_named()));
                    if let Some(tn) = type_node {
                        let mut refs = Vec::new();
                        collect_type_refs(self.source, tn, false, &mut refs);
                        for (name, role) in refs {
                            let ctx = if role == Role::Generic {
                                "generic_arg"
                            } else {
                                "return_type"
                            };
                            let tgt = self.ensure_named_node(&name);
                            if tgt != func_nid {
                                self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
                            }
                        }
                    }
                }
            } else {
                let mut refs = Vec::new();
                collect_type_refs(self.source, result, false, &mut refs);
                for (name, role) in refs {
                    let ctx = if role == Role::Generic {
                        "generic_arg"
                    } else {
                        "return_type"
                    };
                    let tgt = self.ensure_named_node(&name);
                    if tgt != func_nid {
                        self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
                    }
                }
            }
        }
    }

    fn walk(&mut self, node: Node<'a>) {
        match node.kind() {
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let func_name = self.text(name_node);
                    let line = self.line(node);
                    let func_nid = make_id([self.stem.as_str(), func_name.as_str()]);
                    self.add_node(&func_nid, &format!("{func_name}()"), line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &func_nid, "contains", None, line);
                    self.emit_method_refs(node, &func_nid, line);
                    if let Some(body) = node.child_by_field_name("body") {
                        self.function_bodies.push((func_nid, body));
                    }
                }
            }
            "method_declaration" => {
                let mut receiver_type: Option<String> = None;
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    for param in kids(receiver) {
                        if param.kind() == "parameter_declaration" {
                            if let Some(type_node) = param.child_by_field_name("type") {
                                receiver_type = Some(
                                    self.text(type_node)
                                        .trim_start_matches('*')
                                        .trim()
                                        .to_string(),
                                );
                            }
                            break;
                        }
                    }
                }
                let Some(name_node) = node.child_by_field_name("name") else {
                    return;
                };
                let method_name = self.text(name_node);
                let line = self.line(node);
                let method_nid = match receiver_type {
                    Some(rt) => {
                        let parent_nid = make_id([self.pkg_scope.as_str(), rt.as_str()]);
                        self.add_node(&parent_nid, &rt, line);
                        let m = make_id([parent_nid.as_str(), method_name.as_str()]);
                        self.add_node(&m, &format!(".{method_name}()"), line);
                        self.add_edge(&parent_nid, &m, "method", None, line);
                        m
                    }
                    None => {
                        let m = make_id([self.stem.as_str(), method_name.as_str()]);
                        self.add_node(&m, &format!("{method_name}()"), line);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &m, "contains", None, line);
                        m
                    }
                };
                self.emit_method_refs(node, &method_nid, line);
                if let Some(body) = node.child_by_field_name("body") {
                    self.function_bodies.push((method_nid, body));
                }
            }
            "type_declaration" => {
                for child in kids(node) {
                    if child.kind() != "type_spec" {
                        continue;
                    }
                    let Some(name_node) = child.child_by_field_name("name") else {
                        continue;
                    };
                    let type_name = self.text(name_node);
                    let line = self.line(child);
                    let type_nid = make_id([self.pkg_scope.as_str(), type_name.as_str()]);
                    self.add_node(&type_nid, &type_name, line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &type_nid, "contains", None, line);

                    let type_body = kids(child)
                        .into_iter()
                        .find(|tc| matches!(tc.kind(), "struct_type" | "interface_type"));
                    let Some(type_body) = type_body else { continue };
                    if type_body.kind() == "struct_type" {
                        self.go_struct_fields(type_body, &type_nid);
                    } else {
                        self.go_interface_embeds(type_body, &type_nid);
                    }
                }
            }
            "import_declaration" => {
                for child in kids(node) {
                    match child.kind() {
                        "import_spec_list" => {
                            for spec in kids(child) {
                                if spec.kind() == "import_spec" {
                                    self.go_import_spec(spec);
                                }
                            }
                        }
                        "import_spec" => self.go_import_spec(child),
                        _ => {}
                    }
                }
            }
            _ => {
                for child in kids(node) {
                    self.walk(child);
                }
            }
        }
    }

    fn go_struct_fields(&mut self, type_body: Node, type_nid: &str) {
        for fdl in kids(type_body) {
            if fdl.kind() != "field_declaration_list" {
                continue;
            }
            for field in kids(fdl) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                let has_name = kids(field).iter().any(|fc| fc.kind() == "field_identifier");
                let type_node = field.child_by_field_name("type").or_else(|| {
                    kids(field)
                        .into_iter()
                        .find(|fc| fc.is_named() && fc.kind() != "field_identifier")
                });
                let Some(type_node) = type_node else { continue };
                let line = self.line(field);
                let mut refs = Vec::new();
                collect_type_refs(self.source, type_node, false, &mut refs);
                for (ref_name, role) in refs {
                    let tgt = self.ensure_named_node(&ref_name);
                    if tgt == type_nid {
                        continue;
                    }
                    if !has_name && role == Role::Type {
                        self.add_edge(type_nid, &tgt, "embeds", None, line);
                    } else {
                        let ctx = if role == Role::Generic {
                            "generic_arg"
                        } else {
                            "field"
                        };
                        self.add_edge(type_nid, &tgt, "references", Some(ctx), line);
                    }
                }
            }
        }
    }

    fn go_interface_embeds(&mut self, type_body: Node, type_nid: &str) {
        for elem in kids(type_body) {
            if elem.kind() != "type_elem" {
                continue;
            }
            let line = self.line(elem);
            let mut refs = Vec::new();
            for sub in kids(elem) {
                if sub.is_named() {
                    collect_type_refs(self.source, sub, false, &mut refs);
                }
            }
            for (ref_name, role) in refs {
                let tgt = self.ensure_named_node(&ref_name);
                if tgt == type_nid {
                    continue;
                }
                if role == Role::Type {
                    self.add_edge(type_nid, &tgt, "embeds", None, line);
                } else {
                    self.add_edge(type_nid, &tgt, "references", Some("generic_arg"), line);
                }
            }
        }
    }

    fn go_import_spec(&mut self, spec: Node) {
        let Some(path_node) = spec.child_by_field_name("path") else {
            return;
        };
        let raw = self.text(path_node);
        let raw = raw.trim_matches('"');
        let tgt = make_id(["go", "pkg", raw]);
        let f = self.file_nid.clone();
        self.add_edge(&f, &tgt, "imports_from", Some("import"), self.line(spec));
        let local = spec
            .child_by_field_name("name")
            .map(|a| self.text(a))
            .unwrap_or_else(|| raw.rsplit('/').next().unwrap_or("").to_string());
        if !local.is_empty() && local != "_" && local != "." {
            self.imported_pkgs.insert(local);
        }
    }

    // ── calls ───────────────────────────────────────────────────────────────
    fn build_label_map(&mut self) {
        for n in &self.nodes {
            let (Some(id), Some(label)) = (
                n.get("id").and_then(Value::as_str),
                n.get("label").and_then(Value::as_str),
            ) else {
                continue;
            };
            let normalised = label
                .trim_matches(|c| c == '(' || c == ')')
                .trim_start_matches('.');
            self.label_to_nid
                .insert(normalised.to_string(), id.to_string());
        }
    }

    fn walk_calls(&mut self, node: Node<'a>, caller_nid: &str) {
        if matches!(node.kind(), "function_declaration" | "method_declaration") {
            return;
        }
        if node.kind() == "call_expression" {
            if let Some(func_node) = node.child_by_field_name("function") {
                let mut callee: Option<String> = None;
                match func_node.kind() {
                    "identifier" => callee = Some(self.text(func_node)),
                    "selector_expression" => {
                        // Package-qualified call resolves cross-file; receiver
                        // method call has no import evidence and is skipped. Here
                        // single-file: no in-file match either way, so just read
                        // the field name for the in-file label lookup.
                        if let Some(field) = func_node.child_by_field_name("field") {
                            callee = Some(self.text(field));
                        }
                    }
                    _ => {}
                }
                if let Some(name) = callee {
                    if !name.is_empty() && !is_builtin_global(&name) {
                        if let Some(tgt) = self.label_to_nid.get(&name).cloned() {
                            if tgt != caller_nid
                                && self
                                    .seen_call_pairs
                                    .insert((caller_nid.to_string(), tgt.clone()))
                            {
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

    /// graphify drops edges whose endpoints aren't real nodes (except imports).
    fn clean_dangling(&mut self) {
        let valid = &self.seen;
        self.edges.retain(|e| {
            let src = e.get("source").and_then(Value::as_str).unwrap_or("");
            let tgt = e.get("target").and_then(Value::as_str).unwrap_or("");
            let rel = e.get("relation").and_then(Value::as_str).unwrap_or("");
            valid.contains(src)
                && (valid.contains(tgt) || matches!(rel, "imports" | "imports_from"))
        });
    }
}

fn collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    let role = if generic { Role::Generic } else { Role::Type };
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "type_identifier" => {
            let t = text(node);
            if !t.is_empty() && !GO_PREDECLARED.contains(&t.as_str()) {
                out.push((t, role));
            }
        }
        "qualified_type" => {
            let full = text(node);
            let t = full.rsplit('.').next().unwrap_or("").to_string();
            if !t.is_empty() && !GO_PREDECLARED.contains(&t.as_str()) {
                out.push((t, role));
            }
        }
        "generic_type" => {
            if let Some(type_field) = node.child_by_field_name("type") {
                collect_type_refs(src, type_field, generic, out);
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
        "pointer_type" | "slice_type" | "array_type" | "map_type" | "channel_type"
        | "parenthesized_type" => {
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

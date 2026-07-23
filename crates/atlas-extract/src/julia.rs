//! Julia extractor — a Rust port of graphify `graphify/extractors/julia.py`.
//!
//! Extracts modules (`defines`), structs (fields → `references[field]`, `<:` →
//! `inherits`), abstract types, functions and short-form functions (`defines`),
//! `using`/`import` (bare / scoped / relative / selected forms → `imports`), and
//! in-file `calls` (direct + `obj.method(...)`). Cross-file type/callee names
//! become SOURCELESS stubs (`ensure_named_node`), collapsed by graphify's
//! corpus rewire — out of single-file scope, so they stay dangling here.
//!
//! Node ids key off the file stem to match graphify's *built* graph (the Python
//! extractor keys the file node off `str(path)`, then the build remaps it to the
//! stem form — we emit that post-build id directly).

use crate::{edge_map, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_julia::LANGUAGE.into())
        .expect("load julia grammar");
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
    let file_nid = make_id([stem.as_str()]);
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut jl = Julia {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        bodies: Vec::new(),
    };
    jl.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    let fnid = jl.file_nid.clone();
    jl.walk(root, &fnid);

    // Call pass.
    let bodies = std::mem::take(&mut jl.bodies);
    for (func_nid, body, is_func_def) in bodies {
        if is_func_def {
            for child in kids(body) {
                if child.kind() != "signature" {
                    jl.walk_calls(child, &func_nid);
                }
            }
        } else {
            jl.walk_calls(body, &func_nid);
        }
    }

    ExtractResult {
        nodes: jl.nodes,
        edges: jl.edges,
    }
}

struct Julia<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    /// (func_nid, body_node, is_function_definition)
    bodies: Vec<(String, Node<'a>, bool)>,
}

impl<'a> Julia<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if !self.seen.insert(nid.to_string()) {
            return;
        }
        self.nodes.push(node_map(
            nid,
            label,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, ctx: Option<&str>, line: usize) {
        self.edges.push(edge_map(
            src,
            tgt,
            relation,
            ctx,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// graphify `ensure_named_node`: stem-scoped id if already defined, else a
    /// bare-name SOURCELESS stub for corpus rewire.
    fn ensure_named_node(&mut self, name: &str) -> String {
        let scoped = make_id([self.stem.as_str(), name]);
        if self.seen.contains(&scoped) {
            return scoped;
        }
        let bare = make_id([name]);
        if self.seen.insert(bare.clone()) {
            self.nodes.push(node_map(&bare, name, "code", "", ""));
        }
        bare
    }

    fn func_name_from_signature(&self, sig: Node) -> Option<String> {
        for child in kids(sig) {
            if child.kind() == "call_expression" {
                if let Some(callee) = child.child(0) {
                    if callee.kind() == "identifier" {
                        return Some(self.text(callee));
                    }
                }
            }
        }
        None
    }

    fn julia_mod_name(&self, n: Node) -> Option<String> {
        match n.kind() {
            "import_path" => kids(n)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .next_back()
                .map(|c| self.text(c)),
            "identifier" | "scoped_identifier" => Some(self.text(n)),
            _ => None,
        }
    }

    fn emit_import(&mut self, name: Option<String>, scope_nid: &str, line: usize) {
        let Some(name) = name else { return };
        if name.is_empty() {
            return;
        }
        let imp_nid = make_id([name.as_str()]);
        self.add_node(&imp_nid, &name, line);
        self.add_edge(scope_nid, &imp_nid, "imports", Some("import"), line);
    }

    fn walk(&mut self, node: Node<'a>, scope_nid: &str) {
        let t = node.kind();
        let line = node.start_position().row + 1;

        match t {
            "module_definition" => {
                let name_node = kids(node).into_iter().find(|c| c.kind() == "identifier");
                if let Some(nn) = name_node {
                    let mod_name = self.text(nn);
                    let mod_nid = make_id([self.stem.as_str(), mod_name.as_str()]);
                    self.add_node(&mod_nid, &mod_name, line);
                    self.add_edge(scope_nid, &mod_nid, "defines", None, line);
                    for child in kids(node) {
                        self.walk(child, &mod_nid);
                    }
                }
            }
            "struct_definition" => {
                let Some(type_head) = kids(node).into_iter().find(|c| c.kind() == "type_head")
                else {
                    return;
                };
                let mut struct_name: Option<String> = None;
                let mut super_name: Option<String> = None;
                if let Some(bin) = kids(type_head)
                    .into_iter()
                    .find(|c| c.kind() == "binary_expression")
                {
                    let ids: Vec<Node> = kids(bin)
                        .into_iter()
                        .filter(|c| c.kind() == "identifier")
                        .collect();
                    if let Some(first) = ids.first() {
                        struct_name = Some(self.text(*first));
                        if ids.len() >= 2 {
                            super_name = Some(self.text(*ids.last().unwrap()));
                        }
                    }
                } else if let Some(nn) = kids(type_head)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                {
                    struct_name = Some(self.text(nn));
                }
                let Some(struct_name) = struct_name else {
                    return;
                };
                let struct_nid = make_id([self.stem.as_str(), struct_name.as_str()]);
                self.add_node(&struct_nid, &struct_name, line);
                self.add_edge(scope_nid, &struct_nid, "defines", None, line);
                if let Some(sup) = super_name {
                    let tgt = self.ensure_named_node(&sup);
                    self.add_edge(&struct_nid, &tgt, "inherits", None, line);
                }
                for child in kids(node) {
                    if child.kind() == "typed_expression" {
                        let type_ids: Vec<Node> = kids(child)
                            .into_iter()
                            .filter(|c| c.kind() == "identifier")
                            .collect();
                        if type_ids.len() >= 2 {
                            let field_line = child.start_position().row + 1;
                            let type_name = self.text(*type_ids.last().unwrap());
                            let tgt = self.ensure_named_node(&type_name);
                            self.add_edge(
                                &struct_nid,
                                &tgt,
                                "references",
                                Some("field"),
                                field_line,
                            );
                        }
                    }
                }
            }
            "abstract_definition" => {
                if let Some(type_head) = kids(node).into_iter().find(|c| c.kind() == "type_head") {
                    if let Some(nn) = kids(type_head)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                    {
                        let abs_name = self.text(nn);
                        let abs_nid = make_id([self.stem.as_str(), abs_name.as_str()]);
                        self.add_node(&abs_nid, &abs_name, line);
                        self.add_edge(scope_nid, &abs_nid, "defines", None, line);
                    }
                }
            }
            "function_definition" => {
                if let Some(sig) = kids(node).into_iter().find(|c| c.kind() == "signature") {
                    if let Some(func_name) = self.func_name_from_signature(sig) {
                        let func_nid = make_id([self.stem.as_str(), func_name.as_str()]);
                        self.add_node(&func_nid, &format!("{func_name}()"), line);
                        self.add_edge(scope_nid, &func_nid, "defines", None, line);
                        self.bodies.push((func_nid, node, true));
                    }
                }
            }
            "assignment" => {
                let children = kids(node);
                if let Some(lhs) = children.first() {
                    if lhs.kind() == "call_expression" {
                        if let Some(callee) = lhs.child(0) {
                            if callee.kind() == "identifier" {
                                let func_name = self.text(callee);
                                let func_nid = make_id([self.stem.as_str(), func_name.as_str()]);
                                self.add_node(&func_nid, &format!("{func_name}()"), line);
                                self.add_edge(scope_nid, &func_nid, "defines", None, line);
                                if children.len() >= 3 {
                                    let rhs = *children.last().unwrap();
                                    self.bodies.push((func_nid, rhs, false));
                                }
                            }
                        }
                    }
                }
            }
            "using_statement" | "import_statement" => {
                for child in kids(node) {
                    match child.kind() {
                        "identifier" | "scoped_identifier" | "import_path" => {
                            let name = self.julia_mod_name(child);
                            self.emit_import(name, scope_nid, line);
                        }
                        "selected_import" => {
                            if let Some(pkg) = kids(child).into_iter().find(|c| {
                                matches!(
                                    c.kind(),
                                    "identifier" | "scoped_identifier" | "import_path"
                                )
                            }) {
                                let name = self.julia_mod_name(pkg);
                                self.emit_import(name, scope_nid, line);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                for child in kids(node) {
                    self.walk(child, scope_nid);
                }
            }
        }
    }

    fn walk_calls(&mut self, node: Node, func_nid: &str) {
        let t = node.kind();
        if t == "function_definition" || t == "short_function_definition" {
            return;
        }
        if t == "call_expression" {
            if let Some(callee) = node.child(0) {
                let line = node.start_position().row + 1;
                if callee.kind() == "identifier" {
                    let name = self.text(callee);
                    let tgt = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_edge(func_nid, &tgt, "calls", Some("call"), line);
                } else if callee.kind() == "field_expression" && callee.child_count() >= 3 {
                    let method = callee.child(callee.child_count() - 1).unwrap();
                    let name = self.text(method);
                    let tgt = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_edge(func_nid, &tgt, "calls", Some("call"), line);
                }
            }
        }
        for child in kids(node) {
            self.walk_calls(child, func_nid);
        }
    }
}

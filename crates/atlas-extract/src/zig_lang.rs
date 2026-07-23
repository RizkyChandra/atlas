//! Zig extractor — a Rust port of graphify `graphify/extractors/zig.py`.
//!
//! Emits a file node, struct/enum/union type nodes (`contains`), struct methods
//! (`method`), free functions (`contains`), `@import`/`@cImport` module import
//! edges (`imports_from`, dangling target), and in-file `calls` edges resolved by
//! matching the callee's bare name against a defined function/method label.
//!
//! Node ids key off the file stem to match graphify's *built* graph (its
//! extractor keys off `make_id(str(path))`, then the build relativizes to the
//! stem — see `engine.rs`).
//!
//! Out of scope (single-file): member calls (`std.math.sqrt`) resolve to no
//! in-file label and emit no edge — matching the oracle.

use crate::{node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_zig::LANGUAGE.into())
        .expect("load zig grammar");
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

    let mut ex = Zig {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        labels: Vec::new(),
        function_bodies: Vec::new(),
    };

    ex.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    ex.walk(root, None);

    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (caller, body) in bodies {
        ex.walk_calls(body, &caller, &mut seen_pairs);
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Zig<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    /// (label, id) in insertion order — call resolution takes the first match.
    labels: Vec<(String, String)>,
    function_bodies: Vec<(String, Node<'a>)>,
}

impl<'a> Zig<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if !self.seen.insert(nid.to_string()) {
            return;
        }
        self.labels.push((label.to_string(), nid.to_string()));
        self.nodes.push(node_map(
            nid,
            label,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, line: usize) {
        self.edges.push(crate::edge_map(
            src,
            tgt,
            relation,
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// graphify `_extract_import`: follow `@import`/`@cImport` builtin calls,
    /// descending one field_expression (`@import("std").mem`), and emit a file
    /// `imports_from` edge keyed to the module basename-stem.
    fn extract_import(&mut self, node: Node, line: usize) {
        for child in crate::kids(node) {
            if child.kind() == "builtin_function" {
                let mut bi = None;
                let mut args = None;
                for c in crate::kids(child) {
                    match c.kind() {
                        "builtin_identifier" => bi = Some(self.text(c)),
                        "arguments" => args = Some(c),
                        _ => {}
                    }
                }
                if let (Some(bi), Some(args)) = (bi.as_deref(), args) {
                    if bi == "@import" || bi == "@cImport" {
                        for arg in crate::kids(args) {
                            if matches!(arg.kind(), "string_literal" | "string") {
                                let raw = self.text(arg);
                                let raw = raw.trim_matches('"');
                                let module = raw.rsplit('/').next().unwrap_or(raw);
                                let module = module.split('.').next().unwrap_or(module);
                                if !module.is_empty() {
                                    let tgt = make_id([module]);
                                    let f = self.file_nid.clone();
                                    self.add_edge(&f, &tgt, "imports_from", line);
                                }
                                return;
                            }
                        }
                    }
                }
            } else if child.kind() == "field_expression" {
                self.extract_import(child, line);
                return;
            }
        }
    }

    fn walk(&mut self, node: Node<'a>, parent_struct_nid: Option<&str>) {
        match node.kind() {
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let func_name = self.text(name_node);
                    let line = node.start_position().row + 1;
                    let func_nid = match parent_struct_nid {
                        Some(p) => {
                            let nid = make_id([p, func_name.as_str()]);
                            self.add_node(&nid, &format!(".{func_name}()"), line);
                            self.add_edge(p, &nid, "method", line);
                            nid
                        }
                        None => {
                            let nid = make_id([self.stem.as_str(), func_name.as_str()]);
                            self.add_node(&nid, &format!("{func_name}()"), line);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &nid, "contains", line);
                            nid
                        }
                    };
                    if let Some(body) = node.child_by_field_name("body") {
                        self.function_bodies.push((func_nid, body));
                    }
                }
            }
            "variable_declaration" => {
                let mut name_node = None;
                let mut value_node = None;
                for child in crate::kids(node) {
                    match child.kind() {
                        "identifier" => name_node = Some(child),
                        "struct_declaration" | "enum_declaration" | "union_declaration"
                        | "builtin_function" | "field_expression" => value_node = Some(child),
                        _ => {}
                    }
                }
                let line = node.start_position().row + 1;
                match value_node.map(|v| (v.kind(), v)) {
                    Some(("struct_declaration", value)) => {
                        if let Some(nn) = name_node {
                            let struct_name = self.text(nn);
                            let nid = make_id([self.stem.as_str(), struct_name.as_str()]);
                            self.add_node(&nid, &struct_name, line);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &nid, "contains", line);
                            for child in crate::kids(value) {
                                self.walk(child, Some(&nid));
                            }
                        }
                    }
                    Some(("enum_declaration" | "union_declaration", _)) => {
                        if let Some(nn) = name_node {
                            let type_name = self.text(nn);
                            let nid = make_id([self.stem.as_str(), type_name.as_str()]);
                            self.add_node(&nid, &type_name, line);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &nid, "contains", line);
                        }
                    }
                    Some(("builtin_function" | "field_expression", _)) => {
                        self.extract_import(node, line);
                    }
                    _ => {}
                }
            }
            _ => {
                for child in crate::kids(node) {
                    self.walk(child, parent_struct_nid);
                }
            }
        }
    }

    fn walk_calls(
        &mut self,
        node: Node,
        caller_nid: &str,
        seen_pairs: &mut HashSet<(String, String)>,
    ) {
        if node.kind() == "function_declaration" {
            return;
        }
        if node.kind() == "call_expression" {
            if let Some(fn_node) = node.child_by_field_name("function") {
                let fn_text = self.text(fn_node);
                let callee = fn_text.rsplit('.').next().unwrap_or(&fn_text);
                // First node whose label is `{callee}()` or `.{callee}()`.
                let want_a = format!("{callee}()");
                let want_b = format!(".{callee}()");
                let tgt = self
                    .labels
                    .iter()
                    .find(|(label, _)| *label == want_a || *label == want_b)
                    .map(|(_, id)| id.clone());
                if let Some(tgt) = tgt {
                    if tgt != caller_nid {
                        let pair = (caller_nid.to_string(), tgt.clone());
                        if seen_pairs.insert(pair) {
                            let line = node.start_position().row + 1;
                            self.add_edge(caller_nid, &tgt, "calls", line);
                        }
                    }
                }
            }
        }
        for child in crate::kids(node) {
            self.walk_calls(child, caller_nid, seen_pairs);
        }
    }
}

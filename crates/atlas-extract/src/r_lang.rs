//! R extractor (tree-sitter-r grammar). BACKLOG new language — graphify has no
//! R extractor, so there is no oracle; the contract is the hand-authored golden
//! in tests/langs.rs.
//!
//! Nodes: file, top-level functions (`name <- function(...)`, label `name()`).
//! Edges: `contains` (file → function), `imports` (`library(pkg)` /
//! `require(pkg)` → `pkg`), `calls` (function → file-local function). R has no
//! classes/inheritance, so no `inherits` edge (functions + library imports).
//! Calls target only functions defined in this file (avoids builtin god-nodes),
//! mirroring the single-file scope of the other standalone extractors.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("load r grammar");
    let Some(tree) = parser.parse(source, None) else {
        return ExtractResult {
            nodes: vec![],
            edges: vec![],
        };
    };

    let stem = file_stem(path);
    let file_nid = make_id([stem.as_str()]);
    let str_path = path.to_string_lossy().into_owned();
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut ex = R {
        source,
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        defs: HashMap::new(),
    };
    ex.nodes
        .push(file_node(&file_nid, &file_label, &ex.str_path));

    let root = tree.root_node();
    // Pass 1: collect top-level function definitions.
    for child in crate::kids(root) {
        if let Some((name, _fdef)) = ex.assigned_function(child) {
            let nid = make_id([ex.stem.as_str(), name.as_str()]);
            ex.defs.insert(name, nid);
        }
    }
    // Pass 2: emit function nodes + imports + calls.
    ex.imports(root);
    for child in crate::kids(root) {
        if let Some((name, fdef)) = ex.assigned_function(child) {
            let nid = ex.defs[&name].clone();
            ex.add_fn(&nid, &name, child.start_position().row + 1);
            ex.calls_in(fdef, &nid);
        }
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

fn file_node(id: &str, label: &str, source_file: &str) -> Attrs {
    let mut m = node_map(id, label, "code", source_file, "");
    m.insert("source_location".into(), serde_json::Value::Null);
    m
}

struct R<'a> {
    source: &'a [u8],
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    defs: HashMap<String, String>, // fn name → nid
}

impl<'a> R<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    /// If `node` is `ident <- function(...) {...}` (or `<<-` / `=`), return
    /// (name, function_definition node).
    fn assigned_function(&self, node: Node<'a>) -> Option<(String, Node<'a>)> {
        if node.kind() != "binary_operator" {
            return None;
        }
        let kids = crate::kids(node);
        let op = kids
            .iter()
            .find(|c| matches!(c.kind(), "<-" | "<<-" | "="))?;
        let lhs = kids.iter().take_while(|c| c.id() != op.id()).last()?;
        let rhs = kids.iter().skip_while(|c| c.id() != op.id()).nth(1)?;
        if lhs.kind() == "identifier" && rhs.kind() == "function_definition" {
            Some((self.read(*lhs), *rhs))
        } else {
            None
        }
    }

    fn add_fn(&mut self, nid: &str, name: &str, line: usize) {
        self.nodes.push(node_map(
            nid,
            &format!("{name}()"),
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
        self.edges.push(edge_map(
            &self.file_nid,
            nid,
            "contains",
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// `library(pkg)` / `require(pkg)` anywhere → file `imports` pkg.
    fn imports(&mut self, node: Node<'a>) {
        if node.kind() == "call" {
            let kids = crate::kids(node);
            if let (Some(head), Some(args)) = (kids.first(), kids.get(1)) {
                if head.kind() == "identifier"
                    && matches!(self.read(*head).as_str(), "library" | "require")
                {
                    if let Some(pkg) = crate::kids(*args)
                        .into_iter()
                        .find(|a| a.kind() == "argument")
                        .and_then(|a| {
                            crate::kids(a)
                                .into_iter()
                                .find(|c| c.kind() == "identifier")
                        })
                        .map(|c| self.read(c))
                    {
                        let tgt = make_id([pkg.as_str()]);
                        let line = node.start_position().row + 1;
                        self.edges.push(edge_map(
                            &self.file_nid,
                            &tgt,
                            "imports",
                            None,
                            &self.str_path,
                            &format!("L{line}"),
                        ));
                    }
                }
            }
        }
        for c in crate::kids(node) {
            self.imports(c);
        }
    }

    /// Emit `calls` edges from `caller_nid` to file-local functions called
    /// inside `node`.
    fn calls_in(&mut self, node: Node<'a>, caller_nid: &str) {
        if node.kind() == "call" {
            if let Some(head) = crate::kids(node).into_iter().next() {
                if head.kind() == "identifier" {
                    let callee = self.read(head);
                    if let Some(tgt) = self.defs.get(&callee).cloned() {
                        if tgt != caller_nid {
                            let line = node.start_position().row + 1;
                            self.edges.push(edge_map(
                                caller_nid,
                                &tgt,
                                "calls",
                                None,
                                &self.str_path,
                                &format!("L{line}"),
                            ));
                        }
                    }
                }
            }
        }
        for c in crate::kids(node) {
            self.calls_in(c, caller_nid);
        }
    }
}

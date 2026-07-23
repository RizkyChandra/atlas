//! Ada extractor (tree-sitter-ada grammar). BACKLOG new language — graphify has
//! no Ada extractor, so there is no oracle; the contract is the hand-authored
//! golden in tests/langs.rs.
//!
//! Nodes: file, packages (`package [body] Name`, label = name), subprograms
//! (functions/procedures, label `name()`). Edges: `contains` (file → package,
//! package → subprogram), `imports` (`with Ada.Text_IO;` → `ada_text_io`),
//! `calls` (subprogram → file-local subprogram). `with` clauses are the Ada
//! import mechanism (the requested import/type edge).

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ada::LANGUAGE.into())
        .expect("load ada grammar");
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

    let mut ex = Ada {
        source,
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        subs: HashSet::new(),
    };
    ex.nodes
        .push(file_node(&file_nid, &file_label, &ex.str_path));

    let root = tree.root_node();
    ex.collect_subs(root);
    for unit in crate::kids(root)
        .into_iter()
        .filter(|c| c.kind() == "compilation_unit")
    {
        for item in crate::kids(unit) {
            match item.kind() {
                "with_clause" => ex.handle_with(item),
                "package_body" | "package_declaration" | "package_specification" => {
                    ex.handle_package(item)
                }
                "subprogram_body" => {
                    let owner = ex.file_nid.clone();
                    ex.handle_subprogram(item, &owner);
                }
                _ => {}
            }
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

struct Ada<'a> {
    source: &'a [u8],
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    subs: HashSet<String>, // subprogram names defined in this file
}

impl<'a> Ada<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }
    fn first_ident(&self, n: Node<'a>) -> Option<String> {
        crate::kids(n)
            .into_iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| self.read(c))
    }

    fn add_edge(&mut self, src: &str, tgt: &str, rel: &str, line: usize) {
        self.edges.push(edge_map(
            src,
            tgt,
            rel,
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// Name from a `subprogram_body`'s function/procedure specification.
    fn sub_name(&self, body: Node<'a>) -> Option<String> {
        let spec = crate::kids(body).into_iter().find(|c| {
            matches!(
                c.kind(),
                "function_specification" | "procedure_specification"
            )
        })?;
        self.first_ident(spec)
    }

    fn collect_subs(&mut self, node: Node<'a>) {
        if node.kind() == "subprogram_body" {
            if let Some(name) = self.sub_name(node) {
                self.subs.insert(name);
            }
        }
        for c in crate::kids(node) {
            self.collect_subs(c);
        }
    }

    fn handle_with(&mut self, node: Node<'a>) {
        let line = self.line(node);
        // `with A.B, C;` — one imports edge per unit name.
        for c in crate::kids(node) {
            let name = match c.kind() {
                "selected_component" | "identifier" => self.read(c),
                _ => continue,
            };
            let tgt = make_id([name.as_str()]);
            self.add_edge(&self.file_nid.clone(), &tgt, "imports", line);
        }
    }

    fn handle_package(&mut self, node: Node<'a>) {
        let Some(pname) = self.first_ident(node) else {
            return;
        };
        let line = self.line(node);
        let pnid = make_id([self.stem.as_str(), pname.as_str()]);
        self.nodes.push(node_map(
            &pnid,
            &pname,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
        self.add_edge(&self.file_nid.clone(), &pnid, "contains", line);

        // Subprograms declared directly in the package's declarative part.
        for part in crate::kids(node)
            .into_iter()
            .filter(|c| c.kind() == "non_empty_declarative_part")
        {
            for decl in crate::kids(part) {
                if decl.kind() == "subprogram_body" {
                    self.handle_subprogram(decl, &pnid.clone());
                }
            }
        }
    }

    fn handle_subprogram(&mut self, node: Node<'a>, owner_nid: &str) {
        let Some(sname) = self.sub_name(node) else {
            return;
        };
        let line = self.line(node);
        let snid = make_id([self.stem.as_str(), sname.as_str()]);
        self.nodes.push(node_map(
            &snid,
            &format!("{sname}()"),
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
        self.add_edge(owner_nid, &snid, "contains", line);
        self.calls_in(node, &snid);
    }

    /// `calls` edges to file-local subprograms invoked inside `node`.
    fn calls_in(&mut self, node: Node<'a>, caller_nid: &str) {
        let callee = match node.kind() {
            "function_call" => self.first_ident(node),
            "procedure_call_statement" => {
                crate::kids(node)
                    .into_iter()
                    .next()
                    .and_then(|c| match c.kind() {
                        "identifier" => Some(self.read(c)),
                        // `A.B.Put_Line` → last identifier (the called name).
                        "selected_component" => self.last_ident(c),
                        _ => None,
                    })
            }
            _ => None,
        };
        if let Some(callee) = callee {
            if self.subs.contains(&callee) {
                let tgt = make_id([self.stem.as_str(), callee.as_str()]);
                if tgt != caller_nid {
                    self.add_edge(caller_nid, &tgt, "calls", self.line(node));
                }
            }
        }
        for c in crate::kids(node) {
            // Don't descend into nested subprogram bodies (their calls belong to them).
            if c.kind() != "subprogram_body" {
                self.calls_in(c, caller_nid);
            }
        }
    }

    fn last_ident(&self, n: Node<'a>) -> Option<String> {
        crate::kids(n)
            .into_iter()
            .filter(|c| c.kind() == "identifier")
            .last()
            .map(|c| self.read(c))
    }
}

//! Fortran extractor — a Rust port of graphify `graphify/extractors/fortran.py`.
//!
//! Extracts programs/modules (`defines`), derived types (`defines`),
//! subroutines/functions (`defines`, label `name()`), `use` (`imports`), derived-
//! type parameter/return references (`references[parameter_type|return_type]`),
//! and in-file `calls` (`call foo` + `x = foo(...)` where `foo` is a defined
//! procedure). Fortran is case-insensitive: names are lowercased.
//!
//! Node ids key off the file stem to match graphify's *built* graph.
//!
//! #2092: graphify preprocesses capital-`.F90` with `cpp -P`, which renumbers
//! lines and corrupts `source_location`. Our fixture is a plain lowercase `.f90`
//! (no preprocessing), so this oracle is clean and we match it exactly. We do NOT
//! run cpp — a `.F90` path would need it to match graphify's (buggy) line anchors.

use crate::{edge_map, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_fortran::LANGUAGE.into())
        .expect("load fortran grammar");
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

    let mut f = Fortran {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        scope_bodies: Vec::new(),
    };
    f.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    let fnid = f.file_nid.clone();
    f.walk(root, &fnid);

    // Call pass: walk each scope body, skipping the header statement node.
    const HEADERS: &[&str] = &[
        "subroutine_statement",
        "function_statement",
        "program_statement",
        "module_statement",
    ];
    let bodies = std::mem::take(&mut f.scope_bodies);
    for (scope_nid, body) in bodies {
        for child in kids(body) {
            if !HEADERS.contains(&child.kind()) {
                f.walk_calls(child, &scope_nid);
            }
        }
    }

    ExtractResult {
        nodes: f.nodes,
        edges: f.edges,
    }
}

struct Fortran<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    scope_bodies: Vec<(String, Node<'a>)>,
}

impl<'a> Fortran<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn text_lower(&self, n: Node) -> String {
        self.text(n).to_lowercase()
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

    fn fortran_name(&self, stmt: Node) -> Option<String> {
        kids(stmt)
            .into_iter()
            .find(|c| matches!(c.kind(), "name" | "identifier"))
            .map(|c| self.text_lower(c))
    }

    fn emit_signature_refs(&mut self, scope_node: Node, fn_nid: &str, is_function: bool) {
        let stmt_type = if is_function {
            "function_statement"
        } else {
            "subroutine_statement"
        };
        let Some(stmt) = kids(scope_node).into_iter().find(|c| c.kind() == stmt_type) else {
            return;
        };
        let mut param_names: HashSet<String> = HashSet::new();
        if let Some(params) = kids(stmt).into_iter().find(|c| c.kind() == "parameters") {
            for c in kids(params) {
                if c.kind() == "identifier" {
                    param_names.insert(self.text_lower(c));
                }
            }
        }
        let mut result_name: Option<String> = None;
        if is_function {
            if let Some(res) = kids(stmt)
                .into_iter()
                .find(|c| c.kind() == "function_result")
            {
                if let Some(rid) = kids(res).into_iter().find(|c| c.kind() == "identifier") {
                    result_name = Some(self.text_lower(rid));
                }
            } else {
                result_name = self.fortran_name(stmt);
            }
        }
        for child in kids(scope_node) {
            if child.kind() != "variable_declaration" {
                continue;
            }
            let Some(derived) = kids(child).into_iter().find(|c| c.kind() == "derived_type") else {
                continue;
            };
            let Some(tn) = kids(derived).into_iter().find(|c| c.kind() == "type_name") else {
                continue;
            };
            let type_name = self.text_lower(tn);
            for var in kids(child) {
                if var.kind() != "identifier" {
                    continue;
                }
                let var_name = self.text_lower(var);
                let var_line = var.start_position().row + 1;
                if param_names.contains(&var_name) {
                    let tgt = self.ensure_named_node(&type_name);
                    if tgt != fn_nid {
                        self.add_edge(fn_nid, &tgt, "references", Some("parameter_type"), var_line);
                    }
                } else if is_function && Some(&var_name) == result_name.as_ref() {
                    let tgt = self.ensure_named_node(&type_name);
                    if tgt != fn_nid {
                        self.add_edge(fn_nid, &tgt, "references", Some("return_type"), var_line);
                    }
                }
            }
        }
    }

    fn walk(&mut self, node: Node<'a>, scope_nid: &str) {
        let t = node.kind();
        let line = node.start_position().row + 1;
        match t {
            "program" => {
                let name = kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "program_statement")
                    .and_then(|s| self.fortran_name(s));
                if let Some(name) = name {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &name, line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &nid, "defines", None, line);
                    self.scope_bodies.push((nid.clone(), node));
                    for child in kids(node) {
                        self.walk(child, &nid);
                    }
                }
            }
            "module" => {
                let name = kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "module_statement")
                    .and_then(|s| self.fortran_name(s));
                if let Some(name) = name {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &name, line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &nid, "defines", None, line);
                    for child in kids(node) {
                        self.walk(child, &nid);
                    }
                }
            }
            "internal_procedures" => {
                for child in kids(node) {
                    self.walk(child, scope_nid);
                }
            }
            "derived_type_definition" => {
                if let Some(stmt) = kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "derived_type_statement")
                {
                    if let Some(nn) = kids(stmt).into_iter().find(|c| c.kind() == "type_name") {
                        let type_name = self.text_lower(nn);
                        let type_nid = make_id([self.stem.as_str(), type_name.as_str()]);
                        self.add_node(&type_nid, &type_name, line);
                        self.add_edge(scope_nid, &type_nid, "defines", None, line);
                    }
                }
            }
            "subroutine" => {
                let name = kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "subroutine_statement")
                    .and_then(|s| self.fortran_name(s));
                if let Some(name) = name {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &format!("{name}()"), line);
                    self.add_edge(scope_nid, &nid, "defines", None, line);
                    self.scope_bodies.push((nid.clone(), node));
                    self.emit_signature_refs(node, &nid, false);
                    for child in kids(node) {
                        self.walk(child, &nid);
                    }
                }
            }
            "function" => {
                let name = kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "function_statement")
                    .and_then(|s| self.fortran_name(s));
                if let Some(name) = name {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &format!("{name}()"), line);
                    self.add_edge(scope_nid, &nid, "defines", None, line);
                    self.scope_bodies.push((nid.clone(), node));
                    self.emit_signature_refs(node, &nid, true);
                    for child in kids(node) {
                        self.walk(child, &nid);
                    }
                }
            }
            "use_statement" => {
                if let Some(nn) = kids(node)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "module_name" | "name" | "identifier"))
                {
                    let mod_name = self.text_lower(nn);
                    let imp_nid = make_id([mod_name.as_str()]);
                    self.add_node(&imp_nid, &mod_name, line);
                    self.add_edge(scope_nid, &imp_nid, "imports", Some("use"), line);
                }
            }
            _ => {
                for child in kids(node) {
                    self.walk(child, scope_nid);
                }
            }
        }
    }

    fn walk_calls(&mut self, node: Node, scope_nid: &str) {
        let t = node.kind();
        if matches!(
            t,
            "subroutine" | "function" | "module" | "program" | "internal_procedures"
        ) {
            return;
        }
        let line = node.start_position().row + 1;
        if t == "subroutine_call" {
            if let Some(nn) = kids(node).into_iter().find(|c| c.kind() == "identifier") {
                let callee = self.text_lower(nn);
                let tgt = make_id([self.stem.as_str(), callee.as_str()]);
                self.add_edge(scope_nid, &tgt, "calls", Some("call"), line);
            }
        } else if t == "call_expression" {
            if let Some(nn) = kids(node).into_iter().find(|c| c.kind() == "identifier") {
                let callee = self.text_lower(nn);
                let tgt = make_id([self.stem.as_str(), callee.as_str()]);
                if self.seen.contains(&tgt) && tgt != scope_nid {
                    self.add_edge(scope_nid, &tgt, "calls", Some("call"), line);
                }
            }
        }
        for child in kids(node) {
            self.walk_calls(child, scope_nid);
        }
    }
}

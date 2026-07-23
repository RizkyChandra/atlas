//! Solidity extractor (tree-sitter-solidity grammar). BACKLOG new language —
//! graphify has no Solidity extractor, so there is no oracle; the contract is the
//! hand-authored golden in tests/langs.rs.
//!
//! Nodes: file, contracts/interfaces/libraries (label = name), functions
//! (label `name()`). Edges: `contains` (file → contract, contract → function),
//! `imports` (`import "./X.sol"` → `X`), `inherits` (`contract A is B` → B),
//! `calls` (function → file-local function). Function ids key off the file stem
//! (`make_id(stem, name)`), so a call to a function declared in another contract
//! in the same file resolves.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_solidity::LANGUAGE.into())
        .expect("load solidity grammar");
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

    let mut ex = Sol {
        source,
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        funcs: HashSet::new(),
    };
    ex.nodes
        .push(file_node(&file_nid, &file_label, &ex.str_path));

    let root = tree.root_node();
    // Pass 1: collect all function names (any contract) for call resolution.
    ex.collect_funcs(root);
    // Pass 2: walk declarations.
    for child in crate::kids(root) {
        match child.kind() {
            "import_directive" => ex.handle_import(child),
            "contract_declaration" | "interface_declaration" | "library_declaration" => {
                ex.handle_contract(child)
            }
            _ => {}
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

struct Sol<'a> {
    source: &'a [u8],
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    funcs: HashSet<String>,
}

impl<'a> Sol<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }
    fn name_of(&self, n: Node<'a>) -> Option<String> {
        crate::kids(n)
            .into_iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| self.read(c))
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        self.nodes.push(node_map(
            nid,
            label,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
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

    fn collect_funcs(&mut self, node: Node<'a>) {
        if node.kind() == "function_definition" {
            if let Some(name) = self.name_of(node) {
                self.funcs.insert(name);
            }
        }
        for c in crate::kids(node) {
            self.collect_funcs(c);
        }
    }

    fn handle_import(&mut self, node: Node<'a>) {
        let line = self.line(node);
        if let Some(s) = crate::kids(node).into_iter().find(|c| c.kind() == "string") {
            let raw = self.read(s);
            let raw = raw.trim_matches(|c| c == '"' || c == '\'');
            // "./Ownable.sol" → Ownable
            let name = Path::new(raw)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw.to_string());
            let tgt = make_id([name.as_str()]);
            self.add_edge(&self.file_nid.clone(), &tgt, "imports", line);
        }
    }

    fn handle_contract(&mut self, node: Node<'a>) {
        let Some(cname) = self.name_of(node) else {
            return;
        };
        let line = self.line(node);
        let cnid = make_id([self.stem.as_str(), cname.as_str()]);
        self.add_node(&cnid, &cname, line);
        self.add_edge(&self.file_nid.clone(), &cnid, "contains", line);

        // Inheritance: `is Base, Other`.
        for spec in crate::kids(node)
            .into_iter()
            .filter(|c| c.kind() == "inheritance_specifier")
        {
            for udt in crate::kids(spec)
                .into_iter()
                .filter(|c| c.kind() == "user_defined_type")
            {
                if let Some(base) = crate::kids(udt)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                {
                    let base = self.read(base);
                    let bnid = make_id([self.stem.as_str(), base.as_str()]);
                    self.add_edge(&cnid, &bnid, "inherits", self.line(spec));
                }
            }
        }

        // Functions in the contract body.
        if let Some(body) = crate::kids(node)
            .into_iter()
            .find(|c| c.kind() == "contract_body")
        {
            for member in crate::kids(body) {
                if member.kind() == "function_definition" {
                    if let Some(fname) = self.name_of(member) {
                        let fline = self.line(member);
                        let fnid = make_id([self.stem.as_str(), fname.as_str()]);
                        self.add_node(&fnid, &format!("{fname}()"), fline);
                        self.add_edge(&cnid, &fnid, "contains", fline);
                        self.calls_in(member, &fnid);
                    }
                }
            }
        }
    }

    /// `calls` edges to file-local functions invoked inside `node`.
    fn calls_in(&mut self, node: Node<'a>, caller_nid: &str) {
        if node.kind() == "call_expression" {
            if let Some(expr) = crate::kids(node)
                .into_iter()
                .find(|c| c.kind() == "expression")
            {
                if let Some(id) = crate::kids(expr)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                {
                    let callee = self.read(id);
                    if self.funcs.contains(&callee) {
                        let tgt = make_id([self.stem.as_str(), callee.as_str()]);
                        if tgt != caller_nid {
                            self.add_edge(caller_nid, &tgt, "calls", self.line(node));
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

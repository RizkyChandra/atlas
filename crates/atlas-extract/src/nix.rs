//! Nix extractor (tree-sitter-nix grammar). BACKLOG new language — graphify has
//! no Nix extractor, so there is no oracle; the contract is the hand-authored
//! golden in tests/langs.rs.
//!
//! Nodes: file, bindings (`name = value;` in let / attrset). A binding whose
//! value is a lambda gets label `name()`; other bindings label `name`. Edges:
//! `contains` (file → binding), `imports` (`import <nixpkgs>` / `import ./x.nix`
//! → target), `calls` (binding → file-local binding applied as a function). Nix
//! has no classes/inheritance, so no `inherits` edge (attrsets + imports).

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_nix::LANGUAGE.into())
        .expect("load nix grammar");
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

    let mut ex = Nix {
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
    let bindings = ex.collect_bindings(root);
    // Pass 1: register binding names.
    for &b in &bindings {
        if let Some(name) = ex.binding_name(b) {
            ex.defs
                .insert(name.clone(), make_id([ex.stem.as_str(), name.as_str()]));
        }
    }
    // Pass 2: emit nodes + edges.
    for &b in &bindings {
        ex.handle_binding(b);
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

struct Nix<'a> {
    source: &'a [u8],
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    defs: HashMap<String, String>, // binding name → nid
}

impl<'a> Nix<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }

    fn collect_bindings(&self, node: Node<'a>) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        fn rec<'b>(n: Node<'b>, out: &mut Vec<Node<'b>>) {
            if n.kind() == "binding" {
                out.push(n);
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                rec(ch, out);
            }
        }
        rec(node, &mut out);
        out
    }

    fn binding_name(&self, b: Node<'a>) -> Option<String> {
        let attrpath = crate::kids(b)
            .into_iter()
            .find(|c| c.kind() == "attrpath")?;
        crate::kids(attrpath)
            .into_iter()
            .find(|c| c.kind() == "identifier")
            .map(|c| self.read(c))
    }

    fn binding_value(&self, b: Node<'a>) -> Option<Node<'a>> {
        crate::kids(b)
            .into_iter()
            .find(|c| c.is_named() && c.kind() != "attrpath")
    }

    fn handle_binding(&mut self, b: Node<'a>) {
        let Some(name) = self.binding_name(b) else {
            return;
        };
        let nid = self.defs[&name].clone();
        let line = self.line(b);
        let value = self.binding_value(b);
        let is_fn = value
            .map(|v| v.kind() == "function_expression")
            .unwrap_or(false);
        let label = if is_fn {
            format!("{name}()")
        } else {
            name.clone()
        };
        self.nodes.push(node_map(
            &nid,
            &label,
            "code",
            &self.str_path,
            &format!("L{line}"),
        ));
        self.edges.push(edge_map(
            &self.file_nid,
            &nid,
            "contains",
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
        if let Some(v) = value {
            self.scan_value(v, &nid);
        }
    }

    /// Within a binding value: `import <path>` → file `imports`; applying a
    /// file-local binding → binding `calls`.
    fn scan_value(&mut self, node: Node<'a>, owner_nid: &str) {
        if node.kind() == "apply_expression" {
            let kids = crate::kids(node);
            if let Some(func) = kids.first() {
                let head = self.func_head_name(*func);
                match head.as_deref() {
                    Some("import") => {
                        if let Some(arg) = kids.get(1) {
                            if let Some(tgt) = self.import_target(*arg) {
                                self.edges.push(edge_map(
                                    &self.file_nid.clone(),
                                    &tgt,
                                    "imports",
                                    None,
                                    &self.str_path,
                                    &format!("L{}", self.line(node)),
                                ));
                            }
                        }
                    }
                    Some(name) => {
                        if let Some(tgt) = self.defs.get(name).cloned() {
                            if tgt != owner_nid {
                                self.edges.push(edge_map(
                                    owner_nid,
                                    &tgt,
                                    "calls",
                                    None,
                                    &self.str_path,
                                    &format!("L{}", self.line(node)),
                                ));
                            }
                        }
                    }
                    None => {}
                }
            }
        }
        for c in crate::kids(node) {
            self.scan_value(c, owner_nid);
        }
    }

    /// The bare identifier a `variable_expression` function-head refers to.
    fn func_head_name(&self, n: Node<'a>) -> Option<String> {
        if n.kind() == "variable_expression" {
            return crate::kids(n)
                .into_iter()
                .find(|c| c.kind() == "identifier")
                .map(|c| self.read(c));
        }
        None
    }

    /// Resolve `import` argument (`<nixpkgs>`, `"x"`, `./x.nix`) to a node id.
    fn import_target(&self, arg: Node<'a>) -> Option<String> {
        let raw = self.read(arg);
        let raw = raw
            .trim()
            .trim_matches(|c| c == '<' || c == '>' || c == '"' || c == '\'');
        let name = Path::new(raw)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| raw.to_string());
        if name.is_empty() {
            None
        } else {
            Some(make_id([name.as_str()]))
        }
    }
}

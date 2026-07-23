//! Elixir extractor — a Rust port of graphify `graphify/extractors/elixir.py`.
//!
//! Extracts modules (`defmodule` → `contains`), functions (`def`/`defp` →
//! `method` under a module, else `contains`), `alias`/`import`/`require`/`use`
//! imports (including the `Foo.{Bar, Baz}` multi-alias form), and in-file
//! `calls`. Member calls (`Repo.insert`) resolve only if the bare method name
//! matches an in-file node label. Edges are pruned to those whose source is a
//! known node and whose target is known or is an `imports` edge.
//!
//! Node ids key off the file stem to match graphify's *built* graph.

use crate::{edge_map, is_builtin_global, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const IMPORT_KEYWORDS: &[&str] = &["alias", "import", "require", "use"];
const SKIP_KEYWORDS: &[&str] = &[
    "def",
    "defp",
    "defmodule",
    "defmacro",
    "defmacrop",
    "defstruct",
    "defprotocol",
    "defimpl",
    "defguard",
    "alias",
    "import",
    "require",
    "use",
    "if",
    "unless",
    "case",
    "cond",
    "with",
    "for",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_elixir::LANGUAGE.into())
        .expect("load elixir grammar");
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

    let mut ex = Elixir {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        function_bodies: Vec::new(),
    };

    ex.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    ex.walk(root, None);

    // Call pass.
    let mut label_to_nid: HashMap<String, String> = HashMap::new();
    for n in &ex.nodes {
        if let (Some(id), Some(label)) = (
            n.get("id").and_then(Value::as_str),
            n.get("label").and_then(Value::as_str),
        ) {
            let norm = label
                .trim_matches(|c| c == '(' || c == ')')
                .trim_start_matches('.');
            label_to_nid.insert(norm.to_string(), id.to_string());
        }
    }
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (caller, body) in bodies {
        ex.walk_calls(body, &caller, &label_to_nid, &mut seen_pairs);
    }

    // Prune edges: source must be a real node; target real or an import.
    let seen = std::mem::take(&mut ex.seen);
    let edges: Vec<Attrs> = ex
        .edges
        .into_iter()
        .filter(|e| {
            let src = e.get("source").and_then(Value::as_str).unwrap_or("");
            let tgt = e.get("target").and_then(Value::as_str).unwrap_or("");
            let rel = e.get("relation").and_then(Value::as_str).unwrap_or("");
            seen.contains(src) && (seen.contains(tgt) || rel == "imports")
        })
        .collect();

    ExtractResult {
        nodes: ex.nodes,
        edges,
    }
}

struct Elixir<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    function_bodies: Vec<(String, Node<'a>)>,
}

impl<'a> Elixir<'a> {
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

    fn get_alias_text(&self, args: Node) -> Option<String> {
        kids(args)
            .into_iter()
            .find(|c| c.kind() == "alias")
            .map(|c| self.text(c))
    }

    fn get_alias_modules(&self, args: Node) -> Vec<String> {
        for child in kids(args) {
            if child.kind() == "alias" {
                return vec![self.text(child)];
            }
            if child.kind() == "dot" {
                let mut base: Option<String> = None;
                let mut tuple: Option<Node> = None;
                for sub in kids(child) {
                    if sub.kind() == "alias" && base.is_none() {
                        base = Some(self.text(sub));
                    } else if sub.kind() == "tuple" {
                        tuple = Some(sub);
                    }
                }
                if let (Some(base), Some(t)) = (base.as_ref(), tuple) {
                    let members: Vec<String> = kids(t)
                        .into_iter()
                        .filter(|m| m.kind() == "alias")
                        .map(|m| self.text(m))
                        .collect();
                    if !members.is_empty() {
                        return members.into_iter().map(|m| format!("{base}.{m}")).collect();
                    }
                }
                return vec![self.text(child)];
            }
        }
        Vec::new()
    }

    fn walk(&mut self, node: Node<'a>, parent_module_nid: Option<&str>) {
        if node.kind() != "call" {
            for child in kids(node) {
                self.walk(child, parent_module_nid);
            }
            return;
        }

        let mut identifier_node = None;
        let mut arguments_node = None;
        let mut do_block_node = None;
        for child in kids(node) {
            match child.kind() {
                "identifier" => identifier_node = Some(child),
                "arguments" => arguments_node = Some(child),
                "do_block" => do_block_node = Some(child),
                _ => {}
            }
        }
        let Some(id_node) = identifier_node else {
            for child in kids(node) {
                self.walk(child, parent_module_nid);
            }
            return;
        };
        let keyword = self.text(id_node);
        let line = node.start_position().row + 1;

        if keyword == "defmodule" {
            let module_name = arguments_node.and_then(|a| self.get_alias_text(a));
            let Some(module_name) = module_name else {
                return;
            };
            let module_nid = make_id([self.stem.as_str(), module_name.as_str()]);
            self.add_node(&module_nid, &module_name, line);
            let f = self.file_nid.clone();
            self.add_edge(&f, &module_nid, "contains", None, line);
            if let Some(db) = do_block_node {
                for child in kids(db) {
                    self.walk(child, Some(&module_nid));
                }
            }
            return;
        }

        if keyword == "def" || keyword == "defp" {
            let mut func_name: Option<String> = None;
            if let Some(args) = arguments_node {
                for child in kids(args) {
                    if child.kind() == "call" {
                        for sub in kids(child) {
                            if sub.kind() == "identifier" {
                                func_name = Some(self.text(sub));
                                break;
                            }
                        }
                        break;
                    } else if child.kind() == "identifier" {
                        func_name = Some(self.text(child));
                        break;
                    }
                }
            }
            let Some(func_name) = func_name else { return };
            let container = parent_module_nid.unwrap_or(&self.file_nid).to_string();
            let func_nid = make_id([container.as_str(), func_name.as_str()]);
            self.add_node(&func_nid, &format!("{func_name}()"), line);
            match parent_module_nid {
                Some(m) => self.add_edge(m, &func_nid, "method", None, line),
                None => {
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &func_nid, "contains", None, line);
                }
            }
            if let Some(db) = do_block_node {
                self.function_bodies.push((func_nid, db));
            }
            return;
        }

        if IMPORT_KEYWORDS.contains(&keyword.as_str()) {
            if let Some(args) = arguments_node {
                for module_name in self.get_alias_modules(args) {
                    let tgt = make_id([module_name.as_str()]);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &tgt, "imports", Some("import"), line);
                }
            }
            return;
        }

        for child in kids(node) {
            self.walk(child, parent_module_nid);
        }
    }

    fn walk_calls(
        &mut self,
        node: Node,
        caller_nid: &str,
        label_to_nid: &HashMap<String, String>,
        seen_pairs: &mut HashSet<(String, String)>,
    ) {
        if node.kind() != "call" {
            for child in kids(node) {
                self.walk_calls(child, caller_nid, label_to_nid, seen_pairs);
            }
            return;
        }
        // Skip control-flow / definition keyword calls, but recurse their children.
        for child in kids(node) {
            if child.kind() == "identifier" {
                let kw = self.text(child);
                if SKIP_KEYWORDS.contains(&kw.as_str()) {
                    for c in kids(node) {
                        self.walk_calls(c, caller_nid, label_to_nid, seen_pairs);
                    }
                    return;
                }
                break;
            }
        }
        let mut callee_name: Option<String> = None;
        for child in kids(node) {
            if child.kind() == "dot" {
                let dot_text = self.text(child);
                let trimmed = dot_text.trim_end_matches('.');
                callee_name = trimmed.rsplit('.').next().map(|s| s.to_string());
                break;
            }
            if child.kind() == "identifier" {
                callee_name = Some(self.text(child));
                break;
            }
        }
        if let Some(callee) = callee_name {
            if !is_builtin_global(&callee) {
                if let Some(tgt) = label_to_nid.get(&callee) {
                    if tgt != caller_nid && seen_pairs.insert((caller_nid.to_string(), tgt.clone()))
                    {
                        let line = node.start_position().row + 1;
                        self.add_edge(caller_nid, tgt, "calls", Some("call"), line);
                    }
                }
            }
        }
        for child in kids(node) {
            self.walk_calls(child, caller_nid, label_to_nid, seen_pairs);
        }
    }
}

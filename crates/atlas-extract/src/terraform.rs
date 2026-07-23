//! Terraform/HCL extractor — a Rust port of graphify
//! `graphify/extractors/terraform.py` (tree-sitter-hcl grammar).
//!
//! Nodes: resources, data sources, modules, variables, outputs, providers,
//! locals. Edges: `contains` (file → block), `references` (interpolated block
//! address, e.g. `aws_instance.web` → `var.region`), `depends_on`. Block ids are
//! scoped by the parent DIRECTORY name (Terraform is module/dir-scoped), so
//! cross-file references resolve when per-file extracts merge. The file node
//! keys off the file stem (graphify relativizes str_path to it).

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

const META_HEADS: &[&str] = &["count", "each", "self", "path", "terraform"];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_hcl::LANGUAGE.into())
        .expect("load hcl grammar");
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
    let str_path = path.to_string_lossy().into_owned();
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let scope = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tf".to_string());

    let mut ex = Tf {
        source,
        scope,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        seen_edges: HashSet::new(),
    };
    let mut m = node_map(&file_nid, &file_label, "code", &ex.str_path, "");
    m.insert("source_location".into(), Value::Null);
    ex.nodes.push(m);
    ex.seen.insert(file_nid);

    let root = tree.root_node();
    let body = crate::kids(root)
        .into_iter()
        .find(|c| c.kind() == "body")
        .unwrap_or(root);
    for block in crate::kids(body) {
        if block.kind() == "block" {
            ex.handle_block(block);
        }
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Tf<'a> {
    source: &'a [u8],
    scope: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    seen_edges: HashSet<(String, String, String)>,
}

impl<'a> Tf<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn label_text(&self, n: Node) -> String {
        self.read(n).trim().trim_matches('"').to_string()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }

    fn add_node(&mut self, address: &str, label: &str, line: usize) -> String {
        let nid = make_id([self.scope.as_str(), address]);
        if self.seen.insert(nid.clone()) {
            self.nodes.push(node_map(
                &nid,
                label,
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
        }
        nid
    }

    fn add_edge(&mut self, src: &str, address: &str, relation: &str, line: usize) {
        let tgt = make_id([self.scope.as_str(), address]);
        if src == tgt {
            return;
        }
        let key = (src.to_string(), tgt.clone(), relation.to_string());
        if !self.seen_edges.insert(key) {
            return;
        }
        self.edges.push(edge_map(
            src,
            &tgt,
            relation,
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// (block_type, labels) — read the leading identifier + string/identifier
    /// labels, stopping at block_start/body/block_end.
    fn block_parts(&self, block: Node) -> (Option<String>, Vec<String>) {
        let mut btype = None;
        let mut labels = Vec::new();
        for c in crate::kids(block) {
            match c.kind() {
                "block_start" | "body" | "block_end" => break,
                "identifier" if btype.is_none() => btype = Some(self.read(c)),
                "string_lit" | "identifier" => labels.push(self.label_text(c)),
                _ => {}
            }
        }
        (btype, labels)
    }

    fn body_of(&self, block: Node<'a>) -> Option<Node<'a>> {
        crate::kids(block).into_iter().find(|c| c.kind() == "body")
    }

    fn handle_block(&mut self, block: Node<'a>) {
        let (btype, labels) = self.block_parts(block);
        let line = self.line(block);
        let blk_body = self.body_of(block);
        let btype = btype.as_deref().unwrap_or("");

        let owner = match btype {
            "resource" if labels.len() >= 2 => {
                let a = format!("{}.{}", labels[0], labels[1]);
                self.add_node(&a, &a, line)
            }
            "data" if labels.len() >= 2 => {
                let a = format!("data.{}.{}", labels[0], labels[1]);
                self.add_node(&a, &a, line)
            }
            "module" if !labels.is_empty() => {
                let a = format!("module.{}", labels[0]);
                self.add_node(&a, &a, line)
            }
            "variable" if !labels.is_empty() => {
                let a = format!("var.{}", labels[0]);
                self.add_node(&a, &a, line)
            }
            "output" if !labels.is_empty() => {
                let a = format!("output.{}", labels[0]);
                self.add_node(&a, &a, line)
            }
            "provider" if !labels.is_empty() => {
                let a = format!("provider.{}", labels[0]);
                self.add_node(&a, &a, line)
            }
            "locals" => {
                if let Some(bb) = blk_body {
                    for attr in crate::kids(bb) {
                        if attr.kind() != "attribute" {
                            continue;
                        }
                        let Some(key_node) = crate::kids(attr).into_iter().next() else {
                            continue;
                        };
                        let key = self.read(key_node);
                        let lnid = self.add_node(
                            &format!("local.{key}"),
                            &format!("local.{key}"),
                            self.line(attr),
                        );
                        self.collect_refs(attr, &lnid, "references");
                    }
                }
                return;
            }
            _ => return,
        };

        if let Some(bb) = blk_body {
            self.collect_refs(bb, &owner, "references");
        }
    }

    fn collect_refs(&mut self, node: Node, owner: &str, relation: &str) {
        let mut rel = relation;
        if node.kind() == "attribute" {
            if let Some(key_node) = crate::kids(node).into_iter().next() {
                if self.read(key_node) == "depends_on" {
                    rel = "depends_on";
                }
            }
        }
        if node.kind() == "variable_expr" {
            if let Some(addr) = self.ref_address(node) {
                self.add_edge(owner, &addr, rel, self.line(node));
            }
        }
        for c in crate::kids(node) {
            if c.is_named() {
                self.collect_refs(c, owner, rel);
            }
        }
    }

    /// Resolve a `variable_expr` (+ trailing sibling `get_attr`s) to a block
    /// address like `var.region` / `data.aws_ami.ubuntu` / `aws_instance.web`.
    fn ref_address(&self, expr: Node) -> Option<String> {
        let head = self.read(expr);
        let mut attrs: Vec<String> = Vec::new();
        if let Some(parent) = expr.parent() {
            let mut seen_self = false;
            for c in crate::kids(parent) {
                if c.id() == expr.id() {
                    seen_self = true;
                    continue;
                }
                if seen_self {
                    if c.kind() == "get_attr" {
                        let name = crate::kids(c)
                            .into_iter()
                            .find(|gc| gc.kind() == "identifier")
                            .map(|gc| self.read(gc));
                        match name {
                            Some(n) => attrs.push(n),
                            None => break,
                        }
                    } else {
                        break;
                    }
                }
            }
        }
        if head.is_empty() || META_HEADS.contains(&head.as_str()) {
            return None;
        }
        match head.as_str() {
            "var" => attrs.first().map(|a| format!("var.{a}")),
            "local" => attrs.first().map(|a| format!("local.{a}")),
            "module" => attrs.first().map(|a| format!("module.{a}")),
            "data" => {
                if attrs.len() >= 2 {
                    Some(format!("data.{}.{}", attrs[0], attrs[1]))
                } else {
                    None
                }
            }
            _ => attrs.first().map(|a| format!("{head}.{a}")),
        }
    }
}

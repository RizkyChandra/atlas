//! SQL extractor — a Rust port of graphify `graphify/extractors/sql.py`
//! (tree-sitter-sequel / DerekStride's tree-sitter-sql grammar).
//!
//! Nodes: tables (`create_table`), views (`create_view`), functions/procedures
//! (`create_function`/`create_procedure`), triggers (`create_trigger`). Edges:
//! `contains` (file → object), `references` (FK REFERENCES), `reads_from`
//! (view/function FROM/JOIN), `triggers` (trigger → table). Object ids key off
//! the file stem (graphify uses str_path then relativizes to the stem).
//!
//! NOT ported (documented in tests/langs.rs): the dialect regex-recovery paths
//! graphify runs when tree-sitter emits ERROR nodes — PL/pgSQL bodies that fail
//! to parse (`ERROR` CREATE FUNCTION/PROCEDURE scan) and Firebird
//! `fb_proc_or_trigger` / `set_term` / `declare_external_function` statements.
//! The global CREATE TABLE ... REFERENCES regex sweep (recovers FKs dropped by
//! ERROR-in-columns) IS ported. Inputs that lean on the un-ported fallbacks
//! would under-report vs graphify; the standard-SQL tree walk matches exactly.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("load sql grammar");
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

    let mut ex = Sql {
        source,
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        table_nids: HashMap::new(),
    };
    // File node: graphify emits source_location null (not L1).
    ex.nodes
        .push(node_map_no_loc(&file_nid, &file_label, &ex.str_path));
    ex.seen.insert(file_nid);

    let root = tree.root_node();
    for stmt in crate::kids(root) {
        if stmt.kind() == "statement" {
            for child in crate::kids(stmt) {
                ex.walk(child);
            }
        }
    }
    ex.global_reference_fallback();

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

/// A node with an explicit null `source_location` (graphify's file node).
fn node_map_no_loc(id: &str, label: &str, source_file: &str) -> Attrs {
    let mut m = node_map(id, label, "code", source_file, "");
    m.insert("source_location".into(), serde_json::Value::Null);
    m
}

struct Sql<'a> {
    source: &'a [u8],
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    table_nids: HashMap<String, String>, // lowercased name → nid
}

impl<'a> Sql<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }

    /// First `object_reference` child text.
    fn obj_name(&self, n: Node) -> Option<String> {
        crate::kids(n)
            .into_iter()
            .find(|c| c.kind() == "object_reference")
            .map(|c| self.read(c))
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
            self.edges.push(edge_map(
                &self.file_nid,
                nid,
                "contains",
                None,
                &self.str_path,
                &format!("L{line}"),
            ));
        }
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, line: usize) {
        self.edges.push(edge_map(
            src,
            tgt,
            relation,
            None,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    fn ref_nid(&self, name: &str) -> String {
        self.table_nids
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_else(|| make_id([self.stem.as_str(), name]))
    }

    fn walk(&mut self, node: Node<'a>) {
        let line = self.line(node);
        match node.kind() {
            "create_table" => {
                if let Some(name) = self.obj_name(node) {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &name, line);
                    self.table_nids.insert(name.to_lowercase(), nid.clone());
                    self.table_fk_refs(node, &nid, line);
                }
            }
            "create_view" => {
                if let Some(name) = self.obj_name(node) {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &name, line);
                    self.table_nids.insert(name.to_lowercase(), nid.clone());
                    self.walk_from_refs(node, &nid);
                }
            }
            "create_function" | "create_procedure" => {
                if let Some(name) = self.obj_name(node) {
                    let nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&nid, &format!("{name}()"), line);
                    self.walk_from_refs(node, &nid);
                }
            }
            "alter_table" => {
                if let Some(name) = self.obj_name(node) {
                    let src_nid = self.table_nids.get(&name.to_lowercase()).cloned();
                    let src_nid = match src_nid {
                        Some(s) => s,
                        None => {
                            let s = make_id([self.stem.as_str(), name.as_str()]);
                            self.add_node(&s, &name, line);
                            self.table_nids.insert(name.to_lowercase(), s.clone());
                            s
                        }
                    };
                    for child in crate::kids(node) {
                        if child.kind() != "add_constraint" {
                            continue;
                        }
                        for cc in crate::kids(child) {
                            if cc.kind() != "constraint" {
                                continue;
                            }
                            if let Some(rn) = self.constraint_ref(cc) {
                                let ref_nid = self.ref_nid(&rn);
                                self.add_edge(&src_nid, &ref_nid, "references", line);
                            }
                        }
                    }
                }
            }
            "create_trigger" => {
                let (trig_name, tbl_name) = self.trigger_parts(node);
                if let Some(trig_name) = trig_name {
                    let trig_nid = make_id([self.stem.as_str(), trig_name.as_str()]);
                    self.add_node(&trig_nid, &trig_name, line);
                    if let Some(tbl) = tbl_name {
                        let tbl_nid = self.ref_nid(&tbl);
                        self.add_edge(&trig_nid, &tbl_nid, "triggers", line);
                    }
                }
            }
            _ => {}
        }
        for child in crate::kids(node) {
            self.walk(child);
        }
    }

    /// FK REFERENCES inside a create_table's `column_definitions`: inline
    /// column-level and table-level FOREIGN KEY constraints.
    fn table_fk_refs(&mut self, node: Node, tbl_nid: &str, line: usize) {
        for col in crate::kids(node) {
            if col.kind() != "column_definitions" {
                continue;
            }
            for cd in crate::kids(col) {
                match cd.kind() {
                    "column_definition" => {
                        if let Some(rn) = self.after_references(cd) {
                            let ref_nid = self.ref_nid(&rn);
                            self.add_edge(tbl_nid, &ref_nid, "references", line);
                        }
                    }
                    "constraints" => {
                        for c in crate::kids(cd) {
                            if c.kind() == "constraint" {
                                if let Some(rn) = self.constraint_ref(c) {
                                    let ref_nid = self.ref_nid(&rn);
                                    self.add_edge(tbl_nid, &ref_nid, "references", line);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// The `object_reference` that follows a `keyword_references` child.
    fn after_references(&self, n: Node) -> Option<String> {
        let mut found = false;
        for cc in crate::kids(n) {
            if cc.kind() == "keyword_references" {
                found = true;
            } else if found && cc.kind() == "object_reference" {
                return Some(self.read(cc));
            }
        }
        None
    }
    fn constraint_ref(&self, n: Node) -> Option<String> {
        self.after_references(n)
    }

    fn trigger_parts(&self, node: Node) -> (Option<String>, Option<String>) {
        let (mut trig, mut tbl) = (None, None);
        let (mut after_trig, mut after_for) = (false, false);
        for c in crate::kids(node) {
            match c.kind() {
                "keyword_trigger" => after_trig = true,
                "object_reference" if after_trig && trig.is_none() && !after_for => {
                    trig = Some(self.read(c))
                }
                "keyword_for" => after_for = true,
                "object_reference" if after_for && tbl.is_none() => tbl = Some(self.read(c)),
                _ => {}
            }
        }
        (trig, tbl)
    }

    /// Recursively find FROM/JOIN table references, emitting `reads_from`.
    fn walk_from_refs(&mut self, node: Node, caller_nid: &str) {
        if matches!(node.kind(), "from" | "join") {
            for c in crate::kids(node) {
                if c.kind() == "relation" {
                    let rel_line = self.line(c);
                    for cc in crate::kids(c) {
                        if cc.kind() == "object_reference" {
                            let tbl = self.read(cc);
                            let tbl_nid = make_id([self.stem.as_str(), tbl.as_str()]);
                            self.add_edge(caller_nid, &tbl_nid, "reads_from", rel_line);
                        }
                    }
                }
            }
        }
        for child in crate::kids(node) {
            self.walk_from_refs(child, caller_nid);
        }
    }

    /// graphify's global regex sweep: catch any `CREATE TABLE t (... REFERENCES
    /// r ...)` FK dropped by an ERROR node in the parse tree. Only emits for
    /// tables that already have a node, and skips pairs already emitted.
    fn global_reference_fallback(&mut self) {
        let mut emitted: HashSet<(String, String)> = self
            .edges
            .iter()
            .filter(|e| e.get("relation").and_then(|v| v.as_str()) == Some("references"))
            .filter_map(|e| {
                Some((
                    e.get("source")?.as_str()?.to_string(),
                    e.get("target")?.as_str()?.to_string(),
                ))
            })
            .collect();
        let text = String::from_utf8_lossy(self.source).into_owned();
        let table_re = Regex::new(r"(?i)CREATE\s+TABLE\s+([\w$]+)\s*\(").unwrap();
        let split_re = Regex::new(r"(?im)(?:^|\n)(?:CREATE|SET\s+TERM|ALTER)\s").unwrap();
        let ref_re = Regex::new(r"(?i)\bREFERENCES\s+([\w$]+)").unwrap();
        for m in table_re.captures_iter(&text) {
            let tbl_name = m.get(1).unwrap().as_str();
            let Some(tbl_nid) = self.table_nids.get(&tbl_name.to_lowercase()).cloned() else {
                continue;
            };
            let start = m.get(0).unwrap().start();
            let tbl_line = text[..start].matches('\n').count() + 1;
            let tail = &text[start..];
            // Block ends at the next top-level CREATE/SET TERM/ALTER (search past char 0).
            let block = match split_re.find(&tail[1..]) {
                Some(e) => &tail[..e.start() + 1],
                None => tail,
            };
            for rm in ref_re.captures_iter(block) {
                let ref_name = rm.get(1).unwrap().as_str();
                let ref_nid = self.ref_nid(ref_name);
                if emitted.insert((tbl_nid.clone(), ref_nid.clone())) {
                    self.add_edge(&tbl_nid, &ref_nid, "references", tbl_line);
                }
            }
        }
    }
}

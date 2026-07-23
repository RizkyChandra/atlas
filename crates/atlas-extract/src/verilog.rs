//! Verilog / SystemVerilog extractor — a Rust port of graphify
//! `graphify/extractors/verilog.py`.
//!
//! Two passes, mirroring graphify:
//!   1. tree-sitter-verilog (crate 1.0.3, matching the oracle grammar) walk:
//!      modules (`defines`), functions/tasks (`contains`), `import pkg::*`
//!      (`imports_from`), and instantiations (`instantiates`).
//!   2. A regex augmentation over the raw source for SystemVerilog *class*
//!      semantics (`_augment_systemverilog_semantics`): class nodes (`defines`),
//!      `extends`→`inherits`, `implements`→`implements`, field/return/parameter
//!      type references (generics as `generic_arg`), and methods (`method`).
//!      Class subtrees are skipped in the AST walk so methods are not
//!      double-emitted with the wrong (return-type-derived) name.
//!
//! Verilog nodes/edges carry `confidence_score: 1.0` (graphify's verilog
//! extractor sets it explicitly), so this module builds its own node/edge maps
//! rather than reusing `node_map`/`edge_map`.
//!
//! Out of single-file scope: cross-file module/package resolution. An
//! instantiated module `leaf` yields BOTH the defined-module node
//! (`stem_leaf`) and a bare instantiation-target node (`leaf`), matching the
//! oracle (the bare node is sourced, so it is not rewired).

use crate::{kids, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const BUILTIN_TYPES: &[&str] = &[
    "bit",
    "logic",
    "reg",
    "wire",
    "int",
    "integer",
    "shortint",
    "longint",
    "byte",
    "time",
    "real",
    "shortreal",
    "void",
    "string",
    "type",
    "event",
    "mailbox",
    "semaphore",
    "process",
    "chandle",
];
const NON_TYPE_WORDS: &[&str] = &[
    "return",
    "if",
    "else",
    "for",
    "foreach",
    "while",
    "case",
    "begin",
    "end",
    "function",
    "task",
    "class",
    "endclass",
    "endfunction",
    "endtask",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_verilog::LANGUAGE.into())
        .expect("load verilog grammar");
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

    let mut ex = Sv {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
    };
    ex.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    ex.walk(root, None);

    // Pass 2: regex class augmentation over the raw source.
    let raw = String::from_utf8_lossy(source).into_owned();
    ex.augment_classes(&raw);

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Sv<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
}

impl<'a> Sv<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if !self.seen.insert(nid.to_string()) {
            return;
        }
        let mut m = Attrs::new();
        m.insert("id".into(), json!(nid));
        m.insert("label".into(), json!(label));
        m.insert("file_type".into(), json!("code"));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), json!(format!("L{line}")));
        m.insert("confidence_score".into(), json!(1.0));
        self.nodes.push(m);
    }

    fn add_edge(
        &mut self,
        src: &str,
        tgt: &str,
        relation: &str,
        line: usize,
        context: Option<&str>,
    ) {
        let mut m = Attrs::new();
        m.insert("source".into(), json!(src));
        m.insert("target".into(), json!(tgt));
        m.insert("relation".into(), json!(relation));
        m.insert("confidence".into(), json!("EXTRACTED"));
        m.insert("confidence_score".into(), json!(1.0));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), json!(format!("L{line}")));
        m.insert("weight".into(), json!(1.0));
        if let Some(c) = context {
            m.insert("context".into(), json!(c));
        }
        self.edges.push(m);
    }

    /// First `simple_identifier` under `node` in pre-order.
    fn first_identifier(&self, node: Option<Node>) -> Option<String> {
        let node = node?;
        for child in kids(node) {
            if child.kind() == "simple_identifier" {
                return Some(self.text(child));
            }
            if let Some(found) = self.first_identifier(Some(child)) {
                return Some(found);
            }
        }
        None
    }

    fn child<'b>(&self, node: Option<Node<'b>>, kind: &str) -> Option<Node<'b>> {
        kids(node?).into_iter().find(|c| c.kind() == kind)
    }

    fn walk(&mut self, node: Node<'a>, module_nid: Option<&str>) {
        let t = node.kind();

        // Class bodies are handled by the regex augmentation; skip their subtrees.
        if t == "class_declaration" || t == "interface_class_declaration" {
            return;
        }

        if t == "module_declaration" {
            if let Some(name) = self.first_identifier(self.child(Some(node), "module_header")) {
                let line = node.start_position().row + 1;
                let nid = make_id([self.stem.as_str(), name.as_str()]);
                self.add_node(&nid, &name, line);
                let f = self.file_nid.clone();
                self.add_edge(&f, &nid, "defines", line, None);
                for child in kids(node) {
                    self.walk(child, Some(&nid));
                }
                return;
            }
        } else if t == "function_declaration" {
            let fn_body = self.child(Some(node), "function_body_declaration");
            if let Some(name) = self.first_identifier(self.child(fn_body, "function_identifier")) {
                let line = node.start_position().row + 1;
                let parent = module_nid.unwrap_or(&self.file_nid).to_string();
                let nid = make_id([parent.as_str(), name.as_str()]);
                self.add_node(&nid, &format!("{name}()"), line);
                self.add_edge(&parent, &nid, "contains", line, None);
            }
        } else if t == "task_declaration" {
            let tk_body = self.child(Some(node), "task_body_declaration");
            if let Some(name) = self.first_identifier(self.child(tk_body, "task_identifier")) {
                let line = node.start_position().row + 1;
                let parent = module_nid.unwrap_or(&self.file_nid).to_string();
                let nid = make_id([parent.as_str(), name.as_str()]);
                self.add_node(&nid, &name, line);
                self.add_edge(&parent, &nid, "contains", line, None);
            }
        } else if t == "package_import_declaration" {
            for child in kids(node) {
                if child.kind() == "package_import_item" {
                    let pkg = self.text(child);
                    let pkg = pkg.split("::").next().unwrap_or("").trim();
                    if !pkg.is_empty() {
                        let line = node.start_position().row + 1;
                        let tgt = make_id([pkg]);
                        self.add_node(&tgt, pkg, line);
                        let src = module_nid.unwrap_or(&self.file_nid).to_string();
                        self.add_edge(&src, &tgt, "imports_from", line, None);
                    }
                }
            }
        } else if t == "module_instantiation" || t == "checker_instantiation" {
            if let Some(module_nid) = module_nid {
                let module_nid = module_nid.to_string();
                let inst_type = node
                    .child_by_field_name("module_type")
                    .map(|n| self.text(n).trim().to_string())
                    .or_else(|| self.first_identifier(Some(node)));
                if let Some(inst_type) = inst_type {
                    if !inst_type.is_empty() {
                        let line = node.start_position().row + 1;
                        let tgt = make_id([inst_type.as_str()]);
                        self.add_node(&tgt, &inst_type, line);
                        self.add_edge(&module_nid, &tgt, "instantiates", line, None);
                    }
                }
            }
        }

        for child in kids(node) {
            self.walk(child, module_nid);
        }
    }

    // ── Pass 2: SystemVerilog class semantics (regex) ────────────────────────

    fn augment_classes(&mut self, raw: &str) {
        // label → node id, seeded from the AST-pass nodes.
        let mut label_to_nid: HashMap<String, String> = HashMap::new();
        for n in &self.nodes {
            if let (Some(l), Some(i)) = (
                n.get("label").and_then(|v| v.as_str()),
                n.get("id").and_then(|v| v.as_str()),
            ) {
                label_to_nid.insert(l.to_string(), i.to_string());
            }
        }

        let text = strip_comments(raw);
        let class_re =
            Regex::new(r"(?s)\b(?:(interface)\s+)?class\s+(\w+)([^;{]*)\s*;(.*?)\bendclass\b")
                .unwrap();
        let ext_re = Regex::new(r"\bextends\s+(\w+)").unwrap();
        let impl_re = Regex::new(r"\bimplements\s+([^;{]+)").unwrap();
        let type_param_re = Regex::new(r"\btype\s+(\w+)").unwrap();
        let func_re = func_regex();
        let param_re = param_regex();
        let field_re = Regex::new(
            r"(?m)^\s*(?:(?:rand|randc|local|protected|static|const|automatic|var)\s+)*([A-Za-z_]\w*(?:\s*#\s*\([^;]+?\))?)\s+\w+\s*;",
        )
        .unwrap();
        let func_block_re = Regex::new(r"(?s)\bfunction\b.*?\bendfunction\b").unwrap();

        for cm in class_re.captures_iter(&text) {
            let whole = cm.get(0).unwrap();
            let class_name = cm.get(2).unwrap().as_str().to_string();
            let header = cm.get(3).map(|m| m.as_str()).unwrap_or("");
            let body = cm.get(4).map(|m| m.as_str()).unwrap_or("");
            let line = line_for(&text, whole.start());
            let type_params: HashSet<String> = type_param_re
                .captures_iter(header)
                .map(|c| c[1].to_string())
                .collect();

            let class_nid = make_id([self.stem.as_str(), class_name.as_str()]);
            self.aug_add_node(&mut label_to_nid, &class_nid, &class_name, line);
            let f = self.file_nid.clone();
            self.add_edge(&f, &class_nid, "defines", line, None);

            if let Some(e) = ext_re.captures(header) {
                self.aug_edge(&mut label_to_nid, &class_nid, &e[1], "inherits", line, None);
            }
            if let Some(im) = impl_re.captures(header) {
                for iface in split_type_list(&im[1]) {
                    let iface = iface.split('#').next().unwrap_or("").trim();
                    self.aug_edge(
                        &mut label_to_nid,
                        &class_nid,
                        iface,
                        "implements",
                        line,
                        None,
                    );
                }
            }

            // Blank out function..endfunction (preserving newlines) for fields.
            let mut bwf = String::with_capacity(body.len());
            let mut last = 0;
            for m in func_block_re.find_iter(body) {
                bwf.push_str(&body[last..m.start()]);
                for c in m.as_str().chars() {
                    bwf.push(if c == '\n' { '\n' } else { ' ' });
                }
                last = m.end();
            }
            bwf.push_str(&body[last..]);

            for fm in field_re.captures_iter(&bwf) {
                let g1 = fm.get(1).unwrap();
                let field_line = line + bwf[..g1.start()].bytes().filter(|&b| b == b'\n').count();
                for (name, role) in collect_type_refs(g1.as_str(), false, &type_params) {
                    let ctx = if role == "generic_arg" {
                        "generic_arg"
                    } else {
                        "field"
                    };
                    self.aug_edge(
                        &mut label_to_nid,
                        &class_nid,
                        &name,
                        "references",
                        field_line,
                        Some(ctx),
                    );
                }
            }

            for fm in func_re.captures_iter(body) {
                let whole_f = fm.get(0).unwrap();
                let return_type = fm.get(1).unwrap().as_str();
                let func_name = fm.get(2).unwrap().as_str();
                let params = fm.get(3).unwrap().as_str();
                let func_line = line
                    + body[..whole_f.start()]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count();
                let func_nid = make_id([class_nid.as_str(), func_name]);
                self.aug_add_node(&mut label_to_nid, &func_nid, func_name, func_line);
                self.add_edge(&class_nid, &func_nid, "method", func_line, None);
                for (name, role) in collect_type_refs(return_type, false, &type_params) {
                    let ctx = if role == "generic_arg" {
                        "generic_arg"
                    } else {
                        "return_type"
                    };
                    self.aug_edge(
                        &mut label_to_nid,
                        &func_nid,
                        &name,
                        "references",
                        func_line,
                        Some(ctx),
                    );
                }
                for param in split_type_list(params) {
                    if let Some(pm) = param_re.captures(&param) {
                        for (name, role) in collect_type_refs(&pm[1], false, &type_params) {
                            let ctx = if role == "generic_arg" {
                                "generic_arg"
                            } else {
                                "parameter_type"
                            };
                            self.aug_edge(
                                &mut label_to_nid,
                                &func_nid,
                                &name,
                                "references",
                                func_line,
                                Some(ctx),
                            );
                        }
                    }
                }
            }
        }
    }

    fn aug_add_node(
        &mut self,
        label_to_nid: &mut HashMap<String, String>,
        nid: &str,
        label: &str,
        line: usize,
    ) {
        self.add_node(nid, label, line);
        label_to_nid.insert(label.to_string(), nid.to_string());
    }

    /// graphify `add_edge`+`ensure_type`: resolve target label to an existing
    /// node id or a freshly-created `stem_label` node, then emit the edge.
    fn aug_edge(
        &mut self,
        label_to_nid: &mut HashMap<String, String>,
        src: &str,
        target_label: &str,
        relation: &str,
        line: usize,
        context: Option<&str>,
    ) {
        let tgt = match label_to_nid.get(target_label) {
            Some(id) => id.clone(),
            None => {
                let nid = make_id([self.stem.as_str(), target_label]);
                self.aug_add_node(label_to_nid, &nid, target_label, line);
                nid
            }
        };
        self.add_edge(src, &tgt, relation, line, context);
    }
}

fn line_for(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

fn strip_comments(text: &str) -> String {
    let block = Regex::new(r"(?s)/\*.*?\*/").unwrap();
    let line = Regex::new(r"//[^\n]*").unwrap();
    let a = block.replace_all(text, "");
    line.replace_all(&a, "").into_owned()
}

const PARENS_INNER: &str = r"(?:[^()]|\([^()]*\))*";

fn func_regex() -> Regex {
    // \bfunction\s+(<type>(#(...))?)\s+(\w+)\s*\((inner)\)\s*;
    let parens = format!(r"\({PARENS_INNER}\)");
    Regex::new(&format!(
        r"(?m)\bfunction\s+([A-Za-z_]\w*(?:\s*#\s*{parens})?)\s+(\w+)\s*\(({PARENS_INNER})\)\s*;"
    ))
    .unwrap()
}

fn param_regex() -> Regex {
    let parens = format!(r"\({PARENS_INNER}\)");
    Regex::new(&format!(
        r"^\s*(?:input|output|inout|ref|const\s+ref)?\s*([A-Za-z_]\w*(?:\s*#\s*{parens})?)\s+\w+"
    ))
    .unwrap()
}

/// Depth-aware (`()`) comma split, trimmed, non-empty.
fn split_type_list(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            b',' if depth == 0 => {
                let item = text[start..idx].trim();
                if !item.is_empty() {
                    parts.push(item.to_string());
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    let item = text[start..].trim();
    if !item.is_empty() {
        parts.push(item.to_string());
    }
    parts
}

/// (name, role) refs from a type string; role is "generic_arg" or "type".
fn collect_type_refs(
    type_text: &str,
    generic: bool,
    skip: &HashSet<String>,
) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let text = type_text.trim();
    if text.is_empty() {
        return refs;
    }
    let head_re = Regex::new(r"^([A-Za-z_]\w*)").unwrap();
    if let Some(h) = head_re.captures(text) {
        let name = h[1].to_string();
        if !BUILTIN_TYPES.contains(&name.as_str())
            && !NON_TYPE_WORDS.contains(&name.as_str())
            && !skip.contains(&name)
        {
            refs.push((
                name,
                if generic { "generic_arg" } else { "type" }.to_string(),
            ));
        }
    }
    let params_re = Regex::new(&format!(r"#\s*\(({PARENS_INNER})\)")).unwrap();
    if let Some(p) = params_re.captures(text) {
        for arg in split_type_list(&p[1]) {
            refs.extend(collect_type_refs(&arg, true, skip));
        }
    }
    refs
}

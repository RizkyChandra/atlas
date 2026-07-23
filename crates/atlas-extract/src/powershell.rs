//! PowerShell extractor — a Rust port of graphify
//! `graphify/extractors/powershell.py::extract_powershell`.
//!
//! Emits a file node, functions (`contains`), classes (`contains`) with
//! `inherits`/`implements` bases, class methods (`method`), property/param/
//! return type references (sourceless stubs, `references`), `using`/dot-source/
//! `Import-Module` import edges (`imports_from`, dangling target), and in-file
//! `calls` edges (a command whose name matches a defined function/method label).
//!
//! Node ids key off the file stem to match graphify's *built* graph (see
//! `engine.rs`). Manifest `.psd1` extraction is out of scope (not dispatched).

use crate::{node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const PS_SKIP: &[&str] = &[
    "using",
    "return",
    "if",
    "else",
    "elseif",
    "foreach",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "catch",
    "finally",
    "throw",
    "break",
    "continue",
    "exit",
    "param",
    "begin",
    "process",
    "end",
    "import-module",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
        .expect("load powershell grammar");
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

    let mut ex = Ps {
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

    // label (normalized) → node id, last-writer-wins (graphify dict comprehension).
    let mut label_to_nid: HashMap<String, String> = HashMap::new();
    for n in &ex.nodes {
        if let (Some(label), Some(id)) = (
            n.get("label").and_then(|v| v.as_str()),
            n.get("id").and_then(|v| v.as_str()),
        ) {
            let key = label
                .trim_matches(|c| c == '(' || c == ')')
                .trim_start_matches('.')
                .to_lowercase();
            label_to_nid.insert(key, id.to_string());
        }
    }

    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (caller, body) in bodies {
        ex.walk_calls(body, &caller, &label_to_nid, &mut seen_pairs);
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Ps<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    function_bodies: Vec<(String, Node<'a>)>,
}

impl<'a> Ps<'a> {
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
        self.edges.push(crate::edge_map(
            src,
            tgt,
            relation,
            context,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// graphify `ensure_named_node`: in-file id if defined, else a SOURCELESS stub.
    fn ensure_named_node(&mut self, name: &str) -> String {
        let nid = make_id([self.stem.as_str(), name]);
        if self.seen.contains(&nid) {
            return nid;
        }
        let nid = make_id([name]);
        if !self.seen.contains(&nid) {
            self.seen.insert(nid.clone());
            self.nodes.push(node_map(&nid, name, "code", "", ""));
        }
        nid
    }

    fn find_script_block_body(&self, node: Node<'a>) -> Option<Node<'a>> {
        for child in crate::kids(node) {
            if child.kind() == "script_block" {
                for sc in crate::kids(child) {
                    if sc.kind() == "script_block_body" {
                        return Some(sc);
                    }
                }
                return Some(child);
            }
        }
        None
    }

    /// Drill a type_literal → type_spec → type_name → type_identifier text.
    fn ps_type_name(&self, type_literal: Node) -> Option<String> {
        for spec in crate::kids(type_literal) {
            if spec.kind() != "type_spec" {
                continue;
            }
            for tname in crate::kids(spec) {
                if tname.kind() != "type_name" {
                    continue;
                }
                for tid in crate::kids(tname) {
                    if tid.kind() == "type_identifier" {
                        return Some(self.text(tid));
                    }
                }
            }
        }
        None
    }

    fn first_child<'b>(&self, node: Node<'b>, kind: &str) -> Option<Node<'b>> {
        crate::kids(node).into_iter().find(|c| c.kind() == kind)
    }

    fn walk(&mut self, node: Node<'a>, parent_class_nid: Option<&str>) {
        let t = node.kind();

        if t == "function_statement" {
            if let Some(name_node) = self.first_child(node, "function_name") {
                let func_name = self.text(name_node);
                let line = node.start_position().row + 1;
                let func_nid = make_id([self.stem.as_str(), func_name.as_str()]);
                self.add_node(&func_nid, &format!("{func_name}()"), line);
                let f = self.file_nid.clone();
                self.add_edge(&f, &func_nid, "contains", None, line);
                if let Some(body) = self.find_script_block_body(node) {
                    self.function_bodies.push((func_nid, body));
                    // Walk body in main pass too so nested imports emit (#1331).
                    self.walk(body, parent_class_nid);
                }
            }
            return;
        }

        if t == "class_statement" {
            if let Some(name_node) = self.first_child(node, "simple_name") {
                let class_name = self.text(name_node);
                let line = node.start_position().row + 1;
                let class_nid = make_id([self.stem.as_str(), class_name.as_str()]);
                self.add_node(&class_nid, &class_name, line);
                let f = self.file_nid.clone();
                self.add_edge(&f, &class_nid, "contains", None, line);
                // Bases after ':' — first is superclass (inherits), rest implements.
                let mut colon_seen = false;
                let mut base_index = 0;
                for child in crate::kids(node) {
                    if child.kind() == ":" {
                        colon_seen = true;
                    } else if colon_seen && child.kind() == "simple_name" {
                        let base = self.text(child);
                        let base_nid = self.ensure_named_node(&base);
                        if base_nid != class_nid {
                            let rel = if base_index == 0 {
                                "inherits"
                            } else {
                                "implements"
                            };
                            self.add_edge(&class_nid, &base_nid, rel, None, line);
                        }
                        base_index += 1;
                    }
                }
                for child in crate::kids(node) {
                    self.walk(child, Some(&class_nid));
                }
            }
            return;
        }

        if t == "class_property_definition" {
            if let Some(class_nid) = parent_class_nid {
                let class_nid = class_nid.to_string();
                if let Some(type_literal) = self.first_child(node, "type_literal") {
                    if let Some(type_name) = self.ps_type_name(type_literal) {
                        let line = node.start_position().row + 1;
                        let tgt = self.ensure_named_node(&type_name);
                        if tgt != class_nid {
                            self.add_edge(&class_nid, &tgt, "references", Some("field"), line);
                        }
                    }
                }
            }
            return;
        }

        if t == "class_method_definition" {
            if let Some(name_node) = self.first_child(node, "simple_name") {
                let method_name = self.text(name_node);
                let line = node.start_position().row + 1;
                let method_nid = match parent_class_nid {
                    Some(p) => {
                        let nid = make_id([p, method_name.as_str()]);
                        self.add_node(&nid, &format!(".{method_name}()"), line);
                        self.add_edge(p, &nid, "method", None, line);
                        nid
                    }
                    None => {
                        let nid = make_id([self.stem.as_str(), method_name.as_str()]);
                        self.add_node(&nid, &format!("{method_name}()"), line);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &nid, "contains", None, line);
                        nid
                    }
                };
                // Return type: type_literal sibling of simple_name.
                if let Some(rt) = self.first_child(node, "type_literal") {
                    if let Some(rt_name) = self.ps_type_name(rt) {
                        let tgt = self.ensure_named_node(&rt_name);
                        if tgt != method_nid {
                            self.add_edge(
                                &method_nid,
                                &tgt,
                                "references",
                                Some("return_type"),
                                line,
                            );
                        }
                    }
                }
                // Parameter types.
                if let Some(param_list) = self.first_child(node, "class_method_parameter_list") {
                    for p in crate::kids(param_list) {
                        if p.kind() != "class_method_parameter" {
                            continue;
                        }
                        if let Some(pt) = self.first_child(p, "type_literal") {
                            if let Some(pt_name) = self.ps_type_name(pt) {
                                let p_line = p.start_position().row + 1;
                                let tgt = self.ensure_named_node(&pt_name);
                                if tgt != method_nid {
                                    self.add_edge(
                                        &method_nid,
                                        &tgt,
                                        "references",
                                        Some("parameter_type"),
                                        p_line,
                                    );
                                }
                            }
                        }
                    }
                }
                if let Some(body) = self.find_script_block_body(node) {
                    self.function_bodies.push((method_nid, body));
                }
            }
            return;
        }

        if t == "command" {
            self.handle_command(node);
            return;
        }

        for child in crate::kids(node) {
            self.walk(child, parent_class_nid);
        }
    }

    fn handle_command(&mut self, node: Node) {
        let line = node.start_position().row + 1;
        // Dot-sourcing: `. ./Shared.psm1`
        if let Some(invoke_op) = self.first_child(node, "command_invokation_operator") {
            if self.text(invoke_op).trim() == "." {
                if let Some(name_expr) = self.first_child(node, "command_name_expr") {
                    if let Some(name_node) = self.first_child(name_expr, "command_name") {
                        let raw_path = self.text(name_node);
                        // Strip leading ./ .\ dots, drop extension, take basename.
                        let no_prefix =
                            raw_path.trim_start_matches(|c| c == '.' || c == '/' || c == '\\');
                        let no_ext = match no_prefix.rfind('.') {
                            Some(i) => &no_prefix[..i],
                            None => no_prefix,
                        };
                        let module = no_ext.replace('\\', "/");
                        let module = module.rsplit('/').next().unwrap_or(&module);
                        if !module.is_empty() {
                            let tgt = make_id([module]);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &tgt, "imports_from", None, line);
                        }
                    }
                }
                return;
            }
        }

        let Some(cmd_name_node) = self.first_child(node, "command_name") else {
            return;
        };
        let cmd_text = self.text(cmd_name_node).to_lowercase();
        if cmd_text == "using" {
            let mut tokens: Vec<String> = Vec::new();
            for child in crate::kids(node) {
                if child.kind() == "command_elements" {
                    for el in crate::kids(child) {
                        if el.kind() == "generic_token" {
                            tokens.push(self.text(el));
                        }
                    }
                }
            }
            let module_tokens: Vec<&String> = tokens
                .iter()
                .filter(|t| {
                    !matches!(
                        t.to_lowercase().as_str(),
                        "namespace" | "module" | "assembly"
                    )
                })
                .collect();
            if let Some(last) = module_tokens.last() {
                let module = last.rsplit('.').next().unwrap_or(last);
                let tgt = make_id([module]);
                let f = self.file_nid.clone();
                self.add_edge(&f, &tgt, "imports_from", None, line);
            }
        } else if cmd_text == "import-module" {
            let mut module_name: Option<String> = None;
            let mut expect_name = false;
            for child in crate::kids(node) {
                if child.kind() != "command_elements" {
                    continue;
                }
                for el in crate::kids(child) {
                    match el.kind() {
                        "command_parameter" => {
                            let param = self.text(el);
                            let param = param.trim_start_matches('-').to_lowercase();
                            expect_name = param == "name" || param == "n";
                        }
                        "generic_token" => {
                            let token = self.text(el);
                            if module_name.is_none() || expect_name {
                                module_name = Some(token);
                                expect_name = false;
                            }
                        }
                        _ => {}
                    }
                }
            }
            if let Some(module_name) = module_name {
                let no_ext = match module_name.rfind('.') {
                    Some(i) => &module_name[..i],
                    None => &module_name,
                };
                let bare = no_ext
                    .rsplit(|c| c == '/' || c == '\\')
                    .next()
                    .unwrap_or(no_ext);
                if !bare.is_empty() {
                    let tgt = make_id([bare]);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &tgt, "imports_from", None, line);
                }
            }
        }
    }

    fn walk_calls(
        &mut self,
        node: Node,
        caller_nid: &str,
        label_to_nid: &HashMap<String, String>,
        seen_pairs: &mut HashSet<(String, String)>,
    ) {
        if matches!(node.kind(), "function_statement" | "class_statement") {
            return;
        }
        if node.kind() == "command" {
            if let Some(cmd_name_node) = self.first_child(node, "command_name") {
                let cmd_text = self.text(cmd_name_node);
                let lc = cmd_text.to_lowercase();
                if !PS_SKIP.contains(&lc.as_str()) {
                    if let Some(tgt) = label_to_nid.get(&lc) {
                        if tgt != caller_nid {
                            let pair = (caller_nid.to_string(), tgt.clone());
                            if seen_pairs.insert(pair) {
                                let line = node.start_position().row + 1;
                                self.add_edge(caller_nid, tgt, "calls", None, line);
                            }
                        }
                    }
                }
            }
        }
        for child in crate::kids(node) {
            self.walk_calls(child, caller_nid, label_to_nid, seen_pairs);
        }
    }
}

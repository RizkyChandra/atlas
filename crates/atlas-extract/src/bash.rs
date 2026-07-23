//! Bash extractor — a Rust port of graphify `graphify/extractors/bash.py`.
//!
//! Emits a file node, a script `__entry` node, function nodes, program-level
//! variable definitions, `source`/`.` import edges, `.sh` script-invocation
//! call edges, and cross-function `calls` edges (a call resolves only to a
//! function defined *in this file*). Bash nodes carry a `metadata`
//! `{language, kind}` map, matching graphify's built graph.
//!
//! Node ids are keyed off the file stem to match graphify's *built* graph (its
//! extractor keys off `make_id(str(path))`, then the build relativizes to the
//! stem — see `engine.rs`). The `__entry` id reproduces that relativized form:
//! `make_id(stem) + "_" + ext + "__entry"`.
//!
//! Known limitation (#2141): a call to a function defined in a *sourced* file
//! resolves to no edge — cross-file resolution is out of atlas's single-file
//! scope, and current graphify (the oracle) drops it too. See the regression
//! test in `tests/langs.rs`.

use crate::{edge_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id, normalize_id};
use atlas_core::Attrs;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

const SOURCE_COMMANDS: &[&str] = &["source", "."];
const SCRIPT_RUNNERS: &[&str] = &["bash", "sh", "zsh", "ksh", "dash"];
const EXPANSION_PARENTS: &[&str] = &["command_substitution", "process_substitution"];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("load bash grammar");
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
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let entry_nid = format!("{file_nid}_{}__entry", normalize_id(&ext));
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut ex = Bash {
        source,
        path: path.to_path_buf(),
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        entry_nid: entry_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        defined_functions: HashSet::new(),
        function_bodies: Vec::new(),
    };

    ex.add_node(&file_nid, &filename, 1, "file");
    ex.add_node(
        &entry_nid,
        &format!("{filename} script"),
        1,
        "bash_entrypoint",
    );
    let (f, e) = (ex.file_nid.clone(), ex.entry_nid.clone());
    ex.add_edge(&f, &e, "contains", None, 1);

    let root = tree.root_node();
    ex.prescan_functions(root);
    ex.walk(root, &f);

    // Second pass: cross-function calls. Top-level calls → the entrypoint.
    let mut top_seen = HashSet::new();
    let entry = ex.entry_nid.clone();
    ex.walk_calls(root, &entry, &mut top_seen);
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (fn_nid, body) in bodies {
        if let Some(b) = body {
            let mut seen = HashSet::new();
            ex.walk_calls(b, &fn_nid, &mut seen);
        }
    }

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct Bash<'a> {
    source: &'a [u8],
    path: std::path::PathBuf,
    str_path: String,
    stem: String,
    file_nid: String,
    entry_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    defined_functions: HashSet<String>,
    function_bodies: Vec<(String, Option<Node<'a>>)>,
}

impl<'a> Bash<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize, kind: &str) {
        if nid.is_empty() || !self.seen.insert(nid.to_string()) {
            return;
        }
        let mut m = Attrs::new();
        m.insert("id".into(), json!(nid));
        m.insert("label".into(), json!(label));
        m.insert("file_type".into(), json!("code"));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), json!(format!("L{line}")));
        m.insert(
            "metadata".into(),
            json!({ "language": "bash", "kind": kind }),
        );
        self.nodes.push(m);
    }

    fn add_edge(
        &mut self,
        src: &str,
        tgt: &str,
        relation: &str,
        context: Option<&str>,
        line: usize,
    ) {
        if src.is_empty() || tgt.is_empty() || src == tgt {
            return;
        }
        self.edges.push(edge_map(
            src,
            tgt,
            relation,
            context,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// Token filter: strip matching quotes, reject names with shell metachars.
    fn literal(&self, node: Node) -> Option<String> {
        let mut raw = self.text(node).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        let b = raw.as_bytes();
        if matches!(b[0], b'\'' | b'"') && b[b.len() - 1] == b[0] && raw.len() >= 2 {
            raw = raw[1..raw.len() - 1].to_string();
        }
        const META: &[&str] = &["$", "`", "$(", "<(", ">", "|", ";", "&"];
        if META.iter().any(|m| raw.contains(m)) {
            return None;
        }
        Some(raw)
    }

    fn is_inside_expansion(&self, node: Node) -> bool {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if EXPANSION_PARENTS.contains(&p.kind()) {
                return true;
            }
            parent = p.parent();
        }
        false
    }

    fn bash_func_name(&self, node: Node) -> Option<String> {
        for child in crate::kids(node) {
            if child.kind() == "word" {
                return self.literal(child);
            }
        }
        None
    }

    fn command_name_node<'b>(&self, node: Node<'b>) -> Option<Node<'b>> {
        node.child_by_field_name("name").or_else(|| node.child(0))
    }

    fn prescan_functions(&mut self, node: Node) {
        if node.kind() == "function_definition" {
            if let Some(name) = self.bash_func_name(node) {
                self.defined_functions.insert(name);
            }
        }
        for child in crate::kids(node) {
            self.prescan_functions(child);
        }
    }

    fn walk(&mut self, node: Node<'a>, parent_nid: &str) {
        match node.kind() {
            "function_definition" => {
                if let Some(name) = self.bash_func_name(node) {
                    let fn_nid = make_id([self.stem.as_str(), name.as_str()]);
                    let line = node.start_position().row + 1;
                    self.add_node(&fn_nid, &format!("{name}()"), line, "bash_function");
                    self.add_edge(parent_nid, &fn_nid, "defines", None, line);
                    self.defined_functions.insert(name);
                    let body = crate::kids(node)
                        .into_iter()
                        .find(|c| c.kind() == "compound_statement");
                    self.function_bodies.push((fn_nid.clone(), body));
                    if let Some(b) = body {
                        self.walk(b, &fn_nid);
                    }
                }
            }
            "command" => {
                if self.is_inside_expansion(node) {
                    return;
                }
                self.handle_command(node, parent_nid);
            }
            "declaration_command" => {
                if node.parent().map(|p| p.kind()) == Some("program") {
                    for child in crate::kids(node) {
                        if child.kind() == "variable_assignment" {
                            if let Some(var_node) = child.child_by_field_name("name") {
                                let var = self.text(var_node);
                                let var = var.trim();
                                if !var.is_empty() {
                                    let var_nid = make_id([self.stem.as_str(), var]);
                                    let line = child.start_position().row + 1;
                                    self.add_node(&var_nid, var, line, "code");
                                    let f = self.file_nid.clone();
                                    self.add_edge(&f, &var_nid, "defines", None, line);
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                for child in crate::kids(node) {
                    self.walk(child, parent_nid);
                }
            }
        }
    }

    fn handle_command(&mut self, node: Node, parent_nid: &str) {
        let Some(name_node) = self.command_name_node(node) else {
            return;
        };
        let cmd = self.literal(name_node);
        let args: Vec<Node> = crate::kids(node)
            .into_iter()
            .filter(|c| {
                matches!(c.kind(), "word" | "string" | "concatenation") && c.id() != name_node.id()
            })
            .collect();

        let is_source = cmd
            .as_deref()
            .map(|c| SOURCE_COMMANDS.contains(&c))
            .unwrap_or(false)
            && !cmd
                .as_deref()
                .map(|c| self.defined_functions.contains(c))
                .unwrap_or(false);

        if is_source {
            if let Some(arg0) = args.first() {
                let raw = self.text(*arg0);
                let raw = raw.trim().trim_matches(|c| c == '\'' || c == '"');
                let line = node.start_position().row + 1;
                if raw.starts_with('.') || raw.starts_with('/') {
                    let resolved = normalize_abs(
                        &self
                            .path
                            .parent()
                            .unwrap_or_else(|| Path::new(""))
                            .join(raw),
                    );
                    // Only emit the edge if the target exists on disk (bash.py B-1).
                    if resolved.exists() {
                        let tgt = make_id([resolved.to_string_lossy().as_ref()]);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &tgt, "imports_from", Some("import"), line);
                    }
                } else {
                    let tgt = make_id([raw]);
                    if !tgt.is_empty() {
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &tgt, "imports", Some("import"), line);
                    }
                }
            }
            return;
        }

        // Script invocation: `./x.sh` or `bash x.sh` → calls the script's entry.
        let Some(cmd) = cmd else { return };
        if self.defined_functions.contains(&cmd) {
            return;
        }
        let mut raw: Option<String> = if cmd.ends_with(".sh") {
            Some(cmd.clone())
        } else {
            None
        };
        if SCRIPT_RUNNERS.contains(&cmd.as_str()) {
            if let Some(a0) = args.first() {
                raw = self.literal(*a0);
            }
        }
        if let Some(raw) = raw {
            if raw.ends_with(".sh") {
                let resolved = normalize_abs(
                    &self
                        .path
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .join(&raw),
                );
                if resolved.is_file() {
                    let caller = if parent_nid == self.file_nid {
                        self.entry_nid.clone()
                    } else {
                        parent_nid.to_string()
                    };
                    let tgt = format!("{}__entry", make_id([resolved.to_string_lossy().as_ref()]));
                    let line = node.start_position().row + 1;
                    self.add_edge(&caller, &tgt, "calls", Some("script_invocation"), line);
                }
            }
        }
    }

    fn walk_calls(
        &mut self,
        node: Node,
        func_nid: &str,
        seen_calls: &mut HashSet<(String, String)>,
    ) {
        for child in crate::kids(node) {
            if child.kind() == "function_definition" {
                continue; // nested defs are walked from their own body
            }
            if child.kind() == "command" && !self.is_inside_expansion(child) {
                if let Some(name_node) = self.command_name_node(child) {
                    if let Some(name) = self.literal(name_node) {
                        if self.defined_functions.contains(&name) {
                            let tgt = make_id([self.stem.as_str(), name.as_str()]);
                            let key = (func_nid.to_string(), tgt.clone());
                            if !tgt.is_empty() && seen_calls.insert(key) {
                                let line = child.start_position().row + 1;
                                self.add_edge(func_nid, &tgt, "calls", Some("call"), line);
                            }
                        }
                    }
                }
            }
            self.walk_calls(child, func_nid, seen_calls);
        }
    }
}

/// Lexical `.`/`..` collapse against an absolute-ish base, then canonicalize via
/// the filesystem where possible (mirrors Python `Path.resolve()` closely enough
/// for the `.exists()` / `.is_file()` gate).
fn normalize_abs(p: &Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

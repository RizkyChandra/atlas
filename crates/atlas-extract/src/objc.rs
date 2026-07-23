//! Objective-C extractor — a Rust port of graphify
//! `graphify/extractors/objc.py::extract_objc`, plus the single-file (self/super)
//! branch of the build-time member-call resolver `_resolve_objc_member_calls`.
//!
//! Emits a file node, `@interface`/`@implementation` class nodes and `@protocol`
//! nodes (`contains`), `inherits`/`implements` bases, methods (`method`, labels
//! carry the `+`/`-` sigil), property type references (`references`/field),
//! `#import`/`@import` edges (`imports`/import, dangling target), same-file
//! selector-suffix `calls`, `self.x` dot-syntax `accesses`, `[Foo alloc]`
//! `references`/type, and self/super member sends resolved to the enclosing
//! class (`calls` if a selector matches a sibling method, else `references`,
//! context `call`).
//!
//! Node ids key off the file stem to match graphify's *built* graph (see
//! `engine.rs`).
//!
//! Out of scope (single-file / cross-file resolver): `@selector(...)` refs,
//! capitalized-receiver and local-var-typed (`Foo *f; [f m]`) message sends —
//! those need the corpus-wide god-node guard / type table. Quoted `#import`
//! path resolution beyond a same-dir on-disk check is also out of scope.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const BLANK_MACROS: &[&str] = &["NS_ASSUME_NONNULL_BEGIN", "NS_ASSUME_NONNULL_END"];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_objc::LANGUAGE.into())
        .expect("load objc grammar");
    // Blank argument-less annotation macros to equal-length spaces so byte
    // offsets / lines are preserved and the interface still parses (#1475).
    let mut src = source.to_vec();
    for m in BLANK_MACROS {
        blank_all(&mut src, m.as_bytes());
    }
    let tree = match parser.parse(&src, None) {
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

    let mut ex = Objc {
        source: &src,
        path: path.to_path_buf(),
        str_path: path.to_string_lossy().into_owned(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        method_bodies: Vec::new(),
    };

    ex.add_node(&file_nid, &filename, 1);
    let root = tree.root_node();
    ex.walk(root, None);
    ex.resolve_calls();

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

fn blank_all(buf: &mut [u8], needle: &[u8]) {
    if needle.is_empty() || needle.len() > buf.len() {
        return;
    }
    let mut i = 0;
    while i + needle.len() <= buf.len() {
        if &buf[i..i + needle.len()] == needle {
            for b in &mut buf[i..i + needle.len()] {
                *b = b' ';
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
}

struct Objc<'a> {
    source: &'a [u8],
    path: std::path::PathBuf,
    str_path: String,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    /// (method_nid, node, container_nid) for method_definition bodies.
    method_bodies: Vec<(String, Node<'a>, String)>,
}

impl<'a> Objc<'a> {
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

    /// Every `type_identifier` under a node (descends generic_specifier/type_name).
    fn type_identifiers<'b>(&self, node: Node<'b>, out: &mut Vec<Node<'b>>) {
        if node.kind() == "type_identifier" {
            out.push(node);
            return;
        }
        for c in crate::kids(node) {
            self.type_identifiers(c, out);
        }
    }

    fn walk(&mut self, node: Node<'a>, parent_nid: Option<&str>) {
        let t = node.kind();
        let line = node.start_position().row + 1;

        match t {
            "preproc_include" => {
                for child in crate::kids(node) {
                    if child.kind() == "system_lib_string" {
                        let raw = self.text(child);
                        let raw = raw.trim_matches(|c| c == '<' || c == '>');
                        let module = raw.rsplit('/').next().unwrap_or(raw).replace(".h", "");
                        if !module.is_empty() {
                            let tgt = make_id([module.as_str()]);
                            let f = self.file_nid.clone();
                            self.add_edge(&f, &tgt, "imports", Some("import"), line);
                        }
                    } else if child.kind() == "string_literal" {
                        for sub in crate::kids(child) {
                            if sub.kind() == "string_content" {
                                let raw = self.text(sub);
                                // Same-dir on-disk resolution; else bare module stem.
                                let cand = self
                                    .path
                                    .parent()
                                    .unwrap_or_else(|| Path::new(""))
                                    .join(&raw);
                                let f = self.file_nid.clone();
                                if cand.is_file() {
                                    let resolved = cand.canonicalize().unwrap_or(cand);
                                    let tgt = make_id([resolved.to_string_lossy().as_ref()]);
                                    self.add_edge(&f, &tgt, "imports", Some("import"), line);
                                } else {
                                    let module =
                                        raw.rsplit('/').next().unwrap_or(&raw).replace(".h", "");
                                    if !module.is_empty() {
                                        let tgt = make_id([module.as_str()]);
                                        self.add_edge(&f, &tgt, "imports", Some("import"), line);
                                    }
                                }
                            }
                        }
                    }
                }
                return;
            }
            "module_import" => {
                if let Some(path_node) = node.child_by_field_name("path") {
                    let module = self.text(path_node);
                    let module = module.split('.').next().unwrap_or(&module).trim();
                    if !module.is_empty() {
                        let tgt = make_id([module]);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &tgt, "imports", Some("import"), line);
                    }
                }
                return;
            }
            "class_interface" => {
                self.class_interface(node, parent_nid, line);
                return;
            }
            "class_implementation" => {
                let name = crate::kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                    .map(|c| self.text(c));
                let Some(name) = name else {
                    for child in crate::kids(node) {
                        self.walk(child, parent_nid);
                    }
                    return;
                };
                let impl_nid = make_id([self.stem.as_str(), name.as_str()]);
                if !self.seen.contains(&impl_nid) {
                    self.add_node(&impl_nid, &name, line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &impl_nid, "contains", None, line);
                }
                for child in crate::kids(node) {
                    if child.kind() == "implementation_definition" {
                        for sub in crate::kids(child) {
                            self.walk(sub, Some(&impl_nid));
                        }
                    }
                }
                return;
            }
            "protocol_declaration" => {
                let name = crate::kids(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                    .map(|c| self.text(c));
                if let Some(name) = name {
                    let proto_nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&proto_nid, &format!("<{name}>"), line);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &proto_nid, "contains", None, line);
                    for child in crate::kids(node) {
                        if child.kind() == "protocol_reference_list" {
                            for sub in crate::kids(child) {
                                if sub.kind() == "identifier" {
                                    let base = self.text(sub);
                                    let base_nid = self.ensure_named_node(&base);
                                    if base_nid != proto_nid {
                                        self.add_edge(
                                            &proto_nid,
                                            &base_nid,
                                            "implements",
                                            None,
                                            line,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    for child in crate::kids(node) {
                        self.walk(child, Some(&proto_nid));
                    }
                }
                return;
            }
            "method_declaration" | "method_definition" => {
                let container = parent_nid.unwrap_or(&self.file_nid).to_string();
                let mut prefix = "-";
                for child in crate::kids(node) {
                    if matches!(child.kind(), "+" | "-") {
                        prefix = if child.kind() == "+" { "+" } else { "-" };
                        break;
                    }
                }
                let parts: Vec<String> = crate::kids(node)
                    .into_iter()
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| self.text(c))
                    .collect();
                if !parts.is_empty() {
                    let method_name = parts.concat();
                    let method_nid = make_id([container.as_str(), method_name.as_str()]);
                    self.add_node(&method_nid, &format!("{prefix}{method_name}"), line);
                    self.add_edge(&container, &method_nid, "method", None, line);
                    if t == "method_definition" {
                        self.method_bodies.push((method_nid, node, container));
                    }
                }
                return;
            }
            _ => {
                for child in crate::kids(node) {
                    self.walk(child, parent_nid);
                }
            }
        }
    }

    fn class_interface(&mut self, node: Node<'a>, parent_nid: Option<&str>, line: usize) {
        let identifiers: Vec<Node> = crate::kids(node)
            .into_iter()
            .filter(|c| c.kind() == "identifier")
            .collect();
        if identifiers.is_empty() {
            for child in crate::kids(node) {
                self.walk(child, parent_nid);
            }
            return;
        }
        let name = self.text(identifiers[0]);
        let cls_nid = make_id([self.stem.as_str(), name.as_str()]);
        self.add_node(&cls_nid, &name, line);
        let f = self.file_nid.clone();
        self.add_edge(&f, &cls_nid, "contains", None, line);

        let mut colon_seen = false;
        for child in crate::kids(node) {
            match child.kind() {
                ":" => colon_seen = true,
                "identifier" if colon_seen => {
                    let super_nid = self.ensure_named_node(&self.text(child));
                    self.add_edge(&cls_nid, &super_nid, "inherits", None, line);
                    colon_seen = false;
                }
                "parameterized_arguments" => {
                    for sub in crate::kids(child) {
                        if sub.kind() == "type_name" {
                            for s in crate::kids(sub) {
                                if s.kind() == "type_identifier" {
                                    let proto_nid = self.ensure_named_node(&self.text(s));
                                    self.add_edge(&cls_nid, &proto_nid, "implements", None, line);
                                }
                            }
                        }
                    }
                }
                "property_declaration" => {
                    let prop_line = child.start_position().row + 1;
                    for sub in crate::kids(child) {
                        if sub.kind() == "struct_declaration" {
                            let mut seen_types: HashSet<String> = HashSet::new();
                            for s in crate::kids(sub) {
                                if matches!(s.kind(), "struct_declarator" | ";") {
                                    continue;
                                }
                                let mut tids = Vec::new();
                                self.type_identifiers(s, &mut tids);
                                for ti in tids {
                                    let tname = self.text(ti);
                                    if !seen_types.insert(tname.clone()) {
                                        continue;
                                    }
                                    let type_nid = self.ensure_named_node(&tname);
                                    self.add_edge(
                                        &cls_nid,
                                        &type_nid,
                                        "references",
                                        Some("field"),
                                        prop_line,
                                    );
                                }
                            }
                        }
                    }
                }
                "method_declaration" => self.walk(child, Some(&cls_nid)),
                _ => {}
            }
        }
    }

    // ── second pass: resolve calls in method bodies ─────────────────────────
    fn resolve_calls(&mut self) {
        let all_method_nids: Vec<String> = self
            .nodes
            .iter()
            .filter_map(|n| n.get("id").and_then(|v| v.as_str()).map(String::from))
            .filter(|id| *id != self.file_nid)
            .collect();

        // container → set of its method nids.
        let mut class_method_nids: HashMap<String, HashSet<String>> = HashMap::new();
        for (m, _, c) in &self.method_bodies {
            class_method_nids
                .entry(c.clone())
                .or_default()
                .insert(m.clone());
        }

        let bodies: Vec<(String, Node, String)> = self.method_bodies.clone();
        let mut seen_calls: HashSet<(String, String)> = HashSet::new();
        // raw self/super sends: (caller, callee, container, send_line).
        let mut raw_self_super: Vec<(String, String, String, usize)> = Vec::new();

        for (caller_nid, body, container_nid) in &bodies {
            let sibling = class_method_nids
                .get(container_nid)
                .cloned()
                .unwrap_or_default();
            self.walk_calls(
                *body,
                caller_nid,
                container_nid,
                &sibling,
                &all_method_nids,
                &mut seen_calls,
                &mut raw_self_super,
            );
        }

        // Self/super branch of _resolve_objc_member_calls (single-file).
        // enclosing_type[method] = container; method_index[(container,key(sel))]=method.
        let mut enclosing_type: HashMap<String, String> = HashMap::new();
        let mut method_index: HashMap<(String, String), String> = HashMap::new();
        for e in &self.edges {
            if e.get("relation").and_then(|v| v.as_str()) != Some("method") {
                continue;
            }
            let (Some(src), Some(tgt)) = (
                e.get("source").and_then(|v| v.as_str()),
                e.get("target").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            enclosing_type
                .entry(tgt.to_string())
                .or_insert_with(|| src.to_string());
            if let Some(label) = self.node_label(tgt) {
                method_index.insert((src.to_string(), alnum_key(&label)), tgt.to_string());
            }
        }
        let mut existing_pairs: HashSet<(String, String)> = self
            .edges
            .iter()
            .filter_map(|e| {
                Some((
                    e.get("source").and_then(|v| v.as_str())?.to_string(),
                    e.get("target").and_then(|v| v.as_str())?.to_string(),
                ))
            })
            .collect();

        for (caller, callee, _container, line) in raw_self_super {
            let Some(type_nid) = enclosing_type.get(&caller).cloned() else {
                continue;
            };
            let method_nid = method_index
                .get(&(type_nid.clone(), alnum_key(&callee)))
                .cloned();
            let (target, relation) = match method_nid {
                Some(m) => (m, "calls"),
                None => (type_nid, "references"),
            };
            if target == caller || existing_pairs.contains(&(caller.clone(), target.clone())) {
                continue;
            }
            existing_pairs.insert((caller.clone(), target.clone()));
            // Mirror _resolve_objc_member_calls: EXTRACTED w/ confidence_score for
            // the type-qualified (self/super) branch, context "call", send line.
            let mut e = edge_map(
                &caller,
                &target,
                relation,
                Some("call"),
                &self.str_path,
                &format!("L{line}"),
            );
            e.insert("confidence_score".into(), serde_json::json!(1.0));
            self.edges.push(e);
        }
    }

    fn node_label(&self, nid: &str) -> Option<String> {
        self.nodes.iter().find_map(|n| {
            if n.get("id").and_then(|v| v.as_str()) == Some(nid) {
                n.get("label").and_then(|v| v.as_str()).map(String::from)
            } else {
                None
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_calls(
        &mut self,
        node: Node,
        caller_nid: &str,
        container_nid: &str,
        sibling: &HashSet<String>,
        all_method_nids: &[String],
        seen_calls: &mut HashSet<(String, String)>,
        raw_self_super: &mut Vec<(String, String, String, usize)>,
    ) {
        match node.kind() {
            "message_expression" => {
                let meth = node.child_by_field_name("method");
                let recv = node.child_by_field_name("receiver");
                let line = node.start_position().row + 1;
                // [Foo alloc] → references the allocated type.
                if let (Some(meth), Some(recv)) = (meth, recv) {
                    if meth.kind() == "identifier"
                        && self.text(meth) == "alloc"
                        && recv.kind() == "identifier"
                    {
                        let tname = self.text(recv);
                        let type_nid = self.ensure_named_node(&tname);
                        if type_nid != caller_nid {
                            self.add_edge(caller_nid, &type_nid, "references", Some("type"), line);
                        }
                    }
                }
                // Reconstruct the selector from every field=="method" identifier child.
                let mut sel_parts: Vec<String> = Vec::new();
                let mut cursor = node.walk();
                for (i, child) in node.children(&mut cursor).enumerate() {
                    if node.field_name_for_child(i as u32) == Some("method")
                        && child.kind() == "identifier"
                    {
                        sel_parts.push(self.text(child));
                    }
                }
                let method_name = sel_parts.concat();
                if !method_name.is_empty() {
                    let needle = make_id(["", method_name.as_str()]);
                    let needle = needle.trim_start_matches('_').to_string();
                    // Same-file suffix match → calls.
                    let matches: Vec<String> = all_method_nids
                        .iter()
                        .filter(|c| c.ends_with(&needle))
                        .cloned()
                        .collect();
                    for candidate in matches {
                        if candidate != caller_nid {
                            let pair = (caller_nid.to_string(), candidate.clone());
                            if seen_calls.insert(pair) {
                                self.add_edge(caller_nid, &candidate, "calls", Some("call"), line);
                            }
                        }
                    }
                    // Save a self/super raw call for the folded resolver.
                    if let Some(recv) = recv {
                        if recv.kind() == "identifier" {
                            let r = self.text(recv);
                            if r == "self" || r == "super" {
                                raw_self_super.push((
                                    caller_nid.to_string(),
                                    method_name.clone(),
                                    container_nid.to_string(),
                                    line,
                                ));
                            }
                        }
                    }
                }
            }
            "field_expression" => {
                let line = node.start_position().row + 1;
                for child in crate::kids(node) {
                    if child.kind() == "field_identifier" {
                        let field_name = self.text(child);
                        let target = make_id([container_nid, field_name.as_str()]);
                        if sibling.contains(&target) && target != caller_nid {
                            let pair = (caller_nid.to_string(), target.clone());
                            if seen_calls.insert(pair) {
                                self.add_edge(caller_nid, &target, "accesses", None, line);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        for child in crate::kids(node) {
            self.walk_calls(
                child,
                caller_nid,
                container_nid,
                sibling,
                all_method_nids,
                seen_calls,
                raw_self_super,
            );
        }
    }
}

/// graphify `_key`: strip non-alphanumerics (drops the `+`/`-` sigil), lowercase.
fn alnum_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

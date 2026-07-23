//! Config-driven generic AST extractor — a Rust port of graphify
//! `graphify/extractors/engine.py::_extract_generic` driven by `LanguageConfig`
//! (`graphify/extractors/models.py`) and the `_*_CONFIG` blocks in
//! `graphify/extract.py`.
//!
//! Covers the languages graphify routes through `_extract_generic` that this
//! wave targets: Python, JavaScript, TypeScript, Java, C, C++. Language-specific
//! branches are selected on [`Lang`], exactly as graphify keys them on
//! `config.ts_module`. Go and Rust have dedicated modules (graphify ships them
//! as standalone extractors, not configs).

use crate::{edge_map, is_builtin_global, kids, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id, normalize_id};
use atlas_core::Attrs;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    Js,
    Ts,
    Java,
    C,
    Cpp,
}

struct LanguageConfig {
    class_types: &'static [&'static str],
    function_types: &'static [&'static str],
    import_types: &'static [&'static str],
    call_types: &'static [&'static str],
    name_field: &'static str,
    body_field: &'static str,
    body_fallback: &'static [&'static str],
    call_function_field: &'static str,
    call_accessor_node_types: &'static [&'static str],
    call_accessor_field: &'static str,
    call_accessor_object_field: &'static str,
    function_boundary_types: &'static [&'static str],
}

fn config_for(lang: Lang) -> LanguageConfig {
    match lang {
        Lang::Python => LanguageConfig {
            class_types: &["class_definition"],
            function_types: &["function_definition"],
            import_types: &["import_statement", "import_from_statement"],
            call_types: &["call"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "function",
            call_accessor_node_types: &["attribute"],
            call_accessor_field: "attribute",
            call_accessor_object_field: "object",
            function_boundary_types: &["function_definition"],
        },
        Lang::Js => LanguageConfig {
            class_types: &["class_declaration"],
            function_types: &["function_declaration", "generator_function_declaration", "method_definition"],
            import_types: &["import_statement", "export_statement"],
            call_types: &["call_expression", "new_expression"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "function",
            call_accessor_node_types: &["member_expression"],
            call_accessor_field: "property",
            call_accessor_object_field: "object",
            function_boundary_types: &["function_declaration", "generator_function_declaration", "arrow_function", "method_definition"],
        },
        Lang::Ts => LanguageConfig {
            class_types: &["class_declaration", "abstract_class_declaration", "interface_declaration", "enum_declaration", "type_alias_declaration"],
            function_types: &["function_declaration", "generator_function_declaration", "method_definition", "method_signature"],
            import_types: &["import_statement", "export_statement"],
            call_types: &["call_expression", "new_expression"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "function",
            call_accessor_node_types: &["member_expression"],
            call_accessor_field: "property",
            call_accessor_object_field: "object",
            function_boundary_types: &["function_declaration", "generator_function_declaration", "arrow_function", "method_definition"],
        },
        Lang::Java => LanguageConfig {
            class_types: &["class_declaration", "interface_declaration", "record_declaration", "enum_declaration", "annotation_type_declaration"],
            function_types: &["method_declaration", "constructor_declaration"],
            import_types: &["import_declaration"],
            call_types: &["method_invocation", "object_creation_expression"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "name",
            call_accessor_node_types: &[],
            call_accessor_field: "",
            call_accessor_object_field: "",
            function_boundary_types: &["method_declaration", "constructor_declaration"],
        },
        Lang::C => LanguageConfig {
            class_types: &[],
            function_types: &["function_definition"],
            import_types: &["preproc_include"],
            call_types: &["call_expression"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "function",
            call_accessor_node_types: &["field_expression"],
            call_accessor_field: "field",
            call_accessor_object_field: "",
            function_boundary_types: &["function_definition"],
        },
        Lang::Cpp => LanguageConfig {
            class_types: &["class_specifier", "struct_specifier"],
            function_types: &["function_definition"],
            import_types: &["preproc_include"],
            call_types: &["call_expression"],
            name_field: "name",
            body_field: "body",
            body_fallback: &[],
            call_function_field: "function",
            call_accessor_node_types: &["field_expression", "qualified_identifier"],
            call_accessor_field: "field",
            call_accessor_object_field: "",
            function_boundary_types: &["function_definition"],
        },
    }
}

fn language(lang: Lang) -> tree_sitter::Language {
    match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    }
}

pub fn extract(path: &Path, source: &[u8], lang: Lang) -> ExtractResult {
    let mut parser = Parser::new();
    parser.set_language(&language(lang)).expect("load grammar");
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return ExtractResult { nodes: vec![], edges: vec![] },
    };

    let stem = file_stem(path);
    // graphify keys the file node off make_id(str(path)); its build pass then
    // relativizes every path-derived id to the scan-root stem. We run on
    // absolute paths and match the *built* graph, so we key the file node off
    // the stem directly (as M1 did) — making it a prefix of every symbol id.
    let file_nid = make_id([stem.as_str()]);
    let file_label = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut ex = Extractor {
        lang,
        cfg: config_for(lang),
        source,
        str_path: path.to_string_lossy().into_owned(),
        path: path.to_path_buf(),
        stem,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        callable: HashSet::new(),
        function_bodies: Vec::new(),
        label_to_nid: HashMap::new(),
        seen_call_pairs: HashSet::new(),
    };

    let root = tree.root_node();
    ex.add_node(&file_nid, &file_label, 1);
    ex.walk(root, None);

    ex.build_label_map();
    let bodies = std::mem::take(&mut ex.function_bodies);
    for (nid, body) in bodies {
        ex.walk_calls(body, &nid);
    }
    if lang == Lang::Python {
        ex.rationale(root);
    }

    ExtractResult { nodes: ex.nodes, edges: ex.edges }
}

struct Extractor<'a> {
    lang: Lang,
    cfg: LanguageConfig,
    source: &'a [u8],
    str_path: String,
    path: std::path::PathBuf,
    stem: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    callable: HashSet<String>,
    function_bodies: Vec<(String, Node<'a>)>,
    label_to_nid: HashMap<String, String>,
    seen_call_pairs: HashSet<(String, String)>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Role {
    Type,
    Generic,
}

impl<'a> Extractor<'a> {
    fn text(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }
    fn line(&self, n: Node) -> usize {
        n.start_position().row + 1
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if !self.seen.insert(nid.to_string()) {
            return;
        }
        self.nodes.push(node_map(nid, label, "code", &self.str_path, &format!("L{line}")));
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, context: Option<&str>, line: usize) {
        self.edges.push(edge_map(src, tgt, relation, context, &self.str_path, &format!("L{line}")));
    }

    /// graphify `ensure_named_node`: resolve a referenced name to an in-file
    /// node id (keyed off the file stem), else materialize a SOURCELESS stub so
    /// the cross-file rewire can later collapse it onto the real definition.
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

    fn find_body(&self, node: Node<'a>) -> Option<Node<'a>> {
        if let Some(b) = node.child_by_field_name(self.cfg.body_field) {
            return Some(b);
        }
        for child in kids(node) {
            if self.cfg.body_fallback.contains(&child.kind()) {
                return Some(child);
            }
        }
        None
    }

    fn resolve_func_name(&self, node: Node) -> Option<String> {
        if matches!(self.lang, Lang::C | Lang::Cpp) {
            let declarator = node.child_by_field_name("declarator")?;
            return if self.lang == Lang::Cpp {
                get_cpp_func_name(self.source, declarator)
            } else {
                get_c_func_name(self.source, declarator)
            };
        }
        let name_node = node.child_by_field_name(self.cfg.name_field)?;
        Some(self.text(name_node))
    }

    // ── main structural walk ────────────────────────────────────────────────
    fn walk(&mut self, node: Node<'a>, parent_class_nid: Option<&str>) {
        let t = node.kind();

        if self.cfg.import_types.contains(&t) {
            self.imports(node);
            // export_statement: recurse into children unless it is a re-export
            // (has a `from` source string).
            if matches!(self.lang, Lang::Js | Lang::Ts) && t == "export_statement" {
                let has_source = kids(node).iter().any(|c| c.kind() == "string");
                if !has_source {
                    for child in kids(node) {
                        self.walk(child, parent_class_nid);
                    }
                }
            }
            return;
        }

        if self.cfg.class_types.contains(&t) {
            self.handle_class(node, parent_class_nid);
            return;
        }

        // Field declarations (C++ data members, Java fields) inside a class body.
        if self.lang == Lang::Cpp && t == "field_declaration" {
            if let Some(p) = parent_class_nid {
                let p = p.to_string();
                self.cpp_field(node, &p);
                return;
            }
        }
        if self.lang == Lang::Java && t == "field_declaration" {
            if let Some(p) = parent_class_nid {
                let p = p.to_string();
                self.java_field(node, &p);
                return;
            }
        }

        if self.cfg.function_types.contains(&t) {
            self.handle_function(node, parent_class_nid);
            return;
        }

        // Java enum constants → case_of.
        if self.lang == Lang::Java && t == "enum_constant" {
            if let Some(p) = parent_class_nid {
                let name_node = match node.child_by_field_name("name") {
                    Some(n) => n,
                    None => return,
                };
                let const_name = self.text(name_node);
                let line = self.line(node);
                let const_nid = make_id([p, const_name.as_str()]);
                self.add_node(&const_nid, &const_name, line);
                self.add_edge(p, &const_nid, "case_of", None, line);
                for child in kids(node) {
                    if child.kind() == "class_body" {
                        for member in kids(child) {
                            self.walk(member, Some(&const_nid));
                        }
                    }
                }
                return;
            }
        }

        // Python `@decorator`-wrapped def: transparent so parent propagates.
        if self.lang == Lang::Python && t == "decorated_definition" {
            for child in kids(node) {
                self.walk(child, parent_class_nid);
            }
            return;
        }

        // Default recurse — graphify resets parent to None here.
        for child in kids(node) {
            self.walk(child, None);
        }
    }

    fn handle_class(&mut self, node: Node<'a>, parent_class_nid: Option<&str>) {
        let Some(name_node) = node.child_by_field_name(self.cfg.name_field) else { return };
        let class_name = self.text(name_node);
        let class_nid = make_id([self.stem.as_str(), class_name.as_str()]);
        let line = self.line(node);
        self.add_node(&class_nid, &class_name, line);
        self.callable.insert(class_nid.clone());
        match parent_class_nid {
            Some(p) if p != class_nid => self.add_edge(p, &class_nid, "contains", None, line),
            _ => {
                let f = self.file_nid.clone();
                self.add_edge(&f, &class_nid, "contains", None, line);
            }
        }

        match self.lang {
            Lang::Python => self.python_inheritance(node, &class_nid, line),
            Lang::Java => self.java_inheritance(node, node.kind(), &class_nid, line),
            Lang::Cpp => self.cpp_inheritance(node, &class_nid, line),
            _ => {}
        }

        if let Some(body) = self.find_body(node) {
            for child in kids(body) {
                self.walk(child, Some(&class_nid));
            }
        }
    }

    fn python_inheritance(&mut self, node: Node, class_nid: &str, line: usize) {
        if let Some(args) = node.child_by_field_name("superclasses") {
            for arg in kids(args) {
                if arg.kind() == "identifier" {
                    let base = self.text(arg);
                    let base_nid = self.ensure_named_node(&base);
                    self.add_edge(class_nid, &base_nid, "inherits", None, line);
                }
            }
        }
    }

    fn java_inheritance(&mut self, node: Node, t: &str, class_nid: &str, line: usize) {
        // extends → inherits (single superclass).
        if let Some(sup) = node.child_by_field_name("superclass") {
            for sub in kids(sup) {
                if sub.is_named() {
                    self.java_parent_type(sub, class_nid, "inherits", line);
                    break;
                }
            }
        }
        // implements → implements (interface type_list).
        if let Some(ifs) = node.child_by_field_name("interfaces") {
            for sub in kids(ifs) {
                if sub.kind() == "type_list" {
                    for tid in kids(sub) {
                        if tid.is_named() {
                            self.java_parent_type(tid, class_nid, "implements", line);
                        }
                    }
                }
            }
        }
        // interface `extends` → inherits.
        if t == "interface_declaration" {
            for child in kids(node) {
                if child.kind() == "extends_interfaces" {
                    for sub in kids(child) {
                        if sub.kind() == "type_list" {
                            for tid in kids(sub) {
                                if tid.is_named() {
                                    self.java_parent_type(tid, class_nid, "inherits", line);
                                }
                            }
                        }
                    }
                }
            }
        }
        // class-level annotations → references (attribute).
        for anno in java_annotation_names(self.source, node) {
            let tgt = self.ensure_named_node(&anno);
            if tgt != class_nid {
                self.add_edge(class_nid, &tgt, "references", Some("attribute"), line);
            }
        }
    }

    /// graphify `_emit_java_parent_type`: first `type`-role ref is the parent
    /// (inherits/implements), remaining `generic_arg` refs are references.
    fn java_parent_type(&mut self, type_node: Node, class_nid: &str, rel: &str, line: usize) {
        let mut refs = Vec::new();
        java_collect_type_refs(self.source, type_node, false, &mut refs);
        let mut parent_emitted = false;
        for (ref_name, role) in refs {
            if role == Role::Type && !parent_emitted {
                let base_nid = self.ensure_named_node(&ref_name);
                self.add_edge(class_nid, &base_nid, rel, None, line);
                parent_emitted = true;
            } else if role == Role::Generic {
                let tgt = self.ensure_named_node(&ref_name);
                if tgt != class_nid {
                    self.add_edge(class_nid, &tgt, "references", Some("generic_arg"), line);
                }
            }
        }
    }

    fn cpp_inheritance(&mut self, node: Node, class_nid: &str, line: usize) {
        for child in kids(node) {
            if child.kind() != "base_class_clause" {
                continue;
            }
            for sub in kids(child) {
                let (base, template_args) = match sub.kind() {
                    "type_identifier" => (self.text(sub), None),
                    "qualified_identifier" => {
                        let tail = sub.child_by_field_name("name");
                        (tail.map(|n| self.text(n)).unwrap_or_else(|| self.text(sub)), None)
                    }
                    "template_type" => {
                        let name = sub.child_by_field_name("name");
                        (
                            name.map(|n| self.text(n)).unwrap_or_else(|| self.text(sub)),
                            sub.child_by_field_name("arguments"),
                        )
                    }
                    _ => continue,
                };
                if base.is_empty() {
                    continue;
                }
                let base_nid = self.ensure_named_node(&base);
                self.add_edge(class_nid, &base_nid, "inherits", None, line);
                if let Some(args) = template_args {
                    let mut arg_refs = Vec::new();
                    for arg in kids(args) {
                        if arg.is_named() {
                            cpp_collect_type_refs(self.source, arg, true, &mut arg_refs);
                        }
                    }
                    for (ref_name, _) in arg_refs {
                        let tgt = self.ensure_named_node(&ref_name);
                        if tgt != class_nid {
                            self.add_edge(class_nid, &tgt, "references", Some("generic_arg"), line);
                        }
                    }
                }
            }
        }
    }

    fn handle_function(&mut self, node: Node<'a>, parent_class_nid: Option<&str>) {
        let Some(func_name) = self.resolve_func_name(node) else { return };
        if func_name.is_empty() || normalize_id(&func_name).is_empty() {
            return;
        }
        let line = self.line(node);
        let func_nid = match parent_class_nid {
            Some(p) => {
                let nid = make_id([p, func_name.as_str()]);
                self.add_node(&nid, &format!(".{func_name}()"), line);
                self.add_edge(p, &nid, "method", None, line);
                nid
            }
            None => {
                let nid = make_id([self.stem.as_str(), func_name.as_str()]);
                self.add_node(&nid, &format!("{func_name}()"), line);
                let f = self.file_nid.clone();
                self.add_edge(&f, &nid, "contains", None, line);
                nid
            }
        };
        self.callable.insert(func_nid.clone());

        match self.lang {
            Lang::Python => self.python_func_refs(node, &func_nid, line),
            Lang::Java => self.java_func_refs(node, &func_nid, line),
            Lang::C | Lang::Cpp => self.c_func_refs(node, &func_nid, line),
            _ => {}
        }

        if let Some(body) = self.find_body(node) {
            self.function_bodies.push((func_nid, body));
        }
    }

    fn python_func_refs(&mut self, node: Node, func_nid: &str, line: usize) {
        if let Some(params) = node.child_by_field_name("parameters") {
            for child in kids(params) {
                if matches!(child.kind(), "typed_parameter" | "typed_default_parameter") {
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let mut refs = Vec::new();
                        py_collect_type_refs(self.source, type_node, false, &mut refs);
                        for (name, role) in refs {
                            self.emit_ref_line(func_nid, &name, role, "parameter_type", line);
                        }
                    }
                }
            }
        }
        if let Some(rt) = node.child_by_field_name("return_type") {
            let mut refs = Vec::new();
            py_collect_type_refs(self.source, rt, false, &mut refs);
            for (name, role) in refs {
                self.emit_ref_line(func_nid, &name, role, "return_type", line);
            }
        }
    }

    fn java_func_refs(&mut self, node: Node, func_nid: &str, line: usize) {
        if let Some(params) = node.child_by_field_name("parameters") {
            for p in kids(params) {
                if p.kind() != "formal_parameter" {
                    continue;
                }
                if let Some(type_node) = p.child_by_field_name("type") {
                    let mut refs = Vec::new();
                    java_collect_type_refs(self.source, type_node, false, &mut refs);
                    for (name, role) in refs {
                        self.emit_ref_line(func_nid, &name, role, "parameter_type", line);
                    }
                }
            }
        }
        if let Some(return_node) = node.child_by_field_name("type") {
            let mut refs = Vec::new();
            java_collect_type_refs(self.source, return_node, false, &mut refs);
            for (name, role) in refs {
                self.emit_ref_line(func_nid, &name, role, "return_type", line);
            }
        }
        for anno in java_annotation_names(self.source, node) {
            let tgt = self.ensure_named_node(&anno);
            if tgt != func_nid {
                self.add_edge(func_nid, &tgt, "references", Some("attribute"), line);
            }
        }
    }

    fn c_func_refs(&mut self, node: Node, func_nid: &str, line: usize) {
        let collect = if self.lang == Lang::Cpp { cpp_collect_type_refs } else { c_collect_type_refs };
        // Return type first (graphify order → keep-first dedup keeps return_type).
        if let Some(return_node) = node.child_by_field_name("type") {
            let mut refs = Vec::new();
            collect(self.source, return_node, false, &mut refs);
            for (name, role) in refs {
                self.emit_ref_line(func_nid, &name, role, "return_type", line);
            }
        }
        // Unwrap pointer/reference declarators to the function_declarator, then params.
        let mut decl = node.child_by_field_name("declarator");
        while let Some(d) = decl {
            if matches!(d.kind(), "pointer_declarator" | "reference_declarator") {
                decl = d.child_by_field_name("declarator");
            } else {
                break;
            }
        }
        if let Some(d) = decl {
            if d.kind() == "function_declarator" {
                if let Some(params) = d.child_by_field_name("parameters") {
                    for p in kids(params) {
                        if p.kind() != "parameter_declaration" {
                            continue;
                        }
                        if let Some(ptype) = p.child_by_field_name("type") {
                            let mut refs = Vec::new();
                            collect(self.source, ptype, false, &mut refs);
                            for (name, role) in refs {
                                self.emit_ref_line(func_nid, &name, role, "parameter_type", line);
                            }
                        }
                    }
                }
            }
        }
    }

    fn emit_ref_line(&mut self, func_nid: &str, ref_name: &str, role: Role, type_ctx: &str, line: usize) {
        let ctx = if role == Role::Generic { "generic_arg" } else { type_ctx };
        let tgt = self.ensure_named_node(ref_name);
        if tgt != func_nid {
            self.add_edge(func_nid, &tgt, "references", Some(ctx), line);
        }
    }

    fn cpp_field(&mut self, node: Node, class_nid: &str) {
        // Skip method prototypes (declarator is/contains a function_declarator).
        let decls: Vec<Node> = node.children_by_field_name("declarator", &mut node.walk()).collect();
        let is_method = decls.iter().any(|d| {
            d.kind() == "function_declarator"
                || (matches!(d.kind(), "pointer_declarator" | "reference_declarator")
                    && kids(*d).iter().any(|c| c.kind() == "function_declarator"))
        });
        if !is_method {
            if let Some(type_node) = node.child_by_field_name("type") {
                let line = self.line(node);
                let mut refs = Vec::new();
                cpp_collect_type_refs(self.source, type_node, false, &mut refs);
                for (name, role) in refs {
                    self.emit_ref_line(class_nid, &name, role, "field", line);
                }
            }
        }
        for decl in decls {
            if let Some(name) = get_cpp_func_name(self.source, decl) {
                let line = self.line(decl);
                let field_nid = make_id([class_nid, name.as_str()]);
                self.add_node(&field_nid, &name, line);
                self.add_edge(class_nid, &field_nid, "defines", Some("field"), line);
            }
        }
    }

    fn java_field(&mut self, node: Node, class_nid: &str) {
        if let Some(type_node) = node.child_by_field_name("type") {
            let line = self.line(node);
            let mut refs = Vec::new();
            java_collect_type_refs(self.source, type_node, false, &mut refs);
            for (name, role) in refs {
                let ctx = if role == Role::Generic { "generic_arg" } else { "field" };
                let tgt = self.ensure_named_node(&name);
                if tgt != class_nid {
                    self.add_edge(class_nid, &tgt, "references", Some(ctx), line);
                }
            }
        }
    }

    // ── imports (per-language handler) ──────────────────────────────────────
    fn imports(&mut self, node: Node) {
        match self.lang {
            Lang::Python => self.import_python(node),
            Lang::Js | Lang::Ts => self.import_js(node),
            Lang::Java => self.import_java(node),
            Lang::C | Lang::Cpp => self.import_c(node),
        }
    }

    fn import_python(&mut self, node: Node) {
        let line = self.line(node);
        match node.kind() {
            "import_statement" => {
                for child in kids(node) {
                    if matches!(child.kind(), "dotted_name" | "aliased_import") {
                        let raw = self.text(child);
                        let raw_module = raw.split(" as ").next().unwrap_or("");
                        let module_name = raw_module.trim().trim_start_matches('.');
                        let tgt = make_id([module_name]);
                        let f = self.file_nid.clone();
                        self.add_edge(&f, &tgt, "imports", Some("import"), line);
                    }
                }
            }
            "import_from_statement" => {
                if let Some(m) = node.child_by_field_name("module_name") {
                    let raw = self.text(m);
                    let tgt = if raw.starts_with('.') {
                        self.resolve_relative_import(&raw)
                    } else {
                        make_id([raw.as_str()])
                    };
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &tgt, "imports_from", Some("import"), line);
                }
            }
            _ => {}
        }
    }

    fn resolve_relative_import(&self, raw: &str) -> String {
        let dots = raw.chars().take_while(|&c| c == '.').count();
        let module_name = raw.trim_start_matches('.');
        let mut base = Path::new(&self.str_path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for _ in 0..dots.saturating_sub(1) {
            base = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        }
        let rel = if module_name.is_empty() {
            "__init__.py".to_string()
        } else {
            format!("{}.py", module_name.replace('.', "/"))
        };
        make_id([base.join(rel).to_string_lossy().as_ref()])
    }

    /// graphify `_import_js`: relative/bare specifier resolved to a sibling path
    /// (`imports_from` to the module file id) plus one `imports` edge per named
    /// import (`_make_id(target_stem, name)`).
    fn import_js(&mut self, node: Node) {
        let is_reexport = node.kind() == "export_statement";
        if is_reexport {
            let has_from = kids(node).iter().any(|c| c.kind() == "string");
            if !has_from {
                return;
            }
        }
        // Find the module string (direct child, or inside import_require_clause).
        let mut module_string = None;
        for child in kids(node) {
            if child.kind() == "string" {
                module_string = Some(child);
                break;
            }
            if child.kind() == "import_require_clause" {
                module_string = kids(child).into_iter().find(|s| s.kind() == "string");
                break;
            }
        }
        let Some(ms) = module_string else { return };
        let raw = self.text(ms);
        let raw = raw.trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == ' ');
        let Some((tgt_nid, resolved_stem)) = resolve_js_import_target(raw, &self.path) else { return };
        let line = self.line(node);
        let f = self.file_nid.clone();
        let ctx = if is_reexport { "re-export" } else { "import" };
        self.add_edge(&f, &tgt_nid, "imports_from", Some(ctx), line);

        // Named import/re-export symbol edges.
        if is_reexport {
            for child in kids(node) {
                if child.kind() == "export_clause" {
                    for spec in kids(child) {
                        if spec.kind() == "export_specifier" {
                            if let Some(nn) = spec.child_by_field_name("name") {
                                let sym = self.text(nn);
                                if sym == "default" {
                                    continue;
                                }
                                let tgt = make_id([resolved_stem.as_str(), sym.as_str()]);
                                let f = self.file_nid.clone();
                                self.add_edge(&f, &tgt, "re_exports", Some("re-export"), line);
                            }
                        }
                    }
                }
            }
        } else {
            for child in kids(node) {
                if child.kind() == "import_clause" {
                    for sub in kids(child) {
                        if sub.kind() == "named_imports" {
                            for spec in kids(sub) {
                                if spec.kind() == "import_specifier" {
                                    if let Some(nn) = spec.child_by_field_name("name") {
                                        let sym = self.text(nn);
                                        let tgt = make_id([resolved_stem.as_str(), sym.as_str()]);
                                        let f = self.file_nid.clone();
                                        self.add_edge(&f, &tgt, "imports", Some("import"), line);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn import_java(&mut self, node: Node) {
        let line = self.line(node);
        for child in kids(node) {
            if matches!(child.kind(), "scoped_identifier" | "identifier") {
                let path_str = java_scoped_path(self.source, child);
                let parts: Vec<&str> = path_str.split('.').collect();
                let last = parts.last().copied().unwrap_or("");
                let module_name = last.trim_matches('*').trim_matches('.');
                let module_name = if module_name.is_empty() && parts.len() > 1 {
                    parts[parts.len() - 2]
                } else {
                    module_name
                };
                if !module_name.is_empty() {
                    let tgt = make_id([module_name]);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &tgt, "imports", Some("import"), line);
                }
                break;
            }
        }
    }

    fn import_c(&mut self, node: Node) {
        let line = self.line(node);
        for child in kids(node) {
            if matches!(child.kind(), "string_literal" | "system_lib_string" | "string") {
                let raw = self.text(child);
                let raw = raw.trim_matches(|c| c == '"' || c == '<' || c == '>' || c == ' ');
                let module_name = raw.rsplit('/').next().unwrap_or("").split('.').next().unwrap_or("");
                if !module_name.is_empty() {
                    let tgt = make_id([module_name]);
                    let f = self.file_nid.clone();
                    self.add_edge(&f, &tgt, "imports", Some("import"), line);
                }
                break;
            }
        }
    }

    // ── call graph ──────────────────────────────────────────────────────────
    fn build_label_map(&mut self) {
        for n in &self.nodes {
            let (Some(id), Some(label)) = (n.get("id").and_then(Value::as_str), n.get("label").and_then(Value::as_str)) else { continue };
            let normalised = label.trim_matches(|c| c == '(' || c == ')').trim_start_matches('.');
            self.label_to_nid.insert(normalised.to_string(), id.to_string());
        }
    }

    fn walk_calls(&mut self, node: Node<'a>, caller_nid: &str) {
        if self.cfg.function_boundary_types.contains(&node.kind()) {
            return;
        }
        if self.cfg.call_types.contains(&node.kind()) {
            self.handle_call(node, caller_nid);
        }
        for child in kids(node) {
            self.walk_calls(child, caller_nid);
        }
    }

    fn handle_call(&mut self, node: Node, caller_nid: &str) {
        let (callee, is_member, receiver) = self.extract_callee(node);
        let Some(name) = callee else { return };
        if name.is_empty() || is_builtin_global(&name) {
            return;
        }
        // Deferral: Java member calls always defer (receiver-typed resolution,
        // out of scope). Other member calls defer only for a capitalized
        // receiver (`ClassName.method()`).
        let deferred = if self.lang == Lang::Java {
            is_member
        } else {
            is_member
                && receiver
                    .as_deref()
                    .and_then(|r| r.chars().next())
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
        };
        if deferred {
            return;
        }
        let Some(tgt) = self.label_to_nid.get(&name).cloned() else { return };
        if tgt == caller_nid {
            return;
        }
        if self.seen_call_pairs.insert((caller_nid.to_string(), tgt.clone())) {
            let line = self.line(node);
            self.add_edge(caller_nid, &tgt, "calls", Some("call"), line);
        }
    }

    /// Returns (callee_name, is_member_call, simple_receiver_name).
    fn extract_callee(&self, node: Node) -> (Option<String>, bool, Option<String>) {
        match self.lang {
            Lang::Java => {
                if node.kind() == "object_creation_expression" {
                    if let Some(type_node) = node.child_by_field_name("type") {
                        let raw = self.text(type_node);
                        let base = raw.split('<').next().unwrap_or("").trim();
                        let name = base.rsplit('.').next().unwrap_or("").to_string();
                        if !name.is_empty() {
                            return (Some(name), false, None);
                        }
                    }
                    (None, false, None)
                } else {
                    // method_invocation
                    let callee = node.child_by_field_name("name").map(|n| self.text(n));
                    let mut is_member = false;
                    let mut receiver = None;
                    if let Some(recv) = node.child_by_field_name("object") {
                        is_member = true;
                        if recv.kind() == "identifier" {
                            receiver = Some(self.text(recv));
                        }
                    }
                    (callee, is_member, receiver)
                }
            }
            Lang::Cpp => {
                let Some(f) = node.child_by_field_name("function") else { return (None, false, None) };
                match f.kind() {
                    "identifier" => (Some(self.text(f)), false, None),
                    "field_expression" => {
                        let callee = f.child_by_field_name("field").map(|n| self.text(n));
                        let receiver = f.child_by_field_name("argument").and_then(|o| {
                            if o.kind() == "identifier" { Some(self.text(o)) } else { None }
                        });
                        (callee, true, receiver)
                    }
                    "qualified_identifier" => {
                        let callee = f.child_by_field_name("name").map(|n| self.text(n));
                        let receiver = f.child_by_field_name("scope").map(|s| self.text(s));
                        (callee, true, receiver)
                    }
                    _ => (None, false, None),
                }
            }
            _ => {
                // Generic (Python, JS, TS, C).
                let Some(f) = node.child_by_field_name(self.cfg.call_function_field) else {
                    return (None, false, None);
                };
                if f.kind() == "identifier" {
                    (Some(self.text(f)), false, None)
                } else if self.cfg.call_accessor_node_types.contains(&f.kind()) {
                    let mut callee = None;
                    let mut receiver = None;
                    if !self.cfg.call_accessor_field.is_empty() {
                        if let Some(attr) = f.child_by_field_name(self.cfg.call_accessor_field) {
                            callee = Some(self.text(attr));
                        }
                    }
                    if !self.cfg.call_accessor_object_field.is_empty() {
                        if let Some(obj) = f.child_by_field_name(self.cfg.call_accessor_object_field) {
                            if obj.kind() == "identifier" {
                                receiver = Some(self.text(obj));
                            }
                        }
                    }
                    (callee, true, receiver)
                } else {
                    (None, false, None)
                }
            }
        }
    }

    // ── rationale (Python docstrings + NOTE-comments) ───────────────────────
    fn rationale(&mut self, root: Node) {
        if !is_autogenerated_python(self.source) {
            if let Some((text, line)) = self.get_docstring(root) {
                let parent = self.file_nid.clone();
                self.add_rationale(&text, line, &parent);
            }
        }
        let file_nid = self.file_nid.clone();
        self.walk_docstrings(root, &file_nid);

        let text = String::from_utf8_lossy(self.source).into_owned();
        for (i, ln) in text.lines().enumerate() {
            let stripped = ln.trim();
            if RATIONALE_PREFIXES.iter().any(|p| stripped.starts_with(p)) {
                let fnid = self.file_nid.clone();
                self.add_rationale(stripped, i + 1, &fnid);
            }
        }
    }

    fn get_docstring(&self, body: Node) -> Option<(String, usize)> {
        let first = kids(body).into_iter().next()?;
        if first.kind() != "expression_statement" {
            return None;
        }
        for sub in kids(first) {
            if matches!(sub.kind(), "string" | "concatenated_string") {
                let raw = self.text(sub);
                let text = raw.trim_matches(|c| c == '"' || c == '\'').trim().to_string();
                if text.chars().count() > 20 {
                    return Some((text, first.start_position().row + 1));
                }
            }
        }
        None
    }

    fn add_rationale(&mut self, text: &str, line: usize, parent_nid: &str) {
        let label: String = text
            .chars()
            .take(80)
            .collect::<String>()
            .replace("\r\n", " ")
            .replace('\r', " ")
            .replace('\n', " ")
            .trim()
            .to_string();
        let rid = make_id([self.stem.as_str(), "rationale", line.to_string().as_str()]);
        if !self.seen.contains(&rid) {
            self.seen.insert(rid.clone());
            self.nodes.push(node_map(&rid, &label, "rationale", &self.str_path, &format!("L{line}")));
        }
        self.edges.push(edge_map(&rid, parent_nid, "rationale_for", None, &self.str_path, &format!("L{line}")));
    }

    fn walk_docstrings(&mut self, node: Node, parent_nid: &str) {
        match node.kind() {
            "class_definition" => {
                let (Some(name_node), Some(body)) =
                    (node.child_by_field_name("name"), node.child_by_field_name("body"))
                else { return };
                let class_name = self.text(name_node);
                let nid = make_id([self.stem.as_str(), class_name.as_str()]);
                if let Some((text, line)) = self.get_docstring(body) {
                    self.add_rationale(&text, line, &nid);
                }
                for child in kids(body) {
                    self.walk_docstrings(child, &nid);
                }
            }
            "function_definition" => {
                let (Some(name), Some(body)) =
                    (node.child_by_field_name("name"), node.child_by_field_name("body"))
                else { return };
                let func_name = self.text(name);
                let nid = if parent_nid != self.file_nid {
                    make_id([parent_nid, func_name.as_str()])
                } else {
                    make_id([self.stem.as_str(), func_name.as_str()])
                };
                if let Some((text, line)) = self.get_docstring(body) {
                    self.add_rationale(&text, line, &nid);
                }
            }
            _ => {
                for child in kids(node) {
                    self.walk_docstrings(child, parent_nid);
                }
            }
        }
    }
}

// ── Python type-annotation reference collection ─────────────────────────────

fn py_collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    let role = if generic { Role::Generic } else { Role::Type };
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "type" => {
            for c in kids(node) {
                if c.is_named() {
                    py_collect_type_refs(src, c, generic, out);
                }
            }
        }
        "identifier" => {
            let name = text(node);
            if !name.is_empty() && !is_type_container(&name) && !is_annotation_noise(&name) {
                out.push((name, role));
            }
        }
        "attribute" => {
            let full = text(node);
            let tail = full.rsplit('.').next().unwrap_or("").to_string();
            if !tail.is_empty() && !is_type_container(&tail) && !is_annotation_noise(&tail) {
                out.push((tail, role));
            }
        }
        "generic_type" => {
            for c in kids(node) {
                if c.kind() == "identifier" {
                    let container = text(c);
                    if !container.is_empty() && !is_type_container(&container) && !is_annotation_noise(&container) {
                        out.push((container, role));
                    }
                } else if c.kind() == "type_parameter" {
                    for sub in kids(c) {
                        if sub.is_named() {
                            py_collect_type_refs(src, sub, true, out);
                        }
                    }
                }
            }
        }
        "subscript" => {
            let value = node.child_by_field_name("value");
            if let Some(v) = value {
                py_collect_type_refs(src, v, generic, out);
            }
            for c in kids(node) {
                if Some(c.id()) == value.map(|v| v.id()) || !c.is_named() {
                    continue;
                }
                py_collect_type_refs(src, c, true, out);
            }
        }
        _ => {
            if node.is_named() {
                for c in kids(node) {
                    if c.is_named() {
                        py_collect_type_refs(src, c, generic, out);
                    }
                }
            }
        }
    }
}

fn is_type_container(s: &str) -> bool {
    const C: &[&str] = &[
        "list", "dict", "set", "tuple", "frozenset", "type",
        "List", "Dict", "Set", "Tuple", "FrozenSet", "Type",
        "Optional", "Union", "Sequence", "Iterable", "Mapping", "MutableMapping",
        "Iterator", "Callable", "Awaitable", "AsyncIterable", "AsyncIterator", "Coroutine",
        "Generator", "AsyncGenerator", "ContextManager", "AsyncContextManager",
        "Annotated", "ClassVar", "Final", "Literal", "Concatenate", "ParamSpec", "TypeVar",
        "None", "Ellipsis",
    ];
    C.contains(&s)
}

fn is_annotation_noise(s: &str) -> bool {
    const N: &[&str] = &[
        "str", "int", "float", "bool", "bytes", "bytearray", "complex", "object",
        "True", "False",
        "MagicMock", "Mock", "AsyncMock", "NonCallableMock",
        "NonCallableMagicMock", "PropertyMock", "patch", "sentinel",
    ];
    N.contains(&s)
}

/// graphify `_is_autogenerated_python`.
fn is_autogenerated_python(source: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&source[..source.len().min(2048)]);
    if ["DO NOT EDIT", "@generated", "Generated by the protocol buffer"].iter().any(|m| head.contains(m)) {
        return true;
    }
    let has_revision = head
        .lines()
        .any(|l| l.trim_start().starts_with("revision") && l.contains(':') || l.trim_start().starts_with("revision") && l.contains('='));
    if has_revision && head.contains("def upgrade(") && head.contains("down_revision") {
        return true;
    }
    if head.contains("class Migration(migrations.Migration)") && head.contains("operations") {
        return true;
    }
    false
}

const RATIONALE_PREFIXES: &[&str] = &[
    "# NOTE:", "# IMPORTANT:", "# HACK:", "# WHY:", "# RATIONALE:", "# TODO:", "# FIXME:",
];

// ── Java helpers (graphify engine.py) ───────────────────────────────────────

fn java_scoped_path(src: &[u8], node: Node) -> String {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "scoped_identifier" => {
                if let Some(name) = n.child_by_field_name("name") {
                    parts.push(text(name));
                }
                cur = n.child_by_field_name("scope");
            }
            "identifier" => {
                parts.push(text(n));
                break;
            }
            _ => break,
        }
    }
    parts.reverse();
    parts.join(".")
}

fn java_type_params_in_scope(src: &[u8], node: Node) -> HashSet<String> {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    let mut names = HashSet::new();
    let mut scope = Some(node);
    while let Some(s) = scope {
        if matches!(s.kind(), "class_declaration" | "interface_declaration" | "record_declaration" | "method_declaration" | "constructor_declaration") {
            if let Some(params) = s.child_by_field_name("type_parameters") {
                for param in kids(params) {
                    if param.kind() == "type_parameter" {
                        if let Some(nn) = kids(param).into_iter().find(|c| c.kind() == "type_identifier") {
                            names.insert(text(nn));
                        }
                    }
                }
            }
        }
        scope = s.parent();
    }
    names
}

fn java_collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    java_collect_type_refs_skip(src, node, generic, out, None);
}

fn java_collect_type_refs_skip(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>, skip: Option<&HashSet<String>>) {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    let owned_skip;
    let skip = match skip {
        Some(s) => s,
        None => {
            owned_skip = java_type_params_in_scope(src, node);
            &owned_skip
        }
    };
    let role = if generic { Role::Generic } else { Role::Type };
    match node.kind() {
        "integral_type" | "floating_point_type" | "boolean_type" | "void_type" => {}
        "type_identifier" => {
            let name = text(node);
            if !name.is_empty() && !skip.contains(&name) && !is_java_builtin(&name) {
                out.push((name, role));
            }
        }
        "scoped_type_identifier" => {
            let full = text(node);
            let tail = full.rsplit('.').next().unwrap_or("").to_string();
            if !tail.is_empty() && !is_java_builtin(&tail) {
                out.push((tail, role));
            }
        }
        "generic_type" => {
            for c in kids(node) {
                if matches!(c.kind(), "type_identifier" | "scoped_type_identifier") {
                    let full = text(c);
                    let tail = full.rsplit('.').next().unwrap_or("").to_string();
                    if !tail.is_empty()
                        && !is_java_builtin(&tail)
                        && (c.kind() == "scoped_type_identifier" || !skip.contains(&tail))
                    {
                        out.push((tail, role));
                    }
                    break;
                }
            }
            for c in kids(node) {
                if c.kind() == "type_arguments" {
                    for arg in kids(c) {
                        if arg.is_named() {
                            java_collect_type_refs_skip(src, arg, true, out, Some(skip));
                        }
                    }
                }
            }
        }
        "array_type" => {
            for c in kids(node) {
                if c.is_named() {
                    java_collect_type_refs_skip(src, c, generic, out, Some(skip));
                }
            }
        }
        _ => {
            if node.is_named() {
                for c in kids(node) {
                    if c.is_named() {
                        java_collect_type_refs_skip(src, c, generic, out, Some(skip));
                    }
                }
            }
        }
    }
}

fn java_annotation_names(src: &[u8], node: Node) -> Vec<String> {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    let mut names = Vec::new();
    let Some(modifiers) = kids(node).into_iter().find(|c| c.kind() == "modifiers") else { return names };
    for anno in kids(modifiers) {
        if !matches!(anno.kind(), "marker_annotation" | "annotation") {
            continue;
        }
        let name_node = anno.child_by_field_name("name").or_else(|| {
            kids(anno).into_iter().find(|s| matches!(s.kind(), "identifier" | "scoped_identifier" | "type_identifier"))
        });
        if let Some(nn) = name_node {
            let full = text(nn);
            let tail = full.rsplit('.').next().unwrap_or("").to_string();
            if !tail.is_empty() {
                names.push(tail);
            }
        }
    }
    names
}

fn is_java_builtin(s: &str) -> bool {
    const B: &[&str] = &[
        "Object", "String", "CharSequence", "StringBuilder", "StringBuffer",
        "Number", "Byte", "Short", "Integer", "Long", "Float", "Double",
        "Boolean", "Character", "Void", "Class", "Enum", "Record", "Math",
        "System", "Thread", "Runnable", "Comparable", "Iterable", "Cloneable",
        "AutoCloseable", "Appendable", "Readable", "Process", "ProcessBuilder",
        "Runtime", "Package", "ThreadLocal", "InheritableThreadLocal",
        "Throwable", "Exception", "RuntimeException", "Error",
        "IllegalArgumentException", "IllegalStateException", "NullPointerException",
        "IndexOutOfBoundsException", "ArrayIndexOutOfBoundsException",
        "ClassCastException", "NumberFormatException", "ArithmeticException",
        "UnsupportedOperationException", "InterruptedException",
        "CloneNotSupportedException", "SecurityException", "StackOverflowError",
        "OutOfMemoryError", "AssertionError",
        "Collection", "List", "ArrayList", "LinkedList", "Vector", "Stack",
        "Set", "HashSet", "LinkedHashSet", "TreeSet", "SortedSet", "NavigableSet",
        "EnumSet", "Map", "HashMap", "LinkedHashMap", "TreeMap", "SortedMap",
        "NavigableMap", "Hashtable", "EnumMap", "Properties", "Queue", "Deque",
        "ArrayDeque", "PriorityQueue", "Iterator", "ListIterator", "Comparator",
        "Optional", "OptionalInt", "OptionalLong", "OptionalDouble", "Collections",
        "Arrays", "Objects", "Date", "Calendar", "Random", "UUID", "Scanner",
        "StringJoiner", "StringTokenizer", "BitSet", "Spliterator", "Locale",
        "NoSuchElementException", "ConcurrentModificationException",
        "Stream", "IntStream", "LongStream", "DoubleStream", "Collector", "Collectors",
        "Function", "BiFunction", "Consumer", "BiConsumer", "Supplier",
        "Predicate", "BiPredicate", "UnaryOperator", "BinaryOperator",
        "IntFunction", "ToIntFunction", "ToLongFunction", "ToDoubleFunction",
        "Callable", "Future", "CompletableFuture", "CompletionStage", "Executor",
        "ExecutorService", "Executors", "ScheduledExecutorService", "TimeUnit",
        "ConcurrentHashMap", "ConcurrentMap", "CopyOnWriteArrayList",
        "BlockingQueue", "CountDownLatch", "Semaphore", "CyclicBarrier",
        "AtomicInteger", "AtomicLong", "AtomicBoolean", "AtomicReference",
        "Instant", "Duration", "Period", "LocalDate", "LocalTime", "LocalDateTime",
        "ZonedDateTime", "OffsetDateTime", "ZoneId", "ZoneOffset", "DayOfWeek",
        "Month", "Year", "Clock", "DateTimeFormatter",
        "IOException", "UncheckedIOException", "FileNotFoundException", "File",
        "InputStream", "OutputStream", "Reader", "Writer", "BufferedReader",
        "BufferedWriter", "InputStreamReader", "OutputStreamWriter", "FileReader",
        "FileWriter", "PrintStream", "PrintWriter", "ByteArrayInputStream",
        "ByteArrayOutputStream", "Serializable", "Closeable", "Path", "Paths", "Files",
        "BigDecimal", "BigInteger",
    ];
    B.contains(&s)
}

// ── C / C++ helpers ─────────────────────────────────────────────────────────

fn is_c_primitive(kind: &str) -> bool {
    matches!(kind, "primitive_type" | "sized_type_specifier" | "auto" | "placeholder_type_specifier")
}

fn c_collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    if is_c_primitive(node.kind()) {
        return;
    }
    let role = if generic { Role::Generic } else { Role::Type };
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "type_identifier" => {
            let t = text(node);
            if !t.is_empty() {
                out.push((t, role));
            }
        }
        "pointer_declarator" | "reference_declarator" | "array_declarator" | "type_qualifier"
        | "type_descriptor" | "abstract_pointer_declarator" | "abstract_reference_declarator"
        | "abstract_array_declarator" => {
            for c in kids(node) {
                if c.is_named() {
                    c_collect_type_refs(src, c, generic, out);
                }
            }
        }
        _ => {}
    }
}

fn cpp_collect_type_refs(src: &[u8], node: Node, generic: bool, out: &mut Vec<(String, Role)>) {
    if is_c_primitive(node.kind()) {
        return;
    }
    let role = if generic { Role::Generic } else { Role::Type };
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "type_identifier" => {
            let t = text(node);
            if !t.is_empty() {
                out.push((t, role));
            }
        }
        "qualified_identifier" => {
            if let Some(name) = node.child_by_field_name("name") {
                cpp_collect_type_refs(src, name, generic, out);
            }
        }
        "template_type" => {
            if let Some(name) = node.child_by_field_name("name") {
                let t = text(name);
                if !t.is_empty() {
                    out.push((t, role));
                }
            }
            if let Some(args) = node.child_by_field_name("arguments") {
                for c in kids(args) {
                    if c.is_named() {
                        cpp_collect_type_refs(src, c, true, out);
                    }
                }
            }
        }
        "type_descriptor" | "pointer_declarator" | "reference_declarator" | "array_declarator"
        | "type_qualifier" | "abstract_pointer_declarator" | "abstract_reference_declarator"
        | "abstract_array_declarator" => {
            for c in kids(node) {
                if c.is_named() {
                    cpp_collect_type_refs(src, c, generic, out);
                }
            }
        }
        _ => {}
    }
}

fn get_c_func_name(src: &[u8], node: Node) -> Option<String> {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    if node.kind() == "identifier" {
        return Some(text(node));
    }
    if let Some(decl) = node.child_by_field_name("declarator") {
        return get_c_func_name(src, decl);
    }
    kids(node).into_iter().find(|c| c.kind() == "identifier").map(text)
}

fn get_cpp_func_name(src: &[u8], node: Node) -> Option<String> {
    let text = |n: Node| String::from_utf8_lossy(&src[n.byte_range()]).into_owned();
    match node.kind() {
        "identifier" | "field_identifier" | "destructor_name" | "operator_name" | "qualified_identifier" => {
            return Some(text(node));
        }
        _ => {}
    }
    if let Some(decl) = node.child_by_field_name("declarator") {
        return get_cpp_func_name(src, decl);
    }
    kids(node).into_iter().find(|c| c.kind() == "identifier").map(text)
}

// ── JS/TS import resolution (graphify resolution.py) ────────────────────────

const JS_RESOLVE_EXTS: &[&str] = &[".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".vue", ".svelte", ".json"];
const JS_INDEX_FILES: &[&str] = &["index.ts", "index.tsx", "index.js", "index.jsx", "index.mjs", "index.cjs"];

/// Returns (target_nid, resolved_stem) for a JS/TS import specifier.
/// Mirrors `_resolve_js_import_target` → `_resolve_js_import_path`: relative
/// specifiers resolve against the file's dir (falling back to the raw candidate
/// when nothing exists on disk); bare specifiers namespace under `ref`.
fn resolve_js_import_target(raw: &str, file_path: &Path) -> Option<(String, String)> {
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('.') {
        let start_dir = file_path.parent().unwrap_or_else(|| Path::new(""));
        let resolved = resolve_js_import_path(&start_dir.join(raw));
        let stem = path_stem_posix(&resolved);
        return Some((make_id([resolved.to_string_lossy().as_ref()]), stem));
    }
    // Bare/scoped specifier → external, ref-namespaced (no local node).
    Some((make_id(["ref", raw]), String::new()))
}

fn resolve_js_import_path(candidate: &Path) -> std::path::PathBuf {
    let candidate = normalize_path(candidate);
    if candidate.is_file() {
        return candidate;
    }
    match candidate.extension().and_then(|s| s.to_str()) {
        Some("js") => {
            let ts = candidate.with_extension("ts");
            if ts.is_file() {
                return ts;
            }
        }
        Some("jsx") => {
            let tsx = candidate.with_extension("tsx");
            if tsx.is_file() {
                return tsx;
            }
        }
        _ => {}
    }
    let name = candidate.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = candidate.parent().unwrap_or_else(|| Path::new(""));
    for ext in JS_RESOLVE_EXTS {
        let with_ext = parent.join(format!("{name}{ext}"));
        if with_ext.is_file() {
            return with_ext;
        }
    }
    if candidate.is_dir() {
        for index in JS_INDEX_FILES {
            let ic = candidate.join(index);
            if ic.is_file() {
                return ic;
            }
        }
    }
    candidate
}

/// `os.path.normpath` — lexical `.`/`..` collapse, no disk access.
fn normalize_path(p: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            c => out.push(c),
        }
    }
    out.iter().collect()
}

/// graphify `_file_stem`: full path minus final extension, posix separators.
fn path_stem_posix(p: &Path) -> String {
    let no_ext = p.with_extension("");
    no_ext.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

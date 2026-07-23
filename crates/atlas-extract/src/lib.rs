//! atlas-extract — deterministic code-graph extraction, a Rust port of
//! graphify's `extract --code-only` AST pass. Milestone M1: the Python `.py`
//! extractor only.
//!
//! Ported from graphify `graphify/extractors/engine.py` (`_extract_generic`
//! walker + `_PYTHON_CONFIG`) and `graphify/extract.py`
//! (`_import_python`, `extract_python`, `_extract_python_rationale`).
//!
//! Node IDs come from [`atlas_core::ids`] (the one shared recipe). Output is the
//! raw `{nodes, edges}` dict graphify emits before its cross-file resolution and
//! build passes — so import/reference targets can be dangling (that is expected;
//! the build pass reconciles them).
//!
//! Scope kept deliberately to what graphify emits for a single Python file:
//! `contains`, `method`, `imports`, `imports_from`, `calls`, `references`,
//! `inherits`, `rationale_for`. Deferred cross-file resolution, INFERRED
//! `indirect_call` edges (dispatch tables / getattr / call-arg callbacks), and
//! every non-Python language are out of M1 — see the residual-gaps note in
//! `tests/httpx.rs`.

use atlas_core::ids::{file_stem, make_id, normalize_id};
use atlas_core::Attrs;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

/// Raw extraction output: node and edge attribute maps, in emission order.
pub struct ExtractResult {
    pub nodes: Vec<Attrs>,
    pub edges: Vec<Attrs>,
}

/// Extract the code graph for a single Python source file.
pub fn extract_file(path: impl AsRef<Path>) -> std::io::Result<ExtractResult> {
    let path = path.as_ref();
    let source = std::fs::read(path)?;
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("load tree-sitter-python grammar");
    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        // graphify returns {nodes:[], edges:[]} on a parse failure.
        None => return Ok(ExtractResult { nodes: vec![], edges: vec![] }),
    };

    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);
    let file_nid = make_id([stem.as_str()]);
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut ex = Extractor {
        source: &source,
        str_path,
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
    ex.rationale(root);

    Ok(ExtractResult { nodes: ex.nodes, edges: ex.edges })
}

struct Extractor<'a> {
    source: &'a [u8],
    str_path: String,
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

    /// graphify `ensure_named_node`: resolve a referenced name to an in-file node
    /// id, else materialize a SOURCELESS stub (empty source_file/location) so the
    /// cross-file rewire can later collapse it onto the real definition.
    fn ensure_named_node(&mut self, name: &str, _line: usize) -> String {
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

    // ── main structural walk ────────────────────────────────────────────────
    fn walk(&mut self, node: Node<'a>, parent_class_nid: Option<&str>) {
        match node.kind() {
            "import_statement" | "import_from_statement" => {
                self.import_python(node);
            }
            "class_definition" => {
                let Some(name_node) = node.child_by_field_name("name") else { return };
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
                // inheritance: only bare-identifier bases (matches graphify).
                if let Some(args) = node.child_by_field_name("superclasses") {
                    for arg in kids(args) {
                        if arg.kind() == "identifier" {
                            let base = self.text(arg);
                            let base_nid = self.ensure_named_node(&base, line);
                            self.add_edge(&class_nid, &base_nid, "inherits", None, line);
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    for child in kids(body) {
                        self.walk(child, Some(&class_nid));
                    }
                }
            }
            "function_definition" => {
                let Some(name_node) = node.child_by_field_name("name") else { return };
                let func_name = self.text(name_node);
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

                // parameter-type references
                if let Some(params) = node.child_by_field_name("parameters") {
                    for (ref_name, role) in self.py_param_refs(params) {
                        let ctx = if role == Role::Generic { "generic_arg" } else { "parameter_type" };
                        let tgt = self.ensure_named_node(&ref_name, line);
                        if tgt != func_nid {
                            self.edges.push(reference_edge(&func_nid, &tgt, ctx, &self.str_path, line));
                        }
                    }
                }
                // return-type references
                if let Some(rt) = node.child_by_field_name("return_type") {
                    let mut refs = Vec::new();
                    py_collect_type_refs(self.source, rt, false, &mut refs);
                    for (ref_name, role) in refs {
                        let ctx = if role == Role::Generic { "generic_arg" } else { "return_type" };
                        let tgt = self.ensure_named_node(&ref_name, line);
                        if tgt != func_nid {
                            self.edges.push(reference_edge(&func_nid, &tgt, ctx, &self.str_path, line));
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.function_bodies.push((func_nid, body));
                }
            }
            // `@decorator`-wrapped def: transparent so parent_class_nid propagates.
            "decorated_definition" => {
                for child in kids(node) {
                    self.walk(child, parent_class_nid);
                }
            }
            _ => {
                // Default recurse — graphify resets parent to None here.
                for child in kids(node) {
                    self.walk(child, None);
                }
            }
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
                        self.edges.push(import_edge(&f, &tgt, "imports", &self.str_path, line));
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
                    self.edges.push(import_edge(&f, &tgt, "imports_from", &self.str_path, line));
                }
            }
            _ => {}
        }
    }

    /// graphify: `from ..pkg.mod import x` → id keyed off the resolved file path
    /// so it matches that file's file-node id.
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

    // ── call graph ──────────────────────────────────────────────────────────
    fn build_label_map(&mut self) {
        for n in &self.nodes {
            let (Some(id), Some(label)) = (n.get("id").and_then(Value::as_str), n.get("label").and_then(Value::as_str)) else { continue };
            let normalised = label.trim_matches(|c| c == '(' || c == ')').trim_start_matches('.');
            self.label_to_nid.insert(normalised.to_string(), id.to_string());
        }
    }

    fn walk_calls(&mut self, node: Node<'a>, caller_nid: &str) {
        // function_definition is a body boundary: nested defs carry their own nid.
        if node.kind() == "function_definition" {
            return;
        }
        if node.kind() == "call" {
            self.handle_call(node, caller_nid);
        }
        for child in kids(node) {
            self.walk_calls(child, caller_nid);
        }
    }

    fn handle_call(&mut self, node: Node, caller_nid: &str) {
        let mut callee: Option<String> = None;
        let mut is_member = false;
        let mut receiver: Option<String> = None;

        if let Some(f) = node.child_by_field_name("function") {
            match f.kind() {
                "identifier" => callee = Some(self.text(f)),
                "attribute" => {
                    is_member = true;
                    if let Some(attr) = f.child_by_field_name("attribute") {
                        callee = Some(self.text(attr));
                    }
                    if let Some(obj) = f.child_by_field_name("object") {
                        if obj.kind() == "identifier" {
                            receiver = Some(self.text(obj));
                        }
                    }
                }
                // Other callee forms (chained call/subscript) never match an
                // in-file label, so graphify defers then drops them: skip.
                _ => {}
            }
        }

        let Some(name) = callee else { return };
        if name.is_empty() || is_builtin_global(&name) {
            return;
        }
        // A capitalized-receiver member call (`ClassName.method()`) defers to the
        // receiver-typed cross-file resolver (out of M1) → no in-file edge.
        let deferred = is_member
            && receiver
                .as_deref()
                .and_then(|r| r.chars().next())
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
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

    // ── rationale (docstrings + NOTE-comments) ──────────────────────────────
    fn rationale(&mut self, root: Node) {
        if !is_autogenerated_python(self.source) {
            if let Some((text, line)) = self.get_docstring(root) {
                let parent = self.file_nid.clone();
                self.add_rationale(&text, line, &parent);
            }
        }
        let file_nid = self.file_nid.clone();
        self.walk_docstrings(root, &file_nid);

        // `# NOTE:` / `# TODO:` … rationale comments.
        let text = String::from_utf8_lossy(self.source).into_owned();
        for (i, ln) in text.lines().enumerate() {
            let stripped = ln.trim();
            if RATIONALE_PREFIXES.iter().any(|p| stripped.starts_with(p)) {
                let fnid = self.file_nid.clone();
                self.add_rationale(stripped, i + 1, &fnid);
            }
        }
    }

    /// graphify `_get_docstring`: only the FIRST child of the body is inspected.
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
                let (Some(_name), Some(body)) =
                    (node.child_by_field_name("name"), node.child_by_field_name("body"))
                else { return };
                let func_name = self.text(node.child_by_field_name("name").unwrap());
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

    // ── Python type-annotation reference collection ─────────────────────────
    fn py_param_refs(&self, params: Node) -> Vec<(String, Role)> {
        let mut out = Vec::new();
        for child in kids(params) {
            if matches!(child.kind(), "typed_parameter" | "typed_default_parameter") {
                if let Some(type_node) = child.child_by_field_name("type") {
                    py_collect_type_refs(self.source, type_node, false, &mut out);
                }
            }
        }
        out
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Role {
    Type,
    Generic,
}

/// graphify `_python_collect_type_refs`. Builtin/typing containers are not
/// emitted, but their nested args still count as generic_arg.
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

// ── small helpers ──────────────────────────────────────────────────────────

fn kids(n: Node) -> Vec<Node> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

fn node_map(id: &str, label: &str, file_type: &str, source_file: &str, source_location: &str) -> Attrs {
    let mut m = Attrs::new();
    m.insert("id".into(), json!(id));
    m.insert("label".into(), json!(label));
    m.insert("file_type".into(), json!(file_type));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), json!(source_location));
    m
}

fn edge_map(src: &str, tgt: &str, relation: &str, context: Option<&str>, source_file: &str, source_location: &str) -> Attrs {
    let mut m = Attrs::new();
    m.insert("source".into(), json!(src));
    m.insert("target".into(), json!(tgt));
    m.insert("relation".into(), json!(relation));
    m.insert("confidence".into(), json!("EXTRACTED"));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), json!(source_location));
    m.insert("weight".into(), json!(1.0));
    if let Some(ctx) = context {
        m.insert("context".into(), json!(ctx));
    }
    m
}

fn import_edge(src: &str, tgt: &str, relation: &str, source_file: &str, line: usize) -> Attrs {
    edge_map(src, tgt, relation, Some("import"), source_file, &format!("L{line}"))
}

fn reference_edge(src: &str, tgt: &str, context: &str, source_file: &str, line: usize) -> Attrs {
    edge_map(src, tgt, "references", Some(context), source_file, &format!("L{line}"))
}

/// graphify `_is_autogenerated_python`: module docstrings of generated files are
/// change annotations, not rationale, so they are skipped.
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

/// graphify `_LANGUAGE_BUILTIN_GLOBALS` — names that would otherwise become
/// god-nodes as constructor/coercion call targets (JS + Python builtins).
fn is_builtin_global(s: &str) -> bool {
    const G: &[&str] = &[
        "String", "Number", "Boolean", "Object", "Array", "Symbol", "BigInt",
        "Date", "RegExp", "Error", "TypeError", "RangeError", "SyntaxError",
        "ReferenceError", "EvalError", "URIError",
        "Promise", "Map", "Set", "WeakMap", "WeakSet", "JSON", "Math",
        "Reflect", "Proxy", "Intl",
        "parseInt", "parseFloat", "isNaN", "isFinite",
        "encodeURIComponent", "decodeURIComponent", "encodeURI", "decodeURI",
        "URL", "URLSearchParams", "FormData", "Blob", "File",
        "Headers", "Request", "Response", "AbortController", "AbortSignal",
        "TextEncoder", "TextDecoder", "console",
        "str", "int", "float", "bool", "list", "dict", "set", "tuple", "bytes",
        "len", "range", "enumerate", "zip", "map", "filter", "sum", "min", "max",
        "print", "open", "isinstance", "type", "super", "sorted", "reversed",
        "any", "all", "abs", "round", "next", "iter", "hash", "id", "repr",
        "callable", "getattr", "setattr", "hasattr", "delattr", "vars", "dir",
    ];
    G.contains(&s)
}

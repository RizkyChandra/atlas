//! resolve.rs — corpus-level cross-file symbol resolution (build-time pass).
//!
//! atlas extracts each file in isolation, so import targets stay SOURCELESS
//! stubs and cross-file calls are dropped. This pass reconciles them across the
//! merged corpus, porting graphify's build-time Python resolution
//! (`graphify/symbol_resolution.py` + the Python paths of
//! `graphify/extractors/resolution.py`):
//!
//!   1. **Stub collapse** — a sourceless stub node (`source_file == ""`) whose
//!      normalized label matches exactly one real definition corpus-wide is
//!      remapped onto that def and dropped; its edges are repointed. (graphify
//!      build alias index / `_disambiguate_colliding_node_ids` collapse.)
//!   2. **Python `from M import S`** (re-parse each `.py`): emits a file→def
//!      `imports` edge for every resolvable imported symbol, and a class→def
//!      `uses` edge for every CLASS in the importing file (graphify
//!      `_augment_symbol_resolution_edges` + `_resolve_cross_file_imports`).
//!      `uses` targets are class entities only (label not ending `)`, not
//!      `_`-prefixed); `imports` targets any resolvable def (incl. functions).
//!   3. **Import-guided calls** — a bare call to an imported name that resolves
//!      to a unique `(module_stem, name)` definition becomes an EXTRACTED
//!      `calls` edge (graphify `resolve_python_import_guided_calls`).
//!
//! Non-Python cross-file resolution (JS/TS tsconfig aliases, C headers, bash
//! `source`, receiver-typed member calls) is out of scope; this pass only adds
//! Python edges and the language-agnostic stub collapse.
// ponytail: httpx/Python corpus is the gate; JS/C/bash cross-file resolvers are
// separate graphify passes, deferred until a non-Python corpus needs them.

use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

use crate::{dedupe_edges, dedupe_nodes, is_builtin_global};

fn attr<'a>(m: &'a Attrs, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Symbol-match key: strip a surrounding `()` and leading `.`, **case-preserved**
/// (graphify's Python resolvers key `symbol_nodes` by this). Case matters: the
/// class `Cookies` must not collide with the method `.cookies()`.
fn sym_key(label: &str) -> String {
    label
        .trim()
        .trim_matches(|c| c == '(' || c == ')')
        .trim_start_matches('.')
        .to_string()
}

/// graphify `node_is_resolvable_symbol`: a code node with a usable label.
fn is_resolvable(m: &Attrs) -> bool {
    if attr(m, "file_type") != "code" {
        return false;
    }
    let label = attr(m, "label").trim();
    if label.is_empty() {
        return false;
    }
    const SRC_EXTS: [&str; 7] = [".py", ".js", ".ts", ".tsx", ".java", ".go", ".rs"];
    if SRC_EXTS.iter().any(|e| label.ends_with(e)) {
        return false;
    }
    !sym_key(label).is_empty()
}

/// A node is a *class entity* (uses-edge target): a resolvable def whose label is
/// not a callable (`foo()`) and is not private (`_x`). graphify
/// `_resolve_cross_file_imports` Pass-1 filter.
fn is_class_entity(m: &Attrs) -> bool {
    let label = attr(m, "label").trim();
    !label.ends_with(')')
        && !label.ends_with(".py")
        && !label.starts_with('_')
        && attr(m, "file_type") != "rationale"
        && !label.is_empty()
}

/// Basename stem of a source path, e.g. `models.py` → `models` (graphify
/// `_node_source_stem` / `Path.stem`). Case-preserving to mirror graphify.
fn module_stem(source_file: &str) -> String {
    Path::new(source_file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn text(n: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[n.byte_range()]).into_owned()
}

/// Entry point: resolve import stubs and cross-file calls across the merged
/// corpus. Input is the deduped per-file merge; output is deduped again.
pub fn resolve_corpus(nodes: Vec<Attrs>, edges: Vec<Attrs>) -> (Vec<Attrs>, Vec<Attrs>) {
    let (nodes, mut edges) = collapse_stubs(nodes, edges);
    let new_edges = resolve_python(&nodes, &edges);
    edges.extend(new_edges);
    (dedupe_nodes(nodes), dedupe_edges(edges))
}

/// Rule 1: collapse each sourceless stub onto its unique real definition
/// (by normalized label), repoint edges, and drop the stub.
fn collapse_stubs(nodes: Vec<Attrs>, edges: Vec<Attrs>) -> (Vec<Attrs>, Vec<Attrs>) {
    // norm_label → set of real (sourced) def ids.
    let mut reals: HashMap<String, HashSet<String>> = HashMap::new();
    for n in &nodes {
        if attr(n, "source_file").is_empty() || !is_resolvable(n) {
            continue;
        }
        let id = attr(n, "id");
        if !id.is_empty() {
            reals
                .entry(sym_key(attr(n, "label")))
                .or_default()
                .insert(id.to_string());
        }
    }
    // stub id → real id, only when the label maps to exactly one real def.
    let mut remap: HashMap<String, String> = HashMap::new();
    for n in &nodes {
        if !attr(n, "source_file").is_empty() {
            continue;
        }
        let id = attr(n, "id");
        if id.is_empty() {
            continue;
        }
        if let Some(set) = reals.get(&sym_key(attr(n, "label"))) {
            if set.len() == 1 {
                let real = set.iter().next().unwrap();
                if real != id {
                    remap.insert(id.to_string(), real.clone());
                }
            }
        }
    }

    // Repoint `imports_from`/`imports` edges that target a bare module id
    // (`make_id([module_stem])`, e.g. `models`) onto the real file node
    // (`<dir>_models`). Per-file extraction can't know a sibling module's
    // path, so it emits the bare id; here the corpus is whole. External
    // modules (no matching file node, e.g. `hashlib`) are left dangling, as
    // graphify leaves them.
    let node_ids: HashSet<&str> = nodes.iter().map(|n| attr(n, "id")).collect();
    const SRC_EXTS: [&str; 7] = [".py", ".js", ".ts", ".tsx", ".java", ".go", ".rs"];
    for n in &nodes {
        let sf = attr(n, "source_file");
        let label = attr(n, "label");
        if sf.is_empty() || !SRC_EXTS.iter().any(|e| label.ends_with(e)) {
            continue;
        }
        let file_id = attr(n, "id");
        let module_id = make_id([module_stem(sf).as_str()]);
        if module_id != file_id && !node_ids.contains(module_id.as_str()) {
            remap
                .entry(module_id)
                .or_insert_with(|| file_id.to_string());
        }
    }

    if remap.is_empty() {
        return (nodes, edges);
    }
    let nodes = nodes
        .into_iter()
        .filter(|n| !remap.contains_key(attr(n, "id")))
        .collect();
    let edges = edges
        .into_iter()
        .map(|mut e| {
            for k in ["source", "target"] {
                if let Some(real) = remap.get(attr(&e, k)) {
                    e.insert(k.to_string(), json!(real));
                }
            }
            e
        })
        .collect();
    (nodes, edges)
}

/// Rules 2 & 3: re-parse each `.py` source and emit cross-file `imports`,
/// `uses`, and import-guided `calls` edges. Returns only the new edges.
fn resolve_python(nodes: &[Attrs], edges: &[Attrs]) -> Vec<Attrs> {
    let mut py_files: Vec<String> = nodes
        .iter()
        .map(|n| attr(n, "source_file").to_string())
        .filter(|s| s.ends_with(".py"))
        .collect();
    py_files.sort();
    py_files.dedup();
    if py_files.is_empty() {
        return vec![];
    }

    let node_ids: HashSet<&str> = nodes.iter().map(|n| attr(n, "id")).collect();

    // (module_stem, norm_label) → real def ids  — imports/calls targets.
    let mut sym_index: HashMap<(String, String), HashSet<String>> = HashMap::new();
    // (module_stem, exact_label) → class-entity id  — uses targets (last wins).
    let mut class_index: HashMap<(String, String), String> = HashMap::new();
    for n in nodes {
        let sf = attr(n, "source_file");
        if sf.is_empty() || !is_resolvable(n) {
            continue;
        }
        let id = attr(n, "id");
        if id.is_empty() {
            continue;
        }
        let stem = module_stem(sf);
        if stem.is_empty() {
            continue;
        }
        sym_index
            .entry((stem.clone(), sym_key(attr(n, "label"))))
            .or_default()
            .insert(id.to_string());
        if is_class_entity(n) {
            class_index.insert((stem, attr(n, "label").trim().to_string()), id.to_string());
        }
    }

    let mut known: HashSet<(String, String, String)> = edges
        .iter()
        .map(|e| {
            (
                attr(e, "source").to_string(),
                attr(e, "target").to_string(),
                attr(e, "relation").to_string(),
            )
        })
        .collect();

    let unique = |set: &HashSet<String>| -> Option<String> {
        if set.len() == 1 {
            Some(set.iter().next().unwrap().clone())
        } else {
            None
        }
    };

    let mut out: Vec<Attrs> = vec![];
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("load python grammar");

    for path_str in &py_files {
        let path = Path::new(path_str);
        let Ok(src) = std::fs::read(path) else {
            continue;
        };
        let Some(tree) = parser.parse(&src, None) else {
            continue;
        };
        let stem = file_stem(path);
        let file_nid = make_id([stem.as_str()]);

        // Classes defined in this file (uses-edge sources).
        let local_classes: Vec<String> = nodes
            .iter()
            .filter(|n| {
                attr(n, "source_file") == path_str
                    && attr(n, "id") != file_nid
                    && attr(n, "file_type") != "rationale"
                    && {
                        let l = attr(n, "label").trim();
                        !l.ends_with(')') && !l.ends_with(".py")
                    }
            })
            .map(|n| attr(n, "id").to_string())
            .collect();

        // (module_stem, imported_name, local_name, line) per `from M import ...`.
        let mut imports: Vec<(String, String, String, usize)> = vec![];
        // (caller_nid, callee, line) per bare call.
        let mut calls: Vec<(String, String, usize)> = vec![];
        let mut scope: Vec<String> = vec![];
        collect_py(
            tree.root_node(),
            &src,
            &stem,
            &mut scope,
            &mut imports,
            &mut calls,
        );

        // local binding → (module_stem, imported symbol) for call resolution.
        let mut alias: HashMap<String, (String, String)> = HashMap::new();
        for (ms, sym, local, _) in &imports {
            alias.insert(local.clone(), (ms.clone(), sym.clone()));
        }

        // imports (file → def, any resolvable) + uses (class → def, class only).
        for (ms, sym, _local, line) in &imports {
            if let Some(tgt) = sym_index.get(&(ms.clone(), sym_key(sym))).and_then(unique) {
                push_edge(
                    &mut out,
                    &mut known,
                    &file_nid,
                    &tgt,
                    "imports",
                    Some("import"),
                    "EXTRACTED",
                    1.0,
                    path_str,
                    *line,
                );
            }
            if let Some(tgt) = class_index.get(&(ms.clone(), sym.clone())) {
                for c in &local_classes {
                    if c != tgt {
                        push_edge(
                            &mut out, &mut known, c, tgt, "uses", None, "INFERRED", 0.8, path_str,
                            *line,
                        );
                    }
                }
            }
        }

        // import-guided cross-file calls.
        for (caller, callee, line) in &calls {
            if is_builtin_global(callee) || !node_ids.contains(caller.as_str()) {
                continue;
            }
            let Some((ms, sym)) = alias.get(callee) else {
                continue;
            };
            if let Some(tgt) = sym_index.get(&(ms.clone(), sym_key(sym))).and_then(unique) {
                if *caller != tgt {
                    push_edge(
                        &mut out,
                        &mut known,
                        caller,
                        &tgt,
                        "calls",
                        Some("call"),
                        "EXTRACTED",
                        1.0,
                        path_str,
                        *line,
                    );
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_edge(
    out: &mut Vec<Attrs>,
    known: &mut HashSet<(String, String, String)>,
    src: &str,
    tgt: &str,
    relation: &str,
    context: Option<&str>,
    confidence: &str,
    weight: f64,
    source_file: &str,
    line: usize,
) {
    if !known.insert((src.to_string(), tgt.to_string(), relation.to_string())) {
        return;
    }
    let mut m = Attrs::new();
    m.insert("source".into(), json!(src));
    m.insert("target".into(), json!(tgt));
    m.insert("relation".into(), json!(relation));
    m.insert("confidence".into(), json!(confidence));
    if let Some(c) = context {
        m.insert("context".into(), json!(c));
    }
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), json!(format!("L{line}")));
    m.insert("weight".into(), json!(weight));
    out.push(m);
}

/// Parse `from [.]MODULE import a, b as c` into `(module_stem, [(symbol, local)])`.
/// `symbol` is the imported name's last dotted component; `local` is its alias
/// (or the same name). graphify `_python_import_from_module` + `_python_imported_names`.
fn parse_import_from(node: Node, src: &[u8]) -> Option<(String, Vec<(String, String)>)> {
    let mut module = String::new();
    let mut past_import = false;
    let mut names: Vec<(String, String)> = vec![];
    let mut c = node.walk();
    let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_string();
    for child in node.children(&mut c) {
        match child.kind() {
            "import" => past_import = true,
            "relative_import" => {
                let mut cc = child.walk();
                for sub in child.children(&mut cc) {
                    if sub.kind() == "dotted_name" {
                        module = text(sub, src);
                    }
                }
            }
            "dotted_name" if !past_import => module = text(child, src),
            "dotted_name" if past_import => {
                let n = last(&text(child, src));
                names.push((n.clone(), n));
            }
            "aliased_import" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| last(&text(n, src)))
                    .unwrap_or_default();
                let local = child
                    .child_by_field_name("alias")
                    .map(|n| text(n, src))
                    .unwrap_or_else(|| name.clone());
                if !name.is_empty() {
                    names.push((name, local));
                }
            }
            _ => {}
        }
    }
    let stem = last(module.trim());
    if stem.is_empty() || names.is_empty() {
        return None;
    }
    Some((stem, names))
}

/// Walk the tree, tracking class/function scope, collecting import statements and
/// bare-identifier calls (with the enclosing symbol id as caller).
fn collect_py(
    node: Node,
    src: &[u8],
    stem: &str,
    scope: &mut Vec<String>,
    imports: &mut Vec<(String, String, String, usize)>,
    calls: &mut Vec<(String, String, usize)>,
) {
    let line = node.start_position().row + 1;
    match node.kind() {
        "import_from_statement" => {
            if let Some((ms, names)) = parse_import_from(node, src) {
                for (sym, local) in names {
                    imports.push((ms.clone(), sym, local, line));
                }
            }
            return;
        }
        "class_definition" | "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                scope.push(text(name_node, src));
                if let Some(body) = node.child_by_field_name("body") {
                    let mut c = body.walk();
                    for ch in body.children(&mut c) {
                        collect_py(ch, src, stem, scope, imports, calls);
                    }
                }
                scope.pop();
            }
            return;
        }
        "call" => {
            if let Some(f) = node.child_by_field_name("function") {
                if f.kind() == "identifier" {
                    let caller = if scope.is_empty() {
                        make_id([stem])
                    } else {
                        let mut parts = vec![stem.to_string()];
                        parts.extend(scope.iter().cloned());
                        make_id(parts)
                    };
                    calls.push((caller, text(f, src), line));
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        collect_py(ch, src, stem, scope, imports, calls);
    }
}

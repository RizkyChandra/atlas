//! M2 wave-1 gate: our extractor must reproduce graphify's built `graph.json`
//! for one sample file per language, compared as SETS of path-normalized
//! attribute maps (key order and the `_origin` field ignored).
//!
//! Oracle fixtures in `tests/fixtures/sample_<lang>.json` were produced by the
//! graphify venv on the sample file copied ALONE into a temp dir, then read from
//! `graphify-out/graph.json` (the built graph, which collapses parallel edges by
//! `(source,target,relation)` and same-id nodes — our extractor mirrors this).
//!
//! Path-derived ids differ between the oracle (temp dir) and our run (absolute
//! fixture path). `canon` neutralizes them with per-side prefix maps:
//!   * FILE — the file-node stem prefix (symbols keyed off the file).
//!   * DIR  — the JS/TS import sibling-dir prefix / the Go package-scope prefix.
//! JS/TS/Go oracles were generated in FIXED temp dirs so their DIR prefix is a
//! stable constant (`tmp_atlas_ora_js` / `tmp_atlas_ora_ts` / `atlas_ora_go`).
//!
//! Per-language residual deltas vs graphify are documented at each test.

use atlas_core::ids::{file_stem, make_id};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const GFIX: &str = "/home/yoshirakou/work/graphify/tests/fixtures";

/// A prefix rewrite: any id equal to `raw` becomes `token`; any id starting with
/// `raw_` becomes `token_<rest>`.
struct Remap {
    maps: Vec<(String, &'static str)>, // sorted longest-raw first
    basename_source_file: bool,
}

impl Remap {
    fn new(mut maps: Vec<(String, &'static str)>) -> Self {
        maps.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Remap {
            maps,
            basename_source_file: true,
        }
    }
    fn id(&self, s: &str) -> String {
        for (raw, token) in &self.maps {
            if s == raw {
                return token.to_string();
            }
            if let Some(rest) = s.strip_prefix(&format!("{raw}_")) {
                return format!("{token}_{rest}");
            }
        }
        s.to_string()
    }
}

fn basename(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        Path::new(s)
            .file_name()
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.to_string())
    }
}

fn canon(m: &Map<String, Value>, r: &Remap) -> String {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in m {
        if k == "_origin" {
            continue;
        }
        let nv = match (k.as_str(), v.as_str()) {
            ("id" | "source" | "target", Some(s)) => Value::String(r.id(s)),
            ("source_file", Some(s)) if r.basename_source_file => Value::String(basename(s)),
            _ => v.clone(),
        };
        out.insert(k.clone(), nv);
    }
    serde_json::to_string(&out).unwrap()
}

fn canon_set(items: &[Value], r: &Remap) -> Vec<String> {
    let mut v: Vec<String> = items
        .iter()
        .map(|it| canon(it.as_object().unwrap(), r))
        .collect();
    v.sort();
    v
}

fn diff(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

/// `my_extra` / `oracle_extra`: additional (raw_prefix, token) DIR maps.
fn check(
    fixture: &str,
    src_path: &str,
    oracle_json: &str,
    my_extra: Vec<(String, &'static str)>,
    oracle_extra: Vec<(String, &'static str)>,
) {
    let got = atlas_extract::extract_file(src_path).expect("extract");

    let my_file_nid = make_id([file_stem(Path::new(src_path)).as_str()]);
    // Oracle relativizes the file id to the bare-filename stem.
    let oracle_file_nid = make_id([file_stem(Path::new(&format!("sample.{fixture}"))).as_str()]);

    let mut my_maps = vec![(my_file_nid, "FILE")];
    my_maps.extend(my_extra);
    let mut or_maps = vec![(oracle_file_nid, "FILE")];
    or_maps.extend(oracle_extra);
    let my_r = Remap::new(my_maps);
    let or_r = Remap::new(or_maps);

    let fixture_json = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{oracle_json}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");
    let oracle: Value = serde_json::from_str(&fixture_json).expect("parse fixture");

    let my_nodes: Vec<Value> = got.nodes.into_iter().map(Value::Object).collect();
    let my_edges: Vec<Value> = got.edges.into_iter().map(Value::Object).collect();

    let want_nodes = canon_set(oracle["nodes"].as_array().unwrap(), &or_r);
    let got_nodes = canon_set(&my_nodes, &my_r);
    assert_eq!(
        got_nodes,
        want_nodes,
        "NODES mismatch for {oracle_json}\nmissing (oracle, not ours): {:?}\nextra (ours): {:?}",
        diff(&want_nodes, &got_nodes),
        diff(&got_nodes, &want_nodes)
    );

    let want_edges = canon_set(oracle["edges"].as_array().unwrap(), &or_r);
    let got_edges = canon_set(&my_edges, &my_r);
    assert_eq!(
        got_edges,
        want_edges,
        "EDGES mismatch for {oracle_json}\nmissing (oracle, not ours): {:?}\nextra (ours): {:?}",
        diff(&want_edges, &got_edges),
        diff(&got_edges, &want_edges)
    );
}

/// JavaScript. Sample is our own ESM analog of `sample.ts` (graphify ships no
/// `sample.js`). Import targets resolve `./models` against the file's dir.
/// EXACT match — no residual deltas for this fixture. Out of scope generally:
/// arrow functions, `this.x = () => {}` capture, CJS `require`, dynamic import,
/// TS-style type references (JS has none), INFERRED indirect_call callbacks.
#[test]
fn javascript_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/jsmod/sample.js",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "js",
        &src,
        "sample_js",
        vec![(dir, "DIR")],
        vec![("tmp_atlas_ora_js".into(), "DIR")],
    );
}

/// TypeScript. EXACT match for this fixture. Note graphify's generic engine
/// emits NO type-reference edges for TS/JS (unlike Python/Java/C/C++), so param
/// and return type annotations are intentionally not extracted. Out of scope:
/// TS namespaces/modules, decorators, `.tsx` (TSX grammar), constructor
/// parameter-property type table, everything listed under JS above.
#[test]
fn typescript_matches_oracle() {
    let src = format!("{GFIX}/sample.ts");
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "ts",
        &src,
        "sample_ts",
        vec![(dir, "DIR")],
        vec![("tmp_atlas_ora_ts".into(), "DIR")],
    );
}

/// Go. Types/methods key off the package scope (parent dir name → DIR); free
/// functions and the file node key off the stem (FILE). EXACT match: struct
/// fields (references), struct/interface embedding (embeds), method receiver
/// typing, param/return type references, and in-file calls all reproduced.
/// Out of scope: cross-file/package call resolution (single file only).
#[test]
fn go_matches_oracle() {
    let src = format!("{GFIX}/sample.go");
    let pkg = make_id([Path::new(&src)
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .as_ref()]);
    check(
        "go",
        &src,
        "sample_go",
        vec![(pkg, "DIR")],
        vec![("atlas_ora_go".into(), "DIR")],
    );
}

/// Rust. All type/method/free-fn ids key off the stem (FILE); external type
/// refs are sourceless stubs (bare ids, path-independent). EXACT match: structs,
/// enums (variant-payload field refs), traits (bound → inherits), impl blocks
/// (methods + `impl Trait for T` → implements), tuple structs, generic-arg refs,
/// `use` imports, and in-file calls. Out of scope: cross-file resolution.
#[test]
fn rust_matches_oracle() {
    let src = format!("{GFIX}/sample.rs");
    check("rs", &src, "sample_rs", vec![], vec![]);
}

/// Java. EXACT match: classes/interfaces/enums/records, extends→inherits,
/// implements, enum constants→case_of, `@Override`→references(attribute),
/// param/return/field type refs (generics as generic_arg), imports (last
/// segment), and in-file direct calls. Member calls (`items.add`) defer to the
/// receiver-typed resolver (out of scope) and emit no edge — matching the
/// oracle. Out of scope: object_creation to in-file types, nested-type
/// containment metadata, receiver typing.
#[test]
fn java_matches_oracle() {
    let src = format!("{GFIX}/sample.java");
    check("java", &src, "sample_java", vec![], vec![]);
}

/// C. EXACT match: functions (declarator-unwrapped names), `#include`→imports
/// (basename stem), user-typedef return/param type refs (deduped by build to one
/// edge per (src,tgt,relation)), and in-file calls. No classes in C.
#[test]
fn c_matches_oracle() {
    let src = format!("{GFIX}/sample.c");
    check("c", &src, "sample_c", vec![], vec![]);
}

/// C++. EXACT match: classes/structs, base_class_clause→inherits (+ template
/// args as generic_arg), methods, data members (references type + defines
/// field node), param/return type refs (qualified `std::string`→`string`),
/// `#include`→imports, and in-file/member calls. Out of scope: out-of-class
/// method definitions, local-var receiver typing.
#[test]
fn cpp_matches_oracle() {
    let src = format!("{GFIX}/sample.cpp");
    check("cpp", &src, "sample_cpp", vec![], vec![]);
}

//! Gate: our extractor must reproduce graphify's `extract --code-only` raw
//! output for the httpx sample files, compared as SETS of path-normalized
//! attribute maps (key order and the `_origin` field ignored).
//!
//! The oracle fixtures in `tests/fixtures/*.json` were produced by the graphify
//! venv on each file copied alone into a temp dir, so their ids/paths are keyed
//! off the bare filename (stem `utils`, source_file `utils.py`). We run on the
//! ABSOLUTE `worked/httpx/raw/*.py` path, whose stem/source_file differ only by
//! the directory prefix — both are pure functions of the path — so canon() maps
//! each side into a path-neutral space (file-node prefix → `FILE`, source_file →
//! basename) before the set comparison.
//!
//! RESIDUAL DELTAS: all 6 files match exactly. The only behaviors intentionally
//! omitted from M1 (INFERRED `indirect_call` edges, cross-file member-call
//! resolution) happen to emit nothing for these 6 files, so there is no delta to
//! document here. Files with call-arg callbacks / dispatch tables / getattr, or
//! `ClassName.method()` calls resolvable within one file, would diverge.

use atlas_core::ids::{file_stem, make_id};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const RAW_DIR: &str = "/home/yoshirakou/work/graphify/worked/httpx/raw";

/// Map one node/edge map into a path-neutral, order-independent canonical form:
/// drop `_origin`; rewrite id/source/target so the file-node prefix becomes
/// `FILE`; reduce source_file to its basename.
fn canon(m: &Map<String, Value>, file_nid: &str) -> String {
    let remap_id = |s: &str| -> String {
        if s == file_nid {
            "FILE".to_string()
        } else if let Some(rest) = s.strip_prefix(&format!("{file_nid}_")) {
            format!("FILE_{rest}")
        } else {
            s.to_string()
        }
    };
    let basename = |s: &str| -> String {
        if s.is_empty() {
            String::new()
        } else {
            Path::new(s)
                .file_name()
                .map(|b| b.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.to_string())
        }
    };

    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in m {
        if k == "_origin" {
            continue;
        }
        let nv = match (k.as_str(), v.as_str()) {
            ("id" | "source" | "target", Some(s)) => Value::String(remap_id(s)),
            ("source_file", Some(s)) => Value::String(basename(s)),
            _ => v.clone(),
        };
        out.insert(k.clone(), nv);
    }
    serde_json::to_string(&out).unwrap()
}

fn canon_set(items: &[Value], file_nid: &str) -> Vec<String> {
    let mut v: Vec<String> = items
        .iter()
        .map(|it| canon(it.as_object().unwrap(), file_nid))
        .collect();
    v.sort();
    v
}

fn check(file: &str) {
    let path = format!("{RAW_DIR}/{file}.py");
    let got = atlas_extract::extract_file(&path).expect("extract");
    let my_file_nid = make_id([file_stem(Path::new(&path)).as_str()]);

    let fixture = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{file}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");
    let oracle: Value = serde_json::from_str(&fixture).expect("parse fixture");
    // Oracle ids are keyed off the bare stem == basename-without-ext.
    let oracle_file_nid = make_id([file_stem(Path::new(&format!("{file}.py"))).as_str()]);

    let my_nodes: Vec<Value> = got.nodes.into_iter().map(Value::Object).collect();
    let my_edges: Vec<Value> = got.edges.into_iter().map(Value::Object).collect();

    let want_nodes = canon_set(oracle["nodes"].as_array().unwrap(), &oracle_file_nid);
    let got_nodes = canon_set(&my_nodes, &my_file_nid);
    assert_eq!(got_nodes, want_nodes, "NODES mismatch for {file}\nmissing (in oracle, not ours): {:?}\nextra (ours, not oracle): {:?}", diff(&want_nodes, &got_nodes), diff(&got_nodes, &want_nodes));

    let want_edges = canon_set(oracle["edges"].as_array().unwrap(), &oracle_file_nid);
    let got_edges = canon_set(&my_edges, &my_file_nid);
    assert_eq!(got_edges, want_edges, "EDGES mismatch for {file}\nmissing (in oracle, not ours): {:?}\nextra (ours, not oracle): {:?}", diff(&want_edges, &got_edges), diff(&got_edges, &want_edges));
}

fn diff(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

#[test]
fn utils_matches_oracle() {
    check("utils");
}

#[test]
fn auth_matches_oracle() {
    check("auth");
}

#[test]
fn client_matches_oracle() {
    check("client");
}

#[test]
fn exceptions_matches_oracle() {
    check("exceptions");
}

#[test]
fn models_matches_oracle() {
    check("models");
}

#[test]
fn transport_matches_oracle() {
    check("transport");
}

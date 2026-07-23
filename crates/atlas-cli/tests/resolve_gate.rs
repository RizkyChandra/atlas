//! Gate: the full atlas extract pipeline (per-file extract + cross-file
//! `resolve_corpus`) over graphify's httpx corpus must reproduce graphify's
//! `extract --code-only --no-cluster` BUILT multi-file graph.
//!
//! The fixture `fixtures/httpx_built.json` is that oracle's node-id set and
//! `(source, target, relation)` edge triples, produced by the graphify venv over
//! the same 6 files. graphify relativizes every id to the scan root (`auth`,
//! `models_request`); atlas keys ids off the absolute path (`<tmp>_auth`), so we
//! strip the shared directory prefix — `make_id([dir]) + "_"` — from each atlas
//! id before comparing. Both are pure functions of the path, so this is the only
//! systematic difference and it is orthogonal to resolution.
//!
//! Before this pass atlas produced 199 nodes / 321 edges (dangling import stubs,
//! no cross-file calls); the oracle is 195 / 461. This test asserts an exact
//! match — no residual delta.

use atlas_core::ids::make_id;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

const HTTPX_RAW: &str = "/home/yoshirakou/work/graphify/worked/httpx/raw";

#[test]
fn httpx_pipeline_matches_code_only_oracle() {
    if !Path::new(HTTPX_RAW).is_dir() {
        eprintln!("skipping: httpx corpus not present");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("httpx");
    std::fs::create_dir(&src).unwrap();
    for e in std::fs::read_dir(HTTPX_RAW).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("py") {
            std::fs::copy(&p, src.join(p.file_name().unwrap())).unwrap();
        }
    }

    let out = Command::new(env!("CARGO_BIN_EXE_atlas"))
        .args(["extract", src.to_str().unwrap(), "--no-viz"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let g = atlas_core::Graph::from_file(src.join("graphify-out/graph.json")).unwrap();

    // Strip the shared path prefix so atlas ids collapse to the oracle's
    // scan-root-relative ids.
    let prefix = format!("{}_", make_id([src.to_string_lossy().as_ref()]));
    let strip = |id: &str| id.strip_prefix(&prefix).unwrap_or(id).to_string();

    let got_nodes: BTreeSet<String> = g.node_ids().map(strip).collect();
    let got_edges: BTreeSet<(String, String, String)> = g
        .links
        .iter()
        .map(|e| {
            let s = |k| e.get(k).and_then(Value::as_str).unwrap_or("");
            (
                strip(s("source")),
                strip(s("target")),
                s("relation").to_string(),
            )
        })
        .collect();

    let fixture: Value = serde_json::from_str(include_str!("fixtures/httpx_built.json")).unwrap();
    let want_nodes: BTreeSet<String> = fixture["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let want_edges: BTreeSet<(String, String, String)> = fixture["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let a = e.as_array().unwrap();
            (
                a[0].as_str().unwrap().to_string(),
                a[1].as_str().unwrap().to_string(),
                a[2].as_str().unwrap().to_string(),
            )
        })
        .collect();

    let miss_n: Vec<_> = want_nodes.difference(&got_nodes).collect();
    let extra_n: Vec<_> = got_nodes.difference(&want_nodes).collect();
    assert!(
        miss_n.is_empty() && extra_n.is_empty(),
        "NODES differ: missing={miss_n:?} extra={extra_n:?}"
    );

    let miss_e: Vec<_> = want_edges.difference(&got_edges).collect();
    let extra_e: Vec<_> = got_edges.difference(&want_edges).collect();
    assert!(
        miss_e.is_empty() && extra_e.is_empty(),
        "EDGES differ: missing={miss_e:?} extra={extra_e:?}"
    );
}

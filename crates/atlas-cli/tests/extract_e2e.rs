//! End-to-end gate for `atlas extract` (+ query/export) on real source.
//!
//! Uses graphify's httpx corpus when present (the M10 gate corpus); otherwise
//! falls back to a tiny inline fixture so the test stays green in CI, where that
//! path does not exist.

use std::path::{Path, PathBuf};
use std::process::Command;

const HTTPX_RAW: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/graphify/httpx/raw"
);

fn atlas() -> Command {
    Command::new(env!("CARGO_BIN_EXE_atlas"))
}

/// Copy the httpx corpus into `dst` if available, else drop a two-file fixture.
fn seed_corpus(dst: &Path) {
    if Path::new(HTTPX_RAW).is_dir() {
        for e in std::fs::read_dir(HTTPX_RAW).unwrap().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("py") {
                std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
            }
        }
    } else {
        std::fs::write(
            dst.join("models.py"),
            "class Request:\n    def build(self):\n        return Response()\n\nclass Response:\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            dst.join("client.py"),
            "from models import Request\n\nclass Client:\n    def send(self):\n        return Request().build()\n",
        )
        .unwrap();
    }
}

#[test]
fn extract_produces_valid_graph_then_query_and_export() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    seed_corpus(&src);

    // extract
    let out = atlas()
        .args(["extract", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let graph_json: PathBuf = src.join("graphify-out/graph.json");
    assert!(graph_json.exists(), "graph.json not written");
    assert!(src.join("graphify-out/GRAPH_REPORT.md").exists());
    assert!(src.join("graphify-out/graph.html").exists());

    // loads + validates via atlas-core, with >0 nodes/edges
    let g = atlas_core::Graph::from_file(&graph_json).expect("load graph.json");
    assert!(g.validate().is_ok(), "schema validation failed");
    assert!(!g.nodes.is_empty(), "no nodes");
    assert!(!g.links.is_empty(), "no edges");

    // export against the produced graph
    let gp = graph_json.to_str().unwrap();
    for fmt in ["graphml", "cypher", "svg", "html"] {
        let e = atlas()
            .args(["export", fmt, "--graph", gp])
            .output()
            .unwrap();
        assert!(e.status.success(), "export {fmt} failed");
        assert!(!e.stdout.is_empty(), "export {fmt} produced nothing");
    }

    // query returns output referencing a real node
    let some_label = g.nodes[0]
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("Client");
    let q = atlas()
        .args(["query", some_label, "--graph", gp])
        .output()
        .unwrap();
    assert!(q.status.success(), "query failed");
    assert!(!q.stdout.is_empty(), "query produced nothing");
}

#[test]
fn extract_no_viz_skips_report_and_html() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    seed_corpus(&src);

    let out = atlas()
        .args(["extract", src.to_str().unwrap(), "--no-viz"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(src.join("graphify-out/graph.json").exists());
    assert!(!src.join("graphify-out/GRAPH_REPORT.md").exists());
    assert!(!src.join("graphify-out/graph.html").exists());
}

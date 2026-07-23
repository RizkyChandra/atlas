//! M0 gate: every committed graphify `worked/*/graph.json` golden must load into
//! the atlas `Graph` model and re-serialize to a semantically identical document
//! (order-independent, but every node/edge/attribute preserved).

use atlas_core::Graph;
use serde_json::Value;
use std::path::PathBuf;

fn worked_dir() -> PathBuf {
    // Vendored copies of graphify's `worked/*/graph.json` goldens (hermetic — CI
    // has no sibling graphify tree). Each subdir holds one `graph.json`.
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/graphify"
    ))
}

#[test]
fn worked_goldens_round_trip_losslessly() {
    let dir = worked_dir();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let gj = entry.unwrap().path().join("graph.json");
        if !gj.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&gj).unwrap();
        let original: Value = serde_json::from_str(&raw).unwrap();

        let g = Graph::from_json_str(&raw).unwrap_or_else(|e| panic!("load {}: {e}", gj.display()));
        g.validate()
            .unwrap_or_else(|errs| panic!("{} failed schema: {:?}", gj.display(), errs));

        let redumped: Value = serde_json::from_str(&g.to_json_string().unwrap()).unwrap();
        assert_eq!(original, redumped, "round-trip changed {}", gj.display());
        checked += 1;
    }
    assert!(
        checked > 0,
        "no worked/*/graph.json goldens found in {}",
        dir.display()
    );
    eprintln!("round-tripped {checked} worked goldens");
}

//! M0 gate: every committed graphify `worked/*/graph.json` golden must load into
//! the atlas `Graph` model and re-serialize to a semantically identical document
//! (order-independent, but every node/edge/attribute preserved).

use atlas_core::Graph;
use serde_json::Value;
use std::path::PathBuf;

fn worked_dir() -> Option<PathBuf> {
    // The Python graphify tree is the sibling conformance oracle.
    for cand in ["../../../graphify/worked", "../../graphify/worked"] {
        let p = PathBuf::from(cand);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

#[test]
fn worked_goldens_round_trip_losslessly() {
    let Some(dir) = worked_dir() else {
        eprintln!("skipping: graphify/worked not found next to atlas");
        return;
    };
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let gj = entry.unwrap().path().join("graph.json");
        if !gj.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&gj).unwrap();
        let original: Value = serde_json::from_str(&raw).unwrap();

        let g = Graph::from_json_str(&raw)
            .unwrap_or_else(|e| panic!("load {}: {e}", gj.display()));
        g.validate()
            .unwrap_or_else(|errs| panic!("{} failed schema: {:?}", gj.display(), errs));

        let redumped: Value = serde_json::from_str(&g.to_json_string().unwrap()).unwrap();
        assert_eq!(original, redumped, "round-trip changed {}", gj.display());
        checked += 1;
    }
    assert!(checked > 0, "no worked/*/graph.json goldens found in {}", dir.display());
    eprintln!("round-tripped {checked} worked goldens");
}

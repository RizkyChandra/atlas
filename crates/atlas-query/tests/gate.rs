//! M5 gate: query/path/explain over the committed httpx golden graph.json.

use atlas_core::Graph as AtlasGraph;
use atlas_query::QGraph;
use std::collections::HashSet;

const GOLDEN: &str = "/home/yoshirakou/work/graphify/worked/httpx/graph.json";

fn load() -> QGraph {
    QGraph::from_file(GOLDEN).expect("load golden graph.json")
}

/// explain: the known hub node reports the right degree and lists its neighbours.
#[test]
fn explain_hub_degree_and_neighbors() {
    let g = load();
    // client_client (label "Client") is the top-degree hub: 26 incident edges.
    let e = g.explain("client_client").expect("hub resolves");
    assert_eq!(e.id, "client_client");
    assert_eq!(e.label, "Client");
    assert_eq!(e.degree, 26, "hub degree = in+out incident edges");
    assert_eq!(
        e.connections.len(),
        e.degree,
        "one listed connection per incident edge"
    );
    // Fuzzy label resolution: "AsyncClient" -> the AsyncClient class node.
    let via_label = g.explain("AsyncClient").expect("fuzzy label resolves");
    assert_eq!(via_label.id, "client_asyncclient");
    assert_eq!(via_label.label, "AsyncClient");
    // Every connection carries a relation.
    assert!(e.connections.iter().all(|c| !c.relation.is_empty()));
}

/// path: a connected pair yields a path whose every consecutive pair is a real
/// edge; a disconnected pair yields no path.
#[test]
fn path_connected_every_hop_is_a_real_edge() {
    let g = load();
    let p = g
        .path("client_client", "exceptions")
        .expect("endpoints resolve")
        .expect("connected pair has a path");
    assert_eq!(p.src, "client_client");
    assert_eq!(p.tgt, "exceptions");
    assert!(!p.hops.is_empty());

    // Build the real (undirected) edge set from the raw graph and check each hop.
    let raw = AtlasGraph::from_file(GOLDEN).unwrap();
    let real: HashSet<(String, String)> = raw
        .links
        .iter()
        .filter_map(|e| {
            let s = e.get("source")?.as_str()?.to_string();
            let t = e.get("target")?.as_str()?.to_string();
            Some((s.clone().min(t.clone()), s.max(t)))
        })
        .collect();
    // Chain is contiguous and each step is a genuine edge.
    for h in &p.hops {
        let key = (h.from.clone().min(h.to.clone()), h.from.clone().max(h.to.clone()));
        assert!(real.contains(&key), "hop {}->{} must be a real edge", h.from, h.to);
    }
    // Endpoints line up head-to-tail.
    for w in p.hops.windows(2) {
        assert_eq!(w[0].to, w[1].from, "path hops must be contiguous");
    }
    assert_eq!(p.hops.first().unwrap().from, "client_client");
    assert_eq!(p.hops.last().unwrap().to, "exceptions");
}

#[test]
fn path_disconnected_returns_none() {
    // Synthetic: two nodes, no edge between them (httpx golden is fully connected).
    let g = QGraph::from_atlas(
        AtlasGraph::from_json_str(
            r#"{"nodes":[{"id":"a","label":"A"},{"id":"b","label":"B"}],"links":[]}"#,
        )
        .unwrap(),
    );
    let r = g.path("a", "b").expect("both endpoints resolve");
    assert!(r.is_none(), "disconnected pair -> no path");
}

/// query: an obvious term returns a non-empty subgraph containing the matching
/// node, and respects the budget cap.
#[test]
fn query_returns_scoped_subgraph_within_budget() {
    let g = load();
    let r = g.query("client", 1500, false);
    assert!(!r.nodes.is_empty(), "obvious term yields a non-empty subgraph");
    assert!(r.seeds.contains(&"client".to_string()), "seeds on the matching node");
    assert!(r.nodes.contains(&"client".to_string()), "subgraph contains the match");

    // Budget cap is respected.
    let capped = g.query("client", 5, false);
    assert!(capped.nodes.len() <= 5, "subgraph honors the budget cap");
    assert!(capped.nodes.contains(&"client".to_string()), "seed survives the cap");

    // --dfs also works and stays within budget.
    let d = g.query("client", 20, true);
    assert!(!d.nodes.is_empty() && d.nodes.len() <= 20);
    assert_eq!(d.mode, "dfs");
}

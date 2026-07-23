//! M3 gate — runs against the committed httpx golden.
//!
//! Proves god_nodes, cluster and render_report reproduce the structure graphify
//! recorded in the golden `graph.json` / `GRAPH_REPORT.md`.

use atlas_core::Graph;
use atlas_graph::{cluster, god_nodes, render_report, Model, DEFAULT_RESOLUTION};
use std::collections::HashMap;

const GRAPH_JSON: &str = "/home/yoshirakou/work/graphify/worked/httpx/graph.json";
const GOLDEN_REPORT: &str = "/home/yoshirakou/work/graphify/worked/httpx/GRAPH_REPORT.md";

/// Node degree computed directly from the golden's edges (each edge contributes
/// +1 to both endpoints) — the independent oracle the gate checks against.
fn direct_degrees() -> HashMap<String, usize> {
    let g = Graph::from_file(GRAPH_JSON).unwrap();
    let mut deg: HashMap<String, usize> = HashMap::new();
    for e in &g.links {
        for key in ["source", "target"] {
            if let Some(id) = e.get(key).and_then(|v| v.as_str()) {
                *deg.entry(id.to_string()).or_default() += 1;
            }
        }
    }
    deg
}

/// Parse `N. `label` - D edges` god-node lines out of a GRAPH_REPORT.md.
fn parse_god_nodes(report: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for line in report.lines() {
        // e.g. "1. `Client` - 26 edges"
        let Some(rest) = line.split_once(". `").map(|(_, r)| r) else {
            continue;
        };
        if !line
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            continue;
        }
        let Some((label, tail)) = rest.split_once("` - ") else {
            continue;
        };
        let Some(deg) = tail
            .strip_suffix(" edges")
            .and_then(|d| d.trim().parse::<usize>().ok())
        else {
            continue;
        };
        out.push((label.to_string(), deg));
    }
    out
}

fn count_communities(report: &str) -> usize {
    report
        .lines()
        .filter(|l| l.starts_with("### Community"))
        .count()
}

#[test]
fn god_nodes_match_highest_degree_from_edges() {
    let model = Model::from_file(GRAPH_JSON).unwrap();
    let deg = direct_degrees();
    let god = god_nodes(&model, 10);

    assert_eq!(god.len(), 10, "expected 10 god nodes");

    // 1. Each god node's reported degree equals the degree computed directly
    //    from the golden's edges.
    for g in &god {
        assert_eq!(
            g.degree, deg[&g.id],
            "god node {} degree disagrees with direct edge count",
            g.label
        );
    }

    // 2. God nodes are ordered by non-increasing degree.
    for w in god.windows(2) {
        assert!(w[0].degree >= w[1].degree, "god nodes must be sorted desc");
    }

    // 3. They are the same nodes graphify recorded in the golden report — i.e.
    //    the highest-degree real abstractions.
    let golden = parse_god_nodes(&std::fs::read_to_string(GOLDEN_REPORT).unwrap());
    let got: Vec<(String, usize)> = god.iter().map(|g| (g.label.clone(), g.degree)).collect();
    assert_eq!(got, golden, "god nodes must match the golden report");
}

#[test]
fn cluster_assigns_every_node_with_good_modularity() {
    let model = Model::from_file(GRAPH_JSON).unwrap();
    let c = cluster(&model, DEFAULT_RESOLUTION);

    // Every node gets a community.
    let node_ids: std::collections::HashSet<&str> = model
        .nodes
        .iter()
        .filter_map(|n| n.get("id").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(
        c.node_community.len(),
        node_ids.len(),
        "every node must be assigned a community"
    );
    for id in &node_ids {
        assert!(c.node_community.contains_key(*id), "node {id} unassigned");
    }
    for members in &c.communities {
        assert!(!members.is_empty(), "no empty communities");
    }

    // Modularity comfortably above the 0.3 gate.
    assert!(
        c.modularity > 0.3,
        "modularity {} must exceed 0.3",
        c.modularity
    );
}

#[test]
fn render_report_matches_golden_structure() {
    let model = Model::from_file(GRAPH_JSON).unwrap();
    let c = cluster(&model, DEFAULT_RESOLUTION);
    let report = render_report(&model, &c, "worked/httpx/raw", "2026-07-24");

    let golden = std::fs::read_to_string(GOLDEN_REPORT).unwrap();

    // God-node section present with the same god nodes as the golden.
    assert!(
        report.contains("## God Nodes"),
        "report must have a God Nodes section"
    );
    assert_eq!(
        parse_god_nodes(&report),
        parse_god_nodes(&golden),
        "report god nodes must match the golden"
    );

    // Community sections present; same count as the golden.
    let golden_comms = count_communities(&golden);
    let report_comms = count_communities(&report);
    assert_eq!(golden_comms, 6, "golden has 6 communities");
    assert_eq!(
        report_comms, golden_comms,
        "report must list the same number of communities as the golden"
    );
    assert_eq!(
        report_comms,
        c.len(),
        "every partition community must get a section"
    );
}

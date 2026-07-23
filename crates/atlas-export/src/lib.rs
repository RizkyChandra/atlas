//! atlas-export — turn an [`atlas_core::Graph`] into shareable formats.
//!
//! Ported from graphify `graphify/export.py` and `graphify/exporters/html.py`.
//! Every exporter reads directly from the committed `graph.json` node/edge
//! attributes (community id lives on each node), so no separate `communities`
//! map is threaded through the way the Python signatures did.

use atlas_core::{Attrs, Graph};
use serde_json::Value;
use std::collections::HashMap;

mod cypher;
mod graphml;
mod html;
mod svg;

pub use cypher::to_cypher;
pub use graphml::to_graphml;
pub use html::to_html;
pub use svg::to_svg;

/// Categorical palette for community coloring — verbatim from
/// `graphify/exporters/base.py` `COMMUNITY_COLORS`.
pub const COMMUNITY_COLORS: [&str; 10] = [
    "#4E79A7", "#F28E2B", "#E15759", "#76B7B2", "#59A14F", "#EDC948", "#B07AA1", "#FF9DA7",
    "#9C755F", "#BAB0AC",
];

pub(crate) fn attr_str<'a>(a: &'a Attrs, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

/// Node id, or "" for a malformed node missing a string `id`.
pub(crate) fn node_id(n: &Attrs) -> &str {
    attr_str(n, "id").unwrap_or("")
}

/// Community id baked into the node by the extractor; 0 when absent.
pub(crate) fn community_of(n: &Attrs) -> i64 {
    n.get("community").and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) fn color_for(cid: i64) -> &'static str {
    COMMUNITY_COLORS[cid.rem_euclid(COMMUNITY_COLORS.len() as i64) as usize]
}

/// Undirected degree per node id (matches NetworkX `G.degree`).
pub(crate) fn degrees(g: &Graph) -> HashMap<String, usize> {
    let mut d: HashMap<String, usize> = HashMap::new();
    for n in &g.nodes {
        d.entry(node_id(n).to_string()).or_insert(0);
    }
    for e in &g.links {
        for k in ["source", "target"] {
            if let Some(v) = attr_str(e, k) {
                *d.entry(v.to_string()).or_insert(0) += 1;
            }
        }
    }
    d
}

/// Escape text for an XML/HTML body or a double-quoted attribute.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
pub(crate) const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/graphify/httpx/graph.json"
);

#[cfg(test)]
pub(crate) fn httpx() -> Graph {
    Graph::from_file(GOLDEN).expect("load httpx golden")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_loads_with_expected_size() {
        let g = httpx();
        assert_eq!(g.nodes.len(), 144);
        assert_eq!(g.links.len(), 330);
    }
}

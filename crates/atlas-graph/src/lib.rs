//! atlas-graph — graph intelligence over an `atlas_core::Graph`.
//!
//! Port of graphify's `cluster.py` (community detection), `analyze.py` (god
//! nodes / surprising connections) and `report.py` (GRAPH_REPORT.md). Consumes a
//! committed `graph.json` — independent of the extractor.
//!
//! No mature Rust Leiden exists, so community detection is **Louvain** modularity
//! maximization (deterministic, no RNG), implemented over a `petgraph` UnGraph.

use atlas_core::{Attrs, Graph};
use petgraph::graph::{NodeIndex, UnGraph};
use serde_json::Value;
use std::collections::HashMap;

mod analyze;
mod cluster;
mod report;

pub use analyze::{god_nodes, surprising_connections, GodNode, Surprise};
pub use cluster::{cluster, Clustering, DEFAULT_RESOLUTION};
pub use report::render_report;

/// A loaded graph with a `petgraph` view for structural queries.
///
/// `NodeIndex(i)` corresponds to `nodes[i]` — nodes are added to the petgraph in
/// the same order they appear in the source `graph.json`, so original ordering
/// (which drives stable degree tie-breaks) is preserved.
pub struct Model {
    /// Undirected structure. Node weight is the node's position; edge weight is
    /// the link's `weight` attribute (default 1.0).
    pub g: UnGraph<usize, f64>,
    /// Node attribute bags, in original order. Index with `NodeIndex::index()`.
    pub nodes: Vec<Attrs>,
    /// Edge attribute bags, verbatim from the source (petgraph keeps only the
    /// numeric weight, so relation/confidence are recovered from here).
    pub links: Vec<Attrs>,
    id_to_ix: HashMap<String, NodeIndex>,
}

impl Model {
    /// Build from an already-parsed graph. Dangling edges (endpoint id not a
    /// node) are skipped rather than rejected — matching `atlas_core`'s stance
    /// that dangling edges are a separately-tracked bug, not a load failure.
    pub fn new(graph: &Graph) -> Model {
        let mut g = UnGraph::<usize, f64>::new_undirected();
        let mut id_to_ix = HashMap::with_capacity(graph.nodes.len());
        let mut nodes = Vec::with_capacity(graph.nodes.len());
        for (i, n) in graph.nodes.iter().enumerate() {
            let ix = g.add_node(i);
            nodes.push(n.clone());
            if let Some(id) = n.get("id").and_then(Value::as_str) {
                id_to_ix.insert(id.to_string(), ix);
            }
        }
        for e in &graph.links {
            let (Some(s), Some(t)) = (
                e.get("source").and_then(Value::as_str),
                e.get("target").and_then(Value::as_str),
            ) else {
                continue;
            };
            if let (Some(&si), Some(&ti)) = (id_to_ix.get(s), id_to_ix.get(t)) {
                let w = e.get("weight").and_then(Value::as_f64).unwrap_or(1.0);
                g.add_edge(si, ti, w);
            }
        }
        Model {
            g,
            nodes,
            links: graph.links.clone(),
            id_to_ix,
        }
    }

    /// The original edge attribute bags.
    pub fn raw_links(&self) -> &[Attrs] {
        &self.links
    }

    pub fn from_file(path: impl AsRef<std::path::Path>) -> anyhow::Result<Model> {
        Ok(Model::new(&Graph::from_file(path)?))
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Structural degree of node at position `pos` (incident edge count). No
    /// self-loops/parallel edges in graphify output, so this equals the NetworkX
    /// degree the golden was scored with.
    pub fn degree(&self, pos: usize) -> usize {
        self.g.edges(NodeIndex::new(pos)).count()
    }

    pub fn attrs(&self, pos: usize) -> &Attrs {
        &self.nodes[pos]
    }

    pub fn id(&self, pos: usize) -> &str {
        str_attr(&self.nodes[pos], "id").unwrap_or("")
    }

    pub fn label(&self, pos: usize) -> &str {
        str_attr(&self.nodes[pos], "label").unwrap_or_else(|| self.id(pos))
    }

    pub fn source_file(&self, pos: usize) -> &str {
        str_attr(&self.nodes[pos], "source_file").unwrap_or("")
    }

    pub fn pos_of(&self, id: &str) -> Option<usize> {
        self.id_to_ix.get(id).map(|ix| ix.index())
    }
}

pub(crate) fn str_attr<'a>(a: &'a Attrs, key: &str) -> Option<&'a str> {
    a.get(key).and_then(Value::as_str)
}

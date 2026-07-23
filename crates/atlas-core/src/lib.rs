//! atlas-core — the stable data contract shared by every atlas crate.
//!
//! Mirrors graphify's NetworkX node-link `graph.json` exactly. Nodes and edges
//! carry arbitrary attributes, so we model each as a `serde_json::Map` (with the
//! `preserve_order` feature, an order-preserving map) rather than a fixed struct.
//! That keeps load→save byte-for-byte faithful to the Python output while still
//! giving typed accessors for the known fields (`id`, `label`, `source`, ...).
//!
//! Schema reference: graphify `ARCHITECTURE.md` and `graphify/validate.py`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

pub mod ids;

/// Arbitrary attribute bag for a node, edge, or the graph itself.
pub type Attrs = Map<String, Value>;

/// The three edge confidence labels (graphify `ARCHITECTURE.md`).
pub const CONFIDENCE_LABELS: [&str; 3] = ["EXTRACTED", "INFERRED", "AMBIGUOUS"];

/// A knowledge graph in NetworkX node-link form.
///
/// Unknown top-level keys are captured in `extra` so nothing is lost on
/// round-trip. Edges live under `links` (NetworkX's name); we also accept
/// `edges` on input for tools that emit that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    pub directed: bool,
    #[serde(default)]
    pub multigraph: bool,
    #[serde(default)]
    pub graph: Attrs,
    #[serde(default)]
    pub nodes: Vec<Attrs>,
    #[serde(default, alias = "edges")]
    pub links: Vec<Attrs>,
    /// Any other top-level keys, preserved verbatim.
    #[serde(flatten)]
    pub extra: Attrs,
}

impl Default for Graph {
    fn default() -> Self {
        Graph {
            directed: false,
            multigraph: false,
            graph: Attrs::new(),
            nodes: Vec::new(),
            links: Vec::new(),
            extra: Attrs::new(),
        }
    }
}

/// A schema violation found by [`Graph::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn str_attr<'a>(a: &'a Attrs, key: &str) -> Option<&'a str> {
    a.get(key).and_then(Value::as_str)
}

impl Graph {
    pub fn from_json_str(s: &str) -> anyhow::Result<Graph> {
        Ok(serde_json::from_str(s)?)
    }

    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Graph> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
    }

    pub fn to_json_string(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn to_json_string_pretty(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Iterate node ids (skipping any node missing a string `id`).
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().filter_map(|n| str_attr(n, "id"))
    }

    /// Validate against the graphify extraction schema: every node needs a
    /// unique string `id`; every edge needs string `source`/`target`/`relation`,
    /// and a `confidence` (when present) drawn from [`CONFIDENCE_LABELS`].
    ///
    /// Referential integrity (edges pointing at real nodes) is intentionally NOT
    /// enforced here — dangling edges are a real, separately-tracked bug class
    /// (see [`Graph::dangling_edges`]), not a reason to reject a file.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for (i, n) in self.nodes.iter().enumerate() {
            match str_attr(n, "id") {
                Some(id) => {
                    if !ids.insert(id) {
                        errs.push(ValidationError(format!("duplicate node id: {id:?}")));
                    }
                }
                None => errs.push(ValidationError(format!("node[{i}] missing string `id`"))),
            }
        }
        for (i, e) in self.links.iter().enumerate() {
            for key in ["source", "target", "relation"] {
                if str_attr(e, key).is_none() {
                    errs.push(ValidationError(format!("edge[{i}] missing string `{key}`")));
                }
            }
            if let Some(c) = str_attr(e, "confidence") {
                if !CONFIDENCE_LABELS.contains(&c) {
                    errs.push(ValidationError(format!(
                        "edge[{i}] invalid confidence {c:?} (expected one of {CONFIDENCE_LABELS:?})"
                    )));
                }
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    /// Edges whose `source` or `target` names a node id that does not exist.
    /// Backs the fix for graphify issue #2130 (edges referencing missing nodes).
    pub fn dangling_edges(&self) -> Vec<(usize, String)> {
        let ids: std::collections::HashSet<&str> = self.node_ids().collect();
        let mut out = Vec::new();
        for (i, e) in self.links.iter().enumerate() {
            for key in ["source", "target"] {
                if let Some(v) = str_attr(e, key) {
                    if !ids.contains(v) {
                        out.push((i, format!("{key} -> unknown node {v:?}")));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_all_attrs() {
        let src = r#"{"directed":false,"multigraph":false,"graph":{},
            "nodes":[{"label":"a","id":"a","community":1,"weird":[1,2]}],
            "links":[{"relation":"calls","confidence":"EXTRACTED","source":"a","target":"a","weight":1.0}]}"#;
        let g = Graph::from_json_str(src).unwrap();
        let out = g.to_json_string().unwrap();
        let a: Value = serde_json::from_str(src).unwrap();
        let b: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(a, b, "round-trip must be lossless");
    }

    #[test]
    fn validate_flags_bad_confidence_and_missing_id() {
        let g = Graph::from_json_str(
            r#"{"nodes":[{"label":"x"}],"links":[{"source":"a","target":"b","relation":"c","confidence":"NOPE"}]}"#,
        )
        .unwrap();
        let errs = g.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("missing string `id`")));
        assert!(errs.iter().any(|e| e.0.contains("invalid confidence")));
    }

    #[test]
    fn dangling_edges_detected() {
        let g = Graph::from_json_str(
            r#"{"nodes":[{"id":"a"}],"links":[{"source":"a","target":"ghost","relation":"calls"}]}"#,
        )
        .unwrap();
        assert_eq!(g.dangling_edges().len(), 1);
    }
}

//! atlas-ingest — document/media readers + chunking (milestone M8).
//!
//! Turns non-code files into either structural graph fragments ([`Extraction`],
//! for markdown and csv/tsv) or plain extracted text (pdf/office/ipynb, for the
//! later semantic pass). Ported from graphify's `extractors/markdown.py`,
//! `detect.py` (pdf/office readers), `file_slice.py` (chunking), and the
//! backlog CSV/TSV bounded-ingestion spec (issue #2119 / PR #2125).
//!
//! Node IDs come from [`atlas_core::ids`] — the one shared recipe — so doc nodes
//! and their `references`/`contains` edges merge with the rest of the graph.

use atlas_core::Attrs;
use serde_json::{json, Value};

pub mod chunk;
pub mod csv_reader;
pub mod ipynb;
pub mod markdown;

#[cfg(feature = "pdf")]
pub mod pdf;
#[cfg(feature = "office")]
pub mod office;

pub use chunk::{chunk_text, Chunk};

/// Raw extraction output: node and edge attribute maps, in emission order.
/// Mirrors `atlas_extract::ExtractResult` — the raw `{nodes, edges}` graphify
/// emits before cross-file resolution, so reference targets may dangle.
#[derive(Debug, Default, Clone)]
pub struct Extraction {
    pub nodes: Vec<Attrs>,
    pub edges: Vec<Attrs>,
}

impl Extraction {
    /// Edges whose `relation` equals `relation`.
    pub fn links_with_relation<'a>(&'a self, relation: &'a str) -> impl Iterator<Item = &'a Attrs> {
        self.edges
            .iter()
            .filter(move |e| e.get("relation").and_then(Value::as_str) == Some(relation))
    }
}

fn as_map(v: Value) -> Attrs {
    match v {
        Value::Object(m) => m,
        _ => unreachable!("node/edge builders always produce a JSON object"),
    }
}

/// Build a doc/structural node. `location` is a graphify `source_location`
/// (e.g. `"L1"`) or `None`.
pub(crate) fn node(id: &str, label: &str, file_type: &str, source_file: &str, location: Option<&str>) -> Attrs {
    as_map(json!({
        "id": id,
        "label": label,
        "file_type": file_type,
        "source_file": source_file,
        "source_location": location,
    }))
}

/// Build an EXTRACTED edge (weight 1.0), matching graphify's deterministic
/// doc-edge shape.
pub(crate) fn edge(source: &str, target: &str, relation: &str, source_file: &str, location: Option<&str>) -> Attrs {
    as_map(json!({
        "source": source,
        "target": target,
        "relation": relation,
        "confidence": "EXTRACTED",
        "source_file": source_file,
        "source_location": location,
        "weight": 1.0,
    }))
}

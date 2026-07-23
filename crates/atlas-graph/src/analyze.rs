//! Graph analysis — god nodes and surprising connections. Port of `analyze.py`.

use crate::{str_attr, Clustering, Model};

/// Builtin/mock/stdlib names excluded from god-node ranking (`_BUILTIN_NOISE_LABELS`).
const BUILTIN_NOISE: &[&str] = &[
    "str",
    "int",
    "float",
    "bool",
    "bytes",
    "bytearray",
    "complex",
    "object",
    "True",
    "False",
    "MagicMock",
    "Mock",
    "AsyncMock",
    "NonCallableMock",
    "NonCallableMagicMock",
    "PropertyMock",
    "patch",
    "sentinel",
    "Path",
    "Any",
    "Optional",
    "List",
    "Dict",
    "Set",
    "Tuple",
    "Union",
    "Callable",
    "Type",
    "ClassVar",
    "Final",
    "Literal",
    "Protocol",
    "Counter",
    "defaultdict",
    "OrderedDict",
    "datetime",
    "Enum",
    "os",
    "sys",
    "re",
    "json",
    "io",
    "abc",
    "typing",
];

/// Noise keys excluded from god nodes when they come from a `.json` source
/// (`_JSON_NOISE_LABELS`).
const JSON_NOISE: &[&str] = &[
    "start",
    "end",
    "name",
    "id",
    "type",
    "properties",
    "value",
    "key",
    "data",
    "items",
    "title",
    "description",
    "version",
    "dependencies",
    "devdependencies",
    "peerdependencies",
    "optionaldependencies",
    "bundleddependencies",
    "bundledependencies",
];

/// A most-connected "core abstraction" node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GodNode {
    pub id: String,
    pub label: String,
    pub degree: usize,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// File-level hub or AST method/function stub (`_is_file_node`). Excluded from
/// god nodes — these accumulate structural edges mechanically.
pub(crate) fn is_file_node(model: &Model, pos: usize) -> bool {
    let attrs = model.attrs(pos);
    let label = str_attr(attrs, "label").unwrap_or("");
    if label.is_empty() {
        return false;
    }
    let source = str_attr(attrs, "source_file").unwrap_or("");
    if !source.is_empty() && basename(source) == label {
        return true; // file-level hub: label is the source filename
    }
    if label.starts_with('.') && label.ends_with("()") {
        return true; // method stub: `.method()`
    }
    if label.ends_with("()") && model.degree(pos) <= 1 {
        return true; // module-level function stub, structurally isolated
    }
    false
}

/// Manually-injected semantic concept node, not a real source entity
/// (`_is_concept_node`): empty source_file, or a source with no file extension.
pub(crate) fn is_concept_node(model: &Model, pos: usize) -> bool {
    let source = str_attr(model.attrs(pos), "source_file").unwrap_or("");
    if source.is_empty() {
        return true;
    }
    !basename(source).contains('.')
}

fn is_json_key_node(model: &Model, pos: usize) -> bool {
    let attrs = model.attrs(pos);
    let src = str_attr(attrs, "source_file").unwrap_or("").to_lowercase();
    if !src.ends_with(".json") {
        return false;
    }
    let label = str_attr(attrs, "label").unwrap_or("").trim().to_lowercase();
    JSON_NOISE.contains(&label.as_str())
}

/// Top-`top_n` most-connected real entities — the core abstractions.
///
/// File hubs, concept nodes, JSON keys and builtin/mock names are filtered out.
/// Ties break by original node order (a stable sort over positions), matching
/// NetworkX's stable `sorted(..., reverse=True)`.
pub fn god_nodes(model: &Model, top_n: usize) -> Vec<GodNode> {
    let mut ranked: Vec<(usize, usize)> = (0..model.node_count())
        .map(|pos| (pos, model.degree(pos)))
        .collect();
    // Stable sort by degree desc; equal degrees keep ascending position order.
    ranked.sort_by_key(|&(_, deg)| std::cmp::Reverse(deg));

    let mut out = Vec::with_capacity(top_n);
    for (pos, deg) in ranked {
        if is_file_node(model, pos) || is_concept_node(model, pos) || is_json_key_node(model, pos) {
            continue;
        }
        if BUILTIN_NOISE.contains(&model.label(pos)) {
            continue;
        }
        out.push(GodNode {
            id: model.id(pos).to_string(),
            label: model.label(pos).to_string(),
            degree: deg,
        });
        if out.len() >= top_n {
            break;
        }
    }
    out
}

/// A cross-file connection flagged as non-obvious.
#[derive(Debug, Clone)]
pub struct Surprise {
    pub source: String,
    pub target: String,
    pub source_files: [String; 2],
    pub confidence: String,
    pub relation: String,
}

const STRUCTURAL_RELATIONS: &[&str] = &["imports", "imports_from", "contains", "method"];

/// Cross-file edges between real entities, ranked by a surprise score
/// (`_cross_file_surprises`, simplified): confidence weight + cross-community +
/// peripheral→hub. Enough to populate the report's "Surprising Connections".
// ponytail: dropped cross-repo / cross-filetype / semantic-similarity bonuses and the
// single-source betweenness fallback — add if a corpus needs those distinctions.
pub fn surprising_connections(
    model: &Model,
    clustering: &Clustering,
    top_n: usize,
) -> Vec<Surprise> {
    let mut scored: Vec<(i64, Surprise)> = Vec::new();
    for e in model.g.raw_edges() {
        let (u, v) = (e.source().index(), e.target().index());
        // Recover this edge's attributes for relation/confidence.
        let (relation, confidence) = edge_attrs(model, u, v);
        if STRUCTURAL_RELATIONS.contains(&relation.as_str()) {
            continue;
        }
        if is_concept_node(model, u) || is_concept_node(model, v) {
            continue;
        }
        if is_file_node(model, u) || is_file_node(model, v) {
            continue;
        }
        let (su, sv) = (model.source_file(u), model.source_file(v));
        if su.is_empty() || sv.is_empty() || su == sv {
            continue;
        }

        let mut score = match confidence.as_str() {
            "AMBIGUOUS" => 3,
            "INFERRED" => 2,
            _ => 1,
        };
        let cu = clustering.node_community.get(model.id(u));
        let cv = clustering.node_community.get(model.id(v));
        if let (Some(a), Some(b)) = (cu, cv) {
            if a != b {
                score += 1; // bridges separate communities
            }
        }
        let (du, dv) = (model.degree(u), model.degree(v));
        if du.min(dv) <= 2 && du.max(dv) >= 5 {
            score += 1; // peripheral node reaches a hub
        }

        scored.push((
            score,
            Surprise {
                source: model.label(u).to_string(),
                target: model.label(v).to_string(),
                source_files: [su.to_string(), sv.to_string()],
                confidence,
                relation,
            },
        ));
    }
    // Sort by score desc, stable (preserves edge order for ties).
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(top_n).map(|(_, s)| s).collect()
}

/// Look up `(relation, confidence)` for the undirected edge between positions
/// `u` and `v` from the original attribute bags. petgraph edge weights are numeric
/// only, so we recover string attrs by scanning — cheap at report scale.
fn edge_attrs(model: &Model, u: usize, v: usize) -> (String, String) {
    let (uid, vid) = (model.id(u), model.id(v));
    for e in model.raw_links() {
        let s = str_attr(e, "source").unwrap_or("");
        let t = str_attr(e, "target").unwrap_or("");
        if (s == uid && t == vid) || (s == vid && t == uid) {
            return (
                str_attr(e, "relation").unwrap_or("").to_string(),
                str_attr(e, "confidence").unwrap_or("EXTRACTED").to_string(),
            );
        }
    }
    (String::new(), "EXTRACTED".to_string())
}

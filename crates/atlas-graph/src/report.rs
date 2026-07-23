//! GRAPH_REPORT.md rendering. Port of the core sections of `report.py`.
//!
//! Emits: title, Corpus Check, Summary, God Nodes, Surprising Connections,
//! Communities. Sections that need data absent from `graph.json` (token cost,
//! corpus word count) or heavy centrality (Suggested Questions, Import Cycles)
//! are omitted — see the crate report gate for what's covered.

use crate::analyze::is_file_node;
use crate::{god_nodes, str_attr, surprising_connections, Clustering, Model};
use std::collections::BTreeSet;

/// Render a GRAPH_REPORT.md-style document for `model` using `clustering`.
///
/// `root` is the corpus label shown in the title/corpus lines (graphify passes
/// the scanned root path, e.g. `worked/httpx/raw`). `today` is the ISO date
/// stamped in the title.
pub fn render_report(model: &Model, clustering: &Clustering, root: &str, today: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    // --- Title + Corpus Check ---
    let source_files: BTreeSet<&str> = (0..model.node_count())
        .map(|p| model.source_file(p))
        .filter(|s| !s.is_empty())
        .collect();
    lines.push(format!("# Graph Report - {root}  ({today})"));
    lines.push(String::new());
    lines.push("## Corpus Check".into());
    lines.push(format!("- {} files", source_files.len()));
    lines.push("- Verdict: corpus is large enough that graph structure adds value.".into());

    // --- Summary ---
    let n_edges = model.raw_links().len();
    let (mut ext, mut inf, mut amb) = (0usize, 0usize, 0usize);
    for e in model.raw_links() {
        match str_attr(e, "confidence").unwrap_or("EXTRACTED") {
            "INFERRED" => inf += 1,
            "AMBIGUOUS" => amb += 1,
            _ => ext += 1,
        }
    }
    let total = n_edges.max(1);
    let pct = |c: usize| (c as f64 / total as f64 * 100.0).round() as i64;
    lines.push(String::new());
    lines.push("## Summary".into());
    lines.push(format!(
        "- {} nodes · {} edges · {} communities detected",
        model.node_count(),
        n_edges,
        clustering.len()
    ));
    lines.push(format!(
        "- Extraction: {}% EXTRACTED · {}% INFERRED · {}% AMBIGUOUS",
        pct(ext),
        pct(inf),
        pct(amb)
    ));

    // --- God Nodes ---
    lines.push(String::new());
    lines.push("## God Nodes (most connected - your core abstractions)".into());
    for (i, node) in god_nodes(model, 10).iter().enumerate() {
        lines.push(format!(
            "{}. `{}` - {} edges",
            i + 1,
            node.label,
            node.degree
        ));
    }

    // --- Surprising Connections ---
    lines.push(String::new());
    lines.push("## Surprising Connections (you probably didn't know these)".into());
    let surprises = surprising_connections(model, clustering, 5);
    if surprises.is_empty() {
        lines.push("- None detected - all connections are within the same source files.".into());
    } else {
        for s in &surprises {
            lines.push(format!(
                "- `{}` --{}--> `{}`  [{}]",
                s.source, s.relation, s.target, s.confidence
            ));
            lines.push(format!("  {} → {}", s.source_files[0], s.source_files[1]));
        }
    }

    // --- Communities ---
    // Every non-empty community gets a section (labelled "Community {cid}", the
    // no-LLM default). File/stub nodes are filtered from the displayed member
    // list, mirroring report.py; the section itself is always emitted so the
    // report's community count equals the partition's.
    lines.push(String::new());
    lines.push("## Communities".into());
    for (cid, members) in clustering.communities.iter().enumerate() {
        let real: Vec<&str> = members
            .iter()
            .filter_map(|id| model.pos_of(id))
            .filter(|&p| !is_file_node(model, p))
            .map(|p| model.label(p))
            .collect();
        let shown: Vec<&str> = if real.is_empty() {
            members
                .iter()
                .filter_map(|id| model.pos_of(id))
                .map(|p| model.label(p))
                .collect()
        } else {
            real
        };
        let display: Vec<&str> = shown.iter().take(8).copied().collect();
        let suffix = if shown.len() > 8 {
            format!(" (+{} more)", shown.len() - 8)
        } else {
            String::new()
        };
        lines.push(String::new());
        lines.push(format!("### Community {cid} - \"Community {cid}\""));
        lines.push(format!("Cohesion: {:.2}", clustering.cohesion(model, cid)));
        lines.push(format!(
            "Nodes ({}): {}{}",
            shown.len(),
            display.join(", "),
            suffix
        ));
    }

    lines.join("\n")
}

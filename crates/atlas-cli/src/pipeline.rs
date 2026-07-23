//! End-to-end command wiring (M10). Each function calls into the sibling crates
//! and owns only the glue: walking a directory, merging per-file extractions,
//! stamping communities, and rendering the output files graphify writes.

use anyhow::{bail, Context, Result};
use atlas_core::{Attrs, Graph};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Extensions atlas-extract dispatches on (see `atlas_extract::extract_file`).
const SUPPORTED: &[&str] = &[
    "py", "pyi", "js", "jsx", "mjs", "cjs", "ts", "mts", "cts", "java", "c", "h", "cpp", "cc",
    "cxx", "hpp", "hh", "hxx", "go", "rs",
];
/// Directories never worth walking.
const IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules"];

// ── extract ─────────────────────────────────────────────────────────────────

/// Walk `dir`, extract every supported file, merge+dedupe into a `graph.json`,
/// and (unless `no_viz`) cluster + write GRAPH_REPORT.md and graph.html.
///
/// `code_only` is accepted for graphify parity; atlas only does code (AST)
/// extraction today, so the flag currently changes nothing.
///
/// After the per-file merge, `atlas_extract::resolve_corpus` runs graphify's
/// build-time cross-file symbol resolution (import-stub collapse, Python
/// import/uses edges, import-guided cross-file calls).
pub fn extract(dir: &str, _code_only: bool, no_viz: bool) -> Result<()> {
    let root = Path::new(dir);
    if !root.is_dir() {
        bail!("{dir} is not a directory");
    }
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files.sort(); // deterministic merge order

    let mut all_nodes: Vec<Attrs> = Vec::new();
    let mut all_edges: Vec<Attrs> = Vec::new();
    for f in &files {
        let r = atlas_extract::extract_file(f)
            .with_context(|| format!("extracting {}", f.display()))?;
        all_nodes.extend(r.nodes);
        all_edges.extend(r.edges);
    }
    let nodes = atlas_extract::dedupe_nodes(all_nodes);
    let links = atlas_extract::dedupe_edges(all_edges);
    // Cross-file resolution: rewire import stubs and resolve cross-file calls
    // across the merged corpus (graphify's build-time symbol resolution).
    let (nodes, links) = atlas_extract::resolve_corpus(nodes, links);

    let mut g = Graph {
        directed: false,
        multigraph: false,
        nodes,
        links,
        ..Default::default()
    };

    let out_dir = root.join("graphify-out");
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let graph_json = out_dir.join("graph.json");

    if !no_viz {
        stamp_communities(&mut g);
    }
    write_graph(&g, &graph_json)?;
    println!(
        "Extracted {} files → {} ({} nodes, {} edges).",
        files.len(),
        graph_json.display(),
        g.nodes.len(),
        g.links.len()
    );

    if !no_viz {
        write_viz(&g, &out_dir, dir)?;
    }
    Ok(())
}

/// Recursively gather supported source files, skipping ignored dirs.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !IGNORE_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                collect_files(&path, out);
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if SUPPORTED.contains(&ext.to_ascii_lowercase().as_str()) {
                out.push(path);
            }
        }
    }
}

// ── cluster-only ────────────────────────────────────────────────────────────

/// Re-cluster an existing `<dir>/graphify-out/graph.json`, rewrite it with fresh
/// community ids, and regenerate GRAPH_REPORT.md.
pub fn cluster_only(dir: &str) -> Result<()> {
    let out_dir = Path::new(dir).join("graphify-out");
    let graph_json = out_dir.join("graph.json");
    let mut g = Graph::from_file(&graph_json)
        .with_context(|| format!("loading {}", graph_json.display()))?;
    let n = stamp_communities(&mut g);
    write_graph(&g, &graph_json)?;
    write_report(&g, &out_dir, dir)?;
    println!("Re-clustered {} → {n} communities.", graph_json.display());
    Ok(())
}

// ── export ──────────────────────────────────────────────────────────────────

pub fn export(format: &str, graph: &str, output: Option<&str>) -> Result<()> {
    let g = Graph::from_file(graph).with_context(|| format!("loading {graph}"))?;
    let content = match format.to_ascii_lowercase().as_str() {
        "html" => atlas_export::to_html(&g),
        "svg" => atlas_export::to_svg(&g),
        "graphml" => atlas_export::to_graphml(&g),
        "cypher" => atlas_export::to_cypher(&g),
        other => bail!("unknown export format {other:?} (want: html|svg|graphml|cypher)"),
    };
    match output {
        Some(p) => {
            std::fs::write(p, &content).with_context(|| format!("writing {p}"))?;
            println!("Wrote {} ({} bytes).", p, content.len());
        }
        None => print!("{content}"),
    }
    Ok(())
}

// ── query / path / explain ──────────────────────────────────────────────────

pub fn query(graph: &str, question: &str, dfs: bool, budget: Option<usize>) -> Result<()> {
    let qg = atlas_query::QGraph::from_file(graph).with_context(|| format!("loading {graph}"))?;
    let r = qg.query(question, budget.unwrap_or(atlas_query::DEFAULT_BUDGET), dfs);
    if r.seeds.is_empty() {
        println!("No matching nodes found.");
        return Ok(());
    }
    let seeds: Vec<String> = r.seeds.iter().map(|id| qg.label_for_id(id)).collect();
    println!(
        "Traversal: {} depth=2 | Start: {:?} | {} nodes found\n",
        r.mode.to_uppercase(),
        seeds,
        r.nodes.len()
    );
    for id in &r.nodes {
        println!("  {} ({id})", qg.label_for_id(id));
    }
    Ok(())
}

pub fn path(graph: &str, a: &str, b: &str) -> Result<()> {
    let qg = atlas_query::QGraph::from_file(graph).with_context(|| format!("loading {graph}"))?;
    match qg.path(a, b) {
        Ok(Some(p)) => println!("{}", p.render(|id| qg.label_for_id(id))),
        Ok(None) => println!("No path found between '{a}' and '{b}'."),
        Err(msg) => bail!(msg),
    }
    Ok(())
}

pub fn explain(graph: &str, node: &str) -> Result<()> {
    let qg = atlas_query::QGraph::from_file(graph).with_context(|| format!("loading {graph}"))?;
    match qg.explain(node) {
        Some(e) => print!("{}", e.render()),
        None => println!("No node matching '{node}' found."),
    }
    Ok(())
}

// ── serve ───────────────────────────────────────────────────────────────────

/// MCP stdio server: same newline-delimited JSON-RPC loop as the `atlas-serve`
/// bin, reusing `atlas_serve::handle_request`.
pub fn serve(graph: &str, transport: &str) -> Result<()> {
    if transport != "stdio" {
        bail!("transport {transport:?} not implemented; use --transport stdio");
    }
    let qg = atlas_query::QGraph::from_file(graph).with_context(|| format!("loading {graph}"))?;
    eprintln!("atlas: serving {graph} over MCP (stdio)");
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": format!("Parse error: {e}")}
                });
                writeln!(stdout, "{resp}")?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(resp) = atlas_serve::handle_request(&qg, &req) {
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

// ── shared helpers ──────────────────────────────────────────────────────────

/// Run Louvain and write each node's `community` id back onto the graph.
/// Returns the community count.
fn stamp_communities(g: &mut Graph) -> usize {
    let model = atlas_graph::Model::new(g);
    let clustering = atlas_graph::cluster(&model, atlas_graph::DEFAULT_RESOLUTION);
    for n in &mut g.nodes {
        if let Some(id) = n.get("id").and_then(Value::as_str) {
            let cid = clustering.node_community.get(id).copied().unwrap_or(0);
            n.insert("community".into(), json!(cid));
        }
    }
    clustering.len()
}

fn write_graph(g: &Graph, path: &Path) -> Result<()> {
    let s = g.to_json_string_pretty()?;
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn write_report(g: &Graph, out_dir: &Path, root: &str) -> Result<()> {
    let model = atlas_graph::Model::new(g);
    let clustering = atlas_graph::cluster(&model, atlas_graph::DEFAULT_RESOLUTION);
    let today = today();
    let report = atlas_graph::render_report(&model, &clustering, root, &today);
    let p = out_dir.join("GRAPH_REPORT.md");
    std::fs::write(&p, report).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

fn write_viz(g: &Graph, out_dir: &Path, root: &str) -> Result<()> {
    write_report(g, out_dir, root)?;
    let html = atlas_export::to_html(g);
    let p = out_dir.join("graph.html");
    std::fs::write(&p, html).with_context(|| format!("writing {}", p.display()))?;
    println!(
        "Wrote GRAPH_REPORT.md and graph.html to {}.",
        out_dir.display()
    );
    Ok(())
}

/// Current UTC date (YYYY-MM-DD) with no chrono dep.
// ponytail: hand-rolled civil-date from the epoch; swap for `time` only if we
// ever need more than a report datestamp.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

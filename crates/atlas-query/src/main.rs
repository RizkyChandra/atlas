//! atlas-query CLI: `explain` / `path` / `query` over a graph.json.
//!
//! Usage:
//!   atlas-query --graph G explain "<node>"
//!   atlas-query --graph G path "<a>" "<b>"
//!   atlas-query --graph G query "<question>" [--dfs] [--budget N]
//!
//! Thin hand-rolled argv parsing (ponytail: no clap dep for three subcommands).

use atlas_query::{QGraph, DEFAULT_BUDGET};
use std::process::exit;

fn usage() -> ! {
    eprintln!(
        "Usage:\n  atlas-query --graph <graph.json> explain \"<node>\"\n  \
         atlas-query --graph <graph.json> path \"<a>\" \"<b>\"\n  \
         atlas-query --graph <graph.json> query \"<question>\" [--dfs] [--budget N]"
    );
    exit(2)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Pull --graph/--budget/--dfs out; positionals are cmd + operands.
    let mut graph_path: Option<String> = None;
    let mut budget = DEFAULT_BUDGET;
    let mut dfs = false;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--graph" => {
                graph_path = args.get(i + 1).cloned();
                i += 2;
            }
            "--budget" => {
                budget = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| {
                        eprintln!("error: --budget must be an integer");
                        exit(1)
                    });
                i += 2;
            }
            "--dfs" => {
                dfs = true;
                i += 1;
            }
            _ => {
                pos.push(args[i].clone());
                i += 1;
            }
        }
    }
    let graph_path = graph_path.unwrap_or_else(|| {
        eprintln!("error: --graph <graph.json> is required");
        exit(1)
    });
    let Some(cmd) = pos.first().cloned() else {
        usage()
    };
    let g = QGraph::from_file(&graph_path)?;

    match cmd.as_str() {
        "explain" => {
            let node = pos.get(1).unwrap_or_else(|| usage());
            match g.explain(node) {
                Some(e) => print!("{}", e.render()),
                None => println!("No node matching '{node}' found."),
            }
        }
        "path" => {
            let (a, b) = match (pos.get(1), pos.get(2)) {
                (Some(a), Some(b)) => (a, b),
                _ => usage(),
            };
            match g.path(a, b) {
                Ok(Some(p)) => println!("{}", p.render(|id| g.label_for_id(id))),
                Ok(None) => println!("No path found between '{a}' and '{b}'."),
                Err(msg) => {
                    eprintln!("{msg}");
                    exit(1)
                }
            }
        }
        "query" => {
            let question = pos.get(1).unwrap_or_else(|| usage());
            let r = g.query(question, budget, dfs);
            if r.seeds.is_empty() {
                println!("No matching nodes found.");
                return Ok(());
            }
            let seeds: Vec<String> = r.seeds.iter().map(|id| g.label_for_id(id)).collect();
            println!(
                "Traversal: {} depth=2 | Start: {:?} | {} nodes found\n",
                r.mode.to_uppercase(),
                seeds,
                r.nodes.len()
            );
            for id in &r.nodes {
                println!("  {} ({id})", g.label_for_id(id));
            }
        }
        _ => usage(),
    }
    Ok(())
}

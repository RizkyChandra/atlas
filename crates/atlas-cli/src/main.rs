//! atlas CLI entry point.
//!
//! Registers the graphify command surface as clap subcommands so `atlas --help`
//! mirrors `graphify`, and (M10) wires the pipeline commands end-to-end by
//! calling the sibling crates: extract → graph.json, cluster → GRAPH_REPORT.md,
//! export, query/path/explain, and serve (MCP over stdio). Command names/flags
//! are kept identical to graphify so `atlas` is a drop-in replacement.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod pipeline;

/// Default graph location, matching graphify's `graphify-out/graph.json`.
const DEFAULT_GRAPH: &str = "graphify-out/graph.json";

#[derive(Parser)]
#[command(
    name = "atlas",
    version,
    about = "Turn any folder of code, docs, or media into a queryable knowledge graph.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a graph.json against the extraction schema.
    Validate {
        /// Path to graph.json.
        path: String,
    },
    /// Report edges that reference a missing node (graphify #2130).
    Lint {
        /// Path to graph.json.
        path: String,
    },
    /// Load and re-serialize a graph.json (lossless round-trip check).
    Roundtrip {
        /// Path to graph.json.
        path: String,
        /// Write pretty JSON to stdout instead of validating silently.
        #[arg(long)]
        emit: bool,
    },

    /// Extract a knowledge graph from a folder.
    Extract {
        /// Directory to walk for source files.
        dir: String,
        /// Index only code (AST) — atlas is code-only today, so this is the default behavior.
        #[arg(long)]
        code_only: bool,
        /// Skip clustering + graph.html; write graph.json only.
        #[arg(long)]
        no_viz: bool,
    },
    /// Re-run clustering on an existing graph and rewrite GRAPH_REPORT.md.
    ClusterOnly {
        /// Project dir containing graphify-out/graph.json (default: CWD).
        #[arg(default_value = ".")]
        dir: String,
    },
    /// Export the graph (html/svg/graphml/cypher).
    Export {
        /// Output format: html | svg | graphml | cypher.
        format: String,
        /// Path to graph.json.
        #[arg(long, default_value = DEFAULT_GRAPH)]
        graph: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        output: Option<String>,
    },
    /// Query the graph in natural language.
    Query {
        /// The question / search terms.
        question: String,
        #[arg(long, default_value = DEFAULT_GRAPH)]
        graph: String,
        #[arg(long)]
        dfs: bool,
        #[arg(long)]
        budget: Option<usize>,
    },
    /// Trace the shortest path between two nodes.
    Path {
        a: String,
        b: String,
        #[arg(long, default_value = DEFAULT_GRAPH)]
        graph: String,
    },
    /// Explain one node and its connections.
    Explain {
        node: String,
        #[arg(long, default_value = DEFAULT_GRAPH)]
        graph: String,
    },
    /// Start the MCP server (stdio).
    Serve {
        #[arg(long, default_value = DEFAULT_GRAPH)]
        graph: String,
        #[arg(long, default_value = "stdio")]
        transport: String,
    },

    // ---- surface registered, implemented in later milestones ----
    /// [M2/M3] Re-extract only changed files.
    Update { args: Vec<String> },
    /// [M3] (Re)name communities.
    Label { args: Vec<String> },
    /// [M8] Fetch a URL/paper/video and add it to the graph.
    Add { args: Vec<String> },
    /// [M9] Register the skill with an AI assistant.
    Install { args: Vec<String> },
    /// [M9] Remove atlas from all platforms.
    Uninstall { args: Vec<String> },
    /// [M9] Auto-rebuild on git commit.
    Watch { args: Vec<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let r = match cli.command {
        Command::Validate { path } => return run_validate(&path),
        Command::Lint { path } => return run_lint(&path),
        Command::Roundtrip { path, emit } => return run_roundtrip(&path, emit),
        Command::Extract {
            dir,
            code_only,
            no_viz,
        } => pipeline::extract(&dir, code_only, no_viz),
        Command::ClusterOnly { dir } => pipeline::cluster_only(&dir),
        Command::Export {
            format,
            graph,
            output,
        } => pipeline::export(&format, &graph, output.as_deref()),
        Command::Query {
            question,
            graph,
            dfs,
            budget,
        } => pipeline::query(&graph, &question, dfs, budget),
        Command::Path { a, b, graph } => pipeline::path(&graph, &a, &b),
        Command::Explain { node, graph } => pipeline::explain(&graph, &node),
        Command::Serve { graph, transport } => pipeline::serve(&graph, &transport),
        other => {
            eprintln!("atlas: `{}` is not implemented yet.", stub_name(&other));
            eprintln!("       (planned for a later milestone — see the port plan)");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atlas: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn stub_name(c: &Command) -> &'static str {
    match c {
        Command::Update { .. } => "update",
        Command::Label { .. } => "label",
        Command::Add { .. } => "add",
        Command::Install { .. } => "install",
        Command::Uninstall { .. } => "uninstall",
        Command::Watch { .. } => "watch",
        _ => "command",
    }
}

fn load(path: &str) -> Result<atlas_core::Graph, ExitCode> {
    atlas_core::Graph::from_file(path).map_err(|e| {
        eprintln!("atlas: {e}");
        ExitCode::FAILURE
    })
}

fn run_validate(path: &str) -> ExitCode {
    let g = match load(path) {
        Ok(g) => g,
        Err(c) => return c,
    };
    match g.validate() {
        Ok(()) => {
            println!(
                "ok: {} nodes, {} edges, schema valid",
                g.nodes.len(),
                g.links.len()
            );
            ExitCode::SUCCESS
        }
        Err(errs) => {
            eprintln!("invalid: {} schema error(s)", errs.len());
            for e in &errs {
                eprintln!("  - {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run_lint(path: &str) -> ExitCode {
    let g = match load(path) {
        Ok(g) => g,
        Err(c) => return c,
    };
    let dangling = g.dangling_edges();
    if dangling.is_empty() {
        println!("ok: no dangling edges");
        ExitCode::SUCCESS
    } else {
        eprintln!("{} dangling edge(s):", dangling.len());
        for (i, msg) in &dangling {
            eprintln!("  - edge[{i}] {msg}");
        }
        ExitCode::FAILURE
    }
}

fn run_roundtrip(path: &str, emit: bool) -> ExitCode {
    let g = match load(path) {
        Ok(g) => g,
        Err(c) => return c,
    };
    match g.to_json_string_pretty() {
        Ok(s) => {
            if emit {
                println!("{s}");
            } else {
                println!(
                    "ok: round-tripped {} nodes, {} edges",
                    g.nodes.len(),
                    g.links.len()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atlas: {e}");
            ExitCode::FAILURE
        }
    }
}

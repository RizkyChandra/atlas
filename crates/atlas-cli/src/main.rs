//! atlas CLI entry point.
//!
//! M0 registers the graphify command surface as clap subcommands so `atlas
//! --help` mirrors `graphify`. Only the contract-level commands are wired up so
//! far (`validate`, `lint`, `roundtrip`); everything else is a stub that names
//! the milestone that will implement it. Command names/flags are kept identical
//! to graphify so `atlas` is a drop-in replacement.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

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

    // ---- surface registered now, implemented in later milestones ----
    /// [M1/M2] Extract a knowledge graph from a folder.
    Extract { args: Vec<String> },
    /// [M2/M3] Re-extract only changed files.
    Update { args: Vec<String> },
    /// [M3] Re-run clustering on an existing graph.
    ClusterOnly { args: Vec<String> },
    /// [M3] (Re)name communities.
    Label { args: Vec<String> },
    /// [M4] Export the graph (html/svg/graphml/cypher/obsidian/...).
    Export { args: Vec<String> },
    /// [M5] Query the graph in natural language.
    Query { args: Vec<String> },
    /// [M5] Trace the shortest path between two nodes.
    Path { args: Vec<String> },
    /// [M5] Explain one node and its connections.
    Explain { args: Vec<String> },
    /// [M8] Fetch a URL/paper/video and add it to the graph.
    Add { args: Vec<String> },
    /// [M7] Start the MCP server (stdio/http).
    Serve { args: Vec<String> },
    /// [M9] Register the skill with an AI assistant.
    Install { args: Vec<String> },
    /// [M9] Remove atlas from all platforms.
    Uninstall { args: Vec<String> },
    /// [M9] Auto-rebuild on git commit.
    Watch { args: Vec<String> },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate { path } => run_validate(&path),
        Command::Lint { path } => run_lint(&path),
        Command::Roundtrip { path, emit } => run_roundtrip(&path, emit),
        other => {
            eprintln!("atlas: `{}` is not implemented yet.", stub_name(&other));
            eprintln!("       (planned for a later milestone — see the port plan)");
            ExitCode::from(2)
        }
    }
}

fn stub_name(c: &Command) -> &'static str {
    match c {
        Command::Extract { .. } => "extract",
        Command::Update { .. } => "update",
        Command::ClusterOnly { .. } => "cluster-only",
        Command::Label { .. } => "label",
        Command::Export { .. } => "export",
        Command::Query { .. } => "query",
        Command::Path { .. } => "path",
        Command::Explain { .. } => "explain",
        Command::Add { .. } => "add",
        Command::Serve { .. } => "serve",
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
                println!("ok: round-tripped {} nodes, {} edges", g.nodes.len(), g.links.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("atlas: {e}");
            ExitCode::FAILURE
        }
    }
}

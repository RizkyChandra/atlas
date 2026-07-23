//! `atlas-serve` — MCP stdio server binary. Loads a graph.json and serves the
//! query tools over newline-delimited JSON-RPC 2.0 on stdin/stdout.

use anyhow::Result;
use atlas_query::QGraph;
use clap::Parser;
use std::io::{BufRead, Write};

#[derive(Parser)]
#[command(
    name = "atlas-serve",
    about = "Serve an atlas/graphify graph over MCP (stdio)."
)]
struct Args {
    /// Path to graph.json.
    #[arg(long, default_value = "graphify-out/graph.json")]
    graph: String,
    /// Transport. Only `stdio` is implemented; `http` is a stub.
    #[arg(long, default_value = "stdio")]
    transport: String,
    // HTTP transport flags — parsed but not yet wired (stdio is the priority).
    // ponytail: stubbed; port graphify's Streamable-HTTP transport when a shared
    // team server is actually needed.
    #[arg(long, default_value = "127.0.0.1", hide = true)]
    host: String,
    #[arg(long, default_value_t = 8080, hide = true)]
    port: u16,
    #[arg(long, hide = true)]
    api_key: Option<String>,
    #[arg(long, default_value = "/mcp", hide = true)]
    path: String,
    #[arg(long, hide = true)]
    stateless: bool,
    #[arg(long, default_value_t = 3600.0, hide = true)]
    session_timeout: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.transport == "http" {
        anyhow::bail!(
            "http transport not yet implemented (host={}, port={}, path={}, api_key={:?}, \
             stateless={}, session_timeout={}); use --transport stdio.",
            args.host,
            args.port,
            args.path,
            args.api_key,
            args.stateless,
            args.session_timeout
        );
    }

    let qg = QGraph::from_file(&args.graph)
        .map_err(|e| anyhow::anyhow!("loading graph {}: {e}", args.graph))?;
    eprintln!(
        "atlas-serve: loaded {} — serving MCP over stdio",
        args.graph
    );

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue; // some clients send blank lines between messages
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Parse error with a null id, per JSON-RPC.
                let resp = serde_json::json!({
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

//! atlas-serve — MCP server exposing a graphify `graph.json` over stdio.
//!
//! Ported from graphify `graphify/serve.py`. The query tools are thin wrappers
//! over [`atlas_query::QGraph`] (which already ports graphify's query/path/
//! explain semantics), so this crate only owns the MCP wire protocol + the
//! text rendering of each tool's result.
//!
//! Path taken: **raw MCP JSON-RPC 2.0 over stdio**, not the `rmcp` SDK. The
//! only published `rmcp` is `3.0.0-beta.1` — a beta with a churning API that
//! would pull in tokio/schemars for what is a small, well-specified protocol
//! (initialize / tools/list / tools/call). serde_json (already a workspace
//! dep) covers it in ~200 lines with zero new heavy deps.
//! ponytail: raw JSON-RPC; swap to rmcp only once it ships a stable release.

use atlas_query::QGraph;
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// The tool set graphify exposes. The four query tools are wired to `QGraph`;
/// the PR tools are stubs (graphify's `prs` module isn't ported yet).
pub fn tools_list() -> Value {
    json!([
        {
            "name": "query_graph",
            "description": "Search the knowledge graph using BFS or DFS. Returns relevant nodes and edges as text context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "Natural language question or keyword search"},
                    "mode": {"type": "string", "enum": ["bfs", "dfs"], "default": "bfs", "description": "bfs=broad context, dfs=trace a specific path"},
                    "token_budget": {"type": "integer", "default": 1500, "description": "Max nodes in the returned subgraph"}
                },
                "required": ["question"]
            }
        },
        {
            "name": "get_node",
            "description": "Get full details for a specific node by label or ID.",
            "inputSchema": {
                "type": "object",
                "properties": {"label": {"type": "string", "description": "Node label or ID to look up"}},
                "required": ["label"]
            }
        },
        {
            "name": "get_neighbors",
            "description": "Get all direct neighbors of a node with edge details.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "label": {"type": "string"},
                    "relation_filter": {"type": "string", "description": "Optional: filter by relation type"}
                },
                "required": ["label"]
            }
        },
        {
            "name": "shortest_path",
            "description": "Find the shortest path between two concepts in the knowledge graph.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string", "description": "Source concept label or keyword"},
                    "target": {"type": "string", "description": "Target concept label or keyword"}
                },
                "required": ["source", "target"]
            }
        },
        {
            "name": "list_prs",
            "description": "List open GitHub PRs with graph impact. (Not yet implemented in atlas.)",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "get_pr_impact",
            "description": "Get graph impact for a specific PR. (Not yet implemented in atlas.)",
            "inputSchema": {
                "type": "object",
                "properties": {"pr_number": {"type": "integer"}},
                "required": ["pr_number"]
            }
        },
        {
            "name": "triage_prs",
            "description": "Rank actionable open PRs by graph impact. (Not yet implemented in atlas.)",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Dispatch a `tools/call` to its handler. `Err` is a tool-level error string
/// (rendered into an MCP error result); handlers themselves return text that
/// includes "not found" as ordinary content, matching graphify.
pub fn call_tool(qg: &QGraph, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "query_graph" => Ok(tool_query_graph(qg, args)),
        "get_node" => Ok(tool_get_node(qg, args)),
        "get_neighbors" => Ok(tool_get_neighbors(qg, args)),
        "shortest_path" => Ok(tool_shortest_path(qg, args)),
        "list_prs" | "get_pr_impact" | "triage_prs" => Ok(format!(
            "{name}: not yet implemented in atlas (graphify PR tooling is not ported)."
        )),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn tool_get_node(qg: &QGraph, args: &Value) -> String {
    let label = match str_arg(args, "label") {
        Some(l) => l,
        None => return "error: missing required argument 'label'".into(),
    };
    match qg.explain(label) {
        Some(e) => format!(
            "Node: {}\n  ID: {}\n  Source: {}\n  Community: {}\n  Degree: {}",
            e.label, e.id, e.source, e.community, e.degree
        ),
        None => format!("No node matching '{label}' found."),
    }
}

fn tool_get_neighbors(qg: &QGraph, args: &Value) -> String {
    let label = match str_arg(args, "label") {
        Some(l) => l,
        None => return "error: missing required argument 'label'".into(),
    };
    let rel_filter = str_arg(args, "relation_filter")
        .unwrap_or("")
        .to_lowercase();
    let e = match qg.explain(label) {
        Some(e) => e,
        None => return format!("No node matching '{label}' found."),
    };
    let mut lines = vec![format!("Neighbors of {}:", e.label)];
    for c in &e.connections {
        if !rel_filter.is_empty() && !c.relation.to_lowercase().contains(&rel_filter) {
            continue;
        }
        let arrow = if c.direction == "out" { "-->" } else { "<--" };
        lines.push(format!(
            "  {arrow} {} [{}] [{}]",
            c.neighbor, c.relation, c.confidence
        ));
    }
    lines.join("\n")
}

fn tool_shortest_path(qg: &QGraph, args: &Value) -> String {
    let (src, tgt) = match (str_arg(args, "source"), str_arg(args, "target")) {
        (Some(s), Some(t)) => (s, t),
        _ => return "error: missing required argument 'source' and/or 'target'".into(),
    };
    match qg.path(src, tgt) {
        Ok(Some(p)) => p.render(|id| qg.label_for_id(id)),
        Ok(None) => format!("No path found between '{src}' and '{tgt}'."),
        Err(msg) => msg,
    }
}

fn tool_query_graph(qg: &QGraph, args: &Value) -> String {
    let question = match str_arg(args, "question") {
        Some(q) => q,
        None => return "error: missing required argument 'question'".into(),
    };
    let budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(atlas_query::DEFAULT_BUDGET);
    let dfs = str_arg(args, "mode") == Some("dfs");
    let r = qg.query(question, budget, dfs);
    if r.nodes.is_empty() {
        return "No matching nodes found.".into();
    }
    let seeds: Vec<String> = r.seeds.iter().map(|id| qg.label_for_id(id)).collect();
    let mut out = format!(
        "Traversal: {} | Start: {:?} | {} nodes found\n",
        r.mode.to_uppercase(),
        seeds,
        r.nodes.len()
    );
    for id in &r.nodes {
        out.push_str(&format!("NODE {}\n", qg.label_for_id(id)));
    }
    for (u, v) in &r.edges {
        out.push_str(&format!(
            "EDGE {} --> {}\n",
            qg.label_for_id(u),
            qg.label_for_id(v)
        ));
    }
    out
}

// ---- JSON-RPC 2.0 request handling ----

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn err(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handle one parsed JSON-RPC request. Returns `Some(response)` for requests
/// and `None` for notifications (no `id` → nothing is sent back).
pub fn handle_request(qg: &QGraph, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    // Notifications (e.g. notifications/initialized) carry no id: never reply.
    let id = match req.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => return None,
    };
    Some(match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "atlas", "version": env!("CARGO_PKG_VERSION")}
            }),
        ),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({"tools": tools_list()})),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(qg, name, &args) {
                Ok(text) => ok(
                    id,
                    json!({"content": [{"type": "text", "text": text}], "isError": false}),
                ),
                // Unknown tool / bad args surface as an isError result, per MCP:
                // tool failures are results, not protocol errors.
                Err(msg) => ok(
                    id,
                    json!({"content": [{"type": "text", "text": msg}], "isError": true}),
                ),
            }
        }
        _ => err(id, -32601, &format!("Method not found: {method}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_query::QGraph;

    const GOLDEN: &str = "/home/yoshirakou/work/graphify/worked/httpx/graph.json";

    fn qg() -> QGraph {
        QGraph::from_file(GOLDEN).expect("load golden httpx graph")
    }

    #[test]
    fn get_node_returns_attrs() {
        let out = tool_get_node(&qg(), &json!({"label": "client"}));
        assert!(out.contains("ID: client"), "got: {out}");
        assert!(out.contains("Node: client.py"), "got: {out}");
        assert!(out.contains("Degree:"), "got: {out}");
    }

    #[test]
    fn get_neighbors_returns_adjacency() {
        let out = tool_get_neighbors(&qg(), &json!({"label": "client"}));
        // From the golden graph, node `client` has 10 incident edges, and
        // imports_from client->models is one of them.
        assert!(out.starts_with("Neighbors of client.py:"), "got: {out}");
        let n = out
            .lines()
            .filter(|l| l.starts_with("  -->") || l.starts_with("  <--"))
            .count();
        assert_eq!(n, 10, "expected 10 neighbours, got: {out}");
        assert!(out.contains("imports_from"), "got: {out}");
    }

    #[test]
    fn get_neighbors_relation_filter() {
        let out = tool_get_neighbors(
            &qg(),
            &json!({"label": "client", "relation_filter": "contains"}),
        );
        for l in out.lines().skip(1) {
            assert!(l.contains("contains"), "filter leaked: {l}");
        }
    }

    #[test]
    fn shortest_path_valid() {
        let out = tool_shortest_path(&qg(), &json!({"source": "client", "target": "models"}));
        assert!(out.starts_with("Shortest path"), "got: {out}");
        assert!(out.contains("-->") || out.contains("<--"), "got: {out}");
    }

    #[test]
    fn query_graph_nonempty_scoped() {
        let out = tool_query_graph(&qg(), &json!({"question": "client"}));
        assert!(out.contains("nodes found"), "got: {out}");
        assert!(out.contains("NODE "), "expected node lines, got: {out}");
    }

    #[test]
    fn pr_tools_stubbed() {
        for t in ["list_prs", "get_pr_impact", "triage_prs"] {
            let out = call_tool(&qg(), t, &json!({})).unwrap();
            assert!(out.contains("not yet implemented"), "{t}: {out}");
        }
    }

    #[test]
    fn tools_list_has_valid_schemas() {
        let tools = tools_list();
        let arr = tools.as_array().unwrap();
        let names: Vec<&str> = arr.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in [
            "query_graph",
            "get_node",
            "get_neighbors",
            "shortest_path",
            "list_prs",
            "get_pr_impact",
            "triage_prs",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        for t in arr {
            assert!(
                t["description"].is_string(),
                "tool missing description: {t}"
            );
            let schema = &t["inputSchema"];
            assert_eq!(schema["type"], "object", "schema not object: {t}");
            assert!(
                schema["properties"].is_object(),
                "schema missing properties: {t}"
            );
        }
    }

    #[test]
    fn jsonrpc_initialize_and_tools_call() {
        let g = qg();
        // initialize
        let init = handle_request(
            &g,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["result"]["serverInfo"]["name"], "atlas");
        // notification → no response
        assert!(handle_request(
            &g,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .is_none());
        // tools/list
        let list = handle_request(
            &g,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .unwrap();
        assert!(list["result"]["tools"].as_array().unwrap().len() >= 4);
        // tools/call
        let call = handle_request(
            &g,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_node", "arguments": {"label": "client"}}}),
        )
        .unwrap();
        assert_eq!(call["result"]["isError"], false);
        let text = call["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("ID: client"), "got: {text}");
        // unknown method → error
        let bad =
            handle_request(&g, &json!({"jsonrpc": "2.0", "id": 4, "method": "bogus"})).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }
}

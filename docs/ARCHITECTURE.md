# Architecture

`atlas` is a Cargo workspace. Each crate owns one stage of the pipeline, mirroring
the module boundaries of the Python tool it ports ([graphify](https://github.com/Graphify-Labs/graphify)).

## Pipeline

```
extract → resolve → build → cluster → analyze → report → export
                                    ↘ query / path / explain
                                    ↘ serve (MCP)
```

`atlas extract <dir>` walks the directory, runs the per-file extractor on each
supported file, merges and dedupes, resolves cross-file symbols, and writes
`graphify-out/graph.json`. Unless `--no-viz`, it then clusters, renders
`GRAPH_REPORT.md`, and writes a self-contained `graph.html`.

## Crates

| Crate | Responsibility |
|---|---|
| `atlas-core` | The data contract: a lossless NetworkX **node-link** `Graph` (every node/edge attribute + key order preserved), schema `validate`, the `#2130` dangling-edge lint, and `ids` — the one node-ID normalization recipe every producer shares (ported byte-for-byte from graphify `ids.py`). |
| `atlas-extract` | tree-sitter extraction for 46 languages/formats. A config-driven generic engine (`engine.rs`, `LanguageConfig`) handles the grammar-uniform languages; standalone modules handle the rest (`go.rs`, `bash.rs`, `sql.rs`, `vue.rs`, …). `resolve.rs` is the corpus-level cross-file pass (stub collapse, import→file-node repointing, cross-file calls). |
| `atlas-graph` | Builds a `petgraph` graph, runs deterministic **Louvain** community detection (no mature Rust Leiden), ranks god-nodes, finds surprising connections, and renders `GRAPH_REPORT.md`. |
| `atlas-export` | `graph.json` → GraphML, Cypher, hand-rolled SVG, and a self-contained interactive HTML (no external CDN). |
| `atlas-query` | `query` / `path` / `explain` over a `graph.json` via a `petgraph` `DiGraph` — fuzzy node resolution, BFS shortest path, budgeted subgraph expansion. |
| `atlas-llm` | 8 HTTP backends (OpenAI/Gemini/Kimi/DeepSeek/Ollama/Azure/Claude/Bedrock) + `claude-cli` for the optional semantic pass over docs. `reqwest`; tests mock HTTP. |
| `atlas-serve` | MCP server — raw JSON-RPC 2.0 over stdio (`initialize`/`tools/list`/`tools/call`), tools wired to `atlas-query`. |
| `atlas-ingest` | Document readers (markdown links/wikilinks, CSV/TSV, PDF, docx/xlsx, Jupyter) + a boundary-aware text chunker. Heavy readers are behind Cargo features. |
| `atlas-install` | Skill installers for AI assistants (Claude Code, Codex, Cursor, Gemini, …) with embedded skill assets, plus git post-commit/checkout hooks. |
| `atlas-cli` | The `atlas` binary — clap dispatch that wires the crates into the pipeline commands. |

## The graph contract

Every extractor emits `{nodes, edges}` where a node is
`{id, label, source_file, source_location, …}` and an edge is
`{source, target, relation, confidence, …}`. `atlas-core::ids::make_id` derives
every id from the file stem + symbol name, so the three producers (AST extractor,
cross-file resolver, and any future semantic pass) never disagree and split an
entity into ghost nodes.

Confidence: `EXTRACTED` (explicit in source) · `INFERRED` (resolved) ·
`AMBIGUOUS` (flagged).

## Adding a language

1. If the grammar fits the generic engine, add a `Lang` variant + a
   `LanguageConfig` in `engine.rs`. Otherwise write a standalone `src/<lang>.rs`
   module following an existing one (e.g. `go.rs`).
2. Register the file extensions in `lib.rs` dispatch.
3. Add the tree-sitter grammar crate to `atlas-extract/Cargo.toml`.
4. Add a fixture + an oracle-diff test in `tests/langs.rs`. For graphify-supported
   languages, generate the oracle with graphify's `extract --code-only`; for new
   languages, hand-author the expected golden.

## Testing

The suite is **hermetic** — all fixtures live under `fixtures/` in the repo, read
via `CARGO_MANIFEST_DIR`-relative paths, so `cargo test --workspace` needs nothing
outside the repo. Language tests diff extractor output against committed oracle
graphs; `atlas-cli` has an end-to-end `extract → validate → query → export` test.

## Provenance

`atlas` reproduces graphify's deterministic `--code-only` output and folds in the
fixes/features from graphify's open PRs and issues — the triaged ledger is in
[../BACKLOG.md](../BACKLOG.md).

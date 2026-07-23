# atlas

**Turn any folder of code into a queryable knowledge graph — one static binary, no runtime.**

`atlas` parses your project locally with tree-sitter, builds a graph of how
everything connects (calls, imports, inheritance, references), detects
communities, and lets you **query the graph instead of grepping files**. It is a
Rust port of the Python tool [graphify](https://github.com/Graphify-Labs/graphify):
same commands, same `graphify-out/` artifacts (`graph.json`, `graph.html`,
`GRAPH_REPORT.md`), but shipped as a single dependency-free binary.

- **Local & deterministic.** Code is parsed with tree-sitter AST — no LLM, nothing
  leaves your machine. `atlas` reproduces graphify's `--code-only` build output
  exactly (verified against committed goldens).
- **One binary.** No Python, no `pip`, no virtualenv. Download it and run.
- **46 languages & formats.** See the list below.

```bash
atlas extract .                      # build graphify-out/{graph.json, GRAPH_REPORT.md, graph.html}
atlas query "what connects auth to the database?"
atlas path  UserService DatabasePool # shortest path between two nodes
atlas explain RateLimiter            # one node + its connections
atlas serve                          # expose the graph to an AI assistant over MCP
```

## Install

**Prebuilt binary** (recommended) — grab the archive for your platform from the
[latest release](https://github.com/RizkyChandra/atlas/releases) and put `atlas`
on your `PATH`. Linux packages (`.deb`/`.rpm`/`.apk`) are attached too. Releases
are built by [GoReleaser](https://goreleaser.com) for linux/macOS/windows on
x86_64 and aarch64.

**From source** (needs a Rust toolchain):

```bash
cargo install --path crates/atlas-cli    # installs the `atlas` binary
# or
cargo build --release                    # -> target/release/atlas
```

## Commands

| Command | What it does |
|---|---|
| `atlas extract <dir> [--no-viz]` | Walk a directory, extract the code graph, write `graphify-out/graph.json` (+ cluster → `GRAPH_REPORT.md` + `graph.html` unless `--no-viz`) |
| `atlas cluster-only <dir>` | Re-run Louvain community detection on an existing graph |
| `atlas query "<question>"` | Return a scoped subgraph for a plain-language question (`--dfs`, `--budget`) |
| `atlas path <a> <b>` | Shortest path between two nodes, hop by hop |
| `atlas explain <node>` | A node's source, community, degree, and connections |
| `atlas export <html\|svg\|graphml\|cypher> [--graph P]` | Export the graph to another format |
| `atlas serve [--graph P]` | MCP server (stdio) exposing `query_graph`/`get_node`/`get_neighbors`/`shortest_path` |
| `atlas validate\|lint\|roundtrip <graph.json>` | Schema-check / find dangling edges / lossless round-trip |
| `atlas install [--platform …]` | Register the skill with an AI assistant (Claude Code, Codex, Cursor, Gemini, …) |

Every edge carries a confidence tag — `EXTRACTED` (explicit in source),
`INFERRED` (resolved), or `AMBIGUOUS`.

## Languages & formats (46)

**Code:** Python · JavaScript · TypeScript · TSX · Go · Rust · Java · C · C++ ·
CUDA · Metal · Ruby · Kotlin · Scala · C# · PHP · Swift · Lua · Bash · Elixir ·
Zig · PowerShell · Objective-C · Julia · Fortran · Dart · Groovy · SQL ·
Terraform/HCL · Verilog/SystemVerilog · Pascal/Delphi · Apex · R · Nix ·
Solidity · Ada

**Frameworks / markup:** Vue · Svelte · Astro · Razor/Blazor · Blade · XAML

**Project / config:** .NET projects (`.sln`/`.csproj`/`.fsproj`/`.vbproj`) ·
JSON config (`mcp.json`/`package.json`/`tsconfig.json`) · Pascal forms
(`.dfm`/`.lfm`/`.lpk`)

Every graphify-supported language above reproduces graphify's `--code-only`
oracle exactly; R/Nix/Solidity/Ada are new to `atlas` (not in graphify).

## Architecture

A Cargo workspace whose crates mirror the pipeline. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for detail.

```
crates/
  atlas-core/     graph model (NetworkX node-link), schema validation, node-ID recipe
  atlas-extract/  tree-sitter extraction (46 langs) + cross-file symbol resolution
  atlas-graph/    build + Louvain communities + god-nodes + GRAPH_REPORT.md
  atlas-export/   graph.json → html / svg / graphml / cypher
  atlas-query/    query / path / explain over a graph.json
  atlas-llm/      8 LLM backends (semantic pass over docs) — reqwest, mocked in tests
  atlas-serve/    MCP server (JSON-RPC over stdio)
  atlas-ingest/   markdown / csv / pdf / office / ipynb readers + chunking
  atlas-install/  AI-assistant skill installers + git hooks
  atlas-cli/      the `atlas` binary (clap dispatch)
```

## Status

Early but functional: `atlas extract` → `query`/`export`/`serve` works end to end
and its extraction output matches graphify's built graph exactly on the reference
corpus. Provenance and the full remaining-work ledger (folding in graphify's open
PRs/issues) live in [BACKLOG.md](BACKLOG.md); progress is tracked on the
[project board](https://github.com/users/RizkyChandra/projects/6).

Known gaps (tracked): non-Python cross-file resolution, ~45 more requested
languages, and the extraction bug-fixes catalogued in `BACKLOG.md`.

## Development

```bash
cargo test --workspace     # hermetic — fixtures vendored under fixtures/
cargo fmt --all --check
cargo clippy --workspace --all-targets
```

## License

Apache-2.0 (see [LICENSE](LICENSE)). `atlas` is an independent Rust reimplementation
of graphify; vendored test fixtures under `fixtures/graphify/` originate from the
graphify project (Apache-2.0).

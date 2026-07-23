# atlas

A single static binary that turns any folder of code, docs, or media into a
queryable knowledge graph. Rust port of [graphify](../graphify) — same commands,
same `graphify-out/` artifacts (`graph.json`, `graph.html`, `GRAPH_REPORT.md`),
no Python runtime.

> Status: **early**. M0 (workspace + graph.json contract) is done. Extraction,
> clustering, export, query, MCP, and the installer surface land in subsequent
> milestones — see the port plan and [`BACKLOG.md`](BACKLOG.md).

## Build

```bash
cargo build --release        # produces target/release/atlas (one static binary)
cargo test                   # unit + round-trip conformance against graphify goldens
```

## Working today (M0)

```bash
atlas validate  path/to/graph.json   # check against the extraction schema
atlas lint      path/to/graph.json   # report edges pointing at missing nodes (#2130)
atlas roundtrip path/to/graph.json   # lossless load → re-serialize
```

## Layout

```
crates/
  atlas-core/   graph model, node-link (de)serialization, schema validation
  atlas-cli/    the `atlas` binary (clap dispatch)
```

More crates (`atlas-extract`, `atlas-graph`, `atlas-export`, `atlas-query`,
`atlas-llm`, `atlas-serve`, `atlas-ingest`, `atlas-install`) are added as their
milestones begin, mirroring the graphify module boundaries.

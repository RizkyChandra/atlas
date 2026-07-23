//! atlas-extract — deterministic code-graph extraction, a Rust port of
//! graphify's `extract --code-only` AST pass.
//!
//! Milestone M2 wave 1: a config-driven generic engine (mirroring graphify's
//! `LanguageConfig` + `_extract_generic` in `graphify/extractors/engine.py`)
//! covering **Python, JavaScript, TypeScript, Java, C, C++**, plus dedicated
//! ports of graphify's standalone **Go** (`graphify/extractors/go.py`) and
//! **Rust** (`graphify/extractors/rust.py`) extractors.
//!
//! Node IDs come from [`atlas_core::ids`] (the one shared recipe).
//!
//! Output shape: the raw `{nodes, edges}` graphify emits per file, then run
//! through graphify's build-time collapse (`build.py::dedupe_nodes` by id,
//! last-writer-wins; `dedupe_edges` by `(source,target,relation)`, keep-first).
//! The oracle we gate against is graphify's built `graph.json`, which applies
//! exactly those two collapses, so we mirror them here (see [`dedupe_nodes`] /
//! [`dedupe_edges`]). Import/reference targets can still be dangling SOURCELESS
//! stubs (that is expected; graphify's cross-file rewire reconciles them — out
//! of scope for a single-file extract).
//!
//! Scope kept to a single file: no cross-file resolution, no INFERRED
//! `indirect_call` edges (dispatch tables / getattr / callback args), no JS/TS
//! arrow-function / `this.x = () => {}` capture, no receiver-typed member-call
//! resolution. Residual per-language deltas are documented in each test.

use atlas_core::Attrs;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

mod ada;
mod apex;
mod astro;
mod bash;
mod blade;
mod dart;
mod elixir;
mod engine;
mod fortran;
mod go;
mod json_config;
mod julia;
mod nix;
mod objc;
mod pascal;
mod pascal_forms;
mod powershell;
mod r_lang;
mod razor;
mod resolve;
mod rust_lang;
mod sfc;
mod sln;
mod solidity;
mod sql;
mod svelte;
mod terraform;
mod verilog;
mod vue;
mod xaml;
mod zig_lang;

pub use engine::Lang;
pub use resolve::resolve_corpus;

/// Raw extraction output: node and edge attribute maps, in emission order,
/// after the build-time node/edge collapse.
pub struct ExtractResult {
    pub nodes: Vec<Attrs>,
    pub edges: Vec<Attrs>,
}

/// Extract the code graph for a single source file, dispatching on extension.
pub fn extract_file(path: impl AsRef<Path>) -> std::io::Result<ExtractResult> {
    let path = path.as_ref();
    let source = std::fs::read(path)?;
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    // Blade uses the compound `.blade.php` extension — dispatch it before the
    // plain `.php` arm (which `path.extension()` alone would route to).
    let name_lc = path
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name_lc.ends_with(".blade.php") {
        let raw = blade::extract(path, &source);
        let (nodes, edges) = rewire_infile_stubs(raw.nodes, raw.edges);
        return Ok(ExtractResult {
            nodes: dedupe_nodes(nodes),
            edges: dedupe_edges(edges),
        });
    }

    let raw = match ext.as_str() {
        "py" | "pyi" => engine::extract(path, &source, Lang::Python),
        "js" | "jsx" | "mjs" | "cjs" => engine::extract(path, &source, Lang::Js),
        "ts" | "mts" | "cts" => engine::extract(path, &source, Lang::Ts),
        // .tsx: TS config, JSX-aware grammar (graphify routes .tsx → language_tsx).
        "tsx" => engine::extract(path, &source, Lang::Tsx),
        "java" => engine::extract(path, &source, Lang::Java),
        "c" | "h" => engine::extract(path, &source, Lang::C),
        // CUDA (.cu/.cuh) and Metal (.metal) reuse the C++ grammar/config, exactly
        // as graphify routes them to its cpp extractor.
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "cu" | "cuh" | "metal" => {
            engine::extract(path, &source, Lang::Cpp)
        }
        "go" => go::extract(path, &source),
        "rs" => rust_lang::extract(path, &source),
        "rb" | "rake" => engine::extract(path, &source, Lang::Ruby),
        "kt" | "kts" => engine::extract(path, &source, Lang::Kotlin),
        "scala" | "sc" => engine::extract(path, &source, Lang::Scala),
        "cs" => engine::extract(path, &source, Lang::CSharp),
        "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => {
            engine::extract(path, &source, Lang::Php)
        }
        "swift" => engine::extract(path, &source, Lang::Swift),
        "lua" | "luau" | "toc" => engine::extract(path, &source, Lang::Lua),
        "sh" | "bash" => bash::extract(path, &source),
        "ex" | "exs" => elixir::extract(path, &source),
        "zig" => zig_lang::extract(path, &source),
        "ps1" | "psm1" => powershell::extract(path, &source),
        // .psd1 PowerShell module manifest — dedicated extractor (graphify
        // `extract_powershell_manifest`), not the script pass.
        "psd1" => powershell::extract_manifest(path, &source),
        "m" | "mm" => objc::extract(path, &source),
        "jl" => julia::extract(path, &source),
        // Fortran. Lowercase .f* are unpreprocessed (clean anchors). Capital .F*
        // is lowercased by this match and routed here WITHOUT cpp -P — so its
        // line anchors differ from graphify's cpp-renumbered oracle (#2092);
        // out of scope, documented in tests/langs.rs.
        "f90" | "f95" | "f03" | "f08" | "f" | "for" | "ftn" | "fpp" => {
            fortran::extract(path, &source)
        }
        "dart" => dart::extract(path, &source),
        // Groovy/Gradle: engine-config (graphify `_GROOVY_CONFIG`). The Spock
        // regex fallback (`def "feature"()` specs) is NOT ported — such files
        // fall through to the tree-sitter pass; documented in tests/langs.rs.
        "groovy" | "gradle" => engine::extract(path, &source, Lang::Groovy),
        "sql" => sql::extract(path, &source),
        // .tf/.hcl blocks; .tfvars is Terraform's values store (same extractor —
        // top-level attributes only, so the graph is just the file node).
        "tf" | "hcl" | "tfvars" => terraform::extract(path, &source),
        // Verilog / SystemVerilog (tree-sitter walk + regex class augmentation).
        "v" | "sv" | "svh" | "vh" => verilog::extract(path, &source),
        // Pascal / Delphi (regex extractor — Rust grammar crate version does not
        // match the oracle venv grammar; see src/pascal.rs).
        "pas" | "pp" | "dpr" | "dpk" | "inc" | "lpr" => pascal::extract(path, &source),
        // Component single-file components: embedded <script> parsed via the
        // JS/TS engine (Vue masks non-script; Svelte/Astro feed the raw file)
        // plus a regex rescue for template/frontmatter/dynamic imports.
        "vue" => vue::extract(path, &source),
        "svelte" => svelte::extract(path, &source),
        "astro" => astro::extract(path, &source),
        // ASP.NET Razor (`.razor`/`.cshtml`) and WPF/XAML — pure-regex / XML ports.
        "razor" | "cshtml" => razor::extract(path, &source),
        "xaml" => xaml::extract(path, &source),
        // .NET solution/project files (see src/sln.rs).
        "sln" | "slnx" | "csproj" | "fsproj" | "vbproj" => sln::extract(path, &source),
        // Pascal forms (.dfm/.lfm) and Lazarus packages (.lpk) (see src/pascal_forms.rs).
        "dfm" | "lfm" | "lpk" => pascal_forms::extract(path, &source),
        // JSON: only recognized MCP/config/manifest shapes emit nodes; plain data
        // JSON is skipped (graphify #1224/#2107/#2108). See src/json_config.rs.
        "json" => json_config::extract(path, &source),
        // BACKLOG new languages (M2) — no graphify oracle; tree-sitter grammars.
        "r" => r_lang::extract(path, &source),
        "nix" => nix::extract(path, &source),
        "sol" => solidity::extract(path, &source),
        "adb" | "ads" => ada::extract(path, &source),
        // Apex (.cls / .trigger) — regex-based, no grammar (see src/apex.rs).
        "cls" | "trigger" => apex::extract(path, &source),
        // Unknown extension: empty graph (graphify returns nothing for these).
        _ => ExtractResult {
            nodes: vec![],
            edges: vec![],
        },
    };

    let (nodes, edges) = rewire_infile_stubs(raw.nodes, raw.edges);
    Ok(ExtractResult {
        nodes: dedupe_nodes(nodes),
        edges: dedupe_edges(edges),
    })
}

/// graphify's build-time symbol rewire, restricted to the single-file case:
/// a SOURCELESS stub (from `ensure_named_node` for a name not yet defined when
/// referenced — e.g. a forward reference to a class declared later in the file)
/// is collapsed onto the real in-file definition sharing its (normalized) label,
/// and the orphaned stub is dropped. This is why the built `graph.json` shows a
/// forward-referenced type as its real node, not a bare stub.
fn rewire_infile_stubs(nodes: Vec<Attrs>, edges: Vec<Attrs>) -> (Vec<Attrs>, Vec<Attrs>) {
    let norm = |label: &str| {
        label
            .trim_matches(|c| c == '(' || c == ')')
            .trim_start_matches('.')
            .to_string()
    };

    // normalized label → first real (sourced) node id.
    let mut label_to_real: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for n in &nodes {
        let sourced = n
            .get("source_file")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !sourced {
            continue;
        }
        if let (Some(id), Some(label)) = (
            n.get("id").and_then(Value::as_str),
            n.get("label").and_then(Value::as_str),
        ) {
            label_to_real
                .entry(norm(label))
                .or_insert_with(|| id.to_string());
        }
    }

    // stub id → real id (only stubs whose label matches a distinct real node).
    let mut remap: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for n in &nodes {
        let sourced = n
            .get("source_file")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if sourced {
            continue;
        }
        if let (Some(id), Some(label)) = (
            n.get("id").and_then(Value::as_str),
            n.get("label").and_then(Value::as_str),
        ) {
            if let Some(real) = label_to_real.get(&norm(label)) {
                if real != id {
                    remap.insert(id.to_string(), real.clone());
                }
            }
        }
    }
    if remap.is_empty() {
        return (nodes, edges);
    }

    let nodes: Vec<Attrs> = nodes
        .into_iter()
        .filter(|n| {
            n.get("id")
                .and_then(Value::as_str)
                .map(|id| !remap.contains_key(id))
                .unwrap_or(true)
        })
        .collect();
    let edges: Vec<Attrs> = edges
        .into_iter()
        .map(|mut e| {
            for key in ["source", "target"] {
                if let Some(v) = e.get(key).and_then(Value::as_str) {
                    if let Some(real) = remap.get(v) {
                        e.insert(key.to_string(), json!(real));
                    }
                }
            }
            e
        })
        .collect();
    (nodes, edges)
}

// ── build-time collapse (graphify build.py) ─────────────────────────────────

/// graphify `build.py::dedupe_nodes`: collapse nodes sharing an `id`,
/// last-writer-wins on attributes, first-appearance order.
pub fn dedupe_nodes(nodes: Vec<Attrs>) -> Vec<Attrs> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: std::collections::HashMap<String, Attrs> = std::collections::HashMap::new();
    for n in nodes {
        let Some(id) = n.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.to_string();
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, n);
    }
    order
        .into_iter()
        .map(|id| by_id.remove(&id).unwrap())
        .collect()
}

/// graphify `build.py::dedupe_edges`: collapse exact parallel edges by
/// `(source, target, relation)`, keeping the first occurrence.
pub fn dedupe_edges(edges: Vec<Attrs>) -> Vec<Attrs> {
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut out = Vec::with_capacity(edges.len());
    for e in edges {
        let key = (
            e.get("source")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            e.get("target")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            e.get("relation")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        if seen.insert(key) {
            out.push(e);
        }
    }
    out
}

// ── shared node / edge builders (graphify add_node / add_edge shape) ─────────

pub(crate) fn node_map(
    id: &str,
    label: &str,
    file_type: &str,
    source_file: &str,
    source_location: &str,
) -> Attrs {
    let mut m = Attrs::new();
    m.insert("id".into(), json!(id));
    m.insert("label".into(), json!(label));
    m.insert("file_type".into(), json!(file_type));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), json!(source_location));
    m
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn edge_map(
    src: &str,
    tgt: &str,
    relation: &str,
    context: Option<&str>,
    source_file: &str,
    source_location: &str,
) -> Attrs {
    let mut m = Attrs::new();
    m.insert("source".into(), json!(src));
    m.insert("target".into(), json!(tgt));
    m.insert("relation".into(), json!(relation));
    m.insert("confidence".into(), json!("EXTRACTED"));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), json!(source_location));
    m.insert("weight".into(), json!(1.0));
    if let Some(ctx) = context {
        m.insert("context".into(), json!(ctx));
    }
    m
}

/// Direct children of a node (named + anonymous), collected.
pub(crate) fn kids(n: tree_sitter::Node) -> Vec<tree_sitter::Node> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

/// graphify `_LANGUAGE_BUILTIN_GLOBALS` — names that would otherwise become
/// god-nodes as constructor/coercion call targets (JS + Python builtins).
pub(crate) fn is_builtin_global(s: &str) -> bool {
    const G: &[&str] = &[
        "String",
        "Number",
        "Boolean",
        "Object",
        "Array",
        "Symbol",
        "BigInt",
        "Date",
        "RegExp",
        "Error",
        "TypeError",
        "RangeError",
        "SyntaxError",
        "ReferenceError",
        "EvalError",
        "URIError",
        "Promise",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "JSON",
        "Math",
        "Reflect",
        "Proxy",
        "Intl",
        "parseInt",
        "parseFloat",
        "isNaN",
        "isFinite",
        "encodeURIComponent",
        "decodeURIComponent",
        "encodeURI",
        "decodeURI",
        "URL",
        "URLSearchParams",
        "FormData",
        "Blob",
        "File",
        "Headers",
        "Request",
        "Response",
        "AbortController",
        "AbortSignal",
        "TextEncoder",
        "TextDecoder",
        "console",
        "str",
        "int",
        "float",
        "bool",
        "list",
        "dict",
        "set",
        "tuple",
        "bytes",
        "len",
        "range",
        "enumerate",
        "zip",
        "map",
        "filter",
        "sum",
        "min",
        "max",
        "print",
        "open",
        "isinstance",
        "type",
        "super",
        "sorted",
        "reversed",
        "any",
        "all",
        "abs",
        "round",
        "next",
        "iter",
        "hash",
        "id",
        "repr",
        "callable",
        "getattr",
        "setattr",
        "hasattr",
        "delattr",
        "vars",
        "dir",
    ];
    G.contains(&s)
}

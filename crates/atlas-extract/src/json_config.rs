//! JSON config extractor — a Rust port of graphify's `extract_mcp_config`
//! (`graphify/mcp_ingest.py`) and `extract_json` (`graphify/extractors/
//! json_config.py`).
//!
//! `.json` is routed here, but graphify deliberately SKIPS plain data JSON
//! (datasets, GeoJSON, API dumps — #1224/#2107/#2108): a `.json` file produces
//! nodes ONLY when it is a recognized config/manifest, recognized by
//!   * an MCP config FILENAME (`.mcp.json`, `mcp.json`, `mcp_servers.json`,
//!     `claude_desktop_config.json`) → MCP servers/commands/packages/env vars, or
//!   * a config/manifest FILENAME (`package.json`, `tsconfig.json`, …) or a
//!     top-level key probe (`dependencies` / `extends` / `$ref` / `$schema` /
//!     `compilerOptions` …) → structural key nodes + dependency/extends edges.
//! Everything else returns an empty graph (the data-JSON skip rule).
//!
//! The file node keys off the file STEM (graphify's build relativizes its
//! `str(path)`-based raw id to exactly that stem form; every other atlas
//! extractor already keys the file node off the stem).
//!
//! Not ported: `sanitize_label` (labels here are already clean); the 1 MiB / 500-
//! pair / depth-6 caps ARE ported. MCP node `metadata.mcp_kind` is emitted by
//! graphify but ignored by the test's `canon`, so it is omitted here.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id, normalize_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::{Node, Parser};

const MCP_CONFIG_FILENAMES: &[&str] = &[
    ".mcp.json",
    "claude_desktop_config.json",
    "mcp.json",
    "mcp_servers.json",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // MCP configs are routed by filename before generic .json dispatch.
    if MCP_CONFIG_FILENAMES.contains(&name.as_str()) {
        return extract_mcp(path, source);
    }
    extract_json(path, source)
}

fn file_stem_id(path: &Path) -> String {
    make_id([file_stem(path).as_str()])
}

// ── MCP config (serde_json) ─────────────────────────────────────────────────

const MAX_BYTES: usize = 1_048_576;
const MAX_SERVERS: usize = 200;

/// MCP edge — carries `confidence_score` (mcp_ingest emits it), unlike the
/// shared `edge_map`.
fn mcp_edge(src: &str, tgt: &str, relation: &str, ctx: Option<&str>, source_file: &str) -> Attrs {
    let mut m = edge_map(src, tgt, relation, ctx, source_file, "L1");
    m.insert("confidence_score".into(), json!(1.0));
    m
}

fn extract_mcp(path: &Path, source: &[u8]) -> ExtractResult {
    let mut nodes: Vec<Attrs> = Vec::new();
    let mut edges: Vec<Attrs> = Vec::new();
    if source.len() > MAX_BYTES {
        return ExtractResult { nodes, edges };
    }
    let doc: Value = match serde_json::from_slice(source) {
        Ok(v) => v,
        Err(_) => return ExtractResult { nodes, edges },
    };
    let Some(obj) = doc.as_object() else {
        return ExtractResult { nodes, edges };
    };
    // `mcpServers`, or the nested `{"mcp": {"servers": {...}}}` shape.
    let servers = obj
        .get("mcpServers")
        .and_then(Value::as_object)
        .or_else(|| {
            obj.get("mcp")
                .and_then(Value::as_object)
                .and_then(|m| m.get("servers"))
                .and_then(Value::as_object)
        });
    let Some(servers) = servers else {
        return ExtractResult { nodes, edges };
    };

    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);
    let file_nid = file_stem_id(path);
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();

    let add_node = |nid: &str, label: &str, nodes: &mut Vec<Attrs>, seen: &mut HashSet<String>| {
        if !nid.is_empty() && seen.insert(nid.to_string()) {
            nodes.push(node_map(nid, label, "code", &str_path, "L1"));
        }
    };

    add_node(&file_nid, &label, &mut nodes, &mut seen);

    for (i, (server_name, spec)) in servers.iter().enumerate() {
        if i >= MAX_SERVERS {
            break;
        }
        if server_name.is_empty() {
            continue;
        }
        let Some(spec) = spec.as_object() else {
            continue;
        };
        let server_nid = make_id([stem.as_str(), "mcp_server", server_name.as_str()]);
        add_node(&server_nid, server_name, &mut nodes, &mut seen);
        edges.push(mcp_edge(
            &file_nid,
            &server_nid,
            "contains",
            None,
            &str_path,
        ));

        if let Some(cmd) = spec.get("command").and_then(Value::as_str) {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                let cmd_nid = make_id(["mcp_command", cmd]);
                add_node(&cmd_nid, cmd, &mut nodes, &mut seen);
                edges.push(mcp_edge(
                    &server_nid,
                    &cmd_nid,
                    "references",
                    Some("command"),
                    &str_path,
                ));
            }
        }
        if let Some(args) = spec.get("args").and_then(Value::as_array) {
            if let Some(pkg) = detect_package(args) {
                let pkg_nid = make_id(["mcp_package", pkg.as_str()]);
                add_node(&pkg_nid, &pkg, &mut nodes, &mut seen);
                edges.push(mcp_edge(
                    &server_nid,
                    &pkg_nid,
                    "references",
                    Some("package"),
                    &str_path,
                ));
            }
        }
        if let Some(env) = spec.get("env").and_then(Value::as_object) {
            // ONLY keys — env values may hold secrets and are never read.
            for env_name in env.keys() {
                if env_name.is_empty() {
                    continue;
                }
                let env_nid = make_id(["env_var", env_name.as_str()]);
                add_node(&env_nid, env_name, &mut nodes, &mut seen);
                edges.push(mcp_edge(
                    &server_nid,
                    &env_nid,
                    "requires_env",
                    None,
                    &str_path,
                ));
            }
        }
    }

    ExtractResult { nodes, edges }
}

fn npm_pkg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^@[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*(?:@[\w.\-+]+)?$").unwrap()
    })
}
fn py_pkg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[a-z0-9][a-z0-9._-]*-mcp(?:-[a-z0-9._-]+)?$|^mcp-[a-z0-9][a-z0-9._-]*$")
            .unwrap()
    })
}
fn flag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^-{1,2}\w").unwrap())
}

fn detect_package(args: &[Value]) -> Option<String> {
    for raw in args {
        let Some(arg) = raw.as_str() else { continue };
        let arg = arg.trim();
        if arg.is_empty() || flag_re().is_match(arg) {
            continue;
        }
        if npm_pkg_re().is_match(arg) {
            return Some(strip_version(arg));
        }
        if py_pkg_re().is_match(arg) {
            return Some(arg.to_string());
        }
    }
    None
}

fn strip_version(pkg: &str) -> String {
    if let Some(rest) = pkg.strip_prefix('@') {
        match rest.find('@') {
            Some(at) => pkg[..at + 1].to_string(),
            None => pkg.to_string(),
        }
    } else {
        match pkg.find('@') {
            Some(at) => pkg[..at].to_string(),
            None => pkg.to_string(),
        }
    }
}

// ── config/manifest JSON (tree-sitter-json) ─────────────────────────────────

const CONFIG_JSON_NAMES: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "jsconfig.json",
    "composer.json",
    "deno.json",
    "deno.jsonc",
    "bower.json",
    "manifest.json",
    "app.json",
    "now.json",
    "vercel.json",
    "angular.json",
    "nest-cli.json",
    "biome.json",
    "biome.jsonc",
    "renovate.json",
    ".babelrc",
    ".babelrc.json",
    ".eslintrc.json",
    ".prettierrc.json",
    ".prettierrc",
    "babel.config.json",
];
const CONFIG_JSON_KEYS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
    "bundleDependencies",
    "bundledDependencies",
    "extends",
    "$ref",
    "$schema",
    "compilerOptions",
];
const DEP_KEYS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
    "bundleDependencies",
    "bundledDependencies",
];
const CONFIG_NAME_SUFFIXES: &[&str] = &[
    ".eslintrc.json",
    ".prettierrc.json",
    ".babelrc.json",
    "tsconfig.json",
    "jsconfig.json",
];

fn extract_json(path: &Path, source: &[u8]) -> ExtractResult {
    let empty = ExtractResult {
        nodes: vec![],
        edges: vec![],
    };
    if source.len() > MAX_BYTES {
        return empty;
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_json::LANGUAGE.into())
        .is_err()
    {
        return empty;
    }
    let Some(tree) = parser.parse(source, None) else {
        return empty;
    };
    let root = tree.root_node();
    // document → first child value.
    let doc = crate::kids(root).into_iter().next().unwrap_or(root);
    if doc.kind() != "object" {
        return empty; // top-level array/scalar => data JSON
    }

    let mut ex = J {
        source,
        str_path: path.to_string_lossy().into_owned(),
        stem: file_stem(path),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
        pair_count: 0,
    };
    if !ex.is_config_json(path, doc) {
        return empty; // data JSON — deliberately skipped
    }

    let file_nid = file_stem_id(path);
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    ex.add_node(&file_nid, &label, 1, "code");
    ex.walk_object(doc, &file_nid, None, 0);

    ExtractResult {
        nodes: ex.nodes,
        edges: ex.edges,
    }
}

struct J<'a> {
    source: &'a [u8],
    str_path: String,
    stem: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
    pair_count: usize,
}

impl<'a> J<'a> {
    fn read(&self, n: Node) -> String {
        String::from_utf8_lossy(&self.source[n.byte_range()]).into_owned()
    }

    fn add_node(&mut self, nid: &str, label: &str, line: usize, file_type: &str) {
        if !nid.is_empty() && self.seen.insert(nid.to_string()) {
            self.nodes.push(node_map(
                nid,
                label,
                file_type,
                &self.str_path,
                &format!("L{line}"),
            ));
        }
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, line: usize, ctx: Option<&str>) {
        if src.is_empty() || tgt.is_empty() || src == tgt {
            return;
        }
        self.edges.push(edge_map(
            src,
            tgt,
            relation,
            ctx,
            &self.str_path,
            &format!("L{line}"),
        ));
    }

    /// Text of a `string` node's content (string_content child), else the raw
    /// text with surrounding quotes stripped.
    fn string_text(&self, n: Node) -> String {
        if let Some(c) = n.child_by_field_name("string_content") {
            self.read(c)
        } else {
            self.read(n)
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        }
    }

    fn key_text(&self, pair: Node) -> Option<String> {
        let key = pair.child_by_field_name("key")?;
        if key.kind() == "string" {
            Some(self.string_text(key))
        } else {
            Some(self.read(key))
        }
    }

    fn is_config_json(&self, path: &Path, obj: Node) -> bool {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if CONFIG_JSON_NAMES.contains(&name.as_str()) {
            return true;
        }
        if CONFIG_NAME_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            return true;
        }
        for child in crate::kids(obj) {
            if child.kind() != "pair" {
                continue;
            }
            if let Some(k) = self.key_text(child) {
                if CONFIG_JSON_KEYS.contains(&k.as_str()) {
                    return true;
                }
            }
        }
        false
    }

    fn walk_object(&mut self, obj: Node, parent_nid: &str, parent_key: Option<&str>, depth: usize) {
        if depth > 6 {
            return;
        }
        for child in crate::kids(obj) {
            if child.kind() != "pair" {
                continue;
            }
            if self.pair_count >= 500 {
                return;
            }
            self.pair_count += 1;
            let Some(key) = self.key_text(child) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            // A key that normalizes to nothing carries no signal (#1899).
            if normalize_id(&key).is_empty() {
                continue;
            }
            let mut parts: Vec<&str> = vec![self.stem.as_str()];
            if let Some(pk) = parent_key {
                parts.push(pk);
            }
            parts.push(key.as_str());
            let key_nid = make_id(parts);
            if key_nid.is_empty() {
                continue;
            }
            let line = child.start_position().row + 1;
            self.add_node(&key_nid, &key, line, "code");
            self.add_edge(parent_nid, &key_nid, "contains", line, None);

            let Some(val) = child.child_by_field_name("value") else {
                continue;
            };
            match val.kind() {
                "object" => self.walk_object(val, &key_nid, Some(&key), depth + 1),
                "array" => {
                    for item in crate::kids(val) {
                        if item.kind() == "string" {
                            let refv = self.string_text(item);
                            if !refv.is_empty() {
                                let ref_nid = make_id(["ref", refv.as_str()]);
                                if !ref_nid.is_empty() {
                                    self.add_node(&ref_nid, &refv, line, "concept");
                                    self.add_edge(
                                        &key_nid,
                                        &ref_nid,
                                        "extends",
                                        line,
                                        Some("import"),
                                    );
                                }
                            }
                        }
                    }
                }
                "string" => {
                    let val_text = self.string_text(val);
                    if key == "extends" && !val_text.is_empty() {
                        let ref_nid = make_id(["ref", val_text.as_str()]);
                        if !ref_nid.is_empty() {
                            // `extends` string ref anchors to the FILE node.
                            let file_nid = make_id([self.stem.as_str()]);
                            self.add_node(&ref_nid, &val_text, line, "concept");
                            self.add_edge(&file_nid, &ref_nid, "extends", line, Some("import"));
                        }
                    } else if key == "$ref" && !val_text.is_empty() {
                        let ref_nid = make_id(["ref", val_text.as_str()]);
                        if !ref_nid.is_empty() {
                            // $ref emits only an edge (target stays a stub, no node).
                            self.add_edge(parent_nid, &ref_nid, "references", line, None);
                        }
                    } else if parent_key.map(|pk| DEP_KEYS.contains(&pk)).unwrap_or(false)
                        && !val_text.is_empty()
                    {
                        let dep_nid = make_id([key.as_str()]);
                        if !dep_nid.is_empty() {
                            self.add_node(&dep_nid, &key, line, "concept");
                            self.add_edge(&key_nid, &dep_nid, "imports", line, Some("import"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

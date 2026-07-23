//! Shared import regex-rescue for single-file-component langs (Vue/Svelte/Astro).
//!
//! graphify parses only the embedded `<script>`/frontmatter as JS/TS; the
//! template/markup layer is invisible to that parser, and feeding the whole
//! file to the JS grammar yields a top-level ERROR node so `import_statement`
//! nodes are frequently unreachable. A regex pass recovers static (`import … from
//! '…'`) and dynamic (`import('…')`) specifiers the AST cannot edge. Ports the
//! regex rescues in graphify `extract_svelte`/`extract_astro`/`extract_vue`
//! (extract.py:1269-1600).
//!
//! NOT ported (documented gap): tsconfig `paths` alias resolution
//! (`$lib/…`, `@/…`) and workspace-package resolution. The fixtures carry no
//! tsconfig, so graphify's alias map is empty there and the outputs coincide.

use crate::engine::{normalize_path, resolve_js_import_path};
use atlas_core::ids::make_id;
use atlas_core::Attrs;
use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// `import('…')` — dynamic import specifier.
fn dynamic_import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"import\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap())
}

/// `import … from '…'` / `import '…'` — static import specifier.
fn static_import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"import\s+(?:[^'"`;]+?\s+from\s+)?['"]([^'"]+)['"]"#).unwrap())
}

/// `<script …>BODY</script>` — a client-side script block body (group 1).
fn script_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?is)<script\b[^>]*>([\s\S]*?)</script\s*>"#).unwrap())
}

/// Astro `---\n…\n---` frontmatter body (group 1), anchored at file head.
fn frontmatter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\A\s*---\s*\r?\n([\s\S]*?)\r?\n---\s*(?:\r?\n|\z)"#).unwrap())
}

/// Resolve a specifier to `(node_id, stub_source_file)`, mirroring the shared
/// resolver body in graphify's regex rescues. `dynamic` selects the dynamic-import
/// resolver (full on-disk probe) vs the static one (only `.js→.ts`/`.jsx→.tsx`).
fn resolve(raw: &str, path: &Path, dynamic: bool) -> Option<(String, String)> {
    if raw.starts_with('.') {
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let joined = normalize_path(&parent.join(raw));
        let resolved = if dynamic {
            resolve_js_import_path(&joined)
        } else {
            match joined.extension().and_then(|s| s.to_str()) {
                Some("js") => joined.with_extension("ts"),
                Some("jsx") => joined.with_extension("tsx"),
                _ => joined,
            }
        };
        let s = resolved.to_string_lossy().into_owned();
        Some((make_id([s.as_str()]), s))
    } else {
        // Bare/scoped (node_modules): last path segment. tsconfig-alias
        // resolution is not ported (see module docs).
        let module_name = raw.rsplit('/').next().unwrap_or("");
        if module_name.is_empty() {
            return None;
        }
        Some((make_id([module_name]), raw.to_string()))
    }
}

/// Running state for a rescue pass over one SFC file.
pub(crate) struct Rescue<'a> {
    path: &'a Path,
    str_path: String,
    file_nid: &'a str,
    existing_ids: HashSet<String>,
    pub nodes: Vec<Attrs>,
    pub edges: Vec<Attrs>,
}

impl<'a> Rescue<'a> {
    /// Seed from the AST pass so a specifier whose target is already a node only
    /// adds an edge (graphify's `existing_ids` guard).
    pub(crate) fn new(path: &'a Path, file_nid: &'a str, ast_nodes: &[Attrs]) -> Self {
        let existing_ids = ast_nodes
            .iter()
            .filter_map(|n| n.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        Rescue {
            path,
            str_path: path.to_string_lossy().into_owned(),
            file_nid,
            existing_ids,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    fn scan(&mut self, region: &str, re: &Regex, relation: &str, dynamic: bool) {
        for cap in re.captures_iter(region) {
            let raw = &cap[1];
            let Some((node_id, stub_source_file)) = resolve(raw, self.path, dynamic) else {
                continue;
            };
            if !self.existing_ids.contains(&node_id) {
                let mut n = Attrs::new();
                n.insert("id".into(), json!(node_id));
                n.insert("label".into(), json!(raw));
                n.insert("file_type".into(), json!("code"));
                n.insert("source_file".into(), json!(stub_source_file));
                n.insert("confidence".into(), json!("EXTRACTED"));
                self.nodes.push(n);
                self.existing_ids.insert(node_id.clone());
            }
            let mut e = Attrs::new();
            e.insert("source".into(), json!(self.file_nid));
            e.insert("target".into(), json!(node_id));
            e.insert("relation".into(), json!(relation));
            e.insert("confidence".into(), json!("EXTRACTED"));
            e.insert("source_file".into(), json!(self.str_path));
            self.edges.push(e);
        }
    }

    /// `import('…')` anywhere in the source → `dynamic_import` edges.
    pub(crate) fn dynamic_imports(&mut self, src: &str) {
        self.scan(src, dynamic_import_re(), "dynamic_import", true);
    }

    /// Static `import … from '…'` inside every `<script>` body → `imports_from`.
    pub(crate) fn static_imports_in_scripts(&mut self, src: &str) {
        let re = script_block_re();
        // Collect first to avoid borrowing `re`'s captures across `&mut self`.
        let bodies: Vec<String> = re.captures_iter(src).map(|c| c[1].to_string()).collect();
        for body in bodies {
            self.scan(&body, static_import_re(), "imports_from", false);
        }
    }

    /// Static imports inside the Astro `---` frontmatter block (if present).
    pub(crate) fn static_imports_in_frontmatter(&mut self, src: &str) {
        if let Some(cap) = frontmatter_re().captures(src) {
            let body = cap[1].to_string();
            self.scan(&body, static_import_re(), "imports_from", false);
        }
    }
}

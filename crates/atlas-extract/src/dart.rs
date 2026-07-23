//! Dart extractor — a Rust port of graphify `graphify/extractors/dart.py`.
//!
//! graphify's Dart extractor is REGEX-based (no tree-sitter), so matching its
//! oracle means reproducing the same regex passes, not an AST walk. Ported here:
//! comment/string stripping, classes/mixins/enums (`defines`) with
//! extends/on → `inherits`, `with` → `mixes_in`, `implements` → `implements`,
//! extends-generics → `references`; extensions (`defines` + `extends`);
//! top-level/member vars (`defines` + variable-type `references`); top-level/
//! member methods (`defines`); and `import`/`export` (`imports`/`exports`).
//!
//! Bare-name reference targets (a base class, mixin, interface) are emitted as
//! SOURCELESS stubs keyed off `make_id(name)`; the shared in-file rewire in
//! `lib.rs` collapses them onto the real stem-keyed definition, exactly as
//! graphify's corpus build does (why `sample_circle -> sample_shape inherits`
//! appears, not a bare `shape` stub).
//!
//! DELTA (documented, not silently dropped): the Flutter-plugin heuristics —
//! Bloc event/state scans, Riverpod `ref.watch` provider refs, GoRouter/
//! Navigator route detection inside class/function bodies, `@annotation`
//! `configures` edges + Riverpod provider generation, and the generic-call
//! `word<Type>(` type-lookup pass — are NOT ported. They fire only on Flutter
//! idioms; this plain-Dart fixture exercises none, so output is byte-identical
//! to the oracle here. Port those passes if a fixture adds Flutter/Bloc code.

use crate::ExtractResult;
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

const TYPE_BLACKLIST: &[&str] = &[
    "String", "int", "double", "bool", "num", "dynamic", "Object", "List", "Map", "Set", "void",
    "Function",
];
const NAME_KEYWORDS: &[&str] = &["if", "for", "while", "switch", "catch", "return"];
const METHOD_SKIP: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "void", "dynamic", "final", "const", "get",
    "set",
];

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source).into_owned();
    let stem = file_stem(path);
    let str_path = path.to_string_lossy().into_owned();
    let file_nid = make_id([stem.as_str()]);
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Strip comments, preserve string literals (graphify comment_string_pattern).
    let strip_re = Regex::new(
        r#"(?s)"""(?:\\.|.)*?"""|'''(?:\\.|.)*?'''|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|/\*.*?\*/|//[^\n]*"#,
    )
    .unwrap();
    let src_clean = strip_re.replace_all(&src, |caps: &regex::Captures| {
        let tok = caps.get(0).unwrap().as_str();
        if tok.starts_with('/') {
            String::new()
        } else {
            tok.to_string()
        }
    });
    let src_clean = src_clean.as_ref();

    let mut d = Dart {
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        defined: HashSet::new(),
    };
    d.add_node(&file_nid, &filename, "code", Some(&d.str_path.clone()));

    d.pass_classes(src_clean);
    d.pass_extensions(src_clean);
    d.pass_vars(src_clean);
    d.pass_methods(src_clean);
    d.pass_imports_exports(src_clean);

    ExtractResult {
        nodes: d.nodes,
        edges: d.edges,
    }
}

struct Dart {
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    defined: HashSet<String>,
}

impl Dart {
    fn add_node(&mut self, nid: &str, label: &str, ftype: &str, source_file: Option<&str>) {
        if !self.defined.insert(nid.to_string()) {
            return;
        }
        let mut m = Attrs::new();
        m.insert("id".into(), json!(nid));
        m.insert("label".into(), json!(label));
        m.insert("file_type".into(), json!(ftype));
        m.insert(
            "source_file".into(),
            source_file.map(|s| json!(s)).unwrap_or(Value::Null),
        );
        m.insert("source_location".into(), Value::Null);
        self.nodes.push(m);
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, ctx: Option<&str>) {
        let mut m = Attrs::new();
        m.insert("source".into(), json!(src));
        m.insert("target".into(), json!(tgt));
        m.insert("relation".into(), json!(relation));
        m.insert("confidence".into(), json!("EXTRACTED"));
        m.insert("confidence_score".into(), json!(1.0));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), Value::Null);
        m.insert("weight".into(), json!(1.0));
        if let Some(c) = ctx {
            m.insert("context".into(), json!(c));
        }
        self.edges.push(m);
    }

    // ── classes / mixins / enums + inheritance header ───────────────────────
    fn pass_classes(&mut self, src: &str) {
        let class_re = Regex::new(
            r"(?m)^\s*(?:(?:abstract|sealed|base|interface|final|mixin)\s+)*(?:class|mixin|enum|extension\s+type)\s+(\w+)",
        )
        .unwrap();
        let extends_re = Regex::new(r"^\s*(?:extends|on)\s+([a-zA-Z0-9_.]+)").unwrap();
        let with_re = Regex::new(r"^\s*with\s+").unwrap();
        let impl_re = Regex::new(r"^\s*implements\s+").unwrap();

        for m in class_re.captures_iter(src) {
            let class_name = m.get(1).unwrap().as_str().to_string();
            let class_nid = make_id([self.stem.as_str(), class_name.as_str()]);
            self.add_node(
                &class_nid,
                &class_name,
                "code",
                Some(&self.str_path.clone()),
            );
            let fnid = self.file_nid.clone();
            self.add_edge(&fnid, &class_nid, "defines", None);

            let start_idx = m.get(0).unwrap().end();
            let mut rest = slice(src, start_idx, start_idx + 500);

            // Skip class generic parameters <...>
            if rest.trim_start().starts_with('<') {
                rest = after_balanced(&rest, '<', '>');
            }
            // Skip primary constructor (...)
            if rest.trim_start().starts_with('(') {
                rest = after_balanced(&rest, '(', ')');
            }

            let header_end = rest
                .find('{')
                .or_else(|| rest.find(';'))
                .unwrap_or(rest.len());
            let mut header = rest[..header_end].to_string();

            let mut base_class: Option<String> = None;
            let mut generics: Option<String> = None;
            let mut mixins: Vec<String> = Vec::new();
            let mut interfaces: Vec<String> = Vec::new();

            if let Some(em) = extends_re.captures(&header) {
                base_class = Some(em.get(1).unwrap().as_str().to_string());
                let rest_header = header[em.get(0).unwrap().end()..].to_string();
                if rest_header.trim_start().starts_with('<') {
                    // capture generics between the first balanced <...>
                    if let Some((inner, after)) = balanced_capture(&rest_header, '<', '>') {
                        generics = Some(inner);
                        header = after;
                    } else {
                        header = rest_header;
                    }
                } else {
                    header = rest_header;
                }
            }

            if let Some(wm) = with_re.find(&header) {
                let rest_header = header[wm.end()..].to_string();
                if let Some(impl_idx) = rest_header.find("implements") {
                    let mixins_str = &rest_header[..impl_idx];
                    header = rest_header[impl_idx..].to_string();
                    mixins = split_types(mixins_str);
                } else {
                    mixins = split_types(&rest_header);
                    header = String::new();
                }
            }

            if let Some(im) = impl_re.find(&header) {
                interfaces = split_types(&header[im.end()..]);
            }

            if let Some(base) = base_class {
                let base_nid = make_id([base.as_str()]);
                self.add_node(&base_nid, &base, "code", None);
                self.add_edge(&class_nid, &base_nid, "inherits", None);
                if let Some(gen) = generics {
                    for g in split_types(&gen) {
                        let gc = g.split('<').next().unwrap_or("").trim().to_string();
                        if !TYPE_BLACKLIST.contains(&gc.as_str()) {
                            let gnid = make_id([gc.as_str()]);
                            self.add_node(&gnid, &gc, "code", None);
                            self.add_edge(&class_nid, &gnid, "references", None);
                        }
                    }
                }
            }
            for mixin in mixins {
                let mc = mixin.split('<').next().unwrap_or("").trim().to_string();
                let nid = make_id([mc.as_str()]);
                self.add_node(&nid, &mc, "code", None);
                self.add_edge(&class_nid, &nid, "mixes_in", None);
            }
            for iface in interfaces {
                let ic = iface.split('<').next().unwrap_or("").trim().to_string();
                let nid = make_id([ic.as_str()]);
                self.add_node(&nid, &ic, "code", None);
                self.add_edge(&class_nid, &nid, "implements", None);
            }
        }
    }

    fn pass_extensions(&mut self, src: &str) {
        let ext_re =
            Regex::new(r"(?m)^\s{0,4}extension\s+(\w+)?(?:<[^>]+>)?\s+on\s+(\w+)").unwrap();
        for m in ext_re.captures_iter(src) {
            let target_class = m.get(2).unwrap().as_str().to_string();
            let ext_name = m
                .get(1)
                .map(|x| x.as_str().to_string())
                .unwrap_or_else(|| format!("{}_anonymous_extension", self.stem));
            let ext_nid = make_id([self.stem.as_str(), ext_name.as_str()]);
            let label = m
                .get(1)
                .map(|x| x.as_str().to_string())
                .unwrap_or_else(|| format!("Extension on {target_class}"));
            self.add_node(&ext_nid, &label, "code", Some(&self.str_path.clone()));
            let fnid = self.file_nid.clone();
            self.add_edge(&fnid, &ext_nid, "defines", None);
            let tgt = make_id([target_class.as_str()]);
            self.add_node(&tgt, &target_class, "code", None);
            self.add_edge(&ext_nid, &tgt, "extends", None);
        }
    }

    fn pass_vars(&mut self, src: &str) {
        let var_re = Regex::new(
            r"(?m)^\s{0,2}(?:late\s+)?(?:(?:final|const|var)\s+)?(?:\([^)]+\)\s+|([a-zA-Z0-9_<>,.?]+(?:\s+[a-zA-Z0-9_<>,.?]+){0,3})\s+)?(?:(\w+)|(?:\w+\s*)?\(([^)]+)\))\s*(?:=|$|;)",
        )
        .unwrap();
        let lead_re = Regex::new(r"^\s*(?:late|final|const|var)\b").unwrap();
        for m in var_re.captures_iter(src) {
            let whole = m.get(0).unwrap().as_str();
            let var_type = m.get(1).map(|x| x.as_str().to_string());
            let single = m.get(2).map(|x| x.as_str().to_string());
            let destructured = m.get(3).map(|x| x.as_str().to_string());

            if !lead_re.is_match(whole) && var_type.is_none() {
                continue;
            }

            if let Some(name) = single {
                if !NAME_KEYWORDS.contains(&name.as_str()) {
                    let var_nid = make_id([self.stem.as_str(), name.as_str()]);
                    self.add_node(&var_nid, &name, "code", Some(&self.str_path.clone()));
                    let fnid = self.file_nid.clone();
                    self.add_edge(&fnid, &var_nid, "defines", None);
                    if let Some(vt) = &var_type {
                        let mut extra = TYPE_BLACKLIST.to_vec();
                        // graphify's var-type blacklist excludes "Function"
                        extra.retain(|s| *s != "Function");
                        if !extra.contains(&vt.as_str()) {
                            let clean = vt
                                .split('<')
                                .next()
                                .unwrap_or("")
                                .split('.')
                                .next_back()
                                .unwrap_or("")
                                .trim()
                                .to_string();
                            let tnid = make_id([clean.as_str()]);
                            self.add_node(&tnid, &clean, "code", None);
                            self.add_edge(&fnid, &tnid, "references", Some("variable_type"));
                        }
                    }
                }
            } else if let Some(dnames) = destructured {
                for raw in dnames.split(',') {
                    let mut name = raw.trim().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    if name.contains(':') {
                        name = name.rsplit(':').next().unwrap().trim().to_string();
                    }
                    if is_ident_lower(&name) && !NAME_KEYWORDS.contains(&name.as_str()) {
                        let var_nid = make_id([self.stem.as_str(), name.as_str()]);
                        self.add_node(&var_nid, &name, "code", Some(&self.str_path.clone()));
                        let fnid = self.file_nid.clone();
                        self.add_edge(&fnid, &var_nid, "defines", None);
                    }
                }
            }
        }
    }

    fn pass_methods(&mut self, src: &str) {
        let method_re = Regex::new(
            r"(?m)^\s{0,2}(?:factory\s+|static\s+|async\s+|external\s+|abstract\s+)?(?:\([^)]+\)|[a-zA-Z0-9_<>,.?]+)(?:\s+[a-zA-Z0-9_<>,.?]+){0,3}\s+(\w+(?:\.\w+)?)\s*\(",
        )
        .unwrap();
        for m in method_re.captures_iter(src) {
            let raw = m.get(1).unwrap().as_str();
            let name = raw.rsplit('.').next().unwrap().to_string();
            if METHOD_SKIP.contains(&name.as_str()) {
                continue;
            }
            if name.chars().next().map(|c| c.is_ascii_uppercase()) == Some(true) {
                continue;
            }
            let nid = make_id([self.stem.as_str(), name.as_str()]);
            self.add_node(&nid, &name, "code", Some(&self.str_path.clone()));
            let fnid = self.file_nid.clone();
            self.add_edge(&fnid, &nid, "defines", None);
        }
    }

    fn pass_imports_exports(&mut self, src: &str) {
        let import_re = Regex::new(r#"(?m)^\s*import\s+['"]([^'"]+)['"]"#).unwrap();
        let export_re = Regex::new(r#"(?m)^\s*export\s+['"]([^'"]+)['"]"#).unwrap();
        for m in import_re.captures_iter(src) {
            let pkg = m.get(1).unwrap().as_str().to_string();
            let tgt = make_id([pkg.as_str()]);
            self.add_node(&tgt, &pkg, "code", None);
            let fnid = self.file_nid.clone();
            self.add_edge(&fnid, &tgt, "imports", None);
        }
        for m in export_re.captures_iter(src) {
            let pkg = m.get(1).unwrap().as_str().to_string();
            let tgt = make_id([pkg.as_str()]);
            self.add_node(&tgt, &pkg, "code", None);
            let fnid = self.file_nid.clone();
            self.add_edge(&fnid, &tgt, "exports", None);
        }
    }
}

fn slice(s: &str, start: usize, end: usize) -> String {
    let end = end.min(s.len());
    if start >= end {
        return String::new();
    }
    s.get(start..end).unwrap_or("").to_string()
}

fn is_ident_lower(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !s.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return false;
    }
    // not starting with uppercase (graphify `not re.match(r"^[A-Z]", name)`)
    !s.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

/// graphify `_split_types`: depth-aware (`<...>`) comma split, trimmed, non-empty.
fn split_types(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for ch in text.chars() {
        match ch {
            '<' => {
                depth += 1;
                cur.push(ch);
            }
            '>' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                let t = cur.trim().to_string();
                if !t.is_empty() {
                    parts.push(t);
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() {
        parts.push(t);
    }
    parts
}

/// Return the substring after the first balanced `open..close` run (starting at
/// the first `open`), mirroring graphify's inline generic/paren skip loops.
fn after_balanced(s: &str, open: char, close: char) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let Some(off) = bytes.iter().position(|&c| c == open) else {
        return s.to_string();
    };
    let mut depth = 1i32;
    let mut i = off + 1;
    while i < bytes.len() && depth > 0 {
        if bytes[i] == open {
            depth += 1;
        } else if bytes[i] == close {
            depth -= 1;
        }
        i += 1;
    }
    bytes[i..].iter().collect()
}

/// graphify extends-generics loop: capture the inner of the first balanced
/// `open..close` and return (inner, remainder-after-close).
fn balanced_capture(s: &str, open: char, close: char) -> Option<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.iter().position(|&c| c == open)?;
    let mut depth = 1i32;
    let mut i = start + 1;
    while i < chars.len() && depth > 0 {
        if chars[i] == open {
            depth += 1;
        } else if chars[i] == close {
            depth -= 1;
            if depth == 0 {
                let inner: String = chars[start + 1..i].iter().collect();
                let after: String = chars[i + 1..].iter().collect();
                return Some((inner, after));
            }
        }
        i += 1;
    }
    None
}

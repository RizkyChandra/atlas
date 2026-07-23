//! Laravel Blade extractor — a Rust port of graphify `graphify/extractors/blade.py`
//! (pure regex, no grammar). Handles the compound `.blade.php` extension.
//!
//! Edges (all with `confidence_score: 1.0` and null source_location, matching the
//! extractor): `@include('a.b')` → `includes` (target id from `a/b`, label kept
//! dotted), `<livewire:name>` → `uses_component`, `wire:click="m"` →
//! `binds_method`. The file node keys off the stem (FILE); targets are global
//! `make_id` stubs (no file prefix), so cross-file resolution reconciles them.

use crate::ExtractResult;
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source).into_owned();
    let str_path = path.to_string_lossy().into_owned();
    let file_nid = make_id([file_stem(path).as_str()]);
    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut nodes: Vec<Attrs> = vec![node_null_loc(&file_nid, &file_label, &str_path)];
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([file_nid.clone()]);

    let mut emit = |label: &str, tgt_nid: String, relation: &str, nodes: &mut Vec<Attrs>| {
        if seen.insert(tgt_nid.clone()) {
            nodes.push(node_null_loc(&tgt_nid, label, &str_path));
        }
        edges.push(edge_null_loc(&file_nid, &tgt_nid, relation, &str_path));
    };

    // @include('path.to.partial')
    let include_re = Regex::new(r#"@include\(['"]([^'"]+)['"]"#).unwrap();
    for c in include_re.captures_iter(&src) {
        let label = &c[1];
        let nid = make_id([label.replace('.', "/").as_str()]);
        emit(label, nid, "includes", &mut nodes);
    }
    // <livewire:component.name>
    let livewire_re = Regex::new(r"<livewire:([\w.\-]+)").unwrap();
    for c in livewire_re.captures_iter(&src) {
        let label = &c[1];
        let nid = make_id([label]);
        emit(label, nid, "uses_component", &mut nodes);
    }
    // wire:click="methodName"
    let wire_re = Regex::new(r#"wire:click=["']([^"']+)["']"#).unwrap();
    for c in wire_re.captures_iter(&src) {
        let label = &c[1];
        let nid = make_id([label]);
        emit(label, nid, "binds_method", &mut nodes);
    }

    ExtractResult { nodes, edges }
}

fn node_null_loc(id: &str, label: &str, source_file: &str) -> Attrs {
    let mut m = Attrs::new();
    m.insert("id".into(), json!(id));
    m.insert("label".into(), json!(label));
    m.insert("file_type".into(), json!("code"));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), Value::Null);
    m
}

fn edge_null_loc(src: &str, tgt: &str, relation: &str, source_file: &str) -> Attrs {
    let mut m = Attrs::new();
    m.insert("source".into(), json!(src));
    m.insert("target".into(), json!(tgt));
    m.insert("relation".into(), json!(relation));
    m.insert("confidence".into(), json!("EXTRACTED"));
    m.insert("confidence_score".into(), json!(1.0));
    m.insert("source_file".into(), json!(source_file));
    m.insert("source_location".into(), Value::Null);
    m.insert("weight".into(), json!(1.0));
    m
}

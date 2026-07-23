//! Svelte single-file components (`.svelte`). Port of graphify `extract_svelte`
//! (extract.py:1269).
//!
//! Unlike Vue, the raw file is fed straight to the JS grammar (no masking): the
//! HTML markup makes the whole-file parse a top-level ERROR node, so the AST
//! yields little beyond the file node and `import_statement`s are unreachable
//! (graphify #713). A regex pass recovers static imports inside `<script>`
//! blocks and dynamic `import('…')` from the template markup.

use crate::engine::{self, Lang};
use crate::sfc::Rescue;
use crate::ExtractResult;
use atlas_core::ids::{file_stem, make_id};
use std::path::Path;

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source);
    let mut result = engine::extract(path, source, Lang::Js);

    let file_nid = make_id([file_stem(path).as_str()]);
    let mut r = Rescue::new(path, &file_nid, &result.nodes);
    r.dynamic_imports(&src);
    r.static_imports_in_scripts(&src);

    result.nodes.extend(r.nodes);
    result.edges.extend(r.edges);
    result
}

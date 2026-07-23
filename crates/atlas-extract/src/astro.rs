//! Astro components (`.astro`). Port of graphify `extract_astro`
//! (extract.py:1394).
//!
//! An Astro file is a `---\n…\n---` TypeScript frontmatter block followed by an
//! HTML-with-expressions template and optional client `<script>` blocks. Fed to
//! the JS grammar the whole file is a top-level ERROR, but tree-sitter recovers
//! per-statement, so most frontmatter imports/symbols DO come through the AST;
//! the ones adjacent to the `---` fences (and template/script imports) do not
//! (graphify #850). A regex pass recovers static imports from the frontmatter
//! and `<script>` blocks plus dynamic `import('…')` anywhere.

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
    r.static_imports_in_frontmatter(&src);
    r.static_imports_in_scripts(&src);

    result.nodes.extend(r.nodes);
    result.edges.extend(r.edges);
    result
}

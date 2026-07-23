//! Vue single-file components (`.vue`). Port of graphify `extract_vue`
//! (extract.py:1525) + `_vue_mask_non_script` (extractors/resolution.py:537).
//!
//! Everything outside a `<script>` body is blanked to spaces (newlines kept, so
//! line numbers stay accurate) and the masked source is parsed with the grammar
//! the block's `lang` implies. A regex pass then recovers `import('…')` dynamic
//! imports the AST does not edge.
//!
//! atlas has no dedicated TSX grammar, so `lang="tsx"` falls back to the TS
//! grammar (a superset of JS) — documented approximation. `lang="js"`/`"jsx"`
//! use JS; `ts` or unspecified use TS.

use crate::engine::{self, Lang};
use crate::sfc::Rescue;
use crate::ExtractResult;
use atlas_core::ids::{file_stem, make_id};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

/// `_VUE_SCRIPT_RE`: (open-tag)(body)(close-tag). The open-tag matcher skips
/// quoted attribute values so a `>` inside one doesn't end the tag early.
fn vue_script_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(<script\b(?:"[^"]*"|'[^']*'|[^>"'])*>)([\s\S]*?)(</script\s*>)"#)
            .unwrap()
    })
}

/// `_VUE_SCRIPT_LANG_RE`: the declared `lang="…"`.
fn vue_lang_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\blang\s*=\s*['"]?([A-Za-z]+)['"]?"#).unwrap())
}

/// Replace every non-newline char with a space, preserving `\r`/`\n`.
fn blank(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\r' || c == '\n' { c } else { ' ' })
        .collect()
}

/// Blank everything outside `<script>` bodies; return `(masked, first_lang)`.
fn mask_non_script(src: &str) -> (String, Option<String>) {
    let re = vue_script_re();
    let mut out = String::with_capacity(src.len());
    let mut pos = 0;
    let mut lang: Option<String> = None;
    for m in re.captures_iter(src) {
        let whole = m.get(0).unwrap();
        let open = m.get(1).unwrap();
        let body = m.get(2).unwrap();
        let close = m.get(3).unwrap();
        out.push_str(&blank(&src[pos..whole.start()]));
        out.push_str(&blank(open.as_str()));
        out.push_str(body.as_str());
        out.push_str(&blank(close.as_str()));
        pos = whole.end();
        if lang.is_none() {
            if let Some(lm) = vue_lang_re().captures(open.as_str()) {
                lang = Some(lm[1].to_lowercase());
            }
        }
    }
    out.push_str(&blank(&src[pos..]));
    (out, lang)
}

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source);
    let (masked, lang) = mask_non_script(&src);
    let ts_lang = match lang.as_deref() {
        Some("js") | Some("jsx") => Lang::Js,
        // "ts", "tsx" (no dedicated TSX grammar), or unspecified → TS.
        _ => Lang::Ts,
    };

    let mut result = engine::extract(path, masked.as_bytes(), ts_lang);

    let file_nid = make_id([file_stem(path).as_str()]);
    let mut r = Rescue::new(path, &file_nid, &result.nodes);
    r.dynamic_imports(&src);

    result.nodes.extend(r.nodes);
    result.edges.extend(r.edges);
    result
}

//! Node-ID normalization — the single source of truth every ID producer shares.
//!
//! Ported verbatim from graphify `graphify/ids.py`. Three independent producers
//! (the AST extractor, the semantic/LLM pass, and the graph builder) must agree
//! on node IDs or one entity splits into disconnected ghost nodes. Keeping the
//! recipe here means they cannot diverge.
//!
//! Recipe: NFKC-normalize (so composed/decomposed Unicode collapses), replace
//! runs of non-word characters with a single `_` (Unicode-aware, so
//! CJK/Cyrillic/Arabic/accented-Latin letters survive), collapse repeated `_`,
//! strip leading/trailing `_`, and casefold.

use std::path::Path;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

fn non_word() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    // Rust `regex` \w is Unicode-aware by default, matching Python's re.UNICODE.
    RE.get_or_init(|| regex::Regex::new(r"[^\w]+").unwrap())
}

fn multi_underscore() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"_+").unwrap())
}

/// Normalize a single ID string to its canonical form. Idempotent.
pub fn normalize_id(s: &str) -> String {
    let nfkc: String = s.nfkc().collect();
    let underscored = non_word().replace_all(&nfkc, "_");
    let collapsed = multi_underscore().replace_all(&underscored, "_");
    // Python `str.casefold()` is more aggressive than lowercase (e.g. ß→ss);
    // `to_lowercase` matches it for every case graphify's fixtures exercise.
    // ponytail: to_lowercase, swap in a full Unicode casefold if a ß-class ID ever collides.
    collapsed.trim_matches('_').to_lowercase()
}

/// Build a canonical node ID from one or more name parts.
///
/// Parts are joined with `_` (after stripping stray `_`/`.` from each part's
/// edges) then run through [`normalize_id`], so the result is identical to what
/// the builder produces from the already-joined string.
pub fn make_id<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let joined = parts
        .into_iter()
        .map(|p| p.as_ref().trim_matches(|c| c == '_' || c == '.').to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    normalize_id(&joined)
}

/// Stem used as the node-ID prefix for a file and its symbols: the full path
/// with its final extension dropped, as forward-slash segments. `make_id` later
/// collapses the separators. Every path segment is kept so same-named files in
/// different directories get distinct IDs (graphify #1504). Returns "" for a
/// path with no file name (graphify #1618).
pub fn file_stem(path: &Path) -> String {
    if path.file_name().is_none() {
        return String::new();
    }
    let no_ext = path.with_extension("");
    // as_posix(): force forward slashes regardless of platform.
    no_ext
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn basic_normalization() {
        assert_eq!(normalize_id("utils.py"), "utils_py");
        assert_eq!(normalize_id("Foo.Bar()"), "foo_bar");
        assert_eq!(normalize_id("__init__"), "init");
    }

    #[test]
    fn idempotent() {
        for s in ["a.b.c", "Foo—Bar", "x__y", "  spaced  "] {
            assert_eq!(normalize_id(&normalize_id(s)), normalize_id(s));
        }
    }

    #[test]
    fn make_id_joins_and_strips() {
        assert_eq!(make_id(["utils", "primitive_value_to_str"]), "utils_primitive_value_to_str");
        // stray _/. at part edges are trimmed before joining
        assert_eq!(make_id(["utils", ".__init__."]), "utils_init");
        // empty parts dropped
        assert_eq!(make_id(["utils", "", "x"]), "utils_x");
    }

    #[test]
    fn unicode_letters_survive_not_collapse() {
        // graphify #811: CJK/Cyrillic must not collapse to a per-file node.
        assert_eq!(make_id(["模块", "函数"]), "模块_函数");
        assert_eq!(normalize_id("Ölçek"), "ölçek");
    }

    #[test]
    fn file_stem_paths() {
        assert_eq!(file_stem(&PathBuf::from("utils.py")), "utils");
        assert_eq!(file_stem(&PathBuf::from("docs/v1/api/README.md")), "docs/v1/api/README");
        // and make_id over that stem collapses to the node-id prefix
        assert_eq!(make_id([file_stem(&PathBuf::from("docs/v1/api/README.md"))]), "docs_v1_api_readme");
    }
}

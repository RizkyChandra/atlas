//! PDF text extraction (feature `pdf`). Ported from graphify
//! `detect.extract_pdf_text` — plain-text extraction of every page, joined by
//! newlines. graphify used pypdf; the Rust port uses the `pdf-extract` crate.

use std::path::Path;

/// Extract the plain text of a PDF, or empty string on any read/parse error
/// (graphify returns `""` on failure rather than aborting the scan).
pub fn pdf_to_text(path: impl AsRef<Path>) -> String {
    pdf_extract::extract_text(path.as_ref()).unwrap_or_default()
}

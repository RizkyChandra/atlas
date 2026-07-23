//! Office document readers (feature `office`). Ported from graphify
//! `detect.docx_to_markdown` / `detect.xlsx_to_markdown` — the markdown-sidecar
//! text of a `.docx` or `.xlsx`, for the semantic pass.
//!
//! `.docx` is an OOXML zip; we pull run text out of `word/document.xml`
//! directly (zip + regex), which is lighter than the full docx-rs model.
//! `.xlsx` is read with `calamine`.

use regex::Regex;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

fn wt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // <w:t> ... </w:t> run text (attributes like xml:space="preserve" allowed).
    RE.get_or_init(|| Regex::new(r"(?s)<w:t[^>]*>(.*?)</w:t>").unwrap())
}

/// Extract the text of a `.docx`, one line per paragraph. Empty string on any
/// read/parse error.
pub fn docx_to_text(path: impl AsRef<Path>) -> String {
    let Ok(file) = std::fs::File::open(path.as_ref()) else {
        return String::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return String::new();
    };
    let mut xml = String::new();
    match zip.by_name("word/document.xml") {
        Ok(mut entry) => {
            if entry.read_to_string(&mut xml).is_err() {
                return String::new();
            }
        }
        Err(_) => return String::new(),
    }
    // One line per paragraph; runs within a paragraph joined without a gap.
    xml.split("</w:p>")
        .map(|para| {
            wt_re()
                .captures_iter(para)
                .map(|c| unescape_xml(&c[1]))
                .collect::<String>()
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the text of an `.xlsx` as markdown-ish rows, one sheet after another.
/// Empty string on any read/parse error.
pub fn xlsx_to_text(path: impl AsRef<Path>) -> String {
    use calamine::{open_workbook, Reader, Xlsx};
    let Ok(mut wb) = open_workbook::<Xlsx<_>, _>(path.as_ref()) else {
        return String::new();
    };
    let mut sections: Vec<String> = Vec::new();
    for name in wb.sheet_names() {
        let Ok(range) = wb.worksheet_range(&name) else {
            continue;
        };
        let mut lines = vec![format!("## Sheet: {name}")];
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|c| c.to_string()).collect();
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            lines.push(format!("| {} |", cells.join(" | ")));
        }
        if lines.len() > 1 {
            sections.push(lines.join("\n"));
        }
    }
    sections.join("\n\n")
}

fn unescape_xml(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

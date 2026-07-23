//! Bounded CSV/TSV document ingestion (backlog issue #2119 / PR #2125).
//!
//! A tabular file becomes one doc node plus one column node per header, with a
//! `contains` edge from the doc to each column — the same file→structure→column
//! shape graphify uses for `.xlsx` (`detect.xlsx_extract_structure`). Row
//! scanning is *bounded*: we count at most `row_cap` data rows and record
//! whether the file was truncated, so a multi-gigabyte CSV never materializes in
//! memory. The delimiter is inferred from the extension (`.tsv` → tab).

use crate::{edge, node, Extraction};
use atlas_core::ids::{file_stem, make_id};
use serde_json::json;
use std::path::Path;

/// Default cap on data rows scanned. Small: this layer is for structure, not a
/// full table load. ponytail: fixed cap, make it a config knob only if a caller
/// needs a different ceiling.
pub const DEFAULT_ROW_CAP: usize = 1000;

/// Ingest a CSV/TSV file as a bounded document. `row_cap` bounds how many data
/// rows are scanned (0 → [`DEFAULT_ROW_CAP`]).
pub fn extract_csv(path: impl AsRef<Path>, row_cap: usize) -> Extraction {
    let path = path.as_ref();
    let cap = if row_cap == 0 {
        DEFAULT_ROW_CAP
    } else {
        row_cap
    };
    let str_path = path.to_string_lossy().into_owned();
    let stem = file_stem(path);

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let delimiter = if ext == "tsv" { b'\t' } else { b',' };

    let mut rdr = match csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(true)
        .from_path(path)
    {
        Ok(r) => r,
        Err(_) => return Extraction::default(),
    };

    let headers: Vec<String> = match rdr.headers() {
        Ok(h) => h.iter().map(|s| s.trim().to_string()).collect(),
        Err(_) => return Extraction::default(),
    };

    let file_label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_nid = make_id([str_path.as_str()]);

    let mut out = Extraction::default();
    let mut file_node = node(&file_nid, &file_label, "document", &str_path, None);
    file_node.insert("column_count".into(), json!(headers.len()));

    // Column nodes: one per non-empty header, deduped by id.
    let mut seen = std::collections::HashSet::new();
    for (i, header) in headers.iter().enumerate() {
        if header.is_empty() {
            continue;
        }
        // Include the ordinal so two columns sharing a name stay distinct.
        let col_nid = make_id([stem.as_str(), header, &format!("c{i}")]);
        if !seen.insert(col_nid.clone()) {
            continue;
        }
        let mut col = node(&col_nid, header, "document", &str_path, None);
        col.insert("column_index".into(), json!(i));
        out.nodes.push(col);
        out.edges
            .push(edge(&file_nid, &col_nid, "contains", &str_path, None));
    }

    // Bounded row scan: stop as soon as we exceed the cap.
    let mut rows = 0usize;
    let mut truncated = false;
    for rec in rdr.records() {
        if rec.is_err() {
            continue;
        }
        if rows >= cap {
            truncated = true;
            break;
        }
        rows += 1;
    }
    file_node.insert("row_count".into(), json!(rows));
    file_node.insert("rows_truncated".into(), json!(truncated));

    // File node first, then columns (already pushed).
    out.nodes.insert(0, file_node);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn caps_rows_and_emits_columns() {
        let mut f = tempfile(".csv");
        writeln!(f.0, "name,age,city").unwrap();
        for i in 0..10 {
            writeln!(f.0, "person{i},{i},town{i}").unwrap();
        }
        f.0.flush().unwrap();

        let ex = extract_csv(&f.1, 4);
        // 1 doc node + 3 column nodes.
        assert_eq!(ex.nodes.len(), 4);
        assert_eq!(ex.edges.len(), 3);
        let doc = &ex.nodes[0];
        assert_eq!(doc["file_type"], "document");
        assert_eq!(doc["column_count"], 3);
        assert_eq!(doc["row_count"], 4, "row scan must be capped");
        assert_eq!(doc["rows_truncated"], true);
        let labels: Vec<&str> = ex.nodes[1..]
            .iter()
            .map(|n| n["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["name", "age", "city"]);
    }

    // Minimal temp-file helper (no tempfile crate dep).
    fn tempfile(ext: &str) -> (std::fs::File, std::path::PathBuf) {
        let mut p = std::env::temp_dir();
        p.push(format!("atlas_csv_test_{}{}", std::process::id(), ext));
        let f = std::fs::File::create(&p).unwrap();
        (f, p)
    }
}

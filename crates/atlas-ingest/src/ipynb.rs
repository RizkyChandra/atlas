//! Jupyter `.ipynb` markdown-sidecar extraction.
//!
//! A notebook is JSON with a `cells` array; each cell's `source` is a string or
//! a list of line strings. We concatenate markdown and code cells (in order,
//! code fenced) into one markdown document, the same "sidecar" shape graphify's
//! office/pdf converters produce, so the result feeds straight into the
//! markdown reader / chunker. Outputs are dropped.

use serde_json::Value;
use std::path::Path;

/// Extract a notebook's markdown + code cells as one markdown string.
pub fn ipynb_to_text(path: impl AsRef<Path>) -> String {
    let bytes = match std::fs::read(path.as_ref()) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    let nb: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    notebook_to_text(&nb)
}

fn cell_source(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<String>(),
        _ => String::new(),
    }
}

fn notebook_to_text(nb: &Value) -> String {
    let Some(cells) = nb.get("cells").and_then(Value::as_array) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for cell in cells {
        let kind = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
        let src = cell_source(cell);
        if src.trim().is_empty() {
            continue;
        }
        match kind {
            "markdown" | "raw" => parts.push(src),
            "code" => parts.push(format!("```\n{}\n```", src.trim_end())),
            _ => {}
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concatenates_markdown_and_code() {
        let nb = json!({
            "cells": [
                {"cell_type": "markdown", "source": ["# Title\n", "some prose"]},
                {"cell_type": "code", "source": "print('hi')"},
                {"cell_type": "code", "source": "", "outputs": []},
            ],
            "nbformat": 4
        });
        let text = notebook_to_text(&nb);
        assert!(text.contains("# Title"));
        assert!(text.contains("some prose"));
        assert!(text.contains("```\nprint('hi')\n```"));
        assert!(!text.is_empty());
    }
}

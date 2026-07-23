//! Apex extractor — a Rust port of graphify `graphify/extractors/apex.py`.
//!
//! graphify's Apex extractor is REGEX-based (no tree-sitter grammar on PyPI), so
//! matching its oracle means reproducing the same per-line regex passes, not an
//! AST walk. Ported here for `.cls` and `.trigger`: classes/interfaces/enums
//! (`contains`), `extends`/`implements` (INFERRED), methods (`.name()` →
//! `method`, plus a file-level INFERRED `contains` for `@AuraEnabled` /
//! `@InvocableMethod` methods), triggers (`trigger X on SObject` → `contains` +
//! `uses` the SObject), SOQL `[SELECT … FROM S]` → `uses` (INFERRED), and DML
//! (`insert`/`update`/… → `dml_<op>` `uses`, INFERRED).
//!
//! Edge shape matches the built oracle: `confidence` is EXTRACTED or INFERRED,
//! no `confidence_score`. Node shape is the standard `code` node.

use crate::ExtractResult;
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;

const CONTROL_FLOW: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "catch",
    "finally",
    "return",
    "throw",
    "new",
    "void",
    "null",
    "true",
    "false",
    "this",
    "super",
    "class",
    "interface",
    "enum",
    "trigger",
    "on",
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

    // graphify's f-string-composed regex fragments.
    let access = r"(?:public|private|protected|global|webService)?";
    let sharing = r"(?:\s+(?:with|without|inherited)\s+sharing)?";
    let md = r"(?:\s+(?:abstract|virtual|override|static|final|transient|testMethod))?";
    let ann = r"(?:\s*@\w+(?:\s*\([^)]*\))?\s*)*";

    let cls_re = Regex::new(&format!(
        r"(?i)^{ann}\s*{access}{sharing}{md}\s*class\s+(\w+)(?:\s+extends\s+(\w+))?(?:\s+implements\s+([\w,\s]+))?\s*\{{?"
    ))
    .unwrap();
    let iface_re = Regex::new(&format!(
        r"(?i)^{ann}\s*{access}{sharing}{md}\s*interface\s+(\w+)(?:\s+extends\s+([\w,\s]+))?\s*\{{?"
    ))
    .unwrap();
    let enum_re = Regex::new(&format!(
        r"(?i)^{ann}\s*{access}{sharing}{md}\s*enum\s+(\w+)\s*\{{?"
    ))
    .unwrap();
    let trigger_re = Regex::new(r"(?i)^\s*trigger\s+(\w+)\s+on\s+(\w+)\s*\(").unwrap();
    let method_re = Regex::new(&format!(
        r"(?i)^{ann}\s*{access}{md}\s*(?:static\s+)?[\w<>\[\]]+\s+(\w+)\s*\([^)]*\)\s*(?:throws\s+\w+\s*)?\{{?"
    ))
    .unwrap();
    let annotation_re = Regex::new(r"(?i)@(\w+)").unwrap();
    let soql_re = Regex::new(r"(?i)\[\s*SELECT\b[^\]]+FROM\s+(\w+)").unwrap();
    let dml_re = Regex::new(r"(?i)\b(insert|update|delete|upsert|merge|undelete)\s+\w").unwrap();

    let mut a = Apex {
        stem,
        str_path,
        file_nid: file_nid.clone(),
        nodes: Vec::new(),
        edges: Vec::new(),
        seen: HashSet::new(),
    };
    a.add_node(&file_nid, &filename, 1);

    let mut current_class_nid: Option<String> = None;
    let mut pending: Vec<String> = Vec::new();

    for (idx, line_text) in src.lines().enumerate() {
        let lineno = idx + 1;
        let stripped = line_text.trim();

        if stripped.starts_with('@') {
            for m in annotation_re.captures_iter(stripped) {
                pending.push(m[1].to_lowercase());
            }
            continue;
        }

        if let Some(tm) = trigger_re.captures(stripped) {
            let trig = &tm[1];
            let sobject = tm[2].to_string();
            let trig_nid = make_id([a.stem.as_str(), trig]);
            a.add_node(&trig_nid, trig, lineno);
            let f = a.file_nid.clone();
            a.add_edge(&f, &trig_nid, "contains", "EXTRACTED", lineno);
            let sob_nid = make_id([sobject.as_str()]);
            a.add_node(&sob_nid, &sobject, lineno);
            a.add_edge(&trig_nid, &sob_nid, "uses", "INFERRED", lineno);
            current_class_nid = Some(trig_nid);
            pending.clear();
            continue;
        }

        if let Some(cm) = cls_re.captures(stripped) {
            let class_name = cm[1].to_string();
            if CONTROL_FLOW.contains(&class_name.to_lowercase().as_str()) {
                pending.clear();
                continue;
            }
            let class_nid = make_id([a.stem.as_str(), class_name.as_str()]);
            a.add_node(&class_nid, &class_name, lineno);
            let f = a.file_nid.clone();
            a.add_edge(&f, &class_nid, "contains", "EXTRACTED", lineno);
            if let Some(base) = cm.get(2) {
                let base = base.as_str().trim();
                let base_nid = a.resolve_named(base, lineno);
                a.add_edge(&class_nid, &base_nid, "extends", "INFERRED", lineno);
            }
            if let Some(ifaces) = cm.get(3) {
                for iface in ifaces.as_str().split(',') {
                    let iface = iface.trim();
                    if !iface.is_empty() {
                        let iface_nid = a.resolve_named(iface, lineno);
                        a.add_edge(&class_nid, &iface_nid, "implements", "INFERRED", lineno);
                    }
                }
            }
            current_class_nid = Some(class_nid);
            pending.clear();
            continue;
        }

        if let Some(im) = iface_re.captures(stripped) {
            let iface_name = im[1].to_string();
            if CONTROL_FLOW.contains(&iface_name.to_lowercase().as_str()) {
                pending.clear();
                continue;
            }
            let iface_nid = make_id([a.stem.as_str(), iface_name.as_str()]);
            a.add_node(&iface_nid, &iface_name, lineno);
            let parent = current_class_nid
                .clone()
                .unwrap_or_else(|| a.file_nid.clone());
            a.add_edge(&parent, &iface_nid, "contains", "EXTRACTED", lineno);
            if let Some(parents) = im.get(2) {
                for p in parents.as_str().split(',') {
                    let p = p.trim();
                    if !p.is_empty() {
                        let p_nid = a.resolve_named(p, lineno);
                        a.add_edge(&iface_nid, &p_nid, "extends", "INFERRED", lineno);
                    }
                }
            }
            pending.clear();
            continue;
        }

        if let Some(em) = enum_re.captures(stripped) {
            let enum_name = em[1].to_string();
            if CONTROL_FLOW.contains(&enum_name.to_lowercase().as_str()) {
                pending.clear();
                continue;
            }
            let enum_nid = make_id([a.stem.as_str(), enum_name.as_str()]);
            a.add_node(&enum_nid, &enum_name, lineno);
            let parent = current_class_nid
                .clone()
                .unwrap_or_else(|| a.file_nid.clone());
            a.add_edge(&parent, &enum_nid, "contains", "EXTRACTED", lineno);
            pending.clear();
            continue;
        }

        if let Some(cur) = current_class_nid.clone() {
            if let Some(mm) = method_re.captures(stripped) {
                let method_name = mm[1].to_string();
                if !CONTROL_FLOW.contains(&method_name.to_lowercase().as_str()) {
                    let method_nid = make_id([cur.as_str(), method_name.as_str()]);
                    a.add_node(&method_nid, &format!(".{method_name}()"), lineno);
                    a.add_edge(&cur, &method_nid, "method", "EXTRACTED", lineno);
                    if pending
                        .iter()
                        .any(|p| p == "auraenabled" || p == "invocablemethod")
                    {
                        let f = a.file_nid.clone();
                        a.add_edge(&f, &method_nid, "contains", "INFERRED", lineno);
                    }
                    pending.clear();
                    continue;
                }
            }
        }

        pending.clear();

        for sm in soql_re.captures_iter(line_text) {
            let sobject = sm[1].to_string();
            let sob_nid = make_id([sobject.as_str()]);
            a.add_node(&sob_nid, &sobject, lineno);
            let src_nid = current_class_nid
                .clone()
                .unwrap_or_else(|| a.file_nid.clone());
            a.add_edge(&src_nid, &sob_nid, "uses", "INFERRED", lineno);
        }
        for dm in dml_re.captures_iter(line_text) {
            let op = dm[1].to_lowercase();
            let dml_nid = make_id([format!("dml_{op}").as_str()]);
            a.add_node(&dml_nid, &op, lineno);
            let src_nid = current_class_nid
                .clone()
                .unwrap_or_else(|| a.file_nid.clone());
            a.add_edge(&src_nid, &dml_nid, "uses", "INFERRED", lineno);
        }
    }

    ExtractResult {
        nodes: a.nodes,
        edges: a.edges,
    }
}

struct Apex {
    stem: String,
    str_path: String,
    file_nid: String,
    nodes: Vec<Attrs>,
    edges: Vec<Attrs>,
    seen: HashSet<String>,
}

impl Apex {
    fn add_node(&mut self, nid: &str, label: &str, line: usize) {
        if !self.seen.insert(nid.to_string()) {
            return;
        }
        let mut m = Attrs::new();
        m.insert("id".into(), json!(nid));
        m.insert("label".into(), json!(label));
        m.insert("file_type".into(), json!("code"));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), json!(format!("L{line}")));
        self.nodes.push(m);
    }

    fn add_edge(&mut self, src: &str, tgt: &str, relation: &str, confidence: &str, line: usize) {
        let mut m = Attrs::new();
        m.insert("source".into(), json!(src));
        m.insert("target".into(), json!(tgt));
        m.insert("relation".into(), json!(relation));
        m.insert("confidence".into(), json!(confidence));
        m.insert("source_file".into(), json!(self.str_path));
        m.insert("source_location".into(), json!(format!("L{line}")));
        m.insert("weight".into(), json!(1.0));
        self.edges.push(m);
    }

    /// graphify base/interface resolution: prefer an already-defined same-file id
    /// (`stem_name`), else an already-defined bare id (`name`), else create a bare
    /// stub node. Matches `apex.py`'s two-step `seen_ids` lookup.
    fn resolve_named(&mut self, name: &str, line: usize) -> String {
        let same_file = make_id([self.stem.as_str(), name]);
        if self.seen.contains(&same_file) {
            return same_file;
        }
        let bare = make_id([name]);
        if !self.seen.contains(&bare) {
            self.add_node(&bare, name, line);
        }
        bare
    }
}

//! GraphML export — opens in Gephi, yEd, and any GraphML-compatible tool.
//!
//! Ports `export.py::to_graphml`. Node/edge attributes become `<data>` under
//! declared `<key>`s. Internal `_`-prefixed markers (e.g. `_src`/`_tgt`) are
//! dropped. `source`/`target`/`id` are structural, not data. Non-scalar or null
//! values are coerced (null -> "", array/object -> JSON string) exactly as the
//! Python `_graphml_safe` did.

use crate::{attr_str, node_id, xml_escape, Graph};
use atlas_core::Attrs;
use serde_json::Value;

fn val_type(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "double",
        Value::Number(_) => "long",
        _ => "string",
    }
}

fn val_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        // array/object -> JSON string (GraphML has no scalar for them)
        other => other.to_string(),
    }
}

struct Key {
    scope: &'static str,
    name: String,
    ty: &'static str,
}

fn collect(items: &[Attrs], scope: &'static str, skip: &[&str], keys: &mut Vec<Key>) {
    for it in items {
        for (k, v) in it {
            if k.starts_with('_') || skip.contains(&k.as_str()) {
                continue;
            }
            if !keys.iter().any(|x| x.scope == scope && x.name == *k) {
                keys.push(Key { scope, name: k.clone(), ty: val_type(v) });
            }
        }
    }
}

fn key_id(keys: &[Key], scope: &str, name: &str) -> usize {
    keys.iter()
        .position(|k| k.scope == scope && k.name == name)
        .expect("key was collected")
}

pub fn to_graphml(g: &Graph) -> String {
    let mut keys: Vec<Key> = Vec::new();
    collect(&g.nodes, "node", &["id"], &mut keys);
    collect(&g.links, "edge", &["source", "target"], &mut keys);

    let mut out = String::new();
    out.push_str("<?xml version='1.0' encoding='utf-8'?>\n");
    out.push_str(
        "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xsi:schemaLocation=\"http://graphml.graphdrawing.org/xmlns \
         http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd\">\n",
    );

    for (i, k) in keys.iter().enumerate() {
        out.push_str(&format!(
            "  <key id=\"d{i}\" for=\"{}\" attr.name=\"{}\" attr.type=\"{}\"/>\n",
            k.scope,
            xml_escape(&k.name),
            k.ty
        ));
    }

    let edgedefault = if g.directed { "directed" } else { "undirected" };
    out.push_str(&format!("  <graph edgedefault=\"{edgedefault}\">\n"));

    for n in &g.nodes {
        out.push_str(&format!("    <node id=\"{}\">\n", xml_escape(node_id(n))));
        for (k, v) in n {
            if k.starts_with('_') || k == "id" {
                continue;
            }
            let id = key_id(&keys, "node", k);
            out.push_str(&format!(
                "      <data key=\"d{id}\">{}</data>\n",
                xml_escape(&val_str(v))
            ));
        }
        out.push_str("    </node>\n");
    }

    for e in &g.links {
        let src = xml_escape(attr_str(e, "source").unwrap_or(""));
        let tgt = xml_escape(attr_str(e, "target").unwrap_or(""));
        out.push_str(&format!("    <edge source=\"{src}\" target=\"{tgt}\">\n"));
        for (k, v) in e {
            if k.starts_with('_') || k == "source" || k == "target" {
                continue;
            }
            let id = key_id(&keys, "edge", k);
            out.push_str(&format!(
                "      <data key=\"d{id}\">{}</data>\n",
                xml_escape(&val_str(v))
            ));
        }
        out.push_str("    </edge>\n");
    }

    out.push_str("  </graph>\n</graphml>\n");
    out
}

#[cfg(test)]
mod tests {
    use crate::httpx;

    #[test]
    fn one_node_one_edge_each_with_data() {
        let g = httpx();
        let xml = super::to_graphml(&g);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("</graphml>"));
        assert_eq!(xml.matches("<node ").count(), g.nodes.len());
        assert_eq!(xml.matches("<edge ").count(), g.links.len());
        // attrs surface as <data>, and every one is closed (structural balance).
        assert!(xml.contains("<data key="));
        assert_eq!(xml.matches("<data key=").count(), xml.matches("</data>").count());
        assert_eq!(xml.matches("<node ").count(), xml.matches("</node>").count());
        assert_eq!(xml.matches("<edge ").count(), xml.matches("</edge>").count());
    }

    #[test]
    fn drops_internal_markers() {
        // golden edges carry _src/_tgt — they must not leak as data keys.
        let g = httpx();
        let xml = super::to_graphml(&g);
        assert!(!xml.contains("attr.name=\"_src\""));
        assert!(!xml.contains("attr.name=\"_tgt\""));
    }

    #[test]
    fn well_formed_enough_to_stack_parse() {
        // Naive tag-stack check: every open tag closes in order. Catches
        // unescaped '<' in values that would corrupt the document.
        let g = httpx();
        let xml = super::to_graphml(&g);
        assert!(balanced(&xml), "graphml tags not balanced");
    }

    // Minimal well-formedness: matches <tag ...> / </tag> / <tag/> and checks
    // the close order. Enough to prove values are escaped (a raw '<' in a label
    // would desync the stack).
    fn balanced(xml: &str) -> bool {
        let mut stack: Vec<String> = Vec::new();
        let bytes = xml.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            // skip declarations / comments
            if xml[i..].starts_with("<?") {
                i += xml[i..].find("?>").map(|p| p + 2).unwrap_or(2);
                continue;
            }
            let end = match xml[i..].find('>') {
                Some(e) => i + e,
                None => return false,
            };
            let inner = &xml[i + 1..end];
            if let Some(name) = inner.strip_prefix('/') {
                match stack.pop() {
                    Some(open) if open == name.trim() => {}
                    _ => return false,
                }
            } else if inner.ends_with('/') {
                // self-closing, no push
            } else {
                let name = inner.split_whitespace().next().unwrap_or("").to_string();
                stack.push(name);
            }
            i = end + 1;
        }
        stack.is_empty()
    }
}

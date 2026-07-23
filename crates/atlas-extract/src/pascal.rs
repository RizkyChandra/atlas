//! Pascal / Delphi extractor — a Rust port of graphify's Pascal extractor.
//!
//! graphify uses tree-sitter-pascal when available and falls back to a REGEX
//! extractor (`extract_pascal._extract_pascal_regex`) otherwise. The published
//! Rust grammar crate (`tree-sitter-pascal` 0.10.2) does NOT match the oracle's
//! venv grammar (0.11.0), so per the milestone rules we take the sanctioned
//! regex path. It reproduces the oracle for the sample with two structural
//! adjustments that align the regex passes with the tree-sitter *oracle*:
//!
//!   1. Method nodes/edges are sourced from IMPLEMENTATION headers only
//!      (`procedure TFoo.Bar; begin … end;`), not from class-body forward
//!      declarations. The tree-sitter grammar likewise emits method nodes from
//!      `defProc` (implementation) rather than the in-class forward decls, so
//!      method `source_location` is the implementation line — matching the
//!      oracle. A class method declared but never implemented in-file (abstract
//!      / pure interface) therefore gets no node, exactly as the oracle does.
//!   2. Consequently interface method declarations emit no nodes (interfaces
//!      have no implementations) — again matching the oracle.
//!
//! Emits: file `contains` module (unit/program/library); module `imports` each
//! `uses` unit (bare `make_id(name)` targets — cross-file unit/class resolution
//! is out of single-file scope); class/interface type nodes (`contains`) with
//! `inherits` to each base (same-file real node if defined here, else a bare
//! stub); procedure/function implementations (`method` for `TClass.Method`,
//! `contains` for free procs); and in-file `calls` edges scoped by the caller's
//! own class → ancestor chain → file-level free functions → unambiguous global.
//!
//! Node/edge shape is the standard `code` node + EXTRACTED edge (no
//! `confidence_score`), i.e. `node_map` / `edge_map`.

use crate::{edge_map, node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const KEYWORDS: &[&str] = &[
    "begin",
    "end",
    "if",
    "then",
    "else",
    "while",
    "do",
    "for",
    "to",
    "downto",
    "repeat",
    "until",
    "case",
    "of",
    "try",
    "finally",
    "except",
    "with",
    "inherited",
    "result",
    "var",
    "const",
    "type",
    "nil",
    "true",
    "false",
    "exit",
    "break",
    "continue",
    "uses",
    "unit",
    "program",
    "library",
    "interface",
    "implementation",
    "initialization",
    "finalization",
    "procedure",
    "function",
    "constructor",
    "destructor",
    "class",
    "record",
    "object",
    "array",
    "string",
    "integer",
    "boolean",
    "real",
    "char",
    "writeln",
    "write",
    "readln",
    "read",
    "assigned",
    "length",
    "high",
    "low",
    "inc",
    "dec",
    "new",
    "dispose",
    "setlength",
    "copy",
    "pos",
    "trim",
    "format",
    "inttostr",
    "strtoint",
    "ord",
    "chr",
    "sizeof",
    "create",
    "free",
    "destroy",
];

fn lineno(stripped: &str, offset: usize) -> usize {
    stripped[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

/// graphify comment strip: blank out `{}`, `(* *)`, `//` runs (preserving
/// newlines so line numbers survive), keep `'…'` string literals verbatim.
fn strip_comments(text: &str) -> String {
    let re = Regex::new(r"(?s)'(?:''|[^'])*'|\{[^}]*\}|\(\*.*?\*\)|//[^\n]*").unwrap();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in re.find_iter(text) {
        out.push_str(&text[last..m.start()]);
        let tok = m.as_str();
        if tok.starts_with('\'') {
            out.push_str(tok);
        } else {
            for c in tok.chars() {
                out.push(if c == '\n' { '\n' } else { ' ' });
            }
        }
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// (iface_text, iface_off, impl_text, impl_off). Files with no interface/
/// implementation split return the whole text as impl at offset 0.
fn split_sections(text: &str) -> (String, usize, String, usize) {
    let iface = Regex::new(r"(?i)\binterface\b").unwrap().find(text);
    let implm = Regex::new(r"(?i)\bimplementation\b").unwrap().find(text);
    if let (Some(i), Some(p)) = (iface, implm) {
        let iface_off = i.end();
        let impl_off = p.end();
        let end = Regex::new(r"(?i)\b(initialization|finalization)\b")
            .unwrap()
            .find(&text[impl_off..])
            .map(|m| impl_off + m.start())
            .unwrap_or(text.len());
        (
            text[iface_off..p.start()].to_string(),
            iface_off,
            text[impl_off..end].to_string(),
            impl_off,
        )
    } else {
        (String::new(), 0, text.to_string(), 0)
    }
}

/// Split a `uses` list, handling `Foo in 'bar.pas'`.
fn split_uses(s: &str) -> Vec<String> {
    let inre = Regex::new(r"(?i)\s+in\s+").unwrap();
    let identre = Regex::new(r"^[A-Za-z_][\w.]*$").unwrap();
    s.split(',')
        .filter_map(|chunk| {
            let name = inre.splitn(chunk.trim(), 2).next().unwrap_or("");
            let name = name.trim().trim_matches(';').trim();
            (!name.is_empty() && identre.is_match(name)).then(|| name.to_string())
        })
        .collect()
}

/// Split an inheritance list, handling generics like `TList<T, U>`.
fn split_bases(s: &str) -> Vec<String> {
    let identre = Regex::new(r"^[A-Za-z_]\w*$").unwrap();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut buf = String::new();
    let flush = |buf: &str| -> Option<String> {
        let name = buf.split('<').next().unwrap_or("").trim().to_string();
        (!name.is_empty()).then_some(name)
    };
    for ch in s.chars() {
        match ch {
            '<' => {
                depth += 1;
                buf.push(ch);
            }
            '>' => {
                depth -= 1;
                buf.push(ch);
            }
            ',' if depth == 0 => {
                if let Some(n) = flush(&buf) {
                    out.push(n);
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    if let Some(n) = flush(&buf) {
        out.push(n);
    }
    out.into_iter().filter(|n| identre.is_match(n)).collect()
}

/// Balanced `begin..end` after `start`. Returns (body_start, body_end) byte
/// offsets into `text`, or (0, 0) if no `begin`.
fn find_body(text: &str, start: usize, tok_re: &Regex) -> (usize, usize) {
    let beginre = Regex::new(r"(?i)\bbegin\b").unwrap();
    let Some(m) = beginre.find(&text[start..]) else {
        return (0, 0);
    };
    let body_start = start + m.end();
    let mut depth = 1i32;
    for tok in tok_re.find_iter(&text[body_start..]) {
        match tok.as_str().to_lowercase().as_str() {
            "begin" | "case" | "try" | "asm" | "record" => depth += 1,
            "end" => {
                depth -= 1;
                if depth == 0 {
                    return (body_start, body_start + tok.start());
                }
            }
            _ => {}
        }
    }
    (body_start, text.len())
}

/// Per-procedure record: (proc_nid, body_text, container, name_lower,
/// body_abs_off). `body_abs_off` is the byte offset of the body in the
/// comment-stripped source, so call lines resolve to the true source line
/// (matching the tree-sitter oracle; graphify's regex fallback derives the line
/// from the header line + in-body newline count, which is off-by-one).
type Rec = (String, String, String, String, usize);

/// graphify `_resolve_pascal_callee_factory`: caller's own class → ancestors
/// (via `inherits` edges) → file-level free funcs → unambiguous global.
struct CalleeResolver {
    class_bases: HashMap<String, Vec<String>>,
    class_procs: HashMap<String, HashMap<String, Vec<String>>>,
    module_procs: HashMap<String, Vec<String>>,
    global_procs: HashMap<String, Vec<String>>,
    proc_owner: HashMap<String, String>,
}

impl CalleeResolver {
    fn new(records: &[Rec], edges: &[Attrs], module_nid: &str) -> Self {
        let mut class_bases: HashMap<String, Vec<String>> = HashMap::new();
        for e in edges {
            if e.get("relation").and_then(|v| v.as_str()) == Some("inherits") {
                if let (Some(s), Some(t)) = (
                    e.get("source").and_then(|v| v.as_str()),
                    e.get("target").and_then(|v| v.as_str()),
                ) {
                    class_bases
                        .entry(s.to_string())
                        .or_default()
                        .push(t.to_string());
                }
            }
        }
        let mut class_procs: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
        let mut module_procs: HashMap<String, Vec<String>> = HashMap::new();
        let mut global_procs: HashMap<String, Vec<String>> = HashMap::new();
        let mut proc_owner: HashMap<String, String> = HashMap::new();
        for (proc_nid, _body, container, name_lower, _off) in records {
            proc_owner.insert(proc_nid.clone(), container.clone());
            global_procs
                .entry(name_lower.clone())
                .or_default()
                .push(proc_nid.clone());
            if container == module_nid {
                module_procs
                    .entry(name_lower.clone())
                    .or_default()
                    .push(proc_nid.clone());
            } else {
                class_procs
                    .entry(container.clone())
                    .or_default()
                    .entry(name_lower.clone())
                    .or_default()
                    .push(proc_nid.clone());
            }
        }
        CalleeResolver {
            class_bases,
            class_procs,
            module_procs,
            global_procs,
            proc_owner,
        }
    }

    fn resolve(&self, caller_nid: &str, name_lower: &str) -> Option<String> {
        if let Some(owner) = self.proc_owner.get(caller_nid) {
            if let Some(c) = self.class_procs.get(owner).and_then(|m| m.get(name_lower)) {
                return (c.len() == 1).then(|| c[0].clone());
            }
            let mut seen: HashSet<String> = HashSet::new();
            let mut queue: Vec<String> = self.class_bases.get(owner).cloned().unwrap_or_default();
            while let Some(base) = (!queue.is_empty()).then(|| queue.remove(0)) {
                if !seen.insert(base.clone()) {
                    continue;
                }
                if let Some(c) = self.class_procs.get(&base).and_then(|m| m.get(name_lower)) {
                    return (c.len() == 1).then(|| c[0].clone());
                }
                if let Some(b) = self.class_bases.get(&base) {
                    queue.extend(b.clone());
                }
            }
        }
        if let Some(c) = self.module_procs.get(name_lower) {
            return (c.len() == 1).then(|| c[0].clone());
        }
        if let Some(c) = self.global_procs.get(name_lower) {
            if c.len() == 1 {
                return Some(c[0].clone());
            }
        }
        None
    }
}

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    let raw = String::from_utf8_lossy(source).into_owned();
    let stem = file_stem(path);
    let str_path = path.to_string_lossy().into_owned();
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut nodes: Vec<Attrs> = Vec::new();
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    macro_rules! add_node {
        ($nid:expr, $label:expr, $line:expr) => {{
            let nid: String = $nid;
            if seen.insert(nid.clone()) {
                nodes.push(node_map(
                    &nid,
                    $label,
                    "code",
                    &str_path,
                    &format!("L{}", $line),
                ));
            }
        }};
    }
    // dedup on (src,tgt,relation) like graphify's `_add_edge`.
    macro_rules! add_edge {
        ($src:expr, $tgt:expr, $rel:expr, $line:expr, $ctx:expr) => {{
            let key = ($src.to_string(), $tgt.to_string(), $rel.to_string());
            if seen_edges.insert(key) {
                edges.push(edge_map(
                    $src,
                    $tgt,
                    $rel,
                    $ctx,
                    &str_path,
                    &format!("L{}", $line),
                ));
            }
        }};
    }

    let file_nid = make_id([stem.as_str()]);
    add_node!(file_nid.clone(), &filename, 1);

    let stripped = strip_comments(&raw);

    // Module header.
    let mut module_nid = file_nid.clone();
    let mod_re = Regex::new(r"(?i)\b(unit|program|library)\s+([A-Za-z_][\w.]*)\s*;").unwrap();
    if let Some(m) = mod_re.captures(&stripped) {
        let whole = m.get(0).unwrap();
        let mod_name = m.get(2).unwrap().as_str();
        let line = lineno(&stripped, whole.start());
        module_nid = make_id([stem.as_str(), mod_name]);
        add_node!(module_nid.clone(), mod_name, line);
        add_edge!(&file_nid, &module_nid, "contains", line, None);
    }

    let (iface_text, iface_off, impl_text, impl_off) = split_sections(&stripped);

    // Uses clauses (both sections).
    let uses_re = Regex::new(r"(?is)\buses\b\s*([^;]+);").unwrap();
    for (section_text, section_off) in [(&iface_text, iface_off), (&impl_text, impl_off)] {
        for um in uses_re.captures_iter(section_text) {
            let whole = um.get(0).unwrap();
            let line = lineno(&stripped, section_off + whole.start());
            for unit in split_uses(um.get(1).unwrap().as_str()) {
                // Cross-file unit resolution is out of scope: bare id target.
                let tgt = make_id([unit.as_str()]);
                add_edge!(&module_nid, &tgt, "imports", line, Some("import"));
            }
        }
    }

    // Type declarations (class / interface) in the interface section (or whole
    // file when there is no interface section).
    let (search_text, search_off) = if iface_text.is_empty() {
        (stripped.as_str(), 0)
    } else {
        (iface_text.as_str(), iface_off)
    };
    let type_re = Regex::new(
        r"(?i)\b(?P<name>[A-Za-z_]\w*)(?:\s*<[^>]+>)?\s*=\s*(?:packed\s+)?(?P<kind>class|interface)\b(?:\s*\(\s*(?P<bases>[^)]*)\s*\))?",
    )
    .unwrap();
    let end_semi_re = Regex::new(r"(?i)\bend\s*;").unwrap();
    let mut pos = 0usize;
    while pos < search_text.len() {
        let Some(hm) = type_re.captures_at(search_text, pos) else {
            break;
        };
        let whole = hm.get(0).unwrap();
        let type_name = hm.name("name").unwrap().as_str();
        let bases_raw = hm.name("bases").map(|m| m.as_str()).unwrap_or("");
        let line = lineno(&stripped, search_off + whole.start());
        let cls_nid = make_id([stem.as_str(), type_name]);
        add_node!(cls_nid.clone(), type_name, line);
        add_edge!(&module_nid, &cls_nid, "contains", line, None);

        for base in split_bases(bases_raw) {
            let same_file = make_id([stem.as_str(), base.as_str()]);
            let base_nid = if seen.contains(&same_file) {
                same_file
            } else {
                // Cross-file class resolution out of scope → bare sourced stub.
                let bare = make_id([base.as_str()]);
                add_node!(bare.clone(), base.as_str(), line);
                bare
            };
            add_edge!(&cls_nid, &base_nid, "inherits", line, None);
        }

        // (Forward method declarations intentionally NOT emitted here — methods
        // come from implementation headers so their line matches the oracle.)
        pos = end_semi_re
            .find_at(search_text, whole.end())
            .map(|m| m.end())
            .unwrap_or(search_text.len());
    }

    // Implementation headers.
    let impl_re = Regex::new(
        r"(?i)\b(?:procedure|function|constructor|destructor)\s+(?P<qual>[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?)(?:\s*<[^>]+>)?(?:\s*\([^)]*\))?(?:\s*:\s*[\w<>,\s.]+)?\s*;",
    )
    .unwrap();
    let tok_re = Regex::new(r"(?i)\b(begin|end|case|try|asm|record)\b").unwrap();
    let mut records: Vec<Rec> = Vec::new();
    for fm in impl_re.captures_iter(&impl_text) {
        let whole = fm.get(0).unwrap();
        let qualified = fm.name("qual").unwrap().as_str();
        let line = lineno(&stripped, impl_off + whole.start());
        let (container, relation, label, name_lower) =
            if let Some((cls_part, method_part)) = qualified.split_once('.') {
                let cls_nid = make_id([stem.as_str(), cls_part]);
                if seen.contains(&cls_nid) {
                    (
                        cls_nid,
                        "method",
                        format!("{method_part}()"),
                        method_part.to_lowercase(),
                    )
                } else {
                    (
                        module_nid.clone(),
                        "contains",
                        format!("{method_part}()"),
                        method_part.to_lowercase(),
                    )
                }
            } else {
                (
                    module_nid.clone(),
                    "contains",
                    format!("{qualified}()"),
                    qualified.to_lowercase(),
                )
            };
        let proc_nid = make_id([stem.as_str(), qualified]);
        add_node!(proc_nid.clone(), &label, line);
        add_edge!(&container, &proc_nid, relation, line, None);

        let (bs, be) = find_body(&impl_text, whole.end(), &tok_re);
        let body = if bs != 0 {
            impl_text[bs..be].to_string()
        } else {
            String::new()
        };
        let body_abs_off = impl_off + bs;
        records.push((proc_nid, body, container, name_lower, body_abs_off));
    }

    // In-file call edges.
    let resolver = CalleeResolver::new(&records, &edges, &module_nid);
    let call_re = Regex::new(r"\b([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*)\s*[(;]").unwrap();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for (caller_nid, body, _container, _name, body_abs_off) in &records {
        for cm in call_re.captures_iter(body) {
            let name = cm
                .get(1)
                .unwrap()
                .as_str()
                .rsplit('.')
                .next()
                .unwrap()
                .to_lowercase();
            if KEYWORDS.contains(&name.as_str()) {
                continue;
            }
            let call_line = lineno(&stripped, body_abs_off + cm.get(0).unwrap().start());
            let Some(target) = resolver.resolve(caller_nid, &name) else {
                continue; // cross-file / unresolved — out of scope
            };
            if &target == caller_nid {
                continue;
            }
            let pair = (caller_nid.clone(), target.clone());
            if seen_pairs.insert(pair) {
                add_edge!(
                    caller_nid.as_str(),
                    target.as_str(),
                    "calls",
                    call_line,
                    Some("call")
                );
            }
        }
    }

    ExtractResult { nodes, edges }
}

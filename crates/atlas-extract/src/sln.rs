//! .NET project/solution extractor — a Rust port of graphify's `extract_sln`
//! (`graphify/extractors/sln.py`) plus `extract_slnx` / `extract_csproj`
//! (`graphify/extract.py`).
//!
//! Dispatched extensions: `.sln` (legacy text solution), `.slnx` (XML solution),
//! `.csproj` / `.fsproj` / `.vbproj` (MSBuild project).
//!
//! Nodes: the file itself, project nodes (path-keyed), NuGet package refs
//! (`nuget_*`), target-framework (`framework_*`) and SDK (`sdk_*`) concepts.
//! Edges: file `contains` project (.sln/.slnx), project `imports` project
//! (build-order deps / ProjectReference), file `references` framework/sdk, file
//! `imports` package.
//!
//! Project ids key off the RESOLVED absolute path of the referenced project
//! (matching graphify's `_make_id(str((path.parent / rel).resolve()))`); the
//! path prefix is neutralized in the test via a scan-dir remap, exactly like
//! graphify's build-time id relativization / `ext_`-prefixing.
//!
//! Note: sln/slnx/csproj edges carry NO `source_location` and nodes use a null
//! `source_location` (matching the oracle) — so they are built here directly
//! rather than via the shared `edge_map` (which always sets a location).

use crate::{node_map, ExtractResult};
use atlas_core::ids::{file_stem, make_id};
use atlas_core::Attrs;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

pub fn extract(path: &Path, source: &[u8]) -> ExtractResult {
    match path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "sln" => extract_sln(path, source),
        "slnx" => extract_slnx(path, source),
        _ => extract_csproj(path, source), // .csproj / .fsproj / .vbproj
    }
}

/// Lexically resolve `rel` (with `\` or `/` separators) against absolute `base`,
/// collapsing `.`/`..` WITHOUT touching disk — mirrors `Path.resolve()` for the
/// non-existent referenced-project paths .NET solution files carry.
fn resolve_abs(base: &Path, rel: &str) -> String {
    let mut stack: Vec<String> = base
        .to_string_lossy()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    for part in rel.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            p => stack.push(p.to_string()),
        }
    }
    format!("/{}", stack.join("/"))
}

/// A file→project / project→project edge with a NULL source_location and no
/// context, matching the sln/slnx/csproj oracle shape.
fn dotnet_edge(src: &str, tgt: &str, relation: &str, source_file: &str) -> Attrs {
    let mut m = Attrs::new();
    m.insert("source".into(), json!(src));
    m.insert("target".into(), json!(tgt));
    m.insert("relation".into(), json!(relation));
    m.insert("confidence".into(), json!("EXTRACTED"));
    m.insert("source_file".into(), json!(source_file));
    m.insert("weight".into(), json!(1.0));
    m
}

fn file_node(path: &Path, str_path: &str) -> (String, Attrs) {
    let file_nid = make_id([file_stem(path).as_str()]);
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut m = node_map(&file_nid, &label, "code", str_path, "");
    m.insert("source_location".into(), Value::Null);
    (file_nid, m)
}

fn concept_node(nid: &str, label: &str, str_path: &str) -> Attrs {
    let mut m = node_map(nid, label, "concept", str_path, "");
    m.insert("source_location".into(), Value::Null);
    m
}

fn code_node(nid: &str, label: &str, source_file: &str) -> Attrs {
    let mut m = node_map(nid, label, "code", source_file, "");
    m.insert("source_location".into(), Value::Null);
    m
}

// ── .sln (legacy text) ──────────────────────────────────────────────────────

fn extract_sln(path: &Path, source: &[u8]) -> ExtractResult {
    let src = String::from_utf8_lossy(source);
    let str_path = path.to_string_lossy().into_owned();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));

    let (file_nid, file_map) = file_node(path, &str_path);
    let mut nodes = vec![file_map];
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(file_nid.clone());

    let project_re =
        Regex::new(r#"Project\("[^"]*"\)\s*=\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*"([^"]*)""#)
            .unwrap();
    let dep_re = Regex::new(r"\{([0-9a-fA-F-]+)\}\s*=\s*\{([0-9a-fA-F-]+)\}").unwrap();
    let proj_line_re =
        Regex::new(r#"Project\("[^"]*"\)\s*=\s*"[^"]+"\s*,\s*"[^"]+"\s*,\s*"\{([^}]+)\}""#)
            .unwrap();

    let mut guid_to_nid: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for cap in project_re.captures_iter(&src) {
        let proj_name = cap[1].to_string();
        let proj_path = cap[2].replace('\\', "/");
        let proj_guid = cap[3].trim_matches(|c| c == '{' || c == '}').to_string();

        // A solution folder is a virtual grouping (name == path, no real file):
        // key its id off the folder name, never a resolved filesystem path.
        let abs_proj = if proj_path == proj_name {
            proj_name.clone()
        } else {
            resolve_abs(parent, &proj_path)
        };
        let proj_nid = make_id([abs_proj.as_str()]);
        if !proj_nid.is_empty() && seen.insert(proj_nid.clone()) {
            nodes.push(code_node(&proj_nid, &proj_name, &abs_proj));
            edges.push(dotnet_edge(&file_nid, &proj_nid, "contains", &str_path));
        }
        if !proj_guid.is_empty() {
            guid_to_nid.insert(proj_guid.to_lowercase(), proj_nid);
        }
    }

    let mut in_dep_section = false;
    let mut current_guid: Option<String> = None;
    for line in src.lines() {
        if let Some(c) = proj_line_re.captures(line) {
            current_guid = Some(c[1].to_lowercase());
            continue;
        }
        if line.trim() == "EndProject" {
            current_guid = None;
            continue;
        }
        if line.contains("ProjectSection(ProjectDependencies)") {
            in_dep_section = true;
            continue;
        }
        if in_dep_section && line.contains("EndProjectSection") {
            in_dep_section = false;
            continue;
        }
        if in_dep_section {
            if let Some(cur) = &current_guid {
                if let Some(c) = dep_re.captures(line) {
                    let to_guid = c[1].to_lowercase();
                    if let (Some(from), Some(to)) =
                        (guid_to_nid.get(cur), guid_to_nid.get(&to_guid))
                    {
                        if from != to {
                            edges.push(dotnet_edge(from, to, "imports", &str_path));
                        }
                    }
                }
            }
        }
    }

    ExtractResult { nodes, edges }
}

// ── .slnx (XML solution) ────────────────────────────────────────────────────

fn extract_slnx(path: &Path, source: &[u8]) -> ExtractResult {
    let str_path = path.to_string_lossy().into_owned();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let (file_nid, file_map) = file_node(path, &str_path);
    let mut nodes = vec![file_map];
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(file_nid.clone());

    let text = String::from_utf8_lossy(source);
    let doc = match roxmltree::Document::parse(&text) {
        Ok(d) => d,
        Err(_) => return ExtractResult { nodes, edges },
    };

    let mut project_nids: HashSet<String> = HashSet::new();
    // First pass: project nodes (Project elements anywhere in the tree).
    for proj in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Project")
    {
        let Some(proj_path) = proj.attribute("Path") else {
            continue;
        };
        let abs = resolve_abs(parent, proj_path);
        let nid = make_id([abs.as_str()]);
        if nid.is_empty() {
            continue;
        }
        if seen.insert(nid.clone()) {
            let label = Path::new(&proj_path.replace('\\', "/"))
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            nodes.push(code_node(&nid, &label, &abs));
            edges.push(dotnet_edge(&file_nid, &nid, "contains", &str_path));
        }
        project_nids.insert(nid);
    }
    // Second pass: build-order dependencies between known projects.
    for proj in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Project")
    {
        let Some(proj_path) = proj.attribute("Path") else {
            continue;
        };
        let from_nid = make_id([resolve_abs(parent, proj_path).as_str()]);
        for dep in proj
            .descendants()
            .filter(|n| n.tag_name().name() == "BuildDependency")
        {
            let Some(dep_path) = dep.attribute("Project") else {
                continue;
            };
            let to_nid = make_id([resolve_abs(parent, dep_path).as_str()]);
            if !from_nid.is_empty()
                && !to_nid.is_empty()
                && from_nid != to_nid
                && project_nids.contains(&to_nid)
            {
                edges.push(dotnet_edge(&from_nid, &to_nid, "imports", &str_path));
            }
        }
    }

    ExtractResult { nodes, edges }
}

// ── .csproj / .fsproj / .vbproj (MSBuild project) ───────────────────────────

fn extract_csproj(path: &Path, source: &[u8]) -> ExtractResult {
    let str_path = path.to_string_lossy().into_owned();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let (file_nid, file_map) = file_node(path, &str_path);
    let mut nodes = vec![file_map];
    let mut edges: Vec<Attrs> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(file_nid.clone());

    let text = String::from_utf8_lossy(source);
    let doc = match roxmltree::Document::parse(&text) {
        Ok(d) => d,
        Err(_) => return ExtractResult { nodes, edges },
    };
    let root = doc.root_element();

    let add_framework =
        |fw: &str, nodes: &mut Vec<Attrs>, edges: &mut Vec<Attrs>, seen: &mut HashSet<String>| {
            let fw = fw.trim();
            if fw.is_empty() {
                return;
            }
            let nid = make_id(["framework", fw]);
            if !nid.is_empty() && seen.insert(nid.clone()) {
                nodes.push(concept_node(&nid, fw, &str_path));
                edges.push(dotnet_edge(&file_nid, &nid, "references", &str_path));
            }
        };

    for el in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "TargetFramework")
    {
        if let Some(t) = el.text() {
            add_framework(t, &mut nodes, &mut edges, &mut seen);
        }
    }
    for el in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "TargetFrameworks")
    {
        if let Some(t) = el.text() {
            for fw in t.split(';') {
                add_framework(fw, &mut nodes, &mut edges, &mut seen);
            }
        }
    }

    for pkg in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "PackageReference")
    {
        let name = pkg
            .attribute("Include")
            .or_else(|| pkg.attribute("include"))
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let version = pkg
            .attribute("Version")
            .or_else(|| pkg.attribute("version"))
            .unwrap_or("");
        let nid = make_id(["nuget", name]);
        let label = if version.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({version})")
        };
        if !nid.is_empty() && seen.insert(nid.clone()) {
            nodes.push(code_node(&nid, &label, &str_path));
        }
        edges.push(dotnet_edge(&file_nid, &nid, "imports", &str_path));
    }

    for proj in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "ProjectReference")
    {
        let ref_path = proj
            .attribute("Include")
            .or_else(|| proj.attribute("include"))
            .unwrap_or("");
        if ref_path.is_empty() {
            continue;
        }
        let ref_norm = ref_path.replace('\\', "/");
        let abs_ref = resolve_abs(parent, &ref_norm);
        let nid = make_id([abs_ref.as_str()]);
        let label = Path::new(&ref_norm)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !nid.is_empty() && seen.insert(nid.clone()) {
            nodes.push(code_node(&nid, &label, &abs_ref));
        }
        edges.push(dotnet_edge(&file_nid, &nid, "imports", &str_path));
    }

    if let Some(sdk) = root.attribute("Sdk") {
        if !sdk.is_empty() {
            let nid = make_id(["sdk", sdk]);
            if !nid.is_empty() && seen.insert(nid.clone()) {
                nodes.push(concept_node(&nid, sdk, &str_path));
                edges.push(dotnet_edge(&file_nid, &nid, "references", &str_path));
            }
        }
    }

    ExtractResult { nodes, edges }
}

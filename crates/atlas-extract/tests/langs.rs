//! M2 wave-1 gate: our extractor must reproduce graphify's built `graph.json`
//! for one sample file per language, compared as SETS of path-normalized
//! attribute maps (key order and the `_origin` field ignored).
//!
//! Oracle fixtures in `tests/fixtures/sample_<lang>.json` were produced by the
//! graphify venv on the sample file copied ALONE into a temp dir, then read from
//! `graphify-out/graph.json` (the built graph, which collapses parallel edges by
//! `(source,target,relation)` and same-id nodes — our extractor mirrors this).
//!
//! Path-derived ids differ between the oracle (temp dir) and our run (absolute
//! fixture path). `canon` neutralizes them with per-side prefix maps:
//!   * FILE — the file-node stem prefix (symbols keyed off the file).
//!   * DIR  — the JS/TS import sibling-dir prefix / the Go package-scope prefix.
//! JS/TS/Go oracles were generated in FIXED temp dirs so their DIR prefix is a
//! stable constant (`tmp_atlas_ora_js` / `tmp_atlas_ora_ts` / `atlas_ora_go`).
//!
//! Per-language residual deltas vs graphify are documented at each test.

use atlas_core::ids::{file_stem, make_id};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const GFIX: &str = "/home/yoshirakou/work/graphify/tests/fixtures";

/// A prefix rewrite: any id equal to `raw` becomes `token`; any id starting with
/// `raw_` becomes `token_<rest>`.
struct Remap {
    maps: Vec<(String, &'static str)>, // sorted longest-raw first
    basename_source_file: bool,
}

impl Remap {
    fn new(mut maps: Vec<(String, &'static str)>) -> Self {
        maps.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Remap {
            maps,
            basename_source_file: true,
        }
    }
    fn id(&self, s: &str) -> String {
        for (raw, token) in &self.maps {
            if s == raw {
                return token.to_string();
            }
            if let Some(rest) = s.strip_prefix(&format!("{raw}_")) {
                return format!("{token}_{rest}");
            }
        }
        s.to_string()
    }
}

fn basename(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        Path::new(s)
            .file_name()
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.to_string())
    }
}

fn canon(m: &Map<String, Value>, r: &Remap) -> String {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in m {
        if k == "_origin" || k == "metadata" || k == "type" {
            continue;
        }
        let nv = match (k.as_str(), v.as_str()) {
            ("id" | "source" | "target", Some(s)) => Value::String(r.id(s)),
            ("source_file", Some(s)) if r.basename_source_file => Value::String(basename(s)),
            _ => v.clone(),
        };
        out.insert(k.clone(), nv);
    }
    serde_json::to_string(&out).unwrap()
}

fn canon_set(items: &[Value], r: &Remap) -> Vec<String> {
    let mut v: Vec<String> = items
        .iter()
        .map(|it| canon(it.as_object().unwrap(), r))
        .collect();
    v.sort();
    v
}

fn diff(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

/// `my_extra` / `oracle_extra`: additional (raw_prefix, token) DIR maps.
fn check(
    fixture: &str,
    src_path: &str,
    oracle_json: &str,
    my_extra: Vec<(String, &'static str)>,
    oracle_extra: Vec<(String, &'static str)>,
) {
    let got = atlas_extract::extract_file(src_path).expect("extract");

    let my_file_nid = make_id([file_stem(Path::new(src_path)).as_str()]);
    // Oracle relativizes the file id to the bare-filename stem.
    let oracle_file_nid = make_id([file_stem(Path::new(&format!("sample.{fixture}"))).as_str()]);

    let mut my_maps = vec![(my_file_nid, "FILE")];
    my_maps.extend(my_extra);
    let mut or_maps = vec![(oracle_file_nid, "FILE")];
    or_maps.extend(oracle_extra);
    let my_r = Remap::new(my_maps);
    let or_r = Remap::new(or_maps);

    let fixture_json = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{oracle_json}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");
    let oracle: Value = serde_json::from_str(&fixture_json).expect("parse fixture");

    let my_nodes: Vec<Value> = got.nodes.into_iter().map(Value::Object).collect();
    let my_edges: Vec<Value> = got.edges.into_iter().map(Value::Object).collect();

    let want_nodes = canon_set(oracle["nodes"].as_array().unwrap(), &or_r);
    let got_nodes = canon_set(&my_nodes, &my_r);
    assert_eq!(
        got_nodes,
        want_nodes,
        "NODES mismatch for {oracle_json}\nmissing (oracle, not ours): {:?}\nextra (ours): {:?}",
        diff(&want_nodes, &got_nodes),
        diff(&got_nodes, &want_nodes)
    );

    let want_edges = canon_set(oracle["edges"].as_array().unwrap(), &or_r);
    let got_edges = canon_set(&my_edges, &my_r);
    assert_eq!(
        got_edges,
        want_edges,
        "EDGES mismatch for {oracle_json}\nmissing (oracle, not ours): {:?}\nextra (ours): {:?}",
        diff(&want_edges, &got_edges),
        diff(&got_edges, &want_edges)
    );
}

/// JavaScript. Sample is our own ESM analog of `sample.ts` (graphify ships no
/// `sample.js`). Import targets resolve `./models` against the file's dir.
/// EXACT match — no residual deltas for this fixture. Out of scope generally:
/// arrow functions, `this.x = () => {}` capture, CJS `require`, dynamic import,
/// TS-style type references (JS has none), INFERRED indirect_call callbacks.
#[test]
fn javascript_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/jsmod/sample.js",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "js",
        &src,
        "sample_js",
        vec![(dir, "DIR")],
        vec![("tmp_atlas_ora_js".into(), "DIR")],
    );
}

/// TypeScript. EXACT match for this fixture. Note graphify's generic engine
/// emits NO type-reference edges for TS/JS (unlike Python/Java/C/C++), so param
/// and return type annotations are intentionally not extracted. Out of scope:
/// TS namespaces/modules, decorators, `.tsx` (TSX grammar), constructor
/// parameter-property type table, everything listed under JS above.
#[test]
fn typescript_matches_oracle() {
    let src = format!("{GFIX}/sample.ts");
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "ts",
        &src,
        "sample_ts",
        vec![(dir, "DIR")],
        vec![("tmp_atlas_ora_ts".into(), "DIR")],
    );
}

/// Go. Types/methods key off the package scope (parent dir name → DIR); free
/// functions and the file node key off the stem (FILE). EXACT match: struct
/// fields (references), struct/interface embedding (embeds), method receiver
/// typing, param/return type references, and in-file calls all reproduced.
/// Out of scope: cross-file/package call resolution (single file only).
#[test]
fn go_matches_oracle() {
    let src = format!("{GFIX}/sample.go");
    let pkg = make_id([Path::new(&src)
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .as_ref()]);
    check(
        "go",
        &src,
        "sample_go",
        vec![(pkg, "DIR")],
        vec![("atlas_ora_go".into(), "DIR")],
    );
}

/// Rust. All type/method/free-fn ids key off the stem (FILE); external type
/// refs are sourceless stubs (bare ids, path-independent). EXACT match: structs,
/// enums (variant-payload field refs), traits (bound → inherits), impl blocks
/// (methods + `impl Trait for T` → implements), tuple structs, generic-arg refs,
/// `use` imports, and in-file calls. Out of scope: cross-file resolution.
#[test]
fn rust_matches_oracle() {
    let src = format!("{GFIX}/sample.rs");
    check("rs", &src, "sample_rs", vec![], vec![]);
}

/// Java. EXACT match: classes/interfaces/enums/records, extends→inherits,
/// implements, enum constants→case_of, `@Override`→references(attribute),
/// param/return/field type refs (generics as generic_arg), imports (last
/// segment), and in-file direct calls. Member calls (`items.add`) defer to the
/// receiver-typed resolver (out of scope) and emit no edge — matching the
/// oracle. Out of scope: object_creation to in-file types, nested-type
/// containment metadata, receiver typing.
#[test]
fn java_matches_oracle() {
    let src = format!("{GFIX}/sample.java");
    check("java", &src, "sample_java", vec![], vec![]);
}

/// C. EXACT match: functions (declarator-unwrapped names), `#include`→imports
/// (basename stem), user-typedef return/param type refs (deduped by build to one
/// edge per (src,tgt,relation)), and in-file calls. No classes in C.
#[test]
fn c_matches_oracle() {
    let src = format!("{GFIX}/sample.c");
    check("c", &src, "sample_c", vec![], vec![]);
}

/// C++. EXACT match: classes/structs, base_class_clause→inherits (+ template
/// args as generic_arg), methods, data members (references type + defines
/// field node), param/return type refs (qualified `std::string`→`string`),
/// `#include`→imports, and in-file/member calls. Out of scope: out-of-class
/// method definitions, local-var receiver typing.
#[test]
fn cpp_matches_oracle() {
    let src = format!("{GFIX}/sample.cpp");
    check("cpp", &src, "sample_cpp", vec![], vec![]);
}

/// Ruby. EXACT match: classes (`contains`), methods (`.name()`→`method`), free
/// functions (`contains`), `class X < Y`→inherits, and in-file direct calls.
#[test]
fn ruby_matches_oracle() {
    let src = format!("{GFIX}/sample.rb");
    check("rb", &src, "sample_ruby", vec![], vec![]);
}

/// Kotlin (tree-sitter-kotlin-ng 1.1.0, matching the oracle grammar). EXACT
/// match: classes/objects/interfaces, methods, `: Base()`→inherits vs
/// `: Iface`→implements, delegation generic args→generic_arg, property/param/
/// return type refs, enum entries→case_of, and in-file calls.
#[test]
fn kotlin_matches_oracle() {
    let src = format!("{GFIX}/sample.kt");
    check("kt", &src, "sample_kotlin", vec![], vec![]);
}

/// Scala (tree-sitter-scala 0.26.0, matching the oracle grammar). EXACT match:
/// classes/objects, `extends`→inherits + each `with`→mixes_in, class-parameter
/// and val/var field type refs, param/return type refs, `import`→imports, and
/// in-file calls.
#[test]
fn scala_matches_oracle() {
    let src = format!("{GFIX}/sample.scala");
    check("scala", &src, "sample_scala", vec![], vec![]);
}

/// C#. EXACT match: classes/interfaces/enums/structs/records, namespaces
/// (`csharp_namespace:` ids), base list (inherits/implements via interface
/// pre-scan + `I`-prefix heuristic), field/property/param/return type refs
/// (generics as generic_arg), `using`→imports, and in-file direct calls. Member
/// calls with a captured receiver defer to receiver-typed resolution (out of
/// scope) and emit no edge — matching the oracle. graphify-internal node/edge
/// `metadata` and `type` are ignored by `canon`.
#[test]
fn csharp_matches_oracle() {
    let src = format!("{GFIX}/sample.cs");
    check("cs", &src, "sample_cs", vec![], vec![]);
}

/// PHP. EXACT match: classes, methods/free functions, extends→inherits,
/// implements→implements, `use Trait`→mixes_in, property/promoted-param/param/
/// return type refs, `use` imports (last segment), `$this->m()` in-file calls.
/// A `use FQN` import whose bare name is referenced in-file repoints its
/// sourceless stub to an FQN-labeled stub (`_resolve_php_type_references`,
/// use-alias branch).
#[test]
fn php_matches_oracle() {
    let src = format!("{GFIX}/sample.php");
    check("php", &src, "sample_php", vec![], vec![]);
}

/// Swift. EXACT match: classes/protocols/structs/enums/actors, base conformance
/// via protocol/class pre-scan (inherits vs implements), extensions collapsing
/// onto the extended type, init/deinit/subscript methods, property/param/return
/// type refs, enum cases→case_of (+ associated-value type refs), `import`→module
/// anchor node + imports edge, and in-file direct + constructor calls.
#[test]
fn swift_matches_oracle() {
    let src = format!("{GFIX}/sample.swift");
    check("swift", &src, "sample_swift", vec![], vec![]);
}

/// Lua. graphify's `sample.luau` (tree-sitter-lua ignores the type annotations).
/// EXACT match: all functions are top-level `contains`, and the in-file
/// `Server.new(...)` call inside `main` reproduces the one `calls` edge. Method
/// calls (`s:start()`) resolve to no callee — matching the oracle.
#[test]
fn lua_matches_oracle() {
    let src = format!("{GFIX}/sample.luau");
    check("luau", &src, "sample_lua", vec![], vec![]);
}

/// Bash (standalone extractor). Sample is graphify's `sample.sh`. EXACT match:
/// file + `__entry` nodes, functions (`bash_function`), program-level var
/// `defines`, and cross-function `calls`. The `source ./helpers.sh` emits no edge
/// because helpers.sh is absent on disk — matching the oracle's existence gate.
#[test]
fn bash_matches_oracle() {
    let src = format!("{GFIX}/sample.sh");
    check("sh", &src, "sample_bash", vec![], vec![]);
}

/// Elixir (standalone extractor). Sample is graphify's `sample.ex`. EXACT match:
/// module (`contains`), functions (`method`), aliases/import (including the
/// `Foo.{Bar, Baz}` multi-alias form), and the in-file `create→validate` call.
/// Member calls resolve to no in-file label and emit no edge, matching the oracle.
#[test]
fn elixir_matches_oracle() {
    let src = format!("{GFIX}/sample.ex");
    check("ex", &src, "sample_elixir", vec![], vec![]);
}

/// Zig (standalone extractor, tree-sitter-zig 1.1.2 matching the oracle grammar).
/// Sample is graphify's `sample.zig`. EXACT match: file node, struct/enum/union
/// type nodes (`contains`), struct methods (`.distance()`→`method`), free
/// functions (`contains`), `@import("std")`→`imports_from` (deduped to one std
/// edge; the `std.mem` second import resolves to the same std target), and the
/// two in-file `calls` (`main`→`add`, `main`→`multiply`). Out of scope
/// (single-file): member calls (`std.math.sqrt`) resolve to no in-file label and
/// emit no edge — matching the oracle. Struct fields / enum cases are not nodes
/// (graphify's zig extractor emits none).
#[test]
fn zig_matches_oracle() {
    let src = format!("{GFIX}/sample.zig");
    check("zig", &src, "sample_zig", vec![], vec![]);
}

/// PowerShell (standalone extractor, tree-sitter-powershell 0.26.4 matching the
/// oracle grammar). Sample is graphify's `sample.ps1`. EXACT match: functions
/// (`contains`), classes (`contains`), class methods (`.Transform()`→`method`),
/// `Circle : Shape`→`inherits`, property/param/return type refs to sourceless
/// stubs (`string`/`void`/`double`, `references`), `using`→`imports_from`
/// (`System.IO`→`io`, `MyModule`→`mymodule`), and the `Get-Data`→`Process-Items`
/// in-file `calls`. Out of scope: `.psd1` manifest extraction (not dispatched),
/// cross-file dot-source/Import-Module resolution.
#[test]
fn powershell_matches_oracle() {
    let src = format!("{GFIX}/sample.ps1");
    check("ps1", &src, "sample_powershell", vec![], vec![]);
}

/// Objective-C (standalone extractor, tree-sitter-objc 3.0.2 matching the oracle
/// grammar). Sample is graphify's `sample.m`. EXACT match: `@interface`/
/// `@implementation` class nodes + `@protocol` nodes (`contains`), `: NSObject`→
/// `inherits`, `<SampleDelegate>`/`<Base>` adoption→`implements`, methods
/// (`-speak`/`-fetch`, sigil-prefixed labels→`method`), `NSString` property→
/// `references`/field, `#import`→`imports`/import (dangling stub targets), the
/// same-file selector-suffix `[self speak]`→`calls`, and the self/super
/// member-send resolver folded in single-file (`initWithName`→`Animal` and
/// `fetch`→`Dog` as `references`/call with `confidence_score`). Out of scope
/// (cross-file resolver / god-node guard): `@selector(...)` refs, capitalized-
/// receiver and local-var-typed (`Foo *f; [f m]`) sends, and full quoted-`#import`
/// path resolution beyond a same-dir on-disk check.
#[test]
fn objc_matches_oracle() {
    let src = format!("{GFIX}/sample.m");
    check("m", &src, "sample_objc", vec![], vec![]);
}

/// Julia (tree-sitter-julia 0.23.1, matching the oracle grammar). EXACT match:
/// module (`defines`), abstract type + structs (`<:` → inherits, `name::Type`
/// fields → references[field]), functions and short-form `f(x)=...` (`defines`,
/// label `name()`), `using`/`import` (bare / scoped `Base.Threads` / relative
/// `..ParentModule` / selected `import Base: show` → imports), and in-file direct
/// + `obj.method()` calls. Calls to undefined names (`norm`, `show`) stay
/// dangling with the file-stem prefix (single-file scope) — the oracle keeps the
/// temp-dir stem, mapped to FILE via oracle_extra.
#[test]
fn julia_matches_oracle() {
    let src = format!("{GFIX}/sample.jl");
    check(
        "jl",
        &src,
        "sample_julia",
        vec![],
        vec![("tmp_atlas_ora_jl_sample".into(), "FILE")],
    );
}

/// Fortran (tree-sitter-fortran 0.6.0, matching the oracle grammar). Fixture is a
/// plain lowercase `.f90` (NO cpp preprocessing), so the oracle line anchors are
/// clean and we match exactly — #2092 (cpp -P line renumbering on `.F90`) does
/// NOT apply here. EXACT match: program/module (`defines`), derived types
/// (`defines`), subroutines/functions (`defines`, label `name()`), `use`
/// (`imports`), `type(T)` parameter/result declarations → references[parameter_
/// type|return_type], and in-file `call foo` + `x = foo(...)` calls (the latter
/// only when `foo` is a defined procedure, so array indexing can't fake a call).
/// #2092 status: N/A for this plain `.f90` fixture; a `.F90` path would route
/// through atlas WITHOUT cpp and diverge from graphify's cpp-renumbered anchors
/// (documented gap — atlas does not shell out to cpp).
#[test]
fn fortran_matches_oracle() {
    let src = format!("{GFIX}/sample.f90");
    check("f90", &src, "sample_fortran", vec![], vec![]);
}

/// Dart (regex-based extractor, matching graphify's regex oracle — graphify does
/// NOT use tree-sitter for Dart). Fixture is atlas-owned plain Dart. EXACT match:
/// classes/mixins (`defines`), extends/on → inherits, `with` → mixes_in,
/// `implements` → implements, extensions (`defines` + extends), top-level/member
/// vars (`defines` + variable-type references), methods (`defines`), and
/// import/export. Bare base/mixin/interface stubs collapse onto the real stem-
/// keyed defs via the shared in-file rewire. DELTA (documented in src/dart.rs):
/// Flutter/Bloc/Riverpod/navigation in-body heuristics, `@annotation` configures,
/// and the generic-call `word<Type>(` pass are NOT ported (no Flutter idioms in
/// this fixture — output is byte-identical to the oracle regardless).
#[test]
fn dart_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.dart", env!("CARGO_MANIFEST_DIR"));
    check("dart", &src, "sample_dart", vec![], vec![]);
}

// ── Bash backlog #2141: calls to functions defined in a sourced file ─────────
//
// `sourced/main.sh` does `source ./helpers.sh` then calls `greet` — a function
// defined ONLY in helpers.sh. Resolving that call needs cross-file resolution,
// out of atlas's single-file extract scope (and current graphify — the oracle —
// drops it too).

fn edges_by_relation<'a>(edges: &'a [atlas_core::Attrs], rel: &str) -> Vec<&'a atlas_core::Attrs> {
    edges
        .iter()
        .filter(|e| e.get("relation").and_then(Value::as_str) == Some(rel))
        .collect()
}

/// REGRESSION (documents #2141 / current behavior): extracting `main.sh` alone —
/// with `helpers.sh` present on disk — emits the `source` `imports_from` edge but
/// NO cross-function `calls` edge for `greet`. This matches the oracle.
#[test]
fn bash_2141_sourced_call_regression() {
    let src = format!(
        "{}/tests/fixtures/sourced/main.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let got = atlas_extract::extract_file(&src).expect("extract");

    assert!(
        !edges_by_relation(&got.edges, "imports_from").is_empty(),
        "expected a `source ./helpers.sh` imports_from edge"
    );
    assert!(
        edges_by_relation(&got.edges, "calls").is_empty(),
        "#2141: sourced-function call should NOT resolve in single-file scope (matches oracle), got: {:?}",
        edges_by_relation(&got.edges, "calls")
    );
}

/// DESIRED POST-FIX behavior for #2141 (cross-file resolution — OUT OF SCOPE for
/// single-file extraction, hence `#[ignore]`).
#[test]
#[ignore = "#2141: requires cross-file resolution; out of atlas single-file extract scope"]
fn bash_2141_desired_postfix_behavior() {
    let src = format!(
        "{}/tests/fixtures/sourced/main.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let got = atlas_extract::extract_file(&src).expect("extract");
    let has_greet_call = edges_by_relation(&got.edges, "calls").iter().any(|e| {
        e.get("target")
            .and_then(Value::as_str)
            .map(|t| t.contains("greet"))
            .unwrap_or(false)
    });
    assert!(
        has_greet_call,
        "desired: `run` should call sourced `greet` (needs cross-file resolution)"
    );
}

/// Lua `require()` import resolution (not exercised by `sample.luau`). A
/// `require("some.module")` with no file on disk falls back to
/// `make_id("some.module")` → `some_module`, emitting a file `imports` edge.
#[test]
fn lua_require_import() {
    use std::io::Write;
    let dir = format!("{}/target/tmp_lua_require", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let src = format!("{dir}/mod.lua");
    let mut f = std::fs::File::create(&src).unwrap();
    write!(
        f,
        "local dep = require(\"some.module\")\n\nlocal function go()\n  dep.run()\nend\n"
    )
    .unwrap();
    drop(f);

    let got = atlas_extract::extract_file(&src).expect("extract");
    let imports = edges_by_relation(&got.edges, "imports");
    assert!(
        imports
            .iter()
            .any(|e| e.get("target").and_then(Value::as_str) == Some("some_module")),
        "expected imports edge to `some_module`, got: {:?}",
        imports
    );
    let _ = std::fs::remove_dir_all(&dir);
}

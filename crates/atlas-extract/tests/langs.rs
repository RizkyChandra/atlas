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

/// Groovy (engine-config, tree-sitter-groovy 0.1.2 matching the oracle grammar).
/// graphify routes `.groovy`/`.gradle` through `_GROOVY_CONFIG` (`_extract_generic`)
/// and shares the Java extends/implements/annotation branch (engine.py `ts_module
/// in (java, groovy)`) but NOT Java's param/return/field type-ref emission, so the
/// oracle carries inherits/implements only — no `references` edges. EXACT match:
/// classes/interfaces (`contains`), constructors + methods (`.name()`→`method`),
/// `extends`→inherits, `implements`→implements, `import`→imports (last segment),
/// and the in-file `processor.reset()` call — resolved by bare method name to the
/// last-writer `reset` node (member calls are NEVER deferred for Groovy: the
/// config's call-accessor set is empty, so the callee is read from the `name`
/// field and no receiver is captured). Sample is graphify's `sample.groovy`.
/// GAP (not ported): the Spock regex fallback (`def "feature"()` spec methods —
/// graphify's `_extract_spock_fallback`); such files fall through to the plain
/// tree-sitter pass here. This fixture is not a Spock spec, so it is unaffected.
#[test]
fn groovy_matches_oracle() {
    let src = format!("{GFIX}/sample.groovy");
    check("groovy", &src, "sample_groovy", vec![], vec![]);
}

/// SQL (standalone extractor, tree-sitter-sequel 0.3.11 = DerekStride's
/// tree-sitter-sql 0.3.11, matching the oracle grammar). Object ids key off the
/// file stem (FILE). EXACT match: tables (`create_table`→`contains`), FK inline
/// `REFERENCES`→references, view (`create_view`→`contains`, `FROM`→reads_from),
/// function (`create_function`→`contains`, label `name()`). Sample is graphify's
/// `sample.sql`. The PL/pgSQL function body parses without FROM/JOIN clause nodes
/// (dollar-quoted body), so `get_user` emits no reads_from — matching the oracle.
/// GAP (not ported, documented in src/sql.rs): the dialect ERROR-recovery regex
/// paths — PL/pgSQL `ERROR` CREATE FUNCTION/PROCEDURE scan and Firebird
/// `fb_proc_or_trigger`/`set_term`/`declare_external_function`. The global CREATE
/// TABLE ... REFERENCES regex sweep IS ported. This fixture parses cleanly (no
/// ERROR nodes), so the un-ported fallbacks don't fire.
#[test]
fn sql_matches_oracle() {
    let src = format!("{GFIX}/sample.sql");
    check("sql", &src, "sample_sql", vec![], vec![]);
}

/// Terraform/HCL (standalone extractor, tree-sitter-hcl 1.1.0; oracle grammar is
/// the same-major PyPI 1.2.0 — node names identical). Block ids scope by the
/// parent DIRECTORY name (→DIR), like Go; the file node keys off the stem (FILE).
/// EXACT match: resource/data/module/variable/output/locals blocks (`contains`),
/// interpolation `references` (`var.`/`local.`/`data.x.y`/`aws_instance.web`),
/// and `depends_on`. Fixture is atlas-owned (graphify ships no `sample.tf`);
/// oracle generated in a fixed temp dir `atlas_ora_tf` so its DIR prefix is a
/// stable constant. Out of scope: `provider`/`terraform` meta-arg heads
/// (count/each/self/path/terraform) are filtered, matching graphify.
#[test]
fn terraform_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.tf", env!("CARGO_MANIFEST_DIR"));
    let dir = make_id([Path::new(&src)
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .as_ref()]);
    check(
        "tf",
        &src,
        "sample_tf",
        vec![(dir, "DIR")],
        vec![("atlas_ora_tf".into(), "DIR")],
    );
}

/// Verilog / SystemVerilog (tree-sitter-verilog 1.0.3, matching the oracle
/// grammar) + regex class augmentation. Fixture is graphify's `sample.sv`. EXACT
/// match: modules (`defines`), functions (`add()`)/tasks (`contains`),
/// `import math_pkg::*`→`imports_from`, `leaf u_leaf()`→`instantiates` (bare
/// sourced `leaf` node distinct from the defined `sample_leaf` module), and the
/// SystemVerilog class pass — class nodes (`defines`), `extends`→`inherits`,
/// `implements`→`implements`, field/return type refs (`Result`/`Config`/
/// `BaseProcessor`, generics like `Payload` as `generic_arg`), and the `build`
/// method. The `build(Payload input)` parameter_type ref to `Payload` collapses
/// onto the earlier generic_arg ref by `(src,tgt,relation)` dedupe — matching the
/// oracle. Nodes/edges carry `confidence_score: 1.0`. Out of scope: cross-file
/// module/package resolution.
#[test]
fn verilog_matches_oracle() {
    let src = format!("{GFIX}/sample.sv");
    check("sv", &src, "sample_verilog", vec![], vec![]);
}

/// Pascal / Delphi (regex extractor — the Rust `tree-sitter-pascal` crate is
/// 0.10.2 vs the oracle venv's 0.11.0, so per the milestone rules we take
/// graphify's sanctioned regex fallback path). Fixture is graphify's
/// `sample.pas`. EXACT match: file→`contains`→unit, `uses`→`imports` (bare
/// `sysutils`/`classes` targets — cross-file unit resolution out of scope),
/// class/interface type nodes (`contains`), `TBaseProcessor(TObject)`→`inherits`
/// (bare sourced `tobject` stub) and `TDataProcessor(TBaseProcessor,IProcessor)`
/// →two `inherits`, method implementations (`method`, keyed to the IMPL line to
/// match the oracle), and the `Process→Reset` in-file `calls`. DELTA (documented
/// in src/pascal.rs): method nodes come from implementation headers only, so an
/// in-class method DECLARED but never IMPLEMENTED in-file (e.g. interface
/// methods) emits no node — exactly as the tree-sitter oracle does on this
/// grammar (the regex fallback's forward-decl nodes would otherwise over-emit
/// and land on the wrong line).
#[test]
fn pascal_matches_oracle() {
    let src = format!("{GFIX}/sample.pas");
    check("pas", &src, "sample_pascal", vec![], vec![]);
}

/// Apex `.cls` (regex extractor — no tree-sitter grammar on PyPI, matching
/// graphify). Fixture is graphify's `sample.cls`. EXACT match: outer class
/// (`contains`), nested interface/enum (`contains`), methods (`.name()`→
/// `method`, plus file-level INFERRED `contains` for `@AuraEnabled`/
/// `@InvocableMethod`), SOQL `FROM Account`→`uses` (INFERRED, deduped to one),
/// and DML `update`/`insert`/`delete`→`dml_<op>` `uses` (INFERRED). Note methods
/// bind to the enclosing class scope (`Notifiable.notify` attaches to
/// `AccountService`), matching graphify's flat current-class tracking.
#[test]
fn apex_cls_matches_oracle() {
    let src = format!("{GFIX}/sample.cls");
    check("cls", &src, "sample_apex_cls", vec![], vec![]);
}

/// Apex `.trigger` (regex extractor). Fixture is graphify's `sample.trigger`.
/// EXACT match: `trigger AccountTrigger on Account`→trigger node (`contains`) +
/// `uses` the `Account` SObject (INFERRED). The in-body `AccountService.xxx(...)`
/// calls are not method declarations and emit nothing — matching the oracle.
#[test]
fn apex_trigger_matches_oracle() {
    let src = format!("{GFIX}/sample.trigger");
    check("trigger", &src, "sample_apex_trigger", vec![], vec![]);
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

// ══════════════════════════════════════════════════════════════════════════
// merged from batch-h
// ══════════════════════════════════════════════════════════════════════════
/// TSX (`.tsx`). Same TS config, but parsed with the JSX-aware `language_tsx`
/// grammar (graphify `_TSX_CONFIG`) so the `{fmtDate(now)}` / `{fmtCount(42)}`
/// calls nested in JSX expression containers are seen — parsing `.tsx` with the
/// plain TypeScript grammar would drop them. EXACT match: functions (`contains`)
/// + those two in-file `calls`. Fixture is graphify's `sample.tsx`.
#[test]
fn tsx_matches_oracle() {
    let src = format!("{GFIX}/sample.tsx");
    check("tsx", &src, "sample_tsx", vec![], vec![]);
}

/// CUDA (`.cu`/`.cuh`). Reuses the C++ grammar/config (graphify routes `.cu` to
/// its cpp extractor). EXACT match: struct + fields, functions, param/return type
/// refs, and in-file calls. The CUDA qualifiers `__device__`/`__global__` parse
/// as the function return type and become sourceless `references`/return_type
/// stubs (`device`/`global`) — exactly as the shared cpp engine emits them.
/// Fixture is graphify's `sample.cu`.
#[test]
fn cuda_matches_oracle() {
    let src = format!("{GFIX}/sample.cu");
    check("cu", &src, "sample_cu", vec![], vec![]);
}

/// Metal (`.metal`). Also reuses the C++ grammar/config (graphify routes `.metal`
/// to its cpp extractor). EXACT match: struct + fields, functions, param type
/// refs, and the Metal address-space/qualifier keywords (`kernel`/`device`/
/// `constant`/`uint`) as sourceless return/parameter-type stubs — the shared cpp
/// engine's output. Fixture is graphify's `sample.metal`.
#[test]
fn metal_matches_oracle() {
    let src = format!("{GFIX}/sample.metal");
    check("metal", &src, "sample_metal", vec![], vec![]);
}

/// PowerShell `.psd1` module manifest (dedicated hashtable pass, graphify
/// `extract_powershell_manifest` — NOT the script extractor). EXACT match: the
/// file node plus one `imports_from` edge per module referenced by RootModule,
/// NestedModules, and RequiredModules (the latter following both bare strings and
/// the `@{ ModuleName = ... }` spec form). Module names are basename + extension
/// stripped, lowercased by `make_id` (`MyModule.psm1`→`mymodule`). Fixture is
/// graphify's `sample.psd1`.
#[test]
fn psd1_matches_oracle() {
    let src = format!("{GFIX}/sample.psd1");
    check("psd1", &src, "sample_psd1", vec![], vec![]);
}

/// Terraform `.tfvars` values file (same extractor as `.tf`). A tfvars file holds
/// only top-level attribute assignments, no blocks, so the graph is just the file
/// node (`source_location: null`, like every terraform file node) — matching
/// graphify, which routes `.tfvars` → the terraform extractor. Fixture is
/// atlas-owned (graphify ships no `.tfvars` sample).
#[test]
fn tfvars_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/sample.tfvars",
        env!("CARGO_MANIFEST_DIR")
    );
    check("tfvars", &src, "sample_tfvars", vec![], vec![]);
}

// ══════════════════════════════════════════════════════════════════════════
// merged from batch-i
// ══════════════════════════════════════════════════════════════════════════
/// Vue SFC (`.vue`). The `<script setup lang="ts">` body is parsed via the TS
/// engine on the mask-blanked file (non-script regions → spaces, newlines kept),
/// so symbol ids, contains/method/calls edges, and static imports come straight
/// from the shared JS/TS port. A regex pass recovers the template's dynamic
/// `import('./LazyWidget.vue')` as a `dynamic_import` edge + sourced stub node
/// (no `source_location`, matching graphify's rescue shape). EXACT match:
/// interface/function/const nodes, bare default `import axios` → `ref_axios`
/// `imports_from`, relative `./helper` → `imports_from` + named `imports`, in-file
/// `bump→greet` call, and the template dynamic import. Relative-import targets
/// key off the sibling-dir prefix (→DIR), like the JS test; oracle generated in a
/// fixed temp dir so its DIR prefix is a stable constant. Out of scope: TSX
/// grammar (`lang="tsx"` falls back to TS), tsconfig-alias/workspace resolution,
/// AST-level deferred dynamic imports inside `<script>` (kept out of the fixture).
#[test]
fn vue_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.vue", env!("CARGO_MANIFEST_DIR"));
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "vue",
        &src,
        "sample_vue",
        vec![(dir, "DIR")],
        vec![("tmp_tmp_atlas_ora_vue".into(), "DIR")],
    );
}

/// Svelte SFC (`.svelte`). The raw file is fed to the JS grammar (no masking); the
/// HTML markup makes the whole parse a top-level ERROR node, so the AST yields
/// only the file node and every import is recovered by the regex rescue: static
/// `import … from '…'` inside `<script>` (bare `svelte` → last-segment node;
/// relative `./store` → sibling-dir target) and the template's `{#await
/// import('./Modal.svelte')}` → `dynamic_import`. All rescue nodes/edges carry the
/// graphify shape (no `source_location`/`weight`/`context`). EXACT match. Out of
/// scope: any script symbol/call extraction (unreachable under the ERROR parse —
/// same as graphify), tsconfig-alias/workspace resolution, `.svelte.ts` runes.
#[test]
fn svelte_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/sample.svelte",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "svelte",
        &src,
        "sample_svelte",
        vec![(dir, "DIR")],
        vec![("tmp_tmp_atlas_ora_svelte".into(), "DIR")],
    );
}

/// Astro component (`.astro`). The raw file goes to the JS grammar: tree-sitter
/// recovers per-statement, so most frontmatter survives as AST (the `render()`
/// function → node, the `./greet` import → `imports_from` + named `imports`), but
/// the import on the line touching the opening `---` fence (`./Layout.astro`) is
/// swallowed by the ERROR region and recovered only by regex. The regex rescue
/// also handles the client `<script>`'s `canvas-confetti` and the template's
/// `import('./Heavy.astro')` dynamic import. Duplicate AST/regex `imports_from`
/// edges collapse via the build dedupe (keep-first → the AST edge with
/// context/location wins). EXACT match. Out of scope: tsconfig-alias/workspace
/// resolution, and (JS/TS engine gap, not SFC-specific) call-initialized
/// top-level `const`s — `const x = f()` emits no node — so the frontmatter uses a
/// `function` to exercise AST symbol recovery.
#[test]
fn astro_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.astro", env!("CARGO_MANIFEST_DIR"));
    let dir = make_id([Path::new(&src).parent().unwrap().to_string_lossy().as_ref()]);
    check(
        "astro",
        &src,
        "sample_astro",
        vec![(dir, "DIR")],
        vec![("tmp_tmp_atlas_ora_astro".into(), "DIR")],
    );
}

// ══════════════════════════════════════════════════════════════════════════
// merged from batch-j
// ══════════════════════════════════════════════════════════════════════════
/// Razor `.razor` (regex extractor — graphify's `razor.py`, no grammar). Fixture
/// is graphify's `sample.razor`. EXACT match: `@page` route (concept node +
/// `references`, no edge source_location), `@using`/`@inject`→`imports`,
/// `@inherits`→`inherits`, PascalCase component tags `<WeatherDisplay>`/
/// `<DataGrid>` and the generic-arg `List<CounterRecord>`→`calls` (HTML tags
/// filtered), and `@code` C# methods→`contains` (no edge source_location).
/// Symbols key off the file stem (FILE); directive/component targets are global
/// stubs. GAP: graphify's razor extractor handles `@code` only (not
/// `@functions`), so neither do we — matching the oracle.
#[test]
fn razor_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.razor", env!("CARGO_MANIFEST_DIR"));
    check("razor", &src, "sample_razor", vec![], vec![]);
}

/// Razor `.cshtml` (same extractor, dispatch coverage). Atlas-owned fixture.
/// EXACT match: `@model`→`references`, `@using`/`@inject`→`imports`, component
/// tags→`calls`.
#[test]
fn cshtml_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/sample.cshtml",
        env!("CARGO_MANIFEST_DIR")
    );
    check("cshtml", &src, "sample_cshtml", vec![], vec![]);
}

/// Blade `.blade.php` (regex extractor — graphify's `blade.py`, no grammar).
/// Compound extension is dispatched before the `.php` arm. Atlas-owned fixture.
/// EXACT match: `@include('a.b')`→`includes` (target id from `a/b`, label kept
/// dotted), `<livewire:x>`→`uses_component`, `wire:click="m"`→`binds_method`.
/// All edges carry `confidence_score: 1.0` and null source_location; nodes null
/// source_location. The file node keys off the stem (`sample.blade`→`sample_blade`,
/// FILE); component/include/method targets are global stubs.
#[test]
fn blade_matches_oracle() {
    let src = format!(
        "{}/tests/fixtures/sample.blade.php",
        env!("CARGO_MANIFEST_DIR")
    );
    check("blade.php", &src, "sample_blade", vec![], vec![]);
}

/// XAML `.xaml` (roxmltree DOM port of graphify's `extract_xaml`). Atlas-owned
/// fixture (no code-behind sibling, no ViewModel `.cs` — so the un-ported
/// cross-file paths stay dormant in the oracle too). EXACT match: file→root
/// `contains`, `x:Class`→class node + `references`(context `x_class`), named
/// elements (`x:Name`)→`contains` + `references`(context `type`) to `xaml_<type>`
/// concept nodes, `{Binding …}` paths→`references`(context `binding_path` /
/// `binding_command`), `{StaticResource …}` converters→`references`(context
/// `binding_converter`), and the direct `<Binding Path=… Converter=…>` element.
/// Element ids key off the stem (FILE); type/binding/converter nodes use global
/// `xaml`/`binding`/`binding_converter` prefixes. GAP (documented in src/xaml.rs,
/// all needing OTHER files — out of single-file scope): code-behind event-handler
/// wiring, ViewModel inference + project C# scan, and CommunityToolkit member
/// generation. The fixture omits event attributes so nothing is silently dropped.
#[test]
fn xaml_matches_oracle() {
    let src = format!("{}/tests/fixtures/sample.xaml", env!("CARGO_MANIFEST_DIR"));
    check("xaml", &src, "sample_xaml", vec![], vec![]);
}

// ══════════════════════════════════════════════════════════════════════════
// merged from batch-k
// ══════════════════════════════════════════════════════════════════════════
/// Like [`check`] but the oracle file was generated from a file whose basename
/// is NOT `sample.<ext>` (e.g. `package.json`, `widgets.lpk`). The oracle file
/// node id is derived from the source basename stem, which is identical on both
/// sides.
fn check_named(
    oracle_json: &str,
    src_path: &str,
    my_extra: Vec<(String, &'static str)>,
    oracle_extra: Vec<(String, &'static str)>,
) {
    let got = atlas_extract::extract_file(src_path).expect("extract");

    let my_file_nid = make_id([file_stem(Path::new(src_path)).as_str()]);
    let base = basename(src_path);
    let oracle_file_nid = make_id([file_stem(Path::new(&base)).as_str()]);

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

/// The parent dir of `src`, as a `make_id` prefix token (mirrors how graphify's
/// build derives the scan-relative id prefix). `up` = how many dirs to ascend.
fn dir_prefix(src: &str, up: usize) -> String {
    let mut p = Path::new(src).parent().unwrap();
    for _ in 0..up {
        p = p.parent().unwrap();
    }
    make_id([p.to_string_lossy().as_ref()])
}

fn fx(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

// ── M2 file-type coverage: .NET, JSON config, Pascal forms ───────────────────

/// .NET `.sln` (legacy text solution). Fixture is graphify's `sample.sln`.
/// EXACT match: file `contains` each project (path-keyed nodes), and the
/// `ProjectSection(ProjectDependencies)` GUID edge WebApi→Domain (`imports`).
/// Project ids key off the RESOLVED project path (INSIDE the scan root, so
/// graphify keeps the absolute-derived id) → neutralized by the SCAN remap
/// (oracle generated in the fixed dir `/tmp/atlas_ora_sln`). Solution folders
/// (virtual, no file) key off the folder name — none in this fixture.
#[test]
fn sln_matches_oracle() {
    let src = fx("sample.sln");
    check_named(
        "sample_sln",
        &src,
        vec![(dir_prefix(&src, 0), "SCAN")],
        vec![("tmp_atlas_ora_sln".into(), "SCAN")],
    );
}

/// .NET `.slnx` (XML solution). Fixture is graphify's `sample.slnx`. EXACT
/// match: file `contains` each `<Project Path=…>` (label = project file stem),
/// and `<BuildDependency>` WebApi→Domain (`imports`). Project ids resolve INSIDE
/// the scan root → SCAN remap (`/tmp/atlas_ora_slnx`).
#[test]
fn slnx_matches_oracle() {
    let src = fx("sample.slnx");
    check_named(
        "sample_slnx",
        &src,
        vec![(dir_prefix(&src, 0), "SCAN")],
        vec![("tmp_atlas_ora_slnx".into(), "SCAN")],
    );
}

/// .NET `.csproj` (MSBuild project). Fixture is graphify's `sample.csproj`.
/// EXACT match: `TargetFramework`→`framework_*` (`references`), `Sdk` attr→
/// `sdk_*` (`references`), each `PackageReference`→`nuget_*` (`imports`, label
/// `name (version)`), each `ProjectReference`→project node (`imports`). The two
/// `..\…\*.csproj` refs resolve OUTSIDE the scan root, so graphify `ext_`-
/// prefixes their ids — reproduced by mapping our resolved-parent prefix to
/// `ext`. `.fsproj`/`.vbproj` route through the same code path.
#[test]
fn csproj_matches_oracle() {
    let src = fx("sample.csproj");
    check_named(
        "sample_csproj",
        &src,
        vec![(dir_prefix(&src, 1), "ext")],
        vec![],
    );
}

/// Delphi `.dfm` form. Fixture is graphify's `sample.dfm` (text form). EXACT
/// match: file `contains` root form class, nested `object … : TClass` →
/// component nodes (`contains`, parent→child), and `OnXxx = Handler` → handler
/// nodes (`references`, context `event`). All ids key off the file stem (FILE).
/// Binary `.dfm` (FF 0A magic) is skipped — this fixture is text.
#[test]
fn dfm_matches_oracle() {
    check_named("sample_dfm", &fx("sample.dfm"), vec![], vec![]);
}

/// Lazarus `.lfm` form (same text grammar as `.dfm`). Fixture is graphify's
/// `sample.lfm`. EXACT match: component containment tree + `OnXxx` event handler
/// references. All ids key off the file stem (FILE).
#[test]
fn lfm_matches_oracle() {
    check_named("sample_lfm", &fx("sample.lfm"), vec![], vec![]);
}

/// Lazarus `.lpk` package (XML). Fixture is atlas-authored `widgets.lpk`
/// (graphify's `sample.lpk` names a unit `sample` that collides with the file
/// stem under graphify's build-time id relativization — a merge quirk atlas's
/// single-file pipeline does not reproduce; the collision-free fixture avoids
/// it). EXACT match: file `contains` package (`<Name>`→stem-keyed node),
/// package `imports` each `<PackageName>` dep (global id, context `import`),
/// package `contains` each `<UnitName>` unit (global bare id). DELTA: on-disk
/// unit→.pas resolution (graphify rglobs the project) is out of single-file
/// scope — units stay bare `make_id(unit_name)` (graphify's on-disk-miss
/// fallback), matching the oracle when no sibling `.pas` is present.
#[test]
fn lpk_matches_oracle() {
    check_named("widgets_lpk", &fx("widgets.lpk"), vec![], vec![]);
}

/// MCP config JSON, routed by filename to the MCP extractor. Fixture is atlas-
/// authored `claude_desktop_config.json` (the other MCP filenames — `mcp.json`,
/// `.mcp.json`, `mcp_servers.json` — route through the identical code path; this
/// name is used because a `mcp.json` stem collides with the global `mcp_*` id
/// prefix under the test's canon FILE remap, not in the extractor). EXACT match:
/// file `contains` each server (stem-keyed), server `references` command
/// (`mcp_command_*`, context `command`), server `references` package parsed from
/// args (`mcp_package_*`, context `package`), server `requires_env` each env var
/// (`env_var_*`, NAMES ONLY — values never read). Edges carry `confidence_score`.
/// Command/package/env ids are global (shared across configs); server ids key
/// off the file stem (FILE).
#[test]
fn mcp_config_matches_oracle() {
    check_named(
        "mcp_config",
        &fx("claude_desktop_config.json"),
        vec![],
        vec![],
    );
}

/// `package.json` (config/manifest JSON, recognized by filename). Fixture is
/// atlas-authored. EXACT match: file `contains` each top-level key (stem-keyed),
/// `dependencies`/`devDependencies` blocks `contains` each entry key, and each
/// dependency entry `imports` a global `concept` package node (context
/// `import`). Plain scalar keys (`name`, `version`) are key nodes only.
#[test]
fn package_json_matches_oracle() {
    check_named("package_json", &fx("package.json"), vec![], vec![]);
}

/// `tsconfig.json` (config JSON). Fixture is atlas-authored. EXACT match:
/// `extends` string → global `ref_*` concept anchored to the FILE (`extends`,
/// context `import`); `compilerOptions` nested object → key nodes (`contains`),
/// depth-first up to graphify's depth-6 / 500-pair caps.
#[test]
fn tsconfig_json_matches_oracle() {
    check_named("tsconfig_json", &fx("tsconfig.json"), vec![], vec![]);
}

/// Plain data JSON is deliberately SKIPPED (graphify #1224/#2107/#2108): a
/// `.json` that is neither an MCP config nor a recognized config/manifest (by
/// filename or top-level key probe) emits an EMPTY graph, so it does not explode
/// into orphan key-nodes. Fixture `data.json` (`{"users": […], "count": …}`)
/// has no recognized key → zero nodes/edges on both sides.
#[test]
fn data_json_skipped_matches_oracle() {
    check_named("data_json", &fx("data.json"), vec![], vec![]);
}

// ══════════════════════════════════════════════════════════════════════════
// merged from batch-l
// ══════════════════════════════════════════════════════════════════════════

// ─────────────────────────────────────────────────────────────────────────────
// BACKLOG new languages (M2): R, Nix, Solidity, Ada. graphify has NO extractor
// for these, so there is no oracle. The tests below are HAND-AUTHORED GOLDENS —
// they document the intended single-file extraction contract for each language.
// Node ids embed the (absolute) path stem, so we resolve nodes by label and
// assert edges structurally. External import/call targets are `make_id([name])`,
// which is path-independent, so those are asserted literally.

/// id of the sole node carrying `label`.
fn nid_of<'a>(nodes: &'a [atlas_core::Attrs], label: &str) -> String {
    nodes
        .iter()
        .find(|n| n.get("label").and_then(Value::as_str) == Some(label))
        .unwrap_or_else(|| panic!("no node labelled {label:?}"))
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string()
}

fn has_edge(edges: &[atlas_core::Attrs], src: &str, rel: &str, tgt: &str) -> bool {
    edges.iter().any(|e| {
        e.get("source").and_then(Value::as_str) == Some(src)
            && e.get("relation").and_then(Value::as_str) == Some(rel)
            && e.get("target").and_then(Value::as_str) == Some(tgt)
    })
}

/// Every edge must carry confidence EXTRACTED (the standalone-module contract).
fn all_extracted(edges: &[atlas_core::Attrs]) {
    assert!(edges
        .iter()
        .all(|e| e.get("confidence").and_then(Value::as_str) == Some("EXTRACTED")));
}

/// R — functions + `library()` imports (no classes/inheritance).
/// file → contains square()/compute(); file → imports dplyr; compute → calls square.
#[test]
fn r_backlog_golden() {
    let src = format!("{}/tests/fixtures/sample.R", env!("CARGO_MANIFEST_DIR"));
    let g = atlas_extract::extract_file(&src).expect("extract");
    let file = nid_of(&g.nodes, "sample.R");
    assert!(g.nodes.iter().any(
        |n| n.get("label").and_then(Value::as_str) == Some("sample.R")
            && n.get("source_location").unwrap().is_null()
    ));
    let square = nid_of(&g.nodes, "square()");
    let compute = nid_of(&g.nodes, "compute()");
    assert!(has_edge(&g.edges, &file, "contains", &square));
    assert!(has_edge(&g.edges, &file, "contains", &compute));
    assert!(has_edge(&g.edges, &file, "imports", "dplyr"));
    assert!(has_edge(&g.edges, &compute, "calls", &square));
    all_extracted(&g.edges);
}

/// Nix — attrset bindings + `import` (no classes/inheritance).
/// file → contains greet()/pkgs/message; file → imports nixpkgs; message → calls greet.
#[test]
fn nix_backlog_golden() {
    let src = format!("{}/tests/fixtures/sample.nix", env!("CARGO_MANIFEST_DIR"));
    let g = atlas_extract::extract_file(&src).expect("extract");
    let file = nid_of(&g.nodes, "sample.nix");
    let greet = nid_of(&g.nodes, "greet()"); // lambda binding → name()
    let message = nid_of(&g.nodes, "message");
    assert!(nid_of(&g.nodes, "pkgs").ends_with("pkgs"));
    assert!(has_edge(&g.edges, &file, "contains", &greet));
    assert!(has_edge(&g.edges, &file, "contains", &message));
    assert!(has_edge(&g.edges, &file, "imports", "nixpkgs"));
    assert!(has_edge(&g.edges, &message, "calls", &greet));
    all_extracted(&g.edges);
}

/// Solidity — contracts, functions, `is` inheritance, `import`.
/// file → contains Base; Base → contains ping(); Token → inherits Base;
/// file → imports ownable; mint → calls ping.
#[test]
fn solidity_backlog_golden() {
    let src = format!("{}/tests/fixtures/sample.sol", env!("CARGO_MANIFEST_DIR"));
    let g = atlas_extract::extract_file(&src).expect("extract");
    let file = nid_of(&g.nodes, "sample.sol");
    let base = nid_of(&g.nodes, "Base");
    let token = nid_of(&g.nodes, "Token");
    let ping = nid_of(&g.nodes, "ping()");
    let mint = nid_of(&g.nodes, "mint()");
    assert!(has_edge(&g.edges, &file, "contains", &base));
    assert!(has_edge(&g.edges, &base, "contains", &ping));
    assert!(has_edge(&g.edges, &token, "contains", &mint));
    assert!(has_edge(&g.edges, &token, "inherits", &base));
    assert!(has_edge(&g.edges, &file, "imports", "ownable"));
    assert!(has_edge(&g.edges, &mint, "calls", &ping));
    all_extracted(&g.edges);
}

/// Ada — packages, subprograms, `with` imports, calls.
/// file → contains Sample; Sample → contains Square()/Run(); file → imports
/// ada_text_io; Run → calls Square.
#[test]
fn ada_backlog_golden() {
    let src = format!("{}/tests/fixtures/sample.adb", env!("CARGO_MANIFEST_DIR"));
    let g = atlas_extract::extract_file(&src).expect("extract");
    let file = nid_of(&g.nodes, "sample.adb");
    let pkg = nid_of(&g.nodes, "Sample");
    let square = nid_of(&g.nodes, "Square()");
    let run = nid_of(&g.nodes, "Run()");
    assert!(has_edge(&g.edges, &file, "contains", &pkg));
    assert!(has_edge(&g.edges, &pkg, "contains", &square));
    assert!(has_edge(&g.edges, &pkg, "contains", &run));
    assert!(has_edge(&g.edges, &file, "imports", "ada_text_io"));
    assert!(has_edge(&g.edges, &run, "calls", &square));
    all_extracted(&g.edges);
}

//! Context matrix (TESTPLAN Phase 1.3).
//!
//! Runs the same core operations per type through every origin context
//! and requires identical behavior. The .tnx files are generated at
//! runtime (nothing checked in); one file per (type × context) with all
//! operations for that type.
//!
//! Known gaps live in KNOWN_FAILURES: a case listed there MUST fail
//! (otherwise the entry is stale and the test goes red), an unlisted
//! case MUST pass. Fixes therefore force the list to be kept up to date.

mod common;
use common::{parse_case, run_case};
use std::fs;
use std::path::PathBuf;

/// Contexts that still fail today — each entry is an open inference gap
/// (format: "matrix_<type>_<context>"). When fixed: remove the entry,
/// otherwise the test fails with "stale entry".
const KNOWN_FAILURES: &[&str] = &[];

struct TypeSpec {
    key: &'static str,
    /// Tinox type name (for annotations, parameters, fields, payloads)
    tnx: &'static str,
    /// Literal expression of the reference value
    lit: &'static str,
    /// Another literal of the same type (initial value before reassignment)
    other: &'static str,
    /// Operations: statement template with {V} as the value expression + expected lines
    ops: &'static [(&'static str, &'static [&'static str])],
    /// Type-specific preamble (e.g. class declaration), emitted into every file
    prelude: &'static str,
}

const TYPES: &[TypeSpec] = &[
    TypeSpec {
        key: "string",
        tnx: "String",
        lit: r#""hello""#,
        other: r#""zz""#,
        ops: &[
            ("println({V}.len());", &["5"]),
            ("println({V});", &["hello"]),
            (r#"println({V} + "!");"#, &["hello!"]),
            (r#"if ({V} == "hello") { println("eq"); } else { println("ne"); }"#, &["eq"]),
            (r#"if ({V}.contains("ell")) { println("has"); } else { println("not"); }"#, &["has"]),
            ("println({V}.substring(1, 3));", &["el"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "listint",
        tnx: "List<Int64>",
        lit: "[10, 20, 30]",
        other: "[7]",
        ops: &[
            ("println({V}.len());", &["3"]),
            ("println({V}[1]);", &["20"]),
            ("var s = 0;\n    for x in {V} { s += x; }\n    println(s);", &["60"]),
            (r#"if ({V}.contains(20)) { println("has"); } else { println("not"); }"#, &["has"]),
            ("println({V}.first());", &["10"]),
            ("println({V}.last());", &["30"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "liststr",
        tnx: "List<String>",
        lit: r#"["ab", "cde"]"#,
        other: r#"["z"]"#,
        ops: &[
            ("println({V}.len());", &["2"]),
            ("println({V}[0]);", &["ab"]),
            ("println({V}[1].len());", &["3"]),
            ("for x in {V} { println(x.len()); }", &["2", "3"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "float",
        tnx: "Float64",
        lit: "1.5",
        other: "9.0",
        ops: &[
            ("println({V}.toString());", &["1.5"]),
            ("println(({V} + 1.25).toString());", &["2.75"]),
            (r#"if ({V} < 2.0) { println("lt"); } else { println("ge"); }"#, &["lt"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "mapint",
        tnx: "Map<String, Int64>",
        lit: r#"@{"a" => 1, "b" => 2}"#,
        other: r#"@{"z" => 9}"#,
        ops: &[
            ("println({V}.len());", &["2"]),
            (r#"println({V}.get("b"));"#, &["2"]),
            (r#"if ({V}.contains("a")) { println("has"); } else { println("not"); }"#, &["has"]),
            (r#"println({V}["a"]);"#, &["1"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "mapstr",
        tnx: "Map<String, String>",
        lit: r#"@{"k" => "hello"}"#,
        other: r#"@{"z" => "y"}"#,
        ops: &[
            ("println({V}.len());", &["1"]),
            (r#"println({V}.get("k"));"#, &["hello"]),
            (r#"println({V}.get("k").len());"#, &["5"]),
            (r#"println({V}["k"]);"#, &["hello"]),
        ],
        prelude: "",
    },
    TypeSpec {
        key: "user",
        tnx: "User",
        lit: r#"User { id: 7, name: "Alice" }"#,
        other: r#"User { id: 1, name: "z" }"#,
        ops: &[
            ("println({V}.name);", &["Alice"]),
            ("println({V}.id);", &["7"]),
            ("println({V}.name.len());", &["5"]),
            ("println({V}.greet());", &["Hi Alice"]),
        ],
        prelude: "class User {\n    var id: Int64;\n    var name: String;\n    fn greet() -> String {\n        return \"Hi \" + this.name;\n    }\n}\n",
    },
    TypeSpec {
        key: "listfloat",
        tnx: "List<Float64>",
        lit: "[1.5, 2.25]",
        other: "[9.0]",
        ops: &[
            ("println({V}.len());", &["2"]),
            ("println({V}[1].toString());", &["2.25"]),
            ("var s = 0.0;\n    for x in {V} { s += x; }\n    println(s.toString());", &["3.75"]),
        ],
        prelude: "",
    },
];

/// How the value reaches the operation. Returns (preamble decls,
/// setup statements in main, value expression) — or None if the context
/// doesn't make sense for the type.
fn apply_context(ctx: &str, ty: &TypeSpec) -> Option<(String, String, String)> {
    let t = ty.tnx;
    let lit = ty.lit;
    let other = ty.other;
    Some(match ctx {
        // Operation directly on the literal
        "literal" => (String::new(), String::new(), lit.to_string()),
        // let without a type annotation
        "let" => (String::new(), format!("let v = {lit};"), "v".into()),
        // let with a type annotation
        "let_ann" => (String::new(), format!("let v: {t} = {lit};"), "v".into()),
        // var, then reassignment from a function result
        "var_reassign" => (
            format!("fnc make() -> {t} {{\n    return {lit};\n}}\n"),
            format!("var v: {t} = {other};\n    v = make();"),
            "v".into(),
        ),
        // Function parameter
        "param" => (
            String::new(),
            String::new(),
            // Special case: ops run in their own function, see emit_case
            "v".into(),
        ),
        // let from a function return
        "let_from_fn" => (
            format!("fnc make() -> {t} {{\n    return {lit};\n}}\n"),
            "let v = make();".to_string(),
            "v".into(),
        ),
        // Operation directly on the call expression
        "ret_direct" => (
            format!("fnc make() -> {t} {{\n    return {lit};\n}}\n"),
            String::new(),
            "make()".into(),
        ),
        // Element of a list (literal)
        "list_elem" => (
            String::new(),
            format!("let xs: List<{t}> = [{lit}];"),
            "xs[0]".into(),
        ),
        // Element of a list from a function return
        "list_elem_fn" => (
            format!("fnc makeList() -> List<{t}> {{\n    return [{lit}];\n}}\n"),
            "let xs = makeList();".to_string(),
            "xs[0]".into(),
        ),
        // Class field
        "field" => (
            format!("class Holder {{\n    var f: {t};\n}}\n"),
            format!("let h = Holder {{ f: {lit} }};"),
            "h.f".into(),
        ),
        // Element of a list field
        "field_list_elem" => (
            format!("class Holder {{\n    var xs: List<{t}>;\n}}\n"),
            format!("let h = Holder {{ xs: [{lit}] }};"),
            "h.xs[0]".into(),
        ),
        // Match payload
        "match_payload" => (
            format!("enum Box {{\n    Val({t}),\n    Empty,\n}}\n"),
            format!("let b = Box::Val({lit});"),
            // Special case: ops run inside the match arm, see emit_case
            "v".into(),
        ),
        // Loop variable (list of the type), just 1 element
        "loop_var" => (
            String::new(),
            format!("let xs: List<{t}> = [{lit}];"),
            // Special case: ops run inside the loop body
            "v".into(),
        ),
        // Map value: the value sits as a value in a Map<String, T>
        "map_value" => (
            String::new(),
            format!(
                "var m: Map<String, {t}> = Map::new();\n    m.insert(\"k\", {lit});"
            ),
            r#"m.get("k")"#.into(),
        ),
        // Cross-module: the value comes from a function in another module.
        // Types with their own preamble (classes) are excluded — the class
        // can't be declared in both modules (cross-module classes are
        // covered by bug06).
        // Issue #149 stage 3: mk_X() is now a static method of `class
        // MatrixMod` (no top-level free `fn` allowed anymore), called
        // qualified -- still exercises a cross-module call, just through
        // the only form left instead of the old bare-name one. `MatrixMod`
        // is imported by its short sibling name because generate_all
        // copies `MatrixMod.tnx` into every cross_module case's own
        // directory (imports resolve relative to the importing file's own
        // directory, no parent-relative import exists to reach a single
        // shared copy elsewhere -- same reasoning as examples/modules).
        "cross_module" => {
            if !ty.prelude.is_empty() {
                return None;
            }
            (
                "import MatrixMod;\n".to_string(),
                format!("let v = MatrixMod::mk_{}();", ty.key),
                "v".into(),
            )
        }
        _ => return None,
    })
}

const CONTEXTS: &[&str] = &[
    "literal",
    "let",
    "let_ann",
    "var_reassign",
    "param",
    "let_from_fn",
    "ret_direct",
    "list_elem",
    "list_elem_fn",
    "field",
    "field_list_elem",
    "match_payload",
    "loop_var",
    "map_value",
    "cross_module",
];

/// Returns (case name, type prelude class/enum or "", context prelude
/// class/enum or "", expect-comment block, class-body text (issue #149
/// stage 3: always `fnc`-based now, always ends up inside `class Main` --
/// see `generate_all`, which does the actual wrapping since it also needs
/// to interleave any function-shaped `prelude`/`ty.prelude` into the same
/// class body).
fn emit_case(ctx: &str, ty: &TypeSpec) -> Option<(String, String, String, String, String)> {
    let (prelude, setup, vexpr) = apply_context(ctx, ty)?;
    let name = format!("matrix_{}_{}", ty.key, ctx);

    let mut expects = Vec::new();
    let mut op_stmts = String::new();
    for (tpl, lines) in ty.ops {
        let stmt = tpl.replace("{V}", &vexpr);
        op_stmts.push_str("    ");
        op_stmts.push_str(&stmt);
        op_stmts.push('\n');
        expects.extend(lines.iter().map(|s| s.to_string()));
    }

    let body = match ctx {
        // Ops run in their own function with a type parameter
        "param" => format!(
            "fnc useIt(v: {t}) -> Nothing {{\n{ops}}}\n\nfnc main() -> Int32 {{\n    useIt({lit});\n    return 0;\n}}",
            t = ty.tnx,
            ops = op_stmts,
            lit = ty.lit
        ),
        // Ops run inside the match arm
        "match_payload" => format!(
            "fnc main() -> Int32 {{\n    {setup}\n    match b {{\n        Val(v) => {{\n{ops}        }}\n        _ => println(\"none\");\n    }}\n    return 0;\n}}",
            setup = setup,
            ops = op_stmts
                .lines()
                .map(|l| format!("        {l}\n"))
                .collect::<String>(),
        ),
        // Ops run inside the loop body
        "loop_var" => format!(
            "fnc main() -> Int32 {{\n    {setup}\n    for v in xs {{\n{ops}    }}\n    return 0;\n}}",
            setup = setup,
            ops = op_stmts
                .lines()
                .map(|l| format!("    {l}\n"))
                .collect::<String>(),
        ),
        _ => {
            let setup_line = if setup.is_empty() {
                String::new()
            } else {
                format!("    {setup}\n")
            };
            format!("fnc main() -> Int32 {{\n{setup_line}{op_stmts}    return 0;\n}}")
        }
    };

    let mut expects_block = String::new();
    for e in &expects {
        expects_block.push_str(&format!("// expect: {e}\n"));
    }

    // ty.prelude / prelude are always exactly one `class`/`enum Name { ... }`
    // declaration (see TYPES/apply_context above) — one-type-per-file means
    // each needs its own `<Name>.tnx` instead of being pasted into the
    // driver script alongside `class Main`.
    Some((name, ty.prelude.to_string(), prelude, expects_block, body))
}

/// Extracts the declared type name from a generated `class Name { ... }` /
/// `enum Name { ... }` prelude, or `None` if this prelude is something else
/// entirely (a free `fn make() -> T {...}`, an `import _matrix_mod;` line —
/// `ty.prelude`/context prelude aren't always a type, only "field",
/// "field_list_elem", "match_payload" and the `user` TypeSpec are). Only an
/// actual type needs its own `<Name>.tnx` file; a function/import prelude
/// stays inline in the driver script exactly as before.
fn prelude_type_name(prelude: &str) -> Option<&str> {
    prelude
        .strip_prefix("class ")
        .or_else(|| prelude.strip_prefix("enum "))
        .or_else(|| prelude.strip_prefix("interface "))
        .and_then(|rest| rest.split_whitespace().next())
}

/// Issue #149 stage 3: `mk_X` used to be free top-level functions; now
/// static methods of a single `class MatrixMod`, called qualified
/// (`MatrixMod::mk_X()`, see the "cross_module" context above).
fn helper_module() -> String {
    let mut s = String::from("class MatrixMod\n{\n");
    for ty in TYPES.iter().filter(|t| t.prelude.is_empty()) {
        s.push_str(&format!(
            "    fnc mk_{key}() -> {t} {{\n        return {lit};\n    }}\n\n",
            key = ty.key,
            t = ty.tnx,
            lit = ty.lit
        ));
    }
    s.push_str("}\n");
    s
}

fn generate_all(shard: usize) -> PathBuf {
    // A separate directory per shard — shards run as parallel threads, a
    // shared directory would cause remove/rewrite races.
    let dir = std::env::temp_dir().join(format!("tinox-matrix-{}-{shard}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir matrix dir");
    // Issue #149 stage 3: `class MatrixMod` must live in a file named
    // exactly `MatrixMod.tnx` (one-class-per-file); every case now lives in
    // its own subdirectory (see below), and imports resolve relative to
    // the importing file's own directory only (no parent-relative import
    // exists) — so instead of one shared copy at the shard root (the old
    // flat `_matrix_mod.tnx` layout), a copy gets written into each
    // "cross_module" case's own directory as a plain sibling of `Main.tnx`,
    // same duplication trade-off as examples/modules/*_example/.
    let matrixmod_src = helper_module();
    for ty in TYPES {
        for ctx in CONTEXTS {
            if let Some((name, ty_prelude, ctx_prelude, expects_block, body)) = emit_case(ctx, ty) {
                // Split preludes into actual types (need their own file) vs.
                // anything else (a free `fnc make()`, `import
                // _matrixmod.MatrixMod;` — no type-per-file constraint,
                // stays inline as either a header import or a sibling
                // class-body member, see below).
                let mut type_preludes: Vec<(&str, &str)> = Vec::new();
                let mut inline_prelude = String::new();
                for p in [&ty_prelude, &ctx_prelude] {
                    if p.is_empty() {
                        continue;
                    }
                    match prelude_type_name(p) {
                        Some(type_name) => type_preludes.push((type_name, p)),
                        None => {
                            inline_prelude.push_str(p);
                            inline_prelude.push('\n');
                        }
                    }
                }
                // issue #149 stage 3: every case's `fnc main` (and any
                // sibling `fnc make()`/`makeList()`/`useIt()`) now lives in
                // one `class Main` — an `import` line is the only kind of
                // inline_prelude that must stay OUTSIDE the class (imports
                // are always top-level); everything else is class-body
                // material. The two never mix for one case (see
                // apply_context: "cross_module", the only import-line
                // producer, always returns early when the type also has
                // its own prelude), so this split is unambiguous.
                let (header_import, class_extra) = if inline_prelude.starts_with("import ") {
                    (inline_prelude.clone(), String::new())
                } else {
                    (String::new(), inline_prelude.clone())
                };

                // Issue #149 stage 3: `class Main` always requires a file
                // named exactly `Main.tnx` (one-class-per-file) -- every
                // case gets its own subdirectory now, whether or not it
                // also needs sibling type files (previously only the
                // type-prelude case did; a bare 1-type "Main only" script
                // used to stay a flat `<name>.tnx`, which is no longer
                // legal once that script's `fn main` becomes `class Main`).
                let case_dir = dir.join(&name);
                fs::create_dir_all(&case_dir).expect("mkdir case dir");
                let imports: String =
                    type_preludes.iter().map(|(t, _)| format!("import {t};\n")).collect();
                for (type_name, p) in &type_preludes {
                    // Every sibling prelude type is imported into every
                    // other one too (harmless if unused — e.g. `Holder`
                    // referencing `User`'s field type needs it, `User`
                    // itself doesn't need `Holder` back).
                    let others: String = type_preludes
                        .iter()
                        .filter(|(t, _)| t != type_name)
                        .map(|(t, _)| format!("import {t};\n"))
                        .collect();
                    let content = if others.is_empty() {
                        (*p).to_string()
                    } else {
                        format!("{others}\n{p}")
                    };
                    fs::write(case_dir.join(format!("{type_name}.tnx")), content)
                        .expect("write case prelude type");
                }
                debug_assert!(
                    type_preludes.is_empty() || header_import.is_empty(),
                    "cross_module never co-occurs with a type prelude"
                );
                if header_import.starts_with("import MatrixMod;") {
                    fs::write(case_dir.join("MatrixMod.tnx"), &matrixmod_src)
                        .expect("write case's MatrixMod.tnx copy");
                }
                let content = format!(
                    "{expects_block}\n{header_import}{imports}\nclass Main\n{{\n{class_extra}{body}\n}}\n"
                );
                fs::write(case_dir.join("Main.tnx"), content).expect("write case driver");
            }
        }
    }
    dir
}

fn run_shard(shard: usize, num_shards: usize) {
    let dir = generate_all(shard);
    let mut cases: Vec<(String, PathBuf)> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .map(|n| n.to_string_lossy().starts_with('_'))
                .unwrap_or(false)
        })
        .filter_map(|p| {
            if p.is_dir() {
                // Issue #149 stage 2: mirrors the same `Main.tnx`-first,
                // `main.tnx`-fallback lookup e2e.rs uses (crates/tinox/tests/e2e.rs)
                // for directory-based cases. This generator's own templates
                // (see `generate_all` above) still only ever write
                // `main.tnx` — that's deferred, unrelated to this read-side
                // lookup becoming forward-compatible.
                let entry = p.join("Main.tnx");
                let entry = if entry.is_file() { entry } else { p.join("main.tnx") };
                entry.is_file().then(|| {
                    (p.file_name().unwrap().to_string_lossy().to_string(), entry)
                })
            } else if p.extension().map(|x| x == "tnx").unwrap_or(false) {
                Some((p.file_stem().unwrap().to_string_lossy().to_string(), p))
            } else {
                None
            }
        })
        .collect();
    cases.sort_by(|a, b| a.0.cmp(&b.0));

    let mut unexpected_failures = Vec::new();
    let mut stale_entries = Vec::new();
    for (i, (name, path)) in cases.iter().enumerate() {
        if i % num_shards != shard {
            continue;
        }
        let mut case = parse_case(path);
        case.name = name.clone();
        let known_bad = KNOWN_FAILURES.contains(&name.as_str());
        match run_case(&case) {
            Ok(()) if known_bad => stale_entries.push(name.clone()),
            Ok(()) => {}
            Err(_) if known_bad => {}
            Err(msg) => unexpected_failures.push(format!("== {name} ==\n{msg}")),
        }
    }

    let mut problems = Vec::new();
    if !unexpected_failures.is_empty() {
        problems.push(format!(
            "{} matrix cases fail (new inference gaps — fix or add to KNOWN_FAILURES):\n\n{}",
            unexpected_failures.len(),
            unexpected_failures.join("\n\n")
        ));
    }
    if !stale_entries.is_empty() {
        problems.push(format!(
            "stale KNOWN_FAILURES (now passing — remove the entry): {}",
            stale_entries.join(", ")
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}

#[test]
fn matrix_shard_0() { run_shard(0, 4); }
#[test]
fn matrix_shard_1() { run_shard(1, 4); }
#[test]
fn matrix_shard_2() { run_shard(2, 4); }
#[test]
fn matrix_shard_3() { run_shard(3, 4); }

//! Composition checks exercise the real compiler pipeline and ABI in each mode.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Work(PathBuf);
impl Work {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tinox-semantics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .arg(action)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"));
        command
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/e2e")
        .join(name)
        .join("Main.tnx")
}
#[test]
fn composed_callbacks_generics_fields_conversions_and_interfaces_work_in_all_modes() {
    let work = Work::new();
    let expected = "2\n3\n4\n5\n6\n7\n255\n255\n1.5\ntuple\n2.5\n3\n";
    for (name, flags) in [
        ("debug", vec!["--debug"]),
        ("release", vec![]),
        ("checked", vec!["--checked"]),
    ] {
        let executable = work.0.join(name);
        let build = work
            .command("build")
            .arg(fixture("semantic_composition"))
            .args(flags)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(build.status.success(), "{name}: {build:?}");
        let run = Command::new(&executable).output().unwrap();
        assert!(run.status.success(), "{name}: {run:?}");
        assert_eq!(String::from_utf8_lossy(&run.stdout), expected, "{name}");
        let ir = fs::read_to_string(executable.with_extension("ll")).unwrap();
        assert!(ir.contains("zext i8"));
        assert!(ir.contains("call double %"));
        assert!(ir.contains("call float %"));
    }
}
#[test]
fn typed_task_results_keep_the_existing_runtime_word_abi() {
    let work = Work::new();
    let expected = "1.5\n2.5\n255\ndone\n";
    for flags in [vec!["--debug"], vec![], vec!["--checked"]] {
        let executable = work.0.join("tasks");
        let build = work
            .command("build")
            .arg(fixture("semantic_task_results"))
            .args(flags)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(build.status.success(), "{build:?}");
        for _ in 0..20 {
            let run = Command::new(&executable).output().unwrap();
            assert!(run.status.success(), "{run:?}");
            assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
        }
    }
}
#[test]
fn incompatible_callback_and_generic_compositions_fail_in_the_frontend() {
    let work = Work::new();
    fs::write(work.0.join("Box.tnx"), "class Box<T> { var value: T; }").unwrap();
    for source in [
        "class Main { fnc bad(f: fnc(String) -> String) -> String { return f(42); } fnc main() -> Int32 { return 0; } }",
        "class Main { fnc bad(f: fnc(String) -> String) -> String { return f(); } fnc main() -> Int32 { return 0; } }",
        "class Main { fnc take(f: fnc(Float64) -> Float64) -> Nothing {} fnc main() -> Int32 { Main::take(fnc(x: String) -> String { return x; }); return 0; } }",
        "import Box; class Main { fnc main() -> Int32 { const wrong: Box<String> = Box { value: 42 }; return 0; } }",
        "import Box; class Main { fnc take(value: Box<String>) -> Nothing {} fnc main() -> Int32 { Main::take(Box { value: 42 }); return 0; } }",
    ] {
        fs::write(work.0.join("Main.tnx"), source).unwrap();
        for action in ["check", "build"] {
            let result = work.command(action).args(["Main.tnx", "--json"]).env("PATH", "").output().unwrap();
            assert!(!result.status.success(), "{action}: {result:?}");
            let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(result["diagnostics"][0]["category"], "type", "{result}");
        }
    }
}

#[test]
fn missing_checked_callback_data_is_an_actionable_lowering_error() {
    let tokens = tinox_lexer::Lexer::new("class Main { fnc apply(cb: fnc(Int64) -> Int64) -> Int64 { const local = cb; return local(1); } fnc main() -> Int32 { Main::apply(x => x); return 0; } }").tokenize().unwrap();
    let mut ast = tinox_parser::Parser::new(tokens).parse().unwrap();
    tinox_parser::assign_node_ids(&mut ast);
    let mut checker = tinox_typecheck::TypeChecker::new();
    checker.check(&ast).unwrap();
    let mut facts = checker.semantic_facts();
    let closure = facts
        .calls
        .iter()
        .find(|(_, call)| call.dispatch == tinox_typecheck::semantic::CallDispatch::Closure)
        .map(|(node, _)| *node)
        .unwrap();
    facts.calls.remove(&closure);
    let mut types = checker.expr_value_types();
    types.remove(&closure);
    let mut codegen = tinox_codegen::CodeGen::new();
    codegen.set_semantic_facts(facts);
    codegen.set_expr_value_types(types);
    let errors = codegen.gen(&ast).unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.message.contains("Missing resolved call semantics")),
        "{errors:?}"
    );
}

#[test]
fn complete_semantic_compositions_run_in_all_modes() {
    let work = Work::new();
    let expected = "256\n3.5\n127\ntrue\n255\n-1\ntext\nnested\n255\n2.5\n1.5\n2.5\n2.5\n256\n1.5\n2\n1.5\n255\n1.5\n3.5\n4.5\n4.5\n3.5\n7.5\n6.5\n255\n1.5\nb\n3.5\n255\n255\n";
    for flags in [vec!["--debug"], vec![], vec!["--checked"]] {
        let executable = work.0.join("complete");
        let build = work
            .command("build")
            .arg(fixture("semantic_complete"))
            .args(flags)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(build.status.success(), "{build:?}");
        for _ in 0..20 {
            let run = Command::new(&executable).output().unwrap();
            assert!(run.status.success(), "{run:?}");
            assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
        }
        let ir = fs::read_to_string(executable.with_extension("ll")).unwrap();
        assert!(ir.contains("udiv i8"));
        assert!(ir.contains("icmp ugt i8"));
        assert!(ir.contains("sitofp i64"));
        assert!(ir.contains("Cell__u8_get"));
        assert!(ir.contains("Cell__i8_get"));
    }
}

#[test]
fn unresolved_and_conflicting_generic_bindings_fail_before_lowering() {
    let work = Work::new();
    for source in [
        "class Main { fnc main() -> Int32 { const source: List<Float64> = [1.5]; const invalid: List<Float32> = source; return 0; } }",
        "class Main { fnc make<T>() -> T { throw 1; } fnc main() -> Int32 { Main::make(); return 0; } }",
        "class Main { fnc same<T>(left: T, right: T) -> T { return left; } fnc main() -> Int32 { Main::same(\"text\", 42); return 0; } }",
    ] {
        fs::write(work.0.join("Main.tnx"), source).unwrap();
        let result = work.command("build").args(["Main.tnx", "--json"]).env("PATH", "").output().unwrap();
        assert!(!result.status.success(), "{result:?}");
        let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(result["diagnostics"][0]["category"], "type", "{result}");
    }
}

#[test]
fn unsigned_zero_division_and_remainder_fail_with_visible_runtime_diagnostics() {
    let work = Work::new();
    for (operator, message) in [("/", "division by zero"), ("%", "modulo by zero")] {
        fs::write(work.0.join("Main.tnx"), format!("class Main {{ fnc main() -> Int32 {{ const value: UInt8 = 255; const zero: UInt8 = 0; println(value {operator} zero); return 0; }} }}")).unwrap();
        for flags in [vec!["--debug"], vec![], vec!["--checked"]] {
            let executable = work.0.join("zero");
            let build = work
                .command("build")
                .arg("Main.tnx")
                .args(flags)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(build.status.success(), "{build:?}");
            let run = Command::new(executable).output().unwrap();
            assert_eq!(run.status.code(), Some(1), "{run:?}");
            assert!(
                String::from_utf8_lossy(&run.stderr).contains(message),
                "{run:?}"
            );
        }
    }
}

#[test]
fn ambiguous_qualified_declarations_never_reach_native_emission() {
    let source = "namespace left { class Thing { fnc side() -> String { return \"left\"; } } } namespace right { class Thing { fnc side() -> String { return \"right\"; } } }";
    let tokens = tinox_lexer::Lexer::new(source).tokenize().unwrap();
    let mut ast = tinox_parser::Parser::new(tokens).parse().unwrap();
    tinox_parser::assign_node_ids(&mut ast);
    let mut checker = tinox_typecheck::TypeChecker::new();
    checker.check(&ast).unwrap();
    let mut codegen = tinox_codegen::CodeGen::new();
    codegen.set_expr_value_types(checker.expr_value_types());
    codegen.set_semantic_facts(checker.semantic_facts());
    let errors = codegen.gen(&ast).unwrap_err();
    assert!(errors
        .errors
        .iter()
        .any(|error| error.message.contains("Ambiguous declaration")));
}

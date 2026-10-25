//! Real CLI descriptions must follow compiler resolution, including installed versions.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tinox-describe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let project = Self(root);
        project.write(
            "tinox.toml",
            "[package]\ncompiler = \"2.5.1\"\nname=\"describe\"\nversion=\"0.1.0\"\nentry=\"src/Main.tnx\"\n",
        );
        project.write(
            "src/Main.tnx",
            "class Main { fnc main() -> Int32 { return 0 } }",
        );
        project
    }
    fn write(&self, file: &str, source: &str) {
        let path = self.0.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .arg(action)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"))
            .env("TINOX_HOME", &self.0);
        command
    }
    fn query(&self, arguments: &[&str], success: bool) -> Value {
        let output = self
            .command("describe")
            .args(arguments)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success, "{output:?}");
        let result = json(&output);
        assert_eq!(result["schema_version"], 1);
        assert_eq!(result["success"], success);
        result
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
}

#[test]
fn generics_callbacks_visibility_and_member_bounds_are_retained() {
    let project = Project::new();
    project.write(
        "src/Main.tnx",
        "import model.Box\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write("src/model/Box.tnx", "namespace model { /** A typed box. */ class Box<T> { private var value: T\n/** Apply a callback. */ fn map<U>(callback: fnc(T) -> U) -> U { return callback(this.value) } fnc make(value: T) -> Box<T> { return Box { value: value } } } }");
    let checked = project.command("check").output().unwrap();
    assert!(checked.status.success(), "{checked:?}");
    let result = project.query(&["model.Box"], true);
    let symbol = &result["symbol"];
    assert_eq!(symbol["type_parameters"], serde_json::json!(["T"]));
    assert_eq!(symbol["required_import"], "import model.Box");
    assert_eq!(symbol["members"][0]["visibility"], "private");
    let member = project.query(&["Box::map"], true);
    assert_eq!(member["symbol"]["members"].as_array().unwrap().len(), 1);
    assert_eq!(
        member["symbol"]["members"][0]["parameters"][0]["type_name"],
        "fnc(T) -> U"
    );
    assert!(member["symbol"]["members"][0]["signature"]
        .as_str()
        .unwrap()
        .contains("map<U>"));
    assert_eq!(symbol["members"][2]["kind"], "factory");
    let bounded = project.query(&["Box", "--limit", "1", "--signatures-only"], true);
    assert_eq!(bounded["symbol"]["members"].as_array().unwrap().len(), 1);
    assert_eq!(bounded["symbol"]["omitted_members"], 2);
    assert!(bounded["symbol"]["documentation"].is_null());
    let text = project
        .command("describe")
        .args(["Box::map", "--signatures-only"])
        .output()
        .unwrap();
    assert!(text.status.success());
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(stdout.contains("fn map<U>(callback: fnc(T) -> U) -> U"));
    assert!(!stdout.contains("return callback"));
    assert!(!stdout.contains("Apply a callback"));
    project.query(&["Box::absent"], false);
}

#[test]
fn namespaced_and_ambiguous_symbols_fail_or_resolve_explicitly() {
    let project = Project::new();
    project.write(
        "src/Main.tnx",
        "import left.Thing\nimport right.Thing\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write(
        "src/left/Thing.tnx",
        "namespace left { class Thing { fnc side() -> String { return \"left\" } } }",
    );
    project.write(
        "src/right/Thing.tnx",
        "namespace right { class Thing { fnc side() -> String { return \"right\" } } }",
    );
    let failure = project.query(&["Thing"], false);
    assert!(failure["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Ambiguous symbol"));
    assert_eq!(failure["error"]["code"], "TINOX-DESCRIBE-AMBIGUOUS");
    assert_eq!(failure["error"]["candidates"].as_array().unwrap().len(), 2);
    let result = project.query(&["right.Thing"], true);
    assert_eq!(result["symbol"]["qualified_name"], "right.Thing");
    assert!(result["symbol"]["source"]
        .as_str()
        .unwrap()
        .ends_with("/right/Thing.tnx"));
    project.query(&["Missing"], false);
    project.query(&["Thing", "--limit", "0"], false);
    project.query(&["Thing", "--limit", "201"], false);
    project.query(&["Thing", "--unknown"], false);
}

#[test]
fn descriptions_follow_installed_versions_and_never_bundled_extended_sources() {
    let project = Project::new();
    let manifest = |version| {
        format!("[package]\ncompiler = \"2.5.1\"\nname=\"describe\"\nversion=\"0.1.0\"\nentry=\"src/Main.tnx\"\n[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"crypto\"\nversion=\"{version}\"\n")
    };
    for (version, method) in [("9.0.0", "installedNine"), ("10.0.0", "installedTen")] {
        project.write("tinox.toml", &manifest(version));
        project.write(&format!(".tinox/repository/central/tinox.core/crypto/{version}/tinox/core/crypto/Crypto.tnx"), &format!("namespace tinox.core.crypto {{ class Crypto {{ fnc {method}() -> UInt8 {{ return 1 }} }} }}"));
        let result = project.query(&["tinox.core.crypto.Crypto"], true);
        assert_eq!(
            result["symbol"]["package"],
            format!("tinox.core:crypto:{version}")
        );
        assert_eq!(result["symbol"]["members"][0]["name"], method);
        assert_eq!(result["symbol"]["members"][0]["return_type"], "UInt8");
        project.write("src/Main.tnx", &format!("import tinox.core.crypto\nclass Main {{ fnc main() -> Int32 {{ println(Crypto::{method}())\nreturn 0 }} }}"));
        assert!(project.command("check").output().unwrap().status.success());
        project.write(
            "src/Main.tnx",
            "class Main { fnc main() -> Int32 { return 0 } }",
        );
    }
    project.write("tinox.toml", &manifest("11.0.0"));
    let failure = project.query(&["tinox.core.crypto.Crypto"], false);
    assert!(failure["error"]["message"]
        .as_str()
        .unwrap()
        .contains("install"));
}

#[test]
fn core_and_annotation_queries_reuse_compiler_metadata() {
    let project = Project::new();
    let result = project.query(
        &["tinox.core.array.Arrays", "--member", "containsInt"],
        true,
    );
    assert_eq!(
        result["symbol"]["members"][0]["parameters"][0]["type_name"],
        "List<Int64>"
    );
    assert!(result["symbol"]["members"][0]["documentation"]
        .as_str()
        .unwrap()
        .contains("Checks"));
    let result = project.query(&["@GET"], true);
    let processor = tinox_typecheck::annotations::AnnotationProcessor::new();
    assert_eq!(
        result["annotation"]["min_args"],
        processor.info("GET").unwrap().min_args
    );
    assert_eq!(
        result["annotation"]["valid_targets"],
        serde_json::json!(["method"])
    );
    assert!(result["annotation"]["description"]
        .as_str()
        .unwrap()
        .contains("endpoint"));
    project.write(
        "src/Main.tnx",
        "import Mark\n@Mark class Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write("src/Mark.tnx", "@annotation class Mark {}");
    let result = project.query(&["@Mark"], true);
    assert_eq!(result["symbol"]["name"], "Mark");
    assert_eq!(result["annotation"]["name"], "Mark");
}

#[test]
fn explicit_sources_generated_accessors_and_doc_limits_are_honest() {
    let project = Project::new();
    project.write(
        "src/Person.tnx",
        &format!(
            "@Getter /** {} */ class Person {{ var name: String = \"\" }}",
            "Documentation ".repeat(150)
        ),
    );
    let output = project
        .command("check")
        .arg("src/Person.tnx")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result = project.query(
        &["Person", "--file", "src/Person.tnx", "--member", "getName"],
        true,
    );
    assert_eq!(result["symbol"]["members_scope"], "declared");
    assert_eq!(result["symbol"]["members"][0]["return_type"], "String");
    assert_eq!(result["symbol"]["members"][0]["generated"], true);
    assert!(result["symbol"]["members"][0]["range"].is_null());
    assert_eq!(result["symbol"]["documentation_truncated"], true);
    assert_eq!(
        result["symbol"]["documentation"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1200
    );
    let result = project.query(
        &["Person", "--file", "src/Person.tnx", "--signatures-only"],
        true,
    );
    assert_eq!(result["symbol"]["documentation_truncated"], false);
    assert!(result["symbol"]["documentation"].is_null());
    let result = project.query(&["Person", "--file", "missing.tnx"], false);
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Cannot read"));
}

#[test]
fn annotations_use_source_overrides_and_queries_can_run_without_a_project() {
    let project = Project::new();
    project.write(
        "src/Main.tnx",
        "import Test\n@Test class Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write("src/Test.tnx", "@annotation class Test {}");
    let result = project.query(&["@Test"], true);
    assert_eq!(result["symbol"]["name"], "Test");
    assert!(result["annotation"]["max_args"].is_null());
    assert!(result["annotation"]["valid_targets"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("class")));
    fs::remove_file(project.0.join("tinox.toml")).unwrap();
    project.query(&["@GET"], true);
    let result = project.query(&["tinox.core.array.Arrays::containsInt"], true);
    assert_eq!(
        result["symbol"]["qualified_name"],
        "tinox.core.array.Arrays"
    );
    project.query(&["@GET::missing"], false);
}

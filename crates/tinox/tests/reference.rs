//! The exported reference must track compiler semantics, never a copied list.
use serde_json::Value;
use std::process::Command;

fn export(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("reference")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn reference_matches_annotation_validator_and_typechecker() {
    let output = export(&[]);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["compiler_version"], env!("CARGO_PKG_VERSION"));
    let processor = tinox_typecheck::annotations::AnnotationProcessor::new();
    let entries = processor.reference_annotations();
    assert_eq!(
        value["annotations"].as_array().unwrap().len(),
        entries.len()
    );
    for (exported, actual) in value["annotations"].as_array().unwrap().iter().zip(entries) {
        assert_eq!(exported["name"], actual.name);
        assert_eq!(exported["min_args"], actual.min_args);
        assert_eq!(exported["max_args"], actual.max_args);
        let targets: Vec<_> = actual
            .valid_targets
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        assert_eq!(exported["targets"], serde_json::json!(targets));
    }
    let checker = tinox_typecheck::TypeChecker::new();
    assert_eq!(
        value["builtins"],
        serde_json::json!(checker.reference_signatures())
    );
    assert_eq!(
        output.stdout,
        export(&[]).stdout,
        "export must be deterministic"
    );
}

#[test]
fn source_reference_preserves_generic_signatures_and_fails_on_invalid_source() {
    let dir = std::env::temp_dir().join(format!("tinox-reference-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("Box.tnx");
    std::fs::write(
        &source,
        "class Box<T> { fn identity(value: T) -> T { return value; } }",
    )
    .unwrap();
    let output = export(&[source.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let class = &value["sources"]["Box.tnx"][0];
    assert_eq!(class["type_params"], serde_json::json!(["T"]));
    assert_eq!(class["methods"][0]["params"][0]["ty"], "T");
    assert_eq!(class["methods"][0]["ret"], "T");
    std::fs::write(&source, "class Box {").unwrap();
    let bad = export(&[dir.to_str().unwrap()]);
    assert!(!bad.status.success());
    assert!(
        bad.stdout.is_empty(),
        "invalid export must not look complete"
    );
    assert!(!export(&[dir.join("missing").to_str().unwrap()])
        .status
        .success());
    std::fs::remove_dir_all(dir).unwrap();
}

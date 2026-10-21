//! Build once, then exercise deployment configuration in independent processes.
use std::{fs, path::{Path, PathBuf}, process::{Command, Output}};

fn success(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{output:?}");
    output
}

fn run(directory: &Path, overrides: &[(&str, &str)]) -> Output {
    let mut command = Command::new(directory.join("program"));
    command.current_dir(directory);
    for key in ["CASE_TYPE", "CASE_POLICY", "CASE_DEFAULT", "COLLISION", "CONFLICT", "RESERVED"] {
        command.env_remove(key);
    }
    // Keep tests independent of configuration in the test runner's environment.
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("TINOX_CONFIG_")) {
        command.env_remove(key);
    }
    command.envs(overrides.iter().copied()).output().unwrap()
}

#[test]
fn one_artifact_validates_before_main_and_resolves_runtime_overrides() {
    let directory = std::env::temp_dir().join(format!("tinox-runtime-config-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("Main.tnx"), r#"
@JsonSerializable
class Main {
    @Config("service.port", "required") var port: Int32;
    @Config("service.enabled", "optional") var enabled: Bool;
    @Config("service.label", "default", "development") var label: String;
    @Sensitive @Config("service.token", "required") var token: String;
    fnc main() -> Int32 {
        let config = Main {};
        println(config.port);
        println(config.enabled);
        println(config.label);
        println(config.toString());
        println(config.toJson());
        return 0;
    }
}
"#).unwrap();
    success(Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&directory)
        .args(["build", "Main.tnx", "-o", "program"]));
    let secret = "runtime-only-secret-308";
    for port in ["101", "202"] {
        fs::write(directory.join("application.properties"), format!("service.port={port}\nservice.token={secret}\n")).unwrap();
        let output = run(&directory, &[]);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with(&format!("{port}\nfalse\ndevelopment\n")), "{stdout}");
        assert!(!stdout.contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
    let ir = fs::read_to_string(directory.join("program.ll")).unwrap();
    assert!(!ir.contains(secret));
    let output = run(&directory, &[("TINOX_CONFIG_SERVICE_PORT", "303"), ("TINOX_CONFIG_SERVICE_ENABLED", "true")]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("303\ntrue\n"));

    fs::write(directory.join("deployment.properties"), format!("service.port=404\nservice.token={secret}\nservice.label=production\n")).unwrap();
    let output = run(&directory, &[("TINOX_CONFIG_FILE", "deployment.properties")]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("404\nfalse\nproduction\n"));
    for (key, value, message) in [
        ("TINOX_CONFIG_SERVICE_PORT", "", "required value is empty"),
        ("TINOX_CONFIG_SERVICE_PORT", "2147483648", "declared type range"),
        ("TINOX_CONFIG_SERVICE_PORT", "12oops", "decimal integer"),
        ("TINOX_CONFIG_SERVICE_PORT", " 12", "decimal integer"),
        ("TINOX_CONFIG_SERVICE_ENABLED", "yes", "expected true or false"),
        ("TINOX_CONFIG_SERVICE_ENABLED", "1", "expected true or false"),
        ("TINOX_CONFIG_SERVICE_ENABLED", "TRUE", "expected true or false"),
        ("TINOX_CONFIG_SERVICE_ENABLED", "", "value is empty"),
        ("TINOX_CONFIG_FILE", "missing.properties", "cannot open explicit"),
    ] {
        let output = run(&directory, &[(key, value)]);
        assert_eq!(output.status.code(), Some(78), "{key}={value}: {output:?}");
        assert!(output.stdout.is_empty(), "main ran before validation: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(message), "{stderr}");
        assert!(!stderr.contains(secret));
    }
    let output = run(&directory, &[("TINOX_CONFIG_SERVICE_LABEL", "")]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("202\nfalse\n\n"));
    fs::remove_file(directory.join("application.properties")).unwrap();
    let output = run(&directory, &[]);
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("service.port': required value is missing"), "{stderr}");
    assert!(stderr.contains("service.token': required value is missing"), "{stderr}");
    assert!(output.stdout.is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_client_checks_integer_boundaries_conflicts_and_redacted_introspection() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = std::env::temp_dir().join(format!("tinox-config-native-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("client.c"), r#"
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
extern void tinox_config_declare(const char*, const char*, const char*, const char*);
extern char* tinox_config_dump_json(void);
extern int64_t tinox_config_get_int(const char*);
extern int64_t tinox_config_get_bool(const char*);
extern char* tinox_config_get(const char*);
void tinox_config_init(void) {
    const char *policy = getenv("CASE_POLICY");
    tinox_config_declare("number", getenv("CASE_TYPE"), policy ? policy : "required", getenv("CASE_DEFAULT"));
    if (getenv("RESERVED")) tinox_config_declare("file", "String", "optional", NULL);
    if (getenv("COLLISION")) {
        tinox_config_declare("a.b", "String", "optional", NULL);
        tinox_config_declare("a-b", "String", "optional", NULL);
    }
    if (getenv("CONFLICT")) tinox_config_declare("number", "Bool", "optional", NULL);
}
int64_t tinox_main(void) {
    puts(tinox_config_dump_json());
    printf("integer=%lld boolean=%lld string=%s\n", (long long)tinox_config_get_int("number"),
        (long long)tinox_config_get_bool("number"), tinox_config_get("number"));
    printf("bits=%llu\n", (unsigned long long)tinox_config_get_int("number"));
    return 0;
}
"#).unwrap();
    success(Command::new("cc").current_dir(&directory).arg("client.c").arg(root.join("runtime/runtime.c"))
        .args(["-o", "program", "-lm", "-lpthread", "-lgc", "-lz", "-no-pie"]));
    fs::write(directory.join("application.properties"), "undeclared.secret=never-expose-this\nnumber=1\nnumber=2\n").unwrap();
    for (ty, min, max, under, over) in [
        ("Int8", "-128", "127", "-129", "128"),
        ("Int16", "-32768", "32767", "-32769", "32768"),
        ("Int32", "-2147483648", "2147483647", "-2147483649", "2147483648"),
        ("Int64", "-9223372036854775808", "9223372036854775807", "-9223372036854775809", "9223372036854775808"),
        ("UInt8", "0", "255", "-1", "256"),
        ("UInt16", "0", "65535", "-1", "65536"),
        ("UInt32", "0", "4294967295", "-1", "4294967296"),
        ("UInt64", "0", "18446744073709551615", "-1", "18446744073709551616"),
    ] {
        for (value, valid) in [(min, true), (max, true), (under, false), (over, false), ("1x", false), ("1 ", false), ("0x10", false), ("+1", true)] {
            let output = run(&directory, &[("CASE_TYPE", ty), ("TINOX_CONFIG_NUMBER", value)]);
            assert_eq!(output.status.success(), valid, "{ty} {value}: {output:?}");
            assert_eq!(output.status.code(), Some(if valid { 0 } else { 78 }));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("never-expose-this"));
            if valid {
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains("[redacted]"));
                let expected_bits = value.parse::<i128>().unwrap() as u64;
                assert!(stdout.contains(&format!("bits={expected_bits}\n")), "{stdout}");
            }
        }
    }
    for (flag, message) in [("COLLISION", "environment name collides"), ("CONFLICT", "conflicting declarations"), ("RESERVED", "environment name is reserved")] {
        let output = run(&directory, &[("CASE_TYPE", "Int32"), (flag, "1")]);
        assert_eq!(output.status.code(), Some(78), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert!(output.stdout.is_empty());
    }
    let output = run(&directory, &[("CASE_TYPE", "Int32")]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("integer=2"));
    fs::write(directory.join("application.properties"), "").unwrap();
    for ty in ["String", "Bool", "Int32"] {
        let output = run(&directory, &[("CASE_TYPE", ty), ("CASE_POLICY", "optional")]);
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("integer=0 boolean=0 string=\n"));
    }
    for (ty, value, valid) in [("Int32", "42", true), ("Int32", "invalid", false), ("Bool", "true", true), ("Bool", "yes", false), ("String", "", true)] {
        let output = run(&directory, &[("CASE_TYPE", ty), ("CASE_POLICY", "default"), ("CASE_DEFAULT", value)]);
        assert_eq!(output.status.success(), valid, "{output:?}");
    }
    for (ty, value, expected) in [("Int32", "12tail", "integer=12"), ("Bool", "yes", "boolean=1"), ("Bool", "nonsense", "boolean=0")] {
        let output = run(&directory, &[("CASE_TYPE", ty), ("CASE_POLICY", "legacy"), ("TINOX_CONFIG_NUMBER", value)]);
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
    fs::remove_dir_all(directory).unwrap();
}

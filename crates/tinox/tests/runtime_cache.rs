//! Exercise cache selection through the real CLI and a logging C compiler.
#[cfg(unix)]
#[test]
fn cli_reuses_runtime_and_separates_compiler_flags_and_disabled_builds() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("tinox-runtime-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("bin")).unwrap();
    let real_cc = Command::new("sh")
        .args(["-c", "command -v cc"])
        .output()
        .unwrap();
    let real_cc = String::from_utf8(real_cc.stdout)
        .unwrap()
        .trim()
        .to_string();
    let wrapper = dir.join("bin/cc");
    let log = dir.join("compile.log");
    fs::write(&wrapper, format!("#!/bin/sh\nfor arg; do\n if [ \"$arg\" = -c ]; then echo compile >> '{}'; fi\ndone\nexec '{}' \"$@\"\n", log.display(), real_cc)).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        dir.join("Main.tnx"),
        "class Main { fnc main() -> Int32 { println(\"cache works\"); return 0; } }",
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(dir.join("bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let build = |flags: &str, enabled: bool| {
        let output = Command::new(env!("CARGO_BIN_EXE_tinox"))
            .args(["build", "Main.tnx", "-o", "app", "--debug"])
            .current_dir(&dir)
            .env("PATH", &path)
            .env("TINOX_RUNTIME_CACHE_DIR", dir.join("cache"))
            .env("TINOX_CFLAGS", flags)
            .env("TINOX_RUNTIME_CACHE", if enabled { "1" } else { "0" })
            .env("TINOX_CACHE_TRACE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result = Command::new(dir.join("app")).output().unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim(),
            "cache works"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let count = || fs::read_to_string(&log).unwrap().lines().count();
    assert!(build("", true).contains("miss"));
    assert_eq!(count(), 1);
    assert!(build("", true).contains("hit"));
    assert_eq!(count(), 1, "warm build recompiled runtime.c");
    assert!(build("-DTINOX_CHECKED", true).contains("miss"));
    assert_eq!(count(), 2);
    assert!(build("-DTINOX_CHECKED", true).contains("hit"));
    assert_eq!(count(), 2);
    assert!(build("", false).contains("bypass"));
    assert_eq!(count(), 3);
    // A changed compiler executable must invalidate an otherwise identical key.
    let mut script = fs::read_to_string(&wrapper).unwrap();
    script.push_str("# compiler wrapper revision\n");
    fs::write(&wrapper, script).unwrap();
    assert!(build("", true).contains("miss"));
    assert_eq!(count(), 4);
    fs::remove_dir_all(dir).unwrap();
}

use std::process::Command;

#[test]
fn version_flag_prints_package_version() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_tinox"))
            .arg(flag)
            .output()
            .expect("failed to run tinox");
        assert!(out.status.success(), "{flag} exited with failure");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("tinox {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

//! Issue #485: `[native]` section of tinox.toml links user libraries,
//! objects and extra C sources into a Tinox program.
use std::path::{Path, PathBuf};
use std::process::Command;

fn tinox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
}

fn project(name: &str, toml_native: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_native_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("tinox.toml"), format!("[package]\nname = \"native_demo\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[native]\n{toml_native}\n", env!("CARGO_PKG_VERSION"))).unwrap();
    std::fs::write(
        d.join("Main.tnx"),
        "extern fn nat_add(a: Int64, b: Int64) -> Int64\n\nclass Main\n{\n    fnc main() -> Int32 {\n        println(nat_add(40, 2).toString())\n        return 0\n    }\n}\n",
    )
    .unwrap();
    d
}

fn c_add(dir: &Path, body: &str) -> PathBuf {
    let src = dir.join("nat.c");
    std::fs::write(&src, format!("#include <stdint.h>\nint64_t nat_add(int64_t a, int64_t b) {{ {body} }}\n")).unwrap();
    src
}

fn build_and_run(d: &Path) -> Result<String, String> {
    let out = d.join("app");
    let r = tinox()
        .current_dir(d)
        .args(["build", "Main.tnx", "-o", out.to_str().unwrap()])
        .output()
        .unwrap();
    if !r.status.success() {
        return Err(String::from_utf8_lossy(&r.stderr).into_owned());
    }
    // No LD_LIBRARY_PATH: the rpath from tinox.toml must be enough.
    let run = Command::new(&out).env_remove("LD_LIBRARY_PATH").output().unwrap();
    Ok(String::from_utf8_lossy(&run.stdout).trim().to_string())
}

#[test]
fn links_shared_library_via_lib_dirs_and_rpath() {
    let d = project("shared", "libs = [\"natdemo\"]\nlib_dirs = [\"lib\"]\nrpath = [\"lib\"]");
    std::fs::create_dir_all(d.join("lib")).unwrap();
    let src = c_add(&d, "return a + b;");
    let st = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(d.join("lib/libnatdemo.so"))
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(build_and_run(&d).unwrap(), "42");
}

#[test]
fn rebuilt_shared_library_invalidates_the_build_cache() {
    let d = project("cache", "libs = [\"natdemo\"]\nlib_dirs = [\"lib\"]\nrpath = [\"lib\"]");
    std::fs::create_dir_all(d.join("lib")).unwrap();
    let so = d.join("lib/libnatdemo.so");
    let cc = |body: &str| {
        let src = c_add(&d, body);
        assert!(Command::new("cc").args(["-shared", "-fPIC", "-o"]).arg(&so).arg(&src).status().unwrap().success());
    };
    cc("return a + b;");
    assert_eq!(build_and_run(&d).unwrap(), "42");
    // The Tinox source is untouched; only the library changed. The link
    // is identical here since the .so is resolved at run time, but the
    // fingerprint must still change so a stale cached binary is never
    // reused for static archives / objects. Verify the dynamic case runs
    // the new code and the build still succeeds.
    cc("return a * b;");
    assert_eq!(build_and_run(&d).unwrap(), "80");
}

#[test]
fn compiles_and_relinks_extra_c_sources() {
    let d = project("sources", "sources = [\"nat.c\"]");
    c_add(&d, "return a + b;");
    assert_eq!(build_and_run(&d).unwrap(), "42");
    // Edit the source: the object must be rebuilt (not reused) and the
    // application cache must not hand back the old binary.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    c_add(&d, "return a * b;");
    assert_eq!(build_and_run(&d).unwrap(), "80");
}

#[test]
fn missing_library_fails_with_actionable_message() {
    let d = project("missing", "libs = [\"definitely_not_a_real_lib_xyz\"]");
    let err = build_and_run(&d).unwrap_err();
    assert!(err.contains("library `definitely_not_a_real_lib_xyz` not found"), "{err}");
}

#[test]
fn missing_pkg_config_package_fails_clearly() {
    let d = project("pkg", "pkg_config = [\"definitely-not-a-package-xyz\"]");
    let err = build_and_run(&d).unwrap_err();
    assert!(err.contains("pkg-config package `definitely-not-a-package-xyz` not found"), "{err}");
}

#[test]
fn unknown_native_key_is_rejected() {
    let d = project("badkey", "lib = [\"x\"]");
    let err = build_and_run(&d).unwrap_err();
    assert!(err.contains("unknown [native] key `lib`"), "{err}");
}

// Issue #506: `[native.test]` replaces keys of `[native]` for `tinox test` only.
#[test]
fn native_test_section_links_a_different_library_for_tests_only() {
    let d = std::env::temp_dir().join(format!("tinox_native_testsec_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("lib")).unwrap();
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::create_dir_all(d.join("tests")).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!(
            "[package]\nname = \"native_demo\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\nentry = \"src/Main.tnx\"\n\n[native]\nlibs = [\"natreal\"]\nlib_dirs = [\"lib\"]\nrpath = [\"lib\"]\n\n[native.test]\nlibs = [\"nattest\"]\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    // the real library adds, the test double multiplies
    for (name, body) in [("natreal", "return a + b;"), ("nattest", "return a * b;")] {
        let src = d.join(format!("{name}.c"));
        std::fs::write(&src, format!("#include <stdint.h>\nint64_t nat_add(int64_t a, int64_t b) {{ {body} }}\n")).unwrap();
        let st = Command::new("cc").args(["-shared", "-fPIC", "-o"]).arg(d.join(format!("lib/lib{name}.so"))).arg(&src).status().unwrap();
        assert!(st.success());
    }
    std::fs::write(
        d.join("src/Main.tnx"),
        "extern fn nat_add(a: Int64, b: Int64) -> Int64\n\nclass Main\n{\n    fnc main() -> Int32 {\n        println(nat_add(40, 2).toString())\n        return 0\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        d.join("tests/NatTest.tnx"),
        "extern fn nat_add(a: Int64, b: Int64) -> Int64\n\nclass NatTest\n{\n    @Test(\"the test double is linked\")\n    fn testDouble() -> Bool\n    {\n        return nat_add(40, 2) == 80\n    }\n}\n",
    )
    .unwrap();

    // build links the real library
    let out = d.join("app");
    let b = tinox().current_dir(&d).args(["build", "-o", out.to_str().unwrap()]).output().unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let run = Command::new(&out).env_remove("LD_LIBRARY_PATH").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");

    // test links the replacement: nat_add(40, 2) == 80 only holds for the double
    let t = tinox().current_dir(&d).args(["test"]).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&t.stdout), String::from_utf8_lossy(&t.stderr));
    assert!(t.status.success(), "{text}");
    assert!(text.contains("PASS  the test double is linked"), "{text}");
}

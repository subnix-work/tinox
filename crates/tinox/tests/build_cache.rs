//! #295: prove reuse by refusing every LLVM invocation and actual C build/link
//! after a successful build, then exercise invalidation through the real CLI.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project {
    root: PathBuf,
    path: std::ffi::OsString,
}
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tinox-build-cache-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        let mut paths = vec![root.join("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let path = std::env::join_paths(paths).unwrap();
        let p = Self { root, path };
        // Cargo may relink its output while stress tests run in parallel with
        // make check. Pin this fixture's compiler so it tests stable inputs.
        fs::copy(env!("CARGO_BIN_EXE_tinox"), p.root.join("tinox")).unwrap();
        p.write(
            "tinox.toml",
            "[package]\ncompiler = \"2.5.1\"\nname = \"cache\"\nversion = \"0.1.0\"\n",
        );
        p.write("Main.tnx", "import Value\nclass Main { fnc main() -> Int32 { println(Value::get().toString())\nreturn 0 } }");
        p.value(42);
        for tool in ["cc", "opt", "llc"] {
            let real = Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .unwrap();
            let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
            assert!(!real.is_empty());
            let body = if tool == "cc" {
                "for arg; do\n case \"$arg\" in -E|-###|--version|-print-*|-dump*) exec REAL \"$@\" ;; esac\ndone\n".replace("REAL", &format!("'{real}'"))
            } else {
                String::new()
            };
            p.write(&format!("bin/{tool}"), &format!("#!/bin/sh\n{body}echo '{tool}' >> '{}/backend.log'\nif [ -f '{}/blocked' ]; then echo backend-blocked >&2; exit 81; fi\nexec '{real}' \"$@\"\n", p.root.display(), p.root.display()));
            fs::set_permissions(
                p.root.join(format!("bin/{tool}")),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        p
    }
    fn write(&self, name: &str, body: &str) {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fn value(&self, value: i32) {
        self.write(
            "Value.tnx",
            &format!("class Value {{ fnc get() -> Int64 {{ return {value} }} }}"),
        );
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(self.root.join("tinox"));
        cmd.args(args)
            .current_dir(&self.root)
            .env("PATH", &self.path)
            .env("TINOX_CACHE_TRACE", "1")
            .env("TINOX_RUNTIME_CACHE_DIR", self.root.join("runtime"));
        cmd
    }
    fn invoke(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn build(&self) -> Output {
        self.invoke(&["build", "Main.tnx", "-o", "app", "--debug"])
    }
    fn count(&self) -> usize {
        fs::read_to_string(self.root.join("backend.log"))
            .unwrap_or_default()
            .lines()
            .count()
    }
    fn output(&self, expected: &str) {
        let out = Command::new(self.root.join("app")).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expected);
    }
    fn blocked(&self, blocked: bool) {
        if blocked {
            self.write("blocked", "");
        } else {
            fs::remove_file(self.root.join("blocked")).unwrap();
        }
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn success(out: &Output) {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn hit(out: &Output) {
    success(out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("[build-cache] hit"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unchanged_build_never_invokes_llvm_or_linker_and_checks_output_integrity() {
    let p = Project::new();
    success(&p.build());
    p.output("42");
    let count = p.count();
    assert!(count > 0);
    p.blocked(true);
    hit(&p.build());
    assert_eq!(count, p.count());
    fs::remove_file(p.root.join("app")).unwrap();
    assert!(!p.build().status.success(), "deleted output was reused");
    p.blocked(false);
    success(&p.build());
    p.write("app", "replaced executable");
    success(&p.build());
    p.output("42");
    fs::set_permissions(p.root.join("app"), fs::Permissions::from_mode(0o600)).unwrap();
    success(&p.build());
    p.output("42");
    // An owner without execute permission cannot borrow its group's execute
    // bit. Check actual OS behavior so this also works when tests run as root.
    fs::set_permissions(p.root.join("app"), fs::Permissions::from_mode(0o654)).unwrap();
    if Command::new(p.root.join("app")).output().is_err() {
        let count = p.count();
        success(&p.build());
        assert!(p.count() > count);
        p.output("42");
    }
}

#[test]
fn sources_config_flags_compiler_and_corrupt_metadata_invalidate() {
    let p = Project::new();
    success(&p.build());
    p.value(19);
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    p.output("19");
    p.write(
        "tinox.toml",
        "[package]\ncompiler = \"2.5.1\"\nname = \"cache-renamed\"\nversion = \"0.1.1\"\n",
    );
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    let count = p.count();
    let out = p
        .command(&["build", "Main.tnx", "-o", "app", "--debug"])
        .env("TINOX_CFLAGS", "-DTINOX_CHECKED")
        .output()
        .unwrap();
    success(&out);
    assert!(p.count() > count);
    success(&p.build());
    let wrapper = p.root.join("bin/llc");
    let mut body = fs::read_to_string(&wrapper).unwrap();
    body.push_str("# upgraded compiler\n");
    fs::write(wrapper, body).unwrap();
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    p.write(".app.tinox-build/manifest.yaml", "corrupt: [");
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
}

#[test]
fn failed_frontend_and_forced_failed_backend_leave_no_success_marker() {
    let p = Project::new();
    success(&p.build());
    p.write("Value.tnx", "class Value { broken");
    assert!(!p.build().status.success());
    assert!(!p.root.join(".app.tinox-build/manifest.yaml").exists());
    p.value(42);
    success(&p.build());
    p.blocked(true);
    assert!(!p
        .invoke(&["build", "Main.tnx", "-o", "app", "--debug", "--force"])
        .status
        .success());
    assert!(!p.root.join(".app.tinox-build/manifest.yaml").exists());
    p.blocked(false);
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    let count = p.count();
    success(
        &p.command(&["build", "Main.tnx", "-o", "app", "--debug"])
            .env("TINOX_BUILD_CACHE", "0")
            .output()
            .unwrap(),
    );
    assert!(p.count() > count);
    p.output("42");
}

#[test]
fn installed_dependency_replacement_changes_output() {
    let p = Project::new();
    p.write("tinox.toml", "[package]\ncompiler = \"2.5.1\"\nname = \"cache\"\nversion = \"0.1.0\"\n[[dependencies]]\ngroup = \"example\"\nartifactId = \"dep\"\nversion = \"1.0.0\"\nurl = \"https://example.invalid/dep.tar.gz\"\n");
    fs::remove_file(p.root.join("Value.tnx")).unwrap();
    let source = ".tinox/deps/example/dep/1.0.0/Value.tnx";
    p.write(source, "class Value { fnc get() -> Int64 { return 7 } }");
    success(&p.build());
    p.output("7");
    p.write(source, "class Value { fnc get() -> Int64 { return 8 } }");
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    p.output("8");
}

#[test]
fn run_reuses_immutable_binary_and_separate_outputs_do_not_alias() {
    let p = Project::new();
    let args = ["run", "Main.tnx", "--debug"];
    let first = p.invoke(&args);
    success(&first);
    assert_eq!(String::from_utf8_lossy(&first.stdout).trim(), "42");
    let count = p.count();
    p.blocked(true);
    let second = p.invoke(&args);
    hit(&second);
    assert_eq!(count, p.count());
    assert_eq!(String::from_utf8_lossy(&second.stdout).trim(), "42");
    p.blocked(false);
    success(&p.build());
    let other = p.invoke(&["build", "Main.tnx", "-o", "other", "--debug"]);
    success(&other);
    assert!(p.root.join("other").is_file());
    p.blocked(true);
    hit(&p.build());
    hit(&p.invoke(&["build", "Main.tnx", "-o", "other", "--debug"]));
}

#[test]
fn concurrent_same_output_builds_publish_one_complete_result() {
    let p = Project::new();
    let children: Vec<_> = (0..6)
        .map(|_| {
            p.command(&["build", "Main.tnx", "-o", "app", "--debug"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    for output in &outputs {
        success(output);
    }
    let diagnostics = outputs
        .iter()
        .map(|out| String::from_utf8_lossy(&out.stderr).into_owned())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert_eq!(
        fs::read_to_string(p.root.join("backend.log"))
            .unwrap()
            .lines()
            .filter(|line| *line == "llc")
            .count(),
        1,
        "{diagnostics}"
    );
    p.output("42");
    p.blocked(true);
    hit(&p.build());
}

#[test]
fn changed_linked_library_invalidates_even_when_program_behavior_is_unchanged() {
    let p = Project::new();
    let library = Command::new("cc")
        .arg("-print-file-name=libz.so")
        .output()
        .unwrap();
    let library = PathBuf::from(String::from_utf8(library.stdout).unwrap().trim());
    let libs = p.root.join("libs");
    fs::create_dir_all(&libs).unwrap();
    fs::copy(library, libs.join("libz.so.1")).unwrap();
    std::os::unix::fs::symlink("libz.so.1", libs.join("libz.so")).unwrap();
    let build = || {
        p.command(&["build", "Main.tnx", "-o", "app", "--debug"])
            .env("LIBRARY_PATH", &libs)
            .env("LD_LIBRARY_PATH", &libs)
            .output()
            .unwrap()
    };
    success(&build());
    p.blocked(true);
    hit(&build());
    p.blocked(false);
    // Trailing ELF bytes do not change execution, but replacing a link input
    // must still invalidate the build independently of its filename/mtime.
    let path = libs.join("libz.so.1");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"replacement");
    fs::write(path, bytes).unwrap();
    let count = p.count();
    success(&build());
    assert!(p.count() > count);
    p.output("42");
    p.blocked(true);
    hit(&build());
}

#[test]
fn force_and_unsupported_native_flags_rebuild_explicitly() {
    let p = Project::new();
    success(&p.build());
    let count = p.count();
    success(&p.invoke(&["build", "Main.tnx", "-o", "app", "--debug", "--force"]));
    assert!(p.count() > count);
    p.blocked(true);
    hit(&p.build());
    p.blocked(false);
    for _ in 0..2 {
        let count = p.count();
        let out = p
            .command(&["build", "Main.tnx", "-o", "app", "--debug"])
            .env("TINOX_CFLAGS", "-fwrapv")
            .output()
            .unwrap();
        success(&out);
        assert!(p.count() > count);
    }
    assert!(!p.root.join(".app.tinox-build/manifest.yaml").exists());
}

#[test]
fn missing_project_entry_is_an_error_and_invalidates_known_build_output() {
    let p = Project::new();
    p.write(
        "tinox.toml",
        "[package]\ncompiler = \"2.5.1\"\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"Main.tnx\"\n",
    );
    success(&p.build());
    assert!(p.root.join(".app.tinox-build/manifest.yaml").is_file());
    fs::remove_file(p.root.join("Main.tnx")).unwrap();
    assert!(!p.invoke(&["build"]).status.success());
    assert!(!p.root.join(".app.tinox-build/manifest.yaml").exists());
    assert!(!p.invoke(&["run"]).status.success());
}

#[test]
fn native_digest_memo_detects_same_size_compiler_change_with_restored_mtime() {
    let p = Project::new();
    let wrapper = p.root.join("bin/llc");
    let script = fs::read_to_string(&wrapper).unwrap() + "# version1\n";
    fs::write(&wrapper, &script).unwrap();
    let modified = fs::metadata(&wrapper).unwrap().modified().unwrap();
    // Old timestamps are deliberately required before a native digest can be
    // memoized; this ensures the test exercises that path, not the recent-file fallback.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    success(&p.build());
    p.blocked(true);
    hit(&p.build());
    p.blocked(false);
    fs::write(&wrapper, script.replace("version1", "version2")).unwrap();
    fs::File::options()
        .write(true)
        .open(&wrapper)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let count = p.count();
    success(&p.build());
    assert!(p.count() > count);
    p.blocked(true);
    hit(&p.build());
}

#[test]
fn non_executable_path_shadows_do_not_hide_actual_compiler_changes() {
    let p = Project::new();
    p.write("shadow/llc", "this is not an executable compiler\n");
    p.write("shadow/cc", "this is not an executable C compiler\n");
    let paths = std::env::join_paths(
        std::iter::once(p.root.join("shadow")).chain(std::env::split_paths(&p.path)),
    )
    .unwrap();
    let build = || {
        p.command(&["build", "Main.tnx", "-o", "app", "--debug"])
            .env("PATH", &paths)
            .output()
            .unwrap()
    };
    success(&build());
    p.blocked(true);
    hit(&build());
    p.blocked(false);
    let wrapper = p.root.join("bin/llc");
    let body = fs::read_to_string(&wrapper).unwrap() + "# real compiler changed\n";
    fs::write(wrapper, body).unwrap();
    let count = p.count();
    success(&build());
    assert!(p.count() > count);
    p.blocked(true);
    hit(&build());
}

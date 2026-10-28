//! Independent finite semantic contracts; see tests/fixtures/semantic_matrix/README.md.
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const BUILD_LIMIT: Duration = Duration::from_secs(60);
const RUN_LIMIT: Duration = Duration::from_secs(3);

struct Work(PathBuf);
impl Work {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tinox-semantic-matrix-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn build(&self, case: &str, mode: &str, executable: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .arg("build")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/semantic_matrix")
                    .join(case)
                    .join("Main.tnx"),
            )
            .args(match mode {
                "debug" => vec!["--debug"],
                "checked" => vec!["--checked"],
                "release" => vec![],
                _ => panic!("unknown mode"),
            })
            .arg("-o")
            .arg(executable)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"));
        command
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Captured {
    code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}
// Files avoid pipe backpressure even if a broken loop emits unlimited output.
// A process group also bounds compiler/tool descendants, not only the parent.
fn capture(mut command: Command, work: &Work, limit: Duration) -> Captured {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let out = work.0.join(format!("{id}.stdout"));
    let err = work.0.join(format!("{id}.stderr"));
    command
        .stdin(Stdio::null())
        .stdout(fs::File::create(&out).unwrap())
        .stderr(fs::File::create(&err).unwrap())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let start = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status, false);
        }
        if start.elapsed() >= limit {
            // SAFETY: child started its own process group; only that group is killed.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            break (child.wait().unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Captured {
        code: status.code(),
        timed_out,
        stdout: fs::read_to_string(out).unwrap(),
        stderr: fs::read_to_string(err).unwrap(),
    }
}
fn conforms(result: &Captured, expected: &str) -> bool {
    !result.timed_out
        && result.code == Some(0)
        && result.stdout == expected
        && result.stderr.is_empty()
}
fn expected(case: &str) -> &'static str {
    match case {
        "jumps" => "5\n26\n1\n3\n65\n195\n169\n",
        "unwind" => {
            "return inner\nreturn outer\n7\nthrow cleanup\nsecond\ncatch cleanup\n0\n1\ndone\n"
        }
        "aliasing" => "3\n7\n9\n6\n",
        "typed_calls" => "255\n-1\n127\ntrue\n2\n3\n",
        _ => panic!("missing independent oracle"),
    }
}
fn build(work: &Work, case: &str, mode: &str, executable: &Path) {
    let result = capture(work.build(case, mode, executable), work, BUILD_LIMIT);
    assert!(
        !result.timed_out && result.code == Some(0),
        "{case}/{mode} build: {result:?}"
    );
}
#[test]
fn independent_contracts_in_debug_release_and_checked_modes() {
    let work = Work::new();
    for case in ["jumps", "unwind", "aliasing", "typed_calls"] {
        for mode in ["debug", "release", "checked"] {
            let executable = work.0.join(format!("{case}-{mode}"));
            build(&work, case, mode, &executable);
            let result = capture(Command::new(executable), &work, RUN_LIMIT);
            assert!(
                conforms(&result, expected(case)),
                "{case}/{mode}: {result:?}"
            );
        }
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

#[test]
fn control_flow_and_type_lowering_mutants_are_detected() {
    let work = Work::new();
    // Resolve before adding the wrapper to PATH. No compiler source is changed.
    let resolved = Command::new("sh")
        .args(["-c", "command -v llc"])
        .output()
        .unwrap();
    assert!(resolved.status.success());
    let llc = PathBuf::from(String::from_utf8(resolved.stdout).unwrap().trim());
    assert!(llc.is_absolute());
    for case in ["jumps", "typed_calls"] {
        let executable = work.0.join(format!("{case}-original"));
        build(&work, case, "debug", &executable);
        let original = capture(Command::new(&executable), &work, RUN_LIMIT);
        assert!(
            conforms(&original, expected(case)),
            "mutation baseline: {original:?}"
        );
        let ir = fs::read_to_string(executable.with_extension("ll")).unwrap();
        let mutated = if case == "jumps" {
            let label = |prefix: &str| {
                ir.lines()
                    .find_map(|line| {
                        let (name, _) = line.split_once(':')?;
                        name.starts_with(prefix).then_some(name.to_string())
                    })
                    .unwrap()
            };
            // Reintroduce #513: continue jumps to the condition before advancing.
            ir.replacen(
                &format!("br label %{}", label("for_advance")),
                &format!("br label %{}", label("for_cond")),
                1,
            )
        } else {
            // Reintroduce the unsigned widening bug: 255 becomes -1.
            ir.replacen("zext i8", "sext i8", 1)
        };
        assert_ne!(mutated, ir, "mutation must actually change LLVM IR");
        let mutant_ir = work.0.join(format!("{case}-mutant.ll"));
        fs::write(&mutant_ir, mutated).unwrap();
        let wrappers = work.0.join(format!("{case}-tools"));
        fs::create_dir(&wrappers).unwrap();
        let marker = work.0.join(format!("{case}-mutation-applied"));
        let wrapper = wrappers.join("llc");
        fs::write(&wrapper, format!("#!/bin/sh\nset -eu\nfor input do\n case \"$input\" in\n *.ll) cp {} \"$input\"; touch {} ;;\n esac\ndone\nexec {} \"$@\"\n", quote(&mutant_ir), quote(&marker), quote(&llc))).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let mutant = work.0.join(format!("{case}-mutant"));
        let mut command = work.build(case, "debug", &mutant);
        let mut paths = vec![wrappers];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).unwrap());
        let compilation = capture(command, &work, BUILD_LIMIT);
        assert!(
            !compilation.timed_out && compilation.code == Some(0),
            "mutant must compile, not fail for unrelated reasons: {compilation:?}"
        );
        assert!(marker.exists(), "llc wrapper must apply mutation");
        let result = capture(Command::new(mutant), &work, Duration::from_millis(300));
        assert!(
            !conforms(&result, expected(case)),
            "oracle failed to detect {case} mutant: {result:?}"
        );
        if case == "jumps" {
            assert!(
                result.timed_out,
                "continue-before-advance must be classified as a hang: {result:?}"
            );
        } else {
            assert!(
                !result.timed_out && result.code == Some(0),
                "type mutant must run successfully: {result:?}"
            );
            assert!(
                result.stdout.starts_with("-1\n"),
                "must observe signed widening, not an unrelated difference: {result:?}"
            );
        }
    }
}

#[test]
fn timeout_is_distinct_from_successful_empty_output() {
    let work = Work::new();
    let mut empty = Command::new("sh");
    empty.args(["-c", "exit 0"]);
    let empty = capture(empty, &work, RUN_LIMIT);
    assert!(conforms(&empty, ""), "{empty:?}");
    let mut infinite = Command::new("sh");
    infinite.args(["-c", "while :; do :; done"]);
    let infinite = capture(infinite, &work, Duration::from_millis(100));
    assert!(
        infinite.timed_out && infinite.stdout.is_empty(),
        "{infinite:?}"
    );
    assert!(!conforms(&infinite, ""));
}

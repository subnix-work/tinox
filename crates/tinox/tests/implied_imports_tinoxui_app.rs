//! Issue #268: `@TinoxUIApp` implies `import tinox.core.websocket;` and
//! `import tinox.core.http_server;` -- both modules back ONLY the
//! compiler-generated bootstrap (WS accept loop / HTTP shell server), and
//! there is no version of a `@TinoxUIApp` class that wants a different
//! set. `tinox.core.ui` deliberately stays explicit: `Component` is a
//! name every real `@View` method writes, so implying it away would hide
//! exactly the "where does this name come from" information issue #194's
//! explicit-import rule exists to keep visible -- see
//! `implied_import_paths_for`'s own doc comment in crates/tinox/src/
//! main.rs for the full reasoning.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Stages a throwaway project with one `.tnx` file (an `@TinoxUIApp` class
/// plus a `Main.tnx` driver importing it) and `tinox.core.ui`'s source
/// copied in directly -- same "ui isn't published to tinox-central yet"
/// workaround `tinox_ui_annotated_hello.rs` already uses -- with real
/// `[[dependencies]]` on `http_server`/`websocket` so implied-import
/// resolution goes through the exact same `tinox install`ed-dependency
/// path a real project would.
fn stage_project(name: &str, app_imports: &str) -> PathBuf {
    let root = repo_root();
    let workdir = std::env::temp_dir().join(format!(
        "tinox-implied-imports-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir src");

    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }

    std::fs::write(
        workdir.join("src/MyApp.tnx"),
        format!(
            "{app_imports}\n\n@TinoxUIApp(18490, 18491)\nclass MyApp\n{{\n    var clicks: Int64;\n\n    @View\n    fn render() -> Component\n    {{\n        return Component::vbox([Component::label(\"Clicks: \" + this.clicks.toString())]);\n    }}\n}}\n"
        ),
    )
    .expect("write MyApp.tnx");
    std::fs::write(
        workdir.join("src/Main.tnx"),
        "import MyApp;\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0;\n    }\n}\n",
    )
    .expect("write Main.tnx");

    let toml = "[package]\nname = \"impliedimportstest\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"1.0.3\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\nversion = \"1.0.3\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"1.0.0\"\n";
    std::fs::write(workdir.join("tinox.toml"), toml).expect("write tinox.toml");

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox)
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    workdir
}

#[test]
fn tinoxui_app_compiles_with_only_ui_imported_explicitly() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = stage_project(
        "only-ui",
        "import tinox.core.ui;",
    );
    let exe = workdir.join("myapp_exe");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "expected build to succeed with only tinox.core.ui imported (websocket/http_server should be implied) — \
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn tinoxui_app_still_requires_explicit_ui_import() {
    // Negative control: tinox.core.ui is NOT implied (Component is a name
    // the user's own @View method always writes) -- omitting it must
    // still be a real compile error, not silently accepted.
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = stage_project("missing-ui", "");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(workdir.join("myapp_exe"))
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        !build.status.success(),
        "expected build to FAIL without an explicit tinox.core.ui import (Component is a user-referenced name, \
         not implied) — this must not silently start compiling"
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("Component"),
        "expected the unresolved-name error to mention Component, got: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn tinoxui_app_redundant_explicit_imports_still_work() {
    // Explicit imports must keep working unchanged, including a redundant
    // one that duplicates what's now implied (per the issue's own design
    // requirement).
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = stage_project(
        "redundant",
        "import tinox.core.websocket;\nimport tinox.core.http_server;\nimport tinox.core.ui;",
    );
    let exe = workdir.join("myapp_exe");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "expected build to succeed with redundant explicit imports of the now-implied modules — \
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn non_tinoxui_app_file_is_unaffected() {
    // A plain file with no @TinoxUIApp class at all must not have
    // websocket/http_server silently appear from nowhere -- confirms the
    // implication is gated on the annotation actually being present, not
    // unconditional.
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-implied-imports-unaffected-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir src");
    std::fs::write(
        workdir.join("src/Main.tnx"),
        "class Main\n{\n    fnc main() -> Int32\n    {\n        println(\"no ui here\");\n        return 0;\n    }\n}\n",
    )
    .expect("write Main.tnx");
    std::fs::write(
        workdir.join("tinox.toml"),
        "[package]\nname = \"noui\"\nversion = \"0.1.0\"\ndescription = \"\"\n",
    )
    .expect("write tinox.toml");

    let run = Command::new(tinox)
        .arg("run")
        .arg(workdir.join("src/Main.tnx"))
        .current_dir(&workdir)
        .output()
        .expect("spawn run");
    assert!(
        run.status.success(),
        "expected a plain program with no @TinoxUIApp class to compile/run normally — \
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("no ui here"));
    let _ = std::fs::remove_dir_all(&workdir);
}

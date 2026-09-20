//! Issue #269: `Component`'s "|"-joined list props (dropdown/radioGroup/
//! tabs/comboBox/checkboxGroup/menuBar) used to silently corrupt any item
//! containing "|" (it would misparse into extra items client-side, with
//! no error anywhere). Fixed via a single shared `Component::
//! encodePipeList` helper (Component.tnx) that throws a clear error
//! instead. This is a plain server-side compile+run check -- no browser/
//! WebSocket protocol involved, unlike the other tinox_ui_*.rs tests.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

#[test]
fn component_pipe_list_props_reject_embedded_pipe_and_still_join_normally() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-pipe-list-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    // ui isn't published to tinox-central yet -- stage its own source
    // directly, same approach every other tinox_ui_*.rs test already
    // uses (see tinox_ui_annotated_hello.rs).
    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }

    std::fs::write(
        workdir.join("src/Main.tnx"),
        r#"import tinox.core.ui;

class Main
{
    fnc main() -> Int32
    {
        // Normal case: unchanged wire encoding, still "|"-joined.
        let dd: Component = Component::dropdown(["a", "b", "c"], "a", fn(v: String) { });
        println("options=" + dd.props.get("options"));

        // An item containing "|" must throw loudly, not corrupt the
        // encoding silently -- the exact regression this issue is about.
        try
        {
            let bad: Component = Component::dropdown(["red|green", "blue"], "blue", fn(v: String) { });
            println("BUG: no throw");
        }
        catch (e: String)
        {
            println("threw: " + e);
        }

        // Second list-typed parameter (checkboxGroup's `selected`) is
        // ALSO validated, not just the first (`options`) -- confirms
        // every call site was actually migrated to the shared helper,
        // not just the first one found.
        try
        {
            let bad2: Component = Component::checkboxGroup(["x", "y"], ["x|y"], fn(v: String) { });
            println("BUG: no throw for selected");
        }
        catch (e: String)
        {
            println("threw for selected: " + e);
        }

        return 0;
    }
}
"#,
    )
    .expect("write Main.tnx");

    let toml = "[package]\nname = \"uipipelisttest\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"1.0.4\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\nversion = \"1.0.3\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"1.0.0\"\n";
    std::fs::write(workdir.join("tinox.toml"), toml).expect("write tinox.toml");

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

    let run = Command::new(tinox)
        .arg("run")
        .arg(workdir.join("src/Main.tnx"))
        .current_dir(&workdir)
        .output()
        .expect("spawn run");
    assert!(
        run.status.success(),
        "run failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("options=a|b|c"), "expected unchanged normal-case encoding, got: {stdout}");
    assert!(!stdout.contains("BUG:"), "a pipe-containing item was silently accepted instead of throwing: {stdout}");
    assert!(stdout.contains("threw:") && stdout.contains("red|green"), "expected a clear error naming the offending item, got: {stdout}");
    assert!(stdout.contains("threw for selected:"), "expected the second list parameter to be validated too, got: {stdout}");

    let _ = std::fs::remove_dir_all(&workdir);
}

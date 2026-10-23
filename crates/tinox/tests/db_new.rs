//! Scaffolding is offline: no compiler runtime or database tools are required.
use std::path::PathBuf;
use std::process::{Command, Output};

struct Project(PathBuf);
impl Project {
    fn new(tag: &str, changelog: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tinox-db-new-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("tinox.toml"),
            format!("[database]\ndriver = \"sqlite\"\nchangelog = \"{changelog}\"\n"),
        )
        .unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str], author: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tinox"));
        cmd.current_dir(self.0.join("src"))
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_COUNT", if author.is_some() { "1" } else { "0" });
        if let Some(author) = author {
            cmd.env("GIT_CONFIG_KEY_0", "user.name")
                .env("GIT_CONFIG_VALUE_0", author);
        }
        cmd.output().unwrap()
    }

    fn new_set(&self, args: &[&str]) -> Output {
        let out = self.run(args, Some("A: <Dev> & \"Team\""));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.0.join(path)).unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn creates_and_appends_yaml_preserving_text_and_order() {
    let p = Project::new("yaml", "migrations/changelog.yaml");
    p.new_set(&["db", "new", "Create people: #1"]);
    let first = p.read("migrations/changelog.yaml");
    let first_doc: serde_yaml::Value = serde_yaml::from_str(&first).unwrap();
    let cs = &first_doc["databaseChangeLog"][0]["changeSet"];
    assert_eq!(cs["author"].as_str(), Some("A: <Dev> & \"Team\""));
    assert_eq!(cs["comment"].as_str(), Some("Create people: #1"));
    assert!(cs["changes"].as_sequence().unwrap().is_empty());
    let first = format!("# Keep this comment\n{first}# And this TODO\n");
    std::fs::write(p.0.join("migrations/changelog.yaml"), &first).unwrap();
    p.new_set(&["db", "new", "Add email"]);
    let second = p.read("migrations/changelog.yaml");
    assert!(second.starts_with(&first));
    let doc: serde_yaml::Value = serde_yaml::from_str(&second).unwrap();
    assert_eq!(
        doc["databaseChangeLog"][0],
        first_doc["databaseChangeLog"][0]
    );
    assert!(
        doc["databaseChangeLog"][1]["changeSet"]["id"]
            .as_str()
            .unwrap()
            > cs["id"].as_str().unwrap()
    );
    // Completing the skeleton makes it a normal migration accepted by the engine.
    std::fs::write(
        p.0.join("migrations/changelog.yaml"),
        second.replace(
            "changes: []",
            "changes: [{dropTable: {tableName: example}}]",
        ),
    )
    .unwrap();
    let out = p.run(&["db", "validate"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn include_is_relative_and_never_overwrites_a_file() {
    let p = Project::new("include", "migrations/changelog.yaml");
    p.new_set(&["db", "new", "First", "--include", "parts/first.yaml"]);
    let root = p.read("migrations/changelog.yaml");
    let child = p.read("migrations/parts/first.yaml");
    let doc: serde_yaml::Value = serde_yaml::from_str(&root).unwrap();
    assert_eq!(
        doc["databaseChangeLog"][0]["include"]["file"].as_str(),
        Some("parts/first.yaml")
    );
    let out = p.run(
        &["db", "new", "Duplicate", "--include", "parts/first.yaml"],
        Some("Dev"),
    );
    assert!(!out.status.success());
    assert_eq!(p.read("migrations/changelog.yaml"), root);
    assert_eq!(p.read("migrations/parts/first.yaml"), child);
    // Existing includes determine ID ordering, even with an unfinished changeSet.
    let child = child.replace(
        child
            .lines()
            .find(|l| l.trim_start().starts_with("id:"))
            .unwrap(),
        "    id: 99999999999999999998-first",
    );
    std::fs::write(p.0.join("migrations/parts/first.yaml"), child).unwrap();
    p.new_set(&["db", "new", "Second"]);
    assert!(p
        .read("migrations/changelog.yaml")
        .contains("99999999999999999999-second"));
}

#[test]
fn xml_and_mixed_includes_escape_author_and_description() {
    let p = Project::new("xml", "changes.xml");
    std::fs::write(
        p.0.join("changes.xml"),
        "<?xml version=\"1.0\"?>\n<databaseChangeLog xmlns=\"example\"/>\n<!-- tail -->",
    )
    .unwrap();
    p.new_set(&["db", "new", "People <&> \"names\""]);
    let first = p.read("changes.xml");
    assert!(first.contains("A: &lt;Dev&gt; &amp; &quot;Team&quot;"));
    assert!(first.contains("People &lt;&amp;&gt; &quot;names&quot;"));
    assert!(first.ends_with("<!-- tail -->"));
    p.new_set(&["db", "new", "Next", "--include", "next.yaml"]);
    assert!(p
        .read("changes.xml")
        .contains("<include file=\"next.yaml\"/>"));
    let yaml: serde_yaml::Value = serde_yaml::from_str(&p.read("next.yaml")).unwrap();
    assert_eq!(
        yaml["databaseChangeLog"][0]["changeSet"]["comment"].as_str(),
        Some("Next")
    );
    // Validate through the independent migration parser after completing both skeletons.
    std::fs::write(
        p.0.join("changes.xml"),
        p.read("changes.xml").replace(
            "<!-- TODO: add changes here -->",
            "<dropTable tableName=\"example\"/>",
        ),
    )
    .unwrap();
    std::fs::write(
        p.0.join("next.yaml"),
        p.read("next.yaml").replace(
            "changes: []",
            "changes: [{dropTable: {tableName: example}}]",
        ),
    )
    .unwrap();
    let out = p.run(&["db", "validate"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn invalid_input_does_not_modify_the_changelog() {
    let p = Project::new("invalid", "changelog.yaml");
    for args in [
        &["db", "new"][..],
        &["db", "new", " "],
        &["db", "new", "x", "--bad"],
        &["db", "new", "x", "--include"],
    ] {
        assert!(!p.run(args, Some("Dev")).status.success());
        assert!(!p.0.join("changelog.yaml").exists());
    }
    let missing = p.run(&["db", "new", "x"], None);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("git user.name"));
    for original in [
        "databaseChangeLog: [broken",
        "databaseChangeLog:\n- include: {file: missing.yaml}\n",
        "databaseChangeLog:\n- include: {file: changelog.yaml}\n",
    ] {
        std::fs::write(p.0.join("changelog.yaml"), original).unwrap();
        assert!(!p.run(&["db", "new", "x"], Some("Dev")).status.success());
        assert_eq!(p.read("changelog.yaml"), original);
    }
}

#[test]
fn flow_yaml_and_document_end_keep_existing_content() {
    let p = Project::new("flow", "changelog.yaml");
    for original in [
        "{databaseChangeLog: []} # keep\n",
        "databaseChangeLog:\n- changeSet: {id: 1, author: a, changes: []}\n...\n# tail\n",
    ] {
        std::fs::write(p.0.join("changelog.yaml"), original).unwrap();
        p.new_set(&["db", "new", "Flow"]);
        let text = p.read("changelog.yaml");
        assert!(text.contains(if original.contains("# keep") {
            "# keep"
        } else {
            "# tail"
        }));
        let doc: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(
            doc["databaseChangeLog"].as_sequence().unwrap().len(),
            if original.contains("id: 1") { 2 } else { 1 }
        );
    }
}

#[test]
fn creates_xml_include_and_accepts_scalar_like_git_names() {
    let p = Project::new("xml-include", "migrations/changelog.yaml");
    p.new_set(&["db", "new", "First", "--include", "first.xml"]);
    assert!(p.read("migrations/first.xml").contains("<changeSet"));
    let p = Project::new("xml-create", "migrations/changelog.xml");
    let out = p.run(&["db", "new", "First"], Some("true"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(p
        .read("migrations/changelog.xml")
        .contains("author=\"true\""));
    let out = p.run(&["db", "new", "Second"], Some("123"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

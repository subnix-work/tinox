//! Issue #508: compare native filesystem behavior with independent OS metadata.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

struct Tree(PathBuf);
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn metadata_batch_atomic_directory_rename_and_no_follow_removal() {
    exercise(false);
}
#[test]
fn checked_metadata_batch_atomic_directory_rename_and_no_follow_removal() {
    exercise(true);
}
fn exercise(checked: bool) {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tree = Tree(std::env::temp_dir().join(format!(
        "tinox-fs-metadata-{}-{checked}",
        std::process::id()
    )));
    let root = &tree.0;
    let _ = fs::remove_dir_all(root);
    for dir in ["source", "outside/child", "large", "home", "denied"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("source/file"), b"hello").unwrap();
    fs::File::open(root.join("source/file"))
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH - std::time::Duration::new(1, 123)),
        )
        .unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("source/socket")).unwrap();
    fs::write(root.join("denied/entry"), b"hidden").unwrap();
    fs::set_permissions(root.join("denied"), fs::Permissions::from_mode(0o0)).unwrap();
    fs::set_permissions(root.join("source/file"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(root.join("source"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::write(root.join("outside/child/sentinel"), b"keep me").unwrap();
    symlink("../outside", root.join("source/link")).unwrap();
    symlink("../absent", root.join("source/dangling")).unwrap();
    symlink("outside", root.join("rootlink")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    for i in 0..5000 {
        fs::write(root.join(format!("large/{i:04}")), b"abc").unwrap();
    }
    let metadata = fs::symlink_metadata(root.join("source/file")).unwrap();
    let nanos = metadata.mtime() as i128 * 1_000_000_000 + metadata.mtime_nsec() as i128;
    // The source fixture does not use a shell or external filesystem tools.
    let mut compiler = Command::new(env!("CARGO_BIN_EXE_tinox"));
    compiler
        .args(["build", "tests/fixtures/filesystem_metadata/Main.tnx", "-o"])
        .arg(root.join("probe"))
        .current_dir(&repo);
    if checked {
        compiler.arg("--checked");
    }
    let build = compiler.output().unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    // Exercise permission denial as an unprivileged user even on root CI.
    let mut permission = Command::new(root.join("probe"));
    permission
        .env("TINOX_FS_CASE", root)
        .env("TINOX_FS_MODE", "permission");
    if fs::metadata(root).unwrap().uid() == 0 {
        permission.uid(65534);
    }
    let denied = permission.output().unwrap();
    assert!(
        denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    assert_eq!(String::from_utf8(denied.stdout).unwrap(), "true\ntrue\n");
    fs::set_permissions(root.join("denied"), fs::Permissions::from_mode(0o755)).unwrap();
    // EXDEV must be visible and leave the source untouched. /dev/shm is an
    // independent tmpfs on typical Linux hosts; skip only if not available.
    if let Ok(other) = fs::metadata("/dev/shm") {
        if other.dev() != metadata.dev() {
            let cross = PathBuf::from(format!(
                "/dev/shm/tinox-fs-cross-{}-{checked}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&cross);
            let result = Command::new(root.join("probe"))
                .env("TINOX_FS_CASE", root)
                .env("TINOX_FS_MODE", "cross-device")
                .env("TINOX_FS_CROSS", &cross)
                .output()
                .unwrap();
            assert!(result.status.success());
            assert_eq!(String::from_utf8(result.stdout).unwrap(), "true\n");
            assert!(root.join("source/file").exists());
            assert!(!cross.exists());
        }
    }
    let kept = Command::new(root.join("probe"))
        .env("TINOX_FS_CASE", root)
        .env("TINOX_FS_MODE", "keep-rename")
        .output()
        .unwrap();
    assert!(
        kept.status.success(),
        "{}",
        String::from_utf8_lossy(&kept.stderr)
    );
    let renamed = fs::metadata(root.join("renamed")).unwrap();
    assert_eq!(renamed.permissions().mode() & 0o7777, 0o750);
    assert_eq!(fs::read(root.join("renamed/file")).unwrap(), b"hello");
    assert_eq!(
        fs::metadata(root.join("renamed/file")).unwrap().ino(),
        metadata.ino()
    );
    fs::rename(root.join("renamed"), root.join("source")).unwrap();
    let output = Command::new(root.join("probe"))
        .env("TINOX_FS_CASE", root)
        .env("HOME", root.join("home"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let expected = format!("file\n5\n416\n{nanos}\ntrue\ntrue\ntrue\ntrue\nfalse\nfalse\ntrue\nother\n5000\n3\n5000\ntrue\nfalse\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue");
    assert_eq!(lines.join("\n"), expected);
    assert_eq!(
        fs::read(root.join("outside/child/sentinel")).unwrap(),
        b"keep me"
    );
    assert!(!root.join("source").exists());
    assert!(!root.join("renamed").exists());
    assert!(fs::symlink_metadata(root.join("rootlink")).is_err());
    assert_eq!(fs::read_dir(root.join("large")).unwrap().count(), 5000);
}

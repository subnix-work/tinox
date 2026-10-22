//! Invalidate persisted queries whenever compiler sources, dependencies, or the
//! Rust toolchain change, including local builds that keep the same version.
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command};

fn sources(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, files);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "toml")
        {
            files.push(path);
        }
    }
}
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for name in [
        "tinox-common",
        "tinox-lexer",
        "tinox-parser",
        "tinox-typecheck",
        "tinox",
        "tinox-lsp",
    ] {
        let dir = root.join("crates").join(name);
        sources(&dir.join("src"), &mut files);
        files.push(dir.join("Cargo.toml"));
    }
    files.extend([
        root.join("Cargo.lock"),
        root.join("Cargo.toml"),
        root.join("crates/tinox-common/build.rs"),
    ]);
    let mut hash = Sha256::new();
    hash.update(b"tinox-frontend-v1\0");
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        hash.update(
            path.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        );
        let content = fs::read(&path).unwrap();
        hash.update((content.len() as u64).to_le_bytes());
        hash.update(content);
    }
    for var in ["TARGET", "PROFILE", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        println!("cargo:rerun-if-env-changed={var}");
        hash.update(env::var(var).unwrap_or_default().as_bytes());
        hash.update([0]);
    }
    let rustc = Command::new(env::var_os("RUSTC").unwrap())
        .arg("-vV")
        .output()
        .unwrap();
    assert!(rustc.status.success());
    hash.update(rustc.stdout);
    println!(
        "cargo:rustc-env=TINOX_FRONTEND_SCHEMA={:x}",
        hash.finalize()
    );
}

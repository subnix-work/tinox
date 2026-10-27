//! `[native]` section of `tinox.toml` (issue #485): link third-party native
//! libraries, objects and extra C/C++ sources into a Tinox program.
//!
//! ```toml
//! [native]
//! libs       = ["tinoxkde"]            # -l<name>
//! lib_dirs   = ["bridge/build"]        # -L<dir>, relative to tinox.toml
//! rpath      = ["bridge/build"]        # -Wl,-rpath,<abs dir>; `$ORIGIN/..` kept verbatim
//! pkg_config = ["Qt6Widgets"]          # pkg-config --libs (and --cflags for `sources`)
//! objects    = ["vendor/shim.o"]       # linked as-is
//! sources    = ["native/shim.c"]       # compiled (.c with cc, .cc/.cpp/.cxx with c++)
//! cflags     = ["-O2", "-Ivendor"]     # only used to compile `sources`
//! ```
//!
//! Every problem is a hard error with an actionable message: unknown keys,
//! missing files, a library the linker cannot find, a pkg-config package
//! that does not exist.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default, PartialEq)]
pub struct NativeConfig {
    /// Directory containing the `tinox.toml` the section came from.
    pub root: PathBuf,
    pub libs: Vec<String>,
    pub lib_dirs: Vec<PathBuf>,
    pub rpath: Vec<String>,
    pub pkg_config: Vec<String>,
    pub objects: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
    pub cflags: Vec<String>,
}

/// Looks for the nearest `tinox.toml` from the current directory upwards
/// (same lookup as every other tinox.toml reader) and parses `[native]`.
pub fn read_native_config() -> Result<Option<NativeConfig>, String> {
    let Ok(mut dir) = std::env::current_dir() else { return Ok(None) };
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path)
                .map_err(|e| format!("cannot read {}: {e}", toml_path.display()))?;
            return parse(&content, &dir);
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

/// Parses the `[native]` table out of a tinox.toml's text. Returns `None`
/// when the file has no such section.
pub fn parse(content: &str, root: &Path) -> Result<Option<NativeConfig>, String> {
    let mut cfg = NativeConfig { root: root.to_path_buf(), ..Default::default() };
    let mut in_native = false;
    let mut found = false;
    let mut lines = content.lines().enumerate();
    while let Some((idx, raw)) = lines.next() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_native = line == "[native]";
            found |= in_native;
            continue;
        }
        if !in_native {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("tinox.toml line {}: [native] expects `key = value`", idx + 1))?;
        let key = key.trim();
        let mut value = value.trim().to_string();
        // Arrays may span several lines.
        while value.starts_with('[') && !value.contains(']') {
            let (_, next) = lines
                .next()
                .ok_or_else(|| format!("tinox.toml: unterminated array for [native] {key}"))?;
            value.push(' ');
            value.push_str(strip_comment(next).trim());
        }
        let items = parse_string_array(&value)
            .ok_or_else(|| format!("tinox.toml line {}: [native] {key} must be an array of strings", idx + 1))?;
        match key {
            "libs" => cfg.libs = items,
            "lib_dirs" => cfg.lib_dirs = items.iter().map(|s| root.join(s)).collect(),
            "rpath" => cfg.rpath = items,
            "pkg_config" => cfg.pkg_config = items,
            "objects" => cfg.objects = items.iter().map(|s| root.join(s)).collect(),
            "sources" => cfg.sources = items.iter().map(|s| root.join(s)).collect(),
            "cflags" => cfg.cflags = items,
            other => {
                return Err(format!(
                    "tinox.toml line {}: unknown [native] key `{other}` (expected libs, lib_dirs, rpath, pkg_config, objects, sources, cflags)",
                    idx + 1
                ))
            }
        }
    }
    Ok(found.then_some(cfg))
}

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_string_array(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(part.strip_prefix('"')?.strip_suffix('"')?.to_string());
    }
    Some(out)
}

impl NativeConfig {
    /// Validates the configuration, compiles `sources`, and returns the
    /// extra linker arguments in link order.
    pub fn link_flags(&self) -> Result<Vec<String>, String> {
        for dir in &self.lib_dirs {
            if !dir.is_dir() {
                return Err(format!("[native] lib_dirs entry `{}` is not a directory", dir.display()));
            }
        }
        for obj in &self.objects {
            if !obj.is_file() {
                return Err(format!("[native] objects entry `{}` does not exist", obj.display()));
            }
        }
        for src in &self.sources {
            if !src.is_file() {
                return Err(format!("[native] sources entry `{}` does not exist", src.display()));
            }
        }

        let mut pkg_libs = Vec::new();
        let mut pkg_cflags = Vec::new();
        for pkg in &self.pkg_config {
            pkg_libs.extend(pkg_config_output(pkg, "--libs")?);
            pkg_cflags.extend(pkg_config_output(pkg, "--cflags")?);
        }

        let mut flags: Vec<String> = Vec::new();
        for obj in &self.objects {
            flags.push(obj.to_string_lossy().into_owned());
        }
        let mut cxx = false;
        for src in &self.sources {
            let (obj, is_cxx) = self.compile_source(src, &pkg_cflags)?;
            cxx |= is_cxx;
            flags.push(obj.to_string_lossy().into_owned());
        }
        for dir in &self.lib_dirs {
            flags.push(format!("-L{}", dir.display()));
        }
        for entry in &self.rpath {
            // `$ORIGIN`-style entries are meant for the dynamic loader as-is.
            let value = if entry.starts_with('$') {
                entry.clone()
            } else {
                self.root.join(entry).to_string_lossy().into_owned()
            };
            flags.push(format!("-Wl,-rpath,{value}"));
        }
        for lib in &self.libs {
            self.check_lib_exists(lib)?;
            flags.push(format!("-l{lib}"));
        }
        flags.extend(pkg_libs);
        if cxx {
            flags.push("-lstdc++".into());
        }
        Ok(flags)
    }

    fn check_lib_exists(&self, lib: &str) -> Result<(), String> {
        for dir in &self.lib_dirs {
            if dir.join(format!("lib{lib}.so")).exists() || dir.join(format!("lib{lib}.a")).exists() {
                return Ok(());
            }
        }
        // Ask the compiler driver: it echoes the bare name back when the
        // file is not on its search path.
        for ext in ["so", "a"] {
            let name = format!("lib{lib}.{ext}");
            if let Ok(out) = Command::new("cc").arg(format!("-print-file-name={name}")).output() {
                let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if out.status.success() && printed != name && Path::new(&printed).exists() {
                    return Ok(());
                }
            }
        }
        Err(format!(
            "[native] library `{lib}` not found (looked for lib{lib}.so / lib{lib}.a in lib_dirs and the compiler's search path); \
             check `libs`/`lib_dirs` in tinox.toml or install the development package"
        ))
    }

    /// Compiles one extra source into `<root>/.tinox/native/` and returns the
    /// object path. The object is reused when it is newer than the source and
    /// every header the compiler reported in its dependency file.
    fn compile_source(&self, src: &Path, pkg_cflags: &[String]) -> Result<(PathBuf, bool), String> {
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("");
        let (compiler, is_cxx) = match ext {
            "c" => ("cc", false),
            "cc" | "cpp" | "cxx" | "C" => ("c++", true),
            other => {
                return Err(format!(
                    "[native] sources entry `{}` has unsupported extension `.{other}` (use .c, .cc, .cpp or .cxx)",
                    src.display()
                ))
            }
        };
        let mut key = Sha256::new();
        key.update(src.to_string_lossy().as_bytes());
        key.update(compiler.as_bytes());
        for f in self.cflags.iter().chain(pkg_cflags) {
            key.update(f.as_bytes());
            key.update([0]);
        }
        let digest = key.finalize();
        let tag: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("native");
        let out_dir = self.root.join(".tinox").join("native");
        fs::create_dir_all(&out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
        let obj = out_dir.join(format!("{stem}-{tag}.o"));
        let dep = out_dir.join(format!("{stem}-{tag}.d"));
        if is_up_to_date(&obj, &dep, src) {
            return Ok((obj, is_cxx));
        }
        let output = Command::new(compiler)
            .args(["-c", "-fPIC", "-O2", "-MMD", "-MF"])
            .arg(&dep)
            .args(&self.cflags)
            .args(pkg_cflags)
            .arg(src)
            .arg("-o")
            .arg(&obj)
            .output()
            .map_err(|e| format!("failed to run {compiler} for [native] source {}: {e}", src.display()))?;
        if !output.status.success() {
            let _ = fs::remove_file(&obj);
            return Err(format!(
                "{compiler} failed for [native] source {} ({}):\n{}{}",
                src.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok((obj, is_cxx))
    }
}

fn is_up_to_date(obj: &Path, dep: &Path, src: &Path) -> bool {
    let Ok(obj_time) = fs::metadata(obj).and_then(|m| m.modified()) else { return false };
    let newer = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).map(|t| t > obj_time).unwrap_or(true);
    if newer(src) {
        return false;
    }
    let Ok(deps) = fs::read_to_string(dep) else { return false };
    // Make-style: `target.o: a.c b.h \` ... (no spaces inside paths assumed;
    // an unparsable path counts as stale so we never reuse on doubt).
    deps.replace("\\\n", " ")
        .split_whitespace()
        .skip(1)
        .all(|p| !newer(Path::new(p)))
}

fn pkg_config_output(pkg: &str, what: &str) -> Result<Vec<String>, String> {
    let out = Command::new("pkg-config")
        .arg(what)
        .arg(pkg)
        .output()
        .map_err(|e| format!("[native] pkg_config needs the `pkg-config` tool: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "[native] pkg-config package `{pkg}` not found: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).split_whitespace().map(String::from).collect())
}

/// Files whose content must be part of the build-cache fingerprint: explicit
/// objects/archives on the link line and `-l<name>` resolved through the
/// `-L` directories given alongside. System libraries found only on the
/// compiler's default path are covered by the existing toolchain hashing.
pub fn fingerprint_inputs(link_flags: &[&str]) -> Vec<PathBuf> {
    let dirs: Vec<&Path> = link_flags.iter().filter_map(|f| f.strip_prefix("-L")).map(Path::new).collect();
    let mut files = Vec::new();
    for flag in link_flags {
        if let Some(name) = flag.strip_prefix("-l") {
            for dir in &dirs {
                for ext in ["so", "a"] {
                    let p = dir.join(format!("lib{name}.{ext}"));
                    if p.is_file() {
                        files.push(p);
                    }
                }
            }
        } else if !flag.starts_with('-') && Path::new(flag).is_file() {
            files.push(PathBuf::from(flag));
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_keys_and_multiline_arrays() {
        let toml = r#"
[package]
name = "x"

[native]
libs = ["a", "b"]   # trailing comment
lib_dirs = [
    "build",
    "vendor/lib",
]
rpath = ["build", "$ORIGIN/../lib"]
pkg_config = ["Qt6Widgets"]
objects = ["x.o"]
sources = ["s.c"]
cflags = ["-O1", "-Ivendor"]

[database]
driver = "sqlite"
"#;
        let cfg = parse(toml, Path::new("/proj")).unwrap().unwrap();
        assert_eq!(cfg.libs, ["a", "b"]);
        assert_eq!(cfg.lib_dirs, [PathBuf::from("/proj/build"), PathBuf::from("/proj/vendor/lib")]);
        assert_eq!(cfg.rpath, ["build", "$ORIGIN/../lib"]);
        assert_eq!(cfg.pkg_config, ["Qt6Widgets"]);
        assert_eq!(cfg.objects, [PathBuf::from("/proj/x.o")]);
        assert_eq!(cfg.sources, [PathBuf::from("/proj/s.c")]);
        assert_eq!(cfg.cflags, ["-O1", "-Ivendor"]);
    }

    #[test]
    fn missing_section_is_none() {
        assert!(parse("[package]\nname = \"x\"\n", Path::new("/p")).unwrap().is_none());
    }

    #[test]
    fn unknown_key_is_an_error() {
        let err = parse("[native]\nlib = [\"a\"]\n", Path::new("/p")).unwrap_err();
        assert!(err.contains("unknown [native] key `lib`"), "{err}");
    }

    #[test]
    fn non_array_value_is_an_error() {
        let err = parse("[native]\nlibs = \"a\"\n", Path::new("/p")).unwrap_err();
        assert!(err.contains("array of strings"), "{err}");
    }

    #[test]
    fn missing_library_is_reported_clearly() {
        let cfg = NativeConfig { libs: vec!["definitely_not_a_real_lib_xyz".into()], ..Default::default() };
        let err = cfg.link_flags().unwrap_err();
        assert!(err.contains("library `definitely_not_a_real_lib_xyz` not found"), "{err}");
    }

    #[test]
    fn fingerprint_inputs_resolves_libs_via_dash_l() {
        let dir = std::env::temp_dir().join(format!("tinox_native_fp_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("libfoo.so"), b"x").unwrap();
        fs::write(dir.join("bar.o"), b"x").unwrap();
        let l = format!("-L{}", dir.display());
        let o = dir.join("bar.o").to_string_lossy().into_owned();
        let files = fingerprint_inputs(&[&o, &l, "-lfoo", "-lm"]);
        assert!(files.contains(&dir.join("libfoo.so")));
        assert!(files.contains(&dir.join("bar.o")));
        assert_eq!(files.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}

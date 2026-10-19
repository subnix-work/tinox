//! Content-addressed runtime objects. Compile the exact preprocessor snapshot
//! that was hashed, so changing a header during a build cannot poison the key.
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn trace(message: &str) {
    if env::var_os("TINOX_CACHE_TRACE").is_some_and(|v| v == "1") {
        eprintln!("[runtime-cache] {message}");
    }
}
fn executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return path.canonicalize().ok();
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        let p = dir.join(name);
        p.is_file().then(|| p.canonicalize().ok()).flatten()
    })
}
fn command_output(compiler: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(compiler)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "C compiler failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out.stdout)
}

pub fn cache_dir() -> Option<PathBuf> {
    env::var_os("TINOX_RUNTIME_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .map(|p| p.join("tinox/runtime-v1"))
        })
}

/// Arbitrary compiler extensions can read inputs that the preprocessor cannot
/// describe (plugins, specs, profiles, response files, native CPU probing).
/// Preserve those builds by compiling normally instead of guessing a key.
fn supported_flags(flags: &[&str]) -> bool {
    flags.iter().all(|flag| {
        flag.starts_with("-D")
            || flag.starts_with("-U")
            || flag.starts_with("-I")
            || matches!(
                *flag,
                "-O0"
                    | "-O1"
                    | "-O2"
                    | "-O3"
                    | "-Os"
                    | "-Og"
                    | "-g"
                    | "-g0"
                    | "-g1"
                    | "-g2"
                    | "-g3"
                    | "-pthread"
                    | "-fPIC"
                    | "-fpic"
                    | "-fno-omit-frame-pointer"
            )
            || flag.starts_with("-fsanitize=")
            || flag.starts_with("-fno-sanitize=")
    })
}

#[derive(Clone)]
struct RuntimeBuildConfig {
    compiler: PathBuf,
    preprocessed: Vec<u8>,
    key: String,
}
impl RuntimeBuildConfig {
    fn capture(source: &str, flags: &[&str]) -> Result<Option<Self>, String> {
        if !supported_flags(flags) || env::var_os("LD_PRELOAD").is_some() {
            trace("bypass: flags or loader configuration require an uncached build");
            return Ok(None);
        }
        let Some(compiler) = executable("cc") else {
            return Ok(None);
        };
        let mut hash = Sha256::new();
        // Length-prefix every field; concatenation must not alias configurations.
        let mut field = |data: &[u8]| {
            hash.update((data.len() as u64).to_le_bytes());
            hash.update(data);
        };
        field(b"tinox-runtime-cache-v1");
        field(env!("CARGO_PKG_VERSION").as_bytes());
        field(compiler.to_string_lossy().as_bytes());
        field(&fs::read(&compiler).map_err(|e| e.to_string())?);
        let version = command_output(&compiler, &["--version"])?;
        field(&version);
        field(&command_output(&compiler, &["-dumpmachine"])?);
        for tool in ["cc1", "as"] {
            let name = command_output(&compiler, &[&format!("-print-prog-name={tool}")])?;
            let name = String::from_utf8_lossy(&name).trim().to_string();
            if let Some(path) = executable(&name) {
                field(path.to_string_lossy().as_bytes());
                field(&fs::read(path).map_err(|e| e.to_string())?);
            } else if tool == "cc1" && String::from_utf8_lossy(&version).contains("clang") {
                // Clang's cc1 is integrated into the already hashed driver.
                field(b"integrated-cc1");
            } else {
                trace("bypass: cannot fingerprint C compiler tools");
                return Ok(None);
            }
        }
        for flag in flags {
            field(flag.as_bytes());
        }
        for name in [
            "CPATH",
            "C_INCLUDE_PATH",
            "GCC_EXEC_PREFIX",
            "COMPILER_PATH",
            "SOURCE_DATE_EPOCH",
            "SDKROOT",
            "MACOSX_DEPLOYMENT_TARGET",
        ] {
            field(name.as_bytes());
            field(env::var_os(name).unwrap_or_default().as_encoded_bytes());
        }
        // Ask the driver for its effective backend configuration without
        // temporary assembly filenames. This includes default specs/config
        // changes and target defaults that are not visible in --version.
        let plan = Command::new(&compiler)
            .args(flags)
            .args(["-###", "-S", source, "-o", "/dev/null"])
            .output()
            .map_err(|e| format!("Cannot inspect C compiler configuration: {e}"))?;
        if !plan.status.success() {
            trace("bypass: C compiler cannot describe its backend configuration");
            return Ok(None);
        }
        field(&plan.stdout);
        field(&plan.stderr);
        let out = Command::new(&compiler)
            .args(flags)
            .args(["-E", source])
            .output()
            .map_err(|e| format!("Failed to preprocess runtime: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Runtime preprocessing failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        // Preserve compiler warnings even on a cache hit.
        if !out.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
        }
        field(&out.stdout);
        Ok(Some(Self {
            compiler,
            preprocessed: out.stdout,
            key: format!("{:x}", hash.finalize()),
        }))
    }
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn temp_dir(root: &Path) -> Result<TempDir, String> {
    loop {
        let path = root.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(TempDir(path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "Cannot create runtime cache staging directory: {e}"
                ))
            }
        }
    }
}
fn valid_entry(entry: &Path) -> bool {
    match (
        fs::read(entry.join("runtime.o")),
        fs::read_to_string(entry.join("sha256")),
    ) {
        (Ok(object), Ok(expected)) => !object.is_empty() && digest(&object) == expected,
        _ => false,
    }
}

/// Return an immutable cached object, or None to use the ordinary compiler path.
/// Publication is an atomic directory rename. No reader ever sees a partial
/// object, and simultaneous producers may safely compile the same key.
pub fn runtime_object(source: &str, flags: &[&str]) -> Result<Option<PathBuf>, String> {
    if env::var_os("TINOX_RUNTIME_CACHE").is_some_and(|v| v == "0") {
        trace("bypass: disabled by TINOX_RUNTIME_CACHE=0");
        return Ok(None);
    }
    let Some(root) = cache_dir() else {
        trace("bypass: no cache directory");
        return Ok(None);
    };
    let Some(config) = RuntimeBuildConfig::capture(source, flags)? else {
        return Ok(None);
    };
    cached_object(&root, config, flags).map(Some)
}

fn cached_object(
    root: &Path,
    config: RuntimeBuildConfig,
    flags: &[&str],
) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|e| format!("Cannot create runtime cache: {e}"))?;
    // The kernel releases this lock on process death, including interrupted
    // compilation. Separate keys never block one another.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{}.lock", config.key)))
        .map_err(|e| e.to_string())?;
    lock.lock()
        .map_err(|e| format!("Cannot lock runtime cache entry: {e}"))?;
    let entry = root.join(&config.key);
    if valid_entry(&entry) {
        trace(&format!("hit {}", config.key));
        return Ok(entry.join("runtime.o"));
    }
    trace(&format!("miss {} (absent or invalid entry)", config.key));
    let temp = temp_dir(root)?;
    let snapshot = temp.0.join("runtime.i");
    let object = temp.0.join("runtime.o");
    fs::write(&snapshot, &config.preprocessed).map_err(|e| e.to_string())?;
    let status = Command::new(&config.compiler)
        .args(flags)
        .args(["-x", "cpp-output", "-c"])
        .arg(&snapshot)
        .arg("-o")
        .arg(&object)
        .status()
        .map_err(|e| format!("Failed to compile runtime: {e}"))?;
    if !status.success() {
        return Err("Runtime compilation failed".into());
    }
    let bytes = fs::read(&object).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("C compiler produced an empty runtime object".into());
    }
    fs::write(temp.0.join("sha256"), digest(&bytes)).map_err(|e| e.to_string())?;
    fs::remove_file(&snapshot).map_err(|e| e.to_string())?;
    if entry.exists() {
        fs::remove_dir_all(&entry)
            .map_err(|e| format!("Cannot remove invalid runtime cache entry: {e}"))?;
    }
    fs::rename(&temp.0, &entry).map_err(|e| format!("Cannot publish runtime cache: {e}"))?;
    Ok(entry.join("runtime.o"))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let base = env::temp_dir().join(format!(
                "tinox-runtime-unit-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("value.h"), "#define VALUE 42\n").unwrap();
            fs::write(
                base.join("runtime.c"),
                "#include \"value.h\"\nint answer(void) { return VALUE; }\n",
            )
            .unwrap();
            Self(base)
        }
        fn config(&self, flags: &[&str]) -> RuntimeBuildConfig {
            RuntimeBuildConfig::capture(self.0.join("runtime.c").to_str().unwrap(), flags)
                .unwrap()
                .unwrap()
        }
        fn object(&self, flags: &[&str]) -> PathBuf {
            cached_object(&self.0.join("cache"), self.config(flags), flags).unwrap()
        }
        fn verify(&self, object: &Path, value: i32) {
            let main = self.0.join("main.c");
            let exe = self.0.join("program");
            fs::write(
                &main,
                format!("int answer(void); int main(void) {{ return answer() != {value}; }}"),
            )
            .unwrap();
            assert!(Command::new("cc")
                .arg(&main)
                .arg(object)
                .arg("-o")
                .arg(&exe)
                .status()
                .unwrap()
                .success());
            assert!(Command::new(exe).status().unwrap().success());
        }
        #[cfg(unix)]
        fn script(&self, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join("compiler");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reuse_header_and_flag_invalidation_and_corruption_repair() {
        let f = Fixture::new();
        let first = f.object(&["-O3"]);
        f.verify(&first, 42);
        assert_eq!(first, f.object(&["-O3"]));
        fs::write(f.0.join("value.h"), "#define VALUE 17\n").unwrap();
        let second = f.object(&["-O3"]);
        assert_ne!(first, second);
        f.verify(&second, 17);
        for flags in [
            vec!["-O0"],
            vec!["-O3", "-DTINOX_CHECKED"],
            vec!["-O3", "-DTINOX_NO_GC", "-fsanitize=address"],
        ] {
            assert_ne!(f.config(&flags).key, f.config(&["-O3"]).key);
        }
        fs::write(&second, b"corrupt").unwrap();
        let repaired = f.object(&["-O3"]);
        f.verify(&repaired, 17);
        assert!(valid_entry(repaired.parent().unwrap()));
    }

    #[test]
    fn compiler_extensions_bypass_instead_of_using_incomplete_keys() {
        for flag in [
            "@custom.rsp",
            "-fplugin=custom.so",
            "-march=native",
            "-specs=custom.spec",
            "-fprofile-use=profile",
            "-Bcustom",
        ] {
            assert!(!supported_flags(&["-O3", flag]), "{flag}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_builds_compile_once_and_publish_complete_objects() {
        let f = Fixture::new();
        let mut config = f.config(&["-O3"]);
        // Test instrumentation only: capture the real compiler first, then log
        // executions of the C compilation step without altering its behavior.
        let log = f.0.join("compilations");
        config.compiler = f.script(&format!(
            "echo compile >> '{}'\nexec '{}' \"$@\"",
            log.display(),
            config.compiler.display()
        ));
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let config = config.clone();
                    let root = f.0.join("cache");
                    scope.spawn(move || cached_object(&root, config, &["-O3"]).unwrap())
                })
                .collect();
            for handle in handles {
                f.verify(&handle.join().unwrap(), 42);
            }
        });
        assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn failed_compilation_never_publishes_partial_object() {
        let f = Fixture::new();
        let mut config = f.config(&["-O3"]);
        let key = config.key.clone();
        config.compiler = f.script("for last; do :; done\necho partial > \"$last\"\nexit 1");
        let root = f.0.join("cache");
        assert!(cached_object(&root, config, &["-O3"]).is_err());
        assert!(!root.join(key).exists());
        f.verify(&f.object(&["-O3"]), 42);
    }
}

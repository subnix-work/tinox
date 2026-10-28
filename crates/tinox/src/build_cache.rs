//! Application backend reuse. The frontend remains authoritative on every run;
//! cache keys include the bytes actually read, generated IR and native tools.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: u32 = 1;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static INPUTS: RefCell<Option<Inputs>> = const { RefCell::new(None) };
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}
fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}
pub fn trace(message: &str) {
    if env::var_os("TINOX_CACHE_TRACE").is_some_and(|v| v == "1") {
        eprintln!("[build-cache] {message}");
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FileInput {
    path: PathBuf,
    resolved: PathBuf,
    sha256: String,
}
impl FileInput {
    fn from_bytes(path: &Path, bytes: &[u8]) -> io::Result<Self> {
        Ok(Self {
            path: absolute(path)?,
            resolved: path.canonicalize()?,
            sha256: digest(bytes),
        })
    }
    fn capture(path: &Path) -> io::Result<Self> {
        Self::from_bytes(path, &fs::read(path)?)
    }
    fn unchanged(&self) -> bool {
        Self::capture(&self.path).is_ok_and(|now| now == *self)
    }
}
#[derive(Clone, Default)]
pub struct Inputs {
    files: BTreeMap<PathBuf, FileInput>,
    unstable: bool,
}
pub struct InputGuard(Option<Inputs>);
impl InputGuard {
    pub fn begin() -> Self {
        Self(INPUTS.with(|state| state.replace(Some(Inputs::default()))))
    }
    pub fn snapshot(&self) -> Inputs {
        INPUTS.with(|state| state.borrow().as_ref().unwrap().clone())
    }
}
impl Drop for InputGuard {
    fn drop(&mut self) {
        INPUTS.with(|state| {
            state.replace(self.0.take());
        });
    }
}
/// Record source and manifest bytes at the moment the compiler reads them.
/// Outside application compilation this is exactly fs::read_to_string.
pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    INPUTS.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(inputs) = state.as_mut() {
            match FileInput::from_bytes(path, content.as_bytes()) {
                Ok(input) => {
                    if inputs
                        .files
                        .get(&input.path)
                        .is_some_and(|old| old != &input)
                    {
                        inputs.unstable = true;
                    }
                    inputs.files.insert(input.path.clone(), input);
                }
                Err(_) => inputs.unstable = true,
            }
        }
    });
    Ok(content)
}

/// Memoize native-tool digests only after the file's change time is safely
/// older than the hash operation. A new inode or any status/content change
/// invalidates the memo even if a caller restores mtime. Recent/coarse-time
/// files are hashed directly, avoiding the classic racy-clean timestamp case.
#[cfg(unix)]
fn native_digest(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let identity = |meta: &fs::Metadata| {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mode(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
            path.display()
        )
    };
    let before = fs::metadata(path).ok()?;
    let token = identity(&before);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let stamp = u64::try_from(before.ctime()).ok().and_then(|seconds| {
        u32::try_from(before.ctime_nsec())
            .ok()
            .map(|nanos| std::time::Duration::new(seconds, nanos))
    });
    let eligible = before.ctime_nsec() > 0
        && stamp.is_some_and(|stamp| {
            now.checked_sub(stamp)
                .is_some_and(|age| age >= std::time::Duration::from_secs(2))
        });
    let memo = eligible
        .then(|| {
            crate::runtime_cache::cache_dir().map(|root| {
                root.join("native-digests-v1")
                    .join(digest(token.as_bytes()))
            })
        })
        .flatten();
    if let Some(memo) = &memo {
        if let Ok(value) = fs::read_to_string(memo) {
            if let Some((sha, checksum)) = value.split_once('\n') {
                if sha.len() == 64
                    && sha.bytes().all(|c| c.is_ascii_hexdigit())
                    && checksum == digest(format!("{token}\n{sha}").as_bytes())
                    && identity(&fs::metadata(path).ok()?) == token
                {
                    return Some(sha.into());
                }
            }
        }
    }
    let sha = digest(&fs::read(path).ok()?);
    if identity(&fs::metadata(path).ok()?) != token {
        return None;
    }
    if let Some(memo) = memo {
        // This memo is an optional acceleration. Corrupt or interrupted writes
        // fail checksum validation and fall back to a complete content hash.
        if fs::create_dir_all(memo.parent()?).is_ok() {
            let checksum = digest(format!("{token}\n{sha}").as_bytes());
            let _ = fs::write(memo, format!("{sha}\n{checksum}"));
        }
    }
    Some(sha)
}
#[cfg(not(unix))]
fn native_digest(path: &Path) -> Option<String> {
    Some(digest(&fs::read(path).ok()?))
}

fn tool_output(tool: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(tool).args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}
/// Hash compiler/linker selection as well as file contents; timestamps alone
/// cannot prove that a replaced compiler or dependency is unchanged.
pub fn fingerprint(
    ir: &[u8],
    mode: &str,
    runtime: &str,
    cflags: &[&str],
    link_flags: &[&str],
    inputs: &Inputs,
) -> Option<String> {
    if inputs.unstable {
        trace("bypass: an input changed while the frontend read it");
        return None;
    }
    let runtime_key = crate::runtime_cache::fingerprint(runtime, cflags).ok()??;
    let mut hash = Sha256::new();
    field(&mut hash, b"tinox-application-cache-v1");
    field(&mut hash, env!("CARGO_PKG_VERSION").as_bytes());
    field(&mut hash, mode.as_bytes());
    field(&mut hash, ir);
    field(&mut hash, runtime_key.as_bytes());
    for input in inputs.files.values() {
        field(&mut hash, input.path.as_os_str().as_encoded_bytes());
        field(&mut hash, input.resolved.as_os_str().as_encoded_bytes());
        field(&mut hash, input.sha256.as_bytes());
    }
    let mut tools = BTreeSet::new();
    tools.insert(env::current_exe().ok()?.canonicalize().ok()?);
    for name in ["cc", "llc", "opt", "ld", "ldd"] {
        // Missing optional opt is part of the configuration too.
        field(&mut hash, name.as_bytes());
        if let Some(path) = crate::runtime_cache::executable(name) {
            tools.insert(path);
        } else if name != "opt" {
            return None;
        }
    }
    let cc = crate::runtime_cache::executable("cc")?;
    for helper in ["collect2", "ld", "cc1", "as"] {
        if let Some(name) = tool_output(&cc, &[&format!("-print-prog-name={helper}")]) {
            if let Some(path) =
                crate::runtime_cache::executable(String::from_utf8(name).ok()?.trim())
            {
                tools.insert(path);
            }
        }
    }
    // LLVM's command-line drivers may be tiny ELF launchers backed by a
    // replaceable libLLVM.so. Hash their loaded libraries, not just the driver.
    let mut libraries = BTreeSet::new();
    for tool in &tools {
        use std::io::Read;
        let mut magic = [0u8; 4];
        fs::File::open(tool).ok()?.read_exact(&mut magic).ok()?;
        if &magic == b"\x7fELF" {
            for library in loaded_library_paths(tool)? {
                libraries.insert(library.canonicalize().ok()?);
            }
        }
    }
    tools.extend(libraries);
    // GCC emits a fresh LTO resolution filename even in a linker-only dry
    // run. Normalize only that nonexistent, driver-generated scratch file.
    let plan = Command::new(&cc)
        .args(["-###", "/dev/null", "-o", "/dev/null"])
        .args(link_flags)
        .output()
        .ok()?;
    if !plan.status.success() {
        return None;
    }
    for bytes in [&plan.stdout, &plan.stderr] {
        let text = String::from_utf8(bytes.clone()).ok()?;
        for word in text.split_whitespace() {
            let word = word.trim_matches('"');
            let path = word.strip_prefix("-plugin-opt=").unwrap_or(word);
            let path = Path::new(path);
            if path.is_absolute() && path.is_file() {
                tools.insert(path.canonicalize().ok()?);
            }
        }
        field(&mut hash, normalize_link_plan(text)?.as_bytes());
    }
    for path in tools {
        field(&mut hash, path.as_os_str().as_encoded_bytes());
        field(&mut hash, native_digest(&path)?.as_bytes());
    }
    for flag in link_flags {
        field(&mut hash, flag.as_bytes());
    }
    // Content of user-supplied native objects/libraries ([native] in
    // tinox.toml): the flag text alone would not notice a rebuilt .so.
    for path in crate::native::fingerprint_inputs(link_flags) {
        field(&mut hash, path.as_os_str().as_encoded_bytes());
        field(&mut hash, native_digest(&path)?.as_bytes());
    }
    let mut environment: Vec<_> = env::vars_os().collect();
    environment.sort();
    for (name, value) in environment {
        // Observability and explicit rebuild controls do not change code.
        if name == "TINOX_CACHE_TRACE" || name == "TINOX_BUILD_CACHE" {
            continue;
        }
        field(&mut hash, name.as_encoded_bytes());
        field(&mut hash, value.as_encoded_bytes());
    }
    Some(format!("{:x}", hash.finalize()))
}

fn normalize_link_plan(mut text: String) -> Option<String> {
    let prefix = "-plugin-opt=-fresolution=";
    let mut offset = 0;
    while let Some(start) = text[offset..]
        .find(prefix)
        .map(|i| i + offset + prefix.len())
    {
        let end = text[start..]
            .find(['"', ' ', '\n'])
            .map_or(text.len(), |i| start + i);
        let path = Path::new(&text[start..end]);
        if !path.is_absolute()
            || path.exists()
            || path.extension()? != "res"
            || !path.file_name()?.to_str()?.starts_with("cc")
        {
            return None;
        }
        let replacement = format!("{}/<gcc-resolution>.res", path.parent()?.display());
        text.replace_range(start..end, &replacement);
        offset = start + replacement.len();
    }
    Some(text)
}

#[derive(Serialize, Deserialize)]
struct Marker {
    schema: u32,
    key: String,
    output_sha256: String,
    inputs: Vec<FileInput>,
    linked: Vec<FileInput>,
    loaded: Vec<FileInput>,
}
fn loaded_library_paths(exe: &Path) -> Option<BTreeSet<PathBuf>> {
    let out = Command::new("ldd").arg(exe).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut paths = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("linux-vdso.") || line.starts_with("linux-gate.") {
            continue;
        }
        let path = line
            .split_once("=>")
            .map_or(line, |(_, right)| right.trim());
        let (path, _) = path.rsplit_once(" (")?;
        let path = PathBuf::from(path.trim());
        if !path.is_absolute() {
            return None;
        }
        paths.insert(path);
    }
    Some(paths)
}
fn loaded_libraries(exe: &Path) -> Option<Vec<FileInput>> {
    loaded_library_paths(exe)?
        .into_iter()
        .map(|p| FileInput::capture(&p).ok())
        .collect()
}
fn link_selection_unchanged(linked: &[FileInput]) -> bool {
    let Some(cc) = crate::runtime_cache::executable("cc") else {
        return false;
    };
    linked.iter().all(|input| {
        let Some(name) = input.path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let Some(path) = tool_output(&cc, &[&format!("-print-file-name={name}")]) else {
            return false;
        };
        let Ok(path) = String::from_utf8(path) else {
            return false;
        };
        Path::new(path.trim())
            .canonicalize()
            .is_ok_and(|resolved| resolved == input.resolved)
            && input.unchanged()
    })
}
fn executable_file(path: &Path) -> bool {
    crate::runtime_cache::can_execute(path)
}

/// A lock spans the frontend, validation and publication for one output. Staging
/// paths are private; a failed or killed build cannot publish a success marker.
pub struct Build {
    _lock: fs::File,
    output: PathBuf,
    root: PathBuf,
    staging: PathBuf,
    completed: bool,
}
impl Build {
    pub fn begin(output: &str) -> Result<Self, String> {
        let output = absolute(Path::new(output)).map_err(|e| e.to_string())?;
        let parent = output
            .parent()
            .unwrap()
            .canonicalize()
            .map_err(|e| format!("Cannot resolve output directory: {e}"))?;
        let output = parent.join(output.file_name().ok_or("Missing output filename")?);
        // Replacing a symlink would change the old compiler's output contract.
        // Resolve existing symlinks before selecting the per-output lock.
        let output = if output.is_symlink() {
            output.canonicalize().map_err(|e| e.to_string())?
        } else {
            output
        };
        let root = output.parent().unwrap().join(format!(
            ".{}.tinox-build",
            output.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir_all(&root)
            .map_err(|e| format!("Cannot create application build directory: {e}"))?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("lock"))
            .map_err(|e| e.to_string())?;
        lock.lock()
            .map_err(|e| format!("Cannot lock application output: {e}"))?;
        // Clear the old success marker before any fallible work, even parsing.
        // Keep its bytes privately so a fully validated hit can restore it.
        let staging = loop {
            let path = root.join(format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        };
        if root.join("manifest.yaml").exists() {
            fs::rename(root.join("manifest.yaml"), staging.join("previous.yaml"))
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            _lock: lock,
            output,
            root,
            staging,
            completed: false,
        })
    }
    pub fn staged_output(&self) -> PathBuf {
        self.staging.join("app")
    }
    pub fn ir_path(&self) -> PathBuf {
        self.staging.join("app.ll")
    }
    fn artifact(&self, sha256: &str) -> PathBuf {
        self.root.join("artifacts").join(sha256)
    }
    pub fn reuse(&mut self, key: Option<&str>, force: bool) -> Option<PathBuf> {
        if force
            || env::var_os("TINOX_BUILD_CACHE").is_some_and(|v| v == "0")
            || env::var_os("TINOX_RUNTIME_CACHE").is_some_and(|v| v == "0")
        {
            trace("bypass: forced or cache disabled");
            return None;
        }
        let Some(key) = key else {
            trace("bypass: native inputs cannot be proven complete");
            return None;
        };
        let marker: Marker = fs::read(self.staging.join("previous.yaml"))
            .ok()
            .and_then(|bytes| serde_yaml::from_slice(&bytes).ok())?;
        if marker.schema != SCHEMA || marker.key != key {
            trace("miss: build inputs or configuration changed");
            return None;
        }
        let artifact = self.artifact(&marker.output_sha256);
        if !executable_file(&self.output)
            || !executable_file(&artifact)
            || fs::read(&self.output)
                .ok()
                .is_none_or(|b| digest(&b) != marker.output_sha256)
            || fs::read(&artifact)
                .ok()
                .is_none_or(|b| digest(&b) != marker.output_sha256)
        {
            trace("miss: executable missing, modified or not executable");
            return None;
        }
        if !marker.inputs.iter().all(FileInput::unchanged)
            || !link_selection_unchanged(&marker.linked)
            || loaded_libraries(&self.output).is_none_or(|now| now != marker.loaded)
        {
            trace("miss: source, linker input or loaded library changed");
            return None;
        }
        fs::rename(
            self.staging.join("previous.yaml"),
            self.root.join("manifest.yaml"),
        )
        .ok()?;
        self.completed = true;
        trace(&format!("hit {key}"));
        Some(artifact)
    }
    pub fn publish(
        mut self,
        key: Option<&str>,
        inputs: &Inputs,
        linked: Option<Vec<PathBuf>>,
    ) -> Result<PathBuf, String> {
        let staged = self.staged_output();
        let bytes = fs::read(&staged).map_err(|e| e.to_string())?;
        let sha256 = digest(&bytes);
        let artifact = self.artifact(&sha256);
        fs::create_dir_all(artifact.parent().unwrap()).map_err(|e| e.to_string())?;
        if !executable_file(&artifact)
            || fs::read(&artifact)
                .ok()
                .is_none_or(|b| digest(&b) != sha256)
        {
            fs::rename(&staged, &artifact).map_err(|e| e.to_string())?;
        }
        let publishing = self.staging.join("publish");
        fs::copy(&artifact, &publishing).map_err(|e| e.to_string())?;
        fs::rename(&publishing, &self.output)
            .map_err(|e| format!("Cannot publish executable: {e}"))?;
        // Preserve the existing build command's generated-IR artifact.
        fs::copy(
            self.ir_path(),
            self.output.with_file_name(format!(
                "{}.ll",
                self.output.file_name().unwrap().to_string_lossy()
            )),
        )
        .map_err(|e| e.to_string())?;
        if let (Some(key), Some(paths), Some(loaded)) =
            (key, linked, loaded_libraries(&self.output))
        {
            let linked: Option<Vec<_>> = paths.iter().map(|p| FileInput::capture(p).ok()).collect();
            if let Some(linked) = linked.filter(|files| link_selection_unchanged(files)) {
                if !inputs.unstable && inputs.files.values().all(FileInput::unchanged) {
                    let marker = Marker {
                        schema: SCHEMA,
                        key: key.into(),
                        output_sha256: sha256,
                        inputs: inputs.files.values().cloned().collect(),
                        linked,
                        loaded,
                    };
                    let bytes = serde_yaml::to_string(&marker).map_err(|e| e.to_string())?;
                    fs::write(self.staging.join("manifest.yaml"), bytes)
                        .map_err(|e| e.to_string())?;
                    fs::rename(
                        self.staging.join("manifest.yaml"),
                        self.root.join("manifest.yaml"),
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    trace("bypass: source changed during compilation");
                }
            } else {
                trace("bypass: linker selection could not be verified");
            }
        } else {
            trace("bypass: native dependency inventory unavailable");
        }
        self.completed = true;
        Ok(artifact)
    }
}
impl Drop for Build {
    fn drop(&mut self) {
        if !self.completed {
            let _ = fs::remove_file(self.root.join("manifest.yaml"));
        }
        let _ = fs::remove_dir_all(&self.staging);
    }
}

pub fn run_output(input: &str, mode: &str) -> Result<PathBuf, String> {
    let input = Path::new(input).canonicalize().map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    field(&mut hash, input.as_os_str().as_encoded_bytes());
    field(&mut hash, mode.as_bytes());
    let root = crate::pm::find_project_root_from(input.parent().unwrap())
        .map(|root| root.join(".tinox/run"))
        .or_else(|| {
            crate::runtime_cache::cache_dir().and_then(|p| p.parent().map(|p| p.join("run-v1")))
        })
        .ok_or("Cannot determine application run cache directory")?;
    let dir = root.join(format!("{:x}", hash.finalize()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("app"))
}

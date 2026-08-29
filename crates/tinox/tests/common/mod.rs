//! Shared helpers for the golden-test harnesses (e2e.rs, matrix.rs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const COMPILE_TIMEOUT: Duration = Duration::from_secs(60);
const RUN_TIMEOUT: Duration = Duration::from_secs(15);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Duplicated from `CORE_MODULES` in `crates/tinox/src/main.rs` — kept in
/// sync by hand (tinox is a bin-only crate with no lib target to share a
/// single source of truth from). If a module moves between the core and
/// extended tiers, update both. Anything `tinox.core.<mod>` NOT in this
/// list needs a synthesized `[[dependencies]]` entry (see
/// `extended_deps_used`/`run_case` below) for the case to resolve at all,
/// mirroring what a real consuming project has to do since the core/
/// extended stdlib split.
const CORE_MODULES: &[&str] = &[
    "array", "collections", "set", "queue", "heap", "trie", "graph", "iter",
    "sort", "option", "result", "cache", "pool",
    "math", "mathf", "mathx", "complex", "decimal",
    "string", "fmt", "format", "encoding",
    "io", "fs", "env", "process", "debug", "socket",
    "semaphore", "time", "cron", "events",
    "logger", "validation", "regex", "random", "hash", "uuid",
    "base64", "hex", "uri",
];

pub struct Case {
    pub path: PathBuf,
    pub name: String,
    pub expect_lines: Vec<String>,
    pub expect_contains: Vec<String>,
    pub expect_exit: i32,
    pub args: Vec<String>,
    pub db_sql: Vec<String>,
    pub test_mode: bool,
    /// `// tls-fixture` — copies tests/fixtures/tls/selfsigned_{cert,key}.pem
    /// into the isolated workdir as tls_cert.pem/tls_key.pem before running,
    /// so TLS e2e tests (HTTPS/WSS/AMQPS) can reference them by a fixed
    /// relative path instead of a workdir-relative-unresolvable repo path.
    pub tls_fixture: bool,
}

#[allow(dead_code)] // nur von e2e.rs genutzt — andere Test-Binaries teilen sich dieses Modul
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

pub fn parse_case(path: &Path) -> Case {
    let src = fs::read_to_string(path).expect("read test file");
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let mut expect_lines = Vec::new();
    let mut expect_contains = Vec::new();
    let mut expect_exit = 0;
    let mut args = Vec::new();
    let mut db_sql = Vec::new();
    let mut test_mode = false;
    let mut tls_fixture = false;
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("// expect-exit:") {
            expect_exit = rest.trim().parse().expect("expect-exit code");
        } else if let Some(rest) = t.strip_prefix("// expect-contains:") {
            expect_contains.push(rest.trim().to_string());
        } else if let Some(rest) = t.strip_prefix("// expect:") {
            expect_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        } else if let Some(rest) = t.strip_prefix("// args:") {
            args = rest.split_whitespace().map(String::from).collect();
        } else if let Some(rest) = t.strip_prefix("// db:") {
            db_sql.push(rest.trim().to_string());
        } else if t == "// mode: test" {
            test_mode = true;
        } else if t == "// tls-fixture" {
            tls_fixture = true;
        }
    }
    Case { path: path.to_path_buf(), name, expect_lines, expect_contains, expect_exit, args, db_sql, test_mode, tls_fixture }
}

/// Scans `import tinox.core.<mod>...;` lines in `src` and returns every
/// distinct `<mod>` that ISN'T in `CORE_MODULES` — the set of extended-tier
/// dependencies this source file needs declared in a synthesized
/// tinox.toml before it can build post-split. A plain substring/line scan
/// (not a real parser) is enough here: false positives inside a string
/// literal or comment are vanishingly unlikely in this codebase's test
/// fixtures and would only cause an unnecessary (harmless) extra
/// dependency entry, never a missed one.
/// The published version to pin an extended-tier dependency to in a
/// synthesized tinox.toml — read from that module's own source-of-truth
/// manifest (`crates/tinox-core-ext/<module>/tinox.toml`) rather than a
/// hardcoded "1.0.0". A stale hardcoded version silently keeps testing
/// whatever was published under that old version forever, even after the
/// module's real content moves on — exactly what happened on 2026-08-10,
/// when 13 extended modules got republished as 1.0.1/1.0.2 to fix content
/// drift (translation, a real SASL handshake bug in amqp10, ...) but e2e
/// kept pinning "1.0.0", so cases importing them kept exercising the OLD,
/// pre-fix package instead of the one that's actually current. Falls back
/// to "1.0.0" if the module's manifest can't be read/parsed, matching the
/// old hardcoded behavior for anything not (yet) bumped past its first
/// release.
fn extended_module_version(module: &str) -> String {
    let manifest = repo_root()
        .join("crates/tinox-core-ext")
        .join(module)
        .join("tinox.toml");
    fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("version"))
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|rest| rest.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

/// The `artifactId`s an extended-tier module's OWN published manifest
/// declares as ITS dependencies (`[[dependencies]] artifactId = "..."`
/// entries, `group = "tinox.core"` — the only group this repo's extended
/// tier uses, so not filtered on separately). Same "plain line scan is
/// enough" reasoning as `extended_module_version` above, and same
/// source-of-truth file. Used to avoid synthesizing a REDUNDANT, and
/// potentially conflicting, direct `[[dependencies]]` entry in a case's
/// tinox.toml for a module some OTHER required module already pulls in
/// transitively (see `run_case`'s dedup below) — issue found while
/// investigating `oidc_roles_allowed_guard`'s "Ambiguous import...
/// resolves in more than one installed dependency" failure: this harness
/// independently pinned BOTH `http_server` (via `extended_module_version`,
/// which tracks that package's own CURRENT version, 1.0.2 at the time)
/// AND `rest` (whose own manifest, still at 1.0.1, transitively pins
/// `http_server` to the OLDER 1.0.1) for the same case, so the resolver
/// correctly refused to silently pick one -- a synthesis bug in THIS
/// harness (declaring the same transitive package twice, independently
/// versioned), not a real resolver defect, and not stale/corrupt shared
/// cache state either (confirmed live: `rest@1.0.1`'s cached manifest is
/// exactly what's published, immutable by design -- editing this repo's
/// own `crates/tinox-core-ext/rest/tinox.toml` locally does nothing for
/// an already-published version). The actual, permanent fix (bumping
/// http2_server/oidc/rest/http3_server to a new version with a corrected
/// http_server pin) needs a real `tinox publish`, out of scope for a test
/// harness change -- see the follow-up issue this investigation filed.
fn extended_module_manifest_deps(module: &str) -> Vec<String> {
    let manifest = repo_root()
        .join("crates/tinox-core-ext")
        .join(module)
        .join("tinox.toml");
    let Ok(text) = fs::read_to_string(&manifest) else { return Vec::new() };
    let mut deps = Vec::new();
    let mut in_deps_block = false;
    for line in text.lines() {
        let l = line.trim();
        if l == "[[dependencies]]" {
            in_deps_block = true;
            continue;
        }
        if l.starts_with('[') {
            in_deps_block = false;
            continue;
        }
        if in_deps_block {
            if let Some(rest) = l.strip_prefix("artifactId") {
                if let Some(v) = rest.trim_start().strip_prefix('=') {
                    deps.push(v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    deps
}

fn extended_deps_in_source(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("import tinox.core.") else { continue };
        let module = rest.split(['.', ';']).next().unwrap_or("").trim();
        if module.is_empty() || CORE_MODULES.contains(&module) {
            continue;
        }
        if !found.iter().any(|m: &String| m == module) {
            found.push(module.to_string());
        }
    }
    found
}

/// Recursively copies every `.tnx` file (and the directory structure that
/// contains them) from `src` into `dest`, collecting the union of
/// extended-tier module names any of them import (see
/// `extended_deps_in_source`) into `extended_deps`. Used for directory-
/// based e2e cases, which can nest a whole second module in an
/// underscore-prefixed subdirectory.
fn copy_tnx_tree(src: &Path, dest: &Path, extended_deps: &mut Vec<String>) -> Result<(), String> {
    for entry in fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let sub_dest = dest.join(p.file_name().unwrap());
            fs::create_dir_all(&sub_dest).map_err(|e| format!("mkdir {}: {e}", sub_dest.display()))?;
            copy_tnx_tree(&p, &sub_dest, extended_deps)?;
        } else if p.extension().map(|x| x == "tnx").unwrap_or(false) {
            let src_content = fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
            for m in extended_deps_in_source(&src_content) {
                if !extended_deps.contains(&m) {
                    extended_deps.push(m);
                }
            }
            let dest_file = dest.join(p.file_name().unwrap());
            fs::copy(&p, &dest_file).map_err(|e| format!("copy {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

/// Wait with timeout; kill on expiry. Returns None on timeout.
pub fn wait_timeout(child: &mut std::process::Child, dur: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            return Some(st);
        }
        if start.elapsed() > dur {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn run_captured(
    mut cmd: Command,
    dur: Duration,
) -> Result<(Option<std::process::ExitStatus>, String), String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {:?} failed: {e}", cmd.get_program()))?;
    // Read pipes on threads so a chatty child can't fill the pipe and stall.
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out_h = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err_h = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });
    let status = wait_timeout(&mut child, dur);
    let mut output = out_h.join().unwrap_or_default();
    output.push_str(&err_h.join().unwrap_or_default());
    Ok((status, output))
}

pub fn run_case(case: &Case) -> Result<(), String> {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-e2e-{}-{}",
        case.name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&workdir);
    fs::create_dir_all(&workdir).map_err(|e| format!("mkdir workdir: {e}"))?;

    // Copy the case's own source into the isolated workdir rather than
    // building it in place from tests/e2e/ -- the core/extended stdlib
    // split (see CLAUDE.md) means a case importing an extended-tier module
    // needs a synthesized tinox.toml (below) sitting next to its own
    // entry file, since `find_project_root_from` walks up from the FILE
    // BEING BUILT's own directory, not the process's cwd. Writing that
    // transiently into the real tests/e2e/<case>/ tree would risk leftover
    // files in the repo on a panicked/killed test run; copying into the
    // already-isolated, always-cleaned-up workdir avoids that entirely.
    // Directory-based cases (one-type-per-file convention) get their whole
    // subtree copied RECURSIVELY, not just top-level siblings -- a
    // cross-module test case can nest a second module in its own
    // underscore-prefixed subdirectory (e.g. bug06_cross_module_struct/
    // _bug06_mod/*.tnx, imported via `import _bug06_mod;`), matching
    // exactly how it already sits on disk; single-file cases just get the
    // one file.
    let src_dir = case.path.parent().unwrap_or(Path::new("."));
    let is_dir_case = src_dir.file_name().map(|n| n != "e2e").unwrap_or(false);
    let mut extended_deps: Vec<String> = Vec::new();
    if is_dir_case {
        copy_tnx_tree(src_dir, &workdir, &mut extended_deps)?;
    } else {
        let src = fs::read_to_string(&case.path).map_err(|e| format!("read {}: {e}", case.path.display()))?;
        extended_deps = extended_deps_in_source(&src);
        let dest = workdir.join(case.path.file_name().unwrap());
        fs::copy(&case.path, &dest).map_err(|e| format!("copy {}: {e}", case.path.display()))?;
    }
    let entry = workdir.join(case.path.file_name().unwrap());

    // Optional TLS fixture: a fixed self-signed cert/key so HTTPS/WSS/AMQPS
    // e2e tests don't need to generate one at test time.
    if case.tls_fixture {
        let fixtures = repo_root().join("tests/fixtures/tls");
        fs::copy(fixtures.join("selfsigned_cert.pem"), workdir.join("tls_cert.pem"))
            .map_err(|e| format!("copy tls cert fixture: {e}"))?;
        fs::copy(fixtures.join("selfsigned_key.pem"), workdir.join("tls_key.pem"))
            .map_err(|e| format!("copy tls key fixture: {e}"))?;
    }

    // Optional sqlite fixture
    if !case.db_sql.is_empty() {
        let sql = case.db_sql.join("\n");
        let st = Command::new("sqlite3")
            .arg(workdir.join("test.db"))
            .arg(&sql)
            .status()
            .map_err(|e| format!("sqlite3 fixture: {e} (sqlite3 installed?)"))?;
        if !st.success() {
            return Err("sqlite3 fixture SQL failed".to_string());
        }
    }

    // A tinox.toml is needed if either the sqlite fixture above needs its
    // [database] section, or the case imports an extended-tier stdlib
    // module needing a [[dependencies]] declaration (or both at once --
    // e.g. the orm_sqlite_* cases, which do both) -- write ONE merged file
    // covering whichever apply, then `tinox install` before build/test.
    if !case.db_sql.is_empty() || !extended_deps.is_empty() {
        let mut toml = String::new();
        if !case.db_sql.is_empty() {
            toml.push_str("[database]\ndriver = \"sqlite\"\nurl = \"test.db\"\n");
        }
        if !extended_deps.is_empty() {
            toml.push_str("[package]\nname = \"");
            toml.push_str(&case.name);
            toml.push_str("\"\nversion = \"0.0.0\"\ndescription = \"\"\n");
            // Skip a module that's already a transitive dependency of
            // some OTHER module this case also needs (per that other
            // module's own manifest) -- emitting an independent direct
            // pin for it too risks requesting a DIFFERENT version than
            // the one its dependent already pulls in, which the resolver
            // then (correctly) refuses to silently pick between. See
            // `extended_module_manifest_deps`'s own doc comment for the
            // real case this fixes (`oidc_roles_allowed_guard`: `rest`
            // transitively needs `http_server` 1.0.1, this file's own
            // direct `import tinox.core.http_server;` would otherwise
            // pin the independently-tracked CURRENT version, 1.0.2).
            // Letting the dependent module's own transitive install
            // supply it keeps exactly one version in play, matching what
            // a real consuming project gets from `tinox install` alone
            // (no redundant top-level entry needed for a package nothing
            // here imports standalone-without-also-importing its parent).
            let transitively_covered: std::collections::HashSet<String> = extended_deps
                .iter()
                .flat_map(|m| extended_module_manifest_deps(m))
                .collect();
            for m in &extended_deps {
                if transitively_covered.contains(m) {
                    continue;
                }
                let v = extended_module_version(m);
                toml.push_str(&format!(
                    "\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{v}\"\n"
                ));
            }
        }
        fs::write(workdir.join("tinox.toml"), toml).map_err(|e| format!("write tinox.toml: {e}"))?;
    }
    if !extended_deps.is_empty() {
        let mut install = Command::new(tinox);
        install.arg("install").current_dir(&workdir);
        let (status, out) = run_captured(install, INSTALL_TIMEOUT)?;
        match status {
            None => return Err("tinox install TIMEOUT".to_string()),
            Some(st) if !st.success() => return Err(format!("tinox install failed:\n{}", out.trim_end())),
            _ => {}
        }
    }

    let (status, output) = if case.test_mode {
        // `tinox test` compiles and runs @Test methods itself.
        let mut cmd = Command::new(tinox);
        cmd.arg("test").arg(&entry).current_dir(&workdir);
        let (status, output) = run_captured(cmd, COMPILE_TIMEOUT)?;
        match status {
            None => return Err("tinox test TIMEOUT".to_string()),
            Some(st) => (st, output),
        }
    } else {
        // Compile — explicit -o: for directory-based cases (one-type-per-file
        // convention, entry point is always `<dir>/main.tnx`) the default
        // output name would be "main", not `case.name`, so the run step below
        // (which looks for `workdir/<case.name>`) would find nothing.
        let exe = workdir.join(&case.name);
        let mut build = Command::new(tinox);
        build.arg("build").arg(&entry).arg("-o").arg(&exe).current_dir(&workdir);
        let (status, out) = run_captured(build, COMPILE_TIMEOUT)?;
        match status {
            None => return Err("compile TIMEOUT".to_string()),
            Some(st) if !st.success() => {
                return Err(format!("compile failed:\n{}", out.trim_end()));
            }
            _ => {}
        }

        // Run
        let mut run = Command::new(&exe);
        run.args(&case.args).current_dir(&workdir);
        let (status, output) = run_captured(run, RUN_TIMEOUT)?;
        match status {
            None => return Err("run TIMEOUT".to_string()),
            Some(st) => (st, output),
        }
    };

    let _ = fs::remove_dir_all(&workdir);

    let exit = status.code().unwrap_or(-1);
    let actual = output.trim_end_matches('\n');
    let expected = case.expect_lines.join("\n");
    let mut errors = Vec::new();
    if exit != case.expect_exit {
        errors.push(format!("exit code: expected {}, got {}", case.expect_exit, exit));
    }
    if !case.expect_lines.is_empty() && actual != expected {
        errors.push(format!("output mismatch:\n--- expected ---\n{expected}\n--- actual ---\n{actual}\n---"));
    }
    for needle in &case.expect_contains {
        if !actual.contains(needle.as_str()) {
            errors.push(format!("output does not contain {needle:?}:\n{actual}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}


use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `[package]`'s `name`/`version`/`description`/`group` — parsed/written by
/// `parse_manifest`/`write_manifest` (hand-rolled, see there for why).
///
/// `group` is optional (most projects never publish, so most manifests
/// have no reason to declare one) — only `tinox publish` (issue #172)
/// requires it, since `name`/`version` alone aren't full registry
/// coordinates without it.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub group: Option<String>,
}

/// One `[[repositories]]` table — a named base URL a `Dependency` can
/// reference by `id`. Purely local configuration (tinox.toml only), never
/// itself fetched or cached.
#[derive(Debug, Clone)]
pub struct Repository {
    pub id: String,
    pub url: String,
}

/// The default repository base URL used when a dependency specifies
/// neither `url` nor `repository` — see `effective_download_url`.
pub const DEFAULT_REPOSITORY_URL: &str = "https://central.tinox-lang.de";

/// One `[[dependencies]]` table — parsed/written by
/// `parse_manifest`/`write_manifest` (hand-rolled, see there for why).
///
/// Exactly one of `url`/`repository` may be set (see
/// `effective_download_url`): `url` is the legacy, explicit-URL form
/// (unchanged since before #172); `repository` names a `[[repositories]]`
/// entry to resolve `{base}/api/v1/{group}/{artifactId}/{version}`
/// against. If neither is set, resolution falls back to
/// `DEFAULT_REPOSITORY_URL` — an unqualified dependency does NOT pick "the
/// first configured repository," it always uses the hardcoded default.
#[derive(Debug, Clone)]
pub struct Dependency {
    pub group: String,
    pub artifact_id: String,
    pub version: String,
    pub url: Option<String>,
    pub repository: Option<String>,
    /// Expected SHA-256 of the downloaded artifact, lowercase hex. Optional
    /// for backward compatibility with existing manifests, but strongly
    /// recommended: without it, `tinox install` only pins against whatever
    /// tinox.lock happens to have recorded (see verify_checksum).
    pub sha256: Option<String>,
}

/// Resolves the exact URL to download `dep` from. `owning_manifest` is
/// whichever tinox.toml actually declared `dep` — for a transitive
/// dependency read back from an installed package's own tinox.toml, this
/// is THAT package's manifest, not the top-level project's, so a
/// `repository` reference resolves against the repositories the package
/// that declared it configured, not whoever happens to be installing it.
pub fn effective_download_url(dep: &Dependency, owning_manifest: &TinoxManifest) -> Result<String, String> {
    if let Some(url) = &dep.url {
        if dep.repository.is_some() {
            return Err(format!(
                "dependency {}:{} {} specifies both `url` and `repository` — use only one",
                dep.group, dep.artifact_id, dep.version
            ));
        }
        return Ok(url.clone());
    }
    let base = match &dep.repository {
        Some(id) => owning_manifest
            .repositories
            .iter()
            .find(|r| &r.id == id)
            .map(|r| r.url.clone())
            .ok_or_else(|| format!(
                "dependency {}:{} {} references repository \"{}\", but no [[repositories]] entry has that id",
                dep.group, dep.artifact_id, dep.version, id
            ))?,
        None => DEFAULT_REPOSITORY_URL.to_string(),
    };
    Ok(format!(
        "{}/api/v1/{}/{}/{}",
        base.trim_end_matches('/'),
        dep.group,
        dep.artifact_id,
        dep.version
    ))
}

/// Resolves a registry base URL (no trailing slash, no `/api/v1/...` path
/// yet) for a command that isn't resolving one specific `Dependency` — the
/// `cmd_publish`/`cmd_search` (issue #172) equivalent of
/// `effective_download_url`'s `[[repositories]]` lookup. `explicit_repo`
/// is a `--repository <id>` CLI flag; `None` falls back to
/// `TINOX_CENTRAL_URL` (matching `scripts/publish-stdlib-ext.sh`'s own
/// override, handy for pointing at a local/staging instance without
/// editing tinox.toml) and finally `DEFAULT_REPOSITORY_URL`.
fn resolve_registry_base_url(explicit_repo: Option<&str>, manifest: &TinoxManifest) -> Result<String, String> {
    let base = match explicit_repo {
        Some(id) => manifest
            .repositories
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.url.clone())
            .ok_or_else(|| format!("no [[repositories]] entry with id \"{}\"", id))?,
        None => std::env::var("TINOX_CENTRAL_URL").unwrap_or_else(|_| DEFAULT_REPOSITORY_URL.to_string()),
    };
    Ok(base.trim_end_matches('/').to_string())
}

#[derive(Debug, Default)]
pub struct TinoxManifest {
    pub package: Option<Package>,
    pub dependencies: Vec<Dependency>,
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LockEntry {
    pub group: String,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct TinoxLock {
    #[serde(default)]
    pub dependencies: Vec<LockEntry>,
}

pub fn read_lock(root: &Path) -> Result<TinoxLock, String> {
    let lock_path = root.join("tinox.lock");
    if !lock_path.exists() {
        return Ok(TinoxLock::default());
    }
    let content = fs::read_to_string(&lock_path)
        .map_err(|e| format!("Cannot read tinox.lock: {}", e))?;
    serde_yaml::from_str(&content).map_err(|e| format!("Invalid tinox.lock: {}", e))
}

pub fn write_lock(root: &Path, lock: &TinoxLock) -> Result<(), String> {
    let lock_path = root.join("tinox.lock");
    let content =
        serde_yaml::to_string(lock).map_err(|e| format!("Cannot serialize tinox.lock: {}", e))?;
    fs::write(&lock_path, content).map_err(|e| format!("Cannot write tinox.lock: {}", e))
}

fn lock_entry_for<'a>(lock: &'a TinoxLock, dep: &Dependency) -> Option<&'a LockEntry> {
    lock.dependencies.iter().find(|e| {
        e.group == dep.group && e.artifact_id == dep.artifact_id && e.version == dep.version
    })
}

fn upsert_lock_entry(lock: &mut TinoxLock, entry: LockEntry) {
    lock.dependencies.retain(|e| {
        !(e.group == entry.group && e.artifact_id == entry.artifact_id && e.version == entry.version)
    });
    lock.dependencies.push(entry);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Walks upward from `start` (not necessarily cwd) looking for a
/// `tinox.toml`. `start` need not itself be a directory — pass a source
/// file's parent, not the file itself.
pub fn find_project_root_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("tinox.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// cwd-based project root lookup — correct for `tinox install`/`add`/
/// `package`, which are always invoked by a human already `cd`'d into the
/// project. Compiling a specific source file (`tinox build <path>`) must
/// NOT use this: cwd is unrelated to the file being built whenever the
/// caller's cwd differs from the file's own directory (e.g. `dogfood.sh`
/// invokes `tinox build examples/foo/Main.tnx` from the repo root, and the
/// e2e/fixture test harness builds from an isolated temp workdir) — use
/// `find_project_root_from` with the file's directory instead.
pub fn find_project_root() -> Option<PathBuf> {
    std::env::current_dir().ok().and_then(|d| find_project_root_from(&d))
}

#[derive(PartialEq, Clone, Copy)]
enum ManifestSection {
    None,
    Package,
    Dependency,
    Repository,
    Other,
}

fn manifest_section_for(header_line: &str) -> ManifestSection {
    if header_line == "[[dependencies]]" {
        ManifestSection::Dependency
    } else if header_line == "[[repositories]]" {
        ManifestSection::Repository
    } else if header_line == "[package]" {
        ManifestSection::Package
    } else {
        ManifestSection::Other
    }
}

/// `key = "value"` (or bare `key = value`) → `(key, unquoted value)`, the
/// same convention every other tinox.toml reader in this codebase already
/// uses (see `read_project_entry`/`read_metrics_section` in `main.rs`).
fn parse_toml_kv(line: &str) -> Option<(&str, String)> {
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim().trim_matches('"').to_string()))
}

/// Parses `[package]` (name/version/description) and every `[[dependencies]]`
/// table from `tinox.toml`'s content — hand-rolled rather than a TOML crate
/// dependency, matching every other tinox.toml reader in this codebase.
/// Unknown keys (`[package] entry`/`output`, `[build]`, `[metrics]`, …) are
/// simply skipped here, not lost — `write_manifest` below only ever
/// rewrites the keys THIS function understands, leaving the rest of the
/// file untouched (see #154 — a prior version of this used a completely
/// separate `tinox.yaml` file/format that the rest of the CLI never read).
fn parse_manifest(content: &str) -> TinoxManifest {
    let mut section = ManifestSection::None;
    let mut pkg_name = String::new();
    let mut pkg_version = String::new();
    let mut pkg_description = String::new();
    let mut pkg_group: Option<String> = None;
    let mut have_package = false;

    fn empty_dep() -> Dependency {
        Dependency { group: String::new(), artifact_id: String::new(), version: String::new(), url: None, repository: None, sha256: None }
    }
    fn empty_repo() -> Repository {
        Repository { id: String::new(), url: String::new() }
    }

    let mut dependencies: Vec<Dependency> = Vec::new();
    let mut cur = empty_dep();
    let mut have_cur = false;

    let mut repositories: Vec<Repository> = Vec::new();
    let mut cur_repo = empty_repo();
    let mut have_cur_repo = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            if have_cur {
                dependencies.push(std::mem::replace(&mut cur, empty_dep()));
                have_cur = false;
            }
            if have_cur_repo {
                repositories.push(std::mem::replace(&mut cur_repo, empty_repo()));
                have_cur_repo = false;
            }
            section = manifest_section_for(line);
            if section == ManifestSection::Dependency {
                have_cur = true;
            } else if section == ManifestSection::Repository {
                have_cur_repo = true;
            } else if section == ManifestSection::Package {
                have_package = true;
            }
            continue;
        }
        let Some((key, value)) = parse_toml_kv(line) else { continue };
        match section {
            ManifestSection::Package => match key {
                "name" => pkg_name = value,
                "version" => pkg_version = value,
                "description" => pkg_description = value,
                "group" if !value.is_empty() => pkg_group = Some(value),
                _ => {}
            },
            ManifestSection::Dependency => match key {
                "group" => cur.group = value,
                "artifactId" => cur.artifact_id = value,
                "version" => cur.version = value,
                "url" if !value.is_empty() => cur.url = Some(value),
                "repository" if !value.is_empty() => cur.repository = Some(value),
                "sha256" if !value.is_empty() => cur.sha256 = Some(value),
                _ => {}
            },
            ManifestSection::Repository => match key {
                "id" => cur_repo.id = value,
                "url" => cur_repo.url = value,
                _ => {}
            },
            ManifestSection::None | ManifestSection::Other => {}
        }
    }
    if have_cur {
        dependencies.push(cur);
    }
    if have_cur_repo {
        repositories.push(cur_repo);
    }

    let package = have_package.then_some(Package { name: pkg_name, version: pkg_version, description: pkg_description, group: pkg_group });
    TinoxManifest { package, dependencies, repositories }
}

pub fn read_manifest(root: &Path) -> Result<TinoxManifest, String> {
    let toml_path = root.join("tinox.toml");
    if !toml_path.exists() {
        return Ok(TinoxManifest::default());
    }
    let content = fs::read_to_string(&toml_path)
        .map_err(|e| format!("Cannot read tinox.toml: {}", e))?;
    Ok(parse_manifest(&content))
}

fn format_dependency(dep: &Dependency) -> String {
    let mut s = format!(
        "[[dependencies]]\ngroup = \"{}\"\nartifactId = \"{}\"\nversion = \"{}\"\n",
        dep.group, dep.artifact_id, dep.version
    );
    if let Some(url) = &dep.url {
        s.push_str(&format!("url = \"{}\"\n", url));
    }
    if let Some(repository) = &dep.repository {
        s.push_str(&format!("repository = \"{}\"\n", repository));
    }
    if let Some(sha256) = &dep.sha256 {
        s.push_str(&format!("sha256 = \"{}\"\n", sha256));
    }
    s
}

fn format_repository(repo: &Repository) -> String {
    format!("[[repositories]]\nid = \"{}\"\nurl = \"{}\"\n", repo.id, repo.url)
}

/// Surgically rewrites `tinox.toml`'s `name`/`version`/`description` keys
/// (inside `[package]`) and every `[[dependencies]]` table, leaving every
/// OTHER line untouched — `entry`/`output` inside `[package]`, `[build]`,
/// `[metrics]`, `[database]`, comments, … all round-trip byte-for-byte.
/// A blind whole-file rewrite from just the `TinoxManifest` struct (which
/// doesn't model those other keys/sections at all) would silently drop
/// them — exactly the failure mode #154 was filed over, just moved one
/// layer deeper if done carelessly.
pub fn write_manifest(root: &Path, manifest: &TinoxManifest) -> Result<(), String> {
    let toml_path = root.join("tinox.toml");
    let existing = if toml_path.exists() {
        fs::read_to_string(&toml_path).map_err(|e| format!("Cannot read tinox.toml: {}", e))?
    } else {
        String::new()
    };

    let mut out: Vec<String> = Vec::new();
    let mut section = ManifestSection::None;
    let mut saw_package_header = false;

    for raw_line in existing.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            section = manifest_section_for(line);
            if section == ManifestSection::Dependency || section == ManifestSection::Repository {
                continue; // dropped — every [[dependencies]]/[[repositories]] table is rebuilt below
            }
            out.push(raw_line.to_string());
            if section == ManifestSection::Package {
                saw_package_header = true;
                // Fresh name/version/description right after the header;
                // any OLD copies of these three keys further down this
                // section are skipped below, everything else (entry,
                // output, …) round-trips untouched.
                if let Some(pkg) = &manifest.package {
                    out.push(format!("name = \"{}\"", pkg.name));
                    out.push(format!("version = \"{}\"", pkg.version));
                    out.push(format!("description = \"{}\"", pkg.description));
                }
            }
            continue;
        }
        match section {
            ManifestSection::Dependency | ManifestSection::Repository => {} // dropped — rebuilt below
            ManifestSection::Package => {
                let key = parse_toml_kv(line).map(|(k, _)| k);
                if !matches!(key, Some("name" | "version" | "description")) {
                    out.push(raw_line.to_string());
                }
            }
            ManifestSection::None | ManifestSection::Other => out.push(raw_line.to_string()),
        }
    }

    let mut content = out.join("\n");
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !saw_package_header {
        if let Some(pkg) = &manifest.package {
            content.push_str(&format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\ndescription = \"{}\"\n",
                pkg.name, pkg.version, pkg.description
            ));
        }
    }
    if !manifest.repositories.is_empty() {
        content.push('\n');
        for repo in &manifest.repositories {
            content.push_str(&format_repository(repo));
            content.push('\n');
        }
        content.pop();
    }
    if !manifest.dependencies.is_empty() {
        content.push('\n');
        for dep in &manifest.dependencies {
            content.push_str(&format_dependency(dep));
            content.push('\n');
        }
        // Drop the one trailing blank line left after the last dependency block.
        content.pop();
    }

    fs::write(&toml_path, content).map_err(|e| format!("Cannot write tinox.toml: {}", e))
}

/// Rejects anything that isn't a single, plain path segment: empty, ".", "..",
/// or containing a path separator would let a dependency's group/artifactId/version
/// escape `.tinox/deps` (e.g. via an absolute path or a `..` segment).
fn sanitize_path_component(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(format!(
            "invalid dependency {}: {:?} is not a valid path segment",
            field, value
        ));
    }
    Ok(())
}

pub fn dep_install_dir(root: &Path, dep: &Dependency) -> Result<PathBuf, String> {
    sanitize_path_component(&dep.group, "group")?;
    sanitize_path_component(&dep.artifact_id, "artifactId")?;
    sanitize_path_component(&dep.version, "version")?;
    Ok(root
        .join(".tinox")
        .join("deps")
        .join(&dep.group)
        .join(&dep.artifact_id)
        .join(&dep.version))
}

/// The tinox home directory for the GLOBAL, Maven-style repository cache
/// (`~/.tinox` by default) — `TINOX_HOME` overrides it, mirroring the
/// existing `TINOX_PATH` stdlib-dir override convention in `main.rs`. The
/// override is what makes this testable/isolatable (both in this file's own
/// unit tests and in CI) without ever touching a real `~/.tinox`.
fn tinox_home_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TINOX_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Install directory for a COORDINATE-resolved dependency (no explicit
/// `url`): `~/.tinox/repository/<repoId>/<group>/<artifactId>/<version>/`,
/// shared across every project on the machine. `repoId` is the dependency's
/// `repository` id, or the literal `"central"` for the default-fallback
/// case (no `url`, no `repository`).
///
/// Deliberately NOT unified with `dep_install_dir`'s project-local
/// `.tinox/deps/` tree (used for explicit-`url` dependencies): tinox-central
/// enforces immutable versions (a `group:artifactId:version` triple always
/// names the same bytes), so it's safe to share globally — a raw `url` has
/// no such guarantee, and two unrelated projects on the same machine could
/// legitimately declare the same coordinates pointing at different bytes.
/// Globalizing that case would be a silent cross-project collision; keeping
/// it project-scoped (as today) avoids that. If this ever gets "simplified"
/// into one shared cache, that guarantee is what would need re-establishing
/// first.
pub fn global_dep_install_dir(dep: &Dependency) -> Result<PathBuf, String> {
    let repo_id = dep.repository.as_deref().unwrap_or("central");
    sanitize_path_component(repo_id, "repository")?;
    sanitize_path_component(&dep.group, "group")?;
    sanitize_path_component(&dep.artifact_id, "artifactId")?;
    sanitize_path_component(&dep.version, "version")?;
    let home = tinox_home_dir()
        .ok_or_else(|| "Cannot determine home directory (HOME/TINOX_HOME unset)".to_string())?;
    Ok(home
        .join(".tinox")
        .join("repository")
        .join(repo_id)
        .join(&dep.group)
        .join(&dep.artifact_id)
        .join(&dep.version))
}

/// Picks the right install directory for `dep`: project-local `.tinox/deps/`
/// for an explicit-`url` dependency (unchanged behavior), the global
/// `~/.tinox/repository/...` cache for a coordinate-resolved one.
fn resolved_install_dir(root: &Path, dep: &Dependency) -> Result<PathBuf, String> {
    if dep.url.is_some() {
        dep_install_dir(root, dep)
    } else {
        global_dep_install_dir(dep)
    }
}

/// A coordinate-resolved dependency `manifest` declares that isn't in the
/// global cache yet — surfaced instead of silently dropped, so a caller can
/// tell "never declared" apart from "declared but `tinox install` hasn't
/// been run" (see `resolve_imports`'s use of this in `main.rs`).
#[derive(Debug, Clone)]
pub struct MissingDep {
    pub group: String,
    pub artifact_id: String,
    pub version: String,
}

/// Every installed dependency directory reachable for import resolution —
/// not just the ones this project's own tinox.toml lists directly. Merges
/// two trees: the project-local `.tinox/deps/**` glob (explicit-`url`
/// dependencies, unchanged since before the global cache existed — see
/// `install_dep_transitively`'s doc comment for why a blind glob is safe
/// there specifically, being inherently project-scoped) and the global,
/// coordinate-resolved cache (`global_dep_dirs`, scoped rather than
/// globbed — see there for why a blind glob of THAT tree would be unsafe).
pub fn installed_dep_dirs(root: &Path, manifest: &TinoxManifest) -> (Vec<PathBuf>, Vec<MissingDep>) {
    let deps_root = root.join(".tinox").join("deps");
    let mut dirs = Vec::new();
    if let Ok(groups) = fs::read_dir(&deps_root) {
        for group in groups.flatten() {
            let Ok(artifacts) = fs::read_dir(group.path()) else { continue };
            for artifact in artifacts.flatten() {
                let Ok(versions) = fs::read_dir(artifact.path()) else { continue };
                for version in versions.flatten() {
                    let p = version.path();
                    if p.is_dir() {
                        dirs.push(p);
                    }
                }
            }
        }
    }
    let (global_dirs, missing) = global_dep_dirs(manifest);
    dirs.extend(global_dirs);
    (dirs, missing)
}

/// Scoped transitive walk of the GLOBAL cache for `manifest`'s
/// coordinate-resolved dependencies (the ones with no explicit `url`,
/// handled instead by the project-local glob above). Deliberately NOT a
/// blind glob of `~/.tinox/repository/**`, unlike the project-local
/// `.tinox/deps/**` glob above — the global tree can hold every OTHER
/// project's cached dependencies ever fetched on this machine, and a blind
/// glob would pull all of them into every build's import resolution,
/// reintroducing exactly the ambiguous-import class of bug #156's hard
/// error exists to catch, just against a much larger, mostly irrelevant
/// candidate set. Instead: start from what THIS manifest (and, recursively,
/// whatever manifest each resolved dependency itself ships) actually
/// declares — mirrors `install_dep_transitively`'s walk, but read-only (no
/// network, no installation), with the same cycle-guard.
pub fn global_dep_dirs(manifest: &TinoxManifest) -> (Vec<PathBuf>, Vec<MissingDep>) {
    let mut dirs = Vec::new();
    let mut missing = Vec::new();
    let mut visited: HashSet<(String, String, String)> = HashSet::new();
    for dep in &manifest.dependencies {
        if dep.url.is_some() {
            continue; // handled by the project-local .tinox/deps/ glob instead
        }
        collect_global_dep_dir(dep, &mut dirs, &mut missing, &mut visited);
    }
    (dirs, missing)
}

fn collect_global_dep_dir(
    dep: &Dependency,
    dirs: &mut Vec<PathBuf>,
    missing: &mut Vec<MissingDep>,
    visited: &mut HashSet<(String, String, String)>,
) {
    let coord = (dep.group.clone(), dep.artifact_id.clone(), dep.version.clone());
    if !visited.insert(coord) {
        return;
    }
    let Ok(dir) = global_dep_install_dir(dep) else { return };
    if !dir.is_dir() {
        missing.push(MissingDep {
            group: dep.group.clone(),
            artifact_id: dep.artifact_id.clone(),
            version: dep.version.clone(),
        });
        return;
    }
    dirs.push(dir.clone());
    if let Ok(sub_manifest) = read_manifest(&dir) {
        for sub_dep in &sub_manifest.dependencies {
            if sub_dep.url.is_some() {
                continue;
            }
            collect_global_dep_dir(sub_dep, dirs, missing, visited);
        }
    }
}

/// GET with a few retries on TRANSIENT failures only: a 5xx status (server
/// overload/restart/reverse-proxy hiccup — observed in practice against
/// central.tinox-lang.de under concurrent load, e.g. multiple `cargo test`
/// shards installing the same coordinate at once) or a transport-level
/// error (DNS/connection reset). A 4xx status (404 not found, 401/403 auth)
/// is NOT retried — retrying can't fix a request that's wrong by
/// construction, only a genuinely transient server-side condition.
fn get_with_retry(url: &str) -> Result<ureq::Response, String> {
    // Returns a String, not ureq::Error, per clippy::result_large_err
    // (ureq::Error is 272 bytes — every install_dep call site immediately
    // formats it into a String anyway, see below, so there's no reason to
    // carry the large variant any further than this function).
    const MAX_ATTEMPTS: u32 = 4;
    const BACKOFF: [u64; 3] = [200, 600, 1500];
    let mut last_err = None;
    for attempt in 0..MAX_ATTEMPTS {
        match ureq::get(url).call() {
            Ok(resp) => return Ok(resp),
            Err(ureq::Error::Status(code, resp)) if !(500..600).contains(&code) => {
                return Err(ureq::Error::Status(code, resp).to_string());
            }
            Err(e) => {
                if attempt + 1 < MAX_ATTEMPTS {
                    eprintln!("  ({e}, retrying...)");
                    std::thread::sleep(std::time::Duration::from_millis(BACKOFF[attempt as usize]));
                }
                last_err = Some(e.to_string());
            }
        }
    }
    Err(last_err.expect("loop always sets last_err before exiting on failure"))
}

/// Resolves the expected checksum for `dep` per the priority described on
/// `install_dep`: an explicit `dep.sha256` always wins; otherwise, unless
/// `update` is set, a `tinox.lock` entry for the same coordinates *and*
/// effective URL pins it (a changed URL for the same version has no
/// comparable baseline, so it's treated as an unpinned first install rather
/// than a mismatch).
fn expected_checksum<'a>(dep: &'a Dependency, effective_url: &str, lock: &'a TinoxLock, update: bool) -> Option<&'a str> {
    dep.sha256.as_deref().or_else(|| {
        if update {
            None
        } else {
            lock_entry_for(lock, dep)
                .filter(|e| e.url == effective_url)
                .map(|e| e.sha256.as_str())
        }
    })
}

fn verify_checksum(dep: &Dependency, effective_url: &str, lock: &TinoxLock, update: bool, actual_sha256: &str) -> Result<(), String> {
    if let Some(expected) = expected_checksum(dep, effective_url, lock, update) {
        if !expected.eq_ignore_ascii_case(actual_sha256) {
            return Err(format!(
                "checksum mismatch for {}:{} {} ({}): expected sha256 {}, got {} — refusing to install a dependency whose content doesn't match what was pinned (tinox.toml/tinox.lock). Pass --update to re-pin if this URL's content legitimately changed.",
                dep.group, dep.artifact_id, dep.version, effective_url, expected, actual_sha256
            ));
        }
    }
    Ok(())
}

/// Installs one dependency, verifying the downloaded bytes against an
/// expected SHA-256 when one is available (`dep.sha256` from tinox.toml
/// takes priority; otherwise a matching `tinox.lock` entry for the same
/// group/artifactId/version/effective-url pins it). A mismatch is a hard
/// error — no silent fallback to "install anyway" — the same "no silent
/// garbage" principle the rest of this project follows (see CLAUDE.md).
/// Callers are responsible for persisting the resulting hash back into the
/// lock (see `cmd_install`/`cmd_add`) since this function only downloads.
///
/// `effective_url` is precomputed by the caller (`effective_download_url`,
/// a pure function of `dep` + whichever manifest declared it) rather than
/// recomputed here, so a single resolution feeds both the download and the
/// lock-entry/checksum bookkeeping consistently. Installs to
/// `resolved_install_dir`: project-local `.tinox/deps/` for an explicit-
/// `url` dependency, the global `~/.tinox/repository/...` cache for a
/// coordinate-resolved one.
fn install_dep(root: &Path, dep: &Dependency, effective_url: &str, lock: &TinoxLock, update: bool) -> Result<Option<String>, String> {
    let install_dir = resolved_install_dir(root, dep)?;
    if install_dir.exists() {
        println!(
            "  already installed: {}:{} {}",
            dep.group, dep.artifact_id, dep.version
        );
        return Ok(None);
    }

    println!(
        "  downloading {}:{} {} ...",
        dep.group, dep.artifact_id, dep.version
    );

    let response = get_with_retry(effective_url)
        .map_err(|e| format!("Download failed ({}): {}", effective_url, e))?;

    let mut raw_bytes: Vec<u8> = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut raw_bytes)
        .map_err(|e| format!("Read failed: {}", e))?;

    // A registry API (e.g. tinox-central) can't return artifact bytes as a
    // raw response body — tinox's `String` is NUL-terminated at the
    // runtime level (see tinox-central's PLAN.md §7.1), so any server
    // built with tinox.core.http_server has to wrap the artifact in a
    // `{"filename": "...", "contentBase64": "..."}` JSON envelope instead
    // of streaming octets directly. Detect and unwrap that shape before
    // falling back to "response body IS the artifact", so a dependency
    // URL pointing at such a registry (whose path rarely carries a
    // .tar.gz/.zip suffix) still resolves to the right filename/bytes.
    let (bytes, filename_hint): (Vec<u8>, Option<String>) =
        match parse_registry_envelope(&raw_bytes) {
            Some((filename, decoded)) => (decoded, Some(filename)),
            None => (raw_bytes, None),
        };

    let actual_sha256 = sha256_hex(&bytes);
    verify_checksum(dep, effective_url, lock, update, &actual_sha256)?;

    // Stage the extraction in a uniquely-named temp dir next to the real
    // install dir, then atomically rename it into place, rather than
    // extracting directly into `install_dir`. The global cache
    // (`resolved_install_dir` → `global_dep_install_dir`) is shared across
    // every project AND every concurrently-running caller on the machine
    // (e.g. multiple `cargo test` shards each independently resolving the
    // same coordinate) — extracting straight into the final path would let
    // two racing installs of the same coordinate interleave their writes
    // into one directory, and a third caller could read a half-written
    // package in between. A rename is atomic on the same filesystem, so
    // whichever caller finishes first "wins" and everyone else's staged
    // copy is simply discarded once they notice `install_dir` now exists.
    //
    // The uniqueness suffix must be unique per CALLER, not just per OS
    // process: `cargo test` runs multiple `#[test]`s as threads within one
    // process, so `std::process::id()` alone is identical across all of
    // them — two threads racing to install the same coordinate would then
    // pick the SAME staging dir name and stomp on each other (confirmed by
    // hand: this caused an intermittent "declared but not installed"
    // e2e failure before this counter was added). An atomic counter is
    // unique per call within the process regardless of threading; combined
    // with the pid it stays unique across separate processes too.
    static STAGING_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = STAGING_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staging_dir = install_dir.with_file_name(format!(
        ".{}-staging-{}-{}",
        install_dir.file_name().and_then(|n| n.to_str()).unwrap_or("pkg"),
        std::process::id(),
        unique,
    ));
    let _ = fs::remove_dir_all(&staging_dir);
    fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("Cannot create staging dir: {}", e))?;

    // Prefer the artifact's own filename (from a registry envelope) for
    // the tar.gz/zip/single-file dispatch — the dependency URL itself
    // (e.g. a registry API path with no file extension at all) isn't a
    // reliable signal in that case.
    let name_for_dispatch = filename_hint
        .clone()
        .unwrap_or_else(|| effective_url.split('/').next_back().unwrap_or("lib.tnx").to_string());
    let name_lower = name_for_dispatch.to_lowercase();
    let extract_result = if name_lower.ends_with(".tar.gz") || name_lower.ends_with(".tgz") {
        extract_tar_gz(&bytes, &staging_dir)
    } else if name_lower.ends_with(".zip") {
        extract_zip(&bytes, &staging_dir)
    } else {
        // Single .tnx file — save directly
        let filename = if name_for_dispatch.ends_with(".tnx") {
            name_for_dispatch
        } else {
            format!("{}.tnx", name_for_dispatch)
        };
        fs::write(staging_dir.join(filename), &bytes).map_err(|e| format!("Cannot write file: {}", e))
    };
    if let Err(e) = extract_result {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(e);
    }

    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create install parent dir: {}", e))?;
    }
    match fs::rename(&staging_dir, &install_dir) {
        Ok(()) => {}
        Err(_) if install_dir.is_dir() => {
            // Another process finished installing the same coordinate first
            // — that's a cache hit, not a failure. Discard our own copy.
            let _ = fs::remove_dir_all(&staging_dir);
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(format!("Cannot move staged install into place: {}", e));
        }
    }

    println!(
        "  installed: {}:{} {} (sha256 {})",
        dep.group, dep.artifact_id, dep.version, actual_sha256
    );
    Ok(Some(actual_sha256))
}

/// Recognizes a tinox-central-shaped download response — a JSON object
/// with (at least) `filename` and `contentBase64` string fields — and
/// returns the decoded artifact bytes plus its reported filename. `None`
/// for anything else (a plain tar.gz/zip/.tnx response body, which is
/// the common case for a dependency hosted as a static file), so callers
/// fall back to treating the raw bytes as the artifact itself.
///
/// Hand-rolled rather than pulling in a JSON crate, matching this file's
/// existing hand-rolled TOML manifest parser — the shape needed here is
/// two fixed string fields, not general JSON.
fn parse_registry_envelope(bytes: &[u8]) -> Option<(String, Vec<u8>)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let filename = extract_json_string_field(trimmed, "filename")?;
    let content_base64 = extract_json_string_field(trimmed, "contentBase64")?;
    let decoded = base64_decode(&content_base64)?;
    Some((filename, decoded))
}

/// Finds `"field": "value"` in a (trusted, server-controlled) JSON object
/// string and returns `value`, unescaping `\"` and `\\` only (the only
/// escapes tinox-central's `Json::serialize` needs for a filename/base64
/// payload — both are otherwise plain-ASCII fields).
fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{}\"", field);
    let key_pos = json.find(&needle)?;
    let after_key = &json[key_pos + needle.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();
    let mut chars = after_colon.char_indices();
    let (_, first) = chars.next()?;
    if first != '"' {
        return None;
    }
    let mut result = String::new();
    let mut escaped = false;
    for (i, c) in chars {
        if escaped {
            match c {
                '"' => result.push('"'),
                '\\' => result.push('\\'),
                other => result.push(other),
            }
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            let _ = i;
            return Some(result);
        }
        result.push(c);
    }
    None
}

/// One row of tinox-central's `GET /api/v1/packages` catalog response
/// (`PackageSummary.tnx` in the registry backend) — only the three string
/// fields `cmd_search` (issue #172) actually displays/filters on;
/// `versionCount`/`latestPublishedAt` are intentionally not parsed (no
/// `extract_json_int_field` exists in this file, and search has no use
/// for them yet).
struct PackageSummaryRow {
    group: String,
    artifact_id: String,
    latest_version: String,
}

/// Splits a top-level JSON array of objects (`[{...}, {...}, ...]`) into
/// each object's own substring, respecting nested `{}`/`[]` and skipping
/// braces inside quoted strings — needed because `extract_json_string_field`
/// only ever looks at ONE object's fields, and the catalog response is an
/// array of them. Same "one fixed, well-known shape, not a JSON crate"
/// rationale as the rest of this file's hand-rolled JSON handling.
fn split_json_object_array(json: &str) -> Option<Vec<&str>> {
    let start = json.find('[')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut obj_start: Option<usize> = None;
    let mut out = Vec::new();
    for (i, c) in json[start..].char_indices() {
        let byte_pos = start + i;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    obj_start = Some(byte_pos);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let s = obj_start.take()?;
                    out.push(&json[s..=byte_pos]);
                }
            }
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    Some(out)
}

fn parse_package_summaries(bytes: &[u8]) -> Option<Vec<PackageSummaryRow>> {
    let text = std::str::from_utf8(bytes).ok()?;
    split_json_object_array(text).map(|objects| {
        objects
            .into_iter()
            .filter_map(|obj| {
                Some(PackageSummaryRow {
                    group: extract_json_string_field(obj, "group")?,
                    artifact_id: extract_json_string_field(obj, "artifactId")?,
                    latest_version: extract_json_string_field(obj, "latestVersion")?,
                })
            })
            .collect()
    })
}

/// Case-insensitive substring match against `group:artifactId` — matches
/// either half, so `tinox search json` finds `tinox.core:json` via the
/// artifactId half without requiring the group to be typed too.
fn matches_search_query(row: &PackageSummaryRow, query: &str) -> bool {
    let q = query.to_lowercase();
    row.group.to_lowercase().contains(&q) || row.artifact_id.to_lowercase().contains(&q)
}

/// Standard base64 (RFC 4648, with `=` padding) decoder — the alphabet
/// `tinox.core.base64`'s `Base64::encodeBytes` uses on the server side.
/// Hand-rolled for the same reason as `parse_registry_envelope`: one
/// fixed, well-known format, not worth a new crate dependency for.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    let mut i = 0;
    while i < clean.len() {
        let b0 = *clean.get(i)?;
        let c0 = val(b0)?;
        let b1 = *clean.get(i + 1)?;
        let c1 = val(b1)?;
        let b2 = clean.get(i + 2).copied();
        let b3 = clean.get(i + 3).copied();

        out.push((c0 << 2) | (c1 >> 4));

        match (b2, b3) {
            (Some(b'='), _) | (None, _) => break,
            (Some(b2v), Some(b'=')) | (Some(b2v), None) => {
                let c2 = val(b2v)?;
                out.push((c1 << 4) | (c2 >> 2));
                break;
            }
            (Some(b2v), Some(b3v)) => {
                let c2 = val(b2v)?;
                out.push((c1 << 4) | (c2 >> 2));
                let c3 = val(b3v)?;
                out.push((c2 << 6) | c3);
            }
        }
        i += 4;
    }
    Some(out)
}

/// Standard base64 (RFC 4648, with `=` padding) encoder — the counterpart
/// to `base64_decode` above, needed by `cmd_publish` (issue #172) to build
/// the `contentBase64` field of a tinox-central publish request. Same
/// hand-rolled rationale as `base64_decode`.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        match (b1, b2) {
            (Some(b1), Some(b2)) => {
                out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
                out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
            }
            (Some(b1), None) => {
                out.push(ALPHABET[((b1 & 0x0f) << 2) as usize] as char);
                out.push('=');
            }
            (None, _) => {
                out.push('=');
                out.push('=');
            }
        }
    }
    out
}

/// Escapes `\` and `"` (the only characters `extract_json_string_field`
/// unescapes on the way in, kept symmetric on the way out) — enough for
/// the plain-ASCII filename/base64 fields `cmd_publish`'s request body
/// needs. Not a general JSON string escaper (no control-character/unicode
/// handling), matching this file's existing "one fixed shape, not a JSON
/// crate" convention (see `parse_registry_envelope`).
fn json_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::Cursor;
    use tar::Archive;

    let gz = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(gz);
    archive
        .unpack(dest)
        .map_err(|e| format!("Cannot extract tar.gz: {}", e))
}

fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), String> {
    use std::io::Cursor;

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("Cannot open zip: {}", e))?;
    archive
        .extract(dest)
        .map_err(|e| format!("Cannot extract zip: {}", e))
}

/// Installs `dep`, then — if its own install directory contains a
/// `tinox.toml` declaring further dependencies — recursively installs
/// those too (#157). Flat resolution: transitive dependencies land in
/// the SAME tree as direct ones (project-local `.tinox/deps/` for
/// explicit-`url` deps, the global `~/.tinox/repository/...` cache for
/// coordinate-resolved ones — see `resolved_install_dir`), not nested
/// per-dependency, matching the flat namespace `resolve_imports`/
/// `resolve_in_dep_dirs` already assumes. `visited` is shared across the
/// whole call tree for one `install`/`add` run: it guards against a
/// dependency cycle (A depends on B depends on A) and against re-walking
/// a coordinate reached twice (a diamond — two dependencies both
/// depending on the same third one at the same version).
///
/// `owning_manifest` is whichever tinox.toml actually declared `dep` — see
/// `effective_download_url`'s doc comment for why this must be threaded
/// through rather than always using the top-level project's manifest: a
/// `repository` reference on a transitive dependency resolves against the
/// repositories ITS OWN manifest configured, not the top-level consumer's.
///
/// Diamond dependencies at DIFFERENT versions of the same
/// group:artifactId are deliberately not specially handled here — they
/// install into two different version-suffixed directories without
/// conflict, and if that ever results in an import genuinely resolving
/// against both, #156's ambiguous-import hard error already catches it;
/// no separate version-conflict detection is needed on top of that.
///
/// Returns `(installed_ok, failed)`, aggregated over `dep` and everything
/// transitively reached from it.
fn install_dep_transitively(
    root: &Path,
    dep: &Dependency,
    owning_manifest: &TinoxManifest,
    lock: &mut TinoxLock,
    update: bool,
    visited: &mut HashSet<(String, String, String)>,
    lock_changed: &mut bool,
) -> (usize, usize) {
    let coord = (dep.group.clone(), dep.artifact_id.clone(), dep.version.clone());
    if !visited.insert(coord) {
        return (0, 0);
    }

    let mut ok = 0usize;
    let mut fail = 0usize;
    let effective_url = match effective_download_url(dep, owning_manifest) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("  error: {}", e);
            return (ok, fail + 1);
        }
    };
    match install_dep(root, dep, &effective_url, lock, update) {
        Ok(Some(sha256)) => {
            upsert_lock_entry(
                lock,
                LockEntry {
                    group: dep.group.clone(),
                    artifact_id: dep.artifact_id.clone(),
                    version: dep.version.clone(),
                    url: effective_url,
                    sha256,
                },
            );
            *lock_changed = true;
            ok += 1;
        }
        Ok(None) => ok += 1, // already installed, nothing new to pin
        Err(e) => {
            eprintln!("  error: {}", e);
            // A dependency we couldn't install has no readable manifest of
            // its own to walk — nothing transitive to attempt.
            return (ok, fail + 1);
        }
    }

    if let Ok(install_dir) = resolved_install_dir(root, dep) {
        // read_manifest returns an empty manifest (not an error) when the
        // dependency doesn't ship its own tinox.toml — the common case,
        // handled the same as "no transitive dependencies" below.
        if let Ok(sub_manifest) = read_manifest(&install_dir) {
            for sub_dep in &sub_manifest.dependencies {
                let (sub_ok, sub_fail) =
                    install_dep_transitively(root, sub_dep, &sub_manifest, lock, update, visited, lock_changed);
                ok += sub_ok;
                fail += sub_fail;
            }
        }
    }

    (ok, fail)
}

/// `tinox install [--update]`. Without `--update`, a dependency already
/// pinned in tinox.lock must download to the exact same sha256 or the
/// install fails (catches a dependency URL's content silently changing
/// underneath a pinned version — see #112). `--update` re-pins instead of
/// verifying, for when that change is intentional.
///
/// Returns `true` iff every dependency installed cleanly (or was already
/// installed) — callers (`main.rs`'s CLI dispatch) must turn a `false`
/// into a non-zero process exit code. Previously this returned `()`
/// unconditionally, so `tinox install` always exited 0 even when some
/// dependencies failed (only a `println!("N installed, M failed")` line
/// hinted at it) — a caller relying on the exit code alone (the e2e test
/// harness's `run_case`, or a CI script) would silently proceed with a
/// dependency actually missing, only to hit a confusing later failure
/// ("declared but not installed") instead of a clear one right here.
pub fn cmd_install(args: &[String]) -> bool {
    let update = args.iter().any(|a| a == "--update");
    let root = match find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: no tinox.toml found");
            return false;
        }
    };
    let manifest = match read_manifest(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return false;
        }
    };
    if manifest.dependencies.is_empty() {
        println!("No dependencies to install.");
        return true;
    }
    let mut lock = match read_lock(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {}", e);
            return false;
        }
    };
    println!(
        "Installing {} dependenc{} ...",
        manifest.dependencies.len(),
        if manifest.dependencies.len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut lock_changed = false;
    let mut visited: HashSet<(String, String, String)> = HashSet::new();
    for dep in &manifest.dependencies {
        let (dep_ok, dep_fail) =
            install_dep_transitively(&root, dep, &manifest, &mut lock, update, &mut visited, &mut lock_changed);
        ok += dep_ok;
        fail += dep_fail;
    }
    if lock_changed {
        if let Err(e) = write_lock(&root, &lock) {
            eprintln!("warning: failed to update tinox.lock: {}", e);
        }
    }
    println!("{} installed, {} failed", ok, fail);
    fail == 0
}

pub fn cmd_package() {
    let root = match find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: no tinox.toml found");
            return;
        }
    };
    let manifest = match read_manifest(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };

    let pkg = match &manifest.package {
        Some(p) => p.clone(),
        None => {
            eprintln!("error: tinox.toml is missing [package] section");
            return;
        }
    };

    match build_package_archive(&root, &pkg) {
        Ok(archive_path) => println!("Packaged: {}", archive_path.file_name().unwrap().to_string_lossy()),
        Err(e) => eprintln!("error: {}", e),
    }
}

/// Shared by `cmd_package` and `cmd_publish` (issue #172): stages `src/`
/// (plus `tinox.toml` at the archive root) into `<name>-<version>.tar.gz`
/// under `root`.
///
/// Archive entries are relative to src/, not to the project root: a
/// consumer's `tinox install` extracts this archive directly into
/// .tinox/deps/<group>/<artifactId>/<version>/, and import resolution
/// (resolve_in_dep_dirs in main.rs) looks for the imported module path
/// right under THAT directory — a leading "src/" in the archive would
/// put every file one level too deep and break every import of this
/// package (confirmed by hand: a consumer importing `foo.Bar` expects
/// <dep-dir>/foo/Bar.tnx, not <dep-dir>/src/foo/Bar.tnx).
///
/// tinox.toml itself rides along at the archive root (i.e. also
/// directly under the extracted dep dir), NOT relative to src/ like the
/// .tnx files — it's what makes this package's own [[dependencies]]
/// discoverable at all. Without it, install_dep_transitively's
/// `read_manifest(&install_dir)` silently sees "no manifest" (its
/// documented behavior for a dependency that "doesn't ship its own
/// tinox.toml") and drops every transitive dependency this package
/// declares — confirmed by hand: a package depending on this one
/// installed fine but silently missing everything BUT this package's
/// own direct files.
fn build_package_archive(root: &Path, pkg: &Package) -> Result<PathBuf, String> {
    let src_dir = root.join("src");
    if !src_dir.exists() {
        return Err("src/ directory not found".to_string());
    }

    let mut tnx_files: Vec<PathBuf> = Vec::new();
    collect_tnx_files(&src_dir, &mut tnx_files);
    if tnx_files.is_empty() {
        return Err("no .tnx source files found in src/".to_string());
    }

    let archive_name = format!("{}-{}.tar.gz", pkg.name, pkg.version);
    let archive_path = root.join(&archive_name);
    let manifest_path = root.join("tinox.toml");
    let extra: &[(&Path, &str)] = &[(manifest_path.as_path(), "tinox.toml")];
    build_tar_gz(&archive_path, &src_dir, &tnx_files, extra)?;
    Ok(archive_path)
}

/// `tinox publish [--repository <id>]` (issue #172): packages the current
/// project the same way `tinox package` does and uploads it to a
/// tinox-central-shaped registry (`POST /api/v1/{group}/{artifactId}/
/// {version}`, the same endpoint/payload shape `scripts/
/// publish-stdlib-ext.sh` already uses for the stdlib itself — see that
/// script's own doc comment for the API contract this mirrors).
///
/// Requires `TINOX_CENTRAL_ADMIN_KEY` — the registry backend's
/// `AuthValidator` only recognizes one shared admin bearer token today
/// (no per-user/per-package auth model exists yet), so this is,
/// deliberately, exactly as admin-scoped as the existing stdlib publish
/// script; a real multi-tenant auth model is future registry-side work,
/// not something this CLI command can paper over on its own.
pub fn cmd_publish(args: &[String]) {
    let mut explicit_repo: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repository" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("error: --repository requires a value");
                    return;
                };
                explicit_repo = Some(v.clone());
                i += 2;
            }
            other => {
                eprintln!("error: unknown argument '{}'", other);
                eprintln!("Usage: tinox publish [--repository <id>]");
                return;
            }
        }
    }

    let root = match find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: no tinox.toml found");
            return;
        }
    };
    let manifest = match read_manifest(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };
    let pkg = match &manifest.package {
        Some(p) => p.clone(),
        None => {
            eprintln!("error: tinox.toml is missing [package] section");
            return;
        }
    };
    let Some(group) = pkg.group.clone() else {
        eprintln!("error: tinox.toml's [package] section is missing `group` — publishing needs a full group:artifactId:version coordinate (artifactId/version come from `name`/`version`), e.g.:\n\n  [package]\n  name = \"{}\"\n  version = \"{}\"\n  group = \"your.group\"", pkg.name, pkg.version);
        return;
    };
    let Ok(admin_key) = std::env::var("TINOX_CENTRAL_ADMIN_KEY") else {
        eprintln!("error: TINOX_CENTRAL_ADMIN_KEY is not set (admin bearer token for the target registry)");
        return;
    };

    let base = match resolve_registry_base_url(explicit_repo.as_deref(), &manifest) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };

    let archive_path = match build_package_archive(&root, &pkg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };
    let archive_name = archive_path.file_name().unwrap().to_string_lossy().to_string();

    let result = (|| -> Result<(), String> {
        let content = fs::read(&archive_path).map_err(|e| format!("Cannot read archive: {}", e))?;
        let content_base64 = base64_encode(&content);
        let body = format!(
            "{{\"filename\":\"{}\",\"contentBase64\":\"{}\"}}",
            json_escape_string(&archive_name),
            content_base64
        );

        let url = format!("{}/api/v1/{}/{}/{}", base, group, pkg.name, pkg.version);
        println!("Publishing {}:{} {} to {} ...", group, pkg.name, pkg.version, base);
        let response = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", admin_key))
            .set("Content-Type", "application/json")
            .send_string(&body);

        match response {
            Ok(resp) => {
                println!("Published: {}:{} {}", group, pkg.name, pkg.version);
                let _ = resp;
                Ok(())
            }
            Err(ureq::Error::Status(409, _)) => Err(format!(
                "{}:{} {} already exists on {} — bump the version in tinox.toml to republish",
                group, pkg.name, pkg.version, base
            )),
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                Err(format!("publish failed (HTTP {}): {}", code, body))
            }
            Err(e) => Err(format!("publish failed: {}", e)),
        }
    })();

    let _ = fs::remove_file(&archive_path);
    if let Err(e) = result {
        eprintln!("error: {}", e);
    }
}

/// `tinox search <query> [--repository <id>]` (issue #172): queries a
/// tinox-central-shaped registry's `GET /api/v1/packages` catalog and
/// prints every `group:artifactId` whose group or artifactId contains
/// `query` (case-insensitive), alongside its latest published version.
/// Doesn't require a project (`tinox.toml`) unless `--repository` is
/// given — the default registry needs no local project context to query.
pub fn cmd_search(args: &[String]) {
    let mut query: Option<String> = None;
    let mut explicit_repo: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repository" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("error: --repository requires a value");
                    return;
                };
                explicit_repo = Some(v.clone());
                i += 2;
            }
            other if query.is_none() => {
                query = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("error: unexpected argument '{}'", other);
                eprintln!("Usage: tinox search <query> [--repository <id>]");
                return;
            }
        }
    }
    let Some(query) = query else {
        eprintln!("Usage: tinox search <query> [--repository <id>]");
        return;
    };

    let manifest = find_project_root()
        .and_then(|root| read_manifest(&root).ok())
        .unwrap_or_default();
    let base = match resolve_registry_base_url(explicit_repo.as_deref(), &manifest) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };

    let url = format!("{}/api/v1/packages", base);
    let response = match get_with_retry(&url) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: search failed ({}): {}", url, e);
            return;
        }
    };
    let mut raw_bytes: Vec<u8> = Vec::new();
    if let Err(e) = response.into_reader().read_to_end(&mut raw_bytes) {
        eprintln!("error: search failed: {}", e);
        return;
    }
    let Some(rows) = parse_package_summaries(&raw_bytes) else {
        eprintln!("error: search failed: could not parse catalog response from {}", base);
        return;
    };

    let matches: Vec<&PackageSummaryRow> = rows.iter().filter(|r| matches_search_query(r, &query)).collect();
    if matches.is_empty() {
        println!("No packages found matching \"{}\" on {}", query, base);
        return;
    }
    for row in matches {
        println!("{}:{}  {}", row.group, row.artifact_id, row.latest_version);
    }
}

fn collect_tnx_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_tnx_files(&path, out);
        } else if path.extension().map(|e| e == "tnx").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// `extra` are files added under an explicit archive-relative name instead
/// of being stripped relative to `root` — namely tinox.toml, which lives
/// at the project root while every other archived file lives under src/.
fn build_tar_gz(
    archive_path: &Path,
    root: &Path,
    files: &[PathBuf],
    extra: &[(&Path, &str)],
) -> Result<(), String> {
    use flate2::{write::GzEncoder, Compression};
    use tar::Builder;

    let file = fs::File::create(archive_path)
        .map_err(|e| format!("Cannot create archive: {}", e))?;
    let gz = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(gz);

    for file_path in files {
        let rel = file_path
            .strip_prefix(root)
            .map_err(|_| format!("Path error: {}", file_path.display()))?;
        builder
            .append_path_with_name(file_path, rel)
            .map_err(|e| format!("Cannot add {}: {}", rel.display(), e))?;
    }

    for (extra_path, archive_name) in extra {
        if extra_path.exists() {
            builder
                .append_path_with_name(extra_path, archive_name)
                .map_err(|e| format!("Cannot add {}: {}", archive_name, e))?;
        }
    }

    builder
        .finish()
        .map_err(|e| format!("Cannot finalize archive: {}", e))?;

    Ok(())
}

pub fn cmd_add(args: &[String]) {
    if args.len() < 4 {
        eprintln!("Usage: tinox add <group> <artifactId> <version> <url>");
        return;
    }
    let dep = Dependency {
        group: args[0].clone(),
        artifact_id: args[1].clone(),
        version: args[2].clone(),
        url: Some(args[3].clone()),
        repository: None,
        sha256: None,
    };
    let root = match find_project_root() {
        Some(r) => r,
        None => {
            eprintln!("error: no tinox.toml found");
            return;
        }
    };
    let mut manifest = match read_manifest(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {}", e);
            return;
        }
    };
    manifest
        .dependencies
        .retain(|d| !(d.group == dep.group && d.artifact_id == dep.artifact_id));
    manifest.dependencies.push(dep.clone());
    if let Err(e) = write_manifest(&root, &manifest) {
        eprintln!("error: {}", e);
        return;
    }
    println!(
        "Added {}:{} {} to tinox.toml",
        dep.group, dep.artifact_id, dep.version
    );
    // The real on-disk lock, not a fresh empty one: `dep` itself has
    // nothing pinned yet either way (a brand-new coordinate can't be in
    // it), but any TRANSITIVE dependency reached from it (#157) might
    // already be pinned from an earlier `install`/`add`, and should still
    // be checksum-verified against that, not treated as unpinned.
    let mut lock = match read_lock(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("warning: failed to read tinox.lock: {}", e);
            TinoxLock::default()
        }
    };
    let mut lock_changed = false;
    let mut visited: HashSet<(String, String, String)> = HashSet::new();
    install_dep_transitively(&root, &dep, &manifest, &mut lock, false, &mut visited, &mut lock_changed);
    if lock_changed {
        if let Err(e) = write_lock(&root, &lock) {
            eprintln!("warning: failed to update tinox.lock: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TINOX_HOME` is process-global state; `cargo test` runs tests in
    /// parallel threads by default, so any test that sets/reads it must
    /// hold this lock for its whole body — otherwise two such tests can
    /// interleave their `set_var`/read, each seeing the OTHER's value.
    /// (Found the hard way: `global_dep_dirs_finds_an_installed_coordinate_
    /// dep_and_its_transitive_deps` failed intermittently before this was
    /// added, with `dirs` empty despite the fixture directories existing
    /// on disk — a second TINOX_HOME-touching test had stomped the env var
    /// mid-test.)
    static TINOX_HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn dep(group: &str, artifact_id: &str, version: &str) -> Dependency {
        Dependency {
            group: group.to_string(),
            artifact_id: artifact_id.to_string(),
            version: version.to_string(),
            url: Some("https://example.com/lib.tnx".to_string()),
            repository: None,
            sha256: None,
        }
    }

    fn lock_entry(d: &Dependency, sha256: &str) -> LockEntry {
        LockEntry {
            group: d.group.clone(),
            artifact_id: d.artifact_id.clone(),
            version: d.version.clone(),
            url: d.url.clone().unwrap_or_default(),
            sha256: sha256.to_string(),
        }
    }

    #[test]
    fn find_project_root_from_walks_up_from_an_arbitrary_start_dir_not_cwd() {
        // #172 follow-on prerequisite: `tinox build <path>` must find the
        // built file's OWN tinox.toml even when cwd is somewhere unrelated
        // (dogfood.sh builds from the repo root; the e2e harness builds from
        // an isolated temp workdir) — this is the fix for that.
        let root = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "find_project_root_from"
        ));
        let nested = root.join("src").join("deeper");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("tinox.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\ndescription = \"\"\n").unwrap();

        // cwd (wherever the test runner happens to be) has no tinox.toml of
        // its own reachable from `nested` — only walking up from `nested`
        // itself finds it.
        assert_eq!(find_project_root_from(&nested), Some(root.clone()));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn find_project_root_from_stops_at_the_nearest_tinox_toml_not_a_further_one() {
        // A tinox.toml two levels up must not shadow one right at `start`'s
        // own directory — the walk should return the CLOSEST ancestor match.
        let root = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "find_project_root_from_nearest"
        ));
        let inner = root.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(root.join("tinox.toml"), "[package]\nname = \"outer\"\nversion = \"0.1.0\"\ndescription = \"\"\n").unwrap();
        fs::write(inner.join("tinox.toml"), "[package]\nname = \"inner\"\nversion = \"0.1.0\"\ndescription = \"\"\n").unwrap();

        assert_eq!(find_project_root_from(&inner), Some(inner.clone()));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_dotdot_traversal_in_any_field() {
        let root = Path::new("/project");
        assert!(dep_install_dir(root, &dep("../../etc", "x", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("x", "../../etc", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("x", "y", "..")).is_err());
        assert!(dep_install_dir(root, &dep("x", "y", "../../../tmp/evil")).is_err());
    }

    #[test]
    fn rejects_absolute_and_separator_segments() {
        let root = Path::new("/project");
        assert!(dep_install_dir(root, &dep("/etc", "x", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("x", "a/b", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("x", "a\\b", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("", "x", "1.0")).is_err());
        assert!(dep_install_dir(root, &dep("x", ".", "1.0")).is_err());
    }

    #[test]
    fn accepts_normal_coordinates_and_stays_under_deps() {
        let root = Path::new("/project");
        let dir = dep_install_dir(root, &dep("com.example", "mylib", "1.2.3")).unwrap();
        assert!(dir.starts_with(root.join(".tinox").join("deps")));
        assert_eq!(dir, root.join(".tinox/deps/com.example/mylib/1.2.3"));
    }

    #[test]
    fn installed_dep_dirs_finds_transitively_installed_deps_not_just_direct_ones() {
        // #172 follow-on: install_dep_transitively installs a dependency's
        // OWN dependencies into the same flat .tinox/deps tree as direct
        // ones. installed_dep_dirs must surface all of them for import
        // resolution, not just whatever the project's own tinox.toml lists
        // directly — otherwise a package like oidc (which itself imports
        // oauth2) fails to compile for a consumer that only declared oidc.
        let root = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "installed_dep_dirs_transitive"
        ));
        let direct = root.join(".tinox/deps/tinox.core/oidc/1.0.0");
        let transitive = root.join(".tinox/deps/tinox.core/oauth2/1.0.0");
        fs::create_dir_all(&direct).unwrap();
        fs::create_dir_all(&transitive).unwrap();

        // Empty manifest: nothing declared directly in tinox.toml here —
        // both dirs must still show up, since they're both already on disk.
        let manifest = TinoxManifest::default();
        let (mut dirs, missing) = installed_dep_dirs(&root, &manifest);
        dirs.sort();
        let mut expected = vec![direct, transitive];
        expected.sort();
        assert_eq!(dirs, expected);
        assert!(missing.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("") and sha256("abc") — canonical FIPS 180-4 test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_checksum_with_no_pin_accepts_anything() {
        let d = dep("g", "a", "1.0");
        let lock = TinoxLock::default();
        assert!(verify_checksum(&d, d.url.as_deref().unwrap(), &lock, false, "deadbeef").is_ok());
    }

    #[test]
    fn verify_checksum_explicit_sha256_takes_priority_over_lock() {
        let mut d = dep("g", "a", "1.0");
        d.sha256 = Some("AAAA".to_string()); // uppercase — comparison must be case-insensitive
        let mut lock = TinoxLock::default();
        lock.dependencies.push(lock_entry(&d, "bbbb")); // would mismatch if consulted
        let url = d.url.clone().unwrap();
        assert!(verify_checksum(&d, &url, &lock, false, "aaaa").is_ok());
        assert!(verify_checksum(&d, &url, &lock, false, "cccc").is_err());
    }

    #[test]
    fn verify_checksum_falls_back_to_lock_entry() {
        let d = dep("g", "a", "1.0");
        let mut lock = TinoxLock::default();
        lock.dependencies.push(lock_entry(&d, "cafebabe"));
        let url = d.url.clone().unwrap();
        assert!(verify_checksum(&d, &url, &lock, false, "cafebabe").is_ok());
        let err = verify_checksum(&d, &url, &lock, false, "00000000").unwrap_err();
        assert!(err.contains("checksum mismatch"), "unexpected message: {err}");
        assert!(err.contains("--update"), "should mention the escape hatch: {err}");
    }

    #[test]
    fn verify_checksum_lock_entry_for_different_url_is_not_a_pin() {
        let mut d = dep("g", "a", "1.0");
        let mut lock = TinoxLock::default();
        lock.dependencies.push(lock_entry(&d, "cafebabe")); // pinned for the original URL
        d.url = Some("https://example.com/moved.tnx".to_string()); // same coordinates, different source
        // No comparable baseline for the new URL — anything is accepted rather than
        // spuriously failing against a hash that describes a different download.
        assert!(verify_checksum(&d, d.url.as_deref().unwrap(), &lock, false, "anything").is_ok());
    }

    #[test]
    fn verify_checksum_update_flag_bypasses_lock_pin() {
        let d = dep("g", "a", "1.0");
        let mut lock = TinoxLock::default();
        lock.dependencies.push(lock_entry(&d, "cafebabe"));
        // Without --update this would fail; with it, re-pinning is allowed.
        assert!(verify_checksum(&d, d.url.as_deref().unwrap(), &lock, true, "brand-new-hash").is_ok());
    }

    #[test]
    fn lock_roundtrips_through_yaml() {
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "lock_roundtrips_through_yaml"
        ));
        fs::create_dir_all(&dir).unwrap();
        let d = dep("g", "a", "1.0");
        let mut lock = TinoxLock::default();
        upsert_lock_entry(&mut lock, lock_entry(&d, "aaaa"));
        write_lock(&dir, &lock).unwrap();

        let read_back = read_lock(&dir).unwrap();
        assert_eq!(read_back.dependencies.len(), 1);
        assert_eq!(read_back.dependencies[0].sha256, "aaaa");

        // upsert replaces rather than duplicates an entry for the same coordinates
        upsert_lock_entry(&mut lock, lock_entry(&d, "bbbb"));
        assert_eq!(lock.dependencies.len(), 1);
        assert_eq!(lock.dependencies[0].sha256, "bbbb");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_manifest_reads_package_and_dependencies() {
        let content = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"x\"\nentry = \"src/main.tnx\"\n\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"mylib\"\nversion = \"1.0.0\"\nurl = \"https://example.com/mylib.tar.gz\"\nsha256 = \"abc123\"\n";
        let m = parse_manifest(content);
        let pkg = m.package.expect("package");
        assert_eq!(pkg.name, "demo");
        assert_eq!(pkg.version, "0.1.0");
        assert_eq!(pkg.description, "x");
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].group, "com.example");
        assert_eq!(m.dependencies[0].artifact_id, "mylib");
        assert_eq!(m.dependencies[0].sha256.as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_manifest_missing_file_content_has_no_package() {
        let m = parse_manifest("");
        assert!(m.package.is_none());
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn write_manifest_preserves_unrelated_toml_sections_and_keys() {
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "write_manifest_preserves"
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("tinox.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/main.tnx\"\n\n[build]\noutput = \"demo_bin\"\n\n[metrics]\nenabled = true\n",
        )
        .unwrap();

        let mut manifest = read_manifest(&dir).unwrap();
        manifest.dependencies.push(dep("com.example", "mylib", "1.0.0"));
        write_manifest(&dir, &manifest).unwrap();

        let rewritten = fs::read_to_string(dir.join("tinox.toml")).unwrap();
        assert!(rewritten.contains("entry = \"src/main.tnx\""), "{rewritten}");
        assert!(rewritten.contains("[build]"), "{rewritten}");
        assert!(rewritten.contains("output = \"demo_bin\""), "{rewritten}");
        assert!(rewritten.contains("[metrics]"), "{rewritten}");
        assert!(rewritten.contains("enabled = true"), "{rewritten}");
        assert!(rewritten.contains("[[dependencies]]"), "{rewritten}");
        assert!(rewritten.contains("artifactId = \"mylib\""), "{rewritten}");

        // Round-trips cleanly through read_manifest again, and the
        // preserved [package] section still parses correctly.
        let reread = read_manifest(&dir).unwrap();
        assert_eq!(reread.package.unwrap().name, "demo");
        assert_eq!(reread.dependencies.len(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_manifest_dedup_replaces_existing_coordinate() {
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "write_manifest_dedup"
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut manifest = TinoxManifest {
            package: Some(Package { name: "demo".to_string(), version: "0.1.0".to_string(), description: String::new(), group: None }),
            dependencies: vec![dep("com.example", "mylib", "1.0.0")],
            repositories: Vec::new(),
        };
        write_manifest(&dir, &manifest).unwrap();

        manifest.dependencies.retain(|d| d.artifact_id != "mylib");
        manifest.dependencies.push(dep("com.example", "mylib", "2.0.0"));
        write_manifest(&dir, &manifest).unwrap();

        let reread = read_manifest(&dir).unwrap();
        assert_eq!(reread.dependencies.len(), 1);
        assert_eq!(reread.dependencies[0].version, "2.0.0");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_manifest_reads_repositories_and_coordinate_only_dependency() {
        let content = "[[repositories]]\nid = \"internal\"\nurl = \"https://pkg.example.internal\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"rest\"\nversion = \"1.0.0\"\n\n[[dependencies]]\ngroup = \"com.acme\"\nartifactId = \"widgets\"\nversion = \"2.1.0\"\nrepository = \"internal\"\n";
        let m = parse_manifest(content);
        assert_eq!(m.repositories.len(), 1);
        assert_eq!(m.repositories[0].id, "internal");
        assert_eq!(m.repositories[0].url, "https://pkg.example.internal");
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dependencies[0].url, None);
        assert_eq!(m.dependencies[0].repository, None);
        assert_eq!(m.dependencies[1].repository.as_deref(), Some("internal"));
    }

    #[test]
    fn write_manifest_round_trips_repositories() {
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "write_manifest_repositories"
        ));
        fs::create_dir_all(&dir).unwrap();
        let manifest = TinoxManifest {
            package: Some(Package { name: "demo".to_string(), version: "0.1.0".to_string(), description: String::new(), group: None }),
            dependencies: vec![],
            repositories: vec![Repository { id: "internal".to_string(), url: "https://pkg.example.internal".to_string() }],
        };
        write_manifest(&dir, &manifest).unwrap();

        let rewritten = fs::read_to_string(dir.join("tinox.toml")).unwrap();
        assert!(rewritten.contains("[[repositories]]"), "{rewritten}");
        assert!(rewritten.contains("id = \"internal\""), "{rewritten}");

        let reread = read_manifest(&dir).unwrap();
        assert_eq!(reread.repositories.len(), 1);
        assert_eq!(reread.repositories[0].id, "internal");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn effective_download_url_uses_explicit_url_as_is() {
        let d = dep("g", "a", "1.0"); // url = Some("https://example.com/lib.tnx")
        let m = TinoxManifest::default();
        assert_eq!(effective_download_url(&d, &m).unwrap(), "https://example.com/lib.tnx");
    }

    #[test]
    fn effective_download_url_rejects_both_url_and_repository() {
        let mut d = dep("g", "a", "1.0");
        d.repository = Some("internal".to_string());
        let m = TinoxManifest::default();
        let err = effective_download_url(&d, &m).unwrap_err();
        assert!(err.contains("both `url` and `repository`"), "{err}");
    }

    #[test]
    fn effective_download_url_defaults_to_central_when_unqualified() {
        let mut d = dep("tinox.core", "rest", "1.0.0");
        d.url = None;
        let m = TinoxManifest::default(); // no [[repositories]] configured at all
        assert_eq!(
            effective_download_url(&d, &m).unwrap(),
            "https://central.tinox-lang.de/api/v1/tinox.core/rest/1.0.0"
        );
    }

    #[test]
    fn effective_download_url_default_ignores_configured_repositories_when_unreferenced() {
        // An unqualified dependency does NOT pick "the first configured
        // repository" — it always falls back to the hardcoded default, even
        // if [[repositories]] entries exist elsewhere in the same manifest.
        let mut d = dep("tinox.core", "rest", "1.0.0");
        d.url = None;
        let m = TinoxManifest {
            package: None,
            dependencies: vec![],
            repositories: vec![Repository { id: "internal".to_string(), url: "https://pkg.example.internal".to_string() }],
        };
        assert_eq!(
            effective_download_url(&d, &m).unwrap(),
            "https://central.tinox-lang.de/api/v1/tinox.core/rest/1.0.0"
        );
    }

    #[test]
    fn effective_download_url_resolves_named_repository() {
        let mut d = dep("com.acme", "widgets", "2.1.0");
        d.url = None;
        d.repository = Some("internal".to_string());
        let m = TinoxManifest {
            package: None,
            dependencies: vec![],
            repositories: vec![Repository { id: "internal".to_string(), url: "https://pkg.example.internal/".to_string() }],
        };
        assert_eq!(
            effective_download_url(&d, &m).unwrap(),
            "https://pkg.example.internal/api/v1/com.acme/widgets/2.1.0"
        );
    }

    #[test]
    fn effective_download_url_unknown_repository_id_is_a_hard_error() {
        let mut d = dep("com.acme", "widgets", "2.1.0");
        d.url = None;
        d.repository = Some("nonexistent".to_string());
        let m = TinoxManifest::default();
        let err = effective_download_url(&d, &m).unwrap_err();
        assert!(err.contains("nonexistent"), "{err}");
        assert!(err.contains("no [[repositories]] entry"), "{err}");
    }

    #[test]
    fn global_dep_install_dir_shape_uses_home_or_tinox_home_override() {
        let _guard = TINOX_HOME_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("TINOX_HOME", "/tmp/tinox-home-test-fixture") };
        let mut d = dep("tinox.core", "rest", "1.0.0");
        d.url = None;
        let dir = global_dep_install_dir(&d).unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/tmp/tinox-home-test-fixture/.tinox/repository/central/tinox.core/rest/1.0.0")
        );
        unsafe { std::env::remove_var("TINOX_HOME") };
    }

    #[test]
    fn global_dep_install_dir_uses_repository_id_when_set() {
        let _guard = TINOX_HOME_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("TINOX_HOME", "/tmp/tinox-home-test-fixture2") };
        let mut d = dep("com.acme", "widgets", "2.1.0");
        d.url = None;
        d.repository = Some("internal".to_string());
        let dir = global_dep_install_dir(&d).unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/tmp/tinox-home-test-fixture2/.tinox/repository/internal/com.acme/widgets/2.1.0")
        );
        unsafe { std::env::remove_var("TINOX_HOME") };
    }

    #[test]
    fn global_dep_dirs_reports_missing_dep_instead_of_silently_dropping_it() {
        let _guard = TINOX_HOME_ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "global_dep_dirs_missing"
        ));
        fs::create_dir_all(&home).unwrap();
        unsafe { std::env::set_var("TINOX_HOME", home.to_str().unwrap()) };

        let mut d = dep("tinox.core", "amqp10", "1.0.0");
        d.url = None; // coordinate-only, not yet installed anywhere
        let manifest = TinoxManifest {
            package: None,
            dependencies: vec![d],
            repositories: Vec::new(),
        };
        let (dirs, missing) = global_dep_dirs(&manifest);
        assert!(dirs.is_empty());
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].group, "tinox.core");
        assert_eq!(missing[0].artifact_id, "amqp10");

        unsafe { std::env::remove_var("TINOX_HOME") };
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn global_dep_dirs_finds_an_installed_coordinate_dep_and_its_transitive_deps() {
        let _guard = TINOX_HOME_ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "global_dep_dirs_found"
        ));
        fs::create_dir_all(&home).unwrap();
        unsafe { std::env::set_var("TINOX_HOME", home.to_str().unwrap()) };

        // Simulate a prior successful install: oidc (declared directly) and
        // oauth2 (its own transitive dep, per oidc's own tinox.toml) both
        // already sitting in the global cache.
        let oidc_dir = home.join(".tinox/repository/central/tinox.core/oidc/1.0.0");
        let oauth2_dir = home.join(".tinox/repository/central/tinox.core/oauth2/1.0.0");
        fs::create_dir_all(&oidc_dir).unwrap();
        fs::create_dir_all(&oauth2_dir).unwrap();
        fs::write(
            oidc_dir.join("tinox.toml"),
            "[package]\nname = \"oidc\"\nversion = \"1.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"oauth2\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let mut d = dep("tinox.core", "oidc", "1.0.0");
        d.url = None;
        let manifest = TinoxManifest { package: None, dependencies: vec![d], repositories: Vec::new() };
        let (mut dirs, missing) = global_dep_dirs(&manifest);
        dirs.sort();
        let mut expected = vec![oidc_dir, oauth2_dir];
        expected.sort();
        assert_eq!(dirs, expected);
        assert!(missing.is_empty());

        unsafe { std::env::remove_var("TINOX_HOME") };
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn base64_decode_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_decode("").unwrap(), b"".to_vec());
        assert_eq!(base64_decode("Zg==").unwrap(), b"f".to_vec());
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo".to_vec());
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo".to_vec());
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob".to_vec());
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba".to_vec());
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar".to_vec());
    }

    #[test]
    fn base64_decode_roundtrips_binary_with_embedded_nul() {
        // The whole point of this codepath: bytes a plain tinox `String`
        // can't represent (embedded 0x00) must still decode correctly.
        let raw: Vec<u8> = vec![0x00, 0x01, 0xff, 0x00, b'A', 0xfe];
        // Precomputed standard base64 of the bytes above.
        let encoded = "AAH/AEH+";
        assert_eq!(base64_decode(encoded).unwrap(), raw);
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // Same RFC 4648 vectors as base64_decode_matches_known_vectors, the
        // other direction — round-trips through base64_decode too, since
        // that's exactly how cmd_publish's payload gets consumed server-side.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_decode_roundtrips_binary_with_embedded_nul() {
        let raw: Vec<u8> = vec![0x00, 0x01, 0xff, 0x00, b'A', 0xfe];
        assert_eq!(base64_decode(&base64_encode(&raw)).unwrap(), raw);
    }

    #[test]
    fn json_escape_string_escapes_backslash_and_quote() {
        assert_eq!(json_escape_string(r#"weird"name\.tnx"#), r#"weird\"name\\.tnx"#);
        assert_eq!(json_escape_string("plain-1.0.0.tar.gz"), "plain-1.0.0.tar.gz");
    }

    #[test]
    fn parse_package_summaries_parses_catalog_array() {
        let json = br#"[
            {"group":"tinox.core","artifactId":"json","latestVersion":"1.0.0","versionCount":1,"latestPublishedAt":123},
            {"group":"tinox.core","artifactId":"rest","latestVersion":"1.0.2","versionCount":3,"latestPublishedAt":456}
        ]"#;
        let rows = parse_package_summaries(json).expect("catalog should parse");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group, "tinox.core");
        assert_eq!(rows[0].artifact_id, "json");
        assert_eq!(rows[0].latest_version, "1.0.0");
        assert_eq!(rows[1].artifact_id, "rest");
        assert_eq!(rows[1].latest_version, "1.0.2");
    }

    #[test]
    fn parse_package_summaries_empty_catalog() {
        let rows = parse_package_summaries(b"[]").expect("empty catalog should parse");
        assert!(rows.is_empty());
    }

    #[test]
    fn matches_search_query_matches_group_or_artifact_id_case_insensitively() {
        let row = PackageSummaryRow {
            group: "tinox.core".to_string(),
            artifact_id: "json".to_string(),
            latest_version: "1.0.0".to_string(),
        };
        assert!(matches_search_query(&row, "json"));
        assert!(matches_search_query(&row, "JSON"));
        assert!(matches_search_query(&row, "tinox.core"));
        assert!(matches_search_query(&row, "core"));
        assert!(!matches_search_query(&row, "rest"));
    }

    #[test]
    fn resolve_registry_base_url_defaults_to_central_with_no_explicit_repo() {
        let m = TinoxManifest::default();
        assert_eq!(resolve_registry_base_url(None, &m).unwrap(), DEFAULT_REPOSITORY_URL);
    }

    #[test]
    fn resolve_registry_base_url_resolves_named_repository() {
        let m = TinoxManifest {
            package: None,
            dependencies: vec![],
            repositories: vec![Repository { id: "internal".to_string(), url: "https://pkg.example.com/".to_string() }],
        };
        assert_eq!(resolve_registry_base_url(Some("internal"), &m).unwrap(), "https://pkg.example.com");
    }

    #[test]
    fn resolve_registry_base_url_unknown_repository_id_is_a_hard_error() {
        let m = TinoxManifest::default();
        let err = resolve_registry_base_url(Some("nope"), &m).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn parse_manifest_reads_package_group() {
        let content = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"\"\ngroup = \"com.example\"\n";
        let m = parse_manifest(content);
        assert_eq!(m.package.expect("package").group.as_deref(), Some("com.example"));
    }

    #[test]
    fn parse_manifest_missing_group_is_none() {
        let content = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"\"\n";
        let m = parse_manifest(content);
        assert_eq!(m.package.expect("package").group, None);
    }

    #[test]
    fn parse_registry_envelope_extracts_filename_and_decodes_content() {
        let json = br#"{"filename":"websocket-1.0.0.tar.gz","sha256":"abc","sizeBytes":3,"contentBase64":"Zm9v"}"#;
        let (filename, decoded) = parse_registry_envelope(json).expect("envelope should parse");
        assert_eq!(filename, "websocket-1.0.0.tar.gz");
        assert_eq!(decoded, b"foo".to_vec());
    }

    #[test]
    fn parse_registry_envelope_handles_escaped_filename() {
        let json = br#"{"filename":"weird\"name\\.tnx","contentBase64":"Zg=="}"#;
        let (filename, decoded) = parse_registry_envelope(json).expect("envelope should parse");
        assert_eq!(filename, "weird\"name\\.tnx");
        assert_eq!(decoded, b"f".to_vec());
    }

    #[test]
    fn build_tar_gz_entries_have_no_src_prefix() {
        // #172 follow-on: `tinox package` archives must extract directly
        // into a dependency install dir with the module path at the top
        // level (matching resolve_in_dep_dirs' expectations), not nested
        // under an extra "src/" the consumer never asked for.
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "build_tar_gz_no_src_prefix"
        ));
        let src_dir = dir.join("src");
        fs::create_dir_all(src_dir.join("foo")).unwrap();
        fs::write(src_dir.join("foo").join("Bar.tnx"), "class Bar {}").unwrap();

        let files = vec![src_dir.join("foo").join("Bar.tnx")];
        let archive_path = dir.join("out.tar.gz");
        build_tar_gz(&archive_path, &src_dir, &files, &[]).unwrap();

        let extract_dir = dir.join("extracted");
        let bytes = fs::read(&archive_path).unwrap();
        extract_tar_gz(&bytes, &extract_dir).unwrap();

        assert!(
            extract_dir.join("foo").join("Bar.tnx").exists(),
            "expected foo/Bar.tnx directly under the extract dir, found: {:?}",
            fs::read_dir(&extract_dir).ok().map(|e| e.filter_map(|x| x.ok().map(|x| x.path())).collect::<Vec<_>>())
        );
        assert!(!extract_dir.join("src").exists(), "archive must not carry a leading src/ path segment");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_tar_gz_carries_manifest_for_transitive_deps() {
        // A dependency's own [[dependencies]] are only discoverable by
        // install_dep_transitively if tinox.toml itself is in the
        // archive, at the extracted root (NOT under src/, unlike every
        // .tnx file) — otherwise read_manifest(&install_dir) silently
        // sees "no manifest" and drops every transitive dependency.
        let dir = std::env::temp_dir().join(format!(
            "tinox-pm-test-{}-{}",
            std::process::id(),
            "build_tar_gz_manifest"
        ));
        let src_dir = dir.join("src");
        fs::create_dir_all(src_dir.join("foo")).unwrap();
        fs::write(src_dir.join("foo").join("Bar.tnx"), "class Bar {}").unwrap();
        fs::write(
            dir.join("tinox.toml"),
            "[package]\nname = \"foo\"\nversion = \"1.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"g\"\nartifactId = \"a\"\nversion = \"1.0.0\"\nurl = \"http://example.com/a\"\n",
        )
        .unwrap();

        let files = vec![src_dir.join("foo").join("Bar.tnx")];
        let archive_path = dir.join("out.tar.gz");
        let manifest_path = dir.join("tinox.toml");
        build_tar_gz(&archive_path, &src_dir, &files, &[(manifest_path.as_path(), "tinox.toml")]).unwrap();

        let extract_dir = dir.join("extracted");
        let bytes = fs::read(&archive_path).unwrap();
        extract_tar_gz(&bytes, &extract_dir).unwrap();

        assert!(extract_dir.join("foo").join("Bar.tnx").exists());
        let extracted_manifest = read_manifest(&extract_dir).unwrap();
        assert_eq!(extracted_manifest.dependencies.len(), 1);
        assert_eq!(extracted_manifest.dependencies[0].artifact_id, "a");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_registry_envelope_returns_none_for_non_json_bytes() {
        // A real tar.gz/zip response body never starts with '{' as valid
        // UTF-8 text -- must fall through untouched, not be misdetected.
        let gzip_magic: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00];
        assert!(parse_registry_envelope(gzip_magic).is_none());
        assert!(parse_registry_envelope(b"not json at all").is_none());
    }
}

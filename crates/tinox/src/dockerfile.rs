//! Pure Dockerfile / build-context / provenance rendering for `tinox docker`.
//!
//! Everything here is side-effect free apart from `stage_source_context`
//! (plain file copies), so it is unit-testable without Docker. The
//! orchestration (calling `docker`, reading tinox.toml) stays in `main.rs`.

use std::fs;
use std::path::Path;

/// Default UID/GID of the unprivileged user the container runs as. High and
/// fixed so it never collides with a base-image system account and file
/// ownership stays predictable across rebuilds.
pub const DEFAULT_CONTAINER_UID: u32 = 10001;

/// Directory inside the image the unprivileged user may write to. Everything
/// else under `/app` (including the binary) is root-owned and read-only to it.
pub const CONTAINER_DATA_DIR: &str = "/app/data";

/// Pinned builder image for `--source-build`: tagged with the exact compiler
/// version and the Debian release it is built on (see `docker/builder/`), so
/// the toolchain, glibc and native headers are a known set rather than
/// whatever the developer's machine happens to have.
pub fn default_builder_image() -> String {
    format!("ghcr.io/subnix-work/tinox-builder:{}-trixie", env!("CARGO_PKG_VERSION"))
}

/// Escapes `"` and `\` for embedding in a JSON string / Dockerfile LABEL.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The shared tail of both Dockerfile flavours, starting at the runtime
/// `FROM`: runtime packages, an unprivileged user, the binary, EXPOSE and an
/// exec-form ENTRYPOINT. `copy_src` is the `COPY` source (a build-context
/// path, or `--from=build <path>` in the multi-stage flavour).
#[allow(clippy::too_many_arguments)]
fn runtime_stage(
    base: &str,
    packages: &[String],
    binary_name: &str,
    ports: &[u16],
    uid: u32,
    copy_src: &str,
    labels: &[(String, String)],
    extra_copies: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("FROM {base}\n\n"));
    if !packages.is_empty() {
        s.push_str("RUN apt-get update && apt-get install -y --no-install-recommends \\\n");
        for pkg in packages {
            // Every package line continues (there's always a trailing
            // `&& rm -rf ...` line after the loop).
            s.push_str(&format!("    {pkg} \\\n"));
        }
        s.push_str("    && rm -rf /var/lib/apt/lists/*\n\n");
    }
    if uid != 0 {
        s.push_str(&format!(
            "RUN groupadd --system --gid {uid} tinox \\\n    \
             && useradd --system --uid {uid} --gid {uid} --no-create-home --shell /usr/sbin/nologin tinox \\\n    \
             && mkdir -p {CONTAINER_DATA_DIR} && chown {uid}:{uid} {CONTAINER_DATA_DIR}\n\n"
        ));
    }
    s.push_str("WORKDIR /app\n");
    s.push_str(&format!("COPY {copy_src} /app/{binary_name}\n"));
    s.push_str(extra_copies);
    s.push_str(&format!("RUN chmod 0755 /app/{binary_name}\n\n"));
    for (k, v) in labels {
        s.push_str(&format!("LABEL {k}=\"{}\"\n", escape(v)));
    }
    if !labels.is_empty() {
        s.push('\n');
    }
    if !ports.is_empty() {
        let port_list = ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
        s.push_str(&format!("EXPOSE {port_list}\n\n"));
    }
    if uid != 0 {
        s.push_str(&format!("USER {uid}:{uid}\n"));
    }
    s.push_str(&format!("ENTRYPOINT [\"/app/{binary_name}\"]\n"));
    s
}

/// Renders the (minimal, single-stage) host-compiled Dockerfile: install just
/// the runtime shared libraries the binary needs, copy it in, EXPOSE the
/// configured ports, drop to an unprivileged user and run the binary
/// directly (exec form) as the entrypoint.
pub fn generate_dockerfile(base: &str, packages: &[String], binary_name: &str, ports: &[u16], uid: u32) -> String {
    runtime_stage(base, packages, binary_name, ports, uid, binary_name, &[], "")
}

pub struct SourceBuildSpec<'a> {
    pub builder: &'a str,
    pub base: &'a str,
    pub packages: &'a [String],
    pub binary_name: &'a str,
    pub ports: &'a [u16],
    pub uid: u32,
    pub tls: bool,
    pub http3: bool,
    pub has_dependencies: bool,
    pub labels: &'a [(String, String)],
}

/// Renders the two-stage `--source-build` Dockerfile. Stage `build` runs the
/// project's own `tinox install` + `tinox build` inside the builder (so the
/// binary is linked against the builder's glibc/native libs, not the host's);
/// the runtime stage copies only the binary out of it plus `build-info.json`.
/// `tls`/`http3` are forwarded as the same `TINOX_TLS`/`TINOX_HTTP3` env vars
/// a host build reads.
pub fn generate_source_build_dockerfile(spec: &SourceBuildSpec) -> String {
    let mut s = String::new();
    s.push_str(&format!("FROM {} AS build\n", spec.builder));
    s.push_str(&format!("ENV TINOX_TLS={} TINOX_HTTP3={}\n", u8::from(spec.tls), u8::from(spec.http3)));
    s.push_str("WORKDIR /src\n");
    s.push_str("COPY project/ /src/\n");
    // `tinox install` verifies every dependency against tinox.lock when one
    // is in the context, so a locked project builds the pinned inputs.
    let install = if spec.has_dependencies { "tinox install && " } else { "" };
    let install = format!("mkdir -p /out && {install}");
    s.push_str(&format!("RUN {install}tinox build --release -o /out/{}\n\n", spec.binary_name));
    s.push_str(&runtime_stage(
        spec.base,
        spec.packages,
        spec.binary_name,
        spec.ports,
        spec.uid,
        &format!("--from=build /out/{}", spec.binary_name),
        spec.labels,
        "COPY build-info.json /app/build-info.json\n",
    ));
    s
}

/// Copies the allowlisted subset of the project into `dest`: tinox.toml,
/// tinox.lock, src/, a root `application.properties`, the entry file if it
/// lives outside src/, plus `include`. Dotfiles (`.env`, `.git`, `.tinox`)
/// and symlinks are never copied, so secrets and build output stay out of the
/// context. Returns the project-relative files copied, sorted.
pub fn stage_source_context(root: &Path, dest: &Path, entry_rel: &str, include: &[String]) -> Result<Vec<String>, String> {
    fn copy_tree(root: &Path, rel: &Path, dest: &Path, out: &mut Vec<String>) -> Result<(), String> {
        let src = root.join(rel);
        let meta = fs::symlink_metadata(&src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("{} is a symlink -- refusing to put it in the build context", src.display()));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&src).map_err(|e| format!("cannot read {}: {e}", src.display()))? {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
                    continue;
                }
                copy_tree(root, &rel.join(&name), dest, out)?;
            }
        } else {
            let to = dest.join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            fs::copy(&src, &to).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
            out.push(rel.to_string_lossy().into_owned());
        }
        Ok(())
    }

    let mut wanted: Vec<String> = ["tinox.toml", "tinox.lock", "src", "application.properties"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if !wanted.iter().any(|w| entry_rel == w || entry_rel.starts_with(&format!("{w}/"))) {
        wanted.push(entry_rel.to_string());
    }
    for inc in include {
        let p = Path::new(inc);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("[docker] include entry `{inc}` must be a path inside the project"));
        }
        if inc.split('/').any(|seg| seg.starts_with('.')) {
            return Err(format!("[docker] include entry `{inc}` is a dotfile/dot-directory, which is never put in the build context"));
        }
        wanted.push(inc.clone());
    }

    let _ = fs::remove_dir_all(dest);
    fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let mut copied = Vec::new();
    for w in &wanted {
        let rel = Path::new(w);
        if fs::symlink_metadata(root.join(rel)).is_err() {
            if include.contains(w) {
                return Err(format!("[docker] include entry `{w}` does not exist"));
            }
            continue;
        }
        copy_tree(root, rel, dest, &mut copied)?;
    }
    copied.sort();
    copied.dedup();
    Ok(copied)
}

pub struct BuildInfo<'a> {
    pub compiler_version: &'a str,
    pub target: &'a str,
    /// `(role, image reference pinned by digest, or "unresolved")`.
    pub images: &'a [(&'a str, String)],
    pub tls: bool,
    pub http3: bool,
    pub db_driver: Option<&'a str>,
    pub locked: bool,
    pub files: &'a [String],
}

/// Renders the provenance record baked into a `--source-build` image
/// (`/app/build-info.json`) and written next to the Dockerfile.
pub fn render_build_info(b: &BuildInfo) -> String {
    let images: Vec<String> = b
        .images
        .iter()
        .map(|(k, v)| format!("    \"{}\": \"{}\"", escape(k), escape(v)))
        .collect();
    let files: Vec<String> = b.files.iter().map(|f| format!("    \"{}\"", escape(f))).collect();
    let db = b.db_driver.map(|d| format!("\"{}\"", escape(d))).unwrap_or_else(|| "null".to_string());
    format!(
        "{{\n  \"compilerVersion\": \"{}\",\n  \"target\": \"{}\",\n  \"tls\": {},\n  \"http3\": {},\n  \"databaseDriver\": {},\n  \"dependenciesLocked\": {},\n  \"images\": {{\n{}\n  }},\n  \"sourceFiles\": [\n{}\n  ]\n}}\n",
        escape(b.compiler_version),
        escape(b.target),
        b.tls,
        b.http3,
        db,
        b.locked,
        images.join(",\n"),
        files.join(",\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_stage_has_nonroot_user_and_exec_entrypoint() {
        let out = generate_dockerfile("debian:trixie-slim", &pk(&["libgc1"]), "myapp", &[8080], 10001);
        assert!(out.contains("USER 10001:10001\n"), "{out}");
        assert!(out.contains("useradd --system --uid 10001"));
        assert!(out.contains("chown 10001:10001 /app/data"));
        assert!(out.contains("ENTRYPOINT [\"/app/myapp\"]"));
        // USER must come after the root-only setup and before the entrypoint.
        let user = out.find("USER 10001").unwrap();
        assert!(out.find("useradd").unwrap() < user && user < out.find("ENTRYPOINT").unwrap());
    }

    #[test]
    fn uid_zero_is_explicit_root_optin() {
        let out = generate_dockerfile("debian:trixie-slim", &[], "myapp", &[], 0);
        assert!(!out.contains("USER "));
        assert!(!out.contains("useradd"));
    }

    #[test]
    fn source_build_is_two_stage_and_copies_only_binary_and_info() {
        let labels = vec![("tinox.compiler.version".to_string(), "2.4.0".to_string())];
        let packages = pk(&["libgc1", "libssl3"]);
        let out = generate_source_build_dockerfile(&SourceBuildSpec {
            builder: "ghcr.io/x/tinox-builder@sha256:abc",
            base: "debian:trixie-slim@sha256:def",
            packages: &packages,
            binary_name: "app",
            ports: &[8080],
            uid: 10001,
            tls: true,
            http3: false,
            has_dependencies: true,
            labels: &labels,
        });
        assert!(out.starts_with("FROM ghcr.io/x/tinox-builder@sha256:abc AS build\n"));
        assert!(out.contains("ENV TINOX_TLS=1 TINOX_HTTP3=0"));
        assert!(out.contains("COPY project/ /src/"));
        assert!(out.contains("RUN mkdir -p /out && tinox install && tinox build --release -o /out/app"));
        assert!(out.contains("COPY --from=build /out/app /app/app"));
        assert!(out.contains("COPY build-info.json /app/build-info.json"));
        assert!(out.contains("LABEL tinox.compiler.version=\"2.4.0\""));
        assert!(out.contains("FROM debian:trixie-slim@sha256:def\n"));
        assert!(out.contains("USER 10001:10001"));
        assert!(out.contains("EXPOSE 8080"));
    }

    #[test]
    fn source_build_without_deps_skips_install() {
        let out = generate_source_build_dockerfile(&SourceBuildSpec {
            builder: "b", base: "r", packages: &[], binary_name: "app", ports: &[], uid: 1,
            tls: false, http3: false, has_dependencies: false, labels: &[],
        });
        assert!(!out.contains("tinox install"));
        assert!(out.contains("ENV TINOX_TLS=0"));
    }

    #[test]
    fn build_info_is_valid_shape() {
        let images = vec![("builder", "b@sha256:1".to_string()), ("base", "unresolved".to_string())];
        let files = pk(&["src/Main.tnx", "tinox.toml"]);
        let out = render_build_info(&BuildInfo {
            compiler_version: "2.4.0", target: "linux/amd64", images: &images, tls: true, http3: false,
            db_driver: Some("postgres"), locked: true, files: &files,
        });
        assert!(out.contains("\"databaseDriver\": \"postgres\""));
        assert!(out.contains("\"builder\": \"b@sha256:1\""));
        assert!(out.contains("\"dependenciesLocked\": true"));
        assert!(out.contains("\"src/Main.tnx\""));
    }

    #[test]
    fn context_staging_excludes_dotfiles_and_unlisted_dirs() {
        let tmp = std::env::temp_dir().join(format!("tinox_ctx_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("proj");
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join(".tinox/deps")).unwrap();
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::write(root.join("tinox.toml"), "[package]\ncompiler = \"2.6.0\"\nname=\"a\"\n").unwrap();
        fs::write(root.join("src/Main.tnx"), "x").unwrap();
        fs::write(root.join("src/sub/A.tnx"), "x").unwrap();
        fs::write(root.join("src/.env"), "SECRET=1").unwrap();
        fs::write(root.join(".env"), "SECRET=1").unwrap();
        fs::write(root.join("secrets/key.pem"), "k").unwrap();
        let dest = tmp.join("ctx");
        let files = stage_source_context(&root, &dest, "src/Main.tnx", &[]).unwrap();
        assert_eq!(files, vec!["src/Main.tnx", "src/sub/A.tnx", "tinox.toml"]);
        assert!(!dest.join(".env").exists() && !dest.join("secrets").exists());
        assert!(stage_source_context(&root, &dest, "src/Main.tnx", &["../x".to_string()]).is_err());
        assert!(stage_source_context(&root, &dest, "src/Main.tnx", &[".env".to_string()]).is_err());
        assert!(stage_source_context(&root, &dest, "src/Main.tnx", &["nope".to_string()]).is_err());
        let with = stage_source_context(&root, &dest, "src/Main.tnx", &["secrets".to_string()]).unwrap();
        assert!(with.contains(&"secrets/key.pem".to_string()));
        let _ = fs::remove_dir_all(&tmp);
    }
}

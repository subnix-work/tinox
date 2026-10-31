mod actions;
mod analysis;
mod checked;
mod formatting;
mod navigation;
mod semantic;
mod signature;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use analysis::{
    build_registry_from_source, completions_at, definition_at, document_symbols,
    hover_at, lsp_pos_to_offset, TypeRegistry,
};
use dashmap::DashMap;
use tinox_common::diagnostic::{Diagnostic as CompilerDiagnostic, SCHEMA_VERSION};
use tinox_lexer::Lexer;
use tinox_parser::{ast::SourceFile, Parser};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

// Embed all stdlib .tnx files at compile time so the binary works without any
// install path. One-type-per-file (2026-07-26): most modules are now a
// directory of several `<TypeName>.tnx` files instead of one `<name>.tnx` —
// concatenated per module (still valid Tinox syntax: multiple adjacent
// `module`/`namespace` blocks, same shape the original unsplit file had with
// multiple classes in one `namespace` block).
const EMBEDDED_STDLIB: &[(&str, &str)] = &[
    ("array",         include_str!("../../tinox-core/tinox/core/array/Arrays.tnx")),
    ("collections",   concat!(
        include_str!("../../tinox-core/tinox/core/collections/Collections.tnx"),
        include_str!("../../tinox-core/tinox/core/collections/Pair.tnx"),
        include_str!("../../tinox-core/tinox/core/collections/Queue.tnx"),
        include_str!("../../tinox-core/tinox/core/collections/Stack.tnx"),
    )),
    ("db",            concat!(
        include_str!("../../tinox-core-ext/db/tinox/core/db/DB.tnx"),
        include_str!("../../tinox-core-ext/db/tinox/core/db/EntityQuery.tnx"),
    )),
    ("env",           include_str!("../../tinox-core/tinox/core/env/Env.tnx")),
    ("fmt",           include_str!("../../tinox-core/tinox/core/fmt/Fmt.tnx")),
    ("fs",            concat!(
        include_str!("../../tinox-core/tinox/core/fs/Fs.tnx"),
        include_str!("../../tinox-core/tinox/core/fs/WatchEvent.tnx"),
        include_str!("../../tinox-core/tinox/core/fs/Watcher.tnx"),
    )),
    ("hash",          include_str!("../../tinox-core/tinox/core/hash/Hash.tnx")),
    ("http",          concat!(
        include_str!("../../tinox-core-ext/http/tinox/core/http/Http.tnx"),
        include_str!("../../tinox-core-ext/http/tinox/core/http/HttpClientResponse.tnx"),
    )),
    ("http_server",   concat!(
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/HttpContext.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/HttpRequest.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/HttpResponse.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/HttpServer.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/MediaType.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/QueryString.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/Route.tnx"),
        include_str!("../../tinox-core-ext/http_server/tinox/core/http_server/RouteMatcher.tnx"),
    )),
    ("io",            concat!(
        include_str!("../../tinox-core/tinox/core/io/Buffer.tnx"),
        include_str!("../../tinox-core/tinox/core/io/File.tnx"),
        include_str!("../../tinox-core/tinox/core/io/Io.tnx"),
        include_str!("../../tinox-core/tinox/core/io/Paths.tnx"),
    )),
    ("iter",          concat!(
        include_str!("../../tinox-core/tinox/core/iter/Iter.tnx"),
        include_str!("../../tinox-core/tinox/core/iter/Iterator.tnx"),
    )),
    ("json",          concat!(
        include_str!("../../tinox-core-ext/json/tinox/core/json/Json.tnx"),
        include_str!("../../tinox-core-ext/json/tinox/core/json/JsonField.tnx"),
        include_str!("../../tinox-core-ext/json/tinox/core/json/JsonSerializable.tnx"),
        include_str!("../../tinox-core-ext/json/tinox/core/json/JsonValue.tnx"),
    )),
    ("logger",        concat!(
        include_str!("../../tinox-core/tinox/core/logger/LogLevel.tnx"),
        include_str!("../../tinox-core/tinox/core/logger/Logger.tnx"),
    )),
    ("math",          include_str!("../../tinox-core/tinox/core/math/Math.tnx")),
    ("mathf",         include_str!("../../tinox-core/tinox/core/mathf/Mathf.tnx")),
    ("option",        include_str!("../../tinox-core/tinox/core/option/Option.tnx")),
    ("process",       concat!(
        include_str!("../../tinox-core/tinox/core/process/InteractiveProcess.tnx"),
        include_str!("../../tinox-core/tinox/core/process/Process.tnx"),
        include_str!("../../tinox-core/tinox/core/process/ProcessResult.tnx"),
    )),
    ("random",        include_str!("../../tinox-core/tinox/core/random/Random.tnx")),
    ("regex",         include_str!("../../tinox-core/tinox/core/regex/Regex.tnx")),
    // Keyed "client", not "rest.client": module_names_from_imports() below
    // only ever takes the import path's last segment (a pre-existing
    // simplification in this LSP, separate from and not as general as
    // tinox's own resolve_imports() in crates/tinox/src/main.rs, which
    // does understand tinox.core.X.Y nesting) — for `import
    // tinox.core.rest.client;` that lookup key is "client", so this entry
    // has to match it to actually be found.
    ("client",        concat!(
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/HttpStatus.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/HttpStatusHelper.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/MediaType.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/MediaTypeHelper.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/RequestBuilder.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/RestClient.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/RestResponse.tnx"),
        include_str!("../../tinox-core-ext/rest/tinox/core/rest/client/Url.tnx"),
    )),
    ("result",        include_str!("../../tinox-core/tinox/core/result/Result.tnx")),
    ("set",           include_str!("../../tinox-core/tinox/core/set/Set.tnx")),
    ("socket",        include_str!("../../tinox-core/tinox/core/socket/Socket.tnx")),
    ("sort",          include_str!("../../tinox-core/tinox/core/sort/Sort.tnx")),
    ("string",        include_str!("../../tinox-core/tinox/core/string/Strings.tnx")),
    ("time",          concat!(
        include_str!("../../tinox-core/tinox/core/time/Duration.tnx"),
        include_str!("../../tinox-core/tinox/core/time/Stopwatch.tnx"),
        include_str!("../../tinox-core/tinox/core/time/Time.tnx"),
        include_str!("../../tinox-core/tinox/core/time/Timer.tnx"),
    )),
    ("uuid",          include_str!("../../tinox-core/tinox/core/uuid/Uuid.tnx")),
];

fn build_embedded_stdlib() -> TypeRegistry {
    let mut registry = TypeRegistry::new();
    for (_, src) in EMBEDDED_STDLIB {
        let Ok(tokens) = Lexer::new(src).tokenize() else { continue };
        let Ok(ast) = Parser::new(tokens).parse() else { continue };
        for (name, info) in build_registry_from_source(&ast) {
            registry.entry(name).or_insert(info);
        }
    }
    registry
}

fn load_embedded_module(module: &str, registry: &mut TypeRegistry) {
    let Some((_, src)) = EMBEDDED_STDLIB.iter().find(|(name, _)| *name == module) else { return };
    let Ok(tokens) = Lexer::new(src).tokenize() else { return };
    let Ok(ast) = Parser::new(tokens).parse() else { return };
    for (name, info) in build_registry_from_source(&ast) {
        registry.entry(name).or_insert(info);
    }
}

/// Quiet period after the last keystroke before a document is re-analysed.
const DEBOUNCE_MS: u64 = 120;

struct Inner {
    client: Client,
    docs: DashMap<Url, String>,
    asts: DashMap<Url, SourceFile>,
    stdlib_registry: TypeRegistry,
    stdlib_asts: HashMap<String, SourceFile>,
    /// Member names of every standard-library class (rename safety check).
    stdlib_member_names: std::collections::HashSet<String>,
    /// Whether the client can apply `RenameFile` workspace-edit operations.
    client_renames_files: std::sync::atomic::AtomicBool,
    /// Last semantic-token answer per document, for `full/delta`.
    semantic_cache: DashMap<Url, (String, Vec<SemanticToken>)>,
    semantic_counter: std::sync::atomic::AtomicU64,
    /// Bumped on every change of a document; superseded analyses drop their result.
    generations: DashMap<Url, u64>,
    /// Latest client version seen per document (out-of-order changes are ignored).
    doc_versions: DashMap<Url, i32>,
    /// (import path, class names) for every embedded standard-library module.
    stdlib_import_map: Vec<(String, Vec<String>)>,
    /// Issue #555: resource budget of the project scan, a misconfiguration message to report once,
    /// the disk-read cache (path -> modified time, length, text) and the last partial-index warning.
    budget: ScanBudget,
    budget_problem: Option<String>,
    disk_cache: DashMap<PathBuf, (std::time::SystemTime, u64, String)>,
    partial_warned: std::sync::Mutex<Option<usize>>,
}

/// Limits of one project scan (issue #555). `TINOX_LSP_MAX_FILES` / `TINOX_LSP_MAX_BYTES` override
/// the defaults; the open documents are always indexed, whatever the budget.
#[derive(Clone, Copy)]
struct ScanBudget {
    max_files: usize,
    max_bytes: u64,
}

impl ScanBudget {
    const DEFAULT_FILES: usize = 5000;
    const DEFAULT_BYTES: u64 = 256 * 1024 * 1024;

    /// Reads the overrides; an unusable value keeps the default and is reported, never ignored silently.
    fn from_env() -> (ScanBudget, Option<String>) {
        let mut budget = ScanBudget { max_files: Self::DEFAULT_FILES, max_bytes: Self::DEFAULT_BYTES };
        let mut problems = Vec::new();
        if let Ok(v) = std::env::var("TINOX_LSP_MAX_FILES") {
            match v.parse::<usize>() {
                Ok(n) if n > 0 => budget.max_files = n,
                _ => problems.push(format!("TINOX_LSP_MAX_FILES must be a positive integer (keeping {})", Self::DEFAULT_FILES)),
            }
        }
        if let Ok(v) = std::env::var("TINOX_LSP_MAX_BYTES") {
            match v.parse::<u64>() {
                Ok(n) if n > 0 => budget.max_bytes = n,
                _ => problems.push(format!("TINOX_LSP_MAX_BYTES must be a positive integer (keeping {})", Self::DEFAULT_BYTES)),
            }
        }
        (budget, if problems.is_empty() { None } else { Some(problems.join("; ")) })
    }
}

/// The files of one project scan and what the budget left out (issue #555). Operations that need
/// every file to be correct (rename) must check `complete()`.
struct ProjectScan {
    files: Vec<(Url, String)>,
    /// `.tnx` files found on disk that were not indexed because of the budget
    skipped: usize,
    total: usize,
    budget: ScanBudget,
}

impl ProjectScan {
    fn complete(&self) -> bool {
        self.skipped == 0
    }

    fn describe(&self) -> String {
        format!(
            "workspace index incomplete: {} of {} .tnx files are not indexed (budget {} files / {} bytes; raise TINOX_LSP_MAX_FILES or TINOX_LSP_MAX_BYTES)",
            self.skipped, self.total, self.budget.max_files, self.budget.max_bytes
        )
    }
}

impl Inner {
    fn new(client: Client) -> Self {
        // Start with embedded stdlib; optionally supplement from filesystem if TINOX_STDLIB is set.
        let mut stdlib_registry = build_embedded_stdlib();
        if let Some(path) = find_stdlib_path() {
            for (name, info) in load_stdlib(&path) {
                stdlib_registry.entry(name).or_insert(info);
            }
        }
        // Parse embedded stdlib into ASTs so typecheck_with_prelude can resolve imports.
        let mut stdlib_asts = HashMap::new();
        for (name, src) in EMBEDDED_STDLIB {
            if let Ok(tokens) = Lexer::new(src).tokenize() {
                if let Ok(ast) = Parser::new(tokens).parse() {
                    stdlib_asts.insert(name.to_string(), ast);
                }
            }
        }
        let stdlib_member_names = stdlib_registry
            .values()
            .flat_map(|c| c.fields.iter().map(|f| f.name.clone()).chain(c.methods.iter().map(|m| m.name.clone())))
            .collect();
        let mut stdlib_import_map: Vec<(String, Vec<String>)> = stdlib_asts
            .iter()
            .map(|(key, ast)| {
                // the embedded key is the import path's last segment
                let path = if key == "client" { "tinox.core.rest.client".to_string() } else { format!("tinox.core.{key}") };
                let mut classes: Vec<String> = build_registry_from_source(ast).into_keys().collect();
                classes.sort();
                (path, classes)
            })
            .collect();
        stdlib_import_map.sort();
        let (budget, budget_problem) = ScanBudget::from_env();
        Self {
            client,
            docs: DashMap::new(),
            asts: DashMap::new(),
            stdlib_registry,
            stdlib_asts,
            stdlib_member_names,
            client_renames_files: std::sync::atomic::AtomicBool::new(false),
            semantic_cache: DashMap::new(),
            semantic_counter: std::sync::atomic::AtomicU64::new(0),
            stdlib_import_map,
            generations: DashMap::new(),
            doc_versions: DashMap::new(),
            budget,
            budget_problem,
            disk_cache: DashMap::new(),
            partial_warned: std::sync::Mutex::new(None),
        }
    }

    /// Fresh semantic tokens for `uri`, remembered under a new result id.
    fn semantic_for(&self, uri: &Url) -> Option<(String, Vec<SemanticToken>)> {
        let text = self.docs.get(uri)?.clone();
        let types = self.stdlib_registry.keys().cloned().collect();
        let tokens = semantic::encode(&semantic::compute(&text, uri, &types)?);
        let id = self.semantic_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string();
        self.semantic_cache.insert(uri.clone(), (id.clone(), tokens.clone()));
        Some((id, tokens))
    }

    /// Standard-library classes plus the project's own, and the project's
    /// top-level functions (name, signature, doc).
    ///
    /// `cursor` is the edit position in `uri`: when that buffer does not parse (an
    /// unfinished call), its declarations are recovered by blanking the cursor
    /// line, exactly as completion does, instead of dropping the whole file.
    fn project_signatures(&self, uri: &Url, cursor: Option<usize>) -> (TypeRegistry, Vec<(String, String, Option<String>)>) {
        let mut registry = self.stdlib_registry.clone();
        let mut functions = Vec::new();
        for (file_uri, text) in self.project_files(uri) {
            let ast = match tinox_parser::parse_source_cached(&text) {
                Ok(ast) => ast,
                Err(_) => match cursor.filter(|_| &file_uri == uri).and_then(|c| parse_for_completion(&text, c as u32)) {
                    Some(ast) => ast,
                    None => continue,
                },
            };
            registry.extend(build_registry_from_source(&ast));
            fn top_level(decls: &[tinox_parser::ast::Decl], out: &mut Vec<(String, String, Option<String>)>) {
                for d in decls {
                    match &d.node {
                        tinox_parser::ast::DeclKind::Function(f) => out.push((f.name.clone(), analysis::fn_signature(f), f.doc.clone())),
                        tinox_parser::ast::DeclKind::Namespace(ns) => top_level(&ns.decls, out),
                        _ => {}
                    }
                }
            }
            top_level(&ast.decls, &mut functions);
        }
        (registry, functions)
    }

    fn overlay(&self) -> impl Fn(&Path) -> Option<String> + '_ {
        move |path: &Path| Url::from_file_path(path).ok().and_then(|u| self.docs.get(&u).map(|t| t.clone()))
    }

    fn bump(&self, uri: &Url) -> u64 {
        let mut g = self.generations.entry(uri.clone()).or_insert(0);
        *g += 1;
        *g
    }

    fn generation(&self, uri: &Url) -> u64 {
        self.generations.get(uri).map(|g| *g).unwrap_or(0)
    }

    /// Re-analyses the other open documents that directly import `changed`,
    /// so a fix (or a break) in one file shows up in its users at once.
    async fn refresh_dependents(&self, changed: &Url) {
        let Ok(changed_path) = changed.to_file_path() else { return };
        let changed_canon = changed_path.canonicalize().unwrap_or(changed_path);
        let open: Vec<(Url, String)> = self.docs.iter().filter(|e| e.key() != changed).map(|e| (e.key().clone(), e.value().clone())).collect();
        for (uri, text) in open {
            let Ok(path) = uri.to_file_path() else { continue };
            let Ok(ast) = tinox_parser::parse_source_cached(&text) else { continue };
            let imports_changed = import_paths(&ast).iter().any(|ip| {
                resolve_project_local_import(ip, &path).and_then(|p| p.canonicalize().ok()).is_some_and(|p| p == changed_canon)
            });
            if imports_changed {
                let generation = self.bump(&uri);
                self.update(uri, text, None, generation).await;
            }
        }
    }

    fn nav_ctx(&self) -> navigation::Ctx {
        navigation::Ctx { stdlib_member_names: self.stdlib_member_names.clone() }
    }

    /// Every `.tnx` file of the project containing `uri` (nearest
    /// `tinox.toml` upwards; just the file's directory tree otherwise),
    /// with open documents overriding what is on disk.
    fn project_files(&self, uri: &Url) -> Vec<(Url, String)> {
        self.project_scan(uri).files
    }

    /// Like `project_files`, but also reports what the resource budget left out. The walk only
    /// lists paths; file contents are read lazily and an unchanged file (same modified time and
    /// length) is served from `disk_cache`, so repeated requests and edits do not re-read the
    /// workspace. Open documents are always included first; the rest follows in path order, so
    /// the set that fits the budget is stable between scans.
    fn project_scan(&self, uri: &Url) -> ProjectScan {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(path) = uri.to_file_path() {
            let dir = path.parent().unwrap_or(Path::new("."));
            let root = find_project_root(dir).unwrap_or_else(|| dir.to_path_buf());
            let mut stack = vec![root];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for entry in rd.flatten() {
                    let p = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Ok(ft) = entry.file_type() else { continue };
                    if ft.is_dir() {
                        if !name.starts_with('.') && name != "target" && name != "node_modules" {
                            stack.push(p);
                        }
                    } else if ft.is_file() && p.extension().is_some_and(|e| e == "tnx") {
                        paths.push(p);
                    }
                }
            }
        }
        paths.sort();
        let total = paths.len();
        let mut out: Vec<(Url, String)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut bytes: u64 = 0;
        // open documents first: they are what the user is editing and must never be dropped
        for p in &paths {
            if let Ok(u) = Url::from_file_path(p) {
                if let Some(t) = self.docs.get(&u) {
                    bytes += t.len() as u64;
                    seen.insert(u.clone());
                    out.push((u, t.clone()));
                }
            }
        }
        let mut skipped = 0;
        for p in &paths {
            let Ok(u) = Url::from_file_path(p) else { continue };
            if seen.contains(&u) {
                continue;
            }
            let meta = std::fs::metadata(p).ok();
            let len = meta.as_ref().map_or(0, |m| m.len());
            if out.len() >= self.budget.max_files || bytes + len > self.budget.max_bytes {
                skipped += 1;
                continue;
            }
            let modified = meta.and_then(|m| m.modified().ok());
            let cached = modified.and_then(|mt| self.disk_cache.get(p).filter(|c| c.0 == mt && c.1 == len).map(|c| c.2.clone()));
            let text = match cached {
                Some(t) => t,
                None => match std::fs::read_to_string(p) {
                    Ok(t) => {
                        if let Some(mt) = modified {
                            self.disk_cache.insert(p.clone(), (mt, len, t.clone()));
                        }
                        t
                    }
                    Err(_) => continue,
                },
            };
            bytes += len;
            seen.insert(u.clone());
            out.push((u, text));
        }
        // open documents that are not on disk (untitled) or outside the tree
        for entry in self.docs.iter() {
            if !seen.contains(entry.key()) && (entry.key() == uri || entry.key().scheme() == "file") {
                out.push((entry.key().clone(), entry.value().clone()));
            }
        }
        ProjectScan { files: out, skipped, total, budget: self.budget }
    }

    /// Tells the user (once per distinct skipped count) that results come from a partial index.
    async fn warn_if_partial(&self, scan: &ProjectScan) {
        if scan.complete() {
            *self.partial_warned.lock().unwrap() = None;
            return;
        }
        let first = {
            let mut last = self.partial_warned.lock().unwrap();
            let first = *last != Some(scan.skipped);
            *last = Some(scan.skipped);
            first
        };
        if first {
            self.client.show_message(MessageType::WARNING, format!("Tinox: {}; results may miss symbols in the skipped files.", scan.describe())).await;
        }
    }
}

/// The language server: shared state behind an `Arc` so debounced analyses
/// can run as background tasks instead of occupying a request-handler slot.
struct Backend(std::sync::Arc<Inner>);

impl std::ops::Deref for Backend {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Backend {
    fn new(client: Client) -> Self {
        Backend(std::sync::Arc::new(Inner::new(client)))
    }
}

fn find_stdlib_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TINOX_STDLIB") {
        let path = PathBuf::from(p);
        if path.exists() { return Some(path); }
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        let candidates = [
            exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()).map(|p| p.join("crates/tinox-core")),
            exe.parent().map(|p| p.join("tinox-core")),
            exe.parent().map(|p| p.join("core")),
            exe.parent().map(|p| p.join("stdlib")),
        ];
        for candidate in candidates.into_iter().flatten() {
            if candidate.exists() { return Some(candidate); }
        }
    }
    None
}

/// Extracts module file names from import declarations, e.g.
/// `import tinox.core.http_server` → `"http_server"`
fn module_names_from_imports(source: &tinox_parser::ast::SourceFile) -> Vec<String> {
    use tinox_parser::ast::DeclKind;
    source.decls.iter().filter_map(|d| {
        if let DeclKind::Import(imp) = &d.node {
            imp.path.last().cloned()
        } else {
            None
        }
    }).collect()
}

/// Every `import` statement's full dotted path, e.g. `import demo.dao.
/// PersonDao;` → `["demo", "dao", "PersonDao"]`. Unlike
/// `module_names_from_imports` (last segment only, stdlib lookup key), a
/// PROJECT-LOCAL import needs the full path to resolve a real file on
/// disk.
fn import_paths(source: &tinox_parser::ast::SourceFile) -> Vec<Vec<String>> {
    use tinox_parser::ast::DeclKind;
    source.decls.iter().filter_map(|d| {
        if let DeclKind::Import(imp) = &d.node {
            Some(imp.path.clone())
        } else {
            None
        }
    }).collect()
}

/// Walks up from `start` looking for the nearest `tinox.toml` — same
/// "nearest ancestor manifest" convention `tinox`'s own
/// `pm::find_project_root_from` uses.
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join("tinox.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// `["demo", "dao", "PersonDao"]` + a base directory → `<base>/demo/dao/
/// PersonDao.tnx`, if that file actually exists. Directory-module imports
/// (an import whose last segment names a directory of several `.tnx`
/// files, not a single file — see `tinox`'s own `resolve_module_paths`)
/// aren't resolved here; this LSP only ever needs ONE type's own
/// declaration to typecheck against, and the common project-local case
/// (one class per file, issue #185) is a single file anyway.
fn try_resolve_tnx_file(base: &Path, path: &[String]) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    for seg in path {
        rel.push(seg);
    }
    rel.set_extension("tnx");
    let candidate = base.join(rel);
    candidate.is_file().then_some(candidate)
}

/// Resolves a project-local import to a real file on disk — a smaller
/// mirror of `tinox`'s own `resolve_import_target`'s "Local" branch
/// (relative to the importing file's own directory), plus the project-root
/// fallback (`src/`, `tests/`, or the manifest dir itself) added for issue
/// #202: a full dotted path written outside the entry file otherwise has
/// no valid relative-to-self resolution once it names a DIFFERENT
/// namespace-mirrored directory. Never touches `tinox.core.*` (stdlib) —
/// that's `self.stdlib_asts`' job, kept entirely separate as before.
fn resolve_project_local_import(path: &[String], file_path: &Path) -> Option<PathBuf> {
    if path.first().map(|s| s == "tinox").unwrap_or(false) {
        return None;
    }
    let base_dir = file_path.parent()?;
    if let Some(p) = try_resolve_tnx_file(base_dir, path) {
        return Some(p);
    }
    let root = find_project_root(base_dir)?;
    for candidate_root in [root.join("src"), root.join("tests"), root.clone()] {
        if candidate_root == base_dir {
            continue;
        }
        if let Some(p) = try_resolve_tnx_file(&candidate_root, path) {
            return Some(p);
        }
    }
    None
}

/// Unsaved editor buffers: returns the open document's text for a path, if any,
/// so imported files are analysed as the user sees them, not as saved.
type Overlay<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// Recursively resolves `source`'s own project-local imports (and THEIR
/// project-local imports, and so on) into parsed ASTs — the project-local
/// counterpart to `self.stdlib_asts`, built fresh per `update()` call
/// rather than cached, since project files can change on disk between
/// edits and this LSP has no file-watcher wired up to invalidate a cache.
/// `visited` guards against an import cycle (two files importing each
/// other) looping forever — same purpose as `tinox`'s own `resolve_imports`
/// `visited: &mut HashSet<PathBuf>` parameter.
fn load_project_local_preludes(source: &tinox_parser::ast::SourceFile, file_path: &Path, overlay: Overlay) -> Vec<SourceFile> {
    let mut out = Vec::new();
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Ok(canon) = file_path.canonicalize() {
        visited.insert(canon);
    }
    let mut queue: Vec<(Vec<String>, PathBuf)> = import_paths(source)
        .into_iter()
        .map(|p| (p, file_path.to_path_buf()))
        .collect();
    while let Some((path, importing_from)) = queue.pop() {
        let Some(target) = resolve_project_local_import(&path, &importing_from) else { continue };
        let Ok(canon) = target.canonicalize() else { continue };
        if !visited.insert(canon) {
            continue;
        }
        let Some(src) = overlay(&target).or_else(|| std::fs::read_to_string(&target).ok()) else { continue };
        let Ok(ast) = tinox_parser::parse_source_cached(&src) else { continue };
        for p in import_paths(&ast) {
            queue.push((p, target.clone()));
        }
        out.push(ast);
    }
    out
}


/// Load package imports from the exact graph used by the CLI. Local sources
/// are parsed fresh on every update, so unpublished edits are immediately used.
fn load_package_preludes(source: &SourceFile, file: &Path, dirs: &[PathBuf], overlay: Overlay) -> std::result::Result<Vec<SourceFile>, String> {
    let mut out = Vec::new();
    let mut visited = std::collections::HashSet::new();
    visited.insert(file.canonicalize().unwrap_or_else(|_| file.to_path_buf()));
    let mut queue: Vec<_> = import_paths(source).into_iter().map(|path| (path, file.to_path_buf())).collect();
    while let Some((path, from)) = queue.pop() {
        let relative: PathBuf = path.iter().collect();
        let mut targets = Vec::new();
        // Short sibling imports within a selected package keep working.
        let sibling = dirs.iter().any(|dir| dir.canonicalize().is_ok_and(|dir| from.starts_with(dir)))
            .then(|| from.parent().unwrap_or(Path::new(".")));
        let bases = sibling.into_iter().chain(dirs.iter().map(PathBuf::as_path));
        for base in bases {
            let module = base.join(&relative);
            let single = module.with_extension("tnx");
            let mut found = if single.is_file() {
                vec![single]
            } else if module.is_dir() {
                std::fs::read_dir(&module).map_err(|e| e.to_string())?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "tnx"))
                    .collect()
            } else { Vec::new() };
            found.sort();
            if !found.is_empty() {
                if !targets.is_empty() && targets != found {
                    return Err(format!("Ambiguous package import: {}", path.join(".")));
                }
                targets = found;
            }
        }
        for target in targets {
            let target = target.canonicalize().map_err(|e| e.to_string())?;
            if !visited.insert(target.clone()) { continue; }
            let text = match overlay(&target) {
                Some(text) => text,
                None => std::fs::read_to_string(&target).map_err(|e| e.to_string())?,
            };
            let ast = tinox_parser::parse_source_cached(&text).map_err(|e| format!("{}: {e:?}", target.display()))?;
            queue.extend(import_paths(&ast).into_iter().map(|path| (path, target.clone())));
            // A type import also brings in its own namespace siblings, matching
            // the compiler's implicit same-namespace visibility.
            for declaration in &ast.decls {
                if let tinox_parser::ast::DeclKind::Namespace(namespace) = &declaration.node {
                    queue.push((namespace.name.clone(), target.clone()));
                }
            }
            out.push(ast);
        }
    }
    Ok(out)
}

fn package_context(file: &Path) -> std::result::Result<(Vec<PathBuf>, Vec<String>), String> {
    let Some(root) = find_project_root(file.parent().unwrap_or(Path::new("."))) else {
        return Ok(Default::default());
    };
    let manifest = tinox::pm::read_manifest(&root)?;
    let (dirs, _) = tinox::pm::dependency_dirs(&root, &manifest)?;
    let provenance = tinox::pm::source_provenance(&root, &dirs)?;
    Ok((dirs, provenance))
}

fn package_diagnostic(message: String, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        range: Range::default(),
        severity: Some(severity),
        code: Some(NumberOrString::String("TINOX-DEPENDENCY-SOURCE".into())),
        source: Some("tinox".into()),
        message,
        ..Default::default()
    }
}



fn load_stdlib(path: &Path) -> TypeRegistry {
    let mut registry: TypeRegistry = HashMap::new();
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return registry,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "tnx").unwrap_or(false) {
            let src = match std::fs::read_to_string(&p) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let tokens = match Lexer::new(&src).tokenize() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let ast = match Parser::new(tokens).parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            for (name, info) in build_registry_from_source(&ast) {
                registry.insert(name, info);
            }
        }
    }
    registry
}

impl Inner {
    async fn update(&self, uri: Url, text: String, version: Option<i32>, generation: u64) {
        // Quick pre-parse to extract import names for stdlib prelude resolution.
        let parsed = tinox_parser::parse_source_cached(&text).ok();
        let import_names = parsed.as_ref().map(module_names_from_imports).unwrap_or_default();
        let (package_dirs, provenance) = match uri.to_file_path().ok().map(|file| package_context(&file)).transpose() {
            Ok(context) => context.unwrap_or_default(),
            Err(message) => {
                self.client.publish_diagnostics(uri, vec![package_diagnostic(message, DiagnosticSeverity::ERROR)], None).await;
                return;
            }
        };
        let packages = match (&parsed, uri.to_file_path()) {
            (Some(ast), Ok(file)) => load_package_preludes(ast, &file, &package_dirs, &self.overlay()),
            _ => Ok(Vec::new()),
        };
        let packages = match packages {
            Ok(packages) => packages,
            Err(message) => {
                self.client.publish_diagnostics(uri, vec![package_diagnostic(message, DiagnosticSeverity::ERROR)], None).await;
                return;
            }
        };
        let mut import_names = import_names;
        let mut resolved_names = Vec::new();
        for ast in parsed.iter().chain(packages.iter()) {
            for path in import_paths(ast) {
                let relative: PathBuf = path.iter().collect();
                if package_dirs.iter().any(|dir| dir.join(&relative).is_dir() || dir.join(&relative).with_extension("tnx").is_file()) {
                    if let Some(name) = path.last() { resolved_names.push(name.clone()); }
                } else if let Some(name) = path.last() {
                    if !import_names.contains(name) { import_names.push(name.clone()); }
                }
            }
        }
        let mut preludes: Vec<&SourceFile> = import_names
            .iter()
            .filter(|name| !resolved_names.contains(name))
            .filter_map(|name| self.stdlib_asts.get(name.as_str()))
            .collect();
        preludes.extend(packages.iter());

        // Project-local imports (anything not `tinox.core.*`) aren't in
        // stdlib_asts at all -- resolve them from disk too, or every
        // cross-file project-local reference (the overwhelmingly common
        // case once issue #194 made explicit imports mandatory) shows up
        // as a spurious "undefined variable"/"undefined function"/
        // "expected X, found Y" here even though `tinox build` resolves
        // it fine. Parsed fresh per call (not cached) and owned by this
        // stack frame so `preludes`' borrows stay valid for the `compile`
        // call below.
        let project_local: Vec<SourceFile> = match (&parsed, uri.to_file_path()) {
            (Some(ast), Ok(file_path)) => load_project_local_preludes(ast, &file_path, &self.overlay()),
            _ => Vec::new(),
        };
        preludes.extend(project_local.iter());

        let mut diags = compile(&text, uri.to_file_path().ok().as_deref().and_then(Path::to_str), &preludes, |ast| {
            self.asts.insert(uri.clone(), ast);
        });
        diags.extend(provenance.into_iter().map(|message| package_diagnostic(message, DiagnosticSeverity::INFORMATION)));
        self.docs.insert(uri.clone(), text);
        // A newer edit arrived while this analysis ran: its result is stale.
        if self.generation(&uri) != generation {
            return;
        }
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}

/// Compiler span (columns count Unicode scalar values) to an LSP range
/// (columns count UTF-16 units) over `source`.
fn lsp_range(span: tinox_common::Span, source: &str) -> Range {
    let position = |pos: tinox_common::Pos| Position {
        line: pos.line.saturating_sub(1),
        character: source.lines().nth(pos.line.saturating_sub(1) as usize)
            .unwrap_or("").chars().take(pos.column.saturating_sub(1) as usize)
            .map(|c| c.len_utf16() as u32).sum(),
    };
    Range { start: position(span.start), end: position(span.end) }
}

fn err_to_diag(diagnostic: CompilerDiagnostic, source: &str) -> Diagnostic {
    let range = diagnostic.range.unwrap_or_else(tinox_common::Span::dummy);
    // Secondary locations (e.g. the declaration a mismatch refers to). Their
    // text is only needed for UTF-16 columns: the open buffer when it is the
    // same file, otherwise the saved file.
    let related: Vec<DiagnosticRelatedInformation> = diagnostic
        .related
        .iter()
        .filter_map(|r| {
            let uri = Url::from_file_path(&r.file).ok()?;
            let text = if diagnostic.file.as_deref() == Some(r.file.as_str()) {
                source.to_string()
            } else {
                std::fs::read_to_string(&r.file).unwrap_or_default()
            };
            Some(DiagnosticRelatedInformation { location: Location { uri, range: lsp_range(r.range, &text) }, message: r.message.clone() })
        })
        .collect();
    Diagnostic {
        range: lsp_range(range, source),
        severity: Some(match diagnostic.severity.as_str() {
            "warning" => DiagnosticSeverity::WARNING,
            "info" | "information" => DiagnosticSeverity::INFORMATION,
            "hint" => DiagnosticSeverity::HINT,
            _ => DiagnosticSeverity::ERROR,
        }),
        code: Some(NumberOrString::String(diagnostic.code.clone())),
        message: diagnostic.message.clone(),
        source: Some("tinox".into()),
        related_information: (!related.is_empty()).then_some(related),
        data: Some(serde_json::json!({ "schema_version": SCHEMA_VERSION, "diagnostic": diagnostic })),
        ..Default::default()
    }
}

// Lex → parse → typecheck, using the same records and codes as the CLI.
fn compile(src: &str, file: Option<&str>, preludes: &[&SourceFile], on_ast: impl FnOnce(SourceFile)) -> Vec<Diagnostic> {
    let ast = match tinox_parser::parse_source_cached(src) {
        Ok(ast) => ast,
        Err(error) => return error.into_diagnostics(file).0.into_iter()
            .map(|diagnostic| err_to_diag(diagnostic, src)).collect(),
    };
    let diags = match tinox_typecheck::cache::check_with_prelude_cached(&ast, preludes, file.unwrap_or("<lsp-buffer>")) {
        Ok(_) => vec![],
        Err(bag) => bag.errors.into_iter().map(|error| {
            err_to_diag(CompilerDiagnostic::from_error(error, "type", file), src)
        }).collect(),
    };
    on_ast(ast);
    diags
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let renames = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.workspace_edit.as_ref())
            .and_then(|e| e.resource_operations.as_ref())
            .is_some_and(|ops| ops.contains(&ResourceOperationKind::Rename));
        self.client_renames_files.store(renames, std::sync::atomic::Ordering::Relaxed);
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(true),
                        })),
                        ..Default::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), " ".into()]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: Some(vec![")".into()]),
                    work_done_progress_options: Default::default(),
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                    code_action_kinds: Some(vec![CodeActionKind::QUICKFIX, CodeActionKind::SOURCE_ORGANIZE_IMPORTS]),
                    work_done_progress_options: Default::default(),
                    resolve_provider: Some(false),
                })),
                semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    work_done_progress_options: Default::default(),
                    legend: semantic::legend(),
                    range: Some(true),
                    full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                })),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "tinox-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "tinox-lsp ready")
            .await;
        if let Some(problem) = &self.budget_problem {
            self.client.show_message(MessageType::WARNING, format!("Tinox: invalid workspace budget setting: {problem}")).await;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        let uri = p.text_document.uri;
        self.doc_versions.insert(uri.clone(), p.text_document.version);
        self.docs.insert(uri.clone(), p.text_document.text.clone());
        let generation = self.bump(&uri);
        self.update(uri.clone(), p.text_document.text, Some(p.text_document.version), generation).await;
        self.refresh_dependents(&uri).await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        let uri = p.text_document.uri;
        let version = p.text_document.version;
        let Some(change) = p.content_changes.into_iter().last() else { return };
        if self.doc_versions.get(&uri).is_some_and(|v| *v > version) {
            return; // an older change delivered late
        }
        self.doc_versions.insert(uri.clone(), version);
        // Other features see the new text at once; only the analysis waits.
        self.docs.insert(uri.clone(), change.text.clone());
        let generation = self.bump(&uri);
        // Analyse in the background after a quiet period: the handler returns
        // at once, so a burst of edits never queues behind the server's
        // request-concurrency limit.
        let inner = std::sync::Arc::clone(&self.0);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(DEBOUNCE_MS)).await;
            if inner.generation(&uri) != generation {
                return; // superseded by a later keystroke
            }
            inner.update(uri.clone(), change.text, Some(version), generation).await;
            inner.refresh_dependents(&uri).await;
        });
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        let uri = p.text_document.uri;
        let text = p.text
            .or_else(|| self.docs.get(&uri).map(|t| t.clone()))
            .unwrap_or_default();
        let generation = self.bump(&uri);
        self.update(uri.clone(), text, None, generation).await;
        self.refresh_dependents(&uri).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        let uri = p.text_document.uri;
        self.bump(&uri); // cancels a pending analysis
        self.docs.remove(&uri);
        self.asts.remove(&uri);
        self.semantic_cache.remove(&uri);
        self.doc_versions.remove(&uri);
        // Leave no stale diagnostics behind for a document that is no longer open.
        self.client.publish_diagnostics(uri.clone(), Vec::new(), None).await;
        self.refresh_dependents(&uri).await;
    }

    async fn hover(&self, p: HoverParams) -> Result<Option<Hover>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;

        let Some(text) = self.docs.get(uri) else {
            return Ok(None);
        };
        let Some(ast) = self.asts.get(uri) else {
            return Ok(None);
        };

        let offset = lsp_pos_to_offset(&text, pos);
        let content = hover_at(&ast, offset).unwrap_or_default();
        if content.is_empty() {
            return Ok(None);
        }

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```tinox\n{}\n```", content),
            }),
            range: None,
        }))
    }

    async fn completion(&self, p: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &p.text_document_position.text_document.uri;
        let pos = p.text_document_position.position;

        let Some(text) = self.docs.get(uri) else {
            self.client.log_message(MessageType::WARNING, "completion: no text for uri").await;
            return Ok(None);
        };
        let offset = lsp_pos_to_offset(&text, pos);
        let snippet = {
            let end = offset.min(text.len() as u32) as usize;
            let start = end.saturating_sub(20);
            format!("{:?}", &text[start..end])
        };
        let has_ast = self.asts.contains_key(uri);
        self.client.log_message(
            MessageType::INFO,
            format!("completion pos={:?} offset={} has_ast={} snippet={} stdlib_classes={}",
                pos, offset, has_ast, snippet, self.stdlib_registry.len()),
        ).await;

        // For completion, try parsing the text with the incomplete current line blanked out.
        // This gives us a valid AST with parameter/variable types even when the file
        // doesn't parse (e.g. the user has typed "ctx." which is an incomplete expression).
        let fresh_ast = parse_for_completion(&text, offset);

        // Build a per-request registry: global stdlib + any imports declared in the file.
        // Imported modules are guaranteed to be available via the embedded stdlib.
        let effective_registry = {
            let mut reg = self.stdlib_registry.clone();
            let modules = if let Some(ast) = fresh_ast.as_ref() {
                module_names_from_imports(ast)
            } else if let Some(ast) = self.asts.get(uri).as_deref() {
                module_names_from_imports(ast)
            } else {
                vec![]
            };
            for module in &modules {
                if !reg.contains_key(module.as_str()) {
                    load_embedded_module(module, &mut reg);
                }
            }
            if let (Some(ast), Ok(file)) = (fresh_ast.as_ref(), uri.to_file_path()) {
                if let Ok((dirs, _)) = package_context(&file) {
                    if let Ok(packages) = load_package_preludes(ast, &file, &dirs, &self.overlay()) {
                        for package in packages {
                            reg.extend(build_registry_from_source(&package));
                        }
                    }
                }
            }
            reg
        };

        let items = match (fresh_ast.as_ref(), self.asts.get(uri)) {
            (Some(ast), _) => completions_at(ast, &text, offset, &effective_registry),
            (None, Some(ast)) => completions_at(&ast, &text, offset, &effective_registry),
            (None, None) => completions_generic(),
        };
        self.client.log_message(
            MessageType::INFO,
            format!("completion: {} items (fresh_ast={})", items.len(), fresh_ast.is_some()),
        ).await;
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn goto_definition(
        &self,
        p: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let pos = p.text_document_position_params.position;

        let Some(text) = self.docs.get(uri) else {
            return Ok(None);
        };
        let Some(ast) = self.asts.get(uri) else {
            return Ok(None);
        };

        let offset = lsp_pos_to_offset(&text, pos);
        // The checked AST answers locals and same-file declarations precisely;
        // anything it cannot place is resolved across the project's files.
        if let Some(loc) = definition_at(&ast, uri, offset) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }
        let scan = self.project_scan(uri);
        self.warn_if_partial(&scan).await;
        let files = scan.files;
        let mut found = navigation::definitions(&files, uri, offset as usize, &self.nav_ctx());
        Ok(match found.len() {
            0 => None,
            1 => Some(GotoDefinitionResponse::Scalar(found.remove(0))),
            _ => Some(GotoDefinitionResponse::Array(found)),
        })
    }

    async fn references(&self, p: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &p.text_document_position.text_document.uri;
        let Some(text) = self.docs.get(uri).map(|t| t.clone()) else { return Ok(None) };
        let offset = lsp_pos_to_offset(&text, p.text_document_position.position) as usize;
        let scan = self.project_scan(uri);
        self.warn_if_partial(&scan).await;
        let files = scan.files;
        match navigation::references(&files, uri, offset, p.context.include_declaration, &self.nav_ctx()) {
            Ok(locations) => Ok(Some(locations)),
            Err(_) => Ok(None),
        }
    }

    async fn prepare_rename(&self, p: TextDocumentPositionParams) -> Result<Option<PrepareRenameResponse>> {
        let uri = &p.text_document.uri;
        let Some(text) = self.docs.get(uri).map(|t| t.clone()) else { return Ok(None) };
        let offset = lsp_pos_to_offset(&text, p.position) as usize;
        let scan = self.project_scan(uri);
        if !scan.complete() {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(format!("rename refused: {}", scan.describe())));
        }
        let files = scan.files;
        match navigation::prepare_rename(&files, uri, offset, &self.nav_ctx()) {
            Ok(r) => Ok(Some(PrepareRenameResponse::RangeWithPlaceholder { range: r.range, placeholder: r.placeholder })),
            Err(message) => Err(tower_lsp::jsonrpc::Error::invalid_params(message)),
        }
    }

    async fn rename(&self, p: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &p.text_document_position.text_document.uri;
        let Some(text) = self.docs.get(uri).map(|t| t.clone()) else { return Ok(None) };
        let offset = lsp_pos_to_offset(&text, p.text_document_position.position) as usize;
        let scan = self.project_scan(uri);
        if !scan.complete() {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(format!("rename refused: {}", scan.describe())));
        }
        let files = scan.files;
        let result = navigation::rename(&files, uri, offset, &p.new_name, &self.nav_ctx())
            .map_err(tower_lsp::jsonrpc::Error::invalid_params)?;
        let text_edits = |edits: Vec<TextEdit>, uri: Url| TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
            edits: edits.into_iter().map(OneOf::Left).collect(),
        };
        match result.file_rename {
            Some((old, new)) if self.client_renames_files.load(std::sync::atomic::Ordering::Relaxed) => {
                // Edit first (old URI), then rename the file.
                let mut ops: Vec<DocumentChangeOperation> = result
                    .edits
                    .into_iter()
                    .map(|(u, e)| DocumentChangeOperation::Edit(text_edits(e, u)))
                    .collect();
                ops.push(DocumentChangeOperation::Op(ResourceOp::Rename(RenameFile {
                    old_uri: old,
                    new_uri: new,
                    options: None,
                    annotation_id: None,
                })));
                Ok(Some(WorkspaceEdit { changes: None, document_changes: Some(DocumentChanges::Operations(ops)), change_annotations: None }))
            }
            other => {
                if let Some((old, new)) = other {
                    self.client
                        .show_message(
                            MessageType::WARNING,
                            format!(
                                "Tinox requires one type per file named like the type: rename {} to {} yourself (this editor cannot rename files through the language server).",
                                old.path(),
                                new.path()
                            ),
                        )
                        .await;
                }
                Ok(Some(WorkspaceEdit { changes: Some(result.edits), document_changes: None, change_annotations: None }))
            }
        }
    }

    async fn symbol(&self, p: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        // Any open document locates the project; without one there is nothing to search.
        let Some(any) = self.docs.iter().next().map(|e| e.key().clone()) else { return Ok(None) };
        let scan = self.project_scan(&any);
        self.warn_if_partial(&scan).await;
        let files = scan.files;
        Ok(Some(navigation::workspace_symbols(&files, &p.query)))
    }

    async fn formatting(&self, p: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some(text) = self.docs.get(&p.text_document.uri).map(|t| t.clone()) else { return Ok(None) };
        formatting::format_edits(&text).map(Some).map_err(format_failed)
    }

    async fn range_formatting(&self, p: DocumentRangeFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let Some(text) = self.docs.get(&p.text_document.uri).map(|t| t.clone()) else { return Ok(None) };
        formatting::format_range_edits(&text, p.range).map(Some).map_err(format_failed)
    }

    async fn semantic_tokens_full(&self, p: SemanticTokensParams) -> Result<Option<SemanticTokensResult>> {
        let uri = p.text_document.uri;
        let Some((id, tokens)) = self.semantic_for(&uri) else { return Ok(None) };
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: Some(id), data: tokens })))
    }

    async fn semantic_tokens_full_delta(&self, p: SemanticTokensDeltaParams) -> Result<Option<SemanticTokensFullDeltaResult>> {
        let uri = p.text_document.uri;
        let previous = self.semantic_cache.get(&uri).filter(|c| c.0 == p.previous_result_id).map(|c| c.1.clone());
        let Some((id, tokens)) = self.semantic_for(&uri) else { return Ok(None) };
        Ok(Some(match previous {
            Some(old) => {
                let edits = semantic::delta(&old, &tokens)
                    .map(|(start, delete_count, data)| vec![SemanticTokensEdit { start, delete_count, data: Some(data) }])
                    .unwrap_or_default();
                SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta { result_id: Some(id), edits })
            }
            None => SemanticTokensFullDeltaResult::Tokens(SemanticTokens { result_id: Some(id), data: tokens }),
        }))
    }

    async fn semantic_tokens_range(&self, p: SemanticTokensRangeParams) -> Result<Option<SemanticTokensRangeResult>> {
        let Some(text) = self.docs.get(&p.text_document.uri).map(|t| t.clone()) else { return Ok(None) };
        let types = self.stdlib_registry.keys().cloned().collect();
        let Some(all) = semantic::compute(&text, &p.text_document.uri, &types) else { return Ok(None) };
        let part = semantic::in_range(&all, p.range.start.line, p.range.end.line);
        // relative encoding restarts at the first returned token
        Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens { result_id: None, data: semantic::encode(&part) })))
    }

    async fn signature_help(&self, p: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &p.text_document_position_params.text_document.uri;
        let Some(text) = self.docs.get(uri).map(|t| t.clone()) else { return Ok(None) };
        let offset = lsp_pos_to_offset(&text, p.text_document_position_params.position) as usize;
        let Some(ctx) = signature::call_context(&text, offset) else { return Ok(None) };
        let (registry, functions) = self.project_signatures(uri, Some(offset));
        Ok(signature::build(&ctx, signature::candidates(&ctx.callee, &registry, &functions)))
    }

    async fn code_action(&self, p: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = p.text_document.uri.clone();
        let Some(text) = self.docs.get(&uri).map(|t| t.clone()) else { return Ok(None) };
        let wants = |kind: &CodeActionKind| p.context.only.as_ref().is_none_or(|only| only.iter().any(|k| kind.as_str().starts_with(k.as_str())));
        let edit = |edits: Vec<TextEdit>| WorkspaceEdit { changes: Some(HashMap::from([(uri.clone(), edits)])), document_changes: None, change_annotations: None };
        let mut out: CodeActionResponse = Vec::new();
        if wants(&CodeActionKind::QUICKFIX) {
            let project: Vec<(String, String)> = self.project_files(&uri).into_iter().map(|(u, t)| (u.to_string(), t)).collect();
            let project_refs: Vec<(String, &str)> = project.iter().map(|(u, t)| (u.clone(), t.as_str())).collect();
            let mut seen = std::collections::HashSet::new();
            for diag in &p.context.diagnostics {
                let Some(name) = actions::missing_import_name(&diag.message) else { continue };
                let candidates = actions::import_candidates(&name, &project_refs, &self.stdlib_import_map);
                let preferred = candidates.len() == 1;
                for path in candidates {
                    if !seen.insert((name.clone(), path.clone())) {
                        continue;
                    }
                    out.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Import {path}"),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diag.clone()]),
                        edit: Some(edit(vec![actions::add_import_edit(&text, &path)])),
                        is_preferred: Some(preferred),
                        ..Default::default()
                    }));
                }
            }
        }
        if wants(&CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
            if let Some(e) = actions::organize_imports(&text) {
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Organize imports".into(),
                    kind: Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS),
                    edit: Some(edit(vec![e])),
                    ..Default::default()
                }));
            }
        }
        Ok(if out.is_empty() { None } else { Some(out) })
    }

    async fn document_symbol(
        &self,
        p: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &p.text_document.uri;
        let Some(ast) = self.asts.get(uri) else {
            return Ok(None);
        };
        let mut symbols = document_symbols(&ast);
        if let Some(text) = self.docs.get(uri) {
            navigation::refine_document_symbols(&mut symbols, uri, &text);
        }
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }
}

/// LSP `RequestFailed`: the request was understood but cannot be carried out.
fn format_failed(message: String) -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error { code: tower_lsp::jsonrpc::ErrorCode::ServerError(-32803), message: message.into(), data: None }
}

/// Parse the text for completion by blanking out the line that contains the cursor.
/// This handles the common case where the current line is an incomplete expression
/// (e.g. "ctx.") that prevents the whole file from parsing.
fn parse_for_completion(text: &str, offset: u32) -> Option<SourceFile> {
    let offset = offset as usize;
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[offset..].find('\n').map(|i| offset + i).unwrap_or(text.len());
    // Replace the current line with whitespace so the structure (braces) is preserved
    let blank = " ".repeat(line_end - line_start);
    let cleaned = format!("{}{}{}", &text[..line_start], blank, &text[line_end..]);
    let tokens = Lexer::new(&cleaned).tokenize().ok()?;
    Parser::new(tokens).parse().ok()
}

fn completions_generic() -> Vec<tower_lsp::lsp_types::CompletionItem> {
    use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};
    const KW: &[&str] = &["fn", "const", "var", "return", "if", "else", "while", "for", "class", "import"];
    KW.iter().map(|k| CompletionItem {
        label: k.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        ..Default::default()
    }).collect()
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_call_keeps_same_file_methods_for_signature_help() {
        let text = "class Main\n{\n    fn acceptBox(first: Int64, second: String) -> Nothing {}\n    fn run() -> Nothing\n    {\n        this.acceptBox(1, \n    }\n}\n";
        assert!(tinox_parser::parse_source_cached(text).is_err());
        let cursor = text.find("this.acceptBox(1, ").unwrap() + "this.acceptBox(1, ".len();
        let ast = parse_for_completion(text, cursor as u32).expect("recoverable");
        let registry = build_registry_from_source(&ast);
        assert!(registry["Main"].methods.iter().any(|m| m.name == "acceptBox"));
    }

    #[test]
    fn test_embedded_stdlib_has_http_context() {
        let reg = build_embedded_stdlib();
        println!("embedded stdlib classes: {:?}", reg.keys().collect::<Vec<_>>());
        assert!(reg.contains_key("HttpContext"), "HttpContext must be in embedded stdlib");
        assert!(reg.contains_key("HttpResponse"), "HttpResponse must be in embedded stdlib");
        for class in ["Watcher", "WatchEvent", "InteractiveProcess", "ProcessResult"] {
            assert!(reg.contains_key(class), "{class} must be in embedded stdlib");
        }
    }

    /// Regression coverage for the exact shape of a real-world false
    /// positive reported live in Eclipse against the external `demo`
    /// project (layered demo.model/demo.dao/demo.service, see CLAUDE.md):
    /// `Backend::update` only ever resolved `tinox.core.*` imports
    /// (against `self.stdlib_asts`) — a project-local import like `import
    /// demo.dao.PersonDao;` had NO resolution path at all, so every
    /// cross-file project-local reference (the common case once issue
    /// #194 made explicit imports mandatory) showed up as a spurious
    /// "undefined variable"/"undefined function"/"expected X, found Y"
    /// in the editor even though `tinox build` resolved it fine.
    fn write_demo_fixture(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("demo/model")).unwrap();
        std::fs::create_dir_all(root.join("demo/dao")).unwrap();
        std::fs::write(root.join("tinox.toml"), "[package]\ncompiler = \"2.8.0\"\nname = \"x\"\nversion = \"0.1.0\"\ndescription = \"\"\n").unwrap();
        std::fs::write(
            root.join("demo/model/Person.tnx"),
            "namespace demo.model {\n    class Person {\n        var id: Int64\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("demo/dao/PersonDao.tnx"),
            "import demo.model.Person\n\nnamespace demo.dao {\n    interface PersonDao {\n        fn findAll() -> List<Person>\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("demo/dao/PersonDaoImpl.tnx"),
            "import demo.dao.PersonDao\nimport demo.model.Person\n\nnamespace demo.dao {\n    class PersonDaoImpl implements PersonDao {\n        fn findAll() -> List<Person> {\n            return []\n        }\n    }\n}\n",
        )
        .unwrap();
    }

    #[test]
    fn resolve_project_local_import_finds_a_file_in_a_different_namespace_directory() {
        let root = std::env::temp_dir().join(format!("tinox-lsp-test-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_demo_fixture(&root);

        let importing_from = root.join("demo/dao/PersonDaoImpl.tnx");
        let path = ["demo".to_string(), "model".to_string(), "Person".to_string()];
        let resolved = resolve_project_local_import(&path, &importing_from);
        assert_eq!(resolved, Some(root.join("demo/model/Person.tnx")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_project_local_import_never_touches_tinox_core() {
        let root = std::env::temp_dir().join(format!("tinox-lsp-test-resolve-core-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_demo_fixture(&root);

        let importing_from = root.join("demo/dao/PersonDaoImpl.tnx");
        let path = ["tinox".to_string(), "core".to_string(), "db".to_string()];
        assert_eq!(resolve_project_local_import(&path, &importing_from), None);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_local_preludes_resolves_transitively_and_typechecks_clean() {
        let root = std::env::temp_dir().join(format!("tinox-lsp-test-preludes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_demo_fixture(&root);

        // PersonDaoImpl.tnx directly imports PersonDao (sibling, same dir)
        // and Person (different namespace dir) -- both must resolve, and
        // typecheck_with_prelude must accept `implements PersonDao` and
        // `-> List<Person>` cleanly against them, matching what `tinox
        // build`/`tinox check` already do for the real project.
        let file_path = root.join("demo/dao/PersonDaoImpl.tnx");
        let text = std::fs::read_to_string(&file_path).unwrap();
        let tokens = Lexer::new(&text).tokenize().unwrap();
        let ast = Parser::new(tokens).parse().unwrap();

        let preludes_owned = load_project_local_preludes(&ast, &file_path, &|_| None);
        assert_eq!(preludes_owned.len(), 2, "expected PersonDao.tnx and Person.tnx to both resolve");

        let preludes: Vec<&SourceFile> = preludes_owned.iter().collect();
        let result = tinox_typecheck::typecheck_with_prelude(&ast, &preludes);
        assert!(result.is_ok(), "expected clean typecheck, got: {:?}", result.err().map(|b| b.errors));

        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn shared_codes_details_and_utf16_ranges_reach_lsp() {
        let source = "class Main { fnc main() -> Int32 { println(\"😀\")\nreturn absent } }";
        let diagnostics = compile(source, Some("/tmp/Main.tnx"), &[], |_| {});
        let diagnostic = diagnostics.iter().find(|d| d.message.contains("undefined variable")).unwrap();
        assert_eq!(diagnostic.code, Some(NumberOrString::String("TINOX-TYPE".into())));
        let data = diagnostic.data.as_ref().unwrap();
        assert_eq!(data["schema_version"], 1);
        assert_eq!(data["diagnostic"]["file"], "/tmp/Main.tnx");
        let second_line = source.lines().nth(1).unwrap();
        let prefix = &second_line[..second_line.find("absent").unwrap()];
        assert_eq!(diagnostic.range.start.line, 1);
        assert_eq!(diagnostic.range.start.character, prefix.encode_utf16().count() as u32);
        let diagnostics = compile("class Main { fnc main() -> Int32 { return \"wrong\" } }", None, &[], |_| {});
        let data = diagnostics[0].data.as_ref().unwrap();
        assert_eq!(data["diagnostic"]["expected"], "Int32");
        assert_eq!(data["diagnostic"]["actual"], "String");
        assert_eq!(data["diagnostic"]["code"], "TINOX-TYPE-MISMATCH");
    }

    #[test]
    fn related_locations_and_severity_reach_lsp() {
        let dir = std::env::temp_dir().join(format!("tinox-lsp-related-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("Other.tnx");
        // an emoji before the target pushes its UTF-16 column past its scalar column
        std::fs::write(&other, "// 😀 class Other\nclass Other {}\n").unwrap();
        let main = dir.join("Main.tnx");
        let span = |l: u32, c0: u32, c1: u32| tinox_common::Span {
            start: tinox_common::Pos { offset: 0, line: l, column: c0 },
            end: tinox_common::Pos { offset: 0, line: l, column: c1 },
        };
        let mut d = CompilerDiagnostic::new("type", Some(main.to_str().unwrap()), Some(span(1, 7, 12)), "mismatch");
        d.severity = "warning".into();
        d.related.push(tinox_common::diagnostic::RelatedLocation {
            file: other.canonicalize().unwrap().to_string_lossy().into_owned(),
            range: span(2, 7, 12),
            message: "declared here".into(),
        });
        let lsp = err_to_diag(d, "class Main {}\n");
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::WARNING));
        let rel = lsp.related_information.unwrap();
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0].message, "declared here");
        assert!(rel[0].location.uri.path().ends_with("Other.tnx"));
        assert_eq!((rel[0].location.range.start.line, rel[0].location.range.start.character), (1, 6));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parser_and_annotation_categories_match_the_cli() {
        for (source, category) in [("class Main {", "syntax"), ("`", "lex"), ("@Unknown class Main {}", "annotation")] {
            let diagnostics = compile(source, None, &[], |_| {});
            assert_eq!(diagnostics[0].data.as_ref().unwrap()["diagnostic"]["category"], category);
        }
    }

    #[test]
    fn local_package_selection_refreshes_types_and_reports_cli_provenance() {
        let root = std::env::temp_dir().join(format!("tinox-lsp-overrides-{}", std::process::id()));
        let local = root.join("package/src/tinox/core/json");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(root.join("tinox.toml"), "[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"json\"\nversion=\"1.0.0\"\n").unwrap();
        std::fs::write(root.join("package/tinox.toml"), "[package]\ncompiler = \"2.8.0\"\nname=\"json\"\nversion=\"1.0.0\"\n").unwrap();
        std::fs::write(root.join("tinox.overrides.yaml"), "overrides:\n  'tinox.core:json:1.0.0': package\n").unwrap();
        let entry = root.join("Main.tnx");
        let source = "import tinox.core.json.LocalJson\nclass Main { fnc main() -> Int32 { const n: Int64 = LocalJson::unpublished()\nreturn 0 } }";
        std::fs::write(&entry, source).unwrap();
        std::fs::write(local.join("LocalJson.tnx"), "namespace tinox.core.json { class LocalJson { fnc unpublished() -> Int64 { return Helper::value() } } }").unwrap();
        std::fs::write(local.join("Helper.tnx"), "namespace tinox.core.json { class Helper { fnc value() -> Int64 { return 42 } } }").unwrap();
        let ast = tinox_parser::parse_source_cached(source).unwrap();
        let (dirs, provenance) = package_context(&entry).unwrap();
        let cli_dirs = tinox::pm::dependency_dirs(&root, &tinox::pm::read_manifest(&root).unwrap()).unwrap().0;
        assert_eq!(dirs, cli_dirs);
        assert_eq!(provenance, tinox::pm::source_provenance(&root, &cli_dirs).unwrap());
        assert!(provenance[0].contains("local override: tinox.core:json:1.0.0"));
        let packages = load_package_preludes(&ast, &entry, &dirs, &|_| None).unwrap();
        let preludes: Vec<_> = packages.iter().collect();
        assert!(compile(source, entry.to_str(), &preludes, |_| {}).is_empty());
        std::fs::write(local.join("LocalJson.tnx"), "namespace tinox.core.json { class LocalJson { fnc changed() -> Int64 { return 43 } } }").unwrap();
        let packages = load_package_preludes(&ast, &entry, &dirs, &|_| None).unwrap();
        let preludes: Vec<_> = packages.iter().collect();
        assert!(!compile(source, entry.to_str(), &preludes, |_| {}).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

}

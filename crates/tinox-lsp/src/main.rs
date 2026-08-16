mod analysis;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use analysis::{
    build_registry_from_source, completions_at, definition_at, document_symbols,
    hover_at, lsp_pos_to_offset, pos_to_lsp, TypeRegistry,
};
use dashmap::DashMap;
use tinox_common::Error;
use tinox_lexer::Lexer;
use tinox_parser::{ast::SourceFile, Parser};
use tinox_typecheck::{typecheck_with_prelude};
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
    ("fs",            include_str!("../../tinox-core/tinox/core/fs/Fs.tnx")),
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
    ("process",       include_str!("../../tinox-core/tinox/core/process/Process.tnx")),
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

struct Backend {
    client: Client,
    docs: DashMap<Url, String>,
    asts: DashMap<Url, SourceFile>,
    stdlib_registry: TypeRegistry,
    stdlib_asts: HashMap<String, SourceFile>,
}

impl Backend {
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
        Self {
            client,
            docs: DashMap::new(),
            asts: DashMap::new(),
            stdlib_registry,
            stdlib_asts,
        }
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

/// Recursively resolves `source`'s own project-local imports (and THEIR
/// project-local imports, and so on) into parsed ASTs — the project-local
/// counterpart to `self.stdlib_asts`, built fresh per `update()` call
/// rather than cached, since project files can change on disk between
/// edits and this LSP has no file-watcher wired up to invalidate a cache.
/// `visited` guards against an import cycle (two files importing each
/// other) looping forever — same purpose as `tinox`'s own `resolve_imports`
/// `visited: &mut HashSet<PathBuf>` parameter.
fn load_project_local_preludes(source: &tinox_parser::ast::SourceFile, file_path: &Path) -> Vec<SourceFile> {
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
        let Ok(src) = std::fs::read_to_string(&target) else { continue };
        let Ok(tokens) = Lexer::new(&src).tokenize() else { continue };
        let Ok(ast) = Parser::new(tokens).parse() else { continue };
        for p in import_paths(&ast) {
            queue.push((p, target.clone()));
        }
        out.push(ast);
    }
    out
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

impl Backend {
    async fn update(&self, uri: Url, text: String) {
        // Quick pre-parse to extract import names for stdlib prelude resolution.
        let parsed = if let Ok(tokens) = Lexer::new(&text).tokenize() {
            Parser::new(tokens).parse().ok()
        } else {
            None
        };
        let import_names = parsed.as_ref().map(module_names_from_imports).unwrap_or_default();
        let mut preludes: Vec<&SourceFile> = import_names
            .iter()
            .filter_map(|name| self.stdlib_asts.get(name.as_str()))
            .collect();

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
            (Some(ast), Ok(file_path)) => load_project_local_preludes(ast, &file_path),
            _ => Vec::new(),
        };
        preludes.extend(project_local.iter());

        let diags = compile(&text, &preludes, |ast| {
            self.asts.insert(uri.clone(), ast);
        });
        self.docs.insert(uri.clone(), text);
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

fn err_to_diag(e: Error) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: pos_to_lsp(e.span.start),
            end: pos_to_lsp(e.span.end),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        message: e.message,
        source: Some("tinox".into()),
        ..Default::default()
    }
}

// Lex → parse → typecheck; calls on_ast if parsing succeeded; returns diagnostics.
fn compile(src: &str, preludes: &[&SourceFile], on_ast: impl FnOnce(SourceFile)) -> Vec<Diagnostic> {
    let tokens = match Lexer::new(src).tokenize() {
        Ok(t) => t,
        Err(errs) => return errs.into_iter().map(err_to_diag).collect(),
    };

    let ast = match Parser::new(tokens).parse() {
        Ok(a) => a,
        Err(bag) => return bag.errors.into_iter().map(err_to_diag).collect(),
    };

    let diags = match typecheck_with_prelude(&ast, preludes) {
        Ok(_) => vec![],
        Err(bag) => bag.errors.into_iter().map(err_to_diag).collect(),
    };

    on_ast(ast);
    diags
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
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
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, p: DidOpenTextDocumentParams) {
        self.update(p.text_document.uri, p.text_document.text).await;
    }

    async fn did_change(&self, p: DidChangeTextDocumentParams) {
        if let Some(change) = p.content_changes.into_iter().last() {
            self.update(p.text_document.uri, change.text).await;
        }
    }

    async fn did_save(&self, p: DidSaveTextDocumentParams) {
        let uri = p.text_document.uri;
        let text = p.text
            .or_else(|| self.docs.get(&uri).map(|t| t.clone()))
            .unwrap_or_default();
        self.update(uri, text).await;
    }

    async fn did_close(&self, p: DidCloseTextDocumentParams) {
        self.docs.remove(&p.text_document.uri);
        self.asts.remove(&p.text_document.uri);
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
        let loc = definition_at(&ast, uri, offset);
        Ok(loc.map(GotoDefinitionResponse::Scalar))
    }

    async fn document_symbol(
        &self,
        p: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &p.text_document.uri;
        let Some(ast) = self.asts.get(uri) else {
            return Ok(None);
        };
        Ok(Some(DocumentSymbolResponse::Nested(document_symbols(&ast))))
    }
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
    const KW: &[&str] = &["fn", "let", "var", "return", "if", "else", "while", "for", "class", "import"];
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
    fn test_embedded_stdlib_has_http_context() {
        let reg = build_embedded_stdlib();
        println!("embedded stdlib classes: {:?}", reg.keys().collect::<Vec<_>>());
        assert!(reg.contains_key("HttpContext"), "HttpContext must be in embedded stdlib");
        assert!(reg.contains_key("HttpResponse"), "HttpResponse must be in embedded stdlib");
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
        std::fs::write(root.join("tinox.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\ndescription = \"\"\n").unwrap();
        std::fs::write(
            root.join("demo/model/Person.tnx"),
            "namespace demo.model {\n    class Person {\n        var id: Int64;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("demo/dao/PersonDao.tnx"),
            "import demo.model.Person;\n\nnamespace demo.dao {\n    interface PersonDao {\n        fn findAll() -> List<Person>;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("demo/dao/PersonDaoImpl.tnx"),
            "import demo.dao.PersonDao;\nimport demo.model.Person;\n\nnamespace demo.dao {\n    class PersonDaoImpl implements PersonDao {\n        fn findAll() -> List<Person> {\n            return [];\n        }\n    }\n}\n",
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

        let preludes_owned = load_project_local_preludes(&ast, &file_path);
        assert_eq!(preludes_owned.len(), 2, "expected PersonDao.tnx and Person.tnx to both resolve");

        let preludes: Vec<&SourceFile> = preludes_owned.iter().collect();
        let result = typecheck_with_prelude(&ast, &preludes);
        assert!(result.is_ok(), "expected clean typecheck, got: {:?}", result.err().map(|b| b.errors));

        std::fs::remove_dir_all(&root).ok();
    }
}

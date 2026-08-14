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
        let import_names = if let Ok(tokens) = Lexer::new(&text).tokenize() {
            if let Ok(ast) = Parser::new(tokens).parse() {
                module_names_from_imports(&ast)
            } else {
                vec![]
            }
        } else {
            vec![]
        };
        let preludes: Vec<&SourceFile> = import_names
            .iter()
            .filter_map(|name| self.stdlib_asts.get(name.as_str()))
            .collect();

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
}

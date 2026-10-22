//! Resolve API descriptions through the compiler's real dependency graph.
use super::*;
use serde::Serialize;
use tinox_parser::api::{self, Symbol};
use tinox_typecheck::annotations::{AnnotationInfo, AnnotationProcessor, AnnotationTarget};

#[derive(Default)]
struct Options {
    query: String,
    file: Option<String>,
    member: Option<String>,
    signatures_only: bool,
    json: bool,
    limit: usize,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            limit: 40,
            ..Self::default()
        };
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--json" => options.json = true,
                "--signatures-only" => options.signatures_only = true,
                "--file" | "--member" | "--limit" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .filter(|s| !s.starts_with('-'))
                        .ok_or_else(|| format!("{arg} requires a value"))?;
                    match arg.as_str() {
                        "--file" => options.file = Some(value.clone()),
                        "--member" => options.member = Some(value.clone()),
                        _ => {
                            options.limit = value
                                .parse()
                                .ok()
                                .filter(|n| (1..=200).contains(n))
                                .ok_or("--limit must be between 1 and 200")?
                        }
                    }
                }
                _ if arg.starts_with('-') => return Err(format!("Unknown describe option: {arg}")),
                _ if options.query.is_empty() => options.query = arg.clone(),
                _ => {
                    return Err("Expected one symbol; use --file to select the entry source".into())
                }
            }
            index += 1;
        }
        if options.query.is_empty() {
            return Err("Usage: tinox describe <Symbol|@Annotation> [--file entry.tnx] [--member name] [--signatures-only] [--limit 1..200] [--json]".into());
        }
        if let Some((symbol, member)) = options.query.split_once("::") {
            if options.member.is_some() || member.is_empty() {
                return Err("Specify a member once, using ::member or --member".into());
            }
            options.member = Some(member.into());
            options.query = symbol.into();
        }
        Ok(options)
    }
}

#[derive(Serialize)]
struct Description {
    schema_version: u32,
    kind: &'static str,
    success: bool,
    symbol: Option<Symbol>,
    annotation: Option<AnnotationDescription>,
    checked: bool,
    signatures_only: bool,
}
#[derive(Serialize)]
struct AnnotationDescription {
    name: String,
    valid_targets: Vec<AnnotationTarget>,
    min_args: usize,
    max_args: Option<usize>,
    description: String,
}
impl From<AnnotationInfo> for AnnotationDescription {
    fn from(info: AnnotationInfo) -> Self {
        Self {
            name: info.name,
            valid_targets: info.valid_targets,
            min_args: info.min_args,
            max_args: (info.max_args != usize::MAX).then_some(info.max_args),
            description: info.description,
        }
    }
}
#[derive(Serialize)]
struct Candidate {
    qualified_name: String,
    source: String,
    required_import: Option<String>,
}
#[derive(Serialize)]
struct DescribeError {
    code: &'static str,
    message: String,
    candidates: Vec<Candidate>,
}
impl DescribeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            candidates: vec![],
        }
    }
}
impl From<String> for DescribeError {
    fn from(message: String) -> Self {
        Self::new("TINOX-DESCRIBE", message)
    }
}
impl From<&str> for DescribeError {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

struct Graph {
    symbols: Vec<Symbol>,
    annotations: AnnotationProcessor,
}

fn default_entry(base: &Path) -> Option<PathBuf> {
    let root = pm::find_project_root_from(base)?;
    let manifest = build_cache::read_to_string(root.join("tinox.toml")).ok()?;
    entry_candidates(&manifest)
        .into_iter()
        .map(|entry| root.join(entry))
        .find(|path| path.is_file())
}

fn import_path(path: &Path, base: &Path, dependencies: &[PathBuf]) -> String {
    let project = pm::find_project_root_from(base).unwrap_or_else(|| base.to_path_buf());
    let core = stdlib_dir();
    let mut roots = dependencies.to_vec();
    if let Some(core) = core {
        roots.push(core);
    }
    roots.extend([project.join("src"), project.join("tests"), project]);
    for root in roots {
        let root = root.canonicalize().unwrap_or(root);
        if let Ok(relative) = path.strip_prefix(&root) {
            return relative
                .with_extension("")
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join(".");
        }
    }
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn graph(
    roots: &[PathBuf],
    base: &Path,
    dependencies: &[PathBuf],
    missing: &[pm::MissingDep],
    validate_visibility: bool,
) -> Result<Graph, String> {
    let mut visited = HashSet::new();
    let mut program = tinox_parser::SourceFile::new(vec![]);
    for root in roots {
        let root = root
            .canonicalize()
            .map_err(|e| format!("Cannot read '{}': {e}", root.display()))?;
        if let Some(declarations) =
            resolve_and_merge_file(&root, &mut visited, dependencies, missing)?
        {
            program.decls.extend(declarations);
        }
    }
    if validate_visibility {
        for root in roots {
            check_explicit_imports(root, dependencies, missing)?;
        }
    }
    synthesize_accessors(&mut program, &roots[0].to_string_lossy())?;
    tinox_parser::assign_node_ids(&mut program);
    tinox_typecheck::cache::check_program_cached(&program)
        .map_err(|e| format!("Type error:\n{e}"))?;

    let mut paths: Vec<_> = visited.into_iter().collect();
    paths.sort();
    let mut symbols = vec![];
    let mut processor = AnnotationProcessor::new();
    let mut cache = std::collections::HashMap::new();
    for path in paths {
        parse_into_cache(&path, &mut cache)?;
        let source = &cache[&path];
        processor.register_source(source);
        let pristine = parse_frontend(
            &build_cache::read_to_string(&path).map_err(|e| e.to_string())?,
            Some(&path),
        )?;
        let declared = api::symbols(&pristine, &path.to_string_lossy(), "");
        let mut items = api::symbols(
            source,
            &path.to_string_lossy(),
            &import_path(&path, base, dependencies),
        );
        let package = dependencies
            .iter()
            .find(|root| path.starts_with(root.canonicalize().unwrap_or_else(|_| (*root).clone())))
            .map(|root| dep_dir_coordinate(root));
        for item in &mut items {
            item.package = package.clone();
            if let Some(original) = declared
                .iter()
                .find(|original| original.qualified_name == item.qualified_name)
            {
                for member in &mut item.members {
                    if !original
                        .members
                        .iter()
                        .any(|original| original.name == member.name)
                    {
                        member.generated = true;
                        member.range = None;
                    }
                }
            }
        }
        symbols.extend(items);
    }
    Ok(Graph {
        symbols,
        annotations: processor,
    })
}

fn matches(symbol: &Symbol, query: &str) -> bool {
    symbol.name == query
        || symbol.qualified_name == query
        || symbol.required_import.as_deref() == Some(format!("import {query};").as_str())
}
fn lookup<'a>(graph: &'a Graph, query: &str) -> Vec<&'a Symbol> {
    graph
        .symbols
        .iter()
        .filter(|symbol| matches(symbol, query))
        .collect()
}

fn describe(options: &Options) -> Result<Description, DescribeError> {
    let annotation_name = options.query.strip_prefix('@');
    let query = annotation_name.unwrap_or(&options.query);
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let entry = options
        .file
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| default_entry(&cwd));
    let base = entry
        .as_ref()
        .and_then(|file| file.parent())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(&cwd);
    let (dependencies, missing) = load_dep_dirs(base)?;
    let mut resolved = if let Some(entry) = &entry {
        graph(
            std::slice::from_ref(entry),
            base,
            &dependencies,
            &missing,
            true,
        )?
    } else {
        Graph {
            symbols: vec![],
            annotations: AnnotationProcessor::new(),
        }
    };
    if annotation_name.is_some() && options.member.is_some() {
        return Err("Annotations do not have selectable members".into());
    }
    let custom = lookup(&resolved, query).iter().any(|symbol| {
        symbol
            .annotations
            .iter()
            .any(|annotation| annotation == "@annotation")
    });
    let builtin = AnnotationProcessor::new();
    if !custom {
        if let Some(info) = annotation_name.and_then(|name| builtin.info(name)) {
            let mut annotation: AnnotationDescription = info.clone().into();
            if options.signatures_only {
                annotation.description.clear();
            }
            return Ok(Description {
                schema_version: 1,
                kind: "symbol_description",
                success: true,
                symbol: None,
                annotation: Some(annotation),
                checked: true,
                signatures_only: options.signatures_only,
            });
        }
    }
    if lookup(&resolved, query).is_empty() && query.contains('.') {
        let import = tinox_parser::Import {
            path: query.split('.').map(str::to_owned).collect(),
            alias: None,
            span: tinox_common::Span::dummy(),
        };
        let (roots, origin) = resolve_import_target(&import, base, &dependencies, &missing)
            .map_err(|message| DescribeError::new("TINOX-DESCRIBE-RESOLUTION", message))?;
        resolved = graph(
            &roots,
            base,
            &dependencies,
            &missing,
            origin == ImportOrigin::Local,
        )?;
    }
    let candidates = lookup(&resolved, query);
    if candidates.is_empty() {
        return Err(DescribeError::new("TINOX-DESCRIBE-UNKNOWN", format!("Unknown symbol '{}'. Use a namespace-qualified name, import its module, or select an entry with --file.", options.query)));
    }
    if candidates.len() != 1 {
        let mut failure = DescribeError::new(
            "TINOX-DESCRIBE-AMBIGUOUS",
            format!(
                "Ambiguous symbol '{}':\n{}",
                options.query,
                candidates
                    .iter()
                    .map(|symbol| format!(
                        "  {} ({}) [{}]",
                        symbol.qualified_name,
                        symbol.source,
                        symbol.required_import.as_deref().unwrap_or("entry source")
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        );
        failure.candidates = candidates
            .iter()
            .map(|symbol| Candidate {
                qualified_name: symbol.qualified_name.clone(),
                source: symbol.source.clone(),
                required_import: symbol.required_import.clone(),
            })
            .collect();
        return Err(failure);
    }
    let mut symbol = candidates[0].clone();
    let mut annotation: Option<AnnotationDescription> = if annotation_name.is_some() {
        Some(
            resolved
                .annotations
                .info(&symbol.name)
                .ok_or_else(|| format!("{} is not an annotation", symbol.qualified_name))?
                .clone()
                .into(),
        )
    } else {
        None
    };
    if let Some(member) = &options.member {
        symbol.members.retain(|candidate| &candidate.name == member);
        if symbol.members.is_empty() {
            return Err(DescribeError::new(
                "TINOX-DESCRIBE-MEMBER",
                format!(
                    "{} has no declared member '{member}'",
                    symbol.qualified_name
                ),
            ));
        }
    }
    if options.signatures_only {
        if let Some(annotation) = &mut annotation {
            annotation.description.clear();
        }
    }
    symbol.omitted_members = symbol.members.len().saturating_sub(options.limit);
    symbol.members.truncate(options.limit);
    symbol.documentation_truncated = api::bound_documentation(
        &mut symbol.documentation,
        if options.signatures_only { 0 } else { 1200 },
    );
    if options.signatures_only {
        symbol.documentation = None;
        symbol.documentation_truncated = false;
    }
    for member in &mut symbol.members {
        member.documentation_truncated = api::bound_documentation(
            &mut member.documentation,
            if options.signatures_only { 0 } else { 1200 },
        );
        if options.signatures_only {
            member.documentation = None;
            member.documentation_truncated = false;
        }
    }
    Ok(Description {
        schema_version: 1,
        kind: "symbol_description",
        success: true,
        symbol: Some(symbol),
        annotation,
        checked: true,
        signatures_only: options.signatures_only,
    })
}

pub fn run(args: &[String]) {
    let json = args.iter().any(|arg| arg == "--json");
    match Options::parse(args)
        .map_err(DescribeError::from)
        .and_then(|options| describe(&options))
    {
        Ok(description) if json => println!(
            "{}",
            serde_json::to_string(&description).expect("serializable API description")
        ),
        Ok(description) => {
            if let Some(symbol) = &description.symbol {
                println!("{}", symbol.signature);
                if !description.signatures_only {
                    println!("  {}", symbol.qualified_name);
                    println!("  source: {}", symbol.source);
                    if let Some(package) = &symbol.package {
                        println!("  package: {package}");
                    }
                    if let Some(import) = &symbol.required_import {
                        println!("  {import}");
                    }
                    for annotation in &symbol.annotations {
                        println!("  {annotation}");
                    }
                    if let Some(doc) = &symbol.documentation {
                        println!(
                            "\n{doc}{}",
                            if symbol.documentation_truncated {
                                " … [documentation truncated]"
                            } else {
                                ""
                            }
                        );
                    }
                }
                for member in &symbol.members {
                    println!("  {}", member.signature);
                    if !description.signatures_only {
                        for annotation in &member.annotations {
                            println!("    {annotation}");
                        }
                        if let Some(doc) = &member.documentation {
                            println!(
                                "    {}{}",
                                doc.replace('\n', "\n    "),
                                if member.documentation_truncated {
                                    " … [documentation truncated]"
                                } else {
                                    ""
                                }
                            );
                        }
                    }
                }
                if symbol.omitted_members > 0 {
                    println!(
                        "  … {} members omitted; select --member or increase --limit",
                        symbol.omitted_members
                    );
                }
            }
            if let Some(annotation) = &description.annotation {
                println!("@{}", annotation.name);
                println!("  targets: {:?}", annotation.valid_targets);
                println!(
                    "  arguments: {}..{}",
                    annotation.min_args,
                    annotation
                        .max_args
                        .map(|max| max.to_string())
                        .unwrap_or_else(|| "unbounded".into())
                );
                if !description.signatures_only {
                    println!("  {}", annotation.description);
                }
            }
        }
        Err(message) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "schema_version": 1, "kind": "symbol_description", "success": false, "error": message })
                );
            } else {
                eprintln!("error[{}]: {}", message.code, message.message);
            }
            std::process::exit(1);
        }
    }
}

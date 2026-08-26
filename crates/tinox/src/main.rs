use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tinox_codegen::CodeGen;
use tinox_lexer::Lexer;
use tinox_parser::{DeclKind, Formatter, Parser};

mod callgraph;
mod pm;

fn main() {
    // Run the compiler on a thread with a large stack. The parser, type checker
    // and code generator all recurse over the AST, so deeply nested (or maliciously
    // deep) input can overflow the default 8 MB main-thread stack. A 512 MB stack
    // pushes the safe nesting depth far beyond any real program; the parser's own
    // MAX_RECURSION_DEPTH guard rejects the truly pathological case with a clean
    // error before even this stack is exhausted.
    let child = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .expect("failed to spawn compiler thread");
    // Propagate a panic in the worker as a non-zero exit (no double-panic noise).
    if child.join().is_err() {
        std::process::exit(101);
    }
}

fn run() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_help();
        return;
    }

    match args[1].as_str() {
        "new"   => new_project(&args[2..]),
        "build" => build(&args[2..]),
        "docker" => docker_build(&args[2..]),
        "run"   => run_file(&args[2..]),
        "dev"   => dev_mode(&args[2..]),
        "test"  => run_tests(&args[2..]),
        "doc"   => gen_docs(&args[2..]),
        "graph" => gen_call_graph(&args[2..]),
        "check"   => check(&args[2..]),
        "fmt"     => fmt(&args[2..]),
        "repl"    => repl(),
        "install" => {
            if !pm::cmd_install(&args[2..]) {
                std::process::exit(1);
            }
        }
        "add"     => pm::cmd_add(&args[2..]),
        "package" => pm::cmd_package(),
        "publish" => pm::cmd_publish(&args[2..]),
        "search"  => pm::cmd_search(&args[2..]),
        "help" | "--help" | "-h" => print_help(),
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_help();
        }
    }
}

fn print_help() {
    println!("Tinox Compiler v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Usage:");
    println!("  tinox new <name>           Create a new Tinox project");
    println!("  tinox build [file]         Compile to an executable (uses tinox.toml if no file)");
    println!("  tinox docker               Build a minimal Docker image (needs [docker] in tinox.toml)");
    println!("  tinox docker --tag n:t     Build the image under an explicit name:tag");
    println!("  tinox run   [file]         Compile and run (uses tinox.toml if no file)");
    println!("  tinox dev   [file]         Dev mode: hot-reload on file changes");
    println!("  tinox test  [file]         Run all @Test-annotated methods");
    println!("  tinox test --watch         Re-run tests on file changes (TDD mode)");
    println!("  tinox doc   [--open]       Generate HTML documentation in docs/");
    println!("  tinox graph [file]         Generate a Mermaid call-graph diagram in docs/");
    println!("  tinox check [file]         Type-check without compiling");
    println!("  tinox fmt   <file>         Format a Tinox file (print to stdout)");
    println!("  tinox fmt --write <file>   Format a Tinox file in place");
    println!("  tinox repl                 Start interactive REPL");
    println!("  tinox install              Download and install all dependencies");
    println!("  tinox install --update     Re-pin tinox.lock instead of verifying against it");
    println!("  tinox add <g> <a> <v> <u>  Add a dependency and install it");
    println!("  tinox package              Pack src/ into <name>-<version>.tar.gz");
    println!("  tinox publish              Pack and upload to a registry (needs [package] group + TINOX_CENTRAL_ADMIN_KEY)");
    println!("  tinox search <query>       Search a registry's package catalog");
    println!("  tinox help                 Show this help message");
}

/// The scaffolded project's file contents — `(tinox.toml, src/Main.tnx,
/// test class name, tests/{test class name}.tnx)`. Pure/pathless so it's
/// unit-testable without touching the filesystem or CWD (`new_project`
/// below writes these to disk relative to CWD, which isn't safely
/// testable in a parallel test binary).
///
/// Both `src/Main.tnx` (`class Main { fnc main() -> Int32 { ... } }`) and
/// the entry point (`class Main` in a file literally named `Main.tnx`)
/// follow the one-class-per-file rule and the mandatory class-qualified
/// entry point (#149) — a bare top-level `fn main()` (this scaffold's
/// pre-v2.0.0 shape) is now a hard compile error (#155). The test
/// scaffold's file name likewise has to match its `class {name}Tests`
/// declaration (#159).
fn new_project_files(name: &str) -> (String, String, String, String) {
    let toml = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n"
    );
    let main_tnx = format!(
        "class Main\n{{\n    fnc main() -> Int32\n    {{\n        println(\"Hello from {name}!\");\n        return 0;\n    }}\n}}\n"
    );
    let test_class = format!("{name}Tests");
    let test_tnx = format!(
        "class {test_class}\n{{\n    @Test(\"example test\")\n    fn testExample() -> Bool\n    {{\n        return 1 + 1 == 2;\n    }}\n}}\n"
    );
    (toml, main_tnx, test_class, test_tnx)
}

fn new_project(args: &[String]) {
    let name = match args.first() {
        Some(n) => n.clone(),
        None => {
            eprintln!("Error: Project name required. Usage: tinox new <name>");
            return;
        }
    };

    if name.is_empty() || name.contains('/') || name.contains('\\') {
        eprintln!("Error: Invalid project name '{}'", name);
        return;
    }

    let root = PathBuf::from(&name);
    if root.exists() {
        eprintln!("Error: '{}' already exists", name);
        return;
    }

    let src_dir = root.join("src");
    let tests_dir = root.join("tests");

    let create = |path: &PathBuf| -> bool {
        if let Err(e) = fs::create_dir_all(path) {
            eprintln!("Error creating {}: {}", path.display(), e);
            return false;
        }
        true
    };

    let write_file = |path: &PathBuf, content: &str| -> bool {
        if let Err(e) = fs::write(path, content) {
            eprintln!("Error writing {}: {}", path.display(), e);
            return false;
        }
        true
    };

    if !create(&src_dir) || !create(&tests_dir) { return; }

    let (toml, main_tnx, test_class, test_tnx) = new_project_files(&name);
    let gitignore = ".tinox/\n";

    if !write_file(&root.join("tinox.toml"), &toml) { return; }
    if !write_file(&root.join(".gitignore"), gitignore) { return; }
    if !write_file(&src_dir.join("Main.tnx"), &main_tnx) { return; }
    if !write_file(&tests_dir.join(format!("{test_class}.tnx")), &test_tnx) { return; }

    println!("Created project '{name}'");
    println!("  {name}/tinox.toml");
    println!("  {name}/src/Main.tnx");
    println!("  {name}/tests/{test_class}.tnx");
    println!();
    println!("Get started:");
    println!("  cd {name}");
    println!("  tinox run");
}

fn fmt(args: &[String]) {
    let (write_mode, file_arg) = if args.first().map(|s| s.as_str()) == Some("--write") {
        (true, args.get(1))
    } else {
        (false, args.first())
    };

    let input_file = match file_arg {
        Some(f) => f,
        None => {
            eprintln!("Error: No input file specified");
            return;
        }
    };

    let source = match fs::read_to_string(input_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{}': {}", input_file, e);
            return;
        }
    };

    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(errors) => {
            eprintln!("Lex error: {:?}", errors);
            return;
        }
    };

    let mut parser = Parser::new(tokens);
    let ast = match parser.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Parse error: {:?}", e);
            return;
        }
    };

    let mut formatter = Formatter::new();
    let formatted = formatter.format(&ast);

    if write_mode {
        if let Err(e) = fs::write(input_file, &formatted) {
            eprintln!("error: cannot write '{}': {}", input_file, e);
        }
    } else {
        print!("{}", formatted);
    }
}

/// Returns the entry `.tnx` file for the current project.
/// Read `entry` from the nearest `tinox.toml`'s `[package]` section, if present.
fn read_project_entry(content: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package { continue; }
        if let Some(rest) = line.strip_prefix("entry") {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                let entry = rest.trim().trim_matches('"').to_string();
                if !entry.is_empty() {
                    return Some(entry);
                }
            }
        }
    }
    None
}

/// If `args` has a file, use that. Otherwise read tinox.toml → its
/// `[package] entry` field (defaulting to `src/main.tnx` if unset).
fn resolve_entry_file(args: &[String]) -> Option<String> {
    if let Some(f) = args.iter().find(|a| !a.starts_with('-')) {
        return Some(f.clone());
    }
    // Project mode: look for tinox.toml in current dir or parents
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml = dir.join("tinox.toml");
        if toml.exists() {
            let content = fs::read_to_string(&toml).ok()?;
            let entry = read_project_entry(&content).unwrap_or_else(|| "src/main.tnx".to_string());
            let candidate = dir.join(&entry);
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
            eprintln!("error: tinox.toml found but {entry} is missing");
            return None;
        }
        if !dir.pop() { break; }
    }
    eprintln!("error: no input file and no tinox.toml found");
    None
}

/// `--checked`: build the runtime with the heap-kind registry — array/map
/// functions check their pointers and abort loudly on dispatch bugs
/// instead of silently reading garbage. Implemented via TINOX_CFLAGS,
/// which compile_ll_to_exe passes through to both cc invocations.
fn apply_checked_flag(args: &[String]) {
    if args.iter().any(|a| a == "--checked") {
        let mut flags = std::env::var("TINOX_CFLAGS").unwrap_or_default();
        if !flags.contains("-DTINOX_CHECKED") {
            if !flags.is_empty() {
                flags.push(' ');
            }
            flags.push_str("-DTINOX_CHECKED");
            std::env::set_var("TINOX_CFLAGS", flags);
        }
    }
}

fn build(args: &[String]) {
    let release = args.iter().any(|a| a == "--release");
    let debug   = args.iter().any(|a| a == "--debug");
    apply_checked_flag(args);
    let opt = if release { OptLevel::Release } else if debug { OptLevel::Debug } else { OptLevel::Release };

    let input_file = match resolve_entry_file(args) {
        Some(f) => f,
        None => return,
    };
    let output_name = parse_output_flag(args).unwrap_or_else(|| {
        read_project_name().unwrap_or_else(|| {
            Path::new(&input_file).file_stem().unwrap_or_default().to_string_lossy().into_owned()
        })
    });

    match compile_file(&input_file, &output_name, opt) {
        Ok(_) => println!("Compiled successfully: {} ({:?})", output_name, opt),
        Err(e) => {
            eprintln!("Compilation failed: {}", e);
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OptLevel { Release, Debug }

/// Read `[metrics]` section from the nearest `tinox.toml`, if present.
/// Returns `Some(path)` when `enabled = true` is set; `None` otherwise.
fn read_metrics_config() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            let mut in_metrics = false;
            let mut enabled = false;
            let mut path = "/metrics".to_string();
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_metrics = line == "[metrics]";
                    continue;
                }
                if !in_metrics { continue; }
                if let Some(rest) = line.strip_prefix("enabled") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    enabled = rest == "true";
                } else if let Some(rest) = line.strip_prefix("path") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("/metrics");
                    path = rest.trim_matches('"').to_string();
                }
            }
            return if enabled { Some(path) } else { None };
        }
        if !dir.pop() { break; }
    }
    None
}

/// Read `[startup] banner` from the nearest `tinox.toml`, if present.
/// Defaults to `true` (the banner is on by default, matching
/// emit_tinox_main_bootstrap's "systemic, no opt-in" design) -- this is
/// purely an escape hatch for programs that have an auto-run endpoint
/// (so the banner would otherwise fire) but still need clean stdout, e.g.
/// a tool piped into another program. Most plain CLI/script-style
/// programs (jgrep-tinox, ygrep-tinox, ...) never trigger the banner in
/// the first place -- it's gated on having at least one @GET/
/// @WebsocketEndpoint/@Amqp*Consumer/@Http3RestController, which those
/// don't have -- so this setting has no effect on them; it's here for
/// the (currently hypothetical, but plausible) case where such a tool
/// gains one later, without anyone having to remember it.
fn read_startup_banner_config() -> bool {
    let mut dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return true,
    };
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = match fs::read_to_string(&toml_path) {
                Ok(c) => c,
                Err(_) => return true,
            };
            let mut in_startup = false;
            let mut banner = true;
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_startup = line == "[startup]";
                    continue;
                }
                if !in_startup { continue; }
                if let Some(rest) = line.strip_prefix("banner") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("true");
                    banner = rest != "false";
                }
            }
            return banner;
        }
        if !dir.pop() { break; }
    }
    true
}

struct DbConfig {
    driver: String,
    url: String,
    pool: usize,
}

/// Read `[database]` section from the nearest `tinox.toml`, if present.
fn read_database_config() -> Option<DbConfig> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            let mut in_db = false;
            let mut driver = String::new();
            let mut url = String::new();
            // Default pool size when [database] pool is omitted entirely --
            // sized for a small server under real concurrent load, not just
            // "works for a single request at a time" (that's what the old
            // default of 1 amounted to, back when this field was still dead
            // code and every driver shared one single global connection
            // regardless of what it said).
            let mut pool: Option<usize> = None;
            let mut found = false;
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_db = line == "[database]";
                    if in_db { found = true; }
                    continue;
                }
                if !in_db { continue; }
                if let Some(rest) = line.strip_prefix("driver") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    driver = rest.trim_matches('"').to_string();
                } else if let Some(rest) = line.strip_prefix("url") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    url = rest.trim_matches('"').to_string();
                } else if let Some(rest) = line.strip_prefix("pool") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    match rest.parse::<usize>() {
                        Ok(n) if n >= 1 => pool = Some(n),
                        _ => {
                            eprintln!("error: [database] pool must be a positive integer, found '{rest}'");
                            std::process::exit(1);
                        }
                    }
                }
            }
            if found && !driver.is_empty() {
                return Some(DbConfig { driver, url, pool: pool.unwrap_or(5) });
            }
            return None;
        }
        if !dir.pop() { break; }
    }
    None
}

/// Read `name` from the nearest `tinox.toml`, if present.
fn read_project_name() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            for line in content.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("name") {
                    let rest = rest.trim();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let name = rest.trim().trim_matches('"').to_string();
                        if !name.is_empty() {
                            return Some(name);
                        }
                    }
                }
            }
            return None;
        }
        if !dir.pop() { break; }
    }
    None
}

/// Read `version` from the nearest `tinox.toml`, if present. Same
/// deliberately-loose shape as `read_project_name` (first `version =` line
/// in the file, not scoped to `[package]`) -- matches existing convention.
fn read_project_version() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            for line in content.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("version") {
                    let rest = rest.trim();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let version = rest.trim().trim_matches('"').to_string();
                        if !version.is_empty() {
                            return Some(version);
                        }
                    }
                }
            }
            return None;
        }
        if !dir.pop() { break; }
    }
    None
}

/// Container ports the compiled program listens on, plus optional image
/// naming/base overrides -- purely build-time metadata for `tinox docker`.
/// Setting `ports` here doesn't change runtime behavior (the program still
/// binds them itself, e.g. via `HttpServer::new(port)`); it only controls
/// what the generated Dockerfile `EXPOSE`s.
#[derive(Default)]
struct DockerConfig {
    ports: Vec<u16>,
    image: Option<String>,
    base: Option<String>,
    extra_packages: Vec<String>,
}

/// Parse a TOML-ish inline array (`[8080, 9090]` or `["a", "b"]`) into its
/// raw comma-separated elements, with surrounding whitespace and quotes
/// stripped. Not a general TOML parser -- matches the hand-rolled,
/// single-line-value style the rest of this file's tinox.toml readers use.
fn parse_toml_array(rest: &str) -> Vec<String> {
    let rest = rest.trim();
    let inner = rest.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or("");
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Read the `[docker]` section from the nearest `tinox.toml`, if present.
fn read_docker_config() -> Option<DockerConfig> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            let mut in_docker = false;
            let mut found = false;
            let mut cfg = DockerConfig::default();
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_docker = line == "[docker]";
                    if in_docker { found = true; }
                    continue;
                }
                if !in_docker { continue; }
                if let Some(rest) = line.strip_prefix("ports") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    cfg.ports = parse_toml_array(rest).iter().filter_map(|s| s.parse::<u16>().ok()).collect();
                } else if let Some(rest) = line.strip_prefix("extra_packages") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    cfg.extra_packages = parse_toml_array(rest);
                } else if let Some(rest) = line.strip_prefix("image") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    let v = rest.trim_matches('"').to_string();
                    if !v.is_empty() { cfg.image = Some(v); }
                } else if let Some(rest) = line.strip_prefix("base") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    let v = rest.trim_matches('"').to_string();
                    if !v.is_empty() { cfg.base = Some(v); }
                }
            }
            return if found { Some(cfg) } else { None };
        }
        if !dir.pop() { break; }
    }
    None
}

/// Whether to compile in the dev-mode introspection API, and on which port.
/// `enabled = true` alone is sufficient -- respected by `tinox build`/`run`
/// too, not gated behind the `tinox dev` command specifically (see
/// `compile_file`'s companion release-build warning for the safety net that
/// decision needs instead).
struct DevConfig {
    enabled: bool,
    port: u16,
    /// Docker image tag for the tinox-devui dashboard `tinox dev` launches
    /// alongside the compiled program. Defaults to `tinox-devui:latest`
    /// (a locally built image, see the tinox-devui repo's README) --
    /// override once a real registry tag exists.
    devui_image: Option<String>,
}

impl Default for DevConfig {
    fn default() -> Self {
        DevConfig { enabled: false, port: 9090, devui_image: None }
    }
}

/// Read the `[dev]` section from the nearest `tinox.toml`, if present.
fn read_dev_config() -> Option<DevConfig> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml_path = dir.join("tinox.toml");
        if toml_path.exists() {
            let content = fs::read_to_string(&toml_path).ok()?;
            let mut in_dev = false;
            let mut found = false;
            let mut cfg = DevConfig::default();
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_dev = line == "[dev]";
                    if in_dev { found = true; }
                    continue;
                }
                if !in_dev { continue; }
                if let Some(rest) = line.strip_prefix("enabled") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    cfg.enabled = rest == "true";
                } else if let Some(rest) = line.strip_prefix("port") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    if let Ok(p) = rest.parse::<u16>() {
                        cfg.port = p;
                    }
                } else if let Some(rest) = line.strip_prefix("devui_image") {
                    let rest = rest.trim().strip_prefix('=').map(|s| s.trim()).unwrap_or("");
                    let v = rest.trim_matches('"').to_string();
                    if !v.is_empty() { cfg.devui_image = Some(v); }
                }
            }
            return if found { Some(cfg) } else { None };
        }
        if !dir.pop() { break; }
    }
    None
}

/// Escapes `"` and `\` for embedding a Rust string as a JSON string value.
/// Not a general JSON encoder -- just enough for the handful of
/// tinox.toml-derived string values `build_dev_config_summary_json` embeds.
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Converts typecheck's per-route parameter bindings (@PathParam/
/// @QueryParam/@PostParam/@HttpContext) into codegen's own duplicated
/// shape -- same explicit-conversion-at-the-boundary convention already
/// used for `DiScope` right below each of this function's two call sites,
/// factored here since the 4-way kind match is more repetitive than
/// DiScope's 3-way one.
fn convert_route_params(
    params: &[tinox_typecheck::annotations::RouteParamBinding],
) -> Vec<tinox_codegen::RouteParamBinding> {
    params.iter().map(|p| tinox_codegen::RouteParamBinding {
        kind: match p.kind {
            tinox_typecheck::annotations::RouteParamKind::PathParam => tinox_codegen::RouteParamKind::PathParam,
            tinox_typecheck::annotations::RouteParamKind::QueryParam => tinox_codegen::RouteParamKind::QueryParam,
            tinox_typecheck::annotations::RouteParamKind::PostParam => tinox_codegen::RouteParamKind::PostParam,
            tinox_typecheck::annotations::RouteParamKind::HttpContext => tinox_codegen::RouteParamKind::HttpContext,
        },
        name: p.name.clone(),
        ty: p.ty.clone(),
    }).collect()
}

/// Walks up from the current directory to find the nearest `tinox.toml`,
/// returning its containing directory -- same search `read_dev_config`/
/// `read_docker_config`/etc. already do, but returning the directory
/// itself instead of a section's parsed contents (needed for the dev-mode
/// introspection API's `/tests/run`, which needs the actual project root
/// to hand to `tinox test`).
fn find_project_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("tinox.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() { break; }
    }
    None
}

/// Single-quotes `s` for safe embedding in a `/bin/sh -c` command string
/// (the dev-mode introspection API's `/tests/run` uses `popen`, which
/// invokes a shell), escaping any literal `'` as `'\''`. Only ever applied
/// to compiler-controlled paths (the tinox binary's own path, the project
/// root) -- never request input -- but paths can still contain spaces.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The dev-mode introspection API's `/config` endpoint has two halves: this
/// builds the compile-time one (what's configured in tinox.toml), baked as
/// a JSON constant at codegen time; `tinox_config_dump_json` (runtime.c)
/// builds the other (live `application.properties` values) per request.
/// Deliberately omits `[database] url` even though this endpoint only ever
/// binds to 127.0.0.1 -- connection strings routinely carry credentials,
/// and there's no reason to bake those into the compiled binary's constant
/// pool just to answer "what database am I using".
fn build_dev_config_summary_json() -> String {
    let docker = read_docker_config();
    let db = read_database_config();
    let metrics = read_metrics_config();
    let startup_banner = read_startup_banner_config();

    let docker_json = match docker {
        Some(d) => format!(
            "{{\"enabled\":true,\"ports\":[{}]}}",
            d.ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",")
        ),
        None => "{\"enabled\":false}".to_string(),
    };
    let db_json = match db {
        Some(d) => format!("{{\"enabled\":true,\"driver\":\"{}\"}}", json_escape(&d.driver)),
        None => "{\"enabled\":false}".to_string(),
    };
    let metrics_json = match metrics {
        Some(path) => format!("{{\"enabled\":true,\"path\":\"{}\"}}", json_escape(&path)),
        None => "{\"enabled\":false}".to_string(),
    };

    format!(
        "{{\"docker\":{docker_json},\"database\":{db_json},\"metrics\":{metrics_json},\"startupBanner\":{startup_banner}}}"
    )
}

/// Debian/Ubuntu runtime packages for the shared libraries `compile_ll_to_exe`
/// links against, given the same feature flags that decide what it links.
/// Pure so it's directly unit-testable; `docker_runtime_packages` below
/// gathers the (env-var- and tinox.toml-dependent) inputs.
fn compute_runtime_packages(tls_enabled: bool, db_driver: Option<&str>, extra: &[String]) -> Vec<String> {
    // libgc1: Boehm GC (-lgc). zlib1g: WebSocket permessage-deflate (-lz).
    // Both linked unconditionally, same as in compile_ll_to_exe. libm/
    // libpthread need no separate package -- part of glibc (libc6).
    let mut pkgs = vec!["libgc1".to_string(), "zlib1g".to_string(), "ca-certificates".to_string()];
    if tls_enabled {
        pkgs.push("libssl3".to_string());
    }
    match db_driver {
        Some("postgres") => pkgs.push("libpq5".to_string()),
        Some("mysql") => pkgs.push("libmariadb3".to_string()),
        Some("sqlite") => pkgs.push("libsqlite3-0".to_string()),
        _ => {}
    }
    for p in extra {
        if !pkgs.contains(p) {
            pkgs.push(p.clone());
        }
    }
    pkgs
}

/// Reads the same env vars / tinox.toml `[database]` section
/// `compile_ll_to_exe` reads to decide what to link, so the Docker image's
/// installed packages track the actual binary instead of guessing.
fn docker_runtime_packages(extra: &[String]) -> Vec<String> {
    let tls_enabled = std::env::var("TINOX_TLS").map(|v| v != "0" && v != "false").unwrap_or(true);
    let http3_enabled = tls_enabled
        && std::env::var("TINOX_HTTP3").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    if http3_enabled {
        eprintln!(
            "warning: TINOX_HTTP3 is set, but ngtcp2/nghttp3 runtime package names vary by distro \
             and aren't auto-provisioned here -- add them via [docker] extra_packages in tinox.toml, \
             or the post-build library check below will fail with a clear error instead of shipping \
             a broken image."
        );
    }
    let db_driver = read_database_config();
    compute_runtime_packages(tls_enabled, db_driver.as_ref().map(|d| d.driver.as_str()), extra)
}

/// Renders the (minimal, single-stage) Dockerfile: install just the
/// runtime shared libraries the binary needs, copy it in, EXPOSE the
/// configured ports, run it directly as the container's entrypoint.
fn generate_dockerfile(base: &str, packages: &[String], binary_name: &str, ports: &[u16]) -> String {
    let mut s = String::new();
    s.push_str(&format!("FROM {base}\n\n"));
    if !packages.is_empty() {
        s.push_str("RUN apt-get update && apt-get install -y --no-install-recommends \\\n");
        for pkg in packages {
            // Every package line continues (there's always a trailing
            // `&& rm -rf ...` line after the loop), unlike a plain
            // comma/newline-joined list where only the interior separators
            // need it.
            s.push_str(&format!("    {pkg} \\\n"));
        }
        s.push_str("    && rm -rf /var/lib/apt/lists/*\n\n");
    }
    s.push_str("WORKDIR /app\n");
    s.push_str(&format!("COPY {binary_name} /app/{binary_name}\n"));
    s.push_str(&format!("RUN chmod +x /app/{binary_name}\n\n"));
    if !ports.is_empty() {
        let port_list = ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
        s.push_str(&format!("EXPOSE {port_list}\n\n"));
    }
    s.push_str(&format!("ENTRYPOINT [\"/app/{binary_name}\"]\n"));
    s
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

/// `tinox docker [--tag name:tag] [--debug]` -- compiles the project (same
/// pipeline as `tinox build`, Release by default) and packages the result
/// into a minimal Docker image: install only the runtime shared libraries
/// actually linked, COPY the binary in, EXPOSE the `[docker] ports` from
/// tinox.toml, run it as the entrypoint.
///
/// The binary is compiled on the host and copied into the image rather
/// than rebuilt inside a container, so it must run under the base image's
/// glibc (older host glibc than the image's is fine; newer generally isn't).
/// Rather than silently shipping a binary that fails at container start,
/// the build ends with an `ldd` check inside the freshly built image and
/// hard-fails with the exact missing library if anything doesn't resolve.
fn docker_build(args: &[String]) {
    let docker_available = Command::new("docker")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !docker_available {
        eprintln!("error: `docker` not found on PATH -- install Docker to use `tinox docker`");
        std::process::exit(1);
    }

    let debug = args.iter().any(|a| a == "--debug");
    let opt = if debug { OptLevel::Debug } else { OptLevel::Release };
    let tag_override = parse_flag_value(args, "--tag");

    // Project mode only -- the [docker] section this command reads only
    // exists in a tinox.toml, so (unlike `build`/`run`) there's no bare-file
    // mode to fall back to.
    let input_file = match resolve_entry_file(&[]) {
        Some(f) => f,
        None => return,
    };

    let cfg = read_docker_config().unwrap_or_default();
    if cfg.ports.is_empty() {
        eprintln!("note: no [docker] ports configured in tinox.toml -- image will not EXPOSE any ports");
    }

    let project_name = read_project_name().unwrap_or_else(|| "app".to_string());
    let binary_name = project_name.clone();
    // Docker image names must be lowercase; only lowercase the name we
    // derive ourselves -- an explicit `image =`/`--tag` is used verbatim.
    let image_ref = tag_override.unwrap_or_else(|| {
        let image_name = cfg.image.clone().unwrap_or_else(|| project_name.to_lowercase());
        format!("{image_name}:latest")
    });
    // debian:trixie-slim (current Debian stable, glibc 2.41) rather than
    // the older bookworm-slim (glibc 2.36) -- bookworm's glibc proved too
    // old in practice for a host-compiled binary from any reasonably
    // current dev machine (Arch, recent Ubuntu/Fedora, ...), tripping the
    // ldd check below on the very first real-world run. Still fully
    // overridable via `[docker] base`.
    let base_image = cfg.base.clone().unwrap_or_else(|| "debian:trixie-slim".to_string());

    println!("Compiling {binary_name} ({opt:?})...");
    if let Err(e) = compile_file(&input_file, &binary_name, opt) {
        eprintln!("Compilation failed: {}", e);
        std::process::exit(1);
    }

    let build_dir = Path::new(".tinox").join("docker");
    if let Err(e) = fs::create_dir_all(&build_dir) {
        eprintln!("error: cannot create {}: {}", build_dir.display(), e);
        std::process::exit(1);
    }
    let staged_binary = build_dir.join(&binary_name);
    if let Err(e) = fs::copy(&binary_name, &staged_binary) {
        eprintln!("error: cannot stage compiled binary into {}: {}", build_dir.display(), e);
        std::process::exit(1);
    }

    let packages = docker_runtime_packages(&cfg.extra_packages);
    let dockerfile = generate_dockerfile(&base_image, &packages, &binary_name, &cfg.ports);
    let dockerfile_path = build_dir.join("Dockerfile");
    if let Err(e) = fs::write(&dockerfile_path, &dockerfile) {
        eprintln!("error: cannot write {}: {}", dockerfile_path.display(), e);
        std::process::exit(1);
    }

    println!("Building image {image_ref} from {base_image}...");
    let build_status = Command::new("docker")
        .arg("build")
        .arg("-t").arg(&image_ref)
        .arg("-f").arg(&dockerfile_path)
        .arg(&build_dir)
        .status();
    match build_status {
        Ok(s) if s.success() => {}
        Ok(_) => {
            eprintln!("error: `docker build` failed");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: failed to run docker: {}", e);
            std::process::exit(1);
        }
    }

    print!("Verifying linked libraries resolve inside the image... ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let ldd_out = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "ldd"])
        .arg(&image_ref)
        .arg(format!("/app/{binary_name}"))
        .output();
    match ldd_out {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let missing: Vec<String> = stdout.lines().filter(|l| l.contains("not found")).map(|l| l.trim().to_string()).collect();
            if !missing.is_empty() {
                println!("FAILED");
                eprintln!("error: image {image_ref} is missing shared libraries the compiled binary needs:");
                for m in &missing {
                    eprintln!("  {}", m);
                }
                eprintln!(
                    "Add the missing package(s) via [docker] extra_packages in tinox.toml \
                     (base image: {base_image}), then run `tinox docker` again."
                );
                std::process::exit(1);
            }
            println!("ok");
        }
        Err(e) => {
            println!("skipped ({e})");
        }
    }

    println!("Built {image_ref}");
    println!("Run it with:");
    if cfg.ports.is_empty() {
        println!("  docker run --rm {image_ref}");
    } else {
        let publish_flags: String = cfg.ports.iter().map(|p| format!("-p {p}:{p}")).collect::<Vec<_>>().join(" ");
        println!("  docker run --rm {publish_flags} {image_ref}");
    }
}

fn parse_output_flag(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

fn repl() {
    use std::io::{self, BufRead, Write};

    println!("  Tinox REPL v{}", env!("CARGO_PKG_VERSION"));
    println!("  Type Tinox expressions or declarations. Empty line = evaluate.");
    println!("  :quit  to exit   :clear  to reset session   :help  for commands");
    println!();

    // Accumulated class/function declarations across REPL turns
    let mut session_decls = String::new();
    // Input accumulator for multi-line blocks
    let mut input_buf = String::new();
    let mut line_no: usize = 0;

    let stdin = io::stdin();
    loop {
        let prompt = if input_buf.is_empty() { ">>> " } else { "... " };
        print!("{}", prompt);
        io::stdout().flush().ok();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => { eprintln!("read error: {}", e); break; }
        }
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');

        // REPL commands
        match trimmed {
            ":quit" | ":q" | ":exit" => break,
            ":clear" | ":reset" => {
                session_decls.clear();
                input_buf.clear();
                line_no = 0;
                println!("  Session cleared.");
                continue;
            }
            ":help" => {
                println!("  Commands:");
                println!("    :quit / :q     Exit the REPL");
                println!("    :clear         Reset session (forget all declarations)");
                println!("    :session       Show accumulated declarations");
                println!("    :help          Show this message");
                println!();
                println!("  Tinox REPL tips:");
                println!("    • Enter expressions to evaluate them");
                println!("    • Declare functions and classes; they persist across entries");
                println!("    • Multi-line: keep typing — empty line submits");
                continue;
            }
            ":session" => {
                if session_decls.is_empty() {
                    println!("  (empty session)");
                } else {
                    println!("{}", session_decls);
                }
                continue;
            }
            _ => {}
        }

        input_buf.push_str(trimmed);
        input_buf.push('\n');

        let open_braces = input_buf.chars().filter(|&c| c == '{').count();
        let close_braces = input_buf.chars().filter(|&c| c == '}').count();
        let is_empty_submit = trimmed.is_empty() && !input_buf.trim().is_empty();
        let has_open_brace = open_braces > 0;
        // For block constructs (fn, class, etc.): submit only when braces are fully balanced
        // and at least one closing brace has been seen.
        // For simple expressions (no braces): submit immediately.
        let is_complete = if has_open_brace {
            open_braces == close_braces
        } else {
            // No braces yet: submit only if the line doesn't look like it needs more input
            let first = input_buf.split_whitespace().next().unwrap_or("");
            !matches!(first, "fn" | "class" | "interface" | "enum" | "trait"
                           | "if" | "while" | "for" | "loop"
                           | "let" | "var") // let/var need explicit submit (empty line)
        };

        if !is_empty_submit && !is_complete {
            continue;
        }

        let entry = input_buf.trim().to_string();
        input_buf.clear();

        if entry.is_empty() { continue; }

        line_no += 1;
        repl_eval(&entry, &mut session_decls, line_no);
    }

    println!("Bye!");
}

/// Evaluate one REPL entry: either a declaration (saved to session) or an expression (printed).
fn repl_eval(entry: &str, session_decls: &mut String, turn: usize) {
    // Detect declarations: starts with fn, class, interface, enum, let, var at top level
    let first_token = entry.split_whitespace().next().unwrap_or("");
    // Only top-level structural declarations go into session_decls.
    // let/var are statements (must live inside a function body).
    let is_decl = matches!(first_token,
        "fn" | "class" | "interface" | "enum" | "trait" | "import"
    ) || entry.starts_with('@'); // annotations precede class/fn

    if is_decl {
        // Try to parse and type-check the new declaration
        let combined = format!("{}\n{}", session_decls, entry);
        let tokens = match Lexer::new(&combined).tokenize() {
            Ok(t) => t,
            Err(errs) => {
                for e in &errs { eprintln!("error: {}", e.message); }
                return;
            }
        };
        match tinox_parser::Parser::new(tokens).parse() {
            Ok(_) => {
                session_decls.push_str(entry);
                session_decls.push('\n');
                println!("  defined.");
            }
            Err(bag) => {
                for e in &bag.errors { eprintln!("error: {}", e.message); }
            }
        }
        return;
    }

    // Detect if this is a statement block (contains ; or multiple lines or let/var)
    let lines: Vec<&str> = entry.lines().filter(|l| !l.trim().is_empty()).collect();
    let has_semicolons = entry.contains(';');
    let is_multi_line = lines.len() > 1;
    let starts_with_stmt = matches!(first_token, "let" | "var" | "println" | "print" | "return");

    let is_stmt_block = is_multi_line || has_semicolons || starts_with_stmt;

    if is_stmt_block {
        // Ensure each statement line ends with ; (normalize)
        let body: String = entry.lines()
            .map(|l| {
                let t = l.trim_end();
                if t.is_empty() || t.ends_with(';') || t.ends_with('{') || t.ends_with('}') {
                    l.to_string()
                } else {
                    format!("{};", l)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let src = format!(
            "{}\nfn __repl_{}() -> Int64 {{\n{}\n    return 0;\n}}\nfn main() -> Int64 {{\n    __repl_{}();\n    return 0;\n}}\n",
            session_decls, turn, body, turn
        );
        match repl_compile_and_run(&src, turn) {
            Ok(output) => {
                if !output.is_empty() {
                    print!("{}", output);
                    if !output.ends_with('\n') { println!(); }
                }
            }
            Err(msg) => eprintln!("error: {}", msg),
        }
        return;
    }

    // Single expression: try to print the value via println()
    let expr_clean = entry.trim_end_matches(';').trim();
    let src = format!(
        "{}\nfn __repl_{}() -> Int64 {{\n    println({});\n    return 0;\n}}\nfn main() -> Int64 {{\n    __repl_{}();\n    return 0;\n}}\n",
        session_decls, turn, expr_clean, turn
    );

    let result = repl_compile_and_run(&src, turn);
    match result {
        Ok(output) => {
            if !output.is_empty() {
                print!("{}", output);
                if !output.ends_with('\n') { println!(); }
            }
        }
        Err(msg) => {
            // Fallback: run as a void statement (e.g. method calls that return Nothing)
            let src2 = format!(
                "{}\nfn __repl_{}() -> Int64 {{\n    {};\n    return 0;\n}}\nfn main() -> Int64 {{\n    __repl_{}();\n    return 0;\n}}\n",
                session_decls, turn, expr_clean, turn
            );
            match repl_compile_and_run(&src2, turn) {
                Ok(output) => {
                    if !output.is_empty() {
                        print!("{}", output);
                        if !output.ends_with('\n') { println!(); }
                    }
                }
                Err(_) => eprintln!("error: {}", msg),
            }
        }
    }
}

fn repl_compile_and_run(src: &str, turn: usize) -> Result<String, String> {
    // Lex + parse + codegen
    let tokens = Lexer::new(src).tokenize()
        .map_err(|errs| errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; "))?;

    let ast = tinox_parser::Parser::new(tokens).parse()
        .map_err(|bag| bag.errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; "))?;

    let mut cg = CodeGen::new();
    cg.gen(&ast).map_err(|e| format!("{:?}", e))?;
    let ir = cg.into_ir();

    // Write to a temp file and compile
    let tmp_base = format!("/tmp/.tinox_repl_{}", turn);
    let ir_path = format!("{}.ll", tmp_base);
    fs::write(&ir_path, &ir)
        .map_err(|e| format!("write IR: {}", e))?;

    let runtime_obj = find_runtime_object();

    // Compile to executable
    let exe = format!("{}.out", tmp_base);
    let mut cmd = Command::new("clang");
    cmd.arg(&ir_path).arg("-o").arg(&exe).arg("-O0").arg("-lm").arg("-lgc").arg("-lz");
    if let Some(ref rt) = runtime_obj {
        cmd.arg(rt);
    }

    let out = cmd.output()
        .map_err(|e| format!("clang: {}", e))?;
    if !out.status.success() {
        let _ = fs::remove_file(&ir_path);
        return Err(String::from_utf8_lossy(&out.stderr)
            .lines().take(3).collect::<Vec<_>>().join("; "));
    }

    // Run
    let run_out = Command::new(&exe).output()
        .map_err(|e| format!("run: {}", e))?;

    let _ = fs::remove_file(&ir_path);
    let _ = fs::remove_file(&exe);

    if !run_out.status.success() {
        return Err(format!("exited with {}", run_out.status));
    }

    Ok(String::from_utf8_lossy(&run_out.stdout).to_string())
}

fn find_runtime_object() -> Option<String> {
    // Try common locations for the precompiled runtime.o
    let candidates = [
        "runtime/runtime.o",
        "../runtime/runtime.o",
        "runtime.o",
    ];
    for c in &candidates {
        if Path::new(c).exists() { return Some(c.to_string()); }
    }
    // If runtime.c exists but not runtime.o, compile it on the fly
    let c_candidates = [
        "runtime/runtime.c",
        "../runtime/runtime.c",
        "runtime.c",
    ];
    for c in &c_candidates {
        if Path::new(c).exists() {
            let obj = "/tmp/.tinox_runtime.o";
            let status = Command::new("clang")
                .args(["-c", c, "-o", obj, "-O3"])
                .status().ok()?;
            if status.success() { return Some(obj.to_string()); }
        }
    }
    // Same dev/system resolution used by the main build path (compile_ll_to_exe).
    if let Some(c) = runtime_c_path() {
        let obj = "/tmp/.tinox_runtime.o";
        let status = Command::new("clang")
            .args(["-c", &c.to_string_lossy(), "-o", obj, "-O3"])
            .status().ok()?;
        if status.success() { return Some(obj.to_string()); }
    }
    None
}

fn run_file(args: &[String]) {
    let input_file = match resolve_entry_file(args) {
        Some(f) => f,
        None => return,
    };
    let exe_name = format!(".tinox_tmp_{}", std::process::id());

    let opt = if args.iter().any(|a| a == "--debug") { OptLevel::Debug } else { OptLevel::Release };
    apply_checked_flag(args);
    match compile_file(&input_file, &exe_name, opt) {
        Ok(_) => {
            let status = Command::new(format!("./{}", exe_name))
                .status()
                .expect("Failed to run executable");

            let _ = fs::remove_file(&exe_name);
            let _ = fs::remove_file(format!("{}.ll", exe_name));

            std::process::exit(status.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("Compilation failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn print_dev_banner(watching: &str) {
    eprintln!();
    eprintln!("  ████████╗██╗███╗   ██╗ ██████╗ ██╗  ██╗");
    eprintln!("     ██╔══╝██║████╗  ██║██╔═══██╗╚██╗██╔╝");
    eprintln!("     ██║   ██║██╔██╗ ██║██║   ██║ ╚███╔╝ ");
    eprintln!("     ██║   ██║██║╚██╗██║██║   ██║ ██╔██╗ ");
    eprintln!("     ██║   ██║██║ ╚████║╚██████╔╝██╔╝ ██╗");
    eprintln!("     ╚═╝   ╚═╝╚═╝  ╚═══╝ ╚═════╝ ╚═╝  ╚═╝");
    eprintln!();
    eprintln!("  :: Dev Mode ::  (v{})", env!("CARGO_PKG_VERSION"));
    eprintln!("  Watching : {}", watching);
    eprintln!("  Stop     : Ctrl+C");
    eprintln!("  ─────────────────────────────────────────");
    eprintln!();
}

/// Launches the tinox-devui dashboard as a Docker container alongside a
/// `tinox dev`-run program with `[dev] enabled = true` -- reuses the
/// "shell out to docker" pattern `docker_build` already established.
/// `--network host` (Linux-only, matches this whole toolchain's target)
/// lets the container reach the loopback-only introspection API on the
/// host's 127.0.0.1 directly, no `host.docker.internal` needed. A soft
/// failure by design: if docker isn't installed or the image can't be
/// pulled/found, `tinox dev` still runs the actual program fine, just
/// without the dashboard -- printed as a warning, not a hard error.
fn launch_devui_container(cfg: &DevConfig) -> Option<String> {
    let docker_available = Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !docker_available {
        eprintln!("[dev] docker not found -- skipping the tinox-devui dashboard");
        return None;
    }

    // Default is the real published image (ghcr.io/subnix-work/tinox-devui,
    // pushed by that repo's own `.github/workflows/publish.yml` on every
    // `vX.Y.Z` tag) -- `docker run` pulls it automatically on a machine
    // that's never built it locally. Override to a local `tinox-devui:latest`
    // (or any other tag) via `[dev] devui_image` in tinox.toml when
    // developing the dashboard itself.
    let image = cfg.devui_image.clone()
        .unwrap_or_else(|| "ghcr.io/subnix-work/tinox-devui:latest".to_string());
    let container_name = format!("tinox-devui-{}", std::process::id());
    let app_url = format!("http://127.0.0.1:{}", cfg.port);

    let status = Command::new("docker")
        .args([
            "run", "-d", "--rm", "--network", "host",
            "--name", &container_name,
            "-e", &format!("TINOX_APP_URL={app_url}"),
            &image,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => {
            // Fixed -- matches tinox-devui's own application.properties
            // (quarkus.http.port=9091), reached directly via --network host.
            let dashboard_url = "http://localhost:9091";
            println!("[dev] tinox-devui dashboard: {dashboard_url}");
            let _ = Command::new("xdg-open").arg(dashboard_url).spawn()
                .or_else(|_| Command::new("open").arg(dashboard_url).spawn());
            Some(container_name)
        }
        _ => {
            eprintln!(
                "[dev] failed to start the tinox-devui container (image '{image}' missing? \
                 build it from the tinox-devui repo: docker build -t {image} .)"
            );
            None
        }
    }
}

fn stop_devui_container(name: &str) {
    let _ = Command::new("docker")
        .args(["stop", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn dev_mode(args: &[String]) {
    use notify::{RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    let input_file = match resolve_entry_file(args) {
        Some(f) => f,
        None => return,
    };
    let exe_name = format!(".tinox_dev_{}", std::process::id());

    print_dev_banner(&input_file);

    // Arc<Mutex<...>> instead of a plain local, because the Ctrl-C handler
    // below (a separate thread, see ctrlc::set_handler) needs to reach the
    // same child process and devui container name to clean them up --
    // without this, hitting Ctrl-C (the ordinary way to exit `tinox dev`)
    // bypasses every cleanup step after the watch loop entirely: SIGINT's
    // default disposition kills the process immediately, and neither the
    // compiled child (still holding its port) nor a `[dev]`-launched
    // tinox-devui container (not in this process's group, so it's NOT
    // killed by the terminal's own SIGINT delivery the way the child is)
    // would ever get cleaned up otherwise.
    let child: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
    let devui_container: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let compile_and_run = {
        let child = Arc::clone(&child);
        let input_file = input_file.clone();
        let exe_name = exe_name.clone();
        move || {
            {
                let mut guard = child.lock().unwrap();
                if let Some(ref mut c) = *guard {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                *guard = None;
            }

            eprint!("[dev] compiling... ");
            match compile_file(&input_file, &exe_name, OptLevel::Debug) {
                Ok(_) => {
                    eprintln!("ok");
                    match Command::new(format!("./{}", exe_name)).spawn() {
                        Ok(c) => {
                            eprintln!("[dev] started (pid {})", c.id());
                            *child.lock().unwrap() = Some(c);
                        }
                        Err(e) => eprintln!("[dev] launch failed: {}", e),
                    }
                }
                Err(e) => eprintln!("[dev] compile error:\n{}", e),
            }
        }
    };

    compile_and_run();

    // Launched once, outside compile_and_run's rebuild cycle: the
    // dashboard container talks to the introspection API over HTTP, so it
    // doesn't need restarting when the compiled program itself is rebuilt
    // (a fresh curl/reconnect on the devui side sees the new routes fine).
    if let Some(cfg) = read_dev_config().filter(|cfg| cfg.enabled) {
        *devui_container.lock().unwrap() = launch_devui_container(&cfg);
    }

    let cleanup = {
        let child = Arc::clone(&child);
        let devui_container = Arc::clone(&devui_container);
        let exe_name = exe_name.clone();
        move || {
            if let Some(ref mut c) = *child.lock().unwrap() {
                let _ = c.kill();
                let _ = c.wait();
            }
            let _ = fs::remove_file(&exe_name);
            let _ = fs::remove_file(format!("{}.ll", exe_name));
            if let Some(ref name) = *devui_container.lock().unwrap() {
                stop_devui_container(name);
            }
        }
    };

    {
        let cleanup = cleanup.clone();
        let _ = ctrlc::set_handler(move || {
            cleanup();
            std::process::exit(0);
        });
    }

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .expect("Failed to create file watcher");

    let watch_dir = Path::new(&input_file)
        .parent()
        .unwrap_or(Path::new("."));
    watcher
        .watch(watch_dir, RecursiveMode::Recursive)
        .expect("Failed to watch directory");

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let is_tnx = event.paths.iter().any(|p| {
                    p.extension().map(|e| e == "tnx").unwrap_or(false)
                });
                if is_tnx {
                    eprintln!("[dev] change detected — rebuilding...");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    while rx.try_recv().is_ok() {}
                    compile_and_run();
                }
            }
            Ok(Err(e)) => eprintln!("[dev] watcher error: {}", e),
            Err(_) => break,
        }
    }

    cleanup();
}

fn check(args: &[String]) {
    let input_file = match resolve_entry_file(args) {
        Some(f) => f,
        None => std::process::exit(1),
    };
    let source = match fs::read_to_string(&input_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{}': {}", input_file, e);
            std::process::exit(1);
        }
    };

    let lines: Vec<&str> = source.lines().collect();

    // Lex
    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(errors) => {
            for e in &errors {
                print_error(&input_file, &lines, e.span, &e.message);
            }
            eprintln!("\naborting: {} error{}", errors.len(), if errors.len() == 1 { "" } else { "s" });
            std::process::exit(1);
        }
    };

    // Parse
    let mut parser = tinox_parser::Parser::new(tokens);
    let mut ast = match parser.parse() {
        Ok(a) => a,
        Err(bag) => {
            let count = bag.errors.len();
            for e in &bag.errors {
                print_error(&input_file, &lines, e.span, &e.message);
            }
            eprintln!("\naborting: {} error{}", count, if count == 1 { "" } else { "s" });
            std::process::exit(1);
        }
    };
    if let Err(e) = check_one_type_per_file(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_no_top_level_fn(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_namespace_path_matches(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    // Resolve imports
    let base_dir = Path::new(&input_file)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut visited = HashSet::new();
    if let Ok(canonical) = Path::new(&input_file).canonicalize() {
        visited.insert(canonical);
    }
    let (dep_dirs, missing_deps) = load_dep_dirs(&base_dir);
    if let Err(e) = resolve_imports(&mut ast, &base_dir, &mut visited, &dep_dirs, &missing_deps) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_explicit_imports(Path::new(&input_file), &dep_dirs, &missing_deps) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    // Assign node ids before type-checking so infer_type's memoization is active
    // (Bug 50) — without ids every sub-expression is re-inferred, making deep
    // method chains exponential. The build path already does this before check.
    tinox_parser::assign_node_ids(&mut ast);

    // Type-check
    let mut typechecker = tinox_typecheck::TypeChecker::new();
    match typechecker.check(&ast) {
        Ok(_) => {
            // Also run annotation processing for check mode
            let ann_result = tinox_typecheck::annotations::process_annotations(&ast);
            for warning in &ann_result.deprecated_warnings {
                eprintln!("warning: {}", warning);
            }
            for route in &ann_result.route_entries {
                eprintln!("  route: {} {} -> {}.{}", route.method, route.path, route.class_name, route.method_name);
            }
            println!("{}: no errors", input_file);
            std::process::exit(0);
        }
        Err(bag) => {
            let count = bag.errors.len();
            for e in &bag.errors {
                print_error(&input_file, &lines, e.span, &e.message);
            }
            eprintln!("\n{} error{} found", count, if count == 1 { "" } else { "s" });
            std::process::exit(1);
        }
    }
}

fn print_error(file: &str, lines: &[&str], span: tinox_common::Span, message: &str) {
    let line = span.start.line as usize;
    let col = span.start.column as usize;
    eprintln!("{}:{}:{}: error: {}", file, line, col, message);
    if line > 0 && line <= lines.len() {
        let src_line = lines[line - 1];
        eprintln!("{:>4} | {}", line, src_line);
        let padding = " ".repeat(col.saturating_sub(1));
        eprintln!("     | {}^", padding);
    }
}

fn run_tests(args: &[String]) {
    let watch_mode = args.iter().any(|a| a == "--watch" || a == "-w");
    let filtered_args: Vec<String> = args.iter()
        .filter(|a| *a != "--watch" && *a != "-w")
        .cloned()
        .collect();

    if watch_mode {
        run_tests_watch(&filtered_args);
        return;
    }
    run_tests_once(&filtered_args);
}

fn run_tests_watch(args: &[String]) {
    use notify::{RecursiveMode, Watcher};
    use std::sync::mpsc;

    let source_files = collect_test_files(args);
    if source_files.is_empty() { return; }

    eprintln!();
    eprintln!("  Tinox Test Watch");
    eprintln!("  ─────────────────────────────────────────");
    eprintln!("  Press Ctrl+C to stop");
    eprintln!();

    run_tests_once(args);

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| { let _ = tx.send(res); })
        .expect("watcher");

    // Watch every directory that contains a test or source file
    let mut watched_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for f in &source_files {
        if let Some(parent) = Path::new(f).parent() {
            let dir = parent.to_path_buf();
            if watched_dirs.insert(dir.clone()) {
                let _ = watcher.watch(&dir, RecursiveMode::Recursive);
            }
        }
    }
    // Also watch src/
    if let Ok(cwd) = std::env::current_dir() {
        let src = cwd.join("src");
        if src.is_dir() && watched_dirs.insert(src.clone()) {
            let _ = watcher.watch(&src, RecursiveMode::Recursive);
        }
    }

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let is_tnx = event.paths.iter().any(|p| {
                    p.extension().map(|e| e == "tnx").unwrap_or(false)
                });
                if is_tnx {
                    eprintln!("\n[watch] change detected — re-running tests...");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    while rx.try_recv().is_ok() {}
                    run_tests_once(args);
                }
            }
            Ok(Err(e)) => eprintln!("[watch] error: {}", e),
            Err(_) => break,
        }
    }
}

fn collect_test_files(args: &[String]) -> Vec<String> {
    if let Some(f) = args.first().filter(|a| !a.starts_with('-')) {
        return vec![f.clone()];
    }
    let mut files = Vec::new();
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        if dir.join("tinox.toml").exists() {
            for sub in &["tests", "src"] {
                let d = dir.join(sub);
                if d.is_dir() {
                    if let Ok(entries) = fs::read_dir(&d) {
                        for entry in entries.flatten() {
                            let p = entry.path();
                            if p.extension().map(|e| e == "tnx").unwrap_or(false) {
                                files.push(p.to_string_lossy().into_owned());
                            }
                        }
                    }
                }
            }
            break;
        }
        if !dir.pop() { break; }
    }
    files
}

fn run_tests_once(args: &[String]) {
    let source_files = collect_test_files(args);
    if source_files.is_empty() {
        eprintln!("error: no test files found — run from a project directory or pass a file");
        return;
    }

    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;

    for source_path in &source_files {
        let test_entries = match collect_tests(source_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error processing {}: {}", source_path, e);
                continue;
            }
        };
        if test_entries.is_empty() { continue; }

        println!("Running {} test{} from {}",
            test_entries.len(),
            if test_entries.len() == 1 { "" } else { "s" },
            source_path);

        for t in &test_entries {
            total += 1;
            let exe = format!(".tinox_test_{}_{}", std::process::id(), total);
            let result = compile_test_exe(source_path, &t.class_name, &t.method_name, &exe);
            if let Err(e) = result {
                println!("  FAIL  {} — compile error: {}", t.description, e);
                failed += 1;
                continue;
            }
            let status = Command::new(format!("./{exe}")).status();
            let _ = fs::remove_file(&exe);
            let _ = fs::remove_file(format!("{exe}.ll"));
            match status {
                Ok(s) if s.code() == Some(0) => {
                    println!("  PASS  {}", t.description);
                    passed += 1;
                }
                Ok(s) => {
                    println!("  FAIL  {} — exit code {}", t.description, s.code().unwrap_or(-1));
                    failed += 1;
                }
                Err(e) => {
                    println!("  FAIL  {} — {}", t.description, e);
                    failed += 1;
                }
            }
        }
    }

    println!();
    println!("{total} test{} — {passed} passed, {failed} failed",
        if total == 1 { "" } else { "s" });

    if failed > 0 {
        std::process::exit(1);
    }
}

// ─── tinox doc ────────────────────────────────────────────────────────────────

/// Recursively collects every .tnx file under `dir` — a multi-file module
/// (e.g. tinox-core's `websocket/` with Ws.tnx/WsClient.tnx/WsFrame.tnx/
/// WsServer.tnx as siblings) needs all of them merged into one doc page,
/// not just whichever file happens to sort first.
fn collect_tnx_files_for_docs(dir: &Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_tnx_files_for_docs(&path, out);
        } else if path.extension().map(|x| x == "tnx").unwrap_or(false) {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}

fn gen_docs(args: &[String]) {
    let open = args.iter().any(|a| a == "--open");
    let out_override: Option<&String> = args.iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1));

    // Collect source files from project or explicit arg, and remember the
    // project root along the way — description/dependencies/examples below
    // all read from files relative to it, same as tinox.toml itself.
    let mut project_root: Option<PathBuf> = None;
    let source_files: Vec<String> = if let Some(f) = args.first().filter(|a| !a.starts_with('-')) {
        vec![f.clone()]
    } else {
        let mut files = Vec::new();
        let mut dir = std::env::current_dir().unwrap_or_default();
        loop {
            if dir.join("tinox.toml").exists() {
                let src = dir.join("src");
                if src.is_dir() {
                    collect_tnx_files_for_docs(&src, &mut files);
                }
                project_root = Some(dir);
                break;
            }
            if !dir.pop() { break; }
        }
        files
    };

    if source_files.is_empty() {
        eprintln!("error: no source files found");
        return;
    }

    let project_name = read_project_name().unwrap_or_else(|| "Tinox Project".to_string());
    let mut doc_items: Vec<DocItem> = Vec::new();

    for path in &source_files {
        let src = match fs::read_to_string(path) { Ok(s) => s, Err(_) => continue };
        let mut lexer = Lexer::new(&src);
        let tokens = match lexer.tokenize() { Ok(t) => t, Err(_) => continue };
        let mut parser = tinox_parser::Parser::new(tokens);
        let ast = match parser.parse() { Ok(a) => a, Err(_) => continue };

        for decl in &ast.decls {
            collect_doc_items(&decl.node, &mut doc_items);
        }
    }

    // Description + declared dependencies come straight from tinox.toml —
    // real project metadata, not re-derived/guessed. Examples are read from
    // an `examples/*.tnx` directory next to `src/`, one file per example,
    // sorted by filename so an author can order them (01_basic.tnx, ...).
    let (description, dependencies) = match &project_root {
        Some(root) => match pm::read_manifest(root) {
            Ok(m) => (
                m.package.as_ref().map(|p| p.description.clone()).filter(|d| !d.is_empty()),
                m.dependencies,
            ),
            Err(_) => (None, Vec::new()),
        },
        None => (None, Vec::new()),
    };
    let examples: Vec<(String, String)> = project_root.as_ref()
        .map(|root| root.join("examples"))
        .filter(|dir| dir.is_dir())
        .map(|dir| {
            let mut files: Vec<PathBuf> = fs::read_dir(&dir)
                .map(|entries| entries.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "tnx").unwrap_or(false))
                    .collect())
                .unwrap_or_default();
            files.sort();
            files.into_iter()
                .filter_map(|p| {
                    let src = fs::read_to_string(&p).ok()?;
                    let stem = p.file_stem()?.to_string_lossy().into_owned();
                    Some((humanize_example_name(&stem), src))
                })
                .collect()
        })
        .unwrap_or_default();

    let html = render_docs_html(&project_name, description.as_deref(), &dependencies, &examples, &doc_items);

    let out_path = match out_override {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("docs").join("index.html"),
    };
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {}", parent.display(), e);
            return;
        }
    }
    if let Err(e) = fs::write(&out_path, &html) {
        eprintln!("error: cannot write {}: {}", out_path.display(), e);
        return;
    }

    println!("Documentation written to {}", out_path.display());

    if open {
        let _ = Command::new("xdg-open").arg(&out_path).spawn()
            .or_else(|_| Command::new("open").arg(&out_path).spawn());
    }
}

/// Issue #186: `tinox graph` -- statically analyzes the project (same
/// project-root/`--out` discovery shape as `gen_docs` above) and writes a
/// Mermaid call-graph diagram seeded from every auto-run entry point
/// (`@GET`/etc, `@WebsocketEndpoint`, `@Amqp10Consumer`/`@Amqp091Consumer`,
/// `@Command`). Runs the same parse -> resolve_imports -> typecheck ->
/// process_annotations pipeline `compile_file` uses (needs a real,
/// type-checked AST: `TypeChecker::interface_info()` backs the
/// interface-dispatch fan-out in `callgraph::build_call_graph`) -- the
/// actual graph construction and Mermaid rendering live in
/// `callgraph.rs`, this function only assembles the AST and writes the
/// output file.
fn gen_call_graph(args: &[String]) {
    let out_override: Option<&String> =
        args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1));

    let input_file = match resolve_entry_file(args) {
        Some(f) => f,
        None => std::process::exit(1),
    };

    let source = match fs::read_to_string(&input_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", input_file, e);
            std::process::exit(1);
        }
    };
    let mut lexer = Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: lex error: {:?}", e);
            std::process::exit(1);
        }
    };
    let mut parser = Parser::new(tokens);
    let mut ast = match parser.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: parse error: {:?}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = check_one_type_per_file(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_no_top_level_fn(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_namespace_path_matches(&ast.decls, Path::new(&input_file)) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    stamp_file_identity(&mut ast.decls, Path::new(&input_file));

    let base_dir = Path::new(&input_file).parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut visited = HashSet::new();
    if let Ok(c) = Path::new(&input_file).canonicalize() {
        visited.insert(c);
    }
    let (dep_dirs, missing_deps) = load_dep_dirs(&base_dir);
    if let Err(e) = resolve_imports(&mut ast, &base_dir, &mut visited, &dep_dirs, &missing_deps) {
        eprintln!("error: import error: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = check_explicit_imports(Path::new(&input_file), &dep_dirs, &missing_deps) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    tinox_parser::assign_node_ids(&mut ast);
    let mut typechecker = tinox_typecheck::TypeChecker::new();
    if let Err(e) = typechecker.check(&ast) {
        eprintln!("error: type error:\n{}", e);
        std::process::exit(1);
    }
    let (iface_methods, class_implements) = typechecker.interface_info();

    let ann_result = tinox_typecheck::annotations::process_annotations(&ast);
    let project_root = pm::find_project_root_from(&base_dir).unwrap_or(base_dir);
    let graph = callgraph::build_call_graph(&ast.decls, &ann_result, &iface_methods, &class_implements, &project_root);

    if graph.entry_points.is_empty() {
        eprintln!(
            "warning: no auto-run entry points found (no @GET/@POST/etc, \
             @WebsocketEndpoint, @Amqp10Consumer/@Amqp091Consumer, or @Command) \
             -- writing an empty graph"
        );
    }

    let mermaid = callgraph::render_mermaid(&graph);

    let out_path = match out_override {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("docs").join("callgraph.mmd"),
    };
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {}", parent.display(), e);
            std::process::exit(1);
        }
    }
    if let Err(e) = fs::write(&out_path, &mermaid) {
        eprintln!("error: cannot write {}: {}", out_path.display(), e);
        std::process::exit(1);
    }

    println!(
        "Call graph written to {} ({} entry point{}, {} edge{}, {} unresolved call{})",
        out_path.display(),
        graph.entry_points.len(),
        if graph.entry_points.len() == 1 { "" } else { "s" },
        graph.edges.len(),
        if graph.edges.len() == 1 { "" } else { "s" },
        graph.unresolved.len(),
        if graph.unresolved.len() == 1 { "" } else { "s" },
    );
}

// ── Doc data model ────────────────────────────────────────────────────────────

struct DocParam  { name: String, ty: String }
struct DocMethod { name: String, doc: Option<String>, params: Vec<DocParam>, ret: String, annotations: Vec<String>, is_static: bool }
struct DocField  { name: String, ty: String, doc: Option<String>, annotations: Vec<String> }

enum DocItem {
    Class {
        name: String,
        doc: Option<String>,
        annotations: Vec<String>,
        fields: Vec<DocField>,
        methods: Vec<DocMethod>,
        implements: Vec<String>,
        extends: Option<String>,
    },
    Interface {
        name: String,
        doc: Option<String>,
        methods: Vec<DocMethod>,
    },
    Function {
        name: String,
        doc: Option<String>,
        params: Vec<DocParam>,
        ret: String,
        annotations: Vec<String>,
    },
}

fn collect_doc_items(decl: &tinox_parser::DeclKind, out: &mut Vec<DocItem>) {
    use tinox_parser::DeclKind;
    match decl {
        DeclKind::Class(c) if c.type_params.is_empty() => {
            let annotations = c.annotations.iter().map(|a| a.name.clone()).collect();
            let fields = c.fields.iter().map(|f| DocField {
                name: f.name.clone(),
                ty: type_str_simple(&f.field_type),
                doc: f.doc.clone(),
                annotations: f.annotations.iter().map(|a| a.name.clone()).collect(),
            }).collect();
            let methods = c.methods.iter().map(method_to_doc).collect();
            out.push(DocItem::Class {
                name: c.name.clone(),
                doc: c.doc.clone(),
                annotations,
                fields,
                methods,
                implements: c.implements.clone(),
                extends: c.extends.clone(),
            });
        }
        DeclKind::Interface(i) => {
            let methods = i.methods.iter().map(|m| {
                let params = m.params.iter().map(|p| DocParam { name: p.name.clone(), ty: type_str_simple(&p.param_type) }).collect();
                DocMethod {
                    name: m.name.clone(),
                    doc: m.doc.clone(),
                    params,
                    ret: type_str_simple(&m.ret_type),
                    annotations: m.annotations.iter().map(|a| a.name.clone()).collect(),
                    is_static: false,
                }
            }).collect();
            out.push(DocItem::Interface { name: i.name.clone(), doc: i.doc.clone(), methods });
        }
        DeclKind::Function(f) => {
            let params = f.params.iter().map(|p| DocParam { name: p.name.clone(), ty: type_str_simple(&p.param_type) }).collect();
            out.push(DocItem::Function {
                name: f.name.clone(),
                doc: f.doc.clone(),
                params,
                ret: type_str_simple(&f.ret_type),
                annotations: f.annotations.iter().map(|a| a.name.clone()).collect(),
            });
        }
        DeclKind::Namespace(ns) => {
            for inner in &ns.decls { collect_doc_items(&inner.node, out); }
        }
        _ => {}
    }
}

fn method_to_doc(m: &tinox_parser::Method) -> DocMethod {
    let params = m.params.iter().map(|p| DocParam { name: p.name.clone(), ty: type_str_simple(&p.param_type) }).collect();
    DocMethod {
        name: m.name.clone(),
        doc: m.doc.clone(),
        params,
        ret: type_str_simple(&m.ret_type),
        annotations: m.annotations.iter().map(|a| a.name.clone()).collect(),
        is_static: m.static_,
    }
}

fn type_str_simple(ty: &tinox_parser::Type) -> String {
    use tinox_parser::Type;
    match ty {
        Type::Int8 => "Int8".into(),   Type::Int16 => "Int16".into(),
        Type::Int32 => "Int32".into(), Type::Int64 => "Int64".into(),
        Type::UInt8 => "UInt8".into(), Type::UInt16 => "UInt16".into(),
        Type::UInt32 => "UInt32".into(), Type::UInt64 => "UInt64".into(),
        Type::Float32 => "Float32".into(), Type::Float64 => "Float64".into(),
        Type::Bool => "Bool".into(), Type::String => "String".into(),
        Type::Char => "Char".into(), Type::Nothing => "Nothing".into(),
        Type::Named(n) => n.clone(),
        Type::Array(t) => format!("{}[]", type_str_simple(t)),
        Type::Map(k, v) => format!("Map<{}, {}>", type_str_simple(k), type_str_simple(v)),
        Type::Tuple(ts) => format!("({})", ts.iter().map(type_str_simple).collect::<Vec<_>>().join(", ")),
        Type::Generic { name: n, args } => format!("{}<{}>", n, args.iter().map(type_str_simple).collect::<Vec<_>>().join(", ")),
        Type::Fn { params, ret } => format!("fn({}) -> {}", params.iter().map(type_str_simple).collect::<Vec<_>>().join(", "), type_str_simple(ret)),
        Type::Never => "Never".into(),
        Type::Any => "Any".into(),
        Type::Infer => "_".into(),
        Type::Mutable(t) => format!("mut {}", type_str_simple(t)),
        Type::Ref(t) => format!("&{}", type_str_simple(t)),
        Type::Nullable(t) => format!("{}?", type_str_simple(t)),
    }
}

// ── HTML renderer ─────────────────────────────────────────────────────────────

fn render_docs_html(
    project_name: &str,
    description: Option<&str>,
    dependencies: &[pm::Dependency],
    examples: &[(String, String)],
    items: &[DocItem],
) -> String {
    let mut nav = String::new();
    let mut body = String::new();

    // 1) Description, 2) Dependencies, 3) Examples, 4) the existing
    // class/interface/function reference — in that fixed order, matching
    // "what it is → what it needs → how to use it → full API".
    if let Some(desc) = description {
        nav.push_str("<li class=\"nav-section\">Overview</li><li><a href=\"#overview\">Description</a></li>");
        body.push_str(&format!(
            "<section id=\"overview\" class=\"item\"><p class=\"doc\" style=\"margin-bottom:0\">{}</p></section>",
            html_escape(desc)
        ));
    }

    if !dependencies.is_empty() {
        nav.push_str("<li class=\"nav-section\">Dependencies</li><li><a href=\"#dependencies\">Dependencies</a></li>");
        let rows: String = dependencies.iter().map(|d| {
            // Sibling docs.html, one directory per artifactId THEN version
            // — matches how these pages are actually laid out
            // (docs/tinox-core/<mod>/<version-with-dashes>/docs.html).
            let version_slug = version_path_slug(&d.version);
            format!(
                "<tr><td class=\"member-name\"><a href=\"../../{}/{}/docs.html\"><code>{}</code></a></td><td class=\"member-type\"><code>{}</code></td><td>{}</td></tr>",
                html_escape(&d.artifact_id), version_slug, html_escape(&d.artifact_id), html_escape(&d.version), html_escape(&d.group)
            )
        }).collect();
        body.push_str(&format!(
            "<section id=\"dependencies\" class=\"item\"><table class=\"members\"><tr><th style=\"text-align:left;color:var(--text3);font-size:0.75rem;text-transform:uppercase;padding-bottom:6px\">Module</th><th style=\"text-align:left;color:var(--text3);font-size:0.75rem;text-transform:uppercase;padding-bottom:6px\">Version</th><th style=\"text-align:left;color:var(--text3);font-size:0.75rem;text-transform:uppercase;padding-bottom:6px\">Group</th></tr>{}</table></section>",
            rows
        ));
    }

    if !examples.is_empty() {
        nav.push_str("<li class=\"nav-section\">Examples</li>");
        let mut ex_body = String::new();
        for (title, src) in examples {
            let slug: String = title.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
            nav.push_str(&format!("<li><a href=\"#ex-{slug}\">{}</a></li>", html_escape(title)));
            ex_body.push_str(&format!(
                "<section id=\"ex-{slug}\" class=\"item\"><h3 style=\"margin-top:0\">{}</h3><pre><code>{}</code></pre></section>",
                html_escape(title), highlight_tinox_source(src)
            ));
        }
        body.push_str(&ex_body);
    }

    let classes: Vec<&DocItem> = items.iter().filter(|i| matches!(i, DocItem::Class {..})).collect();
    let interfaces: Vec<&DocItem> = items.iter().filter(|i| matches!(i, DocItem::Interface {..})).collect();
    let functions: Vec<&DocItem> = items.iter().filter(|i| matches!(i, DocItem::Function {..})).collect();

    if !classes.is_empty() {
        nav.push_str("<li class=\"nav-section\">Classes</li>");
        for item in &classes {
            if let DocItem::Class { name, .. } = item {
                nav.push_str(&format!("<li><a href=\"#class-{name}\">{name}</a></li>"));
            }
        }
    }
    if !interfaces.is_empty() {
        nav.push_str("<li class=\"nav-section\">Interfaces</li>");
        for item in &interfaces {
            if let DocItem::Interface { name, .. } = item {
                nav.push_str(&format!("<li><a href=\"#iface-{name}\">{name}</a></li>"));
            }
        }
    }
    if !functions.is_empty() {
        nav.push_str("<li class=\"nav-section\">Functions</li>");
        for item in &functions {
            if let DocItem::Function { name, .. } = item {
                nav.push_str(&format!("<li><a href=\"#fn-{name}\">{name}</a></li>"));
            }
        }
    }

    for item in items {
        match item {
            DocItem::Class { name, doc, annotations, fields, methods, implements, extends } => {
                let anns = render_annotations(annotations);
                let mut subtitle = String::new();
                if let Some(p) = extends { subtitle.push_str(&format!(" extends <code>{p}</code>")); }
                if !implements.is_empty() {
                    subtitle.push_str(&format!(" implements {}", implements.iter().map(|i| format!("<code>{i}</code>")).collect::<Vec<_>>().join(", ")));
                }
                body.push_str(&format!(
                    "<section id=\"class-{name}\" class=\"item\"><h2 class=\"item-name\">{anns}<span class=\"kw\">class</span> {name}{subtitle}</h2>"
                ));
                if let Some(d) = doc { body.push_str(&format!("<p class=\"doc\">{}</p>", html_escape(d))); }

                if !fields.is_empty() {
                    body.push_str("<h3>Fields</h3><table class=\"members\">");
                    for f in fields {
                        let fanns = render_annotations(&f.annotations);
                        let fdoc = f.doc.as_deref().unwrap_or("");
                        body.push_str(&format!(
                            "<tr><td class=\"member-name\">{fanns}<code>{}</code></td><td class=\"member-type\"><code>{}</code></td><td>{}</td></tr>",
                            html_escape(&f.name), html_escape(&f.ty), html_escape(fdoc)
                        ));
                    }
                    body.push_str("</table>");
                }
                if !methods.is_empty() {
                    body.push_str("<h3>Methods</h3>");
                    for m in methods {
                        render_method_html(&mut body, m);
                    }
                }
                body.push_str("</section>");
            }
            DocItem::Interface { name, doc, methods } => {
                body.push_str(&format!(
                    "<section id=\"iface-{name}\" class=\"item\"><h2 class=\"item-name\"><span class=\"kw\">interface</span> {name}</h2>"
                ));
                if let Some(d) = doc { body.push_str(&format!("<p class=\"doc\">{}</p>", html_escape(d))); }
                for m in methods { render_method_html(&mut body, m); }
                body.push_str("</section>");
            }
            DocItem::Function { name, doc, params, ret, annotations } => {
                let anns = render_annotations(annotations);
                let sig = render_sig(name, params, ret, false);
                body.push_str(&format!(
                    "<section id=\"fn-{name}\" class=\"item\"><h2 class=\"item-name\">{anns}<span class=\"kw\">fn</span> {sig}</h2>"
                ));
                if let Some(d) = doc { body.push_str(&format!("<p class=\"doc\">{}</p>", html_escape(d))); }
                body.push_str("</section>");
            }
        }
    }

    // Same palette/layout conventions as docs_en.html (the hand-written
    // language reference) so auto-generated per-module doc pages read as
    // one consistent site rather than a visibly different tool's output.
    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{project_name} — Tinox Docs</title>
<style>
@import url('https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@500;600;700&family=Inter:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap');
:root {{
  --bg:        #070b14;
  --bg2:       rgba(17, 25, 43, 0.6);
  --bg3:       rgba(34, 211, 238, 0.07);
  --sidebar:   rgba(7, 11, 20, 0.72);
  --border:    rgba(103, 232, 249, 0.16);
  --accent:    #22d3ee;
  --accent2:   #a78bfa;
  --link:      #67e8f9;
  --green:     #34d399;
  --text:      #e2e8f0;
  --text2:     #93a4bd;
  --text3:     #64748b;
  --code-bg:   rgba(5, 7, 13, 0.65);
  --tag-bg:    rgba(34, 211, 238, 0.08);
  color-scheme: dark;
}}
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  font-family: 'Inter', ui-sans-serif, system-ui, sans-serif;
  background:
    radial-gradient(60rem 32rem at 12% -10%, rgba(34, 211, 238, 0.14), transparent 60%),
    radial-gradient(50rem 30rem at 110% 10%, rgba(167, 139, 250, 0.12), transparent 55%),
    linear-gradient(180deg, #05070d 0%, #070b14 40%, #060910 100%);
  background-attachment: fixed;
  color: var(--text);
  line-height: 1.7;
  display: flex;
  min-height: 100vh;
}}
body::before {{
  content: '';
  position: fixed;
  inset: 0;
  pointer-events: none;
  z-index: 0;
  background-image:
    linear-gradient(rgba(103, 232, 249, 0.035) 1px, transparent 1px),
    linear-gradient(90deg, rgba(103, 232, 249, 0.035) 1px, transparent 1px);
  background-size: 40px 40px;
  mask-image: radial-gradient(80% 60% at 50% 0%, black, transparent 75%);
}}
nav {{ position: sticky; top: 0; z-index: 1; width: 270px; min-width: 270px; height: 100vh; background: var(--sidebar); border-right: 1px solid var(--border); backdrop-filter: blur(14px); -webkit-backdrop-filter: blur(14px); padding: 32px 0; overflow-y: auto; flex-shrink: 0; }}
nav h1 {{ padding: 0 24px 20px; border-bottom: 1px solid var(--border); margin-bottom: 12px; font-family: 'Space Grotesk', 'JetBrains Mono', sans-serif; font-size: 1.2rem; font-weight: 700; letter-spacing: -0.2px; background: linear-gradient(120deg, #f8fafc 0%, #67e8f9 55%, #a78bfa 100%); -webkit-background-clip: text; background-clip: text; color: transparent; }}
nav ul {{ list-style: none; }}
nav li.nav-section {{ font-size: 0.65rem; font-weight: 700; text-transform: uppercase; letter-spacing: 1.2px; color: var(--text3); padding: 16px 24px 6px; }}
nav li a {{ display: block; padding: 7px 24px; color: var(--text2); text-decoration: none; font-size: 0.88rem; border-left: 2px solid transparent; transition: all 0.15s; }}
nav li a:hover {{ color: var(--text); background: var(--bg3); border-left-color: var(--accent); }}
main {{ position: relative; z-index: 1; flex: 1; min-width: 0; padding: 56px clamp(2rem, 4vw, 4rem); }}
main h1 {{ font-family: 'Space Grotesk', 'Inter', sans-serif; font-size: 1.9rem; font-weight: 700; color: #fff; margin-bottom: 8px; letter-spacing: -0.4px; }}
main > p.generated-by {{ color: var(--text3); margin-bottom: 28px; font-size: 0.78rem; }}
.tool-tag {{ font-family: 'JetBrains Mono', ui-monospace, monospace; color: var(--text3); }}
.item {{ border: 1px solid var(--border); border-radius: 10px; padding: 24px; margin-bottom: 24px; background: var(--bg2); backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px); }}
.item-name {{ font-size: 1.1rem; font-weight: 600; color: var(--text); margin-bottom: 12px; font-family: 'JetBrains Mono', 'Fira Code', monospace; }}
.kw {{ color: #c792ea; }}
.doc {{ color: var(--text2); font-size: 0.88rem; margin-bottom: 16px; line-height: 1.6; }}
h3 {{ font-size: 0.8rem; color: var(--text3); text-transform: uppercase; letter-spacing: 0.06em; margin: 20px 0 8px; font-weight: 700; }}
table.members {{ width: 100%; border-collapse: collapse; font-size: 0.85rem; }}
table.members tr {{ border-bottom: 1px solid var(--border); }}
table.members td {{ padding: 8px 10px; vertical-align: top; }}
.member-name {{ font-family: 'JetBrains Mono', 'Fira Code', monospace; color: var(--text); width: 35%; }}
.member-type {{ color: #ffcb6b; width: 20%; }}
.method-sig {{ background: var(--code-bg); border-radius: 6px; padding: 10px 14px; font-family: 'JetBrains Mono', 'Fira Code', monospace; font-size: 0.85rem; color: var(--text); margin-bottom: 8px; border: 1px solid var(--border); }}
.method-doc {{ color: var(--text2); font-size: 0.85rem; margin-bottom: 12px; line-height: 1.5; }}
.ann {{ color: var(--green); font-size: 0.78rem; display: inline-block; margin-right: 6px; }}
code {{ font-family: 'JetBrains Mono', 'Fira Code', monospace; font-size: 0.85em; background: var(--code-bg); border: 1px solid var(--border); border-radius: 4px; padding: 2px 6px; color: var(--link); }}
a {{ color: var(--link); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
</style>
</head>
<body>
<nav>
  <h1>📦 {project_name}</h1>
  <ul>{nav}</ul>
</nav>
<main>
  <h1>{project_name}</h1>
  <p class="generated-by">Generated by <span class="tool-tag">tinox doc</span></p>
  {body}
</main>
</body>
</html>"#)
}

fn render_method_html(out: &mut String, m: &DocMethod) {
    let anns = render_annotations(&m.annotations);
    let kw = if m.is_static { "fnc" } else { "fn" };
    let sig = render_sig(&m.name, &m.params, &m.ret, m.is_static);
    out.push_str(&format!(
        "<div class=\"method-sig\">{anns}<span class=\"kw\">{kw}</span> {sig}</div>"
    ));
    if let Some(d) = &m.doc {
        out.push_str(&format!("<div class=\"method-doc\">{}</div>", html_escape(d)));
    }
}

fn render_sig(name: &str, params: &[DocParam], ret: &str, _static: bool) -> String {
    let ps: Vec<String> = params.iter()
        .map(|p| format!("{}: <span style=\"color:#ffcb6b\">{}</span>", html_escape(&p.name), html_escape(&p.ty)))
        .collect();
    format!("{}({}) → <span style=\"color:#ffcb6b\">{}</span>", html_escape(name), ps.join(", "), html_escape(ret))
}

fn render_annotations(anns: &[String]) -> String {
    anns.iter().map(|a| format!("<span class=\"ann\">@{a}</span>")).collect()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `01_basic_publish.tnx` → `Basic publish`; `-`/`_` become spaces, a
/// leading numeric ordering prefix (`01_`, `2-`) is dropped, first letter
/// capitalized. Falls back to the stem itself if that leaves nothing.
/// `1.0.0` → `1-0-0` — the version-directory naming convention used for
/// `docs/tinox-core/<module>/<version>/docs.html` (dots aren't ideal
/// directory-name characters on every filesystem/URL context, dashes are).
fn version_path_slug(version: &str) -> String {
    version.replace('.', "-")
}

fn humanize_example_name(stem: &str) -> String {
    let no_prefix = stem.trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['_', '-']);
    let words: Vec<&str> = if no_prefix.is_empty() { stem } else { no_prefix }
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return stem.to_string();
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 { out.push(' '); }
        if i == 0 {
            let mut chars = w.chars();
            if let Some(c) = chars.next() {
                out.extend(c.to_uppercase());
                out.push_str(chars.as_str());
            }
        } else {
            out.push_str(w);
        }
    }
    out
}

/// Real-lexer-based syntax highlighting for example code blocks — reuses
/// `tinox_lexer::Lexer` (the same tokenizer the compiler itself runs)
/// rather than a regex approximation, so keywords/strings/comments/numbers
/// are colored exactly per the real grammar, not guessed. Falls back to
/// plain escaped text if the example doesn't lex cleanly. Types aren't a
/// distinct token kind in this lexer, so a capitalized identifier is
/// treated as one — matches this codebase's own PascalCase-for-types,
/// camelCase-for-values convention throughout tinox-core.
fn highlight_tinox_source(src: &str) -> String {
    use tinox_lexer::TokenKind;

    let mut lexer = Lexer::new(src);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(_) => return html_escape(src),
    };

    let mut out = String::new();
    let mut pos = 0usize;
    for tok in &tokens {
        let start = tok.span.start.offset as usize;
        let end = tok.span.end.offset as usize;
        if start > pos && start <= src.len() {
            out.push_str(&html_escape(&src[pos..start]));
        }
        if start > end || end > src.len() {
            continue;
        }
        let text = &src[start..end];
        let escaped = html_escape(text);
        let class = match &tok.kind {
            TokenKind::Keyword(_) | TokenKind::Bool(_) => Some("kw"),
            TokenKind::String(_) | TokenKind::RawString(_) | TokenKind::InterpString(_) | TokenKind::Char(_) => Some("str"),
            TokenKind::Integer(_) | TokenKind::Float(_) | TokenKind::IntegerSuffix(_) | TokenKind::FloatSuffix(_) => Some("num"),
            TokenKind::Comment(_) | TokenKind::DocComment(_) => Some("cmt"),
            TokenKind::Ident(name) if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) => Some("type"),
            _ => None,
        };
        match class {
            Some(c) => out.push_str(&format!("<span class=\"{}\">{}</span>", c, escaped)),
            None => out.push_str(&escaped),
        }
        pos = end.max(pos);
    }
    if pos < src.len() {
        out.push_str(&html_escape(&src[pos..]));
    }
    out
}

/// Parse a file and return all @Test entries without compiling.
fn collect_tests(path: &str) -> Result<Vec<tinox_typecheck::annotations::TestInfo>, String> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read: {e}"))?;
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| format!("lex error: {e:?}"))?;
    let mut parser = tinox_parser::Parser::new(tokens);
    let mut ast = parser.parse().map_err(|e| format!("parse error: {e:?}"))?;
    check_one_type_per_file(&ast.decls, Path::new(path))?;
    check_no_top_level_fn(&ast.decls, Path::new(path))?;
    check_namespace_path_matches(&ast.decls, Path::new(path))?;
    let base = Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut visited = HashSet::new();
    if let Ok(c) = Path::new(path).canonicalize() { visited.insert(c); }
    let (dep_dirs, missing_deps) = load_dep_dirs(&base);
    resolve_imports(&mut ast, &base, &mut visited, &dep_dirs, &missing_deps)
        .map_err(|e| format!("import error: {e}"))?;
    check_explicit_imports(Path::new(path), &dep_dirs, &missing_deps)?;
    let result = tinox_typecheck::annotations::process_annotations(&ast);
    Ok(result.test_entries)
}

/// Compile `source` with a synthetic main that runs one test method and exits 0/1.
/// Compile a test-mode executable: the test method returns Bool; main exits 0 on true.
fn compile_test_exe(source: &str, class_name: &str, method_name: &str, exe: &str) -> Result<(), String> {
    let src = fs::read_to_string(source)
        .map_err(|e| format!("cannot read: {e}"))?;

    let mut lexer = Lexer::new(&src);
    let tokens = lexer.tokenize().map_err(|e| format!("lex: {e:?}"))?;
    let mut parser = tinox_parser::Parser::new(tokens);
    let mut ast = parser.parse().map_err(|e| format!("parse: {e:?}"))?;
    check_one_type_per_file(&ast.decls, Path::new(source))?;
    check_no_top_level_fn(&ast.decls, Path::new(source))?;
    check_namespace_path_matches(&ast.decls, Path::new(source))?;

    let base = Path::new(source).parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut visited = HashSet::new();
    if let Ok(c) = Path::new(source).canonicalize() { visited.insert(c); }
    let (dep_dirs, missing_deps) = load_dep_dirs(&base);
    resolve_imports(&mut ast, &base, &mut visited, &dep_dirs, &missing_deps)?;
    check_explicit_imports(Path::new(source), &dep_dirs, &missing_deps)?;
    tinox_parser::assign_node_ids(&mut ast);

    let mut tc = tinox_typecheck::TypeChecker::new();
    tc.check(&ast).map_err(|e| format!("type error:\n{e}"))?;
    let (iface, impls) = tc.interface_info();

    let ann = tinox_typecheck::annotations::process_annotations(&ast);

    let route_entries = ann.route_entries.iter().map(|r| tinox_codegen::RouteEntry {
        http_method: r.method.clone(), path: r.path.clone(),
        class_name: r.class_name.clone(), method_name: r.method_name.clone(),
        status_code: r.status_code, produces: r.produces.clone(),
        consumes: r.consumes.clone(), auth_type: r.auth_type.clone(),
        oidc_roles: r.oidc_roles.clone(),
        is_static: r.is_static,
        params: convert_route_params(&r.params),
        return_type: r.return_type.clone(),
    }).collect();
    let di_components = ann.di_components.iter().map(|c| tinox_codegen::DiComponentInfo {
        class_name: c.class_name.clone(),
        scope: match c.scope {
            tinox_typecheck::annotations::DiScope::Application => tinox_codegen::DiScope::Application,
            tinox_typecheck::annotations::DiScope::Startup => tinox_codegen::DiScope::Startup,
            tinox_typecheck::annotations::DiScope::HttpRequest => tinox_codegen::DiScope::HttpRequest,
        },
        inject_fields: c.inject_fields.iter().map(|f| tinox_codegen::DiInjectField {
            field_name: f.field_name.clone(), field_type: f.field_type.clone(),
        }).collect(),
    }).collect();
    let config_fields = ann.config_fields.iter().map(|f| tinox_codegen::ConfigFieldInfo {
        class_name: f.class_name.clone(), field_name: f.field_name.clone(),
        config_key: f.config_key.clone(), field_llvm_type: f.field_llvm_type.clone(),
    }).collect();
    let cli_commands = ann.cli_commands.iter().map(|c| tinox_codegen::CliCommandInfo {
        class_name: c.class_name.clone(), cmd_name: c.cmd_name.clone(),
        description: c.description.clone(), version: c.version.clone(),
        options: c.options.iter().map(|o| tinox_codegen::CliOptionInfo {
            field_name: o.field_name.clone(), names: o.names.clone(),
            description: o.description.clone(), required: o.required,
            field_type: o.field_type.clone(),
        }).collect(),
        arguments: c.arguments.iter().map(|a| tinox_codegen::CliArgumentInfo {
            field_name: a.field_name.clone(), index: a.index,
            description: a.description.clone(), required: a.required,
            field_type: a.field_type.clone(),
        }).collect(),
    }).collect();

    let sensitive_fields = ann.sensitive_fields.iter().map(|f| tinox_codegen::LogMaskFieldInfo {
        class_name: f.class_name.clone(), field_name: f.field_name.clone(),
    }).collect();
    let masked_fields = ann.masked_fields.iter().map(|f| tinox_codegen::LogMaskFieldInfo {
        class_name: f.class_name.clone(), field_name: f.field_name.clone(),
    }).collect();

    let mut cg = CodeGen::new();
    cg.set_expr_value_types(tc.expr_value_types());
    cg.set_interface_info(iface, impls);
    let do_not_serialize_fields: Vec<tinox_codegen::LogMaskFieldInfo> = ann.do_not_serialize_fields
        .iter()
        .map(|f| tinox_codegen::LogMaskFieldInfo {
            class_name: f.class_name.clone(),
            field_name: f.field_name.clone(),
        })
        .collect();
    cg.set_annotation_info(tinox_codegen::AnnotationInfo {
        inline_fns: ann.inline_functions,
        inline_meths: ann.inline_methods,
        routes: route_entries,
        di_components,
        log_classes: ann.log_classes,
        config_fields,
        cli_commands,
        sensitive_fields,
        masked_fields,
        do_not_serialize_fields,
        json_serializable_classes: ann.json_serializable_classes,
        metric_entries: vec![],
        transactional_methods: ann.transactional_methods,
    });
    let entity_entries_test: Vec<tinox_codegen::EntityEntry> = ann.entity_entries
        .iter()
        .map(|e| tinox_codegen::EntityEntry {
            class_name: e.class_name.clone(),
            table_name: e.table_name.clone(),
            fields: e.fields.iter().map(|f| tinox_codegen::EntityFieldEntry {
                field_name: f.field_name.clone(),
                column_name: f.column_name.clone(),
                is_id: f.is_id,
                is_generated: f.is_generated,
                not_null: f.not_null,
                field_llvm_type: f.field_llvm_type.clone(),
            }).collect(),
        })
        .collect();
    cg.set_entity_entries(entity_entries_test);
    cg.set_test_entry(class_name.to_string(), method_name.to_string());
    cg.gen(&ast).map_err(|e| format!("codegen: {e:?}"))?;

    let ir = cg.into_ir();
    let ir_path = format!("{exe}.ll");
    fs::write(&ir_path, ir).map_err(|e| format!("write IR: {e}"))?;
    compile_ll_to_exe(&ir_path, exe, OptLevel::Debug)
}

/// Returns the Tinox standard library directory.
/// Checks TINOX_PATH env var first, then the path relative to this binary's
/// source location (works for `cargo run` during development), then the
/// fixed system install path used by distro packages (e.g. the AUR
/// `tinox-bin` package installs tinox-core to /usr/share/tinox/core).
fn stdlib_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TINOX_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return Some(pb);
        }
    }
    // Compiled-in dev path: crates/tinox/../../crates/tinox-core = crates/tinox-core
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/tinox-core");
    if dev.is_dir() {
        return dev.canonicalize().ok();
    }
    let system = PathBuf::from("/usr/share/tinox/core");
    if system.is_dir() {
        return Some(system);
    }
    None
}

/// The always-available "core" tier of `tinox.core.*` — resolves
/// unconditionally via `stdlib_dir()`, exactly like every `tinox.core.*`
/// import did before this split, with no `tinox.toml` declaration needed.
/// Everything else under `tinox.core.*` is "extended tier": it must be
/// declared as a real dependency (group `tinox.core`, the module name as
/// `artifactId`) and installed via `tinox install` — see `resolve_imports`'s
/// branch 3 gating below, and CLAUDE.md's core/extended stdlib split notes.
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

/// Returns the path to runtime.c: the dev-checkout path relative to this
/// binary's compiled-in source location (works for `cargo run` during
/// development), then the fixed system install path used by distro
/// packages. Unlike `stdlib_dir`, there is no env var override — runtime.c
/// is an implementation detail, not something a user is expected to point
/// at directly.
fn runtime_c_path() -> Option<PathBuf> {
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/runtime.c");
    if dev.is_file() {
        return Some(dev);
    }
    let system = PathBuf::from("/usr/share/tinox/runtime.c");
    if system.is_file() {
        return Some(system);
    }
    None
}

/// Returns installed dependency directories plus any coordinate-resolved
/// dependency the manifest declares but that isn't installed yet (see
/// `resolve_imports`'s use of the latter to distinguish "never declared"
/// from "declared but `tinox install` wasn't run").
fn load_dep_dirs(base_dir: &Path) -> (Vec<PathBuf>, Vec<pm::MissingDep>) {
    pm::find_project_root_from(base_dir)
        .and_then(|root| pm::read_manifest(&root).ok().map(|m| (root, m)))
        .map(|(root, m)| pm::installed_dep_dirs(&root, &m))
        .unwrap_or_default()
}

/// `tinox.core` module names (artifactIds) declared in the nearest
/// tinox.toml's `[[dependencies]]` -- fed into the generated program's
/// startup banner via `CodeGen::set_loaded_modules`. Declared, not
/// actually-imported: simpler and accurate enough (an unused declared
/// dependency is already unusual, not the common case this needs to
/// optimize for), and avoids threading resolved-import bookkeeping across
/// the typecheck/codegen boundary just for a log line.
fn loaded_tinox_core_modules(base_dir: &Path) -> Vec<String> {
    pm::find_project_root_from(base_dir)
        .and_then(|root| pm::read_manifest(&root).ok())
        .map(|m| {
            m.dependencies
                .iter()
                .filter(|d| d.group == "tinox.core")
                .map(|d| d.artifact_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Collects the names of every top-level `class`/`interface`/`enum` in a
/// single file's own decls, descending into `namespace { ... }` wrappers
/// (the stdlib's `namespace tinox.core.X { class Y { ... } }` shape) since
/// those are organizational, not a second nesting level from the user's
/// perspective. Order matches declaration order.
fn collect_type_decl_names(decls: &[tinox_parser::Decl]) -> Vec<&str> {
    let mut names = Vec::new();
    for d in decls {
        match &d.node {
            DeclKind::Class(c) => names.push(c.name.as_str()),
            DeclKind::Interface(i) => names.push(i.name.as_str()),
            DeclKind::Enum(e) => names.push(e.name.as_str()),
            DeclKind::Namespace(ns) => names.extend(collect_type_decl_names(&ns.decls)),
            _ => {}
        }
    }
    names
}

/// Enforces "at most one top-level class/interface/enum per file, and if
/// there is one, the file must be named exactly after it" (case-sensitive).
/// Must run on a SINGLE file's own (pre-merge) decls, before those decls are
/// merged into the importer — once merged, a decl's originating file can no
/// longer be determined (`Spanned<T>` carries no filename). Wired into
/// `resolve_imports` (for every imported file) and `check`/`compile_file`
/// (for the entry file).
fn check_one_type_per_file(decls: &[tinox_parser::Decl], path: &Path) -> Result<(), String> {
    let names = collect_type_decl_names(decls);
    match names.as_slice() {
        [] => Ok(()),
        [only] => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if *only == stem {
                Ok(())
            } else {
                Err(format!(
                    "'{}' declares '{}', but the file must be named '{}.tnx' (one type per file, filename must match exactly)",
                    path.display(),
                    only,
                    only
                ))
            }
        }
        many => Err(format!(
            "'{}' declares {} types ({}), but only one class/interface/enum is allowed per file — split it into separate files",
            path.display(),
            many.len(),
            many.join(", ")
        )),
    }
}

/// Collects `(namespace segments, type name)` for every class/interface/enum
/// declared inside a `namespace a.b.c { ... }` block, at any nesting depth
/// (segments accumulate across nested `namespace` blocks). A type declared
/// OUTSIDE any `namespace` block is intentionally excluded — issue #185's
/// path-mirroring rule is strictly opt-in, matching current adoption
/// exactly (0% of project-local files declare a namespace today; only
/// stdlib-style code does).
fn collect_namespaced_type_decls(decls: &[tinox_parser::Decl]) -> Vec<(Vec<String>, &str)> {
    fn walk<'a>(
        decls: &'a [tinox_parser::Decl],
        prefix: &[String],
        out: &mut Vec<(Vec<String>, &'a str)>,
    ) {
        for d in decls {
            match &d.node {
                DeclKind::Namespace(ns) => {
                    let mut segs = prefix.to_vec();
                    segs.extend(ns.name.iter().cloned());
                    walk(&ns.decls, &segs, out);
                }
                DeclKind::Class(c) if !prefix.is_empty() => {
                    out.push((prefix.to_vec(), c.name.as_str()))
                }
                DeclKind::Interface(i) if !prefix.is_empty() => {
                    out.push((prefix.to_vec(), i.name.as_str()))
                }
                DeclKind::Enum(e) if !prefix.is_empty() => {
                    out.push((prefix.to_vec(), e.name.as_str()))
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(decls, &[], &mut out);
    out
}

/// Issue #185: enforces "a type declared inside `namespace a.b.c { ... }`
/// must live at a file path that mirrors the namespace" — finishes what the
/// one-type-per-file convention started (previously only the LAST namespace
/// segment became a directory, e.g. `crates/tinox-core-ext/amqp10/`, not the
/// full `tinox/core/amqp10/`). Strictly opt-in via
/// `collect_namespaced_type_decls`: a type with no enclosing `namespace`
/// block is exempt, so this only ever fires for stdlib-style code that
/// already declares one.
///
/// Root resolution: walks up from `path`'s parent for the nearest
/// `tinox.toml` (`pm::find_project_root_from`). If found, the mirrored path
/// is checked against whichever of these the file actually resolves under,
/// most specific first: `<manifest_dir>/src` (the ordinary project
/// convention), `<manifest_dir>/tests` (the `tests/<namespace-path>/
/// <TypeName>Test.tnx` convention — a test file's own namespace/type-name
/// pair mirrors the same way a source file's does, just rooted at `tests/`
/// instead of `src/`), else `<manifest_dir>` itself directly (the
/// stdlib-ext convention: `.tnx` files sit directly beside `tinox.toml`, no
/// `src/` layer). If no `tinox.toml` ancestor exists at all, or `path`
/// doesn't resolve under ANY of these, the check is skipped — there's no
/// root to meaningfully validate against.
///
/// Never applied to a file inside an INSTALLED dependency
/// (`.tinox/deps/...` or the global `~/.tinox/repository/...` cache —
/// `pm::dep_install_dir`/`global_dep_install_dir`), detected by a literal
/// `.tinox` path component anywhere in `path`. Hit live during `make
/// check`: a pre-existing core-tier module (`socket`) published before
/// this migration has no `tinox.toml` of its own inside its installed
/// package directory, so `find_project_root_from` walked straight past it
/// and found the CONSUMING project's own manifest instead — producing a
/// nonsensical "must be located at <consumer project root>/tinox/core/
/// socket/Socket.tnx" error for a file the current project doesn't even
/// own. Installed dependencies are pre-vetted, address-scoped, immutable
/// content this check has no business re-validating in the first place
/// (there's nothing a local compile error would let anyone fix); only
/// this project's OWN source is in scope.
/// Every distinct namespace path (as segments) any top-level declaration in
/// `decls` was declared under — issue #194 Phase 2's own notion of "this
/// file's namespace(s)" (at most one in practice, given the one-type-per-
/// file convention, but handled generally). Empty for the common case (no
/// `namespace {}` block at all), which is what keeps Phase 2 a zero-cost
/// no-op for the vast majority of project-local code (see issue #194's own
/// "0% adoption in project-local code" blast-radius finding).
fn own_namespace_paths(decls: &[tinox_parser::Decl]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for (segs, _name) in collect_namespaced_type_decls(decls) {
        if !out.contains(&segs) {
            out.push(segs);
        }
    }
    out
}

/// Issue #194 Phase 2 ("same namespace as the current file → implicit
/// visibility, no import statement needed"): every `.tnx` file directly in
/// `dir` (no subdirectory recursion — a subdirectory corresponds to a
/// DEEPER namespace segment, a different namespace, per issue #185's
/// namespace-mirroring convention) whose OWN namespace path exactly equals
/// `ns_path`. Checked by actually parsing each candidate, not just trusting
/// directory placement — issue #185's own path-match check is skipped for
/// installed dependencies / when no tinox.toml ancestor exists, so
/// directory placement alone isn't proof. Includes the file this was
/// computed FROM (trivially matches its own namespace) — both call sites
/// rely on their own `visited`/self-equality check to skip it rather than
/// excluding it here, since they already need that check anyway (a
/// same-namespace sibling can itself already be visited via an explicit
/// import, or via ANOTHER sibling's own auto-merge).
fn find_namespace_siblings(dir: &Path, ns_path: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.extension().map(|e| e == "tnx").unwrap_or(false) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&p) else { continue };
        let Ok(tokens) = Lexer::new(&source).tokenize() else { continue };
        let Ok(parsed) = Parser::new(tokens).parse() else { continue };
        if own_namespace_paths(&parsed.decls).iter().any(|s| s.as_slice() == ns_path) {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn check_namespace_path_matches(decls: &[tinox_parser::Decl], path: &Path) -> Result<(), String> {
    let namespaced = collect_namespaced_type_decls(decls);
    if namespaced.is_empty() {
        return Ok(());
    }
    let Ok(abs_path) = path.canonicalize() else {
        return Ok(());
    };
    if abs_path.components().any(|c| c.as_os_str() == ".tinox") {
        return Ok(());
    }
    let Some(manifest_dir) = pm::find_project_root_from(path.parent().unwrap_or(Path::new(".")))
    else {
        return Ok(());
    };
    let candidates = [
        manifest_dir.join("src"),
        manifest_dir.join("tests"),
        manifest_dir.clone(),
    ];
    let Some((root, rel)) = candidates.iter().find_map(|candidate| {
        let abs_root = candidate.canonicalize().ok()?;
        let rel = abs_path.strip_prefix(&abs_root).ok()?;
        Some((candidate.clone(), rel.to_path_buf()))
    }) else {
        return Ok(());
    };
    for (segs, type_name) in &namespaced {
        let mut expected = PathBuf::new();
        for seg in segs {
            expected.push(seg);
        }
        expected.push(format!("{}.tnx", type_name));
        if rel != expected {
            return Err(format!(
                "'{}' declares '{}' in namespace '{}', but the file must be located at '{}' to match its namespace",
                path.display(),
                type_name,
                segs.join("."),
                root.join(&expected).display(),
            ));
        }
    }
    Ok(())
}

/// Collects the names of every top-level free `fn` WITH A BODY in a single
/// file's own decls (descending into `namespace { ... }` like
/// `collect_type_decl_names` does). `extern fn` declarations are excluded
/// (`StmtKind::Empty` is the parser's marker for a body-less
/// declare-only signature, confirmed in `tinox-codegen`'s `gen_fn`) —
/// those are FFI bindings to `runtime.c`, not free functions in the
/// issue #149 sense, and stay legal.
fn collect_top_level_fn_names(decls: &[tinox_parser::Decl]) -> Vec<&str> {
    let mut names = Vec::new();
    for d in decls {
        match &d.node {
            DeclKind::Function(f) if !matches!(f.body.node, tinox_parser::StmtKind::Empty) => {
                names.push(f.name.as_str())
            }
            DeclKind::Namespace(ns) => names.extend(collect_top_level_fn_names(&ns.decls)),
            _ => {}
        }
    }
    names
}

/// Whether the (post-import-merge) decl list contains a class literally
/// named `Main` — existence only, not full shape validation (that stays
/// `emit_class_main_entry_point`'s job in codegen, which has a real `Span`
/// to attach a precise error to). Used by `compile_file` to hard-error
/// early, with a clearer message than the "undefined reference to
/// tinox_main" link failure that used to be the only signal when no
/// annotation-driven auto-run kind happened to be present either.
fn has_class_named_main(decls: &[tinox_parser::Decl]) -> bool {
    decls.iter().any(|d| match &d.node {
        DeclKind::Class(c) => c.name == "Main",
        DeclKind::Namespace(ns) => has_class_named_main(&ns.decls),
        _ => false,
    })
}

/// Issue #149 stage 3: hard-enforces "no top-level `fn` with a body" — the
/// language has no implicit global function namespace anymore, every
/// function must be a class method (`fn`/`fnc`). Mirrors
/// `check_one_type_per_file` exactly: must run on a SINGLE file's own
/// (pre-merge) decls for the same reason (a decl's originating file can't
/// be recovered after `resolve_imports` merges everything), and is wired
/// into the identical call sites (`resolve_imports` for every imported
/// file, `check`/`compile_file`/test-mode entry points for the entry
/// file).
fn check_no_top_level_fn(decls: &[tinox_parser::Decl], path: &Path) -> Result<(), String> {
    let names = collect_top_level_fn_names(decls);
    if names.is_empty() {
        return Ok(());
    }
    Err(format!(
        "'{}' declares {} top-level function{} ({}) — Tinox no longer allows free functions outside a class; move {} into a class as a `fnc` (static) or `fn` (instance) method",
        path.display(),
        names.len(),
        if names.len() == 1 { "" } else { "s" },
        names.join(", "),
        if names.len() == 1 { "it" } else { "them" }
    ))
}

/// Stamps every `Function`/`Class` method's `file` field with `path`
/// (issue #114): the parser has no notion of a filename (`Parser::new`
/// only sees a token stream), so it always leaves `file` at
/// `tinox_parser::UNKNOWN_FILE`. This is the one place — called right
/// after each individual file is parsed, both for the entry file
/// (`compile_file`) and every imported file (`resolve_imports`), BEFORE
/// `resolve_imports` merges everything into one flat decl list — where
/// the real path is actually known. Recurses into `Namespace` decls
/// (matches `CodeGen::gen`'s own decl-walking for `gen_fn`/
/// `gen_class_method`, the only two codegen sites that read `file`).
/// Uses the canonicalized absolute path so DWARF's `!DIFile` directory/
/// filename split (`tinox-codegen`) is well-defined regardless of the
/// cwd `tinox build` was invoked from.
fn stamp_file_identity(decls: &mut [tinox_parser::Decl], path: &Path) {
    let file: std::sync::Arc<str> = std::sync::Arc::from(
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    );
    stamp_file_identity_with(decls, &file);
}

fn stamp_file_identity_with(decls: &mut [tinox_parser::Decl], file: &std::sync::Arc<str>) {
    for decl in decls {
        match &mut decl.node {
            DeclKind::Function(f) => f.file = file.clone(),
            DeclKind::Class(c) => {
                for m in &mut c.methods {
                    m.file = file.clone();
                }
            }
            DeclKind::Namespace(ns) => stamp_file_identity_with(&mut ns.decls, file),
            _ => {}
        }
    }
}

/// Resolves a module reference to a list of source files: prefers a single
/// `<name>.tnx` file (legacy / not-yet-migrated modules); if that doesn't
/// exist, falls back to a `<name>/` directory containing one `.tnx` file per
/// top-level type (one-type-per-file convention, Issue: filename must match
/// its type). Returns `Ok(None)` if neither a matching file nor directory
/// exists under `base`; `Err` if the directory exists but is empty/unreadable.
fn resolve_module_paths(
    base: &Path,
    rel_file: &Path,
    rel_dir: &Path,
) -> Result<Option<Vec<PathBuf>>, String> {
    if let Ok(p) = base.join(rel_file).canonicalize() {
        return Ok(Some(vec![p]));
    }
    let dir = base.join(rel_dir);
    if dir.is_dir() {
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("Cannot read module directory '{}': {}", dir.display(), e))?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e == "tnx").unwrap_or(false))
            .collect();
        if files.is_empty() {
            return Err(format!("Module directory '{}' contains no .tnx files", dir.display()));
        }
        files.sort();
        let canon: Result<Vec<PathBuf>, String> = files
            .iter()
            .map(|p| p.canonicalize().map_err(|e| format!("Cannot resolve '{}': {}", p.display(), e)))
            .collect();
        return canon.map(Some);
    }
    Ok(None)
}

/// Best-effort `group:artifactId:version` label for an installed
/// dependency directory (`.tinox/deps/<group>/<artifactId>/<version>/`,
/// see `pm::dep_install_dir`), for the ambiguous-import diagnostic below.
/// Falls back to the raw path if it doesn't have that shape (defensive
/// only — every entry `installed_dep_dirs` produces does).
fn dep_dir_coordinate(dep_dir: &Path) -> String {
    let parts: Vec<&str> = dep_dir
        .components()
        .rev()
        .take(3)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if parts.len() == 3 {
        format!("{}:{}:{}", parts[2], parts[1], parts[0])
    } else {
        dep_dir.display().to_string()
    }
}

/// Resolves `rel_file`/`rel_dir` against every installed dependency
/// directory, requiring **at most one** to match. Two dependencies
/// shipping a module at the same relative path used to resolve via
/// `.find_map` — first (manifest-declaration-order) match silently wins,
/// the other is shadowed with no diagnostic of any kind. That's exactly
/// the shape of bug this project's own "no silent garbage" principle
/// (CLAUDE.md) exists to prevent, so an ambiguity is now a hard error
/// instead (#156) — a per-dependency resolution error (e.g. an empty
/// module directory) still doesn't count as a match and doesn't block
/// resolution via a different dependency, unchanged from before.
fn resolve_in_dep_dirs(
    dep_dirs: &[PathBuf],
    rel_file: &Path,
    rel_dir: &Path,
) -> Result<Option<Vec<PathBuf>>, String> {
    let matches: Vec<(&PathBuf, Vec<PathBuf>)> = dep_dirs
        .iter()
        .filter_map(|d| resolve_module_paths(d, rel_file, rel_dir).ok().flatten().map(|p| (d, p)))
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap().1)),
        _ => {
            let coords: Vec<String> = matches.iter().map(|(d, _)| dep_dir_coordinate(d)).collect();
            // Two matches for the SAME group:artifactId:version aren't a
            // real ambiguity — a coordinate is immutable on tinox-central
            // (see pm.rs's global cache doc comments), so any two
            // installations of it are guaranteed the same content, just
            // reached via different resolution paths. This happens in
            // practice when a project directly declares a coordinate
            // dependency (→ the global ~/.tinox/repository/ cache) that's
            // ALSO reachable transitively through another dependency whose
            // own manifest still pins it via an explicit `url` (→ the
            // project-local .tinox/deps/ tree) — e.g. before every
            // already-published extended-tier stdlib package is
            // republished under the new coordinate-only manifest style.
            // Only a genuine coordinate MISMATCH is a real, hard-error
            // ambiguity (#156's original case: two unrelated dependencies
            // shipping conflicting content at the same relative path).
            if coords.iter().all(|c| c == &coords[0]) {
                return Ok(Some(matches.into_iter().next().unwrap().1));
            }
            Err(format!(
                "Ambiguous import '{}': resolves in more than one installed dependency ({}). \
                 Remove or rename one of them so their module paths don't collide.",
                rel_file.display(),
                coords.join(", "),
            ))
        }
    }
}

/// Whether an import resolved to project-local file(s) (relative to the
/// importing file's own directory) or to trusted, external content
/// (an installed dependency, or stdlib_dir()/CORE_MODULES) -- used by
/// `check_explicit_imports` (issue #194) to decide whether to recurse and
/// re-validate, or trust the target as an opaque prelude. `resolve_imports`
/// itself ignores this distinction; it merges either way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportOrigin {
    Local,
    External,
}

/// Resolves one `import` statement to the real file(s) it refers to, in the
/// same order `resolve_imports` has always used:
/// 1. Relative to the source file's own directory (`ImportOrigin::Local`).
/// 2. Installed package dependencies (.tinox/deps/... or the global
///    ~/.tinox/repository/... cache — see resolve_in_dep_dirs).
/// 3. tinox.core.X  →  <stdlib_dir>/tinox/core/X.tnx or
///    <stdlib_dir>/tinox/core/X/*.tnx (issue #185: the full dotted
///    import path, including the "tinox"/"core" prefix itself, is
///    resolved as a literal nested path under stdlib_dir() — the
///    SAME `rel_file`/`rel_dir` built below from the whole
///    import path, no special-cased tail-stripping. This mirrors how
///    branch 2 (resolve_in_dep_dirs) already resolves the full path
///    under each dep dir, and matches what published/downloaded
///    tinox-core-ext packages already look like on disk — see
///    CLAUDE.md's namespace-mirroring migration notes) — but ONLY
///    for CORE_MODULES. `tinox.core.<mod>` for anything else is
///    extended-tier: it must have already resolved via branch 2 (a
///    declared+installed dependency); if we're here, it didn't, so
///    fail with a specific, actionable error instead of silently
///    falling through to stdlib_dir() (see CLAUDE.md's core/extended
///    stdlib split notes and the "no silent garbage" philosophy this
///    project follows throughout).
///
/// Branches 2 and 3 are both `ImportOrigin::External`.
fn resolve_import_target(
    import: &tinox_parser::ast::Import,
    base_dir: &Path,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<(Vec<PathBuf>, ImportOrigin), String> {
    // ["foo", "bar"] → "foo/bar.tnx" (single-file module) or "foo/bar/"
    // (directory module, one .tnx per top-level type) relative to base_dir.
    let mut rel_file = PathBuf::new();
    let mut rel_dir = PathBuf::new();
    for (i, seg) in import.path.iter().enumerate() {
        if i == import.path.len() - 1 {
            rel_file.push(format!("{}.tnx", seg));
            rel_dir.push(seg);
        } else {
            rel_file.push(seg);
            rel_dir.push(seg);
        }
    }

    if let Some(p) = resolve_module_paths(base_dir, &rel_file, &rel_dir)? {
        return Ok((p, ImportOrigin::Local));
    }
    // Fallback: relative to the nearest project root's src/, tests/, or the
    // manifest dir itself (the same three candidates issue #185's own
    // namespace-mirroring check already treats as valid roots) instead of
    // the importing file's own directory. Needed for a full dotted path to
    // reach a DIFFERENT namespace-mirrored directory from anywhere in the
    // project, not just from the entry file itself: a project-local import
    // resolves relative to the importing file's own directory (branch
    // above), which means a plain sibling import (`import PersonDao;` from
    // within the same directory) works from anywhere, but a full dotted
    // path (`import demo.model.Person;`) written OUTSIDE the entry file
    // has no valid relative-to-self resolution at all once that file lives
    // more than one level deep — confirmed live via the external `demo`
    // project (issue #194's own motivating example) once Phase 1 made the
    // import mandatory: `demo.dao.PersonDaoImpl` genuinely could not
    // express any working import for `demo.model.Person`. Only tried after
    // the direct relative-to-self lookup already failed, so this is purely
    // additive — it can only resolve imports that previously errored, never
    // change one that already worked.
    if let Some(root) = pm::find_project_root_from(base_dir) {
        for candidate_root in [root.join("src"), root.join("tests"), root.clone()] {
            if candidate_root == base_dir {
                continue;
            }
            if let Some(p) = resolve_module_paths(&candidate_root, &rel_file, &rel_dir)? {
                return Ok((p, ImportOrigin::Local));
            }
        }
    }
    if let Some(p) = resolve_in_dep_dirs(dep_dirs, &rel_file, &rel_dir)? {
        return Ok((p, ImportOrigin::External));
    }
    if import.path.first().map(|s| s == "tinox").unwrap_or(false) {
        if import.path.len() >= 3 && import.path[1] == "core" {
            let module = import.path[2].as_str();
            if !CORE_MODULES.contains(&module) {
                if let Some(m) = missing_deps
                    .iter()
                    .find(|m| m.group == "tinox.core" && m.artifact_id == module)
                {
                    return Err(format!(
                        "tinox.toml declares tinox.core:{}:{} but it isn't installed — run `tinox install`.",
                        m.artifact_id, m.version
                    ));
                }
                return Err(format!(
                    "Cannot resolve import 'tinox.core.{}...': '{}' is an extended-tier stdlib module, not part of the always-available core — declare it in tinox.toml:\n\n  [[dependencies]]\n  group = \"tinox.core\"\n  artifactId = \"{}\"\n  version = \"1.0.0\"\n\nthen run `tinox install`.",
                    module, module, module
                ));
            }
        }
        let dir = stdlib_dir().ok_or_else(|| {
            format!(
                "Cannot resolve stdlib import '{}': TINOX_PATH not set and dev path not found",
                rel_file.display()
            )
        })?;
        let p = resolve_module_paths(&dir, &rel_file, &rel_dir)?.ok_or_else(|| {
            format!("Cannot resolve stdlib import '{}': no such file or directory", rel_file.display())
        })?;
        return Ok((p, ImportOrigin::External));
    }
    Err(format!("Cannot resolve import '{}': file not found", rel_file.display()))
}

fn resolve_imports(
    ast: &mut tinox_parser::SourceFile,
    base_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<(), String> {
    let imports: Vec<_> = ast
        .decls
        .iter()
        .filter_map(|d| {
            if let DeclKind::Import(i) = &d.node {
                Some(i.clone())
            } else {
                None
            }
        })
        .collect();

    // Collected separately and prepended (not appended) below: the
    // typechecker does a single linear pass over decls with no forward-
    // declaration/hoisting pass for interface-implementation records
    // (`interface_implementations` is populated lazily inside `check_class`
    // as each class is visited, see tinox-typecheck/src/lib.rs) — a `main()`
    // that upcasts an imported class to an imported interface it implements
    // must see both of those decls EARLIER in the list than `main` itself,
    // otherwise `types_compatible` sees an empty implements-record and
    // rejects the assignment. Single-file programs always satisfied this by
    // convention (types declared above `main`); merging via simple `extend`
    // put every import AFTER the importing file's own decls instead, which
    // broke exactly this pattern once one-type-per-file split types and
    // their `main()` driver across separate files.
    let mut imported_decls: Vec<tinox_parser::Decl> = Vec::new();

    for import in imports {
        let (full_paths, _origin) = resolve_import_target(&import, base_dir, dep_dirs, missing_deps)?;

        for full_path in full_paths {
            if let Some(decls) = resolve_and_merge_file(&full_path, visited, dep_dirs, missing_deps)? {
                imported_decls.extend(decls);
            }
        }
    }

    // Issue #194 Phase 2: same-namespace siblings are implicitly visible,
    // with zero `import` statement needed — same prepend-before-own-decls
    // treatment as an explicit import, via the same shared
    // resolve_and_merge_file (including this file's own `visited` entry
    // transparently excluding itself from its own sibling scan).
    for ns_path in own_namespace_paths(&ast.decls) {
        for sib_path in find_namespace_siblings(base_dir, &ns_path) {
            if let Some(decls) = resolve_and_merge_file(&sib_path, visited, dep_dirs, missing_deps)? {
                imported_decls.extend(decls);
            }
        }
    }

    // Drop Import and Module decls — they are resolved or informational only
    ast.decls
        .retain(|d| !matches!(&d.node, DeclKind::Import(_) | DeclKind::Module(_)));

    imported_decls.append(&mut ast.decls);
    ast.decls = imported_decls;

    Ok(())
}

/// Reads, parses, validates, and recursively resolves one file's own
/// imports (plus, transitively, its own same-namespace siblings) — the
/// common tail shared by explicit `import` resolution and issue #194 Phase
/// 2's same-namespace sibling auto-merge in `resolve_imports` above.
/// Returns `None` if `full_path` was already visited (nothing new to
/// merge — the standard `visited`-based cycle/dedup guard this whole
/// pipeline already relies on), `Some(decls)` otherwise.
fn resolve_and_merge_file(
    full_path: &Path,
    visited: &mut HashSet<PathBuf>,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<Option<Vec<tinox_parser::Decl>>, String> {
    if visited.contains(full_path) {
        return Ok(None);
    }
    visited.insert(full_path.to_path_buf());

    let source = fs::read_to_string(full_path)
        .map_err(|e| format!("Failed to read '{}': {}", full_path.display(), e))?;
    let tokens = Lexer::new(&source)
        .tokenize()
        .map_err(|e| format!("Lexer error in '{}': {:?}", full_path.display(), e))?;
    let mut parser = Parser::new(tokens);
    let mut imported = parser
        .parse()
        .map_err(|e| format!("Parse error in '{}': {:?}", full_path.display(), e))?;
    check_one_type_per_file(&imported.decls, full_path)?;
    check_no_top_level_fn(&imported.decls, full_path)?;
    check_namespace_path_matches(&imported.decls, full_path)?;
    stamp_file_identity(&mut imported.decls, full_path);

    let imported_dir = full_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    resolve_imports(&mut imported, &imported_dir, visited, dep_dirs, missing_deps)?;

    Ok(Some(imported.decls))
}

fn parse_into_cache(
    path: &Path,
    cache: &mut std::collections::HashMap<PathBuf, tinox_parser::SourceFile>,
) -> Result<(), String> {
    if cache.contains_key(path) {
        return Ok(());
    }
    let source = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read '{}': {}", path.display(), e))?;
    let tokens = Lexer::new(&source)
        .tokenize()
        .map_err(|e| format!("Lexer error in '{}': {:?}", path.display(), e))?;
    let mut ast = Parser::new(tokens)
        .parse()
        .map_err(|e| format!("Parse error in '{}': {:?}", path.display(), e))?;
    // Without node ids, infer_type's memoization (Bug 50) never activates,
    // making deep method chains exponential again -- caught live via the
    // e2e regression test for that exact bug (method_chain_linear) timing
    // out under check_explicit_imports (issue #194).
    tinox_parser::assign_node_ids(&mut ast);
    cache.insert(path.to_path_buf(), ast);
    Ok(())
}

/// Parses `path` (cached) and resolves its own `import` statements one hop,
/// returning each target alongside where it came from. Shared by
/// `check_explicit_imports`'s outer per-file loop and
/// `transitive_import_closure`'s inner expansion below — both need exactly
/// "what does this one file import."
fn file_direct_imports(
    path: &Path,
    cache: &mut std::collections::HashMap<PathBuf, tinox_parser::SourceFile>,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<Vec<(PathBuf, ImportOrigin)>, String> {
    parse_into_cache(path, cache)?;
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let imports: Vec<tinox_parser::ast::Import> = cache[path]
        .decls
        .iter()
        .filter_map(|d| if let DeclKind::Import(i) = &d.node { Some(i.clone()) } else { None })
        .collect();

    let mut out = Vec::new();
    for import in &imports {
        let (targets, origin) = resolve_import_target(import, &base_dir, dep_dirs, missing_deps)?;
        for t in targets {
            parse_into_cache(&t, cache)?;
            out.push((t, origin));
        }
    }

    // Issue #194 Phase 2: same-namespace siblings are implicitly visible —
    // this check must accept exactly what resolve_imports (main compile
    // path) now actually merges in, or it would report spurious "missing
    // import" violations for names Phase 2 already made legitimately
    // visible. ImportOrigin::Local since these are project-local files
    // check_explicit_imports's outer loop must also independently validate
    // in their own right, same as an explicitly imported one.
    for ns_path in own_namespace_paths(&cache[path].decls) {
        for sib in find_namespace_siblings(&base_dir, &ns_path) {
            if sib == path {
                continue;
            }
            parse_into_cache(&sib, cache)?;
            out.push((sib, ImportOrigin::Local));
        }
    }
    Ok(out)
}

/// Expands `seeds` (a file's own direct imports) into the full transitive
/// closure of everything THOSE files import, recursively -- both
/// `ImportOrigin::Local` and `External` (unlike `check_explicit_imports`'s
/// outer per-file loop, which only recurses into `Local` files to decide
/// what to independently VALIDATE, this closure is only ever used to build
/// a PRELUDE set, i.e. "what's visible," so it must include the same
/// external stdlib/dependency content the importing file's own imports
/// already trust).
///
/// Needed so a file that implements/extends a type from one of its own
/// direct imports doesn't ALSO have to import that type's own supertypes
/// by hand -- e.g. `Circle.tnx` (`examples/interface_extends/`) imports
/// `IDrawable` and implements it; `IDrawable extends IShape` in a separate
/// file `IDrawable.tnx` itself imports. Circle never spells "IShape"
/// anywhere in its own source, so requiring Circle to ALSO explicitly
/// import IShape would go beyond "explicit import for every name you
/// reference" into "explicit import for every transitive supertype of
/// every type you reference" -- stricter than normal language convention
/// (Java/C# don't require this either) and not what issue #194 asked for.
fn transitive_import_closure(
    seeds: Vec<PathBuf>,
    cache: &mut std::collections::HashMap<PathBuf, tinox_parser::SourceFile>,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<Vec<PathBuf>, String> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack = seeds;
    let mut ordered = Vec::new();
    while let Some(p) = stack.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        ordered.push(p.clone());
        for (target, _origin) in file_direct_imports(&p, cache, dep_dirs, missing_deps)? {
            stack.push(target);
        }
    }
    Ok(ordered)
}

/// Phase 1 of issue #194: a file may only reference names it declares
/// itself or explicitly imports directly — "some other file in the program
/// imports it transitively" (the ONLY way project-local cross-file
/// visibility has worked until now, since `resolve_imports` merges every
/// reachable file into one flat, whole-program decl list before
/// typechecking) is no longer sufficient. This is what let a file like
/// `PersonService.tnx` reference `Person`/`PersonDao`/`PersonDaoImpl`
/// without importing any of them, and it's exactly what makes tinox-lsp —
/// which only ever typechecks one open file plus its own declared
/// preludes, with no whole-program import graph to walk — unable to
/// resolve them, even though `tinox build` was fine with it.
///
/// Reuses `tinox_typecheck::typecheck_with_prelude` (already proven via
/// tinox-lsp's identical single-file-plus-preludes use) as the enforcement
/// mechanism: checking a file with the transitive closure of its own direct
/// imports (see `transitive_import_closure`) registered as prelude
/// declarations means any name the typechecker still can't resolve is, by
/// construction, a name that needed an explicit import and didn't have one.
///
/// The OUTER per-file loop below only recurses into (and independently
/// validates) files resolved as `ImportOrigin::Local` (relative to the
/// importing file's own directory). `ImportOrigin::External` targets
/// (installed dependencies, stdlib) are still included in prelude sets, but
/// never independently re-validated as their own primary target — mirroring
/// the precedent already established for the namespace-mirroring check
/// (issue #185): installed/stdlib content is pre-vetted, address-scoped,
/// immutable-per-version content this check has no business re-validating.
/// Without this split, virtually every directory-style stdlib module would
/// fail: e.g. `tinox.core.db`'s `DB.tnx` references `EntityQuery` with no
/// import of its own, relying entirely on the *consumer's* one
/// directory-level import merging both files as one unit — that's the
/// entire point of the one-type-per-file → directory-of-files convention,
/// not a bug to flag.
///
/// Runs as its own, independent traversal — deliberately not threaded
/// through `resolve_imports`'s own merge/`visited` state, so it re-parses
/// files `resolve_imports` also parses. A narrow, acceptable trade-off:
/// `.tnx` files are small and this is compile-time-only, and it keeps
/// `resolve_imports`'s existing, proven merge path (used by codegen)
/// completely untouched.
fn check_explicit_imports(
    entry_path: &Path,
    dep_dirs: &[PathBuf],
    missing_deps: &[pm::MissingDep],
) -> Result<(), String> {
    let mut cache: std::collections::HashMap<PathBuf, tinox_parser::SourceFile> =
        std::collections::HashMap::new();
    let entry_canon = entry_path
        .canonicalize()
        .map_err(|e| format!("Cannot read '{}': {}", entry_path.display(), e))?;
    let mut queue: Vec<PathBuf> = vec![entry_canon];
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut violations: Vec<String> = Vec::new();

    while let Some(path) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let direct = file_direct_imports(&path, &mut cache, dep_dirs, missing_deps)?;
        for (target, origin) in &direct {
            if *origin == ImportOrigin::Local {
                queue.push(target.clone());
            }
        }

        let seeds: Vec<PathBuf> = direct.iter().map(|(p, _)| p.clone()).collect();
        let prelude_paths = transitive_import_closure(seeds, &mut cache, dep_dirs, missing_deps)?;
        // Exclude `path` itself: an import cycle running back through `path`
        // (e.g. two mutually-`import`ing files, see tests/e2e/
        // inherited_static_dispatch's Base.tnx/Derived.tnx) would otherwise
        // put `path` in its own prelude set, causing `typecheck_with_prelude`
        // to call `register_declarations` on it twice — the second call's
        // plain `class_fields.insert` (not a merge) wipes out any inherited
        // fields the first `expand_class_inheritance` pass had just added,
        // producing a spurious "has no field" error. `path`'s own decls are
        // already `source` in the `typecheck_with_prelude` call below; they
        // have no business also being a prelude of themselves.
        let preludes: Vec<&tinox_parser::SourceFile> = prelude_paths
            .iter()
            .filter(|p| *p != &path)
            .filter_map(|p| cache.get(p))
            .collect();

        if let Err(bag) = tinox_typecheck::typecheck_with_prelude(&cache[&path], &preludes) {
            for err in bag.errors {
                // "missing return statement" is a real, independent
                // typechecker gap (return-completeness analysis doesn't
                // look inside try/catch bodies, confirmed unrelated to
                // imports/namespaces — found live via the external `demo`
                // project) with nothing to do with import visibility.
                // Reporting it here, wrapped in this function's own
                // "add the missing `import`" trailer, would be actively
                // misleading — skip it and let the REAL compile pipeline's
                // own typecheck pass (which runs right after this function
                // returns Ok, on the fully merged whole-program AST) catch
                // it properly, with its own accurate error instead.
                if err.message == "missing return statement" {
                    continue;
                }
                violations.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    err.span.start.line,
                    err.message
                ));
            }
        }
    }

    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\n\nEvery cross-namespace name must be explicitly imported in the file that uses it — \
         being imported transitively by some other file in the program is no longer sufficient \
         (see issue #194). Add the missing `import` statement(s) above.",
        violations.join("\n")
    ))
}

fn compile_file(input_path: &str, output_name: &str, opt: OptLevel) -> Result<(), String> {
    let source =
        fs::read_to_string(input_path).map_err(|e| format!("Failed to read file: {}", e))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer
        .tokenize()
        .map_err(|e| format!("Lexer error: {:?}", e))?;

    let mut parser = Parser::new(tokens);
    let mut ast = parser
        .parse()
        .map_err(|e| format!("Parse error: {:?}", e))?;
    check_one_type_per_file(&ast.decls, Path::new(input_path))?;
    check_no_top_level_fn(&ast.decls, Path::new(input_path))?;
    check_namespace_path_matches(&ast.decls, Path::new(input_path))?;
    stamp_file_identity(&mut ast.decls, Path::new(input_path));

    let base_dir = Path::new(input_path)
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut visited = HashSet::new();
    if let Ok(canonical) = Path::new(input_path).canonicalize() {
        visited.insert(canonical);
    }
    let (dep_dirs, missing_deps) = load_dep_dirs(&base_dir);
    let loaded_modules = loaded_tinox_core_modules(&base_dir);
    let startup_banner_enabled = read_startup_banner_config();
    let dev_config = read_dev_config().unwrap_or_default();
    if dev_config.enabled && opt == OptLevel::Release {
        eprintln!(
            "warning: [dev] is enabled in a release build -- the dev UI introspection API \
             will be reachable on 127.0.0.1:{} at runtime",
            dev_config.port
        );
    }
    resolve_imports(&mut ast, &base_dir, &mut visited, &dep_dirs, &missing_deps)
        .map_err(|e| format!("Import error: {}", e))?;
    check_explicit_imports(Path::new(input_path), &dep_dirs, &missing_deps)
        .map_err(|e| format!("Import error: {}", e))?;
    // NodeIds for the type table (typecheck → codegen)
    tinox_parser::assign_node_ids(&mut ast);

    let mut typechecker = tinox_typecheck::TypeChecker::new();
    typechecker
        .check(&ast)
        .map_err(|e| format!("Type error:\n{}", e))?;

    let (iface_methods, class_implements) = typechecker.interface_info();

    // Annotation processing pass
    let ann_result = tinox_typecheck::annotations::process_annotations(&ast);
    for warning in &ann_result.deprecated_warnings {
        eprintln!("warning: {}", warning);
    }
    for route in &ann_result.route_entries {
        eprintln!("  route: {} {} -> {}.{}", route.method, route.path, route.class_name, route.method_name);
    }

    // `class Main { fnc main() -> Int32 }` is now the mandatory program
    // entry point (shape itself is still validated with a real Span by
    // emit_class_main_entry_point in codegen) -- @Command CLI programs are
    // exempt, they dispatch via argv through their own, separately
    // generated tinox_main and were never part of this unification.
    if ann_result.cli_commands.is_empty() && !has_class_named_main(&ast.decls) {
        return Err(format!(
            "'{}' has no `class Main {{ fnc main() -> Int32 }}` -- every Tinox program requires this as its entry point now (create src/Main.tnx, see `tinox new` for the scaffold shape); @GET/@Http3RestController/@WebsocketEndpoint/@Amqp10Consumer/@Amqp091Consumer classes run alongside it instead of providing their own implicit main",
            input_path
        ));
    }

    // @Transactional is postgres-only in v1 (issue #191): the connection
    // pool + BEGIN/COMMIT/ROLLBACK primitives it compiles down to only
    // exist for that driver (runtime.c's sqlite/mysql tinox_db_tx_* stubs
    // hard-abort if ever actually called, as a second line of defense, but
    // that's meant to catch a bug in THIS check, not substitute for it).
    // A hard compile error here, not a silent no-op or a runtime surprise
    // the first time a transactional method actually runs. Checked here,
    // before ann_result.transactional_methods is moved into the
    // set_annotation_info() call further down.
    if !ann_result.transactional_methods.is_empty() {
        let driver_cfg = read_database_config();
        let driver = driver_cfg.as_ref().map(|c| c.driver.as_str()).unwrap_or("");
        if driver != "postgres" {
            let (class_name, method_name) = ann_result.transactional_methods.iter().next().unwrap();
            return Err(format!(
                "'{class_name}.{method_name}' is @Transactional, but [database] driver is {} -- \
                 @Transactional is only supported for driver = \"postgres\" in this version",
                if driver.is_empty() { "not configured".to_string() } else { format!("\"{driver}\"") }
            ));
        }
    }

    let route_entries: Vec<tinox_codegen::RouteEntry> = ann_result
        .route_entries
        .iter()
        .map(|r| tinox_codegen::RouteEntry {
            http_method: r.method.clone(),
            path: r.path.clone(),
            class_name: r.class_name.clone(),
            method_name: r.method_name.clone(),
            status_code: r.status_code,
            produces: r.produces.clone(),
            consumes: r.consumes.clone(),
            auth_type: r.auth_type.clone(),
            oidc_roles: r.oidc_roles.clone(),
            is_static: r.is_static,
            params: convert_route_params(&r.params),
            return_type: r.return_type.clone(),
        })
        .collect();

    // Multiple @WebsocketEndpoint classes are fine now (Phase 4: each is
    // spawned on its own thread by emit_tinox_main_bootstrap, no longer
    // competing for a single auto-run `main`) -- but unlike AMQP consumers
    // (where several consumers sharing one broker/port on different queues
    // is normal), two endpoints binding the *same* port is a real, easily
    // checkable mistake: the second one's WsServer_listen would silently
    // fail to bind at runtime with no compile-time signal otherwise. Port
    // resolution mirrors emit_ws_code's exactly (explicit port, else
    // TINOX_PORT, else 8080) so this can't disagree with what actually gets
    // bound.
    {
        let mut by_port: std::collections::HashMap<i64, Vec<&str>> = std::collections::HashMap::new();
        for e in &ann_result.ws_endpoints {
            let port = e.port
                .or_else(|| std::env::var("TINOX_PORT").ok().and_then(|s| s.parse::<i64>().ok()))
                .unwrap_or(8080);
            by_port.entry(port).or_default().push(e.class_name.as_str());
        }
        for (port, classes) in &by_port {
            if classes.len() > 1 {
                return Err(format!(
                    "@WebsocketEndpoint classes {} all resolve to port {port} -- each needs a distinct port (pass it explicitly: @WebsocketEndpoint(\"/path\", port))",
                    classes.join(", ")
                ));
            }
        }
    }
    let ws_endpoints: Vec<tinox_codegen::WsEndpointEntry> = ann_result
        .ws_endpoints
        .iter()
        .map(|e| tinox_codegen::WsEndpointEntry {
            class_name: e.class_name.clone(),
            path: e.path.clone(),
            port: e.port,
            on_open: e.on_open.clone(),
            on_message: e.on_message.clone(),
            on_close: e.on_close.clone(),
        })
        .collect();

    // Multiple @Amqp10Consumer classes are fine now (Phase 4: each spawned
    // on its own thread) -- unlike @WebsocketEndpoint, several consumers
    // sharing the same broker host/port but different queues/addresses is
    // a normal, expected shape, so there is no port-collision check here.
    let amqp10_consumers: Vec<tinox_codegen::Amqp10ConsumerEntry> = ann_result
        .amqp10_consumers
        .iter()
        .map(|e| tinox_codegen::Amqp10ConsumerEntry {
            class_name: e.class_name.clone(),
            host: e.host.clone(),
            port: e.port,
            user: e.user.clone(),
            pass: e.pass.clone(),
            address: e.address.clone(),
            on_message: e.on_message.clone(),
        })
        .collect();

    // Same reasoning as @Amqp10Consumer above: multiple @Amqp091Consumer
    // classes against the same broker/port but different queues is normal,
    // no port-collision check needed.
    let amqp091_consumers: Vec<tinox_codegen::Amqp091ConsumerEntry> = ann_result
        .amqp091_consumers
        .iter()
        .map(|e| tinox_codegen::Amqp091ConsumerEntry {
            class_name: e.class_name.clone(),
            host: e.host.clone(),
            port: e.port,
            vhost: e.vhost.clone(),
            user: e.user.clone(),
            pass: e.pass.clone(),
            queue: e.queue.clone(),
            on_message: e.on_message.clone(),
        })
        .collect();

    if ann_result.http3_rest_controllers.len() > 1 {
        return Err(format!(
            "found {} @Http3RestController classes ({}); v1 supports exactly one auto-run HTTP/3 REST controller per program",
            ann_result.http3_rest_controllers.len(),
            ann_result.http3_rest_controllers.iter().map(|e| e.class_name.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    // @TinoxUIApp (issue #215, Phase 4): at most one class per program
    // (same v1 restriction as @Http3RestController -- multiple apps in one
    // program is architecturally ambiguous for now), and exactly one
    // @View method on that class (zero = nothing to render; more than one
    // = ambiguous which builds the tree).
    if ann_result.tinoxui_apps.len() > 1 {
        return Err(format!(
            "found {} @TinoxUIApp classes ({}); v1 supports exactly one Tinox-UI app per program",
            ann_result.tinoxui_apps.len(),
            ann_result.tinoxui_apps.iter().map(|e| e.class_name.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let tinoxui_app: Option<tinox_codegen::TinoxUIAppEntry> = match ann_result.tinoxui_apps.first() {
        Some(app) => {
            if app.view_methods.is_empty() {
                return Err(format!(
                    "@TinoxUIApp class '{}' has no @View method -- exactly one method returning Component is required to build its UI",
                    app.class_name
                ));
            }
            if app.view_methods.len() > 1 {
                return Err(format!(
                    "@TinoxUIApp class '{}' has {} @View methods ({}); exactly one is required",
                    app.class_name,
                    app.view_methods.len(),
                    app.view_methods.join(", ")
                ));
            }
            Some(tinox_codegen::TinoxUIAppEntry {
                class_name: app.class_name.clone(),
                http_port: app.http_port,
                ws_port: app.ws_port,
                view_method: app.view_methods[0].clone(),
            })
        }
        None => None,
    };
    // Cross-kind combos (@Http3RestController + @WebsocketEndpoint/@Amqp10Consumer/
    // @Amqp091Consumer, or any of those + plain @GET/@Path routes) used to be
    // rejected here because each auto-run kind generated its own competing
    // @tinox_main. Since emit_tinox_main_bootstrap now spawns every kind on
    // its own thread from one unified @tinox_main, they can coexist -- only
    // more than one instance of the *same* kind (checked above) is still
    // disallowed.
    let http3_rest_controller: Option<tinox_codegen::Http3RestControllerEntry> = ann_result
        .http3_rest_controllers
        .first()
        .map(|e| tinox_codegen::Http3RestControllerEntry {
            class_name: e.class_name.clone(),
            port: e.port,
            cert_path: e.cert_path.clone(),
            key_path: e.key_path.clone(),
        });

    let di_components: Vec<tinox_codegen::DiComponentInfo> = ann_result.di_components
        .iter()
        .map(|c| tinox_codegen::DiComponentInfo {
            class_name: c.class_name.clone(),
            scope: match c.scope {
                tinox_typecheck::annotations::DiScope::Application => tinox_codegen::DiScope::Application,
                tinox_typecheck::annotations::DiScope::Startup => tinox_codegen::DiScope::Startup,
                tinox_typecheck::annotations::DiScope::HttpRequest => tinox_codegen::DiScope::HttpRequest,
            },
            inject_fields: c.inject_fields.iter().map(|f| tinox_codegen::DiInjectField {
                field_name: f.field_name.clone(),
                field_type: f.field_type.clone(),
            }).collect(),
        })
        .collect();

    let mut codegen = CodeGen::new();
    codegen.set_expr_value_types(typechecker.expr_value_types());
    codegen.set_interface_info(iface_methods, class_implements);
    codegen.set_loaded_modules(loaded_modules);
    codegen.set_startup_banner_enabled(startup_banner_enabled);
    codegen.set_dev_config(dev_config.enabled, dev_config.port);
    let config_fields: Vec<tinox_codegen::ConfigFieldInfo> = ann_result.config_fields
        .iter()
        .map(|f| tinox_codegen::ConfigFieldInfo {
            class_name: f.class_name.clone(),
            field_name: f.field_name.clone(),
            config_key: f.config_key.clone(),
            field_llvm_type: f.field_llvm_type.clone(),
        })
        .collect();
    let cli_commands: Vec<tinox_codegen::CliCommandInfo> = ann_result.cli_commands
        .iter()
        .map(|c| tinox_codegen::CliCommandInfo {
            class_name: c.class_name.clone(),
            cmd_name: c.cmd_name.clone(),
            description: c.description.clone(),
            version: c.version.clone(),
            options: c.options.iter().map(|o| tinox_codegen::CliOptionInfo {
                field_name: o.field_name.clone(),
                names: o.names.clone(),
                description: o.description.clone(),
                required: o.required,
                field_type: o.field_type.clone(),
            }).collect(),
            arguments: c.arguments.iter().map(|a| tinox_codegen::CliArgumentInfo {
                field_name: a.field_name.clone(),
                index: a.index,
                description: a.description.clone(),
                required: a.required,
                field_type: a.field_type.clone(),
            }).collect(),
        })
        .collect();
    let sensitive_fields: Vec<tinox_codegen::LogMaskFieldInfo> = ann_result.sensitive_fields
        .iter()
        .map(|f| tinox_codegen::LogMaskFieldInfo {
            class_name: f.class_name.clone(),
            field_name: f.field_name.clone(),
        })
        .collect();
    let masked_fields: Vec<tinox_codegen::LogMaskFieldInfo> = ann_result.masked_fields
        .iter()
        .map(|f| tinox_codegen::LogMaskFieldInfo {
            class_name: f.class_name.clone(),
            field_name: f.field_name.clone(),
        })
        .collect();
    let do_not_serialize_fields: Vec<tinox_codegen::LogMaskFieldInfo> = ann_result.do_not_serialize_fields
        .iter()
        .map(|f| tinox_codegen::LogMaskFieldInfo {
            class_name: f.class_name.clone(),
            field_name: f.field_name.clone(),
        })
        .collect();
    let metric_entries: Vec<tinox_codegen::MetricEntry> = ann_result.metric_entries
        .iter()
        .map(|m| tinox_codegen::MetricEntry {
            kind: match m.kind {
                tinox_typecheck::annotations::MetricKind::Timed   => tinox_codegen::MetricKind::Timed,
                tinox_typecheck::annotations::MetricKind::Counted => tinox_codegen::MetricKind::Counted,
                tinox_typecheck::annotations::MetricKind::Gauge   => tinox_codegen::MetricKind::Counted, // gauge on fields, handled separately
            },
            metric_name: m.metric_name.clone(),
            class_name:  m.class_name.clone(),
            fn_name:     m.fn_name.clone(),
        })
        .collect();
    codegen.set_annotation_info(tinox_codegen::AnnotationInfo {
        inline_fns: ann_result.inline_functions,
        inline_meths: ann_result.inline_methods,
        routes: route_entries,
        di_components,
        log_classes: ann_result.log_classes,
        config_fields,
        cli_commands,
        sensitive_fields,
        masked_fields,
        do_not_serialize_fields,
        json_serializable_classes: ann_result.json_serializable_classes,
        metric_entries,
        transactional_methods: ann_result.transactional_methods,
    });
    codegen.set_metrics_config(read_metrics_config());
    let entity_entries: Vec<tinox_codegen::EntityEntry> = ann_result.entity_entries
        .iter()
        .map(|e| tinox_codegen::EntityEntry {
            class_name: e.class_name.clone(),
            table_name: e.table_name.clone(),
            fields: e.fields.iter().map(|f| tinox_codegen::EntityFieldEntry {
                field_name: f.field_name.clone(),
                column_name: f.column_name.clone(),
                is_id: f.is_id,
                is_generated: f.is_generated,
                not_null: f.not_null,
                field_llvm_type: f.field_llvm_type.clone(),
            }).collect(),
        })
        .collect();
    codegen.set_entity_entries(entity_entries);
    codegen.set_ws_endpoints(ws_endpoints);
    codegen.set_amqp10_consumers(amqp10_consumers);
    codegen.set_amqp091_consumers(amqp091_consumers);
    codegen.set_http3_rest_controller(http3_rest_controller);
    codegen.set_tinoxui_apps(tinoxui_app.into_iter().collect());
    let db_config_for_codegen = read_database_config();
    codegen.set_db_url(db_config_for_codegen.as_ref().map(|c| c.url.clone()));
    codegen.set_db_pool_size(db_config_for_codegen.as_ref().map(|c| c.pool as i64).unwrap_or(5));
    if dev_config.enabled {
        codegen.set_dev_info(
            read_project_name().unwrap_or_else(|| "app".to_string()),
            read_project_version().unwrap_or_else(|| "0.0.0".to_string()),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        codegen.set_dev_config_summary_json(build_dev_config_summary_json());
        // `tinox test` takes a single positional arg as a FILE path, not a
        // project directory (collect_test_files, main.rs) -- there's no
        // "run against this directory" flag, only "run with no args,
        // scanning tests/+src/ under the nearest tinox.toml found by
        // walking up from the current directory". So `cd` into the
        // project root first and invoke it bare, letting its own
        // discovery do the work, instead of passing the root as an
        // argument (which would make it try to read the directory itself
        // as a single test file and fail with "Is a directory").
        let test_command = match (std::env::current_exe(), find_project_root()) {
            (Ok(exe), Some(root)) => format!(
                "cd {} && {} test 2>&1",
                shell_quote(&root.to_string_lossy()),
                shell_quote(&exe.to_string_lossy())
            ),
            _ => String::new(),
        };
        codegen.set_dev_test_command(test_command);
    }
    codegen
        .gen(&ast)
        .map_err(|e| format!("Codegen error: {:?}", e))?;

    let ir = codegen.into_ir();
    let ir_path = format!("{}.ll", output_name);
    fs::write(&ir_path, ir).map_err(|e| format!("Failed to write IR: {}", e))?;

    compile_ll_to_exe(&ir_path, output_name, opt)
}

/// IR verifier gate: run the LLVM verifier on the generated .ll so invalid IR
/// fails immediately with a real diagnostic (instead of a bare "opt failed"/
/// "llc failed" later — or a silent miscompile in Debug mode, where opt is
/// skipped entirely). Invalid IR is always a codegen bug, never a user error.
fn verify_ir(ir_path: &str) -> Result<(), String> {
    let out = Command::new("opt")
        .args(["-passes=verify", "-disable-output", ir_path])
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let excerpt: Vec<&str> = stderr.lines().take(20).collect();
            Err(format!(
                "internal compiler error: generated invalid LLVM IR ({})\n{}\n\
                 This is a Tinox codegen bug — please report it with the source file.",
                ir_path,
                excerpt.join("\n")
            ))
        }
        // opt not installed — skip the gate, the normal pipeline will complain.
        Err(_) => Ok(()),
    }
}

fn compile_ll_to_exe(ir_path: &str, output_name: &str, opt: OptLevel) -> Result<(), String> {
    let obj_path = format!("{}.o", output_name);

    verify_ir(ir_path)?;

    let (llc_opt_flag, opt_flag) = match opt {
        OptLevel::Release => ("-O3", "-O3"),
        OptLevel::Debug   => ("-O0", "-O0"),
    };

    // In Release mode, try opt for mid-level optimizations before llc.
    // In Debug mode skip opt entirely for faster compile times.
    let opt_available = opt == OptLevel::Release && Command::new("opt")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let llc_input: String;
    let bc_path_opt: Option<String>;

    if opt_available {
        let bc_path = format!("{}.opt.bc", output_name);
        let opt_status = Command::new("opt")
            .args([opt_flag, "-o", &bc_path, ir_path])
            .status()
            .map_err(|e| format!("opt failed: {}", e))?;
        if !opt_status.success() {
            return Err("opt failed".to_string());
        }
        llc_input = bc_path.clone();
        bc_path_opt = Some(bc_path);
    } else {
        llc_input = ir_path.to_string();
        bc_path_opt = None;
    }

    let llc_status = Command::new("llc")
        .args([
            llc_opt_flag,
            "-march=x86-64",
            "-filetype=obj",
            "-o",
            &obj_path,
            &llc_input,
        ])
        .status()
        .map_err(|e| format!("llc failed: {}", e))?;

    if !llc_status.success() {
        return Err("llc failed".to_string());
    }

    if let Some(bc_path) = bc_path_opt {
        let _ = fs::remove_file(&bc_path);
    }

    let runtime_src = runtime_c_path().ok_or_else(|| {
        "Cannot find runtime.c (checked the dev checkout path and /usr/share/tinox/runtime.c)".to_string()
    })?;
    let runtime_src = runtime_src.to_string_lossy().into_owned();
    let runtime_obj = format!("{}_runtime.o", output_name);

    let db_cfg = read_database_config();
    let db_driver = db_cfg.as_ref().map(|c| c.driver.as_str()).unwrap_or("");

    // Extra C flags from the environment, e.g. for sanitizer runs:
    // TINOX_CFLAGS="-fsanitize=address -g -DTINOX_NO_GC" (see make asan)
    let extra_cflags: Vec<String> = std::env::var("TINOX_CFLAGS")
        .map(|v| v.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    // HTTPS/TLS server: on by default. Enables the TLS code in runtime.c
    // (-DTINOX_TLS) and links OpenSSL (-lssl -lcrypto). Opt out via
    // TINOX_TLS=0, e.g. if no OpenSSL is available to build with.
    let tls_enabled = std::env::var("TINOX_TLS").map(|v| v != "0" && v != "false").unwrap_or(true);

    // HTTP/3 (QUIC) server: opt-in, default OFF -- unlike TLS (OpenSSL is
    // near-universally installed), ngtcp2/nghttp3 are far less common on a
    // typical build machine, so defaulting this on would break `tinox
    // build` with a compile error on any system lacking them, rather than
    // the graceful runtime -1 the rest of this file's opt-out flags give.
    // Also gated on tls_enabled: ngtcp2_crypto_ossl needs OpenSSL underneath,
    // so TINOX_TLS=0 implies HTTP/3 support is unavailable regardless of
    // TINOX_HTTP3.
    let http3_enabled = tls_enabled
        && std::env::var("TINOX_HTTP3")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

    // Where does this system keep libpq-fe.h? Not the same place everywhere:
    // on this dev machine (Arch) `libpq` flattens its headers straight into
    // /usr/include, so a bare `#include <libpq-fe.h>` "just worked" there --
    // but CI (Ubuntu, PGDG's own postgresql-client apt repo) nests them
    // under /usr/include/postgresql/ instead, which clang/gcc's default
    // search path does NOT include. Found the hard way: local testing only
    // ever ran on this machine, so the CI failure (`libpq-fe.h: No such
    // file or directory`, package reported as "already the newest version")
    // was invisible until the real GitHub Actions run. `pg_config
    // --includedir` is the portable, canonical way to ask libpq itself
    // where its headers live, on any distro -- not a hardcoded guess at a
    // specific nested path.
    let pg_include_dir = if db_driver == "postgres" {
        Command::new("pg_config")
            .arg("--includedir")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        None
    };
    let mut cc_args = vec!["-c", &runtime_src, "-o", &runtime_obj, "-O3"];
    let pg_include_flag = pg_include_dir.as_ref().map(|d| format!("-I{d}"));
    if db_driver == "postgres" {
        cc_args.push("-DTINOX_DB_POSTGRES");
        if let Some(flag) = &pg_include_flag {
            cc_args.push(flag);
        }
    } else if db_driver == "mysql" {
        cc_args.push("-DTINOX_DB_MYSQL");
    } else if db_driver == "sqlite" {
        cc_args.push("-DTINOX_DB_SQLITE");
    }
    if tls_enabled {
        cc_args.push("-DTINOX_TLS");
    }
    if http3_enabled {
        cc_args.push("-DTINOX_HTTP3");
    }
    cc_args.extend(extra_cflags.iter().map(|s| s.as_str()));
    let cc_status = Command::new("cc")
        .args(&cc_args)
        .status()
        .map_err(|e| format!("Failed to compile runtime: {}", e))?;

    if !cc_status.success() {
        return Err("Runtime compilation failed".to_string());
    }

    // -lz: WebSocket permessage-deflate (issue #122, RFC 7692) raw-deflate
    // wrappers in runtime.c. Unlike -lssl/-lcrypto (opt-out via TINOX_TLS,
    // since OpenSSL isn't always available in minimal build environments),
    // zlib is assumed always present — same tier as -lm/-lpthread/-lgc, no
    // opt-out needed.
    let mut link_args = vec![obj_path.as_str(), runtime_obj.as_str(), "-o", output_name, "-lm", "-lpthread", "-lgc", "-lz", "-no-pie"];
    if db_driver == "postgres" {
        link_args.push("-lpq");
    } else if db_driver == "mysql" {
        link_args.push("-lmysqlclient");
    } else if db_driver == "sqlite" {
        link_args.push("-lsqlite3");
    }
    if tls_enabled {
        link_args.push("-lssl");
        link_args.push("-lcrypto");
    }
    if http3_enabled {
        link_args.push("-lngtcp2");
        link_args.push("-lngtcp2_crypto_ossl");
        link_args.push("-lnghttp3");
    }
    link_args.extend(extra_cflags.iter().map(|s| s.as_str()));
    let link_status = Command::new("cc")
        .args(&link_args)
        .status()
        .map_err(|e| format!("Failed to link: {}", e))?;

    if !link_status.success() {
        return Err("Linking failed".to_string());
    }

    let _ = fs::remove_file(&obj_path);
    let _ = fs::remove_file(&runtime_obj);

    Ok(())
}

#[cfg(test)]
mod one_type_per_file_tests {
    use super::*;

    fn parse_decls(src: &str) -> Vec<tinox_parser::Decl> {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        parser.parse().expect("parse").decls
    }

    #[test]
    fn zero_types_ok() {
        let decls = parse_decls("fn main() -> Int32 { return 0; }");
        assert!(check_one_type_per_file(&decls, Path::new("script.tnx")).is_ok());
    }

    #[test]
    fn one_type_matching_name_ok() {
        let decls = parse_decls("class Player { var hp: Int64; }");
        assert!(check_one_type_per_file(&decls, Path::new("Player.tnx")).is_ok());
    }

    #[test]
    fn one_type_mismatched_name_err() {
        let decls = parse_decls("class Player { var hp: Int64; }");
        let err = check_one_type_per_file(&decls, Path::new("player.tnx")).unwrap_err();
        assert!(err.contains("Player"), "error should name the type: {err}");
        assert!(err.contains("Player.tnx"), "error should name the required filename: {err}");
    }

    #[test]
    fn two_types_err() {
        let decls = parse_decls("class A { var x: Int64; } class B { var y: Int64; }");
        let err = check_one_type_per_file(&decls, Path::new("AB.tnx")).unwrap_err();
        assert!(err.contains('A') && err.contains('B'), "error should list both types: {err}");
    }

    #[test]
    fn namespace_wrapped_type_matching_name_ok() {
        let decls = parse_decls("namespace tinox.core.base64 { class Base64 { var x: Int64; } }");
        assert!(check_one_type_per_file(&decls, Path::new("Base64.tnx")).is_ok());
    }

    #[test]
    fn namespace_wrapped_type_mismatched_name_err() {
        let decls = parse_decls("namespace tinox.core.base64 { class Base64 { var x: Int64; } }");
        assert!(check_one_type_per_file(&decls, Path::new("base64.tnx")).is_err());
    }

    #[test]
    fn interface_and_enum_count_too() {
        let decls = parse_decls("interface Shape { fn area() -> Int64; } enum Color { Red, Blue }");
        let err = check_one_type_per_file(&decls, Path::new("x.tnx")).unwrap_err();
        assert!(err.contains("Shape") && err.contains("Color"), "error should list both: {err}");
    }
}

#[cfg(test)]
mod namespace_path_matches_tests {
    use super::*;
    use std::fs;

    fn parse_decls(src: &str) -> Vec<tinox_parser::Decl> {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        parser.parse().expect("parse").decls
    }

    /// Builds a throwaway manifest dir (a `tinox.toml`, optionally a `src/`
    /// subdir) under the OS temp dir, uniquely named per test + pid so
    /// parallel `cargo test` runs never collide.
    fn make_manifest_dir(name: &str, with_src: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tinox_ns_path_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tinox.toml"), "[package]\nname = \"t\"\n").unwrap();
        if with_src {
            fs::create_dir_all(dir.join("src")).unwrap();
        }
        dir
    }

    fn write_file(root: &Path, rel: &str, content: &str) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn no_namespace_is_exempt() {
        let dir = make_manifest_dir("no_ns", true);
        let path = write_file(&dir, "src/Anywhere.tnx", "class Foo { var x: Int64; }");
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_path_under_src_ok() {
        let dir = make_manifest_dir("match_src", true);
        let path = write_file(
            &dir,
            "src/tinox/core/amqp10/Amqp10Connection.tnx",
            "namespace tinox.core.amqp10 { class Amqp10Connection { var x: Int64; } }",
        );
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_path_flat_no_src_ok() {
        // stdlib-ext convention: .tnx files sit directly beside tinox.toml,
        // no `src/` layer.
        let dir = make_manifest_dir("match_flat", false);
        let path = write_file(
            &dir,
            "tinox/core/amqp10/Amqp10Connection.tnx",
            "namespace tinox.core.amqp10 { class Amqp10Connection { var x: Int64; } }",
        );
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_path_under_tests_ok() {
        // The tests/<namespace-path>/<TypeName>Test.tnx convention -- a
        // real gap hit live while writing the first example test: this
        // must be its own recognized root, not just checked against
        // src/ or the bare manifest dir.
        let dir = make_manifest_dir("match_tests", true);
        let path = write_file(
            &dir,
            "tests/tinox/core/amqp10/Amqp10ConnectionTest.tnx",
            "namespace tinox.core.amqp10 { class Amqp10ConnectionTest { var x: Int64; } }",
        );
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mismatched_path_err() {
        let dir = make_manifest_dir("mismatch", true);
        let path = write_file(
            &dir,
            "src/wrong/place/Amqp10Connection.tnx",
            "namespace tinox.core.amqp10 { class Amqp10Connection { var x: Int64; } }",
        );
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        let err = check_namespace_path_matches(&decls, &path).unwrap_err();
        assert!(
            err.contains("tinox.core.amqp10"),
            "error should name the namespace: {err}"
        );
        assert!(
            err.contains(&format!(
                "{}",
                dir.join("src/tinox/core/amqp10/Amqp10Connection.tnx").display()
            )),
            "error should name the expected path: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_manifest_ancestor_is_exempt() {
        // A namespaced file with no `tinox.toml` anywhere above it in the
        // filesystem -- nothing to validate against, so this must not
        // hard-fail.
        let dir = std::env::temp_dir().join(format!(
            "tinox_ns_path_test_no_manifest_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = write_file(
            &dir,
            "Whatever.tnx",
            "namespace tinox.core.amqp10 { class Amqp10Connection { var x: Int64; } }",
        );
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_dependency_without_its_own_manifest_is_exempt() {
        // Reproduces a real `make check` failure: a project depends on a
        // pre-existing core-tier package published before this migration
        // (no `tinox.toml` of its own inside the installed dir). Without
        // the `.tinox` path-component guard, `find_project_root_from`
        // walks straight past it into the CONSUMING project's own
        // manifest and wrongly validates the dependency's file against
        // ITS layout instead of skipping.
        let dir = make_manifest_dir("dep_no_manifest", true);
        let dep_dir = dir.join(".tinox/deps/tinox.core/socket/1.0.0");
        let path = write_file(
            &dep_dir,
            "tinox/core/socket/Socket.tnx",
            "namespace tinox.core.socket { class Socket { var x: Int64; } }",
        );
        // No tinox.toml written anywhere under dep_dir -- matches the real
        // pre-existing package this reproduces.
        let decls = parse_decls(&fs::read_to_string(&path).unwrap());
        assert!(check_namespace_path_matches(&decls, &path).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod new_project_files_tests {
    use super::*;

    // #155/#159: the scaffold must produce a project that compiles and
    // tests cleanly under the one-class-per-file + mandatory
    // class-qualified-entry-point rules (#149) — not the pre-v2.0.0 bare
    // `fn main()` shape this used to generate.

    #[test]
    fn main_tnx_is_class_qualified_not_bare_fn() {
        let (_, main_tnx, _, _) = new_project_files("demo");
        assert!(main_tnx.contains("class Main"), "{main_tnx}");
        assert!(main_tnx.contains("fnc main() -> Int32"), "{main_tnx}");
        assert!(!main_tnx.trim_start().starts_with("fn main"), "{main_tnx}");
    }

    #[test]
    fn toml_declares_entry_matching_the_scaffolded_file_name() {
        let (toml, _, _, _) = new_project_files("demo");
        assert_eq!(read_project_entry(&toml), Some("src/Main.tnx".to_string()));
    }

    #[test]
    fn test_class_name_matches_its_own_scaffolded_file_name() {
        let (_, _, test_class, test_tnx) = new_project_files("demo");
        assert_eq!(test_class, "demoTests");
        assert!(test_tnx.contains(&format!("class {test_class}")), "{test_tnx}");
        // The file this content is written to (new_project) is named
        // "{test_class}.tnx" — the whole point being that the class name
        // inside the content and the file name it's written under match.
    }
}

#[cfg(test)]
mod read_project_entry_tests {
    use super::*;

    #[test]
    fn entry_field_found() {
        let toml = "[package]\nname = \"foo\"\nentry = \"src/Main.tnx\"\noutput = \"foo\"\n";
        assert_eq!(read_project_entry(toml), Some("src/Main.tnx".to_string()));
    }

    #[test]
    fn no_entry_field_returns_none() {
        let toml = "[package]\nname = \"foo\"\noutput = \"foo\"\n";
        assert_eq!(read_project_entry(toml), None);
    }

    #[test]
    fn entry_outside_package_section_ignored() {
        let toml = "[build]\nentry = \"not/this/one.tnx\"\n[package]\nname = \"foo\"\n";
        assert_eq!(read_project_entry(toml), None);
    }

    #[test]
    fn entry_field_whitespace_tolerant() {
        let toml = "[package]\nentry=\"src/Main.tnx\"\n";
        assert_eq!(read_project_entry(toml), Some("src/Main.tnx".to_string()));
    }
}

#[cfg(test)]
mod no_top_level_fn_tests {
    use super::*;

    fn parse_decls(src: &str) -> Vec<tinox_parser::Decl> {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        parser.parse().expect("parse").decls
    }

    #[test]
    fn class_only_ok() {
        let decls = parse_decls("class Main { fnc main() -> Int32 { return 0; } }");
        assert!(check_no_top_level_fn(&decls, Path::new("Main.tnx")).is_ok());
    }

    #[test]
    fn top_level_fn_err() {
        let decls = parse_decls("fn main() -> Int32 { return 0; }");
        let err = check_no_top_level_fn(&decls, Path::new("main.tnx")).unwrap_err();
        assert!(err.contains("main"), "error should name the function: {err}");
    }

    #[test]
    fn multiple_top_level_fns_err() {
        let decls = parse_decls("fn helper() -> Int64 { return 1; } fn main() -> Int32 { return 0; }");
        let err = check_no_top_level_fn(&decls, Path::new("x.tnx")).unwrap_err();
        assert!(err.contains("helper") && err.contains("main"), "error should list both: {err}");
    }

    #[test]
    fn extern_fn_stays_legal() {
        // `extern fn` (StmtKind::Empty body) is an FFI binding, not a free
        // function in the issue #149 sense -- must not trip the check.
        let decls = parse_decls("extern fn tinoxSomeRuntimeFn(x: Int64) -> Int64;");
        assert!(check_no_top_level_fn(&decls, Path::new("x.tnx")).is_ok());
    }

    #[test]
    fn namespace_wrapped_fn_err() {
        let decls = parse_decls("namespace tinox.core.demo { fn helper() -> Int64 { return 1; } }");
        let err = check_no_top_level_fn(&decls, Path::new("x.tnx")).unwrap_err();
        assert!(err.contains("helper"), "error should name the function: {err}");
    }
}

#[cfg(test)]
mod docker_tests {
    use super::*;

    #[test]
    fn parse_toml_array_ints() {
        assert_eq!(parse_toml_array("[8080, 9090]"), vec!["8080", "9090"]);
    }

    #[test]
    fn parse_toml_array_strings_and_trailing_comma() {
        assert_eq!(parse_toml_array("[\"libpq5\", \"foo\", ]"), vec!["libpq5", "foo"]);
    }

    #[test]
    fn parse_toml_array_empty() {
        assert_eq!(parse_toml_array("[]"), Vec::<String>::new());
    }

    #[test]
    fn runtime_packages_baseline_no_tls() {
        let pkgs = compute_runtime_packages(false, None, &[]);
        assert_eq!(pkgs, vec!["libgc1", "zlib1g", "ca-certificates"]);
    }

    #[test]
    fn runtime_packages_tls_adds_libssl() {
        let pkgs = compute_runtime_packages(true, None, &[]);
        assert!(pkgs.contains(&"libssl3".to_string()));
    }

    #[test]
    fn runtime_packages_db_driver_mapped() {
        assert!(compute_runtime_packages(false, Some("postgres"), &[]).contains(&"libpq5".to_string()));
        assert!(compute_runtime_packages(false, Some("mysql"), &[]).contains(&"libmariadb3".to_string()));
        assert!(compute_runtime_packages(false, Some("sqlite"), &[]).contains(&"libsqlite3-0".to_string()));
    }

    #[test]
    fn runtime_packages_extra_appended_without_duplicates() {
        let pkgs = compute_runtime_packages(true, None, &["libssl3".to_string(), "libfoo".to_string()]);
        assert_eq!(pkgs.iter().filter(|p| *p == "libssl3").count(), 1);
        assert!(pkgs.contains(&"libfoo".to_string()));
    }

    #[test]
    fn dockerfile_includes_expose_and_entrypoint() {
        let out = generate_dockerfile("debian:bookworm-slim", &["libgc1".to_string()], "myapp", &[8080, 9090]);
        assert!(out.starts_with("FROM debian:bookworm-slim\n"));
        assert!(out.contains("RUN apt-get update"));
        // Every package line (including the last, single one here) must
        // keep its line-continuation backslash -- the `&& rm -rf ...`
        // line right after it is not a standalone RUN on its own and
        // `docker build` hard-errors on "unknown instruction: &&" if a
        // package line drops it.
        assert!(out.contains("    libgc1 \\\n"), "package line must continue with backslash:\n{out}");
        assert!(out.contains("    && rm -rf /var/lib/apt/lists/*"));
        assert!(out.contains("COPY myapp /app/myapp"));
        assert!(out.contains("EXPOSE 8080 9090"));
        assert!(out.contains("ENTRYPOINT [\"/app/myapp\"]"));
    }

    #[test]
    fn dockerfile_multi_package_all_lines_continue() {
        let out = generate_dockerfile(
            "debian:bookworm-slim",
            &["libgc1".to_string(), "zlib1g".to_string(), "libssl3".to_string()],
            "myapp",
            &[],
        );
        for pkg in ["libgc1", "zlib1g", "libssl3"] {
            assert!(out.contains(&format!("    {pkg} \\\n")), "{pkg} line must continue with backslash:\n{out}");
        }
    }

    #[test]
    fn dockerfile_no_ports_omits_expose() {
        let out = generate_dockerfile("debian:bookworm-slim", &[], "myapp", &[]);
        assert!(!out.contains("EXPOSE"));
        assert!(!out.contains("apt-get"));
    }

    #[test]
    fn flag_value_found_and_missing() {
        let args = vec!["--tag".to_string(), "myapp:v1".to_string()];
        assert_eq!(parse_flag_value(&args, "--tag"), Some("myapp:v1".to_string()));
        assert_eq!(parse_flag_value(&args, "--other"), None);
    }
}

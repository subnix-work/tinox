//! Bootstrap inspection uses real source resolution and must never start the app.
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let project = Self(std::env::temp_dir().join(format!(
            "tinox-explain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        project.write(
            "tinox.toml",
            "[package]\ncompiler = \"2.8.0\"\nname=\"explain\"\nversion=\"0.1.0\"\nentry=\"src/Main.tnx\"\n",
        );
        project.write(
            "src/Main.tnx",
            "class Main { fnc main() -> Int32 { println(\"MAIN_MUST_NOT_RUN\")\nreturn 0 } }",
        );
        project
    }
    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .arg(action)
            .env("TINOX_HOME", &self.0)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"))
            .env_remove("TINOX_PORT");
        command
    }
    fn query(&self) -> Value {
        let output = self
            .command("explain")
            .args(["--bootstrap", "--json"])
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["success"], true);
        value
    }
    fn install_local(&self, modules: &[&str]) {
        let mut manifest = fs::read_to_string(self.0.join("tinox.toml")).unwrap();
        for module in modules {
            manifest.push_str(&format!("\n[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"9.9.9\"\n"));
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../tinox-core-ext")
                .join(module)
                .join("tinox");
            let target = self.0.join(format!(
                ".tinox/repository/central/tinox.core/{module}/9.9.9/tinox"
            ));
            copy_tree(&source, &target);
        }
        self.write("tinox.toml", &manifest);
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}
fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
}
#[test]
fn main_scopes_injections_config_and_database_are_explained_without_backend_or_execution() {
    let project = Project::new();
    project.write("src/Main.tnx", "import Service\nimport Request\nclass Main { fnc main() -> Int32 { println(\"MAIN_MUST_NOT_RUN\")\nreturn 0 } }");
    project.write("src/Dependency.tnx", "@ApplicationComponent class Dependency {}");
    project.write("src/Service.tnx", "import Dependency\n@Startup class Service { @Inject var dependency: Dependency\n@Config(\"APP_PASSWORD\") var password: String = \"FIELD_SECRET\" }");
    project.write("src/Request.tnx", "import Dependency\n@HttpRequestScoped class Request { @Inject var dependency: Dependency }");
    let manifest = fs::read_to_string(project.0.join("tinox.toml")).unwrap();
    project.write("tinox.toml", &format!("{manifest}\n[database]\ndriver=\"postgres\"\nurl=\"postgres://USER_SECRET:DB_SECRET@127.0.0.1:1/db\"\n"));
    let result = project.query();
    let plan = &result["plan"];
    assert_eq!(plan["entry_point"], "Main::main");
    assert_eq!(plan["initializers"][0]["priority"], 10);
    assert_eq!(plan["initializers"][1]["components"], json!(["Service"]));
    let service = plan["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["class"] == "Service")
        .unwrap();
    assert_eq!(service["scope"], "startup_singleton");
    assert_eq!(service["injections"][0]["provider"], "Dependency_di_get");
    let request = plan["components"].as_array().unwrap().iter()
        .find(|c| c["class"] == "Request").unwrap();
    assert_eq!(request["scope"], "request_context");
    assert_eq!(request["provider"], "Request_di_create");
    assert_eq!(request["injections"][0]["provider"], "Dependency_di_get");
    assert_eq!(request["injections"][0]["target_scope"], "application_singleton");
    assert_eq!(plan["configuration"][0]["value"], Value::Null);
    assert_eq!(plan["configuration"][0]["key"], "APP_PASSWORD");
    let encoded = result.to_string();
    for secret in [
        "FIELD_SECRET",
        "DB_SECRET",
        "USER_SECRET",
        "MAIN_MUST_NOT_RUN",
    ] {
        assert!(!encoded.contains(secret));
    }
    assert!(!project.0.join("explain").exists());
    let text = project
        .command("explain")
        .arg("--bootstrap")
        .env("PATH", "")
        .env("APP_PASSWORD", "ENV_SECRET")
        .output()
        .unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Inject: dependency -> Dependency via Dependency_di_get"));
    for secret in ["FIELD_SECRET", "DB_SECRET", "USER_SECRET", "ENV_SECRET"] {
        assert!(!text.contains(secret));
    }
}
#[test]
fn mixed_rest_websocket_and_amqp_registrations_are_reported_in_spawn_and_join_order() {
    let project = Project::new();
    project.install_local(&["http_server", "json", "websocket", "amqp10", "amqp091"]);
    project.write("src/Main.tnx", "import Api\nimport EventsEndpoint\nimport Consumer\nimport Legacy\nclass Main { fnc main() -> Int32 { return 0 } }");
    project.write("src/Api.tnx", "import tinox.core.http_server\n@ApplicationComponent @Path(\"/api\") class Api { @GET(\"/hello\") fn hello() -> String { return \"hi\" } }");
    project.write("src/EventsEndpoint.tnx", "import tinox.core.websocket\n@WebsocketEndpoint(\"/events\", 0) class EventsEndpoint { @OnMessage fn message(conn: Int64, text: String) -> Nothing {} }");
    project.write("src/Consumer.tnx", "import tinox.core.amqp10\n@Amqp10Consumer(\"localhost\", 1, \"USER_SECRET\", \"AMQP_SECRET\", \"/jobs\") class Consumer { @OnMessage fn consume(message: Amqp10Message) -> Nothing {} }");
    project.write("src/Legacy.tnx", "import tinox.core.amqp091\n@Amqp091Consumer(\"amqp://USER_SECRET:URL_SECRET@localhost\", 2, \"/\", \"USER_SECRET\", \"LEGACY_SECRET\", \"jobs\") class Legacy { @OnMessage fn consume(message: AmqpMessage091) -> Nothing {} }");
    let result = project.query();
    let plan = &result["plan"];
    let tasks = plan["background_tasks"].as_array().unwrap();
    assert_eq!(
        tasks.iter().map(|t| t["kind"].clone()).collect::<Vec<_>>(),
        vec![
            json!("rest"),
            json!("websocket"),
            json!("amqp10"),
            json!("amqp091")
        ]
    );
    assert_eq!(tasks[1]["endpoint"]["port"]["configured"], 0);
    assert_eq!(tasks[1]["endpoint"]["port"]["bound"], Value::Null);
    assert_eq!(tasks[1]["instance_lifetime"], "per_connection");
    assert_eq!(tasks[2]["endpoint"]["destination"], "/jobs");
    assert_eq!(tasks[3]["endpoint"]["host"], "[redacted]");
    assert_eq!(plan["routes"][0]["path"], "/api/hello");
    assert_eq!(plan["routes"][0]["handler"], "Api::hello");
    let steps = plan["steps"].as_array().unwrap();
    let spawns: Vec<_> = steps
        .iter()
        .filter(|s| s["action"] == "spawn")
        .map(|s| s["target"].clone())
        .collect();
    let joins: Vec<_> = steps
        .iter()
        .filter(|s| s["action"] == "join")
        .map(|s| s["target"].clone())
        .collect();
    assert_eq!(spawns, joins);
    assert_eq!(spawns.len(), 4);
    for secret in ["USER_SECRET", "AMQP_SECRET", "LEGACY_SECRET", "URL_SECRET"] {
        assert!(!result.to_string().contains(secret));
    }
    let env_output = project
        .command("explain")
        .args(["--bootstrap", "--json"])
        .env("TINOX_PORT", "8123")
        .output()
        .unwrap();
    assert!(env_output.status.success(), "{env_output:?}");
    assert_eq!(
        parse(&env_output)["plan"]["background_tasks"][0]["endpoint"]["port"]["source"],
        "compiler_environment:TINOX_PORT"
    );
}
#[test]
fn invalid_injection_graphs_fail_consistently_in_check_build_and_explain() {
    for (service, dependency, expected) in [
        ("import Dependency\n@Startup class Service { @Inject var dep: Dependency }", "class Dependency {}", "no component scope"),
        ("@Startup class Service { @Inject var dep: Service }", "class Dependency {}", "Cyclic @Inject"),
        ("import Dependency\n@ApplicationComponent class Service { @Inject var dep: Dependency }", "import Service\n@HttpRequestScoped class Dependency { @Inject var service: Service }", "Cyclic @Inject"),
    ] {
        let project = Project::new();
        project.write("src/Main.tnx", "import Service\nclass Main { fnc main() -> Int32 { return 0 } }");
        project.write("src/Service.tnx", service);
        project.write("src/Dependency.tnx", dependency);
        let mut messages = Vec::new();
        for action in ["check", "build", "explain"] {
            let mut command = project.command(action);
            command.arg("--json").env("PATH", "");
            if action == "explain" { command.arg("--bootstrap"); }
            let output = command.output().unwrap();
            assert!(!output.status.success(), "{output:?}");
            let result = parse(&output);
            let message = result["diagnostics"][0]["message"].as_str().unwrap();
            assert!(message.contains(expected), "{result}");
            messages.push(message.to_owned());
        }
        assert!(messages.windows(2).all(|w| w[0] == w[1]));
    }
}
#[test]
fn options_imports_and_entry_shape_fail_visibly() {
    let project = Project::new();
    for args in [
        vec!["--json"],
        vec!["--bootstrap", "--unknown", "--json"],
        vec!["--bootstrap", "missing.tnx", "--json"],
    ] {
        let output = project.command("explain").args(args).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(parse(&output)["success"], false);
    }
    project.write(
        "src/Main.tnx",
        "class Main { fn main() -> Int32 { return 0 } }",
    );
    let output = project
        .command("explain")
        .args(["--bootstrap", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(parse(&output)["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("fnc"));
    project.write(
        "src/Main.tnx",
        "import Missing\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    let output = project
        .command("explain")
        .args(["--bootstrap", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(parse(&output)["success"], false);
}

#[test]
fn combined_database_and_startup_program_passes_real_llvm_verification() {
    let project = Project::new();
    project.write(
        "src/Main.tnx",
        "import Service\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write("src/Service.tnx", "@Startup class Service {}");
    let manifest = fs::read_to_string(project.0.join("tinox.toml")).unwrap();
    project.write(
        "tinox.toml",
        &format!(
            "{manifest}\n[database]\ndriver=\"postgres\"\nurl=\"postgres://localhost:1/db\"\n"
        ),
    );
    let executable = project.0.join("verified");
    // Compile and link only. Running would try the unreachable database.
    let output = project
        .command("build")
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(executable.is_file());
}

#[test]
fn managed_lifecycle_plan_exposes_order_and_budgets_without_running_hooks() {
    let project = Project::new();
    project.write("src/Main.tnx", "import State\n@Service(1200, 2300) class Main { fnc main() -> Int32 { return 0 } }");
    project.write("src/State.tnx", "@Startup class State { @OnStart fn start() -> Nothing { println(\"HOOK_MUST_NOT_RUN\") } @OnStop fn stop() -> Nothing {} @Readiness fn ready() -> Bool { return true } }");
    let explanation = project.query();
    let lifecycle = &explanation["plan"]["lifecycle"];
    assert_eq!(lifecycle["enabled"], true);
    assert_eq!(lifecycle["startup_timeout_ms"], 1200);
    assert_eq!(lifecycle["shutdown_timeout_ms"], 2300);
    assert!(lifecycle["hooks"].as_array().unwrap().iter().any(|hook| hook["kind"] == "OnStart" && hook["class"] == "State"));
    assert_eq!(lifecycle["health_paths"], json!(["/health/live", "/health/ready"]));
    assert_eq!(explanation["plan"]["steps"][0]["action"], "managed_initializer");
}

#[test]
fn qualified_provider_inspection_agrees_with_declared_interface_and_scope() {
    let project = Project::new();
    project.write("src/Main.tnx", "import App\nclass Main { fnc main() -> Int32 { return 0 } }");
    project.write("src/Store.tnx", "interface Store { fn value() -> Int64 }");
    project.write("src/Fast.tnx", "import Store\n@ApplicationComponent @Provider(\"fast\") class Fast implements Store { fn value() -> Int64 { return 1 } }");
    project.write("src/Safe.tnx", "import Store\n@HttpRequestScoped @Provider(\"safe\") class Safe implements Store { fn value() -> Int64 { return 2 } }");
    project.write("src/App.tnx", "import Fast\nimport Safe\nimport Store\n@Startup class App { @Inject @Qualifier(\"fast\") var store: Store }");
    let result = project.query();
    let app = result["plan"]["components"].as_array().unwrap().iter().find(|c| c["class"] == "App").unwrap();
    let selected = &app["injections"][0];
    assert_eq!(selected["target"], "Store");
    assert_eq!(selected["provider_class"], "Fast");
    assert_eq!(selected["qualifier"], "fast");
    assert_eq!(selected["provider"], "Fast_di_get");
    assert_eq!(selected["target_scope"], "application_singleton");
    let text = project.command("explain").arg("--bootstrap").output().unwrap();
    assert!(text.status.success(), "{text:?}");
    assert!(String::from_utf8_lossy(&text.stdout).contains("Inject: store -> Store via Fast_di_get"));
}

#[test]
fn qualified_graph_failures_never_require_a_backend() {
    for (fast, safe, injection, expected) in [
        ("@ApplicationComponent @Provider(\"fast\")", "@ApplicationComponent @Provider(\"safe\")", "@Inject var store: Store", "Ambiguous providers"),
        ("@ApplicationComponent @Provider(\"fast\")", "@ApplicationComponent @Provider(\"safe\")", "@Inject @Qualifier(\"missing\") var store: Store", "Missing provider"),
        ("@ApplicationComponent @Provider(\"same\")", "@ApplicationComponent @Provider(\"same\")", "@Inject @Qualifier(\"same\") var store: Store", "Duplicate provider name"),
    ] {
        let project = Project::new();
        project.write("src/Main.tnx", "import Service\nclass Main { fnc main() -> Int32 { return 0 } }");
        project.write("src/Store.tnx", "interface Store { fn value() -> Int64 }");
        project.write("src/Fast.tnx", &format!("import Store\n{fast} class Fast implements Store {{ fn value() -> Int64 {{ return 1 }} }}"));
        project.write("src/Safe.tnx", &format!("import Store\n{safe} class Safe implements Store {{ fn value() -> Int64 {{ return 2 }} }}"));
        project.write("src/Service.tnx", &format!("import Store\nimport Fast\nimport Safe\n@Startup class Service {{ {injection} }}"));
        for action in ["check", "build", "explain"] {
            let mut command = project.command(action);
            command.arg("--json").env("PATH", "");
            if action == "explain" { command.arg("--bootstrap"); }
            let output = command.output().unwrap();
            assert!(!output.status.success(), "{output:?}");
            let result = parse(&output);
            assert!(result["diagnostics"].as_array().unwrap().iter().any(|diagnostic|
                diagnostic["message"].as_str().unwrap().contains(expected)), "{result}");
        }
    }
}

//! Inspection records are built from the same registrations and startup plan
//! that emit generated code. Credentials and configuration values stay private.
use super::{CodeGen, DiScope};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Port {
    pub configured: i64,
    pub bound: Option<i64>,
    pub source: String,
    pub phase: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Endpoint {
    pub protocol: String,
    pub direction: String,
    pub port: Port,
    pub host: Option<String>,
    pub path: Option<String>,
    pub destination: Option<String>,
    pub credentials: String,
}
impl Endpoint {
    pub fn listen(protocol: &str, port: i64, source: &str, path: Option<&str>) -> Self {
        Self {
            protocol: protocol.into(),
            direction: "listen".into(),
            port: Port {
                configured: port,
                bound: None,
                source: source.into(),
                phase: "compile_time".into(),
            },
            host: None,
            path: path.map(str::to_owned),
            destination: None,
            credentials: "not_reported".into(),
        }
    }
    pub fn connect(protocol: &str, port: i64, host: &str, destination: &str) -> Self {
        let mut endpoint = Self::listen(protocol, port, "annotation", None);
        endpoint.direction = "connect".into();
        endpoint.host = Some(public_address(host));
        endpoint.destination = Some(public_address(destination));
        endpoint.credentials = "redacted".into();
        endpoint
    }
    pub fn with_host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }
}
fn public_address(value: &str) -> String {
    // Never echo URI userinfo, query tokens or fragments as an endpoint label.
    if value.contains('@') || value.contains('?') || value.contains('#') || value.contains("://") {
        "[redacted]".into()
    } else {
        value.into()
    }
}
pub(super) fn environment_port_source(port: i64) -> &'static str {
    if std::env::var("TINOX_PORT")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        == Some(port)
    {
        "compiler_environment:TINOX_PORT"
    } else {
        "default"
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundTask {
    pub kind: String,
    pub owner: Option<String>,
    pub function: String,
    pub instance_lifetime: String,
    pub concurrency: String,
    pub endpoint: Endpoint,
    pub callbacks: Vec<String>,
}
impl BackgroundTask {
    pub fn new(
        kind: &str,
        owner: Option<&str>,
        function: &str,
        lifetime: &str,
        concurrency: &str,
        endpoint: Endpoint,
    ) -> Self {
        Self {
            kind: kind.into(),
            owner: owner.map(str::to_owned),
            function: function.into(),
            instance_lifetime: lifetime.into(),
            concurrency: concurrency.into(),
            endpoint,
            callbacks: vec![],
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Injection {
    pub field: String,
    pub target: String,
    pub provider_class: String,
    pub qualifier: Option<String>,
    pub provider: String,
    pub target_scope: String,
    pub resolution: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub class: String,
    pub scope: String,
    pub initialization: String,
    pub provider: String,
    pub injections: Vec<Injection>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub handler: String,
    pub controller_allocation: String,
    pub transport: String,
    pub auth_required: bool,
    pub parameters: Vec<RouteParameter>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RouteParameter {
    pub name: String,
    pub type_name: String,
    pub binding: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Initializer {
    pub name: String,
    pub priority: u32,
    pub function: String,
    pub components: Vec<String>,
    pub configuration: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct ConfigurationBinding {
    pub class: String,
    pub field: String,
    pub key: String,
    pub resolution: String,
    pub value: Option<String>,
    pub application: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub action: String,
    pub target: String,
    pub ordering: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapPlan {
    pub lifecycle: super::lifecycle::LifecyclePlan,
    pub mode: String,
    pub entry_point: Option<String>,
    pub banner: bool,
    pub initializers: Vec<Initializer>,
    pub background_tasks: Vec<BackgroundTask>,
    pub components: Vec<Component>,
    pub routes: Vec<Route>,
    pub configuration: Vec<ConfigurationBinding>,
    pub steps: Vec<Step>,
    pub notes: Vec<String>,
}
fn scope(scope: &DiScope) -> &'static str {
    match scope {
        DiScope::Application => "application_singleton",
        DiScope::Startup => "startup_singleton",
        DiScope::HttpRequest => "request_context",
    }
}
impl CodeGen {
    pub(super) fn register_background_task(&mut self, mut task: BackgroundTask) {
        match task.kind.as_str() {
            "websocket" => {
                if let Some(endpoint) = self
                    .ws_endpoints
                    .iter()
                    .find(|e| Some(&e.class_name) == task.owner.as_ref())
                {
                    task.callbacks = endpoint
                        .on_open
                        .iter()
                        .chain(endpoint.on_message.iter())
                        .chain(endpoint.on_close.iter())
                        .cloned()
                        .collect();
                }
            }
            "amqp10" => {
                if let Some(consumer) = self
                    .amqp10_consumers
                    .iter()
                    .find(|c| Some(&c.class_name) == task.owner.as_ref())
                {
                    task.callbacks = consumer.on_message.iter().cloned().collect();
                }
            }
            "amqp091" => {
                if let Some(consumer) = self
                    .amqp091_consumers
                    .iter()
                    .find(|c| Some(&c.class_name) == task.owner.as_ref())
                {
                    task.callbacks = consumer.on_message.iter().cloned().collect();
                }
            }
            "ui_websocket" => {
                if let Some(app) = self
                    .tinoxui_apps
                    .iter()
                    .find(|a| Some(&a.class_name) == task.owner.as_ref())
                {
                    task.callbacks.push(app.view_method.clone());
                }
            }
            _ => {}
        }
        self.background_tasks.push(task);
    }
    pub(super) fn emit_global_initializers(&mut self) {
        use std::fmt::Write;
        if self.service_enabled() {
            return;
        }
        let initializers = self.bootstrap_plan().initializers;
        if initializers.is_empty() {
            return;
        }
        let entries = initializers
            .iter()
            .map(|init| {
                format!(
                    "{{ i32, void ()*, i8* }} {{ i32 {}, void ()* @{}, i8* null }}",
                    init.priority, init.function
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            &mut self.ir,
            "@llvm.global_ctors = appending global [{} x {{ i32, void ()*, i8* }}] [{}]",
            initializers.len(),
            entries
        )
        .unwrap();
    }
    pub(super) fn startup_component_names(&self) -> Vec<String> {
        self.di_components
            .iter()
            .filter(|c| c.scope == DiScope::Startup)
            .map(|c| c.class_name.clone())
            .collect()
    }
    /// Public facts only. The model never serializes arbitrary annotation
    /// arguments, DB URLs, passwords, usernames or resolved configuration data.
    pub fn bootstrap_plan(&self) -> BootstrapPlan {
        let mode = if !self.test_entries.is_empty() {
            "test"
        } else if self.bootstrap_emitted {
            "bootstrap"
        } else if self.has_main && !self.cli_commands.is_empty() {
            "command"
        } else if self.has_main {
            "direct"
        } else {
            "none"
        };
        let entry_point = match mode {
            "bootstrap" if self.user_main_class => Some("Main::main".into()),
            "bootstrap" => Some("first background task result".into()),
            "command" => self
                .cli_commands
                .first()
                .map(|c| format!("{}::run", c.class_name)),
            "test" => Some("generated test runner".into()),
            "direct" => Some("main".into()),
            _ => None,
        };
        let background_tasks = if mode == "bootstrap" {
            self.background_tasks.clone()
        } else {
            vec![]
        };
        let components = self.di_components.iter().map(|component| Component {
            class: component.class_name.clone(), scope: scope(&component.scope).into(),
            initialization: match component.scope { DiScope::Startup if self.service_enabled() => "eager managed initialization before hooks and listeners", DiScope::Startup => "eager global constructor before main", DiScope::Application => "lazy on first provider call", DiScope::HttpRequest => "one instance per annotated REST request context" }.into(),
            provider: format!("{}_di_{}", component.class_name, if component.scope == DiScope::HttpRequest { "create" } else { "get" }),
            injections: component.inject_fields.iter().map(|field| {
                let target = self.di_components.iter().find(|c| c.class_name == field.provider_class);
                let singleton = target.is_some_and(|c| c.scope != DiScope::HttpRequest);
                Injection { field: field.field_name.clone(), target: field.field_type.clone(), provider_class: field.provider_class.clone(), qualifier: field.qualifier.clone(), provider: format!("{}_di_{}", field.provider_class, if singleton { "get" } else { "create" }),
                    target_scope: target.map(|c| scope(&c.scope)).unwrap_or("unregistered").into(),
                    resolution: if target.is_some_and(|c| c.scope == DiScope::HttpRequest) { "resolved through the active request context; captive singleton lifetimes are rejected" } else { "resolved recursively during owner construction" }.into() }
            }).collect(),
        }).collect();
        let routes = self
            .route_entries
            .iter()
            .map(|route| {
                let allocation = if route.is_static {
                    "static handler; no controller instance"
                } else {
                    match self
                        .di_components
                        .iter()
                        .find(|c| c.class_name == route.class_name)
                        .map(|c| &c.scope)
                    {
                        Some(DiScope::Application | DiScope::Startup) => {
                            "singleton provider; captive request state is rejected"
                        }
                        Some(DiScope::HttpRequest) => "request-context provider; identity shared across injection graph",
                        None => "fresh controller per request",
                    }
                };
                Route {
                    method: route.http_method.clone(),
                    path: route.path.clone(),
                    handler: format!("{}::{}", route.class_name, route.method_name),
                    controller_allocation: allocation.into(),
                    transport: if self.http3_rest_controller.is_some() {
                        "HTTP/3"
                    } else {
                        "HTTP"
                    }
                    .into(),
                    auth_required: route.auth_type.is_some(),
                    parameters: route
                        .params
                        .iter()
                        .map(|p| RouteParameter {
                            name: p.name.clone(),
                            type_name: tinox_parser::api::type_name(&p.ty),
                            binding: match p.kind {
                                super::RouteParamKind::PathParam => "path",
                                super::RouteParamKind::QueryParam => "query",
                                super::RouteParamKind::PostParam => "body",
                                super::RouteParamKind::MultipartParam => "multipart",
                                super::RouteParamKind::HttpContext => "context",
                            }
                            .into(),
                        })
                        .collect(),
                }
            })
            .collect();
        let mut initializers = vec![];
        if self.db_url.is_some() {
            initializers.push(Initializer {
                name: "database_pool".into(),
                function: "__tinox_db_init".into(),
                priority: 10,
                components: vec![],
                configuration: "connection URL redacted; connect at process startup".into(),
            });
        }
        let startup = self.startup_component_names();
        if !startup.is_empty() {
            initializers.push(Initializer {
                name: "di_startup".into(),
                function: "tinox_di_startup".into(),
                priority: 65535,
                components: startup,
                configuration: "declaration order; dependencies initialized recursively first"
                    .into(),
            });
        }
        let mut steps = initializers
            .iter()
            .map(|init| Step {
                action: if self.service_enabled() {
                    "managed_initializer"
                } else { "global_constructor"
                }.into(),
                target: init.name.clone(),
                ordering: format!("priority {}, before hooks and entry point", init.priority),
            })
            .collect::<Vec<_>>();
        for task in &background_tasks {
            steps.push(Step {
                action: "spawn".into(),
                target: task.function.clone(),
                ordering: "registration order; execution and readiness are concurrent".into(),
            });
        }
        if let Some(entry) = &entry_point {
            if self.user_main_class || mode != "bootstrap" {
                steps.push(Step {
                    action: "call".into(),
                    target: entry.clone(),
                    ordering: "after issuing all background spawns".into(),
                });
            }
        }
        for task in &background_tasks {
            steps.push(Step {
                action: "join".into(),
                target: task.function.clone(),
                ordering: if self.service_enabled() { "registration order, after Main returns; bounded by the common shutdown deadline after a stop request" } else { "registration order, after Main returns; may wait indefinitely" }.into(),
            });
        }
        let mut notes = vec![
            "Listener binding, network connection success and readiness are runtime-dependent; inspection starts nothing.".into(),
            "Spawn order does not establish execution order between threads or readiness before Main.".into(),
            "Application and startup singleton getters serialize initialization/publication; shared mutable state needs @Synchronized or explicit coordination. Captive request-scoped dependencies in singletons are rejected before backend emission.".into(),
            "Application/startup-scoped WebSocket endpoints use their DI provider; unscoped WebSocket/UI instances are allocated per connection and AMQP instances per consumer connection. HTTP request scope is not supported for WebSocket endpoints.".into(),
            if self.service_enabled() {
            "Managed lifecycle: signal coordinator, admission control, joined workers, reverse OnStop hooks and database disposal share one shutdown deadline." } else { "Coordinated lifecycle is opt-in: annotate class Main with @Service." }.into(),
        ];
        if mode != "command" && !self.cli_commands.is_empty() {
            notes.push("Command dispatch is suppressed because an earlier generated entry point already owns main.".into());
        }
        BootstrapPlan { lifecycle: self.lifecycle_plan(), mode: mode.into(), entry_point, banner: self.banner_enabled && !background_tasks.is_empty(), initializers, background_tasks, components, routes,
            configuration: self.config_fields.iter().map(|field| ConfigurationBinding { class: field.class_name.clone(), field: field.field_name.clone(), key: field.config_key.clone(), resolution: "runtime; unresolved".into(), value: None, application: "generated struct-literal construction path; not automatically read by DI/endpoint allocation".into() }).collect(),
            steps, notes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{
        Amqp091ConsumerEntry, Amqp10ConsumerEntry, AnnotationInfo, DiComponentInfo, DiInjectField,
        RouteEntry, WsEndpointEntry,
    };
    use tinox_parser::{Parser, Type};

    #[test]
    fn explanation_matches_real_generated_spawns_joins_routes_and_injection_calls() {
        let source = "class Main { fnc main() -> Int32 { return 0 } } class Api { fn hello() -> String { return \"hello\" } } class Events { fn message(conn: Int64, text: String) -> Nothing {} } class WsFrame { var opcode: Int64 } class Amqp10Message {} class AmqpMessage091 {} class Consumer {} class Legacy {} class Request {} class Service { var request: Request }";
        let tokens = tinox_lexer::Lexer::new(source).tokenize().unwrap();
        let ast = Parser::new(tokens).parse().unwrap();
        let mut codegen = CodeGen::new();
        codegen.set_annotation_info(AnnotationInfo {
            routes: vec![RouteEntry {
                http_method: "GET".into(),
                path: "/hello".into(),
                class_name: "Api".into(),
                method_name: "hello".into(),
                status_code: None,
                produces: None,
                consumes: None,
                auth_type: None,
                oidc_roles: vec![],
                is_static: false,
                params: vec![],
                return_type: Type::String,
            }],
            di_components: vec![
                DiComponentInfo {
                    class_name: "Service".into(),
                    scope: DiScope::Startup,
                    inject_fields: vec![DiInjectField {
                        field_name: "request".into(),
                        field_type: "Request".into(), provider_class: "Request".into(), qualifier: None,
                    }],
                },
                DiComponentInfo {
                    class_name: "Request".into(),
                    scope: DiScope::HttpRequest,
                    inject_fields: vec![],
                },
            ],
            ..Default::default()
        });
        codegen.set_ws_endpoints(vec![WsEndpointEntry {
            class_name: "Events".into(),
            path: "/events".into(),
            port: Some(0),
            on_open: None,
            on_message: Some("message".into()),
            on_close: None,
        }]);
        codegen.set_amqp10_consumers(vec![Amqp10ConsumerEntry {
            class_name: "Consumer".into(),
            host: "localhost".into(),
            port: 5672,
            user: "secret_user".into(),
            pass: "secret_pass".into(),
            address: "/jobs".into(),
            on_message: None,
            failure: None,
        }]);
        codegen.set_amqp091_consumers(vec![Amqp091ConsumerEntry {
            class_name: "Legacy".into(),
            host: "localhost".into(),
            port: 5673,
            vhost: "/".into(),
            user: "secret_user".into(),
            pass: "secret_pass".into(),
            queue: "jobs".into(),
            on_message: None,
            failure: None,
        }]);
        codegen.gen(&ast).unwrap();
        let plan = codegen.bootstrap_plan();
        let ir = codegen.into_ir();
        assert_eq!(
            plan.background_tasks
                .iter()
                .map(|task| task.kind.as_str())
                .collect::<Vec<_>>(),
            ["rest", "websocket", "amqp10", "amqp091"]
        );
        let main = ir
            .split("define i32 @tinox_main()")
            .nth(1)
            .unwrap()
            .split("\n}")
            .next()
            .unwrap();
        assert_eq!(
            main.matches("call i8* @tinox_task_spawn(").count(),
            plan.background_tasks.len()
        );
        assert_eq!(
            main.matches("call i64 @tinox_task_await(").count(),
            plan.background_tasks.len()
        );
        for (index, task) in plan.background_tasks.iter().enumerate() {
            assert!(
                ir.contains(&format!("define i64 @{}()", task.function)),
                "{}",
                task.function
            );
            assert!(ir.contains(&format!("call i64 @{}()", task.function)));
            assert!(main.contains(&format!("%h_{index}")));
        }
        let spawn = main.rfind("@tinox_task_spawn").unwrap();
        let call = main.find("@Main_main").unwrap();
        let join = main.find("@tinox_task_await").unwrap();
        assert!(spawn < call && call < join);
        assert_eq!(plan.routes[0].handler, "Api::hello");
        assert!(ir.contains("@__route_Api_hello"));
        assert!(ir.contains("call i64* @Request_di_create()"));
        assert_eq!(
            plan.components[0].injections[0].provider,
            "Request_di_create"
        );
        assert!(ir.contains("call i64* @Service_di_get()"));
        assert_eq!(plan.initializers[0].components, ["Service"]);
        assert_eq!(plan.background_tasks[1].callbacks, ["message"]);
    }
    #[test]
    fn test_dispatch_does_not_run_imported_main_or_background_routes() {
        let source = "class Main { fnc main() -> Int32 { return 42 } } class Api { fn hello() -> String { return \"hello\" } } class Suite { fn first() -> Bool { return true } fn second() -> Bool { return false } }";
        let tokens = tinox_lexer::Lexer::new(source).tokenize().unwrap();
        let ast = Parser::new(tokens).parse().unwrap();
        let mut codegen = CodeGen::new();
        codegen.set_annotation_info(AnnotationInfo {
            routes: vec![RouteEntry {
                http_method: "GET".into(), path: "/hello".into(),
                class_name: "Api".into(), method_name: "hello".into(),
                status_code: None, produces: None, consumes: None, auth_type: None,
                oidc_roles: vec![], is_static: false, params: vec![], return_type: Type::String,
            }],
            ..Default::default()
        });
        codegen.set_test_entries(
            ["first", "second"]
                .map(|m| crate::codegen::TestEntry { class: "Suite".into(), method: m.into(), args: vec![], before: vec![], after: vec![] })
                .to_vec(),
        );
        codegen.gen(&ast).unwrap();
        assert_eq!(codegen.bootstrap_plan().mode, "test");
        let ir = codegen.into_ir();
        let main = ir.split("define i64 @tinox_main()").nth(1).unwrap().split("\n}").next().unwrap();
        assert!(main.contains("@Suite_first"));
        assert!(main.contains("@Suite_second"));
        assert!(!main.contains("@Main_main"));
        assert!(!main.contains("@tinox_task_spawn"));
    }

    #[test]
    fn database_and_startup_share_one_llvm_constructor_table_with_preserved_priorities() {
        let tokens = tinox_lexer::Lexer::new(
            "class Main { fnc main() -> Int32 { return 0 } } class Service {}",
        )
        .tokenize()
        .unwrap();
        let ast = Parser::new(tokens).parse().unwrap();
        let mut codegen = CodeGen::new();
        codegen.set_db_url(Some("postgres://localhost:1/db".into()));
        codegen.set_annotation_info(AnnotationInfo {
            di_components: vec![DiComponentInfo {
                class_name: "Service".into(),
                scope: DiScope::Startup,
                inject_fields: vec![],
            }],
            ..Default::default()
        });
        codegen.gen(&ast).unwrap();
        let plan = codegen.bootstrap_plan();
        let ir = codegen.into_ir();
        assert_eq!(ir.matches("@llvm.global_ctors =").count(), 1);
        assert_eq!(
            plan.initializers
                .iter()
                .map(|init| init.priority)
                .collect::<Vec<_>>(),
            [10, 65535]
        );
        assert!(ir.contains("[2 x { i32, void ()*, i8* }]"));
        for init in plan.initializers {
            assert!(ir.contains(&format!(
                "i32 {}, void ()* @{}",
                init.priority, init.function
            )));
        }
    }
}

use std::collections::{HashMap, HashSet};
use tinox_common::{Error, Span};
use tinox_parser::{Annotation, AnnotationArg, Class, DeclKind, FieldDef, Function, Literal, Method, Namespace, Type};

fn media_type_arg_to_mime(arg: &AnnotationArg) -> Option<String> {
    match arg {
        AnnotationArg::EnumValue(type_name, variant) if type_name == "MediaType" => {
            match variant.as_str() {
                "APPLICATION_JSON" => Some("application/json".to_string()),
                "PLAIN_TEXT"       => Some("text/plain".to_string()),
                _ => None,
            }
        }
        // String literal form: @Produces("application/json")
        AnnotationArg::Literal(Literal::String(s)) => Some(s.clone()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnnotationTarget {
    Function,
    Method,
    Class,
    Field,
    Interface,
    Enum,
    Trait,
    Namespace,
    Param,
}

#[derive(Debug, Clone)]
pub struct AnnotationInfo {
    pub name: String,
    pub valid_targets: Vec<AnnotationTarget>,
    pub min_args: usize,
    pub max_args: usize,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ProcessedAnnotation {
    pub name: String,
    pub args: Vec<tinox_parser::Literal>,
    pub span: Span,
}

/// Which of the four REST parameter-binding annotations a handler
/// parameter carries -- see CLAUDE.md's REST parameter binding section.
/// Every parameter of an `@GET`/`@POST`/etc. handler carries exactly one
/// (validated in `extract_route_from_method`); there is no unannotated/
/// implicit shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteParamKind {
    PathParam,
    QueryParam,
    PostParam,
    HttpContext,
}

#[derive(Debug, Clone)]
pub struct RouteParamBinding {
    pub kind: RouteParamKind,
    /// The Tinox parameter's own name -- also the `:name` path segment /
    /// query string key it binds to for Path/QueryParam (no separate key
    /// argument on the annotation).
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub method: String,
    pub path: String,
    pub class_name: String,
    pub method_name: String,
    pub status_code: Option<i64>,
    pub produces: Option<String>,
    pub consumes: Option<String>,
    pub auth_type: Option<String>,
    /// Roles from @OIDCRolesAllowed(["role1", "role2"]) -- request must
    /// carry a verified OIDC access token with at least one of these
    /// realm roles. Empty = no OIDC role check on this route.
    pub oidc_roles: Vec<String>,
    pub is_static: bool,
    /// Per-parameter bindings, in declared order -- drives the shim's
    /// call-argument construction (emit_route_shim_body, codegen.rs).
    pub params: Vec<RouteParamBinding>,
    /// `HttpContext` = manual-response mode (handler builds `ctx.response`
    /// itself); anything else = auto-serialize mode (the shim serializes
    /// the returned value as the JSON response body).
    pub return_type: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiScope {
    Application,
    Startup,
    HttpRequest,
}

#[derive(Debug, Clone)]
pub struct DiInjectField {
    pub field_name: String,
    pub field_type: String,
}

#[derive(Debug, Clone)]
pub struct DiComponentInfo {
    pub class_name: String,
    pub scope: DiScope,
    pub inject_fields: Vec<DiInjectField>,
}

#[derive(Debug, Clone)]
pub struct ConfigFieldInfo {
    pub class_name: String,
    pub field_name: String,
    pub config_key: String,
    /// LLVM type of the field: "i8*" for String, "i64" for Int*, "i1" for Bool
    pub field_llvm_type: String,
}

#[derive(Debug, Clone)]
pub struct LogMaskFieldInfo {
    pub class_name: String,
    pub field_name: String,
}

#[derive(Debug, Clone)]
pub struct TestInfo {
    pub class_name: String,
    pub method_name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct EntityFieldInfo {
    pub field_name: String,
    pub column_name: String,
    pub is_id: bool,
    pub is_generated: bool,
    pub not_null: bool,
    pub field_llvm_type: String,
}

#[derive(Debug, Clone)]
pub struct EntityInfo {
    pub class_name: String,
    pub table_name: String,
    pub fields: Vec<EntityFieldInfo>,
}

#[derive(Debug, Clone)]
pub struct CliOptionInfo {
    pub field_name: String,
    /// All flag names, e.g. `["--name", "-n"]`.
    pub names: Vec<String>,
    pub description: String,
    pub required: bool,
    /// "String" | "Bool" | "Int"
    pub field_type: String,
}

#[derive(Debug, Clone)]
pub struct CliArgumentInfo {
    pub field_name: String,
    pub index: usize,
    pub description: String,
    pub required: bool,
    pub field_type: String,
}

#[derive(Debug, Clone)]
pub struct CliCommandInfo {
    pub class_name: String,
    pub cmd_name: String,
    pub description: String,
    pub version: Option<String>,
    pub options: Vec<CliOptionInfo>,
    pub arguments: Vec<CliArgumentInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricKind {
    Timed,
    Counted,
    Gauge,
}

#[derive(Debug, Clone)]
pub struct WsEndpointInfo {
    pub class_name: String,
    pub path: String,
    pub port: Option<i64>,
    pub on_open: Option<String>,
    pub on_message: Option<String>,
    pub on_close: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Amqp10ConsumerInfo {
    pub class_name: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub pass: String,
    pub address: String,
    pub on_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Amqp091ConsumerInfo {
    pub class_name: String,
    pub host: String,
    pub port: i64,
    pub vhost: String,
    pub user: String,
    pub pass: String,
    pub queue: String,
    pub on_message: Option<String>,
}

/// @Http3RestController(port, certPath, keyPath) on a class -- routes
/// every @GET/@POST/@PUT/@PATCH/@DELETE route in the program (the same
/// program-wide route_entries the TCP auto-server uses) through
/// tinox.core.http3_server.Http3Server on this port/cert/key instead of
/// the GC-crash-prone tinox_HttpServer_listen (issue #140). At most one
/// per program (checked in main.rs, same as WsEndpointInfo/
/// Amqp10ConsumerInfo/Amqp091ConsumerInfo).
#[derive(Debug, Clone)]
pub struct Http3RestControllerInfo {
    pub class_name: String,
    pub port: i64,
    pub cert_path: String,
    pub key_path: String,
}

/// @TinoxUIApp(httpPort, wsPort) on a class (issue #215, Phase 4) --
/// annotation sugar over the hand-wired @WebsocketEndpoint + HttpServer
/// shell-serving boilerplate every Tinox-UI app (tinox_ui_hello,
/// tinox_ui_signup) previously had to write by hand: the compiler
/// generates an HTTP server on httpPort serving the shell page ("/") and
/// client JS ("/ui.js"), plus a WebSocket accept loop on wsPort driving
/// the class's own @View method -- full-resend rendering (build the tree,
/// send it, rebuild + resend after every event), matching Phase 1's
/// TinoxUIRuntime.buildHandlers/sendInit/sendUpdate shape exactly.
/// Diff-based rendering (Phase 3) stays a manual, lower-level opt-in for
/// apps that want it -- not generated by this annotation, since it needs
/// app-owned persistent id-counter state this sugar has nowhere to put
/// (see TinoxUIRuntime.tnx's own doc comment on why diffing can't just be
/// the default here). `view_methods` collects every method carrying
/// @View so the caller (main.rs) can validate "exactly one", the same
/// place Http3RestControllerInfo's "at most one class" cardinality is
/// enforced today.
#[derive(Debug, Clone)]
pub struct TinoxUIAppInfo {
    pub class_name: String,
    pub http_port: i64,
    pub ws_port: i64,
    pub view_methods: Vec<String>,
    /// @Route(path)-annotated methods on this class, in declaration order
    /// (first-match-wins at dispatch time) -- (path pattern, method name).
    /// Empty when the app doesn't use @Route at all (the common case,
    /// unchanged from pre-@Route behavior: `view_methods[0]` alone builds
    /// every render). See annotations.rs's "Route" registry entry for the
    /// pattern syntax.
    pub route_entries: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct MetricInfo {
    pub kind: MetricKind,
    /// Custom label from the annotation argument, e.g. @Timed("my_label").
    /// Falls back to "<ClassName>_<methodName>" or "<funcName>" when omitted.
    pub metric_name: String,
    /// Set for methods; empty for top-level functions.
    pub class_name: String,
    /// The Tinox function / method name.
    pub fn_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct AnnotationProcessingResult {
    pub route_entries: Vec<RouteInfo>,
    pub inline_functions: HashSet<String>,
    pub inline_methods: HashSet<(String, String)>,
    /// (class, method) pairs that should run inside a DB transaction --
    /// either the method itself carries @Transactional, or its class does
    /// (class-level applies to every method, same precedence pattern as
    /// @Auth; a method-level annotation on top of a class-level one is
    /// simply redundant, not an override, since there's no argument to
    /// override — unlike @Auth's "bearer"/"basic" choice).
    pub transactional_methods: HashSet<(String, String)>,
    pub deprecated_warnings: Vec<String>,
    pub custom_annotation_names: Vec<String>,
    pub di_components: Vec<DiComponentInfo>,
    pub log_classes: HashSet<String>,
    pub config_fields: Vec<ConfigFieldInfo>,
    pub sensitive_fields: Vec<LogMaskFieldInfo>,
    pub masked_fields: Vec<LogMaskFieldInfo>,
    pub do_not_serialize_fields: Vec<LogMaskFieldInfo>,
    pub json_serializable_classes: Vec<String>,
    pub cli_commands: Vec<CliCommandInfo>,
    pub test_entries: Vec<TestInfo>,
    pub metric_entries: Vec<MetricInfo>,
    pub entity_entries: Vec<EntityInfo>,
    pub ws_endpoints: Vec<WsEndpointInfo>,
    pub amqp10_consumers: Vec<Amqp10ConsumerInfo>,
    pub amqp091_consumers: Vec<Amqp091ConsumerInfo>,
    pub http3_rest_controllers: Vec<Http3RestControllerInfo>,
    pub tinoxui_apps: Vec<TinoxUIAppInfo>,
}

pub struct AnnotationProcessor {
    registry: HashMap<String, AnnotationInfo>,
}

impl AnnotationProcessor {
    pub fn new() -> Self {
        let mut registry: HashMap<String, AnnotationInfo> = HashMap::new();

        // HTTP method annotations
        registry.insert(
            "GET".to_string(),
            AnnotationInfo {
                name: "GET".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Marks a method as a GET endpoint. Optional path arg, or use @Path.".to_string(),
            },
        );
        registry.insert(
            "POST".to_string(),
            AnnotationInfo {
                name: "POST".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Marks a method as a POST endpoint. Optional path arg, or use @Path.".to_string(),
            },
        );
        registry.insert(
            "PUT".to_string(),
            AnnotationInfo {
                name: "PUT".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Marks a method as a PUT endpoint. Optional path arg, or use @Path.".to_string(),
            },
        );
        registry.insert(
            "PATCH".to_string(),
            AnnotationInfo {
                name: "PATCH".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Marks a method as a PATCH endpoint. Optional path arg, or use @Path.".to_string(),
            },
        );
        registry.insert(
            "DELETE".to_string(),
            AnnotationInfo {
                name: "DELETE".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Marks a method as a DELETE endpoint. Optional path arg, or use @Path.".to_string(),
            },
        );

        // REST framework annotations
        registry.insert(
            "Path".to_string(),
            AnnotationInfo {
                name: "Path".to_string(),
                valid_targets: vec![AnnotationTarget::Class, AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "Sets the URL path prefix for a controller or route".to_string(),
            },
        );
        registry.insert(
            "Produces".to_string(),
            AnnotationInfo {
                name: "Produces".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "Specifies the response content type".to_string(),
            },
        );
        registry.insert(
            "Consumes".to_string(),
            AnnotationInfo {
                name: "Consumes".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "Specifies the accepted request content type".to_string(),
            },
        );
        registry.insert(
            "StatusCode".to_string(),
            AnnotationInfo {
                name: "StatusCode".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "Sets the default HTTP status code for the response".to_string(),
            },
        );
        registry.insert(
            "Auth".to_string(),
            AnnotationInfo {
                name: "Auth".to_string(),
                valid_targets: vec![AnnotationTarget::Method, AnnotationTarget::Class],
                min_args: 1,
                max_args: 1,
                description: "Requires authentication (\"bearer\" or \"basic\")".to_string(),
            },
        );
        registry.insert(
            "OIDCRolesAllowed".to_string(),
            AnnotationInfo {
                name: "OIDCRolesAllowed".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "Requires a verified OIDC access token (RS256/JWKS, IdP config via OIDC_ISSUER/OIDC_JWKS_URI/OIDC_AUDIENCE env vars) carrying at least one of the listed realm roles".to_string(),
            },
        );
        registry.insert(
            "Transactional".to_string(),
            AnnotationInfo {
                name: "Transactional".to_string(),
                valid_targets: vec![AnnotationTarget::Method, AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Wraps the method (or every method of the class) in a database transaction: BEGIN before, COMMIT on normal return, ROLLBACK on any thrown exception. Postgres only in v1 (issue #191) -- a hard compile error on any other [database] driver".to_string(),
            },
        );
        // REST parameter binding annotations -- exactly one required on
        // every parameter of an @GET/@POST/etc. handler (see
        // extract_route_from_method's validation and CLAUDE.md's REST
        // parameter binding section; no backward-compat "bare ctx param"
        // shape -- every handler parameter must be annotated).
        registry.insert(
            "PathParam".to_string(),
            AnnotationInfo {
                name: "PathParam".to_string(),
                valid_targets: vec![AnnotationTarget::Param],
                min_args: 0,
                max_args: 0,
                description: "Binds a `:name` path segment (same name as the parameter) to this parameter — String/Int64/Int32/Bool/Float64/Float32".to_string(),
            },
        );
        registry.insert(
            "QueryParam".to_string(),
            AnnotationInfo {
                name: "QueryParam".to_string(),
                valid_targets: vec![AnnotationTarget::Param],
                min_args: 0,
                max_args: 0,
                description: "Binds a query string key (same name as the parameter) to this parameter — String/Int64/Int32/Bool/Float64/Float32".to_string(),
            },
        );
        registry.insert(
            "PostParam".to_string(),
            AnnotationInfo {
                name: "PostParam".to_string(),
                valid_targets: vec![AnnotationTarget::Param],
                min_args: 0,
                max_args: 0,
                description: "Binds the deserialized JSON request body to this parameter — type must be @JsonSerializable".to_string(),
            },
        );
        registry.insert(
            "HttpContext".to_string(),
            AnnotationInfo {
                name: "HttpContext".to_string(),
                valid_targets: vec![AnnotationTarget::Param],
                min_args: 0,
                max_args: 0,
                description: "Binds the request/response handle to this parameter — type must be HttpContext".to_string(),
            },
        );
        registry.insert(
            "annotation".to_string(),
            AnnotationInfo {
                name: "annotation".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Marks a class as an annotation definition".to_string(),
            },
        );

        // WebSocket endpoint annotations
        registry.insert(
            "WebsocketEndpoint".to_string(),
            AnnotationInfo {
                name: "WebsocketEndpoint".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 1,
                max_args: 2,
                description: "@WebsocketEndpoint(\"/path\"[, port]) — marks a class as a WebSocket endpoint; the compiler generates an auto-run accept/message loop as `main` (port defaults to the TINOX_PORT env var, else 8080). Only valid when the file defines no `main` and has exactly one @WebsocketEndpoint class.".to_string(),
            },
        );
        registry.insert(
            "OnOpen".to_string(),
            AnnotationInfo {
                name: "OnOpen".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 0,
                description: "Marks the method called once a WebSocket connection is accepted; signature fn(conn: Int64) -> Nothing".to_string(),
            },
        );
        registry.insert(
            "OnMessage".to_string(),
            AnnotationInfo {
                name: "OnMessage".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 0,
                description: "Marks the method called for each incoming message; exact signature depends on the enclosing class annotation: fn(conn: Int64, msg: String) -> Nothing for @WebsocketEndpoint, fn(msg: Amqp10Message) -> Nothing for @Amqp10Consumer, fn(msg: AmqpMessage091) -> Nothing for @Amqp091Consumer".to_string(),
            },
        );
        registry.insert(
            "OnClose".to_string(),
            AnnotationInfo {
                name: "OnClose".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 0,
                description: "Marks the method called when the connection ends (Close/EOF/protocol error); signature fn(conn: Int64) -> Nothing".to_string(),
            },
        );

        // Annotation-driven HTTP/3 REST controller: routes @GET/@POST/
        // @PUT/@PATCH/@DELETE methods through tinox.core.http3_server's
        // Http3Server (not the GC-crash-prone TCP auto-server, issue #140).
        registry.insert(
            "Http3RestController".to_string(),
            AnnotationInfo {
                name: "Http3RestController".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 3,
                max_args: 3,
                description: "@Http3RestController(port, certPath, keyPath) — marks a class whose @GET/@POST/@PUT/@PATCH/@DELETE methods (anywhere in the program) should be served over HTTP/3 (QUIC) via tinox.core.http3_server.Http3Server, instead of the TCP auto-server. Requires `import tinox.core.http3_server;` and a runtime built with TINOX_HTTP3=1. Only valid when the file defines no `main`, has exactly one @Http3RestController class, and no @WebsocketEndpoint/@Amqp10Consumer/@Amqp091Consumer.".to_string(),
            },
        );

        // Tinox-UI annotation sugar (issue #215, Phase 4)
        registry.insert(
            "TinoxUIApp".to_string(),
            AnnotationInfo {
                name: "TinoxUIApp".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 2,
                max_args: 2,
                description: "@TinoxUIApp(httpPort, wsPort) — marks a class as a Tinox-UI application; the compiler generates the HTTP shell/client-JS server on httpPort and a WebSocket accept loop on wsPort that calls the class's own @View method to build/rebuild the component tree (full-resend rendering, auto re-render after every event). Requires `import tinox.core.ui;`, `import tinox.core.websocket;`, and `import tinox.core.http_server;`. At most one @TinoxUIApp class per program, with exactly one @View method.".to_string(),
            },
        );
        registry.insert(
            "View".to_string(),
            AnnotationInfo {
                name: "View".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 0,
                description: "Marks the method that builds this @TinoxUIApp's component tree; signature fn() -> Component".to_string(),
            },
        );
        registry.insert(
            "Route".to_string(),
            AnnotationInfo {
                name: "Route".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 1,
                max_args: 1,
                description: "@Route(\"/path/:param\") -- on a `fn() -> Component` method inside a @TinoxUIApp class, registers it as that path's builder (Vaadin-style route dispatch). The class must declare `var currentRoute: String;`: the compiler seeds it from the browser's initial request path at WS connect time, and the app's own navigation (e.g. Component::link's onNavigate handler) is expected to assign it on every subsequent navigation -- the compiler re-dispatches off its current value on every render. Patterns may use `:name` path-parameter segments (RouteMatcher syntax); a matched `:name` is auto-assigned into a same-named String field on the class, if one exists, before the method runs. When the current route matches no @Route pattern, the class's plain @View method renders instead (fallback/404 case) -- @View stays required even when @Route is used. Also registers the HTTP shell at each literal @Route path so a hard reload/deep link doesn't 404.".to_string(),
            },
        );

        // AMQP-1.0 annotation-driven consumer (Issue #81)
        registry.insert(
            "Amqp10Consumer".to_string(),
            AnnotationInfo {
                name: "Amqp10Consumer".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 5,
                max_args: 5,
                description: "@Amqp10Consumer(host, port, user, pass, address) — marks a class as an auto-run AMQP-1.0 receiver; the compiler generates a connect/begin/attach/grantCredit/nextMessage/ack loop as `main` that calls the class's @OnMessage method for each delivered message. Only valid when the file defines no `main` and has exactly one @Amqp10Consumer class.".to_string(),
            },
        );

        // AMQP-0-9-1 annotation-driven consumer (Issue #126)
        registry.insert(
            "Amqp091Consumer".to_string(),
            AnnotationInfo {
                name: "Amqp091Consumer".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 6,
                max_args: 6,
                description: "@Amqp091Consumer(host, port, vhost, user, pass, queue) — marks a class as an auto-run AMQP-0-9-1 receiver; the compiler generates a connect/open/qos/consume/nextMessage/ack loop as `main` that calls the class's @OnMessage method for each delivered message. Only valid when the file defines no `main` and has exactly one @Amqp091Consumer class.".to_string(),
            },
        );

        // Config injection annotation
        registry.insert(
            "Config".to_string(),
            AnnotationInfo {
                name: "Config".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 1,
                max_args: 1,
                description: "Injects a value from application.properties into the field".to_string(),
            },
        );

        // Logging annotation
        registry.insert(
            "Log".to_string(),
            AnnotationInfo {
                name: "Log".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Injects a 'log: Logger' field initialized with Logger::new(ClassName)".to_string(),
            },
        );

        // DI scope annotations
        registry.insert(
            "ApplicationComponent".to_string(),
            AnnotationInfo {
                name: "ApplicationComponent".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Lazy singleton — one instance for the lifetime of the application".to_string(),
            },
        );
        registry.insert(
            "Startup".to_string(),
            AnnotationInfo {
                name: "Startup".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Eager singleton — created immediately at application startup".to_string(),
            },
        );
        registry.insert(
            "HttpRequestScoped".to_string(),
            AnnotationInfo {
                name: "HttpRequestScoped".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "One instance per HTTP request, lives as long as the request".to_string(),
            },
        );
        registry.insert(
            "Inject".to_string(),
            AnnotationInfo {
                name: "Inject".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Marks a field for compile-time dependency injection".to_string(),
            },
        );

        // Test framework annotations
        registry.insert(
            "Test".to_string(),
            AnnotationInfo {
                name: "Test".to_string(),
                valid_targets: vec![AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "@Test[(\"description\")] — marks a method as a test case".to_string(),
            },
        );

        // CLI framework annotations (@Command / @Option / @Argument)
        registry.insert(
            "Command".to_string(),
            AnnotationInfo {
                name: "Command".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 1,
                max_args: 3,
                description: "@Command(\"name\", \"description\"[, \"version\"]) — marks a class as a CLI command".to_string(),
            },
        );
        registry.insert(
            "Option".to_string(),
            AnnotationInfo {
                name: "Option".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 1,
                max_args: 3,
                description: "@Option(\"--long,-s\", \"description\"[, required]) — CLI option backed by a field".to_string(),
            },
        );
        registry.insert(
            "Argument".to_string(),
            AnnotationInfo {
                name: "Argument".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 1,
                max_args: 3,
                description: "@Argument(index, \"description\"[, required]) — positional CLI argument".to_string(),
            },
        );

        // Log masking annotations
        registry.insert(
            "Sensitive".to_string(),
            AnnotationInfo {
                name: "Sensitive".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Marks a field as fully sensitive — logged as '***'".to_string(),
            },
        );
        registry.insert(
            "Masked".to_string(),
            AnnotationInfo {
                name: "Masked".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Marks a field for partial masking in logs (shows first/last chars)".to_string(),
            },
        );

        // Serialization annotations
        registry.insert(
            "DoNotSerialize".to_string(),
            AnnotationInfo {
                name: "DoNotSerialize".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Excludes a field from all serialization (JSON, XML, etc.)".to_string(),
            },
        );

        // Metrics annotations
        registry.insert(
            "Timed".to_string(),
            AnnotationInfo {
                name: "Timed".to_string(),
                valid_targets: vec![AnnotationTarget::Function, AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "@Timed[(\"label\")] — records execution time as a Prometheus summary".to_string(),
            },
        );
        registry.insert(
            "Counted".to_string(),
            AnnotationInfo {
                name: "Counted".to_string(),
                valid_targets: vec![AnnotationTarget::Function, AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "@Counted[(\"label\")] — increments a Prometheus counter on each call".to_string(),
            },
        );
        registry.insert(
            "Gauge".to_string(),
            AnnotationInfo {
                name: "Gauge".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 1,
                max_args: 1,
                description: "@Gauge(\"label\") — tracks a numeric field as a Prometheus gauge".to_string(),
            },
        );

        // ORM annotations
        registry.insert(
            "Entity".to_string(),
            AnnotationInfo {
                name: "Entity".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 0,
                max_args: 0,
                description: "Marks a class as a database entity".to_string(),
            },
        );
        registry.insert(
            "Table".to_string(),
            AnnotationInfo {
                name: "Table".to_string(),
                valid_targets: vec![AnnotationTarget::Class],
                min_args: 1,
                max_args: 1,
                description: "@Table(\"table_name\") — sets the DB table name".to_string(),
            },
        );
        registry.insert(
            "Id".to_string(),
            AnnotationInfo {
                name: "Id".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Marks field as primary key".to_string(),
            },
        );
        registry.insert(
            "Column".to_string(),
            AnnotationInfo {
                name: "Column".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 1,
                description: "@Column[(\"col_name\")] — maps field to DB column".to_string(),
            },
        );
        registry.insert(
            "GeneratedValue".to_string(),
            AnnotationInfo {
                name: "GeneratedValue".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "ID is generated by the DB (AUTO_INCREMENT / SERIAL)".to_string(),
            },
        );
        registry.insert(
            "NotNull".to_string(),
            AnnotationInfo {
                name: "NotNull".to_string(),
                valid_targets: vec![AnnotationTarget::Field],
                min_args: 0,
                max_args: 0,
                description: "Column is NOT NULL".to_string(),
            },
        );

        // Compiler annotations
        registry.insert(
            "inline".to_string(),
            AnnotationInfo {
                name: "inline".to_string(),
                valid_targets: vec![AnnotationTarget::Function, AnnotationTarget::Method],
                min_args: 0,
                max_args: 1,
                description: "Hints that the function should be inlined".to_string(),
            },
        );
        registry.insert(
            "deprecated".to_string(),
            AnnotationInfo {
                name: "deprecated".to_string(),
                valid_targets: vec![
                    AnnotationTarget::Function,
                    AnnotationTarget::Method,
                    AnnotationTarget::Class,
                ],
                min_args: 0,
                max_args: 1,
                description: "Marks the declaration as deprecated".to_string(),
            },
        );

        Self { registry }
    }

    pub fn register_custom_annotation(&mut self, name: &str) {
        self.registry.insert(
            name.to_string(),
            AnnotationInfo {
                name: name.to_string(),
                valid_targets: vec![
                    AnnotationTarget::Function,
                    AnnotationTarget::Method,
                    AnnotationTarget::Class,
                    AnnotationTarget::Field,
                    AnnotationTarget::Interface,
                    AnnotationTarget::Enum,
                    AnnotationTarget::Trait,
                    AnnotationTarget::Namespace,
                ],
                min_args: 0,
                max_args: usize::MAX,
                description: format!("User-defined annotation @{}", name),
            },
        );
    }

    pub fn validate(&self, annotations: &[Annotation], target: AnnotationTarget) -> Vec<Error> {
        let mut errors = Vec::new();
        for ann in annotations {
            match self.registry.get(&ann.name) {
                Some(info) => {
                    if !info.valid_targets.contains(&target) {
                        errors.push(Error::new(
                            ann.span,
                            format!(
                                "@{} cannot be applied to {:?} (valid targets: {})",
                                ann.name,
                                target,
                                info.valid_targets
                                    .iter()
                                    .map(|t| format!("{:?}", t))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        ));
                    }
                    if ann.args.len() < info.min_args {
                        errors.push(Error::new(
                            ann.span,
                            format!(
                                "@{} requires at least {} argument(s), found {}",
                                ann.name,
                                info.min_args,
                                ann.args.len()
                            ),
                        ));
                    }
                    if ann.args.len() > info.max_args {
                        errors.push(Error::new(
                            ann.span,
                            format!(
                                "@{} accepts at most {} argument(s), found {}",
                                ann.name,
                                info.max_args,
                                ann.args.len()
                            ),
                        ));
                    }
                }
                None => {
                    errors.push(Error::new(
                        ann.span,
                        format!("unknown annotation: @{}", ann.name),
                    ));
                }
            }
        }
        errors
    }

    pub fn process_source(
        &self,
        source: &tinox_parser::SourceFile,
    ) -> AnnotationProcessingResult {
        let mut result = AnnotationProcessingResult::default();

        for decl in &source.decls {
            match &decl.node {
                DeclKind::Class(c) => {
                    self.process_class_annotations(c, &mut result);
                }
                DeclKind::Function(f) => {
                    self.process_function_annotations(f, &mut result);
                }
                DeclKind::Namespace(ns) => {
                    self.process_namespace_annotations(ns, &mut result);
                }
                _ => {}
            }
        }

        result
    }

    fn process_class_annotations(
        &self,
        class: &Class,
        result: &mut AnnotationProcessingResult,
    ) {
        let mut class_base_path: Option<String> = None;
        let mut class_auth: Option<String> = None;
        let mut class_transactional = false;
        let mut di_scope: Option<DiScope> = None;
        let mut ws_endpoint_path: Option<String> = None;
        let mut ws_endpoint_port: Option<i64> = None;
        let mut amqp10_consumer_args: Option<(String, i64, String, String, String)> = None;
        let mut amqp091_consumer_args: Option<(String, i64, String, String, String, String)> = None;
        let mut http3_rest_controller_args: Option<(i64, String, String)> = None;
        let mut tinoxui_app_args: Option<(i64, i64)> = None;

        for ann in &class.annotations {
            match ann.name.as_str() {
                "Path" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        class_base_path = Some(s.clone());
                    }
                }
                "WebsocketEndpoint" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        ws_endpoint_path = Some(s.clone());
                    }
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.get(1) {
                        ws_endpoint_port = Some(*p);
                    }
                }
                "Amqp10Consumer" => {
                    let host = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() { Some(s.clone()) } else { None };
                    let port = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.get(1) { Some(*p) } else { None };
                    let user = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(2) { Some(s.clone()) } else { None };
                    let pass = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(3) { Some(s.clone()) } else { None };
                    let address = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(4) { Some(s.clone()) } else { None };
                    if let (Some(host), Some(port), Some(user), Some(pass), Some(address)) = (host, port, user, pass, address) {
                        amqp10_consumer_args = Some((host, port, user, pass, address));
                    }
                }
                "Amqp091Consumer" => {
                    let host = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() { Some(s.clone()) } else { None };
                    let port = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.get(1) { Some(*p) } else { None };
                    let vhost = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(2) { Some(s.clone()) } else { None };
                    let user = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(3) { Some(s.clone()) } else { None };
                    let pass = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(4) { Some(s.clone()) } else { None };
                    let queue = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(5) { Some(s.clone()) } else { None };
                    if let (Some(host), Some(port), Some(vhost), Some(user), Some(pass), Some(queue)) = (host, port, vhost, user, pass, queue) {
                        amqp091_consumer_args = Some((host, port, vhost, user, pass, queue));
                    }
                }
                "Http3RestController" => {
                    let port = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.first() { Some(*p) } else { None };
                    let cert_path = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(1) { Some(s.clone()) } else { None };
                    let key_path = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(2) { Some(s.clone()) } else { None };
                    if let (Some(port), Some(cert_path), Some(key_path)) = (port, cert_path, key_path) {
                        http3_rest_controller_args = Some((port, cert_path, key_path));
                    }
                }
                "TinoxUIApp" => {
                    let http_port = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.first() { Some(*p) } else { None };
                    let ws_port = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(p))) = ann.args.get(1) { Some(*p) } else { None };
                    if let (Some(http_port), Some(ws_port)) = (http_port, ws_port) {
                        tinoxui_app_args = Some((http_port, ws_port));
                    }
                }
                "Auth" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        class_auth = Some(s.clone());
                    }
                }
                "Transactional" => {
                    class_transactional = true;
                }
                "deprecated" => {
                    let msg = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        format!("class '{}' is deprecated: {}", class.name, s)
                    } else {
                        format!("class '{}' is deprecated", class.name)
                    };
                    result.deprecated_warnings.push(msg);
                }
                "annotation" => {
                    result.custom_annotation_names.push(class.name.clone());
                }
                "ApplicationComponent" => di_scope = Some(DiScope::Application),
                "Startup" => di_scope = Some(DiScope::Startup),
                "HttpRequestScoped" => di_scope = Some(DiScope::HttpRequest),
                "Log" => {
                    result.log_classes.insert(class.name.clone());
                }
                "Command" => {
                    let cmd_name = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        s.clone()
                    } else {
                        class.name.clone()
                    };
                    let description = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(1) {
                        s.clone()
                    } else {
                        String::new()
                    };
                    let version = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.get(2) {
                        Some(s.clone())
                    } else {
                        None
                    };

                    let mut options: Vec<CliOptionInfo> = Vec::new();
                    let mut arguments: Vec<CliArgumentInfo> = Vec::new();
                    for field in &class.fields {
                        for fann in &field.annotations {
                            match fann.name.as_str() {
                                "Option" => {
                                    let names_str = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = fann.args.first() { s.clone() } else { String::new() };
                                    let names: Vec<String> = names_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                                    let desc = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = fann.args.get(1) { s.clone() } else { String::new() };
                                    let required = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Bool(b))) = fann.args.get(2) { *b } else { false };
                                    options.push(CliOptionInfo {
                                        field_name: field.name.clone(),
                                        names,
                                        description: desc,
                                        required,
                                        field_type: field_type_to_cli_str(&field.field_type),
                                    });
                                }
                                "Argument" => {
                                    let index = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(i))) = fann.args.first() { *i as usize } else { 0 };
                                    let desc = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = fann.args.get(1) { s.clone() } else { String::new() };
                                    let required = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Bool(b))) = fann.args.get(2) { *b } else { false };
                                    arguments.push(CliArgumentInfo {
                                        field_name: field.name.clone(),
                                        index,
                                        description: desc,
                                        required,
                                        field_type: field_type_to_cli_str(&field.field_type),
                                    });
                                }
                                _ => {}
                            }
                        }
                    }

                    result.cli_commands.push(CliCommandInfo {
                        class_name: class.name.clone(),
                        cmd_name,
                        description,
                        version,
                        options,
                        arguments,
                    });
                }
                _ => {}
            }
        }

        if let Some(scope) = di_scope {
            let inject_fields = collect_inject_fields(&class.fields);
            result.di_components.push(DiComponentInfo {
                class_name: class.name.clone(),
                scope,
                inject_fields,
            });
        }

        let cfg_fields = collect_config_fields(&class.name, &class.fields);
        result.config_fields.extend(cfg_fields);

        result.sensitive_fields.extend(collect_log_mask_fields("Sensitive", &class.name, &class.fields));
        result.masked_fields.extend(collect_log_mask_fields("Masked", &class.name, &class.fields));
        result.do_not_serialize_fields.extend(collect_log_mask_fields("DoNotSerialize", &class.name, &class.fields));
        if class.annotations.iter().any(|a| a.name == "JsonSerializable") {
            result.json_serializable_classes.push(class.name.clone());
        }

        let has_entity = class.annotations.iter().any(|a| a.name == "Entity" || a.name == "Table");
        if has_entity {
            let table_name = class.annotations.iter()
                .find(|a| a.name == "Table")
                .and_then(|a| a.args.first())
                .and_then(|arg| if let tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s)) = arg { Some(s.clone()) } else { None })
                .unwrap_or_else(|| class.name.to_lowercase());

            let fields = class.fields.iter().map(|f| {
                let col_name = f.annotations.iter()
                    .find(|a| a.name == "Column")
                    .and_then(|a| a.args.first())
                    .and_then(|arg| if let tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s)) = arg { Some(s.clone()) } else { None })
                    .unwrap_or_else(|| f.name.clone());
                EntityFieldInfo {
                    field_name: f.name.clone(),
                    column_name: col_name,
                    is_id: f.annotations.iter().any(|a| a.name == "Id"),
                    is_generated: f.annotations.iter().any(|a| a.name == "GeneratedValue"),
                    not_null: f.annotations.iter().any(|a| a.name == "NotNull"),
                    field_llvm_type: field_type_to_llvm(&f.field_type),
                }
            }).collect();

            result.entity_entries.push(EntityInfo {
                class_name: class.name.clone(),
                table_name,
                fields,
            });
        }

        for method in &class.methods {
            let route = self.extract_route_from_method(
                method,
                &class.name,
                class_base_path.as_deref(),
                class_auth.as_deref(),
            );
            if let Some(route) = route {
                result.route_entries.push(route);
            }

            if class_transactional || method.annotations.iter().any(|a| a.name == "Transactional") {
                result.transactional_methods.insert((class.name.clone(), method.name.clone()));
            }

            for ann in &method.annotations {
                match ann.name.as_str() {
                    "inline" => {
                        result
                            .inline_methods
                            .insert((class.name.clone(), method.name.clone()));
                    }
                    "deprecated" => {
                        let msg = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                            format!("method '{}.{}' is deprecated: {}", class.name, method.name, s)
                        } else {
                            format!("method '{}.{}' is deprecated", class.name, method.name)
                        };
                        result.deprecated_warnings.push(msg);
                    }
                    "Test" => {
                        let description = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                            s.clone()
                        } else {
                            method.name.clone()
                        };
                        result.test_entries.push(TestInfo {
                            class_name: class.name.clone(),
                            method_name: method.name.clone(),
                            description,
                        });
                    }
                    "Timed" | "Counted" => {
                        let default_label = format!("{}_{}", class.name, method.name);
                        let label = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                            s.clone()
                        } else {
                            default_label
                        };
                        let kind = if ann.name == "Timed" { MetricKind::Timed } else { MetricKind::Counted };
                        result.metric_entries.push(MetricInfo {
                            kind,
                            metric_name: label,
                            class_name: class.name.clone(),
                            fn_name: method.name.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }

        if let Some(path) = ws_endpoint_path {
            let mut on_open: Option<String> = None;
            let mut on_message: Option<String> = None;
            let mut on_close: Option<String> = None;
            for method in &class.methods {
                for ann in &method.annotations {
                    match ann.name.as_str() {
                        "OnOpen" => on_open = Some(method.name.clone()),
                        "OnMessage" => on_message = Some(method.name.clone()),
                        "OnClose" => on_close = Some(method.name.clone()),
                        _ => {}
                    }
                }
            }
            result.ws_endpoints.push(WsEndpointInfo {
                class_name: class.name.clone(),
                path,
                port: ws_endpoint_port,
                on_open,
                on_message,
                on_close,
            });
        }

        if let Some((host, port, user, pass, address)) = amqp10_consumer_args {
            let mut on_message: Option<String> = None;
            for method in &class.methods {
                for ann in &method.annotations {
                    if ann.name == "OnMessage" {
                        on_message = Some(method.name.clone());
                    }
                }
            }
            result.amqp10_consumers.push(Amqp10ConsumerInfo {
                class_name: class.name.clone(),
                host,
                port,
                user,
                pass,
                address,
                on_message,
            });
        }

        if let Some((host, port, vhost, user, pass, queue)) = amqp091_consumer_args {
            let mut on_message: Option<String> = None;
            for method in &class.methods {
                for ann in &method.annotations {
                    if ann.name == "OnMessage" {
                        on_message = Some(method.name.clone());
                    }
                }
            }
            result.amqp091_consumers.push(Amqp091ConsumerInfo {
                class_name: class.name.clone(),
                host,
                port,
                vhost,
                user,
                pass,
                queue,
                on_message,
            });
        }

        if let Some((port, cert_path, key_path)) = http3_rest_controller_args {
            result.http3_rest_controllers.push(Http3RestControllerInfo {
                class_name: class.name.clone(),
                port,
                cert_path,
                key_path,
            });
        }

        if let Some((http_port, ws_port)) = tinoxui_app_args {
            let mut view_methods: Vec<String> = Vec::new();
            let mut route_entries: Vec<(String, String)> = Vec::new();
            for method in &class.methods {
                for ann in &method.annotations {
                    if ann.name == "View" {
                        view_methods.push(method.name.clone());
                    }
                    if ann.name == "Route" {
                        if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(path))) = ann.args.first() {
                            route_entries.push((path.clone(), method.name.clone()));
                        }
                    }
                }
            }
            result.tinoxui_apps.push(TinoxUIAppInfo {
                class_name: class.name.clone(),
                http_port,
                ws_port,
                view_methods,
                route_entries,
            });
        }
    }

    fn process_function_annotations(
        &self,
        f: &Function,
        result: &mut AnnotationProcessingResult,
    ) {
        for ann in &f.annotations {
            match ann.name.as_str() {
                "inline" => {
                    result.inline_functions.insert(f.name.clone());
                }
                "deprecated" => {
                    let msg = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        format!("function '{}' is deprecated: {}", f.name, s)
                    } else {
                        format!("function '{}' is deprecated", f.name)
                    };
                    result.deprecated_warnings.push(msg);
                }
                "Timed" | "Counted" => {
                    let label = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        s.clone()
                    } else {
                        f.name.clone()
                    };
                    let kind = if ann.name == "Timed" { MetricKind::Timed } else { MetricKind::Counted };
                    result.metric_entries.push(MetricInfo {
                        kind,
                        metric_name: label,
                        class_name: String::new(),
                        fn_name: f.name.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    fn process_namespace_annotations(
        &self,
        ns: &Namespace,
        result: &mut AnnotationProcessingResult,
    ) {
        for inner in &ns.decls {
            match &inner.node {
                DeclKind::Class(c) => self.process_class_annotations(c, result),
                DeclKind::Function(f) => self.process_function_annotations(f, result),
                DeclKind::Namespace(nested) => self.process_namespace_annotations(nested, result),
                _ => {}
            }
        }
    }

    fn extract_route_from_method(
        &self,
        method: &Method,
        class_name: &str,
        class_base_path: Option<&str>,
        class_auth: Option<&str>,
    ) -> Option<RouteInfo> {
        let mut http_method: Option<String> = None;
        let mut method_path: Option<String> = None;
        let mut status_code: Option<i64> = None;
        let mut produces: Option<String> = None;
        let mut consumes: Option<String> = None;
        let mut auth: Option<String> = class_auth.map(|s| s.to_string());
        let mut oidc_roles: Vec<String> = Vec::new();

        for ann in &method.annotations {
            match ann.name.as_str() {
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" => {
                    http_method = Some(ann.name.clone());
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        method_path = Some(s.clone());
                    }
                }
                "Path" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        method_path = Some(s.clone());
                    }
                }
                "StatusCode" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::Integer(n))) = ann.args.first() {
                        status_code = Some(*n);
                    }
                }
                "Produces" => {
                    produces = ann.args.first().and_then(media_type_arg_to_mime);
                }
                "Consumes" => {
                    consumes = ann.args.first().and_then(media_type_arg_to_mime);
                }
                "Auth" => {
                    if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                        auth = Some(s.clone());
                    }
                }
                "OIDCRolesAllowed" => {
                    if let Some(tinox_parser::AnnotationArg::Array(items)) = ann.args.first() {
                        oidc_roles = items
                            .iter()
                            .filter_map(|a| match a {
                                tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s)) => Some(s.clone()),
                                _ => None,
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }

        let m = http_method?;
        let p = method_path.unwrap_or_default();
        let full_path = match class_base_path {
            Some(base) => {
                if p.is_empty() {
                    base.to_string()
                } else if base.ends_with('/') && p.starts_with('/') {
                    format!("{}{}", &base[..base.len() - 1], p)
                } else if base.ends_with('/') || p.starts_with('/') {
                    format!("{}{}", base, p)
                } else {
                    format!("{}/{}", base, p)
                }
            }
            None => p,
        };

        // Data extraction only -- infallible by design (matches this
        // function's existing convention). A parameter with zero or more
        // than one binding annotation simply gets no entry here; the real
        // "every parameter needs exactly one" + type-compatibility +
        // return-type checks live in validate_decl/validate_route_params
        // below, which run as part of the real typecheck error pipeline
        // (check_source_file -> validate_annotations) and produce proper
        // spanned compile errors instead of a confusing codegen-internal
        // failure downstream.
        let params: Vec<RouteParamBinding> = method.params.iter().filter_map(|p| {
            let kind = p.annotations.iter().find_map(|a| match a.name.as_str() {
                "PathParam" => Some(RouteParamKind::PathParam),
                "QueryParam" => Some(RouteParamKind::QueryParam),
                "PostParam" => Some(RouteParamKind::PostParam),
                "HttpContext" => Some(RouteParamKind::HttpContext),
                _ => None,
            })?;
            Some(RouteParamBinding { kind, name: p.name.clone(), ty: p.param_type.clone() })
        }).collect();

        Some(RouteInfo {
            method: m,
            path: full_path,
            class_name: class_name.to_string(),
            method_name: method.name.clone(),
            status_code,
            produces,
            consumes,
            auth_type: auth,
            oidc_roles,
            is_static: method.static_,
            params,
            return_type: method.ret_type.clone(),
        })
    }
}

impl Default for AnnotationProcessor {
    fn default() -> Self {
        Self::new()
    }
}

pub fn process_annotations(
    source: &tinox_parser::SourceFile,
) -> AnnotationProcessingResult {
    let processor = AnnotationProcessor::new();
    processor.process_source(source)
}

fn field_type_to_cli_str(ty: &Type) -> String {
    match ty {
        Type::Bool => "Bool".to_string(),
        Type::String => "String".to_string(),
        _ => "Int".to_string(),
    }
}

fn field_type_to_llvm(ty: &Type) -> String {
    match ty {
        Type::String => "i8*".to_string(),
        Type::Bool => "i1".to_string(),
        _ => "i64".to_string(),
    }
}

fn collect_config_fields(class_name: &str, fields: &[FieldDef]) -> Vec<ConfigFieldInfo> {
    fields
        .iter()
        .filter_map(|f| {
            let ann = f.annotations.iter().find(|a| a.name == "Config")?;
            let key = if let Some(tinox_parser::AnnotationArg::Literal(tinox_parser::Literal::String(s))) = ann.args.first() {
                s.clone()
            } else {
                return None;
            };
            Some(ConfigFieldInfo {
                class_name: class_name.to_string(),
                field_name: f.name.clone(),
                config_key: key,
                field_llvm_type: field_type_to_llvm(&f.field_type),
            })
        })
        .collect()
}

fn collect_inject_fields(fields: &[FieldDef]) -> Vec<DiInjectField> {
    fields
        .iter()
        .filter(|f| f.annotations.iter().any(|a| a.name == "Inject"))
        .filter_map(|f| {
            if let Type::Named(type_name) = &f.field_type {
                Some(DiInjectField {
                    field_name: f.name.clone(),
                    field_type: type_name.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn collect_log_mask_fields(ann_name: &str, class_name: &str, fields: &[FieldDef]) -> Vec<LogMaskFieldInfo> {
    fields
        .iter()
        .filter(|f| f.annotations.iter().any(|a| a.name == ann_name))
        .map(|f| LogMaskFieldInfo {
            class_name: class_name.to_string(),
            field_name: f.name.clone(),
        })
        .collect()
}

fn collect_custom_annotation_classes(decl: &DeclKind, processor: &mut AnnotationProcessor) {
    match decl {
        DeclKind::Class(c) => {
            if c.annotations.iter().any(|a| a.name == "annotation") {
                processor.register_custom_annotation(&c.name);
            }
        }
        DeclKind::Namespace(ns) => {
            for inner in &ns.decls {
                collect_custom_annotation_classes(&inner.node, processor);
            }
        }
        _ => {}
    }
}

fn validate_decl(processor: &AnnotationProcessor, decl: &DeclKind, json_serializable: &HashSet<String>, errors: &mut Vec<Error>) {
    match decl {
        DeclKind::Function(f) => {
            errors.extend(processor.validate(&f.annotations, AnnotationTarget::Function));
            for param in &f.params {
                errors.extend(processor.validate(&param.annotations, AnnotationTarget::Param));
            }
        }
        DeclKind::Class(c) => {
            errors.extend(processor.validate(&c.annotations, AnnotationTarget::Class));
            for field in &c.fields {
                errors.extend(processor.validate(&field.annotations, AnnotationTarget::Field));
            }
            for method in &c.methods {
                errors.extend(processor.validate(&method.annotations, AnnotationTarget::Method));
                for param in &method.params {
                    errors.extend(processor.validate(&param.annotations, AnnotationTarget::Param));
                }
                validate_route_params(method, json_serializable, errors);
            }
        }
        DeclKind::Interface(i) => {
            errors.extend(processor.validate(&i.annotations, AnnotationTarget::Interface));
            for method in &i.methods {
                errors.extend(processor.validate(&method.annotations, AnnotationTarget::Method));
                for param in &method.params {
                    errors.extend(processor.validate(&param.annotations, AnnotationTarget::Param));
                }
            }
        }
        DeclKind::Enum(e) => {
            errors.extend(processor.validate(&e.annotations, AnnotationTarget::Enum));
        }
        DeclKind::Trait(t) => {
            errors.extend(processor.validate(&t.annotations, AnnotationTarget::Trait));
            for method in &t.methods {
                errors.extend(processor.validate(&method.annotations, AnnotationTarget::Method));
                for param in &method.params {
                    errors.extend(processor.validate(&param.annotations, AnnotationTarget::Param));
                }
            }
        }
        DeclKind::Namespace(ns) => {
            errors.extend(processor.validate(&ns.annotations, AnnotationTarget::Namespace));
            for inner in &ns.decls {
                validate_decl(processor, &inner.node, json_serializable, errors);
            }
        }
        _ => {}
    }
}

/// Scalar types `@PathParam`/`@QueryParam` can convert a raw path/query
/// string into -- matches the strict runtime parsers
/// (`tinox_parse_int_checked`/etc., runtime.c) added alongside this.
fn is_supported_param_scalar_type(ty: &Type) -> bool {
    matches!(ty, Type::String | Type::Int64 | Type::Int32 | Type::Bool | Type::Float64 | Type::Float32)
}

fn is_json_serializable_class_type(ty: &Type, json_serializable: &HashSet<String>) -> bool {
    matches!(ty, Type::Named(n) if json_serializable.contains(n))
}

fn is_json_serializable_list_type(ty: &Type, json_serializable: &HashSet<String>) -> bool {
    match ty {
        Type::Generic { name, args } if name == "List" || name == "Array" => {
            args.first().is_some_and(|t| is_json_serializable_class_type(t, json_serializable))
        }
        Type::Array(inner) => is_json_serializable_class_type(inner, json_serializable),
        _ => false,
    }
}

fn is_http_context_type(ty: &Type) -> bool {
    matches!(ty, Type::Named(n) if n == "HttpContext")
}

/// Semantic validation for REST parameter-binding annotations, beyond the
/// generic per-annotation target/arity checks `validate_decl`/`validate`
/// already do: cardinality (every parameter needs exactly one of the
/// four), type compatibility per binding kind, and return-type validity.
/// Only runs for methods that actually carry an HTTP-verb annotation
/// (`@GET`/`@POST`/`@PUT`/`@PATCH`/`@DELETE`) -- an ordinary method is
/// never reachable here, so e.g. `@HttpContext` on a plain helper method's
/// parameter is simply never checked by this function (the generic
/// target-mismatch check in `validate` already rejects it regardless,
/// since `@HttpContext`'s `valid_targets` is `[Param]` either way -- this
/// function only adds the REST-specific cardinality/compatibility rules
/// on top).
fn validate_route_params(method: &Method, json_serializable: &HashSet<String>, errors: &mut Vec<Error>) {
    let has_verb = method.annotations.iter()
        .any(|a| matches!(a.name.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE"));
    if !has_verb {
        return;
    }

    for param in &method.params {
        let bindings: Vec<&Annotation> = param.annotations.iter()
            .filter(|a| matches!(a.name.as_str(), "PathParam" | "QueryParam" | "PostParam" | "HttpContext"))
            .collect();
        match bindings.len() {
            0 => {
                errors.push(Error::new(param.span, format!(
                    "REST handler parameter '{}' needs exactly one of @PathParam/@QueryParam/@PostParam/@HttpContext",
                    param.name
                )));
                continue;
            }
            1 => {}
            _ => {
                errors.push(Error::new(param.span, format!(
                    "REST handler parameter '{}' has more than one binding annotation ({}) -- exactly one is required",
                    param.name,
                    bindings.iter().map(|a| format!("@{}", a.name)).collect::<Vec<_>>().join(", ")
                )));
                continue;
            }
        }
        let binding = bindings[0];
        match binding.name.as_str() {
            "PathParam" | "QueryParam" => {
                if !is_supported_param_scalar_type(&param.param_type) {
                    errors.push(Error::new(param.span, format!(
                        "@{} on '{}' must be String/Int64/Int32/Bool/Float64/Float32, found {:?}",
                        binding.name, param.name, param.param_type
                    )));
                }
            }
            "PostParam" => {
                if !is_json_serializable_class_type(&param.param_type, json_serializable) {
                    errors.push(Error::new(param.span, format!(
                        "@PostParam on '{}' must be a class marked @JsonSerializable, found {:?}",
                        param.name, param.param_type
                    )));
                }
            }
            "HttpContext" => {
                if !is_http_context_type(&param.param_type) {
                    errors.push(Error::new(param.span, format!(
                        "@HttpContext on '{}' must be of type HttpContext, found {:?}",
                        param.name, param.param_type
                    )));
                }
            }
            _ => unreachable!("filtered to the four binding annotation names above"),
        }
    }

    let return_ok = is_http_context_type(&method.ret_type)
        || is_json_serializable_class_type(&method.ret_type, json_serializable)
        || is_json_serializable_list_type(&method.ret_type, json_serializable)
        || matches!(&method.ret_type, Type::String | Type::Int64 | Type::Int32 | Type::Bool);
    if !return_ok {
        errors.push(Error::new(method.span, format!(
            "REST handler '{}' cannot auto-serialize return type {:?} -- use HttpContext for a manually-built \
             response, or a @JsonSerializable class / List<@JsonSerializable class> / String / Int64 / Int32 / Bool",
            method.name, method.ret_type
        )));
    }
}

/// `extra_decls` covers declarations that are visible to `source` but not
/// physically present in `source.decls` -- true for tinox-lsp's
/// `typecheck_with_prelude`, which keeps the main file and its stdlib
/// preludes as separate `SourceFile`s instead of merging them the way the
/// real compiler's `resolve_imports` does before typechecking ever runs.
/// Without this, a custom annotation declared in a prelude (e.g.
/// `@JsonSerializable`, itself declared via `@annotation class
/// JsonSerializable {}` inside `tinox.core.json`'s own
/// `JsonSerializable.tnx`) reads as "unknown annotation" for any file that
/// merely imports it, since the registration passes below never saw it.
/// The real compiler's own call site passes an empty slice here -- its
/// `source.decls` is already fully merged, so there's nothing extra to add.
pub fn validate_annotations(
    source: &tinox_parser::SourceFile,
    extra_decls: &[tinox_parser::Decl],
) -> Vec<Error> {
    let mut processor = AnnotationProcessor::new();

    // First pass: register all @annotation-class definitions so they are valid in the second pass
    for decl in &source.decls {
        collect_custom_annotation_classes(&decl.node, &mut processor);
    }
    for decl in extra_decls {
        collect_custom_annotation_classes(&decl.node, &mut processor);
    }

    // @JsonSerializable class names, needed by validate_route_params for
    // @PostParam/return-type checks -- imports are already merged into
    // source.decls by this point (typecheck runs after resolve_imports),
    // so this also sees classes defined in other files.
    let mut json_serializable: HashSet<String> = HashSet::new();
    collect_json_serializable_classes(&source.decls, &mut json_serializable);
    collect_json_serializable_classes(extra_decls, &mut json_serializable);

    let mut errors = Vec::new();
    for decl in &source.decls {
        validate_decl(&processor, &decl.node, &json_serializable, &mut errors);
    }
    errors
}

fn collect_json_serializable_classes(decls: &[tinox_parser::Decl], out: &mut HashSet<String>) {
    for decl in decls {
        match &decl.node {
            DeclKind::Class(c) => {
                if c.annotations.iter().any(|a| a.name == "JsonSerializable") {
                    out.insert(c.name.clone());
                }
            }
            DeclKind::Namespace(ns) => collect_json_serializable_classes(&ns.decls, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    fn parse(src: &str) -> tinox_parser::SourceFile {
        let tokens = Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens).parse().unwrap()
    }

    fn proc(src: &str) -> AnnotationProcessingResult {
        process_annotations(&parse(src))
    }

    fn valid(src: &str) -> Vec<Error> {
        validate_annotations(&parse(src), &[])
    }

    // --- validate: unknown annotation ---

    #[test]
    fn test_validate_unknown_annotation_on_fn() {
        let errors = valid("@unknownThing\nfn f() -> Nothing {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("unknown annotation"));
    }

    #[test]
    fn test_validate_known_inline_on_fn_ok() {
        let errors = valid("@inline\nfn f() -> Nothing {}");
        assert!(errors.is_empty(), "errors: {:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    #[test]
    fn test_validate_inline_on_class_err() {
        let errors = valid("@inline\nclass Foo {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }

    #[test]
    fn test_validate_get_on_method_ok() {
        let errors = valid("class Ctrl { @GET\nfn list(@HttpContext ctx: HttpContext) -> HttpContext { return ctx; } }");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    // --- validate: REST parameter binding (@PathParam/@QueryParam/@PostParam/@HttpContext) ---

    #[test]
    fn test_route_param_unannotated_err() {
        // No backward compatibility: a bare, unannotated ctx param is a hard error now.
        let errors = valid("class Ctrl { @GET\nfn list(ctx: HttpContext) -> HttpContext { return ctx; } }");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("needs exactly one of")));
    }

    #[test]
    fn test_route_param_multiple_bindings_err() {
        let errors = valid("class Ctrl { @GET\nfn list(@PathParam @QueryParam id: Int64) -> HttpContext { return HttpContext::new(); } }");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("more than one binding annotation")));
    }

    #[test]
    fn test_route_pathparam_wrong_type_err() {
        let errors = valid("class Ctrl { @GET\nfn list(@PathParam id: Ctx) -> HttpContext { return HttpContext::new(); } }");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("must be String/Int64/Int32/Bool/Float64/Float32")));
    }

    #[test]
    fn test_route_postparam_wrong_type_err() {
        // Person isn't @JsonSerializable here, so @PostParam on it is rejected.
        let errors = valid("class Person { var id: Int64; }\nclass Ctrl { @POST\nfn create(@PostParam p: Person) -> Person { return p; } }");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("must be a class marked @JsonSerializable")));
    }

    #[test]
    fn test_route_postparam_json_serializable_ok() {
        // @annotation class JsonSerializable {} stands in for the real one
        // (tinox.core.json's JsonSerializable.tnx) -- this test harness
        // parses the snippet directly with no import resolution, so a
        // custom annotation must be defined in-snippet to be "known"
        // (see test_validate_custom_annotation_usable_after_registration).
        let errors = valid("@annotation\nclass JsonSerializable {}\n@JsonSerializable\nclass Person { var id: Int64; }\nclass Ctrl { @POST\nfn create(@PostParam p: Person) -> Person { return p; } }");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    #[test]
    fn test_route_return_type_not_serializable_err() {
        let errors = valid("class Ctrl { @GET\nfn list() -> Ctx { return Ctx::new(); } }");
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.message.contains("cannot auto-serialize return type")));
    }

    #[test]
    fn test_route_return_list_of_json_serializable_ok() {
        let errors = valid("@annotation\nclass JsonSerializable {}\n@JsonSerializable\nclass Person { var id: Int64; }\nclass Ctrl { @GET\nfn list() -> List<Person> { return []; } }");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    #[test]
    fn test_validate_get_on_function_err() {
        let errors = valid("@GET\nfn list() -> Nothing {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }

    #[test]
    fn test_validate_path_missing_arg_err() {
        // @Path requires 1 arg
        let errors = valid("class Ctrl { @Path\nfn list() -> Nothing {} }");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("requires at least"));
    }

    #[test]
    fn test_validate_application_component_on_class_ok() {
        let errors = valid("@ApplicationComponent\nclass Svc {}");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_inject_on_field_ok() {
        let errors = valid("class A { @Inject\nvar svc: SomeService; }");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_inject_on_method_err() {
        let errors = valid("class A { @Inject\nfn doThing() -> Nothing {} }");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }

    #[test]
    fn test_validate_deprecated_on_fn_ok() {
        let errors = valid("@deprecated\nfn old() -> Nothing {}");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_deprecated_on_class_ok() {
        let errors = valid("@deprecated\nclass OldClass {}");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_test_on_method_ok() {
        let errors = valid("class Suite { @Test(\"should pass\")\nfn myTest() -> Nothing {} }");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_command_on_class_ok() {
        let errors = valid("@Command(\"build\", \"build the project\")\nclass BuildCmd {}");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_custom_annotation_class_self_valid() {
        // @annotation marks a class as custom annotation — it should be valid on itself
        let errors = valid("@annotation\nclass MyAnn {}");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_custom_annotation_usable_after_registration() {
        // After @annotation class MyAnn, using @MyAnn should not produce "unknown annotation"
        let errors = valid("@annotation\nclass MyAnn {}\n@MyAnn\nfn f() -> Nothing {}");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    // --- process: routes ---

    #[test]
    fn test_process_get_route() {
        let result = proc(r#"
@Path("/users")
class UserCtrl {
    @GET
    fn list() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries.len(), 1);
        let r = &result.route_entries[0];
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/users");
        assert_eq!(r.class_name, "UserCtrl");
        assert_eq!(r.method_name, "list");
    }

    #[test]
    fn test_process_post_route_with_method_path() {
        let result = proc(r#"
@Path("/api")
class Ctrl {
    @POST
    @Path("/items")
    fn create() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries.len(), 1);
        assert_eq!(result.route_entries[0].method, "POST");
        assert_eq!(result.route_entries[0].path, "/api/items");
    }

    #[test]
    fn test_process_status_code() {
        let result = proc(r#"
class Ctrl {
    @POST
    @StatusCode(201)
    fn create() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries[0].status_code, Some(201));
    }

    #[test]
    fn test_process_produces_consumes() {
        let result = proc(r#"
class Ctrl {
    @GET
    @Produces("application/json")
    @Consumes("application/json")
    fn get() -> Nothing {}
}
"#);
        let r = &result.route_entries[0];
        assert_eq!(r.produces.as_deref(), Some("application/json"));
        assert_eq!(r.consumes.as_deref(), Some("application/json"));
    }

    #[test]
    fn test_process_no_routes_when_no_http_annotation() {
        let result = proc("class Svc { fn doWork() -> Nothing {} }");
        assert!(result.route_entries.is_empty());
    }

    #[test]
    fn test_process_multiple_routes() {
        let result = proc(r#"
class Ctrl {
    @GET
    fn list() -> Nothing {}
    @POST
    fn create() -> Nothing {}
    @DELETE
    fn delete() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries.len(), 3);
    }

    // --- process: class-level auth propagates to methods ---

    #[test]
    fn test_process_class_auth_propagates() {
        let result = proc(r#"
@Auth("bearer")
class Ctrl {
    @GET
    fn list() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries[0].auth_type.as_deref(), Some("bearer"));
    }

    #[test]
    fn test_process_method_auth_overrides_class() {
        let result = proc(r#"
@Auth("bearer")
class Ctrl {
    @GET
    @Auth("basic")
    fn list() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries[0].auth_type.as_deref(), Some("basic"));
    }

    // --- process: @OIDCRolesAllowed ---

    #[test]
    fn test_process_oidc_roles_allowed() {
        let result = proc(r#"
class Ctrl {
    @GET
    @OIDCRolesAllowed(["admin", "api-user"])
    fn list() -> Nothing {}
}
"#);
        assert_eq!(result.route_entries[0].oidc_roles, vec!["admin".to_string(), "api-user".to_string()]);
    }

    #[test]
    fn test_process_no_oidc_roles_allowed_is_empty() {
        let result = proc(r#"
class Ctrl {
    @GET
    fn list() -> Nothing {}
}
"#);
        assert!(result.route_entries[0].oidc_roles.is_empty());
    }

    #[test]
    fn test_validate_oidc_roles_allowed_on_class_err() {
        let errors = valid(r#"@OIDCRolesAllowed(["admin"])
class Ctrl {}"#);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }

    // --- process: inline ---

    #[test]
    fn test_process_inline_function() {
        let result = proc("@inline\nfn fast() -> Nothing {}");
        assert!(result.inline_functions.contains("fast"));
    }

    #[test]
    fn test_process_inline_method() {
        let result = proc("class Util { @inline\nfn compute() -> Nothing {} }");
        assert!(result.inline_methods.contains(&("Util".to_string(), "compute".to_string())));
    }

    // --- process: deprecated ---

    #[test]
    fn test_process_deprecated_function_warning() {
        let result = proc("@deprecated\nfn old() -> Nothing {}");
        assert_eq!(result.deprecated_warnings.len(), 1);
        assert!(result.deprecated_warnings[0].contains("old"));
    }

    #[test]
    fn test_process_deprecated_with_message() {
        let result = proc("@deprecated(\"use newFn\")\nfn old() -> Nothing {}");
        assert!(result.deprecated_warnings[0].contains("use newFn"));
    }

    #[test]
    fn test_process_deprecated_class() {
        let result = proc("@deprecated\nclass OldClass {}");
        assert!(result.deprecated_warnings[0].contains("OldClass"));
    }

    #[test]
    fn test_process_deprecated_method() {
        let result = proc("class Svc { @deprecated\nfn oldMethod() -> Nothing {} }");
        assert!(result.deprecated_warnings[0].contains("oldMethod"));
    }

    // --- process: DI components ---

    #[test]
    fn test_process_application_component() {
        let result = proc("@ApplicationComponent\nclass Repo {}");
        assert_eq!(result.di_components.len(), 1);
        assert_eq!(result.di_components[0].scope, DiScope::Application);
        assert_eq!(result.di_components[0].class_name, "Repo");
    }

    #[test]
    fn test_process_startup_component() {
        let result = proc("@Startup\nclass Initializer {}");
        assert_eq!(result.di_components[0].scope, DiScope::Startup);
    }

    #[test]
    fn test_process_http_request_scoped() {
        let result = proc("@HttpRequestScoped\nclass Handler {}");
        assert_eq!(result.di_components[0].scope, DiScope::HttpRequest);
    }

    #[test]
    fn test_process_inject_fields_collected() {
        let result = proc(r#"
@ApplicationComponent
class UserService {
    @Inject
    var repo: UserRepository;
}
"#);
        let comp = &result.di_components[0];
        assert_eq!(comp.inject_fields.len(), 1);
        assert_eq!(comp.inject_fields[0].field_name, "repo");
        assert_eq!(comp.inject_fields[0].field_type, "UserRepository");
    }

    // --- process: @Log ---

    #[test]
    fn test_process_log_class() {
        let result = proc("@Log\nclass MyService {}");
        assert!(result.log_classes.contains("MyService"));
    }

    // --- process: @Sensitive / @Masked ---

    #[test]
    fn test_process_sensitive_field() {
        let result = proc(r#"
class User {
    var name: String;
    @Sensitive
    var password: String;
}
"#);
        assert_eq!(result.sensitive_fields.len(), 1);
        assert_eq!(result.sensitive_fields[0].class_name, "User");
        assert_eq!(result.sensitive_fields[0].field_name, "password");
        assert!(result.masked_fields.is_empty());
    }

    #[test]
    fn test_process_masked_field() {
        let result = proc(r#"
class User {
    var name: String;
    @Masked
    var email: String;
}
"#);
        assert_eq!(result.masked_fields.len(), 1);
        assert_eq!(result.masked_fields[0].class_name, "User");
        assert_eq!(result.masked_fields[0].field_name, "email");
        assert!(result.sensitive_fields.is_empty());
    }

    #[test]
    fn test_process_sensitive_and_masked_fields() {
        let result = proc(r#"
class Payment {
    var amount: Int64;
    @Sensitive
    var cardNumber: String;
    @Masked
    var email: String;
}
"#);
        assert_eq!(result.sensitive_fields.len(), 1);
        assert_eq!(result.sensitive_fields[0].field_name, "cardNumber");
        assert_eq!(result.masked_fields.len(), 1);
        assert_eq!(result.masked_fields[0].field_name, "email");
    }

    #[test]
    fn test_sensitive_only_on_fields() {
        let errors = valid(r#"
@Sensitive
class Bad {}
"#);
        assert!(!errors.is_empty(), "@Sensitive should not be valid on a class");
    }

    #[test]
    fn test_masked_only_on_fields() {
        let errors = valid(r#"
@Masked
class Bad {}
"#);
        assert!(!errors.is_empty(), "@Masked should not be valid on a class");
    }

    // --- process: @DoNotSerialize ---

    #[test]
    fn test_process_do_not_serialize_field() {
        let result = proc(r#"
class User {
    var name: String;
    @DoNotSerialize
    var internalToken: String;
}
"#);
        assert_eq!(result.do_not_serialize_fields.len(), 1);
        assert_eq!(result.do_not_serialize_fields[0].class_name, "User");
        assert_eq!(result.do_not_serialize_fields[0].field_name, "internalToken");
        assert!(result.sensitive_fields.is_empty());
        assert!(result.masked_fields.is_empty());
    }

    #[test]
    fn test_process_multiple_do_not_serialize_fields() {
        let result = proc(r#"
class Order {
    var id: Int64;
    @DoNotSerialize
    var internalRef: String;
    @DoNotSerialize
    var debugInfo: String;
}
"#);
        assert_eq!(result.do_not_serialize_fields.len(), 2);
        let names: Vec<&str> = result.do_not_serialize_fields.iter().map(|f| f.field_name.as_str()).collect();
        assert!(names.contains(&"internalRef"));
        assert!(names.contains(&"debugInfo"));
    }

    #[test]
    fn test_do_not_serialize_only_on_fields() {
        let errors = valid(r#"
@DoNotSerialize
class Bad {}
"#);
        assert!(!errors.is_empty(), "@DoNotSerialize should not be valid on a class");
    }

    #[test]
    fn test_do_not_serialize_not_valid_on_method() {
        let errors = valid(r#"
class Foo {
    @DoNotSerialize
    fn bar() -> Nothing {}
}
"#);
        assert!(!errors.is_empty(), "@DoNotSerialize should not be valid on a method");
    }

    #[test]
    fn test_do_not_serialize_combined_with_sensitive() {
        let result = proc(r#"
class Payment {
    var amount: Int64;
    @Sensitive
    var cardNumber: String;
    @DoNotSerialize
    var internalId: String;
}
"#);
        assert_eq!(result.sensitive_fields.len(), 1);
        assert_eq!(result.sensitive_fields[0].field_name, "cardNumber");
        assert_eq!(result.do_not_serialize_fields.len(), 1);
        assert_eq!(result.do_not_serialize_fields[0].field_name, "internalId");
    }

    #[test]
    fn test_do_not_serialize_multiple_classes() {
        let result = proc(r#"
class User {
    var name: String;
    @DoNotSerialize
    var sessionToken: String;
}
class Product {
    var title: String;
    @DoNotSerialize
    var warehouseCode: String;
}
"#);
        assert_eq!(result.do_not_serialize_fields.len(), 2);
        let user_fields: Vec<&str> = result.do_not_serialize_fields.iter()
            .filter(|f| f.class_name == "User")
            .map(|f| f.field_name.as_str())
            .collect();
        assert_eq!(user_fields, vec!["sessionToken"]);
        let product_fields: Vec<&str> = result.do_not_serialize_fields.iter()
            .filter(|f| f.class_name == "Product")
            .map(|f| f.field_name.as_str())
            .collect();
        assert_eq!(product_fields, vec!["warehouseCode"]);
    }

    #[test]
    fn test_json_serializable_class_collected() {
        let result = proc(r#"
@annotation
class JsonSerializable {}
@JsonSerializable
class User {
    var id: Int64;
    var name: String;
}
"#);
        assert!(result.json_serializable_classes.contains(&"User".to_string()));
    }

    #[test]
    fn test_json_serializable_with_do_not_serialize() {
        let result = proc(r#"
@annotation
class JsonSerializable {}
@JsonSerializable
class Product {
    var id: Int64;
    var name: String;
    @DoNotSerialize
    var internalCode: String;
}
"#);
        assert!(result.json_serializable_classes.contains(&"Product".to_string()));
        assert_eq!(result.do_not_serialize_fields.len(), 1);
        assert_eq!(result.do_not_serialize_fields[0].field_name, "internalCode");
    }

    #[test]
    fn test_non_json_serializable_class_not_collected() {
        let result = proc(r#"
class Plain {
    var x: Int64;
}
"#);
        assert!(result.json_serializable_classes.is_empty());
    }

    // --- process: @Config ---

    #[test]
    fn test_process_config_field() {
        let result = proc(r#"
class App {
    @Config("server.port")
    var port: Int64;
}
"#);
        assert_eq!(result.config_fields.len(), 1);
        assert_eq!(result.config_fields[0].field_name, "port");
        assert_eq!(result.config_fields[0].config_key, "server.port");
    }

    // --- process: @Test ---

    #[test]
    fn test_process_test_entries() {
        let result = proc(r#"
class MyTests {
    @Test("should add correctly")
    fn testAdd() -> Nothing {}
    @Test("should subtract")
    fn testSub() -> Nothing {}
}
"#);
        assert_eq!(result.test_entries.len(), 2);
        assert_eq!(result.test_entries[0].class_name, "MyTests");
        assert_eq!(result.test_entries[0].description, "should add correctly");
        assert_eq!(result.test_entries[1].method_name, "testSub");
    }

    #[test]
    fn test_process_test_uses_method_name_when_no_description() {
        let result = proc("class T { @Test\nfn myTest() -> Nothing {} }");
        assert_eq!(result.test_entries[0].description, "myTest");
    }

    // --- process: @Command / @Option / @Argument ---

    #[test]
    fn test_process_command_basic() {
        let result = proc("@Command(\"build\", \"build the project\")\nclass BuildCmd {}");
        assert_eq!(result.cli_commands.len(), 1);
        let cmd = &result.cli_commands[0];
        assert_eq!(cmd.cmd_name, "build");
        assert_eq!(cmd.description, "build the project");
        assert!(cmd.version.is_none());
    }

    #[test]
    fn test_process_command_with_version() {
        let result = proc("@Command(\"app\", \"desc\", \"1.0.0\")\nclass App {}");
        assert_eq!(result.cli_commands[0].version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn test_process_command_with_options() {
        let result = proc(r#"
@Command("greet", "greet someone")
class GreetCmd {
    @Option("--name,-n", "recipient name")
    var name: String;
}
"#);
        let cmd = &result.cli_commands[0];
        assert_eq!(cmd.options.len(), 1);
        assert_eq!(cmd.options[0].field_name, "name");
        assert!(cmd.options[0].names.contains(&"--name".to_string()));
        assert!(cmd.options[0].names.contains(&"-n".to_string()));
    }

    #[test]
    fn test_process_command_with_required_option() {
        let result = proc(r#"
@Command("run", "run it")
class RunCmd {
    @Option("--file,-f", "file path", true)
    var file: String;
}
"#);
        assert!(result.cli_commands[0].options[0].required);
    }

    #[test]
    fn test_process_command_with_argument() {
        let result = proc(r#"
@Command("convert", "convert file")
class ConvertCmd {
    @Argument(0, "input file")
    var input: String;
}
"#);
        let cmd = &result.cli_commands[0];
        assert_eq!(cmd.arguments.len(), 1);
        assert_eq!(cmd.arguments[0].index, 0);
        assert_eq!(cmd.arguments[0].field_name, "input");
    }

    // --- process: namespace ---

    #[test]
    fn test_process_annotations_in_namespace() {
        let result = proc(r#"
namespace web {
    class Ctrl {
        @GET
        fn list() -> Nothing {}
    }
}
"#);
        assert_eq!(result.route_entries.len(), 1);
        assert_eq!(result.route_entries[0].method, "GET");
    }

    // --- process: @WebsocketEndpoint / @OnOpen / @OnMessage / @OnClose ---

    #[test]
    fn test_process_ws_endpoint_full() {
        let result = proc(r#"
@WebsocketEndpoint("/echo", 8790)
class EchoEndpoint {
    @OnOpen
    fn onOpen(conn: Int64) -> Nothing {}
    @OnMessage
    fn onMessage(conn: Int64, msg: String) -> Nothing {}
    @OnClose
    fn onClose(conn: Int64) -> Nothing {}
}
"#);
        assert_eq!(result.ws_endpoints.len(), 1);
        let ep = &result.ws_endpoints[0];
        assert_eq!(ep.class_name, "EchoEndpoint");
        assert_eq!(ep.path, "/echo");
        assert_eq!(ep.port, Some(8790));
        assert_eq!(ep.on_open.as_deref(), Some("onOpen"));
        assert_eq!(ep.on_message.as_deref(), Some("onMessage"));
        assert_eq!(ep.on_close.as_deref(), Some("onClose"));
    }

    #[test]
    fn test_process_ws_endpoint_default_port() {
        let result = proc("@WebsocketEndpoint(\"/chat\")\nclass ChatEndpoint { @OnMessage\nfn handle(conn: Int64, msg: String) -> Nothing {} }");
        assert_eq!(result.ws_endpoints[0].port, None);
    }

    #[test]
    fn test_process_ws_endpoint_only_on_message() {
        let result = proc(r#"
@WebsocketEndpoint("/chat")
class ChatEndpoint {
    @OnMessage
    fn handle(conn: Int64, msg: String) -> Nothing {}
}
"#);
        let ep = &result.ws_endpoints[0];
        assert!(ep.on_open.is_none());
        assert_eq!(ep.on_message.as_deref(), Some("handle"));
        assert!(ep.on_close.is_none());
    }

    #[test]
    fn test_process_no_ws_endpoint_without_annotation() {
        let result = proc("class Plain { fn onMessage(conn: Int64, msg: String) -> Nothing {} }");
        assert!(result.ws_endpoints.is_empty());
    }

    #[test]
    fn test_process_http3_rest_controller_full() {
        let result = proc(r#"
@Http3RestController(8843, "cert.pem", "key.pem")
class TaskController {
    @GET
    @Path("/tasks")
    fn listTasks(ctx: HttpContext) -> Nothing {}
}
"#);
        assert_eq!(result.http3_rest_controllers.len(), 1);
        let c = &result.http3_rest_controllers[0];
        assert_eq!(c.class_name, "TaskController");
        assert_eq!(c.port, 8843);
        assert_eq!(c.cert_path, "cert.pem");
        assert_eq!(c.key_path, "key.pem");
        // Method-level @GET/@Path collection is unaffected by the new
        // class annotation -- same route_entries as plain TCP REST.
        assert_eq!(result.route_entries.len(), 1);
        assert_eq!(result.route_entries[0].path, "/tasks");
    }

    #[test]
    fn test_process_no_http3_rest_controller_without_annotation() {
        let result = proc("class Plain { @GET\n@Path(\"/x\")\nfn f(ctx: HttpContext) -> Nothing {} }");
        assert!(result.http3_rest_controllers.is_empty());
    }

    #[test]
    fn test_validate_http3_rest_controller_on_class_ok() {
        let errors = valid("@Http3RestController(8843, \"cert.pem\", \"key.pem\")\nclass C {}");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    #[test]
    fn test_validate_http3_rest_controller_missing_args_err() {
        let errors = valid("@Http3RestController(8843)\nclass C {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("requires at least"));
    }

    #[test]
    fn test_validate_server_endpoint_on_class_ok() {
        let errors = valid("@WebsocketEndpoint(\"/x\")\nclass E {}");
        assert!(errors.is_empty(), "{:?}", errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    #[test]
    fn test_validate_server_endpoint_missing_arg_err() {
        let errors = valid("@WebsocketEndpoint\nclass E {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("requires at least"));
    }

    #[test]
    fn test_validate_on_message_on_function_err() {
        let errors = valid("@OnMessage\nfn f() -> Nothing {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }

    // --- custom annotation ---

    #[test]
    fn test_process_custom_annotation_registered() {
        let result = proc("@annotation\nclass MyAnn {}");
        assert!(result.custom_annotation_names.contains(&"MyAnn".to_string()));
    }

    // --- @Transactional ---

    #[test]
    fn test_process_transactional_on_method() {
        let result = proc(r#"
class Svc {
    @Transactional
    fn transfer() -> Nothing {}
    fn readOnly() -> Nothing {}
}
"#);
        assert!(result.transactional_methods.contains(&("Svc".to_string(), "transfer".to_string())));
        assert!(!result.transactional_methods.contains(&("Svc".to_string(), "readOnly".to_string())));
    }

    #[test]
    fn test_process_transactional_on_class_applies_to_every_method() {
        let result = proc(r#"
@Transactional
class Svc {
    fn a() -> Nothing {}
    fn b() -> Nothing {}
}
"#);
        assert!(result.transactional_methods.contains(&("Svc".to_string(), "a".to_string())));
        assert!(result.transactional_methods.contains(&("Svc".to_string(), "b".to_string())));
    }

    #[test]
    fn test_validate_transactional_on_class_ok() {
        let errors = valid("@Transactional\nclass Svc { fn a() -> Nothing {} }");
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_transactional_on_function_err() {
        let errors = valid("@Transactional\nfn f() -> Nothing {}");
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("cannot be applied"));
    }
}
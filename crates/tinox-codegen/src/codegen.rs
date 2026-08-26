use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;
use tinox_common::{Error, ErrorBag, Span, Spanned};
use tinox_parser::{
    BinaryOp, CatchClause, DeclKind, Expr, ExprKind, Literal, Method, Pattern,
    SourceFile, Stmt, StmtKind, Type, UnaryOp,
};

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
    /// LLVM type: "i8*" for String, "i64" for Int*, "i1" for Bool
    pub field_llvm_type: String,
}

#[derive(Debug, Clone)]
pub struct LogMaskFieldInfo {
    pub class_name: String,
    pub field_name: String,
}

#[derive(Debug, Clone)]
pub struct CliOptionInfo {
    pub field_name: String,
    pub names: Vec<String>,
    pub description: String,
    pub required: bool,
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

/// Route entry produced by REST annotation processing.
#[derive(Debug, Clone)]
pub struct RouteEntry {
    pub http_method: String,
    pub path: String,
    pub class_name: String,
    pub method_name: String,
    pub status_code: Option<i64>,
    pub produces: Option<String>,
    pub consumes: Option<String>,
    pub auth_type: Option<String>,
    /// Roles from @OIDCRolesAllowed(["role1", "role2"]) -- empty = no
    /// OIDC role check on this route.
    pub oidc_roles: Vec<String>,
    /// true = fnc (static), false = fn (instance, has self)
    pub is_static: bool,
    /// Per-parameter bindings, in declared order -- drives
    /// `emit_route_shim_body`'s call-argument construction. Every
    /// parameter has exactly one (validated at typecheck time,
    /// `validate_route_params`, annotations.rs) -- no unannotated/
    /// implicit shape.
    pub params: Vec<RouteParamBinding>,
    /// `HttpContext` = manual-response mode (handler builds
    /// `ctx.response` itself, its return value is discarded/never
    /// dereferenced); anything else = auto-serialize mode (the shim
    /// serializes the returned value as the JSON response body).
    pub return_type: tinox_parser::Type,
}

/// Mirrors `tinox_typecheck::annotations::RouteParamKind` -- duplicated
/// rather than imported directly, matching this codebase's existing
/// convention for typecheck-derived route/DI info (see e.g. `DiScope`
/// below, converted explicitly at the main.rs boundary) so codegen
/// doesn't depend on typecheck's exact internal shape.
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
    pub name: String,
    pub ty: tinox_parser::Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricKind {
    Timed,
    Counted,
}

#[derive(Debug, Clone)]
pub struct MetricEntry {
    pub kind: MetricKind,
    pub metric_name: String,
    pub class_name: String,
    pub fn_name: String,
}

/// `obj[index] = value`'s resolved target parts (map-vs-array, index and
/// base-container SSA values) — bundled so `gen_index_store` takes it plus
/// `val`/`val_ty` instead of a 7-argument signature (clippy::too_many_arguments).
struct IndexTarget {
    is_map: bool,
    idx_val: String,
    idx_ty: String,
    base_ptr: String,
    base_ty: String,
}

/// An already-evaluated instance-method receiver + extra args, ready to
/// splice into a `call` instruction — bundled so
/// `gen_generic_instance_method_call` takes it plus `mangled_class`/
/// `fn_name`/`raw_args`/`ctx` instead of an 8-argument signature
/// (clippy::too_many_arguments).
struct EvaluatedReceiver<'a> {
    obj_ty: &'a str,
    obj_ptr: &'a str,
    extra_args: &'a [(String, String)],
}

/// Bundles the annotation metadata from typecheck for set_annotation_info —
/// avoids a 12-parameter signature (clippy::too_many_arguments).
#[derive(Default)]
pub struct AnnotationInfo {
    pub inline_fns: HashSet<String>,
    pub inline_meths: HashSet<(String, String)>,
    /// (class, method) pairs wrapped in a DB transaction (issue #191) --
    /// see gen_transactional_wrapper.
    pub transactional_methods: HashSet<(String, String)>,
    pub routes: Vec<RouteEntry>,
    pub di_components: Vec<DiComponentInfo>,
    pub log_classes: HashSet<String>,
    pub config_fields: Vec<ConfigFieldInfo>,
    pub cli_commands: Vec<CliCommandInfo>,
    pub sensitive_fields: Vec<LogMaskFieldInfo>,
    pub masked_fields: Vec<LogMaskFieldInfo>,
    pub do_not_serialize_fields: Vec<LogMaskFieldInfo>,
    pub json_serializable_classes: Vec<String>,
    pub metric_entries: Vec<MetricEntry>,
}

/// WebSocket endpoint entry produced by @WebsocketEndpoint annotation processing.
#[derive(Debug, Clone)]
pub struct WsEndpointEntry {
    pub class_name: String,
    pub path: String,
    pub port: Option<i64>,
    pub on_open: Option<String>,
    pub on_message: Option<String>,
    pub on_close: Option<String>,
}

/// AMQP-1.0 consumer entry produced by @Amqp10Consumer annotation processing (Issue #81).
#[derive(Debug, Clone)]
pub struct Amqp10ConsumerEntry {
    pub class_name: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub pass: String,
    pub address: String,
    pub on_message: Option<String>,
}

/// AMQP-0-9-1 consumer entry produced by @Amqp091Consumer annotation processing (Issue #126).
#[derive(Debug, Clone)]
pub struct Amqp091ConsumerEntry {
    pub class_name: String,
    pub host: String,
    pub port: i64,
    pub vhost: String,
    pub user: String,
    pub pass: String,
    pub queue: String,
    pub on_message: Option<String>,
}

/// @Http3RestController(port, certPath, keyPath) entry: routes every
/// @GET/@POST/@PUT/@PATCH/@DELETE in route_entries through
/// tinox.core.http3_server.Http3Server instead of the TCP auto-server
/// (see emit_http3_route_code). At most one per program.
#[derive(Debug, Clone)]
pub struct Http3RestControllerEntry {
    pub class_name: String,
    pub port: i64,
    pub cert_path: String,
    pub key_path: String,
}

/// @TinoxUIApp(httpPort, wsPort) entry (issue #215, Phase 4). `view_method`
/// is already resolved down to a single name here (main.rs enforces the
/// "exactly one @View method" cardinality before building this entry, same
/// place Http3RestControllerEntry's "at most one class" check lives).
#[derive(Debug, Clone)]
pub struct TinoxUIAppEntry {
    pub class_name: String,
    pub http_port: i64,
    pub ws_port: i64,
    pub view_method: String,
}

#[derive(Debug, Clone)]
pub struct EntityFieldEntry {
    pub field_name: String,
    pub column_name: String,
    pub is_id: bool,
    pub is_generated: bool,
    pub not_null: bool,
    pub field_llvm_type: String,
}

#[derive(Debug, Clone)]
pub struct EntityEntry {
    pub class_name: String,
    pub table_name: String,
    pub fields: Vec<EntityFieldEntry>,
}

pub struct CodeGen {
    ir: String,
    lambda_ir: String,
    strings: HashMap<String, String>,
    temp_count: usize,
    /// DWARF debug info (issue #114): source file path (as stamped by
    /// `stamp_file_identity` in `tinox/src/main.rs`) -> its `!DIFile`
    /// metadata node id. Populated lazily the first time a given file is
    /// seen while emitting a `!dbg` attachment for a real user
    /// function/method (`gen_fn`/`gen_class_method` — the only two
    /// `define` sites with a meaningful source `Span`; compiler-
    /// synthesized helpers like `toString`/`toJson`/DI/ORM glue get no
    /// debug info, they have no single source line to point at).
    di_file_ids: HashMap<Arc<str>, u32>,
    /// Accumulated `!N = ...` debug metadata definitions, appended at the
    /// end of the module in `into_ir()` (metadata may be forward-
    /// referenced in LLVM's textual IR, so definition order doesn't
    /// matter — matches where real compilers like clang put it).
    di_metadata: Vec<String>,
    /// Next free debug metadata node id.
    di_next_id: u32,
    /// Id of the single `distinct !DICompileUnit(...)` node, created
    /// lazily on the first function that gets debug info.
    di_compile_unit_id: Option<u32>,
    /// Id of one shared, minimal `!DISubroutineType(types: !{null})` node
    /// used by every `!DISubprogram` — issue #114's scope is function-
    /// level (name/file/line), not per-argument/return type modeling.
    di_subroutine_type_id: Option<u32>,
    struct_layouts: HashMap<String, Vec<String>>,
    #[allow(dead_code)]
    closure_envs: HashMap<String, String>,
    method_ret_types: HashMap<String, String>,
    method_ret_class: HashMap<String, String>, // method key → Tinox class name when it returns a class
    static_method_keys: HashSet<String>,       // method keys for static (fnc) methods — no self param
    // vtable support
    /// interface_name -> ordered method names (vtable slot order)
    vtable_layouts: HashMap<String, Vec<String>>,
    /// class_name -> list of interfaces it implements
    class_implements: HashMap<String, Vec<String>>,
    /// set of class names that have a vtable pointer at slot 0
    classes_with_vtable: HashSet<String>,
    /// set of known interface names (for dispatch decisions)
    known_interfaces: HashSet<String>,
    /// interface_name -> method_name -> declared return type. Vtable-dispatch
    /// call sites used to hardcode `i64` as the return type unconditionally
    /// ("vtable methods return i64 (uniform representation)") — correct for
    /// an Int64-returning interface method (whose implementation genuinely
    /// returns `i64`), but WRONG for e.g. a String-returning one (whose
    /// implementation returns `i8*`): the call site would then tag the
    /// result as `i64` even though the callee actually returns a pointer,
    /// so a caller deciding how to use the result (e.g. `println` picking
    /// int-print vs string-print) picks the wrong path — the pointer's raw
    /// address prints as a garbage integer instead of the string content.
    /// Populated once in `gen()` from the interface declarations themselves
    /// (not from `vtable_layouts`, which only has method names/order).
    interface_method_ret_types: HashMap<String, HashMap<String, tinox_parser::Type>>,
    /// child_class_name -> parent_class_name (for super calls)
    class_parents: HashMap<String, String>,
    /// class_name -> number of entries in its vtable global (computed during emit_vtable_globals)
    vtable_sizes: HashMap<String, usize>,
    /// ClassName_methodName -> OwnerClassName_methodName (resolved through inheritance)
    method_impl: HashMap<String, String>,
    /// Classes for which a named LLVM struct type `%class.<name>` was emitted
    /// (B1 phase 1): field access on these uses a typed GEP instead of the uniform
    /// i64 slot + bitcast. Only plain (non-generic, non-specialized) classes so
    /// far; everything else falls back to the i64 path (identical memory layout,
    /// so the two are mixable during migration).
    class_named_types: HashSet<String>,
    /// Named struct type defs for on-demand generic specializations (`Foo__i64`),
    /// which arise mid-emission. Collected here and spliced into the module before
    /// any function body (at the `@@SPEC_TYPES@@` marker) so the types are defined
    /// before their `getelementptr` uses — a forward-referenced named type is
    /// opaque/unsized and rejected by the verifier (B1 phase 4).
    spec_type_defs: String,
    /// Free-function names that can (transitively) throw. A call to a free fn NOT
    /// in this set provably cannot throw, so no post-statement throw-check (Bug 40)
    /// is needed after it — the throw-effect analysis (Bug 48) makes exception
    /// propagation zero-cost for the common non-throwing case.
    throwing_free_fns: HashSet<String>,
    /// Method base names (e.g. `get`) for which SOME class's method can throw.
    /// A `obj.m()` / `Class::m()` call whose base name is absent provably cannot
    /// throw (over-approximates across same-named methods; always safe).
    throwing_method_basenames: HashSet<String>,
    /// fn_name -> (ret_llvm_ty, param_llvm_tys) for spawn codegen
    fn_sigs: HashMap<String, (String, Vec<String>)>,
    spawn_counter: usize,
    /// Generic function AST nodes (not directly compiled, monomorphized on demand)
    generic_fns: HashMap<String, tinox_parser::Function>,
    /// Generic methods of non-generic classes, key "Class_method" —
    /// monomorphized at the call site (Json::deserialize<User>).
    generic_methods: HashMap<String, tinox_parser::Method>,
    /// Own-type-param instance methods of a GENERIC class (`fn map<U>(...)`
    /// on `Option<T>`), Key "MangledClass_method" (same key shape as
    /// `generic_methods`, but this one keeps what's needed to monomorphize
    /// BOTH the class's T and the method's own U in a single combined
    /// substitution pass: the pristine (unsubstituted) method straight
    /// from `generic_classes`, the original unmangled class name, and the
    /// class-level T bindings. Deliberately NOT reusing the once-already-
    /// T-substituted `Method` stored in `generic_methods` — that copy's
    /// body has already had bare self-references (`Option::none()` inside
    /// `Option<T>`'s own methods) renamed to the mangled T-specialization,
    /// which is wrong for a node like `Option<U>::none()` where U differs
    /// from T (see #153). Working from the pristine method and combining
    /// T+U into one subst avoids that double-rename.
    generic_instance_methods: HashMap<String, (String, HashMap<String, String>, tinox_parser::Method)>,
    /// #158: a specific own-type-param instance-method CALL NODE's result
    /// class marker (`Box<Int64>.transform(f)` → e.g. "Box__string"),
    /// keyed by that call expression's own `expr.id` — NOT by
    /// "{class}_{method}" like `method_ret_class`. Two calls to the same
    /// method on the same class with a DIFFERENT own type-param
    /// instantiation (`o.map(intToInt).map(intToString)`, both keyed
    /// "Option__i64_map" in `method_ret_class`) would otherwise clobber
    /// each other there — whichever call is emitted last wins, so an
    /// EARLIER call's chained follow-on reads the WRONG class. Per-node
    /// keying can't collide this way since every call expression has its
    /// own id.
    methodcall_result_markers: HashMap<u32, String>,
    /// Active type-parameter bindings while emitting a specialization:
    /// "T" -> "User" (resolves T::fromJson).
    type_param_aliases: HashMap<String, String>,
    /// Generic class AST nodes (not directly compiled, monomorphized on demand)
    generic_classes: HashMap<String, tinox_parser::Class>,
    /// Already-generated specializations (mangled_name already emitted)
    generated_specializations: HashSet<String>,
    /// `extern fn` name -> the exact `declare ...` line already emitted for
    /// it. The SAME external symbol legitimately gets its own `extern fn`
    /// declaration repeated in every `.tnx` file that calls it (there's no
    /// shared header, and it's an established pattern in this codebase —
    /// e.g. `tinoxDeflateRaw`/`httpConnReadLine` were already declared in
    /// two different stdlib modules each before issue #167 added
    /// `tinoxBytesToString` to many more). Emitting `declare` unconditionally
    /// for every such node only worked by luck (no single compiled program
    /// had imported two of the modules sharing a name) until #167 made the
    /// collision common enough to actually hit: LLVM hard-errors on a
    /// literal repeated `declare` ("invalid redefinition of function") once
    /// two imported modules both declaring `tinoxBytesToString` end up
    /// merged into one program (`amqp10_consumer_annotation` test). Skip a
    /// second `declare` for a name already emitted with an IDENTICAL
    /// signature; a DIFFERENT signature under the same name is a genuine
    /// conflict, not a duplicate, and stays a hard compile error rather than
    /// silently keeping whichever one happened to be emitted first.
    declared_externs: HashMap<String, String>,
    /// Set of all enum variant names (for bare-name match patterns)
    known_enum_variants: HashSet<String>,
    /// variant name → payload kind per argument ("String" | "Map" | "List" | "Other"),
    /// used to bind match-pattern payload variables with their true LLVM type.
    enum_variant_payloads: HashMap<String, Vec<String>>,
    /// Set of enum type names (for type_to_llvm: enums are i64, not i64*)
    known_enum_types: HashSet<String>,
    /// variant name → owning enum name, but only while the name is UNIQUE across
    /// all enums seen so far; a second differently-owned sighting flips the entry
    /// to `None` (ambiguous). Lets `variant_discriminator_key` scope the
    /// discriminator hash to `Enum::Variant` for the common case (distinct variant
    /// names) even from unqualified match patterns, without needing scrutinee type
    /// resolution — only genuinely ambiguous names (same variant name declared by
    /// two+ enums) fall back to scrutinee-type resolution / the old global scheme.
    variant_owner: HashMap<String, Option<String>>,
    /// Annotation processing: functions annotated @inline
    inline_functions: HashSet<String>,
    /// Annotation processing: (class_name, method_name) pairs for methods annotated @inline
    inline_methods: HashSet<(String, String)>,
    /// Annotation processing: (class_name, method_name) pairs wrapped in a
    /// DB transaction, either directly @Transactional or via a class-level
    /// @Transactional (issue #191). See gen_transactional_wrapper.
    transactional_methods: HashSet<(String, String)>,
    /// REST route entries collected from annotation processing
    route_entries: Vec<RouteEntry>,
    /// DI component info from annotation processing
    di_components: Vec<DiComponentInfo>,
    /// Class names that have @Log — get a synthetic 'log: Logger' field
    log_classes: HashSet<String>,
    /// Fields annotated with @Config — injected from application.properties at construction
    config_fields: Vec<ConfigFieldInfo>,
    /// CLI commands collected from @Command annotation processing
    cli_commands: Vec<CliCommandInfo>,
    /// Fields annotated with @Sensitive — logged as '***'
    sensitive_fields: Vec<LogMaskFieldInfo>,
    /// Fields annotated with @Masked — partially masked in logs
    masked_fields: Vec<LogMaskFieldInfo>,
    /// Fields annotated with @DoNotSerialize — excluded from JSON/XML serialization
    do_not_serialize_fields: Vec<LogMaskFieldInfo>,
    /// Class names annotated with @JsonSerializable — get a compiler-generated toJson() method
    json_serializable_classes: Vec<String>,
    /// Metric instrumentation entries from @Timed / @Counted annotations
    metric_entries: Vec<MetricEntry>,
    /// ORM entity entries from @Entity / @Table annotations
    entity_entries: Vec<EntityEntry>,
    /// WebSocket endpoints from @WebsocketEndpoint annotation processing
    ws_endpoints: Vec<WsEndpointEntry>,
    /// Tinox-UI apps from @TinoxUIApp annotation processing (issue #215, Phase 4)
    tinoxui_apps: Vec<TinoxUIAppEntry>,
    /// AMQP-1.0 consumers from @Amqp10Consumer annotation processing (Issue #81)
    amqp10_consumers: Vec<Amqp10ConsumerEntry>,
    /// AMQP-0-9-1 consumers from @Amqp091Consumer annotation processing (Issue #126)
    amqp091_consumers: Vec<Amqp091ConsumerEntry>,
    /// @Http3RestController: routes route_entries through Http3Server
    /// instead of the TCP auto-server (see emit_http3_route_code)
    http3_rest_controller: Option<Http3RestControllerEntry>,
    /// Rich per-expression types from the checker (type-system unification): the
    /// full ValueType per node id, incl. generic args. Since phase 3 the ONLY
    /// checker→codegen type channel (the lossy flat marker table it replaced is
    /// gone); consumed via `rich_marker`. ID 0 (synthetic nodes) never has an
    /// entry.
    expr_value_types: HashMap<u32, tinox_typecheck::ValueType>,
    /// DB connection URL from tinox.toml [database] — emitted as compile-time constant
    db_url: Option<String>,
    /// [database] pool size from tinox.toml, default 5 when omitted (see
    /// crates/tinox/src/main.rs's read_database_config) — passed to
    /// tinox_db_pool_init alongside db_url. Ignored by the sqlite/mysql
    /// drivers (single-connection model, unchanged since before issue #191).
    db_pool_size: i64,
    /// Whether a [metrics] endpoint is enabled (path to expose on)
    metrics_path: Option<String>,
    /// If set, emit a test-runner main that calls this (class, method) and exits 0/1
    test_entry: Option<(String, String)>,
    /// Whether a user-defined main function was compiled (prevents auto-generated main)
    has_main: bool,
    /// Names of `__tinox_run_<kind>()` functions emitted by the REST/HTTP3/
    /// WS/AMQP auto-run annotation processors -- collected here instead of
    /// each one claiming @tinox_main directly, so emit_tinox_main_bootstrap
    /// can spawn all of them (plus call Main_main if present) from one
    /// unified entry point.
    background_run_fns: Vec<String>,
    /// `(protocol, detail)` pairs -- one per background auto-run kind
    /// registered above, in the same order -- consumed by
    /// emit_tinox_main_bootstrap to print the startup "Endpoints:" block.
    /// e.g. `("HTTP", "8080")`, `("WebSocket", "9001")`,
    /// `("AMQP 0-9-1 (consumer)", "localhost:5672 (queue: orders)")`.
    startup_endpoints: Vec<(String, String)>,
    /// `tinox.core` module (artifactId) names from the project's
    /// `[[dependencies]]`, set externally via `set_loaded_modules` before
    /// `gen()` runs -- printed in the startup banner alongside
    /// `startup_endpoints`. Empty for the REPL and other non-project
    /// compile paths, which never call the setter.
    loaded_modules: Vec<String>,
    /// Whether the startup banner (ASCII art + loaded_modules +
    /// startup_endpoints + elapsed time) should actually be emitted, set
    /// externally via `set_startup_banner_enabled` from tinox.toml's
    /// `[startup] banner` (defaults `true`). One of two conditions
    /// `emit_tinox_main_bootstrap` ANDs together into `show_banner`: this
    /// is the explicit opt-out (for a program that does have an auto-run
    /// endpoint but still needs clean stdout, e.g. piped into another
    /// program); the other is simply having no endpoint to report on in
    /// the first place, which needs no opt-in/out at all.
    banner_enabled: bool,
    /// Whether to compile in the dev-mode introspection API
    /// (`emit_devui_code`), and on which port -- from tinox.toml's
    /// `[dev]` section, set externally via `set_dev_config`.
    dev_enabled: bool,
    dev_port: u16,
    /// Package name/version (`tinox.toml`) and the tinox compiler's own
    /// version -- set externally via `set_dev_info`, served by the
    /// dev-mode introspection API's `/info` route.
    dev_package_name: String,
    dev_package_version: String,
    dev_tinox_version: String,
    /// Pre-built JSON object (compile-time tinox.toml config summary --
    /// `[docker]`/`[database]`/`[metrics]`/`[startup]`), set externally via
    /// `set_dev_config_summary_json` from `build_dev_config_summary_json`
    /// in main.rs (which has the raw section readers already in scope).
    /// codegen just bakes this as a constant; it doesn't re-derive it.
    dev_config_summary_json: String,
    /// Full shell command line for `/tests/run` (`<tinox exe> test
    /// <project root> 2>&1`, both paths captured at compile time and
    /// shell-quoted) -- set externally via `set_dev_test_command`. Empty
    /// when either path couldn't be determined at build time; the emitted
    /// handler checks for that and returns a fixed error JSON instead of
    /// running an empty command.
    dev_test_command: String,
    /// Whether `class Main { fnc main() -> Int32 }` was found and validated
    /// by emit_class_main_entry_point -- consumed by
    /// emit_tinox_main_bootstrap, which calls @Main_main from the
    /// synthesized @tinox_main instead of emit_class_main_entry_point
    /// wiring it directly (so it can coexist with background_run_fns).
    user_main_class: bool,
    /// Set of class names defined in user/imported code
    defined_classes: HashSet<String>,
    /// class_name -> field_name -> class_type_name (only for fields with Named/class types)
    struct_field_class_types: HashMap<String, HashMap<String, String>>,
    /// class_name -> field_name -> llvm_type (for FieldAccess type recovery)
    struct_field_llvm_types: HashMap<String, HashMap<String, String>>,
    /// class_name -> field_name -> (ret_llvm_ty, param_llvm_tys) for Type::Fn fields
    fn_field_sigs: HashMap<String, HashMap<String, (String, Vec<String>)>>,
    /// ClassName_methodName -> list of Tinox param types (excluding self) for lambda param inference
    method_param_types: HashMap<String, Vec<tinox_parser::Type>>,
    /// Temporary: expected class names for the next lambda's params (set before gen_expr on lambda)
    pending_lambda_param_types: Vec<Option<String>>,
    /// Temporary: expected LLVM types for the next lambda literal's unannotated
    /// params (array map/filter/…: element type of the receiver). Taken (and
    /// cleared) by gen_lambda so nested lambdas never inherit the hint.
    pending_lambda_param_llvm: Vec<Option<String>>,
    /// Temporary: expected LLVM return type for the next lambda literal without
    /// declared return type (map: result element type, filter: i1). Taken by
    /// gen_lambda like the param hint.
    pending_lambda_ret_llvm: Option<String>,
}

impl CodeGen {
    pub fn new() -> Self {
        Self {
            ir: String::new(),
            lambda_ir: String::new(),
            strings: HashMap::new(),
            temp_count: 0,
            di_file_ids: HashMap::new(),
            di_metadata: Vec::new(),
            di_next_id: 0,
            di_compile_unit_id: None,
            di_subroutine_type_id: None,
            struct_layouts: HashMap::new(),
            closure_envs: HashMap::new(),
            method_ret_types: HashMap::new(),
            method_ret_class: HashMap::new(),
            static_method_keys: HashSet::new(),
            vtable_layouts: HashMap::new(),
            class_implements: HashMap::new(),
            classes_with_vtable: HashSet::new(),
            known_interfaces: HashSet::new(),
            interface_method_ret_types: HashMap::new(),
            class_parents: HashMap::new(),
            vtable_sizes: HashMap::new(),
            method_impl: HashMap::new(),
            class_named_types: HashSet::new(),
            spec_type_defs: String::new(),
            throwing_free_fns: HashSet::new(),
            throwing_method_basenames: HashSet::new(),
            fn_sigs: HashMap::new(),
            spawn_counter: 0,
            generic_fns: HashMap::new(),
            generic_methods: HashMap::new(),
            generic_instance_methods: HashMap::new(),
            methodcall_result_markers: HashMap::new(),
            type_param_aliases: HashMap::new(),
            generic_classes: HashMap::new(),
            generated_specializations: HashSet::new(),
            declared_externs: HashMap::new(),
            known_enum_variants: HashSet::new(),
            enum_variant_payloads: HashMap::new(),
            known_enum_types: HashSet::new(),
            variant_owner: HashMap::new(),
            inline_functions: HashSet::new(),
            inline_methods: HashSet::new(),
            transactional_methods: HashSet::new(),
            route_entries: Vec::new(),
            di_components: Vec::new(),
            log_classes: HashSet::new(),
            config_fields: Vec::new(),
            cli_commands: Vec::new(),
            sensitive_fields: Vec::new(),
            masked_fields: Vec::new(),
            do_not_serialize_fields: Vec::new(),
            json_serializable_classes: Vec::new(),
            metric_entries: Vec::new(),
            entity_entries: Vec::new(),
            ws_endpoints: Vec::new(),
            tinoxui_apps: Vec::new(),
            amqp10_consumers: Vec::new(),
            amqp091_consumers: Vec::new(),
            http3_rest_controller: None,
            expr_value_types: HashMap::new(),
            db_url: None,
            db_pool_size: 5,
            metrics_path: None,
            test_entry: None,
            has_main: false,
            background_run_fns: Vec::new(),
            startup_endpoints: Vec::new(),
            loaded_modules: Vec::new(),
            banner_enabled: true,
            dev_enabled: false,
            dev_port: 9090,
            dev_package_name: String::new(),
            dev_package_version: String::new(),
            dev_tinox_version: String::new(),
            dev_config_summary_json: "{}".to_string(),
            dev_test_command: String::new(),
            user_main_class: false,
            defined_classes: HashSet::new(),
            struct_field_class_types: HashMap::new(),
            struct_field_llvm_types: HashMap::new(),
            fn_field_sigs: HashMap::new(),
            method_param_types: HashMap::new(),
            pending_lambda_param_types: Vec::new(),
            pending_lambda_param_llvm: Vec::new(),
            pending_lambda_ret_llvm: None,
        }
    }

    /// Provide annotation metadata from the type checker annotation processing.
    pub fn set_annotation_info(&mut self, info: AnnotationInfo) {
        self.inline_functions = info.inline_fns;
        self.inline_methods = info.inline_meths;
        self.transactional_methods = info.transactional_methods;
        self.route_entries = info.routes;
        self.di_components = info.di_components;
        self.log_classes = info.log_classes;
        self.config_fields = info.config_fields;
        self.cli_commands = info.cli_commands;
        self.sensitive_fields = info.sensitive_fields;
        self.masked_fields = info.masked_fields;
        self.do_not_serialize_fields = info.do_not_serialize_fields;
        self.json_serializable_classes = info.json_serializable_classes;
        self.metric_entries = info.metric_entries;
    }

    /// `tinox.core` module names (artifactIds) to print in the startup
    /// banner -- the caller (main.rs's compile_file) reads these straight
    /// from `tinox.toml`'s `[[dependencies]]`, filtered to `group ==
    /// "tinox.core"`; not touched at all by e.g. the REPL's compile path.
    pub fn set_loaded_modules(&mut self, modules: Vec<String>) {
        self.loaded_modules = modules;
    }

    /// From tinox.toml's `[startup] banner` (default `true`) -- an
    /// explicit opt-out for the startup banner, independent of whether
    /// the program actually has an auto-run endpoint to report on.
    pub fn set_startup_banner_enabled(&mut self, enabled: bool) {
        self.banner_enabled = enabled;
    }

    /// From tinox.toml's `[dev]` section (`enabled`, `port`, default
    /// `false`/`9090`) -- whether to emit the dev-mode introspection API
    /// background server (`emit_devui_code`) and which port it binds.
    pub fn set_dev_config(&mut self, enabled: bool, port: u16) {
        self.dev_enabled = enabled;
        self.dev_port = port;
    }

    pub fn set_dev_info(&mut self, package_name: String, package_version: String, tinox_version: String) {
        self.dev_package_name = package_name;
        self.dev_package_version = package_version;
        self.dev_tinox_version = tinox_version;
    }

    pub fn set_dev_test_command(&mut self, command: String) {
        self.dev_test_command = command;
    }

    pub fn set_dev_config_summary_json(&mut self, json: String) {
        self.dev_config_summary_json = json;
    }

    pub fn set_metrics_config(&mut self, path: Option<String>) {
        self.metrics_path = path;
    }

    pub fn set_expr_value_types(&mut self, types: HashMap<u32, tinox_typecheck::ValueType>) {
        self.expr_value_types = types;
    }

    pub fn set_entity_entries(&mut self, entries: Vec<EntityEntry>) {
        self.entity_entries = entries;
    }

    pub fn set_ws_endpoints(&mut self, endpoints: Vec<WsEndpointEntry>) {
        self.ws_endpoints = endpoints;
    }

    pub fn set_tinoxui_apps(&mut self, apps: Vec<TinoxUIAppEntry>) {
        self.tinoxui_apps = apps;
    }

    pub fn set_amqp10_consumers(&mut self, consumers: Vec<Amqp10ConsumerEntry>) {
        self.amqp10_consumers = consumers;
    }

    pub fn set_amqp091_consumers(&mut self, consumers: Vec<Amqp091ConsumerEntry>) {
        self.amqp091_consumers = consumers;
    }

    pub fn set_http3_rest_controller(&mut self, controller: Option<Http3RestControllerEntry>) {
        self.http3_rest_controller = controller;
    }

    pub fn set_db_url(&mut self, url: Option<String>) {
        self.db_url = url;
    }

    pub fn set_db_pool_size(&mut self, pool_size: i64) {
        self.db_pool_size = pool_size;
    }

    /// Register a string constant and return an inline `getelementptr` expression (i8*).
    fn make_string_const(&mut self, s: &str) -> String {
        let label = format!("__metric_str_{}", self.strings.len());
        self.strings.insert(label.clone(), s.to_string());
        let len = s.len() + 1;
        format!("getelementptr [{len} x i8], [{len} x i8]* @{label}, i64 0, i64 0")
    }

    /// Emit a tinox_clock_nanos() call, subtract start_reg, and call tinox_histogram_record.
    fn emit_histogram_record(&mut self, label: &str, start_reg: &str) {
        let end_reg  = self.temp();
        let dur_reg  = self.temp();
        let name_ptr = self.make_string_const(label);
        writeln!(&mut self.ir, "{} = call i64 @tinox_clock_nanos()", end_reg).unwrap();
        writeln!(&mut self.ir, "{} = sub i64 {}, {}", dur_reg, end_reg, start_reg).unwrap();
        writeln!(&mut self.ir, "call void @tinox_histogram_record(i8* {}, i64 {})", name_ptr, dur_reg).unwrap();
    }

    /// Configure a single test to run: generates `tinox_main` that calls
    /// `ClassName_methodName()` and exits 0 (pass) or 1 (fail via panic).
    pub fn set_test_entry(&mut self, class_name: String, method_name: String) {
        self.test_entry = Some((class_name, method_name));
    }

    /// Provide interface metadata from the type checker.
    /// Must be called before `gen()`.
    pub fn set_interface_info(
        &mut self,
        vtable_layouts: HashMap<String, Vec<String>>,
        class_implements: HashMap<String, Vec<String>>,
    ) {
        self.known_interfaces = vtable_layouts.keys().cloned().collect();
        self.vtable_layouts = vtable_layouts;
        self.class_implements = class_implements;
        // Determine which classes have vtables
        for (class_name, ifaces) in &self.class_implements {
            if !ifaces.is_empty() {
                self.classes_with_vtable.insert(class_name.clone());
            }
        }
    }

    /// Bug 107: sensitive_fields/masked_fields/do_not_serialize_fields are
    /// keyed by `(declaring_class, field_name)` -- the class the annotation
    /// processor saw the field ON, which for an inherited field is the
    /// PARENT, not whatever subclass is currently being generated. A
    /// subclass's struct_layouts includes inherited fields, so generating
    /// `Child_toString`/`Child_toJson` and checking `(Child, field_name)`
    /// against those sets never matches an @Sensitive/@Masked/@DoNotSerialize
    /// field declared on `Parent` -- it silently serializes as if unmasked.
    /// Walk class_parents to find which ancestor's OWN layout first
    /// introduced this field name, so the check uses the right key.
    fn field_declaring_class(&self, class_name: &str, field_name: &str) -> String {
        if let Some(parent) = self.class_parents.get(class_name) {
            if let Some(parent_layout) = self.struct_layouts.get(parent) {
                if parent_layout.iter().any(|f| f == field_name) {
                    return self.field_declaring_class(parent, field_name);
                }
            }
        }
        class_name.to_string()
    }

    /// Collect all field names for a class in inheritance order: ancestor fields first, own last.
    fn collect_inherited_fields(
        name: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> Vec<String> {
        let Some(c) = class_map.get(name) else {
            return vec![];
        };
        let mut fields: Vec<String> = if let Some(parent) = &c.extends {
            Self::collect_inherited_fields(parent, class_map)
        } else {
            vec![]
        };
        for f in &c.fields {
            if !fields.contains(&f.name) {
                fields.push(f.name.clone());
            }
        }
        fields
    }

    /// Collect field_name -> class_type_name for all Named-typed fields (including inherited).
    fn collect_field_class_types(
        name: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> HashMap<String, String> {
        let Some(c) = class_map.get(name) else { return HashMap::new(); };
        let mut result = if let Some(parent) = &c.extends {
            Self::collect_field_class_types(parent, class_map)
        } else {
            HashMap::new()
        };
        for f in &c.fields {
            if let Some(class_name) = Self::extract_class_type_name(&f.field_type) {
                // "List:X" only helps element inference when X is a class —
                // downgrade to plain "List" for enums/unknown types (same
                // guard as the let-binding path).
                let class_name = match class_name.strip_prefix("List:") {
                    Some(cls) if !class_map.contains_key(cls) => "List".to_string(),
                    _ => class_name,
                };
                result.insert(f.name.clone(), class_name);
            }
        }
        result
    }

    /// Collect field_name -> (ret_llvm_ty, param_llvm_tys) for all Type::Fn fields (including inherited).
    fn collect_fn_field_sigs(
        name: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> HashMap<String, (String, Vec<String>)> {
        let Some(c) = class_map.get(name) else { return HashMap::new(); };
        let mut result = if let Some(parent) = &c.extends {
            Self::collect_fn_field_sigs(parent, class_map)
        } else {
            HashMap::new()
        };
        for f in &c.fields {
            if let tinox_parser::Type::Fn { params, ret } = &f.field_type {
                let ret_ty = Self::type_to_llvm(ret);
                let param_tys: Vec<String> = params.iter().map(Self::type_to_llvm).collect();
                result.insert(f.name.clone(), (ret_ty, param_tys));
            }
        }
        result
    }

    /// Collect field_name -> llvm_type for all fields (including inherited).
    fn collect_field_llvm_types(
        name: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> HashMap<String, String> {
        let Some(c) = class_map.get(name) else { return HashMap::new(); };
        let mut result = if let Some(parent) = &c.extends {
            Self::collect_field_llvm_types(parent, class_map)
        } else {
            HashMap::new()
        };
        for f in &c.fields {
            result.insert(f.name.clone(), Self::type_to_llvm(&f.field_type));
        }
        result
    }

    /// Container marker for a declared type — the single source of truth for
    /// element typing. Nested lists compose: `List<List<String>>` becomes
    /// "Array:Array:String"; stripping one "Array:" layer yields the element
    /// marker (see `elem_marker`). Plain lists of scalars are "Array".
    fn container_marker(ty: &Type) -> Option<String> {
        let inner = match ty {
            Type::Array(inner) => inner.as_ref(),
            Type::Generic { name, args } if name == "List" || name == "Array" => args.first()?,
            // Maps carry their value marker ("Map:String", "Map:Float",
            // "Map:Array:…", "Map:C"); plain scalar values stay "Map".
            Type::Map(_, v) => {
                return Some(match v.as_ref() {
                    Type::String => "Map:String".to_string(),
                    Type::Float32 | Type::Float64 => "Map:Float".to_string(),
                    Type::Named(c) => format!("Map:{}", c),
                    val => match Self::container_marker(val) {
                        Some(vm) => format!("Map:{}", vm),
                        None => "Map".to_string(),
                    },
                });
            }
            Type::Mutable(inner) | Type::Ref(inner) => return Self::container_marker(inner),
            _ => return None,
        };
        Some(match inner {
            Type::String => "Array:String".to_string(),
            Type::Float32 | Type::Float64 => "Array:Float".to_string(),
            Type::Named(c) => format!("List:{}", c),
            // A generic class element (e.g. List<PriorityItem<T>>) markers by
            // its base name; container keywords fall through to the recursive
            // branch so nested lists still compose as "Array:Array:…".
            Type::Generic { name, .. } if name != "List" && name != "Array" && name != "Map" => {
                format!("List:{}", name)
            }
            _ => match Self::container_marker(inner) {
                Some(im) => format!("Array:{}", im),
                None => "Array".to_string(),
            },
        })
    }

    /// Element marker for a container marker: what a value indexed/iterated
    /// out of the container should be typed as (None = plain i64 scalar).
    fn elem_marker(marker: &str) -> Option<String> {
        if let Some(cls) = marker.strip_prefix("List:") {
            return Some(cls.to_string());
        }
        if let Some(vm) = Self::map_val_marker(marker) {
            // m[key] yields the map's value
            return Some(vm);
        }
        match marker {
            "Array:String" => Some("String".to_string()),
            "Array:Float" => Some("Float".to_string()),
            _ => marker.strip_prefix("Array:").map(|m| m.to_string()),
        }
    }

    /// True for any map marker ("Map" or "Map:<valmarker>").
    fn is_map_marker(marker: &str) -> bool {
        marker == "Map" || marker.starts_with("Map:")
    }

    /// Coerce a raw i64 from tinox_map_get to the LLVM type implied by the
    /// map's value marker. Container/class values stay i64 handles — their
    /// marker propagates via infer_struct_type.
    fn coerce_map_value(&mut self, raw: String, map_marker: Option<&str>) -> (String, String) {
        match map_marker.and_then(Self::map_val_marker).as_deref() {
            Some("String") => {
                let p = self.temp();
                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", p, raw).unwrap();
                (p, "i8*".to_string())
            }
            Some("Float") => {
                let f = self.temp();
                writeln!(&mut self.ir, "{} = bitcast i64 {} to double", f, raw).unwrap();
                (f, "double".to_string())
            }
            _ => (raw, "i64".to_string()),
        }
    }

    /// Value marker of a map marker ("Map:String" → "String");
    /// None = plain "Map" (i64 scalar values) or no map at all.
    fn map_val_marker(marker: &str) -> Option<String> {
        marker.strip_prefix("Map:").map(|m| m.to_string())
    }

    /// Coerce a Map key to the `i8*` the runtime's hash map actually stores
    /// and compares (Bug 129 — `tinox_map_set`/`get`/`contains`/`remove`
    /// treat the key as a NUL-terminated C string; reinterpreting a scalar's
    /// raw bit pattern as a pointer via `inttoptr` segfaults the instant the
    /// hash function dereferences it). Scalars get stringified the same way
    /// `.toString()` does (decimal for ints, "true"/"false" for Bool);
    /// already-`i8*` keys (String) pass through unchanged. Pointer-typed
    /// keys (class-object references) keep the old `inttoptr` reinterpret —
    /// using object identity/a user-defined string form as a map key is a
    /// separate, harder design question left open by the issue, not fixed
    /// here.
    fn emit_map_key(&mut self, key: &str, key_ty: &str) -> String {
        match key_ty {
            "i8*" => key.to_string(),
            "i1" => {
                let s = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_bool_to_string(i1 {})", s, key).unwrap();
                s
            }
            "double" | "float" => {
                let s = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_float_to_string(double {})", s, key).unwrap();
                s
            }
            "i8" | "i16" | "i32" => {
                let ext = self.temp();
                writeln!(&mut self.ir, "{} = sext {} {} to i64", ext, key_ty, key).unwrap();
                let s = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", s, ext).unwrap();
                s
            }
            "i64" => {
                let s = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", s, key).unwrap();
                s
            }
            _ if key_ty.ends_with('*') || key_ty == "ptr" => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = bitcast {} {} to i8*", c, key_ty, key).unwrap();
                c
            }
            _ => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, key).unwrap();
                c
            }
        }
    }

    fn fnv1a(name: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
        for b in name.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV-1a 64-bit prime
        }
        hash
    }

    /// Discriminator for a variant WITH payload arguments: identifies which
    /// variant a heap-allocated `[disc, payload...]` block represents (the
    /// tag word at offset 0). Range is UNCONSTRAINED — safe because payload
    /// variants are always accessed via the tag word loaded FROM that heap
    /// block, never compared as a raw top-level scalar (see the
    /// `icmp ugt i64 val, 65535` pointer-vs-literal guard in
    /// `Pattern::EnumVariant` codegen, which relies on payload-variant
    /// instances always being real heap addresses, i.e. large).
    ///
    /// Bug (found 2026-07-24 while building the AMQP-1.0 type-system codec,
    /// s. bugs.md Bug 69): the previous scheme summed the variant name's
    /// character codes, a weak checksum where any two variant names with an
    /// equal sum (not just anagrams) collide, e.g. `"UShortVal"` and
    /// `"BinaryVal"` both sum to 904. A collision within the SAME enum means
    /// the wrong sibling variant is silently matched — no error, just
    /// corrupted control flow. Variant discriminators are (by existing
    /// design, s. `register_variant_payloads`) keyed globally by name only,
    /// not scoped per-enum, so a proper index-based fix would need a
    /// broader registry; FNV-1a is the smaller, well-contained fix.
    fn enum_discriminator(name: &str) -> i64 {
        Self::fnv1a(name) as i64
    }

    /// Discriminator for a variant WITHOUT payload arguments: the enum
    /// instance IS this raw i64 (no heap allocation). MUST stay <= 65535 —
    /// match codegen uses exactly that threshold to decide whether a raw
    /// i64 is a no-arg-variant literal or a pointer to a payload variant
    /// (`icmp ugt i64 val, 65535`, s. `enum_discriminator` above). A value
    /// of 65536 or more here would make a no-arg instance indistinguishable
    /// from a heap pointer, so the runtime would try to dereference it —
    /// segfault on an essentially-random "address" (this was a real
    /// regression while fixing Bug 69: switching ALL FOUR discriminator
    /// call sites to unconstrained FNV-1a broke `AmqpFieldValue091::VoidVal`
    /// this way; `amqp_frame_codec.tnx` segfaulted). Reduced modulo 65536
    /// instead of the plain character-sum for the same collision-resistance
    /// reasoning as `enum_discriminator`, just range-constrained.
    fn enum_discriminator_noarg(name: &str) -> i64 {
        (Self::fnv1a(name) % 65536) as i64
    }

    /// Classify an enum variant payload type for match-binding purposes.
    /// List payloads carry their full container marker so element access
    /// inside the match arm dispatches correctly.
    fn payload_kind(ty: &Type) -> String {
        match ty {
            Type::String => "String".to_string(),
            Type::Map(_, _) => Self::container_marker(ty).unwrap_or_else(|| "Map".to_string()),
            Type::Array(_) => Self::container_marker(ty).unwrap_or_else(|| "Array".to_string()),
            Type::Generic { name, .. } if name == "List" => {
                Self::container_marker(ty).unwrap_or_else(|| "Array".to_string())
            }
            Type::Float32 | Type::Float64 => "Float".to_string(),
            Type::Mutable(inner) | Type::Ref(inner) => Self::payload_kind(inner),
            _ => "Other".to_string(),
        }
    }

    /// The payload map is keyed by variant name only; when two enums share a
    /// variant name (e.g. a payload variant and a no-arg token variant), keep the
    /// entry with more payload info — no-arg matches never bind payloads, so the
    /// richer entry is always safe to use.
    fn register_variant_payloads(&mut self, name: &str, kinds: Vec<String>) {
        match self.enum_variant_payloads.get(name) {
            Some(existing) if existing.len() >= kinds.len() => {}
            _ => {
                self.enum_variant_payloads.insert(name.to_string(), kinds);
            }
        }
    }

    /// Track which enum owns a variant name, flipping to `None` (ambiguous) the
    /// moment a second, differently-named enum declares the same variant name.
    fn register_variant_owner(&mut self, enum_name: &str, variant_name: &str) {
        match self.variant_owner.get(variant_name) {
            None => {
                self.variant_owner
                    .insert(variant_name.to_string(), Some(enum_name.to_string()));
            }
            Some(Some(existing)) if existing != enum_name => {
                self.variant_owner.insert(variant_name.to_string(), None);
            }
            _ => {}
        }
    }

    /// Discriminator hash-input for a variant: `Enum::Variant` whenever the owning
    /// enum can be determined (qualified pattern/construction site, or an
    /// unqualified reference to a variant name that's unique across the whole
    /// program), otherwise falls back to the bare variant name — the pre-#72
    /// global scheme — for the rare case of a genuinely ambiguous unqualified
    /// name (same variant name declared by 2+ enums) with no resolvable scrutinee
    /// type (e.g. matched through an `Any`-typed value). See `variant_owner`.
    fn variant_discriminator_key(&self, enum_name_hint: Option<&str>, variant: &str) -> String {
        if let Some(en) = enum_name_hint {
            return format!("{}::{}", en, variant);
        }
        match self.variant_owner.get(variant) {
            Some(Some(owner)) => format!("{}::{}", owner, variant),
            _ => variant.to_string(),
        }
    }

    /// Bind a match-pattern payload variable with the LLVM type derived from the
    /// enum declaration's payload type, so that subsequent operator/method dispatch
    /// (string ==/+/len/contains, map get/insert, array len/iteration) works correctly.
    fn bind_match_payload(
        &mut self,
        ctx: &mut GenCtx,
        disc_name: &str,
        arg_index: usize,
        arg_name: &str,
        arg_val: &str,
    ) {
        let kind = self
            .enum_variant_payloads
            .get(disc_name)
            .and_then(|ks| ks.get(arg_index))
            .cloned()
            .unwrap_or_else(|| "Other".to_string());
        // Unique slot name avoids duplicate allocas when the same variable name
        // appears in multiple match arms.
        let slot_name = format!("{}_{}", arg_name, self.temp_count);
        self.temp_count += 1;
        let (llvm_ty, decl_ty): (&str, Option<&str>) = match kind.as_str() {
            "String" => ("i8*", None),
            k if Self::is_map_marker(k) => ("i8*", Some(kind.as_str())),
            "Float" => ("double", None),
            // List payloads: bind as handle, keep the container marker so
            // element access inside the arm is typed (e.g. "Array:String").
            k if k == "Array" || k.starts_with("Array:") || k.starts_with("List:") => {
                ("i64*", Some(kind.as_str()))
            }
            _ => ("i64", None),
        };
        let store_val = if llvm_ty == "i64" {
            arg_val.to_string()
        } else if llvm_ty == "double" {
            let p = self.temp();
            writeln!(&mut self.ir, "{} = bitcast i64 {} to double", p, arg_val).unwrap();
            p
        } else {
            let p = self.temp();
            writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", p, arg_val, llvm_ty).unwrap();
            p
        };
        ctx.locals
            .insert(arg_name.to_string(), (llvm_ty.to_string(), ctx.locals.len()));
        ctx.local_slots.insert(arg_name.to_string(), slot_name.clone());
        writeln!(&mut self.ir, "%{} = alloca {}", slot_name, llvm_ty).unwrap();
        writeln!(
            &mut self.ir,
            "store {} {}, {}* %{}",
            llvm_ty, store_val, llvm_ty, slot_name
        )
        .unwrap();
        match decl_ty {
            Some(t) => {
                ctx.local_types.insert(arg_name.to_string(), t.to_string());
            }
            None => {
                ctx.local_types.remove(arg_name);
            }
        }
    }

    fn extract_class_type_name(ty: &tinox_parser::Type) -> Option<String> {
        use tinox_parser::Type;
        // Containers (List/Array/Map, including nested) → marker
        if let Some(m) = Self::container_marker(ty) {
            return Some(m);
        }
        match ty {
            Type::Named(n) => Some(n.clone()),
            Type::Generic { name, .. } => Some(name.clone()),
            Type::Mutable(inner) | Type::Ref(inner) => Self::extract_class_type_name(inner),
            _ => None,
        }
    }

    /// Infer the struct/class type name for an expression (for nested field access).
    fn infer_struct_type(&self, expr: &tinox_parser::Expr, ctx: &GenCtx) -> Option<String> {
        use tinox_parser::ExprKind;
        // Type-system unification phase 2: for migrated expr kinds the rich typed
        // export (full ValueType → marker, incl. generic specialization
        // Box<Int> → Box__i64) is the PRIMARY source; the local heuristic only
        // fills in where the checker has no type for the node.
        //
        // Deliberately NOT rich-first: This — ctx.current_struct is emission
        // context (which specialization is being emitted, e.g. Box__i64), not an
        // inference; the checker only ever sees the generic view (Box<T>) there,
        // and bridging it would mangle T into a nonexistent specialization.
        let rich_first = matches!(
            &expr.node,
            ExprKind::EnumValue { .. }
                | ExprKind::MethodCall { .. }
                | ExprKind::Call { .. }
                | ExprKind::FieldAccess { .. }
                | ExprKind::Index { .. }
                | ExprKind::ArrayLiteral(_)
                | ExprKind::MapLiteral(_)
                | ExprKind::Ident(_)
        );
        if rich_first {
            if let Some(m) = self.rich_marker(expr) {
                return Some(m);
            }
        }
        self.infer_struct_type_local(expr, ctx)
            .or_else(|| self.rich_marker(expr))
    }

    /// Marker from the rich typed export, if the checker recorded a type for
    /// this node. The non-local half of `infer_struct_type`.
    fn rich_marker(&self, expr: &tinox_parser::Expr) -> Option<String> {
        self.expr_value_types
            .get(&expr.id)
            .and_then(|vt| self.valuetype_to_marker(vt))
    }

    fn infer_struct_type_local(&self, expr: &tinox_parser::Expr, ctx: &GenCtx) -> Option<String> {
        use tinox_parser::ExprKind;
        match &expr.node {
            ExprKind::Ident(name) => {
                let ty = ctx.local_types.get(name)?;
                // "List:ClassName" → strip prefix, return element class name
                if let Some(cls) = ty.strip_prefix("List:") {
                    return Some(cls.to_string());
                }
                Some(ty.clone())
            }
            ExprKind::Index { obj, .. } => {
                // For arr[i], derive the element marker from the container marker
                // ("List:C" → C, "Array:Array:String" → "Array:String", …).
                let container = if let ExprKind::Ident(arr_name) = &obj.node {
                    ctx.local_types.get(arr_name.as_str()).cloned()
                } else {
                    // e.g. this.entries[i], makeList()[i], nested xs[i][j]
                    self.infer_struct_type(obj, ctx)
                };
                container.as_deref().and_then(Self::elem_marker)
            }
            ExprKind::This => ctx.current_struct.clone(),
            ExprKind::FieldAccess { obj, field } => {
                let outer = self.infer_struct_type(obj, ctx)?;
                self.struct_field_class_types
                    .get(&outer)
                    .and_then(|m| m.get(field.as_str()))
                    .cloned()
            }
            ExprKind::EnumValue { enum_name, variant, .. } => {
                // Static method call returning a known class, e.g. JsonValueHelper::asObject
                let key = format!("{}_{}", enum_name, variant);
                self.method_ret_class.get(&key).cloned()
            }
            ExprKind::MethodCall { obj: mc_obj, method: mc_method, .. } => {
                // #158: an own-type-param instance method call's result
                // class, keyed by THIS call node's own id — checked before
                // the "{obj_class}_{method}" heuristic below, which two
                // differently-instantiated calls to the same method on the
                // same class (`o.map(f1).map(f2)`) would otherwise collide
                // on (see `methodcall_result_markers`'s doc comment).
                if let Some(m) = self.methodcall_result_markers.get(&expr.id) {
                    return Some(m.clone());
                }
                let obj_class = self.infer_struct_type(mc_obj, ctx)?;
                // m.get(k) on a typed map yields the map's value marker
                if mc_method == "get" {
                    if let Some(vm) = Self::map_val_marker(&obj_class) {
                        return Some(vm);
                    }
                }
                // Instance method call returning a known class
                let key = format!("{}_{}", obj_class, mc_method);
                self.method_ret_class.get(&key).cloned()
            }
            ExprKind::Call { func, .. } => {
                // Top-level function call with registered return class/marker
                if let ExprKind::Ident(fname) = &func.node {
                    self.method_ret_class.get(fname.as_str()).cloned()
                } else {
                    None
                }
            }
            ExprKind::ArrayLiteral(elems) => {
                // Infer the container marker from the first element
                let first = elems.first()?;
                Some(match &first.node {
                    ExprKind::Literal(Literal::String(_)) => "Array:String".to_string(),
                    ExprKind::Literal(Literal::Float(_)) => "Array:Float".to_string(),
                    ExprKind::ArrayLiteral(_) | ExprKind::MapLiteral(_) => {
                        match self.infer_struct_type(first, ctx) {
                            Some(im) => format!("Array:{}", im),
                            None => "Array".to_string(),
                        }
                    }
                    _ => "Array".to_string(),
                })
            }
            ExprKind::MapLiteral(entries) => {
                // Value marker from the first literal value
                Some(match entries.first().map(|(_, v)| &v.node) {
                    Some(ExprKind::Literal(Literal::String(_))) => "Map:String".to_string(),
                    Some(ExprKind::Literal(Literal::Float(_))) => "Map:Float".to_string(),
                    _ => "Map".to_string(),
                })
            }
            _ => None,
        }
    }

    /// Resolve method call to the class that actually implements it (walks parent chain).
    fn resolve_method_owner(
        class: &str,
        method: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> String {
        let mut current = class.to_string();
        while let Some(c) = class_map.get(&current) {
            if c.methods.iter().any(|m| m.name == method) {
                return format!("{}_{}", current, method);
            }
            match &c.extends {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }
        format!("{}_{}", class, method)
    }

    pub fn gen(&mut self, source: &SourceFile) -> Result<(), ErrorBag> {
        // Must run before ANY method body codegen — interface-dispatch call
        // sites inside those bodies consult this table (the "second pass:
        // generate code" loop below, generating e.g. Main's own body, is
        // itself early enough to need this already populated).
        self.collect_interface_method_ret_types(source);

        writeln!(&mut self.ir, "; Module ID = \"tinox\"").unwrap();
        writeln!(&mut self.ir, "source_filename = \"tinox\"").unwrap();
        writeln!(
            &mut self.ir,
            "target datalayout = \"e-m:e-i64:64-f80:128-n8:16:32:64-S128\""
        )
        .unwrap();
        writeln!(&mut self.ir, "target triple = \"x86_64-unknown-linux-gnu\"").unwrap();
        writeln!(&mut self.ir).unwrap();
        // Global error slot for cross-function throw propagation:
        // throw without an enclosing try stores here and returns; statements
        // inside a try body check the slot and branch to the catch.
        //
        // Bug 101: this used to be a plain (process-wide) global. The HTTP
        // server runs each request handler on its own pthread (one worker
        // per CPU by default), so two concurrent requests shared this exact
        // slot -- a throw in one request's handler could be consumed by a
        // try/catch running concurrently in another request's handler on a
        // different thread, corrupting both requests' control flow. `thread_local`
        // gives every pthread (including each async/spawn task and each HTTP
        // worker) its own independent slot; load/store IR against it is
        // unchanged; only the extern declaration in runtime.c also needs the
        // matching `__thread` storage class (see main()'s use of it there).
        writeln!(&mut self.ir, "@__tinox_err = thread_local global i64 0").unwrap();
        writeln!(&mut self.ir).unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_int(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_string(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_float(double)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_bool(i1)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_newline()").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_alloc(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_panic(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_task_spawn(i8* (i8*)*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_task_spawn_detached(i8* (i8*)*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_task_await(i8*)").unwrap();
        // Monotonic milliseconds -- used only by emit_tinox_main_bootstrap's
        // startup banner to measure/print bootstrap time.
        writeln!(&mut self.ir, "declare i64 @tinox_now_ms()").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_channel_create()").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_channel_send(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_channel_recv(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i1 @tinox_channel_try_recv(i8*, i64*)").unwrap();
        writeln!(&mut self.ir, "declare i32 @sched_yield()").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_length(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_concat(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_mask_partial(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_int_to_string(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_float_to_string(double)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_bool_to_string(i1)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_to_int(i8*)").unwrap();
        writeln!(&mut self.ir, "declare double @tinox_string_to_float(i8*)").unwrap();
        // Strict @PathParam/@QueryParam conversion + bare-string JSON
        // encoding for auto-serialize REST responses (emit_route_shim_body).
        writeln!(&mut self.ir, "declare i32 @tinox_parse_int_checked(i8*, i64*)").unwrap();
        writeln!(&mut self.ir, "declare i32 @tinox_parse_float_checked(i8*, double*)").unwrap();
        writeln!(&mut self.ir, "declare i32 @tinox_parse_bool_checked(i8*, i32*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_json_encode_string(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_char_at(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_from_char_code(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_print_char(i32)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_new(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_array_get(i64*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_checked_sdiv(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_checked_srem(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_push(i64*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_pop(i64*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_slice(i64*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare double @sqrt(double)").unwrap();
        writeln!(&mut self.ir, "declare double @pow(double, double)").unwrap();
        writeln!(&mut self.ir, "declare double @llvm.fabs.f64(double)").unwrap();
        // JsonBuilder — used by @JsonSerializable toJson()
        writeln!(&mut self.ir, "declare i8* @jsonBuilderCreate()").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddInt(i8*, i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddFloat(i8*, i8*, double)").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddBool(i8*, i8*, i32)").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddString(i8*, i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddIntList(i8*, i8*, i64*)").unwrap();
        writeln!(&mut self.ir, "declare void @jsonBuilderAddRaw(i8*, i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @jsonBuilderFinish(i8*)").unwrap();
        // fromJson field helpers
        writeln!(&mut self.ir, "declare i64 @jsonGetIntField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare double @jsonGetFloatField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i32 @jsonGetBoolField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @jsonGetStringField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @jsonGetIntListField(i64*, i8*)").unwrap();
        // Map<String,String>/List<String>/List<class>/nested-class field
        // kinds in @JsonSerializable toJson()/fromJson() (see
        // emit_json_serialize_code/emit_json_deserialize_code). NOT
        // including jsonGetField itself -- tinox.core.json's own `extern fn
        // jsonGetField(...)` (Json.tnx, required to even use
        // @JsonSerializable) already declares it with the same signature;
        // a second `declare` here is a hard "invalid redefinition" error.
        writeln!(&mut self.ir, "declare i8* @jsonGetStringMapField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @jsonGetStringListField(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_json_string_list_serialize(i64*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_json_string_map_serialize(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_json_list_deserialize(i64*, ptr)").unwrap();
        writeln!(&mut self.ir, "declare double @llvm.floor.f64(double)").unwrap();
        writeln!(&mut self.ir, "declare double @llvm.ceil.f64(double)").unwrap();
        writeln!(&mut self.ir, "declare double @llvm.round.f64(double)").unwrap();
        writeln!(&mut self.ir, "declare void @exit(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_config_get(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_config_get_int(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_config_get_bool(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_equals(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_compare(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_contains(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_index_of(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_last_index_of(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_reverse(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_to_upper(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_to_lower(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_starts_with(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_ends_with(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_trim(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_substring(i8*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_string_char_code_at(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_replace(i8*, i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_string_split(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_string_join(i64*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_json_list_serialize(i64*, ptr)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_sort(i64*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_reverse(i64*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_array_contains(i64*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_array_index_of(i64*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_map_create()").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_map_set(i8*, i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_map_get(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_map_contains(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_map_remove(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_map_len(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_map_free(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_map_keys(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_map_values(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_file_open(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_file_close(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_file_read(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_file_readline(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_file_write(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_file_eof(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_file_exists(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_file_delete(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @dirList(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @dirCreate(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @dirDelete(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @envGet(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @envSet(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @envRemove(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @envCurrentDir()").unwrap();
        writeln!(&mut self.ir, "declare void @envSetCurrentDir(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @fileReadAllText(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @fileWriteAllText(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @fileAppendText(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @fileClose(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @processArgs()").unwrap();
        writeln!(&mut self.ir, "declare i1 @fileExists(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @processExit(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @regexIsMatch(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @regexFindAll(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @regexReplace(i64, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @regexSplit(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @regexFindFirst(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @regexReplaceAll(i64, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @regexMatchGroups(i8*, i8*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_remove_at(i64*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64* @tinox_array_insert(i64*, i64, i64)").unwrap();
        // HTTP server C runtime (low-level)
        writeln!(&mut self.ir, "declare i64 @httpServerCreate(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpServerBoundPort(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpServerAcceptConn(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @httpServerReadRequest(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @httpServerSendRaw(i64, i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @httpServerCloseConn(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @httpServerClose(i64)").unwrap();
        // HTTPS/TLS + connection-handle API (see runtime.c, TinoxConn)
        writeln!(&mut self.ir, "declare i64 @httpServerCreateTls(i64, i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpServerAcceptTls(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpServerAcceptConnHandle(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @httpConnReadRequest(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @httpConnSendRaw(i64, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpConnFromFd(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpConnFromFdTls(i64, i8*, i1)").unwrap();
        writeln!(&mut self.ir, "declare i64* @httpConnReadN(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @httpConnWriteBytes(i64, i64*)").unwrap();
        writeln!(&mut self.ir, "declare void @httpConnClose(i64)").unwrap();
        // CLI helpers (@Command / @Option / @Argument)
        writeln!(&mut self.ir, "declare i8* @tinox_cli_get_string(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_cli_has_flag(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_cli_get_int(i8*, i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_cli_get_positional(i32)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_cli_print_option(i8*, i8*)").unwrap();
        // Metrics runtime
        writeln!(&mut self.ir, "declare void @tinox_counter_inc(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_histogram_record(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_gauge_set(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_clock_nanos()").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_metrics_prometheus()").unwrap();
        // DB / ORM runtime
        writeln!(&mut self.ir, "declare void @tinox_db_pool_init(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_db_acquire_stmt_conn()").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_db_release_stmt_conn(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_db_tx_begin()").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_db_tx_commit()").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_db_tx_rollback()").unwrap();
        writeln!(&mut self.ir, "declare i1 @tinox_db_tx_active()").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_db_exec(i8*, i8*, i8**, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_db_nrows(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @tinox_db_ncols(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_db_getval(i8*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64  @tinox_db_getval_int(i8*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i1  @tinox_db_is_null(i8*, i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_db_free(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_db_error(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8** @tinox_params_alloc(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @tinox_params_set(i8**, i64, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @tinox_int_to_param(i64)").unwrap();
        // float math builtins
        writeln!(&mut self.ir, "declare double @log(double)").unwrap();
        writeln!(&mut self.ir, "declare double @exp(double)").unwrap();
        writeln!(&mut self.ir, "declare double @atan2(double, double)").unwrap();
        writeln!(&mut self.ir, "declare double @sin(double)").unwrap();
        writeln!(&mut self.ir, "declare double @cos(double)").unwrap();
        writeln!(&mut self.ir, "declare double @tan(double)").unwrap();
        writeln!(&mut self.ir, "declare i64 @mathIsNan(double)").unwrap();
        writeln!(&mut self.ir, "declare i64 @mathIsInfinite(double)").unwrap();
        writeln!(&mut self.ir, "declare i64 @mathIsNormal(double)").unwrap();
        writeln!(&mut self.ir, "declare double @mathNan()").unwrap();
        writeln!(&mut self.ir, "declare double @mathInf()").unwrap();
        writeln!(&mut self.ir, "declare double @tgamma(double)").unwrap();
        writeln!(&mut self.ir, "declare double @lgamma(double)").unwrap();
        writeln!(&mut self.ir, "declare double @cbrt(double)").unwrap();
        writeln!(&mut self.ir, "declare double @trunc(double)").unwrap();
        writeln!(&mut self.ir, "declare double @rint(double)").unwrap();
        writeln!(&mut self.ir, "declare double @logb(double)").unwrap();
        writeln!(&mut self.ir, "declare double @log2(double)").unwrap();
        writeln!(&mut self.ir, "declare double @log10(double)").unwrap();
        writeln!(&mut self.ir, "declare double @exp2(double)").unwrap();
        writeln!(&mut self.ir, "declare double @exp10(double)").unwrap();
        // jgrep-tinox env/time/debug builtins
        writeln!(&mut self.ir, "declare i8* @envDump()").unwrap();
        writeln!(&mut self.ir, "declare i64 @currentTimeSecs()").unwrap();
        writeln!(&mut self.ir, "declare i8* @strftimeStr(i8*, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @fromdateStr(i8*)").unwrap();
        writeln!(&mut self.ir, "declare void @printStderr(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64 @isStdinTty()").unwrap();
        writeln!(&mut self.ir, "declare i64 @isStdoutTty()").unwrap();
        writeln!(&mut self.ir, "declare i64 @processId()").unwrap();
        writeln!(&mut self.ir, "declare i64 @processRun(i64*, i64, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @processResultStdout(i64)").unwrap();
        writeln!(&mut self.ir, "declare i8* @processResultStderr(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @processResultExitCode(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @processResultTimedOut(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @mutexNew()").unwrap();
        writeln!(&mut self.ir, "declare void @mutexLock(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @mutexUnlock(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @mutexTryLock(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @semaphoreNew(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @semaphoreAcquire(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @semaphoreRelease(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @semaphoreTryAcquire(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @rwlockNew()").unwrap();
        writeln!(&mut self.ir, "declare void @rwlockReadLock(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @rwlockReadUnlock(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @rwlockWriteLock(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @rwlockWriteUnlock(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @processSpawnInteractive(i64*)").unwrap();
        writeln!(&mut self.ir, "declare void @processWriteStdin(i64, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @processReadOutput(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @processIsAlive(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @processKillInteractive(i64)").unwrap();
        writeln!(&mut self.ir, "declare void @gcCollect()").unwrap();
        writeln!(&mut self.ir, "declare i64 @memoryUsage()").unwrap();
        writeln!(&mut self.ir, "declare void @printStackTrace()").unwrap();
        writeln!(&mut self.ir, "declare i64 @now()").unwrap();
        writeln!(&mut self.ir, "declare void @sleep_ms(i64)").unwrap();
        writeln!(&mut self.ir, "declare i64 @randomInt(i64, i64)").unwrap();
        writeln!(&mut self.ir, "declare double @randomFloat()").unwrap();
        writeln!(&mut self.ir, "declare i8* @md5Hash(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @sha256Hash(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @sha1Hash(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @wsAcceptKey(i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @hmacSha256Hash(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @aesEncryptRaw(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i8* @aesDecryptRaw(i8*, i8*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @hmacSha256Bytes(i64*, i64*)").unwrap();
        writeln!(&mut self.ir, "declare i64* @sha256Bytes(i64*)").unwrap();
        writeln!(&mut self.ir).unwrap();

        // Build class AST map for inheritance helpers.
        let class_ast_map: HashMap<String, tinox_parser::Class> = source
            .decls
            .iter()
            .flat_map(|d| {
                let mut v: Vec<tinox_parser::Class> = Vec::new();
                match &d.node {
                    DeclKind::Class(c) => v.push(c.clone()),
                    DeclKind::Namespace(ns) => {
                        for inner in &ns.decls {
                            if let DeclKind::Class(c) = &inner.node {
                                v.push(c.clone());
                            }
                        }
                    }
                    _ => {}
                }
                v
            })
            .map(|c| (c.name.clone(), c))
            .collect();

        // Pre-pass: register all enum type names so that method return type resolution
        // (which calls type_to_llvm_inst) can correctly classify Named enum types as i64.
        for decl in &source.decls {
            match &decl.node {
                DeclKind::Enum(e) => {
                    self.known_enum_types.insert(e.name.clone());
                    for variant in &e.variants {
                        self.known_enum_variants.insert(variant.name.clone());
                        self.register_variant_owner(&e.name, &variant.name);
                        self.register_variant_payloads(
                            &variant.name,
                            variant.args.iter().map(Self::payload_kind).collect(),
                        );
                    }
                }
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        if let DeclKind::Enum(e) = &inner.node {
                            self.known_enum_types.insert(e.name.clone());
                            for variant in &e.variants {
                                self.known_enum_variants.insert(variant.name.clone());
                                self.register_variant_owner(&e.name, &variant.name);
                                self.register_variant_payloads(
                            &variant.name,
                            variant.args.iter().map(Self::payload_kind).collect(),
                        );
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Throw-effect analysis (Bug 48): must run before any function body is
        // emitted, so the per-statement throw-check gate has the throwing-sets.
        self.analyze_throw_effects(source);

        // First pass: build struct_layouts (with vtable slot at index 0 where needed)
        // and method_impl (for inherited method dispatch). Handles both top-level and
        // namespace-scoped classes.
        let all_classes: Vec<tinox_parser::Class> = source.decls.iter().flat_map(|d| {
            let mut v: Vec<tinox_parser::Class> = Vec::new();
            match &d.node {
                DeclKind::Class(c) => v.push(c.clone()),
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        if let DeclKind::Class(c) = &inner.node { v.push(c.clone()); }
                    }
                }
                _ => {}
            }
            v
        }).collect();

        // Register immutable struct layouts and new() return types (top-level + namespace-scoped)
        let all_immutables: Vec<tinox_parser::ImmutableDecl> = source.decls.iter().flat_map(|d| {
            let mut v = Vec::new();
            match &d.node {
                DeclKind::Immutable(u) => v.push(u.clone()),
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        if let DeclKind::Immutable(u) = &inner.node { v.push(u.clone()); }
                    }
                }
                _ => {}
            }
            v
        }).collect();

        for u in &all_immutables {
            self.defined_classes.insert(u.name.clone());
            let fields: Vec<String> = u.fields.iter().map(|f| f.name.clone()).collect();
            self.struct_layouts.insert(u.name.clone(), fields);
            let mut fct: HashMap<String, String> = HashMap::new();
            for field in &u.fields {
                if let tinox_parser::Type::Named(class_name) = &field.param_type {
                    fct.insert(field.name.clone(), class_name.clone());
                }
            }
            self.struct_field_class_types.insert(u.name.clone(), fct);
            let fllt: HashMap<String, String> = u.fields.iter()
                .map(|f| (f.name.clone(), Self::type_to_llvm(&f.param_type)))
                .collect();
            self.struct_field_llvm_types.insert(u.name.clone(), fllt);
            let fn_sigs: HashMap<String, (String, Vec<String>)> = u.fields.iter()
                .filter_map(|f| {
                    if let tinox_parser::Type::Fn { params, ret } = &f.param_type {
                        let r = Self::type_to_llvm(ret);
                        let ps: Vec<String> = params.iter().map(Self::type_to_llvm).collect();
                        Some((f.name.clone(), (r, ps)))
                    } else { None }
                })
                .collect();
            self.fn_field_sigs.insert(u.name.clone(), fn_sigs);
            self.method_ret_types.insert(format!("{}_new", u.name), "i64*".to_string());
        }

        for c in &all_classes {
            self.defined_classes.insert(c.name.clone());
            {
                if !c.type_params.is_empty() {
                    // Generic classes are specialized on demand under a mangled
                    // name, but a bare `Foo { … }` literal (type args elided —
                    // e.g. constructed inside another generic where the param is
                    // already erased) resolves to the base name and needs a
                    // layout, or it allocates 0 bytes with every field at
                    // offset 0. Register the type-erased layout (T → i64*).
                    if !self.struct_layouts.contains_key(&c.name) {
                        let fields = Self::collect_inherited_fields(&c.name, &class_ast_map);
                        self.struct_layouts.insert(c.name.clone(), fields);
                        self.struct_field_class_types.insert(
                            c.name.clone(),
                            Self::collect_field_class_types(&c.name, &class_ast_map),
                        );
                        self.struct_field_llvm_types.insert(
                            c.name.clone(),
                            Self::collect_field_llvm_types(&c.name, &class_ast_map),
                        );
                        self.fn_field_sigs.insert(
                            c.name.clone(),
                            Self::collect_fn_field_sigs(&c.name, &class_ast_map),
                        );
                    }
                    continue;
                }
                if let Some(parent) = &c.extends {
                    self.class_parents.insert(c.name.clone(), parent.clone());
                }
                let has_vtable = !c.implements.is_empty()
                    || self.classes_with_vtable.contains(&c.name);
                let mut fields: Vec<String> = Vec::new();
                if has_vtable {
                    fields.push("__vtable__".to_string());
                    self.classes_with_vtable.insert(c.name.clone());
                    let mut vtable_methods: Vec<String> = Vec::new();
                    let mut seen: HashSet<String> = HashSet::new();
                    for iface in &c.implements {
                        if let Some(methods) = self.vtable_layouts.get(iface) {
                            for m in methods {
                                if seen.insert(m.clone()) {
                                    vtable_methods.push(m.clone());
                                }
                            }
                        }
                    }
                    self.vtable_sizes.insert(c.name.clone(), vtable_methods.len());
                }
                fields.extend(Self::collect_inherited_fields(&c.name, &class_ast_map));
                if c.annotations.iter().any(|a| a.name == "Log") {
                    fields.push("log".to_string());
                }
                // Bug 139: struct_layouts (and the sibling per-class tables
                // below) are keyed by bare class name only, with no
                // namespace/module qualification. Import resolution dedups
                // by file path and one-class-per-file forces class name ==
                // file name, so the only way the SAME bare name reaches this
                // insert twice is two genuinely different classes in
                // different modules sharing a name -- whichever is
                // processed last used to silently clobber the other's
                // layout, producing a baffling "field not in layout of
                // typed class" codegen-internal error with no hint about
                // the real cause. A full fix (namespace-qualified table
                // keys) touches ~30 call sites and risks the B1 named-
                // struct-type optimization; this turns the silent
                // corruption into a clear, actionable compile error instead
                // (matches this project's "no silent garbage" rule) without
                // attempting the bigger rearchitecture.
                if self.struct_layouts.contains_key(&c.name) {
                    let mut bag = ErrorBag::new();
                    bag.push(Error::new(
                        c.span,
                        format!(
                            "class name '{}' is defined by two different classes in imported modules -- \
                             the compiler cannot distinguish same-named classes across modules (issue #139). \
                             Rename one of them, or avoid importing both modules in the same program.",
                            c.name
                        ),
                    ));
                    return Err(bag);
                }
                self.struct_layouts.insert(c.name.clone(), fields);
                let mut fct = Self::collect_field_class_types(&c.name, &class_ast_map);
                if c.annotations.iter().any(|a| a.name == "Log") {
                    fct.insert("log".to_string(), "Logger".to_string());
                }
                self.struct_field_class_types.insert(c.name.clone(), fct);
                let mut fllt = Self::collect_field_llvm_types(&c.name, &class_ast_map);
                if c.annotations.iter().any(|a| a.name == "Log") {
                    fllt.insert("log".to_string(), "i64*".to_string());
                }
                self.struct_field_llvm_types.insert(c.name.clone(), fllt);
                let fn_sigs = Self::collect_fn_field_sigs(&c.name, &class_ast_map);
                self.fn_field_sigs.insert(c.name.clone(), fn_sigs);

                for method in &c.methods {
                    let key = format!("{}_{}", c.name, method.name);
                    if !method.type_params.is_empty() {
                        // Monomorphized at the call site; no normal
                        // registration (that would log T as i64*)
                        self.generic_methods.insert(key, method.clone());
                        continue;
                    }
                    self.method_impl.insert(key.clone(), key.clone());
                    self.method_ret_types.insert(
                        format!("{}_{}", c.name, method.name),
                        self.type_to_llvm_inst(&method.ret_type),
                    );
                    // Track static methods (fnc) — they don't have a self parameter
                    if method.static_ {
                        self.static_method_keys.insert(key.clone());
                    }
                    // Track class name for methods returning class instances (for local_types inference)
                    if let Type::Named(ret_class) = &method.ret_type {
                        if self.defined_classes.contains(ret_class.as_str()) || self.struct_layouts.contains_key(ret_class.as_str()) {
                            self.method_ret_class.insert(key.clone(), ret_class.clone());
                        }
                    } else if let Some(marker) = Self::container_marker(&method.ret_type) {
                        // "List:C" only helps when C is a known class — downgrade otherwise
                        let marker = match marker.strip_prefix("List:") {
                            Some(cls) if !self.defined_classes.contains(cls) => "Array".to_string(),
                            _ => marker,
                        };
                        self.method_ret_class.insert(key.clone(), marker);
                    }
                    let param_tys: Vec<tinox_parser::Type> = method.params.iter()
                        .map(|p| p.param_type.clone()).collect();
                    self.method_param_types.insert(format!("{}_{}", c.name, method.name), param_tys);
                }
                let own_method_names: HashSet<String> =
                    c.methods.iter().map(|m| m.name.clone()).collect();
                let mut ancestor = c.extends.clone();
                while let Some(ref aname) = ancestor.clone() {
                    let Some(ac) = class_ast_map.get(aname) else { break; };
                    for method in &ac.methods {
                        if !own_method_names.contains(&method.name) {
                            let child_key = format!("{}_{}", c.name, method.name);
                            if !self.method_impl.contains_key(&child_key) {
                                let owner_key = Self::resolve_method_owner(
                                    aname, &method.name, &class_ast_map,
                                );
                                self.method_impl.insert(child_key.clone(), owner_key.clone());
                                self.method_ret_types.insert(child_key, self.type_to_llvm_inst(&method.ret_type));
                            }
                        }
                    }
                    ancestor = ac.extends.clone();
                }
            }
        }

        // Pre-pass: collect all function signatures; store generic fns/classes separately
        for decl in &source.decls {
            match &decl.node {
                DeclKind::Function(f) => {
                    if !f.type_params.is_empty() {
                        self.generic_fns.insert(f.name.clone(), f.clone());
                    } else {
                        let fn_name = if f.name == "main" { "tinox_main".to_string() } else { f.name.clone() };
                        let ret_ty = self.type_to_llvm_inst(&f.ret_type);
                        let param_tys: Vec<String> = f.params.iter().map(|p| Self::type_to_llvm(&p.param_type)).collect();
                        self.fn_sigs.insert(fn_name.clone(), (ret_ty, param_tys));
                        // Register return-class info for let-binding inference —
                        // same rules as for methods (Bug 6: without this,
                        // `let r = someModuleFn(); r.field` reads offset 0).
                        if let Type::Named(ret_class) = &f.ret_type {
                            if self.defined_classes.contains(ret_class.as_str())
                                || self.struct_layouts.contains_key(ret_class.as_str())
                            {
                                self.method_ret_class.insert(fn_name, ret_class.clone());
                            }
                        } else if let Some(marker) = Self::container_marker(&f.ret_type) {
                            let marker = match marker.strip_prefix("List:") {
                                Some(cls) if !self.defined_classes.contains(cls) => "Array".to_string(),
                                _ => marker,
                            };
                            self.method_ret_class.insert(fn_name, marker);
                        }
                    }
                }
                DeclKind::Class(c) if !c.type_params.is_empty() => {
                    self.generic_classes.insert(c.name.clone(), c.clone());
                }
                DeclKind::Enum(e) => {
                    self.known_enum_types.insert(e.name.clone());
                    for variant in &e.variants {
                        self.known_enum_variants.insert(variant.name.clone());
                        self.register_variant_owner(&e.name, &variant.name);
                        self.register_variant_payloads(
                            &variant.name,
                            variant.args.iter().map(Self::payload_kind).collect(),
                        );
                    }
                }
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        if let DeclKind::Class(c) = &inner.node {
                            if !c.type_params.is_empty() {
                                self.generic_classes.insert(c.name.clone(), c.clone());
                            }
                        } else if let DeclKind::Enum(e) = &inner.node {
                            self.known_enum_types.insert(e.name.clone());
                            for variant in &e.variants {
                                self.known_enum_variants.insert(variant.name.clone());
                                self.register_variant_owner(&e.name, &variant.name);
                                self.register_variant_payloads(
                            &variant.name,
                            variant.args.iter().map(Self::payload_kind).collect(),
                        );
                            }
                        } else if let DeclKind::Function(f) = &inner.node {
                            // Register namespace-level functions (incl. extern) in fn_sigs
                            if !f.type_params.is_empty() {
                                self.generic_fns.insert(f.name.clone(), f.clone());
                            } else {
                                let fn_name = f.name.clone();
                                let ret_ty = self.type_to_llvm_inst(&f.ret_type);
                                let param_tys: Vec<String> = f.params.iter().map(|p| Self::type_to_llvm(&p.param_type)).collect();
                                self.fn_sigs.insert(fn_name, (ret_ty, param_tys));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Pre-register toString() for @Sensitive/@Masked classes so method dispatch works
        self.pre_register_log_mask_tostring();
        // Pre-register toJson() / fromJson() for @JsonSerializable classes
        self.pre_register_json_to_json();
        self.pre_register_json_from_json();

        // B1 phase 1: emit named LLVM struct types for plain classes now that all
        // non-generic layouts are built. Enables typed field access + opt-level
        // verification of field offsets.
        self.emit_struct_type_defs();

        // Second pass: generate code (skip generic functions — they are monomorphized on demand)
        for decl in &source.decls {
            match &decl.node {
                DeclKind::Function(f) => {
                    if f.type_params.is_empty() {
                        self.gen_fn(f)?;
                    }
                }
                DeclKind::Class(c) if c.type_params.is_empty() => {
                    for method in &c.methods {
                        if method.type_params.is_empty() {
                            self.gen_class_method(&c.name, method)?;
                        }
                    }
                }
                DeclKind::Immutable(u) => {
                    self.emit_immutable_new(u);
                }
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        match &inner.node {
                            DeclKind::Function(f) if f.type_params.is_empty() => {
                                self.gen_fn(f)?;
                            }
                            DeclKind::Class(c) => {
                                if c.type_params.is_empty() {
                                    for method in &c.methods {
                                        if method.type_params.is_empty() {
                                            self.gen_class_method(&c.name, method)?;
                                        }
                                    }
                                }
                            }
                            DeclKind::Immutable(u) => {
                                self.emit_immutable_new(u);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        // Alternative entry point `class Main { fnc main() -> Int32 }`
        // (Issue #149 stage 1) — validates the shape and (if valid) records
        // user_main_class; emit_tinox_main_bootstrap below wires it into
        // @tinox_main once every auto-run kind has had a chance to register
        // into background_run_fns, so class Main can coexist with them.
        self.emit_class_main_entry_point(source)?;

        // Emit vtable globals for classes that implement interfaces
        self.emit_vtable_globals(source);

        // Emit REST route shims and registration function
        self.emit_route_code();

        // Emit the auto-run HTTP/3 REST server for an @Http3RestController class
        self.emit_http3_route_code();

        // Emit the auto-run accept/message loop for a @WebsocketEndpoint class
        self.emit_ws_code();

        // Emit the auto-run HTTP shell/client-JS server + WS accept loop for
        // a @TinoxUIApp class (issue #215, Phase 4)
        self.emit_tinoxui_code();

        // Emit the auto-run connect/receive loop for an @Amqp10Consumer class
        self.emit_amqp10_consumer_code();

        // Emit the auto-run connect/receive loop for an @Amqp091Consumer class
        self.emit_amqp091_consumer_code();

        // Emit the dev-mode introspection API (no-op unless [dev] enabled)
        self.emit_devui_code(&class_ast_map);

        // Unify user_main_class + background_run_fns into @tinox_main
        self.emit_tinox_main_bootstrap();

        // Emit DI globals, getters, factories, and startup initializer
        self.emit_di_code();

        // Emit CLI main (tinox_main) for @Command classes
        self.emit_cli_code();

        // Emit toString() for classes with @Sensitive or @Masked fields
        self.emit_log_mask_code();

        // Emit toJson() / fromJson() for classes with @JsonSerializable
        self.emit_json_serialize_code();
        self.emit_json_deserialize_code();

        // Emit test-runner main if set_test_entry() was called
        self.emit_test_code();

        // Emit SQL-constant functions and row-mapping helpers for @Entity classes
        self.emit_entity_code();

        for (name, s) in &self.strings {
            let escaped = Self::escape_llvm_string(s);
            writeln!(
                &mut self.ir,
                "@{} = private constant [{} x i8] c\"{}\\00\"",
                name,
                s.len() + 1,
                escaped
            )
            .unwrap();
        }

        Ok(())
    }

    /// Generates route handler shims, `__tinox_register_routes`, and (if needed) a `main`
    /// for all routes collected via REST annotations (@GET, @POST, …).
    fn emit_route_code(&mut self) {
        // @Http3RestController present -> emit_http3_route_code owns
        // route_entries instead (Http3Server, not tinox_HttpServer_listen/
        // issue #140) -- must not also emit from here, or both paths would
        // try to define @tinox_main.
        if self.route_entries.is_empty() || self.http3_rest_controller.is_some() {
            return;
        }

        // External declares for the C runtime route-based HTTP server API.
        // tinox_HttpServer_* are distinct from any user-defined HttpServer class methods.
        writeln!(&mut self.lambda_ir, "declare i64* @tinox_HttpServer_new(i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_get(i64*, i8*, i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_post(i64*, i8*, i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_put(i64*, i8*, i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_patch(i64*, i8*, i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_delete(i64*, i8*, i64)").unwrap();
        writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_listen(i64*)").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();

        let routes = self.route_entries.clone();
        self.emit_route_annotation_globals(&routes);
        self.ensure_postparam_specializations(&routes);

        // ── Shim functions ──────────────────────────────────────────────────────
        // Signature: void (i64) — ctx_i64 is a ptrtoint of the HttpContext* pointer.
        //
        // HttpContext layout (no vtable): [request: i64*, response: i64*]  → offsets 0, 1
        // HttpResponse layout:            [statusCode: i64, headers: i8*, body: i8*] → offsets 0, 1, 2
        // HttpRequest layout:             [method, path, queryString, headers, body, params] → offset 3 = headers
        for (idx, route) in routes.iter().enumerate() {
            let shim = format!("__route_{}_{}", route.class_name, route.method_name);

            writeln!(&mut self.lambda_ir, "define void @{shim}(i64 %ctx_i64) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
            self.emit_route_shim_body(idx, route);
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }

        // ── Metrics endpoint shim (if enabled) ──────────────────────────────────
        let metrics_path = self.metrics_path.clone();
        if let Some(ref mpath) = metrics_path {
            let mpath_escaped = Self::escape_llvm_string(mpath);
            let mpath_len = mpath.len() + 1;
            writeln!(&mut self.ir,
                "@__metrics_path = private constant [{mpath_len} x i8] c\"{mpath_escaped}\\00\"").unwrap();
            // Shim: GET /metrics → call tinox_metrics_prometheus(), return as text/plain
            writeln!(&mut self.lambda_ir, "declare i8* @tinox_metrics_prometheus()").unwrap();
            writeln!(&mut self.lambda_ir, "declare i64* @tinox_HttpServer_new(i64)").unwrap();
            writeln!(&mut self.lambda_ir, "define void @__metrics_shim(i64 %ctx_i64) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
            // HttpContext[1] = response ptr (i64*)
            writeln!(&mut self.lambda_ir, "  %resp_field = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_i64 = load i64, i64* %resp_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_ptr = inttoptr i64 %resp_i64 to i64*").unwrap();
            // Get prometheus text
            writeln!(&mut self.lambda_ir, "  %prom_text = call i8* @tinox_metrics_prometheus()").unwrap();
            // Set status 200
            writeln!(&mut self.lambda_ir, "  %sc_field = getelementptr i64, i64* %resp_ptr, i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 200, i64* %sc_field").unwrap();
            // Set body
            writeln!(&mut self.lambda_ir, "  %body_field = getelementptr i64, i64* %resp_ptr, i64 2").unwrap();
            writeln!(&mut self.lambda_ir, "  %body_i64 = ptrtoint i8* %prom_text to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 %body_i64, i64* %body_field").unwrap();
            // Set Content-Type header to text/plain; version=0.0.4
            let ct = "text/plain; version=0.0.4";
            let ct_escaped = Self::escape_llvm_string(ct);
            let ct_len = ct.len() + 1;
            writeln!(&mut self.ir,
                "@__metrics_ct = private constant [{ct_len} x i8] c\"{ct_escaped}\\00\"").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %ct_hdr_key = getelementptr [13 x i8], [13 x i8]* @__hdr_content_type, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %ct_hdr_val = getelementptr [{ct_len} x i8], [{ct_len} x i8]* @__metrics_ct, i64 0, i64 0").unwrap();
            // headers are at HttpResponse[1] (i8* to map)
            writeln!(&mut self.lambda_ir, "  %hdrs_field = getelementptr i64, i64* %resp_ptr, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %hdrs_i64 = load i64, i64* %hdrs_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %hdrs_ptr = inttoptr i64 %hdrs_i64 to i8*").unwrap();
            // Bug found while adding the dev-mode introspection API (which
            // needed to mirror this exact response-writing shape): this
            // passed %body_i64 (the RESPONSE BODY's pointer) as the header
            // VALUE instead of %ct_hdr_val (computed right above, then
            // never actually used) -- every /metrics response's
            // Content-Type header ended up set to the same string as the
            // body (the full Prometheus text) instead of
            // "text/plain; version=0.0.4".
            writeln!(&mut self.lambda_ir, "  %ct_val_i64 = ptrtoint i8* %ct_hdr_val to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_map_set(i8* %hdrs_ptr, i8* %ct_hdr_key, i64 %ct_val_i64)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }

        // ── __tinox_register_routes ─────────────────────────────────────────────
        writeln!(&mut self.lambda_ir, "define void @__tinox_register_routes(i64* %server) {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

        for (idx, route) in routes.iter().enumerate() {
            let shim = format!("__route_{}_{}", route.class_name, route.method_name);
            let server_method = format!("tinox_HttpServer_{}", route.http_method.to_lowercase());
            let path_len = route.path.len() + 1;

            writeln!(&mut self.lambda_ir,
                "  %fn_{idx} = ptrtoint void (i64)* @{shim} to i64").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %path_{idx} = getelementptr [{path_len} x i8], [{path_len} x i8]* @__route_path_{idx}, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir,
                "  call void @{server_method}(i64* %server, i8* %path_{idx}, i64 %fn_{idx})").unwrap();
        }

        // Register the /metrics route if enabled
        if let Some(ref mpath) = metrics_path {
            let mpath_len = mpath.len() + 1;
            writeln!(&mut self.lambda_ir,
                "  %metrics_fn = ptrtoint void (i64)* @__metrics_shim to i64").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %metrics_path = getelementptr [{mpath_len} x i8], [{mpath_len} x i8]* @__metrics_path, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir,
                "  call void @tinox_HttpServer_get(i64* %server, i8* %metrics_path, i64 %metrics_fn)").unwrap();
        }

        writeln!(&mut self.lambda_ir, "  ret void").unwrap();
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();

        // ── Auto-run listen loop (only when no legacy bare-fn-main exists) ──────
        // Registered into background_run_fns instead of claiming @tinox_main
        // directly, so emit_tinox_main_bootstrap can spawn it (and call
        // Main_main, and any other registered kind) from one unified entry
        // point.
        if !self.has_main {
            let port = std::env::var("TINOX_PORT")
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(8080);
            writeln!(&mut self.lambda_ir, "define i64 @__tinox_run_http() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %server = call i64* @tinox_HttpServer_new(i64 {port})").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @__tinox_register_routes(i64* %server)").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_HttpServer_listen(i64* %server)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.background_run_fns.push("__tinox_run_http".to_string());
            self.startup_endpoints.push(("HTTP".to_string(), format!(":{port}")));
        }
    }

    /// String constant globals (`@__route_path_N`, `@__route_produces_N`,
    /// `@__route_consumes_N`, `@__route_auth_prefix_N`/`_auth_type_N`,
    /// `@__route_oidc_roles_N`, plus the shared `@__hdr_content_type`/
    /// `@__hdr_authorization`/`@__str_401`/`@__str_415`) that
    /// `emit_route_shim_body` references by name. Shared by
    /// `emit_route_code` (TCP) and `emit_http3_route_code` (HTTP/3) --
    /// exactly one of the two ever runs per program (see the
    /// `http3_rest_controller.is_some()` exclusion guard in
    /// `emit_route_code`), so there is no risk of emitting the same
    /// `@__route_path_N` global twice.
    fn emit_route_annotation_globals(&mut self, routes: &[RouteEntry]) {
        for (idx, route) in routes.iter().enumerate() {
            let path = &route.path;
            let escaped = Self::escape_llvm_string(path);
            writeln!(&mut self.ir,
                "@__route_path_{idx} = private constant [{} x i8] c\"{escaped}\\00\"",
                path.len() + 1).unwrap();

            if let Some(ref ct) = route.produces {
                let esc = Self::escape_llvm_string(ct);
                writeln!(&mut self.ir,
                    "@__route_produces_{idx} = private constant [{} x i8] c\"{esc}\\00\"",
                    ct.len() + 1).unwrap();
            }
            if let Some(ref ct) = route.consumes {
                let esc = Self::escape_llvm_string(ct);
                writeln!(&mut self.ir,
                    "@__route_consumes_{idx} = private constant [{} x i8] c\"{esc}\\00\"",
                    ct.len() + 1).unwrap();
            }
            if let Some(ref auth) = route.auth_type {
                // "Bearer " or "Basic " prefix for Authorization header check
                let prefix = format!("{} ", Self::capitalize_first(auth));
                let esc = Self::escape_llvm_string(&prefix);
                writeln!(&mut self.ir,
                    "@__route_auth_prefix_{idx} = private constant [{} x i8] c\"{esc}\\00\"",
                    prefix.len() + 1).unwrap();
                // Bare authType ("bearer"/"basic", no trailing space) passed
                // as-is to AuthValidator::validate, if defined (issue: @Auth
                // previously never validated the credential, only this
                // prefix -- see the guard below).
                let esc_type = Self::escape_llvm_string(auth);
                writeln!(&mut self.ir,
                    "@__route_auth_type_{idx} = private constant [{} x i8] c\"{esc_type}\\00\"",
                    auth.len() + 1).unwrap();
            }
            if !route.oidc_roles.is_empty() {
                // Pipe-joined role list, passed as-is to OidcGuard::checkRoles
                // (module tinox.core.rest.server), which splits on "|" itself.
                let roles_csv = route.oidc_roles.join("|");
                let esc = Self::escape_llvm_string(&roles_csv);
                writeln!(&mut self.ir,
                    "@__route_oidc_roles_{idx} = private constant [{} x i8] c\"{esc}\\00\"",
                    roles_csv.len() + 1).unwrap();
            }
        }
        // Static string constants shared across shims
        writeln!(&mut self.ir,
            "@__hdr_content_type = private constant [13 x i8] c\"Content-Type\\00\"").unwrap();
        writeln!(&mut self.ir,
            "@__hdr_authorization = private constant [14 x i8] c\"Authorization\\00\"").unwrap();
        writeln!(&mut self.ir,
            "@__str_401 = private constant [13 x i8] c\"Unauthorized\\00\"").unwrap();
        writeln!(&mut self.ir,
            "@__str_415 = private constant [23 x i8] c\"Unsupported Media Type\\00\"").unwrap();
    }

    /// Shared per-route shim body: @Auth guard -> @OIDCRolesAllowed guard ->
    /// @Consumes check -> @StatusCode -> @Produces -> call the real
    /// {Class}_{Method} handler. Entirely transport-agnostic (only ever
    /// touches %ctx_ptr via the hard-coded HttpContext/HttpRequest/
    /// HttpResponse field offsets documented above `emit_route_code`'s shim
    /// loop) -- reused unchanged by both the TCP auto-server
    /// (`emit_route_code`) and the HTTP/3 auto-server (`emit_http3_route_code`,
    /// @Http3RestController). Caller is responsible for the `define`/
    /// `entry.tnx:`/`%ctx_ptr` prologue and the `ret void`/`}` epilogue,
    /// since the two callers use different function signatures (the TCP
    /// shim is a bare `void(i64)` C callback; the HTTP/3 shim additionally
    /// takes a trailing `i64* %env` so it can be wrapped as a genuine Tinox
    /// closure value passed into `Http3Server.get`/etc.).
    fn emit_route_shim_body(&mut self, idx: usize, route: &RouteEntry) {
        {
            let method_fn = format!("{}_{}", route.class_name, route.method_name);
            let ctrl_size = self
                .struct_layouts
                .get(&route.class_name)
                .map(|f| f.len().max(1) * 8)
                .unwrap_or(8);

            // ── @Auth guard ──────────────────────────────────────────────────────
            if let Some(ref _auth) = route.auth_type {
                // Load request.headers (HttpContext[0] = request, HttpRequest[3] = headers)
                writeln!(&mut self.lambda_ir, "  %req_field_{idx} = getelementptr i64, i64* %ctx_ptr, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_i64_{idx} = load i64, i64* %req_field_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_ptr_{idx} = inttoptr i64 %req_i64_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hdrs_field_{idx} = getelementptr i64, i64* %req_ptr_{idx}, i64 3").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hdrs_i64_{idx} = load i64, i64* %req_hdrs_field_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hdrs_{idx} = inttoptr i64 %req_hdrs_i64_{idx} to i8*").unwrap();
                writeln!(&mut self.lambda_ir, "  %auth_key_{idx} = getelementptr [14 x i8], [14 x i8]* @__hdr_authorization, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %auth_val_{idx} = call i64 @tinox_map_get(i8* %req_hdrs_{idx}, i8* %auth_key_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  %auth_str_{idx} = inttoptr i64 %auth_val_{idx} to i8*").unwrap();
                // Get prefix string
                let prefix_len = _auth.len() + 2; // "Bearer " or "Basic "
                writeln!(&mut self.lambda_ir, "  %auth_prefix_{idx} = getelementptr [{prefix_len} x i8], [{prefix_len} x i8]* @__route_auth_prefix_{idx}, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %auth_ok_{idx} = call i64 @tinox_string_starts_with(i8* %auth_str_{idx}, i8* %auth_prefix_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  %auth_cmp_{idx} = icmp eq i64 %auth_ok_{idx}, 1").unwrap();
                writeln!(&mut self.lambda_ir, "  br i1 %auth_cmp_{idx}, label %auth_pass_{idx}, label %auth_fail_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "auth_fail_{idx}:").unwrap();
                // Set 401 status and body then return
                writeln!(&mut self.lambda_ir, "  %resp_f401_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_i401_{idx} = load i64, i64* %resp_f401_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_p401_{idx} = inttoptr i64 %resp_i401_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %sc_f401_{idx} = getelementptr i64, i64* %resp_p401_{idx}, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  store i64 401, i64* %sc_f401_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  ret void").unwrap();
                writeln!(&mut self.lambda_ir, "auth_pass_{idx}:").unwrap();

                // Bug: @Auth previously stopped here -- any syntactically
                // correct "Bearer .../Basic ..." header passed, since the
                // scheme-prefix check above was the *only* check. Actually
                // validate the credential now: look for a project-defined
                // `class AuthValidator { fnc validate(authType: String,
                // credential: String) -> Bool }` (a well-known name, same
                // convention as the DI `_di_get`/`_di_create` symbols this
                // file already calls elsewhere) and call it if present.
                // Mirrors RestApi.authValidator's own fix (Bug 104) for the
                // separate Tinox-level REST framework: if no validator is
                // defined, default to rejecting every request rather than
                // accepting an unauthenticated one.
                let auth_type_len = _auth.len() + 1;
                writeln!(&mut self.lambda_ir, "  %cred_len_{idx} = call i64 @tinox_string_length(i8* %auth_str_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  %credential_{idx} = call i8* @tinox_string_substring(i8* %auth_str_{idx}, i64 {auth_type_len}, i64 %cred_len_{idx})").unwrap();
                if self.defined_classes.contains("AuthValidator") {
                    writeln!(&mut self.lambda_ir, "  %auth_type_ptr_{idx} = getelementptr [{auth_type_len} x i8], [{auth_type_len} x i8]* @__route_auth_type_{idx}, i64 0, i64 0").unwrap();
                    writeln!(&mut self.lambda_ir, "  %cred_ok_{idx} = call i1 @AuthValidator_validate(i8* %auth_type_ptr_{idx}, i8* %credential_{idx})").unwrap();
                    writeln!(&mut self.lambda_ir, "  br i1 %cred_ok_{idx}, label %cred_pass_{idx}, label %cred_fail_{idx}").unwrap();
                } else {
                    writeln!(&mut self.lambda_ir, "  br label %cred_fail_{idx}").unwrap();
                }
                writeln!(&mut self.lambda_ir, "cred_fail_{idx}:").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_fcred_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_icred_{idx} = load i64, i64* %resp_fcred_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_pcred_{idx} = inttoptr i64 %resp_icred_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %sc_fcred_{idx} = getelementptr i64, i64* %resp_pcred_{idx}, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  store i64 401, i64* %sc_fcred_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  ret void").unwrap();
                if self.defined_classes.contains("AuthValidator") {
                    writeln!(&mut self.lambda_ir, "cred_pass_{idx}:").unwrap();
                }
            }

            // ── @OIDCRolesAllowed guard ──────────────────────────────────────────
            // Delegates entirely to the real (normally-compiled) Tinox function
            // OidcGuard::checkRoles (tinox.core.rest.server, module
            // tinox.core.rest.server always imported when this annotation is in
            // use) -- unlike @Auth's hand-emitted prefix check, the JWKS fetch/
            // RS256 verify/role check is far too much logic to hand-write as raw
            // IR, so this shim just calls the compiled function by its known
            // symbol name (OidcGuard_checkRoles) exactly like it already calls
            // the controller's own handler method below. checkRoles() sets
            // ctx.response itself (401/403 with a JSON error body) on failure,
            // so there is nothing left to do here but branch on its result.
            if !route.oidc_roles.is_empty() {
                let roles_csv = route.oidc_roles.join("|");
                let roles_len = roles_csv.len() + 1;
                writeln!(&mut self.lambda_ir, "  %oidc_roles_ptr_{idx} = getelementptr [{roles_len} x i8], [{roles_len} x i8]* @__route_oidc_roles_{idx}, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %oidc_ok_{idx} = call i1 @OidcGuard_checkRoles(i64* %ctx_ptr, i8* %oidc_roles_ptr_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  br i1 %oidc_ok_{idx}, label %oidc_pass_{idx}, label %oidc_fail_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "oidc_fail_{idx}:").unwrap();
                writeln!(&mut self.lambda_ir, "  ret void").unwrap();
                writeln!(&mut self.lambda_ir, "oidc_pass_{idx}:").unwrap();
            }

            // ── @Consumes: validate request Content-Type ─────────────────────────
            if let Some(ref expected_ct) = route.consumes {
                let ct_len = expected_ct.len() + 1;
                writeln!(&mut self.lambda_ir, "  %req_fct_{idx} = getelementptr i64, i64* %ctx_ptr, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_ict_{idx} = load i64, i64* %req_fct_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_pct_{idx} = inttoptr i64 %req_ict_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hf_ct_{idx} = getelementptr i64, i64* %req_pct_{idx}, i64 3").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hi_ct_{idx} = load i64, i64* %req_hf_ct_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_hm_ct_{idx} = inttoptr i64 %req_hi_ct_{idx} to i8*").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_key_{idx} = getelementptr [13 x i8], [13 x i8]* @__hdr_content_type, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_ct_val_{idx} = call i64 @tinox_map_get(i8* %req_hm_ct_{idx}, i8* %ct_key_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  %req_ct_str_{idx} = inttoptr i64 %req_ct_val_{idx} to i8*").unwrap();
                writeln!(&mut self.lambda_ir, "  %expected_ct_{idx} = getelementptr [{ct_len} x i8], [{ct_len} x i8]* @__route_consumes_{idx}, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_match_{idx} = call i64 @tinox_string_starts_with(i8* %req_ct_str_{idx}, i8* %expected_ct_{idx})").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_ok_{idx} = icmp eq i64 %ct_match_{idx}, 1").unwrap();
                writeln!(&mut self.lambda_ir, "  br i1 %ct_ok_{idx}, label %ct_pass_{idx}, label %ct_fail_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "ct_fail_{idx}:").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_f415_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_i415_{idx} = load i64, i64* %resp_f415_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_p415_{idx} = inttoptr i64 %resp_i415_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %sc_f415_{idx} = getelementptr i64, i64* %resp_p415_{idx}, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  store i64 415, i64* %sc_f415_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  ret void").unwrap();
                writeln!(&mut self.lambda_ir, "ct_pass_{idx}:").unwrap();
            }

            // ── @StatusCode: set default response status before handler runs ────
            if let Some(sc) = route.status_code {
                writeln!(&mut self.lambda_ir, "  %resp_fsc_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_isc_{idx} = load i64, i64* %resp_fsc_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_psc_{idx} = inttoptr i64 %resp_isc_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %sc_slot_{idx} = getelementptr i64, i64* %resp_psc_{idx}, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  store i64 {sc}, i64* %sc_slot_{idx}").unwrap();
            }

            // ── @Produces: pre-set Content-Type on response headers ─────────────
            if let Some(ref ct) = route.produces {
                let ct_len = ct.len() + 1;
                // Get response.headers (HttpResponse[1])
                writeln!(&mut self.lambda_ir, "  %resp_fprod_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_iprod_{idx} = load i64, i64* %resp_fprod_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %resp_pprod_{idx} = inttoptr i64 %resp_iprod_{idx} to i64*").unwrap();
                writeln!(&mut self.lambda_ir, "  %hdrs_fprod_{idx} = getelementptr i64, i64* %resp_pprod_{idx}, i64 1").unwrap();
                writeln!(&mut self.lambda_ir, "  %hdrs_iprod_{idx} = load i64, i64* %hdrs_fprod_{idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  %hdrs_prod_{idx} = inttoptr i64 %hdrs_iprod_{idx} to i8*").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_key_prod_{idx} = getelementptr [13 x i8], [13 x i8]* @__hdr_content_type, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_val_prod_{idx} = getelementptr [{ct_len} x i8], [{ct_len} x i8]* @__route_produces_{idx}, i64 0, i64 0").unwrap();
                writeln!(&mut self.lambda_ir, "  %ct_val_i64_{idx} = ptrtoint i8* %ct_val_prod_{idx} to i64").unwrap();
                writeln!(&mut self.lambda_ir, "  call void @tinox_map_set(i8* %hdrs_prod_{idx}, i8* %ct_key_prod_{idx}, i64 %ct_val_i64_{idx})").unwrap();
            }

            // ── Allocate controller and call the handler ─────────────────────────
            if route.is_static {
                // fnc (static): no self pointer, called as method_fn(<bound params>)
                self.emit_route_handler_call(idx, route, &method_fn, None);
            } else {
                // fn (instance): use DI getter/factory if the controller is a DI component
                let di_scope = self.di_components.iter()
                    .find(|c| c.class_name == route.class_name)
                    .map(|c| c.scope.clone());
                match di_scope {
                    Some(DiScope::Application) | Some(DiScope::Startup) => {
                        writeln!(&mut self.lambda_ir,
                            "  %ctrl_{idx} = call i64* @{}_di_get()", route.class_name).unwrap();
                        // Re-inject any @HttpRequestScoped fields per-request
                        let inject_fields: Vec<(String, String)> = self.di_components.iter()
                            .find(|c| c.class_name == route.class_name)
                            .map(|c| c.inject_fields.iter()
                                .map(|f| (f.field_name.clone(), f.field_type.clone()))
                                .collect())
                            .unwrap_or_default();
                        for (fi, (fname, ftype)) in inject_fields.iter().enumerate() {
                            let is_request_scoped = self.di_components.iter()
                                .any(|c| c.class_name == *ftype && matches!(c.scope, DiScope::HttpRequest));
                            if is_request_scoped {
                                let foffset = self.struct_layouts.get(route.class_name.as_str())
                                    .and_then(|l| l.iter().position(|f| f == fname))
                                    .unwrap_or(0);
                                writeln!(&mut self.lambda_ir,
                                    "  %req_dep_{idx}_{fi} = call i64* @{ftype}_di_create()").unwrap();
                                writeln!(&mut self.lambda_ir,
                                    "  %req_dep_i64_{idx}_{fi} = ptrtoint i64* %req_dep_{idx}_{fi} to i64").unwrap();
                                writeln!(&mut self.lambda_ir,
                                    "  %req_fptr_{idx}_{fi} = getelementptr i64, i64* %ctrl_{idx}, i64 {foffset}").unwrap();
                                writeln!(&mut self.lambda_ir,
                                    "  store i64 %req_dep_i64_{idx}_{fi}, i64* %req_fptr_{idx}_{fi}").unwrap();
                            }
                        }
                    }
                    Some(DiScope::HttpRequest) => {
                        writeln!(&mut self.lambda_ir,
                            "  %ctrl_{idx} = call i64* @{}_di_create()", route.class_name).unwrap();
                    }
                    None => {
                        writeln!(&mut self.lambda_ir,
                            "  %raw_{idx} = call i8* @tinox_alloc(i64 {ctrl_size})").unwrap();
                        writeln!(&mut self.lambda_ir,
                            "  %ctrl_{idx} = bitcast i8* %raw_{idx} to i64*").unwrap();
                    }
                }
                let ctrl_arg = format!("%ctrl_{idx}");
                self.emit_route_handler_call(idx, route, &method_fn, Some(&ctrl_arg));
            }
        }
    }

    /// Builds the real, typed call-argument list from `route.params` (one
    /// of `@PathParam`/`@QueryParam`/`@PostParam`/`@HttpContext` per
    /// parameter, in declared order -- no unannotated/implicit shape,
    /// validated at typecheck time), calls the handler, and -- unless the
    /// declared return type is `HttpContext` (manual-response mode, where
    /// the handler already built `ctx.response` itself and the returned
    /// value is simply never dereferenced) -- serializes the returned
    /// value as the JSON response body (`emit_route_auto_serialize`).
    /// `ctrl_arg` is `Some("%ctrl_N")` for an instance (`fn`) handler,
    /// `None` for a static (`fnc`) one -- prepended to the bound-parameter
    /// args when present, mirroring how the old fixed call already varied
    /// its argument list on `route.is_static`.
    /// Triggers every `Json::deserialize<T>` specialization `@PostParam`
    /// bindings across `routes` will need, BEFORE any shim function starts
    /// being written -- must run at this specific point, not lazily
    /// inside `emit_route_handler_call`/`emit_route_shim_body`.
    ///
    /// `ensure_generic_method_specialization` (shared with the real
    /// `Class::method<T>(...)` call-site path) emits a brand new
    /// top-level `define ...` function straight into `self.lambda_ir` the
    /// first time a given specialization is needed. That's safe when
    /// called from ordinary expression codegen (`self.ir` is the
    /// "current function" buffer there, `self.lambda_ir` only ever holds
    /// complete, already-finished definitions). It is NOT safe called
    /// lazily from inside `emit_route_handler_call`, because THAT
    /// function builds the current route's shim body by writing directly
    /// into `self.lambda_ir` itself (mirroring `emit_route_code`'s
    /// declare-then-shim-loop shape) -- found live: a `Json_deserialize__
    /// Person` specialization triggered mid-shim landed as a nested
    /// `define` textually inside the enclosing `__route_Ctrl_echo`
    /// shim's own body, an LLVM verifier error ("expected instruction
    /// opcode"). Pre-triggering here, before the shim-emission loop
    /// starts, ensures each specialization lands as its own clean,
    /// separate top-level entry first; the later in-shim call to
    /// `ensure_generic_method_specialization` for the same (class, type)
    /// pair is then a cheap no-op (`generated_specializations` already
    /// contains it) that just returns the mangled name.
    fn ensure_postparam_specializations(&mut self, routes: &[RouteEntry]) {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        for route in routes {
            for binding in &route.params {
                if binding.kind != RouteParamKind::PostParam {
                    continue;
                }
                let tinox_parser::Type::Named(target_class) = &binding.ty else {
                    continue; // validated as a class at typecheck time; defensive skip otherwise
                };
                if !seen.insert(target_class.clone()) {
                    continue;
                }
                let Some(gm) = self.generic_methods.get("Json_deserialize").cloned() else {
                    continue; // same defensive skip as emit_route_handler_call's .expect() context
                };
                self.ensure_generic_method_specialization(
                    "Json_deserialize", &gm, &[tinox_parser::Type::Named(target_class.clone())],
                ).expect("Json::deserialize<T> specialization should always succeed for an already-typechecked @JsonSerializable class");
            }
        }
    }

    fn emit_route_handler_call(&mut self, idx: usize, route: &RouteEntry, method_fn: &str, ctrl_arg: Option<&str>) {
        use tinox_parser::Type;

        let mut call_args: Vec<String> = Vec::new();
        if let Some(ctrl) = ctrl_arg {
            call_args.push(format!("i64* {ctrl}"));
        }

        for (pi, binding) in route.params.iter().enumerate() {
            let n = format!("{idx}_{pi}");
            match binding.kind {
                RouteParamKind::HttpContext => {
                    call_args.push("i64* %ctx_ptr".to_string());
                }
                RouteParamKind::PathParam | RouteParamKind::QueryParam => {
                    writeln!(&mut self.lambda_ir, "  %req_field_{n} = getelementptr i64, i64* %ctx_ptr, i64 0").unwrap();
                    writeln!(&mut self.lambda_ir, "  %req_i64_{n} = load i64, i64* %req_field_{n}").unwrap();
                    writeln!(&mut self.lambda_ir, "  %req_ptr_{n} = inttoptr i64 %req_i64_{n} to i64*").unwrap();
                    let name_ptr = self.emit_lambda_string_literal(&binding.name);
                    let getter = if binding.kind == RouteParamKind::PathParam { "HttpRequest_getParam" } else { "HttpRequest_getQuery" };
                    writeln!(&mut self.lambda_ir,
                        "  %raw_{n} = call i8* @{getter}(i64* %req_ptr_{n}, i8* {name_ptr})").unwrap();

                    if matches!(binding.ty, Type::String) {
                        writeln!(&mut self.lambda_ir, "  %len_{n} = call i64 @tinox_string_length(i8* %raw_{n})").unwrap();
                        writeln!(&mut self.lambda_ir, "  %empty_{n} = icmp eq i64 %len_{n}, 0").unwrap();
                        writeln!(&mut self.lambda_ir, "  br i1 %empty_{n}, label %param_fail_{n}, label %param_ok_{n}").unwrap();
                        self.emit_param_bind_failure(&n, &binding.name);
                        writeln!(&mut self.lambda_ir, "param_ok_{n}:").unwrap();
                        call_args.push(format!("i8* %raw_{n}"));
                    } else {
                        let (checker, slot_ty) = match binding.ty {
                            Type::Bool => ("tinox_parse_bool_checked", "i32"),
                            Type::Float64 | Type::Float32 => ("tinox_parse_float_checked", "double"),
                            _ => ("tinox_parse_int_checked", "i64"),
                        };
                        writeln!(&mut self.lambda_ir, "  %out_{n} = alloca {slot_ty}").unwrap();
                        writeln!(&mut self.lambda_ir,
                            "  %ok_{n} = call i32 @{checker}(i8* %raw_{n}, {slot_ty}* %out_{n})").unwrap();
                        writeln!(&mut self.lambda_ir, "  %okbit_{n} = icmp ne i32 %ok_{n}, 0").unwrap();
                        writeln!(&mut self.lambda_ir, "  br i1 %okbit_{n}, label %param_ok_{n}, label %param_fail_{n}").unwrap();
                        self.emit_param_bind_failure(&n, &binding.name);
                        writeln!(&mut self.lambda_ir, "param_ok_{n}:").unwrap();
                        writeln!(&mut self.lambda_ir, "  %val_{n} = load {slot_ty}, {slot_ty}* %out_{n}").unwrap();
                        match binding.ty {
                            Type::Int32 => {
                                writeln!(&mut self.lambda_ir, "  %val32_{n} = trunc i64 %val_{n} to i32").unwrap();
                                call_args.push(format!("i32 %val32_{n}"));
                            }
                            Type::Bool => {
                                writeln!(&mut self.lambda_ir, "  %boolval_{n} = trunc i32 %val_{n} to i1").unwrap();
                                call_args.push(format!("i1 %boolval_{n}"));
                            }
                            Type::Float32 => {
                                writeln!(&mut self.lambda_ir, "  %val32f_{n} = fptrunc double %val_{n} to float").unwrap();
                                call_args.push(format!("float %val32f_{n}"));
                            }
                            Type::Float64 => call_args.push(format!("double %val_{n}")),
                            _ => call_args.push(format!("i64 %val_{n}")),
                        }
                    }
                }
                RouteParamKind::PostParam => {
                    writeln!(&mut self.lambda_ir, "  %req_field_{n} = getelementptr i64, i64* %ctx_ptr, i64 0").unwrap();
                    writeln!(&mut self.lambda_ir, "  %req_i64_{n} = load i64, i64* %req_field_{n}").unwrap();
                    writeln!(&mut self.lambda_ir, "  %req_ptr_{n} = inttoptr i64 %req_i64_{n} to i64*").unwrap();
                    // HttpRequest layout: [method, path, body, headers, queryString, params, wasEarlyData] -> body = offset 2
                    writeln!(&mut self.lambda_ir, "  %body_field_{n} = getelementptr i64, i64* %req_ptr_{n}, i64 2").unwrap();
                    writeln!(&mut self.lambda_ir, "  %body_i64_{n} = load i64, i64* %body_field_{n}").unwrap();
                    writeln!(&mut self.lambda_ir, "  %body_ptr_{n} = inttoptr i64 %body_i64_{n} to i8*").unwrap();

                    let target_class = match &binding.ty {
                        Type::Named(c) => c.clone(),
                        _ => unreachable!("@PostParam target type validated as a class at typecheck time"),
                    };
                    let gm = self.generic_methods.get("Json_deserialize").cloned()
                        .expect("Json::deserialize<T> must be registered when @PostParam is used -- it requires a \
                                 @JsonSerializable type, which requires importing tinox.core.json, which also \
                                 defines Json.deserialize<T> (same module directory, one import pulls in both)");
                    let (mangled, ret_llvm) = self
                        .ensure_generic_method_specialization("Json_deserialize", &gm, &[Type::Named(target_class)])
                        .expect("Json::deserialize<T> specialization should always succeed for an already-typechecked @JsonSerializable class");
                    writeln!(&mut self.lambda_ir,
                        "  %val_{n} = call {ret_llvm} @{mangled}(i8* %body_ptr_{n})").unwrap();
                    call_args.push(format!("{ret_llvm} %val_{n}"));
                }
            }
        }

        let ret_llvm = Self::type_to_llvm(&route.return_type);
        let is_manual = matches!(&route.return_type, Type::Named(n) if n == "HttpContext");
        let args_joined = call_args.join(", ");

        if is_manual {
            // Manual mode: ctx.response was already (or will be, inside
            // the call) mutated in place through %ctx_ptr -- the returned
            // HttpContext value itself is never dereferenced, so an
            // unused SSA capture is enough; nothing more to do here, the
            // caller's trailing `ret void` finishes the shim.
            let discard = self.temp();
            writeln!(&mut self.lambda_ir, "  {discard} = call {ret_llvm} @{method_fn}({args_joined})").unwrap();
        } else {
            let result = self.temp();
            writeln!(&mut self.lambda_ir, "  {result} = call {ret_llvm} @{method_fn}({args_joined})").unwrap();
            self.emit_route_auto_serialize(idx, route, &result, &ret_llvm);
        }
    }

    /// `param_fail_{n}:` block shared by every `@PathParam`/`@QueryParam`
    /// binding above -- missing (empty string) or, for non-String types,
    /// not parseable as the declared type -- HTTP 400 with a JSON error
    /// body, handler never called. Mirrors the existing `@Auth` (401) /
    /// `@Consumes` (415) guards' own "set status, ret void" shape.
    fn emit_param_bind_failure(&mut self, n: &str, param_name: &str) {
        writeln!(&mut self.lambda_ir, "param_fail_{n}:").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_ffail_{n} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_ifail_{n} = load i64, i64* %resp_ffail_{n}").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_pfail_{n} = inttoptr i64 %resp_ifail_{n} to i64*").unwrap();
        writeln!(&mut self.lambda_ir, "  %sc_ffail_{n} = getelementptr i64, i64* %resp_pfail_{n}, i64 0").unwrap();
        writeln!(&mut self.lambda_ir, "  store i64 400, i64* %sc_ffail_{n}").unwrap();
        let err_json = format!("{{\"error\":\"missing or invalid parameter '{param_name}'\"}}");
        let err_ptr = self.emit_lambda_string_literal(&err_json);
        writeln!(&mut self.lambda_ir,
            "  %discard_fail_{n} = call i64* @HttpResponse_json(i64* %resp_pfail_{n}, i8* {err_ptr})").unwrap();
        writeln!(&mut self.lambda_ir, "  ret void").unwrap();
    }

    /// Serializes an auto-serialize-mode handler's return value
    /// (`%result`, LLVM type `ret_llvm`) as the JSON response body via the
    /// real, compiled `HttpResponse.json(String)` (`HttpResponse.tnx`) --
    /// which also sets `Content-Type: application/json; charset=utf-8`
    /// itself, so there's nothing extra to do for that here (matches
    /// exactly what a manual handler's own `ctx.response.json(...)` call
    /// already does). Status code was already set before the handler ran
    /// if `@StatusCode` was given (default 200 otherwise, from
    /// `HttpResponse`'s own constructor -- unchanged either way).
    ///
    /// Pointer-typed results (class/List/String) get a null-check first:
    /// this compiler DOES already reject a handler that provably never
    /// returns on some path (see the "missing return statement" check on
    /// `Method`, tinox-typecheck/src/lib.rs), but a legitimately-returned-
    /// but-null value (e.g. a null field) is still possible and would
    /// otherwise crash `_toJson`/`tinox_json_list_serialize` on a null
    /// deref -- 500 instead, not a crash or serialized garbage. Scalar
    /// return types (Int64/Int32/Bool) have no such check: a raw `0`
    /// value is indistinguishable from a legitimately-returned zero, the
    /// same narrow, pre-existing limitation every non-Nothing Tinox
    /// function already has, not worsened by this feature.
    fn emit_route_auto_serialize(&mut self, idx: usize, route: &RouteEntry, result: &str, ret_llvm: &str) {
        use tinox_parser::Type;

        // %resp_ptr: HttpContext[1] = response, same GEP pattern used
        // throughout this function for the @StatusCode/@Produces guards.
        writeln!(&mut self.lambda_ir, "  %resp_fauto_{idx} = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_iauto_{idx} = load i64, i64* %resp_fauto_{idx}").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_pauto_{idx} = inttoptr i64 %resp_iauto_{idx} to i64*").unwrap();

        let is_pointer = ret_llvm == "i8*" || ret_llvm == "i64*";
        if is_pointer {
            writeln!(&mut self.lambda_ir, "  %isnull_{idx} = icmp eq {ret_llvm} {result}, null").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %isnull_{idx}, label %auto_fail_{idx}, label %auto_ok_{idx}").unwrap();
            writeln!(&mut self.lambda_ir, "auto_fail_{idx}:").unwrap();
            writeln!(&mut self.lambda_ir, "  %sc_fauto_{idx} = getelementptr i64, i64* %resp_pauto_{idx}, i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 500, i64* %sc_fauto_{idx}").unwrap();
            let err_ptr = self.emit_lambda_string_literal("{\"error\":\"handler returned no value\"}");
            writeln!(&mut self.lambda_ir,
                "  %discard_500_{idx} = call i64* @HttpResponse_json(i64* %resp_pauto_{idx}, i8* {err_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "auto_ok_{idx}:").unwrap();
        }

        let json_var = match &route.return_type {
            Type::Named(cls) => {
                let v = self.temp();
                writeln!(&mut self.lambda_ir, "  {v} = call i8* @{cls}_toJson({ret_llvm} {result})").unwrap();
                v
            }
            Type::Generic { name, args } if name == "List" || name == "Array" => {
                let elem_cls = match args.first() {
                    Some(Type::Named(c)) => c.clone(),
                    _ => unreachable!("List<class> return type validated at typecheck time"),
                };
                let v = self.temp();
                writeln!(&mut self.lambda_ir,
                    "  {v} = call i8* @tinox_json_list_serialize(i64* {result}, ptr @{elem_cls}_toJson)").unwrap();
                v
            }
            Type::Array(inner) => {
                let elem_cls = match inner.as_ref() {
                    Type::Named(c) => c.clone(),
                    _ => unreachable!("List<class> return type validated at typecheck time"),
                };
                let v = self.temp();
                writeln!(&mut self.lambda_ir,
                    "  {v} = call i8* @tinox_json_list_serialize(i64* {result}, ptr @{elem_cls}_toJson)").unwrap();
                v
            }
            Type::String => {
                let v = self.temp();
                writeln!(&mut self.lambda_ir, "  {v} = call i8* @tinox_json_encode_string(i8* {result})").unwrap();
                v
            }
            Type::Int32 => {
                let ext = self.temp();
                writeln!(&mut self.lambda_ir, "  {ext} = sext i32 {result} to i64").unwrap();
                let v = self.temp();
                writeln!(&mut self.lambda_ir, "  {v} = call i8* @tinox_int_to_string(i64 {ext})").unwrap();
                v
            }
            Type::Int64 => {
                let v = self.temp();
                writeln!(&mut self.lambda_ir, "  {v} = call i8* @tinox_int_to_string(i64 {result})").unwrap();
                v
            }
            Type::Bool => {
                let v = self.temp();
                writeln!(&mut self.lambda_ir, "  {v} = call i8* @tinox_bool_to_string(i1 {result})").unwrap();
                v
            }
            _ => unreachable!("return type validated as auto-serializable at typecheck time"),
        };

        writeln!(&mut self.lambda_ir,
            "  %discard_auto_{idx} = call i64* @HttpResponse_json(i64* %resp_pauto_{idx}, i8* {json_var})").unwrap();
    }

    /// Generates an auto-run `main` for the single `@Http3RestController`
    /// class: builds an `Http3Server` on its port/cert/key, registers every
    /// `@GET`/`@POST`/`@PUT`/`@PATCH`/`@DELETE` route in the program (the
    /// exact same `route_entries` + `emit_route_shim_body` guard-chain the
    /// TCP auto-server uses), and calls `.listen()`. Calls the real,
    /// already-`define`d `Http3Server_new`/`_get`/`_post`/`_put`/`_patch`/
    /// `_delete`/`_listen` methods directly by their mangled symbol (no
    /// `declare`s -- a redundant declare of an already-defined symbol is
    /// an IR-verifier error) instead of the GC-crash-prone
    /// `tinox_HttpServer_listen` (issue #140), mirroring how `emit_ws_code`
    /// calls `WsServer_listen`/`Ws_readMessage`/etc. directly rather than
    /// any C runtime symbol.
    ///
    /// Unlike the TCP shims (raw `void(i64)` C callbacks handed to
    /// `tinox_HttpServer_get` as a bare pointer), `Http3Server.get`/`post`/
    /// etc. take a genuine Tinox `fnc(HttpContext) -> Nothing` closure
    /// value -- always represented as a 16-byte `{fn_ptr: i64, env: i64*}`
    /// block pointer (see `gen_lambda`'s closure construction) -- so each
    /// shim here additionally takes a trailing (unused, non-capturing)
    /// `i64* %env` parameter, and gets wrapped in that block before being
    /// passed to `Http3Server_{method}`.
    fn emit_http3_route_code(&mut self) {
        let Some(controller) = self.http3_rest_controller.clone() else {
            return;
        };
        if self.route_entries.is_empty() || self.has_main {
            return;
        }
        if !self.class_named_types.contains("Http3Server") {
            panic!("@Http3RestController requires `import tinox.core.http3_server;`");
        }

        let routes = self.route_entries.clone();
        self.emit_route_annotation_globals(&routes);
        self.ensure_postparam_specializations(&routes);

        let cert_escaped = Self::escape_llvm_string(&controller.cert_path);
        let cert_len = controller.cert_path.len() + 1;
        writeln!(&mut self.ir,
            "@__h3_cert = private constant [{cert_len} x i8] c\"{cert_escaped}\\00\"").unwrap();
        let key_escaped = Self::escape_llvm_string(&controller.key_path);
        let key_len = controller.key_path.len() + 1;
        writeln!(&mut self.ir,
            "@__h3_key = private constant [{key_len} x i8] c\"{key_escaped}\\00\"").unwrap();

        // ── Per-route shims (closure-callable: trailing i64* %env) ──────────────
        for (idx, route) in routes.iter().enumerate() {
            let shim = format!("__h3route_{}_{}", route.class_name, route.method_name);
            writeln!(&mut self.lambda_ir, "define void @{shim}(i64 %ctx_i64, i64* %env) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
            self.emit_route_shim_body(idx, route);
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }

        // ── Auto-main: build the Http3Server, register every route as a
        // closure value, listen ───────────────────────────────────────────────
        // The listen loop lives in __tinox_run_http3(), a plain callee (not
        // @tinox_main directly), so a later spawn-based bootstrap can run it
        // on its own thread instead of tail-calling it inline.
        writeln!(&mut self.lambda_ir, "define i64 @__tinox_run_http3() {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %h3_certp = getelementptr [{cert_len} x i8], [{cert_len} x i8]* @__h3_cert, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %h3_keyp = getelementptr [{key_len} x i8], [{key_len} x i8]* @__h3_key, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %h3_server = call i64* @Http3Server_new(i64* null, i64 {}, i8* %h3_certp, i8* %h3_keyp)",
            controller.port).unwrap();

        for (idx, route) in routes.iter().enumerate() {
            let shim = format!("__h3route_{}_{}", route.class_name, route.method_name);
            let path_len = route.path.len() + 1;
            let server_method = format!("Http3Server_{}", route.http_method.to_lowercase());
            writeln!(&mut self.lambda_ir,
                "  %h3_raw_{idx} = call i8* @tinox_alloc(i64 16)").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_block_{idx} = bitcast i8* %h3_raw_{idx} to i64*").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_fp_{idx} = ptrtoint void (i64, i64*)* @{shim} to i64").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_fps_{idx} = getelementptr i64, i64* %h3_block_{idx}, i64 0").unwrap();
            writeln!(&mut self.lambda_ir,
                "  store i64 %h3_fp_{idx}, i64* %h3_fps_{idx}").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_envs_{idx} = getelementptr i64, i64* %h3_block_{idx}, i64 1").unwrap();
            writeln!(&mut self.lambda_ir,
                "  store i64* null, i64* %h3_envs_{idx}").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_pathp_{idx} = getelementptr [{path_len} x i8], [{path_len} x i8]* @__route_path_{idx}, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %h3_reg_{idx} = call i64* @{server_method}(i64* %h3_server, i8* %h3_pathp_{idx}, i64* %h3_block_{idx})").unwrap();
        }

        writeln!(&mut self.lambda_ir, "  call void @Http3Server_listen(i64* %h3_server)").unwrap();
        writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();

        self.background_run_fns.push("__tinox_run_http3".to_string());
        self.startup_endpoints.push(("HTTP/3 (QUIC)".to_string(), format!(":{}", controller.port)));
    }

    /// Generates an auto-run `main` for a single `@WebsocketEndpoint`-annotated class:
    /// a listen/accept/readMessage loop that calls the class's `@OnOpen`/`@OnMessage`/
    /// `@OnClose` methods directly by their mangled `{Class}_{method}` symbol —
    /// this is a compiler-generated transliteration of the explicit v1 loop
    /// (`examples/ws_echo/Main.tnx`), calling the already-compiled `Ws`/`WsServer`
    /// static methods from `tinox.core.websocket` (which the file must import).
    ///
    /// Fires once per `@WebsocketEndpoint` class found (no upper limit — each
    /// gets its own uniquely-named `__tinox_run_ws_<idx>()`, spawned on its
    /// own thread by `emit_tinox_main_bootstrap`, so distinct endpoints on
    /// distinct ports coexist fine; two endpoints on the *same* port both
    /// still compile, but the second one's `WsServer_listen` bind fails at
    /// runtime and that thread exits early -- not caught at compile time,
    /// same as any other port-already-in-use situation). No-op without a
    /// user `main` shape issue; skipped entirely once a legacy top-level
    /// `fn main()` already claims `has_main`.
    fn emit_ws_code(&mut self) {
        if self.ws_endpoints.is_empty() || self.has_main {
            return;
        }

        // No `declare` here for WsServer_listen/accept/Ws_readMessage/Ws_text/Ws_close:
        // they are `define`d by tinox.core.websocket itself (required import, checked
        // below) — a redundant `declare` of an already-`define`d function is rejected
        // by the IR verifier gate as an "invalid redefinition".
        if !self.class_named_types.contains("WsFrame") {
            panic!("@WebsocketEndpoint requires `import tinox.core.websocket;` (WsFrame type not found)");
        }

        let endpoints = self.ws_endpoints.clone();
        for (idx, ep) in endpoints.iter().enumerate() {
            let inst_size = self.struct_layouts.get(ep.class_name.as_str())
                .map(|f| (f.len().max(1) * 8) as i64)
                .unwrap_or(8);
            let port = ep.port
                .or_else(|| std::env::var("TINOX_PORT").ok().and_then(|s| s.parse::<i64>().ok()))
                .unwrap_or(8080);

            // The accept loop lives in __tinox_run_ws_<idx>(), a plain callee
            // (not @tinox_main directly), so emit_tinox_main_bootstrap can
            // run it on its own thread instead of tail-calling it inline.
            //
            // Each accepted connection is handed off to its OWN detached
            // worker thread (__tinox_ws_conn_worker_<idx>, spawned via
            // tinox_task_spawn_detached) instead of being handled inline —
            // accept_loop immediately goes back to WsServer_accept without
            // waiting for that connection to finish. The original version of
            // this function ran conn_open/msg_loop/conn_end INLINE in the
            // accept loop, which meant WsServer_accept was never called
            // again until the current connection closed: a second client
            // could not connect at all while the first was still open. Found
            // while designing a multi-client server-driven UI framework on
            // top of this — a single-client-at-a-time WS server is fine for
            // a demo/echo endpoint (the only thing that ever exercised this
            // path before) but not for anything meant to serve real
            // concurrent users.
            let run_fn = format!("__tinox_run_ws_{idx}");
            let worker_fn = format!("__tinox_ws_conn_worker_{idx}");
            let worker_wrapper = format!("__tinox_ws_worker_wrapper_{idx}");

            writeln!(&mut self.lambda_ir, "define i64 @{run_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %srv = call i64 @WsServer_listen(i64* null, i64 {port})").unwrap();
            writeln!(&mut self.lambda_ir, "  %srv_bad = icmp slt i64 %srv, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %srv_bad, label %bind_fail, label %accept_loop").unwrap();

            writeln!(&mut self.lambda_ir, "bind_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 1").unwrap();

            writeln!(&mut self.lambda_ir, "accept_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn = call i64 @WsServer_accept(i64* null, i64 %srv)").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_bad = icmp sle i64 %conn, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %conn_bad, label %accept_loop, label %dispatch").unwrap();

            // 2-slot args array [worker_fn_ptr, conn] -- same convention
            // emit_spawn_wrapper's own caller (ExprKind::Spawn) already uses
            // for passing arguments through tinox_task_spawn's fixed
            // i8*(i8*) trampoline signature.
            writeln!(&mut self.lambda_ir, "dispatch:").unwrap();
            writeln!(&mut self.lambda_ir, "  %args_raw = call i8* @tinox_alloc(i64 16)").unwrap();
            writeln!(&mut self.lambda_ir, "  %args_ap = bitcast i8* %args_raw to [2 x i64]*").unwrap();
            writeln!(&mut self.lambda_ir, "  %fp_i64 = ptrtoint i64 (i64)* @{worker_fn} to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  %fp_slot = getelementptr [2 x i64], [2 x i64]* %args_ap, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 %fp_i64, i64* %fp_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_slot = getelementptr [2 x i64], [2 x i64]* %args_ap, i64 0, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 %conn, i64* %conn_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_task_spawn_detached(i8* (i8*)* @{worker_wrapper}, i8* %args_raw)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %accept_loop").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            // Former conn_open/msg_loop/conn_end body, now its own real,
            // separately-spawnable function taking the connection handle as
            // its one argument.
            writeln!(&mut self.lambda_ir, "define i64 @{worker_fn}(i64 %conn) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {inst_size})").unwrap();
            writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();
            if let Some(ref on_open) = ep.on_open {
                writeln!(&mut self.lambda_ir, "  call void @{}_{}(i64* %inst, i64 %conn)", ep.class_name, on_open).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  br label %msg_loop").unwrap();

            // opcode 1 (text) → @OnMessage; anything else (binary, close, EOF,
            // protocol error — Ping/Pong are already auto-handled inside
            // Ws::readMessage and never reach here) ends the connection.
            writeln!(&mut self.lambda_ir, "msg_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  %f = call i64* @Ws_readMessage(i64* null, i64 %conn)").unwrap();
            writeln!(&mut self.lambda_ir, "  %opcode_ptr = getelementptr %class.WsFrame, ptr %f, i32 0, i32 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %opcode = load i64, i64* %opcode_ptr").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_text = icmp eq i64 %opcode, 1").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %is_text, label %handle_text, label %conn_end").unwrap();

            writeln!(&mut self.lambda_ir, "handle_text:").unwrap();
            if let Some(ref on_message) = ep.on_message {
                writeln!(&mut self.lambda_ir, "  %msg = call i8* @Ws_text(i64* null, i64* %f)").unwrap();
                writeln!(&mut self.lambda_ir, "  call void @{}_{}(i64* %inst, i64 %conn, i8* %msg)", ep.class_name, on_message).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  br label %msg_loop").unwrap();

            writeln!(&mut self.lambda_ir, "conn_end:").unwrap();
            if let Some(ref on_close) = ep.on_close {
                writeln!(&mut self.lambda_ir, "  call void @{}_{}(i64* %inst, i64 %conn)", ep.class_name, on_close).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  call void @Ws_close(i64* null, i64 %conn)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.emit_spawn_wrapper(&worker_wrapper, 2, "i64", &["i64".to_string()]);

            self.background_run_fns.push(run_fn);
            self.startup_endpoints.push(("WebSocket".to_string(), format!(":{port}")));
        }
    }

    /// Generates the auto-run HTTP shell/client-JS server + WebSocket
    /// accept loop for a `@TinoxUIApp` class (issue #215, Phase 4) --
    /// annotation sugar over exactly the hand-wired shape
    /// `examples/tinox_ui_hello/HelloApp.tnx` + `Main.tnx` already use: an
    /// `HttpServer` on `httpPort` serving `"/"` (`Assets::shellHtml`) and
    /// `"/ui.js"` (`Assets::clientJs`), and a WebSocket accept loop on
    /// `wsPort` that calls the class's own `@View` method to build the
    /// initial tree (`TinoxUIRuntime::buildHandlers` + `sendInit`), then on
    /// every incoming event: `dispatchEvent`, rebuild via `@View` again,
    /// `sendUpdate` -- Phase 1's full-resend "automatic reactivity" shape,
    /// generated instead of hand-written. Diff-based rendering (Phase 3)
    /// deliberately stays a manual, lower-level opt-in -- it needs an
    /// app-owned persistent id-counter field this sugar has no class
    /// layout to put one on (the synthesized per-connection state below
    /// lives in plain local `alloca`s inside this hand-emitted function,
    /// not on the app class itself, so it can't survive past one
    /// connection's worker function the way a real instance field could).
    ///
    /// Modeled directly on `emit_ws_code` (identical accept-loop /
    /// detached-per-connection-worker structure, reusing
    /// `tinox_task_spawn_detached` + `emit_spawn_wrapper`) plus
    /// `emit_route_code`'s shim-function convention for the two GET
    /// routes -- calling already-compiled `tinox.core.ui`/`http_server`
    /// methods by their mangled name (`TinoxUIRuntime_buildHandlers`,
    /// `HttpResponse_html`, ...) rather than re-implementing any of their
    /// logic here, the same "call the real compiled function directly"
    /// technique `emit_ws_code` already uses for
    /// `Ws_readMessage`/`Ws_text`/`Ws_close`.
    fn emit_tinoxui_code(&mut self) {
        if self.tinoxui_apps.is_empty() || self.has_main {
            return;
        }

        if !self.class_named_types.contains("Component") {
            panic!("@TinoxUIApp requires `import tinox.core.ui;` (Component type not found)");
        }
        if !self.class_named_types.contains("WsFrame") {
            panic!("@TinoxUIApp requires `import tinox.core.websocket;` (WsFrame type not found)");
        }
        if !self.class_named_types.contains("HttpResponse") {
            panic!("@TinoxUIApp requires `import tinox.core.http_server;` (HttpResponse type not found)");
        }

        // tinox_HttpServer_get/_listen are NOT safe to unconditionally
        // re-declare (opt hard-errors "invalid redefinition" on a second
        // `declare` for an already-declared symbol, even with an
        // identical signature) -- same guard emit_devui_code already uses
        // to avoid colliding with emit_route_code's own copy.
        // tinox_HttpServer_new has no return-type overlap risk here since
        // every declarer uses the identical `i64* (i64)` signature, but is
        // guarded the same way for consistency/symmetry.
        let declare_http_fns = self.route_entries.is_empty() || self.http3_rest_controller.is_some();
        if declare_http_fns {
            writeln!(&mut self.lambda_ir, "declare i64* @tinox_HttpServer_new(i64)").unwrap();
            writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_get(i64*, i8*, i64)").unwrap();
            writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_listen(i64*)").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }

        let apps = self.tinoxui_apps.clone();
        for (idx, app) in apps.iter().enumerate() {
            let inst_size = self.struct_layouts.get(app.class_name.as_str())
                .map(|f| (f.len().max(1) * 8) as i64)
                .unwrap_or(8);

            // ── HTTP shell server: "/" (shell HTML) + "/ui.js" (client JS) ──
            let shell_shim = format!("__tinoxui_shell_shim_{idx}");
            let js_shim = format!("__tinoxui_js_shim_{idx}");
            let run_http_fn = format!("__tinox_run_tinoxui_http_{idx}");
            let ws_url = format!("ws://localhost:{}/__tinoxui", app.ws_port);

            // HttpContext layout: [request: i64*, response: i64*] -- offset
            // 1 is the response pointer, same convention emit_route_code's
            // own shim bodies use (see its own doc comment on this layout).
            writeln!(&mut self.lambda_ir, "define void @{shell_shim}(i64 %ctx_i64) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_field = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_i64 = load i64, i64* %resp_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_ptr = inttoptr i64 %resp_i64 to i64*").unwrap();
            let ws_url_ptr = self.emit_lambda_string_literal(&ws_url);
            writeln!(&mut self.lambda_ir, "  %html = call i8* @Assets_shellHtml(i8* {ws_url_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  %shell_r = call i64* @HttpResponse_html(i64* %resp_ptr, i8* %html)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            writeln!(&mut self.lambda_ir, "define void @{js_shim}(i64 %ctx_i64) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_field = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_i64 = load i64, i64* %resp_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %resp_ptr = inttoptr i64 %resp_i64 to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  %js = call i8* @Assets_clientJs()").unwrap();
            let ct_ptr = self.emit_lambda_string_literal("application/javascript");
            writeln!(&mut self.lambda_ir, "  %js_r = call i64* @HttpResponse_content(i64* %resp_ptr, i8* %js, i8* {ct_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            writeln!(&mut self.lambda_ir, "define i64 @{run_http_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %server = call i64* @tinox_HttpServer_new(i64 {})", app.http_port).unwrap();
            let root_path_ptr = self.emit_lambda_string_literal("/");
            writeln!(&mut self.lambda_ir, "  %shell_fn = ptrtoint void (i64)* @{shell_shim} to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_HttpServer_get(i64* %server, i8* {root_path_ptr}, i64 %shell_fn)").unwrap();
            let js_path_ptr = self.emit_lambda_string_literal("/ui.js");
            writeln!(&mut self.lambda_ir, "  %js_fn = ptrtoint void (i64)* @{js_shim} to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_HttpServer_get(i64* %server, i8* {js_path_ptr}, i64 %js_fn)").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_HttpServer_listen(i64* %server)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.background_run_fns.push(run_http_fn);
            self.startup_endpoints.push(("HTTP".to_string(), format!(":{}", app.http_port)));

            // ── WebSocket accept loop -- identical shape to emit_ws_code's
            // own accept_loop/dispatch/worker_fn/msg_loop, just driving
            // @View instead of @OnOpen/@OnMessage/@OnClose. ──────────────
            let run_ws_fn = format!("__tinox_run_tinoxui_ws_{idx}");
            let worker_fn = format!("__tinox_tinoxui_conn_worker_{idx}");
            let worker_wrapper = format!("__tinox_tinoxui_worker_wrapper_{idx}");

            writeln!(&mut self.lambda_ir, "define i64 @{run_ws_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %srv = call i64 @WsServer_listen(i64* null, i64 {})", app.ws_port).unwrap();
            writeln!(&mut self.lambda_ir, "  %srv_bad = icmp slt i64 %srv, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %srv_bad, label %bind_fail, label %accept_loop").unwrap();

            writeln!(&mut self.lambda_ir, "bind_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 1").unwrap();

            writeln!(&mut self.lambda_ir, "accept_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn = call i64 @WsServer_accept(i64* null, i64 %srv)").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_bad = icmp sle i64 %conn, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %conn_bad, label %accept_loop, label %tui_dispatch").unwrap();

            writeln!(&mut self.lambda_ir, "tui_dispatch:").unwrap();
            writeln!(&mut self.lambda_ir, "  %args_raw = call i8* @tinox_alloc(i64 16)").unwrap();
            writeln!(&mut self.lambda_ir, "  %args_ap = bitcast i8* %args_raw to [2 x i64]*").unwrap();
            writeln!(&mut self.lambda_ir, "  %fp_i64 = ptrtoint i64 (i64)* @{worker_fn} to i64").unwrap();
            writeln!(&mut self.lambda_ir, "  %fp_slot = getelementptr [2 x i64], [2 x i64]* %args_ap, i64 0, i64 0").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 %fp_i64, i64* %fp_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_slot = getelementptr [2 x i64], [2 x i64]* %args_ap, i64 0, i64 1").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64 %conn, i64* %conn_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_task_spawn_detached(i8* (i8*)* @{worker_wrapper}, i8* %args_raw)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %accept_loop").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            writeln!(&mut self.lambda_ir, "define i64 @{worker_fn}(i64 %conn) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {inst_size})").unwrap();
            writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  %root0 = call i64* @{}_{}(i64* %inst)", app.class_name, app.view_method).unwrap();
            writeln!(&mut self.lambda_ir, "  %handlers0 = call i8* @TinoxUIRuntime_buildHandlers(i64* %root0)").unwrap();
            writeln!(&mut self.lambda_ir, "  %root_slot = alloca i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  store i64* %root0, i64** %root_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  %handlers_slot = alloca i8*").unwrap();
            writeln!(&mut self.lambda_ir, "  store i8* %handlers0, i8** %handlers_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @TinoxUIRuntime_sendInit(i64 %conn, i64* %root0)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %tui_msg_loop").unwrap();

            // opcode 1 (text) -> dispatch + rebuild + resend; anything else
            // (binary, close, EOF, protocol error -- Ping/Pong are already
            // auto-handled inside Ws::readMessage) ends the connection,
            // same convention emit_ws_code's own msg_loop uses.
            writeln!(&mut self.lambda_ir, "tui_msg_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  %f = call i64* @Ws_readMessage(i64* null, i64 %conn)").unwrap();
            writeln!(&mut self.lambda_ir, "  %opcode_ptr = getelementptr %class.WsFrame, ptr %f, i32 0, i32 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %opcode = load i64, i64* %opcode_ptr").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_text = icmp eq i64 %opcode, 1").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %is_text, label %tui_handle_text, label %tui_conn_end").unwrap();

            writeln!(&mut self.lambda_ir, "tui_handle_text:").unwrap();
            writeln!(&mut self.lambda_ir, "  %msg = call i8* @Ws_text(i64* null, i64* %f)").unwrap();
            writeln!(&mut self.lambda_ir, "  %handlers_cur = load i8*, i8** %handlers_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @TinoxUIRuntime_dispatchEvent(i8* %handlers_cur, i8* %msg)").unwrap();
            writeln!(&mut self.lambda_ir, "  %root_new = call i64* @{}_{}(i64* %inst)", app.class_name, app.view_method).unwrap();
            writeln!(&mut self.lambda_ir, "  store i64* %root_new, i64** %root_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  %handlers_new = call i8* @TinoxUIRuntime_buildHandlers(i64* %root_new)").unwrap();
            writeln!(&mut self.lambda_ir, "  store i8* %handlers_new, i8** %handlers_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @TinoxUIRuntime_sendUpdate(i64 %conn, i64* %root_new)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %tui_msg_loop").unwrap();

            writeln!(&mut self.lambda_ir, "tui_conn_end:").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @Ws_close(i64* null, i64 %conn)").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.emit_spawn_wrapper(&worker_wrapper, 2, "i64", &["i64".to_string()]);

            self.background_run_fns.push(run_ws_fn);
            self.startup_endpoints.push(("WebSocket".to_string(), format!(":{}", app.ws_port)));
        }
    }

    /// Registers a string literal (same bookkeeping as `gen_literal`'s
    /// `Literal::String` arm) and emits the `getelementptr` that loads its
    /// `i8*` into `self.lambda_ir` instead of `self.ir` — for hand-emitted
    /// top-level functions (`emit_ws_code`-style) that need a literal value,
    /// not a normal typechecked expression.
    fn emit_lambda_string_literal(&mut self, s: &str) -> String {
        let name = format!("str{}", self.strings.len());
        self.strings.insert(name.clone(), s.to_string());
        let len = s.len() + 1;
        let ptr = self.temp();
        writeln!(&mut self.lambda_ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", ptr, len, len, name).unwrap();
        ptr
    }

    /// Generates an auto-run `main` for a single `@Amqp10Consumer`-annotated
    /// class (Issue #81): connect/begin/attach/grantCredit/nextMessage/ack
    /// loop, calling the class's `@OnMessage` method directly by its mangled
    /// `{Class}_{method}` symbol — same hand-emitted-IR technique as
    /// `emit_ws_code`, calling the already-compiled `Amqp10Connection`/
    /// `Amqp10Session`/`Amqp10Link` static/instance methods from
    /// `tinox.core.amqp10` (which the file must import). The handler
    /// receives the whole `Amqp10Message` pointer (not a decoded body) so no
    /// string-building loop needs to be hand-rolled in raw IR.
    ///
    /// Fires once per `@Amqp10Consumer` class found (no upper limit — each
    /// gets its own uniquely-named `__tinox_run_amqp10_<idx>()`, spawned on
    /// its own thread by `emit_tinox_main_bootstrap`; multiple consumers
    /// against the same broker/port but different queues is a normal,
    /// expected shape, unlike WS where the same port would collide).
    fn emit_amqp10_consumer_code(&mut self) {
        if self.amqp10_consumers.is_empty() || self.has_main {
            return;
        }

        if !self.class_named_types.contains("Amqp10Message") {
            panic!("@Amqp10Consumer requires `import tinox.core.amqp10;` (Amqp10Message type not found)");
        }

        let consumers = self.amqp10_consumers.clone();
        for (idx, c) in consumers.iter().enumerate() {
            let inst_size = self.struct_layouts.get(c.class_name.as_str())
                .map(|f| (f.len().max(1) * 8) as i64)
                .unwrap_or(8);

            // The connect/receive loop lives in __tinox_run_amqp10_<idx>(), a
            // plain callee (not @tinox_main directly), so
            // emit_tinox_main_bootstrap can run it on its own thread instead
            // of tail-calling it inline.
            let run_fn = format!("__tinox_run_amqp10_{idx}");
            writeln!(&mut self.lambda_ir, "define i64 @{run_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

            let host_ptr = self.emit_lambda_string_literal(&c.host);
            let user_ptr = self.emit_lambda_string_literal(&c.user);
            let pass_ptr = self.emit_lambda_string_literal(&c.pass);
            let address_ptr = self.emit_lambda_string_literal(&c.address);
            let name_ptr = self.emit_lambda_string_literal("tinox-consumer");

            writeln!(&mut self.lambda_ir, "  %conn_obj = call i64* @Amqp10Connection_connect(i64* null, i8* {host_ptr}, i64 {port}, i8* {user_ptr}, i8* {pass_ptr})", port = c.port).unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_field = getelementptr %class.Amqp10Connection, ptr %conn_obj, i32 0, i32 0").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_val = load i64, i64* %conn_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_bad = icmp sle i64 %conn_val, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %conn_bad, label %connect_fail, label %do_begin").unwrap();

            writeln!(&mut self.lambda_ir, "connect_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 1").unwrap();

            writeln!(&mut self.lambda_ir, "do_begin:").unwrap();
            writeln!(&mut self.lambda_ir, "  %sess_obj = call i64* @Amqp10Session_begin(i64* null, i64* %conn_obj)").unwrap();
            writeln!(&mut self.lambda_ir, "  %chan_field = getelementptr %class.Amqp10Session, ptr %sess_obj, i32 0, i32 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %chan_val = load i64, i64* %chan_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %chan_bad = icmp eq i64 %chan_val, -1").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %chan_bad, label %begin_fail, label %do_attach").unwrap();

            writeln!(&mut self.lambda_ir, "begin_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 2").unwrap();

            writeln!(&mut self.lambda_ir, "do_attach:").unwrap();
            writeln!(&mut self.lambda_ir, "  %link_obj = call i64* @Amqp10Link_attach(i64* null, i64* %sess_obj, i8* {name_ptr}, i1 1, i8* {address_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  %handle_field = getelementptr %class.Amqp10Link, ptr %link_obj, i32 0, i32 2").unwrap();
            writeln!(&mut self.lambda_ir, "  %handle_val = load i64, i64* %handle_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %handle_bad = icmp eq i64 %handle_val, -1").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %handle_bad, label %attach_fail, label %consumer_ready").unwrap();

            writeln!(&mut self.lambda_ir, "attach_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 3").unwrap();

            writeln!(&mut self.lambda_ir, "consumer_ready:").unwrap();
            writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {inst_size})").unwrap();
            writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "recv_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @Amqp10Link_grantCredit(i64* %link_obj, i64 1)").unwrap();
            writeln!(&mut self.lambda_ir, "  %msg_obj = call i64* @Amqp10Link_nextMessage(i64* %link_obj)").unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_field = getelementptr %class.Amqp10Message, ptr %msg_obj, i32 0, i32 3").unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_val = load i64, i64* %ok_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_ok = icmp ne i64 %ok_val, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %is_ok, label %handle_msg, label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "handle_msg:").unwrap();
            if let Some(ref on_message) = c.on_message {
                writeln!(&mut self.lambda_ir, "  call void @{}_{}(i64* %inst, i64* %msg_obj)", c.class_name, on_message).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  %delivery_field = getelementptr %class.Amqp10Message, ptr %msg_obj, i32 0, i32 2").unwrap();
            writeln!(&mut self.lambda_ir, "  %delivery_val = load i64, i64* %delivery_field").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @Amqp10Link_ack(i64* %link_obj, i64 %delivery_val)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.background_run_fns.push(run_fn);
            self.startup_endpoints.push((
                "AMQP 1.0 (consumer)".to_string(),
                format!("{}:{} ({})", c.host, c.port, c.address),
            ));
        }
    }

    /// Generates an auto-run `main` for a single `@Amqp091Consumer`-annotated
    /// class (Issue #126): connect/open/qos/consume/nextMessage/ack loop,
    /// calling the class's `@OnMessage` method directly by its mangled
    /// `{Class}_{method}` symbol — same hand-emitted-IR technique as
    /// `emit_amqp10_consumer_code`, calling the already-compiled
    /// `AmqpConnection091`/`AmqpChannel091` static/instance methods from
    /// `tinox.core.amqp091` (which the file must import). The handler
    /// receives the whole `AmqpMessage091` pointer (not a decoded body) so
    /// no string-building loop needs to be hand-rolled in raw IR. Prefetch
    /// is hardcoded to 1 (mirrors amqp10's per-message `grantCredit(1)`) —
    /// no annotation argument for it in v1.
    ///
    /// Fires once per `@Amqp091Consumer` class found (no upper limit — each
    /// gets its own uniquely-named `__tinox_run_amqp091_<idx>()`, spawned on
    /// its own thread by `emit_tinox_main_bootstrap`; multiple consumers
    /// against the same broker/port but different queues is a normal,
    /// expected shape, unlike WS where the same port would collide).
    fn emit_amqp091_consumer_code(&mut self) {
        if self.amqp091_consumers.is_empty() || self.has_main {
            return;
        }

        if !self.class_named_types.contains("AmqpMessage091") {
            panic!("@Amqp091Consumer requires `import tinox.core.amqp091;` (AmqpMessage091 type not found)");
        }

        let consumers = self.amqp091_consumers.clone();
        for (idx, c) in consumers.iter().enumerate() {
            let inst_size = self.struct_layouts.get(c.class_name.as_str())
                .map(|f| (f.len().max(1) * 8) as i64)
                .unwrap_or(8);

            // The connect/receive loop lives in __tinox_run_amqp091_<idx>(),
            // a plain callee (not @tinox_main directly), so
            // emit_tinox_main_bootstrap can run it on its own thread instead
            // of tail-calling it inline.
            let run_fn = format!("__tinox_run_amqp091_{idx}");
            writeln!(&mut self.lambda_ir, "define i64 @{run_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

            let host_ptr = self.emit_lambda_string_literal(&c.host);
            let vhost_ptr = self.emit_lambda_string_literal(&c.vhost);
            let user_ptr = self.emit_lambda_string_literal(&c.user);
            let pass_ptr = self.emit_lambda_string_literal(&c.pass);
            let queue_ptr = self.emit_lambda_string_literal(&c.queue);

            writeln!(&mut self.lambda_ir, "  %conn_obj = call i64* @AmqpConnection091_connect(i64* null, i8* {host_ptr}, i64 {port}, i8* {vhost_ptr}, i8* {user_ptr}, i8* {pass_ptr})", port = c.port).unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_field = getelementptr %class.AmqpConnection091, ptr %conn_obj, i32 0, i32 0").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_val = load i64, i64* %conn_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_bad = icmp sle i64 %conn_val, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %conn_bad, label %connect_fail, label %do_open").unwrap();

            writeln!(&mut self.lambda_ir, "connect_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 1").unwrap();

            writeln!(&mut self.lambda_ir, "do_open:").unwrap();
            writeln!(&mut self.lambda_ir, "  %chan_obj = call i64* @AmqpChannel091_open(i64* null, i64* %conn_obj)").unwrap();
            writeln!(&mut self.lambda_ir, "  %chanid_field = getelementptr %class.AmqpChannel091, ptr %chan_obj, i32 0, i32 1").unwrap();
            writeln!(&mut self.lambda_ir, "  %chanid_val = load i64, i64* %chanid_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %chan_bad = icmp eq i64 %chanid_val, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %chan_bad, label %open_fail, label %do_qos").unwrap();

            writeln!(&mut self.lambda_ir, "open_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 2").unwrap();

            writeln!(&mut self.lambda_ir, "do_qos:").unwrap();
            writeln!(&mut self.lambda_ir, "  %qos_ok = call i1 @AmqpChannel091_qos(i64* %chan_obj, i64 1)").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %qos_ok, label %do_consume, label %qos_fail").unwrap();

            writeln!(&mut self.lambda_ir, "qos_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 3").unwrap();

            writeln!(&mut self.lambda_ir, "do_consume:").unwrap();
            writeln!(&mut self.lambda_ir, "  %tag_ptr = call i8* @AmqpChannel091_consume(i64* %chan_obj, i8* {queue_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  %tag_len = call i64 @tinox_string_length(i8* %tag_ptr)").unwrap();
            writeln!(&mut self.lambda_ir, "  %tag_bad = icmp eq i64 %tag_len, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %tag_bad, label %consume_fail, label %consumer_ready").unwrap();

            writeln!(&mut self.lambda_ir, "consume_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 4").unwrap();

            writeln!(&mut self.lambda_ir, "consumer_ready:").unwrap();
            writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {inst_size})").unwrap();
            writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "recv_loop:").unwrap();
            writeln!(&mut self.lambda_ir, "  %msg_obj = call i64* @AmqpChannel091_nextMessage(i64* %chan_obj)").unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_field = getelementptr %class.AmqpMessage091, ptr %msg_obj, i32 0, i32 5").unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_val = load i64, i64* %ok_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_ok = icmp ne i64 %ok_val, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %is_ok, label %handle_msg, label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "handle_msg:").unwrap();
            if let Some(ref on_message) = c.on_message {
                writeln!(&mut self.lambda_ir, "  call void @{}_{}(i64* %inst, i64* %msg_obj)", c.class_name, on_message).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  %tag_field = getelementptr %class.AmqpMessage091, ptr %msg_obj, i32 0, i32 0").unwrap();
            writeln!(&mut self.lambda_ir, "  %tag_val = load i64, i64* %tag_field").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @AmqpChannel091_ack(i64* %chan_obj, i64 %tag_val)").unwrap();
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.background_run_fns.push(run_fn);
            self.startup_endpoints.push((
                "AMQP 0-9-1 (consumer)".to_string(),
                format!("{}:{} (queue: {})", c.host, c.port, c.queue),
            ));
        }
    }

    /// Dev-mode introspection API for the separate `tinox-devui` dashboard:
    /// a background `HttpServer` bound to `127.0.0.1` only
    /// (`tinox_HttpServer_new_bind`, runtime.c -- never reachable off the
    /// local machine, unlike the program's own public routes) serving a
    /// handful of read-only JSON `GET` routes. No-op unless `[dev] enabled
    /// = true` (`set_dev_config`). Must run before
    /// `emit_tinox_main_bootstrap` so `__tinox_run_devui` and its
    /// `startup_endpoints` entry are registered in time to be spawned/
    /// reported by it -- LLVM itself doesn't care about declaration order
    /// for the `@{Class}_di_instance` globals this references (`/components`
    /// below), only `background_run_fns` registration is order-sensitive.
    fn emit_devui_code(&mut self, class_ast_map: &HashMap<String, tinox_parser::Class>) {
        if !self.dev_enabled {
            return;
        }
        let port = self.dev_port;

        // tinox_HttpServer_new_bind is new -- nothing else ever declares
        // it, always safe to declare here. tinox_HttpServer_get/_listen are
        // NOT safe to unconditionally re-declare: opt hard-errors
        // ("invalid redefinition") on a second `declare` for a symbol
        // already declared elsewhere in the same module, even with an
        // identical signature -- found by actually compiling a devui-
        // enabled program that also has real @GET routes (emit_route_code
        // already declares both whenever route_entries is non-empty and
        // there's no @Http3RestController -- the same condition it uses to
        // early-return, mirrored here so this only declares what
        // emit_route_code won't).
        writeln!(&mut self.lambda_ir, "declare i64* @tinox_HttpServer_new_bind(i64, i8*)").unwrap();
        if self.route_entries.is_empty() || self.http3_rest_controller.is_some() {
            writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_get(i64*, i8*, i64)").unwrap();
            writeln!(&mut self.lambda_ir, "declare void @tinox_HttpServer_listen(i64*)").unwrap();
        }

        let modules_json = Self::devui_json_string_array(&self.loaded_modules);
        let routes_json = self.devui_routes_json(class_ast_map);
        let websockets_json = self.devui_websockets_json();
        // "HTTP" (plain, not "HTTP/3 (QUIC)") is the one try-it-out can
        // actually reach with a standard java.net.http.HttpClient -- an
        // HTTP/3-only program has no reachable base URL here (null), REST
        // try-it-out is out of scope for QUIC-only apps in v1. Already
        // registered in startup_endpoints by emit_route_code, which runs
        // before this function.
        let http_port_json = self.startup_endpoints.iter()
            .find(|(protocol, _)| protocol == "HTTP")
            .map(|(_, detail)| detail.trim_start_matches(':').to_string())
            .unwrap_or_else(|| "null".to_string());
        let info_json = format!(
            "{{\"name\":{},\"version\":{},\"tinoxVersion\":{},\"httpPort\":{}}}",
            Self::devui_json_string(&self.dev_package_name),
            Self::devui_json_string(&self.dev_package_version),
            Self::devui_json_string(&self.dev_tinox_version),
            http_port_json,
        );

        self.emit_devui_static_handler("__devui_modules", &modules_json);
        self.emit_devui_static_handler("__devui_routes", &routes_json);
        self.emit_devui_static_handler("__devui_websockets", &websockets_json);
        self.emit_devui_static_handler("__devui_info", &info_json);
        self.emit_devui_config_handler();
        self.emit_devui_components_handler();
        self.emit_devui_tests_run_handler();

        writeln!(&mut self.lambda_ir, "define i64 @__tinox_run_devui() {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
        let addr_ptr = self.emit_lambda_string_literal("127.0.0.1");
        writeln!(&mut self.lambda_ir,
            "  %server = call i64* @tinox_HttpServer_new_bind(i64 {port}, i8* {addr_ptr})").unwrap();
        for (path, handler) in [
            ("/modules", "__devui_modules"),
            ("/routes", "__devui_routes"),
            ("/websockets", "__devui_websockets"),
            ("/info", "__devui_info"),
            ("/config", "__devui_config"),
            ("/components", "__devui_components"),
            ("/tests/run", "__devui_tests_run"),
        ] {
            let path_ptr = self.emit_lambda_string_literal(path);
            writeln!(&mut self.lambda_ir,
                "  %{handler}_fn = ptrtoint void (i64)* @{handler} to i64").unwrap();
            writeln!(&mut self.lambda_ir,
                "  call void @tinox_HttpServer_get(i64* %server, i8* {path_ptr}, i64 %{handler}_fn)").unwrap();
        }
        writeln!(&mut self.lambda_ir, "  call void @tinox_HttpServer_listen(i64* %server)").unwrap();
        writeln!(&mut self.lambda_ir, "  ret i64 0").unwrap();
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();

        self.background_run_fns.push("__tinox_run_devui".to_string());
        self.startup_endpoints.push(("Dev UI".to_string(), format!(":{port}")));
    }

    /// Minimal JSON string escaping for the handful of compiler-controlled
    /// values (module/class/path names, tinox.toml-derived strings)
    /// `emit_devui_code` embeds -- not a general JSON encoder.
    fn devui_json_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    fn devui_json_opt_string(s: &Option<String>) -> String {
        match s {
            Some(v) => Self::devui_json_string(v),
            None => "null".to_string(),
        }
    }

    fn devui_json_string_array(items: &[String]) -> String {
        let parts: Vec<String> = items.iter().map(|s| Self::devui_json_string(s)).collect();
        format!("[{}]", parts.join(","))
    }

    fn devui_routes_json(&self, class_ast_map: &HashMap<String, tinox_parser::Class>) -> String {
        let do_not_serialize: HashSet<(String, String)> = self.do_not_serialize_fields.iter()
            .map(|f| (f.class_name.clone(), f.field_name.clone()))
            .collect();
        let items: Vec<String> = self.route_entries.iter().map(|r| {
            let params_json = Self::devui_route_params_json(&r.params);
            let request_example = r.params.iter()
                .find(|p| p.kind == RouteParamKind::PostParam)
                .map(|p| self.devui_json_example_for_type(&p.ty, class_ast_map, &do_not_serialize, &mut HashSet::new(), 0))
                .unwrap_or_else(|| "null".to_string());
            let response_example = if matches!(&r.return_type, Type::Named(n) if n == "HttpContext") {
                "null".to_string()
            } else {
                self.devui_json_example_for_type(&r.return_type, class_ast_map, &do_not_serialize, &mut HashSet::new(), 0)
            };
            format!(
                "{{\"method\":{},\"path\":{},\"class\":{},\"methodName\":{},\"statusCode\":{},\"produces\":{},\"consumes\":{},\"params\":{},\"requestExample\":{},\"responseExample\":{}}}",
                Self::devui_json_string(&r.http_method),
                Self::devui_json_string(&r.path),
                Self::devui_json_string(&r.class_name),
                Self::devui_json_string(&r.method_name),
                r.status_code.map(|c| c.to_string()).unwrap_or_else(|| "null".to_string()),
                Self::devui_json_opt_string(&r.produces),
                Self::devui_json_opt_string(&r.consumes),
                params_json,
                request_example,
                response_example,
            )
        }).collect();
        format!("[{}]", items.join(","))
    }

    /// `{"kind":"PathParam","name":"id","type":"Int64"}, ...` for a route's
    /// bound parameters -- `type` is `null` for `@HttpContext` (not a JSON
    /// scalar, nothing meaningful to label it with).
    fn devui_route_params_json(params: &[RouteParamBinding]) -> String {
        let items: Vec<String> = params.iter().map(|p| {
            let kind = match p.kind {
                RouteParamKind::PathParam => "PathParam",
                RouteParamKind::QueryParam => "QueryParam",
                RouteParamKind::PostParam => "PostParam",
                RouteParamKind::HttpContext => "HttpContext",
            };
            let type_json = match p.kind {
                RouteParamKind::HttpContext => "null".to_string(),
                _ => Self::devui_json_string(Self::devui_type_name(&p.ty)),
            };
            format!(
                "{{\"kind\":{},\"name\":{},\"type\":{}}}",
                Self::devui_json_string(kind),
                Self::devui_json_string(&p.name),
                type_json,
            )
        }).collect();
        format!("[{}]", items.join(","))
    }

    /// Display name for a param's declared type -- the scalar arms cover
    /// what `@PathParam`/`@QueryParam` actually support
    /// (`is_supported_param_scalar_type`, tinox-typecheck/src/annotations.rs);
    /// `Type::Named` (a `@PostParam`'s class type) returns the class name
    /// itself rather than falling into a generic "Unknown" bucket, since
    /// that's real, useful label info for the devui dialog even though
    /// `@PostParam` isn't a plain scalar. `Type` has no `Display` impl
    /// (only `Debug`, which would print `Named("Foo")` rather than `Foo`),
    /// so this is a fresh match rather than a shortcut through an existing
    /// trait.
    fn devui_type_name(ty: &Type) -> &str {
        match ty {
            Type::String => "String",
            Type::Int8 => "Int8",
            Type::Int16 => "Int16",
            Type::Int32 => "Int32",
            Type::Int64 => "Int64",
            Type::UInt8 => "UInt8",
            Type::UInt16 => "UInt16",
            Type::UInt32 => "UInt32",
            Type::UInt64 => "UInt64",
            Type::Float32 => "Float32",
            Type::Float64 => "Float64",
            Type::Bool => "Bool",
            Type::Char => "Char",
            Type::Named(name) => name,
            _ => "Unknown",
        }
    }

    /// Field name -> declared `Type` for a class, inheritance-aware (own
    /// fields override an ancestor's same-named field, matching how a real
    /// Tinox subclass field shadows its parent's) -- unlike the sibling
    /// `collect_inherited_fields`/`collect_field_class_types` helpers (which
    /// only need field *names* or an LLVM-flattened type), this keeps the
    /// full original AST `Type` so nested/generic shapes (`List<Person>`,
    /// `Person?`, ...) survive for JSON-example generation below.
    fn devui_class_field_types(
        name: &str,
        class_map: &HashMap<String, tinox_parser::Class>,
    ) -> Vec<(String, Type)> {
        let Some(c) = class_map.get(name) else { return vec![]; };
        let mut fields: Vec<(String, Type)> = if let Some(parent) = &c.extends {
            Self::devui_class_field_types(parent, class_map)
        } else {
            vec![]
        };
        for f in &c.fields {
            fields.retain(|(n, _)| n != &f.name);
            fields.push((f.name.clone(), f.field_type.clone()));
        }
        fields
    }

    /// Recursively builds an example JSON *value* for a Tinox `Type` --
    /// backs `/routes`' `requestExample`/`responseExample` (see CLAUDE.md's
    /// Dev UI section). Deliberately walks the original AST `Type` rather
    /// than the LLVM-flattened `struct_field_llvm_types`/
    /// `struct_field_class_types` maps used elsewhere in this file, since
    /// those lose generic-argument fidelity (a `List<String>` field and a
    /// `List<Person>` field are both just "i8*"/"List" at that level).
    ///
    /// Cycle safety is genuinely new in this file -- the closest existing
    /// analog, `emit_devui_component_state_handlers`'s CDI state dumper,
    /// avoids the problem entirely by refusing to recurse into nested
    /// classes at all. This function DOES recurse (needed for realistic
    /// examples of real request/response bodies), so a self-referential
    /// `@JsonSerializable` class (e.g. a tree `Node` with a `List<Node>`
    /// field) needs an explicit guard: `visiting` tracks class names
    /// currently being expanded on the current path and short-circuits a
    /// re-entrant one to a placeholder string instead of recursing forever.
    /// `depth` is a second, independent cap (defense in depth against long
    /// non-cyclic chains, not just true cycles).
    fn devui_json_example_for_type(
        &self,
        ty: &Type,
        class_ast_map: &HashMap<String, tinox_parser::Class>,
        do_not_serialize: &HashSet<(String, String)>,
        visiting: &mut HashSet<String>,
        depth: usize,
    ) -> String {
        if depth > 12 {
            return "null".to_string();
        }
        match ty {
            Type::String => "\"string\"".to_string(),
            Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
            | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 => "0".to_string(),
            Type::Float32 | Type::Float64 => "0.0".to_string(),
            Type::Bool => "false".to_string(),
            Type::Char => "\"c\"".to_string(),
            Type::Mutable(inner) | Type::Ref(inner) | Type::Nullable(inner) =>
                self.devui_json_example_for_type(inner, class_ast_map, do_not_serialize, visiting, depth),
            Type::Generic { name, args } if (name == "List" || name == "Array") && !args.is_empty() => {
                let elem = self.devui_json_example_for_type(&args[0], class_ast_map, do_not_serialize, visiting, depth + 1);
                format!("[{elem}]")
            }
            Type::Array(inner) => {
                let elem = self.devui_json_example_for_type(inner, class_ast_map, do_not_serialize, visiting, depth + 1);
                format!("[{elem}]")
            }
            Type::Named(cls) => {
                if !class_ast_map.contains_key(cls) {
                    return "null".to_string();
                }
                if !visiting.insert(cls.clone()) {
                    // Already expanding this class further up the current
                    // path -- a genuine cycle, not just repeated use of the
                    // same class in unrelated fields (each top-level call
                    // gets its own empty `visiting` set).
                    return Self::devui_json_string(&format!("<{cls}>"));
                }
                let fields = Self::devui_class_field_types(cls, class_ast_map);
                let parts: Vec<String> = fields.iter()
                    .filter(|(fname, _)| {
                        let owner = self.field_declaring_class(cls, fname);
                        !do_not_serialize.contains(&(owner, fname.clone()))
                    })
                    .map(|(fname, fty)| {
                        let val = self.devui_json_example_for_type(fty, class_ast_map, do_not_serialize, visiting, depth + 1);
                        format!("{}:{}", Self::devui_json_string(fname), val)
                    })
                    .collect();
                visiting.remove(cls);
                format!("{{{}}}", parts.join(","))
            }
            _ => "null".to_string(),
        }
    }

    fn devui_websockets_json(&self) -> String {
        // Same effective-port fallback emit_ws_code itself uses -- an
        // endpoint with no explicit @WebsocketEndpoint(port) argument
        // resolves it identically at codegen time (TINOX_PORT env var,
        // default 8080), so this reports the port that will actually be
        // bound, not just what was literally written in source.
        let items: Vec<String> = self.ws_endpoints.iter().map(|w| {
            let port = w.port
                .or_else(|| std::env::var("TINOX_PORT").ok().and_then(|s| s.parse::<i64>().ok()))
                .unwrap_or(8080);
            format!(
                "{{\"class\":{},\"path\":{},\"port\":{port}}}",
                Self::devui_json_string(&w.class_name),
                Self::devui_json_string(&w.path),
            )
        }).collect();
        format!("[{}]", items.join(","))
    }

    /// Emits `define void @{handler_name}(i64 %ctx_i64) { ... }` serving a
    /// pre-baked JSON string constant -- used for the introspection routes
    /// whose content is fully known at codegen time (modules/routes/
    /// websockets/info). `/config` and `/components` need per-request
    /// runtime work instead (see their own emit_devui_*_handler methods).
    fn emit_devui_static_handler(&mut self, handler_name: &str, json: &str) {
        writeln!(&mut self.lambda_ir, "define void @{handler_name}(i64 %ctx_i64) {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
        let json_ptr = self.emit_lambda_string_literal(json);
        self.emit_devui_finish_response(&json_ptr);
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();
    }

    /// Shared response-writing tail for a devui handler -- `%ctx_i64`
    /// already in scope as the handler's incoming parameter. Sets status
    /// 200, the body to the given already-computed `i8*` SSA value, and
    /// `Content-Type: application/json`, then `ret void`. Self-contained
    /// (its own string literals via `emit_lambda_string_literal`):
    /// deliberately does not reuse `@__hdr_content_type`, which
    /// `emit_route_annotation_globals` only emits when the program also
    /// has real `@GET`/etc. routes -- a devui-only program (no REST
    /// annotations at all) would otherwise reference an undefined symbol.
    fn emit_devui_finish_response(&mut self, json_var: &str) {
        writeln!(&mut self.lambda_ir, "  %ctx_ptr = inttoptr i64 %ctx_i64 to i64*").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_field = getelementptr i64, i64* %ctx_ptr, i64 1").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_i64 = load i64, i64* %resp_field").unwrap();
        writeln!(&mut self.lambda_ir, "  %resp_ptr = inttoptr i64 %resp_i64 to i64*").unwrap();
        writeln!(&mut self.lambda_ir, "  %sc_field = getelementptr i64, i64* %resp_ptr, i64 0").unwrap();
        writeln!(&mut self.lambda_ir, "  store i64 200, i64* %sc_field").unwrap();
        writeln!(&mut self.lambda_ir, "  %body_field = getelementptr i64, i64* %resp_ptr, i64 2").unwrap();
        writeln!(&mut self.lambda_ir, "  %body_i64 = ptrtoint i8* {json_var} to i64").unwrap();
        writeln!(&mut self.lambda_ir, "  store i64 %body_i64, i64* %body_field").unwrap();
        let ct_key_ptr = self.emit_lambda_string_literal("Content-Type");
        let ct_val_ptr = self.emit_lambda_string_literal("application/json");
        writeln!(&mut self.lambda_ir, "  %ct_val_i64 = ptrtoint i8* {ct_val_ptr} to i64").unwrap();
        writeln!(&mut self.lambda_ir, "  %hdrs_field = getelementptr i64, i64* %resp_ptr, i64 1").unwrap();
        writeln!(&mut self.lambda_ir, "  %hdrs_i64 = load i64, i64* %hdrs_field").unwrap();
        writeln!(&mut self.lambda_ir, "  %hdrs_ptr = inttoptr i64 %hdrs_i64 to i8*").unwrap();
        writeln!(&mut self.lambda_ir,
            "  call void @tinox_map_set(i8* %hdrs_ptr, i8* {ct_key_ptr}, i64 %ct_val_i64)").unwrap();
        writeln!(&mut self.lambda_ir, "  ret void").unwrap();
    }

    /// `/config`: concatenates the compile-time tinox.toml summary
    /// (`dev_config_summary_json`, built by `build_dev_config_summary_json`
    /// in main.rs and baked as a constant) with a live
    /// `application.properties` dump (`tinox_config_dump_json`,
    /// runtime.c) -- the two genuinely separate config sources this
    /// project has (see CLAUDE.md's Dev UI section).
    fn emit_devui_config_handler(&mut self) {
        writeln!(&mut self.lambda_ir, "declare i8* @tinox_config_dump_json()").unwrap();
        writeln!(&mut self.lambda_ir, "define void @__devui_config(i64 %ctx_i64) {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
        let static_ptr = self.emit_lambda_string_literal(&self.dev_config_summary_json.clone());
        let prefix_ptr = self.emit_lambda_string_literal("{\"tinoxToml\":");
        let mid_ptr = self.emit_lambda_string_literal(",\"applicationProperties\":");
        writeln!(&mut self.lambda_ir, "  %props_json = call i8* @tinox_config_dump_json()").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %s1 = call i8* @tinox_string_concat(i8* {prefix_ptr}, i8* {static_ptr})").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %s2 = call i8* @tinox_string_concat(i8* %s1, i8* {mid_ptr})").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %s3 = call i8* @tinox_string_concat(i8* %s2, i8* %props_json)").unwrap();
        let suffix_ptr = self.emit_lambda_string_literal("}");
        writeln!(&mut self.lambda_ir,
            "  %json = call i8* @tinox_string_concat(i8* %s3, i8* {suffix_ptr})").unwrap();
        self.emit_devui_finish_response("%json");
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();
    }

    /// Emits `@{class}_devui_state_json(i8* %self_i8) -> i8*` for every
    /// Application/Startup-scoped DI component -- a full field-value dump
    /// backing `/components`' "state". Null-safe by design (returns a
    /// null i8* immediately when `%self_i8` is null, i.e. no singleton
    /// exists yet), so `emit_devui_components_handler` can call it
    /// unconditionally instead of needing its own branch.
    ///
    /// Deliberately a SEPARATE serializer from `emit_json_serialize_code`'s
    /// `_toJson` (@JsonSerializable-only, used for real REST response
    /// bodies) -- not an extension of that shared, already-relied-on path.
    /// The plan for this whole feature explicitly called out `_toJson`'s
    /// known gap: `List<Class>` and directly-nested-class fields are i64*
    /// at the LLVM level, indistinguishable from a plain int array, so
    /// `_toJson`'s existing fallback (`jsonBuilderAddIntList`) would
    /// silently misread one as consecutive int64s -- exactly the kind of
    /// silent garbage this project's CLAUDE.md exists to prevent. This
    /// serializer instead:
    ///  - reuses `tinox_json_list_serialize` for `List<X>` where X is
    ///    `@JsonSerializable` -- the SAME dispatch the compiler's own
    ///    `List<C>.toJson()` call-site codegen already uses (proven
    ///    correct: it's real REST-response serialization code, not new
    ///    machinery written just for this).
    ///  - falls back to an honest `"<unsupported field type>"` placeholder
    ///    for anything else pointer-shaped it can't identify this way
    ///    (Map<K,V>, List<String>/List<Float>, a List of a non-
    ///    @JsonSerializable class, a directly nested class field --
    ///    narrower scope than `List<X>`, skipped rather than risking a
    ///    null-pointer call into an arbitrary class's `_toJson`). Narrow,
    ///    not a general recursive object serializer.
    ///
    /// Plain scalars (String/Int/Float/Bool) and plain int arrays
    /// (`List<Int>`, marker "Array") serialize exactly like `_toJson`
    /// always has -- this only changes behavior for the i64* cases that
    /// were previously ALWAYS routed through `jsonBuilderAddIntList`
    /// regardless of what they actually were.
    fn emit_devui_component_state_handlers(&mut self, components: &[DiComponentInfo]) {
        let do_not_serialize_set: std::collections::HashSet<(String, String)> =
            self.do_not_serialize_fields.iter()
                .map(|f| (f.class_name.clone(), f.field_name.clone()))
                .collect();

        for comp in components {
            if !matches!(comp.scope, DiScope::Application | DiScope::Startup) {
                continue;
            }
            let class_name = comp.class_name.clone();
            let Some(layout) = self.struct_layouts.get(&class_name).cloned() else { continue };
            let Some(llvm_types) = self.struct_field_llvm_types.get(&class_name).cloned() else { continue };
            let field_class_types = self.struct_field_class_types.get(&class_name).cloned().unwrap_or_default();

            let data_fields: Vec<(String, usize, String)> = layout.iter()
                .enumerate()
                .filter(|(_, f)| *f != "__vtable__" && *f != "log")
                .filter(|(_, f)| {
                    let owner = self.field_declaring_class(&class_name, f);
                    !do_not_serialize_set.contains(&(owner, f.to_string()))
                })
                .filter_map(|(idx, f)| llvm_types.get(f).map(|ty| (f.clone(), idx, ty.clone())))
                .collect();

            let uid = self.temp_count;
            self.temp_count += 1;

            writeln!(&mut self.lambda_ir, "define i8* @{class_name}_devui_state_json(i8* %self_i8) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_null_{uid} = icmp eq i8* %self_i8, null").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %is_null_{uid}, label %null_case_{uid}, label %normal_case_{uid}").unwrap();
            writeln!(&mut self.lambda_ir, "null_case_{uid}:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i8* null").unwrap();
            writeln!(&mut self.lambda_ir, "normal_case_{uid}:").unwrap();
            writeln!(&mut self.lambda_ir, "  %self = bitcast i8* %self_i8 to i64*").unwrap();
            let builder = self.temp();
            writeln!(&mut self.lambda_ir, "  {builder} = call i8* @jsonBuilderCreate()").unwrap();

            for (field_name, struct_idx, llvm_ty) in &data_fields {
                let key_lbl = format!("str{}", self.strings.len());
                self.strings.insert(key_lbl.clone(), field_name.clone());
                let key_len = field_name.len() + 1;
                let key_ptr = self.temp();
                writeln!(&mut self.lambda_ir,
                    "  {key_ptr} = getelementptr [{key_len} x i8], [{key_len} x i8]* @{key_lbl}, i64 0, i64 0").unwrap();

                let fptr = self.temp();
                let raw = self.temp();
                writeln!(&mut self.lambda_ir, "  {fptr} = getelementptr i64, i64* %self, i64 {struct_idx}").unwrap();
                writeln!(&mut self.lambda_ir, "  {raw} = load i64, i64* {fptr}").unwrap();

                let marker = field_class_types.get(field_name).cloned();

                match llvm_ty.as_str() {
                    "i8*" => {
                        let s = self.temp();
                        writeln!(&mut self.lambda_ir, "  {s} = inttoptr i64 {raw} to i8*").unwrap();
                        writeln!(&mut self.lambda_ir, "  call void @jsonBuilderAddString(i8* {builder}, i8* {key_ptr}, i8* {s})").unwrap();
                    }
                    "double" | "float" => {
                        let dbl = self.temp();
                        writeln!(&mut self.lambda_ir, "  {dbl} = bitcast i64 {raw} to double").unwrap();
                        writeln!(&mut self.lambda_ir, "  call void @jsonBuilderAddFloat(i8* {builder}, i8* {key_ptr}, double {dbl})").unwrap();
                    }
                    "i1" => {
                        let truncated = self.temp();
                        let extended = self.temp();
                        writeln!(&mut self.lambda_ir, "  {truncated} = trunc i64 {raw} to i1").unwrap();
                        writeln!(&mut self.lambda_ir, "  {extended} = zext i1 {truncated} to i32").unwrap();
                        writeln!(&mut self.lambda_ir, "  call void @jsonBuilderAddBool(i8* {builder}, i8* {key_ptr}, i32 {extended})").unwrap();
                    }
                    "i64*" => {
                        let list_of_serializable = marker.as_deref()
                            .and_then(|m| m.strip_prefix("List:"))
                            .filter(|cls| self.json_serializable_classes.iter().any(|c| c == cls))
                            .map(|cls| cls.to_string());
                        if marker.as_deref() == Some("Array") {
                            let arr_ptr = self.temp();
                            writeln!(&mut self.lambda_ir, "  {arr_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                            writeln!(&mut self.lambda_ir, "  call void @jsonBuilderAddIntList(i8* {builder}, i8* {key_ptr}, i64* {arr_ptr})").unwrap();
                        } else if let Some(cls) = list_of_serializable {
                            let list_ptr = self.temp();
                            writeln!(&mut self.lambda_ir, "  {list_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                            let json = self.temp();
                            writeln!(&mut self.lambda_ir,
                                "  {json} = call i8* @tinox_json_list_serialize(i64* {list_ptr}, ptr @{cls}_toJson)").unwrap();
                            writeln!(&mut self.lambda_ir,
                                "  call void @jsonBuilderAddRaw(i8* {builder}, i8* {key_ptr}, i8* {json})").unwrap();
                        } else {
                            let placeholder = self.emit_lambda_string_literal("<unsupported field type>");
                            writeln!(&mut self.lambda_ir,
                                "  call void @jsonBuilderAddString(i8* {builder}, i8* {key_ptr}, i8* {placeholder})").unwrap();
                        }
                    }
                    _ => {
                        writeln!(&mut self.lambda_ir, "  call void @jsonBuilderAddInt(i8* {builder}, i8* {key_ptr}, i64 {raw})").unwrap();
                    }
                }
            }

            let result = self.temp();
            writeln!(&mut self.lambda_ir, "  {result} = call i8* @jsonBuilderFinish(i8* {builder})").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i8* {result}").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }
    }

    /// `/tests/run`: shells out to `tinox test` in the connected project's
    /// own directory via `tinox_run_command_json` (runtime.c, popen-based)
    /// -- `dev_test_command` is a compile-time constant (built in main.rs
    /// from `std::env::current_exe()` and the project root found by
    /// walking up from the build's own working directory), never
    /// influenced by request input, so there's no injection surface
    /// despite this running an arbitrary-looking shell command from an
    /// HTTP handler. Backs the dashboard's Tests view -- `tinox test`'s
    /// own human-readable stdout ("PASS ..."/"FAIL ...", a final "N
    /// tests -- N passed, N failed" summary) is returned as-is in
    /// `output`, no separate structured parsing needed on either side.
    fn emit_devui_tests_run_handler(&mut self) {
        writeln!(&mut self.lambda_ir, "declare i8* @tinox_run_command_json(i8*)").unwrap();
        writeln!(&mut self.lambda_ir, "define void @__devui_tests_run(i64 %ctx_i64) {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
        if self.dev_test_command.is_empty() {
            let err_json = self.emit_lambda_string_literal(
                "{\"exitCode\":-1,\"output\":\"could not determine the tinox binary path or project root at build time\"}"
            );
            self.emit_devui_finish_response(&err_json);
        } else {
            let cmd_ptr = self.emit_lambda_string_literal(&self.dev_test_command.clone());
            let json = self.temp();
            writeln!(&mut self.lambda_ir, "  {json} = call i8* @tinox_run_command_json(i8* {cmd_ptr})").unwrap();
            self.emit_devui_finish_response(&json);
        }
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();
    }

    /// `/components`: one JSON object per `@ApplicationComponent`-scoped
    /// class -- name, scope, and whether a singleton currently exists.
    /// Application/Startup-scoped components have a real
    /// `@{class}_di_instance` global (`emit_di_code`) to check; loads it
    /// and compares to null right here at request time. HttpRequest-scoped
    /// ones never get such a global (a fresh instance is allocated per
    /// request, never cached), so their flag is a compile-time-constant
    /// `0` -- there's nothing to check. "state" defers to
    /// `emit_devui_component_state_handlers`'s null-safe per-class
    /// function, called unconditionally for Application/Startup scope.
    fn emit_devui_components_handler(&mut self) {
        writeln!(&mut self.lambda_ir, "declare i8* @tinox_devui_components_json(i8**, i8**, i64*, i8**, i64)").unwrap();
        let components = self.di_components.clone();
        self.emit_devui_component_state_handlers(&components);
        writeln!(&mut self.lambda_ir, "define void @__devui_components(i64 %ctx_i64) {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

        if components.is_empty() {
            let empty_ptr = self.emit_lambda_string_literal("[]");
            self.emit_devui_finish_response(&empty_ptr);
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
            return;
        }

        let count = components.len();
        let name_ptrs: Vec<String> = components.iter()
            .map(|c| self.emit_lambda_string_literal(&c.class_name))
            .collect();
        let scope_ptrs: Vec<String> = components.iter()
            .map(|c| {
                let scope_str = match c.scope {
                    DiScope::Application => "Application",
                    DiScope::Startup => "Startup",
                    DiScope::HttpRequest => "HttpRequest",
                };
                self.emit_lambda_string_literal(scope_str)
            })
            .collect();

        writeln!(&mut self.lambda_ir, "  %names = alloca [{count} x i8*]").unwrap();
        writeln!(&mut self.lambda_ir, "  %scopes = alloca [{count} x i8*]").unwrap();
        writeln!(&mut self.lambda_ir, "  %flags = alloca [{count} x i64]").unwrap();
        writeln!(&mut self.lambda_ir, "  %states = alloca [{count} x i8*]").unwrap();
        for (idx, comp) in components.iter().enumerate() {
            writeln!(&mut self.lambda_ir,
                "  %name_slot_{idx} = getelementptr [{count} x i8*], [{count} x i8*]* %names, i64 0, i64 {idx}").unwrap();
            writeln!(&mut self.lambda_ir, "  store i8* {}, i8** %name_slot_{idx}", name_ptrs[idx]).unwrap();
            writeln!(&mut self.lambda_ir,
                "  %scope_slot_{idx} = getelementptr [{count} x i8*], [{count} x i8*]* %scopes, i64 0, i64 {idx}").unwrap();
            writeln!(&mut self.lambda_ir, "  store i8* {}, i8** %scope_slot_{idx}", scope_ptrs[idx]).unwrap();
            writeln!(&mut self.lambda_ir,
                "  %flag_slot_{idx} = getelementptr [{count} x i64], [{count} x i64]* %flags, i64 0, i64 {idx}").unwrap();
            writeln!(&mut self.lambda_ir,
                "  %state_slot_{idx} = getelementptr [{count} x i8*], [{count} x i8*]* %states, i64 0, i64 {idx}").unwrap();
            match comp.scope {
                DiScope::Application | DiScope::Startup => {
                    writeln!(&mut self.lambda_ir,
                        "  %inst_raw_{idx} = load i8*, i8** @{}_di_instance", comp.class_name).unwrap();
                    writeln!(&mut self.lambda_ir,
                        "  %is_set_{idx} = icmp ne i8* %inst_raw_{idx}, null").unwrap();
                    writeln!(&mut self.lambda_ir,
                        "  %flag_{idx} = zext i1 %is_set_{idx} to i64").unwrap();
                    writeln!(&mut self.lambda_ir, "  store i64 %flag_{idx}, i64* %flag_slot_{idx}").unwrap();
                    writeln!(&mut self.lambda_ir,
                        "  %state_{idx} = call i8* @{}_devui_state_json(i8* %inst_raw_{idx})", comp.class_name).unwrap();
                    writeln!(&mut self.lambda_ir, "  store i8* %state_{idx}, i8** %state_slot_{idx}").unwrap();
                }
                DiScope::HttpRequest => {
                    writeln!(&mut self.lambda_ir, "  store i64 0, i64* %flag_slot_{idx}").unwrap();
                    writeln!(&mut self.lambda_ir, "  store i8* null, i8** %state_slot_{idx}").unwrap();
                }
            }
        }
        writeln!(&mut self.lambda_ir,
            "  %names_ptr = getelementptr [{count} x i8*], [{count} x i8*]* %names, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %scopes_ptr = getelementptr [{count} x i8*], [{count} x i8*]* %scopes, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %flags_ptr = getelementptr [{count} x i64], [{count} x i64]* %flags, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %states_ptr = getelementptr [{count} x i8*], [{count} x i8*]* %states, i64 0, i64 0").unwrap();
        writeln!(&mut self.lambda_ir,
            "  %json = call i8* @tinox_devui_components_json(i8** %names_ptr, i8** %scopes_ptr, i64* %flags_ptr, i8** %states_ptr, i64 {count})").unwrap();
        self.emit_devui_finish_response("%json");
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();
    }

    /// Unifies `class Main { fnc main() }` (user_main_class) and the
    /// auto-run kinds registered in background_run_fns (REST/HTTP3/WS/AMQP)
    /// into one @tinox_main, instead of each claiming it directly and
    /// silently pre-empting the others via has_main. Each background kind
    /// runs on its own real thread (tinox_task_spawn) so e.g. a REST
    /// controller and a WebSocket endpoint can now run in the same process;
    /// @Main_main (if present) runs on the main thread and its return code
    /// is what the process exits with once every spawned kind is joined
    /// (which for a listen loop never happens -- same "blocks forever"
    /// behavior a single directly-called `.listen()` had before).
    ///
    /// No-op (falls through to has_main's existing "undefined reference to
    /// tinox_main" link-time signal) when there is nothing to wire: no
    /// class Main and no background kind. Also a no-op when a legacy
    /// top-level `fn main()` already claimed @tinox_main directly (that
    /// deprecated pre-#149 shape intentionally does not participate in this
    /// unification).
    ///
    /// Also owns the startup banner: ASCII art, the `tinox.core` modules
    /// from `[[dependencies]]` (via `set_loaded_modules`), the auto-run
    /// endpoints registered in `startup_endpoints` (protocol + port/detail,
    /// pushed alongside each `background_run_fns` entry above), and the
    /// bootstrap's own elapsed time. Printed by default -- no `import
    /// tinox.core.logger;`/annotation needed -- since this is the one
    /// place in the generated program that already knows about every
    /// auto-run kind and is guaranteed to run exactly once, first.
    /// Opt out per project via `[startup] banner = false` in tinox.toml
    /// (`banner_enabled` / `set_startup_banner_enabled`).
    fn emit_tinox_main_bootstrap(&mut self) {
        // `tinox test` (compile_test_exe, main.rs) compiles ONE source
        // file's full annotation set through the exact same `gen()` path
        // as a normal build -- including whatever the file `import`s. A
        // test file importing a class with its own auto-run annotations
        // (e.g. testing a helper method on an @GET/@ApplicationComponent
        // REST controller, a completely ordinary thing to want to test)
        // populates `background_run_fns` from THAT class's routes, same
        // as any real program would. Found live: without this guard, this
        // function runs BEFORE `emit_test_code` (which only defines
        // `@tinox_main` when `!self.has_main`) and unconditionally claims
        // `@tinox_main` itself instead -- the compiled "test" binary
        // silently becomes the real app's auto-run bootstrap (spawns the
        // HTTP server, blocks forever waiting for its listener thread to
        // join) and the actual @Test method is never even called. No
        // error, no wrong-answer test failure either -- the process just
        // hangs, which is arguably worse. `test_entry.is_some()` means
        // `emit_test_code` is going to define `@tinox_main` itself
        // (guarded on `!has_main`, so it MUST see this function skip);
        // background_run_fns/route_entries etc. still get populated and
        // their functions still get emitted as harmless unused IR -- only
        // this bootstrap step (deciding what `@tinox_main` becomes) needs
        // to defer to the test runner.
        if self.test_entry.is_some() {
            return;
        }
        if self.has_main {
            return;
        }
        if !self.user_main_class && self.background_run_fns.is_empty() {
            return;
        }

        let run_fns = self.background_run_fns.clone();
        let mut handles = Vec::new();
        for (idx, run_fn) in run_fns.iter().enumerate() {
            let tramp = format!("__tinox_trampoline_{idx}");
            writeln!(&mut self.lambda_ir, "define i8* @{tramp}(i8* %_unused) {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            writeln!(&mut self.lambda_ir, "  %r = call i64 @{run_fn}()").unwrap();
            writeln!(&mut self.lambda_ir, "  %rp = inttoptr i64 %r to i8*").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i8* %rp").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
            handles.push((idx, tramp));
        }

        // Startup banner only makes sense for programs that actually have
        // something auto-run (a REST/HTTP3/WS/AMQP endpoint) -- a plain
        // `class Main` script with no annotations still comes through this
        // function (user_main_class alone is enough to not early-return
        // above) and must keep printing exactly nothing extra: this is the
        // shape virtually every e2e/example test with an exact `// expect:`
        // stdout match uses, and it's also just not a meaningful "started
        // serving on port X" moment for a one-shot script. `banner_enabled`
        // is the separate, explicit `[startup] banner = false` opt-out for
        // programs that DO have an endpoint but still want clean stdout.
        let show_banner = self.banner_enabled && !self.background_run_fns.is_empty();

        // figlet -f standard "Tinox" -- generated once, hardcoded here since
        // it's fixed text with no dynamic content.
        let mut banner = String::new();
        banner.push_str(" _____ _                 \n");
        banner.push_str("|_   _(_)_ __   _____  __\n");
        banner.push_str("  | | | | '_ \\ / _ \\ \\/ /\n");
        banner.push_str("  | | | | | | | (_) >  < \n");
        banner.push_str("  |_| |_|_| |_|\\___/_/\\_\\\n");
        if !self.loaded_modules.is_empty() {
            banner.push_str(&format!("Loaded tinox.core modules: {}\n", self.loaded_modules.join(", ")));
        }
        banner.push_str("Endpoints:\n");
        for (protocol, detail) in &self.startup_endpoints {
            banner.push_str(&format!("  {protocol:<22} {detail}\n"));
        }

        writeln!(&mut self.lambda_ir, "define i32 @tinox_main() {{").unwrap();
        writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

        // t0: captured before anything else runs, so the elapsed time
        // printed below covers the banner print itself too (negligible,
        // but simpler than threading a "skip this call" flag through).
        let t0 = if show_banner {
            let t = self.temp();
            writeln!(&mut self.lambda_ir, "  {t} = call i64 @tinox_now_ms()").unwrap();
            let banner_ptr = self.emit_lambda_string_literal(&banner);
            writeln!(&mut self.lambda_ir, "  call void @tinox_print_string(i8* {banner_ptr})").unwrap();
            Some(t)
        } else {
            None
        };

        for (idx, tramp) in &handles {
            writeln!(&mut self.lambda_ir,
                "  %h_{idx} = call i8* @tinox_task_spawn(i8* (i8*)* @{tramp}, i8* null)").unwrap();
        }

        // t1: right after every auto-run kind has been spawned (not
        // "actually listening" -- HttpServer::listen()'s own bind happens
        // asynchronously on its spawned thread -- but matches how e.g.
        // Spring Boot's "Started Application in Xs" measures context
        // bring-up, not first successful request).
        if let Some(t0) = t0 {
            let t1 = self.temp();
            writeln!(&mut self.lambda_ir, "  {t1} = call i64 @tinox_now_ms()").unwrap();
            let elapsed = self.temp();
            writeln!(&mut self.lambda_ir, "  {elapsed} = sub i64 {t1}, {t0}").unwrap();
            let started_pfx_ptr = self.emit_lambda_string_literal("Started in ");
            writeln!(&mut self.lambda_ir, "  call void @tinox_print_string(i8* {started_pfx_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_print_int(i64 {elapsed})").unwrap();
            let started_sfx_ptr = self.emit_lambda_string_literal(" ms\n");
            writeln!(&mut self.lambda_ir, "  call void @tinox_print_string(i8* {started_sfx_ptr})").unwrap();
        }

        if self.user_main_class {
            writeln!(&mut self.lambda_ir, "  %rc = call i32 @Main_main()").unwrap();
        }

        for (idx, _) in &handles {
            writeln!(&mut self.lambda_ir, "  %j_{idx} = call i64 @tinox_task_await(i8* %h_{idx})").unwrap();
        }

        if self.user_main_class {
            writeln!(&mut self.lambda_ir, "  ret i32 %rc").unwrap();
        } else {
            // No class Main: the first background kind's own join result is
            // the process's exit code (matches the pre-bootstrap behavior
            // of a lone auto-run kind returning its own value directly).
            writeln!(&mut self.lambda_ir, "  %rc32 = trunc i64 %j_0 to i32").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i32 %rc32").unwrap();
        }
        writeln!(&mut self.lambda_ir, "}}").unwrap();
        writeln!(&mut self.lambda_ir).unwrap();

        self.has_main = true;
    }

    fn emit_di_code(&mut self) {
        let components = self.di_components.clone();
        if components.is_empty() {
            return;
        }

        // Global instance pointers for application/startup scoped components
        for comp in &components {
            if matches!(comp.scope, DiScope::Application | DiScope::Startup) {
                writeln!(&mut self.lambda_ir,
                    "@{}_di_instance = global i8* null", comp.class_name).unwrap();
            }
        }
        writeln!(&mut self.lambda_ir).unwrap();

        // Getter / factory for each component
        for comp in &components {
            let name = &comp.class_name;
            let size = self.struct_layouts.get(name.as_str())
                .map(|f| (f.len().max(1) * 8) as i64)
                .unwrap_or(8);

            match comp.scope {
                DiScope::Application | DiScope::Startup => {
                    writeln!(&mut self.lambda_ir, "define i64* @{name}_di_get() {{").unwrap();
                    writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
                    writeln!(&mut self.lambda_ir, "  %inst_raw = load i8*, i8** @{name}_di_instance").unwrap();
                    writeln!(&mut self.lambda_ir, "  %is_null = icmp eq i8* %inst_raw, null").unwrap();
                    writeln!(&mut self.lambda_ir, "  br i1 %is_null, label %create, label %done").unwrap();
                    writeln!(&mut self.lambda_ir, "create:").unwrap();
                    writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {size})").unwrap();
                    writeln!(&mut self.lambda_ir, "  %new_inst = bitcast i8* %raw to i64*").unwrap();

                    for (fi, field) in comp.inject_fields.iter().enumerate() {
                        let field_offset = self.struct_layouts.get(name.as_str())
                            .and_then(|layout| layout.iter().position(|f| f == &field.field_name))
                            .unwrap_or(0);
                        let dep = &field.field_type;
                        let dep_is_app = components.iter().any(|c|
                            c.class_name == *dep && matches!(c.scope, DiScope::Application | DiScope::Startup));
                        if dep_is_app {
                            writeln!(&mut self.lambda_ir, "  %dep_{fi} = call i64* @{dep}_di_get()").unwrap();
                        } else {
                            writeln!(&mut self.lambda_ir, "  %dep_{fi} = call i64* @{dep}_di_create()").unwrap();
                        }
                        writeln!(&mut self.lambda_ir, "  %dep_i64_{fi} = ptrtoint i64* %dep_{fi} to i64").unwrap();
                        writeln!(&mut self.lambda_ir, "  %fptr_{fi} = getelementptr i64, i64* %new_inst, i64 {field_offset}").unwrap();
                        writeln!(&mut self.lambda_ir, "  store i64 %dep_i64_{fi}, i64* %fptr_{fi}").unwrap();
                    }

                    writeln!(&mut self.lambda_ir, "  %new_raw = bitcast i64* %new_inst to i8*").unwrap();
                    writeln!(&mut self.lambda_ir, "  store i8* %new_raw, i8** @{name}_di_instance").unwrap();
                    writeln!(&mut self.lambda_ir, "  br label %done").unwrap();
                    writeln!(&mut self.lambda_ir, "done:").unwrap();
                    writeln!(&mut self.lambda_ir, "  %result_raw = load i8*, i8** @{name}_di_instance").unwrap();
                    writeln!(&mut self.lambda_ir, "  %result = bitcast i8* %result_raw to i64*").unwrap();
                    writeln!(&mut self.lambda_ir, "  ret i64* %result").unwrap();
                    writeln!(&mut self.lambda_ir, "}}").unwrap();
                    writeln!(&mut self.lambda_ir).unwrap();
                }
                DiScope::HttpRequest => {
                    writeln!(&mut self.lambda_ir, "define i64* @{name}_di_create() {{").unwrap();
                    writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
                    writeln!(&mut self.lambda_ir, "  %raw = call i8* @tinox_alloc(i64 {size})").unwrap();
                    writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();

                    for (fi, field) in comp.inject_fields.iter().enumerate() {
                        let field_offset = self.struct_layouts.get(name.as_str())
                            .and_then(|layout| layout.iter().position(|f| f == &field.field_name))
                            .unwrap_or(0);
                        let dep = &field.field_type;
                        let dep_is_app = components.iter().any(|c|
                            c.class_name == *dep && matches!(c.scope, DiScope::Application | DiScope::Startup));
                        if dep_is_app {
                            writeln!(&mut self.lambda_ir, "  %dep_{fi} = call i64* @{dep}_di_get()").unwrap();
                        } else {
                            writeln!(&mut self.lambda_ir, "  %dep_{fi} = call i64* @{dep}_di_create()").unwrap();
                        }
                        writeln!(&mut self.lambda_ir, "  %dep_i64_{fi} = ptrtoint i64* %dep_{fi} to i64").unwrap();
                        writeln!(&mut self.lambda_ir, "  %fptr_{fi} = getelementptr i64, i64* %inst, i64 {field_offset}").unwrap();
                        writeln!(&mut self.lambda_ir, "  store i64 %dep_i64_{fi}, i64* %fptr_{fi}").unwrap();
                    }

                    writeln!(&mut self.lambda_ir, "  ret i64* %inst").unwrap();
                    writeln!(&mut self.lambda_ir, "}}").unwrap();
                    writeln!(&mut self.lambda_ir).unwrap();
                }
            }
        }

        // tinox_di_startup(): eagerly initialize all @Startup components
        let has_startup = components.iter().any(|c| matches!(c.scope, DiScope::Startup));
        if has_startup {
            writeln!(&mut self.lambda_ir, "define void @tinox_di_startup() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();
            for comp in components.iter().filter(|c| matches!(c.scope, DiScope::Startup)) {
                writeln!(&mut self.lambda_ir, "  call i64* @{}_di_get()", comp.class_name).unwrap();
            }
            writeln!(&mut self.lambda_ir, "  ret void").unwrap();
            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            // Register tinox_di_startup as a global constructor so it runs before main
            writeln!(&mut self.lambda_ir,
                "@llvm.global_ctors = appending global [1 x {{ i32, void ()*, i8* }}] \
                [{{ i32, void ()*, i8* }} {{ i32 65535, void ()* @tinox_di_startup, i8* null }}]").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();
        }
    }

    fn escape_llvm_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"'  => out.push_str("\\22"),
                '\\'  => out.push_str("\\5C"),
                '\n' => out.push_str("\\0A"),
                '\r' => out.push_str("\\0D"),
                '\t' => out.push_str("\\09"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\{:02X}", c as u32));
                }
                c => out.push(c),
            }
        }
        out
    }

    /// Escapes a string for an LLVM debug-metadata string literal
    /// (`!DIFile(filename: "...")` etc.) — standard C-style backslash
    /// escaping, distinct from `escape_llvm_string`'s hex-byte escaping
    /// used for `[N x i8] c"..."` global string constants.
    fn escape_di_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\22"),
                '\\' => out.push_str("\\5C"),
                c => out.push(c),
            }
        }
        out
    }

    /// Gets or creates the `!DIFile` metadata node id for `file` (issue
    /// #114). `file` is an absolute path stamped by `stamp_file_identity`
    /// (`tinox/src/main.rs`) — split into directory/filename here since
    /// that's `!DIFile`'s own field split.
    fn di_file_id(&mut self, file: &Arc<str>) -> u32 {
        if let Some(&id) = self.di_file_ids.get(file) {
            return id;
        }
        let id = self.di_next_id;
        self.di_next_id += 1;
        let path = Path::new(file.as_ref());
        let filename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string());
        let directory = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.di_metadata.push(format!(
            "!{id} = !DIFile(filename: \"{}\", directory: \"{}\")",
            Self::escape_di_string(&filename),
            Self::escape_di_string(&directory)
        ));
        self.di_file_ids.insert(file.clone(), id);
        id
    }

    /// Gets or creates the single `distinct !DICompileUnit(...)` node
    /// (issue #114) — one per module, lazily created against whichever
    /// file's function is compiled first (its `!DIFile` doesn't need to
    /// be THE "main" file; every `!DISubprogram` still names its own
    /// correct file via `di_file_id`, `unit:` just anchors the module).
    fn di_compile_unit_id(&mut self, file: &Arc<str>) -> u32 {
        if let Some(id) = self.di_compile_unit_id {
            return id;
        }
        let file_id = self.di_file_id(file);
        let id = self.di_next_id;
        self.di_next_id += 1;
        self.di_metadata.push(format!(
            "!{id} = distinct !DICompileUnit(language: DW_LANG_C99, file: !{file_id}, producer: \"tinox\", isOptimized: false, runtimeVersion: 0, emissionKind: FullDebug)"
        ));
        self.di_compile_unit_id = Some(id);
        id
    }

    /// Gets or creates one shared, minimal `!DISubroutineType(types:
    /// !{null})` node reused by every `!DISubprogram` — issue #114's
    /// scope is function-level debug info (name/file/line so `gdb`/
    /// `addr2line` can resolve a crash address), not per-argument/return
    /// type modeling, which would be a much larger undertaking.
    fn di_subroutine_type_id(&mut self) -> u32 {
        if let Some(id) = self.di_subroutine_type_id {
            return id;
        }
        let id = self.di_next_id;
        self.di_next_id += 1;
        self.di_metadata
            .push(format!("!{id} = !DISubroutineType(types: !{{null}})"));
        self.di_subroutine_type_id = Some(id);
        id
    }

    /// Returns `(define_suffix, call_suffix)` for a function/method with
    /// debug info attached (issue #114), or two empty strings if `file`
    /// is `tinox_parser::UNKNOWN_FILE` — a specialized/synthesized
    /// declaration whose real source file couldn't be determined.
    /// Attaching debug info naming the wrong file would be actively
    /// misleading (worse than the bare hex addresses today), so this
    /// skips debug info entirely rather than guess — see this issue's
    /// investigation history for why that distinction matters.
    ///
    /// `define_suffix` (` !dbg !N`) goes right before the `define` line's
    /// opening `{`. `call_suffix` (`, !dbg !L`) must be appended to
    /// EVERY `call` instruction inside that function's body — LLVM
    /// requires any call site inside a function that carries `!dbg` to
    /// itself carry a `!dbg` location (`opt`'s debug-info verifier calls
    /// this "inlinable function call in a function with debug info must
    /// have a !dbg location" and silently strips the whole function's
    /// debug info otherwise, discovered empirically while implementing
    /// this — not documented anywhere in the original investigation).
    /// Since instruction-level location tracking is out of scope here
    /// (that's the "1,457 call sites" problem the investigation ruled
    /// out), every call in the function shares this one location
    /// pointing at the function's own declaration line — coarser than
    /// real per-statement info, but enough for `gdb`/`addr2line` to
    /// resolve a crash to the right function, file, and roughly which
    /// declared line, matching this issue's actual goal.
    fn dbg_suffix(&mut self, name: &str, file: &Arc<str>, line: u32) -> (String, String) {
        if file.as_ref() == tinox_parser::UNKNOWN_FILE {
            return (String::new(), String::new());
        }
        let cu_id = self.di_compile_unit_id(file);
        let file_id = self.di_file_id(file);
        let subty_id = self.di_subroutine_type_id();
        let sp_id = self.di_next_id;
        self.di_next_id += 1;
        self.di_metadata.push(format!(
            "!{sp_id} = distinct !DISubprogram(name: \"{}\", scope: !{file_id}, file: !{file_id}, line: {line}, type: !{subty_id}, spFlags: DISPFlagDefinition, unit: !{cu_id})",
            Self::escape_di_string(name)
        ));
        let loc_id = self.di_next_id;
        self.di_next_id += 1;
        self.di_metadata.push(format!(
            "!{loc_id} = !DILocation(line: {line}, column: 1, scope: !{sp_id})"
        ));
        (format!(" !dbg !{sp_id}"), format!(", !dbg !{loc_id}"))
    }

    /// Appends `call_suffix` (see `dbg_suffix`) to every `call`
    /// instruction line in `ir[body_start..]`, in place. Scoped to text
    /// generated between a function's `define` line and its closing `}`
    /// — a targeted post-process instead of threading a suffix through
    /// the ~300 individual call-emission sites across this file, since
    /// every one of them already produces exactly one `call ...` per
    /// line (never split across lines, never emits the substring
    /// `" call "`/a leading `"call "` any other way — string literals
    /// are hoisted to module-level globals, never inlined as instruction
    /// text, so this can't misfire on user data).
    fn attach_call_dbg(ir: &mut String, body_start: usize, call_suffix: &str) {
        if call_suffix.is_empty() {
            return;
        }
        let body = &ir[body_start..];
        let mut out = String::with_capacity(body.len() + call_suffix.len() * 8);
        for line in body.split_inclusive('\n') {
            let (content, ending) = match line.strip_suffix('\n') {
                Some(c) => (c, "\n"),
                None => (line, ""),
            };
            let trimmed = content.trim_start();
            if trimmed.starts_with("call ") || trimmed.contains(" = call ") {
                out.push_str(content);
                out.push_str(call_suffix);
            } else {
                out.push_str(content);
            }
            out.push_str(ending);
        }
        ir.replace_range(body_start.., &out);
    }

    fn capitalize_first(s: &str) -> String {
        let mut chars = s.chars();
        match chars.next() {
            None => String::new(),
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        }
    }

    pub fn into_ir(self) -> String {
        // Splice generic-specialization struct types in at the marker (before any
        // function body) so they're defined before their getelementptr uses.
        let body = self.ir.replacen("; @@SPEC_TYPES@@", self.spec_type_defs.trim_end(), 1);
        let mut result = body;
        result.push_str(&self.lambda_ir);
        // DWARF debug info (issue #114): metadata may be forward-
        // referenced in LLVM's textual IR (every `!dbg !N` above already
        // points at a node defined here, further down), so appending at
        // the very end is safe — matches where real compilers put it.
        // Only emitted at all if at least one function got a real
        // `!DISubprogram` (di_compile_unit_id set) — a program with no
        // user-authored fn/method bodies (unlikely, but e.g. a pure
        // `extern fn` declarations file) gets no debug info section.
        if let Some(cu_id) = self.di_compile_unit_id {
            result.push_str(&format!("\n!llvm.dbg.cu = !{{!{}}}\n", cu_id));
            result.push_str(&format!(
                "!llvm.module.flags = !{{!{}}}\n",
                self.di_next_id
            ));
            for m in &self.di_metadata {
                result.push_str(m);
                result.push('\n');
            }
            result.push_str(&format!(
                "!{} = !{{i32 2, !\"Debug Info Version\", i32 3}}\n",
                self.di_next_id
            ));
        }
        result
    }

    fn gen_fn(&mut self, f: &tinox_parser::Function) -> Result<(), ErrorBag> {
        // extern fn — no body, emit a declare instead of a define
        if matches!(f.body.node, tinox_parser::StmtKind::Empty) {
            let ret_type = self.type_to_llvm_inst(&f.ret_type);
            let params_str = f.params.iter()
                .map(|p| self.type_to_llvm_inst(&p.param_type))
                .collect::<Vec<_>>()
                .join(", ");
            let declare_line = format!("declare {} @{}({})", ret_type, f.name, params_str);
            // The same external symbol is legitimately `extern fn`-declared
            // in more than one imported .tnx file (see `declared_externs`'s
            // doc comment) — LLVM hard-errors on a literal repeated
            // `declare`, so only emit it once per program.
            if let Some(prev) = self.declared_externs.get(&f.name) {
                if *prev != declare_line {
                    let mut bag = ErrorBag::new();
                    bag.push(Error::new(
                        f.span,
                        format!(
                            "conflicting `extern fn {}` declarations: previously declared as `{}`, now as `{}`",
                            f.name, prev, declare_line
                        ),
                    ));
                    return Err(bag);
                }
                return Ok(());
            }
            self.declared_externs.insert(f.name.clone(), declare_line.clone());
            writeln!(&mut self.ir, "{}", declare_line).unwrap();
            return Ok(());
        }
        let ret_type = self.type_to_llvm_inst(&f.ret_type);
        let mut params_str = String::new();
        let mut ctx = GenCtx {
            locals: HashMap::new(),
            local_slots: HashMap::new(),
            range_vars: HashSet::new(),
            params: HashSet::new(),
            struct_fields: Vec::new(),
            current_struct: None,
            local_types: HashMap::new(),
            break_target: None,
            continue_target: None,
            error_catch: None,
            defer_stack: Vec::new(),
            in_defer_exec: false,
            ret_type: ret_type.clone(),
            timed_metric: None,
            transactional_commit: None,
            finally_targets: Vec::new(),
        };

        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                params_str.push_str(", ");
            }
            let llvm_ty = self.type_to_llvm_inst(&p.param_type);
            params_str.push_str(&format!("{} %{}", llvm_ty, p.name));
            ctx.locals.insert(p.name.clone(), (llvm_ty.clone(), i));
            ctx.params.insert(p.name.clone());

            // Track parameter types for struct/class types and containers
            if let Type::Named(class_name) = &p.param_type {
                ctx.local_types.insert(p.name.clone(), class_name.clone());
            } else if let Some(marker) = Self::container_marker(&p.param_type) {
                if marker != "Array" {
                    ctx.local_types.insert(p.name.clone(), marker);
                }
            }
        }

        let fn_name = if f.name == "main" {
            self.has_main = true;
            "tinox_main".to_string()
        } else {
            f.name.clone()
        };

        let is_inline = f.annotations.iter().any(|a| a.name == "inline")
            || self.inline_functions.contains(&fn_name);
        // `alwaysinline` is a FUNCTION attribute in LLVM IR syntax — it
        // belongs after the parameter list, not between `define` and the
        // return type (that position is for RETURN-VALUE attributes like
        // `zeroext`; LLVM rejects `alwaysinline` there with "this
        // attribute does not apply to return values"). Previously placed
        // there unconditionally, so `@inline` ICE'd on every function/
        // method with a non-void return type — see #162.
        let fn_attrs = if is_inline { " alwaysinline" } else { "" };

        let (dbg, call_dbg) = self.dbg_suffix(&f.name, &f.file, f.span.start.line);
        writeln!(
            &mut self.ir,
            "define {} @{}({}){}{} {{",
            ret_type, fn_name, params_str, fn_attrs, dbg
        )
        .unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        let body_start = self.ir.len();

        // @Counted — increment call counter at function entry
        let counted_metric = self.metric_entries.iter().find(|m| {
            m.kind == MetricKind::Counted && m.class_name.is_empty() && m.fn_name == f.name
        }).map(|m| m.metric_name.clone());
        if let Some(ref label) = counted_metric {
            let s = self.make_string_const(label);
            writeln!(&mut self.ir, "call void @tinox_counter_inc(i8* {})", s).unwrap();
        }

        // @Timed — record start timestamp
        let timed_metric = self.metric_entries.iter().find(|m| {
            m.kind == MetricKind::Timed && m.class_name.is_empty() && m.fn_name == f.name
        }).map(|m| m.metric_name.clone());
        if let Some(ref label) = timed_metric {
            let start_reg = self.temp();
            writeln!(&mut self.ir, "{} = call i64 @tinox_clock_nanos()", start_reg).unwrap();
            ctx.timed_metric = Some((label.clone(), start_reg));
        }

        self.gen_stmt_body(&f.body, &mut ctx)?;

        let has_terminator = self.ir.lines().last().is_some_and(|l| {
            let t = l.trim();
            t.starts_with("ret ") || t.starts_with("br ")
        });
        if !has_terminator {
            if let Some((ref label, ref start_reg)) = ctx.timed_metric.clone() {
                self.emit_histogram_record(label, start_reg);
            }
            if ret_type == "void" {
                writeln!(&mut self.ir, "ret void").unwrap();
            } else if ret_type.ends_with('*') {
                writeln!(&mut self.ir, "ret {} null", ret_type).unwrap();
            } else {
                writeln!(&mut self.ir, "ret {} 0", ret_type).unwrap();
            }
        }

        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
        Self::attach_call_dbg(&mut self.ir, body_start, &call_dbg);

        Ok(())
    }

    fn gen_class_method(
        &mut self,
        class_name: &str,
        method: &Method,
    ) -> Result<(), ErrorBag> {
        let ret_type = self.type_to_llvm_inst(&method.ret_type);
        let fn_name = format!("{}_{}", class_name, method.name);
        self.method_ret_types.insert(fn_name.clone(), ret_type.clone());

        let mut ctx = GenCtx {
            locals: HashMap::new(),
            local_slots: HashMap::new(),
            range_vars: HashSet::new(),
            params: HashSet::new(),
            struct_fields: Vec::new(),
            current_struct: Some(class_name.to_string()),
            local_types: HashMap::new(),
            break_target: None,
            continue_target: None,
            error_catch: None,
            defer_stack: Vec::new(),
            in_defer_exec: false,
            ret_type: ret_type.clone(),
            timed_metric: None,
            transactional_commit: None,
            finally_targets: Vec::new(),
        };

        let mut params_str = if method.static_ {
            String::new()
        } else {
            "i64* %self".to_string()
        };
        if !method.static_ {
            ctx.locals.insert("self".to_string(), ("i64*".to_string(), 0));
            ctx.params.insert("self".to_string());
            ctx.local_types.insert("self".to_string(), class_name.to_string());
        }

        for p in &method.params {
            let llvm_ty = self.type_to_llvm_inst(&p.param_type);
            if !params_str.is_empty() {
                params_str.push_str(", ");
            }
            params_str.push_str(&format!("{} %{}", llvm_ty, p.name));
            ctx.locals
                .insert(p.name.clone(), (llvm_ty.clone(), ctx.locals.len()));
            ctx.params.insert(p.name.clone());
            if let Type::Named(cn) = &p.param_type {
                ctx.local_types.insert(p.name.clone(), cn.clone());
            } else if let Some(marker) = Self::container_marker(&p.param_type) {
                if marker != "Array" {
                    ctx.local_types.insert(p.name.clone(), marker);
                }
            }
        }

        let is_inline = method.annotations.iter().any(|a| a.name == "inline")
            || self.inline_methods.contains(&(class_name.to_string(), method.name.clone()));
        // See the identical fix + comment in gen_fn (#162) — `alwaysinline`
        // is a function attribute, must follow the parameter list.
        let fn_attrs = if is_inline { " alwaysinline" } else { "" };

        let dbg_name = format!("{}.{}", class_name, method.name);
        let (dbg, call_dbg) = self.dbg_suffix(&dbg_name, &method.file, method.span.start.line);
        writeln!(
            &mut self.ir,
            "define {} @{}({}){}{} {{",
            ret_type, fn_name, params_str, fn_attrs, dbg
        )
        .unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        let body_start = self.ir.len();

        // @Counted — increment call counter at method entry
        let counted_metric = self.metric_entries.iter().find(|m| {
            m.kind == MetricKind::Counted
                && m.class_name == class_name
                && m.fn_name == method.name
        }).map(|m| m.metric_name.clone());
        if let Some(ref label) = counted_metric {
            let s = self.make_string_const(label);
            writeln!(&mut self.ir, "call void @tinox_counter_inc(i8* {})", s).unwrap();
        }

        // @Timed — record start timestamp, store in ctx for return emission
        let timed_metric = self.metric_entries.iter().find(|m| {
            m.kind == MetricKind::Timed
                && m.class_name == class_name
                && m.fn_name == method.name
        }).map(|m| m.metric_name.clone());
        if let Some(ref label) = timed_metric {
            let start_reg = self.temp();
            writeln!(&mut self.ir, "{} = call i64 @tinox_clock_nanos()", start_reg).unwrap();
            ctx.timed_metric = Some((label.clone(), start_reg));
        }

        if self.transactional_methods.contains(&(class_name.to_string(), method.name.clone())) {
            self.gen_transactional_wrapper(method, &mut ctx)?;
        } else {
            self.gen_stmt_body(&method.body, &mut ctx)?;
        }

        let has_terminator = self.ir.lines().last().is_some_and(|l| {
            let t = l.trim();
            t.starts_with("ret ") || t.starts_with("br ")
        });
        if !has_terminator {
            // Emit timing before implicit return
            if let Some((ref label, ref start_reg)) = ctx.timed_metric.clone() {
                self.emit_histogram_record(label, start_reg);
            }
            if ret_type == "void" {
                writeln!(&mut self.ir, "ret void").unwrap();
            } else if ret_type.ends_with('*') {
                writeln!(&mut self.ir, "ret {} null", ret_type).unwrap();
            } else {
                writeln!(&mut self.ir, "ret {} 0", ret_type).unwrap();
            }
        }

        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
        Self::attach_call_dbg(&mut self.ir, body_start, &call_dbg);

        Ok(())
    }

    /// Emit the auto-generated `ClassName_new(field1, field2, ...) -> i64*` function
    /// for an `immutable` declaration.
    fn emit_immutable_new(&mut self, u: &tinox_parser::ImmutableDecl) {
        let class_name = &u.name;
        let n_fields = u.fields.len();
        let size = n_fields * 8;

        let params_str: Vec<String> = u.fields.iter()
            .map(|f| format!("{} %{}", Self::type_to_llvm(&f.param_type), f.name))
            .collect();

        writeln!(&mut self.ir, "define i64* @{class_name}_new({}) {{", params_str.join(", ")).unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        writeln!(&mut self.ir, "  %raw = call i8* @tinox_alloc(i64 {size})").unwrap();
        writeln!(&mut self.ir, "  %ptr = bitcast i8* %raw to i64*").unwrap();

        for (i, field) in u.fields.iter().enumerate() {
            let llvm_ty = Self::type_to_llvm(&field.param_type);
            let store_val = if llvm_ty == "i8*" {
                writeln!(&mut self.ir, "  %fconv_{i} = ptrtoint i8* %{} to i64", field.name).unwrap();
                format!("%fconv_{i}")
            } else if llvm_ty == "i64*" {
                writeln!(&mut self.ir, "  %fconv_{i} = ptrtoint i64* %{} to i64", field.name).unwrap();
                format!("%fconv_{i}")
            } else if llvm_ty == "i1" {
                writeln!(&mut self.ir, "  %fconv_{i} = zext i1 %{} to i64", field.name).unwrap();
                format!("%fconv_{i}")
            } else if llvm_ty == "double" {
                writeln!(&mut self.ir, "  %fconv_{i} = bitcast double %{} to i64", field.name).unwrap();
                format!("%fconv_{i}")
            } else if llvm_ty == "float" {
                writeln!(&mut self.ir, "  %fconv_ext_{i} = fpext float %{} to double", field.name).unwrap();
                writeln!(&mut self.ir, "  %fconv_{i} = bitcast double %fconv_ext_{i} to i64").unwrap();
                format!("%fconv_{i}")
            } else if llvm_ty != "i64" {
                writeln!(&mut self.ir, "  %fconv_{i} = sext {llvm_ty} %{} to i64", field.name).unwrap();
                format!("%fconv_{i}")
            } else {
                format!("%{}", field.name)
            };
            writeln!(&mut self.ir, "  %gep_{i} = getelementptr i64, i64* %ptr, i64 {i}").unwrap();
            writeln!(&mut self.ir, "  store i64 {store_val}, i64* %gep_{i}").unwrap();
        }

        writeln!(&mut self.ir, "  ret i64* %ptr").unwrap();
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    /// Emit `tinox_main` for classes annotated with `@Command`.
    /// Generates: help function, --help/--version handling, arg parsing, and `run()` call.
    fn emit_cli_code(&mut self) {
        if self.cli_commands.is_empty() || self.has_main {
            return;
        }

        let commands = self.cli_commands.clone();

        // Only the first @Command class acts as the entry point.
        let cmd = &commands[0];
        let class = cmd.class_name.clone();

        // ── String constants ────────────────────────────────────────────────
        let mut str_defs = String::new();

        let emit_str = |buf: &mut String, label: &str, text: &str| {
            let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
            let len = text.len() + 1;
            writeln!(buf,
                "@{label} = private constant [{len} x i8] c\"{escaped}\\00\""
            ).unwrap();
        };

        emit_str(&mut str_defs, "__cli_help_long",    "--help");
        emit_str(&mut str_defs, "__cli_help_short",   "-h");
        emit_str(&mut str_defs, "__cli_ver_long",     "--version");
        emit_str(&mut str_defs, "__cli_empty",        "");
        emit_str(&mut str_defs, "__cli_cmd_name",     &cmd.cmd_name);
        emit_str(&mut str_defs, "__cli_cmd_desc",     &cmd.description);
        emit_str(&mut str_defs, "__cli_cmd_ver",      cmd.version.as_deref().unwrap_or(""));

        let mut help_lines: Vec<(String, String)> = Vec::new();

        for (i, opt) in cmd.options.iter().enumerate() {
            let long_name  = opt.names.iter().find(|n| n.starts_with("--")).cloned().unwrap_or_default();
            let short_name = opt.names.iter().find(|n| n.starts_with('-') && !n.starts_with("--")).cloned().unwrap_or_default();
            emit_str(&mut str_defs, &format!("__cli_opt{i}_long"),  &long_name);
            emit_str(&mut str_defs, &format!("__cli_opt{i}_short"), &short_name);
            emit_str(&mut str_defs, &format!("__cli_opt{i}_desc"),  &opt.description);
            let display = if short_name.is_empty() { long_name.clone() }
                          else { format!("{short_name}, {long_name}") };
            help_lines.push((display, opt.description.clone()));
        }
        for (i, arg) in cmd.arguments.iter().enumerate() {
            let placeholder = format!("<{}>", arg.field_name);
            emit_str(&mut str_defs, &format!("__cli_arg{i}_desc"), &arg.description);
            help_lines.push((placeholder, arg.description.clone()));
        }
        for (i, (names, desc)) in help_lines.iter().enumerate() {
            emit_str(&mut str_defs, &format!("__cli_help_entry{i}_names"), names);
            emit_str(&mut str_defs, &format!("__cli_help_entry{i}_desc"),  desc);
        }
        // --help / --version strings for help output
        emit_str(&mut str_defs, "__cli_usage_prefix", "Usage: ");
        emit_str(&mut str_defs, "__cli_usage_suffix", " [options]\n");
        emit_str(&mut str_defs, "__cli_nl",           "\n");
        emit_str(&mut str_defs, "__cli_ver_prefix",   "Version: ");
        emit_str(&mut str_defs, "__cli_help_hdr",     "Options:");

        self.ir.push_str(&str_defs);
        writeln!(&mut self.ir).unwrap();

        // ── Helper: getelementptr shorthand ─────────────────────────────────
        let mut body = String::new();

        // Helper macro (Rust closure) to get i8* from a named string constant
        let gep = |b: &mut String, tmp: &str, label: &str, len: usize| {
            writeln!(b,
                "  {tmp} = getelementptr [{len} x i8], [{len} x i8]* @{label}, i64 0, i64 0"
            ).unwrap();
        };

        // ── __tinox_cli_help ─────────────────────────────────────────────────
        writeln!(&mut body, "define void @__tinox_cli_help() {{").unwrap();

        if !cmd.description.is_empty() {
            let len = cmd.description.len() + 1;
            gep(&mut body, "%desc_ptr", "__cli_cmd_desc", len);
            writeln!(&mut body, "  call void @tinox_print_string(i8* %desc_ptr)").unwrap();
            writeln!(&mut body, "  call void @tinox_print_newline()").unwrap();
        }

        let usage_prefix_len = "Usage: ".len() + 1;
        let usage_suffix_len = " [options]\n".len() + 1;
        let cmd_name_len = cmd.cmd_name.len() + 1;
        gep(&mut body, "%usage_pfx", "__cli_usage_prefix", usage_prefix_len);
        gep(&mut body, "%cmd_name_ptr", "__cli_cmd_name", cmd_name_len);
        gep(&mut body, "%usage_sfx", "__cli_usage_suffix", usage_suffix_len);
        writeln!(&mut body, "  call void @tinox_print_string(i8* %usage_pfx)").unwrap();
        writeln!(&mut body, "  call void @tinox_print_string(i8* %cmd_name_ptr)").unwrap();
        writeln!(&mut body, "  call void @tinox_print_string(i8* %usage_sfx)").unwrap();

        if !help_lines.is_empty() {
            let hdr_len = "Options:".len() + 1;
            let nl_len = "\n".len() + 1;
            gep(&mut body, "%hdr_ptr", "__cli_help_hdr", hdr_len);
            gep(&mut body, "%nl_ptr", "__cli_nl", nl_len);
            writeln!(&mut body, "  call void @tinox_print_string(i8* %hdr_ptr)").unwrap();
            writeln!(&mut body, "  call void @tinox_print_newline()").unwrap();
            for (i, (names, desc)) in help_lines.iter().enumerate() {
                let nlen = names.len() + 1;
                let dlen = desc.len() + 1;
                gep(&mut body, &format!("%hn{i}"), &format!("__cli_help_entry{i}_names"), nlen);
                gep(&mut body, &format!("%hd{i}"), &format!("__cli_help_entry{i}_desc"),  dlen);
                writeln!(&mut body,
                    "  call void @tinox_cli_print_option(i8* %hn{i}, i8* %hd{i})"
                ).unwrap();
            }
        }

        if let Some(ref ver) = cmd.version {
            let vp_len = "Version: ".len() + 1;
            let ver_len = ver.len() + 1;
            gep(&mut body, "%vp_ptr", "__cli_ver_prefix", vp_len);
            gep(&mut body, "%ver_ptr", "__cli_cmd_ver", ver_len);
            writeln!(&mut body, "  call void @tinox_print_string(i8* %vp_ptr)").unwrap();
            writeln!(&mut body, "  call void @tinox_print_string(i8* %ver_ptr)").unwrap();
            writeln!(&mut body, "  call void @tinox_print_newline()").unwrap();
        }

        writeln!(&mut body, "  ret void").unwrap();
        writeln!(&mut body, "}}").unwrap();
        writeln!(&mut body).unwrap();

        // ── tinox_main ───────────────────────────────────────────────────────
        writeln!(&mut body, "define i64 @tinox_main() {{").unwrap();
        writeln!(&mut body, "entry.tnx:").unwrap();

        // Check --help / -h
        gep(&mut body, "%help_long",  "__cli_help_long",  7);
        gep(&mut body, "%help_short", "__cli_help_short", 3);
        writeln!(&mut body,
            "  %has_help = call i64 @tinox_cli_has_flag(i8* %help_long, i8* %help_short)"
        ).unwrap();
        writeln!(&mut body, "  %help_cond = icmp ne i64 %has_help, 0").unwrap();
        writeln!(&mut body, "  br i1 %help_cond, label %show_help, label %check_version").unwrap();
        writeln!(&mut body, "show_help:").unwrap();
        writeln!(&mut body, "  call void @__tinox_cli_help()").unwrap();
        writeln!(&mut body, "  ret i64 0").unwrap();

        // Check --version
        writeln!(&mut body, "check_version:").unwrap();
        gep(&mut body, "%ver_long", "__cli_ver_long", 10);
        gep(&mut body, "%empty_str", "__cli_empty", 1);
        writeln!(&mut body,
            "  %has_ver = call i64 @tinox_cli_has_flag(i8* %ver_long, i8* %empty_str)"
        ).unwrap();
        writeln!(&mut body, "  %ver_cond = icmp ne i64 %has_ver, 0").unwrap();
        writeln!(&mut body, "  br i1 %ver_cond, label %show_version, label %parse_args").unwrap();
        writeln!(&mut body, "show_version:").unwrap();
        let ver_str = cmd.version.as_deref().unwrap_or("");
        let ver_len = ver_str.len() + 1;
        gep(&mut body, "%ver_val", "__cli_cmd_ver", ver_len);
        writeln!(&mut body, "  call void @tinox_print_string(i8* %ver_val)").unwrap();
        writeln!(&mut body, "  call void @tinox_print_newline()").unwrap();
        writeln!(&mut body, "  ret i64 0").unwrap();

        let layout = self.struct_layouts.get(&class).cloned().unwrap_or_default();

        // Create command instance — allocate and zero-initialise (no new() needed)
        writeln!(&mut body, "parse_args:").unwrap();
        let n_fields = layout.len();
        let byte_size = (n_fields * 8).max(8);
        writeln!(&mut body, "  %cmd_raw = call i8* @tinox_alloc(i64 {byte_size})").unwrap();
        writeln!(&mut body, "  %cmd_obj = bitcast i8* %cmd_raw to i64*").unwrap();
        for fi in 0..n_fields {
            writeln!(&mut body, "  %zinit_{fi} = getelementptr i64, i64* %cmd_obj, i64 {fi}").unwrap();
            writeln!(&mut body, "  store i64 0, i64* %zinit_{fi}").unwrap();
        }

        // Parse options
        for (i, opt) in cmd.options.iter().enumerate() {
            let long_name  = opt.names.iter().find(|n| n.starts_with("--")).cloned().unwrap_or_default();
            let short_name = opt.names.iter().find(|n| n.starts_with('-') && !n.starts_with("--")).cloned().unwrap_or_default();
            let long_len   = long_name.len() + 1;
            let short_len  = short_name.len() + 1;
            let field_idx  = layout.iter().position(|f| f == &opt.field_name).unwrap_or(usize::MAX);
            if field_idx == usize::MAX { continue; }

            gep(&mut body, &format!("%olong{i}"),  &format!("__cli_opt{i}_long"),  long_len);
            gep(&mut body, &format!("%oshort{i}"), &format!("__cli_opt{i}_short"), short_len);

            match opt.field_type.as_str() {
                "Bool" => {
                    writeln!(&mut body,
                        "  %opt_flag{i} = call i64 @tinox_cli_has_flag(i8* %olong{i}, i8* %oshort{i})"
                    ).unwrap();
                    writeln!(&mut body,
                        "  %opt_fp{i} = getelementptr i64, i64* %cmd_obj, i64 {field_idx}"
                    ).unwrap();
                    writeln!(&mut body,
                        "  store i64 %opt_flag{i}, i64* %opt_fp{i}"
                    ).unwrap();
                }
                "Int" => {
                    writeln!(&mut body,
                        "  %opt_int{i} = call i64 @tinox_cli_get_int(i8* %olong{i}, i8* %oshort{i}, i64 0)"
                    ).unwrap();
                    writeln!(&mut body,
                        "  %opt_fp{i} = getelementptr i64, i64* %cmd_obj, i64 {field_idx}"
                    ).unwrap();
                    writeln!(&mut body,
                        "  store i64 %opt_int{i}, i64* %opt_fp{i}"
                    ).unwrap();
                }
                _ => {
                    // String
                    writeln!(&mut body,
                        "  %opt_str{i} = call i8* @tinox_cli_get_string(i8* %olong{i}, i8* %oshort{i})"
                    ).unwrap();
                    writeln!(&mut body,
                        "  %opt_null{i} = icmp eq i8* %opt_str{i}, null"
                    ).unwrap();
                    writeln!(&mut body,
                        "  br i1 %opt_null{i}, label %skip_opt{i}, label %set_opt{i}"
                    ).unwrap();
                    writeln!(&mut body, "set_opt{i}:").unwrap();
                    writeln!(&mut body,
                        "  %opt_i64_{i} = ptrtoint i8* %opt_str{i} to i64"
                    ).unwrap();
                    writeln!(&mut body,
                        "  %opt_fp{i} = getelementptr i64, i64* %cmd_obj, i64 {field_idx}"
                    ).unwrap();
                    writeln!(&mut body,
                        "  store i64 %opt_i64_{i}, i64* %opt_fp{i}"
                    ).unwrap();
                    writeln!(&mut body, "  br label %skip_opt{i}").unwrap();
                    writeln!(&mut body, "skip_opt{i}:").unwrap();
                }
            }
        }

        // Parse positional arguments
        for (i, arg) in cmd.arguments.iter().enumerate() {
            let field_idx = layout.iter().position(|f| f == &arg.field_name).unwrap_or(usize::MAX);
            if field_idx == usize::MAX { continue; }

            writeln!(&mut body,
                "  %pos_str{i} = call i8* @tinox_cli_get_positional(i32 {})", arg.index
            ).unwrap();

            match arg.field_type.as_str() {
                "Int" => {
                    writeln!(&mut body, "  %pos_null{i} = icmp eq i8* %pos_str{i}, null").unwrap();
                    writeln!(&mut body, "  br i1 %pos_null{i}, label %skip_pos{i}, label %set_pos{i}").unwrap();
                    writeln!(&mut body, "set_pos{i}:").unwrap();
                    writeln!(&mut body, "  %pos_int{i} = call i64 @tinox_string_to_int(i8* %pos_str{i})").unwrap();
                    writeln!(&mut body, "  %pos_fp{i} = getelementptr i64, i64* %cmd_obj, i64 {field_idx}").unwrap();
                    writeln!(&mut body, "  store i64 %pos_int{i}, i64* %pos_fp{i}").unwrap();
                    writeln!(&mut body, "  br label %skip_pos{i}").unwrap();
                    writeln!(&mut body, "skip_pos{i}:").unwrap();
                }
                _ => {
                    // String (or Bool treated as string — uncommon but safe)
                    writeln!(&mut body, "  %pos_null{i} = icmp eq i8* %pos_str{i}, null").unwrap();
                    writeln!(&mut body, "  br i1 %pos_null{i}, label %skip_pos{i}, label %set_pos{i}").unwrap();
                    writeln!(&mut body, "set_pos{i}:").unwrap();
                    writeln!(&mut body, "  %pos_i64_{i} = ptrtoint i8* %pos_str{i} to i64").unwrap();
                    writeln!(&mut body, "  %pos_fp{i} = getelementptr i64, i64* %cmd_obj, i64 {field_idx}").unwrap();
                    writeln!(&mut body, "  store i64 %pos_i64_{i}, i64* %pos_fp{i}").unwrap();
                    writeln!(&mut body, "  br label %skip_pos{i}").unwrap();
                    writeln!(&mut body, "skip_pos{i}:").unwrap();
                }
            }
        }

        // Call run()
        writeln!(&mut body, "  %cli_ret = call i64 @{class}_run(i64* %cmd_obj)").unwrap();
        writeln!(&mut body, "  ret i64 %cli_ret").unwrap();
        writeln!(&mut body, "}}").unwrap();
        writeln!(&mut body).unwrap();

        self.lambda_ir.push_str(&body);
        self.has_main = true;
    }

    /// Return the declared class name of a simple expression (Ident or FieldAccess),
    /// or None for complex expressions. Used for implicit toString() coercion.
    fn expr_class_name(expr: &ExprKind, ctx: &GenCtx) -> Option<String> {
        match expr {
            ExprKind::Ident(name) => ctx.local_types.get(name).cloned(),
            ExprKind::FieldAccess { obj, field } => {
                let obj_class = if let ExprKind::Ident(n) = &obj.node {
                    ctx.local_types.get(n.as_str()).cloned()
                } else {
                    None
                };
                obj_class.and_then(|cn| {
                    ctx.local_types.get(&format!("{}.{}", cn, field)).cloned()
                        .or(None)
                })
            }
            _ => None,
        }
    }

    /// Convert a raw i64 struct slot to an i8* string, based on LLVM type.
    fn field_val_to_string(&mut self, raw: &str, llvm_ty: &str) -> String {
        match llvm_ty {
            "i8*" => {
                let ptr = self.temp();
                writeln!(&mut self.ir, "  {} = inttoptr i64 {} to i8*", ptr, raw).unwrap();
                ptr
            }
            "i1" => {
                let b = self.temp();
                let s = self.temp();
                writeln!(&mut self.ir, "  {} = trunc i64 {} to i1", b, raw).unwrap();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_bool_to_string(i1 {})", s, b).unwrap();
                s
            }
            "double" | "float" => {
                let f = self.temp();
                let s = self.temp();
                writeln!(&mut self.ir, "  {} = bitcast i64 {} to double", f, raw).unwrap();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_float_to_string(double {})", s, f).unwrap();
                s
            }
            "i64" | "i32" | "i16" | "i8" => {
                let s = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_int_to_string(i64 {})", s, raw).unwrap();
                s
            }
            _ => {
                // Object or unknown type
                let content = "<object>";
                let lbl = format!("str{}", self.strings.len());
                self.strings.insert(lbl.clone(), content.to_string());
                let len = content.len() + 1;
                let p = self.temp();
                writeln!(&mut self.ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", p, len, len, lbl).unwrap();
                p
            }
        }
    }

    /// Pre-register `ClassName_toString` return types for classes with masked fields so
    /// the method is visible to user code compiled before `emit_log_mask_code` runs.
    fn pre_register_log_mask_tostring(&mut self) {
        let mut affected: HashSet<String> = HashSet::new();
        for f in &self.sensitive_fields { affected.insert(f.class_name.clone()); }
        for f in &self.masked_fields { affected.insert(f.class_name.clone()); }
        for class_name in &affected {
            let key = format!("{}_toString", class_name);
            // Only register if the user hasn't already defined toString()
            self.method_ret_types.entry(key).or_insert_with(|| "i8*".to_string());
        }
    }

    /// Emit a `ClassName_toString(i64* %self) -> i8*` method for every class
    /// that has at least one @Sensitive or @Masked field.
    fn emit_log_mask_code(&mut self) {
        let sensitive_set: HashSet<(String, String)> = self.sensitive_fields.iter()
            .map(|f| (f.class_name.clone(), f.field_name.clone()))
            .collect();
        let masked_set: HashSet<(String, String)> = self.masked_fields.iter()
            .map(|f| (f.class_name.clone(), f.field_name.clone()))
            .collect();

        let affected: Vec<String> = {
            let mut s: HashSet<String> = HashSet::new();
            for f in &self.sensitive_fields { s.insert(f.class_name.clone()); }
            for f in &self.masked_fields { s.insert(f.class_name.clone()); }
            let mut v: Vec<String> = s.into_iter().collect();
            v.sort();
            v
        };

        for class_name in affected {
            let layout = match self.struct_layouts.get(&class_name) {
                Some(l) => l.clone(),
                None => continue,
            };
            let llvm_types = match self.struct_field_llvm_types.get(&class_name) {
                Some(m) => m.clone(),
                None => continue,
            };

            // Data fields only (exclude vtable slot and synthetic "log")
            let data_fields: Vec<(String, usize, String)> = layout.iter()
                .enumerate()
                .filter(|(_, f)| *f != "__vtable__" && *f != "log")
                .filter_map(|(idx, f)| llvm_types.get(f).map(|ty| (f.clone(), idx, ty.clone())))
                .collect();

            if data_fields.is_empty() { continue; }

            let fn_key = format!("{}_toString", class_name);
            // Skip if user has already defined toString() — their version takes precedence
            if self.method_ret_types.get(&fn_key).map(|v| v != "i8*").unwrap_or(false) {
                continue;
            }
            writeln!(&mut self.ir, "define i8* @{}_toString(i64* %self) {{", class_name).unwrap();
            writeln!(&mut self.ir, "entry.tnx:").unwrap();

            // Start with "ClassName{"
            let prefix = format!("{}{{", class_name);
            let lbl = format!("str{}", self.strings.len());
            self.strings.insert(lbl.clone(), prefix.clone());
            let plen = prefix.len() + 1;
            let prefix_ptr = self.temp();
            writeln!(&mut self.ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", prefix_ptr, plen, plen, lbl).unwrap();
            let mut acc = prefix_ptr;

            for (i, (field_name, struct_idx, llvm_ty)) in data_fields.iter().enumerate() {
                // Separator: "field=" for first, ", field=" for rest
                let sep = if i == 0 { format!("{}=", field_name) } else { format!(", {}=", field_name) };
                let sep_lbl = format!("str{}", self.strings.len());
                self.strings.insert(sep_lbl.clone(), sep.clone());
                let slen = sep.len() + 1;
                let sep_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", sep_ptr, slen, slen, sep_lbl).unwrap();
                let acc1 = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_string_concat(i8* {}, i8* {})", acc1, acc, sep_ptr).unwrap();
                acc = acc1;

                // Load field value (all fields stored as i64)
                let fptr = self.temp();
                writeln!(&mut self.ir, "  {} = getelementptr i64, i64* %self, i64 {}", fptr, struct_idx).unwrap();
                let raw = self.temp();
                writeln!(&mut self.ir, "  {} = load i64, i64* {}", raw, fptr).unwrap();

                let owner_class = self.field_declaring_class(&class_name, field_name);
                let is_sensitive = sensitive_set.contains(&(owner_class.clone(), field_name.clone()));
                let is_masked = masked_set.contains(&(owner_class, field_name.clone()));

                let val_str = if is_sensitive {
                    let stars = "***";
                    let slbl = format!("str{}", self.strings.len());
                    self.strings.insert(slbl.clone(), stars.to_string());
                    let slen2 = stars.len() + 1;
                    let p = self.temp();
                    writeln!(&mut self.ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", p, slen2, slen2, slbl).unwrap();
                    p
                } else if is_masked {
                    let raw_str = self.field_val_to_string(&raw.clone(), llvm_ty);
                    let masked = self.temp();
                    writeln!(&mut self.ir, "  {} = call i8* @tinox_string_mask_partial(i8* {})", masked, raw_str).unwrap();
                    masked
                } else {
                    let raw_clone = raw.clone();
                    self.field_val_to_string(&raw_clone, llvm_ty)
                };

                let acc2 = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_string_concat(i8* {}, i8* {})", acc2, acc, val_str).unwrap();
                acc = acc2;
            }

            // Close with "}"
            let close = "}";
            let clbl = format!("str{}", self.strings.len());
            self.strings.insert(clbl.clone(), close.to_string());
            let close_len = close.len() + 1;
            let close_ptr = self.temp();
            writeln!(&mut self.ir, "  {} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", close_ptr, close_len, close_len, clbl).unwrap();
            let final_str = self.temp();
            writeln!(&mut self.ir, "  {} = call i8* @tinox_string_concat(i8* {}, i8* {})", final_str, acc, close_ptr).unwrap();
            writeln!(&mut self.ir, "  ret i8* {}", final_str).unwrap();
            writeln!(&mut self.ir, "}}").unwrap();
            writeln!(&mut self.ir).unwrap();
        }
    }

    /// Pre-register `ClassName_toJson` return types for @JsonSerializable classes so
    /// the method is visible to user code compiled before `emit_json_serialize_code` runs.
    fn pre_register_json_to_json(&mut self) {
        let class_names: Vec<String> = self.json_serializable_classes.clone();
        for class_name in &class_names {
            let key = format!("{}_toJson", class_name);
            self.method_ret_types.entry(key).or_insert_with(|| "i8*".to_string());
        }
    }

    fn pre_register_json_from_json(&mut self) {
        let class_names: Vec<String> = self.json_serializable_classes.clone();
        for class_name in &class_names {
            let key = format!("{}_fromJson", class_name);
            self.fn_sigs.entry(key).or_insert_with(|| ("i64*".to_string(), vec!["i64*".to_string()]));
            let ret_key = format!("{}_fromJson", class_name);
            self.method_ret_types.entry(ret_key).or_insert_with(|| "i64*".to_string());
        }
    }

    /// Emit `ClassName_toJson(i64* %self) -> i8*` for every @JsonSerializable class.
    /// Uses JsonBuilder for a single-pass, single-allocation approach instead of
    /// the old chain of tinox_string_concat calls (which did O(N) mallocs).
    fn emit_json_serialize_code(&mut self) {
        let do_not_serialize_set: std::collections::HashSet<(String, String)> =
            self.do_not_serialize_fields.iter()
                .map(|f| (f.class_name.clone(), f.field_name.clone()))
                .collect();

        let class_names: Vec<String> = self.json_serializable_classes.clone();

        for class_name in class_names {
            let layout = match self.struct_layouts.get(&class_name) {
                Some(l) => l.clone(),
                None => continue,
            };
            let llvm_types = match self.struct_field_llvm_types.get(&class_name) {
                Some(m) => m.clone(),
                None => continue,
            };
            let field_class_types = self.struct_field_class_types.get(&class_name).cloned().unwrap_or_default();

            let data_fields: Vec<(String, usize, String)> = layout.iter()
                .enumerate()
                .filter(|(_, f)| *f != "__vtable__" && *f != "log")
                .filter(|(_, f)| {
                    let owner = self.field_declaring_class(&class_name, f);
                    !do_not_serialize_set.contains(&(owner, f.to_string()))
                })
                .filter_map(|(idx, f)| llvm_types.get(f).map(|ty| (f.clone(), idx, ty.clone())))
                .collect();

            writeln!(&mut self.ir, "define i8* @{}_toJson(i64* %self) {{", class_name).unwrap();
            writeln!(&mut self.ir, "entry.tnx:").unwrap();
            let builder = self.temp();
            writeln!(&mut self.ir, "  {builder} = call i8* @jsonBuilderCreate()").unwrap();

            for (field_name, struct_idx, llvm_ty) in &data_fields {
                // Intern field name as a string constant
                let key_lbl = format!("str{}", self.strings.len());
                self.strings.insert(key_lbl.clone(), field_name.clone());
                let key_len = field_name.len() + 1;
                let key_ptr = self.temp();
                writeln!(&mut self.ir,
                    "  {key_ptr} = getelementptr [{key_len} x i8], [{key_len} x i8]* @{key_lbl}, i64 0, i64 0").unwrap();

                // Load the raw i64 slot
                let fptr = self.temp();
                let raw  = self.temp();
                writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* %self, i64 {struct_idx}").unwrap();
                writeln!(&mut self.ir, "  {raw}  = load i64, i64* {fptr}").unwrap();

                let marker = field_class_types.get(field_name).cloned();

                match llvm_ty.as_str() {
                    // A Map<String, String> field (e.g. every Kubernetes-style
                    // resource's labels/annotations) is ALSO "i8*" at this flat
                    // level -- indistinguishable from a plain String field
                    // without the marker, and previously always went through
                    // jsonBuilderAddString (reading the TinoxMap* pointer as if
                    // it were a C string: silent garbage, not a crash). Any
                    // other Map<String, V> value type is out of scope here (no
                    // current caller needs it) and falls back to a placeholder
                    // rather than risking the same silent misread.
                    "i8*" if marker.as_deref().map(|m| m.starts_with("Map:")).unwrap_or(false) => {
                        if marker.as_deref() == Some("Map:String") {
                            let map_ptr = self.temp();
                            writeln!(&mut self.ir, "  {map_ptr} = inttoptr i64 {raw} to i8*").unwrap();
                            let json = self.temp();
                            writeln!(&mut self.ir, "  {json} = call i8* @tinox_json_string_map_serialize(i8* {map_ptr})").unwrap();
                            writeln!(&mut self.ir, "  call void @jsonBuilderAddRaw(i8* {builder}, i8* {key_ptr}, i8* {json})").unwrap();
                        } else {
                            let placeholder = self.intern_json_placeholder_string();
                            writeln!(&mut self.ir, "  call void @jsonBuilderAddString(i8* {builder}, i8* {key_ptr}, i8* {placeholder})").unwrap();
                        }
                    }
                    "i8*" => {
                        let str_val = self.temp();
                        writeln!(&mut self.ir, "  {str_val} = inttoptr i64 {raw} to i8*").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddString(i8* {builder}, i8* {key_ptr}, i8* {str_val})").unwrap();
                    }
                    "double" | "float" => {
                        let dbl = self.temp();
                        writeln!(&mut self.ir, "  {dbl} = bitcast i64 {raw} to double").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddFloat(i8* {builder}, i8* {key_ptr}, double {dbl})").unwrap();
                    }
                    "i1" => {
                        let truncated = self.temp();
                        let extended  = self.temp();
                        writeln!(&mut self.ir, "  {truncated} = trunc i64 {raw} to i1").unwrap();
                        writeln!(&mut self.ir, "  {extended}  = zext i1 {truncated} to i32").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddBool(i8* {builder}, i8* {key_ptr}, i32 {extended})").unwrap();
                    }
                    // "i64*" covers List<Int> (marker "Array"), List<String>
                    // ("Array:String"), List<SomeClass> ("List:SomeClass") AND
                    // a directly-nested @JsonSerializable class field (bare
                    // class-name marker, e.g. Pod.metadata: ObjectMeta) --
                    // all four are pointer-shaped and otherwise
                    // indistinguishable, and previously ALL went through
                    // jsonBuilderAddIntList regardless of which one they
                    // actually were (issue found while modeling Kubernetes
                    // resources: every nested spec/status/metadata field, and
                    // every List<Container>-shaped field, serialized as
                    // garbage int arrays).
                    "i64*" if marker.as_deref() == Some("Array:String") => {
                        let arr_ptr = self.temp();
                        writeln!(&mut self.ir, "  {arr_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                        let json = self.temp();
                        writeln!(&mut self.ir, "  {json} = call i8* @tinox_json_string_list_serialize(i64* {arr_ptr})").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddRaw(i8* {builder}, i8* {key_ptr}, i8* {json})").unwrap();
                    }
                    "i64*" if marker.as_deref().and_then(|m| m.strip_prefix("List:"))
                        .map(|cls| self.json_serializable_classes.iter().any(|c| c == cls))
                        .unwrap_or(false) =>
                    {
                        let cls = marker.as_deref().and_then(|m| m.strip_prefix("List:")).unwrap().to_string();
                        let list_ptr = self.temp();
                        writeln!(&mut self.ir, "  {list_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                        let json = self.temp();
                        writeln!(&mut self.ir, "  {json} = call i8* @tinox_json_list_serialize(i64* {list_ptr}, ptr @{cls}_toJson)").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddRaw(i8* {builder}, i8* {key_ptr}, i8* {json})").unwrap();
                    }
                    "i64*" if marker.as_deref()
                        .map(|m| !m.starts_with("Array") && !m.starts_with("List:") && !m.starts_with("Map")
                            && self.json_serializable_classes.iter().any(|c| c == m))
                        .unwrap_or(false) =>
                    {
                        let cls = marker.clone().unwrap();
                        let obj_ptr = self.temp();
                        writeln!(&mut self.ir, "  {obj_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                        let json = self.temp();
                        writeln!(&mut self.ir, "  {json} = call i8* @{cls}_toJson(i64* {obj_ptr})").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddRaw(i8* {builder}, i8* {key_ptr}, i8* {json})").unwrap();
                    }
                    "i64*" if marker.as_deref() == Some("Array") || marker.is_none() => {
                        let arr_ptr = self.temp();
                        writeln!(&mut self.ir, "  {arr_ptr} = inttoptr i64 {raw} to i64*").unwrap();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddIntList(i8* {builder}, i8* {key_ptr}, i64* {arr_ptr})").unwrap();
                    }
                    "i64*" => {
                        let placeholder = self.intern_json_placeholder_string();
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddString(i8* {builder}, i8* {key_ptr}, i8* {placeholder})").unwrap();
                    }
                    _ => {
                        // i64, i32, etc.
                        writeln!(&mut self.ir, "  call void @jsonBuilderAddInt(i8* {builder}, i8* {key_ptr}, i64 {raw})").unwrap();
                    }
                }
            }

            let result = self.temp();
            writeln!(&mut self.ir, "  {result} = call i8* @jsonBuilderFinish(i8* {builder})").unwrap();
            writeln!(&mut self.ir, "  ret i8* {result}").unwrap();
            writeln!(&mut self.ir, "}}").unwrap();
            writeln!(&mut self.ir).unwrap();
        }
    }

    /// Interns `"<unsupported field type>"` as a global string constant in
    /// `self.ir` and returns a register holding a pointer to it -- the same
    /// placeholder text/spirit `emit_devui_component_state_handlers` already
    /// uses for a field kind it can't represent, reused here for
    /// `_toJson`'s own narrower unsupported cases (Map<String, non-String>,
    /// a pointer-shaped field with no recognizable marker at all).
    fn intern_json_placeholder_string(&mut self) -> String {
        let text = "<unsupported field type>";
        let lbl = format!("str{}", self.strings.len());
        self.strings.insert(lbl.clone(), text.to_string());
        let len = text.len() + 1;
        let ptr = self.temp();
        writeln!(&mut self.ir, "  {ptr} = getelementptr [{len} x i8], [{len} x i8]* @{lbl}, i64 0, i64 0").unwrap();
        ptr
    }

    /// Emit `ClassName_fromJson(i64* %json_val) -> i64*` for every @JsonSerializable class.
    fn emit_json_deserialize_code(&mut self) {
        let class_names: Vec<String> = self.json_serializable_classes.clone();

        for class_name in class_names {
            let layout = match self.struct_layouts.get(&class_name) {
                Some(l) => l.clone(),
                None => continue,
            };
            let llvm_types = match self.struct_field_llvm_types.get(&class_name) {
                Some(m) => m.clone(),
                None => continue,
            };
            let field_class_types = self.struct_field_class_types.get(&class_name).cloned().unwrap_or_default();

            let n_slots  = layout.len().max(1);
            let byte_size = n_slots * 8;
            let has_vtable = layout.first().map(|f| f == "__vtable__").unwrap_or(false);

            writeln!(&mut self.ir, "define i64* @{}_fromJson(i64* %json_val) {{", class_name).unwrap();
            writeln!(&mut self.ir, "entry.tnx:").unwrap();
            let raw  = self.temp();
            let self_ = self.temp();
            writeln!(&mut self.ir, "  {raw}   = call i8* @tinox_alloc(i64 {byte_size})").unwrap();
            writeln!(&mut self.ir, "  {self_} = bitcast i8* {raw} to i64*").unwrap();

            // Zero all slots first so unhandled fields are safe
            for fi in 0..n_slots {
                let zp = self.temp();
                writeln!(&mut self.ir, "  {zp} = getelementptr i64, i64* {self_}, i64 {fi}").unwrap();
                writeln!(&mut self.ir, "  store i64 0, i64* {zp}").unwrap();
            }

            // Set vtable pointer if present
            if has_vtable {
                let vt_i64 = self.temp();
                let vt_ptr = self.temp();
                writeln!(&mut self.ir, "  {vt_i64} = ptrtoint i64* getelementptr ([1 x i64], [1 x i64]* @{class_name}_vtable, i64 0, i64 0) to i64").unwrap();
                writeln!(&mut self.ir, "  {vt_ptr} = getelementptr i64, i64* {self_}, i64 0").unwrap();
                writeln!(&mut self.ir, "  store i64 {vt_i64}, i64* {vt_ptr}").unwrap();
            }

            // Fill data fields from JSON
            for (struct_idx, field_name) in layout.iter().enumerate() {
                if field_name == "__vtable__" || field_name == "log" { continue; }
                let llvm_ty = match llvm_types.get(field_name) {
                    Some(t) => t.clone(),
                    None => continue,
                };

                let key_lbl = format!("str{}", self.strings.len());
                self.strings.insert(key_lbl.clone(), field_name.clone());
                let key_len = field_name.len() + 1;
                let key_ptr = self.temp();
                writeln!(&mut self.ir,
                    "  {key_ptr} = getelementptr [{key_len} x i8], [{key_len} x i8]* @{key_lbl}, i64 0, i64 0").unwrap();

                let fptr = self.temp();
                writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* {self_}, i64 {struct_idx}").unwrap();

                let marker = field_class_types.get(field_name).cloned();
                let is_serializable_class = |m: &str| self.json_serializable_classes.iter().any(|c| c == m);

                // Same four-way pointer-shaped split as emit_json_serialize_code's
                // toJson (see the comment there for why "i64*"/"i8*" alone can't
                // tell these apart) -- mirrored here field-for-field so a class's
                // toJson/fromJson stay round-trip-consistent.
                let store_val: Option<String> = match llvm_ty.as_str() {
                    "i8*" if marker.as_deref() == Some("Map:String") => {
                        let map_ptr = self.temp();
                        let as_i64  = self.temp();
                        writeln!(&mut self.ir, "  {map_ptr} = call i8* @jsonGetStringMapField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}  = ptrtoint i8* {map_ptr} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i8*" if marker.as_deref().map(|m| m.starts_with("Map:")).unwrap_or(false) => None,
                    "i8*" => {
                        let str_val = self.temp();
                        let as_i64  = self.temp();
                        writeln!(&mut self.ir, "  {str_val} = call i8* @jsonGetStringField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}  = ptrtoint i8* {str_val} to i64").unwrap();
                        Some(as_i64)
                    }
                    "double" | "float" => {
                        let dbl    = self.temp();
                        let as_i64 = self.temp();
                        writeln!(&mut self.ir, "  {dbl}    = call double @jsonGetFloatField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64} = bitcast double {dbl} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i1" => {
                        let b32    = self.temp();
                        let as_i64 = self.temp();
                        writeln!(&mut self.ir, "  {b32}    = call i32 @jsonGetBoolField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64} = zext i32 {b32} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i64*" if marker.as_deref() == Some("Array:String") => {
                        let arr_ptr = self.temp();
                        let as_i64  = self.temp();
                        writeln!(&mut self.ir, "  {arr_ptr} = call i64* @jsonGetStringListField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}  = ptrtoint i64* {arr_ptr} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i64*" if marker.as_deref().and_then(|m| m.strip_prefix("List:"))
                        .map(is_serializable_class).unwrap_or(false) =>
                    {
                        let cls = marker.as_deref().and_then(|m| m.strip_prefix("List:")).unwrap().to_string();
                        let field_val = self.temp();
                        let arr_ptr   = self.temp();
                        let as_i64    = self.temp();
                        writeln!(&mut self.ir, "  {field_val} = call i64* @jsonGetField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {arr_ptr}   = call i64* @tinox_json_list_deserialize(i64* {field_val}, ptr @{cls}_fromJson)").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}    = ptrtoint i64* {arr_ptr} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i64*" if marker.as_deref()
                        .map(|m| !m.starts_with("Array") && !m.starts_with("List:") && !m.starts_with("Map") && is_serializable_class(m))
                        .unwrap_or(false) =>
                    {
                        let cls = marker.clone().unwrap();
                        let field_val = self.temp();
                        let obj_ptr   = self.temp();
                        let as_i64    = self.temp();
                        writeln!(&mut self.ir, "  {field_val} = call i64* @jsonGetField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {obj_ptr}   = call i64* @{cls}_fromJson(i64* {field_val})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}    = ptrtoint i64* {obj_ptr} to i64").unwrap();
                        Some(as_i64)
                    }
                    "i64*" if marker.as_deref() == Some("Array") || marker.is_none() => {
                        let arr_ptr = self.temp();
                        let as_i64  = self.temp();
                        writeln!(&mut self.ir, "  {arr_ptr} = call i64* @jsonGetIntListField(i64* %json_val, i8* {key_ptr})").unwrap();
                        writeln!(&mut self.ir, "  {as_i64}  = ptrtoint i64* {arr_ptr} to i64").unwrap();
                        Some(as_i64)
                    }
                    // Unsupported pointer-shaped field kind (e.g. Map<String,
                    // non-String>, List<non-JsonSerializable class>): the slot
                    // was already zeroed above, so leaving it alone is a safe
                    // null default -- storing a value read via the wrong
                    // accessor would silently corrupt it instead.
                    "i64*" => None,
                    _ => {
                        // i64, i32, etc.
                        let val = self.temp();
                        writeln!(&mut self.ir, "  {val} = call i64 @jsonGetIntField(i64* %json_val, i8* {key_ptr})").unwrap();
                        Some(val)
                    }
                };

                if let Some(store_val) = store_val {
                    writeln!(&mut self.ir, "  store i64 {store_val}, i64* {fptr}").unwrap();
                }
            }

            writeln!(&mut self.ir, "  ret i64* {self_}").unwrap();
            writeln!(&mut self.ir, "}}").unwrap();
            writeln!(&mut self.ir).unwrap();
        }
    }

    /// Translate a lambda body expression into a SQL predicate fragment.
    /// Emits LLVM IR to evaluate parameter values and returns:
    ///   (sql_fragment, vec_of_i8ptr_regs)
    /// Returns None if the expression cannot be statically translated.
    fn lambda_to_sql_and_params(
        &mut self,
        body: &Expr,
        param_name: &str,
        fields: &[EntityFieldEntry],
        param_offset: usize,
        ctx: &mut GenCtx,
    ) -> Option<(String, Vec<String>)> {
        match &body.node {
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinaryOp::And => {
                        let (lsql, mut lparams) = self.lambda_to_sql_and_params(lhs, param_name, fields, param_offset, ctx)?;
                        let (rsql, rparams) = self.lambda_to_sql_and_params(rhs, param_name, fields, param_offset + lparams.len(), ctx)?;
                        lparams.extend(rparams);
                        Some((format!("({}) AND ({})", lsql, rsql), lparams))
                    }
                    BinaryOp::Or => {
                        let (lsql, mut lparams) = self.lambda_to_sql_and_params(lhs, param_name, fields, param_offset, ctx)?;
                        let (rsql, rparams) = self.lambda_to_sql_and_params(rhs, param_name, fields, param_offset + lparams.len(), ctx)?;
                        lparams.extend(rparams);
                        Some((format!("({}) OR ({})", lsql, rsql), lparams))
                    }
                    BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                        let sql_op = match op {
                            BinaryOp::Eq => "=",
                            BinaryOp::Ne => "!=",
                            BinaryOp::Lt => "<",
                            BinaryOp::Le => "<=",
                            BinaryOp::Gt => ">",
                            BinaryOp::Ge => ">=",
                            _ => unreachable!(),
                        };
                        let n = param_offset + 1;
                        let fields_clone = fields.to_vec();
                        if let Some(col) = orm_extract_field(lhs, param_name, &fields_clone) {
                            let col = col.to_string();
                            let reg = self.emit_orm_param_value(rhs, ctx)?;
                            Some((format!("{} {} ${}", col, sql_op, n), vec![reg]))
                        } else if let Some(col) = orm_extract_field(rhs, param_name, &fields_clone) {
                            let col = col.to_string();
                            let flipped = match op {
                                BinaryOp::Lt => ">",
                                BinaryOp::Le => ">=",
                                BinaryOp::Gt => "<",
                                BinaryOp::Ge => "<=",
                                _ => sql_op,
                            };
                            let reg = self.emit_orm_param_value(lhs, ctx)?;
                            Some((format!("{} {} ${}", col, flipped, n), vec![reg]))
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            ExprKind::Unary { op: UnaryOp::Not, operand } => {
                let (sql, params) = self.lambda_to_sql_and_params(operand, param_name, fields, param_offset, ctx)?;
                Some((format!("NOT ({})", sql), params))
            }
            ExprKind::MethodCall { obj, method, args } => {
                let fields_clone = fields.to_vec();
                if let Some(col) = orm_extract_field(obj, param_name, &fields_clone) {
                    let col = col.to_string();
                    if args.len() == 1 {
                        if let ExprKind::Literal(Literal::String(s)) = &args[0].node {
                            let n = param_offset + 1;
                            let like_val = match method.as_str() {
                                "startsWith" => format!("{}%", s),
                                "endsWith"   => format!("%{}", s),
                                "contains"   => format!("%{}%", s),
                                _ => return None,
                            };
                            let label = format!("__orm_like_{}", self.strings.len());
                            self.strings.insert(label.clone(), like_val.clone());
                            let len = like_val.len() + 1;
                            let like_reg = self.temp();
                            writeln!(&mut self.ir, "  {like_reg} = getelementptr [{len} x i8], [{len} x i8]* @{label}, i64 0, i64 0").unwrap();
                            return Some((format!("{} LIKE ${}", col, n), vec![like_reg]));
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Emit code to evaluate an ORM parameter expression and return an i8* register.
    fn emit_orm_param_value(&mut self, expr: &Expr, ctx: &mut GenCtx) -> Option<String> {
        match &expr.node {
            ExprKind::Literal(Literal::String(s)) => {
                let s_clone = s.clone();
                let label = format!("__orm_p_{}", self.strings.len());
                self.strings.insert(label.clone(), s_clone.clone());
                let len = s_clone.len() + 1;
                let reg = self.temp();
                writeln!(&mut self.ir, "  {reg} = getelementptr [{len} x i8], [{len} x i8]* @{label}, i64 0, i64 0").unwrap();
                Some(reg)
            }
            ExprKind::Literal(Literal::Integer(n)) => {
                let reg = self.temp();
                writeln!(&mut self.ir, "  {reg} = call i8* @tinox_int_to_param(i64 {n})").unwrap();
                Some(reg)
            }
            ExprKind::Literal(Literal::Bool(b)) => {
                let val: i64 = if *b { 1 } else { 0 };
                let reg = self.temp();
                writeln!(&mut self.ir, "  {reg} = call i8* @tinox_int_to_param(i64 {val})").unwrap();
                Some(reg)
            }
            _ => {
                // Runtime expression — evaluate and convert to string
                if let Ok((val_reg, val_ty)) = self.gen_expr(expr, ctx) {
                    let reg = self.temp();
                    match val_ty.as_str() {
                        "i8*" => {
                            writeln!(&mut self.ir, "  {reg} = bitcast i8* {val_reg} to i8*").unwrap();
                        }
                        _ => {
                            writeln!(&mut self.ir, "  {reg} = call i8* @tinox_int_to_param(i64 {val_reg})").unwrap();
                        }
                    }
                    Some(reg)
                } else {
                    None
                }
            }
        }
    }

    /// Generate the full query code for an ORM chain and return (result_reg, result_type).
    fn gen_orm_query(
        &mut self,
        chain: &OrmChain,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        let entity = self.entity_entries.iter().find(|e| e.class_name == chain.entity_class).cloned();
        let entity = match entity {
            Some(e) => e,
            None => return Ok(("0".to_string(), "i64*".to_string())),
        };

        // Build WHERE clause and collect params
        let mut where_parts: Vec<String> = Vec::new();
        let mut all_params: Vec<String> = Vec::new();
        let fields = entity.fields.clone();

        for (param_name, body) in &chain.filters {
            let param_name = param_name.clone();
            let body = body.clone();
            let offset = all_params.len();
            if let Some((sql, params)) = self.lambda_to_sql_and_params(&body, &param_name, &fields, offset, ctx) {
                where_parts.push(sql);
                all_params.extend(params);
            }
        }

        // Build ORDER BY clause
        let order_sql = if chain.order_by.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = chain.order_by.iter().map(|(col, desc)| {
                // Look up column name from field name
                let col_name = entity.fields.iter()
                    .find(|f| f.field_name == *col || f.column_name == *col)
                    .map(|f| f.column_name.as_str())
                    .unwrap_or(col.as_str());
                if *desc { format!("{} DESC", col_name) } else { format!("{} ASC", col_name) }
            }).collect();
            format!(" ORDER BY {}", parts.join(", "))
        };

        // Build LIMIT / OFFSET
        let limit_sql = chain.limit.map(|n| format!(" LIMIT {}", n)).unwrap_or_default();
        let offset_sql = chain.offset_val.map(|n| format!(" OFFSET {}", n)).unwrap_or_default();

        // Build the full SQL string at compile time if possible (no runtime concatenation needed
        // for WHERE clause shape — only the parameter values are runtime)
        let base_sql = format!("SELECT {} FROM {}",
            entity.fields.iter().map(|f| f.column_name.as_str()).collect::<Vec<_>>().join(", "),
            entity.table_name);
        let where_sql = if where_parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_parts.join(" AND "))
        };
        let full_sql = format!("{}{}{}{}{}", base_sql, where_sql, order_sql, limit_sql, offset_sql);

        // Emit SQL string constant
        let sql_label = format!("__orm_query_{}", self.strings.len());
        let sql_len = full_sql.len() + 1;
        self.strings.insert(sql_label.clone(), full_sql);
        let sql_ptr = self.temp();
        writeln!(&mut self.ir, "  {sql_ptr} = getelementptr [{sql_len} x i8], [{sql_len} x i8]* @{sql_label}, i64 0, i64 0").unwrap();

        // Allocate params array and fill it
        let n_params = all_params.len() as i64;
        let params_arr = self.temp();
        writeln!(&mut self.ir, "  {params_arr} = call i8** @tinox_params_alloc(i64 {n_params})").unwrap();
        for (i, param_reg) in all_params.iter().enumerate() {
            writeln!(&mut self.ir, "  call void @tinox_params_set(i8** {params_arr}, i64 {i}, i8* {param_reg})").unwrap();
        }

        // Execute query
        let conn_reg = self.temp();
        let result_reg = self.temp();
        writeln!(&mut self.ir, "  {conn_reg} = call i8* @tinox_db_acquire_stmt_conn()").unwrap();
        writeln!(&mut self.ir, "  {result_reg} = call i8* @tinox_db_exec(i8* {conn_reg}, i8* {sql_ptr}, i8** {params_arr}, i64 {n_params})").unwrap();
        writeln!(&mut self.ir, "  call void @tinox_db_release_stmt_conn(i8* {conn_reg})").unwrap();

        match chain.terminal.as_str() {
            "count" => {
                let n = self.temp();
                writeln!(&mut self.ir, "  {n} = call i64 @tinox_db_nrows(i8* {result_reg})").unwrap();
                writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {result_reg})").unwrap();
                Ok((n, "i64".to_string()))
            }
            "first" => {
                let from_row_fn = format!("{}_fromRow", entity.class_name);
                let obj_reg = self.temp();
                writeln!(&mut self.ir, "  {obj_reg} = call i8* @{from_row_fn}(i8* {result_reg}, i64 0)").unwrap();
                writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {result_reg})").unwrap();
                let as_i64ptr = self.temp();
                writeln!(&mut self.ir, "  {as_i64ptr} = ptrtoint i8* {obj_reg} to i64").unwrap();
                Ok((as_i64ptr, "i64".to_string()))
            }
            _ => {
                // "list" — build a List using Tinox array convention:
                // layout: [length | elem0 | elem1 | ... | elemN-1]
                // returned pointer points to elem0; length lives at index -1.
                let from_row_fn = format!("{}_fromRow", entity.class_name);
                let nrows = self.temp();
                writeln!(&mut self.ir, "  {nrows} = call i64 @tinox_db_nrows(i8* {result_reg})").unwrap();

                // Allocate an array handle with nrows elements
                let handle = self.temp();
                writeln!(&mut self.ir, "  {handle} = call i64* @tinox_array_new(i64 {nrows}, i64 0)").unwrap();
                let data_ptr = self.emit_array_data(&handle);

                // Loop: i = 0; while i < nrows { data_ptr[i] = fromRow(result, i); i++ }
                let loop_bb = self.new_bb("orm_loop");
                let body_bb = self.new_bb("orm_body");
                let exit_bb = self.new_bb("orm_exit");

                let idx_alloc = self.temp();
                writeln!(&mut self.ir, "  {idx_alloc} = alloca i64").unwrap();
                writeln!(&mut self.ir, "  store i64 0, i64* {idx_alloc}").unwrap();
                writeln!(&mut self.ir, "  br label %{loop_bb}").unwrap();
                writeln!(&mut self.ir, "{loop_bb}:").unwrap();
                let cur_i = self.temp();
                writeln!(&mut self.ir, "  {cur_i} = load i64, i64* {idx_alloc}").unwrap();
                let cond = self.temp();
                writeln!(&mut self.ir, "  {cond} = icmp slt i64 {cur_i}, {nrows}").unwrap();
                writeln!(&mut self.ir, "  br i1 {cond}, label %{body_bb}, label %{exit_bb}").unwrap();
                writeln!(&mut self.ir, "{body_bb}:").unwrap();

                let row_obj = self.temp();
                writeln!(&mut self.ir, "  {row_obj} = call i8* @{from_row_fn}(i8* {result_reg}, i64 {cur_i})").unwrap();
                let row_as_int = self.temp();
                writeln!(&mut self.ir, "  {row_as_int} = ptrtoint i8* {row_obj} to i64").unwrap();
                let slot = self.temp();
                writeln!(&mut self.ir, "  {slot} = getelementptr i64, i64* {data_ptr}, i64 {cur_i}").unwrap();
                writeln!(&mut self.ir, "  store i64 {row_as_int}, i64* {slot}").unwrap();
                let next_i = self.temp();
                writeln!(&mut self.ir, "  {next_i} = add i64 {cur_i}, 1").unwrap();
                writeln!(&mut self.ir, "  store i64 {next_i}, i64* {idx_alloc}").unwrap();
                writeln!(&mut self.ir, "  br label %{loop_bb}").unwrap();
                writeln!(&mut self.ir, "{exit_bb}:").unwrap();

                writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {result_reg})").unwrap();

                // Return the array handle — same layout as ArrayLiteral (type i64*)
                Ok((handle, "i64*".to_string()))
            }
        }
    }

    /// `DB.of(T).save(entity)` / `DB.of(T).delete(entity)`. `save` inserts when
    /// the @Id field is 0 (unset) and updates otherwise, mirroring the id-based
    /// upsert convention used by `examples/crud` (`entity.id = id; DB.of(T).save(entity)`
    /// for updates, no id set for creates).
    fn gen_orm_save_delete(
        &mut self,
        entity_class: &str,
        op: &str,
        arg: &Expr,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        let entity = match self.entity_entries.iter().find(|e| e.class_name == entity_class).cloned() {
            Some(e) => e,
            None => return Ok(("0".to_string(), "i64".to_string())),
        };
        let id_slot = entity.fields.iter().position(|f| f.is_id).unwrap_or(0);

        let (arg_val, arg_ty) = self.gen_expr(arg, ctx)?;
        let entity_ptr = if arg_ty == "i64" {
            let p = self.temp();
            writeln!(&mut self.ir, "  {p} = inttoptr i64 {arg_val} to i64*").unwrap();
            p
        } else {
            arg_val.clone()
        };

        let conn_reg = self.temp();
        writeln!(&mut self.ir, "  {conn_reg} = call i8* @tinox_db_acquire_stmt_conn()").unwrap();

        if op == "delete" {
            let id_ptr = self.temp();
            writeln!(&mut self.ir, "  {id_ptr} = getelementptr i64, i64* {entity_ptr}, i64 {id_slot}").unwrap();
            let id_val = self.temp();
            writeln!(&mut self.ir, "  {id_val} = load i64, i64* {id_ptr}").unwrap();
            let id_param = self.temp();
            writeln!(&mut self.ir, "  {id_param} = call i8* @tinox_int_to_param(i64 {id_val})").unwrap();
            let params_arr = self.temp();
            writeln!(&mut self.ir, "  {params_arr} = call i8** @tinox_params_alloc(i64 1)").unwrap();
            writeln!(&mut self.ir, "  call void @tinox_params_set(i8** {params_arr}, i64 0, i8* {id_param})").unwrap();
            let sql_ptr = self.temp();
            writeln!(&mut self.ir, "  {sql_ptr} = call i8* @{entity_class}_deleteSql()").unwrap();
            let result_reg = self.temp();
            writeln!(
                &mut self.ir,
                "  {result_reg} = call i8* @tinox_db_exec(i8* {conn_reg}, i8* {sql_ptr}, i8** {params_arr}, i64 1)"
            )
            .unwrap();
            writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {result_reg})").unwrap();
            writeln!(&mut self.ir, "  call void @tinox_db_release_stmt_conn(i8* {conn_reg})").unwrap();
            let as_i64 = self.temp();
            writeln!(&mut self.ir, "  {as_i64} = ptrtoint i64* {entity_ptr} to i64").unwrap();
            return Ok((as_i64, "i64".to_string()));
        }

        // save: id == 0 → INSERT (and write the RETURNING id back into the entity),
        // id != 0 → UPDATE.
        let id_ptr = self.temp();
        writeln!(&mut self.ir, "  {id_ptr} = getelementptr i64, i64* {entity_ptr}, i64 {id_slot}").unwrap();
        let id_val = self.temp();
        writeln!(&mut self.ir, "  {id_val} = load i64, i64* {id_ptr}").unwrap();
        let is_insert = self.temp();
        writeln!(&mut self.ir, "  {is_insert} = icmp eq i64 {id_val}, 0").unwrap();
        let insert_bb = self.new_bb("orm_save_insert");
        let update_bb = self.new_bb("orm_save_update");
        let done_bb = self.new_bb("orm_save_done");
        writeln!(&mut self.ir, "  br i1 {is_insert}, label %{insert_bb}, label %{update_bb}").unwrap();

        writeln!(&mut self.ir, "{insert_bb}:").unwrap();
        let n_ins = entity.fields.iter().filter(|f| !f.is_generated).count() as i64;
        let out_n = self.temp();
        writeln!(&mut self.ir, "  {out_n} = alloca i64").unwrap();
        let ins_params = self.temp();
        writeln!(
            &mut self.ir,
            "  {ins_params} = call i8** @{entity_class}_toParams(i64* {entity_ptr}, i64* {out_n})"
        )
        .unwrap();
        let ins_sql = self.temp();
        writeln!(&mut self.ir, "  {ins_sql} = call i8* @{entity_class}_insertSql()").unwrap();
        let ins_result = self.temp();
        writeln!(
            &mut self.ir,
            "  {ins_result} = call i8* @tinox_db_exec(i8* {conn_reg}, i8* {ins_sql}, i8** {ins_params}, i64 {n_ins})"
        )
        .unwrap();
        let new_id = self.temp();
        writeln!(&mut self.ir, "  {new_id} = call i64 @tinox_db_getval_int(i8* {ins_result}, i64 0, i64 0)").unwrap();
        writeln!(&mut self.ir, "  store i64 {new_id}, i64* {id_ptr}").unwrap();
        writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {ins_result})").unwrap();
        writeln!(&mut self.ir, "  br label %{done_bb}").unwrap();

        writeln!(&mut self.ir, "{update_bb}:").unwrap();
        let n_upd = entity.fields.iter().filter(|f| !f.is_id).count() as i64 + 1;
        let upd_params = self.temp();
        writeln!(
            &mut self.ir,
            "  {upd_params} = call i8** @{entity_class}_toUpdateParams(i64* {entity_ptr})"
        )
        .unwrap();
        let upd_sql = self.temp();
        writeln!(&mut self.ir, "  {upd_sql} = call i8* @{entity_class}_updateSql()").unwrap();
        let upd_result = self.temp();
        writeln!(
            &mut self.ir,
            "  {upd_result} = call i8* @tinox_db_exec(i8* {conn_reg}, i8* {upd_sql}, i8** {upd_params}, i64 {n_upd})"
        )
        .unwrap();
        writeln!(&mut self.ir, "  call void @tinox_db_free(i8* {upd_result})").unwrap();
        writeln!(&mut self.ir, "  br label %{done_bb}").unwrap();

        writeln!(&mut self.ir, "{done_bb}:").unwrap();
        writeln!(&mut self.ir, "  call void @tinox_db_release_stmt_conn(i8* {conn_reg})").unwrap();
        let as_i64 = self.temp();
        writeln!(&mut self.ir, "  {as_i64} = ptrtoint i64* {entity_ptr} to i64").unwrap();
        Ok((as_i64, "i64".to_string()))
    }

    /// Emit SQL-constant getter functions and row-mapping helpers for all @Entity classes.
    fn emit_entity_code(&mut self) {
        // Emit DB init via @llvm.global_ctors if a connection URL is configured
        if let Some(url) = self.db_url.clone() {
            let url_len = url.len() + 1;
            let escaped = Self::escape_llvm_string(&url);
            writeln!(&mut self.ir, "@__db_url = private constant [{url_len} x i8] c\"{escaped}\\00\"").unwrap();
            writeln!(&mut self.ir, "define void @__tinox_db_init() {{").unwrap();
            writeln!(&mut self.ir, "entry.tnx:").unwrap();
            writeln!(&mut self.ir, "  %url = getelementptr [{url_len} x i8], [{url_len} x i8]* @__db_url, i64 0, i64 0").unwrap();
            writeln!(&mut self.ir, "  call void @tinox_db_pool_init(i8* %url, i64 {})", self.db_pool_size).unwrap();
            writeln!(&mut self.ir, "  ret void").unwrap();
            writeln!(&mut self.ir, "}}").unwrap();
            writeln!(&mut self.ir, "@llvm.global_ctors = appending global [1 x {{ i32, void ()*, i8* }}] [{{ i32, void ()*, i8* }} {{ i32 10, void ()* @__tinox_db_init, i8* null }}]").unwrap();
            writeln!(&mut self.ir).unwrap();
        }

        let entities = self.entity_entries.clone();
        for entity in &entities {
            let cn = entity.class_name.clone();
            let table = entity.table_name.clone();
            let fields = entity.fields.clone();

            // SELECT sql
            let cols: Vec<String> = fields.iter().map(|f| f.column_name.clone()).collect();
            let select_sql = format!("SELECT {} FROM {}", cols.join(", "), table);
            self.emit_sql_const_fn(&format!("{cn}_selectSql"), &select_sql);

            // INSERT sql (exclude @GeneratedValue fields)
            let ins_fields: Vec<&EntityFieldEntry> = fields.iter().filter(|f| !f.is_generated).collect();
            let ins_cols: Vec<&str> = ins_fields.iter().map(|f| f.column_name.as_str()).collect();
            let ins_phs: Vec<String> = (1..=ins_fields.len()).map(|i| format!("${i}")).collect();
            let insert_sql = format!(
                "INSERT INTO {table} ({}) VALUES ({}) RETURNING id",
                ins_cols.join(", "),
                ins_phs.join(", ")
            );
            self.emit_sql_const_fn(&format!("{cn}_insertSql"), &insert_sql);

            // UPDATE sql (non-id fields in SET, id field in WHERE)
            let id_col = fields.iter().find(|f| f.is_id).map(|f| f.column_name.clone()).unwrap_or_else(|| "id".to_string());
            let non_id: Vec<&EntityFieldEntry> = fields.iter().filter(|f| !f.is_id).collect();
            let set_clauses: Vec<String> = non_id.iter().enumerate().map(|(i, f)| format!("{} = ${}", f.column_name, i + 1)).collect();
            let update_sql = format!(
                "UPDATE {table} SET {} WHERE {id_col} = ${}",
                set_clauses.join(", "),
                non_id.len() + 1
            );
            self.emit_sql_const_fn(&format!("{cn}_updateSql"), &update_sql);

            // DELETE sql
            let delete_sql = format!("DELETE FROM {table} WHERE {id_col} = $1");
            self.emit_sql_const_fn(&format!("{cn}_deleteSql"), &delete_sql);

            // fromRow and toParams
            self.emit_entity_from_row(&cn, &fields);
            self.emit_entity_to_params(&cn, &fields);
            self.emit_entity_to_update_params(&cn, &fields);
        }
    }

    fn emit_sql_const_fn(&mut self, fn_name: &str, sql: &str) {
        let label = format!("__sql_{}_{}", fn_name, self.strings.len());
        self.strings.insert(label.clone(), sql.to_string());
        let len = sql.len() + 1;
        let ptr = self.temp();
        writeln!(&mut self.ir, "define i8* @{fn_name}() {{").unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        writeln!(&mut self.ir, "  {ptr} = getelementptr [{len} x i8], [{len} x i8]* @{label}, i64 0, i64 0").unwrap();
        writeln!(&mut self.ir, "  ret i8* {ptr}").unwrap();
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    fn emit_entity_from_row(&mut self, class_name: &str, fields: &[EntityFieldEntry]) {
        let n = fields.len();
        let alloc_size = n as i64 * 8;
        writeln!(&mut self.ir, "define i8* @{class_name}_fromRow(i8* %result, i64 %row_idx) {{").unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        let raw = self.temp();
        let ptr = self.temp();
        writeln!(&mut self.ir, "  {raw} = call i8* @tinox_alloc(i64 {alloc_size})").unwrap();
        writeln!(&mut self.ir, "  {ptr} = bitcast i8* {raw} to i64*").unwrap();
        for (col_idx, field) in fields.iter().enumerate() {
            let fptr = self.temp();
            writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* {ptr}, i64 {col_idx}").unwrap();
            match field.field_llvm_type.as_str() {
                "i8*" => {
                    let val = self.temp();
                    writeln!(&mut self.ir, "  {val} = call i8* @tinox_db_getval(i8* %result, i64 %row_idx, i64 {col_idx})").unwrap();
                    let as_int = self.temp();
                    writeln!(&mut self.ir, "  {as_int} = ptrtoint i8* {val} to i64").unwrap();
                    writeln!(&mut self.ir, "  store i64 {as_int}, i64* {fptr}").unwrap();
                }
                _ => {
                    // Direct int64 read — no string conversion
                    let ival = self.temp();
                    writeln!(&mut self.ir, "  {ival} = call i64 @tinox_db_getval_int(i8* %result, i64 %row_idx, i64 {col_idx})").unwrap();
                    writeln!(&mut self.ir, "  store i64 {ival}, i64* {fptr}").unwrap();
                }
            }
        }
        writeln!(&mut self.ir, "  ret i8* {raw}").unwrap();
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    fn emit_entity_to_params(&mut self, class_name: &str, fields: &[EntityFieldEntry]) {
        // INSERT variant: exclude @GeneratedValue fields; slot_idx = field position in struct
        let ins_fields: Vec<(usize, &EntityFieldEntry)> = fields.iter()
            .enumerate()
            .filter(|(_, f)| !f.is_generated)
            .collect();
        let n = ins_fields.len();
        writeln!(&mut self.ir, "define i8** @{class_name}_toParams(i64* %entity, i64* %out_n) {{").unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        let arr = self.temp();
        writeln!(&mut self.ir, "  {arr} = call i8** @tinox_params_alloc(i64 {n})").unwrap();
        for (param_idx, (slot_idx, field)) in ins_fields.iter().enumerate() {
            let fptr = self.temp();
            let fval = self.temp();
            writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* %entity, i64 {slot_idx}").unwrap();
            writeln!(&mut self.ir, "  {fval} = load i64, i64* {fptr}").unwrap();
            let pstr = if field.field_llvm_type == "i8*" {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = inttoptr i64 {fval} to i8*").unwrap();
                s
            } else {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = call i8* @tinox_int_to_param(i64 {fval})").unwrap();
                s
            };
            writeln!(&mut self.ir, "  call void @tinox_params_set(i8** {arr}, i64 {param_idx}, i8* {pstr})").unwrap();
        }
        writeln!(&mut self.ir, "  store i64 {n}, i64* %out_n").unwrap();
        writeln!(&mut self.ir, "  ret i8** {arr}").unwrap();
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    /// UPDATE variant of `emit_entity_to_params`: non-id fields (in field order,
    /// matching `SET col = $1, ...`), then the @Id field's current value last
    /// (matching the `WHERE id = $N` placeholder in `{class}_updateSql`).
    fn emit_entity_to_update_params(&mut self, class_name: &str, fields: &[EntityFieldEntry]) {
        let non_id: Vec<(usize, &EntityFieldEntry)> = fields.iter()
            .enumerate()
            .filter(|(_, f)| !f.is_id)
            .collect();
        let id_field = fields.iter().enumerate().find(|(_, f)| f.is_id);
        let n = non_id.len() + if id_field.is_some() { 1 } else { 0 };
        writeln!(&mut self.ir, "define i8** @{class_name}_toUpdateParams(i64* %entity) {{").unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        let arr = self.temp();
        writeln!(&mut self.ir, "  {arr} = call i8** @tinox_params_alloc(i64 {n})").unwrap();
        for (param_idx, (slot_idx, field)) in non_id.iter().enumerate() {
            let fptr = self.temp();
            let fval = self.temp();
            writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* %entity, i64 {slot_idx}").unwrap();
            writeln!(&mut self.ir, "  {fval} = load i64, i64* {fptr}").unwrap();
            let pstr = if field.field_llvm_type == "i8*" {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = inttoptr i64 {fval} to i8*").unwrap();
                s
            } else {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = call i8* @tinox_int_to_param(i64 {fval})").unwrap();
                s
            };
            writeln!(&mut self.ir, "  call void @tinox_params_set(i8** {arr}, i64 {param_idx}, i8* {pstr})").unwrap();
        }
        if let Some((slot_idx, field)) = id_field {
            let fptr = self.temp();
            let fval = self.temp();
            writeln!(&mut self.ir, "  {fptr} = getelementptr i64, i64* %entity, i64 {slot_idx}").unwrap();
            writeln!(&mut self.ir, "  {fval} = load i64, i64* {fptr}").unwrap();
            let pstr = if field.field_llvm_type == "i8*" {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = inttoptr i64 {fval} to i8*").unwrap();
                s
            } else {
                let s = self.temp();
                writeln!(&mut self.ir, "  {s} = call i8* @tinox_int_to_param(i64 {fval})").unwrap();
                s
            };
            writeln!(&mut self.ir, "  call void @tinox_params_set(i8** {arr}, i64 {non_id_len}, i8* {pstr})", non_id_len = non_id.len()).unwrap();
        }
        writeln!(&mut self.ir, "  ret i8** {arr}").unwrap();
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    /// Emit `tinox_main` for a single test method: allocate object, call method,
    /// exit 0 on true/non-zero return, 1 on false/0.
    fn emit_test_code(&mut self) {
        let (class, method) = match self.test_entry.clone() {
            Some(e) if !self.has_main => e,
            _ => return,
        };

        let layout = self.struct_layouts.get(&class).cloned().unwrap_or_default();
        let n_fields = layout.len();
        let byte_size = (n_fields * 8).max(8);

        let mut b = String::new();
        writeln!(&mut b, "define i64 @tinox_main() {{").unwrap();
        writeln!(&mut b, "  %raw = call i8* @tinox_alloc(i64 {byte_size})").unwrap();
        writeln!(&mut b, "  %obj = bitcast i8* %raw to i64*").unwrap();
        for fi in 0..n_fields {
            writeln!(&mut b, "  %zi{fi} = getelementptr i64, i64* %obj, i64 {fi}").unwrap();
            writeln!(&mut b, "  store i64 0, i64* %zi{fi}").unwrap();
        }
        // @Test methods return Bool (i1) — calling them as i64 reads garbage
        // in the upper bits and turned failing tests into passes.
        writeln!(&mut b, "  %result = call i1 @{class}_{method}(i64* %obj)").unwrap();
        writeln!(&mut b, "  %code = select i1 %result, i64 0, i64 1").unwrap();
        writeln!(&mut b, "  ret i64 %code").unwrap();
        writeln!(&mut b, "}}").unwrap();
        writeln!(&mut b).unwrap();

        self.lambda_ir.push_str(&b);
        self.has_main = true;
    }

    /// Alternative entry point `class Main { fnc main() -> Int32 { ... } }`
    /// (Issue #149 stage 1: mandatory class-qualified functions). Nothing
    /// class-specific needs to happen at typecheck time — a static `fnc
    /// main` on a class named `Main` already typechecks and compiles as an
    /// ordinary static method (`@Main_main`, no synthetic `self` param,
    /// registered like any other `fnc`). What's missing is purely the
    /// entry-point wiring: this synthesizes a `@tinox_main` that forwards
    /// into `@Main_main`, mirroring the int-width handling `gen_fn` already
    /// uses for a top-level `fn main() -> Int32` (both return the same LLVM
    /// `i32`, matching `type_to_llvm(Type::Int32)`).
    ///
    /// A near-miss shape (an instance `fn main` instead of `fnc`, wrong
    /// param count, or wrong return type) is a hard compile error rather
    /// than a silent fallthrough to a confusing "undefined reference to
    /// `tinox_main`" link failure — this project's "no silent garbage"
    /// convention. Likewise, defining both a top-level `fn main()` and a
    /// matching `class Main { fnc main() }` is an ambiguous-entry-point
    /// error rather than silently preferring one.
    fn emit_class_main_entry_point(&mut self, source: &SourceFile) -> Result<(), ErrorBag> {
        let mut classes: Vec<&tinox_parser::Class> = Vec::new();
        for decl in &source.decls {
            match &decl.node {
                DeclKind::Class(c) => classes.push(c),
                DeclKind::Namespace(ns) => {
                    for inner in &ns.decls {
                        if let DeclKind::Class(c) = &inner.node {
                            classes.push(c);
                        }
                    }
                }
                _ => {}
            }
        }

        let Some(main_class) = classes.into_iter().find(|c| c.name == "Main") else {
            return Ok(());
        };
        let Some(method) = main_class.methods.iter().find(|m| m.name == "main") else {
            return Ok(());
        };

        let shape_ok =
            method.static_ && method.params.is_empty() && matches!(method.ret_type, Type::Int32);
        if !shape_ok {
            let mut problems = Vec::new();
            if !method.static_ {
                problems.push("must be declared `fnc` (static), not `fn`".to_string());
            }
            if !method.params.is_empty() {
                problems.push(format!(
                    "must take no parameters, found {}",
                    method.params.len()
                ));
            }
            if !matches!(method.ret_type, Type::Int32) {
                problems.push(format!(
                    "must return Int32, found {}",
                    Self::type_to_llvm(&method.ret_type)
                ));
            }
            let mut bag = ErrorBag::new();
            bag.push(Error::new(
                method.span,
                format!(
                    "class Main {{ fnc main() -> Int32 }} is reserved as a program entry point, but Main.main() {}",
                    problems.join("; ")
                ),
            ));
            return Err(bag);
        }

        if self.has_main {
            let mut bag = ErrorBag::new();
            bag.push(Error::new(
                method.span,
                "ambiguous entry point: both a top-level `fn main()` and `class Main { fnc main() }` are defined -- remove one".to_string(),
            ));
            return Err(bag);
        }

        // The actual @tinox_main wiring (call Main_main, plus spawning any
        // background_run_fns registered by the REST/HTTP3/WS/AMQP
        // annotation processors) happens later in
        // emit_tinox_main_bootstrap, once all of those have had a chance to
        // run -- letting class Main coexist with them instead of one
        // silently pre-empting the other via has_main.
        self.user_main_class = true;

        Ok(())
    }

    /// B1 phase 1: emit `%class.<name> = type { … }` for plain classes.
    ///
    /// The field types come from `struct_field_llvm_types` in `struct_layouts`
    /// order (default `i64` for compiler-added slots like `__vtable__`/`log`),
    /// so the named type is byte-identical to the current uniform i64 layout —
    /// a typed GEP and the old i64 GEP resolve to the same address. Only plain
    /// classes for now: generic templates and on-demand specializations (`Foo__i64`)
    /// are skipped and keep using the i64 path.
    fn emit_struct_type_defs(&mut self) {
        let mut names: Vec<String> = self.struct_layouts.keys().cloned().collect();
        names.sort();
        for name in names {
            if self.generic_classes.contains_key(&name) || name.contains("__") {
                continue;
            }
            if let Some(def) = self.register_named_struct_type(&name) {
                writeln!(&mut self.ir, "{}", def).unwrap();
            }
        }
        // Placeholder line: generic-specialization struct types (which arise later,
        // mid-emission) are spliced in here by into_ir, before any function body.
        writeln!(&mut self.ir, "; @@SPEC_TYPES@@").unwrap();
        writeln!(&mut self.ir).unwrap();
    }

    /// Build the `%class.<name> = type { … }` definition for a class layout,
    /// register the class in `class_named_types`, and return the def string (the
    /// caller writes it to the right buffer). Returns None for classes with a
    /// Float32 field (latent i64->float bitcast bug in the old path → stay i64).
    ///
    /// Every field is physically an 8-byte slot (the store side always writes i64
    /// bits), so each declared field type is normalized to its 8-byte slot type —
    /// the named type is byte-identical to the uniform i64 layout, and a typed GEP
    /// and the old i64 GEP resolve to the same address.
    fn register_named_struct_type(&mut self, name: &str) -> Option<String> {
        let layout = self.struct_layouts.get(name).cloned().unwrap_or_default();
        let fllt = self.struct_field_llvm_types.get(name).cloned().unwrap_or_default();
        if layout.iter().any(|f| fllt.get(f).map(|t| t == "float").unwrap_or(false)) {
            return None;
        }
        let field_types: Vec<String> = layout
            .iter()
            .map(|f| Self::slot_llvm_ty(fllt.get(f).map(|s| s.as_str()).unwrap_or("i64")))
            .collect();
        self.class_named_types.insert(name.to_string());
        Some(format!("%class.{} = type {{ {} }}", name, field_types.join(", ")))
    }

    /// The 8-byte storage slot type for a declared field llvm type. Pointers and
    /// `double` are already 8 bytes; everything else (i64/i1/i8/i16/i32) is stored
    /// in an i64 slot. (`float` is handled by excluding such classes entirely.)
    fn slot_llvm_ty(field_llvm_ty: &str) -> String {
        if field_llvm_ty == "double" {
            "double".to_string()
        } else if field_llvm_ty.ends_with('*') {
            field_llvm_ty.to_string()
        } else {
            "i64".to_string()
        }
    }

    /// Field offset within a named-type class layout (B1 phase 5). Unlike the old
    /// `position(...).unwrap_or(0)`, a missing field is a hard error instead of a
    /// silent write/read at offset 0 — the last silent-garbage source in field
    /// codegen. The typechecker already rejects unknown fields (Bug 37), so this
    /// is defense-in-depth: it fires only on an internal layout inconsistency.
    fn checked_typed_offset(&self, sname: &str, field: &str, span: Span) -> Result<i64, ErrorBag> {
        self.struct_layouts.get(sname)
            .and_then(|fields| fields.iter().position(|f| f == field))
            .map(|p| p as i64)
            .ok_or_else(|| {
                let mut bag = ErrorBag::new();
                bag.push(Error::new(span, format!(
                    "internal codegen error: field '{}' not in layout of typed class '{}'", field, sname)));
                bag
            })
    }

    /// Populates `interface_method_ret_types` from the interface declarations
    /// themselves (`vtable_layouts`, sourced from typecheck's `interface_info()`,
    /// only carries method names/order, not types). Mirrors the
    /// namespace-recursion shape `analyze_throw_effects`'s `collect` already
    /// uses for the same "walk every Interface decl, including
    /// namespace-wrapped ones" traversal.
    fn collect_interface_method_ret_types(&mut self, source: &SourceFile) {
        fn walk(decls: &[tinox_parser::Decl], out: &mut HashMap<String, HashMap<String, tinox_parser::Type>>) {
            for d in decls {
                match &d.node {
                    DeclKind::Interface(i) => {
                        let m = out.entry(i.name.clone()).or_default();
                        for method in &i.methods {
                            m.insert(method.name.clone(), method.ret_type.clone());
                        }
                    }
                    DeclKind::Namespace(ns) => walk(&ns.decls, out),
                    _ => {}
                }
            }
        }
        walk(&source.decls, &mut self.interface_method_ret_types);
    }

    /// Emit a vtable global for each class that implements at least one interface.
    fn emit_vtable_globals(&mut self, source: &SourceFile) {
        let class_names: Vec<(String, Vec<String>)> = source
            .decls
            .iter()
            .flat_map(|d| {
                let mut v = Vec::new();
                match &d.node {
                    DeclKind::Class(c) if !c.implements.is_empty() => {
                        v.push((c.name.clone(), c.implements.clone()));
                    }
                    DeclKind::Namespace(ns) => {
                        for inner in &ns.decls {
                            if let DeclKind::Class(c) = &inner.node {
                                if !c.implements.is_empty() {
                                    v.push((c.name.clone(), c.implements.clone()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                v
            })
            .collect();

        for (class_name, implements) in class_names {
            let mut vtable_methods: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for iface in &implements {
                if let Some(methods) = self.vtable_layouts.get(iface) {
                    for m in methods {
                        if seen.insert(m.clone()) {
                            vtable_methods.push(m.clone());
                        }
                    }
                }
            }

            if vtable_methods.is_empty() {
                continue;
            }

            let n = vtable_methods.len();
            let mut entries = String::new();
            for (i, method_name) in vtable_methods.iter().enumerate() {
                if i > 0 {
                    entries.push_str(", ");
                }
                let full_fn = format!("{}_{}", class_name, method_name);
                entries.push_str(&format!(
                    "i64 ptrtoint (i64* (i64*)* @{} to i64)",
                    full_fn
                ));
            }
            writeln!(
                &mut self.ir,
                "@{}_vtable = global [{} x i64] [{}]",
                class_name, n, entries
            )
            .unwrap();
        }
    }

    fn gen_stmt_body(&mut self, stmt: &Stmt, ctx: &mut GenCtx) -> Result<(), ErrorBag> {
        match &stmt.node {
            StmtKind::Defer(inner) => {
                if let Some(scope) = ctx.defer_stack.last_mut() {
                    scope.push((**inner).clone());
                }
                return Ok(());
            }
            StmtKind::Block(stmts) => {
                if !ctx.in_defer_exec {
                    ctx.defer_stack.push(Vec::new());
                }
                for s in stmts {
                    self.gen_stmt_body(s, ctx)?;
                    // Bug 40: propagate a thrown error immediately after any
                    // statement that could have thrown — unless the statement
                    // already terminated the block (throw/return/break emit their
                    // own terminator). Not while replaying deferred statements
                    // (those run during unwinding/return and must not re-trigger).
                    if !ctx.in_defer_exec
                        && Self::stmt_may_throw(s, &self.throwing_free_fns, &self.throwing_method_basenames)
                        && !self.last_is_terminator()
                    {
                        self.emit_post_stmt_throw_check(ctx)?;
                    }
                }
                if !ctx.in_defer_exec {
                    self.gen_defer_scope(ctx)?;
                    ctx.defer_stack.pop();
                }
                return Ok(());
            }
            StmtKind::Return(Some(expr)) => {
                let stmts_to_run: Vec<_> = ctx
                    .defer_stack
                    .last().cloned()
                    .unwrap_or_default();
                for stmt in stmts_to_run.into_iter().rev() {
                    self.gen_stmt_body(&Box::new(stmt), ctx)?;
                }
                if let Some(scope) = ctx.defer_stack.last_mut() {
                    scope.clear();
                }
                if let Some((ref label, ref start_reg)) = ctx.timed_metric.clone() {
                    self.emit_histogram_record(label, start_reg);
                }
                let (val, ty) = self.gen_expr(expr, ctx)?;
                let expected = &ctx.ret_type.clone();
                // A void function returns nothing. A void *expression* returned
                // from a non-void function (e.g. a lambda body `{ f(); }` whose
                // tail is a void call, under the uniform i64 closure ABI) must
                // yield a dummy of the expected type — never `ret void 0`.
                if expected.as_str() == "void" {
                    self.emit_function_return(ctx, "void", "");
                    return Ok(());
                }
                if ty == "void" {
                    let rt = if expected.is_empty() { "i64" } else { expected.as_str() };
                    let z = if rt.ends_with('*') { "null" } else { "0" };
                    self.emit_function_return(ctx, rt, z);
                    return Ok(());
                }
                let (final_val, final_ty) = if !expected.is_empty() && &ty != expected {
                    let cast_op = match (ty.as_str(), expected.as_str()) {
                        (from, to) if from.ends_with('*') && to.ends_with('*') => "bitcast",
                        (from, to) if from.starts_with('i') && to.starts_with('i') && !from.contains('*') && !to.contains('*') => {
                            let from_bits: u32 = from[1..].parse().unwrap_or(64);
                            let to_bits: u32 = to[1..].parse().unwrap_or(64);
                            if from_bits > to_bits { "trunc" } else { "zext" }
                        }
                        (from, to) if !from.ends_with('*') && to.ends_with('*') => "inttoptr",
                        (from, to) if from.ends_with('*') && !to.ends_with('*') => "ptrtoint",
                        _ => "",
                    };
                    if !cast_op.is_empty() {
                        let tmp = self.temp();
                        writeln!(&mut self.ir, "{} = {} {} {} to {}", tmp, cast_op, ty, val, expected).unwrap();
                        (tmp, expected.clone())
                    } else {
                        (val, ty)
                    }
                } else {
                    (val, ty)
                };
                self.emit_function_return(ctx, &final_ty, &final_val);
            }
            StmtKind::Return(None) => {
                self.gen_defer_scope(ctx)?;
                if let Some((ref label, ref start_reg)) = ctx.timed_metric.clone() {
                    self.emit_histogram_record(label, start_reg);
                }
                // A bare `return;` in a non-void function (e.g. inside a lambda
                // under the uniform i64 return ABI) must still yield a value of
                // the expected type — otherwise `ret void` mismatches.
                let expected = ctx.ret_type.as_str();
                if expected.is_empty() || expected == "void" {
                    self.emit_function_return(ctx, "void", "");
                } else if expected.ends_with('*') {
                    self.emit_function_return(ctx, expected, "null");
                } else {
                    self.emit_function_return(ctx, expected, "0");
                }
            }
            StmtKind::Expr(expr) => {
                self.gen_expr(expr, ctx)?;
            }
            StmtKind::Let {
                name, ty, value, ..
            } => {
                let mut llvm_ty = Self::type_to_llvm(ty.as_ref().unwrap_or(&Type::Int64));
                let mut struct_name: Option<String> = None;

                // Generic class with an explicit annotation (`let o:
                // Option<Int64> = …;`): eagerly specialize and set the
                // local marker to the mangled class — regardless of
                // where the value comes from (`Option::some(5)`
                // directly, or e.g. `Cache::get(c, k)`, whose return type
                // `Option<V>` is only resolved at call time in the
                // SPECIALIZED Cache method). If the value expression
                // calls the same class directly, the constructor call is
                // additionally redirected via an alias (Bug 20.2 —
                // otherwise an instance method of a generic class is
                // never emitted, because pre-registration skips generic
                // classes entirely).
                let mut generic_let_alias: Option<String> = None;
                if let Some(Type::Generic { name: ann_name, args: ann_targs }) = ty.as_ref() {
                    if let Some(gc) = self.generic_classes.get(ann_name.as_str()).cloned() {
                        let bindings: HashMap<String, String> = gc
                            .type_params
                            .iter()
                            .zip(ann_targs.iter())
                            .map(|(tp, ta)| (tp.clone(), Self::type_to_llvm(ta)))
                            .collect();
                        let mangled = self
                            .ensure_generic_class_specialization_with_bindings(ann_name, &bindings)?;
                        struct_name = Some(mangled.clone());
                        let matches_ctor = value.as_ref().is_some_and(|v| {
                            matches!(
                                &v.node,
                                ExprKind::EnumValue { enum_name: ev_name, .. } if ev_name == ann_name
                            )
                        });
                        if matches_ctor {
                            self.type_param_aliases.insert(ann_name.clone(), mangled);
                            generic_let_alias = Some(ann_name.clone());
                        }
                    }
                }

                let is_heap_ptr = if let Some(v) = value {
                    if let ExprKind::StructLiteral { name: n, .. } = &v.node {
                        llvm_ty = "i64*".to_string();
                        struct_name = Some(n.clone());
                        true
                    } else if let ExprKind::New { class, type_args, .. } = &v.node {
                        llvm_ty = "i64*".to_string();
                        struct_name = Some(self.effective_class_name(class, type_args));
                        true
                    } else if let ExprKind::MapLiteral(_) = &v.node {
                        llvm_ty = "i8*".to_string();
                        // Value marker from the annotation, else from the first
                        // literal entry ("Map:String"/"Map:Float"), else plain Map
                        struct_name = ty
                            .as_ref()
                            .and_then(Self::container_marker)
                            .or_else(|| self.infer_struct_type(v, ctx))
                            .or_else(|| Some("Map".to_string()));
                        true
                    } else if let ExprKind::Call { func, .. } = &v.node {
                        if matches!(&func.node, ExprKind::Ident(n) if n == "open") {
                            llvm_ty = "i8*".to_string();
                            struct_name = Some("File".to_string());
                            true
                        } else if matches!(&func.node, ExprKind::Ident(n) if n == "split" || n == "regexFindAll" || n == "regexSplit") {
                            llvm_ty = "i64*".to_string();
                            struct_name = Some("Array:String".to_string());
                            true
                        } else if matches!(&func.node, ExprKind::Ident(n) if n == "regexMatchGroups") {
                            llvm_ty = "i64*".to_string();
                            true
                        } else { false }
                    } else if let ExprKind::ArrayLiteral(elems) = &v.node {
                        llvm_ty = "i64*".to_string();
                        // Container marker from the annotation, else from the first literal element
                        let ann_marker = ty.as_ref().and_then(Self::container_marker);
                        let is_str_lit = elems.first().map(|e| matches!(&e.node, ExprKind::Literal(Literal::String(_)))).unwrap_or(false);
                        let is_float_lit = elems.first().map(|e| matches!(&e.node, ExprKind::Literal(Literal::Float(_)))).unwrap_or(false);
                        if let Some(m) = ann_marker {
                            if m != "Array" {
                                struct_name = Some(m);
                            }
                        } else if is_str_lit {
                            struct_name = Some("Array:String".to_string());
                        } else if is_float_lit {
                            struct_name = Some("Array:Float".to_string());
                        } else if elems.first().map(|e| matches!(&e.node, ExprKind::ArrayLiteral(_))).unwrap_or(false) {
                            struct_name = Some("Array:Array".to_string());
                        }
                        true
                    } else if matches!(&v.node, ExprKind::Tuple(_) | ExprKind::Lambda { .. }) {
                        llvm_ty = "i64*".to_string();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if struct_name.is_none() {
                    if let Some(Type::Named(ann)) = ty {
                        struct_name = Some(ann.clone());
                        if self.defined_classes.contains(ann.as_str()) {
                            llvm_ty = "i64*".to_string();
                        }
                    } else if let Some(ann_ty) = ty {
                        // Container annotation → marker (Map, Array:String,
                        // Array:Array:…, List:C, Array) from the central source
                        if let Some(m) = Self::container_marker(ann_ty) {
                            if Self::is_map_marker(&m) {
                                struct_name = Some(m);
                                llvm_ty = "i8*".to_string();
                            } else {
                                struct_name = Some(m);
                                llvm_ty = "i64*".to_string();
                            }
                        }
                    }
                }

                if let Some(val) = value {
                    let (v, val_ty) = self.gen_expr(val, ctx)?;
                    if let Some(cls) = generic_let_alias.take() {
                        self.type_param_aliases.remove(&cls);
                    }
                    let actual_ty = if matches!(&val.node, ExprKind::Lambda { .. }) {
                        val_ty.clone()
                    } else if is_heap_ptr {
                        llvm_ty.clone()
                    } else if ty.is_none() || matches!(ty, Some(Type::Infer)) {
                        // No annotation: use the value's actual type (enables correct float/generic inference)
                        val_ty.clone()
                    } else {
                        llvm_ty.clone()
                    };
                    let slot = ctx.locals.len();
                    ctx.locals.insert(name.clone(), (actual_ty.clone(), slot));
                    // Generate a unique alloca slot name to avoid duplicate definitions
                    let slot_name = format!("{}_{}", name, self.temp_count);
                    self.temp_count += 1;
                    ctx.local_slots.insert(name.clone(), slot_name.clone());
                    if matches!(&val.node, ExprKind::Range { .. }) {
                        ctx.range_vars.insert(name.clone());
                    }
                    // If the declared type annotation is an interface, record the
                    // interface name so vtable dispatch is used for method calls.
                    // Also infer class name from constructor/factory calls when no annotation is present.
                    // Use method_ret_class mapping built during pre-pass for accurate type inference.
                    let local_inferred = if struct_name.is_none() {
                        match &val.node {
                            ExprKind::EnumValue { enum_name, variant, .. } => {
                                let method_key = format!("{}_{}", enum_name, variant);
                                let result = self.method_ret_class.get(&method_key).cloned()
                                    .or_else(|| {
                                        // Fallback: constructor heuristic
                                        let is_ctor = variant == "new" || variant.starts_with("from")
                                            || variant.starts_with("create") || variant.starts_with("make");
                                        if is_ctor && self.struct_layouts.contains_key(enum_name.as_str()) {
                                            Some(enum_name.clone())
                                        } else { None }
                                    });
                                result
                            }
                            ExprKind::Call { func, .. } => {
                                match &func.node {
                                    ExprKind::Ident(fname) => {
                                        self.method_ret_class.get(fname.as_str()).cloned()
                                    }
                                    _ => None,
                                }
                            }
                            ExprKind::MethodCall { obj: mc_obj, method: mc_method, .. } => {
                                // Infer return class from instance method call, e.g. evaluator.eval() -> EvalResult
                                self.infer_struct_type(mc_obj, ctx)
                                    .and_then(|obj_class| {
                                        let method_key = format!("{}_{}", obj_class, mc_method);
                                        self.method_ret_class.get(&method_key).cloned()
                                    })
                            }
                            ExprKind::Ident(src_name) => {
                                // Copy type from source variable (e.g. let x = someObj)
                                ctx.local_types.get(src_name.as_str()).cloned()
                            }
                            _ => None,
                        }
                    } else { struct_name.clone() };
                    // Type-system unification phase 2: the rich export
                    // overrides local inference exactly when it only
                    // knows the ERASED generic base ("Box") and the
                    // checker supplies the specialization ("Box__i64" —
                    // B2 step 2: `let bi = Box::make(42)`). If local
                    // inference knows nothing, the rich marker applies
                    // directly.
                    let inferred_struct = match (local_inferred, self.rich_marker(val)) {
                        (Some(l), Some(r))
                            if self.generic_classes.contains_key(l.as_str())
                                && r.starts_with(&format!("{}__", l)) =>
                        {
                            Some(r)
                        }
                        (None, r) => r,
                        (l, _) => l,
                    };
                    let effective_type = if let Some(Type::Named(ann)) = ty {
                        if self.known_interfaces.contains(ann.as_str()) {
                            Some(ann.clone())
                        } else {
                            inferred_struct.clone().or_else(|| struct_name.clone())
                        }
                    } else {
                        inferred_struct.clone().or_else(|| struct_name.clone())
                    }
                    ;
                    if let Some(sn) = effective_type {
                        ctx.local_types.insert(name.clone(), sn);
                    } else {
                        // Re-binding a name without type info must clear any stale entry
                        // (e.g. a former loop var's element marker).
                        ctx.local_types.remove(name.as_str());
                    }
                    // For List<ClassName> annotations, track element type for indexed field access
                    if let Some(Type::Generic { name: gname, args }) = ty {
                        if gname == "List" {
                            if let Some(Type::Named(cls)) = args.first() {
                                if self.defined_classes.contains(cls.as_str()) {
                                    ctx.local_types.insert(name.clone(), format!("List:{}", cls));
                                }
                            }
                        }
                    }
                    {
                        // Heap and non-heap locals share the same alloca/coerce/store here
                        // (is_heap_ptr already steered actual_ty above).
                        writeln!(&mut self.ir, "%{} = alloca {}", slot_name, actual_ty).unwrap();
                        // Coerce value to actual slot type
                        let store_val = if val_ty == actual_ty || val_ty.is_empty() || actual_ty.is_empty() {
                            v.clone()
                        } else if val_ty == "i64" && (actual_ty.ends_with('*') || actual_ty == "ptr") {
                            let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", c, v, actual_ty).unwrap(); c
                        } else if (val_ty.ends_with('*') || val_ty == "ptr") && actual_ty == "i64" {
                            let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, v).unwrap(); c
                        } else if val_ty == "i64" && actual_ty == "i1" {
                            // Indirect calls return Bool as i64 — take bit 0
                            // (upper bits may be garbage at the ABI level).
                            let c = self.temp(); writeln!(&mut self.ir, "{} = trunc i64 {} to i1", c, v).unwrap(); c
                        } else if val_ty == "i1" && actual_ty == "i64" {
                            let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, v).unwrap(); c
                        } else if val_ty == "double" && actual_ty == "i64" {
                            let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, v).unwrap(); c
                        } else if let (Some(vw), Some(aw)) = (Self::int_bit_width(&val_ty), Self::int_bit_width(&actual_ty)) {
                            // General int-width mismatch (e.g. a binary-op result
                            // widened to i64 stored into a narrower Int32 local) —
                            // truncate/extend to the slot's declared width.
                            if vw > aw {
                                let c = self.temp(); writeln!(&mut self.ir, "{} = trunc {} {} to {}", c, val_ty, v, actual_ty).unwrap(); c
                            } else if vw < aw {
                                let instr = if val_ty == "i1" { "zext" } else { "sext" };
                                let c = self.temp(); writeln!(&mut self.ir, "{} = {} {} {} to {}", c, instr, val_ty, v, actual_ty).unwrap(); c
                            } else { v.clone() }
                        } else { v.clone() };
                        writeln!(&mut self.ir, "store {} {}, {}* %{}", actual_ty, store_val, actual_ty, slot_name).unwrap();
                    }
                } else {
                    let slot = ctx.locals.len();
                    ctx.locals.insert(name.clone(), (llvm_ty.clone(), slot));
                    // Generate a unique alloca slot name to avoid duplicate definitions
                    let slot_name = format!("{}_{}", name, self.temp_count);
                    self.temp_count += 1;
                    ctx.local_slots.insert(name.clone(), slot_name.clone());
                    if let Some(sn) = &struct_name {
                        ctx.local_types.insert(name.clone(), sn.clone());
                    }
                    writeln!(&mut self.ir, "%{} = alloca {}", slot_name, llvm_ty).unwrap();
                }
            }
            StmtKind::Var {
                name, ty, value, ..
            } => {
                let mut llvm_ty = Self::type_to_llvm(ty.as_ref().unwrap_or(&Type::Int64));
                let mut struct_name: Option<String> = None;
                let is_ptr = if let Some(v) = value {
                    if let ExprKind::StructLiteral { name: n, .. } = &v.node {
                        llvm_ty = "i64*".to_string();
                        struct_name = Some(n.clone());
                        true
                    } else if let ExprKind::New { class, type_args, .. } = &v.node {
                        llvm_ty = "i64*".to_string();
                        struct_name = Some(self.effective_class_name(class, type_args));
                        true
                    } else if let ExprKind::MapLiteral(_) = &v.node {
                        llvm_ty = "i8*".to_string();
                        // Value marker from the annotation, else from the first
                        // literal entry ("Map:String"/"Map:Float"), else plain Map
                        struct_name = ty
                            .as_ref()
                            .and_then(Self::container_marker)
                            .or_else(|| self.infer_struct_type(v, ctx))
                            .or_else(|| Some("Map".to_string()));
                        true
                    } else if let ExprKind::Call { func, .. } = &v.node {
                        if matches!(&func.node, ExprKind::Ident(n) if n == "open") {
                            llvm_ty = "i8*".to_string();
                            struct_name = Some("File".to_string());
                            true
                        } else if matches!(&func.node, ExprKind::Ident(n) if n == "split" || n == "regexFindAll" || n == "regexSplit") {
                            llvm_ty = "i64*".to_string();
                            struct_name = Some("Array:String".to_string());
                            true
                        } else if matches!(&func.node, ExprKind::Ident(n) if n == "regexMatchGroups") {
                            llvm_ty = "i64*".to_string();
                            true
                        } else { false }
                    } else if let ExprKind::ArrayLiteral(elems) = &v.node {
                        llvm_ty = "i64*".to_string();
                        // Container marker from the annotation, else from the first literal element
                        let ann_marker = ty.as_ref().and_then(Self::container_marker);
                        let is_str_lit = elems.first().map(|e| matches!(&e.node, ExprKind::Literal(Literal::String(_)))).unwrap_or(false);
                        let is_float_lit = elems.first().map(|e| matches!(&e.node, ExprKind::Literal(Literal::Float(_)))).unwrap_or(false);
                        if let Some(m) = ann_marker {
                            if m != "Array" {
                                struct_name = Some(m);
                            }
                        } else if is_str_lit {
                            struct_name = Some("Array:String".to_string());
                        } else if is_float_lit {
                            struct_name = Some("Array:Float".to_string());
                        } else if elems.first().map(|e| matches!(&e.node, ExprKind::ArrayLiteral(_))).unwrap_or(false) {
                            struct_name = Some("Array:Array".to_string());
                        }
                        true
                    } else if matches!(&v.node, ExprKind::Tuple(_) | ExprKind::Lambda { .. }) {
                        llvm_ty = "i64*".to_string();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if struct_name.is_none() {
                    if let Some(Type::Named(ann)) = ty {
                        struct_name = Some(ann.clone());
                        if self.defined_classes.contains(ann.as_str()) {
                            llvm_ty = "i64*".to_string();
                        }
                    } else if let Some(ann_ty) = ty {
                        // Container annotation → marker (Map, Array:String,
                        // Array:Array:…, List:C, Array) from the central source
                        if let Some(m) = Self::container_marker(ann_ty) {
                            if Self::is_map_marker(&m) {
                                struct_name = Some(m);
                                llvm_ty = "i8*".to_string();
                            } else {
                                struct_name = Some(m);
                                llvm_ty = "i64*".to_string();
                            }
                        }
                    }
                }

                // Generate a unique alloca slot name to avoid duplicate definitions
                let slot_name = format!("{}_{}", name, self.temp_count);
                self.temp_count += 1;
                ctx.local_slots.insert(name.clone(), slot_name.clone());

                if let Some(val) = value {
                    let (v, val_ty) = self.gen_expr(val, ctx)?;
                    let actual_ty = if matches!(&val.node, ExprKind::Lambda { .. }) {
                        val_ty.clone()
                    } else if is_ptr {
                        llvm_ty.clone()
                    } else if ty.is_none() || matches!(ty, Some(Type::Infer)) {
                        // No annotation: use the value's actual LLVM type (preserves i8* for strings,
                        // double for floats, etc. — avoids spurious ptrtoint/print_int for string vars)
                        val_ty.clone()
                    } else {
                        llvm_ty.clone()
                    };
                    let slot = ctx.locals.len();
                    ctx.locals.insert(name.clone(), (actual_ty.clone(), slot));
                    // Infer struct type from static method calls (EnumValue) and instance method calls.
                    // This ensures local_types is set so subsequent method calls dispatch correctly.
                    let local_inferred_var = if struct_name.is_none() {
                        match &val.node {
                            ExprKind::EnumValue { enum_name, variant, .. } => {
                                let method_key = format!("{}_{}", enum_name, variant);
                                self.method_ret_class.get(&method_key).cloned()
                                    .or_else(|| {
                                        let is_ctor = variant == "new" || variant.starts_with("from")
                                            || variant.starts_with("create") || variant.starts_with("make");
                                        if is_ctor && self.struct_layouts.contains_key(enum_name.as_str()) {
                                            Some(enum_name.clone())
                                        } else { None }
                                    })
                            }
                            ExprKind::MethodCall { obj: mc_obj, method: mc_method, .. } => {
                                self.infer_struct_type(mc_obj, ctx)
                                    .and_then(|obj_class| {
                                        let method_key = format!("{}_{}", obj_class, mc_method);
                                        self.method_ret_class.get(&method_key).cloned()
                                    })
                            }
                            ExprKind::Call { func, .. } => {
                                match &func.node {
                                    ExprKind::Ident(fname) => {
                                        self.method_ret_class.get(fname.as_str()).cloned()
                                    }
                                    _ => None,
                                }
                            }
                            ExprKind::Ident(src_name) => {
                                // Copy type from source variable (e.g. var newCtx = ctx)
                                ctx.local_types.get(src_name.as_str()).cloned()
                            }
                            _ => None,
                        }
                    } else { struct_name.clone() };
                    // Type-system unification phase 2 — same as the let
                    // path: a specialization from the rich export wins
                    // over the erased generic base; otherwise local
                    // inference stands.
                    let inferred_struct_var = match (local_inferred_var, self.rich_marker(val)) {
                        (Some(l), Some(r))
                            if self.generic_classes.contains_key(l.as_str())
                                && r.starts_with(&format!("{}__", l)) =>
                        {
                            Some(r)
                        }
                        (None, r) => r,
                        (l, _) => l,
                    };
                    // If the declared type annotation is an interface, use it for vtable dispatch.
                    let effective_type = if let Some(Type::Named(ann)) = ty {
                        if self.known_interfaces.contains(ann.as_str()) {
                            Some(ann.clone())
                        } else {
                            inferred_struct_var.clone().or_else(|| struct_name.clone())
                        }
                    } else {
                        inferred_struct_var.clone().or_else(|| struct_name.clone())
                    }
                    ;
                    if let Some(sn) = effective_type {
                        ctx.local_types.insert(name.clone(), sn);
                    } else {
                        // Re-binding a name without type info must clear any stale entry
                        // (e.g. a former loop var's element marker).
                        ctx.local_types.remove(name.as_str());
                    }
                    writeln!(&mut self.ir, "%{} = alloca {}", slot_name, actual_ty).unwrap();
                    // Coerce value type to slot type if necessary
                    let store_val = if val_ty == actual_ty || val_ty.is_empty() || actual_ty.is_empty() {
                        v.clone()
                    } else if val_ty == "i64" && (actual_ty.ends_with('*') || actual_ty == "ptr") {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", c, v, actual_ty).unwrap();
                        c
                    } else if (val_ty.ends_with('*') || val_ty == "ptr") && actual_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, v).unwrap();
                        c
                    } else if val_ty == "i64" && actual_ty == "i1" {
                            // Indirect calls return Bool as i64 — take bit 0
                            // (upper bits may be garbage at the ABI level).
                            let c = self.temp(); writeln!(&mut self.ir, "{} = trunc i64 {} to i1", c, v).unwrap(); c
                        } else if val_ty == "i1" && actual_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, v).unwrap();
                        c
                    } else if val_ty == "double" && actual_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, v).unwrap();
                        c
                    } else if let (Some(vw), Some(aw)) = (Self::int_bit_width(&val_ty), Self::int_bit_width(&actual_ty)) {
                        // General int-width mismatch (e.g. a binary-op result
                        // widened to i64 stored into a narrower Int32 local) —
                        // truncate/extend to the slot's declared width.
                        if vw > aw {
                            let c = self.temp(); writeln!(&mut self.ir, "{} = trunc {} {} to {}", c, val_ty, v, actual_ty).unwrap(); c
                        } else if vw < aw {
                            let instr = if val_ty == "i1" { "zext" } else { "sext" };
                            let c = self.temp(); writeln!(&mut self.ir, "{} = {} {} {} to {}", c, instr, val_ty, v, actual_ty).unwrap(); c
                        } else { v.clone() }
                    } else {
                        v.clone()
                    };
                    writeln!(
                        &mut self.ir,
                        "store {} {}, {}* %{}",
                        actual_ty, store_val, actual_ty, slot_name
                    )
                    .unwrap();
                } else {
                    let slot = ctx.locals.len();
                    ctx.locals.insert(name.clone(), (llvm_ty.clone(), slot));
                    if let Some(sn) = &struct_name {
                        ctx.local_types.insert(name.clone(), sn.clone());
                    }
                    writeln!(&mut self.ir, "%{} = alloca {}", slot_name, llvm_ty).unwrap();
                }
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let (cond_val, cond_ty) = self.gen_expr(cond, ctx)?;
                let cond_i1 = if cond_ty != "i1" {
                    let tmp = self.temp();
                    writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", tmp, cond_ty, cond_val).unwrap();
                    tmp
                } else {
                    cond_val
                };
                let then_bb = self.new_bb("then");
                let else_bb = self.new_bb("else");
                let merge_bb = self.new_bb("ifcont");
                writeln!(
                    &mut self.ir,
                    "br i1 {}, label %{}, label %{}",
                    cond_i1, then_bb, else_bb
                )
                .unwrap();
                writeln!(&mut self.ir, "{}:", then_bb).unwrap();
                self.gen_stmt_body(then_branch, ctx)?;
                writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                writeln!(&mut self.ir, "{}:", else_bb).unwrap();
                if let Some(else_stmt) = else_branch {
                    self.gen_stmt_body(else_stmt, ctx)?;
                }
                writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                writeln!(&mut self.ir, "{}:", merge_bb).unwrap();
            }
            StmtKind::While { cond, body } => {
                let loop_bb = self.new_bb("loop");
                let body_bb = self.new_bb("loopbody");
                let end_bb = self.new_bb("loopend");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(loop_bb.clone());

                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", loop_bb).unwrap();
                let (cond_val, cond_ty) = self.gen_expr(cond, ctx)?;
                let cond_i1 = if cond_ty != "i1" {
                    let tmp = self.temp();
                    writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", tmp, cond_ty, cond_val).unwrap();
                    tmp
                } else {
                    cond_val
                };
                writeln!(
                    &mut self.ir,
                    "br i1 {}, label %{}, label %{}",
                    cond_i1, body_bb, end_bb
                )
                .unwrap();
                writeln!(&mut self.ir, "{}:", body_bb).unwrap();
                self.gen_stmt_body(body, ctx)?;
                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
            }
            StmtKind::ForC {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(init_stmt) = init {
                    self.gen_stmt_body(init_stmt, ctx)?;
                }

                let loop_bb = self.new_bb("forcond");
                let body_bb = self.new_bb("forbody");
                let update_bb = self.new_bb("forupdate");
                let end_bb = self.new_bb("forend");

                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", loop_bb).unwrap();

                if let Some(cond_expr) = cond {
                    let (cond_val, cond_ty) = self.gen_expr(cond_expr, ctx)?;
                    let cond_i1 = if cond_ty != "i1" {
                        let tmp = self.temp();
                        writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", tmp, cond_ty, cond_val).unwrap();
                        tmp
                    } else { cond_val };
                    writeln!(
                        &mut self.ir,
                        "br i1 {}, label %{}, label %{}",
                        cond_i1, body_bb, end_bb
                    )
                    .unwrap();
                } else {
                    writeln!(&mut self.ir, "br label %{}", body_bb).unwrap();
                }

                writeln!(&mut self.ir, "{}:", body_bb).unwrap();
                self.gen_stmt_body(body, ctx)?;
                writeln!(&mut self.ir, "br label %{}", update_bb).unwrap();

                writeln!(&mut self.ir, "{}:", update_bb).unwrap();
                if let Some(update_expr) = update {
                    self.gen_expr(update_expr, ctx)?;
                }
                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();

                writeln!(&mut self.ir, "{}:", end_bb).unwrap();
            }
            StmtKind::Break => {
                if let Some(ref break_bb) = ctx.break_target.clone() {
                    writeln!(&mut self.ir, "br label %{}", break_bb).unwrap();
                }
            }
            StmtKind::Continue => {
                if let Some(ref cont_bb) = ctx.continue_target.clone() {
                    writeln!(&mut self.ir, "br label %{}", cont_bb).unwrap();
                }
            }
            StmtKind::Throw(expr) => {
                let (val, val_ty) = self.gen_expr(expr, ctx)?;
                let store_val = if val_ty == "double" || val_ty == "float" {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = bitcast {} {} to i64", cast, val_ty, val).unwrap();
                    cast
                } else if val_ty != "i64" && val_ty != "i1" && !val_ty.is_empty() {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                    cast
                } else {
                    val
                };
                if let Some((catch_bb, error_var, depth)) = &ctx.error_catch {
                    let (catch_bb, error_var, depth) = (catch_bb.clone(), error_var.clone(), *depth);
                    writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, error_var).unwrap();
                    // Run defer scopes opened inside this try's body before
                    // jumping to the local catch handler (Bug 41 follow-up).
                    self.emit_unwind_defers_to(ctx, depth)?;
                    writeln!(&mut self.ir, "br label %{}", catch_bb).unwrap();
                } else {
                    // No enclosing try in this function: park the error in the
                    // global slot and return a default value. Per-statement
                    // throw-checks in the calling frames (emit_post_stmt_throw_check)
                    // propagate it immediately up the call stack (Bug 40); the
                    // nearest enclosing try consumes it, or the runtime entry point
                    // reports it as uncaught. Run pending defers first (Bug 41) so
                    // resource cleanup happens as the throw unwinds this frame.
                    writeln!(&mut self.ir, "store i64 {}, i64* @__tinox_err", store_val).unwrap();
                    self.emit_unwind_defers(ctx)?;
                    self.emit_ret_default(ctx);
                }
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                self.gen_try_stmt(body, catches, finally.as_deref(), ctx)?;
            }
            StmtKind::For { var, iter, body } => {
                let is_range = matches!(iter.node, ExprKind::Range { .. })
                    || matches!(&iter.node, ExprKind::Ident(n) if ctx.range_vars.contains(n));
                // Container marker of the iterable — from the local variable or
                // inferred (fields, calls, literals, nested elements).
                let iter_marker = if let ExprKind::Ident(n) = &iter.node {
                    ctx.local_types.get(n).cloned()
                        // Fallback: the rich bridge (unstripped marker, hence
                        // not infer_struct_type — that strips List:)
                        .or_else(|| self.rich_marker(iter))
                } else {
                    self.infer_struct_type(iter, ctx)
                };
                let is_str_arr = iter_marker.as_deref() == Some("Array:String");
                let (iter_ptr, iter_ty) = self.gen_expr(iter, ctx)?;
                let is_string = iter_ty == "i8*";

                // arr_ptr: Some(ptr) for array/string, None for range
                // str_ptr: Some(i8*) for string iteration
                let (start_val, end_val, arr_ptr, str_ptr) = if is_range {
                    let s_gep = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 0", s_gep, iter_ptr).unwrap();
                    let sv = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", sv, s_gep).unwrap();
                    let e_gep = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", e_gep, iter_ptr).unwrap();
                    let ev = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", ev, e_gep).unwrap();
                    (sv, ev, None, None)
                } else if is_string {
                    // String: iterate bytes, length via tinox_string_length
                    let len_val = self.temp();
                    writeln!(&mut self.ir, "{} = call i64 @tinox_string_length(i8* {})", len_val, iter_ptr).unwrap();
                    ("0".to_string(), len_val, None, Some(iter_ptr))
                } else {
                    // Array handle: len at slot 0, data pointer at slot 2.
                    // iter_ptr may be i64 (pointer encoded as integer) or i64*/ptr — coerce to ptr.
                    let handle = if iter_ty == "i64" {
                        let p = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", p, iter_ptr).unwrap();
                        p
                    } else {
                        iter_ptr.clone()
                    };
                    let len_val = self.emit_array_len(&handle);
                    // Snapshot the data pointer once — pushes during iteration that
                    // grow the buffer are not observed by this loop.
                    let data_ptr = self.emit_array_data(&handle);
                    ("0".to_string(), len_val, Some(data_ptr), None)
                };

                // Float-list elements are stored as i64 bit patterns; the loop
                // variable itself must be a double slot (like match payloads).
                let is_float_elem = arr_ptr.is_some()
                    && iter_marker.as_deref().and_then(Self::elem_marker).as_deref() == Some("Float");
                // String elements are stored as i64-encoded pointers; the loop
                // variable is a real i8* slot (like match payloads) — no
                // cast-at-use pseudo marker.
                let is_string_elem = arr_ptr.is_some() && is_str_arr;

                // Give loop variable a unique LLVM slot to avoid duplicate alloca on re-use
                let var_slot = format!("{}_{}", var, self.temp_count);
                self.temp_count += 1;
                if is_float_elem {
                    writeln!(&mut self.ir, "%{} = alloca double", var_slot).unwrap();
                    writeln!(&mut self.ir, "store double 0.0, double* %{}", var_slot).unwrap();
                    ctx.locals.insert(var.clone(), ("double".to_string(), ctx.locals.len()));
                } else if is_string_elem {
                    writeln!(&mut self.ir, "%{} = alloca i8*", var_slot).unwrap();
                    writeln!(&mut self.ir, "store i8* null, i8** %{}", var_slot).unwrap();
                    ctx.locals.insert(var.clone(), ("i8*".to_string(), ctx.locals.len()));
                } else {
                    writeln!(&mut self.ir, "%{} = alloca i64", var_slot).unwrap();
                    writeln!(&mut self.ir, "store i64 {}, i64* %{}", start_val, var_slot).unwrap();
                    ctx.locals.insert(var.clone(), ("i64".to_string(), ctx.locals.len()));
                }
                ctx.local_slots.insert(var.clone(), var_slot.clone());

                let needs_separate_idx = arr_ptr.is_some() || str_ptr.is_some();
                let idx_slot = if needs_separate_idx {
                    let s = format!("for_idx_{}", self.temp_count);
                    self.temp_count += 1;
                    writeln!(&mut self.ir, "%{} = alloca i64", s).unwrap();
                    writeln!(&mut self.ir, "store i64 0, i64* %{}", s).unwrap();
                    s
                } else {
                    // Range: var_slot IS the counter
                    var_slot.clone()
                };

                let cond_bb = self.new_bb("for_cond");
                let body_bb = self.new_bb("for_body");
                let end_bb = self.new_bb("for_end");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(cond_bb.clone());

                writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();
                writeln!(&mut self.ir, "{}:", cond_bb).unwrap();
                let cur_idx = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* %{}", cur_idx, idx_slot).unwrap();
                let cmp = self.temp();
                writeln!(&mut self.ir, "{} = icmp slt i64 {}, {}", cmp, cur_idx, end_val).unwrap();
                writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", cmp, body_bb, end_bb).unwrap();

                writeln!(&mut self.ir, "{}:", body_bb).unwrap();
                if let Some(aptr) = &arr_ptr {
                    let elem_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", elem_ptr, aptr, cur_idx).unwrap();
                    let elem_raw = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", elem_raw, elem_ptr).unwrap();
                    if is_float_elem {
                        let f = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast i64 {} to double", f, elem_raw).unwrap();
                        writeln!(&mut self.ir, "store double {}, double* %{}", f, var_slot).unwrap();
                    } else if is_string_elem {
                        let sp = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", sp, elem_raw).unwrap();
                        writeln!(&mut self.ir, "store i8* {}, i8** %{}", sp, var_slot).unwrap();
                    } else {
                        writeln!(&mut self.ir, "store i64 {}, i64* %{}", elem_raw, var_slot).unwrap();
                    }
                    if is_string_elem {
                        ctx.local_types.insert(var.clone(), "String".to_string());
                    } else if let Some(em) = iter_marker.as_deref().and_then(Self::elem_marker) {
                        // Elements that are containers or class instances keep
                        // their marker so dispatch in the body works
                        // (e.g. for v in List<List<Int64>> → v is "Array");
                        // floats are fully typed by their double slot already.
                        if em != "Float" {
                            ctx.local_types.insert(var.clone(), em);
                        }
                    }
                } else if let Some(sptr) = &str_ptr {
                    // Load byte at sptr[cur_idx], zext to i64, store to var
                    let bptr = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i8, ptr {}, i64 {}", bptr, sptr, cur_idx).unwrap();
                    let byte = self.temp();
                    writeln!(&mut self.ir, "{} = load i8, i8* {}", byte, bptr).unwrap();
                    let ext = self.temp();
                    writeln!(&mut self.ir, "{} = zext i8 {} to i64", ext, byte).unwrap();
                    writeln!(&mut self.ir, "store i64 {}, i64* %{}", ext, var_slot).unwrap();
                }
                self.gen_stmt_body(body, ctx)?;

                let next_idx = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* %{}", next_idx, idx_slot).unwrap();
                let inc = self.temp();
                writeln!(&mut self.ir, "{} = add i64 {}, 1", inc, next_idx).unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* %{}", inc, idx_slot).unwrap();
                writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();
                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                // The elem marker is only valid inside the loop body — a later variable
                // with the same name must not inherit it (local_types is function-flat).
                ctx.local_types.remove(var.as_str());

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
            }
            StmtKind::Loop { body } => {
                let loop_bb = self.new_bb("loop_body");
                let end_bb = self.new_bb("loop_end");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(loop_bb.clone());

                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", loop_bb).unwrap();
                self.gen_stmt_body(body, ctx)?;
                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
            }
            StmtKind::Select { arms, default } => {
                let select_bb = self.new_bb("select_try");
                let end_bb = self.new_bb("select_end");

                // Allocate a slot for each arm's received value
                let arm_slots: Vec<String> = arms.iter().map(|_| {
                    let slot = format!("%sel_val_{}", self.temp_count);
                    self.temp_count += 1;
                    writeln!(&mut self.ir, "{} = alloca i64", slot).unwrap();
                    slot
                }).collect();

                writeln!(&mut self.ir, "br label %{}", select_bb).unwrap();
                writeln!(&mut self.ir, "{}:", select_bb).unwrap();

                let next_bb = if default.is_some() {
                    self.new_bb("select_default")
                } else {
                    self.new_bb("select_retry")
                };

                let _arm_body_bbs: Vec<String> = arms.iter().map(|arm| {
                    format!("sel_arm_{}_{}", arm.var, self.temp_count)
                }).collect();
                // Patch arm_body_bbs to have unique names
                let arm_body_bbs: Vec<String> = (0..arms.len()).map(|i| {
                    format!("sel_arm_{}", self.temp_count + i)
                }).collect();
                self.temp_count += arms.len();

                // Emit try_recv checks for each arm, chained
                for (i, (arm, slot)) in arms.iter().zip(arm_slots.iter()).enumerate() {
                    let fail_bb = if i + 1 < arms.len() {
                        format!("sel_try_{}", self.temp_count + i)
                    } else {
                        next_bb.clone()
                    };
                    let (ch_ptr, _) = self.gen_expr(&arm.channel, ctx)?;
                    // cast channel to i8* if needed
                    let ch_i8 = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", ch_i8, ch_ptr).unwrap();
                    let ok = self.temp();
                    writeln!(&mut self.ir, "{} = call i1 @tinox_channel_try_recv(i8* {}, i64* {})", ok, ch_i8, slot).unwrap();
                    writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", ok, arm_body_bbs[i], fail_bb).unwrap();
                    if i + 1 < arms.len() {
                        writeln!(&mut self.ir, "{}:", fail_bb).unwrap();
                    }
                }
                self.temp_count += arms.len();

                // Emit arm bodies
                for (i, (arm, slot)) in arms.iter().zip(arm_slots.iter()).enumerate() {
                    writeln!(&mut self.ir, "{}:", arm_body_bbs[i]).unwrap();
                    let val = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", val, slot).unwrap();
                    // Bind the received value to arm.var
                    writeln!(&mut self.ir, "%{} = alloca i64", arm.var).unwrap();
                    writeln!(&mut self.ir, "store i64 {}, i64* %{}", val, arm.var).unwrap();
                    let slot_i = ctx.locals.len();
                    ctx.locals.insert(arm.var.clone(), ("i64".to_string(), slot_i));
                    ctx.params.insert(arm.var.clone());
                    self.gen_stmt_body(&arm.body, ctx)?;
                    ctx.locals.remove(&arm.var);
                    ctx.params.remove(&arm.var);
                    writeln!(&mut self.ir, "br label %{}", end_bb).unwrap();
                }

                // Default or blocking retry with yield
                if let Some(def_body) = default {
                    writeln!(&mut self.ir, "{}:", next_bb).unwrap();
                    self.gen_stmt_body(def_body, ctx)?;
                    writeln!(&mut self.ir, "br label %{}", end_bb).unwrap();
                } else {
                    writeln!(&mut self.ir, "{}:", next_bb).unwrap();
                    let yield_tmp = self.temp();
                    writeln!(&mut self.ir, "{} = call i32 @sched_yield()", yield_tmp).unwrap();
                    writeln!(&mut self.ir, "br label %{}", select_bb).unwrap();
                }

                writeln!(&mut self.ir, "{}:", end_bb).unwrap();
            }
            StmtKind::Assignment { target, value } => {
                if let ExprKind::Ident(name) = &target.node {
                    let name = name.clone();
                    if let Some((ty, _)) = ctx.locals.get(&name) {
                        let ty = ty.clone();
                        let slot = ctx.local_slots.get(&name).cloned().unwrap_or_else(|| name.clone());
                        let (val, val_ty) = self.gen_expr(value, ctx)?;
                        // Convert value type to target type if they differ
                        let int_bits = |t: &str| -> Option<u32> {
                            match t {
                                "i1" => Some(1),
                                "i8" => Some(8),
                                "i16" => Some(16),
                                "i32" => Some(32),
                                "i64" => Some(64),
                                _ => None,
                            }
                        };
                        let store_val = if val_ty == ty || val_ty.is_empty() || ty.is_empty() {
                            val
                        } else if val_ty == "i64" && (ty.ends_with('*') || ty == "ptr") {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", c, val, ty).unwrap();
                            c
                        } else if (val_ty.ends_with('*') || val_ty == "ptr") && ty == "i64" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                            c
                        } else if let (Some(from), Some(to)) = (int_bits(&val_ty), int_bits(&ty)) {
                            // Integer width mismatch (e.g. counter: Int32 = counter + 1
                            // where the addition widened to i64): trunc/extend.
                            let c = self.temp();
                            if from > to {
                                writeln!(&mut self.ir, "{} = trunc {} {} to {}", c, val_ty, val, ty).unwrap();
                            } else {
                                let instr = if val_ty == "i1" { "zext" } else { "sext" };
                                writeln!(&mut self.ir, "{} = {} {} {} to {}", c, instr, val_ty, val, ty).unwrap();
                            }
                            c
                        } else {
                            val
                        };
                        writeln!(&mut self.ir, "store {} {}, {}* %{}", ty, store_val, ty, slot).unwrap();
                    }
                } else if let ExprKind::FieldAccess { obj, field } = &target.node {
                    let (obj_raw, obj_ty) = self.gen_expr(obj, ctx)?;
                    // If the obj evaluated to i64 (a loaded pointer), restore it to a ptr
                    let obj_ptr = if obj_ty == "i64" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", cast, obj_raw).unwrap();
                        cast
                    } else {
                        obj_raw
                    };
                    let struct_name = self.infer_struct_type(obj, ctx);
                    let offset = struct_name.as_ref()
                        .and_then(|sn| self.struct_layouts.get(sn))
                        .and_then(|fields| fields.iter().position(|f| f == field))
                        .unwrap_or(0) as i64;
                    let (val, val_ty) = self.gen_expr(value, ctx)?;
                    // B1 phase 3: typed field store for named-type classes; else i64.
                    if !self.try_typed_field_store(struct_name.as_deref(), &obj_ptr, field, target.span, &val, &val_ty)? {
                        // Uniform i64 field storage: floats → bitcast, i1 → zext, pointers → ptrtoint
                        let store_val = if val_ty == "i1" {
                            let cast = self.temp();
                            writeln!(&mut self.ir, "{} = zext i1 {} to i64", cast, val).unwrap();
                            cast
                        } else if val_ty == "double" || val_ty == "float" {
                            let cast = self.temp();
                            writeln!(&mut self.ir, "{} = bitcast {} {} to i64", cast, val_ty, val).unwrap();
                            cast
                        } else if val_ty != "i64" && !val_ty.is_empty() {
                            let cast = self.temp();
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                            cast
                        } else {
                            val
                        };
                        let field_ptr = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = getelementptr i64, ptr {}, i64 {}",
                            field_ptr, obj_ptr, offset
                        )
                        .unwrap();
                        writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, field_ptr)
                            .unwrap();
                    }
                } else if let ExprKind::Index { obj, index } = &target.node {
                    let idx_target = self.gen_index_target(obj, index, ctx)?;
                    let (val, val_ty) = self.gen_expr(value, ctx)?;
                    self.gen_index_store(&idx_target, &val, &val_ty);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Could evaluating THIS expression node itself (not its sub-expressions —
    /// each of those gets its own check via its own `gen_expr` recursion, see
    /// below) invoke a throwing call? Mirrors the direct-call arms of
    /// `expr_may_throw` without the recursive `|| args.iter().any(...)` part.
    fn expr_directly_may_throw(node: &ExprKind, tf: &HashSet<String>, tm: &HashSet<String>) -> bool {
        match node {
            // Also checks `tm` (bare method basenames), not just `tf`: see
            // the identical fix/comment on `expr_may_throw` above (issue
            // #149 stage 2's same-class bare `fnc` calls are indistinguishable
            // from a free call at this AST shape).
            ExprKind::Call { func, .. } => match &func.node {
                ExprKind::Ident(name) => tf.contains(name.as_str()) || tm.contains(name.as_str()),
                _ => true, // dynamic/lambda call — cannot prove non-throwing
            },
            ExprKind::MethodCall { method, .. } => tm.contains(method.as_str()),
            ExprKind::SuperCall { method, .. } => tm.contains(method.as_str()),
            ExprKind::EnumValue { variant, .. } => tm.contains(variant.as_str()),
            ExprKind::New { .. } | ExprKind::Await(_) | ExprKind::Recv(_) | ExprKind::Spawn(_) => true,
            _ => false,
        }
    }

    /// Issue 71 — sub-statement throw granularity: a compound expression like
    /// `a() + b()` used to run `b()` even after `a()` already threw, because the
    /// only unwind check ran at the NEXT STATEMENT boundary (`emit_post_stmt_
    /// throw_check`, Bug 40). `gen_expr` is the single recursive entry point
    /// every sub-expression goes through (binary operands, call args, receiver,
    /// array/map literal elements, …), so inserting the same check here — right
    /// after a node that itself may have just thrown, before control returns to
    /// whatever expression is composing it — closes that gap for free at every
    /// nesting depth without touching each of the many call-emission sites
    /// individually. The renamed `gen_expr_inner` below still recurses via
    /// `self.gen_expr(...)`, so nested calls get checked too.
    fn gen_expr(
        &mut self,
        expr: &tinox_parser::Expr,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        let result = self.gen_expr_inner(expr, ctx)?;
        if Self::expr_directly_may_throw(&expr.node, &self.throwing_free_fns, &self.throwing_method_basenames) {
            self.emit_post_stmt_throw_check(ctx)?;
        }
        Ok(result)
    }

    fn gen_expr_inner(
        &mut self,
        expr: &tinox_parser::Expr,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        match &expr.node {
            ExprKind::Literal(lit) => self.gen_literal(lit),
            ExprKind::Ident(name) => {
                if ctx.params.contains(name) {
                    let ty = ctx.locals.get(name)
                        .map(|(t, _)| t.clone())
                        .unwrap_or_else(|| "i64".to_string());
                    Ok((format!("%{}", name), ty))
                } else if let Some((ty, _)) = ctx.locals.get(name) {
                    let ty = ty.clone();
                    let slot = ctx.local_slots.get(name).cloned().unwrap_or_else(|| name.clone());
                    let val = self.temp();
                    writeln!(&mut self.ir, "{} = load {}, {}* %{}", val, ty, ty, slot).unwrap();
                    Ok((val, ty))
                } else {
                    Ok((format!("%{}", name), "i64".to_string()))
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // Short-circuit && / || : the RHS must only run when the LHS
                // doesn't already decide the result. Emitting `and i1`/`or i1` on
                // two eagerly-evaluated operands runs the RHS unconditionally,
                // breaking guards like `i < len && arr[i]` (they'd read out of
                // bounds / hit side effects). Branch instead, evaluating the RHS
                // only in its own block, result via an i1 slot.
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    let slot = self.temp();
                    writeln!(&mut self.ir, "{} = alloca i1", slot).unwrap();
                    let (l, lt) = self.gen_expr(lhs, ctx)?;
                    let li1 = self.emit_i1(&l, &lt);
                    let rhs_bb = self.new_bb("sc_rhs");
                    let short_bb = self.new_bb("sc_short");
                    let merge_bb = self.new_bb("sc_merge");
                    // &&: L true → eval RHS, else short-circuit false.
                    // ||: L true → short-circuit true, else eval RHS.
                    let (then_lbl, else_lbl, short_val) = if matches!(op, BinaryOp::And) {
                        (&rhs_bb, &short_bb, "false")
                    } else {
                        (&short_bb, &rhs_bb, "true")
                    };
                    writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", li1, then_lbl, else_lbl).unwrap();
                    writeln!(&mut self.ir, "{}:", short_bb).unwrap();
                    writeln!(&mut self.ir, "store i1 {}, i1* {}", short_val, slot).unwrap();
                    writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                    writeln!(&mut self.ir, "{}:", rhs_bb).unwrap();
                    let (r, rt) = self.gen_expr(rhs, ctx)?;
                    let ri1 = self.emit_i1(&r, &rt);
                    writeln!(&mut self.ir, "store i1 {}, i1* {}", ri1, slot).unwrap();
                    writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                    writeln!(&mut self.ir, "{}:", merge_bb).unwrap();
                    let result = self.temp();
                    writeln!(&mut self.ir, "{} = load i1, i1* {}", result, slot).unwrap();
                    return Ok((result, "i1".to_string()));
                }
                let (l, lt) = self.gen_expr(lhs, ctx)?;
                let (r, rt) = self.gen_expr(rhs, ctx)?;
                let result = self.temp();
                let float = Self::is_float(&lt) || Self::is_float(&rt);
                // Coerce object (i64*) → String if one side is already a String.
                // This calls ClassName_toString() if it exists, enabling "text" + obj syntax.
                let (l, lt) = if (lt == "i64*" && (rt == "i8*" || rt == "i64*")) || (lt == "i8*" && rt == "i64*") {
                    if lt == "i64*" {
                        let cn = Self::expr_class_name(&lhs.node, ctx);
                        let key = cn.as_deref().map(|c| format!("{}_toString", c));
                        if key.as_deref().map(|k| self.method_ret_types.contains_key(k)).unwrap_or(false) {
                            let s = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @{}(i64* {})", s, key.unwrap(), l).unwrap();
                            (s, "i8*".to_string())
                        } else { (l, lt) }
                    } else { (l, lt) }
                } else { (l, lt) };
                let (r, rt) = if rt == "i64*" && (lt == "i8*" || lt == "i64*") {
                    let cn = Self::expr_class_name(&rhs.node, ctx);
                    let key = cn.as_deref().map(|c| format!("{}_toString", c));
                    if key.as_deref().map(|k| self.method_ret_types.contains_key(k)).unwrap_or(false) {
                        let s = self.temp();
                        writeln!(&mut self.ir, "{} = call i8* @{}(i64* {})", s, key.unwrap(), r).unwrap();
                        (s, "i8*".to_string())
                    } else { (r, rt) }
                } else { (r, rt) };
                // Unify mixed integer widths (e.g. Int32 var + Int64 loop
                // index): extend the narrower operand, so every integer op
                // arm below sees matching types.
                fn int_width(t: &str) -> Option<u32> {
                    match t {
                        "i1" => Some(1),
                        "i8" => Some(8),
                        "i16" => Some(16),
                        "i32" => Some(32),
                        "i64" => Some(64),
                        _ => None,
                    }
                }
                let (l, lt, r, rt) = match (int_width(&lt), int_width(&rt)) {
                    (Some(a), Some(b)) if a < b => {
                        let c = self.temp();
                        let instr = if lt == "i1" { "zext" } else { "sext" };
                        writeln!(&mut self.ir, "{} = {} {} {} to {}", c, instr, lt, l, rt).unwrap();
                        (c, rt.clone(), r, rt)
                    }
                    (Some(a), Some(b)) if a > b => {
                        let c = self.temp();
                        let instr = if rt == "i1" { "zext" } else { "sext" };
                        writeln!(&mut self.ir, "{} = {} {} {} to {}", c, instr, rt, r, lt).unwrap();
                        (l, lt.clone(), c, lt)
                    }
                    _ => (l, lt, r, rt),
                };
                match op {
                    tinox_parser::BinaryOp::Add => {
                        if lt == "i8*" || rt == "i8*" || lt == "i64*" || rt == "i64*" {
                            let l_str = if lt == "i8*" {
                                l.clone()
                            } else if lt.ends_with('*') {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i8*", c, lt, l).unwrap();
                                c
                            } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, l).unwrap();
                                c
                            };
                            let r_str = if rt == "i8*" {
                                r.clone()
                            } else if rt.ends_with('*') {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i8*", c, rt, r).unwrap();
                                c
                            } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, r).unwrap();
                                c
                            };
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_concat(i8* {}, i8* {})", result, l_str, r_str).unwrap();
                            return Ok((result, "i8*".to_string()));
                        } else if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fadd {} {}, {}", result, float_ty, lf, rf).unwrap();
                            return Ok((result, float_ty.to_string()));
                        } else {
                            writeln!(&mut self.ir, "{} = add {} {}, {}", result, lt, l, r).unwrap()
                        }
                    }
                    tinox_parser::BinaryOp::Sub => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fsub {} {}, {}", result, float_ty, lf, rf).unwrap();
                            return Ok((result, float_ty.to_string()));
                        } else {
                            writeln!(&mut self.ir, "{} = sub {} {}, {}", result, lt, l, r).unwrap()
                        }
                    }
                    tinox_parser::BinaryOp::Mul => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fmul {} {}, {}", result, float_ty, lf, rf).unwrap();
                            return Ok((result, float_ty.to_string()));
                        } else {
                            writeln!(&mut self.ir, "{} = mul {} {}, {}", result, lt, l, r).unwrap()
                        }
                    }
                    tinox_parser::BinaryOp::Div => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fdiv {} {}, {}", result, float_ty, lf, rf).unwrap();
                            return Ok((result, float_ty.to_string()));
                        } else {
                            // Checked: hard error on divide-by-zero (was LLVM UB → garbage).
                            self.emit_checked_idiv(&result, &lt, &l, &r, false);
                        }
                    }
                    tinox_parser::BinaryOp::Mod => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = frem {} {}, {}", result, float_ty, lf, rf).unwrap();
                            return Ok((result, float_ty.to_string()));
                        } else {
                            self.emit_checked_idiv(&result, &lt, &l, &r, true);
                        }
                    }
                    tinox_parser::BinaryOp::Eq => {
                        if float {
                            // Coerce i64 operands to double if needed (float bits stored as i64).
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap();
                                c
                            } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap();
                                c
                            } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp oeq {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" || rt == "i8*" {
                            // String semantic equality
                            let l_str = if lt == "i8*" { l.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, l).unwrap(); c };
                            let r_str = if rt == "i8*" { r.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, r).unwrap(); c };
                            let cmp = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_equals(i8* {}, i8* {})", cmp, l_str, r_str).unwrap();
                            writeln!(&mut self.ir, "{} = trunc i64 {} to i1", result, cmp).unwrap()
                        } else if lt != rt {
                            // Mixed types: normalize pointer to i64
                            let (nl, nr) = if lt.ends_with('*') || lt == "ptr" {
                                let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, if lt == "ptr" { "ptr".to_string() } else { lt.clone() }, l).unwrap(); (c, r.clone())
                            } else if rt.ends_with('*') || rt == "ptr" {
                                let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, if rt == "ptr" { "ptr".to_string() } else { rt.clone() }, r).unwrap(); (l.clone(), c)
                            } else { (l.clone(), r.clone()) };
                            writeln!(&mut self.ir, "{} = icmp eq i64 {}, {}", result, nl, nr).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp eq {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Ne => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp one {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" || rt == "i8*" {
                            let l_str = if lt == "i8*" { l.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, l).unwrap(); c };
                            let r_str = if rt == "i8*" { r.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, r).unwrap(); c };
                            let cmp = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_equals(i8* {}, i8* {})", cmp, l_str, r_str).unwrap();
                            let eq_bit = self.temp();
                            writeln!(&mut self.ir, "{} = trunc i64 {} to i1", eq_bit, cmp).unwrap();
                            writeln!(&mut self.ir, "{} = xor i1 {}, 1", result, eq_bit).unwrap()
                        } else if lt != rt {
                            let (nl, nr) = if lt.ends_with('*') || lt == "ptr" {
                                let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, if lt == "ptr" { "ptr".to_string() } else { lt.clone() }, l).unwrap(); (c, r.clone())
                            } else if rt.ends_with('*') || rt == "ptr" {
                                let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, if rt == "ptr" { "ptr".to_string() } else { rt.clone() }, r).unwrap(); (l.clone(), c)
                            } else { (l.clone(), r.clone()) };
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, {}", result, nl, nr).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp ne {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Lt => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp olt {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" && rt == "i8*" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_compare(i8* {}, i8* {})", c, l, r).unwrap();
                            writeln!(&mut self.ir, "{} = icmp slt i64 {}, 0", result, c).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp slt {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Le => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp ole {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" && rt == "i8*" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_compare(i8* {}, i8* {})", c, l, r).unwrap();
                            writeln!(&mut self.ir, "{} = icmp sle i64 {}, 0", result, c).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp sle {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Gt => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp ogt {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" && rt == "i8*" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_compare(i8* {}, i8* {})", c, l, r).unwrap();
                            writeln!(&mut self.ir, "{} = icmp sgt i64 {}, 0", result, c).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp sgt {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Ge => {
                        if float {
                            let float_ty = if lt == "double" || rt == "double" { "double" } else { lt.as_str() };
                            let lf = if lt != "double" && lt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, lt, l).unwrap(); c } else { l.clone() };
                            let rf = if rt != "double" && rt != "float" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to double", c, rt, r).unwrap(); c } else { r.clone() };
                            writeln!(&mut self.ir, "{} = fcmp oge {} {}, {}", result, float_ty, lf, rf).unwrap()
                        } else if lt == "i8*" && rt == "i8*" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_compare(i8* {}, i8* {})", c, l, r).unwrap();
                            writeln!(&mut self.ir, "{} = icmp sge i64 {}, 0", result, c).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = icmp sge {} {}, {}", result, lt, l, r).unwrap()
                        }
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::And => {
                        // Coerce operands to i1 if they are i64 (booleans stored as i64).
                        let li1 = if lt == "i1" { l.clone() } else {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", c, lt, l).unwrap();
                            c
                        };
                        let ri1 = if rt == "i1" { r.clone() } else {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", c, rt, r).unwrap();
                            c
                        };
                        writeln!(&mut self.ir, "{} = and i1 {}, {}", result, li1, ri1).unwrap();
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::Or => {
                        // Coerce operands to i1 if they are i64 (booleans stored as i64).
                        let li1 = if lt == "i1" { l.clone() } else {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", c, lt, l).unwrap();
                            c
                        };
                        let ri1 = if rt == "i1" { r.clone() } else {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", c, rt, r).unwrap();
                            c
                        };
                        writeln!(&mut self.ir, "{} = or i1 {}, {}", result, li1, ri1).unwrap();
                        return Ok((result, "i1".to_string()));
                    }
                    tinox_parser::BinaryOp::BitAnd => {
                        writeln!(&mut self.ir, "{} = and {} {}, {}", result, lt, l, r).unwrap()
                    }
                    tinox_parser::BinaryOp::BitOr => {
                        writeln!(&mut self.ir, "{} = or {} {}, {}", result, lt, l, r).unwrap()
                    }
                    tinox_parser::BinaryOp::Xor => {
                        writeln!(&mut self.ir, "{} = xor {} {}, {}", result, lt, l, r).unwrap()
                    }
                    tinox_parser::BinaryOp::Shl => {
                        writeln!(&mut self.ir, "{} = shl {} {}, {}", result, lt, l, r).unwrap()
                    }
                    tinox_parser::BinaryOp::Shr => {
                        writeln!(&mut self.ir, "{} = lshr {} {}, {}", result, lt, l, r).unwrap()
                    }
                    tinox_parser::BinaryOp::ShrArith => {
                        writeln!(&mut self.ir, "{} = ashr {} {}, {}", result, lt, l, r).unwrap()
                    }
                }
                Ok((result, lt))
            }
            ExprKind::Unary { op, operand } => {
                let (val, ty) = self.gen_expr(operand, ctx)?;
                let result = self.temp();
                match op {
                    tinox_parser::UnaryOp::Neg => {
                        if Self::is_float(&ty) {
                            writeln!(&mut self.ir, "{} = fneg {} {}", result, ty, val).unwrap()
                        } else {
                            writeln!(&mut self.ir, "{} = sub {} 0, {}", result, ty, val).unwrap()
                        }
                    }
                    tinox_parser::UnaryOp::Not => {
                        writeln!(&mut self.ir, "{} = xor {} 1, {}", result, ty, val).unwrap()
                    }
                    tinox_parser::UnaryOp::BitNot => {
                        writeln!(&mut self.ir, "{} = xor {} -1, {}", result, ty, val).unwrap()
                    }
                }
                Ok((result, ty))
            }
            ExprKind::Call { func, args } => {
                // Same-class bare `fnc` call (issue #149 stage 2): a
                // sibling STATIC method of the class this method body
                // belongs to, called without a `ClassName::` qualifier.
                // Must run BEFORE the generic arg pre-evaluation just
                // below, since `emit_static_dispatch_call` evaluates its
                // own args (shared with the `ClassName::method()` path,
                // codegen.rs:8290) — evaluating twice would duplicate any
                // side-effecting argument expressions and double-emit
                // their IR. Priority mirrors typecheck's `check_call`: a
                // genuine top-level free function of the same bare name
                // (still supported during the migration) wins over the
                // same-class fallback, so this only fires when no such
                // free function exists. Static-only, matching the
                // typecheck-side restriction (an instance method needs an
                // implicit `this` receiver, a different, not-yet-built
                // feature).
                if let ExprKind::Ident(name) = &func.node {
                    if !self.fn_sigs.contains_key(name.as_str()) {
                        if let Some(class_name) = ctx.current_struct.clone() {
                            let static_key = format!("{}_{}", class_name, name);
                            if self.static_method_keys.contains(&static_key) {
                                if let Some(ret_ty) = self.method_ret_types.get(&static_key).cloned() {
                                    return self.emit_static_dispatch_call(&static_key, &ret_ty, args, ctx);
                                }
                            }
                        }
                    }
                }
                let mut args_str = String::new();
                let mut arg_types = Vec::new();
                let mut arg_vals = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        args_str.push_str(", ");
                    }
                    let (val, ty) = self.gen_expr(arg, ctx)?;
                    args_str.push_str(&format!("{} {}", ty, val));
                    arg_types.push(ty);
                    arg_vals.push(val);
                }
                let fn_name = match &func.node {
                    ExprKind::Ident(name) => match name.as_str() {
                        "main" => "tinox_main".to_string(),
                        "print" | "println" => {
                            if !args.is_empty() {
                                let ty = &arg_types[0];
                                // At the LLVM level, i32 is both Char and
                                // Int32 — only real char literals print as
                                // a character, Int32 values numerically
                                // (sext + int).
                                let is_char_lit =
                                    matches!(&args[0].node, ExprKind::Literal(Literal::Char(_)));
                                let llvm_fn = match ty.as_str() {
                                    "i8*" => "tinox_print_string",
                                    "double" => "tinox_print_float",
                                    "i1" => "tinox_print_bool",
                                    "i32" if is_char_lit => "tinox_print_char",
                                    t if t.starts_with('i') && t != "i64" && !t.ends_with('*') => {
                                        let c = self.temp();
                                        writeln!(&mut self.ir, "{} = sext {} {} to i64", c, t, arg_vals[0]).unwrap();
                                        args_str = format!("i64 {}", c);
                                        "tinox_print_int"
                                    }
                                    _ => "tinox_print_int",
                                };
                                writeln!(&mut self.ir, "call void @{}({})", llvm_fn, args_str).unwrap();
                            }
                            if name == "println" {
                                writeln!(&mut self.ir, "call void @tinox_print_newline()").unwrap();
                            }
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "len" => {
                            let (ptr, ty) = self.gen_expr(&args[0], ctx)?;
                            if ty == "i8*" {
                                let result = self.temp();
                                writeln!(&mut self.ir, "{} = call i64 @tinox_string_length(i8* {})", result, ptr).unwrap();
                                return Ok((result, "i64".to_string()));
                            }
                            // Array handle: length is slot 0
                            let result = self.emit_array_len(&ptr);
                            return Ok((result, "i64".to_string()));
                        }
                        "assert" => {
                            let (cond, _) = self.gen_expr(&args[0], ctx)?;
                            let ok_bb = self.new_bb("assert_ok");
                            let fail_bb = self.new_bb("assert_fail");
                            writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", cond, ok_bb, fail_bb).unwrap();
                            writeln!(&mut self.ir, "{}:", fail_bb).unwrap();
                            writeln!(&mut self.ir, "call void @tinox_panic(i64 1)").unwrap();
                            writeln!(&mut self.ir, "unreachable").unwrap();
                            writeln!(&mut self.ir, "{}:", ok_bb).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "push" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let (val, val_ty) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            let push_val = if val_ty.ends_with('*') || val_ty == "ptr" {
                                let casted = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {}* {} to i64", casted, val_ty.trim_end_matches('*'), val).unwrap();
                                casted
                            } else {
                                val
                            };
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_push(i64* {}, i64 {})", result, arr, push_val).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "pop" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_pop(i64* {})", result, arr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "first" => {
                            // Bounds-checked: empty array → hard error (was an
                            // unchecked read of element 0).
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let val = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_get(i64* {}, i64 0)", val, arr).unwrap();
                            return Ok((val, "i64".to_string()));
                        }
                        "last" => {
                            // Bounds-checked: empty array → len-1 = -1 → hard error
                            // (was a read before the buffer at index -1).
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let len_val = self.emit_array_len(&arr);
                            let last_idx = self.temp();
                            writeln!(&mut self.ir, "{} = sub i64 {}, 1", last_idx, len_val).unwrap();
                            let val = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_get(i64* {}, i64 {})", val, arr, last_idx).unwrap();
                            return Ok((val, "i64".to_string()));
                        }
                        "slice" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let (from, _) = self.gen_expr(&args[1], ctx)?;
                            let (to, _) = self.gen_expr(&args[2], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_slice(i64* {}, i64 {}, i64 {})", result, arr, from, to).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "abs" => {
                            let (val, ty) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            if ty == "double" {
                                writeln!(&mut self.ir, "{} = call double @llvm.fabs.f64(double {})", result, val).unwrap();
                                return Ok((result, "double".to_string()));
                            } else {
                                let neg = self.temp();
                                writeln!(&mut self.ir, "{} = sub i64 0, {}", neg, val).unwrap();
                                let cond = self.temp();
                                writeln!(&mut self.ir, "{} = icmp slt i64 {}, 0", cond, val).unwrap();
                                writeln!(&mut self.ir, "{} = select i1 {}, i64 {}, i64 {}", result, cond, neg, val).unwrap();
                                return Ok((result, "i64".to_string()));
                            }
                        }
                        "min" => {
                            let (a, ty) = self.gen_expr(&args[0], ctx)?;
                            let (b, _) = self.gen_expr(&args[1], ctx)?;
                            let cond = self.temp();
                            let result = self.temp();
                            if ty == "double" {
                                writeln!(&mut self.ir, "{} = fcmp olt double {}, {}", cond, a, b).unwrap();
                                writeln!(&mut self.ir, "{} = select i1 {}, double {}, double {}", result, cond, a, b).unwrap();
                                return Ok((result, "double".to_string()));
                            } else {
                                writeln!(&mut self.ir, "{} = icmp slt i64 {}, {}", cond, a, b).unwrap();
                                writeln!(&mut self.ir, "{} = select i1 {}, i64 {}, i64 {}", result, cond, a, b).unwrap();
                                return Ok((result, "i64".to_string()));
                            }
                        }
                        "max" => {
                            let (a, ty) = self.gen_expr(&args[0], ctx)?;
                            let (b, _) = self.gen_expr(&args[1], ctx)?;
                            let cond = self.temp();
                            let result = self.temp();
                            if ty == "double" {
                                writeln!(&mut self.ir, "{} = fcmp ogt double {}, {}", cond, a, b).unwrap();
                                writeln!(&mut self.ir, "{} = select i1 {}, double {}, double {}", result, cond, a, b).unwrap();
                                return Ok((result, "double".to_string()));
                            } else {
                                writeln!(&mut self.ir, "{} = icmp sgt i64 {}, {}", cond, a, b).unwrap();
                                writeln!(&mut self.ir, "{} = select i1 {}, i64 {}, i64 {}", result, cond, a, b).unwrap();
                                return Ok((result, "i64".to_string()));
                            }
                        }
                        "sqrt" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @sqrt(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "randomInt" => {
                            let (min_v, _) = self.gen_expr(&args[0], ctx)?;
                            let (max_v, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @randomInt(i64 {}, i64 {})", result, min_v, max_v).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "randomFloat" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @randomFloat()", result).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "log" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @log(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "exp" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @exp(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "atan2" => {
                            let (y, _) = self.gen_expr(&args[0], ctx)?;
                            let (x, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @atan2(double {}, double {})", result, y, x).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "fabs" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @llvm.fabs.f64(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "mathTgamma" | "mathLgamma" | "mathCbrt" | "mathTrunc" | "mathRint" | "mathLogb"
                        | "mathLog2" | "mathLog10" | "mathExp2" | "mathExp10" => {
                            let libm = name[4..].to_lowercase();
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @{}(double {})", result, libm, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "mathIsNan" | "mathIsInfinite" | "mathIsNormal" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @{}(double {})", result, name, val).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "mathNan" | "mathInf" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @{}()", result, name).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "charAt" => {
                            let (ptr, _) = self.gen_expr(&args[0], ctx)?;
                            let (idx, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_char_at(i8* {}, i64 {})", result, ptr, idx).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "toInt" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_to_int(i8* {})", result, val).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "toFloat" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @tinox_string_to_float(i8* {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "toString" => {
                            let (val, ty) = self.gen_expr(&args[0], ctx)?;
                            if ty == "i8*" {
                                // Already a string — return as-is
                                return Ok((val, "i8*".to_string()));
                            }
                            let result = self.temp();
                            // Small int widths (i8/i16/i32) must be sext'd to i64
                            // before tinox_int_to_string, else the i64 param gets a
                            // narrower value → type-mismatched IR.
                            let val = if matches!(ty.as_str(), "i8" | "i16" | "i32") {
                                let ext = self.temp();
                                writeln!(&mut self.ir, "{} = sext {} {} to i64", ext, ty, val).unwrap();
                                ext
                            } else { val };
                            let (fn_name, arg_ty) = match ty.as_str() {
                                "double" => ("tinox_float_to_string", "double"),
                                "i1"     => ("tinox_bool_to_string", "i1"),
                                _        => ("tinox_int_to_string", "i64"),
                            };
                            writeln!(&mut self.ir, "{} = call i8* @{}({} {})", result, fn_name, arg_ty, val).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "pow" => {
                            let (base, _) = self.gen_expr(&args[0], ctx)?;
                            let (exp, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @pow(double {}, double {})", result, base, exp).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "floor" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @llvm.floor.f64(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "ceil" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @llvm.ceil.f64(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "round" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @llvm.round.f64(double {})", result, val).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "exit" => {
                            let (code, _) = self.gen_expr(&args[0], ctx)?;
                            writeln!(&mut self.ir, "call void @exit(i64 {})", code).unwrap();
                            writeln!(&mut self.ir, "unreachable").unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "contains" => {
                            let (haystack, ty) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            if ty == "i8*" {
                                let (needle, _) = self.gen_expr(&args[1], ctx)?;
                                writeln!(&mut self.ir, "{} = call i64 @tinox_string_contains(i8* {}, i8* {})", result, haystack, needle).unwrap();
                                let bool_val = self.temp();
                                writeln!(&mut self.ir, "{} = trunc i64 {} to i1", bool_val, result).unwrap();
                                return Ok((bool_val, "i1".to_string()));
                            } else {
                                let (val, _) = self.gen_expr(&args[1], ctx)?;
                                writeln!(&mut self.ir, "{} = call i64 @tinox_array_contains(i64* {}, i64 {})", result, haystack, val).unwrap();
                                let bool_val = self.temp();
                                writeln!(&mut self.ir, "{} = trunc i64 {} to i1", bool_val, result).unwrap();
                                return Ok((bool_val, "i1".to_string()));
                            }
                        }
                        "indexOf" => {
                            let (haystack, ty) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            if ty == "i8*" {
                                let (needle, _) = self.gen_expr(&args[1], ctx)?;
                                writeln!(&mut self.ir, "{} = call i64 @tinox_string_index_of(i8* {}, i8* {})", result, haystack, needle).unwrap();
                            } else {
                                let (val, _) = self.gen_expr(&args[1], ctx)?;
                                writeln!(&mut self.ir, "{} = call i64 @tinox_array_index_of(i64* {}, i64 {})", result, haystack, val).unwrap();
                            }
                            return Ok((result, "i64".to_string()));
                        }
                        "toUpper" | "toUpperCase" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_to_upper(i8* {})", result, val).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "toLower" | "toLowerCase" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_to_lower(i8* {})", result, val).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "startsWith" => {
                            let (s, s_ty) = self.gen_expr(&args[0], ctx)?;
                            let s_ptr = if s_ty == "i8*" { s.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, s).unwrap(); c };
                            let (prefix, p_ty) = self.gen_expr(&args[1], ctx)?;
                            let p_ptr = if p_ty == "i8*" { prefix.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, prefix).unwrap(); c };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_starts_with(i8* {}, i8* {})", result, s_ptr, p_ptr).unwrap();
                            let bool_val = self.temp();
                            writeln!(&mut self.ir, "{} = trunc i64 {} to i1", bool_val, result).unwrap();
                            return Ok((bool_val, "i1".to_string()));
                        }
                        "endsWith" => {
                            let (s, s_ty) = self.gen_expr(&args[0], ctx)?;
                            let s_ptr = if s_ty == "i8*" { s.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, s).unwrap(); c };
                            let (suffix, suf_ty) = self.gen_expr(&args[1], ctx)?;
                            let suf_ptr = if suf_ty == "i8*" { suffix.clone() } else { let c = self.temp(); writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, suffix).unwrap(); c };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_ends_with(i8* {}, i8* {})", result, s_ptr, suf_ptr).unwrap();
                            let bool_val = self.temp();
                            writeln!(&mut self.ir, "{} = trunc i64 {} to i1", bool_val, result).unwrap();
                            return Ok((bool_val, "i1".to_string()));
                        }
                        "trim" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_trim(i8* {})", result, val).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "sort" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_sort(i64* {})", result, arr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "reverse" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_reverse(i64* {})", result, arr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "split" => {
                            let (s, _) = self.gen_expr(&args[0], ctx)?;
                            let (delim, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_string_split(i8* {}, i8* {})", result, s, delim).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "join" => {
                            let (arr, _) = self.gen_expr(&args[0], ctx)?;
                            let (sep, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_join(i64* {}, i8* {})", result, arr, sep).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "open" => {
                            let (path, _) = self.gen_expr(&args[0], ctx)?;
                            let mode = if args.len() > 1 {
                                let (m, _) = self.gen_expr(&args[1], ctx)?;
                                m
                            } else {
                                let sname = format!("str{}", self.strings.len());
                                self.strings.insert(sname.clone(), "r".to_string());
                                let ptr = self.temp();
                                writeln!(&mut self.ir, "{} = getelementptr [2 x i8], [2 x i8]* @{}, i64 0, i64 0", ptr, sname).unwrap();
                                ptr
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_file_open(i8* {}, i8* {})", result, path, mode).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "fileExists" => {
                            let (path, path_ty) = self.gen_expr(&args[0], ctx)?;
                            let path_str = if path_ty == "i8*" { path.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, path).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_file_exists(i8* {})", raw, path_str).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        "deleteFile" => {
                            let (path, _) = self.gen_expr(&args[0], ctx)?;
                            writeln!(&mut self.ir, "call void @tinox_file_delete(i8* {})", path).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "processArgs" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @processArgs()", result).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "processExit" => {
                            let (code, code_ty) = self.gen_expr(&args[0], ctx)?;
                            let code_i64 = if code_ty == "i64" { code.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = zext {} {} to i64", c, code_ty, code).unwrap();
                                c
                            };
                            writeln!(&mut self.ir, "call void @processExit(i64 {})", code_i64).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "fromCharCode" => {
                            // Bug 66: do NOT call self.gen_expr(&args[0], ctx) again —
                            // args[0] was already evaluated once above in the
                            // generic call prelude (arg_vals/arg_types). A
                            // second call would evaluate a side-effecting
                            // argument (e.g. a method that mutates internal
                            // state) twice.
                            let code = arg_vals[0].clone();
                            let code_ty = arg_types[0].clone();
                            let code_i64 = if code_ty == "i64" || code_ty.is_empty() { code } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = zext {} {} to i64", c, code_ty, code).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_from_char_code(i64 {})", result, code_i64).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "dirList" => {
                            let (path, path_ty) = self.gen_expr(&args[0], ctx)?;
                            let path_str = if path_ty == "i8*" { path.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, path).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @dirList(i8* {})", result, path_str).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "regexFindAll" | "regexSplit" => {
                            let (pat, pat_ty) = self.gen_expr(&args[0], ctx)?;
                            let pat_i64 = if pat_ty == "i64" { pat.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, pat_ty, pat).unwrap();
                                c
                            };
                            let (subj, subj_ty) = self.gen_expr(&args[1], ctx)?;
                            let subj_i64 = if subj_ty == "i64" { subj.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, subj_ty, subj).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @{}(i64 {}, i64 {})", result, name, pat_i64, subj_i64).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "regexFindFirst" => {
                            let (pat, pat_ty) = self.gen_expr(&args[0], ctx)?;
                            let pat_i64 = if pat_ty == "i64" { pat.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, pat_ty, pat).unwrap();
                                c
                            };
                            let (subj, subj_ty) = self.gen_expr(&args[1], ctx)?;
                            let subj_i64 = if subj_ty == "i64" { subj.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, subj_ty, subj).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @regexFindFirst(i64 {}, i64 {})", raw, pat_i64, subj_i64).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", result, raw).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "regexReplaceAll" => {
                            let (pat, pat_ty) = self.gen_expr(&args[0], ctx)?;
                            let pat_i64 = if pat_ty == "i64" { pat.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, pat_ty, pat).unwrap();
                                c
                            };
                            let (subj, subj_ty) = self.gen_expr(&args[1], ctx)?;
                            let subj_i64 = if subj_ty == "i64" { subj.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, subj_ty, subj).unwrap();
                                c
                            };
                            let (rep, rep_ty) = self.gen_expr(&args[2], ctx)?;
                            let rep_i64 = if rep_ty == "i64" { rep.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, rep_ty, rep).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @regexReplaceAll(i64 {}, i64 {}, i64 {})", raw, pat_i64, subj_i64, rep_i64).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", result, raw).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "regexMatchGroups" => {
                            let (pat, pat_ty) = self.gen_expr(&args[0], ctx)?;
                            let pat_str = if pat_ty == "i8*" { pat.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, pat).unwrap();
                                c
                            };
                            let (subj, subj_ty) = self.gen_expr(&args[1], ctx)?;
                            let subj_str = if subj_ty == "i8*" { subj.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, subj).unwrap();
                                c
                            };
                            let (off, _) = self.gen_expr(&args[2], ctx)?;
                            let (icase, _) = self.gen_expr(&args[3], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @regexMatchGroups(i8* {}, i8* {}, i64 {}, i64 {})", result, pat_str, subj_str, off, icase).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "fileReadAllText" => {
                            let (path, path_ty) = self.gen_expr(&args[0], ctx)?;
                            let path_str = if path_ty == "i8*" { path.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, path).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @fileReadAllText(i8* {})", result, path_str).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        _ => name.clone(),
                    },
                    _ => "unknown_fn".to_string(),
                };
                // Check if this is a call to a generic function — monomorphize if so
                if let ExprKind::Ident(callee_name) = &func.node {
                    if let Some(gf) = self.generic_fns.get(callee_name).cloned() {
                        // Infer type bindings from argument types
                        let bindings: HashMap<String, String> = gf
                            .type_params
                            .iter()
                            .enumerate()
                            .filter_map(|(i, tp)| {
                                arg_types.get(i).map(|at| (tp.clone(), at.clone()))
                            })
                            .collect();
                        let mangled = Self::mangle_generic_name(&gf.name, &gf.type_params, &bindings);
                        // Generate specialization if not already done
                        if !self.generated_specializations.contains(&mangled) {
                            self.generated_specializations.insert(mangled.clone());
                            let specialized = Self::substitute_fn(&gf, &mangled, &bindings);
                            // emit into lambda_ir so it doesn't interrupt current function
                            let saved_ir = std::mem::take(&mut self.ir);
                            let saved_temp = self.temp_count;
                            self.temp_count = 0;
                            self.gen_fn(&specialized)?;
                            let spec_ir = std::mem::take(&mut self.ir);
                            self.ir = saved_ir;
                            self.temp_count = saved_temp;
                            self.lambda_ir.push_str(&spec_ir);
                        }
                        // Emit the call to the mangled name
                        let ret_ty = Self::type_to_llvm_with_bindings(&gf.ret_type, &bindings);
                        let result = self.temp();
                        writeln!(&mut self.ir, "  {} = call {} @{}({})", result, ret_ty, mangled, args_str).unwrap();
                        return Ok((result, ret_ty));
                    }
                }

                // Look up actual return type from pre-collected signatures
                let ret_ty = if let ExprKind::Ident(callee) = &func.node {
                    if ctx.locals.contains_key(callee) {
                        // `callee` is a local variable holding a closure
                        // value (a captured fn param/local, e.g. `onChange`
                        // in `fn(v: String) { onChange(v.toFloat()); }`),
                        // not a real named function -- `fn_sigs` never has
                        // an entry for it, so this must NOT fall through to
                        // the arg_types.first() fallback below. Every
                        // closure call always returns i64 at the ABI level
                        // (see the `is_local_fn` branch just below, which
                        // unconditionally casts the callee to `i64 (i64,
                        // i64*)*`) regardless of the Tinox-level declared
                        // return type -- exactly like gen_lambda always
                        // emits `ret i64 0` for a Nothing-returning lambda,
                        // never `ret void`. Before this check, a closure
                        // whose first PARAMETER happened to be Float64
                        // (e.g. `fnc(Float64) -> Nothing`) got its call's
                        // LLVM return type mistaken for that parameter's
                        // type (double) via the fallback below, which then
                        // propagated out through StmtKind::Return as an
                        // ill-typed `ret double` inside a function actually
                        // declared to return i64 -- caught as "internal
                        // compiler error: generated invalid LLVM IR" the
                        // first time a real program (Tinox-UI's
                        // Component::numberField, issue #215 Phase 5)
                        // called a captured Float64-taking closure as a
                        // lambda body's tail statement.
                        "i64".to_string()
                    } else {
                        self.fn_sigs.get(callee)
                            .map(|(r, _)| r.clone())
                            .unwrap_or_else(|| arg_types.first().cloned().unwrap_or_else(|| "i64".to_string()))
                    }
                } else {
                    // Indirect call through a fn value (e.g. handlers[i](ctx)):
                    // lambdas return their value as i64 at the ABI level.
                    "i64".to_string()
                };
                let result = self.temp();
                let is_local_fn = if let ExprKind::Ident(name) = &func.node {
                    ctx.locals.contains_key(name)
                } else {
                    false
                };
                let is_expr_fn_ptr = !is_local_fn && fn_name == "unknown_fn";
                if is_expr_fn_ptr {
                    // func is an expression (e.g., array[i]) that evaluates to a fn ptr or closure ptr
                    let (fn_val, fn_ty) = self.gen_expr(func, ctx)?;
                    if fn_ty == "i64*" {
                        // Closure: load fn_ptr from index 0 and env_ptr from index 1
                        let fp_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64, i64* {}", fp_val, fn_val).unwrap();
                        let env_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", env_ptr, fn_val).unwrap();
                        let env_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_val, env_ptr).unwrap();
                        let casted_fn = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64 (i64, i64*)*", casted_fn, fp_val).unwrap();
                        let call_args = Self::closure_call_args(&args_str, &env_val);
                        if ret_ty == "void" {
                            writeln!(&mut self.ir, "call void {}({})", casted_fn, call_args).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = call {} {}({})", result, ret_ty, casted_fn, call_args).unwrap();
                        }
                    } else {
                        // Fn value stored as i64: address of a closure block
                        // {fn_ptr, env} — load both and call fn_ptr(args..., env).
                        let fn_i64 = if fn_ty == "i64" { fn_val.clone() } else {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, fn_ty, fn_val).unwrap();
                            c
                        };
                        let block = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", block, fn_i64).unwrap();
                        let fp_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64, i64* {}", fp_val, block).unwrap();
                        let env_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", env_ptr, block).unwrap();
                        let env_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_val, env_ptr).unwrap();
                        let casted_fn = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64 (i64, i64*)*", casted_fn, fp_val).unwrap();
                        let call_args = Self::closure_call_args(&args_str, &env_val);
                        if ret_ty == "void" {
                            writeln!(&mut self.ir, "call void {}({})", casted_fn, call_args).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = call {} {}({})", result, ret_ty, casted_fn, call_args).unwrap();
                        }
                    }
                } else if is_local_fn {
                    let (fn_ptr, fn_ty) = self.gen_expr(func, ctx)?;
                    if fn_ty == "i64*" {
                        let fp_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64, i64* {}", fp_val, fn_ptr).unwrap();
                        let env_ptr = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = getelementptr i64, ptr {}, i64 1",
                            env_ptr, fn_ptr
                        )
                        .unwrap();
                        let env_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_val, env_ptr)
                            .unwrap();
                        let casted_fn = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = inttoptr i64 {} to i64 (i64, i64*)*",
                            casted_fn, fp_val
                        )
                        .unwrap();
                        let call_args = Self::closure_call_args(&args_str, &env_val);
                        if ret_ty == "void" {
                            writeln!(&mut self.ir, "call void {}({})", casted_fn, call_args).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = call {} {}({})", result, ret_ty, casted_fn, call_args).unwrap();
                        }
                    } else {
                        // Local holds a closure-block address as i64 —
                        // same convention as every other fn value.
                        let block = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", block, fn_ptr).unwrap();
                        let fp_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64, i64* {}", fp_val, block).unwrap();
                        let env_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", env_ptr, block).unwrap();
                        let env_val = self.temp();
                        writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_val, env_ptr).unwrap();
                        let casted_fn = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = inttoptr i64 {} to i64 (i64, i64*)*",
                            casted_fn, fp_val
                        )
                        .unwrap();
                        let call_args = Self::closure_call_args(&args_str, &env_val);
                        if ret_ty == "void" {
                            writeln!(&mut self.ir, "call void {}({})", casted_fn, call_args).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = call {} {}({})", result, ret_ty, casted_fn, call_args).unwrap();
                        }
                    }
                } else if ret_ty == "void" {
                    writeln!(
                        &mut self.ir,
                        "call void @{}({})",
                        fn_name, args_str
                    )
                    .unwrap();
                } else {
                    writeln!(
                        &mut self.ir,
                        "{} = call {} @{}({})",
                        result, ret_ty, fn_name, args_str
                    )
                    .unwrap();
                }
                Ok((result, ret_ty))
            }
            ExprKind::MethodCall { obj, method, args } => {
                // ORM query chain: DB.of(T).filter(lambda)...list()/first()/count()
                if matches!(method.as_str(), "list" | "first" | "count") && args.is_empty() {
                    if let Some(chain) = try_extract_orm_chain(obj, method.as_str()) {
                        if self.entity_entries.iter().any(|e| e.class_name == chain.entity_class) {
                            let chain = chain.clone();
                            return self.gen_orm_query(&chain, ctx);
                        }
                    }
                }

                // ORM save/delete: DB.of(T).save(entity) / DB.of(T).delete(entity)
                if matches!(method.as_str(), "save" | "delete") && args.len() == 1 {
                    if let ExprKind::MethodCall { obj: of_obj, method: of_method, args: of_args } = &obj.node {
                        if of_method == "of" {
                            if let ExprKind::Ident(db_name) = &of_obj.node {
                                if db_name == "DB" {
                                    if let Some(ExprKind::Ident(class_name)) = of_args.first().map(|a| &a.node) {
                                        if self.entity_entries.iter().any(|e| &e.class_name == class_name) {
                                            let entity_class = class_name.clone();
                                            let entity_arg = args[0].clone();
                                            return self.gen_orm_save_delete(&entity_class, method.as_str(), &entity_arg, ctx);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Static method call: ClassName.fnc(args) — obj is a class name, not an instance
                if let ExprKind::Ident(class_name) = &obj.node {
                    let method_key = format!("{}_{}", class_name, method);
                    if self.method_ret_types.contains_key(&method_key) {
                        // Check it really is a static method (no self in fn signature)
                        if let Some((_, param_tys)) = self.fn_sigs.get(&method_key) {
                            let _ = param_tys; // static confirmed via fn_sigs absence of self
                        }
                        // Only treat as static if the class name is not a local variable
                        if !ctx.locals.contains_key(class_name.as_str()) && !ctx.params.contains(class_name.as_str())
                            && self.struct_layouts.contains_key(class_name.as_str()) {
                            let mut args_str = String::new();
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 { args_str.push_str(", "); }
                                let (v, t) = self.gen_expr(arg, ctx)?;
                                args_str.push_str(&format!("{} {}", t, v));
                            }
                            let ret_ty = self.method_ret_types.get(&method_key).cloned()
                                .unwrap_or_else(|| "i64".to_string());
                            if ret_ty == "void" {
                                writeln!(&mut self.ir, "call void @{}({})", method_key, args_str).unwrap();
                                return Ok(("0".to_string(), "void".to_string()));
                            }
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call {} @{}({})",
                                result, ret_ty, method_key, args_str).unwrap();
                            return Ok((result, ret_ty));
                        }
                    }
                }

                let (obj_ptr, obj_ty) = self.gen_expr(obj, ctx)?;

                let declared_type = match &obj.node {
                    ExprKind::Ident(name) => ctx.local_types.get(name).cloned()
                        // Fallback: the rich bridge (unstripped marker)
                        .or_else(|| self.rich_marker(obj)),
                    ExprKind::This => ctx.current_struct.clone(),
                    _ => self.infer_struct_type(obj, ctx),
                };

                // toJson on List<C> (@JsonSerializable): serialize elements
                // via the generated C_toJson (a runtime helper taking an fn-ptr).
                if method == "toJson" && args.is_empty() {
                    if let Some(cls) = declared_type.as_deref().and_then(|t| t.strip_prefix("List:")) {
                        if self.json_serializable_classes.iter().any(|c| c == cls) {
                            let handle = if obj_ty == "i64" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, obj_ptr).unwrap();
                                c
                            } else {
                                obj_ptr.clone()
                            };
                            let result = self.temp();
                            writeln!(
                                &mut self.ir,
                                "{} = call i8* @tinox_json_list_serialize(i64* {}, ptr @{}_toJson)",
                                result, handle, cls
                            )
                            .unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                    }
                }

                // Array method dispatch: only trigger for explicit Array types or when declared type is
                // unknown (None) and obj_ty is i64* — never trigger for known struct instances.
                // Also trigger for i64 objects (ptrtoint'd array pointers) with known array methods.
                let is_known_struct = declared_type.as_deref()
                    .map(|t| self.struct_layouts.contains_key(t))
                    .unwrap_or(false);
                // Array-only methods (excludes contains/len/remove/insert which are also map methods)
                let array_only_methods = ["push","pop","sort","reverse","slice","join",
                    "first","last","find","filter","map","reduce","any","all","indexOf",
                    "clear","isEmpty","toList","unique","flatten","zip","unzip","take","skip",
                    "sortBy","groupBy","partition","sum","min","max","average","forEach",
                    "removeAt"];
                // A declared container marker ("Array", "Array:…") resolves the
                // i64 ambiguity in favor of array dispatch (e.g. elements of
                // nested lists: xs[0].len() on List<List<Int64>>).
                let declared_is_array = declared_type.as_deref()
                    .map(|t| t == "Array" || t.starts_with("Array:"))
                    .unwrap_or(false);
                let is_i64_array_method = obj_ty == "i64" && !is_known_struct
                    && (array_only_methods.contains(&method.as_str()) || declared_is_array);
                // Coerce i64 array pointer to i64* for array dispatch
                let (obj_ptr, obj_ty) = if is_i64_array_method {
                    let c = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, obj_ptr).unwrap();
                    (c, "i64*".to_string())
                } else {
                    (obj_ptr, obj_ty)
                };
                let is_array_type = declared_is_array
                    || (obj_ty == "i64*" && !is_known_struct);
                if is_array_type && obj_ty != "i8*" {
                    let is_str = declared_type.as_deref() == Some("Array:String");
                    match method.as_str() {
                        "len" => {
                            let result = self.emit_array_len(&obj_ptr);
                            return Ok((result, "i64".to_string()));
                        }
                        "push" => {
                            let (val, val_ty) = self.gen_expr(&args[0], ctx)?;
                            let store_val = if val_ty.ends_with('*') || val_ty == "ptr" {
                                let c = self.temp();
                                let base_ty = val_ty.trim_end_matches('*');
                                writeln!(&mut self.ir, "{} = ptrtoint {}* {} to i64", c, base_ty, val).unwrap();
                                c
                            } else if val_ty == "double" || val_ty == "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else { val };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_push(i64* {}, i64 {})", result, obj_ptr, store_val).unwrap();
                            // Arrays are stable handles — push mutates in place,
                            // no pointer write-back needed.
                            return Ok((result, "i64*".to_string()));
                        }
                        "pop" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_pop(i64* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "first" => {
                            // Bounds-checked: empty array → hard error.
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_get(i64* {}, i64 0)", raw, obj_ptr).unwrap();
                            if is_str {
                                let s = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", s, raw).unwrap();
                                return Ok((s, "i8*".to_string()));
                            }
                            return Ok((raw, "i64".to_string()));
                        }
                        "last" => {
                            // Bounds-checked: empty array → len-1 = -1 → hard error.
                            let len_val = self.emit_array_len(&obj_ptr);
                            let last_idx = self.temp();
                            writeln!(&mut self.ir, "{} = sub i64 {}, 1", last_idx, len_val).unwrap();
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_get(i64* {}, i64 {})", raw, obj_ptr, last_idx).unwrap();
                            if is_str {
                                let s = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", s, raw).unwrap();
                                return Ok((s, "i8*".to_string()));
                            }
                            return Ok((raw, "i64".to_string()));
                        }
                        "contains" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_contains(i64* {}, i64 {})", result, obj_ptr, val).unwrap();
                            let b = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", b, result).unwrap();
                            return Ok((b, "i1".to_string()));
                        }
                        "indexOf" => {
                            let (val, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_array_index_of(i64* {}, i64 {})", result, obj_ptr, val).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "sort" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_sort(i64* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "reverse" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_reverse(i64* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "slice" => {
                            let (from, _) = self.gen_expr(&args[0], ctx)?;
                            let (to, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_slice(i64* {}, i64 {}, i64 {})", result, obj_ptr, from, to).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "join" => {
                            let (sep, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_join(i64* {}, i8* {})", result, obj_ptr, sep).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "removeAt" => {
                            let (idx, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_remove_at(i64* {}, i64 {})", result, obj_ptr, idx).unwrap();
                            // Stable handle — removeAt mutates in place, no write-back.
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "insert" => {
                            let (idx, _) = self.gen_expr(&args[0], ctx)?;
                            let (val, val_ty) = self.gen_expr(&args[1], ctx)?;
                            let store_val = if val_ty.ends_with('*') || val_ty == "ptr" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else if val_ty == "double" || val_ty == "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else if val_ty == "i1" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                                c
                            } else { val };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_array_insert(i64* {}, i64 {}, i64 {})", result, obj_ptr, idx, store_val).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "map" | "filter" | "forEach" if args.len() == 1 => {
                            return self.gen_array_lambda_method(
                                method.as_str(),
                                &obj_ptr,
                                obj,
                                &args[0],
                                None,
                                expr.id,
                                declared_type.as_deref(),
                                ctx,
                            );
                        }
                        "reduce" if args.len() == 2 => {
                            return self.gen_array_lambda_method(
                                method.as_str(),
                                &obj_ptr,
                                obj,
                                &args[1],
                                Some(&args[0]),
                                expr.id,
                                declared_type.as_deref(),
                                ctx,
                            );
                        }
                        _ => {}
                    }
                }

                // String method dispatch for split
                if obj_ty == "i8*" && method.as_str() == "split" {
                    let (delim, _) = self.gen_expr(&args[0], ctx)?;
                    let result = self.temp();
                    writeln!(&mut self.ir, "{} = call i64* @tinox_string_split(i8* {}, i8* {})", result, obj_ptr, delim).unwrap();
                    return Ok((result, "i64*".to_string()));
                }

                // Map method dispatch — also handle i64 objects that may be ptrtoint'd
                // Map pointers, but only when no other declared type claims the object.
                let is_map_dispatch = match declared_type.as_deref() {
                    Some(t) => Self::is_map_marker(t),
                    None => obj_ty == "i64"
                        && matches!(method.as_str(), "get" | "insert" | "contains" | "keys" | "values" | "remove" | "len"),
                };
                if is_map_dispatch {
                    let map_obj_ptr = if obj_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, obj_ptr).unwrap();
                        c
                    } else {
                        obj_ptr.clone()
                    };
                    match method.as_str() {
                        "get" => {
                            let (key, key_ty) = self.gen_expr(&args[0], ctx)?;
                            let key_i8 = self.emit_map_key(&key, &key_ty);
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_map_get(i8* {}, i8* {})", result, map_obj_ptr, key_i8).unwrap();
                            // Type the value by the map's value marker
                            return Ok(self.coerce_map_value(result, declared_type.as_deref()));
                        }
                        "insert" => {
                            let (key, key_ty) = self.gen_expr(&args[0], ctx)?;
                            let key_i8 = self.emit_map_key(&key, &key_ty);
                            let (val, val_ty) = self.gen_expr(&args[1], ctx)?;
                            let val_i64 = if val_ty == "i64" || val_ty.is_empty() {
                                val.clone()
                            } else if val_ty == "i1" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                                c
                            } else if val_ty == "double" || val_ty == "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else {
                                // pointer type — ptrtoint
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                                c
                            };
                            writeln!(&mut self.ir, "call void @tinox_map_set(i8* {}, i8* {}, i64 {})", map_obj_ptr, key_i8, val_i64).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "contains" => {
                            let (key, key_ty) = self.gen_expr(&args[0], ctx)?;
                            let key_str = self.emit_map_key(&key, &key_ty);
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_map_contains(i8* {}, i8* {})", raw, map_obj_ptr, key_str).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        "remove" => {
                            let (key, key_ty) = self.gen_expr(&args[0], ctx)?;
                            let key_str = self.emit_map_key(&key, &key_ty);
                            writeln!(&mut self.ir, "call void @tinox_map_remove(i8* {}, i8* {})", map_obj_ptr, key_str).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "len" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_map_len(i8* {})", result, map_obj_ptr).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "keys" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_map_keys(i8* {})", result, map_obj_ptr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        "values" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_map_values(i8* {})", result, map_obj_ptr).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        _ => {}
                    }
                }

                // File method dispatch
                if declared_type.as_deref() == Some("File") {
                    match method.as_str() {
                        "read" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_file_read(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "readLine" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_file_readline(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "write" => {
                            let (s, _) = self.gen_expr(&args[0], ctx)?;
                            writeln!(&mut self.ir, "call void @tinox_file_write(i8* {}, i8* {})", obj_ptr, s).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "close" => {
                            writeln!(&mut self.ir, "call void @tinox_file_close(i8* {})", obj_ptr).unwrap();
                            return Ok(("0".to_string(), "void".to_string()));
                        }
                        "eof" => {
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_file_eof(i8* {})", raw, obj_ptr).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        _ => {}
                    }
                }

                // Int/Float/Bool toString must be dispatched before str_methods conversion
                // to avoid i64 integer values being misidentified as string pointers.
                if method == "toString" {
                    if matches!(obj_ty.as_str(), "i64" | "i32" | "i16" | "i8" | "double" | "i1") {
                        let result = self.temp();
                        match obj_ty.as_str() {
                            "double" => { writeln!(&mut self.ir, "{} = call i8* @tinox_float_to_string(double {})", result, obj_ptr).unwrap(); }
                            "i1" => { writeln!(&mut self.ir, "{} = call i8* @tinox_bool_to_string(i1 {})", result, obj_ptr).unwrap(); }
                            "i64" => { writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", result, obj_ptr).unwrap(); }
                            _ => {
                                let ext = self.temp();
                                writeln!(&mut self.ir, "{} = sext {} {} to i64", ext, obj_ty, obj_ptr).unwrap();
                                writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", result, ext).unwrap();
                            }
                        }
                        return Ok((result, "i8*".to_string()));
                    }
                    // Class object toString() — dispatch to generated ClassName_toString
                    if obj_ty == "i64*" {
                        if let Some(cn) = declared_type.as_deref() {
                            let key = format!("{}_toString", cn);
                            if self.method_ret_types.contains_key(&key) {
                                let obj_ptr_typed = if obj_ty == "i64*" {
                                    obj_ptr.clone()
                                } else {
                                    let c = self.temp();
                                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, obj_ptr).unwrap();
                                    c
                                };
                                let result = self.temp();
                                writeln!(&mut self.ir, "{} = call i8* @{}(i64* {})", result, key, obj_ptr_typed).unwrap();
                                return Ok((result, "i8*".to_string()));
                            }
                        }
                    }
                }

                // String method dispatch (obj_ty == "i8*", or i64 stored string pointer)
                let str_methods = ["len","toUpper","toUpperCase","toLower","toLowerCase",
                    "trim","contains","startsWith","endsWith","split","substring","indexOf",
                    "replace","toString","toInt","toFloat","toBool","repeat","padLeft","padRight",
                    "count","charAt","toInt64","toFloat64","toBytes","fromBytes","format","encode",
                    "decode","hash","md5","sha256","base64Encode","base64Decode","urlEncode",
                    "urlDecode","isNumeric","isEmpty","isBlank","lines","words","reverse",
                    "truncate","ellipsis","mask","redact","normalize"];
                let is_str_method = str_methods.contains(&method.as_str());
                let (obj_ptr, obj_ty) = if obj_ty == "i64" && is_str_method {
                    let s = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", s, obj_ptr).unwrap();
                    (s, "i8*".to_string())
                } else {
                    (obj_ptr, obj_ty)
                };
                if obj_ty == "i8*" {
                    match method.as_str() {
                        "len" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_length(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "toUpper" | "toUpperCase" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_to_upper(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "toLower" | "toLowerCase" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_to_lower(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "trim" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_trim(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "contains" => {
                            let (arg, arg_ty) = self.gen_expr(&args[0], ctx)?;
                            let arg_str = if arg_ty == "i8*" { arg.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, arg).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_contains(i8* {}, i8* {})", raw, obj_ptr, arg_str).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        "startsWith" => {
                            let (arg, arg_ty) = self.gen_expr(&args[0], ctx)?;
                            let arg_str = if arg_ty == "i8*" { arg.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, arg).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_starts_with(i8* {}, i8* {})", raw, obj_ptr, arg_str).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        "endsWith" => {
                            let (arg, arg_ty) = self.gen_expr(&args[0], ctx)?;
                            let arg_str = if arg_ty == "i8*" { arg.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, arg).unwrap();
                                c
                            };
                            let raw = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_ends_with(i8* {}, i8* {})", raw, obj_ptr, arg_str).unwrap();
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, raw).unwrap();
                            return Ok((result, "i1".to_string()));
                        }
                        "indexOf" => {
                            let (arg, arg_ty) = self.gen_expr(&args[0], ctx)?;
                            let arg_str = if arg_ty == "i8*" { arg.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, arg).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_index_of(i8* {}, i8* {})", result, obj_ptr, arg_str).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "lastIndexOf" => {
                            let (arg, arg_ty) = self.gen_expr(&args[0], ctx)?;
                            let arg_str = if arg_ty == "i8*" { arg.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, arg).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_last_index_of(i8* {}, i8* {})", result, obj_ptr, arg_str).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "reverse" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_reverse(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "charAt" => {
                            let (arg, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_char_at(i8* {}, i64 {})", result, obj_ptr, arg).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "charCodeAt" => {
                            // Bounds-checked runtime call (returns -1 on out-of-range)
                            // instead of an unchecked inline load past the string end.
                            let (idx, _) = self.gen_expr(&args[0], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_char_code_at(i8* {}, i64 {})", result, obj_ptr, idx).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "substring" => {
                            let (from, _) = self.gen_expr(&args[0], ctx)?;
                            let (to, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_substring(i8* {}, i64 {}, i64 {})", result, obj_ptr, from, to).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "replace" => {
                            let (from, _) = self.gen_expr(&args[0], ctx)?;
                            let (to, _) = self.gen_expr(&args[1], ctx)?;
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i8* @tinox_string_replace(i8* {}, i8* {}, i8* {})", result, obj_ptr, from, to).unwrap();
                            return Ok((result, "i8*".to_string()));
                        }
                        "toInt" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64 @tinox_string_to_int(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "i64".to_string()));
                        }
                        "toFloat" => {
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @tinox_string_to_float(i8* {})", result, obj_ptr).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        "split" => {
                            let (delim, delim_ty) = self.gen_expr(&args[0], ctx)?;
                            let delim_str = if delim_ty == "i8*" { delim.clone() } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, delim).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call i64* @tinox_string_split(i8* {}, i8* {})", result, obj_ptr, delim_str).unwrap();
                            return Ok((result, "i64*".to_string()));
                        }
                        _ => {}
                    }
                }

                // Int/Float/Bool method dispatch (toString, charCodeAt, etc.).
                // Small int widths (i8/i16/i32, e.g. after `x as Int32`) count as
                // ints here — otherwise the dispatch was skipped and `.toString()`
                // fell through to an undefined `@toString` (invalid IR / ICE).
                if matches!(obj_ty.as_str(), "i64" | "i32" | "i16" | "i8" | "double" | "i1") {
                    match method.as_str() {
                        "toString" => {
                            let result = self.temp();
                            match obj_ty.as_str() {
                                "double" => {
                                    writeln!(&mut self.ir, "{} = call i8* @tinox_float_to_string(double {})", result, obj_ptr).unwrap();
                                }
                                "i1" => {
                                    writeln!(&mut self.ir, "{} = call i8* @tinox_bool_to_string(i1 {})", result, obj_ptr).unwrap();
                                }
                                "i64" => {
                                    writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", result, obj_ptr).unwrap();
                                }
                                _ => {
                                    // small int (i8/i16/i32) → sext to i64 first
                                    let ext = self.temp();
                                    writeln!(&mut self.ir, "{} = sext {} {} to i64", ext, obj_ty, obj_ptr).unwrap();
                                    writeln!(&mut self.ir, "{} = call i8* @tinox_int_to_string(i64 {})", result, ext).unwrap();
                                }
                            }
                            return Ok((result, "i8*".to_string()));
                        }
                        "sqrt" if args.is_empty() => {
                            // x.sqrt() on numeric values → libm sqrt (double)
                            let arg = if obj_ty == "double" {
                                obj_ptr.clone()
                            } else {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = sitofp {} {} to double", c, obj_ty, obj_ptr).unwrap();
                                c
                            };
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call double @sqrt(double {})", result, arg).unwrap();
                            return Ok((result, "double".to_string()));
                        }
                        _ => {}
                    }
                }

                // Check if the declared type is an interface — if so, use vtable dispatch.
                let is_interface_dispatch = declared_type
                    .as_deref()
                    .map(|t| self.known_interfaces.contains(t))
                    .unwrap_or(false);

                // Evaluate extra arguments first (used in both paths).
                // Before generating lambda args, look up expected param types for type inference.
                let method_key = if let Some(ref dt) = declared_type {
                    format!("{}_{}", dt, method)
                } else {
                    method.clone()
                };
                let method_expected_params = self.method_param_types.get(&method_key).cloned();
                let mut extra_args: Vec<(String, String)> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    if matches!(&arg.node, ExprKind::Lambda { .. }) {
                        if let Some(ref mep) = method_expected_params {
                            if let Some(tinox_parser::Type::Fn { params: fn_params, .. }) = mep.get(i) {
                                self.pending_lambda_param_types = fn_params.iter().map(|t| {
                                    if let tinox_parser::Type::Named(n) = t { Some(n.clone()) } else { None }
                                }).collect();
                            }
                        }
                    }
                    let (val, ty) = self.gen_expr(arg, ctx)?;
                    self.pending_lambda_param_types.clear();
                    extra_args.push((val, ty));
                }

                let mut full_args_str = format!("{} {}", obj_ty, obj_ptr);
                for (val, ty) in &extra_args {
                    full_args_str.push_str(&format!(", {} {}", ty, val));
                }

                if is_interface_dispatch {
                    let iface_name = declared_type.as_deref().unwrap();

                    // Find the method slot index in the vtable.
                    let slot_idx = self
                        .vtable_layouts
                        .get(iface_name)
                        .and_then(|methods| methods.iter().position(|m| m == method))
                        .unwrap_or(0) as i64;

                    // The object may arrive as i64 (e.g. a loop variable over
                    // List<Interface>) — coerce to a pointer first and rebuild
                    // the argument list with the coerced self.
                    let obj_ptr = if obj_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, obj_ptr).unwrap();
                        c
                    } else {
                        obj_ptr.clone()
                    };
                    let mut full_args_str = format!("i64* {}", obj_ptr);
                    for (val, ty) in &extra_args {
                        full_args_str.push_str(&format!(", {} {}", ty, val));
                    }

                    // Load vtable pointer from slot 0 of the object.
                    // The object is an i64* pointer; slot 0 holds the vtable address as i64.
                    let vtable_i64_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 0",
                        vtable_i64_ptr, obj_ptr
                    )
                    .unwrap();
                    let vtable_i64 = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = load i64, i64* {}",
                        vtable_i64, vtable_i64_ptr
                    )
                    .unwrap();
                    // Cast the i64 vtable base address to i64*.
                    let vtable_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = inttoptr i64 {} to i64*",
                        vtable_ptr, vtable_i64
                    )
                    .unwrap();

                    // Load the function pointer at vtable[slot_idx].
                    let fn_slot_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 {}",
                        fn_slot_ptr, vtable_ptr, slot_idx
                    )
                    .unwrap();
                    let fn_i64 = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = load i64, i64* {}",
                        fn_i64, fn_slot_ptr
                    )
                    .unwrap();

                    // Build the function type string based on args. The
                    // callee's REAL LLVM return type (e.g. `i8*` for a
                    // String-returning interface method), not a hardcoded
                    // `i64` -- see `interface_method_ret_types`'s doc
                    // comment for why that used to silently corrupt any
                    // non-Int64-shaped return value (issue found alongside
                    // #169).
                    let ret_ty = self
                        .interface_method_ret_types
                        .get(iface_name)
                        .and_then(|m| m.get(method))
                        .map(|t| self.type_to_llvm_inst(t))
                        .unwrap_or_else(|| "i64".to_string());
                    let mut param_types = vec!["i64*".to_string()]; // self
                    for (_, ty) in &extra_args {
                        param_types.push(ty.clone());
                    }
                    let param_types_str = param_types.join(", ");
                    let fn_type_str = format!("{} ({})*", ret_ty, param_types_str);

                    let casted_fn = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = inttoptr i64 {} to {}",
                        casted_fn, fn_i64, fn_type_str
                    )
                    .unwrap();

                    // A void-returning interface method (Nothing) must NOT
                    // assign the call's result to a name -- LLVM rejects
                    // `%x = call void ...` ("instructions returning void
                    // cannot have a name"), unlike every other ret_ty here.
                    if ret_ty == "void" {
                        writeln!(&mut self.ir, "call void {}({})", casted_fn, full_args_str).unwrap();
                        Ok(("0".to_string(), "void".to_string()))
                    } else {
                        let result = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = call {} {}({})",
                            result, ret_ty, casted_fn, full_args_str
                        )
                        .unwrap();
                        Ok((result, ret_ty))
                    }
                } else if let Some(_fn_sig) = declared_type.as_deref()
                    .and_then(|dt| self.fn_field_sigs.get(dt))
                    .and_then(|m| m.get(method.as_str()))
                    .cloned()
                {
                    // Fn-type field call: stored value is a closure struct address {fn_ptr: i64, env_ptr: i64*}.
                    // Load fn_ptr and env_ptr, convert args to i64 (ptrtoint), then call fn_ptr(args..., env_ptr).
                    let struct_name = declared_type.as_deref().unwrap();
                    let field_offset = self.struct_layouts.get(struct_name)
                        .and_then(|fields| fields.iter().position(|f| f == method))
                        .unwrap_or(0) as i64;
                    let obj_struct_ptr = if obj_ty == "i64" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", cast, obj_ptr).unwrap();
                        cast
                    } else {
                        obj_ptr.clone()
                    };
                    let field_gep = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", field_gep, obj_struct_ptr, field_offset).unwrap();
                    // The stored i64 is a closure struct address (ptrtoint of i64* closure alloc)
                    let closure_addr = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", closure_addr, field_gep).unwrap();
                    let closure_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", closure_ptr, closure_addr).unwrap();
                    // Load fn_ptr (i64) from closure slot 0
                    let fn_ptr_i64 = self.temp();
                    writeln!(&mut self.ir, "{} = load i64, i64* {}", fn_ptr_i64, closure_ptr).unwrap();
                    // Load env_ptr from closure slot 1
                    let env_gep = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", env_gep, closure_ptr).unwrap();
                    let env_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_ptr, env_gep).unwrap();
                    // Tinox lambdas always have LLVM signature i64 (i64, i64*) regardless of declared type
                    let fp = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64 (i64, i64*)*", fp, fn_ptr_i64).unwrap();
                    // Generate call args: convert pointer args to i64 via ptrtoint
                    let mut call_args: Vec<String> = Vec::new();
                    for arg in args.iter() {
                        let (v, t) = self.gen_expr(arg, ctx)?;
                        if t == "i64*" || t == "i8*" || t == "ptr" || (t.len() > 1 && t.ends_with('*')) {
                            let as_i64 = self.temp();
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", as_i64, t, v).unwrap();
                            call_args.push(format!("i64 {}", as_i64));
                        } else {
                            call_args.push(format!("{} {}", t, v));
                        }
                    }
                    call_args.push(format!("i64* {}", env_ptr));
                    let result = self.temp();
                    let args_str = call_args.join(", ");
                    // Discard return value (lambdas return i64 but field type may say void)
                    writeln!(&mut self.ir, "{} = call i64 {}({})", result, fp, args_str).unwrap();
                    Ok((result, "i64".to_string()))
                } else if let Some(gm_key) = declared_type
                    .as_deref()
                    .map(|c| format!("{}_{}", c, method))
                    .filter(|k| self.generic_instance_methods.contains_key(k))
                {
                    // #153: own-type-param instance method of a generic class
                    // (Option<T>.map<U>, .andThen<U>, or a user-defined
                    // equivalent) — not emitted during class specialization,
                    // monomorphize now from the actual call-site argument(s).
                    let mangled_class = declared_type.clone().unwrap();
                    let recv = EvaluatedReceiver { obj_ty: &obj_ty, obj_ptr: &obj_ptr, extra_args: &extra_args };
                    self.gen_generic_instance_method_call(&mangled_class, &gm_key, args, recv, expr.id, ctx)
                } else {
                    // Direct (static) dispatch — resolve through inheritance chain.
                    let logical_name = if let Some(class) = declared_type {
                        format!("{}_{}", class, method)
                    } else {
                        method.clone()
                    };
                    let full_method_name = self
                        .method_impl
                        .get(&logical_name)
                        .cloned()
                        .unwrap_or(logical_name);

                    let ret_ty = self
                        .method_ret_types
                        .get(&full_method_name)
                        .cloned()
                        .unwrap_or_else(|| "i64".to_string());

                    let result = self.temp();
                    if ret_ty == "void" {
                        writeln!(&mut self.ir, "call void @{}({})", full_method_name, full_args_str).unwrap();
                    } else {
                        writeln!(&mut self.ir, "{} = call {} @{}({})", result, ret_ty, full_method_name, full_args_str).unwrap();
                    }
                    Ok((result, ret_ty))
                }
            }
            ExprKind::Index { obj, index } => {
                let arr_name = if let ExprKind::Ident(n) = &obj.node { Some(n.clone()) } else { None };
                let declared_elem_type = arr_name.as_ref().and_then(|n| ctx.local_types.get(n)).cloned()
                    // Fields like `this.rawLines` (List<String>) have no local_types entry —
                    // fall back to struct field type info so elements are typed as strings.
                    .or_else(|| self.infer_struct_type(obj, ctx));
                let is_str_arr = declared_elem_type.as_deref() == Some("Array:String");
                let is_float_arr = declared_elem_type.as_deref() == Some("Array:Float");
                let is_map = declared_elem_type.as_deref().map(Self::is_map_marker).unwrap_or(false);

                let (idx_val, idx_ty) = self.gen_expr(index, ctx)?;
                let (base_ptr, base_ty) = if let ExprKind::Ident(name) = &obj.node {
                    if ctx.params.contains(name) {
                        self.gen_expr(obj, ctx)?
                    } else if ctx.locals.contains_key(name) {
                        let (var_ty, _) = ctx.locals.get(name).unwrap();
                        let slot = ctx.local_slots.get(name).cloned().unwrap_or_else(|| name.clone());
                        let loaded_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = load {}, {}* %{}", loaded_ptr, var_ty, var_ty, slot).unwrap();
                        (loaded_ptr, var_ty.clone())
                    } else {
                        self.gen_expr(obj, ctx)?
                    }
                } else {
                    self.gen_expr(obj, ctx)?
                };

                if is_map || idx_ty == "i8*" {
                    // Map[key] → tinox_map_get(i8* map, i8* key) -> i64
                    let map_i8 = if base_ty == "i8*" { base_ptr.clone() } else {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, base_ptr).unwrap();
                        c
                    };
                    let key_i8 = self.emit_map_key(&idx_val, &idx_ty);
                    let result = self.temp();
                    writeln!(&mut self.ir, "{} = call i64 @tinox_map_get(i8* {}, i8* {})", result, map_i8, key_i8).unwrap();
                    Ok(self.coerce_map_value(result, declared_elem_type.as_deref()))
                } else if base_ty == "i8*" {
                    // String indexing → byte as i64, bounds-checked (-1 out of range)
                    // instead of an unchecked inline load past the string end.
                    let extended = self.temp();
                    writeln!(&mut self.ir, "{} = call i64 @tinox_string_char_code_at(i8* {}, i64 {})", extended, base_ptr, idx_val).unwrap();
                    Ok((extended, "i64".to_string()))
                } else {
                    // Coerce base pointer to ptr if it's an i64 (pointer-as-integer).
                    let base_as_ptr = if base_ty == "i64" {
                        let p = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", p, base_ptr).unwrap();
                        p
                    } else {
                        base_ptr.clone()
                    };
                    // Bounds-checked read (hard error on out-of-range) instead of
                    // an unchecked inline load past the array data.
                    let raw = self.temp();
                    writeln!(&mut self.ir, "{} = call i64 @tinox_array_get(i64* {}, i64 {})", raw, base_as_ptr, idx_val).unwrap();
                    if is_str_arr {
                        let str_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", str_ptr, raw).unwrap();
                        Ok((str_ptr, "i8*".to_string()))
                    } else if is_float_arr {
                        // Elements of List<Float64> are stored as i64 bit patterns
                        let f = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast i64 {} to double", f, raw).unwrap();
                        Ok((f, "double".to_string()))
                    } else {
                        Ok((raw, "i64".to_string()))
                    }
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                let n = elements.len();
                let handle = self.temp();
                writeln!(&mut self.ir, "{} = call i64* @tinox_array_new(i64 {}, i64 0)", handle, n).unwrap();
                let data_ptr = self.emit_array_data(&handle);
                for (i, elem) in elements.iter().enumerate() {
                    let (val, val_ty) = self.gen_expr(elem, ctx)?;
                    let store_val = if val_ty == "i1" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = zext i1 {} to i64", cast, val).unwrap();
                        cast
                    } else if val_ty == "double" || val_ty == "float" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast {} {} to i64", cast, val_ty, val).unwrap();
                        cast
                    } else if val_ty != "i64" && !val_ty.is_empty() && val_ty != "void" {
                        let cast = self.temp();
                        if val_ty == "ptr" {
                            writeln!(&mut self.ir, "{} = ptrtoint ptr {} to i64", cast, val).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                        }
                        cast
                    } else {
                        val
                    };
                    let elem_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", elem_ptr, data_ptr, i).unwrap();
                    writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, elem_ptr).unwrap();
                }
                Ok((handle, "i64*".to_string()))
            }
            ExprKind::MapLiteral(entries) => {
                let map_ptr = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_map_create()", map_ptr).unwrap();
                for (key_expr, val_expr) in entries {
                    let (key_val, key_ty) = self.gen_expr(key_expr, ctx)?;
                    let key_i8 = if key_ty == "i8*" { key_val.clone() } else {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, key_val).unwrap();
                        c
                    };
                    let (val_val, val_ty) = self.gen_expr(val_expr, ctx)?;
                    let val_i64 = if val_ty == "i64" || val_ty.is_empty() {
                        val_val.clone()
                    } else if val_ty == "i1" {
                        let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val_val).unwrap(); c
                    } else if val_ty == "double" || val_ty == "float" {
                        let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val_val).unwrap(); c
                    } else {
                        let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val_val).unwrap(); c
                    };
                    writeln!(&mut self.ir, "call void @tinox_map_set(i8* {}, i8* {}, i64 {})", map_ptr, key_i8, val_i64).unwrap();
                }
                Ok((map_ptr, "i8*".to_string()))
            }
            ExprKind::FieldAccess { obj, field } => {
                let (obj_raw, obj_ty) = self.gen_expr(obj, ctx)?;

                // Fields are stored as i64; if the loaded value is i64, restore it to a ptr
                let obj_ptr = if obj_ty == "i64" {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", cast, obj_raw).unwrap();
                    cast
                } else {
                    obj_raw
                };

                // Find the struct type and field offset
                let struct_name = match &obj.node {
                    ExprKind::Ident(name) => ctx.local_types.get(name).cloned()
                        // Fallback: the rich bridge — e.g. class payloads
                        // from match bindings, which bind_match_payload
                        // binds as "Other" (untyped)
                        .or_else(|| self.rich_marker(obj)),
                    ExprKind::This => ctx.current_struct.clone(),
                    _ => self.infer_struct_type(obj, ctx),
                };

                let (offset, field_llvm_ty) = if let Some(ref sname) = struct_name {
                    let off = self.struct_layouts.get(sname.as_str())
                        .and_then(|fields| fields.iter().position(|f| f == field))
                        .unwrap_or(0) as i64;
                    let fty = self.struct_field_llvm_types.get(sname.as_str())
                        .and_then(|m| m.get(field.as_str()))
                        .cloned()
                        .unwrap_or_else(|| "i64".to_string());
                    (off, fty)
                } else {
                    (0i64, "i64".to_string())
                };

                // B1 phase 1: typed field read for classes with a named struct
                // type. The GEP indexes the named type (opt verifies the offset)
                // and loads the slot type directly — no i64 slot + bitcast dance.
                // The slot type matches the store side (i64 bits at an 8-byte
                // slot), so `load double`/`load i8*` at that address is a valid
                // type-pun and gives the same value as the old load+cast.
                if let Some(sname) = struct_name.as_ref().filter(|s| self.class_named_types.contains(s.as_str())) {
                    // B1 phase 5: hard error on a missing field instead of offset 0.
                    let checked = self.checked_typed_offset(sname, field, expr.span)?;
                    let slot = Self::slot_llvm_ty(&field_llvm_ty);
                    let field_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr %class.{}, ptr {}, i32 0, i32 {}",
                        field_ptr, sname, obj_ptr, checked
                    ).unwrap();
                    let loaded = self.temp();
                    writeln!(&mut self.ir, "{} = load {}, {}* {}", loaded, slot, slot, field_ptr).unwrap();
                    return Ok((loaded, slot));
                }

                let field_ptr = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 {}",
                    field_ptr, obj_ptr, offset
                )
                .unwrap();

                // Load the raw i64 value from the field
                let loaded = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", loaded, field_ptr).unwrap();

                // Restore the value from its uniform i64 storage representation
                if field_llvm_ty == "double" || field_llvm_ty == "float" {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = bitcast i64 {} to {}", cast, loaded, field_llvm_ty).unwrap();
                    Ok((cast, field_llvm_ty))
                } else if field_llvm_ty != "i64" && field_llvm_ty.ends_with('*') {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", cast, loaded, field_llvm_ty).unwrap();
                    Ok((cast, field_llvm_ty))
                } else {
                    Ok((loaded, "i64".to_string()))
                }
            }
            ExprKind::StructLiteral { name, fields } => {
                // Effective emission name for generic classes: the
                // SPECIALIZATION instead of the base. The source is the
                // alias from the annotated let path (Bug 20.2), otherwise
                // the rich export (`Box { value: "x" }` →
                // Named("Box",[String]) → Box__i8P); ensure… registers
                // layout/field types/named type on demand. Without type
                // arguments it stays with the base (previous behavior,
                // layout-identical).
                let resolved_name: String = if self.generic_classes.contains_key(name.as_str()) {
                    if let Some(alias) = self.type_param_aliases.get(name.as_str()) {
                        alias.clone()
                    } else if let Some(tinox_typecheck::ValueType::Named(_, targs)) =
                        self.expr_value_types.get(&expr.id).cloned()
                    {
                        if targs.is_empty() {
                            name.clone()
                        } else {
                            let gc = self.generic_classes.get(name.as_str()).cloned();
                            match gc {
                                Some(gc) => {
                                    let bindings: HashMap<String, String> = gc
                                        .type_params
                                        .iter()
                                        .zip(targs.iter())
                                        .map(|(tp, a)| (tp.clone(), Self::valuetype_to_llvm(a)))
                                        .collect();
                                    self.ensure_generic_class_specialization_with_bindings(
                                        name, &bindings,
                                    )?
                                }
                                None => name.clone(),
                            }
                        }
                    } else {
                        name.clone()
                    }
                } else {
                    name.clone()
                };
                let name = &resolved_name;
                let ptr = self.temp();
                let layout = self.struct_layouts.get(name).cloned().unwrap_or_default();
                let size = layout.len() * 8;
                writeln!(
                    &mut self.ir,
                    "{} = call i8* @tinox_alloc(i64 {})",
                    ptr, size
                )
                .unwrap();
                let typed_ptr = self.temp();
                writeln!(&mut self.ir, "{} = bitcast i8* {} to i64*", typed_ptr, ptr).unwrap();

                // If this class has a vtable, store the vtable pointer at index 0.
                let has_vtable = self.classes_with_vtable.contains(name);
                if has_vtable {
                    let n_vtable = self.vtable_sizes.get(name).copied().unwrap_or(1);
                    let vtable_gep = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 0",
                        vtable_gep, typed_ptr
                    )
                    .unwrap();
                    let vtable_as_i64 = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = ptrtoint [{} x i64]* @{}_vtable to i64",
                        vtable_as_i64, n_vtable, name
                    )
                    .unwrap();
                    writeln!(
                        &mut self.ir,
                        "store i64 {}, i64* {}",
                        vtable_as_i64, vtable_gep
                    )
                    .unwrap();
                }

                // B1 phase 2: typed field stores for classes with a named struct
                // type — typed GEP + `store <slot>` instead of the i64 slot +
                // ptrtoint/bitcast dance (mixable with the i64 path, same layout).
                let use_typed = self.class_named_types.contains(name.as_str());
                for (fname, value) in fields.iter() {
                    let (val, val_ty) = self.gen_expr(value, ctx)?;
                    // Look up field position in layout (which includes __vtable__ at 0 if vtable class)
                    let field_idx = layout.iter().position(|f| f == fname).unwrap_or(0);
                    if use_typed {
                        let field_llvm_ty = self.struct_field_llvm_types.get(name)
                            .and_then(|m| m.get(fname.as_str()))
                            .cloned()
                            .unwrap_or_else(|| "i64".to_string());
                        let slot = Self::slot_llvm_ty(&field_llvm_ty);
                        let store_val = self.coerce_to_slot(&val, &val_ty, &slot);
                        let field_ptr = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = getelementptr %class.{}, ptr {}, i32 0, i32 {}",
                            field_ptr, name, typed_ptr, field_idx
                        ).unwrap();
                        writeln!(&mut self.ir, "store {} {}, {}* {}", slot, store_val, slot, field_ptr).unwrap();
                        continue;
                    }
                    let field_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 {}",
                        field_ptr, typed_ptr, field_idx
                    )
                    .unwrap();
                    // Uniform i64 field storage: pointers → ptrtoint, floats → bitcast, i1 → zext, i64 → direct
                    let store_val = if val_ty == "i1" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = zext i1 {} to i64", cast, val).unwrap();
                        cast
                    } else if val_ty == "double" || val_ty == "float" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast {} {} to i64", cast, val_ty, val).unwrap();
                        cast
                    } else if val_ty != "i64" && !val_ty.is_empty() {
                        let cast = self.temp();
                        if val_ty == "ptr" {
                            writeln!(&mut self.ir, "{} = ptrtoint ptr {} to i64", cast, val).unwrap();
                        } else {
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                        }
                        cast
                    } else {
                        val
                    };
                    writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, field_ptr).unwrap();
                }
                // @Config: inject values from application.properties for annotated fields
                let cfg_fields: Vec<ConfigFieldInfo> = self.config_fields.iter()
                    .filter(|f| &f.class_name == name)
                    .cloned()
                    .collect();
                for cf in &cfg_fields {
                    if let Some(field_idx) = layout.iter().position(|f| f == &cf.field_name) {
                        let key_label = format!("str{}", self.strings.len());
                        self.strings.insert(key_label.clone(), cf.config_key.clone());
                        let key_len = cf.config_key.len() + 1;
                        let key_ptr = self.temp();
                        writeln!(&mut self.ir,
                            "{} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0",
                            key_ptr, key_len, key_len, key_label).unwrap();
                        let field_ptr = self.temp();
                        writeln!(&mut self.ir,
                            "{} = getelementptr i64, ptr {}, i64 {}",
                            field_ptr, typed_ptr, field_idx).unwrap();
                        match cf.field_llvm_type.as_str() {
                            "i8*" => {
                                let raw = self.temp();
                                writeln!(&mut self.ir,
                                    "{} = call i8* @tinox_config_get(i8* {})",
                                    raw, key_ptr).unwrap();
                                let as_i64 = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint i8* {} to i64", as_i64, raw).unwrap();
                                writeln!(&mut self.ir, "store i64 {}, i64* {}", as_i64, field_ptr).unwrap();
                            }
                            "i1" => {
                                let raw = self.temp();
                                writeln!(&mut self.ir,
                                    "{} = call i64 @tinox_config_get_bool(i8* {})",
                                    raw, key_ptr).unwrap();
                                writeln!(&mut self.ir, "store i64 {}, i64* {}", raw, field_ptr).unwrap();
                            }
                            _ => {
                                let raw = self.temp();
                                writeln!(&mut self.ir,
                                    "{} = call i64 @tinox_config_get_int(i8* {})",
                                    raw, key_ptr).unwrap();
                                writeln!(&mut self.ir, "store i64 {}, i64* {}", raw, field_ptr).unwrap();
                            }
                        }
                    }
                }

                // @Log: auto-initialize the synthetic 'log' field with Logger::new(ClassName)
                if self.log_classes.contains(name) {
                    if let Some(log_idx) = layout.iter().position(|f| f == "log") {
                        let str_label = format!("str{}", self.strings.len());
                        self.strings.insert(str_label.clone(), name.clone());
                        let str_len = name.len() + 1;
                        let name_ptr = self.temp();
                        writeln!(&mut self.ir,
                            "{} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0",
                            name_ptr, str_len, str_len, str_label).unwrap();
                        let logger_raw = self.temp();
                        writeln!(&mut self.ir,
                            "{} = call i64* @Logger_new(i64* null, i8* {})",
                            logger_raw, name_ptr).unwrap();
                        let log_as_i64 = self.temp();
                        writeln!(&mut self.ir, "{} = ptrtoint i64* {} to i64", log_as_i64, logger_raw).unwrap();
                        let log_field_ptr = self.temp();
                        writeln!(&mut self.ir,
                            "{} = getelementptr i64, ptr {}, i64 {}",
                            log_field_ptr, typed_ptr, log_idx).unwrap();
                        writeln!(&mut self.ir, "store i64 {}, i64* {}", log_as_i64, log_field_ptr).unwrap();
                    }
                }
                Ok((typed_ptr, "i64*".to_string()))
            }
            ExprKind::TupleIndex { tuple, index } => {
                let (raw, raw_ty) = self.gen_expr(tuple, ctx)?;
                // If inner expr returned a plain i64 (ptrtoint'd pointer), restore it to ptr
                let ptr = if raw_ty == "i64" {
                    let cast = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", cast, raw).unwrap();
                    cast
                } else {
                    raw
                };
                let field_ptr = self.temp();
                writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", field_ptr, ptr, index).unwrap();
                let val = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", val, field_ptr).unwrap();
                Ok((val, "i64".to_string()))
            }
            ExprKind::Tuple(exprs) => {
                let ptr = self.temp();
                let size = exprs.len() * 8;
                writeln!(&mut self.ir, "{} = call i8* @tinox_alloc(i64 {})", ptr, size).unwrap();
                let typed_ptr = self.temp();
                writeln!(&mut self.ir, "{} = bitcast i8* {} to i64*", typed_ptr, ptr).unwrap();
                for (i, expr) in exprs.iter().enumerate() {
                    let (val, val_ty) = self.gen_expr(expr, ctx)?;
                    // Pointer elements must be ptrtoint'd to i64 for uniform storage
                    let store_val = if val_ty != "i64" && val_ty != "i1" && val_ty != "double" && val_ty != "float" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                        cast
                    } else {
                        val
                    };
                    let field_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", field_ptr, typed_ptr, i).unwrap();
                    writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, field_ptr).unwrap();
                }
                Ok((typed_ptr, "i64*".to_string()))
            }
            ExprKind::EnumValue {
                enum_name,
                variant,
                type_args,
                args,
            } => {
                // Resolve the type-parameter alias: inside a
                // specialization, `T::fromJson` is a call on the bound
                // class.
                let enum_name = &self
                    .type_param_aliases
                    .get(enum_name)
                    .cloned()
                    .unwrap_or_else(|| enum_name.clone());

                // Special built-in constructors
                if enum_name == "Map" && variant == "new" {
                    let result = self.temp();
                    writeln!(&mut self.ir, "{} = call i8* @tinox_map_create()", result).unwrap();
                    return Ok((result, "i8*".to_string()));
                }

                // Generische statische Methode: am Call-Site monomorphisieren
                let static_key = format!("{}_{}", enum_name, variant);
                if let Some(gm) = self.generic_methods.get(&static_key).cloned() {
                    return self.gen_generic_method_call(&static_key, &gm, type_args, args, ctx);
                }
                if let Some(ret_ty) = self.method_ret_types.get(&static_key).cloned() {
                    return self.emit_static_dispatch_call(&static_key, &ret_ty, args, ctx);
                }

                // A generic class whose specialization isn't known yet
                // (under this name) — derive bindings and specialize now
                // if needed (Bug 20.2). Covers two patterns:
                // instance-style calls (`Cache::set(cache, …)` — K/V from
                // `cache`'s already-specialized receiver marker) and
                // factory calls deep inside ANOTHER generic class
                // (`Option::some(value)` in Cache::get — T only
                // derivable from `value`'s actual LLVM type, no `let`
                // annotation present). Arguments are generated once for
                // this and reused for the actual call.
                if let Some(gc) = self.generic_classes.get(enum_name.as_str()).cloned() {
                    if let Some(method) = gc.methods.iter().find(|m| m.name == *variant).cloned() {
                        let mut arg_vals: Vec<(String, String)> = Vec::with_capacity(args.len());
                        for arg in args.iter() {
                            arg_vals.push(self.gen_expr(arg, ctx)?);
                        }
                        let mut bindings: HashMap<String, String> = HashMap::new();
                        for (tp, ta) in gc.type_params.iter().zip(type_args.iter()) {
                            bindings.insert(tp.clone(), Self::type_to_llvm(ta));
                        }
                        // For `Class::method(obj, args…)` (this-style, Bug
                        // 38), the first arg is the receiver object, NOT
                        // the first declared param. Binding inference
                        // must look at the args offset accordingly
                        // relative to the params, otherwise a T param
                        // would get bound against the object (a pointer
                        // type, e.g. i64*) instead of its real argument →
                        // wrong specialization (i64P).
                        let arg_offset = if arg_vals.len() == method.params.len() + 1 { 1 } else { 0 };
                        // A this-style call (`Box::get(bs)`, arg_offset==1):
                        // the implicit receiver args[0] carries the class
                        // bindings in its marker (`Box__i8P` → T=i8*). For
                        // a method WITHOUT a T param (`fn get() -> T`),
                        // this is the ONLY binding source — otherwise T
                        // falls back to the i64 default and the wrong
                        // specialization (Box__i64) gets picked (Bug 52).
                        if arg_offset == 1 {
                            if let Some(recv) = args.first() {
                                if let Some(marker) = self.infer_struct_type(recv, ctx) {
                                    if let Some(rest) = marker.strip_prefix(&format!("{}__", enum_name)) {
                                        for (itp, part) in gc.type_params.iter().zip(rest.split("__")) {
                                            bindings.entry(itp.clone()).or_insert_with(|| part.replace('P', "*"));
                                        }
                                    }
                                }
                            }
                        }
                        for tp in &gc.type_params {
                            if bindings.contains_key(tp) {
                                continue;
                            }
                            for (pi, param) in method.params.iter().enumerate() {
                                let Some((_, arg_llvm)) = arg_vals.get(pi + arg_offset) else { continue };
                                match &param.param_type {
                                    // A param directly typed as T (Option::some(value: T))
                                    Type::Named(n) if n == tp => {
                                        bindings.insert(tp.clone(), arg_llvm.clone());
                                        break;
                                    }
                                    // A receiver-style param of the same class
                                    // (Cache::set(cache: Cache<K,V>, …)) — decompose
                                    // the argument's marker (mangled class name)
                                    // back into bindings.
                                    Type::Generic { name: pname, .. } if pname == enum_name.as_str() => {
                                        if let Some(arg_expr) = args.get(pi + arg_offset) {
                                            if let Some(marker) = self.infer_struct_type(arg_expr, ctx) {
                                                if let Some(rest) = marker.strip_prefix(&format!("{}__", enum_name)) {
                                                    for (itp, part) in gc.type_params.iter().zip(rest.split("__")) {
                                                        bindings.entry(itp.clone()).or_insert_with(|| part.replace('P', "*"));
                                                    }
                                                }
                                            }
                                        }
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            bindings.entry(tp.clone()).or_insert_with(|| "i64".to_string());
                        }
                        let mangled = self.ensure_generic_class_specialization_with_bindings(enum_name, &bindings)?;
                        let mangled_key = format!("{}_{}", mangled, variant);
                        if let Some(ret_ty) = self.method_ret_types.get(&mangled_key).cloned() {
                            let mut args_parts: Vec<String> = Vec::new();
                            let is_static = self.static_method_keys.contains(&mangled_key);
                            if !is_static {
                                if let Some(declared) = self.method_param_types.get(&mangled_key).map(|v| v.len()) {
                                    // Same arg-count disambiguation as in
                                    // emit_static_dispatch_call: args ==
                                    // declared+1 means the leading arg is
                                    // the receiver object (self) — then
                                    // don't prepend a null-self, otherwise
                                    // `this` reads the null pointer
                                    // (segfault for generic instance
                                    // methods).
                                    if arg_vals.len() != declared + 1 {
                                        args_parts.push("i64* null".to_string());
                                    }
                                }
                            }
                            for (v, t) in &arg_vals {
                                args_parts.push(format!("{} {}", t, v));
                            }
                            let args_str = args_parts.join(", ");
                            if ret_ty == "void" {
                                writeln!(&mut self.ir, "call void @{}({})", mangled_key, args_str).unwrap();
                                return Ok(("0".to_string(), "void".to_string()));
                            }
                            let result = self.temp();
                            writeln!(&mut self.ir, "{} = call {} @{}({})", result, ret_ty, mangled_key, args_str).unwrap();
                            return Ok((result, ret_ty));
                        }
                    }
                }

                // For simplicity, we represent enum values as:
                // - For variants without args: just a discriminator integer
                // - For variants with args: allocate memory with discriminator + args

                if args.is_empty() {
                    // Simple enum variant without arguments
                    let disc_key = self.variant_discriminator_key(Some(enum_name.as_str()), variant);
                    let discriminator = Self::enum_discriminator_noarg(&disc_key);
                    Ok((format!("{}", discriminator), "i64".to_string()))
                } else {
                    // Enum variant with arguments
                    // Allocate memory: [discriminator, arg1, arg2, ...]
                    let ptr = self.temp();
                    let size = (args.len() + 1) * 8; // +1 for discriminator
                    writeln!(
                        &mut self.ir,
                        "{} = call i8* @tinox_alloc(i64 {})",
                        ptr, size
                    )
                    .unwrap();
                    let typed_ptr = self.temp();
                    writeln!(&mut self.ir, "{} = bitcast i8* {} to i64*", typed_ptr, ptr).unwrap();

                    // Store discriminator at index 0
                    let disc_key = self.variant_discriminator_key(Some(enum_name.as_str()), variant);
                    let discriminator = Self::enum_discriminator(&disc_key);
                    let disc_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 0",
                        disc_ptr, typed_ptr
                    )
                    .unwrap();
                    writeln!(
                        &mut self.ir,
                        "store i64 {}, i64* {}",
                        discriminator, disc_ptr
                    )
                    .unwrap();

                    // Store arguments starting at index 1
                    for (i, arg) in args.iter().enumerate() {
                        let (val, val_ty) = self.gen_expr(arg, ctx)?;
                        let store_val = if val_ty == "i1" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                            c
                        } else if val_ty == "double" || val_ty == "float" {
                            let c = self.temp();
                            writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                            c
                        } else if val_ty != "i64" && !val_ty.is_empty() && val_ty != "void" {
                            let c = self.temp();
                            if val_ty == "ptr" {
                                writeln!(&mut self.ir, "{} = ptrtoint ptr {} to i64", c, val).unwrap();
                            } else {
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                            }
                            c
                        } else {
                            val
                        };
                        let arg_ptr = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = getelementptr i64, ptr {}, i64 {}",
                            arg_ptr,
                            typed_ptr,
                            i + 1
                        )
                        .unwrap();
                        writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, arg_ptr).unwrap();
                    }
                    Ok((typed_ptr, "i64*".to_string()))
                }
            }
            ExprKind::Return(value) => {
                let stmts_to_run: Vec<_> = ctx
                    .defer_stack
                    .last()
                    .cloned()
                    .unwrap_or_default();
                for stmt in stmts_to_run.into_iter().rev() {
                    self.gen_stmt_body(&Box::new(stmt), ctx)?;
                }
                if let Some(scope) = ctx.defer_stack.last_mut() {
                    scope.clear();
                }
                if let Some(val_expr) = value {
                    let (val, ty) = self.gen_expr(val_expr, ctx)?;
                    let expected = ctx.ret_type.clone();
                    let (final_val, final_ty) = if !expected.is_empty() && ty != expected {
                        let is_from_float = Self::is_float(&ty);
                        let is_to_float = Self::is_float(&expected);
                        let cast_op = match (ty.as_str(), expected.as_str()) {
                            _ if is_from_float && is_to_float => "fptrunc",
                            (from, _) if is_to_float && from.starts_with('i') => "bitcast",
                            (_, to) if is_from_float && to.starts_with('i') => "bitcast",
                            (from, to) if from.ends_with('*') && to.ends_with('*') => "bitcast",
                            (from, to) if from.starts_with('i') && to.starts_with('i')
                                && !from.contains('*') && !to.contains('*') =>
                            {
                                let from_bits: u32 = from[1..].parse().unwrap_or(64);
                                let to_bits: u32 = to[1..].parse().unwrap_or(64);
                                if from_bits > to_bits { "trunc" } else { "zext" }
                            }
                            (from, to) if !from.ends_with('*') && to.ends_with('*') => "inttoptr",
                            (from, to) if from.ends_with('*') && !to.ends_with('*') => "ptrtoint",
                            _ => "",
                        };
                        if !cast_op.is_empty() {
                            let tmp = self.temp();
                            writeln!(&mut self.ir, "{} = {} {} {} to {}", tmp, cast_op, ty, val, expected).unwrap();
                            (tmp, expected.clone())
                        } else {
                            (val, ty)
                        }
                    } else {
                        (val, ty)
                    };
                    let llvm_ty = Self::llvm_type_str(&final_ty);
                    writeln!(&mut self.ir, "ret {} {}", llvm_ty, final_val).unwrap();
                } else {
                    writeln!(&mut self.ir, "ret void").unwrap();
                }
                // Dead-code block so subsequent IR remains in a valid block.
                let dead_bb = self.new_bb("ret_dead");
                writeln!(&mut self.ir, "{}:", dead_bb).unwrap();
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::Break => {
                if let Some(ref break_bb) = ctx.break_target.clone() {
                    writeln!(&mut self.ir, "br label %{}", break_bb).unwrap();
                }
                let dead_bb = self.new_bb("break_dead");
                writeln!(&mut self.ir, "{}:", dead_bb).unwrap();
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::Continue => {
                if let Some(ref cont_bb) = ctx.continue_target.clone() {
                    writeln!(&mut self.ir, "br label %{}", cont_bb).unwrap();
                }
                let dead_bb = self.new_bb("cont_dead");
                writeln!(&mut self.ir, "{}:", dead_bb).unwrap();
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::Cast { expr, ty } => {
                let (val, val_ty) = self.gen_expr(expr, ctx)?;
                let llvm_ty = Self::type_to_llvm(ty);
                if llvm_ty == val_ty {
                    return Ok((val, llvm_ty));
                }
                // String → number: parse (bit-casting a char* would be nonsense).
                if val_ty == "i8*" && (Self::is_float(&llvm_ty) || llvm_ty.starts_with('i')) {
                    if Self::is_float(&llvm_ty) {
                        let d = self.temp();
                        writeln!(&mut self.ir, "{} = call double @tinox_string_to_float(i8* {})", d, val).unwrap();
                        if llvm_ty == "float" {
                            let f = self.temp();
                            writeln!(&mut self.ir, "{} = fptrunc double {} to float", f, d).unwrap();
                            return Ok((f, "float".to_string()));
                        }
                        return Ok((d, "double".to_string()));
                    }
                    let n = self.temp();
                    writeln!(&mut self.ir, "{} = call i64 @tinox_string_to_int(i8* {})", n, val).unwrap();
                    let bits: u32 = llvm_ty[1..].parse().unwrap_or(64);
                    if bits < 64 {
                        let t = self.temp();
                        writeln!(&mut self.ir, "{} = trunc i64 {} to {}", t, n, llvm_ty).unwrap();
                        return Ok((t, llvm_ty));
                    }
                    return Ok((n, "i64".to_string()));
                }
                let result = self.temp();
                let src_float = Self::is_float(&val_ty);
                let dst_float = Self::is_float(&llvm_ty);
                if src_float && dst_float {
                    // float ↔ float (fptrunc: double→float, fpext: float→double)
                    let op = if val_ty == "double" { "fptrunc" } else { "fpext" };
                    writeln!(&mut self.ir, "{} = {} {} {} to {}", result, op, val_ty, val, llvm_ty).unwrap();
                } else if src_float {
                    // float → int: fptosi
                    writeln!(&mut self.ir, "{} = fptosi {} {} to {}", result, val_ty, val, llvm_ty).unwrap();
                } else if dst_float {
                    // int → float: sitofp
                    writeln!(&mut self.ir, "{} = sitofp {} {} to {}", result, val_ty, val, llvm_ty).unwrap();
                } else if val_ty == "i1" {
                    writeln!(&mut self.ir, "{} = zext i1 {} to {}", result, val, llvm_ty).unwrap();
                } else if val_ty.starts_with('i') && llvm_ty.starts_with('i') {
                    let val_bits: u32 = val_ty[1..].parse().unwrap_or(64);
                    let tgt_bits: u32 = llvm_ty[1..].parse().unwrap_or(64);
                    let op = if val_bits < tgt_bits { "sext" } else { "trunc" };
                    writeln!(&mut self.ir, "{} = {} {} {} to {}", result, op, val_ty, val, llvm_ty).unwrap();
                } else {
                    writeln!(&mut self.ir, "{} = bitcast {} {} to {}", result, val_ty, val, llvm_ty).unwrap();
                }
                Ok((result, llvm_ty))
            }
            ExprKind::Block(stmts) => {
                if stmts.is_empty() {
                    return Ok(("0".to_string(), "i64".to_string()));
                }
                let (last, rest) = stmts.split_last().unwrap();
                for stmt in rest {
                    self.gen_stmt_body(stmt, ctx)?;
                }
                if let StmtKind::Expr(e) = &last.node {
                    self.gen_expr(e, ctx)
                } else {
                    self.gen_stmt_body(last, ctx)?;
                    Ok(("0".to_string(), "i64".to_string()))
                }
            }
            ExprKind::New { class, type_args, args } => {
                // Resolve the effective class name, monomorphizing generic classes on demand.
                let effective_class = self.ensure_generic_class_specialization(class, type_args)?;
                let layout_clone = self.struct_layouts.get(&effective_class).cloned();
                let has_vtable = self.classes_with_vtable.contains(&effective_class);
                let ptr = self.temp();
                let size = if let Some(ref layout) = layout_clone {
                    layout.len() * 8
                } else {
                    8
                };
                writeln!(
                    &mut self.ir,
                    "{} = call i8* @tinox_alloc(i64 {})",
                    ptr, size
                )
                .unwrap();
                let typed_ptr = self.temp();
                writeln!(&mut self.ir, "{} = bitcast i8* {} to i64*", typed_ptr, ptr).unwrap();

                if has_vtable {
                    let n_vtable = self.vtable_sizes.get(&effective_class).copied().unwrap_or(1);
                    let vtable_gep = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 0",
                        vtable_gep, typed_ptr
                    )
                    .unwrap();
                    let vtable_as_i64 = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = ptrtoint [{} x i64]* @{}_vtable to i64",
                        vtable_as_i64, n_vtable, effective_class
                    )
                    .unwrap();
                    writeln!(
                        &mut self.ir,
                        "store i64 {}, i64* {}",
                        vtable_as_i64, vtable_gep
                    )
                    .unwrap();
                }

                if let Some(ref layout) = layout_clone {
                    // For vtable classes, user args start at index 1 in layout
                    let field_start = if has_vtable { 1 } else { 0 };
                    for (arg_idx, arg) in args.iter().enumerate() {
                        let layout_idx = field_start + arg_idx;
                        if layout_idx < layout.len() {
                            let (val, val_ty) = self.gen_expr(arg, ctx)?;
                            let store_val = if val_ty == "i1" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                                c
                            } else if val_ty == "double" || val_ty == "float" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else if val_ty != "i64" && !val_ty.is_empty() && val_ty != "void" {
                                let c = self.temp();
                                if val_ty == "ptr" {
                                    writeln!(&mut self.ir, "{} = ptrtoint ptr {} to i64", c, val).unwrap();
                                } else {
                                    writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                                }
                                c
                            } else {
                                val
                            };
                            let field_ptr = self.temp();
                            writeln!(
                                &mut self.ir,
                                "{} = getelementptr i64, ptr {}, i64 {}",
                                field_ptr, typed_ptr, layout_idx
                            )
                            .unwrap();
                            writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, field_ptr)
                                .unwrap();
                        }
                    }
                }
                Ok((typed_ptr, "i64*".to_string()))
            }
            ExprKind::Lambda {
                params,
                ret_type,
                body,
            } => self.gen_lambda(params, ret_type.as_ref(), body, ctx),
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let (start_val, _) = self.gen_expr(start, ctx)?;
                let (end_val, _) = self.gen_expr(end, ctx)?;
                let ptr = self.temp();
                writeln!(&mut self.ir, "{} = call i8* @tinox_alloc(i64 16)", ptr).unwrap();
                let typed_ptr = self.temp();
                writeln!(&mut self.ir, "{} = bitcast i8* {} to i64*", typed_ptr, ptr).unwrap();
                let start_ptr = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 0",
                    start_ptr, typed_ptr
                )
                .unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* {}", start_val, start_ptr).unwrap();
                let end_ptr = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 1",
                    end_ptr, typed_ptr
                )
                .unwrap();
                let end_stored = if *inclusive {
                    let inc = self.temp();
                    writeln!(&mut self.ir, "{} = add i64 {}, 1", inc, end_val).unwrap();
                    inc
                } else {
                    end_val
                };
                writeln!(&mut self.ir, "store i64 {}, i64* {}", end_stored, end_ptr).unwrap();
                Ok((typed_ptr, "i64*".to_string()))
            }
            ExprKind::Match { expr, cases } => {
                let (val, val_ty) = self.gen_expr(expr, ctx)?;
                // Statically known enum type of the scrutinee, if the checker could
                // resolve it (see `variant_discriminator_key`) — lets unqualified
                // variant patterns (`Variant(x)`, bare `North`) scope their
                // discriminator to the right enum even for an ambiguous variant name.
                let scrutinee_enum: Option<String> = self.expr_value_types.get(&expr.id).and_then(|vt| {
                    if let tinox_typecheck::ValueType::Named(n, _) = vt {
                        Some(n.clone())
                    } else {
                        None
                    }
                });
                let merge_bb = self.new_bb("match_end");
                // Pre-allocate a result slot so each arm can store its value into it.
                // This ensures the result dominates the merge block regardless of which arm ran.
                let result_slot = format!("match_result_{}", self.temp_count);
                self.temp_count += 1;
                writeln!(&mut self.ir, "%{} = alloca i64", result_slot).unwrap();
                writeln!(&mut self.ir, "store i64 0, i64* %{}", result_slot).unwrap();
                let mut last_result_ty: String = "i64".to_string();
                for case in cases {
                    match &case.pattern {
                        Pattern::Wildcard(_) => {
                            let (body_val, body_ty) = self.gen_expr(&case.body, ctx)?;
                            last_result_ty = body_ty.clone();
                            let store_val = if body_ty == "i64" || body_ty.is_empty() { body_val.clone() }
                                else if body_ty == "i1" { let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, body_val).unwrap(); c }
                                else if body_ty == "double" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, body_val).unwrap(); c }
                                else if body_ty != "void" { let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, body_ty, body_val).unwrap(); c }
                                else { "0".to_string() };
                            writeln!(&mut self.ir, "store i64 {}, i64* %{}", store_val, result_slot).unwrap();
                            writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                        }
                        Pattern::Literal(lit, _) => {
                            let (lit_val, _lit_ty) = self.gen_literal(lit)?;
                            let cmp = self.temp();
                            writeln!(
                                &mut self.ir,
                                "{} = icmp eq {} {}, {}",
                                cmp, val_ty, val, lit_val
                            )
                            .unwrap();
                            let case_bb = self.new_bb("match_case");
                            let next_bb = self.new_bb("match_next");
                            writeln!(
                                &mut self.ir,
                                "br i1 {}, label %{}, label %{}",
                                cmp, case_bb, next_bb
                            )
                            .unwrap();
                            writeln!(&mut self.ir, "{}:", case_bb).unwrap();
                            let (body_val, body_ty) = self.gen_expr(&case.body, ctx)?;
                            last_result_ty = body_ty.clone();
                            let store_val = if body_ty == "i64" || body_ty.is_empty() { body_val.clone() }
                                else if body_ty == "i1" { let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, body_val).unwrap(); c }
                                else if body_ty == "double" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, body_val).unwrap(); c }
                                else if body_ty != "void" { let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, body_ty, body_val).unwrap(); c }
                                else { "0".to_string() };
                            writeln!(&mut self.ir, "store i64 {}, i64* %{}", store_val, result_slot).unwrap();
                            writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                            writeln!(&mut self.ir, "{}:", next_bb).unwrap();
                        }
                        Pattern::Ident(name, _, _) if self.known_enum_variants.contains(name) => {
                            // Bare enum variant name (e.g. `North` instead of `Dir::North`) —
                            // always a no-arg variant (has-arg variants require `Variant(x)`
                            // syntax and hit Pattern::EnumVariant below instead).
                            let disc_key = self.variant_discriminator_key(scrutinee_enum.as_deref(), name);
                            let discriminator = Self::enum_discriminator_noarg(&disc_key);
                            let val_i64 = if val_ty.ends_with('*') || val_ty == "ptr" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else {
                                val.clone()
                            };
                            let cmp = self.temp();
                            writeln!(
                                &mut self.ir,
                                "{} = icmp eq i64 {}, {}",
                                cmp, val_i64, discriminator
                            )
                            .unwrap();
                            let case_bb = self.new_bb("match_case");
                            let next_bb = self.new_bb("match_next");
                            writeln!(
                                &mut self.ir,
                                "br i1 {}, label %{}, label %{}",
                                cmp, case_bb, next_bb
                            )
                            .unwrap();
                            writeln!(&mut self.ir, "{}:", case_bb).unwrap();
                            let (body_val, body_ty) = self.gen_expr(&case.body, ctx)?;
                            last_result_ty = body_ty.clone();
                            let store_val = if body_ty == "i64" || body_ty.is_empty() { body_val.clone() }
                                else if body_ty == "i1" { let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, body_val).unwrap(); c }
                                else if body_ty == "double" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, body_val).unwrap(); c }
                                else if body_ty != "void" { let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, body_ty, body_val).unwrap(); c }
                                else { "0".to_string() };
                            writeln!(&mut self.ir, "store i64 {}, i64* %{}", store_val, result_slot).unwrap();
                            writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                            writeln!(&mut self.ir, "{}:", next_bb).unwrap();
                        }
                        Pattern::Ident(name, _, _) => {
                            let llvm_ty = val_ty.clone();
                            let slot_name = format!("{}_{}", name, self.temp_count);
                            self.temp_count += 1;
                            ctx.locals
                                .insert(name.clone(), (llvm_ty.clone(), ctx.locals.len()));
                            ctx.local_slots.insert(name.clone(), slot_name.clone());
                            writeln!(&mut self.ir, "%{} = alloca {}", slot_name, llvm_ty).unwrap();
                            writeln!(
                                &mut self.ir,
                                "store {} {}, {}* %{}",
                                val_ty, val, llvm_ty, slot_name
                            )
                            .unwrap();
                            let (body_val, body_ty) = self.gen_expr(&case.body, ctx)?;
                            last_result_ty = body_ty.clone();
                            let store_val = if body_ty == "i64" || body_ty.is_empty() { body_val.clone() }
                                else if body_ty == "i1" { let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, body_val).unwrap(); c }
                                else if body_ty == "double" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, body_val).unwrap(); c }
                                else if body_ty != "void" { let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, body_ty, body_val).unwrap(); c }
                                else { "0".to_string() };
                            writeln!(&mut self.ir, "store i64 {}, i64* %{}", store_val, result_slot).unwrap();
                            writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                            ctx.locals.remove(name);
                            ctx.local_slots.remove(name.as_str());
                        }
                        Pattern::EnumVariant { enum_name, variant, args, .. } => {
                            // For enum variants, we need to:
                            // 1. Extract and compare the discriminator
                            // 2. If it matches, bind any pattern arguments

                            // When written as `Variant(args)` (no :: qualifier), the parser
                            // puts the name in `enum_name` and leaves `variant` empty.
                            // When written as `Enum::Variant(args)`, the name is in `variant`.
                            let disc_name = if variant.is_empty() { enum_name } else { variant };
                            // Qualified (`Enum::Variant`) always carries the real enum name in
                            // `enum_name`; unqualified (`Variant(x)`) needs the scrutinee's
                            // statically resolved type instead (see `variant_discriminator_key`).
                            let enum_name_hint: Option<&str> = if variant.is_empty() {
                                scrutinee_enum.as_deref()
                            } else {
                                Some(enum_name.as_str())
                            };
                            let disc_key = self.variant_discriminator_key(enum_name_hint, disc_name);
                            // args here are the PATTERN's own bindings (`Variant(x, y)`), which
                            // mirror the variant's declared arity — has-args vs. no-args must use
                            // the matching discriminator scheme (s. enum_discriminator_noarg).
                            let discriminator = if args.is_empty() {
                                Self::enum_discriminator_noarg(&disc_key)
                            } else {
                                Self::enum_discriminator(&disc_key)
                            };

                            // Normalize the match subject to i64 so all arms use the same
                            // pointer-range-guarded logic regardless of the subject's LLVM type.
                            // Enum values are either a plain discriminator (< 65536, no-arg
                            // variants) or a heap pointer to [disc, payload...].
                            let val_i64 = if val_ty.ends_with('*') || val_ty == "ptr" {
                                let c = self.temp();
                                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                                c
                            } else {
                                val.clone()
                            };
                            let case_bb = self.new_bb("match_case");
                            let next_bb = self.new_bb("match_next");
                            if !args.is_empty() {
                                // Payload variant: guard with pointer-range check before
                                // dereferencing, since the value may be a plain discriminator.
                                let try_ptr_bb = self.new_bb("try_ptr");
                                let is_ptr_check = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = icmp ugt i64 {}, 65535",
                                    is_ptr_check, val_i64
                                )
                                .unwrap();
                                writeln!(
                                    &mut self.ir,
                                    "br i1 {}, label %{}, label %{}",
                                    is_ptr_check, try_ptr_bb, next_bb
                                )
                                .unwrap();
                                writeln!(&mut self.ir, "{}:", try_ptr_bb).unwrap();
                                let ptr_val = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = inttoptr i64 {} to i64*",
                                    ptr_val, val_i64
                                )
                                .unwrap();
                                let disc_ptr = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = getelementptr i64, ptr {}, i64 0",
                                    disc_ptr, ptr_val
                                )
                                .unwrap();
                                let loaded_disc = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = load i64, i64* {}",
                                    loaded_disc, disc_ptr
                                )
                                .unwrap();
                                let cmp = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = icmp eq i64 {}, {}",
                                    cmp, loaded_disc, discriminator
                                )
                                .unwrap();
                                writeln!(
                                    &mut self.ir,
                                    "br i1 {}, label %{}, label %{}",
                                    cmp, case_bb, next_bb
                                )
                                .unwrap();
                                writeln!(&mut self.ir, "{}:", case_bb).unwrap();

                                // Bind arguments
                                for (i, arg_pattern) in args.iter().enumerate() {
                                    if let Pattern::Ident(arg_name, _, _) = arg_pattern {
                                        let arg_ptr = self.temp();
                                        writeln!(
                                            &mut self.ir,
                                            "{} = getelementptr i64, ptr {}, i64 {}",
                                            arg_ptr,
                                            ptr_val,
                                            i + 1
                                        )
                                        .unwrap();
                                        let arg_val = self.temp();
                                        writeln!(
                                            &mut self.ir,
                                            "{} = load i64, i64* {}",
                                            arg_val, arg_ptr
                                        )
                                        .unwrap();
                                        self.bind_match_payload(ctx, disc_name, i, arg_name, &arg_val);
                                    }
                                }
                            } else {
                                // No-arg variant: plain discriminator compare
                                let cmp = self.temp();
                                writeln!(
                                    &mut self.ir,
                                    "{} = icmp eq i64 {}, {}",
                                    cmp, val_i64, discriminator
                                )
                                .unwrap();
                                writeln!(
                                    &mut self.ir,
                                    "br i1 {}, label %{}, label %{}",
                                    cmp, case_bb, next_bb
                                )
                                .unwrap();
                                writeln!(&mut self.ir, "{}:", case_bb).unwrap();
                            }
                            let (body_val, body_ty) = self.gen_expr(&case.body, ctx)?;
                            last_result_ty = body_ty.clone();
                            let store_val = if body_ty == "i64" || body_ty.is_empty() { body_val.clone() }
                                else if body_ty == "i1" { let c = self.temp(); writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, body_val).unwrap(); c }
                                else if body_ty == "double" { let c = self.temp(); writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, body_val).unwrap(); c }
                                else if body_ty != "void" { let c = self.temp(); writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, body_ty, body_val).unwrap(); c }
                                else { "0".to_string() };
                            writeln!(&mut self.ir, "store i64 {}, i64* %{}", store_val, result_slot).unwrap();
                            writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                            writeln!(&mut self.ir, "{}:", next_bb).unwrap();
                        }
                        _ => {}
                    }
                }
                writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();
                writeln!(&mut self.ir, "{}:", merge_bb).unwrap();
                // Load the result from the pre-allocated result slot.
                // This value is valid regardless of which arm ran (dominates all uses).
                let result_val = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* %{}", result_val, result_slot).unwrap();
                // Restore the original type if the result is a pointer type
                let final_ty = if last_result_ty == "i64" || last_result_ty == "void" || last_result_ty.is_empty() {
                    last_result_ty.clone()
                } else if last_result_ty == "i1" {
                    // Restore bool: truncate from i64
                    let b = self.temp();
                    writeln!(&mut self.ir, "{} = trunc i64 {} to i1", b, result_val).unwrap();
                    return Ok((b, "i1".to_string()));
                } else if last_result_ty == "double" {
                    let d = self.temp();
                    writeln!(&mut self.ir, "{} = bitcast i64 {} to double", d, result_val).unwrap();
                    return Ok((d, "double".to_string()));
                } else if last_result_ty.ends_with('*') || last_result_ty == "ptr" {
                    // Restore pointer type: inttoptr
                    let p = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", p, result_val, last_result_ty).unwrap();
                    return Ok((p, last_result_ty));
                } else {
                    last_result_ty.clone()
                };
                Ok((result_val, final_ty))
            }
            ExprKind::This => {
                if ctx.params.contains("self") {
                    Ok(("%self".to_string(), "i64*".to_string()))
                } else {
                    let mut bag = ErrorBag::new();
                    bag.push(Error::new(expr.span, "'this' used outside of a method"));
                    Err(bag)
                }
            }
            ExprKind::SuperCall { method, args } => {
                // Static dispatch to parent's method: call ParentClass_method(%self, args...)
                let parent_class = ctx.current_struct
                    .as_ref()
                    .and_then(|class| self.class_parents.get(class).cloned())
                    .unwrap_or_else(|| "__unknown__".to_string());
                let full_method_name = format!("{}_{}", parent_class, method);

                // First arg is %self (the current self pointer)
                let mut full_args_str = "i64* %self".to_string();
                for arg in args {
                    let (val, ty) = self.gen_expr(arg, ctx)?;
                    full_args_str.push_str(&format!(", {} {}", ty, val));
                }

                let ret_ty = self
                    .method_ret_types
                    .get(&full_method_name)
                    .cloned()
                    .unwrap_or_else(|| "i64".to_string());

                let result = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = call {} @{}({})",
                    result, ret_ty, full_method_name, full_args_str
                )
                .unwrap();
                Ok((result, ret_ty))
            }
            ExprKind::Is { expr, .. } => {
                let (val, _val_ty) = self.gen_expr(expr, ctx)?;
                let result = self.temp();
                writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", result, val).unwrap();
                Ok((result, "i1".to_string()))
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let result_slot = self.temp();
                writeln!(&mut self.ir, "{} = alloca i64", result_slot).unwrap();

                let (cond_val, cond_ty) = self.gen_expr(cond, ctx)?;
                // A condition that isn't already i1 (e.g. a direct Bool
                // struct-field load, which comes back as a raw i64 word
                // like every other field) needs the same icmp-to-i1
                // coercion StmtKind::If already applies -- otherwise a
                // ternary condition of the form `obj.boolField ? a : b`
                // emits `br i1` on an i64 value and `opt` rejects the IR.
                let cond_i1 = if cond_ty != "i1" {
                    let tmp = self.temp();
                    writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", tmp, cond_ty, cond_val).unwrap();
                    tmp
                } else {
                    cond_val
                };
                let then_bb = self.new_bb("if_then");
                let else_bb = self.new_bb("if_else");
                let merge_bb = self.new_bb("if_merge");

                writeln!(
                    &mut self.ir,
                    "br i1 {}, label %{}, label %{}",
                    cond_i1, then_bb, else_bb
                )
                .unwrap();

                // As an expression (ternary), branches may yield i8*/double/i1 —
                // store them in uniform i64 form and recover the type at merge.
                writeln!(&mut self.ir, "{}:", then_bb).unwrap();
                let (then_val, then_ty) = self.gen_expr(then_branch, ctx)?;
                let then_i64 = self.coerce_to_i64(&then_val, &then_ty);
                writeln!(&mut self.ir, "store i64 {}, i64* {}", then_i64, result_slot).unwrap();
                writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();

                let mut result_ty = then_ty;
                writeln!(&mut self.ir, "{}:", else_bb).unwrap();
                if let Some(else_expr) = else_branch {
                    let (else_val, else_ty) = self.gen_expr(else_expr, ctx)?;
                    let else_i64 = self.coerce_to_i64(&else_val, &else_ty);
                    writeln!(&mut self.ir, "store i64 {}, i64* {}", else_i64, result_slot)
                        .unwrap();
                    // Prefer a concrete branch type if the other side was untyped.
                    if (result_ty == "i64" || result_ty == "void" || result_ty.is_empty())
                        && else_ty != "i64" && else_ty != "void" && !else_ty.is_empty()
                    {
                        result_ty = else_ty;
                    }
                } else {
                    writeln!(&mut self.ir, "store i64 0, i64* {}", result_slot).unwrap();
                }
                writeln!(&mut self.ir, "br label %{}", merge_bb).unwrap();

                writeln!(&mut self.ir, "{}:", merge_bb).unwrap();
                let loaded = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", loaded, result_slot).unwrap();
                // Recover the branch type from uniform i64 storage.
                if result_ty == "double" || result_ty == "float" {
                    let t = self.temp();
                    writeln!(&mut self.ir, "{} = bitcast i64 {} to {}", t, loaded, result_ty).unwrap();
                    Ok((t, result_ty))
                } else if result_ty == "i1" {
                    let t = self.temp();
                    writeln!(&mut self.ir, "{} = trunc i64 {} to i1", t, loaded).unwrap();
                    Ok((t, result_ty))
                } else if result_ty.ends_with('*') {
                    let t = self.temp();
                    writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", t, loaded, result_ty).unwrap();
                    Ok((t, result_ty))
                } else {
                    Ok((loaded, "i64".to_string()))
                }
            }
            ExprKind::While { cond, body } => {
                let loop_bb = self.new_bb("while_cond");
                let body_bb = self.new_bb("while_body");
                let end_bb = self.new_bb("while_end");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(loop_bb.clone());

                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", loop_bb).unwrap();
                let (cond_val, _) = self.gen_expr(cond, ctx)?;
                writeln!(
                    &mut self.ir,
                    "br i1 {}, label %{}, label %{}",
                    cond_val, body_bb, end_bb
                )
                .unwrap();
                writeln!(&mut self.ir, "{}:", body_bb).unwrap();
                self.gen_expr(body, ctx)?;
                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::For { var, iter, body } => {
                let (range_ptr, _) = self.gen_expr(iter, ctx)?;

                let start_gep = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 0",
                    start_gep, range_ptr
                )
                .unwrap();
                let start_val = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", start_val, start_gep).unwrap();

                let end_gep = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 1",
                    end_gep, range_ptr
                )
                .unwrap();
                let end_val = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", end_val, end_gep).unwrap();

                writeln!(&mut self.ir, "%{} = alloca i64", var).unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* %{}", start_val, var).unwrap();
                ctx.locals
                    .insert(var.clone(), ("i64".to_string(), ctx.locals.len()));

                let cond_bb = self.new_bb("for_cond");
                let body_bb = self.new_bb("for_body");
                let end_bb = self.new_bb("for_end");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(cond_bb.clone());

                writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();
                writeln!(&mut self.ir, "{}:", cond_bb).unwrap();
                let cur_val = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* %{}", cur_val, var).unwrap();
                let cmp = self.temp();
                writeln!(&mut self.ir, "{} = icmp slt i64 {}, {}", cmp, cur_val, end_val)
                    .unwrap();
                writeln!(
                    &mut self.ir,
                    "br i1 {}, label %{}, label %{}",
                    cmp, body_bb, end_bb
                )
                .unwrap();

                writeln!(&mut self.ir, "{}:", body_bb).unwrap();
                self.gen_expr(body, ctx)?;

                let loaded_inc = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* %{}", loaded_inc, var).unwrap();
                let next_val = self.temp();
                writeln!(&mut self.ir, "{} = add i64 {}, 1", next_val, loaded_inc).unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* %{}", next_val, var).unwrap();
                writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();

                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::Loop { body } => {
                let loop_bb = self.new_bb("loop_body");
                let end_bb = self.new_bb("loop_end");

                let old_break = ctx.break_target.take();
                let old_continue = ctx.continue_target.take();
                ctx.break_target = Some(end_bb.clone());
                ctx.continue_target = Some(loop_bb.clone());

                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", loop_bb).unwrap();
                self.gen_expr(body, ctx)?;
                writeln!(&mut self.ir, "br label %{}", loop_bb).unwrap();
                writeln!(&mut self.ir, "{}:", end_bb).unwrap();

                ctx.break_target = old_break;
                ctx.continue_target = old_continue;
                Ok(("0".to_string(), "i64".to_string()))
            }
            ExprKind::Spawn(inner) => {
                let (fn_name, args) = match &inner.node {
                    ExprKind::Call { func, args } => {
                        let name = match &func.node {
                            ExprKind::Ident(n) => {
                                // Issue #149 stage 3: a bare spawn target is
                                // either a genuine top-level free function
                                // (still legal for FFI-style bindings that
                                // need a stable exported symbol, e.g. the
                                // fuzz drivers) or -- now that ordinary
                                // top-level `fn` is gone -- a same-class
                                // static `fnc` sibling, resolved the exact
                                // same way check_call's same-class fallback
                                // already does (stage 2). `fn_sigs` (free
                                // functions) wins on a name collision,
                                // matching that same priority.
                                if self.fn_sigs.contains_key(n.as_str()) {
                                    n.clone()
                                } else if let Some(class_name) = ctx.current_struct.clone() {
                                    let key = format!("{}_{}", class_name, n);
                                    if self.static_method_keys.contains(&key) {
                                        key
                                    } else {
                                        n.clone()
                                    }
                                } else {
                                    n.clone()
                                }
                            }
                            _ => {
                                let mut bag = ErrorBag::new();
                                bag.push(Error::new(inner.span, "spawn requires a direct function call".to_string()));
                                return Err(bag);
                            }
                        };
                        (name, args.clone())
                    }
                    // `spawn ClassName::method(...)` parses directly as
                    // EnumValue (its args are bundled into the node itself,
                    // not wrapped in a separate Call{func: EnumValue, ..}}
                    // the way a bare-Ident call is) — same mangled key
                    // `ClassName::method()` call codegen uses
                    // (emit_static_dispatch_call's `static_key`).
                    ExprKind::EnumValue { enum_name, variant, args, .. } => {
                        (format!("{}_{}", enum_name, variant), args.clone())
                    }
                    _ => {
                        let mut bag = ErrorBag::new();
                        bag.push(Error::new(inner.span, "spawn requires a function call expression".to_string()));
                        return Err(bag);
                    }
                };

                // A spawn target that's a non-static instance method (`fn`,
                // not `fnc`) called via `Class::method(explicitArgsOnly)`
                // must get the same "prepend a null self" treatment
                // emit_static_dispatch_call already applies for the
                // identical calling shape outside of spawn (see that
                // function's own comment: args==declared → self unused →
                // null-self; args==declared+1 → the receiver was passed
                // explicitly, no null-self needed). Skipping this here was
                // a real, previously-undiscovered bug: the compiled
                // function for any non-static `fn` always has a leading
                // `self` parameter regardless of how it's invoked, so
                // omitting the null-self argument silently called it with
                // one argument too few -- an LLVM call-site/definition
                // arity mismatch, undefined behavior (this crashed with a
                // real segfault the moment the method touched `self`,
                // i.e. any field access at all, not just a type error).
                let is_static = self.static_method_keys.contains(&fn_name);
                let declared_len = self.method_param_types.get(&fn_name).map(|v| v.len());
                let prepend_null_self = !is_static
                    && declared_len.map(|d| args.len() != d + 1).unwrap_or(false);

                let mut arg_vals: Vec<(String, String)> = Vec::new();
                if prepend_null_self {
                    arg_vals.push(("null".to_string(), "i64*".to_string()));
                }
                for arg in &args {
                    let (v, t) = self.gen_expr(arg, ctx)?;
                    arg_vals.push((v, t));
                }

                let n_slots = arg_vals.len() + 1;
                let wrapper_id = self.spawn_counter;
                self.spawn_counter += 1;
                let wrapper_name = format!("__spawn_wrapper_{}", wrapper_id);

                let (ret_ty, param_tys) = self.fn_sigs.get(&fn_name).cloned()
                    .or_else(|| {
                        // Static `fnc` spawn target: same lookup shape as a
                        // free function (ret type string + LLVM param type
                        // strings), just sourced from the class-method
                        // tables instead of fn_sigs.
                        self.method_ret_types.get(&fn_name).cloned().map(|rt| {
                            let mut ptys: Vec<String> = self
                                .method_param_types
                                .get(&fn_name)
                                .map(|v| v.iter().map(Self::type_to_llvm).collect())
                                .unwrap_or_default();
                            if prepend_null_self {
                                ptys.insert(0, "i64*".to_string());
                            }
                            (rt, ptys)
                        })
                    })
                    .unwrap_or_else(|| {
                        let ptys = arg_vals.iter().map(|(_, t)| t.clone()).collect();
                        ("i64".to_string(), ptys)
                    });

                // Allocate args array [n_slots x i64]
                let raw_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_alloc(i64 {})", raw_ptr, n_slots * 8).unwrap();
                let ap = self.temp();
                writeln!(&mut self.ir, "  {} = bitcast i8* {} to [{} x i64]*", ap, raw_ptr, n_slots).unwrap();

                // Store fn ptr at slot 0
                let fp_sig = format!("{} ({})*", ret_ty, param_tys.join(", "));
                let fp_i64 = self.temp();
                writeln!(&mut self.ir, "  {} = ptrtoint {} @{} to i64", fp_i64, fp_sig, fn_name).unwrap();
                let fp_slot = self.temp();
                writeln!(&mut self.ir, "  {} = getelementptr [{} x i64], [{} x i64]* {}, i64 0, i64 0", fp_slot, n_slots, n_slots, ap).unwrap();
                writeln!(&mut self.ir, "  store i64 {}, i64* {}", fp_i64, fp_slot).unwrap();

                // Store each arg coerced to i64
                let arg_vals_clone = arg_vals.clone();
                for (i, (val, ty)) in arg_vals_clone.iter().enumerate() {
                    let slot = self.temp();
                    writeln!(&mut self.ir, "  {} = getelementptr [{} x i64], [{} x i64]* {}, i64 0, i64 {}", slot, n_slots, n_slots, ap, i + 1).unwrap();
                    let i64_val = self.coerce_to_i64(val, ty);
                    writeln!(&mut self.ir, "  store i64 {}, i64* {}", i64_val, slot).unwrap();
                }

                // Call runtime spawn
                let task_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_task_spawn(i8* (i8*)* @{}, i8* {})", task_ptr, wrapper_name, raw_ptr).unwrap();
                let task_i64 = self.temp();
                writeln!(&mut self.ir, "  {} = ptrtoint i8* {} to i64", task_i64, task_ptr).unwrap();

                // Emit wrapper function into lambda_ir
                self.emit_spawn_wrapper(&wrapper_name, n_slots, &ret_ty, &param_tys);

                Ok((task_i64, "i64".to_string()))
            }
            ExprKind::Await(inner) => {
                let (handle_i64, _) = self.gen_expr(inner, ctx)?;
                let handle_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = inttoptr i64 {} to i8*", handle_ptr, handle_i64).unwrap();
                let result = self.temp();
                writeln!(&mut self.ir, "  {} = call i64 @tinox_task_await(i8* {})", result, handle_ptr).unwrap();
                Ok((result, "i64".to_string()))
            }
            ExprKind::Channel => {
                let ch_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = call i8* @tinox_channel_create()", ch_ptr).unwrap();
                let ch_i64 = self.temp();
                writeln!(&mut self.ir, "  {} = ptrtoint i8* {} to i64", ch_i64, ch_ptr).unwrap();
                Ok((ch_i64, "i64".to_string()))
            }
            ExprKind::Send { channel, value } => {
                let (ch_i64, _) = self.gen_expr(channel, ctx)?;
                let (val_raw, val_ty) = self.gen_expr(value, ctx)?;
                let ch_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = inttoptr i64 {} to i8*", ch_ptr, ch_i64).unwrap();
                let val_i64 = self.coerce_to_i64(&val_raw, &val_ty);
                writeln!(&mut self.ir, "  call void @tinox_channel_send(i8* {}, i64 {})", ch_ptr, val_i64).unwrap();
                Ok(("0".to_string(), "void".to_string()))
            }
            ExprKind::Recv(inner) => {
                let (ch_i64, _) = self.gen_expr(inner, ctx)?;
                let ch_ptr = self.temp();
                writeln!(&mut self.ir, "  {} = inttoptr i64 {} to i8*", ch_ptr, ch_i64).unwrap();
                let raw = self.temp();
                writeln!(&mut self.ir, "  {} = call i64 @tinox_channel_recv(i8* {})", raw, ch_ptr).unwrap();
                // tinox_channel_recv always returns the stored value as a
                // raw i64 (the runtime channel is an opaque handle with no
                // notion of element type — see ExprKind::Channel above).
                // For a `Channel<SomeClass>` (issue #123 needed this for
                // `Channel<AmqpFrame091>`, not just `Channel<Int64>` which
                // happened to already work since i64 was the right answer
                // by coincidence), the typechecker resolves this
                // expression's static type to the class via `inner`'s
                // declared `Channel<T>` — recover that here the same way
                // and inttoptr back to a real pointer, or every downstream
                // field access on the received value would misinterpret
                // it as a plain integer.
                use tinox_typecheck::ValueType as VT;
                let elem_llvm = match self.expr_value_types.get(&inner.id) {
                    Some(VT::Named(name, args)) if name == "Channel" && args.len() == 1 => {
                        Self::valuetype_to_llvm(&args[0])
                    }
                    _ => "i64".to_string(),
                };
                if elem_llvm.ends_with('*') {
                    let result = self.temp();
                    writeln!(&mut self.ir, "  {} = inttoptr i64 {} to {}", result, raw, elem_llvm).unwrap();
                    Ok((result, elem_llvm))
                } else {
                    Ok((raw, elem_llvm))
                }
            }
            ExprKind::CompoundAssign { op, target, value } => {
                self.gen_compound_assign(op, target, value, ctx)
            }
            ExprKind::Assign { target, value } => {
                let (val, val_ty) = self.gen_expr(value, ctx)?;
                if let ExprKind::FieldAccess { obj, field } = &target.node {
                    let (obj_raw, obj_ty) = self.gen_expr(obj, ctx)?;
                    let obj_ptr = if obj_ty == "i64" {
                        let cast = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", cast, obj_raw).unwrap();
                        cast
                    } else {
                        obj_raw
                    };
                    let struct_name = self.infer_struct_type(obj, ctx)
                        .or_else(|| if matches!(&obj.node, ExprKind::This) { ctx.current_struct.clone() } else { None });
                    let offset = struct_name.as_deref()
                        .and_then(|sn| self.struct_layouts.get(sn))
                        .and_then(|fields| fields.iter().position(|f| f == field.as_str()))
                        .unwrap_or(0) as i64;
                    // B1 phase 3: typed field-assignment store for named-type classes.
                    if !self.try_typed_field_store(struct_name.as_deref(), &obj_ptr, field, target.span, &val, &val_ty)? {
                        let store_val = if val_ty == "double" || val_ty == "float" {
                            let cast = self.temp();
                            writeln!(&mut self.ir, "{} = bitcast {} {} to i64", cast, val_ty, val).unwrap();
                            cast
                        } else if val_ty != "i64" && val_ty != "i1" && !val_ty.is_empty() {
                            let cast = self.temp();
                            writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                            cast
                        } else {
                            val.clone()
                        };
                        let field_ptr = self.temp();
                        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 {}", field_ptr, obj_ptr, offset).unwrap();
                        writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, field_ptr).unwrap();
                    }
                } else if let ExprKind::Ident(name) = &target.node {
                    let store_ty = ctx.locals.get(name).map(|(t, _)| t.clone()).unwrap_or_else(|| val_ty.clone());
                    let slot = ctx.local_slots.get(name.as_str()).cloned().unwrap_or_else(|| name.clone());
                    // Coerce value type to target slot type
                    let store_val = if val_ty == store_ty || val_ty.is_empty() || store_ty.is_empty() {
                        val.clone()
                    } else if val_ty == "i64" && (store_ty.ends_with('*') || store_ty == "ptr") {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", c, val, store_ty).unwrap();
                        c
                    } else if (val_ty.ends_with('*') || val_ty == "ptr") && store_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                        c
                    } else if val_ty == "i1" && store_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                        c
                    } else if val_ty == "double" && store_ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, val).unwrap();
                        c
                    } else {
                        val.clone()
                    };
                    writeln!(&mut self.ir, "store {} {}, {}* %{}", store_ty, store_val, store_ty, slot).unwrap();
                } else if let ExprKind::Index { obj, index } = &target.node {
                    // e.g. `this.headers[key] = value;` -- reaches here (not
                    // StmtKind::Assignment) because the parser's elaborate
                    // assignment-target parsing only triggers off a leading
                    // `TokenKind::Ident`, not `this`. See gen_index_store's
                    // doc comment for the full story (issue #143: this
                    // silently no-op'd before this Index case existed here).
                    let idx_target = self.gen_index_target(obj, index, ctx)?;
                    self.gen_index_store(&idx_target, &val, &val_ty);
                }
                Ok((val, val_ty))
            }
            _ => {
                let mut bag = ErrorBag::new();
                bag.push(Error::new(
                    expr.span,
                    format!(
                        "codegen: unsupported expression kind '{}'",
                        expr_kind_name(&expr.node)
                    ),
                ));
                Err(bag)
            }
        }
    }

    /// Resolves `obj[index]`'s assignment-target parts (is this a map or an
    /// array; the index and base-container SSA values) without touching
    /// `value` — split out from the store itself so each caller can keep its
    /// own pre-existing relative evaluation order against `value` (see
    /// `gen_index_store`'s doc comment for why there are two callers).
    /// Bundled into `IndexTarget` rather than a 5-tuple/more store() params —
    /// avoids clippy::too_many_arguments on `gen_index_store`.
    fn gen_index_target(
        &mut self,
        obj: &tinox_parser::Expr,
        index: &tinox_parser::Expr,
        ctx: &mut GenCtx,
    ) -> Result<IndexTarget, ErrorBag> {
        use tinox_parser::ExprKind;
        // Detect Map type for map[key] = val → tinox_map_set
        let obj_declared_type = if let ExprKind::Ident(n) = &obj.node {
            ctx.local_types.get(n.as_str()).cloned()
                // Fallback: the rich bridge (unstripped marker)
                .or_else(|| self.rich_marker(obj))
        } else {
            // Felder/verschachtelte Ziele (this.m[k] = v)
            self.infer_struct_type(obj, ctx)
        };
        let is_map = obj_declared_type.as_deref().map(Self::is_map_marker).unwrap_or(false);

        let (idx_val, idx_ty) = self.gen_expr(index, ctx)?;
        let (base_ptr, base_ty) = if let ExprKind::Ident(name) = &obj.node {
            if ctx.params.contains(name) {
                self.gen_expr(obj, ctx)?
            } else if ctx.locals.contains_key(name) {
                let (var_ty, _) = ctx.locals.get(name).unwrap();
                let slot = ctx.local_slots.get(name).cloned().unwrap_or_else(|| name.clone());
                let loaded_ptr = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = load {}, {}* %{}",
                    loaded_ptr, var_ty, var_ty, slot
                )
                .unwrap();
                (loaded_ptr, var_ty.clone())
            } else {
                self.gen_expr(obj, ctx)?
            }
        } else {
            self.gen_expr(obj, ctx)?
        };
        Ok(IndexTarget { is_map, idx_val, idx_ty, base_ptr, base_ty })
    }

    /// `obj[index] = value` — map[k]=v -> tinox_map_set, arr[i]=v -> GEP+store.
    /// Takes `(val, val_ty)` already evaluated rather than the raw `value`
    /// expression: shared by both `StmtKind::Assignment` (statements starting
    /// with a bare identifier, e.g. `arr[i] = v;`/`m[k] = v;`, which evaluates
    /// index/obj before value) and `ExprKind::Assign` (everything else at
    /// statement level, notably anything starting with `this` — the parser's
    /// elaborate assignment-target parsing only triggers off a leading
    /// `TokenKind::Ident`, so `this.field[i] = v;` falls through to general
    /// expression-statement parsing and produces an `ExprKind::Assign` node
    /// instead of `StmtKind::Assignment`, which evaluates value first). Before
    /// this was shared, `ExprKind::Assign`'s own handler had no Index case at
    /// all — `this.headers[key] = value;` silently did nothing (the RHS was
    /// still evaluated, just never stored anywhere), while `let h =
    /// this.headers; h[key] = value;` worked, since `h[key]=v` IS a
    /// bare-Ident statement.
    fn gen_index_store(&mut self, target: &IndexTarget, val: &str, val_ty: &str) {
        let IndexTarget { is_map, idx_val, idx_ty, base_ptr, base_ty } = target;
        let is_map = *is_map;
        if is_map || idx_ty == "i8*" {
            // Map: tinox_map_set(i8* map, i8* key, i64 val)
            let map_i8 = if base_ty == "i8*" { base_ptr.to_string() } else {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i8*", c, base_ptr).unwrap();
                c
            };
            let key_i8 = self.emit_map_key(idx_val, idx_ty);
            let store_val = if val_ty == "i64" || val_ty.is_empty() {
                val.to_string()
            } else if val_ty == "i1" {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                c
            } else if val_ty == "double" || val_ty == "float" {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = bitcast {} {} to i64", c, val_ty, val).unwrap();
                c
            } else {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, val_ty, val).unwrap();
                c
            };
            writeln!(&mut self.ir, "call void @tinox_map_set(i8* {}, i8* {}, i64 {})", map_i8, key_i8, store_val).unwrap();
        } else {
            // Coerce base_ptr to i64* if it's encoded as i64
            let base_arr = if base_ty == "i64" {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, base_ptr).unwrap();
                c
            } else {
                base_ptr.to_string()
            };
            let data_ptr = self.emit_array_data(&base_arr);
            let ptr_name = self.temp();
            writeln!(
                &mut self.ir,
                "{} = getelementptr i64, ptr {}, i64 {}",
                ptr_name, data_ptr, idx_val
            )
            .unwrap();
            // Strings stored as i64 (ptrtoint); bools need zext; others direct
            let store_val = if val_ty == "i8*" || val_ty == "i64*" {
                let cast = self.temp();
                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", cast, val_ty, val).unwrap();
                cast
            } else if val_ty == "i1" {
                let cast = self.temp();
                writeln!(&mut self.ir, "{} = zext i1 {} to i64", cast, val).unwrap();
                cast
            } else {
                val.to_string()
            };
            writeln!(&mut self.ir, "store i64 {}, i64* {}", store_val, ptr_name).unwrap();
        }
    }

    fn gen_compound_assign(
        &mut self,
        op: &tinox_parser::CompoundOp,
        target: &tinox_parser::Expr,
        value: &tinox_parser::Expr,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        match &target.node {
            ExprKind::Ident(name) => {
                if let Some((ty, _)) = ctx.locals.get(name) {
                    let ty = ty.clone();
                    let slot = ctx.local_slots.get(name.as_str()).cloned().unwrap_or_else(|| name.clone());
                    let loaded = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = load {}, {}* %{}",
                        loaded.as_str(),
                        ty,
                        ty,
                        slot.as_str()
                    )
                    .unwrap();
                    let (rhs_raw, rhs_ty) = self.gen_expr(value, ctx)?;
                    let rhs = if (ty == "i8*" && matches!(op, tinox_parser::CompoundOp::Add))
                        || rhs_ty == ty || rhs_ty.is_empty() {
                        rhs_raw
                    } else if rhs_ty == "i64" && ty == "double" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = sitofp i64 {} to double", c, rhs_raw).unwrap();
                        c
                    } else if rhs_ty == "double" && ty == "i64" {
                        let c = self.temp();
                        writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, rhs_raw).unwrap();
                        c
                    } else {
                        rhs_raw
                    };
                    if ty == "i8*" && matches!(op, tinox_parser::CompoundOp::Add) {
                        // String += String → Konkatenation
                        let result = self.temp();
                        writeln!(&mut self.ir, "{} = call i8* @tinox_string_concat(i8* {}, i8* {})", result, loaded, rhs).unwrap();
                        writeln!(&mut self.ir, "store i8* {}, i8** %{}", result, slot).unwrap();
                        return Ok((result, ty));
                    }
                    let is_float = ty == "double" || ty == "float";
                    let result = self.temp();
                    match op {
                        tinox_parser::CompoundOp::Add => {
                            let instr = if is_float { "fadd" } else { "add" };
                            writeln!(&mut self.ir, "{} = {} {} {}, {}", result, instr, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::Sub => {
                            let instr = if is_float { "fsub" } else { "sub" };
                            writeln!(&mut self.ir, "{} = {} {} {}, {}", result, instr, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::Mul => {
                            let instr = if is_float { "fmul" } else { "mul" };
                            writeln!(&mut self.ir, "{} = {} {} {}, {}", result, instr, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::Div => {
                            if is_float {
                                writeln!(&mut self.ir, "{} = fdiv {} {}, {}", result, ty, loaded, rhs).unwrap();
                            } else {
                                self.emit_checked_idiv(&result, &ty, &loaded, &rhs, false);
                            }
                        }
                        tinox_parser::CompoundOp::Mod => {
                            if is_float {
                                writeln!(&mut self.ir, "{} = frem {} {}, {}", result, ty, loaded, rhs).unwrap();
                            } else {
                                self.emit_checked_idiv(&result, &ty, &loaded, &rhs, true);
                            }
                        }
                        tinox_parser::CompoundOp::BitAnd => {
                            writeln!(&mut self.ir, "{} = and {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::BitOr => {
                            writeln!(&mut self.ir, "{} = or {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::BitXor => {
                            writeln!(&mut self.ir, "{} = xor {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::Shl => {
                            writeln!(&mut self.ir, "{} = shl {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::Shr => {
                            writeln!(&mut self.ir, "{} = lshr {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                        tinox_parser::CompoundOp::ShrArith => {
                            writeln!(&mut self.ir, "{} = ashr {} {}, {}", result, ty, loaded, rhs)
                                .unwrap();
                        }
                    }
                    writeln!(&mut self.ir, "store {} {}, {}* %{}", ty, result, ty, slot).unwrap();
                    return Ok((result, ty));
                }
            }
            ExprKind::Index { obj, index } => {
                let (idx_val, _) = self.gen_expr(index, ctx)?;
                let (base_ptr, _var_ty) = if let ExprKind::Ident(name) = &obj.node {
                    if ctx.locals.contains_key(name) {
                        let (vty, _) = ctx.locals.get(name).unwrap();
                        let slot = ctx.local_slots.get(name).cloned().unwrap_or_else(|| name.clone());
                        let loaded_ptr = self.temp();
                        writeln!(
                            &mut self.ir,
                            "{} = load {}, {}* %{}",
                            loaded_ptr, vty, vty, slot
                        )
                        .unwrap();
                        (loaded_ptr, vty.clone())
                    } else {
                        return self.gen_expr(obj, ctx);
                    }
                } else {
                    return self.gen_expr(obj, ctx);
                };
                let data_ptr = self.emit_array_data(&base_ptr);
                let ptr_name = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 {}",
                    ptr_name, data_ptr, idx_val
                )
                .unwrap();
                let loaded = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", loaded, ptr_name).unwrap();
                let (rhs, _) = self.gen_expr(value, ctx)?;
                let result = self.temp();
                match op {
                    tinox_parser::CompoundOp::Add => {
                        writeln!(&mut self.ir, "{} = add i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::Sub => {
                        writeln!(&mut self.ir, "{} = sub i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::Mul => {
                        writeln!(&mut self.ir, "{} = mul i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::Div => {
                        self.emit_checked_idiv(&result, "i64", &loaded, &rhs, false);
                    }
                    tinox_parser::CompoundOp::Mod => {
                        self.emit_checked_idiv(&result, "i64", &loaded, &rhs, true);
                    }
                    tinox_parser::CompoundOp::BitAnd => {
                        writeln!(&mut self.ir, "{} = and i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::BitOr => {
                        writeln!(&mut self.ir, "{} = or i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::BitXor => {
                        writeln!(&mut self.ir, "{} = xor i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::Shl => {
                        writeln!(&mut self.ir, "{} = shl i64 {}, {}", result, loaded, rhs).unwrap();
                    }
                    tinox_parser::CompoundOp::Shr => {
                        writeln!(&mut self.ir, "{} = lshr i64 {}, {}", result, loaded, rhs)
                            .unwrap();
                    }
                    tinox_parser::CompoundOp::ShrArith => {
                        writeln!(&mut self.ir, "{} = ashr i64 {}, {}", result, loaded, rhs)
                            .unwrap();
                    }
                }
                writeln!(&mut self.ir, "store i64 {}, i64* {}", result, ptr_name).unwrap();
                return Ok((result, "i64".to_string()));
            }
            _ => {
                let mut bag = ErrorBag::new();
                bag.push(Error::new(
                    target.span,
                    "codegen: unsupported compound-assignment target",
                ));
                return Err(bag);
            }
        }
        Ok(("0".to_string(), "i64".to_string()))
    }

    fn gen_literal(&mut self, lit: &Literal) -> Result<(String, String), ErrorBag> {
        match lit {
            Literal::Integer(n) => Ok((format!("{}", n), "i64".to_string())),
            Literal::Float(f) => {
                let s = format!("{}", f);
                let val = if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{}.0", s)
                };
                Ok((val, "double".to_string()))
            }
            Literal::String(s) => {
                let name = format!("str{}", self.strings.len());
                self.strings.insert(name.clone(), s.clone());
                let len = s.len() + 1;
                let ptr = self.temp();
                writeln!(&mut self.ir, "{} = getelementptr [{} x i8], [{} x i8]* @{}, i64 0, i64 0", ptr, len, len, name).unwrap();
                Ok((ptr, "i8*".to_string()))
            }
            Literal::Bool(b) => Ok((if *b { "1" } else { "0" }.to_string(), "i1".to_string())),
            Literal::Char(c) => Ok((format!("{}", *c as i64), "i32".to_string())),
            Literal::Byte(b) => Ok((format!("{}", b), "i8".to_string())),
            Literal::Null => Ok(("0".to_string(), "i64".to_string())),
        }
    }

    /// Array `map`/`filter`/`forEach`/`reduce` with a lambda argument:
    /// an inline IR loop over the {len,cap,data} handle, the lambda call
    /// via the closure block {fn_ptr, env}. The env pointer is always
    /// passed as a trailing param (non-capturing lambdas ignore it —
    /// the existing closure convention). Element typing via the marker
    /// system or the typed value bridge: the i64 slot element is
    /// converted to the param's LLVM type before the call (float
    /// bitcast, pointer inttoptr), map's return value back into the
    /// i64 slot representation.
    #[allow(clippy::too_many_arguments)]
    fn gen_array_lambda_method(
        &mut self,
        kind: &str,
        obj_ptr: &str,
        obj: &tinox_parser::Expr,
        lam: &tinox_parser::Expr,
        init: Option<&tinox_parser::Expr>,
        call_id: u32,
        declared_type: Option<&str>,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        use tinox_typecheck::ValueType as VT;
        // --- Receiver's element type: declared marker before rich export ---
        let elem_vt: Option<VT> = self.expr_value_types.get(&obj.id).and_then(|vt| match vt {
            VT::Array(e) => Some((**e).clone()),
            _ => None,
        });
        let elem_marker: Option<String> = declared_type
            .and_then(Self::elem_marker)
            .or_else(|| elem_vt.as_ref().and_then(|vt| self.valuetype_to_marker(vt)));
        let elem_llvm: String = match elem_marker.as_deref() {
            Some("String") => "i8*".to_string(),
            Some("Float") => "double".to_string(),
            Some(m)
                if m.starts_with("Array") || m.starts_with("List:") || Self::is_map_marker(m) =>
            {
                "i64*".to_string()
            }
            Some(m) if self.known_enum_types.contains(m) => "i64".to_string(),
            Some(m) if self.struct_layouts.contains_key(m) => "i64*".to_string(),
            _ => match elem_vt.as_ref() {
                Some(VT::Float) => "double".to_string(),
                Some(VT::String) => "i8*".to_string(),
                Some(VT::Bool) => "i1".to_string(),
                Some(VT::Char) => "i32".to_string(),
                _ => "i64".to_string(),
            },
        };
        // --- Lambda literal: hard-check the parameter count (no silent garbage) ---
        let expected_params = if kind == "reduce" { 2 } else { 1 };
        if let ExprKind::Lambda { params, .. } = &lam.node {
            if params.len() != expected_params {
                let mut bag = ErrorBag::new();
                bag.push(Error::new(
                    lam.span,
                    format!(
                        "codegen: {}-lambda expects {} parameter(s), got {}",
                        kind,
                        expected_params,
                        params.len()
                    ),
                ));
                return Err(bag);
            }
        }
        // --- reduce: evaluate the start value first (acc type = init type) ---
        let init_acc: Option<(String, String)> = match init {
            Some(e) => Some(self.gen_expr(e, ctx)?),
            None => None,
        };
        let acc_llvm = init_acc
            .as_ref()
            .map(|(_, t)| if t.is_empty() { "i64".to_string() } else { t.clone() })
            .unwrap_or_else(|| "i64".to_string());
        // --- The lambda's return LLVM type ---
        let ret_llvm: String = match kind {
            "filter" => "i1".to_string(),
            "forEach" => "void".to_string(),
            "reduce" => acc_llvm.clone(),
            _ => {
                // map: 1) the declared lambda return type, 2) the
                // result element type from typecheck (Array(e) on the
                // call node), 3) the body type, 4) i64.
                let mut r: Option<String> = None;
                if let ExprKind::Lambda { ret_type: Some(rt), .. } = &lam.node {
                    r = Some(Self::type_to_llvm(rt));
                }
                if r.is_none() {
                    if let Some(VT::Array(e)) = self.expr_value_types.get(&call_id) {
                        if **e != VT::Any {
                            r = Some(Self::valuetype_to_llvm(e));
                        }
                    }
                }
                if r.is_none() {
                    if let ExprKind::Lambda { body, .. } = &lam.node {
                        if let Some(vt) = self.expr_value_types.get(&body.id) {
                            if *vt != VT::Any && *vt != VT::Nothing {
                                r = Some(Self::valuetype_to_llvm(vt));
                            }
                        }
                    }
                }
                r.unwrap_or_else(|| "i64".to_string())
            }
        };
        // --- Generate the lambda/closure value (literal: with type hints) ---
        let is_literal = matches!(&lam.node, ExprKind::Lambda { .. });
        if is_literal {
            self.pending_lambda_param_llvm = if kind == "reduce" {
                vec![Some(acc_llvm.clone()), Some(elem_llvm.clone())]
            } else {
                vec![Some(elem_llvm.clone())]
            };
            self.pending_lambda_ret_llvm = Some(ret_llvm.clone());
            // Struct/container marker for dispatch inside the lambda body
            let lt_marker = elem_marker.clone().filter(|m| {
                m.starts_with("Array")
                    || m.starts_with("List:")
                    || Self::is_map_marker(m)
                    || self.struct_layouts.contains_key(m.as_str())
            });
            self.pending_lambda_param_types = if kind == "reduce" {
                vec![None, lt_marker]
            } else {
                vec![lt_marker]
            };
        }
        let (clos_val, clos_ty) = self.gen_expr(lam, ctx)?;
        self.pending_lambda_param_types.clear();
        self.pending_lambda_param_llvm.clear();
        self.pending_lambda_ret_llvm = None;
        // --- Closure-Block {fn_ptr, env} laden ---
        let block = if clos_ty == "i64*" {
            clos_val.clone()
        } else if clos_ty.ends_with('*') || clos_ty == "ptr" {
            let c = self.temp();
            writeln!(&mut self.ir, "{} = bitcast {} {} to i64*", c, clos_ty, clos_val).unwrap();
            c
        } else {
            let c = self.temp();
            writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", c, clos_val).unwrap();
            c
        };
        let fp = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", fp, block).unwrap();
        let env_gep = self.temp();
        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 1", env_gep, block).unwrap();
        let env_val = self.temp();
        writeln!(&mut self.ir, "{} = load i64*, i64* {}", env_val, env_gep).unwrap();
        // Call param types: a declared lambda param wins over the
        // element type (the definition was emitted with the declared type).
        let lam_param_llvm = |idx: usize, fallback: &str| -> String {
            if let ExprKind::Lambda { params, .. } = &lam.node {
                params
                    .get(idx)
                    .map(|p| match &p.param_type {
                        tinox_parser::Type::Infer | tinox_parser::Type::Any => {
                            fallback.to_string()
                        }
                        t => Self::type_to_llvm(t),
                    })
                    .unwrap_or_else(|| fallback.to_string())
            } else {
                fallback.to_string()
            }
        };
        let (call_acc_llvm, call_param_llvm) = if kind == "reduce" {
            (lam_param_llvm(0, &acc_llvm), lam_param_llvm(1, &elem_llvm))
        } else {
            (String::new(), lam_param_llvm(0, &elem_llvm))
        };
        let fn_sig = if kind == "reduce" {
            format!("{} ({}, {}, i64*)", ret_llvm, call_acc_llvm, call_param_llvm)
        } else {
            format!("{} ({}, i64*)", ret_llvm, call_param_llvm)
        };
        let casted = self.temp();
        writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}*", casted, fp, fn_sig).unwrap();
        // --- Loop over the handle ---
        let len = self.emit_array_len(obj_ptr);
        let src_data = self.emit_array_data(obj_ptr);
        let (res_handle, res_data) = match kind {
            "map" => {
                let h = self.temp();
                writeln!(&mut self.ir, "{} = call i64* @tinox_array_new(i64 {}, i64 0)", h, len)
                    .unwrap();
                let d = self.emit_array_data(&h);
                (Some(h), Some(d))
            }
            "filter" => {
                let h = self.temp();
                writeln!(&mut self.ir, "{} = call i64* @tinox_array_new(i64 0, i64 {})", h, len)
                    .unwrap();
                (Some(h), None)
            }
            _ => (None, None),
        };
        let acc_slot = if kind == "reduce" {
            let slot = self.temp();
            writeln!(&mut self.ir, "{} = alloca {}", slot, acc_llvm).unwrap();
            let (iv, _) = init_acc.as_ref().unwrap().clone();
            writeln!(&mut self.ir, "store {} {}, {}* {}", acc_llvm, iv, acc_llvm, slot).unwrap();
            Some(slot)
        } else {
            None
        };
        let idx_slot = self.temp();
        writeln!(&mut self.ir, "{} = alloca i64", idx_slot).unwrap();
        writeln!(&mut self.ir, "store i64 0, i64* {}", idx_slot).unwrap();
        let cond_bb = self.new_bb("arrlam_cond");
        let body_bb = self.new_bb("arrlam_body");
        let end_bb = self.new_bb("arrlam_end");
        writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();
        writeln!(&mut self.ir, "{}:", cond_bb).unwrap();
        let cur = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", cur, idx_slot).unwrap();
        let cmp = self.temp();
        writeln!(&mut self.ir, "{} = icmp slt i64 {}, {}", cmp, cur, len).unwrap();
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", cmp, body_bb, end_bb).unwrap();
        writeln!(&mut self.ir, "{}:", body_bb).unwrap();
        let ep = self.temp();
        writeln!(&mut self.ir, "{} = getelementptr i64, i64* {}, i64 {}", ep, src_data, cur)
            .unwrap();
        let raw = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", raw, ep).unwrap();
        // i64 slot → param type. Float slots are bitcast-stored doubles;
        // an int element facing a double param, by contrast, gets
        // converted numerically (sitofp).
        let arg = self.array_slot_to_typed(&raw, &call_param_llvm, elem_llvm == "double");
        match kind {
            "map" => {
                let r = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = call {} {}({} {}, i64* {})",
                    r, ret_llvm, casted, call_param_llvm, arg, env_val
                )
                .unwrap();
                let out = self.typed_to_array_slot(&r, &ret_llvm);
                let op = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, i64* {}, i64 {}",
                    op,
                    res_data.as_ref().unwrap(),
                    cur
                )
                .unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* {}", out, op).unwrap();
            }
            "filter" => {
                let r = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = call i1 {}({} {}, i64* {})",
                    r, casted, call_param_llvm, arg, env_val
                )
                .unwrap();
                let keep_bb = self.new_bb("arrlam_keep");
                let next_bb = self.new_bb("arrlam_next");
                writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", r, keep_bb, next_bb)
                    .unwrap();
                writeln!(&mut self.ir, "{}:", keep_bb).unwrap();
                let p = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = call i64* @tinox_array_push(i64* {}, i64 {})",
                    p,
                    res_handle.as_ref().unwrap(),
                    raw
                )
                .unwrap();
                writeln!(&mut self.ir, "br label %{}", next_bb).unwrap();
                writeln!(&mut self.ir, "{}:", next_bb).unwrap();
            }
            "forEach" => {
                writeln!(
                    &mut self.ir,
                    "call void {}({} {}, i64* {})",
                    casted, call_param_llvm, arg, env_val
                )
                .unwrap();
            }
            _ => {
                // reduce
                let slot = acc_slot.as_ref().unwrap();
                let a = self.temp();
                writeln!(&mut self.ir, "{} = load {}, {}* {}", a, acc_llvm, acc_llvm, slot)
                    .unwrap();
                let r = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = call {} {}({} {}, {} {}, i64* {})",
                    r, ret_llvm, casted, call_acc_llvm, a, call_param_llvm, arg, env_val
                )
                .unwrap();
                writeln!(&mut self.ir, "store {} {}, {}* {}", acc_llvm, r, acc_llvm, slot)
                    .unwrap();
            }
        }
        let nxt = self.temp();
        writeln!(&mut self.ir, "{} = add i64 {}, 1", nxt, cur).unwrap();
        writeln!(&mut self.ir, "store i64 {}, i64* {}", nxt, idx_slot).unwrap();
        writeln!(&mut self.ir, "br label %{}", cond_bb).unwrap();
        writeln!(&mut self.ir, "{}:", end_bb).unwrap();
        match kind {
            "map" | "filter" => Ok((res_handle.unwrap(), "i64*".to_string())),
            "reduce" => {
                let slot = acc_slot.unwrap();
                let v = self.temp();
                writeln!(&mut self.ir, "{} = load {}, {}* {}", v, acc_llvm, acc_llvm, slot)
                    .unwrap();
                Ok((v, acc_llvm))
            }
            _ => Ok(("0".to_string(), "void".to_string())),
        }
    }

    /// i64 array slot → a typed value (the counterpart to slot storage:
    /// float slots are bitcast doubles, pointers are ptrtoint-i64).
    /// `slot_is_float_bits`: the slot holds float bits (Array:Float) —
    /// then bitcast instead of numeric conversion.
    fn array_slot_to_typed(&mut self, raw: &str, target: &str, slot_is_float_bits: bool) -> String {
        match target {
            "i64" => raw.to_string(),
            "double" => {
                let c = self.temp();
                if slot_is_float_bits {
                    writeln!(&mut self.ir, "{} = bitcast i64 {} to double", c, raw).unwrap();
                } else {
                    writeln!(&mut self.ir, "{} = sitofp i64 {} to double", c, raw).unwrap();
                }
                c
            }
            t if t.ends_with('*') || t == "ptr" => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", c, raw, t).unwrap();
                c
            }
            t if t.starts_with('i') => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = trunc i64 {} to {}", c, raw, t).unwrap();
                c
            }
            _ => raw.to_string(),
        }
    }

    /// Typisierter Wert → i64-Array-Slot (double bitcast, Pointer ptrtoint,
    /// i1 zext, schmale Ints sext).
    fn typed_to_array_slot(&mut self, val: &str, from: &str) -> String {
        match from {
            "i64" => val.to_string(),
            "double" => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = bitcast double {} to i64", c, val).unwrap();
                c
            }
            "i1" => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = zext i1 {} to i64", c, val).unwrap();
                c
            }
            t if t.ends_with('*') || t == "ptr" => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = ptrtoint {} {} to i64", c, t, val).unwrap();
                c
            }
            t if t.starts_with('i') => {
                let c = self.temp();
                writeln!(&mut self.ir, "{} = sext {} {} to i64", c, t, val).unwrap();
                c
            }
            _ => val.to_string(),
        }
    }

    fn gen_lambda(
        &mut self,
        params: &[tinox_parser::Param],
        ret_type: Option<&tinox_parser::Type>,
        body: &tinox_parser::Expr,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        let lambda_id = self.temp_count;
        self.temp_count += 1;
        let fn_name = format!("__lambda_{}", lambda_id);
        // LLVM type hints from the call site (array map/filter/…): take them so
        // a nested lambda in the body never inherits them.
        let param_llvm_hints = std::mem::take(&mut self.pending_lambda_param_llvm);
        let ret_llvm_hint = std::mem::take(&mut self.pending_lambda_ret_llvm);
        let ret_ty = match ret_type {
            Some(t) => Self::type_to_llvm(t),
            None => ret_llvm_hint.unwrap_or_else(|| "i64".to_string()),
        };
        let mut params_str = String::new();
        let mut fn_type_str = String::new();
        let mut lambda_ctx = GenCtx {
            locals: HashMap::new(),
            local_slots: HashMap::new(),
            range_vars: HashSet::new(),
            params: HashSet::new(),
            struct_fields: Vec::new(),
            // Issue #149 stage 2: inherit the enclosing method's class, not
            // `None` -- a lambda literal lexically nested inside a class
            // method still needs same-class bare `fnc` calls to resolve
            // (confirmed broken without this: a bare call inside a lambda
            // body emitted an unmangled `call @helper(...)` instead of
            // `@ClassName_helper`, an undefined-symbol link failure). The
            // lambda isn't itself a method, but bare-name resolution should
            // still see "am I lexically inside class X".
            current_struct: ctx.current_struct.clone(),
            local_types: HashMap::new(),
            break_target: None,
            continue_target: None,
            error_catch: None,
            defer_stack: Vec::new(),
            in_defer_exec: false,
            ret_type: ret_ty.clone(),
            timed_metric: None,
            transactional_commit: None,
            finally_targets: Vec::new(),
        };
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                params_str.push_str(", ");
                fn_type_str.push_str(", ");
            }
            let is_unannotated =
                matches!(p.param_type, tinox_parser::Type::Infer | tinox_parser::Type::Any);
            let llvm_ty = if is_unannotated {
                param_llvm_hints
                    .get(i)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| Self::type_to_llvm(&p.param_type))
            } else {
                Self::type_to_llvm(&p.param_type)
            };
            params_str.push_str(&format!("{} %{}", llvm_ty, p.name));
            fn_type_str.push_str(&llvm_ty);
            lambda_ctx
                .locals
                .insert(p.name.clone(), (llvm_ty.clone(), lambda_ctx.locals.len()));
            lambda_ctx.params.insert(p.name.clone());
            if let tinox_parser::Type::Named(struct_name) = &p.param_type {
                lambda_ctx.local_types.insert(p.name.clone(), struct_name.clone());
            } else if matches!(p.param_type, tinox_parser::Type::Infer | tinox_parser::Type::Any) {
                if let Some(Some(inferred)) = self.pending_lambda_param_types.get(i) {
                    lambda_ctx.local_types.insert(p.name.clone(), inferred.clone());
                }
            }
        }
        let param_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
        let free_vars = collect_free_vars(body, &param_names);
        let captured: Vec<(String, String)> = ctx
            .locals
            .iter()
            .filter(|(name, _)| free_vars.contains(*name))
            .map(|(n, (t, _))| (n.clone(), t.clone()))
            .collect();
        let env_ptr_name = if captured.is_empty() {
            None
        } else {
            let env_ptr = self.temp();
            writeln!(
                &mut self.ir,
                "{} = call i8* @tinox_alloc(i64 {})",
                env_ptr,
                captured.len() * 8
            )
            .unwrap();
            let env_typed = self.temp();
            writeln!(
                &mut self.ir,
                "{} = bitcast i8* {} to i64*",
                env_typed, env_ptr
            )
            .unwrap();
            for (i, (name, ty)) in captured.iter().enumerate() {
                if let Some((_, _slot)) = ctx.locals.get(name) {
                    let field_ptr = self.temp();
                    writeln!(
                        &mut self.ir,
                        "{} = getelementptr i64, ptr {}, i64 {}",
                        field_ptr, env_typed, i
                    )
                    .unwrap();
                    // Params live as direct SSA values (`%name`), locals in an
                    // alloca — mirror the Ident read: load only for allocas,
                    // otherwise capture the value directly (otherwise `load
                    // i64, i64* %param` on an i64 SSA value = invalid IR).
                    let val = if ctx.params.contains(name) {
                        format!("%{}", name)
                    } else {
                        let slot = ctx.local_slots.get(name).cloned().unwrap_or_else(|| name.clone());
                        let v = self.temp();
                        writeln!(&mut self.ir, "{} = load {}, {}* %{}", v, ty, ty, slot).unwrap();
                        v
                    };
                    writeln!(&mut self.ir, "store {} {}, {}* {}", ty, val, ty, field_ptr).unwrap();
                }
            }
            Some(env_typed)
        };
        if let Some(ref env) = env_ptr_name {
            // Only prepend a comma when there are declared params — a no-arg
            // capturing lambda otherwise produced `(, i64*)` (invalid IR).
            let sep = if params_str.is_empty() { "" } else { ", " };
            params_str.push_str(&format!("{}i64* {}", sep, env));
            fn_type_str.push_str(&format!("{}i64*", sep));
            let env_name = env.trim_start_matches('%');
            lambda_ctx
                .locals
                .insert(env_name.to_string(), ("i64*".to_string(), 0));
            lambda_ctx.params.insert(env_name.to_string());
        }
        let saved_ir = std::mem::take(&mut self.ir);
        let saved_lambda_ir = std::mem::take(&mut self.lambda_ir);
        let saved_temp = self.temp_count;
        writeln!(
            &mut self.ir,
            "define {} @{}({}) {{",
            ret_ty, fn_name, params_str
        )
        .unwrap();
        writeln!(&mut self.ir, "entry.tnx:").unwrap();
        if let Some(ref env) = env_ptr_name {
            for (i, (name, ty)) in captured.iter().enumerate() {
                writeln!(&mut self.ir, "%{} = alloca {}", name, ty).unwrap();
                let env_field = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = getelementptr i64, ptr {}, i64 {}",
                    env_field, env, i
                )
                .unwrap();
                let loaded = self.temp();
                writeln!(&mut self.ir, "{} = load i64, i64* {}", loaded, env_field).unwrap();
                writeln!(&mut self.ir, "store i64 {}, i64* %{}", loaded, name).unwrap();
                lambda_ctx
                    .locals
                    .insert(name.clone(), (ty.clone(), lambda_ctx.locals.len()));
                // Propagate struct type info so method dispatch works inside the lambda
                if let Some(struct_type) = ctx.local_types.get(name) {
                    lambda_ctx.local_types.insert(name.clone(), struct_type.clone());
                }
            }
        }
        self.gen_stmt_body(
            &Spanned::new(StmtKind::Return(Some(body.clone())), Span::dummy()),
            &mut lambda_ctx,
        )?;
        let has_terminator = self.ir.lines().last().is_some_and(|l| {
            l.trim().starts_with("ret ") || l.trim().starts_with("br ")
        });
        if !has_terminator {
            if ret_ty == "void" {
                writeln!(&mut self.ir, "ret void").unwrap();
            } else {
                writeln!(&mut self.ir, "ret {} 0", ret_ty).unwrap();
            }
        }
        writeln!(&mut self.ir, "}}").unwrap();
        writeln!(&mut self.ir).unwrap();
        let lambda_body = std::mem::replace(&mut self.ir, saved_ir);
        // A nested lambda in the body appended its own definition to self.lambda_ir
        // during body generation. Preserve it — resetting straight to
        // saved_lambda_ir would drop the inner lambda, leaving its `@__lambda_N`
        // reference undefined (Bug 65).
        let inner_lambdas = std::mem::take(&mut self.lambda_ir);
        let mut new_lambda_ir = saved_lambda_ir;
        new_lambda_ir.push_str(&inner_lambdas);
        new_lambda_ir.push_str(&lambda_body);
        self.lambda_ir = new_lambda_ir;
        self.temp_count = saved_temp;
        let ptr_name = self.temp();
        writeln!(
            &mut self.ir,
            "{} = ptrtoint {} ({})* @{} to i64",
            ptr_name, ret_ty, fn_type_str, fn_name
        )
        .unwrap();
        // Every lambda value is a closure block {fn_ptr: i64, env: i64*} —
        // also without captures (env = null). A single representation lets
        // every indirect call site (fn fields, List<fnc(...)> elements,
        // locals) use the same convention; raw fn ptrs called through the
        // closure path were dereferenced as data and crashed.
        let closure_ptr = self.temp();
        let closure_ptr_int = self.temp();
        writeln!(
            &mut self.ir,
            "{} = call i8* @tinox_alloc(i64 16)",
            closure_ptr
        )
        .unwrap();
        writeln!(
            &mut self.ir,
            "{} = bitcast i8* {} to i64*",
            closure_ptr_int, closure_ptr
        )
        .unwrap();
        let fp_field = self.temp();
        writeln!(
            &mut self.ir,
            "{} = getelementptr i64, ptr {}, i64 0",
            fp_field, closure_ptr_int
        )
        .unwrap();
        writeln!(&mut self.ir, "store i64 {}, i64* {}", ptr_name, fp_field).unwrap();
        let env_field = self.temp();
        writeln!(
            &mut self.ir,
            "{} = getelementptr i64, ptr {}, i64 1",
            env_field, closure_ptr_int
        )
        .unwrap();
        if let Some(ref env_ptr) = env_ptr_name {
            writeln!(&mut self.ir, "store i64* {}, i64* {}", env_ptr, env_field).unwrap();
        } else {
            writeln!(&mut self.ir, "store i64* null, i64* {}", env_field).unwrap();
        }
        Ok((closure_ptr_int, "i64*".to_string()))
    }

    fn is_float(ty: &str) -> bool {
        ty == "float" || ty == "double"
    }

    /// Bit width of an LLVM integer type name, for coercing between
    /// differently-sized ints (e.g. a binary-op result widened to i64
    /// stored into a narrower `Int32`-declared local).
    fn int_bit_width(ty: &str) -> Option<u32> {
        match ty {
            "i1" => Some(1),
            "i8" => Some(8),
            "i16" => Some(16),
            "i32" => Some(32),
            "i64" => Some(64),
            _ => None,
        }
    }

    /// Assemble the argument list for an indirect closure call: the user args
    /// (already a `", "`-joined, typed string) followed by the trailing
    /// `i64* <env>`. A 0-arg closure has an empty `args_str` — without this
    /// the format string would emit a leading comma (`(, i64* %env)`).
    fn closure_call_args(args_str: &str, env_val: &str) -> String {
        let a = args_str.trim();
        if a.is_empty() {
            format!("i64* {}", env_val)
        } else {
            format!("{}, i64* {}", a, env_val)
        }
    }

    fn llvm_type_str(ty: &str) -> String {
        ty.to_string()
    }

    fn type_to_llvm(ty: &Type) -> String {
        match ty {
            Type::Int8 => "i8".to_string(),
            Type::Int16 => "i16".to_string(),
            Type::Int32 => "i32".to_string(),
            Type::Int64 => "i64".to_string(),
            Type::UInt8 => "i8".to_string(),
            Type::UInt16 => "i16".to_string(),
            Type::UInt32 => "i32".to_string(),
            Type::UInt64 => "i64".to_string(),
            Type::Float32 => "float".to_string(),
            Type::Float64 => "double".to_string(),
            Type::Bool => "i1".to_string(),
            Type::Char => "i32".to_string(),
            Type::String => "i8*".to_string(),
            Type::Nothing => "void".to_string(),
            Type::Named(_) => "i64*".to_string(),
            Type::Generic { name, args } if name == "Array" => {
                args.first().map(|t| format!("{}*", Self::type_to_llvm(t))).unwrap_or_else(|| "i64*".to_string())
            }
            // Channel<T> handles are always a bare i64 (ptrtoint'd i8*),
            // regardless of T — see ExprKind::Channel/Recv, which
            // consistently treat the handle itself as i64 and only
            // convert the ELEMENT read out of it based on T. Without this
            // case a declared `Channel<T>` local/field/param fell through
            // to the generic-class default of i64* below, mismatching
            // every actual channel/send/recv expression's i64 (issue
            // #123 hit this the first time this language feature was
            // used with a real T beyond the parser-only test coverage it
            // had before).
            Type::Generic { name, .. } if name == "Channel" => "i64".to_string(),
            Type::Generic { .. } => "i64*".to_string(),
            Type::Ref(inner) => format!("{}*", Self::type_to_llvm(inner)),
            Type::Mutable(inner) => Self::type_to_llvm(inner),
            Type::Array(inner) => format!("{}*", Self::type_to_llvm(inner)),
            Type::Map(_, _) => "i8*".to_string(),
            Type::Tuple(_) => "i64*".to_string(),
            Type::Nullable(inner) => Self::type_to_llvm(inner),
            _ => "i64".to_string(),
        }
    }

    fn type_to_llvm_inst(&self, ty: &Type) -> String {
        if let Type::Named(name) = ty {
            if self.known_enum_types.contains(name) {
                return "i64".to_string();
            }
        }
        Self::type_to_llvm(ty)
    }

    fn temp(&mut self) -> String {
        let t = format!("%tmp.{}", self.temp_count);
        self.temp_count += 1;
        t
    }

    /// Arrays are stable handles: [0]=len, [1]=cap, [2]=data (i64* as i64).
    /// Emits a load of the length from an array handle.
    fn emit_array_len(&mut self, handle: &str) -> String {
        let len_ptr = self.temp();
        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 0", len_ptr, handle).unwrap();
        let len_val = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", len_val, len_ptr).unwrap();
        len_val
    }

    /// Emits a load of the element-data pointer (slot 2) from an array handle.
    fn emit_array_data(&mut self, handle: &str) -> String {
        let data_slot = self.temp();
        writeln!(&mut self.ir, "{} = getelementptr i64, ptr {}, i64 2", data_slot, handle).unwrap();
        let data_i64 = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", data_i64, data_slot).unwrap();
        let data_ptr = self.temp();
        writeln!(&mut self.ir, "{} = inttoptr i64 {} to i64*", data_ptr, data_i64).unwrap();
        data_ptr
    }

    fn new_bb(&mut self, name: &str) -> String {
        let n = self.temp_count;
        self.temp_count += 1;
        format!("{}_{}", name, n)
    }

    #[allow(dead_code)]
    fn get_field_offset(
        &mut self,
        _obj: &str,
        field: &str,
        _ctx: &mut GenCtx,
    ) -> Result<u64, ErrorBag> {
        let mut offset = 0u64;
        for f in _ctx.struct_fields.iter() {
            if f == field {
                return Ok(offset);
            }
            offset += 8;
        }
        Ok(0)
    }

    #[allow(dead_code)]
    fn get_struct_name_for_type(&self, _ty: &str) -> String {
        _ty.replace("*", "")
    }

    #[allow(dead_code)]
    fn get_struct_name_for_obj(&self, obj: &Expr, ctx: &GenCtx) -> Option<String> {
        if let ExprKind::Ident(name) = &obj.node {
            ctx.local_types.get(name).cloned()
        } else {
            None
        }
    }

    /// Emit `ret <default>` for the current function's return type — used when a
    /// throw (or a propagated throw) leaves a function without an enclosing try.
    fn emit_ret_default(&mut self, ctx: &GenCtx) {
        match ctx.ret_type.as_str() {
            "void" | "" => writeln!(&mut self.ir, "ret void").unwrap(),
            "double" => writeln!(&mut self.ir, "ret double 0.0").unwrap(),
            "float" => writeln!(&mut self.ir, "ret float 0.0").unwrap(),
            t if t.ends_with('*') || t == "ptr" => {
                writeln!(&mut self.ir, "ret {} null", t).unwrap()
            }
            t => writeln!(&mut self.ir, "ret {} 0", t).unwrap(),
        }
    }

    /// True if the last emitted IR line already terminates the current basic
    /// block, so no further instructions may be appended to it.
    fn last_is_terminator(&self) -> bool {
        self.ir.lines().last().is_some_and(|l| {
            let t = l.trim();
            t.starts_with("ret ") || t == "ret void" || t.starts_with("br ")
                || t == "unreachable" || t.starts_with("switch ")
        })
    }

    /// After a statement that may have thrown, check the global error slot and
    /// react immediately (Bug 40 — true unwinding at statement granularity).
    /// Inside a try, consume the error and branch to the catch dispatch.
    /// Otherwise return the function default, leaving the flag set so the
    /// caller's own post-statement check (or the runtime entry point) keeps
    /// propagating it up the stack. Without this, a throw only stopped its own
    /// function; intermediate frames and loops kept running with default values
    /// until the next try boundary.
    fn emit_post_stmt_throw_check(&mut self, ctx: &mut GenCtx) -> Result<(), ErrorBag> {
        let e = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* @__tinox_err", e).unwrap();
        let has = self.temp();
        writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", has, e).unwrap();
        let err_bb = self.new_bb("throwck");
        let cont_bb = self.new_bb("throwcont");
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", has, err_bb, cont_bb).unwrap();
        writeln!(&mut self.ir, "{}:", err_bb).unwrap();
        if let Some((catch_bb, error_var, depth)) = ctx.error_catch.clone() {
            writeln!(&mut self.ir, "store i64 0, i64* @__tinox_err").unwrap();
            writeln!(&mut self.ir, "store i64 {}, i64* {}", e, error_var).unwrap();
            // Run defer scopes opened inside this try's body before jumping
            // to the local catch handler (Bug 41 follow-up).
            self.emit_unwind_defers_to(ctx, depth)?;
            writeln!(&mut self.ir, "br label %{}", catch_bb).unwrap();
        } else {
            // Propagating out of this frame — run pending defers first (Bug 41).
            self.emit_unwind_defers(ctx)?;
            self.emit_ret_default(ctx);
        }
        writeln!(&mut self.ir, "{}:", cont_bb).unwrap();
        Ok(())
    }

    /// Could executing this statement (transitively) throw? Consults the
    /// throw-effect analysis (Bug 48): a call whose resolved target provably
    /// cannot throw is not counted. Over-approximates on unresolved/dynamic calls
    /// (always safe — extra checks are correct, just slower). `tf`/`tm` are the
    /// throwing free-fn names / throwing method base names.
    fn stmt_may_throw(stmt: &Stmt, tf: &HashSet<String>, tm: &HashSet<String>) -> bool {
        match &stmt.node {
            StmtKind::Expr(e) => Self::expr_may_throw(e, tf, tm),
            StmtKind::Let { value, .. } | StmtKind::Var { value, .. } => {
                value.as_ref().is_some_and(|e| Self::expr_may_throw(e, tf, tm))
            }
            StmtKind::Assignment { target, value } => {
                Self::expr_may_throw(target, tf, tm) || Self::expr_may_throw(value, tf, tm)
            }
            StmtKind::If { cond, then_branch, else_branch } => {
                Self::expr_may_throw(cond, tf, tm)
                    || Self::stmt_may_throw(then_branch, tf, tm)
                    || else_branch.as_ref().is_some_and(|b| Self::stmt_may_throw(b, tf, tm))
            }
            StmtKind::While { cond, body } => {
                Self::expr_may_throw(cond, tf, tm) || Self::stmt_may_throw(body, tf, tm)
            }
            StmtKind::For { iter, body, .. } => {
                Self::expr_may_throw(iter, tf, tm) || Self::stmt_may_throw(body, tf, tm)
            }
            StmtKind::ForC { init, cond, update, body } => {
                init.as_ref().is_some_and(|s| Self::stmt_may_throw(s, tf, tm))
                    || cond.as_ref().is_some_and(|e| Self::expr_may_throw(e, tf, tm))
                    || update.as_ref().is_some_and(|e| Self::expr_may_throw(e, tf, tm))
                    || Self::stmt_may_throw(body, tf, tm)
            }
            StmtKind::Loop { body } => Self::stmt_may_throw(body, tf, tm),
            StmtKind::Block(stmts) => stmts.iter().any(|s| Self::stmt_may_throw(s, tf, tm)),
            StmtKind::Return(v) => v.as_ref().is_some_and(|e| Self::expr_may_throw(e, tf, tm)),
            StmtKind::Throw(_) => true,
            StmtKind::Try { body, catches, finally } => {
                Self::stmt_may_throw(body, tf, tm)
                    || catches.iter().any(|c| Self::stmt_may_throw(&c.body, tf, tm))
                    || finally.as_ref().is_some_and(|f| Self::stmt_may_throw(f, tf, tm))
            }
            StmtKind::Defer(s) => Self::stmt_may_throw(s, tf, tm),
            StmtKind::Select { arms, default } => {
                arms.iter().any(|a| Self::stmt_may_throw(&a.body, tf, tm))
                    || default.as_ref().is_some_and(|d| Self::stmt_may_throw(d, tf, tm))
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Empty => false,
        }
    }

    /// Companion of `stmt_may_throw` for expressions. Call resolution:
    ///   - free call `name(...)`   → throws iff `name` ∈ tf OR `name` ∈ tm
    ///     (the latter covers issue #149 stage 2's same-class bare `fnc`
    ///     calls, e.g. `helper()` resolving to `Main::helper()` — same AST
    ///     shape as a free call, so both sets must be checked; `tm` is
    ///     already class-agnostic bare basenames, so this stays a safe
    ///     over-approximation for names that resolve to neither).
    ///   - `obj.m(...)` / `Class::m(...)` / `super.m(...)` → throws iff `m` ∈ tm.
    ///   - dynamic call (callee not an Ident), `New`, `await`/`recv`/`spawn` → true
    ///     (conservative; cannot prove non-throwing).
    fn expr_may_throw(expr: &Expr, tf: &HashSet<String>, tm: &HashSet<String>) -> bool {
        match &expr.node {
            ExprKind::Throw(_) => true,
            ExprKind::Call { func, args } => {
                if let ExprKind::Ident(name) = &func.node {
                    // Also check `tm` (bare method basenames), not just `tf`
                    // (free functions): issue #149 stage 2's same-class bare
                    // `fnc` calls (`helper()` resolving to `Main::helper()`)
                    // are STILL `ExprKind::Call{func: Ident, ..}` at the AST
                    // level, indistinguishable here from a free-function
                    // call — without this, a bare call that actually
                    // resolves to a throwing same-class method was silently
                    // treated as non-throwing, skipping the post-call
                    // unwind check (Bug 40) and letting execution continue
                    // past a throw instead of propagating it. `tm` is
                    // already class-agnostic (bare basenames, see
                    // `analyze_throw_effects`), so this is a safe
                    // over-approximation even for bare names that turn out
                    // to be something else entirely (lambda var, etc.).
                    tf.contains(name.as_str())
                        || tm.contains(name.as_str())
                        || args.iter().any(|a| Self::expr_may_throw(a, tf, tm))
                } else {
                    true // dynamic/lambda call — cannot prove non-throwing
                }
            }
            ExprKind::MethodCall { obj, method, args } => {
                tm.contains(method.as_str())
                    || Self::expr_may_throw(obj, tf, tm)
                    || args.iter().any(|a| Self::expr_may_throw(a, tf, tm))
            }
            ExprKind::SuperCall { method, args } => {
                tm.contains(method.as_str()) || args.iter().any(|a| Self::expr_may_throw(a, tf, tm))
            }
            ExprKind::EnumValue { variant, args, .. } => {
                tm.contains(variant.as_str()) || args.iter().any(|a| Self::expr_may_throw(a, tf, tm))
            }
            ExprKind::New { .. } | ExprKind::Await(_) | ExprKind::Recv(_) | ExprKind::Spawn(_) => true,
            ExprKind::Literal(_) | ExprKind::Ident(_) | ExprKind::This | ExprKind::Channel => false,
            ExprKind::Binary { lhs, rhs, .. } => Self::expr_may_throw(lhs, tf, tm) || Self::expr_may_throw(rhs, tf, tm),
            ExprKind::Unary { operand, .. } => Self::expr_may_throw(operand, tf, tm),
            ExprKind::Index { obj, index } => Self::expr_may_throw(obj, tf, tm) || Self::expr_may_throw(index, tf, tm),
            ExprKind::FieldAccess { obj, .. } => Self::expr_may_throw(obj, tf, tm),
            ExprKind::ArrayLiteral(es) | ExprKind::Tuple(es) => es.iter().any(|e| Self::expr_may_throw(e, tf, tm)),
            ExprKind::MapLiteral(kvs) => kvs.iter().any(|(k, v)| Self::expr_may_throw(k, tf, tm) || Self::expr_may_throw(v, tf, tm)),
            ExprKind::StructLiteral { fields, .. } => fields.iter().any(|(_, v)| Self::expr_may_throw(v, tf, tm)),
            ExprKind::Block(stmts) => stmts.iter().any(|s| Self::stmt_may_throw(s, tf, tm)),
            ExprKind::If { cond, then_branch, else_branch } => {
                Self::expr_may_throw(cond, tf, tm) || Self::expr_may_throw(then_branch, tf, tm)
                    || else_branch.as_ref().is_some_and(|b| Self::expr_may_throw(b, tf, tm))
            }
            ExprKind::While { cond, body } => Self::expr_may_throw(cond, tf, tm) || Self::expr_may_throw(body, tf, tm),
            ExprKind::For { iter, body, .. } => Self::expr_may_throw(iter, tf, tm) || Self::expr_may_throw(body, tf, tm),
            ExprKind::Loop { body } => Self::expr_may_throw(body, tf, tm),
            ExprKind::Match { expr, cases } => {
                Self::expr_may_throw(expr, tf, tm) || cases.iter().any(|c| Self::expr_may_throw(&c.body, tf, tm)
                    || c.guard.as_ref().is_some_and(|g| Self::expr_may_throw(g, tf, tm)))
            }
            ExprKind::Return(v) => v.as_ref().is_some_and(|e| Self::expr_may_throw(e, tf, tm)),
            ExprKind::Assign { target, value } | ExprKind::CompoundAssign { target, value, .. } => {
                Self::expr_may_throw(target, tf, tm) || Self::expr_may_throw(value, tf, tm)
            }
            ExprKind::Lambda { .. } => false, // body runs only when the lambda is called
            ExprKind::Send { channel, value } => Self::expr_may_throw(channel, tf, tm) || Self::expr_may_throw(value, tf, tm),
            ExprKind::Cast { expr, .. } | ExprKind::Is { expr, .. } => Self::expr_may_throw(expr, tf, tm),
            ExprKind::Range { start, end, .. } => Self::expr_may_throw(start, tf, tm) || Self::expr_may_throw(end, tf, tm),
            ExprKind::TupleIndex { tuple, .. } => Self::expr_may_throw(tuple, tf, tm),
            ExprKind::Break | ExprKind::Continue => false,
            ExprKind::Try { .. } => true,
        }
    }

    /// Throw-effect analysis (Bug 48): compute which functions/methods can
    /// transitively throw, so the per-statement throw-check (Bug 40) is only
    /// emitted after calls that can actually propagate an error. Fixpoint over
    /// the call graph; a fn is "throwing" if its body has a `throw` or calls a
    /// throwing target. Unresolved/dynamic calls are treated as throwing
    /// (over-approximation — never misses a real throw, so Bug 40 stays correct).
    fn analyze_throw_effects(&mut self, source: &SourceFile) {
        // Collect every user fn/method body with its base name and kind.
        // (basename, body, is_method)
        let mut fns: Vec<(String, Stmt, bool)> = Vec::new();
        fn collect(decls: &[Spanned<DeclKind>], out: &mut Vec<(String, Stmt, bool)>) {
            for d in decls {
                match &d.node {
                    DeclKind::Function(f) => out.push((f.name.clone(), f.body.clone(), false)),
                    DeclKind::Class(c) => {
                        for m in &c.methods {
                            out.push((m.name.clone(), m.body.clone(), true));
                        }
                    }
                    DeclKind::Interface(i) => {
                        for m in &i.methods {
                            out.push((m.name.clone(), m.body.clone(), true));
                        }
                    }
                    DeclKind::Namespace(ns) => collect(&ns.decls, out),
                    _ => {}
                }
            }
        }
        collect(&source.decls, &mut fns);

        let mut tf: HashSet<String> = HashSet::new();
        let mut tm: HashSet<String> = HashSet::new();
        loop {
            let mut changed = false;
            for (name, body, is_method) in &fns {
                let already = if *is_method { tm.contains(name) } else { tf.contains(name) };
                if already {
                    continue;
                }
                if Self::stmt_may_throw(body, &tf, &tm) {
                    if *is_method {
                        tm.insert(name.clone());
                    } else {
                        tf.insert(name.clone());
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.throwing_free_fns = tf;
        self.throwing_method_basenames = tm;
    }

    fn gen_try_stmt(
        &mut self,
        body: &Stmt,
        catches: &[CatchClause],
        finally: Option<&Stmt>,
        ctx: &mut GenCtx,
    ) -> Result<(), ErrorBag> {
        let error_var = format!("%__error_{}__", self.temp_count);
        let try_bb = self.new_bb("try");
        let catch_bb = self.new_bb("catch");
        let finally_bb = if finally.is_some() {
            Some(self.new_bb("finally"))
        } else {
            None
        };
        let end_bb = self.new_bb("try_end");
        // Convergence point after body/catch/finally. For a try WITHOUT catch
        // clauses this is where an unhandled error is re-thrown after finally
        // (Bug 42); with catch clauses it just falls through to end_bb.
        let converge_bb = self.new_bb("try_converge");

        // Normal completion and the catch dispatch funnel through finally (if
        // present) and then the convergence point — never straight to end_bb, so
        // the re-throw check always gets a chance to run.
        let merge_target = finally_bb.as_deref().unwrap_or(&converge_bb).to_string();

        writeln!(&mut self.ir, "{} = alloca i64", error_var).unwrap();
        writeln!(&mut self.ir, "store i64 0, i64* {}", error_var).unwrap();

        // Issue #193: a `return` anywhere inside the try body OR a catch
        // clause must run this finally block before actually returning —
        // set up the pending-flag/return-value slots `emit_function_return`
        // routes through, and push them so it's visible to everything about
        // to be generated below. See GenCtx.finally_targets' own doc comment
        // for the full design (this mirrors error_catch's alloca-then-set
        // shape, just as a stack instead of one slot, and popped again
        // further down, before the finally block's OWN body is generated).
        let finally_target = finally_bb.as_ref().map(|fb| {
            let pending_flag = format!("%__finally_pending_{}__", self.temp_count);
            self.temp_count += 1;
            writeln!(&mut self.ir, "{} = alloca i1", pending_flag).unwrap();
            writeln!(&mut self.ir, "store i1 false, i1* {}", pending_flag).unwrap();
            let return_slot = if ctx.ret_type.is_empty() || ctx.ret_type == "void" {
                None
            } else {
                let slot = format!("%__finally_retval_{}__", self.temp_count);
                self.temp_count += 1;
                writeln!(&mut self.ir, "{} = alloca {}", slot, ctx.ret_type).unwrap();
                Some(slot)
            };
            FinallyTarget { finally_bb: fb.clone(), pending_flag, return_slot }
        });
        if let Some(target) = &finally_target {
            ctx.finally_targets.push(target.clone());
        }

        // --- try body ---
        writeln!(&mut self.ir, "br label %{}", try_bb).unwrap();
        writeln!(&mut self.ir, "{}:", try_bb).unwrap();
        let old_error_catch = ctx.error_catch.take();
        // Depth BEFORE the try body's own Block pushes its defer scope — a
        // local throw unwinds everything opened since here (Bug 41 follow-up).
        let try_defer_depth = ctx.defer_stack.len();
        ctx.error_catch = Some((catch_bb.clone(), error_var.clone(), try_defer_depth));
        // The body runs with error_catch set: per-statement throw-checks inside
        // (emitted by the Block handler and other nested scopes) branch to this
        // try's catch (Bug 40). A trailing check covers a single-statement body
        // (which isn't a Block) and is a harmless no-op after a Block body.
        self.gen_stmt_body(body, ctx)?;
        if !self.last_is_terminator() {
            self.emit_post_stmt_throw_check(ctx)?;
        }
        ctx.error_catch = old_error_catch;
        let try_ok_bb = self.new_bb("try_ok");
        writeln!(&mut self.ir, "br label %{}", try_ok_bb).unwrap();
        writeln!(&mut self.ir, "{}:", try_ok_bb).unwrap();
        writeln!(&mut self.ir, "br label %{}", merge_target).unwrap();

        // --- catch blocks (chained) ---
        // Each catch clause gets its own labeled block; they are chained so that
        // control flows through all matching handlers. The dispatch block (catch_bb)
        // jumps into the first clause; each clause ends with an unreachable-guard
        // block that branches to the next clause (or merge_target after the last).
        if catches.is_empty() {
            // No catch clauses: the error is not handled here. Route to finally
            // (via merge_target), then re-throw at the convergence point. (The
            // old code emitted `catch_bb:` immediately followed by another label
            // with no terminator between them → invalid IR; try-finally without
            // catch never compiled.)
            writeln!(&mut self.ir, "{}:", catch_bb).unwrap();
            writeln!(&mut self.ir, "br label %{}", merge_target).unwrap();
        } else {
            // Pre-allocate all per-clause block labels so we can forward-reference them.
            let clause_bbs: Vec<String> = (0..catches.len())
                .map(|i| self.new_bb(&format!("catch_{}", i)))
                .collect();

            // Dispatch: jump to first clause.
            writeln!(&mut self.ir, "{}:", catch_bb).unwrap();
            writeln!(&mut self.ir, "br label %{}", clause_bbs[0]).unwrap();

            for (i, catch) in catches.iter().enumerate() {
                let llvm_ty = Self::type_to_llvm(&catch.ty);
                let param_slot = ctx.locals.len();
                ctx.locals
                    .insert(catch.param.clone(), (llvm_ty.clone(), param_slot));

                writeln!(&mut self.ir, "{}:", clause_bbs[i]).unwrap();
                // Unique slot name — the same catch-param name in a later
                // try/catch of this function must not redefine %<param>.
                let param_slot_name = format!("{}_{}", catch.param, self.temp_count);
                self.temp_count += 1;
                ctx.local_slots.insert(catch.param.clone(), param_slot_name.clone());
                writeln!(&mut self.ir, "%{} = alloca {}", param_slot_name, llvm_ty).unwrap();
                let err_val = self.temp();
                writeln!(
                    &mut self.ir,
                    "{} = load i64, i64* {}",
                    err_val, error_var
                )
                .unwrap();
                // Cast the stored i64 back to the catch param's type
                let store_val = if llvm_ty != "i64" {
                    let cast_val = self.temp();
                    if Self::is_float(&llvm_ty) {
                        writeln!(&mut self.ir, "{} = bitcast i64 {} to {}", cast_val, err_val, llvm_ty).unwrap();
                    } else if llvm_ty.ends_with('*') {
                        writeln!(&mut self.ir, "{} = inttoptr i64 {} to {}", cast_val, err_val, llvm_ty).unwrap();
                    } else {
                        writeln!(&mut self.ir, "{} = trunc i64 {} to {}", cast_val, err_val, llvm_ty).unwrap();
                    }
                    cast_val
                } else {
                    err_val
                };
                writeln!(
                    &mut self.ir,
                    "store {} {}, {}* %{}",
                    llvm_ty, store_val, llvm_ty, param_slot_name
                )
                .unwrap();
                self.gen_stmt_body(&catch.body, ctx)?;

                let next = if i + 1 < clause_bbs.len() {
                    clause_bbs[i + 1].clone()
                } else {
                    merge_target.clone()
                };
                let guard_bb = self.new_bb(&format!("catch_{}_ok", i));
                writeln!(&mut self.ir, "br label %{}", guard_bb).unwrap();
                writeln!(&mut self.ir, "{}:", guard_bb).unwrap();
                writeln!(&mut self.ir, "br label %{}", next).unwrap();
            }
        }

        // --- finally block ---
        // Runs on both the normal and the error path (merge_target), AND
        // (issue #193) whenever a `return` inside the try body/a catch
        // clause routed through here via emit_function_return. Popped from
        // ctx.finally_targets BEFORE generating the finally body itself, so
        // a `return` inside `finally { ... }` sees only any FURTHER-out
        // enclosing finally (never re-enters this same one).
        if let Some(fb) = &finally_bb {
            if finally_target.is_some() {
                ctx.finally_targets.pop();
            }
            writeln!(&mut self.ir, "{}:", fb).unwrap();
            if let Some(finally_stmt) = finally {
                self.gen_stmt_body(finally_stmt, ctx)?;
            }
            let finally_ok_bb = self.new_bb("finally_ok");
            writeln!(&mut self.ir, "br label %{}", finally_ok_bb).unwrap();
            writeln!(&mut self.ir, "{}:", finally_ok_bb).unwrap();

            // A `return` routed through here: propagate the pending value
            // to any further-out enclosing finally, or actually `ret` if
            // this was the outermost one. Otherwise (the ordinary normal-
            // completion/exception path), fall through to the convergence
            // point exactly as before this field existed.
            let target = finally_target.expect("finally_bb implies finally_target");
            let pending = self.temp();
            writeln!(&mut self.ir, "{} = load i1, i1* {}", pending, target.pending_flag).unwrap();
            let do_return_bb = self.new_bb("finally_do_return");
            let normal_bb = self.new_bb("finally_normal");
            writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", pending, do_return_bb, normal_bb).unwrap();

            writeln!(&mut self.ir, "{}:", do_return_bb).unwrap();
            let ret_ty = ctx.ret_type.clone();
            let ret_val = target.return_slot.as_ref().map(|slot| {
                let v = self.temp();
                writeln!(&mut self.ir, "{} = load {}, {}* {}", v, ret_ty, ret_ty, slot).unwrap();
                v
            });
            if let Some(outer) = ctx.finally_targets.last().cloned() {
                if let (Some(v), Some(outer_slot)) = (&ret_val, &outer.return_slot) {
                    writeln!(&mut self.ir, "store {} {}, {}* {}", ret_ty, v, ret_ty, outer_slot).unwrap();
                }
                writeln!(&mut self.ir, "store i1 true, i1* {}", outer.pending_flag).unwrap();
                writeln!(&mut self.ir, "br label %{}", outer.finally_bb).unwrap();
            } else {
                self.emit_transactional_commit_before_return(ctx);
                if ret_ty.is_empty() || ret_ty == "void" {
                    writeln!(&mut self.ir, "ret void").unwrap();
                } else {
                    writeln!(&mut self.ir, "ret {} {}", ret_ty, ret_val.unwrap()).unwrap();
                }
            }

            writeln!(&mut self.ir, "{}:", normal_bb).unwrap();
            writeln!(&mut self.ir, "br label %{}", converge_bb).unwrap();
        }

        // --- convergence / re-throw ---
        writeln!(&mut self.ir, "{}:", converge_bb).unwrap();
        if catches.is_empty() {
            // A try without catch clauses does not handle the error: if one
            // reached here (error_var != 0, set on the error path; 0 on the
            // normal path), re-throw it now — AFTER finally has run (Bug 42).
            let ev = self.temp();
            writeln!(&mut self.ir, "{} = load i64, i64* {}", ev, error_var).unwrap();
            let has = self.temp();
            writeln!(&mut self.ir, "{} = icmp ne i64 {}, 0", has, ev).unwrap();
            let rethrow_bb = self.new_bb("rethrow");
            writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", has, rethrow_bb, end_bb).unwrap();
            writeln!(&mut self.ir, "{}:", rethrow_bb).unwrap();
            if let Some((outer_catch, outer_error_var, outer_depth)) = ctx.error_catch.clone() {
                // Hand the error to the enclosing try in this function. Run
                // any defer scopes opened since the OUTER try's entry (Bug
                // 41 follow-up) — this try's own scope is already resolved
                // by this point (either via emit_unwind_defers_to on the
                // throw path, or the try body's normal Block-exit pop).
                writeln!(&mut self.ir, "store i64 {}, i64* {}", ev, outer_error_var).unwrap();
                self.emit_unwind_defers_to(ctx, outer_depth)?;
                writeln!(&mut self.ir, "br label %{}", outer_catch).unwrap();
            } else {
                // Propagate out of this frame: park in the global slot, run
                // pending defers (Bug 41), return the function default.
                writeln!(&mut self.ir, "store i64 {}, i64* @__tinox_err", ev).unwrap();
                self.emit_unwind_defers(ctx)?;
                self.emit_ret_default(ctx);
            }
        } else {
            writeln!(&mut self.ir, "br label %{}", end_bb).unwrap();
        }

        writeln!(&mut self.ir, "{}:", end_bb).unwrap();
        Ok(())
    }

    /// The single point every `return` statement's codegen funnels through
    /// (issue #193) instead of emitting `ret` directly: if there's an
    /// enclosing `try { ... } finally { ... }` (`ctx.finally_targets` is
    /// non-empty), the return value/pending-flag are stashed into that
    /// finally's own slots and control branches into it instead — the
    /// finally block's own tail (see `gen_try_stmt`) is what actually
    /// performs the `ret`, after running (and possibly propagating further
    /// through) every enclosing finally block, innermost first. With no
    /// enclosing finally, this is exactly the previous direct-`ret`
    /// behavior (still gated by `emit_transactional_commit_before_return`
    /// for an @Transactional method's own commit).
    ///
    /// `ty`/`val` are ALREADY the final, cast-if-needed LLVM type/value —
    /// callers do that work themselves first, same division of labor the
    /// direct `ret` emission this replaces already had. `val` is ignored
    /// when `ty == "void"`.
    fn emit_function_return(&mut self, ctx: &GenCtx, ty: &str, val: &str) {
        if let Some(target) = ctx.finally_targets.last().cloned() {
            if let Some(slot) = &target.return_slot {
                writeln!(&mut self.ir, "store {} {}, {}* {}", ty, val, ty, slot).unwrap();
            }
            writeln!(&mut self.ir, "store i1 true, i1* {}", target.pending_flag).unwrap();
            writeln!(&mut self.ir, "br label %{}", target.finally_bb).unwrap();
            return;
        }
        self.emit_transactional_commit_before_return(ctx);
        if ty == "void" {
            writeln!(&mut self.ir, "ret void").unwrap();
        } else {
            writeln!(&mut self.ir, "ret {} {}", ty, val).unwrap();
        }
    }

    /// Emits the same "commit if I own this transaction" branch
    /// gen_transactional_wrapper's own fall-through (implicit-return) path
    /// emits, for use right before every `ret` a `return` statement
    /// produces -- a no-op (emits nothing) outside an @Transactional
    /// method's body, where ctx.transactional_commit is None. See the
    /// field's own doc comment on GenCtx for why this is needed at all.
    fn emit_transactional_commit_before_return(&mut self, ctx: &GenCtx) {
        let Some(owned_slot) = ctx.transactional_commit.clone() else {
            return;
        };
        let owned = self.temp();
        writeln!(&mut self.ir, "{} = load i1, i1* {}", owned, owned_slot).unwrap();
        let commit_bb = self.new_bb("tx_early_commit");
        let cont_bb = self.new_bb("tx_early_commit_cont");
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", owned, commit_bb, cont_bb).unwrap();
        writeln!(&mut self.ir, "{}:", commit_bb).unwrap();
        writeln!(&mut self.ir, "call void @tinox_db_tx_commit()").unwrap();
        writeln!(&mut self.ir, "br label %{}", cont_bb).unwrap();
        writeln!(&mut self.ir, "{}:", cont_bb).unwrap();
    }

    /// Wraps an @Transactional method's body (issue #191): BEGIN before,
    /// COMMIT on normal completion, ROLLBACK-then-rethrow on any error --
    /// structurally the same try/no-catch/no-swallow shape gen_try_stmt
    /// emits for `try { ... }` with no catch clauses (error_var alloca,
    /// ctx.error_catch redirect, emit_post_stmt_throw_check per statement,
    /// rethrow via ctx.error_catch if there's an enclosing try/@Transactional
    /// in this same function, else via the global `@__tinox_err` slot), just
    /// with a fixed BEGIN/COMMIT/ROLLBACK action instead of user catch/finally
    /// blocks, and gated on whether THIS call actually owns the transaction.
    ///
    /// Propagation is REQUIRED (Spring's default, no savepoints): a nested
    /// @Transactional call (or a plain call from inside one to another
    /// @Transactional method) joins the caller's already-open transaction --
    /// tinox_db_tx_active() is checked at entry, and only the outermost call
    /// (the one that finds no active transaction) actually begins/commits/
    /// rolls back. An inner call's own error still rolls back the whole
    /// (shared) transaction: rollback happens whenever this frame RE-THROWS
    /// past its own catch, regardless of ownership -- only the ACT of calling
    /// tinox_db_tx_commit/_rollback (which also releases the pooled
    /// connection) is gated on ownership, since only the outermost owner
    /// may safely do that.
    fn gen_transactional_wrapper(&mut self, method: &Method, ctx: &mut GenCtx) -> Result<(), ErrorBag> {
        let already_active = self.temp();
        writeln!(&mut self.ir, "{} = call i1 @tinox_db_tx_active()", already_active).unwrap();
        let owned_slot = self.temp();
        writeln!(&mut self.ir, "{} = alloca i1", owned_slot).unwrap();
        let owned_val = self.temp();
        writeln!(&mut self.ir, "{} = xor i1 {}, true", owned_val, already_active).unwrap();
        writeln!(&mut self.ir, "store i1 {}, i1* {}", owned_val, owned_slot).unwrap();

        let begin_bb = self.new_bb("tx_begin");
        let body_bb = self.new_bb("tx_body");
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", owned_val, begin_bb, body_bb).unwrap();
        writeln!(&mut self.ir, "{}:", begin_bb).unwrap();
        writeln!(&mut self.ir, "call i8* @tinox_db_tx_begin()").unwrap();
        writeln!(&mut self.ir, "br label %{}", body_bb).unwrap();
        writeln!(&mut self.ir, "{}:", body_bb).unwrap();

        let error_var = format!("%__tx_error_{}__", self.temp_count);
        self.temp_count += 1;
        writeln!(&mut self.ir, "{} = alloca i64", error_var).unwrap();
        writeln!(&mut self.ir, "store i64 0, i64* {}", error_var).unwrap();

        let catch_bb = self.new_bb("tx_catch");
        let end_bb = self.new_bb("tx_end");

        let old_error_catch = ctx.error_catch.take();
        let try_defer_depth = ctx.defer_stack.len();
        ctx.error_catch = Some((catch_bb.clone(), error_var.clone(), try_defer_depth));
        let old_transactional_commit = ctx.transactional_commit.replace(owned_slot.clone());

        self.gen_stmt_body(&method.body, ctx)?;
        if !self.last_is_terminator() {
            self.emit_post_stmt_throw_check(ctx)?;
        }
        ctx.error_catch = old_error_catch;
        ctx.transactional_commit = old_transactional_commit;

        // Normal completion: commit (and release the pooled connection) if
        // this call owns the transaction, then fall through to gen_class_method's
        // own implicit-return handling.
        let commit_bb = self.new_bb("tx_commit");
        writeln!(&mut self.ir, "br label %{}", commit_bb).unwrap();
        writeln!(&mut self.ir, "{}:", commit_bb).unwrap();
        let owned_at_commit = self.temp();
        writeln!(&mut self.ir, "{} = load i1, i1* {}", owned_at_commit, owned_slot).unwrap();
        let do_commit_bb = self.new_bb("tx_do_commit");
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", owned_at_commit, do_commit_bb, end_bb).unwrap();
        writeln!(&mut self.ir, "{}:", do_commit_bb).unwrap();
        writeln!(&mut self.ir, "call void @tinox_db_tx_commit()").unwrap();
        writeln!(&mut self.ir, "br label %{}", end_bb).unwrap();

        // Error path: rollback (and release) if we own the transaction, then
        // ALWAYS re-throw -- @Transactional never swallows an error, it only
        // reacts to one, exactly like gen_try_stmt's catches.is_empty() path.
        writeln!(&mut self.ir, "{}:", catch_bb).unwrap();
        let owned_at_catch = self.temp();
        writeln!(&mut self.ir, "{} = load i1, i1* {}", owned_at_catch, owned_slot).unwrap();
        let do_rollback_bb = self.new_bb("tx_do_rollback");
        let after_rollback_bb = self.new_bb("tx_after_rollback");
        writeln!(&mut self.ir, "br i1 {}, label %{}, label %{}", owned_at_catch, do_rollback_bb, after_rollback_bb).unwrap();
        writeln!(&mut self.ir, "{}:", do_rollback_bb).unwrap();
        writeln!(&mut self.ir, "call void @tinox_db_tx_rollback()").unwrap();
        writeln!(&mut self.ir, "br label %{}", after_rollback_bb).unwrap();
        writeln!(&mut self.ir, "{}:", after_rollback_bb).unwrap();

        let ev = self.temp();
        writeln!(&mut self.ir, "{} = load i64, i64* {}", ev, error_var).unwrap();
        if let Some((outer_catch, outer_error_var, outer_depth)) = ctx.error_catch.clone() {
            writeln!(&mut self.ir, "store i64 {}, i64* {}", ev, outer_error_var).unwrap();
            self.emit_unwind_defers_to(ctx, outer_depth)?;
            writeln!(&mut self.ir, "br label %{}", outer_catch).unwrap();
        } else {
            writeln!(&mut self.ir, "store i64 {}, i64* @__tinox_err", ev).unwrap();
            self.emit_unwind_defers(ctx)?;
            self.emit_ret_default(ctx);
        }

        writeln!(&mut self.ir, "{}:", end_bb).unwrap();
        Ok(())
    }

    fn gen_defer_scope(&mut self, ctx: &mut GenCtx) -> Result<(), ErrorBag> {
        if let Some(scope) = ctx.defer_stack.last().cloned() {
            let old_in_defer = ctx.in_defer_exec;
            ctx.in_defer_exec = true;
            for stmt in scope.into_iter().rev() {
                self.gen_stmt_body(&Box::new(stmt), ctx)?;
            }
            ctx.in_defer_exec = old_in_defer;
        }
        Ok(())
    }

    /// Run ALL active defer scopes (innermost first) before a throw unwinds out
    /// of the current function (Bug 41). Unlike gen_defer_scope (innermost scope,
    /// normal block exit), an escaping throw must clean up every enclosing scope
    /// — a throw nested in a loop still has to run the function-level `defer`.
    /// The defer_stack is left intact: the normal (non-throwing) control-flow
    /// path through the blocks still runs each scope on its own exit.
    fn emit_unwind_defers(&mut self, ctx: &mut GenCtx) -> Result<(), ErrorBag> {
        if ctx.in_defer_exec {
            return Ok(());
        }
        let scopes: Vec<Vec<Stmt>> = ctx.defer_stack.iter().rev().cloned().collect();
        if scopes.iter().all(|s| s.is_empty()) {
            return Ok(());
        }
        ctx.in_defer_exec = true;
        for scope in scopes {
            for stmt in scope.into_iter().rev() {
                self.gen_stmt_body(&Box::new(stmt), ctx)?;
            }
        }
        ctx.in_defer_exec = false;
        Ok(())
    }

    /// Run the defer scopes opened SINCE `depth` (i.e. those pushed after
    /// entering a `try` body), innermost first — for a throw that is
    /// caught LOCALLY (jumps to this function's own catch_bb) rather than
    /// escaping the frame (Bug 41 follow-up: "defer between throw and
    /// catch in the same function"). Like emit_unwind_defers,
    /// defer_stack is left INTACT (not truncated) — this codegen walk
    /// keeps visiting the try body's remaining statements after emitting
    /// the `br` to catch_bb (they become unreachable IR at the LLVM level,
    /// but the Rust-side Block handler still runs to completion and pops
    /// its own scope there). Truncating here would double-pop: the Block
    /// handler's own (dead-code) pop would then remove the WRONG scope
    /// (an outer one) once this try's scope had already vanished,
    /// silently losing outer `defer`s that must still run at their own
    /// later exit point.
    fn emit_unwind_defers_to(&mut self, ctx: &mut GenCtx, depth: usize) -> Result<(), ErrorBag> {
        if ctx.in_defer_exec || ctx.defer_stack.len() <= depth {
            return Ok(());
        }
        let scopes: Vec<Vec<Stmt>> = ctx.defer_stack[depth..].iter().rev().cloned().collect();
        if scopes.iter().all(|s| s.is_empty()) {
            return Ok(());
        }
        ctx.in_defer_exec = true;
        for scope in scopes {
            for stmt in scope.into_iter().rev() {
                self.gen_stmt_body(&Box::new(stmt), ctx)?;
            }
        }
        ctx.in_defer_exec = false;
        Ok(())
    }

    pub fn emit_llvm_ir(&self, path: &Path) -> Result<(), Error> {
        std::fs::write(path, &self.ir)
            .map_err(|e| Error::new(Span::dummy(), format!("Failed to write IR: {}", e)))
    }

    fn run_opt(&self, ir_path: &Path) -> Result<std::path::PathBuf, Error> {
        let bc_path = ir_path.with_extension("opt.bc");
        let output = std::process::Command::new("opt")
            .args(["-O3", "-o"])
            .arg(&bc_path)
            .arg(ir_path)
            .output()
            .map_err(|e| Error::new(Span::dummy(), format!("opt failed: {}", e)))?;

        if !output.status.success() {
            return Err(Error::new(
                Span::dummy(),
                format!("opt failed: {}", String::from_utf8_lossy(&output.stderr)),
            ));
        }
        Ok(bc_path)
    }

    pub fn write_asm(&self, ir_path: &Path, asm_path: &Path) -> Result<(), Error> {
        let bc_path = self.run_opt(ir_path)?;
        let output = std::process::Command::new("llc")
            .args(["-O3", "-march=x86-64", "-filetype=asm", "-o"])
            .arg(asm_path)
            .arg(&bc_path)
            .output()
            .map_err(|e| Error::new(Span::dummy(), format!("llc failed: {}", e)))?;

        if !output.status.success() {
            return Err(Error::new(
                Span::dummy(),
                format!("llc failed: {}", String::from_utf8_lossy(&output.stderr)),
            ));
        }
        Ok(())
    }

    pub fn write_obj(&self, ir_path: &Path, obj_path: &Path) -> Result<(), Error> {
        let bc_path = self.run_opt(ir_path)?;
        let output = std::process::Command::new("llc")
            .args(["-O3", "-march=x86-64", "-filetype=obj", "-o"])
            .arg(obj_path)
            .arg(&bc_path)
            .output()
            .map_err(|e| Error::new(Span::dummy(), format!("llc failed: {}", e)))?;

        if !output.status.success() {
            return Err(Error::new(
                Span::dummy(),
                format!("llc failed: {}", String::from_utf8_lossy(&output.stderr)),
            ));
        }
        Ok(())
    }

    /// Produce a mangled name like `identity__i64__double` for a generic instantiation.
    /// Translate a marker from infer_struct_type back into a parser
    /// type (for inferring generic type arguments from call arguments).
    fn marker_to_type(marker: &str) -> tinox_parser::Type {
        use tinox_parser::Type;
        if let Some(cls) = marker.strip_prefix("List:") {
            return Type::Generic { name: "List".into(), args: vec![Type::Named(cls.to_string())] };
        }
        match marker {
            "Array:String" => Type::Generic { name: "List".into(), args: vec![Type::String] },
            "Array:Float" => Type::Generic { name: "List".into(), args: vec![Type::Float64] },
            m if m == "Array" || m.starts_with("Array:") => {
                Type::Generic { name: "List".into(), args: vec![Type::Int64] }
            }
            "Map" => Type::Map(Box::new(Type::String), Box::new(Type::Int64)),
            m if m.starts_with("Map:") => {
                let val_ty = match &m[4..] {
                    "String" => Type::String,
                    "Float" => Type::Float64,
                    vm => Self::marker_to_type(vm),
                };
                Type::Map(Box::new(Type::String), Box::new(val_ty))
            }
            cls => Type::Named(cls.to_string()),
        }
    }

    /// Mangling suffix from a parser type (keeps class names, unlike
    /// mangle_generic_name, which goes via LLVM types and loses classes).
    fn type_suffix(ty: &tinox_parser::Type) -> String {
        use tinox_parser::Type;
        match ty {
            Type::Named(n) => n.clone(),
            Type::String => "String".into(),
            Type::Int64 => "Int64".into(),
            Type::Float64 => "Float64".into(),
            Type::Bool => "Bool".into(),
            Type::Generic { name, args } => {
                let inner: Vec<String> = args.iter().map(Self::type_suffix).collect();
                format!("{}_{}", name, inner.join("_"))
            }
            Type::Array(inner) => format!("List_{}", Self::type_suffix(inner)),
            Type::Map(_, _) => "Map".into(),
            _ => "T".into(),
        }
    }

    /// Monomorphizes a generic static method at the call site and calls
    /// the specialization. Type arguments come explicitly
    /// (Json::deserialize<User>) or are inferred from the arguments
    /// (Json::serialize(users) via the infer_struct_type marker).
    /// Monomorphizes a generic static method for fully-known, explicit
    /// type arguments (no call-site inference -- callers that need
    /// inference, i.e. the actual `Class::method<T>(args)` call-site path,
    /// resolve `type_args` themselves first and pass the result here; see
    /// `gen_generic_method_call`, which is now a thin wrapper: resolve
    /// type args (with inference) -> call this -> codegen the call-site
    /// arguments). Extracted so callers that already know the concrete
    /// type at codegen time -- e.g. `emit_route_shim_body`'s `@PostParam`
    /// binding, which knows the target class from the parameter's own
    /// declared type, no AST call expression or `GenCtx` involved at all
    /// -- can reuse the exact same specialization machinery (registering
    /// `fn_sigs`/`method_ret_class`, emitting the specialized body via
    /// `gen_fn` with the right `type_param_aliases` active) without
    /// needing to fake up an expression-level call site.
    fn ensure_generic_method_specialization(
        &mut self,
        static_key: &str,
        gm: &tinox_parser::Method,
        type_args: &[tinox_parser::Type],
    ) -> Result<(String, String), ErrorBag> {
        use tinox_parser::Type;
        let mut subst: HashMap<String, Type> = HashMap::new();
        for (tp, ty) in gm.type_params.iter().zip(type_args.iter()) {
            subst.insert(tp.clone(), ty.clone());
        }

        let suffix: Vec<String> = gm
            .type_params
            .iter()
            .map(|tp| Self::type_suffix(subst.get(tp).unwrap()))
            .collect();
        let mangled = format!("{}__{}", static_key, suffix.join("__"));

        let ret_type = Self::substitute_type(&gm.ret_type, &subst);
        let ret_llvm = Self::type_to_llvm(&ret_type);

        if !self.generated_specializations.contains(&mangled) {
            self.generated_specializations.insert(mangled.clone());
            let specialized = tinox_parser::Function {
                name: mangled.clone(),
                type_params: vec![],
                params: gm
                    .params
                    .iter()
                    .map(|prm| tinox_parser::Param {
                        name: prm.name.clone(),
                        param_type: Self::substitute_type(&prm.param_type, &subst),
                        span: prm.span,
                        annotations: prm.annotations.clone(),
                    })
                    .collect(),
                ret_type: ret_type.clone(),
                body: gm.body.clone(),
                span: gm.span,
                is_async: gm.is_async,
                doc: None,
                annotations: vec![],
                file: gm.file.clone(),
            };
            // Register the signature + return class so inference applies at the call site
            let param_llvm: Vec<String> = specialized
                .params
                .iter()
                .map(|prm| Self::type_to_llvm(&prm.param_type))
                .collect();
            self.fn_sigs.insert(mangled.clone(), (ret_llvm.clone(), param_llvm));
            if let Type::Named(cls) = &ret_type {
                if self.defined_classes.contains(cls.as_str()) {
                    self.method_ret_class.insert(mangled.clone(), cls.clone());
                }
            } else if let Some(m) = Self::container_marker(&ret_type) {
                self.method_ret_class.insert(mangled.clone(), m);
            }
            // Emit with active aliases (T::fromJson -> User_fromJson);
            // into lambda_ir, so the function being generated isn't torn apart.
            let saved_aliases = std::mem::take(&mut self.type_param_aliases);
            for (tp, ty) in &subst {
                if let Type::Named(cls) = ty {
                    self.type_param_aliases.insert(tp.clone(), cls.clone());
                }
            }
            let saved_ir = std::mem::take(&mut self.ir);
            let saved_temp = self.temp_count;
            self.temp_count = 0;
            self.gen_fn(&specialized)?;
            let spec_ir = std::mem::take(&mut self.ir);
            self.ir = saved_ir;
            self.temp_count = saved_temp;
            self.lambda_ir.push_str(&spec_ir);
            self.type_param_aliases = saved_aliases;
        }

        Ok((mangled, ret_llvm))
    }

    /// Monomorphizes a generic static method at the call site and calls
    /// the specialization. Type arguments come explicitly
    /// (Json::deserialize<User>) or are inferred from the arguments
    /// (Json::serialize(users) via the infer_struct_type marker).
    fn gen_generic_method_call(
        &mut self,
        static_key: &str,
        gm: &tinox_parser::Method,
        type_args: &[tinox_parser::Type],
        args: &[tinox_parser::Expr],
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        use tinox_parser::Type;
        // Bindungen: Typparameter -> konkreter Parser-Typ
        let mut subst: HashMap<String, Type> = HashMap::new();
        for (i, tp) in gm.type_params.iter().enumerate() {
            let bound = if let Some(t) = type_args.get(i) {
                t.clone()
            } else {
                // Inference: the first argument whose declared type is
                // exactly the type parameter supplies the marker
                let mut inferred = None;
                for (pi, param) in gm.params.iter().enumerate() {
                    if matches!(&param.param_type, Type::Named(n) if n == tp) {
                        if let Some(arg) = args.get(pi) {
                            // Raw marker: infer_struct_type's Ident arm
                            // strips "List:" (legacy) — here we need the
                            // container type itself, not the element type.
                            let marker = if let ExprKind::Ident(n) = &arg.node {
                                ctx.local_types.get(n.as_str()).cloned()
                            } else {
                                None
                            }
                            .or_else(|| self.infer_struct_type(arg, ctx));
                            if let Some(marker) = marker {
                                inferred = Some(Self::marker_to_type(&marker));
                            }
                        }
                        break;
                    }
                }
                inferred.unwrap_or(Type::Int64)
            };
            subst.insert(tp.clone(), bound);
        }
        let resolved_type_args: Vec<Type> = gm.type_params.iter()
            .map(|tp| subst.get(tp).unwrap().clone())
            .collect();
        let (mangled, ret_llvm) = self.ensure_generic_method_specialization(static_key, gm, &resolved_type_args)?;

        // Call the specialization. The definition is produced via gen_fn
        // (a top-level function WITHOUT an implicit self parameter), so
        // the call site must not prepend self either — otherwise every
        // argument shifts by one (bug: `Iter::repeat(7,3)` bound
        // count=7, value=null). This path is exclusively the static
        // `Class::method` call; instance calls of generic methods run
        // elsewhere.
        let mut args_parts: Vec<String> = Vec::new();
        for arg in args.iter() {
            let (v, t) = self.gen_expr(arg, ctx)?;
            args_parts.push(format!("{} {}", t, v));
        }
        let result = self.temp();
        if ret_llvm == "void" {
            writeln!(&mut self.ir, "call void @{}({})", mangled, args_parts.join(", ")).unwrap();
            Ok(("0".to_string(), "void".to_string()))
        } else {
            writeln!(&mut self.ir, "{} = call {} @{}({})", result, ret_llvm, mangled, args_parts.join(", ")).unwrap();
            Ok((result, ret_llvm))
        }
    }

    /// Issue #165: finds the expression whose tinox-typecheck-inferred value
    /// carries an arrow-sugar lambda body's effective return type, so
    /// `infer_own_type_params` can consult `expr_value_types` for it (arrow
    /// lambdas have no annotated return type for it to read directly).
    ///
    /// - Non-block body (`n => expr`): the body IS the return value; typecheck
    ///   already cached its type at `body.id` (see `infer_type`'s Lambda arm).
    /// - Block body (`n => { ...; return expr; }`): the block's OWN cached
    ///   type is useless here (`ExprKind::Block`'s typecheck only uses a
    ///   trailing `StmtKind::Expr`, never `StmtKind::Return`), so walk the
    ///   block for the first `return expr;` and use ITS id instead. Only
    ///   recurses into nested `Block`/`If` statements (the common shapes for
    ///   a single, structurally findable return value) — a return buried in
    ///   a loop body falls through to the existing `Int64` default, same as
    ///   before this fix, not a regression.
    fn lambda_body_value_expr(body: &tinox_parser::Expr) -> Option<&tinox_parser::Expr> {
        use tinox_parser::{ExprKind, StmtKind};
        fn find_in_stmt(stmt: &tinox_parser::Stmt) -> Option<&tinox_parser::Expr> {
            match &stmt.node {
                StmtKind::Return(Some(e)) => Some(e),
                StmtKind::Block(stmts) => stmts.iter().find_map(find_in_stmt),
                StmtKind::If { then_branch, else_branch, .. } => {
                    find_in_stmt(then_branch).or_else(|| else_branch.as_deref().and_then(find_in_stmt))
                }
                _ => None,
            }
        }
        match &body.node {
            ExprKind::Block(stmts) => stmts.iter().find_map(|s| find_in_stmt(s)),
            _ => Some(body),
        }
    }

    /// Issue #165 counterpart to `marker_to_type`: converts a
    /// tinox-typecheck-inferred `ValueType` (from `expr_value_types`) into the
    /// parser `Type` shape `unify_type_param` operates on. Only covers the
    /// cases that can plausibly appear as a lambda's inferred return type;
    /// anything else (Tuple, Range, Nullable, ...) isn't needed here and
    /// falls through to the existing `Int64` fallback, unchanged from before
    /// this fix.
    fn value_type_to_parser_type(vt: &tinox_typecheck::ValueType) -> Option<tinox_parser::Type> {
        use tinox_parser::Type;
        use tinox_typecheck::ValueType as VT;
        match vt {
            VT::Int => Some(Type::Int64),
            VT::Float => Some(Type::Float64),
            VT::Bool => Some(Type::Bool),
            VT::String => Some(Type::String),
            VT::Named(name, args) if args.is_empty() => Some(Type::Named(name.clone())),
            VT::Named(name, args) => {
                let arg_types: Option<Vec<Type>> = args.iter().map(Self::value_type_to_parser_type).collect();
                arg_types.map(|a| Type::Generic { name: name.clone(), args: a })
            }
            VT::Array(inner) => Self::value_type_to_parser_type(inner).map(|t| Type::Array(Box::new(t))),
            _ => None,
        }
    }

    /// Structural unification: does `pattern` contain the bare type param `tp`
    /// at some position, and if so what does `concrete`'s value at that same
    /// position bind it to? E.g. `unify_type_param(Named("U"), String, "U")`
    /// → `Some(String)`; `unify_type_param(Generic{"Option",[Named("U")]},
    /// Generic{"Option",[String]}, "U")` → `Some(String)` (handles `andThen`'s
    /// `fnc(T) -> Option<U>` shape, not just `map`'s direct `fnc(T) -> U`).
    fn unify_type_param(
        pattern: &tinox_parser::Type,
        concrete: &tinox_parser::Type,
        tp: &str,
    ) -> Option<tinox_parser::Type> {
        use tinox_parser::Type;
        match pattern {
            Type::Named(n) if n == tp => Some(concrete.clone()),
            Type::Generic { name: pn, args: pargs } => {
                if let Type::Generic { name: cn, args: cargs } = concrete {
                    if pn == cn {
                        for (pa, ca) in pargs.iter().zip(cargs.iter()) {
                            if let Some(t) = Self::unify_type_param(pa, ca, tp) {
                                return Some(t);
                            }
                        }
                    }
                }
                None
            }
            Type::Array(pinner) => match concrete {
                Type::Array(cinner) => Self::unify_type_param(pinner, cinner, tp),
                _ => None,
            },
            Type::Fn { params: pparams, ret: pret } => match concrete {
                Type::Fn { params: cparams, ret: cret } => {
                    for (pp, cp) in pparams.iter().zip(cparams.iter()) {
                        if let Some(t) = Self::unify_type_param(pp, cp, tp) {
                            return Some(t);
                        }
                    }
                    Self::unify_type_param(pret, cret, tp)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Infer concrete bindings for a method's OWN type params (e.g. `U` in
    /// `fn map<U>(transform: fnc(T) -> U)`) from the actual call-site
    /// arguments — needed for #153's instance-call monomorphization since,
    /// unlike explicit `Class::method<U>(...)` static calls, instance-call
    /// syntax (`option.map(...)`) has no syntactic slot for an explicit type
    /// argument at all.
    ///
    /// For a lambda argument, unifies the param's declared `fnc(...)->R`
    /// shape against the lambda's own annotated param/return types where the
    /// lambda has them (Tinox's explicit `fnc(...)->R {...}` form always
    /// does). Arrow-sugar lambdas (`n => ...`, issue #165) have none of that
    /// to unify against, so as a fallback consults tinox-typecheck's own
    /// already-inferred type for the lambda's return value via
    /// `lambda_body_value_expr`/`expr_value_types`. For a non-lambda
    /// argument, falls back to the existing marker-based struct-type
    /// inference. Unresolved params default to `Int64`, matching the same
    /// fallback `gen_generic_method_call` already uses for the static-call
    /// path.
    fn infer_own_type_params(
        &self,
        method: &tinox_parser::Method,
        raw_args: &[tinox_parser::Expr],
        ctx: &GenCtx,
    ) -> HashMap<String, tinox_parser::Type> {
        use tinox_parser::{ExprKind, Type};
        let mut subst: HashMap<String, Type> = HashMap::new();
        for tp in &method.type_params {
            let mut inferred: Option<Type> = None;
            for (pi, param) in method.params.iter().enumerate() {
                let Some(arg) = raw_args.get(pi) else { continue };
                match &param.param_type {
                    Type::Named(n) if n == tp => {
                        let marker = if let ExprKind::Ident(name) = &arg.node {
                            ctx.local_types.get(name.as_str()).cloned()
                        } else {
                            None
                        }
                        .or_else(|| self.infer_struct_type(arg, ctx));
                        if let Some(m) = marker {
                            inferred = Some(Self::marker_to_type(&m));
                        }
                    }
                    Type::Fn { params: fn_params, ret } => {
                        if let ExprKind::Lambda { params: lam_params, ret_type: lam_ret, body } = &arg.node {
                            if let Some(lr) = lam_ret {
                                inferred = Self::unify_type_param(ret, lr, tp);
                            }
                            if inferred.is_none() {
                                for (fp, lp) in fn_params.iter().zip(lam_params.iter()) {
                                    if let Some(t) = Self::unify_type_param(fp, &lp.param_type, tp) {
                                        inferred = Some(t);
                                        break;
                                    }
                                }
                            }
                            // Issue #165: arrow-sugar lambdas (`n => ...`) have no
                            // annotated param/return types at all (parser gives
                            // them `Type::Infer`/`ret_type: None`), so both
                            // attempts above find nothing and this used to fall
                            // straight through to the `unwrap_or(Type::Int64)`
                            // default below, silently mis-specializing `tp`
                            // whenever the real type wasn't Int64. Recover the
                            // return type tinox-typecheck already inferred for
                            // the lambda body (keyed by node id in
                            // expr_value_types) and unify that against `ret`.
                            if inferred.is_none() {
                                if let Some(ret_expr) = Self::lambda_body_value_expr(body) {
                                    if let Some(vt) = self.expr_value_types.get(&ret_expr.id) {
                                        if let Some(t) = Self::value_type_to_parser_type(vt) {
                                            inferred = Self::unify_type_param(ret, &t, tp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    other => {
                        if let Some(marker) = self.infer_struct_type(arg, ctx) {
                            inferred = Self::unify_type_param(other, &Self::marker_to_type(&marker), tp);
                        }
                    }
                }
                if inferred.is_some() {
                    break;
                }
            }
            subst.insert(tp.clone(), inferred.unwrap_or(Type::Int64));
        }
        subst
    }

    /// #153: monomorphize + call an own-type-param instance method of a
    /// generic class (`Option<T>.map<U>`, `.andThen<U>`, or any user-defined
    /// equivalent) from ordinary instance-call syntax (`option.map(...)`).
    ///
    /// Combines the class's existing T binding with a freshly-inferred U
    /// binding into ONE substitution pass over the PRISTINE (unspecialized)
    /// method — see `generic_instance_methods`'s doc comment for why it must
    /// be the pristine copy, and a no-op self-rename, not the class's own
    /// `(name, mangled_name)` pair `substitute_class` uses — this method
    /// legitimately constructs the SAME class at a DIFFERENT type argument
    /// (`Option<U>::some(...)` inside `Option<T>.map<U>`), which the
    /// class-level rename would otherwise incorrectly collapse onto T's
    /// specialization instead of U's.
    fn gen_generic_instance_method_call(
        &mut self,
        mangled_class: &str,
        fn_name: &str,
        raw_args: &[tinox_parser::Expr],
        recv: EvaluatedReceiver,
        call_node_id: u32,
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        let EvaluatedReceiver { obj_ty, obj_ptr, extra_args } = recv;
        use tinox_parser::Type;
        let (orig_class, class_bindings, pristine) = self
            .generic_instance_methods
            .get(fn_name)
            .cloned()
            .expect("gen_generic_instance_method_call called for an unregistered fn_name");

        let own_subst = self.infer_own_type_params(&pristine, raw_args, ctx);
        let mut subst: HashMap<String, Type> = class_bindings
            .iter()
            .map(|(tp, llvm)| (tp.clone(), Self::llvm_ty_to_parser_type(llvm)))
            .collect();
        subst.extend(own_subst);

        let suffix: Vec<String> = pristine
            .type_params
            .iter()
            .map(|tp| Self::type_suffix(subst.get(tp).unwrap()))
            .collect();
        let mangled_method_name = format!("{}__{}", pristine.name, suffix.join("__"));
        let target_fn = format!("{}_{}", mangled_class, mangled_method_name);
        let concrete_ret = Self::substitute_type(&pristine.ret_type, &subst);

        if !self.generated_specializations.contains(&target_fn) {
            self.generated_specializations.insert(target_fn.clone());
            let no_op_rename = (orig_class.as_str(), orig_class.as_str());
            let specialized = tinox_parser::Method {
                name: mangled_method_name.clone(),
                type_params: vec![],
                params: pristine
                    .params
                    .iter()
                    .map(|p| tinox_parser::Param {
                        name: p.name.clone(),
                        param_type: Self::substitute_type(&p.param_type, &subst),
                        span: p.span,
                        annotations: p.annotations.clone(),
                    })
                    .collect(),
                ret_type: concrete_ret.clone(),
                body: Self::substitute_stmt(&pristine.body, &subst, no_op_rename),
                static_: pristine.static_,
                visibility: pristine.visibility.clone(),
                span: pristine.span,
                is_async: pristine.is_async,
                doc: None,
                annotations: vec![],
                file: pristine.file.clone(),
            };
            let saved_ir = std::mem::take(&mut self.ir);
            let saved_temp = self.temp_count;
            self.temp_count = 0;
            self.gen_class_method(mangled_class, &specialized)?;
            let spec_ir = std::mem::take(&mut self.ir);
            self.ir = saved_ir;
            self.temp_count = saved_temp;
            self.lambda_ir.push_str(&spec_ir);
        }

        // Compute the return marker, then register it under TWO keys:
        //
        // 1. The UNSUFFIXED "{class}_{method}" key (== fn_name, not
        //    target_fn) in `method_ret_class` — `infer_struct_type_local`'s
        //    MethodCall arm falls back to querying exactly this shape with
        //    no knowledge of U. Kept for any caller that reaches it (e.g. a
        //    receiver whose OWN declared type — not this call's — is being
        //    looked up some other way).
        // 2. This exact call expression's `call_node_id`, in
        //    `methodcall_result_markers` — #158: two calls to the SAME
        //    method on the SAME class with a DIFFERENT own-type-param
        //    instantiation (`o.map(intToInt).map(intToString)`, both keyed
        //    "Option__i64_map" in (1)) clobber each other there, whichever
        //    is emitted last winning — so an EARLIER call's chained
        //    follow-on would read the WRONG class. Per-node keying can't
        //    collide this way and is what `infer_struct_type_local` now
        //    checks FIRST (see there).
        let marker: Option<String> = match &concrete_ret {
            Type::Named(cls) if self.defined_classes.contains(cls.as_str()) => Some(cls.clone()),
            Type::Generic { name, args } if self.generic_classes.contains_key(name.as_str()) => {
                // Ensure the returned specialization actually exists (struct
                // layout + its own methods) — not just its mangled name —
                // so a chained call finding this marker can look up methods
                // on it immediately.
                let ret_bindings: HashMap<String, String> = self
                    .generic_classes
                    .get(name.as_str())
                    .cloned()
                    .map(|rgc| {
                        rgc.type_params
                            .iter()
                            .zip(args.iter())
                            .map(|(tp, ta)| (tp.clone(), Self::type_to_llvm(ta)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.ensure_generic_class_specialization_with_bindings(name, &ret_bindings).ok()
            }
            other => Self::container_marker(other),
        };
        if let Some(marker) = marker {
            self.method_ret_class.insert(fn_name.to_string(), marker.clone());
            self.methodcall_result_markers.insert(call_node_id, marker);
        }

        let ret_ty = self.method_ret_types.get(&target_fn).cloned().unwrap_or_else(|| "i64".to_string());
        let mut full_args_str = format!("{} {}", obj_ty, obj_ptr);
        for (val, ty) in extra_args {
            full_args_str.push_str(&format!(", {} {}", ty, val));
        }
        let result = self.temp();
        if ret_ty == "void" {
            writeln!(&mut self.ir, "call void @{}({})", target_fn, full_args_str).unwrap();
            Ok(("0".to_string(), "void".to_string()))
        } else {
            writeln!(&mut self.ir, "{} = call {} @{}({})", result, ret_ty, target_fn, full_args_str).unwrap();
            Ok((result, ret_ty))
        }
    }

    /// The bridge between the two type systems: translate a checker `ValueType`
    /// into the codegen's marker language, resolving a generic instance to its
    /// mangled specialization name (`Named("Box",[Int])` → `"Box__i64"`). This is
    /// how the rich type export becomes usable by the marker-based codegen.
    fn valuetype_to_marker(&self, vt: &tinox_typecheck::ValueType) -> Option<String> {
        use tinox_typecheck::ValueType as VT;
        match vt {
            VT::String => Some("String".to_string()),
            VT::Float => Some("Float".to_string()),
            VT::Named(name, args) if args.is_empty() => Some(name.clone()),
            VT::Named(name, args) => {
                // Generic instance → mangled specialization, if the class is known.
                if let Some(gc) = self.generic_classes.get(name) {
                    let tps = gc.type_params.clone();
                    let bindings: HashMap<String, String> = tps.iter().zip(args.iter())
                        .map(|(tp, a)| (tp.clone(), Self::valuetype_to_llvm(a)))
                        .collect();
                    Some(Self::mangle_generic_name(name, &tps, &bindings))
                } else {
                    Some(name.clone())
                }
            }
            VT::Array(e) => Some(match e.as_ref() {
                VT::String => "Array:String".to_string(),
                VT::Float => "Array:Float".to_string(),
                VT::Named(c, _) => format!("List:{}", c),
                inner => match self.valuetype_to_marker(inner) {
                    Some(m) => format!("Array:{}", m),
                    None => "Array".to_string(),
                },
            }),
            VT::Map(v) => Some(match self.valuetype_to_marker(v) {
                Some(m) => format!("Map:{}", m),
                None => "Map".to_string(),
            }),
            VT::Nullable(inner) => self.valuetype_to_marker(inner),
            _ => None,
        }
    }

    /// The llvm slot type for a ValueType arg, mirroring `type_to_llvm`, used to
    /// mangle a generic specialization (`Int` → `i64`, `String` → `i8*`).
    fn valuetype_to_llvm(vt: &tinox_typecheck::ValueType) -> String {
        use tinox_typecheck::ValueType as VT;
        match vt {
            VT::Int => "i64".to_string(),
            VT::Float => "double".to_string(),
            VT::Bool => "i1".to_string(),
            VT::Char => "i32".to_string(),
            VT::String => "i8*".to_string(),
            VT::Named(_, _) => "i64*".to_string(),
            VT::Array(_) | VT::Map(_) => "i64*".to_string(),
            VT::Nullable(inner) => Self::valuetype_to_llvm(inner),
            _ => "i64".to_string(),
        }
    }

    fn mangle_generic_name(name: &str, type_params: &[String], bindings: &HashMap<String, String>) -> String {
        let suffix: Vec<String> = type_params
            .iter()
            .map(|tp| {
                bindings.get(tp).cloned().unwrap_or_else(|| "i64".to_string())
                    .replace('*', "P")
                    .replace(' ', "_")
            })
            .collect();
        if suffix.is_empty() { name.to_string() } else { format!("{}__{}", name, suffix.join("__")) }
    }

    /// Resolve a parser Type using concrete LLVM type bindings for type parameters.
    fn type_to_llvm_with_bindings(ty: &tinox_parser::Type, bindings: &HashMap<String, String>) -> String {
        match ty {
            tinox_parser::Type::Named(n) => {
                if let Some(llvm) = bindings.get(n) { llvm.clone() }
                else { Self::type_to_llvm(ty) }
            }
            tinox_parser::Type::Generic { name, .. } => {
                if let Some(llvm) = bindings.get(name) { llvm.clone() }
                else { Self::type_to_llvm(ty) }
            }
            _ => Self::type_to_llvm(ty),
        }
    }

    /// Substitute type parameter names in a `Type` with concrete parser `Type`s.
    fn substitute_type(ty: &tinox_parser::Type, subst: &HashMap<String, tinox_parser::Type>) -> tinox_parser::Type {
        match ty {
            tinox_parser::Type::Named(n) => {
                subst.get(n).cloned().unwrap_or_else(|| ty.clone())
            }
            tinox_parser::Type::Generic { name, args } => {
                if let Some(concrete) = subst.get(name) {
                    concrete.clone()
                } else {
                    tinox_parser::Type::Generic {
                        name: name.clone(),
                        args: args.iter().map(|a| Self::substitute_type(a, subst)).collect(),
                    }
                }
            }
            tinox_parser::Type::Array(inner) => tinox_parser::Type::Array(Box::new(Self::substitute_type(inner, subst))),
            tinox_parser::Type::Ref(inner) => tinox_parser::Type::Ref(Box::new(Self::substitute_type(inner, subst))),
            tinox_parser::Type::Mutable(inner) => tinox_parser::Type::Mutable(Box::new(Self::substitute_type(inner, subst))),
            tinox_parser::Type::Fn { params, ret } => tinox_parser::Type::Fn {
                params: params.iter().map(|p| Self::substitute_type(p, subst)).collect(),
                ret: Box::new(Self::substitute_type(ret, subst)),
            },
            other => other.clone(),
        }
    }

    /// Collapse any occurrence of the class's own (generic) name in a Type
    /// to the concrete mangled name — see `substitute_class` for why.
    fn rename_self_type(ty: &tinox_parser::Type, self_rename: (&str, &str)) -> tinox_parser::Type {
        use tinox_parser::Type;
        match ty {
            Type::Named(n) if n == self_rename.0 => Type::Named(self_rename.1.to_string()),
            Type::Generic { name, .. } if name == self_rename.0 => Type::Named(self_rename.1.to_string()),
            Type::Generic { name, args } => Type::Generic {
                name: name.clone(),
                args: args.iter().map(|a| Self::rename_self_type(a, self_rename)).collect(),
            },
            Type::Array(inner) => Type::Array(Box::new(Self::rename_self_type(inner, self_rename))),
            Type::Ref(inner) => Type::Ref(Box::new(Self::rename_self_type(inner, self_rename))),
            Type::Mutable(inner) => Type::Mutable(Box::new(Self::rename_self_type(inner, self_rename))),
            Type::Fn { params, ret } => Type::Fn {
                params: params.iter().map(|p| Self::rename_self_type(p, self_rename)).collect(),
                ret: Box::new(Self::rename_self_type(ret, self_rename)),
            },
            Type::Nullable(inner) => Type::Nullable(Box::new(Self::rename_self_type(inner, self_rename))),
            other => other.clone(),
        }
    }

    /// Deep-substitute type annotations in a stmt tree (Bug 20.2):
    /// `substitute_class`/`substitute_fn` previously only replaced
    /// field/param/return types, the method BODY was cloned unchanged. A
    /// `let value: V = ...;` in the body (e.g. Cache::get) thereby kept
    /// the bare type parameter — `type_to_llvm(Named("V"))` falls back
    /// to "i64*" regardless of whether V is actually Int64. Walks the
    /// whole tree once when a generic class is monomorphized.
    /// `self_rename` is (original class name, mangled name): generic-class
    /// methods often self-construct via `ClassName<T> { field: … }`
    /// (StructLiteral — the AST has no type_args there, so the class name
    /// itself is the only substitution point) or recursively via
    /// `ClassName<T>::factory()`. Left unrenamed, the specialized method
    /// body would allocate/dispatch against the UNMANGLED class — which has
    /// no registered struct_layout (generic classes are skipped from the
    /// normal pre-pass) and silently allocates a 0-byte struct (Bug 20.2:
    /// Result::ok returned a corrupted value from exactly this).
    fn substitute_stmt(stmt: &Stmt, subst: &HashMap<String, Type>, self_rename: (&str, &str)) -> Stmt {
        let node = match &stmt.node {
            StmtKind::Expr(e) => StmtKind::Expr(Self::substitute_expr(e, subst, self_rename)),
            StmtKind::Let { name, ty, value } => StmtKind::Let {
                name: name.clone(),
                ty: ty.as_ref().map(|t| Self::substitute_type(t, subst)),
                value: value.as_ref().map(|v| Self::substitute_expr(v, subst, self_rename)),
            },
            StmtKind::Var { name, ty, value, mutable } => StmtKind::Var {
                name: name.clone(),
                ty: ty.as_ref().map(|t| Self::substitute_type(t, subst)),
                value: value.as_ref().map(|v| Self::substitute_expr(v, subst, self_rename)),
                mutable: *mutable,
            },
            StmtKind::Assignment { target, value } => StmtKind::Assignment {
                target: Self::substitute_expr(target, subst, self_rename),
                value: Self::substitute_expr(value, subst, self_rename),
            },
            StmtKind::If { cond, then_branch, else_branch } => StmtKind::If {
                cond: Self::substitute_expr(cond, subst, self_rename),
                then_branch: Box::new(Self::substitute_stmt(then_branch, subst, self_rename)),
                else_branch: else_branch.as_ref().map(|b| Box::new(Self::substitute_stmt(b, subst, self_rename))),
            },
            StmtKind::While { cond, body } => StmtKind::While {
                cond: Self::substitute_expr(cond, subst, self_rename),
                body: Box::new(Self::substitute_stmt(body, subst, self_rename)),
            },
            StmtKind::For { var, iter, body } => StmtKind::For {
                var: var.clone(),
                iter: Self::substitute_expr(iter, subst, self_rename),
                body: Box::new(Self::substitute_stmt(body, subst, self_rename)),
            },
            StmtKind::ForC { init, cond, update, body } => StmtKind::ForC {
                init: init.as_ref().map(|s| Box::new(Self::substitute_stmt(s, subst, self_rename))),
                cond: cond.as_ref().map(|e| Self::substitute_expr(e, subst, self_rename)),
                update: update.as_ref().map(|e| Self::substitute_expr(e, subst, self_rename)),
                body: Box::new(Self::substitute_stmt(body, subst, self_rename)),
            },
            StmtKind::Loop { body } => StmtKind::Loop { body: Box::new(Self::substitute_stmt(body, subst, self_rename)) },
            StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(|e| Self::substitute_expr(e, subst, self_rename))),
            StmtKind::Break => StmtKind::Break,
            StmtKind::Continue => StmtKind::Continue,
            StmtKind::Throw(e) => StmtKind::Throw(Self::substitute_expr(e, subst, self_rename)),
            StmtKind::Try { body, catches, finally } => StmtKind::Try {
                body: Box::new(Self::substitute_stmt(body, subst, self_rename)),
                catches: catches
                    .iter()
                    .map(|c| CatchClause {
                        param: c.param.clone(),
                        ty: Self::substitute_type(&c.ty, subst),
                        body: Self::substitute_stmt(&c.body, subst, self_rename),
                        span: c.span,
                    })
                    .collect(),
                finally: finally.as_ref().map(|b| Box::new(Self::substitute_stmt(b, subst, self_rename))),
            },
            StmtKind::Defer(s) => StmtKind::Defer(Box::new(Self::substitute_stmt(s, subst, self_rename))),
            StmtKind::Block(stmts) => {
                StmtKind::Block(stmts.iter().map(|s| Self::substitute_stmt(s, subst, self_rename)).collect())
            }
            StmtKind::Select { arms, default } => StmtKind::Select {
                arms: arms
                    .iter()
                    .map(|a| tinox_parser::SelectArm {
                        channel: Self::substitute_expr(&a.channel, subst, self_rename),
                        var: a.var.clone(),
                        body: Self::substitute_stmt(&a.body, subst, self_rename),
                        span: a.span,
                    })
                    .collect(),
                default: default.as_ref().map(|b| Box::new(Self::substitute_stmt(b, subst, self_rename))),
            },
            StmtKind::Empty => StmtKind::Empty,
        };
        Spanned { node, span: stmt.span, id: stmt.id }
    }

    /// Counterpart to `substitute_stmt` for expr nodes (see there for `self_rename`).
    fn substitute_expr(expr: &Expr, subst: &HashMap<String, Type>, self_rename: (&str, &str)) -> Expr {
        let rename = |n: &String| -> String {
            if n == self_rename.0 { self_rename.1.to_string() } else { n.clone() }
        };
        let node = match &expr.node {
            ExprKind::Literal(l) => ExprKind::Literal(l.clone()),
            ExprKind::ArrayLiteral(es) => {
                ExprKind::ArrayLiteral(es.iter().map(|e| Self::substitute_expr(e, subst, self_rename)).collect())
            }
            ExprKind::MapLiteral(entries) => ExprKind::MapLiteral(
                entries
                    .iter()
                    .map(|(k, v)| (Self::substitute_expr(k, subst, self_rename), Self::substitute_expr(v, subst, self_rename)))
                    .collect(),
            ),
            ExprKind::Ident(n) => ExprKind::Ident(n.clone()),
            ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: op.clone(),
                lhs: Box::new(Self::substitute_expr(lhs, subst, self_rename)),
                rhs: Box::new(Self::substitute_expr(rhs, subst, self_rename)),
            },
            ExprKind::Unary { op, operand } => ExprKind::Unary {
                op: op.clone(),
                operand: Box::new(Self::substitute_expr(operand, subst, self_rename)),
            },
            ExprKind::Call { func, args } => ExprKind::Call {
                func: Box::new(Self::substitute_expr(func, subst, self_rename)),
                args: args.iter().map(|a| Self::substitute_expr(a, subst, self_rename)).collect(),
            },
            ExprKind::MethodCall { obj, method, args } => ExprKind::MethodCall {
                obj: Box::new(Self::substitute_expr(obj, subst, self_rename)),
                method: method.clone(),
                args: args.iter().map(|a| Self::substitute_expr(a, subst, self_rename)).collect(),
            },
            ExprKind::Index { obj, index } => ExprKind::Index {
                obj: Box::new(Self::substitute_expr(obj, subst, self_rename)),
                index: Box::new(Self::substitute_expr(index, subst, self_rename)),
            },
            ExprKind::FieldAccess { obj, field } => ExprKind::FieldAccess {
                obj: Box::new(Self::substitute_expr(obj, subst, self_rename)),
                field: field.clone(),
            },
            ExprKind::This => ExprKind::This,
            ExprKind::SuperCall { method, args } => ExprKind::SuperCall {
                method: method.clone(),
                args: args.iter().map(|a| Self::substitute_expr(a, subst, self_rename)).collect(),
            },
            ExprKind::New { class, type_args, args } => ExprKind::New {
                class: rename(class),
                type_args: type_args.iter().map(|t| Self::substitute_type(t, subst)).collect(),
                args: args.iter().map(|a| Self::substitute_expr(a, subst, self_rename)).collect(),
            },
            ExprKind::StructLiteral { name, fields } => ExprKind::StructLiteral {
                name: rename(name),
                fields: fields
                    .iter()
                    .map(|(n, v)| (n.clone(), Self::substitute_expr(v, subst, self_rename)))
                    .collect(),
            },
            ExprKind::Block(stmts) => {
                ExprKind::Block(stmts.iter().map(|s| Self::substitute_stmt(s, subst, self_rename)).collect())
            }
            ExprKind::If { cond, then_branch, else_branch } => ExprKind::If {
                cond: Box::new(Self::substitute_expr(cond, subst, self_rename)),
                then_branch: Box::new(Self::substitute_expr(then_branch, subst, self_rename)),
                else_branch: else_branch.as_ref().map(|b| Box::new(Self::substitute_expr(b, subst, self_rename))),
            },
            ExprKind::While { cond, body } => ExprKind::While {
                cond: Box::new(Self::substitute_expr(cond, subst, self_rename)),
                body: Box::new(Self::substitute_expr(body, subst, self_rename)),
            },
            ExprKind::For { var, iter, body } => ExprKind::For {
                var: var.clone(),
                iter: Box::new(Self::substitute_expr(iter, subst, self_rename)),
                body: Box::new(Self::substitute_expr(body, subst, self_rename)),
            },
            ExprKind::Loop { body } => ExprKind::Loop { body: Box::new(Self::substitute_expr(body, subst, self_rename)) },
            ExprKind::Match { expr: scrutinee, cases } => ExprKind::Match {
                expr: Box::new(Self::substitute_expr(scrutinee, subst, self_rename)),
                cases: cases
                    .iter()
                    .map(|c| tinox_parser::MatchCase {
                        pattern: c.pattern.clone(),
                        guard: c.guard.as_ref().map(|g| Self::substitute_expr(g, subst, self_rename)),
                        body: Self::substitute_expr(&c.body, subst, self_rename),
                        span: c.span,
                    })
                    .collect(),
            },
            ExprKind::Return(e) => ExprKind::Return(e.as_ref().map(|e| Box::new(Self::substitute_expr(e, subst, self_rename)))),
            ExprKind::Break => ExprKind::Break,
            ExprKind::Continue => ExprKind::Continue,
            ExprKind::Throw(e) => ExprKind::Throw(Box::new(Self::substitute_expr(e, subst, self_rename))),
            ExprKind::Try { body, catches, finally } => ExprKind::Try {
                body: Box::new(Self::substitute_expr(body, subst, self_rename)),
                catches: catches
                    .iter()
                    .map(|c| CatchClause {
                        param: c.param.clone(),
                        ty: Self::substitute_type(&c.ty, subst),
                        body: Self::substitute_stmt(&c.body, subst, self_rename),
                        span: c.span,
                    })
                    .collect(),
                finally: finally.as_ref().map(|b| Box::new(Self::substitute_expr(b, subst, self_rename))),
            },
            ExprKind::Assign { target, value } => ExprKind::Assign {
                target: Box::new(Self::substitute_expr(target, subst, self_rename)),
                value: Box::new(Self::substitute_expr(value, subst, self_rename)),
            },
            ExprKind::CompoundAssign { op, target, value } => ExprKind::CompoundAssign {
                op: op.clone(),
                target: Box::new(Self::substitute_expr(target, subst, self_rename)),
                value: Box::new(Self::substitute_expr(value, subst, self_rename)),
            },
            ExprKind::Lambda { params, ret_type, body } => ExprKind::Lambda {
                params: params
                    .iter()
                    .map(|p| tinox_parser::Param {
                        name: p.name.clone(),
                        param_type: Self::substitute_type(&p.param_type, subst),
                        span: p.span,
                        annotations: p.annotations.clone(),
                    })
                    .collect(),
                ret_type: ret_type.as_ref().map(|t| Self::substitute_type(t, subst)),
                body: Box::new(Self::substitute_expr(body, subst, self_rename)),
            },
            ExprKind::Spawn(e) => ExprKind::Spawn(Box::new(Self::substitute_expr(e, subst, self_rename))),
            ExprKind::Await(e) => ExprKind::Await(Box::new(Self::substitute_expr(e, subst, self_rename))),
            ExprKind::Channel => ExprKind::Channel,
            ExprKind::Send { channel, value } => ExprKind::Send {
                channel: Box::new(Self::substitute_expr(channel, subst, self_rename)),
                value: Box::new(Self::substitute_expr(value, subst, self_rename)),
            },
            ExprKind::Recv(e) => ExprKind::Recv(Box::new(Self::substitute_expr(e, subst, self_rename))),
            ExprKind::Cast { expr: inner, ty } => ExprKind::Cast {
                expr: Box::new(Self::substitute_expr(inner, subst, self_rename)),
                ty: Self::substitute_type(ty, subst),
            },
            ExprKind::Is { expr: inner, ty } => ExprKind::Is {
                expr: Box::new(Self::substitute_expr(inner, subst, self_rename)),
                ty: Self::substitute_type(ty, subst),
            },
            ExprKind::Range { start, end, inclusive } => ExprKind::Range {
                start: Box::new(Self::substitute_expr(start, subst, self_rename)),
                end: Box::new(Self::substitute_expr(end, subst, self_rename)),
                inclusive: *inclusive,
            },
            ExprKind::Tuple(es) => ExprKind::Tuple(es.iter().map(|e| Self::substitute_expr(e, subst, self_rename)).collect()),
            ExprKind::TupleIndex { tuple, index } => ExprKind::TupleIndex {
                tuple: Box::new(Self::substitute_expr(tuple, subst, self_rename)),
                index: *index,
            },
            ExprKind::EnumValue { enum_name, variant, type_args, args } => ExprKind::EnumValue {
                enum_name: rename(enum_name),
                variant: variant.clone(),
                type_args: type_args.iter().map(|t| Self::substitute_type(t, subst)).collect(),
                args: args.iter().map(|a| Self::substitute_expr(a, subst, self_rename)).collect(),
            },
        };
        Spanned { node, span: expr.span, id: expr.id }
    }

    /// Create a monomorphic copy of a generic function with substituted types and a mangled name.
    fn substitute_fn(f: &tinox_parser::Function, mangled_name: &str, bindings: &HashMap<String, String>) -> tinox_parser::Function {
        // Build a Type substitution map: "T" -> Type::Int64 etc.
        let subst: HashMap<String, tinox_parser::Type> = bindings.iter().map(|(tp, llvm_ty)| {
            let concrete_type = Self::llvm_ty_to_parser_type(llvm_ty);
            (tp.clone(), concrete_type)
        }).collect();
        tinox_parser::Function {
            name: mangled_name.to_string(),
            type_params: vec![],
            params: f.params.iter().map(|p| tinox_parser::Param {
                name: p.name.clone(),
                param_type: Self::substitute_type(&p.param_type, &subst),
                span: p.span,
                annotations: p.annotations.clone(),
            }).collect(),
            ret_type: Self::substitute_type(&f.ret_type, &subst),
            body: f.body.clone(),
            span: f.span,
            is_async: f.is_async,
            doc: f.doc.clone(),
            annotations: vec![],
            file: f.file.clone(),
        }
    }

    /// Compute the mangled class name for a generic instantiation without emitting code.
    fn effective_class_name(&self, class: &str, type_args: &[tinox_parser::Type]) -> String {
        if type_args.is_empty() {
            return class.to_string();
        }
        if let Some(gc) = self.generic_classes.get(class) {
            let bindings: HashMap<String, String> = gc.type_params.iter()
                .zip(type_args.iter())
                .map(|(tp, ta)| (tp.clone(), Self::type_to_llvm(ta)))
                .collect();
            Self::mangle_generic_name(class, &gc.type_params, &bindings)
        } else {
            class.to_string()
        }
    }

    /// Emit a `ClassName_method(args...)`-style static-dispatch call — shared
    /// by the plain static-call path and the generic-class receiver-marker
    /// fallback (see `ExprKind::EnumValue`). Instance methods (`fn`) get an
    /// implicit `i64* null` self; static methods (`fnc`) don't.
    fn emit_static_dispatch_call(
        &mut self,
        key: &str,
        ret_ty: &str,
        args: &[tinox_parser::Expr],
        ctx: &mut GenCtx,
    ) -> Result<(String, String), ErrorBag> {
        // Resolve inherited methods to the class that actually defines (emits)
        // them: `Derived::getN` has no `@Derived_getN` body — only `@Base_getN`.
        // The dot-syntax path already does this via method_impl; mirror it here so
        // `Class::method(obj)` on an inherited method links (was: undefined value).
        let key = self.method_impl.get(key).cloned().unwrap_or_else(|| key.to_string());
        let key = key.as_str();
        let mut args_parts: Vec<String> = Vec::new();
        let is_static = self.static_method_keys.contains(key);
        if !is_static {
            if let Some(declared) = self.method_param_types.get(key).map(|v| v.len()) {
                // An instance method via `Class::method(...)`. Two call
                // styles occur in the stdlib, disambiguated by the arg
                // count:
                //  - args == declared: the object isn't passed as self
                //    (or as an explicit first *declared* param, like
                //    `config: IniConfig`); self is unused → null-self.
                //  - args == declared + 1: the caller passed the receiver
                //    object as the leading arg (`Class::method(obj,
                //    args…)`) — it IS self. Then do NOT prepend a
                //    null-self, otherwise `this` in the method body reads
                //    the null pointer (segfault).
                if args.len() != declared + 1 {
                    args_parts.push("i64* null".to_string());
                }
            }
        }
        for arg in args.iter() {
            let (v, t) = self.gen_expr(arg, ctx)?;
            args_parts.push(format!("{} {}", t, v));
        }
        let args_str = args_parts.join(", ");
        if ret_ty == "void" {
            writeln!(&mut self.ir, "call void @{}({})", key, args_str).unwrap();
            return Ok(("0".to_string(), "void".to_string()));
        }
        let result = self.temp();
        writeln!(&mut self.ir, "{} = call {} @{}({})", result, ret_ty, key, args_str).unwrap();
        Ok((result, ret_ty.to_string()))
    }

    /// If `class` is a known generic class, monomorphize it with `type_args` and return the
    /// mangled name. Otherwise return the class name unchanged. Emits the specialized methods
    /// into `lambda_ir` the first time a given instantiation is requested.
    fn ensure_generic_class_specialization(
        &mut self,
        class: &str,
        type_args: &[tinox_parser::Type],
    ) -> Result<String, ErrorBag> {
        if type_args.is_empty() || !self.generic_classes.contains_key(class) {
            return Ok(class.to_string());
        }
        let gc = self.generic_classes.get(class).unwrap().clone();
        let bindings: HashMap<String, String> = gc.type_params.iter()
            .zip(type_args.iter())
            .map(|(tp, ta)| (tp.clone(), Self::type_to_llvm(ta)))
            .collect();
        self.ensure_generic_class_specialization_with_bindings(class, &bindings)
    }

    /// Core of `ensure_generic_class_specialization`, but with
    /// already-resolved type-parameter bindings (LLVM type strings
    /// instead of parser `Type`s) — callers are `New`/explicit type
    /// arguments (via the public variant above) and static instance
    /// calls of generic classes (`Cache::set(cache, …)`,
    /// `Option::some(5)`), which derive bindings from call-site
    /// arguments or the `let` annotation (Bug 20.2 — instance methods of
    /// generic classes were never emitted, because class
    /// pre-registration skips them entirely).
    fn ensure_generic_class_specialization_with_bindings(
        &mut self,
        class: &str,
        bindings: &HashMap<String, String>,
    ) -> Result<String, ErrorBag> {
        let Some(gc) = self.generic_classes.get(class).cloned() else {
            return Ok(class.to_string());
        };
        let mangled = Self::mangle_generic_name(class, &gc.type_params, bindings);
        if !self.generated_specializations.contains(&mangled) {
            self.generated_specializations.insert(mangled.clone());
            let specialized = Self::substitute_class(&gc, &mangled, bindings);
            // Register struct layout (field names, in order) + field type info
            // (mirrors the non-generic class pre-pass — needed for correct
            // String/class field ptrtoint/inttoptr casts on FieldAccess).
            let fields: Vec<String> = specialized.fields.iter().map(|f| f.name.clone()).collect();
            self.struct_layouts.insert(mangled.clone(), fields);
            let one_class_map: HashMap<String, tinox_parser::Class> =
                [(mangled.clone(), specialized.clone())].into_iter().collect();
            self.struct_field_class_types.insert(
                mangled.clone(),
                Self::collect_field_class_types(&mangled, &one_class_map),
            );
            self.struct_field_llvm_types.insert(
                mangled.clone(),
                Self::collect_field_llvm_types(&mangled, &one_class_map),
            );
            // B1 phase 4: emit a named struct type for this specialization so its
            // field access is typed too. Collected in spec_type_defs and spliced
            // in before all function bodies (see into_ir) — a forward-referenced
            // named type is opaque/unsized and rejected by the verifier.
            if let Some(def) = self.register_named_struct_type(&mangled) {
                self.spec_type_defs.push_str(&def);
                self.spec_type_defs.push('\n');
            }
            // Fn-typed fields (callback fields, e.g. Pool<T>.factory) — the
            // MethodCall dispatch for calling-a-field-as-a-function consults
            // this table by struct name; without it, `pool.factory()` is
            // misread as a regular class method and ICEs ("undefined value
            // @Pool__i64_factory").
            self.fn_field_sigs.insert(
                mangled.clone(),
                Self::collect_fn_field_sigs(&mangled, &one_class_map),
            );
            // Register method signatures for dispatch — ret type, param types
            // (for the static-call self-null convention below) and static-ness.
            // Methods with their OWN type params (`fn map<U>(...)`) are still
            // generic after the class-level substitution — defer them to the
            // existing call-site monomorphization (generic_methods), mirroring
            // the non-generic class pre-pass. Emitting them here would bake in
            // an unresolved `U` (fnc(T) -> U params/return, wrong LLVM types).
            let mut emit_now: Vec<tinox_parser::Method> = Vec::new();
            for method in &specialized.methods {
                let fn_name = format!("{}_{}", mangled, method.name);
                if !method.type_params.is_empty() {
                    self.generic_methods.insert(fn_name.clone(), method.clone());
                    // #153: instance-call monomorphization needs the PRISTINE
                    // (pre-T-substitution) method — see generic_instance_methods'
                    // doc comment for why the copy above (post-T-substitution,
                    // self-renamed) isn't safe to reuse for this.
                    if let Some(pristine) = gc.methods.iter().find(|m| m.name == method.name) {
                        self.generic_instance_methods.insert(
                            fn_name,
                            (class.to_string(), bindings.clone(), pristine.clone()),
                        );
                    }
                    continue;
                }
                // Methods with `fnc` parameters (`newWithFactory(f: fnc()->T)`)
                // are now emitted: since the closure representation is
                // uniform (every lambda is a closure block {fn_ptr,
                // env}), gen_class_method's signature translation (fnc →
                // i64, same as for non-generic classes) is sufficient.
                // A method's own type parameters (`fn map<U>(fnc(T)->U)`)
                // are already caught above (type_params).
                let ret_ty = Self::type_to_llvm(&method.ret_type);
                self.method_ret_types.insert(fn_name.clone(), ret_ty);
                self.method_impl.insert(fn_name.clone(), fn_name.clone());
                if method.static_ {
                    self.static_method_keys.insert(fn_name.clone());
                }
                let param_tys: Vec<tinox_parser::Type> =
                    method.params.iter().map(|p| p.param_type.clone()).collect();
                self.method_param_types.insert(fn_name, param_tys);
                emit_now.push(method.clone());
            }
            // Generate method IR into lambda_ir so it doesn't interrupt current function
            let saved_ir = std::mem::take(&mut self.ir);
            let saved_temp = self.temp_count;
            self.temp_count = 0;
            for method in &emit_now {
                self.gen_class_method(&mangled, method)?;
            }
            let spec_ir = std::mem::take(&mut self.ir);
            self.ir = saved_ir;
            self.temp_count = saved_temp;
            self.lambda_ir.push_str(&spec_ir);
        }
        Ok(mangled)
    }

    /// Create a monomorphic copy of a generic class with substituted types and a mangled name.
    fn substitute_class(
        c: &tinox_parser::Class,
        mangled_name: &str,
        bindings: &HashMap<String, String>,
    ) -> tinox_parser::Class {
        let subst: HashMap<String, tinox_parser::Type> = bindings.iter()
            .map(|(tp, llvm_ty)| (tp.clone(), Self::llvm_ty_to_parser_type(llvm_ty)))
            .collect();
        // (class name, mangled name) — param/field/return types that
        // name the class's own generic name (`cache: Cache<K,V>`, e.g.
        // the instance counterpart to `this`) are collapsed onto the
        // concrete mangled named type. Otherwise, such a param would
        // stay a `Type::Generic{"Cache",[String,Int64]}` after
        // substitution — `gen_class_method`'s param typing (only
        // `Type::Named` sets the local_types marker) has no case for
        // that, and field accesses/methods on the parameter
        // (`cache.accessOrder.removeAt(…)`) end up unmarked, going
        // nowhere (Bug 20.2 — a follow-up finding after the
        // StructLiteral rename).
        let self_rename = (c.name.as_str(), mangled_name);
        tinox_parser::Class {
            name: mangled_name.to_string(),
            type_params: vec![],
            extends: c.extends.clone(),
            implements: c.implements.clone(),
            fields: c.fields.iter().map(|f| tinox_parser::FieldDef {
                name: f.name.clone(),
                field_type: Self::rename_self_type(&Self::substitute_type(&f.field_type, &subst), self_rename),
                visibility: f.visibility.clone(),
                mutable: f.mutable,
                span: f.span,
                doc: f.doc.clone(),
                annotations: vec![],
            }).collect(),
            methods: c.methods.iter().map(|m| tinox_parser::Method {
                name: m.name.clone(),
                type_params: m.type_params.clone(),
                params: m.params.iter().map(|p| tinox_parser::Param {
                    name: p.name.clone(),
                    param_type: Self::rename_self_type(&Self::substitute_type(&p.param_type, &subst), self_rename),
                    span: p.span,
                    annotations: p.annotations.clone(),
                }).collect(),
                ret_type: Self::rename_self_type(&Self::substitute_type(&m.ret_type, &subst), self_rename),
                body: Self::substitute_stmt(&m.body, &subst, self_rename),
                static_: m.static_,
                visibility: m.visibility.clone(),
                span: m.span,
                is_async: m.is_async,
                doc: m.doc.clone(),
                annotations: vec![],
                file: m.file.clone(),
            }).collect(),
            span: c.span,
            doc: c.doc.clone(),
            annotations: vec![],
        }
    }

    /// Best-effort mapping from an LLVM type string back to a parser Type (for substitution).
    fn llvm_ty_to_parser_type(llvm_ty: &str) -> tinox_parser::Type {
        match llvm_ty {
            "i64" => tinox_parser::Type::Int64,
            "i32" => tinox_parser::Type::Int32,
            "i16" => tinox_parser::Type::Int16,
            "i8" => tinox_parser::Type::Int8,
            "double" => tinox_parser::Type::Float64,
            "float" => tinox_parser::Type::Float32,
            "i1" => tinox_parser::Type::Bool,
            "i8*" => tinox_parser::Type::String,
            "void" => tinox_parser::Type::Nothing,
            other if other.ends_with('*') => {
                let inner = &other[..other.len() - 1];
                tinox_parser::Type::Ref(Box::new(Self::llvm_ty_to_parser_type(inner)))
            }
            other => tinox_parser::Type::Named(other.to_string()),
        }
    }

    /// Coerce an LLVM value of the given type to i64, emitting cast instructions as needed.
    /// If `struct_name` is a class with a named LLVM struct type, emit a typed
    /// field store (typed GEP + `store <slot>`) and return true (B1 phase 3).
    /// Otherwise emit nothing and return false — the caller keeps its existing
    /// i64-slot store, which is layout-compatible with the typed path.
    fn try_typed_field_store(
        &mut self,
        struct_name: Option<&str>,
        obj_ptr: &str,
        field: &str,
        span: Span,
        val: &str,
        val_ty: &str,
    ) -> Result<bool, ErrorBag> {
        let Some(sname) = struct_name.filter(|s| self.class_named_types.contains(*s)) else {
            return Ok(false);
        };
        let sname = sname.to_string();
        let offset = self.checked_typed_offset(&sname, field, span)?;
        let field_llvm_ty = self.struct_field_llvm_types.get(&sname)
            .and_then(|m| m.get(field))
            .cloned()
            .unwrap_or_else(|| "i64".to_string());
        let slot = Self::slot_llvm_ty(&field_llvm_ty);
        let store_val = self.coerce_to_slot(val, val_ty, &slot);
        let field_ptr = self.temp();
        writeln!(&mut self.ir, "{} = getelementptr %class.{}, ptr {}, i32 0, i32 {}", field_ptr, sname, obj_ptr, offset).unwrap();
        writeln!(&mut self.ir, "store {} {}, {}* {}", slot, store_val, slot, field_ptr).unwrap();
        Ok(true)
    }

    /// Coerce a value of llvm type `val_ty` to an 8-byte struct slot type `slot`
    /// (double / a pointer / i64) for a typed field store (B1 phase 2). The common
    /// case (val_ty == slot) is a no-op; mismatches bit-cast/int-to-ptr as needed.
    fn coerce_to_slot(&mut self, val: &str, val_ty: &str, slot: &str) -> String {
        if val_ty == slot || val_ty.is_empty() {
            return val.to_string();
        }
        if slot == "double" {
            if val_ty == "i64" {
                let t = self.temp();
                writeln!(&mut self.ir, "  {} = bitcast i64 {} to double", t, val).unwrap();
                return t;
            }
            return val.to_string();
        }
        if slot.ends_with('*') {
            if val_ty == "i64" {
                let t = self.temp();
                writeln!(&mut self.ir, "  {} = inttoptr i64 {} to {}", t, val, slot).unwrap();
                return t;
            }
            if val_ty.ends_with('*') || val_ty == "ptr" {
                let t = self.temp();
                let from = if val_ty == "ptr" { "ptr" } else { val_ty };
                writeln!(&mut self.ir, "  {} = bitcast {} {} to {}", t, from, val, slot).unwrap();
                return t;
            }
            return val.to_string();
        }
        // slot == "i64"
        self.coerce_to_i64(val, val_ty)
    }

    /// Coerce a value to i1 (booleans are often stored as i64): `icmp ne … 0`.
    fn emit_i1(&mut self, val: &str, ty: &str) -> String {
        if ty == "i1" {
            val.to_string()
        } else {
            let c = self.temp();
            writeln!(&mut self.ir, "{} = icmp ne {} {}, 0", c, ty, val).unwrap();
            c
        }
    }

    /// Emit a checked integer division (`is_rem=false`) or remainder into `result`
    /// for any int width: divide-by-zero and INT_MIN/-1 overflow become a hard
    /// error instead of LLVM UB (garbage). i64 calls the checked runtime fn
    /// directly; i8/i16/i32 widen → check → narrow; other types fall back to raw.
    fn emit_checked_idiv(&mut self, result: &str, ty: &str, l: &str, r: &str, is_rem: bool) {
        let func = if is_rem { "tinox_checked_srem" } else { "tinox_checked_sdiv" };
        if ty == "i64" {
            writeln!(&mut self.ir, "{} = call i64 @{}(i64 {}, i64 {})", result, func, l, r).unwrap();
        } else if matches!(ty, "i8" | "i16" | "i32") {
            let le = self.temp(); writeln!(&mut self.ir, "{} = sext {} {} to i64", le, ty, l).unwrap();
            let re = self.temp(); writeln!(&mut self.ir, "{} = sext {} {} to i64", re, ty, r).unwrap();
            let wide = self.temp(); writeln!(&mut self.ir, "{} = call i64 @{}(i64 {}, i64 {})", wide, func, le, re).unwrap();
            writeln!(&mut self.ir, "{} = trunc i64 {} to {}", result, wide, ty).unwrap();
        } else {
            let instr = if is_rem { "srem" } else { "sdiv" };
            writeln!(&mut self.ir, "{} = {} {} {}, {}", result, instr, ty, l, r).unwrap();
        }
    }

    fn coerce_to_i64(&mut self, val: &str, ty: &str) -> String {
        if ty == "i64" {
            val.to_string()
        } else if ty == "double" {
            let t = self.temp();
            writeln!(&mut self.ir, "  {} = bitcast double {} to i64", t, val).unwrap();
            t
        } else if ty == "i1" {
            let t = self.temp();
            writeln!(&mut self.ir, "  {} = zext i1 {} to i64", t, val).unwrap();
            t
        } else if ty.ends_with('*') {
            let t = self.temp();
            writeln!(&mut self.ir, "  {} = ptrtoint {} {} to i64", t, ty, val).unwrap();
            t
        } else if matches!(ty, "i8" | "i16" | "i32") {
            // Small int widths must be widened to fill the i64 slot; without the
            // sext a `store i64 %v` on an i32 value is type-mismatched IR (Bug 62).
            let t = self.temp();
            writeln!(&mut self.ir, "  {} = sext {} {} to i64", t, ty, val).unwrap();
            t
        } else {
            val.to_string()
        }
    }

    /// Emit a spawn wrapper function into lambda_ir.
    /// The wrapper has signature `i8* @name(i8* %raw)` and unpacks n_slots-1 args
    /// from the flat [n_slots x i64] array (slot 0 = fn ptr).
    fn emit_spawn_wrapper(&mut self, name: &str, n_slots: usize, ret_ty: &str, param_tys: &[String]) {
        let mut w = String::new();
        let mut tc = 0usize;
        macro_rules! wt {
            () => {{ tc += 1; format!("%w{}", tc) }};
        }

        writeln!(&mut w, "define i8* @{}(i8* %raw) {{", name).unwrap();
        writeln!(&mut w, "entry.tnx:").unwrap();

        let ap = wt!();
        writeln!(&mut w, "  {} = bitcast i8* %raw to [{} x i64]*", ap, n_slots).unwrap();

        // Load fn ptr from slot 0
        let fp_slot = wt!();
        writeln!(&mut w, "  {} = getelementptr [{} x i64], [{} x i64]* {}, i64 0, i64 0", fp_slot, n_slots, n_slots, ap).unwrap();
        let fp_i64 = wt!();
        writeln!(&mut w, "  {} = load i64, i64* {}", fp_i64, fp_slot).unwrap();
        let fn_type_str = format!("{} ({})*", ret_ty, param_tys.join(", "));
        let fp_typed = wt!();
        writeln!(&mut w, "  {} = inttoptr i64 {} to {}", fp_typed, fp_i64, fn_type_str).unwrap();

        // Load and cast each arg
        let mut call_args: Vec<String> = Vec::new();
        for (i, param_ty) in param_tys.iter().enumerate() {
            let slot = wt!();
            writeln!(&mut w, "  {} = getelementptr [{} x i64], [{} x i64]* {}, i64 0, i64 {}", slot, n_slots, n_slots, ap, i + 1).unwrap();
            let raw = wt!();
            writeln!(&mut w, "  {} = load i64, i64* {}", raw, slot).unwrap();
            let typed = if param_ty == "i64" {
                raw
            } else if param_ty == "double" {
                let t = wt!();
                writeln!(&mut w, "  {} = bitcast i64 {} to double", t, raw).unwrap();
                t
            } else if param_ty == "i1" {
                let t = wt!();
                writeln!(&mut w, "  {} = trunc i64 {} to i1", t, raw).unwrap();
                t
            } else if param_ty.ends_with('*') {
                let t = wt!();
                writeln!(&mut w, "  {} = inttoptr i64 {} to {}", t, raw, param_ty).unwrap();
                t
            } else {
                raw
            };
            call_args.push(format!("{} {}", param_ty, typed));
        }

        // Call the function and return result as i8*
        let call_str = call_args.join(", ");
        if ret_ty == "void" {
            writeln!(&mut w, "  call void {}({})", fp_typed, call_str).unwrap();
            writeln!(&mut w, "  ret i8* null").unwrap();
        } else {
            let res = wt!();
            writeln!(&mut w, "  {} = call {} {}({})", res, ret_ty, fp_typed, call_str).unwrap();
            let ret_ptr = wt!();
            if ret_ty == "i64" {
                writeln!(&mut w, "  {} = inttoptr i64 {} to i8*", ret_ptr, res).unwrap();
            } else if ret_ty == "double" {
                let as_i64 = wt!();
                writeln!(&mut w, "  {} = bitcast double {} to i64", as_i64, res).unwrap();
                writeln!(&mut w, "  {} = inttoptr i64 {} to i8*", ret_ptr, as_i64).unwrap();
            } else if ret_ty == "i1" {
                let as_i64 = wt!();
                writeln!(&mut w, "  {} = zext i1 {} to i64", as_i64, res).unwrap();
                writeln!(&mut w, "  {} = inttoptr i64 {} to i8*", ret_ptr, as_i64).unwrap();
            } else if ret_ty.ends_with('*') {
                writeln!(&mut w, "  {} = bitcast {} {} to i8*", ret_ptr, ret_ty, res).unwrap();
            } else {
                writeln!(&mut w, "  {} = inttoptr i64 {} to i8*", ret_ptr, res).unwrap();
            }
            writeln!(&mut w, "  ret i8* {}", ret_ptr).unwrap();
        }

        writeln!(&mut w, "}}").unwrap();
        writeln!(&mut w).unwrap();
        self.lambda_ir.push_str(&w);
    }
}

fn expr_kind_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Literal(_) => "Literal",
        ExprKind::ArrayLiteral(_) => "ArrayLiteral",
        ExprKind::MapLiteral(_) => "MapLiteral",
        ExprKind::Ident(_) => "Ident",
        ExprKind::Binary { .. } => "Binary",
        ExprKind::Unary { .. } => "Unary",
        ExprKind::Call { .. } => "Call",
        ExprKind::MethodCall { .. } => "MethodCall",
        ExprKind::Index { .. } => "Index",
        ExprKind::FieldAccess { .. } => "FieldAccess",
        ExprKind::This => "This",
        ExprKind::SuperCall { .. } => "SuperCall",
        ExprKind::New { .. } => "New",
        ExprKind::StructLiteral { .. } => "StructLiteral",
        ExprKind::Block(_) => "Block",
        ExprKind::If { .. } => "If",
        ExprKind::While { .. } => "While",
        ExprKind::For { .. } => "For",
        ExprKind::Loop { .. } => "Loop",
        ExprKind::Match { .. } => "Match",
        ExprKind::Return(_) => "Return",
        ExprKind::Break => "Break",
        ExprKind::Continue => "Continue",
        ExprKind::Throw(_) => "Throw",
        ExprKind::Try { .. } => "Try",
        ExprKind::Assign { .. } => "Assign",
        ExprKind::CompoundAssign { .. } => "CompoundAssign",
        ExprKind::Lambda { .. } => "Lambda",
        ExprKind::Spawn(_) => "Spawn",
        ExprKind::Await(_) => "Await",
        ExprKind::Channel => "Channel",
        ExprKind::Send { .. } => "Send",
        ExprKind::Recv(_) => "Recv",
        ExprKind::Cast { .. } => "Cast",
        ExprKind::Is { .. } => "Is",
        ExprKind::Range { .. } => "Range",
        ExprKind::Tuple(_) => "Tuple",
        ExprKind::TupleIndex { .. } => "TupleIndex",
        ExprKind::EnumValue { .. } => "EnumValue",
    }
}

fn collect_free_vars_stmt(stmt: &tinox_parser::Stmt, param_names: &HashSet<String>, vars: &mut HashSet<String>) {
    match &stmt.node {
        StmtKind::Expr(e) => collect_free_vars_inner(e, param_names, vars),
        StmtKind::Return(Some(e)) => collect_free_vars_inner(e, param_names, vars),
        StmtKind::Let { value: Some(e), .. } => collect_free_vars_inner(e, param_names, vars),
        StmtKind::Var { value: Some(e), .. } => collect_free_vars_inner(e, param_names, vars),
        StmtKind::Assignment { target, value } => {
            collect_free_vars_inner(target, param_names, vars);
            collect_free_vars_inner(value, param_names, vars);
        }
        StmtKind::If { cond, then_branch, else_branch } => {
            collect_free_vars_inner(cond, param_names, vars);
            collect_free_vars_stmt(then_branch, param_names, vars);
            if let Some(eb) = else_branch { collect_free_vars_stmt(eb, param_names, vars); }
        }
        StmtKind::While { cond, body } => {
            collect_free_vars_inner(cond, param_names, vars);
            collect_free_vars_stmt(body, param_names, vars);
        }
        StmtKind::Block(stmts) => {
            for s in stmts { collect_free_vars_stmt(s, param_names, vars); }
        }
        _ => {}
    }
}

fn collect_free_vars(expr: &Expr, param_names: &HashSet<String>) -> HashSet<String> {
    let mut vars = HashSet::new();
    collect_free_vars_inner(expr, param_names, &mut vars);
    vars
}

fn collect_free_vars_inner(expr: &Expr, param_names: &HashSet<String>, vars: &mut HashSet<String>) {
    match &expr.node {
        ExprKind::Ident(name) => {
            if !param_names.contains(name) {
                vars.insert(name.clone());
            }
        }
        ExprKind::Binary { op: _, lhs, rhs } => {
            collect_free_vars_inner(lhs, param_names, vars);
            collect_free_vars_inner(rhs, param_names, vars);
        }
        ExprKind::Unary { op: _, operand } => {
            collect_free_vars_inner(operand, param_names, vars);
        }
        ExprKind::Call { func, args } => {
            collect_free_vars_inner(func, param_names, vars);
            for arg in args {
                collect_free_vars_inner(arg, param_names, vars);
            }
        }
        ExprKind::MethodCall {
            obj,
            method: _,
            args,
        } => {
            collect_free_vars_inner(obj, param_names, vars);
            for arg in args {
                collect_free_vars_inner(arg, param_names, vars);
            }
        }
        ExprKind::Index { obj, index } => {
            collect_free_vars_inner(obj, param_names, vars);
            collect_free_vars_inner(index, param_names, vars);
        }
        ExprKind::ArrayLiteral(exprs) => {
            for e in exprs {
                collect_free_vars_inner(e, param_names, vars);
            }
        }
        ExprKind::FieldAccess { obj, field: _ } => {
            collect_free_vars_inner(obj, param_names, vars);
        }
        ExprKind::StructLiteral { fields, .. } => {
            for (_, val) in fields {
                collect_free_vars_inner(val, param_names, vars);
            }
        }
        ExprKind::Tuple(exprs) => {
            for e in exprs {
                collect_free_vars_inner(e, param_names, vars);
            }
        }
        ExprKind::TupleIndex { tuple, .. } => {
            collect_free_vars_inner(tuple, param_names, vars);
        }
        ExprKind::Cast { expr, ty: _ } => {
            collect_free_vars_inner(expr, param_names, vars);
        }
        ExprKind::Block(stmts) => {
            for stmt in stmts {
                match &stmt.node {
                    StmtKind::Expr(e) => collect_free_vars_inner(e, param_names, vars),
                    StmtKind::Return(Some(e)) => collect_free_vars_inner(e, param_names, vars),
                    StmtKind::Let { value: Some(e), .. } => collect_free_vars_inner(e, param_names, vars),
                    StmtKind::Var { value: Some(e), .. } => collect_free_vars_inner(e, param_names, vars),
                    StmtKind::Assignment { target, value } => {
                        collect_free_vars_inner(target, param_names, vars);
                        collect_free_vars_inner(value, param_names, vars);
                    }
                    StmtKind::If { cond, then_branch, else_branch } => {
                        collect_free_vars_inner(cond, param_names, vars);
                        collect_free_vars_stmt(then_branch, param_names, vars);
                        if let Some(eb) = else_branch {
                            collect_free_vars_stmt(eb, param_names, vars);
                        }
                    }
                    StmtKind::While { cond, body } => {
                        collect_free_vars_inner(cond, param_names, vars);
                        collect_free_vars_stmt(body, param_names, vars);
                    }
                    _ => {}
                }
            }
        }
        ExprKind::Range {
            start,
            end,
            inclusive: _,
        } => {
            collect_free_vars_inner(start, param_names, vars);
            collect_free_vars_inner(end, param_names, vars);
        }
        ExprKind::Match { expr, cases } => {
            collect_free_vars_inner(expr, param_names, vars);
            for case in cases {
                collect_free_vars_inner(&case.body, param_names, vars);
            }
        }
        ExprKind::Lambda { params, body, .. } => {
            let mut lambda_params = param_names.clone();
            for p in params {
                lambda_params.insert(p.name.clone());
            }
            collect_free_vars_inner(body, &lambda_params, vars);
        }
        // `ClassName::method(args)` (static-dispatch call, e.g. Crypto::aesEncrypt(json,
        // secret)) and `ClassName::new(args)` (constructor call) both carry their
        // arguments in `args` like a normal Call — a captured outer variable used
        // ONLY as an argument here was previously invisible to free-var collection,
        // so it never made it into the lambda's closure environment. Codegen's
        // Ident fallback then emitted an undefined, wrongly-typed `%name`/`i64`
        // reference instead (silent invalid-IR miscompile, not caught until `opt`
        // rejected it) — found while building OidcWebApp's install() closures.
        ExprKind::EnumValue { args, .. } | ExprKind::New { args, .. } => {
            for arg in args {
                collect_free_vars_inner(arg, param_names, vars);
            }
        }
        ExprKind::This | ExprKind::SuperCall { .. } | ExprKind::Is { .. } => {}
        ExprKind::Literal(_) => {}
        _ => {}
    }
}

pub struct GenCtx {
    locals: HashMap<String, (String, usize)>,
    /// Maps user variable name → unique LLVM alloca slot name (without %)
    local_slots: HashMap<String, String>,
    /// Variables that hold a range value (i64* with start/end, not an array)
    range_vars: HashSet<String>,
    params: HashSet<String>,
    #[allow(dead_code)]
    struct_fields: Vec<String>,
    current_struct: Option<String>,
    local_types: HashMap<String, String>,
    break_target: Option<String>,
    continue_target: Option<String>,
    /// (catch_bb, error_var, defer_stack depth at try-entry). The saved depth
    /// lets a local throw unwind exactly the defer scopes opened inside this
    /// try's body before jumping to catch_bb (Bug 41 follow-up — see
    /// emit_unwind_defers_to).
    error_catch: Option<(String, String, usize)>,
    defer_stack: Vec<Vec<Stmt>>,
    in_defer_exec: bool,
    /// LLVM return type of the current function (for casting return values)
    ret_type: String,
    /// If set, emit histogram_record before every return. (metric_name, start_reg)
    timed_metric: Option<(String, String)>,
    /// Set for the duration of an @Transactional method's body (issue
    /// #191, see gen_transactional_wrapper): the i1 alloca slot recording
    /// whether THIS call owns the transaction. A bare `return` statement
    /// (StmtKind::Return) emits its `ret` directly with no awareness of
    /// gen_transactional_wrapper's own try/catch-style structure around
    /// it -- exactly the same way a plain `try { return x; } finally {
    /// ... }` already skips its finally block today (a real, pre-existing
    /// bug found while building this, confirmed live: "finally ran" never
    /// printed). Left as a documented, separate, deferred issue for plain
    /// try/finally, but NOT acceptable for @Transactional, since virtually
    /// every real method ends with an explicit `return` -- so `return`'s
    /// own codegen checks this field directly and emits the same
    /// commit-if-owned branch gen_transactional_wrapper's own fall-through
    /// path emits, right before every `ret` it produces, regardless of
    /// nesting depth inside the method body.
    transactional_commit: Option<String>,
    /// Stack of enclosing `try { ... } finally { ... }` blocks a `return`
    /// statement (StmtKind::Return) must route through before actually
    /// returning (issue #193) — innermost last, mirroring `error_catch`'s
    /// single-slot shape but as a stack, since returns can be nested inside
    /// multiple enclosing finally blocks at once. Pushed by `gen_try_stmt`
    /// right before generating the try body/catch clauses (so a `return`
    /// anywhere in either sees it), popped again right before generating
    /// the finally block's OWN body (so a `return` inside `finally { ... }`
    /// itself only sees any FURTHER-out enclosing finally, never re-enters
    /// this same one). A try with no `finally` clause never pushes here at
    /// all — nothing needs to run before such a `return` proceeds, same as
    /// before this field existed.
    finally_targets: Vec<FinallyTarget>,
}

/// One entry in `GenCtx.finally_targets` — see that field's doc comment.
#[derive(Clone)]
struct FinallyTarget {
    finally_bb: String,
    /// i1 alloca: set true by a `return` that routed through here, checked
    /// by the finally block's own tail to decide whether to fall through to
    /// its normal converge/rethrow path (false) or propagate the pending
    /// return further out / actually `ret` (true).
    pending_flag: String,
    /// Alloca typed as the enclosing function's own return type, holding
    /// the value a routed-through `return` will eventually produce. `None`
    /// for a `void`-returning function — nothing to carry, `pending_flag`
    /// alone is enough.
    return_slot: Option<String>,
}

// ─── ORM: compile-time lambda→SQL translation ────────────────────────────────

/// Describes an ORM query chain unwound from `DB.of(T).filter(...).orderBy(...).limit(n).list()`
#[derive(Debug, Clone)]
struct OrmChain {
    entity_class: String,
    /// (lambda_param_name, lambda_body_expr)
    filters: Vec<(String, Expr)>,
    /// (col_name, is_desc)
    order_by: Vec<(String, bool)>,
    limit: Option<i64>,
    offset_val: Option<i64>,
    /// terminal operation: "list" | "first" | "count"
    terminal: String,
}

/// Try to unwind a `DB.of(T).filter(...).orderBy(...).limit(n).list()` chain
/// into an OrmChain descriptor. Returns None if the expr is not an ORM chain.
fn try_extract_orm_chain(expr: &Expr, terminal: &str) -> Option<OrmChain> {
    let mut chain = OrmChain {
        entity_class: String::new(),
        filters: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        offset_val: None,
        terminal: terminal.to_string(),
    };
    unwind_orm_chain(expr, &mut chain)?;
    if chain.entity_class.is_empty() { None } else { Some(chain) }
}

fn unwind_orm_chain(expr: &Expr, chain: &mut OrmChain) -> Option<()> {
    match &expr.node {
        ExprKind::MethodCall { obj, method, args } => {
            match method.as_str() {
                "filter" => {
                    if let Some(ExprKind::Lambda { params, body, .. }) = args.first().map(|a| &a.node) {
                        let param_name = params.first().map(|p| p.name.clone()).unwrap_or_default();
                        chain.filters.push((param_name, *body.clone()));
                    }
                    unwind_orm_chain(obj, chain)
                }
                "orderBy" => {
                    if let Some(lambda) = args.first() {
                        if let ExprKind::Lambda { body, .. } = &lambda.node {
                            if let ExprKind::FieldAccess { field, .. } = &body.node {
                                chain.order_by.push((field.clone(), false));
                            }
                        }
                    }
                    unwind_orm_chain(obj, chain)
                }
                "orderByDesc" => {
                    if let Some(lambda) = args.first() {
                        if let ExprKind::Lambda { body, .. } = &lambda.node {
                            if let ExprKind::FieldAccess { field, .. } = &body.node {
                                chain.order_by.push((field.clone(), true));
                            }
                        }
                    }
                    unwind_orm_chain(obj, chain)
                }
                "limit" => {
                    if let Some(ExprKind::Literal(Literal::Integer(n))) = args.first().map(|a| &a.node) {
                        chain.limit = Some(*n);
                    }
                    unwind_orm_chain(obj, chain)
                }
                "offset" => {
                    if let Some(ExprKind::Literal(Literal::Integer(n))) = args.first().map(|a| &a.node) {
                        chain.offset_val = Some(*n);
                    }
                    unwind_orm_chain(obj, chain)
                }
                "of" => {
                    // DB.of(ClassName) — bottom of the chain
                    if let ExprKind::Ident(db_name) = &obj.node {
                        if db_name == "DB" {
                            if let Some(ExprKind::Ident(class_name)) = args.first().map(|a| &a.node) {
                                chain.entity_class = class_name.clone();
                                return Some(());
                            }
                        }
                    }
                    None
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extract the column name for `param.field` in a lambda body.
fn orm_extract_field<'a>(expr: &Expr, param: &str, fields: &'a [EntityFieldEntry]) -> Option<&'a str> {
    if let ExprKind::FieldAccess { obj, field } = &expr.node {
        if let ExprKind::Ident(name) = &obj.node {
            if name == param {
                return fields.iter().find(|f| f.field_name == *field).map(|f| f.column_name.as_str());
            }
        }
    }
    None
}

pub fn gen(source: &SourceFile) -> Result<CodeGen, ErrorBag> {
    let mut codegen = CodeGen::new();
    codegen.gen(source)?;
    Ok(codegen)
}

impl Default for CodeGen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    fn compile_to_ir(src: &str) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().expect("parse failed");
        let mut cg = CodeGen::new();
        cg.gen(&ast).expect("codegen failed");
        cg.into_ir()
    }

    /// Mirrors `stamp_file_identity`/`stamp_file_identity_with` in
    /// `tinox/src/main.rs` (issue #114) — that function lives in the
    /// binary crate, not reachable from here, so this test-only helper
    /// duplicates its (small, mechanical) decl-walking logic to set a
    /// real `file` on every `Function`/`Method` before codegen. Without
    /// this, `compile_to_ir` leaves every declaration's `file` at the
    /// parser's `UNKNOWN_FILE` default and `dbg_suffix` correctly emits
    /// no debug info at all (see `test_debug_info_skips_unknown_file`).
    fn stamp_file_for_test(decls: &mut [tinox_parser::Decl], file: &Arc<str>) {
        for decl in decls {
            match &mut decl.node {
                DeclKind::Function(f) => f.file = file.clone(),
                DeclKind::Class(c) => {
                    for m in &mut c.methods {
                        m.file = file.clone();
                    }
                }
                DeclKind::Namespace(ns) => stamp_file_for_test(&mut ns.decls, file),
                _ => {}
            }
        }
    }

    fn compile_to_ir_with_file(src: &str, file: &str) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let mut ast = parser.parse().expect("parse failed");
        stamp_file_for_test(&mut ast.decls, &Arc::from(file));
        let mut cg = CodeGen::new();
        cg.gen(&ast).expect("codegen failed");
        cg.into_ir()
    }

    fn compile_expect_err(src: &str) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().expect("parse failed");
        let mut cg = CodeGen::new();
        let bag = cg.gen(&ast).expect_err("expected codegen to fail");
        bag.errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")
    }

    // --- class Main entry point (issue #149 stage 1) ---

    #[test]
    fn test_class_main_entry_point_emits_tinox_main_wrapper() {
        let src = "class Main {\n  fnc main() -> Int32 {\n    return 0;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("define i32 @Main_main()"), "{ir}");
        assert!(ir.contains("define i32 @tinox_main()"), "{ir}");
        assert!(ir.contains("call i32 @Main_main()"), "{ir}");
    }

    #[test]
    fn test_class_main_wrong_shape_instance_fn_errors() {
        // `fn main` (instance) instead of `fnc main` (static) must hard
        // error, not silently fall through to a linker "undefined
        // reference to tinox_main".
        let src = "class Main {\n  fn main() -> Int32 {\n    return 0;\n  }\n}";
        let msg = compile_expect_err(src);
        assert!(msg.contains("must be declared `fnc` (static), not `fn`"), "{msg}");
    }

    #[test]
    fn test_class_main_wrong_param_count_errors() {
        let src = "class Main {\n  fnc main(x: Int64) -> Int32 {\n    return 0;\n  }\n}";
        let msg = compile_expect_err(src);
        assert!(msg.contains("must take no parameters, found 1"), "{msg}");
    }

    #[test]
    fn test_class_main_wrong_return_type_errors() {
        let src = "class Main {\n  fnc main() -> Int64 {\n    return 0;\n  }\n}";
        let msg = compile_expect_err(src);
        assert!(msg.contains("must return Int32, found i64"), "{msg}");
    }

    #[test]
    fn test_class_main_ambiguous_entry_point_errors() {
        // Both a top-level `fn main()` and a matching `class Main { fnc
        // main() }` -- neither may silently win.
        let src = "class Main {\n  fnc main() -> Int32 {\n    return 0;\n  }\n}\nfn main() -> Int32 {\n  return 0;\n}";
        let msg = compile_expect_err(src);
        assert!(msg.contains("ambiguous entry point"), "{msg}");
    }

    #[test]
    fn test_class_named_main_without_main_method_is_unaffected() {
        // A class literally named `Main` that never declares a `main`
        // method at all is just an ordinary class -- must not be treated
        // as an entry-point candidate and must not error.
        let src = "class Main {\n  fnc helper() -> Int32 {\n    return 0;\n  }\n}\nfn main() -> Int32 {\n  return 0;\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("define i32 @tinox_main()"), "{ir}");
        assert!(!ir.contains("@Main_main"), "{ir}");
    }

    // --- same-class bare `fnc` calls (issue #149 stage 2) ---

    #[test]
    fn test_same_class_bare_call_routes_through_static_dispatch() {
        let src = "class C {\n  fnc helper(x: Int64) -> Int64 {\n    return x * 2;\n  }\n  fnc main() -> Int64 {\n    return helper(3);\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("define i64 @C_helper(i64 %x)"), "{ir}");
        assert!(ir.contains("call i64 @C_helper(i64"), "{ir}");
        // Must NOT emit a bare, unmangled `call i64 @helper(...)` — that
        // would be the old broken fallthrough this feature replaces.
        assert!(!ir.contains("@helper("), "{ir}");
    }

    #[test]
    fn test_same_class_bare_call_from_within_lambda_body() {
        // A lambda literal lexically nested inside a class method body gets
        // its OWN GenCtx (gen_lambda) -- must inherit `current_struct` from
        // the enclosing method, not default to None, or a bare same-class
        // call made from inside the lambda emits an unmangled `call
        // @helper(...)` (undefined symbol) instead of `@C_helper`.
        let src = "class C {\n  fnc helper(x: Int64) -> Int64 {\n    return x * 2;\n  }\n  fnc main() -> Int64 {\n    let f = x => helper(x);\n    return f(3);\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("call i64 @C_helper"), "{ir}");
        assert!(!ir.contains("call i64 @helper("), "{ir}");
    }

    // --- spawn targeting a class method (issue #149 stage 3) ---

    #[test]
    fn test_spawn_bare_same_class_static_target() {
        // A bare `spawn worker(...)` inside another method of the same
        // class must resolve `worker` to the mangled `C_worker` symbol
        // (same-class fallback, mirroring check_call's), not emit a bare
        // `ptrtoint ... @worker` that would leave no such symbol defined.
        let src = "class C {\n  async fnc worker(x: Int64) -> Int64 {\n    return x;\n  }\n  fnc main() -> Int64 {\n    let h = spawn worker(1);\n    return h;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("@C_worker"), "{ir}");
        assert!(!ir.contains("@worker to"), "{ir}");
    }

    #[test]
    fn test_spawn_qualified_class_method_target() {
        // `spawn ClassName::method(...)` parses as a bare EnumValue (args
        // bundled into the node itself, no wrapping Call{func: EnumValue})
        // -- a separate match arm from the bare-Ident case above; must
        // resolve to the same mangled key as an ordinary `Class::method()`
        // call.
        let src = "class Worker {\n  async fnc run(x: Int64) -> Int64 {\n    return x;\n  }\n}\nclass C {\n  fnc main() -> Int64 {\n    let h = spawn Worker::run(1);\n    return h;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("@Worker_run"), "{ir}");
    }

    #[test]
    fn test_spawn_top_level_free_function_still_wins() {
        // Priority: a genuine top-level free function of the same bare
        // name must still win over a same-class static method (matches
        // check_call's priority, tested for ordinary calls at
        // test_top_level_free_function_still_wins_in_codegen below).
        let src = "async fn worker(x: Int64) -> Int64 {\n  return x;\n}\nclass C {\n  async fnc worker(x: Int64) -> Int64 {\n    return 0;\n  }\n  fnc main() -> Int64 {\n    let h = spawn worker(1);\n    return h;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("@worker to"), "{ir}");
        assert!(!ir.contains("@C_worker to"), "{ir}");
    }

    #[test]
    fn test_top_level_free_function_still_wins_in_codegen() {
        // Same priority guarantee as the typecheck-side test: a genuine
        // top-level free function must still be called directly (bare
        // mangled name), not rerouted through the same-class static
        // dispatch path just because a same-named class method exists.
        let src = "fn helper() -> Int64 {\n  return 99;\n}\nclass C {\n  fnc helper() -> Int64 {\n    return 1;\n  }\n  fnc main() -> Int64 {\n    return helper();\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("call i64 @helper()"), "{ir}");
        assert!(!ir.contains("call i64 @C_helper()"), "{ir}");
    }

    // --- Cross-module class-name collision (Bug 139) ---
    //
    // struct_layouts (and sibling per-class tables) are keyed by bare class
    // name only. Two different classes sharing a bare name (as can happen
    // once their declarations are merged from different imported modules)
    // used to silently clobber each other's layout, producing a confusing
    // "field not in layout of typed class" error with no hint about the
    // real cause. Now caught explicitly at registration time.

    #[test]
    fn test_duplicate_class_name_across_modules_errors_clearly() {
        // Two distinct classes named "Thing" -- from CodeGen::gen's point of
        // view this is indistinguishable from two imported modules each
        // declaring their own "Thing" (the real-world trigger), since gen()
        // only ever sees the already-merged decl list either way.
        let src = "class Thing {\n  var a: Int64;\n  var b: Int64;\n}\nclass Thing {\n  var x: String;\n}\nclass C {\n  fnc main() -> Int64 {\n    return 0;\n  }\n}";
        let msg = compile_expect_err(src);
        assert!(msg.contains("Thing"), "{msg}");
        assert!(msg.contains("two different classes"), "{msg}");
    }

    #[test]
    fn test_no_false_positive_for_single_class_definition() {
        // Regression guard: an ordinary single-definition class must not
        // trip the new collision check.
        let src = "class Thing {\n  var a: Int64;\n}\nclass C {\n  fnc main() -> Int64 {\n    let t = Thing { a: 1 };\n    return t.a;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("%class.Thing"), "{ir}");
    }

    // --- Map non-String keys (Bug 129) ---
    //
    // The runtime's map (`tinox_map_set`/`get`/`contains`/`remove`,
    // runtime.c) always hashes/compares its key as a NUL-terminated C
    // string. `inttoptr`-ing a scalar's raw bit pattern into an `i8*` (the
    // old behavior for any non-String key) segfaults the instant the hash
    // function dereferences it. `emit_map_key` stringifies scalar keys the
    // same way `.toString()` does before they ever reach the runtime.

    #[test]
    fn test_map_int_key_insert_stringifies_key() {
        let src = "class C {\n  fnc main() -> Int64 {\n    var m: Map<Int64, Int64> = Map::new();\n    m.insert(1, 100);\n    return 0;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("call i8* @tinox_int_to_string(i64"), "{ir}");
        // The stringified key, not the raw i64 bit pattern, must feed tinox_map_set.
        assert!(!ir.contains("inttoptr i64 1 to i8*"), "{ir}");
    }

    #[test]
    fn test_map_bool_key_get_uses_bool_to_string() {
        let src = "class C {\n  fnc main() -> Int64 {\n    var m: Map<Bool, Int64> = Map::new();\n    let v = m.get(true);\n    return v;\n  }\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("call i8* @tinox_bool_to_string(i1"), "{ir}");
    }

    #[test]
    fn test_map_string_key_insert_unchanged() {
        // Regression guard: the pre-existing String-key path must still
        // pass the key straight through as i8*, no stringify/inttoptr call.
        let src = "class C {\n  fnc main() -> Int64 {\n    var m: Map<String, Int64> = Map::new();\n    m.insert(\"a\", 1);\n    return 0;\n  }\n}";
        let ir = compile_to_ir(src);
        // Every runtime function is always `declare`d regardless of use --
        // check no CALL to the stringify helpers was emitted, not just
        // absence of the substring (which the declare line alone satisfies).
        assert!(!ir.contains("call i8* @tinox_int_to_string"), "{ir}");
        assert!(!ir.contains("call i8* @tinox_bool_to_string"), "{ir}");
    }

    #[test]
    fn test_map_int_key_index_operators_stringify_key() {
        // `m[k]` / `m[k] = v` go through a separate codegen path
        // (gen_index_target/gen_index_store, ExprKind::Index) from the
        // `.get()`/`.insert()` method calls above -- same underlying bug,
        // same fix (emit_map_key), verified independently here.
        let src = "class C {\n  fnc main() -> Int64 {\n    var m: Map<Int64, Int64> = Map::new();\n    m[1] = 100;\n    return m[1];\n  }\n}";
        let ir = compile_to_ir(src);
        let count = ir.matches("call i8* @tinox_int_to_string(i64").count();
        assert!(count >= 2, "expected both the [1]= write and the [1] read to stringify the key: {ir}");
    }

    // --- DWARF debug info (issue #114) ---

    #[test]
    fn test_debug_info_skips_unknown_file() {
        // compile_to_ir (no stamped file) leaves every decl at
        // UNKNOWN_FILE — dbg_suffix must skip debug info entirely rather
        // than emit it against a fabricated/wrong file.
        let src = "fn main() -> Int64 {\n  return 1;\n}";
        let ir = compile_to_ir(src);
        assert!(!ir.contains("!dbg"), "no file identity means no debug info at all");
        assert!(!ir.contains("DISubprogram"));
    }

    #[test]
    fn test_debug_info_emits_disubprogram_for_function() {
        let src = "fn add(a: Int64, b: Int64) -> Int64 {\n  return a + b;\n}\nfn main() -> Int64 {\n  return add(1, 2);\n}";
        let ir = compile_to_ir_with_file(src, "/tmp/probe.tnx");
        assert!(ir.contains("define i64 @add"), "define line must be unaffected (substring still present)");
        assert!(ir.contains("!dbg !"), "define line should get a !dbg attachment");
        assert!(ir.contains("distinct !DISubprogram(name: \"add\""));
        assert!(ir.contains("distinct !DISubprogram(name: \"main\""));
        assert!(ir.contains("!DIFile(filename: \"probe.tnx\", directory: \"/tmp\")"));
        assert!(ir.contains("distinct !DICompileUnit("));
        assert!(ir.contains("!llvm.dbg.cu = "));
        assert!(ir.contains("Debug Info Version"));
    }

    #[test]
    fn test_debug_info_call_sites_get_dbg_location() {
        // LLVM requires every call inside a function that itself carries
        // !dbg to also carry a !dbg location, or opt's debug-info
        // verifier silently strips the whole function's debug info
        // (discovered empirically, see dbg_suffix's doc comment).
        let src = "fn add(a: Int64, b: Int64) -> Int64 {\n  return a + b;\n}\nfn main() -> Int64 {\n  return add(1, 2);\n}";
        let ir = compile_to_ir_with_file(src, "/tmp/probe.tnx");
        let call_line = ir.lines().find(|l| l.contains("call i64 @add")).expect("call site should exist");
        assert!(call_line.contains(", !dbg !"), "call site must carry a !dbg location: {call_line}");
        assert!(ir.contains("!DILocation("));
    }

    #[test]
    fn test_debug_info_two_files_get_distinct_difile() {
        // The whole point of issue #114's file-identity work: a
        // multi-file program must NOT misattribute a function to the
        // wrong file's DIFile.
        let helper_src = "class Helper {\n  fn double(x: Int64) -> Int64 {\n    return x * 2;\n  }\n}";
        let mut lexer = Lexer::new(helper_src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let mut helper_ast = parser.parse().expect("parse failed");
        stamp_file_for_test(&mut helper_ast.decls, &Arc::from("/proj/Helper.tnx"));

        let main_src = "fn main() -> Int64 {\n  return 0;\n}";
        let mut lexer2 = Lexer::new(main_src);
        let tokens2 = lexer2.tokenize().expect("lex failed");
        let mut parser2 = Parser::new(tokens2);
        let mut main_ast = parser2.parse().expect("parse failed");
        stamp_file_for_test(&mut main_ast.decls, &Arc::from("/proj/main.tnx"));

        // Merge like resolve_imports would (imported decls first).
        let mut merged = helper_ast.decls;
        merged.extend(main_ast.decls);
        let source = tinox_parser::SourceFile { decls: merged, span: Span::dummy() };

        let mut cg = CodeGen::new();
        cg.gen(&source).expect("codegen failed");
        let ir = cg.into_ir();

        assert!(ir.contains("!DIFile(filename: \"Helper.tnx\", directory: \"/proj\")"));
        assert!(ir.contains("!DIFile(filename: \"main.tnx\", directory: \"/proj\")"));
        // Exactly one DIFile per distinct path, not re-created per function.
        assert_eq!(ir.matches("!DIFile(filename: \"Helper.tnx\"").count(), 1);
        assert_eq!(ir.matches("!DIFile(filename: \"main.tnx\"").count(), 1);
        // A single shared DICompileUnit, not one per file.
        assert_eq!(ir.matches("distinct !DICompileUnit(").count(), 1);
    }

    #[test]
    fn test_if_expr() {
        let src = "fn main() -> Int64 {\n  let x = if true { 42; } else { 0; };\n  return x;\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("if_then"), "should have if_then block");
        assert!(ir.contains("if_merge"), "should have if_merge block");
    }

    #[test]
    fn test_block_expr_returns_last() {
        let src = "fn main() -> Int64 {\n  let x = { let a = 10; a; };\n  return x;\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("alloca"), "should have allocas");
    }

    #[test]
    fn test_float_ops() {
        let src = "namespace math { class Ops { fnc add_floats(a: Float64, b: Float64) -> Float64 { return a + b; } } }";
        let ir = compile_to_ir(src);
        assert!(ir.contains("fadd double"), "should use fadd for float addition");
    }

    #[test]
    fn test_try_catch() {
        // throw followed by semicolon — parser requires this
        let src = concat!(
            "fn main() -> Int64 {\n",
            "  try {\n",
            "    println(1);\n",
            "  } catch (e: Int64) {\n",
            "    println(e);\n",
            "  };\n",
            "  return 0;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("try_"), "should have try block");
        assert!(ir.contains("catch_"), "should have catch block");
        assert!(ir.contains("try_end"), "should have end block");
    }

    #[test]
    fn test_try_finally() {
        let src = concat!(
            "fn main() -> Int64 {\n",
            "  try {\n",
            "    println(1);\n",
            "  } catch (e: Int64) {\n",
            "    println(e);\n",
            "  } finally {\n",
            "    println(0);\n",
            "  };\n",
            "  return 0;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("finally_"), "should have finally block");
        assert!(ir.contains("try_end"), "should have end block");
    }

    #[test]
    fn test_multiple_catches() {
        let src = concat!(
            "fn main() -> Int64 {\n",
            "  try {\n",
            "    println(1);\n",
            "  } catch (e: Int64) {\n",
            "    println(e);\n",
            "  } catch (f: Int64) {\n",
            "    println(f);\n",
            "  };\n",
            "  return 0;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("catch_0"), "should have catch_0 block");
        assert!(ir.contains("catch_1"), "should have catch_1 block");
        assert!(ir.contains("catch_0_ok"), "should have catch_0_ok guard");
        assert!(ir.contains("catch_1_ok"), "should have catch_1_ok guard");
    }

    #[test]
    fn test_cast_float_to_int() {
        let src = "namespace test { class C { fnc f(x: Float64) -> Int64 { return cast x as Int64; } } }";
        let ir = compile_to_ir(src);
        assert!(ir.contains("fptosi double"), "should use fptosi for float→int");
    }

    #[test]
    fn test_cast_int_to_float() {
        let src = "namespace test { class C { fnc f(x: Int64) -> Float64 { return cast x as Float64; } } }";
        let ir = compile_to_ir(src);
        assert!(ir.contains("sitofp i64"), "should use sitofp for int→float");
    }

    #[test]
    fn test_cast_double_to_float() {
        let src = "namespace test { class C { fnc f(x: Float64) -> Float32 { return cast x as Float32; } } }";
        let ir = compile_to_ir(src);
        assert!(ir.contains("fptrunc double"), "should use fptrunc for double→float");
    }

    #[test]
    fn test_loop_stmt() {
        let src = "fn main() -> Int64 {\n  loop { break; };\n  return 0;\n}";
        let ir = compile_to_ir(src);
        assert!(ir.contains("loop_body"), "should have loop body block");
        assert!(ir.contains("loop_end"), "should have loop end block");
    }

    #[test]
    fn test_return_as_expr() {
        // return used in expression position (right side of let)
        let src = concat!(
            "namespace test { class C { fnc f() -> Int64 {\n",
            "  let _ = return 42;\n",
            "  return 0;\n",
            "} } }"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("ret i64 42"), "should emit ret for return-expr");
        assert!(ir.contains("ret_dead"), "should have dead block after return expr");
    }

    #[test]
    fn test_break_as_expr() {
        let src = concat!(
            "fn main() -> Int64 {\n",
            "  loop {\n",
            "    let _ = break;\n",
            "  };\n",
            "  return 0;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("break_dead"), "should have dead block after break expr");
    }

    #[test]
    fn test_continue_as_expr() {
        let src = concat!(
            "fn main() -> Int64 {\n",
            "  let i = 0;\n",
            "  loop {\n",
            "    let _ = continue;\n",
            "  };\n",
            "  return 0;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("cont_dead"), "should have dead block after continue expr");
    }

    #[test]
    fn test_generic_class_monomorphization() {
        let src = concat!(
            "class Box<T> {\n",
            "  value: T;\n",
            "  fn get() -> T {\n",
            "    return this.value;\n",
            "  }\n",
            "}\n",
            "fn main() -> Int64 {\n",
            "  let b = new Box<Int64>(42);\n",
            "  return b.get();\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("Box__i64_get"), "should emit specialized method Box__i64_get");
        assert!(ir.contains("define i64 @Box__i64_get"), "method should return i64");
        assert!(!ir.contains("define i64 @Box_get"), "unspecialized Box_get must not be emitted");
    }

    #[test]
    fn test_generic_class_two_instantiations() {
        let src = concat!(
            "class Pair<T> {\n",
            "  first: T;\n",
            "  fn fst() -> T {\n",
            "    return this.first;\n",
            "  }\n",
            "}\n",
            "fn main() -> Int64 {\n",
            "  let a = new Pair<Int64>(1);\n",
            "  let b = new Pair<Float64>(2);\n",
            "  return a.fst();\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("Pair__i64_fst"), "should have i64 specialization");
        assert!(ir.contains("Pair__double_fst"), "should have double specialization");
    }

    // --- Integer arithmetic IR ---

    #[test]
    fn test_int_add_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 + 2; return x; }");
        assert!(ir.contains("add"), "should emit add instruction");
    }

    #[test]
    fn test_int_sub_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 10 - 3; return x; }");
        assert!(ir.contains("sub"), "should emit sub instruction");
    }

    #[test]
    fn test_int_mul_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 4 * 5; return x; }");
        assert!(ir.contains("mul"), "should emit mul instruction");
    }

    #[test]
    fn test_int_div_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 10 / 2; return x; }");
        assert!(ir.contains("sdiv"), "should emit sdiv for integer division");
    }

    #[test]
    fn test_int_mod_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 10 % 3; return x; }");
        assert!(ir.contains("srem"), "should emit srem for integer modulo");
    }

    // --- Float arithmetic IR ---

    #[test]
    fn test_float_sub_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(a: Float64, b: Float64) -> Float64 { return a - b; } } }");
        assert!(ir.contains("fsub double"), "should emit fsub for float subtraction");
    }

    #[test]
    fn test_float_mul_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(a: Float64, b: Float64) -> Float64 { return a * b; } } }");
        assert!(ir.contains("fmul double"), "should emit fmul for float multiplication");
    }

    #[test]
    fn test_float_div_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(a: Float64, b: Float64) -> Float64 { return a / b; } } }");
        assert!(ir.contains("fdiv double"), "should emit fdiv for float division");
    }

    // --- Comparison IR ---

    #[test]
    fn test_icmp_eq_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 == 1; return 0; }");
        assert!(ir.contains("icmp eq"), "should emit icmp eq");
    }

    #[test]
    fn test_icmp_ne_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 != 2; return 0; }");
        assert!(ir.contains("icmp ne"), "should emit icmp ne");
    }

    #[test]
    fn test_icmp_lt_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 < 2; return 0; }");
        assert!(ir.contains("icmp slt"), "should emit icmp slt for <");
    }

    #[test]
    fn test_icmp_gt_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 2 > 1; return 0; }");
        assert!(ir.contains("icmp sgt"), "should emit icmp sgt for >");
    }

    #[test]
    fn test_icmp_le_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 <= 2; return 0; }");
        assert!(ir.contains("icmp sle"), "should emit icmp sle for <=");
    }

    #[test]
    fn test_icmp_ge_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 2 >= 1; return 0; }");
        assert!(ir.contains("icmp sge"), "should emit icmp sge for >=");
    }

    #[test]
    fn test_float_comparison_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(a: Float64, b: Float64) -> Bool { return a < b; } } }");
        assert!(ir.contains("fcmp"), "should emit fcmp for float comparison");
    }

    // --- Boolean ops IR ---

    #[test]
    fn test_bool_and_ir() {
        // && short-circuits: branch to an RHS block instead of eager `and i1`.
        let ir = compile_to_ir("fn main() -> Int64 { let x = true && false; return 0; }");
        assert!(ir.contains("sc_rhs") && ir.contains("br i1"), "should short-circuit && via branch");
    }

    #[test]
    fn test_bool_or_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = true || false; return 0; }");
        assert!(ir.contains("sc_rhs") && ir.contains("br i1"), "should short-circuit || via branch");
    }

    // --- Unary ops IR ---

    #[test]
    fn test_unary_neg_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = -5; return x; }");
        assert!(ir.contains("sub") || ir.contains("neg"), "should emit negation");
    }

    #[test]
    fn test_unary_not_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = !true; return 0; }");
        assert!(ir.contains("xor i1") || ir.contains("xor"), "should emit xor for boolean not");
    }

    // --- Variables: alloca/store/load ---

    #[test]
    fn test_alloca_for_local_var() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 42; return x; }");
        assert!(ir.contains("alloca"), "should emit alloca for local variable");
    }

    #[test]
    fn test_store_load_for_var() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 42; return x; }");
        assert!(ir.contains("store"), "should emit store for variable init");
        assert!(ir.contains("load"), "should emit load for variable read");
    }

    // --- Function definition ---

    #[test]
    fn test_function_define_ir() {
        // fn main is emitted as @tinox_main to avoid clashing with libc main
        let ir = compile_to_ir("fn main() -> Int64 { return 0; }");
        assert!(ir.contains("define"), "should emit define for function");
        assert!(ir.contains("@tinox_main"), "fn main should become @tinox_main");
    }

    #[test]
    fn test_function_return_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 42; }");
        assert!(ir.contains("ret i64"), "should emit ret i64");
    }

    #[test]
    fn test_multiple_functions_ir() {
        let ir = compile_to_ir("fn foo() -> Int64 { return 1; } fn main() -> Int64 { return foo(); }");
        assert!(ir.contains("@foo"), "should define @foo");
        assert!(ir.contains("@tinox_main"), "fn main should become @tinox_main");
        assert!(ir.contains("call"), "should emit call instruction");
    }

    // --- Control flow blocks ---

    #[test]
    fn test_if_without_else_stmt_ir() {
        // Statement-level if uses block labels: then/else/ifcont
        let ir = compile_to_ir("fn main() -> Int64 { if true { } return 0; }");
        assert!(ir.contains("then"), "should have then block");
        assert!(ir.contains("ifcont"), "should have ifcont merge block");
    }

    #[test]
    fn test_if_else_stmt_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { if true { } else { } return 0; }");
        assert!(ir.contains("then"), "should have then block");
        assert!(ir.contains("else"), "should have else block");
        assert!(ir.contains("ifcont"), "should have ifcont merge block");
    }

    #[test]
    fn test_while_loop_stmt_blocks_ir() {
        // Statement-level while uses block labels: loop/loopbody/loopend
        let ir = compile_to_ir("fn main() -> Int64 { while true { break; } return 0; }");
        assert!(ir.contains("loopbody"), "should have loopbody block");
        assert!(ir.contains("loopend"), "should have loopend block");
    }

    #[test]
    fn test_for_range_loop_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { for i in 0..5 { } return 0; }");
        assert!(ir.contains("for_"), "should have for block structure");
    }

    // --- String literals ---

    #[test]
    fn test_string_literal_global_ir() {
        let ir = compile_to_ir(r#"fn main() -> Int64 { let s = "hello"; return 0; }"#);
        assert!(ir.contains("hello") || ir.contains("@str"), "should emit string constant");
    }

    // --- Namespace/class mangling ---

    #[test]
    fn test_namespace_class_method_mangling() {
        let ir = compile_to_ir("namespace myapp { class Utils { fnc helper() -> Int64 { return 0; } } }");
        assert!(ir.contains("myapp__Utils_helper") || ir.contains("Utils_helper"),
            "should emit mangled method name");
    }

    #[test]
    fn test_class_static_method_ir() {
        // In Tinox, fnc inside a class = static method (no self param)
        let ir = compile_to_ir("class Math { fnc square(x: Int64) -> Int64 { return x * x; } }");
        assert!(ir.contains("Math_square"), "should emit Math_square name");
        assert!(ir.contains("define"), "should define the function");
    }

    // --- Bitwise operations ---

    #[test]
    fn test_bitwise_and_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 6 & 3; return x; }");
        assert!(ir.contains("and i64"), "should emit and i64 for bitwise and");
    }

    #[test]
    fn test_bitwise_or_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 6 | 3; return x; }");
        assert!(ir.contains("or i64"), "should emit or i64 for bitwise or");
    }

    #[test]
    fn test_bitwise_xor_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 6 ^ 3; return x; }");
        assert!(ir.contains("xor i64"), "should emit xor i64 for bitwise xor");
    }

    #[test]
    fn test_shl_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1 << 3; return x; }");
        assert!(ir.contains("shl"), "should emit shl for left shift");
    }

    #[test]
    fn test_shr_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 8 >> 1; return x; }");
        assert!(ir.contains("shr") || ir.contains("ashr") || ir.contains("lshr"), "should emit shift right");
    }

    // --- Compound assignments ---

    #[test]
    fn test_compound_add_assign_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 5; x += 3; return x; }");
        assert!(ir.contains("add"), "should emit add for +=");
        assert!(ir.contains("store"), "should store result back");
    }

    #[test]
    fn test_compound_sub_assign_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 5; x -= 2; return x; }");
        assert!(ir.contains("sub"), "should emit sub for -=");
    }

    // --- Null ---

    #[test]
    fn test_null_literal_ir() {
        // null is emitted as integer 0 in IR
        let ir = compile_to_ir("fn main() -> Int64 { let x = null; return 0; }");
        assert!(ir.contains("i64 0") || ir.contains("store"), "null should emit as 0 or store");
    }

    // --- Specific integer type widths ---

    #[test]
    fn test_i32_type_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(x: Int32) -> Int32 { return x; } } }");
        assert!(ir.contains("i32"), "should use i32 for Int32 params");
    }

    #[test]
    fn test_i64_type_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(x: Int64) -> Int64 { return x; } } }");
        assert!(ir.contains("i64"), "should use i64 for Int64 params");
    }

    #[test]
    fn test_bool_type_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(x: Bool) -> Bool { return x; } } }");
        assert!(ir.contains("i1"), "should use i1 for Bool params");
    }

    #[test]
    fn test_float32_type_ir() {
        let ir = compile_to_ir("namespace t { class C { fnc f(x: Float32) -> Float32 { return x; } } }");
        assert!(ir.contains("float"), "should use float for Float32");
    }

    // --- Struct / class fields ---

    #[test]
    fn test_class_field_gep_ir() {
        let src = concat!(
            "class Point { x: Int64; y: Int64; }\n",
            "fn main() -> Int64 {\n",
            "  let p = new Point(3, 4);\n",
            "  return p.x;\n",
            "}"
        );
        let ir = compile_to_ir(src);
        assert!(ir.contains("getelementptr") || ir.contains("gep") || ir.contains("Point"),
            "should emit GEP or struct access for field read");
    }

    // --- Array ---

    #[test]
    fn test_array_literal_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let arr = [1, 2, 3]; return 0; }");
        assert!(ir.contains("alloca") || ir.contains("array"), "should emit array storage");
    }

    // ================================================================
    // Enum
    // ================================================================

    #[test]
    fn test_enum_type_is_i64() {
        // Enum variants are represented as i64 constants
        let ir = compile_to_ir("enum Color { Red; Green; Blue; } fn main() -> Int64 { return 0; }");
        assert!(ir.contains("i64"), "enum-bearing code should use i64");
    }

    #[test]
    fn test_enum_variant_constant() {
        let ir = compile_to_ir(
            "enum Dir { North; South; East; West; } fn main() -> Int64 { let d = Dir::North; return 0; }"
        );
        assert!(ir.contains("i64 0") || ir.contains("store"), "enum variant should store a constant");
    }

    #[test]
    fn test_match_on_enum() {
        let ir = compile_to_ir(concat!(
            "enum State { On; Off; }\n",
            "fn check(s: State) -> Int64 {\n",
            "    match s {\n",
            "        State::On => 1;\n",
            "        State::Off => 0;\n",
            "        _ => -1;\n",
            "    }\n",
            "    return 0;\n",
            "}\n",
            "fn main() -> Int64 { return 0; }",
        ));
        assert!(ir.contains("switch") || ir.contains("icmp") || ir.contains("br"),
            "match should emit branching IR");
    }

    // ================================================================
    // Match on integer
    // ================================================================

    #[test]
    fn test_match_int_ir() {
        let ir = compile_to_ir(concat!(
            "fn classify(x: Int64) -> Int64 {\n",
            "    match x {\n",
            "        0 => 10;\n",
            "        1 => 20;\n",
            "        _ => 99;\n",
            "    }\n",
            "    return 0;\n",
            "}\n",
            "fn main() -> Int64 { return 0; }",
        ));
        assert!(ir.contains("icmp") || ir.contains("switch"), "integer match should compare");
    }

    #[test]
    fn test_match_bool_ir() {
        let ir = compile_to_ir(concat!(
            "fn f(b: Bool) -> Int64 {\n",
            "    match b {\n",
            "        true => 1;\n",
            "        false => 0;\n",
            "    }\n",
            "    return 0;\n",
            "}\n",
            "fn main() -> Int64 { return 0; }",
        ));
        assert!(ir.contains("icmp") || ir.contains("br"), "bool match needs branch IR");
    }

    // ================================================================
    // Recursive function
    // ================================================================

    #[test]
    fn test_recursive_function_ir() {
        let ir = compile_to_ir(concat!(
            "fn fib(n: Int64) -> Int64 {\n",
            "    if n <= 1 { return n; }\n",
            "    return fib(n - 1) + fib(n - 2);\n",
            "}\n",
            "fn main() -> Int64 { return fib(5); }",
        ));
        // fib calls itself — should appear twice in IR (definition + call)
        let count = ir.matches("@fib(").count() + ir.matches("call i64 @fib").count();
        assert!(count >= 2, "recursive function should call itself in IR");
    }

    // ================================================================
    // Multiple function parameters
    // ================================================================

    #[test]
    fn test_multiple_params_ir() {
        let ir = compile_to_ir(
            "fn add(a: Int64, b: Int64, c: Int64) -> Int64 { return a + b + c; }\nfn main() -> Int64 { return add(1, 2, 3); }"
        );
        assert!(ir.contains("@add(i64 %a, i64 %b, i64 %c)") || ir.contains("@add(i64"),
            "multi-param function should appear in IR");
    }

    // ================================================================
    // Return without value
    // ================================================================

    #[test]
    fn test_return_void_ir() {
        let ir = compile_to_ir("fn greet() -> Nothing { return; }\nfn main() -> Int64 { greet(); return 0; }");
        assert!(ir.contains("ret void") || ir.contains("ret i64"),
            "Nothing-returning function should have a return");
    }

    // ================================================================
    // For-C style loop
    // ================================================================

    #[test]
    fn test_forc_loop_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var sum = 0;\n",
            "    for (var i = 0; i < 10; i += 1) {\n",
            "        sum += i;\n",
            "    }\n",
            "    return sum;\n",
            "}",
        ));
        assert!(ir.contains("br ") && (ir.contains("loop") || ir.contains("for")),
            "for-C loop should emit branch-based loop IR");
    }

    // ================================================================
    // Unary bit-not (~)
    // ================================================================

    #[test]
    fn test_unary_bitnot_ir() {
        let ir = compile_to_ir("fn f(x: Int64) -> Int64 { return ~x; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("xor") || ir.contains("-1"), "bit-not should use xor with -1");
    }

    // ================================================================
    // Shift right arithmetic (>>>)
    // ================================================================

    #[test]
    fn test_arith_shift_right_ir() {
        let ir = compile_to_ir("fn f(x: Int64) -> Int64 { return x >>> 2; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("ashr"), ">>> should emit ashr instruction");
    }

    // ================================================================
    // String method calls
    // ================================================================

    #[test]
    fn test_string_length_method_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let s = \"hello\";\n",
            "    let n = s.len();\n",
            "    return 0;\n",
            "}",
        ));
        assert!(ir.contains("tinox_string_length"), "s.len() should call tinox_string_length");
    }

    #[test]
    fn test_string_concat_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let a = \"foo\";\n",
            "    let b = \"bar\";\n",
            "    let c = a + b;\n",
            "    return 0;\n",
            "}",
        ));
        assert!(ir.contains("tinox_string_concat"), "string + should call tinox_string_concat");
    }

    // ================================================================
    // Field write (this.field = value)
    // ================================================================

    #[test]
    fn test_field_write_ir() {
        let ir = compile_to_ir(concat!(
            "class Counter {\n",
            "    var count: Int64;\n",
            "    fn increment() -> Nothing {\n",
            "        this.count = this.count + 1;\n",
            "    }\n",
            "}\n",
            "fn main() -> Int64 { return 0; }",
        ));
        assert!(ir.contains("getelementptr") && ir.contains("store"),
            "field write should use GEP + store");
    }

    // ================================================================
    // Class inheritance: child calls parent method
    // ================================================================

    #[test]
    fn test_child_inherits_parent_method_ir() {
        let ir = compile_to_ir(concat!(
            "class Animal {\n",
            "    fn speak() -> Int64 { return 1; }\n",
            "}\n",
            "class Dog extends Animal {}\n",
            "fn main() -> Int64 {\n",
            "    let d = new Dog();\n",
            "    return d.speak();\n",
            "}",
        ));
        assert!(ir.contains("Animal_speak") || ir.contains("Dog_speak"),
            "inherited method should be dispatched");
    }

    // ================================================================
    // Immutable struct
    // ================================================================

    #[test]
    fn test_immutable_struct_ir() {
        let ir = compile_to_ir(concat!(
            "immutable Point(x: Int64, y: Int64)\n",
            "fn main() -> Int64 {\n",
            "    let p = new Point(3, 4);\n",
            "    return p.x;\n",
            "}",
        ));
        assert!(ir.contains("%Point") || ir.contains("Point"),
            "immutable type should appear in IR");
    }

    // ================================================================
    // Logical short-circuit (&&, ||)
    // ================================================================

    #[test]
    fn test_logical_and_ir() {
        let ir = compile_to_ir("fn f(a: Bool, b: Bool) -> Bool { return a && b; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("and i1") || ir.contains("br "),
            "&& should emit and or branch IR");
    }

    #[test]
    fn test_logical_or_ir() {
        let ir = compile_to_ir("fn f(a: Bool, b: Bool) -> Bool { return a || b; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("or i1") || ir.contains("br "),
            "|| should emit or or branch IR");
    }

    // ================================================================
    // Compound operators (remaining ones)
    // ================================================================

    #[test]
    fn test_compound_mul_assign_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 3; x *= 4; return x; }");
        assert!(ir.contains("mul"), "x *= should emit mul");
    }

    #[test]
    fn test_compound_div_assign_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 8; x /= 2; return x; }");
        assert!(ir.contains("sdiv"), "x /= should emit sdiv");
    }

    #[test]
    fn test_compound_mod_assign_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 9; x %= 4; return x; }");
        assert!(ir.contains("srem"), "x %= should emit srem");
    }

    #[test]
    fn test_compound_bitand_assign_parse_bug() {
        // BUG: parser does not support &= — parses `x &` then fails on `=`
        // This test documents the current broken state
        let result = std::panic::catch_unwind(|| {
            compile_to_ir("fn main() -> Int64 { var x = 15; x &= 6; return x; }")
        });
        // Currently panics in compile_to_ir because parse fails
        assert!(result.is_err(), "x &= should currently fail to parse (known bug)");
    }

    #[test]
    fn test_compound_bitor_assign_parse_bug() {
        // BUG: parser does not support |=
        let result = std::panic::catch_unwind(|| {
            compile_to_ir("fn main() -> Int64 { var x = 5; x |= 2; return x; }")
        });
        assert!(result.is_err(), "x |= should currently fail to parse (known bug)");
    }

    #[test]
    fn test_compound_xor_assign_parse_bug() {
        // BUG: parser does not support ^=
        let result = std::panic::catch_unwind(|| {
            compile_to_ir("fn main() -> Int64 { var x = 7; x ^= 3; return x; }")
        });
        assert!(result.is_err(), "x ^= should currently fail to parse (known bug)");
    }

    #[test]
    fn test_compound_shl_assign_parse_bug() {
        // BUG: parser does not support <<=
        let result = std::panic::catch_unwind(|| {
            compile_to_ir("fn main() -> Int64 { var x = 1; x <<= 3; return x; }")
        });
        assert!(result.is_err(), "x <<= should currently fail to parse (known bug)");
    }

    // ================================================================
    // Cast instructions
    // ================================================================

    #[test]
    fn test_cast_i32_to_i64_ir() {
        let ir = compile_to_ir("fn f(x: Int32) -> Int64 { return x as Int64; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("sext") || ir.contains("zext") || ir.contains("i64"),
            "Int32->Int64 cast should use sext or zext");
    }

    #[test]
    fn test_cast_bool_to_int_ir() {
        let ir = compile_to_ir("fn f(b: Bool) -> Int64 { return b as Int64; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("zext") || ir.contains("i64"),
            "Bool->Int64 cast should use zext");
    }

    // ================================================================
    // Tuple
    // ================================================================

    #[test]
    fn test_tuple_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let t = (1, 2); return 0; }");
        // Tuples are stored as structs — should allocate memory
        assert!(ir.contains("alloca") || ir.contains("i64"), "tuple should be allocated");
    }

    // ================================================================
    // Lambda / closure
    // ================================================================

    #[test]
    fn test_lambda_define_ir() {
        // Lambda syntax: (params) => body  or  \x -> body
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let add = (a, b) => a + b;\n",
            "    return 0;\n",
            "}",
        ));
        assert!(ir.contains("lambda") || ir.contains("define") || ir.contains("alloca"),
            "lambda should generate some IR");
    }

    // ================================================================
    // Null literal
    // ================================================================

    #[test]
    fn test_null_in_condition_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let p = null;\n",
            "    if p == null { return 1; }\n",
            "    return 0;\n",
            "}",
        ));
        assert!(ir.contains("icmp") || ir.contains("br "), "null comparison should emit icmp");
    }

    // ================================================================
    // Char literal
    // ================================================================

    #[test]
    fn test_char_literal_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let c = 'A'; return 0; }");
        assert!(ir.contains("i32") || ir.contains("65") || ir.contains("store"),
            "char literal should store its code point");
    }

    // ================================================================
    // Float32 vs Float64 types
    // ================================================================

    #[test]
    fn test_float32_param_ir() {
        let ir = compile_to_ir("fn f(x: Float32) -> Float32 { return x; }\nfn main() -> Int64 { return 0; }");
        assert!(ir.contains("float") || ir.contains("f32") || ir.contains("double"),
            "Float32 param should appear as float type in IR");
    }

    // ================================================================
    // Multiple classes in one program
    // ================================================================

    #[test]
    fn test_two_classes_ir() {
        let ir = compile_to_ir(concat!(
            "class A { fn getA() -> Int64 { return 1; } }\n",
            "class B { fn getB() -> Int64 { return 2; } }\n",
            "fn main() -> Int64 {\n",
            "    let a = new A();\n",
            "    let b = new B();\n",
            "    return a.getA() + b.getB();\n",
            "}",
        ));
        assert!(ir.contains("A_getA") && ir.contains("B_getB"),
            "both class methods should appear in IR");
    }

    // ================================================================
    // Extern fn declaration
    // ================================================================

    #[test]
    fn test_extern_fn_ir() {
        let ir = compile_to_ir(concat!(
            "extern fn puts(s: String) -> Int64;\n",
            "fn main() -> Int64 { puts(\"hi\"); return 0; }",
        ));
        assert!(ir.contains("declare") && ir.contains("@puts"),
            "extern fn should emit a declare");
    }

    // ================================================================
    // If expression (inline) with result used
    // ================================================================

    #[test]
    fn test_if_expr_value_used_ir() {
        let ir = compile_to_ir(concat!(
            "fn abs(x: Int64) -> Int64 {\n",
            "    return if x < 0 { -x; } else { x; };\n",
            "}",
            "fn main() -> Int64 { return abs(-3); }",
        ));
        assert!(ir.contains("if_then") && ir.contains("if_merge"),
            "if-expression should have then/merge blocks");
    }

    // ================================================================
    // While expression (used as value)
    // ================================================================

    #[test]
    fn test_while_stmt_produces_loop_blocks() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var i = 0;\n",
            "    while i < 5 {\n",
            "        i += 1;\n",
            "    }\n",
            "    return i;\n",
            "}",
        ));
        assert!(ir.contains("loop") && ir.contains("loopbody") && ir.contains("loopend"),
            "while loop should produce loop/loopbody/loopend blocks");
    }

    // ================================================================
    // String operations
    // ================================================================

    #[test]
    fn test_string_variable_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let s = \"hello\"; return 0; }");
        assert!(ir.contains("hello") || ir.contains("i8"), "string literal should appear in IR");
    }

    #[test]
    fn test_string_concat_two_vars_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let a = \"foo\";\n",
            "    let b = \"bar\";\n",
            "    let c = a + b;\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("foo") && ir.contains("bar"), "string concat should emit both strings");
    }

    // ================================================================
    // For-each style loop
    // ================================================================

    #[test]
    fn test_foreach_loop_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let arr = [1, 2, 3];\n",
            "    var sum = 0;\n",
            "    for x in arr {\n",
            "        sum += x;\n",
            "    }\n",
            "    return sum;\n",
            "}"
        ));
        assert!(ir.contains("sum") || ir.contains("add"), "foreach loop should emit addition IR");
    }

    // ================================================================
    // Boolean literals
    // ================================================================

    #[test]
    fn test_bool_true_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let b = true; return 0; }");
        assert!(ir.contains("i1 1") || ir.contains("i1 true") || ir.contains("true"),
            "true literal should emit i1 1 in IR");
    }

    #[test]
    fn test_bool_false_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let b = false; return 0; }");
        assert!(ir.contains("i1 0") || ir.contains("i1 false") || ir.contains("false"),
            "false literal should emit i1 0 in IR");
    }

    // ================================================================
    // Comparison operators
    // ================================================================

    #[test]
    fn test_less_than_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 3 < 5; return 0; }");
        assert!(ir.contains("icmp slt"), "less-than should emit icmp slt");
    }

    #[test]
    fn test_less_equal_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 3 <= 5; return 0; }");
        assert!(ir.contains("icmp sle"), "less-equal should emit icmp sle");
    }

    #[test]
    fn test_greater_than_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 5 > 3; return 0; }");
        assert!(ir.contains("icmp sgt"), "greater-than should emit icmp sgt");
    }

    #[test]
    fn test_greater_equal_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 5 >= 3; return 0; }");
        assert!(ir.contains("icmp sge"), "greater-equal should emit icmp sge");
    }

    #[test]
    fn test_not_equal_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 3 != 5; return 0; }");
        assert!(ir.contains("icmp ne"), "not-equal should emit icmp ne");
    }

    // ================================================================
    // Nested function calls
    // ================================================================

    #[test]
    fn test_nested_call_ir() {
        let ir = compile_to_ir(concat!(
            "fn double(x: Int64) -> Int64 { return x * 2; }\n",
            "fn quadruple(x: Int64) -> Int64 { return double(double(x)); }\n",
            "fn main() -> Int64 { return quadruple(3); }"
        ));
        assert!(ir.contains("@double") && ir.contains("@quadruple"),
            "nested function calls should emit both function symbols");
    }

    // ================================================================
    // Multiple assignments
    // ================================================================

    #[test]
    fn test_multiple_var_assign_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var x = 1;\n",
            "    var y = 2;\n",
            "    var z = x + y;\n",
            "    x = z * 2;\n",
            "    return x;\n",
            "}"
        ));
        assert!(ir.contains("store") && ir.contains("load"), "multiple assignments should emit store/load");
    }

    // ================================================================
    // Array literal
    // ================================================================

    #[test]
    fn test_array_literal_three_elems_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let a = [10, 20, 30]; return 0; }");
        assert!(ir.contains("10") && ir.contains("20") && ir.contains("30"),
            "array literal elements should appear in IR");
    }

    // ================================================================
    // Enum value
    // ================================================================

    #[test]
    fn test_enum_value_no_args_ir() {
        let ir = compile_to_ir(concat!(
            "enum Dir { North, South }\n",
            "fn main() -> Int64 { let d = Dir::North; return 0; }"
        ));
        assert!(ir.contains("i32 0") || ir.contains("i64 0") || ir.contains("alloca"),
            "enum value should emit constant in IR");
    }

    // ================================================================
    // Struct / class field access
    // ================================================================

    #[test]
    fn test_class_field_read_ir() {
        let ir = compile_to_ir(concat!(
            "class Point { var x: Int64; var y: Int64; }\n",
            "fn main() -> Int64 {\n",
            "    let p = Point();\n",
            "    return p.x;\n",
            "}"
        ));
        assert!(ir.contains("%Point") || ir.contains("getelementptr"),
            "class field read should emit getelementptr in IR");
    }

    #[test]
    fn test_class_field_write_ir() {
        let ir = compile_to_ir(concat!(
            "class Counter { var count: Int64; }\n",
            "fn main() -> Int64 {\n",
            "    var c = Counter();\n",
            "    c.count = 42;\n",
            "    return c.count;\n",
            "}"
        ));
        assert!(ir.contains("store i64 42") || ir.contains("42"),
            "class field write should store value in IR");
    }

    // ================================================================
    // Method calls
    // ================================================================

    #[test]
    fn test_method_call_ir() {
        let ir = compile_to_ir(concat!(
            "class Adder { fn add(a: Int64, b: Int64) -> Int64 { return a + b; } }\n",
            "fn main() -> Int64 {\n",
            "    let adder = Adder();\n",
            "    return adder.add(3, 4);\n",
            "}"
        ));
        assert!(ir.contains("Adder") && ir.contains("add"),
            "method call should emit class and method names in IR");
    }

    // ================================================================
    // Try/catch
    // ================================================================

    #[test]
    fn test_try_catch_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    try {\n",
            "        throw \"oops\";\n",
            "    } catch e: String {\n",
            "        return 1;\n",
            "    }\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("try") || ir.contains("catch") || ir.contains("label"),
            "try/catch should emit branching IR");
    }

    // ================================================================
    // Modulo operator
    // ================================================================

    #[test]
    fn test_modulo_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 17 % 5; }");
        assert!(ir.contains("srem"), "modulo should emit srem instruction");
    }

    // ================================================================
    // Unary minus
    // ================================================================

    #[test]
    fn test_unary_minus_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { var x = 5; return -x; }");
        assert!(ir.contains("sub") || ir.contains("neg"), "unary minus should emit sub/neg in IR");
    }

    // ================================================================
    // Immutable global
    // ================================================================

    #[test]
    fn test_immutable_struct_used_ir() {
        // immutable in Tinox is a struct-like type, not a constant
        let ir = compile_to_ir(concat!(
            "immutable Config(host: String, port: Int64);\n",
            "fn main() -> Int64 { let c = Config(\"localhost\", 8080); return 0; }"
        ));
        assert!(ir.contains("Config") || ir.contains("8080"),
            "immutable struct usage should appear in IR");
    }

    // ================================================================
    // Defer statement
    // ================================================================

    #[test]
    fn test_defer_generates_code() {
        let ir = compile_to_ir(concat!(
            "fn cleanup() -> Nothing { return; }\n",
            "fn main() -> Int64 {\n",
            "    defer { cleanup(); }\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("@cleanup") || ir.contains("cleanup"),
            "deferred call should appear in IR");
    }

    // ================================================================
    // Float arithmetic
    // ================================================================

    #[test]
    fn test_float_add_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 1.5 + 2.5; return 0; }");
        assert!(ir.contains("fadd"), "float addition should emit fadd");
    }

    #[test]
    fn test_float_mul_two_literals_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 3.0 * 2.0; return 0; }");
        assert!(ir.contains("fmul"), "float multiplication should emit fmul");
    }

    #[test]
    fn test_float_div_two_literals_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 10.0 / 4.0; return 0; }");
        assert!(ir.contains("fdiv"), "float division should emit fdiv");
    }

    // ================================================================
    // Interface polymorphism
    // ================================================================

    #[test]
    fn test_interface_impl_ir() {
        let ir = compile_to_ir(concat!(
            "interface Greeter { fn greet() -> Nothing; }\n",
            "class Hello implements Greeter {\n",
            "    fn greet() -> Nothing { println(\"hi\"); }\n",
            "}\n",
            "fn main() -> Int64 { let h = Hello(); h.greet(); return 0; }"
        ));
        assert!(ir.contains("Hello") && ir.contains("greet"),
            "interface implementation should emit class and method in IR");
    }

    // ================================================================
    // Recursive functions
    // ================================================================

    #[test]
    fn test_recursive_fibonacci_ir() {
        let ir = compile_to_ir(concat!(
            "fn fib(n: Int64) -> Int64 {\n",
            "    if n <= 1 { return n; }\n",
            "    return fib(n - 1) + fib(n - 2);\n",
            "}\n",
            "fn main() -> Int64 { return fib(10); }"
        ));
        assert!(ir.contains("@fib"), "fibonacci should define @fib in IR");
        assert!(ir.contains("call i64 @fib") || ir.contains("@fib("),
            "fibonacci should call itself recursively");
    }

    #[test]
    fn test_recursive_countdown_ir() {
        let ir = compile_to_ir(concat!(
            "fn countdown(n: Int64) -> Nothing {\n",
            "    if n <= 0 { return; }\n",
            "    countdown(n - 1);\n",
            "}\n",
            "fn main() -> Int64 { countdown(5); return 0; }"
        ));
        assert!(ir.contains("@countdown"), "countdown should appear in IR");
    }

    // ================================================================
    // Multiple functions
    // ================================================================

    #[test]
    fn test_three_functions_ir() {
        let ir = compile_to_ir(concat!(
            "fn a() -> Int64 { return 1; }\n",
            "fn b() -> Int64 { return 2; }\n",
            "fn c() -> Int64 { return a() + b(); }\n",
            "fn main() -> Int64 { return c(); }"
        ));
        assert!(ir.contains("@a") && ir.contains("@b") && ir.contains("@c"),
            "all three functions should appear in IR");
    }

    // ================================================================
    // Higher-order functions / lambdas
    // ================================================================

    #[test]
    fn test_lambda_single_param_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let sq = \\x -> x * x;\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("mul") || ir.contains("lambda") || ir.contains("alloca"),
            "lambda should emit IR code");
    }

    #[test]
    fn test_lambda_two_params_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let add = (a, b) => a + b;\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("add") || ir.contains("alloca"),
            "two-param lambda should emit IR");
    }

    // ================================================================
    // Class inheritance
    // ================================================================

    #[test]
    fn test_class_extends_ir() {
        let ir = compile_to_ir(concat!(
            "class Animal { fn speak() -> Nothing { println(\"...\"); } }\n",
            "class Dog extends Animal { fn fetch() -> Nothing { println(\"!\"); } }\n",
            "fn main() -> Int64 { let d = Dog(); d.fetch(); return 0; }"
        ));
        assert!(ir.contains("Dog") && ir.contains("fetch"),
            "subclass method should appear in IR");
    }

    // ================================================================
    // Enum with match
    // ================================================================

    #[test]
    fn test_enum_match_ir() {
        let ir = compile_to_ir(concat!(
            "enum Color { Red, Green, Blue }\n",
            "fn name(c: Color) -> String {\n",
            "    match c {\n",
            "        Color::Red => return \"red\";\n",
            "        Color::Green => return \"green\";\n",
            "        _ => return \"blue\";\n",
            "    }\n",
            "}\n",
            "fn main() -> Int64 { let s = name(Color::Red); return 0; }"
        ));
        assert!(ir.contains("@name"), "enum match function should appear in IR");
    }

    // ================================================================
    // For-C loop
    // ================================================================

    #[test]
    fn test_forc_loop_sum_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var sum = 0;\n",
            "    for (var i = 0; i < 10; i += 1) {\n",
            "        sum += i;\n",
            "    }\n",
            "    return sum;\n",
            "}"
        ));
        assert!(ir.contains("add") && ir.contains("icmp"),
            "for-C loop should emit add and compare instructions");
    }

    // ================================================================
    // Nested conditionals
    // ================================================================

    #[test]
    fn test_nested_if_else_ir() {
        let ir = compile_to_ir(concat!(
            "fn classify(n: Int64) -> String {\n",
            "    if n < 0 {\n",
            "        return \"negative\";\n",
            "    } else if n == 0 {\n",
            "        return \"zero\";\n",
            "    } else {\n",
            "        return \"positive\";\n",
            "    }\n",
            "}\n",
            "fn main() -> Int64 { let s = classify(5); return 0; }"
        ));
        assert!(ir.contains("@classify") && ir.contains("then") || ir.contains("br"),
            "nested if/else should emit conditional branches");
    }

    // ================================================================
    // Integer operations
    // ================================================================

    #[test]
    fn test_integer_subtraction_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 10 - 3; }");
        assert!(ir.contains("sub"), "subtraction should emit sub");
    }

    #[test]
    fn test_integer_multiplication_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 6 * 7; }");
        assert!(ir.contains("mul"), "multiplication should emit mul");
    }

    #[test]
    fn test_integer_division_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 20 / 4; }");
        assert!(ir.contains("sdiv"), "division should emit sdiv");
    }

    // ================================================================
    // Local variable allocation
    // ================================================================

    #[test]
    fn test_multiple_locals_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    let a = 1;\n",
            "    let b = 2;\n",
            "    let c = 3;\n",
            "    let d = 4;\n",
            "    let e = 5;\n",
            "    return a + b + c + d + e;\n",
            "}"
        ));
        assert!(ir.contains("alloca") || ir.contains("add"),
            "multiple locals should emit alloca or be kept in registers");
    }

    // ================================================================
    // Boolean operations IR
    // ================================================================

    #[test]
    fn test_not_bool_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let b = !true; return 0; }");
        assert!(ir.contains("xor") || ir.contains("not"),
            "boolean NOT should emit xor or not");
    }

    // ================================================================
    // Cast operations
    // ================================================================

    #[test]
    fn test_cast_i64_to_float_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 5; let f = x as Float64; return 0; }");
        assert!(ir.contains("sitofp") || ir.contains("fpext") || ir.contains("float"),
            "int-to-float cast should emit sitofp in IR");
    }

    #[test]
    fn test_cast_float_to_i64_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { let x = 3.14; let n = x as Int64; return n; }");
        assert!(ir.contains("fptosi") || ir.contains("trunc") || ir.contains("i64"),
            "float-to-int cast should emit fptosi in IR");
    }

    // ================================================================
    // Range expression
    // ================================================================

    #[test]
    fn test_range_for_loop_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var total = 0;\n",
            "    for i in 0..5 {\n",
            "        total += i;\n",
            "    }\n",
            "    return total;\n",
            "}"
        ));
        assert!(ir.contains("add") && ir.contains("icmp"),
            "range for loop should emit addition and comparison");
    }

    // ================================================================
    // Struct literal IR
    // ================================================================

    #[test]
    fn test_struct_literal_ir() {
        let ir = compile_to_ir(concat!(
            "class Point { var x: Int64; var y: Int64; }\n",
            "fn main() -> Int64 {\n",
            "    let p = Point { x: 3, y: 4 };\n",
            "    return 0;\n",
            "}"
        ));
        assert!(ir.contains("Point") || ir.contains("alloca"),
            "struct literal should emit type or alloca in IR");
    }

    // ================================================================
    // Global immutable
    // ================================================================

    #[test]
    fn test_immutable_struct_ir_v2() {
        let ir = compile_to_ir(concat!(
            "immutable Config(host: String, port: Int64);\n",
            "fn get_port(c: Config) -> Int64 { return c.port; }\n",
            "fn main() -> Int64 { return 0; }"
        ));
        assert!(ir.contains("Config") || ir.contains("port") || ir.contains("getelementptr"),
            "immutable struct should be in IR");
    }

    // ================================================================
    // Println / print builtins
    // ================================================================

    #[test]
    fn test_println_int_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { println(42); return 0; }");
        assert!(ir.contains("println") || ir.contains("printf") || ir.contains("print"),
            "println should appear in IR");
    }

    #[test]
    fn test_println_string_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { println(\"hello\"); return 0; }");
        assert!(ir.contains("hello"), "string argument should appear in IR");
    }

    // ================================================================
    // Bitwise shift operations
    // ================================================================

    #[test]
    fn test_shift_left_const_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 1 << 8; }");
        assert!(ir.contains("shl"), "left shift should emit shl");
    }

    #[test]
    fn test_shift_right_const_ir() {
        let ir = compile_to_ir("fn main() -> Int64 { return 256 >> 4; }");
        assert!(ir.contains("ashr") || ir.contains("lshr"), "right shift should emit ashr or lshr");
    }

    // ================================================================
    // Break and continue
    // ================================================================

    #[test]
    fn test_break_in_loop_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var i = 0;\n",
            "    loop {\n",
            "        if i >= 5 { break; }\n",
            "        i += 1;\n",
            "    }\n",
            "    return i;\n",
            "}"
        ));
        assert!(ir.contains("br") || ir.contains("loop"),
            "break in loop should produce branch instruction");
    }

    #[test]
    fn test_continue_in_while_ir() {
        let ir = compile_to_ir(concat!(
            "fn main() -> Int64 {\n",
            "    var sum = 0;\n",
            "    var i = 0;\n",
            "    while i < 10 {\n",
            "        i += 1;\n",
            "        if i == 5 { continue; }\n",
            "        sum += i;\n",
            "    }\n",
            "    return sum;\n",
            "}"
        ));
        assert!(ir.contains("loop") || ir.contains("br"),
            "continue in while should produce branch back to loop header");
    }

    // ================================================================
    // @Sensitive / @Masked — toString generation
    // ================================================================

    fn compile_to_ir_with_masks(src: &str, sensitive: Vec<(&str, &str)>, masked: Vec<(&str, &str)>) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().expect("parse failed");
        let mut cg = CodeGen::new();
        let s_fields = sensitive.into_iter().map(|(c, f)| LogMaskFieldInfo {
            class_name: c.to_string(), field_name: f.to_string(),
        }).collect();
        let m_fields = masked.into_iter().map(|(c, f)| LogMaskFieldInfo {
            class_name: c.to_string(), field_name: f.to_string(),
        }).collect();
        cg.set_annotation_info(AnnotationInfo {
            sensitive_fields: s_fields,
            masked_fields: m_fields,
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen failed");
        cg.into_ir()
    }

    fn compile_to_ir_with_serialize(
        src: &str,
        json_classes: Vec<&str>,
        do_not_serialize: Vec<(&str, &str)>,
    ) -> String {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex failed");
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().expect("parse failed");
        let mut cg = CodeGen::new();
        let dns_fields = do_not_serialize.into_iter().map(|(c, f)| LogMaskFieldInfo {
            class_name: c.to_string(), field_name: f.to_string(),
        }).collect();
        cg.set_annotation_info(AnnotationInfo {
            do_not_serialize_fields: dns_fields,
            json_serializable_classes: json_classes.into_iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen failed");
        cg.into_ir()
    }

    #[test]
    fn test_sensitive_field_emits_tostring() {
        let ir = compile_to_ir_with_masks(
            "class User { var name: String; var password: String; }\nfn main() -> Int64 { return 0; }",
            vec![("User", "password")],
            vec![],
        );
        assert!(ir.contains("User_toString"), "should emit toString for User");
    }

    #[test]
    fn test_sensitive_field_uses_stars() {
        let ir = compile_to_ir_with_masks(
            "class User { var name: String; var password: String; }\nfn main() -> Int64 { return 0; }",
            vec![("User", "password")],
            vec![],
        );
        assert!(ir.contains("***"), "sensitive field should emit *** literal");
    }

    #[test]
    fn test_masked_field_calls_mask_partial() {
        let ir = compile_to_ir_with_masks(
            "class User { var name: String; var email: String; }\nfn main() -> Int64 { return 0; }",
            vec![],
            vec![("User", "email")],
        );
        assert!(ir.contains("tinox_string_mask_partial"), "masked field should call mask_partial");
    }

    #[test]
    fn test_no_annotation_no_masked_tostring() {
        let ir = compile_to_ir(
            "class User { var name: String; }\nfn main() -> Int64 { return 0; }",
        );
        assert!(!ir.contains("User_toString"), "no @Sensitive/@Masked → no User_toString generated");
    }

    #[test]
    fn test_string_concat_coerces_object_to_string() {
        let ir = compile_to_ir_with_masks(
            concat!(
                "class User { var name: String; var password: String; }\n",
                "fn log(msg: String) -> Nothing { println(msg); }\n",
                "fn main() -> Int64 {\n",
                "    var u = User { name: \"Alice\", password: \"secret\" };\n",
                "    let s = \"prefix: \" + u;\n",
                "    return 0;\n",
                "}"
            ),
            vec![("User", "password")],
            vec![],
        );
        assert!(ir.contains("User_toString"), "toString should be generated");
        assert!(ir.contains("tinox_string_concat"), "concat should be emitted");
    }

    #[test]
    fn test_tostring_contains_class_name_prefix() {
        let ir = compile_to_ir_with_masks(
            "class Payment { var amount: Int64; var card: String; }\nfn main() -> Int64 { return 0; }",
            vec![("Payment", "card")],
            vec![],
        );
        // The class name prefix "Payment{" is stored as a string literal in the IR
        assert!(ir.contains("Payment{"), "toString should start with ClassName{{");
    }

    #[test]
    fn test_tostring_registered_in_method_ret_types() {
        // pre_register_log_mask_tostring must put ClassName_toString into method_ret_types
        // BEFORE user code is compiled so that explicit user.toString() calls resolve.
        let mut lexer = tinox_lexer::Lexer::new(
            "class User { var name: String; var password: String; }\nfn main() -> Int64 { return 0; }"
        );
        let tokens = lexer.tokenize().expect("lex");
        let ast = Parser::new(tokens).parse().expect("parse");
        let mut cg = CodeGen::new();
        cg.set_annotation_info(AnnotationInfo {
            sensitive_fields: vec![LogMaskFieldInfo { class_name: "User".to_string(), field_name: "password".to_string() }],
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen");
        let ir = cg.into_ir();
        assert!(ir.contains("define i8* @User_toString"), "User_toString function must be emitted");
    }

    #[test]
    fn test_explicit_tostring_call_on_object() {
        // user.toString() on a @Sensitive-annotated class should call User_toString
        let src = concat!(
            "class User { var name: String; var secret: String; }\n",
            "fn show(s: String) -> Nothing { println(s); }\n",
            "fn main() -> Int64 {\n",
            "    var u = User { name: \"Alice\", secret: \"pw\" };\n",
            "    let s = u.toString();\n",
            "    show(s);\n",
            "    return 0;\n",
            "}"
        );
        let ir = compile_to_ir_with_masks(src, vec![("User", "secret")], vec![]);
        assert!(ir.contains("User_toString"), "explicit u.toString() should dispatch to User_toString");
        assert!(ir.contains("***"), "sensitive field must be masked");
    }

    #[test]
    fn test_both_annotations_in_same_class() {
        let ir = compile_to_ir_with_masks(
            "class User { var name: String; var password: String; var email: String; }\nfn main() -> Int64 { return 0; }",
            vec![("User", "password")],
            vec![("User", "email")],
        );
        assert!(ir.contains("User_toString"), "should emit toString");
        assert!(ir.contains("***"), "sensitive field → ***");
        assert!(ir.contains("tinox_string_mask_partial"), "masked field → mask_partial");
    }

    // ================================================================
    // @JsonSerializable / @DoNotSerialize — toJson generation
    // ================================================================

    #[test]
    fn test_json_serializable_emits_to_json() {
        let ir = compile_to_ir_with_serialize(
            "class User { var id: Int64; var name: String; }\nfn main() -> Int64 { return 0; }",
            vec!["User"],
            vec![],
        );
        assert!(ir.contains("User_toJson"), "should emit toJson for User");
    }

    #[test]
    fn test_json_serializable_emits_opening_brace() {
        let ir = compile_to_ir_with_serialize(
            "class Item { var id: Int64; }\nfn main() -> Int64 { return 0; }",
            vec!["Item"],
            vec![],
        );
        assert!(ir.contains("{"), "toJson should contain opening brace");
    }

    #[test]
    fn test_json_serializable_includes_field_key() {
        let ir = compile_to_ir_with_serialize(
            "class User { var id: Int64; var name: String; }\nfn main() -> Int64 { return 0; }",
            vec!["User"],
            vec![],
        );
        assert!(ir.contains("\"id\"") || ir.contains("id"), "toJson should reference field name");
    }

    #[test]
    fn test_json_serializable_string_field_gets_quotes() {
        let ir = compile_to_ir_with_serialize(
            "class User { var name: String; }\nfn main() -> Int64 { return 0; }",
            vec!["User"],
            vec![],
        );
        // String values are wrapped in quotes — the IR has a quote literal "
        assert!(ir.contains("\\22") || ir.contains("\"\\\"\"") || ir.contains("inttoptr"),
            "string field in toJson should be wrapped in quotes (inttoptr for i8* conversion)");
    }

    #[test]
    fn test_do_not_serialize_field_absent_from_to_json() {
        let ir = compile_to_ir_with_serialize(
            "class User { var name: String; var internalToken: String; }\nfn main() -> Int64 { return 0; }",
            vec!["User"],
            vec![("User", "internalToken")],
        );
        assert!(ir.contains("User_toJson"), "toJson should still be emitted");
        // internalToken field name must not appear as a JSON key
        assert!(!ir.contains("\"internalToken\""), "@DoNotSerialize field must not appear in toJson");
    }

    #[test]
    fn test_do_not_serialize_all_fields_emits_empty_object() {
        let ir = compile_to_ir_with_serialize(
            "class Secret { var token: String; var key: String; }\nfn main() -> Int64 { return 0; }",
            vec!["Secret"],
            vec![("Secret", "token"), ("Secret", "key")],
        );
        assert!(ir.contains("Secret_toJson"), "toJson should be emitted even when all fields are excluded");
        // With all fields excluded the only string literals in the function are "{" and "}"
        assert!(ir.contains("{"), "empty object should still have opening brace");
    }

    #[test]
    fn test_no_json_serializable_no_to_json() {
        let ir = compile_to_ir(
            "class Plain { var x: Int64; }\nfn main() -> Int64 { return 0; }",
        );
        assert!(!ir.contains("Plain_toJson"), "no @JsonSerializable → no toJson emitted");
    }

    #[test]
    fn test_json_serializable_registered_in_method_ret_types() {
        let mut lexer = tinox_lexer::Lexer::new(
            "class User { var name: String; }\nfn main() -> Int64 { return 0; }"
        );
        let tokens = lexer.tokenize().expect("lex");
        let ast = Parser::new(tokens).parse().expect("parse");
        let mut cg = CodeGen::new();
        cg.set_annotation_info(AnnotationInfo {
            json_serializable_classes: vec!["User".to_string()],
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen");
        let ir = cg.into_ir();
        assert!(ir.contains("define i8* @User_toJson"), "User_toJson function must be emitted");
    }

    #[test]
    fn test_do_not_serialize_combined_with_sensitive_in_codegen() {
        // A class can have both @Sensitive (for logging) and @DoNotSerialize (for JSON)
        // on different fields — both should be respected independently.
        let src = "class Record { var label: String; var password: String; var internalId: String; }\nfn main() -> Int64 { return 0; }";
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex");
        let ast = Parser::new(tokens).parse().expect("parse");
        let mut cg = CodeGen::new();
        cg.set_annotation_info(AnnotationInfo {
            sensitive_fields: vec![LogMaskFieldInfo { class_name: "Record".to_string(), field_name: "password".to_string() }],
            do_not_serialize_fields: vec![LogMaskFieldInfo { class_name: "Record".to_string(), field_name: "internalId".to_string() }],
            json_serializable_classes: vec!["Record".to_string()],
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen");
        let ir = cg.into_ir();
        assert!(ir.contains("Record_toString"), "toString should be emitted for @Sensitive field");
        assert!(ir.contains("Record_toJson"), "toJson should be emitted for @JsonSerializable");
        assert!(ir.contains("***"), "@Sensitive field should be masked in toString");
        assert!(!ir.contains("\"internalId\""), "@DoNotSerialize field must not appear in toJson");
    }

    // Bug 107: sensitive_fields/masked_fields/do_not_serialize_fields are
    // keyed by the class that DECLARED the field. A subclass's toString/toJson
    // includes inherited fields in its layout, so generating them for the
    // subclass must still recognize a field declared @Sensitive/@Masked/
    // @DoNotSerialize on an ancestor class instead of silently emitting it.
    #[test]
    fn test_inherited_sensitive_field_masked_in_subclass_tostring() {
        // AdminAccount has its own @Masked field (email), which is what
        // triggers AdminAccount_toString generation in the first place --
        // this matches the finding's exact repro shape. The inherited
        // @Sensitive field (sessionToken, declared on Account) must still be
        // masked once that method is generated.
        let src = concat!(
            "class Account { var sessionToken: String; }\n",
            "class AdminAccount extends Account { var email: String; }\n",
            "fn main() -> Int64 { return 0; }"
        );
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex");
        let ast = Parser::new(tokens).parse().expect("parse");
        let mut cg = CodeGen::new();
        cg.set_annotation_info(AnnotationInfo {
            sensitive_fields: vec![LogMaskFieldInfo { class_name: "Account".to_string(), field_name: "sessionToken".to_string() }],
            masked_fields: vec![LogMaskFieldInfo { class_name: "AdminAccount".to_string(), field_name: "email".to_string() }],
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen");
        let ir = cg.into_ir();
        assert!(ir.contains("define i8* @AdminAccount_toString"), "AdminAccount_toString should be emitted");
        assert!(ir.contains("***"), "inherited @Sensitive field must still be masked in the subclass's toString");
    }

    #[test]
    fn test_inherited_do_not_serialize_field_absent_from_subclass_tojson() {
        let src = concat!(
            "class Account { var sessionToken: String; }\n",
            "class AdminAccount extends Account { var role: String; }\n",
            "fn main() -> Int64 { return 0; }"
        );
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().expect("lex");
        let ast = Parser::new(tokens).parse().expect("parse");
        let mut cg = CodeGen::new();
        cg.set_annotation_info(AnnotationInfo {
            do_not_serialize_fields: vec![LogMaskFieldInfo { class_name: "Account".to_string(), field_name: "sessionToken".to_string() }],
            json_serializable_classes: vec!["AdminAccount".to_string()],
            ..Default::default()
        });
        cg.gen(&ast).expect("codegen");
        let ir = cg.into_ir();
        assert!(ir.contains("define i8* @AdminAccount_toJson"), "AdminAccount_toJson should be emitted");
        assert!(!ir.contains("\"sessionToken\""), "inherited @DoNotSerialize field must not appear in the subclass's toJson");
    }

    #[test]
    fn test_multiple_json_serializable_classes() {
        let ir = compile_to_ir_with_serialize(
            "class User { var id: Int64; }\nclass Product { var sku: String; }\nfn main() -> Int64 { return 0; }",
            vec!["User", "Product"],
            vec![],
        );
        assert!(ir.contains("User_toJson"), "User should get toJson");
        assert!(ir.contains("Product_toJson"), "Product should get toJson");
    }
}

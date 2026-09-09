//! Issue #186: `tinox graph` -- static call-graph construction and Mermaid
//! rendering, seeded from every auto-run entry point the compiler already
//! knows how to find (REST/WebSocket/AMQP/CLI, via
//! `tinox_typecheck::annotations::process_annotations`). Pure data-in/
//! data-out: this module never touches the filesystem or parses/resolves
//! imports itself -- `gen_call_graph` in `main.rs` assembles the merged,
//! typechecked AST first (the same pipeline `compile_file` already uses)
//! and hands it to `build_call_graph` here.
//!
//! v1 scope (see CLAUDE.md's "tinox graph" section for the full writeup):
//! per-method nodes, whole-project graph (no `--from`/`--depth` filter
//! yet), stops expanding at the project's own class boundary (stdlib/
//! external-dependency calls are shown but not recursed into).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use tinox_parser::{Class, Decl, DeclKind, Expr, ExprKind, Stmt, StmtKind, Type};
use tinox_typecheck::annotations::AnnotationProcessingResult;

/// One auto-run entry point this graph is seeded from.
pub struct EntryPoint {
    pub kind: &'static str,
    pub label: String,
    pub class: String,
    pub method: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeStyle {
    /// Static call, self-call, or a call through a receiver whose
    /// concrete class is statically known.
    Direct,
    /// Call through an interface-typed receiver -- one edge per class
    /// that implements the interface (an actual call fans out to exactly
    /// one of these at runtime, but which one isn't statically known).
    InterfaceFanout,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub style: EdgeStyle,
}

pub struct CallGraph {
    pub entry_points: Vec<EntryPoint>,
    pub edges: Vec<Edge>,
    /// (caller "Class.method", human-readable text of the unresolved call).
    pub unresolved: Vec<(String, String)>,
    /// "Class.method" nodes whose class isn't part of this project (stdlib
    /// or an external dependency) -- reached, but not expanded further.
    pub external_nodes: HashSet<String>,
}

/// Extracts the leaf type name from a declared `Type`, when it's shaped
/// like something that could name a project class/interface
/// (`Type::Named`/`Type::Generic` -- e.g. `Foo`, `List<Foo>`'s outer name
/// is "List" which won't resolve to a project class either, so this
/// deliberately doesn't unwrap generic type ARGUMENTS, only the type's own
/// head name). Primitive/structural types (`Int64`, `Array(_)`, `Map(_,_)`,
/// ...) return `None` -- there's no class/interface name to resolve there.
fn type_head_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n) => Some(n.clone()),
        Type::Generic { name, .. } => Some(name.clone()),
        Type::Mutable(inner) | Type::Ref(inner) => type_head_name(inner),
        _ => None,
    }
}

fn collect_classes<'a>(decls: &'a [Decl], out: &mut HashMap<String, &'a Class>) {
    for d in decls {
        match &d.node {
            DeclKind::Class(c) => {
                out.insert(c.name.clone(), c);
            }
            DeclKind::Namespace(ns) => collect_classes(&ns.decls, out),
            _ => {}
        }
    }
}

/// Which of `classes` are actually part of THIS project, as opposed to
/// merged in from an import (stdlib or an external dependency) --
/// `resolve_imports` (main.rs) merges every imported file's decls into
/// the same flat list before this module ever sees it, so `classes`
/// itself can't tell "own code" from "library code" apart on its own.
///
/// One-type-per-file is a hard-enforced compiler rule (CLAUDE.md), so
/// every method of a given class is guaranteed to share the same
/// originating file -- `stamp_file_identity` (main.rs) already stamps
/// that onto every `Method.file` for exactly this kind of lookup, for
/// BOTH the entry file and every imported file uniformly. A class is
/// "project-owned" here iff its first method's file is a descendant of
/// `project_root` and not inside a `.tinox` (installed-dependency)
/// subtree -- matches the exact same `.tinox`-component heuristic issue
/// #185's `check_namespace_path_matches` uses to recognize installed
/// dependencies. A method-less class (pure data, no signal available)
/// conservatively counts as NOT project-owned -- it can never be a call
/// target anyway (nothing to call on it), so this never actually matters
/// in practice.
///
/// Found live while smoke-testing `examples/ws_echo_annotated`: without
/// this distinction, `tinox.core.websocket`'s `Ws` class (merged in via
/// `import tinox.core.websocket;`) was indistinguishable from the
/// project's own classes, so the traversal recursed straight into the
/// stdlib's OWN internals (`Ws.sendText` -> `Ws.writeFrame` -> ... ->
/// `httpConnWriteBytes`) instead of stopping at the call to `Ws.sendText`
/// -- exactly the "expanding into tinox.core.* internals" issue #186
/// explicitly asks to avoid.
fn project_owned_classes(classes: &HashMap<String, &Class>, project_root: &std::path::Path) -> HashSet<String> {
    let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    classes
        .iter()
        .filter(|(_, c)| {
            c.methods.first().is_some_and(|m| {
                let file = std::path::Path::new(&*m.file);
                file.starts_with(&root) && !file.components().any(|comp| comp.as_os_str() == ".tinox")
            })
        })
        .map(|(name, _)| name.clone())
        .collect()
}

fn collect_enum_names(decls: &[Decl], out: &mut HashSet<String>) {
    for d in decls {
        match &d.node {
            DeclKind::Enum(e) => {
                out.insert(e.name.clone());
            }
            DeclKind::Namespace(ns) => collect_enum_names(&ns.decls, out),
            _ => {}
        }
    }
}

/// Finds a method by name on `class_name`, walking the single-inheritance
/// `extends` chain if not declared directly on the class itself. Returns
/// `None` if the class isn't in `classes` at all, or the method isn't
/// found anywhere up the chain (including if the chain leaves the
/// project's own classes, e.g. extending an external/stdlib base).
fn find_method<'a>(
    classes: &HashMap<String, &'a Class>,
    class_name: &str,
    method_name: &str,
) -> Option<(&'a Class, &'a tinox_parser::Method)> {
    let mut cur = class_name.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(cur.clone()) {
            return None; // defensive: a cyclic `extends` chain (shouldn't parse, but don't hang)
        }
        let class = *classes.get(&cur)?;
        if let Some(m) = class.methods.iter().find(|m| m.name == method_name) {
            return Some((class, m));
        }
        cur = class.extends.clone()?;
    }
}

/// Everything the call-site walker needs that stays constant across a
/// single method-body traversal (`current_class`/`locals` change per
/// entry into `traverse`; `classes`/`iface_names`/`enum_names` are the
/// whole project's indices, unchanged for the whole `build_call_graph`
/// run). Bundled into one struct so `walk_stmt`/`walk_expr` take one
/// reference instead of accumulating positional parameters as the walker
/// grows.
struct WalkCtx<'a> {
    current_class: &'a str,
    locals: &'a HashMap<String, String>,
    classes: &'a HashMap<String, &'a Class>,
    iface_names: &'a HashSet<String>,
    enum_names: &'a HashSet<String>,
}

/// What a `MethodCall`'s receiver expression resolves to, before knowing
/// whether the resolved name is a class or an interface.
enum ReceiverType {
    Known(String),
    Unknown,
}

fn resolve_receiver_type(obj: &Expr, ctx: &WalkCtx) -> ReceiverType {
    match &obj.node {
        ExprKind::This => ReceiverType::Known(ctx.current_class.to_string()),
        ExprKind::New { class, .. } => ReceiverType::Known(class.clone()),
        ExprKind::Ident(name) => {
            if ctx.classes.contains_key(name) {
                // `ClassName.method(...)` -- a static call.
                ReceiverType::Known(name.clone())
            } else if let Some(ty) = ctx.locals.get(name) {
                ReceiverType::Known(ty.clone())
            } else {
                ReceiverType::Unknown
            }
        }
        _ => ReceiverType::Unknown,
    }
}

/// A resolved call target, before fan-out/edge-emission.
enum Resolved {
    Direct(String, String),
    InterfaceFanout(String, String),
    Unresolved(String),
}

fn classify(type_name: String, method: String, iface_names: &HashSet<String>) -> Resolved {
    if iface_names.contains(&type_name) {
        Resolved::InterfaceFanout(type_name, method)
    } else {
        Resolved::Direct(type_name, method)
    }
}

/// Collects every method parameter's and locally-declared variable's
/// (`var`/`let`) statically-known type name, keyed by name. A SINGLE flat
/// pass over the whole method body, not scope-aware (a variable shadowed
/// in a nested block overwrites its outer entry in this map) -- a
/// deliberate v1 simplification: precise lexical scoping isn't needed to
/// get the common cases (`var x: Foo = ...`, `var x = new Foo()`, method
/// parameters) right, and this project's own real handlers (see
/// `examples/rest_with_mini/UserController.tnx`) don't shadow locals this
/// way.
fn collect_locals(method: &tinox_parser::Method) -> HashMap<String, String> {
    let mut locals = HashMap::new();
    for p in &method.params {
        if let Some(ty) = type_head_name(&p.param_type) {
            locals.insert(p.name.clone(), ty);
        }
    }
    collect_locals_stmt(&method.body, &mut locals);
    locals
}

fn collect_locals_stmt(stmt: &Stmt, locals: &mut HashMap<String, String>) {
    match &stmt.node {
        StmtKind::Var { name, ty, value, .. } | StmtKind::Let { name, ty, value } => {
            let resolved = ty.as_ref().and_then(type_head_name).or_else(|| {
                value.as_ref().and_then(|v| match &v.node {
                    ExprKind::New { class, .. } => Some(class.clone()),
                    _ => None,
                })
            });
            if let Some(t) = resolved {
                locals.insert(name.clone(), t);
            }
        }
        StmtKind::Expr(_)
        | StmtKind::Assignment { .. }
        | StmtKind::Return(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Throw(_)
        | StmtKind::Empty => {}
        StmtKind::If { then_branch, else_branch, .. } => {
            collect_locals_stmt(then_branch, locals);
            if let Some(e) = else_branch {
                collect_locals_stmt(e, locals);
            }
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::Loop { body } => {
            collect_locals_stmt(body, locals);
        }
        StmtKind::ForC { init, body, .. } => {
            if let Some(i) = init {
                collect_locals_stmt(i, locals);
            }
            collect_locals_stmt(body, locals);
        }
        StmtKind::Try { body, catches, finally } => {
            collect_locals_stmt(body, locals);
            for c in catches {
                collect_locals_stmt(&c.body, locals);
            }
            if let Some(f) = finally {
                collect_locals_stmt(f, locals);
            }
        }
        StmtKind::Defer(inner) => collect_locals_stmt(inner, locals),
        StmtKind::Block(stmts) => {
            for s in stmts {
                collect_locals_stmt(s, locals);
            }
        }
        StmtKind::Select { arms, default } => {
            for a in arms {
                collect_locals_stmt(&a.body, locals);
            }
            if let Some(d) = default {
                collect_locals_stmt(d, locals);
            }
        }
    }
}

/// Walks every statement/expression reachable from `stmt`, resolving each
/// call-shaped node (`MethodCall`, a bare `Call` to a same-class method,
/// `SuperCall`, a `Type::method(...)` written via the `EnumValue` node --
/// see its own comment below) into `out`. Always recurses into every
/// nested statement/expression regardless of whether the current node is
/// itself a call, so calls nested in arguments, lambda bodies, match
/// arms, etc. are all found.
fn walk_stmt(stmt: &Stmt, ctx: &WalkCtx, out: &mut Vec<Resolved>) {
    match &stmt.node {
        StmtKind::Expr(e) => walk_expr(e, ctx, out),
        StmtKind::Let { value, .. } | StmtKind::Var { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, ctx, out);
            }
        }
        StmtKind::Assignment { target, value } => {
            walk_expr(target, ctx, out);
            walk_expr(value, ctx, out);
        }
        StmtKind::If { cond, then_branch, else_branch } => {
            walk_expr(cond, ctx, out);
            walk_stmt(then_branch, ctx, out);
            if let Some(e) = else_branch {
                walk_stmt(e, ctx, out);
            }
        }
        StmtKind::While { cond, body } => {
            walk_expr(cond, ctx, out);
            walk_stmt(body, ctx, out);
        }
        StmtKind::For { iter, body, .. } => {
            walk_expr(iter, ctx, out);
            walk_stmt(body, ctx, out);
        }
        StmtKind::ForC { init, cond, update, body } => {
            if let Some(i) = init {
                walk_stmt(i, ctx, out);
            }
            if let Some(c) = cond {
                walk_expr(c, ctx, out);
            }
            if let Some(u) = update {
                walk_expr(u, ctx, out);
            }
            walk_stmt(body, ctx, out);
        }
        StmtKind::Loop { body } => walk_stmt(body, ctx, out),
        StmtKind::Return(e) => {
            if let Some(e) = e {
                walk_expr(e, ctx, out);
            }
        }
        StmtKind::Throw(e) => walk_expr(e, ctx, out),
        StmtKind::Try { body, catches, finally } => {
            walk_stmt(body, ctx, out);
            for c in catches {
                walk_stmt(&c.body, ctx, out);
            }
            if let Some(f) = finally {
                walk_stmt(f, ctx, out);
            }
        }
        StmtKind::Defer(inner) => walk_stmt(inner, ctx, out),
        StmtKind::Block(stmts) => {
            for s in stmts {
                walk_stmt(s, ctx, out);
            }
        }
        StmtKind::Select { arms, default } => {
            for a in arms {
                walk_expr(&a.channel, ctx, out);
                walk_stmt(&a.body, ctx, out);
            }
            if let Some(d) = default {
                walk_stmt(d, ctx, out);
            }
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Empty => {}
    }
}

fn walk_expr(expr: &Expr, ctx: &WalkCtx, out: &mut Vec<Resolved>) {
    macro_rules! w {
        ($e:expr) => {
            walk_expr($e, ctx, out)
        };
    }
    macro_rules! ws {
        ($s:expr) => {
            walk_stmt($s, ctx, out)
        };
    }
    match &expr.node {
        ExprKind::Literal(_)
        | ExprKind::Ident(_)
        | ExprKind::This
        | ExprKind::Break
        | ExprKind::Continue
        | ExprKind::Channel => {}
        ExprKind::ArrayLiteral(items) | ExprKind::Tuple(items) => {
            for i in items {
                w!(i);
            }
        }
        ExprKind::MapLiteral(pairs) => {
            for (k, v) in pairs {
                w!(k);
                w!(v);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            w!(lhs);
            w!(rhs);
        }
        ExprKind::Unary { operand, .. } => w!(operand),
        ExprKind::Call { func, args } => {
            w!(func);
            for a in args {
                w!(a);
            }
            if let ExprKind::Ident(name) = &func.node {
                // No top-level free functions (issue #149): a bare
                // `name(...)` call is either an implicit same-class
                // method call (static or instance) or a lambda-variable
                // invocation.
                if find_method(ctx.classes, ctx.current_class, name).is_some() {
                    out.push(Resolved::Direct(ctx.current_class.to_string(), name.clone()));
                } else {
                    out.push(Resolved::Unresolved(format!("{name}(...)")));
                }
            }
        }
        ExprKind::MethodCall { obj, method, args } => {
            w!(obj);
            for a in args {
                w!(a);
            }
            match resolve_receiver_type(obj, ctx) {
                ReceiverType::Known(ty) => out.push(classify(ty, method.clone(), ctx.iface_names)),
                ReceiverType::Unknown => out.push(Resolved::Unresolved(format!("?.{method}(...)"))),
            }
        }
        ExprKind::Index { obj, index } => {
            w!(obj);
            w!(index);
        }
        ExprKind::FieldAccess { obj, .. } => w!(obj),
        ExprKind::SuperCall { method, args } => {
            for a in args {
                w!(a);
            }
            match ctx.classes.get(ctx.current_class).and_then(|c| c.extends.clone()) {
                Some(parent) => out.push(Resolved::Direct(parent, method.clone())),
                None => out.push(Resolved::Unresolved(format!("super.{method}(...)"))),
            }
        }
        ExprKind::New { args, .. } => {
            // Bare construction is deliberately not its own graph edge
            // (v1 scope cut, see module doc) -- only the constructor
            // ARGUMENTS are walked for nested calls.
            for a in args {
                w!(a);
            }
        }
        ExprKind::StructLiteral { fields, .. } => {
            for (_, v) in fields {
                w!(v);
            }
        }
        ExprKind::Block(stmts) => {
            for s in stmts {
                ws!(s);
            }
        }
        ExprKind::If { cond, then_branch, else_branch } => {
            w!(cond);
            w!(then_branch);
            if let Some(e) = else_branch {
                w!(e);
            }
        }
        ExprKind::While { cond, body } => {
            w!(cond);
            w!(body);
        }
        ExprKind::For { iter, body, .. } => {
            w!(iter);
            w!(body);
        }
        ExprKind::Loop { body } => w!(body),
        ExprKind::Match { expr: scrutinee, cases } => {
            w!(scrutinee);
            for c in cases {
                if let Some(g) = &c.guard {
                    w!(g);
                }
                w!(&c.body);
            }
        }
        ExprKind::Return(e) => {
            if let Some(e) = e {
                w!(e);
            }
        }
        ExprKind::Throw(e) => w!(e),
        ExprKind::Try { body, catches, finally } => {
            w!(body);
            for c in catches {
                ws!(&c.body);
            }
            if let Some(f) = finally {
                w!(f);
            }
        }
        ExprKind::Assign { target, value } | ExprKind::CompoundAssign { target, value, .. } => {
            w!(target);
            w!(value);
        }
        ExprKind::Lambda { body, .. } => w!(body),
        ExprKind::Spawn(e) | ExprKind::Await(e) | ExprKind::Recv(e) => w!(e),
        ExprKind::Send { channel, value } => {
            w!(channel);
            w!(value);
        }
        ExprKind::Cast { expr: inner, .. } | ExprKind::Is { expr: inner, .. } => w!(inner),
        ExprKind::Range { start, end, .. } => {
            w!(start);
            w!(end);
        }
        ExprKind::TupleIndex { tuple, .. } => w!(tuple),
        ExprKind::EnumValue { enum_name, variant, args, .. } => {
            for a in args {
                w!(a);
            }
            // `X::y(...)` is the SAME AST node for a real enum-variant
            // literal (`Color::Red`, `Option::Some(value)`) and a
            // static-method-call-like reference (`Json::deserialize<T>(...)`,
            // `Ws::sendText(...)`) -- there is no separate syntax. Found
            // live while smoke-testing `examples/ws_echo_annotated`:
            // `Ws::sendText(conn, ...)` was silently invisible in the
            // graph (neither an edge nor an unresolved entry) before this
            // case was added. Disambiguate the same way MethodCall's
            // receiver is resolved: a known project class -> a real
            // static call; a known project enum -> not a call at all
            // (skip, it's a real variant construction); anything else
            // (stdlib/external, e.g. `Ws`/`Json`) -> unresolved rather
            // than silently dropped, since it's genuinely ambiguous
            // without cross-crate type info.
            if ctx.classes.contains_key(enum_name) {
                out.push(Resolved::Direct(enum_name.clone(), variant.clone()));
            } else if !ctx.enum_names.contains(enum_name) {
                out.push(Resolved::Unresolved(format!("{enum_name}::{variant}(...)")));
            }
        }
    }
}

const MAX_DEPTH: usize = 40;

#[allow(clippy::too_many_arguments)]
fn traverse(
    class: &str,
    method: &str,
    classes: &HashMap<String, &Class>,
    project_classes: &HashSet<String>,
    iface_names: &HashSet<String>,
    enum_names: &HashSet<String>,
    iface_to_classes: &HashMap<String, HashSet<String>>,
    depth: usize,
    expanded: &mut HashSet<String>,
    edges: &mut Vec<Edge>,
    unresolved: &mut Vec<(String, String)>,
    external: &mut HashSet<String>,
) {
    let key = format!("{class}.{method}");
    if depth > MAX_DEPTH || !expanded.insert(key.clone()) {
        return;
    }
    let Some((_, m)) = find_method(classes, class, method) else {
        external.insert(key);
        return;
    };
    let locals = collect_locals(m);
    let ctx = WalkCtx { current_class: class, locals: &locals, classes, iface_names, enum_names };
    let mut resolved = Vec::new();
    walk_stmt(&m.body, &ctx, &mut resolved);

    for r in resolved {
        match r {
            Resolved::Direct(c, meth) => {
                let to = format!("{c}.{meth}");
                edges.push(Edge { from: key.clone(), to: to.clone(), style: EdgeStyle::Direct });
                // Only recurse into the project's OWN classes -- a call
                // that lands in a merged-in stdlib/dependency class (e.g.
                // `Ws.sendText`) is shown, but its own internals aren't
                // expanded further (see `project_owned_classes`'s doc
                // comment for the real case this fixed).
                if project_classes.contains(&c) {
                    traverse(&c, &meth, classes, project_classes, iface_names, enum_names, iface_to_classes, depth + 1, expanded, edges, unresolved, external);
                } else {
                    external.insert(to);
                }
            }
            Resolved::InterfaceFanout(iface, meth) => match iface_to_classes.get(&iface) {
                Some(impls) if !impls.is_empty() => {
                    for c in impls {
                        let to = format!("{c}.{meth}");
                        edges.push(Edge { from: key.clone(), to: to.clone(), style: EdgeStyle::InterfaceFanout });
                        if project_classes.contains(c) {
                            traverse(c, &meth, classes, project_classes, iface_names, enum_names, iface_to_classes, depth + 1, expanded, edges, unresolved, external);
                        } else {
                            external.insert(to);
                        }
                    }
                }
                _ => unresolved.push((key.clone(), format!("{iface}.{meth}(...) [no known implementors]"))),
            },
            Resolved::Unresolved(text) => unresolved.push((key.clone(), text)),
        }
    }
}

/// Builds the whole-project call graph from every entry point
/// `process_annotations` found. `iface_methods`/`class_implements` come
/// straight from `TypeChecker::interface_info()` (already-typechecked,
/// reused rather than re-derived from the raw AST). `project_root` is
/// used only to tell the project's own classes apart from ones merged in
/// via `import` (see `project_owned_classes`) -- it does not need to be
/// the same directory the entry file lives in for a multi-file project,
/// just an ancestor of every one of the project's OWN source files.
pub fn build_call_graph(
    decls: &[Decl],
    ann: &AnnotationProcessingResult,
    iface_methods: &HashMap<String, Vec<String>>,
    class_implements: &HashMap<String, Vec<String>>,
    project_root: &std::path::Path,
) -> CallGraph {
    let mut classes = HashMap::new();
    collect_classes(decls, &mut classes);
    let project_classes = project_owned_classes(&classes, project_root);
    let mut enum_names = HashSet::new();
    collect_enum_names(decls, &mut enum_names);

    let iface_names: HashSet<String> = iface_methods.keys().cloned().collect();
    let mut iface_to_classes: HashMap<String, HashSet<String>> = HashMap::new();
    for (class_name, ifaces) in class_implements {
        for iface in ifaces {
            iface_to_classes.entry(iface.clone()).or_default().insert(class_name.clone());
        }
    }

    let mut entry_points = Vec::new();
    for r in &ann.route_entries {
        entry_points.push(EntryPoint {
            kind: "REST",
            label: format!("{} {}", r.method, r.path),
            class: r.class_name.clone(),
            method: r.method_name.clone(),
        });
    }
    for ws in &ann.ws_endpoints {
        for (handler, tag) in [(&ws.on_open, "OnOpen"), (&ws.on_message, "OnMessage"), (&ws.on_close, "OnClose")] {
            if let Some(m) = handler {
                entry_points.push(EntryPoint {
                    kind: "WebSocket",
                    label: format!("{} {}", tag, ws.path),
                    class: ws.class_name.clone(),
                    method: m.clone(),
                });
            }
        }
    }
    for c in &ann.amqp10_consumers {
        if let Some(m) = &c.on_message {
            entry_points.push(EntryPoint {
                kind: "AMQP 1.0",
                label: format!("consumer {}", c.address),
                class: c.class_name.clone(),
                method: m.clone(),
            });
        }
    }
    for c in &ann.amqp091_consumers {
        if let Some(m) = &c.on_message {
            entry_points.push(EntryPoint {
                kind: "AMQP 0-9-1",
                label: format!("consumer {}", c.queue),
                class: c.class_name.clone(),
                method: m.clone(),
            });
        }
    }
    for cmd in &ann.cli_commands {
        entry_points.push(EntryPoint {
            kind: "CLI",
            label: format!("@Command {}", cmd.cmd_name),
            class: cmd.class_name.clone(),
            // Fixed convention (codegen.rs: `call i64 @{class}_run(...)`)
            // -- @Command has no per-method handler annotation to discover.
            method: "run".to_string(),
        });
    }

    let mut expanded = HashSet::new();
    let mut edges = Vec::new();
    let mut unresolved = Vec::new();
    let mut external = HashSet::new();
    for ep in &entry_points {
        traverse(&ep.class, &ep.method, &classes, &project_classes, &iface_names, &enum_names, &iface_to_classes, 0, &mut expanded, &mut edges, &mut unresolved, &mut external);
    }

    CallGraph { entry_points, edges, unresolved, external_nodes: external }
}

fn node_id(key: &str) -> String {
    key.replace(['.', '-', ' ', '/', ':'], "_")
}

/// Renders `graph` as a Mermaid `flowchart TD` diagram.
pub fn render_mermaid(graph: &CallGraph) -> String {
    let mut out = String::new();
    out.push_str("flowchart TD\n");
    out.push_str("    classDef entry fill:#e0f7fa,stroke:#00796b,stroke-width:2px;\n");
    out.push_str("    classDef external fill:#f5f5f5,stroke:#9e9e9e,stroke-dasharray: 3 3;\n\n");

    let entry_keys: HashMap<String, &EntryPoint> =
        graph.entry_points.iter().map(|e| (format!("{}.{}", e.class, e.method), e)).collect();

    let mut nodes: BTreeSet<String> = BTreeSet::new();
    for e in &graph.edges {
        nodes.insert(e.from.clone());
        nodes.insert(e.to.clone());
    }
    for key in entry_keys.keys() {
        nodes.insert(key.clone());
    }

    for node in &nodes {
        let id = node_id(node);
        let label = match entry_keys.get(node) {
            Some(ep) => format!("{} [{}: {}]", node, ep.kind, ep.label),
            None => node.clone(),
        };
        writeln!(out, "    {id}[\"{label}\"]").unwrap();
    }
    out.push('\n');
    for node in &nodes {
        let id = node_id(node);
        if entry_keys.contains_key(node) {
            writeln!(out, "    class {id} entry").unwrap();
        } else if graph.external_nodes.contains(node) {
            writeln!(out, "    class {id} external").unwrap();
        }
    }
    out.push('\n');

    let mut seen_edges: HashSet<(String, String, bool)> = HashSet::new();
    for e in &graph.edges {
        let fanout = matches!(e.style, EdgeStyle::InterfaceFanout);
        if !seen_edges.insert((e.from.clone(), e.to.clone(), fanout)) {
            continue;
        }
        let arrow = if fanout { "-.->" } else { "-->" };
        writeln!(out, "    {} {} {}", node_id(&e.from), arrow, node_id(&e.to)).unwrap();
    }

    if !graph.unresolved.is_empty() {
        out.push('\n');
        out.push_str("    unresolved[\"? (unresolved calls)\"]\n");
        let mut froms: BTreeSet<String> = BTreeSet::new();
        for (from, _) in &graph.unresolved {
            froms.insert(from.clone());
        }
        for from in &froms {
            writeln!(out, "    {} -.- unresolved", node_id(from)).unwrap();
        }
        out.push('\n');
        for (from, text) in &graph.unresolved {
            writeln!(out, "%% unresolved: {from}: {text}").unwrap();
        }
    }

    out
}

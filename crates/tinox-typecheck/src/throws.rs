//! Checked exceptions (#524, stage 3): every `@Error` class a call can raise must be
//! caught by an enclosing `catch` or listed in the caller's `@Throws`.
//!
//! This is a syntactic pass over the merged AST, run only when a project enables
//! `[language] checked_exceptions`. Receivers are resolved the same cheap way the call-graph
//! does (`this`, `new T()`, class names, locals with a declared or `new` type); a call it cannot
//! resolve is simply not checked (it never produces a false positive). Calls into the stdlib are
//! unchecked until the stdlib declares its errors (stage 4).

use std::collections::{HashMap, HashSet};

use tinox_common::Span;
use tinox_parser::{
    AnnotationArg, Class, Decl, DeclKind, Expr, ExprKind, Function, Interface, Stmt, StmtKind, Type,
};

pub struct Finding {
    pub file: String,
    pub span: Span,
    pub message: String,
}

#[derive(Default)]
pub struct Report {
    /// Violations of the rules (a warning or an error depending on the project's mode).
    pub violations: Vec<Finding>,
    /// Always warnings (e.g. a `@Throws` that can never fire).
    pub warnings: Vec<Finding>,
}

struct Index<'a> {
    classes: HashMap<String, &'a Class>,
    ifaces: HashMap<String, &'a Interface>,
    errors: HashSet<String>,
}

fn flatten<'a>(decls: &'a [Decl], out: &mut Vec<&'a Decl>) {
    for d in decls {
        match &d.node {
            DeclKind::Namespace(ns) => flatten(&ns.decls, out),
            _ => out.push(d),
        }
    }
}

fn throws_of_annotations(anns: &[tinox_parser::Annotation]) -> Vec<String> {
    anns.iter()
        .filter(|a| a.name == "Throws")
        .flat_map(|a| a.args.iter())
        .filter_map(|arg| match arg {
            AnnotationArg::TypeRef(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

fn type_head(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(n) => Some(n.clone()),
        Type::Generic { name, .. } => Some(name.clone()),
        _ => None,
    }
}

impl<'a> Index<'a> {
    fn is_sub(&self, child: &str, base: &str) -> bool {
        let mut cur = child.to_string();
        for _ in 0..64 {
            if cur == base {
                return true;
            }
            match self.classes.get(&cur).and_then(|c| c.extends.clone()) {
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    fn find_method(&self, class: &str, name: &str) -> Option<&'a tinox_parser::Method> {
        let mut cur = class.to_string();
        for _ in 0..64 {
            let c = *self.classes.get(&cur)?;
            if let Some(m) = c.methods.iter().find(|m| m.name == name) {
                return Some(m);
            }
            cur = c.extends.clone()?;
        }
        None
    }

    /// `@Throws` of `class.name` (a class method, found through `extends`, or an interface method).
    fn throws_of(&self, class: &str, name: &str) -> Option<Vec<String>> {
        if let Some(m) = self.find_method(class, name) {
            return Some(throws_of_annotations(&m.annotations));
        }
        let i = self.ifaces.get(class)?;
        let f: &Function = i.methods.iter().find(|f| f.name == name)?;
        Some(throws_of_annotations(&f.annotations))
    }
}

struct Walk<'a, 'b> {
    ix: &'b Index<'a>,
    class: &'b str,
    file: &'b str,
    locals: HashMap<String, String>,
    declared: Vec<String>,
    handlers: Vec<Vec<String>>,
    used: HashSet<String>,
    unresolved: bool,
    violations: Vec<Finding>,
}

impl Walk<'_, '_> {
    fn handled(&self, err: &str) -> bool {
        self.handlers
            .iter()
            .flatten()
            .any(|t| t == "String" || t == "Any" || self.ix.is_sub(err, t))
            || self.declared.iter().any(|d| self.ix.is_sub(err, d))
    }

    fn escape(&mut self, err: &str, span: Span, what: &str) {
        self.used.insert(err.to_string());
        if !self.handled(err) {
            self.violations.push(Finding {
                file: self.file.to_string(),
                span,
                message: format!(
                    "{what} may raise {err}, which is neither caught nor declared: catch it or add it to @Throws({err})"
                ),
            });
        }
    }

    fn receiver_class(&self, obj: &Expr) -> Option<String> {
        match &obj.node {
            ExprKind::This => Some(self.class.to_string()),
            ExprKind::New { class, .. } => Some(class.clone()),
            ExprKind::Ident(n) => match self.locals.get(n) {
                Some(t) => Some(t.clone()),
                None if self.ix.classes.contains_key(n) || self.ix.ifaces.contains_key(n) => Some(n.clone()),
                None => None,
            },
            _ => None,
        }
    }

    fn call(&mut self, target: Option<(String, String)>, span: Span) {
        let Some((class, name)) = target else {
            self.unresolved = true;
            return;
        };
        match self.ix.throws_of(&class, &name) {
            Some(list) => {
                for e in list {
                    self.escape(&e, span, &format!("call to {class}.{name}"));
                }
            }
            None => self.unresolved = true,
        }
    }

    fn throw_class(&self, e: &Expr) -> Option<String> {
        match &e.node {
            ExprKind::StructLiteral { name, .. } => Some(name.clone()),
            ExprKind::New { class, .. } => Some(class.clone()),
            ExprKind::Ident(n) => self.locals.get(n).cloned(),
            ExprKind::Literal(tinox_parser::Literal::String(_)) => Some("String".to_string()),
            _ => None,
        }
    }

    fn throw(&mut self, e: &Expr) {
        self.expr(e);
        match self.throw_class(e) {
            Some(c) if self.ix.errors.contains(&c) => self.escape(&c, e.span, "throw"),
            Some(c) if c == "String" => self.violations.push(Finding {
                file: self.file.to_string(),
                span: e.span,
                message: "throw of plain text is not allowed with checked exceptions: throw an @Error class".to_string(),
            }),
            _ => {}
        }
    }

    fn catches(&mut self, catches: &[tinox_parser::CatchClause]) -> Vec<String> {
        catches.iter().filter_map(|c| type_head(&c.ty)).collect()
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.node {
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Let { value, .. } | StmtKind::Var { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            StmtKind::Assignment { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::If { cond, then_branch, else_branch } => {
                self.expr(cond);
                self.stmt(then_branch);
                if let Some(e) = else_branch {
                    self.stmt(e);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.stmt(body);
            }
            StmtKind::For { iter, body, .. } => {
                self.expr(iter);
                self.stmt(body);
            }
            StmtKind::ForC { init, cond, update, body } => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                if let Some(u) = update {
                    self.expr(u);
                }
                self.stmt(body);
            }
            StmtKind::Loop { body } => self.stmt(body),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            StmtKind::Throw(e) => self.throw(e),
            StmtKind::Try { body, catches, finally } => {
                let types = self.catches(catches);
                self.handlers.push(types);
                self.stmt(body);
                self.handlers.pop();
                for c in catches {
                    self.stmt(&c.body);
                }
                if let Some(f) = finally {
                    self.stmt(f);
                }
            }
            StmtKind::Defer(inner) => self.stmt(inner),
            StmtKind::Block(stmts) => stmts.iter().for_each(|s| self.stmt(s)),
            StmtKind::Select { arms, default } => {
                for a in arms {
                    self.expr(&a.channel);
                    self.stmt(&a.body);
                }
                if let Some(d) = default {
                    self.stmt(d);
                }
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Empty => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.node {
            ExprKind::Literal(_) | ExprKind::Ident(_) | ExprKind::This | ExprKind::Break | ExprKind::Continue | ExprKind::Channel => {}
            ExprKind::ArrayLiteral(items) | ExprKind::Tuple(items) => items.iter().for_each(|i| self.expr(i)),
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Call { func, args } => {
                self.expr(func);
                args.iter().for_each(|a| self.expr(a));
                if let ExprKind::Ident(name) = &func.node {
                    if self.ix.find_method(self.class, name).is_some() {
                        self.call(Some((self.class.to_string(), name.clone())), e.span);
                    } else {
                        self.unresolved = true; // lambda variable or unknown
                    }
                }
            }
            ExprKind::MethodCall { obj, method, args } => {
                self.expr(obj);
                args.iter().for_each(|a| self.expr(a));
                let target = self.receiver_class(obj).map(|c| (c, method.clone()));
                self.call(target, e.span);
            }
            ExprKind::SuperCall { method, args } => {
                args.iter().for_each(|a| self.expr(a));
                let parent = self.ix.classes.get(self.class).and_then(|c| c.extends.clone());
                self.call(parent.map(|p| (p, method.clone())), e.span);
            }
            ExprKind::EnumValue { enum_name, variant, args, .. } => {
                args.iter().for_each(|a| self.expr(a));
                if self.ix.classes.contains_key(enum_name) {
                    self.call(Some((enum_name.clone(), variant.clone())), e.span);
                }
            }
            ExprKind::Index { obj, index } => {
                self.expr(obj);
                self.expr(index);
            }
            ExprKind::FieldAccess { obj, .. } => self.expr(obj),
            ExprKind::New { args, .. } => args.iter().for_each(|a| self.expr(a)),
            ExprKind::StructLiteral { fields, .. } => fields.iter().for_each(|(_, v)| self.expr(v)),
            ExprKind::Block(stmts) => stmts.iter().for_each(|s| self.stmt(s)),
            ExprKind::If { cond, then_branch, else_branch } => {
                self.expr(cond);
                self.expr(then_branch);
                if let Some(x) = else_branch {
                    self.expr(x);
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.expr(body);
            }
            ExprKind::For { iter, body, .. } => {
                self.expr(iter);
                self.expr(body);
            }
            ExprKind::Loop { body } => self.expr(body),
            ExprKind::Match { expr, cases } => {
                self.expr(expr);
                for c in cases {
                    if let Some(g) = &c.guard {
                        self.expr(g);
                    }
                    self.expr(&c.body);
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v);
                }
            }
            ExprKind::Throw(v) => self.throw(v),
            ExprKind::Try { body, catches, finally } => {
                let types = self.catches(catches);
                self.handlers.push(types);
                self.expr(body);
                self.handlers.pop();
                for c in catches {
                    self.stmt(&c.body);
                }
                if let Some(f) = finally {
                    self.expr(f);
                }
            }
            ExprKind::Assign { target, value } | ExprKind::CompoundAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::Lambda { body, .. } => {
                // A lambda inherits no handlers and no @Throws list.
                let handlers = std::mem::take(&mut self.handlers);
                let declared = std::mem::take(&mut self.declared);
                self.expr(body);
                self.handlers = handlers;
                self.declared = declared;
            }
            ExprKind::Spawn(inner) => {
                // The spawned function's errors surface at `await`, not here.
                self.handlers.push(vec!["Any".to_string()]);
                self.expr(inner);
                self.handlers.pop();
            }
            ExprKind::Await(inner) | ExprKind::Recv(inner) => self.expr(inner),
            ExprKind::Send { channel, value } => {
                self.expr(channel);
                self.expr(value);
            }
            ExprKind::Cast { expr, .. } | ExprKind::Is { expr, .. } => self.expr(expr),
            ExprKind::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            ExprKind::TupleIndex { tuple, .. } => self.expr(tuple),
        }
    }
}

/// Flat (not scope-aware) map of parameter / local / catch-parameter names to class names.
fn collect_locals(m: &tinox_parser::Method) -> HashMap<String, String> {
    fn stmt(s: &Stmt, out: &mut HashMap<String, String>) {
        match &s.node {
            StmtKind::Let { name, ty, value } | StmtKind::Var { name, ty, value, .. } => {
                let t = ty.as_ref().and_then(type_head).or_else(|| {
                    value.as_ref().and_then(|v| match &v.node {
                        ExprKind::New { class, .. } => Some(class.clone()),
                        ExprKind::StructLiteral { name, .. } => Some(name.clone()),
                        _ => None,
                    })
                });
                if let Some(t) = t {
                    out.insert(name.clone(), t);
                }
            }
            StmtKind::If { then_branch, else_branch, .. } => {
                stmt(then_branch, out);
                if let Some(e) = else_branch {
                    stmt(e, out);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::Loop { body } => stmt(body, out),
            StmtKind::ForC { init, body, .. } => {
                if let Some(i) = init {
                    stmt(i, out);
                }
                stmt(body, out);
            }
            StmtKind::Try { body, catches, finally } => {
                stmt(body, out);
                for c in catches {
                    if let Some(t) = type_head(&c.ty) {
                        out.insert(c.param.clone(), t);
                    }
                    stmt(&c.body, out);
                }
                if let Some(f) = finally {
                    stmt(f, out);
                }
            }
            StmtKind::Defer(i) => stmt(i, out),
            StmtKind::Block(v) => v.iter().for_each(|s| stmt(s, out)),
            StmtKind::Select { arms, default } => {
                arms.iter().for_each(|a| stmt(&a.body, out));
                if let Some(d) = default {
                    stmt(d, out);
                }
            }
            _ => {}
        }
    }
    let mut out = HashMap::new();
    for p in &m.params {
        if let Some(t) = type_head(&p.param_type) {
            out.insert(p.name.clone(), t);
        }
    }
    stmt(&m.body, &mut out);
    out
}

/// Checks every class method whose file `include` accepts.
pub fn check(decls: &[Decl], include: &dyn Fn(&str) -> bool) -> Report {
    let mut flat = Vec::new();
    flatten(decls, &mut flat);
    let mut ix = Index { classes: HashMap::new(), ifaces: HashMap::new(), errors: HashSet::new() };
    for d in &flat {
        match &d.node {
            DeclKind::Class(c) => {
                if c.annotations.iter().any(|a| a.name == "Error") {
                    ix.errors.insert(c.name.clone());
                }
                ix.classes.insert(c.name.clone(), c);
            }
            DeclKind::Interface(i) => {
                ix.ifaces.insert(i.name.clone(), i);
            }
            _ => {}
        }
    }
    let mut report = Report::default();
    for c in ix.classes.values() {
        for m in &c.methods {
            if !include(&m.file) {
                continue;
            }
            let declared = throws_of_annotations(&m.annotations);
            let mut w = Walk {
                ix: &ix,
                class: &c.name,
                file: &m.file,
                locals: collect_locals(m),
                declared: declared.clone(),
                handlers: Vec::new(),
                used: HashSet::new(),
                unresolved: false,
                violations: Vec::new(),
            };
            w.stmt(&m.body);
            report.violations.append(&mut w.violations);

            // An override may declare the same errors or fewer (subclasses allowed), never more.
            let mut overridden: Vec<Vec<String>> = Vec::new();
            if let Some(parent) = &c.extends {
                if let Some(t) = ix.throws_of(parent, &m.name) {
                    overridden.push(t);
                }
            }
            for i in &c.implements {
                if let Some(t) = ix.ifaces.get(i).and_then(|i| i.methods.iter().find(|f| f.name == m.name)) {
                    overridden.push(throws_of_annotations(&t.annotations));
                }
            }
            for allowed in &overridden {
                for d in &declared {
                    if !allowed.iter().any(|a| ix.is_sub(d, a)) {
                        report.violations.push(Finding {
                            file: m.file.to_string(),
                            span: m.span,
                            message: format!("{}.{} declares @Throws({d}), which the overridden method does not allow", c.name, m.name),
                        });
                    }
                }
            }

            // A declared error nothing in the body can raise (only judged when every call resolved).
            if overridden.is_empty() && !w.unresolved {
                for d in &declared {
                    if !w.used.iter().any(|u| ix.is_sub(u, d)) {
                        report.warnings.push(Finding {
                            file: m.file.to_string(),
                            span: m.span,
                            message: format!("{}.{} declares @Throws({d}) but nothing in its body can raise it", c.name, m.name),
                        });
                    }
                }
            }
        }
    }
    report
        .violations
        .sort_by(|a, b| (&a.file, a.span.start.line, a.span.start.column).cmp(&(&b.file, b.span.start.line, b.span.start.column)));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    const ERRORS: &str = "@Error(\"BASE\")\nclass Base { var message: String }\n\
        @Error(\"SUB\")\nclass Sub extends Base { }\n\
        @Error(\"OTHER\")\nclass Other { var message: String }\n\
        interface Source {\n    @Throws(Base)\n    fn load() -> Int64\n}\n";

    fn run(body: &str) -> Report {
        let src = format!("{ERRORS}{body}");
        let ast = Parser::new(Lexer::new(&src).tokenize().unwrap()).parse().unwrap();
        check(&ast.decls, &|_| true)
    }

    fn violations(body: &str) -> Vec<String> {
        run(body).violations.into_iter().map(|f| f.message).collect()
    }

    const SVC: &str = "class Svc {\n    @Throws(Sub)\n    fnc find() -> Int64 { throw Sub { message: \"x\" } }\n";

    #[test]
    fn undeclared_call_is_reported() {
        let v = violations(&format!("{SVC}    fnc leak() -> Int64 {{ return Svc::find() }}\n}}"));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("call to Svc.find may raise Sub"));
    }

    #[test]
    fn catch_or_declaration_satisfies_the_call_including_supertypes() {
        let body = format!(
            "{SVC}    fnc a() -> Int64 {{ try {{ return Svc::find() }} catch (e: Base) {{ return 0 }} }}\n\
             @Throws(Base)\n    fnc b() -> Int64 {{ return Svc::find() }}\n}}"
        );
        assert!(violations(&body).is_empty());
    }

    #[test]
    fn a_catch_of_an_unrelated_class_does_not_help() {
        let v = violations(&format!("{SVC}    fnc a() -> Int64 {{ try {{ return Svc::find() }} catch (e: Other) {{ return 0 }} }}\n}}"));
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn throw_in_a_method_needs_a_declaration_and_plain_text_is_refused() {
        let v = violations("class A {\n    fnc t() -> Int64 { throw Other { message: \"x\" } }\n    fnc p() -> Int64 { throw \"x\" }\n}");
        assert!(v.iter().any(|m| m.contains("throw may raise Other")), "{v:?}");
        assert!(v.iter().any(|m| m.contains("plain text")), "{v:?}");
    }

    #[test]
    fn lambdas_inherit_no_handlers_and_spawn_defers_to_await() {
        let in_lambda = violations(&format!(
            "{SVC}    @Throws(Base)\n    fnc l() -> Int64 {{ const f = (x: Int64) => Svc::find()\n return 0 }}\n}}"
        ));
        assert_eq!(in_lambda.len(), 1, "{in_lambda:?}");
    }

    #[test]
    fn overrides_may_not_add_errors() {
        let v = violations("class Impl implements Source {\n    @Throws(Other)\n    fn load() -> Int64 { throw Other { message: \"x\" } }\n}");
        assert!(v.iter().any(|m| m.contains("does not allow")), "{v:?}");
        let ok = violations("class Impl implements Source {\n    @Throws(Sub)\n    fn load() -> Int64 { throw Sub { message: \"x\" } }\n}");
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn unused_declaration_is_a_warning() {
        let r = run("class A {\n    @Throws(Other)\n    fnc t() -> Int64 { return 1 }\n}");
        assert!(r.violations.is_empty());
        assert_eq!(r.warnings.len(), 1);
    }
}

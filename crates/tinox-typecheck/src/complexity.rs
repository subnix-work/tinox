//! Code complexity limits, enforced by the compiler (`[analysis]` in tinox.toml).
//!
//! A syntactic pass over the merged AST, like `throws`: it measures every function and method
//! the caller considers project code and reports the ones that exceed a configured limit.
//!
//! Metrics (per function/method; lambdas count towards their enclosing function):
//! - cyclomatic complexity: 1 + `if` + loops + `match` cases + `catch` clauses + `select` arms
//!   + short-circuit `&&` / `||`
//! - nesting depth: deepest chain of nested control structures (`else if` does not nest)
//! - length: lines from the function's first to last line
//! - parameter count
//!
//! `@AllowComplexity` on a method, function or class opts it out of every limit.

use tinox_common::Span;
use tinox_parser::{BinaryOp, Decl, DeclKind, Expr, ExprKind, Function, Method, Stmt, StmtKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_cyclomatic: u32,
    pub max_nesting: u32,
    pub max_function_lines: u32,
    pub max_params: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_cyclomatic: 15, max_nesting: 4, max_function_lines: 80, max_params: 7 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub cyclomatic: u32,
    pub nesting: u32,
    pub lines: u32,
    pub params: u32,
}

pub struct Finding {
    pub file: String,
    pub span: Span,
    pub message: String,
}

/// Measures one function body. `lines` and `params` are filled in by the caller.
fn measure_body(body: &Stmt) -> (u32, u32, u32) {
    let mut w = Walker { cyclomatic: 1, max_depth: 0, last_line: body.span.start.line };
    w.stmt(body, 0);
    (w.cyclomatic, w.max_depth, w.last_line)
}

struct Walker {
    cyclomatic: u32,
    max_depth: u32,
    /// Last source line on which any node of the body starts (spans carry only a start position).
    last_line: u32,
}

impl Walker {
    /// Enters a control structure at `depth` (its own nesting level, 1 = outermost).
    fn enter(&mut self, depth: u32) {
        self.max_depth = self.max_depth.max(depth);
    }

    fn stmt(&mut self, s: &Stmt, depth: u32) {
        self.last_line = self.last_line.max(s.span.start.line);
        match &s.node {
            StmtKind::Expr(e) => self.expr(e, depth),
            StmtKind::Let { value, .. } | StmtKind::Var { value, .. } => {
                if let Some(v) = value {
                    self.expr(v, depth);
                }
            }
            StmtKind::Assignment { target, value } => {
                self.expr(target, depth);
                self.expr(value, depth);
            }
            StmtKind::If { cond, then_branch, else_branch } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(cond, depth);
                self.stmt(then_branch, depth + 1);
                if let Some(e) = else_branch {
                    // `else if` is a chain, not a deeper nesting level.
                    if matches!(e.node, StmtKind::If { .. }) {
                        self.stmt(e, depth);
                    } else {
                        self.stmt(e, depth + 1);
                    }
                }
            }
            StmtKind::While { cond, body } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(cond, depth);
                self.stmt(body, depth + 1);
            }
            StmtKind::For { iter, body, .. } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(iter, depth);
                self.stmt(body, depth + 1);
            }
            StmtKind::ForC { init, cond, update, body } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                if let Some(i) = init {
                    self.stmt(i, depth);
                }
                if let Some(c) = cond {
                    self.expr(c, depth);
                }
                if let Some(u) = update {
                    self.expr(u, depth);
                }
                self.stmt(body, depth + 1);
            }
            StmtKind::Loop { body } => {
                self.enter(depth + 1);
                self.stmt(body, depth + 1);
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, depth);
                }
            }
            StmtKind::Throw(e) => self.expr(e, depth),
            StmtKind::Try { body, catches, finally } => {
                self.enter(depth + 1);
                self.stmt(body, depth + 1);
                for c in catches {
                    self.cyclomatic += 1;
                    self.stmt(&c.body, depth + 1);
                }
                if let Some(f) = finally {
                    self.stmt(f, depth + 1);
                }
            }
            StmtKind::Defer(b) => self.stmt(b, depth),
            StmtKind::Block(stmts) => {
                for s in stmts {
                    self.stmt(s, depth);
                }
            }
            StmtKind::Select { arms, default } => {
                self.enter(depth + 1);
                for arm in arms {
                    self.cyclomatic += 1;
                    self.expr(&arm.channel, depth);
                    self.stmt(&arm.body, depth + 1);
                }
                if let Some(d) = default {
                    self.stmt(d, depth + 1);
                }
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Empty => {}
        }
    }

    fn expr(&mut self, e: &Expr, depth: u32) {
        self.last_line = self.last_line.max(e.span.start.line);
        match &e.node {
            ExprKind::Literal(_) | ExprKind::Ident(_) | ExprKind::This | ExprKind::Channel
            | ExprKind::Break | ExprKind::Continue => {}
            ExprKind::ArrayLiteral(items) | ExprKind::Tuple(items) => {
                items.iter().for_each(|i| self.expr(i, depth));
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.expr(k, depth);
                    self.expr(v, depth);
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    self.cyclomatic += 1;
                }
                self.expr(lhs, depth);
                self.expr(rhs, depth);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand, depth),
            ExprKind::Call { func, args } => {
                self.expr(func, depth);
                args.iter().for_each(|a| self.expr(a, depth));
            }
            ExprKind::MethodCall { obj, args, .. } => {
                self.expr(obj, depth);
                args.iter().for_each(|a| self.expr(a, depth));
            }
            ExprKind::Index { obj, index } => {
                self.expr(obj, depth);
                self.expr(index, depth);
            }
            ExprKind::FieldAccess { obj, .. } => self.expr(obj, depth),
            ExprKind::SuperCall { args, .. } | ExprKind::New { args, .. } | ExprKind::EnumValue { args, .. } => {
                args.iter().for_each(|a| self.expr(a, depth));
            }
            ExprKind::StructLiteral { fields, .. } => fields.iter().for_each(|(_, v)| self.expr(v, depth)),
            ExprKind::Block(stmts) => stmts.iter().for_each(|s| self.stmt(s, depth)),
            ExprKind::If { cond, then_branch, else_branch } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(cond, depth);
                self.expr(then_branch, depth + 1);
                if let Some(e) = else_branch {
                    if matches!(e.node, ExprKind::If { .. }) {
                        self.expr(e, depth);
                    } else {
                        self.expr(e, depth + 1);
                    }
                }
            }
            ExprKind::While { cond, body } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(cond, depth);
                self.expr(body, depth + 1);
            }
            ExprKind::For { iter, body, .. } => {
                self.cyclomatic += 1;
                self.enter(depth + 1);
                self.expr(iter, depth);
                self.expr(body, depth + 1);
            }
            ExprKind::Loop { body } => {
                self.enter(depth + 1);
                self.expr(body, depth + 1);
            }
            ExprKind::Match { expr, cases } => {
                self.enter(depth + 1);
                self.expr(expr, depth);
                for c in cases {
                    self.cyclomatic += 1;
                    if let Some(g) = &c.guard {
                        self.cyclomatic += 1;
                        self.expr(g, depth + 1);
                    }
                    self.expr(&c.body, depth + 1);
                }
            }
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.expr(v, depth);
                }
            }
            ExprKind::Throw(v) | ExprKind::Spawn(v) | ExprKind::Await(v) | ExprKind::Recv(v) => self.expr(v, depth),
            ExprKind::Try { body, catches, finally } => {
                self.enter(depth + 1);
                self.expr(body, depth + 1);
                for c in catches {
                    self.cyclomatic += 1;
                    self.stmt(&c.body, depth + 1);
                }
                if let Some(f) = finally {
                    self.expr(f, depth + 1);
                }
            }
            ExprKind::Assign { target, value } | ExprKind::CompoundAssign { target, value, .. } => {
                self.expr(target, depth);
                self.expr(value, depth);
            }
            ExprKind::Lambda { body, .. } => {
                self.enter(depth + 1);
                self.expr(body, depth + 1);
            }
            ExprKind::Send { channel, value } => {
                self.expr(channel, depth);
                self.expr(value, depth);
            }
            ExprKind::Cast { expr, .. } | ExprKind::Is { expr, .. } => self.expr(expr, depth),
            ExprKind::Range { start, end, .. } => {
                self.expr(start, depth);
                self.expr(end, depth);
            }
            ExprKind::TupleIndex { tuple, .. } => self.expr(tuple, depth),
        }
    }
}

pub fn measure(body: &Stmt, span: Span, params: usize) -> Metrics {
    let (cyclomatic, nesting, last_line) = measure_body(body);
    // Spans have no end position: the length runs to the last statement's starting line plus
    // the function's closing brace, so a trailing multi-line construct is undercounted slightly.
    let lines = last_line.saturating_sub(span.start.line) + 2;
    Metrics { cyclomatic, nesting, lines, params: params as u32 }
}

fn exceeded(m: &Metrics, l: &Limits) -> Vec<String> {
    let mut out = Vec::new();
    if m.cyclomatic > l.max_cyclomatic {
        out.push(format!("cyclomatic complexity {} (max_cyclomatic = {})", m.cyclomatic, l.max_cyclomatic));
    }
    if m.nesting > l.max_nesting {
        out.push(format!("nesting depth {} (max_nesting = {})", m.nesting, l.max_nesting));
    }
    if m.lines > l.max_function_lines {
        out.push(format!("{} lines (max_function_lines = {})", m.lines, l.max_function_lines));
    }
    if m.params > l.max_params {
        out.push(format!("{} parameters (max_params = {})", m.params, l.max_params));
    }
    out
}

fn allowed(anns: &[tinox_parser::Annotation]) -> bool {
    anns.iter().any(|a| a.name == "AllowComplexity")
}

fn report(name: &str, file: &str, span: Span, m: Metrics, l: &Limits, out: &mut Vec<Finding>) {
    let problems = exceeded(&m, l);
    if problems.is_empty() {
        return;
    }
    out.push(Finding {
        file: file.to_string(),
        span,
        message: format!(
            "'{name}' is too complex: {}. Split it up, raise the limit in [analysis] of tinox.toml, \
             or annotate it with @AllowComplexity",
            problems.join(", ")
        ),
    });
}

fn check_function(f: &Function, in_scope: &dyn Fn(&str) -> bool, l: &Limits, out: &mut Vec<Finding>) {
    if allowed(&f.annotations) || !in_scope(&f.file) {
        return;
    }
    report(&f.name, &f.file, f.span, measure(&f.body, f.span, f.params.len()), l, out);
}

fn check_method(class: &str, m: &Method, in_scope: &dyn Fn(&str) -> bool, l: &Limits, out: &mut Vec<Finding>) {
    if allowed(&m.annotations) || !in_scope(&m.file) {
        return;
    }
    let name = format!("{class}.{}", m.name);
    report(&name, &m.file, m.span, measure(&m.body, m.span, m.params.len()), l, out);
}

fn walk(decls: &[Decl], in_scope: &dyn Fn(&str) -> bool, l: &Limits, out: &mut Vec<Finding>) {
    for d in decls {
        match &d.node {
            DeclKind::Function(f) => check_function(f, in_scope, l, out),
            DeclKind::Class(c) => {
                // Hidden helper classes the compiler synthesizes (`__Enum_*`) are not user code.
                if c.name.starts_with("__") || allowed(&c.annotations) {
                    continue;
                }
                for m in &c.methods {
                    check_method(&c.name, m, in_scope, l, out);
                }
            }
            DeclKind::Trait(t) => {
                if !allowed(&t.annotations) {
                    t.methods.iter().for_each(|f| check_function(f, in_scope, l, out));
                }
            }
            DeclKind::Interface(i) => {
                if !allowed(&i.annotations) {
                    i.methods.iter().for_each(|f| check_function(f, in_scope, l, out));
                }
            }
            DeclKind::Namespace(ns) => walk(&ns.decls, in_scope, l, out),
            _ => {}
        }
    }
}

/// Returns every function/method in `decls` (restricted to files `in_scope` accepts) that
/// exceeds `limits`, in source order of declaration.
pub fn check(decls: &[Decl], limits: &Limits, in_scope: &dyn Fn(&str) -> bool) -> Vec<Finding> {
    let mut out = Vec::new();
    walk(decls, in_scope, limits, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(src: &str, limits: Limits) -> Vec<Finding> {
        let tokens = tinox_lexer::Lexer::new(src).tokenize().expect("lex");
        let ast = tinox_parser::Parser::new(tokens).parse().expect("parse");
        check(&ast.decls, &limits, &|_| true)
    }

    fn wrap(body: &str, extra: &str) -> String {
        format!("class C {{\n{extra}\n fnc f(a: Int64, b: Int64) -> Int64 {{\n{body}\n return 0\n }}\n}}\n")
    }

    #[test]
    fn simple_function_is_clean() {
        assert!(findings(&wrap("if (a > b) { return 1 }", ""), Limits::default()).is_empty());
    }

    #[test]
    fn counts_branches_loops_and_short_circuit() {
        let l = Limits { max_cyclomatic: 4, ..Limits::default() };
        // 1 + if + while + && = 4 -> fine; one more `||` -> 5 -> too complex
        assert!(findings(&wrap("if (a > b) { return 1 }\nwhile (a < b && b > 0) { return 2 }", ""), l).is_empty());
        let f = findings(&wrap("if (a > b || a == 0) { return 1 }\nwhile (a < b && b > 0) { return 2 }", ""), l);
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("cyclomatic complexity 5"), "{}", f[0].message);
    }

    #[test]
    fn else_if_chain_does_not_nest() {
        let l = Limits { max_nesting: 1, ..Limits::default() };
        let body = "if (a == 1) { return 1 } else if (a == 2) { return 2 } else if (a == 3) { return 3 } else { return 4 }";
        assert!(findings(&wrap(body, ""), l).is_empty());
    }

    #[test]
    fn nesting_depth_is_limited() {
        let l = Limits { max_nesting: 2, ..Limits::default() };
        let f = findings(&wrap("if (a > 0) { if (b > 0) { if (a > b) { return 1 } } }", ""), l);
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("nesting depth 3"), "{}", f[0].message);
    }

    #[test]
    fn param_and_line_limits() {
        let l = Limits { max_params: 1, ..Limits::default() };
        assert_eq!(findings(&wrap("", ""), l).len(), 1);
        let l = Limits { max_function_lines: 2, ..Limits::default() };
        assert_eq!(findings(&wrap("println(\"a\")\nprintln(\"b\")", ""), l).len(), 1);
    }

    #[test]
    fn allow_complexity_exempts_method_and_class() {
        let l = Limits { max_params: 1, ..Limits::default() };
        let method = "class C {\n @AllowComplexity(\"legacy\")\n fnc f(a: Int64, b: Int64) -> Int64 { return 0 }\n}\n";
        assert!(findings(method, l).is_empty());
        let class = "@AllowComplexity\nclass C {\n fnc f(a: Int64, b: Int64) -> Int64 { return 0 }\n}\n";
        assert!(findings(class, l).is_empty());
    }
}

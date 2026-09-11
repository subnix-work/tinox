use crate::ast::*;

pub struct Formatter {
    indent: usize,
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

impl Formatter {
    pub fn new() -> Self {
        Self { indent: 0 }
    }

    pub fn format(&mut self, source: &SourceFile) -> String {
        let mut out = String::new();
        for (i, decl) in source.decls.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&self.fmt_decl(&decl.node));
            out.push('\n');
        }
        out
    }

    fn ind(&self) -> String {
        "    ".repeat(self.indent)
    }

    fn fmt_doc(&self, doc: &Option<String>) -> String {
        match doc {
            Some(d) => {
                let lines: Vec<&str> = d.lines().collect();
                if lines.len() <= 1 {
                    let content = d.trim();
                    let content = content.strip_suffix("*/").unwrap_or(content).trim();
                    format!("{}/** {} */\n", self.ind(), content)
                } else {
                    let mut out = String::new();
                    out.push_str(&format!("{}/**\n", self.ind()));
                    for line in &lines {
                        let trimmed = line.trim();
                        // Skip the closing */
                        if trimmed == "*/" || trimmed.is_empty() {
                            continue;
                        }
                        let mut cleaned = trimmed.trim_start_matches('*');
                        cleaned = cleaned.strip_suffix("*/").unwrap_or(cleaned);
                        let cleaned = cleaned.trim();
                        if !cleaned.is_empty() {
                            out.push_str(&format!("{} * {}\n", self.ind(), cleaned));
                        }
                    }
                    out.push_str(&format!("{} */\n", self.ind()));
                    out
                }
            }
            None => String::new(),
        }
    }

    fn fmt_decl(&mut self, decl: &DeclKind) -> String {
        match decl {
            DeclKind::Function(f) => self.fmt_fn(f),
            DeclKind::Class(c) => self.fmt_class(c),
            DeclKind::Interface(i) => self.fmt_interface(i),
            DeclKind::Enum(e) => self.fmt_enum(e),
            DeclKind::Trait(t) => self.fmt_trait(t),
            DeclKind::Import(i) => self.fmt_import(i),
            DeclKind::Module(name) => format!("module {};", name),
            DeclKind::Namespace(ns) => self.fmt_namespace(ns),
            DeclKind::Immutable(u) => {
                let fields: Vec<String> = u.fields.iter()
                    .map(|f| format!("{}: {}", f.name, self.fmt_type(&f.param_type)))
                    .collect();
                format!("{}immutable {}({})", self.fmt_doc(&u.doc), u.name, fields.join(", "))
            }
        }
    }

fn fmt_annotations(&self, annotations: &[Annotation]) -> String {
        let mut out = String::new();
        for ann in annotations {
            if ann.args.is_empty() {
                out.push_str(&format!("{}@{}\n", self.ind(), ann.name));
            } else {
                let args: Vec<String> = ann.args.iter().map(fmt_annotation_arg).collect();
                out.push_str(&format!("{}@{}({})\n", self.ind(), ann.name, args.join(", ")));
            }
        }
        out
    }

    fn fmt_fn(&mut self, f: &Function) -> String {
        let ann = self.fmt_annotations(&f.annotations);
        let doc = self.fmt_doc(&f.doc);
        let async_ = if f.is_async { "async " } else { "" };
        let type_params = fmt_type_params(&f.type_params);
        let params = self.fmt_params(&f.params);
        let ret = self.fmt_type(&f.ret_type);
        let sig = format!("{}fn {}{}({}) -> {}", async_, f.name, type_params, params, ret);
        format!("{}{}{}\n{}", ann, doc, sig, self.fmt_block_stmt(&f.body))
    }

    fn fmt_method(&mut self, m: &Method) -> String {
        let ann = self.fmt_annotations(&m.annotations);
        let doc = self.fmt_doc(&m.doc);
        let async_ = if m.is_async { "async " } else { "" };
        let vis = fmt_visibility(&m.visibility);
        let fn_kw = if m.static_ { "fnc" } else { "fn" };
        let params = self.fmt_params(&m.params);
        let ret = self.fmt_type(&m.ret_type);
        let sig = format!("{}{}{}{} {}({}) -> {}", self.ind(), vis, async_, fn_kw, m.name, params, ret);
        format!("{}{}{}\n{}", ann, doc, sig, self.fmt_block_stmt(&m.body))
    }

    fn fmt_namespace(&mut self, ns: &Namespace) -> String {
        let mut body = String::new();
        self.indent += 1;
        for (i, decl) in ns.decls.iter().enumerate() {
            if i > 0 {
                body.push('\n');
            }
            body.push_str(&format!("{}{}\n", self.ind(), self.fmt_decl(&decl.node)));
        }
        self.indent -= 1;
        format!("namespace {}\n{}{{\n{}{}}}", ns.name.join("."), self.ind(), body, self.ind())
    }

    fn fmt_class(&mut self, c: &Class) -> String {
        let ann = self.fmt_annotations(&c.annotations);
        let doc = self.fmt_doc(&c.doc);
        let type_params = fmt_type_params(&c.type_params);
        let mut header = format!("class {}{}", c.name, type_params);
        if let Some(ext) = &c.extends {
            header.push_str(&format!(" extends {}", ext));
        }
        if !c.implements.is_empty() {
            header.push_str(&format!(" implements {}", c.implements.join(", ")));
        }
        let mut body = String::new();
        self.indent += 1;
        for field in &c.fields {
            let field_ann = self.fmt_annotations(&field.annotations);
            let field_doc = self.fmt_doc(&field.doc);
            body.push_str(&format!("{}{}{}{}{}{}: {};\n",
                field_ann,
                field_doc,
                self.ind(),
                fmt_visibility(&field.visibility),
                if field.mutable { "var " } else { "" },
                field.name,
                self.fmt_type(&field.field_type)
            ));
        }
        if !c.fields.is_empty() && !c.methods.is_empty() {
            body.push('\n');
        }
        for (i, method) in c.methods.iter().enumerate() {
            if i > 0 {
                body.push('\n');
            }
            body.push_str(&self.fmt_method(method));
            body.push('\n');
        }
        self.indent -= 1;
        format!("{}{}{}{}\n{}{{\n{}{}}}", ann, doc, self.ind(), header, self.ind(), body, self.ind())
    }

    fn fmt_interface(&mut self, i: &Interface) -> String {
        let ann = self.fmt_annotations(&i.annotations);
        let doc = self.fmt_doc(&i.doc);
        let mut header = format!("interface {}", i.name);
        if !i.extends.is_empty() {
            header.push_str(&format!(" extends {}", i.extends.join(", ")));
        }
        let mut body = String::new();
        self.indent += 1;
        for f in &i.methods {
            let method_doc = self.fmt_doc(&f.doc);
            let type_params = fmt_type_params(&f.type_params);
            let params = self.fmt_params(&f.params);
            let ret = self.fmt_type(&f.ret_type);
            body.push_str(&format!("{}{}fn {}{}({}) -> {};\n",
                self.ind(), method_doc, f.name, type_params, params, ret));
        }
        self.indent -= 1;
        format!("{}{}{}{}\n{}{{\n{}{}}}", ann, doc, self.ind(), header, self.ind(), body, self.ind())
    }

    fn fmt_enum(&mut self, e: &Enum) -> String {
        let ann = self.fmt_annotations(&e.annotations);
        let doc = self.fmt_doc(&e.doc);
        let mut body = String::new();
        self.indent += 1;
        for v in &e.variants {
            let variant_doc = self.fmt_doc(&v.doc);
            if v.args.is_empty() {
                body.push_str(&format!("{}{}{};\n", self.ind(), variant_doc, v.name));
            } else {
                let args: Vec<String> = v.args.iter().map(|t| self.fmt_type(t)).collect();
                body.push_str(&format!("{}{}{}({});\n", self.ind(), variant_doc, v.name, args.join(", ")));
            }
        }
        self.indent -= 1;
        format!("{}{}{}enum {}\n{}{{\n{}{}}}", ann, doc, self.ind(), e.name, self.ind(), body, self.ind())
    }

    fn fmt_trait(&mut self, t: &Trait) -> String {
        let ann = self.fmt_annotations(&t.annotations);
        let doc = self.fmt_doc(&t.doc);
        let mut body = String::new();
        self.indent += 1;
        for f in &t.methods {
            let method_doc = self.fmt_doc(&f.doc);
            let type_params = fmt_type_params(&f.type_params);
            let params = self.fmt_params(&f.params);
            let ret = self.fmt_type(&f.ret_type);
            body.push_str(&format!("{}{}fn {}{}({}) -> {};\n",
                self.ind(), method_doc, f.name, type_params, params, ret));
        }
        self.indent -= 1;
        format!("{}{}{}trait {}\n{}{{\n{}{}}}", ann, doc, self.ind(), t.name, self.ind(), body, self.ind())
    }

    fn fmt_import(&self, i: &Import) -> String {
        let path = i.path.join("::");
        match &i.alias {
            Some(a) => format!("import {} as {};", path, a),
            None => format!("import {};", path),
        }
    }

    fn ind_inner(&self) -> String {
        "    ".repeat(self.indent + 1)
    }

    fn fmt_block_stmt(&mut self, stmt: &Stmt) -> String {
        match &stmt.node {
            StmtKind::Block(stmts) => self.fmt_block(stmts),
            other => {
                let s = self.fmt_stmt_kind(other);
                format!("{}{{\n{}{}\n{}}}", self.ind(), self.ind_inner(), s, self.ind())
            }
        }
    }

    fn fmt_block(&mut self, stmts: &[Stmt]) -> String {
        let mut body = String::new();
        self.indent += 1;
        for stmt in stmts {
            let s = self.fmt_stmt_kind(&stmt.node);
            if !s.is_empty() {
                body.push_str(&format!("{}{}\n", self.ind(), s));
            }
        }
        self.indent -= 1;
        format!("{}{{\n{}{}}}", self.ind(), body, self.ind())
    }

    fn fmt_stmt_kind(&mut self, stmt: &StmtKind) -> String {
        match stmt {
            StmtKind::Expr(e) => format!("{};", self.fmt_expr(&e.node)),
            StmtKind::Let { name, ty, value } => {
                let ty_str = ty.as_ref().map(|t| format!(": {}", self.fmt_type(t))).unwrap_or_default();
                let val_str = value.as_ref().map(|v| format!(" = {}", self.fmt_expr(&v.node))).unwrap_or_default();
                format!("let {}{}{};", name, ty_str, val_str)
            }
            StmtKind::Var { name, ty, value, .. } => {
                let ty_str = ty.as_ref().map(|t| format!(": {}", self.fmt_type(t))).unwrap_or_default();
                let val_str = value.as_ref().map(|v| format!(" = {}", self.fmt_expr(&v.node))).unwrap_or_default();
                format!("var {}{}{};", name, ty_str, val_str)
            }
            StmtKind::Assignment { target, value } => {
                format!("{} = {};", self.fmt_expr(&target.node), self.fmt_expr(&value.node))
            }
            StmtKind::If { cond, then_branch, else_branch } => {
                let cond_str = self.fmt_expr(&cond.node);
                let then_str = self.fmt_block_stmt(then_branch);
                let mut s = format!("if ({})\n{}", cond_str, then_str);
                if let Some(else_b) = else_branch {
                    match &else_b.node {
                        StmtKind::If { .. } => {
                            let else_str = self.fmt_stmt_kind(&else_b.node);
                            s.push_str(&format!("\nelse {}", else_str));
                        }
                        _ => {
                            let else_str = self.fmt_block_stmt(else_b);
                            s.push_str(&format!("\n{}else\n{}", self.ind(), else_str));
                        }
                    }
                }
                s
            }
            StmtKind::While { cond, body } => {
                let cond_str = self.fmt_expr(&cond.node);
                let body_str = self.fmt_block_stmt(body);
                format!("while {}\n{}", cond_str, body_str)
            }
            StmtKind::For { var, iter, body } => {
                let iter_str = self.fmt_expr(&iter.node);
                let body_str = self.fmt_block_stmt(body);
                format!("for {} in {}\n{}", var, iter_str, body_str)
            }
            StmtKind::ForC { init, cond, update, body } => {
                let init_str = init.as_ref().map(|s| {
                    let k = self.fmt_stmt_kind(&s.node);
                    k.trim_end_matches(';').to_string()
                }).unwrap_or_default();
                let cond_str = cond.as_ref().map(|e| self.fmt_expr(&e.node)).unwrap_or_default();
                let upd_str = update.as_ref().map(|e| self.fmt_expr(&e.node)).unwrap_or_default();
                let body_str = self.fmt_block_stmt(body);
                format!("for {}; {}; {}\n{}", init_str, cond_str, upd_str, body_str)
            }
            StmtKind::Loop { body } => {
                let body_str = self.fmt_block_stmt(body);
                format!("loop\n{}", body_str)
            }
            StmtKind::Return(Some(e)) => format!("return {};", self.fmt_expr(&e.node)),
            StmtKind::Return(None) => "return;".to_string(),
            StmtKind::Break => "break;".to_string(),
            StmtKind::Continue => "continue;".to_string(),
            StmtKind::Throw(e) => format!("throw {};", self.fmt_expr(&e.node)),
            StmtKind::Try { body, catches, finally } => {
                let body_str = self.fmt_block_stmt(body);
                let mut s = format!("try\n{}", body_str);
                for catch in catches {
                    let ty = self.fmt_type(&catch.ty);
                    let catch_body = self.fmt_block_stmt(&catch.body);
                    s.push_str(&format!("\n{}catch {}: {}\n{}", self.ind(), catch.param, ty, catch_body));
                }
                if let Some(fin) = finally {
                    let fin_str = self.fmt_block_stmt(fin);
                    s.push_str(&format!("\n{}finally\n{}", self.ind(), fin_str));
                }
                s
            }
            StmtKind::Defer(inner) => {
                let inner_str = self.fmt_stmt_kind(&inner.node);
                format!("defer {{ {} }}", inner_str)
            }
            StmtKind::Block(stmts) => self.fmt_block(stmts),
            StmtKind::Select { arms, default } => {
                let mut body = String::new();
                self.indent += 1;
                for arm in arms {
                    let ch = self.fmt_expr(&arm.channel.node);
                    let arm_body = self.fmt_block_stmt(&arm.body);
                    body.push_str(&format!("{}recv {} -> {}\n{}\n",
                        self.ind(), ch, arm.var, arm_body));
                }
                if let Some(def) = default {
                    let def_body = self.fmt_block_stmt(def);
                    body.push_str(&format!("{}default\n{}\n", self.ind(), def_body));
                }
                self.indent -= 1;
                format!("select\n{}{{\n{}{}}}", self.ind(), body, self.ind())
            }
            StmtKind::Empty => String::new(),
        }
    }

    fn fmt_expr(&mut self, expr: &ExprKind) -> String {
        match expr {
            ExprKind::Literal(lit) => fmt_literal(lit),
            ExprKind::Ident(name) => name.clone(),
            ExprKind::This => "this".to_string(),
            ExprKind::Channel => "channel".to_string(),
            ExprKind::Break => "break".to_string(),
            ExprKind::Continue => "continue".to_string(),
            ExprKind::ArrayLiteral(elems) => {
                let items: Vec<String> = elems.iter().map(|e| self.fmt_expr(&e.node)).collect();
                format!("[{}]", items.join(", "))
            }
            ExprKind::MapLiteral(pairs) => {
                let items: Vec<String> = pairs.iter()
                    .map(|(k, v)| format!("{} => {}", self.fmt_expr(&k.node), self.fmt_expr(&v.node)))
                    .collect();
                format!("@{{{}}}", items.join(", "))
            }
            ExprKind::Tuple(elems) => {
                let items: Vec<String> = elems.iter().map(|e| self.fmt_expr(&e.node)).collect();
                format!("({})", items.join(", "))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                format!("{} {} {}", self.fmt_expr(&lhs.node), fmt_binop(op), self.fmt_expr(&rhs.node))
            }
            ExprKind::Unary { op, operand } => {
                format!("{}{}", fmt_unop(op), self.fmt_expr(&operand.node))
            }
            ExprKind::CompoundAssign { op, target, value } => {
                format!("{} {}= {}", self.fmt_expr(&target.node), fmt_compound_op(op), self.fmt_expr(&value.node))
            }
            ExprKind::Call { func, args } => {
                let args_str: Vec<String> = args.iter().map(|a| self.fmt_expr(&a.node)).collect();
                format!("{}({})", self.fmt_expr(&func.node), args_str.join(", "))
            }
            ExprKind::MethodCall { obj, method, args } => {
                let args_str: Vec<String> = args.iter().map(|a| self.fmt_expr(&a.node)).collect();
                format!("{}.{}({})", self.fmt_expr(&obj.node), method, args_str.join(", "))
            }
            ExprKind::Index { obj, index } => {
                format!("{}[{}]", self.fmt_expr(&obj.node), self.fmt_expr(&index.node))
            }
            ExprKind::FieldAccess { obj, field } => {
                format!("{}.{}", self.fmt_expr(&obj.node), field)
            }
            ExprKind::TupleIndex { tuple, index } => {
                format!("{}.{}", self.fmt_expr(&tuple.node), index)
            }
            ExprKind::New { class, type_args, args } => {
                let ta = if type_args.is_empty() {
                    String::new()
                } else {
                    let ts: Vec<String> = type_args.iter().map(|t| self.fmt_type(t)).collect();
                    format!("<{}>", ts.join(", "))
                };
                let args_str: Vec<String> = args.iter().map(|a| self.fmt_expr(&a.node)).collect();
                format!("new {}{}({})", class, ta, args_str.join(", "))
            }
            ExprKind::StructLiteral { name, fields } => {
                let fs: Vec<String> = fields.iter()
                    .map(|(k, v)| format!("{}: {}", k, self.fmt_expr(&v.node)))
                    .collect();
                format!("{} {{ {} }}", name, fs.join(", "))
            }
            ExprKind::EnumValue { enum_name, variant, args, .. } => {
                if args.is_empty() {
                    format!("{}::{}", enum_name, variant)
                } else {
                    let as_: Vec<String> = args.iter().map(|a| self.fmt_expr(&a.node)).collect();
                    format!("{}::{}({})", enum_name, variant, as_.join(", "))
                }
            }
            ExprKind::SuperCall { method, args } => {
                let args_str: Vec<String> = args.iter().map(|a| self.fmt_expr(&a.node)).collect();
                format!("super.{}({})", method, args_str.join(", "))
            }
            ExprKind::If { cond, then_branch, else_branch } => {
                let c = self.fmt_expr(&cond.node);
                let t = self.fmt_expr(&then_branch.node);
                match else_branch {
                    Some(e) => format!("if ({}) {{ {} }} else {{ {} }}", c, t, self.fmt_expr(&e.node)),
                    None => format!("if ({}) {{ {} }}", c, t),
                }
            }
            ExprKind::While { cond, body } => {
                format!("while {} {{ {} }}", self.fmt_expr(&cond.node), self.fmt_expr(&body.node))
            }
            ExprKind::For { var, iter, body } => {
                format!("for {} in {} {{ {} }}", var, self.fmt_expr(&iter.node), self.fmt_expr(&body.node))
            }
            ExprKind::Loop { body } => {
                format!("loop {{ {} }}", self.fmt_expr(&body.node))
            }
            ExprKind::Block(stmts) => {
                let parts: Vec<String> = stmts.iter()
                    .map(|s| self.fmt_stmt_kind(&s.node))
                    .filter(|s| !s.is_empty())
                    .collect();
                format!("{{ {} }}", parts.join(" "))
            }
            ExprKind::Match { expr, cases } => {
                let e = self.fmt_expr(&expr.node);
                let mut body = String::new();
                self.indent += 1;
                for case in cases {
                    let pat = fmt_pattern(&case.pattern);
                    let guard = case.guard.as_ref()
                        .map(|g| format!(" if {}", self.fmt_expr(&g.node)))
                        .unwrap_or_default();
                    let case_body = self.fmt_expr(&case.body.node);
                    body.push_str(&format!("{}{}{} => {};\n", self.ind(), pat, guard, case_body));
                }
                self.indent -= 1;
                format!("match {}\n{}{{\n{}{}}}", e, self.ind(), body, self.ind())
            }
            ExprKind::Lambda { params, ret_type, body } => {
                let ps = self.fmt_params(params);
                let ret = ret_type.as_ref().map(|t| format!(" -> {}", self.fmt_type(t))).unwrap_or_default();
                format!("|{}|{} => {}", ps, ret, self.fmt_expr(&body.node))
            }
            ExprKind::Spawn(e) => format!("spawn {}", self.fmt_expr(&e.node)),
            ExprKind::Await(e) => format!("await {}", self.fmt_expr(&e.node)),
            ExprKind::Send { channel, value } => {
                format!("send {} -> {}", self.fmt_expr(&channel.node), self.fmt_expr(&value.node))
            }
            ExprKind::Recv(e) => format!("recv {}", self.fmt_expr(&e.node)),
            ExprKind::Cast { expr, ty } => format!("{} as {}", self.fmt_expr(&expr.node), self.fmt_type(ty)),
            ExprKind::Is { expr, ty } => format!("{} is {}", self.fmt_expr(&expr.node), self.fmt_type(ty)),
            ExprKind::Range { start, end, inclusive } => {
                let op = if *inclusive { "..." } else { ".." };
                format!("{}{}{}", self.fmt_expr(&start.node), op, self.fmt_expr(&end.node))
            }
            ExprKind::Return(Some(e)) => format!("return {}", self.fmt_expr(&e.node)),
            ExprKind::Return(None) => "return".to_string(),
            ExprKind::Throw(e) => format!("throw {}", self.fmt_expr(&e.node)),
            ExprKind::Assign { target, value } => {
                format!("{} = {}", self.fmt_expr(&target.node), self.fmt_expr(&value.node))
            }
            ExprKind::Try { body, catches, finally } => {
                let b = self.fmt_expr(&body.node);
                let mut s = format!("try {{ {} }}", b);
                for catch in catches {
                    let ty = self.fmt_type(&catch.ty);
                    let cb = self.fmt_stmt_kind(&catch.body.node);
                    s.push_str(&format!(" catch {}: {} {{ {} }}", catch.param, ty, cb));
                }
                if let Some(fin) = finally {
                    s.push_str(&format!(" finally {{ {} }}", self.fmt_expr(&fin.node)));
                }
                s
            }
        }
    }

    fn fmt_params(&mut self, params: &[Param]) -> String {
        params.iter()
            .map(|p| format!("{}: {}", p.name, self.fmt_type(&p.param_type)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn fmt_type(&self, ty: &Type) -> String {
        match ty {
            Type::Int8 => "Int8".to_string(),
            Type::Int16 => "Int16".to_string(),
            Type::Int32 => "Int32".to_string(),
            Type::Int64 => "Int64".to_string(),
            Type::UInt8 => "UInt8".to_string(),
            Type::UInt16 => "UInt16".to_string(),
            Type::UInt32 => "UInt32".to_string(),
            Type::UInt64 => "UInt64".to_string(),
            Type::Float32 => "Float32".to_string(),
            Type::Float64 => "Float64".to_string(),
            Type::Bool => "Bool".to_string(),
            Type::Char => "Char".to_string(),
            Type::String => "String".to_string(),
            Type::Nothing => "Nothing".to_string(),
            Type::Never => "Never".to_string(),
            Type::Any => "Any".to_string(),
            Type::Infer => "_".to_string(),
            Type::Named(n) => n.clone(),
            Type::Generic { name, args } => {
                let as_: Vec<String> = args.iter().map(|t| self.fmt_type(t)).collect();
                format!("{}<{}>", name, as_.join(", "))
            }
            Type::Array(inner) => format!("Array<{}>", self.fmt_type(inner)),
            Type::Map(k, v) => format!("Map<{}, {}>", self.fmt_type(k), self.fmt_type(v)),
            Type::Mutable(inner) => format!("mut {}", self.fmt_type(inner)),
            Type::Ref(inner) => format!("&{}", self.fmt_type(inner)),
            Type::Fn { params, ret } => {
                let ps: Vec<String> = params.iter().map(|t| self.fmt_type(t)).collect();
                format!("fn({}) -> {}", ps.join(", "), self.fmt_type(ret))
            }
            Type::Tuple(types) => {
                let ts: Vec<String> = types.iter().map(|t| self.fmt_type(t)).collect();
                format!("({})", ts.join(", "))
            }
            Type::Nullable(inner) => format!("{}?", self.fmt_type(inner)),
        }
    }
}

fn fmt_type_params(params: &[String]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!("<{}>", params.join(", "))
    }
}

fn fmt_visibility(vis: &Visibility) -> &'static str {
    match vis {
        Visibility::Public => "",
        Visibility::Private => "private ",
        Visibility::Protected => "protected ",
        Visibility::Package => "",
    }
}

fn fmt_annotation_arg(arg: &AnnotationArg) -> String {
    match arg {
        AnnotationArg::Literal(lit) => fmt_literal(lit),
        AnnotationArg::EnumValue(type_name, variant) => format!("{}.{}", type_name, variant),
        AnnotationArg::Array(items) => format!(
            "[{}]",
            items.iter().map(fmt_annotation_arg).collect::<Vec<_>>().join(", ")
        ),
    }
}

// Issue #246: `Literal::String`/`Literal::Char` already hold the DECODED
// value (the lexer's `read_escape`, tinox-lexer/src/lib.rs, resolves
// `\"`/`\\`/`\n`/etc. before the literal ever reaches the parser/AST) --
// printing that decoded value back between quotes with no re-escaping at
// all corrupts the literal the instant it contains a character that's
// only legal inside a string/char literal when escaped. A raw `"` inside
// a re-printed string literal ends it early (`\"ERROR\"` became `"ERROR"`,
// splitting one string into three tokens and breaking the surrounding
// expression); a raw newline/tab would do the same for `\n`/`\t`. This is
// the exact inverse of `read_escape`'s decode table.
fn escape_tinox_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_tinox_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\'' => "\\'".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        '\r' => "\\r".to_string(),
        '\0' => "\\0".to_string(),
        _ => c.to_string(),
    }
}

fn fmt_literal(lit: &Literal) -> String {
    match lit {
        Literal::Integer(n) => n.to_string(),
        Literal::Float(f) => {
            let s = format!("{}", f);
            if s.contains('.') { s } else { format!("{}.0", s) }
        }
        Literal::String(s) => format!("\"{}\"", escape_tinox_string(s)),
        Literal::Char(c) => format!("'{}'", escape_tinox_char(*c)),
        Literal::Byte(b) => format!("{}b", b),
        Literal::Bool(b) => b.to_string(),
        Literal::Null => "null".to_string(),
    }
}

fn fmt_binop(op: &BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+", BinaryOp::Sub => "-", BinaryOp::Mul => "*",
        BinaryOp::Div => "/", BinaryOp::Mod => "%", BinaryOp::And => "&&",
        BinaryOp::Or => "||", BinaryOp::BitAnd => "&", BinaryOp::BitOr => "|", BinaryOp::Xor => "^",
        BinaryOp::Shl => "<<", BinaryOp::Shr => ">>", BinaryOp::ShrArith => ">>>",
        BinaryOp::Eq => "==", BinaryOp::Ne => "!=", BinaryOp::Lt => "<",
        BinaryOp::Le => "<=", BinaryOp::Gt => ">", BinaryOp::Ge => ">=",
    }
}

fn fmt_unop(op: &UnaryOp) -> &'static str {
    match op { UnaryOp::Neg => "-", UnaryOp::Not => "!", UnaryOp::BitNot => "~" }
}

fn fmt_compound_op(op: &CompoundOp) -> &'static str {
    match op {
        CompoundOp::Add => "+", CompoundOp::Sub => "-", CompoundOp::Mul => "*",
        CompoundOp::Div => "/", CompoundOp::Mod => "%", CompoundOp::BitAnd => "&",
        CompoundOp::BitOr => "|", CompoundOp::BitXor => "^", CompoundOp::Shl => "<<",
        CompoundOp::Shr => ">>", CompoundOp::ShrArith => ">>>",
    }
}

fn fmt_pattern(pat: &Pattern) -> String {
    match pat {
        Pattern::Wildcard(_) => "_".to_string(),
        Pattern::Ident(name, sub, _) => match sub {
            Some(p) => format!("{} @ {}", name, fmt_pattern(p)),
            None => name.clone(),
        },
        Pattern::Literal(lit, _) => fmt_literal(lit),
        Pattern::Tuple(pats, _) => {
            let ps: Vec<String> = pats.iter().map(fmt_pattern).collect();
            format!("({})", ps.join(", "))
        }
        Pattern::EnumVariant { enum_name, variant, args, .. } => {
            if args.is_empty() {
                format!("{}::{}", enum_name, variant)
            } else {
                let as_: Vec<String> = args.iter().map(fmt_pattern).collect();
                format!("{}::{}({})", enum_name, variant, as_.join(", "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use crate::Parser;

    fn fmt(src: &str) -> String {
        let tokens = Lexer::new(src).tokenize().unwrap();
        let ast = Parser::new(tokens).parse().unwrap();
        Formatter::new().format(&ast)
    }

    // --- functions ---

    #[test]
    fn test_fmt_simple_function() {
        let out = fmt("fn add(a: Int32, b: Int32) -> Int32 { return a + b; }");
        assert!(out.contains("fn add(a: Int32, b: Int32) -> Int32"));
        assert!(out.contains("return a + b;"));
    }

    #[test]
    fn test_fmt_function_no_params() {
        let out = fmt("fn greet() -> Nothing {}");
        assert!(out.contains("fn greet() -> Nothing"));
    }

    #[test]
    fn test_fmt_async_function() {
        let out = fmt("async fn fetch() -> Nothing {}");
        assert!(out.contains("async fn fetch()"));
    }

    #[test]
    fn test_fmt_function_with_annotation() {
        let out = fmt("@inline\nfn fast() -> Nothing {}");
        assert!(out.contains("@inline"));
        assert!(out.contains("fn fast()"));
    }

    #[test]
    fn test_fmt_function_with_annotation_args() {
        let out = fmt("@deprecated(\"use newFunc\")\nfn old() -> Nothing {}");
        assert!(out.contains("@deprecated(\"use newFunc\")"));
    }

    #[test]
    fn test_fmt_function_with_doc_comment() {
        let out = fmt("/** Returns x doubled */\nfn double(x: Int32) -> Int32 { return x * 2; }");
        assert!(out.contains("Returns x doubled"));
        assert!(out.contains("fn double(x: Int32)"));
    }

    #[test]
    fn test_fmt_generic_function() {
        let out = fmt("fn identity<T>(x: T) -> T { return x; }");
        assert!(out.contains("fn identity<T>(x: T) -> T"));
    }

    // --- statements ---

    #[test]
    fn test_fmt_let_with_type() {
        let out = fmt("fn f() { let x: Int32 = 5; }");
        assert!(out.contains("let x: Int32 = 5;"));
    }

    #[test]
    fn test_fmt_let_inferred() {
        let out = fmt("fn f() { let x = 42; }");
        assert!(out.contains("let x = 42;"));
    }

    #[test]
    fn test_fmt_var() {
        let out = fmt("fn f() { var count: Int32 = 0; }");
        assert!(out.contains("var count: Int32 = 0;"));
    }

    #[test]
    fn test_fmt_return() {
        let out = fmt("fn f() -> Int32 { return 1; }");
        assert!(out.contains("return 1;"));
    }

    #[test]
    fn test_fmt_return_void() {
        let out = fmt("fn f() { return; }");
        assert!(out.contains("return;"));
    }

    #[test]
    fn test_fmt_if_else() {
        let out = fmt("fn f(x: Int32) -> Nothing { if (x > 0) { return; } else { return; } }");
        assert!(out.contains("if (x > 0)"));
        assert!(out.contains("else"));
    }

    #[test]
    fn test_fmt_while() {
        let out = fmt("fn f() { while true { break; } }");
        assert!(out.contains("while true"));
        assert!(out.contains("break;"));
    }

    #[test]
    fn test_fmt_for_in() {
        let out = fmt("fn f() { for i in 0..10 { continue; } }");
        assert!(out.contains("for i in"));
        assert!(out.contains("0..10"));
    }

    #[test]
    fn test_fmt_loop() {
        let out = fmt("fn f() { loop { break; } }");
        assert!(out.contains("loop"));
        assert!(out.contains("break;"));
    }

    #[test]
    fn test_fmt_assignment() {
        let out = fmt("fn f() { var x = 1; x = 2; }");
        assert!(out.contains("x = 2;"));
    }

    #[test]
    fn test_fmt_throw() {
        let out = fmt("fn f() { throw \"error\"; }");
        assert!(out.contains("throw"));
        assert!(out.contains("\"error\""));
    }

    #[test]
    fn test_fmt_try_catch() {
        let out = fmt("fn f() { try { throw \"x\"; } catch e: String { return; } }");
        assert!(out.contains("try"));
        assert!(out.contains("catch e: String"));
    }

    #[test]
    fn test_fmt_break_continue() {
        let out = fmt("fn f() { while true { break; continue; } }");
        assert!(out.contains("break;"));
        assert!(out.contains("continue;"));
    }

    // --- expressions ---

    #[test]
    fn test_fmt_binary_ops() {
        let out = fmt("fn f() -> Int32 { return 1 + 2 * 3; }");
        assert!(out.contains("1 + 2 * 3"));
    }

    #[test]
    fn test_fmt_unary_neg() {
        let out = fmt("fn f(x: Int32) -> Int32 { return -x; }");
        assert!(out.contains("-x"));
    }

    #[test]
    fn test_fmt_call() {
        let out = fmt("fn f() { println(\"hi\"); }");
        assert!(out.contains("println(\"hi\")"));
    }

    #[test]
    fn test_fmt_method_call() {
        let out = fmt("fn f(s: String) { s.length(); }");
        assert!(out.contains("s.length()"));
    }

    #[test]
    fn test_fmt_field_access() {
        let out = fmt("fn f(p: Point) -> Int32 { return p.x; }");
        assert!(out.contains("p.x"));
    }

    #[test]
    fn test_fmt_index() {
        let out = fmt("fn f(a: Array<Int32>) -> Int32 { return a[0]; }");
        assert!(out.contains("a[0]"));
    }

    #[test]
    fn test_fmt_new() {
        let out = fmt("fn f() -> Foo { return new Foo(); }");
        assert!(out.contains("new Foo()"));
    }

    #[test]
    fn test_fmt_array_literal() {
        let out = fmt("fn f() { let a = [1, 2, 3]; }");
        assert!(out.contains("[1, 2, 3]"));
    }

    #[test]
    fn test_fmt_map_literal() {
        let out = fmt("fn f() { let m = @{\"a\" => 1}; }");
        assert!(out.contains("@{"));
        assert!(out.contains("=>"));
    }

    #[test]
    fn test_fmt_cast() {
        let out = fmt("fn f(x: Int32) -> Int64 { return x as Int64; }");
        assert!(out.contains("x as Int64"));
    }

    #[test]
    fn test_fmt_range_exclusive() {
        let out = fmt("fn f() { for i in 0..5 {} }");
        assert!(out.contains("0..5"));
    }

    #[test]
    fn test_fmt_range_inclusive() {
        let out = fmt("fn f() { for i in 0...5 {} }");
        assert!(out.contains("0...5"));
    }

    #[test]
    fn test_fmt_compound_assign() {
        let out = fmt("fn f() { var x = 1; x += 2; }");
        assert!(out.contains("+="));
    }

    #[test]
    fn test_fmt_match() {
        let out = fmt("fn f(x: Int32) -> Nothing { match x { 1 => return; _ => return; } }");
        assert!(out.contains("match x"));
        assert!(out.contains("=>"));
        assert!(out.contains("_"));
    }

    #[test]
    fn test_fmt_enum_value() {
        let out = fmt("fn f() -> Color { return Color::Red; }");
        assert!(out.contains("Color::Red"));
    }

    // --- types ---

    #[test]
    fn test_fmt_type_array() {
        let out = fmt("fn f(a: Array<Int32>) -> Nothing {}");
        assert!(out.contains("Array<Int32>"));
    }

    #[test]
    fn test_fmt_type_map() {
        let out = fmt("fn f(m: Map<String, Int32>) -> Nothing {}");
        assert!(out.contains("Map<String, Int32>"));
    }

    #[test]
    fn test_fmt_type_fn() {
        let out = fmt("fn f(cb: fnc(Int32) -> Bool) -> Nothing {}");
        assert!(out.contains("-> Bool"));
    }

    #[test]
    fn test_fmt_type_tuple() {
        let out = fmt("fn f() -> (Int32, String) { return (1, \"x\"); }");
        assert!(out.contains("(Int32, String)"));
    }

    // --- class ---

    #[test]
    fn test_fmt_class_basic() {
        let out = fmt("class Dog { var name: String; fn bark() -> Nothing {} }");
        assert!(out.contains("class Dog"));
        assert!(out.contains("var name: String;"));
        assert!(out.contains("fn bark() -> Nothing"));
    }

    #[test]
    fn test_fmt_class_extends() {
        let out = fmt("class Cat extends Animal { fn meow() -> Nothing {} }");
        assert!(out.contains("extends Animal"));
    }

    #[test]
    fn test_fmt_class_implements() {
        let out = fmt("class Dog implements Runnable { fn run() -> Nothing {} }");
        assert!(out.contains("implements Runnable"));
    }

    #[test]
    fn test_fmt_class_static_method() {
        let out = fmt("class Util { fnc create() -> Util {} }");
        assert!(out.contains("fnc create()"));
    }

    #[test]
    fn test_fmt_class_with_annotation() {
        let out = fmt("@ApplicationComponent\nclass Repo { fn find() -> Nothing {} }");
        assert!(out.contains("@ApplicationComponent"));
        assert!(out.contains("class Repo"));
    }

    #[test]
    fn test_fmt_class_generic() {
        let out = fmt("class Box<T> { var value: T; }");
        assert!(out.contains("class Box<T>"));
        assert!(out.contains("value: T"));
    }

    // --- interface / enum / trait ---

    #[test]
    fn test_fmt_interface() {
        let out = fmt("interface Printable { fn print() -> Nothing; }");
        assert!(out.contains("interface Printable"));
        assert!(out.contains("fn print() -> Nothing;"));
    }

    #[test]
    fn test_fmt_interface_extends() {
        let out = fmt("interface ReadWrite extends Read, Write { fn rw() -> Nothing; }");
        assert!(out.contains("extends Read, Write"));
    }

    #[test]
    fn test_fmt_enum_simple() {
        let out = fmt("enum Color { Red; Green; Blue; }");
        assert!(out.contains("enum Color"));
        assert!(out.contains("Red;"));
        assert!(out.contains("Green;"));
    }

    #[test]
    fn test_fmt_enum_with_args() {
        let out = fmt("enum Shape { Circle(Float64); Rect(Float64, Float64); }");
        assert!(out.contains("Circle(Float64)"));
        assert!(out.contains("Rect(Float64, Float64)"));
    }

    #[test]
    fn test_fmt_trait() {
        let out = fmt("trait Serialize { fn serialize() -> String; }");
        assert!(out.contains("trait Serialize"));
        assert!(out.contains("fn serialize() -> String;"));
    }

    // --- import / module / namespace ---

    #[test]
    fn test_fmt_import() {
        let out = fmt("import std.io;");
        assert!(out.contains("import std::io;"));
    }

    #[test]
    fn test_fmt_import_alias() {
        let out = fmt("import std.io as io;");
        assert!(out.contains("as io;"));
    }

    #[test]
    fn test_fmt_module() {
        let out = fmt("module myapp;");
        assert!(out.contains("module myapp;"));
    }

    #[test]
    fn test_fmt_namespace() {
        let out = fmt("namespace net { fn connect() -> Nothing {} }");
        assert!(out.contains("namespace net"));
        assert!(out.contains("fn connect()"));
    }

    // --- immutable ---

    #[test]
    fn test_fmt_immutable() {
        let out = fmt("immutable Point(x: Int32, y: Int32)");
        assert!(out.contains("immutable Point(x: Int32, y: Int32)"));
    }

    // --- multiple declarations ---

    #[test]
    fn test_fmt_multiple_decls_separated_by_blank_line() {
        let out = fmt("fn a() -> Nothing {}\nfn b() -> Nothing {}");
        // Two functions should have a blank line between them
        assert!(out.contains("fn a()"));
        assert!(out.contains("fn b()"));
        // There should be a newline separating them
        let a_pos = out.find("fn a()").unwrap();
        let b_pos = out.find("fn b()").unwrap();
        assert!(b_pos > a_pos);
    }

    // --- idempotence: format(format(src)) == format(src) ---

    #[test]
    fn test_fmt_idempotent_function() {
        let src = "fn add(a: Int32, b: Int32) -> Int32 { return a + b; }";
        let first = fmt(src);
        // Parse the formatted output and format again
        let tokens = tinox_lexer::Lexer::new(&first).tokenize().unwrap();
        let ast2 = Parser::new(tokens).parse().unwrap();
        let second = Formatter::new().format(&ast2);
        assert_eq!(first, second, "formatter should be idempotent");
    }

    #[test]
    fn test_fmt_idempotent_class() {
        let src = "class Foo { var x: Int32; fn bar() -> Nothing {} }";
        let first = fmt(src);
        let tokens = tinox_lexer::Lexer::new(&first).tokenize().unwrap();
        let ast2 = Parser::new(tokens).parse().unwrap();
        let second = Formatter::new().format(&ast2);
        assert_eq!(first, second);
    }

    // --- issue #246: re-escaping string/char literals ---
    //
    // `Literal::String`/`Literal::Char` hold the DECODED value (escapes
    // already resolved by the lexer) -- printing that value back between
    // quotes with no re-escaping corrupts the literal the moment it
    // contains a character that's only legal there when escaped (a raw
    // `"` inside a re-printed string ends it early). These tests re-lex
    // and re-parse the FORMATTED output and check the literal's actual
    // decoded VALUE survives unchanged -- not just that formatting
    // "succeeds", which the bug's own corrupted output also did.

    fn parsed_string_literal_value(src: &str) -> String {
        let tokens = tinox_lexer::Lexer::new(src).tokenize().unwrap();
        let ast = Parser::new(tokens).parse().unwrap();
        match &ast.decls[0].node {
            DeclKind::Function(f) => match &f.body.node {
                StmtKind::Block(stmts) => match &stmts[0].node {
                    StmtKind::Expr(e) => match &e.node {
                        ExprKind::Literal(Literal::String(s)) => s.clone(),
                        other => panic!("expected a string literal statement, got {other:?}"),
                    },
                    other => panic!("expected an expr statement, got {other:?}"),
                },
                other => panic!("expected a block body, got {other:?}"),
            },
            other => panic!("expected a function decl, got {other:?}"),
        }
    }

    #[test]
    fn test_fmt_string_literal_with_escaped_quote_round_trips() {
        let src = r#"fn main() -> Nothing { "select == \"ERROR\""; }"#;
        let original_value = parsed_string_literal_value(src);
        let formatted = fmt(src);
        let reparsed_value = parsed_string_literal_value(&formatted);
        assert_eq!(
            original_value, reparsed_value,
            "formatting must not change a string literal's decoded value -- formatted output was: {formatted}"
        );
        // Also confirm this isn't vacuously true because the value happens
        // to contain no `"` at all.
        assert!(original_value.contains('"'));
    }

    #[test]
    fn test_fmt_string_literal_with_backslash_and_newline_round_trips() {
        let src = r#"fn main() -> Nothing { "a\\b\nc\td"; }"#;
        let original_value = parsed_string_literal_value(src);
        let formatted = fmt(src);
        let reparsed_value = parsed_string_literal_value(&formatted);
        assert_eq!(original_value, reparsed_value);
    }

    #[test]
    fn test_fmt_char_literal_with_quote_and_backslash() {
        let out = fmt(r"fn main() -> Nothing { '\''; '\\'; }");
        // A literal, un-escaped `'`/`\` here would either end the char
        // literal early or otherwise fail to re-parse -- re-tokenizing
        // the formatted output is itself the assertion.
        tinox_lexer::Lexer::new(&out).tokenize().unwrap();
        assert!(out.contains(r"'\''"));
        assert!(out.contains(r"'\\'"));
    }
}

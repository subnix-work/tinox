use std::collections::HashMap;

use tinox_common::{Pos, Span};
use tinox_parser::ast::*;
use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, Documentation, Location, MarkupContent, MarkupKind, Position, Range, Url};

// --- Type Registry ---

#[derive(Clone)]
pub struct FieldInfo {
    pub name: String,
    pub type_name: String,
    pub doc: Option<String>,
}

#[derive(Clone)]
pub struct MethodInfo {
    pub name: String,
    pub signature: String,
    pub doc: Option<String>,
}

#[derive(Clone)]
pub struct ClassInfo {
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
}

pub type TypeRegistry = HashMap<String, ClassInfo>;

pub fn build_registry_from_source(source: &SourceFile) -> TypeRegistry {
    let mut registry = HashMap::new();
    for decl in &source.decls {
        collect_classes_from_decl(decl, &mut registry);
    }
    registry
}

fn collect_classes_from_decl(decl: &Decl, registry: &mut TypeRegistry) {
    match &decl.node {
        DeclKind::Class(c) => {
            registry.insert(c.name.clone(), class_to_info(c));
        }
        DeclKind::Namespace(ns) => {
            for inner in &ns.decls {
                collect_classes_from_decl(inner, registry);
            }
        }
        _ => {}
    }
}

fn class_to_info(c: &Class) -> ClassInfo {
    ClassInfo {
        fields: c.fields.iter().map(|f| FieldInfo {
            name: f.name.clone(),
            type_name: type_str(&f.field_type),
            doc: f.doc.clone(),
        }).collect(),
        methods: c.methods.iter().map(|m| {
            let params: Vec<String> = m.params.iter()
                .map(|p| format!("{}: {}", p.name, type_str(&p.param_type)))
                .collect();
            let kw = if m.static_ { "fnc" } else { "fn" };
            MethodInfo {
                name: m.name.clone(),
                signature: format!("{} {}({}) -> {}", kw, m.name, params.join(", "), type_str(&m.ret_type)),
                doc: m.doc.clone(),
            }
        }).collect(),
    }
}

pub fn lsp_pos_to_offset(src: &str, pos: Position) -> u32 {
    let mut line = 0u32;
    let mut col = 0u32;
    let mut offset = 0u32;
    for ch in src.chars() {
        if line == pos.line && col == pos.character {
            return offset;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
        offset += ch.len_utf8() as u32;
    }
    offset
}

fn span_contains(span: Span, offset: u32) -> bool {
    span.start.offset <= offset && offset <= span.end.offset
}

pub fn pos_to_lsp(p: Pos) -> Position {
    Position {
        line: p.line.saturating_sub(1),
        character: p.column.saturating_sub(1),
    }
}

pub fn span_to_range(span: Span) -> Range {
    Range {
        start: pos_to_lsp(span.start),
        end: pos_to_lsp(span.end),
    }
}

// --- Hover ---

pub fn hover_at(source: &SourceFile, offset: u32) -> Option<String> {
    // Check if cursor is on an annotation — scan all annotations in the AST
    if let Some(doc) = hover_annotation(source, offset) {
        return Some(doc);
    }
    for decl in &source.decls {
        if let Some(s) = hover_decl(decl, offset) {
            return Some(s);
        }
    }
    None
}

fn hover_annotation(source: &SourceFile, offset: u32) -> Option<String> {
    for decl in &source.decls {
        if let Some(doc) = annotation_in_decl(&decl.node, offset) {
            return Some(doc);
        }
    }
    None
}

fn annotation_in_decl(decl: &DeclKind, offset: u32) -> Option<String> {
    let annotations: &[Annotation] = match decl {
        DeclKind::Class(c) => {
            for ann in &c.annotations {
                if let Some(d) = check_annotation(ann, offset) { return Some(d); }
            }
            for m in &c.methods {
                for ann in &m.annotations { if let Some(d) = check_annotation(ann, offset) { return Some(d); } }
            }
            for f in &c.fields {
                for ann in &f.annotations { if let Some(d) = check_annotation(ann, offset) { return Some(d); } }
            }
            return None;
        }
        DeclKind::Function(f) => &f.annotations,
        DeclKind::Namespace(ns) => {
            for inner in &ns.decls { if let Some(d) = annotation_in_decl(&inner.node, offset) { return Some(d); } }
            return None;
        }
        _ => return None,
    };
    for ann in annotations {
        if let Some(d) = check_annotation(ann, offset) { return Some(d); }
    }
    None
}

fn check_annotation(ann: &Annotation, offset: u32) -> Option<String> {
    if span_contains(ann.span, offset) {
        annotation_doc(&ann.name)
    } else {
        None
    }
}

fn hover_decl(decl: &Decl, offset: u32) -> Option<String> {
    // Note: decl.span is a point-span (mk_span limitation), so we skip the outer guard
    // and rely on expression-level spans (from tokens) which are accurate.
    match &decl.node {
        DeclKind::Function(f) => {
            // f.span is a point at the start of `fn` keyword
            // Show full signature when hovering the fn keyword or name (before the body starts)
            if offset < f.body.span.start.offset {
                let mut content = fn_signature(f);
                if let Some(doc) = &f.doc {
                    content.push_str("\n\n---\n\n");
                    content.push_str(&format_doc(doc));
                }
                return Some(content);
            }
            hover_stmt(&f.body, offset)
        }
        DeclKind::Class(c) => {
            // Check if hovering on class declaration itself
            if offset < c.span.start.offset + c.name.len() as u32 + 10 {
                let mut content = format!("class {}", c.name);
                if let Some(doc) = &c.doc {
                    content.push_str("\n\n---\n\n");
                    content.push_str(&format_doc(doc));
                }
                return Some(content);
            }
            for m in &c.methods {
                if let Some(s) = hover_stmt(&m.body, offset) {
                    return Some(s);
                }
                if span_contains(m.span, offset) {
                    let mut content = method_signature(&c.name, m);
                    if let Some(doc) = &m.doc {
                        content.push_str("\n\n---\n\n");
                        content.push_str(&format_doc(doc));
                    }
                    return Some(content);
                }
            }
            // Check fields
            for f in &c.fields {
                if span_contains(f.span, offset) {
                    let mut content = format!("{}: {}", f.name, type_str(&f.field_type));
                    if let Some(doc) = &f.doc {
                        content.push_str("\n\n---\n\n");
                        content.push_str(&format_doc(doc));
                    }
                    return Some(content);
                }
            }
            None
        }
        DeclKind::Enum(e) => {
            if offset < e.span.start.offset + e.name.len() as u32 + 10 {
                let mut content = format!("enum {}", e.name);
                if let Some(doc) = &e.doc {
                    content.push_str("\n\n---\n\n");
                    content.push_str(&format_doc(doc));
                }
                return Some(content);
            }
            for v in &e.variants {
                if span_contains(v.span, offset) {
                    let mut content = format!("{}.{}", e.name, v.name);
                    if let Some(doc) = &v.doc {
                        content.push_str("\n\n---\n\n");
                        content.push_str(&format_doc(doc));
                    }
                    return Some(content);
                }
            }
            None
        }
        DeclKind::Interface(i) => {
            if offset < i.span.start.offset + i.name.len() as u32 + 10 {
                let mut content = format!("interface {}", i.name);
                if let Some(doc) = &i.doc {
                    content.push_str("\n\n---\n\n");
                    content.push_str(&format_doc(doc));
                }
                return Some(content);
            }
            for m in &i.methods {
                if span_contains(m.span, offset) {
                    let mut content = fn_signature(m);
                    if let Some(doc) = &m.doc {
                        content.push_str("\n\n---\n\n");
                        content.push_str(&format_doc(doc));
                    }
                    return Some(content);
                }
            }
            None
        }
        DeclKind::Trait(t) => {
            if offset < t.span.start.offset + t.name.len() as u32 + 10 {
                let mut content = format!("trait {}", t.name);
                if let Some(doc) = &t.doc {
                    content.push_str("\n\n---\n\n");
                    content.push_str(&format_doc(doc));
                }
                return Some(content);
            }
            for m in &t.methods {
                if span_contains(m.span, offset) {
                    let mut content = fn_signature(m);
                    if let Some(doc) = &m.doc {
                        content.push_str("\n\n---\n\n");
                        content.push_str(&format_doc(doc));
                    }
                    return Some(content);
                }
            }
            None
        }
        DeclKind::Immutable(u) => {
            if offset < u.span.start.offset + u.name.len() as u32 + 20 {
                let fields: Vec<String> = u.fields.iter()
                    .map(|f| format!("{}: {}", f.name, type_str(&f.param_type)))
                    .collect();
                return Some(format!("immutable {}({})", u.name, fields.join(", ")));
            }
            for f in &u.fields {
                if span_contains(f.span, offset) {
                    return Some(format!("{}: {}", f.name, type_str(&f.param_type)));
                }
            }
            None
        }
        _ => None,
    }
}

fn format_doc(doc: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    if lines.len() <= 1 {
        doc.trim().to_string()
    } else {
        let mut out = String::new();
        for line in &lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                out.push('\n');
                continue;
            }
            let cleaned = trimmed.trim_start_matches('*').trim_end_matches("*/").trim();
            if !cleaned.is_empty() {
                out.push_str(cleaned);
                out.push('\n');
            }
        }
        out.trim_end().to_string()
    }
}

fn hover_stmt(stmt: &Stmt, offset: u32) -> Option<String> {
    match &stmt.node {
        StmtKind::Let { name, ty, value, .. } => {
            let ty_str = ty.as_ref().map(type_str).unwrap_or_else(|| "inferred".into());
            if let Some(v) = value {
                if let Some(s) = hover_expr(v, offset) {
                    return Some(s);
                }
            }
            Some(format!("let {}: {}", name, ty_str))
        }
        StmtKind::Var { name, ty, value, .. } => {
            let ty_str = ty.as_ref().map(type_str).unwrap_or_else(|| "inferred".into());
            if let Some(v) = value {
                if let Some(s) = hover_expr(v, offset) {
                    return Some(s);
                }
            }
            Some(format!("var {}: {}", name, ty_str))
        }
        StmtKind::Block(stmts) => {
            for s in stmts {
                if let Some(r) = hover_stmt(s, offset) {
                    return Some(r);
                }
            }
            None
        }
        StmtKind::Expr(e) => hover_expr(e, offset),
        StmtKind::Return(Some(e)) => hover_expr(e, offset),
        StmtKind::If { cond, then_branch, else_branch } => {
            hover_expr(cond, offset)
                .or_else(|| hover_stmt(then_branch, offset))
                .or_else(|| else_branch.as_ref().and_then(|e| hover_stmt(e, offset)))
        }
        StmtKind::While { cond, body } => {
            hover_expr(cond, offset).or_else(|| hover_stmt(body, offset))
        }
        StmtKind::For { body, .. } => hover_stmt(body, offset),
        StmtKind::Loop { body } => hover_stmt(body, offset),
        _ => None,
    }
}

fn hover_expr(expr: &Expr, offset: u32) -> Option<String> {
    if !span_contains(expr.span, offset) {
        return None;
    }
    match &expr.node {
        ExprKind::Call { func, args } => {
            if let ExprKind::Ident(name) = &func.node {
                if span_contains(func.span, offset) {
                    return Some(format!("fn {}(…)", name));
                }
            }
            for a in args {
                if let Some(s) = hover_expr(a, offset) {
                    return Some(s);
                }
            }
            None
        }
        ExprKind::Literal(lit) => Some(format!("{}: {}", literal_str(lit), type_str(&lit.ty()))),
        ExprKind::Binary { lhs, rhs, .. } => {
            hover_expr(lhs, offset).or_else(|| hover_expr(rhs, offset))
        }
        ExprKind::Block(stmts) => {
            for s in stmts {
                if let Some(r) = hover_stmt(s, offset) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn fn_signature(f: &Function) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_str(&p.param_type)))
        .collect();
    let async_ = if f.is_async { "async " } else { "" };
    format!("{}fn {}({}) -> {}", async_, f.name, params.join(", "), type_str(&f.ret_type))
}

pub fn method_signature(class: &str, m: &Method) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_str(&p.param_type)))
        .collect();
    format!(
        "{}.{}({}) -> {}",
        class,
        m.name,
        params.join(", "),
        type_str(&m.ret_type)
    )
}

pub fn type_str(ty: &Type) -> String {
    match ty {
        Type::Int8 => "Int8".into(),
        Type::Int16 => "Int16".into(),
        Type::Int32 => "Int32".into(),
        Type::Int64 => "Int64".into(),
        Type::UInt8 => "UInt8".into(),
        Type::UInt16 => "UInt16".into(),
        Type::UInt32 => "UInt32".into(),
        Type::UInt64 => "UInt64".into(),
        Type::Float32 => "Float32".into(),
        Type::Float64 => "Float64".into(),
        Type::Bool => "Bool".into(),
        Type::String => "String".into(),
        Type::Char => "Char".into(),
        Type::Nothing => "Nothing".into(),
        Type::Never => "Never".into(),
        Type::Any => "Any".into(),
        Type::Infer => "_".into(),
        Type::Named(n) => n.clone(),
        Type::Array(inner) => format!("Array<{}>", type_str(inner)),
        Type::Tuple(types) => {
            let inner: Vec<_> = types.iter().map(type_str).collect();
            format!("({})", inner.join(", "))
        }
        Type::Fn { params, ret } => {
            let ps: Vec<_> = params.iter().map(type_str).collect();
            format!("fn({}) -> {}", ps.join(", "), type_str(ret))
        }
        Type::Generic { name, args } => {
            let as_: Vec<_> = args.iter().map(type_str).collect();
            format!("{}<{}>", name, as_.join(", "))
        }
        Type::Map(k, v) => format!("Map<{}, {}>", type_str(k), type_str(v)),
        Type::Mutable(inner) => format!("mut {}", type_str(inner)),
        Type::Ref(inner) => format!("&{}", type_str(inner)),
        Type::Nullable(inner) => format!("{}?", type_str(inner)),
    }
}

fn literal_str(lit: &Literal) -> String {
    match lit {
        Literal::Integer(n) => n.to_string(),
        Literal::Float(f) => f.to_string(),
        Literal::String(s) => format!("\"{}\"", s),
        Literal::Bool(b) => b.to_string(),
        Literal::Char(c) => format!("'{}'", c),
        Literal::Byte(b) => format!("0x{:02x}", b),
        Literal::Null => "null".into(),
    }
}

// --- Completion ---

const KEYWORDS: &[(&str, CompletionItemKind)] = &[
    ("fn", CompletionItemKind::KEYWORD),
    ("let", CompletionItemKind::KEYWORD),
    ("var", CompletionItemKind::KEYWORD),
    ("return", CompletionItemKind::KEYWORD),
    ("if", CompletionItemKind::KEYWORD),
    ("else", CompletionItemKind::KEYWORD),
    ("while", CompletionItemKind::KEYWORD),
    ("for", CompletionItemKind::KEYWORD),
    ("in", CompletionItemKind::KEYWORD),
    ("loop", CompletionItemKind::KEYWORD),
    ("break", CompletionItemKind::KEYWORD),
    ("continue", CompletionItemKind::KEYWORD),
    ("class", CompletionItemKind::KEYWORD),
    ("interface", CompletionItemKind::KEYWORD),
    ("enum", CompletionItemKind::KEYWORD),
    ("extends", CompletionItemKind::KEYWORD),
    ("implements", CompletionItemKind::KEYWORD),
    ("new", CompletionItemKind::KEYWORD),
    ("this", CompletionItemKind::KEYWORD),
    ("match", CompletionItemKind::KEYWORD),
    ("try", CompletionItemKind::KEYWORD),
    ("catch", CompletionItemKind::KEYWORD),
    ("finally", CompletionItemKind::KEYWORD),
    ("import", CompletionItemKind::KEYWORD),
    ("async", CompletionItemKind::KEYWORD),
    ("spawn", CompletionItemKind::KEYWORD),
    ("await", CompletionItemKind::KEYWORD),
    ("send", CompletionItemKind::KEYWORD),
    ("recv", CompletionItemKind::KEYWORD),
    ("channel", CompletionItemKind::KEYWORD),
    ("true", CompletionItemKind::VALUE),
    ("false", CompletionItemKind::VALUE),
    ("null", CompletionItemKind::VALUE),
];

const BUILTINS: &[(&str, &str)] = &[
    ("println", "println(value: Any)"),
    ("print", "print(value: Any)"),
    ("assert", "assert(cond: Bool)"),
    ("exit", "exit(code: Int64)"),
    ("abs", "abs(x: Int64) -> Int64"),
    ("min", "min(a: Int64, b: Int64) -> Int64"),
    ("max", "max(a: Int64, b: Int64) -> Int64"),
    ("sqrt", "sqrt(x: Float64) -> Float64"),
    ("pow", "pow(x: Float64, y: Float64) -> Float64"),
    ("floor", "floor(x: Float64) -> Float64"),
    ("ceil", "ceil(x: Float64) -> Float64"),
    ("round", "round(x: Float64) -> Float64"),
];

pub fn completions(source: &SourceFile) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = Vec::new();

    for (kw, kind) in KEYWORDS {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(*kind),
            ..Default::default()
        });
    }

    for (name, detail) in BUILTINS {
        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(detail.to_string()),
            ..Default::default()
        });
    }

    for decl in &source.decls {
        match &decl.node {
            DeclKind::Function(f) => {
                let doc = f.doc.as_ref().map(|d| format_doc(d));
                items.push(CompletionItem {
                    label: f.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(fn_signature(f)),
                    documentation: doc.map(|d| tower_lsp::lsp_types::Documentation::MarkupContent(
                        tower_lsp::lsp_types::MarkupContent {
                            kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                            value: d,
                        }
                    )),
                    ..Default::default()
                });
            }
            DeclKind::Class(c) => {
                let doc = c.doc.as_ref().map(|d| format_doc(d));
                items.push(CompletionItem {
                    label: c.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {}", c.name)),
                    documentation: doc.map(|d| tower_lsp::lsp_types::Documentation::MarkupContent(
                        tower_lsp::lsp_types::MarkupContent {
                            kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                            value: d,
                        }
                    )),
                    ..Default::default()
                });
            }
            DeclKind::Immutable(u) => {
                let fields: Vec<String> = u.fields.iter()
                    .map(|f| format!("{}: {}", f.name, type_str(&f.param_type)))
                    .collect();
                items.push(CompletionItem {
                    label: u.name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("immutable {}({})", u.name, fields.join(", "))),
                    ..Default::default()
                });
            }
            DeclKind::Enum(e) => {
                let doc = e.doc.as_ref().map(|d| format_doc(d));
                items.push(CompletionItem {
                    label: e.name.clone(),
                    kind: Some(CompletionItemKind::ENUM),
                    documentation: doc.map(|d| tower_lsp::lsp_types::Documentation::MarkupContent(
                        tower_lsp::lsp_types::MarkupContent {
                            kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                            value: d,
                        }
                    )),
                    ..Default::default()
                });
                for v in &e.variants {
                    let doc = v.doc.as_ref().map(|d| format_doc(d));
                    items.push(CompletionItem {
                        label: format!("{}.{}", e.name, v.name),
                        kind: Some(CompletionItemKind::ENUM_MEMBER),
                        documentation: doc.map(|d| tower_lsp::lsp_types::Documentation::MarkupContent(
                            tower_lsp::lsp_types::MarkupContent {
                                kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                                value: d,
                            }
                        )),
                        ..Default::default()
                    });
                }
            }
            DeclKind::Interface(i) => {
                let doc = i.doc.as_ref().map(|d| format_doc(d));
                items.push(CompletionItem {
                    label: i.name.clone(),
                    kind: Some(CompletionItemKind::INTERFACE),
                    detail: Some(format!("interface {}", i.name)),
                    documentation: doc.map(|d| tower_lsp::lsp_types::Documentation::MarkupContent(
                        tower_lsp::lsp_types::MarkupContent {
                            kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                            value: d,
                        }
                    )),
                    ..Default::default()
                });
            }
            _ => {}
        }
    }

    items
}

// --- Context-aware Dot Completion ---

pub fn completions_at(
    source: &SourceFile,
    text: &str,
    offset: u32,
    stdlib: &TypeRegistry,
) -> Vec<CompletionItem> {
    let end = offset.min(text.len() as u32) as usize;

    // @-annotation completions
    if let Some(items) = annotation_completions_at(&text[..end]) {
        return items;
    }

    if let Some(chain) = extract_dot_chain(&text[..end]) {
        // Build a combined registry: stdlib + classes from the current file
        let mut combined = build_registry_from_source(source);
        for (k, v) in stdlib {
            combined.entry(k.clone()).or_insert_with(|| clone_class_info(v));
        }

        if let Some(type_name) = resolve_chain_type(source, &combined, &chain) {
            if let Some(info) = combined.get(&type_name) {
                return member_completions(info);
            }
        }
    }

    completions(source)
}

/// Returns annotation completions if the cursor is directly after `@`.
fn annotation_completions_at(text: &str) -> Option<Vec<CompletionItem>> {
    let t = text.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    if !t.ends_with('@') {
        return None;
    }
    // Prefix the user has already typed after @
    let prefix = &text[t.len()..];
    Some(annotation_items()
        .into_iter()
        .filter(|i| i.label.to_lowercase().starts_with(&prefix.to_lowercase()))
        .collect())
}

struct AnnotationMeta {
    name: &'static str,
    detail: &'static str,
    doc: &'static str,
    /// snippet inserted after @Name, e.g. `("$1")` or `("$1", "$2")`
    insert: &'static str,
    target: &'static str,
}

const ANNOTATIONS: &[AnnotationMeta] = &[
    AnnotationMeta { name: "Command",          detail: "CLI entry point",
        doc: "Marks a class as a CLI command.  \n**Args:** `name`, `description`[, `version`]",
        insert: "(\"${1:name}\", \"${2:description}\")", target: "class" },
    AnnotationMeta { name: "Option",           detail: "CLI option",
        doc: "Binds a field to a command-line option.  \n**Args:** `\"--long,-s\"`, `\"description\"`[, `required`]",
        insert: "(\"${1:--name,-n}\", \"${2:description}\")", target: "field" },
    AnnotationMeta { name: "Argument",         detail: "Positional CLI argument",
        doc: "Binds a field to a positional argument.  \n**Args:** `index`, `\"description\"`",
        insert: "(${1:0}, \"${2:description}\")", target: "field" },
    AnnotationMeta { name: "Test",             detail: "Unit test method",
        doc: "Marks a method as a test case.  \nReturn `true` to pass, `false` to fail.",
        insert: "(\"${1:test description}\")", target: "method" },
    AnnotationMeta { name: "GET",              detail: "HTTP GET route",
        doc: "Maps the method to a GET endpoint.",
        insert: "", target: "method" },
    AnnotationMeta { name: "POST",             detail: "HTTP POST route",
        doc: "Maps the method to a POST endpoint.",
        insert: "", target: "method" },
    AnnotationMeta { name: "PUT",              detail: "HTTP PUT route",  doc: "Maps the method to a PUT endpoint.",
        insert: "", target: "method" },
    AnnotationMeta { name: "DELETE",           detail: "HTTP DELETE route", doc: "Maps the method to a DELETE endpoint.",
        insert: "", target: "method" },
    AnnotationMeta { name: "Path",             detail: "URL path prefix",
        doc: "Sets the URL path for a controller class or route method.  \n**Arg:** `\"/path\"`",
        insert: "(\"${1:/path}\")", target: "class or method" },
    AnnotationMeta { name: "StatusCode",       detail: "HTTP status code",
        doc: "Sets the default HTTP status code.  \n**Arg:** integer",
        insert: "(${1:200})", target: "method" },
    AnnotationMeta { name: "ApplicationComponent", detail: "Singleton DI component",
        doc: "One instance per application lifetime (lazy singleton).", insert: "", target: "class" },
    AnnotationMeta { name: "Startup",          detail: "Eager singleton DI component",
        doc: "Created immediately at application startup.", insert: "", target: "class" },
    AnnotationMeta { name: "HttpRequestScoped",detail: "Per-request DI component",
        doc: "A fresh instance is created for each HTTP request.", insert: "", target: "class" },
    AnnotationMeta { name: "Inject",           detail: "DI field injection",
        doc: "Marks a field for compile-time dependency injection.", insert: "", target: "field" },
    AnnotationMeta { name: "Config",           detail: "application.properties injection",
        doc: "Injects a value from `application.properties`.  \n**Arg:** `\"key\"`",
        insert: "(\"${1:config.key}\")", target: "field" },
    AnnotationMeta { name: "Log",              detail: "Logger injection",
        doc: "Injects a `log: Logger` field initialized with the class name.", insert: "", target: "class" },
    AnnotationMeta { name: "PATCH",            detail: "HTTP PATCH route",
        doc: "Maps the method to a PATCH endpoint.",
        insert: "", target: "method" },
    AnnotationMeta { name: "Auth",             detail: "HTTP authentication",
        doc: "Requires authentication on this route or controller.  \n**Arg:** `\"bearer\"` or `\"basic\"`",
        insert: "(\"${1:bearer}\")", target: "class or method" },
    AnnotationMeta { name: "Produces",         detail: "Response Content-Type",
        doc: "Sets the `Content-Type` header of the response.  \n**Arg:** MIME type string, e.g. `\"application/json\"`",
        insert: "(\"${1:application/json}\")", target: "method" },
    AnnotationMeta { name: "Consumes",         detail: "Accepted request Content-Type",
        doc: "Declares the expected `Content-Type` of the request body.  \n**Arg:** MIME type string, e.g. `\"application/json\"`",
        insert: "(\"${1:application/json}\")", target: "method" },
    AnnotationMeta { name: "Sensitive",        detail: "Fully mask field in logs",
        doc: "Marks a field as sensitive.  \nThe value is replaced with `***` in all log output.",
        insert: "", target: "field" },
    AnnotationMeta { name: "Masked",           detail: "Partially mask field in logs",
        doc: "Marks a field for partial masking in log output.  \nOptional **args:** `visibleStart`, `visibleEnd` (default 2, 2).",
        insert: "", target: "field" },
    AnnotationMeta { name: "DoNotSerialize",   detail: "Exclude field from serialization",
        doc: "Excludes the field from JSON/XML serialization.  \nThe field is omitted when `toJson()` or `toXml()` is called.",
        insert: "", target: "field" },
    AnnotationMeta { name: "JsonSerializable", detail: "Generate toJson() method",
        doc: "Generates a `toJson(): String` method for the class.  \nRespects `@DoNotSerialize` on individual fields.",
        insert: "", target: "class" },
    AnnotationMeta { name: "annotation",       detail: "Define a custom annotation",
        doc: "Marks a class as a custom annotation definition.  \nInstances of this class can then be used as `@ClassName` on other declarations.",
        insert: "", target: "class" },
    AnnotationMeta { name: "inline",           detail: "Inline hint",
        doc: "Hints to the compiler that this function or method should be inlined.",
        insert: "", target: "fn or method" },
    AnnotationMeta { name: "deprecated",       detail: "Deprecation marker",
        doc: "Marks the declaration as deprecated.  \nOptional **arg:** deprecation message.",
        insert: "(\"${1:use X instead}\")", target: "class, fn, or method" },
];

fn annotation_items() -> Vec<CompletionItem> {
    ANNOTATIONS.iter().map(|a| {
        let insert_text = if a.insert.is_empty() {
            a.name.to_string()
        } else {
            format!("{}{}", a.name, a.insert)
        };
        CompletionItem {
            label: a.name.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(format!("@{} — {} [{}]", a.name, a.detail, a.target)),
            insert_text: Some(insert_text),
            insert_text_format: Some(tower_lsp::lsp_types::InsertTextFormat::SNIPPET),
            documentation: Some(mk_doc(a.doc)),
            ..Default::default()
        }
    }).collect()
}

/// Returns the documentation string for an annotation name, or None if unknown.
pub fn annotation_doc(name: &str) -> Option<String> {
    ANNOTATIONS.iter().find(|a| a.name == name).map(|a| {
        format!("**@{}** — {}  \n\nTarget: `{}`  \n\n{}", a.name, a.detail, a.target, a.doc)
    })
}

fn clone_class_info(src: &ClassInfo) -> ClassInfo {
    ClassInfo {
        fields: src.fields.iter().map(|f| FieldInfo {
            name: f.name.clone(),
            type_name: f.type_name.clone(),
            doc: f.doc.clone(),
        }).collect(),
        methods: src.methods.iter().map(|m| MethodInfo {
            name: m.name.clone(),
            signature: m.signature.clone(),
            doc: m.doc.clone(),
        }).collect(),
    }
}

/// Extracts the dot-receiver chain from the text ending with `.` or `.partial_ident`.
/// e.g. `"ctx."` → `["ctx"]`, `"ctx.re"` → `["ctx"]`, `"ctx.response."` → `["ctx", "response"]`
fn extract_dot_chain(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim_end_matches([' ', '\t']);
    // strip partial identifier the user may have started typing after the last dot
    let trimmed = trimmed.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    if !trimmed.ends_with('.') {
        return None;
    }

    let mut chain: Vec<String> = Vec::new();
    let mut rest = &trimmed[..trimmed.len() - 1]; // strip trailing dot

    loop {
        let rest_trimmed = rest.trim_end();
        let end = rest_trimmed.len();
        let start = rest_trimmed
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let ident = &rest_trimmed[start..end];

        if ident.is_empty()
            || !ident.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
        {
            break;
        }
        chain.push(ident.to_string());

        let before = rest_trimmed[..start].trim_end();
        if let Some(stripped) = before.strip_suffix('.') {
            rest = stripped;
        } else {
            break;
        }
    }

    if chain.is_empty() {
        return None;
    }
    chain.reverse();
    Some(chain)
}

/// Resolves the type of a dot-chain like `["ctx"]` or `["ctx", "response"]`.
fn resolve_chain_type(source: &SourceFile, registry: &TypeRegistry, chain: &[String]) -> Option<String> {
    // Try variable lookup first; fall back to class name itself for static access (e.g. `Json.`)
    let first_type = resolve_var_type(source, &chain[0])
        .or_else(|| if registry.contains_key(&chain[0]) { Some(chain[0].clone()) } else { None })?;
    let mut current = first_type;
    for field_name in &chain[1..] {
        let info = registry.get(&current)?;
        current = info.fields.iter().find(|f| &f.name == field_name)?.type_name.clone();
    }
    Some(current)
}

/// Finds the declared type of a variable by name across all methods/functions in the AST.
fn resolve_var_type(source: &SourceFile, name: &str) -> Option<String> {
    for decl in &source.decls {
        match &decl.node {
            DeclKind::Function(f) => {
                for p in &f.params {
                    if p.name == name {
                        return Some(type_str(&p.param_type));
                    }
                }
                if let Some(t) = find_var_in_stmt(&f.body, name) {
                    return Some(t);
                }
            }
            DeclKind::Class(c) => {
                if name == "this" {
                    return Some(c.name.clone());
                }
                for m in &c.methods {
                    for p in &m.params {
                        if p.name == name {
                            return Some(type_str(&p.param_type));
                        }
                    }
                    if let Some(t) = find_var_in_stmt(&m.body, name) {
                        return Some(t);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn find_var_in_stmt(stmt: &Stmt, name: &str) -> Option<String> {
    match &stmt.node {
        StmtKind::Block(stmts) => stmts.iter().find_map(|s| find_var_in_stmt(s, name)),
        StmtKind::Let { name: n, ty: Some(t), .. } if n == name => Some(type_str(t)),
        StmtKind::Var { name: n, ty: Some(t), .. } if n == name => Some(type_str(t)),
        StmtKind::Let { name: n, ty: None, value: Some(v), .. } if n == name => {
            infer_type_from_expr(v)
        }
        StmtKind::Var { name: n, ty: None, value: Some(v), .. } if n == name => {
            infer_type_from_expr(v)
        }
        StmtKind::If { then_branch, else_branch, .. } => {
            find_var_in_stmt(then_branch, name)
                .or_else(|| else_branch.as_ref().and_then(|e| find_var_in_stmt(e, name)))
        }
        StmtKind::While { body, .. } | StmtKind::Loop { body } | StmtKind::For { body, .. } => {
            find_var_in_stmt(body, name)
        }
        _ => None,
    }
}

fn infer_type_from_expr(expr: &Expr) -> Option<String> {
    match &expr.node {
        ExprKind::New { class, .. } => Some(class.clone()),
        _ => None,
    }
}

fn member_completions(info: &ClassInfo) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for f in &info.fields {
        items.push(CompletionItem {
            label: f.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(f.type_name.clone()),
            documentation: f.doc.as_deref().map(mk_doc),
            ..Default::default()
        });
    }
    for m in &info.methods {
        items.push(CompletionItem {
            label: m.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: Some(m.signature.clone()),
            documentation: m.doc.as_deref().map(mk_doc),
            ..Default::default()
        });
    }
    items
}

fn mk_doc(d: &str) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: d.to_owned(),
    })
}

// --- Go to Definition ---

pub fn definition_at(source: &SourceFile, uri: &Url, offset: u32) -> Option<Location> {
    let target = ident_at(source, offset)?;
    for decl in &source.decls {
        let (name, span) = match &decl.node {
            DeclKind::Function(f) => (f.name.as_str(), f.span),
            DeclKind::Class(c) => (c.name.as_str(), c.span),
            DeclKind::Enum(e) => (e.name.as_str(), e.span),
            DeclKind::Interface(i) => (i.name.as_str(), i.span),
            DeclKind::Immutable(u) => (u.name.as_str(), u.span),
            _ => continue,
        };
        if name == target {
            return Some(Location {
                uri: uri.clone(),
                range: span_to_range(span),
            });
        }
    }
    None
}

fn ident_at(source: &SourceFile, offset: u32) -> Option<String> {
    for decl in &source.decls {
        if let Some(s) = ident_in_decl(decl, offset) {
            return Some(s);
        }
    }
    None
}

fn ident_in_decl(decl: &Decl, offset: u32) -> Option<String> {
    if !span_contains(decl.span, offset) {
        return None;
    }
    match &decl.node {
        DeclKind::Function(f) => ident_in_stmt(&f.body, offset),
        DeclKind::Class(c) => {
            for m in &c.methods {
                if span_contains(m.span, offset) {
                    if let Some(s) = ident_in_stmt(&m.body, offset) {
                        return Some(s);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn ident_in_stmt(stmt: &Stmt, offset: u32) -> Option<String> {
    if !span_contains(stmt.span, offset) {
        return None;
    }
    match &stmt.node {
        StmtKind::Block(stmts) => {
            for s in stmts {
                if let Some(r) = ident_in_stmt(s, offset) {
                    return Some(r);
                }
            }
            None
        }
        StmtKind::Expr(e) => ident_in_expr(e, offset),
        StmtKind::Return(Some(e)) => ident_in_expr(e, offset),
        StmtKind::Let { value: Some(v), .. } => ident_in_expr(v, offset),
        StmtKind::Var { value: Some(v), .. } => ident_in_expr(v, offset),
        StmtKind::If { cond, then_branch, else_branch } => {
            ident_in_expr(cond, offset)
                .or_else(|| ident_in_stmt(then_branch, offset))
                .or_else(|| else_branch.as_ref().and_then(|e| ident_in_stmt(e, offset)))
        }
        StmtKind::While { cond, body } => {
            ident_in_expr(cond, offset).or_else(|| ident_in_stmt(body, offset))
        }
        StmtKind::For { body, .. } => ident_in_stmt(body, offset),
        StmtKind::Loop { body } => ident_in_stmt(body, offset),
        _ => None,
    }
}

fn ident_in_expr(expr: &Expr, offset: u32) -> Option<String> {
    if !span_contains(expr.span, offset) {
        return None;
    }
    match &expr.node {
        ExprKind::Ident(name) => Some(name.clone()),
        ExprKind::Call { func, args } => {
            ident_in_expr(func, offset).or_else(|| {
                args.iter().find_map(|a| ident_in_expr(a, offset))
            })
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            ident_in_expr(lhs, offset).or_else(|| ident_in_expr(rhs, offset))
        }
        ExprKind::Block(stmts) => stmts.iter().find_map(|s| ident_in_stmt(s, offset)),
        _ => None,
    }
}

// --- Document Symbols ---

pub fn document_symbols(source: &SourceFile) -> Vec<tower_lsp::lsp_types::DocumentSymbol> {
    source.decls.iter().filter_map(decl_to_symbol).collect()
}

#[allow(deprecated)]
fn decl_to_symbol(decl: &Decl) -> Option<tower_lsp::lsp_types::DocumentSymbol> {
    use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};
    match &decl.node {
        DeclKind::Function(f) => {
            let detail = if let Some(doc) = &f.doc {
                Some(format!("{}\n\n{}", fn_signature(f), format_doc(doc)))
            } else {
                Some(fn_signature(f))
            };
            Some(DocumentSymbol {
                name: f.name.clone(),
                detail,
                kind: SymbolKind::FUNCTION,
                range: span_to_range(f.span),
                selection_range: span_to_range(f.span),
                children: None,
                tags: None,
                deprecated: None,
            })
        }
        DeclKind::Class(c) => {
            let detail = c.doc.as_ref().map(|doc| format_doc(doc));
            let children: Vec<_> = c
                .methods
                .iter()
                .map(|m| {
                    let method_detail = if let Some(doc) = &m.doc {
                        Some(format!("{}\n\n{}", method_signature(&c.name, m), format_doc(doc)))
                    } else {
                        Some(method_signature(&c.name, m))
                    };
                    DocumentSymbol {
                        name: m.name.clone(),
                        detail: method_detail,
                        kind: SymbolKind::METHOD,
                        range: span_to_range(m.span),
                        selection_range: span_to_range(m.span),
                        children: None,
                        tags: None,
                deprecated: None,
                            }
                })
                .collect();
            Some(DocumentSymbol {
                name: c.name.clone(),
                detail,
                kind: SymbolKind::CLASS,
                range: span_to_range(c.span),
                selection_range: span_to_range(c.span),
                children: Some(children),
                tags: None,
                deprecated: None,
            })
        }
        DeclKind::Enum(e) => {
            let detail = e.doc.as_ref().map(|doc| format_doc(doc));
            let children: Vec<_> = e
                .variants
                .iter()
                .map(|v| {
                    let variant_detail = v.doc.as_ref().map(|doc| format_doc(doc));
                    DocumentSymbol {
                        name: v.name.clone(),
                        detail: variant_detail,
                        kind: SymbolKind::ENUM_MEMBER,
                        range: span_to_range(v.span),
                        selection_range: span_to_range(v.span),
                        children: None,
                        tags: None,
                deprecated: None,
                            }
                })
                .collect();
            Some(DocumentSymbol {
                name: e.name.clone(),
                detail,
                kind: SymbolKind::ENUM,
                range: span_to_range(e.span),
                selection_range: span_to_range(e.span),
                children: Some(children),
                tags: None,
                deprecated: None,
            })
        }
        DeclKind::Interface(i) => {
            let detail = i.doc.as_ref().map(|doc| format_doc(doc));
            Some(DocumentSymbol {
                name: i.name.clone(),
                detail,
                kind: SymbolKind::INTERFACE,
                range: span_to_range(i.span),
                selection_range: span_to_range(i.span),
                children: None,
                tags: None,
                deprecated: None,
            })
        }
        DeclKind::Immutable(u) => {
            let fields: Vec<String> = u.fields.iter()
                .map(|f| format!("{}: {}", f.name, type_str(&f.param_type)))
                .collect();
            let children: Vec<_> = u.fields.iter().map(|f| {
                DocumentSymbol {
                    name: f.name.clone(),
                    detail: Some(type_str(&f.param_type)),
                    kind: SymbolKind::FIELD,
                    range: span_to_range(f.span),
                    selection_range: span_to_range(f.span),
                    children: None,
                    tags: None,
                deprecated: None,
                    }
            }).collect();
            Some(DocumentSymbol {
                name: u.name.clone(),
                detail: Some(format!("immutable {}({})", u.name, fields.join(", "))),
                kind: SymbolKind::STRUCT,
                range: span_to_range(u.span),
                selection_range: span_to_range(u.span),
                children: Some(children),
                tags: None,
                deprecated: None,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    fn parse_src(src: &str) -> SourceFile {
        let tokens = Lexer::new(src).tokenize().unwrap();
        Parser::new(tokens).parse().unwrap()
    }

    fn make_stdlib() -> TypeRegistry {
        // CARGO_MANIFEST_DIR = .../crates/tinox-lsp
        // One-type-per-file (2026-07-26): http_server is now a directory of
        // several `<TypeName>.tnx` files instead of one `http_server.tnx` —
        // parse and merge all of them, same as build_embedded_stdlib() does.
        // Core/extended stdlib split (see CLAUDE.md): http_server is
        // extended-tier, so it lives under tinox-core-ext now, not
        // tinox-core. Namespace-mirroring migration (issue #185): its
        // content moved one level deeper, under its own
        // tinox/core/http_server/ subtree.
        let manifest = env!("CARGO_MANIFEST_DIR");
        let dir = format!("{}/../../crates/tinox-core-ext/http_server/tinox/core/http_server", manifest);
        let mut registry = HashMap::new();
        let Ok(entries) = std::fs::read_dir(&dir) else { return registry };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let Ok(tokens) = Lexer::new(&src).tokenize() else { continue };
            let Ok(ast) = Parser::new(tokens).parse() else { continue };
            for (name, info) in build_registry_from_source(&ast) {
                registry.entry(name).or_insert(info);
            }
        }
        registry
    }

    #[test]
    fn test_extract_dot_chain_simple() {
        assert_eq!(extract_dot_chain("ctx."), Some(vec!["ctx".to_string()]));
        assert_eq!(extract_dot_chain("  ctx."), Some(vec!["ctx".to_string()]));
        assert_eq!(extract_dot_chain("ctx.response."), Some(vec!["ctx".to_string(), "response".to_string()]));
        assert_eq!(extract_dot_chain("no_dot"), None);
        assert_eq!(extract_dot_chain(""), None);
        // partial identifier after dot should still trigger completion
        assert_eq!(extract_dot_chain("ctx.re"), Some(vec!["ctx".to_string()]));
        assert_eq!(extract_dot_chain("ctx.response.bo"), Some(vec!["ctx".to_string(), "response".to_string()]));
    }

    #[test]
    fn test_stdlib_loads() {
        let stdlib = make_stdlib();
        println!("Loaded {} classes: {:?}", stdlib.len(), stdlib.keys().collect::<Vec<_>>());
        assert!(stdlib.contains_key("HttpContext"), "HttpContext not in stdlib");
        assert!(stdlib.contains_key("HttpResponse"), "HttpResponse not in stdlib");
    }

    #[test]
    fn test_resolve_param_type() {
        let src = r#"
class UserController {
    fn listUsers(ctx: HttpContext) -> Nothing {
    }
}
"#;
        let ast = parse_src(src);
        let result = resolve_var_type(&ast, "ctx");
        assert_eq!(result, Some("HttpContext".to_string()));
    }

    #[test]
    fn test_completions_at_ctx_dot() {
        let src = r#"
class UserController {
    fn listUsers(ctx: HttpContext) -> Nothing {
    }
}
"#;
        let ast = parse_src(src);
        let stdlib = make_stdlib();
        let text = format!("{}\n    ctx.", &src[..src.len()-2]);
        let offset = text.len() as u32;
        let items = completions_at(&ast, &text, offset, &stdlib);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        println!("completion items: {:?}", labels);
        assert!(labels.contains(&"response"), "should suggest 'response'");
        assert!(labels.contains(&"request"), "should suggest 'request'");
    }

    #[test]
    fn test_annotation_completions_at_sign() {
        let ast = parse_src("class Foo {}");
        let stdlib = HashMap::new();
        // Cursor right after @
        let text = "@".to_string();
        let items = completions_at(&ast, &text, text.len() as u32, &stdlib);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Command"), "@ should suggest Command");
        assert!(labels.contains(&"Test"),    "@ should suggest Test");
        assert!(labels.contains(&"GET"),     "@ should suggest GET");
    }

    #[test]
    fn test_annotation_completions_filtered() {
        let ast = parse_src("class Foo {}");
        let stdlib = HashMap::new();
        let text = "@Co".to_string();
        let items = completions_at(&ast, &text, text.len() as u32, &stdlib);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Command"), "@Co should suggest Command");
        assert!(!labels.contains(&"GET"),    "@Co should not suggest GET");
    }

    #[test]
    fn test_this_dot_completion() {
        let src = r#"
class UserService {
    var repo: UserRepository;
    fn save() -> Nothing {
    }
}
class UserRepository {
    fn findAll() -> Nothing {
    }
}
"#;
        let ast = parse_src(src);
        let combined = build_registry_from_source(&ast);
        let text = format!("{}\n    this.", &src[..src.len()-2]);
        let offset = text.len() as u32;
        let items = completions_at(&ast, &text, offset, &combined);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"repo"), "this. should suggest 'repo' field");
        assert!(labels.contains(&"save"), "this. should suggest 'save' method");
    }

    #[test]
    fn test_new_type_inference() {
        let src = r#"
class Foo {
    fn bar() -> Nothing {
        let svc = new UserService();
    }
}
class UserService {
    fn doThing() -> Nothing {
    }
}
"#;
        let ast = parse_src(src);
        let result = resolve_var_type(&ast, "svc");
        assert_eq!(result, Some("UserService".to_string()));
    }

    fn parse_for_completion_test(text: &str, offset: usize) -> Option<SourceFile> {
        let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[offset..].find('\n').map(|i| offset + i).unwrap_or(text.len());
        let blank = " ".repeat(line_end - line_start);
        let cleaned = format!("{}{}{}", &text[..line_start], blank, &text[line_end..]);
        let tokens = Lexer::new(&cleaned).tokenize().ok()?;
        Parser::new(tokens).parse().ok()
    }

    #[test]
    fn test_import_dot_completion() {
        // Exact scenario: import + ctx.re should suggest response
        let src = r#"import tinox.core.http_server;

class UserController
{
    fn listUsers(ctx: HttpContext) -> Nothing
    {
        ctx.re
    }
}
"#;
        let stdlib = make_stdlib();
        println!("stdlib classes: {:?}", stdlib.keys().collect::<Vec<_>>());
        assert!(!stdlib.is_empty(), "stdlib must not be empty for this test");
        assert!(stdlib.contains_key("HttpContext"), "HttpContext must be in stdlib");

        let cursor_line = "        ctx.re";
        let offset = src.find(cursor_line).unwrap() + cursor_line.len();

        // Use parse_for_completion (blanks the incomplete line) — same as LSP does
        let ast = parse_for_completion_test(src, offset)
            .expect("parse_for_completion should succeed");
        let items = completions_at(&ast, src, offset as u32, &stdlib);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        println!("completion items: {:?}", labels);
        assert!(labels.contains(&"response"), "ctx.re should suggest 'response'");
        assert!(labels.contains(&"request"), "ctx.re should suggest 'request'");
    }

    #[test]
    fn test_annotation_doc() {
        let doc = annotation_doc("Test");
        assert!(doc.is_some());
        assert!(doc.unwrap().contains("@Test"));

        let doc = annotation_doc("NonExistent");
        assert!(doc.is_none());
    }

    // --- lsp_pos_to_offset ---

    #[test]
    fn test_lsp_pos_to_offset_start() {
        let src = "hello\nworld";
        assert_eq!(lsp_pos_to_offset(src, Position { line: 0, character: 0 }), 0);
    }

    #[test]
    fn test_lsp_pos_to_offset_midline() {
        let src = "hello\nworld";
        assert_eq!(lsp_pos_to_offset(src, Position { line: 0, character: 3 }), 3);
    }

    #[test]
    fn test_lsp_pos_to_offset_second_line() {
        let src = "hello\nworld";
        // line 1, character 0 → offset 6 (after 'h','e','l','l','o','\n')
        assert_eq!(lsp_pos_to_offset(src, Position { line: 1, character: 0 }), 6);
    }

    #[test]
    fn test_lsp_pos_to_offset_second_line_mid() {
        let src = "hello\nworld";
        assert_eq!(lsp_pos_to_offset(src, Position { line: 1, character: 3 }), 9);
    }

    // --- type_str ---

    #[test]
    fn test_type_str_primitives() {
        assert_eq!(type_str(&Type::Int32), "Int32");
        assert_eq!(type_str(&Type::Int64), "Int64");
        assert_eq!(type_str(&Type::Float64), "Float64");
        assert_eq!(type_str(&Type::Bool), "Bool");
        assert_eq!(type_str(&Type::String), "String");
        assert_eq!(type_str(&Type::Nothing), "Nothing");
        assert_eq!(type_str(&Type::Never), "Never");
        assert_eq!(type_str(&Type::Any), "Any");
        assert_eq!(type_str(&Type::Infer), "_");
    }

    #[test]
    fn test_type_str_array() {
        assert_eq!(type_str(&Type::Array(Box::new(Type::Int32))), "Array<Int32>");
    }

    #[test]
    fn test_type_str_tuple() {
        let t = Type::Tuple(vec![Type::Int32, Type::String]);
        assert_eq!(type_str(&t), "(Int32, String)");
    }

    #[test]
    fn test_type_str_fn() {
        let t = Type::Fn {
            params: vec![Type::Int32],
            ret: Box::new(Type::Bool),
        };
        assert_eq!(type_str(&t), "fn(Int32) -> Bool");
    }

    #[test]
    fn test_type_str_generic() {
        let t = Type::Generic { name: "List".into(), args: vec![Type::String] };
        assert_eq!(type_str(&t), "List<String>");
    }

    #[test]
    fn test_type_str_map() {
        let t = Type::Map(Box::new(Type::String), Box::new(Type::Int64));
        assert_eq!(type_str(&t), "Map<String, Int64>");
    }

    #[test]
    fn test_type_str_mutable() {
        assert_eq!(type_str(&Type::Mutable(Box::new(Type::Int32))), "mut Int32");
    }

    #[test]
    fn test_type_str_ref() {
        assert_eq!(type_str(&Type::Ref(Box::new(Type::String))), "&String");
    }

    // --- fn_signature ---

    #[test]
    fn test_fn_signature_no_params() {
        let ast = parse_src("fn greet() -> Nothing {}");
        let DeclKind::Function(f) = &ast.decls[0].node else { panic!() };
        assert_eq!(fn_signature(f), "fn greet() -> Nothing");
    }

    #[test]
    fn test_fn_signature_with_params() {
        let ast = parse_src("fn add(a: Int32, b: Int32) -> Int32 { return a + b; }");
        let DeclKind::Function(f) = &ast.decls[0].node else { panic!() };
        let sig = fn_signature(f);
        assert!(sig.contains("fn add("));
        assert!(sig.contains("a: Int32"));
        assert!(sig.contains("b: Int32"));
        assert!(sig.contains("-> Int32"));
    }

    #[test]
    fn test_fn_signature_async() {
        let ast = parse_src("async fn fetch() -> Nothing {}");
        let DeclKind::Function(f) = &ast.decls[0].node else { panic!() };
        assert!(fn_signature(f).starts_with("async fn fetch"));
    }

    // --- completions ---

    #[test]
    fn test_completions_includes_keywords() {
        let ast = parse_src("fn main() {}");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"fn"));
        assert!(labels.contains(&"let"));
        assert!(labels.contains(&"class"));
        assert!(labels.contains(&"while"));
        assert!(labels.contains(&"true"));
        assert!(labels.contains(&"null"));
    }

    #[test]
    fn test_completions_includes_builtins() {
        let ast = parse_src("fn main() {}");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"println"));
        assert!(labels.contains(&"print"));
        assert!(labels.contains(&"sqrt"));
    }

    #[test]
    fn test_completions_includes_user_functions() {
        let ast = parse_src("fn myFunc(x: Int32) -> Int32 { return x; }");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"myFunc"));
    }

    #[test]
    fn test_completions_includes_user_class() {
        let ast = parse_src("class Dog { fn bark() -> Nothing {} }");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Dog"));
    }

    #[test]
    fn test_completions_includes_enum_and_variants() {
        let ast = parse_src("enum Color { Red, Green, Blue }");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Color"));
        assert!(labels.contains(&"Color.Red"));
        assert!(labels.contains(&"Color.Green"));
    }

    #[test]
    fn test_completions_includes_interface() {
        let ast = parse_src("interface Runnable { fn run() -> Nothing; }");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Runnable"));
    }

    #[test]
    fn test_completions_includes_immutable() {
        let ast = parse_src("immutable Point(x: Int32, y: Int32)");
        let items = completions(&ast);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Point"));
    }

    // --- build_registry_from_source ---

    #[test]
    fn test_registry_class_with_fields_and_methods() {
        let ast = parse_src(r#"
class Person {
    var name: String;
    var age: Int32;
    fn greet() -> Nothing {}
}
"#);
        let reg = build_registry_from_source(&ast);
        assert!(reg.contains_key("Person"));
        let info = &reg["Person"];
        let field_names: Vec<_> = info.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"name"));
        assert!(field_names.contains(&"age"));
        let method_names: Vec<_> = info.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(method_names.contains(&"greet"));
    }

    #[test]
    fn test_registry_empty_for_no_classes() {
        let ast = parse_src("fn main() {}");
        let reg = build_registry_from_source(&ast);
        assert!(reg.is_empty());
    }

    #[test]
    fn test_registry_namespace_classes_collected() {
        let ast = parse_src(r#"
namespace net {
    class Socket {
        var port: Int32;
    }
}
"#);
        let reg = build_registry_from_source(&ast);
        assert!(reg.contains_key("Socket"), "class inside namespace should be collected");
    }

    // --- document_symbols ---

    #[test]
    fn test_document_symbols_function() {
        let ast = parse_src("fn doWork() -> Nothing {}");
        let syms = document_symbols(&ast);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "doWork");
    }

    #[test]
    fn test_document_symbols_class_has_children() {
        let ast = parse_src(r#"
class Animal {
    fn speak() -> Nothing {}
    fn eat() -> Nothing {}
}
"#);
        let syms = document_symbols(&ast);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Animal");
        let children = syms[0].children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"speak"));
        assert!(names.contains(&"eat"));
    }

    #[test]
    fn test_document_symbols_enum_has_variant_children() {
        let ast = parse_src("enum Dir { North, South, East, West }");
        let syms = document_symbols(&ast);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Dir");
        let children = syms[0].children.as_ref().unwrap();
        assert_eq!(children.len(), 4);
    }

    #[test]
    fn test_document_symbols_multiple_decls() {
        let ast = parse_src(r#"
fn helper() -> Nothing {}
class Main {}
enum Status { Ok, Err }
"#);
        let syms = document_symbols(&ast);
        assert_eq!(syms.len(), 3);
    }

    #[test]
    fn test_document_symbols_immutable() {
        let ast = parse_src("immutable Vec2(x: Float32, y: Float32)");
        let syms = document_symbols(&ast);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Vec2");
        let children = syms[0].children.as_ref().unwrap();
        assert_eq!(children.len(), 2);
    }

    // --- annotation completions ---

    #[test]
    fn test_annotation_completions_all_annotations_present() {
        let ast = parse_src("class Foo {}");
        let items = completions_at(&ast, "@", 1, &HashMap::new());
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        // check a selection of known annotations
        for name in &["Command", "Option", "Test", "GET", "POST", "Inject", "Log", "inline", "deprecated"] {
            assert!(labels.contains(name), "missing annotation: {}", name);
        }
    }

    #[test]
    fn test_annotation_doc_all_known() {
        for ann in &["Command", "Option", "Argument", "Test", "GET", "POST", "PUT", "DELETE",
                     "Path", "StatusCode", "ApplicationComponent", "Startup",
                     "HttpRequestScoped", "Inject", "Config", "Log", "inline", "deprecated"] {
            let doc = annotation_doc(ann);
            assert!(doc.is_some(), "annotation_doc should return Some for @{}", ann);
            assert!(doc.unwrap().contains(*ann));
        }
    }

    #[test]
    fn test_annotation_completions_case_insensitive_filter() {
        let ast = parse_src("class Foo {}");
        let text = "@get";
        let items = completions_at(&ast, text, text.len() as u32, &HashMap::new());
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"GET"), "@get (lowercase) should match @GET");
    }

    // --- extract_dot_chain edge cases ---

    #[test]
    fn test_extract_dot_chain_no_dot() {
        assert_eq!(extract_dot_chain("hello"), None);
        assert_eq!(extract_dot_chain(""), None);
    }

    #[test]
    fn test_extract_dot_chain_three_levels() {
        let result = extract_dot_chain("a.b.c.");
        assert_eq!(result, Some(vec!["a".into(), "b".into(), "c".into()]));
    }

    #[test]
    fn test_extract_dot_chain_with_spaces() {
        assert_eq!(extract_dot_chain("  foo."), Some(vec!["foo".into()]));
    }

    #[test]
    fn test_extract_dot_chain_leading_dot_is_none() {
        assert_eq!(extract_dot_chain("."), None);
    }

    // --- hover ---

    #[test]
    fn test_hover_returns_none_for_empty_source() {
        let ast = parse_src("fn main() {}");
        // offset way past any token
        let result = hover_at(&ast, 99999);
        assert!(result.is_none());
    }

    #[test]
    fn test_hover_function_before_body() {
        let src = "fn add(a: Int32) -> Int32 { return a; }";
        let ast = parse_src(src);
        // offset 0 = start of `fn` keyword, definitely before the body `{`
        let result = hover_at(&ast, 0);
        // should get the signature
        assert!(result.is_some(), "hover at fn keyword should return something");
        let text = result.unwrap();
        assert!(text.contains("add"), "hover should show function name");
    }

    // --- completions_at falls back to full list when no dot ---

    #[test]
    fn test_completions_at_no_dot_returns_general() {
        let ast = parse_src("fn myHelper() -> Nothing {}");
        let text = "my";
        let items = completions_at(&ast, text, text.len() as u32, &HashMap::new());
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"myHelper"), "should include user function");
        assert!(labels.contains(&"let"), "should include keywords");
    }

    #[test]
    fn test_type_str_nullable() {
        assert_eq!(type_str(&Type::Nullable(Box::new(Type::String))), "String?");
        assert_eq!(type_str(&Type::Nullable(Box::new(Type::Int64))), "Int64?");
        assert_eq!(type_str(&Type::Nullable(Box::new(Type::Named("Foo".into())))), "Foo?");
    }

    #[test]
    fn test_hover_shows_nullable_type() {
        let ast = parse_src("fn f(x: String?) -> Nothing {}");
        let result = hover_at(&ast, 3);
        if let Some(text) = result {
            assert!(text.contains("String?"), "hover should display nullable type as 'String?', got: {}", text);
        }
    }
}

//! Body-free API records derived from the compiler AST, shared by CLI and LSP.
use crate::ast::*;
use crate::Formatter;
use serde::Serialize;
use tinox_common::Span;

pub fn type_name(ty: &Type) -> String {
    Formatter::new().fmt_type(ty)
}
pub fn type_parameters(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    }
}
pub fn visibility(value: &Visibility) -> &'static str {
    match value {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Protected => "protected",
        Visibility::Package => "package",
    }
}
fn parameters(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_name(&p.param_type)))
        .collect::<Vec<_>>()
        .join(", ")
}
pub fn function_signature(function: &Function) -> String {
    format!(
        "{}fn {}{}({}) -> {}",
        if function.is_async { "async " } else { "" },
        function.name,
        type_parameters(&function.type_params),
        parameters(&function.params),
        type_name(&function.ret_type)
    )
}
pub fn method_signature(method: &Method) -> String {
    format!(
        "{} {}{} {}{}({}) -> {}",
        visibility(&method.visibility),
        if method.is_async { "async " } else { "" },
        if method.static_ { "fnc" } else { "fn" },
        method.name,
        type_parameters(&method.type_params),
        parameters(&method.params),
        type_name(&method.ret_type)
    )
}
fn annotation_uses(annotations: &[Annotation]) -> Vec<String> {
    annotations
        .iter()
        .map(|a| {
            if a.args.is_empty() {
                format!("@{}", a.name)
            } else {
                format!(
                    "@{}({})",
                    a.name,
                    a.args
                        .iter()
                        .map(crate::formatter::fmt_annotation_arg)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Parameter {
    pub name: String,
    pub type_name: String,
    pub annotations: Vec<String>,
}
fn parameter_records(params: &[Param]) -> Vec<Parameter> {
    params
        .iter()
        .map(|p| Parameter {
            name: p.name.clone(),
            type_name: type_name(&p.param_type),
            annotations: annotation_uses(&p.annotations),
        })
        .collect()
}
#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub visibility: String,
    pub is_static: bool,
    pub type_parameters: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub type_name: Option<String>,
    pub documentation: Option<String>,
    pub documentation_truncated: bool,
    pub annotations: Vec<String>,
    pub range: Option<Span>,
    pub generated: bool,
}
fn method_record(method: &Method, owner: &str) -> Member {
    let factory = method.static_
        && match &method.ret_type {
            Type::Named(name) | Type::Generic { name, .. } => name == owner,
            _ => false,
        };
    Member {
        name: method.name.clone(),
        kind: if factory { "factory" } else { "method" }.into(),
        signature: method_signature(method),
        visibility: visibility(&method.visibility).into(),
        is_static: method.static_,
        type_parameters: method.type_params.clone(),
        parameters: parameter_records(&method.params),
        return_type: Some(type_name(&method.ret_type)),
        type_name: None,
        documentation: method.doc.clone(),
        documentation_truncated: false,
        annotations: annotation_uses(&method.annotations),
        range: Some(method.span),
        generated: false,
    }
}
fn field_record(field: &FieldDef) -> Member {
    Member {
        name: field.name.clone(),
        kind: "field".into(),
        signature: format!(
            "{} {} {}: {}",
            visibility(&field.visibility),
            if field.mutable { "var" } else { "const" },
            field.name,
            type_name(&field.field_type)
        ),
        visibility: visibility(&field.visibility).into(),
        is_static: false,
        type_parameters: vec![],
        parameters: vec![],
        return_type: None,
        type_name: Some(type_name(&field.field_type)),
        documentation: field.doc.clone(),
        documentation_truncated: false,
        annotations: annotation_uses(&field.annotations),
        range: Some(field.span),
        generated: false,
    }
}
fn function_record(function: &Function) -> Member {
    Member {
        name: function.name.clone(),
        kind: "method".into(),
        signature: function_signature(function),
        visibility: "public".into(),
        is_static: false,
        type_parameters: function.type_params.clone(),
        parameters: parameter_records(&function.params),
        return_type: Some(type_name(&function.ret_type)),
        type_name: None,
        documentation: function.doc.clone(),
        documentation_truncated: false,
        annotations: annotation_uses(&function.annotations),
        range: Some(function.span),
        generated: false,
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    pub qualified_name: String,
    pub namespace: String,
    pub kind: String,
    pub signature: String,
    pub source: String,
    pub required_import: Option<String>,
    pub package: Option<String>,
    pub type_parameters: Vec<String>,
    pub extends: Vec<String>,
    pub implements: Vec<String>,
    pub documentation: Option<String>,
    pub documentation_truncated: bool,
    pub annotations: Vec<String>,
    pub members: Vec<Member>,
    pub range: Span,
    pub omitted_members: usize,
    pub members_scope: String,
}

/// Keep declaration-level types intact; the backend's erased type markers are
/// deliberately unsuitable as an API schema (e.g. UInt8 and callback types).
pub fn symbols(source: &SourceFile, file: &str, import_prefix: &str) -> Vec<Symbol> {
    fn collect(
        decls: &[Decl],
        namespace: &str,
        file: &str,
        import_prefix: &str,
        out: &mut Vec<Symbol>,
    ) {
        for decl in decls {
            if let DeclKind::Namespace(ns) = &decl.node {
                let name = ns.name.join(".");
                let nested = if namespace.is_empty() {
                    name
                } else {
                    format!("{namespace}.{name}")
                };
                collect(&ns.decls, &nested, file, import_prefix, out);
                continue;
            }
            let (
                name,
                kind,
                signature,
                type_parameters,
                extends,
                implements,
                doc,
                annotations,
                members,
            ) = match &decl.node {
                DeclKind::Class(c) => (
                    c.name.clone(),
                    "class",
                    format!("class {}{}", c.name, self::type_parameters(&c.type_params)),
                    c.type_params.clone(),
                    c.extends.clone().into_iter().collect(),
                    c.implements.clone(),
                    c.doc.clone(),
                    annotation_uses(&c.annotations),
                    c.fields
                        .iter()
                        .map(field_record)
                        .chain(c.methods.iter().map(|m| method_record(m, &c.name)))
                        .collect(),
                ),
                DeclKind::Interface(i) => (
                    i.name.clone(),
                    "interface",
                    format!("interface {}", i.name),
                    vec![],
                    i.extends.clone(),
                    vec![],
                    i.doc.clone(),
                    annotation_uses(&i.annotations),
                    i.methods.iter().map(function_record).collect(),
                ),
                DeclKind::Trait(t) => (
                    t.name.clone(),
                    "trait",
                    format!("trait {}", t.name),
                    vec![],
                    vec![],
                    vec![],
                    t.doc.clone(),
                    annotation_uses(&t.annotations),
                    t.methods.iter().map(function_record).collect(),
                ),
                DeclKind::Function(f) => (
                    f.name.clone(),
                    "function",
                    function_signature(f),
                    f.type_params.clone(),
                    vec![],
                    vec![],
                    f.doc.clone(),
                    annotation_uses(&f.annotations),
                    vec![function_record(f)],
                ),
                DeclKind::Enum(e) => (
                    e.name.clone(),
                    "enum",
                    format!("enum {}", e.name),
                    vec![],
                    vec![],
                    vec![],
                    e.doc.clone(),
                    annotation_uses(&e.annotations),
                    e.variants
                        .iter()
                        .map(|v| Member {
                            name: v.name.clone(),
                            kind: "variant".into(),
                            signature: if v.args.is_empty() {
                                v.name.clone()
                            } else {
                                format!(
                                    "{}({})",
                                    v.name,
                                    v.args.iter().map(type_name).collect::<Vec<_>>().join(", ")
                                )
                            },
                            visibility: "public".into(),
                            is_static: true,
                            type_parameters: vec![],
                            parameters: vec![],
                            return_type: None,
                            type_name: None,
                            documentation: v.doc.clone(),
                            documentation_truncated: false,
                            annotations: vec![],
                            range: Some(v.span),
                            generated: false,
                        })
                        .collect(),
                ),
                DeclKind::Immutable(i) => (
                    i.name.clone(),
                    "immutable",
                    format!("immutable {}({})", i.name, parameters(&i.fields)),
                    vec![],
                    vec![],
                    vec![],
                    i.doc.clone(),
                    annotation_uses(&i.annotations),
                    vec![],
                ),
                _ => continue,
            };
            let mut signature = signature;
            if !extends.is_empty() {
                signature.push_str(&format!(" extends {}", extends.join(", ")));
            }
            if !implements.is_empty() {
                signature.push_str(&format!(" implements {}", implements.join(", ")));
            }
            let qualified_name = if namespace.is_empty() {
                name.clone()
            } else {
                format!("{namespace}.{name}")
            };
            out.push(Symbol {
                name,
                qualified_name,
                namespace: namespace.into(),
                kind: kind.into(),
                signature,
                source: file.into(),
                required_import: if import_prefix.is_empty() {
                    None
                } else {
                    Some(format!("import {import_prefix};"))
                },
                package: None,
                type_parameters,
                extends,
                implements,
                documentation: doc,
                documentation_truncated: false,
                annotations,
                members,
                range: decl.span,
                omitted_members: 0,
                members_scope: "declared".into(),
            });
        }
    }
    let mut out = vec![];
    collect(&source.decls, "", file, import_prefix, &mut out);
    out
}

pub fn bound_documentation(documentation: &mut Option<String>, limit: usize) -> bool {
    if let Some(text) = documentation {
        if text.chars().count() > limit {
            *text = text.chars().take(limit).collect();
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_and_reference_types_roundtrip_through_source_formatting() {
        let source =
            "class Main { fnc accept(cb: fnc(?Int64) -> UInt8, value: ?Int64) -> Nothing {} }";
        let ast = crate::parse_source_cached(source).unwrap();
        let formatted = Formatter::new().format(&ast);
        let reparsed = crate::parse_source_cached(&formatted).unwrap();
        let DeclKind::Class(class) = &reparsed.decls[0].node else {
            panic!("class expected")
        };
        assert_eq!(
            type_name(&class.methods[0].params[0].param_type),
            "fnc(?Int64) -> UInt8"
        );
        assert_eq!(type_name(&class.methods[0].params[1].param_type), "?Int64");
        let symbol = &symbols(&reparsed, "/Main.tnx", "Main")[0];
        assert!(symbol.members[0]
            .signature
            .contains("fnc accept(cb: fnc(?Int64) -> UInt8"));
    }
}

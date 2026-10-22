//! OpenAPI output from the same validated route metadata used for lowering.
use std::collections::BTreeMap;
use serde_yaml::{Mapping, Value};
use tinox_parser::{Class, Decl, DeclKind, Type};
use tinox_typecheck::annotations::{response_body_type, AnnotationProcessingResult, RouteParamKind};

fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Mapping(entries.into_iter().map(|(k, v)| (Value::String(k), v)).collect())
}

fn map(entries: &[(&str, Value)]) -> Value {
    object(entries.iter().map(|(k, v)| (k.to_string(), v.clone())))
}

fn text(s: &str) -> Value { Value::String(s.to_string()) }

fn schema(ty: &Type) -> Value {
    match ty {
        Type::String | Type::Char => map(&[("type", text("string"))]),
        Type::Bool => map(&[("type", text("boolean"))]),
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 => map(&[("type", text("integer"))]),
        Type::Float32 | Type::Float64 => map(&[("type", text("number"))]),
        Type::Named(name) => map(&[("$ref", text(&format!("#/components/schemas/{name}")))]),
        Type::Array(inner) => map(&[("type", text("array")), ("items", schema(inner))]),
        Type::Generic { name, args } if (name == "List" || name == "Array") && args.len() == 1 => {
            map(&[("type", text("array")), ("items", schema(&args[0]))])
        }
        Type::Map(_, value) => map(&[("type", text("object")), ("additionalProperties", schema(value))]),
        Type::Mutable(inner) | Type::Ref(inner) => schema(inner),
        Type::Nullable(inner) => map(&[("anyOf", Value::Sequence(vec![schema(inner), map(&[("type", text("null"))])]))]),
        _ => Value::Mapping(Mapping::new()),
    }
}

fn error_schema() -> Value {
    map(&[
        ("type", text("object")),
        ("properties", map(&[("error", schema(&Type::String))])),
        ("required", Value::Sequence(vec![text("error")])),
    ])
}

/// Some existing serializer shapes produce placeholders. Leave those
/// unconstrained instead of asserting a schema the wire cannot satisfy.
fn field_schema(ty: &Type, classes: &BTreeMap<String, &Class>) -> Value {
    let known_class = |name: &str| classes.get(name)
        .is_some_and(|c| c.annotations.iter().any(|a| a.name == "JsonSerializable"));
    let list = |inner: &Type| match inner {
        Type::String => schema(&Type::Array(Box::new(Type::String))),
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 | Type::Bool
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 => schema(&Type::Array(Box::new(Type::Int64))),
        Type::Named(name) if known_class(name) => schema(&Type::Array(Box::new(inner.clone()))),
        _ => Value::Mapping(Mapping::new()),
    };
    match ty {
        Type::Named(name) if !known_class(name) => Value::Mapping(Mapping::new()),
        Type::Generic { name, args } if (name == "List" || name == "Array") && args.len() == 1 => list(&args[0]),
        Type::Array(inner) => list(inner),
        Type::Map(key, value) if matches!(key.as_ref(), Type::String) && matches!(value.as_ref(), Type::String) => schema(ty),
        Type::Map(_, _) | Type::Generic { .. } | Type::Nullable(_) => Value::Mapping(Mapping::new()),
        Type::Char => schema(&Type::Int32),
        Type::Mutable(inner) | Type::Ref(inner) => field_schema(inner, classes),
        _ => schema(ty),
    }
}

fn response(description: &str, body: Option<Value>) -> Value {
    let mut fields = vec![("description".to_string(), text(description))];
    if let Some(body) = body {
        fields.push(("content".to_string(), map(&[("application/json", map(&[("schema", body)]))])));
    }
    object(fields)
}

fn collect_classes<'a>(decls: &'a [Decl], classes: &mut BTreeMap<String, &'a Class>) {
    for decl in decls {
        match &decl.node {
            DeclKind::Class(c) => { classes.insert(c.name.clone(), c); }
            DeclKind::Namespace(ns) => collect_classes(&ns.decls, classes),
            _ => {}
        }
    }
}

/// Match serializer inheritance and @DoNotSerialize filtering. Guard cycles
/// here as well as in the type checker, so schema generation always terminates.
fn class_properties(class: &Class, classes: &BTreeMap<String, &Class>, seen: &mut Vec<String>) -> BTreeMap<String, Value> {
    if seen.contains(&class.name) { return BTreeMap::new(); }
    seen.push(class.name.clone());
    let mut properties = class.extends.as_ref().and_then(|n| classes.get(n))
        .map(|parent| class_properties(parent, classes, seen)).unwrap_or_default();
    for field in &class.fields {
        if field.name == "log" || field.annotations.iter().any(|a| a.name == "DoNotSerialize") {
            properties.remove(&field.name);
        } else {
            properties.insert(field.name.clone(), field_schema(&field.field_type, classes));
        }
    }
    seen.pop();
    properties
}

/// This describes permitted response alternatives, rather than inferring
/// control flow from constructor calls. Dynamic errors use OpenAPI ranges.
pub fn generate(decls: &[Decl], annotations: &AnnotationProcessingResult) -> Result<String, String> {
    let mut classes = BTreeMap::new();
    collect_classes(decls, &mut classes);
    let schemas = object(classes.values()
        .filter(|c| c.type_params.is_empty() && c.annotations.iter().any(|a| a.name == "JsonSerializable"))
        .map(|c| {
            let properties = class_properties(c, &classes, &mut Vec::new());
            let required = Value::Sequence(properties.keys().map(|k| text(k)).collect());
            (c.name.clone(), map(&[("type", text("object")), ("properties", object(properties)), ("required", required)]))
        }));
    let mut paths: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for route in &annotations.route_entries {
        let typed_body = response_body_type(&route.return_type);
        let manual = matches!(&route.return_type, Type::Named(n) if n == "HttpContext");
        let mut responses = BTreeMap::new();
        if let Some(body) = typed_body {
            responses.insert("2XX".to_string(), response("JSON success (except 204/205)", Some(schema(body))));
            responses.insert("204".to_string(), response("No content", None));
            responses.insert("4XX".to_string(), response("Typed client error", Some(error_schema())));
            responses.insert("5XX".to_string(), response("Typed server error", Some(error_schema())));
        } else if manual {
            responses.insert("default".to_string(), response("Manual HttpContext response; body and status are application-defined", None));
        } else {
            responses.insert(route.status_code.unwrap_or(200).to_string(), response("JSON response", Some(schema(&route.return_type))));
            responses.insert("500".to_string(), response("Handler returned no value", Some(error_schema())));
        }
        let mut parameters = Vec::new();
        for param in &route.params {
            match param.kind {
                RouteParamKind::PathParam | RouteParamKind::QueryParam => {
                    parameters.push(map(&[
                        ("name", text(&param.name)),
                        ("in", text(if param.kind == RouteParamKind::PathParam { "path" } else { "query" })),
                        ("required", Value::Bool(true)),
                        ("schema", schema(&param.ty)),
                    ]));
                    responses.insert("400".to_string(), response("Missing or invalid bound parameter", Some(error_schema())));
                }
                RouteParamKind::PostParam | RouteParamKind::HttpContext => {}
            }
        }
        if route.consumes.is_some() {
            responses.insert("415".to_string(), response("Request Content-Type rejected before the handler", None));
        }
        if route.auth_type.is_some() || !route.oidc_roles.is_empty() {
            // Auth/role guards own their response representation. Do not
            // promise the typed handler's JSON body on these early exits.
            responses.insert("401".to_string(), response("Authentication rejected before the handler", None));
            responses.insert("403".to_string(), response("Authorization rejected before the handler", None));
        }
        let mut operation = vec![
            ("operationId".to_string(), text(&format!("{}_{}", route.class_name, route.method_name))),
            ("parameters".to_string(), Value::Sequence(parameters)),
            ("responses".to_string(), object(responses)),
            ("x-tinox-manual-response".to_string(), Value::Bool(manual)),
        ];
        if typed_body.is_some() {
            operation.push(("x-tinox-default-success-status".to_string(), Value::Number(route.status_code.unwrap_or(200).into())));
        }
        if let Some(param) = route.params.iter().find(|p| p.kind == RouteParamKind::PostParam) {
            operation.push(("requestBody".to_string(), map(&[
                ("required", Value::Bool(true)),
                ("content", object([(route.consumes.clone().unwrap_or_else(|| "application/json".to_string()), map(&[("schema", schema(&param.ty))]))])),
            ])));
        }
        let path = route.path.split('/').map(|segment| segment.strip_prefix(':')
            .map(|name| format!("{{{name}}}")).unwrap_or_else(|| segment.to_string())).collect::<Vec<_>>().join("/");
        let methods = paths.entry(path.clone()).or_default();
        if methods.insert(route.method.to_lowercase(), object(operation)).is_some() {
            return Err(format!("cannot generate API contract: duplicate {} route for {path}", route.method));
        }
    }
    let document = map(&[
        ("openapi", text("3.1.0")),
        ("info", map(&[("title", text("Tinox REST API")), ("version", text("1.0.0"))])),
        ("paths", object(paths.into_iter().map(|(path, methods)| (path, object(methods))))),
        ("components", map(&[("schemas", schemas)])),
    ]);
    serde_yaml::to_string(&document).map_err(|e| format!("cannot serialize API contract: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_and_alternatives_follow_route_metadata() {
        let source = r#"
@annotation class JsonSerializable {}
@JsonSerializable class User { var name: String; @DoNotSerialize var secret: String; }
class Ctrl {
    @GET @Path("/users/:id") @StatusCode(201)
    fn get(@PathParam id: Int64) -> Response<User> {}
    @GET @Path("/raw") fn raw(@HttpContext ctx: HttpContext) -> HttpContext {}
    @POST @Path("/users") @Consumes("application/json")
    fn create(@PostParam user: User) -> Response<User> {}
}"#;
        let tokens = tinox_lexer::Lexer::new(source).tokenize().unwrap();
        let ast = tinox_parser::Parser::new(tokens).parse().unwrap();
        let annotations = tinox_typecheck::annotations::process_annotations(&ast);
        let output = generate(&ast.decls, &annotations).unwrap();
        assert_eq!(output, generate(&ast.decls, &annotations).unwrap());
        let doc: Value = serde_yaml::from_str(&output).unwrap();
        let op = &doc["paths"]["/users/{id}"]["get"];
        assert_eq!(op["x-tinox-default-success-status"], Value::Number(201.into()));
        assert_eq!(op["responses"]["2XX"]["content"]["application/json"]["schema"]["$ref"], text("#/components/schemas/User"));
        assert!(op["responses"]["204"]["content"].is_null());
        assert_eq!(op["responses"]["4XX"]["content"]["application/json"]["schema"]["properties"]["error"]["type"], text("string"));
        assert!(doc["components"]["schemas"]["User"]["properties"]["secret"].is_null());
        assert_eq!(doc["paths"]["/raw"]["get"]["x-tinox-manual-response"], Value::Bool(true));
        assert!(doc["paths"]["/users"]["post"]["responses"]["415"]["content"].is_null());
    }
}

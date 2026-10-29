//! Request validation metadata, shared by lowering and API contracts.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tinox_common::Error;
use tinox_parser::{AnnotationArg, Class, FieldDef, Literal, Type};

pub fn bounds(field: &FieldDef, name: &str) -> Option<(f64, f64)> {
    let annotation = field.annotations.iter().find(|a| a.name == name)?;
    let number = |a: &AnnotationArg| match a {
        AnnotationArg::Literal(Literal::Integer(n)) => Some(*n as f64),
        AnnotationArg::Literal(Literal::Float(n)) => Some(*n),
        _ => None,
    };
    Some((
        number(annotation.args.first()?)?,
        number(annotation.args.get(1)?)?,
    ))
}
pub fn required(field: &FieldDef) -> bool {
    !field.annotations.iter().any(|a| a.name == "Optional")
        && field
            .annotations
            .iter()
            .any(|a| matches!(a.name.as_str(), "Required" | "Length" | "Range"))
}
pub fn validate_field(field: &FieldDef) -> Vec<Error> {
    let mut errors = vec![];
    let has = |n| field.annotations.iter().any(|a| a.name == n);
    if has("Required") && has("Optional") {
        errors.push(Error::new(
            field.span,
            "@Required and @Optional cannot be combined",
        ));
    }
    for name in ["Required", "Optional", "Length", "Range"] {
        if field.annotations.iter().filter(|a| a.name == name).count() > 1 {
            errors.push(Error::new(
                field.span,
                format!("duplicate @{name} constraint"),
            ));
        }
    }
    for name in ["Length", "Range"] {
        if !has(name) {
            continue;
        }
        let valid_type = match name {
            "Length" => {
                matches!(&field.field_type, Type::String | Type::Array(_))
                    || matches!(&field.field_type, Type::Generic {name, ..} if name == "List" || name == "Array")
            }
            _ => matches!(
                field.field_type,
                Type::Int8
                    | Type::Int16
                    | Type::Int32
                    | Type::Int64
                    | Type::UInt8
                    | Type::UInt16
                    | Type::UInt32
                    | Type::UInt64
                    | Type::Float32
                    | Type::Float64
            ),
        };
        if !valid_type {
            errors.push(Error::new(
                field.span,
                format!("@{name} is not supported on this field type"),
            ));
        }
        let exact_ordered = match bound_values(field, name) {
            Some((Value::Number(a), Value::Number(b))) => match (a.as_i64(), b.as_i64()) {
                (Some(a), Some(b)) => a <= b,
                _ => true,
            },
            _ => false,
        };
        match bounds(field, name) {
            Some((min,max)) if exact_ordered && min.is_finite() && max.is_finite() && min <= max && (name != "Length" || (min >= 0.0 && min.fract() == 0.0 && max.fract() == 0.0)) => {},
            _ => errors.push(Error::new(field.span, format!("@{name} requires two finite ordered literal bounds (nonnegative integers for @Length)"))),
        }
    }
    errors
}
pub fn fields<'a>(name: &str, classes: &'a BTreeMap<String, Class>) -> Vec<&'a FieldDef> {
    fn walk<'a>(
        name: &str,
        classes: &'a BTreeMap<String, Class>,
        seen: &mut Vec<String>,
    ) -> Vec<&'a FieldDef> {
        if seen.iter().any(|n| n == name) {
            return vec![];
        }
        seen.push(name.into());
        let Some(c) = classes.get(name) else {
            return vec![];
        };
        let mut result = c
            .extends
            .as_ref()
            .map(|p| walk(p, classes, seen))
            .unwrap_or_default();
        for field in &c.fields {
            result.retain(|f| f.name != field.name);
            result.push(field);
        }
        seen.pop();
        result
    }
    walk(name, classes, &mut vec![])
}
pub fn type_schema(ty: &Type) -> Value {
    match ty {
        Type::String => json!({"type":"string"}),
        Type::Bool => json!({"type":"boolean"}),
        Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::UInt8
        | Type::UInt16
        | Type::UInt32
        | Type::UInt64 => {
            let bits = match ty {
                Type::Int8 | Type::UInt8 => 8,
                Type::Int16 | Type::UInt16 => 16,
                Type::Int32 | Type::UInt32 => 32,
                _ => 64,
            };
            let unsigned = matches!(ty, Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64);
            let min = if unsigned {
                0
            } else if bits == 64 {
                i64::MIN
            } else {
                -(1_i64 << (bits - 1))
            };
            let max = if bits == 64 {
                i64::MAX
            } else if unsigned {
                (1_i64 << bits) - 1
            } else {
                (1_i64 << (bits - 1)) - 1
            };
            json!({"type":"integer","bits":bits,"unsigned":unsigned,"minimum":min,"maximum":max})
        }
        Type::Char => json!({"type":"integer","bits":32,"unsigned":true}),
        Type::Map(key, value)
            if matches!(key.as_ref(), Type::String) && matches!(value.as_ref(), Type::String) =>
        {
            json!({"type":"object","values":required_item(value)})
        }
        Type::Float32 => {
            json!({"type":"number","minimum":-(f32::MAX as f64),"maximum":f32::MAX as f64})
        }
        Type::Float64 => json!({"type":"number"}),
        Type::Named(name) => json!({"class":name}),
        Type::Generic { name, args } if (name == "List" || name == "Array") && args.len() == 1 => {
            list_schema(&args[0])
        }
        Type::Array(inner) => list_schema(inner),
        Type::Nullable(inner) | Type::Ref(inner) | Type::Mutable(inner) => type_schema(inner),
        _ => json!({"type":"unsupported"}),
    }
}
fn list_schema(inner: &Type) -> Value {
    match inner {
        Type::String
        | Type::Int8
        | Type::Int16
        | Type::Int32
        | Type::Int64
        | Type::UInt8
        | Type::UInt16
        | Type::UInt32
        | Type::UInt64
        | Type::Char
        | Type::Named(_) => json!({"type":"array","items":required_item(inner)}),
        _ => json!({"type":"unsupported"}),
    }
}
fn required_item(ty: &Type) -> Value {
    let mut schema = type_schema(ty);
    schema["required"] = json!(true);
    schema
}
pub fn bound_values(field: &FieldDef, name: &str) -> Option<(Value, Value)> {
    let a = field.annotations.iter().find(|a| a.name == name)?;
    let value = |arg: &AnnotationArg| match arg {
        AnnotationArg::Literal(Literal::Integer(n)) => Some(json!(n)),
        AnnotationArg::Literal(Literal::Float(n)) => Some(json!(n)),
        _ => None,
    };
    Some((value(a.args.first()?)?, value(a.args.get(1)?)?))
}
fn number_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    match (left.as_i64(), right.as_i64()) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => left
            .as_f64()
            .unwrap()
            .partial_cmp(&right.as_f64().unwrap())
            .unwrap(),
    }
}
pub fn field_schema(field: &FieldDef) -> Value {
    let mut schema = type_schema(&field.field_type);
    schema["required"] = json!(required(field));
    if let Some((min, max)) = bounds(field, "Length") {
        schema["minLength"] = json!(min as u64);
        schema["maxLength"] = json!(max as u64);
    }
    if let Some((min, max)) = bound_values(field, "Range") {
        if schema
            .get("minimum")
            .is_none_or(|v| number_order(v, &min).is_lt())
        {
            schema["minimum"] = min;
        }
        if schema
            .get("maximum")
            .is_none_or(|v| number_order(v, &max).is_gt())
        {
            schema["maximum"] = max;
        }
    }
    schema
}
pub fn document(classes: &BTreeMap<String, Class>) -> String {
    let mut definitions = serde_json::Map::new();
    for (name, c) in classes {
        if !c.annotations.iter().any(|a| a.name == "JsonSerializable") {
            continue;
        }
        let fs: Vec<_> = fields(name, classes)
            .iter()
            .filter(|f| f.name != "log")
            .map(|f| json!({"name":f.name,"schema":field_schema(f)}))
            .collect();
        definitions.insert(name.clone(), json!(fs));
    }
    Value::Object(definitions).to_string()
}

/// Reject request graphs that the existing generated JSON binder cannot
/// materialize. Validation must never promise a valid shape then bind zero.
pub fn validate_request_graphs(
    decls: &[tinox_parser::Decl],
    extra: &[tinox_parser::Decl],
) -> Vec<Error> {
    use tinox_parser::DeclKind;
    fn collect(decls: &[tinox_parser::Decl], classes: &mut BTreeMap<String, Class>) {
        for d in decls {
            match &d.node {
                DeclKind::Class(c) => {
                    classes.insert(c.name.clone(), c.clone());
                }
                DeclKind::Namespace(n) => collect(&n.decls, classes),
                _ => {}
            }
        }
    }
    fn walk(
        name: &str,
        path: &str,
        classes: &BTreeMap<String, Class>,
        seen: &mut Vec<String>,
        errors: &mut Vec<Error>,
    ) {
        if seen.iter().any(|n| n == name) {
            return;
        }
        seen.push(name.into());
        for f in fields(name, classes) {
            let field_path = format!("{path}.{}", f.name);
            if f.name == "log" {
                continue;
            }
            if type_schema(&f.field_type)["type"] == "unsupported"
                || matches!(f.field_type, Type::Nullable(_))
            {
                errors.push(Error::new(f.span,format!("@PostParam DTO field '{field_path}' has an unsupported JSON binding type; use scalar fields, nested @JsonSerializable objects, List<String>/integer/List<DTO>, or Map<String,String>; use @Optional instead of nullable DTO field types")));
                continue;
            }
            let nested = match &f.field_type {
                Type::Named(n) => Some(n.as_str()),
                Type::Generic { name, args }
                    if (name == "List" || name == "Array") && args.len() == 1 =>
                {
                    match &args[0] {
                        Type::Named(n) => Some(n.as_str()),
                        _ => None,
                    }
                }
                Type::Array(inner) => match inner.as_ref() {
                    Type::Named(n) => Some(n.as_str()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(nested) = nested {
                if classes
                    .get(nested)
                    .is_some_and(|c| c.annotations.iter().any(|a| a.name == "JsonSerializable"))
                {
                    walk(nested, &field_path, classes, seen, errors);
                } else {
                    errors.push(Error::new(f.span,format!("@PostParam DTO field '{field_path}' must reference a @JsonSerializable class")));
                }
            }
        }
        seen.pop();
    }
    let mut classes = BTreeMap::new();
    collect(extra, &mut classes);
    collect(decls, &mut classes);
    let mut errors = vec![];
    for c in classes.values() {
        for m in &c.methods {
            for p in &m.params {
                if p.annotations.iter().any(|a| a.name == "PostParam") {
                    if let Type::Named(name) = &p.param_type {
                        walk(name, name, &classes, &mut vec![], &mut errors);
                    }
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::{DeclKind, Parser};
    fn class(text: &str) -> Class {
        let source = Parser::new(Lexer::new(text).tokenize().unwrap())
            .parse()
            .unwrap();
        let DeclKind::Class(c) = &source.decls[0].node else {
            panic!("class")
        };
        c.clone()
    }
    #[test]
    fn rejects_ambiguous_and_invalid_constraints() {
        for declaration in [
            "@Required @Optional var value: String",
            "@Required @Required var value: String",
            "@Range(1,2) @Range(3,4) var value: Int64",
            "@Length(2,1) var value: String",
            "@Length(1,2) var value: Int64",
            "@Range(1,2) var value: String",
            "@Range(9007199254740993,9007199254740992) var value: Int64",
        ] {
            let c = class(&format!("class Input {{ {declaration} }}"));
            assert!(!validate_field(&c.fields[0]).is_empty(), "{declaration}");
        }
        let c = class("class Input { @Range(-10,10) var value: Int64 }");
        assert!(validate_field(&c.fields[0]).is_empty());
    }
    #[test]
    fn unsupported_request_graphs_fail_before_binding() {
        for ty in [
            "Map<String,Int64>",
            "List<Float64>",
            "List<Bool>",
            "List<List<Int64>>",
            "String?",
            "Ordinary",
        ] {
            let source=format!("@JsonSerializable class Input {{ var unsupported: {ty} }}\nclass Ordinary {{ var name: String }}\nclass Ctrl {{ @POST fn create(@PostParam input: Input) -> String {{ return \"ok\" }} }}");
            let parsed = Parser::new(Lexer::new(&source).tokenize().unwrap())
                .parse()
                .unwrap();
            let errors = validate_request_graphs(&parsed.decls, &[]);
            assert!(!errors.is_empty(), "{ty}");
            assert!(errors[0].message.contains("Input.unsupported"));
        }
    }
    #[test]
    fn exact_integer_ranges_and_nested_numeric_widths() {
        let c = class("class Input { @Range(9007199254740992,9007199254740993) var value: Int64 }");
        let schema = field_schema(&c.fields[0]);
        assert_eq!(schema["minimum"].as_i64(), Some(9007199254740992));
        assert_eq!(schema["maximum"].as_i64(), Some(9007199254740993));
        let c = class("class Input { var values: List<UInt8> }");
        assert_eq!(field_schema(&c.fields[0])["items"]["bits"], 8);
        assert_eq!(field_schema(&c.fields[0])["items"]["unsigned"], true);
        assert_eq!(
            type_schema(&Type::Nullable(Box::new(Type::UInt8)))["maximum"],
            255
        );
    }
    #[test]
    fn inherited_constraints_shadow_and_not_null_stays_database_only() {
        let base = class(
            "class Base { @Required var id: Int64
 @Length(1,3) var name: String }",
        );
        let child = class(
            "class Child extends Base { @Optional @Length(2,4) var name: String
 @NotNull var dbOnly: String }",
        );
        let classes = BTreeMap::from([("Base".into(), base), ("Child".into(), child)]);
        let fields = fields("Child", &classes);
        assert_eq!(
            fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["id", "name", "dbOnly"]
        );
        assert!(required(fields[0]));
        assert!(!required(fields[1]));
        assert_eq!(bounds(fields[1], "Length"), Some((2.0, 4.0)));
        assert!(!required(fields[2]));
    }
}

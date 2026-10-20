//! Explicit semantic facts carried across the cached frontend/lowering boundary.
use crate::{TypeChecker, ValueType};
use std::collections::HashMap;
use tinox_parser::{Expr, ExprKind, Type};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedCall {
    /// The same unambiguous class/member symbol key used by emission.
    pub symbol: Option<String>,
    pub owner: Option<String>,
    pub dispatch: CallDispatch,
    pub params: Vec<ValueType>,
    pub result: ValueType,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CallDispatch {
    Direct,
    Instance,
    Interface,
    Closure,
    Dynamic,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedField {
    pub owner: ValueType,
    pub name: String,
    pub result: ValueType,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Conversion {
    pub source: ValueType,
    pub target: ValueType,
}
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SemanticFacts {
    pub calls: HashMap<u32, ResolvedCall>,
    pub fields: HashMap<u32, ResolvedField>,
    pub conversions: HashMap<u32, Conversion>,
}
impl ValueType {
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Int | Self::Integer { .. })
    }
    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float | Self::Float32)
    }
    pub fn is_unsigned(&self) -> bool {
        matches!(self, Self::Integer { signed: false, .. })
    }
    pub fn dispatch_name(&self) -> String {
        if self.is_integer() {
            "Int64".into()
        } else if self.is_float() {
            "Float64".into()
        } else {
            self.to_string()
        }
    }
    /// Recover the complete declared type, never inventing Int64 for unknowns.
    pub fn to_parser_type(&self) -> Option<Type> {
        Some(match self {
            Self::Int => Type::Int64,
            Self::Integer { bits, signed } => match (bits, signed) {
                (8, true) => Type::Int8,
                (16, true) => Type::Int16,
                (32, true) => Type::Int32,
                (64, true) => Type::Int64,
                (8, false) => Type::UInt8,
                (16, false) => Type::UInt16,
                (32, false) => Type::UInt32,
                (64, false) => Type::UInt64,
                _ => return None,
            },
            Self::Float => Type::Float64,
            Self::Float32 => Type::Float32,
            Self::Bool => Type::Bool,
            Self::Char => Type::Char,
            Self::String => Type::String,
            Self::Nothing => Type::Nothing,
            Self::Never => Type::Never,
            Self::Any => Type::Any,
            Self::Named(name, args) if args.is_empty() => Type::Named(name.clone()),
            Self::Named(name, args) => Type::Generic {
                name: name.clone(),
                args: args
                    .iter()
                    .map(Self::to_parser_type)
                    .collect::<Option<_>>()?,
            },
            Self::Array(inner) => Type::Array(Box::new(inner.to_parser_type()?)),
            Self::Map(value) => {
                Type::Map(Box::new(Type::String), Box::new(value.to_parser_type()?))
            }
            Self::Nullable(inner) => Type::Nullable(Box::new(inner.to_parser_type()?)),
            Self::Reference { inner, mutable } => {
                if *mutable {
                    Type::Mutable(Box::new(inner.to_parser_type()?))
                } else {
                    Type::Ref(Box::new(inner.to_parser_type()?))
                }
            }
            Self::TupleOf(items) => Type::Tuple(
                items
                    .iter()
                    .map(Self::to_parser_type)
                    .collect::<Option<_>>()?,
            ),
            Self::Callable { params, ret } => Type::Fn {
                params: params
                    .iter()
                    .map(Self::to_parser_type)
                    .collect::<Option<_>>()?,
                ret: Box::new(ret.to_parser_type()?),
            },
            // Legacy builtin categories and runtime handles have no parser spelling.
            Self::Task(_) | Self::Ref | Self::Fn | Self::Tuple | Self::Range | Self::Null => {
                return None
            }
        })
    }
}
impl TypeChecker {
    pub fn semantic_facts(&self) -> SemanticFacts {
        self.semantic_facts.clone()
    }
    // Function ABI and generic identity require exact scalar types: implicit
    // numeric conversions at ordinary value boundaries do not adapt closures
    // or mutate the element type of an existing generic instance.
    pub(crate) fn semantic_compatible(&self, a: &ValueType, b: &ValueType) -> bool {
        if a == b || *a == ValueType::Any || *b == ValueType::Any {
            return true;
        }
        if matches!(a, ValueType::Named(n, args) if args.is_empty() && self.type_param_scope.contains(n))
            || matches!(b, ValueType::Named(n, args) if args.is_empty() && self.type_param_scope.contains(n))
        {
            return true;
        }
        match (a, b) {
            (ValueType::Named(a, aa), ValueType::Named(b, ba)) => {
                a == b
                    && aa.len() == ba.len()
                    && aa
                        .iter()
                        .zip(ba)
                        .all(|(a, b)| self.semantic_compatible(a, b))
            }
            (ValueType::Array(a), ValueType::Array(b))
            | (ValueType::Map(a), ValueType::Map(b))
            | (ValueType::Nullable(a), ValueType::Nullable(b)) => self.semantic_compatible(a, b),
            (
                ValueType::Callable { params: a, ret: ar },
                ValueType::Callable { params: b, ret: br },
            ) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|(a, b)| self.semantic_compatible(a, b))
                    && self.semantic_compatible(ar, br)
            }
            _ => false,
        }
    }
    pub(crate) fn record_semantics(&mut self, expr: &Expr, result: &ValueType) {
        let node_type = |expr: &Expr| {
            self.expr_types
                .get(&expr.id)
                .cloned()
                .unwrap_or(ValueType::Any)
        };
        let mut owner = None;
        let mut dispatch = CallDispatch::Direct;
        let mut callable = None;
        let symbol = match &expr.node {
            ExprKind::Call { func, .. } => match &func.node {
                ExprKind::Ident(name) if self.symbols.variables.contains_key(name) => {
                    dispatch = CallDispatch::Closure;
                    callable = self.symbols.variables.get(name).map(|(t, _)| t.clone());
                    None
                }
                ExprKind::Ident(name) if self.symbols.functions.contains_key(name) => {
                    Some(name.clone())
                }
                ExprKind::Ident(name) => self.current_class.as_ref().map(|class| {
                    owner = Some(class.clone());
                    format!("{class}_{name}")
                }),
                _ => {
                    dispatch = CallDispatch::Closure;
                    callable = Some(node_type(func));
                    None
                }
            },
            ExprKind::EnumValue {
                enum_name, variant, ..
            } if self
                .symbols
                .functions
                .contains_key(&format!("{enum_name}_{variant}")) =>
            {
                owner = Some(enum_name.clone());
                Some(format!("{enum_name}_{variant}"))
            }
            ExprKind::MethodCall { obj, method, .. } => {
                let ty = node_type(obj);
                if let ExprKind::Ident(class) = &obj.node {
                    if self.known_class_names.contains(class)
                        && !self.symbols.variables.contains_key(class)
                    {
                        owner = Some(class.clone());
                    }
                }
                if owner.is_none() {
                    owner = Some(ty.dispatch_name());
                    dispatch = if self.interfaces.contains_key(owner.as_ref().unwrap()) {
                        CallDispatch::Interface
                    } else {
                        CallDispatch::Instance
                    };
                }
                let key = format!("{}.{}", owner.as_ref().unwrap(), method);
                if let Some((field, _)) = self.symbols.variables.get(&key) {
                    if matches!(field, ValueType::Callable { .. } | ValueType::Fn) {
                        callable = Some(if let ValueType::Named(class, args) = &ty {
                            self.substitute_type_params(field, class, args)
                        } else {
                            field.clone()
                        });
                        dispatch = CallDispatch::Closure;
                    }
                }
                if ty == ValueType::Any
                    && callable.is_none()
                    && !matches!(dispatch, CallDispatch::Direct)
                {
                    dispatch = CallDispatch::Dynamic;
                    None
                } else if callable.is_some() {
                    None
                } else {
                    Some(format!("{}_{}", owner.as_ref().unwrap(), method))
                }
            }
            ExprKind::FieldAccess { obj, field } => {
                self.semantic_facts.fields.insert(
                    expr.id,
                    ResolvedField {
                        owner: node_type(obj),
                        name: field.clone(),
                        result: result.clone(),
                    },
                );
                return;
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.semantic_facts.conversions.insert(
                    expr.id,
                    Conversion {
                        source: node_type(inner),
                        target: result.clone(),
                    },
                );
                return;
            }
            _ => return,
        };
        let params = if let Some(ValueType::Callable { params, .. }) = callable {
            params
        } else {
            symbol
                .as_ref()
                .and_then(|key| self.symbols.functions.get(key))
                .map(|sig| sig.params.iter().map(|(_, t)| t.clone()).collect())
                .unwrap_or_default()
        };
        self.semantic_facts.calls.insert(
            expr.id,
            ResolvedCall {
                symbol,
                owner,
                dispatch,
                params,
                result: result.clone(),
            },
        );
    }
}

impl TypeChecker {
    pub(crate) fn check_callable(
        &mut self,
        ty: &ValueType,
        args: &[Expr],
        span: tinox_common::Span,
    ) -> Option<ValueType> {
        let ValueType::Callable { params, ret } = ty else {
            if *ty == ValueType::Fn {
                for arg in args {
                    self.infer_type(arg);
                }
                return Some(ValueType::Any);
            }
            return None;
        };
        if params.len() != args.len() {
            self.errors.push(
                crate::TypeError::InvalidArgumentCount {
                    expected: params.len(),
                    found: args.len(),
                    span,
                }
                .to_error(),
            );
        }
        for (index, arg) in args.iter().enumerate() {
            let found = if let Some(expected) = params.get(index) {
                self.infer_argument(arg, expected)
            } else {
                self.infer_type(arg)
            };
            if let Some(expected) = params.get(index) {
                if !self.types_compatible(expected, &found) {
                    self.errors.push(
                        crate::TypeError::TypeMismatch {
                            expected: expected.display(),
                            found: found.display(),
                            span: arg.span,
                        }
                        .to_error(),
                    );
                }
            }
        }
        Some((**ret).clone())
    }
}
impl TypeChecker {
    pub(crate) fn infer_argument(&mut self, expr: &Expr, expected: &ValueType) -> ValueType {
        if let ValueType::Callable { params, .. } = expected {
            if matches!(expr.node, ExprKind::Lambda { .. }) {
                let previous = self.expr_types.get(&expr.id).cloned();
                let hints = if let Some(ValueType::Callable {
                    params: previous, ..
                }) = &previous
                {
                    params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            if *p == ValueType::Any {
                                previous.get(i).unwrap_or(p).clone()
                            } else {
                                p.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    params.clone()
                };
                self.infer_lambda_with_param_hints(expr, &hints);
                if let (ValueType::Callable { ret, .. }, ExprKind::Lambda { ret_type: None, .. }) =
                    (expected, &expr.node)
                {
                    if **ret == ValueType::Nothing {
                        if let Some(ValueType::Callable { ret, .. }) =
                            self.expr_types.get_mut(&expr.id)
                        {
                            **ret = ValueType::Nothing;
                        }
                    }
                }
            }
        }
        let result = self.infer_type(expr);
        // A factory without value arguments can bind its class parameters from
        // the receiving annotation (Stack<Int64> = Stack::new()).
        if let (ValueType::Named(expected_class, expected_args), ValueType::Named(class, _)) =
            (expected, &result)
        {
            if expected_class == class && !expected_args.is_empty() {
                if let Some(params) = self.class_type_params.get(class) {
                    if Self::contains_type_param(&result, params) {
                        let bindings = params
                            .iter()
                            .cloned()
                            .zip(expected_args.iter().cloned())
                            .collect();
                        let resolved = Self::substitute_bindings(&result, &bindings);
                        if expr.id != 0 {
                            self.expr_types.insert(expr.id, resolved.clone());
                            if let Some(call) = self.semantic_facts.calls.get_mut(&expr.id) {
                                call.result = resolved.clone();
                            }
                            if let Some(field) = self.semantic_facts.fields.get_mut(&expr.id) {
                                field.result = resolved.clone();
                            }
                        }
                        return resolved;
                    }
                }
            }
        }
        result
    }
}
impl TypeChecker {
    pub(crate) fn lambda_result_type(&mut self, body: &Expr) -> Option<ValueType> {
        fn visit(checker: &mut TypeChecker, stmt: &tinox_parser::Stmt) -> Option<ValueType> {
            use tinox_parser::StmtKind;
            match &stmt.node {
                StmtKind::Return(Some(value)) => Some(checker.infer_type(value)),
                StmtKind::Block(stmts) => stmts
                    .iter()
                    .filter_map(|s| visit(checker, s))
                    .reduce(|a, b| TypeChecker::lub(&a, &b)),
                StmtKind::If {
                    then_branch,
                    else_branch,
                    ..
                } => visit(checker, then_branch)
                    .or_else(|| else_branch.as_ref().and_then(|s| visit(checker, s))),
                _ => None,
            }
        }
        if let ExprKind::Block(stmts) = &body.node {
            stmts
                .iter()
                .filter_map(|s| visit(self, s))
                .reduce(|a, b| TypeChecker::lub(&a, &b))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(source: &str) -> TypeChecker {
        let tokens = tinox_lexer::Lexer::new(source).tokenize().unwrap();
        let mut source = tinox_parser::Parser::new(tokens).parse().unwrap();
        tinox_parser::assign_node_ids(&mut source);
        let mut checker = TypeChecker::new();
        checker.check(&source).unwrap();
        checker
    }
    #[test]
    fn scalar_identity_does_not_forbid_numeric_value_comparisons() {
        check("fn compare(a: Int32, b: UInt8) -> Bool { return a == 0 && b != 0; }");
    }
    #[test]
    fn factory_results_bind_class_arguments_before_field_resolution() {
        let checker = check("class Box<T> { var value: T; fn make(v: T) -> Box<T> { return Box { value: v }; } } class Main { fnc read() -> String { return Box::make(\"ok\").value; } }");
        assert!(checker
            .semantic_facts()
            .fields
            .values()
            .any(
                |field| field.owner == ValueType::Named("Box".into(), vec![ValueType::String])
                    && field.result == ValueType::String
            ));
    }
    #[test]
    fn callable_field_contract_substitutes_the_receiver_arguments() {
        let checker = check("class Holder<T> { var factory: fnc(T) -> T; } class Main { fnc invoke(holder: Holder<String>) -> String { return holder.factory(\"ok\"); } }");
        let facts = checker.semantic_facts();
        let call = facts
            .calls
            .values()
            .find(|call| call.dispatch == CallDispatch::Closure)
            .unwrap();
        assert_eq!(call.params, vec![ValueType::String]);
        assert_eq!(call.result, ValueType::String);
    }
    #[test]
    fn receiving_annotation_binds_a_factory_without_value_arguments() {
        let checker = check("class Box<T> { var values: List<T>; fnc make() -> Box<T> { return Box { values: [] }; } } class Main { fnc use() -> Nothing { const box: Box<String> = Box::make(); } }");
        assert!(checker
            .semantic_facts()
            .calls
            .values()
            .any(|call| call.result == ValueType::Named("Box".into(), vec![ValueType::String])));
    }
    #[test]
    fn exact_declared_types_roundtrip_without_width_signature_or_generic_loss() {
        let scalars = [
            Type::Int8,
            Type::Int16,
            Type::Int32,
            Type::Int64,
            Type::UInt8,
            Type::UInt16,
            Type::UInt32,
            Type::UInt64,
            Type::Float32,
            Type::Float64,
            Type::Bool,
            Type::Char,
            Type::String,
            Type::Nothing,
            Type::Never,
            Type::Any,
        ];
        for scalar in scalars {
            let nested = Type::Generic {
                name: "Box".into(),
                args: vec![Type::Fn {
                    params: vec![scalar.clone()],
                    ret: Box::new(Type::Tuple(vec![
                        Type::Ref(Box::new(scalar.clone())),
                        Type::Nullable(Box::new(scalar.clone())),
                    ])),
                }],
            };
            assert_eq!(
                format!(
                    "{:?}",
                    ValueType::from_parser_type(&scalar)
                        .to_parser_type()
                        .unwrap()
                ),
                format!("{scalar:?}")
            );
            assert_eq!(
                format!(
                    "{:?}",
                    ValueType::from_parser_type(&nested)
                        .to_parser_type()
                        .unwrap()
                ),
                format!("{nested:?}")
            );
        }
        assert_ne!(
            ValueType::from_parser_type(&Type::UInt8),
            ValueType::from_parser_type(&Type::Int8)
        );
        assert_ne!(
            ValueType::Named("Box".into(), vec![ValueType::String]),
            ValueType::Named("Box".into(), vec![ValueType::Int])
        );
    }
    #[test]
    fn checked_calls_fields_and_conversions_have_explicit_semantic_contracts() {
        let checker = check("interface Reader { fn read() -> Float64; } class Packet { var byte: UInt8; } class Main { fnc invoke(callback: fnc(UInt8) -> Float32, packet: Packet, reader: Reader) -> Float64 { callback(packet.byte); reader.read(); return packet.byte as Float64; } }");
        let facts = checker.semantic_facts();
        let callback = facts
            .calls
            .values()
            .find(|c| c.dispatch == CallDispatch::Closure)
            .unwrap();
        assert_eq!(
            callback.params,
            vec![ValueType::Integer {
                bits: 8,
                signed: false
            }]
        );
        assert_eq!(callback.result, ValueType::Float32);
        let interface = facts
            .calls
            .values()
            .find(|c| c.dispatch == CallDispatch::Interface)
            .unwrap();
        assert_eq!(interface.owner.as_deref(), Some("Reader"));
        assert_eq!(interface.symbol.as_deref(), Some("Reader_read"));
        assert_eq!(interface.result, ValueType::Float);
        let conversion = facts.conversions.values().next().unwrap();
        assert_eq!(
            conversion.source,
            ValueType::Integer {
                bits: 8,
                signed: false
            }
        );
        assert_eq!(conversion.target, ValueType::Float);
        assert!(facts.fields.values().all(|field| field.result
            == ValueType::Integer {
                bits: 8,
                signed: false
            }));
        for (node, call) in facts.calls {
            assert_eq!(checker.expr_value_types().get(&node), Some(&call.result));
        }
    }
    #[test]
    fn nested_generic_substitution_and_lambda_returns_keep_concrete_identity() {
        let checker = check("class Box<T> { var value: T; fnc make(value: T) -> Box<T> { return Box { value: value }; } } class Main { fnc convert(f: fnc(Int64) -> Box<String>) -> Box<String> { return f(1); } fnc use() -> Nothing { const result = Main::convert(x => { return Box::make(\"ok\"); }); } }");
        assert!(checker
            .semantic_facts()
            .calls
            .values()
            .any(|call| call.dispatch == CallDispatch::Closure
                && call.result == ValueType::Named("Box".into(), vec![ValueType::String])));
    }
}

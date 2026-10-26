//! Explicit semantic facts carried across the cached frontend/lowering boundary.
use crate::{TypeChecker, ValueType};
use std::collections::HashMap;
use tinox_parser::{Expr, ExprKind, Type};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedCall {
    /// The same unambiguous class/member symbol key used by emission.
    pub symbol: Option<String>,
    pub declaration: Option<DeclarationId>,
    pub bindings: HashMap<String, ValueType>,
    pub type_params: Vec<String>,
    pub scope_params: Vec<String>,
    pub span: tinox_common::Span,
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
    pub declarations: HashMap<String, DeclarationId>,
    pub ambiguous_declarations: HashMap<String, Vec<DeclarationId>>,
    pub fields: HashMap<u32, ResolvedField>,
    pub field_types: HashMap<String, HashMap<String, ValueType>>,
    pub conversions: HashMap<u32, Conversion>,
    pub numeric_operations: HashMap<u32, NumericOperation>,
    pub enum_payloads: HashMap<String, Vec<ValueType>>,
}
/// Stable source identity is distinct from the backend's linkage spelling.
/// Namespace plus source identity disambiguates declarations from dependencies.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeclarationId {
    pub namespace: Vec<String>,
    pub file: String,
    pub owner: Option<String>,
    pub name: String,
}

/// Checked promotion shared by arithmetic and comparisons. Both operands are
/// converted as values before emission; an LLVM word is never a type decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NumericOperation {
    pub left: ValueType,
    pub right: ValueType,
    pub operand: ValueType,
}

impl ValueType {
    pub fn with_bindings(&self, bindings: &HashMap<String, ValueType>) -> Self {
        TypeChecker::substitute_bindings(self, bindings)
    }

    pub fn numeric_promotion(left: &Self, right: &Self) -> Option<Self> {
        if !(left.is_integer() || left.is_float()) || !(right.is_integer() || right.is_float()) {
            return None;
        }
        Some(if left == right {
            left.clone()
        } else if left.is_float() || right.is_float() {
            Self::Float
        } else if matches!(left, Self::Integer { bits: 64, signed: false })
            || matches!(right, Self::Integer { bits: 64, signed: false })
        {
            // A 64-bit unsigned operand wins (as in C): comparisons, division
            // and remainder must be unsigned or large values turn negative.
            Self::Integer { bits: 64, signed: false }
        } else {
            Self::Int
        })
    }

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
            Self::Task(result) => Type::Generic {
                name: "Task".into(),
                args: vec![result.to_parser_type()?],
            },
            // Legacy builtin categories have no parser spelling.
            Self::Ref | Self::Fn | Self::Tuple | Self::Range | Self::Null => return None,
        })
    }
}
impl TypeChecker {
    pub(crate) fn register_semantic_declarations(&mut self, source: &tinox_parser::SourceFile) {
        fn visit(
            decls: &[tinox_parser::Decl],
            namespace: &[String],
            out: &mut Vec<(String, DeclarationId)>,
        ) {
            use tinox_parser::DeclKind;
            for decl in decls {
                match &decl.node {
                    DeclKind::Namespace(ns) => {
                        let mut nested = namespace.to_vec();
                        nested.extend(ns.name.iter().cloned());
                        visit(&ns.decls, &nested, out);
                    }
                    DeclKind::Function(f) => {
                        let native = matches!(f.body.node, tinox_parser::StmtKind::Empty);
                        out.push((
                            f.name.clone(),
                            DeclarationId {
                                namespace: if native { vec![] } else { namespace.to_vec() },
                                file: if native {
                                    "<native>".into()
                                } else {
                                    f.file.to_string()
                                },
                                owner: None,
                                name: f.name.clone(),
                            },
                        ));
                    }
                    DeclKind::Class(c) => {
                        out.push((
                            format!("type:{}", c.name),
                            DeclarationId {
                                namespace: namespace.to_vec(),
                                file: decl
                                    .file
                                    .as_deref()
                                    .unwrap_or(tinox_parser::UNKNOWN_FILE)
                                    .into(),
                                owner: None,
                                name: c.name.clone(),
                            },
                        ));
                        for method in &c.methods {
                            out.push((
                                format!("{}_{}", c.name, method.name),
                                DeclarationId {
                                    namespace: namespace.to_vec(),
                                    file: method.file.to_string(),
                                    owner: Some(c.name.clone()),
                                    name: method.name.clone(),
                                },
                            ));
                        }
                    }
                    DeclKind::Interface(interface) => {
                        for method in &interface.methods {
                            out.push((
                                format!("{}_{}", interface.name, method.name),
                                DeclarationId {
                                    namespace: namespace.to_vec(),
                                    file: decl
                                        .file
                                        .as_deref()
                                        .unwrap_or(tinox_parser::UNKNOWN_FILE)
                                        .into(),
                                    owner: Some(interface.name.clone()),
                                    name: method.name.clone(),
                                },
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut declarations = Vec::new();
        visit(&source.decls, &[], &mut declarations);
        for decl in Self::flatten_decls(source) {
            if let tinox_parser::DeclKind::Class(class) = &decl.node {
                self.semantic_facts.field_types.insert(
                    class.name.clone(),
                    class
                        .fields
                        .iter()
                        .map(|field| {
                            (
                                field.name.clone(),
                                ValueType::from_parser_type(&field.field_type),
                            )
                        })
                        .collect(),
                );
            }
        }
        for (symbol, identity) in declarations {
            if let Some(previous) = self.semantic_facts.declarations.get(&symbol) {
                if previous != &identity {
                    self.semantic_facts
                        .ambiguous_declarations
                        .entry(symbol.clone())
                        .or_insert_with(|| vec![previous.clone()])
                        .push(identity);
                    continue;
                }
            }
            self.semantic_facts.declarations.insert(symbol, identity);
        }
    }

    pub(crate) fn validate_semantic_contracts(&mut self) {
        let mut calls: Vec<_> = self.semantic_facts.calls.values().collect();
        calls.sort_by_key(|call| (call.span.start.offset, call.symbol.as_deref()));
        for call in calls {
            for param in &call.type_params {
                if !call.bindings.contains_key(param) && !call.scope_params.contains(param) {
                    self.errors.push(tinox_common::Error::new(call.span, format!("Cannot resolve generic type parameter '{param}' for {}; provide a concrete type argument or receiving annotation", call.symbol.as_deref().unwrap_or("call"))));
                }
            }
        }
    }

    pub fn semantic_facts(&self) -> SemanticFacts {
        let mut facts = self.semantic_facts.clone();
        facts.enum_payloads = self.enum_variant_payloads.clone();
        facts
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
            | (ValueType::Nullable(a), ValueType::Nullable(b))
            | (ValueType::Task(a), ValueType::Task(b)) => self.semantic_compatible(a, b),
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
        if let ExprKind::Binary { lhs, rhs, .. } = &expr.node {
            let left = node_type(lhs);
            let right = node_type(rhs);
            if let Some(operand) = ValueType::numeric_promotion(&left, &right) {
                self.semantic_facts.numeric_operations.insert(
                    expr.id,
                    NumericOperation {
                        left,
                        right,
                        operand,
                    },
                );
            }
            return;
        }
        let mut owner = None;
        let mut dispatch = CallDispatch::Direct;
        let mut callable = None;
        let mut symbol = match &expr.node {
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
        if let (Some(class), ExprKind::MethodCall { method, .. }) = (&owner, &expr.node) {
            let mut declaring = class.clone();
            let mut visited = std::collections::HashSet::new();
            while visited.insert(declaring.clone())
                && !self
                    .semantic_facts
                    .declarations
                    .contains_key(&format!("{declaring}_{method}"))
            {
                let Some(parent) = self.class_parents.get(&declaring) else {
                    break;
                };
                declaring = parent.clone();
            }
            if self
                .semantic_facts
                .declarations
                .contains_key(&format!("{declaring}_{method}"))
            {
                symbol = Some(format!("{declaring}_{method}"));
            }
        }
        let mut bindings = HashMap::new();
        let signature = symbol
            .as_ref()
            .and_then(|key| self.symbols.functions.get(key));
        let mut params = if let Some(ValueType::Callable { params, .. }) = callable {
            params
        } else {
            symbol
                .as_ref()
                .and_then(|key| self.generic_method_param_types.get(key))
                .map(|(params, _)| params.clone())
                .or_else(|| {
                    signature.map(|sig| sig.params.iter().map(|(_, t)| t.clone()).collect())
                })
                .unwrap_or_default()
        };
        let args = match &expr.node {
            ExprKind::Call { args, .. }
            | ExprKind::MethodCall { args, .. }
            | ExprKind::EnumValue { args, .. } => args.as_slice(),
            _ => &[],
        };
        let has_self =
            signature.is_some_and(|sig| sig.params.first().is_some_and(|(name, _)| name == "self"));
        let builtin_instance = matches!(dispatch, CallDispatch::Instance)
            && symbol
                .as_ref()
                .is_some_and(|key| !self.semantic_facts.declarations.contains_key(key));
        if !params.is_empty()
            && ((has_self && params.len() == args.len() + 1)
                || builtin_instance
                || (matches!(dispatch, CallDispatch::Instance | CallDispatch::Interface)
                    && params.len() == args.len() + 1))
        {
            params.remove(0);
        }
        if builtin_instance {
            params.resize(args.len(), ValueType::Any);
        }
        if let ExprKind::MethodCall { obj, .. } = &expr.node {
            if let ValueType::Named(class, args) = node_type(obj) {
                if let Some(names) = self.class_type_params.get(&class) {
                    bindings.extend(names.iter().cloned().zip(args));
                }
            }
        }
        let type_params = symbol
            .as_ref()
            .and_then(|key| self.generic_method_param_types.get(key))
            .map(|(_, names)| names.clone())
            .unwrap_or_default();
        if let ExprKind::EnumValue { type_args, .. } = &expr.node {
            bindings.extend(
                type_params
                    .iter()
                    .cloned()
                    .zip(type_args.iter().map(ValueType::from_parser_type)),
            );
        }
        if !type_params.is_empty() {
            let names = &type_params;
            for (formal, actual) in params.iter().zip(args) {
                Self::unify_param(formal, &node_type(actual), names, &mut bindings);
            }
            if let Some(signature) = signature {
                Self::unify_param(&signature.return_type, result, names, &mut bindings);
            }
        }
        params = params
            .iter()
            .map(|param| Self::substitute_bindings(param, &bindings))
            .collect();
        let actual_types: Vec<_> = args.iter().map(node_type).collect();
        for ((expected, actual), found) in params.iter().zip(args).zip(actual_types) {
            let found = if !type_params.is_empty() {
                self.infer_argument(actual, expected)
            } else {
                found
            };
            if !type_params.is_empty() && !self.types_compatible(expected, &found) {
                self.errors.push(
                    crate::TypeError::TypeMismatch {
                        expected: expected.display(),
                        found: found.display(),
                        span: actual.span,
                    }
                    .to_error(),
                );
            }
        }
        let declaration = symbol
            .as_ref()
            .and_then(|symbol| self.semantic_facts.declarations.get(symbol))
            .cloned();
        self.semantic_facts.calls.insert(
            expr.id,
            ResolvedCall {
                declaration,
                bindings,
                type_params,
                scope_params: self.type_param_scope.iter().cloned().collect(),
                span: expr.span,
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
        let contextual_literal = match (&expr.node, expected) {
            (ExprKind::ArrayLiteral(items), ValueType::Array(element))
                if **element != ValueType::Any =>
            {
                items.iter().all(|item| {
                    let found = self.infer_argument(item, element);
                    self.types_compatible(element, &found)
                })
            }
            (ExprKind::MapLiteral(entries), ValueType::Map(value)) if **value != ValueType::Any => {
                entries.iter().all(|(_, item)| {
                    let found = self.infer_argument(item, value);
                    self.types_compatible(value, &found)
                })
            }
            (ExprKind::Tuple(items), ValueType::TupleOf(types)) if items.len() == types.len() => {
                items.iter().zip(types).all(|(item, expected)| {
                    let found = self.infer_argument(item, expected);
                    self.types_compatible(expected, &found)
                })
            }
            _ => false,
        };
        if contextual_literal {
            self.expr_types.insert(expr.id, expected.clone());
            return expected.clone();
        }
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
                                call.bindings.extend(bindings.clone());
                                call.params = call
                                    .params
                                    .iter()
                                    .map(|ty| Self::substitute_bindings(ty, &bindings))
                                    .collect();
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
    fn task_annotation_identity_roundtrips_through_nested_generic_bindings() {
        let task = ValueType::Task(Box::new(ValueType::Named("T".into(), vec![])));
        let bindings = [(
            "T".into(),
            ValueType::Integer {
                bits: 8,
                signed: false,
            },
        )]
        .into_iter()
        .collect();
        let concrete = task.with_bindings(&bindings);
        assert_eq!(
            ValueType::from_parser_type(&concrete.to_parser_type().unwrap()),
            concrete
        );
        let nested = ValueType::Named("Box".into(), vec![concrete.clone()]);
        assert_eq!(
            ValueType::from_parser_type(&nested.to_parser_type().unwrap()),
            nested
        );
    }

    #[test]
    fn qualified_type_collisions_are_preserved_and_shared_native_linkage_is_allowed() {
        let tokens = tinox_lexer::Lexer::new("namespace first { class Value { var n: Int64 } } namespace second { class Value { var n: String } }").tokenize().unwrap();
        let mut source = tinox_parser::Parser::new(tokens).parse().unwrap();
        tinox_parser::assign_node_ids(&mut source);
        let mut checker = TypeChecker::new();
        checker.check(&source).unwrap();
        assert_eq!(
            checker.semantic_facts().ambiguous_declarations["type:Value"].len(),
            2
        );
        check("namespace first { extern fn nativeShared(value: Int64) -> Int64 } namespace second { extern fn nativeShared(value: Int64) -> Int64 }");
    }

    #[test]
    fn generic_calls_export_concrete_bindings_and_full_returns() {
        let checker = check("class Cell<T> { var value: T\nfnc of(value: T) -> Cell<T> { return Cell { value: value } } fn get() -> T { return this.value } } class Main { fnc use() -> String { return Cell::of(Cell::of(\"nested\")).get().get() } }");
        let facts = checker.semantic_facts();
        assert!(facts
            .calls
            .values()
            .any(|call| call.symbol.as_deref() == Some("Cell_get")
                && call.result == ValueType::String));
        assert!(facts.calls.values().any(|call| call.bindings.get("T")
            == Some(&ValueType::Named("Cell".into(), vec![ValueType::String]))));
    }

    #[test]
    fn qualified_declaration_identity_survives_the_checked_boundary() {
        let checker = check("namespace demo.library { class Reader { fnc read() -> Int32 { return 7 } } } class Main { fnc use() -> Int32 { return Reader::read() } }");
        let facts = checker.semantic_facts();
        let call = facts
            .calls
            .values()
            .find(|call| call.symbol.as_deref() == Some("Reader_read"))
            .unwrap();
        let identity = call.declaration.as_ref().unwrap();
        assert_eq!(identity.namespace, ["demo", "library"]);
        assert_eq!(identity.owner.as_deref(), Some("Reader"));
        assert_eq!(facts.declarations.get("Reader_read"), Some(identity));
    }

    #[test]
    fn numeric_operations_export_promotions_without_changing_sources() {
        let checker = check("fn compare(byte: UInt8, small: Float32) -> Float64 { const larger = byte > (2 as UInt8)\nreturn byte + small }");
        let facts = checker.semantic_facts();
        assert!(facts
            .numeric_operations
            .values()
            .any(|plan| plan.operand == ValueType::Float
                && plan.left.is_unsigned()
                && plan.right == ValueType::Float32));
        assert!(facts
            .numeric_operations
            .values()
            .any(|plan| plan.operand.is_unsigned()));
    }

    #[test]
    fn scalar_identity_does_not_forbid_numeric_value_comparisons() {
        check("fn compare(a: Int32, b: UInt8) -> Bool { return a == 0 && b != 0 }");
    }
    #[test]
    fn factory_results_bind_class_arguments_before_field_resolution() {
        let checker = check("class Box<T> { var value: T\nfn make(v: T) -> Box<T> { return Box { value: v } } } class Main { fnc read() -> String { return Box::make(\"ok\").value } }");
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
        let checker = check("class Holder<T> { var factory: fnc(T) -> T } class Main { fnc invoke(holder: Holder<String>) -> String { return holder.factory(\"ok\") } }");
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
        let checker = check("class Box<T> { var values: List<T>\nfnc make() -> Box<T> { return Box { values: [] } } } class Main { fnc use() -> Nothing { const box: Box<String> = Box::make() } }");
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
        let checker = check("interface Reader { fn read() -> Float64 } class Packet { var byte: UInt8 } class Main { fnc invoke(callback: fnc(UInt8) -> Float32, packet: Packet, reader: Reader) -> Float64 { callback(packet.byte)\nreader.read()\nreturn packet.byte as Float64 } }");
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
        let checker = check("class Box<T> { var value: T\nfnc make(value: T) -> Box<T> { return Box { value: value } } } class Main { fnc convert(f: fnc(Int64) -> Box<String>) -> Box<String> { return f(1) } fnc use() -> Nothing { const result = Main::convert(x => { return Box::make(\"ok\") }) } }");
        assert!(checker
            .semantic_facts()
            .calls
            .values()
            .any(|call| call.dispatch == CallDispatch::Closure
                && call.result == ValueType::Named("Box".into(), vec![ValueType::String])));
    }
}

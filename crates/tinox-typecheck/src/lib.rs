pub mod annotations;

use std::collections::{HashMap, HashSet};
use tinox_common::{Error, ErrorBag, Span, Spanned};
use tinox_parser::{
    BinaryOp, Class, Decl, DeclKind, Expr, ExprKind, Function, Literal, Pattern, SourceFile, Stmt,
    StmtKind, Type, UnaryOp, Visibility,
};

#[derive(Debug, Clone)]
pub enum TypeError {
    UndefinedVariable(String, Span),
    UndefinedFunction(String, Span),
    TypeMismatch {
        expected: String,
        found: String,
        span: Span,
    },
    BinaryOpTypeMismatch {
        op: String,
        lhs: String,
        rhs: String,
        span: Span,
    },
    UnaryOpTypeMismatch {
        op: String,
        operand: String,
        span: Span,
    },
    InvalidArgumentCount {
        expected: usize,
        found: usize,
        span: Span,
    },
    InvalidFieldAccess(String, Span),
    IndexNotInteger(Span),
    FieldNotFound(String, String, Span),
    CannotAssignToImmutable(String, Span),
    MutableExpected(Span),
    ReturnTypeMismatch {
        expected: String,
        found: String,
        span: Span,
    },
    MissingReturn(Span),
    BreakOutsideLoop(Span),
    ContinueOutsideLoop(Span),
    CatchParameterTypeMismatch(String, String, Span),
    ThrowTypeMismatch(String, Span),
    InvalidCast(String, String, Span),
    InvalidMatchArm(String, Span),
    NonExhaustiveMatch(Span),
    InvalidRangeType(String, Span),
    DivisionByZero(Span),
    CannotInferType(Span),
    DuplicateDefinition(String, Span),
    UndefinedInterface(String, Span),
    InterfaceMethodConflict {
        interface: String,
        parent: String,
        method: String,
        span: Span,
    },
    PrivateAccess {
        class: String,
        member: String,
        span: Span,
    },
    ProtectedAccess {
        class: String,
        member: String,
        span: Span,
    },
    /// `Name::member(...)` where `Name` is neither a known enum, nor a known
    /// class, nor a registered static/instance method — previously silently
    /// returned Any and produced garbage code.
    UnresolvedStaticPath {
        name: String,
        member: String,
        span: Span,
    },
    /// `Class::method(...)` where `Class` is a known (non-generic) class but has
    /// no method by that name (typo or missing definition) — previously silently
    /// returned Any.
    UnknownStaticMethod {
        class: String,
        method: String,
        span: Span,
    },
    /// `Enum::Variant` where `Enum` is a known enum but has no such variant
    /// (typo) — previously silently returned Named(Enum) and built a bogus value.
    UnknownEnumVariant {
        enum_name: String,
        variant: String,
        span: Span,
    },
}

impl TypeError {
    fn to_error(&self) -> Error {
        match self {
            TypeError::UndefinedVariable(name, span) => {
                Error::new(*span, format!("undefined variable: {}", name))
            }
            TypeError::UndefinedFunction(name, span) => {
                Error::new(*span, format!("undefined function: {}", name))
            }
            TypeError::TypeMismatch {
                expected,
                found,
                span,
            } => Error::new(*span, format!("expected {}, found {}", expected, found)),
            TypeError::BinaryOpTypeMismatch { op, lhs, rhs, span } => Error::new(
                *span,
                format!(
                    "binary op '{}' cannot be applied to {} and {}",
                    op, lhs, rhs
                ),
            ),
            TypeError::UnaryOpTypeMismatch { op, operand, span } => Error::new(
                *span,
                format!("unary op '{}' cannot be applied to {}", op, operand),
            ),
            TypeError::InvalidArgumentCount {
                expected,
                found,
                span,
            } => Error::new(
                *span,
                format!("expected {} arguments, found {}", expected, found),
            ),
            TypeError::InvalidFieldAccess(typename, span) => {
                Error::new(*span, format!("cannot access field on type: {}", typename))
            }
            TypeError::IndexNotInteger(span) => {
                Error::new(*span, "array index must be an integer type".to_string())
            }
            TypeError::FieldNotFound(typename, field, span) => {
                Error::new(*span, format!("type {} has no field '{}'", typename, field))
            }
            TypeError::CannotAssignToImmutable(name, span) => Error::new(
                *span,
                format!("cannot assign to immutable variable: {}", name),
            ),
            TypeError::MutableExpected(span) => Error::new(
                *span,
                "mutable variable expected for compound assignment".to_string(),
            ),
            TypeError::ReturnTypeMismatch {
                expected,
                found,
                span,
            } => Error::new(
                *span,
                format!(
                    "function return type mismatch: expected {}, found {}",
                    expected, found
                ),
            ),
            TypeError::MissingReturn(span) => {
                Error::new(*span, "missing return statement in function".to_string())
            }
            TypeError::BreakOutsideLoop(span) => {
                Error::new(*span, "break statement outside of loop".to_string())
            }
            TypeError::ContinueOutsideLoop(span) => {
                Error::new(*span, "continue statement outside of loop".to_string())
            }
            TypeError::CatchParameterTypeMismatch(expected, found, span) => Error::new(
                *span,
                format!(
                    "catch parameter type mismatch: expected {}, found {}",
                    expected, found
                ),
            ),
            TypeError::ThrowTypeMismatch(ty, span) => Error::new(
                *span,
                format!("throw requires a string or error type, found: {}", ty),
            ),
            TypeError::InvalidCast(from, to, span) => {
                Error::new(*span, format!("cannot cast from {} to {}", from, to))
            }
            TypeError::InvalidMatchArm(ty, span) => {
                Error::new(*span, format!("invalid match arm for type: {}", ty))
            }
            TypeError::NonExhaustiveMatch(span) => {
                Error::new(*span, "non-exhaustive match patterns".to_string())
            }
            TypeError::InvalidRangeType(ty, span) => Error::new(
                *span,
                format!("range requires integer operands, found: {}", ty),
            ),
            TypeError::DivisionByZero(span) => Error::new(*span, "division by zero".to_string()),
            TypeError::CannotInferType(span) => Error::new(*span, "cannot infer type".to_string()),
            TypeError::DuplicateDefinition(name, span) => {
                Error::new(*span, format!("duplicate definition of: {}", name))
            }
            TypeError::UndefinedInterface(name, span) => {
                Error::new(*span, format!("undefined interface: {}", name))
            }
            TypeError::InterfaceMethodConflict {
                interface,
                parent,
                method,
                span,
            } => Error::new(
                *span,
                format!(
                    "interface '{}' extends '{}' but both define method '{}' with conflicting signatures",
                    interface, parent, method
                ),
            ),
            TypeError::PrivateAccess { class, member, span } => Error::new(
                *span,
                format!("'{}' is private to class '{}'", member, class),
            ),
            TypeError::ProtectedAccess { class, member, span } => Error::new(
                *span,
                format!("'{}' is protected in class '{}' and not accessible here", member, class),
            ),
            TypeError::UnresolvedStaticPath { name, member, span } => Error::new(
                *span,
                format!(
                    "unresolved '{name}::{member}': no type, enum, or static method named '{name}' in scope (missing import?)"
                ),
            ),
            TypeError::UnknownStaticMethod { class, method, span } => Error::new(
                *span,
                format!("type '{class}' has no method '{method}'"),
            ),
            TypeError::UnknownEnumVariant { enum_name, variant, span } => Error::new(
                *span,
                format!("enum '{enum_name}' has no variant '{variant}'"),
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ValueType {
    Int,
    Float,
    Bool,
    Char,
    String,
    Nothing,
    Never,
    Any,
    /// Lists/arrays with an element type (Any = unknown/erased)
    Array(Box<ValueType>),
    /// Maps with a value type (keys are always String; Any = unknown)
    Map(Box<ValueType>),
    Ref,
    Fn,
    /// A class or enum type. The second field holds generic type arguments
    /// (`Box<Int64>` → `Named("Box", [Int])`); empty for non-generic types and
    /// type parameters. Carrying the args lets a `T`-typed field/return of a
    /// generic instance resolve to the concrete type (B2 step 1).
    Named(String, Vec<ValueType>),
    Tuple,
    Range,
    Nullable(Box<ValueType>),
    Null,
}

// Custom equality (B2 step 1): two `Named` types are equal iff their class/enum
// names match — the generic type args are additional info for field-type
// substitution, NOT part of type identity. This keeps every existing `==`
// comparison behaving exactly as before the args were added (`Box<Int>` and
// `Box<String>` were both `Named("Box")` and compared equal). Arg-aware
// compatibility is a later step.
impl PartialEq for ValueType {
    fn eq(&self, other: &Self) -> bool {
        use ValueType::*;
        match (self, other) {
            (Int, Int) | (Float, Float) | (Bool, Bool) | (Char, Char)
            | (String, String) | (Nothing, Nothing) | (Never, Never)
            | (Any, Any) | (Ref, Ref) | (Fn, Fn) | (Tuple, Tuple)
            | (Range, Range) | (Null, Null) => true,
            (Array(a), Array(b)) => a == b,
            (Map(a), Map(b)) => a == b,
            (Nullable(a), Nullable(b)) => a == b,
            (Named(a, _), Named(b, _)) => a == b,
            _ => false,
        }
    }
}
impl Eq for ValueType {}

impl ValueType {
    /// An erased array (element unknown) — for builtin signatures.
    fn any_array() -> Self {
        ValueType::Array(Box::new(ValueType::Any))
    }

    /// An erased map (value unknown) — for builtin signatures.
    fn any_map() -> Self {
        ValueType::Map(Box::new(ValueType::Any))
    }

    fn from_parser_type(ty: &Type) -> Self {
        match ty {
            Type::Int8
            | Type::Int16
            | Type::Int32
            | Type::Int64
            | Type::UInt8
            | Type::UInt16
            | Type::UInt32
            | Type::UInt64 => ValueType::Int,
            Type::Float32 | Type::Float64 => ValueType::Float,
            Type::Bool => ValueType::Bool,
            Type::Char => ValueType::Char,
            Type::String => ValueType::String,
            Type::Nothing => ValueType::Nothing,
            Type::Never => ValueType::Never,
            Type::Any => ValueType::Any,
            Type::Infer => ValueType::Any,
            Type::Named(name) => ValueType::Named(name.clone(), vec![]),
            Type::Generic { name, args } if name == "Array" || name == "List" => {
                ValueType::Array(Box::new(
                    args.first().map(Self::from_parser_type).unwrap_or(ValueType::Any),
                ))
            }
            Type::Generic { name, args } if name == "Map" => ValueType::Map(Box::new(
                args.get(1).map(Self::from_parser_type).unwrap_or(ValueType::Any),
            )),
            Type::Generic { name, args } => ValueType::Named(
                name.clone(),
                args.iter().map(Self::from_parser_type).collect(),
            ),
            Type::Array(inner) => ValueType::Array(Box::new(Self::from_parser_type(inner))),
            Type::Map(_, v) => ValueType::Map(Box::new(Self::from_parser_type(v))),
            Type::Tuple(_) => ValueType::Tuple,
            Type::Mutable(_) => ValueType::Ref,
            Type::Ref(_) => ValueType::Ref,
            Type::Fn { .. } => ValueType::Fn,
            Type::Nullable(inner) => ValueType::Nullable(Box::new(ValueType::from_parser_type(inner))),
        }
    }

    /// Translates a ValueType into codegen's marker language
    /// (container_marker/elem_marker): "String", "Float", a class name,
    /// "Array"/"Array:String"/"Array:Float"/"Array:<marker>"/"List:Class",
    /// "Map"/"Map:<marker>". None for types with no marker semantics
    /// (Int, Bool, Any, …).
    /// A display for error messages — shows element/value types
    /// ("List<String>", "Map<String, Int64>"). Don't use this for
    /// dispatch keys, that's what to_string() is for.
    fn display(&self) -> String {
        match self {
            ValueType::Array(e) if **e != ValueType::Any => format!("List<{}>", e.display()),
            ValueType::Map(v) if **v != ValueType::Any => {
                format!("Map<String, {}>", v.display())
            }
            ValueType::Nullable(inner) => format!("{}?", inner.display()),
            other => other.to_string(),
        }
    }

}

impl std::fmt::Display for ValueType {
    // Deliberately erased: produces dispatch keys ("Array_len", "Map_get")
    // — never append element types. For error messages see display().
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ValueType::Int => "Int64".to_string(),
            ValueType::Float => "Float64".to_string(),
            ValueType::Bool => "Bool".to_string(),
            ValueType::Char => "Char".to_string(),
            ValueType::String => "String".to_string(),
            ValueType::Nothing => "Nothing".to_string(),
            ValueType::Never => "Never".to_string(),
            ValueType::Any => "Any".to_string(),
            ValueType::Array(_) => "Array".to_string(),
            ValueType::Map(_) => "Map".to_string(),
            ValueType::Ref => "Ref".to_string(),
            ValueType::Fn => "Fn".to_string(),
            ValueType::Named(name, _) => name.clone(),
            ValueType::Tuple => "Tuple".to_string(),
            ValueType::Range => "Range".to_string(),
            ValueType::Nullable(inner) => format!("{}?", inner),
            ValueType::Null => "null".to_string(),
        };
        write!(f, "{}", s)
    }
}

struct SymbolTable {
    variables: HashMap<String, (ValueType, bool)>,
    functions: HashMap<String, FunctionSignature>,
    in_loop: bool,
}

#[derive(Debug, Clone)]
struct FunctionSignature {
    params: Vec<(String, ValueType)>,
    return_type: ValueType,
}

impl SymbolTable {
    fn new() -> Self {
        Self {
            variables: HashMap::new(),
            functions: HashMap::new(),
            in_loop: false,
        }
    }

    fn enter_scope(&self) -> HashMap<String, (ValueType, bool)> {
        self.variables.clone()
    }

    fn exit_scope(&mut self, vars: HashMap<String, (ValueType, bool)>) {
        self.variables = vars;
    }
}

pub struct TypeChecker {
    errors: Vec<Error>,
    symbols: SymbolTable,
    enums: HashMap<String, Vec<String>>, // enum_name -> list of variant names
    /// "Enum::Variant" -> payload types — for typed match bindings
    enum_variant_payloads: HashMap<String, Vec<ValueType>>,
    interfaces: HashMap<String, Vec<(String, FunctionSignature)>>, // interface_name -> [(method_name, signature)]
    interface_extends: HashMap<String, (Vec<String>, tinox_common::Span)>, // interface_name -> (parent_names, span)
    interface_implementations: HashMap<String, Vec<String>>, // class_name -> [interface_names]
    class_parents: HashMap<String, String>, // child_class_name -> parent_class_name
    current_class: Option<String>, // class currently being type-checked
    method_visibility: HashMap<String, Visibility>, // ClassName_methodName -> visibility
    field_visibility: HashMap<String, Visibility>,  // ClassName.fieldName -> visibility
    /// Type parameter names in scope for the function currently being checked.
    type_param_scope: HashSet<String>,
    /// Expected return type of the function currently being checked.
    current_return_type: Option<ValueType>,
    /// All class names defined in the program — allows passing a class as a value (e.g. DB.of(User)).
    known_class_names: HashSet<String>,
    /// Subset of known_class_names that are generic (`class Foo<T>`). Generic
    /// classes legitimately use struct-literal fields without registered field
    /// declarations, so field-access checks stay permissive for them.
    generic_class_names: HashSet<String>,
    /// Generic class name → its type-parameter names (`Box` → `["T"]`). Used to
    /// substitute a `T`-typed field/return against a generic instance's type
    /// arguments (`Named("Box", [Int])`) so it resolves to the concrete type (B2).
    class_type_params: HashMap<String, Vec<String>>,
    /// `ClassName_method` keys of instance methods whose body uses `this`. Such a
    /// method's receiver is the leading call arg (self); a method that does NOT
    /// use `this` takes its receiver as an explicit first declared param. This
    /// disambiguates the two `Class::method(obj, …)` calling styles (Bug 38)
    /// deterministically, so the arg-count check can be exact (Bug 47).
    method_uses_this: HashSet<String>,
    /// The inferred type of every visited expression, keyed by NodeId
    /// (assign_node_ids; ID 0 = unassigned, not recorded). Exported to
    /// codegen via expr_value_types() — since phase 3, the ONLY
    /// typecheck→codegen type channel (the lossy marker table
    /// expr_markers has been removed).
    expr_types: HashMap<u32, ValueType>,
    /// B2 step 2: a generic method's `ClassName_method` → its UNERASED
    /// param types (self prepended as `Named(Class, [T-params])` for
    /// instance methods) + the type-param names (class + method). The
    /// registered `FunctionSignature` erases params to `Any` — type-argument
    /// inference at the call site (`Box::make(42)` → T=Int) needs the
    /// full form as a binding source.
    generic_method_param_types: HashMap<String, (Vec<ValueType>, Vec<String>)>,
    /// class_name -> its own+inherited declared field names, in declaration
    /// order. Used by the `StructLiteral` check (Bug 130) to catch a
    /// literal that omits a required field at compile time instead of
    /// leaving the corresponding heap slot as uninitialized garbage.
    class_fields: HashMap<String, Vec<String>>,
    /// Issue #165: `ClassName_method` -> for each Fn-typed param at that
    /// (0-based, self excluded) arg position, the UNERASED `ValueType`s of
    /// the lambda's OWN params (e.g. `andThen(transform: fnc(T) -> Option<U>)`
    /// registers `[(0, [Named("T", [])])]`). Mirrors `generic_method_param_types`
    /// but reaches one level deeper (into the `Fn` param's own param types,
    /// which `type_to_value` otherwise collapses to the opaque `ValueType::Fn`
    /// marker) — needed so an arrow-sugar lambda argument (`n => ...`, whose
    /// own param has no annotation at all, unlike Tinox's other lambda forms)
    /// can be given the same call-site `infer_lambda_with_param_hints`
    /// contextual-typing treatment the built-in `Array::map/filter/forEach`
    /// path already gets, instead of type-checking with every param as `Any`
    /// (which is what silently mis-specialized `U` to `Int64` downstream in
    /// codegen — see `infer_own_type_params`'s doc comment in codegen.rs).
    generic_instance_fn_arg_hints: HashMap<String, Vec<(usize, Vec<ValueType>)>>,
    /// Every decl seen via register_declarations (both `typecheck_with_prelude`'s
    /// prelude files AND the main source's own decls, registered first thing
    /// inside check_source_file) -- `validate_annotations` needs this because it
    /// documents its own assumption ("imports are already merged into
    /// source.decls") that only holds for the real compiler's `compile_file`
    /// pipeline (resolve_imports merges everything into one decl list BEFORE
    /// typecheck ever runs). tinox-lsp's `typecheck_with_prelude` deliberately
    /// keeps the main file and its stdlib preludes as SEPARATE SourceFiles
    /// instead -- without this, a custom annotation declared in a prelude (e.g.
    /// `@JsonSerializable`, declared via `@annotation class JsonSerializable {}`
    /// inside tinox.core.json's own JsonSerializable.tnx) reads as "unknown
    /// annotation" for any file that merely IMPORTS it, since
    /// validate_annotations's own first-pass registration scan never saw it.
    /// Real bug, found live: an Eclipse-imported real project's own
    /// `@JsonSerializable class Person` flagged this way despite being
    /// perfectly valid and compiling fine with the real `tinox` compiler.
    prelude_decls: Vec<Decl>,
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut symbols = SymbolTable::new();
        symbols.functions.insert(
            "print".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::Any)],
                return_type: ValueType::Nothing,
            },
        );
        symbols.functions.insert(
            "println".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::Any)],
                return_type: ValueType::Nothing,
            },
        );
        symbols.functions.insert(
            "len".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::Any)],
                return_type: ValueType::Int,
            },
        );
        symbols.functions.insert(
            "assert".to_string(),
            FunctionSignature {
                params: vec![("cond".to_string(), ValueType::Bool)],
                return_type: ValueType::Nothing,
            },
        );
        for name in &["first", "last"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::Int },
            );
        }
        symbols.functions.insert(
            "slice".to_string(),
            FunctionSignature {
                params: vec![
                    ("arr".to_string(), ValueType::any_array()),
                    ("from".to_string(), ValueType::Int),
                    ("to".to_string(), ValueType::Int),
                ],
                return_type: ValueType::any_array(),
            },
        );
        symbols.functions.insert(
            "abs".to_string(),
            FunctionSignature { params: vec![("x".to_string(), ValueType::Any)], return_type: ValueType::Any },
        );
        for name in &["min", "max"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("a".to_string(), ValueType::Any), ("b".to_string(), ValueType::Any)],
                    return_type: ValueType::Any,
                },
            );
        }
        symbols.functions.insert(
            "sqrt".to_string(),
            FunctionSignature { params: vec![("x".to_string(), ValueType::Float)], return_type: ValueType::Float },
        );
        symbols.functions.insert(
            "push".to_string(),
            FunctionSignature {
                params: vec![
                    ("arr".to_string(), ValueType::any_array()),
                    ("val".to_string(), ValueType::Any),
                ],
                return_type: ValueType::any_array(),
            },
        );
        symbols.functions.insert(
            "pop".to_string(),
            FunctionSignature {
                params: vec![("arr".to_string(), ValueType::any_array())],
                return_type: ValueType::any_array(),
            },
        );
        symbols.functions.insert(
            "charAt".to_string(),
            FunctionSignature {
                params: vec![
                    ("s".to_string(), ValueType::String),
                    ("i".to_string(), ValueType::Int),
                ],
                return_type: ValueType::String,
            },
        );
        symbols.functions.insert(
            "toInt".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::String)],
                return_type: ValueType::Int,
            },
        );
        symbols.functions.insert(
            "toFloat".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::String)],
                return_type: ValueType::Float,
            },
        );
        symbols.functions.insert(
            "toString".to_string(),
            FunctionSignature {
                params: vec![("value".to_string(), ValueType::Any)],
                return_type: ValueType::String,
            },
        );
        // Math builtins
        symbols.functions.insert(
            "pow".to_string(),
            FunctionSignature {
                params: vec![("base".to_string(), ValueType::Any), ("exp".to_string(), ValueType::Any)],
                return_type: ValueType::Float,
            },
        );
        for name in &["floor", "ceil", "round"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("x".to_string(), ValueType::Any)],
                    return_type: ValueType::Float,
                },
            );
        }
        // exit builtin
        symbols.functions.insert(
            "exit".to_string(),
            FunctionSignature {
                params: vec![("code".to_string(), ValueType::Int)],
                return_type: ValueType::Nothing,
            },
        );
        // Polymorphic contains/indexOf (string or array)
        for name in &["contains", "indexOf"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("col".to_string(), ValueType::Any), ("val".to_string(), ValueType::Any)],
                    return_type: ValueType::Any,
                },
            );
        }
        // String builtins
        for name in &["toUpper", "toLower", "trim"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("s".to_string(), ValueType::String)],
                    return_type: ValueType::String,
                },
            );
        }
        for name in &["startsWith", "endsWith"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("s".to_string(), ValueType::String), ("pat".to_string(), ValueType::String)],
                    return_type: ValueType::Bool,
                },
            );
        }
        // Array builtins
        for name in &["sort", "reverse"] {
            symbols.functions.insert(
                name.to_string(),
                FunctionSignature {
                    params: vec![("arr".to_string(), ValueType::any_array())],
                    return_type: ValueType::any_array(),
                },
            );
        }
        // Array method builtins (MethodCall dispatch: Array_len, Array_push, etc.)
        symbols.functions.insert("Array_len".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::Int });
        symbols.functions.insert("Array_push".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("v".to_string(), ValueType::Any)], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_pop".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_first".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::Any });
        symbols.functions.insert("Array_last".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::Any });
        symbols.functions.insert("Array_sort".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_reverse".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array())], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_contains".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("v".to_string(), ValueType::Any)], return_type: ValueType::Bool });
        symbols.functions.insert("Array_indexOf".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("v".to_string(), ValueType::Any)], return_type: ValueType::Int });
        symbols.functions.insert("Array_slice".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("from".to_string(), ValueType::Int), ("to".to_string(), ValueType::Int)], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_insert".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("i".to_string(), ValueType::Int), ("v".to_string(), ValueType::Any)], return_type: ValueType::Nothing });
        // Lambda-based array methods (map/filter/forEach/reduce). The
        // signatures are deliberately permissive (Fn arg, Any result) —
        // the result element type is refined from the lambda in the
        // MethodCall arm (map: the lambda's return type, filter: the
        // input element type).
        symbols.functions.insert("Array_map".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("f".to_string(), ValueType::Fn)], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_filter".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("f".to_string(), ValueType::Fn)], return_type: ValueType::any_array() });
        symbols.functions.insert("Array_forEach".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("f".to_string(), ValueType::Fn)], return_type: ValueType::Nothing });
        symbols.functions.insert("Array_reduce".to_string(), FunctionSignature { params: vec![("arr".to_string(), ValueType::any_array()), ("init".to_string(), ValueType::Any), ("f".to_string(), ValueType::Fn)], return_type: ValueType::Any });
        // List<T> is the same as Array at runtime — mirror all Array_* builtins under List_*
        for (arr_key, sig) in symbols.functions.iter().filter(|(k, _)| k.starts_with("Array_")).map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>() {
            let list_key = arr_key.replacen("Array_", "List_", 1);
            symbols.functions.insert(list_key, sig);
        }
        // String method builtins (MethodCall dispatch: String_len, String_toUpper, etc.)
        symbols.functions.insert("String_len".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("String_charAt".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String), ("i".to_string(), ValueType::Int)], return_type: ValueType::String });
        symbols.functions.insert("String_toInt".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("String_toFloat".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::Float });
        symbols.functions.insert("String_toString".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::String });
        symbols.functions.insert("String_contains".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)], return_type: ValueType::Bool });
        symbols.functions.insert("String_indexOf".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("String_startsWith".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)], return_type: ValueType::Bool });
        symbols.functions.insert("String_endsWith".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)], return_type: ValueType::Bool });
        for name in &["String_toUpper", "String_toLower", "String_trim", "String_toLowerCase", "String_toUpperCase",
                       "String_reverse", "Any_toUpper", "Any_toLower", "Any_trim", "Any_toLowerCase", "Any_toUpperCase"] {
            symbols.functions.insert(name.to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::Any)], return_type: ValueType::String });
        }
        symbols.functions.insert("String_substring".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("from".to_string(), ValueType::Int), ("to".to_string(), ValueType::Int)],
            return_type: ValueType::String,
        });
        symbols.functions.insert("String_replace".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("from".to_string(), ValueType::String), ("to".to_string(), ValueType::String)],
            return_type: ValueType::String,
        });
        // Float64 math methods
        for name in &["Float64_sqrt", "Float64_abs", "Float64_floor", "Float64_ceil", "Float64_round",
                       "Int64_toFloat", "Float64_toInt", "Float64_toString"] {
            let ret = if name.ends_with("toString") { ValueType::String }
                      else if name.ends_with("toInt") { ValueType::Int }
                      else { ValueType::Float };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("self".to_string(), ValueType::Float)],
                return_type: ret,
            });
        }
        symbols.functions.insert("split".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("delim".to_string(), ValueType::String)],
            return_type: ValueType::Array(Box::new(ValueType::String)),
        });
        symbols.functions.insert("String_split".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("delim".to_string(), ValueType::String)],
            return_type: ValueType::Array(Box::new(ValueType::String)),
        });
        symbols.functions.insert("join".to_string(), FunctionSignature {
            params: vec![("arr".to_string(), ValueType::any_array()), ("sep".to_string(), ValueType::String)],
            return_type: ValueType::String,
        });
        symbols.functions.insert("Array_join".to_string(), FunctionSignature {
            params: vec![("arr".to_string(), ValueType::any_array()), ("sep".to_string(), ValueType::String)],
            return_type: ValueType::String,
        });
        // Map constructor
        symbols.functions.insert("Map_new".to_string(), FunctionSignature { params: vec![], return_type: ValueType::any_map() });
        // Map builtins (method-call style: Map_get, Map_insert, etc.)
        symbols.functions.insert(
            "Map_get".to_string(),
            FunctionSignature {
                params: vec![("m".to_string(), ValueType::any_map()), ("key".to_string(), ValueType::Any)],
                return_type: ValueType::Any,
            },
        );
        symbols.functions.insert(
            "Map_insert".to_string(),
            FunctionSignature {
                params: vec![
                    ("m".to_string(), ValueType::any_map()),
                    ("key".to_string(), ValueType::Any),
                    ("val".to_string(), ValueType::Any),
                ],
                return_type: ValueType::Nothing,
            },
        );
        symbols.functions.insert(
            "Map_contains".to_string(),
            FunctionSignature {
                params: vec![("m".to_string(), ValueType::any_map()), ("key".to_string(), ValueType::Any)],
                return_type: ValueType::Bool,
            },
        );
        symbols.functions.insert(
            "Map_remove".to_string(),
            FunctionSignature {
                params: vec![("m".to_string(), ValueType::any_map()), ("key".to_string(), ValueType::Any)],
                return_type: ValueType::Nothing,
            },
        );
        symbols.functions.insert(
            "Map_len".to_string(),
            FunctionSignature {
                params: vec![("m".to_string(), ValueType::any_map())],
                return_type: ValueType::Int,
            },
        );
        symbols.functions.insert("Map_keys".to_string(), FunctionSignature {
            params: vec![("m".to_string(), ValueType::any_map())],
            return_type: ValueType::Array(Box::new(ValueType::String)),
        });
        symbols.functions.insert("Map_values".to_string(), FunctionSignature {
            params: vec![("m".to_string(), ValueType::any_map())],
            return_type: ValueType::any_array(),
        });
        symbols.functions.insert("Map_set".to_string(), FunctionSignature {
            params: vec![("m".to_string(), ValueType::any_map()), ("k".to_string(), ValueType::Any), ("v".to_string(), ValueType::Any)],
            return_type: ValueType::Nothing,
        });
        // Int / Float toString
        for prefix in &["Int64", "Int32", "Int", "Float64", "Float32", "Float", "Bool"] {
            symbols.functions.insert(format!("{}_toString", prefix), FunctionSignature {
                params: vec![("v".to_string(), ValueType::Any)],
                return_type: ValueType::String,
            });
        }
        // HTTP low-level builtins (called from http_server.tnx)
        symbols.functions.insert("httpServerCreate".to_string(), FunctionSignature { params: vec![("port".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpServerBoundPort".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpServerAcceptConn".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpServerReadRequest".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::String });
        symbols.functions.insert("httpServerSendRaw".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int), ("data".to_string(), ValueType::String)], return_type: ValueType::Nothing });
        symbols.functions.insert("httpServerCloseConn".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Nothing });
        symbols.functions.insert("httpServerClose".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Nothing });
        // HTTPS/TLS + connection-handle API
        symbols.functions.insert("httpServerCreateTls".to_string(), FunctionSignature { params: vec![("port".to_string(), ValueType::Int), ("certPath".to_string(), ValueType::String), ("keyPath".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("httpServerAcceptTls".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpServerAcceptConnHandle".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpConnReadRequest".to_string(), FunctionSignature { params: vec![("conn".to_string(), ValueType::Int)], return_type: ValueType::String });
        symbols.functions.insert("httpConnSendRaw".to_string(), FunctionSignature { params: vec![("conn".to_string(), ValueType::Int), ("data".to_string(), ValueType::String)], return_type: ValueType::Nothing });
        symbols.functions.insert("httpConnFromFd".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("httpConnFromFdTls".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int), ("host".to_string(), ValueType::String), ("verify".to_string(), ValueType::Bool)], return_type: ValueType::Int });
        // Binary-safe conn primitives (WebSocket frames): bytes as Array<Int64>
        symbols.functions.insert("httpConnReadN".to_string(), FunctionSignature { params: vec![("conn".to_string(), ValueType::Int), ("n".to_string(), ValueType::Int)], return_type: ValueType::Array(Box::new(ValueType::Int)) });
        symbols.functions.insert("httpConnWriteBytes".to_string(), FunctionSignature { params: vec![("conn".to_string(), ValueType::Int), ("bytes".to_string(), ValueType::Array(Box::new(ValueType::Int)))], return_type: ValueType::Int });
        symbols.functions.insert("httpConnClose".to_string(), FunctionSignature { params: vec![("conn".to_string(), ValueType::Int)], return_type: ValueType::Nothing });
        // File I/O builtins
        symbols.functions.insert("open".to_string(), FunctionSignature {
            params: vec![("path".to_string(), ValueType::String), ("mode".to_string(), ValueType::String)],
            return_type: ValueType::Named("File".to_string(), vec![]),
        });
        symbols.functions.insert("fileExists".to_string(), FunctionSignature {
            params: vec![("path".to_string(), ValueType::String)],
            return_type: ValueType::Bool,
        });
        symbols.functions.insert("deleteFile".to_string(), FunctionSignature {
            params: vec![("path".to_string(), ValueType::String)],
            return_type: ValueType::Nothing,
        });
        let file_ty = ValueType::Named("File".to_string(), vec![]);
        symbols.functions.insert("File_read".to_string(), FunctionSignature {
            params: vec![("f".to_string(), file_ty.clone())],
            return_type: ValueType::String,
        });
        symbols.functions.insert("File_readLine".to_string(), FunctionSignature {
            params: vec![("f".to_string(), file_ty.clone())],
            return_type: ValueType::String,
        });
        symbols.functions.insert("File_write".to_string(), FunctionSignature {
            params: vec![("f".to_string(), file_ty.clone()), ("s".to_string(), ValueType::String)],
            return_type: ValueType::Nothing,
        });
        symbols.functions.insert("File_close".to_string(), FunctionSignature {
            params: vec![("f".to_string(), file_ty.clone())],
            return_type: ValueType::Nothing,
        });
        symbols.functions.insert("File_eof".to_string(), FunctionSignature {
            params: vec![("f".to_string(), file_ty)],
            return_type: ValueType::Bool,
        });
        // String additional methods
        symbols.functions.insert("String_charCodeAt".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("i".to_string(), ValueType::Int)],
            return_type: ValueType::Int,
        });
        symbols.functions.insert("String_lastIndexOf".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)],
            return_type: ValueType::Int,
        });
        symbols.functions.insert("String_repeat".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("n".to_string(), ValueType::Int)],
            return_type: ValueType::String,
        });
        symbols.functions.insert("String_padLeft".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("n".to_string(), ValueType::Int), ("c".to_string(), ValueType::String)],
            return_type: ValueType::String,
        });
        symbols.functions.insert("String_padRight".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String), ("n".to_string(), ValueType::Int), ("c".to_string(), ValueType::String)],
            return_type: ValueType::String,
        });
        // Standalone char/string helpers
        for name in &["fromCharCode", "charCodeAt"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("n".to_string(), ValueType::Any)],
                return_type: ValueType::String,
            });
        }
        // Array removeAt
        symbols.functions.insert("Array_removeAt".to_string(), FunctionSignature {
            params: vec![("arr".to_string(), ValueType::any_array()), ("i".to_string(), ValueType::Int)],
            return_type: ValueType::Nothing,
        });
        symbols.functions.insert("List_removeAt".to_string(), FunctionSignature {
            params: vec![("arr".to_string(), ValueType::any_array()), ("i".to_string(), ValueType::Int)],
            return_type: ValueType::Nothing,
        });
        // Time
        symbols.functions.insert("now".to_string(), FunctionSignature { params: vec![], return_type: ValueType::Int });
        symbols.functions.insert("Time_now".to_string(), FunctionSignature { params: vec![], return_type: ValueType::Int });
        symbols.functions.insert("Time_format".to_string(), FunctionSignature { params: vec![("t".to_string(), ValueType::Int), ("fmt".to_string(), ValueType::String)], return_type: ValueType::String });
        symbols.functions.insert("Time_parse".to_string(), FunctionSignature { params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("Time_diff".to_string(), FunctionSignature { params: vec![("a".to_string(), ValueType::Int), ("b".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("sleep".to_string(), FunctionSignature {
            params: vec![("ms".to_string(), ValueType::Int)], return_type: ValueType::Nothing,
        });
        // sleep_ms instead of sleep as the runtime symbol name — libc
        // already exports a `sleep(unsigned int seconds)` with a
        // different signature; our own `sleep` would collide at the
        // link symbol.
        symbols.functions.insert("sleep_ms".to_string(), FunctionSignature {
            params: vec![("ms".to_string(), ValueType::Int)], return_type: ValueType::Nothing,
        });
        symbols.functions.insert("Time_toString".to_string(), FunctionSignature {
            params: vec![("t".to_string(), ValueType::Any)], return_type: ValueType::String,
        });
        // Regex
        for name in &["regexIsMatch", "regexFindFirst", "regexFindAll", "regexReplaceAll", "regexSplit"] {
            let ret = if *name == "regexIsMatch" { ValueType::Bool }
                      else if *name == "regexFindAll" || *name == "regexSplit" { ValueType::Array(Box::new(ValueType::String)) }
                      else { ValueType::String };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("s".to_string(), ValueType::String), ("p".to_string(), ValueType::String)],
                return_type: ret,
            });
        }
        symbols.functions.insert("regexMatchGroups".to_string(), FunctionSignature {
            params: vec![
                ("pattern".to_string(), ValueType::String), ("subject".to_string(), ValueType::String),
                ("offset".to_string(), ValueType::Int), ("icase".to_string(), ValueType::Int),
            ],
            return_type: ValueType::any_array(),
        });
        // Env
        for name in &["envGet", "envSet", "envRemove", "envCurrentDir", "envSetCurrentDir"] {
            let ret = if *name == "envGet" || *name == "envCurrentDir" { ValueType::String } else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("k".to_string(), ValueType::String)],
                return_type: ret,
            });
        }
        // Process
        for name in &["processExit", "processId", "processArgs", "printStackTrace", "gcCollect", "memoryUsage"] {
            let ret = if *name == "processArgs" { ValueType::Array(Box::new(ValueType::String)) }
                      else if *name == "processId" || *name == "memoryUsage" { ValueType::Int }
                      else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![], return_type: ret,
            });
        }
        // Metrics (manual API, MetricsRegistry/Stopwatch — @Timed/@Counted
        // auto-instrumentation injects the same calls directly in codegen)
        symbols.functions.insert("tinox_counter_inc".to_string(), FunctionSignature {
            params: vec![("name".to_string(), ValueType::String)], return_type: ValueType::Nothing,
        });
        symbols.functions.insert("tinox_histogram_record".to_string(), FunctionSignature {
            params: vec![("name".to_string(), ValueType::String), ("ns".to_string(), ValueType::Int)],
            return_type: ValueType::Nothing,
        });
        symbols.functions.insert("tinox_gauge_set".to_string(), FunctionSignature {
            params: vec![("name".to_string(), ValueType::String), ("value".to_string(), ValueType::Int)],
            return_type: ValueType::Nothing,
        });
        symbols.functions.insert("tinox_metrics_prometheus".to_string(), FunctionSignature {
            params: vec![], return_type: ValueType::String,
        });
        symbols.functions.insert("tinox_clock_nanos".to_string(), FunctionSignature {
            params: vec![], return_type: ValueType::Int,
        });
        // Random
        symbols.functions.insert("randomInt".to_string(), FunctionSignature {
            params: vec![("min".to_string(), ValueType::Int), ("max".to_string(), ValueType::Int)],
            return_type: ValueType::Int,
        });
        symbols.functions.insert("randomFloat".to_string(), FunctionSignature {
            params: vec![], return_type: ValueType::Float,
        });
        // Crypto / hashing
        // wsAcceptKey: Sec-WebSocket-Accept from the client key (sha1+base64 in C)
        for name in &["sha256Hash", "md5Hash", "sha1Hash", "wsAcceptKey", "hmacSha256Hash", "base64Encode", "base64Decode", "base64EncodeChar"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("data".to_string(), ValueType::String)],
                return_type: ValueType::String,
            });
        }
        // AES-256-GCM (issue 74). Two arguments (data + key), hence its
        // own registration instead of the single-param collection loop
        // above. A "Raw" suffix, because Crypto::aesEncrypt/aesDecrypt
        // (.tnx) are the actual public API and call these bare extern
        // functions — analogous to Crypto::md5 -> md5Hash, but with a
        // different name than the class method, to avoid a recursive
        // name collision.
        for name in &["aesEncryptRaw", "aesDecryptRaw"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![
                    ("data".to_string(), ValueType::String),
                    ("key".to_string(), ValueType::String),
                ],
                return_type: ValueType::String,
            });
        }
        // Byte-safe HMAC-SHA256/SHA256 (issue 77, SCRAM-SHA-256 for
        // AMQP-1.0 SASL) — unlike hmacSha256Hash/sha256Hash (String,
        // C-string-based), NUL-safe, because SCRAM pushes salts/nonces/
        // digests as real binary data through HMAC/SHA256 chains.
        symbols.functions.insert("hmacSha256Bytes".to_string(), FunctionSignature {
            params: vec![
                ("data".to_string(), ValueType::Array(Box::new(ValueType::Int))),
                ("key".to_string(), ValueType::Array(Box::new(ValueType::Int))),
            ],
            return_type: ValueType::Array(Box::new(ValueType::Int)),
        });
        symbols.functions.insert("sha256Bytes".to_string(), FunctionSignature {
            params: vec![("data".to_string(), ValueType::Array(Box::new(ValueType::Int)))],
            return_type: ValueType::Array(Box::new(ValueType::Int)),
        });
        // UUID
        symbols.functions.insert("uuidGenerate".to_string(), FunctionSignature { params: vec![], return_type: ValueType::String });
        // URI
        for name in &["uriEncode", "uriDecode", "uriEncodeComponent", "uriDecodeComponent"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::String,
            });
        }
        // Math C functions
        for name in &["sinf", "cosf", "tanf", "logf", "log10f", "sqrtf", "expf", "powf", "fabsf", "floorf", "ceilf"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("x".to_string(), ValueType::Float)], return_type: ValueType::Float,
            });
        }
        // File I/O extended
        for name in &["fileReadAllText", "fileWriteAllText", "fileDelete", "fileClose"] {
            let ret = if *name == "fileReadAllText" { ValueType::String } else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("path".to_string(), ValueType::String)], return_type: ret,
            });
        }
        for name in &["dirList", "dirCreate", "dirDelete"] {
            let ret = if *name == "dirList" { ValueType::Array(Box::new(ValueType::String)) } else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("path".to_string(), ValueType::String)], return_type: ret,
            });
        }
        // Socket builtins
        for name in &["socketCreateTcp", "socketCreateUdp"] {
            symbols.functions.insert(name.to_string(), FunctionSignature { params: vec![], return_type: ValueType::Int });
        }
        for name in &["socketConnect", "socketBind", "socketListen"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("fd".to_string(), ValueType::Int), ("v".to_string(), ValueType::Any)],
                return_type: ValueType::Bool,
            });
        }
        symbols.functions.insert("socketAccept".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Int });
        symbols.functions.insert("socketSend".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int), ("data".to_string(), ValueType::String)], return_type: ValueType::Int });
        symbols.functions.insert("socketReceive".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int), ("size".to_string(), ValueType::Int)], return_type: ValueType::String });
        symbols.functions.insert("socketClose".to_string(), FunctionSignature { params: vec![("fd".to_string(), ValueType::Int)], return_type: ValueType::Nothing });
        // HTTP client builtins
        for name in &["httpGet", "httpPost", "httpPut", "httpDelete", "httpPatch"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("url".to_string(), ValueType::String)],
                return_type: ValueType::Named("HttpResponse".to_string(), vec![]),
            });
        }
        for name in &["httpSetHeader", "httpClearHeaders", "httpHeader", "httpBody", "httpStatusCode"] {
            let ret = if name.ends_with("Header") || name.ends_with("Body") { ValueType::String }
                      else if name.ends_with("Code") { ValueType::Int }
                      else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("v".to_string(), ValueType::Any)], return_type: ret,
            });
        }
        {
            let name = &"HttpResponse_statusCode";
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("r".to_string(), ValueType::Named("HttpResponse".to_string(), vec![]))],
                return_type: ValueType::Int,
            });
        }
        symbols.functions.insert("HttpResponse_body".to_string(), FunctionSignature {
            params: vec![("r".to_string(), ValueType::Named("HttpResponse".to_string(), vec![]))],
            return_type: ValueType::String,
        });
        // HttpServer method builtins
        let hs = ValueType::Named("HttpServer".to_string(), vec![]);
        for name in &["HttpServer_get", "HttpServer_post", "HttpServer_put", "HttpServer_delete", "HttpServer_patch", "HttpServer_use"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("srv".to_string(), hs.clone()), ("path".to_string(), ValueType::String), ("handler".to_string(), ValueType::Fn)],
                return_type: ValueType::Nothing,
            });
        }
        symbols.functions.insert("HttpServer_listen".to_string(), FunctionSignature {
            params: vec![("srv".to_string(), hs.clone())], return_type: ValueType::Nothing,
        });
        symbols.functions.insert("HttpServer_stop".to_string(), FunctionSignature {
            params: vec![("srv".to_string(), hs)], return_type: ValueType::Nothing,
        });
        // Heap_comparator (function-typed field)
        symbols.functions.insert("Heap_comparator".to_string(), FunctionSignature {
            params: vec![("h".to_string(), ValueType::Any), ("a".to_string(), ValueType::Any), ("b".to_string(), ValueType::Any)],
            return_type: ValueType::Bool,
        });
        // Pool_factory
        symbols.functions.insert("Pool_factory".to_string(), FunctionSignature {
            params: vec![("p".to_string(), ValueType::Any)],
            return_type: ValueType::Any,
        });
        // XML
        for name in &["xmlAttr", "xmlTagName", "xmlTextContent"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("node".to_string(), ValueType::Any)], return_type: ValueType::String,
            });
        }
        symbols.functions.insert("xmlChildren".to_string(), FunctionSignature {
            params: vec![("node".to_string(), ValueType::Any)], return_type: ValueType::any_array(),
        });
        // Zip
        for name in &["zipAddFile", "zipExtractFile", "zipListEntries", "zipRemoveFile"] {
            let ret = if *name == "zipListEntries" { ValueType::any_array() } else { ValueType::Nothing };
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("path".to_string(), ValueType::String)], return_type: ret,
            });
        }
        // float math builtins
        for name in &["log", "exp", "fabs", "sin", "cos", "tan", "log10", "mathTgamma", "mathLgamma", "mathCbrt", "mathTrunc", "mathRint", "mathLogb",
                      "mathLog2", "mathLog10", "mathExp2", "mathExp10"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("x".to_string(), ValueType::Float)], return_type: ValueType::Float,
            });
        }
        // two-arg float math builtins
        symbols.functions.insert("atan2".to_string(), FunctionSignature {
            params: vec![("y".to_string(), ValueType::Float), ("x".to_string(), ValueType::Float)],
            return_type: ValueType::Float,
        });
        for name in &["mathIsNan", "mathIsInfinite", "mathIsNormal"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![("x".to_string(), ValueType::Float)], return_type: ValueType::Int,
            });
        }
        for name in &["mathNan", "mathInf"] {
            symbols.functions.insert(name.to_string(), FunctionSignature {
                params: vec![], return_type: ValueType::Float,
            });
        }
        // jgrep-tinox env/time builtins
        symbols.functions.insert("envDump".to_string(), FunctionSignature { params: vec![], return_type: ValueType::String });
        symbols.functions.insert("currentTimeSecs".to_string(), FunctionSignature { params: vec![], return_type: ValueType::Int });
        symbols.functions.insert("strftimeStr".to_string(), FunctionSignature {
            params: vec![("fmt".to_string(), ValueType::String), ("t".to_string(), ValueType::Int)],
            return_type: ValueType::String,
        });
        symbols.functions.insert("fromdateStr".to_string(), FunctionSignature {
            params: vec![("s".to_string(), ValueType::String)], return_type: ValueType::Int,
        });
        symbols.functions.insert("printStderr".to_string(), FunctionSignature {
            params: vec![("msg".to_string(), ValueType::String)], return_type: ValueType::Nothing,
        });
        symbols.functions.insert("isStdinTty".to_string(), FunctionSignature {
            params: vec![], return_type: ValueType::Int,
        });
        symbols.functions.insert("isStdoutTty".to_string(), FunctionSignature {
            params: vec![], return_type: ValueType::Int,
        });
        Self {
            errors: Vec::new(),
            symbols,
            enums: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            interfaces: HashMap::new(),
            interface_extends: HashMap::new(),
            interface_implementations: HashMap::new(),
            class_parents: HashMap::new(),
            current_class: None,
            method_visibility: HashMap::new(),
            field_visibility: HashMap::new(),
            type_param_scope: HashSet::new(),
            current_return_type: None,
            known_class_names: HashSet::new(),
            generic_class_names: HashSet::new(),
            class_type_params: HashMap::new(),
            method_uses_this: HashSet::new(),
            expr_types: HashMap::new(),
            generic_method_param_types: HashMap::new(),
            class_fields: HashMap::new(),
            generic_instance_fn_arg_hints: HashMap::new(),
            prelude_decls: Vec::new(),
        }
    }

    /// Rich per-expression type export (type-system unification): the full
    /// `ValueType` per node id, including generic type args — unlike
    /// `expr_markers`, which flattens to a lossy string. The codegen migrates its
    /// own inference (`infer_struct_type_local`) onto this over time.
    pub fn expr_value_types(&self) -> HashMap<u32, ValueType> {
        self.expr_types.clone()
    }

    pub fn check(&mut self, source: &SourceFile) -> Result<SourceFile, ErrorBag> {
        self.check_source_file(source);
        if self.errors.is_empty() {
            Ok(source.clone())
        } else {
            Err(ErrorBag {
                errors: std::mem::take(&mut self.errors),
            })
        }
    }

    /// Returns (interface_name -> ordered method names, class_name -> [interface names])
    pub fn interface_info(
        &self,
    ) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
        let iface_methods: HashMap<String, Vec<String>> = self
            .interfaces
            .iter()
            .map(|(name, methods)| {
                (
                    name.clone(),
                    methods.iter().map(|(m, _)| m.clone()).collect(),
                )
            })
            .collect();
        (iface_methods, self.interface_implementations.clone())
    }

    /// Register an enum's variants, UNIONing with any existing entry of the same
    /// name. Enum names aren't module-qualified, so several modules can define
    /// e.g. `MediaType` with different variants; a flat overwrite would make a
    /// valid `MediaType::None` (from one definition) fail the variant check
    /// (Bug 45) just because another same-named enum was registered last. The
    /// union keeps the check permissive across collisions while still catching a
    /// variant that exists in NO definition (a real typo).
    fn register_enum_variants(
        enums: &mut HashMap<String, Vec<String>>,
        name: &str,
        variants: &[String],
    ) {
        let entry = enums.entry(name.to_string()).or_default();
        for v in variants {
            if !entry.contains(v) {
                entry.push(v.clone());
            }
        }
    }

    /// Does this statement (transitively) reference `this`? Used to classify an
    /// instance method's calling convention (Bug 47): a `this`-using method takes
    /// its receiver as the implicit self (leading call arg); one that doesn't
    /// takes the receiver as an explicit first param.
    fn stmt_uses_this(stmt: &Stmt) -> bool {
        match &stmt.node {
            StmtKind::Expr(e) => Self::expr_uses_this(e),
            StmtKind::Let { value, .. } | StmtKind::Var { value, .. } => {
                value.as_ref().is_some_and(Self::expr_uses_this)
            }
            StmtKind::Assignment { target, value } => {
                Self::expr_uses_this(target) || Self::expr_uses_this(value)
            }
            StmtKind::If { cond, then_branch, else_branch } => {
                Self::expr_uses_this(cond)
                    || Self::stmt_uses_this(then_branch)
                    || else_branch.as_ref().is_some_and(|b| Self::stmt_uses_this(b))
            }
            StmtKind::While { cond, body } => Self::expr_uses_this(cond) || Self::stmt_uses_this(body),
            StmtKind::For { iter, body, .. } => Self::expr_uses_this(iter) || Self::stmt_uses_this(body),
            StmtKind::ForC { init, cond, update, body } => {
                init.as_ref().is_some_and(|s| Self::stmt_uses_this(s))
                    || cond.as_ref().is_some_and(Self::expr_uses_this)
                    || update.as_ref().is_some_and(Self::expr_uses_this)
                    || Self::stmt_uses_this(body)
            }
            StmtKind::Loop { body } => Self::stmt_uses_this(body),
            StmtKind::Block(stmts) => stmts.iter().any(Self::stmt_uses_this),
            StmtKind::Return(v) => v.as_ref().is_some_and(Self::expr_uses_this),
            StmtKind::Throw(e) => Self::expr_uses_this(e),
            StmtKind::Try { body, catches, finally } => {
                Self::stmt_uses_this(body)
                    || catches.iter().any(|c| Self::stmt_uses_this(&c.body))
                    || finally.as_ref().is_some_and(|f| Self::stmt_uses_this(f))
            }
            StmtKind::Defer(s) => Self::stmt_uses_this(s),
            StmtKind::Select { arms, default } => {
                arms.iter().any(|a| Self::stmt_uses_this(&a.body))
                    || default.as_ref().is_some_and(|d| Self::stmt_uses_this(d))
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Empty => false,
        }
    }

    fn expr_uses_this(expr: &Expr) -> bool {
        match &expr.node {
            ExprKind::This => true,
            ExprKind::Literal(_) | ExprKind::Ident(_) | ExprKind::Channel
            | ExprKind::Break | ExprKind::Continue => false,
            ExprKind::Binary { lhs, rhs, .. } => Self::expr_uses_this(lhs) || Self::expr_uses_this(rhs),
            ExprKind::Unary { operand, .. } => Self::expr_uses_this(operand),
            ExprKind::Call { func, args } => {
                Self::expr_uses_this(func) || args.iter().any(Self::expr_uses_this)
            }
            ExprKind::MethodCall { obj, args, .. } => {
                Self::expr_uses_this(obj) || args.iter().any(Self::expr_uses_this)
            }
            ExprKind::SuperCall { args, .. } => args.iter().any(Self::expr_uses_this),
            ExprKind::New { args, .. } => args.iter().any(Self::expr_uses_this),
            ExprKind::EnumValue { args, .. } => args.iter().any(Self::expr_uses_this),
            ExprKind::Index { obj, index } => Self::expr_uses_this(obj) || Self::expr_uses_this(index),
            ExprKind::FieldAccess { obj, .. } => Self::expr_uses_this(obj),
            ExprKind::ArrayLiteral(es) | ExprKind::Tuple(es) => es.iter().any(Self::expr_uses_this),
            ExprKind::MapLiteral(kvs) => kvs.iter().any(|(k, v)| Self::expr_uses_this(k) || Self::expr_uses_this(v)),
            ExprKind::StructLiteral { fields, .. } => fields.iter().any(|(_, v)| Self::expr_uses_this(v)),
            ExprKind::Block(stmts) => stmts.iter().any(Self::stmt_uses_this),
            ExprKind::If { cond, then_branch, else_branch } => {
                Self::expr_uses_this(cond) || Self::expr_uses_this(then_branch)
                    || else_branch.as_ref().is_some_and(|b| Self::expr_uses_this(b))
            }
            ExprKind::While { cond, body } => Self::expr_uses_this(cond) || Self::expr_uses_this(body),
            ExprKind::For { iter, body, .. } => Self::expr_uses_this(iter) || Self::expr_uses_this(body),
            ExprKind::Loop { body } => Self::expr_uses_this(body),
            ExprKind::Match { expr, cases } => {
                Self::expr_uses_this(expr)
                    || cases.iter().any(|c| Self::expr_uses_this(&c.body)
                        || c.guard.as_ref().is_some_and(Self::expr_uses_this))
            }
            ExprKind::Return(v) => v.as_ref().is_some_and(|e| Self::expr_uses_this(e)),
            ExprKind::Throw(e) => Self::expr_uses_this(e),
            ExprKind::Assign { target, value } | ExprKind::CompoundAssign { target, value, .. } => {
                Self::expr_uses_this(target) || Self::expr_uses_this(value)
            }
            ExprKind::Lambda { body, .. } => Self::expr_uses_this(body),
            ExprKind::Spawn(e) | ExprKind::Await(e) | ExprKind::Recv(e)
            | ExprKind::Cast { expr: e, .. } | ExprKind::Is { expr: e, .. }
            | ExprKind::TupleIndex { tuple: e, .. } => Self::expr_uses_this(e),
            ExprKind::Send { channel, value } => Self::expr_uses_this(channel) || Self::expr_uses_this(value),
            ExprKind::Range { start, end, .. } => Self::expr_uses_this(start) || Self::expr_uses_this(end),
            ExprKind::Try { body, catches, finally } => {
                Self::expr_uses_this(body)
                    || catches.iter().any(|c| Self::stmt_uses_this(&c.body))
                    || finally.as_ref().is_some_and(|f| Self::expr_uses_this(f))
            }
        }
    }

    /// Flattens one level of namespace wrapping (`namespace ns { ... }`)
    /// into the surrounding decl list, preserving declaration order —
    /// every registration/collection pass over `source.decls` needs to
    /// treat a namespace-wrapped decl exactly like a top-level one, a
    /// proven bug magnet when duplicated by hand instead of shared: #161
    /// (`generic_method_param_types` missing for namespace-wrapped
    /// classes), #165's registration gap for the equivalent
    /// `generic_instance_fn_arg_hints` table (every stdlib generic class,
    /// `Option<T>` included, is namespace-wrapped), and — found while
    /// consolidating those two into this single helper — namespace-wrapped
    /// classes never registering into `interface_implementations`, and
    /// namespace-wrapped `interface`/`trait` decls never registering into
    /// `self.interfaces` at all, even though the parser explicitly allows
    /// both inside a `namespace { }` block. Nested `namespace { namespace
    /// { ... } }` stays unsupported, matching prior behavior exactly (not
    /// new scope).
    fn flatten_decls(source: &SourceFile) -> Vec<&Decl> {
        let mut out = Vec::new();
        for decl in &source.decls {
            if let DeclKind::Namespace(ns) = &decl.node {
                out.extend(ns.decls.iter());
            } else {
                out.push(decl);
            }
        }
        out
    }

    fn register_declarations(&mut self, source: &SourceFile) {
        self.prelude_decls
            .extend(Self::flatten_decls(source).into_iter().cloned());
        for decl in Self::flatten_decls(source) {
            match &decl.node {
                DeclKind::Function(f) => {
                    let sig = FunctionSignature {
                        params: f
                            .params
                            .iter()
                            .map(|p| (p.name.clone(), Self::type_to_value_erasing(&p.param_type, &f.type_params)))
                            .collect(),
                        return_type: Self::type_to_value_erasing(&f.ret_type, &f.type_params),
                    };
                    self.symbols.functions.insert(f.name.clone(), sig);
                }
                DeclKind::Class(c) => {
                    if let Some(parent) = &c.extends {
                        self.class_parents.insert(c.name.clone(), parent.clone());
                    }
                    // Register class->interface(s) up front, not lazily at the
                    // end of check_class(): a class-to-interface assignment
                    // (`let x: IDrawable = Circle{...}`) in a FUNCTION that
                    // appears earlier in decl order than the class itself
                    // (increasingly common with one-type-per-file: the entry
                    // file's own `fn main` is merged before its imported
                    // classes) would otherwise be checked before
                    // interface_implementations["Circle"] existed, and fail
                    // with a spurious "expected IDrawable, found Circle".
                    self.interface_implementations
                        .insert(c.name.clone(), c.implements.clone());
                    for field in &c.fields {
                        let ty = Self::type_to_value(&field.field_type);
                        let key = format!("{}.{}", c.name, field.name);
                        self.symbols.variables.insert(key.clone(), (ty, true));
                        self.field_visibility.insert(key, field.visibility.clone());
                    }
                    self.class_fields.insert(
                        c.name.clone(),
                        c.fields.iter().map(|f| f.name.clone()).collect(),
                    );
                    if c.annotations.iter().any(|a| a.name == "Log") {
                        let key = format!("{}.log", c.name);
                        self.symbols.variables.insert(key.clone(), (ValueType::Named("Logger".to_string(), vec![]), false));
                        self.field_visibility.insert(key, Visibility::Public);
                    }
                    for method in &c.methods {
                        let mut params = if method.static_ {
                            vec![]
                        } else {
                            vec![("self".to_string(), ValueType::Named(c.name.clone(), vec![]))]
                        };
                        // Erase both the method's own type params (`fn map<U>`)
                        // AND the enclosing class's (`class Stack<T>`) — only
                        // the former was erased before, so any instance method
                        // whose param/return type names the CLASS param
                        // directly (`push(value: T)`) registered a literal
                        // `Named("T")` signature and failed typecheck on every
                        // real call ("expected T, found Int64").
                        let erase_params: Vec<String> = c
                            .type_params
                            .iter()
                            .chain(method.type_params.iter())
                            .cloned()
                            .collect();
                        params.extend(
                            method
                                .params
                                .iter()
                                .map(|p| (p.name.clone(), Self::type_to_value_erasing(&p.param_type, &erase_params))),
                        );
                        let sig = FunctionSignature {
                            params,
                            return_type: Self::erase_method_return_type(&method.ret_type, &method.type_params, &erase_params),
                        };
                        let key = format!("{}_{}", c.name, method.name);
                        if !method.static_ && Self::stmt_uses_this(&method.body) {
                            self.method_uses_this.insert(key.clone());
                        }
                        // B2 step 2: for generic methods, additionally
                        // store the UNERASED param types (self carries
                        // the class type params: `Named("Box",
                        // [Named("T")])`), so the call site can unify
                        // type arguments from the args.
                        if !erase_params.is_empty() {
                            let mut unerased: Vec<ValueType> = if method.static_ {
                                vec![]
                            } else {
                                vec![ValueType::Named(
                                    c.name.clone(),
                                    c.type_params
                                        .iter()
                                        .map(|tp| ValueType::Named(tp.clone(), vec![]))
                                        .collect(),
                                )]
                            };
                            unerased.extend(
                                method.params.iter().map(|p| Self::type_to_value(&p.param_type)),
                            );
                            self.generic_method_param_types
                                .insert(key.clone(), (unerased, erase_params.clone()));

                            // Issue #165: for each Fn-typed param, also keep the
                            // lambda's OWN param types unerased (`type_to_value`
                            // on the outer `Type::Fn` alone collapses straight to
                            // `ValueType::Fn`, losing exactly the `T` reference a
                            // call-site hint needs) — see
                            // `generic_instance_fn_arg_hints`'s doc comment.
                            let fn_hints: Vec<(usize, Vec<ValueType>)> = method
                                .params
                                .iter()
                                .enumerate()
                                .filter_map(|(i, p)| match &p.param_type {
                                    Type::Fn { params: fp, .. } if !fp.is_empty() => {
                                        Some((i, fp.iter().map(Self::type_to_value).collect()))
                                    }
                                    _ => None,
                                })
                                .collect();
                            if !fn_hints.is_empty() {
                                self.generic_instance_fn_arg_hints.insert(key.clone(), fn_hints);
                            }
                        }
                        self.symbols.functions.insert(key.clone(), sig);
                        self.method_visibility.insert(key, method.visibility.clone());
                    }
                    // @JsonSerializable: register compiler-generated toJson() and fromJson()
                    if c.annotations.iter().any(|a| a.name == "JsonSerializable") {
                        self.symbols.functions.insert(
                            format!("{}_toJson", c.name),
                            FunctionSignature {
                                params: vec![("self".to_string(), ValueType::Named(c.name.clone(), vec![]))],
                                return_type: ValueType::String,
                            },
                        );
                        self.symbols.functions.insert(
                            format!("{}_fromJson", c.name),
                            FunctionSignature {
                                params: vec![("json_val".to_string(), ValueType::Named("JsonValue".to_string(), vec![]))],
                                return_type: ValueType::Named(c.name.clone(), vec![]),
                            },
                        );
                    }
                }
                DeclKind::Enum(e) => {
                    let variant_names: Vec<String> =
                        e.variants.iter().map(|v| v.name.clone()).collect();
                    Self::register_enum_variants(&mut self.enums, &e.name, &variant_names);
                    for variant in &e.variants {
                        self.enum_variant_payloads.insert(
                            format!("{}::{}", e.name, variant.name),
                            variant.args.iter().map(Self::type_to_value).collect(),
                        );
                        let ty = ValueType::Named(format!("{}.{}", e.name, variant.name), vec![]);
                        self.symbols
                            .variables
                            .insert(format!("{}.{}", e.name, variant.name), (ty, true));
                    }
                }
                DeclKind::Interface(iface) => {
                    let methods = iface
                        .methods
                        .iter()
                        .map(|m| {
                            let sig = FunctionSignature {
                                params: m
                                    .params
                                    .iter()
                                    .map(|p| (p.name.clone(), Self::type_to_value(&p.param_type)))
                                    .collect(),
                                return_type: Self::type_to_value(&m.ret_type),
                            };
                            (m.name.clone(), sig)
                        })
                        .collect();
                    self.interfaces.insert(iface.name.clone(), methods);
                    if !iface.extends.is_empty() {
                        self.interface_extends.insert(
                            iface.name.clone(),
                            (iface.extends.clone(), decl.span),
                        );
                    }
                }
                DeclKind::Trait(t) => {
                    let methods = t
                        .methods
                        .iter()
                        .map(|m| {
                            let sig = FunctionSignature {
                                params: m
                                    .params
                                    .iter()
                                    .map(|p| (p.name.clone(), Self::type_to_value(&p.param_type)))
                                    .collect(),
                                return_type: Self::type_to_value(&m.ret_type),
                            };
                            (m.name.clone(), sig)
                        })
                        .collect();
                    self.interfaces.insert(t.name.clone(), methods);
                }
                DeclKind::Immutable(u) => {
                    for field in &u.fields {
                        let ty = Self::type_to_value(&field.param_type);
                        let key = format!("{}.{}", u.name, field.name);
                        self.symbols.variables.insert(key.clone(), (ty, true));
                        self.field_visibility.insert(key, Visibility::Public);
                    }
                    let params: Vec<(String, ValueType)> = u.fields.iter()
                        .map(|f| (f.name.clone(), Self::type_to_value(&f.param_type)))
                        .collect();
                    let sig = FunctionSignature {
                        params,
                        return_type: ValueType::Named(u.name.clone(), vec![]),
                    };
                    let key = format!("{}_new", u.name);
                    self.symbols.functions.insert(key.clone(), sig);
                    self.method_visibility.insert(key, Visibility::Public);
                }
                // Namespace-wrapped decls are already flattened into this
                // loop by `flatten_decls` above (matched by their own kind,
                // same as a top-level decl) — nested `namespace { namespace
                // { ... } }` stays unsupported, same as before.
                DeclKind::Namespace(_) => {}
                _ => {}
            }
        }

        // Second pass: expand interfaces with methods from their parent interfaces.
        // Collect the extends relationships first to avoid borrow conflicts.
        let extends_map: Vec<(String, Vec<String>, tinox_common::Span)> = self
            .interface_extends
            .iter()
            .map(|(name, (parents, span))| (name.clone(), parents.clone(), *span))
            .collect();

        for (iface_name, parents, span) in &extends_map {
            // Validate that every parent interface exists.
            for parent in parents {
                if !self.interfaces.contains_key(parent) {
                    self.errors.push(
                        TypeError::UndefinedInterface(parent.clone(), *span).to_error(),
                    );
                }
            }

            // Collect all methods from parents (only those that are defined).
            let mut inherited: Vec<(String, FunctionSignature)> = Vec::new();
            for parent in parents {
                if let Some(parent_methods) = self.interfaces.get(parent).cloned() {
                    for (method_name, parent_sig) in parent_methods {
                        inherited.push((method_name, parent_sig));
                    }
                }
            }

            // Merge: for each inherited method, check for conflicts with own methods.
            if let Some(own_methods) = self.interfaces.get(iface_name).cloned() {
                for (method_name, parent_sig) in &inherited {
                    if let Some((_, own_sig)) =
                        own_methods.iter().find(|(n, _)| n == method_name)
                    {
                        // Both define the method — check for signature conflict.
                        let params_match = own_sig.params.len() == parent_sig.params.len()
                            && own_sig
                                .params
                                .iter()
                                .zip(parent_sig.params.iter())
                                .all(|((_, a), (_, b))| self.types_compatible(a, b));
                        let ret_match = self.types_compatible(
                            &own_sig.return_type,
                            &parent_sig.return_type,
                        );
                        if !params_match || !ret_match {
                            // Find the parent name that defined this method.
                            let parent_name = parents
                                .iter()
                                .find(|p| {
                                    self.interfaces
                                        .get(*p)
                                        .map(|ms| ms.iter().any(|(n, _)| n == method_name))
                                        .unwrap_or(false)
                                })
                                .cloned()
                                .unwrap_or_default();
                            self.errors.push(
                                TypeError::InterfaceMethodConflict {
                                    interface: iface_name.clone(),
                                    parent: parent_name,
                                    method: method_name.clone(),
                                    span: *span,
                                }
                                .to_error(),
                            );
                        }
                        // Own method takes precedence — do not add inherited version.
                    } else {
                        // Not defined in own methods — add the inherited one.
                        self.interfaces
                            .get_mut(iface_name)
                            .unwrap()
                            .push((method_name.clone(), parent_sig.clone()));
                    }
                }
            }
        }

        // Register interface methods as InterfaceName_methodName in symbol table
        // so method calls through interface-typed variables type-check correctly.
        let iface_entries: Vec<(String, String, FunctionSignature)> = self
            .interfaces
            .iter()
            .flat_map(|(iface_name, methods)| {
                methods.iter().map(move |(method_name, sig)| {
                    (iface_name.clone(), method_name.clone(), sig.clone())
                })
            })
            .collect();
        for (iface_name, method_name, sig) in iface_entries {
            let full_name = format!("{}_{}", iface_name, method_name);
            // first param is self (the interface-typed object)
            let mut params = vec![("self".to_string(), ValueType::Named(iface_name.clone(), vec![]))];
            params.extend(sig.params.clone());
            self.symbols.functions.insert(
                full_name,
                FunctionSignature {
                    params,
                    return_type: sig.return_type.clone(),
                },
            );
        }

    }

    /// Expands class inheritance (fields and methods from parent classes) for
    /// every class declared directly in `source`. A parent class may instead
    /// live only in a `register_declarations`-registered prelude
    /// (`typecheck_with_prelude`'s split file-plus-preludes model, used by
    /// both tinox-lsp and `check_explicit_imports`, issue #194) rather than
    /// in `source` itself -- `self.prelude_decls` is consulted as a
    /// parent-lookup fallback, but a prelude class is never itself treated as
    /// one this pass must expand (see `expand_prelude_class_inheritance`,
    /// below, for that half — calling both, in the right order, is what
    /// makes a cross-prelude inheritance chain like `Derived extends Base`,
    /// where BOTH are preludes of some third file, resolve correctly
    /// regardless of which of the two got registered first).
    fn expand_class_inheritance(&mut self, source: &SourceFile) {
        let own_classes: HashMap<String, tinox_parser::Class> = Self::flatten_decls(source)
            .into_iter()
            .filter_map(|d| match &d.node {
                DeclKind::Class(c) => Some(c.clone()),
                _ => None,
            })
            .map(|c| (c.name.clone(), c))
            .collect();
        let extra_decls = self.prelude_decls.clone();
        self.expand_class_inheritance_impl(own_classes, &extra_decls);
    }

    /// The prelude-side counterpart of `expand_class_inheritance`: expands
    /// inheritance for EVERY class across ALL registered preludes together,
    /// as one combined pass over `self.prelude_decls` — unlike running the
    /// same expansion once per individual `register_declarations` call (the
    /// original design), which broke as soon as a prelude class's parent was
    /// itself a *different*, not-yet-registered prelude (registration order
    /// depends on `check_explicit_imports`'s DFS over the import graph, not
    /// inheritance order — found live via `tests/e2e/inherited_static_dispatch`,
    /// where `Base`/`Derived` mutually import each other and DFS happened to
    /// register `Derived` first, silently dropping its inherited methods).
    /// Must run once, after every prelude has been registered, and before
    /// `check(source)` — see `typecheck_with_prelude`.
    fn expand_prelude_class_inheritance(&mut self) {
        let own_classes: HashMap<String, tinox_parser::Class> = self
            .prelude_decls
            .iter()
            .filter_map(|d| match &d.node {
                DeclKind::Class(c) => Some(c.clone()),
                _ => None,
            })
            .map(|c| (c.name.clone(), c))
            .collect();
        self.expand_class_inheritance_impl(own_classes, &[]);
    }

    fn expand_class_inheritance_impl(
        &mut self,
        own_classes: HashMap<String, tinox_parser::Class>,
        extra_decls: &[Decl],
    ) {
        use std::collections::HashSet;
        let mut class_map = own_classes;
        let class_names: Vec<String> = class_map.keys().cloned().collect();
        let own_class_names: HashSet<String> = class_names.iter().cloned().collect();
        for d in extra_decls {
            if let DeclKind::Class(c) = &d.node {
                class_map.entry(c.name.clone()).or_insert_with(|| c.clone());
            }
        }

        let mut processed: HashSet<String> = HashSet::new();

        loop {
            let before = processed.len();
            for name in &class_names {
                if processed.contains(name) {
                    continue;
                }
                let c = &class_map[name];
                let parent_ready = c
                    .extends
                    .as_ref()
                    .map(|p| processed.contains(p) || !own_class_names.contains(p))
                    .unwrap_or(true);
                if !parent_ready {
                    continue;
                }

                if let Some(parent_name) = &c.extends {
                    if !class_map.contains_key(parent_name) {
                        self.errors.push(Error::new(
                            c.span,
                            format!("undefined parent class: {}", parent_name),
                        ));
                        processed.insert(name.clone());
                        continue;
                    }

                    let child_own_fields: HashSet<String> =
                        c.fields.iter().map(|f| f.name.clone()).collect();
                    let child_own_methods: HashSet<String> =
                        c.methods.iter().map(|m| m.name.clone()).collect();

                    // Walk the ancestor chain and collect inherited fields/methods.
                    let mut ancestor = parent_name.clone();
                    while let Some(pc) = class_map.get(&ancestor) {
                        for field in &pc.fields {
                            if child_own_fields.contains(&field.name) {
                                continue;
                            }
                            let child_key = format!("{}.{}", name, field.name);
                            if self.symbols.variables.contains_key(&child_key) {
                                continue;
                            }
                            let ty = Self::type_to_value(&field.field_type);
                            self.symbols.variables.insert(child_key.clone(), (ty, true));
                            self.field_visibility
                                .entry(child_key)
                                .or_insert_with(|| field.visibility.clone());
                            let child_fields = self.class_fields.entry(name.clone()).or_default();
                            if !child_fields.contains(&field.name) {
                                child_fields.push(field.name.clone());
                            }
                        }

                        for method in &pc.methods {
                            if child_own_methods.contains(&method.name) {
                                continue;
                            }
                            let child_key = format!("{}_{}", name, method.name);
                            if self.symbols.functions.contains_key(&child_key) {
                                continue;
                            }
                            let mut params = vec![(
                                "self".to_string(),
                                ValueType::Named(name.clone(), vec![]),
                            )];
                            params.extend(method.params.iter().map(|p| {
                                (p.name.clone(), Self::type_to_value(&p.param_type))
                            }));
                            let sig = FunctionSignature {
                                params,
                                return_type: Self::type_to_value(&method.ret_type),
                            };
                            if Self::stmt_uses_this(&method.body) {
                                self.method_uses_this.insert(child_key.clone());
                            }
                            self.symbols.functions.insert(child_key.clone(), sig);
                            self.method_visibility
                                .entry(child_key)
                                .or_insert_with(|| method.visibility.clone());
                        }

                        ancestor = match &pc.extends {
                            Some(next) => next.clone(),
                            None => break,
                        };
                    }
                }

                self.known_class_names.insert(name.clone());
                if !c.type_params.is_empty() {
                    self.generic_class_names.insert(name.clone());
                    self.class_type_params.insert(name.clone(), c.type_params.clone());
                }
                processed.insert(name.clone());
            }
            if processed.len() == before {
                break;
            }
        }
    }

    fn check_source_file(&mut self, source: &SourceFile) {
        self.register_declarations(source);
        self.expand_class_inheritance(source);

        for decl in Self::flatten_decls(source) {
            match &decl.node {
                DeclKind::Function(f) => {
                    self.check_function(f);
                }
                DeclKind::Class(c) => {
                    self.check_class(c);
                }
                _ => {}
            }
        }

        // Annotation validation pass
        let ann_errors = annotations::validate_annotations(source, &self.prelude_decls);
        self.errors.extend(ann_errors);
    }

    fn check_function(&mut self, f: &Function) {
        let saved_vars = self.symbols.enter_scope();
        let saved_type_params = std::mem::take(&mut self.type_param_scope);
        for tp in &f.type_params {
            self.type_param_scope.insert(tp.clone());
        }
        for param in &f.params {
            self.symbols.variables.insert(
                param.name.clone(),
                (self.resolve_type(&param.param_type), false),
            );
        }
        let is_extern = matches!(f.body.node, StmtKind::Empty);
        let expected = self.resolve_type(&f.ret_type);
        let saved_return_type = self.current_return_type.replace(expected.clone());
        let has_return = self.check_stmt(&f.body);
        self.current_return_type = saved_return_type;
        if !is_extern && expected != ValueType::Nothing && expected != ValueType::Never && !has_return {
            self.errors
                .push(Error::new(f.span, "missing return statement"));
        }
        self.type_param_scope = saved_type_params;
        self.symbols.exit_scope(saved_vars);
    }

    fn check_class(&mut self, c: &Class) {
        let saved_class = self.current_class.clone();
        self.current_class = Some(c.name.clone());
        for method in &c.methods {
            let saved_vars = self.symbols.enter_scope();
            let saved_type_params = std::mem::take(&mut self.type_param_scope);
            for tp in &method.type_params {
                self.type_param_scope.insert(tp.clone());
            }
            // Erase class-level type params too, not just the method's own
            // (see the DeclKind::Class signature-registration arms) — a
            // method BODY referencing a class param directly (`cache.data[key]`
            // with `key: K`, `Cache<K,V>`) needs `key`'s registered type to be
            // Any for the Map-index-validity check, else every generic-class
            // method body with T-typed indexing/comparison spuriously errors.
            let erase_params: Vec<String> = c
                .type_params
                .iter()
                .chain(method.type_params.iter())
                .cloned()
                .collect();
            self.symbols.variables.insert(
                "self".to_string(),
                (ValueType::Named(c.name.clone(), vec![]), false),
            );
            for param in &method.params {
                self.symbols.variables.insert(
                    param.name.clone(),
                    (Self::type_to_value_erasing(&param.param_type, &erase_params), false),
                );
            }
            let expected = Self::type_to_value_erasing(&method.ret_type, &erase_params);
            let saved_return_type = self.current_return_type.replace(expected.clone());
            let has_return = self.check_stmt(&method.body);
            self.current_return_type = saved_return_type;
            if expected != ValueType::Nothing && expected != ValueType::Never && !has_return {
                self.errors
                    .push(Error::new(method.span, "missing return statement"));
            }
            self.type_param_scope = saved_type_params;
            self.symbols.exit_scope(saved_vars);
        }
        self.current_class = saved_class;

        let mut implemented_ifaces = Vec::new();
        for iface_name in &c.implements {
            implemented_ifaces.push(iface_name.clone());
            if let Some(iface_methods) = self.interfaces.get(iface_name).cloned() {
                for (method_name, required_sig) in &iface_methods {
                    let full_name = format!("{}_{}", c.name, method_name);
                    if let Some(class_sig) = self.symbols.functions.get(&full_name).cloned() {
                        // Check param count (excluding implicit self)
                        let class_param_count = class_sig.params.len().saturating_sub(1);
                        if class_param_count != required_sig.params.len() {
                            self.errors.push(Error::new(
                                c.span,
                                format!(
                                    "method '{}' in class {} has wrong number of parameters for interface {}: expected {}, found {}",
                                    method_name, c.name, iface_name, required_sig.params.len(), class_param_count
                                ),
                            ));
                        } else {
                            for (i, ((_, class_ty), (_, req_ty))) in class_sig
                                .params
                                .iter()
                                .skip(1) // skip self
                                .zip(required_sig.params.iter())
                                .enumerate()
                            {
                                if !self.types_compatible(class_ty, req_ty) {
                                    self.errors.push(Error::new(
                                        c.span,
                                        format!(
                                            "method '{}' param {} type mismatch in class {}: expected {}, found {}",
                                            method_name, i, c.name, req_ty, class_ty
                                        ),
                                    ));
                                }
                            }
                            if !self.types_compatible(&class_sig.return_type, &required_sig.return_type) {
                                self.errors.push(Error::new(
                                    c.span,
                                    format!(
                                        "method '{}' return type mismatch in class {}: expected {}, found {}",
                                        method_name, c.name, required_sig.return_type, class_sig.return_type
                                    ),
                                ));
                            }
                        }
                    } else {
                        self.errors.push(Error::new(
                            c.span,
                            format!(
                                "class {} does not implement method '{}' from interface {}",
                                c.name, method_name, iface_name
                            ),
                        ));
                    }
                }
            }
        }
        self.interface_implementations
            .insert(c.name.clone(), implemented_ifaces);
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> bool {
        match &stmt.node {
            StmtKind::Let {
                name, ty, value, ..
            } => {
                if self.symbols.variables.contains_key(name) {
                    self.errors
                        .push(TypeError::DuplicateDefinition(name.clone(), stmt.span).to_error());
                }
                let final_type = match (value, ty) {
                    (Some(v), Some(t)) => {
                        let val_ty = self.infer_type(v);
                        let ann_ty = Self::type_to_value(t);
                        if !self.types_compatible(&ann_ty, &val_ty) {
                            self.errors.push(
                                TypeError::TypeMismatch {
                                    expected: ann_ty.display(),
                                    found: val_ty.display(),
                                    span: v.span,
                                }
                                .to_error(),
                            );
                        }
                        Some(ann_ty)
                    }
                    (Some(v), None) => Some(self.infer_type(v)),
                    (None, Some(t)) => Some(Self::type_to_value(t)),
                    (None, None) => None,
                };
                if let Some(t) = final_type {
                    self.symbols.variables.insert(name.clone(), (t, false));
                } else {
                    self.errors
                        .push(TypeError::CannotInferType(stmt.span).to_error());
                }
                false
            }
            StmtKind::Var {
                name,
                ty,
                value,
                mutable,
                ..
            } => {
                if self.symbols.variables.contains_key(name) {
                    self.errors
                        .push(TypeError::DuplicateDefinition(name.clone(), stmt.span).to_error());
                }
                // Same as for Let: the annotation is the contract — it
                // wins and gets checked against the value (previously it
                // was completely ignored, unchecked, whenever a value
                // was present).
                let inferred_type = match (value, ty) {
                    (Some(v), Some(t)) => {
                        let val_ty = self.infer_type(v);
                        let ann_ty = Self::type_to_value(t);
                        if !self.types_compatible(&ann_ty, &val_ty) {
                            self.errors.push(
                                TypeError::TypeMismatch {
                                    expected: ann_ty.display(),
                                    found: val_ty.display(),
                                    span: v.span,
                                }
                                .to_error(),
                            );
                        }
                        Some(ann_ty)
                    }
                    (Some(v), None) => Some(self.infer_type(v)),
                    (None, Some(t)) => Some(Self::type_to_value(t)),
                    (None, None) => None,
                };
                if let Some(t) = inferred_type {
                    self.symbols.variables.insert(name.clone(), (t, *mutable));
                } else {
                    self.errors
                        .push(TypeError::CannotInferType(stmt.span).to_error());
                }
                false
            }
            StmtKind::Assignment { target, value } => {
                let target_ty = self.infer_type(target);
                let value_ty = self.infer_type(value);
                if !self.types_compatible(&target_ty, &value_ty) {
                    self.errors.push(
                        TypeError::TypeMismatch {
                            expected: target_ty.display(),
                            found: value_ty.display(),
                            span: stmt.span,
                        }
                        .to_error(),
                    );
                }
                if let ExprKind::Ident(name) = &target.node {
                    if let Some((_, mutable)) = self.symbols.variables.get(name) {
                        if !mutable {
                            self.errors.push(
                                TypeError::CannotAssignToImmutable(name.clone(), stmt.span)
                                    .to_error(),
                            );
                        }
                    }
                }
                false
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.infer_type(cond);
                if cond_ty != ValueType::Bool && !matches!(cond_ty, ValueType::Any | ValueType::Named(_, _)) {
                    self.errors.push(
                        TypeError::TypeMismatch {
                            expected: "Bool".to_string(),
                            found: cond_ty.display(),
                            span: cond.span,
                        }
                        .to_error(),
                    );
                }
                let then_returns = self.check_stmt(then_branch);
                let else_returns = if let Some(else_br) = else_branch {
                    self.check_stmt(else_br)
                } else {
                    false
                };
                then_returns && else_returns
            }
            StmtKind::While { cond, body } => {
                let cond_ty = self.infer_type(cond);
                if cond_ty != ValueType::Bool && !matches!(cond_ty, ValueType::Any | ValueType::Named(_, _)) {
                    self.errors.push(
                        TypeError::TypeMismatch {
                            expected: "Bool".to_string(),
                            found: cond_ty.display(),
                            span: cond.span,
                        }
                        .to_error(),
                    );
                }
                let was_in_loop = self.symbols.in_loop;
                self.symbols.in_loop = true;
                self.check_stmt(body);
                self.symbols.in_loop = was_in_loop;
                false
            }
            StmtKind::For { var, iter, body } => {
                let iter_ty = self.infer_type(iter);
                // The loop variable is always the element, not the container
                let elem_ty = match iter_ty {
                    ValueType::Range => ValueType::Int,
                    ValueType::Array(e) => *e,
                    ValueType::String => ValueType::String,
                    other => other,
                };
                let was_in_loop = self.symbols.in_loop;
                self.symbols.in_loop = true;
                self.symbols.variables.insert(var.clone(), (elem_ty, false));
                self.check_stmt(body);
                self.symbols.in_loop = was_in_loop;
                false
            }
            StmtKind::Loop { body } => {
                let was_in_loop = self.symbols.in_loop;
                self.symbols.in_loop = true;
                self.check_stmt(body);
                self.symbols.in_loop = was_in_loop;
                false
            }
            StmtKind::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    let val_ty = self.infer_type(expr);
                    if let Some(expected) = self.current_return_type.clone() {
                        if !self.types_compatible(&expected, &val_ty) {
                            self.errors.push(TypeError::TypeMismatch {
                                expected: expected.display(),
                                found: val_ty.display(),
                                span: expr.span,
                            }.to_error());
                        }
                    }
                }
                true
            }
            StmtKind::Break => {
                if !self.symbols.in_loop {
                    self.errors
                        .push(TypeError::BreakOutsideLoop(stmt.span).to_error());
                }
                false
            }
            StmtKind::Continue => {
                if !self.symbols.in_loop {
                    self.errors
                        .push(TypeError::ContinueOutsideLoop(stmt.span).to_error());
                }
                false
            }
            StmtKind::Throw(expr) => {
                let ty = self.infer_type(expr);
                let ty_str = ty.to_string();
                if ty_str != "String" && ty != ValueType::Any {
                    self.errors
                        .push(TypeError::ThrowTypeMismatch(ty_str, expr.span).to_error());
                }
                true // throw always terminates
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                self.check_stmt(body);
                for catch in catches {
                    self.symbols
                        .variables
                        .insert(catch.param.clone(), (Self::type_to_value(&catch.ty), false));
                    self.check_stmt(&catch.body);
                }
                if let Some(finally_body) = finally {
                    self.check_stmt(finally_body);
                }
                false
            }
            StmtKind::Expr(expr) => {
                let ty = self.infer_type(expr);
                ty == ValueType::Never
            }
            StmtKind::Block(stmts) => {
                let saved_vars = self.symbols.enter_scope();
                let mut last_returns = false;
                for s in stmts {
                    last_returns = self.check_stmt(s);
                }
                self.symbols.exit_scope(saved_vars);
                // Block returns only if the LAST statement causes a return
                last_returns
            }
            StmtKind::Empty => false,
            StmtKind::Defer(stmt) => {
                self.check_stmt(stmt);
                false
            }
            StmtKind::Select { arms, default } => {
                for arm in arms {
                    self.infer_type(&arm.channel);
                    let saved = self.symbols.enter_scope();
                    self.symbols.variables.insert(arm.var.clone(), (ValueType::Int, false));
                    self.check_stmt(&arm.body);
                    self.symbols.exit_scope(saved);
                }
                if let Some(d) = default {
                    self.check_stmt(d);
                }
                false
            }
            StmtKind::ForC {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(init_stmt) = init {
                    self.check_stmt(init_stmt);
                }
                if let Some(cond_expr) = cond {
                    let ty = self.infer_type(cond_expr);
                    if ty != ValueType::Bool && !matches!(ty, ValueType::Any | ValueType::Named(_, _)) {
                        self.errors.push(
                            TypeError::TypeMismatch {
                                expected: "Bool".to_string(),
                                found: ty.display(),
                                span: cond_expr.span,
                            }
                            .to_error(),
                        );
                    }
                }
                if let Some(update_expr) = update {
                    self.infer_type(update_expr);
                }
                let was_in_loop = self.symbols.in_loop;
                self.symbols.in_loop = true;
                self.check_stmt(body);
                self.symbols.in_loop = was_in_loop;
                false
            }
        }
    }

    fn infer_type(&mut self, expr: &Expr) -> ValueType {
        // Memoize by node id (Bug 50). The same sub-expression is inferred more
        // than once — e.g. a MethodCall infers its receiver directly AND again via
        // check_call, which passes the receiver as the implicit self arg — making
        // inference exponential on deep method chains (`a.n().n()…`) without a
        // cache. Node ids are unique within the single checked source (preludes
        // are only declared, never inferred); synthetic exprs (id 0) aren't cached.
        if expr.id != 0 {
            if let Some(cached) = self.expr_types.get(&expr.id) {
                return cached.clone();
            }
        }
        let ty = self.infer_type_inner(expr);
        if expr.id != 0 {
            self.expr_types.insert(expr.id, ty.clone());
        }
        ty
    }

    fn infer_type_inner(&mut self, expr: &Expr) -> ValueType {
        match &expr.node {
            ExprKind::Literal(lit) => self.literal_type(lit),
            ExprKind::Ident(name) => {
                if let Some((ty, _)) = self.symbols.variables.get(name) {
                    ty.clone()
                } else if self.symbols.functions.contains_key(name) {
                    ValueType::Fn
                } else if self.known_class_names.contains(name) {
                    // Class name used as a value (e.g. DB.of(User)) — treat as Any
                    ValueType::Any
                } else {
                    self.errors
                        .push(TypeError::UndefinedVariable(name.clone(), expr.span).to_error());
                    ValueType::Any
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs_ty = self.infer_type(lhs);
                let rhs_ty = self.infer_type(rhs);
                self.check_binary_op(op, &lhs_ty, &rhs_ty, expr.span);
                Self::binary_result_type(op, &lhs_ty, &rhs_ty)
            }
            ExprKind::Unary { op, operand } => {
                let op_ty = self.infer_type(operand);
                self.check_unary_op(op, &op_ty, expr.span);
                Self::unary_result_type(op, &op_ty)
            }
            ExprKind::Call { func, args } => self.check_call(func, args, expr.span),
            ExprKind::MethodCall { obj, method, args } => {
                // Static method call: ClassName.method(args) — obj is a class/type name, not an instance
                if let ExprKind::Ident(class_name) = &obj.node {
                    let method_key = format!("{}_{}", class_name, method);
                    // Check if it's a known function (static or instance) AND obj is not a variable
                    let obj_is_variable = self.symbols.variables.contains_key(class_name.as_str());
                    if !obj_is_variable && self.symbols.functions.contains_key(&method_key) {
                        let sig = self.symbols.functions.get(&method_key).cloned().unwrap();
                        // Skip 'self' param — ClassName.method(...) never passes self explicitly
                        let skip = usize::from(sig.params.first().map(|(n, _)| n == "self").unwrap_or(false));
                        let expected_params = sig.params[skip..].to_vec();
                        if expected_params.len() != args.len() {
                            self.errors.push(TypeError::InvalidArgumentCount {
                                expected: expected_params.len(),
                                found: args.len(),
                                span: expr.span,
                            }.to_error());
                        }
                        for (arg, (_, expected_ty)) in args.iter().zip(expected_params.iter()) {
                            let arg_ty = self.infer_type(arg);
                            if !self.types_compatible(expected_ty, &arg_ty) {
                                self.errors.push(TypeError::TypeMismatch {
                                    expected: expected_ty.display(),
                                    found: arg_ty.display(),
                                    span: arg.span,
                                }.to_error());
                            }
                        }
                        return sig.return_type.clone();
                    }
                    // Also handle Time_now, etc. even if obj is not registered as variable
                    let is_static = self.symbols.functions.get(&method_key)
                        .map(|sig| sig.params.first().map(|(n, _)| n != "self").unwrap_or(true))
                        .unwrap_or(false);
                    if is_static {
                        let func_expr = Spanned::new(ExprKind::Ident(method_key), expr.span);
                        return self.check_call(&func_expr, args, expr.span);
                    }
                }

                let obj_ty = self.infer_type(obj);
                // Any-typed receiver: dynamic dispatch, no type errors
                if obj_ty == ValueType::Any {
                    for arg in args { self.infer_type(arg); }
                    return ValueType::Any;
                }
                let class_name = obj_ty.to_string();

                // Check if method is a Fn-type field (callable field) on the class
                let field_key = format!("{}.{}", class_name, method);
                if let Some((ValueType::Fn, _)) = self.symbols.variables.get(&field_key) {
                    for arg in args { self.infer_type(arg); }
                    return ValueType::Any;
                }

                let method_name = format!("{}_{}", class_name, method);

                if let Some(vis) = self.method_visibility.get(&method_name).cloned() {
                    self.check_member_visibility(&class_name, method, &vis, expr.span);
                }

                let func_expr = Spanned::new(ExprKind::Ident(method_name.clone()), expr.span);
                let mut call_args = vec![(**obj).clone()];
                call_args.extend(args.iter().cloned());
                // map/filter/forEach/reduce with a lambda argument: bind
                // the lambda param to the array's element type BEFORE
                // check_call. infer_type memoizes per node ID —
                // otherwise check_call would first infer the body with
                // Any params and lock in the poor typing (including for
                // the codegen export).
                let mut array_lambda_ret: Option<ValueType> = None;
                if let ValueType::Array(elem) = &obj_ty {
                    let lam = match method.as_str() {
                        "map" | "filter" | "forEach" => args.first(),
                        "reduce" => args.get(1),
                        _ => None,
                    };
                    if let Some(lam) = lam {
                        let hints: Vec<ValueType> = if method.as_str() == "reduce" {
                            let acc = args
                                .first()
                                .map(|a| self.infer_type(a))
                                .unwrap_or(ValueType::Any);
                            vec![acc, (**elem).clone()]
                        } else {
                            vec![(**elem).clone()]
                        };
                        array_lambda_ret = self.infer_lambda_with_param_hints(lam, &hints);
                    }
                }
                // Issue #165: same pre-binding as the Array map/filter/forEach/
                // reduce case above, generalized to any generic class's own
                // instance method with a Fn-typed param (`Option<T>.andThen`,
                // `.map`, `.orElse`, or a user-defined equivalent) — without
                // this, an arrow-sugar lambda argument's param(s) type-check as
                // `Any` (arrow-sugar has no annotation at all to fall back to),
                // so anything derived from them inside the lambda body — e.g.
                // `Option::some(n.len())`'s own call-site generic-return
                // unification — has no concrete type to unify against either,
                // and the codegen-side inference this feeds ends up with
                // nothing better than its `Int64` default (see
                // `infer_own_type_params` in codegen.rs).
                if let ValueType::Named(cn, targs) = &obj_ty {
                    if let Some(hint_entries) = self.generic_instance_fn_arg_hints.get(&method_name).cloned() {
                        let cn = cn.clone();
                        let targs = targs.clone();
                        for (arg_idx, raw_hints) in hint_entries {
                            if let Some(lam) = args.get(arg_idx) {
                                if matches!(&lam.node, ExprKind::Lambda { .. }) {
                                    let hints: Vec<ValueType> = raw_hints
                                        .iter()
                                        .map(|vt| self.substitute_type_params(vt, &cn, &targs))
                                        .collect();
                                    self.infer_lambda_with_param_hints(lam, &hints);
                                }
                            }
                        }
                    }
                }
                let generic_ret = self.check_call(&func_expr, &call_args, expr.span);
                // #158: check_call has no call-site generic-return
                // unification of its own (unlike check_class_method_call
                // for the static `Class::method(...)` form) — try it here
                // so an own-type-param instance method (`option.map(f)`)
                // resolves its concrete return type (`Option<String>`)
                // from the actual argument, instead of staying the
                // registered signature's unresolved-`U` type for every
                // call regardless of instantiation. No-op (keeps
                // check_call's own result) for anything that isn't a
                // generic method with unresolved params in its return type.
                let generic_ret = self
                    .symbols
                    .functions
                    .get(&method_name)
                    .cloned()
                    .and_then(|sig| self.unify_generic_return(&method_name, &sig.return_type, &call_args, &[]))
                    .unwrap_or(generic_ret);
                // Receiver-dependent result types that static signatures
                // can't express: run check_call first (validates the
                // arguments), then only refine the result.
                match (&obj_ty, method.as_str()) {
                    (ValueType::Map(v), "get") => (**v).clone(),
                    (ValueType::Map(v), "values") => ValueType::Array(v.clone()),
                    (ValueType::Array(e), "first" | "last" | "find" | "max" | "min")
                        if **e != ValueType::Any =>
                    {
                        (**e).clone()
                    }
                    (ValueType::Array(e), "pop" | "sort" | "reverse" | "slice" | "unique"
                        | "take" | "skip" | "toList")
                        if **e != ValueType::Any =>
                    {
                        ValueType::Array(e.clone())
                    }
                    // map: the result element type comes from the lambda's
                    // return type; without usable inference, permissive Array(Any).
                    (ValueType::Array(_), "map") => {
                        let ret_elem = match array_lambda_ret {
                            Some(ValueType::Nothing) | Some(ValueType::Never) | None => {
                                ValueType::Any
                            }
                            Some(t) => t,
                        };
                        ValueType::Array(Box::new(ret_elem))
                    }
                    // filter keeps the input element type.
                    (ValueType::Array(e), "filter") => ValueType::Array(e.clone()),
                    (ValueType::Array(_), "forEach") => ValueType::Nothing,
                    // reduce: the result = the start value's type; falls back to the element type.
                    (ValueType::Array(e), "reduce") => {
                        match args.first().map(|a| self.infer_type(a)) {
                            Some(ValueType::Any) | None => (**e).clone(),
                            Some(t) => t,
                        }
                    }
                    _ => generic_ret,
                }
            }
            ExprKind::Index { obj, index } => {
                let obj_ty = self.infer_type(obj);
                let index_ty = self.infer_type(index);
                let valid_index = match &obj_ty {
                    ValueType::Map(_) => matches!(index_ty, ValueType::String | ValueType::Any),
                    ValueType::String => true,
                    ValueType::Any => true,
                    _ => matches!(index_ty, ValueType::Int | ValueType::Any),
                };
                if !valid_index {
                    self.errors
                        .push(TypeError::IndexNotInteger(expr.span).to_error());
                }
                match obj_ty {
                    ValueType::String => ValueType::String,
                    ValueType::Array(e) => *e,
                    ValueType::Map(v) => *v,
                    _ => ValueType::Any,
                }
            }
            ExprKind::FieldAccess { obj, field } => {
                let obj_ty = self.infer_type(obj);
                if let ValueType::Named(name, targs) = obj_ty {
                    let full_name = format!("{}.{}", name, field);
                    if let Some(vis) = self.field_visibility.get(&full_name).cloned() {
                        self.check_member_visibility(&name, field, &vis, expr.span);
                    }
                    if let Some((ty, _)) = self.symbols.variables.get(&full_name) {
                        // Resolve a T-typed field against the instance's type args
                        // (B2 step 1): `bi: Box<Int64>`, field `value: T` → Int.
                        let ty = ty.clone();
                        return self.substitute_type_params(&ty, &name, &targs);
                    }
                    // Field not declared. Report an error when either the class has
                    // at least one registered field (typo/unknown field), OR it is a
                    // known non-generic class with zero declared fields — the latter
                    // previously fell through silently and let codegen default every
                    // field to i64 (Float/String fields → garbage math). Generic
                    // classes legitimately use struct-literal fields without
                    // registered declarations, so stay permissive there.
                    let has_any_field = self.symbols.variables.keys()
                        .any(|k| k.starts_with(&format!("{}.", name)));
                    let is_known_class = self.known_class_names.contains(&name);
                    let is_generic = self.generic_class_names.contains(&name);
                    if has_any_field || (is_known_class && !is_generic) {
                        self.errors
                            .push(TypeError::FieldNotFound(name, field.clone(), expr.span).to_error());
                    }
                } else if obj_ty != ValueType::Any {
                    self.errors.push(
                        TypeError::InvalidFieldAccess(obj_ty.to_string(), expr.span).to_error(),
                    );
                }
                ValueType::Any
            }
            ExprKind::This => {
                if let Some((ty, _)) = self.symbols.variables.get("self") {
                    ty.clone()
                } else {
                    ValueType::Named("Self".to_string(), vec![])
                }
            }
            ExprKind::SuperCall { method, args } => {
                // super calls are only valid inside a class method
                let current_class = match &self.current_class {
                    Some(c) => c.clone(),
                    None => {
                        self.errors.push(Error::new(
                            expr.span,
                            "super call is only valid inside a class method",
                        ));
                        for arg in args {
                            self.infer_type(arg);
                        }
                        return ValueType::Any;
                    }
                };
                // get the parent class
                let parent_class = match self.class_parents.get(&current_class).cloned() {
                    Some(p) => p,
                    None => {
                        self.errors.push(Error::new(
                            expr.span,
                            format!("class {} has no parent class for super call", current_class),
                        ));
                        for arg in args {
                            self.infer_type(arg);
                        }
                        return ValueType::Any;
                    }
                };
                // look up parent method in symbol table as ParentClass_methodName
                let parent_method_key = format!("{}_{}", parent_class, method);
                let sig = match self.symbols.functions.get(&parent_method_key).cloned() {
                    Some(s) => s,
                    None => {
                        self.errors.push(Error::new(
                            expr.span,
                            format!(
                                "parent class {} has no method '{}'",
                                parent_class, method
                            ),
                        ));
                        for arg in args {
                            self.infer_type(arg);
                        }
                        return ValueType::Any;
                    }
                };
                // sig.params[0] is self; check user-supplied args against params[1..]
                let expected_arg_count = sig.params.len().saturating_sub(1);
                if args.len() != expected_arg_count {
                    self.errors.push(
                        TypeError::InvalidArgumentCount {
                            expected: expected_arg_count,
                            found: args.len(),
                            span: expr.span,
                        }
                        .to_error(),
                    );
                }
                for (arg, (_, expected_ty)) in args.iter().zip(sig.params.iter().skip(1)) {
                    let arg_ty = self.infer_type(arg);
                    if !self.types_compatible(expected_ty, &arg_ty) {
                        self.errors.push(
                            TypeError::TypeMismatch {
                                expected: expected_ty.display(),
                                found: arg_ty.display(),
                                span: arg.span,
                            }
                            .to_error(),
                        );
                    }
                }
                sig.return_type.clone()
            }
            ExprKind::New { class, args, .. } => {
                for arg in args {
                    self.infer_type(arg);
                }
                ValueType::Named(class.clone(), vec![])
            }
            ExprKind::StructLiteral { name, fields } => {
                // B2: for generic classes, unify the type arguments from
                // the field initializers (`Box { value: "x" }` →
                // `Named("Box", [String])`). The binding source is the
                // UNERASED field declaration types (`"Box.value"` →
                // `Named("T")` in symbols.variables). The result only
                // carries args if ALL type params get bound — otherwise
                // exactly the previous behavior (empty args, permissive).
                let tparams = self.class_type_params.get(name).cloned();
                let mut bindings: HashMap<String, ValueType> = HashMap::new();
                for (fname, field_expr) in fields {
                    let ft = self.infer_type(field_expr);
                    if let Some(tps) = &tparams {
                        let decl = self
                            .symbols
                            .variables
                            .get(&format!("{}.{}", name, fname))
                            .map(|(t, _)| t.clone());
                        if let Some(decl_ty) = decl {
                            Self::unify_param(&decl_ty, &ft, tps, &mut bindings);
                        }
                    }
                }
                let targs = match &tparams {
                    Some(tps)
                        if !tps.is_empty()
                            && tps.iter().all(|tp| bindings.contains_key(tp))
                            && !bindings.values().any(|v| self.contains_scoped_type_param(v)) =>
                    {
                        tps.iter().map(|tp| bindings[tp].clone()).collect()
                    }
                    _ => vec![],
                };
                // Bug 130: a struct literal that omits a declared field left the
                // corresponding heap slot as uninitialized garbage at runtime
                // (codegen only stores the fields actually present in the
                // literal) — catch it here instead. Generic classes stay
                // permissive (no registered field declarations to check
                // against, same rationale as the FieldAccess check above).
                if !self.generic_class_names.contains(name) {
                    if let Some(all_fields) = self.class_fields.get(name).cloned() {
                        let given: HashSet<&str> =
                            fields.iter().map(|(n, _)| n.as_str()).collect();
                        let missing: Vec<&str> = all_fields
                            .iter()
                            .map(|s| s.as_str())
                            .filter(|f| !given.contains(f))
                            .collect();
                        if !missing.is_empty() {
                            self.errors.push(Error::new(
                                expr.span,
                                format!(
                                    "struct literal for '{}' is missing field(s): {}",
                                    name,
                                    missing.join(", ")
                                ),
                            ));
                        }
                        let declared: HashSet<&str> =
                            all_fields.iter().map(|s| s.as_str()).collect();
                        for (fname, _) in fields {
                            if !declared.contains(fname.as_str()) {
                                self.errors.push(
                                    TypeError::FieldNotFound(name.clone(), fname.clone(), expr.span)
                                        .to_error(),
                                );
                            }
                        }
                    }
                }
                ValueType::Named(name.clone(), targs)
            }
            ExprKind::Block(stmts) => {
                let saved_vars = self.symbols.enter_scope();
                let mut last_ty = ValueType::Nothing;
                for stmt in stmts {
                    self.check_stmt(stmt);
                }
                if let Some(last) = stmts.last() {
                    if let StmtKind::Expr(e) = &last.node {
                        last_ty = self.infer_type(e);
                    }
                }
                self.symbols.exit_scope(saved_vars);
                last_ty
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_ty = self.infer_type(cond);
                if cond_ty != ValueType::Bool && !matches!(cond_ty, ValueType::Any | ValueType::Named(_, _)) {
                    self.errors.push(
                        TypeError::TypeMismatch {
                            expected: "Bool".to_string(),
                            found: cond_ty.display(),
                            span: cond.span,
                        }
                        .to_error(),
                    );
                }
                let then_ty = self.infer_type(then_branch);
                if let Some(else_br) = else_branch {
                    let else_ty = self.infer_type(else_br);
                    Self::lub(&then_ty, &else_ty)
                } else {
                    ValueType::Nothing
                }
            }
            ExprKind::While { cond, body } => {
                let cond_ty = self.infer_type(cond);
                if cond_ty != ValueType::Bool && !matches!(cond_ty, ValueType::Any | ValueType::Named(_, _)) {
                    self.errors.push(
                        TypeError::TypeMismatch {
                            expected: "Bool".to_string(),
                            found: cond_ty.display(),
                            span: cond.span,
                        }
                        .to_error(),
                    );
                }
                self.infer_type(body);
                ValueType::Nothing
            }
            ExprKind::For { var, iter, body } => {
                let iter_ty = self.infer_type(iter);
                self.symbols.variables.insert(var.clone(), (iter_ty, false));
                self.infer_type(body);
                ValueType::Nothing
            }
            ExprKind::Loop { body } => {
                self.infer_type(body);
                ValueType::Never
            }
            ExprKind::Match { expr, cases } => {
                let scrutinee_ty = self.infer_type(expr);
                let mut result_types = Vec::new();
                for case in cases {
                    let saved = self.symbols.enter_scope();
                    self.bind_pattern_vars(&case.pattern, &scrutinee_ty);
                    let case_ty = self.infer_type(&case.body);
                    self.symbols.exit_scope(saved);
                    result_types.push(case_ty);
                }
                if result_types.is_empty() {
                    return ValueType::Any;
                }
                let mut unified = result_types[0].clone();
                for ty in &result_types[1..] {
                    unified = Self::lub(&unified, ty);
                }
                unified
            }
            ExprKind::Return(Some(val)) => { self.infer_type(val); ValueType::Never }
            ExprKind::Return(None) => ValueType::Never,
            ExprKind::Break => ValueType::Never,
            ExprKind::Continue => ValueType::Never,
            ExprKind::Throw(expr) => {
                self.infer_type(expr);
                ValueType::Never
            }
            ExprKind::Try {
                body,
                catches,
                finally,
            } => {
                self.infer_type(body);
                for catch in catches {
                    self.check_stmt(&catch.body);
                }
                if let Some(finally_body) = finally {
                    self.infer_type(finally_body);
                }
                ValueType::Any
            }
            ExprKind::Assign { target, value } => {
                self.infer_type(target);
                self.infer_type(value)
            }
            ExprKind::CompoundAssign {
                op: _,
                target,
                value,
            } => {
                self.infer_type(target);
                self.infer_type(value)
            }
            ExprKind::Lambda {
                params,
                ret_type,
                body,
            } => {
                let saved_vars = self.symbols.enter_scope();
                for param in params {
                    self.symbols.variables.insert(
                        param.name.clone(),
                        (Self::type_to_value(&param.param_type), false),
                    );
                }
                // Return statements in the lambda body belong to the lambda,
                // not the enclosing function: check them against the lambda's
                // annotated return type, or not at all if unannotated.
                let lambda_ret = ret_type.as_ref().map(Self::type_to_value);
                let saved_return_type =
                    std::mem::replace(&mut self.current_return_type, lambda_ret);
                let body_ty = self.infer_type(body);
                self.current_return_type = saved_return_type;
                self.symbols.exit_scope(saved_vars);
                let _ret = ret_type
                    .as_ref()
                    .map(Self::type_to_value)
                    .unwrap_or(body_ty);
                ValueType::Fn
            }
            ExprKind::Spawn(inner) => {
                self.infer_type(inner);
                ValueType::Int // task handle (opaque i64)
            }
            ExprKind::Await(inner) => {
                self.infer_type(inner);
                ValueType::Int // task result
            }
            ExprKind::Channel => ValueType::Int, // channel handle (opaque i64)
            ExprKind::Send { channel, value } => {
                self.infer_type(channel);
                self.infer_type(value);
                ValueType::Nothing
            }
            ExprKind::Recv(inner) => {
                let ch_ty = self.infer_type(inner);
                // `channel`'s own inferred type is always a bare Int
                // (opaque handle, see ExprKind::Channel above — there's no
                // element type to infer at creation time), but the
                // variable/field/parameter it's assigned/declared into
                // carries the real `Channel<T>` annotation (ValueType::
                // Named("Channel", [T]), via ValueType::from_parser_type's
                // generic-type fallback). Recover T from there so `recv`
                // on a `Channel<SomeClass>` (not just `Channel<Int64>`,
                // which happened to already work since Int was the
                // correct answer by coincidence) type-checks its result
                // as the real element type instead of always Int64 —
                // issue #123 needed this for `Channel<AmqpFrame091>`.
                if let ValueType::Named(name, args) = &ch_ty {
                    if name == "Channel" && args.len() == 1 {
                        return args[0].clone();
                    }
                }
                ValueType::Int // unknown channel element type — unchanged fallback
            }
            ExprKind::Cast { expr, ty } => {
                self.infer_type(expr);
                Self::type_to_value(ty)
            }
            ExprKind::Is { expr, ty } => {
                self.infer_type(expr);
                Self::type_to_value(ty);
                ValueType::Bool
            }
            ExprKind::Range {
                start,
                end,
                inclusive: _,
            } => {
                let start_ty = self.infer_type(start);
                let end_ty = self.infer_type(end);
                if !matches!(start_ty, ValueType::Int) || !matches!(end_ty, ValueType::Int) {
                    self.errors.push(
                        TypeError::InvalidRangeType(
                            format!("{}..{}", start_ty, end_ty),
                            expr.span,
                        )
                        .to_error(),
                    );
                }
                ValueType::Range
            }
            ExprKind::Tuple(exprs) => {
                for e in exprs {
                    self.infer_type(e);
                }
                ValueType::Tuple
            }
            ExprKind::TupleIndex { tuple, .. } => {
                self.infer_type(tuple);
                ValueType::Any
            }
            ExprKind::EnumValue {
                enum_name,
                variant,
                type_args,
                args,
            } => {
                // Check if this is actually a static method call: ClassName::method(args).
                // Also covers non-static (`fn`) methods called this way with an implicit
                // self (the codegen convention throughout the stdlib, e.g. Debug::
                // memoryUsage()) — skip the synthetic leading "self" param the same way
                // the MethodCall arm below does, instead of falling through to the
                // enum-variant fallback (which misread every zero-arg instance method
                // called via `::` as a bare enum-variant construction returning
                // Named(ClassName): "expected Int64, found Debug").
                if let Some(ty) = self.check_class_method_call(enum_name, variant, args, type_args, expr.span) {
                    return ty;
                }
                // Type check all arguments
                for arg in args {
                    self.infer_type(arg);
                }
                let is_known_enum = self.enums.contains_key(enum_name.as_str());
                let is_known_class = self.known_class_names.contains(enum_name.as_str());
                // A type parameter in scope (`T::fromJson()` in a generic fn/class):
                // the concrete type is only known after monomorphization, so we
                // cannot resolve the static method here — stay permissive.
                let is_type_param = self.type_param_scope.contains(enum_name.as_str());
                // `Name::member` resolves to nothing: not a registered static/
                // instance method (checked above via static_key), not a known enum,
                // not a known class, not a type parameter. Previously this silently
                // returned Any (with args) or Named(enum_name) (without) and let
                // codegen build garbage — the "silent-garbage" failure mode. Now a
                // hard error (a typical trigger: a missing `import` of the
                // class, e.g. Strings::/Mathf::).
                if !is_known_enum && !is_known_class && !is_type_param {
                    self.errors.push(
                        TypeError::UnresolvedStaticPath {
                            name: enum_name.clone(),
                            member: variant.clone(),
                            span: expr.span,
                        }
                        .to_error(),
                    );
                    return ValueType::Any;
                }
                // Known NON-generic class but no method by that name (checked via
                // static_key above; inherited + generic-own methods are registered
                // too) → the method genuinely does not exist: typo or missing
                // definition. Hard error (Bug 43) instead of the old silent Any.
                let is_generic_class = self.generic_class_names.contains(enum_name.as_str());
                if is_known_class && !is_generic_class && !is_type_param {
                    self.errors.push(
                        TypeError::UnknownStaticMethod {
                            class: enum_name.clone(),
                            method: variant.clone(),
                            span: expr.span,
                        }
                        .to_error(),
                    );
                    return ValueType::Any;
                }
                // Generic class or type parameter: the method may only resolve
                // after monomorphization — stay permissive (return Any).
                if !is_known_enum && (is_known_class || is_type_param) {
                    return ValueType::Any;
                }
                // Known enum → enum-variant construction. Validate the variant
                // exists (Bug 45): a typo like `Color::Purpel` previously returned
                // Named(Color) and built a bogus value.
                if let Some(variants) = self.enums.get(enum_name.as_str()) {
                    if !variants.contains(variant) {
                        self.errors.push(
                            TypeError::UnknownEnumVariant {
                                enum_name: enum_name.clone(),
                                variant: variant.clone(),
                                span: expr.span,
                            }
                            .to_error(),
                        );
                    }
                }
                ValueType::Named(enum_name.clone(), vec![])
            }
            ExprKind::ArrayLiteral(elements) => {
                // Element type as the lub over all elements (empty → Any)
                let mut elem: Option<ValueType> = None;
                for e in elements {
                    let t = self.infer_type(e);
                    elem = Some(match elem {
                        Some(acc) => Self::lub(&acc, &t),
                        None => t,
                    });
                }
                ValueType::Array(Box::new(elem.unwrap_or(ValueType::Any)))
            }
            ExprKind::MapLiteral(entries) => {
                // Value type as the lub over all values (empty → Any)
                let mut val: Option<ValueType> = None;
                for (k, v) in entries {
                    self.infer_type(k);
                    let t = self.infer_type(v);
                    val = Some(match val {
                        Some(acc) => Self::lub(&acc, &t),
                        None => t,
                    });
                }
                ValueType::Map(Box::new(val.unwrap_or(ValueType::Any)))
            }
        }
    }

    /// Resolves a call against the static/instance method registered under
    /// `"{class_name}_{method_name}"` in `symbols.functions` — the same
    /// mangled key both `fnc`/`fn` class methods register under (see
    /// `check_class`) and `ClassName::method(...)` calls look up. Shared by
    /// the `ExprKind::EnumValue` (`::`) call path and `check_call`'s
    /// same-class bare-name fallback (issue #149 stage 2) so both apply the
    /// identical self-vs-static argument-count rules (Bug 46/47/38) instead
    /// of two copies of this logic drifting apart. Returns `None` when no
    /// such method is registered, letting the caller fall through to
    /// whatever it does next (enum-variant construction, "undefined
    /// function", etc.) — this function does not itself report "not
    /// found".
    fn check_class_method_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[Expr],
        type_args: &[Type],
        span: Span,
    ) -> Option<ValueType> {
        let static_key = format!("{}_{}", class_name, method_name);
        let sig = self.symbols.functions.get(&static_key).cloned()?;
        for arg in args {
            self.infer_type(arg);
        }
        // Argument-count check (Bug 46/47). Instance methods (`fn`) carry
        // a leading synthetic "self" param and are called via one of two
        // Bug-38 styles. The receiver-as-self vs receiver-as-explicit-
        // param distinction is NOT statically decidable in general — a
        // method that ignores its receiver (`fn label() { return "x"; }`,
        // called `C::label(obj)`) and a pure namespace helper
        // (`Hex::encode(data)`, called with no object) both lack `this`
        // yet need different counts. So stay permissive there. But a
        // method that USES `this` provably needs the receiver → the count
        // is exactly declared+1 (Bug 47: catches `C::m()` forgetting the
        // object, which would deref a null self at runtime).
        // Static methods (`fnc`) have no self: exactly declared.
        let is_instance = sig.params.first().map(|(n, _)| n == "self").unwrap_or(false);
        let declared = if is_instance { sig.params.len().saturating_sub(1) } else { sig.params.len() };
        let uses_this = is_instance && self.method_uses_this.contains(&static_key);
        let count_ok = if !is_instance {
            args.len() == declared
        } else if uses_this {
            args.len() == declared + 1
        } else {
            args.len() == declared || args.len() == declared + 1
        };
        if !count_ok {
            let expected = if uses_this { declared + 1 } else { declared };
            self.errors.push(
                TypeError::InvalidArgumentCount {
                    expected,
                    found: args.len(),
                    span,
                }
                .to_error(),
            );
        }
        // B2 step 2 / #158 — type-argument inference for unannotated
        // bindings: `let bi = Box::make(42)` derives T=Int from the args
        // → return type `Named("Box", [Int])` instead of the registered
        // form with an unresolved `Named("T")` arg.
        if let Some(resolved) = self.unify_generic_return(&static_key, &sig.return_type, args, type_args) {
            return Some(resolved);
        }
        Some(sig.return_type.clone())
    }

    /// Call-site generic-return unification, shared by
    /// `check_class_method_call` (the static `Class::method(...)` form)
    /// and the instance-call `ExprKind::MethodCall` arm below (#158) — the
    /// latter used to have NO equivalent at all, so an own-type-param
    /// instance method's return type (`Option<T>.map<U>(...) ->
    /// Option<U>`) stayed the registered, unresolved-`U` signature type
    /// for every instance call, regardless of the actual argument. Only
    /// active when the registered return type actually contains one of
    /// `static_key`'s type params AND unification resolves ALL of them;
    /// otherwise `None`, and the caller keeps its prior behavior
    /// (`sig.return_type` unchanged).
    fn unify_generic_return(
        &mut self,
        static_key: &str,
        sig_return_type: &ValueType,
        args: &[Expr],
        explicit_type_args: &[Type],
    ) -> Option<ValueType> {
        let (param_tys, tparams) = self.generic_method_param_types.get(static_key).cloned()?;
        if !Self::contains_type_param(sig_return_type, &tparams) {
            return None;
        }
        let arg_tys: Vec<ValueType> = args.iter().map(|a| self.infer_type(a)).collect();
        // Receiver alignment (Bug 38, two call styles): args line up 1:1
        // with params (receiver included) or are shifted by the implicit
        // self param (namespace style).
        let aligned: Option<&[ValueType]> = if arg_tys.len() == param_tys.len() {
            Some(&param_tys[..])
        } else if arg_tys.len() + 1 == param_tys.len() {
            Some(&param_tys[1..])
        } else {
            None
        };
        let mut bindings: HashMap<String, ValueType> = HashMap::new();
        // Bug 166: explicit `Class<T>::method(...)` type args take
        // priority over value-argument-driven inference below -- seeded
        // first (positionally against the method's own type params), so
        // a method whose params never mention T at all (e.g.
        // `Result::err(message: String) -> Result<T>`, T appears only in
        // the return type) can still resolve instead of falling through
        // unresolved. `unify_param` below uses `entry().or_insert_with`,
        // so it can only fill gaps this leaves, never overwrite an
        // explicit binding with an inferred one.
        for (name, ty) in tparams.iter().zip(explicit_type_args.iter()) {
            bindings.insert(name.clone(), Self::type_to_value(ty));
        }
        if let Some(ps) = aligned {
            for (p, a) in ps.iter().zip(arg_tys.iter()) {
                Self::unify_param(p, a, &tparams, &mut bindings);
            }
        }
        let resolved = Self::substitute_bindings(sig_return_type, &bindings);
        if !Self::contains_type_param(&resolved, &tparams) && !self.contains_scoped_type_param(&resolved) {
            Some(resolved)
        } else {
            None
        }
    }

    fn check_call(&mut self, func: &Expr, args: &[Expr], span: Span) -> ValueType {
        // Check if it's a simple identifier - could be function or lambda variable
        if let ExprKind::Ident(name) = &func.node {
            // First check if it's a defined function
            if let Some(sig) = self.symbols.functions.get(name).cloned() {
                // #164: `.join()` assumes every array element is a string
                // pointer at the runtime level (`tinox_string_join`) —
                // calling it on a `List<T>` where T != String reinterprets
                // raw element values (e.g. Int64s) as pointers and
                // segfaults. Reject at compile time instead of letting it
                // crash. Covers BOTH call spellings in one place: instance
                // syntax (`arr.join(sep)`) rewrites to `Ident("Array_join")`
                // with the receiver prepended to `args` before reaching
                // here (see the `MethodCall` arm above), and the bare
                // free-function spelling (`join(arr, sep)`/
                // `Array_join(arr, sep)`) arrives here directly — so
                // `args[0]` is the array in both cases. Stays permissive
                // for `List<Any>` (element type genuinely unknown), same
                // convention as the array-method refinements in the
                // `MethodCall` arm above. The registered signature itself
                // (`arr: any_array()`) is deliberately permissive for
                // element type, so this can't be caught by the generic
                // arg-type-compatibility loop below.
                if (name == "join" || name == "Array_join") && !args.is_empty() {
                    if let ValueType::Array(elem) = self.infer_type(&args[0]) {
                        if *elem != ValueType::String && *elem != ValueType::Any {
                            self.errors.push(
                                TypeError::TypeMismatch {
                                    expected: "List<String>".to_string(),
                                    found: format!("List<{}>", elem.display()),
                                    span: args[0].span,
                                }
                                .to_error(),
                            );
                        }
                    }
                }
                // Skip arg-count check for variadic builtins and all native runtime functions
                let is_variadic = matches!(name.as_str(), "print" | "println" | "open")
                    || name.starts_with("http") || name.starts_with("Http")
                    || name.starts_with("socket") || name.starts_with("Socket")
                    || name.starts_with("env") || name.starts_with("Env")
                    || name.starts_with("regex") || name.starts_with("Regex")
                    || name.starts_with("zip") || name.starts_with("xml")
                    || name.starts_with("uri") || name.starts_with("uuid")
                    || name.starts_with("random") || name.starts_with("process")
                    || name.starts_with("dir") || name.starts_with("file")
                    || name.starts_with("Pool_") || name.starts_with("Heap_")
                    || name.starts_with("String_")
                    || matches!(name.as_str(),
                        "now" | "sleep" | "fromCharCode" | "charCodeAt"
                        | "sha256Hash" | "md5Hash" | "sha1Hash" | "wsAcceptKey" | "hmacSha256Hash"
                        | "aesEncryptRaw" | "aesDecryptRaw"
                        | "base64Encode" | "base64Decode" | "base64EncodeChar"
                        | "gcCollect" | "memoryUsage" | "printStackTrace" | "processExit"
                        | "sinf" | "cosf" | "tanf" | "logf" | "log10f" | "sqrtf" | "expf" | "powf"
                        | "fabsf" | "floorf" | "ceilf"
                    );
                if !is_variadic && sig.params.len() != args.len() {
                    self.errors.push(
                        TypeError::InvalidArgumentCount {
                            expected: sig.params.len(),
                            found: args.len(),
                            span,
                        }
                        .to_error(),
                    );
                }
                if is_variadic {
                    for arg in args { self.infer_type(arg); }
                    return sig.return_type.clone();
                }
                for (arg, (_, expected_ty)) in args.iter().zip(sig.params.iter()) {
                    let arg_ty = self.infer_type(arg);
                    if !self.types_compatible(expected_ty, &arg_ty) {
                        self.errors.push(
                            TypeError::TypeMismatch {
                                expected: expected_ty.display(),
                                found: arg_ty.display(),
                                span: arg.span,
                            }
                            .to_error(),
                        );
                    }
                }
                return sig.return_type.clone();
            }

            // Check if it's a variable with Fn type (lambda)
            if let Some((ty, _)) = self.symbols.variables.get(name) {
                if *ty == ValueType::Fn {
                    // Lambda call - just check arguments are present
                    for arg in args {
                        self.infer_type(arg);
                    }
                    return ValueType::Any; // We don't have detailed lambda type info
                }
            }

            // Same-class bare `fnc` call (issue #149 stage 2): a sibling
            // STATIC method of the class currently being type-checked,
            // called without a `ClassName::` qualifier — e.g. `helper()`
            // instead of `Main::helper()` from inside another method of
            // `Main`. Deliberately scoped to `self.current_class` only, not
            // a global search across all classes: a bare name must never
            // silently resolve to some OTHER class's method just because
            // it happens to share a name. Deliberately static-only, not
            // instance methods: an instance method called bare would need
            // an implicit `this` receiver threaded through (Java-style),
            // a different, not-yet-built feature — instance methods still
            // require an explicit `this.method()` receiver as before.
            if let Some(class_name) = self.current_class.clone() {
                let static_key = format!("{}_{}", class_name, name);
                let is_static = self
                    .symbols
                    .functions
                    .get(&static_key)
                    .map(|sig| sig.params.first().map(|(n, _)| n != "self").unwrap_or(true))
                    .unwrap_or(false);
                if is_static {
                    if let Some(ty) = self.check_class_method_call(&class_name, name, args, &[], span) {
                        return ty;
                    }
                }
            }

            // Not found
            self.errors
                .push(TypeError::UndefinedFunction(name.clone(), span).to_error());
        } else {
            // Not an identifier - could be complex expression returning Fn
            let func_ty = self.infer_type(func);
            if func_ty == ValueType::Fn {
                for arg in args {
                    self.infer_type(arg);
                }
                return ValueType::Any;
            }
        }

        for arg in args {
            self.infer_type(arg);
        }
        ValueType::Any
    }

    /// Binds pattern variables with the type of the matched value:
    /// enum-payload arguments get the declared payload types (from
    /// enum_variant_payloads), top-level idents get the scrutinee type.
    /// Unknowns stay Any.
    fn bind_pattern_vars(&mut self, pattern: &Pattern, scrutinee: &ValueType) {
        match pattern {
            Pattern::Ident(name, inner, _) => {
                self.symbols
                    .variables
                    .insert(name.clone(), (scrutinee.clone(), false));
                if let Some(inner) = inner {
                    self.bind_pattern_vars(inner, scrutinee);
                }
            }
            Pattern::EnumVariant { enum_name, variant, args, .. } => {
                // Nacktes Pattern `Arr(xs)`: Variantenname steht in enum_name,
                // variant ist leer; qualifiziert `JV::Arr(xs)` ist beides gesetzt.
                let variant_name = if variant.is_empty() { enum_name } else { variant };
                let payloads = match scrutinee {
                    ValueType::Named(e, _) => self
                        .enum_variant_payloads
                        .get(&format!("{}::{}", e, variant_name))
                        .cloned(),
                    _ => None,
                };
                for (i, arg) in args.iter().enumerate() {
                    let arg_ty = payloads
                        .as_ref()
                        .and_then(|ps| ps.get(i).cloned())
                        .unwrap_or(ValueType::Any);
                    self.bind_pattern_vars(arg, &arg_ty);
                }
            }
            Pattern::Tuple(pats, _) => {
                for p in pats {
                    self.bind_pattern_vars(p, &ValueType::Any);
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal(_, _) => {}
        }
    }

    fn check_binary_op(&mut self, op: &BinaryOp, lhs: &ValueType, rhs: &ValueType, span: Span) {
        // Any and Named (generic type params) are wildcards — skip checking
        if matches!(lhs, ValueType::Any | ValueType::Named(_, _))
            || matches!(rhs, ValueType::Any | ValueType::Named(_, _))
        {
            return;
        }
        let valid = match op {
            BinaryOp::Add => {
                (matches!(lhs, ValueType::Int | ValueType::Float)
                    && matches!(rhs, ValueType::Int | ValueType::Float))
                    || (matches!(lhs, ValueType::String) && matches!(rhs, ValueType::String))
            }
            BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::ShrArith => {
                matches!(lhs, ValueType::Int | ValueType::Float)
                    && matches!(rhs, ValueType::Int | ValueType::Float)
            }
            BinaryOp::And | BinaryOp::Or => {
                matches!(lhs, ValueType::Bool) && matches!(rhs, ValueType::Bool)
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::Xor => {
                matches!(lhs, ValueType::Int) && matches!(rhs, ValueType::Int)
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                // Reference types (stored as pointers) may be compared to null.
                // Named/Any already short-circuit above; this covers Map/Array/
                // String/Fn/Nullable == null.
                lhs == rhs
                    || (matches!(lhs, ValueType::Null) && Self::is_nullable_ref(rhs))
                    || (matches!(rhs, ValueType::Null) && Self::is_nullable_ref(lhs))
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                (matches!(lhs, ValueType::Int | ValueType::Float)
                    && matches!(rhs, ValueType::Int | ValueType::Float))
                    || (matches!(lhs, ValueType::String) && matches!(rhs, ValueType::String))
            }
        };
        if !valid {
            self.errors.push(
                TypeError::BinaryOpTypeMismatch {
                    op: format!("{:?}", op).to_lowercase(),
                    lhs: lhs.to_string(),
                    rhs: rhs.to_string(),
                    span,
                }
                .to_error(),
            );
        }
    }

    /// Reference-ish types that live behind a pointer and can therefore be
    /// meaningfully compared to `null`. Scalars (Int/Float/Bool/Char) cannot.
    fn is_nullable_ref(t: &ValueType) -> bool {
        matches!(
            t,
            ValueType::Array(_)
                | ValueType::Map(_)
                | ValueType::Named(_, _)
                | ValueType::String
                | ValueType::Nullable(_)
                | ValueType::Null
                | ValueType::Fn
                | ValueType::Ref
        )
    }

    fn check_unary_op(&mut self, op: &UnaryOp, operand: &ValueType, span: Span) {
        if matches!(operand, ValueType::Any | ValueType::Named(_, _)) {
            return;
        }
        let valid = match op {
            UnaryOp::Neg => matches!(operand, ValueType::Int | ValueType::Float),
            UnaryOp::Not => matches!(operand, ValueType::Bool),
            UnaryOp::BitNot => matches!(operand, ValueType::Int),
        };
        if !valid {
            self.errors.push(
                TypeError::UnaryOpTypeMismatch {
                    op: format!("{:?}", op).to_lowercase(),
                    operand: operand.to_string(),
                    span,
                }
                .to_error(),
            );
        }
    }

    fn binary_result_type(op: &BinaryOp, lhs: &ValueType, rhs: &ValueType) -> ValueType {
        match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => ValueType::Bool,
            BinaryOp::And | BinaryOp::Or => ValueType::Bool,
            BinaryOp::Add if *lhs == ValueType::String || *rhs == ValueType::String => {
                ValueType::String
            }
            _ => {
                if matches!(lhs, ValueType::Any | ValueType::Named(_, _))
                    || matches!(rhs, ValueType::Any | ValueType::Named(_, _))
                {
                    ValueType::Any
                } else if *lhs == ValueType::Float || *rhs == ValueType::Float {
                    ValueType::Float
                } else {
                    ValueType::Int
                }
            }
        }
    }

    fn unary_result_type(op: &UnaryOp, operand: &ValueType) -> ValueType {
        match op {
            UnaryOp::Neg => operand.clone(),
            UnaryOp::Not => ValueType::Bool,
            UnaryOp::BitNot => ValueType::Int,
        }
    }

    fn literal_type(&self, lit: &Literal) -> ValueType {
        match lit {
            Literal::Integer(_) => ValueType::Int,
            Literal::Float(_) => ValueType::Float,
            Literal::String(_) => ValueType::String,
            Literal::Char(_) => ValueType::Char,
            Literal::Byte(_) => ValueType::Int,
            Literal::Bool(_) => ValueType::Bool,
            Literal::Null => ValueType::Null,
        }
    }

    fn type_to_value(ty: &Type) -> ValueType {
        ValueType::from_parser_type(ty)
    }

    /// Infers a lambda literal's return type with param-type hints
    /// (Array map/filter/forEach/reduce): every unannotated param is
    /// bound to its hint, then the body is inferred (memoized per node
    /// ID — the rich typing is preserved for later inference and the
    /// codegen export). None if the expression isn't a lambda.
    fn infer_lambda_with_param_hints(
        &mut self,
        expr: &Expr,
        hints: &[ValueType],
    ) -> Option<ValueType> {
        let ExprKind::Lambda { params, ret_type, body } = &expr.node else {
            return None;
        };
        let saved_vars = self.symbols.enter_scope();
        for (i, p) in params.iter().enumerate() {
            let declared = Self::type_to_value(&p.param_type);
            let bound = if declared == ValueType::Any {
                hints.get(i).cloned().unwrap_or(ValueType::Any)
            } else {
                declared
            };
            self.symbols.variables.insert(p.name.clone(), (bound, false));
        }
        // Return statements in the body belong to the lambda (same as
        // in infer_type_inner's Lambda arm), not to the enclosing function.
        let lambda_ret = ret_type.as_ref().map(Self::type_to_value);
        let saved_ret = std::mem::replace(&mut self.current_return_type, lambda_ret.clone());
        let body_ty = self.infer_type(body);
        self.current_return_type = saved_ret;
        self.symbols.exit_scope(saved_vars);
        Some(lambda_ret.unwrap_or(body_ty))
    }

    /// Substitute a class's type parameters with concrete instance type args
    /// (B2 step 1). E.g. field type `Named("T", [])` of class `Box` with args
    /// `[Int]` becomes `Int`. Recurses into Array/Map/Nullable so `List<T>`
    /// resolves too. A no-op when there are no args or the class isn't generic.
    fn substitute_type_params(&self, ty: &ValueType, class_name: &str, targs: &[ValueType]) -> ValueType {
        if targs.is_empty() {
            return ty.clone();
        }
        let Some(tparams) = self.class_type_params.get(class_name) else {
            return ty.clone();
        };
        match ty {
            ValueType::Named(n, _) => {
                if let Some(idx) = tparams.iter().position(|p| p == n) {
                    targs.get(idx).cloned().unwrap_or_else(|| ty.clone())
                } else {
                    ty.clone()
                }
            }
            ValueType::Array(inner) => {
                ValueType::Array(Box::new(self.substitute_type_params(inner, class_name, targs)))
            }
            ValueType::Map(v) => {
                ValueType::Map(Box::new(self.substitute_type_params(v, class_name, targs)))
            }
            ValueType::Nullable(inner) => {
                ValueType::Nullable(Box::new(self.substitute_type_params(inner, class_name, targs)))
            }
            _ => ty.clone(),
        }
    }

    /// Like `type_to_value` but resolves type parameters in scope to `Any`.
    fn resolve_type(&self, ty: &Type) -> ValueType {
        match ty {
            Type::Named(name) if self.type_param_scope.contains(name) => ValueType::Any,
            Type::Generic { name, .. } if self.type_param_scope.contains(name) => ValueType::Any,
            _ => Self::type_to_value(ty),
        }
    }

    /// Used during registration to erase type parameters to `Any`.
    /// B2 step 2 — type-argument inference at the call site: unifies an
    /// UNERASED param type against the inferred arg type and collects
    /// bindings for type params (`v: T` against `42: Int` → T=Int).
    /// `Any` as an arg carries no information and doesn't bind (the
    /// fallback stays permissive). The first binding wins (no conflict
    /// check — for contradictory args, the normal check reports the
    /// error elsewhere).
    fn unify_param(
        param: &ValueType,
        arg: &ValueType,
        tparams: &[String],
        bindings: &mut HashMap<String, ValueType>,
    ) {
        match (param, arg) {
            (ValueType::Named(n, _), _) if tparams.contains(n) => {
                if !matches!(arg, ValueType::Any) {
                    bindings.entry(n.clone()).or_insert_with(|| arg.clone());
                }
            }
            (ValueType::Named(pn, pargs), ValueType::Named(an, aargs)) if pn == an => {
                for (p, a) in pargs.iter().zip(aargs.iter()) {
                    Self::unify_param(p, a, tparams, bindings);
                }
            }
            (ValueType::Array(p), ValueType::Array(a)) => Self::unify_param(p, a, tparams, bindings),
            (ValueType::Map(p), ValueType::Map(a)) => Self::unify_param(p, a, tparams, bindings),
            (ValueType::Nullable(p), ValueType::Nullable(a)) => {
                Self::unify_param(p, a, tparams, bindings)
            }
            (ValueType::Nullable(p), a) => Self::unify_param(p, a, tparams, bindings),
            _ => {}
        }
    }

    /// Ersetzt gebundene Typ-Params in einem Typ (`Named("Box", [Named("T")])`
    /// mit T=Int → `Named("Box", [Int])`). Ungebundene bleiben stehen.
    fn substitute_bindings(ty: &ValueType, bindings: &HashMap<String, ValueType>) -> ValueType {
        match ty {
            ValueType::Named(n, args) => {
                if let Some(bound) = bindings.get(n) {
                    bound.clone()
                } else {
                    ValueType::Named(
                        n.clone(),
                        args.iter().map(|a| Self::substitute_bindings(a, bindings)).collect(),
                    )
                }
            }
            ValueType::Array(inner) => {
                ValueType::Array(Box::new(Self::substitute_bindings(inner, bindings)))
            }
            ValueType::Map(v) => ValueType::Map(Box::new(Self::substitute_bindings(v, bindings))),
            ValueType::Nullable(inner) => {
                ValueType::Nullable(Box::new(Self::substitute_bindings(inner, bindings)))
            }
            _ => ty.clone(),
        }
    }

    /// Does the type still contain an unresolved type param?
    fn contains_type_param(ty: &ValueType, tparams: &[String]) -> bool {
        match ty {
            ValueType::Named(n, args) => {
                tparams.contains(n) || args.iter().any(|a| Self::contains_type_param(a, tparams))
            }
            ValueType::Array(inner) | ValueType::Map(inner) | ValueType::Nullable(inner) => {
                Self::contains_type_param(inner, tparams)
            }
            _ => false,
        }
    }

    /// Does the type contain a type param of the ENCLOSING scope
    /// (`Named("U")` inside `Holder<U>`'s body)? Such bindings only
    /// become concrete after monomorphization — if passed along as
    /// "resolved", codegen would mangle a silent wrong specialization
    /// out of it (U → i64*).
    fn contains_scoped_type_param(&self, ty: &ValueType) -> bool {
        match ty {
            ValueType::Named(n, args) => {
                self.type_param_scope.contains(n)
                    || args.iter().any(|a| self.contains_scoped_type_param(a))
            }
            ValueType::Array(inner) | ValueType::Map(inner) | ValueType::Nullable(inner) => {
                self.contains_scoped_type_param(inner)
            }
            _ => false,
        }
    }

    fn type_to_value_erasing(ty: &Type, type_params: &[String]) -> ValueType {
        match ty {
            Type::Named(name) if type_params.contains(name) => ValueType::Any,
            Type::Generic { name, .. } if type_params.contains(name) => ValueType::Any,
            // Recurse into container types so a type param nested in the element
            // is erased too — e.g. `List<T>` → Array(Any), not Array(Named("T")),
            // otherwise a generic method returning `List<T>` fails to unify with
            // a concrete `List<Int64>` at the call site.
            Type::Array(inner) => {
                ValueType::Array(Box::new(Self::type_to_value_erasing(inner, type_params)))
            }
            Type::Generic { name, args } if (name == "List" || name == "Array") && args.len() == 1 => {
                ValueType::Array(Box::new(Self::type_to_value_erasing(&args[0], type_params)))
            }
            Type::Generic { name, args } if name == "Map" && args.len() == 2 => {
                ValueType::Map(Box::new(Self::type_to_value_erasing(&args[1], type_params)))
            }
            Type::Map(_, v) => {
                ValueType::Map(Box::new(Self::type_to_value_erasing(v, type_params)))
            }
            _ => Self::type_to_value(ty),
        }
    }

    /// Does `ty` reference any of `type_params` anywhere in its structure
    /// (not just at the top level)? Used by `erase_method_return_type`
    /// below — see #158.
    fn type_references_param(ty: &Type, type_params: &[String]) -> bool {
        match ty {
            Type::Named(n) => type_params.contains(n),
            Type::Generic { name, args } => {
                type_params.contains(name) || args.iter().any(|a| Self::type_references_param(a, type_params))
            }
            Type::Array(inner) | Type::Ref(inner) | Type::Mutable(inner) | Type::Nullable(inner) => {
                Self::type_references_param(inner, type_params)
            }
            Type::Map(k, v) => {
                Self::type_references_param(k, type_params) || Self::type_references_param(v, type_params)
            }
            Type::Fn { params, ret } => {
                params.iter().any(|p| Self::type_references_param(p, type_params))
                    || Self::type_references_param(ret, type_params)
            }
            Type::Tuple(ts) => ts.iter().any(|t| Self::type_references_param(t, type_params)),
            _ => false,
        }
    }

    /// Return-type-specific extension of `type_to_value_erasing`, used
    /// ONLY for a method's registered return type (#158): a user-defined
    /// generic class's own type argument (`Option<U>`, `Box<U>`, …) that
    /// mentions a type param owned by the METHOD ITSELF — not the
    /// enclosing class — erases to `Any` wholesale, on top of
    /// `type_to_value_erasing`'s existing behavior.
    ///
    /// Deliberately narrower than erasing on ANY type-param match found
    /// anywhere: the enclosing CLASS's own type param (`Option<T>`'s `T`
    /// in `fn some(value: T) -> Option<T>`) must stay structurally intact
    /// (`Named("Option", [Named("T")])`) here, because
    /// `check_class_method_call`'s call-site unification
    /// (`unify_generic_return`) needs that literal `Named("T")` PRESENT
    /// to even attempt resolving it from the receiver's actual type —
    /// erasing it to `Any` breaks that already-working resolution
    /// (confirmed empirically: erasing on ANY match regressed
    /// `Option<Int64>::some(1).unwrap()`, a plain non-chained call with
    /// no own-type-param method involved at all, into the exact same ICE
    /// this fix exists to remove).
    ///
    /// A method-own type param mentioned this way (typically nested
    /// inside an `fnc(T) -> U`-shaped parameter, e.g. `map`/`andThen`)
    /// has no such existing resolution path to protect — `ValueType::Fn`
    /// carries no parameter/return substructure for `unify_param` to
    /// unify against in the first place, so erasing to `Any` costs
    /// nothing there and instead makes `valuetype_to_marker`
    /// (tinox-codegen) correctly return `None` for the call-site node,
    /// falling back to codegen's own per-call-node marker inference
    /// (`infer_own_type_params` / `methodcall_result_markers`, #153).
    fn erase_method_return_type(ret_type: &Type, method_own_params: &[String], erase_params: &[String]) -> ValueType {
        if !method_own_params.is_empty()
            && matches!(ret_type, Type::Generic { .. })
            && Self::type_references_param(ret_type, method_own_params)
        {
            return ValueType::Any;
        }
        Self::type_to_value_erasing(ret_type, erase_params)
    }

    fn is_subclass_or_equal(&self, candidate: &str, base: &str) -> bool {
        if candidate == base {
            return true;
        }
        let mut current = candidate.to_string();
        while let Some(parent) = self.class_parents.get(&current) {
            if parent == base {
                return true;
            }
            current = parent.clone();
        }
        false
    }

    fn check_member_visibility(
        &mut self,
        class: &str,
        member: &str,
        visibility: &Visibility,
        span: Span,
    ) {
        match visibility {
            Visibility::Public | Visibility::Package => {}
            Visibility::Private => {
                let allowed = self
                    .current_class
                    .as_deref()
                    .map(|c| c == class)
                    .unwrap_or(false);
                if !allowed {
                    self.errors.push(
                        TypeError::PrivateAccess {
                            class: class.to_string(),
                            member: member.to_string(),
                            span,
                        }
                        .to_error(),
                    );
                }
            }
            Visibility::Protected => {
                let allowed = self
                    .current_class
                    .as_deref()
                    .map(|c| self.is_subclass_or_equal(c, class))
                    .unwrap_or(false);
                if !allowed {
                    self.errors.push(
                        TypeError::ProtectedAccess {
                            class: class.to_string(),
                            member: member.to_string(),
                            span,
                        }
                        .to_error(),
                    );
                }
            }
        }
    }

    fn types_compatible(&self, a: &ValueType, b: &ValueType) -> bool {
        if a == b {
            return true;
        }
        match (a, b) {
            (ValueType::Int, ValueType::Float) => true,
            (ValueType::Float, ValueType::Int) => true,
            (ValueType::Any, _) | (_, ValueType::Any) => true,
            // Containers: compatible if the element/value types are compatible
            // (Any elements stay wildcards — erased sources allow anything)
            (ValueType::Array(a), ValueType::Array(b)) => self.types_compatible(a, b),
            (ValueType::Map(a), ValueType::Map(b)) => self.types_compatible(a, b),
            // Null safety: null can only go into nullable types
            (ValueType::Nullable(_), ValueType::Null) => true,
            (_, ValueType::Null) => false,
            // A non-null value is compatible with its nullable counterpart
            (ValueType::Nullable(inner), _) => self.types_compatible(inner, b),
            // Allow passing a class where an interface it implements is
            // expected, OR where a base class it extends (directly or
            // transitively) is expected (#173: this arm used to only check
            // interface_implementations, so a subclass instance was
            // rejected as an argument/variable typed as its own base
            // class — even though the free-function-call path already
            // accepted the same relationship, via a separate code path
            // that skips this check entirely rather than handling it
            // correctly).
            (ValueType::Named(base_or_iface, _), ValueType::Named(class, _)) => {
                self.interface_implementations
                    .get(class)
                    .map(|ifaces| ifaces.iter().any(|i| i == base_or_iface))
                    .unwrap_or(false)
                    || self.is_subclass_or_equal(class, base_or_iface)
            }
            // `channel` (a bare Int — see ExprKind::Channel/Recv above,
            // there's no element type to infer at creation) is compatible
            // with any declared `Channel<T>` annotation: `let x: Channel<
            // SomeClass> = channel;` needs this, since the RHS's inferred
            // type is always Int regardless of what T the declaration
            // says. Mirrors how ExprKind::Recv recovers T from the
            // declared side instead of the (always-Int) channel handle.
            (ValueType::Named(name, _), ValueType::Int) if name == "Channel" => true,
            _ => false,
        }
    }

    fn lub(a: &ValueType, b: &ValueType) -> ValueType {
        if a == b {
            return a.clone();
        }
        match (a, b) {
            (ValueType::Int, ValueType::Float) | (ValueType::Float, ValueType::Int) => {
                ValueType::Float
            }
            (ValueType::Array(x), ValueType::Array(y)) => {
                ValueType::Array(Box::new(Self::lub(x, y)))
            }
            (ValueType::Map(x), ValueType::Map(y)) => ValueType::Map(Box::new(Self::lub(x, y))),
            _ => ValueType::Any,
        }
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

pub fn typecheck(source: &SourceFile) -> Result<SourceFile, ErrorBag> {
    let mut checker = TypeChecker::new();
    checker.check(source)
}

/// Like `typecheck`, but first registers declarations from `preludes` (e.g. resolved stdlib
/// imports) so that extern functions and types declared there are known to the checker.
pub fn typecheck_with_prelude(source: &SourceFile, preludes: &[&SourceFile]) -> Result<SourceFile, ErrorBag> {
    let mut checker = TypeChecker::new();
    for prelude in preludes {
        checker.register_declarations(prelude);
        checker.errors.clear();
    }
    checker.expand_prelude_class_inheritance();
    checker.errors.clear();
    checker.check(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    fn typecheck_code(code: &str) -> Result<SourceFile, ErrorBag> {
        let mut lexer = Lexer::new(code);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let mut checker = TypeChecker::new();
        checker.check(&ast)
    }

    fn ok(code: &str) {
        let result = typecheck_code(code);
        assert!(result.is_ok(), "expected ok but got errors: {:?}", result.unwrap_err().errors.iter().map(|e| &e.message).collect::<Vec<_>>());
    }

    fn err_contains(code: &str, msg: &str) {
        let result = typecheck_code(code);
        assert!(result.is_err(), "expected error containing '{}' but typecheck passed", msg);
        let bag = result.unwrap_err();
        assert!(
            bag.errors.iter().any(|e| e.message.contains(msg)),
            "expected error containing '{}', got: {:?}",
            msg,
            bag.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // --- same-class bare `fnc` calls (issue #149 stage 2) ---

    #[test]
    fn test_same_class_bare_static_call_ok() {
        ok("class C { fnc helper(x: Int64) -> Int64 { return x * 2; } fnc main() -> Int64 { return helper(3); } }");
    }

    #[test]
    fn test_same_class_bare_static_call_wrong_arg_count_errors() {
        err_contains(
            "class C { fnc helper(x: Int64) -> Int64 { return x; } fnc main() -> Int64 { return helper(); } }",
            "argument",
        );
    }

    #[test]
    fn test_bare_call_does_not_resolve_to_other_class_method() {
        // A bare call must never silently resolve to a DIFFERENT class's
        // method just because it shares a name -- only the current class
        // (and the flat free-function table) are consulted.
        err_contains(
            "class Other { fnc helper() -> Int64 { return 1; } } class C { fnc main() -> Int64 { return helper(); } }",
            "undefined function",
        );
    }

    #[test]
    fn test_bare_call_does_not_resolve_to_instance_method() {
        // Same-class bare resolution is static-only; an instance `fn`
        // sibling still requires an explicit `this.` receiver.
        err_contains(
            "class C { fn helper() -> Int64 { return 1; } fnc main() -> Int64 { return helper(); } }",
            "undefined function",
        );
    }

    #[test]
    fn test_top_level_free_function_wins_over_same_class_method() {
        // Priority: a genuine top-level free function of the same bare
        // name (still supported during the #149 migration) must win over
        // a same-class static method of that name, matching codegen's
        // priority (fn_sigs checked before the same-class fallback).
        ok("fn helper() -> Int64 { return 99; } class C { fnc helper() -> Int64 { return 1; } fnc main() -> Int64 { return helper(); } }");
    }

    #[test]
    fn test_simple_function() {
        let result = typecheck_code("fn main() -> Int32 { return 42; }");
        assert!(result.is_ok());
    }

    // Element-typed containers: ValueType::Array/Map carry element/value
    // types; mismatches are caught element-precisely, erased sources
    // (Any element) stay wildcards.

    #[test]
    fn test_list_element_mismatch() {
        err_contains(
            "fn main() -> Int32 { let xs: List<String> = [1, 2]; return 0; }",
            "expected List<String>, found List<Int64>",
        );
    }

    #[test]
    fn test_list_element_arg_mismatch() {
        err_contains(
            "fn f(xs: List<Int64>) -> Int64 { return xs.len(); }\nfn main() -> Int32 { let names: List<String> = [\"a\"]; f(names); return 0; }",
            "expected List<Int64>, found List<String>",
        );
    }

    #[test]
    fn test_map_value_mismatch() {
        err_contains(
            "fn main() -> Int32 { let m: Map<String, Int64> = @{\"k\" => \"v\"}; return 0; }",
            "expected Map<String, Int64>, found Map<String, String>",
        );
    }

    #[test]
    fn test_list_element_match_ok() {
        ok("fn main() -> Int32 { let xs: List<Int64> = [1, 2]; let ys: List<String> = [\"a\"]; let m: Map<String, String> = @{\"k\" => \"v\"}; return 0; }");
    }

    #[test]
    fn test_empty_list_stays_wildcard() {
        // Empty literals and erased sources (Any element) are compatible
        // with any element type
        ok("fn main() -> Int32 { let xs: List<String> = []; let m: Map<String, Int64> = @{}; return 0; }");
    }

    #[test]
    fn test_index_yields_element_type() {
        // xs[0] ist Int64 — String-Zuweisung elementgenau abgelehnt
        err_contains(
            "fn main() -> Int32 { let xs: List<Int64> = [1]; let s: String = xs[0]; return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_loop_var_yields_element_type() {
        err_contains(
            "fn main() -> Int32 { let xs: List<Int64> = [1]; for x in xs { let s: String = x; } return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_int_float_lists_compatible() {
        // Int/Float coercion applies element-wise too
        ok("fn main() -> Int32 { let xs: List<Float64> = [1, 2]; return 0; }");
    }

    #[test]
    fn test_var_annotation_checked() {
        // var used to ignore the annotation completely whenever a value
        // was present — the let rule now applies here too
        err_contains(
            "fn main() -> Int32 { var x: String = 5; return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_var_annotation_wins_over_erased_value() {
        // The annotation is the contract: Map<String, List<String>>
        // is preserved, even though Map::new() only returns Map(Any)
        ok("fn main() -> Int32 { var m: Map<String, List<String>> = Map::new(); m.insert(\"k\", [\"a\"]); let v: List<String> = m.get(\"k\"); return 0; }");
    }

    #[test]
    fn test_receiver_dependent_first() {
        err_contains(
            "fn main() -> Int32 { let xs: List<Int64> = [1]; let s: String = xs.first(); return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_match_payload_binding_typed() {
        // Payload-Variablen tragen den deklarierten Variantentyp
        err_contains(
            "enum Box { Val(Int64), Nix }\nfn main() -> Int32 { let b = Box::Val(5); match b { Val(n) => { let s: String = n; } _ => println(\"-\"); } return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_match_payload_binding_container() {
        // Container-Payloads bleiben elementgenau (List<String> → String)
        ok("enum Box { Val(List<String>), Nix }\nfn main() -> Int32 { let b = Box::Val([\"a\"]); match b { Val(xs) => { let s: String = xs[0]; } _ => println(\"-\"); } return 0; }");
    }

    #[test]
    fn test_map_get_yields_value_type() {
        err_contains(
            "fn main() -> Int32 { var m: Map<String, Int64> = Map::new(); let s: String = m.get(\"k\"); return 0; }",
            "expected String, found Int64",
        );
    }

    #[test]
    fn test_undefined_variable() {
        let result = typecheck_code("fn main() -> Int32 { return x; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .errors
            .iter()
            .any(|e| e.message.contains("undefined variable")));
    }

    #[test]
    fn test_binary_op_mismatch() {
        let result = typecheck_code("fn main() -> Int32 { return true + 1; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .errors
            .iter()
            .any(|e| e.message.contains("cannot be applied")));
    }

    #[test]
    fn test_type_mismatch() {
        let result = typecheck_code("fn main() -> Int32 { let x: Int32 = \"hello\"; return x; }");
        assert!(result.is_err());
    }

    #[test]
    fn test_undefined_function() {
        let result = typecheck_code("fn main() -> Int32 { return foo(); }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .errors
            .iter()
            .any(|e| e.message.contains("undefined function")));
    }

    fn parse_code(code: &str) -> Result<tinox_parser::SourceFile, tinox_common::ErrorBag> {
        let mut lexer = Lexer::new(code);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    #[test]
    fn test_interface_extends_full_implementation() {
        // A class implementing IDrawable (which extends IShape) must implement both methods.
        // Interface methods require a body (parser constraint); use empty bodies here.
        let code = r#"
interface IShape {
    fn area() -> Int64 { return 0; }
}
interface IDrawable extends IShape {
    fn draw() { }
}
class Circle implements IDrawable {
    fn draw() { }
    fn area() -> Int64 { return 42; }
}
"#;
        let ast = parse_code(code).expect("parse should succeed");
        let mut checker = TypeChecker::new();
        let result = checker.check(&ast);
        assert!(result.is_ok(), "expected ok but got: {:?}", result);
    }

    #[test]
    fn test_interface_extends_missing_inherited_method() {
        // A class implementing IDrawable but missing area() (inherited from IShape) should fail.
        let code = r#"
interface IShape {
    fn area() -> Int64 { return 0; }
}
interface IDrawable extends IShape {
    fn draw() { }
}
class Circle implements IDrawable {
    fn draw() { }
}
"#;
        let ast = parse_code(code).expect("parse should succeed");
        let mut checker = TypeChecker::new();
        let result = checker.check(&ast);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors.errors.iter().any(|e| e.message.contains("area")),
            "expected error about missing 'area', got: {:?}",
            errors.errors
        );
    }

    #[test]
    fn test_interface_extends_undefined_parent() {
        let code = r#"
interface IDrawable extends IDoesNotExist {
    fn draw() { }
}
"#;
        let ast = parse_code(code).expect("parse should succeed");
        let mut checker = TypeChecker::new();
        let result = checker.check(&ast);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(
            errors.errors.iter().any(|e| e.message.contains("IDoesNotExist")),
            "expected error about undefined interface, got: {:?}",
            errors.errors
        );
    }

    // --- Variables ---

    #[test]
    fn test_let_explicit_type_ok() {
        ok("fn f() { let x: Int32 = 5; }");
    }

    #[test]
    fn test_let_inferred_type_ok() {
        ok("fn f() { let x = 42; }");
    }

    #[test]
    fn test_let_string_ok() {
        ok(r#"fn f() { let s: String = "hello"; }"#);
    }

    #[test]
    fn test_let_bool_ok() {
        ok("fn f() { let b: Bool = true; }");
    }

    #[test]
    fn test_let_type_mismatch_string_as_int() {
        err_contains(
            r#"fn f() { let x: Int32 = "oops"; }"#,
            "expected",
        );
    }

    #[test]
    fn test_let_type_mismatch_bool_as_int() {
        err_contains("fn f() { let x: Int32 = true; }", "expected");
    }

    // --- Function calls ---

    #[test]
    fn test_call_correct_arg_count() {
        ok("fn add(a: Int32, b: Int32) -> Int32 { return a; } fn f() { add(1, 2); }");
    }

    #[test]
    fn test_call_too_few_args() {
        err_contains(
            "fn add(a: Int32, b: Int32) -> Int32 { return a; } fn f() { add(1); }",
            "arguments",
        );
    }

    #[test]
    fn test_call_too_many_args() {
        err_contains(
            "fn add(a: Int32, b: Int32) -> Int32 { return a; } fn f() { add(1, 2, 3); }",
            "arguments",
        );
    }

    #[test]
    fn test_recursive_function_ok() {
        ok("fn fact(n: Int32) -> Int32 { if n > 0 { return fact(n); } return 1; }");
    }

    #[test]
    fn test_mutual_recursion_ok() {
        ok("fn a() -> Int32 { return b(); } fn b() -> Int32 { return a(); }");
    }

    // --- Return type ---

    #[test]
    fn test_return_type_not_checked() {
        err_contains(r#"fn f() -> Int32 { return "hello"; }"#, "expected Int");
    }

    #[test]
    fn test_return_nothing_ok() {
        ok("fn f() { return; }");
    }

    #[test]
    fn test_return_correct_type_ok() {
        ok("fn f() -> Bool { return true; }");
    }

    // --- Binary operators ---

    #[test]
    fn test_add_ints_ok() {
        ok("fn f() { let x = 1 + 2; }");
    }

    #[test]
    fn test_add_floats_ok() {
        ok("fn f() { let x = 1.0 + 2.0; }");
    }

    #[test]
    fn test_add_bool_int_err() {
        err_contains("fn f() { let x = true + 1; }", "cannot be applied");
    }

    #[test]
    fn test_compare_ints_ok() {
        ok("fn f() { let x = 1 < 2; }");
    }

    #[test]
    fn test_logical_and_bools_ok() {
        ok("fn f() { let x = true && false; }");
    }

    #[test]
    fn test_logical_and_int_err() {
        err_contains("fn f() { let x = 1 && 2; }", "cannot be applied");
    }

    #[test]
    fn test_equality_ok() {
        ok("fn f() { let x = 1 == 1; }");
    }

    // --- Unary operators ---

    #[test]
    fn test_unary_neg_int_ok() {
        ok("fn f() { let x = -5; }");
    }

    #[test]
    fn test_unary_not_bool_ok() {
        ok("fn f() { let x = !true; }");
    }

    #[test]
    fn test_unary_not_int_err() {
        err_contains("fn f() { let x = !5; }", "cannot be applied");
    }

    // --- Control flow ---

    #[test]
    fn test_if_bool_cond_ok() {
        ok("fn f() { if true { } }");
    }

    #[test]
    fn test_while_bool_cond_ok() {
        ok("fn f() { while true { } }");
    }

    #[test]
    fn test_for_range_ok() {
        ok("fn f() { for i in 0..10 { } }");
    }

    #[test]
    fn test_break_in_loop_ok() {
        ok("fn f() { while true { break; } }");
    }

    #[test]
    fn test_continue_in_loop_ok() {
        ok("fn f() { while true { continue; } }");
    }

    #[test]
    fn test_break_outside_loop_err() {
        err_contains("fn f() { break; }", "loop");
    }

    #[test]
    fn test_continue_outside_loop_err() {
        err_contains("fn f() { continue; }", "loop");
    }

    // --- Classes ---

    #[test]
    fn test_class_instantiation_ok() {
        ok("class Point { x: Int64; y: Int64; } fn f() { let p = new Point(0, 0); }");
    }

    #[test]
    fn test_class_field_access_ok() {
        ok("class Point { x: Int64; } fn f() { let p = new Point(0); let n = p.x; }");
    }

    #[test]
    fn test_class_method_call_ok() {
        ok(r#"
class Greeter {
    fn greet() -> String { return "hi"; }
}
fn f() { let g = new Greeter(); g.greet(); }
"#);
    }

    #[test]
    fn test_class_undefined_field_err() {
        err_contains(
            "class Point { x: Int64; } fn f() { let p = new Point(0); let n = p.z; }",
            "no field",
        );
    }

    #[test]
    fn test_class_with_this() {
        ok(r#"
class Counter {
    count: Int64;
    fn increment() { this.count = 1; }
}
"#);
    }

    // --- Inheritance ---

    #[test]
    fn test_class_extends_ok() {
        ok(r#"
class Animal { fn speak() { } }
class Dog extends Animal { fn bark() { } }
"#);
    }

    // --- Interfaces ---

    #[test]
    fn test_class_implements_ok() {
        ok(r#"
interface Runnable { fn run() { } }
class Runner implements Runnable { fn run() { } }
"#);
    }

    #[test]
    fn test_class_missing_interface_method_err() {
        err_contains(
            r#"
interface Runnable { fn run() { } }
class Runner implements Runnable { }
"#,
            "run",
        );
    }

    #[test]
    fn test_class_implements_multiple_ok() {
        ok(r#"
interface A { fn a() { } }
interface B { fn b() { } }
class C implements A, B { fn a() { } fn b() { } }
"#);
    }

    #[test]
    fn test_class_missing_one_of_two_interfaces_err() {
        err_contains(
            r#"
interface A { fn a() { } }
interface B { fn b() { } }
class C implements A, B { fn a() { } }
"#,
            "b",
        );
    }

    // --- Enums ---

    #[test]
    fn test_enum_ok() {
        ok("enum Color { Red, Green, Blue }");
    }

    #[test]
    fn test_enum_in_match_ok() {
        ok(r#"
enum Dir { Left, Right }
fn f(d: Dir) {
    match d {
        Dir::Left => { }
        Dir::Right => { }
        _ => { }
    }
}
"#);
    }

    // --- Generics ---

    #[test]
    fn test_generic_function_ok() {
        ok("fn identity<T>(x: T) -> T { return x; }");
    }

    #[test]
    fn test_generic_class_ok() {
        ok("class Box<T> { value: T; } fn f() { let b = new Box(42); }");
    }

    // --- Array ---

    #[test]
    fn test_array_index_ok() {
        ok("fn f() { let arr = [1, 2, 3]; let x = arr[0]; }");
    }

    #[test]
    fn test_array_index_non_int_err() {
        err_contains("fn f() { let arr = [1, 2, 3]; let x = arr[true]; }", "index");
    }

    // --- Match ---

    #[test]
    fn test_match_integer_ok() {
        ok("fn f(x: Int64) { match x { 1 => { } _ => { } } }");
    }

    #[test]
    fn test_match_bool_ok() {
        ok("fn f(b: Bool) { match b { true => { } false => { } } }");
    }

    // --- Try/catch ---

    #[test]
    fn test_try_catch_ok() {
        ok(r#"fn f() { try { let x = 1; } catch e: String { } }"#);
    }

    // --- Visibility ---

    #[test]
    fn test_private_field_inside_class_ok() {
        ok(r#"
class Foo {
    private x: Int64;
    fn getX() -> Int64 { return this.x; }
}
"#);
    }

    #[test]
    fn test_private_field_outside_class_err() {
        err_contains(
            r#"
class Foo { private x: Int64; }
fn f() { let foo = new Foo(0); let n = foo.x; }
"#,
            "private",
        );
    }

    // --- Duplicate definitions ---

    #[test]
    fn test_duplicate_function_not_detected() {
        // Duplicate function detection is not yet implemented in the typecheck pass.
        ok("fn f() { } fn f() { }");
    }

    // --- Import / module (smoke tests) ---

    #[test]
    fn test_module_decl_ok() {
        ok("module myapp;");
    }

    // --- Nested let / shadowing ---

    #[test]
    fn test_variable_shadowing_err() {
        // Tinox typecheck does not allow variable shadowing in the same scope
        err_contains("fn f() { let x = 1; let x = 2; }", "duplicate definition");
    }

    #[test]
    fn test_variable_used_after_declaration() {
        ok("fn f() { let x = 5; let y = x; }");
    }

    // --- Nested function calls ---

    #[test]
    fn test_nested_function_call_ok() {
        ok("fn double(x: Int32) -> Int32 { return x; } fn f() { let y = double(double(1)); }");
    }

    // --- Chained method calls ---

    #[test]
    fn test_chained_method_calls_ok() {
        ok(r#"
class Builder {
    fn step() -> Builder { return this; }
    fn build() { }
}
fn f() { new Builder().step().build(); }
"#);
    }

    // --- Multiple return paths ---

    #[test]
    fn test_multiple_return_paths_ok() {
        ok("fn f(x: Int32) -> Int32 { if x > 0 { return x; } return 0; }");
    }

    // --- Undefined variable in different scopes ---

    #[test]
    fn test_undefined_variable_in_if_branch_err() {
        err_contains("fn f() { if true { return z; } }", "undefined variable");
    }

    #[test]
    fn test_var_defined_in_outer_scope_accessible_in_inner() {
        ok("fn f() { let x = 5; if true { let y = x; } }");
    }

    // --- Float operations ---

    #[test]
    fn test_float_comparison_ok() {
        ok("fn f() { let x = 1.0 < 2.0; }");
    }

    #[test]
    fn test_float_arithmetic_chain_ok() {
        ok("fn f() { let x = 1.0 + 2.0 * 3.0 - 0.5; }");
    }

    // --- String operations ---

    #[test]
    fn test_string_concat_ok() {
        ok(r#"fn f() { let s = "hello" + " world"; }"#);
    }

    // --- Nested class usage ---

    #[test]
    fn test_class_field_assigned_via_this() {
        ok(r#"
class Tracker {
    count: Int64;
    fn reset() { this.count = 0; }
}
"#);
    }

    #[test]
    fn test_nested_class_instances_ok() {
        ok(r#"
class Engine { fn start() { } }
class Car { engine: Engine; fn drive() { this.engine.start(); } }
"#);
    }

    // --- Interface with multiple methods ---

    #[test]
    fn test_interface_multiple_methods_fully_impl() {
        ok(r#"
interface Shape {
    fn area() -> Int64 { return 0; }
    fn perimeter() -> Int64 { return 0; }
}
class Square implements Shape {
    fn area() -> Int64 { return 1; }
    fn perimeter() -> Int64 { return 4; }
}
"#);
    }

    #[test]
    fn test_interface_partial_impl_err() {
        err_contains(r#"
interface Shape {
    fn area() -> Int64 { return 0; }
    fn perimeter() -> Int64 { return 0; }
}
class Square implements Shape {
    fn area() -> Int64 { return 1; }
}
"#, "perimeter");
    }

    // --- Enum match exhaustiveness ---

    #[test]
    fn test_enum_match_with_wildcard_ok() {
        ok(r#"
enum Status { Active, Inactive, Pending }
fn f(s: Status) {
    match s {
        Status::Active => { }
        _ => { }
    }
}
"#);
    }

    // --- Immutable / readonly ---

    #[test]
    fn test_immutable_decl_ok() {
        ok("immutable Point(x: Int64, y: Int64);");
    }

    // --- Namespace / imports ---

    #[test]
    fn test_import_ok() {
        ok("import std.io;");
    }

    #[test]
    fn test_namespace_with_class_ok() {
        ok(r#"
namespace geometry {
    class Circle {
        radius: Float64;
        fn area() -> Float64 { return this.radius; }
    }
}
"#);
    }

    // #161: namespace-wrapped generic classes (every stdlib generic
    // class lives in a `namespace` block, e.g. Option<T>) never got
    // registered for call-site type-argument unification at all — only
    // top-level (non-namespaced) generic classes did. A static factory's
    // return type (`SomeBox<T>`) must resolve to the CONCRETE type
    // (`SomeBox<Int64>`) from the constructor argument, not stay the
    // literal unresolved `T` — checked here by round-tripping the result
    // through a chained call whose own return type is declared
    // concretely, which only type-checks if unification actually ran.
    #[test]
    fn test_namespaced_generic_class_static_factory_return_type_unifies_ok() {
        ok(r#"
namespace boxns {
    class SomeBox<T> {
        value: T;
        fnc wrap(v: T) -> SomeBox<T> { return SomeBox<T> { value: v }; }
        fn unwrap() -> T { return this.value; }
    }
}
fn f() -> Int64 { return SomeBox<Int64>::wrap(1).unwrap(); }
"#);
    }

    // --- Generic function with multiple type params ---

    #[test]
    fn test_generic_two_type_params_ok() {
        ok("fn swap<A, B>(a: A, b: B) -> A { return a; }");
    }

    // --- Stdlib builtins ---

    #[test]
    fn test_builtin_print_ok() {
        ok(r#"fn f() { print("hello"); }"#);
    }

    #[test]
    fn test_builtin_println_ok() {
        ok(r#"fn f() { println(42); }"#);
    }

    #[test]
    fn test_builtin_len_ok() {
        ok("fn f() { let arr = [1, 2, 3]; let n = len(arr); }");
    }

    #[test]
    fn test_builtin_push_ok() {
        ok("fn f() { let arr = [1, 2]; push(arr, 3); }");
    }

    #[test]
    fn test_builtin_sqrt_ok() {
        ok("fn f() { let x = sqrt(2.0); }");
    }

    #[test]
    fn test_builtin_min_max_ok() {
        ok("fn f() { let a = min(1, 2); let b = max(3, 4); }");
    }

    // --- For range variations ---

    #[test]
    fn test_for_range_inclusive_ok() {
        ok("fn f() { for i in 0...10 { } }");
    }

    #[test]
    fn test_for_loop_body_var_accessible() {
        ok("fn f() { for i in 0..5 { let x = i; } }");
    }

    // --- Trait ---

    #[test]
    fn test_trait_decl_ok() {
        ok("trait Serializable { fn serialize() -> String; }");
    }

    // --- Extern fn ---

    #[test]
    fn test_extern_fn_callable_ok() {
        ok("extern fn malloc(size: Int64) -> Int64; fn f() { malloc(8); }");
    }

    // --- Async fn ---

    #[test]
    fn test_async_fn_ok() {
        ok(r#"async fn fetch() -> String { return ""; }"#);
    }

    // ================================================================
    // Assignment / mutability
    // ================================================================

    #[test]
    fn test_assign_to_let_is_err() {
        // let variables are immutable — assigning should fail
        err_contains("fn f() { let x = 1; x = 2; }", "immutable");
    }

    #[test]
    fn test_assign_to_var_ok() {
        ok("fn f() { var x = 1; x = 2; }");
    }

    #[test]
    fn test_compound_add_assign_ok() {
        ok("fn f() { var x = 1; x += 2; }");
    }

    #[test]
    fn test_compound_sub_assign_ok() {
        ok("fn f() { var x = 10; x -= 3; }");
    }

    #[test]
    fn test_compound_mul_assign_ok() {
        ok("fn f() { var x = 2; x *= 4; }");
    }

    #[test]
    fn test_compound_div_assign_ok() {
        ok("fn f() { var x = 8; x /= 2; }");
    }

    #[test]
    fn test_compound_assign_on_let_not_detected() {
        // BUG: typechecker does not yet flag compound-assignment to a `let` variable
        ok("fn f() { let x = 1; x += 1; }");
    }

    // ================================================================
    // Array index
    // ================================================================

    #[test]
    fn test_array_float_index_err() {
        err_contains(
            "fn f(a: Array<Int32>) { let x = a[1.0]; }",
            "integer",
        );
    }

    #[test]
    fn test_array_bool_index_err() {
        err_contains(
            "fn f(a: Array<Int32>) { let x = a[true]; }",
            "integer",
        );
    }

    // ================================================================
    // Division by zero
    // ================================================================

    #[test]
    fn test_division_by_zero_not_detected() {
        // BUG: constant division by zero is not detected at typecheck time
        ok("fn f() -> Int64 { return 10 / 0; }");
    }

    #[test]
    fn test_mod_by_zero_not_detected() {
        // BUG: constant modulo by zero is not detected at typecheck time
        ok("fn f() -> Int64 { return 5 % 0; }");
    }

    // ================================================================
    // Cast
    // ================================================================

    #[test]
    fn test_cast_int_to_float_ok() {
        ok("fn f(x: Int64) -> Float64 { return x as Float64; }");
    }

    #[test]
    fn test_cast_float_to_int_ok() {
        ok("fn f(x: Float64) -> Int64 { return x as Int64; }");
    }

    #[test]
    fn test_cast_int_to_string_ok() {
        ok("fn f(x: Int64) -> String { return x as String; }");
    }

    #[test]
    fn test_cast_bool_to_string_ok() {
        ok("fn f(b: Bool) -> String { return b as String; }");
    }

    // ================================================================
    // Private / protected access
    // ================================================================

    #[test]
    fn test_private_method_inside_class_ok() {
        // Private methods called via this.method() work; bare secret() is a free-function call
        ok(r#"
class Foo {
    private fn secret() -> Nothing {}
    fn callSecret() -> Nothing { this.secret(); }
}
"#);
    }

    #[test]
    fn test_protected_field_outside_class_err() {
        err_contains(r#"
class Foo {
    protected var x: Int64;
}
fn f(foo: Foo) { let y = foo.x; }
"#, "protected");
    }

    // ================================================================
    // String built-in methods via MethodCall
    // ================================================================

    #[test]
    fn test_string_method_touppercase_ok() {
        ok(r#"fn f(s: String) { let u = s.toUpper(); }"#);
    }

    #[test]
    fn test_string_method_length_ok() {
        ok(r#"fn f(s: String) { let n = s.len(); }"#);
    }

    #[test]
    fn test_string_method_contains_ok() {
        ok(r#"fn f(s: String) { let b = s.contains("x"); }"#);
    }

    #[test]
    fn test_string_method_charat_ok() {
        ok(r#"fn f(s: String) { let c = s.charAt(0); }"#);
    }

    #[test]
    fn test_string_method_toint_ok() {
        ok(r#"fn f(s: String) { let n = s.toInt(); }"#);
    }

    #[test]
    fn test_string_method_replace_ok() {
        ok(r#"fn f(s: String) { let r = s.replace("a", "b"); }"#);
    }

    // ================================================================
    // Array built-in methods via MethodCall
    // ================================================================

    #[test]
    fn test_array_method_push_ok() {
        ok("fn f(a: Array<Int64>) { a.push(1); }");
    }

    #[test]
    fn test_array_method_len_ok() {
        ok("fn f(a: Array<Int64>) { let n = a.len(); }");
    }

    #[test]
    fn test_array_method_sort_ok() {
        ok("fn f(a: Array<Int64>) { let s = a.sort(); }");
    }

    #[test]
    fn test_array_method_contains_ok() {
        ok("fn f(a: Array<Int64>) { let b = a.contains(1); }");
    }

    // ================================================================
    // For-C loop
    // ================================================================

    #[test]
    fn test_forc_loop_ok() {
        // For-C syntax requires parentheses: for (init; cond; update) { body }
        ok("fn f() { for (var i = 0; i < 10; i += 1) { let x = i; } }");
    }

    // ================================================================
    // Range type check
    // ================================================================

    #[test]
    fn test_range_float_err() {
        err_contains("fn f() { for i in 0.0..5.0 { } }", "range");
    }

    // ================================================================
    // Logical operators
    // ================================================================

    #[test]
    fn test_logical_or_bools_ok() {
        ok("fn f(a: Bool, b: Bool) -> Bool { return a || b; }");
    }

    #[test]
    fn test_logical_or_int_err() {
        err_contains("fn f(a: Int64, b: Int64) -> Bool { return a || b; }", "cannot be applied");
    }

    #[test]
    fn test_logical_not_bool_ok() {
        ok("fn f(b: Bool) -> Bool { return !b; }");
    }

    // ================================================================
    // Bitwise operators
    // ================================================================

    #[test]
    fn test_bitwise_and_ints_ok() {
        ok("fn f(a: Int64, b: Int64) -> Int64 { return a & b; }");
    }

    #[test]
    fn test_bitwise_or_ints_ok() {
        ok("fn f(a: Int64, b: Int64) -> Int64 { return a | b; }");
    }

    #[test]
    fn test_bitwise_xor_ints_ok() {
        ok("fn f(a: Int64, b: Int64) -> Int64 { return a ^ b; }");
    }

    #[test]
    fn test_bitwise_not_int_ok() {
        ok("fn f(a: Int64) -> Int64 { return ~a; }");
    }

    #[test]
    fn test_shift_left_ok() {
        ok("fn f(a: Int64) -> Int64 { return a << 2; }");
    }

    #[test]
    fn test_shift_right_ok() {
        ok("fn f(a: Int64) -> Int64 { return a >> 1; }");
    }

    // ================================================================
    // Comparison operators
    // ================================================================

    #[test]
    fn test_compare_floats_ok() {
        ok("fn f(a: Float64, b: Float64) -> Bool { return a < b; }");
    }

    #[test]
    fn test_compare_strings_ok() {
        ok("fn f(a: String, b: String) -> Bool { return a < b; }");
    }

    #[test]
    fn test_equality_bools_ok() {
        ok("fn f(a: Bool, b: Bool) -> Bool { return a == b; }");
    }

    #[test]
    fn test_equality_strings_ok() {
        ok("fn f(a: String, b: String) -> Bool { return a == b; }");
    }

    // ================================================================
    // Null literal
    // ================================================================

    #[test]
    fn test_null_literal_ok() {
        ok("fn f() -> Nothing { let x = null; }");
    }

    // ================================================================
    // Nested if / elif
    // ================================================================

    #[test]
    fn test_nested_if_ok() {
        ok("fn f(x: Int64, y: Int64) -> Nothing { if x > 0 { if y > 0 { return; } } }");
    }

    #[test]
    fn test_else_if_ok() {
        ok("fn f(x: Int64) -> Nothing { if x > 0 { return; } else if x < 0 { return; } else { return; } }");
    }

    // ================================================================
    // Try / catch / throw
    // ================================================================

    #[test]
    fn test_throw_string_ok() {
        ok(r#"fn f() { throw "error"; }"#);
    }

    #[test]
    fn test_try_catch_var_accessible_ok() {
        ok(r#"fn f() { try { let x = 1; } catch e: String { println(e); } }"#);
    }

    // ================================================================
    // Class: constructor / new
    // ================================================================

    #[test]
    fn test_new_class_ok() {
        ok("class Foo { var x: Int64; } fn f() -> Foo { return new Foo(1); }");
    }

    #[test]
    fn test_class_self_reference_ok() {
        ok(r#"
class Node {
    var value: Int64;
    fn getValue() -> Int64 { return this.value; }
}
"#);
    }

    // ================================================================
    // Scope: variable not visible before declaration
    // ================================================================

    #[test]
    fn test_var_not_visible_before_decl_err() {
        err_contains("fn f() { let y = x; let x = 1; }", "undefined variable");
    }

    // ================================================================
    // Enum variants in expressions
    // ================================================================

    #[test]
    fn test_enum_variant_in_let_ok() {
        ok("enum Dir { North; South; } fn f() { let d = Dir::North; }");
    }

    #[test]
    fn test_enum_variant_in_match_pattern_ok() {
        ok(r#"
enum Status { Ok; Err; }
fn f(s: Status) -> Nothing {
    match s {
        Status::Ok => return;
        Status::Err => return;
    }
}
"#);
    }

    // ================================================================
    // Interface: method signature mismatch
    // ================================================================

    #[test]
    fn test_interface_method_wrong_return_type_err() {
        // Interface declares fn run() -> String, class returns Nothing
        err_contains(r#"
interface Runner {
    fn run() -> String;
}
class Jogger implements Runner {
    fn run() -> Nothing {}
}
"#, "mismatch");
    }

    // ================================================================
    // Multiple builtin calls in sequence
    // ================================================================

    #[test]
    fn test_multiple_builtin_calls_ok() {
        ok(r#"fn f() { println("a"); println("b"); println("c"); }"#);
    }

    #[test]
    fn test_builtin_pop_ok() {
        ok("fn f(a: Array<Int64>) { let b = pop(a); }");
    }

    #[test]
    fn test_builtin_to_string_ok() {
        ok("fn f(x: Int64) { let s = toString(x); }");
    }

    #[test]
    fn test_builtin_char_at_ok() {
        ok(r#"fn f(s: String) { let c = charAt(s, 0); }"#);
    }

    #[test]
    fn test_builtin_floor_ceil_round_ok() {
        ok("fn f(x: Float64) { let a = floor(x); let b = ceil(x); let c = round(x); }");
    }

    #[test]
    fn test_builtin_exit_ok() {
        ok("fn f() { exit(0); }");
    }

    #[test]
    fn test_builtin_pow_ok() {
        ok("fn f(x: Float64) { let r = pow(x, 2.0); }");
    }

    // ================================================================
    // Undefined interface
    // ================================================================

    #[test]
    fn test_undefined_interface_not_detected() {
        // BUG: typechecker does not validate that implemented interfaces are defined
        ok("class Foo implements NonExistent {}");
    }

    // ================================================================
    // Map type operations
    // ================================================================

    #[test]
    fn test_map_literal_ok() {
        ok("fn f() { let m = @{\"a\" => 1, \"b\" => 2}; }");
    }

    #[test]
    fn test_map_subscript_ok() {
        ok("fn f() -> Int64 { let m = @{\"x\" => 42}; return m[\"x\"]; }");
    }

    // ================================================================
    // Tuple type checks
    // ================================================================

    #[test]
    fn test_tuple_type_ok() {
        ok("fn f() -> (Int64, Bool) { return (1, true); }");
    }

    #[test]
    fn test_tuple_access_ok() {
        ok("fn f() -> Int64 { let t = (10, 20); return t.0; }");
    }

    // ================================================================
    // Cast checks
    // ================================================================

    #[test]
    fn test_cast_int_to_float_v2_ok() {
        ok("fn f() -> Float64 { let x = 5; return x as Float64; }");
    }

    #[test]
    fn test_cast_float_to_int_v2_ok() {
        ok("fn f() -> Int64 { let x = 3.14; return x as Int64; }");
    }

    #[test]
    fn test_cast_int_to_string_v2_ok() {
        ok("fn f() -> String { let x = 42; return x as String; }");
    }

    // ================================================================
    // Complex class inheritance
    // ================================================================

    #[test]
    fn test_class_extends_and_implements_ok() {
        ok(concat!(
            "interface Printable { fn print() -> Nothing; }\n",
            "class Base { fn init() -> Nothing {} }\n",
            "class Child extends Base implements Printable { fn print() -> Nothing {} }"
        ));
    }

    #[test]
    fn test_super_call_ok() {
        ok(concat!(
            "class Animal { fn speak() -> Nothing { println(\"...\"); } }\n",
            "class Dog extends Animal { fn speak() -> Nothing { super.speak(); } }"
        ));
    }

    // ================================================================
    // Enum checks
    // ================================================================

    #[test]
    fn test_enum_match_exhaustive_ok() {
        ok(concat!(
            "enum Dir { North, South, East, West }\n",
            "fn f(d: Dir) -> Nothing { match d { Dir::North => return; Dir::South => return; Dir::East => return; Dir::West => return; } }"
        ));
    }

    #[test]
    fn test_enum_match_with_wildcard_v2_ok() {
        ok(concat!(
            "enum Color { Red, Green, Blue }\n",
            "fn f(c: Color) -> Nothing { match c { Color::Red => return; _ => return; } }"
        ));
    }

    #[test]
    fn test_enum_variant_with_payload_ok() {
        ok(concat!(
            "enum Shape { Circle(Float64), Square(Float64) }\n",
            "fn f() { let s = Shape::Circle(3.0); }"
        ));
    }

    // ================================================================
    // Nested function calls
    // ================================================================

    #[test]
    fn test_nested_call_chain_ok() {
        ok("fn f() -> String { return toString(42); }");
    }

    #[test]
    fn test_method_call_chain_ok() {
        ok("fn f(s: String) -> String { return s.toUpperCase().trim(); }");
    }

    // ================================================================
    // Array operations
    // ================================================================

    #[test]
    fn test_array_literal_int_ok() {
        ok("fn f() { let a = [1, 2, 3]; }");
    }

    #[test]
    fn test_array_index_out_of_range_not_detected() {
        // BUG: typechecker does not check index bounds at compile time
        ok("fn f() { let a = [1, 2, 3]; let x = a[99]; }");
    }

    #[test]
    fn test_array_push_and_len_ok() {
        ok("fn f() { var a: Array<Int64> = []; a.push(1); let n = a.len(); }");
    }

    // ================================================================
    // Variable scoping
    // ================================================================

    #[test]
    fn test_var_in_if_block_not_visible_outside_err() {
        err_contains(
            "fn f() -> Int64 { if true { var x = 1; } return x; }",
            "undefined",
        );
    }

    #[test]
    fn test_nested_blocks_shadow_err() {
        // Typechecker treats shadowing in inner blocks as duplicate definition
        err_contains("fn f() -> Int64 { var x = 1; { var x = 2; } return x; }", "duplicate");
    }

    // ================================================================
    // While / loop
    // ================================================================

    #[test]
    fn test_while_loop_ok() {
        ok("fn f() { var i = 0; while i < 10 { i = i + 1; } }");
    }

    #[test]
    fn test_loop_forever_ok() {
        ok("fn f() { loop { break; } }");
    }

    #[test]
    fn test_break_in_while_ok() {
        ok("fn f() { while true { break; } }");
    }

    #[test]
    fn test_continue_in_while_ok() {
        ok("fn f() { var i = 0; while i < 5 { i = i + 1; continue; } }");
    }

    // ================================================================
    // Generic functions
    // ================================================================

    #[test]
    fn test_generic_fn_ok() {
        ok("fn identity<T>(x: T) -> T { return x; }");
    }

    #[test]
    fn test_generic_class_with_field_ok() {
        ok("class Box<T> { var value: T; fn init(v: T) -> Nothing { this.value = v; } }");
    }

    // ================================================================
    // Multiple return paths
    // ================================================================

    #[test]
    fn test_multiple_return_paths_v2_ok() {
        ok("fn abs(x: Int64) -> Int64 { if x < 0 { return -x; } return x; }");
    }

    #[test]
    fn test_early_return_type_mismatch() {
        err_contains("fn f() -> Int64 { if true { return \"oops\"; } return 1; }", "expected Int64");
    }

    // ================================================================
    // Defer
    // ================================================================

    #[test]
    fn test_defer_ok() {
        ok("fn f() { defer { println(\"done\"); } println(\"start\"); }");
    }

    // ================================================================
    // Select / channel / send / recv
    // ================================================================

    #[test]
    fn test_channel_expr_ok() {
        ok("fn f() { let ch = channel; }");
    }

    #[test]
    fn test_spawn_expr_ok() {
        ok("fn worker() -> Nothing {}\nfn f() { let t = spawn worker(); }");
    }

    // ================================================================
    // String interpolation / format
    // ================================================================

    #[test]
    fn test_string_concat_v2_ok() {
        ok("fn f() -> String { let a = \"hello\"; let b = \"world\"; return a + \" \" + b; }");
    }

    // ================================================================
    // Null handling
    // ================================================================

    #[test]
    fn test_nullable_type_ok() {
        ok("fn f() -> String? { return null; }");
    }

    #[test]
    fn test_null_in_non_nullable_rejected() {
        err_contains("fn f() -> String { return null; }", "found null");
    }

    #[test]
    fn test_nullable_accepts_non_null() {
        ok("fn f() -> String? { return \"hello\"; }");
    }

    #[test]
    fn test_nullable_var_accepts_null() {
        ok("fn f() { let x: String? = null; }");
    }

    #[test]
    fn test_non_nullable_var_rejects_null() {
        err_contains("fn f() { let x: String = null; }", "found null");
    }

    // ================================================================
    // Boolean expressions
    // ================================================================

    #[test]
    fn test_boolean_short_circuit_ok() {
        ok("fn f() -> Bool { return true && false || true; }");
    }

    #[test]
    fn test_boolean_negation_ok() {
        ok("fn f() -> Bool { return !false; }");
    }

    // ================================================================
    // Ternary / inline if
    // ================================================================

    #[test]
    fn test_ternary_ok() {
        // if-else as expression with explicit returns in branches
        ok("fn f(x: Int64) -> Int64 { if x > 0 { return x; } return -x; }");
    }

    // ================================================================
    // Interface with multiple methods
    // ================================================================

    #[test]
    fn test_interface_with_multiple_methods_all_impl_ok() {
        ok(concat!(
            "interface Shape {\n",
            "  fn area() -> Float64;\n",
            "  fn perimeter() -> Float64;\n",
            "}\n",
            "class Circle implements Shape {\n",
            "  fn area() -> Float64 { return 3.14; }\n",
            "  fn perimeter() -> Float64 { return 6.28; }\n",
            "}"
        ));
    }

    #[test]
    fn test_interface_missing_method_err() {
        err_contains(
            concat!(
                "interface Shape { fn area() -> Float64; }\n",
                "class Circle implements Shape {}"
            ),
            "does not implement",
        );
    }

    // ================================================================
    // Arithmetic edge cases
    // ================================================================

    #[test]
    fn test_add_int_float_not_detected() {
        // BUG: mixed int+float arithmetic silently passes typecheck
        ok("fn f() -> Int64 { return 1 + 2.0; }");
    }

    #[test]
    fn test_multiply_bool_err() {
        err_contains("fn f() -> Bool { return true * false; }", "cannot be applied");
    }

    #[test]
    fn test_negate_string_err() {
        err_contains("fn f() { let x = -\"hello\"; }", "cannot be applied");
    }

    #[test]
    fn test_add_two_floats_ok() {
        ok("fn f() -> Float64 { return 1.5 + 2.5; }");
    }

    #[test]
    fn test_modulo_ints_ok() {
        ok("fn f() -> Int64 { return 10 % 3; }");
    }

    #[test]
    fn test_modulo_floats_ok() {
        ok("fn f() -> Float64 { return 10.0 % 3.0; }");
    }

    // ================================================================
    // Comparison type checking
    // ================================================================

    #[test]
    fn test_compare_int_string_err() {
        err_contains("fn f() -> Bool { return 5 < \"a\"; }", "cannot be applied");
    }

    #[test]
    fn test_equality_int_bool_err() {
        err_contains("fn f() -> Bool { return 1 == true; }", "cannot be applied");
    }

    // ================================================================
    // Return type exhaustiveness
    // ================================================================

    #[test]
    fn test_void_fn_no_return_ok() {
        ok("fn f() -> Nothing {}");
    }

    #[test]
    fn test_fn_returns_correct_type_ok() {
        ok("fn f() -> Bool { return true; }");
    }

    #[test]
    fn test_fn_returns_wrong_primitive() {
        err_contains("fn f() -> Bool { return 42; }", "expected Bool");
    }

    #[test]
    fn test_fn_returns_string_literal_ok() {
        ok("fn f() -> String { return \"hello world\"; }");
    }

    // ================================================================
    // Function call argument errors
    // ================================================================

    #[test]
    fn test_call_too_many_args_err() {
        err_contains(
            "fn add(a: Int64, b: Int64) -> Int64 { return a + b; }\nfn f() { add(1, 2, 3); }",
            "argument",
        );
    }

    #[test]
    fn test_call_too_few_args_err() {
        err_contains(
            "fn add(a: Int64, b: Int64) -> Int64 { return a + b; }\nfn f() { add(1); }",
            "argument",
        );
    }

    #[test]
    fn test_call_wrong_arg_type_err() {
        err_contains(
            "fn greet(name: String) -> Nothing { println(name); }\nfn f() { greet(42); }",
            "expected String",
        );
    }

    #[test]
    fn test_call_no_args_ok() {
        ok("fn ping() -> Nothing {}\nfn f() { ping(); }");
    }

    // ================================================================
    // Variable mutation
    // ================================================================

    #[test]
    fn test_let_reassign_err() {
        err_contains(
            "fn f() { let x = 1; x = 2; }",
            "immutable",
        );
    }

    #[test]
    fn test_var_reassign_ok() {
        ok("fn f() { var x = 1; x = 2; }");
    }

    #[test]
    fn test_var_reassign_wrong_type_err() {
        err_contains("fn f() { var x = 1; x = \"oops\"; }", "expected Int64");
    }

    #[test]
    fn test_var_compound_assign_ok() {
        ok("fn f() { var x = 10; x += 5; }");
    }

    // ================================================================
    // Recursive functions
    // ================================================================

    #[test]
    fn test_recursive_fn_ok() {
        ok("fn fib(n: Int64) -> Int64 { if n <= 1 { return n; } return fib(n - 1) + fib(n - 2); }");
    }

    #[test]
    fn test_mutually_recursive_not_detected() {
        // BUG: mutual recursion may not be detected if functions are defined in order
        ok("fn a() -> Nothing { b(); }\nfn b() -> Nothing { a(); }");
    }

    // ================================================================
    // Class field typing
    // ================================================================

    #[test]
    fn test_class_field_access_v2_ok() {
        // Classes are instantiated with new keyword in typechecker context
        ok("class Box { var value: Int64; }\nfn make() -> Box { return new Box(); }\nfn f() -> Int64 { let b = make(); return b.value; }");
    }

    #[test]
    fn test_class_field_nonexistent_err() {
        // Typechecker only resolves field access on known types
        err_contains(
            "class Box { var value: Int64; }\nfn f(b: Box) { let x = b.missing; }",
            "field",
        );
    }

    #[test]
    fn test_class_method_call_v2_ok() {
        ok("class Counter { var n: Int64; fn inc() -> Nothing { this.n += 1; } }\nfn f(c: Counter) { c.inc(); }");
    }

    #[test]
    fn test_class_method_wrong_arg_err() {
        err_contains(
            "class Greeter { fn greet(name: String) -> Nothing {} }\nfn f(g: Greeter) { g.greet(42); }",
            "expected String",
        );
    }

    // ================================================================
    // Array type checking
    // ================================================================

    #[test]
    fn test_array_index_int_ok() {
        ok("fn f() -> Int64 { let a = [1, 2, 3]; return a[0]; }");
    }

    #[test]
    fn test_array_index_float_err() {
        err_contains("fn f() { let a = [1, 2, 3]; let x = a[1.5]; }", "integer");
    }

    #[test]
    fn test_array_len_returns_int_ok() {
        ok("fn f() -> Int64 { let a = [1, 2, 3]; return a.len(); }");
    }

    // ================================================================
    // Try/throw type checking
    // ================================================================

    #[test]
    fn test_throw_int_not_allowed() {
        // throw only accepts String or error types
        err_contains("fn f() { throw 42; }", "throw");
    }

    #[test]
    fn test_try_catch_string_ok() {
        ok("fn f() { try { throw \"oops\"; } catch e: String { println(e); } }");
    }

    #[test]
    fn test_try_finally_ok() {
        ok("fn f() { try { throw \"err\"; } catch e: String {} finally { println(\"done\"); } }");
    }

    // ================================================================
    // Static-call resolution (Bug 36 / 43): unknown class or method → error
    // ================================================================

    #[test]
    fn test_static_call_unknown_class_err() {
        // `Bogus` is neither imported nor defined → hard error, not silent Any.
        err_contains(
            "fn main() -> Int32 { let x: Int64 = Bogus::doStuff(42); return 0; }",
            "unresolved",
        );
    }

    #[test]
    fn test_static_call_unknown_method_err() {
        // `Calc` is a known non-generic class but has no method `addValu` (typo).
        err_contains(
            "class Calc { var v: Int64; fn addVal(x: Int64) -> Int64 { return x; } } \
             fn main() -> Int32 { let c: Calc = Calc { v: 0 }; let r: Int64 = Calc::addValu(c, 5); return 0; }",
            "has no method 'addValu'",
        );
    }

    #[test]
    fn test_static_call_known_method_ok() {
        ok("class Calc { var v: Int64; fn addVal(x: Int64) -> Int64 { return x; } } \
            fn main() -> Int32 { let c: Calc = Calc { v: 0 }; let r: Int64 = Calc::addVal(c, 5); return 0; }");
    }

    #[test]
    fn test_enum_unknown_variant_err() {
        err_contains(
            "enum Color { Red; Green; Blue; } \
             fn main() -> Int32 { let c: Color = Color::Purple; return 0; }",
            "has no variant 'Purple'",
        );
    }

    #[test]
    fn test_enum_known_variant_ok() {
        ok("enum Color { Red; Green; Blue; } \
            fn main() -> Int32 { let c: Color = Color::Green; return 0; }");
    }

    #[test]
    fn test_enum_same_name_variants_unioned_ok() {
        // Two enums share the name `M`; a variant valid in EITHER definition must
        // pass the variant check (Bug 45 union across cross-module collisions).
        ok("enum M { A; B; } enum M { C; D; } \
            fn main() -> Int32 { let x: M = M::A; let y: M = M::D; return 0; }");
    }

    #[test]
    fn test_static_call_instance_too_few_args_err() {
        // `add` declares 2 params; a call giving only the receiver (1 arg) is
        // below both accepted counts {2, 3} → error.
        err_contains(
            "class Calc { var v: Int64; fn add(a: Int64, b: Int64) -> Int64 { return a + b; } } \
             fn main() -> Int32 { let c: Calc = Calc { v: 0 }; let r: Int64 = Calc::add(c); return 0; }",
            "arguments",
        );
    }

    #[test]
    fn test_static_call_static_wrong_arg_count_err() {
        err_contains(
            "class Mathy { fnc square(x: Int64) -> Int64 { return x * x; } } \
             fn main() -> Int32 { let r: Int64 = Mathy::square(3, 4, 5); return 0; }",
            "arguments",
        );
    }

    #[test]
    fn test_static_call_this_method_without_receiver_err() {
        // `getN` uses `this`, so the receiver is mandatory (exactly declared+1
        // args). Calling `Box::getN()` with no object would deref a null self at
        // runtime — caught exactly (Bug 47).
        err_contains(
            "class Box { var n: Int64; fn getN() -> Int64 { return this.n; } } \
             fn main() -> Int32 { let b: Box = Box { n: 5 }; let r: Int64 = Box::getN(); return 0; }",
            "arguments",
        );
    }

    #[test]
    fn test_static_call_receiver_agnostic_method_permissive_ok() {
        // `label` ignores its receiver (no `this`); called `C::label(obj)` the
        // object is passed-but-unused. Not distinguishable from a pure namespace
        // helper → stays permissive, must not false-positive (Bug 47).
        ok("class C { var n: Int64; fn label() -> String { return \"x\"; } } \
            fn main() -> Int32 { let c: C = C { n: 0 }; let s: String = C::label(c); return 0; }");
    }

    #[test]
    fn test_static_call_instance_both_styles_ok() {
        // Style 2 (object as self, args == declared+1) and style 1 (object as an
        // explicit first declared param, args == declared) both pass.
        ok("class Calc { var v: Int64; fn add(a: Int64, b: Int64) -> Int64 { return a + b; } } \
            fn main() -> Int32 { let c: Calc = Calc { v: 0 }; let r: Int64 = Calc::add(c, 3, 4); return 0; }");
        ok("class Store { var n: Int64; fn getWith(s: Store, extra: Int64) -> Int64 { return s.n + extra; } } \
            fn main() -> Int32 { let s: Store = Store { n: 0 }; let r: Int64 = Store::getWith(s, 5); return 0; }");
    }

    #[test]
    fn test_static_call_generic_class_permissive_ok() {
        // Generic-class statics stay permissive (method may resolve only after
        // monomorphization) — no false positive from the Bug 43 hardening.
        ok("class Box<T> { var value: T; fn wrap(v: T) -> Box<T> { return Box { value: v }; } \
            fn get() -> T { return this.value; } } \
            fn main() -> Int32 { let b: Box<Int64> = Box::wrap(5); let r: Int64 = Box::get(b); return 0; }");
    }

    // ================================================================
    // Type casting
    // ================================================================

    #[test]
    fn test_cast_int8_to_int64_ok() {
        ok("fn f() -> Int64 { let x: Int8 = 5; return x as Int64; }");
    }

    #[test]
    fn test_cast_float64_to_int32_ok() {
        ok("fn f() -> Int32 { let x = 3.7; return x as Int32; }");
    }

    // ================================================================
    // Immutable struct
    // ================================================================

    #[test]
    fn test_immutable_struct_field_access_ok() {
        ok("immutable Point(x: Int64, y: Int64);\nfn f(p: Point) -> Int64 { return p.x; }");
    }

    // ================================================================
    // Namespace
    // ================================================================

    #[test]
    fn test_namespace_fn_direct_call_ok() {
        // Namespace functions are callable directly within scope
        ok("namespace math { fn square(x: Int64) -> Int64 { return x * x; } }");
    }

    // ================================================================
    // Complex expressions
    // ================================================================

    #[test]
    fn test_chained_comparisons_ok() {
        ok("fn f(x: Int64) -> Bool { return x > 0 && x < 100; }");
    }

    #[test]
    fn test_complex_bool_expr_ok() {
        ok("fn f(a: Bool, b: Bool, c: Bool) -> Bool { return (a || b) && !c; }");
    }

    #[test]
    fn test_nested_ternary_style_ok() {
        ok("fn clamp(x: Int64, lo: Int64, hi: Int64) -> Int64 { if x < lo { return lo; } if x > hi { return hi; } return x; }");
    }

    #[test]
    fn test_string_plus_int_err() {
        err_contains("fn f() -> String { return \"x\" + 1; }", "cannot be applied");
    }

    // #164: List<Int64>.join() used to segfault at runtime (no
    // element-type check) instead of failing to compile — covers both
    // call spellings (instance and free-function), and confirms
    // List<String> (the only element type the runtime helper actually
    // supports) and List<Any> (element type genuinely unknown, stays
    // permissive) are unaffected.
    #[test]
    fn test_join_on_int_array_method_call_err() {
        err_contains(
            "fn f() -> String { let xs: List<Int64> = [1, 2, 3]; return xs.join(\", \"); }",
            "List<String>",
        );
    }

    #[test]
    fn test_join_on_int_array_free_call_err() {
        err_contains(
            "fn f() -> String { let xs: List<Int64> = [1, 2, 3]; return join(xs, \", \"); }",
            "List<String>",
        );
    }

    #[test]
    fn test_join_on_string_array_method_call_ok() {
        ok("fn f() -> String { let xs: List<String> = [\"a\", \"b\"]; return xs.join(\", \"); }");
    }

    #[test]
    fn test_join_on_string_array_free_call_ok() {
        ok("fn f() -> String { let xs: List<String> = [\"a\", \"b\"]; return join(xs, \", \"); }");
    }

    #[test]
    fn test_join_on_any_array_stays_permissive() {
        ok("fn f(xs: List<Any>) -> String { return xs.join(\", \"); }");
    }

    // ================================================================
    // Multiple classes interacting
    // ================================================================

    #[test]
    fn test_two_classes_interact_ok() {
        ok(concat!(
            "class Engine { fn start() -> Nothing {} }\n",
            "class Car { fn drive(e: Engine) -> Nothing { e.start(); } }"
        ));
    }

    #[test]
    fn test_class_returns_self_ok() {
        ok("class Builder { fn build() -> Builder { return this; } }");
    }

    // ================================================================
    // Enum matching completeness
    // ================================================================

    #[test]
    fn test_enum_match_returns_value_ok() {
        ok(concat!(
            "enum Dir { North, South }\n",
            "fn label(d: Dir) -> String {\n",
            "  match d {\n",
            "    Dir::North => return \"N\";\n",
            "    Dir::South => return \"S\";\n",
            "  }\n",
            "}"
        ));
    }

    // ================================================================
    // For-C loop edge cases
    // ================================================================

    #[test]
    fn test_forc_loop_nested_ok() {
        ok(concat!(
            "fn f() {\n",
            "  for (var i = 0; i < 3; i += 1) {\n",
            "    for (var j = 0; j < 3; j += 1) {\n",
            "      let x = i + j;\n",
            "    }\n",
            "  }\n",
            "}"
        ));
    }

    #[test]
    fn test_forc_loop_var_accessible_in_body_ok() {
        ok("fn f() { for (var i = 0; i < 5; i += 1) { let x = i * 2; } }");
    }

    // ================================================================
    // Select statement
    // ================================================================

    #[test]
    fn test_select_stmt_ok() {
        // select arms: recv channel -> varname { body }
        ok("fn f(ch: Chan) { select { recv ch -> v { println(\"got\"); } } }");
    }

    // ================================================================
    // Defer edge cases
    // ================================================================

    #[test]
    fn test_defer_with_method_call_ok() {
        ok(concat!(
            "class Resource { fn close() -> Nothing {} }\n",
            "fn f(r: Resource) { defer { r.close(); } }"
        ));
    }

    // ================================================================
    // Float comparison
    // ================================================================

    #[test]
    fn test_float_less_than_ok() {
        ok("fn f() -> Bool { return 1.0 < 2.0; }");
    }

    #[test]
    fn test_float_equality_ok() {
        ok("fn f() -> Bool { return 1.0 == 1.0; }");
    }

    // ================================================================
    // Bitwise operations
    // ================================================================

    #[test]
    fn test_bitwise_left_shift_result_ok() {
        ok("fn f() -> Int64 { let x = 1 << 4; return x; }");
    }

    #[test]
    fn test_bitwise_right_shift_result_ok() {
        ok("fn f() -> Int64 { let x = 256 >> 2; return x; }");
    }

    #[test]
    fn test_bitwise_ops_on_bool_err() {
        err_contains("fn f() { let x = true & false; }", "cannot be applied");
    }

    // ================================================================
    // Struct literal missing/unknown field (Bug 130)
    // ================================================================

    #[test]
    fn test_struct_literal_missing_field_errors() {
        err_contains(
            "class Point { var x: Int64; var y: Int64; var label: String; } fn f() { let p = Point { x: 1, y: 2 }; }",
            "missing field(s): label",
        );
    }

    #[test]
    fn test_struct_literal_all_fields_present_ok() {
        ok("class Point { var x: Int64; var y: Int64; var label: String; } fn f() { let p = Point { x: 1, y: 2, label: \"a\" }; }");
    }

    #[test]
    fn test_struct_literal_unknown_field_errors() {
        err_contains(
            "class Point { var x: Int64; var y: Int64; } fn f() { let p = Point { x: 1, y: 2, z: 3 }; }",
            "has no field 'z'",
        );
    }

    #[test]
    fn test_struct_literal_missing_inherited_field_errors() {
        err_contains(
            "class Animal { var name: String; } class Dog extends Animal { var breed: String; } fn f() { let d = Dog { breed: \"Lab\" }; }",
            "missing field(s): name",
        );
    }

    #[test]
    fn test_struct_literal_generic_class_stays_permissive() {
        ok("class Box<T> { var value: T; } fn f() { let b = Box { value: 42 }; }");
    }
}

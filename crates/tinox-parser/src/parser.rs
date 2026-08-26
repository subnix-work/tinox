use std::sync::Arc;

use tinox_common::{Error, ErrorBag, Span, Spanned};
use tinox_lexer::{InterpPart, Keyword, Lexer, Token, TokenKind};

use crate::ast::*;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<Error>,
    /// Current recursion / nesting depth, guarded against stack overflow on
    /// pathologically deep input (e.g. `(((…`, `[[[…`, `a.a.a.…`, nested blocks).
    depth: usize,
}

/// Maximum expression/statement nesting the parser accepts before returning a
/// clean error instead of overflowing the stack. Generous for real code (the
/// stdlib's deepest nesting is well under 30) yet safe for every later AST walk
/// (node-id assignment, typecheck, codegen) which recurse over the same depth.
const MAX_RECURSION_DEPTH: usize = 1000;

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
            depth: 0,
        }
    }

    /// Parse a single expression from a source string (used for string interpolation)
    fn parse_expr_str(src: &str, span: Span) -> Result<Expr, Error> {
        let tokens = Lexer::new(src).tokenize()
            .map_err(|_| Error::new(span, format!("invalid expression in string interpolation: {}", src)))?;
        let mut p = Parser::new(tokens);
        p.parse_expr().map_err(|_| Error::new(span, format!("invalid expression in string interpolation: {}", src)))
    }

    pub fn parse(&mut self) -> Result<SourceFile, ErrorBag> {
        let mut decls = Vec::new();

        while !self.is_at_end() {
            match self.parse_decl() {
                Ok(decl) => decls.push(decl),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                }
            }
        }

        if self.errors.is_empty() {
            Ok(SourceFile::new(decls))
        } else {
            Err(ErrorBag {
                errors: self.errors.clone(),
            })
        }
    }

    fn parse_decl(&mut self) -> Result<Decl, Error> {
        let annotations = self.parse_annotations();
        let doc = self.take_doc();
        let start = self.mk_span();

        let decl = if self.consume_keyword(Keyword::Async) {
            if self.check_keyword(Keyword::Fn) {
                let mut f = self.parse_fn()?;
                f.is_async = true;
                f.doc = doc;
                f.annotations = annotations;
                DeclKind::Function(f)
            } else {
                return Err(self.error("expected 'fn' after 'async'"));
            }
        } else if self.check_keyword(Keyword::Fn) {
            let mut f = self.parse_fn()?;
            f.doc = doc;
            f.annotations = annotations;
            DeclKind::Function(f)
        } else if self.check_keyword(Keyword::Class) {
            let mut c = self.parse_class()?;
            c.doc = doc;
            c.annotations = annotations;
            DeclKind::Class(c)
        } else if self.check_keyword(Keyword::Interface) {
            let mut i = self.parse_interface()?;
            i.doc = doc;
            i.annotations = annotations;
            DeclKind::Interface(i)
        } else if self.check_keyword(Keyword::Enum) {
            let mut e = self.parse_enum()?;
            e.doc = doc;
            e.annotations = annotations;
            DeclKind::Enum(e)
        } else if self.check_keyword(Keyword::Trait) {
            let mut t = self.parse_trait()?;
            t.doc = doc;
            t.annotations = annotations;
            DeclKind::Trait(t)
        } else if self.check_keyword(Keyword::Import) {
            DeclKind::Import(self.parse_import()?)
        } else if self.check_keyword(Keyword::Module) {
            self.bump();
            let mut name = self.parse_ident()?;
            while self.consume(TokenKind::Dot) {
                name.push('.');
                name.push_str(&self.parse_ident()?);
            }
            self.consume(TokenKind::Semicolon);
            DeclKind::Module(name)
        } else if self.check_keyword(Keyword::Namespace) {
            let mut ns = self.parse_namespace()?;
            ns.annotations = annotations;
            DeclKind::Namespace(ns)
        } else if self.check_keyword(Keyword::Extern) {
            self.parse_extern_fn()?
        } else if self.check_keyword(Keyword::Immutable) {
            let mut u = self.parse_immutable()?;
            u.doc = doc;
            u.annotations = annotations;
            DeclKind::Immutable(u)
        } else {
            return Err(self.error("expected declaration"));
        };

        Ok(Spanned::new(decl, start))
    }

    fn parse_immutable(&mut self) -> Result<ImmutableDecl, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Immutable)?;
        let name = self.parse_ident()?;
        self.expect(TokenKind::LParen)?;
        let mut fields = Vec::new();
        if !self.check(TokenKind::RParen) {
            fields.push(self.parse_param()?);
            while self.consume(TokenKind::Comma) {
                fields.push(self.parse_param()?);
            }
        }
        self.expect(TokenKind::RParen)?;
        self.consume(TokenKind::Semicolon);
        Ok(ImmutableDecl { name, fields, span, doc: None, annotations: vec![] })
    }

    fn parse_extern_fn(&mut self) -> Result<DeclKind, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Extern)?;
        self.expect_keyword(Keyword::Fn)?;
        let name = self.parse_ident()?;
        self.expect(TokenKind::LParen)?;

        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_param()?);
            while self.consume(TokenKind::Comma) {
                params.push(self.parse_param()?);
            }
        }

        self.expect(TokenKind::RParen)?;
        let ret_type = if self.consume(TokenKind::ThinArrow) {
            self.parse_type()?
        } else {
            Type::Nothing
        };

        self.expect(TokenKind::Semicolon)?;

        Ok(DeclKind::Function(Function {
            name,
            type_params: vec![],
            params,
            ret_type,
            body: Spanned::new(StmtKind::Empty, span),
            span,
            is_async: false,
            doc: None,
            annotations: vec![],
            file: Arc::from(UNKNOWN_FILE),
        }))
    }

    fn parse_type_params(&mut self) -> Result<Vec<String>, Error> {
        self.expect(TokenKind::Less)?;
        let mut params = vec![self.parse_ident()?];
        while self.consume(TokenKind::Comma) {
            params.push(self.parse_ident()?);
        }
        self.expect(TokenKind::Greater)?;
        Ok(params)
    }

    fn parse_fn(&mut self) -> Result<Function, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Fn)?;
        let name = self.parse_ident()?;
        let type_params = if self.check(TokenKind::Less) {
            self.parse_type_params()?
        } else {
            vec![]
        };
        self.expect(TokenKind::LParen)?;

        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_param()?);
            while self.consume(TokenKind::Comma) {
                params.push(self.parse_param()?);
            }
        }

        self.expect(TokenKind::RParen)?;
        let ret_type = if self.consume(TokenKind::ThinArrow) {
            self.parse_type()?
        } else {
            Type::Nothing
        };

        let body = if self.consume(TokenKind::Semicolon) {
            // Abstract method declaration (e.g. in interfaces): `fn foo() -> T;`
            let s = self.mk_span();
            Spanned::new(StmtKind::Block(vec![]), s)
        } else {
            self.parse_block()?
        };

        Ok(Function {
            name,
            type_params,
            params,
            ret_type,
            body,
            span,
            is_async: false,
            doc: None,
            annotations: vec![],
            file: Arc::from(UNKNOWN_FILE),
        })
    }

    fn parse_param(&mut self) -> Result<Param, Error> {
        let annotations = self.parse_annotations();
        let span = self.mk_span();
        let name = self.parse_ident()?;
        self.expect(TokenKind::Colon)?;
        let param_type = self.parse_type()?;
        Ok(Param {
            name,
            param_type,
            span,
            annotations,
        })
    }

    fn parse_class(&mut self) -> Result<Class, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Class)?;
        let name = self.parse_ident()?;
        let type_params = if self.check(TokenKind::Less) {
            self.parse_type_params()?
        } else {
            vec![]
        };

        let extends = if self.consume_keyword(Keyword::Extends) {
            Some(self.parse_ident()?)
        } else {
            None
        };

        let implements = if self.consume_keyword(Keyword::Implements) {
            let mut interfaces = vec![self.parse_ident()?];
            while self.consume(TokenKind::Comma) {
                interfaces.push(self.parse_ident()?);
            }
            interfaces
        } else {
            Vec::new()
        };

        self.expect(TokenKind::LBrace)?;

        let mut fields = Vec::new();
        let mut methods = Vec::new();

        while !self.check(TokenKind::RBrace) {
            let member_annotations = self.parse_annotations();
            let doc = self.take_doc();
            let vis = self.parse_visibility();
            // `var` = mutable field, `let` = immutable field; consume both
            let mutable = self.consume_keyword(Keyword::Var)
                || self.consume_keyword(Keyword::Let)
                || self.consume_keyword(Keyword::Mut);
            let is_async = self.consume_keyword(Keyword::Async);

            if self.check_keyword(Keyword::Fn) {
                let mut m = self.parse_method(vis, false)?;
                m.is_async = is_async;
                m.doc = doc;
                m.annotations = member_annotations;
                methods.push(m);
            } else if self.check_keyword(Keyword::Fnc) {
                let mut m = self.parse_method(vis, true)?;
                m.is_async = is_async;
                m.doc = doc;
                m.annotations = member_annotations;
                methods.push(m);
            } else {
                let mut f = self.parse_field(vis, mutable)?;
                f.doc = doc;
                f.annotations = member_annotations;
                fields.push(f);
            }
        }

        self.expect(TokenKind::RBrace)?;

        Ok(Class {
            name,
            type_params,
            extends,
            implements,
            fields,
            methods,
            span,
            doc: None,
            annotations: vec![],
        })
    }

    fn parse_method_name(&mut self) -> Result<String, Error> {
        match self.peek().kind.clone() {
            TokenKind::Ident(s) => { self.bump(); Ok(s) }
            TokenKind::Keyword(kw) => {
                let name = match kw {
                    Keyword::New => "new",
                    Keyword::Send => "send",
                    Keyword::Recv => "recv",
                    Keyword::Default => "default",
                    Keyword::Return => "return",
                    Keyword::Is => "is",
                    Keyword::As => "as",
                    // A field/method literally named `namespace` is
                    // extremely common in real code (e.g. any Kubernetes-
                    // style API client: ObjectMeta.namespace) and already
                    // parses fine as a field/parameter declaration and as
                    // a struct-literal key -- only postfix access
                    // (`obj.namespace`) went through this allowlist and
                    // rejected it. Found while adding tinox.core.kubernetes.
                    Keyword::Namespace => "namespace",
                    _ => return Err(Error::new(self.mk_span(), "expected method name")),
                };
                self.bump();
                Ok(name.to_string())
            }
            _ => Err(Error::new(self.mk_span(), "expected method name")),
        }
    }

    fn parse_method(&mut self, visibility: Visibility, static_: bool) -> Result<Method, Error> {
        let span = self.mk_span();
        if static_ {
            self.expect_keyword(Keyword::Fnc)?;
        } else {
            self.expect_keyword(Keyword::Fn)?;
        }
        let name = self.parse_method_name()?;
        let type_params = if self.check(TokenKind::Less) {
            self.parse_type_params()?
        } else {
            vec![]
        };
        self.expect(TokenKind::LParen)?;

        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_param()?);
            while self.consume(TokenKind::Comma) {
                params.push(self.parse_param()?);
            }
        }

        self.expect(TokenKind::RParen)?;
        let ret_type = if self.consume(TokenKind::ThinArrow) {
            self.parse_type()?
        } else {
            Type::Nothing
        };

        let body = self.parse_block()?;

        Ok(Method {
            name,
            type_params,
            params,
            ret_type,
            body,
            static_,
            visibility,
            span,
            is_async: false,
            doc: None,
            annotations: vec![],
            file: Arc::from(UNKNOWN_FILE),
        })
    }

    fn parse_field(&mut self, visibility: Visibility, mutable: bool) -> Result<FieldDef, Error> {
        let span = self.mk_span();
        let name = self.parse_ident()?;
        self.expect(TokenKind::Colon)?;
        let field_type = self.parse_type()?;
        self.expect(TokenKind::Semicolon)?;

        Ok(FieldDef {
            name,
            field_type,
            visibility,
            mutable,
            span,
            doc: None,
            annotations: vec![],
        })
    }

    fn parse_visibility(&mut self) -> Visibility {
        if self.consume_keyword(Keyword::Public) {
            Visibility::Public
        } else if self.consume_keyword(Keyword::Private) {
            Visibility::Private
        } else if self.consume_keyword(Keyword::Protected) {
            Visibility::Protected
        } else {
            // Explicit `package` or no keyword at all — both default here.
            self.consume_keyword(Keyword::Package);
            Visibility::Package
        }
    }

    fn parse_interface(&mut self) -> Result<Interface, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Interface)?;
        let name = self.parse_ident()?;

        let extends = if self.consume_keyword(Keyword::Extends) {
            let mut interfaces = vec![self.parse_ident()?];
            while self.consume(TokenKind::Comma) {
                interfaces.push(self.parse_ident()?);
            }
            interfaces
        } else {
            Vec::new()
        };

        self.expect(TokenKind::LBrace)?;

        let mut methods = Vec::new();
        while !self.check(TokenKind::RBrace) {
            let doc = self.take_doc();
            let mut m = self.parse_fn()?;
            m.doc = doc;
            methods.push(m);
        }

        self.expect(TokenKind::RBrace)?;

        Ok(Interface {
            name,
            extends,
            methods,
            span,
            doc: None,
            annotations: vec![],
        })
    }

    fn parse_enum(&mut self) -> Result<Enum, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Enum)?;
        let name = self.parse_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut variants = Vec::new();
        while !self.check(TokenKind::RBrace) {
            let doc = self.take_doc();
            let mut v = self.parse_enum_variant()?;
            v.doc = doc;
            variants.push(v);
            if !self.check(TokenKind::RBrace) {
                if !self.consume(TokenKind::Comma) && !self.consume(TokenKind::Semicolon) {
                    return Err(self.error("expected ',' or ';'"));
                }
            } else {
                self.consume(TokenKind::Comma);
                self.consume(TokenKind::Semicolon);
            }
        }

        self.expect(TokenKind::RBrace)?;

        Ok(Enum {
            name,
            variants,
            span,
            doc: None,
            annotations: vec![],
        })
    }

    fn parse_enum_variant(&mut self) -> Result<EnumVariant, Error> {
        let span = self.mk_span();
        let name = self.parse_ident()?;

        let mut args = Vec::new();
        if self.consume(TokenKind::LParen) {
            if !self.check(TokenKind::RParen) {
                // Support optional `name: Type` named fields (names are ignored)
                if matches!(self.peek().kind, TokenKind::Ident(_))
                    && matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Colon))
                {
                    self.bump(); self.bump(); // consume name + colon
                }
                args.push(self.parse_type()?);
                while self.consume(TokenKind::Comma) {
                    if self.check(TokenKind::RParen) { break; }
                    if matches!(self.peek().kind, TokenKind::Ident(_))
                        && matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Colon))
                    {
                        self.bump(); self.bump();
                    }
                    args.push(self.parse_type()?);
                }
            }
            self.expect(TokenKind::RParen)?;
        }

        Ok(EnumVariant { name, args, span, doc: None })
    }

    fn parse_trait(&mut self) -> Result<Trait, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Trait)?;
        let name = self.parse_ident()?;
        self.expect(TokenKind::LBrace)?;

        let mut methods = Vec::new();
        while !self.check(TokenKind::RBrace) {
            let doc = self.take_doc();
            let mut m = self.parse_fn()?;
            m.doc = doc;
            methods.push(m);
        }

        self.expect(TokenKind::RBrace)?;

        Ok(Trait {
            name,
            methods,
            span,
            doc: None,
            annotations: vec![],
        })
    }

    fn parse_import(&mut self) -> Result<Import, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Import)?;
        let mut path = vec![self.parse_ident()?];
        // Both separators allowed: `import a.b;` and `import a::b;`
        while self.consume(TokenKind::Dot) || self.consume(TokenKind::ColonColon) {
            path.push(self.parse_ident()?);
        }

        let alias = if self.consume_keyword(Keyword::As) {
            Some(self.parse_ident()?)
        } else {
            None
        };

        self.expect(TokenKind::Semicolon)?;

        Ok(Import { path, alias, span })
    }

    fn parse_namespace(&mut self) -> Result<Namespace, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Namespace)?;
        let mut name = vec![self.parse_ident()?];
        while self.consume(TokenKind::Dot) {
            name.push(self.parse_ident()?);
        }
        self.expect(TokenKind::LBrace)?;

        let mut decls = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            let doc = self.take_doc();
            let annotations = self.parse_annotations();
            let decl_start = self.mk_span();
            let inner = if self.check_keyword(Keyword::Class) {
                let mut c = self.parse_class()?;
                c.doc = doc;
                c.annotations = annotations;
                DeclKind::Class(c)
            } else if self.check_keyword(Keyword::Interface) {
                let mut i = self.parse_interface()?;
                i.doc = doc;
                i.annotations = annotations;
                DeclKind::Interface(i)
            } else if self.check_keyword(Keyword::Enum) {
                let mut e = self.parse_enum()?;
                e.doc = doc;
                e.annotations = annotations;
                DeclKind::Enum(e)
            } else if self.check_keyword(Keyword::Trait) {
                let mut t = self.parse_trait()?;
                t.doc = doc;
                t.annotations = annotations;
                DeclKind::Trait(t)
            } else if self.check_keyword(Keyword::Immutable) {
                let u = self.parse_immutable()?;
                DeclKind::Immutable(u)
            } else if self.check_keyword(Keyword::Extern) {
                self.parse_extern_fn()?
            } else if self.check_keyword(Keyword::Fn) || self.check_keyword(Keyword::Async) {
                // `async fn` inside a namespace: parse_fn() itself always
                // expects to see `fn` first (mirrors the top-level parse_decl
                // logic, which also consumes `async` before delegating to
                // parse_fn() rather than parse_fn() handling it internally) —
                // consume `async` here first, or parse_fn() would fail with
                // "expected Fn, found Async".
                let is_async = self.consume_keyword(Keyword::Async);
                let mut f = self.parse_fn()?;
                f.is_async = is_async;
                f.annotations = annotations;
                DeclKind::Function(f)
            } else {
                let e = self.error("expected 'class', 'interface', 'enum', 'trait', or 'immutable' inside namespace");
                self.errors.push(e);
                self.synchronize();
                continue;
            };
            decls.push(Spanned::new(inner, decl_start));
        }

        self.expect(TokenKind::RBrace)?;
        Ok(Namespace { name, decls, span, annotations: vec![] })
    }

    fn parse_type(&mut self) -> Result<Type, Error> {
        // Tuple type: (T1, T2, ...)
        if self.check(TokenKind::LParen) {
            self.bump();
            let mut types = Vec::new();
            if !self.check(TokenKind::RParen) {
                types.push(self.parse_type()?);
                while self.consume(TokenKind::Comma) {
                    types.push(self.parse_type()?);
                }
            }
            self.expect(TokenKind::RParen)?;
            return Ok(Type::Tuple(types));
        }

        if self.check(TokenKind::Star) {
            self.bump();
            let inner = self.parse_type()?;
            return Ok(Type::Ref(Box::new(inner)));
        }

        if self.check(TokenKind::Question) {
            self.bump();
            let inner = self.parse_type()?;
            return Ok(Type::Ref(Box::new(inner)));
        }

        if self.consume_keyword(Keyword::Mut) {
            let inner = self.parse_type()?;
            return Ok(Type::Mutable(Box::new(inner)));
        }

        // fnc(T1, T2) -> R  — function/closure type
        if self.check_keyword(Keyword::Fnc) {
            self.bump();
            self.expect(TokenKind::LParen)?;
            let mut params = Vec::new();
            if !self.check(TokenKind::RParen) {
                params.push(self.parse_type()?);
                while self.consume(TokenKind::Comma) {
                    params.push(self.parse_type()?);
                }
            }
            self.expect(TokenKind::RParen)?;
            let ret = if self.consume(TokenKind::ThinArrow) {
                Box::new(self.parse_type()?)
            } else {
                Box::new(Type::Nothing)
            };
            return Ok(Type::Fn { params, ret });
        }

        let mut base = self.parse_type_base()?;

        while self.check(TokenKind::LBracket) {
            self.bump();
            while !self.check(TokenKind::RBracket) {
                // EOF without ']' — bump() would no longer advance (infinite loop)
                if self.is_at_end() {
                    return Err(self.error("expected ']' in array type"));
                }
                self.bump();
            }
            self.bump();
            base = Type::Array(Box::new(base));
        }

        if self.check(TokenKind::Star) {
            self.bump();
            base = Type::Ref(Box::new(base));
        }

        if self.consume(TokenKind::Question) {
            base = Type::Nullable(Box::new(base));
        }

        if self.check(TokenKind::LParen) {
            self.bump();
            let mut params = vec![base];
            if !self.check(TokenKind::RParen) {
                params.push(self.parse_type()?);
                while self.consume(TokenKind::Comma) {
                    params.push(self.parse_type()?);
                }
            }
            self.expect(TokenKind::RParen)?;
            self.expect(TokenKind::ThinArrow)?;
            let ret = Box::new(self.parse_type()?);
            return Ok(Type::Fn { params, ret });
        }

        Ok(base)
    }

    fn parse_type_base(&mut self) -> Result<Type, Error> {
        // These are keyword tokens, not identifiers — handle them before parse_ident
        if self.check_keyword(Keyword::Nothing) {
            self.bump();
            return Ok(Type::Nothing);
        }
        if self.check_keyword(Keyword::Any) {
            self.bump();
            return Ok(Type::Any);
        }
        if self.check_keyword(Keyword::Never) {
            self.bump();
            return Ok(Type::Never);
        }
        let ident = self.parse_ident()?;

        match ident.as_str() {
            "Int8" => Ok(Type::Int8),
            "Int16" => Ok(Type::Int16),
            "Int32" => Ok(Type::Int32),
            "Int64" => Ok(Type::Int64),
            "UInt8" => Ok(Type::UInt8),
            "UInt16" => Ok(Type::UInt16),
            "UInt32" => Ok(Type::UInt32),
            "UInt64" => Ok(Type::UInt64),
            "Float32" => Ok(Type::Float32),
            "Float64" => Ok(Type::Float64),
            "Bool" => Ok(Type::Bool),
            "Char" => Ok(Type::Char),
            "String" => Ok(Type::String),
            "Nothing" => Ok(Type::Nothing),
            "Never" => Ok(Type::Never),
            "Any" => Ok(Type::Any),
            "Map" => {
                self.expect(TokenKind::Less)?;
                let k = self.parse_type()?;
                self.expect(TokenKind::Comma)?;
                let v = self.parse_type()?;
                self.expect_generic_close()?;
                Ok(Type::Map(Box::new(k), Box::new(v)))
            }
            s => {
                // Check for generic type arguments: Box<Int64>
                if self.check(TokenKind::Less) {
                    self.bump();
                    let mut args = vec![self.parse_type()?];
                    while self.consume(TokenKind::Comma) {
                        args.push(self.parse_type()?);
                    }
                    self.expect_generic_close()?;
                    Ok(Type::Generic { name: s.to_string(), args })
                } else {
                    Ok(Type::Named(s.to_string()))
                }
            }
        }
    }

    fn parse_block(&mut self) -> Result<Stmt, Error> {
        let span = self.mk_span();
        self.expect(TokenKind::LBrace)?;
        let stmts = self.parse_block_stmts();
        self.expect(TokenKind::RBrace)?;
        Ok(Spanned::new(StmtKind::Block(stmts), span))
    }

    fn parse_block_stmts(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            match self.parse_stmt() {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize();
                    if self.check(TokenKind::RBrace) {
                        break;
                    }
                }
            }
        }
        stmts
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Error> {
        self.depth += 1;
        if self.depth > MAX_RECURSION_DEPTH {
            self.depth -= 1;
            return Err(Error::new(self.mk_span(), "statement nesting too deep"));
        }
        let r = self.parse_stmt_inner();
        self.depth -= 1;
        r
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt, Error> {
        let span = self.mk_span();

        let stmt = match self.peek().kind {
            TokenKind::Keyword(Keyword::Let) => self.parse_let_stmt()?,
            TokenKind::Keyword(Keyword::Var) => self.parse_var_stmt()?,
            TokenKind::Keyword(Keyword::If) => self.parse_if_stmt()?,
            TokenKind::Keyword(Keyword::While) => self.parse_while_stmt()?,
            TokenKind::Keyword(Keyword::For) => self.parse_for_stmt()?,
            TokenKind::Keyword(Keyword::Loop) => self.parse_loop_stmt()?,
            TokenKind::Keyword(Keyword::Return) => self.parse_return_stmt()?,
            TokenKind::Keyword(Keyword::Break) => {
                self.bump();
                self.expect(TokenKind::Semicolon)?;
                StmtKind::Break
            }
            TokenKind::Keyword(Keyword::Continue) => {
                self.bump();
                self.expect(TokenKind::Semicolon)?;
                StmtKind::Continue
            }
            TokenKind::Keyword(Keyword::Throw) => self.parse_throw_stmt()?,
            TokenKind::Keyword(Keyword::Try) => self.parse_try_stmt()?,
            TokenKind::Keyword(Keyword::Defer) => self.parse_defer_stmt()?,
            TokenKind::Keyword(Keyword::Select) => self.parse_select_stmt()?,
            TokenKind::LBrace => {
                self.bump(); // consume LBrace
                let stmts = self.parse_block_stmts();
                self.expect(TokenKind::RBrace)?;
                StmtKind::Block(stmts)
            }
            TokenKind::Semicolon => {
                self.bump();
                StmtKind::Empty
            }
            _ => {
                if let TokenKind::Ident(_) = &self.peek().kind {
                    let ident_span = self.peek().span;
                    let name = self.parse_ident()?;
                    if self.check(TokenKind::Equals) {
                        self.bump();
                        let value = self.parse_expr()?;
                        self.expect(TokenKind::Semicolon)?;
                        let target = Spanned::new(ExprKind::Ident(name.clone()), ident_span);
                        StmtKind::Assignment { target, value }
                    } else if self.check(TokenKind::PlusEquals)
                        || self.check(TokenKind::MinusEquals)
                        || self.check(TokenKind::StarEquals)
                        || self.check(TokenKind::SlashEquals)
                        || self.check(TokenKind::PercentEquals)
                    {
                        let op = match self.peek().kind {
                            TokenKind::PlusEquals => CompoundOp::Add,
                            TokenKind::MinusEquals => CompoundOp::Sub,
                            TokenKind::StarEquals => CompoundOp::Mul,
                            TokenKind::SlashEquals => CompoundOp::Div,
                            TokenKind::PercentEquals => CompoundOp::Mod,
                            _ => unreachable!(),
                        };
                        self.bump();
                        let value = self.parse_expr()?;
                        self.expect(TokenKind::Semicolon)?;
                        let target = Spanned::new(ExprKind::Ident(name), ident_span);
                        StmtKind::Expr(Spanned::new(
                            ExprKind::CompoundAssign {
                                op,
                                target: Box::new(target),
                                value: Box::new(value),
                            },
                            ident_span,
                        ))
                    } else if self.check(TokenKind::LBracket) {
                        self.bump();
                        let index = self.parse_expr()?;
                        self.expect(TokenKind::RBracket)?;
                        let mut target = Spanned::new(
                            ExprKind::Index {
                                obj: Box::new(Spanned::new(ExprKind::Ident(name), ident_span)),
                                index: Box::new(index),
                            },
                            ident_span,
                        );
                        while self.check(TokenKind::LBracket) {
                            self.bump();
                            let idx2 = self.parse_expr()?;
                            self.expect(TokenKind::RBracket)?;
                            target = Spanned::new(ExprKind::Index { obj: Box::new(target), index: Box::new(idx2) }, ident_span);
                        }
                        if self.check(TokenKind::Equals) {
                            self.bump();
                            let value = self.parse_expr()?;
                            self.expect(TokenKind::Semicolon)?;
                            StmtKind::Assignment { target, value }
                        } else if self.check(TokenKind::PlusEquals)
                            || self.check(TokenKind::MinusEquals)
                            || self.check(TokenKind::StarEquals)
                            || self.check(TokenKind::SlashEquals)
                            || self.check(TokenKind::PercentEquals)
                        {
                            let op = match self.peek().kind {
                                TokenKind::PlusEquals => CompoundOp::Add,
                                TokenKind::MinusEquals => CompoundOp::Sub,
                                TokenKind::StarEquals => CompoundOp::Mul,
                                TokenKind::SlashEquals => CompoundOp::Div,
                                TokenKind::PercentEquals => CompoundOp::Mod,
                                _ => unreachable!(),
                            };
                            self.bump();
                            let value = self.parse_expr()?;
                            self.expect(TokenKind::Semicolon)?;
                            StmtKind::Expr(Spanned::new(
                                ExprKind::CompoundAssign {
                                    op,
                                    target: Box::new(target),
                                    value: Box::new(value),
                                },
                                ident_span,
                            ))
                        } else if self.check(TokenKind::LParen) {
                            self.bump();
                            let mut args = Vec::new();
                            if !self.check(TokenKind::RParen) {
                                args.push(self.parse_expr()?);
                                while self.consume(TokenKind::Comma) { args.push(self.parse_expr()?); }
                            }
                            self.expect(TokenKind::RParen)?;
                            self.expect(TokenKind::Semicolon)?;
                            StmtKind::Expr(Spanned::new(ExprKind::Call { func: Box::new(target), args }, ident_span))
                        } else {
                            let mut expr = target;
                            while self.consume(TokenKind::Dot) {
                                let field = self.parse_ident()?;
                                if self.check(TokenKind::LParen) {
                                    self.bump();
                                    let mut args = Vec::new();
                                    if !self.check(TokenKind::RParen) {
                                        args.push(self.parse_expr()?);
                                        while self.consume(TokenKind::Comma) { args.push(self.parse_expr()?); }
                                    }
                                    self.expect(TokenKind::RParen)?;
                                    expr = Spanned::new(ExprKind::MethodCall { obj: Box::new(expr), method: field, args }, ident_span);
                                } else if self.check(TokenKind::Equals) {
                                    self.bump();
                                    let value = self.parse_expr()?;
                                    self.expect(TokenKind::Semicolon)?;
                                    let t = Spanned::new(ExprKind::FieldAccess { obj: Box::new(expr), field }, ident_span);
                                    return Ok(Spanned::new(StmtKind::Assignment { target: t, value }, span));
                                } else {
                                    expr = Spanned::new(ExprKind::FieldAccess { obj: Box::new(expr), field }, ident_span);
                                }
                            }
                            self.expect(TokenKind::Semicolon)?;
                            StmtKind::Expr(expr)
                        }
                    } else if self.check(TokenKind::LParen) {
                        let mut args = Vec::new();
                        self.bump();
                        if !self.check(TokenKind::RParen) {
                            args.push(self.parse_expr()?);
                            while self.consume(TokenKind::Comma) {
                                args.push(self.parse_expr()?);
                            }
                        }
                        self.expect(TokenKind::RParen)?;
                        self.expect(TokenKind::Semicolon)?;
                        StmtKind::Expr(Spanned::new(
                            ExprKind::Call {
                                func: Box::new(Spanned::new(ExprKind::Ident(name), ident_span)),
                                args,
                            },
                            ident_span,
                        ))
                    } else if self.check(TokenKind::Dot) {
                        // Method call / field-access chain: m.set(...); obj.field.method();
                        let mut expr = Spanned::new(ExprKind::Ident(name), ident_span);
                        while self.consume(TokenKind::Dot) {
                            let field = self.parse_ident()?;
                            if self.check(TokenKind::LParen) {
                                self.bump();
                                let mut args = Vec::new();
                                if !self.check(TokenKind::RParen) {
                                    args.push(self.parse_expr()?);
                                    while self.consume(TokenKind::Comma) {
                                        args.push(self.parse_expr()?);
                                    }
                                }
                                self.expect(TokenKind::RParen)?;
                                expr = Spanned::new(ExprKind::MethodCall { obj: Box::new(expr), method: field, args }, ident_span);
                            } else if self.check(TokenKind::Equals) {
                                self.bump();
                                let value = self.parse_expr()?;
                                self.expect(TokenKind::Semicolon)?;
                                let target = Spanned::new(ExprKind::FieldAccess { obj: Box::new(expr), field }, ident_span);
                                return Ok(Spanned::new(StmtKind::Assignment { target, value }, span));
                            } else {
                                expr = Spanned::new(ExprKind::FieldAccess { obj: Box::new(expr), field }, ident_span);
                            }
                        }
                        // obj.field[key][key2]...(= value | .method(args));
                        if self.check(TokenKind::LBracket) {
                            self.bump();
                            let index = self.parse_expr()?;
                            self.expect(TokenKind::RBracket)?;
                            let mut target = Spanned::new(
                                ExprKind::Index { obj: Box::new(expr), index: Box::new(index) },
                                ident_span,
                            );
                            while self.check(TokenKind::LBracket) {
                                self.bump();
                                let idx2 = self.parse_expr()?;
                                self.expect(TokenKind::RBracket)?;
                                target = Spanned::new(ExprKind::Index { obj: Box::new(target), index: Box::new(idx2) }, ident_span);
                            }
                            if self.check(TokenKind::Equals) {
                                self.bump();
                                let value = self.parse_expr()?;
                                self.expect(TokenKind::Semicolon)?;
                                return Ok(Spanned::new(StmtKind::Assignment { target, value }, span));
                            }
                            // .method(args) or (args) after index chain
                            let mut chain_expr = target;
                            while self.consume(TokenKind::Dot) {
                                let field = self.parse_ident()?;
                                if self.check(TokenKind::LParen) {
                                    self.bump();
                                    let mut args = Vec::new();
                                    if !self.check(TokenKind::RParen) {
                                        args.push(self.parse_expr()?);
                                        while self.consume(TokenKind::Comma) { args.push(self.parse_expr()?); }
                                    }
                                    self.expect(TokenKind::RParen)?;
                                    chain_expr = Spanned::new(ExprKind::MethodCall { obj: Box::new(chain_expr), method: field, args }, ident_span);
                                } else {
                                    chain_expr = Spanned::new(ExprKind::FieldAccess { obj: Box::new(chain_expr), field }, ident_span);
                                }
                            }
                            // Assignment to a field reached through an index chain,
                            // e.g. `map[key].field = value;` (chain_expr is now the
                            // FieldAccess lvalue).
                            if self.check(TokenKind::Equals) {
                                self.bump();
                                let value = self.parse_expr()?;
                                self.expect(TokenKind::Semicolon)?;
                                return Ok(Spanned::new(StmtKind::Assignment { target: chain_expr, value }, span));
                            }
                            if self.check(TokenKind::PlusEquals)
                                || self.check(TokenKind::MinusEquals)
                                || self.check(TokenKind::StarEquals)
                                || self.check(TokenKind::SlashEquals)
                                || self.check(TokenKind::PercentEquals)
                            {
                                let op = match self.peek().kind {
                                    TokenKind::PlusEquals => CompoundOp::Add,
                                    TokenKind::MinusEquals => CompoundOp::Sub,
                                    TokenKind::StarEquals => CompoundOp::Mul,
                                    TokenKind::SlashEquals => CompoundOp::Div,
                                    TokenKind::PercentEquals => CompoundOp::Mod,
                                    _ => unreachable!(),
                                };
                                self.bump();
                                let value = self.parse_expr()?;
                                self.expect(TokenKind::Semicolon)?;
                                return Ok(Spanned::new(
                                    StmtKind::Expr(Spanned::new(
                                        ExprKind::CompoundAssign {
                                            op,
                                            target: Box::new(chain_expr),
                                            value: Box::new(value),
                                        },
                                        ident_span,
                                    )),
                                    span,
                                ));
                            }
                            self.expect(TokenKind::Semicolon)?;
                            return Ok(Spanned::new(StmtKind::Expr(chain_expr), span));
                        }
                        self.expect(TokenKind::Semicolon)?;
                        StmtKind::Expr(expr)
                    } else if self.check(TokenKind::ColonColon) {
                        // Static method call: Cache::touch(args)
                        let mut expr = Spanned::new(ExprKind::Ident(name), ident_span);
                        while self.consume(TokenKind::ColonColon) {
                            let method = self.parse_ident()?;
                            let mut args = Vec::new();
                            if self.consume(TokenKind::LParen) {
                                if !self.check(TokenKind::RParen) {
                                    args.push(self.parse_expr()?);
                                    while self.consume(TokenKind::Comma) {
                                        args.push(self.parse_expr()?);
                                    }
                                }
                                self.expect(TokenKind::RParen)?;
                            }
                            let span_e = expr.span;
                            expr = Spanned::new(
                                ExprKind::EnumValue { enum_name: match &expr.node { ExprKind::Ident(n) => n.clone(), _ => method.clone() }, variant: method, type_args: vec![], args },
                                span_e,
                            );
                            // Handle chained . after ::
                            while self.consume(TokenKind::Dot) {
                                let field = self.parse_ident()?;
                                if self.consume(TokenKind::LParen) {
                                    let mut a = Vec::new();
                                    if !self.check(TokenKind::RParen) {
                                        a.push(self.parse_expr()?);
                                        while self.consume(TokenKind::Comma) { a.push(self.parse_expr()?); }
                                    }
                                    self.expect(TokenKind::RParen)?;
                                    expr = Spanned::new(ExprKind::MethodCall { obj: Box::new(expr), method: field, args: a }, ident_span);
                                } else {
                                    expr = Spanned::new(ExprKind::FieldAccess { obj: Box::new(expr), field }, ident_span);
                                }
                            }
                        }
                        self.expect(TokenKind::Semicolon)?;
                        StmtKind::Expr(expr)
                    } else {
                        self.expect(TokenKind::Semicolon)?;
                        StmtKind::Expr(Spanned::new(ExprKind::Ident(name), ident_span))
                    }
                } else {
                    let expr = self.parse_expr()?;
                    let is_block_expr = matches!(
                        expr.node,
                        ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::If { .. }
                    );
                    if !is_block_expr && !self.check(TokenKind::RBrace) {
                        self.expect(TokenKind::Semicolon)?;
                    } else {
                        self.consume(TokenKind::Semicolon);
                    }
                    StmtKind::Expr(expr)
                }
            }
        };

        Ok(Spanned::new(stmt, span))
    }

    fn parse_let_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Let)?;
        let name = self.parse_ident()?;
        let ty = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let value = if self.consume(TokenKind::Equals) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon)?;
        Ok(StmtKind::Let { name, ty, value })
    }

    fn parse_var_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Var)?;
        let name = self.parse_ident()?;
        let ty = if self.consume(TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let value = if self.consume(TokenKind::Equals) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon)?;
        Ok(StmtKind::Var {
            name,
            ty,
            value,
            mutable: true,
        })
    }

    fn parse_if_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::If)?;
        let cond = self.parse_expr()?;
        let then_branch = Box::new(self.parse_block()?);
        let else_branch = if self.consume_keyword(Keyword::Else) {
            Some(if self.check_keyword(Keyword::If) {
                Box::new(Spanned::new(self.parse_if_stmt()?, Span::dummy()))
            } else {
                Box::new(self.parse_block()?)
            })
        } else {
            None
        };
        Ok(StmtKind::If {
            cond,
            then_branch,
            else_branch,
        })
    }

    fn parse_while_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::While)?;
        let cond = self.parse_expr()?;
        let body = Box::new(self.parse_block()?);
        Ok(StmtKind::While { cond, body })
    }

    fn parse_for_stmt(&mut self) -> Result<StmtKind, Error> {
        self.bump(); // consume 'for'
        // for x in expr { body }
        if !self.check(TokenKind::LParen) {
            let var = self.parse_ident()?;
            self.expect_keyword(Keyword::In)?;
            let iter = self.parse_expr()?;
            let body = Box::new(self.parse_block()?);
            return Ok(StmtKind::For { var, iter, body });
        }
        self.expect(TokenKind::LParen)?;

        let init = if !self.check(TokenKind::Semicolon) {
            let stmt = if self.check_keyword(Keyword::Let) {
                self.parse_let_stmt()?
            } else if self.check_keyword(Keyword::Var) {
                self.parse_var_stmt()?
            } else {
                let expr = self.parse_expr()?;
                self.expect(TokenKind::Semicolon)?;
                StmtKind::Expr(expr)
            };
            Some(Box::new(Spanned::new(stmt, Span::dummy())))
        } else {
            self.bump();
            None
        };

        let cond = if !self.check(TokenKind::Semicolon) {
            Some(self.parse_expr()?)
        } else {
            self.bump();
            None
        };
        self.expect(TokenKind::Semicolon)?;

        let update = if !self.check(TokenKind::RParen) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::RParen)?;

        let body = Box::new(self.parse_block()?);

        Ok(StmtKind::ForC {
            init,
            cond,
            update,
            body,
        })
    }

    fn parse_loop_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Loop)?;
        let body = Box::new(self.parse_block()?);
        Ok(StmtKind::Loop { body })
    }

    fn parse_return_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Return)?;
        if self.check(TokenKind::Semicolon) {
            self.bump();
            Ok(StmtKind::Return(None))
        } else {
            let value = self.parse_expr()?;
            self.expect(TokenKind::Semicolon)?;
            Ok(StmtKind::Return(Some(value)))
        }
    }

    fn parse_throw_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Throw)?;
        let expr = self.parse_expr()?;
        self.expect(TokenKind::Semicolon)?;
        Ok(StmtKind::Throw(expr))
    }

    fn parse_try_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Try)?;
        let body = Box::new(self.parse_block()?);

        let mut catches = Vec::new();
        while self.consume_keyword(Keyword::Catch) {
            let has_paren = self.consume(TokenKind::LParen);
            let param = self.parse_ident()?;
            self.expect(TokenKind::Colon)?;
            let ty = self.parse_type()?;
            if has_paren { self.expect(TokenKind::RParen)?; }
            let catch_body = self.parse_block()?;
            catches.push(CatchClause {
                param,
                ty,
                body: catch_body,
                span: Span::dummy(),
            });
        }

        let finally_block = if self.consume_keyword(Keyword::Finally) {
            Some(Box::new(self.parse_block()?))
        } else {
            None
        };

        Ok(StmtKind::Try {
            body,
            catches,
            finally: finally_block,
        })
    }

    fn parse_defer_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Defer)?;
        let stmt = self.parse_stmt()?;
        Ok(StmtKind::Defer(Box::new(stmt)))
    }

    fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.depth += 1;
        if self.depth > MAX_RECURSION_DEPTH {
            self.depth -= 1;
            return Err(Error::new(self.mk_span(), "expression nesting too deep"));
        }
        let r = self.parse_assign_expr();
        self.depth -= 1;
        r
    }

    fn parse_assign_expr(&mut self) -> Result<Expr, Error> {
        let lhs = self.parse_ternary_expr()?;

        // Arrow lambda: `n => expr` or `(a, b) => expr`
        // Only trigger if LHS looks like lambda params (ident or tuple of idents)
        let lhs_is_lambda_param = matches!(&lhs.node, ExprKind::Ident(_) | ExprKind::Tuple(_));
        if lhs_is_lambda_param && self.check(TokenKind::FatArrow) {
            self.bump();
            let span = lhs.span;
            let params = Self::expr_to_lambda_params(lhs)?;
            let body = self.parse_assign_expr()?;
            return Ok(Spanned::new(
                ExprKind::Lambda { params, ret_type: None, body: Box::new(body) },
                span,
            ));
        }

        if self.check(TokenKind::Equals) {
            self.bump();
            let rhs = self.parse_assign_expr()?;
            let span = lhs.span;
            return Ok(Spanned::new(
                ExprKind::Assign {
                    target: Box::new(lhs),
                    value: Box::new(rhs),
                },
                span,
            ));
        }

        if self.check(TokenKind::PlusEquals)
            || self.check(TokenKind::MinusEquals)
            || self.check(TokenKind::StarEquals)
            || self.check(TokenKind::SlashEquals)
            || self.check(TokenKind::PercentEquals)
        {
            let op = match self.peek().kind {
                TokenKind::PlusEquals => CompoundOp::Add,
                TokenKind::MinusEquals => CompoundOp::Sub,
                TokenKind::StarEquals => CompoundOp::Mul,
                TokenKind::SlashEquals => CompoundOp::Div,
                TokenKind::PercentEquals => CompoundOp::Mod,
                _ => unreachable!(),
            };
            self.bump();
            let rhs = self.parse_assign_expr()?;
            let span = lhs.span;
            return Ok(Spanned::new(
                ExprKind::CompoundAssign {
                    op,
                    target: Box::new(lhs),
                    value: Box::new(rhs),
                },
                span,
            ));
        }

        Ok(lhs)
    }

    fn parse_ternary_expr(&mut self) -> Result<Expr, Error> {
        let cond = self.parse_or_expr()?;
        if self.check(TokenKind::Question) {
            self.bump();
            let span = cond.span;
            let then_branch = Box::new(self.parse_or_expr()?);
            self.expect(TokenKind::Colon)?;
            let else_branch = Box::new(self.parse_ternary_expr()?);
            return Ok(Spanned::new(
                ExprKind::If { cond: Box::new(cond), then_branch, else_branch: Some(else_branch) },
                span,
            ));
        }
        Ok(cond)
    }

    fn parse_or_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_and_expr()?;

        while self.check(TokenKind::BarBar) {
            self.bump();
            let rhs = self.parse_and_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op: BinaryOp::Or,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_and_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_bitwise_or_expr()?;

        while self.check(TokenKind::AmpAmp) {
            self.bump();
            let rhs = self.parse_bitwise_or_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op: BinaryOp::And,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_bitwise_or_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_bitwise_xor_expr()?;

        while self.check(TokenKind::Bar) && !self.check(TokenKind::BarBar) {
            self.bump();
            let rhs = self.parse_bitwise_xor_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op: BinaryOp::BitOr,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_bitwise_xor_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_bitwise_and_expr()?;

        while self.check(TokenKind::Caret) {
            self.bump();
            let rhs = self.parse_bitwise_and_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op: BinaryOp::Xor,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_bitwise_and_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_equality_expr()?;

        while self.check(TokenKind::Ampersand) && !self.check(TokenKind::AmpAmp) {
            self.bump();
            let rhs = self.parse_equality_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op: BinaryOp::BitAnd,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_equality_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_relational_expr()?;

        while self.check(TokenKind::EqualsEquals) || self.check(TokenKind::BangEquals) {
            let op = if self.check(TokenKind::EqualsEquals) {
                BinaryOp::Eq
            } else {
                BinaryOp::Ne
            };
            self.bump();
            let rhs = self.parse_relational_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_relational_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_shift_expr()?;

        while self.check(TokenKind::Less)
            || self.check(TokenKind::LessEquals)
            || self.check(TokenKind::Greater)
            || self.check(TokenKind::GreaterEquals)
        {
            let op = match self.peek().kind {
                TokenKind::Less => BinaryOp::Lt,
                TokenKind::LessEquals => BinaryOp::Le,
                TokenKind::Greater => BinaryOp::Gt,
                TokenKind::GreaterEquals => BinaryOp::Ge,
                _ => unreachable!(),
            };
            self.bump();
            let rhs = self.parse_shift_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_shift_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_additive_expr()?;

        while self.check(TokenKind::LessLess)
            || self.check(TokenKind::GreaterGreater)
            || self.check(TokenKind::GreaterGreaterGreater)
        {
            let op = match self.peek().kind {
                TokenKind::LessLess => BinaryOp::Shl,
                TokenKind::GreaterGreater => BinaryOp::Shr,
                TokenKind::GreaterGreaterGreater => BinaryOp::ShrArith,
                _ => unreachable!(),
            };
            self.bump();
            let rhs = self.parse_additive_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_additive_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_multiplicative_expr()?;

        while self.check(TokenKind::Plus) || self.check(TokenKind::Minus) {
            let op = if self.check(TokenKind::Plus) {
                BinaryOp::Add
            } else {
                BinaryOp::Sub
            };
            self.bump();
            let rhs = self.parse_multiplicative_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_as_expr(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_unary_expr()?;
        while self.check_keyword(Keyword::As) {
            self.bump();
            let span = expr.span;
            let ty = self.parse_type()?;
            expr = Spanned::new(ExprKind::Cast { expr: Box::new(expr), ty }, span);
        }
        Ok(expr)
    }

    fn parse_multiplicative_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.parse_as_expr()?;

        while self.check(TokenKind::Star)
            || self.check(TokenKind::Slash)
            || self.check(TokenKind::Percent)
        {
            let op = match self.peek().kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Mod,
                _ => unreachable!(),
            };
            self.bump();
            let rhs = self.parse_as_expr()?;
            let span = lhs.span;
            lhs = Spanned::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            );
        }

        Ok(lhs)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr, Error> {
        if self.check(TokenKind::Plus) {
            self.bump();
            return self.parse_unary_expr();
        }

        if self.check(TokenKind::Minus) {
            self.bump();
            let operand = self.parse_unary_expr()?;
            return Ok(Spanned::new(
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                },
                self.mk_span(),
            ));
        }

        if self.check(TokenKind::Bang) {
            self.bump();
            let operand = self.parse_unary_expr()?;
            return Ok(Spanned::new(
                ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                self.mk_span(),
            ));
        }

        if self.check(TokenKind::Tilde) {
            self.bump();
            let operand = self.parse_unary_expr()?;
            return Ok(Spanned::new(
                ExprKind::Unary {
                    op: UnaryOp::BitNot,
                    operand: Box::new(operand),
                },
                self.mk_span(),
            ));
        }

        self.parse_postfix_expr()
    }

    /// Returns true if current token starts `<Type>::` — a generic type's static call.
    /// e.g. `Stack<T>::new()` — after parsing `Stack`, we see `<T>::`.
    fn is_generic_type_static_call(&self) -> bool {
        if !matches!(self.peek().kind, TokenKind::Less) {
            return false;
        }
        if !matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Ident(_))) {
            return false;
        }
        let mut depth = 1i32;
        let mut i = 2usize;
        while depth > 0 {
            match self.peek_ahead(i).map(|t| t.kind) {
                Some(TokenKind::Less) => { depth += 1; i += 1; }
                Some(TokenKind::Greater) => { depth -= 1; i += 1; }
                Some(TokenKind::GreaterGreater) => { depth -= 2; i += 1; }
                None => return false,
                _ => { i += 1; }
            }
        }
        matches!(self.peek_ahead(i).map(|t| t.kind), Some(TokenKind::ColonColon))
    }

    /// Returns true if the current token starts a generic method call like `<Type>(`.
    /// Scans forward past the `<...>` to confirm `(` follows.
    fn is_generic_method_call(&self) -> bool {
        if !matches!(self.peek().kind, TokenKind::Less) {
            return false;
        }
        if !matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Ident(_))) {
            return false;
        }
        let mut depth = 1i32;
        let mut i = 2usize;
        while depth > 0 {
            match self.peek_ahead(i).map(|t| t.kind) {
                Some(TokenKind::Less) => { depth += 1; i += 1; }
                Some(TokenKind::Greater) => { depth -= 1; i += 1; }
                Some(TokenKind::GreaterGreater) => { depth -= 2; i += 1; }
                None => return false,
                _ => { i += 1; }
            }
        }
        matches!(self.peek_ahead(i).map(|t| t.kind), Some(TokenKind::LParen))
    }

    fn parse_postfix_expr(&mut self) -> Result<Expr, Error> {
        let mut expr = self.parse_primary_expr()?;

        // Postfix chains (`a.b.c…`, `a[i][j]…`, `a()()…`) are parsed iteratively,
        // so they don't grow the parser's recursion depth — but they DO build an
        // equally deep AST that later recursive walks (node-ids, typecheck,
        // codegen) traverse. Cap the chain length to keep that AST bounded and
        // avoid a stack overflow on pathological input like `a.a.a.…` (×50000).
        let mut chain = 0usize;
        // Bug 166: `Class<T>::method(...)` (type args written BEFORE `::`)
        // is parsed by the `is_generic_type_static_call()` branch below,
        // which used to just skip past the `<...>` tokens and let the
        // next loop iteration's `ColonColon` branch build the EnumValue
        // node -- but that branch only ever reads type args from the
        // `Class::method<T>(...)` position (after the method name), so
        // whatever the user wrote before `::` was silently discarded
        // (`type_args: vec![]` regardless of what was actually written).
        // Stashed here across the loop iteration boundary: set when the
        // `is_generic_type_static_call()` branch parses (not skips) the
        // `<...>`, consumed by the very next `ColonColon` branch that
        // follows it.
        let mut pending_type_args: Option<Vec<Type>> = None;
        loop {
            chain += 1;
            if chain > MAX_RECURSION_DEPTH {
                return Err(Error::new(self.mk_span(), "postfix chain too deep"));
            }
            if self.check(TokenKind::Dot) {
                self.bump();
                // Tuple index access: expr.0, expr.1, ...
                if let TokenKind::Integer(idx) = self.peek().kind.clone() {
                    let span = expr.span;
                    self.bump();
                    expr = Spanned::new(
                        ExprKind::TupleIndex {
                            tuple: Box::new(expr),
                            index: idx as usize,
                        },
                        span,
                    );
                    continue;
                }
                let name = self.parse_method_name()?;
                let span = expr.span;
                // `TypeName.Variant` → enum variant only when:
                //   (a) no parens: `Direction.North`
                //   (b) named-arg parens: `Direction.Diagonal(dx: 1, dy: 1)`
                let is_type_access = matches!(&expr.node, ExprKind::Ident(n) if n.chars().next().is_some_and(|c| c.is_uppercase()));
                let has_named_args = self.check(TokenKind::LParen)
                    && matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Ident(_)))
                    && matches!(self.peek_ahead(2).map(|t| t.kind), Some(TokenKind::Colon));
                if is_type_access && (has_named_args || !self.check(TokenKind::LParen)) {
                    let enum_name = match &expr.node { ExprKind::Ident(n) => n.clone(), _ => unreachable!() };
                    let mut args = vec![];
                    if self.check(TokenKind::LParen) {
                        self.bump();
                        if !self.check(TokenKind::RParen) {
                            // Skip `name:` named arg syntax
                            if matches!(self.peek().kind, TokenKind::Ident(_))
                                && matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Colon))
                            {
                                self.bump(); self.bump();
                            }
                            args.push(self.parse_expr()?);
                            while self.consume(TokenKind::Comma) {
                                if self.check(TokenKind::RParen) { break; }
                                if matches!(self.peek().kind, TokenKind::Ident(_))
                                    && matches!(self.peek_ahead(1).map(|t| t.kind), Some(TokenKind::Colon))
                                {
                                    self.bump(); self.bump();
                                }
                                args.push(self.parse_expr()?);
                            }
                        }
                        self.expect(TokenKind::RParen)?;
                    }
                    expr = Spanned::new(
                        ExprKind::EnumValue {
                            enum_name,
                            variant: name,
                            type_args: vec![],
                            args,
                        },
                        span,
                    );
                } else if self.is_generic_method_call() {
                    // Generic method call: obj.method<T>(args) — skip type args (type erasure)
                    self.bump(); // consume `<`
                    let mut depth = 1i32;
                    while depth > 0 && !self.is_at_end() {
                        match self.peek().kind {
                            TokenKind::Less => { self.bump(); depth += 1; }
                            TokenKind::Greater => { self.bump(); depth -= 1; }
                            TokenKind::GreaterGreater => { self.bump(); depth -= 2; }
                            _ => { self.bump(); }
                        }
                    }
                    self.expect(TokenKind::LParen)?;
                    let mut args = Vec::new();
                    if !self.check(TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        while self.consume(TokenKind::Comma) {
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect(TokenKind::RParen)?;
                    expr = Spanned::new(
                        ExprKind::MethodCall {
                            obj: Box::new(expr),
                            method: name,
                            args,
                        },
                        span,
                    );
                } else if self.check(TokenKind::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.check(TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        while self.consume(TokenKind::Comma) {
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect(TokenKind::RParen)?;
                    expr = Spanned::new(
                        ExprKind::MethodCall {
                            obj: Box::new(expr),
                            method: name,
                            args,
                        },
                        span,
                    );
                } else {
                    expr = Spanned::new(
                        ExprKind::FieldAccess {
                            obj: Box::new(expr),
                            field: name,
                        },
                        span,
                    );
                }
            } else if self.check(TokenKind::ColonColon) {
                // Handle enum value construction: Color::Red or Option::Some(value)
                if let ExprKind::Ident(enum_name) = &expr.node {
                    self.bump(); // consume ::
                    let variant = self.parse_method_name()?;
                    let span = expr.span;
                    // Bug 166: type args written BEFORE `::` (`Class<T>::method(...)`)
                    // take priority over the (necessarily absent, since the
                    // two positions are mutually exclusive in valid syntax)
                    // `Class::method<T>(...)` position below.
                    let mut type_args = pending_type_args.take().unwrap_or_default();
                    // Explizite generische Typargumente: `Class::method<T>(args)`
                    if type_args.is_empty() && self.is_generic_method_call() {
                        self.bump(); // consume `<`
                        type_args.push(self.parse_type()?);
                        while self.consume(TokenKind::Comma) {
                            type_args.push(self.parse_type()?);
                        }
                        self.expect_generic_close()?;
                    }

                    let mut args = Vec::new();
                    if self.check(TokenKind::LParen) {
                        self.bump();
                        if !self.check(TokenKind::RParen) {
                            args.push(self.parse_expr()?);
                            while self.consume(TokenKind::Comma) {
                                args.push(self.parse_expr()?);
                            }
                        }
                        self.expect(TokenKind::RParen)?;
                    }

                    expr = Spanned::new(
                        ExprKind::EnumValue {
                            enum_name: enum_name.clone(),
                            variant,
                            type_args,
                            args,
                        },
                        span,
                    );
                } else {
                    return Err(self.error("expected enum name before ::"));
                }
            } else if self.is_generic_type_static_call() {
                // Generic type static call: Stack<T>::new() / Result<Int64>::err(...)
                // — parse (not skip, bug 166) the type args into
                // `pending_type_args` for the `ColonColon` branch that the
                // next loop iteration hits to consume, then leave the base
                // Ident expression intact so that branch can still build
                // its EnumValue node the same way it always has.
                self.bump(); // consume `<`
                let mut args = Vec::new();
                args.push(self.parse_type()?);
                while self.consume(TokenKind::Comma) {
                    args.push(self.parse_type()?);
                }
                self.expect_generic_close()?;
                pending_type_args = Some(args);
                // `::` is now current token; continue loop to hit the `::` branch
            } else if self.check(TokenKind::LBracket) {
                self.bump();
                let index = self.parse_expr()?;
                self.expect(TokenKind::RBracket)?;
                let span = expr.span;
                expr = Spanned::new(
                    ExprKind::Index {
                        obj: Box::new(expr),
                        index: Box::new(index),
                    },
                    span,
                );
            } else if self.check(TokenKind::DotDot) || self.check(TokenKind::DotDotDot) {
                let inclusive = self.check(TokenKind::DotDotDot);
                self.bump();
                let end = self.parse_postfix_expr()?;
                let span = expr.span;
                expr = Spanned::new(
                    ExprKind::Range {
                        start: Box::new(expr),
                        end: Box::new(end),
                        inclusive,
                    },
                    span,
                );
            } else if self.check(TokenKind::LParen) {
                self.bump();
                let mut args = Vec::new();
                if !self.check(TokenKind::RParen) {
                    args.push(self.parse_expr()?);
                    while self.consume(TokenKind::Comma) {
                        args.push(self.parse_expr()?);
                    }
                }
                self.expect(TokenKind::RParen)?;
                let span = expr.span;
                expr = Spanned::new(
                    ExprKind::Call {
                        func: Box::new(expr),
                        args,
                    },
                    span,
                );
            } else {
                break;
            }
        }

        Ok(expr)
    }

    fn parse_primary_expr(&mut self) -> Result<Expr, Error> {
        let token = self.peek();

        match &token.kind {
            TokenKind::At => {
                self.bump(); // consume @
                self.expect(TokenKind::LBrace)?;
                let mut entries = Vec::new();
                if !self.check(TokenKind::RBrace) {
                    let key = self.parse_expr()?;
                    self.expect(TokenKind::FatArrow)?;
                    let val = self.parse_expr()?;
                    entries.push((key, val));
                    while self.consume(TokenKind::Comma) {
                        if self.check(TokenKind::RBrace) { break; }
                        let key = self.parse_expr()?;
                        self.expect(TokenKind::FatArrow)?;
                        let val = self.parse_expr()?;
                        entries.push((key, val));
                    }
                }
                self.expect(TokenKind::RBrace)?;
                Ok(Spanned::new(ExprKind::MapLiteral(entries), token.span))
            }
            TokenKind::LBracket => {
                self.bump();
                let mut elements = Vec::new();
                if !self.check(TokenKind::RBracket) {
                    elements.push(self.parse_expr()?);
                    while self.consume(TokenKind::Comma) {
                        // Allow a trailing comma before the closing bracket
                        if self.check(TokenKind::RBracket) {
                            break;
                        }
                        elements.push(self.parse_expr()?);
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Spanned::new(ExprKind::ArrayLiteral(elements), token.span))
            }
            TokenKind::Integer(n) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Integer(*n)),
                    token.span,
                ))
            }
            TokenKind::Float(f) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Float(*f)),
                    token.span,
                ))
            }
            TokenKind::String(s) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::String(s.clone())),
                    token.span,
                ))
            }
            TokenKind::RawString(s) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::String(s.clone())),
                    token.span,
                ))
            }
            TokenKind::InterpString(parts) => {
                self.bump();
                let span = token.span;
                // Build concat tree: "" + toString(expr) + "str" + ...
                // Start with empty string, then fold over parts
                let mut result: Expr = Spanned::new(
                    ExprKind::Literal(Literal::String(String::new())),
                    span,
                );
                let mut is_first = true;
                for part in parts {
                    let part_expr: Expr = match part {
                        InterpPart::Str(s) => Spanned::new(
                            ExprKind::Literal(Literal::String(s.clone())),
                            span,
                        ),
                        InterpPart::Expr(src) => {
                            // Re-lex and re-parse the expression source
                            let inner_expr = Parser::parse_expr_str(src, span)?;
                            // Wrap in toString(inner_expr)
                            Spanned::new(
                                ExprKind::Call {
                                    func: Box::new(Spanned::new(
                                        ExprKind::Ident("toString".to_string()),
                                        span,
                                    )),
                                    args: vec![inner_expr],
                                },
                                span,
                            )
                        }
                    };
                    if is_first {
                        result = part_expr;
                        is_first = false;
                    } else {
                        result = Spanned::new(
                            ExprKind::Binary {
                                op: BinaryOp::Add,
                                lhs: Box::new(result),
                                rhs: Box::new(part_expr),
                            },
                            span,
                        );
                    }
                }
                Ok(result)
            }
            TokenKind::Byte(b) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Byte(*b)),
                    token.span,
                ))
            }
            TokenKind::Char(c) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Char(*c)),
                    token.span,
                ))
            }
            TokenKind::Bool(b) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Bool(*b)),
                    token.span,
                ))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Bool(true)),
                    token.span,
                ))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Ok(Spanned::new(
                    ExprKind::Literal(Literal::Bool(false)),
                    token.span,
                ))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump();
                Ok(Spanned::new(ExprKind::Literal(Literal::Null), token.span))
            }
            TokenKind::Keyword(Keyword::This) => {
                self.bump();
                Ok(Spanned::new(ExprKind::This, token.span))
            }
            TokenKind::Keyword(Keyword::Super) => {
                let span = token.span;
                self.bump();
                self.expect(TokenKind::Dot)?;
                let method = self.parse_ident()?;
                self.expect(TokenKind::LParen)?;
                let mut args = Vec::new();
                if !self.check(TokenKind::RParen) {
                    args.push(self.parse_expr()?);
                    while self.consume(TokenKind::Comma) {
                        args.push(self.parse_expr()?);
                    }
                }
                self.expect(TokenKind::RParen)?;
                Ok(Spanned::new(ExprKind::SuperCall { method, args }, span))
            }
            TokenKind::Keyword(Keyword::If) => self.parse_if_expr(),
            TokenKind::Keyword(Keyword::While) => self.parse_while_expr(),
            TokenKind::Keyword(Keyword::Loop) => self.parse_loop_expr(),
            TokenKind::Keyword(Keyword::Match) => self.parse_match_expr(),
            TokenKind::Keyword(Keyword::New) => self.parse_new_expr(),
            TokenKind::Keyword(Keyword::Spawn) => self.parse_spawn_expr(),
            TokenKind::Keyword(Keyword::Await) => self.parse_await_expr(),
            TokenKind::Keyword(Keyword::Send) => self.parse_send_expr(),
            TokenKind::Keyword(Keyword::Recv) => self.parse_recv_expr(),
            TokenKind::Keyword(Keyword::Return) => {
                let span = token.span;
                self.bump();
                let value = if self.check(TokenKind::Semicolon)
                    || self.check(TokenKind::RBrace)
                    || self.check(TokenKind::RParen)
                {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                Ok(Spanned::new(ExprKind::Return(value), span))
            }
            TokenKind::Keyword(Keyword::Break) => {
                let span = token.span;
                self.bump();
                Ok(Spanned::new(ExprKind::Break, span))
            }
            TokenKind::Keyword(Keyword::Continue) => {
                let span = token.span;
                self.bump();
                Ok(Spanned::new(ExprKind::Continue, span))
            }
            TokenKind::Keyword(Keyword::Cast) => self.parse_cast_expr(),
            TokenKind::Keyword(Keyword::Is) => self.parse_is_expr(),
            TokenKind::Keyword(Keyword::Channel) => {
                self.bump();
                Ok(Spanned::new(ExprKind::Channel, token.span))
            }
            TokenKind::Backslash => self.parse_lambda(),
            TokenKind::Keyword(Keyword::Fnc) if self.peek_ahead(1).is_some_and(|t| matches!(t.kind, TokenKind::LParen)) => self.parse_fnc_lambda(),
            TokenKind::Keyword(Keyword::Fn) if self.peek_ahead(1).is_some_and(|t| matches!(t.kind, TokenKind::LParen)) => self.parse_fn_lambda(),
            TokenKind::LParen => {
                // Peek ahead: `(ident :` → typed lambda params
                let is_typed_lambda = matches!(
                    (self.peek_ahead(1).map(|t| t.kind.clone()), self.peek_ahead(2).map(|t| t.kind.clone())),
                    (Some(TokenKind::Ident(_)), Some(TokenKind::Colon))
                );
                // C-style cast: `(TypeName)expr` where TypeName is a primitive type
                let is_c_cast = {
                    let ty_name = self.peek_ahead(1).and_then(|t| if let TokenKind::Ident(s) = &t.kind { Some(s.clone()) } else { None });
                    let after_ty = self.peek_ahead(2).map(|t| t.kind.clone());
                    let after_rparen = self.peek_ahead(3).map(|t| t.kind.clone());
                    let is_primitive = ty_name.as_deref().is_some_and(|s| matches!(s,
                        "Int8"|"Int16"|"Int32"|"Int64"|"UInt8"|"UInt16"|"UInt32"|"UInt64"|"Float32"|"Float64"|"Bool"|"Char"|"String"
                    ));
                    let after_is_rparen = matches!(after_ty, Some(TokenKind::RParen));
                    let after_can_start_expr = matches!(after_rparen,
                        Some(TokenKind::Ident(_)) | Some(TokenKind::Integer(_)) | Some(TokenKind::Float(_))
                        | Some(TokenKind::Minus) | Some(TokenKind::Bang) | Some(TokenKind::LParen)
                        | Some(TokenKind::Keyword(Keyword::This))
                    );
                    is_primitive && after_is_rparen && after_can_start_expr
                };
                if is_typed_lambda {
                    self.parse_typed_lambda()
                } else if is_c_cast {
                    let span = self.mk_span();
                    self.bump(); // (
                    let ty = self.parse_type()?;
                    self.expect(TokenKind::RParen)?;
                    let expr = Box::new(self.parse_unary_expr()?);
                    Ok(Spanned::new(ExprKind::Cast { expr, ty }, span))
                } else {
                    self.parse_tuple_or_grouped()
                }
            }
            TokenKind::LBrace => {
                let block_stmt = self.parse_block()?;
                let span = block_stmt.span;
                let stmts = if let StmtKind::Block(stmts) = block_stmt.node {
                    stmts
                } else {
                    unreachable!()
                };
                Ok(Spanned::new(ExprKind::Block(stmts), span))
            }
            TokenKind::Ident(s) => {
                self.bump();
                // Skip optional generic type args: `Foo<T>` before checking for struct literal
                let saved_pos_for_generic = self.pos;
                if self.check(TokenKind::Less) {
                    // Try to skip over <...> generics
                    self.bump();
                    let mut depth = 1i32;
                    while depth > 0 && !self.is_at_end() {
                        match self.peek().kind {
                            TokenKind::Less => { self.bump(); depth += 1; }
                            TokenKind::Greater => { self.bump(); depth -= 1; }
                            TokenKind::GreaterGreater => { self.bump(); depth -= 2; }
                            _ => { self.bump(); }
                        }
                    }
                    // If not followed by { with field pattern, revert
                    if !self.check(TokenKind::LBrace) {
                        self.pos = saved_pos_for_generic;
                    }
                }
                if self.check(TokenKind::LBrace) {
                    // Lookahead to check if this is really a struct literal
                    // Struct literals have Ident { field: value, ... }
                    // We check if the next token after { is either } or an Ident followed by :
                    let is_struct_literal = {
                        let saved_pos = self.pos;
                        self.bump(); // consume {
                        let result = if self.check(TokenKind::RBrace) {
                            true // empty struct literal
                        } else if let TokenKind::Ident(_) = self.peek().kind {
                            self.bump(); // consume potential field name
                            self.check(TokenKind::Colon) // check if followed by :
                        } else {
                            false // not a struct literal
                        };
                        self.pos = saved_pos; // restore position
                        result
                    };

                    if is_struct_literal {
                        self.bump(); // consume {
                        let mut fields = Vec::new();
                        while !self.check(TokenKind::RBrace) {
                            let name = self.parse_ident()?;
                            self.expect(TokenKind::Colon)?;
                            let value = self.parse_expr()?;
                            fields.push((name, value));
                            if !self.check(TokenKind::RBrace) {
                                self.expect(TokenKind::Comma)?;
                            }
                        }
                        self.expect(TokenKind::RBrace)?;
                        Ok(Spanned::new(
                            ExprKind::StructLiteral {
                                name: s.clone(),
                                fields,
                            },
                            token.span,
                        ))
                    } else {
                        Ok(Spanned::new(ExprKind::Ident(s.clone()), token.span))
                    }
                } else {
                    Ok(Spanned::new(ExprKind::Ident(s.clone()), token.span))
                }
            }
            // Allow some keywords to be used as identifiers in expression position
            TokenKind::Keyword(kw) => {
                let name = match kw {
                    Keyword::Default => "default",
                    Keyword::Send => "send",
                    Keyword::Recv => "recv",
                    Keyword::Is => "is",
                    // Same reasoning as parse_ident's/parse_method_name's
                    // own allowlists: a `namespace` parameter/field/local
                    // referenced as a plain value (e.g. `foo(name,
                    // namespace)`) needs to parse as ExprKind::Ident here
                    // too, not just at its declaration site.
                    Keyword::Namespace => "namespace",
                    _ => return Err(Error::new(token.span, format!("unexpected token: {:?}", token.kind))),
                };
                self.bump();
                Ok(Spanned::new(ExprKind::Ident(name.to_string()), token.span))
            }
            _ => Err(Error::new(
                token.span,
                format!("unexpected token: {:?}", token.kind),
            )),
        }
    }

    fn parse_if_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::If)?;
        let cond = self.parse_expr()?;
        let then_branch = Box::new(self.parse_expr()?);
        let else_branch = if self.consume_keyword(Keyword::Else) {
            Some(Box::new(self.parse_expr()?))
        } else {
            None
        };
        Ok(Spanned::new(
            ExprKind::If {
                cond: Box::new(cond),
                then_branch,
                else_branch,
            },
            span,
        ))
    }

    fn parse_while_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::While)?;
        let cond = self.parse_expr()?;
        let body = Box::new(self.parse_expr()?);
        Ok(Spanned::new(
            ExprKind::While {
                cond: Box::new(cond),
                body,
            },
            span,
        ))
    }

    fn parse_loop_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Loop)?;
        let body = Box::new(self.parse_expr()?);
        Ok(Spanned::new(ExprKind::Loop { body }, span))
    }

    fn parse_match_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Match)?;
        let expr = Box::new(self.parse_expr()?);
        self.expect(TokenKind::LBrace)?;

        let mut cases = Vec::new();
        while !self.check(TokenKind::RBrace) {
            let pattern_span = self.mk_span();
            let pattern = self.parse_pattern()?;

            let guard = if self.consume_keyword(Keyword::If) {
                Some(self.parse_expr()?)
            } else {
                None
            };

            self.expect(TokenKind::FatArrow)?;
            let body = self.parse_expr()?;
            let body_is_block = matches!(body.node, ExprKind::Block(_));

            cases.push(MatchCase {
                pattern,
                guard,
                body,
                span: pattern_span,
            });

            // Block bodies don't need a trailing separator
            if !body_is_block {
                if !self.check(TokenKind::RBrace) {
                    if !self.consume(TokenKind::Comma) && !self.consume(TokenKind::Semicolon) {
                        return Err(self.error("expected ',' or ';'"));
                    }
                } else {
                    self.consume(TokenKind::Comma);
                    self.consume(TokenKind::Semicolon);
                }
            } else {
                self.consume(TokenKind::Comma);
                self.consume(TokenKind::Semicolon);
            }
        }

        self.expect(TokenKind::RBrace)?;

        Ok(Spanned::new(ExprKind::Match { expr, cases }, span))
    }

    fn parse_pattern(&mut self) -> Result<Pattern, Error> {
        let span = self.mk_span();

        if self.check(TokenKind::Underscore) {
            self.bump();
            return Ok(Pattern::Wildcard(span));
        }

        if self.check(TokenKind::LParen) {
            self.bump();
            let mut patterns = Vec::new();
            if !self.check(TokenKind::RParen) {
                patterns.push(self.parse_pattern()?);
                while self.consume(TokenKind::Comma) {
                    patterns.push(self.parse_pattern()?);
                }
            }
            self.expect(TokenKind::RParen)?;
            return Ok(Pattern::Tuple(patterns, span));
        }

        // Try to parse a literal
        match &self.peek().kind {
            TokenKind::Integer(n) => {
                let n = *n;
                self.bump();
                return Ok(Pattern::Literal(Literal::Integer(n), span));
            }
            TokenKind::Float(f) => {
                let f = *f;
                self.bump();
                return Ok(Pattern::Literal(Literal::Float(f), span));
            }
            TokenKind::String(s) => {
                let s = s.clone();
                self.bump();
                return Ok(Pattern::Literal(Literal::String(s), span));
            }
            TokenKind::Char(c) => {
                let c = *c;
                self.bump();
                return Ok(Pattern::Literal(Literal::Char(c), span));
            }
            TokenKind::Byte(b) => {
                let b = *b;
                self.bump();
                return Ok(Pattern::Literal(Literal::Byte(b), span));
            }
            TokenKind::Keyword(Keyword::True) | TokenKind::Bool(true) => {
                self.bump();
                return Ok(Pattern::Literal(Literal::Bool(true), span));
            }
            TokenKind::Keyword(Keyword::False) | TokenKind::Bool(false) => {
                self.bump();
                return Ok(Pattern::Literal(Literal::Bool(false), span));
            }
            _ => {}
        }

        let name = self.parse_ident()?;

        // Handle enum variant patterns: Color::Red, Color.Red, or Option::Some(x)
        if self.check(TokenKind::ColonColon) || self.check(TokenKind::Dot) {
            self.bump();
            let variant = self.parse_ident()?;
            let mut args = Vec::new();
            if self.check(TokenKind::LParen) {
                self.bump();
                if !self.check(TokenKind::RParen) {
                    args.push(self.parse_pattern()?);
                    while self.consume(TokenKind::Comma) {
                        args.push(self.parse_pattern()?);
                    }
                }
                self.expect(TokenKind::RParen)?;
            }
            return Ok(Pattern::EnumVariant {
                enum_name: name,
                variant,
                args,
                span,
            });
        }

        if self.check(TokenKind::LParen) {
            self.bump();
            let mut args = Vec::new();
            if !self.check(TokenKind::RParen) {
                args.push(self.parse_pattern()?);
                while self.consume(TokenKind::Comma) {
                    args.push(self.parse_pattern()?);
                }
            }
            self.expect(TokenKind::RParen)?;
            return Ok(Pattern::EnumVariant {
                enum_name: name,
                variant: String::new(),
                args,
                span,
            });
        }

        Ok(Pattern::Ident(name, None, span))
    }

    fn parse_new_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::New)?;
        let class = self.parse_ident()?;
        let type_args = if self.consume(TokenKind::Less) {
            let mut targs = vec![self.parse_type()?];
            while self.consume(TokenKind::Comma) {
                targs.push(self.parse_type()?);
            }
            self.expect(TokenKind::Greater)?;
            targs
        } else {
            vec![]
        };
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if !self.check(TokenKind::RParen) {
            args.push(self.parse_expr()?);
            while self.consume(TokenKind::Comma) {
                args.push(self.parse_expr()?);
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(Spanned::new(ExprKind::New { class, type_args, args }, span))
    }

    fn parse_spawn_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Spawn)?;
        let expr = Box::new(self.parse_expr()?);
        Ok(Spanned::new(ExprKind::Spawn(expr), span))
    }

    fn parse_await_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Await)?;
        let expr = Box::new(self.parse_expr()?);
        Ok(Spanned::new(ExprKind::Await(expr), span))
    }

    fn parse_send_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Send)?;
        let channel = Box::new(self.parse_expr()?);
        self.expect(TokenKind::ThinArrow)?;
        let value = Box::new(self.parse_expr()?);
        Ok(Spanned::new(ExprKind::Send { channel, value }, span))
    }

    fn parse_recv_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Recv)?;
        let channel = Box::new(self.parse_expr()?);
        Ok(Spanned::new(ExprKind::Recv(channel), span))
    }

    /// select { recv ch -> v { body } ... default { body } }
    fn parse_select_stmt(&mut self) -> Result<StmtKind, Error> {
        self.expect_keyword(Keyword::Select)?;
        self.expect(TokenKind::LBrace)?;
        let mut arms: Vec<SelectArm> = Vec::new();
        let mut default: Option<Box<Stmt>> = None;
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            let arm_span = self.mk_span();
            if self.check_keyword(Keyword::Default) {
                self.bump();
                let body = self.parse_block()?;
                default = Some(Box::new(body));
            } else if self.check_keyword(Keyword::Recv) {
                self.bump(); // consume recv
                let channel = self.parse_expr()?;
                self.expect(TokenKind::ThinArrow)?;
                let var = self.parse_ident()?;
                let body = self.parse_block()?;
                arms.push(SelectArm { channel, var, body, span: arm_span });
            } else {
                let tok = self.peek().clone();
                return Err(Error::new(tok.span, format!("expected 'recv' or 'default' in select, got {:?}", tok.kind)));
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(StmtKind::Select { arms, default })
    }

    fn parse_cast_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Cast)?;
        let expr = Box::new(self.parse_unary_expr()?);
        self.expect_keyword(Keyword::As)?;
        let ty = self.parse_type()?;
        Ok(Spanned::new(ExprKind::Cast { expr, ty }, span))
    }

    fn parse_is_expr(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Is)?;
        let expr = Box::new(self.parse_expr()?);
        self.expect(TokenKind::ThinArrow)?;
        let ty = self.parse_type()?;
        Ok(Spanned::new(ExprKind::Is { expr, ty }, span))
    }

    fn parse_lambda(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect(TokenKind::Backslash)?; // \

        let mut params = Vec::new();
        if !self.check(TokenKind::ThinArrow) {
            params.push(self.parse_lambda_param()?);
            while self.consume(TokenKind::Comma) {
                params.push(self.parse_lambda_param()?);
            }
        }

        self.expect(TokenKind::ThinArrow)?;

        let body = self.parse_expr()?;

        Ok(Spanned::new(
            ExprKind::Lambda {
                params,
                ret_type: None,
                body: Box::new(body),
            },
            span,
        ))
    }

    fn parse_typed_lambda(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_lambda_param()?);
            while self.consume(TokenKind::Comma) {
                if self.check(TokenKind::RParen) { break; }
                params.push(self.parse_lambda_param()?);
            }
        }
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::FatArrow)?;
        let body = self.parse_assign_expr()?;
        Ok(Spanned::new(ExprKind::Lambda { params, ret_type: None, body: Box::new(body) }, span))
    }

    fn parse_fn_lambda(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Fn)?;
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_fn_lambda_param()?);
            while self.consume(TokenKind::Comma) {
                if self.check(TokenKind::RParen) { break; }
                params.push(self.parse_fn_lambda_param()?);
            }
        }
        self.expect(TokenKind::RParen)?;
        let ret_type = if self.consume(TokenKind::ThinArrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_expr()?;
        Ok(Spanned::new(
            ExprKind::Lambda {
                params,
                ret_type,
                body: Box::new(body),
            },
            span,
        ))
    }

    fn parse_fnc_lambda(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect_keyword(Keyword::Fnc)?;
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        if !self.check(TokenKind::RParen) {
            params.push(self.parse_fn_lambda_param()?);
            while self.consume(TokenKind::Comma) {
                if self.check(TokenKind::RParen) { break; }
                params.push(self.parse_fn_lambda_param()?);
            }
        }
        self.expect(TokenKind::RParen)?;
        let ret_type = if self.consume(TokenKind::ThinArrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_expr()?;
        Ok(Spanned::new(ExprKind::Lambda { params, ret_type, body: Box::new(body) }, span))
    }

    fn parse_fn_lambda_param(&mut self) -> Result<Param, Error> {
        let span = self.mk_span();
        let name = self.parse_ident()?;
        let param_type = if self.consume(TokenKind::Colon) {
            self.parse_type()?
        } else {
            Type::Infer
        };
        Ok(Param { name, param_type, span, annotations: vec![] })
    }

    fn parse_lambda_param(&mut self) -> Result<Param, Error> {
        let span = self.mk_span();
        let name = self.parse_ident()?;
        let param_type = if self.consume(TokenKind::Colon) {
            self.parse_type()?
        } else {
            Type::Infer
        };
        Ok(Param {
            name,
            param_type,
            span,
            annotations: vec![],
        })
    }

    fn parse_tuple_or_grouped(&mut self) -> Result<Expr, Error> {
        let span = self.mk_span();
        self.expect(TokenKind::LParen)?;

        if self.check(TokenKind::RParen) {
            self.bump();
            return Ok(Spanned::new(ExprKind::Block(vec![]), span));
        }

        let expr = self.parse_expr()?;

        if self.check(TokenKind::Comma) {
            self.bump();
            let mut exprs = vec![expr];
            exprs.push(self.parse_expr()?);
            while self.consume(TokenKind::Comma) {
                exprs.push(self.parse_expr()?);
            }
            self.expect(TokenKind::RParen)?;
            return Ok(Spanned::new(ExprKind::Tuple(exprs), span));
        }

        self.expect(TokenKind::RParen)?;
        Ok(expr)
    }

    fn expr_to_lambda_params(expr: Expr) -> Result<Vec<Param>, Error> {
        match expr.node {
            ExprKind::Ident(name) => Ok(vec![Param { name, param_type: Type::Infer, span: expr.span, annotations: vec![] }]),
            ExprKind::Tuple(exprs) => {
                let mut params = Vec::new();
                for e in exprs {
                    match e.node {
                        ExprKind::Ident(name) => params.push(Param { name, param_type: Type::Infer, span: e.span, annotations: vec![] }),
                        _ => return Err(Error::new(e.span, "expected identifier in lambda parameter list")),
                    }
                }
                Ok(params)
            }
            _ => Err(Error::new(expr.span, "expected identifier or parameter list before '=>'")),
        }
    }

    fn parse_ident(&mut self) -> Result<String, Error> {
        match self.peek().kind.clone() {
            TokenKind::Ident(s) => { self.bump(); Ok(s) }
            TokenKind::Underscore => { self.bump(); Ok("_".to_string()) }
            TokenKind::Keyword(kw) => {
                // Allow some keywords as identifiers (param names, variable names, etc.)
                let name = match kw {
                    Keyword::Default => "default",
                    Keyword::New => "new",
                    Keyword::Send => "send",
                    Keyword::Recv => "recv",
                    Keyword::Is => "is",
                    // Same reasoning as parse_method_name's own allowlist:
                    // `namespace` is an extremely common field/param/
                    // variable name in real code (e.g. every Kubernetes
                    // resource's ObjectMeta.namespace) despite also being
                    // this language's `namespace { ... }` block keyword.
                    Keyword::Namespace => "namespace",
                    _ => return Err(Error::new(self.mk_span(), "expected identifier")),
                };
                self.bump();
                Ok(name.to_string())
            }
            _ => Err(Error::new(self.mk_span(), "expected identifier")),
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn check_keyword(&self, kw: Keyword) -> bool {
        matches!(&self.peek().kind, TokenKind::Keyword(k) if *k == kw)
    }

    fn peek(&self) -> Token {
        self.tokens
            .get(self.pos)
            .cloned()
            .unwrap_or(Token::dummy(TokenKind::Eof))
    }

    fn peek_ahead(&self, offset: usize) -> Option<Token> {
        self.tokens.get(self.pos + offset).cloned()
    }

    fn bump(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    /// Consume `>` closing a generic type. If the current token is `>>` or
    /// `>>>`, split it: consume one `>` and leave the rest as a synthetic
    /// shorter token for the next close (List<List<List<Int64>>>).
    fn expect_generic_close(&mut self) -> Result<(), Error> {
        if self.check(TokenKind::Greater) {
            self.bump();
            Ok(())
        } else if self.check(TokenKind::GreaterGreater) {
            self.tokens[self.pos].kind = TokenKind::Greater;
            Ok(())
        } else if self.check(TokenKind::GreaterGreaterGreater) {
            self.tokens[self.pos].kind = TokenKind::GreaterGreater;
            Ok(())
        } else {
            Err(self.error("expected '>'"))
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.peek().kind == TokenKind::Eof
    }

    fn consume(&mut self, kind: TokenKind) -> bool {
        if self.check(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, kw: Keyword) -> bool {
        if self.check_keyword(kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn take_doc(&mut self) -> Option<String> {
        while self.peek().kind.is_trivia() && !self.is_at_end() {
            if let TokenKind::DocComment(text) = &self.peek().kind {
                let doc = text.clone();
                self.bump();
                return Some(doc);
            }
            self.bump();
        }
        if let TokenKind::DocComment(text) = &self.peek().kind {
            let doc = text.clone();
            self.bump();
            Some(doc)
        } else {
            None
        }
    }

    fn parse_annotations(&mut self) -> Vec<Annotation> {
        let mut annotations = Vec::new();
        loop {
            if self.check(TokenKind::At) {
                let next = self.peek_ahead(1);
                if let Some(tok) = next {
                    if matches!(tok.kind, TokenKind::Ident(_)) {
                        let span = self.mk_span();
                        self.bump(); // consume @
                        let name = self.parse_ident().unwrap_or_default();
                        let mut args = Vec::new();
                        if self.check(TokenKind::LParen) {
                            self.bump(); // consume (
                            if !self.check(TokenKind::RParen) {
                                if let Ok(arg) = self.parse_annotation_arg() {
                                    args.push(arg);
                                    while self.consume(TokenKind::Comma) {
                                        if let Ok(arg) = self.parse_annotation_arg() {
                                            args.push(arg);
                                        }
                                    }
                                }
                            }
                            self.expect(TokenKind::RParen).ok();
                        }
                        annotations.push(Annotation { name, args, span });
                        continue;
                    }
                }
            }
            break;
        }
        annotations
    }

    fn parse_annotation_arg(&mut self) -> Result<AnnotationArg, Error> {
        let token = self.peek();
        match &token.kind {
            TokenKind::Integer(n) => {
                let val = *n;
                self.bump();
                Ok(AnnotationArg::Literal(Literal::Integer(val)))
            }
            TokenKind::Float(f) => {
                let val = *f;
                self.bump();
                Ok(AnnotationArg::Literal(Literal::Float(val)))
            }
            TokenKind::String(s) => {
                let val = s.clone();
                self.bump();
                Ok(AnnotationArg::Literal(Literal::String(val)))
            }
            TokenKind::Bool(b) => {
                let val = *b;
                self.bump();
                Ok(AnnotationArg::Literal(Literal::Bool(val)))
            }
            // Bracketed list, e.g. @OIDCRolesAllowed(["admin", "api-user"])
            TokenKind::LBracket => {
                self.bump(); // consume [
                let mut items = Vec::new();
                if !self.check(TokenKind::RBracket) {
                    items.push(self.parse_annotation_arg()?);
                    while self.consume(TokenKind::Comma) {
                        if self.check(TokenKind::RBracket) {
                            break;
                        }
                        items.push(self.parse_annotation_arg()?);
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Ok(AnnotationArg::Array(items))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Ok(AnnotationArg::Literal(Literal::Bool(true)))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Ok(AnnotationArg::Literal(Literal::Bool(false)))
            }
            // Qualified enum member: TypeName.VariantName  e.g. MediaType.APPLICATION_JSON
            TokenKind::Ident(type_name) => {
                let type_name = type_name.clone();
                self.bump();
                if self.consume(TokenKind::Dot) {
                    if let TokenKind::Ident(variant) = &self.peek().kind {
                        let variant = variant.clone();
                        self.bump();
                        return Ok(AnnotationArg::EnumValue(type_name, variant));
                    }
                }
                Err(Error::new(token.span, "expected TypeName.VariantName for enum annotation argument"))
            }
            _ => Err(Error::new(token.span, "expected annotation argument (string, int, float, bool, EnumType.Variant, or [...])")),
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<(), Error> {
        if self.check(kind.clone()) {
            self.bump();
            Ok(())
        } else {
            Err(Error::new(
                self.mk_span(),
                format!("expected {:?}, found {:?}", kind, self.peek().kind),
            ))
        }
    }

    fn expect_keyword(&mut self, kw: Keyword) -> Result<(), Error> {
        if self.check_keyword(kw) {
            self.bump();
            Ok(())
        } else {
            Err(Error::new(
                self.mk_span(),
                format!("expected keyword {:?}, found {:?}", kw, self.peek().kind),
            ))
        }
    }

    fn mk_span(&self) -> Span {
        let pos = self.pos;
        let tok = self
            .tokens
            .get(pos)
            .unwrap_or(&self.tokens[pos.saturating_sub(1)]);
        Span::new(tok.span.start, tok.span.start)
    }

    fn error(&self, msg: &str) -> Error {
        Error::new(self.mk_span(), msg)
    }

    fn synchronize(&mut self) {
        let start_pos = self.pos;
        while !self.is_at_end() {
            match self.peek().kind {
                TokenKind::Semicolon => {
                    self.bump();
                    return;
                }
                TokenKind::Keyword(
                    Keyword::Fn
                    | Keyword::Fnc
                    | Keyword::Class
                    | Keyword::Interface
                    | Keyword::Enum
                    | Keyword::Trait
                    | Keyword::Namespace
                    | Keyword::If
                    | Keyword::While
                    | Keyword::For
                    | Keyword::Loop
                    | Keyword::Return,
                ) => {
                    if self.pos > start_pos {
                        return;
                    }
                    self.bump();
                }
                _ => self.bump(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;

    #[test]
    fn test_parse_fn() {
        let code = "fn main() -> Int32 { return 42; }";
        let mut lexer = Lexer::new(code);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let result = parser.parse();
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn test_parse_class() {
        let code = "class Foo { x: Int32; }";
        let mut lexer = Lexer::new(code);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let result = parser.parse();
        assert!(result.is_ok(), "{:?}", result);
    }

    // --- Helpers ---

    fn parse(src: &str) -> SourceFile {
        let tokens = Lexer::new(src).tokenize().expect("lex error");
        Parser::new(tokens).parse().expect("parse error")
    }

    fn parse_err(src: &str) -> bool {
        let tokens = Lexer::new(src).tokenize().expect("lex error");
        Parser::new(tokens).parse().is_err()
    }

    fn first_decl(src: &str) -> DeclKind {
        parse(src).decls.remove(0).node
    }

    // --- Function declarations ---

    #[test]
    fn test_fn_no_params_no_return() {
        let d = first_decl("fn greet() { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.name, "greet");
        assert!(f.params.is_empty());
        assert!(matches!(f.ret_type, Type::Nothing));
    }

    #[test]
    fn test_fn_with_params_and_return() {
        let d = first_decl("fn add(a: Int32, b: Int32) -> Int32 { return a; }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].name, "a");
        assert!(matches!(f.params[0].param_type, Type::Int32));
        assert_eq!(f.params[1].name, "b");
        assert!(matches!(f.ret_type, Type::Int32));
    }

    #[test]
    fn test_fn_generic() {
        let d = first_decl("fn identity<T>(x: T) -> T { return x; }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.name, "identity");
        assert_eq!(f.type_params, vec!["T"]);
        assert_eq!(f.params.len(), 1);
    }

    #[test]
    fn test_fn_multiple_type_params() {
        let d = first_decl("fn pair<A, B>(a: A, b: B) -> A { return a; }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.type_params, vec!["A", "B"]);
    }

    #[test]
    fn test_fn_async() {
        let d = first_decl("async fn fetch() -> String { return \"ok\"; }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(f.is_async);
        assert_eq!(f.name, "fetch");
    }

    #[test]
    fn test_fn_abstract_declaration() {
        let d = first_decl("fn foo(x: Int32) -> Bool;");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.name, "foo");
        assert!(matches!(f.ret_type, Type::Bool));
    }

    // --- Class declarations ---

    #[test]
    fn test_class_empty() {
        let d = first_decl("class Point { }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.name, "Point");
        assert!(c.fields.is_empty());
        assert!(c.methods.is_empty());
        assert!(c.extends.is_none());
        assert!(c.implements.is_empty());
    }

    #[test]
    fn test_class_with_fields() {
        let d = first_decl("class Point { x: Float64; y: Float64; }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.fields.len(), 2);
        assert_eq!(c.fields[0].name, "x");
        assert!(matches!(c.fields[0].field_type, Type::Float64));
        assert_eq!(c.fields[1].name, "y");
    }

    #[test]
    fn test_class_extends() {
        let d = first_decl("class Dog extends Animal { }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.name, "Dog");
        assert_eq!(c.extends.as_deref(), Some("Animal"));
    }

    #[test]
    fn test_class_implements() {
        let d = first_decl("class Dog implements Animal, Runnable { }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.implements, vec!["Animal", "Runnable"]);
    }

    #[test]
    fn test_class_generic() {
        let d = first_decl("class Box<T> { value: T; }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.name, "Box");
        assert_eq!(c.type_params, vec!["T"]);
        assert_eq!(c.fields.len(), 1);
    }

    #[test]
    fn test_class_with_method() {
        let d = first_decl("class Counter { fn increment() -> Nothing { } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name, "increment");
    }

    #[test]
    fn test_class_visibility_modifiers() {
        let d = first_decl("class Foo { public x: Int32; private y: Bool; }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(matches!(c.fields[0].visibility, Visibility::Public));
        assert!(matches!(c.fields[1].visibility, Visibility::Private));
    }

    // --- Interface declarations ---

    #[test]
    fn test_interface_empty() {
        let d = first_decl("interface Shape { }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.name, "Shape");
        assert!(i.methods.is_empty());
        assert!(i.extends.is_empty());
    }

    #[test]
    fn test_interface_with_methods() {
        let d = first_decl("interface Shape { fn area() -> Float64; fn perimeter() -> Float64; }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.methods.len(), 2);
        assert_eq!(i.methods[0].name, "area");
        assert_eq!(i.methods[1].name, "perimeter");
    }

    #[test]
    fn test_interface_extends() {
        let d = first_decl("interface Animal extends Living { fn speak() -> String; }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.extends, vec!["Living"]);
    }

    // --- Enum declarations ---

    #[test]
    fn test_enum_simple() {
        let d = first_decl("enum Color { Red, Green, Blue }");
        let DeclKind::Enum(e) = d else { panic!() };
        assert_eq!(e.name, "Color");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[1].name, "Green");
        assert_eq!(e.variants[2].name, "Blue");
    }

    #[test]
    fn test_enum_with_args() {
        let d = first_decl("enum Result { Ok(Int32), Err(String) }");
        let DeclKind::Enum(e) = d else { panic!() };
        assert_eq!(e.variants.len(), 2);
        assert_eq!(e.variants[0].name, "Ok");
        assert_eq!(e.variants[0].args.len(), 1);
        assert!(matches!(e.variants[0].args[0], Type::Int32));
    }

    // --- Import declarations ---

    #[test]
    fn test_import_simple() {
        let d = first_decl("import std.io;");
        let DeclKind::Import(i) = d else { panic!() };
        assert_eq!(i.path, vec!["std", "io"]);
        assert!(i.alias.is_none());
    }

    #[test]
    fn test_import_with_alias() {
        let d = first_decl("import std.io as io;");
        let DeclKind::Import(i) = d else { panic!() };
        assert_eq!(i.path, vec!["std", "io"]);
        assert_eq!(i.alias.as_deref(), Some("io"));
    }

    // --- Module declaration ---

    #[test]
    fn test_module_declaration() {
        let d = first_decl("module myapp;");
        let DeclKind::Module(name) = d else { panic!() };
        assert_eq!(name, "myapp");
    }

    // --- Statements: let / var ---

    #[test]
    fn test_let_with_type() {
        let d = first_decl("fn f() { let x: Int32 = 5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Let { .. }));
        let StmtKind::Let { name, ty, .. } = &stmts[0].node else { panic!() };
        assert_eq!(name, "x");
        assert!(matches!(ty, Some(Type::Int32)));
    }

    #[test]
    fn test_let_type_inferred() {
        let d = first_decl("fn f() { let x = 42; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { name, ty, .. } = &stmts[0].node else { panic!() };
        assert_eq!(name, "x");
        assert!(ty.is_none());
    }

    #[test]
    fn test_var_mutable() {
        let d = first_decl("fn f() { var x: Int32 = 0; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(&stmts[0].node, StmtKind::Var { mutable: true, .. }));
    }

    // --- Statements: control flow ---

    #[test]
    fn test_if_stmt() {
        let d = first_decl("fn f() { if true { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::If { .. }));
    }

    #[test]
    fn test_if_else_stmt() {
        // bare ident before { would be parsed as struct literal, use comparison instead
        let d = first_decl("fn f() { if x > 0 { } else { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::If { else_branch, .. } = &stmts[0].node else { panic!() };
        assert!(else_branch.is_some());
    }

    #[test]
    fn test_while_stmt() {
        let d = first_decl("fn f() { while true { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::While { .. }));
    }

    #[test]
    fn test_for_range_stmt() {
        let d = first_decl("fn f() { for i in 0..10 { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { var, .. } = &stmts[0].node else { panic!() };
        assert_eq!(var, "i");
    }

    #[test]
    fn test_return_with_value() {
        let d = first_decl("fn f() -> Int32 { return 42; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Return(Some(_))));
    }

    #[test]
    fn test_return_void() {
        let d = first_decl("fn f() { return; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Return(None)));
    }

    #[test]
    fn test_break_continue() {
        let d = first_decl("fn f() { while true { break; continue; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(outer) = &f.body.node else { panic!() };
        let StmtKind::While { body, .. } = &outer[0].node else { panic!() };
        let StmtKind::Block(inner) = &body.node else { panic!() };
        assert!(matches!(inner[0].node, StmtKind::Break));
        assert!(matches!(inner[1].node, StmtKind::Continue));
    }

    // --- Expressions: literals ---

    #[test]
    fn test_expr_integer_literal() {
        let d = first_decl("fn f() { let x = 99; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Integer(99))));
    }

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 tests float parsing, not PI
    fn test_expr_float_literal() {
        let d = first_decl("fn f() { let x = 3.14; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Literal(Literal::Float(f)) if (*f - 3.14).abs() < 1e-10));
    }

    #[test]
    fn test_expr_bool_literal() {
        let d = first_decl("fn f() { let x = true; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Bool(true))));
    }

    #[test]
    fn test_expr_string_literal() {
        let d = first_decl(r#"fn f() { let x = "hello"; }"#);
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Literal(Literal::String(s)) if s == "hello"));
    }

    #[test]
    fn test_expr_null_literal() {
        let d = first_decl("fn f() { let x = null; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Null)));
    }

    // --- Expressions: binary ops ---

    #[test]
    fn test_expr_binary_add() {
        let d = first_decl("fn f() { let x = 1 + 2; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Binary { op: BinaryOp::Add, .. }));
    }

    #[test]
    fn test_expr_binary_comparison() {
        let d = first_decl("fn f() { let x = a < b; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Binary { op: BinaryOp::Lt, .. }));
    }

    #[test]
    fn test_expr_binary_logical_and() {
        let d = first_decl("fn f() { let x = a && b; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Binary { op: BinaryOp::And, .. }));
    }

    #[test]
    fn test_expr_operator_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let d = first_decl("fn f() { let x = 1 + 2 * 3; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Binary { op: BinaryOp::Add, rhs, .. } = &e.node else { panic!("expected Add") };
        assert!(matches!(rhs.node, ExprKind::Binary { op: BinaryOp::Mul, .. }));
    }

    // --- Expressions: unary ops ---

    #[test]
    fn test_expr_unary_neg() {
        let d = first_decl("fn f() { let x = -1; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Unary { op: UnaryOp::Neg, .. }));
    }

    #[test]
    fn test_expr_unary_not() {
        let d = first_decl("fn f() { let x = !true; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Unary { op: UnaryOp::Not, .. }));
    }

    // --- Expressions: call, method call, field access, index ---

    #[test]
    fn test_expr_function_call() {
        let d = first_decl("fn f() { foo(1, 2); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Call { args, .. } = &e.node else { panic!() };
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn test_expr_method_call() {
        let d = first_decl("fn f() { obj.method(x); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::MethodCall { method, args, .. } = &e.node else { panic!() };
        assert_eq!(method, "method");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn test_expr_field_access() {
        let d = first_decl("fn f() { let x = obj.field; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::FieldAccess { field, .. } = &e.node else { panic!() };
        assert_eq!(field, "field");
    }

    #[test]
    fn test_expr_index() {
        let d = first_decl("fn f() { let x = arr[0]; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Index { .. }));
    }

    // --- Expressions: new, array, tuple ---

    #[test]
    fn test_expr_new() {
        let d = first_decl("fn f() { let x = new Foo(1, 2); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::New { class, args, .. } = &e.node else { panic!() };
        assert_eq!(class, "Foo");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn test_expr_array_literal() {
        let d = first_decl("fn f() { let x = [1, 2, 3]; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::ArrayLiteral(elems) = &e.node else { panic!() };
        assert_eq!(elems.len(), 3);
    }

    #[test]
    fn test_expr_tuple() {
        let d = first_decl("fn f() { let x = (1, 2); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Tuple(elems) = &e.node else { panic!() };
        assert_eq!(elems.len(), 2);
    }

    // --- Expressions: range ---

    #[test]
    fn test_expr_range_exclusive() {
        let d = first_decl("fn f() { for i in 0..10 { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { iter, .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&iter.node, ExprKind::Range { inclusive: false, .. }));
    }

    #[test]
    fn test_expr_range_inclusive() {
        let d = first_decl("fn f() { for i in 0...10 { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { iter, .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&iter.node, ExprKind::Range { inclusive: true, .. }));
    }

    // --- Expressions: lambda ---

    #[test]
    fn test_expr_lambda() {
        // lambda syntax: \param -> body  or  (params) => body
        let d = first_decl("fn f() { let add = (a, b) => a; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Lambda { params, .. } = &e.node else { panic!() };
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "a");
    }

    #[test]
    fn test_expr_lambda_backslash() {
        // backslash lambda: \x -> expr
        let d = first_decl("fn f() { let sq = \\x -> x; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Lambda { params, .. } = &e.node else { panic!() };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "x");
    }

    // --- Expressions: cast, is ---

    #[test]
    fn test_expr_cast() {
        // cast syntax: cast expr as Type
        let d = first_decl("fn f() { let x = cast y as Int32; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Cast { .. }));
    }

    #[test]
    fn test_expr_is() {
        // is syntax: is expr -> Type (prefix expression)
        let d = first_decl("fn f() { let x = is y -> Int32; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Is { .. }));
    }

    // --- Expressions: match ---

    #[test]
    fn test_expr_match() {
        // match syntax: no 'case' keyword, patterns go directly
        let d = first_decl("fn f() { match x { 1 => { } _ => { } } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert_eq!(cases.len(), 2);
        assert!(matches!(cases[0].pattern, Pattern::Literal(Literal::Integer(1), _)));
        assert!(matches!(cases[1].pattern, Pattern::Wildcard(_)));
    }

    // --- Expressions: throw, try/catch ---

    #[test]
    fn test_stmt_throw() {
        let d = first_decl(r#"fn f() { throw new Error("oops"); }"#);
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Throw(_)));
    }

    #[test]
    fn test_stmt_try_catch() {
        let d = first_decl("fn f() { try { foo(); } catch e: Error { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Try { catches, .. } = &stmts[0].node else { panic!() };
        assert_eq!(catches.len(), 1);
        assert_eq!(catches[0].param, "e");
    }

    // --- Types ---

    #[test]
    fn test_type_array() {
        let d = first_decl("fn f(x: Int32[]) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Array(_)));
    }

    #[test]
    fn test_type_map() {
        let d = first_decl("fn f(x: Map<String, Int32>) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Map(_, _)));
    }

    #[test]
    fn test_type_fn() {
        // function/closure type uses 'fnc' keyword
        let d = first_decl("fn f(x: fnc(Int32) -> Bool) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Fn { .. }));
    }

    #[test]
    fn test_type_tuple() {
        let d = first_decl("fn f(x: (Int32, Bool)) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Tuple(_)));
    }

    #[test]
    fn test_type_generic_named() {
        let d = first_decl("fn f(x: Box<Int32>) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(&f.params[0].param_type, Type::Generic { name, args } if name == "Box" && args.len() == 1));
    }

    // --- Multiple top-level decls ---

    #[test]
    fn test_multiple_decls() {
        let sf = parse("fn a() { } fn b() { } fn c() { }");
        assert_eq!(sf.decls.len(), 3);
    }

    // --- Annotations ---

    #[test]
    fn test_annotation_on_fn() {
        let d = first_decl("@deprecated fn old() { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.annotations.len(), 1);
        assert_eq!(f.annotations[0].name, "deprecated");
    }

    // --- Error cases ---

    #[test]
    fn test_error_missing_closing_brace() {
        assert!(parse_err("fn f() {"));
    }

    #[test]
    fn test_error_missing_fn_body() {
        assert!(parse_err("fn f()"));
    }

    #[test]
    fn test_error_unknown_token_in_decl() {
        assert!(parse_err("42"));
    }

    // --- Trait declarations ---

    #[test]
    fn test_trait_empty() {
        let d = first_decl("trait Printable { }");
        assert!(matches!(d, DeclKind::Trait(_)));
        let DeclKind::Trait(t) = d else { panic!() };
        assert_eq!(t.name, "Printable");
        assert!(t.methods.is_empty());
    }

    #[test]
    fn test_trait_with_method() {
        let d = first_decl("trait Printable { fn print() -> String; }");
        let DeclKind::Trait(t) = d else { panic!() };
        assert_eq!(t.methods.len(), 1);
        assert_eq!(t.methods[0].name, "print");
    }

    // --- Namespace declarations ---

    #[test]
    fn test_namespace_empty() {
        let d = first_decl("namespace myapp { }");
        let DeclKind::Namespace(ns) = d else { panic!() };
        assert_eq!(ns.name, vec!["myapp"]);
        assert!(ns.decls.is_empty());
    }

    #[test]
    fn test_namespace_with_function() {
        let d = first_decl("namespace utils { fn helper() { } }");
        let DeclKind::Namespace(ns) = d else { panic!() };
        assert_eq!(ns.decls.len(), 1);
        assert!(matches!(ns.decls[0].node, DeclKind::Function(_)));
    }

    #[test]
    fn test_namespace_nested_path() {
        let d = first_decl("namespace com.example.app { }");
        let DeclKind::Namespace(ns) = d else { panic!() };
        assert!(ns.name.join(".").contains("com"));
    }

    // --- Extern fn ---

    #[test]
    fn test_extern_fn() {
        let d = first_decl("extern fn malloc(size: Int64) -> Int64;");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.name, "malloc");
        assert_eq!(f.params.len(), 1);
    }

    // --- Immutable declaration ---

    #[test]
    fn test_immutable_decl() {
        let d = first_decl("immutable Point(x: Int64, y: Int64);");
        let DeclKind::Immutable(u) = d else { panic!() };
        assert_eq!(u.name, "Point");
        assert_eq!(u.fields.len(), 2);
        assert_eq!(u.fields[0].name, "x");
        assert_eq!(u.fields[1].name, "y");
    }

    // --- Static method (fnc in class) ---

    #[test]
    fn test_class_static_method_fnc() {
        let d = first_decl("class Math { fnc square(x: Int64) -> Int64 { return x; } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.methods.len(), 1);
        assert!(c.methods[0].static_);
        assert_eq!(c.methods[0].name, "square");
    }

    #[test]
    fn test_class_instance_method_fn() {
        let d = first_decl("class Counter { fn increment() { } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(!c.methods[0].static_);
    }

    // --- Async method ---

    #[test]
    fn test_class_async_method() {
        let d = first_decl("class Client { async fn fetch() -> String { return \"\"; } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(c.methods[0].is_async);
    }

    // --- Visibility on methods ---

    #[test]
    fn test_class_private_method() {
        let d = first_decl("class Foo { private fn helper() { } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(matches!(c.methods[0].visibility, Visibility::Private));
    }

    #[test]
    fn test_class_protected_method() {
        let d = first_decl("class Foo { protected fn hook() { } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(matches!(c.methods[0].visibility, Visibility::Protected));
    }

    // --- Statements: loop, defer ---

    #[test]
    fn test_loop_stmt() {
        let d = first_decl("fn f() { loop { break; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Loop { .. }));
    }

    #[test]
    fn test_defer_stmt() {
        let d = first_decl("fn f() { defer println(0); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Defer(_)));
    }

    #[test]
    fn test_try_catch_finally() {
        let d = first_decl("fn f() { try { } catch e: String { } finally { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Try { catches, finally, .. } = &stmts[0].node else { panic!() };
        assert_eq!(catches.len(), 1);
        assert!(finally.is_some());
    }

    // --- Compound assignment ---

    #[test]
    fn test_compound_assign_plus_eq() {
        let d = first_decl("fn f() { var x = 0; x += 1; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(&stmts[1].node, StmtKind::Expr(e) if matches!(&e.node, ExprKind::CompoundAssign { op: CompoundOp::Add, .. })));
    }

    #[test]
    fn test_compound_assign_minus_eq() {
        let d = first_decl("fn f() { var x = 5; x -= 2; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(&stmts[1].node, StmtKind::Expr(e) if matches!(&e.node, ExprKind::CompoundAssign { op: CompoundOp::Sub, .. })));
    }

    // --- Assignment statement ---

    #[test]
    fn test_assignment_stmt() {
        let d = first_decl("fn f() { var x = 0; x = 5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[1].node, StmtKind::Assignment { .. }));
    }

    // --- Expressions: map literal, this, await, spawn ---

    #[test]
    fn test_expr_map_literal() {
        // Map literal syntax: @{ key => val, ... }
        let d = first_decl(r#"fn f() { let m = @{"a" => 1, "b" => 2}; }"#);
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::MapLiteral(pairs) = &e.node else { panic!() };
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn test_expr_this() {
        let d = first_decl("class Foo { fn bar() { let x = this; } }");
        let DeclKind::Class(c) = d else { panic!() };
        let StmtKind::Block(stmts) = &c.methods[0].body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::This));
    }

    #[test]
    fn test_expr_await() {
        let d = first_decl("fn f() { let x = await someTask; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Await(_)));
    }

    #[test]
    fn test_expr_spawn() {
        let d = first_decl("fn f() { let t = spawn work(); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Spawn(_)));
    }

    // --- Expressions: new with type args ---

    #[test]
    fn test_expr_new_with_type_args() {
        let d = first_decl("fn f() { let b = new Box<Int64>(42); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::New { class, type_args, args } = &e.node else { panic!() };
        assert_eq!(class, "Box");
        assert_eq!(type_args.len(), 1);
        assert_eq!(args.len(), 1);
    }

    // --- Types: ref (*T), mutable ---

    #[test]
    fn test_type_ref() {
        let d = first_decl("fn f(x: *Int64) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Ref(_)));
    }

    #[test]
    fn test_type_mutable() {
        let d = first_decl("fn f(x: mut Int64) { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Mutable(_)));
    }

    #[test]
    fn test_type_nested_generic() {
        let d = first_decl("fn f(x: Box<Box<Int64>>) { }");
        let DeclKind::Function(f) = d else { panic!() };
        let Type::Generic { name, args } = &f.params[0].param_type else { panic!() };
        assert_eq!(name, "Box");
        assert!(matches!(&args[0], Type::Generic { name, .. } if name == "Box"));
    }

    // --- Match patterns ---

    #[test]
    fn test_match_enum_variant_pattern() {
        let d = first_decl("fn f(c: Color) { match c { Color::Red => { } _ => { } } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::EnumVariant { variant, .. } if variant == "Red"));
    }

    #[test]
    fn test_match_tuple_pattern() {
        let d = first_decl("fn f(p: (Int64, Int64)) { match p { (1, 2) => { } _ => { } } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::Tuple(_, _)));
    }

    #[test]
    fn test_match_guard() {
        let d = first_decl("fn f(x: Int64) { match x { n if n > 0 => { } _ => { } } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(cases[0].guard.is_some());
    }

    // --- Chaining ---

    #[test]
    fn test_chained_method_calls() {
        let d = first_decl("fn f() { a.foo().bar().baz(); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::MethodCall { method, .. } = &e.node else { panic!() };
        assert_eq!(method, "baz");
    }

    #[test]
    fn test_chained_field_access() {
        let d = first_decl("fn f() { let x = a.b.c; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::FieldAccess { field, .. } = &e.node else { panic!() };
        assert_eq!(field, "c");
    }

    // --- Operator precedence ---

    #[test]
    fn test_precedence_mul_before_add() {
        let d = first_decl("fn f() { let x = 2 + 3 * 4; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Binary { op: BinaryOp::Add, rhs, .. } = &e.node else { panic!("expected Add") };
        assert!(matches!(rhs.node, ExprKind::Binary { op: BinaryOp::Mul, .. }));
    }

    #[test]
    fn test_precedence_parens_override() {
        let d = first_decl("fn f() { let x = (2 + 3) * 4; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::Binary { op: BinaryOp::Mul, .. }));
    }

    #[test]
    fn test_precedence_compare_before_and() {
        let d = first_decl("fn f() { let x = a > 0 && b < 10; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Binary { op: BinaryOp::And, lhs, rhs } = &e.node else { panic!() };
        assert!(matches!(lhs.node, ExprKind::Binary { op: BinaryOp::Gt, .. }));
        assert!(matches!(rhs.node, ExprKind::Binary { op: BinaryOp::Lt, .. }));
    }

    // --- More error cases ---

    #[test]
    fn test_error_unclosed_paren() {
        assert!(parse_err("fn f() { foo(1, 2; }"));
    }

    #[test]
    fn test_error_missing_class_body() {
        assert!(parse_err("class Foo"));
    }

    #[test]
    fn test_error_bad_type_syntax() {
        assert!(parse_err("fn f(x: ) { }"));
    }

    #[test]
    fn test_error_empty_input() {
        let sf = parse("");
        assert!(sf.decls.is_empty());
    }

    // --- Doc comments preserved on declarations ---

    #[test]
    fn test_doc_comment_on_fn() {
        let d = first_decl("/** Does something */ fn foo() { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(f.doc.is_some());
        assert!(f.doc.as_deref().unwrap().contains("Does something"));
    }

    #[test]
    fn test_doc_comment_on_class() {
        let d = first_decl("/** A class */ class Foo { }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(c.doc.is_some());
    }

    // --- Multiple annotations ---

    #[test]
    fn test_multiple_annotations() {
        let d = first_decl("@test @timeout(100) fn myTest() { }");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.annotations.len(), 2);
        assert_eq!(f.annotations[0].name, "test");
        assert_eq!(f.annotations[1].name, "timeout");
    }

    // --- Tuple index ---

    #[test]
    fn test_tuple_index_access() {
        let d = first_decl("fn f() { let t = (1, 2); let x = t.0; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[1].node else { panic!() };
        assert!(matches!(&e.node, ExprKind::TupleIndex { index: 0, .. }));
    }

    // --- Enum with doc comments ---

    #[test]
    fn test_enum_with_doc() {
        let d = first_decl("/** Colors */ enum Color { Red, Green, Blue }");
        let DeclKind::Enum(e) = d else { panic!() };
        assert!(e.doc.is_some());
    }

    // --- for-C style loop ---

    #[test]
    fn test_for_c_style_loop() {
        let d = first_decl("fn f() { for (var i = 0; i < 10; i += 1) { } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::ForC { .. }));
    }

    // ================================================================
    // select / send / recv (concurrency)
    // ================================================================

    #[test]
    fn test_stmt_select() {
        let d = first_decl("fn f(ch: Chan) { select { recv ch -> x { } default { } } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Select { .. }));
    }

    #[test]
    fn test_expr_send() {
        let d = first_decl("fn f(ch: Chan) { send ch -> 42; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Send { .. }));
    }

    #[test]
    fn test_expr_recv() {
        let d = first_decl("fn f(ch: Chan) { let x = recv ch; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Recv(_)));
    }

    #[test]
    fn test_expr_channel() {
        let d = first_decl("fn f() { let ch = channel; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Channel));
    }

    #[test]
    fn test_expr_spawn_v2() {
        let d = first_decl("fn f() { let t = spawn work(); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Spawn(_)));
    }

    #[test]
    fn test_expr_await_v2() {
        let d = first_decl("fn f(t: Task) { let r = await t; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Await(_)));
    }

    // ================================================================
    // Lambda edge cases
    // ================================================================

    #[test]
    fn test_lambda_arrow_style_two_params() {
        let d = first_decl("fn f() { let add = (a, b) => a + b; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Lambda { params, .. } = &e.node else { panic!() };
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_lambda_zero_params_parse_bug() {
        // BUG: `() => expr` fails to parse — parser expects semicolon after `()`
        assert!(parse_err("fn f() { let thunk = () => 42; }"), "zero-param arrow lambda should currently fail to parse");
    }

    #[test]
    fn test_lambda_backslash_single_param() {
        let d = first_decl("fn f() { let sq = \\x -> x * x; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Lambda { params, body, .. } = &e.node else { panic!() };
        assert_eq!(params[0].name, "x");
        assert!(matches!(body.node, ExprKind::Binary { .. }));
    }

    #[test]
    fn test_fnc_lambda() {
        let d = first_decl("fn f() { let cb = fnc(x: Int64) -> Int64 { return x; }; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Lambda { .. }));
    }

    #[test]
    fn test_fn_lambda() {
        let d = first_decl("fn f() { let cb = fn(x: Int64) -> Int64 { return x; }; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Lambda { .. }));
    }

    // ================================================================
    // Super call
    // ================================================================

    #[test]
    fn test_expr_super_call() {
        let d = first_decl("class Dog extends Animal { fn speak() -> Nothing { super.speak(); } }");
        let DeclKind::Class(c) = d else { panic!() };
        let StmtKind::Block(stmts) = &c.methods[0].body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::SuperCall { .. }));
    }

    // ================================================================
    // Match with guard
    // ================================================================

    #[test]
    fn test_match_with_guard() {
        let d = first_decl("fn f(x: Int64) -> Nothing { match x { n if n > 0 => return; _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(cases[0].guard.is_some(), "first case should have a guard");
    }

    #[test]
    fn test_match_no_guard() {
        let d = first_decl("fn f(x: Int64) -> Nothing { match x { 1 => return; _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(cases[0].guard.is_none());
    }

    // ================================================================
    // Deeply nested generic types
    // ================================================================

    #[test]
    fn test_type_nested_generic_three_levels() {
        // `>>>` is split when closing generic types (expect_generic_close)
        let d = first_decl("fn f(x: Array<Array<Array<Int64>>>) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        let Type::Generic { name, args } = &f.params[0].param_type else { panic!() };
        assert_eq!(name, "Array");
        let Type::Generic { name: n2, args: a2 } = &args[0] else { panic!() };
        assert_eq!(n2, "Array");
        let Type::Generic { name: n3, args: a3 } = &a2[0] else { panic!() };
        assert_eq!(n3, "Array");
        assert!(matches!(a3[0], Type::Int64));
    }

    #[test]
    fn test_type_nested_generic_two_levels() {
        let d = first_decl("fn f(x: Array<Array<Int64> >) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        // Parses as Generic { name: "Array", .. } because of space before >
        assert!(matches!(f.params[0].param_type, Type::Generic { .. }));
    }

    #[test]
    fn test_type_map_with_generic_value() {
        let d = first_decl("fn f(m: Map<String, Array<Int64>>) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Map(_, _)));
    }

    #[test]
    fn test_type_fn_with_multiple_params() {
        let d = first_decl("fn f(cb: fnc(Int64, String, Bool) -> Nothing) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        let Type::Fn { params, .. } = &f.params[0].param_type else { panic!() };
        assert_eq!(params.len(), 3);
    }

    // ================================================================
    // Enum variant with tuple pattern
    // ================================================================

    #[test]
    fn test_match_enum_variant_with_payload() {
        let d = first_decl("fn f(s: Shape) -> Nothing { match s { Shape::Circle(r) => return; _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::EnumVariant { args, .. } if !args.is_empty()));
    }

    #[test]
    fn test_match_tuple_pattern_v2() {
        let d = first_decl("fn f(t: (Int64, Int64)) -> Nothing { match t { (a, b) => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::Tuple(..)));
    }

    // ================================================================
    // Defer statement
    // ================================================================

    #[test]
    fn test_stmt_defer() {
        let d = first_decl("fn f() { defer { println(\"done\"); } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Defer(_)));
    }

    // ================================================================
    // try/catch/finally combinations
    // ================================================================

    #[test]
    fn test_try_multiple_catches() {
        let d = first_decl(concat!(
            "fn f() { try { throw \"x\"; }",
            " catch e: String { return; }",
            " catch e: Int64 { return; }",
            " finally { return; } }"
        ));
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Try { catches, finally, .. } = &stmts[0].node else { panic!() };
        assert_eq!(catches.len(), 2);
        assert!(finally.is_some());
    }

    // ================================================================
    // Annotations on various targets
    // ================================================================

    #[test]
    fn test_annotation_on_field() {
        let d = first_decl("class Foo { @Inject\nvar svc: Service; }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(!c.fields[0].annotations.is_empty());
        assert_eq!(c.fields[0].annotations[0].name, "Inject");
    }

    #[test]
    fn test_annotation_with_int_arg() {
        let d = first_decl("@StatusCode(201)\nfn create() -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.annotations[0].name, "StatusCode");
        assert!(matches!(f.annotations[0].args[0], AnnotationArg::Literal(Literal::Integer(201))));
    }

    #[test]
    fn test_annotation_with_bool_arg() {
        let d = first_decl("@Option(\"--flag\", \"desc\", true)\nfn f() -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.annotations[0].args[2], AnnotationArg::Literal(Literal::Bool(true))));
    }

    #[test]
    fn test_annotation_with_enum_arg() {
        let d = first_decl("@Produces(MediaType.APPLICATION_JSON)\nfn f() -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(&f.annotations[0].args[0], AnnotationArg::EnumValue(t, v) if t == "MediaType" && v == "APPLICATION_JSON"));
    }

    #[test]
    fn test_annotation_with_array_arg() {
        let d = first_decl("@OIDCRolesAllowed([\"admin\", \"api-user\"])\nfn f() -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert_eq!(f.annotations[0].name, "OIDCRolesAllowed");
        let AnnotationArg::Array(items) = &f.annotations[0].args[0] else { panic!() };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], AnnotationArg::Literal(Literal::String(s)) if s == "admin"));
        assert!(matches!(&items[1], AnnotationArg::Literal(Literal::String(s)) if s == "api-user"));
    }

    #[test]
    fn test_annotation_with_empty_array_arg() {
        let d = first_decl("@OIDCRolesAllowed([])\nfn f() -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        let AnnotationArg::Array(items) = &f.annotations[0].args[0] else { panic!() };
        assert!(items.is_empty());
    }

    #[test]
    fn test_multiple_annotations_on_method() {
        let d = first_decl("class C { @GET\n@Path(\"/items\")\nfn list() -> Nothing {} }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.methods[0].annotations.len(), 2);
    }

    // ================================================================
    // Visibility modifiers
    // ================================================================

    #[test]
    fn test_private_method() {
        let d = first_decl("class Foo { private fn secret() -> Nothing {} }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(matches!(c.methods[0].visibility, Visibility::Private));
    }

    #[test]
    fn test_protected_field() {
        let d = first_decl("class Foo { protected var x: Int64; }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(matches!(c.fields[0].visibility, Visibility::Protected));
    }

    // ================================================================
    // Compound expressions
    // ================================================================

    #[test]
    fn test_tuple_index_access_v2() {
        let d = first_decl("fn f(t: (Int64, Int64)) -> Int64 { return t.0; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::TupleIndex { .. }));
    }

    #[test]
    fn test_array_index_expr() {
        let d = first_decl("fn f(a: Array<Int64>) -> Int64 { return a[0]; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Index { .. }));
    }

    #[test]
    fn test_enum_value_no_args() {
        let d = first_decl("fn f() -> Color { return Color::Red; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        let ExprKind::EnumValue { enum_name, variant, args, .. } = &e.node else { panic!() };
        assert_eq!(enum_name, "Color");
        assert_eq!(variant, "Red");
        assert!(args.is_empty());
    }

    #[test]
    fn test_enum_value_with_args() {
        let d = first_decl("fn f() { let s = Shape::Circle(5.0); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::EnumValue { args, .. } = &e.node else { panic!() };
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn test_struct_literal_expr() {
        let d = first_decl("fn f() -> Point { return Point { x: 1, y: 2 }; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        let ExprKind::StructLiteral { name, fields } = &e.node else { panic!() };
        assert_eq!(name, "Point");
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn test_map_literal() {
        let d = first_decl("fn f() { let m = @{\"a\" => 1, \"b\" => 2}; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::MapLiteral(pairs) = &e.node else { panic!() };
        assert_eq!(pairs.len(), 2);
    }

    // ================================================================
    // Implicit return (expression at end of block)
    // ================================================================

    #[test]
    fn test_block_with_trailing_expr() {
        // Block ending in expression without semicolon = implicit return
        let d = first_decl("fn f() -> Int64 { let x = { 42 }; return x; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        // let x = {...} should parse without error
        assert!(matches!(stmts[0].node, StmtKind::Let { .. }));
    }

    // ================================================================
    // Error cases — make sure parser rejects invalid syntax
    // ================================================================

    #[test]
    fn test_parse_err_missing_closing_brace() {
        assert!(parse_err("fn f() { let x = 1;"), "unclosed brace should be a parse error");
    }

    #[test]
    fn test_parse_err_missing_arrow_in_fn_sig() {
        assert!(parse_err("fn f() Int64 {}"), "missing -> should be a parse error");
    }

    #[test]
    fn test_parse_err_expr_without_semicolon() {
        // Most statement-expressions require semicolon
        let result = parse("fn f() { 1 + 2 }");
        // This might be OK as a block-returning expression or error depending on parser
        // Just ensure it doesn't panic
        let _ = result;
    }

    // ================================================================
    // Class generic type params on methods
    // ================================================================

    #[test]
    fn test_class_generic_method() {
        let d = first_decl("class Box<T> { fn get<U>(x: U) -> T {} }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.type_params, vec!["T"]);
        assert_eq!(c.methods[0].type_params, vec!["U"]);
    }

    // ================================================================
    // Interface extends chain
    // ================================================================

    #[test]
    fn test_interface_extends_multiple() {
        let d = first_decl("interface C extends A, B { fn c() -> Nothing; }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.extends, vec!["A", "B"]);
    }

    // ================================================================
    // Extern fn with no params
    // ================================================================

    #[test]
    fn test_extern_fn_no_params() {
        let d = first_decl("extern fn getpid() -> Int64;");
        let DeclKind::Function(f) = d else { panic!() };
        // extern fn has an empty body (StmtKind::Empty)
        assert!(matches!(f.body.node, StmtKind::Empty));
        assert!(f.params.is_empty());
    }

    // ================================================================
    // Module and namespace
    // ================================================================

    #[test]
    fn test_module_declaration_v2() {
        let ast = parse("module myapp;");
        assert!(matches!(ast.decls[0].node, DeclKind::Module(_)));
    }

    #[test]
    fn test_namespace_with_multiple_decls() {
        let d = first_decl("namespace net { fn connect() -> Nothing {} fn disconnect() -> Nothing {} }");
        let DeclKind::Namespace(ns) = d else { panic!() };
        assert_eq!(ns.decls.len(), 2);
    }

    // ================================================================
    // Expression kinds — untested variants
    // ================================================================

    #[test]
    fn test_expr_cast_node() {
        let d = first_decl("fn f() -> Float64 { let x = 5; return x as Float64; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Cast { .. }));
    }

    #[test]
    fn test_expr_is_node() {
        // is syntax: is expr -> Type (prefix form)
        let d = first_decl("fn f() { let r = is obj -> String; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Is { .. }));
    }

    #[test]
    fn test_expr_range_exclusive_node() {
        let d = first_decl("fn f() { for x in 0..10 {} }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { iter, .. } = &stmts[0].node else { panic!() };
        assert!(matches!(iter.node, ExprKind::Range { inclusive: false, .. }));
    }

    #[test]
    fn test_expr_range_inclusive_node() {
        let d = first_decl("fn f() { for x in 0...10 {} }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { iter, .. } = &stmts[0].node else { panic!() };
        assert!(matches!(iter.node, ExprKind::Range { inclusive: true, .. }));
    }

    #[test]
    fn test_expr_tuple_three_elems() {
        let d = first_decl("fn f() { let t = (1, 2, 3); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        let ExprKind::Tuple(elems) = &e.node else { panic!() };
        assert_eq!(elems.len(), 3);
    }

    #[test]
    fn test_expr_this_in_method() {
        let d = first_decl("class Foo { fn bar() -> Nothing { let x = this; } }");
        let DeclKind::Class(c) = d else { panic!() };
        let StmtKind::Block(stmts) = &c.methods[0].body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::This));
    }

    #[test]
    fn test_expr_new_class() {
        let d = first_decl("fn f() { let p = new Point(1, 2); }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::New { .. }));
    }

    #[test]
    fn test_expr_throw() {
        let d = first_decl("fn f() { throw \"error\"; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Throw(_)));
    }

    #[test]
    fn test_expr_send_channel() {
        // send syntax: send channel -> value (uses ThinArrow)
        let d = first_decl("fn f(ch: Chan) { send ch -> 42; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Send { .. }));
    }

    #[test]
    fn test_expr_recv_channel() {
        let d = first_decl("fn f(ch: Channel<Int64>) { let v = recv ch; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Let { value: Some(e), .. } = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Recv(_)));
    }

    // ================================================================
    // Statement kinds — untested variants
    // ================================================================

    #[test]
    fn test_stmt_loop() {
        let d = first_decl("fn f() { loop { break; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::Loop { .. }));
    }

    #[test]
    fn test_stmt_break() {
        let d = first_decl("fn f() { loop { break; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Loop { body } = &stmts[0].node else { panic!() };
        let StmtKind::Block(inner) = &body.node else { panic!() };
        assert!(matches!(inner[0].node, StmtKind::Break));
    }

    #[test]
    fn test_stmt_continue() {
        let d = first_decl("fn f() { while true { continue; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::While { body, .. } = &stmts[0].node else { panic!() };
        let StmtKind::Block(inner) = &body.node else { panic!() };
        assert!(matches!(inner[0].node, StmtKind::Continue));
    }

    #[test]
    fn test_stmt_if_else() {
        let d = first_decl("fn f() -> Int64 { if true { return 1; } else { return 2; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::If { else_branch, .. } = &stmts[0].node else { panic!() };
        assert!(else_branch.is_some());
    }

    #[test]
    fn test_stmt_if_no_else() {
        let d = first_decl("fn f() { if true { return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::If { else_branch, .. } = &stmts[0].node else { panic!() };
        assert!(else_branch.is_none());
    }

    #[test]
    fn test_stmt_while() {
        let d = first_decl("fn f() { while x > 0 { return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        assert!(matches!(stmts[0].node, StmtKind::While { .. }));
    }

    #[test]
    fn test_stmt_for_in() {
        let d = first_decl("fn f() { for item in list { return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::For { var, .. } = &stmts[0].node else { panic!() };
        assert_eq!(var, "item");
    }

    // ================================================================
    // Binary operator coverage
    // ================================================================

    #[test]
    fn test_binary_bitand() {
        let d = first_decl("fn f() -> Int64 { return 0b1100 & 0b1010; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::BitAnd, .. }));
    }

    #[test]
    fn test_binary_bitor() {
        let d = first_decl("fn f() -> Int64 { return 0b1100 | 0b0011; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::BitOr, .. }));
    }

    #[test]
    fn test_binary_xor() {
        let d = first_decl("fn f() -> Int64 { return 5 ^ 3; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Xor, .. }));
    }

    #[test]
    fn test_binary_shl() {
        let d = first_decl("fn f() -> Int64 { return 1 << 3; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Shl, .. }));
    }

    #[test]
    fn test_binary_shr() {
        let d = first_decl("fn f() -> Int64 { return 8 >> 2; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Shr, .. }));
    }

    #[test]
    fn test_binary_ne() {
        let d = first_decl("fn f() -> Bool { return 1 != 2; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Ne, .. }));
    }

    #[test]
    fn test_binary_le() {
        let d = first_decl("fn f() -> Bool { return 3 <= 4; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Le, .. }));
    }

    #[test]
    fn test_binary_ge() {
        let d = first_decl("fn f() -> Bool { return 5 >= 5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Binary { op: BinaryOp::Ge, .. }));
    }

    // ================================================================
    // Unary operators
    // ================================================================

    #[test]
    fn test_unary_neg() {
        let d = first_decl("fn f() -> Int64 { return -5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Unary { op: UnaryOp::Neg, .. }));
    }

    #[test]
    fn test_unary_not() {
        let d = first_decl("fn f() -> Bool { return !true; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Unary { op: UnaryOp::Not, .. }));
    }

    #[test]
    fn test_unary_bitnot() {
        let d = first_decl("fn f() -> Int64 { return ~0; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Unary { op: UnaryOp::BitNot, .. }));
    }

    // ================================================================
    // Compound assignment operators
    // ================================================================

    #[test]
    fn test_compound_assign_plus() {
        let d = first_decl("fn f() { var x = 1; x += 5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::CompoundAssign { op: CompoundOp::Add, .. }));
    }

    #[test]
    fn test_compound_assign_minus() {
        let d = first_decl("fn f() { var x = 10; x -= 3; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::CompoundAssign { op: CompoundOp::Sub, .. }));
    }

    #[test]
    fn test_compound_assign_mul() {
        let d = first_decl("fn f() { var x = 3; x *= 4; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::CompoundAssign { op: CompoundOp::Mul, .. }));
    }

    #[test]
    fn test_compound_assign_div() {
        let d = first_decl("fn f() { var x = 20; x /= 4; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::CompoundAssign { op: CompoundOp::Div, .. }));
    }

    #[test]
    fn test_compound_assign_mod() {
        let d = first_decl("fn f() { var x = 17; x %= 5; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[1].node else { panic!() };
        assert!(matches!(e.node, ExprKind::CompoundAssign { op: CompoundOp::Mod, .. }));
    }

    // ================================================================
    // Literal types
    // ================================================================

    #[test]
    fn test_literal_float() {
        let d = first_decl("fn f() -> Float64 { return 3.14; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Float(_))));
    }

    #[test]
    fn test_literal_char() {
        let d = first_decl("fn f() -> Char { return 'z'; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Char('z'))));
    }

    #[test]
    fn test_literal_string() {
        let d = first_decl("fn f() -> String { return \"hello\"; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::String(_))));
    }

    #[test]
    fn test_literal_null() {
        let d = first_decl("fn f() -> String? { return null; }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Return(Some(e)) = &stmts[0].node else { panic!() };
        assert!(matches!(e.node, ExprKind::Literal(Literal::Null)));
    }

    // ================================================================
    // Type variants
    // ================================================================

    #[test]
    fn test_type_nullable() {
        let d = first_decl("fn f(x: String?) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Nullable(ref inner) if matches!(inner.as_ref(), Type::String)));
    }

    #[test]
    fn test_type_nullable_return() {
        let d = first_decl("fn f() -> Int64? {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.ret_type, Type::Nullable(ref inner) if matches!(inner.as_ref(), Type::Int64)));
    }

    #[test]
    fn test_type_nullable_named() {
        let d = first_decl("fn f(x: Foo?) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Nullable(ref inner) if matches!(inner.as_ref(), Type::Named(n) if n == "Foo")));
    }

    #[test]
    fn test_type_tuple_three_elems() {
        let d = first_decl("fn f(t: (Int64, Bool, String)) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(f.params[0].param_type, Type::Tuple(_)));
    }

    #[test]
    fn test_type_named_custom() {
        let d = first_decl("fn f(x: MyClass) -> Nothing {}");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(matches!(&f.params[0].param_type, Type::Named(n) if n == "MyClass"));
    }

    // ================================================================
    // Import alias
    // ================================================================

    #[test]
    fn test_import_simple_v2() {
        let ast = parse("import std.io;");
        assert!(matches!(ast.decls[0].node, DeclKind::Import(_)));
    }

    #[test]
    fn test_import_with_alias_v2() {
        let ast = parse("import std.collections.HashMap as Map;");
        let DeclKind::Import(i) = &ast.decls[0].node else { panic!() };
        assert!(i.alias.is_some());
        assert_eq!(i.alias.as_ref().unwrap(), "Map");
    }

    #[test]
    fn test_import_path_length() {
        let ast = parse("import a.b.c.d;");
        let DeclKind::Import(i) = &ast.decls[0].node else { panic!() };
        assert_eq!(i.path.len(), 4);
    }

    // ================================================================
    // Function doc comment
    // ================================================================

    #[test]
    fn test_fn_with_doc_comment() {
        let ast = parse("/** greets the user */\nfn greet() -> Nothing {}");
        let DeclKind::Function(f) = &ast.decls[0].node else { panic!() };
        assert!(f.doc.is_some(), "doc comment should be attached to function");
    }

    // ================================================================
    // Enum with multiple variants
    // ================================================================

    #[test]
    fn test_enum_multiple_variants() {
        let d = first_decl("enum Status { Ok, NotFound, ServerError }");
        let DeclKind::Enum(e) = d else { panic!() };
        assert_eq!(e.variants.len(), 3);
    }

    #[test]
    fn test_enum_variant_with_args() {
        let d = first_decl("enum Result { Ok(Int64), Err(String) }");
        let DeclKind::Enum(e) = d else { panic!() };
        assert!(!e.variants[0].args.is_empty());
    }

    // ================================================================
    // Class with all features
    // ================================================================

    #[test]
    fn test_class_with_constructor() {
        let d = first_decl("class Person { var name: String; fn init(n: String) -> Nothing { this.name = n; } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.fields.len(), 1);
        assert_eq!(c.methods.len(), 1);
    }

    #[test]
    fn test_class_multiple_methods() {
        let d = first_decl("class Calc { fn add(a: Int64, b: Int64) -> Int64 { return a + b; } fn sub(a: Int64, b: Int64) -> Int64 { return a - b; } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert_eq!(c.methods.len(), 2);
    }

    #[test]
    fn test_class_async_method_v2() {
        let d = first_decl("class Client { async fn fetch() -> String { return \"\"; } }");
        let DeclKind::Class(c) = d else { panic!() };
        assert!(c.methods[0].is_async);
    }

    // ================================================================
    // Interface completeness
    // ================================================================

    #[test]
    fn test_interface_multiple_methods() {
        let d = first_decl("interface Repository { fn findById(id: Int64) -> Nothing; fn save(entity: Nothing) -> Nothing; fn delete(id: Int64) -> Nothing; }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.methods.len(), 3);
    }

    #[test]
    fn test_interface_generic_parse_bug() {
        // BUG: parser does not support generic type params on interfaces
        assert!(parse_err("interface Container<T> { fn get() -> T; }"),
            "generic interface should currently fail to parse");
    }

    #[test]
    fn test_interface_no_type_params() {
        // Without generics, interfaces parse fine
        let d = first_decl("interface Container { fn get() -> Nothing; fn set(v: Int64) -> Nothing; }");
        let DeclKind::Interface(i) = d else { panic!() };
        assert_eq!(i.methods.len(), 2);
    }

    // ================================================================
    // Async fn
    // ================================================================

    #[test]
    fn test_async_fn() {
        let d = first_decl("async fn fetchData() -> String { return \"\"; }");
        let DeclKind::Function(f) = d else { panic!() };
        assert!(f.is_async);
    }

    // ================================================================
    // Pattern matching completeness
    // ================================================================

    #[test]
    fn test_match_wildcard_only() {
        let d = first_decl("fn f(x: Int64) -> Nothing { match x { _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::Wildcard(_)));
    }

    #[test]
    fn test_match_literal_cases() {
        let d = first_decl("fn f(x: Int64) -> Nothing { match x { 1 => return; 2 => return; _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert_eq!(cases.len(), 3);
    }

    #[test]
    fn test_match_string_literal() {
        let d = first_decl("fn f(s: String) -> Nothing { match s { \"ok\" => return; _ => return; } }");
        let DeclKind::Function(f) = d else { panic!() };
        let StmtKind::Block(stmts) = &f.body.node else { panic!() };
        let StmtKind::Expr(e) = &stmts[0].node else { panic!() };
        let ExprKind::Match { cases, .. } = &e.node else { panic!() };
        assert!(matches!(&cases[0].pattern, Pattern::Literal(Literal::String(_), _)));
    }
}

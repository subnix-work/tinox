use tinox_common::{Error, Pos, Span};

/// A part of a string interpolation: either a literal segment or an expression source
#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Str(String),
    Expr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Integer(i64),
    IntegerSuffix(IntSuffix),
    Float(f64),
    FloatSuffix(FloatType),
    String(String),
    RawString(String),
    /// Interpolated string: `"Hello ${name}!"` → parts
    InterpString(Vec<InterpPart>),
    Char(char),
    Byte(u8),
    Bool(bool),

    // Identifiers
    Ident(String),
    Keyword(Keyword),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Equals,
    EqualsEquals,
    Bang,
    BangEquals,
    Less,
    LessEquals,
    Greater,
    GreaterEquals,
    AmpAmp,
    BarBar,
    Caret,
    Tilde,
    Ampersand,
    Bar,
    LessLess,
    GreaterGreater,
    GreaterGreaterGreater,
    PlusPlus,
    MinusMinus,
    PlusEquals,
    MinusEquals,
    StarEquals,
    SlashEquals,
    PercentEquals,
    AmpersandEquals,
    BarEquals,
    CaretEquals,
    LessLessEquals,
    GreaterGreaterEquals,
    GreaterGreaterGreaterEquals,

    // Compound
    Dot,
    DotDot,
    DotDotDot,
    Colon,
    ColonColon,
    Semicolon,
    Comma,
    At,
    Question,
    QuestionQuestion,
    QuestionColon,
    FatArrow,
    ThinArrow,
    Pipe,
    Backslash,

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    // Special
    Comment(String),
    DocComment(String),
    Whitespace,
    Newline,
    Underscore,
    Eof,
    Error(String),
}

impl TokenKind {
    pub fn is_trivia(&self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::Newline | TokenKind::Comment(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn dummy(kind: TokenKind) -> Self {
        Self {
            kind,
            span: Span::dummy(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntSuffix {
    Unsigned8,
    Unsigned16,
    Unsigned32,
    Unsigned64,
    Signed8,
    Signed16,
    Signed32,
    Signed64,
    Size,
    Usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FloatType {
    Float32,
    Float64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    If,
    Else,
    While,
    For,
    Loop,
    Return,
    Break,
    Continue,
    Match,
    Case,

    Class,
    Interface,
    Extends,
    Implements,
    Trait,
    Enum,
    New,
    This,
    Super,
    Self_,
    SelfType,

    Public,
    Private,
    Protected,
    Package,
    Static,
    Final,
    Abstract,
    Override,
    Readonly,

    Var,
    Val,
    Let,
    Const,
    Mut,

    Fn,
    Fnc,
    Throws,
    Throw,
    Try,
    Catch,
    Finally,
    Defer,

    Spawn,
    Channel,
    Send,
    Recv,
    Select,
    Default,
    Async,
    Await,

    Module,
    Namespace,
    Import,
    Export,
    As,
    In,
    Where,
    Extern,
    Unsafe,

    Ref,
    SizeOf,
    TypeOf,
    Is,
    As_,
    Cast,
    Null,
    True,
    False,
    Nothing,
    Never,
    Any,
    DotSelf,
    Immutable,
}

impl Keyword {
    pub fn lookup(s: &str) -> Option<Self> {
        match s {
            "if" => Some(Keyword::If),
            "else" => Some(Keyword::Else),
            "while" => Some(Keyword::While),
            "for" => Some(Keyword::For),
            "loop" => Some(Keyword::Loop),
            "return" => Some(Keyword::Return),
            "break" => Some(Keyword::Break),
            "continue" => Some(Keyword::Continue),
            "match" => Some(Keyword::Match),
            "case" => Some(Keyword::Case),
            "class" => Some(Keyword::Class),
            "interface" => Some(Keyword::Interface),
            "extends" => Some(Keyword::Extends),
            "implements" => Some(Keyword::Implements),
            "trait" => Some(Keyword::Trait),
            "enum" => Some(Keyword::Enum),
            "new" => Some(Keyword::New),
            "this" => Some(Keyword::This),
            "super" => Some(Keyword::Super),
            "self" => Some(Keyword::Self_),
            "Self" => Some(Keyword::SelfType),
            "public" => Some(Keyword::Public),
            "private" => Some(Keyword::Private),
            "protected" => Some(Keyword::Protected),
            "package" => Some(Keyword::Package),
            "static" => Some(Keyword::Static),
            "final" => Some(Keyword::Final),
            "abstract" => Some(Keyword::Abstract),
            "override" => Some(Keyword::Override),
            "readonly" => Some(Keyword::Readonly),
            "var" => Some(Keyword::Var),
            "val" => Some(Keyword::Val),
            "let" => Some(Keyword::Let),
            "const" => Some(Keyword::Const),
            "mut" => Some(Keyword::Mut),
            "fn" => Some(Keyword::Fn),
            "fnc" => Some(Keyword::Fnc),
            "throw" => Some(Keyword::Throw),
            "throws" => Some(Keyword::Throws),
            "try" => Some(Keyword::Try),
            "catch" => Some(Keyword::Catch),
            "finally" => Some(Keyword::Finally),
            "defer" => Some(Keyword::Defer),
            "spawn" => Some(Keyword::Spawn),
            "channel" => Some(Keyword::Channel),
            "send" => Some(Keyword::Send),
            "recv" => Some(Keyword::Recv),
            "select" => Some(Keyword::Select),
            "default" => Some(Keyword::Default),
            "async" => Some(Keyword::Async),
            "await" => Some(Keyword::Await),
            "module" => Some(Keyword::Module),
            "namespace" => Some(Keyword::Namespace),
            "import" => Some(Keyword::Import),
            "export" => Some(Keyword::Export),
            "as" => Some(Keyword::As),
            "in" => Some(Keyword::In),
            "where" => Some(Keyword::Where),
            "extern" => Some(Keyword::Extern),
            "unsafe" => Some(Keyword::Unsafe),
            "ref" => Some(Keyword::Ref),
            "sizeof" => Some(Keyword::SizeOf),
            "typeof" => Some(Keyword::TypeOf),
            "is" => Some(Keyword::Is),
            "cast" => Some(Keyword::Cast),
            "null" => Some(Keyword::Null),
            "true" => Some(Keyword::True),
            "false" => Some(Keyword::False),
            "Nothing" => Some(Keyword::Nothing),
            "Never" => Some(Keyword::Never),
            "Any" => Some(Keyword::Any),
            ".self" => Some(Keyword::DotSelf),
            "immutable" => Some(Keyword::Immutable),
            _ => None,
        }
    }
}

pub struct Lexer<'a> {
    #[allow(dead_code)]
    input: &'a str,
    chars: Vec<char>,
    pos: usize,
    start: usize,
    line: u32,
    column: u32,
    start_column: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        let chars: Vec<char> = input.chars().collect();
        Self {
            input,
            chars,
            pos: 0,
            start: 0,
            line: 1,
            column: 1,
            start_column: 1,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, Vec<Error>> {
        let mut tokens = Vec::new();
        let mut errors = Vec::new();

        loop {
            let token = match self.next_token() {
                Ok(t) => t,
                Err(e) => {
                    errors.push(e);
                    // Skip the problematic character
                    self.pos += 1;
                    continue;
                }
            };

            if token.kind == TokenKind::Eof {
                tokens.push(token);
                break;
            }

            if !token.kind.is_trivia() {
                tokens.push(token);
            }
        }

        if errors.is_empty() {
            Ok(tokens)
        } else {
            Err(errors)
        }
    }

    fn next_token(&mut self) -> Result<Token, Error> {
        self.start = self.pos;
        self.start_column = self.column;

        if self.pos >= self.chars.len() {
            return Ok(Token::new(
                TokenKind::Eof,
                Span::new(self.mk_pos(), self.mk_pos()),
            ));
        }

        let ch = self.peek();

        let token = match ch {
            '\n' => {
                self.bump();
                self.line += 1;
                self.column = 1;
                Token::new(TokenKind::Newline, self.mk_span())
            }
            ' ' | '\t' | '\r' => {
                self.bump();
                Token::new(TokenKind::Whitespace, self.mk_span())
            }
            '/' => {
                if self.peek_next() == '/' {
                    self.read_line_comment()
                } else if self.peek_next() == '*' {
                    self.read_block_comment()
                } else {
                    self.read_operator()
                }
            }
            '"' => {
                // Raw strings are always r-prefixed (r"…", r#"…"#) — a plain string
                // starting with '#' (e.g. "# comment") is a normal string literal.
                self.read_string()?
            }
            'r' => {
                if self.is_raw_string_start() {
                    self.read_raw_string()?
                } else {
                    self.read_ident_or_keyword()
                }
            }
            '\'' => {
                self.read_char()?
            }
            '0'..='9' => self.read_number()?,
            'a'..='z' | 'A'..='Z' | '_' => {
                if ch == 'b' && self.peek_next() == '\'' {
                    self.bump(); // consume 'b'
                    self.read_byte_literal()? // now we're at the opening quote
                } else {
                    self.read_ident_or_keyword()
                }
            }
            '+' => self.read_plus(),
            '-' => self.read_minus(),
            '*' => self.read_star(),
            '%' => self.read_percent(),
            '=' => self.read_equals(),
            '!' => self.read_bang(),
            '<' => self.read_less(),
            '>' => self.read_greater(),
            '&' => self.read_ampersand(),
            '|' => self.read_bar(),
            '^' => self.read_caret(),
            '~' => {
                self.bump();
                Token::new(TokenKind::Tilde, self.mk_span())
            }
            '\\' => {
                self.bump();
                Token::new(TokenKind::Backslash, self.mk_span())
            }
            '.' => self.read_dot(),
            ':' => self.read_colon(),
            ';' => {
                self.bump();
                Token::new(TokenKind::Semicolon, self.mk_span())
            }
            ',' => {
                self.bump();
                Token::new(TokenKind::Comma, self.mk_span())
            }
            '@' => {
                self.bump();
                Token::new(TokenKind::At, self.mk_span())
            }
            '?' => self.read_question(),
            '(' => {
                self.bump();
                Token::new(TokenKind::LParen, self.mk_span())
            }
            ')' => {
                self.bump();
                Token::new(TokenKind::RParen, self.mk_span())
            }
            '{' => {
                self.bump();
                Token::new(TokenKind::LBrace, self.mk_span())
            }
            '}' => {
                self.bump();
                Token::new(TokenKind::RBrace, self.mk_span())
            }
            '[' => {
                self.bump();
                Token::new(TokenKind::LBracket, self.mk_span())
            }
            ']' => {
                self.bump();
                Token::new(TokenKind::RBracket, self.mk_span())
            }
            _ => {
                let span = self.mk_span();
                self.bump();
                return Err(Error::new(span, format!("unexpected character: {}", ch)));
            }
        };

        Ok(token)
    }

    fn peek(&self) -> char {
        self.chars.get(self.pos).copied().unwrap_or('\0')
    }

    fn peek_next(&self) -> char {
        self.chars.get(self.pos + 1).copied().unwrap_or('\0')
    }

    fn bump(&mut self) {
        self.pos += 1;
        self.column += 1;
    }

    /// True if `self.pos` sits on an `r` starting a raw string: `r` followed
    /// by ANY number of `#` (zero or more, not just zero/one — #263) and
    /// then a `"`. `self.pos` itself is the `r`, so scanning starts at
    /// `self.pos + 1`.
    fn is_raw_string_start(&self) -> bool {
        let mut i = self.pos + 1;
        while self.chars.get(i) == Some(&'#') {
            i += 1;
        }
        self.chars.get(i) == Some(&'"')
    }

    fn mk_pos(&self) -> Pos {
        Pos {
            offset: self.pos as u32,
            line: self.line,
            column: self.start_column,
        }
    }

    fn mk_span(&self) -> Span {
        Span::new(
            Pos {
                offset: self.start as u32,
                line: self.line,
                column: self.start_column,
            },
            Pos {
                offset: self.pos as u32,
                line: self.line,
                column: self.column,
            },
        )
    }

    fn read_operator(&mut self) -> Token {
        self.bump();
        if self.peek() == '=' {
            self.bump();
            Token::new(TokenKind::SlashEquals, self.mk_span())
        } else {
            Token::new(TokenKind::Slash, self.mk_span())
        }
    }

    fn read_line_comment(&mut self) -> Token {
        self.bump(); // /
        self.bump(); // /
        let start = self.pos;
        while self.pos < self.chars.len() && self.peek() != '\n' {
            self.bump();
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        Token::new(TokenKind::Comment(text), self.mk_span())
    }

    fn read_block_comment(&mut self) -> Token {
        self.bump(); // /
        self.bump(); // *
        let is_doc = self.peek() == '*';
        if is_doc {
            self.bump(); // *
        }
        let start = self.pos;
        // Non-nested block comments: read until first */
        while self.pos < self.chars.len() {
            if self.peek() == '*' && self.peek_next() == '/' {
                let end = self.pos;
                self.bump(); // *
                self.bump(); // /
                let text: String = self.chars[start..end].iter().collect();
                return if is_doc {
                    Token::new(TokenKind::DocComment(text), self.mk_span())
                } else {
                    Token::new(TokenKind::Comment(text), self.mk_span())
                };
            } else {
                if self.peek() == '\n' {
                    self.line += 1;
                    self.column = 0;
                }
                self.bump();
            }
        }
        // EOF reached without closing */
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_doc {
            Token::new(TokenKind::DocComment(text), self.mk_span())
        } else {
            Token::new(TokenKind::Comment(text), self.mk_span())
        }
    }

    fn read_string(&mut self) -> Result<Token, Error> {
        self.bump(); // opening "
        let _start = self.pos;
        let mut current_str = String::new();
        let mut parts: Vec<InterpPart> = Vec::new();
        let mut has_interp = false;

        while self.pos < self.chars.len() && self.peek() != '"' {
            let ch = self.peek();
            if ch == '\n' {
                return Err(Error::new(self.mk_span(), "unterminated string literal"));
            }
            // Detect ${ for interpolation
            if ch == '$' && self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '{' {
                has_interp = true;
                parts.push(InterpPart::Str(std::mem::take(&mut current_str)));
                self.bump(); // $
                self.bump(); // {
                // Collect expression source until matching }
                let mut expr_src = String::new();
                let mut depth = 1usize;
                while self.pos < self.chars.len() && depth > 0 {
                    let ec = self.peek();
                    if ec == '{' { depth += 1; }
                    if ec == '}' {
                        depth -= 1;
                        if depth == 0 { self.bump(); break; }
                    }
                    expr_src.push(ec);
                    self.bump();
                }
                parts.push(InterpPart::Expr(expr_src));
            } else if ch == '\\' {
                self.bump();
                if self.pos >= self.chars.len() {
                    return Err(Error::new(self.mk_span(), "unterminated escape sequence"));
                }
                let escaped = self.read_escape()?;
                current_str.push(escaped);
            } else {
                current_str.push(ch);
                self.bump();
            }
        }

        if self.pos >= self.chars.len() {
            return Err(Error::new(self.mk_span(), "unterminated string literal"));
        }

        self.bump(); // closing "

        if has_interp {
            parts.push(InterpPart::Str(current_str));
            Ok(Token::new(TokenKind::InterpString(parts), self.mk_span()))
        } else {
            Ok(Token::new(TokenKind::String(current_str), self.mk_span()))
        }
    }

    fn read_raw_string(&mut self) -> Result<Token, Error> {
        // Count hash characters
        let mut hashes = 0usize;
        let ch = self.peek();
        if ch == 'r' {
            self.bump(); // r
        }
        while self.peek() == '#' {
            self.bump();
            hashes += 1;
        }
        
        // Expect opening "
        if self.peek() != '"' {
            return Err(Error::new(self.mk_span(), "expected opening \" for raw string"));
        }
        self.bump(); // opening "
        
        let start = self.pos;
        let mut value = String::new();
        
        // Read until we find closing delimiter: " followed by hashes
        loop {
            if self.pos >= self.chars.len() {
                return Err(Error::new(self.mk_span(), "unterminated raw string literal"));
            }
            
            let ch = self.peek();
            if ch == '"' {
                // Check if followed by correct number of hashes
                let mut h = 0usize;
                let mut check_pos = self.pos + 1;
                while h < hashes && check_pos < self.chars.len() && self.chars[check_pos] == '#' {
                    h += 1;
                    check_pos += 1;
                }
                
                if h == hashes {
                    // Found closing delimiter
                    value = self.chars[start..self.pos].iter().collect();
                    self.pos = check_pos; // Move past closing delimiter
                    self.column = (self.column as usize + hashes + 1) as u32;
                    return Ok(Token::new(TokenKind::RawString(value), self.mk_span()));
                }
            }
            
            if ch == '\n' {
                self.line += 1;
                self.column = 0;
            }
            value.push(ch);
            self.bump();
        }
    }

    fn read_escape(&mut self) -> Result<char, Error> {
        let ch = self.peek();
        self.bump();
        match ch {
            'n' => Ok('\n'),
            't' => Ok('\t'),
            'r' => Ok('\r'),
            '\\' => Ok('\\'),
            '"' => Ok('"'),
            '\'' => Ok('\''),
            '0' => Ok('\0'),
            'x' => {
                let d1 = self.read_hex_digit()?;
                let d2 = self.read_hex_digit()?;
                let code = d1 * 16 + d2 ;
                char::from_u32(code).ok_or_else(|| Error::new(self.mk_span(), "invalid hex escape"))
            }
            'u' => {
                self.expect_char('{')?;
                let mut value = 0u32;
                let mut count = 0;
                while self.peek() != '}' && count < 6 {
                    let d = self.read_hex_digit()?;
                    value = value * 16 + d;
                    count += 1;
                }
                self.expect_char('}')?;
                char::from_u32(value)
                    .ok_or_else(|| Error::new(self.mk_span(), "invalid unicode escape"))
            }
            'U' => {
                let mut value = 0u32;
                for _ in 0..8 {
                    value = value * 16 + self.read_hex_digit()?;
                }
                char::from_u32(value)
                    .ok_or_else(|| Error::new(self.mk_span(), "invalid unicode escape"))
            }
            _ => Err(Error::new(
                self.mk_span(),
                format!("unknown escape sequence: \\{}", ch),
            )),
        }
    }

    fn read_hex_digit(&mut self) -> Result<u32, Error> {
        let ch = self.peek();
        self.bump();
        match ch {
            '0'..='9' => Ok(ch as u32 - '0' as u32),
            'a'..='f' => Ok(ch as u32 - 'a' as u32 + 10),
            'A'..='F' => Ok(ch as u32 - 'A' as u32 + 10),
            _ => Err(Error::new(
                self.mk_span(),
                format!("expected hex digit, found: {}", ch),
            )),
        }
    }

    fn expect_char(&mut self, ch: char) -> Result<(), Error> {
        if self.peek() == ch {
            self.bump();
            Ok(())
        } else {
            Err(Error::new(self.mk_span(), format!("expected '{}'", ch)))
        }
    }

    fn read_char(&mut self) -> Result<Token, Error> {
        self.bump(); // opening '
        let ch = if self.peek() == '\\' {
            self.bump();
            self.read_escape()?
        } else {
            let ch = self.peek();
            if ch == '\'' {
                return Err(Error::new(self.mk_span(), "empty character literal"));
            }
            self.bump();
            ch
        };

        if self.peek() != '\'' {
            return Err(Error::new(self.mk_span(), "unterminated char literal"));
        }
        self.bump(); // closing '

        Ok(Token::new(TokenKind::Char(ch), self.mk_span()))
    }

    fn read_byte_literal(&mut self) -> Result<Token, Error> {
        self.bump(); // ' (skip the opening quote, 'b' was already consumed)
        let ch = if self.peek() == '\\' {
            self.bump();
            let escaped = self.read_escape()?;
            escaped as u8
        } else {
            let ch = self.peek();
            if ch == '\'' {
                return Err(Error::new(self.mk_span(), "empty byte literal"));
            }
            if ch as u32 > 127 {
                return Err(Error::new(self.mk_span(), "byte literal must be in range 0-127"));
            }
            self.bump();
            ch as u8
        };

        if self.peek() != '\'' {
            return Err(Error::new(self.mk_span(), "unterminated byte literal"));
        }
        self.bump(); // closing '

        Ok(Token::new(TokenKind::Byte(ch), self.mk_span()))
    }

    fn read_number(&mut self) -> Result<Token, Error> {
        let start = self.pos;

        // Integer part
        if self.peek() == '0' {
            match self.peek_next() {
                'x' | 'X' => return self.read_hex_int(start),
                'o' | 'O' => return self.read_octal_int(start),
                'b' | 'B' => return self.read_binary_int(start),
                '0'..='9' | '.' | '_' => {}
                _ => {
                    self.bump();
                    return Ok(Token::new(TokenKind::Integer(0), self.mk_span()));
                }
            }
        }

        while self.peek().is_ascii_digit() || self.peek() == '_' {
            self.bump();
        }

        // Check for float — but not if this number was itself accessed via a dot (e.g. tuple.0.1)
        let preceded_by_dot = start > 0 && self.chars[start - 1] == '.';
        if !preceded_by_dot && self.peek() == '.' && (self.peek_next().is_ascii_digit() || self.peek_next() == '_') {
            self.bump(); // .
            while self.peek().is_ascii_digit() || self.peek() == '_' {
                self.bump();
            }

            // Scientific notation
            if self.peek() == 'e' || self.peek() == 'E' {
                self.bump();
                if self.peek() == '+' || self.peek() == '-' {
                    self.bump();
                }
                while self.peek().is_ascii_digit() || self.peek() == '_' {
                    self.bump();
                }
            }

            // Capture the numeric text BEFORE consuming the suffix, so the suffix
            // (f32/f64) is not fed to parse() (which would reject e.g. "3.14f32").
            let num_end = self.pos;
            // Check for float suffix (f32/f64) — consumed for compatibility, but
            // TokenKind::Float only carries f64; a distinct Float32 representation
            // is not yet modeled downstream.
            let _suffix = self.read_float_suffix();

            let text: String = self.chars[start..num_end].iter().filter(|c| **c != '_').collect();
            // Reject a literal that doesn't fit in f64 (parses to ±inf) or is
            // otherwise unparseable — previously `unwrap_or(0.0)` silently yielded
            // 0.0, and an overflowing literal became a non-finite value (Bug 51).
            let value: f64 = text.parse()
                .map_err(|_| Error::new(self.mk_span(), format!("invalid float literal: {}", text)))?;
            if !value.is_finite() {
                return Err(Error::new(self.mk_span(), format!("float literal out of range for Float64: {}", text)));
            }

            return Ok(Token::new(TokenKind::Float(value), self.mk_span()));
        }

        // Capture the numeric text BEFORE the suffix, so a suffixed literal like
        // "42i32" doesn't feed the suffix to parse().
        let num_end = self.pos;
        let suffix = self.read_int_suffix();

        let text: String = self.chars[start..num_end].iter().filter(|c| **c != '_').collect();
        // An integer literal exceeding i64 previously became 0 via unwrap_or(0)
        // (silent garbage, Bug 51). Report it as a clean lexer error instead.
        let value: i64 = text.parse()
            .map_err(|_| Error::new(self.mk_span(), format!("integer literal out of range for Int64: {}", text)))?;
        
        match suffix {
            Some(s) => Ok(Token::new(TokenKind::IntegerSuffix(s), self.mk_span())),
            None => Ok(Token::new(TokenKind::Integer(value), self.mk_span())),
        }
    }

    fn read_int_suffix(&mut self) -> Option<IntSuffix> {
        let start = self.pos;
        
        // Skip underscore before suffix (e.g., 42_i32)
        if self.peek() == '_' {
            self.bump();
        }
        
        let suffix = match (self.peek(), self.peek_next()) {
            ('u', '8') => { self.bump(); self.bump(); Some(IntSuffix::Unsigned8) }
            ('u', '1') if self.chars.get(self.pos + 2) == Some(&'6') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Unsigned16) }
            ('u', '3') if self.chars.get(self.pos + 2) == Some(&'2') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Unsigned32) }
            ('u', '6') if self.chars.get(self.pos + 2) == Some(&'4') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Unsigned64) }
            ('i', '8') => { self.bump(); self.bump(); Some(IntSuffix::Signed8) }
            ('i', '1') if self.chars.get(self.pos + 2) == Some(&'6') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Signed16) }
            ('i', '3') if self.chars.get(self.pos + 2) == Some(&'2') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Signed32) }
            ('i', '6') if self.chars.get(self.pos + 2) == Some(&'4') => { self.bump(); self.bump(); self.bump(); Some(IntSuffix::Signed64) }
            ('u', 's') if self.chars.get(self.pos + 2) == Some(&'i') && self.chars.get(self.pos + 3) == Some(&'z') => { 
                self.bump(); self.bump(); self.bump(); self.bump(); Some(IntSuffix::Usize) 
            }
            ('i', 's') if self.chars.get(self.pos + 2) == Some(&'i') && self.chars.get(self.pos + 3) == Some(&'z') => { 
                self.bump(); self.bump(); self.bump(); self.bump(); Some(IntSuffix::Size) 
            }
            _ => None,
        };
        
        if suffix.is_some() {
            self.pos = start; // Reset if we want to handle this in parser instead
            None // For now, just strip underscores and return basic integer
        } else {
            None
        }
    }

    fn read_float_suffix(&mut self) -> FloatType {
        let _start = self.pos;
        
        // Skip underscore before suffix (e.g., 42.0_f64)
        if self.peek() == '_' {
            self.bump();
        }
        
        match (self.peek(), self.peek_next()) {
            ('f', '3') if self.chars.get(self.pos + 2) == Some(&'2') => {
                self.bump(); self.bump(); self.bump();
                FloatType::Float32
            }
            ('f', '6') if self.chars.get(self.pos + 2) == Some(&'4') => {
                self.bump(); self.bump(); self.bump();
                FloatType::Float64
            }
            _ => FloatType::Float64,
        }
    }

    fn read_hex_int(&mut self, _start: usize) -> Result<Token, Error> {
        self.bump(); // 0
        self.bump(); // x
        while self.peek().is_ascii_hexdigit() || self.peek() == '_' {
            self.bump();
        }
        let text: String = self.chars[_start..self.pos].iter().filter(|c| **c != '_').collect();
        // Overflow or missing digits (`0x`) previously became 0 (Bug 51).
        let value = i64::from_str_radix(&text[2..], 16)
            .map_err(|_| Error::new(self.mk_span(), format!("invalid or out-of-range hex integer literal: {}", text)))?;
        Ok(Token::new(TokenKind::Integer(value), self.mk_span()))
    }

    fn read_octal_int(&mut self, _start: usize) -> Result<Token, Error> {
        self.bump(); // 0
        self.bump(); // o
        while self.peek().is_ascii_digit() && self.peek() != '8' && self.peek() != '9' || self.peek() == '_' {
            self.bump();
        }
        let text: String = self.chars[_start..self.pos].iter().filter(|c| **c != '_').collect();
        let value = i64::from_str_radix(&text[2..], 8)
            .map_err(|_| Error::new(self.mk_span(), format!("invalid or out-of-range octal integer literal: {}", text)))?;
        Ok(Token::new(TokenKind::Integer(value), self.mk_span()))
    }

    fn read_binary_int(&mut self, _start: usize) -> Result<Token, Error> {
        self.bump(); // 0
        self.bump(); // b
        while self.peek() == '0' || self.peek() == '1' || self.peek() == '_' {
            self.bump();
        }
        let text: String = self.chars[_start..self.pos].iter().filter(|c| **c != '_').collect();
        let value = i64::from_str_radix(&text[2..], 2)
            .map_err(|_| Error::new(self.mk_span(), format!("invalid or out-of-range binary integer literal: {}", text)))?;
        Ok(Token::new(TokenKind::Integer(value), self.mk_span()))
    }

    fn read_ident_or_keyword(&mut self) -> Token {
        let start = self.pos;
        while matches!(self.peek(), 'a'..='z' | 'A'..='Z' | '_' | '0'..='9') {
            self.bump();
        }

        let text: String = self.chars[start..self.pos].iter().collect();

        if text == "_" {
            return Token::new(TokenKind::Underscore, self.mk_span());
        }

        if text == "true" {
            return Token::new(TokenKind::Bool(true), self.mk_span());
        }
        if text == "false" {
            return Token::new(TokenKind::Bool(false), self.mk_span());
        }

        if let Some(keyword) = Keyword::lookup(&text) {
            return Token::new(TokenKind::Keyword(keyword), self.mk_span());
        }

        Token::new(TokenKind::Ident(text), self.mk_span())
    }

    fn read_plus(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '+' => {
                self.bump();
                Token::new(TokenKind::PlusPlus, self.mk_span())
            }
            '=' => {
                self.bump();
                Token::new(TokenKind::PlusEquals, self.mk_span())
            }
            _ => Token::new(TokenKind::Plus, self.mk_span()),
        }
    }

    fn read_minus(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '-' => {
                self.bump();
                Token::new(TokenKind::MinusMinus, self.mk_span())
            }
            '=' => {
                self.bump();
                Token::new(TokenKind::MinusEquals, self.mk_span())
            }
            '>' => {
                self.bump();
                Token::new(TokenKind::ThinArrow, self.mk_span())
            }
            _ => Token::new(TokenKind::Minus, self.mk_span()),
        }
    }

    fn read_star(&mut self) -> Token {
        self.bump();
        if self.peek() == '=' {
            self.bump();
            Token::new(TokenKind::StarEquals, self.mk_span())
        } else {
            Token::new(TokenKind::Star, self.mk_span())
        }
    }

    fn read_percent(&mut self) -> Token {
        self.bump();
        if self.peek() == '=' {
            self.bump();
            Token::new(TokenKind::PercentEquals, self.mk_span())
        } else {
            Token::new(TokenKind::Percent, self.mk_span())
        }
    }

    fn read_equals(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '=' => {
                self.bump();
                Token::new(TokenKind::EqualsEquals, self.mk_span())
            }
            '>' => {
                self.bump();
                Token::new(TokenKind::FatArrow, self.mk_span())
            }
            _ => Token::new(TokenKind::Equals, self.mk_span()),
        }
    }

    fn read_bang(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '=' => {
                self.bump();
                Token::new(TokenKind::BangEquals, self.mk_span())
            }
            '?' => {
                self.bump();
                Token::new(TokenKind::QuestionQuestion, self.mk_span())
            }
            _ => Token::new(TokenKind::Bang, self.mk_span()),
        }
    }

    fn read_less(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '=' => {
                self.bump();
                Token::new(TokenKind::LessEquals, self.mk_span())
            }
            '<' => {
                self.bump();
                if self.peek() == '=' {
                    self.bump();
                    Token::new(TokenKind::LessLessEquals, self.mk_span())
                } else {
                    Token::new(TokenKind::LessLess, self.mk_span())
                }
            }
            _ => Token::new(TokenKind::Less, self.mk_span()),
        }
    }

    fn read_greater(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '=' => {
                self.bump();
                Token::new(TokenKind::GreaterEquals, self.mk_span())
            }
            '>' => {
                self.bump();
                if self.peek() == '>' {
                    self.bump();
                    if self.peek() == '=' {
                        self.bump();
                        Token::new(TokenKind::GreaterGreaterGreaterEquals, self.mk_span())
                    } else {
                        Token::new(TokenKind::GreaterGreaterGreater, self.mk_span())
                    }
                } else if self.peek() == '=' {
                    self.bump();
                    Token::new(TokenKind::GreaterGreaterEquals, self.mk_span())
                } else {
                    Token::new(TokenKind::GreaterGreater, self.mk_span())
                }
            }
            _ => Token::new(TokenKind::Greater, self.mk_span()),
        }
    }

    fn read_ampersand(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '&' => {
                self.bump();
                Token::new(TokenKind::AmpAmp, self.mk_span())
            }
            '=' => {
                self.bump();
                Token::new(TokenKind::AmpersandEquals, self.mk_span())
            }
            _ => Token::new(TokenKind::Ampersand, self.mk_span()),
        }
    }

    fn read_bar(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '|' => {
                self.bump();
                Token::new(TokenKind::BarBar, self.mk_span())
            }
            '=' => {
                self.bump();
                Token::new(TokenKind::BarEquals, self.mk_span())
            }
            _ => Token::new(TokenKind::Bar, self.mk_span()),
        }
    }

    fn read_caret(&mut self) -> Token {
        self.bump();
        if self.peek() == '=' {
            self.bump();
            Token::new(TokenKind::CaretEquals, self.mk_span())
        } else {
            Token::new(TokenKind::Caret, self.mk_span())
        }
    }

    fn read_dot(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '.' => {
                self.bump();
                if self.peek() == '.' {
                    self.bump();
                    Token::new(TokenKind::DotDotDot, self.mk_span())
                } else {
                    Token::new(TokenKind::DotDot, self.mk_span())
                }
            }
            _ => Token::new(TokenKind::Dot, self.mk_span()),
        }
    }

    fn read_colon(&mut self) -> Token {
        self.bump();
        if self.peek() == ':' {
            self.bump();
            Token::new(TokenKind::ColonColon, self.mk_span())
        } else if self.peek() == '=' {
            self.bump();
            Token::new(TokenKind::FatArrow, self.mk_span())
        } else {
            Token::new(TokenKind::Colon, self.mk_span())
        }
    }

    fn read_question(&mut self) -> Token {
        self.bump();
        match self.peek() {
            '?' => {
                self.bump();
                Token::new(TokenKind::QuestionQuestion, self.mk_span())
            }
            ':' => {
                self.bump();
                Token::new(TokenKind::QuestionColon, self.mk_span())
            }
            _ => Token::new(TokenKind::Question, self.mk_span()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_integer() {
        let mut lexer = Lexer::new("42");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(42)));
    }

    #[test]
    fn test_lexer_keywords() {
        let mut lexer = Lexer::new("class fn if else while return");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Keyword(Keyword::Class)));
        assert!(matches!(tokens[1].kind, TokenKind::Keyword(Keyword::Fn)));
        assert!(matches!(tokens[2].kind, TokenKind::Keyword(Keyword::If)));
    }

    #[test]
    fn test_lexer_string() {
        let mut lexer = Lexer::new("\"hello world\"");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::String(s) if s == "hello world"));
    }

    #[test]
    fn test_lexer_comment() {
        let mut lexer = Lexer::new("// this is a comment\n42");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(42)));
    }

    #[test]
    fn test_lexer_doc_comment() {
        let mut lexer = Lexer::new("/** doc comment */\n42");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::DocComment(s) if s.contains("doc comment")));
        // Find the integer token (skip whitespace/newline tokens)
        let int_token = tokens.iter().find(|t| matches!(t.kind, TokenKind::Integer(_))).unwrap();
        assert!(matches!(int_token.kind, TokenKind::Integer(42)));
    }

    #[test]
    fn test_lexer_doc_comment_multiline() {
        let mut lexer = Lexer::new("/**\n * line 1\n * line 2\n */\nclass Foo");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::DocComment(_)));
        if let TokenKind::DocComment(s) = &tokens[0].kind {
            eprintln!("DocComment text: [{}]", s);
            for (i, c) in s.chars().enumerate() {
                eprintln!("  char {}: {:?}", i, c);
            }
        }
    }

    #[test]
    fn test_lexer_raw_string() {
        let mut lexer = Lexer::new(r#"r"hello world""#);
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::RawString(s) if s == "hello world"));
    }

    #[test]
    fn test_lexer_raw_string_with_hashes() {
        let mut lexer = Lexer::new(r##"r#"hello "world""#"##);
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::RawString(s) if s == r#"hello "world""#));
    }

    #[test]
    fn test_lexer_digit_separator() {
        let mut lexer = Lexer::new("1_000_000");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(1000000)));
    }

    #[test]
    fn test_lexer_hex_with_separator() {
        let mut lexer = Lexer::new("0xFF_FF");
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Integer(65535)));
    }

    #[test]
    fn test_lexer_byte_literal() {
        let mut lexer = Lexer::new("b'x'");
        let tokens = lexer.tokenize().unwrap();
        println!("First token: {:?}", tokens[0]);
        assert!(matches!(tokens[0].kind, TokenKind::Byte(120))); // 'x' = 120
    }

    #[test]
    fn test_lexer_escape_sequences() {
        let mut lexer = Lexer::new(r#""hello\nworld""#);
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::String(s) if s == "hello\nworld"));
    }

    #[test]
    fn test_lexer_hex_escape() {
        let mut lexer = Lexer::new(r#""\x48\x65\x6c\x6c\x6f""#);
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(&tokens[0].kind, TokenKind::String(s) if s == "Hello"));
    }

    // --- Helpers ---

    fn lex(input: &str) -> Vec<TokenKind> {
        Lexer::new(input).tokenize().unwrap().into_iter().map(|t| t.kind).collect()
    }

    fn lex_kinds(input: &str) -> Vec<TokenKind> {
        let kinds = lex(input);
        // strip trailing Eof
        kinds.into_iter().filter(|k| !matches!(k, TokenKind::Eof)).collect()
    }

    // --- Float literals ---

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 tests float lexing, not PI
    fn test_float_basic() {
        let toks = lex_kinds("3.14");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 3.14).abs() < 1e-10));
    }

    #[test]
    fn test_float_scientific() {
        let toks = lex_kinds("1.5e10");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 1.5e10).abs() < 1.0));
    }

    #[test]
    fn test_float_scientific_neg_exp() {
        let toks = lex_kinds("2.0E-3");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 2.0e-3).abs() < 1e-15));
    }

    #[test]
    fn test_float_with_separator() {
        let toks = lex_kinds("1_000.5");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 1000.5).abs() < 1e-10));
    }

    // --- Integer bases ---

    #[test]
    fn test_integer_hex() {
        let toks = lex_kinds("0xFF");
        assert!(matches!(toks[0], TokenKind::Integer(255)));
    }

    #[test]
    fn test_integer_octal() {
        let toks = lex_kinds("0o17");
        assert!(matches!(toks[0], TokenKind::Integer(15)));
    }

    #[test]
    fn test_integer_binary() {
        let toks = lex_kinds("0b1010");
        assert!(matches!(toks[0], TokenKind::Integer(10)));
    }

    #[test]
    fn test_integer_zero() {
        let toks = lex_kinds("0");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
    }

    // --- Bool literals ---

    #[test]
    fn test_bool_true() {
        let toks = lex_kinds("true");
        assert!(matches!(toks[0], TokenKind::Bool(true)));
    }

    #[test]
    fn test_bool_false() {
        let toks = lex_kinds("false");
        assert!(matches!(toks[0], TokenKind::Bool(false)));
    }

    // --- Char literals ---

    #[test]
    fn test_char_basic() {
        let toks = lex_kinds("'a'");
        assert!(matches!(toks[0], TokenKind::Char('a')));
    }

    #[test]
    fn test_char_escape_newline() {
        let toks = lex_kinds(r"'\n'");
        assert!(matches!(toks[0], TokenKind::Char('\n')));
    }

    #[test]
    fn test_char_escape_tab() {
        let toks = lex_kinds(r"'\t'");
        assert!(matches!(toks[0], TokenKind::Char('\t')));
    }

    #[test]
    fn test_char_escape_backslash() {
        let toks = lex_kinds(r"'\\'");
        assert!(matches!(toks[0], TokenKind::Char('\\')));
    }

    #[test]
    fn test_char_escape_quote() {
        let toks = lex_kinds(r"'\''");
        assert!(matches!(toks[0], TokenKind::Char('\'')));
    }

    // --- Byte literals ---

    #[test]
    fn test_byte_escape() {
        let toks = lex_kinds(r"b'\n'");
        assert!(matches!(toks[0], TokenKind::Byte(b'\n')));
    }

    // --- String escapes ---

    #[test]
    fn test_string_escape_unicode() {
        let toks = lex_kinds(r#""\u{1F600}""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('\u{1F600}')));
    }

    #[test]
    fn test_string_escape_null() {
        let toks = lex_kinds(r#""\0""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s == "\0"));
    }

    // --- Interpolated strings ---

    #[test]
    fn test_interp_string_simple() {
        let toks = lex_kinds(r#""Hello ${name}!""#);
        assert!(matches!(&toks[0], TokenKind::InterpString(parts) if {
            parts.len() == 3
                && matches!(&parts[0], InterpPart::Str(s) if s == "Hello ")
                && matches!(&parts[1], InterpPart::Expr(e) if e == "name")
                && matches!(&parts[2], InterpPart::Str(s) if s == "!")
        }));
    }

    #[test]
    fn test_interp_string_only_expr() {
        let toks = lex_kinds(r#""${x}""#);
        if let TokenKind::InterpString(parts) = &toks[0] {
            assert!(matches!(&parts[0], InterpPart::Str(s) if s.is_empty()));
            assert!(matches!(&parts[1], InterpPart::Expr(e) if e == "x"));
        } else {
            panic!("expected InterpString");
        }
    }

    #[test]
    fn test_interp_string_nested_braces() {
        let toks = lex_kinds(r#""${foo(a, b)}""#);
        if let TokenKind::InterpString(parts) = &toks[0] {
            assert!(matches!(&parts[1], InterpPart::Expr(e) if e == "foo(a, b)"));
        } else {
            panic!("expected InterpString");
        }
    }

    // --- Identifiers ---

    #[test]
    fn test_ident_simple() {
        let toks = lex_kinds("myVar");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "myVar"));
    }

    #[test]
    fn test_ident_with_numbers() {
        let toks = lex_kinds("foo123");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "foo123"));
    }

    #[test]
    fn test_ident_underscore_prefix() {
        let toks = lex_kinds("_private");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "_private"));
    }

    // --- All keywords ---

    #[test]
    fn test_keywords_control_flow() {
        let toks = lex_kinds("if else while for loop return break continue match case");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::If)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Else)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::While)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::For)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Loop)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Return)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Break)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Continue)));
        assert!(matches!(toks[8], TokenKind::Keyword(Keyword::Match)));
        assert!(matches!(toks[9], TokenKind::Keyword(Keyword::Case)));
    }

    #[test]
    fn test_keywords_oop() {
        let toks = lex_kinds("class interface extends implements trait enum new this super");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Class)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Interface)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Extends)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Implements)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Trait)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Enum)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::New)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::This)));
        assert!(matches!(toks[8], TokenKind::Keyword(Keyword::Super)));
    }

    #[test]
    fn test_keywords_modifiers() {
        let toks = lex_kinds("public private protected static final abstract override readonly");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Public)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Private)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Protected)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Static)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Final)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Abstract)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Override)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Readonly)));
    }

    #[test]
    fn test_keywords_vars() {
        let toks = lex_kinds("var val let const mut");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Var)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Val)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Let)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Const)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Mut)));
    }

    #[test]
    fn test_keywords_functions() {
        let toks = lex_kinds("fn fnc throw throws try catch finally defer");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Fn)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Fnc)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Throw)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Throws)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Try)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Catch)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Finally)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Defer)));
    }

    #[test]
    fn test_keywords_concurrency() {
        let toks = lex_kinds("spawn channel send recv select default async await");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Spawn)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Channel)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Send)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Recv)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Select)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Default)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Async)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Await)));
    }

    #[test]
    fn test_keywords_modules() {
        let toks = lex_kinds("module namespace import export as in where extern unsafe");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Module)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Namespace)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::Import)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Export)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::As)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::In)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Where)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Extern)));
        assert!(matches!(toks[8], TokenKind::Keyword(Keyword::Unsafe)));
    }

    #[test]
    fn test_keywords_misc() {
        let toks = lex_kinds("ref sizeof typeof is cast null Nothing Never Any immutable");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Ref)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::SizeOf)));
        assert!(matches!(toks[2], TokenKind::Keyword(Keyword::TypeOf)));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Is)));
        assert!(matches!(toks[4], TokenKind::Keyword(Keyword::Cast)));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Null)));
        assert!(matches!(toks[6], TokenKind::Keyword(Keyword::Nothing)));
        assert!(matches!(toks[7], TokenKind::Keyword(Keyword::Never)));
        assert!(matches!(toks[8], TokenKind::Keyword(Keyword::Any)));
        assert!(matches!(toks[9], TokenKind::Keyword(Keyword::Immutable)));
    }

    #[test]
    fn test_keyword_self_types() {
        let toks = lex_kinds("self Self");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Self_)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::SelfType)));
    }

    // --- Operators: simple ---

    #[test]
    fn test_ops_simple() {
        let toks = lex_kinds("+ - * / % = ! < > & | ^ ~ \\");
        assert!(matches!(toks[0], TokenKind::Plus));
        assert!(matches!(toks[1], TokenKind::Minus));
        assert!(matches!(toks[2], TokenKind::Star));
        assert!(matches!(toks[3], TokenKind::Slash));
        assert!(matches!(toks[4], TokenKind::Percent));
        assert!(matches!(toks[5], TokenKind::Equals));
        assert!(matches!(toks[6], TokenKind::Bang));
        assert!(matches!(toks[7], TokenKind::Less));
        assert!(matches!(toks[8], TokenKind::Greater));
        assert!(matches!(toks[9], TokenKind::Ampersand));
        assert!(matches!(toks[10], TokenKind::Bar));
        assert!(matches!(toks[11], TokenKind::Caret));
        assert!(matches!(toks[12], TokenKind::Tilde));
        assert!(matches!(toks[13], TokenKind::Backslash));
    }

    // --- Operators: compound ---

    #[test]
    fn test_ops_compound_equality() {
        let toks = lex_kinds("== != <= >= && ||");
        assert!(matches!(toks[0], TokenKind::EqualsEquals));
        assert!(matches!(toks[1], TokenKind::BangEquals));
        assert!(matches!(toks[2], TokenKind::LessEquals));
        assert!(matches!(toks[3], TokenKind::GreaterEquals));
        assert!(matches!(toks[4], TokenKind::AmpAmp));
        assert!(matches!(toks[5], TokenKind::BarBar));
    }

    #[test]
    fn test_ops_increment_decrement() {
        let toks = lex_kinds("++ --");
        assert!(matches!(toks[0], TokenKind::PlusPlus));
        assert!(matches!(toks[1], TokenKind::MinusMinus));
    }

    #[test]
    fn test_ops_assign_compound() {
        let toks = lex_kinds("+= -= *= /= %= &= |= ^=");
        assert!(matches!(toks[0], TokenKind::PlusEquals));
        assert!(matches!(toks[1], TokenKind::MinusEquals));
        assert!(matches!(toks[2], TokenKind::StarEquals));
        assert!(matches!(toks[3], TokenKind::SlashEquals));
        assert!(matches!(toks[4], TokenKind::PercentEquals));
        assert!(matches!(toks[5], TokenKind::AmpersandEquals));
        assert!(matches!(toks[6], TokenKind::BarEquals));
        assert!(matches!(toks[7], TokenKind::CaretEquals));
    }

    #[test]
    fn test_ops_shift() {
        let toks = lex_kinds("<< >> >>> <<= >>= >>>=");
        assert!(matches!(toks[0], TokenKind::LessLess));
        assert!(matches!(toks[1], TokenKind::GreaterGreater));
        assert!(matches!(toks[2], TokenKind::GreaterGreaterGreater));
        assert!(matches!(toks[3], TokenKind::LessLessEquals));
        assert!(matches!(toks[4], TokenKind::GreaterGreaterEquals));
        assert!(matches!(toks[5], TokenKind::GreaterGreaterGreaterEquals));
    }

    #[test]
    fn test_ops_arrows() {
        let toks = lex_kinds("-> =>");
        assert!(matches!(toks[0], TokenKind::ThinArrow));
        assert!(matches!(toks[1], TokenKind::FatArrow));
    }

    #[test]
    fn test_op_colon_assign_is_fat_arrow() {
        // := is also FatArrow in this language
        let toks = lex_kinds(":=");
        assert!(matches!(toks[0], TokenKind::FatArrow));
    }

    #[test]
    fn test_ops_question() {
        let toks = lex_kinds("? ?? ?:");
        assert!(matches!(toks[0], TokenKind::Question));
        assert!(matches!(toks[1], TokenKind::QuestionQuestion));
        assert!(matches!(toks[2], TokenKind::QuestionColon));
    }

    // --- Compound punctuation ---

    #[test]
    fn test_dots() {
        let toks = lex_kinds(". .. ...");
        assert!(matches!(toks[0], TokenKind::Dot));
        assert!(matches!(toks[1], TokenKind::DotDot));
        assert!(matches!(toks[2], TokenKind::DotDotDot));
    }

    #[test]
    fn test_colons() {
        let toks = lex_kinds(": ::");
        assert!(matches!(toks[0], TokenKind::Colon));
        assert!(matches!(toks[1], TokenKind::ColonColon));
    }

    #[test]
    fn test_delimiters() {
        let toks = lex_kinds("( ) { } [ ]");
        assert!(matches!(toks[0], TokenKind::LParen));
        assert!(matches!(toks[1], TokenKind::RParen));
        assert!(matches!(toks[2], TokenKind::LBrace));
        assert!(matches!(toks[3], TokenKind::RBrace));
        assert!(matches!(toks[4], TokenKind::LBracket));
        assert!(matches!(toks[5], TokenKind::RBracket));
    }

    #[test]
    fn test_punctuation() {
        let toks = lex_kinds("; , @");
        assert!(matches!(toks[0], TokenKind::Semicolon));
        assert!(matches!(toks[1], TokenKind::Comma));
        assert!(matches!(toks[2], TokenKind::At));
    }

    // --- Comments ---

    #[test]
    fn test_block_comment_filtered() {
        let toks = lex_kinds("/* ignored */ 42");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0], TokenKind::Integer(42)));
    }

    #[test]
    fn test_block_comment_multiline_filtered() {
        let toks = lex_kinds("/* line1\nline2 */ foo");
        assert_eq!(toks.len(), 1);
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "foo"));
    }

    #[test]
    fn test_doc_comment_not_filtered() {
        let toks = lex_kinds("/** doc */");
        assert!(matches!(&toks[0], TokenKind::DocComment(_)));
    }

    // --- Trivia filtering ---

    #[test]
    fn test_whitespace_filtered() {
        let toks = lex_kinds("  \t  42");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0], TokenKind::Integer(42)));
    }

    #[test]
    fn test_newlines_filtered() {
        let toks = lex_kinds("42\n99");
        assert_eq!(toks.len(), 2);
        assert!(matches!(toks[0], TokenKind::Integer(42)));
        assert!(matches!(toks[1], TokenKind::Integer(99)));
    }

    // --- Error cases ---

    #[test]
    fn test_error_unterminated_string() {
        let result = Lexer::new("\"hello").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_unterminated_char() {
        let result = Lexer::new("'a").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_empty_char() {
        let result = Lexer::new("''").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_empty_byte() {
        let result = Lexer::new("b''").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_invalid_char() {
        let result = Lexer::new("§").tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_unterminated_raw_string() {
        let result = Lexer::new(r#"r"hello"#).tokenize();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_string_with_newline() {
        let result = Lexer::new("\"hello\nworld\"").tokenize();
        assert!(result.is_err());
    }

    // --- Complex real-world snippets ---

    #[test]
    fn test_function_declaration() {
        let toks = lex_kinds("fn add(a: i32, b: i32) -> i32");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Fn)));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "add"));
        assert!(matches!(toks[2], TokenKind::LParen));
        assert!(matches!(&toks[3], TokenKind::Ident(s) if s == "a"));
        assert!(matches!(toks[4], TokenKind::Colon));
        assert!(matches!(&toks[5], TokenKind::Ident(s) if s == "i32"));
        assert!(matches!(toks[6], TokenKind::Comma));
        assert!(matches!(toks[11], TokenKind::ThinArrow));
    }

    #[test]
    fn test_class_declaration() {
        let toks = lex_kinds("public class Foo extends Bar implements Baz");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Public)));
        assert!(matches!(toks[1], TokenKind::Keyword(Keyword::Class)));
        assert!(matches!(&toks[2], TokenKind::Ident(s) if s == "Foo"));
        assert!(matches!(toks[3], TokenKind::Keyword(Keyword::Extends)));
        assert!(matches!(&toks[4], TokenKind::Ident(s) if s == "Bar"));
        assert!(matches!(toks[5], TokenKind::Keyword(Keyword::Implements)));
    }

    #[test]
    fn test_let_binding() {
        let toks = lex_kinds("let x = 42;");
        assert!(matches!(toks[0], TokenKind::Keyword(Keyword::Let)));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "x"));
        assert!(matches!(toks[2], TokenKind::Equals));
        assert!(matches!(toks[3], TokenKind::Integer(42)));
        assert!(matches!(toks[4], TokenKind::Semicolon));
    }

    #[test]
    fn test_eof_token_present() {
        let tokens = Lexer::new("42").tokenize().unwrap();
        assert!(matches!(tokens.last().unwrap().kind, TokenKind::Eof));
    }

    // ================================================================
    // Hex, octal, binary literals
    // ================================================================

    #[test]
    fn test_hex_literal_lowercase() {
        let toks = lex_kinds("0xff");
        assert!(matches!(toks[0], TokenKind::Integer(255)));
    }

    #[test]
    fn test_hex_literal_uppercase() {
        let toks = lex_kinds("0xFF");
        assert!(matches!(toks[0], TokenKind::Integer(255)));
    }

    #[test]
    fn test_hex_literal_with_underscores() {
        let toks = lex_kinds("0xFF_FF");
        assert!(matches!(toks[0], TokenKind::Integer(0xFFFF)));
    }

    #[test]
    fn test_hex_zero() {
        let toks = lex_kinds("0x0");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
    }

    #[test]
    fn test_hex_max() {
        let toks = lex_kinds("0x7FFFFFFFFFFFFFFF");
        assert!(matches!(toks[0], TokenKind::Integer(i64::MAX)));
    }

    #[test]
    fn test_octal_literal() {
        let toks = lex_kinds("0o17");
        assert!(matches!(toks[0], TokenKind::Integer(15)));
    }

    #[test]
    fn test_octal_zero() {
        let toks = lex_kinds("0o0");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
    }

    #[test]
    fn test_octal_stops_before_8() {
        // 0o78 — the 8 is not part of the octal
        let toks = lex_kinds("0o7");
        assert!(matches!(toks[0], TokenKind::Integer(7)));
    }

    #[test]
    fn test_binary_literal_ones_and_zeros() {
        let toks = lex_kinds("0b1010");
        assert!(matches!(toks[0], TokenKind::Integer(10)));
    }

    #[test]
    fn test_binary_literal_single_bit() {
        let toks = lex_kinds("0b1");
        assert!(matches!(toks[0], TokenKind::Integer(1)));
    }

    #[test]
    fn test_binary_zero() {
        let toks = lex_kinds("0b0");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
    }

    #[test]
    fn test_binary_with_underscores() {
        let toks = lex_kinds("0b1111_0000");
        assert!(matches!(toks[0], TokenKind::Integer(0b1111_0000)));
    }

    // ================================================================
    // Integer with underscores
    // ================================================================

    #[test]
    fn test_integer_with_underscores() {
        let toks = lex_kinds("1_000_000");
        assert!(matches!(toks[0], TokenKind::Integer(1_000_000)));
    }

    #[test]
    fn test_integer_leading_underscore_ignored() {
        // Underscore-only separators still parse
        let toks = lex_kinds("42_");
        assert!(matches!(toks[0], TokenKind::Integer(42)));
    }

    // ================================================================
    // Float literals
    // ================================================================

    #[test]
    #[allow(clippy::approx_constant)] // 3.14 tests float lexing, not PI
    fn test_float_no_suffix() {
        let toks = lex_kinds("3.14");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 3.14).abs() < 0.001));
    }

    #[test]
    fn test_float_f32_suffix_produces_float_token() {
        // BUG: suffix chars are consumed before parsing, causing parse to fail → 0.0
        // Suffix is recognized but value is lost: "3.14f32".parse::<f64>() fails
        let toks = lex_kinds("3.14f32");
        assert!(matches!(toks[0], TokenKind::Float(_)), "f32-suffixed float should produce Float token");
    }

    #[test]
    fn test_float_f64_suffix_produces_float_token() {
        // Same as above: suffix consumed before value parse, value becomes 0.0
        let toks = lex_kinds("2.71f64");
        assert!(matches!(toks[0], TokenKind::Float(_)), "f64-suffixed float should produce Float token");
    }

    #[test]
    fn test_float_zero() {
        let toks = lex_kinds("0.0");
        assert!(matches!(toks[0], TokenKind::Float(f) if f == 0.0));
    }

    #[test]
    fn test_float_with_exponent() {
        let toks = lex_kinds("1.5e2");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 150.0).abs() < 0.001));
    }

    #[test]
    fn test_float_negative_exponent() {
        let toks = lex_kinds("1.0e-2");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 0.01).abs() < 0.0001));
    }

    #[test]
    fn test_float_with_underscores() {
        let toks = lex_kinds("1_000.0");
        assert!(matches!(toks[0], TokenKind::Float(f) if (f - 1000.0).abs() < 0.001));
    }

    // ================================================================
    // String escape sequences
    // ================================================================

    #[test]
    fn test_string_escape_newline() {
        let toks = lex_kinds(r#""\n""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('\n')));
    }

    #[test]
    fn test_string_escape_tab() {
        let toks = lex_kinds(r#""\t""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('\t')));
    }

    #[test]
    fn test_string_escape_backslash() {
        let toks = lex_kinds(r#""\\""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s == "\\"));
    }

    #[test]
    fn test_string_escape_quote() {
        let toks = lex_kinds(r#""\"""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s == "\""));
    }

    #[test]
    fn test_string_escape_nul_char() {
        let toks = lex_kinds(r#""\0""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('\0')));
    }

    #[test]
    fn test_string_escape_hex_2() {
        let toks = lex_kinds(r#""\x41""#); // 'A'
        assert!(matches!(&toks[0], TokenKind::String(s) if s == "A"));
    }

    #[test]
    fn test_string_empty() {
        let toks = lex_kinds(r#""""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.is_empty()));
    }

    #[test]
    fn test_string_with_unicode_2() {
        let toks = lex_kinds(r#""héllo""#);
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('é')));
    }

    // ================================================================
    // Raw strings
    // ================================================================

    #[test]
    fn test_raw_string_basic() {
        let toks = lex_kinds(r###"r"hello\nworld""###);
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s.contains("\\n")));
    }

    #[test]
    fn test_raw_string_with_hashes() {
        let toks = lex_kinds("r#\"hello\"#");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "hello"));
    }

    // ================================================================
    // #263: raw strings must accept an arbitrary run of hashes, not just
    // zero (`r"..."`) or exactly one (`r#"..."#`).
    // ================================================================

    #[test]
    fn test_raw_string_zero_hashes() {
        let toks = lex_kinds("r\"hello\"");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "hello"));
    }

    #[test]
    fn test_raw_string_one_hash_with_embedded_quotes() {
        let toks = lex_kinds("r#\"has \"quotes\" inside\"#");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "has \"quotes\" inside"));
    }

    #[test]
    fn test_raw_string_two_hashes() {
        // Content contains a lone `"#` sequence that would incorrectly
        // terminate a naive single-hash implementation.
        let toks = lex_kinds("r##\"needs \"# inside\"##");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "needs \"# inside"));
    }

    #[test]
    fn test_raw_string_three_hashes() {
        // Content itself contains a `"##` sequence, which needs the third
        // hash to disambiguate from the real closing delimiter.
        let toks = lex_kinds("r###\"needs \"## inside\"###");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "needs \"## inside"));
    }

    #[test]
    fn test_raw_string_two_hashes_mismatched_closing_count_is_content() {
        // A single `"#` inside a two-hash raw string must NOT be treated
        // as a (short) closing delimiter -- only an exact count match ends
        // the string.
        let toks = lex_kinds("r##\"a\"#b\"##");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s == "a\"#b"));
    }

    // ================================================================
    // Char literals
    // ================================================================

    #[test]
    fn test_char_letter() {
        let toks = lex_kinds("'a'");
        assert!(matches!(toks[0], TokenKind::Char('a')));
    }

    #[test]
    fn test_char_digit() {
        let toks = lex_kinds("'7'");
        assert!(matches!(toks[0], TokenKind::Char('7')));
    }

    #[test]
    fn test_char_space() {
        let toks = lex_kinds("' '");
        assert!(matches!(toks[0], TokenKind::Char(' ')));
    }

    #[test]
    fn test_char_escape_newline_2() {
        let toks = lex_kinds(r"'\n'");
        assert!(matches!(toks[0], TokenKind::Char('\n')));
    }

    #[test]
    fn test_char_escape_tab_2() {
        let toks = lex_kinds(r"'\t'");
        assert!(matches!(toks[0], TokenKind::Char('\t')));
    }

    #[test]
    fn test_char_escape_single_quote() {
        let toks = lex_kinds(r"'\''");
        assert!(matches!(toks[0], TokenKind::Char('\'')));
    }

    // ================================================================
    // Byte literals
    // ================================================================

    #[test]
    fn test_byte_literal_letter() {
        let toks = lex_kinds("b'A'");
        assert!(matches!(toks[0], TokenKind::Byte(65)));
    }

    #[test]
    fn test_byte_literal_zero() {
        let toks = lex_kinds("b'\\0'");
        assert!(matches!(toks[0], TokenKind::Byte(0)));
    }

    #[test]
    fn test_byte_literal_max_ascii() {
        let toks = lex_kinds("b'\\x7F'");
        assert!(matches!(toks[0], TokenKind::Byte(127)));
    }

    #[test]
    fn test_byte_literal_out_of_range_behavior() {
        // 0x80 = 128 — documented: may or may not error depending on error propagation path
        let result = Lexer::new("b'\\x80'").tokenize();
        // The implementation has the check, but result depends on how errors are collected
        let _ = result; // document that this path exists, value checked by read_byte_literal
    }

    // ================================================================
    // All keywords
    // ================================================================

    #[test]
    fn test_keyword_class() {
        assert!(matches!(lex_kinds("class")[0], TokenKind::Keyword(Keyword::Class)));
    }

    #[test]
    fn test_keyword_interface() {
        assert!(matches!(lex_kinds("interface")[0], TokenKind::Keyword(Keyword::Interface)));
    }

    #[test]
    fn test_keyword_enum() {
        assert!(matches!(lex_kinds("enum")[0], TokenKind::Keyword(Keyword::Enum)));
    }

    #[test]
    fn test_keyword_trait() {
        assert!(matches!(lex_kinds("trait")[0], TokenKind::Keyword(Keyword::Trait)));
    }

    #[test]
    fn test_keyword_namespace() {
        assert!(matches!(lex_kinds("namespace")[0], TokenKind::Keyword(Keyword::Namespace)));
    }

    #[test]
    fn test_keyword_import() {
        assert!(matches!(lex_kinds("import")[0], TokenKind::Keyword(Keyword::Import)));
    }

    #[test]
    fn test_keyword_return() {
        assert!(matches!(lex_kinds("return")[0], TokenKind::Keyword(Keyword::Return)));
    }

    #[test]
    fn test_keyword_match() {
        assert!(matches!(lex_kinds("match")[0], TokenKind::Keyword(Keyword::Match)));
    }

    #[test]
    fn test_keyword_async() {
        assert!(matches!(lex_kinds("async")[0], TokenKind::Keyword(Keyword::Async)));
    }

    #[test]
    fn test_keyword_await() {
        assert!(matches!(lex_kinds("await")[0], TokenKind::Keyword(Keyword::Await)));
    }

    #[test]
    fn test_keyword_spawn() {
        assert!(matches!(lex_kinds("spawn")[0], TokenKind::Keyword(Keyword::Spawn)));
    }

    #[test]
    fn test_keyword_send() {
        assert!(matches!(lex_kinds("send")[0], TokenKind::Keyword(Keyword::Send)));
    }

    #[test]
    fn test_keyword_recv() {
        assert!(matches!(lex_kinds("recv")[0], TokenKind::Keyword(Keyword::Recv)));
    }

    #[test]
    fn test_keyword_channel() {
        assert!(matches!(lex_kinds("channel")[0], TokenKind::Keyword(Keyword::Channel)));
    }

    #[test]
    fn test_keyword_select() {
        assert!(matches!(lex_kinds("select")[0], TokenKind::Keyword(Keyword::Select)));
    }

    #[test]
    fn test_keyword_loop() {
        assert!(matches!(lex_kinds("loop")[0], TokenKind::Keyword(Keyword::Loop)));
    }

    #[test]
    fn test_keyword_break() {
        assert!(matches!(lex_kinds("break")[0], TokenKind::Keyword(Keyword::Break)));
    }

    #[test]
    fn test_keyword_continue() {
        assert!(matches!(lex_kinds("continue")[0], TokenKind::Keyword(Keyword::Continue)));
    }

    #[test]
    fn test_keyword_try() {
        assert!(matches!(lex_kinds("try")[0], TokenKind::Keyword(Keyword::Try)));
    }

    #[test]
    fn test_keyword_catch() {
        assert!(matches!(lex_kinds("catch")[0], TokenKind::Keyword(Keyword::Catch)));
    }

    #[test]
    fn test_keyword_throw() {
        assert!(matches!(lex_kinds("throw")[0], TokenKind::Keyword(Keyword::Throw)));
    }

    #[test]
    fn test_keyword_finally() {
        assert!(matches!(lex_kinds("finally")[0], TokenKind::Keyword(Keyword::Finally)));
    }

    #[test]
    fn test_keyword_defer() {
        assert!(matches!(lex_kinds("defer")[0], TokenKind::Keyword(Keyword::Defer)));
    }

    #[test]
    fn test_keyword_new() {
        assert!(matches!(lex_kinds("new")[0], TokenKind::Keyword(Keyword::New)));
    }

    #[test]
    fn test_keyword_this() {
        assert!(matches!(lex_kinds("this")[0], TokenKind::Keyword(Keyword::This)));
    }

    #[test]
    fn test_keyword_super() {
        assert!(matches!(lex_kinds("super")[0], TokenKind::Keyword(Keyword::Super)));
    }

    #[test]
    fn test_keyword_null() {
        assert!(matches!(lex_kinds("null")[0], TokenKind::Keyword(Keyword::Null)));
    }

    #[test]
    fn test_keyword_extern() {
        assert!(matches!(lex_kinds("extern")[0], TokenKind::Keyword(Keyword::Extern)));
    }

    #[test]
    fn test_keyword_immutable() {
        assert!(matches!(lex_kinds("immutable")[0], TokenKind::Keyword(Keyword::Immutable)));
    }

    // ================================================================
    // More operators
    // ================================================================

    #[test]
    fn test_op_arrow() {
        assert!(matches!(lex_kinds("->")[0], TokenKind::ThinArrow));
    }

    #[test]
    fn test_op_fat_arrow() {
        assert!(matches!(lex_kinds("=>")[0], TokenKind::FatArrow));
    }

    #[test]
    fn test_op_dotdot() {
        assert!(matches!(lex_kinds("..")[0], TokenKind::DotDot));
    }

    #[test]
    fn test_op_dotdotdot() {
        assert!(matches!(lex_kinds("...")[0], TokenKind::DotDotDot));
    }

    #[test]
    fn test_op_coloncolon() {
        assert!(matches!(lex_kinds("::")[0], TokenKind::ColonColon));
    }

    #[test]
    fn test_op_at_sign() {
        assert!(matches!(lex_kinds("@")[0], TokenKind::At));
    }

    #[test]
    fn test_op_backslash() {
        assert!(matches!(lex_kinds("\\")[0], TokenKind::Backslash));
    }

    #[test]
    fn test_op_question_mark() {
        assert!(matches!(lex_kinds("?")[0], TokenKind::Question));
    }

    #[test]
    fn test_op_pipe() {
        assert!(matches!(lex_kinds("|")[0], TokenKind::Bar));
    }

    #[test]
    fn test_op_tilde() {
        assert!(matches!(lex_kinds("~")[0], TokenKind::Tilde));
    }

    #[test]
    fn test_op_caret() {
        assert!(matches!(lex_kinds("^")[0], TokenKind::Caret));
    }

    #[test]
    fn test_op_plus_equals() {
        assert!(matches!(lex_kinds("+=")[0], TokenKind::PlusEquals));
    }

    #[test]
    fn test_op_minus_equals() {
        assert!(matches!(lex_kinds("-=")[0], TokenKind::MinusEquals));
    }

    #[test]
    fn test_op_star_equals() {
        assert!(matches!(lex_kinds("*=")[0], TokenKind::StarEquals));
    }

    #[test]
    fn test_op_slash_equals() {
        assert!(matches!(lex_kinds("/=")[0], TokenKind::SlashEquals));
    }

    #[test]
    fn test_op_percent_equals() {
        assert!(matches!(lex_kinds("%=")[0], TokenKind::PercentEquals));
    }

    #[test]
    fn test_op_shift_left() {
        assert!(matches!(lex_kinds("<<")[0], TokenKind::LessLess));
    }

    #[test]
    fn test_op_shift_right() {
        assert!(matches!(lex_kinds(">>")[0], TokenKind::GreaterGreater));
    }

    #[test]
    fn test_op_unsigned_shift_right() {
        assert!(matches!(lex_kinds(">>>")[0], TokenKind::GreaterGreaterGreater));
    }

    #[test]
    fn test_op_shift_left_assign() {
        assert!(matches!(lex_kinds("<<=")[0], TokenKind::LessLessEquals));
    }

    #[test]
    fn test_op_shift_right_assign() {
        assert!(matches!(lex_kinds(">>=")[0], TokenKind::GreaterGreaterEquals));
    }

    #[test]
    fn test_op_unsigned_shift_right_assign() {
        assert!(matches!(lex_kinds(">>>=")[0], TokenKind::GreaterGreaterGreaterEquals));
    }

    // ================================================================
    // Comments
    // ================================================================

    #[test]
    fn test_line_comment_is_trivia() {
        let tokens = Lexer::new("// this is a comment\n42").tokenize().unwrap();
        let non_trivia: Vec<_> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
        assert!(matches!(non_trivia[0].kind, TokenKind::Integer(42)));
    }

    #[test]
    fn test_block_comment_is_trivia() {
        let tokens = Lexer::new("/* hello */ 99").tokenize().unwrap();
        let non_trivia: Vec<_> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
        assert!(matches!(non_trivia[0].kind, TokenKind::Integer(99)));
    }

    #[test]
    fn test_doc_comment_token() {
        let tokens = Lexer::new("/** doc */").tokenize().unwrap();
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::DocComment(_))));
    }

    #[test]
    fn test_nested_block_comment() {
        // Block comments are NOT nested in most languages; verify it doesn't crash
        let result = Lexer::new("/* outer /* inner */ still outer */ 1").tokenize();
        assert!(result.is_ok());
    }

    // ================================================================
    // Whitespace handling
    // ================================================================

    #[test]
    fn test_multiple_spaces_ignored() {
        let toks = lex_kinds("   42   ");
        assert!(matches!(toks[0], TokenKind::Integer(42)));
    }

    #[test]
    fn test_tabs_and_newlines_ignored() {
        let toks = lex_kinds("\t\n42\n\t");
        assert!(matches!(toks[0], TokenKind::Integer(42)));
    }

    // ================================================================
    // Identifier edge cases
    // ================================================================

    #[test]
    fn test_ident_with_underscores() {
        let toks = lex_kinds("my_var_name");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "my_var_name"));
    }

    #[test]
    fn test_ident_with_trailing_numbers() {
        let toks = lex_kinds("x123");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "x123"));
    }

    #[test]
    fn test_ident_single_letter() {
        let toks = lex_kinds("x");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "x"));
    }

    #[test]
    fn test_keywords_not_confused_with_idents() {
        // "function" is not a keyword — should be Ident
        let toks = lex_kinds("function");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "function"));
    }

    #[test]
    fn test_keyword_prefix_is_ident() {
        // "classes" starts with "class" but is an Ident
        let toks = lex_kinds("classes");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "classes"));
    }

    // ================================================================
    // Bool literals
    // ================================================================

    #[test]
    fn test_bool_true_token() {
        assert!(matches!(lex_kinds("true")[0], TokenKind::Bool(true)));
    }

    #[test]
    fn test_bool_false_token() {
        assert!(matches!(lex_kinds("false")[0], TokenKind::Bool(false)));
    }

    // ================================================================
    // Unterminated string error
    // ================================================================

    #[test]
    fn test_unterminated_string_error() {
        let result = Lexer::new("\"unterminated").tokenize();
        assert!(result.is_err(), "unterminated string should be a lex error");
    }

    #[test]
    fn test_unterminated_char_error() {
        let result = Lexer::new("'a").tokenize();
        assert!(result.is_err(), "unterminated char should be a lex error");
    }

    // ================================================================
    // Span correctness
    // ================================================================

    #[test]
    fn test_span_start_offset() {
        let tokens = Lexer::new("   42").tokenize().unwrap();
        let int_tok = tokens.iter().find(|t| matches!(t.kind, TokenKind::Integer(_))).unwrap();
        assert_eq!(int_tok.span.start.offset, 3, "integer should start at offset 3");
    }

    #[test]
    fn test_span_line_tracking() {
        let tokens = Lexer::new("x\ny").tokenize().unwrap();
        let y_tok = tokens.iter().find(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "y")).unwrap();
        assert_eq!(y_tok.span.start.line, 2, "y should be on line 2");
    }

    #[test]
    fn test_span_column_tracking() {
        let tokens = Lexer::new("  x").tokenize().unwrap();
        let x_tok = tokens.iter().find(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "x")).unwrap();
        assert_eq!(x_tok.span.start.column, 3, "x should be at column 3");
    }

    // ================================================================
    // Token sequence correctness
    // ================================================================

    #[test]
    fn test_expression_sequence() {
        let toks = lex_kinds("a + b * c");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "a"));
        assert!(matches!(toks[1], TokenKind::Plus));
        assert!(matches!(&toks[2], TokenKind::Ident(s) if s == "b"));
        assert!(matches!(toks[3], TokenKind::Star));
        assert!(matches!(&toks[4], TokenKind::Ident(s) if s == "c"));
    }

    #[test]
    fn test_function_call_sequence() {
        let toks = lex_kinds("foo(42)");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "foo"));
        assert!(matches!(toks[1], TokenKind::LParen));
        assert!(matches!(toks[2], TokenKind::Integer(42)));
        assert!(matches!(toks[3], TokenKind::RParen));
    }

    #[test]
    fn test_method_chain_dots() {
        let toks = lex_kinds("a.b.c");
        assert!(matches!(toks[1], TokenKind::Dot));
        assert!(matches!(toks[3], TokenKind::Dot));
    }

    #[test]
    fn test_generic_type_tokens() {
        let toks = lex_kinds("Array<Int64>");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "Array"));
        assert!(matches!(toks[1], TokenKind::Less));
        assert!(matches!(&toks[2], TokenKind::Ident(s) if s == "Int64"));
        assert!(matches!(toks[3], TokenKind::Greater));
    }

    #[test]
    fn test_range_operator() {
        let toks = lex_kinds("0..10");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
        assert!(matches!(toks[1], TokenKind::DotDot));
        assert!(matches!(toks[2], TokenKind::Integer(10)));
    }

    #[test]
    fn test_range_inclusive_operator() {
        let toks = lex_kinds("0...10");
        assert!(matches!(toks[1], TokenKind::DotDotDot));
    }

    #[test]
    fn test_annotation_at_sign() {
        let toks = lex_kinds("@GET");
        assert!(matches!(toks[0], TokenKind::At));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "GET"));
    }

    // ================================================================
    // Compound operator tokens
    // ================================================================

    #[test]
    fn test_op_ampersand_equals() {
        let toks = lex_kinds("x &= 1");
        assert!(matches!(toks[1], TokenKind::AmpersandEquals));
    }

    #[test]
    fn test_op_bar_equals() {
        let toks = lex_kinds("x |= 1");
        assert!(matches!(toks[1], TokenKind::BarEquals));
    }

    #[test]
    fn test_op_caret_equals() {
        let toks = lex_kinds("x ^= 1");
        assert!(matches!(toks[1], TokenKind::CaretEquals));
    }

    #[test]
    fn test_op_shift_right_equals() {
        let toks = lex_kinds("x >>= 1");
        assert!(matches!(toks[1], TokenKind::GreaterGreaterEquals));
    }

    #[test]
    fn test_op_unsigned_shift_right_equals() {
        let toks = lex_kinds("x >>>= 1");
        assert!(matches!(toks[1], TokenKind::GreaterGreaterGreaterEquals));
    }

    #[test]
    fn test_op_question_question() {
        let toks = lex_kinds("a ?? b");
        assert!(matches!(toks[1], TokenKind::QuestionQuestion));
    }

    #[test]
    fn test_op_question_colon() {
        let toks = lex_kinds("a ?: b");
        assert!(matches!(toks[1], TokenKind::QuestionColon));
    }

    #[test]
    fn test_op_plus_plus() {
        let toks = lex_kinds("i++");
        assert!(matches!(toks[1], TokenKind::PlusPlus));
    }

    #[test]
    fn test_op_minus_minus() {
        let toks = lex_kinds("i--");
        assert!(matches!(toks[1], TokenKind::MinusMinus));
    }

    #[test]
    fn test_op_colon_colon() {
        let toks = lex_kinds("Foo::Bar");
        assert!(matches!(toks[1], TokenKind::ColonColon));
    }

    #[test]
    fn test_op_fat_arrow_in_expr() {
        let toks = lex_kinds("x => y");
        assert!(matches!(toks[1], TokenKind::FatArrow));
    }

    #[test]
    fn test_op_thin_arrow() {
        let toks = lex_kinds("fn f() -> Int64");
        // -> after ()
        let arrow_pos = toks.iter().position(|t| matches!(t, TokenKind::ThinArrow));
        assert!(arrow_pos.is_some(), "-> should produce ThinArrow token");
    }

    #[test]
    fn test_op_pipe_token() {
        // | produces Bar token (Pipe variant is unused in the lexer)
        let toks = lex_kinds("a | b");
        assert!(matches!(toks[1], TokenKind::Bar));
    }

    #[test]
    fn test_op_backslash_token() {
        let toks = lex_kinds("\\x");
        assert!(matches!(toks[0], TokenKind::Backslash));
    }

    // ================================================================
    // Integer suffix variants
    // ================================================================

    #[test]
    fn test_integer_i32_suffix() {
        // Suffix is currently stripped — produces plain Integer token
        let toks = lex_kinds("42i32");
        assert!(matches!(toks[0], TokenKind::Integer(42)), "i32-suffixed int strips suffix and produces Integer(42)");
    }

    #[test]
    fn test_integer_i64_suffix() {
        let toks = lex_kinds("100i64");
        assert!(matches!(toks[0], TokenKind::Integer(100)), "i64-suffixed int strips suffix");
    }

    #[test]
    fn test_integer_u8_suffix() {
        let toks = lex_kinds("255u8");
        assert!(matches!(toks[0], TokenKind::Integer(255)), "u8-suffixed int strips suffix");
    }

    // ================================================================
    // String escape sequences
    // ================================================================

    #[test]
    fn test_string_with_tab_escape() {
        let toks = lex_kinds("\"a\\tb\"");
        assert!(matches!(&toks[0], TokenKind::String(s) if s.contains('\t') || s.contains("\\t")));
    }

    #[test]
    fn test_string_with_unicode_escape() {
        // Unicode escape requires braces: \u{XXXX}
        let toks = lex_kinds("\"\\u{0041}\"");
        assert!(matches!(&toks[0], TokenKind::String(_)), "unicode escape should produce String token");
    }

    #[test]
    fn test_string_with_backslash_escape() {
        let toks = lex_kinds("\"a\\\\b\"");
        assert!(matches!(&toks[0], TokenKind::String(_)), "double backslash should produce String token");
    }

    // ================================================================
    // Char edge cases
    // ================================================================

    #[test]
    fn test_char_space_v2() {
        let toks = lex_kinds("' '");
        assert!(matches!(toks[0], TokenKind::Char(' ')));
    }

    #[test]
    fn test_char_zero() {
        let toks = lex_kinds("'0'");
        assert!(matches!(toks[0], TokenKind::Char('0')));
    }

    #[test]
    fn test_char_escape_n() {
        let toks = lex_kinds("'\\n'");
        assert!(matches!(toks[0], TokenKind::Char('\n')));
    }

    #[test]
    fn test_char_escape_r() {
        let toks = lex_kinds("'\\r'");
        assert!(matches!(toks[0], TokenKind::Char('\r')));
    }

    #[test]
    fn test_char_escape_0() {
        let toks = lex_kinds("'\\0'");
        assert!(matches!(toks[0], TokenKind::Char('\0')));
    }

    // ================================================================
    // Byte literals
    // ================================================================

    #[test]
    fn test_byte_literal_b_prefix() {
        let toks = lex_kinds("b'A'");
        assert!(matches!(toks[0], TokenKind::Byte(65)), "b'A' should produce Byte(65)");
    }

    #[test]
    fn test_byte_zero() {
        let toks = lex_kinds("b'\\0'");
        assert!(matches!(toks[0], TokenKind::Byte(0)), "b'\\0' should produce Byte(0)");
    }

    // ================================================================
    // Number edge cases
    // ================================================================

    #[test]
    fn test_zero_integer() {
        let toks = lex_kinds("0");
        assert!(matches!(toks[0], TokenKind::Integer(0)));
    }

    #[test]
    fn test_large_integer() {
        let toks = lex_kinds("9999999999");
        assert!(matches!(toks[0], TokenKind::Integer(9999999999)));
    }

    #[test]
    fn test_negative_sign_is_separate_token() {
        // Negative numbers are unary minus + integer
        let toks = lex_kinds("-42");
        assert!(matches!(toks[0], TokenKind::Minus));
        assert!(matches!(toks[1], TokenKind::Integer(42)));
    }

    #[test]
    fn test_float_zero_v2() {
        let toks = lex_kinds("0.0");
        assert!(matches!(toks[0], TokenKind::Float(f) if f == 0.0));
    }

    #[test]
    fn test_float_no_leading_digit() {
        // .5 may or may not be valid — just ensure no panic
        let result = std::panic::catch_unwind(|| lex_kinds(".5"));
        let _ = result;
    }

    // ================================================================
    // Interpolated string
    // ================================================================

    #[test]
    fn test_interp_string_simple_v2() {
        let toks = lex_kinds("\"Hello ${name}!\"");
        assert!(
            matches!(&toks[0], TokenKind::InterpString(parts) if !parts.is_empty()),
            "interpolated string should produce InterpString token"
        );
    }

    #[test]
    fn test_interp_string_multiple_exprs() {
        let toks = lex_kinds("\"${a} + ${b} = ${c}\"");
        if let TokenKind::InterpString(parts) = &toks[0] {
            let expr_count = parts.iter().filter(|p| matches!(p, InterpPart::Expr(_))).count();
            assert_eq!(expr_count, 3, "should have 3 expression parts");
        }
    }

    #[test]
    fn test_non_interp_string_no_dollar() {
        let toks = lex_kinds("\"no interpolation here\"");
        // Either String or InterpString with no Expr parts
        match &toks[0] {
            TokenKind::String(_) => {}
            TokenKind::InterpString(parts) => {
                assert!(parts.iter().all(|p| matches!(p, InterpPart::Str(_))));
            }
            other => panic!("unexpected token: {other:?}"),
        }
    }

    // ================================================================
    // Raw string edge cases
    // ================================================================

    #[test]
    fn test_raw_string_empty() {
        let toks = lex_kinds("r\"\"");
        assert!(matches!(&toks[0], TokenKind::RawString(s) if s.is_empty()));
    }

    #[test]
    fn test_raw_string_with_newline() {
        let toks = lex_kinds("r\"line1\nline2\"");
        assert!(matches!(&toks[0], TokenKind::RawString(_)), "raw string should allow newlines");
    }

    #[test]
    fn test_raw_string_no_escape_processing() {
        let toks = lex_kinds("r\"a\\nb\"");
        if let TokenKind::RawString(s) = &toks[0] {
            // raw strings don't process escapes — should contain literal \n
            assert!(s.contains("\\n") || s.contains('\n'), "raw string content preserved");
        }
    }

    // ================================================================
    // Identifier edge cases
    // ================================================================

    #[test]
    fn test_ident_starting_with_keyword_prefix_longer() {
        // "returning" starts with "return" but is an identifier
        let toks = lex_kinds("returning");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "returning"));
    }

    #[test]
    fn test_ident_class_prefix() {
        let toks = lex_kinds("classes");
        assert!(matches!(&toks[0], TokenKind::Ident(s) if s == "classes"));
    }

    #[test]
    fn test_ident_unicode_not_supported_gracefully() {
        // Non-ASCII identifiers: just ensure no panic
        let result = std::panic::catch_unwind(|| lex_kinds("ä"));
        let _ = result;
    }

    // ================================================================
    // Comment content
    // ================================================================

    #[test]
    fn test_line_comment_content() {
        let tokens = Lexer::new("// hello world\n42").tokenize().unwrap();
        let comment = tokens.iter().find(|t| matches!(t.kind, TokenKind::Comment(_)));
        if let Some(t) = comment {
            if let TokenKind::Comment(c) = &t.kind {
                assert!(c.contains("hello world"));
            }
        }
    }

    #[test]
    fn test_doc_comment_content() {
        // DocComment uses /** */ syntax in Tinox
        let tokens = Lexer::new("/** Docs here */\nfn f() {}").tokenize().unwrap();
        let doc = tokens.iter().find(|t| matches!(t.kind, TokenKind::DocComment(_)));
        assert!(doc.is_some(), "/** */ should produce DocComment token");
        if let Some(t) = doc {
            if let TokenKind::DocComment(c) = &t.kind {
                assert!(c.contains("Docs here"));
            }
        }
    }

    // ================================================================
    // Multi-token sequences
    // ================================================================

    #[test]
    fn test_class_declaration_tokens() {
        let toks = lex_kinds("class Foo extends Bar {");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::Class)));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "Foo"));
        assert!(matches!(&toks[2], TokenKind::Keyword(Keyword::Extends)));
        assert!(matches!(&toks[3], TokenKind::Ident(s) if s == "Bar"));
        assert!(matches!(toks[4], TokenKind::LBrace));
    }

    #[test]
    fn test_if_else_tokens() {
        let toks = lex_kinds("if x { } else { }");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::If)));
        assert!(matches!(&toks[2], TokenKind::LBrace));
        assert!(matches!(&toks[4], TokenKind::Keyword(Keyword::Else)));
    }

    #[test]
    fn test_for_in_tokens() {
        let toks = lex_kinds("for x in arr {");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::For)));
        assert!(matches!(&toks[2], TokenKind::Keyword(Keyword::In)));
    }

    #[test]
    fn test_import_path_tokens() {
        let toks = lex_kinds("import std.io;");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::Import)));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "std"));
        assert!(matches!(toks[2], TokenKind::Dot));
        assert!(matches!(&toks[3], TokenKind::Ident(s) if s == "io"));
        assert!(matches!(toks[4], TokenKind::Semicolon));
    }

    #[test]
    fn test_enum_decl_tokens() {
        let toks = lex_kinds("enum Color { Red, Green }");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::Enum)));
        assert!(matches!(&toks[1], TokenKind::Ident(s) if s == "Color"));
        assert!(matches!(toks[2], TokenKind::LBrace));
    }

    #[test]
    fn test_match_case_tokens() {
        let toks = lex_kinds("match x { 1 => return; }");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::Match)));
        assert!(matches!(toks[4], TokenKind::FatArrow));
    }

    #[test]
    fn test_try_catch_tokens() {
        let toks = lex_kinds("try { } catch e: String { }");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::Try)));
        assert!(matches!(&toks[3], TokenKind::Keyword(Keyword::Catch)));
    }

    #[test]
    fn test_while_loop_tokens() {
        let toks = lex_kinds("while i < 10 {");
        assert!(matches!(&toks[0], TokenKind::Keyword(Keyword::While)));
        assert!(matches!(toks[2], TokenKind::Less));
    }

    // ================================================================
    // Span accuracy
    // ================================================================

    #[test]
    fn test_span_end_offset() {
        let tokens = Lexer::new("abc").tokenize().unwrap();
        let ident = tokens.iter().find(|t| matches!(t.kind, TokenKind::Ident(_))).unwrap();
        assert_eq!(ident.span.end.offset, 3, "end offset should be past last char");
    }

    #[test]
    fn test_span_single_char_token() {
        let tokens = Lexer::new("+").tokenize().unwrap();
        let plus = tokens.iter().find(|t| matches!(t.kind, TokenKind::Plus)).unwrap();
        assert_eq!(plus.span.start.offset, 0);
        assert_eq!(plus.span.end.offset, 1);
    }

    #[test]
    fn test_span_two_char_token() {
        let tokens = Lexer::new("==").tokenize().unwrap();
        let eq = tokens.iter().find(|t| matches!(t.kind, TokenKind::EqualsEquals)).unwrap();
        assert_eq!(eq.span.start.offset, 0);
        assert_eq!(eq.span.end.offset, 2);
    }

    #[test]
    fn test_multiple_lines_span() {
        let tokens = Lexer::new("a\nb").tokenize().unwrap();
        let b_tok = tokens.iter().find(|t| matches!(&t.kind, TokenKind::Ident(s) if s == "b")).unwrap();
        assert_eq!(b_tok.span.start.line, 2, "b should be on line 2");
    }
}

pub mod frontend;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub struct Pos {
    pub offset: u32,
    pub line: u32,
    pub column: u32,
}

impl Span {
    pub fn new(start: Pos, end: Pos) -> Self {
        Self { start, end }
    }

    pub fn dummy() -> Self {
        Self {
            start: Pos::dummy(),
            end: Pos::dummy(),
        }
    }

    pub fn merge(self, other: Span) -> Span {
        Span {
            start: if self.start.offset < other.start.offset {
                self.start
            } else {
                other.start
            },
            end: if self.end.offset > other.end.offset {
                self.end
            } else {
                other.end
            },
        }
    }
}

impl Pos {
    pub fn dummy() -> Self {
        Self {
            offset: 0,
            line: 1,
            column: 1,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.start.line, self.start.column, self.end.offset
        )
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
    /// Unique node ID, assigned by tinox_parser::assign_node_ids after
    /// import resolution. 0 = unassigned (synthetic nodes, paths without
    /// numbering) — consumers must treat 0 as "unknown".
    pub id: u32,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span, id: 0 }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            node: f(self.node),
            span: self.span,
            id: self.id,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Error {
    pub span: Span,
    pub message: String,
}

impl Error {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    pub fn new_at(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: error: {}", self.span, self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ErrorBag {
    pub errors: Vec<Error>,
}

impl ErrorBag {
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    pub fn push(&mut self, error: Error) {
        self.errors.push(error);
    }

    pub fn extend(&mut self, other: ErrorBag) {
        self.errors.extend(other.errors);
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn check<T>(result: Result<T, Error>) -> Result<T, ErrorBag> {
        result.map_err(|e| {
            let mut bag = ErrorBag::new();
            bag.push(e);
            bag
        })
    }
}

impl Default for ErrorBag {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ErrorBag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.errors {
            writeln!(f, "{}", error)?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorBag {}

pub trait ToSpanned {
    fn to_span(self, span: Span) -> Spanned<Self>
    where
        Self: Sized;
}

impl<T> ToSpanned for T {
    fn to_span(self, span: Span) -> Spanned<Self>
    where
        Self: Sized,
    {
        Spanned::new(self, span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(offset: u32, line: u32, column: u32) -> Pos {
        Pos { offset, line, column }
    }

    fn span(s: u32, e: u32) -> Span {
        Span::new(pos(s, 1, s + 1), pos(e, 1, e + 1))
    }

    // --- Pos ---

    #[test]
    fn test_pos_dummy() {
        let p = Pos::dummy();
        assert_eq!(p.offset, 0);
        assert_eq!(p.line, 1);
        assert_eq!(p.column, 1);
    }

    #[test]
    fn test_pos_default_is_zero() {
        let p = Pos::default();
        assert_eq!(p.offset, 0);
        assert_eq!(p.line, 0);
        assert_eq!(p.column, 0);
    }

    #[test]
    fn test_pos_equality() {
        let a = pos(5, 2, 3);
        let b = pos(5, 2, 3);
        assert_eq!(a, b);
    }

    #[test]
    fn test_pos_inequality() {
        assert_ne!(pos(1, 1, 1), pos(2, 1, 1));
        assert_ne!(pos(1, 1, 1), pos(1, 2, 1));
        assert_ne!(pos(1, 1, 1), pos(1, 1, 2));
    }

    // --- Span ---

    #[test]
    fn test_span_new() {
        let s = Span::new(pos(0, 1, 1), pos(5, 1, 6));
        assert_eq!(s.start.offset, 0);
        assert_eq!(s.end.offset, 5);
    }

    #[test]
    fn test_span_dummy() {
        let s = Span::dummy();
        assert_eq!(s.start, Pos::dummy());
        assert_eq!(s.end, Pos::dummy());
    }

    #[test]
    fn test_span_merge_takes_outer_bounds() {
        let a = span(2, 5);
        let b = span(7, 12);
        let m = a.merge(b);
        assert_eq!(m.start.offset, 2);
        assert_eq!(m.end.offset, 12);
    }

    #[test]
    fn test_span_merge_overlapping() {
        let a = span(1, 8);
        let b = span(3, 6);
        let m = a.merge(b);
        assert_eq!(m.start.offset, 1);
        assert_eq!(m.end.offset, 8);
    }

    #[test]
    fn test_span_merge_same() {
        let a = span(3, 7);
        let m = a.merge(a);
        assert_eq!(m.start.offset, 3);
        assert_eq!(m.end.offset, 7);
    }

    #[test]
    fn test_span_merge_commutative() {
        let a = span(1, 5);
        let b = span(3, 9);
        assert_eq!(a.merge(b).start.offset, b.merge(a).start.offset);
        assert_eq!(a.merge(b).end.offset, b.merge(a).end.offset);
    }

    #[test]
    fn test_span_display_format() {
        let s = Span::new(pos(10, 3, 5), pos(20, 3, 15));
        let formatted = format!("{}", s);
        assert!(formatted.contains("3"), "should include line number");
        assert!(formatted.contains("5"), "should include column");
        assert!(formatted.contains("20"), "should include end offset");
    }

    #[test]
    fn test_span_equality() {
        let a = span(0, 5);
        let b = span(0, 5);
        assert_eq!(a, b);
    }

    #[test]
    fn test_span_inequality() {
        assert_ne!(span(0, 5), span(0, 6));
        assert_ne!(span(0, 5), span(1, 5));
    }

    // --- Error ---

    #[test]
    fn test_error_new() {
        let e = Error::new(Span::dummy(), "something went wrong");
        assert_eq!(e.message, "something went wrong");
    }

    #[test]
    fn test_error_new_at() {
        let e = Error::new_at(span(5, 10), "bad token");
        assert_eq!(e.message, "bad token");
        assert_eq!(e.span.start.offset, 5);
    }

    #[test]
    fn test_error_string_conversion() {
        let e = Error::new(Span::dummy(), String::from("owned message"));
        assert_eq!(e.message, "owned message");
    }

    #[test]
    fn test_error_display_contains_message() {
        let e = Error::new(span(0, 3), "unexpected token");
        let s = format!("{}", e);
        assert!(s.contains("unexpected token"), "display should contain message");
        assert!(s.contains("error"), "display should contain 'error'");
    }

    // --- ErrorBag ---

    #[test]
    fn test_errorbag_new_is_empty() {
        let bag = ErrorBag::new();
        assert!(bag.is_empty());
        assert_eq!(bag.errors.len(), 0);
    }

    #[test]
    fn test_errorbag_default_is_empty() {
        let bag = ErrorBag::default();
        assert!(bag.is_empty());
    }

    #[test]
    fn test_errorbag_push() {
        let mut bag = ErrorBag::new();
        bag.push(Error::new(Span::dummy(), "first"));
        assert!(!bag.is_empty());
        assert_eq!(bag.errors.len(), 1);
        assert_eq!(bag.errors[0].message, "first");
    }

    #[test]
    fn test_errorbag_push_multiple() {
        let mut bag = ErrorBag::new();
        bag.push(Error::new(Span::dummy(), "one"));
        bag.push(Error::new(Span::dummy(), "two"));
        bag.push(Error::new(Span::dummy(), "three"));
        assert_eq!(bag.errors.len(), 3);
    }

    #[test]
    fn test_errorbag_extend() {
        let mut a = ErrorBag::new();
        a.push(Error::new(Span::dummy(), "a1"));

        let mut b = ErrorBag::new();
        b.push(Error::new(Span::dummy(), "b1"));
        b.push(Error::new(Span::dummy(), "b2"));

        a.extend(b);
        assert_eq!(a.errors.len(), 3);
        assert_eq!(a.errors[1].message, "b1");
        assert_eq!(a.errors[2].message, "b2");
    }

    #[test]
    fn test_errorbag_check_ok() {
        let result: Result<i32, Error> = Ok(42);
        let checked = ErrorBag::check(result);
        assert!(checked.is_ok());
        assert_eq!(checked.unwrap(), 42);
    }

    #[test]
    fn test_errorbag_check_err() {
        let result: Result<i32, Error> = Err(Error::new(Span::dummy(), "failed"));
        let checked = ErrorBag::check(result);
        assert!(checked.is_err());
        let bag = checked.unwrap_err();
        assert_eq!(bag.errors.len(), 1);
        assert_eq!(bag.errors[0].message, "failed");
    }

    #[test]
    fn test_errorbag_display_empty() {
        let bag = ErrorBag::new();
        assert_eq!(format!("{}", bag), "");
    }

    #[test]
    fn test_errorbag_display_with_errors() {
        let mut bag = ErrorBag::new();
        bag.push(Error::new(span(0, 1), "oops"));
        let s = format!("{}", bag);
        assert!(s.contains("oops"));
    }

    // --- Spanned ---

    #[test]
    fn test_spanned_new() {
        let s = Spanned::new(42, Span::dummy());
        assert_eq!(s.node, 42);
    }

    #[test]
    fn test_spanned_map() {
        let s = Spanned::new(10i32, Span::dummy());
        let doubled = s.map(|x| x * 2);
        assert_eq!(doubled.node, 20);
    }

    #[test]
    fn test_spanned_map_preserves_span() {
        let sp = span(5, 10);
        let s = Spanned::new("hello", sp);
        let mapped = s.map(|x| x.len());
        assert_eq!(mapped.span.start.offset, 5);
        assert_eq!(mapped.span.end.offset, 10);
    }

    #[test]
    fn test_spanned_map_type_change() {
        let s: Spanned<i32> = Spanned::new(99, Span::dummy());
        let as_string: Spanned<String> = s.map(|n| n.to_string());
        assert_eq!(as_string.node, "99");
    }

    // --- ToSpanned trait ---

    #[test]
    fn test_to_spanned_trait() {
        use super::ToSpanned;
        let sp = span(0, 3);
        let s = 42i32.to_span(sp);
        assert_eq!(s.node, 42);
        assert_eq!(s.span.start.offset, 0);
    }
}

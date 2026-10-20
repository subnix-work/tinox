//! Versioned compiler diagnostics shared by CLI and editor consumers.
use crate::{Error, Span};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedLocation {
    pub file: String,
    pub range: Span,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    pub category: String,
    pub file: Option<String>,
    pub range: Option<Span>,
    pub message: String,
    pub related: Vec<RelatedLocation>,
    pub notes: Vec<String>,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl Diagnostic {
    pub fn new(
        category: &str,
        file: Option<&str>,
        range: Option<Span>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: format!("TINOX-{}", category.to_ascii_uppercase()),
            severity: "error".into(),
            category: category.into(),
            file: file.map(|file| {
                std::path::Path::new(file)
                    .canonicalize()
                    .unwrap_or_else(|_| std::path::PathBuf::from(file))
                    .to_string_lossy()
                    .into_owned()
            }),
            range,
            message: message.into(),
            related: Vec::new(),
            notes: Vec::new(),
            expected: None,
            actual: None,
        }
    }

    pub fn from_error(error: Error, category: &str, file: Option<&str>) -> Self {
        let mut diagnostic = Self::new(
            if error.code.as_deref() == Some("TINOX-ANNOTATION") {
                "annotation"
            } else {
                category
            },
            error.file.as_deref().or(file),
            Some(error.span),
            error.message,
        );
        if let Some(code) = error.code {
            diagnostic.code = code.into();
        }
        diagnostic.expected = error.expected.map(Into::into);
        diagnostic.actual = error.actual.map(Into::into);
        diagnostic
    }
}

/// Carries diagnostic records through the frontend and backend. Conversion to a
/// string is reserved for legacy command consumers that only render text.
#[derive(Debug)]
pub struct Failure(pub Vec<Diagnostic>);
impl Failure {
    pub fn with_code(mut self, code: &str) -> Self {
        for diagnostic in &mut self.0 {
            diagnostic.code = code.into();
        }
        self
    }

    pub fn new(
        category: &str,
        file: Option<&str>,
        range: Option<Span>,
        message: impl Into<String>,
    ) -> Self {
        Self(vec![Diagnostic::new(category, file, range, message)])
    }
    pub fn errors(errors: Vec<Error>, category: &str, file: Option<&str>) -> Self {
        Self(
            errors
                .into_iter()
                .map(|e| Diagnostic::from_error(e, category, file))
                .collect(),
        )
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for diagnostic in &self.0 {
            if let (Some(file), Some(range)) = (&diagnostic.file, diagnostic.range) {
                write!(f, "{}:{}:{}: ", file, range.start.line, range.start.column)?;
            }
            writeln!(f, "{}", diagnostic.message)?;
            for note in &diagnostic.notes {
                writeln!(f, "{note}")?;
            }
        }
        Ok(())
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new("driver", None, None, message)
    }
}
impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.to_string()
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

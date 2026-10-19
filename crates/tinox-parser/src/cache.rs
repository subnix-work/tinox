//! Persist only pristine parsed sources: no imported declarations, generated
//! accessors, stamped file paths, or transient node IDs enter this query.
use crate::{Parser, SourceFile};
use tinox_common::{Error, ErrorBag};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ParseFailure {
    Lex(Vec<Error>),
    Parse(ErrorBag),
}
impl ParseFailure {
    pub fn into_errors(self) -> Vec<Error> {
        match self {
            Self::Lex(errors) => errors,
            Self::Parse(bag) => bag.errors,
        }
    }
}

pub fn parse_source_cached(source: &str) -> Result<SourceFile, ParseFailure> {
    let _stage = tinox_common::frontend::Stage::new("parse");
    tinox_common::frontend::query("parse", &source, || {
        let tokens = tinox_lexer::Lexer::new(source)
            .tokenize()
            .map_err(ParseFailure::Lex)?;
        Parser::new(tokens).parse().map_err(ParseFailure::Parse)
    })
}

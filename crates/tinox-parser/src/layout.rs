//! Layout-only source formatter behind `tinox fmt`.
//!
//! It normalises indentation (4 spaces per block / bracket level), trailing
//! whitespace, runs of blank lines and the final newline -- and nothing else.
//! Every token, comment and line break of the input is kept, so formatting can
//! never change what a program means or lose a comment, and the result is a
//! pure function of the token structure (hence idempotent).

use tinox_common::Error;
use tinox_lexer::{Lexer, TokenKind};

const INDENT: &str = "    ";

/// One open bracket: the indentation level of the line it was opened on.
struct Open {
    line_indent: usize,
}

fn is_closer(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket)
}

fn is_opener(kind: &TokenKind) -> bool {
    matches!(kind, TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket)
}

/// Binary operators that, at the end of a line, continue the statement.
fn continues_statement(prev: &TokenKind, before_prev: Option<&TokenKind>) -> bool {
    match prev {
        TokenKind::Star => !matches!(before_prev, Some(TokenKind::Dot)), // `import a.b.*`
        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::Equals
        | TokenKind::EqualsEquals
        | TokenKind::BangEquals
        | TokenKind::LessEquals
        | TokenKind::GreaterEquals
        | TokenKind::AmpAmp
        | TokenKind::BarBar
        | TokenKind::QuestionQuestion
        | TokenKind::QuestionColon
        | TokenKind::FatArrow
        | TokenKind::ThinArrow
        | TokenKind::PlusEquals
        | TokenKind::MinusEquals
        | TokenKind::StarEquals
        | TokenKind::SlashEquals => true,
        _ => false,
    }
}

fn starts_continuation(first: &TokenKind) -> bool {
    matches!(
        first,
        TokenKind::Dot
            | TokenKind::AmpAmp
            | TokenKind::BarBar
            | TokenKind::QuestionQuestion
            | TokenKind::QuestionColon
            | TokenKind::Question
            | TokenKind::Colon
    )
}

pub fn format_source(source: &str) -> Result<String, Vec<Error>> {
    let tokens = Lexer::new(source).tokenize_with_trivia()?;
    let lines: Vec<&str> = source.split('\n').collect();

    // Lines that are the 2nd..nth line of a multi-line token, and whether that
    // token is a comment (re-indentable `*` lines) or a literal (untouchable).
    #[derive(Clone, Copy, PartialEq)]
    enum Inside {
        No,
        Comment,
        Literal,
    }
    let mut inside = vec![Inside::No; lines.len() + 2];
    // Per line: index of its first non-whitespace token.
    let mut first_token: Vec<Option<usize>> = vec![None; lines.len() + 2];
    for (i, t) in tokens.iter().enumerate() {
        if matches!(t.kind, TokenKind::Whitespace | TokenKind::Newline | TokenKind::Eof) {
            continue;
        }
        let (start, end) = (t.span.start.line as usize, t.span.end.line as usize);
        if start >= 1 && start < first_token.len() && first_token[start].is_none() {
            first_token[start] = Some(i);
        }
        if end > start {
            let kind = if matches!(t.kind, TokenKind::Comment(_) | TokenKind::DocComment(_)) {
                Inside::Comment
            } else {
                Inside::Literal
            };
            for line in (start + 1)..=end.min(inside.len() - 1) {
                inside[line] = kind;
            }
        }
    }

    // Significant (non-trivia) token before token `i`, and the one before that.
    let significant: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.kind.is_trivia() && !matches!(t.kind, TokenKind::Eof))
        .map(|(i, _)| i)
        .collect();
    let sig_pos = |token_index: usize| significant.partition_point(|&i| i < token_index);

    let mut stack: Vec<Open> = Vec::new();
    let mut cursor = 0usize; // next token to feed into `stack`
    let mut advance = |stack: &mut Vec<Open>, line_no: usize, indent: usize| {
        while cursor < tokens.len() && (tokens[cursor].span.start.line as usize) <= line_no {
            let kind = &tokens[cursor].kind;
            if is_opener(kind) {
                stack.push(Open { line_indent: indent });
            } else if is_closer(kind) {
                stack.pop();
            }
            cursor += 1;
        }
    };

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut blank_run = 0usize;
    // Indentation of the most recent regular line (also what brackets opened on
    // the tail of a multi-line token inherit).
    let mut last_indent = 0usize;
    for (idx, raw) in lines.iter().enumerate() {
        let line_no = idx + 1;
        match inside[line_no] {
            Inside::Literal => {
                out.push((*raw).trim_end_matches('\r').to_string());
                blank_run = 0;
                advance(&mut stack, line_no, last_indent);
                continue;
            }
            Inside::Comment => {
                let trimmed = raw.trim();
                if trimmed.starts_with('*') {
                    out.push(format!("{} {}", INDENT.repeat(last_indent), trimmed));
                } else {
                    out.push(raw.trim_end().to_string());
                }
                blank_run = 0;
                advance(&mut stack, line_no, last_indent);
                continue;
            }
            Inside::No => {}
        }
        if raw.trim().is_empty() {
            blank_run += 1;
            if blank_run == 1 && !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;

        let Some(first) = first_token[line_no] else {
            out.push(raw.trim_end().to_string());
            continue;
        };
        let first_kind = &tokens[first].kind;

        let mut indent = if is_closer(first_kind) {
            stack.last().map(|o| o.line_indent).unwrap_or(0)
        } else {
            stack.last().map(|o| o.line_indent + 1).unwrap_or(0)
        };
        // Statement continuation lines (operator-terminated or operator-led).
        if !is_closer(first_kind) && !matches!(first_kind, TokenKind::LBrace) {
            let sp = sig_pos(first);
            let prev_continues = sp >= 1 && {
                let prev = &tokens[significant[sp - 1]].kind;
                let before = if sp >= 2 { Some(&tokens[significant[sp - 2]].kind) } else { None };
                continues_statement(prev, before)
            };
            if prev_continues || starts_continuation(first_kind) {
                indent += 1;
            }
        }
        last_indent = indent;
        out.push(format!("{}{}", INDENT.repeat(indent), raw.trim()));
        advance(&mut stack, line_no, indent);
    }

    // Trim leading/trailing blank lines; exactly one final newline.
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let mut result = out.join("\n");
    result.push('\n');
    Ok(result)
}

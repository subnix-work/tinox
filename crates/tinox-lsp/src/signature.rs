//! `textDocument/signatureHelp` (issue #494).
//!
//! The call under the cursor is found by scanning back to the unmatched `(`,
//! counting top-level commas for the active parameter. The callee is resolved
//! by form: `name(` to project functions, `Type::name(` to that type's
//! methods, `expr.name(` to every project or standard-library method of that
//! name (the receiver type is unknown here, so all candidates are offered,
//! project types first).

use crate::analysis::TypeRegistry;
use tower_lsp::lsp_types::{Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation};

#[derive(Debug, PartialEq)]
pub enum Callee {
    /// `name(`
    Bare(String),
    /// `Type::name(`
    Static(String, String),
    /// `expr.name(`
    Member(String),
}

#[derive(Debug, PartialEq)]
pub struct CallContext {
    pub callee: Callee,
    pub active_parameter: u32,
}

/// Finds the innermost call the cursor (`offset`, a byte offset) is inside.
pub fn call_context(text: &str, offset: usize) -> Option<CallContext> {
    let head = &text[..offset.min(text.len())];
    let bytes = head.as_bytes();
    let mut depth = 0i32;
    let mut commas = 0u32;
    let mut i = bytes.len();
    let mut open = None;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'"' => {
                // skip the string literal backwards (naive: ignores escaped quotes)
                while i > 0 {
                    i -= 1;
                    if bytes[i] == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                        break;
                    }
                }
            }
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    if bytes[i] == b'(' {
                        open = Some(i);
                    }
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            b'\n' if depth == 0 && head[..i].trim_end().ends_with(['{', '}']) => break,
            _ => {}
        }
    }
    let open = open?;
    // identifier directly before `(`
    let before = head[..open].trim_end();
    let name_start = before.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map(|p| p + before[p..].chars().next().unwrap().len_utf8()).unwrap_or(0);
    let name = &before[name_start..];
    if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    if matches!(name, "if" | "while" | "for" | "match" | "catch" | "fn" | "fnc" | "return") {
        return None;
    }
    let rest = &before[..name_start];
    let callee = if let Some(q) = rest.strip_suffix("::") {
        let tstart = q.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map(|p| p + q[p..].chars().next().unwrap().len_utf8()).unwrap_or(0);
        Callee::Static(q[tstart..].to_string(), name.to_string())
    } else if rest.ends_with('.') {
        Callee::Member(name.to_string())
    } else {
        // a declaration `fn name(` is not a call
        let t = rest.trim_end();
        if t.ends_with("fn") || t.ends_with("fnc") {
            return None;
        }
        Callee::Bare(name.to_string())
    };
    Some(CallContext { callee, active_parameter: commas })
}

/// Splits `fn name(a: Int64, b: Map<String, Int64>) -> X` into parameter labels.
pub fn split_params(signature: &str) -> Vec<String> {
    let Some(open) = signature.find('(') else { return Vec::new() };
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut cur = String::new();
    let mut out = Vec::new();
    for c in signature[open + 1..].chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            '<' => angle += 1,
            '>' => angle -= 1,
            ',' if depth == 0 && angle <= 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub struct Candidate {
    pub signature: String,
    pub doc: Option<String>,
}

/// Collects the candidate signatures for a callee.
pub fn candidates(callee: &Callee, registry: &TypeRegistry, project_fns: &[(String, String, Option<String>)]) -> Vec<Candidate> {
    let mut out = Vec::new();
    match callee {
        Callee::Bare(name) => {
            for (n, sig, doc) in project_fns {
                if n == name {
                    out.push(Candidate { signature: sig.clone(), doc: doc.clone() });
                }
            }
        }
        Callee::Static(ty, name) => {
            if let Some(info) = registry.get(ty) {
                for m in info.methods.iter().filter(|m| &m.name == name) {
                    out.push(Candidate { signature: m.signature.clone(), doc: m.doc.clone() });
                }
            }
        }
        Callee::Member(name) => {
            let mut classes: Vec<_> = registry.iter().collect();
            classes.sort_by(|a, b| a.0.cmp(b.0));
            for (_, info) in classes {
                for m in info.methods.iter().filter(|m| &m.name == name) {
                    if !out.iter().any(|c: &Candidate| c.signature == m.signature) {
                        out.push(Candidate { signature: m.signature.clone(), doc: m.doc.clone() });
                    }
                }
            }
            out.truncate(10);
        }
    }
    out
}

pub fn build(ctx: &CallContext, cands: Vec<Candidate>) -> Option<SignatureHelp> {
    if cands.is_empty() {
        return None;
    }
    // Prefer the signature that can still accept the active parameter.
    let active_signature = cands
        .iter()
        .position(|c| split_params(&c.signature).len() > ctx.active_parameter as usize)
        .unwrap_or(0) as u32;
    let signatures = cands
        .into_iter()
        .map(|c| SignatureInformation {
            parameters: Some(split_params(&c.signature).into_iter().map(|p| ParameterInformation { label: ParameterLabel::Simple(p), documentation: None }).collect()),
            documentation: c.doc.map(|d| Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: d })),
            active_parameter: None,
            label: c.signature,
        })
        .collect::<Vec<_>>();
    let params = signatures[active_signature as usize].parameters.as_ref().map(|p| p.len()).unwrap_or(0) as u32;
    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature),
        // past the last parameter: no highlight rather than a wrong one
        active_parameter: Some(if ctx.active_parameter < params { ctx.active_parameter } else { u32::MAX }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_call_and_active_parameter() {
        let t = "const x = Math::max(a, f(b, c), ";
        let c = call_context(t, t.len()).unwrap();
        assert_eq!(c.callee, Callee::Static("Math".into(), "max".into()));
        assert_eq!(c.active_parameter, 2);
        let t = "const x = Math::max(a, f(b, ";
        let c = call_context(t, t.len()).unwrap();
        assert_eq!(c.callee, Callee::Bare("f".into()));
        assert_eq!(c.active_parameter, 1);
        let t = "list.push(\"a,b\", ";
        let c = call_context(t, t.len()).unwrap();
        assert_eq!(c.callee, Callee::Member("push".into()));
        assert_eq!(c.active_parameter, 1, "commas in strings do not count");
    }

    #[test]
    fn not_a_call() {
        for t in ["if (a, ", "fn make(", "fnc make(", "x = (1, ", "foo(1)", "class A {"] {
            assert!(call_context(t, t.len()).is_none(), "{t}");
        }
    }

    #[test]
    fn splits_params_with_generics() {
        assert_eq!(split_params("fn f(a: Map<String, Int64>, b: fnc(Int64) -> Int64) -> X"), ["a: Map<String, Int64>", "b: fnc(Int64) -> Int64"]);
        assert_eq!(split_params("fn f() -> X"), Vec::<String>::new());
    }
}

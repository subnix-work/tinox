//! `@Retry` / `@CircuitBreaker` (#527): argument parsing shared with the annotation validator and
//! the parse-time desugaring that wraps an annotated method.
//!
//! `@Retry(maxAttempts, backoffMs, ["PREFIX", ...][, maxBackoffMs[, jitterPercent[, budgetMs]]])`
//! `@CircuitBreaker(failureThreshold, openMs[, ["PREFIX", ...][, probes]])`
//!
//! A failure is a thrown `String`; "eligible" lists the error prefixes that count (an empty list
//! on `@CircuitBreaker` means every failure). The method `m` becomes a wrapper that loops over the
//! original body, which is kept as the private method `m__tinox_impl`. Retries additionally need
//! `@Idempotent` on the method (checked by the validator): replaying a write is never implicit.

use crate::ast::*;
use tinox_lexer::Lexer;

#[derive(Debug, Clone)]
pub struct RetrySpec {
    pub max_attempts: i64,
    pub backoff_ms: i64,
    pub eligible: Vec<String>,
    pub max_backoff_ms: i64,
    pub jitter_pct: i64,
    pub budget_ms: i64,
}

#[derive(Debug, Clone)]
pub struct BreakerSpec {
    pub threshold: i64,
    pub open_ms: i64,
    pub eligible: Vec<String>,
    pub probes: i64,
}

fn int(arg: Option<&AnnotationArg>, what: &str, lo: i64, hi: i64, default: Option<i64>) -> Result<i64, String> {
    match arg {
        None => default.ok_or_else(|| format!("missing {what}")),
        Some(AnnotationArg::Literal(Literal::Integer(n))) if (lo..=hi).contains(n) => Ok(*n),
        Some(AnnotationArg::Literal(Literal::Integer(_))) => Err(format!("{what} must be between {lo} and {hi}")),
        Some(_) => Err(format!("{what} must be an integer literal")),
    }
}

fn prefixes(arg: Option<&AnnotationArg>, required: bool) -> Result<Vec<String>, String> {
    match arg {
        None if required => Err("missing the list of eligible error prefixes, e.g. [\"UNAVAILABLE\", \"TIMEOUT\"]".into()),
        None => Ok(vec![]),
        Some(AnnotationArg::Array(items)) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    AnnotationArg::Literal(Literal::String(s)) if !s.is_empty() && !s.contains('|') && !s.chars().any(|c| c.is_control()) => out.push(s.clone()),
                    _ => return Err("eligible error prefixes must be non-empty string literals without '|'".into()),
                }
            }
            if required && out.is_empty() {
                return Err("the eligible error list must not be empty: name the failures that may be retried".into());
            }
            Ok(out)
        }
        Some(_) => Err("eligible errors must be a list of string literals".into()),
    }
}

pub fn retry_spec(a: &Annotation) -> Result<RetrySpec, String> {
    if a.args.len() > 6 {
        return Err("@Retry takes at most 6 arguments".into());
    }
    let max_attempts = int(a.args.first(), "maxAttempts", 2, 10, None)?;
    let backoff_ms = int(a.args.get(1), "backoffMs", 0, 600_000, None)?;
    let eligible = prefixes(a.args.get(2), true)?;
    let max_backoff_ms = int(a.args.get(3), "maxBackoffMs", 0, 3_600_000, Some(30_000))?;
    let jitter_pct = int(a.args.get(4), "jitterPercent", 0, 100, Some(20))?;
    let budget_ms = int(a.args.get(5), "budgetMs", 0, 3_600_000, Some(30_000))?;
    Ok(RetrySpec { max_attempts, backoff_ms, eligible, max_backoff_ms, jitter_pct, budget_ms })
}

pub fn breaker_spec(a: &Annotation) -> Result<BreakerSpec, String> {
    if a.args.len() > 4 {
        return Err("@CircuitBreaker takes at most 4 arguments".into());
    }
    let threshold = int(a.args.first(), "failureThreshold", 1, 1000, None)?;
    let open_ms = int(a.args.get(1), "openMs", 1, 3_600_000, None)?;
    let eligible = prefixes(a.args.get(2), false)?;
    let probes = int(a.args.get(3), "probes", 1, 16, Some(1))?;
    Ok(BreakerSpec { threshold, open_ms, eligible, probes })
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Expands an annotated method into `[wrapper, impl]`; returns `Ok(None)` when it carries neither
/// annotation. `Err` is a user-facing message (bad arguments, generics).
pub fn expand(class_name: &str, class_generic: bool, m: &Method) -> Result<Option<Vec<Method>>, String> {
    let retry = m.annotations.iter().find(|a| a.name == "Retry");
    let breaker = m.annotations.iter().find(|a| a.name == "CircuitBreaker");
    if retry.is_none() && breaker.is_none() {
        return Ok(None);
    }
    if class_generic || !m.type_params.is_empty() {
        return Err("@Retry/@CircuitBreaker are not supported on generic classes or methods".into());
    }
    let retry = retry.map(retry_spec).transpose().map_err(|e| format!("@Retry: {e}"))?;
    let breaker = breaker.map(breaker_spec).transpose().map_err(|e| format!("@CircuitBreaker: {e}"))?;

    let impl_name = format!("{}__tinox_impl", m.name);
    let site = quote(&format!("{}.{}", class_name, m.name));
    let args = m.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
    let call = if m.static_ { format!("{class_name}::{impl_name}({args})") } else { format!("this.{impl_name}({args})") };
    let returns_value = !matches!(m.ret_type, Type::Nothing);

    let mut body = String::from("{\n");
    if retry.is_some() {
        body.push_str("const __start = resilienceNow()\nvar __attempt: Int64 = 0\n");
    }
    body.push_str("while (true) {\n");
    if let Some(b) = &breaker {
        body.push_str(&format!(
            "const __ticket = resilienceAllow({site}, {}, {}, {}, {})\nif (__ticket == 0) {{ throw {} }}\n",
            b.threshold, b.open_ms, b.probes, quote(&b.eligible.join("|")),
            quote(&format!("CIRCUIT_OPEN: {}.{} rejected the call", class_name, m.name)),
        ));
    }
    body.push_str("try {\n");
    body.push_str(&if returns_value { format!("const __r = {call}\n") } else { format!("{call}\n") });
    if let Some(b) = &breaker {
        body.push_str(&format!("resilienceRecord({site}, __ticket, 0, \"\", {}, {})\n", b.threshold, quote(&b.eligible.join("|"))));
    }
    body.push_str(if returns_value { "return __r\n" } else { "return\n" });
    body.push_str("} catch (__e: String) {\n");
    if let Some(b) = &breaker {
        body.push_str(&format!("resilienceRecord({site}, __ticket, 1, __e, {}, {})\n", b.threshold, quote(&b.eligible.join("|"))));
    }
    if let Some(r) = &retry {
        body.push_str(&format!(
            "__attempt = __attempt + 1\nif (resilienceRetry({site}, __attempt, __e, {}, {}, {}, {}, {}, {}, __start) == 0) {{ throw __e }}\n",
            quote(&r.eligible.join("|")), r.max_attempts, r.backoff_ms, r.max_backoff_ms, r.jitter_pct, r.budget_ms,
        ));
    } else {
        body.push_str("throw __e\n");
    }
    body.push_str("}\n}\nthrow \"unreachable\"\n}\n");

    // Pad so generated statements carry the method's own line number (debugger markers).
    let padded = format!("{}{}", "\n".repeat(m.span.start.line.saturating_sub(1) as usize), body);
    let tokens = Lexer::new(&padded).tokenize().map_err(|e| format!("internal: resilience wrapper does not lex: {e:?}"))?;
    let mut sub = crate::parser::Parser::new(tokens);
    let block = sub.parse_wrapper_block().map_err(|e| format!("internal: resilience wrapper does not parse: {}", e.message))?;

    // Per-attempt concerns move to the body so each attempt gets its own transaction / monitor.
    let per_attempt = |a: &Annotation| a.name == "Transactional" || a.name == "Synchronized";
    let mut inner = m.clone();
    inner.name = impl_name;
    inner.visibility = Visibility::Private;
    inner.doc = None;
    inner.annotations = m.annotations.iter().filter(|a| per_attempt(a)).cloned().collect();
    for p in &mut inner.params {
        p.annotations.clear();
    }
    let mut wrapper = m.clone();
    wrapper.annotations = m.annotations.iter().filter(|a| !per_attempt(a)).cloned().collect();
    wrapper.body = block;
    Ok(Some(vec![wrapper, inner]))
}

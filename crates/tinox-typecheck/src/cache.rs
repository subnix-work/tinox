//! Query boundaries shared by the compiler and language server. Dependency
//! inputs deliberately retain full bodies: `this` usage affects call semantics,
//! so stripping bodies to approximate an exported signature is unsafe today.
use crate::{TypeChecker, ValueType};
use std::collections::HashMap;
use tinox_common::ErrorBag;
use tinox_parser::SourceFile;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CheckedProgram {
    pub expr_types: HashMap<u32, ValueType>,
    pub interface_methods: HashMap<String, Vec<String>>,
    pub class_implements: HashMap<String, Vec<String>>,
}

/// Exact ordered AST (including fresh node numbering) is the key. Cached type
/// tables can ONLY be used with that numbering; a changed graph always misses.
pub fn check_program_cached(source: &SourceFile) -> Result<CheckedProgram, ErrorBag> {
    let _stage = tinox_common::frontend::Stage::new("typecheck");
    tinox_common::frontend::query("program", source, || {
        let mut checker = TypeChecker::new();
        checker.check(source)?;
        let (interface_methods, class_implements) = checker.interface_info();
        Ok(CheckedProgram {
            expr_types: checker.expr_value_types(),
            interface_methods,
            class_implements,
        })
    })
}

/// Each canonical file's independent visibility/diagnostics query depends on its
/// ordered, resolved prelude closure. Changes to any dependency conservatively
/// invalidate the user; unrelated files retain their results. Cache diagnostics
/// as structured spans/messages so callers keep their existing rendering.
pub fn check_with_prelude_cached(
    source: &SourceFile,
    preludes: &[&SourceFile],
    identity: &str,
) -> Result<(), ErrorBag> {
    let _stage = tinox_common::frontend::Stage::new("file-typecheck");
    tinox_common::frontend::query("visibility", &(identity, source, preludes), || {
        crate::typecheck_with_prelude(source, preludes).map(|_| ())
    })
}

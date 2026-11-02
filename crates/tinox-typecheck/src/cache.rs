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
    pub semantics: crate::semantic::SemanticFacts,
    pub interface_methods: HashMap<String, Vec<String>>,
    pub class_implements: HashMap<String, Vec<String>>,
    pub warnings: Vec<String>,
}

/// Exact ordered AST (including fresh node numbering) is the key. Cached type
/// tables can ONLY be used with that numbering; a changed graph always misses.
pub fn check_program_cached(
    source: &SourceFile,
    project_root: Option<&std::path::Path>,
) -> Result<CheckedProgram, ErrorBag> {
    let _stage = tinox_common::frontend::Stage::new("typecheck");
    tinox_common::frontend::query("program", &(source, project_root.map(|p| p.to_string_lossy().into_owned())), || {
        let mut checker = TypeChecker::new();
        if let Some(root) = project_root { checker.set_project_root(root.to_path_buf()); }
        checker.check(source)?;
        let (interface_methods, class_implements) = checker.interface_info();
        Ok(CheckedProgram {
            expr_types: checker.expr_value_types(),
            semantics: checker.semantic_facts(),
            interface_methods,
            class_implements,
            warnings: checker.warnings(),
        })
    })
}

/// Content digest of a parsed source, for keying `check_with_prelude_digests`. `None` when it cannot be encoded.
pub fn ast_digest(source: &SourceFile) -> Option<[u8; 32]> {
    tinox_common::frontend::digest(source)
}

/// Each canonical file's independent visibility/diagnostics query depends on its
/// ordered, resolved prelude closure. Changes to any dependency conservatively
/// invalidate the user; unrelated files retain their results. Cache diagnostics
/// as structured spans/messages so callers keep their existing rendering.
///
/// The key is the file identity plus the digests of the exact ordered ASTs of the file and its preludes, so it
/// still changes with any byte-level change of any of them (full bodies included, see the module note). A caller
/// that checks many files over shared preludes computes each digest once and calls `check_with_prelude_digests`
/// directly; encoding every prelude again for every file is quadratic in densely connected projects (#559).
pub fn check_with_prelude_cached(
    source: &SourceFile,
    preludes: &[&SourceFile],
    identity: &str,
) -> Result<(), ErrorBag> {
    let digests = ast_digest(source).zip(preludes.iter().map(|p| ast_digest(p)).collect::<Option<Vec<_>>>());
    match digests {
        Some((source_digest, prelude_digests)) => check_with_prelude_digests(source, &source_digest, preludes, &prelude_digests, identity),
        None => {
            let _stage = tinox_common::frontend::Stage::new("file-typecheck");
            crate::typecheck_with_prelude(source, preludes).map(|_| ())
        }
    }
}

/// Like `check_with_prelude_cached` with the digests supplied (`prelude_digests[i]` belongs to `preludes[i]`).
pub fn check_with_prelude_digests(
    source: &SourceFile,
    source_digest: &[u8; 32],
    preludes: &[&SourceFile],
    prelude_digests: &[[u8; 32]],
    identity: &str,
) -> Result<(), ErrorBag> {
    debug_assert_eq!(preludes.len(), prelude_digests.len());
    let _stage = tinox_common::frontend::Stage::new("file-typecheck");
    tinox_common::frontend::query("visibility", &(identity, source_digest, prelude_digests), || {
        crate::typecheck_with_prelude(source, preludes).map(|_| ())
    })
}

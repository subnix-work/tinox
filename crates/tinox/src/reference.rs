//! Compiler-owned reference export. No parallel signature/annotation registry.
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};
use tinox_parser::DeclKind;

pub fn metadata(path: Option<&Path>) -> Result<Value, String> {
    let processor = tinox_typecheck::annotations::AnnotationProcessor::new();
    let annotations: Vec<_> = processor
        .reference_annotations()
        .into_iter()
        .map(|a| {
            json!({
                "name": a.name,
                "targets": a.valid_targets.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
                "min_args": a.min_args,
                "max_args": a.max_args,
                "description": a.description,
            })
        })
        .collect();
    let mut sources = BTreeMap::new();
    if let Some(path) = path {
        if !path.exists() {
            return Err(format!("source path does not exist: {}", path.display()));
        }
        let mut files = Vec::new();
        collect_sources(path, &mut files)?;
        if files.is_empty() {
            return Err(format!("no .tnx sources in {}", path.display()));
        }
        for file in files {
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
            let tokens = tinox_lexer::Lexer::new(&text)
                .tokenize()
                .map_err(|e| format!("{file}: lex error: {e:?}"))?;
            let ast = tinox_parser::Parser::new(tokens)
                .parse()
                .map_err(|e| format!("{file}: parse error: {e:?}"))?;
            let mut items = Vec::new();
            for decl in &ast.decls {
                super::collect_doc_items(&decl.node, &mut items);
            }
            let mut items = serde_json::to_value(items).map_err(|e| e.to_string())?;
            for decl in &ast.decls {
                collect_additional_items(
                    &decl.node,
                    items.as_array_mut().expect("doc items array"),
                );
            }
            // Relative keys keep the export independent of the checkout location.
            let base = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(Path::new("."))
            };
            let key = Path::new(&file)
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            sources.insert(key, items);
        }
    }
    Ok(json!({
        "schema_version": 1,
        "compiler_version": env!("CARGO_PKG_VERSION"),
        "builtin_signature_scope": "Type-checker signatures; erased types and runtime variadic calls are not full source API contracts.",
        "annotation_scope": "Validator targets and argument counts; acceptance alone does not guarantee runtime support. Descriptions may impose additional argument constraints.",
        "builtins": tinox_typecheck::TypeChecker::new().reference_signatures(),
        "annotations": annotations,
        "sources": sources,
    }))
}

fn collect_sources(path: &Path, files: &mut Vec<String>) -> Result<(), String> {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for entry in entries {
            collect_sources(&entry, files)?;
        }
    } else if path.extension().is_some_and(|e| e == "tnx") {
        files.push(path.to_string_lossy().into_owned());
    }
    Ok(())
}

// The HTML model currently covers classes, interfaces and functions. Include
// enums/traits too in the machine reference, directly from the same parser AST.
fn collect_additional_items(decl: &DeclKind, out: &mut Vec<Value>) {
    match decl {
        DeclKind::Enum(e) => out.push(json!({
            "kind": "Enum", "name": e.name, "doc": e.doc,
            "variants": e.variants.iter().map(|v| json!({
                "name": v.name, "args": v.args.iter().map(super::type_str_simple).collect::<Vec<_>>(), "doc": v.doc,
            })).collect::<Vec<_>>(),
        })),
        DeclKind::Trait(t) => {
            let mut methods = Vec::new();
            for method in &t.methods {
                super::collect_doc_items(&DeclKind::Function(method.clone()), &mut methods);
            }
            out.push(json!({"kind": "Trait", "name": t.name, "doc": t.doc, "methods": methods}));
        }
        DeclKind::Namespace(ns) => {
            for inner in &ns.decls { collect_additional_items(&inner.node, out); }
        }
        _ => {}
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.len() > 1 || args.first().is_some_and(|a| a.starts_with('-')) {
        return Err("usage: tinox reference [source-file-or-directory]".into());
    }
    let value = metadata(args.first().map(Path::new))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}

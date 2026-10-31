//! Checked receiver identities for member navigation (issue #554).
//!
//! `navigation` works on tokens and knows no receiver types. For a member name declared by several
//! unrelated types this module asks the SAME checker the compiler uses (resolved calls and field
//! accesses from `tinox_typecheck::semantic`) which class each `x.member` access really reaches, and
//! maps that back to the member-name token. Nothing here is a second analyzer: an access the
//! checker could not resolve simply has no entry, and the caller must treat it as unproven.
use std::collections::{HashMap, HashSet};

use tinox_lexer::TokenKind;
use tinox_parser::ast::{Decl, DeclKind, SourceFile};
use tinox_typecheck::ValueType;
use tower_lsp::lsp_types::Url;

use crate::navigation::{char_to_byte, FileIndex};

pub(crate) struct Checked {
    /// (file, byte offset of the member-name token) -> class of the receiver
    owners: HashMap<(Url, usize), String>,
    /// class/interface -> direct parents (`extends` and `implements`)
    parents: HashMap<String, Vec<String>>,
}

fn collect_parents(decls: &[Decl], out: &mut HashMap<String, Vec<String>>) {
    for d in decls {
        match &d.node {
            DeclKind::Class(c) => {
                let mut p: Vec<String> = c.extends.iter().cloned().collect();
                p.extend(c.implements.iter().cloned());
                out.entry(c.name.clone()).or_default().extend(p);
            }
            DeclKind::Interface(i) => out.entry(i.name.clone()).or_default().extend(i.extends.iter().cloned()),
            DeclKind::Namespace(ns) => collect_parents(&ns.decls, out),
            _ => {}
        }
    }
}

impl Checked {
    /// Resolves the receivers of every access to `member` in the project's files.
    pub(crate) fn analyze(files: &[FileIndex], member: &str) -> Checked {
        let parsed: Vec<Option<SourceFile>> = files.iter().map(|f| tinox_parser::parse_source_cached(&f.text).ok()).collect();
        let mut parents = HashMap::new();
        for ast in parsed.iter().flatten() {
            collect_parents(&ast.decls, &mut parents);
        }
        let mut owners = HashMap::new();
        for (fi, f) in files.iter().enumerate() {
            let Some(ast) = &parsed[fi] else { continue };
            // only files that actually access the member need a checker run
            let accesses = |t: usize| f.toks[t].ident() == Some(member) && f.prev(t).is_some_and(|p| p.is(&TokenKind::Dot) || p.is(&TokenKind::ColonColon));
            if !(0..f.toks.len()).any(accesses) {
                continue;
            }
            let mut numbered = ast.clone();
            tinox_parser::assign_node_ids(&mut numbered);
            let preludes: Vec<&SourceFile> = parsed.iter().enumerate().filter(|(i, _)| *i != fi).filter_map(|(_, a)| a.as_ref()).collect();
            let facts = tinox_typecheck::semantic_facts_with_prelude(&numbered, &preludes);
                let byte_of = char_to_byte(&f.text);
            let at = |off: u32| byte_of.get(off as usize).copied();
            // The checker records the span of the RECEIVER expression; the member name is the first
            // `.member` (or `::member`) token after it.
            let member_after = |end: usize, call: bool| -> Option<usize> {
                (0..f.toks.len()).find(|&t| f.toks[t].start >= end && accesses(t) && (!call || f.toks.get(t + 1).is_some_and(|n| n.is(&TokenKind::LParen))))
            };
            for call in facts.calls.values() {
                let (Some(owner), Some(symbol)) = (&call.owner, &call.symbol) else { continue };
                if !symbol.ends_with(&format!("_{member}")) {
                    continue;
                }
                let Some(end) = at(call.span.end.offset) else { continue };
                if let Some(t) = member_after(end, true) {
                    owners.insert((f.uri.clone(), f.toks[t].start), owner.clone());
                }
            }
            for field in facts.fields.values() {
                if field.name != member {
                    continue;
                }
                let ValueType::Named(class, _) = &field.owner else { continue };
                let Some(end) = at(field.span.end.offset) else { continue };
                if let Some(t) = member_after(end, false) {
                    owners.insert((f.uri.clone(), f.toks[t].start), class.clone());
                }
            }
        }
        Checked { owners, parents }
    }

    pub(crate) fn owner_of(&self, file: &Url, tok_start: usize) -> Option<&String> {
        self.owners.get(&(file.clone(), tok_start))
    }

    /// `class` followed by all its ancestors (extends and implements, transitively).
    pub(crate) fn chain(&self, class: &str) -> Vec<String> {
        let mut out = vec![class.to_string()];
        let mut seen: HashSet<String> = out.iter().cloned().collect();
        let mut i = 0;
        while i < out.len() {
            for p in self.parents.get(&out[i]).into_iter().flatten() {
                if seen.insert(p.clone()) {
                    out.push(p.clone());
                }
            }
            i += 1;
        }
        out
    }

    /// Same class, or one is an ancestor of the other (an override or an upcast reaches the same member).
    pub(crate) fn related(&self, a: &str, b: &str) -> bool {
        a == b || self.chain(a).iter().any(|c| c == b) || self.chain(b).iter().any(|c| c == a)
    }
}

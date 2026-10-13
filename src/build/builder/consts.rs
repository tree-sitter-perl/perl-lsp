//! The constant-fold table, keyed by what a name resolves to rather than
//! how it is spelled: `my $m` in two subs, `our @ISA` in two packages, and
//! `use constant M` in two packages are distinct entries. Reads are
//! positional (the latest write at or before the read), so a post-walk
//! reader sees the value its own site saw, not the walk's last write.
//!
//! Interim: the value lane (`docs/prompt-cfg-tier.md`, `Place` +
//! `JoinFold`) replaces this table. Until then a read sees the latest
//! write in source order, with no branch or loop joins.

use super::*;
use crate::model::conventions::{split_qualified, strip_variable_sigils, PERL_SPELLINGS};

/// What a folded name denotes. Built only by `Builder::const_key`, so a
/// raw spelling can never key the table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConstKey {
    /// A `my` / `state` / loop variable: its declaring symbol.
    Lexical(SymbolId),
    /// A package variable or `use constant` name: `(package, sigil+name)`.
    /// `our @ISA` under `package B` and `@B::ISA` are the same key.
    Package(String, String),
}

/// The spelling every key of a name shares (`$Foo::x` and `$x` → `$x`).
fn base_spelling(name: &str) -> String {
    let bare = strip_variable_sigils(name);
    let sigil = &name[..name.len() - bare.len()];
    format!("{sigil}{}", split_qualified(bare, &PERL_SPELLINGS).1)
}

struct Write {
    at: Point,
    /// `None`: a write the fold can't name.
    values: Option<Vec<String>>,
    /// Rename provenance for a scalar bound to exactly one string literal
    /// (`my $m = 'process'`): the literal's content span, stamped as
    /// `Ref.folded_from` on calls folded through it (rule #9).
    source: Option<Span>,
}

#[derive(Default)]
pub(super) struct ConstTable {
    writes: std::collections::HashMap<ConstKey, Vec<Write>>,
    /// Base spellings that ever held a value. A read of any other name
    /// answers `None` without resolving its key, which keeps the scope
    /// lookup off the common miss (`$self`, `$class`, …).
    written: std::collections::HashSet<String>,
}

impl ConstTable {
    /// The write a read at `at` sees: the latest at or before it, ties to
    /// the later write.
    fn visible(&self, key: &ConstKey, at: Point) -> Option<&Write> {
        self.writes
            .get(key)?
            .iter()
            .filter(|w| w.at <= at)
            .fold(None, |best: Option<&Write>, w| match best {
                Some(b) if b.at > w.at => Some(b),
                _ => Some(w),
            })
    }
}

impl<'a> Builder<'a> {
    /// Resolve a name as written at `at`. A sigiled unqualified name binds
    /// to the innermost declaration visible there, by the rule
    /// `resolve_variable_refs` applies post-walk; an `our` declaration, an
    /// undeclared name, a qualified name and a bareword constant are
    /// package entries.
    fn const_key(&self, name: &str, at: Point) -> ConstKey {
        let bare = strip_variable_sigils(name);
        let sigil = &name[..name.len() - bare.len()];
        if let (Some(pkg), base) = split_qualified(bare, &PERL_SPELLINGS) {
            // `$::x` is `$main::x`.
            let pkg = if pkg.is_empty() { "main" } else { pkg };
            return ConstKey::Package(pkg.to_string(), format!("{sigil}{base}"));
        }
        let use_pkg = self.package_at_pos(at);
        if !sigil.is_empty() {
            if let Some(sym) = self.visible_variable_decl(name, at, use_pkg) {
                let sym = &self.symbols[sym.0 as usize];
                return match super::fold::our_gate(sym) {
                    Some(pkg) => ConstKey::Package(pkg.to_string(), name.to_string()),
                    None => ConstKey::Lexical(sym.id),
                };
            }
        }
        ConstKey::Package(use_pkg.unwrap_or("main").to_string(), name.to_string())
    }

    /// The innermost declaration of `name` visible at `at`: the scope
    /// chain innermost-out, the latest declaration at or before `at`.
    fn visible_variable_decl(&self, name: &str, at: Point, use_pkg: Option<&str>) -> Option<SymbolId> {
        let mut scope = Some(self.scope_at_point(at));
        while let Some(sid) = scope {
            if let Some(decls) = self.var_decls.get(&(sid, name.to_string())) {
                let hit = decls.iter().rev().find(|&&id| {
                    let sym = &self.symbols[id.0 as usize];
                    sym.span.start <= at
                        && super::fold::our_gate_admits(super::fold::our_gate(sym), use_pkg)
                });
                if let Some(&id) = hit {
                    return Some(id);
                }
            }
            scope = self.scopes[sid.0 as usize].parent;
        }
        None
    }

    fn visible_constant(&self, name: &str, at: Point) -> Option<&Write> {
        if !self.consts.written.contains(&base_spelling(name)) {
            return None;
        }
        self.consts.visible(&self.const_key(name, at), at)
    }

    /// The constant string values `name` holds where `read` is.
    pub(super) fn resolve_constant_strings(&self, name: &str, read: Node<'_>) -> Option<Vec<String>> {
        self.visible_constant(name, read.start_position())?.values.clone()
    }

    /// The source literal of a single-literal fold of `name` where `read` is.
    pub(super) fn constant_string_source(&self, name: &str, read: Node<'_>) -> Option<Span> {
        self.visible_constant(name, read.start_position())?.source
    }

    /// The one write: after `write` (the node the write belongs to), `name`
    /// folds to `values`. `None` (or an empty list) is a write the fold
    /// can't name, which clears it.
    pub(super) fn write_constant(&mut self, name: &str, write: Node<'_>, values: Option<Vec<String>>) {
        self.push_constant(name, write.end_position(), values, None);
    }

    /// `write_constant` for a scalar bound to exactly one string literal,
    /// keeping the literal's span for rename.
    pub(super) fn write_constant_literal(&mut self, name: &str, write: Node<'_>, value: String, source: Span) {
        self.push_constant(name, write.end_position(), Some(vec![value]), Some(source));
    }

    /// Bind the topic `$_` to `values` for the extent of `node`; the outer
    /// topic resumes at its end.
    pub(super) fn with_topic(&mut self, node: Node<'_>, values: Vec<String>) {
        let outer = self.resolve_constant_strings("$_", node);
        self.push_constant("$_", node.start_position(), Some(values), None);
        self.push_constant("$_", node.end_position(), outer, None);
    }

    fn push_constant(&mut self, name: &str, at: Point, values: Option<Vec<String>>, source: Option<Span>) {
        let values = values.filter(|v| !v.is_empty());
        let spelling = base_spelling(name);
        if values.is_some() {
            self.consts.written.insert(spelling);
        } else if !self.consts.written.contains(&spelling) {
            // Clearing a name that never held a value changes no read.
            return;
        }
        let key = self.const_key(name, at);
        self.consts.writes.entry(key).or_default().push(Write { at, values, source });
    }
}

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
use crate::model::file_analysis::Binding;
use crate::model::conventions::{name_match_key, split_qualified, split_qualified_var, PERL_SPELLINGS};

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
    writes: std::collections::HashMap<Binding, Vec<Write>>,
    /// Base spellings that ever held a value. A read of any other name
    /// answers `None` without resolving its key, which keeps the scope
    /// lookup off the common miss (`$self`, `$class`, …).
    written: std::collections::HashSet<String>,
}

impl ConstTable {
    /// The write a read at `at` sees: the latest at or before it, ties to
    /// the later write.
    fn visible(&self, key: &Binding, at: Point) -> Option<&Write> {
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
    /// `name` as written at `at`, from any phase: the scope is found by
    /// position.
    fn const_key(&self, name: &str, at: Point) -> Binding {
        self.binding_key_in(name, at, self.scope_at_point(at))
    }

    /// `name` as written at `at` during the walk, where the scope in force
    /// is in hand (`scope_at_point` scans every scope).
    pub(super) fn binding_key_here(&self, name: &str, at: Point) -> Binding {
        self.binding_key_in(name, at, self.current_scope())
    }

    /// Resolve a name as written at `at` in `scope`. A sigiled unqualified
    /// name binds to the innermost declaration visible there, by the rule
    /// `resolve_variable_refs` applies post-walk; an `our` declaration, an
    /// undeclared name, a qualified name and a bareword constant are
    /// package entries.
    pub(super) fn binding_key_in(&self, name: &str, at: Point, scope: ScopeId) -> Binding {
        if let Some((pkg, base)) = split_qualified_var(name, &PERL_SPELLINGS) {
            return Binding::Package(pkg.to_string(), base);
        }
        if let (Some(pkg), base) = split_qualified(name, &PERL_SPELLINGS) {
            return Binding::Package(PERL_SPELLINGS.qualifier_package(pkg).to_string(), base.to_string());
        }
        let use_pkg = self.package_at_pos(at);
        if name.starts_with(|c| PERL_SPELLINGS.is_sigil(c)) {
            if let Some(sym) = self.visible_variable_decl(name, at, scope, use_pkg) {
                let sym = &self.symbols[sym.0 as usize];
                return match super::fold::our_gate(sym) {
                    Some(pkg) => Binding::Package(pkg.to_string(), name.to_string()),
                    None => Binding::Lexical(sym.id),
                };
            }
        }
        Binding::Package(use_pkg.unwrap_or("main").to_string(), name.to_string())
    }

    /// The innermost declaration of `name` visible at `at`: the scope
    /// chain innermost-out, the latest declaration at or before `at`.
    fn visible_variable_decl(&self, name: &str, at: Point, scope: ScopeId, use_pkg: Option<&str>) -> Option<SymbolId> {
        let by_scope = self.var_decls.get(name)?;
        for sid in crate::model::file_analysis::lookup_chain(&self.scopes, scope).scopes() {
            if let Some(decls) = by_scope.get(&sid) {
                let hit = decls.iter().rev().find(|&&id| {
                    let sym = &self.symbols[id.0 as usize];
                    sym.span.start <= at
                        && super::fold::our_gate_admits(super::fold::our_gate(sym), use_pkg)
                });
                if let Some(&id) = hit {
                    return Some(id);
                }
            }
        }
        None
    }

    fn visible_constant(&self, name: &str, at: Point) -> Option<&Write> {
        if !self.consts.written.contains(&name_match_key(name, &PERL_SPELLINGS)) {
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
        let spelling = name_match_key(name, &PERL_SPELLINGS);
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

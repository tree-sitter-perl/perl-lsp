//! The member family: every finding that hangs off resolving a member
//! access or a call — undefined member, access control, arity, the widened
//! answer, and a deprecated callee. One walk over the refs, because the
//! ladder resolves each site once and every one of these reads that result.

use super::*;

impl FileAnalysis {
    /// A deprecated declaration's notice: `Some(text)` when the declaration
    /// carries the flag (the text itself may be absent), `None` otherwise.
    pub(crate) fn deprecation_of(sym: &Symbol) -> Option<Option<String>> {
        sym.flags
            .contains(SymbolFlags::DEPRECATED)
            .then(|| sym.presentation.deprecation.clone())
    }

    /// Every finding a member site produces. Precision first: each silence
    /// rule is named where it applies, and the whole lane stands down for a
    /// class whose ancestry this workspace cannot read.
    pub fn member_findings(&self, facts: &LaneFacts<'_>) -> Vec<Finding> {
        let idx = facts.idx;
        let mut out = Vec::new();
        // Every per-class fact is derived ONCE per class, never per ref: a
        // 10k-line class file has thousands of member calls on a handful of
        // classes, and a symbol scan per call is quadratic.
        let local_classes: std::collections::HashSet<&str> = self
            .symbols()
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Class | SymKind::Package))
            .map(|s| s.name.as_str())
            .collect();
        // The members this language declares by WRITING them, per class. A
        // write is a ref fact, so it stays a per-file set — but resolving
        // each write's invocant is work the member loop already pays, so it
        // is derived once, on the first lane that asks.
        let mut written: Option<HashMap<String, std::collections::HashSet<String>>> = None;
        // The class's DEFINING analysis plus the facts every lane asks of
        // it, memoized per class. `None` = a class the lanes stay silent on.
        struct OwnerFacts {
            owner: Option<std::sync::Arc<FileAnalysis>>,
            is_interface: bool,
            /// A trait's receiver is whatever class composes it: every
            /// member it does not declare may live there.
            is_trait: bool,
        }
        // keyed by (leaf, the namespace the CALL means): a `parent::` call's
        // parent is the parent-namespace row of the class it is written in —
        // an aliased parent carrying the child's own leaf is not the child
        let mut owner_memo: HashMap<(String, Option<String>), Option<OwnerFacts>> = HashMap::new();
        // Template method: a receiver call in a base whose SUBCLASS declares
        // the member dispatches on the runtime class, which is that
        // subclass. One graph walk per (class, member, kind).
        let mut below_memo: HashMap<(String, String, bool), bool> = HashMap::new();

        for r in self.refs() {
            let Some(site) = r.member_site() else { continue };
            // a member site always states its family
            let Some(want) = MemberKind::of_ref(&r.kind) else { continue };
            let invocant = site.invocant;
            // only a call can be named by a string (`[$obj, 'name']`)
            let named_by_string = matches!(r.kind, RefKind::MethodCall { named_by_string: true, .. });
            // The method TOKEN the model spells: a `parent::` call is minted
            // on the model's SUPER lane (`SUPER::m`), whose qualifier is the
            // model's own spelling and not the language's namespace
            // separator, so splitting on that separator would leave the
            // qualifier on the name and every `parent::` call read undefined.
            let name =
                crate::model::conventions::MethodToken::parse(r.unqualified_target_name(self.names()))
                    .name();
            // a member named by a variable, and a sigil is a variable only
            // where the language declares one
            if name.is_empty() || name.chars().next().is_some_and(|c| self.names().is_sigil(c)) {
                continue;
            }
            let class_literal = self.spellings().class_literal_member;
            if !class_literal.is_empty() && name == class_literal {
                continue; // the class-name literal, never a member
            }
            // The dispatch projection every verb reads (the receiver is a
            // typed one here — the extractor witnesses it at the class body).
            let Some(class) = self.method_call_invocant_class(r, idx) else { continue };
            // The receiver is an identity: its own namespace names the
            // class's defining candidate (a pack without namespaces makes no
            // claim).
            let want_ns = self.identity_namespace(&class);
            let facts_for_class = owner_memo.entry((class.clone(), want_ns.clone())).or_insert_with(|| {
                // The class's DEFINING analysis: this file, or — once the
                // index is settled — the candidate its namespace names. An
                // unsettled index would flag every cross-file member.
                let is_local = local_classes.contains(class.as_str())
                    && (want_ns.is_none() || self.declared_class_namespace(&class) == want_ns);
                let owner_arc: Option<std::sync::Arc<FileAnalysis>> = if is_local {
                    None
                } else {
                    let i = facts.settled_lookup()?;
                    Some(i.defining_analysis(&class, &|a| {
                        let declared = a.declared_class_namespace(&class);
                        declared.is_some() && (want_ns.is_none() || declared == want_ns)
                    })?)
                };
                let owner: &FileAnalysis = owner_arc.as_deref().unwrap_or(self);
                let owner_has_members = owner.symbols().iter().any(|s| {
                    matches!(s.kind, SymKind::Sub | SymKind::Method | SymKind::Field)
                        && s.package.as_deref() == Some(class.as_str())
                });
                if !owner_has_members
                    || owner.class_answers_any_member(&class, idx)
                    || !owner.ancestry_fully_visible(&class, idx)
                {
                    return None;
                }
                let flavors = owner.class_flags(&class);
                Some(OwnerFacts {
                    // A receiver typed as an INTERFACE names any
                    // implementation. `instanceof` narrowing retypes a
                    // VARIABLE receiver, but a member subject, a method guard
                    // or an `is_a()` leave the interface type standing — so
                    // the interface stays silent on undefined members
                    // (resolved ones still check arity).
                    is_interface: flavors.contains(SymbolFlags::INTERFACE),
                    is_trait: flavors.contains(SymbolFlags::TRAIT),
                    owner: owner_arc,
                })
            });
            let Some(class_facts) = facts_for_class.as_ref() else { continue };
            let owner: &FileAnalysis = class_facts.owner.as_deref().unwrap_or(self);
            match owner.resolve_member(&class, name, want, idx) {
                None if class_facts.is_interface || class_facts.is_trait => {}
                // a class with no declared constructor has the default one
                None if facts.is_constructor_name(name) => {}
                None if named_by_string => {
                    // `[$obj, 'name']` is data until dispatch proves it a
                    // callable: a claim only when it resolves
                }
                None => {
                    // a language that declares a property by writing it: a
                    // write of this member on the same class anywhere in the
                    // file is its declaration
                    if matches!(want, MemberKind::Value) {
                        let writes = written.get_or_insert_with(|| {
                            let mut by_class: HashMap<String, std::collections::HashSet<String>> =
                                HashMap::new();
                            for w in self.refs() {
                                if w.member_site().is_none()
                                    || !matches!(w.access, AccessKind::Write)
                                {
                                    continue;
                                }
                                if let Some(c) = self.method_call_invocant_class(w, idx) {
                                    by_class
                                        .entry(c)
                                        .or_default()
                                        .insert(w.unqualified_target_name(self.names()).to_string());
                                }
                            }
                            by_class
                        });
                        if writes.get(&class).is_some_and(|m| m.contains(name)) {
                            continue;
                        }
                    }
                    // a read inside an existence probe (`isset($x->p)`) IS
                    // the question of whether the member exists
                    if matches!(want, MemberKind::Value)
                        && self.pack.probe_regions.iter().any(|p| p.contains(&r.span))
                    {
                        continue;
                    }
                    // the receiver is the language's own: the runtime class
                    // may be any descendant, and one of them declares it
                    if facts.is_receiver_token(invocant.text()) {
                        let declared_below = *below_memo
                            .entry((class.clone(), name.to_string(), matches!(want, MemberKind::Value)))
                            .or_insert_with(|| {
                                owner
                                    .dispatch_participants(&class, idx)
                                    .iter()
                                    .filter(|p| **p != class)
                                    .any(|p| owner.resolve_member(p, name, want, idx).is_some())
                            });
                        if declared_below {
                            continue;
                        }
                    }
                    // a same-named member of the OTHER kind is a different
                    // finding (a method read as a property) — still undefined
                    out.push(Finding::new(
                        r.span,
                        match want {
                            MemberKind::Value => codes::UNDEFINED_PROPERTY,
                            _ => codes::UNRESOLVED_METHOD,
                        },
                        FindingData::UndefinedMember { kind: want, name: name.to_string() },
                    ));
                }
                Some(MethodResolution::Local { sym_id, .. }) => {
                    let sym = owner.symbol(sym_id);
                    if let Some(note) = FileAnalysis::deprecation_of(sym) {
                        out.push(Finding::new(
                            r.span,
                            codes::DEPRECATED,
                            FindingData::Deprecated { name: name.to_string(), note },
                        ));
                    }
                    // non-public member reached from outside its class —
                    // unless from inside a closure, whose receiver may be
                    // rebound to the owner (the private-access idiom tests
                    // live on)
                    let in_closure = self
                        .scope_chain(r.scope)
                        .into_iter()
                        .filter_map(|sc| self.scope(sc).owner)
                        .any(|sid| self.symbol(sid).flags.contains(SymbolFlags::ANONYMOUS));
                    // a property READ that resolved only to a same-named
                    // METHOD (or the reverse) is not an access violation
                    let kind_agrees = MemberKind::of_sym(sym.kind) == want;
                    if kind_agrees && !in_closure && sym.flags.contains(SymbolFlags::NON_PUBLIC) {
                        let from = self.enclosing_class_for_scope(r.scope);
                        let owner_class = sym.package.clone().unwrap_or_default();
                        if from.as_deref() != Some(owner_class.as_str())
                            && !from.as_deref().is_some_and(|f| self.class_isa(f, &owner_class, idx))
                        {
                            out.push(Finding::new(
                                r.span,
                                codes::NON_PUBLIC_ACCESS,
                                FindingData::NonPublicAccess {
                                    name: name.to_string(),
                                    owner: owner_class,
                                    from,
                                },
                            ));
                        }
                    }
                    out.extend(arity_findings(r, sym));
                }
                Some(MethodResolution::CrossFile { class: on, widened: true, .. }) => {
                    // The answer stands (goto-def, hover and completion all
                    // serve it), but it is confidently wrong whenever the
                    // code is — an import not yet written, a namespace typo —
                    // so the widening is a finding of its own.
                    out.push(Finding::new(
                        r.span,
                        codes::RESOLVED_BY_WIDENING,
                        FindingData::ResolvedByWidening {
                            name: name.to_string(),
                            on,
                            wanted: class.clone(),
                        },
                    ));
                }
                Some(MethodResolution::CrossFile { .. }) => {}
            }
        }
        out
    }

    /// Arity on plain calls, against callees this file declares.
    ///
    /// A construction mints its constructor call, so `new Foo(...)` is
    /// checked by the member lane — where a class declaring no constructor
    /// is silent, and the default constructor takes any argument list by not
    /// resolving.
    pub fn call_arity_findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        for r in self.refs() {
            if !matches!(r.kind, RefKind::FunctionCall) {
                continue;
            }
            if r.arg_count.is_none() {
                continue;
            }
            let name = r.unqualified_target_name(self.names());
            let Some(sym) = self
                .symbols_named(name)
                .iter()
                .map(|&sid| self.symbol(sid))
                .find(|s| matches!(s.kind, SymKind::Sub))
            else {
                continue;
            };
            out.extend(arity_findings(r, sym));
        }
        out
    }
}

/// The written argument count against a callable's declared list. A callee
/// that reads arguments it never declared (`func_get_args`) accepts any
/// count, and a call whose count the site could not state asks nothing.
fn arity_findings(r: &Ref, sym: &Symbol) -> Option<Finding> {
    let (n, a) = (r.arg_count?, sym.arity?);
    if sym.flags.contains(SymbolFlags::DYNAMIC_ARGS) {
        return None;
    }
    if n < a.required {
        return Some(Finding::new(
            r.span,
            codes::ARITY_MISMATCH,
            FindingData::TooFewArguments { expected: a.required, found: n },
        ));
    }
    if !a.variadic && n > a.total {
        return Some(Finding::new(
            r.span,
            codes::ARITY_MISMATCH,
            FindingData::TooManyArguments { expected: a.total, found: n },
        ));
    }
    None
}

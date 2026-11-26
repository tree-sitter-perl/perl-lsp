//! The one call → callee binding: which callable a call runs, and the
//! owner of the keys it passes by name. Every consumer of a call's named
//! arguments — a key's owner, goto-def, references, rename, label
//! completion — reads this answer instead of resolving the callee itself.

use super::*;
use std::sync::Arc;

/// The callable a call runs.
#[derive(Debug, Clone)]
pub struct Callee {
    /// The owner of the keys the call passes by name. A method call's
    /// keys are named on its receiver's class; consumers widen through
    /// the MRO from there, as a construction's keys widen to every class
    /// it inherits a constructor key from.
    pub input: HashKeyOwner,
    /// The declaration that answered, when one did; `None` when the
    /// callable is known only by name (a framework constructor nothing
    /// indexed declares).
    pub at: Option<CalleeAt>,
    /// The rule for the parameter the call's receiver fills, for a member
    /// call; `None` for a call with no receiver.
    pub receiver: Option<ReceiverParam>,
}

/// Where a callee is declared.
#[derive(Debug, Clone)]
pub enum CalleeAt {
    /// In the asking file.
    Here(SymbolId),
    /// In another file, at the declaring symbol's selection span.
    There { module: Arc<CachedModule>, span: Span },
}

impl Callee {
    /// The declaring symbol, read from the file that holds it.
    pub fn symbol(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Option<Symbol> {
        match self.at.as_ref()? {
            CalleeAt::Here(id) => Some(origin.symbol(*id).clone()),
            CalleeAt::There { module, span } => index?
                .symbols_present(module)
                .symbols()
                .iter()
                .find(|s| s.selection_span == *span && matches!(s.detail, SymbolDetail::Sub { .. } | SymbolDetail::Handler { .. }))
                .cloned(),
        }
    }

    /// The owners the call's keys may be declared under: the declared
    /// callable's own input first — an inherited method's keys are its
    /// declaring class's — then the input the call names, under which a
    /// framework declares a class's constructor keys.
    pub fn owners(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>, place: Option<u32>) -> Vec<HashKeyOwner> {
        let slot = self.slot_at(origin, index, place);
        let declared = match self.at.as_ref() {
            Some(CalleeAt::Here(id)) => Some(HashKeyOwner::input_of(origin.symbol(*id), &origin.namespace_ranges)),
            Some(CalleeAt::There { module, span }) => index.and_then(|idx| {
                let fa = idx.symbols_present(module);
                let sym = fa.symbols().iter().find(|s| s.selection_span == *span)?;
                Some(HashKeyOwner::input_of(sym, &fa.namespace_ranges))
            }),
            None => None,
        };
        let mut out: Vec<HashKeyOwner> = declared.into_iter().map(|o| o.with_slot(slot)).collect();
        let input = self.input.clone().with_slot(slot);
        if !out.contains(&input) {
            out.push(input);
        }
        out
    }

    /// The owner of a key the call passes: its names, or for a key in the
    /// hash literal at `place`, the parameter that argument binds.
    pub fn input_at(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>, place: Option<u32>) -> HashKeyOwner {
        self.input.clone().with_slot(self.slot_at(origin, index, place))
    }

    /// The keys a call may write in the hash literal it passes at
    /// `place`, or by name for `None`: the parameter's keys where that
    /// argument binds one, else the call's [`labels`](Self::labels).
    pub fn labels_at(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>, place: Option<u32>) -> Vec<String> {
        if self.slot_at(origin, index, place) == InputSlot::Named {
            return self.labels(origin, index);
        }
        let mut out: Vec<String> = Vec::new();
        for (_, def) in self.key_defs(origin, index, place) {
            if !out.contains(&def.name) {
                out.push(def.name);
            }
        }
        out
    }

    /// How many written arguments the callee takes by place before the
    /// names it collects.
    pub fn named_offset(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> usize {
        match self.symbol(origin, index).map(|s| s.detail) {
            Some(SymbolDetail::Sub { params, .. } | SymbolDetail::Handler { params, .. }) => named_offset(&params, self.receiver),
            _ => 0,
        }
    }

    fn slot_at(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>, place: Option<u32>) -> InputSlot {
        let Some(place) = place else { return InputSlot::Named };
        match self.symbol(origin, index).map(|s| s.detail) {
            Some(SymbolDetail::Sub { params, .. } | SymbolDetail::Handler { params, .. }) => slot_at_place(&params, self.receiver, place),
            _ => InputSlot::Named,
        }
    }

    /// The files whose key declarations the call's keys may name, the
    /// callee's own first: `None` is the asking file. The class the call
    /// names reaches its files too.
    pub fn key_homes(&self, index: Option<&dyn CrossFileLookup>) -> Vec<Option<Arc<CachedModule>>> {
        let mut homes = match &self.at {
            Some(CalleeAt::There { module, .. }) => vec![Some(module.clone()), None],
            _ => vec![None],
        };
        if let (Some(idx), HashKeyOwner::Input { package: Some(p), .. }) = (index, &self.input) {
            for c in idx.visible_def_candidates(p) {
                if !homes.iter().flatten().any(|h| h.path == c.path) {
                    homes.push(Some(c));
                }
            }
        }
        homes
    }

    /// Every key declaration the call's keys may name, with the file
    /// holding it (`None` the asking file), the callee's own first.
    pub fn key_defs(
        &self,
        origin: &FileAnalysis,
        index: Option<&dyn CrossFileLookup>,
        place: Option<u32>,
    ) -> Vec<(Option<Arc<CachedModule>>, Symbol)> {
        let owners = self.owners(origin, index, place);
        let mut out = Vec::new();
        for home in self.key_homes(index) {
            let fa = match (&home, index) {
                (Some(m), Some(idx)) => idx.symbols_present(m),
                (None, _) => {
                    out.extend(owners.iter().flat_map(|o| origin.hash_key_defs_for_owner(o)).map(|d| (None, d.clone())));
                    continue;
                }
                (Some(_), None) => continue,
            };
            out.extend(owners.iter().flat_map(|o| fa.hash_key_defs_for_owner(o)).map(|d| (home.clone(), d.clone())));
        }
        out
    }

    /// The keys a call may pass, in declaration order: each parameter
    /// the callee takes by name, then each key its owners declare that
    /// no parameter refuses — a key a collecting parameter reads, a
    /// constructor key a framework declares. A parameter taken only by
    /// place mints a key no call may pass.
    pub fn labels(&self, origin: &FileAnalysis, index: Option<&dyn CrossFileLookup>) -> Vec<String> {
        let sym = self.symbol(origin, index);
        let params: &[ParamInfo] = match sym.as_ref().map(|s| &s.detail) {
            Some(SymbolDetail::Sub { params, .. } | SymbolDetail::Handler { params, .. }) => params,
            _ => &[],
        };
        let mut out: Vec<String> = params.iter().filter_map(ParamInfo::label).map(str::to_string).collect();
        // A parameter taken only by place is spelled as the key it mints.
        let refused = |k: &str| params.iter().any(|p| p.label().is_none() && !p.pass.is_rest() && p.name == k);
        for (_, def) in self.key_defs(origin, index, None) {
            if !refused(&def.name) && !out.contains(&def.name) {
                out.push(def.name);
            }
        }
        out
    }
}

impl FileAnalysis {
    /// The callable `call` runs, and the owner of the keys it passes by
    /// name. A call of a name that constructs a class runs its
    /// constructor; a free function call runs the declaration its name
    /// resolves to — this file's, an imported one under its own name, or
    /// the one the index files under the name's identity; a member call
    /// runs what its receiver's MRO dispatches it to. `None` when the
    /// call has no single callee (a union receiver reaches one per arm).
    pub fn callee_of(&self, call: &Ref, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        // A value call reaches the callable its operand holds; so does a
        // call by a name that holds one (python's `g = lambda …; g(…)`).
        if matches!(call.kind, RefKind::Variable) {
            return self.held_callee(call, index);
        }
        // A dispatch call runs the handler registered under its name.
        if let (RefKind::DispatchCall { .. }, Some(owner)) = (&call.kind, call.handler_owner()) {
            return Some(self.handler_callee(owner, &call.target_name, index));
        }
        if !matches!(call.kind, RefKind::FunctionCall) {
            let class = self.receiver_classes(call, index, ReceiverPolicy::Edge).single()?.to_string();
            let receiver = Some(self.names().receiver);
            return self.member_callee(call, &class, index).map(|c| Callee { receiver, ..c });
        }
        if let Some(held) = self.held_callee(call, index) {
            return Some(held);
        }
        if let Some(class) = self.class_named_by_call(call, index) {
            return self.constructor_callee(&class, index);
        }
        if self.names().use_map_sep().is_some() {
            if let Some(sym) = self.free_name_symbol(call) {
                return Some(self.callee_here(sym));
            }
            if let (Some(idx), Some(table)) = (index, call.free_name_table()) {
                let (decls, _) = callee_declarations(idx, table, &self.free_name_identities(call), &self.pack.names);
                if let Some(d) = decls.into_iter().next() {
                    return Some(callee_there(idx, d.module, d.span));
                }
            }
        }
        let name = call.unqualified_target_name(self.names());
        // A qualified token names its package; a bare one is this file's
        // sub, else what an import binds the name to — under the name the
        // exporter declares it by, which `-as` may differ from.
        let qualified = split_qualified(&call.target_name, self.names()).0.is_some();
        if let (true, Some(package)) = (qualified, call.resolved_package()) {
            return Some(self.packaged_callee(package, name, index));
        }
        if let Some(sym) = self
            .symbols_named(name)
            .iter()
            .map(|&sid| self.symbol(sid))
            .find(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        {
            return Some(self.callee_here(sym));
        }
        if let Some((module, remote)) = index.and_then(|idx| self.imported_callable(name, call.span.start, idx)) {
            return Some(self.packaged_callee(&module, &remote, index));
        }
        if let Some(package) = call.resolved_package() {
            return Some(self.packaged_callee(package, name, index));
        }
        Some(Callee { input: HashKeyOwner::named_input(None, name), at: None, receiver: None })
    }

    /// The handler registered on `owner` as `name`: this file's first
    /// registration, else the first another file holds.
    fn handler_callee(&self, owner: &HandlerOwner, name: &str, index: Option<&dyn CrossFileLookup>) -> Callee {
        let input = HashKeyOwner::Handler { owner: Box::new(owner.clone()), name: name.into(), slot: InputSlot::Named };
        let registered = |s: &Symbol| s.name == name && matches!(&s.detail, SymbolDetail::Handler { owner: o, .. } if o == owner);
        // A handler that runs a named callable is called as that callable.
        let runs = |s: &Symbol| match &s.detail {
            SymbolDetail::Handler { runs: Some(runs), .. } => match &**runs {
                HashKeyOwner::Input { package: Some(p), name, .. } => Some((p.to_string(), name.to_string())),
                _ => None,
            },
            _ => None,
        };
        if let Some(sym) = self.symbols.iter().find(|s| registered(s)) {
            if let Some((package, callable)) = runs(sym) {
                return self.packaged_callee(&package, &callable, index);
            }
            return Callee { input, at: Some(CalleeAt::Here(sym.id)), receiver: None };
        }
        let there = index.and_then(|idx| {
            idx.modules_with_symbol(name).into_iter().find_map(|module| {
                idx.visible_def_candidates(&module).into_iter().find_map(|cached| {
                    let found = idx.symbols_present(&cached).symbols().iter().find(|s| registered(s)).map(|s| (s.selection_span, runs(s)))?;
                    Some((cached, found))
                })
            })
        });
        match there {
            Some((_, (_, Some((package, callable))))) => self.packaged_callee(&package, &callable, index),
            Some((module, (span, None))) => Callee { input, at: Some(CalleeAt::There { module, span }), receiver: None },
            None => Callee { input, at: None, receiver: None },
        }
    }

    /// The callable the value named by the call's token holds there.
    fn held_callee(&self, call: &Ref, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        let held = self.inferred_type_via_bag(&call.target_name, call.span.start)?;
        match held.held_callable()? {
            HeldCallable::Symbol(sid) => Some(self.callee_here(self.symbol(sid))),
            HeldCallable::Named { package, name } => Some(self.packaged_callee(package, name, index)),
        }
    }

    /// Every dispatch call that says where it writes its handler's
    /// arguments, with that region.
    pub fn handler_regions(&self) -> impl Iterator<Item = (&Ref, HandlerRegion)> + '_ {
        self.refs.iter().filter_map(|r| match &r.kind {
            RefKind::DispatchCall { args: Some(region), .. } => Some((r, *region)),
            _ => None,
        })
    }

    /// The callee of the call a key is passed to.
    pub fn arg_key_callee(&self, key: &Ref, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        let crate::model::witnesses::RefIdx(i) = key.arg_key_call()?;
        self.callee_of(self.refs.get(i as usize)?, index)
    }

    /// The callee of the call whose callee token starts at `at` — a
    /// cursor-time site that names its call by position.
    pub fn callee_at(&self, at: Point, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        let call = self.refs().iter().find(|r| match &r.kind {
            RefKind::FunctionCall | RefKind::Variable => r.span.start == at,
            RefKind::MethodCall { method_name_span, .. } => method_name_span.start == at || r.span.start == at,
            _ => false,
        })?;
        self.callee_of(call, index)
    }

    fn callee_here(&self, sym: &Symbol) -> Callee {
        Callee { input: HashKeyOwner::input_of(sym, &self.namespace_ranges), at: Some(CalleeAt::Here(sym.id)), receiver: None }
    }

    /// `package`'s `name`: declared here, else in the candidate file the
    /// index says defines it.
    fn packaged_callee(&self, package: &str, name: &str, index: Option<&dyn CrossFileLookup>) -> Callee {
        if let Some(sym) = self.symbols_named(name).iter().map(|&sid| self.symbol(sid)).find(|s| {
            matches!(s.kind, SymKind::Sub | SymKind::Method) && s.package.as_deref() == Some(package)
        }) {
            return self.callee_here(sym);
        }
        let there = index.and_then(|idx| {
            let module = idx.defining_module_cached(package, name).or_else(|| idx.candidate_defining_sub(package, name))?;
            let fa = idx.symbols_present(&module);
            let sym = fa
                .symbols()
                .iter()
                .filter(|s| s.name == name && matches!(s.detail, SymbolDetail::Sub { .. }))
                .min_by_key(|s| s.package.as_deref() != Some(package))?;
            Some((sym.package.clone(), CalleeAt::There { module: module.clone(), span: sym.selection_span }))
        });
        match there {
            // The callable's own package owns its keys: a re-exported
            // sub's is the module that declares it.
            Some((declared, at)) => Callee { input: HashKeyOwner::named_input(declared, name), at: Some(at), receiver: None },
            None => Callee { input: HashKeyOwner::named_input(Some(package.to_string()), name), at: None, receiver: None },
        }
    }

    /// The member `call` runs on `class`.
    fn member_callee(&self, call: &Ref, class: &str, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        let member = self.called_member(call, class, index)?.into_owned();
        let at = self.member_at(class, &member, index);
        Some(Callee { input: HashKeyOwner::named_input(Some(class.to_string()), member), at, receiver: None })
    }

    /// The constructor a construction of `class` runs.
    fn constructor_callee(&self, class: &str, index: Option<&dyn CrossFileLookup>) -> Option<Callee> {
        let (_, member) = self.constructor_of(class, index)?;
        let at = self.member_at(class, &member, index);
        Some(Callee { input: HashKeyOwner::named_input(Some(class.to_string()), member), at, receiver: None })
    }

    fn member_at(&self, class: &str, member: &str, index: Option<&dyn CrossFileLookup>) -> Option<CalleeAt> {
        match self.resolve_method_in_ancestors(class, member, index)? {
            MethodResolution::Local { sym_id, .. } => Some(CalleeAt::Here(sym_id)),
            MethodResolution::CrossFile { class, member, def_module, .. } => {
                let idx = index?;
                let module = idx.candidate_defining_sub_in_package(def_module.as_deref().unwrap_or(&class), &class, &member)?;
                let span = idx
                    .symbols_present(&module)
                    .symbols()
                    .iter()
                    .find(|s| {
                        s.name == member
                            && s.package.as_deref() == Some(class.as_str())
                            && matches!(s.detail, SymbolDetail::Sub { .. })
                    })?
                    .selection_span;
                Some(CalleeAt::There { module, span })
            }
        }
    }

    /// The module a bare call of `name` at `at` imports it from, and the
    /// name it is declared under there: the package's own `use`s, later
    /// wins, each binding what `imported_names` says it binds (a bare
    /// `use M` its exports, a `-as` rename its local name to the remote
    /// one). A split exporter's surface lives across its candidates.
    pub(crate) fn imported_callable(&self, name: &str, at: Point, index: &dyn CrossFileLookup) -> Option<(String, String)> {
        self.imports.in_scope(self.import_scope_at(at)).rev().find_map(|import| {
            index.visible_def_candidates(&import.module_name).iter().find_map(|cached| {
                let surface = cached.analysis.export_surface_with_index(&import.module_name, index);
                imported_names(import, &surface)
                    .into_iter()
                    .find(|(local, _)| local == name)
                    .map(|(_, remote)| (import.module_name.clone(), remote))
            })
        })
    }
}

fn callee_there(index: &dyn CrossFileLookup, module: Arc<CachedModule>, span: Span) -> Callee {
    let fa = index.symbols_present(&module);
    let sym = fa.symbols().iter().find(|s| s.selection_span == span);
    let input = sym.map_or_else(|| HashKeyOwner::named_input(None, ""), |s| HashKeyOwner::input_of(s, &fa.namespace_ranges));
    Callee { input, at: Some(CalleeAt::There { module, span }), receiver: None }
}

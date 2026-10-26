//! Readings a question picks between. A site whose meaning waits on other
//! files mints every reading it can have, each tagged with the question
//! and the arm it belongs to; the base tables hold the arm a file alone
//! supports (the question's `Fallback`), and every other arm sits in a
//! dormant slot of its own. A view decides the questions with the index in
//! hand and swaps slots, so every symbol id and ref index is the same in
//! every view (`docs/adr/query-delayed-facts.md`).

use super::*;
use crate::model::witnesses::Witness;

/// The two answers a question can give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arm {
    Then,
    Otherwise,
}

impl Fallback {
    /// The arm an undecided question stands for; `None` for neither.
    pub fn arm(self) -> Option<Arm> {
        match self {
            Fallback::Then => Some(Arm::Then),
            Fallback::Otherwise => Some(Arm::Otherwise),
            Fallback::Neither => None,
        }
    }
}

/// One question this file asks, indexed by its position in
/// `Readings::questions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldQuestion {
    pub question: Question,
    pub fallback: Fallback,
}

/// An entry that is live only when its question picks `arm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Held<T> {
    pub question: u32,
    pub arm: Arm,
    /// The table slot the entry occupies (a symbol's id, a ref's index).
    pub slot: u32,
    pub live: T,
}

/// Every reading this file holds, by question.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Readings {
    pub questions: Vec<HeldQuestion>,
    pub symbols: Vec<Held<Symbol>>,
    pub refs: Vec<Held<Ref>>,
    /// Witnesses a non-fallback arm adds. A reading that must retract a
    /// type the fallback states goes through a `Choose` payload instead,
    /// since a view only appends to the bag.
    pub witnesses: Vec<(u32, Arm, Witness)>,
}

/// Both readings' head refs at one deferred `meth Class(ARGS)` (the
/// builder's `hold_indirect_heads`).
#[derive(Debug, Clone)]
pub struct IndirectHeads {
    pub site: IndirectSite,
    /// `Class->meth(ARGS)`: the `Then` reading.
    pub method: Vec<Ref>,
    /// `meth(Class(ARGS))`: the `Otherwise` reading.
    pub function: Vec<Ref>,
}

/// One pick per question: the arm a view installs, `None` for neither.
pub type Picks = Vec<Option<Arm>>;

/// Where a dormant slot points: past any real position, so no span probe
/// ever lands on it.
const DORMANT_SPAN: Span = Span {
    start: Point { row: usize::MAX, column: 0 },
    end: Point { row: usize::MAX, column: 0 },
};

impl Symbol {
    pub fn dormant(id: SymbolId) -> Symbol {
        Symbol {
            id,
            name: String::new(),
            kind: SymKind::Dormant,
            span: DORMANT_SPAN,
            selection_span: DORMANT_SPAN,
            scope: ScopeId(0),
            package: None,
            detail: SymbolDetail::None,
            namespace: Namespace::default(),
            presentation: Presentation::default(),
            attributes: Vec::new(),
            flags: Default::default(),
            declared_with: None,
            deref_stack: Vec::new(),
            arity: None,
        }
    }
}

impl Ref {
    pub fn dormant() -> Ref {
        Ref {
            kind: RefKind::Dormant,
            span: DORMANT_SPAN,
            scope: ScopeId(0),
            target_name: String::new(),
            access: AccessKind::Read,
            binding: Binding::NONE,
            folded_from: None,
            arg_count: None,
        }
    }

    pub fn is_dormant(&self) -> bool {
        matches!(self.kind, RefKind::Dormant)
    }
}

impl Readings {
    pub fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    fn fallback(&self, q: u32) -> Option<Arm> {
        self.questions[q as usize].fallback.arm()
    }

    /// The picks every base table already shows.
    pub fn fallbacks(&self) -> Picks {
        self.questions.iter().map(|q| q.fallback.arm()).collect()
    }

    /// Every entry's live form, picked or not — what registration lists,
    /// since a name a view can show must be findable before any view asks.
    pub fn live_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().map(|h| &h.live)
    }

    pub fn live_refs(&self) -> impl Iterator<Item = &Ref> {
        self.refs.iter().map(|h| &h.live)
    }
}

impl FileAnalysis {
    /// Ask a question of this file, appending it to the readings. Returns
    /// its index for the entries that follow.
    pub fn ask(&mut self, question: Question) -> u32 {
        let fallback = question.when_unknown();
        self.readings.questions.push(HeldQuestion { question, fallback });
        (self.readings.questions.len() - 1) as u32
    }

    /// Hold a new symbol under `(question, arm)`: a fresh slot, live in the
    /// base only when `arm` is the fallback.
    pub fn hold_symbol(&mut self, question: u32, arm: Arm, mut live: Symbol) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        live.id = id;
        let base = if self.readings.fallback(question) == Some(arm) { live.clone() } else { Symbol::dormant(id) };
        self.symbols.push(base);
        self.readings.symbols.push(Held { question, arm, slot: id.0, live });
        id
    }

    /// Hold a new ref under `(question, arm)`, as `hold_symbol` does.
    pub fn hold_ref(&mut self, question: u32, arm: Arm, live: Ref) {
        let slot = self.refs.len() as u32;
        let base = if self.readings.fallback(question) == Some(arm) { live.clone() } else { Ref::dormant() };
        self.refs.push(base);
        self.readings.refs.push(Held { question, arm, slot, live });
    }

    /// Hold a ref the walk already minted as the fallback's: it stays in
    /// its slot, and a view that picks the other arm empties it.
    pub fn hold_existing_ref(&mut self, question: u32, slot: usize) {
        let Some(arm) = self.readings.fallback(question) else { return };
        let live = self.refs[slot].clone();
        self.readings.refs.push(Held { question, arm, slot: slot as u32, live });
    }

    /// Seat a plugin emission its `ClassIsa` trigger did not fire locally:
    /// one `ClassIsaPrefix` question, the emission's symbols and refs held
    /// under its `Then`, with the return-type witnesses a fired emit pushes.
    pub fn hold_gated_emission(&mut self, em: GatedEmission) {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload, WitnessSource};
        let q = self.ask(Question::ClassIsaPrefix { package: em.package.clone(), prefixes: em.gate_prefixes });
        let scope = self.scope_at(em.scope_point).unwrap_or(ScopeId(0));
        let ns = Namespace::framework(em.plugin_id.clone());
        let source = || WitnessSource::Plugin(em.plugin_id.clone());
        for gs in em.symbols {
            let package = gs.on_class.or_else(|| Some(em.package.clone()));
            // The emit dedup: never stack a second identical synthesized
            // symbol over one the walk minted.
            let minted = self.symbols.named(&gs.name).iter().any(|id| {
                let s = &self.symbols[id.0 as usize];
                s.kind == gs.kind && s.package == package && s.namespace == ns
            });
            if minted {
                continue;
            }
            let class_method = matches!(gs.kind, SymKind::Method | SymKind::Sub).then(|| package.clone()).flatten();
            let id = self.hold_symbol(
                q,
                Arm::Then,
                Symbol {
                    id: SymbolId(0),
                    name: gs.name.clone(),
                    kind: gs.kind,
                    span: gs.span,
                    selection_span: gs.selection_span,
                    scope,
                    package,
                    detail: gs.detail,
                    namespace: ns.clone(),
                    presentation: gs.presentation,
                    attributes: Vec::new(),
                    flags: Default::default(),
                    declared_with: None,
                    deref_stack: Vec::new(),
                    arity: None,
                },
            );
            if let Some(rt) = gs.return_type {
                // The fold writeback's `PackageSymbol → Edge(Symbol)` mirror
                // too, so a cross-file return-type query reaches it.
                self.hold_witness(q, Arm::Then, Witness {
                    attachment: WitnessAttachment::Symbol(id),
                    source: source(),
                    payload: WitnessPayload::InferredType(rt),
                    span: gs.span,
                });
                if let Some(class) = class_method {
                    self.hold_witness(q, Arm::Then, Witness {
                        attachment: WitnessAttachment::PackageSymbol { package: class, name: gs.name },
                        source: source(),
                        payload: WitnessPayload::Edge(WitnessAttachment::Symbol(id)),
                        span: gs.span,
                    });
                }
            }
        }
        for gr in em.refs {
            self.hold_ref(q, Arm::Then, Ref {
                kind: gr.kind,
                span: gr.span,
                scope,
                target_name: gr.target_name,
                access: gr.access,
                binding: Binding::from(gr.binding),
                folded_from: None,
                arg_count: None,
            });
        }
    }

    /// Seat a deferred indirect-object site: the walk minted the undecided
    /// reading's heads, which stay in their slots; the other reading's
    /// heads take fresh ones.
    pub fn hold_indirect(&mut self, heads: IndirectHeads) {
        let q = self.ask(Question::IndirectCall(heads.site));
        let fallback = self.readings.fallback(q);
        for (arm, refs) in [(Arm::Then, heads.method), (Arm::Otherwise, heads.function)] {
            for r in refs {
                if Some(arm) != fallback {
                    self.hold_ref(q, arm, r);
                    continue;
                }
                let same = |m: &Ref| m.span == r.span && std::mem::discriminant(&m.kind) == std::mem::discriminant(&r.kind);
                if let Some(slot) = self.refs.iter().position(same) {
                    self.hold_existing_ref(q, slot);
                }
            }
        }
    }

    pub fn hold_witness(&mut self, question: u32, arm: Arm, w: Witness) {
        if self.readings.fallback(question) == Some(arm) {
            self.witnesses.push(w);
        } else {
            self.readings.witnesses.push((question, arm, w));
        }
    }

    /// Decide every question this file holds, through the index when given.
    pub fn picks(&self, index: Option<&dyn CrossFileLookup>) -> Picks {
        let facts = InFile { fa: self, index };
        self.readings
            .questions
            .iter()
            .map(|q| match q.question.decide(&facts) {
                Verdict::Yes => Some(Arm::Then),
                Verdict::No => Some(Arm::Otherwise),
                Verdict::Unknown => q.fallback.arm(),
            })
            .collect()
    }

    /// This analysis with `picks` installed, or `None` when they are the
    /// fallbacks the base already shows.
    pub fn decided(&self, picks: &[Option<Arm>]) -> Option<FileAnalysis> {
        if picks == self.readings.fallbacks().as_slice() {
            return None;
        }
        let mut view = self.clone();
        view.install(picks);
        Some(view)
    }

    /// The view of this analysis its own questions pick under `index`.
    pub fn decided_with(&self, index: &dyn CrossFileLookup) -> Option<FileAnalysis> {
        if self.readings.is_empty() {
            return None;
        }
        self.decided(&self.picks(Some(index)))
    }

    fn install(&mut self, picks: &[Option<Arm>]) {
        let readings = std::mem::take(&mut self.readings);
        let pick = |q: u32| picks.get(q as usize).copied().flatten();
        let on = |q: u32, arm: Arm| pick(q) == Some(arm);
        // A question at its fallback keeps the base's slots: the base's
        // entries carry the build's own stamps, which a held copy predates.
        let moved = |q: u32| pick(q) != readings.fallback(q);
        // Two questions can mint the same synthesized symbol; a view shows
        // it once, as the builder's emit dedup would have.
        let mut shown: HashSet<(String, SymKind, Option<String>, Namespace)> = HashSet::new();
        for h in readings.symbols.iter().filter(|h| moved(h.question)) {
            let slot = h.slot as usize;
            let key = (h.live.name.clone(), h.live.kind, h.live.package.clone(), h.live.namespace.clone());
            self.symbols[slot] = if on(h.question, h.arm) && shown.insert(key) {
                h.live.clone()
            } else {
                Symbol::dormant(h.live.id)
            };
        }
        for h in readings.refs.iter().filter(|h| moved(h.question)) {
            self.refs[h.slot as usize] = if on(h.question, h.arm) { h.live.clone() } else { Ref::dormant() };
        }
        for (q, arm, w) in &readings.witnesses {
            if on(*q, *arm) {
                self.witnesses.push(w.clone());
            }
        }
        self.readings = readings;
        self.rebuild_decided_indices();
    }
}

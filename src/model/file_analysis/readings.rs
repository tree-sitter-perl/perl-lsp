//! Readings a question picks between. A site whose meaning waits on other
//! files mints every reading it can have, each tagged with the question
//! and the arm it belongs to, into `Readings`: a table of its own, never
//! the symbol or ref tables. The tables hold what the file states whatever
//! the answers (the base) followed by the readings a view installs; the
//! stored analysis installs each question's fallback, and a decided view
//! truncates to the base and installs its own picks. A base id or index is
//! the same in every view; an installed reading's is its view's own.
//!
//! One kind of reading replaces rather than adds: a site the walk had to
//! read one way (an indirect-object call, whose undecided reading the walk
//! runs whole) keeps its head refs in base slots, and each slot holds the
//! head of whichever reading is installed (`docs/adr/query-delayed-facts.md`).

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

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

/// An entry that holds only when its question picks `arm`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Held<T> {
    pub question: u32,
    pub arm: Arm,
    pub entry: T,
}

/// A witness a reading adds. One that states a held symbol's value names
/// the symbol by its place in `Readings::symbols`, since its id is the
/// view's to assign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeldWitness {
    Plain(Witness),
    /// `Symbol(symbol)` is `ty`.
    Returns { symbol: u32, ty: InferredType, source: WitnessSource, span: Span },
    /// `PackageSymbol{package, name}` edges to `Symbol(symbol)`.
    Dispatches { symbol: u32, package: String, name: String, source: WitnessSource, span: Span },
}

/// A base ref slot whose content is a reading's: `then` or `otherwise`, as
/// the question picks. The witnesses the walk attached to the slot
/// (`Expression(slot)`) state the undecided reading's head, so
/// `seat_readings` moves them into the readings under the fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldSlot {
    pub question: u32,
    pub slot: u32,
    pub then: Ref,
    pub otherwise: Ref,
}

/// How long each table is before any reading is installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseLengths {
    pub symbols: u32,
    pub refs: u32,
    pub witnesses: u32,
}

/// Every reading this file holds, by question.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Readings {
    pub questions: Vec<HeldQuestion>,
    /// Held symbols carry no id of their own: a view assigns one as it
    /// installs them.
    pub symbols: Vec<Held<Symbol>>,
    pub refs: Vec<Held<Ref>>,
    pub witnesses: Vec<Held<HeldWitness>>,
    pub slots: Vec<HeldSlot>,
    base: BaseLengths,
    /// The picks the tables show.
    installed: Picks,
}

/// Both readings' head refs at one deferred `meth Class(ARGS)` (the
/// builder's `hold_indirect_heads`), as `(Then, Otherwise)` pairs: the
/// verb's head (`Class->meth`'s method call, `meth(..)`'s function call)
/// and the class word's (`Class`, the package; `Class(..)`, a call).
#[derive(Debug, Clone)]
pub struct IndirectHeads {
    pub site: IndirectSite,
    pub verb: (Ref, Ref),
    pub class: (Ref, Ref),
}

/// One pick per question: the arm a view installs, `None` for neither.
pub type Picks = Vec<Option<Arm>>;

impl Readings {
    pub fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    fn fallback(&self, q: u32) -> Option<Arm> {
        self.questions[q as usize].fallback.arm()
    }

    /// Each question's fallback: what an undecided view installs.
    pub fn fallbacks(&self) -> Picks {
        self.questions.iter().map(|q| q.fallback.arm()).collect()
    }

    /// The picks the tables show.
    pub fn installed(&self) -> &[Option<Arm>] {
        &self.installed
    }

    pub fn base(&self) -> BaseLengths {
        self.base
    }

    /// Every held symbol, picked or not — what registration lists, since a
    /// name some view can show must be findable before any view asks.
    pub fn held_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().map(|h| &h.entry)
    }

    pub fn held_refs(&self) -> impl Iterator<Item = &Ref> {
        self.refs.iter().map(|h| &h.entry)
    }
}

impl FileAnalysis {
    /// How long each table is without any reading installed.
    pub fn base_lengths(&self) -> BaseLengths {
        if self.readings.is_empty() {
            return BaseLengths {
                symbols: self.symbols.len() as u32,
                refs: self.refs.len() as u32,
                witnesses: self.witnesses.len() as u32,
            };
        }
        self.readings.base
    }

    /// The symbols every view shows. Empty on a symbol-evicted copy, as
    /// `symbols()` is.
    pub fn base_symbols(&self) -> &[Symbol] {
        let all = self.symbols();
        &all[..(self.base_lengths().symbols as usize).min(all.len())]
    }

    /// The held symbols a view installing `picks` shows, without building
    /// it: one per synthesized identity, as `install` dedups. Their ids are
    /// the stored copy's to give, so a lister reads them for their names
    /// and spans only.
    pub fn shown_held_symbols(&self, picks: &[Option<Arm>]) -> Vec<&Symbol> {
        let mut shown: HashSet<(&str, SymKind, Option<&str>, &Namespace)> = HashSet::new();
        self.readings
            .symbols
            .iter()
            .filter(|h| picks.get(h.question as usize).copied().flatten() == Some(h.arm))
            .map(|h| &h.entry)
            .filter(|s| shown.insert((s.name.as_str(), s.kind, s.package.as_deref(), &s.namespace)))
            .collect()
    }

    /// Every name a held reading could add to this file, as the relational
    /// rows key them: the candidate presence its own row table records, so
    /// a retrieval that must not miss a view's entries finds the file
    /// without the base rows claiming them.
    pub fn held_row_names(&self) -> Vec<String> {
        let names = &self.pack.names;
        let mut out: Vec<String> = self.readings.held_refs().map(|r| r.match_key(names)).collect();
        for s in self.readings.held_symbols() {
            out.push(s.name.clone());
            out.push(name_match_key(&s.name, names));
        }
        out.sort();
        out.dedup();
        out
    }

    /// Everything the relational rows store of this file.
    pub fn row_seeds(&self) -> RowSeeds {
        RowSeeds { refs: self.ref_row_seeds(), syms: self.sym_row_seeds(), held: self.held_row_names() }
    }

    /// Ask a question of this file, appending it to the readings. Returns
    /// its index for the entries that follow.
    pub fn ask(&mut self, question: Question) -> u32 {
        let fallback = question.when_unknown();
        self.readings.questions.push(HeldQuestion { question, fallback });
        (self.readings.questions.len() - 1) as u32
    }

    /// Hold a symbol under `(question, arm)`. Returns its place among the
    /// held symbols, which a `HeldWitness` names it by.
    pub fn hold_symbol(&mut self, question: u32, arm: Arm, entry: Symbol) -> u32 {
        self.readings.symbols.push(Held { question, arm, entry });
        (self.readings.symbols.len() - 1) as u32
    }

    pub fn hold_ref(&mut self, question: u32, arm: Arm, entry: Ref) {
        self.readings.refs.push(Held { question, arm, entry });
    }

    pub fn hold_witness(&mut self, question: u32, arm: Arm, entry: HeldWitness) {
        self.readings.witnesses.push(Held { question, arm, entry });
    }

    /// Hold a plugin emission its `ClassIsa` trigger did not fire locally:
    /// one `ClassIsaPrefix` question, the emission's symbols and refs held
    /// under its `Then`, with the return-type witnesses a fired emit pushes.
    pub fn hold_gated_emission(&mut self, em: GatedEmission) {
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
            let held = self.hold_symbol(
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
            if let Some(ty) = gs.return_type {
                // The fold writeback's `PackageSymbol → Edge(Symbol)` mirror
                // too, so a cross-file return-type query reaches it.
                self.hold_witness(q, Arm::Then, HeldWitness::Returns { symbol: held, ty, source: source(), span: gs.span });
                if let Some(package) = class_method {
                    self.hold_witness(q, Arm::Then, HeldWitness::Dispatches {
                        symbol: held,
                        package,
                        name: gs.name,
                        source: source(),
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

    /// Hold a deferred indirect-object site's heads. Where the walk minted
    /// the undecided reading's head, that slot holds whichever head the
    /// view installs; where it minted none (a framework keyword the walk
    /// read as a declaration), the other reading's head is added.
    pub fn hold_indirect(&mut self, heads: IndirectHeads) {
        let q = self.ask(Question::IndirectCall(heads.site));
        let Some(fallback) = self.readings.fallback(q) else { return };
        for (then, otherwise) in [heads.verb, heads.class] {
            let minted = match fallback {
                Arm::Then => &then,
                Arm::Otherwise => &otherwise,
            };
            let same = |m: &Ref| m.span == minted.span && std::mem::discriminant(&m.kind) == std::mem::discriminant(&minted.kind);
            match self.refs.iter().position(same) {
                Some(slot) => self.readings.slots.push(HeldSlot { question: q, slot: slot as u32, then, otherwise }),
                None => match fallback {
                    Arm::Then => self.hold_ref(q, Arm::Otherwise, otherwise),
                    Arm::Otherwise => self.hold_ref(q, Arm::Then, then),
                },
            }
        }
    }

    /// Close the base at the tables as they stand and install every
    /// question's fallback. The builder's last step: nothing the walk or
    /// its passes mint may land after it, or a view would truncate it away.
    pub fn seat_readings(&mut self) {
        if self.readings.is_empty() {
            return;
        }
        // A slot's own facts state the walk's reading of its head: they
        // move into the readings under the fallback.
        for i in 0..self.readings.slots.len() {
            let HeldSlot { question, slot, .. } = self.readings.slots[i];
            let Some(arm) = self.readings.fallback(question) else { continue };
            let att = WitnessAttachment::Expression(crate::model::witnesses::RefIdx(slot));
            for w in self.witnesses.take_attachment(&att) {
                self.hold_witness(question, arm, HeldWitness::Plain(w));
            }
        }
        self.readings.base = BaseLengths {
            symbols: self.symbols.len() as u32,
            refs: self.refs.len() as u32,
            witnesses: self.witnesses.len() as u32,
        };
        let fallbacks = self.readings.fallbacks();
        self.install(&fallbacks);
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
    /// ones it already shows.
    pub fn decided(&self, picks: &[Option<Arm>]) -> Option<FileAnalysis> {
        if picks == self.readings.installed() {
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

    /// Cut the tables back to the base and append the readings `picks`
    /// selects. A base ref is never linked to an installed symbol (the
    /// links below touch installed refs only), so the cut leaves no base
    /// entry naming an id it removed.
    fn install(&mut self, picks: &[Option<Arm>]) {
        let readings = std::mem::take(&mut self.readings);
        let base = readings.base;
        let on = |q: u32, arm: Arm| picks.get(q as usize).copied().flatten() == Some(arm);
        self.symbols.truncate(base.symbols as usize);
        self.refs.truncate(base.refs as usize);
        self.witnesses.truncate(base.witnesses as usize);
        for s in &readings.slots {
            let arm = picks.get(s.question as usize).copied().flatten().or(readings.fallback(s.question));
            self.refs.as_mut_slice()[s.slot as usize] = match arm {
                Some(Arm::Then) => s.then.clone(),
                _ => s.otherwise.clone(),
            };
        }
        // Two questions can mint the same synthesized symbol; a view shows
        // it once, as the builder's emit dedup would have.
        let mut shown: HashMap<(String, SymKind, Option<String>, Namespace), SymbolId> = HashMap::new();
        let mut ids: Vec<Option<SymbolId>> = Vec::with_capacity(readings.symbols.len());
        for h in &readings.symbols {
            if !on(h.question, h.arm) {
                ids.push(None);
                continue;
            }
            let key = (h.entry.name.clone(), h.entry.kind, h.entry.package.clone(), h.entry.namespace.clone());
            let id = *shown.entry(key).or_insert_with(|| {
                let id = SymbolId(self.symbols.len() as u32);
                let mut s = h.entry.clone();
                s.id = id;
                self.symbols.push(s);
                id
            });
            ids.push(Some(id));
        }
        for h in readings.refs.iter().filter(|h| on(h.question, h.arm)) {
            self.refs.push(h.entry.clone());
        }
        for h in readings.witnesses.iter().filter(|h| on(h.question, h.arm)) {
            let w = match &h.entry {
                HeldWitness::Plain(w) => w.clone(),
                HeldWitness::Returns { symbol, ty, source, span } => {
                    let Some(id) = ids[*symbol as usize] else { continue };
                    Witness {
                        attachment: WitnessAttachment::Symbol(id),
                        source: source.clone(),
                        payload: WitnessPayload::InferredType(ty.clone()),
                        span: *span,
                    }
                }
                HeldWitness::Dispatches { symbol, package, name, source, span } => {
                    let Some(id) = ids[*symbol as usize] else { continue };
                    Witness {
                        attachment: WitnessAttachment::PackageSymbol { package: package.clone(), name: name.clone() },
                        source: source.clone(),
                        payload: WitnessPayload::Edge(WitnessAttachment::Symbol(id)),
                        span: *span,
                    }
                }
            };
            self.witnesses.push(w);
        }
        self.readings = readings;
        self.readings.installed = picks.to_vec();
        self.rebuild_decided_indices(base.refs as usize);
    }
}

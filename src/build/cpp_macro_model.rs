//! Config-variant macro modeling — one macro is the COMPLETE set of its
//! `#if`-guarded definitions, joined for typing and reachability-ranked for
//! navigation.
//!
//! A config-dependent macro (`PERL_BITFIELD16`: `U16` on WIN32, `U16` under
//! `HAS_NON_INT_BITFIELDS`, `unsigned` otherwise) is NOT one definition the
//! collection order happened to keep — it is all of them. Modeling it as a
//! variant SET keeps goto-def from jumping to an arbitrary winner (e.g. the
//! win32 def on Linux).
//!
//! Nothing is ever pruned. Portability is a first-class output — the
//! programmer frequently WANTS the win32 def. Reachability is RANKING +
//! LABELING against the knowable config (predefined macros ∪ the `#define`s
//! we can see), never deletion:
//!   - ACTIVE       — guard satisfiable given what we know (sorted first).
//!   - UNKNOWN      — guard depends on a config macro we've seen defined
//!                    somewhere but can't resolve here (kept, labeled).
//!   - UNREACHABLE  — guard provably false: it needs a macro that is neither
//!                    predefined nor ever `#define`d in the closure, so it is
//!                    a platform/compiler macro absent on this target (kept,
//!                    sorted last, labeled).
//!
//! The UNKNOWN/UNREACHABLE split is seeded by the definition UNIVERSE, not a
//! hard-coded platform-macro list (that would be rule #10): a guard macro
//! that is `#define`d anywhere we looked is a knob whose value we merely
//! don't know here (UNKNOWN); one that is defined nowhere and is not
//! predefined cannot be turned on by any code we can see (UNREACHABLE).
//! The universe then SETTLES: a knob whose every `#define` sits under a
//! provably-false trail cannot be turned on either, so it leaves the
//! universe, and what it guards becomes UNREACHABLE in turn.

use crate::model::file_analysis::{CondTerm, CondTest};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What we can decide about the live config. `defined` are the macros known
/// ON (predefined ∪ accumulated `#define`s); `universe` is every macro name
/// `#define`d ANYWHERE in the closure regardless of its own guard — the set
/// of knobs that COULD be on. A guard macro outside `universe` and outside
/// `defined` is a platform/compiler macro absent here.
#[derive(Debug, Clone, Default)]
pub struct KnownConfig {
    pub defined: HashSet<String>,
    pub universe: HashSet<String>,
    /// Knobs settled out of `universe`: name → the root reason every one of
    /// its `#define`s is unreachable (`__cplusplus undefined`).
    settled: HashMap<String, String>,
}

/// Every `#define` site a config is built from: the name and the trail of
/// the arm it sits in.
#[derive(Debug, Default)]
pub struct DefineSites {
    unconditional: HashSet<String>,
    conditional: BTreeMap<String, Vec<Vec<CondTerm>>>,
}

impl DefineSites {
    pub fn add(&mut self, name: &str, trail: &[CondTerm]) {
        if trail.is_empty() {
            self.unconditional.insert(name.to_string());
        } else if !self.unconditional.contains(name) {
            self.conditional.entry(name.to_string()).or_default().push(trail.to_vec());
        }
    }
}

impl KnownConfig {
    #[cfg(test)]
    pub fn new(defined: HashSet<String>, universe: HashSet<String>) -> Self {
        KnownConfig { defined, universe, settled: HashMap::new() }
    }

    /// The config a closure states: its unconditional `#define`s and the
    /// toolchain's `predefined` names are ON, every name `#define`d anywhere
    /// is a knob, and then the knobs settle. The one constructor build-side
    /// variant selection and navigation share, so they cannot disagree on
    /// which arm is Active.
    // TODO(#255): a define the closure later `#undef`s still counts as ON
    // here; #255's undef spans feed `DefineSites` at this seam.
    pub fn from_defines<S: Into<String>>(sites: DefineSites, predefined: impl IntoIterator<Item = S>) -> Self {
        let DefineSites { unconditional, conditional } = sites;
        let mut defined = unconditional;
        defined.extend(predefined.into_iter().map(Into::into));
        let mut universe = defined.clone();
        universe.extend(conditional.keys().cloned());
        let mut cfg = KnownConfig { defined, universe, settled: HashMap::new() };
        let pending: Vec<(String, Vec<Vec<Cond>>)> = conditional
            .into_iter()
            .filter(|(name, _)| !cfg.defined.contains(name))
            .map(|(name, trails)| (name, trails.iter().map(|t| t.iter().map(term_cond).collect()).collect()))
            .collect();
        cfg.settle(pending);
        cfg
    }

    /// The fixed point: drop every knob whose `#define`s are all provably
    /// unreachable under the config so far. Only ever shrinks the universe,
    /// so each pass can only turn Unknown into False, and it terminates.
    fn settle(&mut self, mut pending: Vec<(String, Vec<Vec<Cond>>)>) {
        loop {
            let mut out: Vec<(String, String)> = Vec::new();
            let asked = Asked { cfg: self, own: None };
            pending.retain(|(name, trails)| {
                if !trails.iter().all(|t| conjunction(t, &asked) == Tri::False) {
                    return true;
                }
                let root = trails.iter().map(|t| root_reason(t, &asked)).min().unwrap_or_default();
                out.push((name.clone(), root));
                false
            });
            if out.is_empty() {
                return;
            }
            for (name, root) in out {
                self.universe.remove(&name);
                self.settled.insert(name, root);
            }
        }
    }
}

/// A variant's reachability verdict under a `KnownConfig`. Never a delete —
/// the label rides the variant into hover / the goto-def picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    Active,
    Unknown { guard: String },
    Unreachable { reason: String },
}

impl Reachability {
    /// Sort key: ACTIVE first, then UNKNOWN, UNREACHABLE last. Stable within
    /// a rank preserves source/collection order.
    pub fn rank(&self) -> u8 {
        match self {
            Reachability::Active => 0,
            Reachability::Unknown { .. } => 1,
            Reachability::Unreachable { .. } => 2,
        }
    }
    pub fn label(&self) -> Option<String> {
        match self {
            Reachability::Active => None,
            Reachability::Unknown { guard } => Some(format!("if {guard}")),
            Reachability::Unreachable { reason } => Some(format!("unreachable: {reason}")),
        }
    }
}

/// Kleene three-valued logic — guards evaluate under PARTIAL knowledge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
    fn and(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }
    fn or(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
}

/// A parsed preprocessor condition — a boolean expression over `defined(X)`
/// and bare macro atoms. `Opaque` is a well-formed term whose value the
/// evaluator does not model — arithmetic, a comparison, a function-like
/// macro call (`__GNUC_PREREQ (4, 4)`) — so the boolean structure around it
/// still decides (`False && opaque` is False). `Unparsed` is a condition
/// that did not parse at all. Both evaluate Unknown, never guessed.
#[derive(Debug, Clone)]
enum Cond {
    Defined(String),
    Ident(String),
    Lit(bool),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Opaque,
    Unparsed,
}

impl Cond {
    fn eval(&self, cfg: &Asked) -> Tri {
        match self {
            Cond::Lit(b) => {
                if *b {
                    Tri::True
                } else {
                    Tri::False
                }
            }
            // `defined(X)`: ON → True; a known knob we can't resolve → Unknown;
            // never defined anywhere → provably absent → False.
            Cond::Defined(x) => defined_tri(x, cfg),
            // `#if FOO` truthiness: we only know defined-ness, not the value, so
            // a defined/known-knob macro is Unknown; a macro defined nowhere is
            // 0 → False.
            Cond::Ident(x) => match defined_tri(x, cfg) {
                Tri::False => Tri::False,
                _ => Tri::Unknown,
            },
            Cond::Not(c) => c.eval(cfg).not(),
            Cond::And(a, b) => a.eval(cfg).and(b.eval(cfg)),
            Cond::Or(a, b) => a.eval(cfg).or(b.eval(cfg)),
            Cond::Opaque | Cond::Unparsed => Tri::Unknown,
        }
    }

    /// Collect the `defined(X)` atoms under an even (`positive=true`) vs odd
    /// number of negations — for building the human reason on UNREACHABLE.
    fn atoms(&self, positive: bool, out: &mut Vec<(String, bool)>) {
        match self {
            Cond::Defined(x) | Cond::Ident(x) => out.push((x.clone(), positive)),
            Cond::Not(c) => c.atoms(!positive, out),
            Cond::And(a, b) | Cond::Or(a, b) => {
                a.atoms(positive, out);
                b.atoms(positive, out);
            }
            Cond::Lit(_) | Cond::Opaque | Cond::Unparsed => {}
        }
    }
}

/// A config as one question asks it: `own` is the macro whose variants are
/// being ranked, which is never ON at its own definition — `#ifndef X …
/// #define X …` guards a definition on X's not-yet-defined-ness.
#[derive(Clone, Copy)]
struct Asked<'a> {
    cfg: &'a KnownConfig,
    own: Option<&'a str>,
}

impl Asked<'_> {
    fn on(&self, x: &str) -> bool {
        self.own != Some(x) && self.cfg.defined.contains(x)
    }
    fn known(&self, x: &str) -> bool {
        self.on(x) || self.cfg.universe.contains(x)
    }
}

fn defined_tri(x: &str, cfg: &Asked) -> Tri {
    if cfg.on(x) {
        Tri::True
    } else if cfg.known(x) {
        Tri::Unknown
    } else {
        Tri::False
    }
}

/// How a term reads in a label, as the preprocessor would spell it. Display
/// only: the evaluator reads the structure, never this text.
pub fn render_term(t: &CondTerm) -> String {
    let bang = if t.negated { "!" } else { "" };
    match &t.test {
        CondTest::Defined(name) => format!("{bang}defined({name})"),
        CondTest::Expr(e) if !t.negated => e.clone(),
        CondTest::Expr(e) => format!("!({e})"),
    }
}

/// A trail as the conjunction it states.
pub fn render_trail(trail: &[CondTerm]) -> String {
    trail.iter().map(render_term).collect::<Vec<_>>().join(" && ")
}

/// One trail term as a condition: an `#ifdef`'s name is a `defined` atom,
/// an `#if`'s source text is parsed, and the negation is applied to the
/// structure — never rendered and re-read.
fn term_cond(t: &CondTerm) -> Cond {
    let c = match &t.test {
        CondTest::Defined(name) => Cond::Defined(name.clone()),
        CondTest::Expr(text) => parse_cond(text),
    };
    if t.negated { Cond::Not(Box::new(c)) } else { c }
}

fn conjunction(conds: &[Cond], cfg: &Asked) -> Tri {
    conds.iter().fold(Tri::True, |acc, c| acc.and(c.eval(cfg)))
}

/// Evaluate the AND of a declaration's enclosing guard terms under `cfg`.
pub fn classify(guards: &[CondTerm], cfg: &KnownConfig) -> Reachability {
    classify_asked(guards, Asked { cfg, own: None })
}

/// Evaluate a variant of macro `own`'s guard terms: `own` itself never
/// counts as ON while its own variants are ranked.
pub fn classify_variant(guards: &[CondTerm], cfg: &KnownConfig, own: &str) -> Reachability {
    classify_asked(guards, Asked { cfg, own: Some(own) })
}

fn classify_asked(guards: &[CondTerm], cfg: Asked) -> Reachability {
    if guards.is_empty() {
        return Reachability::Active;
    }
    let conds: Vec<Cond> = guards.iter().map(term_cond).collect();
    let cfg = &cfg;
    match conjunction(&conds, cfg) {
        Tri::True => Reachability::Active,
        Tri::Unknown => Reachability::Unknown { guard: render_trail(guards) },
        Tri::False => Reachability::Unreachable { reason: unreachable_reason(&conds, cfg) },
    }
}

/// The first atom that sinks a provably-false guard: a `defined(X)` that
/// fails or a negated one that holds.
fn failing_atom(conds: &[Cond], cfg: &Asked) -> Option<(String, bool)> {
    let mut atoms = Vec::new();
    for c in conds {
        c.atoms(true, &mut atoms);
    }
    atoms.into_iter().find(|(x, positive)| (*positive && !cfg.known(x)) || (!*positive && cfg.on(x)))
}

/// A short human reason for a provably-false guard (`X undefined`, `X
/// defined`); a knob that settled out names the root it settled on.
fn unreachable_reason(conds: &[Cond], cfg: &Asked) -> String {
    match failing_atom(conds, cfg) {
        Some((x, true)) => match cfg.cfg.settled.get(&x) {
            Some(root) => format!("{x} undefined ({root})"),
            None => format!("{x} undefined"),
        },
        Some((x, false)) => format!("{x} defined"),
        None => "guard not satisfiable".to_string(),
    }
}

/// The reason a settled knob carries: chained through any knob that
/// settled before it, so a label names the config fact at the bottom.
fn root_reason(conds: &[Cond], cfg: &Asked) -> String {
    match failing_atom(conds, cfg) {
        Some((x, true)) => cfg.cfg.settled.get(&x).cloned().unwrap_or_else(|| format!("{x} undefined")),
        Some((x, false)) => format!("{x} defined"),
        None => "guard not satisfiable".to_string(),
    }
}

// ---- a tiny recursive-descent parser for the guard subset we model ----

fn parse_cond(s: &str) -> Cond {
    let toks = lex(s);
    let mut p = Parser { toks: &toks, i: 0, malformed: false };
    let c = p.parse_or();
    if p.malformed || p.i != p.toks.len() { Cond::Unparsed } else { c }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Defined,
    Not,
    And,
    Or,
    LParen,
    RParen,
    Num(bool),
    Other,
}

fn lex(s: &str) -> Vec<Tok> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    while i < b.len() {
        let c = b[i];
        match c {
            _ if c.is_ascii_whitespace() => i += 1,
            // A line splice: the condition continues on the next line.
            b'\\' if b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()) => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'!' => {
                // `!=` is a comparison we don't model — bail to Other.
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Other);
                    i += 2;
                } else {
                    out.push(Tok::Not);
                    i += 1;
                }
            }
            b'&' if b.get(i + 1) == Some(&b'&') => {
                out.push(Tok::And);
                i += 2;
            }
            b'|' if b.get(i + 1) == Some(&b'|') => {
                out.push(Tok::Or);
                i += 2;
            }
            _ if c.is_ascii_digit() => {
                let s0 = i;
                while i < b.len() && b[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                // any nonzero integer literal is truthy
                out.push(Tok::Num(&s[s0..i] != "0"));
            }
            _ if is_id(c) => {
                let s0 = i;
                while i < b.len() && is_id(b[i]) {
                    i += 1;
                }
                let w = &s[s0..i];
                out.push(if w == "defined" { Tok::Defined } else { Tok::Ident(w.to_string()) });
            }
            _ => {
                out.push(Tok::Other);
                i += 1;
            }
        }
    }
    out
}

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
    malformed: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn bump(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.i);
        if t.is_some() {
            self.i += 1;
        }
        t
    }
    fn parse_or(&mut self) -> Cond {
        let mut lhs = self.parse_and();
        while self.peek() == Some(&Tok::Or) {
            self.bump();
            let rhs = self.parse_and();
            lhs = Cond::Or(Box::new(lhs), Box::new(rhs));
        }
        lhs
    }
    fn parse_and(&mut self) -> Cond {
        let mut lhs = self.parse_unary();
        while self.peek() == Some(&Tok::And) {
            self.bump();
            let rhs = self.parse_unary();
            lhs = Cond::And(Box::new(lhs), Box::new(rhs));
        }
        lhs
    }
    fn fail(&mut self) -> Cond {
        self.malformed = true;
        Cond::Unparsed
    }
    fn parse_unary(&mut self) -> Cond {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            return Cond::Not(Box::new(self.parse_unary()));
        }
        let mut c = self.parse_atom();
        // An operator the boolean subset does not model (`>=`, `+`, `?:`)
        // joins its operands into one opaque term; `&&`/`||` still bind
        // looser, so `defined(A) && X >= 2` keeps its structure.
        while self.peek() == Some(&Tok::Other) {
            while self.peek() == Some(&Tok::Other) {
                self.bump();
            }
            self.parse_unary();
            c = Cond::Opaque;
        }
        c
    }
    fn parse_atom(&mut self) -> Cond {
        match self.bump().cloned() {
            Some(Tok::Num(b)) => Cond::Lit(b),
            // A function-like macro call: its value is not modelled.
            Some(Tok::Ident(_)) if self.peek() == Some(&Tok::LParen) => {
                let mut depth = 0usize;
                while let Some(t) = self.bump() {
                    match t {
                        Tok::LParen => depth += 1,
                        Tok::RParen => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        return Cond::Opaque;
                    }
                }
                self.fail()
            }
            Some(Tok::Ident(x)) => Cond::Ident(x),
            Some(Tok::Defined) => {
                // `defined X` or `defined(X)`
                let paren = self.peek() == Some(&Tok::LParen);
                if paren {
                    self.bump();
                }
                let name = match self.bump().cloned() {
                    Some(Tok::Ident(x)) => x,
                    _ => return self.fail(),
                };
                if paren {
                    if self.peek() == Some(&Tok::RParen) {
                        self.bump();
                    } else {
                        return self.fail();
                    }
                }
                Cond::Defined(name)
            }
            Some(Tok::LParen) => {
                let c = self.parse_or();
                if self.peek() == Some(&Tok::RParen) {
                    self.bump();
                    c
                } else {
                    self.fail()
                }
            }
            // A prefix operator (`-1`, `~X`): an opaque term.
            Some(Tok::Other) => {
                self.parse_unary();
                Cond::Opaque
            }
            _ => self.fail(),
        }
    }
}

#[cfg(test)]
#[path = "cpp_macro_model_tests.rs"]
mod tests;

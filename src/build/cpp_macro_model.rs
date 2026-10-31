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

use crate::model::file_analysis::{CondTerm, CondTest};
use std::collections::HashSet;

/// What we can decide about the live config. `defined` are the macros known
/// ON (predefined ∪ accumulated `#define`s); `universe` is every macro name
/// `#define`d ANYWHERE in the closure regardless of its own guard — the set
/// of knobs that COULD be on. A guard macro outside `universe` and outside
/// `defined` is a platform/compiler macro absent here.
#[derive(Debug, Clone, Default)]
pub struct KnownConfig {
    pub defined: HashSet<String>,
    pub universe: HashSet<String>,
}

impl KnownConfig {
    pub fn new(defined: HashSet<String>, universe: HashSet<String>) -> Self {
        KnownConfig { defined, universe }
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
/// and bare macro atoms. `Unparsed` is the honest fallback for a shape the
/// tiny parser doesn't model (arithmetic comparisons etc.) → evaluates
/// Unknown, never guessed.
#[derive(Debug, Clone)]
enum Cond {
    Defined(String),
    Ident(String),
    Lit(bool),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Unparsed,
}

impl Cond {
    fn eval(&self, cfg: &KnownConfig) -> Tri {
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
            Cond::Unparsed => Tri::Unknown,
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
            Cond::Lit(_) | Cond::Unparsed => {}
        }
    }
}

fn defined_tri(x: &str, cfg: &KnownConfig) -> Tri {
    if cfg.defined.contains(x) {
        Tri::True
    } else if cfg.universe.contains(x) {
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

/// Evaluate the AND of a variant's enclosing guard terms under `cfg`.
pub fn classify(guards: &[CondTerm], cfg: &KnownConfig) -> Reachability {
    if guards.is_empty() {
        return Reachability::Active;
    }
    let mut combined = Tri::True;
    let conds: Vec<Cond> = guards.iter().map(term_cond).collect();
    for c in &conds {
        combined = combined.and(c.eval(cfg));
    }
    match combined {
        Tri::True => Reachability::Active,
        Tri::Unknown => Reachability::Unknown { guard: render_trail(guards) },
        Tri::False => Reachability::Unreachable { reason: unreachable_reason(&conds, cfg) },
    }
}

/// A short human reason for a provably-false guard: the first `defined(X)`
/// atom that fails (`X undefined`) or negated atom that holds (`X defined`).
fn unreachable_reason(conds: &[Cond], cfg: &KnownConfig) -> String {
    let mut atoms = Vec::new();
    for c in conds {
        c.atoms(true, &mut atoms);
    }
    for (x, positive) in &atoms {
        let on = cfg.defined.contains(x);
        let known = on || cfg.universe.contains(x);
        if *positive && !known {
            return format!("{x} undefined");
        }
        if !*positive && on {
            return format!("{x} defined");
        }
    }
    "guard not satisfiable".to_string()
}

// ---- a tiny recursive-descent parser for the guard subset we model ----

fn parse_cond(s: &str) -> Cond {
    let toks = lex(s);
    let mut p = Parser { toks: &toks, i: 0 };
    let c = p.parse_or();
    if p.i == p.toks.len() {
        c
    } else {
        Cond::Unparsed // trailing tokens → a shape we don't model
    }
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
    fn parse_unary(&mut self) -> Cond {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            return Cond::Not(Box::new(self.parse_unary()));
        }
        self.parse_atom()
    }
    fn parse_atom(&mut self) -> Cond {
        match self.bump().cloned() {
            Some(Tok::Num(b)) => Cond::Lit(b),
            Some(Tok::Ident(x)) => Cond::Ident(x),
            Some(Tok::Defined) => {
                // `defined X` or `defined(X)`
                let paren = self.peek() == Some(&Tok::LParen);
                if paren {
                    self.bump();
                }
                let name = match self.bump().cloned() {
                    Some(Tok::Ident(x)) => x,
                    _ => return Cond::Unparsed,
                };
                if paren {
                    if self.peek() == Some(&Tok::RParen) {
                        self.bump();
                    } else {
                        return Cond::Unparsed;
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
                    Cond::Unparsed
                }
            }
            _ => Cond::Unparsed,
        }
    }
}

#[cfg(test)]
#[path = "cpp_macro_model_tests.rs"]
mod tests;

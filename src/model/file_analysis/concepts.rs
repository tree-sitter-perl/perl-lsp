//! SPIKE (throwaway): flow-level idiom recognition over the witness bag.
//!
//! Every Sub/Method's return arms are normalized into a canonical `Term`
//! by chasing the bag's own edges (variable flow writes, `Expr` edges,
//! projections, `Invoke`s), so `$_[0]{k}`, `shift->{k}` and
//! `my $s = shift; my $h = $s; $h->{k}` all become `Field(Arg(0), k)`.
//! Concepts are then matched on the terms, never on spelling. Without the
//! value lane a variable with more than one disagreeing write is `Opaque`.

use super::*;
use crate::model::witnesses::{
    CallArg, Callee, FactValue, ProjectionStep, ReturnExpr, WitnessAttachment, WitnessPayload,
    WitnessSource,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Term {
    /// Positional argument `i` at sub entry (0 is the invocant).
    Arg(u32),
    /// `@_` itself.
    Args,
    Field(Box<Term>, String),
    Call { method: String, recv: Box<Term>, forwards_args: bool },
    /// A `bless` whose class is the invocant (`ReceiverOr`).
    Blessed,
    /// A short-circuit / ternary: any of the arms.
    Alt(Vec<Term>),
    /// Disagreeing writes of one variable: the join only the value lane folds.
    Join(Vec<Term>),
    /// A value the builder baked (literal, operator result).
    Lit,
    /// A plain function call.
    FnCall,
    /// A bare `return` (undef / empty list).
    Undef,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Concept {
    Accessor { key: String, writes: bool },
    LazyBuilder { key: String, builder: String },
    Delegates { via: String, method: String, forwards_args: bool },
    Constructor,
    Fluent { writes: Vec<String> },
    /// Every arm calls another method on the invocant (`shift->m(@_)`,
    /// `$self->SUPER::m(@_)`): a wrapper or alias.
    Forwards { method: String, forwards_args: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recognized {
    pub sub: String,
    pub package: Option<String>,
    pub line: usize,
    pub concept: Concept,
    /// Variable writes the normalizer followed. 0 means the
    /// arm already was the canonical shape — the part a spelling matcher
    /// gets for free; >0 is what flow matching adds.
    pub hops: u32,
    /// Calls in the body beyond the ones the concept itself names: 0 means
    /// the sub IS the idiom; more means the idiom is its tail.
    pub side_calls: u32,
}

/// What the recognizer saw for one sub when nothing matched — the census
/// of what the next concept (or the value lane) would have to cover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unmatched {
    pub sub: String,
    pub line: usize,
    pub arms: Vec<Term>,
}

struct Norm<'a> {
    fa: &'a FileAnalysis,
    hops: u32,
    depth: u32,
}

impl FileAnalysis {
    pub fn recognize_concepts(&self) -> (Vec<Recognized>, Vec<Unmatched>) {
        let mut out = Vec::new();
        let mut miss = Vec::new();
        for sym in self.symbols() {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                continue;
            }
            let Some(body) = self.scopes.iter().find(|s| s.owner == Some(sym.id)) else { continue };
            let mut n = Norm { fa: self, hops: 0, depth: 0 };
            let arms: Vec<Term> = self
                .witnesses
                .iter_attachment(&WitnessAttachment::SymbolReturnArm(sym.id))
                .filter_map(|w| match &w.payload {
                    WitnessPayload::Edge(att) => Some(n.norm(att, w.span.start)),
                    // A bare `return;` has no rvalue, so the builder mints it
                    // as a fact rather than an edge.
                    WitnessPayload::Fact { family, .. } if family == "undef_arm" => Some(Term::Undef),
                    _ => None,
                })
                .collect();
            let writes = self.written_keys_in(body.span);
            let line = sym.selection_span.start.row + 1;
            let calls = self
                .refs()
                .iter()
                .filter(|r| matches!(r.kind, RefKind::MethodCall { .. } | RefKind::FunctionCall))
                .filter(|r| contains(body.span, r.span.start))
                // `shift`, `bless`, `defined`: the idiom's own vocabulary.
                .filter(|r| !(matches!(r.kind, RefKind::FunctionCall) && crate::model::builtins::is_builtin(&r.target_name)))
                .count() as u32;
            match match_concept(&arms, &writes) {
                Some(concept) => out.push(Recognized {
                    side_calls: calls.saturating_sub(match &concept {
                        Concept::Delegates { .. } | Concept::Forwards { .. } => 1,
                        Concept::LazyBuilder { .. } => 1,
                        _ => 0,
                    }),
                    sub: sym.name.clone(),
                    package: sym.package.clone(),
                    line,
                    concept,
                    hops: n.hops,
                }),
                None if !arms.is_empty() => miss.push(Unmatched { sub: sym.name.clone(), line, arms }),
                None => {}
            }
        }
        (out, miss)
    }

    pub fn concept_of(&self, sym: &Symbol) -> Option<Concept> {
        if self.bag_is_evicted() || self.refs_are_evicted() {
            return None;
        }
        let line = sym.selection_span.start.row + 1;
        self.recognize_concepts().0.into_iter().find(|r| r.sub == sym.name && r.line == line).map(|r| r.concept)
    }

    /// Instance keys written inside `span` (the builder's `mutation` facts).
    fn written_keys_in(&self, span: Span) -> Vec<String> {
        let mut keys: Vec<String> = self
            .witnesses
            .all()
            .iter()
            .filter(|w| matches!(w.attachment, WitnessAttachment::HashKey { .. }))
            .filter(|w| contains(span, w.span.start))
            .filter_map(|w| match &w.payload {
                WitnessPayload::Fact { family, value: FactValue::Str(k), .. } if family == "mutation" => {
                    Some(k.clone())
                }
                _ => None,
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }
}

impl Concept {
    pub fn describe(&self) -> String {
        match self {
            Concept::Accessor { key, writes: false } => format!("accessor: reads `{{{key}}}`"),
            Concept::Accessor { key, writes: true } => format!("accessor: reads and writes `{{{key}}}`"),
            Concept::LazyBuilder { key, builder } => format!("lazy `{{{key}}}`, built by `{builder}`"),
            Concept::Delegates { via, method, .. } => format!("delegates `{method}` to `{{{via}}}`"),
            Concept::Constructor => "constructor: blesses into the invocant's class".into(),
            Concept::Fluent { writes } => format!("fluent setter of `{}`, returns the invocant", writes.join("`, `")),
            Concept::Forwards { method, forwards_args: true } => format!("alias for `{method}`"),
            Concept::Forwards { method, .. } => format!("wraps `{method}` on the invocant"),
        }
    }
}

fn contains(span: Span, p: Point) -> bool {
    span.start <= p && p <= span.end
}

fn match_concept(arms: &[Term], writes: &[String]) -> Option<Concept> {
    use Term::*;
    let first = arms.first()?;
    if !arms.iter().all(|a| a == first) {
        // Arms disagree: a setter-then-getter (`return $self if @_; $self->{k}`)
        // is the one disagreement worth naming in a spike.
        let fields: Vec<&String> = arms
            .iter()
            .filter_map(|a| match a { Field(b, k) if **b == Arg(0) => Some(k), _ => None })
            .collect();
        if fields.len() == 1 && arms.iter().all(|a| *a == Arg(0) || matches!(a, Field(..))) {
            return Some(Concept::Accessor { key: fields[0].clone(), writes: writes.contains(fields[0]) });
        }
        return None;
    }
    match first {
        Field(b, k) if **b == Arg(0) => Some(Concept::Accessor { key: k.clone(), writes: writes.contains(k) }),
        Alt(alts) => {
            let key = alts.iter().find_map(|a| match a { Field(b, k) if **b == Arg(0) => Some(k), _ => None })?;
            let builder = alts.iter().find_map(|a| match a {
                Call { method, recv, .. } if **recv == Arg(0) => Some(method),
                _ => None,
            })?;
            // `$s->{k} || $s->SUPER::k` is a fallback, not a cache.
            writes.contains(key).then(|| Concept::LazyBuilder { key: key.clone(), builder: builder.clone() })
        }
        Call { method, recv, forwards_args } => match &**recv {
            Field(b, via) if **b == Arg(0) => Some(Concept::Delegates {
                via: via.clone(),
                method: method.clone(),
                forwards_args: *forwards_args,
            }),
            Arg(0) => Some(Concept::Forwards { method: method.clone(), forwards_args: *forwards_args }),
            _ => None,
        },
        Blessed => Some(Concept::Constructor),
        // Returning arg 0 is only an idiom on a method that changed it.
        Arg(0) if !writes.is_empty() => Some(Concept::Fluent { writes: writes.to_vec() }),
        _ => None,
    }
}

impl Norm<'_> {
    const MAX_DEPTH: u32 = 32;

    fn norm(&mut self, att: &WitnessAttachment, at: Point) -> Term {
        if self.depth >= Self::MAX_DEPTH {
            return Term::Opaque;
        }
        self.depth += 1;
        let t = self.norm_inner(att, at);
        self.depth -= 1;
        t
    }

    fn norm_inner(&mut self, att: &WitnessAttachment, at: Point) -> Term {
        match att {
            WitnessAttachment::Variable { name, scope } => self.norm_var(name, *scope, at),
            WitnessAttachment::Expr(span) => self.norm_expr(*span),
            WitnessAttachment::Expression(idx) => self.norm_call(&WitnessAttachment::Expression(*idx), at),
            WitnessAttachment::Symbol(_) => Term::FnCall,
            WitnessAttachment::BranchArm(_) => {
                let arms: Vec<Term> = self
                    .fa
                    .witnesses
                    .iter_attachment(att)
                    .filter_map(|w| match &w.payload {
                        WitnessPayload::Edge(a) => Some(a.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|a| self.norm(a, at))
                    .collect();
                Term::Alt(arms)
            }
            _ => Term::Opaque,
        }
    }

    fn norm_expr(&mut self, span: Span) -> Term {
        let att = WitnessAttachment::Expr(span);
        let ws: Vec<WitnessPayload> =
            self.fa.witnesses.iter_attachment(&att).map(|w| w.payload.clone()).collect();
        // Structure before observations: `$_[0]` carries both its projection
        // and a `ReturnExpr(Receiver)`.
        for p in &ws {
            match p {
                WitnessPayload::Projected { base, step } => {
                    let b = self.norm(base, span.start);
                    return self.project(b, step, span.start);
                }
                WitnessPayload::Edge(a) => {
                    return self.norm(a, span.start);
                }
                WitnessPayload::Invoke { .. } => return self.norm_call(&att, span.start),
                _ => {}
            }
        }
        if ws.iter().any(|p| matches!(p, WitnessPayload::ReturnExpr(ReturnExpr::ReceiverOr(_)))) {
            return Term::Blessed;
        }
        if ws.iter().any(|p| matches!(p, WitnessPayload::InferredType(_) | WitnessPayload::ReturnExpr(ReturnExpr::Concrete(_)))) {
            return Term::Lit;
        }
        Term::Opaque
    }

    fn project(&mut self, base: Term, step: &ProjectionStep, at: Point) -> Term {
        match (base, step) {
            (Term::Args, ProjectionStep::ArrayIndex(i)) if *i >= 0 => {
                Term::Arg(*i as u32 + self.consumed_before(at))
            }
            (Term::Opaque, _) => Term::Opaque,
            (b, ProjectionStep::HashKey(k)) => Term::Field(Box::new(b), k.clone()),
            _ => Term::Opaque,
        }
    }

    fn norm_call(&mut self, att: &WitnessAttachment, at: Point) -> Term {
        let invoke = self.fa.witnesses.iter_attachment(att).find_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Method { name, .. }, receiver, args } => {
                Some((name.clone(), receiver.clone(), args.clone()))
            }
            _ => None,
        });
        let Some((method, receiver, args)) = invoke else { return Term::Opaque };
        let recv = match receiver {
            Some(r) => self.norm(&r, at),
            None => Term::Opaque,
        };
        let forwards_args = match args.as_deref() {
            Some([CallArg::Spread(a)]) => self.norm(a, at) == Term::Args,
            _ => false,
        };
        Term::Call { method, recv: Box::new(recv), forwards_args }
    }

    /// The writes of `name` visible at `at`, nearest declaring scope first.
    /// One write (or several that agree) is followed; disagreeing writes are
    /// a join only the value lane can fold.
    fn norm_var(&mut self, name: &str, scope: ScopeId, at: Point) -> Term {
        if name == "@_" {
            return Term::Args;
        }
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let att = WitnessAttachment::Variable { name: name.to_string(), scope: s };
            let writes: Vec<(WitnessPayload, Point)> = self
                .fa
                .witnesses
                .iter_attachment(&att)
                .filter(|w| matches!(&w.source, WitnessSource::Builder(t) if t == "flow"))
                .filter(|w| w.span.start < at)
                .map(|w| (w.payload.clone(), w.span.start))
                .collect();
            if !writes.is_empty() {
                self.hops += 1;
                let terms: Vec<Term> = writes
                    .iter()
                    .map(|(p, pt)| match p {
                        WitnessPayload::Edge(a) => self.norm(a, *pt),
                        WitnessPayload::Projected { base, step } => {
                            let b = self.norm(base, *pt);
                            self.project(b, step, *pt)
                        }
                        _ => Term::Opaque,
                    })
                    .collect();
                return if terms.iter().all(|t| *t == terms[0]) { terms[0].clone() } else { Term::Join(terms) };
            }
            cur = self.fa.scope(s).parent;
        }
        Term::Opaque
    }

    /// `shift`s of `@_` consumed before `at` in the enclosing sub: the
    /// builder records each as a zero-width window marker on `@_`. The
    /// argument POSITION is not minted at the producer (finding).
    fn consumed_before(&self, at: Point) -> u32 {
        let Some(sub_scope) = self.enclosing_sub(at) else { return 0 };
        let att = WitnessAttachment::Variable { name: "@_".into(), scope: sub_scope };
        self.fa
            .witnesses
            .iter_attachment(&att)
            .filter(|w| matches!(&w.source, WitnessSource::Builder(t) if t == "type_constraint"))
            // The sub-entry window is seeded at the scope's own start.
            .filter(|w| w.span.start == w.span.end && w.span.start <= at)
            .filter(|w| w.span.start > self.fa.scope(sub_scope).span.start)
            .count() as u32
    }

    fn enclosing_sub(&self, at: Point) -> Option<ScopeId> {
        let mut cur = self.fa.scope_at(at);
        while let Some(s) = cur {
            let sc = self.fa.scope(s);
            if sc.owner.is_some() {
                return Some(s);
            }
            cur = sc.parent;
        }
        None
    }
}

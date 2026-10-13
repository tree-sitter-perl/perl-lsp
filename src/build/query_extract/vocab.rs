//! The capture vocabulary: every capture name a pack's query document may
//! use, as a closed enum. `Capture::parse` is the one place a capture name
//! is read as text; a pack query is checked against it when it compiles, so
//! a name nobody handles fails the load instead of extracting nothing.
//! `docs/adr/capture-vocabulary.md`.

use std::fmt;

/// A closed enum whose variants each carry their query spelling: `parse`
/// and `spelling` are generated from the one table, so they cannot drift.
macro_rules! spelled {
    ($(#[$m:meta])* pub enum $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $var),* }
        impl $name {
            pub fn spelling(self) -> &'static str {
                match self { $($name::$var => $s),* }
            }
            fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$var),)* _ => None }
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.spelling())
            }
        }
    };
}

spelled! {
    /// What a `@def.<kind>` declares. The symbol model it projects to is
    /// `sym_kind`; the structural marker it carries is `marker`.
    pub enum DefKind {
        Sub = "sub",
        Method = "method",
        /// `(anon)`-named callable (Perl `sub { ... }`).
        Anon = "anon",
        Class = "class",
        Package = "package",
        Var = "var",
        /// A parameter or block local.
        Local = "local",
        /// A struct/class data member.
        Field = "field",
        Constant = "constant",
        Enumerator = "enumerator",
        Label = "label",
        /// A function-like `#define`.
        Macro = "macro",
        /// `using Base::m;` in a class body.
        Reexport = "reexport",
        /// A named union type.
        Union = "union",
        /// An anonymous inline union's member container.
        UnionField = "unionfield",
    }
}

spelled! {
    /// What a `@ref.<kind>` refers through.
    pub enum RefKind {
        Call = "call",
        /// A `::`-qualified call (`fmt::format_to(...)`).
        QCall = "qcall",
        Method = "method",
        /// `recv.field` / `recv->field`; joins `@member.recv` by match.
        Member = "member",
        /// A type-position name.
        Type = "type",
        Var = "var",
        Key = "key",
        /// `goto LABEL`.
        Label = "label",
    }
}

spelled! {
    pub enum ScopeCap {
        /// A plain lexical block.
        Block = "",
        /// Sub-body content (function bodies, prototype signatures).
        Sub = "sub",
    }
}

spelled! {
    /// A sticky container context. The kinds are handled alike; the
    /// spelling says what the document means.
    pub enum ContextKind { Class = "class", Namespace = "namespace", Package = "package" }
}

spelled! {
    pub enum ImportCap {
        /// The whole import statement (pattern anchor).
        Statement = "",
        Name = "name",
        /// An import CALL's callee (R `library`); joins `Arg` by match.
        Fn = "fn",
        Arg = "arg",
    }
}

spelled! {
    /// The two halves of a type alias (`typedef`/`using`, or an object-like
    /// `#define` under `@macro.alias.*`), joined by match.
    pub enum AliasPart { Name = "name", Of = "of" }
}

spelled! {
    pub enum TmplCap { Owner = "owner", Param = "param" }
}

spelled! {
    pub enum FlowCap {
        /// The assignment node (pattern anchor).
        Assign = "assign",
        Target = "target",
        Source = "source",
        /// A rebind with no inflowing value (loop variables).
        Rebind = "rebind",
    }
}

spelled! {
    /// `@expr.lit.<kind>`: the engine's value lattice, not a pack's. A pack
    /// chooses which nodes carry each kind.
    pub enum LitKind {
        String = "string",
        Number = "number",
        Bool = "bool",
        ArrayRef = "arrayref",
        HashRef = "hashref",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExprCap {
    Call,
    Lit(LitKind),
    ReadVar,
    ReturnValue,
    /// A keyed-value constructor call; `@shape.*` join it by span.
    Shape,
}

spelled! {
    pub enum ObsKind { Numeric = "numeric", String = "string" }
}

spelled! {
    pub enum NarrowCap { Var = "var", Type = "type", Guard = "guard", Block = "block" }
}

spelled! {
    pub enum MoveCap { Scope = "scope", Name = "name", Var = "var", Call = "call" }
}

spelled! {
    pub enum MemberCap { Recv = "recv", Op = "op" }
}

spelled! {
    pub enum ArityCap { Args = "args", Sig = "sig" }
}

spelled! {
    pub enum DomainCap { Slot = "slot", Value = "value" }
}

spelled! {
    pub enum ShapeCap { Ctor = "ctor", Key = "key" }
}

spelled! {
    pub enum CmdCap {
        /// The command identifier.
        Name = "",
        Arg = "arg",
    }
}

/// Whether a `@def.<kind>` capture is the def node or its name token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefPart {
    Node,
    Name,
}

/// One capture of a pack query document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capture {
    Def { kind: DefKind, part: DefPart },
    Ref(RefKind),
    Scope(ScopeCap),
    Context(ContextKind),
    /// A base class, paired with the match's `@def.class.name`.
    Parent,
    Import(ImportCap),
    Alias(AliasPart),
    MacroAlias(AliasPart),
    Tmpl(TmplCap),
    Flow(FlowCap),
    Expr(ExprCap),
    Obs(ObsKind),
    Narrow(NarrowCap),
    Move(MoveCap),
    /// A control-flow construct's span (`@guard.region`).
    GuardRegion,
    /// An unevaluated operand (`sizeof(...)`, `decltype(...)`).
    Unevaluated,
    ParamRegion,
    Member(MemberCap),
    Arity(ArityCap),
    Domain(DomainCap),
    Shape(ShapeCap),
    Cmd(CmdCap),
    /// A pointer/reference declarator chain, peeled to its leaf.
    NestedTarget,
    /// An out-of-line def's `Class::` owner.
    Qualifier,
    /// An out-of-line function definition.
    OolDef,
    RetType,
    TypeAnnot,
    /// A token whose text rides onto the match's def symbol as an attribute.
    SymAttr,
    /// The primary a class specialization specializes.
    SpecPrimary,
    /// An inline namespace's name token.
    NsInline,
    /// A field typed by an anonymous aggregate.
    AnonAggMember,
}

/// A capture name outside the vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCapture(pub String);

impl fmt::Display for UnknownCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown capture @{}", self.0)
    }
}

impl Capture {
    pub fn parse(name: &str) -> Result<Capture, UnknownCapture> {
        Self::parse_segments(name).ok_or_else(|| UnknownCapture(name.to_string()))
    }

    fn parse_segments(name: &str) -> Option<Capture> {
        use Capture as C;
        let segs: Vec<&str> = name.split('.').collect();
        Some(match segs.as_slice() {
            ["def", k] => C::Def { kind: DefKind::parse(k)?, part: DefPart::Node },
            ["def", k, "name"] => C::Def { kind: DefKind::parse(k)?, part: DefPart::Name },
            ["ref", k] => C::Ref(RefKind::parse(k)?),
            ["scope"] => C::Scope(ScopeCap::Block),
            ["scope", k] if !k.is_empty() => C::Scope(ScopeCap::parse(k)?),
            ["context", k] => C::Context(ContextKind::parse(k)?),
            ["parent"] => C::Parent,
            ["import"] => C::Import(ImportCap::Statement),
            ["import", k] if !k.is_empty() => C::Import(ImportCap::parse(k)?),
            ["alias", k] => C::Alias(AliasPart::parse(k)?),
            ["macro", "alias", k] => C::MacroAlias(AliasPart::parse(k)?),
            ["tmpl", k] => C::Tmpl(TmplCap::parse(k)?),
            ["flow", k] => C::Flow(FlowCap::parse(k)?),
            ["expr", "call"] => C::Expr(ExprCap::Call),
            ["expr", "lit", k] => C::Expr(ExprCap::Lit(LitKind::parse(k)?)),
            ["expr", "read", "var"] => C::Expr(ExprCap::ReadVar),
            ["expr", "return", "value"] => C::Expr(ExprCap::ReturnValue),
            ["expr", "shape"] => C::Expr(ExprCap::Shape),
            ["obs", k] => C::Obs(ObsKind::parse(k)?),
            ["narrow", k] => C::Narrow(NarrowCap::parse(k)?),
            ["move", k] => C::Move(MoveCap::parse(k)?),
            ["guard", "region"] => C::GuardRegion,
            ["unevaluated"] => C::Unevaluated,
            ["param", "region"] => C::ParamRegion,
            ["member", k] => C::Member(MemberCap::parse(k)?),
            ["arity", k] => C::Arity(ArityCap::parse(k)?),
            ["domain", k] => C::Domain(DomainCap::parse(k)?),
            ["shape", k] => C::Shape(ShapeCap::parse(k)?),
            ["cmd"] => C::Cmd(CmdCap::Name),
            ["cmd", k] if !k.is_empty() => C::Cmd(CmdCap::parse(k)?),
            ["nested", "target"] => C::NestedTarget,
            ["qualifier"] => C::Qualifier,
            ["ool", "def"] => C::OolDef,
            ["rettype"] => C::RetType,
            ["type", "annot"] => C::TypeAnnot,
            ["sym", "attr"] => C::SymAttr,
            ["spec", "primary"] => C::SpecPrimary,
            ["ns", "inline"] => C::NsInline,
            ["anonagg", "member"] => C::AnonAggMember,
            _ => return None,
        })
    }
}

impl fmt::Display for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Capture as C;
        // A family whose bare spelling is the family name itself.
        let family = |f: &mut fmt::Formatter<'_>, head: &str, tail: &str| {
            if tail.is_empty() { f.write_str(head) } else { write!(f, "{head}.{tail}") }
        };
        match *self {
            C::Def { kind, part: DefPart::Node } => write!(f, "def.{kind}"),
            C::Def { kind, part: DefPart::Name } => write!(f, "def.{kind}.name"),
            C::Ref(k) => write!(f, "ref.{k}"),
            C::Scope(k) => family(f, "scope", k.spelling()),
            C::Context(k) => write!(f, "context.{k}"),
            C::Parent => f.write_str("parent"),
            C::Import(k) => family(f, "import", k.spelling()),
            C::Alias(k) => write!(f, "alias.{k}"),
            C::MacroAlias(k) => write!(f, "macro.alias.{k}"),
            C::Tmpl(k) => write!(f, "tmpl.{k}"),
            C::Flow(k) => write!(f, "flow.{k}"),
            C::Expr(ExprCap::Call) => f.write_str("expr.call"),
            C::Expr(ExprCap::Lit(k)) => write!(f, "expr.lit.{k}"),
            C::Expr(ExprCap::ReadVar) => f.write_str("expr.read.var"),
            C::Expr(ExprCap::ReturnValue) => f.write_str("expr.return.value"),
            C::Expr(ExprCap::Shape) => f.write_str("expr.shape"),
            C::Obs(k) => write!(f, "obs.{k}"),
            C::Narrow(k) => write!(f, "narrow.{k}"),
            C::Move(k) => write!(f, "move.{k}"),
            C::GuardRegion => f.write_str("guard.region"),
            C::Unevaluated => f.write_str("unevaluated"),
            C::ParamRegion => f.write_str("param.region"),
            C::Member(k) => write!(f, "member.{k}"),
            C::Arity(k) => write!(f, "arity.{k}"),
            C::Domain(k) => write!(f, "domain.{k}"),
            C::Shape(k) => write!(f, "shape.{k}"),
            C::Cmd(k) => family(f, "cmd", k.spelling()),
            C::NestedTarget => f.write_str("nested.target"),
            C::Qualifier => f.write_str("qualifier"),
            C::OolDef => f.write_str("ool.def"),
            C::RetType => f.write_str("rettype"),
            C::TypeAnnot => f.write_str("type.annot"),
            C::SymAttr => f.write_str("sym.attr"),
            C::SpecPrimary => f.write_str("spec.primary"),
            C::NsInline => f.write_str("ns.inline"),
            C::AnonAggMember => f.write_str("anonagg.member"),
        }
    }
}

/// A compiled query's capture table, indexed by capture id. `None` marks a
/// predicate-only capture: tree-sitter's `_` prefix (`@_const` under
/// `#eq?`) names a node a predicate reads, never one the extractor does.
pub(super) fn capture_table(names: &[&str]) -> Result<Vec<Option<Capture>>, UnknownCapture> {
    names
        .iter()
        .map(|n| if n.starts_with('_') { Ok(None) } else { Capture::parse(n).map(Some) })
        .collect()
}

impl DefKind {
    /// The symbol kind a def of this kind projects to. A `UnionField` stays
    /// a Variable: its `union` marker drives the outline nesting.
    pub fn sym_kind(self) -> crate::model::file_analysis::SymKind {
        use crate::model::file_analysis::SymKind;
        match self {
            DefKind::Package => SymKind::Package,
            DefKind::Class | DefKind::Union => SymKind::Class,
            // A function-like `#define` is a real callable everywhere; its
            // `macro` marker lets hover say so.
            DefKind::Sub | DefKind::Anon | DefKind::Constant | DefKind::Macro => SymKind::Sub,
            // `using Base::m;` is a Method on the class surface; its
            // `reexport` marker makes resolution see through it.
            DefKind::Method | DefKind::Reexport => SymKind::Method,
            DefKind::Field => SymKind::Field,
            DefKind::Enumerator => SymKind::Enumerator,
            DefKind::Var | DefKind::Local | DefKind::Label | DefKind::UnionField => SymKind::Variable,
        }
    }

    /// The structural marker a def of this kind carries onto
    /// `Symbol.attributes` (and its `SymbolFlags` twin), so consumers ask
    /// the symbol rather than its kind.
    pub fn marker(self) -> Option<&'static str> {
        match self {
            DefKind::Union | DefKind::UnionField => Some("union"),
            DefKind::Reexport => Some("reexport"),
            DefKind::Macro => Some("macro"),
            _ => None,
        }
    }
}

impl LitKind {
    pub fn value_type(self) -> crate::model::file_analysis::InferredType {
        use crate::model::file_analysis::InferredType;
        match self {
            LitKind::String => InferredType::String,
            LitKind::Number => InferredType::Numeric,
            LitKind::Bool => InferredType::Bool,
            LitKind::ArrayRef => InferredType::ArrayRef,
            LitKind::HashRef => InferredType::HashRef,
        }
    }
}

impl ObsKind {
    pub fn observation(self) -> crate::model::witnesses::TypeObservation {
        use crate::model::witnesses::TypeObservation;
        match self {
            ObsKind::Numeric => TypeObservation::NumericUse,
            ObsKind::String => TypeObservation::StringUse,
        }
    }
}

#[cfg(test)]
#[path = "vocab_tests.rs"]
mod tests;

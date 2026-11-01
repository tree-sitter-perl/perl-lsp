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
            #[cfg(test)]
            pub const ALL: &'static [Self] = &[$($name::$var),*];
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
    pub enum DefCap {
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
        /// An instance slot: a key of the per-instance store a write through
        /// a receiver creates (python's `self.x = …`). A class-owned
        /// `HashKeyDef`, Perl's `$self->{x}` slot.
        Slot = "slot",
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
    /// What a `@mark.<flag>` states about the declaration whose NAME token
    /// it captures. The document says it in a pattern of its own, so a
    /// modifier in any order, a keyword that is not the flag's spelling
    /// (`private` → non-public) and a fact about a class read off its body
    /// all reach the symbol as the flag `flag` names.
    pub enum MarkCap {
        Static = "static",
        Abstract = "abstract",
        NonPublic = "non_public",
        Constructor = "constructor",
        Contract = "contract",
        DynamicMembers = "dynamic_members",
    }
}

spelled! {
    /// What a `@ref.<kind>` refers through.
    pub enum RefCap {
        Call = "call",
        Method = "method",
        /// A member token READ as a value (`recv.field` / `recv->field`);
        /// joins `@member.recv` by match.
        MemberValue = "member.value",
        /// A member token CALLED (`recv.f(…)`).
        MemberCall = "member.call",
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
        /// A sub body whose bare names may elide the member receiver.
        SubImplicitReceiver = "sub.implicit_receiver",
        /// A scope whose names are in force only for code written
        /// DIRECTLY in it: a callable nested inside looks a name up in the
        /// scopes around the scope, never in it. Python's class body says
        /// it of its class scope; a document that says nothing keeps the
        /// plain chain. It names what the scope does, not which language
        /// declared it, so `Scope::direct` and the one lookup walk over it
        /// (`lookup_chain`) serve any language that states the same rule.
        Direct = "direct",
        /// A def's header: the code its node holds outside its body
        /// (Python's parameter annotations and defaults, its `-> T`, a
        /// class's base list), which runs in the scope the def is declared
        /// in though the def's own scope covers it. Minted as a scope whose
        /// parent is that declaring scope, so every use written in it, and
        /// every lookup at a point in it, starts outside the def; a name
        /// the header declares (a parameter) still binds in the def's own
        /// scope.
        Header = "header",
    }
}

spelled! {
    /// A sticky container context. A `Namespace` is also the namespace a
    /// class declared inside it joins when spellings resolve through a
    /// use-map; otherwise the kinds are handled alike.
    pub enum ContextKind { Class = "class", Namespace = "namespace", Package = "package" }
}

spelled! {
    pub enum ImportCap {
        /// The whole import statement (pattern anchor).
        Statement = "",
        Name = "name",
        /// An import token that is a source PATH, not a name: C/C++
        /// `#include` splices a file and binds no name.
        Path = "path",
        /// The module an import CALL names (R `library(pkg)`, CMake
        /// `include(f)`), as written.
        Arg = "arg",
        /// The module an import STATEMENT loads, as written, where the
        /// names it binds are rows of their own (python's `from a.b import
        /// C` loads `a.b`).
        Module = "module",
        /// The namespace a group import's names hang off (php's
        /// `use A\B\{C, D}`), joined to each name by match.
        Prefix = "prefix",
        /// A relative import's leading dots (python's `import_prefix` in
        /// `from ..x import C`), one level per separator it holds; what a
        /// level climbs is the language's `ImportPrefix`.
        Relative = "relative",
        /// The name an import binds in place of its own leaf
        /// (`use A\B as C`), joined to the import name by match.
        Alias = "alias",
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
        /// The left side of a write: the place its own read projects
        /// takes the value (`obj.x` in `obj.x = v` is the member of the
        /// class `obj` is). A name's place is its binding, which `Target`
        /// writes and rebinds.
        Place = "place",
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
    /// A member value read (`recv.field`): its `@member.recv` and
    /// `@ref.member.value` share the match.
    Member,
}

spelled! {
    pub enum ObsKind { Numeric = "numeric", String = "string" }
}

spelled! {
    pub enum NarrowCap { Var = "var", Type = "type", Guard = "guard", Arm = "arm" }
}

spelled! {
    pub enum MoveCap { Scope = "scope", Name = "name", Var = "var", Call = "call" }
}

spelled! {
    pub enum MemberCap {
        /// A receiver written as an expression (`self.x`, `f()`, `(*p)`).
        Recv = "recv",
        /// A receiver written as a bare name (`w`, `Widget`): it may spell
        /// a class, and its operator can be checked by its name.
        RecvName = "recv.name",
        Op = "op",
    }
}

spelled! {
    /// Which member operator a token is; the suffix names the model's
    /// `MemberOp`, so a token no pattern names claims no operator.
    pub enum MemberOpCap { Arrow = "arrow", Dot = "dot" }
}

impl MemberOpCap {
    pub fn op(self) -> crate::model::file_analysis::MemberOp {
        match self {
            MemberOpCap::Arrow => crate::model::file_analysis::MemberOp::Arrow,
            MemberOpCap::Dot => crate::model::file_analysis::MemberOp::Dot,
        }
    }
}

spelled! {
    pub enum ArityCap {
        Args = "args",
        Arg = "arg",
        Sig = "sig",
        Spread = "spread",
        /// A declared parameter of the `@arity.sig` it sits in: a required
        /// one, one with a default, or one that takes any number of values.
        Required = "required",
        Optional = "optional",
        Variadic = "variadic",
    }
}

spelled! {
    pub enum DomainCap { Slot = "slot", Value = "value" }
}

spelled! {
    pub enum ShapeCap { Key = "key" }
}

spelled! {
    pub enum CmdCap {
        /// The command identifier.
        Name = "",
        Arg = "arg",
    }
}

spelled! {
    /// The walk a link belongs to, named by its `@link.from.<walk>` suffix.
    /// A walk starts at its root capture and follows only its own links, so
    /// two walks never read each other's edges.
    pub enum LinkWalk {
        /// An out-of-line definition's declarator, down to its qualified name.
        Ool = "ool",
        /// A `@qualifier` owner, to the base name a templated owner files under.
        Qualifier = "qualifier",
        /// A `@nested.target` declarator chain, level by level to its leaf.
        Deref = "deref",
        /// A `@ref.spelled` reference, down its spelling's levels to the
        /// token it rewrites.
        Spelled = "spelled",
    }
}

impl LinkWalk {
    /// The capture a walk starts from.
    pub fn root(self) -> Capture {
        match self {
            LinkWalk::Ool => Capture::OolDef,
            LinkWalk::Qualifier => Capture::Qualifier,
            LinkWalk::Deref => Capture::NestedTarget,
            LinkWalk::Spelled => Capture::RefSpelled,
        }
    }
}

spelled! {
    /// One level of structure the query states and the driver chains to any
    /// depth: the match's `@link.from.<walk>` is the node, each `@link.<rel>`
    /// the node it links to by that relation.
    pub enum LinkCap {
        /// A transparent wrapper's operand (`*p`, `&r`, `(d)` declarators).
        Inner = "inner",
        /// What a definition or function declarator declares.
        Declares = "declares",
        /// A qualified name's scope (`A` in `A::b`).
        Scope = "scope",
        /// A qualified name's name (`b` in `A::b`).
        Name = "name",
        /// A templated name's base name (`Buf` in `Buf<T>`).
        Base = "base",
    }
}

spelled! {
    /// What one level of a `deref` walk is: the document marks the level node
    /// (or, for a qualifier, its child), and the walk reads the marks.
    pub enum DerefCap {
        /// A pointer level (`*p`).
        Pointer = "pointer",
        /// A reference level (`&r`).
        Ref = "ref",
        /// A cv-qualifier on the level that is its parent (`* const`).
        Annot = "annot",
        /// A level the walk descends without a step of its own: grouping
        /// parens, a function pointer's function declarator.
        Pass = "pass",
    }
}

/// Whether a `@def.<kind>` capture is the def node, its name token, or
/// the receiver a member def is written through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefPart {
    Node,
    Name,
    /// The receiver a slot def is written through (python's `self` in
    /// `self.x = …`): the slot belongs to the class the receiver is typed
    /// as at the write.
    Receiver,
}

/// One capture of a pack query document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capture {
    Def { kind: DefCap, part: DefPart },
    /// The one name token two defs of this match declare together (python's
    /// class-body binding: the class's member and the name the body reads).
    /// They are one declaration, linked each way as `Symbol::declared_with`.
    DefPair,
    Ref(RefCap),
    /// The whole written spelling of the match's reference when its token
    /// is only the leaf (`Base\Model` around `Model`): the reference
    /// resolves by the spelling and rewrites only the leaf. Where the
    /// spelling nests (`a::b::f` is `a::(b::f)`), the `spelled` walk carries
    /// the reference from the node the document tagged down to that leaf.
    RefSpelled,
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
    /// A conditionally-run arm (`@arm`): an observation made inside holds
    /// only within it.
    Arm,
    Narrow(NarrowCap),
    Move(MoveCap),
    /// A control-flow construct's span (`@guard.region`).
    GuardRegion,
    /// An unevaluated operand (`sizeof(...)`, `decltype(...)`).
    Unevaluated,
    ParamRegion,
    Member(MemberCap),
    /// The operator token a member access wrote, by which operator it is.
    MemberOp(MemberOpCap),
    /// A node that names the receiver of the method around it — declared
    /// (python's `self` parameter) or not (C++ `this`): the object, never a
    /// member and never a class spelling.
    Receiver,
    /// A fact the document states about the declaration at this name token.
    Mark(MarkCap),
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
    /// The node a pattern's links start from, and the walk they belong to.
    LinkFrom(LinkWalk),
    /// One level of a declarator chain, as the `deref` walk reads it.
    Deref(DerefCap),
    /// A declarator chain's leaf; the suffix names the def the leaf mints.
    DerefLeaf(DefCap),
    Link(LinkCap),
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
            ["def", "pair"] => C::DefPair,
            ["def", k] => C::Def { kind: DefCap::parse(k)?, part: DefPart::Node },
            ["def", k, "name"] => C::Def { kind: DefCap::parse(k)?, part: DefPart::Name },
            ["def", k, "receiver"] => C::Def { kind: DefCap::parse(k)?, part: DefPart::Receiver },
            ["ref", "spelled"] => C::RefSpelled,
            ["ref", rest @ ..] => C::Ref(RefCap::parse(&rest.join("."))?),
            ["scope"] => C::Scope(ScopeCap::Block),
            ["scope", "sub", "implicit_receiver"] => C::Scope(ScopeCap::SubImplicitReceiver),
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
            ["expr", "member"] => C::Expr(ExprCap::Member),
            ["obs", k] => C::Obs(ObsKind::parse(k)?),
            ["arm"] => C::Arm,
            ["narrow", k] => C::Narrow(NarrowCap::parse(k)?),
            ["move", k] => C::Move(MoveCap::parse(k)?),
            ["guard", "region"] => C::GuardRegion,
            ["unevaluated"] => C::Unevaluated,
            ["param", "region"] => C::ParamRegion,
            ["receiver"] => C::Receiver,
            ["mark", k] => C::Mark(MarkCap::parse(k)?),
            ["member", "op", k] => C::MemberOp(MemberOpCap::parse(k)?),
            ["member", rest @ ..] => C::Member(MemberCap::parse(&rest.join("."))?),
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
            ["link", "from", w] => C::LinkFrom(LinkWalk::parse(w)?),
            ["deref", "leaf", k] => C::DerefLeaf(DefCap::parse(k)?),
            ["deref", k] => C::Deref(DerefCap::parse(k)?),
            ["link", k] => C::Link(LinkCap::parse(k)?),
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
            C::DefPair => f.write_str("def.pair"),
            C::Def { kind, part: DefPart::Name } => write!(f, "def.{kind}.name"),
            C::Def { kind, part: DefPart::Receiver } => write!(f, "def.{kind}.receiver"),
            C::Ref(k) => write!(f, "ref.{k}"),
            C::RefSpelled => f.write_str("ref.spelled"),
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
            C::Expr(ExprCap::Member) => f.write_str("expr.member"),
            C::Obs(k) => write!(f, "obs.{k}"),
            C::Arm => f.write_str("arm"),
            C::Narrow(k) => write!(f, "narrow.{k}"),
            C::Move(k) => write!(f, "move.{k}"),
            C::GuardRegion => f.write_str("guard.region"),
            C::Unevaluated => f.write_str("unevaluated"),
            C::ParamRegion => f.write_str("param.region"),
            C::Member(k) => write!(f, "member.{k}"),
            C::MemberOp(k) => write!(f, "member.op.{k}"),
            C::Receiver => f.write_str("receiver"),
            C::Mark(k) => write!(f, "mark.{k}"),
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
            C::LinkFrom(w) => write!(f, "link.from.{w}"),
            C::Deref(k) => write!(f, "deref.{k}"),
            C::DerefLeaf(k) => write!(f, "deref.leaf.{k}"),
            C::Link(k) => write!(f, "link.{k}"),
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

/// Why a query document's links are malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LinkError {
    /// A pattern with a `@link.<rel>` lacks exactly one `@link.from.<walk>`,
    /// or one with a `from` links nothing. Carries the pattern's start byte.
    Pattern(usize),
    /// The document links for a walk whose root capture it never uses, so
    /// nothing would read those links.
    Unrooted(LinkWalk),
}

/// A `@link.<rel>` names a node one level from its match's `@link.from.<walk>`,
/// so a pattern that links captures exactly one `from`, and a walk the
/// document links for must have its root in the document. Checked when the
/// query compiles.
pub(super) fn check_links(query: &tree_sitter::Query, captures: &[Option<Capture>]) -> Result<(), LinkError> {
    use tree_sitter::CaptureQuantifier as Q;
    for p in 0..query.pattern_count() {
        let present: Vec<(Capture, Q)> = captures
            .iter()
            .zip(query.capture_quantifiers(p))
            .filter_map(|(c, q)| c.filter(|_| *q != Q::Zero).map(|c| (c, *q)))
            .collect();
        let links = present.iter().any(|(c, _)| matches!(c, Capture::Link(_)));
        let froms: Vec<Q> =
            present.iter().filter(|(c, _)| matches!(c, Capture::LinkFrom(_))).map(|(_, q)| *q).collect();
        if (links && froms != [Q::One]) || (!links && !froms.is_empty()) {
            return Err(LinkError::Pattern(query.start_byte_for_pattern(p)));
        }
    }
    for walk in captures.iter().flatten().filter_map(|c| match c {
        Capture::LinkFrom(w) => Some(*w),
        _ => None,
    }) {
        if !captures.contains(&Some(walk.root())) {
            return Err(LinkError::Unrooted(walk));
        }
    }
    Ok(())
}

impl DefCap {
    /// The symbol kind a def of this kind projects to. A `UnionField` stays
    /// a Variable: its `union` marker drives the outline nesting.
    pub fn sym_kind(self) -> crate::model::file_analysis::SymKind {
        use crate::model::file_analysis::SymKind;
        match self {
            DefCap::Package => SymKind::Package,
            DefCap::Class | DefCap::Union => SymKind::Class,
            // A function-like `#define` is a real callable everywhere; its
            // `macro` marker lets hover say so.
            DefCap::Sub | DefCap::Anon | DefCap::Constant | DefCap::Macro => SymKind::Sub,
            // `using Base::m;` is a Method on the class surface; its
            // `reexport` marker makes resolution see through it.
            DefCap::Method | DefCap::Reexport => SymKind::Method,
            DefCap::Field => SymKind::Field,
            DefCap::Slot => SymKind::HashKeyDef,
            DefCap::Enumerator => SymKind::Enumerator,
            DefCap::Var | DefCap::Local | DefCap::Label | DefCap::UnionField => SymKind::Variable,
        }
    }

    /// The structural marker a def of this kind carries onto
    /// `Symbol.attributes` (and its `SymbolFlags` twin), so consumers ask
    /// the symbol rather than its kind.
    pub fn marker(self) -> Option<&'static str> {
        match self {
            DefCap::Union | DefCap::UnionField => Some("union"),
            DefCap::Reexport => Some("reexport"),
            DefCap::Macro => Some("macro"),
            _ => None,
        }
    }
}

impl MarkCap {
    pub fn flag(self) -> crate::model::file_analysis::SymbolFlags {
        use crate::model::file_analysis::SymbolFlags as F;
        match self {
            MarkCap::Static => F::STATIC,
            MarkCap::Abstract => F::ABSTRACT,
            MarkCap::NonPublic => F::NON_PUBLIC,
            MarkCap::Constructor => F::CONSTRUCTOR,
            MarkCap::Contract => F::CONTRACT,
            MarkCap::DynamicMembers => F::DYNAMIC_MEMBERS,
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

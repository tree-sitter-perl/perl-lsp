//! Core vocabulary types: spans, scopes, symbols, refs, deref stacks — the
//! serde-cacheable building blocks every layer speaks.

use super::*;

// ---- Serde proxy for tree_sitter::Point ----

/// Remote-derive proxy for `tree_sitter::Point`, which doesn't implement serde.
/// Fields mirror `Point` exactly — use `#[serde(with = "PointDef")]` on Point fields.
#[derive(Serialize, Deserialize)]
#[serde(remote = "Point")]
pub(crate) struct PointDef {
    pub row: usize,
    pub column: usize,
}

/// Helper module for `Option<Point>` serialization via the remote-derived proxy.
pub(crate) mod point_opt_serde {
    use super::PointDef;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tree_sitter::Point;

    pub fn serialize<S: Serializer>(val: &Option<Point>, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct W<'a>(#[serde(with = "PointDef")] &'a Point);
        val.as_ref().map(W).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Point>, D::Error> {
        #[derive(Deserialize)]
        struct W(#[serde(with = "PointDef")] Point);
        Option::<W>::deserialize(d).map(|o| o.map(|W(p)| p))
    }
}

// ---- Shared types ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    #[serde(with = "PointDef")]
    pub start: Point,
    #[serde(with = "PointDef")]
    pub end: Point,
}


/// A single `#define` — the identity/navigation lane's view of a macro (the
/// type lane models the same `#define` as a `TypeName` edge; this is the
/// symbol/edge side goto-def consults). One per `#define`, so a config-variant
/// macro `#define`d N times under N different `#if`s is N `MacroDef`s sharing a
/// name — the complete set `cpp_macro_model` ranks. `#[serde(default)]` on the
/// field: cache blobs written before this lane deserialize with no macro defs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroDef {
    pub name: String,
    /// `Some(params)` = function-like; `None` = object-like.
    pub params: Option<Vec<String>>,
    pub body: String,
    /// Enclosing `#if`/`#ifdef`/`#else` conditions, OUTERMOST first — the config
    /// guard trail (`cpp_macro_model`). Empty = unconditional. What the
    /// reachability rank is computed from.
    pub guards: Vec<String>,
    /// The `#define NAME` span — where goto-def lands.
    pub selection_span: Span,
    /// A direct-delegation wrapper's callee: when the body is a single call
    /// `G(args)`, `Some("G")` — the see-through target goto-def also offers
    /// (`SvREFCNT_inc → Perl_SvREFCNT_inc`). `None` otherwise.
    pub delegate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: FoldKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum FoldKind {
    Region,
    Comment,
}

// ---- IDs ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScopeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

// ---- Scope ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub span: Span,
    /// The enclosing package/class name at this scope level.
    /// For `package Foo;` regions, this is "Foo".
    /// Inherited from parent when not overridden.
    pub package: Option<String>,
    /// The callable this scope is the body of (a Sub / Method scope's own
    /// symbol), minted when the scope is pushed. The one hop from a scope
    /// to its parameters — `binding_site_of`, the enclosing-callable
    /// question — so neither is a span scan. `None` for every other scope.
    #[serde(default)]
    pub owner: Option<SymbolId>,
    /// A bare name in this scope may elide the member receiver — the
    /// language's own rule, stated by the capture that mints the scope
    /// (`@scope.sub.implicit_receiver`). The chain carries it: a block or
    /// lambda body nested in such a scope elides too, so a consumer walks
    /// to the nearest scope that declares it and reads its `owner`'s
    /// package. Whether a class is in fact there is resolution, not syntax
    /// — a free function's body carries the flag and no owning package, so
    /// nothing binds.
    #[serde(default)]
    pub implicit_receiver: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum ScopeKind {
    File,
    /// `class Foo { ... }` block.
    Class { name: String },
    /// `sub foo { ... }` block.
    Sub { name: String },
    /// `method foo { ... }` block.
    Method { name: String },
    /// Bare `{ ... }`, if/while/for bodies, etc.
    Block,
    /// `for my $x (...) { }` — loop variable scoped to block.
    ForLoop { var: String },
}

// ---- Package context ----

/// One plugin-emitted diagnostic — the payload of
/// `EmitAction::Diagnostic`, stamped with the emitting plugin's id.
/// `severity` is an open string (`"error"` / `"warning"` / `"info"` /
/// anything else renders as hint) — the vocabulary is the plugin's,
/// core only maps it to the LSP enum at render time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDiagnostic {
    pub message: String,
    pub span: Span,
    pub severity: String,
    /// Diagnostic code shown in the editor (e.g. `"ddp-debug-left"`).
    pub code: String,
    /// Emitting plugin id — surfaced as the diagnostic source.
    pub plugin_id: String,
}

/// Flat per-file record of which `package`/`class` declaration governs a
/// byte range. Independent of the lexical scope tree — `package Foo;` is
/// not a lexical boundary in Perl, so collapsing the two concepts would
/// force shims (lift `my` past the package "scope", merge buckets in the
/// outline) we'd rather not have.
///
/// Query via `FileAnalysis::package_at(point)` — innermost (latest-starting)
/// containing range wins for nested `package Foo { … package Bar; … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRange {
    pub package: String,
    pub span: Span,
    pub kind: PackageKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackageKind {
    /// `package Foo;` / `class Foo;` — flows until the next sibling
    /// declaration or end of file/block.
    Statement,
    /// `package Foo { … }` / `class Foo { … }` — span equals the block.
    Block,
}

// ---- Namespace ----

/// Origin tag for symbols and refs: native Perl vs produced by a framework
/// rule (built-in or plugin). Downstream features (completion bucketing,
/// diagnostic suppression, plugin-aware rename) read this tag instead of
/// reconstructing provenance from names and positions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Namespace {
    /// Native Perl: subs, variables, packages, classes, hash keys extracted
    /// directly from the CST by the builder.
    Language,
    /// Produced by a framework plugin. `id` is the plugin identifier
    /// (e.g. `"mojo-base"`, `"moo"`, `"dbic-columns"`), allowing per-plugin
    /// filtering, rename coordination, and diagnostic attribution.
    Framework { id: String },
}

impl Default for Namespace {
    fn default() -> Self { Self::Language }
}

impl Namespace {
    pub fn framework(id: impl Into<String>) -> Self {
        Self::Framework { id: id.into() }
    }

    pub fn is_framework(&self) -> bool {
        matches!(self, Self::Framework { .. })
    }
}

impl Span {
    /// `other` lies within this span (inclusive at both ends).
    pub fn contains(&self, other: &Span) -> bool {
        (self.start.row, self.start.column) <= (other.start.row, other.start.column)
            && (other.end.row, other.end.column) <= (self.end.row, self.end.column)
    }
}

/// How a language spells names: the separator its qualified names split on
/// and the sigils its variables carry. Every key function
/// (`split_qualified`, `name_match_key`, `is_bareword_class_name`) takes
/// the spellings of the language whose name it is handling, so no
/// separator or sigil is ever assumed and no language's spelling can
/// mis-key another's. Perl declares its own (`conventions::PERL_SPELLINGS`)
/// exactly as a pack declares its (`LangPack::names`); an analysis carries
/// the ones it was built under (`PackFacts::names`). Borrowed for the
/// languages the binary declares, owned when read back from a blob.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NameSpellings {
    /// The namespace separator (`::`, `\`); `None` for a language whose
    /// names are never qualified (a flat linkage).
    pub namespace_sep: Option<std::borrow::Cow<'static, str>>,
    /// Variable sigils a name may lead with (`$`, `@`, `%`); a language
    /// with none declares none, so a `$` in its identifiers is identifier
    /// text.
    pub sigils: std::borrow::Cow<'static, [char]>,
    /// What a written class spelling denotes.
    pub class_spelling: ClassSpelling,
}

/// What a class spelling written in a file denotes — the capability the
/// use-map questions (`use_map`, `identity_namespace`, `spelled_identity`)
/// gate on. A separator alone does not decide it: Perl qualifies with
/// `::` and every spelling is its own identity, while a use-map language
/// resolves a bare leaf through the file's imports and namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClassSpelling {
    /// The spelling IS the identity (Perl's `Foo::Bar`, C's flat linkage).
    #[default]
    Identity,
    /// The spelling resolves through the file's use-map: its import rows,
    /// aliases and own namespace (`use A\B\Collection;` then `Collection`).
    UseMap,
}

impl NameSpellings {
    /// A language whose names carry no separator and no sigil.
    pub const NONE: NameSpellings = NameSpellings {
        namespace_sep: None,
        sigils: std::borrow::Cow::Borrowed(&[]),
        class_spelling: ClassSpelling::Identity,
    };

    /// A language with a namespace separator, no sigils, and spellings
    /// that are identities as written (C++ until its `using` directives
    /// are modeled).
    pub const fn with_separator(sep: &'static str) -> NameSpellings {
        NameSpellings {
            namespace_sep: Some(std::borrow::Cow::Borrowed(sep)),
            sigils: std::borrow::Cow::Borrowed(&[]),
            class_spelling: ClassSpelling::Identity,
        }
    }

    pub fn sep(&self) -> Option<&str> {
        self.namespace_sep.as_deref()
    }

    /// The separator a use-map resolves with — `None` for a language whose
    /// class spellings are identities as written, whether or not it
    /// qualifies names. The one gate the use-map questions read.
    pub fn use_map_sep(&self) -> Option<&str> {
        match self.class_spelling {
            ClassSpelling::UseMap => self.sep(),
            ClassSpelling::Identity => None,
        }
    }

    pub fn is_sigil(&self, c: char) -> bool {
        self.sigils.contains(&c)
    }
}

// ---- Symbol ----

bitflags::bitflags! {
    /// Declaration facts a symbol carries, as a closed flag set. Every
    /// language maps its own attribute spellings onto these where it
    /// mints the symbol — a pack at skeleton conversion through
    /// `TryFrom<&str>` over the canonical names, Perl through
    /// `conventions::field_attribute_flag` — and consumers ask
    /// `contains(..)`, never compare an attribute string. Closed on
    /// purpose: every flag so far has a language-generic meaning, and the
    /// set rides the cache blob (`docs/adr/symbol-flags.md`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct SymbolFlags: u32 {
        /// A class-level member (`static`), reached through the class, not an instance.
        const STATIC = 1 << 0;
        /// A `Class` symbol that is an interface: its members are contracts, and a
        /// concrete definer outranks it on the MRO.
        const INTERFACE = 1 << 1;
        /// Declared abstract.
        const ABSTRACT = 1 << 2;
        /// A default-named container (`(union)`): structure, not an addressable name.
        const ANONYMOUS = 1 << 3;
        /// Not public: completes only from inside its own class's body.
        const NON_PUBLIC = 1 << 4;
        /// A union container: its body nests its members in the outline.
        const UNION = 1 << 5;
        /// An `extern` declaration: a definition elsewhere is the landing.
        const EXTERN = 1 << 6;
        /// An inline namespace: its members are visible from the parent.
        const INLINE = 1 << 7;
        /// A re-export (`using Base::m;`): API surface, not a definition.
        const REEXPORT = 1 << 8;
        /// An include-guard `#define`: compilation plumbing, folded from listings.
        const INCLUDE_GUARD = 1 << 9;
        /// A function-like `#define`: a real callable that hover labels a macro.
        const MACRO = 1 << 10;
        /// Marked deprecated (the notice text rides `Presentation::deprecation`).
        const DEPRECATED = 1 << 11;
        /// A storage slot with a constructor key (Corinna `:param`, a promoted
        /// constructor parameter): `Class->new(name => …)` binds it.
        const PARAM = 1 << 12;
        /// A storage slot with a generated reader (Corinna `:reader`, Moo
        /// `is => 'ro'`).
        const READER = 1 << 13;
        /// A storage slot with a generated writer (Corinna `:writer` /
        /// `:mutator` / `:accessor`, Moo `is => 'rw'`).
        const WRITER = 1 << 14;
        /// A handler registered on a class-named rail (`@def.handler.class.<rail>`
        /// / `.by.<rail>`): it sits on another symbol's token (a listener's
        /// `handle`), so listings show that symbol, not this one.
        const CLASS_RAIL = 1 << 15;
        /// This callable reads arguments it never declared (php
        /// `func_get_args`): its written signature does not bound what a
        /// caller may pass, so the arity lanes must not call an extra
        /// argument an error.
        const DYNAMIC_ARGS = 1 << 16;
        /// This callable materializes variables no declaration names (php
        /// `extract`, `eval`): a read of an undeclared name inside it is
        /// not evidence of a typo, so the undefined-variable lane stays
        /// silent.
        const DYNAMIC_VARS = 1 << 17;
        /// A `Class` symbol that is a trait (php) / mixin: its members are
        /// composed into the using class, so it declares an API the model
        /// resolves without the trait ever being an instance's class.
        const TRAIT = 1 << 18;
        /// A `Class` symbol that is an enumeration: its cases are its
        /// members, and the language gives every enum the same extra ones.
        const ENUM = 1 << 19;
        /// A callable that DECLARES an obligation without meeting it — an
        /// interface method, an abstract method, a Perl `requires` marker.
        /// Not `ABSTRACT`: an interface's methods carry no `abstract`
        /// token, and the container's own flag is a different fact.
        const CONTRACT = 1 << 20;
        /// The DECLARATION was synthesized from a documentation row (a php
        /// docblock `@method`), so the source has no signature to read or
        /// annotate. Provenance of the declaration, not of the doc text —
        /// `Presentation::doc` answers a different question.
        const DOC_DECLARED = 1 << 21;
        /// The object the enclosing method runs on (`$this`, `this`, a
        /// python `self`/`cls` parameter): lexically inside the class body
        /// and tagged with its package, but the instance itself and never
        /// one of its members. Minted from the receiver capture, so no
        /// consumer matches a receiver's spelling.
        const RECEIVER = 1 << 22;
        /// The constructor of the class that declares it (php
        /// `__construct`): a `new Foo(...)` invokes it, and its name belongs
        /// to the language, so nothing renames it. Minted from the
        /// constructor capture; Perl's `new` is a name convention and stays
        /// in `conventions`.
        const CONSTRUCTOR = 1 << 23;
        /// A binding written to be DISCARDED (php `$_` in `foreach ($a as $k
        /// => $_)`): declared, never read on purpose, so the unused-variable
        /// lane stays silent on it.
        const THROWAWAY = 1 << 24;
        /// The declaration has no token of its own — the LANGUAGE provides
        /// the member (php's `->value` / `::cases()` on every enum), so the
        /// extractor mints it at the container's name. Resolvable,
        /// completable and hoverable like any member; never a dead-code
        /// candidate, because nothing in the source could reference it into
        /// existence.
        const SYNTHESIZED = 1 << 25;
        /// A binding written to REACH another slot's storage (php `$h =
        /// &$opts['h']`): the write IS the point of it, so the liveness lanes
        /// never ask whether anything read it.
        const ALIAS = 1 << 26;
        /// This class answers ANY member name at runtime — a php `__call`,
        /// a Perl `AUTOLOAD` — so its declared member set is not its
        /// surface. Minted on the CLASS by whoever sees the catch-all
        /// declaration; the lanes ask `class_answers_any_member`.
        const DYNAMIC_MEMBERS = 1 << 27;
        /// A stored slot whose VALUE is called (a C function-pointer member,
        /// `int (*read)(char *)`). The declarator says so, so a call landing
        /// on the slot asks the declaration instead of a callback-name list.
        const CALLABLE_VALUE = 1 << 28;
    }
}

/// The wire form is the bare bit set — the same `u32` the cache blob has
/// always carried, so a flag added at the tail reads old blobs unchanged.
impl Serialize for SymbolFlags {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.bits().serialize(s)
    }
}

impl<'de> Deserialize<'de> for SymbolFlags {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u32::deserialize(d).map(SymbolFlags::from_bits_retain)
    }
}

/// An attribute spelling no flag answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAttribute(pub String);

impl std::fmt::Display for UnknownAttribute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown symbol attribute `{}`", self.0)
    }
}

/// The canonical attribute vocabulary — the ONE table from a spelling to
/// a flag. A pack's skeleton conversion and its overlay validation both
/// go through it, so a spelling that mints nothing is an error at the
/// producer rather than a silent skip.
impl TryFrom<&str> for SymbolFlags {
    type Error = UnknownAttribute;
    fn try_from(name: &str) -> Result<Self, Self::Error> {
        Ok(match name {
            "static" => SymbolFlags::STATIC,
            "interface" => SymbolFlags::INTERFACE,
            "abstract" => SymbolFlags::ABSTRACT,
            "anonymous" => SymbolFlags::ANONYMOUS,
            "non_public" => SymbolFlags::NON_PUBLIC,
            "union" => SymbolFlags::UNION,
            "extern" => SymbolFlags::EXTERN,
            "inline" => SymbolFlags::INLINE,
            "reexport" => SymbolFlags::REEXPORT,
            "include_guard" => SymbolFlags::INCLUDE_GUARD,
            "macro" => SymbolFlags::MACRO,
            "deprecated" => SymbolFlags::DEPRECATED,
            "param" => SymbolFlags::PARAM,
            "reader" => SymbolFlags::READER,
            "writer" => SymbolFlags::WRITER,
            "class_rail" => SymbolFlags::CLASS_RAIL,
            "dynamic_args" => SymbolFlags::DYNAMIC_ARGS,
            "dynamic_vars" => SymbolFlags::DYNAMIC_VARS,
            "trait" => SymbolFlags::TRAIT,
            "enum" => SymbolFlags::ENUM,
            "contract" => SymbolFlags::CONTRACT,
            "documented" => SymbolFlags::DOC_DECLARED,
            "synthesized" => SymbolFlags::SYNTHESIZED,
            "alias" => SymbolFlags::ALIAS,
            "dynamic_members" => SymbolFlags::DYNAMIC_MEMBERS,
            "receiver" => SymbolFlags::RECEIVER,
            "constructor" => SymbolFlags::CONSTRUCTOR,
            "throwaway" => SymbolFlags::THROWAWAY,
            "callable_value" => SymbolFlags::CALLABLE_VALUE,
            other => return Err(UnknownAttribute(other.to_string())),
        })
    }
}

/// How a symbol presents to humans — the ONE policy home for listing
/// views (document outline, workspace-symbol, heatmap, completion
/// icons). Minted at symbol synthesis by whoever creates the symbol
/// (builder, plugin emit, pack skeleton conversion); every view reads
/// it and never re-derives presentation from the detail. Kind-semantic
/// facts (`is_constant`, `opaque_return`, `lexical`) stay on
/// `SymbolDetail` — they change behavior, not rendering.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Presentation {
    /// Suppress this symbol in listing views. Set for presentation
    /// duplicates (the arity-variant accessor twin sharing its getter's
    /// name/span), plugin-synthesized DSL infrastructure
    /// (Mojolicious::Lite's `get`/`post`/`app`/…), anonymous subs, and
    /// include-guard `#define`s — hover/gd/completion still resolve the
    /// name (rule #7); the outline stays focused on user-visible
    /// structure.
    #[serde(default)]
    pub hide_in_outline: bool,
    /// The declaration's documentation text (a php docblock's summary
    /// paragraph, rendered under the hover signature). Types parsed from
    /// the same comment ride the witness bag, never this string.
    #[serde(default)]
    pub doc: Option<String>,
    /// The deprecation notice (`@deprecated text`), shown by the lane that
    /// flags uses; the `deprecated` symbol attribute is the flag itself.
    #[serde(default)]
    pub deprecation: Option<String>,
    /// Plugin's final word on the LSP kind this symbol renders as
    /// (helper/route/task/event/…). Framework-synthesized entities
    /// resolve/complete/goto-def like regular symbols; `None` leaves
    /// the default `SymKind` → LSP mapping.
    #[serde(default)]
    pub display: Option<HandlerDisplay>,
    /// Outline-only display-name override. When set, the outline uses
    /// this verbatim instead of `name` (and drops any kind prefix).
    /// A chained Mojo helper leaf has `name: "create"` (so method
    /// resolution works on `$c->users->create`) but labels itself
    /// `"users.create"`; a mojo-lite route prepends the HTTP verb.
    /// Doesn't affect resolution, rename, or workspace-symbol.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymKind,
    pub span: Span,
    pub selection_span: Span,
    /// Scope this symbol is declared in.
    pub scope: ScopeId,
    /// The package this symbol belongs to (captured at creation time).
    pub package: Option<String>,
    /// Kind-specific extra data.
    pub detail: SymbolDetail,
    /// Provenance tag. Defaults to `Language` for builder-native symbols;
    /// framework plugins stamp their plugin id.
    #[serde(default)]
    pub namespace: Namespace,
    /// How this symbol presents in listing views — see [`Presentation`].
    #[serde(default)]
    pub presentation: Presentation,
    /// Free-string annotations the language pack attaches to this symbol
    /// — the pack's OWN vocabulary (a php `readonly`, the signal a recovered
    /// C++ attribute macro carried), surfaced in pack hover. Display only:
    /// the model reasons on `flags`, never on these strings (rule #12).
    #[serde(default)]
    pub attributes: Vec<String>,
    /// The closed set of declaration facts the model reasons on, minted by
    /// whoever creates the symbol — the pack maps its attribute vocabulary
    /// onto them at conversion, the Perl builder sets them directly.
    #[serde(default)]
    pub flags: SymbolFlags,
    /// The OTHER symbol the same declaration token minted: a `has` accessor
    /// and its constructor key, a promoted constructor parameter and its
    /// field. Minted where the pair is synthesized, so no consumer
    /// re-derives the pairing from span equality or column arithmetic
    /// (rule #11).
    #[serde(default)]
    pub declared_with: Option<SymbolId>,
    /// The pointer/reference declarator stack a typed variable carries,
    /// outermost→leaf (`Box** pp` → `[Pointer, Pointer]`, `Box*& rp` →
    /// `[Reference, Pointer]`). Pointer-ness is dropped for type RESOLUTION
    /// (the leaf class answers member access), so this rides alongside for
    /// the consumers that need the real shape: hover renders the exact type
    /// (`pp: Box**`), and member-access DX knows which operator the depth
    /// requires (`pp->` wants `(*pp)->`). Empty for non-pointer symbols and
    /// every non-pack language. See `docs/adr/pointer-stack.md`.
    #[serde(default)]
    pub deref_stack: Vec<DerefStep>,
    /// Declared parameter arity for a callable (`Sub`/`Method`), minted by a
    /// pack that reads the parameter list structurally. Drives overload
    /// arity ranking. `None` for non-callables and languages that carry
    /// params elsewhere (Perl keeps them in `SymbolDetail::Sub` — ask
    /// `param_arity()`, which reads both sources).
    #[serde(default)]
    pub arity: Option<ParamArity>,
}

/// A callable's declared parameter shape, as a set of counts — the fuel for
/// overload arity ranking. Structural: whoever mints it counts the parameter
/// list; the "which overload wins" interpretation lives on `fit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamArity {
    /// Declared parameters, NOT counting a trailing `...`.
    pub total: usize,
    /// Parameters without a default value — the minimum acceptable arg count.
    pub required: usize,
    /// A trailing `...` (C variadic / template pack): any arg count ≥
    /// `required` is accepted.
    pub variadic: bool,
}

impl ParamArity {
    /// How well `argc` written arguments fit this signature, as a ranking key
    /// (higher = better) — never a hard filter, since overload sets stay
    /// visible unpruned. `2` = exact (`argc == total`, no variadic); `1` =
    /// compatible (defaults fill the gap, or a variadic tail absorbs the
    /// extra); `0` = mismatch (too few required, or too many for a fixed arity).
    pub fn fit(&self, argc: usize) -> u8 {
        let compatible = argc >= self.required && (self.variadic || argc <= self.total);
        if !compatible {
            0
        } else if !self.variadic && argc == self.total {
            2
        } else {
            1
        }
    }
}

/// One level of a pointer/reference declarator chain. `annotations` holds
/// the per-level qualifiers as written (`const`, `volatile`, `restrict`,
/// `_Atomic`, …) — kept GENERIC rather than typed flags so new qualifiers
/// and the diagnostics that read them (const-correctness: "write through a
/// `const` pointer") needn't reshape the type. They don't change deref depth
/// — display + diagnostics only, never navigation. Mirrors the free-string
/// approach of `Symbol.attributes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerefStep {
    pub kind: DerefKind,
    #[serde(default)]
    pub annotations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerefKind {
    Pointer,
    Reference,
}

impl DerefStep {
    /// The written symbol for this level (`*` / `&`) plus any per-level
    /// qualifiers, as it reads left-to-right after the base type.
    pub fn render(&self) -> String {
        let mut s = String::from(match self.kind {
            DerefKind::Pointer => "*",
            DerefKind::Reference => "&",
        });
        for a in &self.annotations {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

/// The member-access operator a receiver requires, in the single-level
/// `.`↔`->` regime. Computed purely from a receiver's `deref_stack` (rule
/// #10 — the depth, not an operator-string branch, decides). A DEEP stack
/// (`Box**` = `[Pointer, Pointer]`) wants `(*pp)->`, an expression WRAP not
/// a token swap, so it has no `MemberOp` — `expected_member_op` returns
/// `None` there and consumers leave the access untouched (show-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberOp {
    Dot,
    Arrow,
}

impl MemberOp {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberOp::Dot => ".",
            MemberOp::Arrow => "->",
        }
    }
}

/// The operator a receiver with this deref stack requires, when one
/// single-level `.`↔`->` token swap can express it. `None` for a DEEP
/// stack (≥2 levels: `Box**` needs `(*pp)->`, an expression wrap) — the
/// caller shows members without an auto-fix. The OUTERMOST level (closest
/// to the variable name) is the last element; in the single-level case
/// that is also the only element.
pub fn expected_member_op(stack: &[DerefStep]) -> Option<MemberOp> {
    match stack {
        // value, or a reference (auto-derefs) → `.`
        [] => Some(MemberOp::Dot),
        [one] => Some(match one.kind {
            DerefKind::Pointer => MemberOp::Arrow,
            DerefKind::Reference => MemberOp::Dot,
        }),
        _ => None,
    }
}

/// A member-access whose typed operator disagrees with the operator its
/// receiver's pointer depth requires. `op_span` covers the written
/// operator token; replace it with `expected.as_str()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberOpMismatch {
    pub op_span: Span,
    pub typed: MemberOp,
    pub expected: MemberOp,
}

/// A member-access whose receiver is too deeply indirected for ANY single
/// `.`/`->` token — the fix is an expression WRAP (`(*pp)->m`), not a swap,
/// so there is no auto-fix. Complements `MemberOpMismatch`: the two partition
/// the flagged accesses by whether `expected_member_op` can name one operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberOpPeel {
    /// The written operator token (`.`/`->`) — where the hint is anchored.
    pub op_span: Span,
    /// The receiver rewritten to reach a single-indirection value, e.g.
    /// `(*op_p)`. The caller composes `<wrap>-><member>`.
    pub wrap: String,
    /// Pointer indirection levels on the receiver (≥2).
    pub depth: usize,
}

/// The peeled receiver spelling for a DEEP stack — one whose pointer depth
/// exceeds what a single `.`/`->` can reach (`expected_member_op` is `None`).
/// Returns `(wrap, depth)`: a plain pointer chain of depth N peels to
/// `(` + `*`×(N-1) + name + `)`, accessed with `->`. `None` when a single
/// token already suffices, or the stack isn't a plain pointer chain we can
/// spell (reference-mixed shapes stay silent — rule #10: the stack
/// composition, not a name, decides). The pointer count, not `stack.len()`,
/// drives the star count so an interleaved reference (which auto-derefs)
/// neither adds a `*` nor blocks the hint.
pub fn deref_peel(stack: &[DerefStep], receiver: &str) -> Option<(String, usize)> {
    // A single token already reaches the members — not a peel case.
    if expected_member_op(stack).is_some() {
        return None;
    }
    let pointers = stack
        .iter()
        .filter(|s| s.kind == DerefKind::Pointer)
        .count();
    if pointers < 2 {
        return None;
    }
    let stars = "*".repeat(pointers - 1);
    Some((format!("({stars}{receiver})"), pointers))
}

/// How a value is taken out of its source when it flows to a target — the
/// shape of the assignment. `Whole` is `$x = RHS`; the rest model list /
/// destructuring / element / key binding. An OPEN ontology: Rust makes adding
/// a variant + its lowering cheap, so it grows as the producers need it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Extraction {
    /// The whole source value (`my $x = f()`, `T x = init`).
    Whole,
    /// The Nth element of a list/tuple source (`my ($a, $b) = LIST` → 0, 1).
    Positional(usize),
    /// The list tail from index N onward (a slurpy `@rest`/`%opts`).
    Slurpy(usize),
    /// The value at a literal key of the source (`my %h = (k => v)` keyed).
    KeyOf(String),
    /// A scalar bind that CLEARS to undef — `my $x;` / `local $x;`. The undef
    /// is a REGION assertion (true from the bind until the next rebind: `my $x;
    /// $x->[0]` autovivifies, ending it), so its TYPING composes with the
    /// narrowing tier (region + cutoff), NOT a plain position-blind bag
    /// witness. Producers emit `Rebind` for the cutoff today; this variant +
    /// its `Undef` lowering land when narrowing goes edge-driven.
    Cleared,
    /// A rebind event whose inflowing type we don't (yet) determine —
    /// `foreach my $x (…)` (element), `our $x;` (alias), lvalue-sub. Lowers to
    /// NO type witness; it exists for provenance + the narrowing cutoff (an
    /// edge targeting the subject ends a narrowed region).
    Rebind,
}

/// A VALUE-FLOW EDGE: a value flows from a `source` expression to a `target`
/// binding, taken via `extraction`. THE one concept every assignment/binding
/// shape mints (cpp `@flow`, the Perl builder, the Perl port) and every
/// flow-aware feature reads. It LOWERS to the type-tier witness (`Variable →
/// Edge(Expr)`) so type inference is undisturbed, while keeping the `source`
/// span + `extraction` that the witness discards — the provenance the
/// narrowing / `folded_from` / instance-brand consumers need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowEdge {
    /// Target binding name (sigiled, as the bag keys variables).
    pub target_name: String,
    pub target_scope: ScopeId,
    /// The target's site, for the lowered witness's span (temporal ordering).
    #[serde(with = "PointDef")]
    pub target_at: Point,
    /// The source expression's span — resolved to a type at query time via
    /// `expr_type_at_span`, and the value-provenance anchor.
    pub source: Span,
    pub extraction: Extraction,
    /// A plain assignment to a name the scope ALREADY binds (`$x = …`, never
    /// `my $x = …` / a parameter / a class member). Provenance: the reset a
    /// write performs is its own witness (`WitnessPayload::Reset`, minted
    /// at every write site), not a property of this edge.
    #[serde(default)]
    pub reassigns: bool,
}

impl FlowEdge {
    /// Lower to the type-tier witness — the target's type flows from the
    /// source expression. `Whole` is a direct `Variable → Edge(Expr(source))`;
    /// the projecting extractions (Positional/Slurpy/KeyOf) await their
    /// projected-edge lowering and return `None` for now (no type witness yet,
    /// the FlowEdge itself still records the provenance).
    pub fn lower_to_witness(&self) -> Option<crate::model::witnesses::Witness> {
        use crate::model::witnesses::{
            ProjectionStep, Witness, WitnessAttachment, WitnessPayload, WitnessSource,
        };
        let payload = match &self.extraction {
            // The whole source value flows in.
            Extraction::Whole => WitnessPayload::Edge(WitnessAttachment::Expr(self.source)),
            // The Nth list element — a projection through `element_at(n)` at
            // query time (`None` for a scalar source, which has no element n).
            Extraction::Positional(n) => WitnessPayload::Projected {
                base: WitnessAttachment::Expr(self.source),
                step: ProjectionStep::ArrayIndex(*n as i32),
            },
            // `my (@all) = …` IS the source; a hash is not the list, and a tail
            // from a later position has no projection step yet.
            Extraction::Slurpy(0) if self.target_name.starts_with('@') => {
                WitnessPayload::Edge(WitnessAttachment::Expr(self.source))
            }
            Extraction::Slurpy(_) => return None,
            // The value at a literal key of the source — a keyed
            // destructure (`['k' => $v] = f()`) projecting through the
            // source's keyed shape (`HashWithKeys`) at query time.
            Extraction::KeyOf(k) => WitnessPayload::Projected {
                base: WitnessAttachment::Expr(self.source),
                step: ProjectionStep::HashKey(k.clone()),
            },
            // A bare bind clears to undef — a value the bind uniquely knows
            // (like a literal), so a direct `InferredType`, not an edge.
            Extraction::Cleared => WitnessPayload::InferredType(InferredType::Undef),
            // Rebind-only: recorded in `flow_edges` for the cutoff, no type.
            Extraction::Rebind => return None,
        };
        Some(Witness {
            attachment: WitnessAttachment::Variable {
                name: self.target_name.clone(),
                scope: self.target_scope,
            },
            source: WitnessSource::Builder("flow".into()),
            payload,
            span: Span { start: self.target_at, end: self.target_at },
        })
    }
}

/// The earliest FlowEdge that rebinds `var` within `region` — the
/// LANGUAGE-AGNOSTIC narrowing cutoff. A rebind is a value flowing into the
/// subject (a `@flow` edge), so this one truncation is shared by the Perl
/// narrowing pass (`first_subject_write_via_edges`) and the query-engine
/// (cpp/python) narrowing. `None` ⇒ no rebind in the region, narrowing runs to
/// the region's end. The grammar scan it replaced lived in one language;
/// reading the edge set, every LangPack feeding `@flow` gets the cutoff.
pub fn earliest_rebind_in(flow_edges: &[FlowEdge], var: &str, region: Span) -> Option<Point> {
    let key = |p: &Point| (p.row, p.column);
    let (lo, hi) = (key(&region.start), key(&region.end));
    flow_edges
        .iter()
        .filter(|fe| fe.target_name == var)
        .map(|fe| fe.target_at)
        .filter(|p| lo <= key(p) && key(p) < hi)
        .min_by_key(key)
}

impl Symbol {
    /// Does this callable construct the class that declares it? Every
    /// language mints the flag where it knows — a pack's document names its
    /// own constructor, Perl's builder reads its `new` convention at the
    /// symbol's mint — so this asks the flag and never a name.
    pub fn is_constructor(&self) -> bool {
        self.flags.contains(SymbolFlags::CONSTRUCTOR)
    }

    /// A member RE-EXPORT (`using Base::insert;` in a class body): part of
    /// the class's API surface (outline/completion) but not a definition —
    /// member resolution sees through it to the origin ancestor.
    pub fn is_reexport(&self) -> bool {
        self.flags.contains(SymbolFlags::REEXPORT)
    }

    /// Bare variable/field name without the sigil. Uses the sigil stored
    /// in `detail` so we never re-derive it by text-stripping (which would
    /// mis-handle forms like `$$ref` if the name ever carried that shape).
    /// For non-variable symbols, returns `name` unchanged.
    pub fn bare_name(&self) -> &str {
        match &self.detail {
            SymbolDetail::Variable { sigil, .. } | SymbolDetail::Field { sigil, .. } => {
                let off = sigil.len_utf8();
                self.name.get(off..).unwrap_or(&self.name)
            }
            _ => &self.name,
        }
    }

    /// The displayed TYPE of this symbol: the inferred type's exact class (or
    /// the generic primitive name) plus this symbol's pointer/reference stack
    /// (`Box**`, `const Box*`). THE single "name: type" projection — hover,
    /// member hover, inlay hints, and signature help all render through it, so
    /// the pointer stars can't vanish on some surfaces and not others.
    pub fn display_type(&self, ty: &InferredType) -> String {
        self.display_type_with(ty, &|_| None)
    }

    /// `display_type` through a language's type vocabulary
    /// (`FileAnalysis::display_type_of` supplies the file's).
    pub fn display_type_with(&self, ty: &InferredType, vocab: super::completion::TypeVocab) -> String {
        // A template instance displays its full spelling (`Box<Widget>`)
        // — presentation keeps the args even though dispatch keys the
        // base. Other flavors keep the dispatch-class display.
        let base = match ty.as_parametric() {
            Some(p @ ParametricType::Instance { .. }) => super::completion::format_parametric_with(p, vocab),
            _ => ty
                .class_name()
                .map(String::from)
                .unwrap_or_else(|| super::completion::format_type_with(ty, vocab)),
        };
        let stars: String = self.deref_stack.iter().map(|s| s.render()).collect();
        format!("{}{}", base, stars)
    }

    /// This callable's declared parameter arity, from whichever source carries
    /// it: the pack-minted `arity` field, else the Perl `SymbolDetail::Sub`
    /// param list (`is_slurpy` → variadic, a `default` → optional). One
    /// question, both answers — overload ranking asks the symbol, never the
    /// shape (rule #10). `None` for non-callables.
    pub fn param_arity(&self) -> Option<ParamArity> {
        if let Some(a) = self.arity {
            return Some(a);
        }
        if let SymbolDetail::Sub { params, .. } = &self.detail {
            let total = params.iter().filter(|p| !p.is_slurpy && !p.is_invocant).count();
            let required = params
                .iter()
                .filter(|p| !p.is_slurpy && !p.is_invocant && p.default.is_none())
                .count();
            let variadic = params.iter().any(|p| p.is_slurpy);
            return Some(ParamArity { total, required, variadic });
        }
        None
    }

    /// The return annotation this declaration writes, as the language
    /// spells it. A declaration that writes one is already typed, whatever
    /// the type (`: void` names none), so the hover's inferred-return line
    /// and the missing-return-type lane both ask this.
    pub fn declared_return(&self) -> Option<&str> {
        match &self.detail {
            SymbolDetail::Sub { declared_return, .. } => declared_return.as_deref(),
            _ => None,
        }
    }

    /// True when this symbol is a presentation duplicate that symbol-listing
    /// views should fold away — the getter/primary carries the listing; the
    /// hidden twin exists only so arity-discriminated type inference can
    /// answer both `$o->attr` and `$o->attr($v)`. Every view that enumerates
    /// symbols for humans (outline, workspace-symbol, usage heatmap) asks
    /// this; the verdict is stamped on `presentation` at synthesis.
    pub fn hidden_in_outline(&self) -> bool {
        self.presentation.hide_in_outline
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymKind {
    Variable,
    Sub,
    Method,
    Package,
    Class,
    Module,
    Field,
    /// A named enum value (C/C++ `enum Color { RED }`) — leaks into the
    /// enclosing scope like C allows, but is neither an assignable
    /// `Variable` nor a class `Field`: it's a compile-time constant scoped
    /// to its enum. Distinct kind so hover/outline/completion can label it
    /// `enumerator` instead of collapsing into the variable catch-all.
    Enumerator,
    HashKeyDef,
    /// Named handler registered on a class via string-dispatch (e.g. Mojo
    /// events, Dancer routes, Catalyst actions). Not a Perl method — it
    /// can't be called as `$self->name()`. It's dispatched through named
    /// methods (`->emit`, `->get`, `->forward`) whose first string arg
    /// selects which Handler to run. Multiple Handlers with the same
    /// `(owner, name)` stack, they don't override.
    Handler,
    /// Plugin-controlled scope (mojo app, Minion instance, mojo-events
    /// emitter). Surfaces in the document outline and workspace symbol
    /// search as a navigable entry. Outline entity — not backed by a
    /// Perl symbol, so most queries (gd/gr/rename) don't hit it.
    Namespace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum SymbolDetail {
    Variable {
        sigil: char,
        decl_kind: DeclKind,
    },
    Sub {
        params: Vec<ParamInfo>,
        is_method: bool,
        /// Pre-rendered markdown from POD or comments preceding this sub.
        doc: Option<String>,
        /// The return type is plugin-internal plumbing — use it for
        /// chain resolution but don't render it in completion details,
        /// hover return-type lines, or inlay hints. Lets framework
        /// plugins thread proxy classes (Mojo helper namespaces, DBIC
        /// result wrappers, etc.) without leaking "returns:
        /// _Helper::users::create" at every call site. Plugin-declared,
        /// no core heuristic on the type name.
        #[serde(default)]
        opaque_return: bool,
        /// This Sub symbol is a `use constant` declaration, not an ordinary
        /// sub. Set by `register_constant_symbol`. Consumers (semantic tokens)
        /// ask the symbol whether it's a constant rather than re-deriving from
        /// a name set (rule #10).
        #[serde(default)]
        is_constant: bool,
        /// `my sub helper { … }` — scoped to its enclosing block, not
        /// callable by name from anywhere else. Document symbols show
        /// it (it's real in-file structure); workspace-symbol search
        /// does not (it's not a workspace-addressable entity).
        #[serde(default)]
        lexical: bool,
        /// The return annotation the declaration WRITES, spelled as the
        /// language writes it (php `: string`). A structural fact a type
        /// witness cannot carry — `: void` names no type — so the
        /// signature label and the "this declaration is already typed"
        /// consumers read it instead of re-scanning source or comparing
        /// an attribute string. `None` for a declaration with no
        /// annotation and for a language that writes none (Perl).
        #[serde(default)]
        declared_return: Option<String>,
    },
    Class {
        parent: Option<String>,
        roles: Vec<String>,
        fields: Vec<FieldDetail>,
    },
    Field {
        sigil: char,
        attributes: Vec<String>,
    },
    HashKeyDef {
        owner: HashKeyOwner,
        is_dynamic: bool,
    },
    /// String-dispatched handler detail. `owner` ties the handler to a
    /// class (so two classes can each register a handler named "ready"
    /// without collision). `dispatchers` is the set of method names that
    /// select this handler by string — e.g. `["emit", "subscribe"]` for
    /// Mojo events, `["forward"]` for Catalyst actions. `params` is the
    /// handler's sub signature, consumed by signature help at call
    /// sites and by hover to describe the handler shape.
    ///
    /// The plugin's choice of LSP kind rides `Symbol.presentation` —
    /// handlers share internal machinery (the ref target index, stacking
    /// semantics, cross-file resolution) but aren't all "events": Mojo
    /// events are, but routes are methods, config keys are fields, etc.
    Handler {
        owner: HandlerOwner,
        dispatchers: Vec<String>,
        params: Vec<ParamInfo>,
    },
    /// Package, Module, or other kinds needing no extra data.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclKind {
    My,
    Our,
    State,
    Field,
    Param,
    ForVar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamInfo {
    pub name: String,
    pub default: Option<String>,
    pub is_slurpy: bool,
    /// True when this param is the implicit receiver of the call,
    /// supplied by the caller via `$obj->method(...)` rather than
    /// written in the argument list. Sig help, hover, and outline
    /// drop invocant params so users see what they actually type.
    /// Whoever constructs the ParamInfo is responsible for setting
    /// this — the core never infers it from the name.
    #[serde(default)]
    pub is_invocant: bool,
    /// Where the parameter is BOUND: its signature token, or the `my (…) =
    /// @_` / `my $x = shift` declaration that unpacks it. Minted where the
    /// extractor reads that token (rule #11), so every parameter-anchored
    /// fact lands here and the declaration's own write marker leaves it
    /// standing. `None` for a synthesized parameter (a generated writer's
    /// value, a plugin-declared signature) — nothing in the source binds it.
    #[serde(default, with = "point_opt_serde")]
    pub binding_site: Option<Point>,
    /// The parameter's declared type as written (`int`, `?Foo`, `T&`), minted
    /// from the declaration's type node; `None` for a language whose
    /// parameters carry no type (Perl) or a parameter written without one.
    /// Source text, never a rendering — the signature label shows it verbatim.
    #[serde(default)]
    pub declared_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct FieldDetail {
    pub name: String,
    pub sigil: char,
    pub attributes: Vec<String>,
}

// ---- Ref ----

bitflags::bitflags! {
    /// What the DOCUMENT said at a reference site, as a closed flag set.
    /// A receiver's flavour and a construction are facts the grammar states
    /// outright; the extractor mints them from the capture that said so, and
    /// a consumer asks the reference instead of matching its token text back
    /// against a set of spellings (rule #11).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct RefFlags: u8 {
        /// The receiver is the language's own object token (`$this`, `this`):
        /// its runtime class may be any DESCENDANT of the written one, so a
        /// member absent from that class is not yet a missing member.
        const RECEIVER_THIS = 1 << 0;
        /// The written name is the enclosing class or its parent (`self::`,
        /// `static::`, `parent::`, `new self()`, `: static`): it resolves off
        /// the enclosing scope, never out of a namespace.
        const RELATIVE_SCOPE = 1 << 1;
        /// The site calls the receiver's CONSTRUCTOR — a class that declares
        /// none still has the default one.
        const CONSTRUCTS = 1 << 2;
    }
}

/// The wire form is the bare bit set, like `SymbolFlags`: a flag added at
/// the tail reads old blobs unchanged.
impl Serialize for RefFlags {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.bits().serialize(s)
    }
}

impl<'de> Deserialize<'de> for RefFlags {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        u8::deserialize(d).map(RefFlags::from_bits_retain)
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ref {
    pub kind: RefKind,
    pub span: Span,
    pub scope: ScopeId,
    pub target_name: String,
    pub access: AccessKind,
    /// Where this ref's resolution landed — the ONE home for every
    /// resolution outcome, whatever the kind (`RefKind` stays pure
    /// written shape). `None` means honestly unresolved: no name-only
    /// fallback (that re-introduces the `->new` over-collect).
    /// Populated by the post-passes (`resolve_variable_refs`,
    /// `resolve_hash_key_owners`, the PostFold method stamp,
    /// `build_indices` sym linking, enrichment re-links) via the
    /// `bind_*`/`link_*` mutators; read via the projection accessors
    /// (`resolved_symbol`, `method_target`, `resolved_package`,
    /// `hash_key_owner`, `handler_owner`) so consumers never match the
    /// binding shape against the kind themselves.
    #[serde(default)]
    pub binding: Option<RefBinding>,
    /// Provenance for a call whose name was constant-folded from a variable
    /// (`my $m = 'process'; $self->$m()`): the source string literal's
    /// content span. The call-site token (`$m`) is a non-rewritable variable
    /// read, so rename can't rewrite it — it follows this edge to rewrite the
    /// literal `'process'` instead (rule #9). `None` for non-folded refs.
    #[serde(default)]
    pub folded_from: Option<Span>,
    /// Written argument count at a call site (`FunctionCall` / `MethodCall`):
    /// the args as spelled, receiver excluded. A structural count the pack
    /// mints from the argument list; interpretation ("which overload fits")
    /// lives downstream (`ParamArity::fit`, overload ranking, arity-
    /// discriminated return typing). `None` when unminted (Perl refs, non-call
    /// kinds).
    #[serde(default)]
    pub arg_count: Option<usize>,
    /// The facts the document stated at this site (rule #11): which flavour
    /// of receiver was written, and whether the call constructs.
    #[serde(default)]
    pub flags: RefFlags,
}

pub use crate::model::conventions::{name_match_key, split_qualified};

impl Ref {
    /// The receiver is the language's own object token (`$this`, `this`) —
    /// so its runtime class may be any descendant of the written one.
    pub fn receiver_is_own_object(&self) -> bool {
        self.flags.contains(RefFlags::RECEIVER_THIS)
    }

    /// The written name is the enclosing class or its parent, which resolves
    /// off the enclosing scope rather than out of a namespace.
    pub fn names_relative_scope(&self) -> bool {
        self.flags.contains(RefFlags::RELATIVE_SCOPE)
    }

    /// This site calls the receiver's constructor. A class declaring none
    /// still has the default one, so an unresolved member here is no finding.
    pub fn constructs(&self) -> bool {
        self.flags.contains(RefFlags::CONSTRUCTS)
    }

    /// The receiver view of a member access (`MethodCall` / `FieldAccess`);
    /// `None` for every other kind.
    pub fn member_site(&self) -> Option<MemberSite<'_>> {
        match &self.kind {
            RefKind::MethodCall { invocant, invocant_span, method_name_span, member_op, .. } => {
                Some(MemberSite {
                    invocant,
                    invocant_span: *invocant_span,
                    name_span: *method_name_span,
                    member_op: member_op.as_ref(),
                })
            }
            RefKind::FieldAccess { invocant, invocant_span, member_name_span, member_op } => {
                Some(MemberSite {
                    invocant,
                    invocant_span: *invocant_span,
                    name_span: *member_name_span,
                    member_op: member_op.as_ref(),
                })
            }
            _ => None,
        }
    }

    /// The unqualified callable name for a `FunctionCall` ref. A
    /// fully-qualified call (`Foo::Bar::baz(...)`) keeps the whole path in
    /// `target_name` (the qualified-name hash-key binding logic and rename
    /// rely on it), while symbols are keyed by their bare name (`baz`)
    /// inside their package. Resolution sites that match a call against a
    /// `Sub` symbol pair this bare tail with the ref's `resolved_package`
    /// (= the qualifier) so `Foo::baz()` lands on `sub baz` in package
    /// `Foo`.
    pub fn unqualified_target_name(&self, names: &NameSpellings) -> &str {
        split_qualified(&self.target_name, names).1
    }

    /// The name key this ref is retrievable under in the relational ref
    /// index (`docs/adr/relational-ref-index.md`). One function serves both
    /// sides: rows are keyed by `match_key(ref)`, queries probe
    /// `match_key`-shaped spellings of the target name — so retrieval can
    /// never miss a ref any matcher arm could match (arms compare either the
    /// exact `target_name` or its unqualified tail; equal full names have
    /// equal tails). Sigil variables keep their sigil on the tail because
    /// variable symbols key with it (`$x`, not `x`).
    pub fn match_key(&self, names: &NameSpellings) -> String {
        name_match_key(&self.target_name, names)
    }

    /// For a fully-qualified variable read (`$Foo::Bar::x`, `@Pkg::arr`,
    /// `%Pkg::h`) return `(package, sigil+basename)` — the package the
    /// global lives in, paired with the sigil-bearing bare name that keys
    /// the declaring symbol (`("Foo::Bar", "$x")`). `None` for unqualified
    /// reads (those resolve lexically via the `Symbol` binding). The sigil rides
    /// the basename because variable symbols are keyed with their sigil
    /// (`$x`, `@arr`, `%h`); a leading-`::` `main::` spelling yields an
    /// empty-string package, matching how package-globals in `main` key.
    pub fn qualified_var_target(&self, names: &NameSpellings) -> Option<(&str, String)> {
        let mut chars = self.target_name.chars();
        let sigil = chars.next()?;
        if !names.is_sigil(sigil) {
            return None;
        }
        let (pkg, base) = split_qualified(chars.as_str(), names);
        pkg.map(|p| (p, format!("{sigil}{base}")))
    }
}

/// Projection accessors + binding mutators — the only vocabulary consumers
/// use to read/write a ref's resolution outcome. Each accessor answers one
/// question and returns `None` for kinds that don't carry that flavor, so
/// call sites never match `RefBinding` against `RefKind` themselves.
impl Ref {
    /// The declaring symbol this ref resolved to, whatever flavor carried
    /// it (lexical `Symbol`, a linked `HashKeyDef`, a linked `Handler`).
    /// Method dispatch is NOT projected here — its symbol rides
    /// `method_target()` with the frozen invocant class.
    pub fn resolved_symbol(&self) -> Option<SymbolId> {
        match self.binding.as_ref()? {
            RefBinding::Symbol(sym) => Some(*sym),
            RefBinding::HashKey { sym, .. } | RefBinding::Handler { sym, .. } => *sym,
            RefBinding::Function { .. } | RefBinding::Method(_) | RefBinding::Runtime => None,
        }
    }

    /// The frozen dispatch target of a `MethodCall` ref.
    pub fn method_target(&self) -> Option<&MethodTarget> {
        match self.binding.as_ref()? {
            RefBinding::Method(t) => Some(t),
            _ => None,
        }
    }

    /// The package pin of a `FunctionCall` ref.
    pub fn resolved_package(&self) -> Option<&str> {
        match self.binding.as_ref()? {
            RefBinding::Function { package } => Some(package),
            _ => None,
        }
    }

    /// The resolved owner of a `HashKeyAccess` ref.
    pub fn hash_key_owner(&self) -> Option<&HashKeyOwner> {
        match self.binding.as_ref()? {
            RefBinding::HashKey { owner, .. } => Some(owner),
            _ => None,
        }
    }

    /// The resolved owner of a `DispatchCall` ref.
    pub fn handler_owner(&self) -> Option<&HandlerOwner> {
        match self.binding.as_ref()? {
            RefBinding::Handler { owner, .. } => Some(owner),
            _ => None,
        }
    }

    /// A ref that never wins a same-span cursor tie: it rides ANOTHER
    /// ref's token and surfaces through projections (goto-def's union,
    /// the handler side's hierarchy), never as the cursor's identity. The
    /// one case today is a class-named rail's use (`event(new X)`), a
    /// companion of the class token's own ref — the stacked-refs fork in
    /// `docs/open-forks.md` is where a second case would reopen the shape.
    pub fn is_cursor_companion(&self, pack: &PackFacts) -> bool {
        self.handler_owner().is_some_and(|o| o.names_are_classes(pack))
    }

    /// Whether matching this ref against a target consults only the frozen
    /// build-time verdict on the ref itself. `false` means the matcher's
    /// fallback arm re-derives the verdict at query time through this FILE's
    /// witness bag (`method_call_invocant_class` on an unstamped method
    /// call / `deferred_hash_key_owner` on an unowned or variable-owned
    /// key), so a bag-stripped `refs_present` view could silently drop the
    /// site — the caller upgrades that file to `whole_present` instead.
    /// Over-approximates on purpose: a `false` that would never have
    /// matched costs one whole decode, never a wrong answer.
    pub fn match_verdict_baked(&self) -> bool {
        match &self.kind {
            RefKind::MethodCall { .. } | RefKind::FieldAccess { .. } => self.method_target().is_some(),
            RefKind::HashKeyAccess { .. } => matches!(
                self.hash_key_owner(),
                Some(o) if !matches!(o, HashKeyOwner::Variable { .. })
            ),
            _ => true,
        }
    }

    pub fn bind_symbol(&mut self, sym: SymbolId) {
        self.binding = Some(RefBinding::Symbol(sym));
    }

    pub fn bind_method(&mut self, target: MethodTarget) {
        self.binding = Some(RefBinding::Method(target));
    }

    pub fn bind_function_package(&mut self, package: String) {
        self.binding = Some(RefBinding::Function { package });
    }

    /// Stamp (or re-stamp) a `HashKeyAccess` owner. Drops any previously
    /// linked `HashKeyDef` — a new owner invalidates the old link; the
    /// linker (`build_indices` / enrichment) re-fills `sym` against it.
    pub fn bind_hash_key_owner(&mut self, owner: HashKeyOwner) {
        self.binding = Some(RefBinding::HashKey { owner, sym: None });
    }

    /// Fill the linked symbol on an owner-resolved `HashKey`/`Handler`
    /// binding. A no-op without a resolved owner — the linkers only find
    /// defs through one, so there is nothing to attach it to.
    pub fn link_owned_symbol(&mut self, sym_id: SymbolId) {
        if let Some(RefBinding::HashKey { sym, .. } | RefBinding::Handler { sym, .. }) =
            self.binding.as_mut()
        {
            *sym = Some(sym_id);
        }
    }
}

/// One ref's projection into the relational ref index
/// (`docs/adr/relational-ref-index.md`) — pure data, no storage types.
/// Minted post-fold (the qual columns carry the baked verdicts), consumed
/// by `module_cache::shred_derived_rows`. Kind/qual discriminants are the row
/// format's contract: changing them means bumping `REF_ROWS_VERSION`.
#[derive(Debug, Clone)]
pub struct RefRowSeed {
    /// Retrieval key — `Ref::match_key()`.
    pub key: String,
    pub kind: u8,
    pub span: Span,
    pub access: u8,
    pub flags: u8,
    pub qual_kind: u8,
    pub qual: Option<String>,
    pub arg_count: Option<i64>,
}

impl Ref {
    pub fn row_seed(&self, names: &NameSpellings) -> RefRowSeed {
        let kind = match &self.kind {
            RefKind::Variable => 0,
            RefKind::FunctionCall => 1,
            RefKind::MethodCall { .. } => 2,
            RefKind::PackageRef => 3,
            RefKind::HashKeyAccess { .. } => 4,
            RefKind::ContainerAccess => 5,
            RefKind::DispatchCall { .. } => 6,
            RefKind::FieldAccess { .. } => 7,
        };
        let (qual_kind, qual): (u8, Option<String>) = match &self.kind {
            RefKind::FunctionCall => {
                (1, self.resolved_package().map(str::to_string))
            }
            RefKind::MethodCall { .. } | RefKind::FieldAccess { .. } => (
                2,
                self.method_target().map(|t| t.invocant_class().to_string()),
            ),
            RefKind::DispatchCall { dispatcher } => (3, Some(dispatcher.clone())),
            RefKind::HashKeyAccess { .. } => match self.hash_key_owner() {
                Some(HashKeyOwner::Class(c)) => (4, Some(c.clone())),
                Some(HashKeyOwner::Sub { name, .. }) => (5, Some(name.clone())),
                _ => (0, None),
            },
            _ => (0, None),
        };
        let flags = u8::from(self.folded_from.is_some())
            | (u8::from(self.resolved_symbol().is_some()) << 1);
        RefRowSeed {
            key: self.match_key(names),
            kind,
            span: self.span,
            access: match self.access {
                AccessKind::Read => 0,
                AccessKind::Write => 1,
                AccessKind::Declaration => 2,
            },
            flags,
            qual_kind,
            qual,
            arg_count: self.arg_count.map(|c| c as i64),
        }
    }
}

/// One symbol's projection into the relational store — the enumeration
/// surface: workspace/symbol answers from these rows, declaration-only
/// files become backward-walk candidates through them, and a future
/// register-from-store warm start reads name + kind + the linkage flag.
/// Full `Symbol` detail (SymbolDetail, deref stacks, attributes, full
/// span) stays blob-only and rehydrates. Kind/flag discriminants are
/// row-format contract: changing them bumps `REF_ROWS_VERSION`.
#[derive(Debug, Clone)]
pub struct SymRowSeed {
    pub name: String,
    /// What a REFERENCE to the symbol is keyed by (`name_match_key` under
    /// the analysis's own spellings) — computed where the spellings are in
    /// hand, so the store never re-derives it.
    pub key: String,
    pub kind: u8,
    /// `selection_span` — the landing site workspace/symbol reports.
    pub span: Span,
    pub container: Option<String>,
    /// bit 0: linkage-visible (the `is_linkage_visible` scope-kind gate,
    /// baked at shred time so the registration feed never needs scopes).
    /// bit 1: hidden-in-outline; bit 2: lexical sub — the two
    /// `symbol_to_workspace_info` suppressions, baked so the rows-backed
    /// workspace/symbol filters identically to the resident sweep.
    /// bit 3: exported — the name is in this file's `@EXPORT` / `@EXPORT_OK`
    /// surface (`exports_name`), baked so the unused-exports query selects the
    /// export set straight from the rows, no blob rehydrate.
    pub flags: u8,
}

impl SymRowSeed {
    pub const FLAG_LINKAGE_VISIBLE: u8 = 1;
    pub const FLAG_HIDDEN_IN_OUTLINE: u8 = 1 << 1;
    pub const FLAG_LEXICAL_SUB: u8 = 1 << 2;
    pub const FLAG_EXPORTED: u8 = 1 << 3;
}

/// Inverse of `sym_kind_code` — rehydrating a row's kind for consumers
/// (workspace/symbol) that project it back into LSP kinds.
pub fn sym_kind_from_code(code: u8) -> Option<SymKind> {
    Some(match code {
        0 => SymKind::Variable,
        1 => SymKind::Sub,
        2 => SymKind::Method,
        3 => SymKind::Package,
        4 => SymKind::Class,
        5 => SymKind::Module,
        6 => SymKind::Field,
        7 => SymKind::Enumerator,
        8 => SymKind::HashKeyDef,
        9 => SymKind::Handler,
        10 => SymKind::Namespace,
        _ => return None,
    })
}

/// Stable row-format discriminant for `SymKind` (explicit, not `as u8` —
/// enum reordering must not silently re-key persisted rows).
pub fn sym_kind_code(k: &SymKind) -> u8 {
    match k {
        SymKind::Variable => 0,
        SymKind::Sub => 1,
        SymKind::Method => 2,
        SymKind::Package => 3,
        SymKind::Class => 4,
        SymKind::Module => 5,
        SymKind::Field => 6,
        SymKind::Enumerator => 7,
        SymKind::HashKeyDef => 8,
        SymKind::Handler => 9,
        SymKind::Namespace => 10,
    }
}

/// Build-time-resolved dispatch target for a `MethodCall` ref.
/// `invocant_class` is the class the invocant resolved to at build time
/// (frozen); it drives the inheritance rename-chain match in `refs_to`,
/// replacing the query-time `method_call_invocant_class` re-derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MethodTarget {
    /// Method found on a local symbol in this file (via the ancestor walk).
    Local { sym_id: SymbolId, invocant_class: String },
    /// Method found cross-file (real method in the invocant's module, an
    /// inherited ancestor, or a plugin bridge). The defining symbol lives
    /// in another file; the LSP adapter resolves location via ModuleIndex.
    CrossFile { invocant_class: String },
}

impl MethodTarget {
    /// The invocant class this target resolved against (drives the
    /// rename-chain match in `refs_to`).
    pub fn invocant_class(&self) -> &str {
        match self {
            MethodTarget::Local { invocant_class, .. }
            | MethodTarget::CrossFile { invocant_class } => invocant_class,
        }
    }
}

/// A ref's resolution outcome — one variant per resolution flavor, fusing
/// the components that resolve together (owner + linked symbol) so they can
/// never drift apart. Which flavor a ref carries follows from its `RefKind`
/// (a `MethodCall` binds `Method`, a `HashKeyAccess` binds `HashKey`, …);
/// consumers read through the `Ref` projection accessors rather than
/// matching this enum against the kind themselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RefBinding {
    /// Lexically/package-resolved declaring symbol (variable and label
    /// reads, `our`-globals, pack local refs).
    Symbol(SymbolId),
    /// `FunctionCall` package pin: the package whose `sub` this call
    /// targets — computed at build time by walking the
    /// enclosing-package-then-imports graph (`Builder::resolve_call_package`;
    /// packs pin qualified calls and implicit-`this` sibling calls the same
    /// way). Unpinned calls carry no binding: class/package-scoped queries
    /// treat them as no-match rather than cross-linking same-named subs.
    Function { package: String },
    /// `MethodCall` dispatch target, stamped by the build pipeline's
    /// PostFold invocant fill: the fill already has the invocant class in
    /// hand, so it resolves the method on that class once and freezes the
    /// edge here. `refs_to` / `find_definition` / hover all read this
    /// stored edge instead of re-deriving the invocant class at query
    /// time, so they can never disagree (the NAV unification — a call
    /// that resolved at build time stays matched regardless of query-time
    /// inference flakiness).
    Method(MethodTarget),
    /// `HashKeyAccess` resolution: which hash this key belongs to, plus
    /// the linked `HashKeyDef` symbol once `build_indices` / enrichment
    /// finds one for `(target_name, owner)`.
    HashKey { owner: HashKeyOwner, sym: Option<SymbolId> },
    /// `DispatchCall` resolution: the receiver the string-dispatch resolved
    /// against, plus the linked `Handler` symbol (first stacked def —
    /// `refs_to_symbol` walks all stacked defs separately).
    Handler { owner: HandlerOwner, sym: Option<SymbolId> },
    /// Bound by the RUNTIME, with no declaration to point at (php's
    /// `$this` and its superglobals). A read of one is resolved — nothing
    /// to navigate to, and nothing undefined about it.
    Runtime,
}

/// What kind of entity is being renamed — determines single-file vs cross-file scope.
#[derive(Debug)]
pub enum RenameKind {
    Variable,
    /// A sub defined in (or imported from) a specific package.
    /// `package == None` means a top-level/script sub with no
    /// package context; otherwise package-scoped so cross-file
    /// walks don't rename same-named subs in unrelated packages.
    Function { name: String, package: Option<String> },
    Package(String),
    /// A method with its owning class. Cross-file walks use `class`
    /// to avoid unioning unrelated classes that share a method name
    /// (e.g. `Foo::run` vs `Bar::run`, mojo-helper leaves vs route
    /// targets).
    Method { name: String, class: String },
    HashKey(String),
    /// Rename a `Handler` by (owner, name) — touches the handler symbol's
    /// name + every `DispatchCall` ref targeting it.
    Handler { owner: HandlerOwner, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum RefKind {
    Variable,
    /// Bare `foo()` or `Pkg::foo()` call. The build-time package pin
    /// lives in `Ref::binding` (`RefBinding::Function`).
    FunctionCall,
    /// Method call site `$obj->m(...)` / `Class->m(...)` /
    /// `chain()->m(...)`. **Invocant class is NOT cached on the
    /// variant** — it's resolved on demand via
    /// `FileAnalysis::method_call_invocant_class(ref, module_index)`,
    /// which dispatches by invocant shape (variable / chain receiver /
    /// function-call receiver / bareword / `__PACKAGE__` / `shift` /
    /// `$_[0]`) and queries the witness bag. This is intentional:
    /// a cached field would silently miss invocants that get typed
    /// only by post-build cross-file enrichment, and chain hops
    /// can't be cached without invalidation. The bag-routed helper
    /// composes through cross-file enrichment automatically.
    ///
    /// Build-time chain typing still runs in the builder — it
    /// publishes Variable witnesses + chain-receiver `Expression`
    /// edge witnesses; the helper reads those at query time.
    MethodCall {
        invocant: crate::model::conventions::Invocant,
        /// Span of the invocant node. Used by
        /// `method_call_invocant_class` to find an inner-receiver
        /// ref via `RefTable::call_at_start` (chain dispatch).
        invocant_span: Option<Span>,
        /// Span of just the method name (for rename — r.span covers the whole expression).
        method_name_span: Span,
        /// The member operator a pack wrote (`.`/`->`) + its token span, `Some`
        /// ONLY when the IMMEDIATE receiver is a simple variable (op-DX applies
        /// — its `deref_stack` decides the expected operator). `None` for Perl
        /// (one operator) and wrapper/chain receivers.
        member_op: Option<(MemberOp, Span)>,
        /// The member was NAMED BY A STRING (`[$obj, 'method']`, a class-array
        /// callable): a rename/reference target when it resolves, never an
        /// unresolved-member finding when it does not — a two-element array
        /// holding an object and a string is data until dispatch proves it a
        /// callable (PHPUnit providers, key/value pairs).
        #[serde(default)]
        named_by_string: bool,
    },
    PackageRef,
    /// Key access `$h{k}` / `$obj->{k}`. Which hash owns the key (and the
    /// linked `HashKeyDef`) lives in `Ref::binding` (`RefBinding::HashKey`).
    HashKeyAccess {
        var_text: String,
    },
    /// The container variable in `$hash{key}`, `@arr[0]`, etc.
    ContainerAccess,
    /// Call site that dispatches to a `Handler` symbol by string name,
    /// e.g. `$emitter->emit('ready', ...)`. `dispatcher` is the method
    /// name chosen on the receiver (`"emit"`, `"subscribe"`, etc.);
    /// `target_name` on the enclosing `Ref` is the handler name (the
    /// string literal first-arg). The resolved receiver lives in
    /// `Ref::binding` (`RefBinding::Handler`) — stamped at build time
    /// when the receiver type is known, otherwise re-linked by
    /// enrichment later.
    DispatchCall {
        dispatcher: String,
    },
    /// A member VALUE on a receiver — `$this->prop`, `obj->field`, a
    /// `$this->prop = …` write. The syntax told the extractor the token
    /// names a stored value, not a callable, so the ref says so instead
    /// of carrying a call ref plus a shape tag (rule #11). Resolves over
    /// the class's value members only (`Field` / class-content
    /// `Variable`), never a method: a value read of a name only a method
    /// carries is an undeclared property. A language whose member read IS
    /// a call (Perl's `$o->m`) never mints one. Receiver fields mirror
    /// `MethodCall`'s so the invocant ladder serves both through
    /// `Ref::member_site`. Kept at the END for bincode variant-index
    /// stability (bump `EXTRACT_VERSION`).
    FieldAccess {
        invocant: crate::model::conventions::Invocant,
        invocant_span: Option<Span>,
        /// The member token alone (`r.span` may cover the whole access).
        member_name_span: Span,
        member_op: Option<(MemberOp, Span)>,
    },
}

/// The receiver half of a member access, whatever the member's kind — the
/// one view the invocant ladder, op-DX and the rename token gate read, so
/// a `FieldAccess` and a `MethodCall` on the same receiver resolve it the
/// same way.
#[derive(Debug, Clone, Copy)]
pub struct MemberSite<'a> {
    pub invocant: &'a crate::model::conventions::Invocant,
    pub invocant_span: Option<Span>,
    /// The member token: the method name or the field name.
    pub name_span: Span,
    pub member_op: Option<&'a (MemberOp, Span)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessKind {
    Read,
    Write,
    Declaration,
}


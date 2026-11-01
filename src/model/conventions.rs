//! Perl-convention name predicates.
//!
//! Each convention the analyzer leans on is asked through ONE predicate here
//! instead of being re-spelled as a string match at every consumer (rule #10:
//! the value answers the question). When a convention grows — a plugin
//! declaring extra invocant names, configurable constructor verbs — the
//! change lands here once and every consumer inherits it.
//!
//! Pure `&str` predicates only: no tree-sitter, so `file_analysis.rs` (which
//! must stay tree-free) can use them. Node-level semantics live in `cst.rs`.
//!
//! Perl's name spellings — the `::` separator and the `$`/`@`/`%` sigils —
//! are declared here once (`PERL_SPELLINGS`) exactly as a pack declares
//! its own, and the key functions below take the spellings of the language
//! whose name they handle: nothing in the model assumes a separator or a
//! sigil (CLAUDE.md rule #12).

use crate::model::file_analysis::{ClassSpelling, LatticeRules, MemberNamespace, NameSpellings, Subsumption};

/// Perl's name spellings: `::` qualifies, `$` / `@` / `%` lead variables.
pub const PERL_SPELLINGS: NameSpellings = NameSpellings {
    namespace_sep: Some(std::borrow::Cow::Borrowed("::")),
    sigils: std::borrow::Cow::Borrowed(&['$', '@', '%']),
    class_spelling: ClassSpelling::Identity,
    root_package: Some(std::borrow::Cow::Borrowed("main")),
    // A call reaches a sub, never a `field` of the same name.
    member_namespace: MemberNamespace::PerFamily,
    imports: NameSpellings::NONE.imports,
    // `Foo()` calls a sub; construction is `Foo->new`.
    class_call: crate::model::file_analysis::ClassCall::FunctionCall,
    // A package is both the namespace and the class its code belongs to.
    enclosing_class: crate::model::file_analysis::EnclosingClass::NamespaceInForce,
    // `$obj->{key}` reaches a slot; `$obj->key` never does.
    instance_slots: crate::model::file_analysis::InstanceSlots::Apart,
};

/// Perl's language id: the native builder's, and `FileAnalysis::language`'s
/// default for blobs that predate the field.
pub const PERL_LANGUAGE: &str = "perl";

/// Perl's subsumption rules: an object is a blessed hash, and truth is a
/// number (`1` / `''`).
pub static PERL_LATTICE: LatticeRules =
    LatticeRules::new(&[Subsumption::ObjectAbsorbsHash, Subsumption::BoolIntoNumeric]);

/// Each pack language's subsumption rules, by `FileAnalysis::language`. The
/// model cannot see the packs that declare them, so the build layer registers
/// each served language's set (`docs/adr/union-types.md`); Perl's are
/// declared here, beside its other conventions, and never registered. Append-only slots, not a
/// locked map: every bag query reads this, and a shared lock's reader count is
/// one contended cache line across a sweep's workers.
static LANGUAGE_LATTICES: [std::sync::OnceLock<(&'static str, LatticeRules)>; 16] =
    [const { std::sync::OnceLock::new() }; 16];

/// Register `language`'s rules once; later calls are no-ops, so `rules` is
/// built only on the first.
pub fn register_lattice(language: &'static str, rules: impl FnOnce() -> LatticeRules) {
    debug_assert_ne!(language, PERL_LANGUAGE, "Perl's rules are declared, not registered");
    let mut rules = Some(rules);
    for slot in &LANGUAGE_LATTICES {
        // A racing registration of another language can take this slot
        // between the check and the init; the loop then moves on.
        let (id, _) = slot.get_or_init(|| (language, (rules.take().expect("init runs once"))()));
        if *id == language {
            return;
        }
    }
    panic!("more than {} languages registered", LANGUAGE_LATTICES.len());
}

/// The subsumption rules of the language an analysis was built in. A
/// language nobody registered holds no rules: it never borrows another's.
pub fn lattice_for(language: &str) -> &'static LatticeRules {
    static NO_RULES: LatticeRules = LatticeRules::new(&[]);
    if language == PERL_LANGUAGE {
        return &PERL_LATTICE;
    }
    let registered = LANGUAGE_LATTICES
        .iter()
        .map_while(|slot| slot.get())
        .find(|(id, _)| *id == language)
        .map(|(_, rules)| rules);
    debug_assert!(registered.is_some(), "no lattice registered for language {language:?}");
    registered.unwrap_or(&NO_RULES)
}

/// Split a possibly-qualified name into `(Option<package>, basename)`.
///
/// A name token may carry a `Pkg::` qualifier (`Foo::Bar::baz`, `@Pkg::EXPORT`,
/// `$Foo::Bar::x`). Resolution is always `(qualifier ?? current_package,
/// basename)`. This is the ONE place that decides "is this name qualified" —
/// every per-construct stripper (`Ref::unqualified_target_name`,
/// `Builder::export_var_basename`, FQ-variable ref emission) routes through it
/// (rule #10: encode the "is qualified" property once).
///
/// Input must be sigil-free (callers strip the sigils first). The text
/// after the last separator is the basename; everything before it is the
/// package. An unqualified name yields `(None, name)`. A leading separator
/// (`::foo`, Perl's `main::` shorthand) yields an empty-string package,
/// preserved verbatim.
///
/// The separator is `names`' — the language of the name — and nothing
/// else: a name is split on exactly the separator its language declares,
/// so a name that joins a class and a member (`App\Foo::bar`) must never
/// reach here; the extractor mints those as two fields.
pub fn split_qualified<'a>(name: &'a str, names: &NameSpellings) -> (Option<&'a str>, &'a str) {
    match names.sep().and_then(|sep| name.rsplit_once(sep)) {
        Some((pkg, base)) => (Some(pkg), base),
        None => (None, name),
    }
}

/// `split_qualified`'s inverse: a namespace and a leaf as one qualified
/// name. The global namespace (an empty `namespace`) yields the bare leaf,
/// never a dangling separator, so a caller need not ask first whether the
/// name has a namespace.
pub fn join_qualified(namespace: &str, leaf: &str, sep: &str) -> String {
    if namespace.is_empty() {
        leaf.to_string()
    } else {
        format!("{namespace}{sep}{leaf}")
    }
}

/// The relational ref index's shared key function: rows are keyed by
/// `name_match_key(ref.target_name)`, retrieval probes
/// `name_match_key(target.name)` — one function on both sides, so a row can
/// never be missed by a spelling the matcher would accept (arms compare
/// exact names or their unqualified tails; equal names have equal tails).
/// Sigil variables keep the sigil on the tail (`$Foo::x` → `$x`) because
/// variable identities carry it — under the language's OWN sigils, so a
/// `$` in a language that declares none is identifier text.
pub fn name_match_key(name: &str, names: &NameSpellings) -> String {
    let mut chars = name.chars();
    if let Some(sigil) = chars.next() {
        if names.is_sigil(sigil) {
            let (_, base) = split_qualified(chars.as_str(), names);
            return format!("{sigil}{base}");
        }
    }
    split_qualified(name, names).1.to_string()
}

/// Split a sigil-led qualified variable into the package it names and the
/// sigil-bearing basename that keys its declaring symbol (`$Foo::Bar::x` →
/// `("Foo::Bar", "$x")`). An empty qualifier names the language's root
/// package (`$::x` → `("main", "$x")`). `None` for an unqualified or
/// sigil-less name.
pub fn split_qualified_var<'a>(name: &'a str, names: &'a NameSpellings) -> Option<(&'a str, String)> {
    let mut chars = name.chars();
    let sigil = chars.next()?;
    if !names.is_sigil(sigil) {
        return None;
    }
    let (pkg, base) = split_qualified(chars.as_str(), names);
    pkg.map(|p| (names.qualifier_package(p), format!("{sigil}{base}")))
}

/// Whether a sigil-led basename (`split_qualified_var`'s, or a bare
/// spelling) is `@ISA`, the array that holds a package's parents.
pub fn is_isa_var(base: &str) -> bool {
    base == "@ISA"
}

/// The qualified spelling of a variable: `('$', "Foo", "x")` → `$Foo::x`.
/// The inverse of `split_qualified_var`.
pub fn qualify_var(sigil: char, package: &str, base: &str, names: &NameSpellings) -> String {
    format!("{sigil}{package}{}{base}", names.sep().unwrap_or_default())
}

/// Conventional invocant variable names — `sub f { my ($self) = @_ }` and
/// friends. Accepts the bare identifier or the `$`-sigiled spelling so both
/// param names (`"$self"`) and canonical varnames (`"self"`) route here.
///
/// "Conventional" means: the *name alone* signals receiver-ness. A variable
/// not on this list can still be the invocant (`my ($c) = @_;`) — callers
/// that know the position (first param of a method) must not gate on this.
pub fn is_conventional_invocant_name(name: &str) -> bool {
    matches!(
        name.strip_prefix('$').unwrap_or(name),
        "self" | "class" | "this" | "proto"
    )
}

/// A variable Perl itself defines (perlvar): punctuation (`$_`, `$@`, `@_`),
/// digit (`$0`, `$1`), caret (`${^W}`) and the identifiers Perl forces into
/// `main` (`%ENV`, `@ARGV`, `%SIG`, ...). Such a name never needs a
/// declaration and never names user state, so an undeclared use of one is
/// not an implicit global. `name` is sigil-led, as a variable ref spells it.
pub fn is_special_variable(name: &str) -> bool {
    let base = name.get(1..).unwrap_or("");
    let base = base.strip_prefix('{').and_then(|b| b.strip_suffix('}')).unwrap_or(base);
    match base.chars().next() {
        None => true,
        Some(c) if !(c.is_alphabetic() || c == '_') => true,
        _ => matches!(
            base,
            "_" | "ENV" | "INC" | "ARGV" | "ARGVOUT" | "SIG" | "STDIN" | "STDOUT" | "STDERR"
        ),
    }
}

/// Whether `use VERSION` turns `strict` on: every version from 5.11 does.
/// Reads both spellings, the v-string (`v5.36`, `5.36.0`) and the decimal
/// (`5.012`, `5.010_001`, whose minor is the first three fraction digits).
pub fn version_enables_strict(version: &str) -> bool {
    let text = version.replace('_', "");
    let (vstring, text) = match text.strip_prefix('v') {
        Some(rest) => (true, rest.to_string()),
        None => (text.matches('.').count() > 1, text.clone()),
    };
    let mut parts = text.split('.');
    let Some(Ok(major)) = parts.next().map(str::parse::<u32>) else { return false };
    let minor = match parts.next() {
        None => 0,
        Some(m) if vstring => m.parse::<u32>().unwrap_or(0),
        Some(m) => format!("{m:0<3}").get(..3).and_then(|d| d.parse::<u32>().ok()).unwrap_or(0),
    };
    (major, minor) >= (5, 11)
}

/// `use strict 'vars'` exempts these from declaration on top of the special
/// variables: `$a`/`$b` are `sort`'s comparator globals.
pub fn is_strict_vars_exempt(name: &str) -> bool {
    is_special_variable(name) || matches!(name, "$a" | "$b")
}

/// A name spelled as a variable or a glob (`$x`, `@x`, `*x`) rather than a
/// sub — an import list carries both.
pub fn is_variable_or_glob_spelling(name: &str) -> bool {
    name.starts_with('*') || name.chars().next().is_some_and(|c| PERL_SPELLINGS.is_sigil(c))
}

/// The typeglob holding a variable's slot (`$x` → `*x`). Exporting the
/// glob exports every sigil's variable of that name.
pub fn glob_of_variable(name: &str) -> String {
    format!("*{}", name.get(1..).unwrap_or(""))
}

/// Conventional constructor method name. Perl has no `new` keyword — this is
/// pure convention, but it's the convention every framework and the inference
/// rules ("`Class->new` returns `Class`") build on.
pub fn is_constructor_name(name: &str) -> bool {
    name == "new"
}

/// A syntactically valid bareword package/class name: `Foo`, `Foo::Bar`,
/// `_Private`. The tolerant grammar hands `->new`'s invocant back as raw
/// text, and a computed receiver — `(ref $self)->new`, the DBIC
/// receiver-polymorphic constructor idiom — parses to a leading `(`, which
/// must NEVER be frozen as a class name (that produced `return_type: "("`).
/// A class here is `InferredType::ClassName(text)`; only accept text a
/// package could actually be spelled as.
pub fn is_bareword_class_name(text: &str, names: &NameSpellings) -> bool {
    // Every segment between the language's separator must be a plain
    // identifier, so a receiver EXPRESSION (`(new Coll([1]))->wrapUp([2])`)
    // never passes as a class token. A leading separator is the language's
    // ABSOLUTE spelling (`\A\F`, `::main`): still a class token, resolved
    // as written. The identifier class is ASCII today — known debt
    // (docs/PARKED.md, identifier classification per language).
    let is_ident = |seg: &str| {
        let mut chars = seg.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let Some(sep) = names.sep() else { return is_ident(text) };
    let text = text.strip_prefix(sep).unwrap_or(text);
    if text.is_empty() {
        return false;
    }
    text.split(sep).all(is_ident)
}

/// Perl's attribute vocabulary, spelling → flag — the one table for a
/// Corinna `field` attribute (`:param`, `:reader`, the writer family
/// `:writer` / `:mutator` / `:accessor`), the way a pack declares its own.
/// A spelling with no declaration fact (`:ro` style hints) is display-only.
pub fn field_attribute_flag(attr: &str) -> Option<crate::model::file_analysis::SymbolFlags> {
    use crate::model::file_analysis::SymbolFlags;
    Some(match attr {
        "param" => SymbolFlags::PARAM,
        "reader" => SymbolFlags::READER,
        "writer" | "mutator" | "accessor" => SymbolFlags::WRITER,
        _ => return None,
    })
}

/// A Perl sigil: what kind of thing a name, or a deref, denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sigil {
    Scalar,
    Array,
    Hash,
    Code,
    Glob,
}

impl Sigil {
    pub fn char(self) -> char {
        match self {
            Sigil::Scalar => '$',
            Sigil::Array => '@',
            Sigil::Hash => '%',
            Sigil::Code => '&',
            Sigil::Glob => '*',
        }
    }
}

/// `__PACKAGE__` — the compile-time token for the enclosing package. Perl's
/// own word: a pack mints the class its receiver names, never this token
/// (`layering_tests::packs_do_not_borrow_perls_current_package_token`).
pub const CURRENT_PACKAGE_TOKEN: &str = "__PACKAGE__";

/// Whether `text` is `CURRENT_PACKAGE_TOKEN`.
pub fn is_current_package_token(text: &str) -> bool {
    text == CURRENT_PACKAGE_TOKEN
}

/// Whether `name` is the `ref` builtin, which reports what a reference is.
/// The library functions that do the same are plugin-declared
/// (`ref_tag_functions()`).
pub fn is_ref_builtin(name: &str) -> bool {
    name == "ref"
}

/// A name that can be written as a method / sub call — a syntactically valid
/// Perl identifier (or `::`-qualified chain of them). Synthetic symbols the
/// analyzer mints for value-carrying constructs — an anonymous `sub { ... }`
/// gets the placeholder name `(anon)` — are NOT callable by name, so they must
/// never surface as method-completion candidates (`$obj->(anon)` is not a
/// thing). Gate completion sources on this property, not on the literal
/// `(anon)` spelling (rule #10).
pub fn is_callable_sub_name(name: &str, names: &NameSpellings) -> bool {
    is_bareword_class_name(name, names)
}

/// A method-call invocant in canonical spelling: variable invocants are
/// sigil + bare varname (`${ sner }` stores as `$sner`, via the grammar's
/// `varname` child), `__PACKAGE__` resolved to the enclosing package,
/// anything else raw expression text. The newtype exists so a raw
/// `node.utf8_text()` can't be slotted into an invocant field by
/// accident — every producer either goes through the builder's
/// canonicalizing path or owns the claim with [`assume_canonical`].
///
/// [`assume_canonical`]: InvocantName::assume_canonical
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct InvocantName(String);

impl InvocantName {
    /// The caller asserts the text is already canonical: plugin manifests
    /// declaring a literal receiver class, synthesized refs spelled
    /// `$self`, tests. Named so the assertion is grep-able — there is
    /// deliberately no blanket `From<String>`.
    pub fn assume_canonical(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn classify(&self) -> InvocantText<'_> {
        InvocantText::parse(&self.0)
    }
}

impl std::ops::Deref for InvocantName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for InvocantName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for InvocantName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::fmt::Display for InvocantName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A method-call invocant, structurally split by *who resolves it*.
///
/// The ordinary [`Name`](Invocant::Name) case is a receiver written in
/// the source — a variable, class, `__PACKAGE__`, or positional `shift` /
/// `$_[0]` — resolved by the model's own inference. The
/// [`Bridged`](Invocant::Bridged) case is a *token* a plugin routed here
/// that is NOT a Perl receiver expression (a Mojo route controller key:
/// `->to('users#list')` carries `Bridged { plugin: "mojo-routes", token:
/// "users" }`). It is deliberately a distinct variant — not a
/// guessed-by-leading-case string — so:
///   * the builder never freezes the raw token as a class
///     (its `Method` binding stays unstamped), and
///   * the model asks the *owning plugin* to resolve it instead of
///     encoding the plugin's algorithm in core (rule #10 + rule #8).
///
/// Mirrors the existing "Bridged" naming (`HashKeyOwner::Bridged`,
/// `TargetKind::HashKeyOfBridged`): a thing whose identity is owned by a
/// plugin, not a Perl class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Invocant {
    Name(InvocantName),
    /// A receiver EXPRESSION (a pack's `recv.m`, `obj->m`): its value is
    /// whatever the bag types at its span, and its text is never a class
    /// spelling, however much it looks like one (`self.x`).
    Expr(InvocantName),
    Bridged { plugin: String, token: String, match_mode: BridgedMatch },
}

/// How a [`Bridged`](Invocant::Bridged) token resolves to a class. Strict
/// (exact name) by default; tail is **opt-in** for framework tokens that
/// drop the namespace — a Mojo controller key camelizes to `Users` and
/// resolves to any indexed `*::Users` that owns the action. The emitting
/// plugin declares the mode; core never guesses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgedMatch {
    /// The token is the exact fully-qualified class name.
    #[default]
    Exact,
    /// The token is a `::`-tail; match any class whose tail equals it.
    Tail,
}

impl Invocant {
    /// Wrap an already-canonical receiver text (the producer owns the
    /// claim — same contract as [`InvocantName::assume_canonical`]).
    pub fn assume_canonical(s: impl Into<String>) -> Self {
        Invocant::Name(InvocantName::assume_canonical(s))
    }

    /// A receiver expression, typed only by the bag at its span.
    pub fn expr(s: impl Into<String>) -> Self {
        Invocant::Expr(InvocantName::assume_canonical(s))
    }

    /// A plugin-routed token: `plugin` names the emitting plugin (provenance),
    /// `token` is the class key it already transformed (e.g. a camelized Mojo
    /// controller name), and `match_mode` says how core matches it to a class.
    pub fn bridged(
        plugin: impl Into<String>,
        token: impl Into<String>,
        match_mode: BridgedMatch,
    ) -> Self {
        Invocant::Bridged { plugin: plugin.into(), token: token.into(), match_mode }
    }

    /// The ordinary receiver, or `None` when this is a plugin-bridged
    /// token. Consumers that only handle real receivers (`$self->`,
    /// `Class->`) bind through this and skip bridged tokens.
    pub fn as_name(&self) -> Option<&InvocantName> {
        match self {
            Invocant::Name(n) | Invocant::Expr(n) => Some(n),
            Invocant::Bridged { .. } => None,
        }
    }

    pub fn is_bridged(&self) -> bool {
        matches!(self, Invocant::Bridged { .. })
    }

    /// The raw text of the invocant — receiver text for `Name`, the token
    /// for `Bridged`. A deliberate projection for peripheral string
    /// consumers (completion display, dedup keys); resolution-bearing
    /// callers match the variant instead.
    pub fn text(&self) -> &str {
        match self {
            Invocant::Name(n) | Invocant::Expr(n) => n,
            Invocant::Bridged { token, .. } => token,
        }
    }
}

impl Default for Invocant {
    fn default() -> Self {
        Invocant::Name(InvocantName::default())
    }
}

/// The text of a method-call invocant, classified once. Consumers match
/// the variant instead of re-deriving the shape with sigil/keyword string
/// checks at each site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocantText<'a> {
    /// `$obj` — a scalar variable, carried WITHOUT its sigil (only a
    /// scalar can dispatch, so the `$` is content-free). Its class comes
    /// from inference, never from the spelling — bag lookups key on the
    /// original sigiled text the caller still holds.
    Scalar(&'a str),
    /// `__PACKAGE__` — the enclosing package.
    CurrentPackage,
    /// `shift` / `$_[0]` / `@_[0]` — the method's own receiver argument
    /// read positionally (`my $self = shift`); resolves to the enclosing
    /// class. Not real variables — the bag has no witness for them.
    PositionalReceiver,
    /// `@list` / `%h` — not a legal invocant (Perl methods dispatch on a
    /// scalar or a class), but tree-sitter-perl's tolerant grammar still
    /// parses `@list->m` as a method call, and mid-edit completion text
    /// can spell anything. Unresolvable by construction; consumers
    /// answer `None`, never a class.
    NonScalar(&'a str),
    /// Anything else — a bareword: a class name, or a class-returning
    /// zero-arg sub (`app->routes`).
    Bareword(&'a str),
}

impl<'a> InvocantText<'a> {
    /// Classify invocant text. Callers with a node in hand canonicalize
    /// FIRST (`cst::canonical_var_name` — the grammar's `varname` child
    /// already strips `${...}` brace spellings); this never re-derives
    /// node structure from text.
    pub fn parse(text: &'a str) -> Self {
        match text {
            t if is_current_package_token(t) => Self::CurrentPackage,
            "shift" | "$_[0]" | "@_[0]" => Self::PositionalReceiver,
            t if t.starts_with('$') => Self::Scalar(&t[1..]),
            t if t.starts_with('@') || t.starts_with('%') => Self::NonScalar(&t[1..]),
            t => Self::Bareword(t),
        }
    }

    /// True when the invocant is a hash/array **element place** — `$h{k}`,
    /// `$a[0]`, `$h{a}{b}`, or the arrow forms `$x->{k}` / `$x->[0]` — i.e.
    /// a stable slot that flow-narrowing may have refined, so the receiver
    /// query consults a place witness for it. A scalar **deref** (`${$ref}`,
    /// `${name}`) is NOT a place: its brace follows the sigil directly,
    /// whereas a place's subscript follows the container name (or an arrow /
    /// a prior subscript). Plain scalars and non-scalars are never places.
    pub fn is_element_place(&self) -> bool {
        let Self::Scalar(inner) = self else { return false };
        // `inner` is the text after the `$` sigil. A deref spelling leads
        // with the brace (`${…}` → inner `"{…}"`); a place has its container
        // name / `->` / prior subscript before the first `{` or `[`.
        let b = inner.as_bytes();
        b.iter().position(|&c| c == b'{' || c == b'[').is_some_and(|i| {
            i > 0 && !matches!(b[i - 1], b'$' | b'@' | b'%')
        })
    }
}

/// A method-call name token (`$obj->Foo::Bar::m`, `$self->SUPER::m`,
/// `->::m`, `->m`), parsed once. Consumers match the variant instead of
/// re-deriving qualifier semantics with string ops — the qualifier's
/// *meaning* (SUPER is not a class; `::` is the `main` shorthand; anything
/// else is the literal dispatch package) lives here and nowhere else.
///
/// Scope: method tokens only. Function/decl names (`Foo::bar()`, glob
/// splices, `our @Pkg::EXPORT`) have no SUPER keyword — they keep
/// `file_analysis::split_qualified`, the raw `(package, basename)` seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodToken<'a> {
    /// `m` — dispatch starts at the invocant's class.
    Bare(&'a str),
    /// `SUPER::m` — the one qualifier that does NOT name a class: dispatch
    /// starts at the parents of the package the call is *written* in
    /// (and there may be several).
    Super(&'a str),
    /// `::m` — `main::` shorthand; the dispatch package is `main`.
    Main(&'a str),
    /// `Foo::Bar::m` — the qualifier is the literal dispatch package.
    Qualified { package: &'a str, name: &'a str },
}

impl<'a> MethodToken<'a> {
    pub fn parse(token: &'a str) -> Self {
        match token.rsplit_once("::") {
            None => Self::Bare(token),
            Some(("SUPER", tail)) => Self::Super(tail),
            Some(("", tail)) => Self::Main(tail),
            Some((pkg, tail)) => Self::Qualified { package: pkg, name: tail },
        }
    }

    /// The bare method name — the tail after any qualifier.
    pub fn name(&self) -> &'a str {
        match self {
            Self::Bare(n) | Self::Super(n) | Self::Main(n) => n,
            Self::Qualified { name, .. } => name,
        }
    }

    /// The literal dispatch package, when the qualifier names one.
    /// `None` for `Bare` (the invocant decides) and `Super` (the writing
    /// package's parent MRO decides — resolving it needs ancestry).
    pub fn literal_package(&self) -> Option<&'a str> {
        match self {
            Self::Qualified { package, .. } => Some(package),
            Self::Main(_) => Some("main"),
            Self::Bare(_) | Self::Super(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_bareword_class_name, InvocantText, MethodToken, PERL_SPELLINGS};
    use crate::model::file_analysis::{ClassSpelling, NameSpellings};

    #[test]
    fn bareword_class_name_rejects_computed_receivers() {
        for ok in ["Foo", "Foo::Bar", "_Private", "DBIx::Class::ResultSet", "a1"] {
            assert!(is_bareword_class_name(ok, &PERL_SPELLINGS), "{ok} should be a class name");
        }
        // The DBIC `(ref $self)->new` idiom parses to these — never a class.
        for bad in ["(ref $self)", "(", "(ref $self", "$self", "1Foo", "Foo::", ""] {
            assert!(!is_bareword_class_name(bad, &PERL_SPELLINGS), "{bad:?} must NOT be a class name");
        }
    }

    /// Each language's names split on ITS separator and keep ITS sigils —
    /// never another's. A php-shaped language declares `\` and `$`; a
    /// JavaScript-shaped one declares nothing, so `$el` is identifier text.
    #[test]
    fn a_name_splits_on_its_own_language_s_spellings_only() {
        let php = NameSpellings {
            namespace_sep: Some(std::borrow::Cow::Borrowed("\\")),
            sigils: std::borrow::Cow::Borrowed(&['$']),
            class_spelling: ClassSpelling::UseMap,
            root_package: None,
            member_namespace: crate::model::file_analysis::MemberNamespace::PerFamily,
            imports: NameSpellings::NONE.imports,
            class_call: NameSpellings::NONE.class_call,
            enclosing_class: NameSpellings::NONE.enclosing_class,
            instance_slots: NameSpellings::NONE.instance_slots,
        };
        assert_eq!(super::split_qualified("App\\Models\\User", &php), (Some("App\\Models"), "User"));
        assert_eq!(super::name_match_key("App\\Models\\User", &php), "User");
        assert_eq!(super::name_match_key("$App\\x", &php), "$x");
        // An empty qualifier stays empty where the language names no root.
        assert_eq!(super::split_qualified_var("$\\x", &php), Some(("", "$x".to_string())));
        assert_eq!(super::split_qualified_var("$::x", &PERL_SPELLINGS), Some(("main", "$x".to_string())));
        assert_eq!(super::split_qualified_var("@Foo::Bar::l", &PERL_SPELLINGS), Some(("Foo::Bar", "@l".to_string())));
        assert_eq!(super::split_qualified_var("$x", &PERL_SPELLINGS), None);
        assert!(is_bareword_class_name("App\\Support\\Str", &php));
        assert!(is_bareword_class_name("\\A\\F", &php), "an absolute spelling is a class token");
        assert!(!is_bareword_class_name("(new Coll([1]))", &php));
        assert!(!is_bareword_class_name("A\\", &php));
        // Perl's `::` is not php's separator and php's `\` is not Perl's.
        assert_eq!(super::split_qualified("Foo::Bar", &php), (None, "Foo::Bar"));
        assert_eq!(super::split_qualified("App\\User", &PERL_SPELLINGS), (None, "App\\User"));
        assert_eq!(super::split_qualified("Foo::Bar::baz", &PERL_SPELLINGS), (Some("Foo::Bar"), "baz"));
        assert_eq!(super::name_match_key("$Foo::x", &PERL_SPELLINGS), "$x");
        let js = NameSpellings::NONE;
        assert_eq!(super::name_match_key("$el", &js), "$el", "no sigils declared: `$` is identifier text");
        assert_eq!(super::split_qualified("a.b", &js), (None, "a.b"));
        // A separator does not make a use-map language: Perl qualifies with
        // `::` and every spelling is its own identity.
        assert_eq!(PERL_SPELLINGS.use_map_sep(), None);
        assert_eq!(php.use_map_sep(), Some("\\"));
    }

    #[test]
    fn invocant_text_variants() {
        // Scalar carries the bare name — the sigil is content-free since
        // only a scalar can dispatch.
        assert_eq!(InvocantText::parse("$obj"), InvocantText::Scalar("obj"));
        assert_eq!(InvocantText::parse("@list"), InvocantText::NonScalar("list"));
        assert_eq!(InvocantText::parse("%h"), InvocantText::NonScalar("h"));
        assert_eq!(InvocantText::parse("__PACKAGE__"), InvocantText::CurrentPackage);
        assert_eq!(InvocantText::parse("shift"), InvocantText::PositionalReceiver);
        assert_eq!(InvocantText::parse("$_[0]"), InvocantText::PositionalReceiver);
        assert_eq!(InvocantText::parse("@_[0]"), InvocantText::PositionalReceiver);
        assert_eq!(InvocantText::parse("Foo::Bar"), InvocantText::Bareword("Foo::Bar"));
    }

    #[test]
    fn element_place_vs_deref() {
        // Element places: subscript follows the container name / arrow.
        for place in ["$h{k}", "$a[0]", "$h{a}{b}", "$self->{x}", "$self->[0]"] {
            assert!(
                InvocantText::parse(place).is_element_place(),
                "{place} should be an element place",
            );
        }
        // Scalar derefs and plain vars are NOT places — `${...}`'s brace
        // follows the sigil directly.
        for not_place in ["${name}", "${$ref}", "$obj", "@list", "Foo::Bar", "shift"] {
            assert!(
                !InvocantText::parse(not_place).is_element_place(),
                "{not_place} should NOT be an element place",
            );
        }
    }

    #[test]
    fn method_token_variants() {
        assert_eq!(MethodToken::parse("m"), MethodToken::Bare("m"));
        assert_eq!(MethodToken::parse("SUPER::m"), MethodToken::Super("m"));
        assert_eq!(MethodToken::parse("::m"), MethodToken::Main("m"));
        assert_eq!(
            MethodToken::parse("Foo::Bar::m"),
            MethodToken::Qualified { package: "Foo::Bar", name: "m" }
        );
        // SUPER is only the keyword when it is the WHOLE qualifier.
        assert_eq!(
            MethodToken::parse("Foo::SUPER::m"),
            MethodToken::Qualified { package: "Foo::SUPER", name: "m" }
        );
    }

    #[test]
    fn method_token_projections() {
        assert_eq!(MethodToken::parse("SUPER::m").name(), "m");
        assert_eq!(MethodToken::parse("Foo::Bar::m").literal_package(), Some("Foo::Bar"));
        assert_eq!(MethodToken::parse("::m").literal_package(), Some("main"));
        assert_eq!(MethodToken::parse("SUPER::m").literal_package(), None);
        assert_eq!(MethodToken::parse("m").literal_package(), None);
    }
}

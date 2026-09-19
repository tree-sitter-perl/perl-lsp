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

use crate::model::file_analysis::{ClassSpelling, NameSpellings};

/// Perl's name spellings: `::` qualifies, `$` / `@` / `%` lead variables.
pub const PERL_SPELLINGS: NameSpellings = NameSpellings {
    namespace_sep: Some(std::borrow::Cow::Borrowed("::")),
    sigils: std::borrow::Cow::Borrowed(&['$', '@', '%']),
    class_spelling: ClassSpelling::Identity,
};

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

/// Strip Perl variable sigils from a typed name: the bare identity token
/// a rename writes at every collected span (`$total` → `total`). This is
/// the PERL instance of the per-language name-semantics hook on the
/// resolution CandidateSet's identity keying (`CandidateSet::bare_new_name`)
/// — pack languages canonicalize spellings at extraction instead (the
/// LangPack `shape_name` hook; cpp's `canonical_template_spelling`), so
/// their typed names pass through bare.
pub fn strip_variable_sigils(name: &str) -> &str {
    name.trim_start_matches(['$', '@', '%'])
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

/// Perl's WRITE and DISPLAY spellings — the same declaration every pack
/// language makes, reached the same way (`LanguageRegistry::spellings`,
/// `FileAnalysis::spellings()`). Perl has no pack driver of its own, so
/// without this it would read whatever the neutral default happens to be,
/// and a default nobody chose for Perl is a rule the next pack inherits
/// by forgetting.
///
/// Almost everything is empty because Perl genuinely writes none of it:
/// the engine's type tags ARE its vocabulary, there is no declared type to
/// insert, no return annotation, no class-name literal member, no static
/// sigil. The two that matter are the booleans.
pub const PERL_PACK_SPELLINGS: crate::model::file_analysis::PackSpellings =
    crate::model::file_analysis::PackSpellings {
        type_display: &[],
        native_type_spellings: &[],
        class_literal_member: "",
        import_template: "",
        contract_stub: "",
        return_annotation_template: "",
        static_property_sigil: "",
        // A Perl signature writes neither a variadic marker nor a default
        // separator — `@_` is the whole convention.
        variadic_marker: "",
        default_sep: "",
        // Typeglobs install a sub into another package, so a member
        // declaration does NOT belong to the container that encloses it.
        members_are_package_bound: false,
        // An `AUTOLOAD` answers a role's required method at runtime.
        catch_all_satisfies_contracts: true,
    };

/// A `'static` address for Perl's spellings, so the driver can hand out a
/// reference.
pub static PERL_SPELLINGS_PACK: crate::model::file_analysis::PackSpellings = PERL_PACK_SPELLINGS;

/// `__PACKAGE__` — the compile-time token for the enclosing package.
pub fn is_current_package_token(text: &str) -> bool {
    text == "__PACKAGE__"
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
            Invocant::Name(n) => Some(n),
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
            Invocant::Name(n) => n,
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
        };
        assert_eq!(super::split_qualified("App\\Models\\User", &php), (Some("App\\Models"), "User"));
        assert_eq!(super::name_match_key("App\\Models\\User", &php), "User");
        assert_eq!(super::name_match_key("$App\\x", &php), "$x");
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

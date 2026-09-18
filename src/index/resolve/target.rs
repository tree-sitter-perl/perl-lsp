//! The resolution vocabulary: what a cursor can resolve TO (`TargetRef`,
//! `ResolvedTarget`, `TargetKind`), where a hit lives (`RefLocation`), and
//! the per-feature policy those types carry (rename scope/options, group
//! member rename rules).
use super::*;
use crate::model::file_analysis::RailNames;

/// How a method that participates in an inheritance hierarchy is scoped for
/// references + rename — `initializationOptions.rename.overrideScope`.
///
/// * `Hierarchy` (default) — the standard IDE refactor: the whole override
///   family (base decl + every override + all dispatching call sites), gathered
///   over proven `@ISA`/`use parent`/role edges (never name matches).
/// * `Dispatch` — precise: only the cursor's own definition + the call sites
///   that dispatch to *that* definition (incl. `SUPER::` calls targeting it),
///   leaving sibling overrides untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideScope {
    #[default]
    Hierarchy,
    Dispatch,
}

impl OverrideScope {
    /// Parse the override-scope string for the CLI (`--rename … <scope>`);
    /// anything unrecognized (or absent) is the default `Hierarchy`. The LSP
    /// path deserializes `RenameOptions` straight from JSON instead.
    pub fn from_option(s: &str) -> Self {
        match s {
            "dispatch" => OverrideScope::Dispatch,
            _ => OverrideScope::Hierarchy,
        }
    }
}

/// `initializationOptions.rename` — the rename sub-object as a serde schema (the
/// struct IS the schema: camelCase keys, absent ones default). Mirrors
/// `symbols::DiagnosticOptions`; a malformed value leaves the defaults in place.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RenameOptions {
    pub override_scope: OverrideScope,
}

/// Identifies what we're collecting references to.
/// Is `name` the constructor SPELLING of `origin`'s language? The document
/// says so on its own constructor capture; a language whose constructor is
/// a name convention rather than a spelling (Perl's `new`) declares none,
/// which is what keeps `new` renameable.
fn is_ctor_name(origin: &FileAnalysis, name: &str) -> bool {
    crate::build::language_driver::LanguageRegistry::pack_capture_literals(
        &origin.language,
        "def.method.ctor",
    )
    .contains(name)
}

#[derive(Debug, Clone)]
pub struct TargetRef {
    pub name: String,
    /// The ORIGIN file's name spellings — what its keys are computed with.
    pub names: crate::model::file_analysis::NameSpellings,
    pub kind: TargetKind,
    /// Override-fan-out scope for callable (`Sub`/`Method`) targets — read by
    /// `collect_from_analysis` to pick family-membership (Hierarchy) vs
    /// dispatch-chain (Dispatch) matching. Irrelevant for other kinds.
    pub scope: OverrideScope,
    /// For a `Method` target, the inheritance rename-chain
    /// `[cursor_class, ..., defining_class]` computed ONCE from the
    /// originating analysis (the only file that knows the cursor class's
    /// parents). A `sub NAME` declaration in ANY class on this set is a
    /// declaration of the same callable — see `symbol_defines_target`.
    /// Empty for non-Method kinds (their decl match is the strict scope).
    pub method_classes: Vec<String>,
    /// Whether the target's defining symbol is an enum-constant shape — a
    /// name C hoists into the enclosing scope, so BARE unresolved reads
    /// (`case OP_SCOPE:`) are legitimate uses. False for receiver-reached
    /// members (fields/methods) and every Perl target: bare same-named
    /// tokens elsewhere are noise, not references (the `formatter::format`
    /// 1621-hit sweep). Minted from the defining symbol
    /// (`class_content_is_bare_constant`); the matcher may also re-derive it
    /// per scanned file when the index is in hand.
    pub bare_constant: bool,
    /// `Some(class)` when this Method target IS the class's constructor by
    /// the pack's convention (php `__construct`). Read by the rename policy
    /// alone — the name belongs to the language, so nothing renames it. Its
    /// references need no marker: a construction site mints the constructor
    /// call itself, which the ordinary member arm matches. Set in the
    /// identity lane from the pack document's own constructor capture;
    /// `None` everywhere else.
    pub ctor_of: Option<String>,
    /// Which member family this target names, minted from the fact that
    /// produced it: a `FieldAccess` cursor or a stored-member declaration is
    /// `Value`, a `MethodCall` cursor or a sub declaration `Callable`, and a
    /// target that is not a member (or one built where no cursor told) is
    /// `None`. `docs/adr/member-kinds.md`: the value side is strict — a
    /// value target is declared by stored members and referenced by value
    /// reads — while a callable keeps the call walk's value-kind fallback
    /// for declarations and never claims a value read.
    pub member_kind: Option<MemberKind>,
    /// Pack-language visibility identity: the canonical paths of the files
    /// that define this target AS THE ORIGIN FILE SEES IT (the origin itself,
    /// candidates in its include closure, and candidates whose closure reaches
    /// back to the origin — the header-decl ↔ TU-def link). The backward match
    /// side (`collect_from_analysis`) accepts a name match in a scanned file
    /// only when that file can see one of these — the same include-closure
    /// visibility forward resolution (`ScopedLookup`) applies, so gd and gr
    /// stay mirrored on the SAME key: name + visibility. Empty = no gate
    /// (every Perl target: Perl identity is package/sigil, not closure).
    pub def_paths: Vec<String>,
}

impl TargetRef {
    /// Build a `Method` target, precomputing the inheritance rename-chain
    /// from `origin` so declaration matching in any scanned file can admit
    /// `sub NAME` in an ancestor class — not just the cursor's static class.
    ///
    /// The chain can only be derived here: a base file (`BaseWorker.pm`)
    /// scanned later doesn't know its child `MyWorker`, so it can't recompute
    /// the chain that links the call's `MyWorker` invocant to the parent decl.
    pub fn method(
        name: String,
        class: String,
        member_kind: Option<MemberKind>,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
        scope: OverrideScope,
    ) -> Self {
        let method_classes =
            method_classes_for(origin, &class, &name, member_kind, module_index, scope);
        // The pack's constructor convention (php `__construct`) is a fact of
        // the METHOD TARGET itself: every builder of a Method target — the
        // rename-kind mapping, the identity lanes, implementations — gets
        // the ctor marker from this one speller, so the rename policy reads
        // it wherever the cursor landed.
        let ctor_of = is_ctor_name(origin, &name).then(|| class.clone());
        TargetRef {
            name,
            names: origin.names().clone(),
            kind: TargetKind::Method { class },
            method_classes,
            scope,
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of,
            member_kind,
        }
    }

    /// Build a `Method` target for a class-OWNED synthesized accessor (a Moo
    /// `has` reader, a DBIC column/relationship accessor). Its override family
    /// is `owned_accessor_family` — the owning class and its descendants only,
    /// NEVER a framework ancestor that happens to define a real `sub` of the
    /// same name (`DBIx::Class::PK::id`). Renaming a synthesized `id` column
    /// must not reach that generic accessor nor every unrelated sibling Result
    /// class under it. Fixed `Hierarchy` scope: an owned accessor is
    /// shared down the hierarchy by construction; the family already encodes
    /// exactly the classes that inherit it.
    pub fn owned_accessor(
        name: String,
        class: String,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Self {
        let method_classes = origin.owned_accessor_family(&class, module_index);
        TargetRef {
            name,
            names: origin.names().clone(),
            kind: TargetKind::Method { class },
            method_classes,
            scope: OverrideScope::Hierarchy,
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of: None,
            member_kind: None,
        }
    }

    /// A bare target for tests: the name and kind under test, every policy
    /// field at the value the suites all wrote by hand. ONE constructor, so
    /// a new field lands here instead of in thirty test literals — and a
    /// test that cares about a policy field says so by setting it.
    #[cfg(test)]
    pub fn for_test(name: impl Into<String>, kind: TargetKind) -> Self {
        TargetRef {
            name: name.into(),
            names: crate::model::conventions::PERL_SPELLINGS,
            kind,
            method_classes: Vec::new(),
            scope: OverrideScope::Dispatch,
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of: None,
            member_kind: None,
        }
    }

    /// Build a non-Method target (no inheritance fan-out for declarations).
    /// `origin` is the file the cursor sits in: its spellings key the
    /// target's name in every store the walk consults.
    pub fn new(name: String, kind: TargetKind, origin: &FileAnalysis) -> Self {
        debug_assert!(
            !matches!(kind, TargetKind::Method { .. }),
            "use TargetRef::method so the rename chain is populated"
        );
        TargetRef {
            name,
            names: origin.names().clone(),
            kind,
            method_classes: Vec::new(),
            scope: OverrideScope::default(),
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of: None,
            member_kind: None,
        }
    }

    /// Does this target's name belong to the LANGUAGE rather than the
    /// author? A pack's constructor convention (`__construct`) is spelled by
    /// nothing a rename could rewrite — its `new self(...)` sites carry no
    /// token naming it — and a class-keyed rail's spans are emission tokens
    /// whose names belong to the class rename. Nothing renames either one,
    /// cross-file OR locally, so the one policy method answers for
    /// `rename_edits` and for the prepareRename gate alike: an offer the
    /// rename would refuse is worse than no offer.
    pub fn rename_is_language_owned(&self) -> bool {
        self.ctor_of.is_some() || !self.sites_are_rewritable()
    }

    /// Do this target's reference spans hold tokens of its OWN name? A
    /// class-keyed rail's sites spell the CLASS the rail is keyed on, so an
    /// edit writing the target's new name over them corrupts a class
    /// reference. The collector marks the sites it emits with this and the
    /// rename policy above composes it — one answer, both readers.
    pub fn sites_are_rewritable(&self) -> bool {
        !matches!(&self.kind, TargetKind::Handler { names: RailNames::Classes, .. })
    }

    /// Whether this target renames cross-file through `refs_to` (matched by
    /// owner/scope structure across the workspace) vs. the single-file
    /// `rename_at` fallback. Per-feature policy lives on the target (rule #10),
    /// not inline in a handler. The kinds here key on a workspace-stable owner
    /// (a package, a class, a sub, a package global, a sub-owned hash key, a
    /// handler owner), so name + owner pins them in any file; a lexical or an
    /// owner-less hash key can't be matched by name alone elsewhere and stays
    /// single-file. References ignores this — it walks every kind cross-file.
    pub fn supports_cross_file_rename(&self) -> bool {
        if self.rename_is_language_owned() {
            return false;
        }
        matches!(
            self.kind,
            TargetKind::Sub { .. }
                | TargetKind::Method { .. }
                | TargetKind::Package
                | TargetKind::HashKeyOfSub { .. }
                | TargetKind::Handler { .. }
                | TargetKind::PackageVar { .. }
                | TargetKind::FileScopeValue
        )
    }

    /// Map a cursor-resolved `RenameKind` to the cross-file target, sharing
    /// the one mapping across both LSP handlers and both CLI modes so
    /// references and rename can't diverge on target identity (rule #5).
    /// `HashKey`/`Variable` aren't simple cross-file callables — they return
    /// `None` and the caller keeps its owner-expansion / lexical handling.
    pub fn from_rename_kind(
        kind: crate::model::file_analysis::RenameKind,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
        scope: OverrideScope,
    ) -> Option<Self> {
        use crate::model::file_analysis::RenameKind;
        Some(match kind {
            RenameKind::Function { name, package } => {
                // A `sub` in a class IS a method (Perl's only sub/method
                // distinction is call shape), so it carries the same override
                // family/chain — a base-`sub` rename reaches overrides + their
                // dispatch sites. A package-less script sub has no class, hence
                // no family.
                let method_classes = match &package {
                    Some(class) => method_classes_for(
                        origin,
                        class,
                        &name,
                        Some(MemberKind::Callable),
                        module_index,
                        scope,
                    ),
                    None => Vec::new(),
                };
                // Function targets keep empty def_paths HERE: a Sub cursor
                // is language-neutral (Perl subs mint the same RenameKind)
                // and Perl visibility is package-keyed, never closure-gated.
                // The pack instance of the gate is minted at the set level
                // (`CandidateSet::resolution`), on the caller-declared pack
                // routing fact. Macro-named cursors never reach this arm
                // (the canonical FileScopeValue lanes claim them first,
                // WITH def_paths).
                // The pack's constructor convention is a fact of the target
                // whichever cursor minted it: a decl-side cursor on
                // `__construct` arrives here as a Sub, and the rename policy
                // must refuse it exactly as it does the call-side Method
                // target.
                let ctor_of = package.as_ref().filter(|_| is_ctor_name(origin, &name)).cloned();
                TargetRef {
                    name,
                    names: origin.names().clone(),
                    kind: TargetKind::Sub { package },
                    method_classes,
                    scope,
                    def_paths: Vec::new(),
                    bare_constant: false,
                    ctor_of,
                    member_kind: Some(MemberKind::Callable),
                }
            }
            RenameKind::Method { name, class, member } => {
                TargetRef::method(name, class, member, origin, module_index, scope)
            }
            RenameKind::Package(name) => {
                // A class-name cursor names an identity; the matcher
                // resolves every scanned file's spelling to one too, so
                // three same-leaf `Collection`s never share a target.
                TargetRef::new(name, TargetKind::Package, origin)
            }
            RenameKind::Handler { owner, name } => {
                let names = owner.names_are(&origin.pack);
                TargetRef::new(name.clone(), TargetKind::Handler { owner, name, names }, origin)
            }
            RenameKind::HashKey(_) | RenameKind::Variable => return None,
        })
    }
}

/// What the cursor position resolves to, for cross-file queries.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// A target `refs_to` can walk: callables, packages, handlers, and
    /// hash keys whose owner resolved at build time.
    Target(TargetRef),
    /// A projection group: one source decl spelled several ways (a
    /// Corinna `field $x :param :reader` ↔ its constructor key ↔ its
    /// reader calls). `targets` walk cross-file via `refs_to`;
    /// `local_spans` are the origin-file-only spellings (the field
    /// variable is lexical to the class block). Every span — local and
    /// walked — covers a bare name token, so rename writes one
    /// replacement text everywhere and references list them uniformly.
    Group {
        local_spans: Vec<Span>,
        /// Spellings pinned to a specific file — the class file's
        /// variable/decl spans when the group was minted remotely (the
        /// cursor sat in a consumer; the source decl lives with the
        /// class).
        pinned_spans: Vec<(PathBuf, Span)>,
        /// Where the group is DECLARED — a subset of the spellings above,
        /// each carrying its file (`None` = the origin, matching
        /// `local_spans`). The declaration axis of the identity, so
        /// goto-def projects the same group references walks instead of
        /// re-deriving where an inherited attr was declared.
        decl_spans: Vec<(Option<PathBuf>, Span)>,
        members: Vec<GroupMember>,
    },
    /// Inherently file-local: lexical variables, and hash keys with no
    /// resolvable owner. Callers keep their single-file path.
    Local,
}

/// One walkable member of a projection group, carrying its own rename
/// rule: bare spellings take the plain new name; name-mapped accessors
/// (`has_size`) re-derive theirs; members whose names don't embed the
/// attr join references but skip rename (honest).
#[derive(Debug, Clone)]
pub struct GroupMember {
    pub target: TargetRef,
    pub rename: MemberRename,
}

#[derive(Debug, Clone)]
pub enum MemberRename {
    Bare,
    Affixed { prefix: String, suffix: String },
    Skip,
}

impl MemberRename {
    pub(super) fn text_for(&self, bare_new: &str) -> Option<String> {
        match self {
            MemberRename::Bare => Some(bare_new.to_string()),
            MemberRename::Affixed { prefix, suffix } => {
                Some(format!("{}{}{}", prefix, bare_new, suffix))
            }
            MemberRename::Skip => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    /// An `our` (package-global) variable. `package` is the declaring
    /// package; `target.name` is the sigil-bearing name (`$debug`). Unlike a
    /// lexical `my` (which stays `Local`/single-file), a package global is
    /// reachable everywhere as `$Pkg::var`, so rename fans out cross-file:
    /// matches the `our` decl, every qualified `$Pkg::var` access in any file,
    /// and the declaring file's unqualified reads that resolve to it.
    PackageVar { package: String },
    /// A sub defined in a specific package. `None` = the sub has
    /// no package context (top-level script). Matches `Sub`/`Method`
    /// symbols whose `package` field equals this, and `FunctionCall`
    /// refs whose `resolved_package` equals this. Package-scoping
    /// mirrors method's class-scoping — name-only matching
    /// cross-links `Foo::run` and `Bar::run`.
    Sub { package: Option<String> },
    /// A method on a specific class. Matches `Sub`/`Method` symbols
    /// whose `package == class`, and `MethodCall` refs whose invocant
    /// resolves to `class`.
    Method { class: String },
    /// A package/class/module name — matches PackageRef refs.
    Package,
    /// A hash key owned by a specific sub's return value. `package` is the
    /// sub's defining package (or None for top-level / unpackaged subs);
    /// matches `HashKeyOwner::Sub { package, name }` by structural equality.
    HashKeyOfSub { package: Option<String>, name: String },
    /// A hash key owned by a class (Moo `has` slots, DBIC columns on a Result class).
    HashKeyOfBridged(String),
    /// An attr's internal hash slot (`$self->{attr}` — or any
    /// `$obj->{attr}` poke; Perl culture is promiscuous about reaching
    /// into the hashref). STRICT `HashKeyOwner::Class` matching, never
    /// `found_by` — broadening would leak other subs' same-named arg
    /// keys into the projection group this member serves.
    InternalHashKey { class: String },
    /// A `Handler` symbol registered on a class (Mojo events, Dancer
    /// routes, etc.). Both the definition (`Handler` symbol) and call
    /// sites (`DispatchCall` refs) match; stacked registrations all
    /// surface separately so features can enumerate every handler.
    Handler {
        owner: HandlerOwner,
        name: String,
        /// What the rail's names denote, minted from the origin's rail
        /// declarations (`HandlerOwner::names_are`): a class-keyed rail's
        /// spans are emission/handler tokens of the CLASS, so its target is
        /// navigable but never rewritten.
        names: RailNames,
    },
    /// A pack-language file-scope value reachable by BARE NAME from any file
    /// that can see it (C's flat linkage): an object- or function-like
    /// `#define`, a global variable, an anonymous-enum constant. The backward
    /// (def→uses) mirror of the by-name forward resolutions — the macro
    /// goto-def lane and the generic cross-file name tail — so both
    /// directions share one key: the bare name. Matches every `#define` of
    /// the name (config variants included, never pruned), file-scope
    /// `Variable` decls, and name-keyed bare reads / unresolved calls /
    /// type-position uses. Never minted from a Perl cursor (Perl variables
    /// carry sigils; Perl callables aren't `Variable` symbols).
    FileScopeValue,
}

/// A located reference in some file.
#[derive(Debug, Clone)]
pub struct RefLocation {
    pub key: FileKey,
    pub span: Span,
    /// Read/Write/Declaration — the access classification the matcher minted
    /// at the site. `highlights()` renders it as the LSP highlight kind;
    /// other projections carry it for symmetry (a reference IS its access).
    pub access: AccessKind,
    /// Whether rename may rewrite this span, and — when it may not — WHY.
    /// The producer that saw the site knows the reason (it saw the macro,
    /// the fold, the rail emission), so no consumer re-derives it from the
    /// span, and rename's policy reads the reason rather than the language.
    pub rewritable: Rewritable,
    /// A per-candidate fact worth surfacing beside the location — a macro
    /// variant's reachability verdict, a delegation see-through note. LSP
    /// `Location` has no label slot so the editor adapter drops it (ordering
    /// conveys rank); the CLI renders it and the gold harness asserts on it.
    pub label: Option<String>,
}

/// May rename rewrite a located reference's span?
///
/// A site that is not rewritable is still a reference — references lists it —
/// and the reason decides what rename does with it: most reasons SKIP (the
/// token that does spell the target is collected elsewhere, so the remaining
/// edits are complete on their own), while a reason whose site would be left
/// silently wrong REFUSES the whole edit set. `NotRewritable::refuses_rename`
/// is that split, stated once for every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewritable {
    /// The span holds the target's own name token.
    Yes,
    No(NotRewritable),
}

/// Why a span may not be rewritten. Closed: a producer that cannot name its
/// reason is emitting a site it has not understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRewritable {
    /// The name reached this site FOLDED out of a variable or constant
    /// (`my $e = 'ready'; $obj->on($e)`) — the span is the variable.
    /// Skipped: the literal the fold came from is collected on its own and
    /// carries the edit.
    ConstFolded,
    /// The token spells a delegating macro (`#define IncRef(sv)
    /// Perl_Inc(sv)`), not the target. REFUSES: the macro body is not a
    /// collected span, so an edit set that merely skipped this site would
    /// leave the delegation chain pointing at the old name — code that still
    /// compiles and does the wrong thing.
    MacroDelegated,
    /// A class-keyed rail's emission site: the token spells the CLASS the
    /// rail is keyed on. Skipped — that token renames with the class.
    RailEmission,
    /// The span declares or spells a DIFFERENT name: a template
    /// specialization's whole `X<args>` spelling, a descendant class's own
    /// declaration, a call-hierarchy item's anchor on its caller. Skipped;
    /// the target's own token is collected separately.
    OtherNameToken,
    /// No name token at all — the file-top anchor a module resolves to when
    /// its package symbol was never scanned. Skipped.
    NoNameToken,
}

impl NotRewritable {
    /// Would leaving this site unedited silently break the code? Then rename
    /// refuses the whole set rather than emitting a partial edit.
    pub fn refuses_rename(self) -> bool {
        matches!(self, NotRewritable::MacroDelegated)
    }

    /// What a refusal tells the user — the real reason, never a stand-in for
    /// another language's.
    pub fn describe(self) -> &'static str {
        match self {
            NotRewritable::ConstFolded => "sites reached through a folded name",
            NotRewritable::MacroDelegated => {
                "sites spelled through a delegating macro (the macro body is not rewritten)"
            }
            NotRewritable::RailEmission => "sites emitted on a class-keyed rail",
            NotRewritable::OtherNameToken => "sites whose token spells another name",
            NotRewritable::NoNameToken => "sites that carry no name token",
        }
    }
}

impl Rewritable {
    pub fn is_yes(self) -> bool {
        matches!(self, Rewritable::Yes)
    }

    /// The reason, when there is one.
    pub fn reason(self) -> Option<NotRewritable> {
        match self {
            Rewritable::Yes => None,
            Rewritable::No(r) => Some(r),
        }
    }
}

impl RefLocation {
    /// May rename write over this span?
    pub fn is_rewritable(&self) -> bool {
        self.rewritable.is_yes()
    }

    pub fn to_url(&self) -> Option<Url> {
        match &self.key {
            FileKey::Url(u) => Some(u.clone()),
            FileKey::Path(p) => Url::from_file_path(p).ok(),
        }
    }
}

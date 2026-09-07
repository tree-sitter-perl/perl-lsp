//! The resolution vocabulary: what a cursor can resolve TO (`TargetRef`,
//! `ResolvedTarget`, `TargetKind`), where a hit lives (`RefLocation`), and
//! the per-feature policy those types carry (rename scope/options, group
//! member rename rules).
use super::*;

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
#[derive(Debug, Clone)]
pub struct TargetRef {
    pub name: String,
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
    /// the pack's convention (php `__construct`): references then also
    /// admit the class's construction sites (`new Foo(...)` — the ctor
    /// FunctionCall ref carries the CLASS name), non-rewritable, since the
    /// token spells the class. Set in the identity lane from
    /// `PackFacts::constructor_names`; `None` everywhere else.
    pub ctor_of: Option<String>,
    /// The namespace the ORIGIN's scope pins for this target's class leaf
    /// (`CrossFileLookup::pinned_namespace` — a use-map axis's `use` row,
    /// own declaration, or own namespace). Class-keyed targets are leaf-keyed everywhere else (the
    /// override family, the dispatch chain, the by-name index), and three
    /// same-leaf `Factory`s share every one of those; the pin is what tells
    /// a scanned file's `Factory` from the target's. `None` = no claim
    /// (Perl, cpp, an un-imported leaf), and the gate stands down.
    pub class_ns: Option<String>,
    /// The written shape of the member this target names, set ONLY when
    /// the declaring class overloads the name across kinds (a property AND
    /// a method called `recorded` — `member_kinds_overloaded`). Declaration
    /// and reference matching are then shape-strict; everywhere else it is
    /// `Unknown` and the matchers stay name-keyed as before.
    pub member_shape: crate::model::file_analysis::MemberShape,
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
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
        scope: OverrideScope,
    ) -> Self {
        let method_classes = method_classes_for(origin, &class, &name, module_index, scope);
        // The pack's constructor convention (php `__construct`) is a fact of
        // the METHOD TARGET itself: every builder of a Method target — the
        // rename-kind mapping, the identity lanes, implementations — gets
        // the ctor marker from this one speller.
        let ctor_of = origin
            .pack
            .constructor_names
            .iter()
            .any(|c| c == &name)
            .then(|| class.clone());
        let class_ns = module_index.and_then(|idx| idx.pinned_namespace(&class));
        TargetRef {
            name,
            kind: TargetKind::Method { class },
            method_classes,
            scope,
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of,
            class_ns,
            member_shape: Default::default(),
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
        let class_ns = module_index.and_then(|idx| idx.pinned_namespace(&class));
        TargetRef {
            name,
            kind: TargetKind::Method { class },
            method_classes,
            scope: OverrideScope::Hierarchy,
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of: None,
            class_ns,
            member_shape: Default::default(),
        }
    }

    /// Build a non-Method target (no inheritance fan-out for declarations).
    pub fn new(name: String, kind: TargetKind) -> Self {
        debug_assert!(
            !matches!(kind, TargetKind::Method { .. }),
            "use TargetRef::method so the rename chain is populated"
        );
        TargetRef {
            name,
            kind,
            method_classes: Vec::new(),
            scope: OverrideScope::default(),
            def_paths: Vec::new(),
            bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
        }
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
        // A pack's constructor-convention name (`__construct`) is the
        // language's, not the author's: nothing renames it, and its `new
        // self(...)` sites carry no token that spells it.
        if self.ctor_of.is_some() {
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
                    Some(class) => method_classes_for(origin, class, &name, module_index, scope),
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
                // `__construct` arrives here as a Sub, and its references
                // must admit the class's `new Foo(...)` sites exactly as the
                // call-side Method target does.
                let ctor_of = package
                    .as_ref()
                    .filter(|_| origin.pack.constructor_names.iter().any(|c| c == &name))
                    .cloned();
                let class_ns = package
                    .as_deref()
                    .and_then(|c| module_index.and_then(|idx| idx.pinned_namespace(c)));
                // A Sub cursor names a callable; the shape matters only where
                // the class also stores a value under the name.
                let member_shape = match package.as_deref() {
                    Some(cls) if origin.member_kinds_overloaded(cls, &name, module_index) => {
                        crate::model::file_analysis::MemberShape::Callable
                    }
                    _ => Default::default(),
                };
                TargetRef {
                    name,
                    kind: TargetKind::Sub { package },
                    method_classes,
                    scope,
                    def_paths: Vec::new(),
                    bare_constant: false,
                    ctor_of,
                    class_ns,
                    member_shape,
                }
            }
            RenameKind::Method { name, class } => {
                TargetRef::method(name, class, origin, module_index, scope)
            }
            RenameKind::Package(name) => {
                // A class-name cursor is leaf-keyed like a member's class:
                // the origin's use-map pin tells its `Collection` from the
                // two other files' `Collection`s in the references walk.
                let class_ns = module_index.and_then(|idx| idx.pinned_namespace(&name));
                let mut t = TargetRef::new(name, TargetKind::Package);
                t.class_ns = class_ns;
                t
            }
            RenameKind::Handler { owner, name } => {
                TargetRef::new(name.clone(), TargetKind::Handler { owner, name })
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
    /// Whether rename may rewrite this span. `false` for a site whose name has
    /// no literal token to replace — a const-folded event name
    /// (`my $e = 'ready'; $obj->on($e)`) whose dispatch span IS the variable.
    /// References lists it (it's a real use); rename skips it (rewriting the
    /// variable would corrupt it). True for every literal occurrence.
    pub rewritable: bool,
    /// A per-candidate fact worth surfacing beside the location — a macro
    /// variant's reachability verdict, a delegation see-through note. LSP
    /// `Location` has no label slot so the editor adapter drops it (ordering
    /// conveys rank); the CLI renders it and the gold harness asserts on it.
    pub label: Option<String>,
}

impl RefLocation {
    pub fn to_url(&self) -> Option<Url> {
        match &self.key {
            FileKey::Url(u) => Some(u.clone()),
            FileKey::Path(p) => Url::from_file_path(p).ok(),
        }
    }
}

//! The diagnostics lanes' vocabulary: what a lane answers, and what a lane
//! needs beyond the analysis itself.
//!
//! A lane is a `FileAnalysis` method returning `Vec<Finding>`. It states
//! FACTS — which span, which code, and the values a message is built from —
//! and never a rendered string: anything that wants "is this variable
//! unused" gets the answer without the LSP's `Diagnostic` struct, and the
//! adapter is the one place that turns a finding into wire text
//! (`docs/adr/php-diagnostics.md`).

use super::*;

/// Every diagnostic code the lanes mint, spelled ONCE. Clients filter and
/// configure on these strings and the per-file yield counters key on them,
/// so a literal at a mint site is both a code nobody can configure against
/// and a silently separate metric bucket.
pub mod codes {
    pub const UNRESOLVED_FUNCTION: &str = "unresolved-function";
    pub const UNRESOLVED_METHOD: &str = "unresolved-method";
    /// A member found on a same-named class in ANOTHER namespace because
    /// the class this file names is not indexed — the honest over-
    /// approximation, said out loud (`docs/prompt-class-identity.md`).
    pub const RESOLVED_BY_WIDENING: &str = "resolved-by-widening";
    pub const UNDEF_DEREF: &str = "undef-deref";
    pub const OPTIONAL_DEREF: &str = "optional-deref";
    pub const DEREF_SHAPE_MISMATCH: &str = "deref-shape-mismatch";
    pub const ROLE_REQUIRES_UNFULFILLED: &str = "role-requires-unfulfilled";
    pub const HELPER_NOT_LOADED: &str = "helper-not-loaded";
    pub const UNRESOLVED_DISPATCH: &str = "unresolved-dispatch";
    pub const UNKNOWN_HASH_KEY: &str = "unknown-hash-key";
    /// A member read as a property that no declaration of the receiver's
    /// class provides.
    pub const UNDEFINED_PROPERTY: &str = "undefined-property";
    /// A member reached from outside the scope its access modifier allows.
    pub const NON_PUBLIC_ACCESS: &str = "non-public-access";
    /// A call whose written argument count the callee's declared list
    /// cannot take.
    pub const ARITY_MISMATCH: &str = "arity-mismatch";
    /// A read of a name nothing in the callable binds.
    pub const UNDEFINED_VARIABLE: &str = "undefined-variable";
    /// A local written and never read.
    pub const UNUSED_VARIABLE: &str = "unused-variable";
    /// An import row binding a name the file never spells.
    pub const UNUSED_IMPORT: &str = "unused-import";
    /// A class name the file's namespace evidence cannot supply.
    pub const UNDEFINED_TYPE: &str = "undefined-type";
    /// A contract callable a concrete composer neither declares nor
    /// inherits.
    pub const UNIMPLEMENTED_METHOD: &str = "unimplemented-method";
    /// A callable with an inferrable return and no native annotation, in a
    /// file that writes them.
    pub const MISSING_RETURN_TYPE: &str = "missing-return-type";
    /// A use of a declaration marked deprecated.
    pub const DEPRECATED: &str = "deprecated";
    /// A use on a named rail that no definition on that rail answers, where
    /// the rail's own document declares no code of its own. The rail rides
    /// the diagnostic's `data`, so the set of codes this adapter can mint
    /// stays closed whatever a plugin's rail document is called.
    pub const UNDEFINED_RAIL_NAME: &str = "undefined-rail-name";
    /// A use of a local moved from (`std::move`), opt-in.
    pub const USE_AFTER_MOVE: &str = "use-after-move";
}


/// One lane's answer: where, and the facts its message and quick-fix
/// payload are built from. The wire code is a pure function of the data
/// (`FindingData::code`), so a finding cannot carry one that disagrees.
#[derive(Debug, Clone)]
pub struct Finding {
    pub span: Span,
    pub data: FindingData,
}

impl Finding {
    pub fn new(span: Span, data: FindingData) -> Self {
        Finding { span, data }
    }

    /// The wire code this finding reports under.
    pub fn code(&self) -> &'static str {
        self.data.code()
    }
}

/// The closed set of things a lane can find. One variant per shape of
/// message, carrying the values it needs — never the message.
#[derive(Debug, Clone)]
pub enum FindingData {
    /// A member no declaration of the receiver's class provides. `kind` is
    /// the family the SITE asked for, which is what tells a missing method
    /// from a missing property.
    UndefinedMember { kind: MemberKind, name: String },
    /// A member reached from outside the scope its access modifier allows.
    NonPublicAccess { name: String, owner: String, from: Option<String> },
    /// A call with fewer arguments than the callee requires.
    TooFewArguments { expected: usize, found: usize },
    /// A call with more arguments than a non-variadic callee takes.
    TooManyArguments { expected: usize, found: usize },
    /// A member that resolved only because a same-named class in another
    /// namespace answered (`MethodResolution::CrossFile::widened`).
    ResolvedByWidening { name: String, on: String, wanted: String },
    /// A use of a declaration marked deprecated, with the notice text the
    /// declaration carried.
    Deprecated { name: String, note: Option<String> },
    /// A read of a name nothing in the callable binds.
    UndefinedVariable { name: String },
    /// A local written and never read.
    UnusedVariable { name: String },
    /// An import row binding a name the file never spells. `sole_row` is
    /// the whole statement's row range when this import is the only one on
    /// it — what a remove-the-line fix needs.
    UnusedImport { bound: String, sole_row: Option<(usize, usize)> },
    /// A class name the file's namespace evidence cannot supply, with every
    /// namespace that DOES declare the leaf (the import quick-fix's offers).
    UndefinedType { identity: String, candidates: Vec<String> },
    /// A concrete class that neither declares nor inherits the contract
    /// callables its roles require.
    UnimplementedContracts { class: String, missing: Vec<UnfulfilledRequire> },
    /// A callable with an inferrable return, no native annotation, in a file
    /// that writes them.
    MissingReturnType { name: String, spelling: String },
}

impl FindingData {
    /// The wire code this shape reports under. Clients filter on these
    /// strings and the per-file yield counters key on them, so the pairing
    /// lives once, here, beside the shapes it names.
    pub fn code(&self) -> &'static str {
        match self {
            FindingData::UndefinedMember { kind: MemberKind::Value, .. } => {
                codes::UNDEFINED_PROPERTY
            }
            FindingData::UndefinedMember { .. } => codes::UNRESOLVED_METHOD,
            FindingData::NonPublicAccess { .. } => codes::NON_PUBLIC_ACCESS,
            FindingData::TooFewArguments { .. } | FindingData::TooManyArguments { .. } => {
                codes::ARITY_MISMATCH
            }
            FindingData::ResolvedByWidening { .. } => codes::RESOLVED_BY_WIDENING,
            FindingData::Deprecated { .. } => codes::DEPRECATED,
            FindingData::UndefinedVariable { .. } => codes::UNDEFINED_VARIABLE,
            FindingData::UnusedVariable { .. } => codes::UNUSED_VARIABLE,
            FindingData::UnusedImport { .. } => codes::UNUSED_IMPORT,
            FindingData::UndefinedType { .. } => codes::UNDEFINED_TYPE,
            FindingData::UnimplementedContracts { .. } => codes::UNIMPLEMENTED_METHOD,
            FindingData::MissingReturnType { .. } => codes::MISSING_RETURN_TYPE,
        }
    }
}

/// What a lane needs that the analysis cannot answer for itself.
///
/// The cross-file lookup and its verdict about its own bulk pass, plus the
/// two document-declared sets with no per-site fact to mint: a runtime's
/// type names, and whether the language's import rows bind a name at all.
/// Those documents live above this layer, so the tier that can read them
/// hands them down here rather than a lane reaching up for them. Anything a
/// capture states AT a site is on the ref or the symbol instead (rule #11) —
/// it never arrives here as a set of spellings to match back.
pub struct LaneFacts<'a> {
    pub idx: Option<&'a dyn CrossFileLookup>,
    /// Is absence meaningful yet? A lane that reports "no such name" claims
    /// the index would have it if it existed; while the index is warming
    /// that claim is false (`IndexState`).
    pub index_settled: bool,
    /// The type names the runtime provides, which the workspace carries no
    /// declaration for.
    pub builtin_types: &'a [String],
    /// Do the language's import rows bind a name the file then spells? The
    /// document says so by minting `@import.binds`; a text-splicing include
    /// binds nothing and can never be unused.
    pub imports_bind_names: bool,
}

impl LaneFacts<'_> {
    /// The lanes that report absence answer only against a settled index.
    pub fn settled_lookup(&self) -> Option<&dyn CrossFileLookup> {
        self.index_settled.then_some(self.idx).flatten()
    }
    pub fn is_builtin_type(&self, leaf: &str) -> bool {
        self.builtin_types.iter().any(|b| b == leaf)
    }
}

//! Name relations as tracked values. Every relation the index derives from
//! many files (who declares a name, who exports it, a class's children)
//! comes back as a `Tracked<T>`: the value is private, and `read` is the
//! only way out, so a computation inside an engine frame cannot use a
//! relation without recording it as a dependency (`engine::Dep::Lookup`).
//!
//! A dependency is the relation's identity (`Lookup`, a closed enum: each
//! variant is one relation with its arguments) plus the hash of the value
//! read. Verify recomputes the relation through the same index method and
//! compares hashes (`lookup_hash`), so an edit that leaves a relation's
//! value alone leaves every answer that read it standing. The hash is the
//! dependency in a later process too.
//!
//! A relation's value names files, never their content: content is read
//! through the residency views, which record the file.

use std::path::PathBuf;
use std::sync::Arc;

use crate::model::file_analysis::{CachedModule, CrossFileLookup};

use super::engine::{self, Dep, MentionKind};

/// One name relation with its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Lookup {
    /// `get_cached`: the name's winning provider.
    Winner(String),
    /// `def_candidates`: every file declaring the name.
    DefCandidates(String),
    /// `visible_def_candidates`.
    VisibleDefs(String),
    /// `visible_def_candidates_widening`.
    VisibleDefsWidening(String),
    /// `candidate_defining_sub_in_package`.
    DefiningSub { module_key: String, pkg: String, member: String },
    /// `parents_cached`.
    Parents(String),
    /// `modules_with_symbol`.
    ModulesWithSymbol(String),
    /// `find_exporters`.
    Exporters(String),
    /// `defining_module_cached`.
    DefiningModule { entry: String, name: String },
    /// `module_declaring_method_in_package`.
    DeclaringModule { name: String, class: String },
    /// `module_path_cached`.
    ModulePath(String),
    /// `cached_by_path`: whether the path is held.
    ByPath(PathBuf),
    /// `direct_children_of`.
    Children(String),
    /// `direct_specializations_of`.
    Specializations(String),
    /// `class_is_bridged_to`.
    Bridged(String),
    /// `bridging_modules`.
    BridgingModules(String),
    /// `for_each_loader_shape`.
    LoaderShapes,
    /// `for_each_cached`: every name with its winner.
    AllModules,
    /// `for_each_cached_file`: every held file.
    AllFiles,
    /// `open_callers`: which files are open.
    OpenCallers,
    /// `mentions_of`: the files mentioning a name.
    Mentions(MentionKind, String),
}

/// A relation's value, hashed for its dependency.
pub trait LookupHash {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher);
}

impl LookupHash for Arc<CachedModule> {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.path.hash(h);
    }
}

impl LookupHash for PathBuf {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.hash(h);
    }
}

impl LookupHash for String {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.hash(h);
    }
}

impl LookupHash for bool {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.hash(h);
    }
}

impl LookupHash for crate::model::file_analysis::InferredType {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        bincode::serialize(self).unwrap_or_default().hash(h);
    }
}

impl LookupHash for str {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.hash(h);
    }
}

impl LookupHash for () {
    fn lookup_hash(&self, _h: &mut std::collections::hash_map::DefaultHasher) {}
}

impl<T: LookupHash> LookupHash for Option<T> {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.is_some().hash(h);
        if let Some(v) = self {
            v.lookup_hash(h);
        }
    }
}

impl<T: LookupHash> LookupHash for Vec<T> {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.len().hash(h);
        for v in self {
            v.lookup_hash(h);
        }
    }
}

impl LookupHash for Arc<Vec<PathBuf>> {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        (**self).lookup_hash(h);
    }
}

impl LookupHash for Arc<Vec<Arc<CachedModule>>> {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        (**self).lookup_hash(h);
    }
}

impl<A: LookupHash, B: LookupHash> LookupHash for (A, B) {
    fn lookup_hash(&self, h: &mut std::collections::hash_map::DefaultHasher) {
        self.0.lookup_hash(h);
        self.1.lookup_hash(h);
    }
}

/// Hash a value the way its dependency records it.
pub fn hash_of<T: LookupHash + ?Sized>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.lookup_hash(&mut h);
    h.finish()
}

/// Accumulates a visited relation's hash, for the `for_each_*` relations
/// whose values are handed to a callback instead of returned.
#[derive(Default)]
pub struct VisitHash(std::collections::hash_map::DefaultHasher);

impl VisitHash {
    pub fn add<T: LookupHash + ?Sized>(&mut self, v: &T) {
        v.lookup_hash(&mut self.0);
    }
    pub fn finish(self) -> u64 {
        use std::hash::Hasher;
        self.0.finish()
    }
}

/// A relation's value, readable only through `read`, which records it.
#[must_use = "a relation is used through `read`, which records it as a dependency"]
pub struct Tracked<T> {
    value: T,
    lookup: Lookup,
    /// Set when the value no longer hashes as the relation it came from
    /// (`map` narrowed it, or a visit already folded the hash).
    hash: Option<u64>,
}

impl<T: LookupHash> Tracked<T> {
    pub fn new(lookup: Lookup, value: T) -> Tracked<T> {
        Tracked { value, lookup, hash: None }
    }

    /// The relation `lookup`, computed by `f` from other relations and
    /// file content. Those reads are not recorded: verifying recomputes
    /// `lookup` itself, which reads them again.
    pub fn derive(lookup: Lookup, f: impl FnOnce() -> T) -> Tracked<T> {
        Tracked { value: engine::untracked(f), lookup, hash: None }
    }

    /// The hash this relation's dependency records.
    pub fn dep_hash(&self) -> u64 {
        self.hash.unwrap_or_else(|| hash_of(&self.value))
    }

    /// The value, recorded into the open engine frame.
    pub fn read(self) -> T {
        if engine::frame_open() {
            let hash = self.dep_hash();
            engine::note_dep(Dep::Lookup { lookup: self.lookup, hash });
        }
        self.value
    }

    /// A value derived from this relation's, still depending on the
    /// relation: verifying recomputes the relation, not the derivation.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Tracked<U> {
        let hash = self.dep_hash();
        Tracked { value: f(self.value), lookup: self.lookup, hash: Some(hash) }
    }
}

impl<T> Tracked<T> {
    /// A relation a memo served, with the hash its miss recorded: a memo
    /// hit replays the dependency instead of serving the value bare.
    pub(crate) fn from_memo(value: T, lookup: Lookup, hash: u64) -> Tracked<T> {
        Tracked { value, lookup, hash: Some(hash) }
    }
}

impl Tracked<()> {
    /// A relation whose value went to a callback, hashed as it was visited.
    pub fn visited(lookup: Lookup, hash: VisitHash) -> Tracked<()> {
        Tracked { value: (), lookup, hash: Some(hash.finish()) }
    }
}

/// Recompute `lookup` through `idx` and hash it: the verify half of a
/// `Dep::Lookup`. Exhaustive, so a new relation must say how it recomputes.
pub fn lookup_hash(idx: &dyn CrossFileLookup, lookup: &Lookup) -> u64 {
    match lookup {
        Lookup::Winner(n) => idx.get_cached(n).dep_hash(),
        Lookup::DefCandidates(n) => idx.def_candidates(n).dep_hash(),
        Lookup::VisibleDefs(n) => idx.visible_def_candidates(n).dep_hash(),
        Lookup::VisibleDefsWidening(n) => idx.visible_def_candidates_widening(n).dep_hash(),
        Lookup::DefiningSub { module_key, pkg, member } => {
            idx.candidate_defining_sub_in_package(module_key, pkg, member).dep_hash()
        }
        Lookup::Parents(n) => idx.parents_cached(n).dep_hash(),
        Lookup::ModulesWithSymbol(n) => idx.modules_with_symbol(n).dep_hash(),
        Lookup::Exporters(n) => idx.find_exporters(n).dep_hash(),
        Lookup::DefiningModule { entry, name } => idx.defining_module_cached(entry, name).dep_hash(),
        Lookup::DeclaringModule { name, class } => idx.module_declaring_method_in_package(name, class).dep_hash(),
        Lookup::ModulePath(n) => idx.module_path_cached(n).dep_hash(),
        Lookup::ByPath(p) => idx.cached_by_path(p).dep_hash(),
        Lookup::Children(n) => idx.direct_children_of(n).dep_hash(),
        Lookup::Specializations(n) => idx.direct_specializations_of(n).dep_hash(),
        Lookup::Bridged(n) => idx.class_is_bridged_to(n).dep_hash(),
        Lookup::BridgingModules(n) => idx.bridging_modules(n).dep_hash(),
        Lookup::LoaderShapes => idx.for_each_loader_shape(&mut |_, _| {}).dep_hash(),
        Lookup::AllModules => idx.for_each_cached(&mut |_, _| {}).dep_hash(),
        Lookup::AllFiles => idx.for_each_cached_file(&mut |_| {}).dep_hash(),
        Lookup::OpenCallers => idx.open_callers().dep_hash(),
        Lookup::Mentions(kind, key) => engine::mentions_of(idx, *kind, key).dep_hash(),
    }
}

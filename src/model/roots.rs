//! The members a language guarantees every class: Perl's `UNIVERSAL`,
//! Python's `object`, PHP's implicit constructor. They answer a member
//! ask only after the whole MRO misses (`FileAnalysis::root_member`),
//! so a real declaration anywhere in the chain always wins.
//!
//! The vocabulary is a data document per language (`language_root()`
//! manifests, `frameworks/roots/*.rhai`), installed process-wide when the
//! plugin registry loads. It is keyed by language id, never baked per
//! file: every file of a language shares the one value.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::file_analysis::MemberKey;

/// Which asks a root member answers: one that spells its name, one that
/// asks for the role (a construction asks for the constructor), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RootReach {
    #[default]
    Name,
    Role,
    Both,
}

impl RootReach {
    fn by_name(self) -> bool {
        matches!(self, RootReach::Name | RootReach::Both)
    }
    fn by_role(self) -> bool {
        matches!(self, RootReach::Role | RootReach::Both)
    }
}

/// One member of a language's root surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootMember {
    pub name: String,
    #[serde(default)]
    pub constructor: bool,
    #[serde(default)]
    pub reach: RootReach,
    /// The parameters a caller writes, receiver excluded.
    #[serde(default)]
    pub params: Vec<String>,
    #[serde(default)]
    pub doc: Option<String>,
}

impl RootMember {
    /// Does this member answer `key`?
    pub fn answers(&self, key: MemberKey<'_>) -> bool {
        match key {
            MemberKey::Named(n) => self.reach.by_name() && self.name == n,
            MemberKey::Constructor => self.constructor && self.reach.by_role(),
        }
    }

    /// Visible to a caller who types the name (completion's list).
    pub fn named(&self) -> bool {
        self.reach.by_name()
    }
}

/// One `language_root()` manifest entry: a language's root class (when it
/// is a real class code can reopen) and the members it guarantees.
/// Several documents for one language merge; see `LanguageRoots`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootDocument {
    pub language: String,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub members: Vec<RootMember>,
}

/// A language's merged root surface.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RootSurface {
    pub class: Option<String>,
    pub members: Vec<RootMember>,
}

impl RootSurface {
    pub fn member(&self, key: MemberKey<'_>) -> Option<&RootMember> {
        self.members.iter().find(|m| m.answers(key))
    }
}

/// Every language's root surface, by language id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LanguageRoots {
    by_language: HashMap<String, RootSurface>,
}

impl LanguageRoots {
    /// Merge documents in load order (bundled first, then plugin dirs): the
    /// first document naming a class sets it, and a member a later document
    /// repeats by name keeps the earlier declaration.
    pub fn from_documents<'a>(docs: impl IntoIterator<Item = &'a RootDocument>) -> Self {
        let mut by_language: HashMap<String, RootSurface> = HashMap::new();
        for doc in docs {
            let surface = by_language.entry(doc.language.clone()).or_default();
            if surface.class.is_none() {
                surface.class = doc.class.clone();
            }
            for m in &doc.members {
                if !surface.members.iter().any(|have| have.name == m.name) {
                    surface.members.push(m.clone());
                }
            }
        }
        LanguageRoots { by_language }
    }

    pub fn surface(&self, language: &str) -> Option<&RootSurface> {
        self.by_language.get(language)
    }

    /// Is `class` the root class of `language`?
    pub fn is_root_class(&self, language: &str, class: &str) -> bool {
        self.surface(language).and_then(|s| s.class.as_deref()) == Some(class)
    }
}

/// Replaced, not set once: the registry rebuilds when the workspace root
/// changes the plugin set, and the latest build's documents are the ones
/// in force.
static INSTALLED: RwLock<Option<Arc<LanguageRoots>>> = RwLock::new(None);

/// Install the merged roots. The plugin registry calls this each time it
/// loads.
pub fn install(roots: LanguageRoots) {
    if let Ok(mut g) = INSTALLED.write() {
        *g = Some(Arc::new(roots));
    }
}

/// The installed roots; empty before any registry has loaded.
pub fn installed() -> Arc<LanguageRoots> {
    INSTALLED
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// Whether a registry has installed roots yet.
pub fn is_installed() -> bool {
    INSTALLED.read().map(|g| g.is_some()).unwrap_or(false)
}

#[cfg(test)]
#[path = "roots_tests.rs"]
mod roots_tests;

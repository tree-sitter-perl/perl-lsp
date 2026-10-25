//! The use-map resolver: a file's `use` rows and its own namespace, folded
//! into ONE function from a class spelling as written to the class's
//! identity (its fully-qualified name). Every consumer of "what does this
//! spelling mean here" — the use-map visibility pins, the extraction pass
//! that mints class identities, hover's rendering — calls this rather than
//! re-deriving the head/alias/row/own-namespace ladder, so the rules cannot
//! drift between them.
//!
//! A row binds only where it was written: each carries its `reach`, and a
//! map is built FOR a site (`FileAnalysis::use_map_at`), seeing the rows
//! that reach it and the namespace in force there. Two namespace sections
//! of one file never read each other's rows.
//!
//! Pure `&str` over the pack lanes: no tree, no index. The separator is
//! the pack's declared one (`PackFacts::names`); a language whose
//! spellings are already identities (C's flat linkage) never builds one.
//! Rows and answers are (namespace, leaf) parts: the only strings split
//! here are written spellings, never an identity this code joined.

use super::{contains_point, namespace_range_around, namespace_range_at, NameSpellings, NamespaceRange, SectionKind, Span};
use crate::model::conventions::join_qualified as join;
use serde::{Deserialize, Serialize};
use tree_sitter::Point;

/// A class name as the parts its producer holds — the namespace (the
/// global one is `""`) and the leaf. The joined identity is formed only
/// where a string key is needed (`identity`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedName {
    pub namespace: String,
    pub leaf: String,
}

impl QualifiedName {
    pub fn new(namespace: &str, leaf: &str) -> Self {
        Self { namespace: namespace.to_string(), leaf: leaf.to_string() }
    }

    /// The fully-qualified name under `sep`.
    pub fn identity(&self, sep: &str) -> String {
        join(&self.namespace, &self.leaf, sep)
    }
}

/// One `use` row of a use-map language: the class it imports, the span of
/// its name token, the alias it is bound under (`use A\B as C`), and the
/// region it binds in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseRow {
    pub span: Span,
    pub name: QualifiedName,
    pub alias: Option<String>,
    /// Where the row binds, minted by the extractor from the scope and
    /// namespace section it was written in. A language whose imports bind
    /// only from the import onward narrows this span's start; consumers
    /// only ever ask `binds_at`.
    pub reach: Span,
}

impl UseRow {
    pub fn binds_at(&self, site: Point) -> bool {
        contains_point(&self.reach, site)
    }
}

/// The classes a file declares, asked by identity — what the class-body
/// step of a resolution reads.
pub trait DeclaredClasses {
    fn declares_class(&self, identity: &str) -> bool;
}

impl DeclaredClasses for Vec<super::Symbol> {
    fn declares_class(&self, identity: &str) -> bool {
        self.iter().any(|s| matches!(s.kind, super::SymKind::Package | super::SymKind::Class) && s.name == identity)
    }
}

/// The class body a site is written directly in (`SectionKind::ClassBody`)
/// and the file's classes: a bare head the class declares names its nested
/// class before anything else binds it.
#[derive(Clone, Copy)]
pub struct ClassBodyStep<'a> {
    pub class: &'a str,
    pub classes: &'a dyn DeclaredClasses,
}

impl std::fmt::Debug for ClassBodyStep<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ClassBodyStep").field(&self.class).finish()
    }
}

/// One file's name-resolution context for class spellings, as seen from
/// one site.
#[derive(Debug, Clone, Copy)]
pub struct UseMap<'a> {
    /// The file's `use` rows (`PackFacts::use_rows`).
    pub rows: &'a [UseRow],
    /// The class body the site is written directly in, consulted first.
    pub class_body: Option<ClassBodyStep<'a>>,
    /// The namespace in force at the site, past any class body.
    pub own_namespace: Option<&'a str>,
    /// The namespace separator the spellings use (`PackFacts::names`).
    pub sep: &'a str,
    /// Where the spelling being resolved is written; only rows that reach
    /// it bind. `None` is the whole-file view the per-origin visibility
    /// pins fold, which has no single site.
    pub site: Option<Point>,
}

impl<'a> UseMap<'a> {
    /// The map a spelling written at `site` resolves through: the rows
    /// that reach it, the section in force there — a class body the site is
    /// written directly in, then the section around it. `None` for a
    /// language whose spellings are identities as written.
    pub fn at(
        rows: &'a [UseRow],
        sections: &'a [NamespaceRange],
        names: &'a NameSpellings,
        site: Point,
        classes: &'a dyn DeclaredClasses,
    ) -> Option<Self> {
        let sep = names.use_map_sep()?;
        let class_body = namespace_range_at(sections, site)
            .filter(|s| s.kind == SectionKind::ClassBody)
            .map(|s| ClassBodyStep { class: s.namespace.as_str(), classes });
        Some(UseMap {
            rows,
            class_body,
            own_namespace: namespace_range_around(sections, site)
                .map(|s| s.namespace.as_str())
                .filter(|ns| !ns.is_empty()),
            sep,
            site: Some(site),
        })
    }

    /// The class `written` names in this file. An absolute spelling
    /// (leading separator) names itself; otherwise the HEAD segment
    /// resolves — an alias to what it aliases, an imported leaf to its row,
    /// anything else into the file's own namespace (a file with no
    /// namespace resolves it globally) — and the rest of the spelling hangs
    /// off that. `Foo` under `namespace App` is `App\Foo`; `P\Promise`
    /// under `use GuzzleHttp\Promise as P` is `GuzzleHttp\Promise\Promise`;
    /// `Psr7\Utils` under `use GuzzleHttp\Psr7` is `GuzzleHttp\Psr7\Utils`.
    ///
    /// The answer is the class the FILE claims; whether a class of that
    /// name exists is the index's question, never this one's.
    pub fn resolve(&self, written: &str) -> QualifiedName {
        match written.rsplit_once(self.sep) {
            Some((prefix, leaf)) => self.resolve_in(prefix, leaf),
            None => self.bind(written),
        }
    }

    /// `leaf` written under the qualifier `prefix`, both as written:
    /// absolute when `prefix` opens with the separator, the global
    /// namespace when it is empty (`\Exception`), otherwise its head
    /// segment resolves like a bare spelling.
    pub fn resolve_in(&self, prefix: &str, leaf: &str) -> QualifiedName {
        let sep = self.sep;
        let namespace = match prefix.strip_prefix(sep) {
            Some(abs) => abs.to_string(),
            None if prefix.is_empty() => String::new(),
            None => match prefix.split_once(sep) {
                Some((head, rest)) => join(&self.bind(head).identity(sep), rest, sep),
                None => self.bind(prefix).identity(sep),
            },
        };
        QualifiedName { namespace, leaf: leaf.to_string() }
    }

    /// The class a single bare segment names: a class the enclosing class
    /// body declares first (`Inner` in `class Outer:`'s body is
    /// `Outer.Inner`), then an alias, then an unaliased row's leaf — a row
    /// bound under an alias binds the alias, never its leaf (`use A\Event
    /// as ScriptEvent` leaves `Event` to the file's own namespace) — then
    /// the file's own namespace.
    fn bind(&self, head: &str) -> QualifiedName {
        if let Some(body) = self.class_body {
            let nested = QualifiedName::new(body.class, head);
            if body.classes.declares_class(&nested.identity(self.sep)) {
                return nested;
            }
        }
        let rows = || self.rows.iter().filter(|r| self.site.is_none_or(|p| r.binds_at(p)));
        rows()
            .find(|r| r.alias.as_deref() == Some(head))
            .or_else(|| rows().find(|r| r.alias.is_none() && r.name.leaf == head))
            .map(|r| r.name.clone())
            .unwrap_or_else(|| QualifiedName::new(self.own_namespace.unwrap_or(""), head))
    }
}

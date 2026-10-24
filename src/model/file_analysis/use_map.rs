//! The use-map resolver: a file's `use` rows and its own namespace, folded
//! into ONE function from a class spelling as written to the class's
//! identity (its fully-qualified name). Every consumer of "what does this
//! spelling mean here" — the use-map visibility pins, the extraction pass
//! that mints class identities, hover's rendering — calls this rather than
//! re-deriving the head/alias/row/own-namespace ladder, so the rules cannot
//! drift between them.
//!
//! Pure `&str` over the pack lanes: no tree, no index. The separator is
//! the pack's declared one (`PackFacts::names`); a language whose
//! spellings are already identities (C's flat linkage) never builds one.
//! Rows and answers are (namespace, leaf) parts: the only strings split
//! here are written spellings, never an identity this code joined.

use super::Span;
use crate::model::conventions::join_qualified as join;
use serde::{Deserialize, Serialize};

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
/// its name token, and the alias it is bound under (`use A\B as C`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseRow {
    pub span: Span,
    pub name: QualifiedName,
    pub alias: Option<String>,
}

/// One file's name-resolution context for class spellings.
#[derive(Debug, Clone, Copy)]
pub struct UseMap<'a> {
    /// The file's `use` rows (`PackFacts::use_rows`).
    pub rows: &'a [UseRow],
    /// The namespace the file declares, when it declares exactly one.
    pub own_namespace: Option<&'a str>,
    /// The namespace separator the spellings use (`PackFacts::names`).
    pub sep: &'a str,
}

impl<'a> UseMap<'a> {
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

    /// The class a single bare segment names: an alias first, then an
    /// unaliased row's leaf — a row bound under an alias binds the alias,
    /// never its leaf (`use A\Event as ScriptEvent` leaves `Event` to the
    /// file's own namespace) — then the file's own namespace.
    fn bind(&self, head: &str) -> QualifiedName {
        self.rows
            .iter()
            .find(|r| r.alias.as_deref() == Some(head))
            .or_else(|| self.rows.iter().find(|r| r.alias.is_none() && r.name.leaf == head))
            .map(|r| r.name.clone())
            .unwrap_or_else(|| QualifiedName::new(self.own_namespace.unwrap_or(""), head))
    }
}

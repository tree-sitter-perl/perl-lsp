//! The use-map resolver: a file's `use` rows, its aliases and its own
//! namespace, folded into ONE function from a class spelling as written
//! to the class's identity (its fully-qualified name). Every consumer of
//! "what does this spelling mean here" — the use-map visibility pins,
//! the extraction pass that mints class identities, hover's rendering —
//! calls this rather than re-deriving the head/alias/row/own-namespace
//! ladder, so the rules cannot drift between them.
//!
//! Pure `&str` over the pack lanes: no tree, no index. The separator is
//! the pack's declared one (`PackFacts::namespace_sep`); a language whose
//! spellings are already identities (C's flat linkage) never builds one.

use super::Span;

/// One file's name-resolution context for class spellings.
#[derive(Debug, Clone, Copy)]
pub struct UseMap<'a> {
    /// The file's `use` rows as written (`PackFacts::include_directives`):
    /// a row's last segment is the leaf it binds, the whole row the
    /// identity.
    pub rows: &'a [(Span, String)],
    /// `use A\B as C` rows as (alias, namespace, real leaf)
    /// (`PackFacts::use_aliases`).
    pub aliases: &'a [(String, String, String)],
    /// The namespace the file declares, when it declares exactly one.
    pub own_namespace: Option<&'a str>,
    /// The namespace separator the spellings use (`PackFacts::namespace_sep`).
    pub sep: &'a str,
}

impl<'a> UseMap<'a> {
    /// The identity `written` names in this file. An absolute spelling
    /// (leading separator) is its own identity; otherwise the HEAD
    /// segment resolves — an alias to what it aliases, an imported leaf to
    /// its row, anything else into the file's own namespace (a file with
    /// no namespace resolves it globally) — and the rest of the spelling
    /// hangs off that. `Foo` under `namespace App` is `App\Foo`;
    /// `P\Promise` under `use GuzzleHttp\Promise as P` is
    /// `GuzzleHttp\Promise\Promise`; `Psr7\Utils` under `use
    /// GuzzleHttp\Psr7` is `GuzzleHttp\Psr7\Utils`.
    ///
    /// The answer is the identity the FILE claims; whether a class of that
    /// name exists is the index's question, never this one's.
    pub fn resolve(&self, written: &str) -> String {
        let sep = self.sep;
        if let Some(abs) = written.strip_prefix(sep) {
            return abs.to_string();
        }
        let (head, rest) = match written.split_once(sep) {
            Some((h, r)) => (h, Some(r)),
            None => (written, None),
        };
        let base = self
            .aliases
            .iter()
            .find(|(alias, _, _)| alias == head)
            .map(|(_, ns, leaf)| join(ns, leaf, sep))
            .or_else(|| {
                self.rows.iter().find_map(|(_, raw)| {
                    let t = raw.trim_start_matches(sep);
                    if t.rsplit(sep).next() != Some(head) {
                        return None;
                    }
                    // a row bound under an alias binds the alias, never
                    // its leaf (`use A\Event as ScriptEvent` leaves `Event`
                    // to the file's own namespace)
                    let aliased = self
                        .aliases
                        .iter()
                        .any(|(_, ns, leaf)| join(ns, leaf, sep) == t);
                    (!aliased).then(|| t.to_string())
                })
            })
            .unwrap_or_else(|| match self.own_namespace {
                Some(own) => join(own, head, sep),
                None => head.to_string(),
            });
        match rest {
            Some(r) => join(&base, r, sep),
            None => base,
        }
    }

    /// `resolve`, split into (namespace, leaf) — the shape the visibility
    /// pins are keyed by. The global namespace is the empty string.
    pub fn resolve_split(&self, written: &str) -> (String, String) {
        let fqn = self.resolve(written);
        match fqn.rsplit_once(self.sep) {
            Some((ns, leaf)) => (ns.to_string(), leaf.to_string()),
            None => (String::new(), fqn),
        }
    }
}

/// `a sep b`, without a dangling separator when `a` is the global
/// namespace.
fn join(a: &str, b: &str, sep: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}{sep}{b}")
    }
}

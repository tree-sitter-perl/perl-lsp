//! The use-map resolver: a file's `use` rows, its aliases and its own
//! namespace, folded into ONE function from a class spelling as written
//! to the class's identity (its fully-qualified name). Every consumer of
//! "what does this spelling mean here" — the use-map visibility pins,
//! the extraction pass that mints class identities, hover's rendering —
//! calls this rather than re-deriving the head/alias/row/own-namespace
//! ladder, so the rules cannot drift between them.
//!
//! Pure `&str` over the pack lanes: no tree, no index. The separator is
//! the caller's (`\` for the namespace-visible packs); a language whose
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
    /// The namespace separator the spellings use.
    pub sep: char,
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
fn join(a: &str, b: &str, sep: char) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}{sep}{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span { start: tree_sitter::Point::new(0, 0), end: tree_sitter::Point::new(0, 0) }
    }

    fn map<'a>(
        rows: &'a [(Span, String)],
        aliases: &'a [(String, String, String)],
        own: Option<&'a str>,
    ) -> UseMap<'a> {
        UseMap { rows, aliases, own_namespace: own, sep: '\\' }
    }

    #[test]
    fn absolute_spelling_is_its_own_identity() {
        let m = map(&[], &[], Some("App"));
        assert_eq!(m.resolve("\\Vendor\\Thing"), "Vendor\\Thing");
        assert_eq!(m.resolve("\\Exception"), "Exception");
    }

    #[test]
    fn bare_leaf_resolves_in_own_namespace_else_globally() {
        let m = map(&[], &[], Some("App\\Models"));
        assert_eq!(m.resolve("User"), "App\\Models\\User");
        let g = map(&[], &[], None);
        assert_eq!(g.resolve("User"), "User");
    }

    #[test]
    fn use_row_binds_its_leaf_and_carries_a_tail() {
        let rows = vec![(span(), "GuzzleHttp\\Psr7".to_string()), (span(), "\\Exception".to_string())];
        let m = map(&rows, &[], Some("App"));
        assert_eq!(m.resolve("Psr7"), "GuzzleHttp\\Psr7");
        assert_eq!(m.resolve("Psr7\\Utils"), "GuzzleHttp\\Psr7\\Utils");
        assert_eq!(m.resolve("Exception"), "Exception");
    }

    #[test]
    fn alias_wins_over_row_and_own_namespace() {
        let rows = vec![(span(), "GuzzleHttp\\Promise".to_string())];
        let aliases = vec![("P".to_string(), "GuzzleHttp".to_string(), "Promise".to_string())];
        let m = map(&rows, &aliases, Some("App"));
        assert_eq!(m.resolve("P"), "GuzzleHttp\\Promise");
        assert_eq!(m.resolve("P\\Promise"), "GuzzleHttp\\Promise\\Promise");
        // the real leaf of an aliased import is NOT bound by the alias row:
        // `Promise` is whatever else names it, i.e. the file's own namespace
        assert_eq!(m.resolve("Promise"), "App\\Promise");
    }

    #[test]
    fn an_aliased_row_binds_its_alias_never_its_leaf() {
        // the row is in `rows` too (every import row is), yet `Event`
        // means the file's own class, not the aliased import
        let rows = vec![(span(), "B\\Event".to_string())];
        let aliases = vec![("ScriptEvent".to_string(), "B".to_string(), "Event".to_string())];
        let m = map(&rows, &aliases, Some("A"));
        assert_eq!(m.resolve("ScriptEvent"), "B\\Event");
        assert_eq!(m.resolve("Event"), "A\\Event");
    }

    #[test]
    fn aliased_leaf_alone_falls_to_own_namespace() {
        let aliases =
            vec![("BaseCollection".to_string(), "Support".to_string(), "Collection".to_string())];
        let m = map(&[], &aliases, Some("App"));
        assert_eq!(m.resolve("BaseCollection"), "Support\\Collection");
        assert_eq!(m.resolve("Collection"), "App\\Collection");
    }

    #[test]
    fn split_keeps_the_global_namespace_empty() {
        let m = map(&[], &[], None);
        assert_eq!(m.resolve_split("Exception"), ("".to_string(), "Exception".to_string()));
        let n = map(&[], &[], Some("A\\B"));
        assert_eq!(n.resolve_split("C"), ("A\\B".to_string(), "C".to_string()));
    }
}

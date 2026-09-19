//! php's text→structure half: the declared-type and phpdoc type
//! spellings, and the class surface the runtime provides.
//!
//! Parsing SOURCE text is what a pack fn is for (CLAUDE.md #13): these
//! read what an author wrote in a comment or a type position, never a
//! string this codebase rendered.

use crate::model::file_analysis::InferredType;

// Live only under `feature = "php"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
/// php's type-spelling predicate — declared syntax types AND phpdoc rows
/// both parse through here. A named fn (not a closure) because the
/// sequence spellings recurse on their element.
pub(super) fn php_annot_type(text: &str) -> Option<InferredType> {
    use InferredType::*;
    let t = text.trim().trim_start_matches('?');
    if t.contains('|') {
        // A union of two or more non-null arms (`WP_Term|WP_Error`,
        // `int|string`) is KNOWN untypable: the author said what the value
        // is and this lattice cannot hold it. `Unknown` rides every chase
        // (a call, a copy, a return arm) instead of letting whatever else
        // resolved elect one arm; `A|null` is one arm, an optional.
        let mut arms = phpdoc_split_top_level(t, '|');
        arms.retain(|a| !a.eq_ignore_ascii_case("null") && !a.is_empty());
        match arms.as_slice() {
            // `A|null`: the one arm, re-read without the null.
            [one] if *one != t => return php_annot_type(one),
            // The `|` sits INSIDE a generic (`array<int|string>`): not a
            // union at the top, and re-reading the same text would recurse
            // forever — the element spellings below decide the shape.
            [_] => {}
            [] => return None,
            _ => return Some(Unknown),
        }
    }
    if t.contains('&') {
        return None;
    }
    // Sequence spellings — `list<X>` / `array<X>` / `iterable<X>`,
    // `array<K, V>` (the element is V), `Type[]`. A homogeneous sequence
    // carries its element as a one-slot `Sequence` (`element_at(0)` and
    // the foreach `Element` peel both read it). The element recurses
    // through this same predicate, so `\App\User[]` leafs like any class
    // spelling. Without these arms the whole spelling fell to the
    // ClassName fallback and minted a bogus class `list<X>`.
    if let Some(inner) = t.strip_suffix("[]") {
        return php_annot_type(inner).map(|e| match e {
            Unknown => Unknown,
            e => Sequence(vec![e]),
        });
    }
    // Array shapes. Positional (`array{A, B}` / `list{A, B}` / `array{0: A,
    // 1: B}`) → a per-slot `Sequence` tuple, the destructuring source;
    // string-keyed (`array{name: string}` / `object{jobs: array}`) → the
    // structural `HashWithKeys` shape. All-or-nothing on the slots — a
    // holey tuple mis-projects (docs/adr/destructuring.md).
    for prefix in ["array{", "list{", "object{", "non-empty-array{", "non-empty-list{"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            let inner = rest.strip_suffix('}')?;
            let parts = phpdoc_split_top_level(inner, ',');
            let mut slots: Vec<InferredType> = Vec::new();
            let mut keyed: Vec<(std::string::String, Option<Box<InferredType>>)> = Vec::new();
            for part in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
                // `key: T` — a key is an identifier/int token before a `:`
                // that is NOT part of a nested generic.
                let split = part
                    .find(':')
                    .filter(|&i| !part[..i].contains(['<', '{', '(']))
                    .map(|i| (part[..i].trim().trim_end_matches('?'), part[i + 1..].trim()));
                match split {
                    Some((k, ty)) if k.parse::<usize>().is_err() => {
                        let v = php_annot_type(ty);
                        if matches!(v, Some(Unknown)) {
                            return Some(Unknown);
                        }
                        keyed.push((k.to_string(), v.map(Box::new)));
                    }
                    Some((_, ty)) => match php_annot_type(ty)? {
                        Unknown => return Some(Unknown),
                        v => slots.push(v),
                    },
                    None => match php_annot_type(part)? {
                        Unknown => return Some(Unknown),
                        v => slots.push(v),
                    },
                }
            }
            if !keyed.is_empty() {
                return Some(HashWithKeys {
                    keys: crate::model::file_analysis::SharedKeys::new(keyed),
                    open: false,
                });
            }
            return (!slots.is_empty()).then_some(Sequence(slots));
        }
    }
    for prefix in [
        "list<",
        "array<",
        "iterable<",
        "non-empty-list<",
        "non-empty-array<",
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            let inner = rest.strip_suffix('>')?;
            // `array<K, V>`: the element is the LAST top-level argument.
            let args = phpdoc_split_top_level(inner, ',');
            let elem_text = args.last()?.trim();
            // `array<mixed>` / `array<string, mixed>` is a container of
            // unknowns — the bare `array` keyword's shape, so the OUTER
            // generic of `array<array<mixed>>` still types its element as
            // an array instead of the whole annotation collapsing.
            let Some(elem) = php_annot_type(elem_text) else {
                return (elem_text == "mixed").then_some(HashRef);
            };
            // A NON-int-keyed `array<K, V>` keeps its key axis: the value
            // rides as a two-argument parametric instance ([K, V] — the
            // positional convention `ParamOf`/`Element`/`Key` project), so
            // the pair-form foreach types BOTH bindings. Int-keyed (and
            // key-less) spellings are sequences — their keys ARE positions.
            if args.len() > 1 {
                let key_text = args[0].trim();
                if key_text != "int" {
                    let key = php_annot_type(key_text)?;
                    return Some(Parametric(
                        crate::model::file_analysis::ParametricType::Instance {
                            base: "array".to_string(),
                            args: vec![key, elem],
                        },
                    ));
                }
            }
            return Some(match elem {
                Unknown => Unknown,
                elem => Sequence(vec![elem]),
            });
        }
    }
    match t {
        "string" => Some(String),
        "int" | "float" => Some(Numeric),
        "bool" | "false" | "true" => Some(Bool),
        "array" | "iterable" => Some(HashRef),
        "void" | "null" | "mixed" | "never" | "object" | "callable" | "self"
        | "static" | "parent" => None,
        t => {
            // The spelling as written, qualifier and all: the extractor's
            // identity pass resolves it through the file's use-map
            // (`\App\Models\User` is absolute, `Op\Install` hangs off the
            // namespace); only the leaf is checked for class-name shape.
            let leaf = t.rsplit('\\').next().unwrap_or(t);
            (!leaf.is_empty()
                && leaf.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && !leaf.contains(['<', '>', '[', ']', '{', '}']))
            .then(|| ClassName(t.to_string()))
        }
    }
}


/// The ONE bracket alphabet of phpdoc type text (`<{(` / `>})`): every
/// top-level split and the token boundary step depth through here, so a
/// new bracket spelling is added once, never in lockstep across walkers.
fn phpdoc_depth_step(c: char, depth: &mut usize) {
    match c {
        '<' | '{' | '(' => *depth += 1,
        '>' | '}' | ')' => *depth = depth.saturating_sub(1),
        _ => {}
    }
}

/// Split phpdoc type text on `sep` at bracket depth 0 (a separator inside
/// generics / an array shape belongs to the enclosing part).
fn phpdoc_split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in s.char_indices() {
        if c == sep && depth == 0 {
            out.push(&s[start..i]);
            start = i + 1;
        } else {
            phpdoc_depth_step(c, &mut depth);
        }
    }
    out.push(&s[start..]);
    out
}


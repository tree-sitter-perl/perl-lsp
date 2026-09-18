//! php's text→structure half: the declared-type and phpdoc type
//! spellings, and the class surface the runtime provides.
//!
//! Parsing SOURCE text is what a pack fn is for (CLAUDE.md #13): these
//! read what an author wrote in a comment or a type position, never a
//! string this codebase rendered.

use crate::build::query_extract::DocFact;
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


/// phpdoc `@return` / `@param` / `@var` facts out of one `/** */` comment.
/// Only doc comments participate (a `//` or plain `/* */` never carries
/// the vocabulary); each tag line yields at most one fact.
pub(super) fn php_doc_types(text: &str, uses_method_tags: &[&str]) -> Vec<DocFact> {
    if !text.starts_with("/**") {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Summary paragraph: the prose before the first tag line, one line per
    // source line (a blank line keeps a paragraph break as a blank line).
    let mut desc: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim().trim_start_matches('/').trim_start_matches('*').trim_end_matches('/').trim_end_matches('*').trim();
        if l.starts_with('@') {
            break;
        }
        desc.push(l.to_string());
    }
    while desc.last().is_some_and(|l| l.is_empty()) {
        desc.pop();
    }
    while desc.first().is_some_and(|l| l.is_empty()) {
        desc.remove(0);
    }
    if desc.is_empty() {
        // `/** @var array Default request options */`: a property's only
        // prose is the tag's trailer — after the type and an optional
        // `$name`.
        for line in text.lines() {
            let l = line.trim().trim_start_matches('/').trim_start_matches('*').trim_end_matches('/').trim_end_matches('*').trim();
            if let Some(rest) = l.strip_prefix("@var ") {
                // The trailer starts where the TYPE token ends, which a
                // whitespace split gets wrong: `array<int, string>` has a
                // space inside its brackets, so the split published
                // `string>` as the property's hover text.
                let rest = rest.trim_start();
                let mut words: Vec<&str> =
                    rest[phpdoc_type_token_end(rest)..].split_whitespace().collect();
                if words.first().is_some_and(|w| w.starts_with('$')) {
                    words.remove(0);
                }
                if !words.is_empty() {
                    desc.push(words.join(" "));
                }
                break;
            }
        }
    }
    if !desc.is_empty() {
        out.push(DocFact::Description(desc.join("\n")));
    }
    for (lineno, line) in text.lines().enumerate() {
        // Normalize both spellings: a `* @param` continuation line and the
        // single-line `/** @return X */` form.
        let l = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('*')
            .trim_end_matches('/')
            .trim_end_matches('*')
            .trim();
        if let Some(rest) = l
            .strip_prefix("@return ")
            .or_else(|| l.strip_prefix("@phpstan-return "))
            .or_else(|| l.strip_prefix("@psalm-return "))
        {
            // `Base<static>` / `Base<self>` / `Base<$this>`: the value is
            // an instance of Base parametrized by the RECEIVER — a
            // deferred shape, not a strippable generic.
            let head = rest.split_whitespace().next().unwrap_or("");
            let recv_inst = head
                .strip_suffix('>')
                .and_then(|h| h.split_once('<'))
                .filter(|(_, arg)| matches!(*arg, "static" | "self" | "$this"))
                // As written: the extractor's identity pass resolves the
                // base (an FQ `\Illuminate\...\Builder<static>` and a bare
                // `Builder<static>` land on one identity).
                .and_then(|(base, _)| phpdoc_type(base));
            if let Some(base) = recv_inst {
                out.push(DocFact::ReturnRecvInstance { base });
            } else if let Some(t) = phpdoc_type(rest) {
                out.push(DocFact::Return(t));
            }
        } else if let Some(rest) = l.strip_prefix("@template ")
            .or_else(|| l.strip_prefix("@template-covariant "))
        {
            if let Some(name) = rest.split_whitespace().next() {
                if !name.is_empty()
                    && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    out.push(DocFact::Template { name: name.to_string(), line: lineno });
                }
            }
        } else if let Some(rest) = l
            .strip_prefix("@param ")
            .or_else(|| l.strip_prefix("@phpstan-param "))
            .or_else(|| l.strip_prefix("@psalm-param "))
            .or_else(|| l.strip_prefix("@global "))
        {
            // `@global wpdb $wpdb` types the `global $wpdb;` binding the
            // def below declares — same (type, $name) shape as @param,
            // same join (the first var of that name declared in the def).
            // `@param string $name description`; the typeless `@param $x`
            // form and variadics (`...$args`) carry nothing typeable.
            let rest = rest.trim_start();
            let ty_end = phpdoc_type_token_end(rest);
            let (ty, tail) = rest.split_at(ty_end);
            if let Some(name) = tail.split_whitespace().next() {
                if name.starts_with('$') {
                    if let Some(t) = phpdoc_type(ty) {
                        out.push(DocFact::Param {
                            name: name.trim_end_matches(',').to_string(),
                            ty: t,
                        });
                    }
                }
            }
        } else if let Some(rest) = l
            .strip_prefix("@var ")
            .or_else(|| l.strip_prefix("@phpstan-var "))
            .or_else(|| l.strip_prefix("@psalm-var "))
        {
            if let Some(t) = phpdoc_type(rest) {
                let rest = rest.trim_start();
                let name = rest[phpdoc_type_token_end(rest)..]
                    .split_whitespace()
                    .next()
                    .filter(|n| n.starts_with('$'))
                    .map(|n| n.to_string());
                out.push(DocFact::Var { ty: t, name });
            }
        } else if let Some((tag, rest)) = l.strip_prefix('@').and_then(|body| {
            uses_method_tags.iter().find_map(|t| {
                body.strip_prefix(*t).and_then(|r| r.strip_prefix(' ')).map(|r| (*t, r))
            })
        }) {
            if let Some(name) = rest.split_whitespace().next() {
                if name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) {
                    // Byte column of the named METHOD in the RAW line, so
                    // the ref spans the token (rename rewrites it in place;
                    // anchored past the tag so a name that happens to be a
                    // substring of the tag can't mis-anchor).
                    let col = line
                        .find(tag)
                        .and_then(|t| line[t..].find(name).map(|o| t + o))
                        .unwrap_or(0);
                    out.push(DocFact::UsesMethod {
                        name: name.to_string(),
                        line: lineno,
                        col,
                    });
                }
            }
        } else if l == "@deprecated" || l.starts_with("@deprecated ") {
            let text = l["@deprecated".len()..].trim();
            out.push(DocFact::Deprecated((!text.is_empty()).then(|| text.to_string())));
        } else if let Some(rest) = l.strip_prefix("@method ") {
            // `@method [static] T name(args)`; the type is optional
            // (`@method foo()`), the name token is whatever carries the
            // `(`. `static` is dispatch surface, not a return spelling.
            let rest = rest.strip_prefix("static ").unwrap_or(rest);
            let mut it = rest.split_whitespace();
            if let Some(t0) = it.next() {
                let (ret, name_tok) = if t0.contains('(') {
                    (None, t0)
                } else {
                    match it.next() {
                        Some(t1) => (Some(t0), t1),
                        None => (None, t0),
                    }
                };
                let name = name_tok.split('(').next().unwrap_or("");
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    // Byte column of the method NAME in the RAW line —
                    // the synthesized symbol spans the token, so the
                    // cursor lands on it and rename rewrites it in place.
                    // Anchored on `name(` (the name always carries the
                    // paren) so a name echoed in the return type can't
                    // mis-anchor.
                    let col = line
                        .find(&format!("{name}("))
                        .or_else(|| line.find(name))
                        .unwrap_or(0);
                    out.push(DocFact::Method {
                        name: name.to_string(),
                        ret: ret.and_then(phpdoc_type),
                        line: lineno,
                        col,
                    });
                }
            }
        }
    }
    out
}

/// Normalize one phpdoc type expression to a spelling `annot_type` speaks:
/// generics stripped (`Collection<int,User>` → `Collection`), `User[]` is
/// an array, the `null` arm of a union dropped (`?T` too), a REAL union
/// (`string|false`) rejected — a two-armed claim is not a type answer.
/// Where the leading type token of a phpdoc tail ends: the first
/// whitespace OUTSIDE angle brackets — `array<string, User> $map` keeps
/// its generic arguments (a plain whitespace split truncated it to
/// `array<string,`). Callers slice `[..end]` for the type and
/// `[end..]` for what follows (the `$name` of a @param).
fn phpdoc_type_token_end(s: &str) -> usize {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && depth == 0 {
            return i;
        }
        phpdoc_depth_step(c, &mut depth);
    }
    s.len()
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

fn phpdoc_type(raw: &str) -> Option<String> {
    let raw = raw.trim_start();
    let raw = raw[..phpdoc_type_token_end(raw)].trim_start_matches('?');
    if raw.is_empty() {
        return None;
    }
    // Union split at TOP LEVEL only — a `|` inside generics is part of one
    // arm (`static<int, static<int, TValue|TZipValue>>` is a single type;
    // the naive split saw three and dropped laravel's whole fluent surface).
    let mut arms = phpdoc_split_top_level(raw, '|');
    arms.retain(|a| !a.eq_ignore_ascii_case("null") && !a.is_empty());
    // A union survives WHOLE: `annot_type` answers `Unknown` for it, the
    // fact every doc row (return, param, var, @method) carries the same way.
    if arms.len() > 1 {
        return Some(arms.join("|"));
    }
    let [one] = arms.as_slice() else { return None };
    // Sequence spellings survive WHOLE — `annot_type` parses the element
    // (`list<X>` / `array<K,V>` / `iterable<X>` / `X[]` → a one-slot
    // `Sequence`); every other generic still strips to its base class
    // (`Collection<int,User>` → `Collection`).
    if one.ends_with("[]")
        || (one.ends_with('>')
            && ["list<", "array<", "iterable<", "non-empty-list<", "non-empty-array<"]
                .iter()
                .any(|p| one.starts_with(p)))
        // array-shape spellings (`array{A, B}` / `object{k: T}`) survive
        // whole too — `annot_type` parses the tuple / keyed shape.
        || (one.ends_with('}')
            && ["array{", "list{", "object{", "non-empty-array{", "non-empty-list{"]
                .iter()
                .any(|p| one.starts_with(p)))
    {
        return Some(one.to_string());
    }
    let base = one.split('<').next().unwrap_or(one);
    (!base.is_empty()).then(|| base.to_string())
}


/// The classes, interfaces and attributes php provides in the global
/// namespace (core + SPL + the bundled extensions a stock build carries).
pub(super) const PHP_BUILTIN_TYPES: &[&str] = &[
    "AllowDynamicProperties", "AppendIterator", "ArgumentCountError", "ArithmeticError", "ArrayAccess",
    "ArrayIterator", "ArrayObject", "AssertionError", "Attribute", "BackedEnum", "BadFunctionCallException",
    "BadMethodCallException", "CachingIterator", "CallbackFilterIterator", "Closure", "Collator", "Countable",
    "CurlHandle", "CurlMultiHandle", "CurlShareHandle", "DOMAttr", "DOMDocument", "DOMElement", "DOMNode",
    "DOMNodeList", "DOMText", "DOMXPath", "DateInterval", "DatePeriod", "DateTime", "DateTimeImmutable",
    "DateTimeInterface", "DateTimeZone", "Deprecated", "Directory", "DirectoryIterator", "DivisionByZeroError",
    "DomainException", "EmptyIterator", "Error", "ErrorException", "Exception", "Fiber", "FilesystemIterator",
    "FilterIterator", "GMP", "GdImage", "Generator", "GlobIterator", "HashContext", "InfiniteIterator",
    "IntlCalendar", "IntlChar", "IntlDateFormatter", "IntlException", "IntlTimeZone", "InvalidArgumentException",
    "Iterator", "IteratorAggregate", "IteratorIterator", "JsonException", "JsonSerializable", "LengthException",
    "LimitIterator", "Locale", "LogicException", "Memcached", "MessageFormatter", "MultipleIterator",
    "NoRewindIterator", "Normalizer", "NumberFormatter", "OpenSSLAsymmetricKey", "OpenSSLCertificate",
    "OuterIterator", "OutOfBoundsException", "OutOfRangeException", "OverflowException", "Override",
    "PDO", "PDOException", "PDOStatement", "ParentIterator", "ParseError", "Phar", "PharData", "RangeException",
    "RecursiveArrayIterator", "RecursiveCallbackFilterIterator", "RecursiveDirectoryIterator",
    "RecursiveIterator", "RecursiveIteratorIterator", "Redis", "RedisException", "ReflectionAttribute",
    "ReflectionClass", "ReflectionClassConstant", "ReflectionEnum", "ReflectionException", "ReflectionFunction",
    "ReflectionMethod", "ReflectionNamedType", "ReflectionObject", "ReflectionParameter", "ReflectionProperty",
    "ReflectionType", "ReflectionUnionType", "RegexIterator", "ResourceBundle", "ReturnTypeWillChange",
    "RuntimeException", "SeekableIterator", "SensitiveParameter", "Serializable", "SessionHandler",
    "SessionHandlerInterface", "SimpleXMLElement", "SoapClient", "SoapFault", "SoapHeader", "SoapServer",
    "SoapVar", "Socket", "SplDoublyLinkedList", "SplFileInfo", "SplFileObject", "SplFixedArray", "SplHeap",
    "SplMaxHeap", "SplMinHeap", "SplObjectStorage", "SplObserver", "SplPriorityQueue", "SplQueue", "SplStack",
    "SplSubject", "SplTempFileObject", "Stringable", "Throwable", "Transliterator", "Traversable", "TypeError",
    "UConverter", "UnderflowException", "UnexpectedValueException", "UnhandledMatchError", "UnitEnum",
    "ValueError", "WeakMap", "WeakReference", "XMLReader", "XMLWriter", "ZipArchive", "finfo", "mysqli",
    "mysqli_result", "mysqli_stmt", "stdClass",
];


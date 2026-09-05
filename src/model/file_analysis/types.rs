//! Type inference vocabulary: `TypeConstraint`, `InferredType`,
//! `ParametricType` + template matching, `TypeProvenance`, return-arm joins.

use super::*;

// ---- Type constraints ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeConstraint {
    pub variable: String,
    pub scope: ScopeId,
    pub constraint_span: Span,
    pub inferred_type: InferredType,
}

/// Shared key list for [`InferredType::HashWithKeys`]. Clone is a refcount.
///
/// The by-value spelling cost Znuny 33s and 7.3GB: rule #10's rich-type
/// returns are the right contract, but every consumer that queried a
/// variable typed by a 4.8k-key generated literal took delivery of the
/// whole key list — N sites x O(S), in three separate consumers. Sharing
/// deletes the product; what remains is O(S) once, linear in the input.
///
/// `PartialEq` takes the pointer fast path first: all sites of one literal
/// share one allocation, so the common comparison is O(1).
///
/// Serializes EXACTLY as the inner `Vec` (delegating impls, not a newtype
/// wrapper), so cache blobs are byte-identical and `EXTRACT_VERSION` does
/// not move.
#[derive(Debug, Clone, Eq)]
pub struct SharedKeys(std::sync::Arc<Vec<(String, Option<Box<InferredType>>)>>);

impl SharedKeys {
    pub fn new(v: Vec<(String, Option<Box<InferredType>>)>) -> Self {
        SharedKeys(std::sync::Arc::new(v))
    }

    /// Copy-on-write access for the one mutation path (shape extension).
    /// Clones the key list only when the allocation is shared.
    pub fn to_mut(&mut self) -> &mut Vec<(String, Option<Box<InferredType>>)> {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl std::ops::Deref for SharedKeys {
    type Target = [(String, Option<Box<InferredType>>)];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for SharedKeys {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl Serialize for SharedKeys {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for SharedKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::deserialize(d).map(|v| SharedKeys(std::sync::Arc::new(v)))
    }
}

impl FromIterator<(String, Option<Box<InferredType>>)> for SharedKeys {
    fn from_iter<I: IntoIterator<Item = (String, Option<Box<InferredType>>)>>(i: I) -> Self {
        SharedKeys::new(i.into_iter().collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferredType {
    /// `$p = Point->new(...)` — variable is an instance of ClassName.
    ClassName(String),
    /// `my ($self) = @_` in `package Foo` — first param is the class.
    FirstParam { package: String },
    /// `$x = {}` or `$x = { ... }` — unblessed hash reference.
    HashRef,
    /// `$x = []` or `$x = [ ... ]` — unblessed array reference.
    ArrayRef,
    /// `$x = sub { ... }` — code reference. `return_edge` is a
    /// witness-bag attachment whose type IS the callable's return
    /// when invoked. Two shapes populate it:
    ///
    ///   - Anonymous-sub literals (`sub { ... }`) →
    ///     `Expr(body_last_expr_span)`. The bag walks that span's
    ///     own witnesses at query time, after the body is built.
    ///   - Named-sub references (`\&foo`, `\&Foo::bar`) →
    ///     `PackageSymbol { package, name }`. Same attachment the
    ///     bag's existing edge-chase uses for method dispatch —
    ///     resolves in-file via the named-sub's Symbol witnesses
    ///     AND cross-file via `module_index` (the bag transparently
    ///     recurses into the cached module's bag).
    ///
    /// Survives variable rebinding because chain typing propagates
    /// the whole `InferredType` through `my $sub = ...` via the
    /// bag's TC machinery — so `helper(name => sub {...})` and
    /// `my $cb = \&foo; helper(name => $cb)` both reach the same
    /// attachment-driven resolution.
    ///
    /// `None` for opaque sources (params typed `CodeRef`, deref-
    /// shape narrowing, `Rep::Code` observations) where no body or
    /// named target is reachable from the syntax alone.
    CodeRef { return_edge: Option<crate::model::witnesses::WitnessAttachment> },
    /// `$x = qr/.../` — compiled regular expression.
    Regexp,
    /// Used in numeric context (`+`, `-`, `==`, etc.).
    Numeric,
    /// Used in string context (`.`, `eq`, `=~`, etc.).
    String,
    /// Parametric type — a sealed enum where each variant carries
    /// its own data shape (concrete flavors) or wraps an operand
    /// for type-level projections. Per-axis methods on
    /// `ParametricType` (`class_name`, `hash_key_class`,
    /// `method_arg_owner`) carry per-flavor policy. **Match
    /// invariant: never `_ => …`** — compiler exhaustiveness is
    /// the safety net for the future `Plugin` escape hatch variant.
    /// See `docs/adr/parametric-types.md`.
    Parametric(ParametricType),
    /// Positional container — `my @arr = (...)` or
    /// `push @arr, ...` contributions accumulated walk-side. The
    /// `Vec` stores per-index types; `element_at(i)` projects.
    /// Tuple shape only (no homogeneous/heterogeneous classification).
    ///
    /// Placed at the END of the enum so bincode-serialized cache
    /// blobs keep their existing variant indices stable. Inserting
    /// new variants in the middle would shift every subsequent
    /// variant's wire-format index and silently misread old blobs.
    Sequence(Vec<InferredType>),
    /// A Type::Tiny / Types::Standard constraint *object* —
    /// `InstanceOf['Foo']`, `ArrayRef[Int]`, … — carrying the type it
    /// constrains values to. The constraint is a value in its own right:
    /// method dispatch on it routes to `Type::Tiny` (deferred), NOT the
    /// inner type. Its one job here is projection: an `isa => <constraint>`
    /// gives its accessor the *constrained* (inner) type via
    /// `constrained_inner()`. A plugin's `type_constraint_inner` fold
    /// produces the inner; the core wraps it. See
    /// `docs/adr/type-constraints.md`. Kept at the END for
    /// bincode variant-index stability (bump `EXTRACT_VERSION`).
    TypeConstraintOf(Box<InferredType>),
    /// A Mojolicious route-builder value carrying the **accumulated
    /// route defaults** in force at this point in the builder chain.
    /// `base` is the class for method dispatch
    /// (`Mojolicious::Routes::Route`); `controller` / `stash` are the
    /// inherited `->to(...)` defaults a partial `->to('#action')`
    /// reads. This is the "brand on the value" from
    /// `docs/adr/route-branding.md` (option C, collapsed):
    /// the defaults ride the type through assignment / chaining /
    /// nesting via the witness bag for free, so there is no separate
    /// brand-id + side-table to keep cache-stable — the resolved
    /// defaults ARE the value, content-addressed. Inheritance is baked
    /// in: each route method that sets a default produces a NEW
    /// `BrandedRoute` that overlays its own keys onto the receiver's,
    /// so children never mutate parents and a sibling group with its
    /// own `->to('other#')` re-brands its descendants without leaking.
    /// Kept at the END for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    BrandedRoute {
        base: String,
        controller: Option<String>,
        stash: Vec<(String, String)>,
    },
    /// `{ host => 'x', port => 5432 }` — a hash literal with literal
    /// keys, each carrying its value's type when inferable (`None` =
    /// key present, value type unknown). `open` = a spread (`%$other`)
    /// or dynamic key makes the key set open-ended, so an unknown key
    /// is not a claimable miss. `->{key}` narrows via
    /// [`InferredType::key_value_type`]; nesting recurses naturally
    /// (the value's own `HashWithKeys` rides in the box). Kept at the
    /// END for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    HashWithKeys {
        keys: SharedKeys,
        open: bool,
    },
    /// `Optional(Box<T>)` — value-or-undef (Type::Tiny `Maybe[T]` /
    /// `Optional[T]`). Produced when an arm/branch fold sees `{T, undef}`:
    /// the join of a concrete arm with an undef arm. `defined $x` /
    /// `blessed $x` narrowing strips it back to `T`. NOT a class itself —
    /// `class_name()` returns `None` (an optional is not *definitely* an
    /// instance), so it cannot dispatch until narrowed. See
    /// `docs/adr/optional-types.md`. Kept at the END for bincode
    /// variant-index stability (bump `EXTRACT_VERSION`).
    Optional(Box<InferredType>),
    /// The bottom element — a value proven `undef`. Produced only by
    /// flow narrowing: the negative side of a `defined`/`blessed` guard
    /// (`if (defined $x) {} else { ... }`, `return if defined $x`). NOT a
    /// class (`class_name()` → `None`), so a method call on it stays
    /// unresolved (a value-known-undef can't dispatch). Never produced by
    /// the return-arm join (that signals undef via a source tag, not a
    /// type — `docs/adr/optional-types.md`). Kept at the END for
    /// bincode variant-index stability (bump `EXTRACT_VERSION`).
    Undef,
    /// A boolean-valued expression — a comparison (`$a == $b`, `$x eq $y`),
    /// a logical negation (`!$x`, `!!$x`, `not $x`), a truth-test builtin
    /// (`defined`, `exists`), a Moo/Moose `isa => 'Bool'` accessor, or a
    /// C++ `bool` declaration / `true`/`false` literal / relational-or-
    /// logical operator. NOT a class (`class_name()` → `None`).
    ///
    /// Perl has no distinct boolean value — truth is `1` / `''`, so Bool
    /// is a *sub-lattice of Numeric*: it prints and dispatches like a
    /// number, and a return-arm fold that sees `{Bool, Numeric}` joins to
    /// `Numeric` (see `resolve_return_type`) rather than degrading to
    /// Unknown. The variant lets the analyzer *say* "this is a truth
    /// value" where it knows one — hover, inlay hints, future
    /// boolean-context diagnostics — without lying that it's a general
    /// number. Kept at the END for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    Bool,
    /// KNOWN to be untypable — the value a reassignment whose source this
    /// tier cannot type leaves behind (`FlowEdge::reassigns`). Distinct from
    /// "no evidence": it flows through every chase like a type (a return
    /// arm that reads a reset variable makes the fold a disagreement, a
    /// `$y = $x` copy carries it on) and is projected to `None` at the
    /// registry's public boundary, so no consumer ever renders it. Two
    /// sources: `materialize` mints it for a reassignment edge that cannot
    /// resolve, and a documented or declared UNION (`A|B`) annotates it
    /// directly (`php_annot_type`). Never a walk-time inference. Kept at the
    /// END for bincode variant-index stability.
    Unknown,
}

/// Concrete parametric flavors + type-level operators. Each
/// concrete flavor carries the data its semantics need; operators
/// (`RowOf`) wrap a sub-`InferredType` and project via the
/// value-side accessors (`class_name` / `hash_key_class`), or in
/// symbol-declarative form via `ReturnExprReducer`'s `Operator(RowOf)`
/// arm.
///
/// **Match invariant: never `_ => …` arms.** Every consumer
/// explicitly handles every variant. The compiler enforces this
/// when the (deferred) `Plugin` escape hatch lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParametricType {
    /// DBIC `$schema->resultset('Foo')` shape. `base` is the
    /// resolved resultset class (default
    /// `DBIx::Class::ResultSet`, or a discovered custom resultset
    /// class — see `goto_def_offers_custom_resultset_method` red-
    /// pin). `row` is the row class (where `add_columns`
    /// synthesizes its column accessors). Two distinct fields
    /// because the value carries dual identity:
    ///   - method dispatch goes through `base`
    ///   - hash-key arg owner / direct hash-key access go through `row`
    ///
    /// Pinned by internal-shape tests so a refactor to a single-class
    /// encoding can't silently merge the two dimensions.
    ///
    /// The row-of projection (`find`/`first`/… → the row class) lives on
    /// the deferred side as `ReturnExpr::Operator(RowOf)`, which
    /// `eval_return_expr` projects eagerly to `ClassName(row)` — there is
    /// no value-side `RowOf` variant.
    ResultSet { base: String, row: String },

    /// A template/generic instance — `Box<Widget> b;` peeled from its
    /// declared-type spelling. `base` is the unqualified template name
    /// (`Box`) — the dispatch axis, so member gd / completion / refs
    /// resolve through the SAME `PackageSymbol`/ancestor machinery a
    /// plain class uses. `args` ride along un-consumed (each is a
    /// `ClassName(canonical spelling)` leaf or a nested `Instance`):
    /// they are the substitution witness instantiation-aware typing
    /// consumes; nothing here interprets them. `exact_spelling()`
    /// reconstructs the canonical full spelling (`Box<Widget>`) —
    /// presentation, and the per-spec dispatch key when a
    /// specialization class by that exact spelling exists.
    ///
    /// Kept AFTER `ResultSet` for bincode variant-index stability
    /// (bump `EXTRACT_VERSION`).
    Instance { base: String, args: Vec<InferredType> },
}

impl ParametricType {
    /// Class to consult for method dispatch on this value.
    /// `$rs->all` resolves against `class_name()`'s answer.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            ParametricType::ResultSet { base, .. } => Some(base.as_str()),
            ParametricType::Instance { base, .. } => Some(base.as_str()),
        }
    }

    /// Class to consult for direct `recv->{key}` hash-key access
    /// on this value. ResultSet returns `row` (the column-keyed
    /// class — used today only by the cleanup-pass HashKeyAccess
    /// owner-resolution paths; HRI shape isn't supported but the
    /// field is the right one when it lands).
    pub fn hash_key_class(&self) -> Option<&str> {
        match self {
            ParametricType::ResultSet { row, .. } => Some(row.as_str()),
            // No key/value duality on a template instance — hash-key
            // access (if it ever occurs) reads the same class methods do.
            ParametricType::Instance { base, .. } => Some(base.as_str()),
        }
    }

    /// Owner for hash-key args of `recv->method({KEY => ...})`.
    /// `Some(owner)` means "this flavor claims this method's args
    /// — emit the HashKeyAccess unconditionally with this owner;
    /// the type IS the gate." `None` means "this flavor doesn't
    /// claim, fall through to the strict-eq local-symbol path."
    ///
    /// ResultSet claims the row-keyed methods (search, search_rs,
    /// find, find_or_new, find_or_create, update_or_create, create,
    /// update, populate, new_result). Methods that take filters or no
    /// args (count, exists, delete, all without args) return None.
    pub fn method_arg_owner(&self, method: &str) -> Option<HashKeyOwner> {
        match self {
            ParametricType::ResultSet { row, .. } => match method {
                "search" | "search_rs" | "find" | "find_or_new" | "find_or_create"
                | "update_or_create" | "create" | "update" | "populate" | "new_result" => {
                    Some(HashKeyOwner::Bridged { class: row.clone() })
                }
                _ => None,
            },
            ParametricType::Instance { .. } => None,
        }
    }


    /// Symbol-declarative projection table — list of `(method_name,
    /// ReturnExpr)` pairs the flavor publishes on
    /// `PackageSymbol{base, method}` so consumers chasing through
    /// inheritance / coderef-edge / dynamic-method routes hit the
    /// projection without the call-site `parametric_resultset` witness
    /// firing. Used by `emit_parametric_return_expr_decls`
    /// after every `extract_resultset_parametric` hit — the `base`
    /// discovered there pins the class slot for the witness.
    ///
    /// `ReturnExpr::Operator(RowOf(Receiver))` evaluates at the
    /// reducer with `q.receiver = the call's invocant type`. For
    /// `\&MyRS::find; $cb->($rs, ...)`, the chain typer's coderef
    /// arm sees the target is `PackageSymbol{MyRS, find}`,
    /// inheritance walks to `PackageSymbol{DBIx::Class::ResultSet,
    /// find}`, finds the `Operator(RowOf, Receiver)` declaration,
    /// substitutes `q.receiver = $rs`'s `Parametric(ResultSet)`,
    /// evaluates `RowOf(ResultSet { row, .. }) → ClassName(row)`.
    pub fn return_method_declarations(
        &self,
    ) -> Vec<(&'static str, crate::model::witnesses::ReturnExpr)> {
        match self {
            ParametricType::ResultSet { .. } => {
                let row_of_receiver = crate::model::witnesses::ReturnExpr::Operator(
                    crate::model::witnesses::ParametricOp::RowOf(Box::new(
                        crate::model::witnesses::ReturnExpr::Receiver,
                    )),
                );
                ["find", "first", "single", "next", "create",
                 "find_or_new", "find_or_create", "update_or_create",
                 "new_result"]
                    .iter()
                    .map(|m| (*m, row_of_receiver.clone()))
                    .collect()
            }
            // Members come from the base class's own defs; return
            // projection (arg substitution) is instantiation-aware
            // typing's job, not a declaration table.
            ParametricType::Instance { .. } => Vec::new(),
        }
    }

    /// The canonical full spelling of a template `Instance`
    /// (`Box<Widget>`, `formatter<int, char>`) — `None` for every other
    /// flavor. Two consumers: presentation (hover shows the args even
    /// though dispatch uses the base), and the per-spec dispatch key —
    /// when a specialization class by this exact spelling exists
    /// (`template<> struct formatter<int>` minted `formatter<int>`),
    /// member resolution keys there instead of the base primary
    /// (`FileAnalysis::dispatch_class_of`). Canonical by construction:
    /// args were canonicalized at peel time and joined `", "`, matching
    /// `canonical_template_spelling`'s output for the source text.
    pub fn exact_spelling(&self) -> Option<String> {
        match self {
            ParametricType::ResultSet { .. } => None,
            ParametricType::Instance { base, args } => {
                let parts: Vec<String> = args
                    .iter()
                    .map(|a| match a {
                        // A leaf arg IS its carried spelling.
                        InferredType::ClassName(n) => n.clone(),
                        // A nested instance reconstructs recursively;
                        // anything else (never minted by the peel, but
                        // reachable if a future pass substitutes) renders
                        // through the shared formatter.
                        InferredType::Parametric(p) => p
                            .exact_spelling()
                            .unwrap_or_else(|| format_parametric_type(p)),
                        other => format_inferred_type(other),
                    })
                    .collect();
                Some(format!("{}<{}>", base, parts.join(", ")))
            }
        }
    }

    /// Structurally peel a `Base<Args>` type spelling into
    /// `Instance { base, args }` — the ONE peel every consumer routes
    /// through (cpp `annot_type`, the `TypeName` alias-chase terminal).
    /// `None` when the text isn't a well-formed template spelling
    /// (no `<…>`, unbalanced brackets, non-identifier base) — callers
    /// fall through to their existing non-template handling.
    ///
    /// The base keeps its LAST `::` segment (matching how `annot_type`
    /// keys classes unqualified); arg spellings are carried VERBATIM
    /// (namespace-qualified, uninterpreted — `int` stays
    /// `ClassName("int")`, not `Numeric`) so the exact-spelling dispatch
    /// key reconstructs and substitution stays a later pass's decision.
    /// Whitespace canonicalizes per `canonical_template_spelling`; a
    /// template-shaped arg recurses into its own `Instance`.
    pub fn instance_from_spelling(text: &str) -> Option<ParametricType> {
        let t = text.trim();
        let lt = t.find('<')?;
        if !t.ends_with('>') || t.len() < lt + 2 {
            return None;
        }
        let base_full = t[..lt].trim_end();
        let ident_ok = !base_full.is_empty()
            && base_full.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && base_full.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':');
        if !ident_ok {
            return None;
        }
        let inner = &t[lt + 1..t.len() - 1];
        // Split on top-level commas only — nested `<…>` / `(…)` / `[…]`
        // keep their commas.
        let mut raw_args: Vec<&str> = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        for (i, c) in inner.char_indices() {
            match c {
                '<' | '(' | '[' => depth += 1,
                '>' | ')' | ']' => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                }
                ',' if depth == 0 => {
                    raw_args.push(&inner[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None;
        }
        raw_args.push(&inner[start..]);
        let base = base_full.rsplit("::").next().unwrap_or(base_full).to_string();
        let mut args = Vec::with_capacity(raw_args.len());
        for raw in raw_args {
            let spelling = canonical_template_spelling(raw.trim());
            if spelling.is_empty() {
                return None;
            }
            args.push(match ParametricType::instance_from_spelling(&spelling) {
                Some(p) => InferredType::Parametric(p),
                None => InferredType::ClassName(spelling),
            });
        }
        Some(ParametricType::Instance { base, args })
    }
}

/// The ONE whitespace-canonical form for a C++ template spelling — the
/// identity key a specialization/instantiation is filed under
/// (`formatter<int, char>`), however the source spaced or wrapped it. Rules:
/// every whitespace RUN collapses; a space survives only between two word
/// characters (`[A-Za-z0-9_]`), where it is lexically load-bearing
/// (`unsigned long`); a comma is followed by exactly one space when more
/// text follows. Ordinary identifiers (no whitespace, no comma) pass
/// through unchanged. Lives in the Model layer so the `Instance` peel and
/// the Build-layer pack `shape_name` share one rule.
pub fn canonical_template_spelling(raw: &str) -> String {
    if !raw.contains(|c: char| c.is_whitespace() || c == ',') {
        return raw.to_string();
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            let prev_word = out.chars().next_back().is_some_and(is_word);
            let next_word = chars.peek().copied().is_some_and(is_word);
            if prev_word && next_word {
                out.push(' ');
            }
        } else if c == ',' {
            out.push(',');
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if chars.peek().is_some() {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Substitute a class's template parameters with a receiver instance's
/// type arguments, structurally: a `ClassName` leaf naming `params[i]`
/// becomes `args[i]`; a nested `Instance` recurses so a param one hop
/// under a template spelling (`vector<T>`) substitutes; everything else
/// passes through. The value-side twin of `ParametricOp::ParamOf` —
/// fields substitute here at query time, methods through the reducer.
pub fn substitute_type_params(
    ty: &InferredType,
    params: &[String],
    args: &[InferredType],
) -> InferredType {
    match ty {
        InferredType::ClassName(n) => {
            if let Some(a) = params.iter().position(|p| p == n).and_then(|i| args.get(i)) {
                return a.clone();
            }
            ty.clone()
        }
        InferredType::Parametric(p) => match p {
            // A ResultSet's row/base are Perl package names — no C++
            // template params can occur inside them.
            ParametricType::ResultSet { .. } => ty.clone(),
            ParametricType::Instance { base, args: inner } => {
                InferredType::Parametric(ParametricType::Instance {
                    base: base.clone(),
                    args: inner
                        .iter()
                        .map(|a| substitute_type_params(a, params, args))
                        .collect(),
                })
            }
        },
        other => other.clone(),
    }
}

/// Structural match of a concrete template instance against a partial
/// specialization's PATTERN (`formatter<vector<int>>` vs the spec
/// `formatter<vector<T>>`, `params = ["T"]`). On success returns the
/// bindings in PARAM ORDER (`T → int`) — the receiver args a member query
/// on the spec's class substitutes — plus a specificity score (count of
/// literal structure the pattern pinned; more literal = more specific,
/// the tie-break when several partial patterns match). `None` when the
/// shapes differ or a param stays unbound / binds inconsistently.
///
/// A general walk, never per-name: a pattern leaf that IS a param binds
/// the whole concrete arg; a leaf with one param embedded at a word
/// boundary (`T*`, `const T`) binds the middle against the literal
/// prefix/suffix; nested instances recurse. Template-template patterns
/// (a param in base position) are out of scope — parked with the
/// deduction rungs.
pub fn match_template_pattern(
    pattern: &ParametricType,
    params: &[String],
    concrete: &ParametricType,
) -> Option<(Vec<InferredType>, u32)> {
    let (
        ParametricType::Instance { base: pb, args: pa },
        ParametricType::Instance { base: cb, args: ca },
    ) = (pattern, concrete)
    else {
        return None;
    };
    if pb != cb || pa.len() != ca.len() {
        return None;
    }
    let mut bound: Vec<Option<InferredType>> = vec![None; params.len()];
    let mut score = 1u32; // the base literal itself
    for (p, c) in pa.iter().zip(ca.iter()) {
        match_pattern_arg(p, c, params, &mut bound, &mut score)?;
    }
    let bindings: Option<Vec<InferredType>> = bound.into_iter().collect();
    bindings.map(|b| (b, score))
}

/// One pattern-arg vs concrete-arg step of `match_template_pattern`.
/// `Some(())` = matched (bindings/score updated); `None` = mismatch.
fn match_pattern_arg(
    pattern: &InferredType,
    concrete: &InferredType,
    params: &[String],
    bound: &mut [Option<InferredType>],
    score: &mut u32,
) -> Option<()> {
    let bind = |i: usize, val: InferredType, bound: &mut [Option<InferredType>]| -> Option<()> {
        match &bound[i] {
            Some(prev) if *prev != val => None, // inconsistent re-binding
            _ => {
                bound[i] = Some(val);
                Some(())
            }
        }
    };
    match pattern {
        InferredType::ClassName(p) => {
            // The leaf IS a param → binds the whole concrete arg.
            if let Some(i) = params.iter().position(|q| q == p) {
                return bind(i, concrete.clone(), bound);
            }
            // One param embedded at a word boundary (`T*`, `const T&`):
            // the literal prefix/suffix anchors, the middle binds.
            let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
            for (i, q) in params.iter().enumerate() {
                let Some(pos) = p.match_indices(q.as_str()).find_map(|(pos, _)| {
                    let before_ok =
                        pos == 0 || !p[..pos].chars().next_back().is_some_and(is_word);
                    let after_ok = p[pos + q.len()..].chars().next().is_none_or(|c| !is_word(c));
                    (before_ok && after_ok).then_some(pos)
                }) else {
                    continue;
                };
                let (prefix, suffix) = (&p[..pos], &p[pos + q.len()..]);
                let InferredType::ClassName(cs) = concrete else { return None };
                let mid = cs.strip_prefix(prefix)?.strip_suffix(suffix)?;
                if mid.is_empty() {
                    return None;
                }
                *score += 1; // the literal structure around the hole
                return bind(i, InferredType::ClassName(mid.to_string()), bound);
            }
            // Pure literal: spellings must agree.
            let InferredType::ClassName(cs) = concrete else { return None };
            (cs == p).then(|| *score += 1)
        }
        InferredType::Parametric(pp) => {
            // Recurse with the SAME binding table: an inner pattern
            // (`vector<T>`) shares the spec's params.
            let InferredType::Parametric(cp) = concrete else { return None };
            let (
                ParametricType::Instance { base: pb, args: pa },
                ParametricType::Instance { base: cb, args: ca },
            ) = (pp, cp)
            else {
                return None;
            };
            if pb != cb || pa.len() != ca.len() {
                return None;
            }
            *score += 1; // the nested base literal
            for (p, c) in pa.iter().zip(ca.iter()) {
                match_pattern_arg(p, c, params, bound, score)?;
            }
            Some(())
        }
        // Peel-minted patterns only carry ClassName / Instance leaves.
        _ => None,
    }
}

impl InferredType {
    /// Extract the class name if this is an object type
    /// (ClassName, FirstParam, or Parametric — the latter
    /// delegates to the flavor's `class_name()`). For the row-
    /// class / hash-key-arg dimension on a Parametric, see
    /// `hash_key_class`.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            InferredType::ClassName(name) => Some(name.as_str()),
            InferredType::FirstParam { package } => Some(package.as_str()),
            InferredType::Parametric(p) => p.class_name(),
            // A branded route still dispatches methods against its
            // base class — `$r->get(...)` works the same whether `$r`
            // carries inherited defaults or not.
            InferredType::BrandedRoute { base, .. } => Some(base.as_str()),
            _ => None,
        }
    }

    /// The wrapped type of an `Optional<T>`, else `None`. The `defined` /
    /// `blessed` guards strip an optional to its inner via this.
    pub fn optional_inner(&self) -> Option<&InferredType> {
        match self {
            InferredType::Optional(inner) => Some(inner),
            _ => None,
        }
    }

    /// The class this value **dispatches / navigates to**, leniently
    /// peeling `Optional<...>` layers. Distinct from `class_name()` (which
    /// the fold's return-type math relies on staying strict — a sub that
    /// returns `Optional<Foo>` must keep *typing* as `Optional<Foo>`):
    /// this is the consumer-side projection for receiver resolution, where
    /// an `Optional<Foo>` should still resolve methods/goto/hover on `Foo`.
    /// The author may simply not have written the `defined` guard yet; a
    /// future deref diagnostic owns the "might be undef" warning, not the
    /// silence of a dead receiver. `Undef` peels to nothing — it is
    /// definitely not an object.
    pub fn class_name_lenient(&self) -> Option<&str> {
        let mut t = self;
        while let Some(inner) = t.optional_inner() {
            t = inner;
        }
        t.class_name()
    }

    /// `->{key}` narrowing on a structurally-typed hash (rule #10: ask
    /// the value). `Some(Some(t))` = key present with a known value
    /// type; `Some(None)` = key present, value type unknown; `None` =
    /// not a key of this value (or not a keyed hash at all). Closed
    /// shapes answer `None` for unknown keys; open shapes (spread)
    /// also answer `None` — the caller can't claim a miss either way,
    /// the distinction is for future diagnostics.
    /// Hash-shaped rep, regardless of key knowledge — the predicate
    /// every "is this a hashref" gate asks instead of `== HashRef`.
    pub fn is_hash_shaped(&self) -> bool {
        matches!(
            self,
            InferredType::HashRef | InferredType::HashWithKeys { .. }
        )
    }

    /// Array-shaped rep, regardless of element knowledge — the
    /// `is_hash_shaped` twin for `== ArrayRef` gates.
    pub fn is_array_shaped(&self) -> bool {
        matches!(self, InferredType::ArrayRef | InferredType::Sequence(_))
    }

    pub fn key_value_type(&self, key: &str) -> Option<Option<&InferredType>> {
        match self {
            InferredType::HashWithKeys { keys, .. } => keys
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, t)| t.as_deref()),
            _ => None,
        }
    }

    /// Read the inherited route default for `key` from a branded
    /// route value, where `controller` is a distinguished key and
    /// everything else lives in the stash. `None` for non-route
    /// types or absent keys. This is the "ask the value" entry point
    /// (rule #10): a partial `->to('#action')` consumer asks the
    /// receiver value what controller is in force; it never inspects
    /// the chain shape. The build-time consumer reads the flattened
    /// `route_defaults` projection; this is the query-time
    /// surface for cursor-time stash lookups (hover/completion), which
    /// aren't wired yet — hence `allow(dead_code)`.
    #[allow(dead_code)]
    pub fn route_default(&self, key: &str) -> Option<&str> {
        let InferredType::BrandedRoute { controller, stash, .. } = self else {
            return None;
        };
        if key == "controller" {
            return controller.as_deref();
        }
        stash.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Project a `TypeConstraintOf(inner)` to its constrained inner type —
    /// the type a value satisfying this constraint has. `None` for any
    /// non-constraint type. This is the rule-#10 "ask the value" entry
    /// point: `has`'s isa→accessor projection calls it without ever
    /// matching on the constraint's shape itself.
    pub fn constrained_inner(&self) -> Option<&InferredType> {
        match self {
            InferredType::TypeConstraintOf(inner) => Some(inner),
            _ => None,
        }
    }

    /// Project a `Sequence(...)` to its element at index `i`. Negative
    /// indices wrap from the end (Perl `$arr[-1]`). `None` for any
    /// non-Sequence type or out-of-bounds index.
    pub fn element_at(&self, i: i32) -> Option<&InferredType> {
        let InferredType::Sequence(elems) = self else { return None };
        let n = elems.len() as i32;
        let idx = if i < 0 { n + i } else { i };
        if idx < 0 || idx >= n { return None; }
        elems.get(idx as usize)
    }

    /// True if this is any object-shaped variant (ClassName,
    /// FirstParam, or a Parametric flavor that has a class_name).
    pub fn is_object(&self) -> bool {
        self.class_name().is_some()
    }

    /// Class to consult for direct `recv->{key}` hash-key access
    /// on this value. For Parametric, delegates to the flavor; for
    /// other variants, falls back to `class_name()` (constructor
    /// keys etc. on `bless { } 'Foo'`-shaped values).
    pub fn hash_key_class(&self) -> Option<&str> {
        match self {
            InferredType::Parametric(p) => p.hash_key_class(),
            _ => self.class_name(),
        }
    }

    /// Direct accessor to the parametric flavor, when this type
    /// is `Parametric(_)`. Lets consumers route to flavor-specific
    /// methods (`method_arg_owner`, etc.) without re-matching.
    pub fn as_parametric(&self) -> Option<&ParametricType> {
        match self {
            InferredType::Parametric(p) => Some(p),
            _ => None,
        }
    }

    /// Witness-bag attachment whose type IS this callable's return
    /// when invoked. `Expr(span)` for anon-sub literals (resolves
    /// at query time via the body's last-expression witnesses);
    /// `PackageSymbol{package, name}` for named-sub references
    /// (`\&foo`, `\&Foo::bar` — resolves via the bag's existing
    /// MRO + cross-file machinery, same shape used by method
    /// dispatch). Returns `None` for opaque coderef sources.
    ///
    /// Survives variable rebinding: chain typing propagates the
    /// `InferredType` through `my $cb = ...` via the bag's TC
    /// machinery, so consumers see the same attachment whether
    /// the callable arrives as a literal or a rebound scalar.
    pub fn callable_return_edge(&self) -> Option<&crate::model::witnesses::WitnessAttachment> {
        match self {
            InferredType::CodeRef { return_edge } => return_edge.as_ref(),
            _ => None,
        }
    }

    /// True when `self` is at least as informative as `narrowing`
    /// — adding the narrowing's TC would not refine `self` further.
    /// "Informativeness" is defined per-variant: same discriminant
    /// AND, where the variant carries refinable payload, `self`'s
    /// payload is at least as specific as `narrowing`'s.
    ///
    /// Used by `infer_deref_type` to suppress the
    /// `$cb->()`-shaped narrowing TC when the operand was already
    /// typed with a richer attachment (e.g. an anon-sub literal's
    /// `CodeRef { return_edge: Some(_) }` should NOT be clobbered
    /// by the deref's `CodeRef { return_edge: None }` under
    /// latest-wins reduction).
    ///
    /// Conservative: returns false on different discriminants
    /// (let the reducer-stack decide the conflict). Variants
    /// without refinable payload (HashRef/ArrayRef/Regexp/Numeric/
    /// String) subsume themselves trivially.
    pub fn subsumes_narrowing(&self, narrowing: &InferredType) -> bool {
        match (self, narrowing) {
            // Refinable-payload variants — `self` subsumes only
            // if its payload is at least as specific.
            (
                InferredType::CodeRef { return_edge: have },
                InferredType::CodeRef { return_edge: want },
            ) => want.is_none() || have.is_some(),
            (InferredType::ClassName(a), InferredType::ClassName(b)) => a == b,
            (InferredType::FirstParam { package: a }, InferredType::FirstParam { package: b }) => {
                a == b
            }
            (InferredType::Parametric(a), InferredType::Parametric(b)) => a == b,
            // A route with strictly more resolved defaults subsumes a
            // plainer one (more keys = more informative). Keep the
            // assignment chain from clobbering an accumulated brand
            // with a freshly-typed bare `ClassName(Route)` re-derivation.
            (
                InferredType::BrandedRoute { controller: hc, stash: hs, .. },
                InferredType::BrandedRoute { controller: wc, stash: ws, .. },
            ) => (wc.is_none() || hc.is_some()) && ws.len() <= hs.len(),
            // Structure dominates rep: a keyed hash / positional tuple is
            // strictly more informative than the bare ref a deref-
            // narrowing observation re-derives. Structured-vs-structured
            // only subsumes on equality (a genuine reassignment with a
            // different shape must win as latest).
            (InferredType::HashWithKeys { .. }, InferredType::HashRef) => true,
            (a @ InferredType::HashWithKeys { .. }, b @ InferredType::HashWithKeys { .. }) => {
                a == b
            }
            (InferredType::Sequence(_), InferredType::ArrayRef) => true,
            (a @ InferredType::Sequence(_), b @ InferredType::Sequence(_)) => a == b,
            // Identity dominates rep: a blessed object accessed as
            // `$self->{field}` / `$self->[i]` / `$self->()` reveals its
            // internal REPRESENTATION, it does not narrow the object's TYPE
            // to a bare ref. The class identity stays — the same
            // structure-over-rep rule above, lifted to class-over-rep. So a
            // deref-narrowing never clobbers an invocant's class at its
            // access site (which would otherwise mask the identity at inner-
            // scope reads, since the rep witness lands on the nested block
            // while the class lives on the sub scope).
            (
                a,
                InferredType::HashRef | InferredType::ArrayRef | InferredType::CodeRef { .. },
            ) if a.class_name().is_some() => true,
            // An optional subsumes a narrowing only as specifically as its
            // inner does; a CONCRETE self is at least as specific as an
            // optional narrowing (the narrowing already happened). The
            // reverse — `Optional` vs a concrete narrowing — falls to the
            // discriminant check below and is `false`, so a `defined` /
            // `blessed` guard's `Optional<T> → T` refinement wins.
            (InferredType::Optional(a), InferredType::Optional(b)) => a.subsumes_narrowing(b),
            (a, InferredType::Optional(b)) => a.subsumes_narrowing(b),
            // Unit-shape variants subsume themselves; mismatched
            // discriminants don't subsume.
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}

/// Where a type judgement came from. Lets debugging surface "the
/// analyzer worked this out from your code" vs "a plugin override
/// said so" without changing the shape of `InferredType` at every
/// callsite. Stored in a sidecar map keyed by SymbolId — entries
/// only exist for non-default provenances, so the common case (an
/// inferred type) costs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeProvenance {
    /// Derived from the analyzed source — return statements,
    /// last-expression inference, framework synthesis, constructor
    /// patterns. The default; never stored explicitly.
    Inferred,
    /// Asserted by a plugin's `overrides()` manifest because
    /// inference can't (or shouldn't) reach the right answer here.
    /// Carries the asserting plugin's id and a free-form reason for
    /// the debugger.
    PluginOverride { plugin_id: String, reason: String },
    /// Produced by a witness-bag fold at type-resolution time.
    /// `reducer` names the rule that fired (currently only
    /// `"return_arms"` is recorded — see `seed_return_types_from_bag`);
    /// `evidence` is a short list of human-readable facts the fold
    /// leaned on. Read-only debugging aid surfaced by `--dump-package`.
    /// Empty `evidence` is fine — the reducer name alone often answers
    /// "why".
    ReducerFold { reducer: String, evidence: Vec<String> },
    /// Tail-delegation: the sub's body ends in `shift->M(...)` /
    /// `$self->M(...)` / `return Y()` and inherits the tail's
    /// return type. `via` is the delegate's name; `kind` is
    /// "self_method_tail" or "sub_return". Lets `--dump-package`
    /// answer "get returns ClassName(Route) — because it tails on
    /// _generate_route which the framework-aware reducer typed as
    /// ClassName(Route)".
    Delegation { kind: String, via: String },
    /// Core framework synthesis — Mojo::Base / Moo / Moose `has`,
    /// DBIx::Class `add_columns` / `has_many` / etc. The accessor
    /// has no source body to fold; the type comes directly from the
    /// declaration shape (Mojo writers always return the invocant;
    /// Moo getters honour `isa`; DBIC relationships return the
    /// related class). `framework` names the rule set
    /// ("Mojo::Base" / "Moo" / "Moose" / "DBIx::Class") and
    /// `reason` describes the specific accessor ("`has 'level'`
    /// fluent writer", "DBIx::Class row relationship `book`").
    /// Distinct from `PluginOverride` because plugins are user-installed
    /// and configurable; framework synthesis is built into the analyzer.
    FrameworkSynthesis { framework: String, reason: String },
}

/// Resolve a return type from a list of inferred types (one per return statement).
///
/// Rules (from spec):
/// - All agree → that type
/// - Object subsumes HashRef (overloaded objects are common)
/// - Disagreement → None (Unknown)
///
/// The input should already have bare returns / undef filtered out.
pub fn resolve_return_type(return_types: &[InferredType]) -> Option<InferredType> {
    if return_types.is_empty() {
        return None;
    }
    let first = &return_types[0];
    if return_types.iter().all(|t| t == first) {
        return Some(first.clone());
    }
    // All arms hash-shaped but structurally different (`{a=>1}` vs
    // `{b=>2}`) → degrade to the coarse HashRef rather than Unknown.
    if return_types.iter().all(|t| t.is_hash_shaped()) {
        return Some(InferredType::HashRef);
    }
    // Same rule for arrays: structurally different tuples agree on the
    // coarse ArrayRef.
    if return_types.iter().all(|t| t.is_array_shaped()) {
        return Some(InferredType::ArrayRef);
    }
    // Bool is a sub-lattice of Numeric in Perl (truth is `1`/`''`), so a
    // sub whose arms mix a comparison (`Bool`) with a plain number
    // (`Numeric`) is coherently number-ish — join to the coarser Numeric
    // rather than degrading to Unknown. All-Bool already returned via the
    // exact-equal check above, so this only fires on a genuine mix.
    if return_types
        .iter()
        .all(|t| matches!(t, InferredType::Bool | InferredType::Numeric))
    {
        return Some(InferredType::Numeric);
    }
    // Object subsumes HashRef: if some returns are Object(X) and others are
    // hash-shaped, the Object wins (overloaded hash access is common in Perl).
    let mut object: Option<InferredType> = None;
    for t in return_types {
        if t.is_object() {
            // Two different classes are a disagreement, not a choice of the
            // arm that came last (`WP_Term` vs `WP_Error`).
            if object.as_ref().is_some_and(|o| o != t) {
                return None;
            }
            object = Some(t.clone());
        } else if !t.is_hash_shaped() {
            // Non-hash, non-Object disagreement → Unknown
            return None;
        }
    }
    object
}

/// Join return/branch arms where some arm may be `undef` (a bare
/// `return;`, `return undef`, or an `undef` branch). The value arms fold
/// by [`resolve_return_type`]; if any arm was undef and the value arms
/// agree on a single non-optional `T`, the result is `Optional<T>` —
/// `{Foo, undef} → Optional<Foo>`. No undef arm leaves the fold
/// unchanged; genuinely-conflicting value arms (`Foo` vs `Bar`) stay
/// `None` (no arbitrary union); only-undef arms stay `None` (no useful
/// value type). See `docs/adr/optional-types.md`.
pub fn join_return_arms(value_types: &[InferredType], has_undef_arm: bool) -> Option<InferredType> {
    let base = resolve_return_type(value_types);
    match base {
        // An untypable value stays untypable; `Optional<Unknown>` would
        // smuggle it past the boundary scrub.
        Some(InferredType::Unknown) => Some(InferredType::Unknown),
        Some(t) if has_undef_arm && !matches!(t, InferredType::Optional(_)) => {
            Some(InferredType::Optional(Box::new(t)))
        }
        other => other,
    }
}


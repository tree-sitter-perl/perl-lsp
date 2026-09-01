//! The generic extraction driver: capture events, the wrapper-chain
//! peel, and `extract()` — knows no language specifics.

use super::*;

/// One capture event, flattened from query matches and sorted by
/// position so the driver can run its state machine in source order.
struct Event {
    start_byte: usize,
    end_byte: usize,
    start: Point,
    end: Point,
    /// Capture vocabulary name, e.g. "def.sub", "def.sub.name",
    /// "scope", "context.package", "ref.call", "import.name".
    cap: String,
    text: String,
    /// Query match id — name captures join their parent def through it.
    match_id: usize,
}

/// Flatten a wrapper chain — the recursion tree-sitter's fixed-depth queries
/// cannot express — to its leaf, recording a `DerefStep` per level when
/// `spec.record_stack`. A non-empty `leaf_to_def` REQUIRES the leaf to match
/// (returns its def capture); an empty one accepts ANY leaf and mints nothing
/// (the receiver peel — the leaf is an invocant). Outermost level first
/// (left-to-right display order, `Box*&` → `[Pointer, Reference]`). Depth-
/// capped. The ONE peel: `nested_peel` and `recv_peel` are both this.
pub(crate) fn peel<'a>(
    mut node: tree_sitter::Node<'a>,
    spec: &PeelSpec,
    src: &[u8],
) -> Option<(tree_sitter::Node<'a>, Vec<crate::model::file_analysis::DerefStep>, Option<&'static str>)> {
    use crate::model::file_analysis::DerefStep;
    let is_leaf = |k: &str| spec.leaf_to_def.iter().find(|(lk, _)| *lk == k);
    let mut stack = Vec::new();
    for _ in 0..32 {
        if let Some((_, dk)) = spec.wrappers.iter().find(|(k, _)| *k == node.kind()) {
            let mut annotations = Vec::new();
            let mut inner = None;
            let mut cur = node.walk();
            for ch in node.children(&mut cur) {
                if spec.annot_kinds.contains(&ch.kind()) {
                    if let Ok(t) = ch.utf8_text(src) {
                        annotations.push(t.to_string());
                    }
                } else if inner.is_none()
                    && (spec.wrappers.iter().any(|(k, _)| *k == ch.kind())
                        || is_leaf(ch.kind()).is_some()
                        || (spec.leaf_to_def.is_empty() && ch.is_named()))
                {
                    inner = Some(ch);
                }
            }
            if spec.record_stack {
                stack.push(DerefStep { kind: *dk, annotations });
            }
            node = inner?;
        } else if spec.leaf_to_def.is_empty() {
            // receiver peel: the leaf is an invocant of any shape, no def minted.
            return Some((node, stack, None));
        } else if let Some((_, def_cap)) = is_leaf(node.kind()) {
            // `identifier`→`def.local` (param/local), `field_identifier`→
            // `def.var` (a class member), so a pointer field outlines as a member.
            return Some((node, stack, Some(def_cap)));
        } else {
            return None;
        }
    }
    None
}

pub fn extract(tree: &Tree, source: &[u8], pack: &LangPack) -> Result<SkeletonAnalysis, String> {
    let language = tree.language();
    let query = cached_query(&language, effective_query_source(&language, pack))?;
    let cap_names: Vec<String> = query
        .capture_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    // ---- flatten matches into ordered events ----
    let mut events: Vec<Event> = Vec::new();
    // match_id → the pointer/reference declarator stack a `@nested.target`
    // capture unravelled to. Read by the `def.*` handler to stamp the symbol.
    let mut nested_stacks: std::collections::HashMap<usize, Vec<crate::model::file_analysis::DerefStep>> =
        std::collections::HashMap::new();
    // match_id → was the IMMEDIATE member-access receiver a simple variable?
    // Recorded at construction (the un-peeled node); gates op-DX at the mint.
    let mut member_simple: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
    // A call's written arg count, keyed by its argument_list's START point
    // (the `(`), which is adjacent to the callee/method name token's END — so
    // a ref finds its arity by `ref.end == arglist.start` without a match join
    // (member calls fire a separate match from their arg list). One entry per
    // `@arity.args`.
    let mut arg_counts_by_start: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();
    // A callable's declared parameter arity, keyed by the parameter_list span.
    // Associated to its def symbol by span containment in `into_file_analysis`
    // (`@arity.sig` fires a separate match from the def name).
    let mut param_sigs: Vec<(crate::model::file_analysis::Span, crate::model::file_analysis::ParamArity)> =
        Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut match_counter = 0usize;
    while let Some(m) = matches.next() {
        match_counter += 1;
        for c in m.captures {
            let node = c.node;
            let cap = cap_names[c.index as usize].as_str();
            // `@ool.def`: an out-of-line definition (`Ret Class::method(...) {}`).
            // The one general capture (fires for EVERY function_definition) —
            // peel the declarator to the function declarator, walk its qualified
            // name to the leaf + owning class, and synthesize the
            // `def.method` / `def.method.name` / `qualifier` events downstream
            // extraction consumes — the same vocabulary the narrow per-shape
            // patterns emit for the shapes they own. A non-qualified declarator
            // (free function / in-class method) yields nothing here — its own
            // pattern owns it. Arbitrary declarator nesting + multi-level
            // qualifiers (which fixed-depth S-queries can't express) work by
            // construction.
            if cap == "ool.def" {
                if let Some((scope_text, leaf)) = node
                    .child_by_field_name("declarator")
                    .and_then(|d| unwrap_to_function_declarator(d, &pack.oolfn))
                    .and_then(|fd| fd.child_by_field_name("declarator"))
                    .and_then(|q| {
                        walk_qualifier_chain(q, pack.oolfn.qualified_name, pack.qualifier_peel, source)
                    })
                {
                    let leaf_text = leaf.utf8_text(source).unwrap_or("").to_string();
                    // the def symbol spans the whole function_definition; name +
                    // owner come from the qualified declarator's leaf + scope.
                    events.push(Event {
                        start_byte: node.start_byte(),
                        end_byte: node.end_byte(),
                        start: node.start_position(),
                        end: node.end_position(),
                        cap: "def.method".to_string(),
                        text: leaf_text.clone(),
                        match_id: match_counter,
                    });
                    events.push(Event {
                        start_byte: leaf.start_byte(),
                        end_byte: leaf.end_byte(),
                        start: leaf.start_position(),
                        end: leaf.end_position(),
                        cap: "def.method.name".to_string(),
                        text: leaf_text,
                        match_id: match_counter,
                    });
                    events.push(Event {
                        start_byte: node.start_byte(),
                        end_byte: node.end_byte(),
                        start: node.start_position(),
                        end: node.end_position(),
                        cap: "qualifier".to_string(),
                        text: scope_text,
                        match_id: match_counter,
                    });
                }
                continue;
            }
            // `@nested.target`: a pointer/reference declarator CHAIN of any
            // depth. Peel it (where the node is live) to the leaf identifier
            // + the deref stack, then emit the leaf as if the query had
            // captured it directly — downstream join/symbol/witness paths are
            // unchanged, and arbitrary nesting works without enumerating it.
            if cap == "nested.target" {
                if let Some((leaf, stack, Some(def_cap))) = peel(node, &pack.nested_peel, source) {
                    nested_stacks.insert(match_counter, stack);
                    let ltext = leaf.utf8_text(source).unwrap_or("").to_string();
                    for syn in ["flow.target", def_cap] {
                        events.push(Event {
                            start_byte: leaf.start_byte(),
                            end_byte: leaf.end_byte(),
                            start: leaf.start_position(),
                            end: leaf.end_position(),
                            cap: syn.to_string(),
                            text: ltext.clone(),
                            match_id: match_counter,
                        });
                    }
                }
                continue;
            }
            // `@member.recv`: a member access receiver. Peel transparent
            // wrappers (`(*p)`, `(&o)`, `(p)`) to the typed inner where the
            // node is live, so the minted MethodCall ref's invocant_span lands
            // on the inner expression `expr_type_at_span` already types.
            if cap == "member.recv" {
                // op-DX applies only to a bare-variable immediate receiver
                // (its deref_stack resolves by name); a wrapper/chain doesn't.
                member_simple.insert(match_counter, pack.simple_var_kinds.contains(&node.kind()));
                let inner = peel(node, &pack.recv_peel, source)
                    .map(|(leaf, _, _)| leaf)
                    .unwrap_or(node);
                events.push(Event {
                    start_byte: inner.start_byte(),
                    end_byte: inner.end_byte(),
                    start: inner.start_position(),
                    end: inner.end_position(),
                    cap: cap.to_string(),
                    text: inner.utf8_text(source).unwrap_or("").to_string(),
                    match_id: match_counter,
                });
                continue;
            }
            // `@arity.args`: a call's argument_list — count its arguments (the
            // named children; the C `...` at a CALL site never appears here).
            // Keyed by the list's start so the callee ref finds it by adjacency.
            if cap == "arity.args" {
                arg_counts_by_start
                    .insert((node.start_position().row, node.start_position().column),
                            node.named_child_count());
                continue;
            }
            // `@arity.sig`: a callable's parameter_list — count declared params
            // structurally. `optional_parameter_declaration` carries a default
            // (counts toward `total`, not `required`); a template pack
            // (`variadic_parameter_declaration`) or a C `...` token makes the
            // signature variadic.
            if cap == "arity.sig" {
                let mut total = 0usize;
                let mut required = 0usize;
                let mut variadic = false;
                let mut c = node.walk();
                for ch in node.children(&mut c) {
                    match ch.kind() {
                        "parameter_declaration" => { total += 1; required += 1; }
                        "optional_parameter_declaration" => { total += 1; }
                        // PHP: a parameter with a default is optional; a
                        // promoted ctor param still counts toward arity.
                        "simple_parameter" | "property_promotion_parameter" => {
                            total += 1;
                            if ch.child_by_field_name("default_value").is_none() {
                                required += 1;
                            }
                        }
                        "variadic_parameter_declaration" | "variadic_parameter" | "..." => {
                            variadic = true
                        }
                        _ => {}
                    }
                }
                param_sigs.push((
                    crate::model::file_analysis::Span {
                        start: node.start_position(),
                        end: node.end_position(),
                    },
                    crate::model::file_analysis::ParamArity { total, required, variadic },
                ));
                continue;
            }
            // `@qualifier` on a templated owner (`Buf<T>::grow`): the class
            // the def joins is the BASE name — peel the `name` field where
            // the node is live (structural, never a string split on `<`).
            let text = if cap == "qualifier" && pack.qualifier_peel.contains(&node.kind()) {
                node.child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or(node.utf8_text(source).unwrap_or(""))
                    .to_string()
            } else {
                node.utf8_text(source).unwrap_or("").to_string()
            };
            events.push(Event {
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start: node.start_position(),
                end: node.end_position(),
                cap: cap.to_string(),
                text,
                match_id: match_counter,
            });
        }
    }
    // Source order; outermost first on ties so scopes push before their
    // contents. A `@scope` on the SAME node as a `@def` (a function_definition
    // carries its own body scope) must open AFTER the def is recorded, so the
    // symbol attributes to its ENCLOSING scope, not its own body — hence scope
    // sorts last among identical-span ties.
    events.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
            .then(a.cap.starts_with("scope").cmp(&b.cap.starts_with("scope")))
    });

    // ---- join def name-captures to their def event ----
    use std::collections::HashMap;
    let mut names_by_match: HashMap<(usize, String), (String, Point, Point)> = HashMap::new();
    // `@qualifier` (a `Class::` on an out-of-line def) and `@rettype` (a
    // method's declared return type) — pre-collected like names because the
    // `@def` event fires before these inner captures.
    let mut qualifier_by_match: HashMap<usize, String> = HashMap::new();
    let mut rettype_by_match: HashMap<usize, String> = HashMap::new();
    // `@sym.attr` — a token whose TEXT rides onto the match's def symbol as
    // an attribute (cpp: a declaration's storage class, so "extern" is a
    // symbol-borne fact goto-def's decl→def ranking can ask the value for).
    let mut attrs_by_match: HashMap<usize, Vec<String>> = HashMap::new();
    // `@member.recv` → the receiver span of a `recv.field` access, joined to
    // its `@ref.member` by match_id so the minted MethodCall ref carries it.
    let mut member_recv: HashMap<usize, (crate::model::file_analysis::Span, String)> = HashMap::new();
    // `@member.op` → the written operator mapped through `pack.op_map` + its
    // span; joined to `@ref.member` so op-DX rides the minted ref.
    let mut member_op_raw: HashMap<usize, (crate::model::file_analysis::MemberOp, crate::model::file_analysis::Span)> =
        HashMap::new();
    // `@hop.call` → the WHOLE member-call expression's span, joined to its
    // `@ref.member` so the chain-hop witness (`Projected{base, MethodHop}`)
    // attaches where an OUTER call's receiver span will look for it.
    let mut hop_call_by_match: HashMap<usize, crate::model::file_analysis::Span> = HashMap::new();
    // `@dispatch.via` — the dispatching function's name token (`do_action`),
    // joined to the same match's `@ref.dispatch.named` string as the minted
    // DispatchCall's `dispatcher` label.
    let mut dispatch_via_by_match: HashMap<usize, String> = HashMap::new();
    // `@seq.source` — a foreach's collection (span + text), joined to the
    // same match's `@def.var` so the bound var carries the ELEMENT peel;
    // `@seq.source.key` is the pair form's KEY twin (the Key step).
    let mut seq_source_by_match: HashMap<usize, (crate::model::file_analysis::Span, String)> =
        HashMap::new();
    let mut seq_key_by_match: HashMap<usize, (crate::model::file_analysis::Span, String)> =
        HashMap::new();
    // `@nonpublic.target` — def NAME spans whose member carries an access
    // modifier meaning non-public (the vocabulary lives in the query's
    // #any-of?). Joined to symbols by name span in a post-pass, stamping
    // the same `non_public` attribute cpp access regions stamp.
    let mut nonpublic_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@classattr.<flavor>` — container-def name spans stamped with a
    // flavor attribute ("interface"/"trait"): the model's SymKind::Class
    // covers all three php container kinds, and SUPER/reference walks
    // need to ask the value which one it is.
    let mut classattr_by_name_span: HashMap<(Point, Point), String> = HashMap::new();
    for e in &events {
        if let Some(prefix) = e.cap.strip_suffix(".name") {
            names_by_match
                .insert((e.match_id, prefix.to_string()), (e.text.clone(), e.start, e.end));
        }
        if e.cap == "qualifier" {
            qualifier_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "rettype" {
            rettype_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "sym.attr" {
            attrs_by_match.entry(e.match_id).or_default().push(e.text.clone());
        }
        if e.cap == "hop.call" {
            hop_call_by_match.insert(
                e.match_id,
                crate::model::file_analysis::Span { start: e.start, end: e.end },
            );
        }
        if e.cap == "dispatch.via" {
            dispatch_via_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "seq.source.key" {
            seq_key_by_match.insert(
                e.match_id,
                (
                    crate::model::file_analysis::Span { start: e.start, end: e.end },
                    e.text.clone(),
                ),
            );
        }
        if e.cap == "nonpublic.target" {
            nonpublic_name_spans.insert((e.start, e.end));
        }
        if let Some(flavor) = e.cap.strip_prefix("classattr.") {
            classattr_by_name_span.insert((e.start, e.end), flavor.to_string());
        }
        if e.cap == "seq.source" {
            seq_source_by_match.insert(
                e.match_id,
                (
                    crate::model::file_analysis::Span { start: e.start, end: e.end },
                    e.text.clone(),
                ),
            );
        }
    }
    // `@ns.inline` — an inline namespace's NAME token, fired by a name-only
    // sibling pattern (its def/scope/context come from the base namespace
    // pattern, a different match). Joined to the Package symbol by name span
    // in a post-pass below, tagging it "inline" so the qualified-completion
    // gather can lift its members into the enclosing namespace.
    let inline_ns_spans: Vec<(Point, Point)> = events
        .iter()
        .filter(|e| e.cap == "ns.inline")
        .map(|e| (e.start, e.end))
        .collect();
    // (var name, declaring scope) → its declared-type text, joined per
    // declaration match. Feeds the token-less optional-engagement narrowing
    // (`if (opt)`): the guard names no type, so the refinement reads the
    // subject's declared `std::optional<T>`. Keyed by SCOPE (not bare name), so
    // two functions each declaring an `opt` of a DIFFERENT `optional<T>` peel
    // the right inner type — resolved by the guard's scope chain at consumption.
    // Populated inside the main loop (a decl's scope is only known there); the
    // type-annot half is pre-collected here since it precedes the declarator.
    let mut annot_by_match: HashMap<usize, String> = HashMap::new();
    // `@alias.name` (a typedef/using type alias) + `@alias.of` (its
    // underlying type text), joined per match → `TypeName` alias witnesses.
    let mut alias_name_by_match: HashMap<usize, String> = HashMap::new();
    let mut alias_of_by_match: HashMap<usize, String> = HashMap::new();
    // Object-like `#define X body` alias halves — kept SEPARATE from the
    // typedef alias so the emission can gate on a type-shaped body (a
    // macro-heavy header has thousands of value macros we must NOT mint
    // TypeName witnesses for).
    let mut macro_alias_name_by_match: HashMap<usize, String> = HashMap::new();
    let mut macro_alias_of_by_match: HashMap<usize, String> = HashMap::new();
    // `@spec.primary` — the base name a class-spec def specializes; joined to
    // its `@def.class` by match to mint the (spec, primary) family edge.
    let mut spec_primary_by_match: HashMap<usize, String> = HashMap::new();
    // `@domain.value` — the operand a field slot is compared/assigned
    // against. Joined to its `@domain.slot` by match_id (the slot event
    // pushes the site); the value's own enum resolves cross-file later. Only
    // an identifier-shaped operand can name an enumerator, so anything else
    // (a literal, arithmetic, a call — the capture is ungated) is stored as
    // the empty sentinel: it stays a SITE (counter-evidence in the coherence
    // vote's denominator) without persisting arbitrary expression text.
    let mut domain_value_by_match: HashMap<usize, String> = HashMap::new();
    // `@tmpl.param`/`@tmpl.owner` — one template parameter + the class it
    // parameterizes per match; joined below into ordered per-class lists.
    let mut tmpl_param_by_match: HashMap<usize, (String, usize)> = HashMap::new();
    let mut tmpl_owner_by_match: HashMap<usize, String> = HashMap::new();
    for e in &events {
        if e.cap == "type.annot" {
            annot_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "domain.value" {
            let v = if is_identifier_text(&e.text) { e.text.clone() } else { String::new() };
            domain_value_by_match.insert(e.match_id, v);
        }
        if e.cap == "alias.name" {
            alias_name_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "alias.of" {
            alias_of_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "macro.alias.name" {
            macro_alias_name_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "macro.alias.of" {
            macro_alias_of_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "spec.primary" {
            spec_primary_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "tmpl.param" {
            tmpl_param_by_match.insert(e.match_id, (e.text.clone(), e.start_byte));
        }
        if e.cap == "tmpl.owner" {
            tmpl_owner_by_match.insert(e.match_id, e.text.clone());
        }
    }
    let mut annot_text_by_var: HashMap<(String, crate::model::file_analysis::ScopeId), String> =
        HashMap::new();
    // ---- the file's use-map + written parent qualifiers ----
    // `binding leaf (or alias) → (namespace, real leaf)`, from the `@use.*`
    // captures; `@parent.fq` carries a parent's own written qualifier. Both
    // feed the namespace-relative parent resolution in the `@parent` handler
    // (packs with `namespace_relative_parents` only — empty otherwise).
    let mut use_map: HashMap<String, (String, String)> = HashMap::new();
    let mut parent_fq_by_match: HashMap<usize, String> = HashMap::new();
    if pack.namespace_relative_parents {
        let mut use_fqn: HashMap<usize, String> = HashMap::new();
        let mut use_prefix: HashMap<usize, String> = HashMap::new();
        let mut use_leaf: HashMap<usize, String> = HashMap::new();
        let mut use_alias: HashMap<usize, String> = HashMap::new();
        for e in &events {
            match e.cap.as_str() {
                "use.fqn" => {
                    use_fqn.insert(e.match_id, e.text.clone());
                }
                "use.prefix" => {
                    use_prefix.insert(e.match_id, e.text.clone());
                }
                "use.leaf" => {
                    use_leaf.insert(e.match_id, e.text.clone());
                }
                "use.alias" => {
                    use_alias.insert(e.match_id, e.text.clone());
                }
                "parent.fq" => {
                    parent_fq_by_match.insert(e.match_id, e.text.clone());
                }
                _ => {}
            }
        }
        for (mid, fqn) in &use_fqn {
            let (leaf, ns) = split_ns_leaf(fqn);
            let key = use_alias.get(mid).cloned().unwrap_or_else(|| leaf.clone());
            use_map.insert(key, (ns, leaf));
        }
        // group form: `use A\B\{C, D as E}` — the prefix is the namespace,
        // each clause's own name the leaf.
        for (mid, leaf) in &use_leaf {
            let Some(prefix) = use_prefix.get(mid) else { continue };
            let key = use_alias.get(mid).cloned().unwrap_or_else(|| leaf.clone());
            use_map.insert(key, (prefix.trim_start_matches('\\').to_string(), leaf.clone()));
        }
    }

    // ---- the state machine: scope stack + sticky contexts ----
    let mut out = SkeletonAnalysis::default();
    out.receiver_names = pack.receiver_names.iter().map(|s| s.to_string()).collect();
    out.function_scoped_vars = pack.function_scoped_vars;
    out.constructor_names = pack.constructor_names.iter().map(|s| s.to_string()).collect();
    out.type_display = pack
        .type_display
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // Template params joined to their owner class — the owner shaped like a
    // def name (a partial spec's spelling canonicalizes) so the key matches
    // the Class symbol's identity. Source order = the `ParamOf` index axis.
    {
        let mut rows: Vec<(String, String, usize)> = tmpl_param_by_match
            .iter()
            .filter_map(|(mid, (param, pos))| {
                let owner = tmpl_owner_by_match.get(mid)?;
                Some(((pack.shape_name)("tmpl.owner", owner), param.clone(), *pos))
            })
            .collect();
        rows.sort_by_key(|&(_, _, pos)| pos);
        rows.dedup();
        out.template_params = rows;
    }
    // (end_byte, ScopeId) — real Scope rows are minted as we go so the
    // resulting FileAnalysis carries a genuine lexical tree.
    let mut scope_stack: Vec<(usize, crate::model::file_analysis::ScopeId)> = Vec::new();
    // (registered_at_depth, value) — a context set inside a scope pops
    // with it (Python class blocks); one set at file depth is sticky
    // (Perl's flat `package Foo;`).
    let mut context_stack: Vec<(usize, String)> = Vec::new();
    let mut def_name_spans: Vec<(usize, usize)> = Vec::new();

    use crate::model::file_analysis::{Scope, ScopeId, ScopeKind};
    out.scopes.push(Scope {
        id: ScopeId(0),
        parent: None,
        kind: ScopeKind::File,
        span: Span { start: tree.root_node().start_position(), end: tree.root_node().end_position() },
        package: None,
    });
    scope_stack.push((tree.root_node().end_byte(), ScopeId(0)));

    // flow.assign joins: match_id → (target name+scope, source span)
    let mut flow_targets: HashMap<usize, (String, ScopeId, Point)> = HashMap::new();
    let mut flow_sources: HashMap<usize, Span> = HashMap::new();
    // Rebind shapes with no inflowing value (loop vars: `for x in …`,
    // `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    // cutoff sees them, exactly like Perl's `foreach` var.
    let mut flow_rebinds: Vec<(String, ScopeId, Point)> = Vec::new();
    // Destructuring slots (`@flow.slot` in a `@flow.slot.list`) and
    // key-less array-literal tuples (`@tuple.*`) — joined per match after
    // the loop (docs/adr/destructuring.md).
    let mut flow_slots: Vec<(usize, String, ScopeId, Point, usize)> = Vec::new();
    let mut slot_lists: HashMap<usize, (Span, usize, String)> = HashMap::new();
    let mut tuple_arr_by_match: HashMap<usize, Span> = HashMap::new();
    let mut tuple_elem_by_match: HashMap<usize, Span> = HashMap::new();
    let mut tuple_init_by_match: HashMap<usize, (usize, bool)> = HashMap::new();
    let mut tuple_keyed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // `@branch.expr` / `@branch.arm` (match / ternary) and `@subscript.*`,
    // joined per match after the loop.
    let mut branch_expr_by_match: HashMap<usize, Span> = HashMap::new();
    let mut branch_arm_by_match: HashMap<usize, Span> = HashMap::new();
    let mut subscript_by_match: HashMap<usize, (Span, Option<Span>, Option<i32>, Option<String>)> =
        HashMap::new();
    let mut annots: HashMap<usize, String> = HashMap::new();
    // keyed-shape collection: ctor + keys grouped per @expr.shape span
    let mut shape_spans: Vec<(usize, usize, Span)> = Vec::new();
    let mut shape_ctors: HashMap<(usize, usize), String> = HashMap::new();
    let mut shape_keys: Vec<(usize, usize, String)> = Vec::new();
    // command-dispatch collection: per match, the command identifier
    // and its ordered arguments
    let mut cmd_names: std::collections::BTreeMap<usize, (String, Span, crate::model::file_analysis::ScopeId)> =
        Default::default();
    let mut cmd_args: std::collections::BTreeMap<usize, Vec<(String, Span)>> = Default::default();
    // import-call halves, joined per match (BTreeMap: match ids are
    // source-ordered, so imports come out deterministic)
    let mut import_fns: std::collections::BTreeMap<usize, String> = Default::default();
    let mut import_args: std::collections::BTreeMap<usize, String> = Default::default();
    // expr-literal spans, for narrowing an Edge target onto the actual
    // literal when the rhs node wraps it.
    let mut lit_spans: Vec<(usize, usize, Span)> = Vec::new();
    // guard narrowing: match_id → (var, narrowed-type-text). A guard
    // (`isinstance(x, Foo)`, `ref $x eq 'Foo'`, `dynamic_cast<Foo>`) and
    // its guarded `@scope` share a match; when that scope is pushed we
    // emit a Variable witness scoped to the block — so the narrowed type
    // holds INSIDE the guard and nowhere else (scope = the refinement's
    // extent). The condition's captures precede the block in source, so
    // by the time the `@scope` event fires these are populated.
    let mut narrow_var: HashMap<usize, String> = HashMap::new();
    let mut narrow_type: HashMap<usize, String> = HashMap::new();
    let mut narrow_guard: HashMap<usize, String> = HashMap::new();
    // Recognized narrowings deferred to after flow-edge minting, so the region
    // cutoff can read the edges (`apply` below) — (subject, refined type, FULL
    // guarded-block region, block scope).
    let mut pending_narrow: Vec<(String, crate::model::file_analysis::InferredType, Span, ScopeId)> =
        Vec::new();
    // `std::move(x)` halves, joined per match: the qualifier (`std`) + name
    // (`move`) verify the call IS std::move (no query predicates), the var is
    // the moved subject, the call span the region start + enclosing scope.
    let mut move_scope_txt: HashMap<usize, String> = HashMap::new();
    let mut move_name_txt: HashMap<usize, String> = HashMap::new();
    let mut move_var_txt: HashMap<usize, String> = HashMap::new();
    let mut move_call: HashMap<usize, (Span, ScopeId)> = HashMap::new();
    // A context whose same-match `@scope` starts AFTER it (C++ namespace:
    // `@context` is on the name, `@scope` on the body `{`) must be
    // registered at the BODY depth, not the file depth — else it goes
    // sticky and leaks past the closing brace. Pre-index each match's
    // scope start; defer such contexts to the scope push.
    let mut scope_start_by_match: HashMap<usize, usize> = HashMap::new();
    for e in &events {
        if e.cap.starts_with("scope") {
            scope_start_by_match.entry(e.match_id).or_insert(e.start_byte);
        }
    }
    let mut pending_context: HashMap<usize, String> = HashMap::new();
    // A guard narrowing tags its consequence block `@narrow.block` (NOT `@scope`)
    // so the block mints exactly one scope from the general arm-scope pattern;
    // this maps a block's start byte → the narrow match, so when that arm @scope
    // is pushed we recover the guard's var/type/token by block position.
    let mut narrow_block_start: HashMap<usize, usize> = HashMap::new();
    for e in &events {
        if e.cap == "narrow.block" {
            narrow_block_start.entry(e.start_byte).or_insert(e.match_id);
        }
    }
    // Unevaluated-operand regions (`noexcept(...)`/`sizeof(...)`/`decltype(...)`):
    // a `std::move` whose call sits inside one is a type-trait, not a move.
    let unevaluated: Vec<(usize, usize)> = events
        .iter()
        .filter(|e| e.cap == "unevaluated")
        .map(|e| (e.start_byte, e.end_byte))
        .collect();
    // Control-flow construct spans (`@guard.region`): the use-after-move check
    // reads these to decide whether a move is straight-line in its scope.
    out.control_regions = events
        .iter()
        .filter(|e| e.cap == "guard.region")
        .map(|e| Span { start: e.start, end: e.end })
        .collect();
    // Parameter-list spans (`@param.region`): the use-after-move check reads
    // these to tell a moved parameter (not flagged) from a moved local.
    out.param_regions = events
        .iter()
        .filter(|e| e.cap == "param.region")
        .map(|e| Span { start: e.start, end: e.end })
        .collect();

    for e in &events {
        while scope_stack.len() > 1
            && scope_stack.last().is_some_and(|&(end, _)| e.start_byte >= end)
        {
            scope_stack.pop();
            while context_stack.last().is_some_and(|&(d, _)| d > scope_stack.len()) {
                context_stack.pop();
            }
        }
        let cur_scope = scope_stack.last().unwrap().1;
        let package: Option<String> = context_stack.last().map(|(_, p)| p.clone());
        match e.cap.as_str() {
            // `@scope` = a plain lexical Block; `@scope.sub` = sub-body
            // content (function bodies, prototype signatures, explicit
            // instantiations, requires-expressions) — the kind
            // `scope_within_sub_body` reads to shield params/locals from the
            // outline and the class-content lane. Pack subs carry no name on
            // the scope (the Symbol holds identity).
            "scope" | "scope.sub" => {
                let id = ScopeId(out.scopes.len() as u32);
                out.scopes.push(Scope {
                    id,
                    parent: Some(cur_scope),
                    kind: if e.cap == "scope.sub" {
                        ScopeKind::Sub { name: String::new() }
                    } else {
                        ScopeKind::Block
                    },
                    span: Span { start: e.start, end: e.end },
                    package: package.clone(),
                });
                scope_stack.push((e.end_byte, id));
                out.scope_count += 1;
                // a context deferred to THIS scope (C++ namespace) →
                // register at the body depth so it pops with the block.
                if let Some(text) = pending_context.remove(&e.match_id) {
                    while context_stack.last().is_some_and(|&(d, _)| d >= scope_stack.len()) {
                        context_stack.pop();
                    }
                    context_stack.push((scope_stack.len(), text));
                }
                // a guard narrowing whose block is THIS scope → the refined type
                // holds within `id` (invisible outside it). Two join shapes:
                // the narrow condition either shares this @scope's match (python:
                // `consequence: (block) @scope`), or tagged the block by position
                // (`@narrow.block`) so the refinement rides a general arm @scope
                // without a fragile duplicate (cpp if/else arms).
                let narrow_mid = narrow_block_start
                    .get(&e.start_byte)
                    .copied()
                    .filter(|nmid| narrow_var.contains_key(nmid))
                    .or_else(|| narrow_var.contains_key(&e.match_id).then_some(e.match_id));
                if let Some((nmid, var)) =
                    narrow_mid.and_then(|nmid| narrow_var.get(&nmid).map(|v| (nmid, v.clone())))
                {
                    let subject = (pack.shape_name)("ref.var", &var);
                    // Type text: the guard's own `@narrow.type` when it names one
                    // (`dynamic_cast<Derived*>`), else the subject's declared type
                    // (the optional-engagement form peels `T` from it). The guard
                    // token is absent for the bare `if (opt)` truthiness form.
                    // Resolve the subject's declared type up the guard's scope
                    // chain (innermost first), so a same-named var in a sibling
                    // function never supplies the inner type — the nearest
                    // enclosing declaration of `subject` wins.
                    let ty = narrow_type.get(&nmid).cloned().or_else(|| {
                        scope_stack.iter().rev().find_map(|&(_, sid)| {
                            annot_text_by_var.get(&(subject.clone(), sid)).cloned()
                        })
                    });
                    let guard = narrow_guard.get(&nmid).map(String::as_str);
                    if let Some(refined) = ty.and_then(|t| (pack.narrow_guard)(guard, &t)) {
                        // Defer: the region cutoff (first rebind edge) needs the
                        // FlowEdges, minted after this loop. Carry the FULL
                        // guarded-block region [start, end]; the post-pass
                        // truncates it at the earliest rebind.
                        pending_narrow.push((
                            subject,
                            refined,
                            Span { start: e.start, end: e.end },
                            id,
                        ));
                    }
                }
            }
            "narrow.var" => {
                narrow_var.insert(e.match_id, e.text.clone());
            }
            "narrow.type" => {
                narrow_type.insert(e.match_id, e.text.clone());
            }
            "narrow.guard" => {
                narrow_guard.insert(e.match_id, e.text.clone());
            }
            "move.scope" => {
                move_scope_txt.insert(e.match_id, e.text.clone());
            }
            "move.name" => {
                move_name_txt.insert(e.match_id, e.text.clone());
            }
            "move.var" => {
                move_var_txt.insert(e.match_id, e.text.clone());
            }
            "move.call" => {
                // Drop moves inside an unevaluated operand — they never execute,
                // so nothing is moved-from (rule #10: the property is "does this
                // move run", asked of the region, not a shape-branch downstream).
                let unevaluated_move = unevaluated
                    .iter()
                    .any(|&(s, en)| e.start_byte >= s && e.end_byte <= en);
                if !unevaluated_move {
                    move_call
                        .insert(e.match_id, (Span { start: e.start, end: e.end }, cur_scope));
                }
            }
            cap if cap.starts_with("context.") => {
                // Shape the context like a def name (cpp canonicalizes a
                // spec's template spelling) so members' `package` matches
                // the container Symbol's identity exactly.
                let text = (pack.shape_name)(&e.cap, &e.text);
                // If this match's `@scope` starts AFTER this context, the
                // context belongs to that (not-yet-pushed) body — defer it
                // so it registers at the body depth and pops with the block.
                if scope_start_by_match.get(&e.match_id).is_some_and(|&s| s > e.start_byte) {
                    pending_context.insert(e.match_id, text);
                } else {
                    // Replace any context at the same depth; deeper ones
                    // were already popped with their scopes.
                    while context_stack
                        .last()
                        .is_some_and(|&(d, _)| d >= scope_stack.len())
                    {
                        context_stack.pop();
                    }
                    context_stack.push((scope_stack.len(), e.text.clone()));
                }
            }
            "parent" => {
                // `@parent` (a base class) pairs with the `@def.class.name`
                // in the same match — record the inheritance edge.
                if let Some((child, _, _)) =
                    names_by_match.get(&(e.match_id, "def.class".to_string()))
                {
                    // Shaped like the child's def name (cpp canonicalizes a
                    // template-spelled base) so the edge joins the identity
                    // the target class was filed under.
                    let shaped = (pack.shape_name)("parent", &e.text);
                    if !pack.namespace_relative_parents {
                        out.parents.push((child.clone(), shaped));
                    } else {
                        // php name binding, most-specific first: a written
                        // qualifier is authoritative; else the file's
                        // use-map (an ALIAS resolves to the real leaf — the
                        // `use X as Y` edge was dead under the alias
                        // spelling); else the unqualified default IS the
                        // child's own namespace (PHP class names never fall
                        // through to global). Every edge records its
                        // namespace for FQ chain validation.
                        let (leaf, ns) = if let Some(fq) =
                            parent_fq_by_match.get(&e.match_id)
                        {
                            split_ns_leaf(fq)
                        } else if let Some((ns, real_leaf)) = use_map.get(shaped.as_str()) {
                            (real_leaf.clone(), ns.clone())
                        } else {
                            let ns = out
                                .symbols
                                .iter()
                                .rev()
                                .find(|s| s.kind == "class" && &s.name == child)
                                .and_then(|s| s.package.clone())
                                .unwrap_or_default();
                            (shaped, ns)
                        };
                        out.parents.push((child.clone(), leaf.clone()));
                        out.parent_namespaces.push((child.clone(), leaf, ns));
                    }
                }
            }
            // Hook-NAME identity (the Handler rail): a registration string
            // (`add_action('init', …)` arg 1) DECLARES the hook — a Handler
            // symbol whose name and span are the string content, stacking
            // like every same-named Handler. A firing string
            // (`do_action('init')`) mints the DispatchCall ref that matches
            // it. Both are Global-owned: the program shares one flat hook
            // namespace, no receiver.
            "def.handler.named" => {
                out.symbols.push(SkelSymbol {
                    name: e.text.clone(),
                    kind: "handler".to_string(),
                    start: e.start,
                    end: e.end,
                    name_start: e.start,
                    name_end: e.end,
                    package: None,
                    scope: cur_scope,
                    return_type: None,
                    receiver_instance_of: None,
                    receiver_return: false,
                    deref_stack: Vec::new(),
                    attributes: Vec::new(),
                    arity: None,
                    qualifier_owned: false,
                });
            }
            cap if cap.starts_with("def.") && !cap.ends_with(".name") => {
                let kind = cap.strip_prefix("def.").unwrap().to_string();
                let (name, name_start, name_end, defaulted) = names_by_match
                    .get(&(e.match_id, e.cap.clone()))
                    .cloned()
                    .map(|(n, s, en)| (n, s, en, false))
                    .or_else(|| {
                        (pack.default_name)(&kind)
                            .map(|n| (n.to_string(), e.start, e.start, true))
                    })
                    .unwrap_or((e.text.clone(), e.start, e.end, false));
                def_name_spans.push((e.start_byte, e.end_byte));
                // An out-of-line def's `Class::` qualifier names its owner
                // (the LAST `::` segment, the unqualified class the engine
                // keys by) — override the enclosing-namespace context.
                let pkg = qualifier_by_match
                    .get(&e.match_id)
                    .map(|q| q.rsplit("::").next().unwrap_or(q).to_string())
                    .or_else(|| package.clone());
                let shaped = (pack.shape_name)(&format!("def.{kind}"), &name);
                // A class-spec def carries its primary's name — the
                // (spec, primary) family edge `Specializes` derives from.
                if let Some(primary) = spec_primary_by_match.get(&e.match_id) {
                    out.specializations
                        .push((shaped.clone(), (pack.shape_name)("spec.primary", primary)));
                }
                // Registry field edge (pack-gated — see `field_registry_edges`
                // on `LangPack`): a data member's type lives as a Variable
                // witness in its declaring scope, and this edge lets a
                // property-access hop dispatch the field through the same
                // class-keyed chase methods use.
                // Foreach element peel: the loop var's value IS the
                // collection's uniform element, deferred to query time via
                // `Projected{base, Element}`. A simple-variable collection
                // bases on the Variable (its witnesses live on the decl
                // scope); anything else bases on the collection's own Expr
                // span, where a member-access hop or call witness already
                // answers (the same simple-vs-expression split the chain
                // hop's receiver makes).
                if kind == "var" {
                    let seq_join = seq_source_by_match
                        .get(&e.match_id)
                        .map(|sv| (sv, crate::model::witnesses::ProjectionStep::Element))
                        .or_else(|| {
                            seq_key_by_match
                                .get(&e.match_id)
                                .map(|sv| (sv, crate::model::witnesses::ProjectionStep::Key))
                        });
                    if let Some(((src_span, src_text), step)) = seq_join {
                        use crate::model::witnesses as wit;
                        let simple_var = src_text.starts_with('$')
                            && src_text[1..].chars().all(|c| c.is_alphanumeric() || c == '_');
                        let base = if simple_var {
                            wit::WitnessAttachment::Variable {
                                name: (pack.shape_name)("ref.var", src_text),
                                scope: cur_scope,
                            }
                        } else {
                            wit::WitnessAttachment::Expr(*src_span)
                        };
                        out.witnesses.push(wit::Witness {
                            attachment: wit::WitnessAttachment::Variable {
                                name: (pack.shape_name)("def.var", &name),
                                scope: cur_scope,
                            },
                            source: wit::WitnessSource::Builder("foreach_element".into()),
                            payload: wit::WitnessPayload::Projected { base, step },
                            // Zero-width at the decl: the binding types the
                            // var for its whole lifetime, so it must not be
                            // skipped as a narrowing fact scoped to the
                            // token (the same rule annot witnesses follow).
                            span: Span { start: e.start, end: e.start },
                        });
                    }
                }
                if kind == "field" && pack.field_registry_edges {
                    if let Some(cls) = &pkg {
                        use crate::model::witnesses as wit;
                        out.witnesses.push(wit::Witness {
                            attachment: wit::WitnessAttachment::PackageSymbol {
                                package: cls.clone(),
                                name: shaped.clone(),
                            },
                            // The tag IS the member's value shape: a
                            // `ValueHop` on this attachment prefers it, a
                            // `MethodHop` prefers the callable edges.
                            source: wit::WitnessSource::Builder(
                                crate::model::witnesses::FIELD_EDGE_SOURCE.into(),
                            ),
                            payload: wit::WitnessPayload::Edge(wit::WitnessAttachment::Variable {
                                name: shaped.clone(),
                                scope: cur_scope,
                            }),
                            span: Span { start: e.start, end: e.end },
                        });
                    }
                }
                out.symbols.push(SkelSymbol {
                    name: shaped,
                    kind,
                    start: e.start,
                    end: e.end,
                    name_start,
                    name_end,
                    package: pkg,
                    scope: cur_scope,
                    return_type: rettype_by_match
                        .get(&e.match_id)
                        .and_then(|t| (pack.annot_type)(t)),
                    receiver_instance_of: None,
                    receiver_return: rettype_by_match
                        .get(&e.match_id)
                        .is_some_and(|t| (pack.rettype_receiver)(t)),
                    deref_stack: nested_stacks.get(&e.match_id).cloned().unwrap_or_default(),
                    attributes: {
                        let mut a =
                            attrs_by_match.get(&e.match_id).cloned().unwrap_or_default();
                        // a default-named symbol is structure, not an
                        // addressable name — completion skips it.
                        if defaulted {
                            a.push("anonymous".to_string());
                        }
                        a
                    },
                    // Filled by span association in `into_file_analysis` — the
                    // `@arity.sig` match fires separately from this def name.
                    arity: None,
                    qualifier_owned: qualifier_by_match.contains_key(&e.match_id),
                });
            }
            "ref.label" => {
                out.label_refs.push((
                    (pack.shape_name)("def.label", &e.text),
                    cur_scope,
                    Span { start: e.start, end: e.end },
                ));
            }
            "member.recv" => {
                // Shaped: a pack can canonicalize a receiver spelling to the
                // model's invocant vocabulary (php `self::`/`static::` → the
                // current-package token), so relative static dispatch rides
                // the same lane as Perl's `__PACKAGE__->`.
                member_recv.insert(
                    e.match_id,
                    (
                        crate::model::file_analysis::Span { start: e.start, end: e.end },
                        (pack.shape_name)("member.recv", &e.text),
                    ),
                );
            }
            "member.op" => {
                // Map the operator token's KIND (== its text, an anonymous
                // token) to a MemberOp via the pack's open op_map. Unmapped
                // (`.*`) → no entry → no op-DX. No source-text re-decision.
                if let Some((_, op)) = pack.op_map.iter().find(|(k, _)| *k == e.text) {
                    member_op_raw.insert(
                        e.match_id,
                        (*op, crate::model::file_analysis::Span { start: e.start, end: e.end }),
                    );
                }
            }
            // String-named references (the tier-1 pack-plugin vocabulary,
            // docs/prompt-pack-plugins.md): the captured string-content
            // node's TEXT is the referenced name and its span IS the rename
            // unit — the characters inside the quotes. `@ref.call.named`
            // mints a FunctionCall ref (WP `add_action('init', 'wp_cron')`);
            // `@ref.method.named` mints a MethodCall ref joined to the same
            // match's `@member.recv` (`array($this, 'method')` callbacks),
            // so dispatch types through the receiver like any member ref.
            // No arg_count: the site registers the callee, it doesn't call
            // it — an arity hint here would misfeed arity discrimination.
            "ref.call.named" => {
                out.refs.push(SkelRef {
                    via: None,
                    kind: "call".to_string(),
                    name: e.text.clone(),
                    start: e.start,
                    end: e.end,
                    scope: cur_scope,
                    invocant: None,
                    member_op: None,
                    arg_count: None,
                    shape: crate::model::file_analysis::MemberShape::Unknown,
                });
            }
            "ref.dispatch.named" => {
                out.refs.push(SkelRef {
                    via: dispatch_via_by_match.get(&e.match_id).cloned(),
                    kind: "dispatch".to_string(),
                    name: e.text.clone(),
                    start: e.start,
                    end: e.end,
                    scope: cur_scope,
                    invocant: None,
                    member_op: None,
                    arg_count: None,
                    shape: crate::model::file_analysis::MemberShape::Unknown,
                });
            }
            // consumed by the prepass join above; nothing to mint here
            "dispatch.via" => {}
            // `.self` flavor: the string names a method of the ENCLOSING
            // class (a PHPUnit attribute argument) — no receiver node
            // exists, so the invocant is the current-package token,
            // resolved by the enclosing-class walk like `self::`.
            "ref.method.named.self" => {
                out.refs.push(SkelRef {
                    via: None,
                    kind: "member".to_string(),
                    name: e.text.clone(),
                    start: e.start,
                    end: e.end,
                    scope: cur_scope,
                    invocant: Some((
                        crate::model::file_analysis::Span { start: e.start, end: e.end },
                        "__PACKAGE__".to_string(),
                    )),
                    member_op: None,
                    arg_count: None,
                    shape: crate::model::file_analysis::MemberShape::Callable,
                });
            }
            "ref.method.named" => {
                if let Some(inv) = member_recv.get(&e.match_id).cloned() {
                    out.refs.push(SkelRef {
                    via: None,
                        kind: "member".to_string(),
                        name: e.text.clone(),
                        start: e.start,
                        end: e.end,
                        scope: cur_scope,
                        invocant: Some(inv),
                        member_op: None,
                        arg_count: None,
                        shape: crate::model::file_analysis::MemberShape::Callable,
                    });
                }
            }
            cap if cap.starts_with("ref.") => {
                // Generic suppression: a "reference" inside a def's own
                // header is the declaration, not a use. `ref.type` is exempt:
                // a prototype's RETURN type is the def node's first token
                // (`Widget make_widget();` starts at `Widget`), which is a
                // genuine use — its decl-name overlap is suppressed precisely
                // (exact selection-span match) in `into_file_analysis`.
                let inside_def = e.cap != "ref.type"
                    && def_name_spans
                        .iter()
                        .any(|&(s, en)| e.start_byte >= s && e.end_byte <= en && {
                            // only suppress when it IS the def name region
                            // (cheap heuristic: same start)
                            s == e.start_byte || en == e.end_byte
                        });
                if !inside_def {
                    let member_op = member_simple
                        .get(&e.match_id)
                        .copied()
                        .unwrap_or(false)
                        .then(|| member_op_raw.get(&e.match_id).copied())
                        .flatten();
                    // Reset-via-method: a rebinding method call on a simple-var
                    // receiver (`x.clear()`/`.reset()`/`.assign()`) puts a
                    // moved-from object back into a known state — a rebind. Mint
                    // a Rebind FlowEdge at the RECEIVER position so the moved-from
                    // window (and the narrowing cutoff) end there, sparing the
                    // receiver read itself. The pack owns which method names
                    // rebind (cpp vocab, like its op_map).
                    if e.cap == "ref.member"
                        && (pack.rebind_method)(&e.text)
                        && member_simple.get(&e.match_id).copied().unwrap_or(false)
                    {
                        if let Some((recv_span, recv_text)) = member_recv.get(&e.match_id) {
                            flow_rebinds.push((
                                (pack.shape_name)("def.var", recv_text),
                                cur_scope,
                                recv_span.start,
                            ));
                        }
                    }
                    // A SUPER receiver (php `parent::`) spells the model's
                    // SUPER method token: dispatch starts above the writing
                    // class, and gd/references/rename ride the existing
                    // SUPER lane. The invocant becomes the current-package
                    // token (the receiver is still this object); the ref
                    // span stays the bare name token, so rename rewrites
                    // only the name.
                    let super_recv = e.cap == "ref.member"
                        && member_recv
                            .get(&e.match_id)
                            .is_some_and(|(_, t)| (pack.super_receiver)(t));
                    out.refs.push(SkelRef {
                    via: None,
                        kind: e.cap.strip_prefix("ref.").unwrap().to_string(),
                        name: if super_recv {
                            format!("SUPER::{}", (pack.shape_name)(&e.cap, &e.text))
                        } else {
                            (pack.shape_name)(&e.cap, &e.text)
                        },
                        start: e.start,
                        end: e.end,
                        scope: cur_scope,
                        invocant: if super_recv {
                            member_recv
                                .get(&e.match_id)
                                .map(|(sp, _)| (*sp, "__PACKAGE__".to_string()))
                        } else {
                            member_recv.get(&e.match_id).cloned()
                        },
                        member_op,
                        // A call ref's arg list opens right where its callee /
                        // method token ends; plain (uncalled) member/type refs
                        // have no adjacent arg list and stay `None`.
                        arg_count: matches!(e.cap.as_str(), "ref.call" | "ref.qcall" | "ref.member")
                            .then(|| arg_counts_by_start.get(&(e.end.row, e.end.column)).copied())
                            .flatten(),
                        // A member token with an argument list right after it
                        // names a callable; without one it reads a value. Only
                        // member tokens carry the fact (a plain call is a
                        // callable by construction, a type ref neither).
                        shape: if e.cap == "ref.member" {
                            if arg_counts_by_start.contains_key(&(e.end.row, e.end.column)) {
                                crate::model::file_analysis::MemberShape::Callable
                            } else {
                                crate::model::file_analysis::MemberShape::Value
                            }
                        } else {
                            crate::model::file_analysis::MemberShape::Unknown
                        },
                    });
                    // The chain-hop witness: the whole call's value is
                    // "dispatch `member` on the receiver's class" — deferred
                    // to query time via `MethodHop`, so a receiver that is
                    // itself a call (`$a->b()->c()`) chains through its own
                    // hop witness at exactly the receiver span. (cpp mints
                    // via the dedicated `@hop.member` arm below — its ref
                    // pattern is call-blind, so the called form re-matches.)
                    if e.cap == "ref.member" && !super_recv {
                        if let (Some(call_span), Some((recv_span, recv_text))) = (
                            hop_call_by_match.get(&e.match_id),
                            member_recv.get(&e.match_id),
                        ) {
                            push_hop_witness(
                                &mut out.witnesses,
                                pack,
                                &e.text,
                                *call_span,
                                *recv_span,
                                recv_text,
                                member_simple.get(&e.match_id).copied().unwrap_or(false),
                                cur_scope,
                                arg_counts_by_start
                                    .get(&(e.end.row, e.end.column))
                                    .map(|n| *n as u32),
                                package.as_deref(),
                            );
                        }
                    }
                }
            }
            // cpp's called-member pattern: the ref was already minted by the
            // call-blind field pattern, so this arm mints ONLY the hop.
            "hop.member" => {
                if let (Some(call_span), Some((recv_span, recv_text))) = (
                    hop_call_by_match.get(&e.match_id),
                    member_recv.get(&e.match_id),
                ) {
                    push_hop_witness(
                        &mut out.witnesses,
                        pack,
                        &e.text,
                        *call_span,
                        *recv_span,
                        recv_text,
                        member_simple.get(&e.match_id).copied().unwrap_or(false),
                        cur_scope,
                        arg_counts_by_start
                            .get(&(e.end.row, e.end.column))
                            .map(|n| *n as u32),
                        package.as_deref(),
                    );
                }
            }
            "import.name" => {
                out.import_sites
                    .push((e.text.clone(), Span { start: e.start, end: e.end }));
                out.imports.push(e.text.clone());
            }
            cap if cap.starts_with("expr.lit.") => {
                let suffix = cap.strip_prefix("expr.lit.").unwrap();
                if let Some(t) = lit_type(suffix) {
                    let span = Span { start: e.start, end: e.end };
                    lit_spans.push((e.start_byte, e.end_byte, span));
                    out.witnesses.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Expr(span),
                        source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                        payload: crate::model::witnesses::WitnessPayload::InferredType(t),
                        span,
                    });
                }
            }
            "expr.read.var" => {
                // a variable READ is an edge: Expr(span) resolves to
                // whatever the Variable resolves to — same shape the
                // builder's emit_expr_witness uses.
                let span = Span { start: e.start, end: e.end };
                // …and a candidate local-var reference, resolved to its
                // declaration in into_file_analysis (goto-def + hover).
                out.var_reads.push((
                    (pack.shape_name)("ref.var", &e.text),
                    cur_scope,
                    span,
                ));
                out.witnesses.push(crate::model::witnesses::Witness {
                    attachment: crate::model::witnesses::WitnessAttachment::Expr(span),
                    source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                    payload: crate::model::witnesses::WitnessPayload::Edge(
                        crate::model::witnesses::WitnessAttachment::Variable {
                            name: (pack.shape_name)("ref.var", &e.text),
                            scope: cur_scope,
                        },
                    ),
                    span,
                });
            }
            "expr.return.value" => {
                // The returned expression's own general-rule witness (literal
                // / var-read / member / call — whichever matched this same
                // node) already carries its type; this just records the site
                // (scope + span) so `emit_return_fuel` (language_driver.rs,
                // phase 7) can chain the enclosing function's `Symbol` onto it
                // when undeclared.
                out.return_sites
                    .push((cur_scope, Span { start: e.start, end: e.end }));
            }
            "flow.slot" => {
                flow_slots.push((
                    e.match_id,
                    (pack.shape_name)("def.var", &e.text),
                    cur_scope,
                    e.start,
                    e.start_byte,
                ));
            }
            "flow.slot.list" => {
                slot_lists.insert(
                    e.match_id,
                    (Span { start: e.start, end: e.end }, e.start_byte, e.text.clone()),
                );
            }
            "tuple.arr" => {
                tuple_arr_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "tuple.elem" => {
                tuple_elem_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "tuple.init" => {
                tuple_init_by_match
                    .insert(e.match_id, (e.start_byte, e.text.trim_start().starts_with("...")));
            }
            "tuple.keyed" => {
                tuple_keyed.insert(e.match_id);
            }
            "branch.expr" => {
                branch_expr_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "branch.arm" => {
                branch_arm_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "subscript.expr" => {
                subscript_by_match
                    .entry(e.match_id)
                    .or_insert((Span { start: e.start, end: e.end }, None, None, None))
                    .0 = Span { start: e.start, end: e.end };
            }
            "subscript.base" => {
                subscript_by_match
                    .entry(e.match_id)
                    .or_insert((Span { start: e.start, end: e.end }, None, None, None))
                    .1 = Some(Span { start: e.start, end: e.end });
            }
            "subscript.int" => {
                subscript_by_match
                    .entry(e.match_id)
                    .or_insert((Span { start: e.start, end: e.end }, None, None, None))
                    .2 = e.text.trim().parse::<i32>().ok();
            }
            "subscript.key" => {
                subscript_by_match
                    .entry(e.match_id)
                    .or_insert((Span { start: e.start, end: e.end }, None, None, None))
                    .3 = Some(e.text.clone());
            }
            "flow.target" => {
                flow_targets.insert(
                    e.match_id,
                    ((pack.shape_name)("def.var", &e.text), cur_scope, e.start),
                );
                // Record the declared-type text keyed by (var, DECLARING scope)
                // for the token-less optional-engagement narrowing. cur_scope is
                // only known here (scopes mint during this walk); the type-annot
                // half was pre-collected (it precedes the declarator in source).
                if let Some(annot) = annot_by_match.get(&e.match_id) {
                    annot_text_by_var.insert(
                        ((pack.shape_name)("ref.var", &e.text), cur_scope),
                        annot.clone(),
                    );
                }
            }
            "flow.rebind" => {
                flow_rebinds.push(((pack.shape_name)("def.var", &e.text), cur_scope, e.start));
            }
            "anonagg.member" => {
                // A field typed by an anonymous aggregate: its members are
                // flattened onto the enclosing named container, so the field's
                // own type IS that container (the anon hop is identity) —
                // `u->data.ping` types `u->data` as U and finds `ping` there.
                // TypeName (not ClassName) so a typedef'd container chases.
                if let Some(owner) = &package {
                    out.witnesses.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Variable {
                            name: (pack.shape_name)("def.var", &e.text),
                            scope: cur_scope,
                        },
                        source: crate::model::witnesses::WitnessSource::Builder(
                            "skeleton-anon-agg".into(),
                        ),
                        payload: crate::model::witnesses::WitnessPayload::Edge(
                            crate::model::witnesses::WitnessAttachment::TypeName(owner.clone()),
                        ),
                        span: Span { start: e.start, end: e.start },
                    });
                }
            }
            "flow.source" => {
                flow_sources.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "type.annot" => {
                annots.insert(e.match_id, e.text.clone());
            }
            "domain.slot" => {
                // A field slot used against a typed value — one domain-typing
                // site. The value is joined by match_id; its enum resolves
                // cross-file at query time. The span is the SLOT's, so a
                // find-references on the enum surfaces the field's own uses.
                if let Some(value) = domain_value_by_match.get(&e.match_id) {
                    out.domain_sites.push(crate::model::file_analysis::DomainSite {
                        slot: (pack.shape_name)("ref.member", &e.text),
                        value: value.clone(),
                        slot_span: Span { start: e.start, end: e.end },
                    });
                }
            }
            "expr.shape" => {
                shape_spans.push((e.start_byte, e.end_byte, Span { start: e.start, end: e.end }));
            }
            "shape.ctor" => {
                // belongs to the smallest enclosing expr.shape; matches
                // share the call node so byte keys line up
                shape_ctors
                    .entry(byte_range_of(&events, e.match_id, "expr.shape").unwrap_or((0, 0)))
                    .or_insert_with(|| e.text.clone());
            }
            "shape.key" => {
                if let Some(range) = byte_range_of(&events, e.match_id, "expr.shape") {
                    shape_keys.push((range.0, range.1, e.text.clone()));
                }
            }
            "cmd" => {
                cmd_names.insert(
                    e.match_id,
                    (e.text.clone(), Span { start: e.start, end: e.end }, cur_scope),
                );
            }
            "cmd.arg" => {
                cmd_args
                    .entry(e.match_id)
                    .or_default()
                    .push((e.text.clone(), Span { start: e.start, end: e.end }));
            }
            "import.fn" => {
                import_fns.insert(e.match_id, e.text.clone());
            }
            "import.arg" => {
                import_args.insert(e.match_id, e.text.clone());
            }
            cap if cap.starts_with("obs.") => {
                // Usage-site evidence: a mono-typed operator observes
                // its operand. Same Observation payloads the Perl
                // walker emits; the fold is the production one.
                let obs = match cap.strip_prefix("obs.").unwrap() {
                    "numeric" => Some(crate::model::witnesses::TypeObservation::NumericUse),
                    "string" => Some(crate::model::witnesses::TypeObservation::StringUse),
                    _ => None,
                };
                if let Some(o) = obs {
                    let span = Span { start: e.start, end: e.end };
                    out.witnesses.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Variable {
                            name: (pack.shape_name)("ref.var", &e.text),
                            scope: cur_scope,
                        },
                        source: crate::model::witnesses::WitnessSource::Builder("skeleton-obs".into()),
                        payload: crate::model::witnesses::WitnessPayload::Observation(o),
                        span,
                    });
                }
            }
            "expr.ctor" => {
                // `new X(...)`: the value IS an instance of X — a structural
                // fact (the ctor syntax names a class by definition), NOT the
                // name-case guess the macro rule forbids for bare calls. Edge
                // into the alias graph: `TypeName` recurses into the defining
                // file when an index is in hand and terminates at
                // `ClassName(X)` otherwise, so a cross-file `new WP_Query()`
                // types the variable with zero local knowledge.
                let callee = events
                    .iter()
                    .find(|x| x.match_id == e.match_id && x.cap == "ref.call")
                    .map(|x| (pack.shape_name)("ref.call", &x.text));
                if let Some(name) = callee {
                    let span = Span { start: e.start, end: e.end };
                    lit_spans.push((e.start_byte, e.end_byte, span));
                    // `new self()` / `new static()`: the class is the
                    // ENCLOSING one (the pack's `hop.recv` shaping names
                    // the current-class spellings) — a bare `TypeName`
                    // edge would chase a class literally named "self".
                    let payload = if crate::model::conventions::is_current_package_token(
                        &(pack.shape_name)("hop.recv", &name),
                    ) {
                        // `self` outside a class (invalid source) has no
                        // enclosing class — mint nothing.
                        package.as_ref().map(|cls| {
                            crate::model::witnesses::WitnessPayload::InferredType(
                                InferredType::ClassName(cls.clone()),
                            )
                        })
                    } else {
                        Some(crate::model::witnesses::WitnessPayload::Edge(
                            crate::model::witnesses::WitnessAttachment::TypeName(name),
                        ))
                    };
                    if let Some(payload) = payload {
                        out.witnesses.push(crate::model::witnesses::Witness {
                            attachment: crate::model::witnesses::WitnessAttachment::Expr(span),
                            source: crate::model::witnesses::WitnessSource::Builder(
                                "skeleton-ctor".into(),
                            ),
                            payload,
                            span,
                        });
                    }
                }
            }
            "expr.call" => {
                // A call's VALUE is the callee's own resolution — deferred to
                // `into_file_analysis`, where the symbol table is known: a
                // `Class` callee is a functional cast / constructor, a callable
                // flows its return, an unresolvable name types nothing. Record
                // (span, callee) and mark the span as a value-producing site so
                // an enclosing `auto x = f(..)` flow edge targets it; the type
                // witness is minted later once the callee resolves (no name-case
                // guess). `docs/adr/macro-handling.md`.
                let callee = events
                    .iter()
                    .find(|x| x.match_id == e.match_id && x.cap == "ref.call")
                    .map(|x| x.text.clone());
                if let Some(callee) = callee {
                    let span = Span { start: e.start, end: e.end };
                    lit_spans.push((e.start_byte, e.end_byte, span));
                    out.call_sites.push((span, callee));
                }
            }
            _ => {}
        }
    }

    // ---- inline namespaces: tag the Package symbol by name span ----
    if !inline_ns_spans.is_empty() {
        let same = |a: Point, b: Point| a.row == b.row && a.column == b.column;
        for s in out.symbols.iter_mut() {
            if s.kind == "package"
                && inline_ns_spans
                    .iter()
                    .any(|&(st, en)| same(st, s.name_start) && same(en, s.name_end))
            {
                s.attributes.push("inline".to_string());
            }
        }
    }

    // ---- keyed shapes → HashWithKeys witnesses ----
    {
        let mut seen_spans: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        for &(sb, eb, span) in &shape_spans {
            if !seen_spans.insert((sb, eb)) {
                continue;
            }
            let Some(ctor) = shape_ctors.get(&(sb, eb)) else { continue };
            if !(pack.shape_ctor)(ctor) {
                continue;
            }
            let mut keys: Vec<(String, Option<Box<InferredType>>)> = shape_keys
                .iter()
                .filter(|&&(s2, e2, _)| s2 == sb && e2 == eb)
                .map(|(_, _, k)| (k.clone(), None))
                .collect();
            keys.dedup_by(|a, b| a.0 == b.0);
            lit_spans.push((sb, eb, span));
            out.witnesses.push(crate::model::witnesses::Witness {
                attachment: crate::model::witnesses::WitnessAttachment::Expr(span),
                source: crate::model::witnesses::WitnessSource::Builder("skeleton-shape".into()),
                payload: crate::model::witnesses::WitnessPayload::InferredType(
                    InferredType::HashWithKeys { keys: crate::model::file_analysis::SharedKeys::new(keys), open: false },
                ),
                span,
            });
        }
    }

    // ---- import CALLS (library/source) → imports ----
    for (mid, f) in &import_fns {
        if let Some(arg) = import_args.get(mid) {
            if let Some(module) = (pack.import_call)(f, arg) {
                if !out.imports.contains(&module) {
                    out.imports.push(module);
                }
            }
        }
    }

    // ---- def dedup: `f <- function` matches both the sub and the
    // generic var pattern (keep the more specific kind per name site), and
    // a trailing-return function matches both its leading-`auto` pattern
    // and the trailing sibling (keep the rettype-bearing copy) ----
    {
        // Keyed per name site AND per field-ness: a framework overlay
        // legitimately declares a PROPERTY at a method's own name token
        // (Eloquent relations — `pages()` the method, `->pages` the
        // accessor), and that pair must survive while the same-kind
        // duplicates (var vs sub, rettype twins) still collapse.
        let mut best: HashMap<(usize, usize, bool), usize> = HashMap::new();
        let mut keep = vec![true; out.symbols.len()];
        for (i, sym) in out.symbols.iter().enumerate() {
            let key = (sym.name_start.row, sym.name_start.column, sym.kind == "field");
            match best.get(&key) {
                None => {
                    best.insert(key, i);
                }
                Some(&j) => {
                    let (gen_i, gen_j) =
                        (out.symbols[i].kind == "var", out.symbols[j].kind == "var");
                    let upgrade_ret = out.symbols[i].kind == out.symbols[j].kind
                        && out.symbols[i].return_type.is_some()
                        && out.symbols[j].return_type.is_none();
                    if (gen_j && !gen_i) || upgrade_ret {
                        keep[j] = false;
                        best.insert(key, i);
                    } else {
                        keep[i] = false;
                    }
                }
            }
        }
        let mut it = keep.iter();
        out.symbols.retain(|_| *it.next().unwrap());
    }

    // ---- command dispatch: classify each command's effects ----
    for (mid, (cmd, cmd_span, scope)) in &cmd_names {
        let args = cmd_args.get(mid).cloned().unwrap_or_default();
        // every invocation identifier is a call ref (user functions
        // rename through it; builtin names match no defs, harmlessly)
        out.refs.push(SkelRef {
                    via: None,
            kind: "call".into(),
            name: cmd.clone(),
            start: cmd_span.start,
            end: cmd_span.end,
            scope: *scope,
            invocant: None,
            member_op: None,
            arg_count: Some(args.len()),
            shape: crate::model::file_analysis::MemberShape::Unknown,
        });
        for effect in (pack.cmd_effects)(cmd) {
            match effect {
                CmdEffect::Def { kind, name_arg } => {
                    if let Some((name, span)) = args.get(name_arg) {
                        out.symbols.push(SkelSymbol {
                            kind: kind.to_string(),
                            name: name.clone(),
                            start: cmd_span.start,
                            end: span.end,
                            name_start: span.start,
                            name_end: span.end,
                            package: None,
                            scope: *scope,
                            return_type: None,
                            receiver_return: false,
            receiver_instance_of: None,
                            deref_stack: Vec::new(),
                            attributes: Vec::new(),
                            arity: None,
                            qualifier_owned: false,
                        });
                    }
                }
                CmdEffect::RefArgsFrom { from } => {
                    for (name, span) in args.iter().skip(from) {
                        let is_keyword =
                            !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_');
                        if !is_keyword && !name.contains("${") {
                            out.refs.push(SkelRef {
                    via: None,
                                kind: "call".into(),
                                name: name.clone(),
                                start: span.start,
                                end: span.end,
                                scope: *scope,
                                invocant: None,
                                member_op: None,
                                arg_count: None,
                                shape: crate::model::file_analysis::MemberShape::Unknown,
                            });
                        }
                    }
                }
                CmdEffect::Import { arg } => {
                    if let Some((name, _)) = args.get(arg) {
                        if !out.imports.contains(name) {
                            out.imports.push(name.clone());
                        }
                    }
                }
            }
        }
    }

    // ---- typedef / using aliases → TypeName witnesses (the alias graph) ----
    // `typedef unsigned short U16` / `using U16 = unsigned short` push
    // `TypeName("U16") → <underlying>`: a primitive leaf stays an
    // `InferredType`; a class-shaped underlying edges to `TypeName(that)` so
    // an alias chain (`typedef V16 W16`) chases; an unrecognized leaf spelling
    // (`unsigned short` — has a space) is `ClassName(text)` so hover shows the
    // raw spelling. Struct/union/enum tag typedefs (`typedef struct op OP`)
    // don't reach here — the skeleton's @parent edge already aliases them.
    // Emit in match-id (≈ source) order: two `#define`s / typedefs of the
    // SAME alias name (a config-variant type macro like `PERL_BITFIELD16`,
    // guarded `#ifdef … #else …`) land competing `TypeName(alias)` witnesses,
    // and the reducer is latest-wins — a HashMap-iteration emission order
    // makes the winner flip per process (Rust's randomized hasher). Sorted
    // emission fixes the winner to the last-defined variant, deterministically.
    let mut alias_mids: Vec<&usize> = alias_name_by_match.keys().collect();
    alias_mids.sort_unstable();
    for mid in alias_mids {
        let alias = &alias_name_by_match[mid];
        let Some(underlying) = alias_of_by_match.get(mid) else { continue };
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::TypeName(alias.clone()),
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-typedef".into()),
            payload: type_alias_payload(underlying.trim(), pack.annot_type),
            span: Span { start: Point { row: 0, column: 0 }, end: Point { row: 0, column: 0 } },
        });
    }

    // ---- object-like `#define X body` type aliases → TypeName witnesses ----
    // Same alias graph as a typedef, so a field/var typed `PERL_BITFIELD16`
    // (defined cross-file in another header via a config-guarded `#define`)
    // chases through to its integer leaf. Gated on a TYPE-shaped body so the
    // sea of value macros (`#define MAX 100`) mints nothing.
    // Same deterministic-emission rule as the typedef loop above (the
    // `PERL_BITFIELD16` config-variant JOIN lives here): sorted by match id so
    // the latest-wins reducer's winner is order-independent.
    let mut macro_mids: Vec<&usize> = macro_alias_name_by_match.keys().collect();
    macro_mids.sort_unstable();
    for mid in macro_mids {
        let alias = &macro_alias_name_by_match[mid];
        let Some(underlying) = macro_alias_of_by_match.get(mid) else { continue };
        let underlying = underlying.trim();
        if !looks_like_type_spelling(underlying) {
            continue;
        }
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::TypeName(alias.clone()),
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-macro-alias".into()),
            payload: type_alias_payload(underlying, pack.annot_type),
            span: Span { start: Point { row: 0, column: 0 }, end: Point { row: 0, column: 0 } },
        });
    }

    // ---- join flow captures into Variable witnesses ----
    // Match-id order (deterministic) — two captures targeting the same
    // `Variable{name, scope}` slot would otherwise land witnesses in
    // HashMap-iteration order, flipping the latest-wins winner per process.
    // Branch arms (match / ternary): the expression's value is its arms'
    // AGREEMENT (`BranchArmFold`), never a literal found inside it.
    {
        let mut seen_expr: std::collections::HashSet<(Point, Point)> = Default::default();
        for (mid, arm) in &branch_arm_by_match {
            let Some(expr) = branch_expr_by_match.get(mid) else { continue };
            if seen_expr.insert((expr.start, expr.end)) {
                out.witnesses.push(crate::model::witnesses::Witness {
                    attachment: crate::model::witnesses::WitnessAttachment::Expr(*expr),
                    source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                    payload: crate::model::witnesses::WitnessPayload::Edge(
                        crate::model::witnesses::WitnessAttachment::BranchArm(*expr),
                    ),
                    span: *expr,
                });
            }
            out.witnesses.push(crate::model::witnesses::Witness {
                attachment: crate::model::witnesses::WitnessAttachment::BranchArm(*expr),
                source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                payload: crate::model::witnesses::WitnessPayload::Edge(
                    crate::model::witnesses::WitnessAttachment::Expr(*arm),
                ),
                span: *arm,
            });
        }
    }
    let mut flow_mids: Vec<&usize> = flow_targets.keys().collect();
    flow_mids.sort_unstable();
    // A member-expression rhs (`$q->where('a')`, `w.get()`) is a
    // value-producing site whose value is NOT any literal inside it (that's
    // an argument). The literal narrowing below exists for transparent
    // wrappers only — when a member ref's invocant opens exactly at the
    // source span, the rhs IS the member expression and the edge must stay
    // on the full span (the member-chain arm / MCB lane resolve it).
    let member_anchored: std::collections::HashSet<(usize, usize)> = out
        .refs
        .iter()
        .filter(|r| r.kind == "member")
        .filter_map(|r| r.invocant.as_ref().map(|(s, _)| (s.start.row, s.start.column)))
        .collect();
    // A class/struct DATA MEMBER is visible throughout its class body
    // regardless of declaration order (C++ member lookup is not sequential:
    // a method reads a field declared later in a `private:` section below
    // it). The type-witness temporal filter is position-based — it admits a
    // witness only at/after its span — so a zero-width witness pinned to the
    // field's decl point would be REJECTED for every read textually above it.
    // Give the field's declared-type witness its class-body scope's span so
    // "this type holds class-wide" is what the filter sees. Locals keep their
    // decl-point span (flow narrowing is sequential and correct for them).
    // Scopes are pushed in id order (`id = ScopeId(scopes.len())`), so a
    // ScopeId indexes its own scope directly.
    let field_scope_span: std::collections::HashMap<(String, ScopeId), Span> = out
        .symbols
        .iter()
        .filter(|s| s.kind == "field")
        .filter_map(|s| {
            out.scopes
                .get(s.scope.0 as usize)
                .map(|sc| ((s.name.clone(), s.scope), sc.span))
        })
        .collect();
    for mid in flow_mids {
        let (name, scope, at) = &flow_targets[mid];
        let var = crate::model::witnesses::WitnessAttachment::Variable {
            name: name.clone(),
            scope: *scope,
        };
        // Class-wide extent for a data member; the sequential decl point for a
        // local (see `field_scope_span`).
        let annot_span = field_scope_span
            .get(&(name.clone(), *scope))
            .copied()
            .unwrap_or(Span { start: *at, end: *at });
        if let Some(annot) = annots.get(mid) {
            // A class-shaped declared type edges into the alias graph (it may
            // be a typedef — `U16 x;` where `typedef unsigned short U16`);
            // primitives stay leaves; `None` (auto/void) defers to the flow
            // edge as before. `TypeName` chases the typedef or falls back to
            // the same `ClassName`, so a plain struct/class is unchanged.
            let payload = match (pack.annot_type)(annot) {
                Some(InferredType::ClassName(cn)) => Some(
                    crate::model::witnesses::WitnessPayload::Edge(
                        crate::model::witnesses::WitnessAttachment::TypeName(cn),
                    ),
                ),
                Some(t) => Some(crate::model::witnesses::WitnessPayload::InferredType(t)),
                None => None,
            };
            if let Some(payload) = payload {
                out.witnesses.push(crate::model::witnesses::Witness {
                    attachment: var.clone(),
                    source: crate::model::witnesses::WitnessSource::Builder(crate::model::witnesses::ANNOT_SOURCE.into()),
                    payload,
                    span: annot_span,
                });
            }
        }
        if let Some(src_span) = flow_sources.get(mid) {
            // Narrow onto the outermost literal the rhs wraps, when the
            // rhs node itself carries no witness (paren wrappers) — but
            // never into a member expression's argument (see
            // `member_anchored` above), and never when the rhs span
            // ALREADY carries its own witness: a chain off a ctor
            // receiver (`$x = (new W())->c()`) has the ctor as its
            // largest inner literal, and narrowing onto it would hand
            // the variable the receiver's class instead of the call's
            // value (the rhs's own hop witness).
            let src_bytes = byte_range_of(&events, *mid, "flow.source");
            let rhs_has_own_witness = out.witnesses.iter().any(|w| {
                matches!(
                    &w.attachment,
                    crate::model::witnesses::WitnessAttachment::Expr(sp)
                        if sp.start == src_span.start && sp.end == src_span.end
                )
            });
            let target_span = if rhs_has_own_witness
                || member_anchored
                    .contains(&(src_span.start.row, src_span.start.column))
            {
                *src_span
            } else {
                lit_spans
                    .iter()
                    .filter(|&&(s, en, _)| {
                        src_bytes.is_some_and(|(ss, se)| s >= ss && en <= se)
                    })
                    .max_by_key(|&&(s, en, _)| en - s)
                    .map(|&(_, _, sp)| sp)
                    .filter(|sp| sp != src_span)
                    .unwrap_or(*src_span)
            };
            // Mint a value-flow edge (cpp init is `Whole`); the witness is its
            // lowering, so type inference sees the same `Variable → Edge(Expr)`
            // it always did — now with the source span kept for provenance.
            out.flow_edges.push(crate::model::file_analysis::FlowEdge {
                target_name: name.clone(),
                target_scope: *scope,
                target_at: *at,
                source: target_span,
                extraction: crate::model::file_analysis::Extraction::Whole,
            });
        }
    }
    // Bind-shape rebinds (loop vars): no inflowing value, recorded for the
    // narrowing cutoff (`Rebind` lowers to nothing — provenance only).
    for (name, scope, at) in flow_rebinds {
        out.flow_edges.push(crate::model::file_analysis::FlowEdge {
            target_name: name,
            target_scope: scope,
            target_at: at,
            source: Span { start: at, end: at },
            extraction: crate::model::file_analysis::Extraction::Rebind,
        });
    }
    // Destructuring slots bind POSITIONALLY off their source — the same
    // FlowEdge lowering Perl's list assignment uses. A keyed list never
    // binds (its positions are not positions); the defs still landed.
    {
        let mut element_hops: std::collections::HashSet<(Point, Point)> = Default::default();
        for (mid, name, scope, at, byte) in &flow_slots {
            let Some((list_span, list_byte, list_text)) = slot_lists.get(mid) else { continue };
            let offset = byte.saturating_sub(*list_byte);
            let extraction = match slot_position(list_text, offset) {
                Some(pos) => crate::model::file_analysis::Extraction::Positional(pos),
                None => match slot_key(list_text, offset) {
                    Some(k) => crate::model::file_analysis::Extraction::KeyOf(k),
                    None => continue,
                },
            };
            let source = if let Some(src) = flow_sources.get(mid) {
                *src
            } else if let Some((seq_src, _)) = seq_source_by_match.get(mid) {
                // foreach: the list IS the collection's element; the slots
                // index into it — two projections chained through the
                // list's own Expr span.
                if element_hops.insert((list_span.start, list_span.end)) {
                    out.witnesses.push(crate::model::witnesses::Witness {
                        attachment: crate::model::witnesses::WitnessAttachment::Expr(*list_span),
                        source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                        payload: crate::model::witnesses::WitnessPayload::Projected {
                            base: crate::model::witnesses::WitnessAttachment::Expr(*seq_src),
                            step: crate::model::witnesses::ProjectionStep::Element,
                        },
                        span: *list_span,
                    });
                }
                *list_span
            } else {
                continue;
            };
            out.flow_edges.push(crate::model::file_analysis::FlowEdge {
                target_name: name.clone(),
                target_scope: *scope,
                target_at: *at,
                source,
                extraction,
            });
        }
    }
    // Key-less array literals are positional TUPLES of their elements'
    // edges (`return [$queue, $agent]`); a keyed element or a spread makes
    // the literal a map / open list — the tuple witness is withheld and the
    // `expr.lit.hashref` / keyed-shape witnesses stand.
    {
        let mut by_arr: HashMap<(Point, Point), (Span, Vec<(usize, Span)>, bool)> = HashMap::new();
        for (mid, arr_span) in &tuple_arr_by_match {
            let entry = by_arr
                .entry((arr_span.start, arr_span.end))
                .or_insert((*arr_span, Vec::new(), false));
            if tuple_keyed.contains(mid) {
                entry.2 = true;
                continue;
            }
            if let (Some(elem), Some((byte, spread))) =
                (tuple_elem_by_match.get(mid), tuple_init_by_match.get(mid))
            {
                if *spread {
                    entry.2 = true;
                    continue;
                }
                entry.1.push((*byte, *elem));
            }
        }
        const MAX_TUPLE: usize = 64;
        for (_, (arr_span, mut elems, disqualified)) in by_arr {
            if disqualified || elems.is_empty() || elems.len() > MAX_TUPLE {
                continue;
            }
            elems.sort_by_key(|(b, _)| *b);
            out.witnesses.push(crate::model::witnesses::Witness {
                attachment: crate::model::witnesses::WitnessAttachment::Expr(arr_span),
                source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
                payload: crate::model::witnesses::WitnessPayload::Tuple(
                    elems
                        .into_iter()
                        .map(|(_, s)| crate::model::witnesses::WitnessAttachment::Expr(s))
                        .collect(),
                ),
                span: arr_span,
            });
        }
    }
    // Subscripts project off their base: an integer index peels a slot, a
    // literal string key drills a keyed shape — the same `Projected` steps
    // the foreach/destructuring binders ride.
    for (expr, base, idx, key) in subscript_by_match.values() {
        let Some(base) = base else { continue };
        let step = match (idx, key) {
            (Some(i), _) => crate::model::witnesses::ProjectionStep::ArrayIndex(*i),
            (None, Some(k)) => crate::model::witnesses::ProjectionStep::HashKey(k.clone()),
            _ => continue,
        };
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Expr(*expr),
            source: crate::model::witnesses::WitnessSource::Builder("skeleton".into()),
            payload: crate::model::witnesses::WitnessPayload::Projected {
                base: crate::model::witnesses::WitnessAttachment::Expr(*base),
                step,
            },
            span: *expr,
        });
    }
    // Lower the value-flow edges to type-tier witnesses (the bag is canonical
    // for types; the edges are the provenance tier above it).
    for fe in &out.flow_edges {
        if let Some(w) = fe.lower_to_witness() {
            out.witnesses.push(w);
        }
    }
    // Narrowing cutoffs (THE cross-language lift): truncate each guarded region
    // at the first FlowEdge that rebinds the subject — the SAME edge-driven
    // cutoff the Perl narrowing uses (`earliest_rebind_in`). Deferred to here so
    // the edges exist. The witness gets a REAL region span [start, cutoff], so
    // point-containment ends the narrowing at the rebind — the soundness Perl
    // got from its cutoff, now generic. Every LangPack that narrows (python
    // isinstance, cpp dynamic_cast + optional engagement) gets it free.
    for (name, refined, region, scope) in pending_narrow {
        let end = crate::model::file_analysis::earliest_rebind_in(&out.flow_edges, &name, region)
            .unwrap_or(region.end);
        if (region.start.row, region.start.column) >= (end.row, end.column) {
            continue; // rebound before the region even opens — nothing holds
        }
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable { name, scope },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-narrow".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(refined),
            span: Span { start: region.start, end },
        });
    }
    // ---- std::move sites → moved-from facts. The qualifier/name verify the
    // call IS `std::move` here (no query predicates), so the diagnostic never
    // sees the call shape — it reads the recorded var + span + scope.
    for (mid, (span, scope)) in &move_call {
        if move_scope_txt.get(mid).map(String::as_str) == Some("std")
            && move_name_txt.get(mid).map(String::as_str) == Some("move")
        {
            if let Some(v) = move_var_txt.get(mid) {
                out.moved_from.push(((pack.shape_name)("ref.var", v), *span, *scope));
            }
        }
    }
    // ---- documentation-comment types: pack vocabulary, positional join ----
    // A doc comment documents the def that STARTS on the line directly below
    // its last line (an attribute/modifier line between them breaks the join —
    // accepted v1). DECLARED types always win: a doc fact fills only where
    // the syntax carried nothing, because docblocks drift and the tree
    // doesn't. Perl/C++ packs return no facts, so the pass is a no-op there.
    {
        use crate::build::query_extract::DocFact;
        // Keyed by the comment's END row (the def sits on the next line);
        // the start row rides along so a `@method` fact can span its own
        // line inside the comment.
        let mut by_end_row: HashMap<usize, (usize, Vec<DocFact>)> = HashMap::new();
        for e in &events {
            if e.cap == "doc.comment" {
                let facts = (pack.doc_types)(&e.text);
                if !facts.is_empty() {
                    let entry = by_end_row
                        .entry(e.end.row)
                        .or_insert_with(|| (e.start.row, Vec::new()));
                    entry.1.extend(facts);
                }
            }
        }
        if !by_end_row.is_empty() {
            let scope_spans: Vec<Span> = out.scopes.iter().map(|s| s.span).collect();
            let param_syms: Vec<(String, crate::model::file_analysis::ScopeId, Point)> =
                out.symbols
                    .iter()
                    .filter(|s| s.kind == "var")
                    .map(|s| (s.name.clone(), s.scope, s.start))
                    .collect();
            let mut doc_witnesses: Vec<crate::model::witnesses::Witness> = Vec::new();
            let mut doc_refs: Vec<SkelRef> = Vec::new();
            let mut doc_methods: Vec<SkelSymbol> = Vec::new();
            for sym in out.symbols.iter_mut() {
                // `@method` rows join to the CLASS docblock (Laravel facades,
                // Eloquent's `__call` surface): each synthesizes a real
                // method symbol on the class, spanning the class name token
                // so gd lands somewhere honest. The other fact kinds join to
                // callables/fields as before.
                if matches!(sym.kind.as_str(), "class" | "interface") {
                    let Some((cstart, facts)) =
                        sym.start.row.checked_sub(1).and_then(|r| by_end_row.get(&r))
                    else {
                        continue;
                    };
                    for f in facts {
                        // `@template T` rows: the class's generic params, in
                        // row order — the same per-class axis cpp templates
                        // feed, so `@return TModel` methods publish
                        // `ParamOf(i)` through the existing writeback.
                        if let DocFact::Template { name, line } = f {
                            out.template_params.push((sym.name.clone(), name.clone(), *line));
                            continue;
                        }
                        if let DocFact::Method { name, ret, line, col } = f {
                            // Span = the method NAME TOKEN in the fact's own
                            // `@method` line: a distinct gd/cursor target per
                            // row (every row on the class name span would
                            // collapse to one symbol), and the row's ONE
                            // declaration site — references from the token
                            // resolve the Method target, rename rewrites it.
                            let at = Point { row: cstart + line, column: *col };
                            let at_end = Point { row: at.row, column: col + name.len() };
                            doc_methods.push(SkelSymbol {
                                kind: "method".to_string(),
                                name: name.clone(),
                                start: at,
                                end: at_end,
                                name_start: at,
                                name_end: at_end,
                                package: Some(sym.name.clone()),
                                scope: sym.scope,
                                return_type: ret
                                    .as_deref()
                                    .and_then(|t| (pack.annot_type)(t)),
                                receiver_return: ret
                                    .as_deref()
                                    .is_some_and(|t| (pack.rettype_receiver)(t)),
                                receiver_instance_of: None,
                                deref_stack: Vec::new(),
                                attributes: Vec::new(),
                                arity: None,
                                qualifier_owned: false,
                            });
                        }
                    }
                    continue;
                }
                if !matches!(sym.kind.as_str(), "sub" | "method" | "field" | "anon" | "var") {
                    continue;
                }
                let Some((cstart, facts)) =
                    sym.start.row.checked_sub(1).and_then(|r| by_end_row.get(&r))
                else {
                    continue;
                };
                let cstart = *cstart;
                for f in facts {
                    match f {
                        // class-docblock facts; no callable/field join
                        DocFact::Method { .. } | DocFact::Template { .. } => {}
                        DocFact::ReturnRecvInstance { base } => {
                            if sym.return_type.is_none()
                                && !sym.receiver_return
                                && sym.receiver_instance_of.is_none()
                            {
                                sym.receiver_instance_of = Some(base.clone());
                            }
                        }
                        DocFact::Return(t) => {
                            // A doc row fills an undeclared return, and REFINES
                            // a bare declared container (`: array` +
                            // `@return array{Queue, Agent}`) — the same rule
                            // `doc_admits` applies to params.
                            let bare_container = matches!(
                                sym.return_type,
                                Some(InferredType::HashRef | InferredType::ArrayRef)
                            );
                            if (sym.return_type.is_none() || bare_container)
                                && !sym.receiver_return
                            {
                                if (pack.rettype_receiver)(t) {
                                    if sym.return_type.is_none() {
                                        sym.receiver_return = true;
                                    }
                                } else if let Some(doc) = (pack.annot_type)(t) {
                                    if !bare_container
                                        || matches!(
                                            doc,
                                            InferredType::Sequence(_)
                                                | InferredType::Parametric(_)
                                                | InferredType::HashWithKeys { .. }
                                        )
                                    {
                                        sym.return_type = Some(doc);
                                    }
                                }
                            }
                        }
                        DocFact::UsesMethod { name, line, col } => {
                            // PHPUnit `@dataProvider name`: a method REF on
                            // the enclosing class, spanning the provider
                            // NAME TOKEN in the docblock — providers gain
                            // real fan-in, and rename rewrites the token in
                            // place. Only meaningful on class members (the
                            // invocant is the class).
                            if let (true, Some(cls)) = (
                                matches!(sym.kind.as_str(), "sub" | "method"),
                                sym.package.as_deref(),
                            ) {
                                let start = Point { row: cstart + line, column: *col };
                                let end = Point {
                                    row: start.row,
                                    column: col + name.len(),
                                };
                                // Invocant is the CLASS NAME, not
                                // `__PACKAGE__`: the doc row sits in the
                                // class-body scope, whose package is the
                                // NAMESPACE, so the current-package walk
                                // would resolve the wrong owner — the join
                                // already knows the class.
                                doc_refs.push(SkelRef {
                                    via: None,
                                    kind: "member".to_string(),
                                    name: name.clone(),
                                    start,
                                    end,
                                    scope: sym.scope,
                                    invocant: Some((
                                        Span { start, end },
                                        cls.to_string(),
                                    )),
                                    member_op: None,
                                    arg_count: None,
                                    shape: crate::model::file_analysis::MemberShape::Callable,
                                });
                            }
                        }
                        DocFact::Var { ty: t, name: var_name } => {
                            // The NAMED inline form (`/** @var Type[] $rows */`
                            // above an assignment) types that specific local.
                            if let Some(vn) = var_name {
                                if sym.kind == "var" && &sym.name == vn {
                                    if let Some(ty) = (pack.annot_type)(t) {
                                        doc_witnesses.push(doc_cast_witness(
                                            &sym.name,
                                            sym.scope,
                                            ty,
                                            Span { start: sym.start, end: sym.start },
                                        ));
                                    }
                                }
                                continue;
                            }
                            if sym.kind == "var" {
                                continue;
                            }
                            // A documented property types class-wide (member
                            // lookup is not sequential), exactly like a
                            // declared field type. Syntax-typed fields skip
                            // (declared wins — docblocks drift) EXCEPT when
                            // the doc STRICTLY REFINES a bare container:
                            // `protected array $h` + `@var list<X>` is the
                            // canonical refinement — the syntax cannot spell
                            // the element, the doc exists to add it.
                            if sym.kind == "field" {
                                let Some(ty) = (pack.annot_type)(t) else { continue };
                                if doc_admits(
                                    pack,
                                    &annot_text_by_var,
                                    (&sym.name, sym.scope),
                                    &ty,
                                ) {
                                    let span = scope_spans
                                        .get(sym.scope.0 as usize)
                                        .copied()
                                        .unwrap_or(Span { start: sym.start, end: sym.start });
                                    doc_witnesses.push(doc_witness(
                                        &sym.name, sym.scope, ty, span,
                                    ));
                                }
                            }
                        }
                        DocFact::Param { name, ty } => {
                            // The def's own parameter — untyped, or a bare
                            // container the doc refines (same rule as Var).
                            let Some(ty) = (pack.annot_type)(ty) else { continue };
                            let in_def = |p: Point| {
                                (p.row, p.column) >= (sym.start.row, sym.start.column)
                                    && (p.row, p.column) <= (sym.end.row, sym.end.column)
                            };
                            if let Some((n, sc, at)) = param_syms.iter().find(|(n, sc, at)| {
                                n == name
                                    && in_def(*at)
                                    && doc_admits(pack, &annot_text_by_var, (n, *sc), &ty)
                            }) {
                                // Publish the row CLASS-KEYED too
                                // (`method#p#$name`): an @inheritDoc override
                                // in another file reaches it through the
                                // registry's PackageSymbol inheritance walk
                                // (every monolog `handleBatch` override was
                                // blind without this).
                                if let Some(cls) = sym.package.as_deref() {
                                    if matches!(sym.kind.as_str(), "sub" | "method") {
                                        doc_witnesses.push(crate::model::witnesses::Witness {
                                            attachment:
                                                crate::model::witnesses::WitnessAttachment::PackageSymbol {
                                                    package: cls.to_string(),
                                                    name: format!("{}#p#{}", sym.name, n),
                                                },
                                            source: crate::model::witnesses::WitnessSource::Builder(
                                                "skeleton-doc".into(),
                                            ),
                                            payload:
                                                crate::model::witnesses::WitnessPayload::InferredType(
                                                    ty.clone(),
                                                ),
                                            span: Span { start: *at, end: *at },
                                        });
                                    }
                                }
                                doc_witnesses.push(doc_witness(
                                    n,
                                    *sc,
                                    ty,
                                    Span { start: *at, end: *at },
                                ));
                            }
                        }
                    }
                }
            }
            // A refining doc row REPLACES the redundant bare-container annot
            // witness on its slot (the fold is not latest-wins; leaving the
            // `array` witness in place would keep beating the refinement).
            let refined: std::collections::HashSet<(std::string::String, crate::model::file_analysis::ScopeId)> =
                doc_witnesses
                    .iter()
                    .filter(|w| {
                        matches!(
                            &w.payload,
                            crate::model::witnesses::WitnessPayload::InferredType(
                                InferredType::Sequence(_) | InferredType::Parametric(_)
                            )
                        )
                    })
                    .filter_map(|w| match &w.attachment {
                        crate::model::witnesses::WitnessAttachment::Variable { name, scope } => {
                            Some((name.clone(), *scope))
                        }
                        _ => None,
                    })
                    .collect();
            if !refined.is_empty() {
                out.witnesses.retain(|w| {
                    let is_container_annot = matches!(
                        &w.source,
                        crate::model::witnesses::WitnessSource::Builder(s)
                            if s == crate::model::witnesses::ANNOT_SOURCE
                    ) && matches!(
                        &w.payload,
                        crate::model::witnesses::WitnessPayload::InferredType(
                            InferredType::HashRef | InferredType::ArrayRef
                        )
                    );
                    !(is_container_annot
                        && matches!(
                            &w.attachment,
                            crate::model::witnesses::WitnessAttachment::Variable { name, scope }
                                if refined.contains(&(name.clone(), *scope))
                        ))
                });
            }
            // A named `@var T $x` above a RE-assignment (php's function-
            // scoped locals: the def is the FIRST assignment, a later one is
            // a rebind FlowEdge, not a symbol) casts the variable from that
            // row on — the `$x = Factory::make(); /** @var Concrete $x */`
            // idiom that narrows a base-typed factory return.
            for (end_row, (_, facts)) in &by_end_row {
                for f in facts {
                    let DocFact::Var { ty, name: Some(vn) } = f else { continue };
                    let Some(t) = (pack.annot_type)(ty) else { continue };
                    let has_def = out
                        .symbols
                        .iter()
                        .any(|s| s.kind == "var" && &s.name == vn && s.start.row == end_row + 1);
                    if has_def {
                        continue;
                    }
                    if let Some(fe) = out
                        .flow_edges
                        .iter()
                        .find(|fe| &fe.target_name == vn && fe.target_at.row == end_row + 1)
                    {
                        out.witnesses.push(doc_cast_witness(
                            vn,
                            fe.target_scope,
                            t,
                            Span { start: fe.target_at, end: fe.target_at },
                        ));
                    }
                }
            }
            out.witnesses.extend(doc_witnesses);
            out.symbols.extend(doc_methods);
            out.refs.extend(doc_refs);
        }
    }
    // @inheritDoc param inheritance every syntax-untyped,
    // locally-undocumented PARAM edges to a class-keyed row
    // (`PackageSymbol{class, "method#p#$name"}`); the doc-join above
    // publishes the row where an ancestor's docblock declares the type,
    // and the registry's inheritance walk carries it across files. A
    // dangling edge (nothing ever publishes) resolves to None for free.
    {
        let method_rows: Vec<(String, String, Span)> = out
            .symbols
            .iter()
            .filter(|s| matches!(s.kind.as_str(), "sub" | "method"))
            .filter_map(|s| {
                s.package
                    .as_ref()
                    .map(|p| (p.clone(), s.name.clone(), Span { start: s.start, end: s.end }))
            })
            .collect();
        let in_span = |p: Point, sp: &Span| {
            (p.row, p.column) >= (sp.start.row, sp.start.column)
                && (p.row, p.column) <= (sp.end.row, sp.end.column)
        };
        let mut edges: Vec<crate::model::witnesses::Witness> = Vec::new();
        // NB: the local `param_sigs` vec — it moves into `out` only at the
        // end of this fn, so `out.param_sigs` is still empty here.
        for (sig_span, _) in &param_sigs {
            let Some((cls, method, _)) = method_rows
                .iter()
                .find(|(_, _, msp)| in_span(sig_span.start, msp))
            else {
                continue;
            };
            for v in out
                .symbols
                .iter()
                .filter(|v| v.kind == "var" && in_span(v.start, sig_span))
            {
                // A specifically-typed param never subscribes; a BARE
                // container annot (`array $records`) still does — the whole
                // @inheritDoc idiom is "syntax says array, the ancestor's
                // doc says which element type" (same refinement rule as
                // `doc_admits`).
                if let Some(annot) = annot_text_by_var.get(&(v.name.clone(), v.scope)) {
                    if !matches!(
                        (pack.annot_type)(annot),
                        Some(InferredType::HashRef | InferredType::ArrayRef)
                    ) {
                        continue;
                    }
                }
                edges.push(crate::model::witnesses::Witness {
                    attachment: crate::model::witnesses::WitnessAttachment::Variable {
                        name: v.name.clone(),
                        scope: v.scope,
                    },
                    source: crate::model::witnesses::WitnessSource::Builder(
                        crate::model::witnesses::INHERIT_PARAM_SOURCE.into(),
                    ),
                    payload: crate::model::witnesses::WitnessPayload::Edge(
                        crate::model::witnesses::WitnessAttachment::PackageSymbol {
                            package: cls.clone(),
                            name: format!("{}#p#{}", method, v.name),
                        },
                    ),
                    span: Span { start: v.start, end: v.start },
                });
            }
        }
        out.witnesses.extend(edges);
    }

    // Access-modifier stamp: the `@nonpublic.target` name spans mark
    // members whose modifier means non-public — the same `non_public`
    // attribute cpp access regions stamp, read by the completion gates.
    if !nonpublic_name_spans.is_empty() || !classattr_by_name_span.is_empty() {
        for sym in &mut out.symbols {
            if nonpublic_name_spans.contains(&(sym.name_start, sym.name_end))
                && !sym.attributes.iter().any(|a| a == "non_public")
            {
                sym.attributes.push("non_public".to_string());
            }
            if let Some(flavor) = classattr_by_name_span.get(&(sym.name_start, sym.name_end)) {
                if sym.kind == "class" && !sym.attributes.iter().any(|a| a == flavor) {
                    sym.attributes.push(flavor.clone());
                }
            }
        }
    }
    out.param_sigs = param_sigs;
    Ok(out)
}

/// Does a doc row get to type this (name, scope) slot? Yes when the syntax
/// declared nothing (declared wins — docblocks drift), and ALSO when the doc
/// is a `Sequence` refining a bare declared container (`array`/`iterable` —
/// the spelling that cannot carry an element). The doc witness lands AFTER
/// the declared one, so latest-wins reduction serves the refinement.
/// The positional index of a destructuring slot: the number of TOP-LEVEL
/// commas in the list text before the slot's byte offset (`[, $b]` → 1).
/// `None` for a keyed list (a top-level `=>`): its positions are not
/// positions, so the slot never binds positionally.
fn slot_position(list_text: &str, slot_offset: usize) -> Option<usize> {
    let bytes = list_text.as_bytes();
    let (mut depth, mut commas) = (0i32, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 1 && i < slot_offset => commas += 1,
            b'=' if depth == 1 && bytes.get(i + 1) == Some(&b'>') => return None,
            _ => {}
        }
        i += 1;
    }
    Some(commas)
}

/// The literal key of a KEYED destructuring slot (`['k' => $v]`): the
/// quoted string before the `=>` that precedes the slot in its own
/// top-level segment. `None` for a positional list or a non-literal key.
fn slot_key(list_text: &str, slot_offset: usize) -> Option<String> {
    let bytes = list_text.as_bytes();
    let (mut depth, mut seg_start) = (0i32, 0usize);
    for (i, &c) in bytes.iter().enumerate().take(slot_offset.min(bytes.len())) {
        match c {
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth == 1 {
                    seg_start = i + 1;
                }
            }
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 1 => seg_start = i + 1,
            _ => {}
        }
    }
    let seg = &list_text[seg_start..slot_offset.min(list_text.len())];
    let (key, _) = seg.split_once("=>")?;
    let key = key.trim();
    let quoted = key.len() >= 2
        && ((key.starts_with('\'') && key.ends_with('\''))
            || (key.starts_with('"') && key.ends_with('"')));
    quoted.then(|| key[1..key.len() - 1].to_string())
}

fn doc_admits(
    pack: &LangPack,
    annot_text_by_var: &std::collections::HashMap<
        (std::string::String, crate::model::file_analysis::ScopeId),
        std::string::String,
    >,
    slot: (&str, crate::model::file_analysis::ScopeId),
    doc_ty: &InferredType,
) -> bool {
    match annot_text_by_var.get(&(slot.0.to_string(), slot.1)) {
        None => true,
        Some(declared) => {
            matches!(
                doc_ty,
                InferredType::Sequence(_) | InferredType::Parametric(_)
            ) && matches!(
                (pack.annot_type)(declared),
                Some(InferredType::HashRef | InferredType::ArrayRef)
            )
        }
    }
}

/// A documentation-sourced type witness on a Variable slot — its own source
/// tag (not `ANNOT_SOURCE`): a doc type is real typing fuel, but the inlay
/// suppression that hides hints for syntax-annotated declarations should
/// still show one here (the docblock can sit far from the use).
/// A NAMED `@var T $x` is a cast the author wrote at that site: it rides
/// at annotation priority (`REFINE_SOURCE`) so the flow / call-binding
/// edges the same assignment mints — pushed later, equal priority, and
/// latest-wins — cannot override it with the factory's declared base.
fn doc_cast_witness(
    name: &str,
    scope: crate::model::file_analysis::ScopeId,
    ty: InferredType,
    span: Span,
) -> crate::model::witnesses::Witness {
    let mut w = doc_witness(name, scope, ty, span);
    w.source = crate::model::witnesses::WitnessSource::Builder(
        crate::model::witnesses::REFINE_SOURCE.into(),
    );
    w
}

fn doc_witness(
    name: &str,
    scope: crate::model::file_analysis::ScopeId,
    ty: InferredType,
    span: Span,
) -> crate::model::witnesses::Witness {
    crate::model::witnesses::Witness {
        attachment: crate::model::witnesses::WitnessAttachment::Variable {
            name: name.to_string(),
            scope,
        },
        source: crate::model::witnesses::WitnessSource::Builder("skeleton-doc".into()),
        payload: crate::model::witnesses::WitnessPayload::InferredType(ty),
        span,
    }
}

/// The `TypeName(alias) → …` payload for an underlying type spelling, resolving
/// it through the pack's `annot_type`: a class-shaped leaf edges into the alias
/// graph (`Edge(TypeName(cn))`), a primitive is a terminal `InferredType`, an
/// unrecognized spelling (`unsigned short` — has a space) is `ClassName(text)`
/// so hover shows it verbatim. Shared by typedef, file-local `#define`, and
/// gathered-external `#define` alias emission.
pub(crate) fn type_alias_payload(
    underlying: &str,
    annot_type: fn(&str) -> Option<InferredType>,
) -> crate::model::witnesses::WitnessPayload {
    use crate::model::witnesses::{WitnessAttachment, WitnessPayload};
    match annot_type(underlying) {
        Some(InferredType::ClassName(cn)) => WitnessPayload::Edge(WitnessAttachment::TypeName(cn)),
        Some(t) => WitnessPayload::InferredType(t),
        None => WitnessPayload::InferredType(InferredType::ClassName(underlying.to_string())),
    }
}

// The canonical template-spelling rule lives in the Model layer
// (`file_analysis.rs`) so the `ParametricType::Instance` peel shares it;
// re-exported here because the pack `shape_name`s are its Build-side home.
pub use crate::model::file_analysis::canonical_template_spelling;

/// Is `body` a bare TYPE spelling (a macro that aliases a type), rather than a
/// value/expression? Accepts identifier/keyword words possibly `::`-qualified
/// and space-separated (`U16`, `unsigned short`, `std::string`, `struct op`);
/// rejects numeric literals (`100`), operators, and punctuation (`1 << 3`,
/// `(x)`, `&y`). Cheap first-token gate: a type spelling never starts with a
/// digit or a symbol, and contains only word/`:`/space bytes.
pub(crate) fn looks_like_type_spelling(body: &str) -> bool {
    let b = body.trim();
    if b.is_empty() {
        return false;
    }
    if !b.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
        return false;
    }
    b.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == ' ')
}

/// Split a written qualified name into `(leaf, namespace)` at its last
/// separator, leading-`\` (a php global-anchored spelling) trimmed. A
/// separator-less spelling is a bare leaf in the global namespace.
fn split_ns_leaf(fq: &str) -> (String, String) {
    let t = fq.trim_start_matches('\\');
    match t.rsplit_once('\\') {
        Some((ns, leaf)) => (leaf.to_string(), ns.to_string()),
        None => (t.to_string(), String::new()),
    }
}

/// The chain-hop witness for one member-call site: the whole call's value
/// is `Projected{base, MethodHop{member, arity}}` — dispatch deferred to
/// query time, when the base's class and the index are in hand. A
/// simple-var receiver bases on the `Variable` (its witnesses live on the
/// scope chain, not on the read's span); a current-class receiver (php
/// `$this->`/`self::` via the pack's `hop.recv` shaping) bases on the
/// receiver span with a companion `ClassName(enclosing class)` witness —
/// extraction is the only place that class is in hand; anything else
/// bases on the receiver's `Expr` span, where a nested call carries its
/// OWN hop.
#[allow(clippy::too_many_arguments)]
fn push_hop_witness(
    witnesses: &mut Vec<crate::model::witnesses::Witness>,
    pack: &super::packs::LangPack,
    member_text: &str,
    call_span: crate::model::file_analysis::Span,
    recv_span: crate::model::file_analysis::Span,
    recv_text: &str,
    recv_simple: bool,
    scope: crate::model::file_analysis::ScopeId,
    arity: Option<u32>,
    enclosing_class: Option<&str>,
) {
    use crate::model::witnesses as wit;
    let hop_recv = (pack.shape_name)("hop.recv", recv_text);
    let base = if crate::model::conventions::is_current_package_token(&hop_recv) {
        let Some(cls) = enclosing_class else { return };
        witnesses.push(wit::Witness {
            attachment: wit::WitnessAttachment::Expr(recv_span),
            source: wit::WitnessSource::Builder("skeleton".into()),
            payload: wit::WitnessPayload::InferredType(
                crate::model::file_analysis::InferredType::ClassName(cls.to_string()),
            ),
            span: recv_span,
        });
        wit::WitnessAttachment::Expr(recv_span)
    } else if recv_simple {
        wit::WitnessAttachment::Variable {
            name: (pack.shape_name)("def.var", recv_text),
            scope,
        }
    } else if is_identifier_text(recv_text) {
        // A bareword receiver dispatches as the class (Perl's
        // `User->make` rule; php `Level::Debug` / `Foo::create()`): the
        // span carries no expression witness of its own, so seed it.
        witnesses.push(wit::Witness {
            attachment: wit::WitnessAttachment::Expr(recv_span),
            source: wit::WitnessSource::Builder("skeleton".into()),
            payload: wit::WitnessPayload::InferredType(
                crate::model::file_analysis::InferredType::ClassName(recv_text.to_string()),
            ),
            span: recv_span,
        });
        wit::WitnessAttachment::Expr(recv_span)
    } else {
        wit::WitnessAttachment::Expr(recv_span)
    };
    witnesses.push(wit::Witness {
        attachment: wit::WitnessAttachment::Expr(call_span),
        source: wit::WitnessSource::Builder("skeleton".into()),
        payload: wit::WitnessPayload::Projected {
            base,
            // Arity-less = a value read (`$this->prop`): the hop asks the
            // class for the member's VALUE edge, so a same-named method's
            // return can't answer for the property.
            step: match arity {
                Some(arity) => wit::ProjectionStep::MethodHop {
                    member: (pack.shape_name)("ref.member", member_text),
                    arity,
                },
                None => wit::ProjectionStep::ValueHop {
                    member: (pack.shape_name)("ref.member", member_text),
                },
            },
        },
        span: call_span,
    });
}

/// A bare identifier lexeme — the only shape that can name an enumerator.
/// Pure string test (no node-kind probe) so every language's capture text
/// routes through the same rule.
fn is_identifier_text(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

fn byte_range_of(events: &[Event], match_id: usize, cap: &str) -> Option<(usize, usize)> {
    events
        .iter()
        .find(|e| e.match_id == match_id && e.cap == cap)
        .map(|e| (e.start_byte, e.end_byte))
}

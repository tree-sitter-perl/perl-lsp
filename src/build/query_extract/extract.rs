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


/// What the document says about one declarator node: a level of the peel, a
/// per-level cv-qualifier, a level that says the declared name holds a value
/// that is CALLED, a level that only groups, or the chain's leaf (whose
/// capture suffix names the def the synthetic leaf event mints —
/// `@deref.leaf.field` → `def.field`).
#[derive(Clone)]
enum DerefCap {
    Step(crate::model::file_analysis::DerefKind),
    Annot,
    /// `@deref.callable` — the declared name is invoked (a function-pointer
    /// declarator). Adds no level to the stack; the leaf carries the fact.
    Callable,
    /// `@deref.paren` — a grouping level that denotes nothing of its own.
    Transparent,
    Leaf(String),
}

/// The `@deref.*` captures of one tree, by node identity — everything the
/// declarator peel needs to know about a language's declarators.
///
/// The extraction pass fills it as it flattens matches; a consumer with a tree
/// the extractor never saw (a reparsed macro body) fills it with
/// [`DerefCaps::of_tree`]. Both then peel through the SAME walk, so a pointer
/// field's `*`s are extracted once however the field was written.
///
/// Keyed by `Node::id`, not by byte range: a grammar routinely nests a node
/// inside another of the SAME extent (a declarator whose only child spans
/// its own bytes), and two captures on such a pair would collapse to one
/// entry with the last write winning — the peel then reads the wrong level's
/// verdict. Every fill and every peel runs against one tree, so the id is
/// exact.
#[derive(Default)]
pub struct DerefCaps(std::collections::HashMap<usize, DerefCap>);

impl DerefCaps {
    /// Record `node` under the capture that named it, ignoring every capture
    /// outside the `@deref` family. Returns whether it was one.
    fn record(&mut self, cap: &str, node: tree_sitter::Node) -> bool {
        use crate::model::file_analysis::DerefKind;
        let Some(rest) = cap.strip_prefix("deref.") else { return false };
        let what = match rest {
            "pointer" => DerefCap::Step(DerefKind::Pointer),
            "ref" => DerefCap::Step(DerefKind::Reference),
            "annot" => DerefCap::Annot,
            "callable" => DerefCap::Callable,
            "paren" => DerefCap::Transparent,
            _ => match rest.strip_prefix("leaf.") {
                Some(kind) => DerefCap::Leaf(format!("def.{kind}")),
                None => return false,
            },
        };
        self.0.insert(node.id(), what);
        true
    }

    /// Every `@deref.*` capture in `tree`, for a caller running the pack's
    /// query itself.
    pub fn of_tree(tree: &Tree, src: &[u8], pack: &LangPack) -> DerefCaps {
        let language = tree.language();
        let mut out = DerefCaps::default();
        let Ok(query) = cached_query(&language, effective_query_source(&language, pack)) else {
            return out;
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), src);
        while let Some(m) = matches.next() {
            for c in m.captures {
                out.record(names[c.index as usize], c.node);
            }
        }
        out
    }

    /// Flatten a declarator chain — the recursion tree-sitter's fixed-depth
    /// queries cannot express — to its leaf, recording a `DerefStep` per level.
    /// Outermost level first (left-to-right display order, `Box*&` →
    /// `[Pointer, Reference]`); depth-capped. The `callable` verdict is true
    /// when some level said the declared value is invoked. `None` when the
    /// chain reaches no leaf the document named, which mints nothing rather
    /// than a half-read shape.
    pub fn peel<'a>(
        &self,
        mut node: tree_sitter::Node<'a>,
        src: &[u8],
    ) -> Option<PeeledChain<'a>> {
        use crate::model::file_analysis::DerefStep;
        let at = |n: &tree_sitter::Node| self.0.get(&n.id());
        // The one descent: the first child the document named. A level's
        // inner declarator is the only child it captures, so this needs no
        // field name and no kind list.
        let inner_of = |n: tree_sitter::Node<'a>| -> Option<tree_sitter::Node<'a>> {
            let mut cur = n.walk();
            let found =
                n.children(&mut cur).find(|ch| !matches!(at(ch), None | Some(DerefCap::Annot)));
            found
        };
        let mut stack = Vec::new();
        let mut callable = false;
        for _ in 0..32 {
            match at(&node) {
                Some(DerefCap::Step(kind)) => {
                    let mut annotations = Vec::new();
                    let mut cur = node.walk();
                    for ch in node.children(&mut cur) {
                        if let Some(DerefCap::Annot) = at(&ch) {
                            if let Ok(t) = ch.utf8_text(src) {
                                annotations.push(t.to_string());
                            }
                        }
                    }
                    stack.push(DerefStep { kind: *kind, annotations });
                    node = inner_of(node)?;
                }
                Some(DerefCap::Callable) => {
                    callable = true;
                    node = inner_of(node)?;
                }
                Some(DerefCap::Transparent) => node = inner_of(node)?,
                // `identifier`→`def.local` (param/local), `field_identifier`→
                // `def.field` (a class member), so a pointer field outlines as a
                // member.
                Some(DerefCap::Leaf(def_cap)) => {
                    return Some(PeeledChain { leaf: node, stack, def_cap: def_cap.clone(), callable })
                }
                _ => return None,
            }
        }
        None
    }
}

/// A declarator chain read to its end: the name token, the levels above it,
/// the def capture its suffix names, and whether the value is invoked.
pub struct PeeledChain<'a> {
    pub leaf: tree_sitter::Node<'a>,
    pub stack: Vec<crate::model::file_analysis::DerefStep>,
    pub def_cap: String,
    pub callable: bool,
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

/// Peel declarator wrappers (`@ool.wrap`, ANY depth) to the inner function
/// declarator (`@ool.declarator`) — the arbitrary nesting S-queries can't
/// express (`Foo**& Class::m()`). THE out-of-line unwrap, spelled once so no
/// call site enumerates wrapper kinds. `None` when no function declarator is
/// reachable (not a function-def shape).
fn unwrap_to_function_declarator<'a>(
    mut node: tree_sitter::Node<'a>,
    wraps: &std::collections::HashSet<(usize, usize)>,
    declarators: &std::collections::HashSet<(usize, usize)>,
) -> Option<tree_sitter::Node<'a>> {
    for _ in 0..32 {
        let range = (node.start_byte(), node.end_byte());
        if declarators.contains(&range) {
            return Some(node);
        }
        if !wraps.contains(&range) {
            return None;
        }
        // a pointer declarator carries its inner under `declarator:`; a
        // reference/parenthesized declarator holds it as the first named child
        // (the `&`/parens are anonymous tokens).
        node = node.child_by_field_name("declarator").or_else(|| node.named_child(0))?;
    }
    None
}

/// Walk a qualified-name chain (`A::B::c`, `@ool.qualifier` at every hop) to its
/// leaf name token, returning the full scope text (`A::B`) and the leaf. THE
/// out-of-line owner walk: the owning class is the innermost scope —
/// `rsplit("::")` of the returned text, as the `def.` handler already does for
/// single-hop qualifiers — and the leaf is the member/ctor/dtor/operator name.
/// A segment the document captured a `@qualifier.name` for (a templated owner
/// `Buf<T>`) contributes that name. `None` when the node is not a qualified name
/// (a free function / in-class method — its own pattern owns it).
fn walk_qualifier_chain<'a>(
    mut node: tree_sitter::Node<'a>,
    qualifiers: &std::collections::HashSet<(usize, usize)>,
    peel: &std::collections::HashMap<usize, (usize, String)>,
    src: &[u8],
) -> Option<(String, tree_sitter::Node<'a>)> {
    let is_qualified =
        |n: &tree_sitter::Node| qualifiers.contains(&(n.start_byte(), n.end_byte()));
    if !is_qualified(&node) {
        return None;
    }
    let mut scopes: Vec<String> = Vec::new();
    for _ in 0..32 {
        if !is_qualified(&node) {
            return Some((scopes.join("::"), node));
        }
        if let Some(scope) = node.child_by_field_name("scope") {
            scopes.push(match peel.get(&scope.start_byte()) {
                Some((name_end, name)) if scope.end_byte() > *name_end => name.clone(),
                _ => scope.utf8_text(src).unwrap_or("").to_string(),
            });
        }
        node = node.child_by_field_name("name")?;
    }
    None
}

/// Drop transparent receiver wrappers (`(*p)`, `(&o)`, `(p)`) to the value
/// underneath. Depth-capped; the leaf is an invocant of any shape, so —
/// unlike the declarator peel — nothing is minted per level.
pub(crate) fn peel_receiver<'a>(
    mut node: tree_sitter::Node<'a>,
    wrappers: &std::collections::HashSet<&'static str>,
) -> tree_sitter::Node<'a> {
    for _ in 0..32 {
        if !wrappers.contains(node.kind()) {
            return node;
        }
        match node.named_child(0) {
            Some(inner) => node = inner,
            None => return node,
        }
    }
    node
}

/// The namespace segments a WRITTEN qualifier spells, in order. Source
/// text, split on the language's own separator — the parts the
/// `QualifiedSpelling` keeps so that nothing downstream has to.
fn segments_of(raw: &str, sep: &str) -> Vec<String> {
    if sep.is_empty() {
        return Vec::new();
    }
    raw.split(sep).filter(|s| !s.is_empty()).map(str::to_string).collect()
}
/// The `ImportBinds` a capture-name suffix declares, or `None` when the
/// suffix is not one. `use function` / `use const` rows bind a callable or a
/// constant; an unsuffixed row binds a type, so the pack spells only the two
/// exceptions and nothing has to read the leaf's capitalization to guess.
pub(super) fn import_binds_suffix(suffix: &str) -> Option<crate::model::file_analysis::ImportBinds> {
    match suffix {
        "function" => Some(crate::model::file_analysis::ImportBinds::Function),
        "const" => Some(crate::model::file_analysis::ImportBinds::Const),
        _ => None,
    }
}

/// A capture name with its import-binding suffix removed, so the dispatch
/// arms stay spelled as the bare capture (`import`, `import.name`) however
/// the pack's query declared the binding.
pub(super) fn strip_import_binds(cap: &str) -> &str {
    match cap.rsplit_once('.') {
        Some((head, sfx)) if cap.starts_with("import") && import_binds_suffix(sfx).is_some() => head,
        _ => cap,
    }
}

pub fn extract(tree: &Tree, source: &[u8], pack: &LangPack) -> Result<SkeletonAnalysis, String> {
    let language = tree.language();
    let query_source = effective_query_source(&language, pack);
    let query = cached_query(&language, query_source)?;
    // The cursor-time runner serves THIS object, never one of its own.
    super::cursor_query::remember(pack.lang_id, query, query_source);
    // A BARE variable: the node kinds the document reads as one. The
    // by-reference binding lane, the parameter-name walk and the op-DX gate
    // all mean the same shape, so they ask the same patterns.
    let simple_var_kinds = super::cursor_query::pattern_root_kinds(query, "expr.read.var");
    // Transparent receiver wrappers — `(p)`, `*p`, `&o` — named by the
    // document, so the mint's invocant span lands on the inner expression
    // `expr_type_at_span` already types.
    let recv_peel_kinds = super::cursor_query::recv_peel_kinds(query);
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
    // `f(...)` sites: a call with no countable arguments — still a call.
    let mut placeholder_call_at: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    // The same facts keyed by the MATCH that captured the callee alongside
    // its list: the call pattern joins the two, so `$this->m (1)` — a space
    // before the parentheses — is still a call. Adjacency stays the fallback
    // for shapes whose list lands in another match.
    let mut arg_counts_by_match: HashMap<usize, usize> = HashMap::new();
    let mut placeholder_by_match: std::collections::HashSet<usize> = Default::default();
    // The `@arity.args` lists, with the match that captured each, and every
    // `@arity.arg*` capture in the file. Joined below: a capture belongs to
    // the nearest enclosing list, which is what nests `f(g($x))` correctly.
    let mut arity_lists: Vec<(usize, tree_sitter::Node)> = Vec::new();
    let mut arg_caps: Vec<(&str, tree_sitter::Node)> = Vec::new();
    // A callable's declared parameter arity, keyed by the parameter_list span.
    // Associated to its def symbol by span containment in `into_file_analysis`
    // (`@arity.sig` fires a separate match from the def name).
    let mut param_sigs: Vec<(crate::model::file_analysis::Span, crate::model::file_analysis::ParamArity)> =
        Vec::new();
    // The out-of-line-definition vocabulary, by byte range: the declarator
    // wrappers, the function declarators, the qualified names, and a templated
    // qualifier's base name keyed by the spelling's start (a `template_type`
    // starts where its name does).
    let mut ool_defs: Vec<(tree_sitter::Node, usize)> = Vec::new();
    let mut ool_wraps: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut ool_declarators: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut ool_qualifiers: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut qualifier_peel: std::collections::HashMap<usize, (usize, String)> = Default::default();
    // The declarator peel's vocabulary, and the chains waiting on it: a level is
    // matched AFTER the chain that contains it, so the peel runs post-loop.
    let mut deref_caps = DerefCaps::default();
    let mut nested_targets: Vec<(tree_sitter::Node, usize)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    let mut match_counter = 0usize;
    while let Some(m) = matches.next() {
        match_counter += 1;
        for c in m.captures {
            let node = c.node;
            let cap = cap_names[c.index as usize].as_str();
            // The out-of-line vocabulary, collected here and joined once the
            // whole tree has been matched: a wrapper the peel descends, the
            // function declarator it stops at, the qualified name whose chain
            // names the owner, and a templated owner's base name. A wrapper
            // captured inside a definition is matched AFTER it, so the join
            // cannot run here.
            if let Some(set) = match cap {
                "ool.wrap" => Some(&mut ool_wraps),
                "ool.declarator" => Some(&mut ool_declarators),
                "ool.qualifier" => Some(&mut ool_qualifiers),
                _ => None,
            } {
                set.insert((node.start_byte(), node.end_byte()));
                continue;
            }
            if deref_caps.record(cap, node) {
                continue;
            }
            if cap == "qualifier.name" {
                qualifier_peel.insert(
                    node.start_byte(),
                    (node.end_byte(), node.utf8_text(source).unwrap_or("").to_string()),
                );
                continue;
            }
            if cap == "ool.def" {
                ool_defs.push((node, match_counter));
                continue;
            }
            if cap == "nested.target" {
                nested_targets.push((node, match_counter));
                continue;
            }
            // `@member.recv`: a member access receiver. Peel transparent
            // wrappers (`(*p)`, `(&o)`, `(p)`) to the typed inner where the
            // node is live, so the minted MethodCall ref's invocant_span lands
            // on the inner expression `expr_type_at_span` already types.
            if matches!(cap, "member.recv" | "member.recv.named" | "hop.recv") {
                // op-DX applies only to a bare-variable immediate receiver
                // (its deref_stack resolves by name); a wrapper/chain doesn't.
                member_simple.insert(match_counter, simple_var_kinds.contains(node.kind()));
                let inner = peel_receiver(node, recv_peel_kinds);
                events.push(Event {
                    start_byte: inner.start_byte(),
                    end_byte: inner.end_byte(),
                    start: inner.start_position(),
                    end: inner.end_position(),
                    // One receiver lane whichever capture named it: a
                    // string-named member and a chain hop have the same
                    // receiver a written member access does. The spellings
                    // differ so each capture's patterns keep stating one
                    // thing — `@member.recv`'s roots ARE the member-access
                    // kinds the cursor climbs to.
                    cap: "member.recv".to_string(),
                    text: inner.utf8_text(source).unwrap_or("").to_string(),
                    match_id: match_counter,
                });
                continue;
            }
            // Declaration-only captures: they state a language fact the
            // cursor paths read off the compiled query, and mint nothing
            // here. Dropped before they become events.
            if matches!(
                cap,
                "skip" | "recv.peel" | "recv.peel.deref" | "domain.compare.op" | "def.var.fn"
            ) {
                continue;
            }
            // `@arity.args`: a call's argument list. What is IN it the
            // document says argument by argument (`@arity.arg` and its
            // marks), so the count and the by-reference sites are joined
            // after the walk — a list's captures and its call's can sit in
            // different matches.
            if cap == "arity.args" {
                arity_lists.push((match_counter, node));
                continue;
            }
            if cap == "arity.arg" || cap.starts_with("arity.arg.") || cap == "arity.placeholder" {
                arg_caps.push((cap, node));
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
                        "variadic_parameter_declaration" | "..." => variadic = true,
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
            let text = node.utf8_text(source).unwrap_or("").to_string();
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
    // ---- `@nested.target`: a declarator CHAIN of any depth. Peel it to the
    // leaf + the deref stack, then emit the leaf as if the query had captured
    // it directly — downstream join/symbol/witness paths are unchanged, and
    // arbitrary nesting works without enumerating it.
    for (node, match_id) in &nested_targets {
        let Some(chain) = deref_caps.peel(*node, source) else { continue };
        nested_stacks.insert(*match_id, chain.stack);
        let leaf = chain.leaf;
        let ltext = leaf.utf8_text(source).unwrap_or("").to_string();
        for syn in ["flow.target", &chain.def_cap] {
            events.push(Event {
                start_byte: leaf.start_byte(),
                end_byte: leaf.end_byte(),
                start: leaf.start_position(),
                end: leaf.end_position(),
                cap: syn.to_string(),
                text: ltext.clone(),
                match_id: *match_id,
            });
        }
    }
    // ---- out-of-line definitions: peel each `@ool.def`'s declarator to its
    // function declarator, walk the qualified name to the leaf + owning class,
    // and synthesize the `def.method` / `def.method.name` / `qualifier` events
    // downstream extraction consumes — the same vocabulary the narrow per-shape
    // patterns emit for the shapes they own. A non-qualified declarator (free
    // function / in-class method) yields nothing — its own pattern owns it.
    // Arbitrary declarator nesting + multi-level qualifiers (which fixed-depth
    // S-queries can't express) work by construction.
    for (node, match_id) in &ool_defs {
        let Some((scope_text, leaf)) = node
            .child_by_field_name("declarator")
            .and_then(|d| unwrap_to_function_declarator(d, &ool_wraps, &ool_declarators))
            .and_then(|fd| fd.child_by_field_name("declarator"))
            .and_then(|q| walk_qualifier_chain(q, &ool_qualifiers, &qualifier_peel, source))
        else {
            continue;
        };
        let leaf_text = leaf.utf8_text(source).unwrap_or("").to_string();
        // the def symbol spans the whole function_definition; name + owner come
        // from the qualified declarator's leaf + scope.
        for (cap, n, text) in [
            ("def.method", *node, leaf_text.clone()),
            ("def.method.name", leaf, leaf_text.clone()),
            ("qualifier", *node, scope_text),
        ] {
            events.push(Event {
                start_byte: n.start_byte(),
                end_byte: n.end_byte(),
                start: n.start_position(),
                end: n.end_position(),
                cap: cap.to_string(),
                text,
                match_id: *match_id,
            });
        }
    }
    // A templated qualifier (`Buf<T>::grow`) joins its BASE class: peel every
    // `@qualifier` the document captured as a template spelling, once the peel
    // names are all in.
    for e in events.iter_mut().filter(|e| e.cap == "qualifier") {
        if let Some((name_end, name)) = qualifier_peel.get(&e.start_byte) {
            if e.end_byte > *name_end {
                e.text = name.clone();
            }
        }
    }
    // ---- join each argument capture to its list, then each list to its call ----
    // The node kinds an argument list IS come from the patterns that capture
    // an argument, so a document that teaches the language a new call shape
    // teaches the arity lane with it.
    {
        let arg_list_kinds = super::cursor_query::pattern_root_kinds(query, "arity.arg");
        #[derive(Default)]
        struct ListArgs<'t> {
            args: Vec<tree_sitter::Node<'t>>,
            named: std::collections::HashSet<usize>,
            spread: bool,
            placeholder: bool,
        }
        let mut by_list: HashMap<usize, ListArgs> = HashMap::new();
        for (cap, node) in arg_caps {
            // the nearest enclosing list — `f(g($x))` gives `$x` to g's
            let mut owner = node.parent();
            for _ in 0..8 {
                match owner {
                    Some(n) if arg_list_kinds.contains(n.kind()) => break,
                    Some(n) => owner = n.parent(),
                    None => break,
                }
            }
            let Some(owner) = owner.filter(|n| arg_list_kinds.contains(n.kind())) else { continue };
            let slot = by_list.entry(owner.id()).or_default();
            match cap {
                "arity.arg" => slot.args.push(node),
                "arity.arg.named" => {
                    slot.named.insert(node.id());
                }
                "arity.arg.spread" => slot.spread = true,
                "arity.placeholder" => slot.placeholder = true,
                _ => {}
            }
        }
        for slot in by_list.values_mut() {
            slot.args.sort_by_key(|n| n.start_byte());
        }
        let empty = ListArgs::default();
        for (match_id, list) in arity_lists {
            let at = (list.start_position().row, list.start_position().column);
            let slot = by_list.get(&list.id()).unwrap_or(&empty);
            // `f(...)` passes nothing — a first-class callable, not a call;
            // `f(...$args)` passes an unknowable number. Neither mints a
            // count: the callee still reads as callable, the arity lane
            // stands down.
            if slot.placeholder || slot.spread {
                placeholder_call_at.insert(at);
                placeholder_by_match.insert(match_id);
                continue;
            }
            arg_counts_by_start.insert(at, slot.args.len());
            arg_counts_by_match.insert(match_id, slot.args.len());
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
    // `@member.write` — a member on the LEFT of an assignment (php's
    // dynamic property declaration site).
    let mut member_writes: Vec<Span> = Vec::new();
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
    // `@ref.var.implicit` — reads the runtime binds without a declaration.
    let mut runtime_bound_reads: Vec<Span> = Vec::new();
    // What an import row BINDS, per match (`@import.function` / `@import
    // .const`, on the row capture or on its name token). Read once here so
    // every capture in the `import` family accepts the suffix and the flat
    // and group forms of a row answer the same way.
    let mut binds_by_match: HashMap<usize, crate::model::file_analysis::ImportBinds> =
        HashMap::new();
    // The NAME an import row binds (`@import.binds`), per match. A clause
    // spells at most two candidates — its leaf and its alias — on the one
    // capture, and the alias is always the later token, so the LATEST byte
    // is the binding whichever arm fired.
    let mut bound_name_by_match: HashMap<usize, (usize, String)> = HashMap::new();
    for e in &events {
        if e.cap == "import.binds" {
            let slot = bound_name_by_match.entry(e.match_id).or_insert((0, String::new()));
            if e.start_byte >= slot.0 {
                *slot = (e.start_byte, e.text.clone());
            }
        }
        if let (true, Some(b)) = (
            e.cap.starts_with("import"),
            e.cap.rsplit_once('.').and_then(|(_, sfx)| import_binds_suffix(sfx)),
        ) {
            binds_by_match.insert(e.match_id, b);
        }
        if let Some(prefix) = e.cap.strip_suffix(".name") {
            names_by_match
                .insert((e.match_id, prefix.to_string()), (e.text.clone(), e.start, e.end));
        }
        if e.cap == "member.write" {
            member_writes.push(Span { start: e.start, end: e.end });
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
        if e.cap == "ref.var.implicit" {
            runtime_bound_reads.push(Span { start: e.start, end: e.end });
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
    // feed the class-identity resolver below (packs with a namespace
    // separator only — empty otherwise).
    let mut out_use_aliases: Vec<(String, String, String)> = Vec::new();
    let mut use_map: HashMap<String, (String, String)> = HashMap::new();
    let mut group_import_sites:
        Vec<(String, Span, crate::model::file_analysis::ImportBinds, Option<String>)> = Vec::new();
    let mut parent_fq_by_match: HashMap<usize, String> = HashMap::new();
    // `@ref.qualified`: the WRITTEN qualifier of a call/ctor/type/parent
    // spelling (`Downloader\DownloadManager`, `\A\B`) — the use-map pins
    // the leaf to that namespace instead of counting it as a bare spelling.
    let qualified_by_match: HashMap<usize, String> = events
        .iter()
        .filter(|e| e.cap == "ref.qualified")
        .map(|e| (e.match_id, e.text.clone()))
        .collect();
    if let Some(sep) = pack.names.use_map_sep() {
        let mut use_fqn: HashMap<usize, String> = HashMap::new();
        let mut use_prefix: HashMap<usize, String> = HashMap::new();
        let mut use_leaf: HashMap<usize, (String, Span)> = HashMap::new();
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
                    use_leaf.insert(e.match_id, (e.text.clone(), Span { start: e.start, end: e.end }));
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
            let (leaf, ns) = split_ns_leaf(fqn, sep);
            let key = use_alias.get(mid).cloned().unwrap_or_else(|| leaf.clone());
            if use_alias.contains_key(mid) {
                out_use_aliases.push((key.clone(), ns.clone(), leaf.clone()));
            }
            use_map.insert(key, (ns, leaf));
        }
        // group form: `use A\B\{C, D as E}` — the prefix is the namespace,
        // each clause's own name the leaf.
        for (mid, (leaf, span)) in &use_leaf {
            let Some(prefix) = use_prefix.get(mid) else { continue };
            let key = use_alias.get(mid).cloned().unwrap_or_else(|| leaf.clone());
            let ns = prefix.trim_start_matches(sep).to_string();
            if use_alias.contains_key(mid) {
                out_use_aliases.push((key.clone(), ns.clone(), leaf.clone()));
            }
            // A group clause is an import row like any flat one: the same
            // `include_directives` row (spelled in full, spanning the leaf
            // token) feeds the use-map pin and the row-namespace lanes.
            group_import_sites.push((
                format!("{ns}{sep}{leaf}"),
                *span,
                binds_by_match.get(mid).copied().unwrap_or_default(),
                bound_name_by_match.get(mid).map(|(_, n)| n.clone()),
            ));
            use_map.insert(key, (ns, leaf.clone()));
        }
    }
    // ---- class identities ----
    // With a namespace separator every class spelling resolves ONCE, here,
    // to the identity it names: `UseMap::resolve` — the same ladder the
    // query side's pins are built from — over the rows just collected and
    // the namespace in force at the spelling's position. A declaration
    // joins its namespace directly. Without a separator a spelling IS its
    // identity, and every resolver below is the identity function.
    let ident_rows: Vec<crate::model::file_analysis::ImportRow> = use_map
        .iter()
        .filter(|(key, (_, leaf))| *key == leaf)
        .map(|(_, (ns, leaf))| {
            let zero = Point { row: 0, column: 0 };
            // rows exist only where the use-map block above ran, i.e. a
            // declared separator
            let sep = pack.names.use_map_sep().unwrap_or_default();
            let row = if ns.is_empty() { leaf.clone() } else { format!("{ns}{sep}{leaf}") };
            // A class-identity row, by construction: this is the ladder that
            // resolves class SPELLINGS, so the function/const rows are not
            // what it is built from — but they never key a spelling either,
            // and the span is synthetic (nothing resolves against it).
            crate::model::file_analysis::ImportRow {
                span: Span { start: zero, end: zero },
                raw: row,
                binds: crate::model::file_analysis::ImportBinds::Type,
                bound: None,
            }
        })
        .collect();
    let ident_aliases = out_use_aliases.clone();
    let namespace_marks: Vec<(Point, String)> = {
        let mut v: Vec<(Point, String)> = events
            .iter()
            .filter(|e| e.cap == "def.package.name")
            .map(|e| (e.start, e.text.trim_start_matches(pack.names.use_map_sep().unwrap_or_default()).to_string()))
            .collect();
        v.sort_by_key(|(p, _)| (p.row, p.column));
        v
    };
    let namespace_at = |at: Point| -> Option<&str> {
        namespace_marks
            .iter()
            .rev()
            .find(|(p, _)| (p.row, p.column) <= (at.row, at.column))
            .map(|(_, n)| n.as_str())
    };
    // The spellings that name the WRITING class rather than a namespaced
    // one (`self`, `static`): the document says which, on the receiver
    // capture that fires on them, and every reader asks the document.
    let self_class_tokens = super::cursor_query::capture_literals(query, "receiver.self");
    let ident = |written: &str, at: Point| -> String {
        // the current-class spellings name no namespace; the model
        // resolves them to the enclosing class
        if self_class_tokens.contains(written)
            || crate::model::conventions::is_current_package_token(written)
        {
            return written.to_string();
        }
        match pack.names.use_map_sep() {
            None => written.to_string(),
            Some(sep) => crate::model::file_analysis::UseMap {
                rows: &ident_rows,
                aliases: &ident_aliases,
                own_namespace: namespace_at(at),
                sep,
            }
            .resolve(written),
        }
    };
    let ident_type = |ty: InferredType, at: Point| -> InferredType {
        match pack.names.use_map_sep() {
            None => ty,
            Some(_) => ty.map_class_names(&mut |c| ident(c, at)),
        }
    };
    let annot_ident = |text: &str, at: Point| -> Option<InferredType> {
        (pack.annot_type)(text).map(|t| ident_type(t, at))
    };
    // A declared return, use-map-resolved like any other written type: the
    // pack turns the spelling into a shape, the file's imports decide what
    // its class names mean.
    let declared_ret = |text: &str, at: Point| -> Option<crate::model::witnesses::ReturnExpr> {
        (pack.declared_return)(text).map(|re| ret_expr_ident(re, &|t| ident_type(t, at), &|c| ident(c, at)))
    };
    let decl_ident = |leaf: &str, at: Point| -> String {
        match (pack.names.use_map_sep(), namespace_at(at)) {
            (Some(sep), Some(ns)) if !ns.is_empty() => format!("{ns}{sep}{leaf}"),
            _ => leaf.to_string(),
        }
    };

    // ---- the state machine: scope stack + sticky contexts ----
    let mut out = SkeletonAnalysis::default();
    out.use_aliases = out_use_aliases;
    // Group rows land ahead of the flat rows the main loop pushes in
    // document order; every reader of these lanes is span- or map-keyed,
    // so the order carries no meaning — do not make a consumer assume it.
    for (raw, span, binds, bound) in group_import_sites {
        out.imports.push(raw.clone());
        out.import_sites
            .push(crate::model::file_analysis::ImportRow { span, raw, binds, bound });
    }
    out.receiver_names = pack.receiver_names.iter().map(|s| s.to_string()).collect();
    out.spellings = Some(pack.spellings);
    out.runtime_bound_reads = std::mem::take(&mut runtime_bound_reads);
    out.member_writes = std::mem::take(&mut member_writes);
    out.names = pack.names.clone();
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
        owner: None,
        implicit_receiver: false,
    });
    scope_stack.push((tree.root_node().end_byte(), ScopeId(0)));

    // flow.assign joins: match_id → (target name+scope, source span)
    let mut flow_targets: HashMap<usize, (String, ScopeId, Point)> = HashMap::new();
    let mut flow_sources: HashMap<usize, Span> = HashMap::new();
    // `@flow.assign`: the match is a plain assignment to an existing local
    // (`FlowEdge::reassigns`) — never a declaration or a member write.
    let mut flow_assigns: std::collections::HashSet<usize> = Default::default();
    // Rebind shapes with no inflowing value (loop vars: `for x in …`,
    // `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    // cutoff sees them, exactly like Perl's `foreach` var.
    let mut flow_rebinds: Vec<(String, ScopeId, Point)> = Vec::new();
    // `@branch.expr` / `@branch.arm` (match / ternary) and `@subscript.*`,
    // joined per match after the loop.
    let mut branch_expr_by_match: HashMap<usize, Span> = HashMap::new();
    let mut branch_arm_by_match: HashMap<usize, Span> = HashMap::new();
    let mut annots: HashMap<usize, String> = HashMap::new();
    // keyed-shape collection: ctor + keys grouped per @expr.shape span
    let mut shape_spans: Vec<(usize, usize, Span)> = Vec::new();
    let mut shape_ctor_at: std::collections::HashSet<(usize, usize)> = Default::default();
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
    // Existence probes (`@probe.region`: the argument list of `isset` /
    // `empty`): a member read inside one asks whether the member exists.
    out.probe_regions = events
        .iter()
        .filter(|e| e.cap == "probe.region")
        .map(|e| Span { start: e.start, end: e.end })
        .collect();
    // Fold-only regions (`@fold` / `@fold.comment`): blocks and comment
    // runs that fold in an editor without being scopes (php has no block
    // scoping, so an `if` body must not mint one).
    out.fold_regions = events
        .iter()
        .filter(|e| e.cap == "fold" || e.cap == "fold.comment")
        .map(|e| (Span { start: e.start, end: e.end }, e.cap == "fold.comment"))
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
        let import_binds = binds_by_match.get(&e.match_id).copied().unwrap_or_default();
        match strip_import_binds(&e.cap) {
            // `@scope` = a plain lexical Block; `@scope.sub` = sub-body
            // content (function bodies, prototype signatures, explicit
            // instantiations, requires-expressions) — the kind
            // `scope_within_sub_body` reads to shield params/locals from the
            // outline and the class-content lane. The
            // `.implicit_receiver` suffix is the language saying a bare name
            // in this body may elide the member receiver. Pack subs carry no
            // name on the scope (the Symbol holds identity).
            cap @ ("scope" | "scope.sub" | "scope.sub.implicit_receiver") => {
                let id = ScopeId(out.scopes.len() as u32);
                out.scopes.push(Scope {
                    id,
                    parent: Some(cur_scope),
                    kind: if cap.starts_with("scope.sub") {
                        ScopeKind::Sub { name: String::new() }
                    } else {
                        ScopeKind::Block
                    },
                    span: Span { start: e.start, end: e.end },
                    package: package.clone(),
                    owner: None,
                    implicit_receiver: cap == "scope.sub.implicit_receiver",
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
                    // The parent pattern is its own match: its name entry
                    // is the pre-scan leaf, so the child's identity joins
                    // the namespace here exactly as the def handler did.
                    let child = decl_ident(child, e.start);
                    match pack.names.use_map_sep() {
                        None => out.parents.push((child.clone(), shaped)),
                        Some(_) => {
                            // The written spelling — its own qualifier when
                            // it has one, else the (possibly aliased) leaf —
                            // resolves to the parent's identity.
                            let written =
                                parent_fq_by_match.get(&e.match_id).cloned().unwrap_or(shaped);
                            out.parents.push((child.clone(), ident(&written, e.start)));
                        }
                    }
                }
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
                // A class declaration's identity is its FQN — the leaf
                // joined to the namespace in force — so its members file
                // under it and every spelling of it resolves to one key.
                // The match's name entry carries it too: the `@context` and
                // `@parent` handlers of the same match read ONE identity.
                let name = if kind == "class" {
                    let fqn = decl_ident(&name, e.start);
                    if fqn != name {
                        names_by_match
                            .insert((e.match_id, e.cap.clone()), (fqn.clone(), name_start, name_end));
                    }
                    fqn
                } else {
                    name
                };
                def_name_spans.push((e.start_byte, e.end_byte));
                // An out-of-line def's `Class::` qualifier names its owner
                // (the LAST `::` segment, the unqualified class the engine
                // keys by) — override the enclosing-namespace context.
                let pkg = qualifier_by_match
                    .get(&e.match_id)
                    .map(|q| q.rsplit("::").next().unwrap_or(q).to_string())
                    .or_else(|| package.clone());
                // A class's package is its NAMESPACE, whatever context it
                // sits in (an anonymous class inside a method is still
                // filed under the namespace its identity carries).
                let pkg = if kind == "class" && pack.names.use_map_sep().is_some() {
                    namespace_at(e.start).map(str::to_string)
                } else {
                    pkg
                };
                let shaped = (pack.shape_name)(&format!("def.{kind}"), &name);
                // A class-spec def carries its primary's name — the
                // (spec, primary) family edge `Specializes` derives from.
                if let Some(primary) = spec_primary_by_match.get(&e.match_id) {
                    out.specializations
                        .push((shaped.clone(), (pack.shape_name)("spec.primary", primary)));
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
                    declared_return: rettype_by_match
                        .get(&e.match_id)
                        .and_then(|t| declared_ret(t, e.start)),
                    // The annotation AS WRITTEN, through the pack's own
                    // spelling — the label a signature shows and the
                    // "already typed" gate both read this fact rather than
                    // re-scanning the declaration's source line.
                    return_annotation: rettype_by_match
                        .get(&e.match_id)
                        .filter(|_| !pack.spellings.return_annotation_template.is_empty())
                        .map(|t| pack.spellings.return_annotation_template.replace("{}", t)),
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
                member_recv.insert(
                    e.match_id,
                    (crate::model::file_analysis::Span { start: e.start, end: e.end }, e.text.clone()),
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
                    out.refs.push(SkelRef {
                        kind: e.cap.strip_prefix("ref.").unwrap().to_string(),
                        name: (pack.shape_name)(&e.cap, &e.text),
                        start: e.start,
                        end: e.end,
                        scope: cur_scope,
                        invocant: member_recv.get(&e.match_id).cloned(),
                        member_op,
                        // A call ref's arg list opens right where its callee /
                        // method token ends; plain (uncalled) member/type refs
                        // have no adjacent arg list and stay `None`.
                        arg_count: matches!(e.cap.as_str(), "ref.call" | "ref.qcall" | "ref.member")
                            .then(|| {
                                arg_counts_by_match
                                    .get(&e.match_id)
                                    .or_else(|| arg_counts_by_start.get(&(e.end.row, e.end.column)))
                                    .copied()
                            })
                            .flatten(),
                        flags: Default::default(),
                    });
                    if let Some(q) = qualified_by_match.get(&e.match_id) {
                        let leaf = (pack.shape_name)(&e.cap, &e.text);
                        let raw = q.strip_suffix(leaf.as_str()).unwrap_or_default();
                        let sep = pack.names.use_map_sep().unwrap_or_default();
                        // `\Throwable` carries no segments and is still
                        // ABSOLUTE — the leading separator is the whole claim.
                        let spelling = crate::model::file_analysis::QualifiedSpelling {
                            leaf,
                            segments: segments_of(raw, sep),
                            absolute: !sep.is_empty() && raw.starts_with(sep),
                        };
                        if spelling.absolute || !spelling.segments.is_empty() {
                            out.qualified_spellings.push(spelling);
                        }
                    }
                }
            }
            // One import row either way; the two spellings differ in what the
            // DOCUMENT claims about the token — `include.path` is a path the
            // preprocessor splices, `import.name` a name the file then spells.
            "import.name" | "include.path" => {
                out.import_sites.push(crate::model::file_analysis::ImportRow {
                    span: Span { start: e.start, end: e.end },
                    raw: e.text.clone(),
                    binds: import_binds,
                    bound: bound_name_by_match.get(&e.match_id).map(|(_, n)| n.clone()),
                });
                out.imports.push(e.text.clone());
            }
            "preamble" => {
                out.preamble_end = Some(out.preamble_end.map_or(e.end.row, |r| r.max(e.end.row)));
            }
            "import" => {
                let row = Span { start: e.start, end: e.end };
                if out.import_rows.last() != Some(&row) {
                    out.import_rows.push(row);
                }
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
            "branch.expr" => {
                branch_expr_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
            }
            "branch.arm" => {
                branch_arm_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
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
            "flow.assign" => {
                flow_assigns.insert(e.match_id);
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
                shape_ctor_at
                    .insert(byte_range_of(&events, e.match_id, "expr.shape").unwrap_or((0, 0)));
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
            // `@import.call.<kind>` — the document says what kind of import
            // this call is; the pack maps its argument to a module.
            cap if cap.starts_with("import.call.") => {
                import_fns.insert(e.match_id, cap["import.call.".len()..].to_string());
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
            // a keyed value only where the document named the constructor
            if !shape_ctor_at.contains(&(sb, eb)) {
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
    for (mid, kind) in &import_fns {
        if let Some(arg) = import_args.get(mid) {
            if let Some(module) = (pack.import_module)(kind, arg) {
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
        let mut best: HashMap<(usize, usize), usize> = HashMap::new();
        let mut keep = vec![true; out.symbols.len()];
        for (i, sym) in out.symbols.iter().enumerate() {
            let key = (sym.name_start.row, sym.name_start.column);
            match best.get(&key) {
                None => {
                    best.insert(key, i);
                }
                Some(&j) => {
                    let (gen_i, gen_j) =
                        (out.symbols[i].kind == "var", out.symbols[j].kind == "var");
                    let upgrade_ret = out.symbols[i].kind == out.symbols[j].kind
                        && out.symbols[i].declared_return.is_some()
                        && out.symbols[j].declared_return.is_none();
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
            kind: "call".into(),
            name: cmd.clone(),
            start: cmd_span.start,
            end: cmd_span.end,
            scope: *scope,
            invocant: None,
            member_op: None,
            arg_count: Some(args.len()),
            flags: Default::default(),
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
                            declared_return: None,
                            return_annotation: None,
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
                                kind: "call".into(),
                                name: name.clone(),
                                start: span.start,
                                end: span.end,
                                scope: *scope,
                                invocant: None,
                                member_op: None,
                                arg_count: None,
                                flags: Default::default(),
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
            let payload = match annot_ident(annot, *at) {
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
                    source: crate::model::witnesses::WitnessSource::Annotation(crate::model::witnesses::AnnotationKind::Declared),
                    payload,
                    span: annot_span,
                });
            }
        }
        if let Some(src_span) = flow_sources.get(mid) {
            // Narrow onto the outermost literal the rhs wraps, when the
            // rhs node itself carries no witness (paren wrappers).
            let src_bytes = byte_range_of(&events, *mid, "flow.source");
            let target_span = lit_spans
                .iter()
                .filter(|&&(s, en, _)| {
                    src_bytes.is_some_and(|(ss, se)| s >= ss && en <= se)
                })
                .max_by_key(|&&(s, en, _)| en - s)
                .map(|&(_, _, sp)| sp)
                .filter(|sp| sp != src_span)
                .unwrap_or(*src_span);
            // Mint a value-flow edge (cpp init is `Whole`); the witness is its
            // lowering, so type inference sees the same `Variable → Edge(Expr)`
            // it always did — now with the source span kept for provenance.
            out.flow_edges.push(crate::model::file_analysis::FlowEdge {
                target_name: name.clone(),
                target_scope: *scope,
                target_at: *at,
                source: target_span,
                extraction: crate::model::file_analysis::Extraction::Whole,
                reassigns: flow_assigns.contains(mid),
            });
            // A plain assignment RESETS every earlier belief about the name,
            // whatever its right-hand side types to: without the marker a
            // class assertion outlives the write that replaced it
            // (`$q = new Err(); $q = ['a' => 1];`) and an untyped rebind
            // leaves the old class standing. Zero-width at the write — the
            // fact's own site, and a point is a binding, not a narrowing
            // region.
            if flow_assigns.contains(mid) {
                use crate::model::witnesses as wit;
                out.witnesses.push(wit::Witness {
                    attachment: wit::WitnessAttachment::Variable {
                        name: name.clone(),
                        scope: *scope,
                    },
                    source: wit::WitnessSource::Builder(wit::RESET_SOURCE.into()),
                    payload: wit::WitnessPayload::Reset,
                    span: Span { start: *at, end: *at },
                });
            }
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
            reassigns: false,
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
    out.param_sigs = param_sigs;
    Ok(out)
}

/// Resolve every class name a declared return mentions through the file's
/// use map. The shape is the pack's; what its names MEAN is the file's, and
/// only this side knows the imports — so the walk is here, exhaustive, and
/// no consumer re-reads the spelling.
fn ret_expr_ident(
    re: crate::model::witnesses::ReturnExpr,
    ty: &dyn Fn(InferredType) -> InferredType,
    class: &dyn Fn(&str) -> std::string::String,
) -> crate::model::witnesses::ReturnExpr {
    use crate::model::witnesses::{ParametricOp, ReturnExpr as RE};
    let sub = |e: Box<RE>| Box::new(ret_expr_ident(*e, ty, class));
    match re {
        RE::Concrete(t) => RE::Concrete(ty(t)),
        RE::ReceiverOr(t) => RE::ReceiverOr(ty(t)),
        RE::Receiver => RE::Receiver,
        RE::Arg(n) => RE::Arg(n),
        RE::UnionOnArgs { branches } => RE::UnionOnArgs {
            branches: branches
                .into_iter()
                .map(|(g, e)| (g, ret_expr_ident(e, ty, class)))
                .collect(),
        },
        RE::Operator(op) => RE::Operator(match op {
            ParametricOp::RowOf(e) => ParametricOp::RowOf(sub(e)),
            ParametricOp::ParamOf { index, of } => {
                ParametricOp::ParamOf { index, of: sub(of) }
            }
            ParametricOp::InstanceOf { base, args } => ParametricOp::InstanceOf {
                base: class(&base),
                args: args.into_iter().map(|e| ret_expr_ident(e, ty, class)).collect(),
            },
        }),
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
/// separator, a leading separator (a global-anchored spelling) trimmed. A
/// separator-less spelling is a bare leaf in the global namespace.
fn split_ns_leaf(fq: &str, sep: &str) -> (String, String) {
    let t = fq.trim_start_matches(sep);
    match t.rsplit_once(sep) {
        Some((ns, leaf)) => (leaf.to_string(), ns.to_string()),
        None => (t.to_string(), String::new()),
    }
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

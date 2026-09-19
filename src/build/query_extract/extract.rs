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

/// A parameter's NAME token inside its declaration node: the one the
/// document named (`@arity.param.name`), else the first simple-variable
/// descendant — a C++ declarator nests the `identifier` under however many
/// pointer/array/reference declarators the type wrote, which no fixed-depth
/// pattern reaches. `None` for an unnamed parameter (`void f(int)`, a bare
/// `...`), which binds nothing. The by-reference lane and the parameter lane
/// locate the same token, so they ask the same function.
fn param_name_node<'t>(
    ch: tree_sitter::Node<'t>,
    captured: Option<tree_sitter::Node<'t>>,
    simple_var_kinds: &std::collections::HashSet<&'static str>,
) -> Option<tree_sitter::Node<'t>> {
    if captured.is_some() {
        return captured;
    }
    // The descent's stopping kinds are the document's own too: the nodes its
    // read patterns root at ARE this language's simple variables.
    let mut stack = vec![ch];
    while let Some(n) = stack.pop() {
        if n != ch && simple_var_kinds.contains(n.kind()) {
            return Some(n);
        }
        let mut w = n.walk();
        let kids: Vec<_> = n.named_children(&mut w).collect();
        stack.extend(kids.into_iter().rev());
    }
    None
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

/// The capture suffix by which a query DECLARES that the defs one match
/// mints are co-declared — one token, two symbols, `Symbol::declared_with`
/// each way (php's promoted constructor property and its ctor-body local).
/// It is opt-in at the capture because "two defs in one match" is a
/// property of those patterns, not of the capture vocabulary: a future
/// bundled pattern, or a plugin overlay's query, that happens to capture
/// two defs would otherwise mint a false pair (rule #10).
pub(super) const CODECLARED_SUFFIX: &str = ".declared_with";

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
    // Matches whose peeled chain said the declared value is CALLED.
    let mut nested_callable: std::collections::HashSet<usize> = Default::default();
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
    // The `@arity.sig` signatures and every `@arity.param*` capture in the
    // file. Joined below: a parameter belongs to the signature it is a child
    // of, a part (`name` / `default` / `type`) to the parameter that contains
    // it — so a signature nested inside a parameter (a function-pointer
    // parameter) counts its own.
    let mut arity_sigs: Vec<tree_sitter::Node> = Vec::new();
    let mut param_caps: Vec<(&str, tree_sitter::Node)> = Vec::new();
    // A callable's declared parameter arity AND the parameters themselves,
    // keyed by the parameter_list span. Associated to its def symbol by span
    // containment in `into_file_analysis` (`@arity.sig` fires a separate match
    // from the def name). The parameters ride with the counts because the walk
    // that counts them is the one that holds their name tokens (rule #11) — a
    // consumer that wants names must never re-scan the source for them.
    let mut param_sigs: Vec<(
        crate::model::file_analysis::Span,
        crate::model::file_analysis::ParamArity,
        Vec<crate::model::file_analysis::ParamInfo>,
    )> = Vec::new();
    // Bare variables written as call arguments, keyed by the argument
    // list's start (the callee token's end — the same adjacency the arity
    // join uses): (position, the variable's name, its token start). The
    // callee ref's push mints one binding edge per entry
    // (`docs/adr/by-ref-binding.md`); the callee's bag decides which
    // positions alias.
    let mut arg_vars_by_start: HashMap<(usize, usize), Vec<(u32, String, Point)>> = HashMap::new();
    // A callable's by-reference parameter positions with their names, keyed
    // by the parameter list's span (joined to the def symbol like the arity).
    // The def symbols each MATCH minted, in mint order — paired where the
    // match's captures declared it (`CODECLARED_SUFFIX`).
    let mut defs_by_match: HashMap<usize, Vec<u32>> = HashMap::new();
    // Matches whose `@def.*` captures declared the pair (`CODECLARED_SUFFIX`).
    let mut codeclared_matches: std::collections::HashSet<usize> = Default::default();
    let mut by_ref_params: Vec<(crate::model::file_analysis::Span, u32, String, crate::model::file_analysis::Span)> =
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
            // `@arity.sig`: a callable's parameter list. WHICH children of
            // it are parameters, and what each one carries, the document
            // states capture by capture (`@arity.param*`) — joined to the
            // signature after the walk, because a part's capture and its
            // parameter's can sit in different matches.
            if cap == "arity.sig" {
                arity_sigs.push(node);
                continue;
            }
            if cap.starts_with("arity.param") {
                param_caps.push((cap, node));
                continue;
            }
            // A `@def.*` capture may declare that this match's defs are
            // co-declared; the marker is read and stripped here, so every
            // path below sees the plain capture it already knows.
            let cap = match cap.strip_suffix(CODECLARED_SUFFIX) {
                Some(base) if base.starts_with("def.") => {
                    codeclared_matches.insert(match_counter);
                    base
                }
                _ => cap,
            };
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
    // ---- declared parameters: the arity counts and the parameters
    // themselves. Each `@arity.param*` capture states what its parameter
    // does to the count — one that must be written, one carrying a default,
    // one that absorbs the rest — so a language that grows a parameter
    // spelling grows a pattern, and an overlay can teach the lane one.
    {
        let param_ids: std::collections::HashSet<usize> = param_caps
            .iter()
            .filter(|(c, _)| !matches!(*c, "arity.param.name" | "arity.param.default" | "arity.param.type"))
            .map(|(_, n)| n.id())
            .collect();
        // parameters by the signature they sit in, parts by the parameter
        // that encloses them, and the by-reference mark as a set: it lands
        // on the parameter node its own pattern already captured.
        let mut params_of: HashMap<usize, Vec<(&str, tree_sitter::Node)>> = HashMap::new();
        let mut parts_of: HashMap<usize, Vec<(&str, tree_sitter::Node)>> = HashMap::new();
        let mut by_ref_ids: std::collections::HashSet<usize> = Default::default();
        for (cap, node) in &param_caps {
            match *cap {
                "arity.param" | "arity.param.optional" | "arity.param.variadic" => {
                    if let Some(sig) = node.parent() {
                        params_of.entry(sig.id()).or_default().push((cap, *node));
                    }
                }
                "arity.param.byref" => {
                    by_ref_ids.insert(node.id());
                }
                _ => {
                    let mut owner = node.parent();
                    while let Some(n) = owner {
                        if param_ids.contains(&n.id()) {
                            parts_of.entry(n.id()).or_default().push((cap, *node));
                            break;
                        }
                        owner = n.parent();
                    }
                }
            }
        }
        let mut seen_sigs: std::collections::HashSet<usize> = Default::default();
        for sig in &arity_sigs {
            if !seen_sigs.insert(sig.id()) {
                continue;
            }
            let sig_span = crate::model::file_analysis::Span {
                start: sig.start_position(),
                end: sig.end_position(),
            };
            let mut declared = params_of.remove(&sig.id()).unwrap_or_default();
            declared.sort_by_key(|(_, n)| n.start_byte());
            let mut total = 0usize;
            let mut required = 0usize;
            let mut variadic = false;
            let mut params: Vec<crate::model::file_analysis::ParamInfo> = Vec::new();
            for (cap, ch) in declared {
                let is_slurpy = cap == "arity.param.variadic";
                let parts = parts_of.get(&ch.id());
                let part = |want: &str| {
                    parts
                        .and_then(|ps| ps.iter().find(|(c, _)| *c == want))
                        .map(|(_, n)| *n)
                };
                let name_node = param_name_node(ch, part("arity.param.name"), simple_var_kinds);
                // A by-reference position (php `&$out`, C++ `T& x`): the
                // parameter's variable name, so the def mints the aliasing
                // edge the call sites bind through. A slurpy parameter takes
                // no position, so it claims none.
                if by_ref_ids.contains(&ch.id()) && !is_slurpy {
                    if let Some(name_node) = name_node {
                        by_ref_params.push((
                            sig_span,
                            total as u32,
                            (pack.shape_name)("def.var", name_node.utf8_text(source).unwrap_or("")),
                            crate::model::file_analysis::Span {
                                start: name_node.start_position(),
                                end: name_node.end_position(),
                            },
                        ));
                    }
                }
                // The parameter itself, in source order, with the default as
                // SOURCE TEXT (rule #13 — a rendering would have to be parsed
                // back). A parameter with no name token (C's bare `...`, an
                // unnamed `void f(int)`) binds nothing and mints nothing.
                if let Some(name_node) = name_node {
                    params.push(crate::model::file_analysis::ParamInfo {
                        name: (pack.shape_name)(
                            "def.var",
                            name_node.utf8_text(source).unwrap_or(""),
                        ),
                        default: part("arity.param.default")
                            .and_then(|d| d.utf8_text(source).ok())
                            .map(str::to_string),
                        is_slurpy,
                        is_invocant: false,
                        binding_site: Some(name_node.start_position()),
                        declared_type: part("arity.param.type")
                            .and_then(|t| t.utf8_text(source).ok())
                            .map(str::to_string),
                    });
                }
                match cap {
                    "arity.param" => {
                        total += 1;
                        required += 1;
                    }
                    "arity.param.optional" => total += 1,
                    _ => variadic = true,
                }
            }
            param_sigs.push((
                sig_span,
                crate::model::file_analysis::ParamArity { total, required, variadic },
                params,
            ));
        }
    }
    // ---- `@nested.target`: a declarator CHAIN of any depth. Peel it to the
    // leaf + the deref stack, then emit the leaf as if the query had captured
    // it directly — downstream join/symbol/witness paths are unchanged, and
    // arbitrary nesting works without enumerating it.
    for (node, match_id) in &nested_targets {
        let Some(chain) = deref_caps.peel(*node, source) else { continue };
        nested_stacks.insert(*match_id, chain.stack);
        if chain.callable {
            nested_callable.insert(*match_id);
        }
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
            vars: Vec<tree_sitter::Node<'t>>,
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
                "arity.arg.var" => slot.vars.push(node),
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
            for (position, arg) in slot.args.iter().enumerate() {
                // a named argument (`f(out: $x)`) is matched by name, not
                // position — no site
                if slot.named.contains(&arg.id()) {
                    continue;
                }
                let Some(var) = slot
                    .vars
                    .iter()
                    .find(|v| v.start_byte() >= arg.start_byte() && v.end_byte() <= arg.end_byte())
                else {
                    continue;
                };
                let text = var.utf8_text(source).unwrap_or("");
                arg_vars_by_start.entry(at).or_default().push((
                    position as u32,
                    (pack.shape_name)("def.var", text),
                    var.start_position(),
                ));
            }
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
    // `@def.<kind>.anchor` — a name-less def's anchor token (php's `class`
    // keyword): the pack synthesizes the name from the position, and the
    // match joins it like a `.name` capture so the def, its `@context`
    // and its `@parent` edges all read ONE identity.
    let mut defaulted_matches: HashMap<usize, String> = HashMap::new();
    // Spans a `variable_name` read pattern must NOT mint as reads: a
    // static property's `$name` (`Foo::$bar` — a member, `@var.member`) and
    // any declaration's own name token (a property `$chunks`, a parameter).
    let mut not_a_read: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut def_name_ends: std::collections::HashSet<usize> = std::collections::HashSet::new();
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
    // `@member.op` → the written operator + its span; joined to
    // `@ref.member` so op-DX rides the minted ref.
    let mut member_op_raw: HashMap<usize, (crate::model::file_analysis::MemberOp, crate::model::file_analysis::Span)> =
        HashMap::new();
    // `@member.op.<which>` — the operator token span the document names as
    // an arrow or a dot. A separate pattern per operator, so the general
    // `@member.op` arm above keeps minting the reference for an operator
    // neither names.
    let mut member_op_by_span: HashMap<(Point, Point), crate::model::file_analysis::MemberOp> =
        HashMap::new();
    // `@hop.call` → the WHOLE member-call expression's span, joined to its
    // `@ref.member` so the chain-hop witness (`Projected{base, MethodHop}`)
    // attaches where an OUTER call's receiver span will look for it.
    let mut hop_call_by_match: HashMap<usize, crate::model::file_analysis::Span> = HashMap::new();
    // `@dispatch.via` — the dispatching function's name token (`do_action`),
    // joined to the same match's `@ref.dispatch.named` string as the minted
    // DispatchCall's `dispatcher` label.
    let mut dispatch_via_by_match: HashMap<usize, String> = HashMap::new();
    // `@handler.name` — the token whose TEXT names a `@def.handler.by.<rail>`
    // handler in the same match (a listener's `handle(X $e)` is a handler
    // named `X` sitting on the method's name token).
    let mut handler_name_by_match: HashMap<usize, String> = HashMap::new();
    // `@key.elem` — the array element a `@def.handler.key` string heads.
    let mut key_elem_by_match: HashMap<usize, Span> = HashMap::new();
    // `@nonpublic.target` — def NAME spans whose member carries an access
    // modifier meaning non-public (the vocabulary lives in the query's
    // #any-of?). Joined to symbols by name span in a post-pass, stamping
    // the same `non_public` attribute cpp access regions stamp.
    // `@static.target` — def NAME spans of `static` members (the "static"
    // attribute a scoped completion reads).
    let mut static_name_spans: std::collections::HashSet<(Point, Point)> = std::collections::HashSet::new();
    // `@alias.target` — a variable declared by reference assignment
    // (`$h = &$opts['h']`): the `alias` attribute, a write through which is
    // a use of the storage it names. Keyed by the name token's END: the
    // capture sits on the sigil-less inner name the def's `$name` wraps.
    let mut alias_name_ends: std::collections::HashSet<Point> = std::collections::HashSet::new();
    // `@contract.target` — def NAME spans of contract callables (an
    // interface's methods, an abstract method): a `contract` attribute, the
    // requires of the role the declaring container is.
    let mut contract_name_spans: std::collections::HashSet<(Point, Point)> = std::collections::HashSet::new();
    let mut nonpublic_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@receiver.super` — the match whose receiver dispatches ABOVE the
    // writing class (php `parent::`): its `@ref.member` mints on the model's
    // SUPER lane. A per-match fact, because the ref and its receiver kind
    // arrive in the same match by construction.
    let mut super_recv_matches: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // `@receiver.self` — the match whose receiver NAMES the enclosing class
    // (php `self::` / `static::`). The class is in hand at the mint (the
    // class-body scope's package), so the invocant carries it and no
    // consumer re-derives a receiver token's meaning.
    let mut self_recv_matches: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // `@param.receiver` — the receiver PARAMETER's name span (python
    // `self`/`cls`): the symbol carries `RECEIVER`, and outline / member
    // completion ask the symbol instead of matching its name.
    let mut receiver_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@def.method.ctor` — the constructor declaration's name span; the
    // symbol carries `CONSTRUCTOR`, which is what rename policy, the dead-code
    // shield and the annotation lane ask.
    // What the document said AT a reference site, by the token's span: the
    // language's own object token, a name that resolves off the enclosing
    // class rather than a namespace, and a call that names the constructor.
    // Stamped onto the refs once, below, so no lane re-matches a spelling.
    let mut this_receiver_spans: std::collections::HashSet<(Point, Point)> = Default::default();
    let mut relative_scope_spans: std::collections::HashSet<(Point, Point)> = Default::default();
    let mut ctor_call_spans: std::collections::HashSet<(Point, Point)> = Default::default();
    let mut ctor_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@def.var.throwaway` — a binding written to be discarded; the symbol
    // carries `THROWAWAY`, which is what the unused-variable lane asks.
    let mut throwaway_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@def.method.catch_all` — the def NAME spans of catch-all members. The
    // CLASS that declares one carries `DYNAMIC_MEMBERS`, which is what the
    // undefined-member lanes ask before they call a member missing.
    let mut catch_all_name_spans: std::collections::HashSet<(Point, Point)> =
        std::collections::HashSet::new();
    // `@sym.attr.deprecated` — the ATTRIBUTE spelling of `@deprecated`,
    // per match, so the def it annotates carries the same fact the docblock
    // tag gives.
    let mut deprecated_matches: std::collections::HashSet<usize> = Default::default();
    // `@ref.var.implicit` — reads the runtime binds without a declaration.
    let mut runtime_bound_reads: Vec<Span> = Vec::new();
    // The attribute TOKENS the document names deprecated. A def's own
    // pattern captures its attributes as `@sym.attr` and must stay
    // predicate-free (a `#eq?` there would gate the whole def), so the
    // marking pattern is separate and the two meet at the token's span.
    let deprecated_attr_spans: std::collections::HashSet<(Point, Point)> = events
        .iter()
        .filter(|e| e.cap == "sym.attr.deprecated")
        .map(|e| (e.start, e.end))
        .collect();
    // `@classattr.<flavor>` — container-def name spans stamped with a
    // flavor attribute ("interface"/"trait"): the model's SymKind::Class
    // covers all three php container kinds, and SUPER/reference walks
    // need to ask the value which one it is.
    let mut classattr_by_name_span: HashMap<(Point, Point), String> = HashMap::new();
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
        if e.cap == "var.member" || e.cap.ends_with(".name") {
            not_a_read.insert((e.start_byte, e.end_byte));
            // a declaration's name token is nested in the `$name` a read
            // pattern also matches: same END byte, never a read
            def_name_ends.insert(e.end_byte);
        }
        if e.cap == "member.write" {
            member_writes.push(Span { start: e.start, end: e.end });
        }
        if let Some(prefix) = e.cap.strip_suffix(".anchor") {
            let kind = prefix.strip_prefix("def.").unwrap_or(prefix);
            if let Some(n) = (pack.default_name)(kind, e.start.row, e.start.column) {
                names_by_match.insert((e.match_id, prefix.to_string()), (n.clone(), e.start, e.end));
                defaulted_matches.insert(e.match_id, n);
            }
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
        if e.cap == "handler.name" {
            handler_name_by_match.insert(e.match_id, e.text.clone());
        }
        if e.cap == "key.elem" {
            key_elem_by_match.insert(e.match_id, Span { start: e.start, end: e.end });
        }
        if e.cap == "static.target" {
            static_name_spans.insert((e.start, e.end));
        }
        if e.cap == "alias.target" {
            alias_name_ends.insert(e.end);
        }
        if e.cap == "contract.target" {
            contract_name_spans.insert((e.start, e.end));
        }
        if e.cap == "nonpublic.target" {
            nonpublic_name_spans.insert((e.start, e.end));
        }
        if e.cap == "receiver.super" {
            super_recv_matches.insert(e.match_id);
            relative_scope_spans.insert((e.start, e.end));
        }
        if e.cap == "receiver.self" {
            self_recv_matches.insert(e.match_id);
            relative_scope_spans.insert((e.start, e.end));
        }
        if e.cap == "receiver.this" {
            this_receiver_spans.insert((e.start, e.end));
        }
        if e.cap == "ref.method.ctor" {
            ctor_call_spans.insert((e.start, e.end));
        }
        if e.cap == "param.receiver" {
            receiver_name_spans.insert((e.start, e.end));
        }
        if e.cap == "def.method.ctor" {
            ctor_name_spans.insert((e.start, e.end));
        }
        if e.cap == "def.var.throwaway" {
            throwaway_name_spans.insert((e.start, e.end));
        }
        if e.cap == "def.method.catch_all" {
            catch_all_name_spans.insert((e.start, e.end));
        }
        if e.cap == "sym.attr" && deprecated_attr_spans.contains(&(e.start, e.end)) {
            deprecated_matches.insert(e.match_id);
        }
        if e.cap == "ref.var.implicit" {
            runtime_bound_reads.push(Span { start: e.start, end: e.end });
        }
        if let Some(flavor) = e.cap.strip_prefix("classattr.") {
            classattr_by_name_span.insert((e.start, e.end), flavor.to_string());
        }
    }
    // The spellings this file writes for the object the enclosing method runs
    // on (`$this`, `this`, a `self`/`cls` parameter). The class body witnesses
    // each as an instance of its class, which is how a receiver with no
    // declaration types.
    let receiver_tokens: Vec<String> = {
        let mut v: Vec<String> = events
            .iter()
            .filter(|e| matches!(e.cap.as_str(), "receiver.this" | "param.receiver"))
            .map(|e| e.text.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };
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
    // A template parameter is a name in its own axis (`ParamOf` keys on
    // the bare spelling), never a class spelling to resolve.
    let template_names: std::collections::HashSet<String> = events
        .iter()
        .filter(|e| e.cap == "doc.comment")
        .flat_map(|e| (pack.doc_types)(&e.text, pack.doc_uses_method_tags))
        .filter_map(|f| match f {
            super::packs::DocFact::Template { name, .. } => Some(name),
            _ => None,
        })
        .collect();
    // The spellings that name the WRITING class rather than a namespaced
    // one (`self`, `static`): the document says which, on the receiver
    // capture that fires on them, and every reader asks the document.
    let self_class_tokens = super::cursor_query::capture_literals(query, "receiver.self");
    let ident = |written: &str, at: Point| -> String {
        // the current-class spellings name no namespace; the model
        // resolves them to the enclosing class
        if self_class_tokens.contains(written)
            || crate::model::conventions::is_current_package_token(written)
            || template_names.contains(written)
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
    out.spellings = Some(pack.spellings);
    out.lang_id = pack.lang_id;
    {
        let conv = crate::build::query_extract::rail_conventions_for(pack);
        out.rail_name_seps = conv.name_seps.clone();
        out.class_named_rails = conv.class_named_rails.clone();
    }
    out.runtime_bound_reads = std::mem::take(&mut runtime_bound_reads);
    out.member_writes = std::mem::take(&mut member_writes);
    // Assignment-declares-for-the-function is what the document SAYS on the
    // pattern that mints such a def: a capability is what the query mints,
    // never a pack flag beside it.
    out.function_scoped_vars = cap_names.iter().any(|c| c == "def.var.fn");
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
    // (depth, identity, is_class): the class flag is what tells an
    // enclosing NAMESPACE from an enclosing class body, so a bare call's
    // callee is keyed where a free function would be declared.
    let mut context_stack: Vec<(usize, String, bool)> = Vec::new();
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
    // `@cmd.def.<kind>` — the entity kind a command declares, with the
    // command's own start (the def spans from the command to the name) and the
    // scope it sits in, joined to this match's `@cmd.def.name`.
    let mut cmd_defs: HashMap<usize, (String, Point, ScopeId)> = HashMap::new();
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
    let mut narrow_type_txt: HashMap<usize, String> = HashMap::new();
    // Recognized narrowings deferred to after flow-edge minting, so the region
    // cutoff can read the edges (`apply` below) — (subject, refined type, FULL
    // guarded-block region, block scope).
    let mut pending_narrow: Vec<(String, crate::model::file_analysis::InferredType, Span, ScopeId)> =
        Vec::new();
    // Class-body scopes (`register_class_body`), for the member-targeted
    // flow (`@flow.target.member`): the witness lands where field readers look.
    let mut class_body_scopes: std::collections::HashSet<ScopeId> = std::collections::HashSet::new();
    // Region-shaped narrowings, resolved after the loop (the guard's own
    // captures may follow the region node in event order): `@narrow.after`
    // holds from the node's END to the enclosing scope's end, `@narrow.within`
    // over the node itself — each in the scope open at the node.
    let mut narrow_after: Vec<(usize, Point, ScopeId)> = Vec::new();
    let mut narrow_within: Vec<(usize, Span, ScopeId)> = Vec::new();
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
        if let Some(which) = e.cap.strip_prefix("member.op.") {
            if let Some(op) = member_op_suffix(which) {
                member_op_by_span.insert((e.start, e.end), op);
            }
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
    out.by_ref_params = by_ref_params;
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
            while context_stack.last().is_some_and(|(d, _, _)| *d > scope_stack.len()) {
                context_stack.pop();
            }
        }
        let cur_scope = scope_stack.last().unwrap().1;
        let package: Option<String> = context_stack.last().map(|(_, p, _)| p.clone());
        // The innermost CLASS context, spelled the way THIS file spells it —
        // what a receiver that names "the class I am written in" desugars
        // to. Distinct from `package`, which is whatever context is open (a
        // namespace body has one too). A use-map language writes the
        // declaration's own leaf, which its use-map resolves back to the
        // identity; an identity language writes the identity. Either way a
        // consumer that re-resolves the spelling lands on the same class.
        let enclosing_class: Option<String> = context_stack
            .iter()
            .rev()
            .find(|(_, _, is_class)| *is_class)
            .map(|(_, p, _)| match pack.names.use_map_sep() {
                Some(sep) => p.rsplit(sep).next().unwrap_or(p).to_string(),
                None => p.clone(),
            });
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
                    while context_stack.last().is_some_and(|(d, _, _)| *d >= scope_stack.len()) {
                        context_stack.pop();
                    }
                    // The receiver name (`$this`) IS this class inside its
                    // body — a witness at the body scope, so a chain based on
                    // it (`$this->mailer->send()`) resolves through the same
                    // registry chase as any typed variable. Class bodies
                    // only: a namespace body carries a context too.
                    let is_class =
                        names_by_match.contains_key(&(e.match_id, "def.class".to_string()));
                    if is_class {
                        register_class_body(&mut out, &receiver_tokens, id, &text, e.start);
                        class_body_scopes.insert(id);
                    }
                    context_stack.push((scope_stack.len(), text, is_class));
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
                    // (the engagement forms peel `T` from it). The declared type
                    // resolves up the guard's scope chain (innermost first), so a
                    // same-named var in a sibling function never supplies the
                    // inner type — the nearest enclosing declaration wins.
                    let captured = narrow_type_txt.get(&nmid).cloned();
                    let declared = match captured {
                        Some(_) => None,
                        None => scope_stack.iter().rev().find_map(|&(_, sid)| {
                            annot_text_by_var.get(&(subject.clone(), sid)).cloned()
                        }),
                    };
                    // A refinement that lands back on the subject's own
                    // declaration refines nothing, and the witness would shadow
                    // a stronger refinement already in force at that point.
                    let refines_nothing = |r: &crate::model::file_analysis::InferredType| {
                        declared.as_deref().and_then(pack.annot_type).as_ref() == Some(r)
                    };
                    if let Some(refined) = captured
                        .or_else(|| declared.clone())
                        .and_then(|t| (pack.narrow_type)(&t))
                        .filter(|r| !refines_nothing(r))
                        .map(|r| ident_type(r, e.start))
                    {
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
                narrow_type_txt.insert(e.match_id, e.text.clone());
            }
            "narrow.after" => {
                if let Some(&(_, sid)) = scope_stack.last() {
                    narrow_after.push((e.match_id, e.end, sid));
                }
            }
            "narrow.within" => {
                if let Some(&(_, sid)) = scope_stack.last() {
                    narrow_within.push((e.match_id, Span { start: e.start, end: e.end }, sid));
                }
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
                // A name-less def's context is its synthesized identity,
                // never the anchor token's text.
                let raw = names_by_match
                    .get(&(e.match_id, "def.class".to_string()))
                    .filter(|_| pack.names.use_map_sep().is_some())
                    .map(|(n, _, _)| n.clone())
                    .or_else(|| defaulted_matches.get(&e.match_id).cloned())
                    .unwrap_or_else(|| e.text.clone());
                let text = (pack.shape_name)(&e.cap, &raw);
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
                        .is_some_and(|(d, _, _)| *d >= scope_stack.len())
                    {
                        context_stack.pop();
                    }
                    // php puts the class scope on the whole declaration, so
                    // the body scope is ALREADY open here: it carries the
                    // class as its package and the receiver witness.
                    let is_class =
                        names_by_match.contains_key(&(e.match_id, "def.class".to_string()));
                    if is_class {
                        let id = scope_stack.last().unwrap().1;
                        register_class_body(&mut out, &receiver_tokens, id, &raw, e.start);
                        class_body_scopes.insert(id);
                    }
                    context_stack.push((scope_stack.len(), raw, is_class));
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
            // A registration string (`add_action('init', …)` arg 1)
            // DECLARES a name on its rail — a Handler symbol whose name and
            // span are the string content, stacking like every same-named
            // Handler. A firing string (`do_action('init')`) mints the
            // DispatchCall ref that matches it. The capture's suffix names
            // the rail and is REQUIRED: an unnamed namespace would claim
            // the whole program for one framework's hook space, so an
            // unsuffixed capture mints nothing and the overlay lint says so.
            c if super::rail_of(c).is_some_and(|(k, _)| k.is_handler()) => {
                let Some((kind, rail)) = super::rail_of(c) else { continue };
                let span = Span { start: e.start, end: e.end };
                let mut attributes = Vec::new();
                let mut name = e.text.clone();
                if kind == super::RailCapture::ClassHandlerNamedByMatch {
                    // named by another token of the match; no name → no handler
                    let Some(n) = handler_name_by_match.get(&e.match_id) else { continue };
                    name = n.clone();
                }
                if kind.is_class_named() {
                    out.class_rails.push((span, rail.to_string()));
                    attributes.push("class_rail".to_string());
                } else {
                    out.rails.push((span, rail.to_string()));
                }
                out.symbols.push(SkelSymbol {
                    declared_with: None,
                    return_annotation: None,
                    name,
                    kind: "handler".to_string(),
                    start: e.start,
                    end: e.end,
                    name_start: e.start,
                    name_end: e.end,
                    package: None,
                    scope: cur_scope,
                    declared_return: None,
                    deref_stack: Vec::new(),
                    attributes,
                    arity: None,
                    params: Vec::new(),
                    qualifier_owned: false,
                    doc: None,
                    deprecation: None,
                    flags: Default::default(),
                });
            }
            "handler.name" => {}
            "key.elem" => {}
            "def.handler.key" => {
                if let Some(elem) = key_elem_by_match.get(&e.match_id) {
                    out.key_defs.push(crate::build::query_extract::KeyDef {
                        key: e.text.clone(),
                        key_span: Span { start: e.start, end: e.end },
                        elem_span: *elem,
                    });
                }
            }
            cap if cap.starts_with("def.") && !cap.ends_with(".name") && !cap.ends_with(".anchor") => {
                let kind = cap.strip_prefix("def.").unwrap().to_string();
                let (name, name_start, name_end, defaulted) = names_by_match
                    .get(&(e.match_id, e.cap.clone()))
                    .cloned()
                    .map(|(n, s, en)| {
                        let d = defaulted_matches.contains_key(&e.match_id);
                        (n, s, en, d)
                    })
                    .or_else(|| {
                        (pack.default_name)(&kind, e.start.row, e.start.column)
                            .map(|n| (n, e.start, e.start, true))
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
                    .map(|q| match pack.names.sep() {
                        Some(sep) => q.rsplit(sep).next().unwrap_or(q).to_string(),
                        None => q.to_string(),
                    })
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
                defs_by_match.entry(e.match_id).or_default().push(out.symbols.len() as u32);
                out.symbols.push(SkelSymbol {
                    declared_with: None,
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
                        // `#[Deprecated]` is the attribute spelling of `@deprecated`
                        if deprecated_matches.contains(&e.match_id)
                            && !a.iter().any(|x| x == "deprecated")
                        {
                            a.push("deprecated".to_string());
                        }
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
                    params: Vec::new(),
                    doc: None,
                    deprecation: None,
                    // A member whose declarator says its value is invoked
                    // (`int (*read)(char *)`): the declaration carries the
                    // fact, so a call landing on it needs no name list.
                    flags: if nested_callable.contains(&e.match_id) {
                        crate::model::file_analysis::SymbolFlags::CALLABLE_VALUE
                    } else {
                        Default::default()
                    },
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
                // A receiver the document tags `@receiver.self` names the
                // class it is written in, so the invocant IS that class —
                // minted here, where the class-body scope has it, rather
                // than left as a token every consumer would have to know.
                // Late static binding over-approximates to the writing
                // class (accepted).
                let text = match (self_recv_matches.contains(&e.match_id), &enclosing_class) {
                    (true, Some(cls)) => cls.clone(),
                    _ => (pack.shape_name)("member.recv", &e.text),
                };
                member_recv.insert(
                    e.match_id,
                    (crate::model::file_analysis::Span { start: e.start, end: e.end }, text),
                );
            }
            // A call whose callee makes its ENCLOSING callable read
            // arguments it never declared, or materialize variables no
            // declaration names. Recorded against the call's scope; the
            // scope chain names the callable, so the fact lands on the
            // callable's own symbol (rule #14).
            "call.dynamic_args" => out.dynamic_markers.push((
                cur_scope,
                crate::model::file_analysis::SymbolFlags::DYNAMIC_ARGS,
            )),
            "call.dynamic_vars" => out.dynamic_markers.push((
                cur_scope,
                crate::model::file_analysis::SymbolFlags::DYNAMIC_VARS,
            )),
            "member.op" => {
                // The operator the document NAMED at this span
                // (`@member.op.arrow` / `.dot`). An operator the document
                // names neither way (`.*`) has no entry and gets no op-DX —
                // never a re-decision from the token's text.
                if let Some(op) = member_op_by_span.get(&(e.start, e.end)) {
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
                // `'A\\B\\X::method'`: a static-method callable string. The
                // class part is a qualified spelling (pinned like any other),
                // the ref a Callable member on that class — the same identity
                // `[X::class, 'method']` carries, so the backward walk finds it.
                let split = pack
                    .names
                    .member_sep()
                    .zip(pack.names.use_map_sep())
                    .and_then(|(msep, sep)| e.text.split_once(msep).map(|(q, m)| (q, m, sep)));
                if let Some((qual, method, sep)) = split {
                    let (leaf, ns) = split_ns_leaf(qual, sep);
                    if !ns.is_empty() {
                        // a class named through a member qualifier reaches the
                        // global namespace outright (`A\F::cb`, never relative)
                        out.qualified_spellings.push(
                            crate::model::file_analysis::QualifiedSpelling {
                                leaf: leaf.clone(),
                                segments: ns.split(sep).map(str::to_string).collect(),
                                absolute: true,
                            },
                        );
                    }
                    // The ref's span is the METHOD tail only — rename rewrites
                    // exactly those characters; the invocant span is the
                    // qualifier ahead of the `::`.
                    let method_start = Point {
                        row: e.end.row,
                        column: e.end.column.saturating_sub(method.len()),
                    };
                    let qual_span = Span {
                        start: e.start,
                        end: Point { row: e.start.row, column: e.start.column + qual.len() },
                    };
                    out.refs.push(SkelRef {
                        via: None,
                        kind: "member".to_string(),
                        name: method.to_string(),
                        start: method_start,
                        end: e.end,
                        scope: cur_scope,
                        // A string names its class ABSOLUTELY (`'A\F::cb'`
                        // is `\A\F`, never relative to the namespace): the
                        // invocant spells it so, and the use-map resolution
                        // every bareword receiver goes through keeps it.
                        invocant: Some((qual_span, format!("{sep}{}", qual.trim_start_matches(sep)))),
                        member_op: None,
                        arg_count: None,
                        named_by_string: false,
                        flags: Default::default(),
                    });
                    continue;
                }
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
                    named_by_string: false,
                    flags: Default::default(),
                });
            }
            c if super::rail_of(c).is_some_and(|(k, _)| !k.is_handler()) => {
                let Some((kind, rail)) = super::rail_of(c) else { continue };
                let span = Span { start: e.start, end: e.end };
                if kind.is_class_named() {
                    out.class_rails.push((span, rail.to_string()));
                } else {
                    out.rails.push((span, rail.to_string()));
                }
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
                    named_by_string: false,
                    flags: Default::default(),
                });
            }
            // consumed by the prepass join above; nothing to mint here
            "dispatch.via" => {}
            // `.self` flavor: the string names a method of the ENCLOSING
            // class (a PHPUnit attribute argument) — no receiver node
            // exists, so the invocant is that class, taken from the scope
            // the attribute is written in.
            "ref.method.named.self" => {
                out.refs.push(SkelRef {
                    via: None,
                    kind: "member".to_string(),
                    name: e.text.clone(),
                    start: e.start,
                    end: e.end,
                    scope: cur_scope,
                    invocant: enclosing_class.clone().map(|cls| {
                        (crate::model::file_analysis::Span { start: e.start, end: e.end }, cls)
                    }),
                    member_op: None,
                    arg_count: None,
                    named_by_string: true,
                    flags: Default::default(),
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
                        named_by_string: true,
                        flags: Default::default(),
                    });
                }
            }
            // `@ref.method.ctor`: the constructor NAMED at a call site. Read
            // in the pre-pass as a span and stamped onto the reference the
            // member pattern already minted — it declares nothing of its own.
            "ref.method.ctor" => {}
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
                    // A SUPER receiver (php `parent::`) spells the model's
                    // SUPER method token: dispatch starts above the writing
                    // class, and gd/references/rename ride the existing
                    // SUPER lane. The invocant becomes the enclosing class
                    // (the receiver is still this object); the ref span
                    // stays the bare name token, so rename rewrites only
                    // the name.
                    // A call of a name the pack declares a dynamic-argument
                    // or dynamic-variable marker makes its ENCLOSING callable
                    // one. Recorded against the call's scope; the skeleton's
                    // scope chain names the callable, so the fact lands on the
                    // callable's own symbol instead of waiting for a consumer
                    // to join spans (rule #14).
                    let super_recv = e.cap == "ref.member" && super_recv_matches.contains(&e.match_id);
                    out.refs.push(SkelRef {
                        via: None,
                        kind: e.cap.strip_prefix("ref.").unwrap().to_string(),
                        name: if super_recv {
                            crate::model::conventions::MethodToken::Super(&(pack.shape_name)(&e.cap, &e.text))
                                .render()
                        } else {
                            (pack.shape_name)(&e.cap, &e.text)
                        },
                        start: e.start,
                        end: e.end,
                        scope: cur_scope,
                        invocant: if super_recv {
                            member_recv.get(&e.match_id).and_then(|(sp, _)| {
                                enclosing_class.clone().map(|cls| (*sp, cls))
                            })
                        } else {
                            member_recv.get(&e.match_id).cloned()
                        },
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
                        named_by_string: false,
                        flags: Default::default(),
                    });
                    if matches!(e.cap.as_str(), "ref.call" | "ref.qcall" | "ref.member") {
                        if let Some(vars) = arg_vars_by_start.get(&(e.end.row, e.end.column)) {
                            use crate::model::witnesses as wit;
                            let callee = (pack.shape_name)(&e.cap, &e.text);
                            bind_call_args(&mut out.witnesses, vars, cur_scope, |index| {
                                if e.cap == "ref.member" {
                                    let (inv_span, inv_text) = member_recv.get(&e.match_id)?;
                                    let base = if member_simple.get(&e.match_id).copied().unwrap_or(false) {
                                        wit::WitnessAttachment::Variable {
                                            name: (pack.shape_name)("def.var", inv_text),
                                            scope: cur_scope,
                                        }
                                    } else {
                                        wit::WitnessAttachment::Expr(*inv_span)
                                    };
                                    Some(wit::WitnessPayload::Projected {
                                        base,
                                        step: wit::ProjectionStep::ParamOf {
                                            member: callee.clone(),
                                            index,
                                        },
                                    })
                                } else {
                                    let (pkg, bare) =
                                        crate::model::file_analysis::split_qualified(&callee, &pack.names);
                                    Some(wit::WitnessPayload::Edge(wit::WitnessAttachment::Param {
                                        package: pkg
                                            .map(str::to_string)
                                            // A bare call names a free function, and a
                                            // free function belongs to the enclosing
                                            // NAMESPACE — never to the class whose body
                                            // the call sits in. The callee's own bag
                                            // keys its parameters that way.
                                            .or_else(|| {
                                                context_stack
                                                    .iter()
                                                    .rev()
                                                    .find(|(_, _, is_class)| !*is_class)
                                                    .map(|(_, p, _)| p.clone())
                                            })
                                            .unwrap_or_default(),
                                        name: bare.to_string(),
                                        index,
                                    }))
                                }
                            });
                        }
                    }
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
                                self_recv_matches.contains(&e.match_id),
                                cur_scope,
                                arg_counts_by_start
                                    .get(&(e.end.row, e.end.column))
                                    .map(|n| *n as u32),
                                package.as_deref(),
                                &|c| ident(c, e.start),
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
                        self_recv_matches.contains(&e.match_id),
                        cur_scope,
                        arg_counts_by_start
                            .get(&(e.end.row, e.end.column))
                            .map(|n| *n as u32),
                        package.as_deref(),
                        &|c| ident(c, e.start),
                    );
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
            "expr.read.var"
                if not_a_read.contains(&(e.start_byte, e.end_byte))
                    || def_name_ends.contains(&e.end_byte) => {}
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
            // A member of the enclosing class as the flow target
            // (`$this->foo = …`): the witness belongs at the CLASS scope,
            // where the field's readers look, not at the method's.
            "flow.target.member" => {
                let class_scope = scope_stack
                    .iter()
                    .rev()
                    .map(|&(_, sid)| sid)
                    .find(|sid| class_body_scopes.contains(sid))
                    .unwrap_or(cur_scope);
                flow_targets.insert(
                    e.match_id,
                    ((pack.shape_name)("def.var", &e.text), class_scope, e.start),
                );
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
            // The command's effect, as the document names it: a def of the
            // capture's kind at the captured argument, a reference per captured
            // argument, an import of the captured file.
            cap if cap.starts_with("cmd.def.") && cap != "cmd.def.name" => {
                cmd_defs.insert(
                    e.match_id,
                    (cap["cmd.def.".len()..].to_string(), e.start, cur_scope),
                );
            }
            "cmd.def.name" => {
                if let Some((kind, cmd_start, scope)) = cmd_defs.get(&e.match_id) {
                    out.symbols.push(SkelSymbol {
                        declared_with: None,
                        flags: Default::default(),
                        return_annotation: None,
                        kind: kind.clone(),
                        name: e.text.clone(),
                        start: *cmd_start,
                        end: e.end,
                        name_start: e.start,
                        name_end: e.end,
                        package: None,
                        scope: *scope,
                        declared_return: None,
                        deref_stack: Vec::new(),
                        attributes: Vec::new(),
                        arity: None,
                        params: Vec::new(),
                        qualifier_owned: false,
                        doc: None,
                        deprecation: None,
                    });
                }
            }
            "cmd.refargs" => {
                // ALL-CAPS keyword arguments (PRIVATE/STATIC) are CMake's
                // keyword convention, and an interpolation names no one
                // symbol — neither is a reference.
                let is_keyword =
                    !e.text.is_empty() && e.text.chars().all(|c| c.is_ascii_uppercase() || c == '_');
                if !is_keyword && !e.text.contains("${") {
                    out.refs.push(SkelRef {
                        via: None,
                        kind: "call".into(),
                        name: e.text.clone(),
                        start: e.start,
                        end: e.end,
                        scope: cur_scope,
                        invocant: None,
                        member_op: None,
                        arg_count: None,
                        named_by_string: false,
                        flags: Default::default(),
                    });
                }
            }
            "cmd.import" => {
                if !out.imports.contains(&e.text) {
                    out.imports.push(e.text.clone());
                }
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
            // `@expr.annot`: an expression whose VALUE is the class the same
            // match's `@type.annot` names (`app(Foo::class)` is a Foo — a
            // container resolves what the argument spells). Declared by an
            // overlay, so it rides the plugin priority: the expression's own
            // evidence outranks the callee-return derivation.
            "expr.annot" => {
                if let Some(annot) = annot_by_match.get(&e.match_id) {
                    if let Some(InferredType::ClassName(cn)) = annot_ident(annot, e.start) {
                        let span = Span { start: e.start, end: e.end };
                        lit_spans.push((e.start_byte, e.end_byte, span));
                        out.annot_expr_spans.push(span);
                        out.witnesses.push(crate::model::witnesses::Witness {
                            attachment: crate::model::witnesses::WitnessAttachment::Expr(span),
                            source: crate::model::witnesses::WitnessSource::Plugin(
                                "overlay-annot".into(),
                            ),
                            payload: crate::model::witnesses::WitnessPayload::Edge(
                                crate::model::witnesses::WitnessAttachment::TypeName(cn),
                            ),
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

    // A declaration token whose query DECLARED the pair and minted exactly
    // two defs declared both: link them each way where they were minted, so
    // goto-def, references and rename read the relation instead of
    // reconstructing it from spans (rule #11).
    for mid in &codeclared_matches {
        let Some(defs) = defs_by_match.get(mid) else { continue };
        let [a, b] = defs[..] else { continue };
        out.symbols[a as usize].declared_with =
            Some(crate::model::file_analysis::SymbolId(b));
        out.symbols[b as usize].declared_with =
            Some(crate::model::file_analysis::SymbolId(a));
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
        // Keyed per name site AND per kind family: a framework overlay
        // legitimately declares a PROPERTY at a method's own name token
        // (Eloquent relations — `pages()` the method, `->pages` the
        // accessor) or a HANDLER there (a listener's `handle(X $e)` is the
        // event rail's handler named X), and those pairs must survive while
        // the same-kind duplicates (var vs sub, rettype twins) still collapse.
        // Handlers key by NAME too: one token can carry several rails'
        // handlers (a listener's `handle(X $e)` is X's handler AND its own
        // class's job handler).
        let family = |kind: &str| match kind {
            "field" => 1u8,
            "handler" => 2u8,
            _ => 0u8,
        };
        let mut best: HashMap<(usize, usize, u8, String), usize> = HashMap::new();
        let mut keep = vec![true; out.symbols.len()];
        for (i, sym) in out.symbols.iter().enumerate() {
            let tag = if sym.kind == "handler" { sym.name.clone() } else { String::new() };
            let key = (sym.name_start.row, sym.name_start.column, family(&sym.kind), tag);
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
        // Renumbering invalidates symbol ids, and the co-declaration pairs
        // minted above are the only ones held this early — remap them here
        // (a partner that did not survive leaves no link).
        let mut renumbered: Vec<Option<u32>> = vec![None; keep.len()];
        let mut next = 0u32;
        for (i, k) in keep.iter().enumerate() {
            if *k {
                renumbered[i] = Some(next);
                next += 1;
            }
        }
        let mut it = keep.iter();
        out.symbols.retain(|_| *it.next().unwrap());
        for sym in out.symbols.iter_mut() {
            sym.declared_with = sym.declared_with.and_then(|id| {
                renumbered[id.0 as usize].map(crate::model::file_analysis::SymbolId)
            });
        }
        // The pair the dedup just kept IS a relation, so mint it as one
        // (rule #11): a stored member and a callable declared at ONE name
        // token are two spellings of a single declaration (an Eloquent
        // relation — `pages()` the method, `->pages` the accessor through
        // `__get`), and without the fact a rename of either spelling leaves
        // the other stale. Pairs the query already declared
        // (`CODECLARED_SUFFIX`) keep theirs.
        let mut by_site: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for (i, sym) in out.symbols.iter().enumerate() {
            by_site.entry((sym.name_start.row, sym.name_start.column)).or_default().push(i);
        }
        for at in by_site.values() {
            let stored = at.iter().copied().find(|&i| out.symbols[i].kind == "field");
            let callable =
                at.iter().copied().find(|&i| matches!(out.symbols[i].kind.as_str(), "method" | "sub"));
            let (Some(f), Some(c)) = (stored, callable) else { continue };
            if out.symbols[f].declared_with.is_some() || out.symbols[c].declared_with.is_some() {
                continue;
            }
            out.symbols[f].declared_with = Some(crate::model::file_analysis::SymbolId(c as u32));
            out.symbols[c].declared_with = Some(crate::model::file_analysis::SymbolId(f as u32));
        }
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
            named_by_string: false,
            flags: Default::default(),
        });
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
    // The region shapes join here, once every guard capture is in.
    let regions = narrow_after
        .into_iter()
        .map(|(mid, at, sid)| (mid, Span { start: at, end: out.scopes[sid.0 as usize].span.end }, sid))
        .chain(narrow_within);
    for (mid, region, sid) in regions {
        let (Some(var), Some(ty)) = (narrow_var.get(&mid), narrow_type_txt.get(&mid)) else {
            continue;
        };
        if let Some(refined) = (pack.narrow_type)(ty).map(|r| ident_type(r, region.start)) {
            pending_narrow.push(((pack.shape_name)("ref.var", var), refined, region, sid));
        }
    }
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
    // ---- documentation-comment types: pack vocabulary, query join ----
    // The query pairs a doc comment with the def it documents (`@doc.comment`
    // and `@doc.subject` in ONE match), and `@doc.subject` sits on the very
    // node the def's own capture sits on — so the two meet at one point and
    // nothing here measures a distance. DECLARED types always win: a doc fact
    // fills only where the syntax carried nothing, because docblocks drift and
    // the tree doesn't. Perl/C++ packs return no facts, so the pass is a no-op
    // there.
    {
        use crate::build::query_extract::DocFact;
        // Keyed by the SUBJECT's start point; the comment's start row rides
        // along so a `@method` fact can span its own line inside the comment.
        let mut by_subject: HashMap<(usize, usize), (usize, Vec<DocFact>)> = HashMap::new();
        // The bound names imports bring in, for the doc-mention scan below:
        // an import used only in a docblock (`@var Foo $x`) is used.
        let bound: std::collections::HashSet<String> = out
            .import_sites
            .iter()
            .map(|r| match pack.names.sep() {
                Some(sep) => r.raw.rsplit(sep).next().unwrap_or(&r.raw).to_string(),
                None => r.raw.clone(),
            })
            .chain(out.use_aliases.iter().map(|(alias, _, _)| alias.clone()))
            .collect();
        // The subject each match names, so the comment of that same match
        // knows what it documents. A bare `@doc.comment` with no subject is
        // the mention scan's input only.
        let mut subject_by_match: HashMap<usize, Point> = HashMap::new();
        for e in &events {
            if e.cap == "doc.subject" {
                subject_by_match.insert(e.match_id, e.start);
            }
        }
        for e in &events {
            if e.cap == "doc.comment" {
                if !bound.is_empty() {
                    for word in e.text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                        if bound.contains(word) && !out.doc_mentions.iter().any(|m| m == word) {
                            out.doc_mentions.push(word.to_string());
                        }
                    }
                }
                let Some(subject) = subject_by_match.get(&e.match_id) else { continue };
                let facts = (pack.doc_types)(&e.text, pack.doc_uses_method_tags);
                if !facts.is_empty() {
                    let entry = by_subject
                        .entry((subject.row, subject.column))
                        .or_insert_with(|| (e.start.row, Vec::new()));
                    entry.1.extend(facts);
                }
            }
        }
        if !by_subject.is_empty() {
            let scope_spans: Vec<Span> = out.scopes.iter().map(|s| s.span).collect();
            let param_syms: Vec<(String, crate::model::file_analysis::ScopeId, Point, Point)> =
                out.symbols
                    .iter()
                    .filter(|s| s.kind == "var")
                    .map(|s| (s.name.clone(), s.scope, s.start, s.end))
                    .collect();
            let mut doc_witnesses: Vec<crate::model::witnesses::Witness> = Vec::new();
            let mut disagreements: Vec<crate::model::file_analysis::DocDisagreement> = Vec::new();
            // The declared type of a slot, for the hint that reports the pair.
            let declared_of = |slot: (&str, crate::model::file_analysis::ScopeId)| {
                annot_text_by_var
                    .get(&(slot.0.to_string(), slot.1))
                    .and_then(|d| (pack.annot_type)(d))
            };
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
                        by_subject.get(&(sym.start.row, sym.start.column))
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
                        if let DocFact::Description(d) = f {
                            sym.doc = Some(d.clone());
                            continue;
                        }
                        if let DocFact::Deprecated(t) = f {
                            mark_deprecated(sym, t.clone());
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
                                declared_with: None,
                                return_annotation: None,
                                kind: "method".to_string(),
                                name: name.clone(),
                                start: at,
                                end: at_end,
                                name_start: at,
                                name_end: at_end,
                                package: Some(sym.name.clone()),
                                scope: sym.scope,
                                declared_return: ret
                                    .as_deref()
                                    .and_then(|t| declared_ret(t, at)),
                                deref_stack: Vec::new(),
                                // documentation, not a declaration: no body, no annotation to add
                                attributes: vec!["documented".to_string()],
                                arity: None,
                                params: Vec::new(),
                                qualifier_owned: false,
                                doc: None,
                                deprecation: None,
                                flags: Default::default(),
                            });
                        }
                    }
                    continue;
                }
                if !matches!(sym.kind.as_str(), "sub" | "method" | "field" | "anon" | "var") {
                    continue;
                }
                let Some((cstart, facts)) = by_subject.get(&(sym.start.row, sym.start.column))
                else {
                    continue;
                };
                let cstart = *cstart;
                for f in facts {
                    match f {
                        // class-docblock facts; no callable/field join
                        DocFact::Method { .. } | DocFact::Template { .. } => {}
                        DocFact::Description(d) => {
                            if !matches!(sym.kind.as_str(), "var" | "anon") {
                                sym.doc = Some(d.clone());
                            }
                        }
                        DocFact::Deprecated(t) => mark_deprecated(sym, t.clone()),
                        DocFact::ReturnRecvInstance { base } => {
                            // `@return Base<static>`: an instance of `base`
                            // parametrized by the receiver — `Book::query()`
                            // carries `Builder<Book>`, and a later
                            // `@return TModel` hop projects `Book` out via
                            // the same `ParamOf` axis cpp instantiations use.
                            if sym.declared_return.is_none() {
                                use crate::model::witnesses::{ParametricOp, ReturnExpr};
                                sym.declared_return =
                                    Some(ReturnExpr::Operator(ParametricOp::InstanceOf {
                                        base: ident(base, sym.start),
                                        args: vec![ReturnExpr::Receiver],
                                    }));
                            }
                        }
                        DocFact::Return(t) => {
                            use crate::model::witnesses::ReturnExpr;
                            // A doc row fills an undeclared return and NARROWS
                            // a declared one — the same rule `doc_admits`
                            // applies to params and properties. It never
                            // widens, and a contradiction leaves the
                            // declaration standing and states itself.
                            let Some(doc) = declared_ret(t, sym.start) else { continue };
                            match (&sym.declared_return, &doc) {
                                (None, _) => sym.declared_return = Some(doc),
                                (
                                    Some(ReturnExpr::Concrete(declared)),
                                    ReturnExpr::Concrete(documented),
                                ) => match doc_verdict(declared, documented, &out.parents) {
                                    DocVerdict::Narrows => sym.declared_return = Some(doc.clone()),
                                    DocVerdict::Contradicts => {
                                        disagreements.push(
                                            crate::model::file_analysis::DocDisagreement {
                                                span: Span {
                                                    start: sym.name_start,
                                                    end: sym.name_end,
                                                },
                                                declared: declared.clone(),
                                                documented: documented.clone(),
                                            },
                                        );
                                    }
                                    DocVerdict::Unknown => {}
                                },
                                // A receiver-shaped return on either side is a
                                // late binding, not a value shape: neither
                                // narrows the other, and the declaration
                                // stands.
                                (Some(_), _) => {}
                            }
                        }
                        DocFact::UsesMethod { name, line, col } => {
                            // A method REF on the enclosing class, spanning
                            // the NAME TOKEN in the docblock — the named
                            // method gains real fan-in, and rename rewrites
                            // the token in place. Only meaningful on class
                            // members (the invocant is the class).
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
                                    named_by_string: false,
                                    flags: Default::default(),
                                });
                            }
                        }
                        DocFact::Var { ty: t, name: var_name } => {
                            // The NAMED inline form (`/** @var Type[] $rows */`
                            // above an assignment) types that specific local.
                            if let Some(vn) = var_name {
                                if sym.kind == "var" && &sym.name == vn {
                                    if let Some(ty) = annot_ident(t, sym.start) {
                                        doc_witnesses.push(doc_cast_witness(
                                            &sym.name,
                                            sym.scope,
                                            ty,
                                            Span { start: sym.start, end: sym.start },
                                        ));
                                    }
                                    continue;
                                }
                                // `@var T $prop` above a PROPERTY (WordPress
                                // spells every field doc with its name) is
                                // that field's doc, not a local cast.
                                if !(sym.kind == "field"
                                    && vn.trim_start_matches(|c| pack.names.is_sigil(c)) == sym.name)
                                {
                                    continue;
                                }
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
                                let Some(ty) = annot_ident(t, sym.start) else { continue };
                                let slot = (sym.name.as_str(), sym.scope);
                                let verdict =
                                    doc_admits(pack, &annot_text_by_var, slot, &ty, &out.parents);
                                if verdict == DocVerdict::Contradicts {
                                    if let Some(declared) = declared_of(slot) {
                                        disagreements.push(
                                            crate::model::file_analysis::DocDisagreement {
                                                span: Span { start: sym.start, end: sym.end },
                                                declared,
                                                documented: ty.clone(),
                                            },
                                        );
                                    }
                                }
                                if verdict == DocVerdict::Narrows {
                                    let span = scope_spans
                                        .get(sym.scope.0 as usize)
                                        .copied()
                                        .unwrap_or(Span { start: sym.start, end: sym.start });
                                    // An ANNOTATION, like the declared field
                                    // type it stands in for: it outranks what
                                    // a constructor happens to write to the
                                    // field (`@var A|B $skin` + `$this->skin =
                                    // new B` reads as the documented union).
                                    let mut w = doc_witness(&sym.name, sym.scope, ty, span);
                                    w.source = crate::model::witnesses::WitnessSource::Annotation(
                                        crate::model::witnesses::AnnotationKind::Declared,
                                    );
                                    doc_witnesses.push(w);
                                }
                            }
                        }
                        DocFact::Param { name, ty } => {
                            // The def's own parameter — untyped, or a bare
                            // container the doc refines (same rule as Var).
                            let Some(ty) = annot_ident(ty, sym.start) else { continue };
                            let in_def = |p: Point| {
                                (p.row, p.column) >= (sym.start.row, sym.start.column)
                                    && (p.row, p.column) <= (sym.end.row, sym.end.column)
                            };
                            let mine = param_syms
                                .iter()
                                .find(|(n, _, at, _)| n == name && in_def(*at));
                            if let Some((n, sc, at, end)) = mine {
                                match doc_admits(
                                    pack,
                                    &annot_text_by_var,
                                    (n, *sc),
                                    &ty,
                                    &out.parents,
                                ) {
                                    DocVerdict::Contradicts => {
                                        if let Some(declared) = declared_of((n, *sc)) {
                                            disagreements.push(
                                                crate::model::file_analysis::DocDisagreement {
                                                    span: Span { start: *at, end: *end },
                                                    declared,
                                                    documented: ty.clone(),
                                                },
                                            );
                                        }
                                        continue;
                                    }
                                    DocVerdict::Unknown => continue,
                                    DocVerdict::Narrows => {}
                                }
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
                        crate::model::witnesses::WitnessSource::Annotation(
                            crate::model::witnesses::AnnotationKind::Declared
                        )
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
            // site on — the `$x = Factory::make(); /** @var Concrete $x */`
            // idiom that narrows a base-typed factory return. The subject IS
            // the assignment target, so the rebind is found at its own point.
            for ((row, column), (_, facts)) in &by_subject {
                let at = Point { row: *row, column: *column };
                for f in facts {
                    let DocFact::Var { ty, name: Some(vn) } = f else { continue };
                    let Some(t) = annot_ident(ty, at) else { continue };
                    let has_def =
                        out.symbols.iter().any(|s| s.kind == "var" && &s.name == vn && s.start == at);
                    if has_def {
                        continue;
                    }
                    if let Some(fe) = out
                        .flow_edges
                        .iter()
                        .find(|fe| &fe.target_name == vn && fe.target_at == at)
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
            out.doc_disagreements.extend(disagreements);
        }
    }
    // What a `@<fact>.target` capture said about a DECLARATION, stamped as
    // flags — one carriage for the whole family, so a fact minted here and
    // the same fact written as an attribute token arrive as the same bit.
    // (`sym.attributes` stays what a human reads, never what the model asks.)
    if !nonpublic_name_spans.is_empty()
        || !classattr_by_name_span.is_empty()
        || !static_name_spans.is_empty()
        || !contract_name_spans.is_empty()
        || !alias_name_ends.is_empty()
        || !receiver_name_spans.is_empty()
        || !ctor_name_spans.is_empty()
        || !throwaway_name_spans.is_empty()
    {
        for sym in &mut out.symbols {
            if receiver_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::RECEIVER;
            }
            if ctor_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::CONSTRUCTOR;
            }
            if throwaway_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::THROWAWAY;
            }
            if sym.kind == "var" && alias_name_ends.contains(&sym.name_end) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::ALIAS;
            }
            if contract_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::CONTRACT;
            }
            if nonpublic_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::NON_PUBLIC;
            }
            if static_name_spans.contains(&(sym.name_start, sym.name_end)) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::STATIC;
            }
            if let Some(flavor) = classattr_by_name_span.get(&(sym.name_start, sym.name_end)) {
                if sym.kind == "class" && !sym.attributes.iter().any(|a| a == flavor) {
                    sym.attributes.push(flavor.clone());
                }
            }
        }
    }

    // A catch-all member is a fact about the CLASS that declares it, not
    // about the member's own name: the class answers ANY member at runtime.
    // The declaring container is the sticky package the member sits under,
    // so the flag lands on that class and every lane asks the class.
    if !catch_all_name_spans.is_empty() {
        let dynamic: std::collections::HashSet<String> = out
            .symbols
            .iter()
            .filter(|s| catch_all_name_spans.contains(&(s.name_start, s.name_end)))
            .filter_map(|s| s.package.clone())
            .collect();
        for sym in &mut out.symbols {
            if sym.kind == "class" && dynamic.contains(&sym.name) {
                sym.flags |= crate::model::file_analysis::SymbolFlags::DYNAMIC_MEMBERS;
            }
        }
    }

    // Every enum carries the members the LANGUAGE gives it (php's
    // `->value`, `::cases()`). Which containers are enums is the query's
    // word (`@classattr.enum`), read as the capture suffix it is.
    //
    // They have no token of their own, so they are
    // minted here, at the enum's name, as real members — SYNTHESIZED says
    // no source could reference them into existence. A consumer resolves
    // them through the symbol table like any other member; none matches
    // their names.
    if !pack.enum_members.is_empty() {
        let enums: Vec<(std::string::String, Point, Point, crate::model::file_analysis::ScopeId)> =
            out.symbols
                .iter()
                .filter(|s| {
                    s.kind == "class"
                        && classattr_by_name_span.get(&(s.name_start, s.name_end)).map(String::as_str)
                            == Some(ENUM_CLASSATTR)
                })
                .map(|s| {
                    // The enum's BODY scope is where its declared members
                    // live, so the synthesized ones live there too.
                    let body = out
                        .scopes
                        .iter()
                        .find(|sc| {
                            sc.package.as_deref() == Some(s.name.as_str())
                                && (sc.span.start.row, sc.span.start.column)
                                    >= (s.start.row, s.start.column)
                        })
                        .map(|sc| sc.id)
                        .unwrap_or(s.scope);
                    (s.name.clone(), s.name_start, s.name_end, body)
                })
                .collect();
        for (name, name_start, name_end, scope) in enums {
            for m in pack.enum_members {
                out.symbols.push(SkelSymbol {
                    declared_with: None,
                    flags: Default::default(),
                    declared_return: None,
                    return_annotation: None,
                    kind: if m.callable { "method" } else { "field" }.to_string(),
                    name: m.name.to_string(),
                    start: name_start,
                    end: name_end,
                    name_start,
                    name_end,
                    package: Some(name.clone()),
                    scope,
                    deref_stack: Vec::new(),
                    attributes: vec!["synthesized".to_string()],
                    arity: None,
                    params: Vec::new(),
                    qualifier_owned: false,
                    doc: None,
                    deprecation: None,
                });
            }
        }
    }
    // What the document said AT each reference site, stamped once. A
    // receiver's flavour and a constructor call are facts a capture stated;
    // a consumer that matched the token text back against a set of spellings
    // was asking a question already answered (rule #11).
    if !this_receiver_spans.is_empty()
        || !relative_scope_spans.is_empty()
        || !ctor_call_spans.is_empty()
    {
        use crate::model::file_analysis::RefFlags;
        for r in &mut out.refs {
            if let Some((inv, _)) = &r.invocant {
                if this_receiver_spans.contains(&(inv.start, inv.end)) {
                    r.flags |= RefFlags::RECEIVER_THIS;
                }
            }
            if relative_scope_spans.contains(&(r.start, r.end)) {
                r.flags |= RefFlags::RELATIVE_SCOPE;
            }
            if ctor_call_spans.contains(&(r.start, r.end)) {
                r.flags |= RefFlags::CONSTRUCTS;
            }
        }
    }
    out.param_sigs = param_sigs;
    Ok(out)
}

/// Bind every bare variable an argument list passes to the callee's
/// parameter slot — `Variable(arg) → Edge(Param)` for a plain callee,
/// `Projected{receiver, ParamOf}` through a dispatch. Only a position the
/// callee declares by reference ever answers
/// (`docs/adr/by-ref-binding.md`); the witness is zero-width at the
/// argument token, a binding rather than a narrowing region.
///
/// `payload` is the only thing a call site varies — which callee the slot
/// belongs to. A construction site knows its class statically and answers
/// with the constructor's `Param`; nothing about the mint differs, so
/// there is one body (rule #10).
fn bind_call_args(
    witnesses: &mut Vec<crate::model::witnesses::Witness>,
    vars: &[(u32, String, Point)],
    scope: crate::model::file_analysis::ScopeId,
    mut payload: impl FnMut(u32) -> Option<crate::model::witnesses::WitnessPayload>,
) {
    use crate::model::witnesses as wit;
    for (index, var, at) in vars {
        let Some(payload) = payload(*index) else { continue };
        witnesses.push(wit::Witness {
            attachment: wit::WitnessAttachment::Variable { name: var.clone(), scope },
            source: wit::WitnessSource::Builder("call_arg_binding".into()),
            payload,
            span: Span { start: *at, end: *at },
        });
    }
}

/// The `@classattr.<flavor>` suffix a container-def carries when the query
/// calls it an enumeration — the capture's own word, not the attribute
/// string a consumer would otherwise compare.
const ENUM_CLASSATTR: &str = "enum";

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

/// What a documented type says about the declared one on the same slot.
/// A docblock exists to say what the syntax could not spell, so it may only
/// NARROW — never widen, never contradict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocVerdict {
    /// The declaration already admits the documented type: the doc is the
    /// finer statement and wins the slot.
    Narrows,
    /// No value satisfies both. The declaration wins (the tree does not
    /// drift); the pair is minted so a hint can say the two disagree.
    Contradicts,
    /// Neither provable here — a class whose ancestry lives in another file,
    /// a union this lattice cannot hold. The declaration wins in silence.
    Unknown,
}

/// `doc_ty` against `declared`. Every unprovable relation is `Unknown`:
/// calling one a contradiction would report a declaration for being right.
pub(crate) fn doc_verdict(
    declared: &InferredType,
    doc_ty: &InferredType,
    parents: &[(std::string::String, std::string::String)],
) -> DocVerdict {
    use InferredType::*;
    if declared == doc_ty {
        return DocVerdict::Narrows;
    }
    match (declared, doc_ty) {
        // A union this lattice cannot hold admits each of its arms, and the
        // doc is the only place the arms are written.
        (Unknown, _) | (_, Unknown) => DocVerdict::Unknown,
        // A bare container says "a container"; the doc says of what.
        (
            HashRef | ArrayRef,
            Sequence(_) | Parametric(_) | HashWithKeys { .. } | HashRef | ArrayRef,
        ) => DocVerdict::Narrows,
        // A class admits its descendants. Local ancestry is what this side
        // can walk; a chain that leaves the file is Unknown, not a clash.
        (ClassName(base), ClassName(doc)) => {
            if local_ancestors(doc, parents).iter().any(|a| a == base) {
                DocVerdict::Narrows
            } else {
                DocVerdict::Unknown
            }
        }
        // Definite disagreements: two settled shapes no value shares.
        (
            String | Numeric | Bool,
            String | Numeric | Bool | HashRef | ArrayRef | Sequence(_) | HashWithKeys { .. }
            | ClassName(_),
        )
        | (
            HashRef | ArrayRef | Sequence(_) | HashWithKeys { .. },
            String | Numeric | Bool | ClassName(_),
        )
        | (ClassName(_), String | Numeric | Bool | HashRef | ArrayRef) => DocVerdict::Contradicts,
        // Everything else — an optional, a constraint object, a coderef, a
        // shape one side refines structurally — is a relation this side
        // cannot settle. Silence, and the declaration stands.
        _ => DocVerdict::Unknown,
    }
}

/// `class`'s ancestors along the edges THIS FILE declares, transitively.
/// Bounded by the edge count; a cycle visits each class once.
fn local_ancestors(
    class: &str,
    parents: &[(std::string::String, std::string::String)],
) -> Vec<std::string::String> {
    let mut seen: Vec<std::string::String> = Vec::new();
    let mut queue = vec![class.to_string()];
    while let Some(c) = queue.pop() {
        for (child, parent) in parents {
            if child == &c && !seen.iter().any(|s| s == parent) {
                seen.push(parent.clone());
                queue.push(parent.clone());
            }
        }
    }
    seen
}

/// Does a doc row's type get to stand beside the declared type of this
/// (name, scope) slot? A doc NARROWS a declaration (a subclass of the declared
/// class, a parametric over the declared container, a union arm) and never
/// widens or contradicts it; the verdict is `DocVerdict`, and a contradiction
/// is minted as a `DocDisagreement` for the mismatch lane instead of a witness.
fn doc_admits(
    pack: &LangPack,
    annot_text_by_var: &std::collections::HashMap<
        (std::string::String, crate::model::file_analysis::ScopeId),
        std::string::String,
    >,
    slot: (&str, crate::model::file_analysis::ScopeId),
    doc_ty: &InferredType,
    parents: &[(std::string::String, std::string::String)],
) -> DocVerdict {
    match annot_text_by_var.get(&(slot.0.to_string(), slot.1)) {
        None => DocVerdict::Narrows,
        Some(declared) => match (pack.annot_type)(declared) {
            // A spelling the pack does not read carries no claim to
            // contradict — the slot is untyped as far as this side knows.
            None => DocVerdict::Narrows,
            Some(declared) => doc_verdict(&declared, doc_ty, parents),
        },
    }
}

/// A documentation-sourced type witness on a Variable slot — its own source
/// tag (not `Annotation(Declared)`): a doc type is real typing fuel, but the inlay
/// suppression that hides hints for syntax-annotated declarations should
/// still show one here (the docblock can sit far from the use).
/// A NAMED `@var T $x` is a cast the author wrote at that site: it rides
/// at annotation priority (`Annotation(Refinement)`) so the flow / call-binding
/// edges the same assignment mints — pushed later, equal priority, and
/// latest-wins — cannot override it with the factory's declared base.
fn doc_cast_witness(
    name: &str,
    scope: crate::model::file_analysis::ScopeId,
    ty: InferredType,
    span: Span,
) -> crate::model::witnesses::Witness {
    let mut w = doc_witness(name, scope, ty, span);
    w.source = crate::model::witnesses::WitnessSource::Annotation(
        crate::model::witnesses::AnnotationKind::Refinement,
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
/// separator, a leading separator (a global-anchored spelling) trimmed. A
/// separator-less spelling is a bare leaf in the global namespace.
fn split_ns_leaf(fq: &str, sep: &str) -> (String, String) {
    let t = fq.trim_start_matches(sep);
    match t.rsplit_once(sep) {
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
    // The document tagged this receiver `@receiver.self` — it names the
    // class it is written in, so the hop bases on that class. A receiver
    // TOKEN (`$this`, `self`, `this`) is not this: it is a value the class
    // body already witnesses, and it bases as the variable it is.
    recv_names_enclosing_class: bool,
    scope: crate::model::file_analysis::ScopeId,
    arity: Option<u32>,
    enclosing_class: Option<&str>,
    class_ident: &dyn Fn(&str) -> String,
) {
    use crate::model::witnesses as wit;
    let base = if recv_names_enclosing_class {
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
                crate::model::file_analysis::InferredType::ClassName(class_ident(recv_text)),
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

/// A class body scope: its package is the class (a member declared
/// directly in the body — a constant, a property default — resolves its
/// enclosing class as this, not the namespace), and the receiver name
/// (`$this`) is witnessed as an instance of it, so every chain based on
/// the receiver resolves through the registry like any typed variable.
/// The one spelling of "this declaration is deprecated": the attribute the
/// lane reads, plus the notice hover and the diagnostic show.
fn mark_deprecated(sym: &mut crate::build::query_extract::SkelSymbol, text: Option<String>) {
    if !sym.attributes.iter().any(|a| a == "deprecated") {
        sym.attributes.push("deprecated".to_string());
    }
    if text.is_some() || sym.deprecation.is_none() {
        sym.deprecation = text;
    }
}
fn register_class_body(
    out: &mut SkeletonAnalysis,
    receivers: &[String],
    scope: crate::model::file_analysis::ScopeId,
    class: &str,
    at: Point,
) {
    if let Some(sc) = out.scopes.iter_mut().find(|s| s.id == scope) {
        sc.package = Some(class.to_string());
    }
    for recv in receivers {
        out.witnesses.push(crate::model::witnesses::Witness {
            attachment: crate::model::witnesses::WitnessAttachment::Variable {
                name: recv.clone(),
                scope,
            },
            source: crate::model::witnesses::WitnessSource::Builder("skeleton-receiver".into()),
            payload: crate::model::witnesses::WitnessPayload::InferredType(
                crate::model::file_analysis::InferredType::ClassName(class.to_string()),
            ),
            span: Span { start: at, end: at },
        });
    }
}

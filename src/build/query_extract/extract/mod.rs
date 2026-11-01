//! The generic extraction driver: capture events, the wrapper-chain
//! peel, and `extract()` — knows no language specifics.
//!
//! `extract()` owns the event order, the scope stack and the routing; what
//! each capture family DOES lives in its own file (`Family`). A family
//! handler sees one event at a time through the shared `ExtractState`.
//! `docs/adr/capture-vocabulary.md`.

use super::*;
use crate::model::file_analysis::{Scope, ScopeId, ScopeKind};

mod commands;
mod defs;
mod flow;
mod imports;
mod links;
mod members;
mod narrowing;
mod refs;
mod scopes;

/// One capture event, flattened from query matches and sorted by
/// position so the driver can run its state machine in source order.
struct Event {
    start_byte: usize,
    end_byte: usize,
    start: Point,
    end: Point,
    cap: Capture,
    text: String,
    /// Query match id — name captures join their parent def through it.
    match_id: usize,
}

impl Event {
    fn at(node: tree_sitter::Node<'_>, cap: Capture, text: String, match_id: usize) -> Event {
        Event {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start: node.start_position(),
            end: node.end_position(),
            cap,
            text,
            match_id,
        }
    }

    fn span(&self) -> Span {
        Span { start: self.start, end: self.end }
    }
}

/// Which family file handles a capture. Exhaustive, so a new `Capture`
/// variant does not compile until it has a home.
#[derive(Clone, Copy)]
enum Family {
    Scopes,
    Defs,
    Refs,
    Members,
    Flow,
    Narrowing,
    Commands,
    Imports,
    /// Read while flattening; a link never becomes an event.
    Links,
}

fn family(cap: Capture) -> Family {
    use Capture as C;
    match cap {
        C::Scope(_) | C::Context(_) | C::Parent => Family::Scopes,
        C::Def { .. }
        | C::DefPair
        | C::Qualifier
        | C::OolDef
        | C::NestedTarget
        | C::RetType
        | C::TypeEnclosing
        | C::SymAttr
        | C::SpecPrimary
        | C::NsInline
        | C::Alias(_)
        | C::MacroAlias(_)
        | C::Tmpl(_)
        | C::Mark(_) => Family::Defs,
        C::Ref(_) | C::RefSpelled => Family::Refs,
        C::Member(_) | C::MemberOp(_) | C::Arity(_) => Family::Members,
        C::Flow(_)
        | C::Expr(_)
        | C::Obs(_)
        | C::Arm
        | C::Domain(_)
        | C::Shape(_)
        | C::TypeAnnot
        | C::AnonAggMember
        | C::Receiver
        | C::NamedReceiver(_) => Family::Flow,
        C::Narrow(_) | C::Move(_) | C::GuardRegion | C::Unevaluated | C::ParamRegion => {
            Family::Narrowing
        }
        C::Cmd(_) => Family::Commands,
        C::Import(_) => Family::Imports,
        C::LinkFrom(_) | C::Link(_) | C::Deref(_) | C::DerefLeaf(_) => Family::Links,
    }
}

/// Everything the family handlers share: the output under construction,
/// the scope stack in force at the current event, and each family's
/// per-match joins.
struct ExtractState<'p> {
    pack: &'p LangPack,
    out: SkeletonAnalysis,
    /// (end_byte, ScopeId) — real Scope rows are minted as we go so the
    /// resulting FileAnalysis carries a genuine lexical tree.
    scope_stack: Vec<(usize, ScopeId)>,
    /// (registered_at_depth, kind, value) — a context set inside a scope
    /// pops with it (Python class blocks); one set at file depth is sticky
    /// (Perl's flat `package Foo;`).
    context_stack: Vec<(usize, ContextKind, String)>,
    /// The innermost scope and sticky context at the current event.
    cur_scope: ScopeId,
    package: Option<String>,
    scopes: scopes::State,
    defs: defs::State,
    refs: refs::State,
    members: members::State,
    flow: flow::State,
    narrowing: narrowing::State,
    commands: commands::State,
    imports: imports::State,
    /// The module the file IS, when its language names it by path and the
    /// driver had one: its dotted name is the section every declaration
    /// without a namespace of its own joins.
    file_module: Option<&'p FileModule>,
}

pub use crate::model::file_analysis::FileModule;

impl ExtractState<'_> {
    /// The section of the namespace lane in force at `at`, as written so
    /// far. Every section that holds `at` is in by then — a section opens
    /// at or before the code it covers, and a statement form is cut short
    /// only where its successor starts.
    fn namespace_at(&self, at: Point) -> Option<&str> {
        crate::model::file_analysis::namespace_range_at(&self.out.namespace_ranges, at)
            .map(|r| r.namespace.as_str())
            .filter(|ns| !ns.is_empty())
    }

    /// The scope a class member declared at the current event belongs to:
    /// the innermost scope that is not a callable's body.
    fn member_scope(&self) -> ScopeId {
        self.scope_stack
            .iter()
            .rev()
            .map(|&(_, id)| id)
            .find(|id| !matches!(self.out.scopes[id.0 as usize].kind, ScopeKind::Sub { .. }))
            .unwrap_or(self.cur_scope)
    }

    /// The identity a class named `leaf`, declared in `from` at `at`, is
    /// filed under. Under a use-map it is the leaf joined to its home
    /// (`scopes::home_of`) — the class it is written in, else the
    /// namespace in force — so every spelling of the class resolves to the
    /// key it is filed under. Otherwise the spelling is the identity.
    /// The scope a def at the current event binds in: the innermost one,
    /// or for a def written in a header (a parameter), the def scope the
    /// header belongs to.
    fn bind_scope(&self) -> ScopeId {
        self.scopes.headed.get(&self.cur_scope).copied().unwrap_or(self.cur_scope)
    }

    /// The scope a def of `kind` at the current event binds in. A data
    /// member belongs to its class body even where it is written inside a
    /// callable (php's constructor-promoted property).
    fn def_scope(&self, kind: DefCap) -> ScopeId {
        match kind {
            DefCap::Field | DefCap::Slot => self.member_scope(),
            DefCap::Sub
            | DefCap::Method
            | DefCap::Anon
            | DefCap::Class
            | DefCap::Package
            | DefCap::Var
            | DefCap::Local
            | DefCap::Constant
            | DefCap::Enumerator
            | DefCap::Label
            | DefCap::Macro
            | DefCap::Reexport
            | DefCap::Union
            | DefCap::UnionField => self.bind_scope(),
        }
    }

    fn class_identity(&self, leaf: &str, from: ScopeId, at: Point) -> String {
        match self.pack.names.use_map_sep() {
            Some(sep) => crate::model::conventions::join_qualified(scopes::home_of(self, from, at).unwrap_or(""), leaf, sep),
            None => leaf.to_string(),
        }
    }
}

/// A written type as the identities it names at `site`: under a use-map
/// every class spelling it carries resolves through the rows that reach
/// the site and the namespace in force there. Runs once every row,
/// section and class is in.
fn written_type_at(out: &SkeletonAnalysis, names: &crate::model::file_analysis::NameSpellings, t: InferredType, site: Point) -> InferredType {
    match crate::model::file_analysis::UseMap::at(&out.use_rows, &out.namespace_ranges, &out.scopes, names, site, &out.symbols) {
        Some(map) => t.map_class_names(&mut |c| map.resolve(c).identity(map.sep)),
        None => t,
    }
}

/// Run the document over `root`. Links and `@deref` marks are recorded
/// first, because a level inside a definition is matched after the
/// definition, so every link must be in before any capture reads them;
/// every other capture comes back with its match id.
#[allow(clippy::type_complexity)]
fn record_matches<'t>(
    compiled: &CompiledPack,
    root: tree_sitter::Node<'t>,
    source: &[u8],
) -> (links::Links<'t>, Vec<(tree_sitter::Node<'t>, Capture, usize)>) {
    let mut links = links::Links::default();
    let mut captured = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&compiled.query, root, source);
    let mut match_counter = 0usize;
    let cap_of = |c: &tree_sitter::QueryCapture| compiled.captures[c.index as usize];
    while let Some(m) = matches.next() {
        if !crate::build::query_predicates::admits(&compiled.query, m, root) {
            continue;
        }
        match_counter += 1;
        let from = m.captures.iter().find_map(|c| match cap_of(c) {
            Some(Capture::LinkFrom(walk)) => Some((walk, c.node)),
            _ => None,
        });
        for c in m.captures {
            match cap_of(c) {
                None | Some(Capture::LinkFrom(_)) => {}
                Some(Capture::Link(rel)) => {
                    if let Some((walk, from)) = from {
                        links.record(walk, from, rel, c.node);
                    }
                }
                Some(Capture::Deref(level)) => links.mark_level(c.node, level),
                Some(Capture::DerefLeaf(def)) => links.mark_leaf(c.node, def),
                Some(cap) => captured.push((c.node, cap, match_counter)),
            }
        }
    }
    (links, captured)
}

/// The `deref` walk over a tree the extractor never saw (a macro body
/// reparsed on its own): the pack's document runs over the tree once, and
/// each declarator is peeled by the same walk extraction uses, so a field
/// reads the same however it was written.
pub struct DeclaratorPeel<'t>(links::Links<'t>);

impl<'t> DeclaratorPeel<'t> {
    pub fn of_tree(tree: &'t Tree, source: &[u8], pack: &LangPack) -> Self {
        match cached_query(&tree.language(), pack.query_source) {
            Ok(compiled) => DeclaratorPeel(record_matches(compiled, tree.root_node(), source).0),
            Err(_) => DeclaratorPeel(links::Links::default()),
        }
    }

    /// The declarator's deref stack; empty when the chain reaches no leaf.
    pub fn stack(&self, declarator: tree_sitter::Node<'t>, source: &[u8]) -> Vec<crate::model::file_analysis::DerefStep> {
        self.0.peel_declarator(declarator, source).map(|p| p.stack).unwrap_or_default()
    }
}

/// `extract_in` for a file that is no module of its own.
#[cfg(test)]
pub fn extract(tree: &Tree, source: &[u8], pack: &LangPack) -> Result<SkeletonAnalysis, String> {
    extract_in(tree, source, pack, None)
}

/// Run the pack's document over a parsed file. `file_module` is the module
/// the file is, for a language that names modules by path; its classes
/// join that namespace.
pub fn extract_in(
    tree: &Tree,
    source: &[u8],
    pack: &LangPack,
    file_module: Option<&FileModule>,
) -> Result<SkeletonAnalysis, String> {
    let language = tree.language();
    let compiled = cached_query(&language, pack.query_source)?;

    let root = tree.root_node();
    let mut out = SkeletonAnalysis::for_pack(pack);
    out.scopes.push(Scope {
        id: ScopeId(0),
        parent: None,
        kind: ScopeKind::File,
        span: Span { start: root.start_position(), end: root.end_position() },
        package: None,
        owner: None,
        implicit_receiver: false,
        direct: false,
        declaration_rules: Vec::new(),
    });
    let mut st = ExtractState {
        pack,
        out,
        scope_stack: vec![(root.end_byte(), ScopeId(0))],
        context_stack: Vec::new(),
        cur_scope: ScopeId(0),
        package: None,
        scopes: Default::default(),
        defs: Default::default(),
        refs: Default::default(),
        members: Default::default(),
        flow: Default::default(),
        narrowing: Default::default(),
        commands: Default::default(),
        imports: Default::default(),
        file_module,
    };
    // A module named by its path is one section, the whole file.
    if let (Some(m), Some(sep)) = (file_module, pack.names.use_map_sep()) {
        let ns = m.segments.iter().fold(String::new(), |ns, seg| crate::model::conventions::join_qualified(&ns, seg, sep));
        st.out.namespace_ranges.push(crate::model::file_analysis::NamespaceRange {
            namespace: ns,
            span: st.out.scopes[0].span,
            kind: crate::model::file_analysis::SectionKind::Statement,
        });
    }

    // ---- flatten matches into ordered events ----
    let (links, captured) = record_matches(compiled, root, source);
    // A capture whose meaning needs its LIVE node (a declarator to peel, an
    // argument list to count) is read here, where the node is in hand;
    // everything else becomes an event carrying its text.
    let mut events: Vec<Event> = Vec::new();
    for (node, cap, match_id) in captured {
        match cap {
            Capture::OolDef => defs::flatten_ool_def(node, &links, source, match_id, &mut events),
            Capture::NestedTarget => {
                defs::flatten_nested(&mut st.defs, node, &links, source, match_id, &mut events)
            }
            Capture::Member(cap @ (MemberCap::Recv | MemberCap::RecvName)) => {
                members::flatten_recv(&mut st.members, node, cap, pack, source, match_id, &mut events)
            }
            Capture::Arity(ArityCap::Args) => members::open_args(&mut st.members, match_id),
            Capture::Arity(ArityCap::Arg) => members::record_arg(&mut st.members, node, match_id),
            Capture::Arity(ArityCap::Sig) => members::count_params(&mut st.members, node),
            Capture::Qualifier => events.push(Event::at(node, cap, links.qualifier_text(node, source), match_id)),
            _ => events.push(Event::at(node, cap, node.utf8_text(source).unwrap_or("").to_string(), match_id)),
        }
    }
    // Source order; outermost first on ties so scopes push before their
    // contents. A `@scope` on the SAME node as a `@def` (a function_definition
    // carries its own body scope) must open AFTER the def is recorded, so the
    // symbol attributes to its ENCLOSING scope, not its own body — hence scope
    // sorts last among identical-span ties. A header holds no def of its own
    // node, and a token can be the whole of one (`-> T`), so it opens first.
    let tie_rank = |e: &Event| match e.cap {
        Capture::Scope(ScopeCap::Header) => 0,
        Capture::Scope(_) => 2,
        _ => 1,
    };
    events.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
            .then(tie_rank(a).cmp(&tie_rank(b)))
    });

    // ---- pre-collect the per-match joins a handler reads before the
    // capture that feeds them fires (a def's name token, its qualifier) ----
    for e in &events {
        match family(e.cap) {
            Family::Scopes => scopes::collect(&mut st, e),
            Family::Defs => defs::collect(&mut st, e),
            Family::Flow => flow::collect(&mut st, e),
            Family::Narrowing => narrowing::collect(&mut st, e),
            Family::Members => members::collect(&mut st, e),
            Family::Imports => imports::collect(&mut st, e),
            Family::Refs => refs::collect(&mut st, e),
            Family::Commands | Family::Links => {}
        }
    }

    // ---- the state machine: scope stack + sticky contexts ----
    for e in &events {
        while st.scope_stack.len() > 1
            && st.scope_stack.last().is_some_and(|&(end, _)| e.start_byte >= end)
        {
            st.scope_stack.pop();
            while st.context_stack.last().is_some_and(|&(d, ..)| d > st.scope_stack.len()) {
                st.context_stack.pop();
            }
        }
        st.cur_scope = st.scope_stack.last().unwrap().1;
        st.package = st.context_stack.last().map(|(.., p)| p.clone());
        match family(e.cap) {
            Family::Scopes => scopes::handle(&mut st, e),
            Family::Defs => defs::handle(&mut st, e),
            Family::Refs => refs::handle(&mut st, e),
            Family::Members => members::handle(&mut st, e),
            Family::Flow => flow::handle(&mut st, e, &events),
            Family::Narrowing => narrowing::handle(&mut st, e),
            Family::Commands => commands::handle(&mut st, e),
            Family::Imports => imports::handle(&mut st, e),
            Family::Links => {}
        }
    }

    // ---- post-passes, in the order their rows and witnesses land ----
    defs::join_template_params(&mut st);
    defs::tag_inline_namespaces(&mut st);
    flow::emit_keyed_shapes(&mut st);
    imports::finish(&mut st);
    imports::bind_reach(&mut st);
    // A parent may be written above the import that names it, so it
    // resolves once every row is in.
    scopes::resolve_parents(&mut st);
    // A return type likewise.
    defs::resolve_returns(&mut st);
    defs::dedup(&mut st);
    commands::finish(&mut st);
    defs::emit_type_aliases(&mut st);
    // Reads the symbol table after dedup and the command defs.
    flow::finish(&mut st, &events);
    st.out.class_scopes = scopes::class_scope_rows(&st);
    // The narrowing cutoff reads the flow edges.
    narrowing::finish(&mut st);
    members::finish(&mut st);
    Ok(st.out)
}

// The canonical template-spelling rule lives in the Model layer
// (`file_analysis.rs`) so the `ParametricType::Instance` peel shares it;
// re-exported here because the pack `shape_name`s are its Build-side home.
pub use crate::model::file_analysis::canonical_template_spelling;

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

fn byte_range_of(events: &[Event], match_id: usize, cap: Capture) -> Option<(usize, usize)> {
    events
        .iter()
        .find(|e| e.match_id == match_id && e.cap == cap)
        .map(|e| (e.start_byte, e.end_byte))
}

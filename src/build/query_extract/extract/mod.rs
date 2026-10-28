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
    /// What flattening read off the live node that the text cannot carry.
    payload: Option<Box<Payload>>,
}

enum Payload {
    /// A `@nested.target`'s declarator chain, peeled: its deref stack,
    /// on the def it mints.
    Deref(Vec<crate::model::file_analysis::DerefStep>),
    /// Whether a `@member.recv` was a simple variable before its wrappers
    /// were peeled.
    SimpleRecv(bool),
    /// An `@arity.sig` parameter list's arity.
    Params(crate::model::file_analysis::ParamArity),
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
            payload: None,
        }
    }

    fn with(mut self, payload: Payload) -> Event {
        self.payload = Some(Box::new(payload));
        self
    }

    fn span(&self) -> Span {
        Span { start: self.start, end: self.end }
    }

    fn range(&self) -> (usize, usize) {
        (self.start_byte, self.end_byte)
    }
}

/// The capture events adorned onto the tree. Sorted outermost-first, the
/// events are a preorder walk of the tagged nodes, so the events a node
/// contains are one contiguous run after it; indexed by match, every
/// capture of one match is a lookup. A handler asks "the `@x` of my
/// match" or "what else is tagged on this node" instead of keeping a
/// join table of its own.
struct Adorned {
    events: Vec<Event>,
    /// Event indices grouped by match id (`match_start[m]..match_start[m+1]`
    /// in `by_match`), each group in event order.
    by_match: Vec<u32>,
    match_start: Vec<u32>,
}

impl Adorned {
    fn new(events: Vec<Event>, match_count: usize) -> Self {
        let mut match_start = vec![0u32; match_count + 2];
        for e in &events {
            match_start[e.match_id + 1] += 1;
        }
        for m in 1..match_start.len() {
            match_start[m] += match_start[m - 1];
        }
        let mut fill = match_start.clone();
        let mut by_match = vec![0u32; events.len()];
        for (ix, e) in events.iter().enumerate() {
            by_match[fill[e.match_id] as usize] = ix as u32;
            fill[e.match_id] += 1;
        }
        Adorned { events, by_match, match_start }
    }

    /// The indices of every capture of match `mid`, in event order.
    fn of_match_ixs(&self, mid: usize) -> impl Iterator<Item = usize> + '_ {
        let (lo, hi) = (self.match_start[mid] as usize, self.match_start[mid + 1] as usize);
        self.by_match[lo..hi].iter().map(|&ix| ix as usize)
    }

    /// Every capture of match `mid`, in event order.
    fn of_match(&self, mid: usize) -> impl Iterator<Item = &Event> + '_ {
        self.of_match_ixs(mid).map(|ix| &self.events[ix])
    }

    /// Match `mid`'s `cap` capture: the first, where a quantified capture
    /// repeats.
    fn in_match(&self, mid: usize, cap: Capture) -> Option<&Event> {
        self.in_match_ix(mid, cap).map(|ix| &self.events[ix])
    }

    fn in_match_ix(&self, mid: usize, cap: Capture) -> Option<usize> {
        self.of_match_ixs(mid).find(|&ix| self.events[ix].cap == cap)
    }

    /// Every match's captures, matches in id order.
    fn by_match(&self) -> impl Iterator<Item = &Event> + '_ {
        self.by_match.iter().map(|&ix| &self.events[ix as usize])
    }

    /// The events tagged on exactly `range`.
    fn on_range(&self, range: (usize, usize)) -> impl Iterator<Item = &Event> + '_ {
        self.on_range_ixs(range).map(|ix| &self.events[ix])
    }

    fn on_range_ixs(&self, range: (usize, usize)) -> impl Iterator<Item = usize> + '_ {
        let lo = self.events.partition_point(|e| (e.start_byte, std::cmp::Reverse(e.end_byte)) < (range.0, std::cmp::Reverse(range.1)));
        (lo..self.events.len()).take_while(move |&ix| self.events[ix].range() == range)
    }

    /// The indices of the events inside `range` (and on it): the subtree
    /// of the node there, in preorder.
    fn within(&self, range: (usize, usize)) -> impl Iterator<Item = usize> + '_ {
        let lo = self.events.partition_point(|e| e.start_byte < range.0);
        let hi = self.events.partition_point(|e| e.start_byte < range.1);
        (lo..hi).filter(move |&ix| self.events[ix].end_byte <= range.1)
    }
}

/// What the walk minted on an event's node, for a later capture — of any
/// match — that names the same node to ask, instead of re-deriving it.
#[derive(Default)]
struct Minted {
    /// A value witness on its span: a flow source narrows onto it.
    value: bool,
    /// The name token of a def that declares it: a ref inside is the
    /// declaration, not a use.
    declares: bool,
    /// The identity a class def filed itself under.
    class: Option<Box<str>>,
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
        | C::SymAttr
        | C::SpecPrimary
        | C::NsInline
        | C::Alias(_)
        | C::MacroAlias(_)
        | C::Tmpl(_)
        | C::ParamReceiver
        | C::Mark(_) => Family::Defs,
        C::Ref(_) | C::RefSpelled => Family::Refs,
        C::Member(_) | C::MemberOp(_) | C::Arity(_) => Family::Members,
        C::Flow(_)
        | C::Expr(_)
        | C::Obs(_)
        | C::Domain(_)
        | C::Shape(_)
        | C::TypeAnnot
        | C::AnonAggMember => Family::Flow,
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
    adorned: &'p Adorned,
    out: SkeletonAnalysis,
    /// (end_byte, ScopeId) — real Scope rows are minted as we go so the
    /// resulting FileAnalysis carries a genuine lexical tree.
    scope_stack: Vec<(usize, ScopeId)>,
    /// (registered_at_depth, kind, value) — a context set inside a scope
    /// pops with it (Python class blocks); one set at file depth is sticky
    /// (Perl's flat `package Foo;`).
    context_stack: Vec<(usize, ContextKind, String)>,
    /// The events whose node holds the current event's, outermost first.
    ancestors: Vec<usize>,
    /// Per event, what the walk minted on it so far.
    minted: Vec<Minted>,
    /// The innermost scope and sticky context at the current event.
    cur_scope: ScopeId,
    package: Option<String>,
    scopes: scopes::State,
    flow: flow::State,
    narrowing: narrowing::State,
    commands: commands::State,
    /// The module the file IS, when its language names it by path and the
    /// driver had one: its dotted name is the section every declaration
    /// without a namespace of its own joins.
    file_module: Option<&'p FileModule>,
}

pub use crate::model::file_analysis::FileModule;

impl ExtractState<'_> {
    /// The current event `e`'s enclosing events and the others tagged on
    /// its own node: the ones a fact "around this token" is stated by.
    fn around<'a>(&'a self, e: &Event) -> impl Iterator<Item = usize> + 'a {
        self.ancestors.iter().copied().chain(self.adorned.on_range_ixs(e.range()))
    }

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
    /// The class a def minted on the node `range` names, as its def filed
    /// it.
    fn class_on(&self, range: (usize, usize)) -> Option<&str> {
        self.adorned.on_range_ixs(range).find_map(|ix| self.minted[ix].class.as_deref())
    }

    fn bind_scope(&self) -> ScopeId {
        self.scopes.headed.get(&self.cur_scope).copied().unwrap_or(self.cur_scope)
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
    let mut out = SkeletonAnalysis::default();
    out.names = pack.names.clone();
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

    // ---- flatten matches into ordered events ----
    let (links, captured) = record_matches(compiled, root, source);
    // A capture whose meaning needs its LIVE node (a declarator to peel, an
    // argument list to count) is read here, where the node is in hand;
    // everything else becomes an event carrying its text.
    let mut events: Vec<Event> = Vec::new();
    let match_count = captured.last().map_or(0, |&(_, _, mid)| mid);
    for (node, cap, match_id) in captured {
        match cap {
            Capture::OolDef => defs::flatten_ool_def(node, &links, source, match_id, &mut events),
            Capture::NestedTarget => defs::flatten_nested(node, &links, source, match_id, &mut events),
            Capture::Member(MemberCap::Recv) => events.push(members::flatten_recv(node, pack, source, match_id)),
            // A wildcard in a query also matches the grammar's extras, and a
            // predicate filters a whole match rather than one quantified
            // capture, so a comment between two arguments is dropped here.
            Capture::Arity(ArityCap::Arg) if node.is_extra() => {}
            Capture::Arity(ArityCap::Sig) => {
                events.push(Event::at(node, cap, String::new(), match_id).with(Payload::Params(members::count_params(node))))
            }
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
    let adorned = Adorned::new(events, match_count);

    let mut st = ExtractState {
        pack,
        adorned: &adorned,
        out,
        scope_stack: vec![(root.end_byte(), ScopeId(0))],
        context_stack: Vec::new(),
        ancestors: Vec::new(),
        minted: std::iter::repeat_with(Minted::default).take(adorned.events.len()).collect(),
        cur_scope: ScopeId(0),
        package: None,
        scopes: Default::default(),
        flow: Default::default(),
        narrowing: Default::default(),
        commands: Default::default(),
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

    // ---- one walk in document order: scope stack, sticky contexts and
    // ancestors; everything else a handler needs, it looks up ----
    let events = &adorned.events;
    for (ix, e) in events.iter().enumerate() {
        while st.scope_stack.len() > 1
            && st.scope_stack.last().is_some_and(|&(end, _)| e.start_byte >= end)
        {
            st.scope_stack.pop();
            while st.context_stack.last().is_some_and(|&(d, ..)| d > st.scope_stack.len()) {
                st.context_stack.pop();
            }
        }
        while st.ancestors.last().is_some_and(|&a| {
            let a = &events[a];
            a.start_byte > e.start_byte || a.end_byte < e.end_byte
        }) {
            st.ancestors.pop();
        }
        st.cur_scope = st.scope_stack.last().unwrap().1;
        st.package = st.context_stack.last().map(|(.., p)| p.clone());
        match family(e.cap) {
            Family::Scopes => scopes::handle(&mut st, e),
            Family::Defs => defs::handle(&mut st, ix, e),
            Family::Refs => refs::handle(&mut st, e),
            // Read through the match (`@member.recv`, `@member.op`) or the
            // node (`@member.op.<which>`, `@arity.spread`) they tag.
            Family::Members => {}
            Family::Flow => flow::handle(&mut st, ix, e),
            Family::Narrowing => narrowing::handle(&mut st, e),
            Family::Commands => commands::handle(&mut st, e),
            Family::Imports => imports::handle(&mut st, e),
            Family::Links => {}
        }
        st.ancestors.push(ix);
    }

    // ---- post-passes, in the order their rows and witnesses land ----
    defs::join_template_params(&mut st);
    flow::emit_keyed_shapes(&mut st);
    imports::finish(&mut st);
    imports::bind_reach(&mut st);
    // A parent may be written above the import that names it, so it
    // resolves once every row is in.
    scopes::resolve_parents(&mut st);
    defs::dedup(&mut st);
    defs::emit_receiver_types(&mut st);
    commands::finish(&mut st);
    defs::emit_type_aliases(&mut st);
    // Reads the symbol table after dedup and the command defs.
    flow::finish(&mut st);
    st.out.class_scopes = scopes::class_scope_rows(&st);
    // The narrowing cutoff reads the flow edges.
    narrowing::finish(&mut st);
    members::finish(&mut st);
    dump_for_equivalence(source, pack, file_module, &st.out);
    Ok(st.out)
}

/// `PERL_LSP_SKEL_DUMP_DIR=<dir>`: write every extraction's
/// `SkeletonAnalysis` to `<dir>/<hash of input>`, so two builds of the
/// extractor diff over every source any test feeds it.
fn dump_for_equivalence(source: &[u8], pack: &LangPack, file_module: Option<&FileModule>, out: &SkeletonAnalysis) {
    use std::hash::{Hash, Hasher};
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(dir) = DIR.get_or_init(|| std::env::var_os("PERL_LSP_SKEL_DUMP_DIR").map(Into::into)) else {
        return;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (source, pack.query_source, file_module.map(|m| (&m.segments, m.is_package))).hash(&mut h);
    let _ = std::fs::write(dir.join(format!("{:016x}", h.finish())), format!("{out:#?}"));
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

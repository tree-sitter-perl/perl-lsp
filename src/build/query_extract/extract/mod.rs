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
mod roots;
mod scopes;

/// One capture, as a fact on the node it tags: the node's site, its span,
/// and what the match said there.
struct Event {
    /// The head of the tagged node's same-range chain: every capture on a
    /// range is on one site.
    site: crate::cst::NodeId,
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
    /// A `@member.recv`'s invocant: a written name, or an expression.
    Recv(crate::model::conventions::Invocant),
}

/// A capture before it is placed: the live node it tags, and the event.
type Flattened<'t> = (tree_sitter::Node<'t>, Event);

impl Event {
    fn at<'t>(node: tree_sitter::Node<'t>, cap: Capture, text: String, match_id: usize) -> Flattened<'t> {
        let event = Event {
            site: crate::cst::NodeId::default(),
            start: node.start_position(),
            end: node.end_position(),
            cap,
            text,
            match_id,
            payload: None,
        };
        (node, event)
    }

    fn span(&self) -> Span {
        Span { start: self.start, end: self.end }
    }
}

/// Attach `payload` to a flattened capture.
fn with(f: Flattened<'_>, payload: Payload) -> Flattened<'_> {
    let (node, mut e) = f;
    e.payload = Some(Box::new(payload));
    (node, e)
}

/// The captures as facts on the syntax tree. Each sits on its node's
/// site, so the captures of a node are its facts, those inside it are its
/// subtree's id interval, and those holding it are up its `parent` chain.
/// The one list beside the tree is match → captures: a match spans nodes.
/// A handler asks "the `@x` of my match" or "what else is tagged on this
/// node" instead of keeping a join table of its own.
struct Adorned<'a, 't> {
    tree: &'a crate::cst::Structure<'t>,
    tags: crate::cst::NodeFacts<Event>,
    /// Tag positions grouped by match id (`match_start[m]..match_start[m+1]`
    /// in `by_match`), each group in walk order.
    by_match: Vec<u32>,
    match_start: Vec<u32>,
}

impl<'a, 't> Adorned<'a, 't> {
    fn new(tree: &'a crate::cst::Structure<'t>, tags: crate::cst::NodeFacts<Event>, match_count: usize) -> Self {
        let events = tags.all();
        let mut match_start = vec![0u32; match_count + 2];
        for e in events {
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
        Adorned { tree, tags, by_match, match_start }
    }

    /// Every capture, in walk order: sites in preorder.
    fn events(&self) -> &[Event] {
        self.tags.all()
    }

    /// The indices of every capture of match `mid`, in walk order.
    fn of_match_ixs(&self, mid: usize) -> impl Iterator<Item = usize> + '_ {
        let (lo, hi) = (self.match_start[mid] as usize, self.match_start[mid + 1] as usize);
        self.by_match[lo..hi].iter().map(|&ix| ix as usize)
    }

    /// Every capture of match `mid`, in walk order.
    fn of_match(&self, mid: usize) -> impl Iterator<Item = &Event> + '_ {
        self.of_match_ixs(mid).map(|ix| &self.events()[ix])
    }

    /// Match `mid`'s `cap` capture: the first, where a quantified capture
    /// repeats.
    fn in_match(&self, mid: usize, cap: Capture) -> Option<&Event> {
        self.in_match_ix(mid, cap).map(|ix| &self.events()[ix])
    }

    fn in_match_ix(&self, mid: usize, cap: Capture) -> Option<usize> {
        self.of_match_ixs(mid).find(|&ix| self.events()[ix].cap == cap)
    }

    /// Every match's captures, matches in id order.
    fn by_match(&self) -> impl Iterator<Item = &Event> + '_ {
        self.by_match.iter().map(|&ix| &self.events()[ix as usize])
    }

    /// The captures on `e`'s node (and every node sharing its range).
    fn on(&self, e: &Event) -> impl Iterator<Item = &Event> + '_ {
        self.on_ixs(e).map(|ix| &self.events()[ix])
    }

    fn on_ixs(&self, e: &Event) -> std::ops::Range<usize> {
        self.tags.on(e.site)
    }

    /// The indices of the captures holding `e`'s node: on it, and on each
    /// node above it.
    fn around(&self, e: &Event) -> impl Iterator<Item = usize> + '_ {
        let up = self.tree.node(e.site).enclosing_sites();
        self.on_ixs(e).chain(up.flat_map(|n| self.tags.on(n.id())))
    }

    /// The captures in `list`'s subtree that are its own: a list of the
    /// same tag nested in it (a call in an argument, a callback's
    /// parameters) holds its own items, so its subtree is skipped. An item
    /// whose own match states its list belongs to that list wherever it
    /// sits: a construction tagged a list on its own node (php's `new A(new
    /// B)`) is an item of the list around it, not of itself.
    fn list_items<'e>(&'e self, list: &'e Event) -> impl Iterator<Item = &'e Event> + 'e {
        let mut nested_end = 0;
        self.within(list).filter_map(move |ix| {
            let t = &self.events()[ix];
            if t.cap != list.cap {
                if let Some(own) = self.in_match(t.match_id, list.cap) {
                    return (own.site == list.site).then_some(t);
                }
            }
            if ix < nested_end {
                return None;
            }
            if t.cap == list.cap && t.site != list.site {
                nested_end = self.within(t).end;
                return None;
            }
            Some(t)
        })
    }

    /// The indices of the captures on `e`'s node and inside it, in
    /// preorder.
    fn within(&self, e: &Event) -> std::ops::Range<usize> {
        self.tags.positions(self.tree.node(e.site).subtree())
    }

    fn start_byte(&self, e: &Event) -> usize {
        self.tree.node(e.site).start_byte()
    }

    fn end_byte(&self, e: &Event) -> usize {
        self.tree.node(e.site).end_byte()
    }

    /// The width of `e`'s node, in bytes: of two nodes holding a third,
    /// the narrower is the inner.
    fn width(&self, e: &Event) -> usize {
        self.end_byte(e) - self.start_byte(e)
    }
}

/// What the walk minted on an event's node, for a later capture — of any
/// match — that names the same node to ask, instead of re-deriving it.
#[derive(Default)]
struct Minted {
    /// The name token of a def that declares it: a ref inside is the
    /// declaration, not a use.
    declares: bool,
    /// The identity a class def filed itself under.
    class: Option<Box<str>>,
    /// A member read: what a place written here projects, so a write
    /// lands where the read looks.
    projects: Option<Box<MemberRead>>,
    /// On a def node: the `symbols` row it pushed, whose written return
    /// resolves once every row is in (`defs::resolve_returns`).
    return_row: Option<usize>,
}

/// A member read (`@expr.member`): the receiver it reads through, the
/// member's name, and the token that names it.
struct MemberRead {
    receiver: Span,
    member: String,
    token: Span,
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
    Roots,
    /// Read while flattening; a link never becomes an event.
    Links,
}

fn family(cap: Capture) -> Family {
    use Capture as C;
    match cap {
        C::Scope(_) | C::ClosureCapture | C::Context(_) | C::Parent | C::ParentSuper => Family::Scopes,
        C::Def { .. }
        | C::DefPair
        | C::DefHoist
        | C::Qualifier
        | C::OolDef
        | C::NestedTarget
        | C::RetType
        | C::TypeRelative(_)
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
        C::Root(_) => Family::Roots,
        C::Import(_) => Family::Imports,
        C::LinkFrom(_) | C::Link(_) | C::Deref(_) | C::DerefLeaf(_) => Family::Links,
    }
}

/// Everything the family handlers share: the output under construction,
/// the scope stack in force at the current event, and each family's
/// per-match joins.
struct ExtractState<'p, 't> {
    pack: &'p LangPack,
    adorned: &'p Adorned<'p, 't>,
    out: SkeletonAnalysis,
    /// (end_byte, ScopeId) — real Scope rows are minted as we go so the
    /// resulting FileAnalysis carries a genuine lexical tree.
    scope_stack: Vec<(usize, ScopeId)>,
    /// (registered_at_depth, kind, value) — a context set inside a scope
    /// pops with it (Python class blocks); one set at file depth is sticky
    /// (Perl's flat `package Foo;`).
    context_stack: Vec<(usize, ContextKind, String)>,
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

impl ExtractState<'_, '_> {
    /// The current event `e`'s enclosing events and the others tagged on
    /// its own node: the ones a fact "around this token" is stated by.
    fn around<'a>(&'a self, e: &Event) -> impl Iterator<Item = usize> + 'a {
        self.adorned.around(e)
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
    /// The class a def minted on `e`'s node names, as its def filed it.
    fn class_on(&self, e: &Event) -> Option<&str> {
        self.adorned.on_ixs(e).find_map(|ix| self.minted[ix].class.as_deref())
    }

    fn bind_scope(&self) -> ScopeId {
        self.scopes.headed.get(&self.cur_scope).copied().unwrap_or(self.cur_scope)
    }

    /// The scope a def of `kind`, declared in match `match_id` at the
    /// current event, binds in. A data member belongs to its class body even
    /// where it is written inside a callable (php's constructor-promoted
    /// property); a hoisted def (`@def.hoist`) to the scope around the one
    /// it is written in.
    fn def_scope(&self, kind: DefCap, match_id: usize) -> ScopeId {
        match kind {
            DefCap::Field | DefCap::Slot => self.member_scope(),
            DefCap::Sub
            | DefCap::Method
            | DefCap::Anon
            | DefCap::Class
            | DefCap::Package
            | DefCap::Var
            | DefCap::Local
            | DefCap::Assigned
            | DefCap::Constant
            | DefCap::Enumerator
            | DefCap::Label
            | DefCap::Macro
            | DefCap::Reexport
            | DefCap::Union
            | DefCap::UnionField => match self.adorned.in_match(match_id, Capture::DefHoist).is_some() {
                true => crate::model::file_analysis::scope_chain_of(&self.out.scopes, self.cur_scope)
                    .get(1)
                    .copied()
                    .unwrap_or(self.cur_scope),
                false => self.bind_scope(),
            },
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
    syntax: &crate::cst::SyntaxForest<'t>,
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
        if !crate::build::query_predicates::admits(&compiled.query, m, || syntax) {
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
                // A wildcard also matches the grammar's extras (a comment),
                // which are never code: no capture lands on one.
                Some(_) if c.node.is_extra() => {}
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
            Ok(compiled) => {
                let syntax = crate::cst::SyntaxForest::new([tree.root_node()]);
                DeclaratorPeel(record_matches(compiled, &syntax, tree.root_node(), source).0)
            }
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
        closed: false,
        captures: Vec::new(),
        direct: false,
        declaration_rules: Vec::new(),
    });

    // ---- flatten matches into captures on the tree ----
    // The extractor reads the tree for every capture, so it is built
    // up front.
    let syntax = crate::cst::SyntaxForest::new([root]);
    let (links, captured) = record_matches(compiled, &syntax, root, source);
    // A capture whose meaning needs its LIVE node (a declarator to peel, an
    // argument list to count) is read here, where the node is in hand;
    // everything else becomes an event carrying its text.
    let mut flat: Vec<Flattened<'_>> = Vec::new();
    let match_count = captured.last().map_or(0, |&(_, _, mid)| mid);
    for (node, cap, match_id) in captured {
        match cap {
            Capture::OolDef => defs::flatten_ool_def(node, &links, source, match_id, &mut flat),
            Capture::NestedTarget => defs::flatten_nested(node, &links, source, match_id, &mut flat),
            Capture::Member(cap @ (MemberCap::Recv | MemberCap::RecvName)) => {
                flat.push(members::flatten_recv(node, cap, source, match_id))
            }
            Capture::Qualifier => flat.push(Event::at(node, cap, links.qualifier_text(node, source), match_id)),
            _ => flat.push(Event::at(node, cap, node.utf8_text(source).unwrap_or("").to_string(), match_id)),
        }
    }
    // Each capture sits on its node's site, sites in preorder. Within a
    // site a `@scope` on the SAME node as a `@def` (a function_definition
    // carries its own body scope) must open AFTER the def is recorded, so
    // the symbol attributes to its ENCLOSING scope, not its own body —
    // hence scope ranks last. A header holds no def of its own node, and a
    // token can be the whole of one (`-> T`), so it opens first. Otherwise
    // captures keep their match order.
    let tie_rank = |e: &Event| match e.cap {
        Capture::Scope(ScopeCap::Header) => 0,
        Capture::Scope(_) => 2,
        _ => 1,
    };
    let tree = syntax.main();
    let mut placed: Vec<(crate::cst::NodeId, Event)> = flat
        .into_iter()
        .map(|(node, mut e)| {
            e.site = tree.lift(node).expect("a capture is a node of the parse it matched").site().id();
            (e.site, e)
        })
        .collect();
    placed.sort_by_key(|(site, e)| (site.index(), tie_rank(e)));
    let adorned = Adorned::new(tree, crate::cst::NodeFacts::new(tree.len(), placed), match_count);

    let mut st = ExtractState {
        pack,
        adorned: &adorned,
        out,
        scope_stack: vec![(root.end_byte(), ScopeId(0))],
        context_stack: Vec::new(),
        minted: std::iter::repeat_with(Minted::default).take(adorned.events().len()).collect(),
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

    // ---- one walk in document order: scope stack and sticky contexts;
    // everything else a handler needs, it looks up ----
    for (ix, e) in adorned.events().iter().enumerate() {
        let at = adorned.start_byte(e);
        while st.scope_stack.len() > 1 && st.scope_stack.last().is_some_and(|&(end, _)| at >= end) {
            st.scope_stack.pop();
            while st.context_stack.last().is_some_and(|&(d, ..)| d > st.scope_stack.len()) {
                st.context_stack.pop();
            }
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
            Family::Roots => roots::handle(&mut st, e),
            Family::Links => {}
        }
    }

    // ---- post-passes, in the order their rows and witnesses land ----
    defs::join_template_params(&mut st);
    flow::emit_keyed_shapes(&mut st);
    imports::finish(&mut st);
    imports::bind_reach(&mut st);
    // A parent may be written above the import that names it, so it
    // resolves once every row is in.
    scopes::resolve_parents(&mut st);
    // A return type likewise.
    defs::resolve_returns(&mut st);
    defs::dedup(&mut st);
    // Reads the deduped defs; the flow lane reads the writes it finds.
    defs::unify_assigned(&mut st);
    commands::finish(&mut st);
    defs::emit_type_aliases(&mut st);
    // Reads the symbol table after dedup and the command defs.
    flow::finish(&mut st);
    st.out.class_scopes = scopes::class_scope_rows(&st);
    // The narrowing cutoff reads the flow edges.
    narrowing::finish(&mut st);
    members::finish(&mut st);
    dump_for_equivalence(source, &language, file_module, &st.out);
    Ok(st.out)
}

/// `PERL_LSP_SKEL_DUMP_DIR=<dir>`: write every extraction's
/// `SkeletonAnalysis` to `<dir>/<hash of input>`, so two builds of the
/// extractor diff over every source any test feeds it.
fn dump_for_equivalence(source: &[u8], language: &tree_sitter::Language, file_module: Option<&FileModule>, out: &SkeletonAnalysis) {
    use std::hash::{Hash, Hasher};
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(dir) = DIR.get_or_init(|| std::env::var_os("PERL_LSP_SKEL_DUMP_DIR").map(Into::into)) else {
        return;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (source, language.name(), file_module.map(|m| (&m.segments, m.is_package))).hash(&mut h);
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

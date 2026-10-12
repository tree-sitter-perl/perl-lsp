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
}

fn family(cap: Capture) -> Family {
    use Capture as C;
    match cap {
        C::Scope(_) | C::Context(_) | C::Parent => Family::Scopes,
        C::Def { .. }
        | C::Qualifier
        | C::OolDef
        | C::NestedTarget
        | C::RetType
        | C::SymAttr
        | C::SpecPrimary
        | C::NsInline
        | C::Alias(_)
        | C::MacroAlias(_)
        | C::Tmpl(_) => Family::Defs,
        C::Ref(_) => Family::Refs,
        C::Member(_) | C::Arity(_) => Family::Members,
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
    /// (registered_at_depth, value) — a context set inside a scope pops
    /// with it (Python class blocks); one set at file depth is sticky
    /// (Perl's flat `package Foo;`).
    context_stack: Vec<(usize, String)>,
    /// The innermost scope and sticky context at the current event.
    cur_scope: ScopeId,
    package: Option<String>,
    scopes: scopes::State,
    defs: defs::State,
    members: members::State,
    flow: flow::State,
    narrowing: narrowing::State,
    commands: commands::State,
    imports: imports::State,
}

/// Flatten a wrapper chain — the recursion tree-sitter's fixed-depth queries
/// cannot express — to its leaf, recording a `DerefStep` per level when
/// `spec.record_stack`. A non-empty `leaf_to_def` REQUIRES the leaf to match
/// (returns its def kind); an empty one accepts ANY leaf and mints nothing
/// (the receiver peel — the leaf is an invocant). Outermost level first
/// (left-to-right display order, `Box*&` → `[Pointer, Reference]`). Depth-
/// capped. The ONE peel: `nested_peel` and `recv_peel` are both this.
pub(crate) fn peel<'a>(
    mut node: tree_sitter::Node<'a>,
    spec: &PeelSpec,
    src: &[u8],
) -> Option<(tree_sitter::Node<'a>, Vec<crate::model::file_analysis::DerefStep>, Option<DefKind>)> {
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
        } else if let Some((_, def_kind)) = is_leaf(node.kind()) {
            // `identifier`→`Local` (param/local), `field_identifier`→
            // `Field` (a class member), so a pointer field outlines as a member.
            return Some((node, stack, Some(*def_kind)));
        } else {
            return None;
        }
    }
    None
}

pub fn extract(tree: &Tree, source: &[u8], pack: &LangPack) -> Result<SkeletonAnalysis, String> {
    let language = tree.language();
    let compiled = cached_query(&language, pack.query_source)?;

    let root = tree.root_node();
    let mut out = SkeletonAnalysis::default();
    out.receiver_names = pack.receiver_names.iter().map(|s| s.to_string()).collect();
    out.names = pack.names.clone();
    out.scopes.push(Scope {
        id: ScopeId(0),
        parent: None,
        kind: ScopeKind::File,
        span: Span { start: root.start_position(), end: root.end_position() },
        package: None,
        owner: None,
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
        members: Default::default(),
        flow: Default::default(),
        narrowing: Default::default(),
        commands: Default::default(),
        imports: Default::default(),
    };

    // ---- flatten matches into ordered events ----
    // A capture whose meaning needs its LIVE node (a declarator to peel, an
    // argument list to count) is read here, where the node is in hand;
    // everything else becomes an event carrying its text.
    let mut events: Vec<Event> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&compiled.query, root, source);
    let mut match_counter = 0usize;
    while let Some(m) = matches.next() {
        match_counter += 1;
        for c in m.captures {
            let Some(cap) = compiled.captures[c.index as usize] else { continue };
            let node = c.node;
            match cap {
                Capture::OolDef => defs::flatten_ool_def(node, pack, source, match_counter, &mut events),
                Capture::NestedTarget => {
                    defs::flatten_nested(&mut st.defs, node, pack, source, match_counter, &mut events)
                }
                Capture::Member(MemberCap::Recv) => {
                    members::flatten_recv(&mut st.members, node, pack, source, match_counter, &mut events)
                }
                Capture::Arity(ArityCap::Args) => members::count_args(&mut st.members, node),
                Capture::Arity(ArityCap::Sig) => members::count_params(&mut st.members, node),
                Capture::Qualifier => {
                    events.push(Event::at(node, cap, defs::qualifier_text(node, pack, source), match_counter))
                }
                _ => events.push(Event::at(
                    node,
                    cap,
                    node.utf8_text(source).unwrap_or("").to_string(),
                    match_counter,
                )),
            }
        }
    }
    // Source order; outermost first on ties so scopes push before their
    // contents. A `@scope` on the SAME node as a `@def` (a function_definition
    // carries its own body scope) must open AFTER the def is recorded, so the
    // symbol attributes to its ENCLOSING scope, not its own body — hence scope
    // sorts last among identical-span ties.
    let is_scope = |e: &Event| matches!(e.cap, Capture::Scope(_));
    events.sort_by(|a, b| {
        a.start_byte
            .cmp(&b.start_byte)
            .then(b.end_byte.cmp(&a.end_byte))
            .then(is_scope(a).cmp(&is_scope(b)))
    });

    // ---- pre-collect the per-match joins a handler reads before the
    // capture that feeds them fires (a def's name token, its qualifier) ----
    for e in &events {
        match family(e.cap) {
            Family::Scopes => scopes::collect(&mut st, e),
            Family::Defs => defs::collect(&mut st, e),
            Family::Flow => flow::collect(&mut st, e),
            Family::Narrowing => narrowing::collect(&mut st, e),
            Family::Refs | Family::Members | Family::Commands | Family::Imports => {}
        }
    }

    // ---- the state machine: scope stack + sticky contexts ----
    for e in &events {
        while st.scope_stack.len() > 1
            && st.scope_stack.last().is_some_and(|&(end, _)| e.start_byte >= end)
        {
            st.scope_stack.pop();
            while st.context_stack.last().is_some_and(|&(d, _)| d > st.scope_stack.len()) {
                st.context_stack.pop();
            }
        }
        st.cur_scope = st.scope_stack.last().unwrap().1;
        st.package = st.context_stack.last().map(|(_, p)| p.clone());
        match family(e.cap) {
            Family::Scopes => scopes::handle(&mut st, e),
            Family::Defs => defs::handle(&mut st, e),
            Family::Refs => refs::handle(&mut st, e),
            Family::Members => members::handle(&mut st, e),
            Family::Flow => flow::handle(&mut st, e, &events),
            Family::Narrowing => narrowing::handle(&mut st, e),
            Family::Commands => commands::handle(&mut st, e),
            Family::Imports => imports::handle(&mut st, e),
        }
    }

    // ---- post-passes, in the order their rows and witnesses land ----
    defs::join_template_params(&mut st);
    defs::tag_inline_namespaces(&mut st);
    flow::emit_keyed_shapes(&mut st);
    imports::finish(&mut st);
    defs::dedup(&mut st);
    commands::finish(&mut st);
    defs::emit_type_aliases(&mut st);
    // Reads the symbol table after dedup and the command defs.
    flow::finish(&mut st, &events);
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

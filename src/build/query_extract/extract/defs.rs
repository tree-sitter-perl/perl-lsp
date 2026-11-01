//! `@def.*` and the captures that decorate a def (its qualifier, return
//! type, attributes, template params), plus type aliases: the symbol rows.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// match_id → the pointer/reference declarator stack a `@nested.target`
    /// capture unravelled to. Read by the `def.*` handler to stamp the symbol.
    nested_stacks: HashMap<usize, Vec<crate::model::file_analysis::DerefStep>>,
    /// A def's name token, joined to its def event by match.
    names_by_match: HashMap<(usize, DefCap), (String, Point, Point)>,
    /// The kinds of def a match's name token declares, by where it starts
    /// (php's promoted parameter is a property and a local on one token).
    kinds_by_name: HashMap<(usize, Point), Vec<DefCap>>,
    /// `@def.<kind>.spelled`: the declaration as written around its name
    /// token, joined to the def by match.
    spelled_by_match: HashMap<(usize, DefCap), String>,
    /// Matches whose defs declare in the scope around them (`@def.hoist`).
    hoisted_matches: std::collections::HashSet<usize>,
    /// Matches whose def is named from its anchor (`@def.<kind>.anchor`).
    anchored_matches: std::collections::HashSet<usize>,
    /// `@def.<kind>.receiver`: the receiver a member def is written
    /// through, joined to the def by match.
    receivers_by_match: HashMap<(usize, DefCap), String>,
    /// `@qualifier` (a `Class::` on an out-of-line def) and `@rettype` (a
    /// method's declared return type) — pre-collected like names because the
    /// `@def` event fires before these inner captures.
    qualifier_by_match: HashMap<usize, String>,
    rettype_by_match: HashMap<usize, (String, Point, Point)>,
    /// `@type.<relative>`: the written types that name a class by where
    /// they are written. Joined to a `@rettype` or `@type.annot` by span,
    /// because the stating pattern is not the writing site's match.
    relative_types: HashMap<(Point, Point), RelativeClass>,
    /// Each declared return, resolved once every row and parent edge is in.
    written_returns: Vec<(usize, WrittenType)>,
    /// Flags the document states about the def whose name token sits at a
    /// span (`@mark.<flag>`). Joined by
    /// span because the stating pattern is not the def's match.
    marks: HashMap<(Point, Point), crate::model::file_analysis::SymbolFlags>,
    /// `@sym.attr` — a token whose TEXT rides onto the match's def symbol as
    /// an attribute (cpp: a declaration's storage class, so "extern" is a
    /// symbol-borne fact goto-def's decl→def ranking can ask the value for).
    attrs_by_match: HashMap<usize, Vec<String>>,
    /// `@ns.inline` — an inline namespace's NAME token, fired by a name-only
    /// sibling pattern (its def/scope/context come from the base namespace
    /// pattern, a different match). Joined to the Package symbol by name span
    /// in a post-pass, tagging it "inline" so the qualified-completion gather
    /// can lift its members into the enclosing namespace.
    inline_ns_spans: Vec<(Point, Point)>,
    /// `@alias.name` (a typedef/using type alias) + `@alias.of` (its
    /// underlying type text), joined per match → `TypeName` alias witnesses.
    alias_name_by_match: HashMap<usize, String>,
    alias_of_by_match: HashMap<usize, String>,
    /// Object-like `#define X body` alias halves — kept SEPARATE from the
    /// typedef alias so the emission can gate on a type-shaped body (a
    /// macro-heavy header has thousands of value macros we must NOT mint
    /// TypeName witnesses for).
    macro_alias_name_by_match: HashMap<usize, String>,
    macro_alias_of_by_match: HashMap<usize, String>,
    /// `@spec.primary` — the base name a class-spec def specializes; joined to
    /// its `@def.class` by match to mint the (spec, primary) family edge.
    spec_primary_by_match: HashMap<usize, String>,
    /// `@tmpl.param`/`@tmpl.owner` — one template parameter + the class it
    /// parameterizes per match; joined into ordered per-class lists.
    tmpl_param_by_match: HashMap<usize, (String, usize)>,
    tmpl_owner_by_match: HashMap<usize, String>,
    /// The name tokens of the defs seen so far; a ref inside one is the
    /// declaration, not a use (`refs::handle`).
    pub(super) def_name_spans: Vec<(Point, Point)>,
    /// Each class minted so far, by its name token's start: the identity
    /// it is filed under. A pattern that names the class again (its
    /// `@context.class`, a `@parent` match) reads the class from here
    /// rather than minting it a second time.
    classes_by_name: HashMap<Point, String>,
    /// Matches whose defs share one name token as one declaration (`@def.pair`).
    paired_matches: std::collections::HashSet<usize>,
}

impl State {
    /// The match's `@def.<kind>.name` token: its text and where it starts.
    pub(super) fn name_of(&self, match_id: usize, kind: DefCap) -> Option<(&str, Point)> {
        self.names_by_match.get(&(match_id, kind)).map(|(n, s, _)| (n.as_str(), *s))
    }

    /// The name a match's anchored def was given, which its context names.
    pub(super) fn anchored_name(&self, match_id: usize) -> Option<&str> {
        if !self.anchored_matches.contains(&match_id) {
            return None;
        }
        self.names_by_match.iter().find(|((m, _), _)| *m == match_id).map(|(_, (n, _, _))| n.as_str())
    }

    /// The class whose `@def.class.name` this match captured, as its def
    /// minted it.
    pub(super) fn class_of_match(&self, match_id: usize) -> Option<&str> {
        let (_, at) = self.name_of(match_id, DefCap::Class)?;
        self.classes_by_name.get(&at).map(String::as_str)
    }
}

/// `@ool.def`: a function definition whose declared name may be qualified
/// (`Ret Class::method(...) {}`). When it is, synthesize the `def.method` /
/// `def.method.name` / `qualifier` events the per-shape patterns emit for the
/// shapes they own; an unqualified one yields nothing here, because its own
/// pattern owns it. The links reach any declarator depth and any number of
/// qualifiers.
pub(super) fn flatten_ool_def(
    node: tree_sitter::Node<'_>,
    links: &super::links::Links<'_>,
    source: &[u8],
    match_id: usize,
    events: &mut Vec<Event>,
) {
    let Some((owner, leaf)) = links.qualified_def(node, source) else {
        return;
    };
    let leaf_text = leaf.utf8_text(source).unwrap_or("").to_string();
    // the def symbol spans the whole function_definition; name +
    // owner come from the qualified declarator's leaf + scope.
    let method = |part| Capture::Def { kind: DefCap::Method, part };
    events.push(Event::at(node, method(DefPart::Node), leaf_text.clone(), match_id));
    events.push(Event::at(leaf, method(DefPart::Name), leaf_text, match_id));
    events.push(Event::at(node, Capture::Qualifier, owner, match_id));
}

/// `@nested.target`: a pointer/reference declarator CHAIN of any depth.
/// Peel it (where the node is live) to the leaf identifier + the deref
/// stack, then emit the leaf as if the query had captured it directly —
/// downstream join/symbol/witness paths are unchanged, and arbitrary
/// nesting works without enumerating it.
pub(super) fn flatten_nested(
    defs: &mut State,
    node: tree_sitter::Node<'_>,
    links: &super::links::Links<'_>,
    source: &[u8],
    match_id: usize,
    events: &mut Vec<Event>,
) {
    if let Some(super::links::Peeled { leaf, stack, def }) = links.peel_declarator(node, source) {
        defs.nested_stacks.insert(match_id, stack);
        let ltext = leaf.utf8_text(source).unwrap_or("").to_string();
        let def = Capture::Def { kind: def, part: DefPart::Node };
        for syn in [Capture::Flow(FlowCap::Target), def] {
            events.push(Event::at(leaf, syn, ltext.clone(), match_id));
        }
    }
}

impl State {
    /// Whether match `match_id`'s defs declare in the scope around them.
    pub(super) fn hoists(&self, match_id: usize) -> bool {
        self.hoisted_matches.contains(&match_id)
    }

    /// The class the written type at `start..end` names by where it is
    /// written (`@type.<relative>`), if it is one.
    pub(super) fn names_relative(&self, start: Point, end: Point) -> Option<RelativeClass> {
        self.relative_types.get(&(start, end)).copied()
    }

    /// The kinds of def whose name token `e` is, in its own match.
    pub(super) fn declared_by(&self, e: &Event) -> &[DefCap] {
        self.kinds_by_name.get(&(e.match_id, e.start)).map_or(&[], Vec::as_slice)
    }
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    let d = &mut st.defs;
    match e.cap {
        Capture::Def { kind, part: DefPart::Name } => {
            d.names_by_match.insert((e.match_id, kind), (e.text.clone(), e.start, e.end));
            d.kinds_by_name.entry((e.match_id, e.start)).or_default().push(kind);
        }
        Capture::DefHoist => {
            d.hoisted_matches.insert(e.match_id);
        }
        Capture::Def { kind, part: DefPart::Anchor } => {
            if let Some(name) = (st.pack.default_name)(kind, e.start) {
                d.names_by_match.insert((e.match_id, kind), (name, e.start, e.end));
                d.anchored_matches.insert(e.match_id);
            }
        }
        Capture::Def { kind, part: DefPart::Spelled } => {
            d.spelled_by_match.insert((e.match_id, kind), e.text.clone());
        }
        Capture::DefPair => {
            d.paired_matches.insert(e.match_id);
        }
        Capture::Def { kind, part: DefPart::Receiver } => {
            d.receivers_by_match.insert((e.match_id, kind), e.text.clone());
        }
        Capture::Qualifier => {
            d.qualifier_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::RetType => {
            d.rettype_by_match.insert(e.match_id, (e.text.clone(), e.start, e.end));
        }
        Capture::TypeRelative(rel) => {
            d.relative_types.insert((e.start, e.end), rel);
        }
        Capture::Mark(m) => {
            *d.marks.entry((e.start, e.end)).or_default() |= m.flag();
        }
        Capture::SymAttr => {
            d.attrs_by_match.entry(e.match_id).or_default().push(e.text.clone());
        }
        Capture::NsInline => d.inline_ns_spans.push((e.start, e.end)),
        Capture::Alias(AliasPart::Name) => {
            d.alias_name_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::Alias(AliasPart::Of) => {
            d.alias_of_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::MacroAlias(AliasPart::Name) => {
            d.macro_alias_name_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::MacroAlias(AliasPart::Of) => {
            d.macro_alias_of_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::SpecPrimary => {
            d.spec_primary_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::Tmpl(TmplCap::Param) => {
            d.tmpl_param_by_match.insert(e.match_id, (e.text.clone(), e.start_byte));
        }
        Capture::Tmpl(TmplCap::Owner) => {
            d.tmpl_owner_by_match.insert(e.match_id, e.text.clone());
        }
        _ => {}
    }
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    let Capture::Def { kind, part: DefPart::Node } = e.cap else { return };
    let pack = st.pack;
    let d = &st.defs;
    let (name, name_start, name_end, defaulted) = d
        .names_by_match
        .get(&(e.match_id, kind))
        .cloned()
        .map(|(n, s, en)| (n, s, en, d.anchored_matches.contains(&e.match_id)))
        .or_else(|| (pack.default_name)(kind, e.start).map(|n| (n, e.start, e.start, true)))
        .unwrap_or((e.text.clone(), e.start, e.end, false));
    // A slot written through a receiver is a candidate: its owner is the
    // receiver's type, which the assembled analysis answers
    // (`skeleton::declare_slots`), and which decides whether the write
    // declares. Its name token stays the member reference it is.
    let through = d.receivers_by_match.get(&(e.match_id, kind)).cloned();
    if through.is_none() {
        st.defs.def_name_spans.push((name_start, name_end));
    }
    let d = &st.defs;
    // An out-of-line def's qualifier names its owning class (the innermost
    // scope), overriding the enclosing-namespace context.
    let pkg = match through {
        Some(_) => None,
        None => d.qualifier_by_match.get(&e.match_id).cloned().or_else(|| st.package.clone()),
    };
    let shaped = (pack.shape_name)(&name);
    // Under a use-map a class is filed under its identity, and its package
    // is the home that identity joins (`scopes::home_of`).
    let (shaped, pkg) = match (kind, pack.names.use_map_sep()) {
        (DefCap::Class, Some(sep)) => {
            let home = super::scopes::home_of(st, st.cur_scope, e.start).map(str::to_string);
            (crate::model::conventions::join_qualified(home.as_deref().unwrap_or(""), &shaped, sep), home)
        }
        _ => (shaped, pkg),
    };
    if kind == DefCap::Class {
        st.defs.classes_by_name.insert(name_start, shaped.clone());
    }
    let d = &st.defs;
    // A class-spec def carries its primary's name — the
    // (spec, primary) family edge `Specializes` derives from.
    if let Some(primary) = d.spec_primary_by_match.get(&e.match_id) {
        st.out.specializations.push((shaped.clone(), (pack.shape_name)(primary)));
    }
    // A written return resolves like a parameter's type once the walk is
    // done (`resolve_returns`): a spelling at its site, a relative mark
    // through the class the def is written in.
    let mut written_return = None;
    let return_type = match d.rettype_by_match.get(&e.match_id).cloned() {
        Some((t, s, en)) => match d.names_relative(s, en) {
            Some(rel) => {
                written_return = pkg.clone().map(|class| WrittenType::Relative(rel, class));
                None
            }
            None => {
                let t = (pack.annot_type)(&t);
                if t.is_some() {
                    written_return = Some(WrittenType::Spelled(s));
                }
                t
            }
        },
        None => None,
    };
    let d = &st.defs;
    let symbol = SkelSymbol {
        name: shaped,
        kind,
        start: e.start,
        end: e.end,
        name_start,
        name_end,
        package: pkg,
        scope: st.def_scope(kind, e.match_id),
        spelled: d.spelled_by_match.get(&(e.match_id, kind)).cloned(),
        pair: d.paired_matches.contains(&e.match_id).then_some(e.match_id),
        return_type,
        deref_stack: d.nested_stacks.get(&e.match_id).cloned().unwrap_or_default(),
        attributes: {
            let mut a = d.attrs_by_match.get(&e.match_id).cloned().unwrap_or_default();
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
        qualifier_owned: d.qualifier_by_match.contains_key(&e.match_id),
        flags: d.marks.get(&(name_start, name_end)).copied().unwrap_or_default(),
        // Placed once every arm is in (`extract_in`).
        arm: Default::default(),
    };
    match through {
        Some(receiver) => st.out.slot_writes.push(SlotWrite { symbol, receiver }),
        None => {
            if let Some(written) = written_return {
                st.defs.written_returns.push((st.out.symbols.len(), written));
            }
            st.out.symbols.push(symbol);
        }
    }
}

/// Resolve each written return to the identities it names at its site, the
/// way a parameter's declared type is. Runs before dedup, while a symbol's
/// index is still where its def pushed it.
pub(super) fn resolve_returns(st: &mut ExtractState) {
    for (i, written) in std::mem::take(&mut st.defs.written_returns) {
        let t = match written {
            WrittenType::Spelled(site) => {
                st.out.symbols[i].return_type.take().map(|t| written_type_at(&st.out, &st.pack.names, t, site))
            }
            WrittenType::Relative(rel, class) => st.out.relative_class(rel, &class).map(InferredType::ClassName),
        };
        st.out.symbols[i].return_type = t;
    }
}

/// A declared return as written: a spelling at its site, or a relative mark
/// in the class it is written in.
enum WrittenType {
    Spelled(Point),
    Relative(RelativeClass, String),
}

/// Variables declared by being written (`@def.assigned`). In source order,
/// a write declares its name only when no declaration the write reaches
/// (`write_lookup_chain`) is in force there — a parameter, an earlier
/// write, the variable around a closure that captures it. The declaration
/// is its callable's (or, outside any callable, the file's), so every use
/// in the body reaches it whatever the order. Every other write is a
/// `var_writes` row of the declaration it reaches.
pub(super) fn unify_assigned(st: &mut ExtractState) {
    use crate::model::file_analysis::{callable_reach, latest_in_force, write_lookup_chain, ScopeId, SymKind, VariableDecl};
    let out = &mut st.out;
    if !out.symbols.iter().any(|s| s.kind == DefCap::Assigned) {
        return;
    }
    // Every other Variable def declares where it is written, from its token on.
    let mut decls: HashMap<String, Vec<(ScopeId, VariableDecl<'static, ()>)>> = HashMap::new();
    let decl = |at, scope_wide| VariableDecl { id: (), at, gate: None, scope_wide };
    for s in out.symbols.iter().filter(|s| s.kind != DefCap::Assigned && s.kind.sym_kind() == SymKind::Variable) {
        decls.entry(s.name.clone()).or_default().push((s.scope, decl(s.name_start, false)));
    }
    let mut order: Vec<usize> = (0..out.symbols.len()).filter(|&i| out.symbols[i].kind == DefCap::Assigned).collect();
    order.sort_by_key(|&i| out.symbols[i].name_start);
    let mut keep = vec![true; out.symbols.len()];
    for i in order {
        let s = &out.symbols[i];
        let chain = write_lookup_chain(&out.scopes, s.scope, &s.name);
        // The binders' per-scope rule (`variable_use`), along the write chain.
        let seen = decls.get(&s.name).is_some_and(|d| {
            chain.scopes().any(|sc| latest_in_force(d.iter().filter(|(ds, _)| *ds == sc).map(|(_, d)| *d), s.name_start).is_some())
        });
        if seen {
            keep[i] = false;
            out.var_writes.push((s.name.clone(), s.scope, Span { start: s.name_start, end: s.name_end }));
        } else {
            let home = callable_reach(&out.scopes, s.scope).last().unwrap_or(s.scope);
            decls.entry(s.name.clone()).or_default().push((home, decl(s.name_start, true)));
            out.symbols[i].scope = home;
        }
    }
    let mut it = keep.iter();
    out.symbols.retain(|_| *it.next().unwrap());
}

/// Template params joined to their owner class — the owner shaped like a
/// def name (a partial spec's spelling canonicalizes) so the key matches
/// the Class symbol's identity. Source order = the `ParamOf` index axis.
pub(super) fn join_template_params(st: &mut ExtractState) {
    let d = &st.defs;
    let mut rows: Vec<(String, String, usize)> = d
        .tmpl_param_by_match
        .iter()
        .filter_map(|(mid, (param, pos))| {
            let owner = d.tmpl_owner_by_match.get(mid)?;
            Some(((st.pack.shape_name)(owner), param.clone(), *pos))
        })
        .collect();
    rows.sort_by_key(|&(_, _, pos)| pos);
    rows.dedup();
    st.out.template_params = rows;
}

/// Inline namespaces: tag the Package symbol by name span.
pub(super) fn tag_inline_namespaces(st: &mut ExtractState) {
    let spans = &st.defs.inline_ns_spans;
    if spans.is_empty() {
        return;
    }
    let same = |a: Point, b: Point| a.row == b.row && a.column == b.column;
    for s in st.out.symbols.iter_mut() {
        if s.kind == DefCap::Package
            && spans.iter().any(|&(ns, ne)| same(ns, s.name_start) && same(ne, s.name_end))
        {
            s.attributes.push("inline".to_string());
        }
    }
}

/// Def dedup: `f <- function` matches both the sub and the generic var
/// pattern (keep the more specific kind per name site), and a
/// trailing-return function matches both its leading-`auto` pattern and the
/// trailing sibling (keep the rettype-bearing copy).
pub(super) fn dedup(st: &mut ExtractState) {
    let symbols = &mut st.out.symbols;
    // The defs a pair declares at one token are both kept, and stand in
    // for any other match's def of the same model kind there.
    let paired: std::collections::HashSet<(usize, usize, crate::model::file_analysis::SymKind)> = symbols
        .iter()
        .filter(|s| s.pair.is_some())
        .map(|s| (s.name_start.row, s.name_start.column, s.kind.sym_kind()))
        .collect();
    let mut best: HashMap<(usize, usize, Option<DefCap>), usize> = HashMap::new();
    let mut keep = vec![true; symbols.len()];
    for (i, sym) in symbols.iter().enumerate() {
        if sym.pair.is_none() && paired.contains(&(sym.name_start.row, sym.name_start.column, sym.kind.sym_kind())) {
            keep[i] = false;
            continue;
        }
        let key = (sym.name_start.row, sym.name_start.column, sym.pair.map(|_| sym.kind));
        match best.get(&key) {
            None => {
                best.insert(key, i);
            }
            Some(&j) => {
                let (gen_i, gen_j) = (symbols[i].kind.is_general(), symbols[j].kind.is_general());
                let upgrade_ret = symbols[i].kind == symbols[j].kind
                    && symbols[i].return_type.is_some()
                    && symbols[j].return_type.is_none();
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
    symbols.retain(|_| *it.next().unwrap());
}

/// typedef / using aliases, then object-like `#define X body` aliases →
/// `TypeName` witnesses (the alias graph).
///
/// `typedef unsigned short U16` / `using U16 = unsigned short` push
/// `TypeName("U16") → <underlying>`: a primitive leaf stays an
/// `InferredType`; a class-shaped underlying edges to `TypeName(that)` so
/// an alias chain (`typedef V16 W16`) chases; an unrecognized leaf spelling
/// (`unsigned short` — has a space) is `ClassName(text)` so hover shows the
/// raw spelling. Struct/union/enum tag typedefs (`typedef struct op OP`)
/// don't reach here — the skeleton's @parent edge already aliases them.
///
/// A `#define` alias joins the same graph, so a field/var typed
/// `PERL_BITFIELD16` (defined cross-file in another header via a
/// config-guarded `#define`) chases through to its integer leaf. It is
/// gated on a TYPE-shaped body so the sea of value macros (`#define MAX
/// 100`) mints nothing.
///
/// Both emit in match-id (≈ source) order: two `#define`s / typedefs of
/// the SAME alias name (a config-variant type macro like
/// `PERL_BITFIELD16`, guarded `#ifdef … #else …`) land competing
/// `TypeName(alias)` witnesses, and the reducer is latest-wins — a
/// HashMap-iteration emission order makes the winner flip per process
/// (Rust's randomized hasher). Sorted emission fixes the winner to the
/// last-defined variant, deterministically.
pub(super) fn emit_type_aliases(st: &mut ExtractState) {
    let d = &st.defs;
    let annot_type = st.pack.annot_type;
    let mut alias_mids: Vec<&usize> = d.alias_name_by_match.keys().collect();
    alias_mids.sort_unstable();
    for mid in alias_mids {
        let alias = &d.alias_name_by_match[mid];
        let Some(underlying) = d.alias_of_by_match.get(mid) else { continue };
        st.out.witnesses.push(alias_witness(alias, "skeleton-typedef", type_alias_payload(underlying.trim(), annot_type)));
    }
    let mut macro_mids: Vec<&usize> = d.macro_alias_name_by_match.keys().collect();
    macro_mids.sort_unstable();
    for mid in macro_mids {
        let alias = &d.macro_alias_name_by_match[mid];
        let Some(underlying) = d.macro_alias_of_by_match.get(mid) else { continue };
        let underlying = underlying.trim();
        if !looks_like_type_spelling(underlying) {
            continue;
        }
        st.out.witnesses.push(alias_witness(alias, "skeleton-macro-alias", type_alias_payload(underlying, annot_type)));
    }
}

fn alias_witness(
    alias: &str,
    source: &str,
    payload: crate::model::witnesses::WitnessPayload,
) -> crate::model::witnesses::Witness {
    crate::model::witnesses::Witness {
        attachment: crate::model::witnesses::WitnessAttachment::TypeName(alias.to_string()),
        source: crate::model::witnesses::WitnessSource::Builder(source.into()),
        payload,
        span: Span { start: Point { row: 0, column: 0 }, end: Point { row: 0, column: 0 } },
    }
}

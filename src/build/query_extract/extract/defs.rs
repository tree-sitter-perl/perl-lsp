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
    /// `@def.<kind>.spelled`: the declaration as written around its name
    /// token, joined to the def by match.
    spelled_by_match: HashMap<(usize, DefCap), String>,
    /// Matches whose defs share one name token as one declaration (`@def.pair`).
    paired_matches: std::collections::HashSet<usize>,
    /// Matches whose defs declare in the scope around them (`@def.hoist`).
    hoisted_matches: std::collections::HashSet<usize>,
    /// `@qualifier` (a `Class::` on an out-of-line def) and `@rettype` (a
    /// method's declared return type) — pre-collected like names because the
    /// `@def` event fires before these inner captures.
    qualifier_by_match: HashMap<usize, String>,
    rettype_by_match: HashMap<usize, (String, Point, Point)>,
    /// `@rettype.enclosing`: the declared returns that name the class their
    /// method is written in. Joined to a def's `@rettype` by span, because
    /// the stating pattern is not the def's match.
    enclosing_returns: std::collections::HashSet<(Point, Point)>,
    /// (symbol index, site) of each declared return a class spelling may
    /// name, resolved through the use-map once every row is in.
    written_returns: Vec<(usize, Point)>,
    /// Flags the document states about the def whose name token sits at a
    /// span (`@mark.<flag>`, and `@param.receiver` → `RECEIVER`). Joined by
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
    /// Byte ranges of the def nodes seen so far; a ref inside one is the
    /// declaration, not a use (`refs::handle`).
    pub(super) def_name_spans: Vec<(usize, usize)>,
}

impl State {
    /// The name token a match captured for its def of `kind`.
    /// The match's `@def.<kind>.name` token: its text and where it starts.
    pub(super) fn name_of(&self, match_id: usize, kind: DefCap) -> Option<(&str, Point)> {
        self.names_by_match.get(&(match_id, kind)).map(|(n, s, _)| (n.as_str(), *s))
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

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    let d = &mut st.defs;
    match e.cap {
        Capture::Def { kind, part: DefPart::Name } => {
            d.names_by_match.insert((e.match_id, kind), (e.text.clone(), e.start, e.end));
        }
        Capture::DefPair => {
            d.paired_matches.insert(e.match_id);
        }
        Capture::DefHoist => {
            d.hoisted_matches.insert(e.match_id);
        }
        Capture::Def { kind, part: DefPart::Spelled } => {
            d.spelled_by_match.insert((e.match_id, kind), e.text.clone());
        }
        Capture::Qualifier => {
            d.qualifier_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::RetType => {
            d.rettype_by_match.insert(e.match_id, (e.text.clone(), e.start, e.end));
        }
        Capture::RetTypeEnclosing => {
            d.enclosing_returns.insert((e.start, e.end));
        }
        Capture::ParamReceiver => {
            *d.marks.entry((e.start, e.end)).or_default() |= crate::model::file_analysis::SymbolFlags::RECEIVER;
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
    st.defs.def_name_spans.push((e.start_byte, e.end_byte));
    let d = &st.defs;
    let (name, name_start, name_end, defaulted) = d
        .names_by_match
        .get(&(e.match_id, kind))
        .cloned()
        .map(|(n, s, en)| (n, s, en, false))
        .or_else(|| (pack.default_name)(kind).map(|n| (n.to_string(), e.start, e.start, true)))
        .unwrap_or((e.text.clone(), e.start, e.end, false));
    // An out-of-line def's qualifier names its owning class (the innermost
    // scope), overriding the enclosing-namespace context.
    let pkg = d.qualifier_by_match.get(&e.match_id).cloned().or_else(|| st.package.clone());
    let shaped = (pack.shape_name)(&name);
    // Under a use-map a class is filed under its identity, and its package
    // is the namespace that identity carries.
    let (shaped, pkg) = match (kind, pack.names.use_map_sep()) {
        (DefCap::Class, Some(_)) => (st.class_identity(&shaped, e.start), st.namespace_at(e.start).map(str::to_string)),
        _ => (shaped, pkg),
    };
    // A class-spec def carries its primary's name — the
    // (spec, primary) family edge `Specializes` derives from.
    if let Some(primary) = d.spec_primary_by_match.get(&e.match_id) {
        st.out.specializations.push((shaped.clone(), (pack.shape_name)(primary)));
    }
    // A return naming the enclosing class is the def's own class, already an
    // identity; any other spelling is a written type, read like a
    // parameter's.
    let return_type = match d.rettype_by_match.get(&e.match_id).cloned() {
        Some((_, s, en)) if d.enclosing_returns.contains(&(s, en)) => pkg.clone().map(InferredType::ClassName),
        Some((t, s, _)) => {
            let t = (pack.annot_type)(&t);
            if t.is_some() {
                st.defs.written_returns.push((st.out.symbols.len(), s));
            }
            t
        }
        None => None,
    };
    let d = &st.defs;
    st.out.symbols.push(SkelSymbol {
        name: shaped,
        kind,
        start: e.start,
        end: e.end,
        name_start,
        name_end,
        package: pkg,
        // A data member belongs to its class body even where it is written
        // inside a callable (php's constructor-promoted property).
        scope: if kind == DefCap::Field {
            st.member_scope()
        } else if d.hoisted_matches.contains(&e.match_id) {
            st.out.scopes[st.cur_scope.0 as usize].parent.unwrap_or(st.cur_scope)
        } else {
            st.cur_scope
        },
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
    });
}

/// Resolve each written return to the identities it names at its site, the
/// way a parameter's declared type is. Runs before dedup, while a symbol's
/// index is still where its def pushed it.
pub(super) fn resolve_returns(st: &mut ExtractState) {
    for (i, site) in std::mem::take(&mut st.defs.written_returns) {
        if let Some(t) = st.out.symbols[i].return_type.take() {
            st.out.symbols[i].return_type = Some(written_type_at(&st.out, &st.pack.names, t, site));
        }
    }
}

/// Variables declared by being written (`@def.assigned`). In source order,
/// a write declares its name only when nothing the write can see declares
/// it already — a parameter, an earlier write, the variable around a
/// closure that captures it; the declaration is the callable's (or, outside
/// any callable, the file's), so every use in the body reaches it whatever
/// the order. Every other write is a `var_writes` row of that declaration.
pub(super) fn unify_assigned(st: &mut ExtractState) {
    use crate::model::file_analysis::ScopeKind;
    let out = &mut st.out;
    if !out.symbols.iter().any(|s| s.kind == DefCap::Assigned) {
        return;
    }
    let owning = |mut s: ScopeId| loop {
        let sc = &out.scopes[s.0 as usize];
        match (&sc.kind, sc.parent) {
            (ScopeKind::Sub { .. }, _) | (_, None) => return s,
            (_, Some(p)) => s = p,
        }
    };
    // Every other Variable def declares where it is written, from its token on.
    let mut declared: HashMap<(ScopeId, String), Point> = HashMap::new();
    for s in out.symbols.iter().filter(|s| s.kind != DefCap::Assigned && s.kind.sym_kind() == crate::model::file_analysis::SymKind::Variable) {
        let at = declared.entry((s.scope, s.name.clone())).or_insert(s.name_start);
        *at = (*at).min(s.name_start);
    }
    let mut order: Vec<usize> = (0..out.symbols.len()).filter(|&i| out.symbols[i].kind == DefCap::Assigned).collect();
    order.sort_by_key(|&i| out.symbols[i].name_start);
    let mut accepted: std::collections::HashSet<(ScopeId, String)> = Default::default();
    let mut keep = vec![true; out.symbols.len()];
    for i in order {
        let s = &out.symbols[i];
        let seen = out.variable_lookup_chain(s.scope, &s.name)
            .into_iter()
            .any(|sc| {
                accepted.contains(&(sc, s.name.clone()))
                    || declared.get(&(sc, s.name.clone())).is_some_and(|&at| at <= s.name_start)
            });
        if seen {
            keep[i] = false;
            out.var_writes.push((s.name.clone(), s.scope, Span { start: s.name_start, end: s.name_end }));
        } else {
            let home = owning(s.scope);
            accepted.insert((home, s.name.clone()));
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
    let mut best: HashMap<(usize, usize, Option<DefCap>), usize> = HashMap::new();
    let mut keep = vec![true; symbols.len()];
    for (i, sym) in symbols.iter().enumerate() {
        // The defs a pair declares at one token are both kept.
        let key = (sym.name_start.row, sym.name_start.column, sym.pair.map(|_| sym.kind));
        match best.get(&key) {
            None => {
                best.insert(key, i);
            }
            Some(&j) => {
                let (gen_i, gen_j) = (symbols[i].kind == DefCap::Var, symbols[j].kind == DefCap::Var);
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

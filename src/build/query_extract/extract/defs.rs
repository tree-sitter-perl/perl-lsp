//! `@def.*` and the captures that decorate a def (its qualifier, return
//! type, attributes, template params), plus type aliases: the symbol rows.

use super::*;
use std::collections::HashMap;

/// The class whose `@def.class.name` this match captured, as its def
/// minted it: a pattern that names the class again (its `@context.class`,
/// a `@parent` match) reads it rather than minting it a second time.
pub(super) fn class_of_match<'s>(st: &'s ExtractState, match_id: usize) -> Option<&'s str> {
    let class = |part| st.adorned.in_match(match_id, Capture::Def { kind: DefCap::Class, part });
    let name = class(DefPart::Name).or_else(|| class(DefPart::Anchor))?;
    st.class_on(name)
}

/// The name a match's anchored def (`@def.<kind>.anchor`) is given — the
/// pack's `default_name` at the anchor token — which its context names.
pub(super) fn anchored_name(st: &ExtractState, match_id: usize) -> Option<String> {
    st.adorned.of_match(match_id).find_map(|t| match t.cap {
        Capture::Def { kind, part: DefPart::Anchor } => (st.pack.default_name)(kind, t.start),
        _ => None,
    })
}

/// `@ool.def`: a function definition whose declared name may be qualified
/// (`Ret Class::method(...) {}`). When it is, synthesize the `def.method` /
/// `def.method.name` / `qualifier` events the per-shape patterns emit for the
/// shapes they own; an unqualified one yields nothing here, because its own
/// pattern owns it. The links reach any declarator depth and any number of
/// qualifiers.
pub(super) fn flatten_ool_def<'t>(
    node: tree_sitter::Node<'t>,
    links: &super::links::Links<'t>,
    source: &[u8],
    match_id: usize,
    events: &mut Vec<Flattened<'t>>,
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
pub(super) fn flatten_nested<'t>(
    node: tree_sitter::Node<'t>,
    links: &super::links::Links<'t>,
    source: &[u8],
    match_id: usize,
    events: &mut Vec<Flattened<'t>>,
) {
    if let Some(super::links::Peeled { leaf, stack, def }) = links.peel_declarator(node, source) {
        let ltext = leaf.utf8_text(source).unwrap_or("").to_string();
        events.push(Event::at(leaf, Capture::Flow(FlowCap::Target), ltext.clone(), match_id));
        let def = Capture::Def { kind: def, part: DefPart::Node };
        events.push(with(Event::at(leaf, def, ltext, match_id), Payload::Deref(stack)));
    }
}

/// The scopes a `@flow.target` at `e` binds in. A target that is the name
/// token defs in its match declare binds where each of them binds (php's
/// promoted parameter is a property and a local on one token); any other
/// binds in the innermost scope, or the def scope of the header it is
/// written in (an annotated parameter).
pub(super) fn binding_scopes(st: &ExtractState, e: &Event) -> Vec<ScopeId> {
    let mut scopes: Vec<ScopeId> = st
        .adorned
        .on(e)
        .filter(|t| t.match_id == e.match_id)
        .filter_map(|t| match t.cap {
            Capture::Def { kind, part: DefPart::Name } => Some(st.def_scope(kind, e.match_id)),
            _ => None,
        })
        .collect();
    scopes.dedup();
    if scopes.is_empty() {
        scopes.push(st.bind_scope());
    }
    scopes
}

pub(super) fn handle(st: &mut ExtractState, ix: usize, e: &Event) {
    let Capture::Def { kind, part: DefPart::Node } = e.cap else { return };
    let pack = st.pack;
    let ad = st.adorned;
    let mine = |cap: Capture| ad.in_match(e.match_id, cap);
    // The name token; for a def named from its anchor, the language's
    // default name at the anchor token; for a nameless def the language's
    // default name (a zero-width token) or the def's own text.
    let name_ix = ad.in_match_ix(e.match_id, Capture::Def { kind, part: DefPart::Name });
    let anchor = ad.in_match_ix(e.match_id, Capture::Def { kind, part: DefPart::Anchor }).and_then(|a| {
        let named = (pack.default_name)(kind, ad.events()[a].start)?;
        Some((named, a))
    });
    let (name, name_ix, defaulted) = match (name_ix, anchor, (pack.default_name)(kind, e.start)) {
        (Some(n), ..) => (ad.events()[n].text.clone(), Some(n), false),
        (None, Some((named, a)), _) => (named, Some(a), true),
        (None, None, Some(default)) => (default, None, true),
        (None, None, None) => (e.text.clone(), Some(ix), false),
    };
    let name_tok = name_ix.map(|n| &ad.events()[n]);
    let (name_start, name_end) = name_tok.map_or((e.start, e.start), |n| (n.start, n.end));
    // A slot written through a receiver is a candidate: its owner is the
    // receiver's type, which the assembled analysis answers
    // (`skeleton::declare_slots`), and which decides whether the write
    // declares. Its name token stays the member reference it is.
    let through = mine(Capture::Def { kind, part: DefPart::Receiver }).map(|r| r.text.clone());
    if let (None, Some(n)) = (&through, name_ix) {
        st.minted[n].declares = true;
    }
    let qualifier = mine(Capture::Qualifier);
    // An out-of-line def's qualifier names its owning class (the innermost
    // scope), overriding the enclosing-namespace context.
    let pkg = match through {
        Some(_) => None,
        None => qualifier.map(|q| q.text.clone()).or_else(|| st.package.clone()),
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
        st.minted[name_ix.unwrap_or(ix)].class = Some(shaped.as_str().into());
    }
    // A class-spec def carries its primary's name — the
    // (spec, primary) family edge `Specializes` derives from.
    if let Some(primary) = mine(Capture::SpecPrimary) {
        st.out.specializations.push((shaped.clone(), (pack.shape_name)(&primary.text)));
    }
    // A written return resolves like a parameter's type once the walk is
    // done (`resolve_returns`): a spelling at its site, a relative mark
    // through the class the def is written in.
    let mut written_return = false;
    let return_type = match mine(Capture::RetType) {
        Some(t) if names_relative(ad, t).is_some() => {
            written_return = pkg.is_some();
            None
        }
        Some(t) => {
            let t = (pack.annot_type)(&t.text);
            written_return = t.is_some();
            t
        }
        None => None,
    };
    // Flags another pattern states about the def's name token
    // (`@mark.<flag>`, `@ns.inline`).
    let on_name: Vec<&Event> = name_tok.map(|n| ad.on(n).collect()).unwrap_or_default();
    let flags = on_name.iter().fold(crate::model::file_analysis::SymbolFlags::default(), |f, t| match t.cap {
        Capture::Mark(m) => f | m.flag(),
        _ => f,
    });
    let symbol = SkelSymbol {
        name: shaped,
        kind,
        start: e.start,
        end: e.end,
        name_start,
        name_end,
        package: pkg,
        scope: st.def_scope(kind, e.match_id),
        spelled: mine(Capture::Def { kind, part: DefPart::Spelled }).map(|t| t.text.clone()),
        pair: mine(Capture::DefPair).map(|_| e.match_id),
        return_type,
        deref_stack: ad
            .of_match(e.match_id)
            .find_map(|t| match t.payload.as_deref() {
                Some(Payload::Deref(stack)) => Some(stack.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        attributes: {
            let mut a: Vec<String> =
                ad.of_match(e.match_id).filter(|t| t.cap == Capture::SymAttr).map(|t| t.text.clone()).collect();
            // a default-named symbol is structure, not an
            // addressable name — completion skips it.
            if defaulted {
                a.push("anonymous".to_string());
            }
            // An inline namespace's members lift into the enclosing one
            // in qualified completion.
            if kind == DefCap::Package && on_name.iter().any(|t| t.cap == Capture::NsInline) {
                a.push("inline".to_string());
            }
            a
        },
        // Filled by span association in `into_file_analysis` — the
        // `@arity.sig` match fires separately from this def name.
        arity: None,
        qualifier_owned: qualifier.is_some(),
        flags,
        // Placed once every arm is in (`extract_in`).
        arm: Default::default(),
    };
    match through {
        Some(receiver) => st.out.slot_writes.push(SlotWrite { symbol, receiver }),
        None => {
            if written_return {
                st.minted[ix].return_row = Some(st.out.symbols.len());
            }
            st.out.symbols.push(symbol);
        }
    }
}

/// Resolve each written return to the identities it names at its site, the
/// way a parameter's declared type is. Runs before dedup, while a symbol's
/// index is still where its def pushed it.
pub(super) fn resolve_returns(st: &mut ExtractState) {
    let ad = st.adorned;
    for (ix, e) in ad.events().iter().enumerate() {
        let Some(i) = st.minted[ix].return_row else { continue };
        let Some(written) = ad.in_match(e.match_id, Capture::RetType) else { continue };
        let t = match names_relative(ad, written) {
            Some(rel) => {
                let class = st.out.symbols[i].package.clone();
                class.and_then(|c| st.out.relative_class(rel, &c)).map(InferredType::ClassName)
            }
            None => st.out.symbols[i]
                .return_type
                .take()
                .map(|t| written_type_at(&st.out, &st.pack.names, t, written.start)),
        };
        st.out.symbols[i].return_type = t;
    }
}

/// The class the written type `t` names by where it is written: a
/// `@type.<relative>` another pattern states on its node.
pub(super) fn names_relative(ad: &Adorned, t: &Event) -> Option<RelativeClass> {
    ad.on(t).find_map(|x| match x.cap {
        Capture::TypeRelative(rel) => Some(rel),
        _ => None,
    })
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
    let ad = st.adorned;
    let mut rows: Vec<(String, String, usize)> = ad
        .events()
        .iter()
        .filter(|e| e.cap == Capture::Tmpl(TmplCap::Param))
        .filter_map(|p| {
            let owner = ad.in_match(p.match_id, Capture::Tmpl(TmplCap::Owner))?;
            Some(((st.pack.shape_name)(&owner.text), p.text.clone(), ad.start_byte(p)))
        })
        .collect();
    rows.sort_by_key(|&(_, _, pos)| pos);
    rows.dedup();
    st.out.template_params = rows;
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
    let ad = st.adorned;
    let annot_type = st.pack.annot_type;
    let underlying = |name: &Event, of: Capture| ad.in_match(name.match_id, of).map(|u| u.text.trim());
    for name in ad.by_match().filter(|e| e.cap == Capture::Alias(AliasPart::Name)) {
        let Some(underlying) = underlying(name, Capture::Alias(AliasPart::Of)) else { continue };
        st.out.witnesses.push(alias_witness(&name.text, "skeleton-typedef", type_alias_payload(underlying, annot_type)));
    }
    for name in ad.by_match().filter(|e| e.cap == Capture::MacroAlias(AliasPart::Name)) {
        let Some(underlying) = underlying(name, Capture::MacroAlias(AliasPart::Of)) else { continue };
        if !looks_like_type_spelling(underlying) {
            continue;
        }
        st.out.witnesses.push(alias_witness(&name.text, "skeleton-macro-alias", type_alias_payload(underlying, annot_type)));
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

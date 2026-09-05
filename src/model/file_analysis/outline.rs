//! Document outline + semantic tokens: the outline/token types and the
//! `FileAnalysis` methods that project symbols into them.

use super::*;

// ---- Outline ----

pub struct OutlineSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymKind,
    pub span: Span,
    pub selection_span: Span,
    pub children: Vec<OutlineSymbol>,
    /// The plugin-chosen LSP display kind (`Symbol.presentation.display`),
    /// carried so the outline→DocumentSymbol conversion doesn't re-consult
    /// the symbol. `None` = the fixed SymKind → LSP mapping.
    pub handler_display: Option<HandlerDisplay>,
}

// ---- Semantic tokens ----

// Token type/modifier indices — must match the order in semantic_token_types/modifiers().
// Some are forward-declared for future phases.
pub const TOK_VARIABLE: u32 = 0;
pub const TOK_PARAMETER: u32 = 1;
pub const TOK_FUNCTION: u32 = 2;
pub const TOK_METHOD: u32 = 3;
pub const TOK_MACRO: u32 = 4;
pub const TOK_PROPERTY: u32 = 5;
pub const TOK_NAMESPACE: u32 = 6;
// No TOK_REGEXP: regex literals deliberately emit no semantic token (#63).
pub const TOK_ENUM_MEMBER: u32 = 7;
pub const TOK_KEYWORD: u32 = 8;

pub const MOD_DECLARATION: u32 = 0;
pub const MOD_READONLY: u32 = 1;
pub const MOD_MODIFICATION: u32 = 2;
pub const MOD_DEFAULT_LIBRARY: u32 = 3;
#[allow(dead_code)] pub const MOD_DEPRECATED: u32 = 4;
#[allow(dead_code)] pub const MOD_STATIC: u32 = 5;
pub const MOD_SCALAR: u32 = 6;
pub const MOD_ARRAY: u32 = 7;
pub const MOD_HASH: u32 = 8;

#[derive(Debug, Clone)]
pub struct PerlSemanticToken {
    pub span: Span,
    pub token_type: u32,
    pub modifiers: u32,
}

fn sigil_modifier(sigil: char) -> u32 {
    match sigil {
        '@' => 1 << MOD_ARRAY,
        '%' => 1 << MOD_HASH,
        _ => 1 << MOD_SCALAR,
    }
}


impl FileAnalysis {
    // ---- Document outline ----

    /// Build document outline as a nested symbol tree.
    /// Returns (name, detail, kind, span, selection_span, children) tuples.
    pub fn document_symbols(&self) -> Vec<OutlineSymbol> {
        // All top-level entities (subs/methods declared after
        // `package X;`, `my`/`our` decls, `use` imports, …) attach
        // to the file scope directly. Statement-form `package X;`
        // is package context, not a lexical boundary, so it doesn't
        // create an intermediate scope — `nest_under_packages` folds
        // those siblings under their owning package for the outline
        // tree (#62).
        //
        // Plugin namespaces are NOT surfaced. They exist for
        // cross-file bridge lookups (`for_each_entity_bridged_to`),
        // not as navigation targets — users look for the
        // helpers/routes/tasks themselves, which already render flat
        // with their `<word>` kind prefix.
        let mut flat = self.outline_children_of(ScopeId(0));
        // Siblings render in DOCUMENT order, not symbol-table order.
        // The walk emits in position order so this used to hold for
        // free, but post-walk emission phases (pattern dispatch)
        // append later — the outline is a navigation view, so position
        // is the invariant, not emission time. Stable sort: symbols
        // sharing an anchor (a `has` line's synthesized family) keep
        // their emission order.
        Self::sort_outline_by_position(&mut flat);
        let mut nested = self.nest_under_packages(flat);
        for c in &mut nested {
            Self::sort_outline_by_position(&mut c.children);
        }
        nested
    }

    fn sort_outline_by_position(list: &mut [OutlineSymbol]) {
        list.sort_by_key(|s| {
            (
                s.selection_span.start.row,
                s.selection_span.start.column,
            )
        });
        for s in list {
            Self::sort_outline_by_position(&mut s.children);
        }
    }

    /// Fold file-scope siblings into the package/class they belong to so the
    /// outline (and the editor sticky-scroll / breadcrumb built on it) renders
    /// nested instead of flat (#62).
    ///
    /// A statement-form `package Foo;` is a namespace pin, not a lexical scope,
    /// so its subs/vars live at file scope tagged via `package_ranges`; we
    /// attach each non-container sibling to the most recent preceding container
    /// whose range governs it (`package_at`). Block-form classes already nest
    /// through their lexical body scope and arrive with their children intact;
    /// they just stay containers here. Files with no `package`/`class` at all
    /// (a plain script, or a Mojo::Lite app whose structure is plugin
    /// namespaces) keep the flat list.
    fn nest_under_packages(&self, flat: Vec<OutlineSymbol>) -> Vec<OutlineSymbol> {
        let is_container =
            |k: SymKind| matches!(k, SymKind::Package | SymKind::Class);
        if !flat.iter().any(|s| is_container(s.kind)) {
            return flat;
        }

        let mut result: Vec<OutlineSymbol> = Vec::new();
        for sym in flat {
            if is_container(sym.kind) {
                result.push(sym);
                continue;
            }
            // `package_at` answers "which namespace governs this point",
            // honouring both statement ranges and block spans (innermost wins).
            if let Some(owner) = self.package_at(sym.span.start) {
                if let Some(container) = result
                    .iter_mut()
                    .rev()
                    .find(|c| is_container(c.kind) && c.name == owner)
                {
                    container.children.push(sym);
                    continue;
                }
            }
            result.push(sym);
        }

        // LSP requires a parent symbol's `range` to enclose its children's
        // ranges (sticky scroll, breadcrumb, "symbol at cursor" all rely on
        // containment). A statement-form package's span is just its one-line
        // declaration, so widen each container to cover what we nested under it.
        for c in &mut result {
            if !is_container(c.kind) {
                continue;
            }
            if let Some(end) = c
                .children
                .iter()
                .map(|ch| ch.span.end)
                .max_by_key(|p| (p.row, p.column))
            {
                if (end.row, end.column) > (c.span.end.row, c.span.end.column) {
                    c.span.end = end;
                }
            }
        }
        result
    }

    /// Whether a scope sits inside a sub/method body (directly or via nested
    /// blocks). The single rule behind hiding working-state lexicals from the
    /// outline: variables declared here are local scratch, not structure, so
    /// only file/package- and class-body-scoped variables (`our`, class
    /// `field`s) survive. A `Class` or `File` boundary reached first means
    /// "structural"; a `Sub`/`Method` reached first means "working state".
    /// Both the outline tree builder and the `--outline` CLI ask this.
    pub fn scope_within_sub_body(&self, scope: ScopeId) -> bool {
        // Climb the scope chain; a Sub/Method boundary reached before
        // any Class/File answers true.
        for id in self.scope_chain(scope) {
            match self.scopes[id.0 as usize].kind {
                ScopeKind::Sub { .. } | ScopeKind::Method { .. } => return true,
                ScopeKind::Class { .. } | ScopeKind::File => return false,
                ScopeKind::Block | ScopeKind::ForLoop { .. } => {}
            }
        }
        false
    }

    fn outline_children_of(&self, parent_scope: ScopeId) -> Vec<OutlineSymbol> {
        let mut result = Vec::new();

        // Plugin fan-out (e.g. Mojo helpers register a Method on both
        // Mojolicious::Controller AND Mojolicious) produces multiple
        // Symbols with the same (name, kind, span). Completion and
        // resolution want them all; the outline wants one entry.
        // Keyed by (kind, name, span_start) — tight enough to dedup
        // true fan-out without collapsing user-written overloads.
        let mut outline_seen: HashSet<(SymKind, String, usize, usize)> = HashSet::new();

        for sym in &self.symbols {
            if sym.scope != parent_scope {
                continue;
            }
            // The method receiver (`self`/`cls`) is tagged with the class by
            // the sticky context but isn't outline structure.
            if matches!(sym.kind, SymKind::Variable) && self.pack.receiver_names.contains(&sym.name) {
                continue;
            }
            // Local variables — navigation targets (goto-def, hover) but not
            // outline structure. A local is an unpackaged Variable inside a
            // Sub/Method/Block/ForLoop; fields carry their class as `package`
            // and file/package-level vars live in the File/Package scope, so
            // both still surface.
            if matches!(sym.kind, SymKind::Variable | SymKind::Enumerator)
                && sym.package.is_none()
                && self.scopes.iter().find(|s| s.id == parent_scope).is_some_and(|s| {
                    matches!(
                        s.kind,
                        ScopeKind::Sub { .. }
                            | ScopeKind::Method { .. }
                            | ScopeKind::Block
                            | ScopeKind::ForLoop { .. }
                    )
                })
            {
                continue;
            }
            // Per-symbol opt-out. Plugins mark DSL imports / internal
            // infrastructure so the outline stays focused on real
            // user-visible structure.
            if sym.hidden_in_outline() { continue; }
            if matches!(sym.kind, SymKind::Sub | SymKind::Method) && sym.namespace.is_framework() {
                let key = (
                    sym.kind,
                    sym.name.clone(),
                    sym.span.start.row,
                    sym.span.start.column,
                );
                if !outline_seen.insert(key) {
                    continue;
                }
            }

            let (name, detail, children) = match sym.kind {
                SymKind::Sub | SymKind::Method => {
                    let body_scope = self.find_body_scope(sym);
                    let children = body_scope
                        .map(|s| self.outline_children_of(s))
                        .unwrap_or_default();
                    // LSP `DocumentSymbol.name` should be the bare
                    // identifier; `kind` (Function/Method) is what
                    // tells the client to render the right icon.
                    //
                    // Plugin-tagged subs are the exception: when a
                    // plugin overrides the display word (helper,
                    // action, route, task, event …), SymbolKind has
                    // no enum value that conveys it, so we keep the
                    // `<word>` prefix in `name` for those — it's the
                    // only surviving kind cue once SymbolKind collapses
                    // Helper/Action/Route → FUNCTION. Native subs and
                    // methods get the spec-compliant bare name and
                    // route their kind word through `detail`.
                    let disp = sym.presentation.display;
                    let default_word = if matches!(sym.kind, SymKind::Method) { "method" } else { "sub" };
                    let identifier = sym.presentation.label.clone().unwrap_or_else(|| sym.name.clone());
                    let params_suffix = match &sym.detail {
                        SymbolDetail::Sub { params, .. } => {
                            let visible: Vec<&str> = params.iter()
                                .filter(|p| !p.is_invocant)
                                .map(|p| p.name.as_str())
                                .collect();
                            if visible.is_empty() { String::new() }
                            else { format!(" ({})", visible.join(", ")) }
                        }
                        _ => String::new(),
                    };
                    let (label, outline_detail) = match disp.and_then(|d| d.outline_word()) {
                        Some(plugin_word) => (
                            format!("<{}> {}{}", plugin_word, identifier, params_suffix),
                            Some(plugin_word.to_string()),
                        ),
                        None => (
                            identifier,
                            Some(format!("{}{}", default_word, params_suffix)),
                        ),
                    };
                    (label, outline_detail, children)
                }
                SymKind::Class => {
                    let body_scope = self.find_body_scope(sym);
                    let children = body_scope
                        .map(|s| self.outline_children_of(s))
                        .unwrap_or_default();
                    (sym.name.clone(), Some("class".to_string()), children)
                }
                SymKind::Package => {
                    // Statement-form Perl `package Foo;` has no body scope
                    // (folded via nest_under_packages); a pack-language
                    // namespace has a Block body whose members nest here.
                    let children = self
                        .find_body_scope(sym)
                        .map(|s| self.outline_children_of(s))
                        .unwrap_or_default();
                    (sym.name.clone(), Some("package".to_string()), children)
                }
                // `use` statements are not structure — mainstream language
                // servers (rust-analyzer, pyright, tsserver, gopls, clangd)
                // all keep imports out of the document outline. The synthetic
                // expansions a kit plugin emits would be even worse (a dozen
                // per `use Clove::Base 'Controller'`), but real ones are noise
                // too. Modules still drive resolution; they're just not
                // navigation targets.
                SymKind::Module => continue,
                SymKind::Variable => {
                    if self.scope_within_sub_body(sym.scope) { continue; }
                    // A union CONTAINER (named field-union / synthetic
                    // `(union)`) nests its members: the body scope inside its
                    // span holds them. Attribute-gated — plain variables
                    // never own nested outline structure.
                    if sym.attributes.iter().any(|a| a == "union") {
                        let children = self
                            .find_body_scope(sym)
                            .map(|s| self.outline_children_of(s))
                            .unwrap_or_default();
                        (sym.name.clone(), Some("union".to_string()), children)
                    } else {
                        let detail = match &sym.detail {
                            SymbolDetail::Variable { decl_kind, .. } => match decl_kind {
                                DeclKind::My => "my",
                                DeclKind::Our => "our",
                                DeclKind::State => "state",
                                DeclKind::Field => "field",
                                DeclKind::Param => "param",
                                DeclKind::ForVar => "for",
                            },
                            _ => "my",
                        };
                        (sym.name.clone(), Some(detail.to_string()), Vec::new())
                    }
                }
                SymKind::Field => {
                    (sym.name.clone(), Some("field".to_string()), Vec::new())
                }
                SymKind::Enumerator => {
                    (sym.name.clone(), Some("enumerator".to_string()), Vec::new())
                }
                SymKind::HashKeyDef => continue, // Skip hash key defs from outline
                SymKind::Handler => {
                    // Show registered handlers in the outline. The
                    // semantic word (`event`/`route`/`task`/…) and
                    // params ride along in the NAME — most outline
                    // clients render only `name`, so baking it there
                    // gives stacked registrations (two handlers on
                    // the same name with different sigs, GET + POST
                    // on the same path) visually distinct entries.
                    // `presentation.label` lets the plugin inject a
                    // disambiguator (e.g. HTTP verb) into the
                    // identifier slot without affecting dispatch
                    // lookups, which still key on `sym.name`.
                    let word = sym
                        .presentation
                        .display
                        .and_then(|d| d.outline_word())
                        .unwrap_or("handler");
                    let params_suffix = match &sym.detail {
                        SymbolDetail::Handler { params, .. } => {
                            let visible: Vec<&str> = params
                                .iter()
                                .filter(|p| !p.is_invocant)
                                .map(|p| p.name.as_str())
                                .collect();
                            if visible.is_empty() { String::new() }
                            else { format!(" ({})", visible.join(", ")) }
                        }
                        _ => String::new(),
                    };
                    let identifier = sym.presentation.label.clone().unwrap_or_else(|| sym.name.clone());
                    let label = format!("<{}> {}{}", word, identifier, params_suffix);
                    (label, Some(word.to_string()), Vec::new())
                }
                SymKind::Namespace => {
                    // Real Namespace entries are appended by
                    // `document_symbols` from `plugin_namespaces`,
                    // not from the symbol table. Nothing to do here.
                    continue;
                }
            };

            let handler_display = sym.presentation.display;
            result.push(OutlineSymbol {
                name,
                detail,
                kind: sym.kind,
                span: sym.span,
                selection_span: sym.selection_span,
                children,
                handler_display,
            });
        }

        result
    }

    /// Find the body scope for a sub/method/class symbol.
    pub(super) fn find_body_scope(&self, sym: &Symbol) -> Option<ScopeId> {
        if let Some(id) = self.scopes.iter().find(|s| {
            let kind_matches = match (&s.kind, &sym.kind) {
                (ScopeKind::Sub { name: sn }, SymKind::Sub) => sn == &sym.name,
                (ScopeKind::Method { name: mn }, SymKind::Method) => mn == &sym.name,
                (ScopeKind::Class { name: cn }, SymKind::Class) => cn == &sym.name,
                _ => false,
            };
            kind_matches && s.span == sym.span
        }).map(|s| s.id) {
            return Some(id);
        }
        // Pack-language outline: the query driver mints UNNAMED `Block`
        // scopes for class/namespace bodies, so the Perl name-keyed match
        // above can't find them. A container's body is the Block scope
        // directly inside its span whose parent is the container's own
        // scope. Gated on `Block` so Perl's named containers (which take
        // the exact arm) are untouched. Union-attributed Variables (inline
        // field-union containers) own a body the same way.
        if matches!(sym.kind, SymKind::Package | SymKind::Class)
            || sym.attributes.iter().any(|a| a == "union")
        {
            let start = (sym.span.start.row, sym.span.start.column);
            let end = (sym.span.end.row, sym.span.end.column);
            // The body may be the container's own span (php puts the class
            // scope on the whole declaration): a Block child of the
            // container's scope inside its span is its body either way.
            return self.scopes.iter().find(|s| {
                matches!(s.kind, ScopeKind::Block)
                    && s.parent == Some(sym.scope)
                    && (s.span.start.row, s.span.start.column) >= start
                    && (s.span.end.row, s.span.end.column) <= end
            }).map(|s| s.id);
        }
        None
    }

    // ---- Semantic tokens ----

    /// Collect semantic tokens for all variable references and declarations.
    pub fn semantic_tokens(&self) -> Vec<PerlSemanticToken> {
        let mut tokens: Vec<PerlSemanticToken> = Vec::new();

        // ---- Variable/parameter/self declarations from symbols ----
        for sym in &self.symbols {
            match sym.kind {
                SymKind::Variable | SymKind::Field => {
                    let is_self = crate::model::conventions::is_conventional_invocant_name(&sym.name);
                    let (sigil, is_readonly, is_param) = match &sym.detail {
                        SymbolDetail::Variable { sigil, decl_kind } => {
                            let readonly = matches!(decl_kind, DeclKind::Field);
                            let is_param = matches!(decl_kind, DeclKind::Param | DeclKind::ForVar);
                            (*sigil, readonly, is_param)
                        }
                        SymbolDetail::Field { sigil, attributes } => {
                            let readonly = !attributes.iter().any(|a| a == "writer" || a == "mutator" || a == "accessor");
                            (*sigil, readonly, true)
                        }
                        _ => continue,
                    };
                    let token_type = if is_self { TOK_KEYWORD } else if is_param { TOK_PARAMETER } else { TOK_VARIABLE };
                    // Don't add sigil modifier for $self/$class — it would override the keyword color
                    let mut mods = if is_self { 0 } else { sigil_modifier(sigil) };
                    mods |= 1 << MOD_DECLARATION;
                    if is_readonly { mods |= 1 << MOD_READONLY; }
                    tokens.push(PerlSemanticToken { span: sym.selection_span, token_type, modifiers: mods });
                }
                SymKind::Package | SymKind::Class => {
                    tokens.push(PerlSemanticToken {
                        span: sym.selection_span,
                        token_type: TOK_NAMESPACE,
                        modifiers: 1 << MOD_DECLARATION,
                    });
                }
                SymKind::Module => {
                    tokens.push(PerlSemanticToken {
                        span: sym.selection_span,
                        token_type: TOK_NAMESPACE,
                        modifiers: 0,
                    });
                }
                SymKind::Sub => {
                    let is_constant = matches!(sym.detail, SymbolDetail::Sub { is_constant: true, .. });
                    let token_type = if is_constant { TOK_ENUM_MEMBER } else { TOK_FUNCTION };
                    tokens.push(PerlSemanticToken {
                        span: sym.selection_span,
                        token_type,
                        modifiers: 1 << MOD_DECLARATION,
                    });
                }
                SymKind::Method => {
                    tokens.push(PerlSemanticToken {
                        span: sym.selection_span,
                        token_type: TOK_METHOD,
                        modifiers: 1 << MOD_DECLARATION,
                    });
                }
                _ => {}
            }
        }

        // ---- Refs: variables, functions, methods, properties, namespaces ----
        let imported_names: std::collections::HashSet<&str> = self.imports.iter()
            .flat_map(|imp| imp.imported_symbols.iter().map(|s| s.local_name.as_str()))
            .collect();

        // Local `use constant` decls, keyed by (package, name) — usages color
        // like the declaration. Package-keyed so a same-named non-constant sub
        // in a different package isn't mis-colored as a constant (the recolor
        // matches the call's resolved_package, not just the bare name).
        let constant_names: std::collections::HashSet<(&str, &str)> = self.symbols.iter()
            .filter_map(|s| match &s.detail {
                SymbolDetail::Sub { is_constant: true, .. } =>
                    s.package.as_deref().map(|p| (p, s.name.as_str())),
                _ => None,
            })
            .collect();

        for r in self.refs() {
            // Skip declaration refs — the symbol loop already emits tokens for declarations
            if matches!(r.access, AccessKind::Declaration) {
                continue;
            }
            match &r.kind {
                RefKind::Variable | RefKind::ContainerAccess => {
                    let sigil = r.target_name.chars().next().unwrap_or('$');
                    let is_self =
                        crate::model::conventions::is_conventional_invocant_name(&r.target_name);
                    let token_type = if is_self { TOK_KEYWORD } else { TOK_VARIABLE };
                    // Don't add sigil modifier for $self/$class — it would override the keyword color
                    let mut mods = if is_self { 0 } else { sigil_modifier(sigil) };
                    if matches!(r.access, AccessKind::Write) { mods |= 1 << MOD_MODIFICATION; }
                    tokens.push(PerlSemanticToken { span: r.span, token_type, modifiers: mods });
                }
                RefKind::FunctionCall => {
                    // Constant usages color like the decl; framework DSL keywords → macro.
                    let is_const = r.resolved_package().map_or(false, |pkg| {
                        constant_names.contains(&(pkg, r.unqualified_target_name()))
                    });
                    let token_type = if is_const {
                        TOK_ENUM_MEMBER
                    } else if self.framework_imports.contains(r.target_name.as_str()) {
                        TOK_MACRO
                    } else {
                        TOK_FUNCTION
                    };
                    let mut mods = 0;
                    if imported_names.contains(r.target_name.as_str()) {
                        mods |= 1 << MOD_DEFAULT_LIBRARY;
                    }
                    tokens.push(PerlSemanticToken { span: r.span, token_type, modifiers: mods });
                }
                RefKind::MethodCall { method_name_span, .. } => {
                    // Use method_name_span for precise highlighting of just the method name
                    let mods = 0; // TODO: readonly for ro accessors, static for class methods
                    tokens.push(PerlSemanticToken { span: *method_name_span, token_type: TOK_METHOD, modifiers: mods });
                }
                RefKind::PackageRef => {
                    tokens.push(PerlSemanticToken { span: r.span, token_type: TOK_NAMESPACE, modifiers: 0 });
                }
                RefKind::HashKeyAccess { .. } => {
                    tokens.push(PerlSemanticToken { span: r.span, token_type: TOK_PROPERTY, modifiers: 0 });
                }
                RefKind::DispatchCall { .. } => {
                    // Colors dispatch-call event names like property keys —
                    // same visual weight as other named members of a class.
                    tokens.push(PerlSemanticToken { span: r.span, token_type: TOK_PROPERTY, modifiers: 0 });
                }
            }
        }

        // ---- HashKeyDef symbols → property tokens ----
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::HashKeyDef) {
                tokens.push(PerlSemanticToken {
                    span: sym.selection_span,
                    token_type: TOK_PROPERTY,
                    modifiers: 1 << MOD_DECLARATION,
                });
            }
        }

        // Regex literals deliberately get NO semantic token: the editor's
        // TextMate grammar already scopes them as `string.regexp` *with*
        // embedded escape-sequence highlighting, which a flat `regexp`
        // semantic token would override (and recolor mid-typing). See #63.

        tokens.sort_by_key(|t| (t.span.start.row, t.span.start.column));
        // Dedup by position — if two tokens start at the same (row, col), keep the first
        tokens.dedup_by(|b, a| a.span.start.row == b.span.start.row && a.span.start.column == b.span.start.column);
        tokens
    }
}

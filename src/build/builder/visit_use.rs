//! Import-side visitors: `use` processing, constants, import/export
//! lists, string folding, assignments, and callee-symbol lookup.

use super::*;

impl<'a> Builder<'a> {
    pub(super) fn visit_use(&mut self, node: Node<'a>) {
        // Thin shim: pull the value triple off the CST node and hand it
        // to the value-taking worker. `process_use` runs the actual
        // framework-detection, package_uses tracking, import emission,
        // and plugin dispatch — shared with `EmitAction::SyntheticUse`
        // so kit plugins (`use Co::Base -Class` → SyntheticUse "Moo")
        // hit the exact same code path the user's literal `use Moo`
        // would.
        let Some(module_node) = node.child_by_field_name("module") else { return };
        let Ok(module_name) = module_node.utf8_text(self.source) else { return };
        let raw_args = self.extract_mojo_base_args(node);
        let (imports, _qw_close) = self.extract_use_import_list(node);

        // `use lib` is not an import — it prepends search-path roots to the
        // asker's @INC, which is what decides WHICH file a module name means
        // for this file (`t/lib` for a test, `lib` for the app). Recorded as
        // written; resolution against a root happens at query time.
        if module_name == "lib" {
            for arg in &imports {
                if arg.is_empty() || self.lib_roots.iter().any(|r| r == arg) {
                    continue;
                }
                self.lib_roots.push(arg.clone());
            }
        }

        // Importer consumer form: `use Importer 'M' => qw/foo bar/` imports
        // foo/bar *from M*, not from Importer. Re-target so the import refs
        // and the `Import` entry pin to M — then the existing imported-symbol
        // resolution (goto-def → M's sub; cross-file refs_to) crosses to M.
        // The first stringy arg is the source module; the rest are names.
        // Honest limit: `Importer->import(...)`-style or hashref menu forms
        // aren't covered — only the `use Importer 'M' => @names` line.
        if module_name == "Importer" {
            if let Some((source_module, names)) = imports.split_first() {
                if !source_module.is_empty() && source_module.contains("::") {
                    self.process_use(
                        source_module.clone(),
                        raw_args,
                        names.to_vec(),
                        node_to_span(node),
                        node_to_span(module_node),
                        Some(node),
                        None,
                    );
                    return;
                }
            }
        }

        self.process_use(
            module_name.to_string(),
            raw_args,
            imports,
            node_to_span(node),
            node_to_span(module_node),
            Some(node),
            None, // real source — default `Namespace::Language` on the Module
        );
        // `use Sub::Exporter -setup => { exports => [...] }` declares this
        // package's exports inline — model them so consumers' imports resolve.
        if module_name == "Sub::Exporter" {
            self.detect_sub_exporter_use(node);
        }
        // `use Class::Tiny qw/a b/` / `use Class::Tiny { a => ..., b => ... }`
        // declares rw accessors at the use site (no `has` keyword), so
        // synthesis hangs off the `use` node rather than a framework-mode
        // `has` dispatch. Recognized by the import shape, not a name list.
        if module_name == "Class::Tiny" {
            self.visit_class_tiny_use(node);
        }
        // Don't recurse — use statements don't contain interesting sub-nodes
    }

    /// `use Class::Tiny` synthesizes a rw accessor per attribute name plus the
    /// constructor hash key, mirroring the Moo/Moose `has 'x' => (is=>'rw')`
    /// artifacts (Method symbol + constructor `HashKeyDef`). Class::Tiny
    /// accessors carry no isa constraint, so there's no return type to
    /// publish — just provenance, so `--dump-package` shows the synth origin.
    ///
    /// Two import shapes, both rw:
    ///   `use Class::Tiny qw( a b c );`            — qw list, each word an attr
    ///   `use Class::Tiny { a => $def, b => sub };` — hashref, each KEY an attr
    pub(super) fn visit_class_tiny_use(&mut self, node: Node<'a>) {
        let Some(pkg) = self.current_package.clone() else { return };
        let mut attr_names: Vec<(String, Span)> = Vec::new();
        // The combined form `use Class::Tiny qw(a), { b => ... }` wraps both
        // arg shapes in a `list_expression`, so descend into it.
        self.collect_class_tiny_attrs(node, &mut attr_names);
        if attr_names.is_empty() {
            return;
        }

        let owner = HashKeyOwner::Sub {
            package: Some(pkg),
            name: "new".to_string(),
        };
        for (name, sel_span) in &attr_names {
            // rw accessor: one Method symbol serves getter and writer. No isa
            // → no return type, so no accessor witness (only provenance).
            let acc_id = self.add_symbol(
                name.clone(),
                SymKind::Method,
                node_to_span(node),
                *sel_span,
                SymbolDetail::Sub {
                    params: vec![],
                    is_method: true,
                    doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                },
            );
            self.record_framework_accessor_witness(
                acc_id,
                name,
                None,
                "Class::Tiny",
                format!("Class::Tiny `use` accessor `{}` (rw)", name),
            );
            // Constructor key: `Pkg->new(name => ...)` connects to the attr.
            self.add_symbol(
                name.clone(),
                SymKind::HashKeyDef,
                node_to_span(node),
                *sel_span,
                SymbolDetail::HashKeyDef {
                    owner: owner.clone(),
                    is_dynamic: false,
                },
            );
        }
    }

    /// Walk a `use Class::Tiny ...` node's args for attribute names, handling
    /// the qw-list, hashref, and combined (`qw(a), { b => ... }`) shapes. The
    /// combined form nests both arg shapes under a `list_expression`, so recurse.
    pub(super) fn collect_class_tiny_attrs(&self, node: Node<'a>, names: &mut Vec<(String, Span)>) {
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "quoted_word_list" => self.extract_qw_word_spans(child, names),
                "anonymous_hash_expression" => self.extract_class_tiny_hash_keys(child, names),
                "list_expression" => self.collect_class_tiny_attrs(child, names),
                _ => {}
            }
        }
    }

    /// Collect the KEYS of a Class::Tiny hashref-form `use` (`{ a => $def, ... }`)
    /// as attribute names. Only every other element is a key; the value after
    /// each (default scalar / coderef) is skipped. The grammar nests trailing
    /// pairs as a right-leaning `list_expression`, so recurse into those.
    pub(super) fn extract_class_tiny_hash_keys(&self, node: Node<'a>, names: &mut Vec<(String, Span)>) {
        // Flatten the (possibly right-nested) list of pair elements.
        let mut elems: Vec<Node<'a>> = Vec::new();
        fn flatten<'a>(n: Node<'a>, out: &mut Vec<Node<'a>>) {
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    if c.kind() == "list_expression" {
                        flatten(c, out);
                    } else {
                        out.push(c);
                    }
                }
            }
        }
        flatten(node, &mut elems);
        // Keys are at even indices (key, value, key, value, ...).
        let mut i = 0;
        while i < elems.len() {
            let key = elems[i];
            match key.kind() {
                "autoquoted_bareword" | "bareword" => {
                    if let Ok(text) = key.utf8_text(self.source) {
                        names.push((text.to_string(), node_to_span(key)));
                    }
                }
                "string_literal" | "interpolated_string_literal" => {
                    if let Some(text) = self.extract_string_content(key) {
                        names.push((text, self.string_content_span(key)));
                    }
                }
                _ => {}
            }
            i += 2; // skip the value element
        }
    }

    /// Value-taking core of `use` handling. Shared by real source uses
    /// (`visit_use`) and plugin-emitted `EmitAction::SyntheticUse`. Both
    /// callers route through here so framework mode, package_uses,
    /// Import / Module symbol emission, and the on_use plugin re-dispatch
    /// happen identically — there is no `synthetic: bool` branch inside.
    ///
    /// `node` is `Some` when the call originated from a real CST node and
    /// `None` for synthetic. The only thing it gates is source-positioned
    /// ref emission (parent `PackageRef`s, qw-import `FunctionCall`s,
    /// `use constant` accumulation, the qw-close-paren completion anchor)
    /// — there's no source span to attach those to when the use was
    /// synthesized, so they're skipped. Everything else (the Module
    /// symbol, package_uses, framework_modes, package_parents, the
    /// `Import` entry, plugin dispatch) runs uniformly.
    ///
    /// `synthesized_by` is `Some(plugin_id)` for synthetic uses — the
    /// emitting plugin's id rides through to the Module symbol's
    /// `Namespace::Framework { id }` tag, so `--dump-package` / outline
    /// / completion filters can distinguish "this `use Moo` came from
    /// the user's source" from "this `use Moo` came from the
    /// `co-base` kit plugin reacting to `use Co::Base -Class`". `None`
    /// leaves the Module on the default `Namespace::Language`, matching
    /// every literal `use` line's symbol tagging.
    ///
    /// When a kit chains (`co-base` plugin emits `SyntheticUse "Moo"`,
    /// then the bundled `moo` plugin reacts to it via on_use), the
    /// Module symbol carries the OUTER emitter's id (`co-base`). The
    /// inner emissions get their own emitter's namespace through the
    /// regular `apply_emit_action` path.
    ///
    /// Dedup gate: `(span, current_package, module, raw_args, imports)`
    /// is the work identity. A second real `use Moo;` at a different
    /// span still re-runs (distinct source statements are distinct work);
    /// a SyntheticUse cycle (kit plugin reacting to its own emission,
    /// re-firing with the SAME propagated span) short-circuits here.
    pub(super) fn process_use(
        &mut self,
        module_name: String,
        raw_args: Vec<String>,
        imports: Vec<String>,
        span: Span,
        module_name_span: Span,
        node: Option<Node<'a>>,
        synthesized_by: Option<String>,
    ) {
        let key = (
            span,
            self.current_package.clone(),
            module_name.clone(),
            raw_args.clone(),
            imports.clone(),
        );
        if !self.use_dedup.insert(key) { return; }

        // Module symbol gets the synthesizing plugin's namespace tag
        // when this is a SyntheticUse — that's the channel `--dump-package`
        // / outline / completion already use to surface "this came from
        // plugin X". For literal source `use`, default `Namespace::Language`.
        let module_ns = match &synthesized_by {
            Some(id) => Namespace::framework(id.clone()),
            None => Namespace::Language,
        };
        self.add_symbol_ns(
            module_name.clone(),
            SymKind::Module,
            span,
            module_name_span,
            SymbolDetail::None,
            module_ns,
        );

        // The module name is a package reference: goto-def on `File::Copy` in
        // `use File::Copy;` should reach the external `.pm`, not self-reference
        // (rule #7 — every meaningful token gets a ref; `find_definition` first
        // looks for a ref at the cursor, else falls back to `symbol_at`, which
        // would return this very Module symbol's own span). The cross-file
        // PackageRef resolver (symbols.rs) maps the name to its file; an
        // in-file package resolves locally via `find_package_or_class_in`; a
        // pragma that resolves to neither is an honest no-jump. Only for real
        // source — a synthetic `use` has no name span to anchor on.
        if node.is_some() {
            self.add_ref(
                RefKind::PackageRef,
                module_name_span,
                module_name.clone(),
                AccessKind::Read,
            );
        }

        // Track uses per-package so `Trigger::UsesModule` matches.
        // Populated before any plugin-dispatch site reads it.
        if let Some(pkg) = self.current_package.as_ref().cloned() {
            self.package_uses
                .entry(pkg.clone())
                .or_default()
                .push(module_name.clone());
            // Role verdict: base engines ∪ plugin-declared makers.
            // Shared with SyntheticUse, so kit chains (`use Clove::Role`
            // → SyntheticUse "Moo::Role") mark through either hop.
            if self.role_maker_modules.contains(&module_name) {
                self.role_packages.insert(pkg);
            }
        }

        // Detect framework mode from use statements. Moo-family `has`
        // semantics are manifest-declared (`framework_mode_makers()` in
        // frameworks/moo.rhai — module → flavor + exported keyword surface;
        // rule #8/#10: the plugin owns the module vocabulary, core holds no
        // list). Shared with SyntheticUse, so kit chains grant mode through
        // either hop. Mojo::Base stays a structural arm — its OO-ness is
        // decided by `-base`/parent args, not a name match.
        if let Some(pkg) = self.current_package.as_ref().cloned() {
            if let Some((mode, keywords)) =
                self.framework_mode_modules.get(&module_name).cloned()
            {
                self.framework_modes.insert(pkg, mode);
                self.framework_imports.extend(keywords);
            } else if module_name == "Mojo::Base" {
                // OO-ness is decided by `-base` or a parent-class arg, NOT by
                // `-strict` — `-base` implies strict, and `use Mojo::Base
                // -base, -strict` (redundant but legal) is still a class. The
                // package IS-A its parents (or Mojo::Base for `-base`), so
                // `tap` / `attr` / `new` resolve through the inheritance walk.
                let mut parents: Vec<String> = raw_args.iter()
                    .filter(|s| !s.starts_with('-'))
                    .cloned()
                    .collect();
                let has_base = raw_args.iter().any(|a| a == "-base");
                if has_base {
                    parents.push("Mojo::Base".to_string());
                }
                if !parents.is_empty() {
                    self.apply_mojo_base_mode(pkg, parents, node);
                } else if raw_args.iter().any(|a| a == "-strict") {
                    // Pure `-strict` (no `-base`, no parent): strict-mode only,
                    // no class machinery. A bare `shift` here is arg[0], not the
                    // invocant (see `shift_is_invocant_here`).
                    if let Some(p) = self.current_package.clone() {
                        self.non_oo_packages.insert(p);
                    }
                }
            } else if raw_args.iter().any(|a| a == "-base") {
                // `use Mojo::EventEmitter -base` / any `use X -base`: X becomes a
                // parent AND the package inherits Mojo::Base's `has`/`attr`/`tap`/
                // `new` (the `-base` flag is Mojo::Base's "inherit from me" sugar).
                self.apply_mojo_base_mode(pkg, vec![module_name.clone()], node);
            }
        }

        // Extract parent classes from `use parent` / `use base`
        if module_name == "parent" || module_name == "base" {
            if let Some(pkg) = self.current_package.clone() {
                let parents: Vec<String> = imports.iter()
                    .filter(|s| !s.starts_with('-')) // skip -norequire etc.
                    .cloned()
                    .collect();
                if !parents.is_empty() {
                    if let Some(node) = node {
                        let parent_set: std::collections::HashSet<&str> = parents.iter().map(|s| s.as_str()).collect();
                        self.emit_refs_for_strings(node, &parent_set, RefKind::PackageRef, None);
                    }
                    self.package_parents
                        .entry(pkg)
                        .or_default()
                        .extend(parents);
                }
            }
        }

        // Accumulate constant values: use constant NAME => 'val' / qw(a b).
        // Gated on `Some(node)`: the accumulator scans the CST for the
        // value side of the fat-comma pair, so a synthetic
        // `SyntheticUse "constant"` can't populate `constant_strings`
        // (there's no source to scan). This is the one observable
        // axis where synthetic doesn't match a literal — kit plugins
        // that need to inject constants should request a dedicated
        // `EmitAction::ConstantString { name, values }` rather than
        // routing it through SyntheticUse. See `EmitAction::SyntheticUse`
        // doc for the limitation context.
        if module_name == "constant" {
            if let Some(node) = node {
                self.accumulate_use_constant(node);
            }
        }

        let qw_close_paren = node.and_then(|n| self.find_qw_close_position(n));

        // Emit FunctionCall refs for imported symbol names (for goto-def on import args).
        // These refs pin to the module being imported from — the qw list
        // IS the authoritative source. Synthetic uses skip this — there's
        // no source span for the imported names to live on.
        if module_name != "parent" && module_name != "base" {
            if !imports.is_empty() {
                if let Some(node) = node {
                    // `:tag` / `-tag` group selectors name a `%EXPORT_TAGS`
                    // entry, not a sub — they must not earn a FunctionCall ref
                    // (rule #7: a ref only on a real call target), or the
                    // unresolved-function diagnostic flags the selector itself.
                    let sym_set: std::collections::HashSet<&str> = imports
                        .iter()
                        .filter(|s| !s.starts_with(':') && !s.starts_with('-'))
                        .map(|s| s.as_str())
                        .collect();
                    self.emit_refs_for_strings(
                        node,
                        &sym_set,
                        RefKind::FunctionCall,
                        Some(crate::model::file_analysis::RefBinding::Function {
                            package: module_name.clone(),
                        }),
                    );
                }
            }
        }
        // Base spec: every `qw(a b)` / string-list name is a same-name import.
        // The `name => { -as => 'local' }` rename form (Sub::Exporter /
        // Exporter::Tiny / Exporter `-as`) overrides matching entries with the
        // renamed local→remote binding so goto-def on the local name reaches the
        // origin sub and references bridge the rename.
        let mut imported_symbols: Vec<ImportedSymbol> = imports
            .iter()
            .map(|n| ImportedSymbol::same(n.clone()))
            .collect();
        let mut empty_import = false;
        if let Some(node) = node {
            for (local, remote, alias_span, remote_span) in self.extract_as_renames(node) {
                // The `-as` arg already appears as a bare name in `imports`
                // (the origin) and a string in the hashref (the local); drop
                // both flat entries and replace with the renamed binding.
                imported_symbols.retain(|s| s.local_name != remote && s.local_name != local);
                // Two distinct rename identities:
                //   * the remote-name token IS the source sub — pin it to the
                //     exporting module so renaming `Module::remote` rewrites it.
                //   * the local alias is a binding in the CONSUMING package —
                //     pin it there so it renames with its local calls (which
                //     `resolve_call_package` also keys to this package), never
                //     touching the exporter's symbols.
                if module_name != "parent" && module_name != "base" {
                    self.add_bound_ref(
                        RefKind::FunctionCall,
                        remote_span,
                        remote.clone(),
                        AccessKind::Read,
                        Some(crate::model::file_analysis::RefBinding::Function {
                            package: module_name.clone(),
                        }),
                    );
                    self.add_bound_ref(
                        RefKind::FunctionCall,
                        alias_span,
                        local.clone(),
                        AccessKind::Read,
                        self.current_package
                            .clone()
                            .map(|package| crate::model::file_analysis::RefBinding::Function { package }),
                    );
                }
                imported_symbols.push(ImportedSymbol::renamed(local, remote));
            }
            // `use Foo ();` — explicit empty parens suppress even `@EXPORT`.
            // Distinct from bare `use Foo;` (no arg child at all), which
            // auto-imports the defaults. The parser models the empty list as a
            // `stub_expression`.
            empty_import = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .any(|c| c.kind() == "stub_expression");
        }
        self.imports.push(Import {
            module_name: module_name.clone(),
            imported_symbols,
            span,
            qw_close_paren,
            empty_import,
        });

        // Plugin dispatch for use-statements. Mojolicious::Lite
        // autoimports a verb set (`get`, `post`, `helper`, ...) —
        // the plugin emits `Import` actions pointing at the real
        // source module so hover/gd/sig-help flow through the
        // existing imported-function resolution path. Re-entered
        // (without distinction) when a plugin emits SyntheticUse;
        // `use_dedup` breaks any cycle.
        if !self.plugins.is_empty() {
            let ctx = plugin::UseContext {
                module_name,
                imports,
                raw_args,
                current_package: self.current_package.clone(),
                span,
            };
            self.dispatch_use_plugins(ctx);
        }
    }

    /// Accumulate `use constant NAME => value` into constant_strings and
    /// register each declared constant as a local Sub symbol.
    ///
    /// Two source shapes, both handled:
    ///   * scalar form `use constant NAME => VAL` → `list_expression`
    ///     child whose first named entry is the name, rest the value.
    ///   * block form `use constant { A => 1, B => 2 }` →
    ///     `anonymous_hash_expression` wrapping a flat name/value list.
    ///
    /// `constant`-declared names are package-global subs (no params, no
    /// return shape we track), so emitting a `Sub` symbol satisfies
    /// goto-def and silences the unresolved-function hint at every
    /// same-file callsite.
    pub(super) fn accumulate_use_constant(&mut self, node: Node<'a>) {
        for i in 0..node.child_count() {
            let child = match node.child(i) {
                Some(c) if c.is_named() => c,
                _ => continue,
            };
            match child.kind() {
                "list_expression" => {
                    self.accumulate_constant_pair(child);
                    return;
                }
                "anonymous_hash_expression" => {
                    self.accumulate_constant_block(child);
                    return;
                }
                _ => {}
            }
        }
    }

    /// Scalar `use constant`: first named entry is the name, the rest is
    /// the value side (extracted into `constant_strings`).
    pub(super) fn accumulate_constant_pair(&mut self, list: Node<'a>) {
        let mut name: Option<(String, Node)> = None;
        for j in 0..list.child_count() {
            let c = match list.child(j) {
                Some(c) if c.is_named() => c,
                _ => continue,
            };
            match &name {
                None => {
                    if matches!(c.kind(), "autoquoted_bareword" | "bareword") {
                        if let Ok(text) = c.utf8_text(self.source) {
                            name = Some((text.to_string(), c));
                        }
                    }
                }
                Some((n, _)) => {
                    let values = self.extract_string_names(c);
                    if !values.is_empty() {
                        self.constant_strings.entry(n.clone()).or_default().extend(values);
                    }
                }
            }
        }
        if let Some((n, name_node)) = name {
            self.register_constant_symbol(&n, name_node);
        }
    }

    /// Block `use constant { ... }`: a flat pair list of name/value entries.
    /// Register every name as a Sub symbol and accumulate string values keyed
    /// by name. The separator is irrelevant — `{ A => 1 }` and `{ 'A', 1 }` are
    /// the same positional sequence (autoquoting a bareword key changes its CST
    /// node, not its role), so names come from `extract_node_string` (bareword
    /// *or* quoted string), never from a `=>`-presence gate.
    pub(super) fn accumulate_constant_block(&mut self, hash: Node<'a>) {
        let mut decls: Vec<(String, Node<'a>, Vec<String>)> = Vec::new();
        for (name_node, val_node) in crate::cst::pair_nodes(hash) {
            if let Some(n) = self.extract_node_string(name_node) {
                decls.push((n, name_node, self.extract_string_names(val_node)));
            }
        }
        for (name, name_node, values) in decls {
            self.register_constant_symbol(&name, name_node);
            if !values.is_empty() {
                self.constant_strings.entry(name).or_default().extend(values);
            }
        }
    }

    /// Emit a `FunctionCall` ref for a standalone value-position bareword that
    /// names a `use constant`: the unqualified form when declared in the
    /// enclosing package, the fully-qualified form (`Pkg::NAME`) unconditionally
    /// (it names a constant sub in another package, mirroring an FQ function
    /// call). Resolution is by-name post-walk (same as any FunctionCall ref),
    /// so it points at the constant's Sub symbol — goto-def and references on
    /// the usage, cross-file for the FQ spelling.
    pub(super) fn visit_const_usage(&mut self, node: Node<'a>) {
        let name = match node.utf8_text(self.source) {
            Ok(t) => t,
            Err(_) => return,
        };
        // A `MAX_RETRIES()` call already gets its FunctionCall ref from
        // `visit_function_call` (the `function` field). Don't double-emit.
        // Declaration/name slots aren't value-position barewords either:
        // a sub's own `name`, a package/class/use statement's module —
        // their refs belong to their visitors.
        if let Some(parent) = node.parent() {
            if matches!(
                parent.kind(),
                "function_call_expression" | "ambiguous_function_call_expression"
            ) && parent.child_by_field_name("function").map(|f| f.id()) == Some(node.id())
            {
                return;
            }
            if matches!(
                parent.kind(),
                "subroutine_declaration_statement"
                    | "method_declaration_statement"
                    | "package_statement"
                    | "class_statement"
                    | "use_statement"
                    // `no Foo;` parses as use_statement too — there is
                    // no separate no_statement kind in this grammar.
                    | "require_expression"
            ) {
                return;
            }
        }
        // A fully-qualified value-position bareword (`URI::HAS_RESERVED_...`)
        // names a constant sub in another package. Mirror the FQ function-call
        // ref shape so it resolves cross-file to that sub: `target_name` keeps
        // the full path, the `Function` binding = the qualifier, span = bare tail
        // (narrowest renamable token, rule #7).
        //
        // BUT the method-invocant slot DOES reach this arm — `Foo::Bar->new`
        // recurses the `Foo::Bar` bareword via visit_method_call's children.
        // That's a class name, not a constant call; skip it (visit_method_call
        // already emits the PackageRef for the invocant). Without this guard,
        // references/rename on a sub `Bar` in package `Foo` wrongly matched the
        // `Foo::Bar->new` class invocant.
        if let Some(parent) = node.parent() {
            if parent.kind() == "method_call_expression"
                && parent.child_by_field_name("invocant").map(|i| i.id()) == Some(node.id())
            {
                return;
            }
        }
        if let (Some(qualifier), _) = crate::model::file_analysis::split_qualified(name) {
            let ref_span = fq_tail_span(node, name);
            self.add_bound_ref(
                RefKind::FunctionCall,
                ref_span,
                name.to_string(),
                AccessKind::Read,
                Some(crate::model::file_analysis::RefBinding::Function {
                    package: qualifier.to_string(),
                }),
            );
            return;
        }
        let pkg = match self.current_package.as_ref() {
            Some(p) => p.clone(),
            None => return,
        };
        let is_declared = self
            .declared_constants
            .get(&pkg)
            .is_some_and(|set| set.contains(name));
        if is_declared {
            self.add_bound_ref(
                RefKind::FunctionCall,
                node_to_span(node),
                name.to_string(),
                AccessKind::Read,
                Some(crate::model::file_analysis::RefBinding::Function { package: pkg }),
            );
            return;
        }
        // A bareword naming an in-scope sub IS a call — Perl prefers
        // the defined sub over the class-name reading (`get_config->`
        // calls get_config()), so `my $x = get_config;` and the deref
        // base in `get_config->{host}` deserve the full function
        // treatment: hover, goto-def, references, rename, semantic
        // tokens. `resolve_call_package` is the one seam for "whose
        // sub is this" (enclosing package, then imports) — no pin, no
        // ref, so unresolvable barewords (filehandles, prototypes'
        // leftovers) stay untouched.
        if let Some(owner) = self.resolve_call_package(name) {
            self.add_bound_ref(
                RefKind::FunctionCall,
                node_to_span(node),
                name.to_string(),
                AccessKind::Read,
                Some(crate::model::file_analysis::RefBinding::Function { package: owner }),
            );
        }
    }

    /// Register a single `use constant` name as a parameterless Sub symbol.
    pub(super) fn register_constant_symbol(&mut self, name: &str, name_node: Node<'a>) {
        if let Some(pkg) = self.current_package.clone() {
            self.declared_constants.entry(pkg).or_default().insert(name.to_string());
        }
        let span = node_to_span(name_node);
        self.add_symbol(
            name.to_string(),
            SymKind::Sub,
            span,
            span,
            SymbolDetail::Sub {
                params: Vec::new(),
                is_method: false,
                doc: None,
                opaque_return: false,
                is_constant: true,
                lexical: false,
            },
        );
    }

    /// `require Foo::Bar;` — the bareword module form. Emits the same
    /// `PackageRef` the `use` path emits (rule #7: goto-def reaches the
    /// module's file, references count the load site) plus an `Import`
    /// row that binds nothing (`empty_import`, the `use Foo ()` shape —
    /// `require` never calls `import`) so on-demand @INC resolution
    /// sees the module like any `use`. No `package_uses` entry: a
    /// runtime `require` grants no compile-time keyword surface, so
    /// framework/plugin `UsesModule` triggers must not fire. `require
    /// VERSION` is a different node kind (`require_version_expression`)
    /// and never reaches here; string-path and `$var` operands keep
    /// their normal visitors (documentLink / variable navigation).
    pub(super) fn visit_require(&mut self, node: Node<'a>) {
        let Some(operand) = node.named_child(0) else { return };
        if operand.kind() != "bareword" {
            self.queue_children(node);
            return;
        }
        let Ok(name) = operand.utf8_text(self.source) else { return };
        self.add_ref(
            RefKind::PackageRef,
            node_to_span(operand),
            name.to_string(),
            AccessKind::Read,
        );
        self.imports.push(Import {
            module_name: name.to_string(),
            imported_symbols: vec![],
            span: node_to_span(node),
            qw_close_paren: None,
            empty_import: true,
        });
    }

    /// Strings of a list-ish node, per-word spans included. Syntax
    /// (qw / string literals / list recursion) lives in
    /// `cst::string_list`; this wrapper supplies the constant-folding
    /// hook for bareword / `@list` elements, which needs builder state.
    pub(super) fn extract_string_list(&self, node: Node<'a>) -> Vec<(String, Span)> {
        crate::cst::string_list(node, self.source, &mut |n| {
            let Ok(text) = n.utf8_text(self.source) else { return vec![] };
            let Some(values) = self.resolve_constant_strings(text, 0) else { return vec![] };
            let span = node_to_span(n);
            values.into_iter().map(|v| (v, span)).collect()
        })
    }

    /// Flatten a DSL call's args to `(name, span)` for the `list` projection.
    /// Like `cst::string_list` but with a DSL-arg fold: an `autoquoted_bareword`
    /// is a grammar-certified fat-comma key (its value IS its text, never a
    /// constant lookup); a plain `bareword` / `@array` is a genuine value and
    /// folds through the constant table. The autoquoted rule isn't pushed into
    /// `string_list` itself — that unmasks a use-import bug; see ROADMAP.
    pub(super) fn extract_arg_name_list(&self, node: Node<'a>) -> Vec<(String, Span)> {
        crate::cst::string_list(node, self.source, &mut |n| {
            if n.kind() == "autoquoted_bareword" {
                return match n.utf8_text(self.source) {
                    Ok(text) => vec![(text.to_string(), node_to_span(n))],
                    Err(_) => vec![],
                };
            }
            // plain bareword / @array: a genuine value — fold via constants.
            let Ok(text) = n.utf8_text(self.source) else { return vec![] };
            let Some(values) = self.resolve_constant_strings(text, 0) else { return vec![] };
            let span = node_to_span(n);
            values.into_iter().map(|v| (v, span)).collect()
        })
    }

    /// Extract strings without spans. Convenience for callers that don't need positions.
    pub(super) fn extract_string_names(&self, node: Node<'a>) -> Vec<String> {
        self.extract_string_list(node).into_iter().map(|(s, _)| s).collect()
    }

    /// Emit refs for string names found in a node's children.
    /// Only emits refs for names in `filter` set.
    pub(super) fn emit_refs_for_strings(
        &mut self,
        node: Node<'a>,
        filter: &std::collections::HashSet<&str>,
        ref_kind: RefKind,
        binding: Option<crate::model::file_analysis::RefBinding>,
    ) {
        for (text, span) in self.extract_string_list(node) {
            if filter.contains(text.as_str()) {
                self.add_bound_ref(ref_kind.clone(), span, text, AccessKind::Read, binding.clone());
            }
        }
    }

    pub(super) fn extract_qw_word_spans(&self, qw_node: Node<'a>, results: &mut Vec<(String, Span)>) {
        crate::cst::qw_word_spans(qw_node, self.source, results)
    }

    pub(super) fn find_qw_close_position(&self, node: Node<'a>) -> Option<Point> {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "quoted_word_list" {
                    let end = child.end_position();
                    return Some(Point::new(end.row, end.column.saturating_sub(1)));
                }
            }
        }
        None
    }

    /// CG-3a: when a foreach list is a call to a SAME-FILE sub whose body is a
    /// literal qw/list return, fold that sub's literal return into name(s). The
    /// loop var then constant-folds and downstream glob synthesis (`*$tag = sub`)
    /// installs one symbol per name. Returns empty for anything not a
    /// literal-returning local sub (cross-file callee, computed return, no
    /// match) — the "don't fabricate" boundary; the loop var stays dynamic.
    ///
    /// Tree-walk (rule #1): ascends `call_node` to the file root, scans
    /// top-level `subroutine_declaration_statement`s for the called name. No
    /// symbol-table lookup because the callee may be declared after the loop in
    /// source order (CGI.pm: `_all_html_tags` is defined further down).
    pub(super) fn fold_local_sub_literal_return(&self, call_node: Node<'a>) -> Vec<String> {
        let fname = match call_node.kind() {
            "function_call_expression" | "ambiguous_function_call_expression" => {
                call_node.child_by_field_name("function")
            }
            _ => return vec![],
        };
        let Some(fname) = fname else { return vec![] };
        // Bare callee name only — `&Pkg::foo()` / computed callees are not
        // same-file literal subs we can statically fold.
        if fname.named_child_count() != 0 {
            return vec![];
        }
        let Ok(name) = fname.utf8_text(self.source) else { return vec![] };
        if name.is_empty() {
            return vec![];
        }

        let mut root = call_node;
        while let Some(p) = root.parent() {
            root = p;
        }
        let Some(body) = self.find_local_sub_body(root, name) else { return vec![] };
        self.literal_list_return_names(body)
    }

    /// Find a top-level sub's `body` block by name within `root`'s subtree.
    /// First match wins (Perl's last-decl-wins is irrelevant for the literal
    /// folds this feeds — redefinition of a qw-returning helper is not a shape
    /// we model).
    pub(super) fn find_local_sub_body(&self, root: Node<'a>, name: &str) -> Option<Node<'a>> {
        let mut cursor = root.walk();
        let mut stack: Vec<Node<'a>> = root.children(&mut cursor).collect();
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "subroutine_declaration_statement" | "method_declaration_statement"
            ) {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if name_node.utf8_text(self.source).ok() == Some(name) {
                        return n.child_by_field_name("body");
                    }
                }
            }
            let mut c = n.walk();
            stack.extend(n.children(&mut c));
        }
        None
    }

    /// Extract the literal name list a sub body returns, when its tail/return
    /// expression is a literal list (`return qw(...)`, `return ('a','b')`, a
    /// bare trailing `qw(...)` / list). Empty if the body does anything beyond a
    /// literal list (computed, conditional, interpolated-with-unknown-vars).
    pub(super) fn literal_list_return_names(&self, body: Node<'a>) -> Vec<String> {
        // The return source is either an explicit `return EXPR` or the body's
        // tail expression statement. Probe each statement; a `return_expression`
        // anywhere is authoritative, else the last expression_statement.
        let mut return_expr: Option<Node<'a>> = None;
        let mut tail_expr: Option<Node<'a>> = None;
        for i in 0..body.named_child_count() {
            let Some(stmt) = body.named_child(i) else { continue };
            if stmt.kind() == "expression_statement" {
                if let Some(inner) = stmt.named_child(0) {
                    if inner.kind() == "return_expression" {
                        return_expr = Some(inner);
                    } else {
                        tail_expr = Some(inner);
                    }
                }
            }
        }
        let source = return_expr.or(tail_expr);
        let Some(source) = source else { return vec![] };
        let list_node = if source.kind() == "return_expression" {
            source.named_child(0)
        } else {
            Some(source)
        };
        let Some(list_node) = list_node else { return vec![] };
        match list_node.kind() {
            "quoted_word_list" | "list_expression" | "parenthesized_expression"
            | "string_literal" | "interpolated_string_literal" => {
                self.extract_string_names(list_node)
            }
            _ => vec![],
        }
    }

    /// Resolve a name to its known constant string values.
    /// Recurses through constant references up to max_depth.
    pub(super) fn resolve_constant_strings(&self, name: &str, depth: u8) -> Option<Vec<String>> {
        if depth > 3 { return None; }
        let values = self.constant_strings.get(name)?;
        let mut resolved = Vec::new();
        for val in values {
            if let Some(expanded) = self.resolve_constant_strings(val, depth + 1) {
                resolved.extend(expanded);
            } else {
                resolved.push(val.clone());
            }
        }
        Some(resolved)
    }

    /// Try to resolve an interpolated string to concrete value(s).
    /// Returns empty vec if any interpolated variable is unknown.
    /// Returns multiple values if a variable resolves to multiple strings (loop var).
    pub(super) fn try_fold_interpolated_string(&self, node: Node<'a>) -> Vec<String> {
        // Find the string_content child
        let content = match node.named_child(0) {
            Some(c) if c.kind() == "string_content" => c,
            _ => return vec![],
        };

        // Walk the string_content: split into literal text and interpolated variables.
        // Named children are the scalars; text between/around them is literal.
        let content_start = content.start_byte();
        let content_end = content.end_byte();
        let source_bytes = &self.source[content_start..content_end];

        let mut segments: Vec<Vec<String>> = Vec::new();
        let mut pos = 0usize; // position within source_bytes

        for i in 0..content.named_child_count() {
            if let Some(var_node) = content.named_child(i) {
                if var_node.kind() != "scalar" {
                    return vec![]; // complex interpolation, bail
                }
                // Literal text before this variable
                let var_start = var_node.start_byte() - content_start;
                if var_start > pos {
                    let literal = std::str::from_utf8(&source_bytes[pos..var_start]).unwrap_or("");
                    if !literal.is_empty() {
                        segments.push(vec![literal.to_string()]);
                    }
                }
                // Resolve the variable — canonicalize via the `varname`
                // child so the braced form (`${verb}`) keys the same
                // `$verb` as the bare `$verb`; the raw scalar text carries
                // the braces and would miss the loop/lexical binding.
                let key = match var_node.named_child(0).and_then(|v| v.utf8_text(self.source).ok()) {
                    Some(bare) if !bare.is_empty() => format!("${bare}"),
                    _ => return vec![],
                };
                match self.resolve_constant_strings(&key, 0) {
                    Some(values) if !values.is_empty() => segments.push(values),
                    _ => return vec![],
                }
                pos = var_node.end_byte() - content_start;
            }
        }
        // Literal text after last variable
        if pos < source_bytes.len() {
            let literal = std::str::from_utf8(&source_bytes[pos..]).unwrap_or("");
            if !literal.is_empty() {
                segments.push(vec![literal.to_string()]);
            }
        }

        if segments.is_empty() {
            return vec![];
        }
        // Cartesian product of all segments
        let mut result = vec![String::new()];
        for seg in segments {
            let mut next = Vec::new();
            for prefix in &result {
                for val in &seg {
                    next.push(format!("{}{}", prefix, val));
                }
            }
            result = next;
        }
        result
    }

    /// Extract `name => { -as => 'local' }` renames from a use statement's
    /// args. Returns `(local_name, remote_name)` pairs. Recognized by shape: a
    /// bareword/string `name` immediately followed by a hashref whose `-as`
    /// (or `as`) key names the local alias. Sub::Exporter / Exporter::Tiny /
    /// Exporter's `-as` all spell it this way, so one shape covers them.
    /// `(local_alias, remote_name, alias_span, remote_span)` for each
    /// `name => { -as => 'local' }` pair. The two spans are what make the
    /// renaming import navigable: `remote_span` (the `name` token) joins the
    /// SOURCE sub's rename; `alias_span` (the `-as` value) joins the local
    /// alias group (see `process_use`).
    pub(super) fn extract_as_renames(&self, node: Node<'a>) -> Vec<(String, String, Span, Span)> {
        let mut out = Vec::new();
        // The args live in a `list_expression` (multiple) or directly under the
        // use node. Walk every descendant list once, pairing a name token with
        // the following hashref.
        self.collect_as_renames(node, &mut None, &mut out);
        out
    }

    /// Recurse for `name => { -as => 'local' }` pairs. `pending_name` carries a
    /// just-seen bareword/string (+ its span) awaiting its hashref; when a
    /// hashref follows, its `-as` value becomes the local alias for that name.
    pub(super) fn collect_as_renames(
        &self,
        node: Node<'a>,
        pending_name: &mut Option<(String, Span)>,
        out: &mut Vec<(String, String, Span, Span)>,
    ) {
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { continue };
            match child.kind() {
                "anonymous_hash_expression" => {
                    if let Some((remote, remote_span)) = pending_name.take() {
                        if let Some((local, alias_span)) = self.extract_as_alias(child) {
                            out.push((local, remote, alias_span, remote_span));
                        }
                    }
                }
                "autoquoted_bareword" | "string_literal" | "bareword" => {
                    let text = child.utf8_text(self.source).unwrap_or("");
                    let name = text.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
                    // Skip option flags (`-as` appears at top level too in some
                    // forms) — only real names seed a pending rename.
                    if !name.is_empty() && !name.starts_with('-') {
                        // Content span for a quoted remote name; the whole token
                        // for a bareword (`beta`) — either way, just the name.
                        let span = if child.kind() == "string_literal" {
                            crate::cst::string_content_span(child)
                        } else {
                            node_to_span(child)
                        };
                        *pending_name = Some((name, span));
                    }
                }
                "list_expression" | "parenthesized_expression" => {
                    self.collect_as_renames(child, pending_name, out);
                }
                _ => {
                    *pending_name = None;
                }
            }
        }
    }

    /// Pull the local alias from a `{ -as => 'local' }` hashref: find the `-as`
    /// (or `as`) key and return its value as the local alias. Pairs positionally
    /// through the shared walker, so the plain-comma `{ '-as', 'local' }` spelling
    /// binds identically to the fat-comma form (`=>` is just an autoquoting comma).
    /// The key is matched on its *normalized text* (`-as` parses as a
    /// `unary_expression` wrapping `as` in the fat-comma form, a `string_literal`
    /// in the plain-comma form), never on a `=>` gate.
    pub(super) fn extract_as_alias(&self, hashref: Node<'a>) -> Option<(String, Span)> {
        let body = (0..hashref.named_child_count())
            .filter_map(|i| hashref.named_child(i))
            .find(|c| c.kind() == "list_expression")
            .unwrap_or(hashref);
        let children: Vec<Node<'a>> = (0..body.child_count())
            .filter_map(|i| body.child(i))
            .collect();
        let mut alias = None;
        self.for_each_pair_node_in_children(&children, |k_node, v_node| {
            let key = k_node
                .utf8_text(self.source)
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .trim_start_matches('-')
                .to_string();
            if key == "as" {
                let local = v_node
                    .utf8_text(self.source)
                    .unwrap_or("")
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .trim_start_matches('-')
                    .to_string();
                if !local.is_empty() {
                    // Content span (inside the quotes) so rename rewrites just
                    // the alias name, not `'renamed_beta'`.
                    alias = Some((local, crate::cst::string_content_span(v_node)));
                    return false;
                }
            }
            true
        });
        alias
    }

    /// Extract the import list from a use statement.
    /// Returns (imported_symbols, qw_close_paren_position).
    pub(super) fn extract_use_import_list(&self, node: Node<'a>) -> (Vec<String>, Option<Point>) {
        let qw_close = self.find_qw_close_position(node);
        let names = self.extract_string_names(node);
        if !names.is_empty() {
            return (names, qw_close);
        }
        (vec![], None)
    }

    /// Classify an assignment LHS as an export package-variable, tolerating a
    /// package qualifier. Perl exporters write either the lexical-to-package
    /// `our @EXPORT` form or the fully-qualified `@Bugzilla::Error::EXPORT`
    /// form (Bugzilla, many CPAN modules). Both name the *same* package global;
    /// the qualifier is just an explicit spelling. We strip `our `/`my ` and the
    /// sigil, then compare the trailing identifier after the last `::` against
    /// the export-variable basename — so `@EXPORT`, `@Pkg::EXPORT`,
    /// `%Pkg::EXPORT_TAGS` all match without a per-package branch (rule #10).
    pub(super) fn export_var_basename(lhs_text: &str) -> Option<&'static str> {
        let trimmed = lhs_text.trim();
        let stripped = trimmed
            .strip_prefix("our ")
            .or_else(|| trimmed.strip_prefix("my "))
            .unwrap_or(trimmed)
            .trim_start();
        let no_sigil = stripped
            .strip_prefix('@')
            .or_else(|| stripped.strip_prefix('%'))
            .unwrap_or(stripped);
        match crate::model::file_analysis::split_qualified(no_sigil).1 {
            "EXPORT_OK" => Some("@EXPORT_OK"),
            "EXPORT" => Some("@EXPORT"),
            "EXPORT_TAGS" => Some("%EXPORT_TAGS"),
            _ => None,
        }
    }

    /// The real RHS expression of an assignment. tree-sitter-perl maps the
    /// `right` field to the opening `(` paren when the rvalue is parenthesized
    /// (`$w *= (EXPR)`, `my %h = ( ... )`), so descending the field directly
    /// walks a bare token and skips the entire rvalue (this dropped refs for
    /// any constant/variable inside a parenthesized RHS). When the field is an
    /// unnamed token, fall back to the assignment's last named child that
    /// isn't the `left` field — the parser keeps the rvalue as a sibling.
    pub(super) fn assignment_rhs(&self, node: Node<'a>) -> Option<Node<'a>> {
        let field = node.child_by_field_name("right");
        if let Some(f) = field {
            if f.is_named() {
                return Some(f);
            }
        }
        let left_id = node.child_by_field_name("left").map(|l| l.id());
        let mut rhs = None;
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                if Some(c.id()) != left_id {
                    rhs = Some(c);
                }
            }
        }
        rhs
    }

    pub(super) fn visit_assignment(&mut self, node: Node<'a>) {
        // Check for @ISA assignment: `our @ISA = (...)`
        if let Some(left) = node.child_by_field_name("left") {
            let lhs_text = left.utf8_text(self.source).unwrap_or("");
            if lhs_text == "@ISA" || lhs_text.ends_with("@ISA") {
                if let Some(ref pkg) = self.current_package {
                    // child_by_field_name("right") returns `(` paren, not the list.
                    // Iterate named children to find list_expression/quoted_word_list.
                    let mut parents = Vec::new();
                    for i in 0..node.named_child_count() {
                        if let Some(child) = node.named_child(i) {
                            parents.extend(self.extract_string_names(child));
                        }
                    }
                    if !parents.is_empty() {
                        // @ISA = replaces (not appends)
                        self.package_parents.insert(pkg.clone(), parents);
                    }
                }
            }

            // Export package-variable assignment — `our @EXPORT`, the qualified
            // `@Pkg::EXPORT`, `%EXPORT_TAGS`, etc. `export_var_basename` strips
            // the `our`/`my`, sigil, and any `Pkg::` qualifier so all spellings
            // map to one of the three export basenames (rule #10).
            let export_var = Self::export_var_basename(lhs_text);

            // Fold `%EXPORT_TAGS = ( tag => [names...], ... )` membership into
            // the export surface. The RHS list is the table literal; tag
            // members join `export_ok` like `@EXPORT_OK` names. The
            // call-wrapped `Readonly::Hash %EXPORT_TAGS => (...)` form is an
            // `ambiguous_function_call_expression`, handled in `visit_function_call`.
            if export_var == Some("%EXPORT_TAGS") {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        if child.kind() == "list_expression"
                            || child.kind() == "parenthesized_expression"
                        {
                            self.fold_export_tags_table(child);
                        }
                    }
                }
            }

            // Accumulate @EXPORT / @EXPORT_OK assignments
            let var_name = match export_var {
                Some("@EXPORT_OK") => Some("@EXPORT_OK"),
                Some("@EXPORT") => Some("@EXPORT"),
                _ => None,
            };
            if let Some(export_var) = var_name {
                let mut names = Vec::new();
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        names.extend(self.extract_string_names(child));
                        self.record_export_member_sites(child);
                        // Form 1: `@EXPORT = ( 'own', @Other::EXPORT )` — any
                        // qualified export-var deref in the RHS is a re-export edge.
                        self.record_static_splice_reexports(child);
                    }
                }
                // Union, not clobber: runtime-exporter discovery (`:Export`
                // attrs, Sub::Exporter / Exporter::Declare setup) may have
                // recorded names earlier in this package walk. An overwrite
                // would drop them; the two mechanisms compose.
                let target = if export_var == "@EXPORT" {
                    &mut self.export
                } else {
                    &mut self.export_ok
                };
                for name in names {
                    if !target.contains(&name) {
                        target.push(name);
                    }
                }
            }

            // Glob assignment inside sub import: *{"$caller::name"} = \&name
            // Detect custom import() that exports via typeglob manipulation.
            if left.kind() == "glob" {
                if self.enclosing_sub_name().as_deref() == Some("import") {
                    for name in self.extract_glob_export_names(left, node) {
                        if !self.export.contains(&name) {
                            self.export.push(name);
                        }
                    }
                }
                // Typeglob sub installation: `*name = sub {...}`, `*name = \&other`,
                // `*{ 'literal' } = $coderef`, loop `*$m = sub {...}`. The builder
                // would otherwise never register these as symbols, so every call site
                // flags unresolved. Recognize by shape (rule #10): synthesize only
                // when the RHS produces a sub/coderef AND the LHS name is statically
                // derivable. Truly-dynamic names (`*{$runtime}`) stay out of scope.
                self.synthesize_glob_assigned_sub(left, node);
            }

            // Accumulate array/scalar assignments as constants
            {
                // Strip leading "our " or "my " to get the variable name
                let var_stripped = if lhs_text.starts_with("our ") {
                    &lhs_text[4..]
                } else if lhs_text.starts_with("my ") {
                    &lhs_text[3..]
                } else {
                    lhs_text
                };
                if var_stripped.starts_with('@') {
                    let mut values = Vec::new();
                    for i in 0..node.named_child_count() {
                        if let Some(child) = node.named_child(i) {
                            values.extend(self.extract_string_names(child));
                        }
                    }
                    if !values.is_empty() {
                        self.constant_strings.insert(var_stripped.to_string(), values);
                    }
                } else if var_stripped.starts_with('$') {
                    let var = var_stripped;
                    if let Some(right) = node.child_by_field_name("right") {
                        if right.kind() == "interpolated_string_literal" {
                            // Try interpolated string folding first (has variable refs)
                            let folded = self.try_fold_interpolated_string(right);
                            if !folded.is_empty() {
                                self.constant_strings.insert(var.to_string(), folded);
                            }
                        } else if right.kind() == "string_literal" {
                            // Plain string literal
                            if let Some(text) = self.extract_string_content(right) {
                                self.constant_strings.insert(var.to_string(), vec![text]);
                                self.constant_string_source
                                    .insert(var.to_string(), self.string_content_span(right));
                            }
                        }
                    }
                }
            }
        }

        // Check for type inference from RHS
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "variable_declaration" {
                // Visit the declaration
                self.visit_variable_decl(left);
            }
            if let Some(right) = self.assignment_rhs(node) {
                // Visit RHS children FIRST so any side-effecting walk
                // steps (anon-sub Symbol creation in
                // `visit_anonymous_sub`, ref/scope allocation, plugin
                // hook firing) have run by the time we read the
                // rvalue's `InferredType`. Specifically:
                // `my $cb = sub {...}` needs `coderef_return_edge_for`
                // to see the (anon) Symbol already in
                // `anon_sub_symbol_by_span`, so the TC for `$cb`
                // carries `CodeRef { return_edge: Symbol(_) }` —
                // uniform attachment shape with named subs, so
                // `ReturnExprReducer` claims arity-arm and Receiver-
                // substitution witnesses through the Symbol
                // attachment without a per-shape dispatch in the
                // chase site.
                self.queue_node_then(right, move |b| b.assignment_after_rhs(node, left, right));
            } else if left.kind() != "variable_declaration" {
                // No RHS to read a type from, but the LHS still owes its refs.
                self.queue_node(left);
            }
        } else {
            // No left field — just visit children
            self.queue_children(node);
        }
    }

    /// The half of `visit_assignment` that can only run once the RHS subtree
    /// is walked: read the rvalue's type, seed constraints and key writes
    /// from it, then descend the LHS.
    ///
    /// Split out because the shape is descend → work → descend, which the
    /// `queue_children_then` combinator deliberately cannot express. The type
    /// read is only valid after the RHS walk has allocated its refs and
    /// anon-sub symbols.
    fn assignment_after_rhs(&mut self, node: Node<'a>, left: Node<'a>, right: Node<'a>) {
        // Push the RHS's Expr(span) witness so the bag is
        // canonical for this expression, then query for the
        // resolved type. `emit_expr_witness` covers every
        // shape via `expr_payload` — literals, anon-subs,
        // constructor patterns, binary ops, scalars, calls,
        // ternaries; Edge payloads resolve through the
        // registry's materialization.
        self.emit_expr_witness(right);
        let mut inferred = self.bag_query_expr_span(node_to_span(right));
        // `my %h = (k => v, …)` — the list IS a hash literal in
        // this position, the hashref's second spelling. The
        // list's own Expr witness can't carry that (its meaning
        // depends on the LHS sigil), so type it from the LHS
        // side through the same shape builder.
        if inferred.is_none() && right.kind() == "list_expression" {
            if let Some(vt) = self.get_var_text_from_lhs(left) {
                if vt.starts_with('%') {
                    inferred = Some(self.hash_literal_type(right));
                } else if vt.starts_with('@') {
                    inferred = self.list_literal_type(right);
                }
            }
        }
        if self.lhs_list_targets(left).is_some() {
            // List/destructuring assignment is minted + typed by the
            // declarative `@flow` query pass (`mint_flow_edges_via_query`),
            // which reuses the same `lhs_list_targets`/`list_element_nodes`
            // pairing. This guard just keeps the eager arm below from
            // mis-typing a list's first var (`get_var_text_from_lhs`
            // returns only `$a`) as the whole RHS.
        } else if let Some(it) = inferred {
            if let Some(vt) = self.get_var_text_from_lhs(left) {
                // `my $self = bless {}, $class` — the eager TC below
                // bakes the RHS's MATERIALIZED type (the no-receiver
                // fallback, i.e. the enclosing class); the deferred
                // witness keeps the ctor receiver-polymorphic at
                // call sites (wins via reducer order).
                self.push_receiver_bless_witness(&vt, right);
                self.push_type_constraint(TypeConstraint {
                    variable: vt,
                    scope: self.current_scope(),
                    constraint_span: node_to_span(node),
                    inferred_type: it,
                });
            }
        }
        // The unresolved single-var case (a cross-file chain that
        // didn't type at walk time) is now minted + lowered by the
        // declarative `@flow` query pass (`mint_flow_edges_via_query`),
        // as a fallback that doesn't override the eager TC above.
        // Always record call/method-call bindings (independent
        // of whether the bag resolved a type) — they're the
        // source-sub linkage that hash-key ownership fixup
        // walks. Pre-Step-4, the bag's call resolution wasn't
        // available at walk time so `inferred` was None for
        // function calls and the binding fell out of the
        // `else if` branch; with the implicit-return edge
        // routing through SymbolReturnArm chains, the type
        // surfaces but the binding still has to fire.
        if let Some(func_name) = self.extract_call_name(right) {
            if let Some(vt) = self.get_var_text_from_lhs(left) {
                self.call_bindings.push(CallBinding {
                    variable: vt,
                    func_name,
                    scope: self.current_scope(),
                    span: node_to_span(node),
                });
            }
        } else if right.kind() == "method_call_expression" {
            // RHS is a method call — record binding for return-type post-pass
            if let Some(method_node) = right.child_by_field_name("method") {
                if let Some(invocant_node) = right.child_by_field_name("invocant") {
                    if let (Ok(method), Ok(inv)) = (
                        method_node.utf8_text(self.source),
                        invocant_node.utf8_text(self.source),
                    ) {
                        // Skip constructors — already handled by extract_constructor_class
                        if !crate::model::conventions::is_constructor_name(method) {
                            if let Some(vt) = self.get_var_text_from_lhs(left) {
                                // Resolve dynamic method names via constant folding
                                let method_names = if method.starts_with('$') {
                                    self.resolve_constant_strings(method, 0)
                                        .unwrap_or_else(|| vec![method.to_string()])
                                } else {
                                    vec![method.to_string()]
                                };
                                for mname in method_names {
                                    self.method_call_bindings.push(MethodCallBinding {
                                        variable: vt.clone(),
                                        invocant_var: inv.to_string(),
                                        method_name: mname,
                                        scope: self.current_scope(),
                                        span: node_to_span(node),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        // Branch-arm detection: if RHS is a ternary, emit
        // per-arm `branch_arm`-source `Edge(Expr(arm_span))`
        // witnesses on the LHS variable, plus the arms' own
        // Expr(span) payloads. POST visit_node — needs the
        // arms' refs to exist so `expr_payload` can resolve
        // `Edge(Expression(refidx))` for method-call arms.
        if right.kind() == "conditional_expression" {
            if let Some(vt) = self.get_var_text_from_lhs(left) {
                self.emit_branch_arm_witnesses_for_ternary(&vt, right, node);
            }
        }
        // `$obj->{k} = <rhs>` slot-type seed. Record key-span →
        // rhs-span so `populate_witness_bag` can mint
        // `SlotType{owner_class, k} → Edge(Expr(rhs_span))`
        // keyed off the matching HashKeyAccess Write ref (whose
        // span is this key node's span). `emit_expr_witness(right)`
        // already published the RHS's `Expr(rhs_span)`.
        if left.kind() == "hash_element_expression" {
            if let Some(key_node) = left.child_by_field_name("key") {
                self.slot_write_rhs_span
                    .insert(node_to_span(key_node), node_to_span(right));
            }
        }
        if matches!(
            left.kind(),
            "hash_element_expression" | "array_element_expression"
        ) {
            self.record_key_write(left, Some(right));
        }
        // Slice / keyval writes (`@h{qw(a b)} = …`, `%h{k} = …`,
        // `@$h{…}`) land several keys at once — record an
        // open-switching write (`key: None`) on the container so
        // a closed shape can't claim the slice-written keys as
        // misses.
        if matches!(left.kind(), "slice_expression" | "keyval_expression") {
            {
                // Three container spellings: sigil (`@h{…}` →
                // canonical `%h`), sigil-deref (`@$h{…}` — the
                // varname wraps the scalar; canonical would mint
                // a garbage `%$h`, so the inner scalar wins),
                // and postfix deref (`$h->@{…}` / `$h->%{…}` —
                // grammar field `hashref:`, a plain scalar).
                let name = match left.child_by_field_name("hash") {
                    Some(container) => {
                        crate::cst::varname_inner_scalar_text(container, self.source)
                            .or_else(|| {
                                crate::cst::canonical_container_name(
                                    container,
                                    self.source,
                                )
                            })
                    }
                    None => left
                        .child_by_field_name("hashref")
                        .filter(|c| c.kind() == "scalar")
                        .and_then(|c| c.utf8_text(self.source).ok())
                        .map(|s| s.to_string()),
                };
                if let Some(var_text) = name {
                    let span = node_to_span(left);
                    self.key_writes.push(crate::model::file_analysis::KeyWrite {
                        var_text,
                        key: crate::model::file_analysis::WriteKey::Unknown,
                        scope: self
                            .scope_stack
                            .last()
                            .copied()
                            .unwrap_or(crate::model::file_analysis::ScopeId(0)),
                        span,
                        rhs_span: None,
                        conditional: true,
                    });
                }
            }
        }
        // Visit LHS children (except the variable_declaration we already handled)
        if left.kind() != "variable_declaration" {
            self.queue_node(left);
        }
    }

    /// Find a local Sub or Method symbol by bare name. Used by
    /// `expr_payload`'s function/bareword arms — both at walk
    /// time and post-walk via `resolve_forward_expr_witnesses`'s
    /// retry. One lookup, two emission paths, byte-identical
    /// witnesses.
    pub(super) fn find_callee_symbol(&self, name: &str) -> Option<SymbolId> {
        let (qualifier, bare) = crate::model::file_analysis::split_qualified(name);
        self.symbols
            .iter()
            .find(|s| {
                s.name == bare
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && qualifier_admits_local_sub(qualifier, s.package.as_deref())
            })
            .map(|s| s.id)
    }

    /// The bare name to look a call up by, for callers that resolve through a
    /// name-keyed query rather than a symbol id — `None` when the call's
    /// qualifier rules out every sub this file declares.
    ///
    /// The name-keyed queries can only be asked about a bare name, so the
    /// qualifier has to be honoured *before* the question is put to them.
    /// Same predicate as `find_callee_symbol`, so the two cannot drift into
    /// disagreeing about whether `Foo::bar()` is local.
    pub(super) fn local_callee_name<'n>(&self, name: &'n str) -> Option<&'n str> {
        let (qualifier, bare) = crate::model::file_analysis::split_qualified(name);
        if qualifier.is_none() {
            return Some(bare);
        }
        self.symbols
            .iter()
            .any(|s| {
                s.name == bare
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && qualifier_admits_local_sub(qualifier, s.package.as_deref())
            })
            .then_some(bare)
    }

    /// Locate the SymbolId for a Sub/Method named `name` whose body's
    /// inner scope is (an ancestor of) `body_scope`. Scans
    /// `self.symbols` for a matching Sub symbol.
    pub(super) fn find_sub_symbol_for(&self, name: &str, body_scope: ScopeId) -> Option<SymbolId> {
        // Walk up the scope chain; the Sub scope's own span contains
        // the return. We need the symbol whose selection_span matches
        // the sub name.
        let mut cursor = Some(body_scope);
        while let Some(sid) = cursor {
            let s = &self.scopes[sid.0 as usize];
            if let ScopeKind::Sub { name: n } | ScopeKind::Method { name: n } = &s.kind {
                if n == name {
                    // Find the symbol whose enclosing-scope parent
                    // declared it (sym.scope == s.parent) OR the
                    // symbol's scope equals this one.
                    for sym in &self.symbols {
                        if sym.name == name
                            && matches!(sym.kind, SymKind::Sub | SymKind::Method)
                            && sym.span.start <= s.span.start
                            && s.span.end <= sym.span.end
                        {
                            return Some(sym.id);
                        }
                    }
                    return None;
                }
            }
            cursor = s.parent;
        }
        None
    }

    /// Find the Sub/Method symbol whose body scope IS or CONTAINS
    /// `inner_scope`. Same semantics as `find_sub_symbol_for(name, ...)`
    /// without needing the name in hand. Used by
    /// `emit_arity_return_witnesses` to key per-scope
    /// `ReturnExpr::UnionOnArgs` witnesses by sub.
    pub(super) fn find_sub_symbol_for_scope(&self, inner_scope: ScopeId) -> Option<SymbolId> {
        let mut cursor = Some(inner_scope);
        while let Some(sid) = cursor {
            let s = &self.scopes[sid.0 as usize];
            if let ScopeKind::Sub { name } | ScopeKind::Method { name } = &s.kind {
                for sym in &self.symbols {
                    if sym.name == *name
                        && matches!(sym.kind, SymKind::Sub | SymKind::Method)
                        && sym.span.start <= s.span.start
                        && s.span.end <= sym.span.end
                    {
                        return Some(sym.id);
                    }
                }
                return None;
            }
            cursor = s.parent;
        }
        None
    }

    /// Resolve plugin-queued `NamedSubParamType` requests once the whole CST
    /// is walked. For each `(sub_name, package, param_index)` find the
    /// matching local Sub/Method symbol, read its `param_index`-th positional
    /// variable name, locate that sub's scope, and push the requested TC —
    /// the same `push_type_constraint` path the inline `VarType` form uses,
    /// just keyed by name instead of an anchor span. Forward-declared subs
    /// resolve because every symbol + scope exists by now.
    pub(super) fn flush_deferred_named_sub_param_types(&mut self) {
        let deferred = std::mem::take(&mut self.deferred_named_sub_param_types);
        for d in deferred {
            // Pick the sub symbol matching name + (when known) package, so a
            // qualified `\&Foo::bar` only types a sub that actually lives in
            // `Foo`. A bare name resolved to a package at emit time (the
            // registration's enclosing package); match that too.
            let target = self.symbols.iter().find(|sym| {
                sym.name == d.sub_name
                    && matches!(sym.kind, SymKind::Sub | SymKind::Method)
                    && match &d.package {
                        Some(pkg) => sym.package.as_deref() == Some(pkg.as_str()),
                        None => true,
                    }
            });
            let target = match target {
                Some(t) => t,
                None => continue,
            };
            let params = match &target.detail {
                SymbolDetail::Sub { params, .. } => params,
                _ => continue,
            };
            let var_name = match params.get(d.param_index) {
                Some(p) if p.name.starts_with('$') => p.name.clone(),
                _ => continue,
            };
            let sub_span = target.span;
            // The sub's body scope: a Sub/Method scope whose span matches the
            // declaration span. `record_signature_params` / `my $c = shift`
            // both put the param variable in this scope.
            let scope = self
                .scopes
                .iter()
                .find(|s| {
                    matches!(&s.kind, ScopeKind::Sub { name } | ScopeKind::Method { name } if *name == d.sub_name)
                        && s.span == sub_span
                })
                .map(|s| s.id);
            let scope = match scope {
                Some(s) => s,
                None => continue,
            };
            self.push_plugin_type_constraint(
                TypeConstraint {
                    variable: var_name,
                    scope,
                    constraint_span: sub_span,
                    inferred_type: d.inferred_type.clone(),
                },
                d.plugin_id.clone(),
            );
        }
    }
}

/// Whether a call written with `qualifier` may bind to a sub this file
/// declares in `sym_package`.
///
/// The qualifier is not decoration: `Data::Dumper::stat()` and `stat()` name
/// different subs, and `CORE::stat()` names the builtin *precisely because*
/// something else called `stat` is in scope. Dropping it made every qualified
/// call bind to any same-named local sub, which inverts the one thing the
/// author wrote the qualifier to say.
///
/// An unqualified call keeps the plain name match. A qualified one has to name
/// the declaring package — with `main` (and its bare `::foo` shorthand)
/// covering subs in a file that declares no package at all. Nothing here knows
/// about `CORE`: it names no package this file declares, so it falls out.
fn qualifier_admits_local_sub(qualifier: Option<&str>, sym_package: Option<&str>) -> bool {
    let Some(qualifier) = qualifier else {
        return true;
    };
    let qualifier = if qualifier.is_empty() { "main" } else { qualifier };
    match sym_package {
        Some(pkg) => pkg == qualifier,
        None => qualifier == "main",
    }
}

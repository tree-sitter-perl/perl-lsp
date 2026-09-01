//! Variable-ref and function-call visitors plus runtime exporter
//! modeling (`push @EXPORT*`, glob installs, eval-replacement refs).

use super::*;

impl<'a> Builder<'a> {
    pub(super) fn visit_var_ref(&mut self, node: Node<'a>) {
        // Skip if parent is a variable_declaration (handled by visit_variable_decl)
        if let Some(parent) = node.parent() {
            if parent.kind() == "variable_declaration" { return; }
            // Skip if inside a signature param
            if matches!(parent.kind(), "mandatory_parameter" | "optional_parameter" | "slurpy_parameter") {
                return;
            }
        }

        // Check for block-based dereference: @{expr}, %{expr}, &{expr}
        // In tree-sitter-perl these parse as array/hash/function with a varname
        // child containing a block. The block holds the real expressions.
        // Don't record a variable ref for the outer — just recurse into the block.
        if self.is_block_deref(node) {
            // Recurse into the block to visit inner expressions
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "varname" {
                        for j in 0..child.child_count() {
                            if let Some(gc) = child.child(j) {
                                if gc.kind() == "block" {
                                    self.queue_children(gc);
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            return;
        }

        // Interpolation deref: `${ EXPR }` inside a string or regex
        // parses as `scalar > block` directly — no varname wrapper, so
        // `is_block_deref` doesn't claim it. The block holds real code
        // (`"_${\ $self->filetype }_"` carries a method call); visit it
        // so its refs land, and emit nothing for the outer node — its
        // text isn't a variable name.
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "block" {
                    self.queue_children(child);
                    return;
                }
            }
        }

        // Sigil-deref of a scalar: `%$x` / `@$x` / `$$x` parses as the outer
        // sigil node (hash/array/scalar) whose varname child wraps an inner
        // `scalar` node naming the dereferenced variable. The inner scalar is
        // the renamable/highlightable token (rule #7) — emit its Variable ref
        // and stop, since the outer node's own text (`%$x`) isn't a variable
        // name. Block-derefs (`%{...}`) are handled above.
        if let Some(inner) = self.sigil_deref_inner_scalar(node) {
            self.queue_node(inner);
            return;
        }

        // If this scalar is inside a block-deref, infer the type from the outer sigil.
        // Parent chain: scalar → expression_statement → block → varname → outer_node
        if node.kind() == "scalar" {
            if let Some((deref_type, context)) = self.block_deref_context(node) {
                self.push_var_type_constraint(node, context, deref_type);
            }
        }

        if let Ok(text) = node.utf8_text(self.source) {
            let access = self.determine_access(node);
            // A scalar READ anywhere but an element-access base hands the
            // reference to code that may mutate the referent (call arg,
            // alias, invocant, deref). An escape IS an unknown-key write:
            // record it as an open-switching KeyWrite at the escape span,
            // and the mutation-extension pass widens the shape from that
            // point on — temporal, so reads BEFORE the first escape keep
            // their closed shape. One site per var suffices.
            if access == AccessKind::Read
                && text.starts_with('$')
                && !crate::cst::is_element_access_base(node)
                && !self.escape_recorded.contains(text)
            {
                self.escape_recorded.insert(text.to_string());
                self.record_escape_write(text.to_string(), node);
            }
            // Write with the variable itself as assignment target =
            // reassignment. An element write (`$v->{k} = …`, where this
            // scalar is the access base and the Write came from the
            // grandparent rule) is a shape mutation instead — modeled by
            // the mutation-extension pass, not a trust break.
            if access == AccessKind::Write
                && (text.starts_with('$') || text.starts_with('%'))
                && !crate::cst::is_element_access_base(node)
            {
                self.reassigned_scalars.insert(text.to_string());
            }
            // Hash variables escape only by reference-taking (`\%h`):
            // a bare `%h` in a call or list FLATTENS TO COPIES — the
            // callee can't add keys to the original, so it's not an
            // escape (unlike a scalar, whose value IS the shared ref).
            if text.starts_with('%')
                && node.parent().is_some_and(|p| p.kind() == "refgen_expression")
                && !self.escape_recorded.contains(text)
            {
                self.escape_recorded.insert(text.to_string());
                self.record_escape_write(text.to_string(), node);
            }
            // Fully-qualified read (`$Foo::Bar::x`): narrow the span to the
            // bare tail (rule #7) so rename rewrites only `x` and the
            // qualifier survives, mirroring the FQ-call narrowing. The full
            // `target_name` keeps the path; `qualified_var_target()` decodes
            // `(pkg, sigil+basename)` for cross-package resolution.
            let ref_span = fq_tail_span(node, text);
            self.add_ref(
                RefKind::Variable,
                ref_span,
                text.to_string(),
                access,
            );
        }
    }

    /// Sigil-deref of a scalar (`%$x` / `@$x` / `$$x`): the outer sigil node
    /// (hash/array/scalar) wraps `(varname (scalar (varname)))`. Returns the
    /// inner `scalar` node — the dereferenced variable — so its own Variable
    /// ref is emitted. Excludes the plain non-deref case (a `scalar` whose
    /// varname holds only a leaf varname) and block-derefs (handled separately).
    pub(super) fn sigil_deref_inner_scalar(&self, node: Node<'a>) -> Option<Node<'a>> {
        if !matches!(node.kind(), "scalar" | "array" | "hash") {
            return None;
        }
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            if child.kind() != "varname" {
                continue;
            }
            for j in 0..child.child_count() {
                let gc = child.child(j)?;
                if gc.kind() == "scalar" {
                    return Some(gc);
                }
            }
        }
        None
    }

    /// Check if this node is a block-deref outer node (e.g. @{...} or %{...}).
    pub(super) fn is_block_deref(&self, node: Node<'a>) -> bool {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "varname" {
                    for j in 0..child.child_count() {
                        if let Some(gc) = child.child(j) {
                            if gc.kind() == "block" {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Walk up from a scalar to detect block-deref context.
    /// Returns the inferred type and a context node (for constraint span) if inside
    /// @{$x}, %{$y}, or &{$z}.
    ///
    /// Parent chain: scalar → expression_statement → block → varname → outer_node
    /// where outer_node.kind() is "array" (@{}), "hash" (%{}), or "function" (&{}).
    pub(super) fn block_deref_context(&self, node: Node<'a>) -> Option<(InferredType, Node<'a>)> {
        let stmt = node.parent()?;
        if stmt.kind() != "expression_statement" { return None; }
        let block = stmt.parent()?;
        if block.kind() != "block" { return None; }
        let varname = block.parent()?;
        if varname.kind() != "varname" { return None; }
        let outer = varname.parent()?;
        match outer.kind() {
            "array" => Some((InferredType::ArrayRef, outer)),
            "hash" => Some((InferredType::HashRef, outer)),
            "function" => {
                if let Ok(text) = outer.utf8_text(self.source) {
                    if text.starts_with("&{") {
                        return Some((InferredType::CodeRef { return_edge: None }, outer));
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub(super) fn visit_container_ref(&mut self, node: Node<'a>) {
        // Container variables: $hash{key}, @arr[0], etc.
        // The container itself is a ref to the underlying variable
        if let Ok(text) = node.utf8_text(self.source) {
            // Map container sigil to the declared sigil
            let canonical = self.canonicalize_container(node, text);
            let access = self.determine_access(node);
            self.add_ref(
                RefKind::ContainerAccess,
                node_to_span(node),
                canonical,
                access,
            );
        }
    }

    /// `$#foo` — arraylen. TSP gives us a distinct `arraylen` node
    /// with a `varname` child; the access resolves to `@foo`. We emit
    /// a ContainerAccess ref so goto-def and rename treat it like
    /// any other indirect access into the array.
    pub(super) fn visit_arraylen_ref(&mut self, node: Node<'a>) {
        let bare = match find_varname_child(node).and_then(|v| v.utf8_text(self.source).ok()) {
            Some(b) => b,
            None => return,
        };
        let access = self.determine_access(node);
        self.add_ref(
            RefKind::ContainerAccess,
            node_to_span(node),
            format!("@{}", bare),
            access,
        );
    }

    pub(super) fn visit_function_call(&mut self, node: Node<'a>) {
        // Indirect-object filehandle: `print FH LIST` / `say FH ...` /
        // `printf FH ...` parses as the print verb whose `arguments` is a
        // nested call with `function` = the bareword filehandle. The
        // bareword is a filehandle, not a sub — don't emit a FunctionCall
        // ref for it (otherwise every `print STDERR ...` flags STDERR as
        // unresolved). Visit the real payload args; skip the verb-as-func.
        if self.is_indirect_object_filehandle_call(node) {
            self.queue_children(node);
            return;
        }
        // Symbolic-code-deref call: `&{ EXPR }(...)`. The `function` field is
        // a `function` wrapping a `code_deref_expression`, NOT a sub name —
        // its text is `&{$z}`, not a callable identifier. Emitting a
        // FunctionCall ref here would flag the deref text as an unresolved
        // sub. The `code_deref_expression` arm in `visit_node` narrows the
        // operand to CodeRef; just descend so it runs.
        if node
            .child_by_field_name("function")
            .map(|f| f.kind() == "code_deref_expression" || code_deref_in(f).is_some())
            .unwrap_or(false)
        {
            self.queue_children(node);
            return;
        }
        if let Some(func_node) = node.child_by_field_name("function") {
            if let Ok(raw) = func_node.utf8_text(self.source) {
                // `goto &Foo::bar` / `&Foo::bar()` — the leading `&` is the
                // code-ref sigil; the call targets sub `Foo::bar`. Strip it so
                // FQ resolution and the ref's `target_name` see the real sub
                // name (`&{...}` derefs and `&$var` are handled / skipped
                // above — only a static `&name` reaches here). `raw` keeps the
                // node-aligned text for span math.
                let name = raw
                    .strip_prefix('&')
                    .filter(|n| n.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':'))
                    .unwrap_or(raw);
                let resolved_package = self.resolve_call_package(name);
                // Narrowest span (rule #7): for a qualified call
                // (`Foo::Bar::baz()`) the renamable/highlightable token is
                // the bare tail, not the whole `::` path — so rename rewrites
                // only `baz` and the qualifier survives. `target_name` keeps
                // the full path (the hash-key binding + provenance rely on
                // it); resolution sites read `unqualified_target_name()`.
                let ref_span = fq_tail_span(func_node, raw);
                self.add_bound_ref(
                    RefKind::FunctionCall,
                    ref_span,
                    name.to_string(),
                    AccessKind::Read,
                    resolved_package
                        .clone()
                        .map(|package| RefBinding::Function { package }),
                );
                // Even-position stringy args → HashKeyAccess refs
                // owned by `Sub{resolved_package, name}`. Same
                // mechanism as method-call args; covers
                // `Foo::new(key => val)` and helpers like
                // `connect(timeout => 30)`.
                if let Some(args) = node.child_by_field_name("arguments") {
                    let owner = HashKeyOwner::Sub {
                        package: resolved_package.clone(),
                        name: name.to_string(),
                    };
                    self.emit_call_arg_key_accesses(args, Gate::Strict(owner));
                }
                // Push type constraints on arguments of known builtins
                if let Some(arg_type) = crate::model::builtins::builtin_first_arg_type(name) {
                    if let Some(first_arg) = self.first_call_arg(node) {
                        self.push_var_type_constraint(first_arg, node, arg_type);
                    }
                }
                // `bless $self, $class` promotes $self to ClassName($class)
                // so post-bless `$self->method` resolves (H4).
                if name == "bless" {
                    self.visit_bless_call(node);
                }
                // Framework accessor synthesis: `has` calls in Moo/Moose/Mojo::Base packages
                if name == "has" {
                    if let Some(mode) = self.current_package.as_ref()
                        .and_then(|pkg| self.framework_modes.get(pkg).copied())
                    {
                        self.visit_has_call(node, mode);
                    }
                }
                // `option` (MooX::Options) is a `has` with extra option-parsing
                // keys (format/doc/...). Synthesis is identical, so route it
                // through the same path in the package's framework mode. Gated
                // on the package actually importing `option` (per the
                // framework-mode manifest) so an unrelated `option(...)` sub
                // elsewhere isn't read as an attribute declaration.
                if name == "option" && self.package_imports_framework_keyword("option") {
                    if let Some(mode) = self.current_package.as_ref()
                        .and_then(|pkg| self.framework_modes.get(pkg).copied())
                    {
                        self.visit_has_call(node, mode);
                    }
                }
                // Moose/Moo `extends 'Parent'` — register parent classes
                if name == "extends" {
                    if let Some(pkg) = self.current_package.clone() {
                        if self.framework_modes.contains_key(&pkg) {
                            self.visit_extends_call(node, &pkg);
                        }
                    }
                }
                // Moo/Moose role `requires NAMES` — declared method
                // contracts the composer must fulfill. Gated on the
                // role sugar actually being in scope (a plain class
                // never imports `requires`) so a user-defined sub of
                // the same name doesn't synthesize bogus methods.
                if name == "requires" && self.framework_imports.contains("requires") {
                    if let Some(pkg) = self.current_package.clone() {
                        if self.framework_modes.contains_key(&pkg) {
                            self.visit_requires_call(node, &pkg);
                        }
                    }
                }
                // Moose/Moo `with 'Role'` — register roles as parents for method resolution
                if name == "with" {
                    if let Some(pkg) = self.current_package.clone() {
                        if self.framework_modes.contains_key(&pkg) {
                            self.visit_extends_call(node, &pkg);
                        }
                    }
                }
                // push @EXPORT, 'foo', 'bar' / push @EXPORT_OK, 'foo'
                if name == "push" {
                    self.visit_push_call(node);
                }

                // Call-wrapped `%EXPORT_TAGS` table:
                // `Readonly::Hash our %EXPORT_TAGS => ( tag => [...], ... )`
                // (and Const::Fast / any wrapper). The args are
                // `<%EXPORT_TAGS declaration> => <table list>`; we recognize the
                // declared variable, not the wrapper function (rule #10), and
                // fold the trailing table list the same as a plain assignment.
                self.fold_call_wrapped_export_tags(node);

                // Moose/Moo method modifiers: `around`/`before`/`after foo => sub {...}`.
                // The anonymous sub body is a method body — its invocant parameter needs to
                // be typed as the enclosing class. `around` takes `($orig, $self, @args)`,
                // so the invocant is at position 1; `before`/`after` take `($self, @args)`,
                // position 0. Set `modifier_invocant_pos` so `visit_anonymous_sub` (called
                // when `visit_children` descends into the sub arg) marks the right param.
                // Only applies in Moo/Moose contexts; other frameworks/plain-Perl `around`
                // calls aren't affected because `framework_modes` won't contain the package.
                let is_modifier = matches!(name, "around" | "before" | "after");
                if is_modifier {
                    if let Some(pkg) = self.current_package.as_ref() {
                        if matches!(
                            self.framework_modes.get(pkg),
                            Some(FrameworkMode::Moo | FrameworkMode::Moose)
                        ) {
                            self.modifier_invocant_pos = Some(if name == "around" { 1 } else { 0 });
                        }
                    }
                }

                // Runtime-exporter setup in function-call form:
                // `Sub::Exporter::setup_exporter({ exports => [...] })`.
                // Match on the unqualified tail so the package prefix
                // (which the caller may have aliased) isn't load-bearing.
                let tail = crate::model::file_analysis::split_qualified(name).1;
                if tail == "setup_exporter" {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        self.detect_exporter_setup_call(tail, args);
                    }
                }

                // Runtime-exporter declaration calls (recognizing literal
                // setup syntax, same idiom as the `has`/`extends` branches
                // above — builder parsing of framework syntax, not
                // consumer-side shape-branching). Exporter::Extensible's
                // `export(...)` and Exporter::Declare's `exports(...)` take
                // a flat name list; Exporter::Declare's `export NAME => sub`
                // and `default_export NAME => sub` take a name + coderef.
                // Gated on the package having `use`d the relevant exporter
                // so an unrelated `sub export {}` in some other module isn't
                // mistaken for a declaration.
                if matches!(name, "export" | "exports" | "default_export")
                    && self.package_uses_exporter_declare_family()
                {
                    match name {
                        "exports" => self.detect_export_name_list_call(node),
                        // `export(qw/.../)` (Extensible, parens + qw) vs
                        // `export NAME => sub` (Declare, fat-comma pair):
                        // the qw / pure-name-list form has no fat comma, the
                        // pair form's second arg is a coderef. Try the pair
                        // form first; fall back to the list form.
                        "export" => {
                            if self.is_export_pair_call(node) {
                                self.detect_export_pair_call(node);
                            } else {
                                self.detect_export_name_list_call(node);
                            }
                        }
                        "default_export" => self.detect_export_pair_call(node),
                        _ => {}
                    }
                }
            }
        }
        // A topic-route DSL's scope function (`group { … }` in lite)
        // brackets the implicit base. The span is recorded so the
        // fold-phase pattern dispatch replays the topic-route base
        // stack in document order. The function's NAME comes from the
        // plugin manifest.
        let lite_group = self
            .active_topic_dsl()
            .map(|d| d.group_fn.clone())
            .is_some_and(|g| {
                node.child_by_field_name("function")
                    .and_then(|f| f.utf8_text(self.source).ok())
                    == Some(g.as_str())
            });
        if lite_group {
            self.topic_group_spans.push(node_to_span(node));
        }
        // If `modifier_invocant_pos` wasn't consumed by a nested `visit_anonymous_sub`
        // (malformed or modifier-without-sub-body code), clear it so it doesn't leak
        // to the next anonymous sub in the file.
        self.queue_children_then(node, |b| b.modifier_invocant_pos = None);
    }

    /// True when `node` is the bareword-filehandle argument of a
    /// `print`/`printf`/`say` call: `print FH LIST`. Perl parses the
    /// leading no-paren bareword as the indirect-object filehandle, which
    /// tree-sitter models as a nested `ambiguous_function_call_expression`
    /// (function = the filehandle, arguments = the print list) sitting in
    /// the verb's `arguments` slot. The parenthesized form `print foo(...)`
    /// parses as a `function_call_expression` instead, so it's a real call
    /// and never matches here.
    ///
    /// KLUDGE — works around a parser gap (docs/parser-shortcomings.md G4):
    /// the grammar already emits an `indirect_object` node for `print $fh`
    /// and `print {$fh}`, but NOT for the bareword form, which degrades to
    /// the function-call shape this guard sniffs out. Once upstream extends
    /// `indirect_object` to accept a bareword filehandle, delete this guard
    /// and consume the `indirect_object` node directly.
    pub(super) fn is_indirect_object_filehandle_call(&self, node: Node<'a>) -> bool {
        if node.kind() != "ambiguous_function_call_expression" {
            return false;
        }
        // The filehandle must be a plain bareword identifier (no sigil).
        let func = match node.child_by_field_name("function") {
            Some(f) => f,
            None => return false,
        };
        let fh_text = match func.utf8_text(self.source) {
            Ok(t) => t,
            Err(_) => return false,
        };
        if fh_text.is_empty()
            || !fh_text.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
            || !fh_text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        {
            return false;
        }
        // Parent must be a print-family verb, with `node` in its argument slot.
        let parent = match node.parent() {
            Some(p) => p,
            None => return false,
        };
        if !matches!(
            parent.kind(),
            "ambiguous_function_call_expression" | "function_call_expression"
        ) {
            return false;
        }
        if parent.child_by_field_name("arguments").map(|a| a.id()) != Some(node.id()) {
            return false;
        }
        matches!(
            parent
                .child_by_field_name("function")
                .and_then(|f| f.utf8_text(self.source).ok()),
            Some("print" | "printf" | "say")
        )
    }

    /// Handle `push @EXPORT, 'foo', 'bar'` — append to export lists.
    pub(super) fn visit_push_call(&mut self, node: Node<'a>) {
        let args = match node.child_by_field_name("arguments") {
            Some(a) => a,
            None => return,
        };
        // First arg should be the array, rest are values
        let children: Vec<Node> = if args.kind() == "list_expression" {
            (0..args.child_count()).filter_map(|i| args.child(i)).filter(|c| c.is_named()).collect()
        } else {
            return;
        };
        if children.is_empty() { return; }
        let first = children[0];
        if first.kind() != "array" { return; }
        let arr_name = match first.utf8_text(self.source) {
            Ok(t) => t,
            Err(_) => return,
        };
        // Generic array contribution — `push @arr, X, Y` extends
        // `@arr`'s `Sequence` shape. Walk-time projection: resolve
        // each X / Y through `emit_expr_witness + bag_query_expr_span`,
        // look up the running Sequence for `@arr` in scope, append.
        // Latest-wins Variable witness keeps the running answer
        // queryable at any later point. Tuple shape only — no
        // homogeneous/heterogeneous classification yet.
        self.emit_array_push_contribution(arr_name, &children[1..]);

        let is_export = arr_name.ends_with("@EXPORT") && !arr_name.ends_with("@EXPORT_OK");
        let is_export_ok = arr_name.ends_with("@EXPORT_OK");
        if !is_export && !is_export_ok { return; }

        // Extract string values from remaining args
        let mut values = Vec::new();
        for child in &children[1..] {
            values.extend(self.extract_string_names(*child));
        }
        if !values.is_empty() {
            if is_export {
                self.export.extend(values);
            } else {
                self.export_ok.extend(values);
            }
        }
    }

    // ---- Runtime exporter modeling ----
    //
    // Static analysis can't run an exporter's `import()`, so we model the
    // declarative *setup* shapes: the names a package registers as exports
    // map to same-named subs in the package. We feed the discovered names
    // into `export_ok` — the existing `@EXPORT_OK` plumbing then drives
    // goto-def (`resolve_imported_function` → same-named sub), cross-file
    // `refs_to` (the consumer's `use X 'name'` FunctionCall ref pins to X;
    // the def is a `Sub { package: X }` symbol), and diagnostic suppression
    // (`find_exporters`). Generators (Exporter::Extensible `-name` entries,
    // inline coderefs) are best-effort: the name resolves to a same-named
    // sub if one exists, else goto-def stops at the `use` line. Tags
    // (`-tag`, `:tag`) and sigil'd vars (`$x`, `@y`) are group/var
    // vocabulary, not subs — skipped. Conditional/computed exports built
    // at runtime are unmodeled.

    /// Add `names` to `export_ok` (the package's exported vocabulary),
    /// deduped against what's already there. The defining sub is the
    /// same-named symbol — no separate provenance, matching how `@EXPORT_OK`
    /// names already trace to their subs via the resolver.
    pub(super) fn record_runtime_exports(&mut self, names: impl IntoIterator<Item = String>) {
        for name in names {
            if name.is_empty() { continue; }
            if !self.export_ok.contains(&name) && !self.export.contains(&name) {
                self.export_ok.push(name);
            }
        }
    }

    /// Form 2 recognizer: does `body` contain a `push @EXPORT, @{"${var}::EXPORT"}`
    /// (or `@EXPORT_OK`) where the symbolic deref interpolates the loop variable
    /// `loop_var`? Matched by shape (rule #10): a `push` whose first arg is an
    /// `@EXPORT`/`@EXPORT_OK` array and whose later args include a symbolic
    /// array-deref naming `${loop_var}...::EXPORT[_OK]`. The loop-list resolution
    /// (caller) decides which modules; this only confirms the re-export pattern.
    pub(super) fn body_has_symbolic_export_push(&self, body: Node<'a>, loop_var: &str) -> bool {
        // The interpolated scalar is the bare varname (`m` for `$m`/`${m}`).
        let bare = loop_var.trim_start_matches('$');
        self.find_symbolic_export_push(body, bare)
    }

    pub(super) fn find_symbolic_export_push(&self, node: Node<'a>, loop_var_bare: &str) -> bool {
        if matches!(
            node.kind(),
            "function_call_expression" | "ambiguous_function_call_expression"
        ) {
            if let Some(func) = node.child_by_field_name("function") {
                if func.utf8_text(self.source) == Ok("push") {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        if self.push_args_reexport_export(args, loop_var_bare) {
                            return true;
                        }
                    }
                }
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if self.find_symbolic_export_push(child, loop_var_bare) {
                    return true;
                }
            }
        }
        false
    }

    /// `push @EXPORT, @{"${m}::EXPORT"}` arg list: first arg an `@EXPORT`/
    /// `@EXPORT_OK` array, a later arg a symbolic deref of `${m}...::EXPORT[_OK]`.
    pub(super) fn push_args_reexport_export(&self, args: Node<'a>, loop_var_bare: &str) -> bool {
        let first = args.named_child(0);
        let target_is_export = first
            .and_then(|f| if f.kind() == "array" { f.named_child(0) } else { None })
            .filter(|v| v.kind() == "varname")
            .and_then(|v| v.utf8_text(self.source).ok())
            .map(|t| matches!(t, "EXPORT" | "EXPORT_OK"))
            .unwrap_or(false);
        if !target_is_export {
            return false;
        }
        for i in 1..args.named_child_count() {
            if let Some(arg) = args.named_child(i) {
                if self.is_symbolic_export_deref(arg, loop_var_bare) {
                    return true;
                }
            }
        }
        false
    }

    /// True if `node` is `@{"${var}::EXPORT"}` / `@{"${var}::EXPORT_OK"}` — a
    /// symbolic array-deref whose interpolated string both references the loop
    /// scalar `loop_var_bare` and targets an `::EXPORT`/`::EXPORT_OK` package var.
    pub(super) fn is_symbolic_export_deref(&self, node: Node<'a>, loop_var_bare: &str) -> bool {
        if node.kind() != "array" {
            return false;
        }
        // `@{ "..." }` parses as array > varname > block > ... interpolated string.
        let mut refs_loop_var = false;
        let mut targets_export = false;
        Self::scan_interpolated_export_target(
            node,
            self.source,
            loop_var_bare,
            &mut refs_loop_var,
            &mut targets_export,
        );
        refs_loop_var && targets_export
    }

    pub(super) fn scan_interpolated_export_target(
        node: Node<'a>,
        source: &[u8],
        loop_var_bare: &str,
        refs_loop_var: &mut bool,
        targets_export: &mut bool,
    ) {
        if node.kind() == "interpolated_string_literal" {
            if let Some(content) = node.named_child(0) {
                if let Ok(text) = content.utf8_text(source) {
                    // Static literal portion holds `::EXPORT[_OK]`; the
                    // interpolated scalar is a child node, so check raw text.
                    if text.contains("::EXPORT") {
                        *targets_export = true;
                    }
                }
                // The interpolated scalar (`${m}`) is a `scalar` child whose
                // varname is the loop var.
                for i in 0..content.named_child_count() {
                    if let Some(child) = content.named_child(i) {
                        if child.kind() == "scalar" {
                            if let Some(vn) = child.named_child(0) {
                                if vn.utf8_text(source) == Ok(loop_var_bare) {
                                    *refs_loop_var = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                Self::scan_interpolated_export_target(
                    child, source, loop_var_bare, refs_loop_var, targets_export,
                );
            }
        }
    }

    /// Record a re-export edge to `module` — "this module's surface includes
    /// `module`'s surface." Deduped; the package's own name and empty/sigil'd
    /// junk are skipped. `ExportSurface` walks these transitively at query time.
    pub(super) fn record_reexport_edge(&mut self, module: &str) {
        let module = module.trim();
        if module.is_empty() {
            return;
        }
        // A package re-exporting itself is a no-op edge (and a cycle the
        // query-time seen-set would absorb anyway); drop it here for cleanliness.
        if self.current_package.as_deref() == Some(module) {
            return;
        }
        if !self.reexport_modules.iter().any(|m| m == module) {
            self.reexport_modules.push(module.to_string());
        }
    }

    /// Form 1 (static splice): scan an `@EXPORT`/`@EXPORT_OK` assignment RHS for
    /// `@OtherPkg::EXPORT` / `@OtherPkg::EXPORT_OK` array-deref elements and mint
    /// a re-export edge to each `OtherPkg`. The CST shape is an `array` node
    /// whose `varname` text is the fully-qualified `Pkg::EXPORT[_OK]`; we strip
    /// the trailing `::EXPORT`/`::EXPORT_OK` to recover the package. Recognized
    /// by shape (rule #10): any qualified export-var deref in the list is an edge.
    pub(super) fn record_static_splice_reexports(&mut self, node: Node<'a>) {
        let mut edges = Vec::new();
        Self::collect_export_var_derefs(node, self.source, &mut edges);
        for pkg in edges {
            self.record_reexport_edge(&pkg);
        }
    }

    /// Walk `node` for `array` deref elements whose varname is `Pkg::EXPORT` or
    /// `Pkg::EXPORT_OK`, pushing the bare `Pkg` onto `out`.
    pub(super) fn collect_export_var_derefs(node: Node<'a>, source: &[u8], out: &mut Vec<String>) {
        if node.kind() == "array" {
            if let Some(varname) = node.named_child(0) {
                if varname.kind() == "varname" {
                    if let Ok(text) = varname.utf8_text(source) {
                        if let Some(pkg) = Self::package_from_export_var(text) {
                            out.push(pkg);
                        }
                    }
                }
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                Self::collect_export_var_derefs(child, source, out);
            }
        }
    }

    /// `Foo::Bar::EXPORT` / `Foo::Bar::EXPORT_OK` → `Some("Foo::Bar")`.
    /// Anything else (an unqualified `EXPORT`, a non-export var) → `None`.
    pub(super) fn package_from_export_var(var: &str) -> Option<String> {
        for suffix in ["::EXPORT_OK", "::EXPORT"] {
            if let Some(pkg) = var.strip_suffix(suffix) {
                if !pkg.is_empty() {
                    return Some(pkg.to_string());
                }
            }
        }
        None
    }

    /// Fold `%EXPORT_TAGS` membership into the export surface. A tag table is
    /// a `tag => [ name, ... ], ...` literal: a flat list alternating tag-name
    /// keys (`autoquoted_bareword`/string at even positions) with member-array
    /// values (`anonymous_array_expression` at odd positions). Only the *member*
    /// names are exports — the tag names are selectors, not subs — so we pull
    /// names from the value arrays only and feed them into `export_ok` (same
    /// surface as `@EXPORT_OK`; `exports_name` / `find_exporters` then answer
    /// `true`). Recognized by shape, so the plain `our %EXPORT_TAGS = (...)`
    /// assignment and the call-wrapped `Readonly::Hash our %EXPORT_TAGS => (...)`
    /// (and Const::Fast and friends) ride the same path — `table` is just the
    /// list literal, whatever produced it.
    pub(super) fn fold_export_tags_table(&mut self, table: Node<'a>) {
        let mut names = Vec::new();
        // Track the most recent tag-name key so each value array attributes its
        // members to the right tag for the `:tag` consumer selector. The table
        // is a flat `key => [..], key2 => [..]` list, so the key precedes its
        // array in named-child order.
        let mut pending_tag: Option<String> = None;
        for i in 0..table.named_child_count() {
            let Some(child) = table.named_child(i) else { continue };
            // Member names live in the value arrays (after each tag key); the
            // bareword/string keys are selectors, not subs, so they're skipped.
            if child.kind() == "anonymous_array_expression" {
                let members = Self::keep_sub_export_names(self.extract_string_names(child));
                if let Some(tag) = pending_tag.take() {
                    self.export_tags.entry(tag).or_default().extend(members.iter().cloned());
                }
                names.extend(members);
                self.record_export_member_sites(child);
            } else if let Ok(text) = child.utf8_text(self.source) {
                // A tag-name key: a bareword/string before its member array.
                // `:DEFAULT` is the Exporter alias for `@EXPORT`, synthesized in
                // `ExportSurface`, so don't store it as a literal tag here.
                let tag = text.trim().trim_matches(|c| c == '\'' || c == '"').trim_start_matches([':', '-']);
                if !tag.is_empty() {
                    pending_tag = Some(tag.to_string());
                }
            }
        }
        // Tag members are sub names exactly like `@EXPORT_OK` entries; drop
        // sigil'd globals / nested tag refs the array might have contributed.
        let names = Self::keep_sub_export_names(names);
        self.record_runtime_exports(names);
    }

    /// Recognize a call whose args declare `%EXPORT_TAGS` and fold the trailing
    /// table — the `Readonly::Hash our %EXPORT_TAGS => (...)` shape (Const::Fast
    /// and any other wrapper ride the same path). The wrapper function is not
    /// load-bearing; the witness is the declared variable. The args are a flat
    /// list `<%EXPORT_TAGS declaration>, <table list>`; fold every following
    /// list once we've seen the declaration.
    pub(super) fn fold_call_wrapped_export_tags(&mut self, node: Node<'a>) {
        let Some(args) = node.child_by_field_name("arguments") else { return };
        let mut saw_export_tags_decl = false;
        for i in 0..args.named_child_count() {
            let Some(child) = args.named_child(i) else { continue };
            if !saw_export_tags_decl {
                if child.kind() == "variable_declaration"
                    && child
                        .utf8_text(self.source)
                        .map_or(false, |t| t.trim_end().ends_with("%EXPORT_TAGS"))
                {
                    saw_export_tags_decl = true;
                }
                continue;
            }
            if child.kind() == "list_expression"
                || child.kind() == "parenthesized_expression"
                || child.kind() == "anonymous_array_expression"
            {
                self.fold_export_tags_table(child);
            }
        }
    }

    /// Record export-list member tokens under `node` (their per-word spans)
    /// so the post-walk pass can ref the ones that name a local sub. Applies
    /// the same sub-name filter as the export-surface folds: sigil'd globals
    /// and `-tag` / `:group` selectors aren't subs and never get a ref.
    pub(super) fn record_export_member_sites(&mut self, node: Node<'a>) {
        let pkg = self.current_package.clone();
        for (text, span) in self.extract_string_list(node) {
            if text
                .chars()
                .next()
                .map_or(false, |c| c == '_' || c.is_ascii_alphabetic())
            {
                self.export_member_sites.push((text, span, pkg.clone()));
            }
        }
    }

    /// Emit a `FunctionCall` ref for each recorded export-member site whose
    /// name matches a local `Sub`/`Method` symbol in the same package. Runs
    /// post-walk so forward-declared subs (the export list precedes them) are
    /// visible. A member that names no local sub (or a tag-name key, which is
    /// never recorded) produces no ref — so references/goto-def stay clean.
    pub(super) fn emit_export_member_refs(&mut self) {
        let sites = std::mem::take(&mut self.export_member_sites);
        for (name, span, pkg) in sites {
            let is_local_sub = self.symbols.iter().any(|s| {
                s.name == name
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.package == pkg
            });
            if !is_local_sub {
                continue;
            }
            // Post-walk: the scope stack is empty, so `add_ref`'s
            // `current_scope()` would panic. The file scope is the right home
            // for an export-list ref anyway (it sits at package top level).
            let scope = self.scopes.first().map(|s| s.id).unwrap_or(ScopeId(0));
            self.refs.push(Ref {
                kind: RefKind::FunctionCall,
                span,
                scope,
                target_name: name,
                access: AccessKind::Read,
                binding: pkg.map(|package| RefBinding::Function { package }),
                folded_from: None,
                arg_count: None,
            });
        }
    }

    /// Pin FunctionCall refs the walk left unresolved (`resolved_package:
    /// None`) to a local sub of the same name in the call's enclosing package.
    /// The walk-time `resolve_call_package` only sees subs declared *before*
    /// the call (it scans `self.symbols` as-collected), so a forward reference
    /// — a call above its `sub` — never pinned. That made goto-def /
    /// references / rename silently miss every forward call, while diagnostics
    /// (post-walk name lookup) did not — a resolution asymmetry. This
    /// post-walk pass closes it so forward and backward calls resolve
    /// identically; order-independent by construction (every sub + ref exists).
    pub(super) fn pin_unresolved_call_packages(&mut self) {
        use std::collections::HashMap;
        // Local sub/method name → the package(s) that define it.
        let mut sub_pkgs: HashMap<&str, Vec<&str>> = HashMap::new();
        for s in &self.symbols {
            if matches!(s.kind, SymKind::Sub | SymKind::Method) {
                if let Some(pkg) = s.package.as_deref() {
                    sub_pkgs.entry(s.name.as_str()).or_default().push(pkg);
                }
            }
        }
        if sub_pkgs.is_empty() {
            return;
        }
        let mut pins: Vec<(usize, String)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if !matches!(r.kind, RefKind::FunctionCall) || r.binding.is_some() {
                continue;
            }
            if crate::model::file_analysis::split_qualified(&r.target_name).0.is_some() {
                continue; // qualified calls already pin at walk time (step 1)
            }
            let Some(pkgs) = sub_pkgs.get(r.target_name.as_str()) else { continue };
            // Pin ONLY to a local sub in the call's OWN enclosing package
            // (`package_at_pos` is the canonical span-containment query — block-
            // scoped `{ package X; }` aware). A same-named sub in a DIFFERENT
            // local package is NOT this call's target: it may be an imported sub
            // of that name, which cross-file resolution owns. (An earlier
            // unique-local fallback pinned to it and hijacked the import.)
            let Some(encl) = self.package_at_pos(r.span.start) else { continue };
            if pkgs.iter().any(|p| *p == encl) {
                pins.push((i, encl.to_string()));
            }
        }
        for (i, pkg) in pins {
            self.refs[i].bind_function_package(pkg);
        }
    }

    /// Does this `substitution_regexp` carry the `/e` modifier (replacement is
    /// evaluated as Perl code)?
    pub(super) fn subst_replacement_is_eval(&self, node: Node<'a>) -> bool {
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                if c.kind() == "substitution_regexp_modifiers" {
                    return c.utf8_text(self.source).map_or(false, |t| t.contains('e'));
                }
            }
        }
        false
    }

    /// Re-parse an `s///e` replacement as Perl and emit refs for the entities
    /// inside it — function calls, method calls, and variables — so the code in
    /// the replacement gets the same navigation (goto-def / references / rename /
    /// highlight) as code anywhere else (rule #7: every meaningful token gets a
    /// ref). The snippet is padded with the replacement's leading rows/cols so
    /// the re-parsed nodes carry the *original* file coordinates — no per-node
    /// offset math, spans drop straight in. (`s///ee` double-eval is the
    /// documented edge; one level covers it.)
    ///
    /// This is a deliberate secondary-parse emitter, not the main walker (its
    /// `Node<'a>` is bound to the primary tree's lifetime, so the snippet nodes
    /// can't flow through `visit_*`). It emits the ref shapes directly; the
    /// downstream resolution passes (`resolve_variable_refs`, MCB) wire them up
    /// from `(scope, name)` exactly as for natively-walked refs.
    pub(super) fn emit_refs_in_eval_replacement(&mut self, repl: Node<'a>) {
        let Ok(text) = repl.utf8_text(self.source) else { return };
        if text.trim().is_empty() {
            return;
        }
        let start = repl.start_position();
        let mut snippet = String::with_capacity(start.row + start.column + text.len());
        for _ in 0..start.row { snippet.push('\n'); }
        for _ in 0..start.column { snippet.push(' '); }
        snippet.push_str(text);
        let bytes = snippet.into_bytes();
        let mut parser = create_parser();
        let Some(tree) = parser.parse(&bytes, None) else { return };
        let scope = self.current_scope();

        // A `scalar`/`array`/`hash` node is a plain variable only when its text
        // is sigil + identifier path: `$x`, `@a`, `%h`, `$Foo::bar`. Deref
        // wrappers (`${...}`, `@{...}`, `$$x`) don't match here — their inner
        // `scalar` child is a named child and gets matched on its own.
        let plain_var = |t: &str| {
            let mut cs = t.chars();
            matches!(cs.next(), Some('$' | '@' | '%'))
                && t.len() > 1
                && cs.all(|c| c.is_alphanumeric() || c == '_' || c == ':')
        };

        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "function_call_expression" | "ambiguous_function_call_expression" => {
                    if let Some(func) = n.child_by_field_name("function") {
                        if let Ok(name) = func.utf8_text(&bytes) {
                            let name = name.to_string();
                            let resolved_package = self.resolve_call_package(&name);
                            self.refs.push(Ref {
                                kind: RefKind::FunctionCall,
                                span: node_to_span(func),
                                scope,
                                target_name: name,
                                access: AccessKind::Read,
                                binding: resolved_package
                                    .map(|package| RefBinding::Function { package }),
                                folded_from: None,
                                arg_count: None,
                            });
                        }
                    }
                }
                "method_call_expression" => {
                    let method = n.child_by_field_name("method");
                    let invocant = n.child_by_field_name("invocant");
                    // Bareword method only — `$obj->$dynamic(...)` has a sigil'd
                    // method node and isn't a nameable target.
                    if let Some(method) = method {
                        if let Ok(name) = method.utf8_text(&bytes) {
                            if name.chars().next().map_or(false, |c| c == '_' || c.is_alphabetic()) {
                                self.refs.push(Ref {
                                    kind: RefKind::MethodCall {
                                        invocant: crate::model::conventions::Invocant::assume_canonical(
                                            invocant
                                                .and_then(|i| {
                                                    crate::cst::canonical_var_name(i, &bytes)
                                                        .or_else(|| i.utf8_text(&bytes).ok().map(String::from))
                                                })
                                                .unwrap_or_default(),
                                        ),
                                        invocant_span: invocant.map(node_to_span),
                                        // A fully-qualified call (`$o->Foo::Bar::m`)
                                        // keeps the full path in `target_name` but
                                        // narrows the renamable span to the `m` tail
                                        // (rule #7), mirroring FunctionCall.
                                        method_name_span: crate::cst::fq_tail_span(method, name),
                                        member_op: None,
                                        shape: crate::model::file_analysis::MemberShape::Unknown,
                                    },
                                    span: node_to_span(n),
                                    scope,
                                    target_name: name.to_string(),
                                    access: AccessKind::Read,
                                    binding: None,
                                    folded_from: None,
                                    arg_count: None,
                                });
                            }
                        }
                    }
                }
                "scalar" | "array" | "hash" => {
                    if let Ok(t) = n.utf8_text(&bytes) {
                        if plain_var(t) {
                            let ref_span = fq_tail_span(n, t);
                            self.refs.push(Ref {
                                kind: RefKind::Variable,
                                span: ref_span,
                                scope,
                                target_name: t.to_string(),
                                access: AccessKind::Read,
                                binding: None,
                                folded_from: None,
                                arg_count: None,
                            });
                        }
                    }
                }
                _ => {}
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    stack.push(c);
                }
            }
        }
    }

    /// Keep only entries that name a sub: a plain bareword. Sigil'd vars
    /// (`$x`/`@y`/`%z` — exported package globals, not subs) and tags
    /// (`-tag`/`:group` — Exporter::Extensible export groups) are dropped.
    /// The same-named-sub resolution only makes sense for sub names.
    pub(super) fn keep_sub_export_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
        names
            .into_iter()
            .filter(|n| {
                n.chars()
                    .next()
                    .map_or(false, |c| c == '_' || c.is_ascii_alphabetic())
            })
            .collect()
    }

    /// True when the current package `use`d a module whose manifest-declared
    /// keyword surface (`framework_mode_makers()`) includes `kw`. Per-package
    /// — `framework_imports` is file-global, so it can't gate a keyword like
    /// `option` that only some makers export.
    pub(super) fn package_imports_framework_keyword(&self, kw: &str) -> bool {
        let Some(pkg) = self.current_package.as_ref() else { return false };
        let Some(uses) = self.package_uses.get(pkg) else { return false };
        uses.iter().any(|m| {
            self.framework_mode_modules
                .get(m)
                .is_some_and(|(_, kws)| kws.iter().any(|k| k == kw))
        })
    }

    /// True if the current package `use`d an exporter whose vocabulary
    /// includes `export` / `exports` / `default_export` as declaration
    /// verbs — Exporter::Extensible or Exporter::Declare (incl. its
    /// `-magic` / role variants whose names start with that prefix).
    /// Gates the call-name detection so a plain `sub export {}` elsewhere
    /// isn't read as an export declaration.
    pub(super) fn package_uses_exporter_declare_family(&self) -> bool {
        let Some(pkg) = self.current_package.as_ref() else { return false };
        let Some(uses) = self.package_uses.get(pkg) else { return false };
        uses.iter().any(|m| {
            m == "Exporter::Extensible"
                || m == "Exporter::Declare"
                || m.starts_with("Exporter::Declare::")
        })
    }

    /// True if the current package `use`d an exporter that defines the
    /// method-call setup verbs `setup_import_methods` (Moose::Exporter) /
    /// `add_type` (Type::Library, Exporter::Tiny, Type::Tiny). Gates those
    /// method-call detections so an unrelated `$x->add_type({name=>...})`
    /// or `$x->setup_import_methods(...)` elsewhere isn't read as an export
    /// declaration (false cross-file goto-def + suppressed diagnostics).
    pub(super) fn package_uses_moose_exporter_or_type_library(&self) -> bool {
        let Some(pkg) = self.current_package.as_ref() else { return false };
        let Some(uses) = self.package_uses.get(pkg) else { return false };
        uses.iter().any(|m| {
            m == "Moose::Exporter"
                || m == "Type::Library"
                || m == "Type::Tiny"
                || m == "Exporter::Tiny"
                || m.starts_with("Type::Library::")
        })
    }

    /// Distinguish `export NAME => sub {...}` (Exporter::Declare pair form)
    /// from `export(qw/.../)` / `export('a', 'b')` (name-list form). The
    /// pair form's second positional is a coderef; the list form's args are
    /// all names. We treat presence of an `anonymous_subroutine_expression`
    /// (or `\&sub` reference) among the args as the pair-form signal.
    pub(super) fn is_export_pair_call(&self, node: Node<'a>) -> bool {
        let Some(args) = node.child_by_field_name("arguments") else { return false };
        for i in 0..args.named_child_count() {
            if let Some(c) = args.named_child(i) {
                if matches!(c.kind(), "anonymous_subroutine_expression" | "refgen_expression") {
                    return true;
                }
            }
        }
        false
    }

    /// `export( qw( foo $bar -tag ) )` / `exports(qw/a b/)` — Exporter::
    /// Extensible's `export(...)` and Exporter::Declare's `exports(...)`.
    /// Both take a flat name list; only the sub-name entries are kept.
    pub(super) fn detect_export_name_list_call(&mut self, node: Node<'a>) {
        let Some(args) = node.child_by_field_name("arguments") else { return };
        let names = Self::keep_sub_export_names(self.extract_string_names(args));
        self.record_runtime_exports(names);
    }

    /// `export NAME => sub {...}` / `default_export NAME => sub {...}`
    /// (Exporter::Declare). The first positional is the exported name; the
    /// value is an inline coderef (or, if absent, a same-named sub). We
    /// model the name → same-named sub edge; the inline coderef body isn't
    /// a separately-addressable symbol, so goto-def lands on a `sub NAME`
    /// if the author also defined one (common) and otherwise on the `use`
    /// line. Honest limit: anonymous-only exports aren't resolvable.
    pub(super) fn detect_export_pair_call(&mut self, node: Node<'a>) {
        let Some(args) = node.child_by_field_name("arguments") else { return };
        // First named child is the export name (bareword or string).
        let Some(first) = args.named_child(0) else { return };
        let names: Vec<String> = self.extract_node_string(first).into_iter().collect();
        self.record_runtime_exports(Self::keep_sub_export_names(names));
    }

    /// `sub foo : Export(...)` / `sub foo :Export` — Exporter::Extensible's
    /// method-attribute export form. The sub's own name is the export; the
    /// attribute's args are tags/options, not names. Called from `visit_sub`
    /// with the sub name already known.
    pub(super) fn detect_export_attribute(&mut self, node: Node<'a>, sub_name: &str) {
        let Some(attrs) = node.child_by_field_name("attributes") else { return };
        let mut has_export_attr = false;
        for i in 0..attrs.named_child_count() {
            let Some(attr) = attrs.named_child(i) else { continue };
            if attr.kind() != "attribute" { continue; }
            let attr_name = attr
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(self.source).ok());
            if attr_name == Some("Export") {
                has_export_attr = true;
                break;
            }
        }
        if has_export_attr {
            self.record_runtime_exports(Self::keep_sub_export_names([sub_name.to_string()]));
        }
    }

    /// Scan an `IMPORTER_MENU` sub body for `export => [...]` /
    /// `export_ok => [...]` fat-comma pairs in its return list and record
    /// the named subs. Walks the body for a `return` (or trailing) list;
    /// for each `export`/`export_ok` bareword key, pulls names from the
    /// following arrayref. Best-effort static read — a menu built at
    /// runtime (loops, conditionals, computed keys) isn't covered.
    pub(super) fn detect_importer_menu(&mut self, sub_node: Node<'a>) {
        let Some(body) = sub_node.child_by_field_name("body") else { return };
        // Collect every list_expression under the body (the return list and
        // its operands). We look at the immediate children of each list for
        // `key => [...]` pairs rather than recursing into the arrayrefs.
        let mut lists: Vec<Node<'a>> = Vec::new();
        Self::collect_list_expressions(body, &mut lists);
        // Collect first, record after — `for_each_pair_in_list` holds a
        // shared borrow of self that `record_runtime_exports` (mut) can't
        // overlap.
        let mut names: Vec<String> = Vec::new();
        for list in lists {
            self.for_each_pair_in_list(list, |key, val| {
                if matches!(key, "export" | "export_ok") {
                    names.extend(Self::keep_sub_export_names(self.extract_string_names(val)));
                }
                true
            });
        }
        self.record_runtime_exports(names);
    }

    /// Depth-first collect every `list_expression` node reachable from
    /// `node` (used to find the menu pairs anywhere in a return body).
    pub(super) fn collect_list_expressions(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if node.kind() == "list_expression" {
            out.push(node);
        }
        for i in 0..node.named_child_count() {
            if let Some(c) = node.named_child(i) {
                Self::collect_list_expressions(c, out);
            }
        }
    }

    /// Queue an array contribution for the worklist's re-emittable
    /// pass. We can't resolve method-call return types at walk time
    /// (phase 6 fills them); we just record the contribution shape
    /// and let `emit_array_push_witnesses` re-emit each iteration.
    pub(super) fn emit_array_push_contribution(&mut self, arr_name: &str, args: &[Node<'a>]) {
        if args.is_empty() { return; }
        let scope = self.current_scope();
        // Emit the Expr(span) witness for each arg now — the worklist
        // will look them up by span later. Walk-time emission is the
        // contract for "this expression's type is queryable from the
        // bag once the worklist settles."
        let spans: Vec<Span> = args
            .iter()
            .map(|n| {
                self.emit_expr_witness(*n);
                node_to_span(*n)
            })
            .collect();
        self.pending_array_pushes
            .push((scope, arr_name.to_string(), spans));
    }

    /// Post-fold pass: drain `pending_array_pushes`'s queued
    /// contributions into `Variable{arr_name, scope} +
    /// InferredType(Sequence(...))` witnesses. Runs once after
    /// `fold_to_fixed_point` (which fills `invocant_class`) +
    /// `emit_method_call_return_edges` (which publishes
    /// `Expression(refidx) → Edge(PackageSymbol{...})`), so by
    /// the time we read each contribution's type, the method-call
    /// chain has resolved end-to-end. Each contribution's
    /// `Expr(span)` was queued by `emit_expr_witness` at walk time
    /// and resolved by `resolve_forward_expr_witnesses` post-walk,
    /// so `bag_query_expr_span` chases through to the materialized
    /// type — no per-pass backfill needed here.
    pub(super) fn emit_array_push_witnesses(&mut self) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        self.bag.remove_by_source_tag("array_push");
        // Group by (scope, arr_name) so successive pushes accumulate into one
        // Sequence, keeping first-appearance order. Iterating the grouping
        // map directly would emit in hash order, which differs between two
        // builds of the same file — the rest of the bag is in document order,
        // and a nondeterministic bag means a nondeterministic cache blob.
        let mut order: Vec<(ScopeId, String)> = Vec::new();
        let mut grouped: std::collections::HashMap<(ScopeId, String), Vec<Span>> =
            std::collections::HashMap::new();
        for (scope, name, spans) in &self.pending_array_pushes {
            let key = (*scope, name.clone());
            if !grouped.contains_key(&key) {
                order.push(key.clone());
            }
            grouped.entry(key).or_default().extend(spans.iter().copied());
        }
        for key in order {
            let spans = grouped.remove(&key).expect("keyed on insert");
            let (scope, name) = key;
            let resolved: Vec<InferredType> = spans
                .iter()
                .filter_map(|sp| self.bag_query_expr_span(*sp))
                .collect();
            if resolved.is_empty() { continue; }
            // Zero-span "applies forever after this point" — matches
            // the chain-assignment / TC-mirror convention. A
            // non-zero scoped span would get filtered out by
            // `FrameworkAwareTypeFold`'s narrowing rule for any
            // query point outside the span.
            let last = *spans.last().expect("non-empty by construction");
            let zero = Span { start: last.end, end: last.end };
            self.bag.push(Witness {
                attachment: WitnessAttachment::Variable { name, scope },
                source: WitnessSource::Builder("array_push".into()),
                payload: WitnessPayload::InferredType(InferredType::Sequence(resolved)),
                span: zero,
            });
        }
    }

    /// Record each method-call invocant's resolved type on
    /// `Expr(invocant_span)`. The query-time mirror
    /// (`FileAnalysis::expr_type_at_span`) reads these to type an
    /// expression without re-walking the CST. Materializes the answer
    /// from `invocant_type_at_node` (the single build-time symbolic
    /// executor) rather than an edge: the invocant's span is its own
    /// attachment, no edge mirrors it, so this is a source value the
    /// walker uniquely computes — not a parallel cache of an
    /// edge-reachable result. Chain receivers whose class is only
    /// knowable cross-file resolve to `None` here and stay unrecorded;
    /// the query side's chain recursion fills them at enrichment.
    pub(super) fn emit_invocant_expr_witnesses(&mut self, idx: &ChainTypingIndex<'a>) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let mut pending: Vec<(Span, Node<'a>)> = Vec::new();
        for r in &self.refs {
            if let RefKind::MethodCall {
                invocant_span: Some(sp),
                ..
            } = &r.kind
            {
                if let Some(n) = idx.invocant_nodes.get(&(sp.start, sp.end)).copied() {
                    pending.push((*sp, n));
                }
            }
        }
        for (span, node) in pending {
            let Some(ty) = self.invocant_type_at_node(node) else { continue };
            self.bag.push(Witness {
                attachment: WitnessAttachment::Expr(span),
                source: WitnessSource::Builder("invocant_expr".into()),
                payload: WitnessPayload::InferredType(ty),
                span,
            });
        }
    }

    /// Extract exported function name(s) from a glob assignment in `sub import`.
    /// Handles: `*{"${caller}::np"} = \&np`, `*{"$caller\::$imported"} = \&p`,
    /// `*{$caller . '::confess'} = \&confess`, and loop patterns.
    /// Returns one or more caller-visible names (from the glob string after "::"),
    /// falling back to the RHS \&name if the glob name is dynamic.
    pub(super) fn extract_glob_export_names(&self, glob_node: Node<'a>, assign_node: Node<'a>) -> Vec<String> {
        // Try to extract from glob's interpolated string: the name after "::"
        // AST: glob > varname > block > expression_statement > interpolated_string_literal > string_content
        let names = self.extract_names_from_glob(glob_node);
        if !names.is_empty() {
            return names;
        }

        // Fallback: extract function name from RHS \&name
        // AST: refgen_expression > function > varname
        if let Some(right) = assign_node.child_by_field_name("right") {
            return self.extract_names_from_refgen(right);
        }
        vec![]
    }

    /// Synthesize a local Sub symbol for each name installed via typeglob
    /// assignment whose RHS produces a sub/coderef. Shape-driven (rule #10),
    /// mirroring the DBIC `add_columns` / `mk_group_accessors` producers: the
    /// LHS glob name is the authoritative source, the RHS is the "is this a
    /// sub?" gate. Without this, glob-installed subs (File::Fetch, IPC::Cmd,
    /// File::Path, Type::Tiny) never become symbols and every call site flags
    /// unresolved. Dynamic names (`*{$runtime}`, unfoldable concat) are skipped
    /// rather than guessed.
    pub(super) fn synthesize_glob_assigned_sub(&mut self, glob_node: Node<'a>, assign_node: Node<'a>) {
        // Gate on RHS shape: only a sub-producing rvalue installs a callable.
        let Some(right) = assign_node.child_by_field_name("right") else { return };
        if !self.glob_rhs_is_sub(right) {
            return;
        }
        let names = self.glob_install_names(glob_node, assign_node);
        let sel_span = node_to_span(glob_node);
        let def_span = node_to_span(assign_node);
        for name in names {
            // Glob into another package (`*Other::foo = ...`) installs `foo`;
            // local call sites and goto use the unqualified tail. The glob
            // string's package prefix is authoritative for attribution:
            // `*{ 'DateTime::' . $sub } = __PACKAGE__->can($sub)` synthesizes
            // the tail under `DateTime`, not the file's own `current_package`
            // (`DateTime::PP`), so `PackageSymbol{DateTime, _ymd2rd}` resolves.
            // Unqualified names stay under the current package.
            let (target_pkg, local) = match crate::model::file_analysis::split_qualified(&name) {
                (Some(prefix), tail) if !prefix.is_empty() => {
                    (Some(prefix.to_string()), tail.to_string())
                }
                _ => (self.current_package.clone(), name.clone()),
            };
            if local.is_empty() {
                continue;
            }
            self.add_symbol_in_package(
                local,
                SymKind::Sub,
                def_span,
                sel_span,
                SymbolDetail::Sub {
                    params: vec![],
                    is_method: false,
                    doc: None,
                    opaque_return: false,
                    is_constant: false,
                    lexical: false,
                },
                Namespace::Language,
                target_pkg,
            );
        }
    }

    /// Is this RHS node a sub/coderef rvalue (anon sub, `\&name` alias, or a
    /// scalar holding a coderef)? The gate for glob-assign symbol synthesis.
    pub(super) fn glob_rhs_is_sub(&self, right: Node<'a>) -> bool {
        match right.kind() {
            "anonymous_subroutine_expression" => true,
            // `\&name` / `\&$var` — a code ref. `refgen_expression > function`.
            "refgen_expression" => right
                .named_child(0)
                .map(|c| c.kind() == "function")
                .unwrap_or(false),
            // `*name = $coderef;` — a scalar presumed to hold a code ref. The
            // LHS-name gate keeps this from over-firing on truly-dynamic globs.
            "scalar" => true,
            // CG-3b: `*{ 'Pkg::' . $sub } = __PACKAGE__->can($sub)` (DateTime::PP)
            // — `->can(...)` yields a coderef. Recognize as sub-producing so the
            // glob installs a symbol. `can` on any package/`__PACKAGE__`/class
            // invocant qualifies; the LHS-name gate still requires a
            // statically-derivable target name.
            "method_call_expression" => self.method_call_yields_coderef(right),
            // Conditional install: `*name = $cond ? \&a : sub {...}` (Try::Tiny's
            // `*_subname = $su ? \&Sub::Util::set_subname : sub {...}`, Path::Tiny's
            // `*_same = IS_WIN32() ? sub{} : sub{}`). The glob holds a coderef in
            // every branch, so the name is callable iff BOTH arms are sub-producing.
            "conditional_expression" => {
                let arm = |f| {
                    right
                        .child_by_field_name(f)
                        .is_some_and(|n| self.glob_rhs_is_sub(n))
                };
                arm("consequent") && arm("alternative")
            }
            _ => false,
        }
    }

    /// Does this method call yield a coderef? Today: the universal `->can(NAME)`
    /// (UNIVERSAL::can returns the method's coderef or undef). Invocant must be a
    /// class-ish receiver (`Pkg`, `__PACKAGE__`, a bareword/package node) — not a
    /// `$obj` instance, where `->can` is equally valid but the name list it feeds
    /// (cross-package glob targets) only makes sense for class injection.
    pub(super) fn method_call_yields_coderef(&self, node: Node<'a>) -> bool {
        let Some(method) = node.child_by_field_name("method") else { return false };
        if method.utf8_text(self.source).ok() != Some("can") {
            return false;
        }
        match node.child_by_field_name("invocant").map(|n| n.kind()) {
            Some("package" | "bareword" | "func0op_call_expression") => true,
            _ => false,
        }
    }

    /// Resolve the statically-derivable name(s) the glob LHS installs into.
    /// Returns empty when fully dynamic (no fabrication).
    ///
    /// Shapes: `*name` (static), `*{ 'literal' }` (string block),
    /// `*$m` (loop var, via constant folding), `*{ $runtime }` / unfoldable
    /// concat → empty.
    pub(super) fn glob_install_names(&self, glob_node: Node<'a>, assign_node: Node<'a>) -> Vec<String> {
        // First child is `varname` for the bare form (`*name`) or a
        // `glob_deref_expression` wrapping the block for `*{ EXPR }` — both
        // expose the operand as their first named child, so the index walk
        // below is shape-agnostic.
        let Some(head) = glob_node.named_child(0) else { return vec![] };

        // Bare static name: `*name` / `*Foo::bar` — varname has no inner block.
        if head.named_child_count() == 0 {
            if let Ok(text) = head.utf8_text(self.source) {
                if !text.is_empty() {
                    return vec![text.to_string()];
                }
            }
            return vec![];
        }

        let Some(inner) = head.named_child(0) else { return vec![] };
        match inner.kind() {
            // `*$m = ...` — scalar (loop var). Resolve via constant folding so
            // `for my $m (qw/a b/)` installs both names; bail on unknown vars.
            "scalar" => self.glob_name_from_scalar(inner),
            // `*{ ... }` — block expression. Derive only when literal-ish.
            "block" => self.glob_name_from_block(inner, assign_node),
            _ => vec![],
        }
    }

    /// `*$m` — resolve the scalar to concrete name(s) via constant folding.
    pub(super) fn glob_name_from_scalar(&self, scalar: Node<'a>) -> Vec<String> {
        let Some(varname) = scalar.named_child(0) else { return vec![] };
        let bare = varname.utf8_text(self.source).unwrap_or("");
        if bare.is_empty() {
            return vec![];
        }
        self.resolve_constant_strings(&format!("${}", bare), 0).unwrap_or_default()
    }

    /// `*{ EXPR }` — derive name(s) from the block's single expression.
    pub(super) fn glob_name_from_block(&self, block: Node<'a>, _assign_node: Node<'a>) -> Vec<String> {
        let inner = (|| {
            let stmt = block.named_child(0)?;
            stmt.named_child(0)
        })();
        let Some(expr) = inner else { return vec![] };
        self.enumerate_string_values(expr)
    }

    /// The statically enumerable string values an expression can take, or
    /// empty when not decidable (a call, an unknown variable, a non-`.`
    /// operator). The general "what strings is this expression" query —
    /// folds string literals, interpolations over known loop/lexical vars
    /// (`"get_$name"`), constant/loop-var refs (`$name`, a `use constant`),
    /// and `.`-concatenation of the same (`'find_' . $name`). An
    /// undecidable operand collapses the whole result to empty (honest: no
    /// partial guess). Consumers structurally project these into symbols:
    /// glob-install name derivation (`*{"…"} = sub`) and dynamic helper /
    /// plugin registration names (`$app->helper("get_$name" => …)`).
    pub(super) fn enumerate_string_values(&self, expr: Node<'a>) -> Vec<String> {
        match expr.kind() {
            "string_literal" => self.extract_string_content(expr).into_iter().collect(),
            "interpolated_string_literal" => self.try_fold_interpolated_string(expr),
            "binary_expression" => self.try_fold_string_concat(expr),
            "scalar" | "array" | "hash" | "bareword" => self
                .resolve_constant_strings(expr.utf8_text(self.source).unwrap_or(""), 0)
                .unwrap_or_default(),
            "parenthesized_expression" | "list_expression" => expr
                .named_child(0)
                .map(|c| self.enumerate_string_values(c))
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    /// Fold a `.`-concatenation of constant-resolvable operands into name(s).
    /// Returns empty if any operand can't be resolved to a concrete string —
    /// the "don't fabricate a partial name" boundary.
    pub(super) fn try_fold_string_concat(&self, node: Node<'a>) -> Vec<String> {
        // Cartesian product across operands so a loop-var operand expands.
        let mut acc: Vec<String> = vec![String::new()];
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else { return vec![] };
            let pieces: Vec<String> = match child.kind() {
                "string_literal" => self.extract_string_content(child).into_iter().collect(),
                "interpolated_string_literal" => self.try_fold_interpolated_string(child),
                "binary_expression" => self.try_fold_string_concat(child),
                "scalar" => {
                    let varname = child.named_child(0);
                    match varname.and_then(|v| v.utf8_text(self.source).ok()) {
                        Some(bare) if !bare.is_empty() => self
                            .resolve_constant_strings(&format!("${}", bare), 0)
                            .unwrap_or_default(),
                        _ => vec![],
                    }
                }
                _ => vec![],
            };
            if pieces.is_empty() {
                return vec![]; // unresolvable operand → don't guess
            }
            acc = acc
                .iter()
                .flat_map(|prefix| pieces.iter().map(move |p| format!("{}{}", prefix, p)))
                .collect();
        }
        acc.into_iter().filter(|s| !s.is_empty()).collect()
    }

    /// Walk the glob's interpolated string AST to find the exported name(s) after "::".
    pub(super) fn extract_names_from_glob(&self, glob_node: Node<'a>) -> Vec<String> {
        // glob > (varname | glob_deref_expression) > block > expression_statement
        //   > interpolated_string_literal — the head node differs by spelling
        //   (`*name` vs `*{ EXPR }`) but the index walk is the same.
        let content = (|| {
            let head = glob_node.named_child(0)?;
            let block = head.named_child(0)?;
            let expr_stmt = block.named_child(0)?;
            let interp = expr_stmt.named_child(0)?;
            if interp.kind() != "interpolated_string_literal" { return None; }
            let c = interp.named_child(0)?;
            if c.kind() == "string_content" { Some(c) } else { None }
        })();
        let content = match content {
            Some(c) => c,
            None => return vec![],
        };

        // Walk string_content: find the last "::" in literal segments,
        // then the part after it is the exported name (literal or variable).
        let content_bytes = &self.source[content.start_byte()..content.end_byte()];
        let content_text = match std::str::from_utf8(content_bytes) {
            Ok(t) => t,
            Err(_) => return vec![],
        };

        // Find position of last "::" in the raw content
        let colons_pos = match content_text.rfind("::") {
            Some(p) => p,
            None => return vec![],
        };
        let after_colons = colons_pos + 2;

        // Check if there's a named child (scalar variable) that starts after the "::"
        let last_idx = match content.named_child_count().checked_sub(1) {
            Some(i) => i,
            None => return vec![],
        };
        let last_child = match content.named_child(last_idx) {
            Some(c) => c,
            None => return vec![],
        };
        if last_child.kind() == "scalar" && last_child.start_byte() >= content.start_byte() + after_colons {
            // The name is a variable like $imported or $name — resolve via constant folding.
            // Use scalar > varname to get the canonical name (without ${} braces).
            if let Some(varname_node) = last_child.named_child(0) {
                let bare_name = varname_node.utf8_text(self.source).unwrap_or("");
                let lookup_key = format!("${}", bare_name);
                if let Some(values) = self.resolve_constant_strings(&lookup_key, 0) {
                    return values;
                }
            }
            return vec![];
        }

        // The name is literal text after "::"
        let suffix = &content_text[after_colons..];
        if !suffix.is_empty() && !suffix.starts_with('$') {
            return vec![suffix.to_string()];
        }

        vec![]
    }

    /// Extract bare function name(s) from a `\&name` or `\&$var` refgen expression.
    pub(super) fn extract_names_from_refgen(&self, refgen: Node<'a>) -> Vec<String> {
        if refgen.kind() != "refgen_expression" {
            return vec![];
        }
        let func = match refgen.named_child(0) {
            Some(f) if f.kind() == "function" => f,
            _ => return vec![],
        };
        // `\&{ EXPR }` — symbolic code-deref. The operand is the target
        // (e.g. `\&{$name}` resolves through `$name`'s constant value,
        // `\&{"$pkg::$sym"}` is a runtime string we can't statically name).
        if let Some(code_deref) = code_deref_in(func) {
            if let Some(operand) = code_deref_operand(code_deref) {
                if operand.kind() == "scalar" {
                    if let Some(scalar_varname) = operand.named_child(0) {
                        let bare_name = scalar_varname.utf8_text(self.source).unwrap_or("");
                        let lookup_key = format!("${}", bare_name);
                        if let Some(values) = self.resolve_constant_strings(&lookup_key, 0) {
                            return values;
                        }
                    }
                }
            }
            return vec![];
        }
        // function > varname, possibly containing a scalar child (for \&$name)
        let varname = match func.named_child(0) {
            Some(v) => v,
            None => return vec![],
        };
        if let Some(scalar) = varname.named_child(0) {
            if scalar.kind() == "scalar" {
                // \&$name — resolve variable via scalar > varname (canonical, no ${} braces)
                if let Some(scalar_varname) = scalar.named_child(0) {
                    let bare_name = scalar_varname.utf8_text(self.source).unwrap_or("");
                    let lookup_key = format!("${}", bare_name);
                    if let Some(values) = self.resolve_constant_strings(&lookup_key, 0) {
                        return values;
                    }
                }
                return vec![];
            }
        }
        // Bare name like \&np — varname text is the function name
        match varname.utf8_text(self.source) {
            Ok(name) => vec![name.to_string()],
            Err(_) => vec![],
        }
    }
}

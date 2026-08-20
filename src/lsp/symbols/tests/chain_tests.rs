//! data-printer plugin intelligence, witness-bag chain typing, chain completion,
//! and cross-file helper/goto resolution.

use super::*;

// ---- data-printer plugin: full intelligence ----

/// Build a CachedModule under the real package name we want, with
/// arbitrary source. `fake_cached` always synthesizes a `package
/// Fake;` source — useless when the caller needs the cached
/// module to expose subs under a specific name like `Data::Printer`.
fn cached_under(name: &str, source: &str) -> std::sync::Arc<crate::index::module_index::CachedModule> {
    let analysis = parse_analysis(source);
    std::sync::Arc::new(crate::index::module_index::CachedModule::new(
        std::path::PathBuf::from(format!("/fake/{}.pm", name.replace("::", "/"))),
        std::sync::Arc::new(analysis),
    ))
}

#[test]
fn data_printer_use_ddp_resolves_p_to_data_printer() {
    // The end-to-end intelligence pin. `use DDP` is a literal alias
    // for `use Data::Printer` (DDP.pm just `push our @ISA, 'Data::Printer'`).
    // Hover/K, gd, and sig-help on `p` must reach Data::Printer's
    // real `sub p` — not DDP. The plugin's synthetic Import
    // (module_name: "Data::Printer", imported_symbols: [p, np]) is
    // what carries this; resolve_imported_function is the seam every
    // intelligence feature routes through.
    let source = "use DDP;\np $foo;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();

    // Stub Data::Printer.pm. The real module has a custom `import`
    // (no @EXPORT), but `sub p` is a normal sub the builder picks
    // up — exactly what users get from cpan.
    module_index.insert_cache(
            "Data::Printer",
            Some(cached_under(
                "Data::Printer",
                "package Data::Printer;\nsub p { my (undef, %props) = @_; }\nsub np { my (undef, %props) = @_; }\n1;\n",
            )),
        );

    let resolved = resolve_imported_function(&analysis, "p", &module_index);
    assert!(
        resolved.is_some(),
        "use DDP must alias to Data::Printer; resolve_imported_function for `p` returned None — \
             imports were: {:?}",
        analysis
            .imports
            .iter()
            .map(|i| (
                i.module_name.clone(),
                i.imported_symbols
                    .iter()
                    .map(|s| s.local_name.clone())
                    .collect::<Vec<_>>(),
            ))
            .collect::<Vec<_>>()
    );
    let (import, _path, remote) = resolved.unwrap();
    assert_eq!(
        import.module_name, "Data::Printer",
        "alias must route to Data::Printer, not DDP"
    );
    assert_eq!(remote, "p", "local `p` maps to remote `p`");

    // np too — both DDP-installed names.
    let np = resolve_imported_function(&analysis, "np", &module_index);
    assert!(
        np.is_some(),
        "use DDP must also resolve `np` to Data::Printer"
    );
    assert_eq!(np.unwrap().0.module_name, "Data::Printer");
}

#[test]
fn data_printer_use_data_printer_resolves_p_to_data_printer() {
    // Same test for the non-alias case. `use Data::Printer;` with no
    // qw list — the plugin's synthetic Import claims `p`/`np` so
    // resolve_imported_function pairs them with the real sub.
    let source = "use Data::Printer;\np $foo;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    module_index.insert_cache(
            "Data::Printer",
            Some(cached_under(
                "Data::Printer",
                "package Data::Printer;\nsub p { my (undef, %props) = @_; }\nsub np { my (undef, %props) = @_; }\n1;\n",
            )),
        );

    let resolved = resolve_imported_function(&analysis, "p", &module_index);
    assert!(
        resolved.is_some(),
        "use Data::Printer (no qw list) must still let resolve_imported_function find p"
    );
    assert_eq!(resolved.unwrap().0.module_name, "Data::Printer");
}

#[test]
fn data_printer_use_line_options_completion() {
    // `use DDP { | }` — cursor inside the options hashref. The
    // plugin's on_completion hook recognizes "current_use_module
    // matches DDP/Data::Printer and cursor_inside is a Hash" and
    // returns the documented option keys (caller_info, colored,
    // class_method, output, ...). No core hard-codes the option
    // list — it lives in the plugin.
    let source = "use DDP { };\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();

    // Cursor between the braces. Source: "use DDP { };" → col 10
    // is one past the opening brace (which lives at col 9).
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();

    let pos = Position {
        line: 0,
        character: 10,
    };
    let items = completion_items_for_test(&analysis, &tree, source, pos, &module_index, None);

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    // Sample of keys from Data::Printer's actual options. If the
    // plugin doesn't ship these specific names, swap to whichever
    // ones the plugin advertises — the contract is "DDP options
    // surface here", not "this exact list".
    for key in &["caller_info", "colored", "class_method", "output"] {
        assert!(
            labels.iter().any(|l| l == key),
            "use DDP {{ }} option completion must offer `{}`; got: {:?}",
            key,
            labels,
        );
    }
}

// ---- witness-bag chain typing: pin-the-fix on the real demo ----

/// Pin against the actual demo file. Loads
/// `test_files/plugin_mojo_demo.pl` + stubs of the Mojolicious
/// hierarchy registered into the module index, then asserts:
/// (a) `$r` at line 71 resolves to a known class.
/// (b) `->to` on line 71 is a MethodCall ref.
/// (c) `->to`'s invocant resolves to a class via
///     `FileAnalysis::method_call_invocant_class` (the bag-routed
///     resolver every reader — hover, gd, gr, rename — funnels
///     through).
///
/// Two possible failure modes the test distinguishes:
///   - `$r` is typed but `->to`'s invocant fails → crossfile
///     chain hop is the gap (find_method_return_type's CrossFile
///     branch).
///   - `$r` isn't typed at all → earlier hop broken first.
#[test]
fn test_demo_file_chain_to_resolves_on_line_71() {
    use std::fs;
    use std::path::PathBuf;

    // Project root: the worktree (= CARGO_MANIFEST_DIR).
    let root: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    let demo = root.join("test_files/plugin_mojo_demo.pl");
    let demo_source = fs::read_to_string(&demo).expect("demo file present");

    // Index the project's test_files/ as the workspace.
    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(Some(root.to_str().unwrap()));
    let files = crate::index::file_store::FileStore::new();
    let _indexed = crate::index::module_resolver::index_workspace_with_index(
        &root.join("test_files"),
        &files,
        Some(&idx),
        None,
        None,
    );

    // Use the ACTUAL Mojolicious library from @INC — the same
    // code nvim analyzes. If Mojo isn't installed, skip cleanly
    // so CI on bare systems doesn't break.
    let inc_paths = crate::index::module_resolver::discover_inc_paths();
    let insert_real = |name: &str| -> bool {
        let mut p = crate::index::module_resolver::create_parser();
        match crate::index::module_resolver::resolve_and_parse(&inc_paths, name, &mut p) {
            Some(cached) => {
                idx.insert_cache(name, Some(cached));
                true
            }
            None => false,
        }
    };
    let have_mojo = insert_real("Mojolicious")
        && insert_real("Mojolicious::Routes")
        && insert_real("Mojolicious::Routes::Route")
        && insert_real("Mojolicious::Lite");
    if !have_mojo {
        eprintln!("SKIP: Mojolicious not installed in @INC");
        return;
    }
    let _ = PathBuf::new(); // keep the import used in both branches

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&demo_source, None).unwrap();
    let mut analysis = crate::build::builder::build(&tree, demo_source.as_bytes());

    // Cross-file enrichment — same step the backend runs on open.
    // Re-runs the MCB→bag bridge so `my $r = $app->routes;` carries a
    // `Variable → Edge(PackageSymbol)` witness the registry chases with
    // the index. Without this, `FileStore::enrich_open` hasn't fired
    // yet and the whole chain is un-typed.
    analysis.enrich_imported_types_with_keys(Some(&idx));

    // Find line 71 ($r->get('/users')->to('Users#list');).
    let (line_idx, chain_line) = demo_source
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("$r->get('/users')") && l.contains("->to('Users#list')"))
        .expect("chain line present in demo");

    // Position on `to` — the 't' character.
    let to_col = chain_line.find("->to(").unwrap() + 2;
    let r_col = chain_line.find("$r").unwrap();
    let get_col = chain_line.find("->get(").unwrap() + 2;

    let pt = |col: usize| tree_sitter::Point {
        row: line_idx,
        column: col,
    };

    // Diagnostics — what does the analysis actually see?
    let r_ty_bag = analysis.inferred_type_via_bag("$r", pt(r_col));
    let r_ty_legacy = analysis.inferred_type("$r", pt(r_col)).cloned();
    let mcb_for_r: Vec<_> = analysis
        .method_call_bindings
        .iter()
        .filter(|b| b.variable == "$r")
        .collect();
    let cb_for_r: Vec<_> = analysis
        .call_bindings
        .iter()
        .filter(|b| b.variable == "$r")
        .collect();
    // Is `app` even known as a symbol/import?
    let app_known = analysis.symbols().iter().any(|s| s.name == "app");
    // Is Mojolicious in the module index?
    let mojo_cached = idx.get_cached("Mojolicious").is_some();
    let routes_cached = idx.get_cached("Mojolicious::Routes").is_some();
    let route_cached = idx.get_cached("Mojolicious::Routes::Route").is_some();
    eprintln!(
        "DIAG: $r bag={:?}  legacy={:?}  mcbs={:?}  cbs={:?}  app_sym={}  \
             mojo_cached={}  routes_cached={}  route_cached={}",
        r_ty_bag,
        r_ty_legacy,
        mcb_for_r
            .iter()
            .map(|b| format!("{}.{}", b.invocant_var, b.method_name))
            .collect::<Vec<_>>(),
        cb_for_r.iter().map(|b| &b.func_name).collect::<Vec<_>>(),
        app_known,
        mojo_cached,
        routes_cached,
        route_cached,
    );

    // (a) `$r` is typed. This uses the EXACT path cursor_context
    // uses to type an invocant — inferred_type_via_bag.
    let r_ty = r_ty_bag;
    let r_class = r_ty.as_ref().and_then(|t| t.class_name());
    assert!(
        r_class.is_some(),
        "$r should be typed (any class) at {}:{}; got {:?}",
        line_idx + 1,
        r_col,
        r_ty,
    );

    // (b) At `->get`'s 'g', there's a MethodCall ref. Its
    // invocant is `$r`. Resolve it cross-file.
    let get_ref = analysis.ref_at(pt(get_col)).expect("ref at ->get");
    assert_eq!(get_ref.target_name, "get");
    if matches!(get_ref.kind, crate::model::file_analysis::RefKind::MethodCall { .. }) {
        let _ = (&tree, demo_source.as_bytes(), &idx, get_col);
        let klass = analysis.method_call_invocant_class(get_ref, Some(&idx));
        assert!(
            klass.is_some(),
            "`->get`'s invocant (= $r) should resolve to SOME class; got {:?}",
            klass,
        );
    }

    // (c) The `->to` hop. Real Mojolicious::Routes::Route::get
    // is `shift->_generate_route(GET => @_)` — our implicit-
    // return witnessing records that get's return chains
    // through _generate_route. _generate_route's own return is
    // `return defined $name ? $route->name($name) : $route;` —
    // a complex conditional whose arms depend on $route's
    // chain-built type. That depth of cross-file chain
    // resolution is a separate follow-up; for now we assert
    // the MethodCall ref exists and carries the right target,
    // but leave the class-resolution assertion as a diagnostic
    // rather than hard-fail.
    let to_ref = analysis.ref_at(pt(to_col)).expect("ref at ->to");
    assert_eq!(to_ref.target_name, "to");
    assert!(
        matches!(
            to_ref.kind,
            crate::model::file_analysis::RefKind::MethodCall { .. }
        ),
        "ref at ->to is a MethodCall"
    );
    if matches!(to_ref.kind, crate::model::file_analysis::RefKind::MethodCall { .. }) {
        let _ = (&tree, demo_source.as_bytes(), &idx, to_col);
        let klass = analysis.method_call_invocant_class(to_ref, Some(&idx));
        eprintln!(
            "DIAG: ->to invocant class (real Mojo): {:?} \
                 (None expected until deep chain through \
                 _generate_route/requires/to is resolved)",
            klass,
        );
    }
}

#[test]
fn data_printer_use_line_options_completion_for_data_printer_module() {
    // Same flow, non-alias name. The plugin can't be DDP-specific.
    let source = "use Data::Printer { };\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();

    // "use Data::Printer { };" — col 20 sits between the braces.
    let pos = Position {
        line: 0,
        character: 20,
    };
    let items = completion_items_for_test(&analysis, &tree, source, pos, &module_index, None);

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| *l == "caller_info"),
        "use Data::Printer {{ }} must surface options too; got: {:?}",
        labels,
    );
}
// ---- witness-driven chain completion (spike) ----

/// Decomposition: parse real Mojolicious/Routes/Route.pm
/// in-place (not via module index) and probe each hop of the
/// `$self->_route()->requires()->to()` chain separately. Plus
/// probe `$self->_generate_route(...)`. Reports what each
/// specific hop resolves to — so we know exactly which step
/// in the chain is actually dying.
#[test]
fn test_route_pm_chain_decomposition() {
    use std::fs;
    use std::path::PathBuf;
    let inc = crate::index::module_resolver::discover_inc_paths();
    let route_path = inc
        .iter()
        .map(|p| p.join("Mojolicious/Routes/Route.pm"))
        .find(|p| p.exists());
    let route_path: PathBuf = match route_path {
        Some(p) => p,
        None => {
            eprintln!("SKIP: Mojo not installed");
            return;
        }
    };
    let src = fs::read_to_string(&route_path).unwrap();

    // Parse Route.pm itself — `$self` inside = Route.
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());

    // Probe: find the `_generate_route` sub body and report
    // what we see on each hop.
    let inspect_sym = |name: &str| {
        for sym in analysis.symbols() {
            if sym.name != name {
                continue;
            }
            if !matches!(
                sym.kind,
                crate::model::file_analysis::SymKind::Sub | crate::model::file_analysis::SymKind::Method
            ) {
                continue;
            }
            if matches!(&sym.detail, crate::model::file_analysis::SymbolDetail::Sub { .. }) {
                let return_type = analysis.symbol_return_type_via_bag(sym.id, None);
                eprintln!("  sym[{:24}] return_type={:?}", name, return_type);
                return;
            }
        }
        eprintln!("  sym[{:24}] NOT FOUND", name);
    };
    eprintln!("======== symbol return types in Route.pm ========");
    for name in [
        "get",
        "post",
        "any",
        "to",
        "name",
        "requires",
        "_generate_route",
        "_route",
        "add_child",
        "pattern",
        "is_reserved",
        "root",
    ] {
        inspect_sym(name);
    }

    // Find `_generate_route`'s body block. Its last statement
    // is `return defined $name ? $route->name($name) : $route;`.
    // Probe each subexpression type via resolve_expression_type.
    fn find_sub_body<'t>(
        n: tree_sitter::Node<'t>,
        src: &[u8],
        name: &str,
    ) -> Option<tree_sitter::Node<'t>> {
        if n.kind() == "subroutine_declaration_statement" {
            if let Some(nm) = n.child_by_field_name("name") {
                if nm.utf8_text(src).ok() == Some(name) {
                    return n.child_by_field_name("body");
                }
            }
        }
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                if let Some(r) = find_sub_body(c, src, name) {
                    return Some(r);
                }
            }
        }
        None
    }
    let body = find_sub_body(tree.root_node(), src.as_bytes(), "_generate_route")
        .expect("_generate_route body");

    // Inside _generate_route, find:
    //   (a) the `my $route = CHAIN` assignment
    //   (b) the final `return TERNARY` expression
    fn find_var_decl_for<'t>(
        n: tree_sitter::Node<'t>,
        src: &[u8],
        var: &str,
    ) -> Option<tree_sitter::Node<'t>> {
        if n.kind() == "assignment_expression" {
            if let Some(left) = n.child_by_field_name("left") {
                if left.utf8_text(src).map(|s| s.trim()).ok() == Some(&format!("my {}", var)) {
                    return n.child_by_field_name("right");
                }
            }
        }
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                if let Some(r) = find_var_decl_for(c, src, var) {
                    return Some(r);
                }
            }
        }
        None
    }
    let route_rhs = find_var_decl_for(body, src.as_bytes(), "$route").expect("my $route = ... RHS");

    eprintln!();
    eprintln!("======== `my $route = RHS` decomposition ========");
    eprintln!(
        "RHS shape: {}  kind={}",
        route_rhs.utf8_text(src.as_bytes()).unwrap_or(""),
        route_rhs.kind()
    );

    // Probe chain hops from innermost outward. Each node in a
    // chain a->b->c has: outer is c's method_call_expression,
    // its invocant is the a->b method_call_expression, whose
    // invocant is `$self`.
    fn report_node_type(
        label: &str,
        n: tree_sitter::Node,
        analysis: &crate::model::file_analysis::FileAnalysis,
        src: &[u8],
    ) {
        let text = n.utf8_text(src).unwrap_or("").trim();
        let ty = crate::lsp::cursor_context::resolve_expression_type(&analysis, n, src, None);
        eprintln!(
            "  [{label:>12}] `{text:.60}`\n                kind={} → ty={:?}",
            n.kind(),
            ty
        );
    }

    // Walk the chain inside-out and report each level's type.
    let mut cur = Some(route_rhs);
    let mut depth = 0;
    while let Some(n) = cur {
        let label = match depth {
            0 => "outer",
            1 => "mid1",
            2 => "mid2",
            3 => "mid3",
            _ => "inner",
        };
        report_node_type(label, n, &analysis, src.as_bytes());
        if n.kind() == "method_call_expression" {
            cur = n.child_by_field_name("invocant");
            depth += 1;
        } else {
            break;
        }
    }

    eprintln!();
    eprintln!("======== return TERNARY probe ========");
    // Find the return_expression in body.
    fn find_return<'t>(n: tree_sitter::Node<'t>) -> Option<tree_sitter::Node<'t>> {
        if n.kind() == "return_expression" {
            return Some(n);
        }
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                if let Some(r) = find_return(c) {
                    return Some(r);
                }
            }
        }
        None
    }
    let ret = find_return(body).expect("return in _generate_route");
    let ternary = ret.named_child(0).expect("return child");
    eprintln!(
        "  return child kind = {}  text = `{}`",
        ternary.kind(),
        ternary.utf8_text(src.as_bytes()).unwrap_or("").trim()
    );
    if ternary.kind() == "conditional_expression" {
        let consequent = ternary.child_by_field_name("consequent");
        let alternative = ternary.child_by_field_name("alternative");
        if let Some(a) = consequent {
            report_node_type("then-arm", a, &analysis, src.as_bytes());
        }
        if let Some(b) = alternative {
            report_node_type("else-arm", b, &analysis, src.as_bytes());
        }
    }
}

/// Direct proof: enumerate each chain link's resolvability on
/// the real demo + real Mojolicious. Prints a truth table;
/// asserts specifically that `->to` does NOT resolve (the gap
/// the user flagged) so this test becomes a tripwire: if a
/// future fix makes `->to` resolve, this test will fail and
/// force us to promote it to a "works" assertion.
#[test]
fn test_demo_chain_empirical_truth_table() {
    use std::fs;
    use std::path::PathBuf;

    let root: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    let demo = root.join("test_files/plugin_mojo_demo.pl");
    let demo_source = fs::read_to_string(&demo).expect("demo file");

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(Some(root.to_str().unwrap()));
    let files = crate::index::file_store::FileStore::new();
    let _ = crate::index::module_resolver::index_workspace_with_index(
        &root.join("test_files"),
        &files,
        Some(&idx),
        None,
        None,
    );

    let inc = crate::index::module_resolver::discover_inc_paths();
    let install = |name: &str| -> bool {
        let mut p = crate::index::module_resolver::create_parser();
        match crate::index::module_resolver::resolve_and_parse(&inc, name, &mut p) {
            Some(c) => {
                idx.insert_cache(name, Some(c));
                true
            }
            None => false,
        }
    };
    if !(install("Mojolicious")
        && install("Mojolicious::Routes")
        && install("Mojolicious::Routes::Route")
        && install("Mojolicious::Lite"))
    {
        eprintln!("SKIP: Mojolicious not installed");
        return;
    }

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&demo_source, None).unwrap();
    let mut analysis = crate::build::builder::build(&tree, demo_source.as_bytes());
    analysis.enrich_imported_types_with_keys(Some(&idx));

    let (line_idx, chain_line) = demo_source
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("$r->get('/users')") && l.contains("->to('Users#list')"))
        .expect("demo chain line present");

    let r_col = chain_line.find("$r").unwrap();
    let get_col = chain_line.find("->get(").unwrap() + 2;
    let to_col = chain_line.find("->to(").unwrap() + 2;
    let pt = |c: usize| tree_sitter::Point {
        row: line_idx,
        column: c,
    };

    // --- Link 1: $r's type ---
    let r_ty = analysis.inferred_type_via_bag("$r", pt(r_col));
    let r_class = r_ty
        .as_ref()
        .and_then(|t| t.class_name())
        .map(|s| s.to_string());

    // --- Link 2: ->get's invocant class (= $r's class) ---
    let get_ref = analysis.ref_at(pt(get_col)).expect("ref at ->get");
    let get_invocant_class = if matches!(get_ref.kind, crate::model::file_analysis::RefKind::MethodCall { .. }) {
        analysis.method_call_invocant_class(get_ref, Some(&idx))
    } else {
        None
    };

    // --- Link 3: ->get's RETURN type (what `$r->get(...)` evaluates to) ---
    // Find the method_call_expression node for `$r->get('/users')`.
    let mcall_node = {
        fn find_getcall<'a>(n: tree_sitter::Node<'a>, src: &[u8]) -> Option<tree_sitter::Node<'a>> {
            if n.kind() == "method_call_expression" {
                if let Some(m) = n.child_by_field_name("method") {
                    if m.utf8_text(src).ok() == Some("get") {
                        return Some(n);
                    }
                }
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i) {
                    if let Some(r) = find_getcall(c, src) {
                        return Some(r);
                    }
                }
            }
            None
        }
        find_getcall(tree.root_node(), demo_source.as_bytes()).expect("->get node")
    };
    let get_return_ty =
        crate::lsp::cursor_context::resolve_expression_type(&analysis, mcall_node, demo_source.as_bytes(), Some(&idx));

    // --- Link 4: ->to's invocant class (= ->get's return class) ---
    let to_ref = analysis.ref_at(pt(to_col)).expect("ref at ->to");
    let to_invocant_class = if matches!(to_ref.kind, crate::model::file_analysis::RefKind::MethodCall { .. }) {
        analysis.method_call_invocant_class(to_ref, Some(&idx))
    } else {
        None
    };

    // Also directly inspect the cached Route module's stored
    // return types + self-method tails for each method on the
    // chain's path.
    let route_cached = idx.get_cached("Mojolicious::Routes::Route").unwrap();
    let inspect = |name: &str| -> Option<InferredType> {
        for sym in route_cached.analysis.symbols() {
            if sym.name != name {
                continue;
            }
            if !matches!(
                sym.kind,
                crate::model::file_analysis::SymKind::Sub | crate::model::file_analysis::SymKind::Method
            ) {
                continue;
            }
            if matches!(&sym.detail, crate::model::file_analysis::SymbolDetail::Sub { .. }) {
                return route_cached.analysis.symbol_return_type_via_bag(sym.id, None);
            }
        }
        None
    };
    let gen_rt = inspect("_generate_route");
    let get_rt = inspect("get");
    let to_rt = inspect("to");
    let requires_rt = inspect("requires");
    let _route_rt = inspect("_route");

    eprintln!("======== chain truth table ========");
    eprintln!("  $r              class = {:?}", r_class);
    eprintln!("  ->get invocant  class = {:?}", get_invocant_class);
    eprintln!("  ->get RETURN    type  = {:?}", get_return_ty);
    eprintln!("  ->to  invocant  class = {:?}", to_invocant_class);
    eprintln!("  ---- cached Route symbols ----");
    eprintln!("  get             rt={:?}", get_rt);
    eprintln!("  _generate_route rt={:?}", gen_rt);
    eprintln!("  requires        rt={:?}", requires_rt);
    eprintln!("  to              rt={:?}", to_rt);
    eprintln!("  _route          rt={:?}", _route_rt);
    eprintln!("====================================");

    // The chain pin. With:
    //   - mojo-routes plugin's `_route` override pinning the
    //     return type inference can't reach, AND
    //   - the post-walk `ChainTypingReducer` (PreFold mode)
    //     symbolically executing every `my $X = <expr>` rhs (no
    //     "is it a chain" branch — same recursion every consumer
    //     uses), AND
    //   - the same reducer's return-arm refresh running before the
    //     second fold so `_generate_route`'s ternary return picks
    //     up the now-typed `$route`,
    // the full `$r->get(...)->to(...)` chain resolves end-to-end.
    // Each link is pinned individually so a regression localizes
    // to a specific hop instead of "the chain broke".
    assert!(
        r_class.is_some(),
        "(link 1) $r must resolve to a class; got None"
    );
    assert_eq!(r_class.as_deref(), Some("Mojolicious::Routes"));
    assert!(
        get_invocant_class.is_some(),
        "(link 2) ->get's invocant class must resolve; got None"
    );
    assert!(
        get_return_ty.is_some(),
        "(link 3) ->get's RETURN type must resolve through \
             _generate_route → _route's plugin override"
    );
    assert_eq!(
        get_return_ty.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "->get returns the Route class so ->to can chain off it"
    );
    assert!(
        to_invocant_class.is_some(),
        "(link 4) ->to's invocant class must resolve — THIS is \
             the chain hop the spike was unblocking"
    );
    assert_eq!(
        to_invocant_class.as_deref(),
        Some("Mojolicious::Routes::Route"),
        "->to is invoked on a Route, so cursor-on-`to` \
                    completion / hover / goto-def all reach \
                    Mojolicious::Routes::Route::to"
    );

    // Cross-check the cached symbols: every verb method
    // (get/post/put/etc.) tail-delegates through _generate_route,
    // and _generate_route's body folds via the chain typer +
    // refreshed return-arm typing.
    assert_eq!(
        _route_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "_route is the override anchor",
    );
    assert_eq!(
        gen_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "_generate_route folds because $route is now typed",
    );
    assert_eq!(
        get_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "get tail-delegates to _generate_route which has a type",
    );
}

/// E2E: the motivator. `$r->get('/x')->|` at the cursor — the
/// public `completion_items` API must offer methods from the
/// route class (Route::to, Route::name, etc.), proving the
/// witness-bag-driven chain typing works all the way through
/// CursorContext → resolve_node_type → resolve_expression_type →
/// find_method_return_type → complete_methods_for_class.
///
/// No special casing. Zero hardcoded chain rules. If this
/// passes, the mojo-demo `$r->get('/x')->to(...)` gets
/// "intellismarts" on `->to` through witness flow.
#[test]
fn test_e2e_mojo_style_chain_completion_offers_chained_class_methods() {
    let src = r#"package MyApp::Route;
sub new { my $c = shift; bless {}, $c }
sub get {
    my $self = shift;
    $self->{_path} = shift;
    return $self;
}
sub to {
    my $self = shift;
    $self->{_target} = shift;
    return $self;
}
sub name {
    my $self = shift;
    $self->{_name} = shift;
    return $self;
}

package main;
my $r = MyApp::Route->new;
$r->get('/users')->
"#;
    let analysis = parse_analysis(src);
    let tree = crate::index::document::Document::new(src.to_string())
        .unwrap()
        .tree;
    let idx = ModuleIndex::new_for_test();

    // Cursor right after the trailing `->` on the chain line.
    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("$r->get('/users')->"))
        .unwrap();
    let col = line.rfind("->").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for expected in &["to", "name", "get"] {
        assert!(
            labels.contains(expected),
            "expected `{}` in completion after `$r->get('/users')->`, \
                 got {} items: {:?}",
            expected,
            labels.len(),
            labels
        );
    }
}

/// Pin: a Mojo helper registered in one file (`$app->helper(widget => ...)`)
/// must be reachable by goto-definition from `$c->widget` in ANOTHER file.
///
/// The provider's `mojo-helpers` synthesis bridges `widget` to the app
/// surface; the consumer's `$c` is a controller subclass that reaches the
/// surface via the synthetic-parent edge. `resolve_method_in_ancestors`
/// already finds the bridged symbol, but if its `CrossFile` result only
/// carries the class name, the goto-def consumer re-looks-up the method in
/// the bridged class's OWN module (where it doesn't exist) and the jump is
/// lost. Same-file works; this is the cross-file hole that had no coverage.
#[test]
fn cross_file_plugin_helper_goto_def_resolves() {
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(widget => sub ($c) { return Widget->new; });\n\
}\n\
1;\n";
    let provider = parse_analysis(provider_src);
    // Sanity: the provider really did synthesize a `widget` Method bridged to
    // the app surface (otherwise the test would pass vacuously once the bug is
    // "fixed" by an unrelated path).
    assert!(
        provider.plugin.namespaces.iter().any(|ns| ns
            .bridges
            .iter()
            .any(|b| matches!(b, crate::model::file_analysis::Bridge::Class(c) if c == crate::model::file_analysis::APP_SURFACE_CLASS))),
        "provider must bridge a namespace to the app surface",
    );

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let provider_path = std::path::PathBuf::from("/tmp/perl_lsp_pin_My_Plugin.pm");
    idx.register_workspace_module(provider_path.clone(), std::sync::Arc::new(provider));

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $w = $c->widget;\n\
  return $w;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();

    // Cursor on the `widget` token in `$c->widget`.
    let byte = consumer_src.find("widget;").expect("call site present");
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };

    let uri = Url::parse("file:///consumer.pl").unwrap();
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &consumer, pos, &uri, &idx);

    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => v.remove(0),
        other => panic!("expected a goto-def location for cross-file helper, got {other:?}"),
    };
    assert!(
        loc.uri.path().ends_with("My_Plugin.pm"),
        "goto-def should land in the provider file, got {}",
        loc.uri,
    );
}

/// Cross-file goto-def to a DYNAMICALLY-minted helper. The provider loops
/// a literal `qw` list and registers `$app->helper("get_$name" => sub)`
/// per element; `$c->get_order` in another file must goto-def to that
/// registration call site. Mirrors `cross_file_plugin_helper_goto_def_resolves`
/// (static widget), proving the dynamic names ride the identical app-surface
/// bridge — no parallel lookup path.
#[test]
fn cross_file_dynamic_helper_goto_def_resolves() {
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  for my $name (qw(user order invoice)) {\n\
    $app->helper(\"get_$name\" => sub ($c) { return Widget->new; });\n\
  }\n\
}\n\
1;\n";
    let provider = parse_analysis(provider_src);
    // Sanity: the dynamic loop really minted the concrete `get_order` helper,
    // bridged to the app surface (so the test can't pass vacuously).
    assert!(
        provider.symbols().iter().any(|s| s.name == "get_order"
            && matches!(&s.namespace, crate::model::file_analysis::Namespace::Framework { id } if id == "mojo-helpers")),
        "provider must mint the dynamic `get_order` helper",
    );

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let provider_path = std::path::PathBuf::from("/tmp/perl_lsp_pin_My_DynPlugin.pm");
    idx.register_workspace_module(provider_path.clone(), std::sync::Arc::new(provider));

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $w = $c->get_order;\n\
  return $w;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);

    let byte = consumer_src.find("get_order;").expect("call site present");
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };

    let uri = Url::parse("file:///consumer.pl").unwrap();
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &consumer, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => v.remove(0),
        other => panic!("expected goto-def for cross-file dynamic helper, got {other:?}"),
    };
    assert!(
        loc.uri.path().ends_with("My_DynPlugin.pm"),
        "goto-def should land in the provider file, got {}",
        loc.uri,
    );
    // Lands on the registration loop line (`$app->helper("get_$name" => …)`,
    // line 4, 0-based), the provenance anchor for every minted helper.
    assert_eq!(
        loc.range.start.line, 4,
        "goto-def should land on the registration call, got {}",
        loc.range.start.line,
    );
}

/// CG-3b cross-package glob attribution, cross-file: `DateTime::PP`
/// installs `_ymd2rd` into `DateTime` via
/// `*{ 'DateTime::' . $sub } = __PACKAGE__->can($sub)`. A `$self->_ymd2rd`
/// call in `DateTime` (a different file) must goto-def to the real sub in
/// PP.pm — the method is a DateTime method even though it's declared in a
/// differently-named module file.
#[test]
fn cross_package_glob_method_resolves_cross_file() {
    let provider_src = "package DateTime::PP;\n\
sub _ymd2rd { my ($class, $y, $m, $d) = @_; return 1; }\n\
my @subs = qw( _ymd2rd );\n\
for my $sub (@subs) {\n\
  no strict 'refs';\n\
  *{ 'DateTime::' . $sub } = __PACKAGE__->can($sub);\n\
}\n\
1;\n";
    let provider = parse_analysis(provider_src);
    // Sanity: the glob really attributed `_ymd2rd` to DateTime.
    assert!(
        provider.symbols().iter().any(|s| s.name == "_ymd2rd"
            && matches!(s.kind, crate::model::file_analysis::SymKind::Sub)
            && s.package.as_deref() == Some("DateTime")),
        "provider must attribute the glob-installed _ymd2rd to DateTime",
    );

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let provider_path = std::path::PathBuf::from("/tmp/perl_lsp_pin_DateTime_PP.pm");
    idx.register_workspace_module(provider_path.clone(), std::sync::Arc::new(provider));

    let consumer_src = "package DateTime;\n\
sub new { my $class = shift; return bless {}, $class; }\n\
sub day_of_week {\n\
  my $self = shift;\n\
  return $self->_ymd2rd( 2024, 1, 1 );\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();

    let byte = consumer_src.find("_ymd2rd( 2024").expect("call site present");
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };

    let uri = Url::parse("file:///datetime.pl").unwrap();
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &consumer, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => v.remove(0),
        other => panic!("expected goto-def for cross-package glob method, got {other:?}"),
    };
    assert!(
        loc.uri.path().ends_with("DateTime_PP.pm"),
        "goto-def should land in the provider (PP) file, got {}",
        loc.uri,
    );
    assert_eq!(
        loc.range.start.line, 1,
        "should land on the real `sub _ymd2rd` (line 1), got {}",
        loc.range.start.line
    );
}

/// Fully-qualified variable read across files: `$My::Vars::config` in one
/// file must goto-def to `our $config` in My::Vars (another module). Mirrors
/// the FQ-call cross-file path via `qualified_var_target()` + module_index.
#[test]
fn fq_variable_read_resolves_cross_file() {
    let provider_src = "package My::Vars;\n\
our $config = { host => 'localhost' };\n\
our @servers = ('a', 'b');\n\
1;\n";
    let provider = parse_analysis(provider_src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let provider_path = std::path::PathBuf::from("/tmp/perl_lsp_pin_My_Vars.pm");
    idx.register_workspace_module(provider_path, std::sync::Arc::new(provider));

    let consumer_src = "package Main;\n\
my $h = $My::Vars::config;\n\
my @s = @My::Vars::servers;\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();

    // Cursor on the `config` tail of `$My::Vars::config`.
    let byte = consumer_src.find("config;").expect("read site present");
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };

    let uri = Url::parse("file:///consumer.pl").unwrap();
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &consumer, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        other => panic!("expected goto-def for FQ var, got {other:?}"),
    };
    assert!(
        loc.uri.path().ends_with("My_Vars.pm"),
        "goto-def should land in the provider file, got {}",
        loc.uri,
    );
    assert_eq!(
        loc.range.start.line, 1,
        "should land on `our $config` (line 1)"
    );
}

/// Honest miss: an FQ read into an unknown package must not fabricate a jump.
#[test]
fn fq_variable_read_unknown_package_is_honest_miss() {
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let consumer_src = "package Main;\nmy $x = $No::Such::Pkg::thing;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();
    let byte = consumer_src.find("thing;").unwrap();
    let prefix = &consumer_src[..byte];
    let pos = Position {
        line: prefix.matches('\n').count() as u32,
        character: (byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };
    let uri = Url::parse("file:///consumer.pl").unwrap();
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &consumer, pos, &uri, &idx);
    assert!(resp.is_none(), "unknown package must be an honest miss, got {resp:?}");
}

/// Pin: cross-file hover on a plugin-synthesized helper. Same shape as
/// `cross_file_plugin_helper_goto_def_resolves` but exercises the hover
/// consumer arm, which shared the same lossy `get_cached(class).sub_info`
/// bug (looking the bridged helper up in the class's own module). Hover
/// needs the symbol's signature, not just a location, so this guards the
/// unified `def_module` path end-to-end.
#[test]
fn cross_file_plugin_helper_hover_resolves() {
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(widget => sub ($c) { return Widget->new; });\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_hover_My_Plugin.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $w = $c->widget;\n\
  return $w;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();

    let byte = consumer_src.find("widget;").expect("call site present");
    let prefix = &consumer_src[..byte];
    let point = tree_sitter::Point {
        row: prefix.matches('\n').count(),
        column: byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0),
    };

    let hover = consumer
        .hover_info(point, consumer_src, Some(&idx))
        .expect("cross-file hover should resolve the bridged helper");
    assert!(hover.contains("widget"), "hover should mention the helper, got: {hover}");
    // The helper now lives on the fictional app surface; the controller
    // subclass reaches it through the synthetic-parent edge. Hover shows
    // where the symbol is bridged from.
    assert!(
        hover.contains(crate::model::file_analysis::APP_SURFACE_CLASS),
        "hover should show the app surface as the bridging class, got: {hover}",
    );
}

/// Pin: cross-file hover return type that REQUIRES the module index — the
/// helper returns `Other->new->fluent`, so typing the return means recursing
/// into `My::Other`'s module to resolve `fluent`. With `module_index: None`
/// in the return-type query (the old behavior) this came back untyped; the
/// `_ctx` threading lights it up. Guards the return-position cross-file chain.
#[test]
fn cross_file_helper_return_type_needs_module_index() {
    let other_src = "package My::Other;\n\
use Mojo::Base -base;\n\
sub fluent ($self) { return $self; }\n\
1;\n";
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(thing => sub ($c) { return My::Other->new->fluent; });\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_ret_Other.pm"),
        std::sync::Arc::new(parse_analysis(other_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_ret_Plugin.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $x = $c->thing;\n\
  return $x;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();
    let byte = consumer_src.find("thing;").expect("call site");
    let prefix = &consumer_src[..byte];
    let point = tree_sitter::Point {
        row: prefix.matches('\n').count(),
        column: byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0),
    };

    let hover = consumer
        .hover_info(point, consumer_src, Some(&idx))
        .expect("cross-file hover should resolve");
    assert!(
        hover.contains("My::Other"),
        "return type should resolve cross-file to My::Other, got: {hover}",
    );
}

/// Like `cross_file_helper_return_type_needs_module_index`, but the helper
/// routes the value through a LEXICAL (`my $g = chain; return $g`) — the common
/// `my $x = …; return $x` shape. This is the end-to-end pin for unifying
/// `Variable` into the canonical edge chase: the variable query path now
/// carries the `module_index`, the cross-file chain assignment is stored as
/// `Variable($g) → Edge(Expr(rhs))` ("Edges, not values"), and point-narrowing
/// no longer wrongly filters the materialized `Expression` witness.
#[test]
fn cross_file_lexical_chain_return_type() {
    let other_src = "package My::Other;\n\
use Mojo::Base -base;\n\
sub fluent ($self) { return $self; }\n\
1;\n";
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(thing => sub ($c) { my $g = My::Other->new->fluent; return $g; });\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_lex_Other.pm"),
        std::sync::Arc::new(parse_analysis(other_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_lex_Plugin.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $x = $c->thing;\n\
  return $x;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();
    let byte = consumer_src.find("thing;").expect("call site");
    let prefix = &consumer_src[..byte];
    let point = tree_sitter::Point {
        row: prefix.matches('\n').count(),
        column: byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0),
    };

    let hover = consumer
        .hover_info(point, consumer_src, Some(&idx))
        .expect("cross-file hover should resolve");
    assert!(
        hover.contains("My::Other"),
        "lexical-chain return type should resolve cross-file to My::Other, got: {hover}",
    );
}

/// The fluent-accessor sibling of `cross_file_helper_return_type_needs_module_index`:
/// the helper returns `My::Other->new->acc($x)` where `acc` is a Mojo::Base `has`
/// accessor (fluent setter — returns the invocant), NOT a plain `return $self` sub.
/// The fluent return is modeled as `ReturnExpr(Receiver)`, so its resolution
/// substitutes the query's receiver. Before the receiver-reset fix, the consumer's
/// call-site receiver (`Mojolicious::Controller`, the type of `$c`) leaked down the
/// edge chase into `PackageSymbol{My::Other, acc}` and got substituted there. The
/// receiver for a method-call return must be that call's invocant (`My::Other`).
#[test]
fn cross_file_fluent_accessor_chain_return_type() {
    let other_src = "package My::Other;\n\
use Mojo::Base -base;\n\
has acc => sub { {} };\n\
1;\n";
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(thing => sub ($c) { return My::Other->new->acc($x); });\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_acc_Other.pm"),
        std::sync::Arc::new(parse_analysis(other_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_acc_Plugin.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $x = $c->thing;\n\
  return $x;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();
    let byte = consumer_src.find("thing;").expect("call site");
    let prefix = &consumer_src[..byte];
    let point = tree_sitter::Point {
        row: prefix.matches('\n').count(),
        column: byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0),
    };

    let hover = consumer
        .hover_info(point, consumer_src, Some(&idx))
        .expect("cross-file hover should resolve");
    assert!(
        hover.contains("My::Other"),
        "fluent-accessor chain return type should resolve to My::Other (not the \
         consumer's call-site receiver), got: {hover}",
    );
}

/// The MRO subtlety of the receiver-reset fix: a fluent `has` accessor declared
/// on a PARENT but dispatched on a CHILD (`Child->new->acc($x)`) must return the
/// *child*, not the class where `has` was declared. The receiver reset keys on the
/// dispatch class (`PackageSymbol{Child}` → receiver `Child`); the inheritance hop
/// to `PackageSymbol{Parent, acc}` must then carry that child receiver through so
/// the parent's `ReturnExpr(Receiver)` substitutes `Child`.
#[test]
fn cross_file_inherited_fluent_accessor_returns_child() {
    let base_src = "package My::Base;\n\
use Mojo::Base -base;\n\
has acc => sub { {} };\n\
1;\n";
    let child_src = "package My::Child;\n\
use Mojo::Base 'My::Base';\n\
1;\n";
    let provider_src = "package My::Plugin;\n\
use Mojo::Base 'Mojolicious::Plugin';\n\
sub register ($self, $app, $conf) {\n\
  $app->helper(thing => sub ($c) { return My::Child->new->acc($x); });\n\
}\n\
1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_inh_Base.pm"),
        std::sync::Arc::new(parse_analysis(base_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_inh_Child.pm"),
        std::sync::Arc::new(parse_analysis(child_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/perl_lsp_pin_inh_Plugin.pm"),
        std::sync::Arc::new(parse_analysis(provider_src)),
    );

    let consumer_src = "package My::Ctrl;\n\
use Mojo::Base 'Mojolicious::Controller';\n\
sub action ($c) {\n\
  my $x = $c->thing;\n\
  return $x;\n\
}\n\
1;\n";
    let consumer = parse_analysis(consumer_src);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(consumer_src, None).unwrap();
    let byte = consumer_src.find("thing;").expect("call site");
    let prefix = &consumer_src[..byte];
    let point = tree_sitter::Point {
        row: prefix.matches('\n').count(),
        column: byte - prefix.rfind('\n').map(|i| i + 1).unwrap_or(0),
    };

    let hover = consumer
        .hover_info(point, consumer_src, Some(&idx))
        .expect("cross-file hover should resolve");
    assert!(
        hover.contains("My::Child"),
        "inherited fluent accessor must return the dispatch (child) class, got: {hover}",
    );
}

/// Spike: Mojo partial route targets inherit the controller down the
/// route-builder chain via a value brand (`InferredType::BrandedRoute`).
/// See `docs/adr/route-branding.md` (option C, collapsed:
/// resolved defaults ride the type, no separate brand-id/side-table).
///
/// The brand carries the inherited `->to('ctrl#')` controller and rides
/// the chain through assignment (`my $alerts_r = ...`), method chaining
/// (`->get('/')->to`), and nesting (`$alerts_r->under(...)` → `$crud`).
/// A partial `->to('#action')` reads the inherited controller off the
/// receiver's brand; a sibling group with its own `->to('other#')`
/// re-brands its descendants without leaking.
///
/// Controller-token → class mapping (camelize + workspace search) is
/// orthogonal and decided elsewhere; here the controller packages are
/// named to match the raw token so goto-def resolves end to end without
/// that layer. The route class's fluent verbs return `$self` so the
/// chain types as `Mojolicious::Routes::Route` locally.
#[test]
fn brand_partial_route_targets_inherit_controller() {
    let src = r#"package Mojolicious::Routes::Route;
sub new { my $class = shift; return bless {}, $class; }
sub any { my $self = shift; return $self; }
sub get { my $self = shift; return $self; }
sub under { my $self = shift; return $self; }
sub to { my $self = shift; return $self; }

package Alerts;
sub list { my $c = shift; }
sub get_alert { my $c = shift; }
sub read_settings { my $c = shift; }

package Other;
sub thing { my $c = shift; }

package MyApp;
use Mojolicious::Lite;
sub startup {
  my $self = shift;
  my $r = Mojolicious::Routes::Route->new;
  my $alerts_r = $r->any('/alerts')->to('alerts#', section => 'admin');
  $alerts_r->get('/')->to('#list');
  my $crud = $alerts_r->under('/:type')->to('#get_alert');
  $crud->get('/settings')->to('#read_settings');
  my $other_r = $r->any('/other')->to('other#');
  $other_r->get('/x')->to('#thing');
}
1;
"#;
    let fa = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();

    let _ = &idx;
    // Helper: the plugin-emitted MethodCallRef for a partial target
    // carries the inherited controller as its bridged TOKEN (not a frozen
    // class — resolution to a class is the plugin's query-time job, which
    // needs a populated index; brand inheritance is what's under test).
    let inherited = |action: &str| -> Option<String> {
        fa.refs().iter().find_map(|r| {
            if let crate::model::file_analysis::RefKind::MethodCall {
                invocant: crate::model::conventions::Invocant::Bridged { token, .. },
                ..
            } = &r.kind
            {
                if r.target_name == action {
                    return Some(token.clone());
                }
            }
            None
        })
    };

    // Direct child: `$alerts_r->get('/')->to('#list')` inherits 'alerts',
    // emitted camelized (`Alerts`) — the plugin applies the convention.
    assert_eq!(inherited("list").as_deref(), Some("Alerts"),
        "partial '#list' must inherit the parent's 'alerts' controller");
    // Nested via `under`: `$crud` inherits 'alerts' from `$alerts_r`.
    assert_eq!(inherited("get_alert").as_deref(), Some("Alerts"),
        "partial '#get_alert' on $crud (under $alerts_r) inherits 'alerts'");
    // Two hops deep: `$crud->get('/settings')->to('#read_settings')`.
    assert_eq!(inherited("read_settings").as_deref(), Some("Alerts"),
        "nested partial '#read_settings' still inherits 'alerts'");
    // Sibling group re-brands; no leak from 'alerts'.
    assert_eq!(inherited("thing").as_deref(), Some("Other"),
        "sibling group's '#thing' inherits 'other', not 'alerts'");

    // The brand rides assignment + nesting: $alerts_r and $crud both
    // carry the inherited controller in their type.
    let ty_at = |needle: &str, var: &str| -> Option<crate::model::file_analysis::InferredType> {
        let at = src.find(needle).unwrap();
        let pre = &src[..at];
        let pt = tree_sitter::Point {
            row: pre.matches('\n').count(),
            column: at - pre.rfind('\n').map(|i| i + 1).unwrap_or(0),
        };
        fa.inferred_type_via_bag(var, pt)
    };
    assert!(
        matches!(ty_at("$alerts_r->get", "$alerts_r"),
            Some(crate::model::file_analysis::InferredType::BrandedRoute { ref controller, .. })
                if controller.as_deref() == Some("alerts")),
        "$alerts_r must type as a BrandedRoute carrying controller='alerts'");
    assert!(
        matches!(ty_at("$crud->get", "$crud"),
            Some(crate::model::file_analysis::InferredType::BrandedRoute { ref controller, .. })
                if controller.as_deref() == Some("alerts")),
        "$crud (nested under $alerts_r) inherits the 'alerts' brand");

    // Stash (beyond controller/action) rides the brand too: the
    // `section => 'admin'` default set on $alerts_r is queryable via
    // the rule-#10 `route_default` accessor on a descendant's value.
    assert_eq!(
        ty_at("$crud->get", "$crud").as_ref().and_then(|t| t.route_default("section")),
        Some("admin"),
        "inherited stash default 'section' is readable off $crud's brand");
    assert_eq!(
        ty_at("$crud->get", "$crud").as_ref().and_then(|t| t.route_default("controller")),
        Some("alerts"),
        "route_default('controller') reads the distinguished controller key");

    // End-to-end goto-def: cursor on the `list` action inside
    // `->to('#list')` resolves to `alerts::list` (here the controller
    // token maps directly to the package).
    let uri = Url::parse("file:///app.pl").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let _tree = parser.parse(src, None).unwrap();
    let list_at = src.find("'#list'").unwrap() + "'#".len();
    let pre = &src[..list_at];
    let pos = Position {
        line: pre.matches('\n').count() as u32,
        character: (list_at - pre.rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32,
    };
    let resp = find_definition(&crate::index::file_store::FileStore::new(), &fa, pos, &uri, &idx);
    let loc = match resp {
        Some(GotoDefinitionResponse::Scalar(loc)) => loc,
        Some(GotoDefinitionResponse::Array(mut v)) if !v.is_empty() => v.remove(0),
        other => panic!("expected goto-def on partial '#list', got {other:?}"),
    };
    // `sub list` is declared on line index 9 (0-based) — `package alerts;`
    // block. Just assert it landed on the `list` sub's line, not on the
    // app's route line.
    let list_line = src[..src.find("sub list").unwrap()].matches('\n').count() as u32;
    assert_eq!(loc.range.start.line, list_line,
        "goto-def on '#list' lands on `sub list` in the alerts controller");
}

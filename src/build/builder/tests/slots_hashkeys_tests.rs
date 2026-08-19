use super::*;

// ---- ${@} block-interpolation token-stream bleed recovery (TASK-C / G1) ----
//
// `"${@}"` (the `@` sigil inside `${...}`) mis-lexes: the string's closing
// quote is swallowed, wrapping the rest of the file in an ERROR and dissolving
// every following `sub` into stray tokens — they survive NOWHERE in the tree.
// Source-text recovery inside the ERROR span restores them. See
// docs/parser-shortcomings.md (G1) and docs/adr/error-recovery.md.

fn sub_names(fa: &FileAnalysis) -> Vec<String> {
    fa.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        .map(|s| s.name.clone())
        .collect()
}

#[test]
fn dollar_at_block_interp_bleed_recovers_following_subs() {
    let src = r#"package Foo;
my $x = "err ${@} more text here";
sub alpha { return 1; }
sub beta { return 2; }
sub gamma { my $self = shift; return $self; }
1;
"#;
    let fa = build_fa(src);
    // The bleed produces zero subroutine_declaration_statement nodes; without
    // text recovery this asserts 0 subs.
    let names = sub_names(&fa);
    for want in ["alpha", "beta", "gamma"] {
        assert!(
            names.iter().any(|n| n == want),
            "sub `{want}` must survive the ${{@}} bleed; recovered: {names:?}"
        );
    }
}

#[test]
fn dollar_at_block_interp_recovered_sub_has_correct_position() {
    let src = r#"package Foo;
my $x = "err ${@}";
sub alpha { return 1; }
1;
"#;
    let fa = build_fa(src);
    let alpha = fa
        .symbols()
        .iter()
        .find(|s| s.name == "alpha" && matches!(s.kind, SymKind::Sub))
        .expect("alpha recovered");
    // `sub alpha` is on row 2 (0-based), name token at column 4.
    assert_eq!(alpha.selection_span.start.row, 2, "alpha row");
    assert_eq!(alpha.selection_span.start.column, 4, "alpha name column");
    assert_eq!(alpha.package.as_deref(), Some("Foo"), "alpha package");
}

#[test]
fn dollar_at_block_interp_bleed_keeps_package() {
    // The package statement precedes the bleed and must still be indexed so the
    // recovered subs key under the right package.
    let src = r#"package Net::DNS::RR;
my $e = "${@}in $stmnt\n";
sub new { }
sub decode { }
sub encode { }
1;
"#;
    let fa = build_fa(src);
    assert!(
        fa.symbols()
            .iter()
            .any(|s| s.name == "Net::DNS::RR" && matches!(s.kind, SymKind::Package)),
        "package survives the bleed"
    );
    for want in ["new", "decode", "encode"] {
        assert!(
            fa.symbols().iter().any(|s| s.name == want
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && s.package.as_deref() == Some("Net::DNS::RR")),
            "sub `{want}` recovered under Net::DNS::RR"
        );
    }
}

#[test]
fn normal_parse_unaffected_by_error_text_recovery() {
    // Regression: text recovery only runs inside ERROR spans, so a clean file
    // must produce exactly the structurally-parsed subs with no duplicates.
    let src = r#"package Foo;
sub one { 1 }
sub two { 2 }
1;
"#;
    let fa = build_fa(src);
    let mut names = sub_names(&fa);
    names.sort();
    assert_eq!(names, vec!["one".to_string(), "two".to_string()]);
}

#[test]
fn error_text_recovery_does_not_duplicate_a_recovered_sub() {
    // A sub inside an ERROR region must be recovered exactly once even when the
    // structural loop and the text scan could fire on overlapping spans (the
    // row-based dedup guards this). `if (` wraps the trailing sub in an ERROR;
    // the sub mis-parses, so the text scan recovers it — and must do so once.
    let src = "package Foo;\nif (\nsub kept { 1 }\n";
    let fa = build_fa(src);
    let kept: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "kept" && matches!(s.kind, SymKind::Sub | SymKind::Method))
        .collect();
    assert_eq!(kept.len(), 1, "no duplicate `kept`: {kept:?}");
}

// ---- Typed-slot witness (SlotType) ----
//
// These exercise the typed half of the hash-key-write seed in isolation:
// build a fixture, then query the `SlotType{class, key}` attachment
// through the registry. Nothing in the server consumes this attachment
// yet (typed `$obj->{k}->m()` resolution is a later step), so the
// registry query IS the whole validation surface.

fn slot_type(fa: &FileAnalysis, class: &str, key: &str) -> Option<InferredType> {
    use crate::model::witnesses::{
        BagContext, FrameworkFact, ReducedValue, ReducerQuery, ReducerRegistry, WitnessAttachment,
    };
    let att = WitnessAttachment::SlotType {
        class: class.to_string(),
        key: key.to_string(),
    };
    let ctx = BagContext {
        scopes: &fa.scopes,
        package_framework: &fa.packages,
        module_index: None,
        package_parents: &fa.packages,
        app_surface_consumers: &fa.plugin.app_surface_consumers,
    };
    let q = ReducerQuery {
        args: Vec::new(),        attachment: &att,
        point: None,
        framework: FrameworkFact::Plain,
        arity_hint: None,
        receiver: None,
        context: Some(&ctx),
    };
    let reg = ReducerRegistry::with_defaults();
    match reg.query(&fa.witnesses, &q) {
        ReducedValue::Type(t) => Some(t),
        ReducedValue::FactMap(_) | ReducedValue::None => None,
    }
}

#[test]
fn slot_type_single_typed_write() {
    let src = "package Foo;\nsub init {\n  my $self = shift;\n  $self->{h} = Helper->new;\n}\n";
    let fa = build_fa(src);
    let t = slot_type(&fa, "Foo", "h").expect("SlotType{Foo,h} should fold");
    assert_eq!(t.class_name(), Some("Helper"), "got {t:?}");
}

#[test]
fn slot_type_two_agreeing_writes() {
    let src = "package Foo;\nsub a {\n  my $self = shift;\n  $self->{h} = Helper->new;\n}\nsub b {\n  my $self = shift;\n  $self->{h} = Helper->new;\n}\n";
    let fa = build_fa(src);
    let t = slot_type(&fa, "Foo", "h").expect("agreeing writes fold to the agreed type");
    assert_eq!(t.class_name(), Some("Helper"), "got {t:?}");
}

#[test]
fn slot_type_two_disagreeing_writes_none() {
    let src = "package Foo;\nsub a {\n  my $self = shift;\n  $self->{h} = Helper->new;\n}\nsub b {\n  my $self = shift;\n  $self->{h} = Other->new;\n}\n";
    let fa = build_fa(src);
    // Disagreeing writes → honest None (no guess).
    assert_eq!(slot_type(&fa, "Foo", "h"), None);
}

#[test]
fn slot_type_unknown_rhs_no_slot() {
    // `= shift` / `= $param` carry no resolvable type — no SlotType seed,
    // never a guess.
    let src = "package Foo;\nsub init {\n  my $self = shift;\n  my $param = shift;\n  $self->{h} = $param;\n}\n";
    let fa = build_fa(src);
    assert_eq!(slot_type(&fa, "Foo", "h"), None);
}

#[test]
fn slot_type_keyed_by_owner_class() {
    // `$o->{h}` where `$o` is a typed local `Foo` keys the slot by the
    // OWNER's class, distinct from `$self->{h}` of the enclosing package.
    let src = "package Bar;\nsub mk {\n  my $self = shift;\n  my $o = Foo->new;\n  $o->{h} = Helper->new;\n  $self->{h} = Sidecar->new;\n}\n";
    let fa = build_fa(src);
    let foo_h = slot_type(&fa, "Foo", "h").expect("SlotType keyed by owner class Foo");
    assert_eq!(foo_h.class_name(), Some("Helper"), "got {foo_h:?}");
    // The enclosing-package write lands on Bar, not Foo — no cross-contamination.
    let bar_h = slot_type(&fa, "Bar", "h").expect("SlotType{Bar,h} from $self write");
    assert_eq!(bar_h.class_name(), Some("Sidecar"), "got {bar_h:?}");
}


#[test]
fn test_braced_invocant_bless_is_receiver_poly() {
    // The braced spelling `${self}` / `${class}` must be recognized as the
    // receiver-polymorphic ctor idiom (canonical varname, not raw `$self` text).
    let fa = build_fa(
        "package Base;\nsub new { my $class = shift; bless {}, ref ${class} || ${class} }\npackage Child;\nuse parent -norequire, 'Base';\n",
    );
    assert_eq!(
        fa.find_method_return_type("Child", "new", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "braced-self inherited ctor must type Child->new as Child"
    );
    // a real deref `bless {}, ${$ref}` is NOT receiver-poly -> not Child
    let fa2 = build_fa(
        "package Base;\nsub new { my $ref = \\'X'; bless {}, ${$ref} }\npackage Child;\nuse parent -norequire, 'Base';\n",
    );
    assert_ne!(
        fa2.find_method_return_type("Child", "new", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "a sigil-deref bless target must NOT be treated as the receiver"
    );
}

#[test]
fn test_super_new_types_to_calling_class() {
    // `$self->SUPER::new` looks `new` up on the parent (`Base`), but `Base::new`
    // is receiver-polymorphic (`bless {}, ref $class || $class`), so it blesses
    // into the SUBCLASS — `Child::new` must return `Child`, not `Base`. And a
    // `clone` that calls `$self->new` composes through the SUPER hop back to
    // `Child`.
    let fa = build_fa(
        "package Base;\nsub new { my $class = shift; bless {}, ref $class || $class }\nsub parse { $_[0] }\npackage Child;\nuse parent -norequire, 'Base';\nsub new { my $self = shift; @_ > 1 ? $self->SUPER::new->parse(@_) : $self->SUPER::new }\nsub clone { my $self = shift; my $c = $self->new; @$c{qw(a)} = (1); return $c }\n",
    );
    assert_eq!(
        fa.find_method_return_type("Child", "new", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "SUPER::new on a receiver-polymorphic parent ctor blesses into the subclass"
    );
    assert_eq!(
        fa.find_method_return_type("Child", "clone", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "clone's $self->new composes through the SUPER hop back to the subclass"
    );
}

#[test]
fn test_fq_method_call_dispatches_from_named_class() {
    // `$obj->Maker::build()` is a fully-qualified method call: Perl dispatches
    // `build` from `Maker`, NOT from the invocant's class. `Maker::build` is
    // receiver-polymorphic (returns the invocant's class), so the FQ call types
    // to `Invoker` (the invocant). If we wrongly dispatched from the invocant's
    // class, we'd pick `Invoker::build` → `Numeric` — so asserting `Invoker`
    // also proves the named class won.
    let fa = build_fa(
        "package Maker;\nsub build { my $class = shift; return bless {}, ref $class || $class }\npackage Invoker;\nsub new { my $c = shift; return bless {}, ref $c || $c }\nsub build { return 42 }\npackage main;\nmy $obj = Invoker->new;\nmy $r = $obj->Maker::build();\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$r", Point::new(7, 4)),
        Some(InferredType::ClassName("Invoker".into())),
        "FQ call dispatches build from Maker (receiver-poly → invocant), not from Invoker"
    );
}

#[test]
fn test_bless_return_strands_class_arg_recovered() {
    // tree-sitter-perl strands the class arg of `return bless {BLOCK}, CLASS`
    // (the brace block greedily ends the parenless call). We splice it back, so
    // the foreign literal class is honored instead of falling to the enclosing
    // package.
    let lit = build_fa("package P;\nsub make { return bless {}, 'Widget' }\n");
    assert_eq!(
        lit.find_method_return_type("P", "make", None, Some(0)),
        Some(InferredType::ClassName("Widget".into())),
        "return bless {{}}, 'Widget' must type to Widget, not the enclosing package"
    );
    // The receiver-polymorphic spelling with `return` is the common inherited
    // ctor — recovery makes it ReceiverOr so a subclass types to itself.
    let poly = build_fa(
        "package Base;\nsub new { my $class = shift; return bless {}, ref $class || $class }\npackage Child;\nuse parent -norequire, 'Base';\nsub make { my $self = shift; return $self->new }\n",
    );
    assert_eq!(
        poly.find_method_return_type("Child", "make", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "inherited receiver-poly ctor (return bless {{}}, ref $class || $class) types to the subclass"
    );
}

#[test]
fn test_bless_positional_self_is_receiver() {
    // `$_[0]` is the positional spelling of the invocant — a receiver-poly ctor
    // written `bless {}, ref $_[0] || $_[0]` types to the calling subclass.
    let fa = build_fa(
        "package Base;\nsub new { return bless {}, ref $_[0] || $_[0] }\npackage Child;\nuse parent -norequire, 'Base';\nsub make { my $self = shift; return $self->new }\n",
    );
    assert_eq!(
        fa.find_method_return_type("Child", "make", None, Some(0)),
        Some(InferredType::ClassName("Child".into())),
        "bless {{}}, ref $_[0] || $_[0] is receiver-polymorphic via the positional self"
    );
}

/// `${sner}->thing` is `$sner->thing` — the grammar's `varname` child
/// excludes the braces, and the ref records the canonical sigiled name so
/// invocant-class resolution hits the variable's bag key. A deref block
/// (`${$ref}`) has no bare varname and must keep its raw text (no false
/// canonicalization).
#[test]
fn braced_scalar_invocant_canonicalizes_and_resolves() {
    let src = "\
package main;
my $sner = Foo->new;
${sner}->thing;
my $ref = \\$sner;
${$ref}->other;
";
    let fa = build_fa(src);
    let thing = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "thing")
        .expect("MethodCall ref for thing");
    let RefKind::MethodCall { ref invocant, .. } = thing.kind else {
        panic!("expected MethodCall, got {:?}", thing.kind);
    };
    assert_eq!(invocant.text(), "$sner");
    assert_eq!(
        fa.method_call_invocant_class(thing, None).as_deref(),
        Some("Foo"),
        "braced spelling resolves through the variable's type",
    );

    let other = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "other")
        .expect("MethodCall ref for other");
    let RefKind::MethodCall { ref invocant, .. } = other.kind else {
        panic!("expected MethodCall, got {:?}", other.kind);
    };
    assert_eq!(invocant.text(), "${$ref}", "deref block keeps raw text");
}

/// `my $c = 'Counter'; $c->bump` — a scalar invocant holding a const-folded
/// string dispatches on that class (the same fold dynamic method names use,
/// on the other slot of the arrow). Walk-time `invocant_class` pins it, so
/// class-scoped refs/rename see the call without inference.
#[test]
fn const_folded_scalar_invocant_pins_class() {
    let src = "\
package Counter;
sub bump { 1 }
package main;
my $c = 'Counter';
$c->bump;
";
    let fa = build_fa(src);
    let bump = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "bump" && matches!(r.kind, RefKind::MethodCall { .. }))
        .expect("MethodCall ref for bump");
    assert_eq!(
        fa.method_call_invocant_class(bump, None).as_deref(),
        Some("Counter"),
        "const-folded invocant should dispatch on Counter",
    );
}

/// `field $x :param :reader` is one renameable entity: the field variable,
/// the constructor key, and the reader-method calls rewrite together,
/// from WHICHEVER spelling the cursor is on — and the `$` sigil survives
/// (edits cover only the bare name).
#[test]
fn corinna_field_group_rename_ties_all_spellings() {
    let src = "\
use v5.38;
class Point {
    field $x :param :reader;
    field $y :param;
    method magnitude () { return sqrt($x**2 + $y**2); }
}
my $p = Point->new(x => 3, y => 4);
my $val = $p->x;
";
    let fa = build_fa(src);
    let find = |row: usize, col: usize| {
        fa.rename_at(Point::new(row, col), "coord")
            .map(|mut v| {
                v.sort_by_key(|(s, _)| (s.start.row, s.start.column));
                v
            })
            .expect("rename produces edits")
    };
    // Expected spellings of `x`: field decl (2), body use (4), ctor key (6),
    // reader call (7).
    let from_decl = find(2, 11);
    let rows: Vec<usize> = from_decl.iter().map(|(s, _)| s.start.row).collect();
    assert_eq!(rows, vec![2, 4, 6, 7], "decl rename covers all spellings: {:?}", from_decl);
    // Sigil survives: the decl edit starts AFTER the `$`.
    assert_eq!(from_decl[0].0.start.column, 11);
    assert!(from_decl.iter().all(|(_, t)| t == "coord"));

    // Same union from the constructor key and from the body use.
    assert_eq!(find(6, 19), from_decl, "ctor-key rename == decl rename");
    assert_eq!(find(4, 39), from_decl, "body-use rename == decl rename");

    // `$y` is untouched by `$x`'s group.
    assert!(
        !from_decl.iter().any(|(s, _)| s.start.row == 3),
        "y's decl must not be in x's group"
    );

    // A `:param`-less field still renames as a plain group (no keys).
    let src2 = "\
use v5.38;
class Q {
    field $label = \"q\";
    method tag () { return $label; }
}
my $q = Q->new();
";
    let fa2 = build_fa(src2);
    let edits = fa2.rename_at(Point::new(2, 11), "name").expect("plain field renames");
    assert_eq!(edits.len(), 2, "decl + body use only: {:?}", edits);
}

/// Moo `has name` is the same one-entity story as a Corinna field: the
/// decl token, accessor calls, and constructor keys rename together from
/// whichever spelling the cursor is on.
#[test]
fn moo_attr_group_rename_ties_all_spellings() {
    let src = "\
package Widget;
use Moo;
has size => (is => 'ro');
sub describe { my ($self) = @_; return $self->size; }
package main;
my $w = Widget->new(size => 3);
my $s = $w->size;
";
    let fa = build_fa(src);
    let find = |row: usize, col: usize| {
        fa.rename_at(Point::new(row, col), "extent")
            .map(|mut v| {
                v.sort_by_key(|(s, _)| (s.start.row, s.start.column));
                v
            })
            .expect("rename produces edits")
    };
    // Spellings of `size`: has decl (2), accessor call in describe (3),
    // ctor key (5), accessor call (6).
    let from_decl = find(2, 5);
    let rows: Vec<usize> = from_decl.iter().map(|(s, _)| s.start.row).collect();
    assert_eq!(rows, vec![2, 3, 5, 6], "decl rename covers all spellings: {:?}", from_decl);

    assert_eq!(find(5, 21), from_decl, "ctor-key rename == decl rename");
    assert_eq!(find(3, 47), from_decl, "accessor-call rename == decl rename");
}

/// Plugin-enrolled mapped members: `predicate => 1` synthesizes
/// `has_size`, whose name DERIVES from the attr. Renaming the attr from
/// any spelling re-derives the predicate (`has_size` → `has_extent`) at
/// its call sites, and references include them. A name-mapped member
/// never double-edits the shared decl token.
#[test]
fn moo_mapped_predicate_joins_group_rename() {
    let src = "\
package Widget;
use Moo;
has size => (is => 'ro', predicate => 1);
package main;
my $w = Widget->new(size => 3);
if ($w->has_size) { print $w->size; }
";
    let fa = build_fa(src);
    let edits = fa
        .rename_at(Point::new(2, 5), "extent")
        .expect("rename produces edits");
    // The predicate call site is re-derived, not bare-replaced.
    let predicate_edit = edits
        .iter()
        .find(|(s, _)| s.start.row == 5 && s.start.column == 8)
        .expect("has_size call edited");
    assert_eq!(predicate_edit.1, "has_extent");
    // Everything else gets the bare name (decl, ctor key, accessor call).
    assert!(
        edits.iter().filter(|(_, t)| t == "extent").count() >= 3,
        "bare spellings renamed too: {:?}",
        edits,
    );
    // No span is edited twice.
    let mut spans: Vec<_> = edits.iter().map(|(s, _)| (s.start.row, s.start.column)).collect();
    spans.sort();
    spans.dedup();
    assert_eq!(spans.len(), edits.len(), "no duplicate-span edits: {:?}", edits);

    // References from the attr decl include the predicate call.
    let refs = fa.find_references(Point::new(2, 5), None);
    assert!(
        refs.iter().any(|s| s.start.row == 5 && s.start.column == 8),
        "references include has_size call: {:?}",
        refs,
    );
}

// ---- Tier 2 nested-hashkey: structurally-typed hash literals ----

/// `{ host => 'x', port => 5432 }` carries per-key types; `->{key}`
/// narrows through assignments and direct nesting; a spread flips the
/// shape open (unknown keys aren't claimable misses either way, but the
/// shape records it for future diagnostics).
#[test]
fn hash_literal_structural_typing_and_narrowing() {
    let src = "\
my $config = { db => { host => 'localhost', port => 5432 }, debug => 1 };
my $db = $config->{db};
my $host = $db->{host};
my $port = $config->{db}->{port};
my $open = { %$config, extra => 'x' };
";
    let fa = build_fa(src);

    // The literal's own structure.
    let cfg = fa
        .inferred_type_via_bag("$config", Point::new(1, 0))
        .expect("$config typed");
    let db_ty = cfg.key_value_type("db").expect("db key present").expect("db value typed");
    assert!(
        matches!(db_ty, InferredType::HashWithKeys { open: false, .. }),
        "nested literal rides the value slot: {:?}",
        db_ty,
    );
    assert!(cfg.key_value_type("typo").is_none(), "closed shape: unknown key is no key");

    // Narrowing through an assignment hop.
    let db = fa
        .inferred_type_via_bag("$db", Point::new(2, 0))
        .expect("$db typed from ->{db}");
    assert!(matches!(db, InferredType::HashWithKeys { .. }), "got {:?}", db);
    let host = fa
        .inferred_type_via_bag("$host", Point::new(3, 0))
        .expect("$host typed from ->{host}");
    assert_eq!(host, InferredType::String);

    // Direct double-drill, no intermediate variable.
    let port = fa
        .inferred_type_via_bag("$port", Point::new(4, 0))
        .expect("$port typed from ->{db}->{port}");
    assert_eq!(port, InferredType::Numeric);

    // Spread → open shape.
    let open = fa
        .inferred_type_via_bag("$open", Point::new(4, 9))
        .expect("$open typed");
    assert!(
        matches!(open, InferredType::HashWithKeys { open: true, .. }),
        "spread flips open: {:?}",
        open,
    );
}

/// Mutation extension: an unconditional `$v->{k} = …` write EXTENDS a
/// closed shape (the key joins the list, value typed from the RHS,
/// `open` preserved); a conditional or dynamic-key write switches the
/// shape open. Reads before the write keep the original shape.
#[test]
fn mutation_extension_on_closed_shapes() {
    let src = "\
my $ext = { host => 'x' };
my $before = $ext->{host};
$ext->{added} = 42;
my $after = $ext->{added};
my $cond = { host => 'x' };
$cond->{maybe} = 1 if $ENV{X};
my $dyn = { host => 'x' };
$dyn->{$ENV{K}} = 1;
";
    let fa = build_fa(src);

    // Before the write: the literal's own closed single-key shape.
    let t0 = fa.inferred_type_via_bag("$ext", Point::new(1, 0)).expect("$ext typed");
    assert!(
        matches!(&t0, InferredType::HashWithKeys { keys, open: false } if keys.len() == 1),
        "pre-write shape: {:?}",
        t0,
    );

    // After: extended, still closed, value typed from the RHS.
    let t1 = fa.inferred_type_via_bag("$ext", Point::new(3, 0)).expect("$ext typed");
    let InferredType::HashWithKeys { keys, open: false } = &t1 else {
        panic!("post-write shape: {:?}", t1)
    };
    assert_eq!(keys.len(), 2, "{:?}", keys);
    assert_eq!(keys[1].0, "added");
    assert_eq!(keys[1].1.as_deref(), Some(&InferredType::Numeric));
    let after = fa.inferred_type_via_bag("$after", Point::new(4, 0)).expect("$after typed");
    assert_eq!(after, InferredType::Numeric, "read drills the extended key");

    // Conditional write → open.
    let tc = fa.inferred_type_via_bag("$cond", Point::new(6, 0)).expect("$cond typed");
    assert!(
        matches!(tc, InferredType::HashWithKeys { open: true, .. }),
        "conditional write opens: {:?}",
        tc,
    );

    // Dynamic key → open.
    let td = fa.inferred_type_via_bag("$dyn", Point::new(8, 0)).expect("$dyn typed");
    assert!(
        matches!(td, InferredType::HashWithKeys { open: true, .. }),
        "dynamic key opens: {:?}",
        td,
    );
}

/// The literal-hash spelling: `my %h = (k => v)` types through the
/// same shape builder as the hashref literal, `$h{k}` projects off
/// `%h` (the canonical container name), mutation extension applies,
/// and spreads — arrays included (`@_`) — flip the shape open.
#[test]
fn literal_hash_structural_typing() {
    let src = "\
my %config = (host => 'x', port => 5432);
my $v = $config{host};
$config{added} = 42;
my $a = $config{added};
my %spread = (default => 1, @_);
";
    let fa = build_fa(src);
    let t = fa.inferred_type_via_bag("%config", Point::new(1, 0)).expect("%config typed");
    assert!(
        matches!(&t, InferredType::HashWithKeys { keys, open: false } if keys.len() == 2),
        "literal-list shape: {:?}",
        t,
    );
    let v = fa.inferred_type_via_bag("$v", Point::new(2, 0)).expect("$v typed");
    assert_eq!(v, InferredType::String, "container-form read projects");
    let t2 = fa.inferred_type_via_bag("%config", Point::new(3, 0)).expect("%config typed");
    assert!(
        matches!(&t2, InferredType::HashWithKeys { keys, open: false } if keys.len() == 3),
        "write extends: {:?}",
        t2,
    );
    let a = fa.inferred_type_via_bag("$a", Point::new(4, 0)).expect("$a typed");
    assert_eq!(a, InferredType::Numeric, "extended key value type");
    let sp = fa.inferred_type_via_bag("%spread", Point::new(5, 0)).expect("%spread typed");
    assert!(
        matches!(sp, InferredType::HashWithKeys { open: true, .. }),
        "array spread opens: {:?}",
        sp,
    );
}

/// Slice writes — sigil (`@h{…}`), postfix deref (`$r->@{…}`), and
/// sigil deref (`@$s{…}`) — land several keys at once: each records an
/// open-switching KeyWrite, so the closed shape widens instead of
/// claiming the written keys as misses.
#[test]
fn slice_writes_open_closed_shapes() {
    let src = "\
my %h = (a => 1);
@h{qw(b c)} = (1, 2);
my $r = { a => 1 };
$r->@{qw(d e)} = (3, 4);
my $s = { a => 1 };
@$s{qw(f g)} = (5, 6);
";
    let fa = build_fa(src);
    for (var, line) in [("%h", 2), ("$r", 4), ("$s", 6)] {
        let t = fa
            .inferred_type_via_bag(var, Point::new(line, 0))
            .unwrap_or_else(|| panic!("{var} typed"));
        assert!(
            matches!(t, InferredType::HashWithKeys { open: true, .. }),
            "slice write opens {var}: {:?}",
            t,
        );
    }
}

/// Sequence slot writes: a direct unconditional `$v->[N] = …` retypes
/// the in-bounds slot from the RHS; a write at exactly `len` appends;
/// a conditional write changes nothing (out-of-scope widening — no
/// open flag on Sequence, no array-index diagnostic to protect).
#[test]
fn sequence_index_writes_retype_and_append() {
    let src = "\
my $t = [1, 'x'];
$t->[0] = 'str';
$t->[2] = 99;
my $a = $t->[0];
my $b = $t->[2];
my $c = [1];
$c->[0] = 'maybe' if $ENV{X};
";
    let fa = build_fa(src);
    let t = fa.inferred_type_via_bag("$t", Point::new(3, 0)).expect("$t typed");
    let InferredType::Sequence(elems) = &t else { panic!("{:?}", t) };
    assert_eq!(
        elems.as_slice(),
        &[InferredType::String, InferredType::String, InferredType::Numeric],
        "slot 0 retyped, slot 2 appended",
    );
    let a = fa.inferred_type_via_bag("$a", Point::new(4, 0)).expect("$a typed");
    assert_eq!(a, InferredType::String);
    let b = fa.inferred_type_via_bag("$b", Point::new(5, 0)).expect("$b typed");
    assert_eq!(b, InferredType::Numeric);
    let c = fa.inferred_type_via_bag("$c", Point::new(7, 0)).expect("$c typed");
    let InferredType::Sequence(ce) = &c else { panic!("{:?}", c) };
    assert_eq!(ce.as_slice(), &[InferredType::Numeric], "conditional write unmodeled");
}

/// Sub-return literals narrow at call sites: `cfg()->{host}` → String.
#[test]
fn hash_literal_narrows_through_sub_return() {
    let src = "\
sub cfg { return { host => 'x', port => 1 } }
my $h = cfg()->{host};
";
    let fa = build_fa(src);
    let h = fa
        .inferred_type_via_bag("$h", Point::new(2, 0))
        .expect("$h typed through cfg()->{host}");
    assert_eq!(h, InferredType::String);
}

// ---- Tier 3 nested-hashkey: array element narrowing + mixed drill ----

/// `->[N]` projects array-literal element types (tuple semantics — the
/// heterogeneous case answers per index, better than bailing), and the
/// mixed drill `$obj->{users}->[0]->{name}` chains hash narrowing →
/// element projection → hash narrowing end-to-end.
#[test]
fn array_element_narrowing_and_mixed_drill() {
    let src = "\
my $x = [1, 'a'];
my $n = $x->[0];
my $s = $x->[1];
my $obj = { users => [ { name => 'A', id => 1 } ] };
my $name = $obj->{users}->[0]->{name};
my $id = $obj->{users}->[0]->{id};
";
    let fa = build_fa(src);
    assert_eq!(
        fa.inferred_type_via_bag("$n", Point::new(2, 0)),
        Some(InferredType::Numeric),
        "heterogeneous tuple projects per index",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$s", Point::new(2, 8)),
        Some(InferredType::String),
    );
    assert_eq!(
        fa.inferred_type_via_bag("$name", Point::new(5, 0)),
        Some(InferredType::String),
        "mixed drill end-to-end",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$id", Point::new(5, 30)),
        Some(InferredType::Numeric),
    );
}

/// Out-of-range and unknown-element honesty: `->[7]` of a 2-tuple is
/// None; a literal with an untypable element degrades to plain ArrayRef
/// (no per-slot claims).
#[test]
fn array_element_narrowing_negative_space() {
    let src = "\
my $x = [1, 'a'];
my $oob = $x->[7];
my $mixed = [1, some_call()];
";
    let fa = build_fa(src);
    assert_eq!(
        fa.inferred_type_via_bag("$oob", Point::new(2, 0)),
        None,
        "out-of-range projection stays honest",
    );
    let m = fa.inferred_type_via_bag("$mixed", Point::new(2, 10));
    assert_eq!(m, Some(InferredType::ArrayRef), "untypable element degrades whole literal");
}

/// `with map "Prefix::$_", qw/A B/` — the string-template map over a
/// literal list folds statically: role parents land (resolution walks
/// them) with per-word spans (goto-def on each qw word). The crm
/// role-graph idiom.
#[test]
fn map_built_role_parents() {
    let src = "\
package My::Class;
use Moo;
with map \"My::Roles::$_\", qw/Alpha Beta/;
";
    let fa = build_fa(src);
    let parents = fa.declared_parents("My::Class");
    assert_eq!(
        parents,
        &["My::Roles::Alpha".to_string(), "My::Roles::Beta".to_string()],
    );
}

/// `map BLOCK LIST` — the block spelling of the same template map (at
/// least as idiomatic as the expression form). The callback is a real
/// `block` node; the fold descends to its tail expression, and a tail
/// scalar bound exactly once by `my $x = TEMPLATE` chases to the
/// template. A re-assigned binding is an honest miss — no fold beats a
/// wrong parent.
#[test]
fn map_block_built_role_parents() {
    let src = "\
package My::Class;
use Moo;
with map { \"My::Roles::$_\" } qw/Alpha Beta/;
";
    let fa = build_fa(src);
    assert_eq!(
        fa.declared_parents("My::Class"),
        &["My::Roles::Alpha".to_string(), "My::Roles::Beta".to_string()],
    );

    let src = "\
package My::Stmt;
use Moo;
with map { my $n = \"My::Roles::$_\"; $n } qw/Alpha Beta/;
";
    let fa = build_fa(src);
    assert_eq!(
        fa.declared_parents("My::Stmt"),
        &["My::Roles::Alpha".to_string(), "My::Roles::Beta".to_string()],
    );

    let src = "\
package My::Reassigned;
use Moo;
with map { my $n = \"My::Roles::$_\"; $n = lc $n; $n } qw/Alpha Beta/;
";
    let fa = build_fa(src);
    assert!(
        fa.declared_parents("My::Reassigned").is_empty(),
        "re-assigned tail binding must not fold to a wrong parent",
    );
}

/// `require Foo::Bar;` — the bareword module form gets the same
/// PackageRef the `use` path emits (rule #7) plus a binds-nothing
/// Import row so @INC resolution sees the module. `require VERSION`
/// is a different node kind and must emit neither.
#[test]
fn require_bareword_module_ref() {
    let src = "\
package My::User;
require Some::Module;
require 5.010;
require v5.36;
";
    let fa = build_fa(src);
    assert!(
        fa.refs()
            .iter()
            .any(|r| r.target_name == "Some::Module" && matches!(r.kind, RefKind::PackageRef)),
        "bareword require emits a PackageRef",
    );
    let imp: Vec<_> =
        fa.imports.iter().filter(|i| i.module_name == "Some::Module").collect();
    assert_eq!(imp.len(), 1, "one Import row for the required module");
    assert!(imp[0].empty_import, "require binds nothing — the `use Foo ()` shape");
    assert!(imp[0].imported_symbols.is_empty());
    assert!(
        !fa.refs().iter().any(|r| r.target_name.contains("5.010")
            || r.target_name.contains("v5.36")),
        "version asserts must not mint module refs",
    );
}

/// A bareword naming an in-scope sub IS a call (Perl prefers the
/// defined sub over the class-name reading), so value-position
/// barewords get the full function treatment: a FunctionCall ref per
/// site — hover/goto-def/references/rename ride it. The declaration
/// name slot and unresolvable barewords stay untouched.
#[test]
fn bareword_promotes_to_function_ref() {
    let src = "\
sub get_config { return { host => 1 } }
my $a = get_config;
my $b = get_config->{host};
my @l = (get_config, 1);
my $f = UNRESOLVED_BAREWORD_FH;
";
    let fa = build_fa(src);
    let call_refs: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "get_config" && matches!(r.kind, RefKind::FunctionCall { .. })
        })
        .collect();
    assert_eq!(
        call_refs.len(),
        3,
        "three value-position barewords promote; the decl name does not",
    );
    assert!(
        !fa.refs().iter().any(|r| r.target_name == "UNRESOLVED_BAREWORD_FH"),
        "unresolvable barewords stay untouched",
    );
}

/// Mojolicious::Lite topic routes: `under(...)->to('ctrl#…')` sets the
/// implicit base, `group { }` scopes it (an inner `under` applies only
/// within, the outer base restores after), and `->to('#action')`
/// partials on lite verb calls inherit the controller. Every name in
/// the mechanism comes from the mojo-lite plugin's topic_route_dsl
/// manifest; the base write is the plugin's SetRouteBase emission.
#[test]
fn lite_group_under_route_inheritance() {
    let src = "\
use Mojolicious::Lite;
under('/auth')->to('login#check');
group {
  under('/n')->to('notifications#under');
  get('/x')->to('#missing_fnsku');
};
get('/y')->to('#after_group');
";
    let fa = {
        let mut parser = super::create_parser();
        let tree = parser.parse(src, None).unwrap();
        super::build_with_plugins(&tree, src.as_bytes(), super::default_plugin_registry())
    };
    let invocant_of = |action: &str| -> String {
        fa.refs()
            .iter()
            .find_map(|r| {
                if r.target_name != action {
                    return None;
                }
                let RefKind::MethodCall { ref invocant, .. } = r.kind else { return None };
                Some(format!("{:?}", invocant))
            })
            .unwrap_or_else(|| panic!("no MethodCall ref for {action}"))
    };
    assert!(
        invocant_of("missing_fnsku").contains("Notifications"),
        "in-group partial inherits the group's under (camelized)",
    );
    assert!(
        invocant_of("after_group").contains("Login"),
        "post-group partial inherits the OUTER under — the group frame popped",
    );
}

/// `plugin 'Thing'` emits a register-anchored MethodCall ref whose
/// bridged token is the plugin name camelized to a class key (already
/// CamelCase here, so it passes through), resolved by `::`-tail +
/// ownership of `register` — namespace-agnostic (Mojolicious::Plugin::*
/// and app-specific namespaces both land).
#[test]
fn lite_plugin_name_emits_register_ref() {
    let src = "\
use Mojolicious::Lite;
plugin 'WasLoaded';
plugin 'Foo::BarBaz';
";
    let fa = {
        let mut parser = super::create_parser();
        let tree = parser.parse(src, None).unwrap();
        super::build_with_plugins(&tree, src.as_bytes(), super::default_plugin_registry())
    };
    let invocants: Vec<String> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "register")
        .filter_map(|r| {
            let RefKind::MethodCall { ref invocant, .. } = r.kind else { return None };
            Some(format!("{:?}", invocant))
        })
        .collect();
    assert_eq!(invocants.len(), 2, "{:?}", invocants);
    assert!(invocants[0].contains("WasLoaded"), "{:?}", invocants);
    assert!(invocants[1].contains("Foo::BarBaz"), "{:?}", invocants);
}

/// Framework-assigned Mojo attrs (`has [qw(app tx)]` with no default —
/// the framework sets them at dispatch) type via plugin overrides, and
/// a plugin's `register($self, $app, $conf)` gets `$app: Mojolicious`
/// via the param_types manifest — with or without an indexed Mojo
/// source tree.
#[test]
fn mojo_framework_assigned_attrs_type() {
    let src = "\
package My::App::Plugin::Demo;
use Mojo::Base 'Mojolicious::Plugin';
sub register {
  my ($self, $app, $conf) = @_;
  return $app;
}
1;
";
    let fa = {
        let mut parser = super::create_parser();
        let tree = parser.parse(src, None).unwrap();
        super::build_with_plugins(&tree, src.as_bytes(), super::default_plugin_registry())
    };
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let t = fa.inferred_type_via_bag_ctx("$app", Point::new(4, 10), Some(&idx));
    assert_eq!(
        t,
        Some(InferredType::ClassName("Mojolicious".into())),
        "register's $app is the application",
    );
}

/// Interpolation deref `${ EXPR }` — `scalar > block` with no varname
/// wrapper — carries real code in strings AND regex patterns:
/// `s/_to_${\ $self->filetype }$//` holds a method call that must get
/// refs (the crm Clove::Converter idiom). The outer scalar emits
/// nothing (its text is not a variable name).
#[test]
fn interpolation_deref_code_gets_refs() {
    let src = "\
package T;
sub filetype { 'csv' }
sub run {
  my $self = shift;
  my @m;
  grep {s/_to_${\\$self->filetype}$//} @m;
  my $y = \"x_${\\$self->filetype}_z\";
  return $y;
}
1;
";
    let fa = build_fa(src);
    let calls = fa
        .refs()
        .iter()
        .filter(|r| {
            r.target_name == "filetype" && matches!(r.kind, RefKind::MethodCall { .. })
        })
        .count();
    assert_eq!(calls, 2, "regex-pattern and string interpolations both ref");
    assert!(
        !fa.refs().iter().any(|r| r.target_name.contains("${")),
        "no junk ref for the outer interpolation scalar",
    );
}

#[test]
fn test_plugin_declared_role_maker_marks_consumer_as_role() {
    // The role-maker set is OPEN: core holds no list (the base engines
    // live in frameworks/moo.rhai's manifest), and any plugin can
    // declare another engine. A registry with ONLY this plugin proves
    // the manifest alone carries the fact.
    let plugin_src = r#"
        fn id() { "house-role-kit" }
        fn triggers() { [ #{ UsesModule: "My::CustomRole" } ] }
        fn role_makers() { ["My::CustomRole"] }
    "#;
    let engine = std::sync::Arc::new(crate::build::plugin::rhai_host::make_engine());
    let plugin = crate::build::plugin::rhai_host::RhaiPlugin::from_source(plugin_src, engine)
        .expect("plugin compiles");
    let mut reg = crate::build::plugin::PluginRegistry::new();
    reg.register(Box::new(plugin));

    let source = "package House::Role;\nuse My::CustomRole;\n1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let fa = build_with_plugins(&tree, source.as_bytes(), std::sync::Arc::new(reg));
    assert!(
        fa.is_role_package("House::Role"),
        "plugin-declared role maker must mark the consumer as a role",
    );
    assert!(
        !fa.is_role_package("My::CustomRole"),
        "the maker module itself is not thereby a role",
    );
}

#[test]
fn test_bundled_moo_manifest_carries_base_role_engines() {
    // Regression net for the core-list deletion: the four base engines
    // ride frameworks/moo.rhai's role_makers() manifest through the
    // default registry. If the manifest breaks (rhai parse error, a
    // renamed fn), this is the test that says so directly.
    let source = "package R1;\nuse Moo::Role;\npackage R2;\nuse Moose::Role;\n\
                  package R3;\nuse Mouse::Role;\npackage R4;\nuse Role::Tiny;\n\
                  package C1;\nuse Moo;\npackage C2;\nuse Role::Tiny::With;\n1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let fa = build(&tree, source.as_bytes());
    for role in ["R1", "R2", "R3", "R4"] {
        assert!(fa.is_role_package(role), "{role} should be a role");
    }
    for class in ["C1", "C2"] {
        assert!(!fa.is_role_package(class), "{class} should NOT be a role");
    }
}

#[test]
fn mouse_has_gets_both_native_accessor_and_plugin_predicate() {
    // Drift pin: the Moo-family module vocabulary is ONE declaration
    // (moo.rhai's framework_mode_makers(), which also derives triggers()).
    // The old split — core match arms without Mouse, plugin triggers with
    // it — gave Mouse `has` the plugin predicate but no base accessor.
    // Both halves must synthesize from the one manifest.
    let src = "\
package Pet;
use Mouse;
has name => (is => 'ro', isa => 'Str', predicate => 'has_name');
1;
";
    let fa = build_fa(src);
    let method = |n: &str| {
        fa.symbols()
            .iter()
            .any(|s| s.name == n && s.kind == crate::model::file_analysis::SymKind::Method)
    };
    assert!(method("name"), "Mouse `has` synthesizes the native accessor");
    assert!(method("has_name"), "Mouse `has` synthesizes the plugin predicate");
    assert_eq!(
        fa.package_framework("Pet").as_ref(),
        Some(&crate::model::witnesses::FrameworkFact::Moose),
        "Mouse packages carry the Moose-flavor framework fact",
    );
}

#[test]
fn test_plugin_declared_framework_mode_maker_grants_has_semantics() {
    // The framework-mode set is OPEN: core holds no module list, and any
    // plugin can declare another Moo re-exporter. A registry with ONLY
    // this plugin proves the manifest alone carries the fact.
    let plugin_src = r#"
        fn id() { "house-oo-kit" }
        fn triggers() { [ #{ UsesModule: "My::OO" } ] }
        fn framework_mode_makers() {
            [ #{ "module": "My::OO", flavor: "Moo", imports: ["has", "with", "extends"] } ]
        }
    "#;
    let engine = std::sync::Arc::new(crate::build::plugin::rhai_host::make_engine());
    let plugin = crate::build::plugin::rhai_host::RhaiPlugin::from_source(plugin_src, engine)
        .expect("plugin compiles");
    let mut reg = crate::build::plugin::PluginRegistry::new();
    reg.register(Box::new(plugin));

    let source = "package House::Thing;\nuse My::OO;\nhas 'size' => (is => 'ro');\n1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let fa = build_with_plugins(&tree, source.as_bytes(), std::sync::Arc::new(reg));
    assert!(
        fa.symbols().iter().any(|s| {
            s.name == "size" && s.kind == crate::model::file_analysis::SymKind::Method
        }),
        "plugin-declared maker must grant native `has` accessor synthesis",
    );
    assert!(
        fa.framework_imports.contains("has") && fa.framework_imports.contains("with"),
        "the maker's declared keyword surface lands in framework_imports",
    );
}

#[test]
fn plugin_loads_recorded_trigger_independent_and_multivalue() {
    use crate::model::file_analysis::SymKind;
    // A Mojolicious::Plugin file (NO Mojo app trigger) loading other
    // plugins three ways: literal, qw-loop topic, folded constant.
    let src = "package My::Plugin::All;\n\
        use Mojo::Base 'Mojolicious::Plugin';\n\
        use constant EXTRA => 'Gizmos';\n\
        sub register {\n\
          my ($self, $app, $conf) = @_;\n\
          $app->plugin('FeatureFlags');\n\
          $app->plugin($_) for qw/SheetReaders ImportTasks ExportTasks/;\n\
          $app->plugin(EXTRA);\n\
        }\n\
        1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = build(&tree, src.as_bytes());
    let mut loads: Vec<String> = fa.plugin.loads.iter().map(|f| f.name.clone()).collect();
    loads.sort();
    assert_eq!(
        loads,
        vec!["ExportTasks", "FeatureFlags", "Gizmos", "ImportTasks", "SheetReaders"],
        "all three forms (literal, qw-loop, folded constant) recorded; got {:?}",
        fa.plugin.loads,
    );
    let _ = SymKind::Sub;
}

#[test]
fn list_assign_literal_types_each_slot() {
    // `my ($a, $b) = (10, "str")` types BOTH slots (was first-var-only/None).
    // Each var edges to its own list element's span via a FlowEdge.
    let src = "package T;
sub f {
    my ($a, $b) = (10, \"str\");
}
1;
";
    let fa = build_fa(src);
    let p = tree_sitter::Point::new(3, 0);
    assert_eq!(
        fa.inferred_type_via_bag("$a", p),
        Some(crate::model::file_analysis::InferredType::Numeric),
        "first slot ($a) types from element 0"
    );
    assert_eq!(
        fa.inferred_type_via_bag("$b", p),
        Some(crate::model::file_analysis::InferredType::String),
        "second slot ($b) types from element 1"
    );
}

#[test]
fn flow_query_pass_mints_with_builder_scope() {
    // The declarative @flow query (queries/perl/flow.scm), run inside build(),
    // mints a FlowEdge for $x with the builder's OWN scope. The Perl-on-query-
    // engine path: shape captured in .scm, scope + mint in the builder.
    let fa = build_fa("package T;
sub f {
  my $x = 42;
}
1;
");
    assert!(
        fa.flow_edges.iter().any(|fe| fe.target_name == "$x"),
        "@flow query pass should mint a FlowEdge for $x: {:?}",
        fa.flow_edges
    );
}

#[test]
fn array_destructure_types_each_slot() {
    // `my @arr=(…); my ($x,$y)=@arr` — the array types as a Sequence and each
    // destructured slot projects `element_at`.
    use crate::model::file_analysis::InferredType::{Numeric, Sequence};
    let fa = build_fa("package T;
sub f {
  my @list = (1, 2, 3);
  my ($t, $s) = @list;
}
1;
");
    let p = tree_sitter::Point::new(4, 0);
    assert_eq!(fa.inferred_type_via_bag("@list", p), Some(Sequence(vec![Numeric, Numeric, Numeric])));
    assert_eq!(fa.inferred_type_via_bag("$t", p), Some(Numeric));
    assert_eq!(fa.inferred_type_via_bag("$s", p), Some(Numeric));
}

#[test]
fn bind_shapes_mint_flow_edges() {
    // Every bind shape records a FlowEdge for the narrowing cutoff: bare
    // `my`/`local` + `foreach` var all mint a `Rebind`. (The scalar-clear
    // Undef TYPING lands with the narrowing tier — see Extraction::Cleared.)
    use crate::model::file_analysis::Extraction;
    let fa = build_fa("package T;
sub f {
  my $x;
  local $y;
  for my $i (1, 2) { g($i); }
}
1;
");
    for v in ["$x", "$y", "$i"] {
        assert!(fa.flow_edges.iter().any(|fe| fe.target_name == v
            && matches!(fe.extraction, Extraction::Rebind)), "{v} rebind edge");
    }
}

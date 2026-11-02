//! Tests for flow-sensitive narrowing + the `Optional`/`Undef` lattice
//! it drives (`src/builder/narrowing.rs`). A child of the `tests`
//! module so it shares `build_fa`.

use crate::model::file_analysis::ReceiverPolicy;
use super::*;
use crate::model::file_analysis::{
    Binding, FileAnalysis, InferredType, PathStep, Place, Slot, Slots, TypeProvenance,
};
use tree_sitter::Point;

// ── Flow-sensitive narrowing (docs/adr/flow-narrowing.md) ──

#[test]
fn narrow_block_if_isa() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->go;\n    }\n    $x->go;\n}",
    );
    // Inside the then-block: narrowed to Foo.
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "isa guard narrows inside the block",
    );
    // After the block: widened back (untyped param → None).
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(6, 4)),
        None,
        "narrowing does not leak past the block",
    );
}

#[test]
fn narrow_block_ref_eq_reftype() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if (ref($x) eq 'HASH') {\n        my $v = $x;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 16)),
        Some(InferredType::HashRef),
        "ref() eq 'HASH' narrows to HashRef",
    );
}

#[test]
fn narrow_early_return_unless() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    return unless $x->isa('Widget');\n    $x->layout;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 4)),
        Some(InferredType::ClassName("Widget".into())),
        "return-unless asserts the type over the remainder",
    );
}

#[test]
fn narrow_logical_or_exit() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    $x->isa('Session') or die;\n    $x->go;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 4)),
        Some(InferredType::ClassName("Session".into())),
        "`G or die` narrows the remainder",
    );
}

#[test]
fn narrow_observed_through_method_dispatch() {
    // The invocant of `$x->paint` inside the block resolves to Foo, so a
    // method call dispatches on the narrowed class (the query seam).
    let fa = build_fa(
        "package Foo;\nsub paint { my ($self) = @_; }\npackage P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->paint;\n    }\n}",
    );
    let r = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "paint" && matches!(r.kind, RefKind::MethodCall { .. }))
        .expect("paint method-call ref");
    assert_eq!(
        fa.receiver_classes(r, None, ReceiverPolicy::Navigate).single().map(str::to_string).as_deref(),
        Some("Foo"),
        "narrowed invocant dispatches on Foo",
    );
}

#[test]
fn narrow_negative_else_not_applied() {
    // The else-branch knows `$x` is NOT Foo — no positive type to assert.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->a;\n    } else {\n        $x->b;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "then-branch still narrows",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(6, 8)),
        None,
        "else-branch is not narrowed (negative polarity, deferred)",
    );
}

#[test]
fn narrow_negated_guard_skips_then_block() {
    // `if (!G) { BODY }` proves NOT-Foo in BODY — negative, so no narrowing.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if (!$x->isa('Foo')) {\n        $x->a;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        None,
        "negated guard does not positively narrow its block",
    );
}

#[test]
fn narrow_truncated_at_reassignment() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->before;\n        $x = bar();\n        $x->after;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "narrowed before the reassignment",
    );
    assert_ne!(
        fa.inferred_type_via_bag("$x", Point::new(6, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "reassignment truncates the narrowing region",
    );
}

// Truncation must recognize EVERY way Perl rebinds a scalar, not just
// `$x = …` / `my $x` — else a stale narrowing leaks past the rebind (an
// always-on false `undef-deref` for the `defined`-guard flavor). One test
// per binding shape; the cutoff (`RebindIndex`) reads the
// FlowEdge set, so each shape must mint a `@flow` edge targeting the subject.

#[test]
fn narrow_truncated_at_list_assignment() {
    // `($x, $y) = …` rebinds BOTH targets. Narrow `$y` — the non-first
    // target — to prove every element of the list ends its own narrowing.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    if ($y->isa('Bar')) {\n        $y->before;\n        ($x, $y) = pair();\n        $y->after;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$y", Point::new(4, 8)),
        Some(InferredType::ClassName("Bar".into())),
        "narrowed before the list-assignment",
    );
    assert_ne!(
        fa.inferred_type_via_bag("$y", Point::new(6, 8)),
        Some(InferredType::ClassName("Bar".into())),
        "list-assignment rebinds $y (a non-first target) → truncates",
    );
}

#[test]
fn narrow_truncated_at_foreach_loop_var() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->before;\n        for $x (@items) {\n            $x->after;\n        }\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "narrowed before the loop",
    );
    assert_ne!(
        fa.inferred_type_via_bag("$x", Point::new(6, 12)),
        Some(InferredType::ClassName("Foo".into())),
        "foreach loop variable rebinds $x → truncates",
    );
}

#[test]
fn narrow_conjunction_intersects() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo') && $x->can('m')) {\n        $x->go;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 8)),
        Some(InferredType::ClassName("Foo".into())),
        "&&-chain narrows on the isa conjunct",
    );
}

// ── Optional types: `return undef` ──

#[test]
fn optional_return_undef_or_value() {
    let fa = build_fa(
        "package P;\nsub maybe_make {\n    my ($ok) = @_;\n    return undef unless $ok;\n    return Foo->new;\n}",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("maybe_make", None),
        Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into())))),
        "return undef + return Foo->new folds to Optional<Foo>",
    );
}

#[test]
fn optional_not_applied_without_undef_arm() {
    // No undef arm → plain ClassName, not Optional (existing behavior).
    let fa = build_fa(
        "package P;\nsub make {\n    my ($ok) = @_;\n    return Foo->new;\n}",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("make", None),
        Some(InferredType::ClassName("Foo".into())),
    );
}

#[test]
fn optional_defined_narrows_to_inner() {
    // maybe() : Optional<Foo>; `defined $r` strips it so $r->go dispatches Foo.
    let fa = build_fa(
        "package P;\nsub maybe { return undef unless 1; return Foo->new; }\nsub use_it {\n    my $r = maybe();\n    return unless defined $r;\n    $r->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn optional_blessed_narrows_to_inner() {
    let fa = build_fa(
        "package P; use Scalar::Util qw(blessed);\nsub maybe { return undef unless 1; return Foo->new; }\nsub use_it {\n    my $r = maybe();\n    return unless blessed $r;\n    $r->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn optional_dispatches_leniently_without_guard() {
    // An unguarded Optional<Foo> still resolves its method receiver to Foo
    // (lenient navigation — goto/hover/completion shouldn't go dark just
    // because a `defined` guard hasn't been written; a future deref
    // diagnostic owns the "might be undef" warning). The lattice keeps the
    // value typed Optional<Foo> — only the receiver projection peels.
    let fa = build_fa(
        "package P;\nsub maybe { return undef unless 1; return Foo->new; }\nsub use_it {\n    my $r = maybe();\n    $r->go;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$r", Point::new(4, 4)),
        Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into())))),
        "the value stays typed Optional<Foo> (the lattice is unchanged)",
    );
    assert_eq!(
        invocant_class_of(&fa, "go").as_deref(),
        Some("Foo"),
        "the receiver dispatches leniently to Foo without a guard",
    );
}

#[test]
fn optional_receiver_goto_def_resolves() {
    // The motivating case: goto-def on a method called on an unguarded
    // Optional<Foo> lands on Foo's method, via the stamped nav edge that
    // `resolve_receiver` now resolves leniently.
    let fa = build_fa(
        "package Foo;\nsub go { my ($self) = @_; }\npackage P;\nsub maybe { return undef unless 1; return Foo->new; }\nsub use_it {\n    my $r = maybe();\n    $r->go;\n}",
    );
    let foo_go = fa.symbols().iter().find(|s| s.name == "go").unwrap();
    // Cursor on the `go` token of `$r->go`.
    assert_eq!(
        fa.find_definition(Point::new(6, 8), None),
        Some(foo_go.selection_span),
        "goto-def lands on Foo::go through the optional receiver",
    );
}

#[test]
fn undef_receiver_does_not_dispatch() {
    // A provably-Undef receiver dispatches nowhere — you can't navigate the
    // methods of nothing. `Undef` peels to None, unlike `Optional`.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    return if defined $x;\n    $x->go;\n}",
    );
    assert_eq!(
        invocant_class_of(&fa, "go").as_deref(),
        None,
        "a definitely-Undef receiver stays dark",
    );
}

#[test]
fn optional_ternary_production() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($c) = @_;\n    my $x = $c ? Foo->new : undef;\n    $x;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 4)),
        Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into())))),
        "$c ? Foo->new : undef produces Optional<Foo>",
    );
}

#[test]
fn optional_ternary_empty_list_arm() {
    // `()` coerces to undef in scalar context, so it's an undef arm too.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($c) = @_;\n    my $x = $c ? Foo->new : ();\n    $x;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 4)),
        Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into())))),
        "$c ? Foo->new : () produces Optional<Foo>",
    );
}

#[test]
fn optional_ternary_then_defined_narrows() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($c) = @_;\n    my $x = $c ? Foo->new : undef;\n    return unless defined $x;\n    $x->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn optional_ternary_conflict_is_a_union() {
    // Two distinct concrete arms (no undef) are a union, not Optional.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($c) = @_;\n    my $x = $c ? Foo->new : Bar->new;\n    $x;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 4)),
        Some(crate::model::file_analysis::join(
            &InferredType::ClassName("Foo".into()),
            &InferredType::ClassName("Bar".into()),
            &crate::model::file_analysis::Unordered,
        )),
        "Foo vs Bar is a union, not Optional",
    );
}

// ── Negative lattice: Undef on the `defined` family ──

#[test]
fn negative_return_if_defined_is_undef() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    return if defined $x;\n    my $y = $x;\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 12)),
        Some(InferredType::Undef),
        "after `return if defined $x`, $x is undef",
    );
}

#[test]
fn negative_unless_defined_block_is_undef() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    unless (defined $x) {\n        my $y = $x;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(4, 16)),
        Some(InferredType::Undef),
        "inside `unless (defined $x)`, $x is undef",
    );
}

#[test]
fn negative_defined_then_else_split() {
    // then-branch strips Optional<Foo> → Foo; else-branch → Undef.
    let fa = build_fa(
        "package P;\nsub maybe { return undef unless 1; return Foo->new; }\nsub f {\n    my $r = maybe();\n    if (defined $r) {\n        $r->a;\n    } else {\n        my $y = $r;\n    }\n}",
    );
    assert_eq!(invocant_class_of(&fa, "a").as_deref(), Some("Foo"));
    assert_eq!(
        fa.inferred_type_via_bag("$r", Point::new(7, 16)),
        Some(InferredType::Undef),
        "else-branch: $r is undef",
    );
}

#[test]
fn negative_isa_else_stays_wide() {
    // `isa` has no representable complement, so the else is NOT narrowed.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    if ($x->isa('Foo')) {\n        $x->a;\n    } else {\n        my $y = $x;\n    }\n}",
    );
    assert_eq!(invocant_class_of(&fa, "a").as_deref(), Some("Foo"));
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(6, 16)),
        None,
        "isa else has no positive target — stays wide, not Undef",
    );
}

// ── elsif-chain cumulative negation ──

#[test]
fn narrow_elsif_else_defined_is_undef() {
    // The `else` of an `if (defined) / elsif / else` chain asserts the
    // negation of every preceding condition — `!defined $x` → Undef.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $c) = @_;\n    if (defined $x) {\n        my $a = $x;\n    } elsif ($c) {\n        my $b = $x;\n    } else {\n        my $z = $x;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(8, 16)),
        Some(InferredType::Undef),
        "elsif-chain else: !defined $x → Undef",
    );
    // The intervening elsif block also runs only when !defined $x.
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(6, 16)),
        Some(InferredType::Undef),
        "elsif block: prior !defined $x → Undef",
    );
    // The then-block still strips to the inner (defined holds there).
    assert_ne!(
        fa.inferred_type_via_bag("$x", Point::new(4, 16)),
        Some(InferredType::Undef),
        "then-block is not Undef (defined holds)",
    );
}

#[test]
fn narrow_elsif_else_isa_stays_wide() {
    // `isa` has no representable complement, so an elsif-chain else over an
    // isa guard stays wide (no false Undef).
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $c) = @_;\n    if ($x->isa('Foo')) {\n        my $a = $x;\n    } elsif ($c) {\n        my $b = $x;\n    } else {\n        my $z = $x;\n    }\n}",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(8, 16)),
        None,
        "isa has no positive complement — elsif else stays wide",
    );
}

// ── Place narrowing: `$self->{x}` ──

/// Class an invocant resolves to for the method-call ref named `method`.
fn invocant_class_of(fa: &FileAnalysis, method: &str) -> Option<String> {
    let r = fa
        .refs()
        .iter()
        .find(|r| r.target_name == method && matches!(r.kind, RefKind::MethodCall { .. }))
        .unwrap_or_else(|| panic!("no method-call ref for {method}"));
    fa.receiver_classes(r, None, ReceiverPolicy::Navigate).single().map(str::to_string)
}

/// The place under the lexical `root` (declared once in the source) at `path`.
fn lexical_place(fa: &FileAnalysis, root: &str, path: Vec<PathStep>) -> Place {
    let sym = fa.symbols().iter().find(|s| s.name == root).unwrap_or_else(|| panic!("no symbol {root}"));
    Place { root: Binding::Lexical(sym.id), path }
}

fn key(k: &str) -> PathStep {
    PathStep::Hash(Slots::One(Slot::Const(k.to_string())))
}

#[test]
fn narrow_place_block_if_isa() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    if ($self->{h}->isa('Foo')) {\n        $self->{h}->go;\n    }\n}",
    );
    assert_eq!(
        fa.place_type_at(&lexical_place(&fa, "$self", vec![PathStep::Deref, key("h")]), Point::new(4, 8), None),
        Some(InferredType::ClassName("Foo".into())),
        "place isa guard narrows the slot inside the block",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn narrow_place_early_return() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{cfg}->isa('Cfg');\n    $self->{cfg}->load;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "load").as_deref(), Some("Cfg"));
}

#[test]
fn narrow_place_method_on_value_keeps_slot() {
    // A method call on the slot's VALUE doesn't disturb the slot, so both
    // uses stay narrowed.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{db}->isa('Schema');\n    $self->{db}->connect;\n    $self->{db}->disconnect;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "connect").as_deref(), Some("Schema"));
    assert_eq!(invocant_class_of(&fa, "disconnect").as_deref(), Some("Schema"));
}

#[test]
fn narrow_place_truncated_by_slot_write() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{x}->isa('Foo');\n    $self->{x}->before;\n    $self->{x} = other();\n    $self->{x}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "writing the slot truncates the narrowing",
    );
}

#[test]
fn narrow_place_truncated_by_opaque_root_op() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{x}->isa('Foo');\n    $self->{x}->before;\n    $self->reset;\n    $self->{x}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "an opaque op on the root ($self->reset) truncates the narrowing",
    );
}

#[test]
fn narrow_place_dynamic_key() {
    // A plain-scalar key names one slot while the scalar is unchanged
    // (`$self->{$k}`), so it narrows like a constant-key place.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self, $k) = @_;\n    return unless $self->{$k}->isa('Foo');\n    $self->{$k}->go;\n}",
    );
    assert_eq!(
        invocant_class_of(&fa, "go").as_deref(),
        Some("Foo"),
        "a plain-scalar key is a stable place and narrows",
    );
}

#[test]
fn narrow_place_dynamic_key_truncated_by_key_reassign() {
    // Reassigning the key scalar `$k` breaks the place identity, so the
    // narrowing ends there.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self, $k) = @_;\n    return unless $self->{$k}->isa('Foo');\n    $self->{$k}->before;\n    $k = 'other';\n    $self->{$k}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "reassigning the key scalar truncates the dynamic-key narrowing",
    );
}

#[test]
fn narrow_place_dynamic_key_truncated_by_slot_write() {
    // Writing the same dynamic slot truncates.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self, $k) = @_;\n    return unless $self->{$k}->isa('Foo');\n    $self->{$k}->before;\n    $self->{$k} = other();\n    $self->{$k}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "writing the dynamic slot truncates the narrowing",
    );
}

#[test]
fn narrow_place_multihop_isa() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{a}{b}->isa('Foo');\n    $self->{a}{b}->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn narrow_place_multihop_method_on_value_keeps_slot() {
    // A method call on the deep slot's value doesn't disturb it.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{a}{b}->isa('Foo');\n    $self->{a}{b}->one;\n    $self->{a}{b}->two;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "one").as_deref(), Some("Foo"));
    assert_eq!(invocant_class_of(&fa, "two").as_deref(), Some("Foo"));
}

#[test]
fn narrow_place_multihop_truncated_by_intermediate_prefix_op() {
    // A method call on the intermediate prefix ($self->{a}) could mutate the
    // object holding {b}, so it truncates the deeper narrowing.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{a}{b}->isa('Foo');\n    $self->{a}{b}->before;\n    $self->{a}->mutate;\n    $self->{a}{b}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "an op on the intermediate prefix truncates the deep narrowing",
    );
}

#[test]
fn narrow_place_multihop_sibling_write_keeps_slot() {
    // Writing a sibling slot ($self->{a}{c}) doesn't disturb $self->{a}{b}.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{a}{b}->isa('Foo');\n    $self->{a}{c} = 1;\n    $self->{a}{b}->after;\n}",
    );
    assert_eq!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "a sibling-slot write does not truncate",
    );
}

#[test]
fn narrow_place_ref_eq() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless ref($self->{cfg}) eq 'HASH';\n    my $v = $self->{cfg};\n}",
    );
    assert_eq!(
        fa.place_type_at(&lexical_place(&fa, "$self", vec![PathStep::Deref, key("cfg")]), Point::new(4, 12), None),
        Some(InferredType::HashRef),
        "ref(place) eq 'HASH' narrows the slot",
    );
}

// ── Place identity: a place is its root's binding plus its path ──

#[test]
fn narrow_place_package_alias_spellings_share_the_place() {
    // `our %h` in `P` and `%P::h` are one variable, so a guard on one
    // spelling narrows a use through the other, and a write through either
    // disturbs it.
    let fa = build_fa(
        "package P;\nour %h;\nsub f {\n    return unless $P::h{cfg}->isa('Cfg');\n    $h{cfg}->load;\n    %P::h = ();\n    $h{cfg}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "load").as_deref(), Some("Cfg"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Cfg"),
        "a write through the qualified spelling disturbs the place",
    );
}

#[test]
fn narrow_place_implicit_and_explicit_arrow_are_one_place() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{a}{b}->isa('Foo');\n    $self->{a}->{b}->go;\n    $$self{a}{b}->again;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
    assert_eq!(invocant_class_of(&fa, "again").as_deref(), Some("Foo"), "a sigil deref is the arrow");
}

#[test]
fn narrow_place_quoted_and_bare_keys_are_one_key() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{'cfg'}->isa('Cfg');\n    $self->{cfg}->load;\n    $self->{\"cfg\"}->again;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "load").as_deref(), Some("Cfg"));
    assert_eq!(invocant_class_of(&fa, "again").as_deref(), Some("Cfg"));
}

#[test]
fn narrow_place_slice_disturbs_only_the_slots_it_names() {
    let fa = build_fa(
        "package P;\nsub f {\n    my %h = @_;\n    return unless $h{a}->isa('Foo');\n    @h{qw(b c)} = (1, 2);\n    $h{a}->kept;\n    @h{'a', 'b'} = (1, 2);\n    $h{a}->gone;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "kept").as_deref(), Some("Foo"), "a slice of sibling slots keeps the slot");
    assert_ne!(invocant_class_of(&fa, "gone").as_deref(), Some("Foo"), "a slice naming the slot disturbs it");
}

#[test]
fn narrow_place_shadowing_variable_is_another_place() {
    // The inner `$self` is another variable: writing its slot leaves the
    // outer one narrowed.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{x}->isa('Foo');\n    {\n        my $self = {};\n        $self->{x} = 1;\n    }\n    $self->{x}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "after").as_deref(), Some("Foo"));
}

#[test]
fn narrow_place_truncated_by_computed_key_write() {
    // A computed key may name any slot.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self, $k) = @_;\n    return unless $self->{x}->isa('Foo');\n    $self->{$k . 'y'} = 1;\n    $self->{x}->after;\n}",
    );
    assert_ne!(invocant_class_of(&fa, "after").as_deref(), Some("Foo"));
}

#[test]
fn narrow_place_ends_at_delete_undef_and_local() {
    // Each leaves the slot without the guarded value; `exists` reads it.
    for reset in ["delete $self->{x};", "undef $self->{x};", "undef($self->{x});", "local $self->{x};", "delete local $self->{x};"] {
        let fa = build_fa(&format!(
            "package P;\nsub f {{\n    my ($self) = @_;\n    return unless $self->{{x}}->isa('Foo');\n    $self->{{x}}->before;\n    {reset}\n    $self->{{x}}->after;\n}}",
        ));
        assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"), "{reset}");
        assert_ne!(invocant_class_of(&fa, "after").as_deref(), Some("Foo"), "{reset}");
    }
    let fa = build_fa(
        "package P;\nsub f {\n    my ($self) = @_;\n    return unless $self->{x}->isa('Foo');\n    exists $self->{x};\n    $self->{x}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "after").as_deref(), Some("Foo"), "exists only reads");
}

// ── Place narrowing: direct element form (`$h{k}` / `$a[0]`) ──

#[test]
fn narrow_place_direct_hash_isa() {
    // Direct hash element — base is `%h`, not a scalar deref.
    let fa = build_fa(
        "package P;\nsub f {\n    my %h = @_;\n    return unless $h{cfg}->isa('Cfg');\n    $h{cfg}->load;\n}",
    );
    assert_eq!(
        fa.place_type_at(&lexical_place(&fa, "%h", vec![key("cfg")]), Point::new(4, 4), None),
        Some(InferredType::ClassName("Cfg".into())),
        "direct hash-element isa guard narrows the slot",
    );
    assert_eq!(invocant_class_of(&fa, "load").as_deref(), Some("Cfg"));
}

#[test]
fn narrow_place_direct_array_isa() {
    // Direct array element — base is `@a`.
    let fa = build_fa(
        "package P;\nsub f {\n    my @a = @_;\n    return unless $a[0]->isa('Widget');\n    $a[0]->paint;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "paint").as_deref(), Some("Widget"));
}

#[test]
fn narrow_place_direct_truncated_by_whole_container_write() {
    // Reassigning the whole `%h` truncates the slot narrowing.
    let fa = build_fa(
        "package P;\nsub f {\n    my %h = @_;\n    return unless $h{x}->isa('Foo');\n    $h{x}->before;\n    %h = ();\n    $h{x}->after;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "before").as_deref(), Some("Foo"));
    assert_ne!(
        invocant_class_of(&fa, "after").as_deref(),
        Some("Foo"),
        "reassigning the whole hash truncates the narrowing",
    );
}

#[test]
fn narrow_place_direct_slot_read_keeps_slot() {
    // Reading the slot value doesn't disturb it — both uses stay narrowed.
    let fa = build_fa(
        "package P;\nsub f {\n    my %h = @_;\n    return unless $h{db}->isa('Schema');\n    $h{db}->connect;\n    $h{db}->disconnect;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "connect").as_deref(), Some("Schema"));
    assert_eq!(invocant_class_of(&fa, "disconnect").as_deref(), Some("Schema"));
}

#[test]
fn optional_bare_return_then_defined() {
    // The dominant idiom: `return unless …; return Obj->new` is Optional,
    // and a `defined` guard at the call site recovers the class.
    let fa = build_fa(
        "package P;\nsub maybe { return unless 1; return Foo->new; }\nsub use_it {\n    my $r = maybe();\n    return unless defined $r;\n    $r->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

#[test]
fn optional_return_provenance() {
    // --dump-package should explain an Optional return via the join.
    let fa = build_fa("package P;\nsub maybe { return undef unless 1; return Foo->new; }\n");
    let sid = fa.symbols().iter().find(|s| s.name == "maybe").unwrap().id;
    match fa.type_provenance.get(&sid) {
        Some(TypeProvenance::ReducerFold { evidence, .. }) => assert!(
            evidence.iter().any(|e| e == "optional_join"),
            "evidence should record the optional join, got {evidence:?}",
        ),
        other => panic!("expected ReducerFold provenance, got {other:?}"),
    }
}

#[test]
fn optional_maybe_isa_scalar() {
    let fa = build_fa("package P;\nuse Moo;\nhas x => (is => 'ro', isa => 'Maybe[Int]');\n");
    assert_eq!(
        fa.sub_return_type_at_arity("x", Some(0)),
        Some(InferredType::Optional(Box::new(InferredType::Numeric))),
        "Maybe[Int] accessor returns Optional<Numeric>",
    );
}

#[test]
fn optional_maybe_isa_instance() {
    let fa = build_fa(
        "package P;\nuse Moo;\nhas thing => (is => 'ro', isa => 'Maybe[InstanceOf[\"Foo\"]]');\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::Optional(Box::new(InferredType::ClassName("Foo".into())))),
        "Maybe[InstanceOf['Foo']] accessor returns Optional<Foo>",
    );
}

// ── cst reuse: grouping peel + plain-literal strictness flow through
//    the shared `cst` primitives (cst::peel_groups /
//    cst::first_named_child_where / cst::plain_string_literal_text) ──


#[test]
fn narrow_isa_const_folded_class_arg() {
    // `my $C = 'Foo'; $x->isa($C)` — the fold pins $C to one class, so the
    // guard narrows just like a literal class name.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    my $C = 'Foo';\n    return unless $x->isa($C);\n    $x->go;\n}",
    );
    assert_eq!(
        invocant_class_of(&fa, "go").as_deref(),
        Some("Foo"),
        "a const-folded class name narrows the isa guard",
    );
}

#[test]
fn narrow_isa_interpolated_arg_stays_wide() {
    // plain_string_literal_text rejects interpolation, so `isa("$cls")`
    // is NOT a literal class-name guard and must not narrow.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $cls) = @_;\n    return unless $x->isa(\"$cls\");\n    $x->go;\n}",
    );
    assert_eq!(
        invocant_class_of(&fa, "go").as_deref(),
        None,
        "interpolated isa arg is not a literal class name",
    );
}

#[test]
fn narrow_ref_eq_paren_wrapped_arg() {
    // first_named_child_where peels the `(...)` around the ref() operand.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x) = @_;\n    return unless ref(($x)) eq 'Foo';\n    $x->go;\n}",
    );
    assert_eq!(invocant_class_of(&fa, "go").as_deref(), Some("Foo"));
}

// ---- `Unknown`: a write nothing can type still happened ----

#[test]
fn a_rebind_nothing_can_type_retires_the_prior_belief() {
    // Temporal ordering alone reads the stale belief here: the write's RHS
    // has no type, so without an `Unknown` witness the fold falls back on
    // the newest belief it can read — the one the write replaced.
    let fa = build_fa("sub f {\n  my $ct = undef;\n  $ct = unknown_fn();\n  return defined $ct;\n}");
    assert_eq!(
        fa.inferred_type_via_bag("$ct", Point::new(3, 20)),
        Some(InferredType::Unknown),
        "the untypeable write must land as Unknown, not leave Undef standing",
    );
}

#[test]
fn a_write_inside_a_loop_body_widens_the_read_after_it() {
    // The write's own witness sits on the loop body's scope, which the read
    // after the loop never walks through. The body may not have run, so the
    // binding scope gets `Unknown` at the write — old-or-new is all that
    // can be said there, and it is enough to stop `defined $ct` reading as
    // a guard that can never pass.
    let fa = build_fa(
        "sub f {\n  my $ct = undef;\n  for my $x (1,2) { $ct = unknown_fn($x); }\n  return defined $ct;\n}",
    );
    assert_eq!(fa.inferred_type_via_bag("$ct", Point::new(3, 20)), Some(InferredType::Unknown));
}

#[test]
fn a_typed_write_in_a_block_types_the_block_and_widens_what_follows() {
    // Inside the block, after the write, the value IS the RHS. Past the
    // block it is old-or-new.
    let fa = build_fa(
        "package P;\nsub m {\n  my $x = 'str';\n  if (c()) {\n    $x = Foo->new;\n    my $i = $x;\n  }\n  my $o = $x;\n}\n1;\n",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(5, 13)),
        Some(InferredType::ClassName("Foo".into())),
        "a read in the block after the write sees the write",
    );
    assert_eq!(
        fa.inferred_type_via_bag("$x", Point::new(7, 11)),
        Some(InferredType::Unknown),
        "a read past the block sees neither the stale String nor a Foo that may not have happened",
    );
}

#[test]
fn a_conditional_write_widens_instead_of_asserting() {
    // `$x = undef if COND` may not happen, so its belief is old-or-new on
    // both sides of the statement: the guard's own condition must not read
    // an undef the write has not produced, and the successor must not read
    // one it may never produce.
    let fa = build_fa(
        "sub f {\n  my $isbn = get_isbn();\n  $isbn = undef if $isbn && !$isbn->is_valid;\n  return $isbn;\n}",
    );
    for (label, point) in [("inside the guard condition", Point::new(3, 30)), ("after the statement", Point::new(4, 9))] {
        assert!(
            !matches!(fa.inferred_type_via_bag("$isbn", point), Some(InferredType::Undef)),
            "{label}: a conditional write must not assert its value",
        );
    }
}

#[test]
fn a_shadowed_rebind_neither_erases_the_outer_belief_nor_borrows_it() {
    // The inner `$x` is a different variable. Its untypeable write must not
    // touch the outer belief — and, its own belief gone, the inner read must
    // stop at `Unknown` rather than walk out and answer with the outer's.
    let fa = build_fa(
        "package P;\nsub m {\n    my $x = 'str';\n    my $c = sub {\n        my $x = 42;\n        $x = unknown_fn();\n        my $i = $x;\n    };\n    my $o = $x;\n}\n1;\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(8, 13)), Some(InferredType::String), "outer untouched");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(6, 17)), Some(InferredType::Unknown), "inner stops on Unknown");
}

#[test]
fn a_later_plain_write_retires_an_earlier_class_identity() {
    // The class axis used to answer ahead of the plain axis with no
    // temporal order between them, so this read Foo after the rebind.
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  $x = 'str';\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::String));
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  $x = unknown_fn();\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::Unknown));
    // Identity-over-rep is untouched: a deref reveals representation, not a
    // new value, and the class still answers.
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  my $k = $x->{k};\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::ClassName("Foo".into())));
}

#[test]
fn a_rebind_to_a_bare_ref_retires_an_object() {
    // The class-over-rep rule protects a DEREF (`$x->{k}` reveals the
    // object's representation); an assignment of a bare ref is a new value,
    // and only the reset marker tells the two apart — a subsumption test on
    // the types cannot.
    for (rhs, want) in [
        // `my $o = $x` is an escape, which opens the shape.
        ("{}", super::escaped_empty_hash_shape()),
        ("[]", InferredType::ArrayRef),
        ("$h", super::escaped_empty_hash_shape()),
    ] {
        let src = format!(
            "package P;\nsub m {{\n  my $h = {{}};\n  my $x = Foo->new;\n  $x = {rhs};\n  my $o = $x;\n}}\n1;\n"
        );
        let fa = build_fa(&src);
        assert_eq!(
            fa.inferred_type_via_bag("$x", Point::new(5, 11)),
            Some(want),
            "`$x = {rhs}` after `Foo->new` must read the ref, not Foo",
        );
    }
}

#[test]
fn evidence_after_an_untypable_rebind_revives_the_variable() {
    // The reset has supremacy over its PAST only: a deref after it is new
    // evidence, and the variable reads through it.
    let fa = build_fa(
        "package P;\nsub m {\n  my $x = Foo->new;\n  $x = unknown_fn();\n  my $k = $x->{k};\n  my $o = $x;\n}\n1;\n",
    );
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(5, 11)), Some(InferredType::HashRef));
    // With nothing after the reset the reset IS the answer.
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  $x = unknown_fn();\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::Unknown));
}

#[test]
fn a_declaration_nothing_can_type_stays_absent() {
    // A first binding retires nothing, so its untypable RHS is absence (let
    // enrichment or usage fill it), never a reset.
    let fa = build_fa("package P;\nsub m {\n  my $x = unknown_fn();\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(3, 11)), None);
    let fa = build_fa("package P;\nsub m {\n  my $x = unknown_fn();\n  my $k = $x->{k};\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::HashRef));
}

#[test]
fn a_declaration_is_a_write_and_resets_too() {
    // A redeclaration retires the earlier belief exactly as a plain
    // assignment does — there is one kind of write.
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  my $x = 'str';\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::String));
    let fa = build_fa("package P;\nsub m {\n  my $x = Foo->new;\n  my $x = unknown_fn();\n  my $o = $x;\n}\n1;\n");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(4, 11)), Some(InferredType::Unknown));
}

#[test]
fn a_parameter_assertion_survives_its_own_declaration() {
    // The first-param claim is anchored at the `$self` token, so the
    // declaration's marker (which retires what lies strictly before it)
    // leaves it standing — for the `@_` unpack and the `shift` idiom alike.
    for src in [
        "package P;\nsub m {\n  my ($self, $x) = @_;\n  my $o = $self;\n}\n1;\n",
        "package P;\nsub m {\n  my $self = shift;\n  my $o = $self;\n}\n1;\n",
    ] {
        let fa = build_fa(src);
        assert_eq!(
            fa.inferred_type_via_bag("$self", Point::new(3, 14)),
            Some(InferredType::ClassName("P".into())),
            "{src}"
        );
    }
}

#[test]
fn a_guard_narrows_every_arm_it_decides() {
    // A ternary's branches and a logical operator's right side are arms
    // like any block: each narrows by its guard at the polarity it runs on.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    my $a = $x->isa('Foo') ? $x->go : $x->stop;\n    my $b = defined $y ? $y : $y;\n    my $c = $x->isa('Foo') && $x->go;\n    my $d = defined $y || $y;\n    $x->isa('Foo') and $x->go;\n    $x->go;\n}",
    );
    let at = |row, column| fa.inferred_type_via_bag("$x", Point::new(row, column));
    let at_y = |row, column| fa.inferred_type_via_bag("$y", Point::new(row, column));
    let foo = Some(InferredType::ClassName("Foo".into()));
    assert_eq!(at(3, 29), foo, "the branch a ternary takes when the guard holds");
    assert_eq!(at(3, 38), None, "isa has no complement: the other branch stays wide");
    assert_eq!(at_y(4, 31), Some(InferredType::Undef), "the branch taken when `defined` fails");
    assert_eq!(at(5, 30), foo, "the right of `&&`");
    assert_eq!(at_y(6, 26), Some(InferredType::Undef), "the right of `||` runs where the guard fails");
    assert_eq!(at(7, 24), foo, "the right of `and`");
    assert_eq!(at(8, 4), None, "nothing after");
}

#[test]
fn a_loop_guard_narrows_its_body() {
    // A loop tests its condition before every run of its body, so the body
    // is an arm of the guard; `do BLOCK while G` runs once untested.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    while ($x->isa('Foo')) { $x->go; }\n    until (defined $y) { my $u = $y; }\n    $x->go while $x->isa('Foo');\n    do { $x->go } while $x->isa('Foo');\n    for (my $i = 0; $x->isa('Foo'); $i++) { $x->go; }\n    $x->go;\n}",
    );
    let at = |row, column| fa.inferred_type_via_bag("$x", Point::new(row, column));
    let foo = Some(InferredType::ClassName("Foo".into()));
    assert_eq!(at(3, 29), foo, "a while body");
    assert_eq!(
        fa.inferred_type_via_bag("$y", Point::new(4, 34)),
        Some(InferredType::Undef),
        "an until body runs where the guard fails",
    );
    assert_eq!(at(5, 4), foo, "a statement a while modifier repeats");
    assert_eq!(at(6, 9), None, "a do block runs before its first test");
    assert_eq!(at(7, 46), foo, "a C-style for body");
    assert_eq!(at(8, 4), None, "nothing after: `last` leaves with the guard unchanged");
}

#[test]
fn a_failed_conjunction_proves_nothing_about_either_side() {
    // `!(A && B)` is `!A || !B`: neither side alone is known false, so no
    // region where a conjunction fails narrows its conjuncts.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    if (defined($x) && $y) { 1 } else { my $e = $x; }\n    my $t = (defined($x) && $y) ? 1 : $x;\n    (defined($x) && $y) || print $x;\n    unless (defined($x) and $y) { my $u = $x; }\n    if (!(defined($x) && $y)) { my $n = $x; }\n    return if (defined($x) && $y eq 'f');\n    my $after = $x;\n}",
    );
    let at = |row, column| fa.inferred_type_via_bag("$x", Point::new(row, column));
    assert_eq!(at(3, 50), None, "the else of a conjunction");
    assert_eq!(at(4, 38), None, "the false arm of a ternary on a conjunction");
    assert_eq!(at(5, 33), None, "the right of `||` after a conjunction");
    assert_eq!(at(6, 43), None, "the body of `unless` on an `and`");
    assert_eq!(at(7, 41), None, "a negated conjunction");
    assert_eq!(at(9, 16), None, "after `return if (defined($x) && ...)`");
}

#[test]
fn a_failed_disjunction_proves_both_sides_false() {
    // `!(A || B)` is `!A && !B`, so where a disjunction fails each side
    // narrows at its own false polarity; where it holds, neither does.
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    if (defined($x) || defined($y)) { my $o = $x; } else { my $e = $x; my $f = $y; }\n    return if !defined($x) or !defined $y;\n    my $after = $x;\n}",
    );
    let at = |var, row, column| fa.inferred_type_via_bag(var, Point::new(row, column));
    assert_eq!(at("$x", 3, 47), None, "a disjunction that holds proves no side");
    assert_eq!(at("$x", 3, 67), Some(InferredType::Undef), "the else of `||`: $x undef");
    assert_eq!(at("$y", 3, 78), Some(InferredType::Undef), "the else of `||`: $y undef");
    assert_eq!(fa.inferred_type_via_bag("$x", Point::new(5, 16)), None, "untyped param: strip leaves it wide");
}

#[test]
fn a_low_precedence_not_and_and_narrow_like_their_symbols() {
    let fa = build_fa(
        "package P;\nsub f {\n    my ($x, $y) = @_;\n    if (not defined $x) { my $u = $x; }\n    if ($x->isa('Foo') and $y) { $x->go; }\n    return if not $x->isa('Foo');\n    $x->go;\n}",
    );
    let at = |row, column| fa.inferred_type_via_bag("$x", Point::new(row, column));
    let foo = Some(InferredType::ClassName("Foo".into()));
    assert_eq!(at(3, 34), Some(InferredType::Undef), "`not defined`");
    assert_eq!(at(4, 33), foo, "an `and` condition");
    assert_eq!(at(6, 4), foo, "`return if not` narrows the remainder");
}

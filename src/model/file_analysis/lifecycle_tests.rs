//! The unrowed-residue derivation: the invariant the consult pre-filter
//! trusts is "every class-keyed bag attachment name is either backed by a
//! row-visible symbol/ref or listed in `unrowed_attachment_names`" —
//! enforced by derivation from the FINAL bag, so no push site can bypass
//! it.

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};

fn build(src: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    crate::build::builder::build(&tree, src.as_bytes())
}

fn zero_span() -> Span {
    Span {
        start: Point { row: 0, column: 0 },
        end: Point { row: 0, column: 0 },
    }
}

#[test]
fn locally_backed_names_stay_off_the_residue() {
    // The writeback attaches `PackageSymbol{Foo, bar}`; `bar` is a
    // row-visible symbol, so the rows can speak for it.
    let fa = build("package Foo;\nsub bar { 42 }\n");
    assert!(!fa.unrowed_attachment_names.iter().any(|n| n == "bar"));
}

#[test]
fn a_bagged_name_with_no_backing_symbol_lands_in_the_residue() {
    let mut fa = build("package Foo;\nsub bar { 42 }\n");
    fa.witnesses.push(Witness {
        attachment: WitnessAttachment::PackageSymbol {
            package: "Foo".to_string(),
            name: "phantom".to_string(),
        },
        source: WitnessSource::Builder("test".into()),
        payload: WitnessPayload::Derivation,
        span: zero_span(),
    });
    fa.seal_unrowed_attachment_names();
    assert!(fa.unrowed_attachment_names.iter().any(|n| n == "phantom"));
    assert!(!fa.unrowed_attachment_names.iter().any(|n| n == "bar"));
}

#[test]
fn locally_inherited_names_lean_on_the_parents_gate_not_the_residue() {
    // The local-inheritance writeback attaches every `Base` method under
    // `Derived`; those names are NOT symbol-backed under `Derived`, but the
    // pre-filter's parents gate already fails open for a class with local
    // parent edges, so listing them would only inflate the resident vec.
    let src = "package Base;\nsub inherited_thing { 42 }\n\
               package Derived;\nour @ISA = ('Base');\nsub own_thing { 1 }\n";
    let fa = build(src);
    assert!(
        !fa.declared_parents("Derived").is_empty(),
        "fixture must record the local parent edge"
    );
    assert!(
        !fa.unrowed_attachment_names.iter().any(|n| n == "inherited_thing"),
        "parent-surface names must lean on the parents gate"
    );
}

#[test]
fn a_slot_key_with_no_backing_ref_lands_in_the_residue() {
    // `conn` is a real hash-key ref (row-visible); `ghostkey` is not.
    let src = "package Foo;\nsub init { my $self = shift; $self->{conn} = 1; }\n";
    let mut fa = build(src);
    for (key, expect_unrowed) in [("ghostkey", true), ("conn", false)] {
        fa.witnesses.push(Witness {
            attachment: WitnessAttachment::Field {
                owner: "Foo".to_string(),
                name: key.to_string(),
            },
            source: WitnessSource::Builder("test".into()),
            payload: WitnessPayload::Derivation,
            span: zero_span(),
        });
        fa.seal_unrowed_attachment_names();
        assert_eq!(
            fa.unrowed_attachment_names.iter().any(|n| n == key),
            expect_unrowed,
            "key {key}"
        );
    }
}

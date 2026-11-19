//! The Perl grammar's vocabulary: its fields and named node kinds as enums,
//! generated from the linked grammar (`build.rs`). A field or kind the
//! grammar does not have is a compile error, and a read through them is by
//! id: `child_by_field_name` and `Node::kind` each search or decode a string
//! per call.

include!(concat!(env!("OUT_DIR"), "/perl_grammar.rs"));

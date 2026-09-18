//! One module per language: what each `LangPack` DECLARES.
//!
//! The contract — `LangPack` and its spec types — stays in
//! `query_extract::packs`; the per-language bodies live here so a
//! language's declarations are one file, and so the contract has no
//! thousand lines of data to scroll past. Each module re-exports its
//! `*_pack()` through `query_extract::packs`, which is the path every
//! caller already spells.

pub mod cmake;
pub mod cpp;
pub mod perl;
pub mod python;
pub mod r;

// A pack whose language feature is off has no caller outside the pack
// tests, which drive every language's pack whatever is compiled in.
#[allow(unused_imports)]
pub use cmake::cmake_pack;
#[allow(unused_imports)]
pub use cpp::cpp_pack;
#[allow(unused_imports)]
pub use perl::perl_pack;
#[allow(unused_imports)]
#[allow(unused_imports)]
pub use python::python_pack;
#[allow(unused_imports)]
pub use r::r_pack;

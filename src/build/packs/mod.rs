//! One module per language: what each `LangPack` DECLARES.
//!
//! The contract — `LangPack` and its spec types — stays in
//! `query_extract::packs`; the per-language bodies live here so a
//! language's declarations are one file, and so the contract has no
//! thousand lines of data to scroll past. Each module re-exports its
//! `*_pack()` through `query_extract::packs`, which is the path every
//! caller already spells.

pub mod cmake;
pub mod perl;
pub mod python;
pub mod r;

pub use cmake::cmake_pack;
pub use perl::perl_pack;
pub use python::python_pack;
pub use r::r_pack;

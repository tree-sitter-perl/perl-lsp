//! One file per language: what each language's `LangPack` declares. The
//! contract (`LangPack` and its spec types) is `query_extract`'s. A language
//! compiles in with its feature; the pack tests drive every one.

#[cfg(any(test, feature = "cmake"))]
mod cmake;
// Compiled in every build: `cpp_reparse`, which is too, peels declarators
// through its document.
#[cfg_attr(not(feature = "cpp"), allow(dead_code))]
mod cpp;
// Not a registered driver: the native builder serves Perl, and the parity
// tests measure this pack against it.
#[cfg(test)]
mod perl;
#[cfg(any(test, feature = "php"))]
mod php;
#[cfg(any(test, feature = "python"))]
mod python;
#[cfg(any(test, feature = "r"))]
mod r;

#[cfg(any(test, feature = "cmake"))]
pub use cmake::cmake_pack;
pub use cpp::cpp_pack;
#[cfg(test)]
pub use perl::perl_pack;
#[cfg(any(test, feature = "php"))]
pub use php::php_pack;
#[cfg(any(test, feature = "python"))]
pub use python::{python_file_module, python_pack, PYTHON_PACKAGE_MARKER};
#[cfg(any(test, feature = "r"))]
pub use r::r_pack;

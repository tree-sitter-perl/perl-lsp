//! php's pack: the declaration, its phpdoc type reader, and composer's
//! install-map discovery (php's dependency-root source).

// No caller outside the pack tests when php is compiled out.
#[cfg_attr(not(feature = "php"), allow(dead_code))]
pub mod composer;
mod doc;


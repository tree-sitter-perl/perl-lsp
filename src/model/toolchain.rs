//! What a C/C++ translation unit compiles with, as plain data: the driver,
//! the language and the standard. An analysis carries the one its build
//! used (`PackFacts::toolchain`), so a query reads the asker's toolchain off
//! the analysis instead of re-deriving it from a path. What a toolchain
//! answers (its include roots and predefined macros) is the build tier's
//! probe, `build/cpp_toolchain.rs`.

use serde::{Deserialize, Serialize};

/// Which language a unit compiles as. C++ adds the `c++/<ver>` roots and
/// `__cplusplus`, so the probe's answer differs per language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Lang {
    C,
    Cpp,
}

/// The identity a toolchain probe answers for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Toolchain {
    /// The command's driver (`arguments[0]`); `None` is the language's
    /// default driver.
    compiler: Option<String>,
    lang: Lang,
    std: Option<String>,
}

impl Toolchain {
    pub fn new(compiler: Option<String>, lang: Lang, std: Option<String>) -> Toolchain {
        Toolchain { compiler, lang, std }
    }

    /// The default driver for `lang`, with no `-std`.
    pub fn for_lang(lang: Lang) -> Toolchain {
        Toolchain { compiler: None, lang, std: None }
    }

    pub fn compiler(&self) -> Option<&str> {
        self.compiler.as_deref()
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    pub fn std(&self) -> Option<&str> {
        self.std.as_deref()
    }

    pub fn heap_bytes(&self) -> usize {
        self.compiler.as_ref().map_or(0, String::capacity) + self.std.as_ref().map_or(0, String::capacity)
    }
}

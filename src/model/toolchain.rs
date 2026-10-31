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

/// What an asker's toolchain derives over a whole index (its reachability
/// config), held by the index for one `resolution_epoch`. The value's type
/// is the build tier's, so the slot holds it erased; any index mutation
/// moves the epoch and the slot drops wholesale.
#[derive(Default)]
pub struct ConfigMemo {
    slot: std::sync::Mutex<(u64, Vec<(ConfigKey, std::sync::Arc<dyn std::any::Any + Send + Sync>)>)>,
}

/// An asker's toolchain, and a fingerprint of its own `#define` sites (an
/// open buffer's defines are fresher than its indexed copy's).
pub type ConfigKey = (Option<Toolchain>, u64);

impl ConfigMemo {
    /// Askers in one project are few toolchains; the cap bounds the open
    /// buffers whose own defines differ.
    const ENTRIES: usize = 8;

    /// The value under `key` at `epoch`, built (outside the lock) when absent.
    pub fn get_or_build<T: Send + Sync + 'static>(
        &self,
        epoch: u64,
        key: ConfigKey,
        build: impl FnOnce() -> T,
    ) -> std::sync::Arc<T> {
        let hit = {
            let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            (slot.0 == epoch)
                .then(|| slot.1.iter().find(|(k, _)| *k == key).map(|(_, v)| std::sync::Arc::clone(v)))
                .flatten()
        };
        if let Some(v) = hit.and_then(|v| v.downcast::<T>().ok()) {
            return v;
        }
        let built = std::sync::Arc::new(build());
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if slot.0 != epoch {
            *slot = (epoch, Vec::new());
        }
        slot.1.retain(|(k, _)| *k != key);
        if slot.1.len() >= Self::ENTRIES {
            slot.1.remove(0);
        }
        slot.1.push((key, std::sync::Arc::clone(&built) as std::sync::Arc<dyn std::any::Any + Send + Sync>));
        built
    }
}

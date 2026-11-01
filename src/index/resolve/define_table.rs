//! Every cached file's `#define` sites by name, kept in step with the index
//! one changed file at a time. Reachability ranking reads its sites from
//! here (`collect::ranking_config`), so a ranked question costs what it
//! reaches, never a sweep over every file's macro table.

use crate::model::file_analysis::{ArmId, CachedModule, CondArms, CondTerm, CrossFileLookup};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

/// The file set is the one `for_each_cached_file` serves, read at
/// `synced`; a file is re-read only when the index holds a different copy
/// of it. Its size is one entry per `#define` in the index.
#[derive(Default)]
pub(crate) struct DefineTable {
    /// The `resolution_epoch` the file set was last read at.
    synced: Option<u64>,
    files: HashMap<PathBuf, u32>,
    slots: Vec<Option<FileDefines>>,
    free: Vec<u32>,
    by_name: HashMap<Arc<str>, Vec<Site>>,
    pass: u64,
}

struct FileDefines {
    /// Identity, not ownership: a strong handle would keep alive a copy the
    /// index has replaced. Held, it also keeps the address from being reused.
    module: Weak<CachedModule>,
    path: PathBuf,
    arms: CondArms,
    names: Vec<Arc<str>>,
    pass: u64,
}

#[derive(Clone, Copy)]
struct Site {
    slot: u32,
    def: u32,
    arm: ArmId,
}

impl DefineTable {
    /// Read the index's file set when it has moved since the last read.
    pub(crate) fn ensure_synced(&mut self, idx: &dyn CrossFileLookup) {
        if self.synced != Some(idx.resolution_epoch()) {
            self.sync(idx, &mut |_| {});
        }
    }

    /// Whether the last read is still the index's file set.
    pub(crate) fn is_current(&self, idx: &dyn CrossFileLookup) -> bool {
        self.synced == Some(idx.resolution_epoch())
    }

    /// Walk the index's files, handing each to `visit`, and re-read the
    /// ones whose copy changed; a file the walk no longer serves drops.
    pub(crate) fn sync(&mut self, idx: &dyn CrossFileLookup, visit: &mut dyn FnMut(&Arc<CachedModule>)) {
        // Read before the walk: a mutation landing during it leaves the
        // table behind the epoch, so the next question reads again.
        let epoch = idx.resolution_epoch();
        self.pass += 1;
        let pass = self.pass;
        idx.for_each_cached_file(&mut |cached| {
            visit(cached);
            if let Some(&id) = self.files.get(&cached.path) {
                let held = self.slots[id as usize].as_mut().expect("a filed path has a slot");
                if std::ptr::eq(held.module.as_ptr(), Arc::as_ptr(cached)) {
                    held.pass = pass;
                    return;
                }
                self.unload(id);
            }
            self.load(cached, pass);
        });
        let gone: Vec<u32> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().filter(|s| s.pass != pass).map(|_| i as u32))
            .collect();
        for id in gone {
            self.unload(id);
        }
        self.synced = Some(epoch);
    }

    /// Call `f` with the trail of every `#define` of `name` the index holds.
    pub(crate) fn sites_of(&self, name: &str, f: &mut dyn FnMut(&[CondTerm])) {
        for site in self.by_name.get(name).into_iter().flatten() {
            if let Some(file) = &self.slots[site.slot as usize] {
                f(file.arms.trail(site.arm));
            }
        }
    }

    /// The files holding a `#define` of `name`, each with that define's
    /// index into its `macro_defs`. `None` when a held copy is gone — the
    /// table is behind the index, and the caller reads the index instead.
    pub(crate) fn defs_named(&self, name: &str) -> Option<Vec<(Arc<CachedModule>, usize)>> {
        let mut out = Vec::new();
        for site in self.by_name.get(name).into_iter().flatten() {
            let module = self.slots[site.slot as usize].as_ref()?.module.upgrade()?;
            let def = site.def as usize;
            if module.analysis.pack.macro_defs.get(def).is_none_or(|m| m.name != name) {
                return None;
            }
            out.push((module, def));
        }
        Some(out)
    }

    fn load(&mut self, cached: &Arc<CachedModule>, pass: u64) {
        let id = match self.free.pop() {
            Some(id) => id,
            None => {
                self.slots.push(None);
                (self.slots.len() - 1) as u32
            }
        };
        let pack = &cached.analysis.pack;
        let mut names: Vec<Arc<str>> = Vec::new();
        for (i, m) in pack.macro_defs.iter().enumerate() {
            let site = Site { slot: id, def: i as u32, arm: m.arm };
            let key = match self.by_name.get_key_value(m.name.as_str()) {
                Some((k, _)) => Arc::clone(k),
                None => Arc::from(m.name.as_str()),
            };
            let sites = self.by_name.entry(Arc::clone(&key)).or_default();
            if sites.last().is_none_or(|s| s.slot != id) {
                names.push(key);
            }
            sites.push(site);
        }
        let arms = if pack.macro_defs.is_empty() { CondArms::default() } else { pack.cond_arms.clone() };
        self.slots[id as usize] =
            Some(FileDefines { module: Arc::downgrade(cached), path: cached.path.clone(), arms, names, pass });
        self.files.insert(cached.path.clone(), id);
    }

    fn unload(&mut self, id: u32) {
        let Some(file) = self.slots[id as usize].take() else { return };
        for name in &file.names {
            if let Some(sites) = self.by_name.get_mut(name) {
                sites.retain(|s| s.slot != id);
                if sites.is_empty() {
                    self.by_name.remove(name);
                }
            }
        }
        self.files.remove(&file.path);
        self.free.push(id);
    }
}

#[cfg(test)]
#[path = "define_table_tests.rs"]
mod tests;

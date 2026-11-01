//! The table follows the index: a changed copy is re-read, a file the index
//! drops leaves, and an unmoved epoch costs no walk.

use super::*;
use crate::model::file_analysis::{CondTest, FileAnalysis, MacroDef, Span, Symbol};
use std::path::Path;

fn defines(defs: &[(&str, &[CondTerm])]) -> FileAnalysis {
    let src = "1;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    let mut fa = crate::build::builder::build(&tree, src.as_bytes());
    for (row, (name, trail)) in defs.iter().enumerate() {
        let arm = fa.pack.cond_arms.intern(trail.to_vec());
        let at = tree_sitter::Point { row, column: 8 };
        fa.pack.macro_defs.push(MacroDef {
            name: name.to_string(),
            params: None,
            body: String::new(),
            arm,
            selection_span: Span { start: at, end: at },
            delegate: None,
        });
    }
    fa
}

fn guard(name: &str) -> CondTerm {
    CondTerm { test: CondTest::Defined(name.to_string()), negated: false }
}

#[derive(Default)]
struct Files {
    files: std::cell::RefCell<Vec<Arc<CachedModule>>>,
    epoch: std::cell::Cell<u64>,
    walks: std::cell::Cell<u32>,
}

impl Files {
    fn put(&self, path: &str, fa: FileAnalysis) {
        let mut files = self.files.borrow_mut();
        files.retain(|c| c.path != Path::new(path));
        files.push(Arc::new(CachedModule::new(PathBuf::from(path), Arc::new(fa))));
        self.epoch.set(self.epoch.get() + 1);
    }
    fn drop_file(&self, path: &str) {
        self.files.borrow_mut().retain(|c| c.path != Path::new(path));
        self.epoch.set(self.epoch.get() + 1);
    }
}

impl CrossFileLookup for Files {
    fn resolution_epoch(&self) -> u64 {
        self.epoch.get()
    }
    fn for_each_cached_file(&self, f: &mut dyn FnMut(&Arc<CachedModule>)) {
        self.walks.set(self.walks.get() + 1);
        for c in self.files.borrow().iter() {
            f(c);
        }
    }
    fn get_cached(&self, _m: &str) -> Option<Arc<CachedModule>> {
        None
    }
    fn modules_with_symbol(&self, _n: &str) -> Vec<String> {
        Vec::new()
    }
    fn find_exporters(&self, _n: &str) -> Vec<String> {
        Vec::new()
    }
    fn defining_module_cached(&self, _e: &str, _n: &str) -> Option<Arc<CachedModule>> {
        None
    }
    fn module_declaring_method_in_package(&self, _p: &str, _m: &str) -> Option<String> {
        None
    }
    fn for_each_cached(&self, _f: &mut dyn FnMut(&str, &Arc<CachedModule>)) {}
    fn for_each_reexport_module(
        &self,
        _s: Vec<String>,
        _v: &mut dyn FnMut(&str, &Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    ) {
    }
    fn for_each_entity_bridged_to(
        &self,
        _c: &str,
        _f: &mut dyn FnMut(&str, &Arc<CachedModule>, &Symbol) -> std::ops::ControlFlow<()>,
    ) {
    }
    fn direct_children_of(&self, _p: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    fn for_each_loader_shape(
        &self,
        _f: &mut dyn FnMut(&str, &crate::model::file_analysis::InferredType),
    ) {
    }
}

fn trails(t: &DefineTable, name: &str) -> Vec<Vec<CondTerm>> {
    let mut out = Vec::new();
    t.sites_of(name, &mut |trail| out.push(trail.to_vec()));
    out.sort_by_key(|t| format!("{t:?}"));
    out
}

fn paths(t: &DefineTable, name: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        t.defs_named(name).expect("current").into_iter().map(|(c, _)| c.path.clone()).collect();
    out.sort();
    out
}

#[test]
fn the_table_follows_the_index_file_by_file() {
    let idx = Files::default();
    idx.put("/p/a.h", defines(&[("KNOB", &[guard("PLAT")]), ("A", &[])]));
    idx.put("/p/b.h", defines(&[("KNOB", &[]), ("B", &[guard("KNOB")])]));
    let mut t = DefineTable::default();
    t.ensure_synced(&idx);
    assert_eq!(trails(&t, "KNOB"), vec![vec![guard("PLAT")], vec![]]);
    assert_eq!(paths(&t, "KNOB"), vec![PathBuf::from("/p/a.h"), PathBuf::from("/p/b.h")]);

    // An unmoved index is not walked again.
    let walks = idx.walks.get();
    t.ensure_synced(&idx);
    assert_eq!(idx.walks.get(), walks);

    // A new copy with the same names answers from the new copy's arms.
    idx.put("/p/b.h", defines(&[("KNOB", &[guard("OTHER")]), ("B", &[guard("KNOB")])]));
    t.ensure_synced(&idx);
    assert_eq!(trails(&t, "KNOB"), vec![vec![guard("OTHER")], vec![guard("PLAT")]]);

    // A new copy of b.h replaces its sites; a.h's stay.
    idx.put("/p/b.h", defines(&[("B", &[])]));
    t.ensure_synced(&idx);
    assert_eq!(trails(&t, "KNOB"), vec![vec![guard("PLAT")]]);
    assert_eq!(trails(&t, "B"), vec![vec![]]);

    // A file the index drops leaves the table, and its names with it.
    idx.drop_file("/p/a.h");
    t.ensure_synced(&idx);
    assert!(trails(&t, "KNOB").is_empty());
    assert!(trails(&t, "A").is_empty());
    assert_eq!(paths(&t, "B"), vec![PathBuf::from("/p/b.h")]);
}

/// A copy the table still names but the index no longer holds is not an
/// answer: `defs_named` says the table is behind.
#[test]
fn a_gone_copy_reads_as_behind() {
    let idx = Files::default();
    idx.put("/p/a.h", defines(&[("K", &[])]));
    let mut t = DefineTable::default();
    t.ensure_synced(&idx);
    idx.files.borrow_mut().clear();
    assert!(t.defs_named("K").is_none());
}

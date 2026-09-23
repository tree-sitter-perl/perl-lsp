//! Consult pre-filter tests: the chase-shape gates that keep the
//! rows-backed skip honest. The rows half's truth table is
//! `member_prefilter_may_declare`'s (tested at its definition); these pin
//! that every candidate-LOCAL answer route the rows cannot see — declared
//! parents, dynamic parents, the app-surface edge, the unrowed residue —
//! fails OPEN over a store that swears the candidate is silent.

use super::sweep_candidate_may_answer;
use crate::model::file_analysis::{CachedModule, CrossFileLookup, FileAnalysis};
use std::path::PathBuf;
use std::sync::Arc;

fn build(src: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    crate::build::builder::build(&tree, src.as_bytes())
}

fn module(src: &str) -> Arc<CachedModule> {
    Arc::new(CachedModule::new(PathBuf::from("/t/mod.pm"), Arc::new(build(src))))
}

/// A lookup whose rows half answers a FIXED verdict, so each test can pin
/// that a gate dominates it (or that the verdict is reached at all).
struct RowsSay(bool);

impl CrossFileLookup for RowsSay {
    fn get_cached(&self, _m: &str) -> Option<Arc<CachedModule>> {
        None
    }
    fn holders_with_symbol(&self, _n: &str) -> Vec<crate::model::file_analysis::Holder> {
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
        _v: &mut dyn FnMut(&Arc<CachedModule>) -> std::ops::ControlFlow<()>,
    ) {
    }
    fn for_each_entity_bridged_to(
        &self,
        _c: &str,
        _f: &mut dyn FnMut(
            &str,
            &Arc<CachedModule>,
            &crate::model::file_analysis::Symbol,
        ) -> std::ops::ControlFlow<()>,
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
    fn candidate_bag_may_answer(
        &self,
        _cached: &Arc<CachedModule>,
        _name: &str,
        _class: &str,
        _attributed: bool,
    ) -> bool {
        self.0
    }
}

#[test]
fn a_provably_silent_candidate_is_skipped() {
    let cached = module("package main;\nsub other { 1 }\n");
    assert!(
        !sweep_candidate_may_answer(&RowsSay(false), &cached, "main", "ghost_sub", true),
        "no gate fires and the rows prove absence: the one skip"
    );
}

#[test]
fn a_row_backed_candidate_is_attempted() {
    let cached = module("package main;\nsub other { 1 }\n");
    assert!(sweep_candidate_may_answer(
        &RowsSay(true),
        &cached,
        "main",
        "other",
        true
    ));
}

#[test]
fn declared_parents_fail_open_over_provably_absent_rows() {
    // The attempt walks the candidate's OWN parent declarations even when
    // its bag holds nothing for the name — a parent may answer.
    let cached = module("package main;\nuse parent -norequire, 'Base';\n");
    assert!(
        !cached.analysis.declared_parents("main").is_empty(),
        "fixture must actually declare a parent"
    );
    assert!(sweep_candidate_may_answer(
        &RowsSay(false),
        &cached,
        "main",
        "ghost_sub",
        true
    ));
}

#[test]
fn the_unrowed_residue_fails_open_over_provably_absent_rows() {
    let mut fa = build("package main;\nsub other { 1 }\n");
    fa.unrowed_attachment_names = vec!["ghost_sub".to_string()];
    let cached = Arc::new(CachedModule::new(PathBuf::from("/t/mod.pm"), Arc::new(fa)));
    assert!(sweep_candidate_may_answer(
        &RowsSay(false),
        &cached,
        "main",
        "ghost_sub",
        true
    ));
    // A different name still skips — the residue is per-name, not per-file.
    assert!(!sweep_candidate_may_answer(
        &RowsSay(false),
        &cached,
        "main",
        "another_ghost",
        true
    ));
}

#[test]
fn an_app_surface_consumer_class_fails_open() {
    let mut fa = build("package main;\nsub other { 1 }\n");
    fa.plugin.app_surface_consumers.push("main".to_string());
    let cached = Arc::new(CachedModule::new(PathBuf::from("/t/mod.pm"), Arc::new(fa)));
    assert!(sweep_candidate_may_answer(
        &RowsSay(false),
        &cached,
        "main",
        "ghost_sub",
        true
    ));
}

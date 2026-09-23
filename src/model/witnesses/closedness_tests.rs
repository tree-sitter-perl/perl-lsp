//! Certificate tests. The arrival case is the one that matters most: it is
//! the half of the validity key that per-provider fingerprints structurally
//! cannot see, and it is the half whose failure is a wrong answer.

use super::*;
use crate::model::file_analysis::{CachedModule, CrossFileLookup, FileAnalysis};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn build(src: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    crate::build::builder::build(&tree, src.as_bytes())
}

/// A world the tests can move under a certificate's feet.
#[derive(Default)]
struct World {
    providers: HashMap<String, Vec<Arc<CachedModule>>>,
    fingerprints: HashMap<PathBuf, u64>,
    bridged: HashSet<String>,
    /// Moves when something registers. `mint` brackets itself with this.
    epoch: std::cell::Cell<u64>,
    /// Land a registration mid-mint: the ancestry walk has been read, the
    /// fingerprint reads are starting. That is the exact straddle, because a
    /// registration records its surface before publishing its candidate.
    bump_on_fingerprint: std::cell::Cell<bool>,
}

impl World {
    fn provide(&mut self, class: &str, path: &str, src: &str, fp: u64) -> &mut Self {
        let p = PathBuf::from(path);
        self.providers.entry(class.to_string()).or_default().push(Arc::new(
            CachedModule::new(p.clone(), Arc::new(build(src))),
        ));
        self.fingerprints.insert(p, fp);
        self
    }
}

impl CrossFileLookup for World {
    fn resolution_epoch(&self) -> u64 {
        self.epoch.get()
    }
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
    fn visible_def_candidates(&self, name: &str) -> Vec<Arc<CachedModule>> {
        self.providers.get(name).cloned().unwrap_or_default()
    }
    fn surface_fingerprint_of(&self, path: &Path) -> Option<u64> {
        if self.bump_on_fingerprint.replace(false) {
            self.epoch.set(self.epoch.get() + 1);
        }
        self.fingerprints.get(path).copied()
    }
    // The trait DEFAULTS this to `true` (pessimistic). A fake that forgot to
    // override it would decline every certificate and every test below would
    // pass for the wrong reason.
    fn class_is_bridged_to(&self, class: &str) -> bool {
        self.bridged.contains(class)
    }
}

const CHILD: &str = "package Child;\nuse parent -norequire, 'Base';\nsub c { 1 }\n1;\n";
const BASE: &str = "package Base;\nsub b { 1 }\n1;\n";

fn world() -> World {
    let mut w = World::default();
    w.provide("Child", "/w/Child.pm", CHILD, 11);
    w.provide("Base", "/w/Base.pm", BASE, 22);
    w
}

#[test]
fn a_certificate_validates_against_the_world_it_was_minted_from() {
    let w = world();
    let origin = build(CHILD);
    let cert = ClosednessCertificate::mint(&w, &origin, "Child").expect("mintable");
    assert!(cert.closure_len() >= 2, "the closure must reach Base, not just Child");
    assert!(cert.is_valid(&w), "a certificate must validate against its own world");
    assert!(cert.heap_bytes() > 0);
}

/// The arrival case — the reason provider-set identity is in the key at all.
///
/// A new file providing an ancestor name changes what the class inherits
/// while every fingerprint the certificate recorded still stands. A key made
/// only of per-provider fingerprints validates happily here and the trusted
/// silence becomes a confident lie about a method that now exists.
#[test]
fn a_new_provider_for_an_ancestor_invalidates() {
    let w = world();
    let origin = build(CHILD);
    let cert = ClosednessCertificate::mint(&w, &origin, "Child").expect("mintable");
    assert!(cert.is_valid(&w));

    let mut after = world();
    after.provide("Base", "/w/vendor/Base.pm", "package Base;\nsub arrived { 1 }\n1;\n", 33);
    assert!(
        !cert.is_valid(&after),
        "a NEW file providing an ancestor name left the certificate valid — \
         every recorded fingerprint still stands, so only the provider-set \
         identity can catch this, and trusting it would serve silence about \
         a method that now exists"
    );
}

#[test]
fn an_edited_provider_invalidates() {
    let w = world();
    let origin = build(CHILD);
    let cert = ClosednessCertificate::mint(&w, &origin, "Child").expect("mintable");

    let mut after = World::default();
    after.provide("Child", "/w/Child.pm", CHILD, 11);
    after.provide("Base", "/w/Base.pm", BASE, 999); // same provider, moved surface
    assert!(!cert.is_valid(&after), "an edited provider must invalidate");
}

#[test]
fn a_departed_provider_invalidates() {
    let w = world();
    let origin = build(CHILD);
    let cert = ClosednessCertificate::mint(&w, &origin, "Child").expect("mintable");

    let mut after = World::default();
    after.provide("Child", "/w/Child.pm", CHILD, 11);
    // Base provides nothing now.
    assert!(!cert.is_valid(&after), "a departed provider must invalidate");
}

/// Exclusions ride the VALUE, not a list of names we happen to know about.
#[test]
fn a_bridged_class_in_the_closure_declines() {
    let mut w = world();
    w.bridged.insert("Base".to_string());
    let origin = build(CHILD);
    assert!(
        ClosednessCertificate::mint(&w, &origin, "Child").is_none(),
        "a plugin namespace can bridge content onto a class with no file \
         declaring it, so the ancestry walk does not see the whole world"
    );
}

#[test]
fn a_bridge_arriving_after_mint_invalidates() {
    let w = world();
    let origin = build(CHILD);
    let cert = ClosednessCertificate::mint(&w, &origin, "Child").expect("mintable");
    assert!(cert.is_valid(&w));

    let mut after = world();
    after.bridged.insert("Base".to_string());
    assert!(
        !cert.is_valid(&after),
        "a plugin bridged content onto an ancestor AFTER the certificate was \
         minted. No provider arrived and no fingerprint moved, so nothing in \
         the recorded key can see it — the exclusion has to be re-asked, not \
         just checked at mint. Trusting this serves silence about a method \
         that now exists."
    );
}

#[test]
fn a_dynamic_parent_list_in_the_closure_declines() {
    let mut w = World::default();
    w.provide("Child", "/w/Child.pm", CHILD, 11);
    // A `with` argument that does not fold to a constant — a runtime-generated
    // role. The parent list is not statically knowable, so no fingerprint
    // could make this closure safe.
    w.provide(
        "Base",
        "/w/Base.pm",
        "package Base;\nuse Moo;\nwith ReportProxy(type => 'x');\nsub b { 1 }\n1;\n",
        22,
    );
    let origin = build(CHILD);
    let minted = ClosednessCertificate::mint(&w, &origin, "Child");
    if !w.providers["Base"][0].analysis.has_dynamic_parents("Base") {
        // The fixture did not produce the shape; say so rather than passing
        // for the wrong reason.
        panic!("fixture no longer yields dynamic parents — the exclusion is untested");
    }
    assert!(minted.is_none(), "a dynamic parent list in the closure must decline");
}

/// A provider the index cannot vouch for declines, exactly as an unrecorded
/// path makes a conclusions row read absent.
#[test]
fn an_unvouched_provider_declines() {
    let mut w = World::default();
    w.provide("Child", "/w/Child.pm", CHILD, 11);
    w.provide("Base", "/w/Base.pm", BASE, 22);
    w.fingerprints.remove(Path::new("/w/Base.pm"));
    let origin = build(CHILD);
    assert!(
        ClosednessCertificate::mint(&w, &origin, "Child").is_none(),
        "no freshness record means the index cannot vouch for the provider"
    );
}

#[test]
fn a_registration_across_the_mint_declines() {
    // The closure is read from the candidates and the fingerprints from the
    // freshness index — two reads of shared mutable state. A registration
    // records its surface BEFORE it publishes its candidate, so a mint that
    // straddles one pairs NEW fingerprints with an OLD closure. Every
    // recorded pair then reads as current, and it validates forever over an
    // ancestry it never enumerated.
    let w = world();
    let origin = build(CHILD);
    w.bump_on_fingerprint.set(true);
    assert!(
        ClosednessCertificate::mint(&w, &origin, "Child").is_none(),
        "a registration landed between the ancestry walk and the fingerprint \
         reads and the mint still produced a certificate — its pairs are \
         individually current, so nothing downstream can ever notice that the \
         closure half is stale"
    );

    // Control: the same world, still, mints.
    w.bump_on_fingerprint.set(false);
    assert!(ClosednessCertificate::mint(&w, &origin, "Child").is_some());
}

#[test]
fn a_truncated_ancestry_walk_declines() {
    // `mint` declines on closure WIDTH. Depth is cut off by the graph bound
    // with no signal to the visitor, so without an explicit truncation report
    // a chain deeper than the bound certifies a PREFIX of its ancestry and
    // the names below the cut never invalidate it.
    let mut w = World::default();
    // A chain longer than WalkBound::GRAPH's depth.
    let n = 40usize;
    for i in 0..n {
        let cls = format!("C{i}");
        let src = if i + 1 < n {
            format!("package C{i};\nuse parent -norequire, 'C{}';\n1;\n", i + 1)
        } else {
            format!("package C{i};\nsub leaf {{ 1 }}\n1;\n")
        };
        w.provide(&cls, &format!("/w/C{i}.pm"), &src, 100 + i as u64);
    }
    let origin = build("package C0;\nuse parent -norequire, 'C1';\n1;\n");
    assert!(
        ClosednessCertificate::mint(&w, &origin, "C0").is_none(),
        "a chain deeper than the graph bound minted a certificate over the \
         prefix the walk happened to see — the ancestors below the cut are \
         absent from the closure, so their providers change without ever \
         invalidating it"
    );
}

//! A member access on a union receiver names the member on every arm, in
//! every projection.

use super::*;
use crate::index::module_index::ModuleIndex;
use std::sync::Arc;

const CAT: &str = "package Cat;\nsub new { bless {}, shift }\nsub sound { 'meow' }\n1;\n";
const BIRD: &str = "package Bird;\nsub new { bless {}, shift }\nsub sound { 'tweet' }\n1;\n";
const ZOO: &str = "package Zoo;\nuse Cat;\nuse Bird;\n\
sub pick { my $pet = rand() ? Cat->new : Bird->new; return $pet->sound; }\n\
sub cat { my $c = Cat->new; return $c->sound; }\n1;\n";

struct Fixture {
    store: OpenDocs,
    idx: ModuleIndex,
    cat: PathBuf,
    bird: PathBuf,
    zoo: PathBuf,
}

fn fixture() -> Fixture {
    let store = OpenDocs::new();
    let idx = ModuleIndex::new_for_test();
    let (cat, bird, zoo) =
        (PathBuf::from("/tmp/un_cat.pm"), PathBuf::from("/tmp/un_bird.pm"), PathBuf::from("/tmp/un_zoo.pm"));
    for (p, src) in [(&cat, CAT), (&bird, BIRD), (&zoo, ZOO)] {
        idx.register_workspace_module(p.clone(), Arc::new(parse(src)));
        idx.insert_workspace_for_test(p.clone(), parse(src));
    }
    Fixture { store, idx, cat, bird, zoo }
}

fn at<'a>(f: &'a Fixture, fa: &'a FileAnalysis, path: &Path, row: usize, col: usize) -> CandidateSet<'a> {
    resolve(
        &f.store,
        fa,
        FileKey::Path(path.to_path_buf()),
        tree_sitter::Point { row, column: col },
        Some(&f.idx),
        OverrideScope::default(),
    )
}

fn in_file(locs: &[RefLocation], path: &Path, row: usize) -> bool {
    locs.iter().any(|l| matches!(&l.key, FileKey::Path(p) if p == path) && l.span.start.row == row)
}

use std::path::Path;

/// The `sound` in `$pet->sound`.
fn union_site(f: &Fixture) -> (Arc<FileAnalysis>, usize, usize) {
    let fa = f.idx.workspace_file_for_test(&f.zoo).unwrap();
    let line = ZOO.lines().nth(3).unwrap();
    (fa, 3, line.find("sound").unwrap())
}

#[test]
fn goto_def_on_a_union_receiver_offers_every_arm() {
    let f = fixture();
    let (fa, row, col) = union_site(&f);
    let defs = at(&f, &fa, &f.zoo, row, col).definitions();
    assert!(in_file(&defs, &f.cat, 2) && in_file(&defs, &f.bird, 2), "both arms' `sound`: {defs:?}");
}

#[test]
fn references_on_a_union_receiver_reach_every_arm() {
    let f = fixture();
    let (fa, row, col) = union_site(&f);
    let refs = at(&f, &fa, &f.zoo, row, col).references();
    assert!(in_file(&refs, &f.cat, 2) && in_file(&refs, &f.bird, 2), "both decls: {refs:?}");
    assert!(in_file(&refs, &f.zoo, 3), "the union site: {refs:?}");
    assert!(in_file(&refs, &f.zoo, 4), "Cat's own call: {refs:?}");
}

#[test]
fn an_arms_references_include_the_union_site() {
    let f = fixture();
    let fa = f.idx.workspace_file_for_test(&f.cat).unwrap();
    let col = CAT.lines().nth(2).unwrap().find("sound").unwrap();
    let refs = at(&f, &fa, &f.cat, 2, col).references();
    assert!(in_file(&refs, &f.zoo, 3), "`$pet` may be a Cat: {refs:?}");
    assert!(in_file(&refs, &f.zoo, 4), "{refs:?}");
    assert!(!in_file(&refs, &f.bird, 2), "Bird's own decl is not Cat's: {refs:?}");
}

#[test]
fn rename_on_a_union_receiver_renames_every_arm() {
    let f = fixture();
    let (fa, row, col) = union_site(&f);
    let edits = at(&f, &fa, &f.zoo, row, col).rename_edits("noise").expect("renames");
    let locs: Vec<RefLocation> = edits.into_iter().map(|(l, _)| l).collect();
    assert!(in_file(&locs, &f.cat, 2) && in_file(&locs, &f.bird, 2), "{locs:?}");
    assert!(in_file(&locs, &f.zoo, 3) && in_file(&locs, &f.zoo, 4), "{locs:?}");
}

#[test]
fn completion_on_a_union_offers_the_members_every_arm_has() {
    let fa = parse(
        "package Cat;\nsub new { bless {}, shift }\nsub sound { 1 }\nsub purr { 1 }\n\
         package Bird;\nsub new { bless {}, shift }\nsub sound { 1 }\nsub fly { 1 }\n\
         package main;\nmy $pet = rand() ? Cat->new : Bird->new;\n1;\n",
    );
    let ty = fa.inferred_type_via_bag("$pet", tree_sitter::Point { row: 10, column: 0 }).expect("typed");
    let labels: Vec<String> = fa
        .shared_members(&ty, |c| fa.complete_methods_for_class(c, None))
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(labels.iter().any(|l| l == "sound"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "purr" || l == "fly"), "{labels:?}");
}

#[test]
fn rename_from_one_arm_refuses_the_union_site() {
    // Renaming Cat's `sound` alone would leave `$pet->sound` calling a
    // method Cat no longer has whenever `$pet` is a Cat.
    let f = fixture();
    let fa = f.idx.workspace_file_for_test(&f.cat).unwrap();
    let col = CAT.lines().nth(2).unwrap().find("sound").unwrap();
    let err = at(&f, &fa, &f.cat, 2, col).rename_edits("noise").expect_err("refuses");
    assert!(err.contains("Bird") && err.contains("un_zoo.pm:4"), "{err}");
}

#[test]
fn rename_from_the_union_site_refuses_an_arm_it_does_not_cover() {
    // A second union site names Fish too: renaming Cat's and Bird's `sound`
    // together would still break it for a Fish.
    let store = OpenDocs::new();
    let idx = ModuleIndex::new_for_test();
    let fish = "package Fish;\nsub new { bless {}, shift }\nsub sound { 'blub' }\n1;\n";
    let tank = "package Tank;\nuse Cat;\nuse Fish;\n\
                sub pick { my $pet = rand() ? Cat->new : Fish->new; return $pet->sound; }\n1;\n";
    let files = [
        ("/tmp/un2_cat.pm", CAT),
        ("/tmp/un2_bird.pm", BIRD),
        ("/tmp/un2_fish.pm", fish),
        ("/tmp/un2_zoo.pm", ZOO),
        ("/tmp/un2_tank.pm", tank),
    ];
    for (p, src) in files {
        idx.register_workspace_module(PathBuf::from(p), Arc::new(parse(src)));
        idx.insert_workspace_for_test(PathBuf::from(p), parse(src));
    }
    let zoo = PathBuf::from("/tmp/un2_zoo.pm");
    let fa = idx.workspace_file_for_test(&zoo).unwrap();
    let col = ZOO.lines().nth(3).unwrap().find("sound").unwrap();
    let cs = resolve(
        &store,
        &fa,
        FileKey::Path(zoo.clone()),
        tree_sitter::Point { row: 3, column: col },
        Some(&idx),
        OverrideScope::default(),
    );
    let err = cs.rename_edits("noise").expect_err("refuses");
    assert!(err.contains("Fish") && err.contains("un2_tank.pm:4"), "{err}");
}

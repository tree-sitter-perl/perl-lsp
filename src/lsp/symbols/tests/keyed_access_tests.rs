//! Keyed access, Perl and PHP side by side: each case is one program in both
//! languages with the same line layout, driven through the production
//! projections (goto-def, references, rename, hover). Where the PHP pack is
//! meant to answer exactly as Perl does, the two languages' answers are
//! compared row for row; the few places it departs are pinned separately.

use crate::index::file_store::{FileKey, FileStore};
use crate::index::resolve::{self, OverrideScope, RoleMask, TargetKind, TargetRef};
use crate::model::file_analysis::{AccessKind, FileAnalysis, RefKind};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
enum Lang {
    Perl,
    Php,
}

impl Lang {
    fn analyze(self, src: &str) -> FileAnalysis {
        match self {
            Lang::Perl => super::parse_analysis(src),
            Lang::Php => crate::build::language_driver::LanguageRegistry::with_enabled()
                .for_id("php")
                .expect("php driver")
                .analyze(src),
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Lang::Perl => "pl",
            Lang::Php => "php",
        }
    }
}

/// Every verb's answer at one cursor, reduced to (file, row) so the two
/// languages compare even though their key columns differ.
#[derive(Debug, PartialEq)]
struct Answers {
    defs: BTreeSet<(String, usize)>,
    refs: BTreeSet<(String, usize)>,
    rename: Option<BTreeSet<(String, usize)>>,
    hover: Option<String>,
}

fn file_of(key: &FileKey) -> String {
    match key {
        FileKey::Path(p) => p.file_name().unwrap().to_string_lossy().to_string(),
        FileKey::Url(u) => u.to_string(),
    }
}

/// The cursor one character into the `nth` (1-based) `needle` on `row`.
fn cursor(src: &str, row: usize, needle: &str, nth: usize) -> tree_sitter::Point {
    let line = src.lines().nth(row).expect("row");
    let mut at = None;
    for _ in 0..nth {
        let from = at.map_or(0, |c| c + 1);
        at = Some(from + line[from..].find(needle).expect("needle on row"));
    }
    tree_sitter::Point { row, column: at.unwrap() + 1 }
}

/// Run the four verbs from `(row, needle)` in `files[0]`.
fn answers(lang: Lang, files: &[(&str, &str)], row: usize, needle: &str, nth: usize) -> Answers {
    let store = FileStore::new();
    let mut origin = None;
    for (i, (name, src)) in files.iter().enumerate() {
        let path = PathBuf::from(format!("/tmp/keyed/{name}.{}", lang.ext()));
        let fa = Arc::new(lang.analyze(src));
        store.insert_workspace_arc(path.clone(), fa.clone());
        if i == 0 {
            origin = Some((path, fa));
        }
    }
    let (path, fa) = origin.unwrap();
    let src = files[0].1;
    let point = cursor(src, row, needle, nth);
    let cs = resolve::resolve(&store, &fa, FileKey::Path(path), point, None, OverrideScope::default())
        .with_source(src);
    let rows = |locs: Vec<resolve::RefLocation>| -> BTreeSet<(String, usize)> {
        locs.iter().map(|l| (file_of(&l.key), l.span.start.row)).collect()
    };
    let rename = cs.rename_edits("renamed").ok().map(|edits| {
        assert!(edits.iter().all(|(_, t)| t == "renamed"), "every edit writes the new key");
        edits.iter().map(|(l, _)| (file_of(&l.key), l.span.start.row)).collect()
    });
    let hover = match lang {
        Lang::Perl => fa.hover_info(point, src, None),
        Lang::Php => crate::lsp::symbols::pack_hover_markdown(&cs, "php"),
    };
    Answers { defs: rows(cs.definitions()), refs: rows(cs.references()), rename, hover }
}

fn set(rows: &[(&str, usize)]) -> BTreeSet<(String, usize)> {
    rows.iter().map(|(f, r)| (f.to_string(), *r)).collect()
}

fn key_refs_on_row(fa: &FileAnalysis, row: usize) -> usize {
    fa.refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::HashKeyAccess { .. }) && r.span.start.row == row)
        .count()
}

/// The keys of a sub's returned literal, reached through the variable the
/// call is stored in (`test_refs_to_finds_hash_key_def_and_access_same_file`
/// and the e2e `$db_config->{host}` goto-def, in both languages).
#[test]
fn returned_literal_keys_navigate_the_same_in_both_languages() {
    let perl = "use strict;\n\
                sub get_config { return { host => 1, port => 2 }; }\n\
                my $cfg = get_config();\n\
                my $h = $cfg->{host};\n";
    let php = "<?php\n\
               function get_config() { return ['host' => 1, 'port' => 2]; }\n\
               $cfg = get_config();\n\
               $h = $cfg['host'];\n";
    let pl = answers(Lang::Perl, &[("a", perl)], 3, "host", 1);
    let ph = answers(Lang::Php, &[("a", php)], 3, "host", 1);
    assert_eq!(pl.defs, set(&[("a.pl", 1)]), "perl goto-def: {pl:?}");
    assert_eq!(pl.refs, set(&[("a.pl", 1), ("a.pl", 3)]), "perl references: {pl:?}");
    assert_eq!(pl.rename, Some(pl.refs.clone()), "perl rename: {pl:?}");
    let rename_php = |a: &Answers| {
        a.rename.as_ref().map(|r| r.iter().map(|(f, row)| (f.replace(".php", ".pl"), *row)).collect::<BTreeSet<_>>())
    };
    let as_pl = |s: &BTreeSet<(String, usize)>| -> BTreeSet<(String, usize)> {
        s.iter().map(|(f, r)| (f.replace(".php", ".pl"), *r)).collect()
    };
    assert_eq!(as_pl(&ph.defs), pl.defs, "php goto-def matches perl: {ph:?}");
    assert_eq!(as_pl(&ph.refs), pl.refs, "php references match perl: {ph:?}");
    assert_eq!(rename_php(&ph), pl.rename, "php rename matches perl: {ph:?}");
    assert!(pl.hover.as_deref().is_some_and(|h| h.contains("host")), "perl hover: {pl:?}");
    assert!(
        ph.hover.as_deref().is_some_and(|h| h.contains("'host' => 1") && h.contains("array key")),
        "php hover names the key's declaring line: {ph:?}",
    );
    // From the def side, the same set.
    let pl_def = answers(Lang::Perl, &[("a", perl)], 1, "host", 1);
    let ph_def = answers(Lang::Php, &[("a", php)], 1, "host", 1);
    assert_eq!(as_pl(&ph_def.refs), pl_def.refs, "def-side references agree: {ph_def:?}");
}

/// Two classes with a same-named method returning the same key never share
/// it (`test_refs_to_package_qualified_sub_owner_isolates_name_collisions`),
/// and a typed receiver reaches the method's keys (the e2e chained
/// `$calc->get_self->get_config->{host}` shape).
#[test]
fn method_return_keys_follow_the_receivers_class_in_both_languages() {
    let perl = "package Alpha; sub new { bless {}, shift } sub get_config { return { host => 'a' }; }\n\
                package Beta; sub new { bless {}, shift } sub get_config { return { host => 'b' }; }\n\
                package main;\n\
                my $x = Alpha->new;\n\
                print $x->get_config->{host};\n";
    let php = "<?php class Alpha { function get_config() { return ['host' => 'a']; } }\n\
               class Beta { function get_config() { return ['host' => 'b']; } }\n\
               \n\
               $x = new Alpha();\n\
               echo $x->get_config()['host'];\n";
    let pl = answers(Lang::Perl, &[("a", perl)], 4, "host", 1);
    let ph = answers(Lang::Php, &[("a", php)], 4, "host", 1);
    assert_eq!(pl.refs, set(&[("a.pl", 0), ("a.pl", 4)]), "perl: Alpha's key only: {pl:?}");
    assert_eq!(ph.refs, set(&[("a.php", 0), ("a.php", 4)]), "php: Alpha's key only: {ph:?}");
    assert_eq!(ph.defs, set(&[("a.php", 0)]), "php goto-def lands on Alpha's key: {ph:?}");
    assert_eq!(pl.defs, set(&[("a.pl", 0)]));
    assert_eq!(ph.rename, Some(ph.refs.clone()));
}

/// A lexical hash's key is one renameable unit — the literal, every read and
/// every write — and another hash's same-named key is untouched
/// (`lexical_hash_key_renames_literal_and_accesses_in_scope`).
#[test]
fn literal_held_keys_rename_as_one_unit_in_both_languages() {
    let perl = "use strict;\n\
                my %opts = (timeout => 30, retries => 3);\n\
                my %other = (timeout => 9);\n\
                my $t = $opts{timeout};\n\
                $opts{timeout} = 60;\n\
                print $other{timeout};\n";
    let php = "<?php\n\
               $opts = ['timeout' => 30, 'retries' => 3];\n\
               $other = ['timeout' => 9];\n\
               $t = $opts['timeout'];\n\
               $opts['timeout'] = 60;\n\
               echo $other['timeout'];\n";
    let pl = answers(Lang::Perl, &[("a", perl)], 3, "timeout", 1);
    let ph = answers(Lang::Php, &[("a", php)], 3, "timeout", 1);
    let want = |f: &str| set(&[(f, 1), (f, 3), (f, 4)]);
    assert_eq!(pl.rename, Some(want("a.pl")), "perl: {pl:?}");
    assert_eq!(pl.refs, want("a.pl"), "perl: {pl:?}");
    assert_eq!(ph.rename, Some(want("a.php")), "php: {ph:?}");
    assert_eq!(ph.refs, want("a.php"), "php: {ph:?}");
    // From the write: the same unit.
    let ph_w = answers(Lang::Php, &[("a", php)], 4, "timeout", 1);
    assert_eq!(ph_w.refs, want("a.php"), "php from the write: {ph_w:?}");
    // The other hash keeps its own key.
    let ph_o = answers(Lang::Php, &[("a", php)], 5, "timeout", 1);
    assert_eq!(ph_o.refs, set(&[("a.php", 2), ("a.php", 5)]), "php %other: {ph_o:?}");
}

/// A key added by a write is found from its read, with no def anywhere.
#[test]
fn written_keys_are_found_from_their_reads_in_both_languages() {
    let perl = "use strict;\nmy %d = (alpha => 1);\n$d{extra} = 3;\nprint $d{extra};\n";
    let php = "<?php\n$d = ['alpha' => 1];\n$d['extra'] = 3;\necho $d['extra'];\n";
    let pl = answers(Lang::Perl, &[("a", perl)], 3, "extra", 1);
    let ph = answers(Lang::Php, &[("a", php)], 3, "extra", 1);
    assert_eq!(pl.refs, set(&[("a.pl", 2), ("a.pl", 3)]), "perl: {pl:?}");
    assert_eq!(ph.refs, set(&[("a.php", 2), ("a.php", 3)]), "php: {ph:?}");
    assert!(pl.defs.is_empty() && ph.defs.is_empty(), "no def for a written key");
    let fa = Lang::Php.analyze(php);
    let write = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::HashKeyAccess { .. }) && r.span.start.row == 2)
        .expect("the write mints a key ref");
    assert_eq!(write.access, AccessKind::Write);
}

/// Dynamic keys behave as Perl's do: `extract_key_text` marks a variable or
/// interpolated key dynamic and no key ref is minted, so the key's variable
/// stays a plain variable and nothing const-folds `$k` back to `'host'`. A
/// double-quoted key with nothing to interpolate is static in both.
#[test]
fn dynamic_keys_mint_no_key_ref_in_either_language() {
    let perl = "use strict;\n\
                sub cfg { return { host => 1 }; }\n\
                my $c = cfg();\n\
                my $k = 'host';\n\
                print $c->{$k};\n\
                print $c->{\"$k\"};\n\
                print $c->{\"host\"};\n";
    let php = "<?php\n\
               function cfg() { return ['host' => 1]; }\n\
               $c = cfg();\n\
               $k = 'host';\n\
               echo $c[$k];\n\
               echo $c[\"$k\"];\n\
               echo $c[\"host\"];\n";
    for (lang, src) in [(Lang::Perl, perl), (Lang::Php, php)] {
        let fa = lang.analyze(src);
        assert_eq!(key_refs_on_row(&fa, 4), 0, "{lang:?}: a variable key mints no key ref");
        assert_eq!(key_refs_on_row(&fa, 5), 0, "{lang:?}: an interpolated key mints no key ref");
        assert_eq!(key_refs_on_row(&fa, 6), 1, "{lang:?}: a plain double-quoted key is static");
    }
    let f = |lang: Lang| format!("a.{}", lang.ext());
    for (lang, src) in [(Lang::Perl, perl), (Lang::Php, php)] {
        // The key's references skip the dynamic sites and the folded string.
        let from_def = answers(lang, &[("a", src)], 1, "host", 1);
        assert_eq!(from_def.refs, set(&[(&f(lang), 1), (&f(lang), 6)]), "{lang:?}: {from_def:?}");
        assert_eq!(from_def.rename, Some(from_def.refs.clone()), "{lang:?}: rename misses the fold");
        // `$k` in the subscript is the variable, nothing more.
        let on_var = answers(lang, &[("a", src)], 4, "k", 1);
        assert_eq!(on_var.refs, set(&[(&f(lang), 3), (&f(lang), 4), (&f(lang), 5)]), "{lang:?}: {on_var:?}");
        assert_eq!(on_var.defs, set(&[(&f(lang), 3)]), "{lang:?}: {on_var:?}");
    }
}

/// Cross-file: the producer's key and the consumer's access are one target
/// (`refs_to_links_return_hash_key_cross_file`'s PHP twin — `use function`
/// plays Exporter's part).
#[test]
fn imported_function_keys_reach_across_files_in_php() {
    let producer = "<?php\nnamespace Cfg;\nfunction get_config() { return ['host' => 'h', 'port' => 1]; }\n";
    let consumer = "<?php\nuse function Cfg\\get_config;\n$c = get_config();\n$h = $c['host'];\n";
    let from_consumer = answers(Lang::Php, &[("consumer", consumer), ("cfg", producer)], 3, "host", 1);
    // Goto-def across files asks the module index for the producer's
    // analysis (as Perl's does); a bare store answers references and rename.
    assert_eq!(from_consumer.refs, set(&[("cfg.php", 2), ("consumer.php", 3)]), "{from_consumer:?}");
    assert_eq!(from_consumer.rename, Some(from_consumer.refs.clone()));

    let store = FileStore::new();
    let pa = PathBuf::from("/tmp/keyed/cfg.php");
    let pb = PathBuf::from("/tmp/keyed/consumer.php");
    let fa_a = Lang::Php.analyze(producer);
    store.insert_workspace(pb, Lang::Php.analyze(consumer));
    let target = TargetRef::new(
        "host".into(),
        TargetKind::HashKeyOfSub { package: Some("Cfg".into()), name: "get_config".into() },
        &fa_a,
    );
    store.insert_workspace(pa, fa_a);
    let files: BTreeSet<String> = resolve::refs_to(&store, None, &target, RoleMask::EDITABLE)
        .iter()
        .map(|l| file_of(&l.key))
        .collect();
    assert_eq!(files, ["cfg.php", "consumer.php"].into_iter().map(String::from).collect());
}

/// Where the PHP pack departs from today's Perl, on purpose and still open
/// (keyed-access notes): a copy keeps the key's identity, a nested literal's
/// keys belong to the outer holder, and a literal held in a variable mints
/// defs, so goto-def answers where Perl's literal keys are refs only.
#[test]
fn php_departures_from_perl_are_pinned() {
    let perl = "use strict;\n\
                sub cfg { return { host => 1, db => { name => 'x' } }; }\n\
                my $c = cfg();\n\
                my $e = $c;\n\
                print $e->{host}, $c->{db}{name};\n\
                my $d = { alpha => 1 };\n\
                print $d->{alpha};\n";
    let php = "<?php\n\
               function cfg() { return ['host' => 1, 'db' => ['name' => 'x']]; }\n\
               $c = cfg();\n\
               $e = $c;\n\
               echo $e['host'], $c['db']['name'];\n\
               $d = ['alpha' => 1];\n\
               echo $d['alpha'];\n";
    // A copy: perl's `$e` owns its own keys; php follows `$e = $c` back to cfg.
    assert_eq!(answers(Lang::Perl, &[("a", perl)], 4, "host", 1).refs, set(&[("a.pl", 4)]));
    assert_eq!(answers(Lang::Php, &[("a", php)], 4, "host", 1).refs, set(&[("a.php", 1), ("a.php", 4)]));
    // A nested key: perl mints no ref for the inner subscript.
    assert!(answers(Lang::Perl, &[("a", perl)], 4, "name", 1).refs.is_empty());
    assert_eq!(answers(Lang::Php, &[("a", php)], 4, "name", 1).refs, set(&[("a.php", 1), ("a.php", 4)]));
    // A literal held in a variable: same references, but php also has a def.
    let pl = answers(Lang::Perl, &[("a", perl)], 6, "alpha", 1);
    let ph = answers(Lang::Php, &[("a", php)], 6, "alpha", 1);
    assert_eq!(pl.refs, set(&[("a.pl", 5), ("a.pl", 6)]));
    assert_eq!(ph.refs, set(&[("a.php", 5), ("a.php", 6)]));
    assert!(pl.defs.is_empty(), "perl: {pl:?}");
    assert_eq!(ph.defs, set(&[("a.php", 5)]), "php: {ph:?}");
}

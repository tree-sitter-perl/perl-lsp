use super::*;

fn member(name: &str, reach: RootReach, constructor: bool) -> RootMember {
    RootMember { name: name.into(), constructor, reach, params: vec![], doc: None }
}

#[test]
fn reach_gates_name_and_role_asks() {
    let ctor = member("__construct", RootReach::Role, true);
    assert!(ctor.answers(MemberKey::Constructor));
    assert!(!ctor.answers(MemberKey::Named("__construct")), "a role-only member answers no named ask");

    let init = member("__init__", RootReach::Both, true);
    assert!(init.answers(MemberKey::Constructor));
    assert!(init.answers(MemberKey::Named("__init__")));

    let can = member("can", RootReach::Name, false);
    assert!(can.answers(MemberKey::Named("can")));
    assert!(!can.answers(MemberKey::Constructor));
}

#[test]
fn documents_merge_by_language_in_load_order() {
    let bundled = RootDocument {
        language: "perl".into(),
        class: Some("UNIVERSAL".into()),
        members: vec![member("can", RootReach::Name, false)],
    };
    let extra = RootDocument {
        language: "perl".into(),
        class: Some("Ignored".into()),
        members: vec![
            member("can", RootReach::Role, false),
            member("house_method", RootReach::Name, false),
        ],
    };
    let other = RootDocument { language: "php".into(), class: None, members: vec![] };
    let roots = LanguageRoots::from_documents([&bundled, &extra, &other]);
    let perl = roots.surface("perl").expect("perl surface");
    assert_eq!(perl.class.as_deref(), Some("UNIVERSAL"), "the first class stands");
    assert_eq!(perl.members.len(), 2);
    assert_eq!(perl.member(MemberKey::Named("can")).unwrap().reach, RootReach::Name, "the earlier declaration stands");
    assert!(perl.member(MemberKey::Named("house_method")).is_some(), "a later document adds members");
    assert!(roots.is_root_class("perl", "UNIVERSAL"));
    assert!(!roots.is_root_class("php", "UNIVERSAL"));
    assert!(roots.surface("cpp").is_none());
}

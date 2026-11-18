//! The pure rules of names: wildcard matching, `^` taken back, roots, and
//! special fields.

use super::{first_part, is_root, matches, normalised, rooting, Dir, Directories, Selection};

#[test]
fn a_star_matches_any_run_and_a_hash_any_one_character() {
    assert!(matches(b"*", b""));
    assert!(matches(b"*", b"anything"));
    assert!(matches(b"a*", b"alpha"));
    assert!(!matches(b"a*", b"beta"));
    assert!(matches(b"*a", b"alpha"));
    assert!(matches(b"a*a", b"alpha"));
    assert!(!matches(b"a*a", b"alphab"));
    assert!(matches(b"#lph#", b"alpha"));
    assert!(!matches(b"#lph#", b"alph"));
    assert!(matches(b"b*f", b"betaf"));
    assert!(matches(b"*.*", b"a.b"));
    assert!(!matches(b"", b"a"));
    assert!(matches(b"", b""));
}

#[test]
fn matching_ignores_case() {
    assert!(matches(b"B*F", b"betaf"));
    assert!(matches(b"alpha", b"ALPHA"));
}

#[test]
fn a_star_backs_off_as_far_as_it_must() {
    assert!(matches(b"*ab", b"aaab"));
    assert!(matches(b"*a*b", b"xaxxb"));
    assert!(!matches(b"*a*b", b"xxxb"));
}

#[test]
fn a_caret_takes_back_the_part_before_it_but_never_the_root() {
    assert_eq!(normalised(b"$.a.^.b".to_vec()), b"$.b");
    assert_eq!(normalised(b"$.^".to_vec()), b"$");
    assert_eq!(normalised(b":disc.$.^.x".to_vec()), b":disc.$.x");
    assert_eq!(normalised(b"$.a.b.^.^.c".to_vec()), b"$.c");
    assert_eq!(normalised(b"$.plain".to_vec()), b"$.plain");
}

#[test]
fn a_root_is_a_bare_dollar_or_a_named_discs() {
    assert!(is_root(b"$"));
    assert!(is_root(b":TxDisc.$"));
    assert!(!is_root(b":TxDisc.$.a"));
    assert!(!is_root(b"$.a"));
    assert!(!is_root(b":TxDisc"));
}

#[test]
fn a_path_splits_at_its_first_full_stop() {
    assert_eq!(first_part(b"$.a.b"), (&b"$"[..], &b"a.b"[..]));
    assert_eq!(first_part(b"plain"), (&b"plain"[..], &b""[..]));
}

#[test]
fn an_empty_special_field_is_none_only_to_a_filing_system_with_special_fields() {
    assert_eq!(rooting(true, Some(b"")), None);
    assert_eq!(rooting(true, Some(b"spec")), Some(&b"spec"[..]));
    assert_eq!(rooting(true, None), None);
    assert_eq!(rooting(false, Some(b"")), Some(&b""[..]));
    assert_eq!(rooting(false, Some(b"spec")), Some(&b"spec"[..]));
    assert_eq!(rooting(false, None), None);
}

#[test]
fn each_directory_is_set_and_unset_on_its_own() {
    let mut directories = Directories::default();
    let selection = Selection {
        path: b":Disc.$.dir".to_vec(),
        special: None,
    };
    directories.put(Dir::Library, Some(selection.clone()));
    assert_eq!(directories.get(Dir::Library), Some(&selection));
    assert_eq!(directories.get(Dir::Current), None);
    directories.put(Dir::Library, None);
    assert_eq!(directories.get(Dir::Library), None);
}

#[test]
fn entering_a_directory_makes_the_one_it_replaces_previous_set_or_not() {
    let at = |path: &[u8]| Selection {
        path: path.to_vec(),
        special: None,
    };
    let mut directories = Directories::default();
    directories.put(Dir::Previous, Some(at(b"$.old")));
    directories.enter(at(b"$.one"));
    assert_eq!(directories.get(Dir::Previous), None, "nothing was current");
    directories.enter(at(b"$.two"));
    assert_eq!(directories.get(Dir::Previous), Some(&at(b"$.one")));
    directories.exchange(Dir::Current, Dir::Previous);
    assert_eq!(directories.get(Dir::Current), Some(&at(b"$.one")));
    assert_eq!(directories.get(Dir::Previous), Some(&at(b"$.two")));
    directories.put(Dir::Current, None);
    assert_eq!(
        directories.get(Dir::Previous),
        Some(&at(b"$.two")),
        "unsetting leaves the previous"
    );
}

#[test]
fn a_disc_root_is_its_disc_and_dollar_or_a_bare_dollar() {
    assert_eq!(super::disc_root_of(b":TxDisc.$.subd.onef"), b":TxDisc.$");
    assert_eq!(super::disc_root_of(b":TxDisc.$"), b":TxDisc.$");
    assert_eq!(super::disc_root_of(b"$.subd"), b"$");
    assert_eq!(super::disc_root_of(b":Disc"), b":Disc");
}

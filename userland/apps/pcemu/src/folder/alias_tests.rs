use super::{alias, exact, host_name, label, TRIES};
use crate::folder::tests::name;

#[test]
fn a_name_dos_could_hold_is_its_own_in_upper_case() {
    assert_eq!(exact("README.TXT"), Some(name("README.TXT")));
    assert_eq!(exact("readme.txt"), Some(name("README.TXT")));
    assert_eq!(exact("Makefile"), Some(name("MAKEFILE")));
    assert_eq!(exact("A.B"), Some(name("A.B")));
    assert_eq!(exact("~$TEMP.{1}"), Some(name("~$TEMP.{1}")));
    // é is code page 437's 82h, whose capital is 90h.
    let accented = exact("café.txt").expect("code page 437 holds it");
    assert_eq!(&accented.bytes()[..5], b"CAF\x90 ");
    for long in [
        "LONGFILENAME.TXT",
        "file.html",
        "a.b.c",
        ".profile",
        "two words.txt",
        "plus+sign",
        "日本.txt",
        "",
        "x.",
    ] {
        assert_eq!(exact(long), None, "{long:?} has no 8.3 name of its own");
    }
}

#[test]
fn an_alias_keeps_what_it_can_of_the_name_and_numbers_the_rest() {
    assert_eq!(alias("LONGFILENAME.TXT", 1), Some(name("LONGFI~1.TXT")));
    assert_eq!(alias("Long File Name.html", 2), Some(name("LONGFI~2.HTM")));
    assert_eq!(alias("archive.tar.gz", 1), Some(name("ARCHIV~1.GZ")));
    assert_eq!(alias(".profile", 1), Some(name("PROFIL~1")));
    assert_eq!(alias("a+b=c.txt", 1), Some(name("A_B_C~1.TXT")));
    assert_eq!(alias("日本語のファイル.txt", 1), Some(name("______~1.TXT")));
    assert_eq!(alias("...", 1), Some(name("_~1")), "a name of dots alone");
    assert_eq!(alias("LONGFILENAME.TXT", 10), Some(name("LONGF~10.TXT")));
    assert_eq!(alias("LONGFILENAME.TXT", TRIES), Some(name("L~999999.TXT")));
    assert_eq!(alias("LONGFILENAME.TXT", 0), None);
    assert_eq!(alias("LONGFILENAME.TXT", TRIES + 1), None);
}

#[test]
fn a_name_dos_makes_is_spelled_for_the_host_in_its_own_characters() {
    assert_eq!(host_name(&name("README.TXT")), "README.TXT");
    assert_eq!(host_name(&name("MAKEFILE")), "MAKEFILE");
    let capital = exact("ÉTÉ.TXT").expect("code page 437 holds it");
    assert_eq!(host_name(&capital), "ÉTÉ.TXT");
    // A name beginning with E5h is kept as 05h and spelled as E5h's σ.
    let sigma = exact("σ.TXT").expect("code page 437 holds it");
    assert_eq!(sigma.bytes()[0], 0x05);
    assert_eq!(host_name(&sigma), "σ.TXT");
}

#[test]
fn a_share_is_labelled_by_what_dos_could_hold_of_its_folders_name() {
    assert_eq!(label("Projects"), name("PROJECTS"));
    assert_eq!(label("My Documents"), name("MYDOCUME"));
    assert_eq!(label("v1.2"), name("V12"));
    assert_eq!(label("日本"), name("SHARED"), "nothing DOS could hold");
    assert_eq!(label(""), name("SHARED"));
}

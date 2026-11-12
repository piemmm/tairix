use super::*;

fn canon(path: &[u8], current: &[u8]) -> Result<alloc::vec::Vec<u8>, NameError> {
    let (drive, rest) = drive_of(path, 2);
    canonical(drive, rest, current, 5).map(|c| c.as_bytes().to_vec())
}

#[test]
fn a_component_takes_the_entry_form_dos_gives_it() {
    assert_eq!(field(b"readme.txt", false), Ok(*b"README  TXT"));
    assert_eq!(
        field(b"longfilename.text", false),
        Ok(*b"LONGFILETEX"),
        "each part cut to its length"
    );
    assert_eq!(field(b"*.c*", true), Ok(*b"????????C??"));
    assert_eq!(field(b"a?c", true), Ok(*b"A?C        "));
    assert_eq!(
        field(b"*.*", false),
        Err(NameError::File),
        "no wildcard in a name"
    );
    assert_eq!(field(b"a+b", false), Err(NameError::File));
    assert_eq!(field(b".txt", false), Err(NameError::File), "no base");
    assert_eq!(field(b"a.b.c", false), Err(NameError::File));
    assert_eq!(
        field(b"\xE5x", false),
        Ok(*b"\x05X         "),
        "a leading E5h kept from reading as deleted"
    );
    assert_eq!(
        field(b"caf\x82", false),
        Ok(*b"CAF\x90       "),
        "the country's upper case"
    );
    assert_eq!(field(b"..", false), Ok(*b"..         "));
}

#[test]
fn patterns_match_by_byte_with_question_marks_free() {
    let pattern = field(b"*.TXT", true).unwrap();
    assert!(matches(&pattern, b"README  TXT"));
    assert!(!matches(&pattern, b"README  DOC"));
    assert!(matches(&field(b"*.*", true).unwrap(), b"ANY     EXT"));
    assert!(is_wild(&pattern));
}

#[test]
fn paths_are_made_canonical_against_the_current_directory() {
    assert_eq!(
        canon(b"file.txt", b"C:\\DOS"),
        Ok(b"C:\\DOS\\FILE.TXT".to_vec())
    );
    assert_eq!(
        canon(b"\\file.txt", b"C:\\DOS"),
        Ok(b"C:\\FILE.TXT".to_vec())
    );
    assert_eq!(
        canon(b"..\\a\\.\\b", b"C:\\X\\Y"),
        Ok(b"C:\\X\\A\\B".to_vec())
    );
    assert_eq!(
        canon(b"a:/sub/f", b"C:\\"),
        Ok(b"A:\\SUB\\F".to_vec()),
        "a drive given and slashes"
    );
    assert_eq!(canon(b"", b"C:\\DOS"), Ok(b"C:\\DOS".to_vec()));
    assert_eq!(
        canon(b"..", b"C:\\"),
        Err(NameError::Path),
        "above the root"
    );
    assert_eq!(canon(b"z:x", b"C:\\"), Err(NameError::Drive));
    assert_eq!(
        canon(b"*.com", b"C:\\"),
        Ok(b"C:\\????????.COM".to_vec()),
        "a wildcard kept last"
    );
    assert_eq!(canon(b"*\\x", b"C:\\"), Err(NameError::Path));
    assert_eq!(
        canon(&b"longname\\".repeat(8), b"C:\\").map(|_| ()),
        Err(NameError::Path),
        "past 64 bytes"
    );
    assert_eq!(
        canon(&[b'a'; 70], b"C:\\").map(|p| p.len()),
        Ok(11),
        "one long component is cut"
    );
}

#[test]
fn a_canonical_path_names_its_drive_and_components() {
    let path = canonical(2, b"\\dos\\cmd.com", b"C:\\", 5).unwrap();
    assert_eq!(path.drive(), 2);
    let parts: alloc::vec::Vec<&[u8]> = path.components().collect();
    assert_eq!(parts, [&b"DOS"[..], b"CMD.COM"]);
    assert_eq!(path.last(), Some(&b"CMD.COM"[..]));
    let mut out = [0u8; 13];
    let len = spell(b"CMD     COM", &mut out);
    assert_eq!(&out[..len], b"CMD.COM");
}

use super::*;

fn shown(field: &[u8; 11]) -> ([u8; 12], usize) {
    let mut out = [0u8; 12];
    let len = display(field, &mut out);
    (out, len)
}

#[test]
fn a_field_displays_as_base_dot_extension() {
    let (out, len) = shown(b"README  TXT");
    assert_eq!(&out[..len], b"README.TXT");
    let (out, len) = shown(b"MAKEFILE   ");
    assert_eq!(&out[..len], b"MAKEFILE");
    let (out, len) = shown(b"\x05BC     DAT");
    assert_eq!(&out[..len], b"\xE5BC.DAT", "the lead stood in for");
}

#[test]
fn an_83_name_has_a_field_and_any_other_none() {
    assert_eq!(field(b"readme.txt"), Some(*b"README  TXT"));
    assert_eq!(field(b"COMMAND.COM"), Some(*b"COMMAND COM"));
    assert_eq!(field(b"NOEXT"), Some(*b"NOEXT      "));
    assert_eq!(field(b"."), Some(*b".          "));
    assert_eq!(field(b".."), Some(*b"..         "));
    assert_eq!(field(b"\xE5X.Y"), Some(*b"\x05X      Y  "));
    assert_eq!(
        field(b"\x81\x82.TXT"),
        Some(*b"\x81\x82      TXT"),
        "code-page bytes"
    );
    for refused in [
        &b"a.b.c"[..],
        b"toolongname.txt",
        b"name.text",
        b"",
        b".txt",
        b"a+b",
        b"a b",
    ] {
        assert_eq!(field(refused), None, "{refused:?}");
    }
}

#[test]
fn an_alias_is_the_stem_cut_short_for_its_tail() {
    let stem = AliasStem::of(b"Greeting letter.txt");
    assert_eq!(stem.alias(1), Some(*b"GREETI~1TXT"));
    assert_eq!(stem.alias(42), Some(*b"GREET~42TXT"));
    assert_eq!(stem.alias(999_999), Some(*b"G~999999TXT"));
    assert_eq!(stem.alias(1_000_000), None);
    assert_eq!(stem.tail_of(b"GREETI~1TXT"), Some(1));
    assert_eq!(stem.tail_of(b"GREETI~1TXU"), None);
    assert_eq!(
        AliasStem::of(b".bashrc").alias(1),
        Some(*b"BASHRC~1   "),
        "a leading dot is no extension"
    );
    assert_eq!(
        AliasStem::of(b"caf\xC3\xA9 au lait").alias(2),
        Some(*b"CAF__A~2   "),
        "no code page spelt"
    );
    assert_eq!(AliasStem::of(b"...").alias(1), Some(*b"_~1        "));
    assert_eq!(split(b"a.tar.gz"), (&b"a.tar"[..], &b"gz"[..]));
}

#[test]
fn the_window_finds_the_lowest_tail_left_free() {
    let mut window = TailWindow::starting_at(1);
    for tail in [1, 2, 4, 5000] {
        window.hold(tail);
    }
    assert_eq!(window.lowest_free(), Some(3));
    let mut full = TailWindow::starting_at(1);
    for tail in 1..=TAIL_WINDOW {
        full.hold(tail);
    }
    assert_eq!(full.lowest_free(), None);
}

#[test]
fn a_label_spells_what_a_short_name_may_and_never_the_placeholder() {
    assert_eq!(label(b"WORK 2026"), Ok(Some(*b"WORK 2026  ")));
    assert_eq!(label(b""), Ok(None));
    for refused in [&b"work"[..], b" WORK", b"WORK ", b"NO NAME", b"CAF\xC9"] {
        assert_eq!(label(refused), Err(LabelError::Spelling), "{refused:?}");
    }
    assert_eq!(label(b"TWELVE CHARS"), Err(LabelError::Length));
    assert_eq!(trimmed(b"NO NAME    "), b"NO NAME");
}

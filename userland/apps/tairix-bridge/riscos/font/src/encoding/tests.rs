use super::*;

fn names(file: &EncodingFile) -> Vec<(u32, &[u8])> {
    file.names().collect()
}

#[test]
fn a_names_file_gives_each_name_the_next_code() {
    let file = EncodingFile::read(
        b"% Latin 1\n%%RISCOS_BasedOn 0\n/.notdef /space\n/A%comment /Z\n  /B/C\n".to_vec(),
    );
    assert_eq!(
        names(&file),
        [(1, &b"space"[..]), (2, b"A"), (3, b"B"), (4, b"C")],
        "a comment runs to the line's end; .notdef names no glyph"
    );
}

#[test]
fn a_glyph_list_file_gives_each_line_its_own_code() {
    let file = EncodingFile::read(
        b"0005;fred;SYMBOL FOR FRED\n0002;Jim;SYMBOL FOR JIM\n0007;Harry;X\n# Comment\n22;000C;Tom;SYMBOL FOR TOM\n10FFFF ; Big ;\nZZZZ;bad;x\n0001;;empty\n".to_vec(),
    );
    assert_eq!(
        names(&file),
        [
            (5, &b"fred"[..]),
            (2, b"Jim"),
            (7, b"Harry"),
            (0xC, b"Tom"),
            (0x10_FFFF, b"Big")
        ]
    );
}

#[test]
fn a_based_encoding_maps_each_code_to_the_glyph_its_name_is_in_the_base() {
    let base = EncodingFile::read(b"/.notdef /.NotDef /space /A /B /.NotDef".to_vec());
    let target = EncodingFile::read(b"/.notdef /B /nonesuch /A /space".to_vec());
    let map = CodeMap::based(&target, &base);
    assert_eq!(map.glyph(0), None, ".notdef is no glyph");
    assert_eq!(map.glyph(1), Some(4));
    assert_eq!(map.glyph(2), None);
    assert_eq!(map.glyph(3), Some(3));
    assert_eq!(map.glyph(4), Some(2));
    assert_eq!(map.glyph(200), None, "a code past the names");
    assert_eq!(map.len(), 5);
    assert_eq!(
        (map.next(0), map.next(1), map.next(2), map.next(4)),
        (Some(1), Some(3), Some(3), None)
    );
}

#[test]
fn a_sparse_map_reaches_past_the_table() {
    let base = EncodingFile::read(b"/.notdef /A /B".to_vec());
    let target = EncodingFile::read(b"0041;A;x\n12345;B;y\n0042;B;z\n".to_vec());
    let map = CodeMap::based(&target, &base);
    assert_eq!(
        (map.glyph(0x41), map.glyph(0x12345), map.glyph(0x42)),
        (Some(1), Some(2), Some(2))
    );
    assert_eq!(map.len(), 0x12346);
    assert_eq!(map.next(0x42), Some(0x12345));
    assert_eq!(map.below(0x100).collect::<Vec<_>>(), [(0x41, 1), (0x42, 2)]);
}

#[test]
fn the_first_glyph_a_code_reaches_is_its_glyph() {
    let base = EncodingFile::read(b"/.notdef /A /B".to_vec());
    let target = EncodingFile::read(b"0041;nonesuch;x\n0041;B;y\n0041;A;z\n".to_vec());
    assert_eq!(CodeMap::based(&target, &base).glyph(0x41), Some(2));
}

#[test]
fn utf8_maps_a_glyph_at_its_characters_then_risc_os_s_doubles() {
    let names = GlyphNames::read();
    let base = EncodingFile::read(b"/.notdef /space /mu /uni03BC /a1".to_vec());
    let map = CodeMap::utf8(&base, &names);
    assert_eq!(map.glyph(0x20), Some(1));
    assert_eq!(map.glyph(0xA0), Some(1), "RISC OS's double of space");
    assert_eq!(map.glyph(0xB5), Some(2));
    assert_eq!(map.glyph(0x3BC), Some(3), "a uni name outranks mu's double");
    assert_eq!(map.glyph(0x2701), Some(4), "a dingbats name");
    let without = EncodingFile::read(b"/.notdef /mu".to_vec());
    assert_eq!(
        CodeMap::utf8(&without, &names).glyph(0x3BC),
        Some(1),
        "mu's double"
    );
}

#[test]
fn the_rom_latin1_file_maps_as_risc_os_5_30_s_did() {
    let base =
        EncodingFile::read(include_bytes!("../../resources/ROMFonts/Encodings/Base0").to_vec());
    let latin1 =
        EncodingFile::read(include_bytes!("../../resources/ROMFonts/Encodings/Latin1").to_vec());
    let map = CodeMap::based(&latin1, &base);
    let observed = &crate::rom_encodings::ROM_ENCODINGS[0].glyphs;
    for code in 0..=255u32 {
        let expected = Some(u32::from(observed[code as usize]))
            .filter(|&g| g != u32::from(crate::rom_encodings::NONE));
        assert_eq!(map.glyph(code), expected, "code {code}");
    }
}

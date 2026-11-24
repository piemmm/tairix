use super::*;

#[test]
fn the_glyph_lists_are_adobe_s_byte_for_byte() {
    assert_eq!(
        (LIST.len(), tairix_crc32::checksum(LIST.as_bytes())),
        (78_060, 0x3419_94E4)
    );
    assert_eq!(
        (DINGBATS.len(), tairix_crc32::checksum(DINGBATS.as_bytes())),
        (3_879, 0x478F_6C14)
    );
    assert_eq!(
        (
            NEW_FONTS.len(),
            tairix_crc32::checksum(NEW_FONTS.as_bytes())
        ),
        (27_655, 0x355A_A8DA)
    );
}

#[test]
fn a_name_stands_for_its_characters_matched_exactly() {
    let names = GlyphNames::read();
    assert_eq!(names.characters(b"space"), [0x20]);
    assert_eq!(names.characters(b"mu"), [0xB5]);
    assert_eq!(names.characters(b"a1"), [0x2701], "a Zapf Dingbats name");
    assert_eq!(names.characters(b"uni00E9"), [0xE9]);
    assert_eq!(
        names.characters(b"uniD83DDE00"),
        [0x1F600],
        "a surrogate pair"
    );
    for refused in [
        &b"Scedilla.alt"[..],
        b"uni00e9",
        b"uniD800",
        b"uniDC00D800",
        b"uni12",
        b"u1F600",
        b"nonesuch",
    ] {
        assert!(names.characters(refused).is_empty(), "{refused:?}");
    }
}

#[test]
fn a_character_goes_by_its_recommended_name() {
    let names = GlyphNames::read();
    assert_eq!(
        names.name(0x02DC),
        "tilde",
        "the recommended name, not the list's first"
    );
    assert_eq!(names.name(0x00B5), "mu");
    assert_eq!(names.name(0xE000), "uniE000");
    assert_eq!(names.name(0x1F600), "uniD83DDE00");
    assert_eq!(names.name(0x2701), "uni2701", "dingbats name no character");
}

use super::*;

/// The committed files, by leaf.
fn committed(leaf: &str) -> &'static [u8] {
    match leaf {
        "/Base0" => include_bytes!("../../resources/ROMFonts/Encodings/Base0"),
        "/Default" => include_bytes!("../../resources/ROMFonts/Encodings/Default"),
        "Latin1" => include_bytes!("../../resources/ROMFonts/Encodings/Latin1"),
        "Latin2" => include_bytes!("../../resources/ROMFonts/Encodings/Latin2"),
        "Latin3" => include_bytes!("../../resources/ROMFonts/Encodings/Latin3"),
        "Latin4" => include_bytes!("../../resources/ROMFonts/Encodings/Latin4"),
        "Latin5" => include_bytes!("../../resources/ROMFonts/Encodings/Latin5"),
        "Latin6" => include_bytes!("../../resources/ROMFonts/Encodings/Latin6"),
        "Latin7" => include_bytes!("../../resources/ROMFonts/Encodings/Latin7"),
        "Latin8" => include_bytes!("../../resources/ROMFonts/Encodings/Latin8"),
        "Latin9" => include_bytes!("../../resources/ROMFonts/Encodings/Latin9"),
        "Latin10" => include_bytes!("../../resources/ROMFonts/Encodings/Latin10"),
        "Greek" => include_bytes!("../../resources/ROMFonts/Encodings/Greek"),
        "Cyrillic" => include_bytes!("../../resources/ROMFonts/Encodings/Cyrillic"),
        "Hebrew" => include_bytes!("../../resources/ROMFonts/Encodings/Hebrew"),
        "Welsh" => include_bytes!("../../resources/ROMFonts/Encodings/Welsh"),
        "UTF8" => include_bytes!("../../resources/ROMFonts/Encodings/UTF8"),
        _ => b"",
    }
}

#[test]
fn the_committed_encoding_files_are_the_tables_made_afresh() {
    let names = GlyphNames::read();
    for (leaf, bytes) in files(&names) {
        assert!(
            committed(&leaf) == bytes.as_slice(),
            "resources/ROMFonts/Encodings/{} is stale: run cargo xtask riscos --write-encodings",
            leaf.trim_start_matches('/')
        );
    }
}

#[test]
fn every_base_glyph_has_a_name_of_its_own() {
    let names = GlyphNames::read();
    let mut seen: Vec<String> = Vec::new();
    for glyph in 0..BASE0.len() {
        let name = base_name(&names, glyph);
        if name != BASE_NOTDEF {
            assert!(!seen.contains(&name), "glyph {glyph} repeats {name}");
            seen.push(name);
        }
    }
    assert_eq!(base_name(&names, 0), NOTDEF);
    assert_eq!(base_name(&names, 32), "space");
}

#[test]
fn an_encoding_names_each_code_s_glyph_and_its_alphabet_s_character_past_them() {
    let names = GlyphNames::read();
    let latin1 = String::from_utf8(encoding(&names, &ROM_ENCODINGS[0])).expect("text");
    assert!(latin1.contains("%%RISCOS_BasedOn 0\n%%RISCOS_Alphabet 101\n"));
    let first: Vec<&str> = latin1
        .lines()
        .nth(3)
        .expect("the first names")
        .split(' ')
        .collect();
    assert_eq!(first[..4], ["/ring", "/circumflex", "/tilde", "/dotlessi"]);
    let cyrillic = ROM_ENCODINGS
        .iter()
        .find(|rom| rom.name == "Cyrillic")
        .expect("Cyrillic");
    let text = String::from_utf8(encoding(&names, cyrillic)).expect("text");
    let words: Vec<&str> = text
        .lines()
        .skip(3)
        .flat_map(|line| line.split(' '))
        .collect();
    assert_eq!(words.len(), 256);
    assert_eq!(
        words[0xD0],
        format!("/{}", names.name(0x430)),
        "an alphabet's character"
    );
}

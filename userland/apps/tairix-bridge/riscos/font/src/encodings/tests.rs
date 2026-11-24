use super::*;
use crate::rom::ENCODINGS;

#[test]
fn the_committed_encoding_files_are_the_tables_made_afresh() {
    let names = GlyphNames::read();
    let made = files(&names);
    assert_eq!(made.len(), ENCODINGS.len());
    for &(name, committed) in &ENCODINGS {
        let leaf = name.rsplit_once('.').map_or(name, |(_, leaf)| leaf);
        let fresh = made.iter().find(|(made_leaf, _)| made_leaf == leaf);
        assert!(
            fresh.is_some_and(|(_, bytes)| bytes.as_slice() == committed),
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
    let latin1 = String::from_utf8(encoding(&names, "Latin1", &ROM_ENCODINGS[0])).expect("text");
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
        .find(|rom| alphabet_name(rom.alphabet) == Some("Cyrillic"))
        .expect("Cyrillic");
    let text = String::from_utf8(encoding(&names, "Cyrillic", cyrillic)).expect("text");
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

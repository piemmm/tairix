use super::*;

/// The glyph RISC OS's arrangement gives `code`.
fn risc_os(code: u8) -> Glyph {
    *risc_os_glyph(code).expect("a drawn character")
}

#[test]
fn a_glyph_s_rows_are_its_drawing_s() {
    assert_eq!(
        risc_os(b'A'),
        [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00]
    );
    assert_eq!(risc_os(127), [0xFF; 8], "RISC OS's solid block");
    assert_eq!(
        *bbc_glyph(127).expect("drawn"),
        [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55]
    );
}

#[test]
fn the_bbc_s_arrangement_is_risc_os_s_but_for_96_and_127() {
    for code in FIRST..=127 {
        let bbc = *bbc_glyph(code).expect("drawn");
        match code {
            96 => assert_eq!(bbc, risc_os(163), "the pound sign"),
            127 => assert_ne!(bbc, risc_os(127)),
            _ => assert_eq!(bbc, risc_os(code), "{code}"),
        }
    }
    assert_ne!(
        risc_os(96),
        risc_os(163),
        "RISC OS's 96 is the grave accent"
    );
}

#[test]
fn no_two_characters_share_a_picture_but_those_drawn_alike_on_purpose() {
    let alike = [
        (32, 160),
        (34, 149),
        (39, 145),
        (45, 153),
        (45, 173),
        (153, 173),
    ];
    for first in FIRST..255 {
        for second in first + 1..=255 {
            let same = risc_os(first) == risc_os(second);
            assert_eq!(
                same,
                alike.contains(&(first, second)),
                "{first} and {second}"
            );
        }
    }
}

#[test]
fn an_accented_letter_keeps_its_letter_below_its_accent() {
    for (accented, letter) in [
        (b'\xE9', b'e'),
        (b'\xF1', b'n'),
        (b'\xFC', b'u'),
        (b'\xE0', b'a'),
    ] {
        assert_eq!(risc_os(accented)[2..], risc_os(letter)[2..], "{accented}");
        assert_eq!(risc_os(letter)[..2], [0, 0]);
        assert_ne!(risc_os(accented)[..2], [0, 0]);
    }
}

#[test]
fn only_the_drawn_characters_have_glyphs() {
    assert_eq!(risc_os_glyph(FIRST - 1), None, "a control code");
    assert!(risc_os_glyph(255).is_some());
    assert_eq!(bbc_glyph(128), None, "past the BBC's set");
    assert_eq!(bbc_glyph(FIRST), Some(&BBC[0]));
    assert_eq!(
        draw([
            "#.......", ".......#", "........", "........", "........", "........", "........",
            "########"
        ]),
        [0x80, 0x01, 0, 0, 0, 0, 0, 0xFF]
    );
}

#[test]
fn an_unassigned_code_shows_its_number() {
    assert_eq!(
        risc_os(0x83),
        [0x00, 0xEE, 0xA2, 0xE6, 0xA2, 0xEE, 0x00, 0x00],
        "8 and 3, three pixels wide"
    );
    assert_eq!(number(0x8B)[1..6], [0xEC, 0xAA, 0xEC, 0xAA, 0xEC]);
    for code in 128..=159u8 {
        let assigned = RISC_OS_UPPER[usize::from(code - 128)].is_some();
        assert_eq!(risc_os(code) == number(code), !assigned, "{code}");
    }
}

#[test]
fn characters_find_their_glyphs() {
    assert_eq!(glyph_of('A'), Some(risc_os(b'A')));
    assert_eq!(glyph_of('é'), Some(risc_os(0xE9)));
    assert_eq!(glyph_of('€'), Some(risc_os(0x80)));
    assert_eq!(glyph_of('\u{FB02}'), Some(risc_os(0x9F)), "ﬂ");
    assert_eq!(glyph_of('\u{2588}'), Some(risc_os(127)), "the full block");
    assert_eq!(glyph_of('\u{7F}'), None);
    assert_eq!(
        glyph_of('\u{01CD}'),
        None,
        "Ǎ, in no alphabet, is not drawn"
    );
}

#[test]
fn the_other_alphabets_letters_are_drawn_from_shared_parts() {
    let ogonek = glyph_of('Ą').expect("drawn");
    assert_eq!(
        ogonek[..7],
        risc_os(b'A')[..7],
        "the capital, its mark below"
    );
    assert_ne!(ogonek[7], 0);
    let macron = glyph_of('Ā').expect("drawn");
    assert_eq!(
        macron[2..],
        risc_os(0xC0)[2..],
        "À's body under another accent"
    );
    assert_eq!(macron[..2], [0x7E, 0]);
    assert_eq!(glyph_of('А'), Some(risc_os(b'A')), "Cyrillic A is Latin A");
    assert_eq!(glyph_of('Ο'), Some(risc_os(b'O')), "Greek Omicron is O");
    assert_eq!(glyph_of('Г'), glyph_of('Γ'), "one Gamma for both scripts");
    assert_eq!(
        glyph_of('\u{037E}'),
        Some(risc_os(b';')),
        "the Greek question mark is a semicolon"
    );
    for drawn in ['Ж', 'щ', 'Ω', 'ψ', 'ש', 'ק', 'ł', 'ő', '№'] {
        let glyph = glyph_of(drawn).expect("drawn");
        assert!(glyph.iter().any(|&row| row != 0), "{drawn}");
    }
    assert_eq!(
        glyph_of('\u{200E}'),
        Some([0; 8]),
        "a direction mark shows nothing"
    );
}

#[test]
fn no_two_scripts_letters_collide_but_those_drawn_alike() {
    let soft_sign = glyph_of('Ь').expect("drawn");
    assert_ne!(Some(soft_sign), glyph_of('b'), "the soft sign is not a b");
    assert_ne!(glyph_of('ı'), glyph_of('i'), "the dotless i has no dot");
    assert_ne!(glyph_of('İ'), glyph_of('I'));
}

/// The characters past RISC OS's arrangement drawn as others are on
/// purpose: a letter several scripts share, and a mark with two code
/// points.
const ALIKE: [&[char]; 45] = [
    &[' ', '\u{A0}', '\u{200E}', '\u{200F}'],
    &[';', '\u{37E}'],
    &['A', 'Α', 'А'],
    &['B', 'Β', 'В'],
    &['C', 'С'],
    &['E', 'Ε', 'Е'],
    &['H', 'Η', 'Н'],
    &['I', 'Ι', 'І'],
    &['J', 'Ј'],
    &['K', 'Κ', 'К'],
    &['M', 'Μ', 'М'],
    &['N', 'Ν'],
    &['O', 'Ο', 'О'],
    &['P', 'Ρ', 'Р'],
    &['S', 'Ѕ'],
    &['T', 'Τ', 'Т'],
    &['X', 'Χ', 'Х'],
    &['Y', 'Υ'],
    &['Z', 'Ζ'],
    &['a', 'а'],
    &['c', 'с'],
    &['e', 'е'],
    &['i', 'і'],
    &['j', 'ј'],
    &['o', 'ο', 'о'],
    &['p', 'р'],
    &['s', 'ѕ'],
    &['v', 'ν'],
    &['x', 'х'],
    &['y', 'у'],
    &['—', '―'],
    &['´', '΄'],
    &['µ', 'μ'],
    &['·', '\u{387}'],
    &['Ë', 'Ё'],
    &['Ï', 'Ϊ', 'Ї'],
    &['Ð', 'Đ'],
    &['ë', 'ё'],
    &['ï', 'ї'],
    &['ó', 'ό'],
    &['ĸ', 'κ', 'к'],
    &['Ÿ', 'Ϋ'],
    &['Γ', 'Г'],
    &['Π', 'П'],
    &['Φ', 'Ф'],
];

/// Each character of RISC OS's arrangement, by its code.
fn arranged(code: u8) -> Option<char> {
    match code {
        127 => Some('\u{2588}'),
        0x20..=0x7E => Some(char::from(code)),
        0x80.. => RISC_OS_UPPER[usize::from(code - 0x80)],
        _ => None,
    }
}

#[test]
fn the_other_alphabets_characters_share_pictures_only_on_purpose() {
    let everything = (FIRST..=255)
        .filter_map(arranged)
        .chain(super::extended::GLYPHS.iter().map(|&(c, _)| c));
    for &(character, glyph) in &super::extended::GLYPHS {
        let group = ALIKE
            .iter()
            .find(|group| group.contains(&character))
            .copied()
            .unwrap_or(&[]);
        for other in everything.clone().filter(|&other| other != character) {
            let same = glyph_of(other) == Some(glyph);
            assert_eq!(same, group.contains(&other), "{character} and {other}");
        }
    }
}

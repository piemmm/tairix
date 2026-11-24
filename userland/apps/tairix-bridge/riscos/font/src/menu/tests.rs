use super::*;
use crate::catalogue::families;

const LABELS: Labels<'static> = Labels {
    title: b"Font List",
    regular: b"(Regular)",
    system: b"System font",
};

/// RISC OS 5.30's own fonts, as its catalogue lists them.
const ROM: [&[u8]; 13] = [
    b"Corpus.Bold",
    b"Corpus.Bold.Oblique",
    b"Corpus.Medium",
    b"Corpus.Medium.Oblique",
    b"Homerton.Bold",
    b"Homerton.Bold.Oblique",
    b"Homerton.Medium",
    b"Homerton.Medium.Oblique",
    b"Trinity.Bold",
    b"Trinity.Bold.Italic",
    b"Trinity.Medium",
    b"Trinity.Medium.Italic",
    b"WIMPSymbol",
];

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

#[test]
fn the_rom_fonts_menu_is_laid_out_word_for_word_as_risc_os_5_30_lays_it() {
    let families = families(ROM.into_iter());
    let options = Options {
        system: false,
        tick: None,
        terminator: 0,
    };
    let sizes = sizes(&families, LABELS, options);
    assert_eq!(
        sizes,
        Sizes {
            menu: 548,
            data: 163
        }
    );
    let (menu_at, data_at) = (0x2_BEB4, 0x2_DEC4);
    let mut menu = alloc::vec![0; sizes.menu];
    let data = build(&families, LABELS, options, &mut menu, menu_at, data_at);
    assert_eq!(data.len(), sizes.data);
    let words = words(&menu);
    assert_eq!(
        words[..38],
        [
            0x746E_6F46,
            0x7369_4C20,
            0x74,
            0x7_0207,
            0xAC,
            0x2C,
            0, //
            0,
            0x2_BF30,
            0x700_0101,
            0x2_DEC4,
            NONE,
            7, //
            0,
            0x2_BFAC,
            0x700_0101,
            0x2_DECB,
            NONE,
            9, //
            0,
            0x2_C028,
            0x700_0101,
            0x2_DED4,
            NONE,
            8, //
            0x80,
            NONE,
            0x700_0101,
            0x2_DEDC,
            NONE,
            0xB, //
            0x7072_6F43,
            0x7375,
            0,
            0x7_0207,
            0xEC,
            0x2C,
            0,
        ]
    );
    assert_eq!(words[38..44], [0, NONE, 0x700_0101, 0x2_DEE7, NONE, 5]);
    assert_eq!(
        words[words.len() - 13..],
        [
            0x504D_4957,
            0x626D_7953,
            0x6C6F,
            0x7_0207,
            0x9C,
            0x2C,
            0, //
            0x80,
            NONE,
            0x700_0101,
            0x2_DF5D,
            NONE,
            0xA,
        ],
        "WIMPSymbol's submenu is built, though not linked"
    );
    assert!(data.starts_with(b"Corpus\0Homerton\0Trinity\0WIMPSymbol\0Bold\0Bold.Oblique\0"));
    assert!(data.ends_with(b"Medium.Italic\0(Regular)\0"));
}

#[test]
fn a_title_of_twelve_is_unterminated_and_a_longer_one_indirected() {
    let names: [&[u8]; 3] = [
        b"Thirteenchar.Medium",
        b"Fourteenchars.Medium",
        b"Fourteenchars.X",
    ];
    let families = families(names.into_iter());
    let options = Options {
        system: false,
        tick: None,
        terminator: 0,
    };
    let sizes = sizes(&families, LABELS, options);
    let mut menu = alloc::vec![0xEE; sizes.menu];
    let data = build(&families, LABELS, options, &mut menu, 0x1000, 0x8000);
    let words = words(&menu);
    let first = HEADER / 4 + 2 * ITEM / 4;
    assert_eq!(
        words[first..first + 7],
        [
            0x7269_6854,
            0x6E65_6574,
            0x7261_6863,
            0x7_0207,
            0xAC,
            0x2C,
            0
        ]
    );
    let second = first + (HEADER + ITEM) / 4;
    assert_eq!(words[second..second + 3], [0x8000 + 13 + 14 + 7, 0, 0]);
    assert_eq!(words[second + 4], 0xBC, "width counts the title less two");
    assert_eq!(words[second + 7], flag::INDIRECTED_TITLE);
    assert!(data.ends_with(b"Fourteenchars\0Medium\0X\0"));
}

#[test]
fn a_tick_marks_the_family_and_its_member_and_the_system_font_heads_the_menu() {
    let families = families(ROM.into_iter());
    let options = Options {
        system: true,
        tick: Some(Tick::Font {
            family: 2,
            member: 2,
        }),
        terminator: 13,
    };
    let sizes = sizes(&families, LABELS, options);
    assert_eq!(
        sizes,
        Sizes {
            menu: 572,
            data: 175
        }
    );
    let mut menu = alloc::vec![0; sizes.menu];
    let data = build(&families, LABELS, options, &mut menu, 0, 0);
    let words = words(&menu);
    assert_eq!(words[HEADER / 4 + 3 * ITEM / 4], flag::TICKED);
    assert!(data.starts_with(b"System font\rCorpus\r"));
    assert_eq!(menu[9], 13, "the title ends as the texts do");
}

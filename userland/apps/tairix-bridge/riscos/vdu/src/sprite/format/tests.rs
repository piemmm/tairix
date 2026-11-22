//! RISC OS 5.30's reading of sprite mode words, as `OS_ReadModeVariable` and
//! the sprite calls were recorded reading them.

use tairix_vdu::mode::var;

use super::{variable, word_for, Pixels};
use crate::mode::Format;

/// A RISC OS 3.5 word of `sprite_type` at `x` by `y` dots to the inch.
const fn ro35(sprite_type: u32, x: u32, y: u32) -> u32 {
    sprite_type << 27 | y << 14 | x << 1 | 1
}

/// A RISC OS 5 word of `sprite_type` with flags byte `flags`, eigen factors
/// one across and two down.
const fn ro5(sprite_type: u32, flags: u32) -> u32 {
    0x7800_0001 | sprite_type << 20 | flags << 8 | 2 << 6 | 1 << 4
}

/// Every variable of `word` in turn, none for each RISC OS 5.30 answers with C
/// set, as the recording lists them.
fn variables(word: u32) -> [Option<u32>; 14] {
    core::array::from_fn(|n| variable(word, u32::try_from(n).expect("a small number")))
}

#[test]
fn a_risc_os_3_5_word_answers_its_type_s_colours_depth_and_resolutions() {
    let none = None;
    assert_eq!(
        variables(ro35(4, 90, 90)),
        [
            Some(0),
            none,
            none,
            Some(255),
            Some(1),
            Some(1),
            none,
            none,
            Some(0),
            Some(3),
            Some(3),
            none,
            none,
            none
        ],
        "type 4 counts 256 colours, not sixty-four"
    );
    assert_eq!(variable(ro35(10, 90, 90), var::MODE_FLAGS), Some(0x80));
    assert_eq!(variable(ro35(10, 90, 90), var::N_COLOUR), Some(0xFFFF));
    for unsupported in [7, 8, 9, 11, 14] {
        assert_eq!(
            variable(ro35(unsupported, 90, 90), var::N_COLOUR),
            Some(u32::MAX)
        );
        assert_eq!(variable(ro35(unsupported, 90, 90), var::LOG2_BPP), Some(5));
    }
    let odd = ro35(6, 100, 90);
    assert_eq!(
        (variable(odd, var::X_EIG), variable(odd, var::Y_EIG)),
        (None, Some(1))
    );
    assert_eq!(variable(ro35(6, 22, 23), var::X_EIG), Some(3));
    assert_eq!(
        variable(ro35(4, 90, 90) | 1 << 31, var::N_COLOUR),
        Some(255),
        "the wide bit is no matter"
    );
}

#[test]
fn a_risc_os_5_word_carries_its_flags_while_they_are_ones_risc_os_reads() {
    assert_eq!(variable(ro5(6, 0xC0), var::MODE_FLAGS), Some(0xC000));
    assert_eq!(variable(ro5(10, 0x40), var::MODE_FLAGS), Some(0x4080));
    assert_eq!(variable(ro5(16, 0), var::N_COLOUR), Some(0xFFF));
    assert_eq!(
        (
            variable(ro5(6, 0), var::X_EIG),
            variable(ro5(6, 0), var::Y_EIG)
        ),
        (Some(1), Some(2))
    );
    let strange = ro5(6, 0x10);
    assert_eq!(variable(strange, var::MODE_FLAGS), None);
    assert_eq!(
        variable(strange, var::N_COLOUR),
        Some(u32::MAX),
        "the rest still answered"
    );
    assert_eq!(variables(ro5(19, 0)), [None; 14], "a type past RISC OS 5's");
    for as_type_6 in [7, 8, 9, 11, 17, 18] {
        assert_eq!(variable(ro5(as_type_6, 0), var::LOG2_BPP), Some(5));
    }
}

#[test]
fn a_risc_os_5_word_lacking_its_pattern_is_read_in_risc_os_3_5_s_form() {
    let word = 0x7861_0001;
    assert_eq!(variable(word, var::MODE_FLAGS), Some(0));
    assert_eq!(variable(word, var::N_COLOUR), Some(u32::MAX));
    assert_eq!(
        (variable(word, var::X_EIG), variable(word, var::Y_EIG)),
        (None, None)
    );
    let pixels = Pixels::stored(0x7816_80B5).expect("a type 15 word of 90 by 90");
    assert_eq!(pixels.log2_bpp, 5);
}

#[test]
fn a_numbered_mode_doubling_its_pixels_holds_each_in_its_wider_field() {
    let mode_2 = Pixels::stored(2).expect("mode 2");
    assert_eq!((mode_2.log2_bpp, mode_2.log2_field), (2, 3));
    assert!(mode_2.old_format);
    assert!(Pixels::stored(3).is_err(), "a text mode");
    assert!(Pixels::stored(60).is_err(), "a mode RISC OS does not know");
    assert!(Pixels::stored(0x80).is_err(), "the shadow bit");
    assert!(Pixels::stored(0x8000_000C).is_err(), "a selector's address");
    assert!(
        Pixels::stored(ro5(6, 0x10)).is_err(),
        "a colour space not RGB"
    );
}

#[test]
fn a_screen_s_pixels_take_the_most_backward_compatible_word() {
    let format = |log2_bpp, pairs: &[(u32, u32)]| {
        Format::selector(320, 256, log2_bpp, pairs.iter().copied()).expect("a screen")
    };
    assert_eq!(word_for(&format(4, &[])), Some(0x2816_80B5));
    assert_eq!(word_for(&format(5, &[])), Some(0x3016_80B5));
    assert_eq!(word_for(&format(3, &[])), Some(0x2016_80B5));
    assert_eq!(
        word_for(&format(4, &[(var::MODE_FLAGS, 0x80)])),
        Some(0x5016_80B5)
    );
    assert_eq!(
        word_for(&format(4, &[(var::MODE_FLAGS, 0), (var::N_COLOUR, 4095)])),
        Some(0x7900_0051)
    );
}

use super::*;
use crate::mode::Format;

fn numbered(n: u32) -> Format {
    Format::numbered(tairix_vdu::mode::numbered(n).expect("a mode"))
}

#[test]
fn a_six_bit_colour_and_tint_make_the_vidc1_byte_and_back() {
    assert_eq!(
        internal(21, 0x80),
        0x2E,
        "as genuine RISC OS 5's block read"
    );
    assert_eq!(internal(63, 0xFF), 0xFF);
    assert_eq!(internal(1, 0xFF), 0x07);
    assert_eq!(internal(2, 0), 0x10);
    for c in 0..64 {
        for tint in [0, 0x40, 0x80, 0xC0] {
            assert_eq!(number_of_internal(internal(c, tint)), (c, tint));
        }
    }
}

#[test]
fn a_colour_fills_its_word_as_the_mode_s_pixels() {
    assert_eq!(replicate(&numbered(1), 1), 0x5555_5555);
    assert_eq!(replicate(&numbered(12), 7), 0x7777_7777);
    assert_eq!(replicate(&numbered(28), 0x2E), 0x2E2E_2E2E);
    assert_eq!(pixel(&numbered(12), 21, 0), 5, "within sixteen colours");
}

#[test]
fn bbc_patterns_become_native_ones() {
    assert_eq!(
        bbc_to_native(0x80, 0),
        0x01,
        "one bit: the leftmost pixel the low bit"
    );
    assert_eq!(
        bbc_to_native(0x88, 1),
        0x03,
        "two bits: b7 and b3 the first pixel"
    );
    assert_eq!(
        bbc_to_native(0xAA, 2),
        0x0F,
        "four bits: b7, b5, b3, b1 the first pixel"
    );
    assert_eq!(bbc_to_native(0x12, 3), 0x12);
}

#[test]
fn a_pattern_byte_widens_to_a_double_pixel_mode_s_pixels() {
    assert_eq!(expand(&numbered(2), 0x13), 0x1133_1133);
    assert_eq!(expand(&numbered(12), 0x13), 0x1313_1313);
}

#[test]
fn each_action_s_words_do_what_it_says() {
    let apply = |op, old: u32, c: u32| {
        let [ora, eor] = operate(op, c);
        (old | ora) ^ eor
    };
    let (old, c) = (0b1100, 0b1010);
    assert_eq!(apply(0, old, c), c);
    assert_eq!(apply(1, old, c), old | c);
    assert_eq!(apply(2, old, c), old & c);
    assert_eq!(apply(3, old, c), old ^ c);
    assert_eq!(apply(4, old, c), !old);
    assert_eq!(apply(5, old, c), old);
    assert_eq!(apply(6, old, c), old & !c);
    assert_eq!(apply(7, old, c), old | !c);
}

#[test]
fn a_mode_s_default_patterns_are_its_depth_s() {
    assert_eq!(
        default_patterns(&numbered(0))[3][..4],
        [0xC0, 0x30, 0x0C, 0x03]
    );
    assert_eq!(default_patterns(&numbered(1))[0][..2], [0x66, 0x55]);
    assert_eq!(default_patterns(&numbered(15))[1][..4], [3, 2, 1, 0]);
}

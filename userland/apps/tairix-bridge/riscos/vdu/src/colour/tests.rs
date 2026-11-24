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

#[test]
fn a_256_colour_gcol_and_colour_number_shuffle_their_bits() {
    for (number, gcol) in [
        (0x15, 0x0D),
        (0x62, 0x32),
        (0x88, 0xC0),
        (0xF0, 0xB8),
        (0x25, 0x15),
        (0x98, 0xC8),
    ] {
        assert_eq!(gcol_of_number(number), gcol);
        assert_eq!(number_of_gcol(gcol), number);
    }
    for n in 0..=u8::MAX {
        assert_eq!(number_of_gcol(gcol_of_number(n)), n);
    }
}

#[test]
fn each_gcol_action_lays_its_bits_as_named_by_its_low_three_bits() {
    let (old, bits) = (0b1100u32, 0b1010u32);
    let named = [
        bits,
        old | bits,
        old & bits,
        old ^ bits,
        !old,
        old,
        old & !bits,
        old | !bits,
    ];
    for (action, laid) in (0u32..).zip(named) {
        assert_eq!(combine(action, old, bits), laid, "action {action}");
        assert_eq!(
            combine(action | 0x18, old, bits),
            laid,
            "action {action} with ECF bits"
        );
    }
}

#[test]
fn each_direct_layout_holds_its_channels_in_its_fields_either_way_round() {
    let full = Flags::FULL_PALETTE.bits();
    let layouts = [
        ((4, 0xFFF, 0), [4, 4, 4], 2, 0xFFF),
        ((4, 0xFFFF, full), [5, 6, 5], 2, 0xFFFF),
        ((4, 0xFFFF, 0), [5, 5, 5], 2, 0x7FFF),
        ((5, u32::MAX, 0), [8, 8, 8], 4, 0xFF_FFFF),
    ];
    for ((log2_bpp, ncolour, flags), [low, middle, high], bytes, colour_bits) in layouts {
        for swapped in [false, true] {
            let flags = flags | if swapped { Flags::RGB.bits() } else { 0 };
            let direct = Direct::of_parts(log2_bpp, ncolour, flags);
            let bits = if swapped {
                [high, middle, low]
            } else {
                [low, middle, high]
            };
            assert_eq!(direct.channel_bits(), bits);
            assert_eq!((direct.bytes(), direct.colour_bits()), (bytes, colour_bits));
            let red = (1 << bits[0]) - 1;
            assert_eq!(
                direct.pack_channels([red, 0, 0]),
                if swapped { red << (low + middle) } else { red },
                "red lowest unless swapped"
            );
            for pixel in [0, 1, 0x1234, 0x00AB_CDEF, u32::MAX].map(|p| p & colour_bits) {
                assert_eq!(direct.pack_channels(direct.channels(pixel)), pixel);
                let bytes_of = |rgb: [u32; 3]| rgb.map(|v| v.to_le_bytes()[0]);
                assert_eq!(
                    direct.pack(bytes_of(direct.unpack_replicated(pixel))),
                    pixel
                );
                assert_eq!(direct.pack(bytes_of(direct.unpack(pixel))), pixel);
            }
            assert_eq!(direct.unpack_replicated(colour_bits), [255; 3]);
            assert_eq!(direct.unpack(colour_bits), [255; 3]);
        }
    }
}

#[test]
fn a_colour_number_s_nibbles_widen_as_risc_os_5_widens_them() {
    let five_six_five = Direct::of_parts(4, 0xFFFF, Flags::FULL_PALETTE.bits());
    assert_eq!(
        five_six_five.pack_nibbles([0b1000, 0b1000, 0b1000]),
        0b1_0000 | (0b10_0010 << 5) | (0b1_0000 << 11),
        "five bits the low bit repeated, six the top two"
    );
    let eight = Direct::of_parts(5, u32::MAX, 0);
    assert_eq!(eight.pack_nibbles([0xA, 0x5, 0x1]), 0x11_55AA);
}

#[test]
fn only_4k_and_16m_colours_keep_a_value_beside_their_colour() {
    assert!(Direct::of_parts(5, u32::MAX, 0).tagged(), "16M");
    assert!(Direct::of_parts(4, 0xFFF, 0).tagged(), "4K");
    assert!(!Direct::of_parts(4, 0xFFFF, 0).tagged(), "32K");
    let five_six_five = Direct::of_parts(4, 0xFFFF, Flags::FULL_PALETTE.bits());
    assert!(!five_six_five.tagged(), "64K");
}

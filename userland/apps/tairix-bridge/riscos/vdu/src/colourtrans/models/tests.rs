use super::*;

#[test]
fn rgb_to_hsv_rounds_the_hue_s_share_away_from_nought_and_cuts_the_saturation() {
    // Each as RISC OS 5 converted it, channels past nought and one included.
    for (rgb, hsv) in [
        ([0, 0, 0], [NO_HUE, 0, 0]),
        ([0x4000, 0xC000, 0xFFFF], [0xC7_FFB0, 0xBFFF, 0xFFFF]),
        ([0x8000, 0xABCD, 0x1_0000], [0xDB_77E8, 0x8000, 0x1_0000]),
        ([0xC000, -0x4000, 0x4000], [0x14A_0000, 0x1_5555, 0xC000]),
        ([0x1_0000, 0x8000, 0x1_8000], [0x10E_0000, 0xAAAA, 0x1_8000]),
        ([0x2345; 3], [NO_HUE, 0, 0x2345]),
        ([0xABCD, 0x1_8000, 0x8000], [0x6D_BBF4, 0xAAAA, 0x1_8000]),
        ([0xFFFF, 0x4000, -0x4000], [0x18_0024, 0x1_4000, 0xFFFF]),
        ([0x1_8000, 0x1_0000, 0xABCD], [0x17_CEEC, 0x8D77, 0x1_8000]),
        ([-0x4000, 0xFFFF, 0xC000], [0xA8_0048, 0x1_4000, 0xFFFF]),
        ([0, 0, 0x9E37], [0xF0_0000, 0x1_0000, 0x9E37]),
    ] {
        assert_eq!(rgb_to_hsv(rgb), hsv, "{rgb:X?}");
    }
}

#[test]
fn hsv_to_rgb_cuts_each_step_and_refuses_no_hue_with_no_saturation() {
    for (hsv, rgb) in [
        ([0, 0x8000, 0xC000], [0xC000, 0x6000, 0x6000]),
        ([0, 0x1_0000, 0x8000], [0x8000, 0, 0]),
        ([0x1_0000, 0, 0x1_0000], [0x1_0000; 3]),
        ([0x1_0000, 0x8000, 0xC000], [0xC000, 0x6199, 0x6000]),
        ([0x1_0000, 0x1_0000, 0x8000], [0x8000, 0x222, 0]),
        ([0x3C_0000, 0x8000, 0xC000], [0xC000, 0xC000, 0x6000]),
        ([0x78_0000, 0x8000, 0xC000], [0x6000, 0xC000, 0x6000]),
        ([0xB4_0000, 0x8000, 0xC000], [0x6000, 0xC000, 0xC000]),
        ([0xF0_0000, 0x8000, 0xC000], [0x6000, 0x6000, 0xC000]),
        ([0x167_0000, 0x8000, 0xC000], [0xC000, 0x6000, 0x619A]),
        ([0x167_0000, 0x1_0000, 0x8000], [0x8000, 0, 0x222]),
        ([0x168_0000, 0x8000, 0xC000], [0xC000, 0x6000, 0x6000]),
        ([0x190_0000, 0x8000, 0x8000], [0x8000, 0x6AAA, 0x4000]),
    ] {
        assert_eq!(hsv_to_rgb(hsv), Some(rgb), "{hsv:X?}");
    }
    assert_eq!(hsv_to_rgb([0, 0, 0x8000]), None);
}

#[test]
fn cmyk_takes_black_equally_from_each_ink_and_gives_no_channel_below_nought() {
    for (rgb, cmyk) in [
        ([0, 0, 0], [0, 0, 0, 0x1_0000]),
        ([0x4000, 0xC000, 0xFFFF], [0xBFFF, 0x3FFF, 0, 1]),
        ([0x8000, 0xABCD, 0x1_0000], [0x8000, 0x5433, 0, 0]),
        ([0xC000, -0x4000, 0x4000], [0, 0x1_0000, 0x8000, 0x4000]),
        ([0x1_0000, 0x8000, 0x1_8000], [0x8000, 0x1_0000, 0, -0x8000]),
    ] {
        assert_eq!(rgb_to_cmyk(rgb), cmyk, "{rgb:X?}");
    }
    assert_eq!(cmyk_to_rgb([0, 0xC000, 0x2345, 0xFFFF]), [1, 0, 0]);
    assert_eq!(cmyk_to_rgb([0x4000, 0x1_0000, 0xABCD, 0x1_8000]), [0, 0, 0]);
}

#[test]
fn cie_conversions_cut_each_sum_toward_minus_infinity_as_risc_os_5_does() {
    for (rgb, xyz) in [
        ([0x4000, 0xC000, 0xFFFF], [0x84A0, 0xA37D, 0x1_087E]),
        ([0x1_0000, 0x8000, 0x1_8000], [0xE3AE, 0xB695, 0x1_763F]),
        ([0xFFFF, 0x4000, -0x4000], [0x825A, 0x6827, -0x2AC6]),
        ([0x7FFF_FFFF, 0, 0], [0x3D35_E07F, 0x21B9_66FF, 0x288_C07F]),
        ([-1, -1, -1], [-1, -1, -2]),
    ] {
        assert_eq!(rgb_to_cie(rgb), Some(xyz), "{rgb:X?}");
    }
    for (xyz, rgb) in [
        ([0x1_0000, 0, 0], [0x2_BD48, -0x1_1E76, 0x2352]),
        ([0, 0, 0x3FFF_FFFF], [-0x1B24_0AC0, 0x21F_0A7F, 0x46B5_0E3E]),
        ([0x8000, 0x8000, 0x8000], [0x9_5D6, 0x7_8A9, 0x7_464]),
    ] {
        assert_eq!(cie_to_rgb(xyz), Some(rgb), "{xyz:X?}");
    }
}

#[test]
fn a_cie_channel_past_a_register_is_refused() {
    assert_eq!(rgb_to_cie([i32::MAX; 3]), None);
    assert_eq!(rgb_to_cie([i32::MIN; 3]), None);
    assert_eq!(cie_to_rgb([i32::MAX, 0, 0]), None);
    assert_eq!(cie_to_rgb([0x4000_0000, 0, 0]), None);
}

#[test]
fn any_register_s_value_converts_without_a_fault() {
    let extremes = [i32::MIN, -1, 0, 1, i32::MAX];
    for &a in &extremes {
        for &b in &extremes {
            for &c in &extremes {
                let _ = rgb_to_hsv([a, b, c]);
                let _ = hsv_to_rgb([a, b, c]);
                let _ = rgb_to_cmyk([a, b, c]);
                let _ = cmyk_to_rgb([a, b, c, a]);
            }
        }
    }
}

#[test]
fn a_product_or_quotient_past_a_word_keeps_its_low_word_as_the_processor_does() {
    assert_eq!(super::times(i32::MIN, 2 << 16), 0, "minus two to the 32nd");
    assert_eq!(super::ratio(i32::MIN, 1 << 15), 0, "the same, by division");
    assert_eq!(
        super::times(-(1 << 16), 3 << 16),
        -(3 << 16),
        "within a word, as it is"
    );
}

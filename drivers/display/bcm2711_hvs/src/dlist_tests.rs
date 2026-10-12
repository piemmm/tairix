use super::*;

const FULL_HD: Surface = Surface {
    bus: 0xC100_0000,
    stride: 7680,
    width: 1920,
    height: 1080,
    format: DisplayFormat::Bgra8888,
};

const UHD: Surface = Surface {
    bus: 0xC200_0000,
    stride: 15_360,
    width: 3840,
    height: 2160,
    format: DisplayFormat::Rgba8888,
};

const fn place(x: u32, y: u32, width: u32, height: u32) -> Place {
    Place {
        x,
        y,
        width,
        height,
    }
}

const KERNEL_AT: u32 = 32;

#[test]
fn a_surface_shown_at_its_own_size_is_one_unity_plane() {
    let words = plane(&FULL_HD, &place(0, 0, 1920, 1080), KERNEL_AT, 0).expect("fits");
    assert_eq!(
        words.as_slice(),
        [
            0x4800_D807, // valid, eight words, unity, ARGB, RGBA8888, expanded
            0,
            0x4000_FFF0, // fixed, opaque
            0x0438_0780, // 1920x1080
            CONTEXT,
            0xC100_0000,
            CONTEXT,
            7680,
        ]
    );
}

#[test]
fn an_enlarged_plane_filters_both_axes_through_the_kernel() {
    let at = place(0, 0, 3840, 2160);
    let words = plane(&FULL_HD, &at, KERNEL_AT, 7680).expect("fits");
    let half_scale_ppf = 0x4080_0060; // AGC, 0.5 in 16.16, phase -1/2
    assert_eq!(words.len(), 17);
    assert_eq!(words[0] >> 24 & 0x3F, 17, "the size it states");
    assert_eq!(words[0] & CTL0_UNITY, 0);
    assert_eq!(words[0] >> CTL0_SCL0_SHIFT & 7, 0, "polyphase both ways");
    assert_eq!(words[3], 0x0870_0F00, "drawn at 3840x2160");
    assert_eq!(words[4], 0x0438_0780, "from 1920x1080");
    assert_eq!(
        words[9..],
        [
            7680,
            half_scale_ppf,
            half_scale_ppf,
            CONTEXT,
            KERNEL_AT,
            KERNEL_AT,
            KERNEL_AT,
            KERNEL_AT
        ]
    );
    assert_eq!(
        lbm_words(1920, Scaling::Ppf, Scaling::Ppf, at.width),
        7680,
        "sixteen lines of the source width, in four-pixel words"
    );
}

#[test]
fn a_halved_plane_shrinks_through_the_trapezoidal_filter() {
    let at = place(0, 0, 1920, 1080);
    let words = plane(&UHD, &at, KERNEL_AT, 0).expect("fits");
    assert_eq!(words.len(), 15, "no kernel for a trapezoid");
    assert_eq!(words[0] >> CTL0_SCL0_SHIFT & 7, 3);
    assert_eq!(words[0] >> CTL0_SCL1_SHIFT & 7, 3);
    assert_eq!(words[0] >> CTL0_ORDER_SHIFT & 3, ORDER_ABGR, "R, G, B, A");
    assert_eq!(
        words[10..],
        [0x0200_0000, 0x7FFF, 0x0200_0000, 0x7FFF, CONTEXT],
        "a scale of two and its reciprocal, each way"
    );
    assert_eq!(lbm_words(3840, Scaling::Tpz, Scaling::Tpz, 1920), 3840);
}

#[test]
fn the_longest_plane_fits_its_bound() {
    // Shrunk across past a third, enlarged down: trapezoid and polyphase.
    let words = plane(&UHD, &place(0, 0, 1280, 3000), KERNEL_AT, 0).expect("fits");
    assert_eq!(words.len(), PLANE_WORDS_MAX);
}

#[test]
fn each_axis_takes_the_filter_its_ratio_needs() {
    assert_eq!(Scaling::of(1920, 1920), Scaling::None);
    assert_eq!(Scaling::of(1920, 3840), Scaling::Ppf, "enlarged");
    assert_eq!(Scaling::of(1920, 1280), Scaling::Ppf, "shrunk by a third");
    assert_eq!(Scaling::of(1920, 1279), Scaling::Tpz, "shrunk further");
    assert_eq!(Scaling::of(1, 4), Scaling::Tpz, "a single pixel");
}

#[test]
fn a_surface_fits_its_output_with_its_aspect_kept() {
    assert_eq!(
        Place::fit(1920, 1080, 1024, 768),
        place(0, 96, 1024, 576),
        "letterboxed"
    );
    assert_eq!(
        Place::fit(1024, 768, 1920, 1080),
        place(240, 0, 1440, 1080),
        "pillarboxed"
    );
    assert_eq!(Place::fit(1920, 1080, 3840, 2160), place(0, 0, 3840, 2160));
}

#[test]
fn a_plane_no_field_holds_is_refused() {
    let at = place(0, 0, 1920, 1080);
    for surface in [
        Surface {
            stride: 1 << 16,
            ..FULL_HD
        },
        Surface {
            width: 8192,
            ..FULL_HD
        },
        Surface {
            width: 0,
            ..FULL_HD
        },
    ] {
        assert_eq!(
            plane(&surface, &at, KERNEL_AT, 0),
            Err(DriverError::OutOfRange)
        );
    }
    assert_eq!(
        plane(&FULL_HD, &place(0, 4096, 1920, 1080), KERNEL_AT, 0),
        Err(DriverError::OutOfRange),
        "a line past the field"
    );
    assert_eq!(
        plane(&FULL_HD, &at, 1 << 14, 0),
        Err(DriverError::OutOfRange),
        "a kernel past the memory"
    );
}

#[test]
fn the_kernel_is_its_half_mirrored_about_the_centre() {
    let words = kernel();
    assert_eq!(words[0], 0x07EB_FC00, "0, -2 and -6 as nine-bit fields");
    for at in 0..5 {
        assert_eq!(words[at], words[KERNEL_WORDS - 1 - at]);
    }
}

use tairix_image::{Density, JpegProcess, Orientation};

use super::*;

fn frame(density: Option<Density>) -> JpegFrame {
    JpegFrame {
        stored: (16, 8),
        components: 3,
        process: JpegProcess::Baseline,
        orientation: Orientation::IDENTITY,
        density,
    }
}

fn whole(across: u32, down: u32, unit: DensityUnit) -> Option<Density> {
    Density::new((across, 1), (down, 1), unit)
}

#[test]
fn a_density_reads_as_risc_os_5_reports_it() {
    assert_eq!(
        density(&frame(None)),
        ((90, 90), true),
        "none stated is the standard screen's"
    );
    assert_eq!(
        density(&frame(whole(72, 72, DensityUnit::Inch))),
        ((72, 72), false)
    );
    assert_eq!(
        density(&frame(whole(118, 118, DensityUnit::Centimetre))),
        ((299, 299), false),
        "cut down"
    );
    assert_eq!(
        density(&frame(whole(1, 2, DensityUnit::Aspect))),
        ((90, 180), true)
    );
    assert_eq!(
        density(&frame(whole(72, 144, DensityUnit::Aspect))),
        ((90, 180), true),
        "in lowest terms"
    );
}

#[test]
fn the_workspace_grows_a_part_for_each_32_pixels_across() {
    let workspace =
        |width: u32| WORKSPACE_BASE + width.div_ceil(WORKSPACE_STRIP) * WORKSPACE_PER_STRIP;
    assert_eq!(workspace(16), 0x9000);
    assert_eq!(workspace(40), 0xA000);
}

#[test]
fn a_plot_s_options_come_from_each_call_s_own_bits() {
    let scaled = Picture::of(0x80B, SCALED_TRANSLUCENCY_SHIFT, SCALED_MAPPING, 0x1234);
    assert_eq!((scaled.translucency, scaled.mapping), (0x80, Some(0x1234)));
    let transformed = Picture::of(
        0x1000,
        TRANSFORMED_TRANSLUCENCY_SHIFT,
        TRANSFORMED_MAPPING,
        9,
    );
    assert_eq!(
        (transformed.translucency, transformed.mapping),
        (0x80, None)
    );
}

#[test]
fn inches_are_whole_and_held_to_a_word() {
    assert_eq!(inches(118, 100), 299);
    assert_eq!(inches(u32::MAX, 1), u32::MAX, "held to a word");
}

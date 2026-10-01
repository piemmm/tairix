//! Unit tests for the figures only this crate spells; the shared spellings
//! are tested where they live, in `lib/procinfo`.

use super::{format_latency, format_pixels, pixel_parts};

#[test]
fn pixels_below_a_thousand_are_whole_pixels() {
    assert_eq!(format_pixels(0), "0 px");
    assert_eq!(format_pixels(512), "512 px");
    assert_eq!(format_pixels(999), "999 px");
}

#[test]
fn pixels_scale_by_thousands_with_one_decimal() {
    assert_eq!(format_pixels(1_000), "1.0k px");
    assert_eq!(format_pixels(3_200), "3.2k px");
    assert_eq!(format_pixels(1920 * 1080), "2.0M px");
}

#[test]
fn a_pixel_count_past_the_last_unit_stays_in_it() {
    assert_eq!(format_pixels(4_000_000_000), "4.0G px");
    assert!(
        format_pixels(u64::MAX).ends_with("G px"),
        "a count past the last unit stays in it rather than wrapping"
    );
}

#[test]
fn a_latency_is_scaled_to_the_unit_that_keeps_it_readable() {
    assert_eq!(format_latency(0), "0 ns");
    assert_eq!(format_latency(999), "999 ns");
    assert_eq!(format_latency(125_000), "125.0 us");
    assert_eq!(format_latency(5_000_000_000), "5.0 s");
    // A figure beyond the last unit saturates in that unit rather than
    // wrapping to a smaller, misleading number.
    assert_eq!(format_latency(u64::MAX), "18446744073.7 s");
}

/// The magnitude prefix belongs to the unit, so a hero's figure is the
/// mantissa alone and the joined spelling is unchanged by the split.
#[test]
fn pixel_parts_split_the_magnitude_into_the_unit() {
    assert_eq!(pixel_parts(512), ("512".into(), "px".into()));
    assert_eq!(pixel_parts(3_200), ("3.2".into(), "k px".into()));
    assert_eq!(pixel_parts(4_200_000), ("4.2".into(), "M px".into()));
    assert_eq!(pixel_parts(4_000_000_000), ("4.0".into(), "G px".into()));
    // Saturates in the last unit rather than wrapping to a smaller figure.
    assert_eq!(pixel_parts(u64::MAX).1, "G px");
}

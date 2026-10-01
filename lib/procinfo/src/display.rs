//! How a desktop surface spells a reading: a byte count with its unit, a
//! rate, a share, a span.
//!
//! The Switchboard and the System Monitor screensaver draw the same figures,
//! so they spell them through these: one disk cannot read `1.9 GiB` on one
//! surface and something else on the other. The terminal tools keep the
//! narrower GNU-familiar column spellings in [`crate::human`].

use alloc::format;
use alloc::string::String;

use tairix_abi::Duration64;
use tairix_util::size::{binary_scale, format_at_scale, format_binary, SIZE_TEXT_MAX};

/// A byte count in the largest binary unit that keeps it under four digits,
/// with one decimal place above a kibibyte (`"1.9 GiB"`) and whole bytes below
/// it (`"512 B"`).
///
/// The scaling is `lib/util`'s, which the desktop's Settings reads too.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    let mut buf = [0u8; SIZE_TEXT_MAX];
    String::from(format_binary(bytes, &mut buf))
}

/// `bytes` at `scale`, with one decimal place above whole bytes.
fn digits_at(bytes: u64, scale: u64) -> String {
    let mut buf = [0u8; SIZE_TEXT_MAX];
    String::from(format_at_scale(bytes, scale, &mut buf))
}

/// A byte count `of` a measured whole, as the figure a hero reads and the unit
/// that trails it: `(8.6 GiB, 16 GiB)` → `("8.6", "/ 16.0 GiB")`.
///
/// Both figures are scaled to the whole's unit, so the pair reads as one
/// quantity: 512 MiB of 16 GiB is `"0.5"` against `"/ 16.0 GiB"`, never
/// `"512"` against a whole in another unit.
#[must_use]
pub fn byte_parts(bytes: u64, of: u64) -> (String, String) {
    let (scale, name) = binary_scale(of);
    (
        digits_at(bytes, scale),
        format!("/ {} {name}", digits_at(of, scale)),
    )
}

/// A bytes-per-second rate in the units of a byte count.
#[must_use]
pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// A permille fraction as whole-percent digits with no unit (`"92"`), for a
/// figure whose unit is drawn beside it.
///
/// Whole percent is the precision a share sampled over one interval earns. A
/// total summed across tasks may exceed `100%` on more than one core, so
/// nothing is clamped.
#[must_use]
pub fn whole_percent(permille: u16) -> String {
    format!("{}", permille / 10)
}

/// A permille fraction as whole-percent display text (`"92%"`).
#[must_use]
pub fn percent(permille: u16) -> String {
    format!("{}%", whole_percent(permille))
}

/// An elapsed span in days, hours and minutes, dropping the units that are
/// nought, so four minutes reads `"4m"` and not `"0d 0h 4m"`.
///
/// Seconds appear only below a minute, where they are the whole reading. A
/// negative span — a clock that moved backwards — reads as no elapsed time.
#[must_use]
pub fn format_duration(duration: Duration64) -> String {
    let seconds = duration.secs().max(0).unsigned_abs();
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        return format!("{days}d {hours}h {minutes}m");
    }
    if hours > 0 {
        return format!("{hours}h {minutes}m");
    }
    if minutes > 0 {
        return format!("{minutes}m");
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use tairix_abi::Duration64;

    use super::{byte_parts, format_bytes, format_duration, format_rate, percent, whole_percent};

    #[test]
    fn bytes_below_a_kibibyte_are_whole_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn bytes_scale_to_the_largest_unit_with_one_decimal() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(655_360), "640.0 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
    }

    #[test]
    fn the_largest_unit_is_the_last_one_rather_than_a_wrap() {
        let pib = 1024u64 * 1024 * 1024 * 1024 * 1024;
        assert_eq!(format_bytes(pib), "1.0 PiB");
        assert!(
            format_bytes(u64::MAX).ends_with(" EiB"),
            "a count past the last unit stays in it rather than wrapping"
        );
    }

    #[test]
    fn a_rate_is_a_byte_count_per_second() {
        assert_eq!(format_rate(0), "0 B/s");
        assert_eq!(format_rate(1024), "1.0 KiB/s");
    }

    #[test]
    fn a_duration_drops_the_units_that_are_nought() {
        assert_eq!(format_duration(Duration64::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration64::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration64::from_secs(7_260)), "2h 1m");
        assert_eq!(format_duration(Duration64::from_secs(90_120)), "1d 1h 2m");
    }

    #[test]
    fn a_negative_duration_reads_as_no_elapsed_time() {
        assert_eq!(
            format_duration(Duration64::from_secs(-5)),
            "0s",
            "a clock that moved backwards must not read as an enormous uptime"
        );
    }

    /// A figure whose unit is drawn beside it carries none of its own: spelled
    /// with a `%` a hero would read `18% % busy`.
    #[test]
    fn a_whole_percent_carries_no_unit_and_the_spelled_form_adds_one() {
        assert_eq!(whole_percent(185), "18");
        assert_eq!(percent(185), "18%");
        assert_eq!(percent(0), "0%");
        // Over a hundred percent is legitimate on more than one core, and is
        // shown as measured rather than clamped.
        assert_eq!(whole_percent(2_400), "240");
    }

    /// The figure and the whole it is a share of are scaled to the whole's
    /// unit, so the pair reads as one quantity.
    #[test]
    fn byte_parts_scales_the_figure_to_the_whole_it_is_a_share_of() {
        let gib = 1024u64 * 1024 * 1024;
        assert_eq!(
            byte_parts(8 * gib + gib / 2, 16 * gib),
            (String::from("8.5"), String::from("/ 16.0 GiB"))
        );
        // Half a gibibyte of sixteen is not "512": the whole is in GiB, so the
        // figure is too.
        assert_eq!(
            byte_parts(gib / 2, 16 * gib),
            (String::from("0.5"), String::from("/ 16.0 GiB"))
        );
        let (figure, _) = byte_parts(8 * gib, 16 * gib);
        assert!(
            !figure.contains("iB") && !figure.contains(' '),
            "the figure carried a unit: {figure}"
        );
    }
}

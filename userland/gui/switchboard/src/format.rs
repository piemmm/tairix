//! The figures only this crate shows — pixel counts and latencies — as
//! display text. Byte counts, rates, shares and spans are spelled by
//! `lib/procinfo`'s display spellings, which the System Monitor screensaver
//! shares.

use alloc::format;
use alloc::string::String;

/// The decimal units a pixel count is scaled through, smallest first.
const PIXEL_UNITS: [&str; 4] = ["", "k", "M", "G"];

/// Where `pixels` lands on the decimal ladder: the divisor that brings it
/// under four digits, and the magnitude prefix that divisor stands for.
///
/// Decimal rather than binary: a pixel count is a screen area, so a reader
/// compares it against a resolution they know in millions, not mebibytes.
/// A count beyond the last unit saturates in that unit rather than wrapping
/// to a smaller, misleading number.
fn pixel_scale(pixels: u64) -> (u64, &'static str) {
    let mut scale = 1u64;
    let mut unit = 0usize;
    while pixels / scale >= 1000 && unit + 1 < PIXEL_UNITS.len() {
        scale = scale.saturating_mul(1000);
        unit = unit.saturating_add(1);
    }
    (scale, PIXEL_UNITS.get(unit).copied().unwrap_or(""))
}

/// A pixel count in the largest decimal unit that keeps it under four
/// digits, with one decimal place above a thousand (`"2.0M px"`) and whole
/// pixels below it (`"512 px"`).
#[must_use]
pub fn format_pixels(pixels: u64) -> String {
    let (scale, name) = pixel_scale(pixels);
    if name.is_empty() {
        return format!("{pixels} px");
    }
    let whole = pixels / scale;
    let tenths = (pixels % scale).saturating_mul(10) / scale;
    format!("{whole}.{tenths}{name} px")
}

/// A pixel count as the figure a hero reads and the unit that trails it:
/// `4_200_000` → `("4.2", "M px")`, `512` → `("512", "px")`.
///
/// The magnitude prefix belongs to the unit, not the figure: a hero reads as
/// one number with its unit beside it, so a figure spelled `"4.2M px"` would
/// put two thirds of a unit in the headline and the rest beside it.
#[must_use]
pub fn pixel_parts(pixels: u64) -> (String, String) {
    let (scale, name) = pixel_scale(pixels);
    if name.is_empty() {
        return (format!("{pixels}"), String::from("px"));
    }
    let whole = pixels / scale;
    let tenths = (pixels % scale).saturating_mul(10) / scale;
    (format!("{whole}.{tenths}"), format!("{name} px"))
}

/// The decimal units a latency is scaled through, smallest first.
const LATENCY_UNITS: [&str; 4] = ["ns", "us", "ms", "s"];

/// A nanosecond latency in the largest decimal unit that keeps it under four
/// digits, with one decimal place above nanoseconds (`"140.0 us"`).
///
/// Scaled like a byte count and for the same reason: a reader comparing two
/// volumes needs the magnitude and one significant place, and more digits
/// would imply an accuracy a figure derived from one interval's delta does
/// not have.
#[must_use]
pub fn format_latency(nanos: u64) -> String {
    let mut scale = 1u64;
    let mut unit = 0usize;
    while nanos / scale >= 1000 && unit + 1 < LATENCY_UNITS.len() {
        scale = scale.saturating_mul(1000);
        unit = unit.saturating_add(1);
    }
    let name = LATENCY_UNITS.get(unit).copied().unwrap_or("ns");
    if unit == 0 {
        return format!("{nanos} {name}");
    }
    let whole = nanos / scale;
    let tenths = (nanos % scale).saturating_mul(10) / scale;
    format!("{whole}.{tenths} {name}")
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;

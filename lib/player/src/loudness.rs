//! A track's loudness from its own tags: the Replay Gain track gain, held
//! below the level at which the track's own peak would clip.
//!
//! The gain is what a player applies to bring tracks mastered at different
//! levels to one loudness (Replay Gain 2.0, `REPLAYGAIN_TRACK_GAIN`). Applying
//! a positive gain to a track whose peak is near full scale would clip it, so
//! the gain is capped by the headroom its stated peak leaves; a track that
//! states no peak is never raised at all.

use tairix_audio::volume::linear_to_millibel;
use tairix_sound::{Metadata, TagKind};

/// The tag a track's gain is stated in.
const TRACK_GAIN: &str = "REPLAYGAIN_TRACK_GAIN";

/// The tag a track's peak sample is stated in, as a fraction of full scale.
const TRACK_PEAK: &str = "REPLAYGAIN_TRACK_PEAK";

/// The largest gain believed, in hundredths of a decibel either way: sixty
/// decibels is past the whole dynamic range of sixteen-bit material, so a
/// larger figure is a damaged tag rather than an instruction.
const MAX_GAIN_MILLIBEL: i32 = 6_000;

/// The most fractional digits of a peak that are read; finer digits are past
/// a float's precision.
const PEAK_DIGITS: u32 = 9;

/// The gain, in hundredths of a decibel, `metadata` asks its track be played
/// at, already capped so that it cannot clip; [`None`] when it states none
/// that can be read.
#[must_use]
pub fn track_millibel(metadata: &Metadata) -> Option<i32> {
    let gain = tag(metadata, TRACK_GAIN).and_then(parse_gain)?;
    let ceiling = tag(metadata, TRACK_PEAK)
        .and_then(parse_peak)
        .and_then(linear_to_millibel)
        .map_or(0, |peak| -peak);
    Some(gain.min(ceiling))
}

fn tag<'a>(metadata: &'a Metadata, key: &str) -> Option<&'a str> {
    metadata.tags.iter().find_map(|tag| match &tag.kind {
        TagKind::Other(found) if found.as_str() == key => Some(tag.value.as_str()),
        _ => None,
    })
}

/// A gain spelt `[+-]digits[.digits] [dB]`, in hundredths of a decibel,
/// rounded half away from zero.
fn parse_gain(text: &str) -> Option<i32> {
    let text = text.trim();
    let unit = text.len().checked_sub(2).and_then(|at| text.get(at..));
    let number = match unit {
        Some(unit) if unit.eq_ignore_ascii_case("db") => text[..text.len() - 2].trim_end(),
        _ => text,
    };
    let (negative, digits) = match number.as_bytes().first()? {
        b'-' => (true, &number[1..]),
        b'+' => (false, &number[1..]),
        _ => (false, number),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if !all_digits(whole) || !all_digits(fraction) || whole.len() > 3 {
        return None;
    }
    let mut hundredths = whole
        .bytes()
        .fold(0i32, |sum, digit| sum * 10 + i32::from(digit - b'0'))
        * 100;
    let mut places = fraction.bytes();
    for scale in [10, 1] {
        hundredths += places.next().map_or(0, |digit| i32::from(digit - b'0')) * scale;
    }
    if places.next().is_some_and(|digit| digit >= b'5') {
        hundredths += 1;
    }
    let millibel = if negative { -hundredths } else { hundredths };
    (millibel.abs() <= MAX_GAIN_MILLIBEL).then_some(millibel)
}

/// A peak spelt `digits[.digits]`, as a multiplier; zero is no peak at all.
fn parse_peak(text: &str) -> Option<f32> {
    let (whole, fraction) = text.trim().split_once('.').unwrap_or((text.trim(), ""));
    let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !all_digits(whole) || !all_digits(fraction) || whole.len() > 3 {
        return None;
    }
    let mut mantissa = whole
        .bytes()
        .fold(0u64, |sum, digit| sum * 10 + u64::from(digit - b'0'));
    let mut scale = 1u64;
    for digit in fraction.bytes().take(PEAK_DIGITS as usize) {
        mantissa = mantissa * 10 + u64::from(digit - b'0');
        scale *= 10;
    }
    let peak = ratio(mantissa, scale);
    (peak > 0.0).then_some(peak)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "a peak is read to a float's precision, which is all the gain it caps can use"
)]
fn ratio(mantissa: u64, scale: u64) -> f32 {
    (mantissa as f64 / scale as f64) as f32
}

#[cfg(test)]
#[path = "loudness_tests.rs"]
mod tests;

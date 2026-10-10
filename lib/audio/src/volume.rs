//! The volume model: an endpoint's own level, a stream's own gain and the
//! router's ducking, resolved into a device setting and one multiply per
//! stream.
//!
//! Every gain is in hundredths of a decibel, the unit a codec's own amplifier
//! capability word converts into without a scale factor, and they compose by
//! addition. The endpoint's level is split once, between its device's control
//! and the mixer, because the control is shared by every stream on the
//! endpoint; each stream's multiply is then its own gains plus whatever of the
//! endpoint's level the device could not take.
//!
//! # Unity is exactly one
//!
//! [`millibel_to_linear`] answers exactly `1.0` at zero, so an untouched
//! stream is multiplied by a number that changes nothing. That is not an
//! accident of the exponential's accuracy; it is a special case, because the
//! stack's bit-exactness claim rests on it.
//!
//! # Hardware takes the attenuation it can, software never amplifies to
//! compensate
//!
//! The hardware setting is rounded to the step *above* the target, so the
//! remainder software applies is always attenuation. Rounding the other way
//! would leave software making up the difference with gain, which costs
//! headroom on a path that has none to spare — and where the target lands on
//! the device's own grid (every whole decibel on most codecs) the software
//! remainder is zero and the multiply is exactly one.

use tairix_abi::audio::AudioGain;
use tairix_abi::driver::audio::GainRange;
use tairix_util::mathf;

/// The gain that changes nothing.
pub const UNITY_MILLIBEL: i32 = AudioGain::UNITY.millibel();

/// The quietest gain a user interface's taper reaches before mute.
///
/// Sixty decibels below unity is past the resolution of sixteen-bit material,
/// so nothing is audible below it and a taper that ran further would waste
/// half its travel on silence.
pub const DEFAULT_FLOOR_MILLIBEL: i32 = -6_000;

/// The attenuation a media stream takes while a conversation or assistive
/// output is live on the same sink.
///
/// Twenty decibels is the broadcast convention for speech over a bed: enough
/// that the speech is plainly dominant, little enough that the media has not
/// simply stopped.
pub const DUCK_MILLIBEL: i32 = -2_000;

/// `ln(10) / 2000`: the exponent one hundredth of a decibel contributes.
const MILLIBEL_EXPONENT: f64 = core::f64::consts::LN_10 / 2_000.0;

/// The linear multiplier `millibel` hundredths of a decibel name.
///
/// Exactly one at unity, and saturating at both ends like the maths module's
/// exponential: an absurd gain answers the largest finite multiplier rather
/// than an infinity that would spread through the mix.
#[must_use]
pub fn millibel_to_linear(millibel: i32) -> f32 {
    if millibel == UNITY_MILLIBEL {
        return 1.0;
    }
    as_f32(mathf::exp(f64::from(millibel) * MILLIBEL_EXPONENT))
}

/// The gain in hundredths of a decibel that multiplies by `linear`: the
/// inverse of [`millibel_to_linear`], exactly unity at one.
///
/// A multiplier no gain names — zero, negative or not finite — answers
/// [`None`].
#[must_use]
pub fn linear_to_millibel(linear: f32) -> Option<i32> {
    if !linear.is_finite() || linear <= 0.0 {
        return None;
    }
    Some(round_i32(mathf::ln(f64::from(linear)) / MILLIBEL_EXPONENT))
}

/// The gain a user-interface control at `fraction` of its travel asks for,
/// with the quiet end at `floor`.
///
/// Linear in decibels, which is what a control labelled in decibels must be:
/// equal travel is equal perceived change, and the number beside the slider
/// is the number the mixer uses. A fraction at or below zero is the floor and
/// one at or above unity is [`UNITY_MILLIBEL`]; the caller decides whether the
/// very bottom of its control means mute, because that is a question about
/// the control rather than about the gain.
#[must_use]
pub fn fraction_to_millibel(fraction: f32, floor: i32) -> i32 {
    if fraction <= 0.0 || !fraction.is_finite() {
        return floor;
    }
    if fraction >= 1.0 {
        return UNITY_MILLIBEL;
    }
    let travel = f64::from(floor) * f64::from(1.0 - fraction);
    round_i32(travel)
}

/// The level a volume control at `permille` of its travel asks for: the
/// default floor at the bottom, unity at the top.
#[must_use]
pub fn level_at_permille(permille: u16) -> AudioGain {
    let fraction = f32::from(permille.min(1_000)) / 1_000.0;
    AudioGain::new(fraction_to_millibel(fraction, DEFAULT_FLOOR_MILLIBEL))
        .unwrap_or(AudioGain::UNITY)
}

/// Where a volume control stands, in permille of its travel, for `level`:
/// [`level_at_permille`] read backwards, at the bottom for anything at or
/// below the floor.
#[must_use]
pub fn permille_of_level(level: AudioGain) -> u16 {
    let below = i64::from(level.millibel().clamp(DEFAULT_FLOOR_MILLIBEL, 0));
    let floor = i64::from(DEFAULT_FLOOR_MILLIBEL);
    u16::try_from((floor - below) * 1_000 / floor).unwrap_or(0)
}

/// How an endpoint's own level is delivered: its device's control takes the
/// attenuation it can, and the mixer applies the rest to every stream on it.
///
/// Split once per endpoint rather than per stream, because the control is the
/// endpoint's and every stream on it shares the one setting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EndpointLevel {
    /// The setting for the device's own control, where it has one.
    pub hardware_millibel: Option<i32>,
    /// What the mixer still applies to every stream on the endpoint: zero or
    /// attenuation, never gain.
    pub software_millibel: i32,
    /// Whether the endpoint is silent, a state of its own rather than a very
    /// small level: unmuting restores the level that was set.
    pub muted: bool,
}

impl EndpointLevel {
    /// An endpoint whose level changes nothing.
    pub const UNITY: Self = Self {
        hardware_millibel: None,
        software_millibel: UNITY_MILLIBEL,
        muted: false,
    };
}

/// Split an endpoint's `level` between its device's `hardware` control, where
/// it has one, and the mixer.
///
/// The control never takes the device past its 0 dB point. Muted, it is set to
/// its quietest as well, so the endpoint is silent even where the mixer's
/// multiply is not what reaches it.
#[must_use]
pub fn endpoint_level(level: AudioGain, muted: bool, hardware: Option<GainRange>) -> EndpointLevel {
    let target = level.millibel();
    let Some(range) = hardware else {
        return EndpointLevel {
            hardware_millibel: None,
            software_millibel: target,
            muted,
        };
    };
    let setting = if muted {
        range.min_millibel()
    } else {
        hardware_setting(target, range)
    };
    EndpointLevel {
        hardware_millibel: Some(setting),
        // A control that cannot get as loud as the target leaves a positive
        // difference the mixer must not make up with gain.
        software_millibel: target.saturating_sub(setting).min(UNITY_MILLIBEL),
        muted,
    }
}

/// The independent gains a stream's multiply is composed from, in hundredths
/// of a decibel, beside the endpoint's own.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct VolumeRequest {
    /// What this stream asked for.
    pub stream_millibel: i32,
    /// What the router's ducking rule imposes, which is zero or
    /// [`DUCK_MILLIBEL`].
    pub duck_millibel: i32,
    /// Whether the stream is muted, a state of its own rather than a very
    /// small gain: unmuting restores the level that was set.
    pub muted: bool,
}

impl VolumeRequest {
    /// The stream's gains' sum, saturating rather than wrapping so an absurd
    /// component cannot turn attenuation into gain.
    #[must_use]
    pub const fn total_millibel(&self) -> i32 {
        self.stream_millibel.saturating_add(self.duck_millibel)
    }
}

/// The one multiply the mix applies to a stream on an endpoint: the stream's
/// own gains and whatever of the endpoint's level its device could not take.
///
/// Exactly one when nothing attenuates, so the bit-exact path stays exact, and
/// exactly zero when the stream or the endpoint is muted.
#[must_use]
pub fn stream_multiply(request: &VolumeRequest, endpoint: EndpointLevel) -> f32 {
    if request.muted || endpoint.muted {
        return 0.0;
    }
    millibel_to_linear(
        request
            .total_millibel()
            .saturating_add(endpoint.software_millibel),
    )
}

/// The setting a control of `range` takes for `target`: the lowest step at or
/// above it, so the remainder left to software is attenuation, and never one
/// past the device's 0 dB point.
fn hardware_setting(target: i32, range: GainRange) -> i32 {
    let setting = step_at_or_above(target, range);
    if setting <= UNITY_MILLIBEL {
        return setting;
    }
    let min = i64::from(range.min_millibel());
    if min > i64::from(UNITY_MILLIBEL) {
        // Every setting lies above 0 dB: the quietest is the closest.
        return range.min_millibel();
    }
    // The grid steps over 0 dB, so the loudest step at or below it.
    let step = i64::from(range.step_millibel());
    let loudest = min + (-min).div_euclid(step) * step;
    i32::try_from(loudest).unwrap_or(range.min_millibel())
}

/// The lowest setting on `range`'s own step grid that is at or above `target`,
/// clamped into the range.
///
/// At or above, so the software remainder is attenuation rather than gain.
fn step_at_or_above(target: i32, range: GainRange) -> i32 {
    let min = i64::from(range.min_millibel());
    let max = i64::from(range.max_millibel());
    let step = i64::from(range.step_millibel());
    let wanted = i64::from(target).clamp(min, max);
    let above = min + (wanted - min).div_euclid(step) * step;
    let snapped = if above < wanted { above + step } else { above };
    // The step grid can overshoot the top of the range by less than one step,
    // in which case the loudest setting is the closest the device has.
    i32::try_from(snapped.min(max)).unwrap_or(range.max_millibel())
}

/// `value` as a linear multiplier.
///
/// Clamped into the narrower type's own range as well as the exponential's:
/// the largest double is past the largest float, so an absurd gain would
/// otherwise become an infinity and spread through everything the mix
/// multiplies it into.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is clamped into f32's range on the line above, so the \
              truncation the lint warns about cannot occur"
)]
fn as_f32(value: f64) -> f32 {
    mathf::clamp(value, 0.0, f64::from(f32::MAX)) as f32
}

/// `value` rounded to the nearest whole hundredth of a decibel.
fn round_i32(value: f64) -> i32 {
    mathf::round_i32(value)
}

/// Why typed text is not a level.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LevelError {
    /// Not decibels to the hundredth.
    Malformed,
    /// Above 0 dB: a level only attenuates.
    Boost,
}

/// A level as a person types it: decibels to the hundredth, with an optional
/// sign and an optional `dB` — `-6`, `-3.5`, `0`, `-12dB`. Configuration
/// keeps to [`AudioGain::parse`]'s one spelling instead.
///
/// # Errors
///
/// [`LevelError::Malformed`] for any other text, and [`LevelError::Boost`]
/// for a level above unity.
pub fn typed_level(text: &str) -> Result<AudioGain, LevelError> {
    let number = text.strip_suffix("dB").unwrap_or(text);
    let (negative, magnitude) = match number.as_bytes().first() {
        Some(b'-') => (true, &number[1..]),
        Some(b'+') => (false, &number[1..]),
        _ => (false, number),
    };
    let (whole, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
    if whole.is_empty()
        || fraction.len() > 2
        || (magnitude.contains('.') && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(LevelError::Malformed);
    }
    let hundredths = fraction
        .bytes()
        .chain(core::iter::repeat(b'0'))
        .take(2)
        .fold(0i64, |sum, digit| sum * 10 + i64::from(digit - b'0'));
    let magnitude = whole
        .parse::<i64>()
        .ok()
        .and_then(|whole| whole.checked_mul(100))
        .and_then(|whole| whole.checked_add(hundredths))
        .ok_or(LevelError::Malformed)?;
    let millibel = i32::try_from(if negative { -magnitude } else { magnitude })
        .map_err(|_| LevelError::Malformed)?;
    AudioGain::new(millibel).map_err(|_| LevelError::Boost)
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;

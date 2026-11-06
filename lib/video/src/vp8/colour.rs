//! The colour conversion between RGB and the luma and chroma a VP8 frame
//! holds: ITU-R BT.601 at video range, the one colour space the format names.
//! The decoder's is the reference decoder's fixed point; the encoder's is its
//! inverse, derived from the same five multipliers so the two cannot part.

use super::{clamp_u8, Rgba8};

/// The decoder's multipliers at its fourteen bits of fixed point: luma's
/// gain, red's from red chroma, green's from blue and from red chroma, and
/// blue's from blue chroma.
const LUMA_GAIN: i32 = 19077;
const RED_FROM_RED: i32 = 26149;
const GREEN_FROM_BLUE: i32 = 6419;
const GREEN_FROM_RED: i32 = 13320;
const BLUE_FROM_BLUE: i32 = 33050;

/// The decoder's offsets of red, green and blue, its rounding folded in.
const RED_OFFSET: i32 = -14234;
const GREEN_OFFSET: i32 = 8708;
const BLUE_OFFSET: i32 = -17685;

/// The opaque pixel a luma sample and its blue and red chroma samples make.
pub(super) fn rgba(luma: u8, blue: u8, red: u8) -> Rgba8 {
    let luma = (i32::from(luma) * LUMA_GAIN) >> 8;
    let (blue, red) = (i32::from(blue), i32::from(red));
    let green = luma - ((blue * GREEN_FROM_BLUE) >> 8) - ((red * GREEN_FROM_RED) >> 8);
    [
        channel(luma + ((red * RED_FROM_RED) >> 8) + RED_OFFSET),
        channel(green + GREEN_OFFSET),
        channel(luma + ((blue * BLUE_FROM_BLUE) >> 8) + BLUE_OFFSET),
        u8::MAX,
    ]
}

/// One channel of the conversion, rounded and clamped.
fn channel(value: i32) -> u8 {
    clamp_u8(value >> 6)
}

/// The bits the decoder's multipliers are fixed to.
#[cfg(any(test, feature = "encode"))]
const INVERSE_BITS: u32 = 14;

/// The bits the encoder's are.
#[cfg(any(test, feature = "encode"))]
const FORWARD_BITS: u32 = 16;

/// The decoder's chroma multipliers crossed: the determinant of its matrix
/// over its luma gain, which every term of the inverse divides by.
#[cfg(any(test, feature = "encode"))]
const CROSSED: i64 = GREEN_FROM_RED as i64 * BLUE_FROM_BLUE as i64
    + RED_FROM_RED as i64 * BLUE_FROM_BLUE as i64
    + RED_FROM_RED as i64 * GREEN_FROM_BLUE as i64;

/// The encoder's multipliers at its fixed point, each a term of the decoder's
/// matrix inverted: luma's from red, green and blue.
#[cfg(any(test, feature = "encode"))]
const LUMA_FROM_RED: i64 = inverted(
    GREEN_FROM_RED as i64 * BLUE_FROM_BLUE as i64,
    LUMA_GAIN as i64 * CROSSED,
);
#[cfg(any(test, feature = "encode"))]
const LUMA_FROM_GREEN: i64 = inverted(
    RED_FROM_RED as i64 * BLUE_FROM_BLUE as i64,
    LUMA_GAIN as i64 * CROSSED,
);
#[cfg(any(test, feature = "encode"))]
const LUMA_FROM_BLUE: i64 = inverted(
    RED_FROM_RED as i64 * GREEN_FROM_BLUE as i64,
    LUMA_GAIN as i64 * CROSSED,
);

/// Blue chroma's from red and green; grey has no chroma, so a chroma's terms
/// sum to nought, which gives its third.
#[cfg(any(test, feature = "encode"))]
const BLUE_CHROMA_FROM_RED: i64 = inverted(-(GREEN_FROM_RED as i64), CROSSED);
#[cfg(any(test, feature = "encode"))]
const BLUE_CHROMA_FROM_GREEN: i64 = inverted(-(RED_FROM_RED as i64), CROSSED);
#[cfg(any(test, feature = "encode"))]
const BLUE_CHROMA_FROM_BLUE: i64 = -(BLUE_CHROMA_FROM_RED + BLUE_CHROMA_FROM_GREEN);

/// Red chroma's from green and blue, and so from red.
#[cfg(any(test, feature = "encode"))]
const RED_CHROMA_FROM_GREEN: i64 = inverted(-(BLUE_FROM_BLUE as i64), CROSSED);
#[cfg(any(test, feature = "encode"))]
const RED_CHROMA_FROM_BLUE: i64 = inverted(-(GREEN_FROM_BLUE as i64), CROSSED);
#[cfg(any(test, feature = "encode"))]
const RED_CHROMA_FROM_RED: i64 = -(RED_CHROMA_FROM_GREEN + RED_CHROMA_FROM_BLUE);

/// Video range's black, the luma of nought.
#[cfg(any(test, feature = "encode"))]
const BLACK: i64 = 16;

/// The chroma of grey.
#[cfg(any(test, feature = "encode"))]
const GREY_CHROMA: i64 = 128;

/// `numerator` over `denominator`, from the decoder's fixed point to the
/// encoder's, rounded half away from nought.
#[cfg(any(test, feature = "encode"))]
const fn inverted(numerator: i64, denominator: i64) -> i64 {
    let scaled = numerator * (1 << (INVERSE_BITS + FORWARD_BITS));
    let half = denominator / 2;
    if scaled < 0 {
        (scaled - half) / denominator
    } else {
        (scaled + half) / denominator
    }
}

/// The luma sample of an opaque pixel's red, green and blue.
#[cfg(any(test, feature = "encode"))]
pub(super) fn luma([red, green, blue]: [u8; 3]) -> u8 {
    let weighed = LUMA_FROM_RED * i64::from(red)
        + LUMA_FROM_GREEN * i64::from(green)
        + LUMA_FROM_BLUE * i64::from(blue);
    sample(rounded(weighed + (BLACK << FORWARD_BITS), FORWARD_BITS))
}

/// The blue and red chroma samples of four pixels, from the sums of their
/// reds, greens and blues.
#[cfg(any(test, feature = "encode"))]
pub(super) fn chroma([red, green, blue]: [u32; 3]) -> [u8; 2] {
    let bits = FORWARD_BITS + 2;
    let (red, green, blue) = (i64::from(red), i64::from(green), i64::from(blue));
    let at = |[from_red, from_green, from_blue]: [i64; 3]| {
        let weighed = from_red * red + from_green * green + from_blue * blue;
        sample(rounded(weighed + (GREY_CHROMA << bits), bits))
    };
    [
        at([
            BLUE_CHROMA_FROM_RED,
            BLUE_CHROMA_FROM_GREEN,
            BLUE_CHROMA_FROM_BLUE,
        ]),
        at([
            RED_CHROMA_FROM_RED,
            RED_CHROMA_FROM_GREEN,
            RED_CHROMA_FROM_BLUE,
        ]),
    ]
}

/// `value`, at `bits` of fixed point, rounded to the nearest whole.
#[cfg(any(test, feature = "encode"))]
fn rounded(value: i64, bits: u32) -> i64 {
    (value + (1 << (bits - 1))) >> bits
}

/// `value` held to a sample's range.
#[cfg(any(test, feature = "encode"))]
fn sample(value: i64) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

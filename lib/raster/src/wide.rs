//! Wide pixels: a colour held at 8.8 fixed point while it is worked on, red,
//! green and blue each in a sixteen-bit lane of one word, so a mean or a dither
//! bias is one word's arithmetic for all three and the pixel shown is each
//! lane's high byte.
//!
//! A lane and the bias added to it stay below 65 536, and the word's top lane
//! is nought, so no lane ever carries into the next.

use crate::accel;
use crate::color::Pixel;

/// One in each colour lane of a wide pixel.
pub const LANE_ONES: u64 = 0x0000_0001_0001_0001;

/// The pixel a wide sample of nought shows, whatever its bias.
pub const UNLIT: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: u8::MAX,
};

/// Two wide pixels averaged lane by lane, rounding down.
#[must_use]
pub const fn halve(a: u64, b: u64) -> u64 {
    (a & b) + (((a ^ b) & !LANE_ONES) >> 1)
}

/// Write `span`, whose first pixel is surface column `start`, from wide
/// `samples` two pixels apart, the first the one at or before `start`: a pixel
/// on a sample shows it, one between two their [`halve`], past the last the
/// last, each rounded down after its column's bias in `biases`, one a column
/// of the eight the dither repeats over, spread across the lanes.
pub fn widen(span: &mut [Pixel], (start, samples): (u32, &[u64]), biases: &[u64; 8]) {
    (accel::blend().widen)(span, (start, samples), biases);
}

/// [`widen`] one pixel at a time: the reference every candidate matches.
pub(crate) fn widen_portable(
    span: &mut [Pixel],
    (start, samples): (u32, &[u64]),
    biases: &[u64; 8],
) {
    let bias = |x: u32| biases[(x & 7) as usize];
    let mut x = start;
    let mut pixels = span.iter_mut();
    let mut samples = samples.iter().copied();
    let Some(mut here) = samples.next() else {
        return;
    };
    if x % 2 == 1 {
        let next = samples.next().unwrap_or(here);
        if let Some(pixel) = pixels.next() {
            *pixel = shade(halve(here, next), bias(x));
        }
        here = next;
        x += 1;
    }
    let (pairs, rest) = pixels.into_slice().as_chunks_mut::<2>();
    for [on, between] in pairs {
        let next = samples.next().unwrap_or(here);
        *on = shade(here, bias(x));
        *between = shade(halve(here, next), bias(x + 1));
        here = next;
        x += 2;
    }
    if let Some(pixel) = rest.first_mut() {
        *pixel = shade(here, bias(x));
    }
}

/// [`widen`]'s pixel between two samples, when `start` falls on one, written
/// portably, and what is left to widen from the sample after it: what a packed
/// candidate starts from, always on a sample.
#[cfg(any(raster_sse2, raster_neon))]
pub(crate) fn onto_sample<'a, 's>(
    span: &'a mut [Pixel],
    (start, samples): (u32, &'s [u64]),
    biases: &[u64; 8],
) -> (&'a mut [Pixel], u32, &'s [u64]) {
    if start % 2 == 0 || span.is_empty() {
        return (span, start, samples);
    }
    let (pixel, rest) = span.split_at_mut(1);
    widen_portable(pixel, (start, samples), biases);
    let after = samples
        .get(1..)
        .filter(|after| !after.is_empty())
        .unwrap_or(samples);
    (rest, start + 1, after)
}

/// The pixel `lanes` show, rounded at `bias`: each lane's high byte.
fn shade(lanes: u64, bias: u64) -> Pixel {
    let bytes = (lanes + bias).to_le_bytes();
    Pixel {
        r: bytes[1],
        g: bytes[3],
        b: bytes[5],
        a: u8::MAX,
    }
}

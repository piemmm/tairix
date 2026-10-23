//! x86_64 candidates over SSE2.
//!
//! Compiled only where SSE2 is part of the target's own feature set (the build
//! script's `raster_sse2`). A register holds four pixels; each is widened to
//! sixteen-bit lanes, where every term of the blend fits — a product of two
//! bytes plus a bias below one level is at most 65 278, and so is a mix's sum
//! of two, whose weights add to 255 — and `div255` is
//! `(x + 1 + (x >> 8)) >> 8`, exact for every such sum. A pixel therefore
//! rounds exactly as the portable blend rounds it.

use core::arch::x86_64::{
    __m128i, _mm_add_epi16, _mm_add_epi32, _mm_add_epi64, _mm_and_si128, _mm_andnot_si128,
    _mm_castsi128_ps, _mm_cmpeq_epi32, _mm_cvtsi128_si32, _mm_cvtsi32_si128, _mm_cvtsi64_si128,
    _mm_loadu_si128, _mm_movemask_epi8, _mm_movemask_ps, _mm_mul_epu32, _mm_mullo_epi16,
    _mm_or_si128, _mm_packs_epi32, _mm_packus_epi16, _mm_set1_epi16, _mm_set1_epi32,
    _mm_set1_epi64x, _mm_set_epi16, _mm_set_epi32, _mm_set_epi64x, _mm_setzero_si128,
    _mm_shufflehi_epi16, _mm_shufflelo_epi16, _mm_slli_epi64, _mm_srl_epi64, _mm_srli_epi16,
    _mm_srli_epi64, _mm_srli_si128, _mm_storeu_si128, _mm_sub_epi16, _mm_sub_epi32,
    _mm_unpackhi_epi16, _mm_unpackhi_epi32, _mm_unpackhi_epi64, _mm_unpackhi_epi8,
    _mm_unpacklo_epi16, _mm_unpacklo_epi32, _mm_unpacklo_epi64, _mm_unpacklo_epi8, _mm_xor_si128,
};
use core::ops::Range;

use crate::blur::{
    blur_span, line_portable, mean_portable, LaneDivisor, PixelSum, Reciprocal, Window,
};
use crate::color::{blend_solid_portable, blend_span_portable, mix_span_portable, Color, Pixel};
use crate::dither::DitherRow;
use crate::wide::{onto_sample, widen_portable, LANE_ONES};

/// The blend candidate's log and pin name.
pub(super) const BLEND_NAME: &str = "raster-blend-sse2";

/// The blur candidate's log and pin name.
pub(super) const BLUR_NAME: &str = "raster-blur-sse2";

/// The scan candidate's log and pin name.
pub(super) const SCAN_NAME: &str = "raster-scan-sse2";

/// Pixels the scan tests between branches: four registers.
const SCAN_STEP: usize = 16;

const TILE: usize = DitherRow::PERIOD;

/// A pixel's alpha byte within its little-endian lane.
const ALPHA: i32 = i32::from_le_bytes([0, 0, 0, 0xFF]);

/// [`super::SpanBlend`] over SSE2.
pub(super) fn blend_span(dst: &mut [Pixel], src: &[Pixel], factor: u8, tile: &[u32; TILE]) {
    let paired = dst.len().min(src.len());
    let (Some(dst), Some(src)) = (dst.get_mut(..paired), src.get(..paired)) else {
        return;
    };
    let (dst_tiles, dst_rest) = dst.as_chunks_mut::<TILE>();
    let (src_tiles, src_rest) = src.as_chunks::<TILE>();
    // SAFETY: `span_tiles` needs SSE2, which is part of every x86_64 target and
    // of the one this module is compiled for alone, so every core this runs on
    // has it, whoever calls it.
    unsafe { span_tiles(dst_tiles, src_tiles, factor, tile) };
    blend_span_portable(dst_rest, src_rest, factor, tile);
}

/// [`super::SolidBlend`] over SSE2.
pub(super) fn blend_solid(dst: &mut [Pixel], src: Pixel, factor: u8, tile: &[u32; TILE]) {
    if src.a == 0 {
        return;
    }
    let (tiles, rest) = dst.as_chunks_mut::<TILE>();
    // SAFETY: as `blend_span`'s.
    unsafe { solid_tiles(tiles, src, factor, tile) };
    blend_solid_portable(rest, src, factor, tile);
}

/// [`super::SpanMix`] over SSE2.
pub(super) fn mix_span(
    dst: &mut [Pixel],
    from: &[Pixel],
    to: &[Pixel],
    weight: u8,
    tile: &[u32; TILE],
) {
    let paired = dst.len().min(from.len()).min(to.len());
    let (Some(dst), Some(from), Some(to)) =
        (dst.get_mut(..paired), from.get(..paired), to.get(..paired))
    else {
        return;
    };
    let (dst_tiles, dst_rest) = dst.as_chunks_mut::<TILE>();
    let (from_tiles, from_rest) = from.as_chunks::<TILE>();
    let (to_tiles, to_rest) = to.as_chunks::<TILE>();
    // SAFETY: as `blend_span`'s.
    unsafe { mix_tiles(dst_tiles, from_tiles, to_tiles, weight, tile) };
    mix_span_portable(dst_rest, from_rest, to_rest, weight, tile);
}

/// [`super::SolidCover`] over SSE2.
pub(super) fn cover_span(dst: &mut [Pixel], src: Pixel, coverage: &[u8], tile: &[u32; TILE]) {
    if src.a == 0 {
        return;
    }
    let paired = dst.len().min(coverage.len());
    let (Some(dst), Some(coverage)) = (dst.get_mut(..paired), coverage.get(..paired)) else {
        return;
    };
    let (dst_tiles, dst_rest) = dst.as_chunks_mut::<TILE>();
    let (level_tiles, level_rest) = coverage.as_chunks::<TILE>();
    // SAFETY: as `blend_span`'s.
    unsafe { cover_tiles(dst_tiles, src, level_tiles, tile) };
    super::padded(dst_rest, level_rest, |dst, levels| {
        // SAFETY: as `blend_span`'s.
        unsafe { cover_tiles(dst, src, levels, tile) };
    });
}

/// [`super::SolidWash`] over SSE2.
pub(super) fn wash_weighted(dst: &mut [Pixel], ink: Color, weights: &[u8], tile: &[u32; TILE]) {
    let paired = dst.len().min(weights.len());
    let (Some(dst), Some(weights)) = (dst.get_mut(..paired), weights.get(..paired)) else {
        return;
    };
    let (dst_tiles, dst_rest) = dst.as_chunks_mut::<TILE>();
    let (weight_tiles, weight_rest) = weights.as_chunks::<TILE>();
    // SAFETY: as `blend_span`'s.
    unsafe { wash_tiles(dst_tiles, ink, weight_tiles, tile) };
    super::padded(dst_rest, weight_rest, |dst, weights| {
        // SAFETY: as `blend_span`'s.
        unsafe { wash_tiles(dst, ink, weights, tile) };
    });
}

/// [`widen`](crate::wide::widen) four pixels a step, from two samples and the
/// one after them.
pub(super) fn widen_samples(
    span: &mut [Pixel],
    (start, samples): (u32, &[u64]),
    biases: &[u64; 8],
) {
    let (span, start, samples) = onto_sample(span, (start, samples), biases);
    let (quads, _) = span.as_chunks_mut::<4>();
    // SAFETY: the build enables SSE2 for this target.
    let laid = unsafe { widen_quads(quads, samples, start, biases) };
    let (Some(rest), Some(after)) = (span.get_mut(4 * laid..), samples.get(2 * laid..)) else {
        return;
    };
    let start = start.saturating_add(u32::try_from(4 * laid).unwrap_or(u32::MAX));
    widen_portable(rest, (start, after), biases);
}

/// Lay `quads` from `samples`, the first quad's first pixel on sample `0` at
/// even surface column `start`, while a sample remains past each pair: the
/// pixels on two samples and the two between them and the next, rounded at
/// their columns' biases. Answers how many quads it laid.
///
/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn widen_quads(
    quads: &mut [[Pixel; 4]],
    samples: &[u64],
    start: u32,
    biases: &[u64; 8],
) -> usize {
    let ones = _mm_set1_epi64x(LANE_ONES.cast_signed());
    let alpha = _mm_set1_epi32(0xFF00_0000_u32.cast_signed());
    let bias = |x: u32| biases[(x & 7) as usize].cast_signed();
    // The biases repeat every eight pixels: two quads.
    let phases = [0u32, 4].map(|step| {
        let x = start.wrapping_add(step);
        (
            _mm_set_epi64x(bias(x.wrapping_add(2)), bias(x)),
            _mm_set_epi64x(bias(x.wrapping_add(3)), bias(x.wrapping_add(1))),
        )
    });
    let mut laid = 0;
    for (quad, window) in quads.iter_mut().zip(samples.windows(3).step_by(2)) {
        let (on_bias, between_bias) = phases[laid & 1];
        // SAFETY: `window` holds three samples, so each load reads two inside
        // it; the loads and the store are the unaligned forms, and the store
        // writes the quad's sixteen bytes.
        #[allow(clippy::cast_ptr_alignment)] // The loads and the store are the unaligned forms.
        let (here, next) = unsafe {
            (
                _mm_loadu_si128(window.as_ptr().cast::<__m128i>()),
                _mm_loadu_si128(window.as_ptr().add(1).cast::<__m128i>()),
            )
        };
        let mean = _mm_add_epi64(
            _mm_and_si128(here, next),
            _mm_srli_epi64(_mm_andnot_si128(ones, _mm_xor_si128(here, next)), 1),
        );
        let on = _mm_add_epi64(here, on_bias);
        let between = _mm_add_epi64(mean, between_bias);
        // Each sixteen-bit lane's high byte, the four pixels in column order.
        let first = _mm_srli_epi16(_mm_unpacklo_epi64(on, between), 8);
        let second = _mm_srli_epi16(_mm_unpackhi_epi64(on, between), 8);
        let pixels = _mm_or_si128(_mm_packus_epi16(first, second), alpha);
        #[allow(clippy::cast_ptr_alignment)] // As above.
        // SAFETY: as above.
        unsafe {
            _mm_storeu_si128(quad.as_mut_ptr().cast::<__m128i>(), pixels);
        }
        laid += 1;
    }
    laid
}

/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn span_tiles(
    dst: &mut [[Pixel; TILE]],
    src: &[[Pixel; TILE]],
    factor: u8,
    tile: &[u32; TILE],
) {
    let biases = biases(tile);
    let scale = (factor != 255).then(|| _mm_set1_epi16(i16::from(factor)));
    for (dst, src) in dst.iter_mut().zip(src) {
        // A tile is eight four-byte pixels, so two sixteen-byte halves.
        #[allow(clippy::cast_ptr_alignment)] // The loads and stores are the unaligned forms.
        let (to, from) = (
            dst.as_mut_ptr().cast::<__m128i>(),
            src.as_ptr().cast::<__m128i>(),
        );
        for (half, &[low, high]) in biases.as_chunks::<2>().0.iter().enumerate() {
            // SAFETY: `half` is 0 or 1, so each address is one of the tile's
            // two sixteen-byte halves, readable through `src` and writable
            // through `dst`, which do not overlap.
            let (s, d) = unsafe {
                (
                    _mm_loadu_si128(from.add(half)),
                    _mm_loadu_si128(to.add(half)),
                )
            };
            let skip =
                _mm_cmpeq_epi32(_mm_and_si128(s, _mm_set1_epi32(ALPHA)), _mm_setzero_si128());
            let blended = over4(source4(s, scale, low, high), d, low, high);
            let out = _mm_or_si128(_mm_and_si128(skip, d), _mm_andnot_si128(skip, blended));
            // SAFETY: as the load from the same address above.
            unsafe { _mm_storeu_si128(to.add(half), out) };
        }
    }
}

/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn solid_tiles(dst: &mut [[Pixel; TILE]], src: Pixel, factor: u8, tile: &[u32; TILE]) {
    let [b0, b1, b2, b3] = biases(tile);
    let scale = (factor != 255).then(|| _mm_set1_epi16(i16::from(factor)));
    let s = _mm_set1_epi32(i32::from_le_bytes([src.r, src.g, src.b, src.a]));
    // Every tile lays the same colour at the same biases, so it is scaled once.
    let halves = [
        ((b0, b1), source4(s, scale, b0, b1)),
        ((b2, b3), source4(s, scale, b2, b3)),
    ];
    for dst in dst.iter_mut() {
        #[allow(clippy::cast_ptr_alignment)] // The loads and stores are the unaligned forms.
        let to = dst.as_mut_ptr().cast::<__m128i>();
        for (half, &((low, high), source)) in halves.iter().enumerate() {
            // SAFETY: `half` is 0 or 1, so the address is one of the tile's two
            // sixteen-byte halves, readable and writable through `dst`.
            let d = unsafe { _mm_loadu_si128(to.add(half)) };
            let out = over4(source, d, low, high);
            // SAFETY: as the load from the same address above.
            unsafe { _mm_storeu_si128(to.add(half), out) };
        }
    }
}

/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn mix_tiles(
    dst: &mut [[Pixel; TILE]],
    from: &[[Pixel; TILE]],
    to: &[[Pixel; TILE]],
    weight: u8,
    tile: &[u32; TILE],
) {
    let biases = biases(tile);
    let take = _mm_set1_epi16(i16::from(weight));
    let keep = _mm_set1_epi16(i16::from(u8::MAX - weight));
    for ((dst, from), to) in dst.iter_mut().zip(from).zip(to) {
        #[allow(clippy::cast_ptr_alignment)] // The loads and stores are the unaligned forms.
        let (out, from, to) = (
            dst.as_mut_ptr().cast::<__m128i>(),
            from.as_ptr().cast::<__m128i>(),
            to.as_ptr().cast::<__m128i>(),
        );
        for (half, &[low, high]) in biases.as_chunks::<2>().0.iter().enumerate() {
            // SAFETY: `half` is 0 or 1, so each address is one of the tiles'
            // two sixteen-byte halves, readable through `from` and `to` and
            // writable through `dst`, which does not overlap either.
            let (f, t) = unsafe {
                (
                    _mm_loadu_si128(from.add(half)),
                    _mm_loadu_si128(to.add(half)),
                )
            };
            let zero = _mm_setzero_si128();
            let mixed = _mm_packus_epi16(
                mix2(
                    _mm_unpacklo_epi8(f, zero),
                    _mm_unpacklo_epi8(t, zero),
                    (keep, take),
                    low,
                ),
                mix2(
                    _mm_unpackhi_epi8(f, zero),
                    _mm_unpackhi_epi8(t, zero),
                    (keep, take),
                    high,
                ),
            );
            // SAFETY: as the loads from the same tile above.
            unsafe { _mm_storeu_si128(out.add(half), mixed) };
        }
    }
}

/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn cover_tiles(
    dst: &mut [[Pixel; TILE]],
    src: Pixel,
    coverage: &[[u8; TILE]],
    tile: &[u32; TILE],
) {
    let biases = biases(tile);
    let colour = _mm_unpacklo_epi8(
        _mm_set1_epi32(i32::from_le_bytes([src.r, src.g, src.b, src.a])),
        _mm_setzero_si128(),
    );
    for (dst, levels) in dst.iter_mut().zip(coverage) {
        let Some(factors) = spread(*levels) else {
            continue;
        };
        lay_halves(dst, biases, factors, |d, (low, high), (f_low, f_high)| {
            let source = (
                div255(_mm_add_epi16(_mm_mullo_epi16(colour, f_low), low)),
                div255(_mm_add_epi16(_mm_mullo_epi16(colour, f_high), high)),
            );
            over4(source, d, low, high)
        });
    }
}

/// # Safety
///
/// The CPU must implement SSE2.
#[target_feature(enable = "sse2")]
unsafe fn wash_tiles(
    dst: &mut [[Pixel; TILE]],
    ink: Color,
    weights: &[[u8; TILE]],
    tile: &[u32; TILE],
) {
    let biases = biases(tile);
    let zero = _mm_setzero_si128();
    let full = _mm_set1_epi16(255);
    let colour = _mm_unpacklo_epi8(
        _mm_set1_epi32(i32::from_le_bytes([ink.r, ink.g, ink.b, u8::MAX])),
        zero,
    );
    for (dst, levels) in dst.iter_mut().zip(weights) {
        let Some(takes) = spread(*levels) else {
            continue;
        };
        lay_halves(dst, biases, takes, |d, (low, high), (t_low, t_high)| {
            _mm_packus_epi16(
                mix2(
                    _mm_unpacklo_epi8(d, zero),
                    colour,
                    (_mm_sub_epi16(full, t_low), t_low),
                    low,
                ),
                mix2(
                    _mm_unpackhi_epi8(d, zero),
                    colour,
                    (_mm_sub_epi16(full, t_high), t_high),
                    high,
                ),
            )
        });
    }
}

/// Each of a tile's eight `levels` in all four lanes of its pixel, two pixels
/// a register, or `None` when every one is nought.
#[inline]
#[target_feature(enable = "sse2")]
fn spread(levels: [u8; TILE]) -> Option<[__m128i; 4]> {
    let word = i64::from_le_bytes(levels);
    if word == 0 {
        return None;
    }
    let levels = _mm_unpacklo_epi8(_mm_cvtsi64_si128(word), _mm_setzero_si128());
    let (first, last) = (
        _mm_unpacklo_epi16(levels, levels),
        _mm_unpackhi_epi16(levels, levels),
    );
    Some([
        _mm_unpacklo_epi32(first, first),
        _mm_unpackhi_epi32(first, first),
        _mm_unpacklo_epi32(last, last),
        _mm_unpackhi_epi32(last, last),
    ])
}

/// Lay each half of `dst`, four pixels, through `lay`: its pixels, the biases
/// of its first and last two, and their levels as [`spread`] answers them.
#[inline]
#[target_feature(enable = "sse2")]
fn lay_halves(
    dst: &mut [Pixel; TILE],
    [b0, b1, b2, b3]: [__m128i; 4],
    levels: [__m128i; 4],
    lay: impl Fn(__m128i, (__m128i, __m128i), (__m128i, __m128i)) -> __m128i,
) {
    #[allow(clippy::cast_ptr_alignment)] // The loads and stores are the unaligned forms.
    let to = dst.as_mut_ptr().cast::<__m128i>();
    let halves = [
        ((b0, b1), (levels[0], levels[1])),
        ((b2, b3), (levels[2], levels[3])),
    ];
    for (half, (biases, levels)) in halves.into_iter().enumerate() {
        // SAFETY: `half` is 0 or 1, so the address is one of the tile's two
        // sixteen-byte halves, readable and writable through `dst`.
        let d = unsafe { _mm_loadu_si128(to.add(half)) };
        let out = lay(d, biases, levels);
        // SAFETY: as the load from the same address above.
        unsafe { _mm_storeu_si128(to.add(half), out) };
    }
}

/// Two widened pixels `weight`/255 of the way from `f` to `t`, `(keep,
/// take)` being `255 - weight` and `weight`: `(f * keep + t * take) / 255`,
/// rounded at `bias`.
#[inline]
#[target_feature(enable = "sse2")]
fn mix2(f: __m128i, t: __m128i, (keep, take): (__m128i, __m128i), bias: __m128i) -> __m128i {
    let weighted = _mm_add_epi16(_mm_mullo_epi16(f, keep), _mm_mullo_epi16(t, take));
    div255(_mm_add_epi16(weighted, bias))
}

/// The tile's eight biases in the registers four pixels' channels widen into:
/// pixels 0–1, 2–3, 4–5 and 6–7, each pixel's bias in all four of its lanes.
#[inline]
#[target_feature(enable = "sse2")]
fn biases(tile: &[u32; TILE]) -> [__m128i; 4] {
    let lane = |index: usize| {
        tile.get(index)
            .and_then(|&bias| i16::try_from(bias).ok())
            .unwrap_or(0)
    };
    let pair = |first: usize| {
        let (a, b) = (lane(first), lane(first + 1));
        _mm_set_epi16(b, b, b, b, a, a, a, a)
    };
    [pair(0), pair(2), pair(4), pair(6)]
}

/// Four source pixels `s` widened to sixteen-bit lanes, two a register, and
/// scaled by `scale` when there is one; `low` and `high` are the biases of the
/// first and last two.
#[inline]
#[target_feature(enable = "sse2")]
fn source4(s: __m128i, scale: Option<__m128i>, low: __m128i, high: __m128i) -> (__m128i, __m128i) {
    let zero = _mm_setzero_si128();
    let widened = (_mm_unpacklo_epi8(s, zero), _mm_unpackhi_epi8(s, zero));
    match scale {
        Some(factor) => (
            div255(_mm_add_epi16(_mm_mullo_epi16(widened.0, factor), low)),
            div255(_mm_add_epi16(_mm_mullo_epi16(widened.1, factor), high)),
        ),
        None => widened,
    }
}

/// Four widened source pixels over four destination pixels `d`, at the biases
/// [`source4`] took.
#[inline]
#[target_feature(enable = "sse2")]
fn over4((s_low, s_high): (__m128i, __m128i), d: __m128i, low: __m128i, high: __m128i) -> __m128i {
    let zero = _mm_setzero_si128();
    let blended_low = over(s_low, _mm_unpacklo_epi8(d, zero), low);
    let blended_high = over(s_high, _mm_unpackhi_epi8(d, zero), high);
    // Each lane is at most `255 + 255`, and the pack saturates it to a byte:
    // the saturating add of the portable blend.
    _mm_packus_epi16(blended_low, blended_high)
}

/// Two widened pixels `s` over two widened pixels `d`: `s + d * (255 - s.a)
/// / 255`, rounded at `bias`.
#[inline]
#[target_feature(enable = "sse2")]
fn over(s: __m128i, d: __m128i, bias: __m128i) -> __m128i {
    // Each pixel's alpha, its lane 3, into all four of its lanes.
    let alpha = _mm_shufflehi_epi16::<0xFF>(_mm_shufflelo_epi16::<0xFF>(s));
    let keep = _mm_sub_epi16(_mm_set1_epi16(255), alpha);
    _mm_add_epi16(s, div255(_mm_add_epi16(_mm_mullo_epi16(d, keep), bias)))
}

/// `x / 255`, exact for every lane up to 65 278.
#[inline]
#[target_feature(enable = "sse2")]
fn div255(x: __m128i) -> __m128i {
    let summed = _mm_add_epi16(_mm_add_epi16(x, _mm_srli_epi16::<8>(x)), _mm_set1_epi16(1));
    _mm_srli_epi16::<8>(summed)
}

/// A window's four channel sums in one register's 32-bit lanes.
///
/// It runs [`blur_span`]'s own walk, so the horizontal pass has one
/// definition whatever holds its sums. A window's sums never leave
/// `0..=255 * count`, so these wrapping lanes give exactly what the portable
/// window's saturating ones do.
///
/// Its methods call the module's SSE2 helpers from inside a trait method,
/// which cannot carry `#[target_feature]`; SSE2 is part of x86_64 itself and
/// of the target this module is only compiled for, so every core that runs
/// them has it.
#[derive(Copy, Clone)]
struct Lanes(__m128i);

impl Default for Lanes {
    #[inline]
    fn default() -> Self {
        // SAFETY: SSE2 is part of x86_64 and of this module's target.
        Self(unsafe { _mm_setzero_si128() })
    }
}

impl Window<Pixel> for Lanes {
    #[inline]
    fn add_many(&mut self, pixel: Pixel, times: usize) {
        // SAFETY: SSE2 is part of x86_64 and of this module's target.
        let mut sum = unsafe { sum_of(self.0) };
        sum.add_many(pixel, times);
        // SAFETY: as above.
        self.0 = unsafe { lanes_of(sum) };
    }

    #[inline]
    fn add(&mut self, pixel: Pixel) {
        // SAFETY: SSE2 is part of x86_64 and of this module's target.
        self.0 = unsafe { _mm_add_epi32(self.0, widen(pixel)) };
    }

    #[inline]
    fn sub(&mut self, pixel: Pixel) {
        // SAFETY: SSE2 is part of x86_64 and of this module's target.
        self.0 = unsafe { _mm_sub_epi32(self.0, widen(pixel)) };
    }

    #[inline]
    fn mean(self, recip: Reciprocal) -> Pixel {
        // SAFETY: SSE2 is part of x86_64 and of this module's target.
        unsafe {
            match recip.lanes() {
                Some(divisor) => narrow(mean_lanes(self.0, divisor)),
                None => sum_of(self.0).mean(recip),
            }
        }
    }
}

#[inline]
#[target_feature(enable = "sse2")]
fn sum_of(lanes: __m128i) -> PixelSum {
    let lane = |v: __m128i| _mm_cvtsi128_si32(v).cast_unsigned();
    PixelSum {
        r: lane(lanes),
        g: lane(_mm_srli_si128::<4>(lanes)),
        b: lane(_mm_srli_si128::<8>(lanes)),
        a: lane(_mm_srli_si128::<12>(lanes)),
    }
}

#[inline]
#[target_feature(enable = "sse2")]
fn lanes_of(sum: PixelSum) -> __m128i {
    _mm_set_epi32(
        sum.a.cast_signed(),
        sum.b.cast_signed(),
        sum.g.cast_signed(),
        sum.r.cast_signed(),
    )
}

/// One pixel's channels in four 32-bit lanes.
#[inline]
#[target_feature(enable = "sse2")]
fn widen(pixel: Pixel) -> __m128i {
    let zero = _mm_setzero_si128();
    let bytes = _mm_cvtsi32_si128(i32::from_le_bytes([pixel.r, pixel.g, pixel.b, pixel.a]));
    _mm_unpacklo_epi16(_mm_unpacklo_epi8(bytes, zero), zero)
}

/// Each 32-bit lane's rounded mean over `divisor`, in the same lane.
#[inline]
#[target_feature(enable = "sse2")]
fn mean_lanes(sums: __m128i, divisor: LaneDivisor) -> __m128i {
    let n = _mm_add_epi32(sums, _mm_set1_epi32(divisor.half.cast_signed()));
    let m = _mm_set1_epi32(divisor.m.cast_signed());
    let shift = _mm_cvtsi32_si128(divisor.shift.cast_signed());
    // `PMULUDQ` multiplies the even lanes into 64 bits; the odd ones are
    // shifted down into their places for a second.
    let even = _mm_srl_epi64(_mm_mul_epu32(n, m), shift);
    let odd = _mm_srl_epi64(_mm_mul_epu32(_mm_srli_epi64::<32>(n), m), shift);
    _mm_or_si128(even, _mm_slli_epi64::<32>(odd))
}

/// Four 32-bit lanes, each at most 255, as one pixel.
#[inline]
#[target_feature(enable = "sse2")]
fn narrow(lanes: __m128i) -> Pixel {
    let packed = _mm_packus_epi16(_mm_packs_epi32(lanes, lanes), _mm_setzero_si128());
    let [r, g, b, a] = _mm_cvtsi128_si32(packed).to_le_bytes();
    Pixel { r, g, b, a }
}

/// [`super::LineBlur`] over SSE2.
pub(super) fn line(
    src: &[Pixel],
    dst: &mut [Pixel],
    len: usize,
    radius: usize,
    recip: Reciprocal,
    out: Range<usize>,
) {
    if recip.lanes().is_none() {
        return line_portable(src, dst, len, radius, recip, out);
    }
    blur_span::<Pixel, Lanes>(src, dst, 1, len, radius, recip, out);
}

/// Four pixels' channels, each widened into its own register of 32-bit lanes.
#[inline]
#[target_feature(enable = "sse2")]
fn widen4(pixels: __m128i) -> [__m128i; 4] {
    let zero = _mm_setzero_si128();
    let (low, high) = (
        _mm_unpacklo_epi8(pixels, zero),
        _mm_unpackhi_epi8(pixels, zero),
    );
    [
        _mm_unpacklo_epi16(low, zero),
        _mm_unpackhi_epi16(low, zero),
        _mm_unpacklo_epi16(high, zero),
        _mm_unpackhi_epi16(high, zero),
    ]
}

/// [`super::RowAdd`] over SSE2.
pub(super) fn add(sums: &mut [PixelSum], row: &[Pixel]) {
    let paired = sums.len().min(row.len());
    let (Some(sums), Some(row)) = (sums.get_mut(..paired), row.get(..paired)) else {
        return;
    };
    let (sum_groups, sum_rest) = sums.as_chunks_mut::<4>();
    let (row_groups, row_rest) = row.as_chunks::<4>();
    // SAFETY: as `blend_span`'s.
    unsafe { add_groups(sum_groups, row_groups) };
    crate::blur::add_portable(sum_rest, row_rest);
}

#[inline]
#[target_feature(enable = "sse2")]
fn add_groups(sums: &mut [[PixelSum; 4]], row: &[[Pixel; 4]]) {
    for (sums, pixels) in sums.iter_mut().zip(row) {
        for (sum, lanes) in sums.iter_mut().zip(widen4(load_pixels(pixels))) {
            store_sum(sum, _mm_add_epi32(load_sum(sum), lanes));
        }
    }
}

/// [`super::RowSlide`] over SSE2.
pub(super) fn slide(sums: &mut [PixelSum], entering: &[Pixel], leaving: &[Pixel]) {
    let paired = sums.len().min(entering.len()).min(leaving.len());
    let (Some(sums), Some(entering), Some(leaving)) = (
        sums.get_mut(..paired),
        entering.get(..paired),
        leaving.get(..paired),
    ) else {
        return;
    };
    let (sum_groups, sum_rest) = sums.as_chunks_mut::<4>();
    let (enter_groups, enter_rest) = entering.as_chunks::<4>();
    let (leave_groups, leave_rest) = leaving.as_chunks::<4>();
    // SAFETY: as `add`'s.
    unsafe { slide_groups(sum_groups, enter_groups, leave_groups) };
    crate::blur::slide_portable(sum_rest, enter_rest, leave_rest);
}

#[inline]
#[target_feature(enable = "sse2")]
fn slide_groups(sums: &mut [[PixelSum; 4]], entering: &[[Pixel; 4]], leaving: &[[Pixel; 4]]) {
    for ((sums, enter), leave) in sums.iter_mut().zip(entering).zip(leaving) {
        let (added, removed) = (widen4(load_pixels(enter)), widen4(load_pixels(leave)));
        for ((sum, add), sub) in sums.iter_mut().zip(added).zip(removed) {
            store_sum(sum, _mm_sub_epi32(_mm_add_epi32(load_sum(sum), add), sub));
        }
    }
}

/// [`super::RowMean`] over SSE2.
pub(super) fn mean(out: &mut [Pixel], sums: &[PixelSum], recip: Reciprocal) {
    let Some(divisor) = recip.lanes() else {
        return mean_portable(out, sums, recip);
    };
    let paired = out.len().min(sums.len());
    let (Some(out), Some(sums)) = (out.get_mut(..paired), sums.get(..paired)) else {
        return;
    };
    // SAFETY: as `add`'s.
    unsafe { mean_all(out, sums, divisor) };
}

#[inline]
#[target_feature(enable = "sse2")]
fn mean_all(out: &mut [Pixel], sums: &[PixelSum], divisor: LaneDivisor) {
    let (out_groups, out_rest) = out.as_chunks_mut::<4>();
    let (sum_groups, sum_rest) = sums.as_chunks::<4>();
    for (out, sums) in out_groups.iter_mut().zip(sum_groups) {
        let [a, b, c, d] = sums
            .each_ref()
            .map(|sum| mean_lanes(load_sum(sum), divisor));
        // Each lane is at most 255, so both packs are exact.
        let packed = _mm_packus_epi16(_mm_packs_epi32(a, b), _mm_packs_epi32(c, d));
        #[allow(clippy::cast_ptr_alignment)] // The store is the unaligned form.
        let to = out.as_mut_ptr().cast::<__m128i>();
        // SAFETY: four `#[repr(C)]` four-byte pixels are the sixteen writable
        // bytes the unaligned store writes.
        unsafe { _mm_storeu_si128(to, packed) };
    }
    for (slot, sum) in out_rest.iter_mut().zip(sum_rest) {
        *slot = narrow(mean_lanes(load_sum(sum), divisor));
    }
}

/// Four pixels as one register.
#[inline]
#[target_feature(enable = "sse2")]
fn load_pixels(pixels: &[Pixel; 4]) -> __m128i {
    #[allow(clippy::cast_ptr_alignment)] // The load is the unaligned form.
    let from = pixels.as_ptr().cast::<__m128i>();
    // SAFETY: four `#[repr(C)]` four-byte pixels are the sixteen readable
    // bytes the unaligned load reads.
    unsafe { _mm_loadu_si128(from) }
}

/// One window's sums as one register.
#[inline]
#[target_feature(enable = "sse2")]
fn load_sum(sum: &PixelSum) -> __m128i {
    #[allow(clippy::cast_ptr_alignment)] // The load is the unaligned form.
    let from = core::ptr::from_ref(sum).cast::<__m128i>();
    // SAFETY: `PixelSum` is `#[repr(C)]` four `u32`s, the sixteen readable
    // bytes the unaligned load reads.
    unsafe { _mm_loadu_si128(from) }
}

/// Write one register back as one window's sums.
#[inline]
#[target_feature(enable = "sse2")]
fn store_sum(sum: &mut PixelSum, lanes: __m128i) {
    #[allow(clippy::cast_ptr_alignment)] // The store is the unaligned form.
    let to = core::ptr::from_mut(sum).cast::<__m128i>();
    // SAFETY: `PixelSum` is `#[repr(C)]` four `u32`s, the sixteen writable
    // bytes the unaligned store writes, and `sum` is borrowed exclusively.
    unsafe { _mm_storeu_si128(to, lanes) };
}

/// [`super::LeadingRun`] for full alpha over SSE2.
pub(super) fn leading_opaque(span: &[Pixel]) -> usize {
    // SAFETY: as `blend_span`'s.
    unsafe { leading_run(span, true) }
}

/// [`super::LeadingRun`] for alpha short of full over SSE2.
pub(super) fn leading_translucent(span: &[Pixel]) -> usize {
    // SAFETY: as `leading_opaque`'s.
    unsafe { leading_run(span, false) }
}

/// How many pixels lead `span` whose alpha is full when `opaque`, or short of
/// it otherwise: four registers at a time while the run holds, then one at a
/// time into the step that breaks it, and the tail a pixel at a time.
#[inline]
#[target_feature(enable = "sse2")]
fn leading_run(span: &[Pixel], opaque: bool) -> usize {
    // A lane is all ones where its pixel continues the run.
    let flip = if opaque {
        _mm_setzero_si128()
    } else {
        _mm_set1_epi32(-1)
    };
    let alpha = _mm_set1_epi32(ALPHA);
    let continues = |quad: &[Pixel; 4]| {
        let full = _mm_cmpeq_epi32(_mm_and_si128(load_pixels(quad), alpha), alpha);
        _mm_xor_si128(full, flip)
    };
    let (steps, _) = span.as_chunks::<SCAN_STEP>();
    let held = steps
        .iter()
        .take_while(|step| {
            let all = step
                .as_chunks::<4>()
                .0
                .iter()
                .fold(_mm_set1_epi32(-1), |all, quad| {
                    _mm_and_si128(all, continues(quad))
                });
            _mm_movemask_epi8(all) == 0xFFFF
        })
        .count()
        * SCAN_STEP;
    let (quads, tail) = span.get(held..).unwrap_or_default().as_chunks::<4>();
    let mut at = held;
    for quad in quads {
        let lanes = _mm_movemask_ps(_mm_castsi128_ps(continues(quad))).cast_unsigned();
        if lanes != 0b1111 {
            return at + (!lanes).trailing_zeros() as usize;
        }
        at += 4;
    }
    at + tail
        .iter()
        .take_while(|pixel| (pixel.a == u8::MAX) == opaque)
        .count()
}

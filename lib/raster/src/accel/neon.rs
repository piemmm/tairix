//! aarch64 candidates over Advanced SIMD (NEON).
//!
//! Compiled only where NEON is part of the target's own feature set (the build
//! script's `raster_neon`). `LD4` deinterleaves sixteen pixels into one
//! register per channel, so a blend works a channel at a time: widened to
//! sixteen-bit lanes, where every term fits, with `div255` the same
//! `(x + 1 + (x >> 8)) >> 8` the portable blend's rounding is exactly.
//!
//! Memory is reached only through the structure loads and stores (`LD4`,
//! `ST4`), which need no more than their element's alignment: under the
//! target's strict alignment a plain vector load of a pixel or a sum is split
//! into its bytes, which costs more than the arithmetic it feeds.

use core::arch::aarch64::{
    uint16x8_t, uint32x4_t, uint32x4x4_t, uint8x16_t, uint8x16x4_t, uint8x8_t, vaddhn_u16,
    vaddq_u16, vaddq_u32, vbslq_u8, vceqq_u8, vcombine_u16, vcombine_u32, vcombine_u8, vcreate_u16,
    vcreate_u32, vdup_n_u32, vdup_n_u8, vdupq_n_s64, vdupq_n_u16, vdupq_n_u32, vdupq_n_u8,
    veorq_u8, vget_lane_u32, vget_lane_u64, vget_low_u16, vget_low_u32, vget_low_u8,
    vgetq_lane_u32, vld4q_u32, vld4q_u8, vminvq_u8, vmovl_high_u16, vmovl_high_u8, vmovl_u16,
    vmovl_u8, vmull_high_u32, vmull_high_u8, vmull_u32, vmull_u8, vmvnq_u8, vqaddq_u8, vqmovn_u16,
    vqmovn_u32, vreinterpret_u32_u8, vreinterpret_u64_u8, vreinterpret_u8_u32,
    vreinterpretq_u16_u8, vreinterpretq_u32_u64, vshlq_u64, vshrn_n_u16, vsraq_n_u16, vst4q_u32,
    vst4q_u8, vsubq_u32, vuzp1q_u32,
};
use core::ops::Range;

use crate::blur::{
    blur_span, line_portable, mean_portable, LaneDivisor, PixelSum, Reciprocal, Window,
};
use crate::color::{blend_solid_portable, blend_span_portable, Pixel};
use crate::dither::DitherRow;

/// The blend candidate's log and pin name.
pub(super) const BLEND_NAME: &str = "raster-blend-neon";

/// The blur candidate's log and pin name.
pub(super) const BLUR_NAME: &str = "raster-blur-neon";

/// The scan candidate's log and pin name.
pub(super) const SCAN_NAME: &str = "raster-scan-neon";

const TILE: usize = DitherRow::PERIOD;

/// Pixels one `LD4` takes: two dither tiles, so every lane keeps one bias.
const STEP: usize = 16;

/// [`super::SpanBlend`] over NEON.
pub(super) fn blend_span(dst: &mut [Pixel], src: &[Pixel], factor: u8, tile: &[u32; TILE]) {
    let paired = dst.len().min(src.len());
    let (Some(dst), Some(src)) = (dst.get_mut(..paired), src.get(..paired)) else {
        return;
    };
    let (dst_steps, dst_rest) = dst.as_chunks_mut::<STEP>();
    let (src_steps, src_rest) = src.as_chunks::<STEP>();
    // SAFETY: `span_steps` needs NEON, which is part of the aarch64 target
    // this module is compiled for alone, so every core this runs on has it,
    // whoever calls it.
    unsafe { span_steps(dst_steps, src_steps, factor, tile) };
    blend_span_portable(dst_rest, src_rest, factor, tile);
}

/// [`super::SolidBlend`] over NEON.
pub(super) fn blend_solid(dst: &mut [Pixel], src: Pixel, factor: u8, tile: &[u32; TILE]) {
    if src.a == 0 {
        return;
    }
    let (steps, rest) = dst.as_chunks_mut::<STEP>();
    // SAFETY: as `blend_span`'s.
    unsafe { solid_steps(steps, src, factor, tile) };
    blend_solid_portable(rest, src, factor, tile);
}

#[inline]
#[target_feature(enable = "neon")]
fn span_steps(dst: &mut [[Pixel; STEP]], src: &[[Pixel; STEP]], factor: u8, tile: &[u32; TILE]) {
    let bias = biases(tile);
    for (dst, src) in dst.iter_mut().zip(src) {
        // SAFETY: sixteen `#[repr(C)]` four-byte pixels are the sixty-four
        // bytes `LD4` reads, through `src` and `dst`, which do not overlap.
        let (s, d) = unsafe {
            (
                vld4q_u8(src.as_ptr().cast::<u8>()),
                vld4q_u8(dst.as_ptr().cast::<u8>()),
            )
        };
        let skip = vceqq_u8(s.3, vdupq_n_u8(0));
        let source = if factor == 255 {
            s
        } else {
            scaled(s, factor, bias)
        };
        let out = blend16(source, d, bias);
        let kept = uint8x16x4_t(
            vbslq_u8(skip, d.0, out.0),
            vbslq_u8(skip, d.1, out.1),
            vbslq_u8(skip, d.2, out.2),
            vbslq_u8(skip, d.3, out.3),
        );
        // SAFETY: as the load from the same address above.
        unsafe { vst4q_u8(dst.as_mut_ptr().cast::<u8>(), kept) };
    }
}

#[inline]
#[target_feature(enable = "neon")]
fn solid_steps(dst: &mut [[Pixel; STEP]], src: Pixel, factor: u8, tile: &[u32; TILE]) {
    let bias = biases(tile);
    let colour = uint8x16x4_t(
        vdupq_n_u8(src.r),
        vdupq_n_u8(src.g),
        vdupq_n_u8(src.b),
        vdupq_n_u8(src.a),
    );
    // Every step lays the same colour at the same biases, so it is scaled once.
    let source = if factor == 255 {
        colour
    } else {
        scaled(colour, factor, bias)
    };
    for dst in dst.iter_mut() {
        // SAFETY: sixteen `#[repr(C)]` four-byte pixels are the sixty-four
        // bytes `LD4` reads and `ST4` writes, all through `dst`.
        let d = unsafe { vld4q_u8(dst.as_ptr().cast::<u8>()) };
        let out = blend16(source, d, bias);
        // SAFETY: as the load from the same address above.
        unsafe { vst4q_u8(dst.as_mut_ptr().cast::<u8>(), out) };
    }
}

/// A tile's eight biases, one per sixteen-bit lane: lane `i` is pixel `i` of
/// either half of a step.
#[inline]
#[target_feature(enable = "neon")]
fn biases(tile: &[u32; TILE]) -> uint16x8_t {
    let half = |first: usize| {
        (0..4).fold(0u64, |word, lane| {
            let bias = tile
                .get(first + lane)
                .and_then(|&bias| u16::try_from(bias).ok())
                .unwrap_or(0);
            word | (u64::from(bias) << (16 * lane))
        })
    };
    vcombine_u16(vcreate_u16(half(0)), vcreate_u16(half(4)))
}

/// Every channel of `s` scaled by `factor`/255 at each lane's bias.
#[inline]
#[target_feature(enable = "neon")]
fn scaled(s: uint8x16x4_t, factor: u8, bias: uint16x8_t) -> uint8x16x4_t {
    let scale = |channel: uint8x16_t| {
        let low = vaddq_u16(vmull_u8(vget_low_u8(channel), vdup_n_u8(factor)), bias);
        let high = vaddq_u16(vmull_high_u8(channel, vdupq_n_u8(factor)), bias);
        vcombine_u8(div255(low), div255(high))
    };
    uint8x16x4_t(scale(s.0), scale(s.1), scale(s.2), scale(s.3))
}

/// `s` over `d`, every channel `s + d * (255 - s.a) / 255` at each lane's
/// bias, the add saturating as the portable blend's does.
#[inline]
#[target_feature(enable = "neon")]
fn blend16(s: uint8x16x4_t, d: uint8x16x4_t, bias: uint16x8_t) -> uint8x16x4_t {
    let keep = vmvnq_u8(s.3);
    let over = |s: uint8x16_t, d: uint8x16_t| {
        let low = vaddq_u16(vmull_u8(vget_low_u8(d), vget_low_u8(keep)), bias);
        let high = vaddq_u16(vmull_high_u8(d, keep), bias);
        vqaddq_u8(s, vcombine_u8(div255(low), div255(high)))
    };
    uint8x16x4_t(
        over(s.0, d.0),
        over(s.1, d.1),
        over(s.2, d.2),
        over(s.3, d.3),
    )
}

/// `x / 255` narrowed to bytes, exact for every lane up to 65 278.
#[inline]
#[target_feature(enable = "neon")]
fn div255(x: uint16x8_t) -> uint8x8_t {
    vaddhn_u16(vsraq_n_u16::<8>(x, x), vdupq_n_u16(1))
}

/// A window's four channel sums in one register's 32-bit lanes; the NEON twin
/// of the SSE2 window, for the same reasons, under the same proof.
///
/// Its methods call the module's NEON helpers from inside a trait method,
/// which cannot carry `#[target_feature]`; NEON is part of the target this
/// module is only compiled for, so every core that runs them has it.
#[derive(Copy, Clone)]
struct Lanes(uint32x4_t);

impl Default for Lanes {
    #[inline]
    fn default() -> Self {
        // SAFETY: NEON is part of this module's target.
        Self(unsafe { vdupq_n_u32(0) })
    }
}

impl Window<Pixel> for Lanes {
    #[inline]
    fn add_many(&mut self, pixel: Pixel, times: usize) {
        // SAFETY: NEON is part of this module's target.
        let mut sum = unsafe { sum_of(self.0) };
        sum.add_many(pixel, times);
        // SAFETY: as above.
        self.0 = unsafe { lanes_of(sum) };
    }

    #[inline]
    fn add(&mut self, pixel: Pixel) {
        // SAFETY: NEON is part of this module's target.
        self.0 = unsafe { vaddq_u32(self.0, widen(pixel)) };
    }

    #[inline]
    fn sub(&mut self, pixel: Pixel) {
        // SAFETY: NEON is part of this module's target.
        self.0 = unsafe { vsubq_u32(self.0, widen(pixel)) };
    }

    #[inline]
    fn mean(self, recip: Reciprocal) -> Pixel {
        // SAFETY: NEON is part of this module's target.
        unsafe {
            match recip.lanes() {
                Some(divisor) => narrow(mean_lanes(self.0, divisor)),
                None => sum_of(self.0).mean(recip),
            }
        }
    }
}

#[inline]
#[target_feature(enable = "neon")]
fn sum_of(lanes: uint32x4_t) -> PixelSum {
    PixelSum {
        r: vgetq_lane_u32::<0>(lanes),
        g: vgetq_lane_u32::<1>(lanes),
        b: vgetq_lane_u32::<2>(lanes),
        a: vgetq_lane_u32::<3>(lanes),
    }
}

#[inline]
#[target_feature(enable = "neon")]
fn lanes_of(sum: PixelSum) -> uint32x4_t {
    let pair = |low: u32, high: u32| vcreate_u32(u64::from(low) | (u64::from(high) << 32));
    vcombine_u32(pair(sum.r, sum.g), pair(sum.b, sum.a))
}

/// One pixel's channels in four 32-bit lanes.
#[inline]
#[target_feature(enable = "neon")]
fn widen(pixel: Pixel) -> uint32x4_t {
    let bytes = vreinterpret_u8_u32(vdup_n_u32(u32::from_le_bytes([
        pixel.r, pixel.g, pixel.b, pixel.a,
    ])));
    vmovl_u16(vget_low_u16(vmovl_u8(bytes)))
}

/// Each 32-bit lane's rounded mean over `divisor`, in the same lane.
#[inline]
#[target_feature(enable = "neon")]
fn mean_lanes(sums: uint32x4_t, divisor: LaneDivisor) -> uint32x4_t {
    let n = vaddq_u32(sums, vdupq_n_u32(divisor.half));
    let shift = vdupq_n_s64(-i64::from(divisor.shift));
    let low = vshlq_u64(vmull_u32(vget_low_u32(n), vdup_n_u32(divisor.m)), shift);
    let high = vshlq_u64(vmull_high_u32(n, vdupq_n_u32(divisor.m)), shift);
    vuzp1q_u32(vreinterpretq_u32_u64(low), vreinterpretq_u32_u64(high))
}

/// Four 32-bit lanes, each at most 255, as one pixel.
#[inline]
#[target_feature(enable = "neon")]
fn narrow(lanes: uint32x4_t) -> Pixel {
    let halves = vqmovn_u32(lanes);
    let bytes = vqmovn_u16(vcombine_u16(halves, halves));
    let [r, g, b, a] = vget_lane_u32::<0>(vreinterpret_u32_u8(bytes)).to_le_bytes();
    Pixel { r, g, b, a }
}

/// [`super::LineBlur`] over NEON.
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

/// Sixteen pixels' channels in 32-bit lanes: for each group of four pixels,
/// one register per channel, as [`load_sums`] holds four windows.
#[inline]
#[target_feature(enable = "neon")]
fn widen16(pixels: &[Pixel; STEP]) -> [[uint32x4_t; 4]; 4] {
    // SAFETY: sixteen `#[repr(C)]` four-byte pixels are the sixty-four
    // readable bytes `LD4` reads.
    let planes = unsafe { vld4q_u8(pixels.as_ptr().cast::<u8>()) };
    let widen = |channel: uint8x16_t| {
        let (low, high) = (vmovl_u8(vget_low_u8(channel)), vmovl_high_u8(channel));
        [
            vmovl_u16(vget_low_u16(low)),
            vmovl_high_u16(low),
            vmovl_u16(vget_low_u16(high)),
            vmovl_high_u16(high),
        ]
    };
    let [[r0, r1, r2, r3], [g0, g1, g2, g3], [b0, b1, b2, b3], [a0, a1, a2, a3]] =
        [planes.0, planes.1, planes.2, planes.3].map(widen);
    [
        [r0, g0, b0, a0],
        [r1, g1, b1, a1],
        [r2, g2, b2, a2],
        [r3, g3, b3, a3],
    ]
}

/// Four windows' sums, one register per channel.
#[inline]
#[target_feature(enable = "neon")]
fn load_sums(sums: &[PixelSum; 4]) -> [uint32x4_t; 4] {
    // SAFETY: four `#[repr(C)]` sums of four `u32`s are the sixty-four
    // readable bytes `LD4` reads.
    let lanes = unsafe { vld4q_u32(sums.as_ptr().cast::<u32>()) };
    [lanes.0, lanes.1, lanes.2, lanes.3]
}

/// Write [`load_sums`]' registers back as four windows' sums.
#[inline]
#[target_feature(enable = "neon")]
fn store_sums(sums: &mut [PixelSum; 4], [r, g, b, a]: [uint32x4_t; 4]) {
    // SAFETY: as `load_sums`', writable through the exclusive borrow.
    unsafe { vst4q_u32(sums.as_mut_ptr().cast::<u32>(), uint32x4x4_t(r, g, b, a)) };
}

/// [`super::RowAdd`] over NEON.
pub(super) fn add(sums: &mut [PixelSum], row: &[Pixel]) {
    let paired = sums.len().min(row.len());
    let (Some(sums), Some(row)) = (sums.get_mut(..paired), row.get(..paired)) else {
        return;
    };
    let (sum_steps, sum_rest) = sums.as_chunks_mut::<STEP>();
    let (row_steps, row_rest) = row.as_chunks::<STEP>();
    // SAFETY: as `blend_span`'s.
    unsafe { add_steps(sum_steps, row_steps) };
    crate::blur::add_portable(sum_rest, row_rest);
}

#[inline]
#[target_feature(enable = "neon")]
fn add_steps(sums: &mut [[PixelSum; STEP]], row: &[[Pixel; STEP]]) {
    for (sums, pixels) in sums.iter_mut().zip(row) {
        for (sums, added) in sums.as_chunks_mut::<4>().0.iter_mut().zip(widen16(pixels)) {
            let [r, g, b, a] = load_sums(sums);
            let [dr, dg, db, da] = added;
            store_sums(
                sums,
                [
                    vaddq_u32(r, dr),
                    vaddq_u32(g, dg),
                    vaddq_u32(b, db),
                    vaddq_u32(a, da),
                ],
            );
        }
    }
}

/// [`super::RowSlide`] over NEON.
pub(super) fn slide(sums: &mut [PixelSum], entering: &[Pixel], leaving: &[Pixel]) {
    let paired = sums.len().min(entering.len()).min(leaving.len());
    let (Some(sums), Some(entering), Some(leaving)) = (
        sums.get_mut(..paired),
        entering.get(..paired),
        leaving.get(..paired),
    ) else {
        return;
    };
    let (sum_steps, sum_rest) = sums.as_chunks_mut::<STEP>();
    let (enter_steps, enter_rest) = entering.as_chunks::<STEP>();
    let (leave_steps, leave_rest) = leaving.as_chunks::<STEP>();
    // SAFETY: as `blend_span`'s.
    unsafe { slide_steps(sum_steps, enter_steps, leave_steps) };
    crate::blur::slide_portable(sum_rest, enter_rest, leave_rest);
}

#[inline]
#[target_feature(enable = "neon")]
fn slide_steps(
    sums: &mut [[PixelSum; STEP]],
    entering: &[[Pixel; STEP]],
    leaving: &[[Pixel; STEP]],
) {
    for ((sums, enter), leave) in sums.iter_mut().zip(entering).zip(leaving) {
        let groups = sums
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(widen16(enter))
            .zip(widen16(leave));
        for ((sums, added), removed) in groups {
            let [r, g, b, a] = load_sums(sums);
            let ([ar, ag, ab, aa], [sr, sg, sb, sa]) = (added, removed);
            store_sums(
                sums,
                [
                    vsubq_u32(vaddq_u32(r, ar), sr),
                    vsubq_u32(vaddq_u32(g, ag), sg),
                    vsubq_u32(vaddq_u32(b, ab), sb),
                    vsubq_u32(vaddq_u32(a, aa), sa),
                ],
            );
        }
    }
}

/// [`super::RowMean`] over NEON.
pub(super) fn mean(out: &mut [Pixel], sums: &[PixelSum], recip: Reciprocal) {
    let Some(divisor) = recip.lanes() else {
        return mean_portable(out, sums, recip);
    };
    let paired = out.len().min(sums.len());
    let (Some(out), Some(sums)) = (out.get_mut(..paired), sums.get(..paired)) else {
        return;
    };
    let (out_steps, out_rest) = out.as_chunks_mut::<STEP>();
    let (sum_steps, sum_rest) = sums.as_chunks::<STEP>();
    // SAFETY: as `blend_span`'s.
    unsafe { mean_steps(out_steps, sum_steps, divisor) };
    mean_portable(out_rest, sum_rest, recip);
}

#[inline]
#[target_feature(enable = "neon")]
fn mean_steps(out: &mut [[Pixel; STEP]], sums: &[[PixelSum; STEP]], divisor: LaneDivisor) {
    for (out, sums) in out.iter_mut().zip(sums) {
        let mut means = [[vdupq_n_u32(0); 4]; 4];
        for (means, sums) in means.iter_mut().zip(sums.as_chunks::<4>().0) {
            *means = load_sums(sums).map(|lanes| mean_lanes(lanes, divisor));
        }
        let [[r0, g0, b0, a0], [r1, g1, b1, a1], [r2, g2, b2, a2], [r3, g3, b3, a3]] = means;
        let planes = uint8x16x4_t(
            narrow16([r0, r1, r2, r3]),
            narrow16([g0, g1, g2, g3]),
            narrow16([b0, b1, b2, b3]),
            narrow16([a0, a1, a2, a3]),
        );
        // SAFETY: sixteen `#[repr(C)]` four-byte pixels are the sixty-four
        // writable bytes `ST4` writes.
        unsafe { vst4q_u8(out.as_mut_ptr().cast::<u8>(), planes) };
    }
}

/// Sixteen 32-bit lanes, each at most 255, as one register of bytes: the
/// narrowings are exact.
#[inline]
#[target_feature(enable = "neon")]
fn narrow16([a, b, c, d]: [uint32x4_t; 4]) -> uint8x16_t {
    let low = vqmovn_u16(vcombine_u16(vqmovn_u32(a), vqmovn_u32(b)));
    let high = vqmovn_u16(vcombine_u16(vqmovn_u32(c), vqmovn_u32(d)));
    vcombine_u8(low, high)
}

/// [`super::LeadingRun`] for full alpha over NEON.
pub(super) fn leading_opaque(span: &[Pixel]) -> usize {
    // SAFETY: as `blend_span`'s.
    unsafe { leading_run(span, true) }
}

/// [`super::LeadingRun`] for alpha short of full over NEON.
pub(super) fn leading_translucent(span: &[Pixel]) -> usize {
    // SAFETY: as `leading_opaque`'s.
    unsafe { leading_run(span, false) }
}

/// How many pixels lead `span` whose alpha is full when `opaque`, or short of
/// it otherwise: sixteen alphas at a time, the first that breaks the run
/// found from a mask of four bits a lane, and the tail a pixel at a time.
#[inline]
#[target_feature(enable = "neon")]
fn leading_run(span: &[Pixel], opaque: bool) -> usize {
    let full = vdupq_n_u8(u8::MAX);
    // A lane is all ones where its pixel continues the run.
    let flip = vdupq_n_u8(if opaque { 0 } else { u8::MAX });
    let (steps, tail) = span.as_chunks::<STEP>();
    for (index, step) in steps.iter().enumerate() {
        // SAFETY: sixteen `#[repr(C)]` four-byte pixels are the sixty-four
        // readable bytes `LD4` reads.
        let alphas = unsafe { vld4q_u8(step.as_ptr().cast::<u8>()) }.3;
        let continues = veorq_u8(vceqq_u8(alphas, full), flip);
        if vminvq_u8(continues) != u8::MAX {
            let nibbles = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(
                vreinterpretq_u16_u8(continues),
            )));
            return index * STEP + ((!nibbles).trailing_zeros() / 4) as usize;
        }
    }
    steps.len() * STEP
        + tail
            .iter()
            .take_while(|pixel| (pixel.a == u8::MAX) == opaque)
            .count()
}

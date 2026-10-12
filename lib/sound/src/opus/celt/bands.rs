//! The bands' shapes (RFC 6716, sections 4.3.4 to 4.3.6): each band coded
//! whole or split in time or stereo by a coded angle, its pulses where it has
//! bits for them, and otherwise folded from the bands below it.

use super::alloc::{bits_to_pulses, most_bits, pulses, pulses_to_bits};
use super::pvq::{renormalise, unquantise, MAX_WIDTH};
use super::tables::{BANDS, LOG_N};
use super::{rsqrt, sqrt, BAND_COUNT, SPREAD_AGGRESSIVE};
use alloc::vec::Vec;

use tairix_util::fallible::filled;

use crate::opus::range::{ilog, RangeDecoder, BITRES};
use crate::DecodeError;

const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;

/// The longest frame's coefficients a channel below the last band holds.
const NORM_LEN: usize = 8 * BANDS[BAND_COUNT - 1];

/// The decoder's scratch for one frame's bands, kept across frames so a
/// frame allocates nothing.
pub(super) struct Scratch {
    norm: Vec<f32>,
    fold: Vec<f32>,
    order: Vec<f32>,
    pulses: Vec<i32>,
}

impl Scratch {
    pub(super) fn new() -> Result<Self, DecodeError> {
        let floats = |len: usize| filled(len, 0.0f32).ok_or(DecodeError::OutOfMemory);
        Ok(Self {
            norm: floats(2 * NORM_LEN)?,
            fold: floats(MAX_WIDTH)?,
            order: floats(MAX_WIDTH)?,
            pulses: filled(MAX_WIDTH, 0i32).ok_or(DecodeError::OutOfMemory)?,
        })
    }
}

/// The next value of the shared linear congruential generator.
pub(super) const fn lcg(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

const fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15
}

/// A cosine in Q15 that every platform computes identically, since it
/// decides how bits are allocated.
const fn bitexact_cos(x: i32) -> i32 {
    let x2 = ((4096 + x * x) >> 13) as i16 as i32;
    let x2 =
        ((32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)))) as i16;
    (1 + x2 as i32) as i16 as i32
}

/// `log2(sin / cos)` in Q11, computed identically everywhere.
fn bitexact_log2tan(sin: i32, cos: i32) -> i32 {
    let lc = ilog(cos as u32) as i32;
    let ls = ilog(sin as u32) as i32;
    let cos = cos << (15 - lc);
    let sin = sin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(sin, frac_mul16(sin, -2597) + 7932)
        - frac_mul16(cos, frac_mul16(cos, -2597) + 7932)
}

/// The integer square root.
fn isqrt(mut value: u32) -> u32 {
    let mut root = 0;
    let mut shift = (ilog(value) as i32 - 1) >> 1;
    if shift < 0 {
        return 0;
    }
    let mut bit = 1u32 << shift;
    loop {
        let trial = ((root << 1) + bit) << shift;
        if trial <= value {
            root += bit;
            value -= trial;
        }
        bit >>= 1;
        shift -= 1;
        if shift < 0 {
            return root;
        }
    }
}

/// The resolution an angle is coded at.
fn angle_steps(n: usize, bits: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut n2 = 2 * n as i32 - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let qb = ((bits + n2 * offset) / n2)
        .min(bits - pulse_cap - (4 << BITRES))
        .min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        return 1;
    }
    let qn = EXP2_TABLE8[(qb & 7) as usize] >> (14 - (qb >> BITRES));
    ((qn + 1) >> 1) << 1
}

/// Pairs of samples `stride` apart rotated by π/4.
fn haar(x: &mut [f32], n0: usize, stride: usize) {
    const HALF_ROOT: f32 = core::f32::consts::FRAC_1_SQRT_2;
    for i in 0..stride {
        for j in 0..n0 >> 1 {
            let (a, b) = (stride * 2 * j + i, stride * (2 * j + 1) + i);
            let t1 = HALF_ROOT * x[a];
            let t2 = HALF_ROOT * x[b];
            x[a] = t1 + t2;
            x[b] = t1 - t2;
        }
    }
}

/// The short blocks' natural Hadamard order turned to the order that puts
/// the DC block last.
const ORDERY: [usize; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

/// Gather each of `stride` interleaved blocks of `n0` together.
fn deinterleave(x: &mut [f32], n0: usize, stride: usize, hadamard: bool, tmp: &mut [f32]) {
    let n = n0 * stride;
    for i in 0..stride {
        let row = if hadamard { ORDERY[stride - 2 + i] } else { i };
        for j in 0..n0 {
            tmp[row * n0 + j] = x[j * stride + i];
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

/// Undo [`deinterleave`].
fn interleave(x: &mut [f32], n0: usize, stride: usize, hadamard: bool, tmp: &mut [f32]) {
    let n = n0 * stride;
    for i in 0..stride {
        let row = if hadamard { ORDERY[stride - 2 + i] } else { i };
        for j in 0..n0 {
            tmp[j * stride + i] = x[row * n0 + j];
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

/// A split's angle and what it implies.
struct Split {
    inverted: bool,
    mid: i32,
    side: i32,
    delta: i32,
    itheta: i32,
    spent: i32,
}

/// Decodes one frame's bands.
pub(super) struct Bands<'d, 'f> {
    pub(super) decoder: &'d mut RangeDecoder<'f>,
    pub(super) band: usize,
    pub(super) intensity: usize,
    pub(super) spread: usize,
    pub(super) tf_change: i32,
    pub(super) remaining: i32,
    pub(super) seed: u32,
    pub(super) disable_inversion: bool,
    pub(super) avoid_split_noise: bool,
    pub(super) order: &'d mut [f32],
    pub(super) pulses: &'d mut [i32],
}

impl Bands<'_, '_> {
    #[allow(
        clippy::too_many_arguments,
        reason = "the split's state, one value each"
    )]
    fn theta(
        &mut self,
        n: usize,
        bits: &mut i32,
        blocks: usize,
        blocks0: usize,
        lm: i32,
        stereo: bool,
        fill: &mut u32,
    ) -> Split {
        let pulse_cap = i32::from(LOG_N[self.band]) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if stereo && n == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = angle_steps(n, *bits, offset, pulse_cap, stereo);
        if stereo && self.band >= self.intensity {
            qn = 1;
        }
        let tell = self.decoder.tell_frac();
        let mut itheta = 0;
        let mut inverted = false;
        if qn != 1 {
            itheta = if stereo && n > 2 {
                self.stepped_angle(qn)
            } else if blocks0 > 1 || stereo {
                self.decoder.uint(qn as u32 + 1) as i32
            } else {
                self.triangular_angle(qn)
            };
            itheta = itheta * 16384 / qn;
        } else if stereo {
            if *bits > 2 << BITRES && self.remaining > 2 << BITRES {
                inverted = self.decoder.bit_logp(2);
            }
            if self.disable_inversion {
                inverted = false;
            }
        }
        let spent = self.decoder.tell_frac() - tell;
        *bits -= spent;
        let (mid, side, delta) = match itheta {
            0 => {
                *fill &= (1 << blocks) - 1;
                (32767, 0, -16384)
            }
            16384 => {
                *fill &= ((1 << blocks) - 1) << blocks;
                (0, 32767, 16384)
            }
            _ => {
                let mid = bitexact_cos(itheta);
                let side = bitexact_cos(16384 - itheta);
                (
                    mid,
                    side,
                    frac_mul16(((n as i32) - 1) << 7, bitexact_log2tan(side, mid)),
                )
            }
        };
        Split {
            inverted,
            mid,
            side,
            delta,
            itheta,
            spent,
        }
    }

    /// A stereo angle: probability three to each step up to the middle and
    /// one to each above.
    fn stepped_angle(&mut self, qn: i32) -> i32 {
        let p0 = 3;
        let x0 = qn / 2;
        let total = p0 * (x0 + 1) + x0;
        let point = self.decoder.decode(total as u32) as i32;
        let x = if point < (x0 + 1) * p0 {
            point / p0
        } else {
            x0 + 1 + (point - (x0 + 1) * p0)
        };
        let (low, high) = if x <= x0 {
            (p0 * x, p0 * (x + 1))
        } else {
            ((x - 1 - x0) + (x0 + 1) * p0, (x - x0) + (x0 + 1) * p0)
        };
        self.decoder.update(low as u32, high as u32, total as u32);
        x
    }

    /// A time split's angle, from a triangular distribution.
    fn triangular_angle(&mut self, qn: i32) -> i32 {
        let half = qn >> 1;
        let total = (half + 1) * (half + 1);
        let point = self.decoder.decode(total as u32) as i32;
        let (itheta, width, low) = if point < (half * (half + 1)) >> 1 {
            let itheta = (isqrt(8 * point as u32 + 1) as i32 - 1) >> 1;
            (itheta, itheta + 1, (itheta * (itheta + 1)) >> 1)
        } else {
            let itheta = (2 * (qn + 1) - isqrt(8 * (total - point - 1) as u32 + 1) as i32) >> 1;
            (
                itheta,
                qn + 1 - itheta,
                total - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1),
            )
        };
        self.decoder
            .update(low as u32, (low + width) as u32, total as u32);
        itheta
    }

    /// A band of one coefficient: a sign for each channel.
    fn single(&mut self, x: &mut [f32], y: Option<&mut [f32]>, out: Option<&mut [f32]>) -> u32 {
        x[0] = self.sign();
        if let Some(y) = y {
            y[0] = self.sign();
        }
        if let Some(out) = out {
            out[0] = x[0];
        }
        1
    }

    fn sign(&mut self) -> f32 {
        if self.remaining < 1 << BITRES {
            return 1.0;
        }
        self.remaining -= 1 << BITRES;
        if self.decoder.bits(1) != 0 {
            -1.0
        } else {
            1.0
        }
    }

    /// A mono partition, split in time while it has more bits than one
    /// codebook can use.
    #[allow(
        clippy::too_many_arguments,
        reason = "the partition's state, one value each"
    )]
    fn partition(
        &mut self,
        x: &mut [f32],
        mut bits: i32,
        mut blocks: usize,
        lowband: Option<&[f32]>,
        mut lm: i32,
        gain: f32,
        mut fill: u32,
    ) -> u32 {
        let n = x.len();
        let blocks0 = blocks;
        if lm != -1 && bits > most_bits(self.band, lm) + 12 && n > 2 {
            let half = n >> 1;
            let (x, y) = x.split_at_mut(half);
            lm -= 1;
            if blocks == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            blocks = (blocks + 1) >> 1;
            let split = self.theta(half, &mut bits, blocks, blocks0, lm, false, &mut fill);
            let mid = (1.0 / 32768.0) * split.mid as f32;
            let side = (1.0 / 32768.0) * split.side as f32;
            let mut delta = split.delta;
            if blocks0 > 1 && split.itheta & 0x3fff != 0 {
                if split.itheta > 8192 {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = (delta + (((half as i32) << BITRES) >> (5 - lm))).min(0);
                }
            }
            let mut mbits = bits.min((bits - delta) / 2).max(0);
            let mut sbits = bits - mbits;
            self.remaining -= split.spent;
            let (low, high) = match lowband {
                Some(lowband) => {
                    let (low, high) = lowband.split_at(half);
                    (Some(low), Some(high))
                }
                None => (None, None),
            };
            let before = self.remaining;
            let shift = blocks0 >> 1;
            if mbits >= sbits {
                let mut mask = self.partition(x, mbits, blocks, low, lm, gain * mid, fill);
                let rebalance = mbits - (before - self.remaining);
                if rebalance > 3 << BITRES && split.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                mask |= self.partition(y, sbits, blocks, high, lm, gain * side, fill >> blocks)
                    << shift;
                mask
            } else {
                let mut mask =
                    self.partition(y, sbits, blocks, high, lm, gain * side, fill >> blocks)
                        << shift;
                let rebalance = sbits - (before - self.remaining);
                if rebalance > 3 << BITRES && split.itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                mask |= self.partition(x, mbits, blocks, low, lm, gain * mid, fill);
                mask
            }
        } else {
            self.codebook(x, bits, blocks, lowband, lm, gain, fill)
        }
    }

    /// A partition coded whole: its pulses, or a fold or noise where it has
    /// none.
    #[allow(
        clippy::too_many_arguments,
        reason = "the partition's state, one value each"
    )]
    fn codebook(
        &mut self,
        x: &mut [f32],
        bits: i32,
        blocks: usize,
        lowband: Option<&[f32]>,
        lm: i32,
        gain: f32,
        mut fill: u32,
    ) -> u32 {
        let mut q = bits_to_pulses(self.band, lm, bits);
        let mut cost = pulses_to_bits(self.band, lm, q);
        self.remaining -= cost;
        while self.remaining < 0 && q > 0 {
            self.remaining += cost;
            q -= 1;
            cost = pulses_to_bits(self.band, lm, q);
            self.remaining -= cost;
        }
        if q != 0 {
            return unquantise(
                self.decoder,
                x,
                pulses(q) as usize,
                self.spread,
                blocks,
                gain,
                self.pulses,
            );
        }
        let all = (1u32 << blocks) - 1;
        fill &= all;
        if fill == 0 {
            x.fill(0.0);
            return 0;
        }
        let mask = match lowband {
            None => {
                for value in x.iter_mut() {
                    self.seed = lcg(self.seed);
                    *value = ((self.seed as i32) >> 20) as f32;
                }
                all
            }
            Some(lowband) => {
                for (value, &low) in x.iter_mut().zip(lowband) {
                    self.seed = lcg(self.seed);
                    let noise = if self.seed & 0x8000 != 0 {
                        1.0 / 256.0
                    } else {
                        -1.0 / 256.0
                    };
                    *value = low + noise;
                }
                fill
            }
        };
        renormalise(x, gain);
        mask
    }

    /// A mono band (or one channel of a dual-stereo pair): its time and
    /// frequency resolution changed as the frame states, then partitioned.
    /// `lowband` is the fold's private copy, which this may reorder.
    #[allow(
        clippy::too_many_arguments,
        reason = "the band's state, one value each"
    )]
    pub(super) fn mono(
        &mut self,
        x: &mut [f32],
        bits: i32,
        mut blocks: usize,
        mut lowband: Option<&mut [f32]>,
        lm: i32,
        out: Option<&mut [f32]>,
        gain: f32,
        mut fill: u32,
    ) -> u32 {
        const BIT_INTERLEAVE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
        const BIT_DEINTERLEAVE: [u8; 16] = [
            0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3,
            0xFC, 0xFF,
        ];
        let n0 = x.len();
        if n0 == 1 {
            return self.single(x, None, out);
        }
        let long_blocks = blocks == 1;
        let mut n_b = n0 / blocks;
        let mut tf_change = self.tf_change;
        let recombine = tf_change.max(0) as usize;
        for k in 0..recombine {
            if let Some(lowband) = lowband.as_deref_mut() {
                haar(lowband, n0 >> k, 1 << k);
            }
            fill = u32::from(BIT_INTERLEAVE[(fill & 0xF) as usize])
                | u32::from(BIT_INTERLEAVE[((fill >> 4) & 0xF) as usize]) << 2;
        }
        blocks >>= recombine;
        n_b <<= recombine;
        let mut time_divide = 0;
        while n_b & 1 == 0 && tf_change < 0 {
            if let Some(lowband) = lowband.as_deref_mut() {
                haar(lowband, n_b, blocks);
            }
            fill |= fill << blocks;
            blocks <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf_change += 1;
        }
        let blocks0 = blocks;
        let n_b0 = n_b;
        if blocks0 > 1 {
            if let Some(lowband) = lowband.as_deref_mut() {
                deinterleave(
                    lowband,
                    n_b >> recombine,
                    blocks0 << recombine,
                    long_blocks,
                    self.order,
                );
            }
        }
        let mut mask = self.partition(x, bits, blocks, lowband.as_deref(), lm, gain, fill);
        if blocks0 > 1 {
            interleave(
                x,
                n_b >> recombine,
                blocks0 << recombine,
                long_blocks,
                self.order,
            );
        }
        let mut n_b = n_b0;
        let mut blocks = blocks0;
        for _ in 0..time_divide {
            blocks >>= 1;
            n_b <<= 1;
            mask |= mask >> blocks;
            haar(x, n_b, blocks);
        }
        for k in 0..recombine {
            mask = u32::from(BIT_DEINTERLEAVE.get(mask as usize).copied().unwrap_or(0xFF));
            haar(x, n0 >> k, 1 << k);
        }
        blocks <<= recombine;
        if let Some(out) = out {
            let scale = sqrt(n0 as f32);
            for (folded, &value) in out.iter_mut().zip(x.iter()) {
                *folded = scale * value;
            }
        }
        mask & ((1 << blocks) - 1)
    }

    /// A stereo band: the angle between its channels, then mid and side.
    #[allow(
        clippy::too_many_arguments,
        reason = "the band's state, one value each"
    )]
    pub(super) fn stereo(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        mut bits: i32,
        blocks: usize,
        lowband: Option<&mut [f32]>,
        lm: i32,
        out: Option<&mut [f32]>,
        mut fill: u32,
    ) -> u32 {
        let n = x.len();
        if n == 1 {
            return self.single(x, Some(y), out);
        }
        let original_fill = fill;
        let split = self.theta(n, &mut bits, blocks, blocks, lm, true, &mut fill);
        let mid = (1.0 / 32768.0) * split.mid as f32;
        let side = (1.0 / 32768.0) * split.side as f32;
        let mask;
        if n == 2 {
            let sbits = if split.itheta != 0 && split.itheta != 16384 {
                1 << BITRES
            } else {
                0
            };
            let mbits = bits - sbits;
            let swapped = split.itheta > 8192;
            self.remaining -= split.spent + sbits;
            let sign = if sbits != 0 && self.decoder.bits(1) != 0 {
                -1.0
            } else {
                1.0
            };
            let (x2, y2) = if swapped {
                (&mut *y, &mut *x)
            } else {
                (&mut *x, &mut *y)
            };
            mask = self.mono(x2, mbits, blocks, lowband, lm, out, 1.0, original_fill);
            y2[0] = -sign * x2[1];
            y2[1] = sign * x2[0];
            for i in 0..2 {
                let left = mid * x[i];
                let right = side * y[i];
                x[i] = left - right;
                y[i] = left + right;
            }
        } else {
            let mut mbits = bits.min((bits - split.delta) / 2).max(0);
            let mut sbits = bits - mbits;
            self.remaining -= split.spent;
            let before = self.remaining;
            if mbits >= sbits {
                let mut cm = self.mono(x, mbits, blocks, lowband, lm, out, 1.0, fill);
                let rebalance = mbits - (before - self.remaining);
                if rebalance > 3 << BITRES && split.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                cm |= self.mono(y, sbits, blocks, None, lm, None, side, fill >> blocks);
                mask = cm;
            } else {
                let mut cm = self.mono(y, sbits, blocks, None, lm, None, side, fill >> blocks);
                let rebalance = sbits - (before - self.remaining);
                if rebalance > 3 << BITRES && split.itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                cm |= self.mono(x, mbits, blocks, lowband, lm, out, 1.0, fill);
                mask = cm;
            }
            merge(x, y, mid);
        }
        if split.inverted {
            for value in y.iter_mut() {
                *value = -*value;
            }
        }
        mask
    }
}

/// Turn a band's normalised mid and side back into left and right.
fn merge(x: &mut [f32], y: &mut [f32], mid: f32) {
    let (cross, side) = y
        .iter()
        .zip(x.iter())
        .fold((0.0f32, 0.0f32), |(xy, yy), (&b, &a)| {
            (xy + b * a, yy + b * b)
        });
    let cross = mid * cross;
    let left = mid * mid + side - 2.0 * cross;
    let right = mid * mid + side + 2.0 * cross;
    if right < 6e-4 || left < 6e-4 {
        y.copy_from_slice(x);
        return;
    }
    let left_gain = rsqrt(left);
    let right_gain = rsqrt(right);
    for (a, b) in x.iter_mut().zip(y.iter_mut()) {
        let l = mid * *a;
        let r = *b;
        *a = left_gain * (l - r);
        *b = right_gain * (l + r);
    }
}

/// What [`all`] needs to know of the frame.
pub(super) struct Frame<'p> {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) lm: usize,
    pub(super) short_blocks: bool,
    pub(super) spread: usize,
    pub(super) dual_stereo: bool,
    pub(super) intensity: usize,
    pub(super) tf: &'p [i32; BAND_COUNT],
    pub(super) pulses: &'p [i32; BAND_COUNT],
    pub(super) coded: usize,
    pub(super) total_bits: i32,
    pub(super) balance: i32,
    pub(super) disable_inversion: bool,
}

/// Decode every band's shape into `x` (and `y` for a stereo frame), each
/// one channel's coefficients; the short blocks each band filled.
#[allow(
    clippy::too_many_lines,
    reason = "the reference's quant_all_bands, kept whole so the port can be read against it"
)]
pub(super) fn all(
    decoder: &mut RangeDecoder<'_>,
    frame: &Frame<'_>,
    x: &mut [f32],
    mut y: Option<&mut [f32]>,
    collapse: &mut [u8; 2 * BAND_COUNT],
    seed: &mut u32,
    scratch: &mut Scratch,
) {
    let m = 1usize << frame.lm;
    let lm = frame.lm as i32;
    let blocks = if frame.short_blocks { m } else { 1 };
    let channels = if y.is_some() { 2 } else { 1 };
    let start = frame.start;
    let norm_offset = m * BANDS[start];
    let norm_len = m * BANDS[BAND_COUNT - 1] - norm_offset;
    let (norm, norm2) = scratch.norm.split_at_mut(norm_len);
    let fold = &mut scratch.fold;
    let mut bands = Bands {
        decoder,
        band: start,
        intensity: frame.intensity,
        spread: frame.spread,
        tf_change: 0,
        remaining: 0,
        seed: *seed,
        disable_inversion: frame.disable_inversion,
        avoid_split_noise: blocks > 1,
        order: &mut scratch.order,
        pulses: &mut scratch.pulses,
    };
    let mut balance = frame.balance;
    let mut dual_stereo = frame.dual_stereo;
    let mut lowband_offset = 0;
    let mut update_lowband = true;
    for band in start..frame.end {
        bands.band = band;
        let last = band == frame.end - 1;
        let range = m * BANDS[band]..m * BANDS[band + 1];
        let n = range.len();
        let tell = bands.decoder.tell_frac();
        if band != start {
            balance -= tell;
        }
        let remaining = frame.total_bits - tell - 1;
        bands.remaining = remaining;
        let bits = if band < frame.coded {
            let current = balance / (frame.coded - band).min(3) as i32;
            (remaining + 1)
                .min(frame.pulses[band] + current)
                .clamp(0, 16383)
        } else {
            0
        };
        let below = (m * BANDS[band]) as i32 - n as i32;
        if (below >= norm_offset as i32 || band == start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = band;
        }
        if band == start + 1 {
            hybrid_folding(norm, norm2, start, m, dual_stereo);
        }
        bands.tf_change = frame.tf[band];
        let folding = lowband_offset != 0
            && (frame.spread != SPREAD_AGGRESSIVE || blocks > 1 || bands.tf_change < 0);
        let mut effective = None;
        let (mut x_cm, mut y_cm);
        if folding {
            let low = ((m * BANDS[lowband_offset]) as i32 - norm_offset as i32 - n as i32).max(0)
                as usize;
            effective = Some(low);
            let mut fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if m * BANDS[fold_start] <= low + norm_offset {
                    break;
                }
            }
            let mut fold_end = lowband_offset - 1;
            loop {
                fold_end += 1;
                if !(fold_end < band && m * BANDS[fold_end] < low + norm_offset + n) {
                    break;
                }
            }
            x_cm = 0;
            y_cm = 0;
            let mut index = fold_start;
            loop {
                x_cm |= u32::from(collapse[index * channels]);
                y_cm |= u32::from(collapse[index * channels + channels - 1]);
                index += 1;
                if index >= fold_end {
                    break;
                }
            }
        } else {
            x_cm = (1 << blocks) - 1;
            y_cm = x_cm;
        }
        if dual_stereo && band == frame.intensity {
            dual_stereo = false;
            let folded = m * BANDS[band] - norm_offset;
            #[allow(
                clippy::manual_midpoint,
                reason = "the reference's HALF32(a + b), rounded where it rounds"
            )]
            for (a, &b) in norm[..folded].iter_mut().zip(norm2[..folded].iter()) {
                *a = 0.5 * (*a + b);
            }
        }
        let out_at = m * BANDS[band] - norm_offset;
        let xb = &mut x[range.clone()];
        match y.as_deref_mut() {
            Some(y) if dual_stereo => {
                let low = fold_from(norm, effective, fold, n);
                let out = (!last).then(|| &mut norm[out_at..out_at + n]);
                x_cm = bands.mono(xb, bits / 2, blocks, low, lm, out, 1.0, x_cm);
                let low = fold_from(norm2, effective, fold, n);
                let out = (!last).then(|| &mut norm2[out_at..out_at + n]);
                y_cm = bands.mono(&mut y[range], bits / 2, blocks, low, lm, out, 1.0, y_cm);
            }
            y => {
                let low = fold_from(norm, effective, fold, n);
                let out = (!last).then(|| &mut norm[out_at..out_at + n]);
                x_cm = match y {
                    Some(y) => {
                        bands.stereo(xb, &mut y[range], bits, blocks, low, lm, out, x_cm | y_cm)
                    }
                    None => bands.mono(xb, bits, blocks, low, lm, out, 1.0, x_cm | y_cm),
                };
                y_cm = x_cm;
            }
        }
        collapse[band * channels] = x_cm as u8;
        collapse[band * channels + channels - 1] = y_cm as u8;
        balance += frame.pulses[band] + tell;
        update_lowband = bits > ((n as i32) << BITRES);
        bands.avoid_split_noise = false;
    }
    *seed = bands.seed;
}

/// A private copy of the `n` coefficients the band folds from, which the
/// band may reorder.
fn fold_from<'f>(
    norm: &[f32],
    effective: Option<usize>,
    fold: &'f mut [f32],
    n: usize,
) -> Option<&'f mut [f32]> {
    let low = effective?;
    fold[..n].copy_from_slice(&norm[low..low + n]);
    Some(&mut fold[..n])
}

/// Duplicate enough of the first band's fold for the second to fold from,
/// as a hybrid frame's first CELT band is narrower than its second.
fn hybrid_folding(norm: &mut [f32], norm2: &mut [f32], start: usize, m: usize, dual_stereo: bool) {
    let n1 = m * (BANDS[start + 1] - BANDS[start]);
    let n2 = m * (BANDS[start + 2] - BANDS[start + 1]);
    if n2 > n1 {
        norm.copy_within(2 * n1 - n2..n1, n1);
        if dual_stereo {
            norm2.copy_within(2 * n1 - n2..n1, n1);
        }
    }
}

/// Fill each short block a transient frame's band left empty with noise at
/// the energy its neighbours' imply, so a transient never collapses to
/// silence between its blocks.
#[allow(
    clippy::too_many_arguments,
    reason = "the frame's energies, one row each"
)]
pub(super) fn anti_collapse(
    x: &mut [f32],
    collapse: &[u8; 2 * BAND_COUNT],
    lm: usize,
    channels: usize,
    size: usize,
    bands: core::ops::Range<usize>,
    energy: &[f32; 2 * BAND_COUNT],
    previous: &[f32; 2 * BAND_COUNT],
    before: &[f32; 2 * BAND_COUNT],
    pulses: &[i32; BAND_COUNT],
    mut seed: u32,
) {
    for band in bands {
        let n0 = BANDS[band + 1] - BANDS[band];
        let depth = ((1 + pulses[band]) / n0 as i32) >> lm;
        let threshold = 0.5 * super::exp2(-0.125 * depth as f32);
        let scale = rsqrt((n0 << lm) as f32);
        for channel in 0..channels {
            let mut prev1 = previous[channel * BAND_COUNT + band];
            let mut prev2 = before[channel * BAND_COUNT + band];
            if channels == 1 {
                prev1 = super::max16(prev1, previous[BAND_COUNT + band]);
                prev2 = super::max16(prev2, before[BAND_COUNT + band]);
            }
            let difference = super::max16(
                0.0,
                energy[channel * BAND_COUNT + band] - super::min16(prev1, prev2),
            );
            let mut r = 2.0 * super::exp2(-difference);
            if lm == 3 {
                r *= core::f32::consts::SQRT_2;
            }
            let r = super::min16(threshold, r) * scale;
            let at = channel * size + (BANDS[band] << lm);
            let coefficients = &mut x[at..at + (n0 << lm)];
            let mut renormalize = false;
            for k in 0..1 << lm {
                if u32::from(collapse[band * channels + channel]) & (1 << k) == 0 {
                    for j in 0..n0 {
                        seed = lcg(seed);
                        coefficients[(j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                    }
                    renormalize = true;
                }
            }
            if renormalize {
                renormalise(coefficients, 1.0);
            }
        }
    }
}

/// Scale each band's unit shape by its energy into MDCT coefficients.
pub(super) fn denormalise(
    x: &[f32],
    freq: &mut [f32],
    energy: &[f32],
    start: usize,
    end: usize,
    m: usize,
    silence: bool,
) {
    let (start, end) = if silence { (0, 0) } else { (start, end) };
    let bound = m * BANDS[end];
    freq[..m * BANDS[start]].fill(0.0);
    for band in start..end {
        let range = m * BANDS[band]..m * BANDS[band + 1];
        let log_gain = energy[band] + super::tables::ENERGY_MEANS[band];
        let gain = super::exp2(log_gain.min(32.0));
        for (out, &shape) in freq[range.clone()].iter_mut().zip(&x[range]) {
            *out = shape * gain;
        }
    }
    freq[bound..].fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::{bitexact_cos, bitexact_log2tan, isqrt};

    #[test]
    fn the_integer_square_root_is_exact() {
        for value in (0u32..5000).chain([u32::MAX, 1 << 31, 65535, 65536]) {
            let root = isqrt(value);
            assert!(u64::from(root) * u64::from(root) <= u64::from(value));
            assert!((u64::from(root) + 1) * (u64::from(root) + 1) > u64::from(value));
        }
    }

    #[test]
    fn the_bit_exact_cosine_and_log_tangent_hold_their_reference_values() {
        assert_eq!(bitexact_cos(8192), 23171);
        for x in (1..16384).step_by(97) {
            let (c, s) = (
                i64::from(bitexact_cos(x)),
                i64::from(bitexact_cos(16384 - x)),
            );
            assert!((c * c + s * s - (1 << 30)).abs() < (1 << 30) / 200, "{x}");
        }
        assert_eq!(bitexact_log2tan(23171, 23171), 0);
        assert_eq!(bitexact_log2tan(32767, 1), 30690);
    }
}

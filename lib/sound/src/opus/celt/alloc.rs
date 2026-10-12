//! The bit allocation (RFC 6716, section 4.3.3): the frame's bits split
//! among its bands by interpolating the allocation curves, the bands the
//! encoder skipped, the stereo parameters, and each band's split between
//! fine energy and the shape.
//!
//! None of this is coded except the skip flags and the stereo parameters:
//! the decoder recomputes what the encoder computed, so every step is integer
//! arithmetic the encoder's matches exactly.

use super::tables::{ALLOCATION, BANDS, CACHE_BITS, CACHE_CAPS, CACHE_INDEX, LOG2_FRAC, LOG_N};
use super::{BAND_COUNT, MAX_FINE_BITS};
use crate::opus::range::{RangeDecoder, BITRES};

const ALLOC_STEPS: u32 = 6;
const LOG_MAX_PSEUDO: usize = 6;
const FINE_OFFSET: i32 = 21;

/// What the allocation gives each band.
pub(super) struct Allocation {
    /// The shape's bits, in eighths.
    pub(super) pulses: [i32; BAND_COUNT],
    /// The fine energy's bits per channel.
    pub(super) fine: [u32; BAND_COUNT],
    /// Whether the band takes a leftover bit in the first pass.
    pub(super) priority: [bool; BAND_COUNT],
    /// The bands up to which the shape is coded.
    pub(super) coded: usize,
    /// The first band coded as intensity stereo.
    pub(super) intensity: usize,
    pub(super) dual_stereo: bool,
    /// Bits over the caps, carried to the shape's rebalancing.
    pub(super) balance: i32,
}

fn width(band: usize) -> i32 {
    (BANDS[band + 1] - BANDS[band]) as i32
}

/// The band's most useful shape bits, in eighths.
pub(super) fn caps(lm: usize, channels: usize) -> [i32; BAND_COUNT] {
    core::array::from_fn(|band| {
        let n = width(band) << lm;
        let cap = i32::from(CACHE_CAPS[BAND_COUNT * (2 * lm + channels - 1) + band]);
        ((cap + 64) * channels as i32 * n) >> 2
    })
}

/// The pulse cache's row for `band` at block size `lm`, which may be `-1`
/// for a band split below the shortest block: a count, then the bits each
/// pulse count up to it costs.
fn cache(band: usize, lm: i32) -> &'static [u8] {
    let row = usize::try_from(lm + 1)
        .ok()
        .map(|lm| lm * BAND_COUNT + band);
    row.and_then(|row| CACHE_INDEX.get(row))
        .and_then(|&start| usize::try_from(start).ok())
        .and_then(|start| CACHE_BITS.get(start..))
        .unwrap_or(&[0])
}

/// The most bits the cache states for one partition of `band` at `lm`.
pub(super) fn most_bits(band: usize, lm: i32) -> i32 {
    let row = cache(band, lm);
    i32::from(row.get(usize::from(row[0])).copied().unwrap_or(0))
}

/// The pulse count whose cost is nearest `bits`, in eighths.
pub(super) fn bits_to_pulses(band: usize, lm: i32, bits: i32) -> usize {
    let row = cache(band, lm);
    let at = |index: usize| i32::from(row.get(index).copied().unwrap_or(0));
    let mut lo = 0;
    let mut hi = usize::from(row[0]);
    let bits = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if at(mid) >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let below = if lo == 0 { -1 } else { at(lo) };
    if bits - below <= at(hi) - bits {
        lo
    } else {
        hi
    }
}

/// What `pulses` cost in `band` at `lm`, in eighths.
pub(super) fn pulses_to_bits(band: usize, lm: i32, pulses: usize) -> i32 {
    if pulses == 0 {
        return 0;
    }
    i32::from(cache(band, lm).get(pulses).copied().unwrap_or(0)) + 1
}

/// The pulses a cache index stands for: exact to eight, then coarser.
pub(super) const fn pulses(index: usize) -> u32 {
    if index < 8 {
        index as u32
    } else {
        (8 + (index as u32 & 7)) << ((index >> 3) - 1)
    }
}

/// The inputs the allocation is computed from.
pub(super) struct Request<'r> {
    pub(super) start: usize,
    pub(super) end: usize,
    /// The dynamic allocation's boosts, in eighths.
    pub(super) offsets: &'r [i32; BAND_COUNT],
    pub(super) caps: &'r [i32; BAND_COUNT],
    pub(super) trim: i32,
    /// The bits to allocate, in eighths.
    pub(super) total: i32,
    pub(super) channels: usize,
    pub(super) lm: usize,
}

/// Compute the allocation, reading the skip flags and stereo parameters it
/// states.
#[allow(
    clippy::too_many_lines,
    reason = "the reference's clt_compute_allocation, kept whole so the port can be read against it"
)]
pub(super) fn compute(request: &Request<'_>, decoder: &mut RangeDecoder<'_>) -> Allocation {
    let Request {
        start,
        end,
        offsets,
        caps,
        trim,
        channels,
        lm,
        ..
    } = *request;
    let c = channels as i32;
    let mut total = request.total.max(0);
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut thresh = [0i32; BAND_COUNT];
    let mut trim_offset = [0i32; BAND_COUNT];
    for band in start..end {
        let n = width(band);
        thresh[band] = (c << BITRES).max(((3 * n) << lm << BITRES) >> 4);
        trim_offset[band] = (c
            * n
            * (trim - 5 - lm as i32)
            * (end - band - 1) as i32
            * (1 << (lm as u32 + BITRES)))
            >> 6;
        if n << lm == 1 {
            trim_offset[band] -= c << BITRES;
        }
    }
    let curve = |vector: usize, band: usize| {
        (c * width(band) * i32::from(ALLOCATION[vector][band])) << lm >> 2
    };
    let mut lo = 1;
    let mut hi = ALLOCATION.len() - 1;
    while lo <= hi {
        let mid = lo.midpoint(hi);
        let mut done = false;
        let mut psum = 0;
        for band in (start..end).rev() {
            let mut bits = curve(mid, band);
            if bits > 0 {
                bits = (bits + trim_offset[band]).max(0);
            }
            bits += offsets[band];
            if bits >= thresh[band] || done {
                done = true;
                psum += bits.min(caps[band]);
            } else if bits >= c << BITRES {
                psum += c << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    let hi = lo;
    let lo = lo - 1;
    let mut skip_start = start;
    let mut bits1 = [0i32; BAND_COUNT];
    let mut bits2 = [0i32; BAND_COUNT];
    for band in start..end {
        let mut low = curve(lo, band);
        let mut high = if hi >= ALLOCATION.len() {
            caps[band]
        } else {
            curve(hi, band)
        };
        if low > 0 {
            low = (low + trim_offset[band]).max(0);
        }
        if high > 0 {
            high = (high + trim_offset[band]).max(0);
        }
        if lo > 0 {
            low += offsets[band];
        }
        high += offsets[band];
        if offsets[band] > 0 {
            skip_start = band;
        }
        bits1[band] = low;
        bits2[band] = (high - low).max(0);
    }
    interpolate(
        &Curves {
            request,
            skip_start,
            bits1: &bits1,
            bits2: &bits2,
            thresh: &thresh,
        },
        Reserves {
            total,
            skip: skip_rsv,
            intensity: intensity_rsv,
            dual_stereo: dual_stereo_rsv,
        },
        decoder,
    )
}

struct Curves<'c> {
    request: &'c Request<'c>,
    skip_start: usize,
    bits1: &'c [i32; BAND_COUNT],
    bits2: &'c [i32; BAND_COUNT],
    thresh: &'c [i32; BAND_COUNT],
}

#[derive(Clone, Copy)]
struct Reserves {
    total: i32,
    skip: i32,
    intensity: i32,
    dual_stereo: i32,
}

#[allow(
    clippy::too_many_lines,
    reason = "the reference's interp_bits2pulses, kept whole so the port can be read against it"
)]
fn interpolate(
    curves: &Curves<'_>,
    reserves: Reserves,
    decoder: &mut RangeDecoder<'_>,
) -> Allocation {
    let Request {
        start,
        end,
        caps,
        channels,
        lm,
        ..
    } = *curves.request;
    let Reserves {
        mut total,
        skip: skip_rsv,
        intensity: mut intensity_rsv,
        dual_stereo: mut dual_stereo_rsv,
    } = reserves;
    let (bits1, bits2, thresh) = (curves.bits1, curves.bits2, curves.thresh);
    let c = channels as i32;
    let alloc_floor = c << BITRES;
    let stereo = u32::from(channels > 1);
    let log_m = (lm as i32) << BITRES;
    let mut lo = 0;
    let mut hi = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for band in (start..end).rev() {
            let bits = bits1[band] + ((mid * bits2[band]) >> ALLOC_STEPS);
            if bits >= thresh[band] || done {
                done = true;
                psum += bits.min(caps[band]);
            } else if bits >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0;
    let mut done = false;
    let mut bits = [0i32; BAND_COUNT];
    for band in (start..end).rev() {
        let mut tmp = bits1[band] + ((lo * bits2[band]) >> ALLOC_STEPS);
        if tmp < thresh[band] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(caps[band]);
        bits[band] = tmp;
        psum += tmp;
    }
    let span = |band: usize| (BANDS[band] - BANDS[start]) as i32;
    let mut coded = end;
    loop {
        let j = coded - 1;
        if j <= curves.skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let per_coefficient = left.max(0) / span(coded);
        left -= span(coded) * per_coefficient;
        let rem = (left - span(j)).max(0);
        let band_width = (BANDS[coded] - BANDS[j]) as i32;
        let mut band_bits = bits[j] + per_coefficient * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if decoder.bit_logp(1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC[j - start]);
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded -= 1;
    }
    let intensity = if intensity_rsv > 0 {
        start + decoder.uint((coded + 1 - start) as u32) as usize
    } else {
        0
    };
    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = dual_stereo_rsv > 0 && decoder.bit_logp(1);

    let mut left = total - psum;
    let per_coefficient = left.max(0) / span(coded);
    left -= span(coded) * per_coefficient;
    for (band, bits) in bits.iter_mut().enumerate().take(coded).skip(start) {
        let extra = left.min(width(band));
        *bits += per_coefficient * width(band) + extra;
        left -= extra;
    }

    let mut fine = [0u32; BAND_COUNT];
    let mut priority = [false; BAND_COUNT];
    let mut balance = 0;
    for band in start..coded {
        let n = width(band) << lm;
        let bit = bits[band] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - caps[band]).max(0);
            bits[band] = bit - excess;
            let den = c * n + i32::from(channels == 2 && n > 2 && !dual_stereo && band < intensity);
            let nc_log_n = den * (i32::from(LOG_N[band]) + log_m);
            let mut offset = (nc_log_n >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            if bits[band] + offset < (den * 2) << BITRES {
                offset += nc_log_n >> 2;
            } else if bits[band] + offset < (den * 3) << BITRES {
                offset += nc_log_n >> 3;
            }
            let rounded = (bits[band] + offset + (den << (BITRES - 1))).max(0);
            let mut ebits = (rounded / den) >> BITRES;
            if c * ebits > bits[band] >> BITRES {
                ebits = bits[band] >> stereo >> BITRES;
            }
            let ebits = ebits.min(MAX_FINE_BITS as i32);
            priority[band] = ebits * (den << BITRES) >= bits[band] + offset;
            bits[band] -= (c * ebits) << BITRES;
            fine[band] = ebits as u32;
        } else {
            excess = (bit - (c << BITRES)).max(0);
            bits[band] = bit - excess;
            fine[band] = 0;
            priority[band] = true;
        }
        if excess > 0 {
            let extra_fine =
                (excess >> (stereo + BITRES)).min(MAX_FINE_BITS as i32 - fine[band] as i32);
            fine[band] += extra_fine as u32;
            let extra_bits = (extra_fine * c) << BITRES;
            priority[band] = extra_bits >= excess - balance;
            excess -= extra_bits;
        }
        balance = excess;
    }
    for band in coded..end {
        fine[band] = (bits[band] >> stereo >> BITRES) as u32;
        bits[band] = 0;
        priority[band] = fine[band] < 1;
    }
    Allocation {
        pulses: bits,
        fine,
        priority,
        coded,
        intensity,
        dual_stereo,
        balance,
    }
}

//! The CELT encoder (RFC 6716, section 5.3): fullband frames of 20 ms, coded
//! by the routines the decoder reads them with.
//!
//! A frame is pre-emphasised and transformed by the exact inverse of the
//! decoder's MDCT and overlap-add, as eight short blocks where it holds an
//! onset, then split into its bands' energies and unit shapes, which the
//! shared coding writes. The choices the format leaves an encoder follow its
//! analysis: the spreading from how peaked the audible bands' shapes are,
//! boosts for the bands standing over their neighbours' envelope, the
//! allocation's tilt from the spectrum's slope and the channels'
//! correlation, and every band the bitrate reaches. It runs no pitch
//! pre-filter.

use alloc::vec::Vec;

use tairix_util::fallible::filled;
use tairix_util::mathf;

use super::bands::Scratch;
use super::pvq::inner;
use super::tables::{BANDS, ENERGY_MEANS, LOG_N, TRIM_ICDF, WINDOW};
use super::{
    boost_step, code, max16, min16, sqrt, Aims, Content, Span, BAND_COUNT, EPSILON, EVEN_TRIM,
    MAX_FRAME, MAX_LM, OVERLAP, PREEMPHASIS, SHORT_BLOCK, SIGNAL_SCALE, SILENT_ENERGY,
    SPREAD_AGGRESSIVE, SPREAD_NORMAL,
};
use crate::opus::range::{writer::RangeEncoder, BITRES};
use crate::transform::Dct4;

/// The block size every frame is coded at: 20 ms.
const LM: usize = MAX_LM;
/// The short blocks an onset's frame is coded as.
const BLOCKS: usize = 1 << LM;
/// The samples one frame's transform reads: the overlap the last frame left,
/// then the frame.
const WINDOW_LEN: usize = OVERLAP + MAX_FRAME;
/// How far a short block's energy rises over the mean of those before it to
/// be an onset.
const ONSET: f32 = 10.0;
/// A short block's energy at 60 dB under full scale, below which nothing is
/// an onset.
const QUIET: f32 = SHORT_BLOCK as f32 * (SIGNAL_SCALE / 1000.0) * (SIGNAL_SCALE / 1000.0);
/// How much cheaper than mid and side a frame's channels must be to be coded
/// apart: coupling is always safe, so it is preferred.
const APART: f32 = 0.9;
/// A frame's bytes a channel below which its spreading stays normal.
const SPREAD_BYTES: usize = 10;
/// A shape of no more coefficients than this shows the spreading no peak.
const SHORT_SHAPE: usize = 8;
/// The shares `x²·width` of a peaked band that half its coefficients fall
/// under: the spreading counts how many a shape is peaked to.
const PEAKED: [f32; 3] = [0.25, 0.0625, 0.015_625];
/// The held peakedness under each of which the spreading steps from none to
/// light, normal and aggressive.
const SPREAD_STEPS: [i32; 3] = [384, 256, 80];
/// The peakedness a fresh encoder holds, which spreads normally.
const PEAKEDNESS_START: i32 = 256;
/// How far a band's masking falls a band toward the high bands, in `log2`
/// steps of 6 dB.
const MASK_UP: f32 = 2.0;
/// How far it falls a band toward the low bands.
const MASK_DOWN: f32 = 3.0;
/// How far under the loudest band a mask lies at most.
const MASK_DEPTH: f32 = 12.0;
/// Frames shorter than this spare no bits for boosts.
const BOOST_BYTES: usize = 30 + 5 * LM;
/// How far the envelope a band is boosted over rises a band toward the high
/// bands, in `log2` steps of 6 dB.
const ENVELOPE_UP: f32 = 1.5;
/// How far it rises a band toward the low bands.
const ENVELOPE_DOWN: f32 = 2.0;
/// How far a band stands over the one below it to be the highest the
/// envelope's downward pass starts from.
const ENVELOPE_STEP: f32 = 0.5;
/// How far under the median of the bands about it the envelope may lie.
const MEDIAN_SLACK: f32 = 1.0;
/// How far under the other channel's envelope a channel's may lie.
const CROSSTALK: f32 = 4.0;
/// The most a band stands over its envelope for its boost.
const MOST_EXCESS: f32 = 4.0;
/// The bands below this, whose boosts count twice.
const LOW_BOOSTED: usize = 8;
/// The bands from this, whose boosts count half.
const HIGH_BOOSTED: usize = 12;
/// The bits floating-point input resolves, which sets its noise floor.
const SOURCE_DEPTH: f32 = 24.0;
/// The low bands whose inter-channel correlation tilts the allocation.
const CORRELATED_BANDS: usize = 8;
/// The correlations past each of which the allocation leans a step further
/// toward the high bands.
const CORRELATED: [f32; 4] = [0.8, 0.85, 0.92, 0.995];
/// The spectral slopes past each of which it leans a step toward the high
/// bands.
const RISING: [f32; 2] = [2.0, 8.0];
/// The slopes under each of which it leans a step toward the low bands.
const FALLING: [f32; 2] = [-4.0, -10.0];

/// One stream's CELT encoder, mono or stereo.
pub(crate) struct CeltEncoder {
    channels: usize,
    /// The bytes each frame takes.
    bytes: usize,
    /// The first band coded as intensity stereo at this encoder's bitrate.
    intensity: usize,
    /// The spreading's average of the bands' peakedness across frames.
    peakedness: i32,
    /// The last frame's spreading, which the next is held toward.
    spread: usize,
    /// Each channel's last input sample, at signal scale.
    emphasis: [f32; 2],
    /// Each channel's [`WINDOW_LEN`] samples, pre-emphasised.
    window: Vec<f32>,
    /// The bands' energies as the decoder holds them: the next frame's
    /// prediction.
    energy: [f32; 2 * BAND_COUNT],
    /// The range coder's state after the last frame, which seeds the bands'
    /// folding noise.
    rng: u32,
    /// Whether a frame has been coded, after which each is predicted from the
    /// one before.
    started: bool,
    /// Set by a test to have the next frame taken as having outrun its bytes,
    /// which only an encoder defect makes it do.
    #[cfg(test)]
    pub(crate) outrun: bool,
    /// Each channel's coefficients, then its bands' unit shapes.
    coefficients: Vec<f32>,
    folded: Vec<f32>,
    transformed: Vec<f32>,
    long: Dct4,
    short: Dct4,
    bands: Scratch,
}

/// What coding a frame carries into the next — the fields of the same names
/// and each channel's window's last [`OVERLAP`] samples — so a refused frame
/// is undone to it.
struct Carried {
    overlap: [[f32; OVERLAP]; 2],
    emphasis: [f32; 2],
    energy: [f32; 2 * BAND_COUNT],
    rng: u32,
    started: bool,
    peakedness: i32,
    spread: usize,
}

impl CeltEncoder {
    /// The samples a channel each frame holds.
    pub(crate) const FRAME: usize = MAX_FRAME;
    /// The samples a decoder's output lags the input by: the overlap.
    pub(crate) const DELAY: u16 = OVERLAP as u16;

    /// An encoder of `channels`, one or two, writing frames of `bytes`;
    /// `None` where its memory cannot be had.
    pub(crate) fn new(channels: usize, bytes: usize) -> Option<Self> {
        let floats = |len: usize| filled(len, 0.0f32);
        Some(Self {
            channels,
            bytes,
            intensity: intensity_at(bytes),
            peakedness: PEAKEDNESS_START,
            spread: SPREAD_NORMAL,
            emphasis: [0.0; 2],
            window: floats(channels * WINDOW_LEN)?,
            energy: [0.0; 2 * BAND_COUNT],
            rng: 0,
            started: false,
            #[cfg(test)]
            outrun: false,
            coefficients: floats(2 * MAX_FRAME)?,
            folded: floats(MAX_FRAME)?,
            transformed: floats(MAX_FRAME)?,
            long: Dct4::new(MAX_FRAME)?,
            short: Dct4::new(SHORT_BLOCK)?,
            bands: Scratch::new().ok()?,
        })
    }

    /// The range coder's state after the last frame.
    #[cfg(test)]
    pub(crate) const fn final_range(&self) -> u32 {
        self.rng
    }

    /// Code [`Self::FRAME`] samples a channel of `pcm`, interleaved and
    /// finite, clipped to full scale at ±1, into all of `frame`; `false`
    /// where the coding outran it, the encoder then left as it was.
    pub(crate) fn encode(&mut self, pcm: &[f32], frame: &mut [u8]) -> bool {
        let kept = self.carried();
        let content = if self.analyse(pcm) {
            Content::Silence
        } else if self.onset() {
            Content::Onset
        } else {
            Content::Steady
        };
        self.transform(content == Content::Onset);
        let (energy, amplitudes) = self.normalise();
        let aims = Aims {
            content,
            intra: !self.started,
            energy: &energy,
            amplitudes: &amplitudes,
            tf: [content == Content::Onset; BAND_COUNT],
            intensity: self.intensity,
            dual_stereo: self.channels == 2 && self.apart(),
            spread: self.spreading(content, &energy),
            boosts: self.boosts(&energy, content),
            trim: self.trim(&energy),
        };
        let span = Span {
            start: 0,
            end: BAND_COUNT,
            channels: self.channels,
            lm: LM,
            disable_inversion: false,
        };
        let mut coder = RangeEncoder::new(frame);
        code(
            &mut coder,
            &span,
            &mut self.energy,
            &mut self.rng,
            &mut self.coefficients,
            &mut self.bands,
            &aims,
        );
        self.rng = coder.range();
        self.started = true;
        let sent = coder.finish();
        #[cfg(test)]
        let sent = sent && !core::mem::take(&mut self.outrun);
        // A frame that is not sent is never decoded, so the next is coded
        // from what the frame before it left.
        if !sent {
            self.restore(&kept);
        }
        sent
    }

    /// What coding a frame carries into the next.
    fn carried(&self) -> Carried {
        let mut overlap = [[0.0; OVERLAP]; 2];
        let windows = self.window.as_chunks::<WINDOW_LEN>().0;
        for (kept, window) in overlap.iter_mut().zip(windows) {
            kept.copy_from_slice(&window[MAX_FRAME..]);
        }
        Carried {
            overlap,
            emphasis: self.emphasis,
            energy: self.energy,
            rng: self.rng,
            started: self.started,
            peakedness: self.peakedness,
            spread: self.spread,
        }
    }

    /// Undo a refused frame's coding back to `kept`, what it was coded from.
    fn restore(&mut self, kept: &Carried) {
        let windows = self.window.as_chunks_mut::<WINDOW_LEN>().0;
        for (window, overlap) in windows.iter_mut().zip(&kept.overlap) {
            window[MAX_FRAME..].copy_from_slice(overlap);
        }
        self.emphasis = kept.emphasis;
        self.energy = kept.energy;
        self.rng = kept.rng;
        self.started = kept.started;
        self.peakedness = kept.peakedness;
        self.spread = kept.spread;
    }

    /// Pre-emphasise `pcm` into each channel's window after the overlap the
    /// last frame left; whether the whole window is silent.
    fn analyse(&mut self, pcm: &[f32]) -> bool {
        let channels = self.channels;
        let mut silent = true;
        let windows = self.window.as_chunks_mut::<WINDOW_LEN>().0.iter_mut();
        for (channel, (window, last)) in windows.zip(&mut self.emphasis).enumerate() {
            window.copy_within(MAX_FRAME.., 0);
            let samples = pcm.iter().skip(channel).step_by(channels);
            for (slot, &sample) in window[OVERLAP..].iter_mut().zip(samples) {
                let value = sample.clamp(-1.0, 1.0) * SIGNAL_SCALE;
                *slot = value - PREEMPHASIS * *last;
                *last = value;
            }
            silent &= window.iter().all(|value| value.abs() <= 0.0);
        }
        silent
    }

    /// Whether a channel's window holds an onset: a short block whose energy
    /// rises past [`QUIET`] and past [`ONSET`] times the mean of those before
    /// it.
    fn onset(&self) -> bool {
        self.window
            .as_chunks::<WINDOW_LEN>()
            .0
            .iter()
            .any(|window| {
                let mut before = 0.0f32;
                window
                    .as_chunks::<SHORT_BLOCK>()
                    .0
                    .iter()
                    .enumerate()
                    .any(|(index, block)| {
                        let energy = inner(block, block);
                        let onset =
                            index > 0 && energy > QUIET && energy > ONSET * before / index as f32;
                        before += energy;
                        onset
                    })
            })
    }

    /// Transform each channel's window into its coefficients: one long block,
    /// or short ones interleaved as the decoder gathers them.
    fn transform(&mut self, onset: bool) {
        let Self {
            window,
            coefficients,
            folded,
            transformed,
            long,
            short,
            ..
        } = self;
        let channels = window.as_chunks::<WINDOW_LEN>().0.iter();
        let outs = coefficients.as_chunks_mut::<MAX_FRAME>().0.iter_mut();
        for (samples, out) in channels.zip(outs) {
            if onset {
                for block in 0..BLOCKS {
                    let at = block * SHORT_BLOCK;
                    let input = &samples[at..at + SHORT_BLOCK + OVERLAP];
                    mdct(short, input, folded, transformed);
                    for (k, &value) in transformed[..SHORT_BLOCK].iter().enumerate() {
                        out[block + BLOCKS * k] = value;
                    }
                }
            } else {
                mdct(long, samples, folded, transformed);
                out.copy_from_slice(&transformed[..MAX_FRAME]);
            }
        }
    }

    /// Each band's energy, as the energies are coded, and its amplitude; each
    /// channel's coefficients left as its bands' unit shapes.
    fn normalise(&mut self) -> ([f32; 2 * BAND_COUNT], [f32; 2 * BAND_COUNT]) {
        let mut energy = [0.0f32; 2 * BAND_COUNT];
        let mut amplitudes = [0.0f32; 2 * BAND_COUNT];
        let channels = self
            .coefficients
            .as_chunks_mut::<MAX_FRAME>()
            .0
            .iter_mut()
            .take(self.channels);
        for (channel, coefficients) in channels.enumerate() {
            for band in 0..BAND_COUNT {
                let shape = &mut coefficients[BANDS[band] << LM..BANDS[band + 1] << LM];
                let amplitude = sqrt(EPSILON + inner(shape, shape));
                for value in shape.iter_mut() {
                    *value /= amplitude;
                }
                let at = channel * BAND_COUNT + band;
                amplitudes[at] = amplitude;
                let log = mathf::log2(f64::from(amplitude)) as f32;
                energy[at] = max16(log - ENERGY_MEANS[band], SILENT_ENERGY);
            }
        }
        (energy, amplitudes)
    }

    /// Whether the bands below intensity stereo cost less as left and right
    /// than as mid and side, by their shapes' L1 norms: the rate a Laplacian
    /// source takes.
    fn apart(&self) -> bool {
        let end = BANDS[self.intensity.min(BAND_COUNT)] << LM;
        let (left, right) = self.coefficients.split_at(MAX_FRAME);
        let (apart, together) = left[..end].iter().zip(&right[..end]).fold(
            (0.0f32, 0.0f32),
            |(apart, together), (&l, &r)| {
                (
                    apart + l.abs() + r.abs(),
                    together + (l + r).abs() + (l - r).abs(),
                )
            },
        );
        apart < APART * core::f32::consts::FRAC_1_SQRT_2 * together
    }

    /// The spreading the frame's shapes are coded with (RFC 6716, section
    /// 5.3.7): none where its audible bands' shapes are peaked, as a tone's
    /// are, aggressive where they are flat, as noise's are, from an average
    /// across frames held toward the last decision; normal for an onset and
    /// for a frame too short to tell.
    fn spreading(&mut self, content: Content, energy: &[f32; 2 * BAND_COUNT]) -> usize {
        if content == Content::Silence {
            return self.spread;
        }
        if content == Content::Onset || self.bytes < SPREAD_BYTES * self.channels {
            self.spread = SPREAD_NORMAL;
            return self.spread;
        }
        let audible = audibility(energy, self.channels);
        let (mut peaked, mut weighed) = (0u32, 0u32);
        let channels = self.coefficients.as_chunks::<MAX_FRAME>().0.iter();
        for shapes in channels.take(self.channels) {
            for (band, &weight) in audible.iter().enumerate() {
                let shape = &shapes[BANDS[band] << LM..BANDS[band + 1] << LM];
                if shape.len() > SHORT_SHAPE {
                    peaked += weight * peaks(shape);
                    weighed += weight;
                }
            }
        }
        let peakedness = ((peaked << 8) / weighed.max(1)) as i32;
        self.peakedness = (peakedness + self.peakedness) >> 1;
        let bias = (((SPREAD_AGGRESSIVE - self.spread) as i32) << 7) + 64;
        let held = (3 * self.peakedness + bias + 2) >> 2;
        self.spread = SPREAD_STEPS.iter().filter(|&&step| held < step).count();
        self.spread
    }

    /// Each band's boost in quanta of the dynamic allocation (RFC 6716,
    /// section 4.3.3): a bit a coefficient for each 6 dB the band stands over
    /// its neighbours' [`envelope`], so a peak such as a tone has the bits its
    /// level asks for. A steady frame's are halved, as a constant bitrate
    /// cannot keep up an onset's; the low bands' count twice and the high
    /// bands' half, as the ear weighs them; and all take two thirds of the
    /// frame at most.
    fn boosts(&self, energy: &[f32; 2 * BAND_COUNT], content: Content) -> [i32; BAND_COUNT] {
        let mut boosts = [0; BAND_COUNT];
        if content == Content::Silence || self.bytes < BOOST_BYTES {
            return boosts;
        }
        let (left, right) = energy.split_at(BAND_COUNT);
        let mut envelopes = [envelope(left), envelope(right)];
        let most = ((2 * self.bytes / 3) << (3 + BITRES)) as i32;
        let mut spent = 0;
        for (band, boost) in boosts.iter_mut().enumerate() {
            let mut excess = if self.channels == 2 {
                let [first, second] = &mut envelopes;
                second[band] = max16(second[band], first[band] - CROSSTALK);
                first[band] = max16(first[band], second[band] - CROSSTALK);
                f32::midpoint(
                    max16(0.0, left[band] - first[band]),
                    max16(0.0, right[band] - second[band]),
                )
            } else {
                max16(0.0, left[band] - envelopes[0][band])
            };
            if content == Content::Steady {
                excess *= 0.5;
            }
            if band < LOW_BOOSTED {
                excess *= 2.0;
            } else if band >= HIGH_BOOSTED {
                excess *= 0.5;
            }
            let (width, quanta) = boost_step(band, self.channels, LM);
            let wanted =
                (min16(excess, MOST_EXCESS) * (width << BITRES) as f32 / quanta as f32) as i32;
            *boost = wanted.min((most - spent) / quanta);
            spent += *boost * quanta;
            if *boost < wanted {
                break;
            }
        }
        boosts
    }

    /// The allocation's tilt (RFC 6716, section 4.3.3): a step toward the
    /// high bands for each of [`RISING`] the spectrum's slope passes, toward
    /// the low ones for each of [`FALLING`] it falls under, and further toward
    /// the high ones for each of [`CORRELATED`] the channels' low bands
    /// correlate past, which mid and side then code for less.
    fn trim(&self, energy: &[f32; 2 * BAND_COUNT]) -> usize {
        let weighted: f32 = energy
            .as_chunks::<BAND_COUNT>()
            .0
            .iter()
            .take(self.channels)
            .flat_map(|row| row[..BAND_COUNT - 1].iter().enumerate())
            .map(|(band, &level)| level * (2 * band as i32 + 2 - BAND_COUNT as i32) as f32)
            .sum();
        let slope = weighted / (2 * self.channels * (BAND_COUNT - 1)) as f32;
        let falls = FALLING.iter().filter(|&&fall| slope < fall).count();
        let rises = RISING.iter().filter(|&&rise| slope > rise).count();
        let mut trim = (EVEN_TRIM + falls) as i32 - rises as i32;
        if self.channels == 2 {
            let (left, right) = self.coefficients.split_at(MAX_FRAME);
            let summed: f32 = (0..CORRELATED_BANDS)
                .map(|band| {
                    let range = BANDS[band] << LM..BANDS[band + 1] << LM;
                    inner(&left[range.clone()], &right[range])
                })
                .sum();
            let alike = (summed / CORRELATED_BANDS as f32).abs();
            trim -= CORRELATED.iter().filter(|&&floor| alike > floor).count() as i32;
        }
        trim.clamp(0, (TRIM_ICDF.len() - 1) as i32) as usize
    }
}

/// The band intensity stereo starts at in frames of `bytes`, lower as fewer
/// bits pay for each channel's own shape (200 bytes is 80 kb/s).
const fn intensity_at(bytes: usize) -> usize {
    match bytes {
        200.. => BAND_COUNT,
        160..=199 => 20,
        120..=159 => 18,
        80..=119 => 16,
        _ => 12,
    }
}

/// How many of the [`PEAKED`] shares at least half of `shape`'s
/// coefficients fall under: all three for a tone's band, none for noise's.
fn peaks(shape: &[f32]) -> u32 {
    let width = shape.len() as f32;
    let mut under = [0usize; PEAKED.len()];
    for &value in shape {
        let level = value * value * width;
        for (count, &peak) in under.iter_mut().zip(&PEAKED) {
            *count += usize::from(level < peak);
        }
    }
    let half = shape.len().div_ceil(2);
    under.iter().filter(|&&count| count >= half).count() as u32
}

/// Each band's weight in the spreading: 32 where it is audible, halving for
/// each 6 dB the louder bands about it mask it by, to one. The masking falls
/// [`MASK_UP`] a band toward the high bands and [`MASK_DOWN`] toward the low,
/// and lies at most [`MASK_DEPTH`] under the loudest band.
fn audibility(energy: &[f32; 2 * BAND_COUNT], channels: usize) -> [u32; BAND_COUNT] {
    let level: [f32; BAND_COUNT] = core::array::from_fn(|band| {
        let louder = if channels == 2 {
            max16(energy[band], energy[BAND_COUNT + band])
        } else {
            energy[band]
        };
        louder - noise_floor(band)
    });
    let mut mask = level;
    for band in 1..BAND_COUNT {
        mask[band] = max16(mask[band], mask[band - 1] - MASK_UP);
    }
    for band in (0..BAND_COUNT - 1).rev() {
        mask[band] = max16(mask[band], mask[band + 1] - MASK_DOWN);
    }
    let deepest = max16(
        0.0,
        level.iter().copied().fold(level[0], max16) - MASK_DEPTH,
    );
    core::array::from_fn(|band| {
        let margin = level[band] - max16(deepest, mask[band]);
        let shift = (-mathf::round_i32(f64::from(margin))).clamp(0, 5);
        32u32 >> shift
    })
}

/// The energy a source of [`SOURCE_DEPTH`] bits leaves as noise in `band`:
/// half the band's `log2` width over the depth's floor, less its mean, with
/// the rise the pre-emphasis gives the high bands.
fn noise_floor(band: usize) -> f32 {
    let rise = 0.0062 * ((band + 5) * (band + 5)) as f32;
    f32::from(LOG_N[band]) / 16.0 + 9.5 - SOURCE_DEPTH - ENERGY_MEANS[band] + rise
}

/// The envelope a channel's band energies `level` hold over their
/// neighbours: rising at most [`ENVELOPE_UP`] a band toward the high bands
/// and [`ENVELOPE_DOWN`] toward the low, and no lower than the median of the
/// bands about each less [`MEDIAN_SLACK`], nor than the noise floor.
fn envelope(level: &[f32]) -> [f32; BAND_COUNT] {
    let mut follow = [0.0f32; BAND_COUNT];
    follow[0] = level[0];
    let mut highest = 0;
    for band in 1..BAND_COUNT {
        if level[band] > level[band - 1] + ENVELOPE_STEP {
            highest = band;
        }
        follow[band] = min16(follow[band - 1] + ENVELOPE_UP, level[band]);
    }
    for band in (0..highest).rev() {
        let from_above = min16(follow[band + 1] + ENVELOPE_DOWN, level[band]);
        follow[band] = min16(follow[band], from_above);
    }
    let last = BAND_COUNT - 1;
    let low = median::<3>(core::array::from_fn(|at| level[at]));
    let high = median::<3>(core::array::from_fn(|at| level[last - 2 + at]));
    for (band, value) in follow.iter_mut().enumerate() {
        let middle = match band {
            0 | 1 => low,
            _ if band + 1 >= last => high,
            _ => median::<5>(core::array::from_fn(|at| level[band + at - 2])),
        };
        *value = max16(max16(*value, middle - MEDIAN_SLACK), noise_floor(band));
    }
    follow
}

/// The middle of an odd count of `values`.
fn median<const N: usize>(mut values: [f32; N]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[N / 2]
}

/// The coefficients of one block of `input`, its length and the overlap, into
/// `out`, `folded` its scratch: the exact inverse of the decoder's inverse
/// transform and overlap-add, the window folded in at both ends.
fn mdct(dct: &mut Dct4, input: &[f32], folded: &mut [f32], out: &mut [f32]) {
    let len = dct.len();
    let half = OVERLAP / 2;
    for (j, slot) in folded[..len].iter_mut().rev().enumerate() {
        let value = if j < half {
            WINDOW[half + j] * input[half + j] - WINDOW[half - 1 - j] * input[half - 1 - j]
        } else if j < len - half {
            input[half + j]
        } else {
            let i = j - (len - half);
            WINDOW[OVERLAP - 1 - i] * input[len + i] + WINDOW[i] * input[len + OVERLAP - 1 - i]
        };
        *slot = -value;
    }
    dct.process(&folded[..len], &mut out[..len]);
    let scale = 2.0 / len as f32;
    for value in &mut out[..len] {
        *value *= scale;
    }
}

#[cfg(test)]
#[path = "encoder_tests.rs"]
mod tests;

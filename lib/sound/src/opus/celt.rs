//! CELT (RFC 6716, section 4.3): the MDCT layer, decoded at 48 kHz in frames
//! of 2.5 to 20 ms.
//!
//! The arithmetic is single-precision float throughout and follows the
//! reference implementation's float build operation for operation, so a
//! decode agrees with libopus's to within the rounding of the last bit.
//!
//! A frame's symbols are coded by one routine at either end, `code`: the
//! decoder reads them, and the encoder, behind `encode-opus`, writes the
//! choices its analysis made.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the reference's arithmetic, conversion for conversion: lengths and indices the format bounds far within 16 bits, and the narrowings and reinterpretations its C performs"
)]

mod alloc;
mod bands;
#[cfg(any(test, feature = "encode-opus"))]
mod encoder;
mod energy;
mod plc;
mod pvq;
mod tables;

use ::alloc::vec::Vec;

use tairix_util::fallible::filled;
use tairix_util::mathf;

use self::alloc::{caps, Request};
use self::bands::{anti_collapse, denormalise, Frame as BandFrame, Scratch};
#[cfg(any(test, feature = "encode-opus"))]
pub(crate) use self::encoder::CeltEncoder;
pub(super) use self::tables::WINDOW;
use self::tables::{BANDS, SPREAD_ICDF, TAPSET_ICDF, TF_SELECT, TRIM_ICDF};
use super::range::{Coder, RangeDecoder, BITRES};
use crate::transform::Dct4;
use crate::DecodeError;

/// The bands of the 48 kHz mode.
const BAND_COUNT: usize = 21;
/// The overlap between consecutive blocks.
const OVERLAP: usize = 120;
/// The shortest block's coefficients: 2.5 ms.
const SHORT_BLOCK: usize = 120;
/// The longest frame is `SHORT_BLOCK << MAX_LM`: 20 ms.
const MAX_LM: usize = 3;
/// The longest frame's samples.
pub(super) const MAX_FRAME: usize = SHORT_BLOCK << MAX_LM;
/// The decoded history kept for concealment and the post-filter, before the
/// overlap that runs into the next frame.
const HISTORY: usize = 2048;
const MAX_PERIOD: usize = 1024;
const LPC_ORDER: usize = 24;
const COMB_MIN_PERIOD: usize = 15;
const MAX_FINE_BITS: u32 = 8;
const PREEMPHASIS: f32 = 0.850_006_1;
const SPREAD_NONE: usize = 0;
const SPREAD_NORMAL: usize = 2;
const SPREAD_AGGRESSIVE: usize = 3;
/// The allocation's tilt that leans toward neither end.
const EVEN_TRIM: usize = 5;
const EPSILON: f32 = 1e-15;
const VERY_SMALL: f32 = 1e-30;
/// The scale CELT's signals are carried at: full scale is `32768`.
const SIGNAL_SCALE: f32 = 32768.0;
/// The energy a band of silence is held at.
const SILENT_ENERGY: f32 = -28.0;

const fn max16(a: f32, b: f32) -> f32 {
    if a > b {
        a
    } else {
        b
    }
}

const fn min16(a: f32, b: f32) -> f32 {
    if a < b {
        a
    } else {
        b
    }
}

fn sqrt(x: f32) -> f32 {
    mathf::sqrt(f64::from(x)) as f32
}

fn rsqrt(x: f32) -> f32 {
    1.0 / sqrt(x)
}

fn exp2(x: f32) -> f32 {
    mathf::exp(core::f64::consts::LN_2 * f64::from(x)) as f32
}

/// `cos(πx/2)`, the argument scaled in single precision as the reference
/// scales it.
fn cos_norm(x: f32) -> f32 {
    mathf::cos(f64::from(0.5 * core::f32::consts::PI * x)) as f32
}

/// How a frame reaches the decoder.
pub(super) enum Input<'r, 'f> {
    /// Lost or not sent: conceal it.
    Lost,
    /// Coded in the rest of a range decoder's frame, after whatever came
    /// before it.
    Shared(&'r mut RangeDecoder<'f>),
    /// Coded alone in these bytes.
    Alone(&'f [u8]),
}

/// The post-filter's parameters for one frame.
#[derive(Clone, Copy, Default)]
struct PostFilter {
    period: usize,
    gain: f32,
    tapset: usize,
}

/// The CELT decoder's state between frames.
pub(super) struct Celt {
    channels: usize,
    stream_channels: usize,
    start: usize,
    end: usize,
    disable_inversion: bool,
    rng: u32,
    last_pitch: usize,
    loss_duration: usize,
    skip_plc: bool,
    filter: PostFilter,
    filter_old: PostFilter,
    preemphasis: [f32; 2],
    /// Each channel's decoded signal: [`HISTORY`] samples, then the overlap
    /// running into the next frame.
    history: Vec<f32>,
    lpc: [[f32; LPC_ORDER]; 2],
    energy: [f32; 2 * BAND_COUNT],
    previous: [f32; 2 * BAND_COUNT],
    before: [f32; 2 * BAND_COUNT],
    background: [f32; 2 * BAND_COUNT],
}

/// One frame's scratch, kept so a frame allocates nothing: none of it
/// outlives the frame, so the streams of a packet, decoded one after
/// another, share one.
pub(super) struct Work {
    coefficients: Vec<f32>,
    freq: Vec<f32>,
    gathered: Vec<f32>,
    transformed: Vec<f32>,
    /// The DCT-IV of each block size, 2.5 to 20 ms.
    dct: Vec<Dct4>,
    bands: Scratch,
}

const HISTORY_LEN: usize = HISTORY + OVERLAP;

impl Work {
    pub(super) fn new() -> Result<Self, DecodeError> {
        let buffer = |len: usize| filled(len, 0.0f32).ok_or(DecodeError::OutOfMemory);
        let mut dct = Vec::new();
        dct.try_reserve_exact(MAX_LM + 1)
            .map_err(|_| DecodeError::OutOfMemory)?;
        for lm in 0..=MAX_LM {
            dct.push(Dct4::new(SHORT_BLOCK << lm).ok_or(DecodeError::OutOfMemory)?);
        }
        Ok(Self {
            coefficients: buffer(2 * MAX_FRAME)?,
            freq: buffer(2 * MAX_FRAME)?,
            gathered: buffer(MAX_FRAME)?,
            transformed: buffer(MAX_FRAME)?,
            dct,
            bands: Scratch::new()?,
        })
    }
}

impl Celt {
    /// A decoder for `channels` output channels, one or two.
    pub(super) fn new(channels: usize) -> Result<Self, DecodeError> {
        let mut celt = Self {
            channels,
            stream_channels: channels,
            start: 0,
            end: BAND_COUNT,
            disable_inversion: channels == 1,
            rng: 0,
            last_pitch: 0,
            loss_duration: 0,
            skip_plc: true,
            filter: PostFilter::default(),
            filter_old: PostFilter::default(),
            preemphasis: [0.0; 2],
            history: filled(channels * HISTORY_LEN, 0.0f32).ok_or(DecodeError::OutOfMemory)?,
            lpc: [[0.0; LPC_ORDER]; 2],
            energy: [0.0; 2 * BAND_COUNT],
            previous: [0.0; 2 * BAND_COUNT],
            before: [0.0; 2 * BAND_COUNT],
            background: [0.0; 2 * BAND_COUNT],
        };
        celt.reset();
        Ok(celt)
    }

    /// Forget every frame decoded, as a stream starting afresh.
    pub(super) fn reset(&mut self) {
        self.rng = 0;
        self.last_pitch = 0;
        self.loss_duration = 0;
        self.skip_plc = true;
        self.filter = PostFilter::default();
        self.filter_old = PostFilter::default();
        self.preemphasis = [0.0; 2];
        self.history.fill(0.0);
        self.lpc = [[0.0; LPC_ORDER]; 2];
        self.energy = [0.0; 2 * BAND_COUNT];
        self.previous = [SILENT_ENERGY; 2 * BAND_COUNT];
        self.before = [SILENT_ENERGY; 2 * BAND_COUNT];
        self.background = [0.0; 2 * BAND_COUNT];
    }

    /// Code from band `start`: a hybrid frame's CELT layer starts at 17.
    pub(super) fn set_start_band(&mut self, start: usize) {
        self.start = start;
    }

    /// Code up to band `end`, where the frame's bandwidth ends.
    pub(super) fn set_end_band(&mut self, end: usize) {
        self.end = end;
    }

    /// The channels the next frame codes, which a stereo stream may vary.
    pub(super) fn set_stream_channels(&mut self, channels: usize) {
        self.stream_channels = channels;
    }

    /// The range decoder's state after the last frame.
    #[cfg(test)]
    pub(super) const fn final_range(&self) -> u32 {
        self.rng
    }

    /// Decode a frame of `frame_size` samples a channel into `out`,
    /// interleaved; the frame is refused if it read past its end.
    pub(super) fn decode(
        &mut self,
        input: Input<'_, '_>,
        out: &mut [f32],
        frame_size: usize,
        work: &mut Work,
    ) -> Result<(), DecodeError> {
        let lm = (0..=MAX_LM)
            .find(|&lm| SHORT_BLOCK << lm == frame_size)
            .ok_or(DecodeError::OpusPacketInvalid)?;
        let mut own;
        let decoder = match input {
            Input::Shared(decoder) if decoder.storage() > 1 => decoder,
            Input::Alone(bytes) if bytes.len() > 1 => {
                own = RangeDecoder::new(bytes);
                &mut own
            }
            _ => {
                self.conceal(lm, work);
                self.deemphasise(out, frame_size);
                return Ok(());
            }
        };
        self.decode_coded(decoder, lm, work)?;
        self.deemphasise(out, frame_size);
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        clippy::many_single_char_names,
        reason = "the synthesis of the reference's celt_decode_with_ec, kept whole and in its names so the port can be read against it"
    )]
    fn decode_coded(
        &mut self,
        decoder: &mut RangeDecoder<'_>,
        lm: usize,
        work: &mut Work,
    ) -> Result<(), DecodeError> {
        let c = self.stream_channels;
        let (start, end) = (self.start, self.end);
        let m = 1usize << lm;
        let n = m * SHORT_BLOCK;
        let len = decoder.storage() as i32;
        self.skip_plc = self.loss_duration != 0;
        if c == 1 {
            for band in 0..BAND_COUNT {
                self.energy[band] = max16(self.energy[band], self.energy[BAND_COUNT + band]);
            }
        }
        for channel in 0..self.channels {
            let history = &mut self.history[channel * HISTORY_LEN..(channel + 1) * HISTORY_LEN];
            history.copy_within(n.., 0);
        }
        let span = Span {
            start,
            end,
            channels: c,
            lm,
            disable_inversion: self.disable_inversion,
        };
        let Coded {
            transient,
            silence,
            post,
            anti_collapse: anti_collapse_on,
            collapse,
            pulses,
        } = code(
            decoder,
            &span,
            &mut self.energy,
            &mut self.rng,
            &mut work.coefficients,
            &mut work.bands,
            &Aims::NONE,
        );
        if anti_collapse_on {
            anti_collapse(
                &mut work.coefficients,
                &collapse,
                lm,
                c,
                n,
                start..end,
                &self.energy,
                &self.previous,
                &self.before,
                &pulses,
                self.rng,
            );
        }
        self.synthesise(lm, transient, silence, c, work);

        let first = self.filter_old;
        for channel in 0..self.channels {
            let history = &mut self.history[channel * HISTORY_LEN..(channel + 1) * HISTORY_LEN];
            let at = HISTORY - n;
            comb_filter(
                &mut InPlace { buf: history, at },
                first,
                self.filter,
                SHORT_BLOCK,
                Some(&WINDOW),
            );
            if lm != 0 {
                comb_filter(
                    &mut InPlace {
                        buf: history,
                        at: at + SHORT_BLOCK,
                    },
                    self.filter,
                    post,
                    n - SHORT_BLOCK,
                    Some(&WINDOW),
                );
            }
        }
        self.filter_old = self.filter;
        self.filter = post;
        if lm != 0 {
            self.filter_old = post;
        }

        if c == 1 {
            let (first, second) = self.energy.split_at_mut(BAND_COUNT);
            second.copy_from_slice(first);
        }
        if transient {
            for (previous, &energy) in self.previous.iter_mut().zip(&self.energy) {
                *previous = min16(*previous, energy);
            }
        } else {
            self.before = self.previous;
            self.previous = self.energy;
        }
        let increase = (self.loss_duration + m).min(160) as f32 * 0.001;
        for (background, &energy) in self.background.iter_mut().zip(&self.energy) {
            *background = min16(*background + increase, energy);
        }
        for channel in 0..2 {
            let row = channel * BAND_COUNT;
            for band in (0..start).chain(end..BAND_COUNT) {
                self.energy[row + band] = 0.0;
                self.previous[row + band] = SILENT_ENERGY;
                self.before[row + band] = SILENT_ENERGY;
            }
        }
        self.rng = decoder.range();
        self.loss_duration = 0;
        if decoder.tell() > 8 * len {
            return Err(DecodeError::OpusPacketInvalid);
        }
        Ok(())
    }

    /// Turn the frame's coefficients into the history's newest samples.
    fn synthesise(
        &mut self,
        lm: usize,
        transient: bool,
        silence: bool,
        stream_channels: usize,
        work: &mut Work,
    ) {
        let n = SHORT_BLOCK << lm;
        let m = 1 << lm;
        let (blocks, block_len, dct) = if transient {
            (m, SHORT_BLOCK, 0)
        } else {
            (1, n, lm)
        };
        let (freq, freq2) = work.freq.split_at_mut(MAX_FRAME);
        let coefficients = &work.coefficients;
        let denormalised = |channel: usize, freq: &mut [f32]| {
            denormalise(
                &coefficients[channel * n..(channel + 1) * n],
                &mut freq[..n],
                &self.energy[channel * BAND_COUNT..],
                self.start,
                self.end,
                m,
                silence,
            );
        };
        let mut inverse = |freq: &[f32], channel: usize, history: &mut [f32]| {
            let out = &mut history[channel * HISTORY_LEN + HISTORY - n..];
            for block in 0..blocks {
                imdct(
                    &mut work.dct[dct],
                    &freq[block..],
                    blocks,
                    &mut out[block_len * block..],
                    &mut work.gathered,
                    &mut work.transformed,
                );
            }
        };
        match (self.channels, stream_channels) {
            (2, 1) => {
                denormalised(0, freq);
                inverse(freq, 0, &mut self.history);
                inverse(freq, 1, &mut self.history);
            }
            (1, 2) => {
                denormalised(0, freq);
                denormalised(1, freq2);
                for (a, &b) in freq[..n].iter_mut().zip(&freq2[..n]) {
                    *a = 0.5 * *a + 0.5 * b;
                }
                inverse(freq, 0, &mut self.history);
            }
            _ => {
                for channel in 0..self.channels {
                    denormalised(channel, freq);
                    inverse(freq, channel, &mut self.history);
                }
            }
        }
    }

    /// Write the history's newest `n` samples to `out`, interleaved, the
    /// pre-emphasis undone and scaled to unit full scale.
    fn deemphasise(&mut self, out: &mut [f32], n: usize) {
        let channels = self.channels;
        for channel in 0..channels {
            let history =
                &self.history[channel * HISTORY_LEN + HISTORY - n..channel * HISTORY_LEN + HISTORY];
            let mut memory = self.preemphasis[channel];
            for (sample, &x) in out[channel..].iter_mut().step_by(channels).zip(history) {
                let value = x + VERY_SMALL + memory;
                memory = PREEMPHASIS * value;
                *sample = value * (1.0 / SIGNAL_SCALE);
            }
            self.preemphasis[channel] = memory;
        }
    }
}

/// The inverse MDCT of one block of `coefficients[0], [stride], …` into
/// `out`, overlapped with the previous block's tail there: CELT's inverse is
/// the DCT-IV reversed and negated, folded by the window at its start.
fn imdct(
    dct: &mut Dct4,
    coefficients: &[f32],
    stride: usize,
    out: &mut [f32],
    gathered: &mut [f32],
    transformed: &mut [f32],
) {
    let len = dct.len();
    for (slot, &value) in gathered[..len]
        .iter_mut()
        .zip(coefficients.iter().step_by(stride))
    {
        *slot = value;
    }
    dct.process(&gathered[..len], &mut transformed[..len]);
    let half = OVERLAP / 2;
    for (slot, &value) in out[half..half + len]
        .iter_mut()
        .zip(transformed[..len].iter().rev())
    {
        *slot = -value;
    }
    for i in 0..half {
        let x1 = out[OVERLAP - 1 - i];
        let x2 = out[i];
        out[i] = WINDOW[OVERLAP - 1 - i] * x2 - WINDOW[i] * x1;
        out[OVERLAP - 1 - i] = WINDOW[i] * x2 + WINDOW[OVERLAP - 1 - i] * x1;
    }
}

/// What an encoder's analysis found a frame to hold.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Content {
    /// Nothing: the frame is coded as silence.
    Silence,
    /// Sound one long block codes.
    Steady,
    /// An onset, coded as short blocks.
    Onset,
}

/// What an encoder chose for a frame, which the shared coding writes; a
/// decoder, reading every value from the frame, hands it [`Aims::NONE`].
struct Aims<'a> {
    content: Content,
    /// Whether the energies are predicted within the frame alone.
    intra: bool,
    /// Each band's energy, which the coarse and fine energies aim at.
    energy: &'a [f32; 2 * BAND_COUNT],
    /// Each band's amplitude, which intensity stereo weighs its channels by.
    amplitudes: &'a [f32; 2 * BAND_COUNT],
    /// Each band's time-frequency flag.
    tf: [bool; BAND_COUNT],
    /// The first band coded as intensity stereo.
    intensity: usize,
    /// Whether the bands below it are coded as two mono channels.
    dual_stereo: bool,
    /// The spreading the bands' shapes are coded with.
    spread: usize,
    /// Each band's boost, in quanta of its dynamic allocation.
    boosts: [i32; BAND_COUNT],
    /// The allocation's tilt.
    trim: usize,
}

impl Aims<'static> {
    /// What a decoder hands the shared coding, which reads past it.
    const NONE: Self = Self {
        content: Content::Steady,
        intra: false,
        energy: &[0.0; 2 * BAND_COUNT],
        amplitudes: &[0.0; 2 * BAND_COUNT],
        tf: [false; BAND_COUNT],
        intensity: BAND_COUNT,
        dual_stereo: false,
        spread: SPREAD_NORMAL,
        boosts: [0; BAND_COUNT],
        trim: EVEN_TRIM,
    };
}

/// What a frame is coded over, alike at both ends.
struct Span {
    start: usize,
    end: usize,
    /// The channels the frame codes.
    channels: usize,
    lm: usize,
    disable_inversion: bool,
}

/// What a frame's coding decided, which a decoder synthesises it from.
struct Coded {
    transient: bool,
    silence: bool,
    post: PostFilter,
    anti_collapse: bool,
    /// Which short blocks each band's shape filled.
    collapse: [u8; 2 * BAND_COUNT],
    /// Each band's shape bits, in eighths.
    pulses: [i32; BAND_COUNT],
}

/// Band `band`'s coefficients across `channels` at block size `lm`, and the
/// eighths of a bit each of its boosts adds (RFC 6716, section 4.3.3): six
/// bits, but no more than one and no less than an eighth a coefficient.
fn boost_step(band: usize, channels: usize, lm: usize) -> (i32, i32) {
    let width = ((channels * (BANDS[band + 1] - BANDS[band])) << lm) as i32;
    (width, (width << BITRES).min((6 << BITRES).max(width)))
}

/// Code a frame's every symbol over `span`, at either end: the header's
/// flags, the energies into `energy`, the allocation, and the bands' shapes
/// into `coefficients`, each channel's after the last. An encoder writes
/// what `aims` holds and the shapes `coefficients` holds; a decoder reads
/// them.
#[allow(
    clippy::too_many_lines,
    reason = "the reference's celt_decode_with_ec up to its synthesis, kept whole so the port can be read against it"
)]
fn code<C: Coder>(
    coder: &mut C,
    span: &Span,
    energy: &mut [f32; 2 * BAND_COUNT],
    seed: &mut u32,
    coefficients: &mut [f32],
    scratch: &mut Scratch,
    aims: &Aims<'_>,
) -> Coded {
    let Span {
        start,
        end,
        channels: c,
        lm,
        disable_inversion,
    } = *span;
    let n = SHORT_BLOCK << lm;
    let len = coder.storage() as i32;
    let mut total_bits = len * 8;
    let mut tell = coder.tell();
    let silence = if tell >= total_bits {
        true
    } else if tell == 1 {
        coder.bit_logp(aims.content == Content::Silence, 15)
    } else {
        false
    };
    if silence {
        tell = len * 8;
        coder.skip_to(tell);
    }
    let mut post = PostFilter::default();
    if start == 0 && tell + 16 <= total_bits {
        // The encoder runs no pitch pre-filter.
        if coder.bit_logp(false, 1) {
            let octave = coder.uint(0, 6);
            post.period = ((16 << octave) + coder.bits(0, 4 + octave) - 1) as usize;
            let gain = coder.bits(0, 3);
            if coder.tell() + 2 <= total_bits {
                post.tapset = coder.icdf(0, &TAPSET_ICDF, 2);
            }
            post.gain = 0.093_75 * (gain + 1) as f32;
        }
        tell = coder.tell();
    }
    let transient = lm > 0 && tell + 3 <= total_bits && {
        let transient = coder.bit_logp(aims.content == Content::Onset, 3);
        tell = coder.tell();
        transient
    };
    let intra = tell + 3 <= total_bits && coder.bit_logp(aims.intra, 3);
    energy::coarse(coder, energy, aims.energy, start..end, intra, c, lm);

    let tf = tf_changes(coder, &aims.tf, start..end, transient, lm);
    let spread = if coder.tell() + 4 <= total_bits {
        coder.icdf(aims.spread, &SPREAD_ICDF, 5)
    } else {
        SPREAD_NORMAL
    };
    let caps = caps(lm, c);
    let mut offsets = [0i32; BAND_COUNT];
    let mut dynalloc_logp = 6;
    total_bits <<= BITRES;
    let mut tell = coder.tell_frac();
    for band in start..end {
        let (_, quanta) = boost_step(band, c, lm);
        let mut loop_logp = dynalloc_logp;
        let mut boost = 0;
        while tell + ((loop_logp as i32) << BITRES) < total_bits && boost < caps[band] {
            let more = coder.bit_logp(boost < quanta * aims.boosts[band], loop_logp);
            tell = coder.tell_frac();
            if !more {
                break;
            }
            boost += quanta;
            total_bits -= quanta;
            loop_logp = 1;
        }
        offsets[band] = boost;
        if boost > 0 {
            dynalloc_logp = 2.max(dynalloc_logp - 1);
        }
    }
    let trim = if tell + (6 << BITRES) <= total_bits {
        coder.icdf(aims.trim, &TRIM_ICDF, 7) as i32
    } else {
        EVEN_TRIM as i32
    };
    let mut bits = ((len * 8) << BITRES) - coder.tell_frac() - 1;
    let anti_collapse_rsv = if transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
        1 << BITRES
    } else {
        0
    };
    bits -= anti_collapse_rsv;
    let allocation = alloc::compute(
        &Request {
            start,
            end,
            offsets: &offsets,
            caps: &caps,
            trim,
            total: bits,
            channels: c,
            lm,
            intensity: aims.intensity,
            dual_stereo: aims.dual_stereo,
        },
        coder,
    );
    energy::fine(coder, energy, aims.energy, start..end, &allocation.fine, c);
    let (x, y) = coefficients.split_at_mut(n);
    let y = (c == 2).then_some(&mut y[..n]);
    let mut collapse = [0u8; 2 * BAND_COUNT];
    bands::all(
        coder,
        &BandFrame {
            start,
            end,
            lm,
            short_blocks: transient,
            spread,
            dual_stereo: allocation.dual_stereo,
            intensity: allocation.intensity,
            tf: &tf,
            pulses: &allocation.pulses,
            coded: allocation.coded,
            total_bits: len * (8 << BITRES) - anti_collapse_rsv,
            balance: allocation.balance,
            disable_inversion,
            amplitudes: aims.amplitudes,
        },
        &mut x[..n],
        y,
        &mut collapse,
        seed,
        scratch,
    );
    // The encoder has anti-collapse fill whatever short blocks the shapes
    // left empty.
    let anti_collapse = anti_collapse_rsv > 0 && coder.bits(1, 1) != 0;
    let left = len * 8 - coder.tell();
    energy::finalise(coder, energy, aims.energy, start..end, &allocation, left, c);
    if silence {
        energy[..c * BAND_COUNT].fill(SILENT_ENERGY);
    }
    Coded {
        transient,
        silence,
        post,
        anti_collapse,
        collapse,
        pulses: allocation.pulses,
    }
}

/// Code the time-frequency resolution changes of bands `bands`: an encoder
/// writes the flags `aim` holds.
fn tf_changes<C: Coder>(
    coder: &mut C,
    aim: &[bool; BAND_COUNT],
    bands: core::ops::Range<usize>,
    transient: bool,
    lm: usize,
) -> [i32; BAND_COUNT] {
    let mut budget = coder.storage() as i32 * 8;
    let mut tell = coder.tell();
    let mut logp = if transient { 2 } else { 4 };
    let select_rsv = lm > 0 && tell + logp < budget;
    budget -= i32::from(select_rsv);
    let mut changed = 0;
    let mut current = 0;
    let mut flags = [0usize; BAND_COUNT];
    for band in bands.clone() {
        if tell + logp <= budget {
            let change = aim[band] != (current != 0);
            current ^= usize::from(coder.bit_logp(change, logp as u32));
            tell = coder.tell();
            changed |= current;
        }
        flags[band] = current;
        logp = if transient { 4 } else { 5 };
    }
    let row = &TF_SELECT[lm];
    let base = 4 * usize::from(transient);
    // The encoder keeps the first of each resolution's two tables.
    let select =
        select_rsv && row[base + changed] != row[base + 2 + changed] && coder.bit_logp(false, 1);
    let mut tf = [0i32; BAND_COUNT];
    for band in bands {
        tf[band] = i32::from(row[base + 2 * usize::from(select) + flags[band]]);
    }
    tf
}

/// A signal the post-filter reads its past from and writes its output to.
trait Signal {
    fn get(&self, at: isize) -> f32;
    fn set(&mut self, at: usize, value: f32);
}

/// Filtering in place: the past read is the filter's own output, which makes
/// the decoder's post-filter recursive.
struct InPlace<'b> {
    buf: &'b mut [f32],
    at: usize,
}

impl Signal for InPlace<'_> {
    fn get(&self, at: isize) -> f32 {
        self.buf[(self.at as isize + at) as usize]
    }

    fn set(&mut self, at: usize, value: f32) {
        self.buf[self.at + at] = value;
    }
}

/// Filtering a signal into a separate output.
struct Separate<'b> {
    input: &'b [f32],
    at: usize,
    output: &'b mut [f32],
}

impl Signal for Separate<'_> {
    fn get(&self, at: isize) -> f32 {
        self.input[(self.at as isize + at) as usize]
    }

    fn set(&mut self, at: usize, value: f32) {
        self.output[at] = value;
    }
}

/// The pitch post-filter over `n` samples, cross-fading from `from` to `to`
/// over the overlap when the two differ.
fn comb_filter(
    signal: &mut impl Signal,
    from: PostFilter,
    to: PostFilter,
    n: usize,
    window: Option<&[f32; OVERLAP]>,
) {
    use self::tables::COMB_TAPS;
    if from.gain == 0.0 && to.gain == 0.0 {
        for i in 0..n {
            signal.set(i, signal.get(i as isize));
        }
        return;
    }
    let t0 = from.period.max(COMB_MIN_PERIOD) as isize;
    let t1 = to.period.max(COMB_MIN_PERIOD) as isize;
    let g0 = COMB_TAPS[from.tapset].map(|tap| from.gain * tap);
    let g1 = COMB_TAPS[to.tapset].map(|tap| to.gain * tap);
    let mut x1 = signal.get(-t1 + 1);
    let mut x2 = signal.get(-t1);
    let mut x3 = signal.get(-t1 - 1);
    let mut x4 = signal.get(-t1 - 2);
    #[allow(
        clippy::float_cmp,
        reason = "the reference compares the gains exactly, as both come from the same frame's taps"
    )]
    let unchanged = from.gain == to.gain && t0 == t1 && from.tapset == to.tapset;
    let overlap = match window {
        Some(_) if !unchanged => OVERLAP.min(n),
        _ => 0,
    };
    for i in 0..overlap {
        let w = window.map_or(0.0, |window| window[i]);
        let f = w * w;
        let at = i as isize;
        let x0 = signal.get(at - t1 + 2);
        let value = signal.get(at)
            + ((1.0 - f) * g0[0]) * signal.get(at - t0)
            + ((1.0 - f) * g0[1]) * (signal.get(at - t0 + 1) + signal.get(at - t0 - 1))
            + ((1.0 - f) * g0[2]) * (signal.get(at - t0 + 2) + signal.get(at - t0 - 2))
            + (f * g1[0]) * x2
            + (f * g1[1]) * (x1 + x3)
            + (f * g1[2]) * (x0 + x4);
        signal.set(i, value);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
    if to.gain == 0.0 {
        for i in overlap..n {
            signal.set(i, signal.get(i as isize));
        }
        return;
    }
    let base = overlap as isize;
    let mut x4 = signal.get(base - t1 - 2);
    let mut x3 = signal.get(base - t1 - 1);
    let mut x2 = signal.get(base - t1);
    let mut x1 = signal.get(base - t1 + 1);
    for i in overlap..n {
        let at = i as isize;
        let x0 = signal.get(at - t1 + 2);
        let value = signal.get(at) + g1[0] * x2 + g1[1] * (x1 + x3) + g1[2] * (x0 + x4);
        signal.set(i, value);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
}

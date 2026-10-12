//! SILK (RFC 6716, section 4.2): the linear-prediction layer, decoded at an
//! internal 8, 12 or 16 kHz in 10 or 20 ms frames, three to a 60 ms packet,
//! and resampled to 48 kHz.
//!
//! SILK is defined in fixed point and decoded here exactly as the reference
//! decodes it, so its output is bit-exact.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the reference's arithmetic, conversion for conversion: lengths and indices the format bounds far within 16 bits, and the narrowings and reinterpretations its C performs"
)]

mod excitation;
mod fixed;
mod lpc;
mod plc;
mod resample;
mod stereo;
mod synthesis;
mod tables;

use self::fixed::{lshift, rshift_round, smulww};
use self::lpc::{Codebook, NARROW, WIDE};
use self::plc::{Cng, Plc};
use self::resample::Resampler;
use self::stereo::Stereo;
use self::tables::{
    LBRR_FLAGS_2_ICDF, LBRR_FLAGS_3_ICDF, PITCH_CONTOUR_10_MS_ICDF, PITCH_CONTOUR_10_MS_NB_ICDF,
    PITCH_CONTOUR_ICDF, PITCH_CONTOUR_NB_ICDF, UNIFORM4_ICDF, UNIFORM6_ICDF, UNIFORM8_ICDF,
};
use super::range::RangeDecoder;
use crate::DecodeError;

const MAX_LPC_ORDER: usize = 16;
const MIN_LPC_ORDER: usize = 10;
const MAX_NB_SUBFR: usize = 4;
const MAX_FS_KHZ: usize = 16;
const SUB_FRAME_LENGTH_MS: usize = 5;
const MAX_SUB_FRAME_LENGTH: usize = SUB_FRAME_LENGTH_MS * MAX_FS_KHZ;
const MAX_FRAME_LENGTH: usize = MAX_SUB_FRAME_LENGTH * MAX_NB_SUBFR;
const LTP_MEM_LENGTH_MS: usize = 20;
const LTP_ORDER: usize = 5;
const MAX_FRAMES_PER_PACKET: usize = 3;
/// The output rate.
const API_RATE: i32 = 48_000;
/// A 20 ms frame's samples at 48 kHz.
pub(super) const MAX_OUTPUT: usize = 960;

const TYPE_NO_VOICE_ACTIVITY: i32 = 0;
const TYPE_VOICED: i32 = 2;

/// How a frame's side information was coded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Coding {
    Independently,
    IndependentlyNoLtpScaling,
    Conditionally,
}

/// What a frame's decoder is asked to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Loss {
    /// Decode the frame.
    None,
    /// Conceal a frame that never arrived.
    Lost,
}

/// One frame's coded side information.
#[derive(Clone, Copy, Default)]
struct SideInfo {
    gains: [i8; MAX_NB_SUBFR],
    ltp: [i8; MAX_NB_SUBFR],
    nlsf: [i8; MAX_LPC_ORDER + 1],
    lag: i32,
    contour: i8,
    signal_type: i32,
    quant_offset: i32,
    nlsf_interpolation_q2: i32,
    periodicity: usize,
    ltp_scale: usize,
    seed: i32,
}

/// One frame's decoded parameters.
#[derive(Clone, Copy)]
struct Control {
    pitch: [i32; MAX_NB_SUBFR],
    gains_q16: [i32; MAX_NB_SUBFR],
    lpc_q12: [[i16; MAX_LPC_ORDER]; 2],
    ltp_q14: [i16; LTP_ORDER * MAX_NB_SUBFR],
    ltp_scale_q14: i32,
}

impl Control {
    const fn new() -> Self {
        Self {
            pitch: [0; MAX_NB_SUBFR],
            gains_q16: [0; MAX_NB_SUBFR],
            lpc_q12: [[0; MAX_LPC_ORDER]; 2],
            ltp_q14: [0; LTP_ORDER * MAX_NB_SUBFR],
            ltp_scale_q14: 0,
        }
    }
}

/// One channel's decoder state: the mid or the side of a stereo stream.
#[derive(Clone)]
struct Channel {
    prev_gain_q16: i32,
    excitation_q14: [i32; MAX_FRAME_LENGTH],
    lpc_state_q14: [i32; MAX_LPC_ORDER],
    out: [i16; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
    lag_prev: i32,
    last_gain_index: i32,
    fs_khz: usize,
    nb_subfr: usize,
    frame_length: usize,
    subfr_length: usize,
    ltp_mem_length: usize,
    lpc_order: usize,
    prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    first_frame_after_reset: bool,
    lag_low_icdf: &'static [u8],
    contour_icdf: &'static [u8],
    frames_decoded: usize,
    frames_per_packet: usize,
    ec_prev_signal_type: i32,
    ec_prev_lag: i32,
    vad: [bool; MAX_FRAMES_PER_PACKET],
    lbrr: bool,
    lbrr_frames: [bool; MAX_FRAMES_PER_PACKET],
    resampler: Resampler,
    codebook: &'static Codebook,
    indices: SideInfo,
    cng: Cng,
    loss_count: i32,
    prev_signal_type: i32,
    plc: Plc,
}

impl Channel {
    fn new() -> Self {
        let mut channel = Self {
            prev_gain_q16: 65536,
            excitation_q14: [0; MAX_FRAME_LENGTH],
            lpc_state_q14: [0; MAX_LPC_ORDER],
            out: [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
            lag_prev: 0,
            last_gain_index: 0,
            fs_khz: 0,
            nb_subfr: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            first_frame_after_reset: true,
            lag_low_icdf: &UNIFORM8_ICDF,
            contour_icdf: &PITCH_CONTOUR_ICDF,
            frames_decoded: 0,
            frames_per_packet: 0,
            ec_prev_signal_type: 0,
            ec_prev_lag: 0,
            vad: [false; MAX_FRAMES_PER_PACKET],
            lbrr: false,
            lbrr_frames: [false; MAX_FRAMES_PER_PACKET],
            resampler: Resampler::default(),
            codebook: &NARROW,
            indices: SideInfo::default(),
            cng: Cng::new(),
            loss_count: 0,
            prev_signal_type: 0,
            plc: Plc::new(),
        };
        channel.reset_cng();
        channel.reset_plc();
        channel
    }

    /// Set the internal rate and the frame's subframes.
    fn set_rate(&mut self, fs_khz: usize) {
        self.subfr_length = SUB_FRAME_LENGTH_MS * fs_khz;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz {
            self.resampler = Resampler::new(fs_khz);
        }
        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            self.contour_icdf = match (fs_khz == 8, self.nb_subfr == MAX_NB_SUBFR) {
                (true, true) => &PITCH_CONTOUR_NB_ICDF,
                (true, false) => &PITCH_CONTOUR_10_MS_NB_ICDF,
                (false, true) => &PITCH_CONTOUR_ICDF,
                (false, false) => &PITCH_CONTOUR_10_MS_ICDF,
            };
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = LTP_MEM_LENGTH_MS * fs_khz;
                if fs_khz == 16 {
                    self.lpc_order = MAX_LPC_ORDER;
                    self.codebook = &WIDE;
                    self.lag_low_icdf = &UNIFORM8_ICDF;
                } else {
                    self.lpc_order = MIN_LPC_ORDER;
                    self.codebook = &NARROW;
                    self.lag_low_icdf = if fs_khz == 12 {
                        &UNIFORM6_ICDF
                    } else {
                        &UNIFORM4_ICDF
                    };
                }
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
                self.out.fill(0);
                self.lpc_state_q14.fill(0);
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }

    /// Decode or conceal one frame into `out`, `frame_length` samples.
    fn decode_frame(
        &mut self,
        decoder: &mut RangeDecoder<'_>,
        out: &mut [i16],
        loss: Loss,
        coding: Coding,
    ) {
        let length = self.frame_length;
        let mut control = Control::new();
        if loss == Loss::None {
            self.decode_indices(decoder, self.frames_decoded, false, coding);
            let mut pulses = [0i16; MAX_FRAME_LENGTH];
            excitation::decode_pulses(
                decoder,
                &mut pulses,
                self.indices.signal_type,
                self.indices.quant_offset,
                length,
            );
            self.decode_parameters(&mut control, coding);
            self.synthesise(&mut control, &mut out[..length], &pulses);
            self.update_plc(&control);
            self.loss_count = 0;
            self.prev_signal_type = self.indices.signal_type;
            self.first_frame_after_reset = false;
        } else {
            self.conceal(&mut control, &mut out[..length]);
        }
        let moved = self.ltp_mem_length - length;
        self.out.copy_within(length..length + moved, 0);
        self.out[moved..moved + length].copy_from_slice(&out[..length]);
        self.comfort_noise(&control, &mut out[..length]);
        self.glue_frames(&mut out[..length]);
        self.lag_prev = control.pitch[self.nb_subfr - 1];
    }

    /// Turn the coded indices into gains, predictors and pitch.
    fn decode_parameters(&mut self, control: &mut Control, coding: Coding) {
        excitation::dequantise_gains(
            &mut control.gains_q16,
            self.indices.gains,
            &mut self.last_gain_index,
            coding == Coding::Conditionally,
            self.nb_subfr,
        );
        let order = self.lpc_order;
        let nlsf = self.codebook.decode(&self.indices.nlsf);
        control.lpc_q12[1] = lpc::nlsf_to_lpc(&nlsf[..order]);
        if self.first_frame_after_reset {
            self.indices.nlsf_interpolation_q2 = 4;
        }
        if self.indices.nlsf_interpolation_q2 < 4 {
            let mut interpolated = [0i16; MAX_LPC_ORDER];
            for i in 0..order {
                let previous = i32::from(self.prev_nlsf_q15[i]);
                let step =
                    (self.indices.nlsf_interpolation_q2 * (i32::from(nlsf[i]) - previous)) >> 2;
                interpolated[i] = (previous + step) as i16;
            }
            control.lpc_q12[0] = lpc::nlsf_to_lpc(&interpolated[..order]);
        } else {
            control.lpc_q12[0] = control.lpc_q12[1];
        }
        self.prev_nlsf_q15[..order].copy_from_slice(&nlsf[..order]);
        if self.loss_count != 0 {
            const BWE_AFTER_LOSS_Q16: i32 = 63570;
            lpc::bandwidth_expand(&mut control.lpc_q12[0][..order], BWE_AFTER_LOSS_Q16);
            lpc::bandwidth_expand(&mut control.lpc_q12[1][..order], BWE_AFTER_LOSS_Q16);
        }
        if self.indices.signal_type == TYPE_VOICED {
            excitation::decode_pitch(
                self.indices.lag,
                self.indices.contour,
                &mut control.pitch,
                self.fs_khz,
                self.nb_subfr,
            );
            let codebook = excitation::ltp_codebook(self.indices.periodicity);
            for k in 0..self.nb_subfr {
                let row = codebook
                    .get(usize::from(self.indices.ltp[k] as u8))
                    .copied()
                    .unwrap_or([0; LTP_ORDER]);
                for (i, &tap) in row.iter().enumerate() {
                    control.ltp_q14[k * LTP_ORDER + i] = lshift(i32::from(tap), 7) as i16;
                }
            }
            control.ltp_scale_q14 = i32::from(tables::LTP_SCALES_Q14[self.indices.ltp_scale]);
        } else {
            control.pitch = [0; MAX_NB_SUBFR];
            control.ltp_q14 = [0; LTP_ORDER * MAX_NB_SUBFR];
            self.indices.periodicity = 0;
            control.ltp_scale_q14 = 0;
        }
    }
}

/// The SILK decoder: up to two channels, mid and side.
pub(super) struct Silk {
    channels: [Channel; 2],
    stereo: Stereo,
    channels_api: usize,
    channels_internal: usize,
    prev_decode_only_middle: bool,
}

/// The rate SILK runs at internally for one packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InternalRate {
    Narrow,
    Medium,
    Wide,
}

impl InternalRate {
    const fn khz(self) -> usize {
        match self {
            Self::Narrow => 8,
            Self::Medium => 12,
            Self::Wide => 16,
        }
    }
}

/// What one call decodes.
pub(super) struct Request {
    /// Conceal rather than decode.
    pub(super) lost: bool,
    /// The first call for a new packet.
    pub(super) first: bool,
    /// The packet's duration, 10 to 60 ms.
    pub(super) payload_ms: usize,
    pub(super) rate: InternalRate,
    /// The channels the packet codes.
    pub(super) stream_channels: usize,
}

impl Silk {
    /// A decoder for `channels` output channels.
    pub(super) fn new(channels: usize) -> Self {
        Self {
            channels: [Channel::new(), Channel::new()],
            stereo: Stereo::default(),
            channels_api: channels,
            channels_internal: 0,
            prev_decode_only_middle: false,
        }
    }

    /// Forget every frame decoded.
    pub(super) fn reset(&mut self) {
        self.channels = [Channel::new(), Channel::new()];
        self.stereo = Stereo::default();
        self.prev_decode_only_middle = false;
    }

    /// Decode one 10 or 20 ms frame of the packet into `out`, interleaved at
    /// 48 kHz; the samples a channel written.
    #[allow(
        clippy::too_many_lines,
        reason = "the reference's silk_Decode, kept whole so the port can be read against it"
    )]
    pub(super) fn decode(
        &mut self,
        request: &Request,
        decoder: &mut RangeDecoder<'_>,
        out: &mut [i16],
    ) -> Result<usize, DecodeError> {
        let internal = request.stream_channels;
        let loss = if request.lost { Loss::Lost } else { Loss::None };
        if request.first {
            for channel in &mut self.channels[..internal] {
                channel.frames_decoded = 0;
            }
        }
        if internal > self.channels_internal {
            self.channels[1] = Channel::new();
        }
        let stereo_to_mono = internal == 1
            && self.channels_internal == 2
            && request.rate.khz() == self.channels[0].fs_khz;
        if self.channels[0].frames_decoded == 0 {
            let (frames, subframes) = match request.payload_ms {
                0 | 10 => (1, 2),
                20 => (1, 4),
                40 => (2, 4),
                60 => (3, 4),
                _ => return Err(DecodeError::OpusPacketInvalid),
            };
            for channel in &mut self.channels[..internal] {
                channel.frames_per_packet = frames;
                channel.nb_subfr = subframes;
                channel.set_rate(request.rate.khz());
            }
        }
        if self.channels_api == 2 && internal == 2 && self.channels_internal == 1 {
            self.stereo.reset_prediction();
            self.channels[1].resampler = self.channels[0].resampler.clone();
        }
        self.channels_internal = internal;

        if !request.lost && self.channels[0].frames_decoded == 0 {
            self.decode_flags(decoder, internal);
        }
        let current = self.channels[0].frames_decoded;
        let mut prediction = [0i32; 2];
        let mut decode_only_middle = false;
        if internal == 2 {
            if loss == Loss::None {
                prediction = stereo::decode_prediction(decoder);
                if !self.channels[1].vad[current] {
                    decode_only_middle = stereo::decode_mid_only(decoder);
                }
            } else {
                prediction = self.stereo.previous_prediction();
            }
        }
        if internal == 2 && !decode_only_middle && self.prev_decode_only_middle {
            let side = &mut self.channels[1];
            side.out.fill(0);
            side.lpc_state_q14.fill(0);
            side.lag_prev = 100;
            side.last_gain_index = 10;
            side.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
            side.first_frame_after_reset = true;
        }
        let has_side = if loss == Loss::None {
            !decode_only_middle
        } else {
            !self.prev_decode_only_middle
        };
        let frame_length = self.channels[0].frame_length;
        let mut buffers = [[0i16; MAX_FRAME_LENGTH + 2]; 2];
        let mut samples = 0;
        for (n, buffer) in buffers.iter_mut().enumerate().take(internal) {
            if n == 0 || has_side {
                let index = self.channels[0].frames_decoded as isize - n as isize;
                let coding = if index <= 0 {
                    Coding::Independently
                } else if n > 0 && self.prev_decode_only_middle {
                    Coding::IndependentlyNoLtpScaling
                } else {
                    Coding::Conditionally
                };
                self.channels[n].decode_frame(decoder, &mut buffer[2..], loss, coding);
                samples = self.channels[n].frame_length;
            } else {
                buffer[2..2 + samples].fill(0);
            }
            self.channels[n].frames_decoded += 1;
        }
        let fs_khz = self.channels[0].fs_khz;
        let (mid, side) = buffers.split_at_mut(1);
        if self.channels_api == 2 && internal == 2 {
            self.stereo
                .ms_to_lr(&mut mid[0], &mut side[0], prediction, fs_khz, frame_length);
        } else {
            self.stereo.buffer_mid(&mut mid[0], frame_length);
        }
        let out_len = frame_length * API_RATE as usize / (fs_khz * 1000);
        let api = self.channels_api;
        let mut resampled = [0i16; MAX_OUTPUT];
        for n in 0..api.min(internal) {
            self.channels[n]
                .resampler
                .process(&mut resampled[..out_len], &buffers[n][1..=frame_length]);
            for (i, &value) in resampled[..out_len].iter().enumerate() {
                out[n + api * i] = value;
            }
        }
        if api == 2 && internal == 1 {
            if stereo_to_mono {
                self.channels[1]
                    .resampler
                    .process(&mut resampled[..out_len], &buffers[0][1..=frame_length]);
                for (i, &value) in resampled[..out_len].iter().enumerate() {
                    out[1 + 2 * i] = value;
                }
            } else {
                for i in 0..out_len {
                    out[1 + 2 * i] = out[2 * i];
                }
            }
        }
        if loss == Loss::Lost {
            for channel in &mut self.channels[..self.channels_internal] {
                channel.last_gain_index = 10;
            }
        } else {
            self.prev_decode_only_middle = decode_only_middle;
        }
        Ok(out_len)
    }

    /// The packet's voice activity and redundancy flags, then its redundant
    /// frames, which a decode that has every packet skips.
    fn decode_flags(&mut self, decoder: &mut RangeDecoder<'_>, internal: usize) {
        for channel in &mut self.channels[..internal] {
            for vad in &mut channel.vad[..channel.frames_per_packet] {
                *vad = decoder.bit_logp(1);
            }
            channel.lbrr = decoder.bit_logp(1);
        }
        for channel in &mut self.channels[..internal] {
            channel.lbrr_frames = [false; MAX_FRAMES_PER_PACKET];
            if channel.lbrr {
                if channel.frames_per_packet == 1 {
                    channel.lbrr_frames[0] = true;
                } else {
                    let icdf: &[u8] = if channel.frames_per_packet == 2 {
                        &LBRR_FLAGS_2_ICDF
                    } else {
                        &LBRR_FLAGS_3_ICDF
                    };
                    let symbol = decoder.icdf(icdf, 8) + 1;
                    for (i, flag) in channel.lbrr_frames[..channel.frames_per_packet]
                        .iter_mut()
                        .enumerate()
                    {
                        *flag = (symbol >> i) & 1 != 0;
                    }
                }
            }
        }
        for i in 0..self.channels[0].frames_per_packet {
            for n in 0..internal {
                if !self.channels[n].lbrr_frames[i] {
                    continue;
                }
                if internal == 2 && n == 0 {
                    stereo::decode_prediction(decoder);
                    if !self.channels[1].lbrr_frames[i] {
                        stereo::decode_mid_only(decoder);
                    }
                }
                let coding = if i > 0 && self.channels[n].lbrr_frames[i - 1] {
                    Coding::Conditionally
                } else {
                    Coding::Independently
                };
                let channel = &mut self.channels[n];
                channel.decode_indices(decoder, i, true, coding);
                let mut pulses = [0i16; MAX_FRAME_LENGTH];
                excitation::decode_pulses(
                    decoder,
                    &mut pulses,
                    channel.indices.signal_type,
                    channel.indices.quant_offset,
                    channel.frame_length,
                );
            }
        }
    }
}

/// A Q14 LPC sample scaled by a Q10 gain to 16-bit output.
const fn output(sample_q14: i32, gain_q10: i32) -> i16 {
    fixed::sat16(rshift_round(smulww(sample_q14, gain_q10), 8))
}

/// The short-term prediction of `state[at]` from the `order` samples
/// before it, in Q10, each tap accumulated in the reference's order.
fn lpc_prediction(state: &[i32], at: usize, lpc_q12: &[i16], order: usize) -> i32 {
    let mut prediction = (order >> 1) as i32;
    for (j, &coefficient) in lpc_q12[..order].iter().enumerate() {
        prediction = fixed::smlawb(prediction, state[at - 1 - j], i32::from(coefficient));
    }
    prediction
}

//! The Opus encoder against the tree's own decoder, whose conformance RFC
//! 8251's vectors established: every packet read to the encoder's own range
//! state; tones, noise, stereo and an onset each within a floor of their
//! signal; silence as silence; an Ogg stream of the packets playing in time
//! with its input; and every bound refused.

#![allow(
    clippy::cast_precision_loss,
    reason = "test signals and counts far within single precision"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{EncodeError, Encoder, FRAME_SAMPLES, MAX_BITRATE, MIN_BITRATE, PRE_SKIP};
use crate::ogg::PageWriter;
use crate::opus::{Bandwidth, Layout, Mode, Multistream, Packet, Toc};
use crate::{DecodeLimits, Encoding, PcmSource, SoundFormat};

/// A tone of `hz` at `amplitude`, at sample `index`.
fn tone(hz: f32, amplitude: f32, index: usize) -> f32 {
    amplitude * (core::f32::consts::TAU * hz * index as f32 / 48_000.0).sin()
}

/// White noise, each sample within `amplitude` of zero.
struct Noise(u64);

impl Noise {
    fn draw(&mut self, amplitude: f32) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        amplitude * ((self.0 >> 40) as f32 / 8_388_608.0 - 1.0)
    }
}

/// `len` samples of `noise` within `amplitude`, low-passed at 17 kHz by an
/// 81-tap Blackman-windowed sinc that leaves under -90 dB of their power above
/// 20 kHz, where CELT codes nothing: white noise holds a sixth of its power
/// there, which caps any decode of it at `10·log10(6)` ≈ 7.8 dB.
fn band_limited(noise: &mut Noise, amplitude: f32, len: usize) -> Vec<f32> {
    const HALF: usize = 40;
    const CUTOFF: f32 = 17_000.0 / 48_000.0;
    let kernel: Vec<f32> = (0..=2 * HALF)
        .map(|index| {
            let turn = core::f32::consts::TAU * index as f32 / (2 * HALF) as f32;
            let window = 0.42 - 0.5 * turn.cos() + 0.08 * (2.0 * turn).cos();
            let arc = core::f32::consts::TAU * CUTOFF * (index as f32 - HALF as f32);
            let sinc = if index == HALF { 1.0 } else { arc.sin() / arc };
            2.0 * CUTOFF * sinc * window
        })
        .collect();
    let white: Vec<f32> = core::iter::repeat_with(|| noise.draw(amplitude))
        .take(len + 2 * HALF)
        .collect();
    white
        .windows(kernel.len())
        .map(|span| {
            span.iter()
                .zip(&kernel)
                .map(|(sample, weight)| sample * weight)
                .sum::<f32>()
        })
        .collect()
}

/// `frames` frames of `channels` channels, channel `channel`'s sample
/// `index` being `signal(channel, index)`.
fn frames_of(
    channels: usize,
    frames: usize,
    mut signal: impl FnMut(usize, usize) -> f32,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(channels * frames * FRAME_SAMPLES);
    for index in 0..frames * FRAME_SAMPLES {
        for channel in 0..channels {
            out.push(signal(channel, index));
        }
    }
    out
}

/// `input` coded at `bitrate` and decoded by the tree's decoder, every
/// packet held to the encoder's range state.
fn round_trip(input: &[f32], channels: u8, bitrate: u32) -> Vec<f32> {
    let width = usize::from(channels);
    let mut encoder = Encoder::new(channels, bitrate).expect("an encoder");
    let mut decoder = Multistream::new(&Layout::single(width)).expect("a decoder");
    let mut packet = vec![0u8; encoder.packet_len()];
    let mut frame = vec![0.0f32; FRAME_SAMPLES * width];
    let mut played = Vec::with_capacity(input.len());
    for (index, samples) in input.chunks_exact(FRAME_SAMPLES * width).enumerate() {
        assert_eq!(
            encoder.encode(samples, &mut packet),
            Ok(packet.len()),
            "packet {index}"
        );
        assert_eq!(
            decoder.decode(&packet, &mut frame),
            Ok(FRAME_SAMPLES),
            "packet {index}"
        );
        assert_eq!(
            decoder.final_range(),
            encoder.final_range(),
            "packet {index}"
        );
        played.extend_from_slice(&frame);
    }
    played
}

/// The ratio of `input`'s power in channel `channel` to its error's in
/// `decoded`, in dB, over every sample past the first `skip` frames that the
/// decode, [`PRE_SKIP`] late, reaches.
fn snr(input: &[f32], decoded: &[f32], channels: usize, channel: usize, skip: usize) -> f64 {
    let delay = usize::from(PRE_SKIP);
    let frames = input.len() / channels;
    let (mut signal, mut error) = (0.0f64, 0.0f64);
    for index in skip * FRAME_SAMPLES..frames - delay {
        let want = f64::from(input[index * channels + channel]);
        let got = f64::from(decoded[(index + delay) * channels + channel]);
        signal += want * want;
        error += (want - got) * (want - got);
    }
    10.0 * (signal / error.max(1e-30)).log10()
}

/// The power of channel `channel` of `samples` over `range`, in dB.
fn power(samples: &[f32], channels: usize, channel: usize, range: core::ops::Range<usize>) -> f64 {
    let len = range.len() as f64;
    let sum: f64 = range
        .map(|index| f64::from(samples[index * channels + channel]).powi(2))
        .sum();
    10.0 * (sum / len).max(1e-30).log10()
}

#[test]
fn every_packet_is_one_fullband_celt_frame_of_20_ms() {
    for (channels, stereo) in [(1u8, false), (2, true)] {
        let mut encoder = Encoder::new(channels, 64_000).expect("an encoder");
        let mut packet = vec![0u8; encoder.packet_len()];
        let input = vec![0.25f32; FRAME_SAMPLES * usize::from(channels)];
        assert_eq!(encoder.encode(&input, &mut packet), Ok(160));
        let parsed = Packet::parse(&packet, false).expect("a packet");
        let toc = Toc {
            mode: Mode::Celt,
            bandwidth: Bandwidth::Full,
            frame_size: FRAME_SAMPLES,
            stereo,
        };
        assert_eq!((parsed.toc, parsed.frames().len()), (toc, 1));
    }
}

#[test]
fn the_identification_header_states_the_stream() {
    let head = Encoder::new(2, 64_000).expect("an encoder").head();
    assert_eq!(&head[..8], b"OpusHead");
    assert_eq!((head[8], head[9]), (1, 2));
    assert_eq!(u16::from_le_bytes([head[10], head[11]]), PRE_SKIP);
    assert_eq!(
        u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
        48_000
    );
    assert_eq!(head[16..], [0, 0, 0], "no output gain, mapping family 0");
}

#[test]
fn tones_come_back_within_their_floors_at_each_bitrate() {
    for (hz, bitrate, floor) in [
        (440.0, 32_000, 24.0),
        (1000.0, 64_000, 31.0),
        (5000.0, 64_000, 23.0),
        (440.0, 128_000, 34.0),
        (5000.0, 128_000, 32.0),
    ] {
        let input = frames_of(1, 12, |_, index| tone(hz, 0.5, index));
        let got = snr(&input, &round_trip(&input, 1, bitrate), 1, 0, 2);
        assert!(got >= floor, "{hz} Hz at {bitrate} b/s: {got:.1} dB");
    }
}

#[test]
fn noise_keeps_its_level_and_at_a_high_rate_its_waveform() {
    let mut noise = Noise(0x2545_F491_4F6C_DD1D);
    let input = band_limited(&mut noise, 0.3, 12 * FRAME_SAMPLES);
    let held = 2 * FRAME_SAMPLES..10 * FRAME_SAMPLES;
    let delay = usize::from(PRE_SKIP);
    let decoded = round_trip(&input, 1, 64_000);
    let drift =
        power(&decoded, 1, 0, held.start + delay..held.end + delay) - power(&input, 1, 0, held);
    assert!(drift.abs() < 1.5, "noise at 64 kb/s drifts {drift:.2} dB");
    let got = snr(&input, &round_trip(&input, 1, 256_000), 1, 0, 2);
    assert!(got >= 19.0, "noise at 256 kb/s: {got:.1} dB");
}

/// A channel's sample at an index.
type Signal = fn(usize, usize) -> f32;

#[test]
fn stereo_keeps_each_channel_apart_together_and_inverted() {
    let cases: [(&str, u32, f64, Signal); 3] = [
        ("apart", 96_000, 17.0, |channel, index| {
            tone(if channel == 0 { 440.0 } else { 1320.0 }, 0.4, index)
        }),
        ("together", 96_000, 30.0, |_, index| tone(660.0, 0.4, index)),
        ("inverted", 32_000, 20.0, |channel, index| {
            let value = tone(5000.0, 0.4, index);
            if channel == 0 {
                value
            } else {
                -value
            }
        }),
    ];
    for (what, bitrate, floor, signal) in cases {
        let input = frames_of(2, 12, signal);
        let decoded = round_trip(&input, 2, bitrate);
        for channel in 0..2 {
            let got = snr(&input, &decoded, 2, channel, 2);
            assert!(got >= floor, "{what}, channel {channel}: {got:.1} dB");
        }
    }
}

#[test]
fn an_onset_leaves_the_silence_before_it_silent() {
    let onset = 6 * FRAME_SAMPLES + 480;
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    let input = frames_of(
        1,
        10,
        |_, index| {
            if index < onset {
                0.0
            } else {
                noise.draw(0.5)
            }
        },
    );
    let decoded = round_trip(&input, 1, 64_000);
    let at = onset + usize::from(PRE_SKIP);
    let before = power(&decoded, 1, 0, at - 480..at - 120);
    let after = power(&decoded, 1, 0, at..at + FRAME_SAMPLES);
    assert!(
        after - before >= 15.0,
        "the pre-echo is only {:.1} dB under the onset",
        after - before
    );
}

#[test]
fn silence_codes_to_silence_before_and_after_sound() {
    for channels in [1u8, 2] {
        let width = usize::from(channels);
        let sound = 2 * FRAME_SAMPLES..4 * FRAME_SAMPLES;
        let input = frames_of(width, 8, |_, index| {
            if sound.contains(&index) {
                tone(800.0, 0.5, index)
            } else {
                0.0
            }
        });
        let decoded = round_trip(&input, channels, 48_000);
        let silent = |frames: core::ops::Range<usize>| {
            decoded[frames.start * FRAME_SAMPLES * width..frames.end * FRAME_SAMPLES * width]
                .iter()
                .all(|value| value.abs() < 1e-20)
        };
        assert!(silent(0..2), "{channels} channels before the sound");
        assert!(silent(6..8), "{channels} channels after it");
    }
}

#[test]
fn every_bound_is_refused_and_a_refusal_changes_nothing() {
    for channels in [0u8, 3, 255] {
        let refused = Encoder::new(channels, 64_000).err();
        assert_eq!(refused, Some(EncodeError::Unsupported), "{channels}");
    }
    for bitrate in [0, MIN_BITRATE - 1, MAX_BITRATE + 1] {
        let refused = Encoder::new(2, bitrate).err();
        assert_eq!(refused, Some(EncodeError::Unsupported), "{bitrate}");
    }
    for (bitrate, len) in [(MIN_BITRATE, 15), (MAX_BITRATE, 1275)] {
        let lens = Encoder::new(2, bitrate).map(|encoder| encoder.packet_len());
        assert_eq!(lens, Ok(len), "{bitrate}");
        let input = frames_of(2, 4, |channel, index| {
            tone(300.0 + 200.0 * channel as f32, 0.5, index)
        });
        round_trip(&input, 2, bitrate);
    }
    let input = frames_of(1, 2, |_, index| tone(700.0, 0.5, index));
    let frame = &input[..FRAME_SAMPLES];
    let mut encoder = Encoder::new(1, 64_000).expect("an encoder");
    let mut packet = vec![0u8; encoder.packet_len()];
    let refused = Err(EncodeError::BadSamples);
    assert_eq!(
        encoder.encode(&frame[..FRAME_SAMPLES - 1], &mut packet),
        refused
    );
    assert_eq!(encoder.encode(&input, &mut packet), refused, "two frames");
    for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut poisoned = frame.to_vec();
        poisoned[17] = poison;
        assert_eq!(encoder.encode(&poisoned, &mut packet), refused);
    }
    let mut short = vec![0u8; packet.len() - 1];
    let too_small = Err(EncodeError::BufferTooSmall);
    assert_eq!(encoder.encode(frame, &mut short), too_small);
    let mut fresh = Encoder::new(1, 64_000).expect("an encoder");
    let mut expected = vec![0u8; packet.len()];
    assert_eq!(fresh.encode(frame, &mut expected), Ok(expected.len()));
    assert_eq!(encoder.encode(frame, &mut packet), Ok(packet.len()));
    assert_eq!(packet, expected, "a refusal leaves the encoder as it was");
}

#[test]
fn two_encoders_write_one_stream() {
    let mut noise = Noise(7);
    let input = frames_of(2, 6, |_, _| noise.draw(0.4));
    let stream = || {
        let mut encoder = Encoder::new(2, 96_000).expect("an encoder");
        let mut packet = vec![0u8; encoder.packet_len()];
        let mut bytes = Vec::new();
        for frame in input.as_chunks::<{ 2 * FRAME_SAMPLES }>().0 {
            encoder.encode(frame, &mut packet).expect("encodes");
            bytes.extend_from_slice(&packet);
        }
        bytes
    };
    assert_eq!(stream(), stream());
}

/// A frame refused as an overrun, which only an encoder defect makes, leaves
/// the encoder as it was: the packets after it are those of an encoder that
/// never saw it. Regression: the refused frame had moved the window, the
/// energies the next frame is predicted from and the range state, so a
/// stream that dropped it decoded every frame after it against a prediction
/// the decoder never had.
#[test]
fn a_frame_refused_as_an_overrun_leaves_the_encoder_as_it_was() {
    let mut noise = Noise(11);
    let input = frames_of(2, 6, |channel, index| {
        tone(300.0 + 200.0 * channel as f32, 0.4, index) + noise.draw(0.05)
    });
    let mut refusing = Encoder::new(2, 64_000).expect("an encoder");
    let mut fresh = Encoder::new(2, 64_000).expect("an encoder");
    let mut packet = vec![0u8; refusing.packet_len()];
    let mut expected = vec![0u8; packet.len()];
    for (index, frame) in input
        .as_chunks::<{ 2 * FRAME_SAMPLES }>()
        .0
        .iter()
        .enumerate()
    {
        if index == 2 {
            refusing.celt.outrun = true;
            let refused = refusing.encode(frame, &mut packet);
            assert_eq!(refused, Err(EncodeError::Overrun));
            continue;
        }
        assert_eq!(refusing.encode(frame, &mut packet), Ok(packet.len()));
        assert_eq!(fresh.encode(frame, &mut expected), Ok(expected.len()));
        assert_eq!(packet, expected, "frame {index}");
        assert_eq!(refusing.final_range(), fresh.final_range(), "frame {index}");
    }
}

#[test]
fn an_ogg_stream_of_its_packets_plays_in_time_with_its_input() {
    let frames = 10;
    let input = frames_of(2, frames, |channel, index| {
        tone(if channel == 0 { 600.0 } else { 900.0 }, 0.4, index)
    });
    let mut encoder = Encoder::new(2, 128_000).expect("an encoder");
    let mut writer = PageWriter::new(0x0b05);
    writer.packet(&encoder.head(), 0);
    writer.flush(false);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&6u32.to_le_bytes());
    tags.extend_from_slice(b"tairix");
    tags.extend_from_slice(&0u32.to_le_bytes());
    writer.packet(&tags, 0);
    writer.flush(false);
    let mut packet = vec![0u8; encoder.packet_len()];
    for (index, frame) in input
        .as_chunks::<{ 2 * FRAME_SAMPLES }>()
        .0
        .iter()
        .enumerate()
    {
        encoder.encode(frame, &mut packet).expect("encodes");
        let granule = u64::try_from((index + 1) * FRAME_SAMPLES).expect("a short stream");
        writer.packet(&packet, granule);
        writer.flush(index + 1 == frames);
    }
    let file = writer.into_bytes();
    let mut source =
        PcmSource::open(&mut &file[..], &DecodeLimits::new(8, 4096, 64)).expect("opens");
    let info = source.info();
    let playable = frames * FRAME_SAMPLES - usize::from(PRE_SKIP);
    assert_eq!(
        (
            info.format,
            info.encoding,
            info.channels.channels(),
            info.rate.hz()
        ),
        (SoundFormat::Ogg, Encoding::Opus, 2, 48_000)
    );
    assert_eq!(info.frames, u64::try_from(playable).ok());
    let frame_bytes = info.frame_bytes();
    let mut out = vec![0u8; 4096 * frame_bytes];
    let mut bytes = &file[..];
    let mut decoded = vec![0.0f32; 2 * usize::from(PRE_SKIP)];
    loop {
        let written = source.next_block(&mut bytes, &mut out).expect("decodes");
        if written == 0 {
            break;
        }
        decoded.extend(
            out[..written * frame_bytes]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|sample| f32::from_le_bytes(*sample)),
        );
    }
    assert_eq!(
        decoded.len(),
        2 * frames * FRAME_SAMPLES,
        "the pre-skip trimmed"
    );
    for channel in 0..2 {
        let got = snr(&input, &decoded, 2, channel, 2);
        assert!(got >= 23.0, "channel {channel}: {got:.1} dB");
    }
}

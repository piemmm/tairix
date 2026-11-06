//! Opus packets outside Ogg: each decodes exactly as the codec decodes it,
//! the header's output gain applied, and a header Ogg refuses is refused.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::OpusPackets;
use crate::ogg::gain_factor;
use crate::opus::{Layout, Multistream};
use crate::opus_encode::{Encoder, FRAME_SAMPLES, PRE_SKIP};
use crate::{DecodeError, DecodeLimits};

const LIMITS: DecodeLimits = DecodeLimits::new(8, 1024, 16);

/// `count` packets of a stereo square wave coded at 128 kb/s, and their
/// identification header.
fn coded(count: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut encoder = Encoder::new(2, 128_000).expect("an encoder");
    let mut frame = vec![0.0f32; FRAME_SAMPLES * 2];
    let mut packets = Vec::new();
    for packet in 0..count {
        for (index, pair) in frame.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let value = if ((packet * FRAME_SAMPLES + index) / 24).is_multiple_of(2) {
                0.25
            } else {
                -0.25
            };
            *pair = [value, -value];
        }
        let mut out = vec![0u8; encoder.packet_len()];
        let len = encoder.encode(&frame, &mut out).expect("a packet");
        out.truncate(len);
        packets.push(out);
    }
    (encoder.head().to_vec(), packets)
}

/// Samples as their bits, so two decodes compare exactly.
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

#[test]
fn packets_decode_as_the_codec_decodes_them() {
    let (head, packets) = coded(6);
    let mut stream = OpusPackets::new(&head, &LIMITS).expect("a stream");
    assert_eq!(stream.channels().channels(), 2);
    assert_eq!(stream.pre_skip(), u64::from(PRE_SKIP));
    let mut codec = Multistream::new(&Layout::single(2)).expect("a decoder");
    let mut ours = vec![0.0f32; OpusPackets::MAX_SAMPLES * 2];
    let mut theirs = ours.clone();
    for packet in &packets {
        let samples = stream.decode(packet, &mut ours).expect("decodes");
        assert_eq!(samples, FRAME_SAMPLES);
        assert_eq!(codec.decode(packet, &mut theirs), Ok(FRAME_SAMPLES));
        assert_eq!(bits(&ours[..samples * 2]), bits(&theirs[..samples * 2]));
    }
    let mut short = vec![0.0f32; FRAME_SAMPLES];
    assert_eq!(
        stream.decode(&packets[0], &mut short),
        Err(DecodeError::BufferTooSmall)
    );
}

#[test]
fn the_output_gain_scales_every_sample() {
    let (mut head, packets) = coded(3);
    head[16..18].copy_from_slice(&(-512i16).to_le_bytes());
    let mut stream = OpusPackets::new(&head, &LIMITS).expect("a stream");
    let mut codec = Multistream::new(&Layout::single(2)).expect("a decoder");
    let factor = gain_factor(-512);
    let mut ours = vec![0.0f32; OpusPackets::MAX_SAMPLES * 2];
    let mut theirs = ours.clone();
    for packet in &packets {
        let samples = stream.decode(packet, &mut ours).expect("decodes");
        codec.decode(packet, &mut theirs).expect("decodes");
        let scaled: Vec<f32> = theirs[..samples * 2]
            .iter()
            .map(|sample| sample * factor)
            .collect();
        assert_eq!(bits(&ours[..samples * 2]), bits(&scaled));
    }
}

#[test]
fn a_reset_stream_decodes_as_a_fresh_one() {
    let (head, packets) = coded(4);
    let mut stream = OpusPackets::new(&head, &LIMITS).expect("a stream");
    let mut fresh = OpusPackets::new(&head, &LIMITS).expect("a stream");
    let mut out = vec![0.0f32; OpusPackets::MAX_SAMPLES * 2];
    let mut again = out.clone();
    for packet in &packets[..3] {
        stream.decode(packet, &mut out).expect("decodes");
    }
    stream.reset();
    let samples = stream.decode(&packets[3], &mut out).expect("decodes");
    fresh.decode(&packets[3], &mut again).expect("decodes");
    assert_eq!(bits(&out[..samples * 2]), bits(&again[..samples * 2]));
}

#[test]
fn a_header_the_ogg_mapping_refuses_is_refused() {
    let (head, _) = coded(0);
    let refused = |bytes: &[u8]| OpusPackets::new(bytes, &LIMITS).err();
    let altered = |at: usize, value: u8| {
        let mut bytes = head.clone();
        bytes[at] = value;
        bytes
    };
    assert_eq!(
        refused(&altered(0, b'X')),
        Some(DecodeError::OpusBadIdentification)
    );
    assert_eq!(
        refused(&altered(8, 0x10)),
        Some(DecodeError::OpusUnsupportedVersion)
    );
    assert_eq!(refused(&altered(9, 0)), Some(DecodeError::NoChannels));
    assert_eq!(
        refused(&altered(9, 3)),
        Some(DecodeError::OpusBadIdentification),
        "family 0 holds one channel or two"
    );
    assert_eq!(
        refused(&head[..10]),
        Some(DecodeError::OpusBadIdentification)
    );
    let mono = DecodeLimits::new(1, 1024, 16);
    assert_eq!(
        OpusPackets::new(&head, &mono).err(),
        Some(DecodeError::ChannelsExceedLimit)
    );
}

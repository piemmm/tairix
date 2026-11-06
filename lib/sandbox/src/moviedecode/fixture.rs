//! A movie recording muxed in memory, for the tests of the crates that play
//! one: VP8 frames the bitstream fixture writes, Opus packets the encoder
//! codes, and what an in-process decode makes of each. Test scaffolding,
//! behind the `fixture` feature.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_sound::opus_encode::{Encoder, FRAME_SAMPLES, PRE_SKIP};
use tairix_sound::{DecodeLimits, OpusPackets};
use tairix_video::vp8::fixture::{
    drawn_keyframe, inter_frame, Filter, InterFrame, Movement, Predicted, Reference, Written,
};
use tairix_video::vp8::Decoder;
use tairix_video::webm::{Layout, Muxer, OpusChannels, OpusTrack};

/// The pictures' size: three macroblocks across and two down.
pub const SIZE: (u32, u32) = (48, 32);

/// How far apart the pictures are shown.
pub const PICTURE_NS: u64 = 40_000_000;

/// How far apart the packets of sound start.
pub const SOUND_NS: u64 = 20_000_000;

/// The sound's bitrate.
const BITRATE: u32 = 64_000;

/// The tag bit of a frame the stream shows, which the bitstream fixture's
/// keyframes leave clear.
const SHOWN: u8 = 1 << 4;

/// A recording's file, and the frames muxed into it.
pub struct Recording {
    /// The file.
    pub file: Vec<u8>,
    /// When each picture is shown, whether it is a keyframe, and its VP8 frame.
    pub pictures: Vec<(u64, bool, Vec<u8>)>,
    /// When each packet of sound starts, and the packet.
    pub packets: Vec<(u64, Vec<u8>)>,
}

/// A file the muxer writes, held in memory.
#[derive(Default)]
struct Disk(Vec<u8>);

impl Target for Disk {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let end = start + bytes.len();
        if self.0.len() < end {
            self.0.resize(end, 0);
        }
        self.0[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| Errno::OutOfRange)?;
        self.0.resize(len, 0);
        Ok(())
    }
}

/// What the container's codec delay discards from the sound's start: the
/// encoder's pre-skip at 48 kHz.
#[must_use]
pub fn codec_delay_ns() -> u64 {
    u64::from(PRE_SKIP) * 1_000_000_000 / 48_000
}

/// `count` pictures of [`SIZE`] [`PICTURE_NS`] apart, a keyframe every
/// `key_every` and still inter frames between.
fn pictures(count: usize, key_every: usize) -> Vec<(u64, bool, Vec<u8>)> {
    let mut word = 0x9E37_79B9_7F4A_7C15_u64;
    let mut draw = move || {
        word ^= word << 13;
        word ^= word >> 7;
        word ^= word << 17;
        word
    };
    let filter = Filter {
        simple: false,
        level: 0,
        sharpness: 0,
    };
    let still = Written {
        predicted: Predicted::Inter {
            reference: Reference::Last,
            motion: Movement::Still,
        },
        residual: None,
    };
    let blocks = vec![still; 6];
    (0..count)
        .map(|index| {
            let at = index as u64 * PICTURE_NS;
            if index % key_every == 0 {
                let mut bytes = drawn_keyframe(SIZE.0, SIZE.1, filter, 2, &mut draw);
                bytes[0] |= SHOWN;
                (at, true, bytes)
            } else {
                let bytes = inter_frame(SIZE.0, SIZE.1, &InterFrame::default(), &blocks);
                (at, false, bytes)
            }
        })
        .collect()
}

/// `count` stereo packets of a square wave, [`SOUND_NS`] apart.
fn packets(count: usize) -> Vec<(u64, Vec<u8>)> {
    let mut encoder = Encoder::new(2, BITRATE).expect("an encoder");
    let mut frame = vec![0.0f32; FRAME_SAMPLES * 2];
    (0..count)
        .map(|index| {
            for (offset, pair) in frame.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let value = if ((index * FRAME_SAMPLES + offset) / 40).is_multiple_of(2) {
                    0.2
                } else {
                    -0.2
                };
                *pair = [value; 2];
            }
            let mut out = vec![0u8; encoder.packet_len()];
            let len = encoder.encode(&frame, &mut out).expect("a packet");
            out.truncate(len);
            (index as u64 * SOUND_NS, out)
        })
        .collect()
}

/// A recording of `count` pictures, a keyframe every `key_every`, with twice as
/// many packets of sound where `sound` — finished, or left as a writer that
/// died leaves it.
#[must_use]
pub fn recording(count: usize, key_every: usize, sound: bool, finish: bool) -> Recording {
    let pictures = pictures(count, key_every);
    let packets = if sound {
        packets(count * 2)
    } else {
        Vec::new()
    };
    let layout = Layout {
        width: SIZE.0,
        height: SIZE.1,
        audio: sound.then_some(OpusTrack {
            channels: OpusChannels::Stereo,
            pre_skip: PRE_SKIP,
            input_rate: 48_000,
            output_gain: 0,
        }),
        indexed_keyframes: 64,
    };
    let mut disk = Disk::default();
    let mut muxer = Muxer::begin(&mut disk, &layout).expect("begins");
    let (mut seen, mut heard) = (0, 0);
    while seen < pictures.len() || heard < packets.len() {
        let video = heard == packets.len()
            || (seen < pictures.len() && pictures[seen].0 <= packets[heard].0);
        if video {
            let (at, keyframe, bytes) = &pictures[seen];
            muxer
                .push_video(&mut disk, *at, *keyframe, bytes)
                .expect("takes");
            seen += 1;
        } else {
            let (at, bytes) = &packets[heard];
            muxer.push_audio(&mut disk, *at, bytes).expect("takes");
            heard += 1;
        }
    }
    if finish {
        let end = count as u64 * PICTURE_NS;
        muxer.finish(&mut disk, end).expect("finishes");
    }
    Recording {
        file: disk.0,
        pictures,
        packets,
    }
}

/// Each of `pictures` as an in-process decode of them in order gives it:
/// opaque RGBA8 at [`SIZE`].
#[must_use]
pub fn decoded(pictures: &[(u64, bool, Vec<u8>)]) -> Vec<Vec<u8>> {
    let mut decoder = Decoder::new();
    let (width, height) = (SIZE.0 as usize, SIZE.1 as usize);
    pictures
        .iter()
        .map(|(_, _, bytes)| {
            let frame = decoder.decode(bytes).expect("decodes");
            let mut rgba = vec![0u8; width * height * 4];
            frame
                .rgba_rows(0..height, &mut rgba, width * 4)
                .expect("rows");
            rgba
        })
        .collect()
}

/// Each of `packets`, the first starting at nought, as the movie decode
/// answers it: when its first sound plays, and its samples past the codec
/// delay as interleaved `F32` bytes.
#[must_use]
pub fn decoded_sound(packets: &[(u64, Vec<u8>)]) -> Vec<(u64, Vec<u8>)> {
    let head = Encoder::new(2, BITRATE).expect("an encoder").head();
    let mut stream = OpusPackets::new(&head, &DecodeLimits::new(8, 0, 0)).expect("a stream");
    let mut pcm = vec![0.0f32; OpusPackets::MAX_SAMPLES * 2];
    let delay = codec_delay_ns();
    packets
        .iter()
        .enumerate()
        .map(|(index, (start, packet))| {
            let samples = stream.decode(packet, &mut pcm).expect("decodes");
            let skip = if index == 0 { usize::from(PRE_SKIP) } else { 0 };
            let bytes = pcm[skip * 2..samples * 2]
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            (start.saturating_sub(delay), bytes)
        })
        .collect()
}

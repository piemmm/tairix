//! The MAI's samples: IEC 60958 subframes, each of IEC 60958-1's time slots a
//! bit of a little-endian word as alsa-lib's IEC958 subframe plugin packs them
//! — the preamble code in the low nibble (block start 0x8, first subframe 0x2,
//! second 0x4), the 24-bit sample above it, then the validity, user, channel
//! status and parity bits — each slot of a frame one subframe, in the order
//! the audio `InfoFrame`'s speaker allocation places them.

use tairix_abi::driver::audio::{Rate, SampleFormat};
use tairix_audiochan::cyclic::FrameCodec;
use tairix_edid::{Allocation, SLOTS};

/// Frames in a channel-status block.
const BLOCK_FRAMES: u16 = 192;
/// Bytes of the channel status a block carries a bit a frame of.
const STATUS_BYTES: usize = BLOCK_FRAMES as usize / 8;

/// The preamble of the first subframe of a block: the B-frame identifier the
/// transmitter is told to look for.
pub(crate) const PREAMBLE_BLOCK: u32 = 0x8;
/// The first subframe of every other frame.
const PREAMBLE_FIRST: u32 = 0x2;
/// Every later subframe of a frame.
const PREAMBLE_OTHER: u32 = 0x4;

/// The bits of a 24-bit sample.
const SAMPLE: u32 = 0x00FF_FFFF;
const VALIDITY: u32 = 1 << 28;
const CHANNEL_STATUS: u32 = 1 << 30;
const PARITY: u32 = 1 << 31;

/// The channel status a stream's samples carry (IEC 60958-3 consumer
/// format): linear PCM, copying permitted, no pre-emphasis, the sampling
/// frequency and the word length, the rest unspecified (IEC 60958-3: bytes 0,
/// 3 and 4).
#[must_use]
fn channel_status(rate: Rate, word_bits: u8) -> Option<[u8; STATUS_BYTES]> {
    let sampling = match rate.hz() {
        44_100 => 0x0,
        48_000 => 0x2,
        32_000 => 0x3,
        88_200 => 0x8,
        96_000 => 0xA,
        176_400 => 0xC,
        192_000 => 0xE,
        _ => return None,
    };
    let word_length = match word_bits {
        16 => 0x02,
        20 => 0x03,
        24 => 0x0B,
        _ => return None,
    };
    let mut status = [0; STATUS_BYTES];
    status[0] = 1 << 2;
    status[3] = sampling;
    status[4] = word_length;
    Some(status)
}

/// One subframe: the bits of `sample` that `word` keeps, its channel-status
/// bit, its validity, and the parity that makes bits 4 to 31 even.
fn subframe(sample: i32, word: u32, preamble: u32, status: bool, valid: bool) -> u32 {
    let mut word = (sample.cast_unsigned() & word) << 4;
    if !valid {
        word |= VALIDITY;
    }
    if status {
        word |= CHANNEL_STATUS;
    }
    if (word >> 4).count_ones() % 2 == 1 {
        word |= PARITY;
    }
    word | preamble
}

/// How a stream is framed: the slots its channels fill, the channel status
/// its samples carry, and the sample bits it sends.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Framing {
    channels: u8,
    /// The ring channel each slot carries.
    order: [u8; SLOTS],
    status: [u8; STATUS_BYTES],
    /// The bits within the word length; IEC 60958 sends those below it as
    /// zero.
    word: u32,
}

impl Framing {
    /// `allocation`'s slots carrying `word_bits`-bit samples at `rate`, or
    /// `None` for a rate or a width IEC 60958 has no code for.
    pub(crate) fn new(allocation: &Allocation, rate: Rate, word_bits: u8) -> Option<Self> {
        let status = channel_status(rate, word_bits)?;
        let unsent = 24 - u32::from(word_bits);
        Some(Self {
            channels: allocation.channels,
            order: allocation.order,
            status,
            word: SAMPLE >> unsent << unsent,
        })
    }
}

/// The frame codec of an HDMI audio stream.
pub(crate) struct Iec958 {
    framing: Framing,
    /// The frame's place in its channel-status block.
    frame: u16,
}

impl Iec958 {
    /// A codec for a stereo stream of no rate yet.
    pub(crate) const fn new() -> Self {
        Self {
            framing: Framing {
                channels: 2,
                order: [0, 1, 2, 3, 4, 5, 6, 7],
                status: [0; STATUS_BYTES],
                word: SAMPLE,
            },
            frame: 0,
        }
    }

    /// Frame the stream `framing` describes from its first frame.
    pub(crate) const fn set(&mut self, framing: Framing) {
        self.framing = framing;
        self.frame = 0;
    }

    fn status_bit(&self) -> bool {
        let at = usize::from(self.frame);
        self.framing.status[at / 8] & (1 << (at % 8)) != 0
    }

    /// Write one frame of `samples`, in the ring's interleave, as subframes.
    fn put(&mut self, frame: &mut [u8], samples: &[i32; SLOTS]) {
        let Framing {
            channels,
            order,
            word: kept,
            ..
        } = self.framing;
        let status = self.status_bit();
        let (words, _) = frame.as_chunks_mut::<4>();
        for (slot, word) in words.iter_mut().take(usize::from(channels)).enumerate() {
            let preamble = match (slot, self.frame) {
                (0, 0) => PREAMBLE_BLOCK,
                (0, _) => PREAMBLE_FIRST,
                _ => PREAMBLE_OTHER,
            };
            let sample = samples[usize::from(order[slot])];
            *word = subframe(sample, kept, preamble, status, true).to_le_bytes();
        }
        self.frame = (self.frame + 1) % BLOCK_FRAMES;
    }
}

impl FrameCodec for Iec958 {
    fn format(&self) -> SampleFormat {
        SampleFormat::S24In32
    }

    fn channels(&self) -> u8 {
        self.framing.channels
    }

    fn encode(&mut self, frame: &mut [u8]) {
        let mut samples = [0i32; SLOTS];
        let (words, _) = frame.as_chunks::<4>();
        for (sample, word) in samples.iter_mut().zip(words) {
            *sample = i32::from_le_bytes(*word);
        }
        self.put(frame, &samples);
    }

    fn silence(&mut self, frame: &mut [u8]) {
        self.put(frame, &[0; SLOTS]);
    }

    /// Marked invalid, so a sink plays none of what a parked channel sends.
    fn parked(&self, frame: &mut [u8]) {
        let (words, _) = frame.as_chunks_mut::<4>();
        for (slot, word) in words.iter_mut().enumerate() {
            let preamble = if slot == 0 {
                PREAMBLE_FIRST
            } else {
                PREAMBLE_OTHER
            };
            *word = subframe(0, SAMPLE, preamble, false, false).to_le_bytes();
        }
    }

    fn reset(&mut self) {
        self.frame = 0;
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::driver::audio::ChannelMap;
    use tairix_edid::allocate;

    use super::*;

    fn word(frame: &[u8], slot: usize) -> u32 {
        u32::from_le_bytes(frame[slot * 4..slot * 4 + 4].try_into().expect("a word"))
    }

    fn framing(map: &ChannelMap, hz: u32, word_bits: u8) -> Option<Framing> {
        let rate = Rate::new(hz).expect("a rate");
        Framing::new(&allocate(map).expect("placed"), rate, word_bits)
    }

    fn stereo(hz: u32, word_bits: u8) -> Iec958 {
        let mut codec = Iec958::new();
        codec.set(framing(&ChannelMap::STEREO, hz, word_bits).expect("framed"));
        codec
    }

    /// The status bits the first subframes of a block carry, a byte at a
    /// time.
    fn status(codec: &mut Iec958, frame: &mut [u8]) -> [u8; STATUS_BYTES] {
        let mut status = [0; STATUS_BYTES];
        for index in 0..usize::from(BLOCK_FRAMES) {
            codec.silence(frame);
            let bit = word(frame, 0) & CHANNEL_STATUS != 0;
            status[index / 8] |= u8::from(bit) << (index % 8);
        }
        status
    }

    #[test]
    fn a_sample_takes_bits_4_to_27_with_even_parity_over_the_rest() {
        let word = subframe(-1, SAMPLE, PREAMBLE_FIRST, false, true);
        assert_eq!(word & 0x0FFF_FFF0, 0x0FFF_FFF0);
        assert_eq!(word & 0xF, PREAMBLE_FIRST);
        assert_eq!((word >> 4).count_ones() % 2, 0);
        let one = subframe(1, SAMPLE, PREAMBLE_OTHER, false, true);
        assert_eq!(one, 1 << 4 | PARITY | PREAMBLE_OTHER);
        assert_eq!(
            subframe(0, SAMPLE, PREAMBLE_FIRST, false, false),
            VALIDITY | PARITY | PREAMBLE_FIRST
        );
    }

    #[test]
    fn a_block_starts_on_its_first_frame_and_carries_the_status_a_bit_a_frame() {
        let mut codec = stereo(48_000, 24);
        let mut frame = [0u8; 8];
        for index in 0..=BLOCK_FRAMES {
            frame[..4].copy_from_slice(&0x0012_3456i32.to_le_bytes());
            frame[4..].copy_from_slice(&(-2i32).to_le_bytes());
            codec.encode(&mut frame);
            let first = word(&frame, 0);
            let expected = if index % BLOCK_FRAMES == 0 {
                PREAMBLE_BLOCK
            } else {
                PREAMBLE_FIRST
            };
            assert_eq!(first & 0xF, expected);
            assert_eq!(word(&frame, 1) & 0xF, PREAMBLE_OTHER);
            assert_eq!(first >> 4 & SAMPLE, 0x12_3456);
            assert_eq!(first & CHANNEL_STATUS, word(&frame, 1) & CHANNEL_STATUS);
        }
        codec.reset();
        let status = status(&mut codec, &mut frame);
        assert_eq!(
            [status[0], status[3], status[4]],
            [0x04, 0x02, 0x0B],
            "48 kHz, 24 bits"
        );
        assert!(status[5..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn a_narrower_word_length_is_stated_and_sends_the_bits_below_it_as_zero() {
        let mut codec = stereo(44_100, 16);
        let mut frame = [0u8; 8];
        frame[..4].copy_from_slice(&0x0012_3456i32.to_le_bytes());
        frame[4..].copy_from_slice(&(-1i32).to_le_bytes());
        codec.encode(&mut frame);
        assert_eq!(word(&frame, 0) >> 4 & SAMPLE, 0x12_3400);
        assert_eq!(word(&frame, 1) >> 4 & SAMPLE, 0xFF_FF00);
        assert_eq!(
            (word(&frame, 1) >> 4).count_ones() % 2,
            0,
            "parity over what is sent"
        );
        codec.reset();
        let status = status(&mut codec, &mut frame);
        assert_eq!([status[3], status[4]], [0x00, 0x02], "44.1 kHz, 16 bits");
        let twenty = framing(&ChannelMap::STEREO, 96_000, 20).expect("framed");
        assert_eq!(twenty.word, 0x00FF_FFF0);
        assert_eq!([twenty.status[3], twenty.status[4]], [0x0A, 0x03]);
    }

    #[test]
    fn the_slots_follow_the_allocation_s_order() {
        let mut codec = Iec958::new();
        let surround = ChannelMap::conventional(6).expect("5.1");
        codec.set(framing(&surround, 48_000, 24).expect("framed"));
        assert_eq!(codec.channels(), 6);
        let mut frame = [0u8; 24];
        for (channel, word) in frame.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            word.copy_from_slice(&(i32::try_from(channel).expect("small") + 1).to_le_bytes());
        }
        codec.encode(&mut frame);
        let carried: Vec<u32> = (0..6)
            .map(|slot| word(&frame, slot) >> 4 & SAMPLE)
            .collect();
        assert_eq!(
            carried,
            [1, 2, 4, 3, 5, 6],
            "LFE and centre swapped into their slots"
        );
    }

    #[test]
    fn a_parked_frame_is_marked_invalid_and_silence_keeps_the_block_going() {
        let mut codec = stereo(44_100, 24);
        let mut frame = [0xAA; 8];
        codec.parked(&mut frame);
        assert!((0..2).all(|slot| word(&frame, slot) & VALIDITY != 0));
        codec.silence(&mut frame);
        assert_eq!(
            word(&frame, 0) & 0xF,
            PREAMBLE_BLOCK,
            "silence starts the block"
        );
        assert_eq!(word(&frame, 0) & VALIDITY, 0);
        codec.silence(&mut frame);
        assert_eq!(word(&frame, 0) & 0xF, PREAMBLE_FIRST);
        codec.reset();
        codec.silence(&mut frame);
        assert_eq!(
            word(&frame, 0) & 0xF,
            PREAMBLE_BLOCK,
            "a fresh stream starts a block"
        );
    }

    #[test]
    fn a_rate_or_width_iec_60958_has_no_code_for_is_refused() {
        assert!(framing(&ChannelMap::STEREO, 22_050, 24).is_none());
        assert!(framing(&ChannelMap::STEREO, 48_000, 18).is_none());
        assert!(framing(&ChannelMap::STEREO, 48_000, 32).is_none());
    }
}

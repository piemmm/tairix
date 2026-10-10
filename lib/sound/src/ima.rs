//! IMA/DVI ADPCM as WAVE blocks it (format tag `0x0011`): each block opens
//! with every channel's predictor and step index, then carries four-bit
//! codes, least significant nibble first, a channel's eight in each four-byte
//! word, the channels' words in turn.

use tairix_abi::driver::audio::MAX_CHANNELS;

use crate::pcm::FrameSink;
use crate::DecodeError;

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

const INDEX_STEP: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Bytes of one channel's block header.
pub(crate) const HEADER: usize = 4;

/// Bytes of one channel's word of eight codes.
const WORD: usize = 4;

/// Frames a block of `block` bytes across `channels` channels holds: the
/// header's, then eight a word; [`None`] for a block that is not whole words.
pub(crate) fn frames_in(block: usize, channels: usize) -> Option<usize> {
    let data = block.checked_sub(HEADER * channels)?;
    let words = WORD * channels;
    (data % words == 0).then(|| 1 + data / words * 8)
}

/// One channel's decoder state.
#[derive(Copy, Clone, Debug, Default)]
struct Channel {
    predictor: i32,
    index: i32,
}

impl Channel {
    fn decode(&mut self, code: u8) -> i16 {
        let step = STEPS[usize::try_from(self.index).unwrap_or(0)];
        let mut diff = step >> 3;
        if code & 4 != 0 {
            diff += step;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 1 != 0 {
            diff += step >> 2;
        }
        self.predictor = if code & 8 != 0 {
            self.predictor - diff
        } else {
            self.predictor + diff
        }
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        self.index = (self.index + INDEX_STEP[usize::from(code & 7)]).clamp(0, 88);
        i16::try_from(self.predictor).unwrap_or(0)
    }
}

/// Decode `block`, a whole block or the data's last, across `channels`
/// channels, writing its frames from `skip` on into `out` as 16-bit samples
/// while it has room; answer how many were written.
///
/// # Errors
///
/// [`DecodeError::WavAdpcmBlockCorrupt`] for a block short of its header or
/// a step index past the table.
pub(crate) fn decode_block(
    block: &[u8],
    channels: usize,
    skip: usize,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    let mut state = [Channel::default(); MAX_CHANNELS];
    let state = &mut state[..channels];
    let (header, data) = block
        .split_at_checked(HEADER * channels)
        .ok_or(DecodeError::WavAdpcmBlockCorrupt)?;
    for (channel, bytes) in state.iter_mut().zip(header.as_chunks::<4>().0) {
        if bytes[2] > 88 {
            return Err(DecodeError::WavAdpcmBlockCorrupt);
        }
        channel.predictor = i32::from(i16::from_le_bytes([bytes[0], bytes[1]]));
        channel.index = i32::from(bytes[2]);
    }
    let mut sink = FrameSink::new(out, channels, skip);
    let mut first = [0i16; MAX_CHANNELS];
    for (sample, channel) in first.iter_mut().zip(state.iter()) {
        *sample = i16::try_from(channel.predictor).unwrap_or(0);
    }
    sink.push(&first[..channels]);
    for group in data.chunks_exact(WORD * channels) {
        if sink.full() {
            break;
        }
        let mut decoded = [[0i16; MAX_CHANNELS]; 8];
        let words = group.as_chunks::<WORD>().0;
        for (channel, (word, codec)) in words.iter().zip(state.iter_mut()).enumerate() {
            for (byte, pair) in word.iter().zip(decoded.as_chunks_mut::<2>().0) {
                pair[0][channel] = codec.decode(byte & 0x0F);
                pair[1][channel] = codec.decode(byte >> 4);
            }
        }
        for samples in &decoded {
            sink.push(&samples[..channels]);
        }
    }
    Ok(sink.written())
}

#[cfg(test)]
#[path = "ima_tests.rs"]
mod tests;

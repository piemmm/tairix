//! Microsoft ADPCM (format tag `0x0002`): each block opens with every
//! channel's predictor choice, quantiser scale and two seed samples, then
//! carries four-bit codes, most significant nibble first, the channels' in
//! turn. The predictor coefficient pairs are the format's own.

use tairix_abi::driver::audio::MAX_CHANNELS;

use crate::pcm::FrameSink;
use crate::DecodeError;

const ADAPTATION: [i64; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// The smallest the quantiser scale falls to.
const MIN_DELTA: i64 = 16;

/// Bytes of one channel's block header: its predictor choice, scale and two
/// seed samples.
pub(crate) const HEADER: usize = 7;

/// Frames a block of `block` bytes across `channels` channels holds: the two
/// seeds, then one a code per channel; [`None`] for one short of its header.
pub(crate) fn frames_in(block: usize, channels: usize) -> Option<usize> {
    let data = block.checked_sub(HEADER * channels)?;
    Some(2 + data * 2 / channels)
}

/// One channel's decoder state.
#[derive(Copy, Clone, Debug, Default)]
struct Channel {
    coefficients: [i64; 2],
    delta: i64,
    sample1: i64,
    sample2: i64,
}

impl Channel {
    fn decode(&mut self, code: u8) -> i16 {
        let [first, second] = self.coefficients;
        let predicted = (self.sample1 * first + self.sample2 * second) >> 8;
        let signed = i64::from(code) - if code & 8 != 0 { 16 } else { 0 };
        let sample =
            (predicted + signed * self.delta).clamp(i64::from(i16::MIN), i64::from(i16::MAX));
        self.sample2 = self.sample1;
        self.sample1 = sample;
        // The scale is held within 32 bits, so a hostile run of large codes
        // cannot carry it past what the arithmetic holds.
        self.delta = ((ADAPTATION[usize::from(code)] * self.delta) >> 8)
            .clamp(MIN_DELTA, i64::from(i32::MAX));
        i16::try_from(sample).unwrap_or(0)
    }
}

fn header_i16(header: &[u8], field: usize, channels: usize, channel: usize) -> i64 {
    let at = channels + 2 * (field * channels + channel);
    i64::from(i16::from_le_bytes([header[at], header[at + 1]]))
}

/// Decode `block`, a whole block or the data's last, across `channels`
/// channels with the format's `coefficients`, writing its frames from `skip`
/// on into `out` as 16-bit samples while it has room; answer how many were
/// written.
///
/// # Errors
///
/// [`DecodeError::WavAdpcmBlockCorrupt`] for a block short of its header or
/// a predictor choice past the format's coefficients.
pub(crate) fn decode_block(
    block: &[u8],
    channels: usize,
    coefficients: &[[i16; 2]],
    skip: usize,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    let (header, data) = block
        .split_at_checked(HEADER * channels)
        .ok_or(DecodeError::WavAdpcmBlockCorrupt)?;
    let mut state = [Channel::default(); MAX_CHANNELS];
    let state = &mut state[..channels];
    for (index, channel) in state.iter_mut().enumerate() {
        let pair = coefficients
            .get(usize::from(header[index]))
            .ok_or(DecodeError::WavAdpcmBlockCorrupt)?;
        channel.coefficients = [i64::from(pair[0]), i64::from(pair[1])];
        channel.delta = header_i16(header, 0, channels, index);
        channel.sample1 = header_i16(header, 1, channels, index);
        channel.sample2 = header_i16(header, 2, channels, index);
    }
    let mut sink = FrameSink::new(out, channels, skip);
    let mut frame = [0i16; MAX_CHANNELS];
    for seed in [|c: &Channel| c.sample2, |c: &Channel| c.sample1] {
        for (sample, channel) in frame.iter_mut().zip(state.iter()) {
            *sample = i16::try_from(seed(channel)).unwrap_or(0);
        }
        sink.push(&frame[..channels]);
    }
    let codes = data.iter().flat_map(|&byte| [byte >> 4, byte & 0x0F]);
    let frames = data.len() * 2 / channels;
    for (code, slot) in codes.take(frames * channels).zip((0..channels).cycle()) {
        frame[slot] = state[slot].decode(code);
        if slot + 1 == channels {
            if sink.full() {
                break;
            }
            sink.push(&frame[..channels]);
        }
    }
    Ok(sink.written())
}

#[cfg(test)]
#[path = "msadpcm_tests.rs"]
mod tests;

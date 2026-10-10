//! One FLAC frame: its header, its subframes, and the stereo decorrelation
//! between them (RFC 9639, section 9).
//!
//! Every field is read as the specification defines it, and a reserved or
//! forbidden value is refused rather than guessed at. Samples are held in 64
//! bits, so the widest stream the format allows — 32-bit samples, whose side
//! channel takes 33 — neither overflows nor wraps: a history sample is at most
//! 33 bits and a coefficient at most 15, so 32 of their products sum within
//! 2^51.

use alloc::vec::Vec;

use crate::bits::{BitReader, Exhausted};
use crate::crc::{crc16, crc8};

/// Bytes a frame header can take: sync and codes, a seven-byte coded number,
/// two bytes of block size, two of rate, and the CRC-8.
pub(crate) const MAX_HEADER_LEN: usize = 4 + 7 + 2 + 2 + 1;

/// Bytes of the CRC-16 a frame ends with.
const FOOTER_LEN: usize = 2;

/// Most coefficients a linear predictor has.
const MAX_LPC_ORDER: usize = 32;

/// Most samples a block holds: the 16-bit size `STREAMINFO` can state.
pub(crate) const MAX_BLOCK: u32 = 65_535;

/// The rate each frame-header rate code from 1 to 11 names.
pub(crate) const RATE_CODES: [u32; 11] = [
    88_200, 176_400, 192_000, 8_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000,
];

/// The sample width each frame-header width code names; code 0 refers to
/// `STREAMINFO` and code 3 is reserved.
pub(crate) const WIDTH_CODES: [Option<u8>; 8] = [
    None,
    Some(8),
    Some(12),
    None,
    Some(16),
    Some(20),
    Some(24),
    Some(32),
];

/// The block size a frame-header block code names outright, for the codes
/// that name one.
pub(crate) const fn common_block(code: u32) -> Option<u32> {
    match code {
        1 => Some(192),
        2..=5 => Some(576 << (code - 2)),
        8..=15 => Some(256 << (code - 8)),
        _ => None,
    }
}

/// Most channels a frame codes.
pub(crate) const MAX_CHANNELS: u8 = 8;

/// The widest sample a stream holds.
pub(crate) const MAX_BITS: u8 = 32;

/// Why a frame could not be decoded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FrameError {
    /// The bytes ended inside the frame.
    Truncated,
    /// No frame sync where a frame should start.
    NoSync,
    /// The header's CRC-8 disagrees.
    HeaderCrc,
    /// The frame's CRC-16 disagrees.
    FrameCrc,
    /// A reserved or forbidden code.
    Reserved,
    /// A value that cannot hold: a sample past its width, a partition
    /// shorter than its predictor, a block longer than the stream's.
    Invalid,
    /// The frame's rate, width or channels differ from the stream's.
    Mismatch,
    /// The frame does not continue the stream where it was expected to.
    OutOfOrder,
    /// The allocator refused the frame's samples.
    OutOfMemory,
}

impl From<Exhausted> for FrameError {
    fn from(_: Exhausted) -> Self {
        Self::Truncated
    }
}

/// How a frame's channels are coded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Assignment {
    /// Each channel on its own: this many.
    Independent(u8),
    /// Left, then left minus right.
    LeftSide,
    /// Left minus right, then right.
    SideRight,
    /// The mean, then left minus right.
    MidSide,
}

impl Assignment {
    fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            8 => Self::LeftSide,
            9 => Self::SideRight,
            10 => Self::MidSide,
            _ => Self::Independent(u8::try_from(code).ok().filter(|&code| code < 8)? + 1),
        })
    }

    /// The four-bit code the header carries.
    #[cfg(any(test, feature = "encode"))]
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Independent(count) => count - 1,
            Self::LeftSide => 8,
            Self::SideRight => 9,
            Self::MidSide => 10,
        }
    }

    pub(crate) const fn channels(self) -> u8 {
        match self {
            Self::Independent(count) => count,
            Self::LeftSide | Self::SideRight | Self::MidSide => 2,
        }
    }

    /// Whether `channel` carries the difference, a bit wider than the rest.
    pub(crate) const fn is_side(self, channel: usize) -> bool {
        matches!(
            (self, channel),
            (Self::LeftSide | Self::MidSide, 1) | (Self::SideRight, 0)
        )
    }
}

/// What a frame's header states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrameHeader {
    /// The stream's blocks vary in size, so `number` counts samples.
    pub(crate) variable: bool,
    /// Samples a channel holds.
    pub(crate) block_size: u32,
    /// How its channels are coded.
    pub(crate) assignment: Assignment,
    /// The frame's number, or its first sample's in a variable stream.
    pub(crate) number: u64,
    /// Bytes the header took.
    pub(crate) len: usize,
}

impl FrameHeader {
    /// The first sample this frame holds, in a stream of `fixed_block`
    /// samples a frame.
    pub(crate) const fn first_sample(&self, fixed_block: u32) -> u64 {
        if self.variable {
            self.number
        } else {
            self.number * fixed_block as u64
        }
    }
}

/// What the stream states and every frame must agree with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Stream {
    pub(crate) rate: u32,
    pub(crate) bits: u8,
    pub(crate) channels: u8,
    pub(crate) max_block: u32,
}

impl Stream {
    /// The bytes a frame of `block` samples a channel takes stored verbatim
    /// after a header of `header_len` bytes.
    pub(crate) const fn verbatim_len(&self, block: u32, header_len: usize) -> usize {
        let side = if self.channels == 2 { 1 } else { 0 };
        let samples = block as usize * (self.channels as usize * self.bits as usize + side);
        let subframe_headers = self.channels as usize * 8;
        header_len + (samples + subframe_headers).div_ceil(8) + FOOTER_LEN
    }

    /// The most bytes a frame of `block` samples a channel may take: twice
    /// its verbatim size. Verbatim is always open to an encoder, so a frame
    /// past this holds more unary padding than sound, and a decode that held
    /// it would be held to no bound at all.
    pub(crate) const fn frame_bound(&self, block: u32, header_len: usize) -> usize {
        2 * self.verbatim_len(block, header_len)
    }

    /// The most bytes any frame of this stream takes.
    pub(crate) const fn max_frame_len(&self) -> usize {
        self.frame_bound(self.max_block, MAX_HEADER_LEN)
    }
}

/// Whether a frame's sync code starts `bytes`.
pub(crate) fn has_sync(bytes: &[u8]) -> bool {
    matches!(bytes, [0xFF, second, ..] if second & 0xFE == 0xF8)
}

/// Read the UTF-8-style coded number a header carries: up to 36 bits.
fn coded_number(reader: &mut BitReader<'_>) -> Result<u64, FrameError> {
    let first = reader.read(8)?;
    let ones = (first << 24).leading_ones();
    let (mut value, extra) = match ones {
        0 => (u64::from(first), 0),
        2..=7 => (u64::from(first & (0x7F >> ones)), ones - 1),
        _ => return Err(FrameError::Invalid),
    };
    for _ in 0..extra {
        let next = reader.read(8)?;
        if next & 0xC0 != 0x80 {
            return Err(FrameError::Invalid);
        }
        value = value << 6 | u64::from(next & 0x3F);
    }
    Ok(value)
}

/// Read the frame header `bytes` opens with.
pub(crate) fn header(bytes: &[u8], stream: &Stream) -> Result<FrameHeader, FrameError> {
    if !has_sync(bytes) {
        return Err(FrameError::NoSync);
    }
    let mut reader = BitReader::new(bytes);
    reader.read(15)?;
    let variable = reader.read(1)? == 1;
    let size_code = reader.read(4)?;
    let rate_code = reader.read(4)?;
    let assignment = Assignment::from_code(reader.read(4)?).ok_or(FrameError::Reserved)?;
    let bits = match reader.read(3)? {
        0 => stream.bits,
        code => WIDTH_CODES[code as usize].ok_or(FrameError::Reserved)?,
    };
    if reader.read(1)? != 0 {
        return Err(FrameError::Reserved);
    }
    let number = coded_number(&mut reader)?;
    if !variable && number >> 31 != 0 {
        return Err(FrameError::Invalid);
    }
    let block_size = match size_code {
        0 => return Err(FrameError::Reserved),
        6 => reader.read(8)? + 1,
        7 => reader.read(16)? + 1,
        code => common_block(code).ok_or(FrameError::Reserved)?,
    };
    if block_size > MAX_BLOCK {
        return Err(FrameError::Reserved);
    }
    let rate = match rate_code {
        0 => stream.rate,
        1..=11 => RATE_CODES[rate_code as usize - 1],
        12 => reader.read(8)? * 1_000,
        13 => reader.read(16)?,
        14 => reader.read(16)? * 10,
        _ => return Err(FrameError::Reserved),
    };
    let len = reader.bytes_consumed();
    let crc = reader.read(8)?;
    if u32::from(crc8(&bytes[..len])) != crc {
        return Err(FrameError::HeaderCrc);
    }
    if rate != stream.rate || bits != stream.bits || assignment.channels() != stream.channels {
        return Err(FrameError::Mismatch);
    }
    if block_size > stream.max_block {
        return Err(FrameError::Invalid);
    }
    Ok(FrameHeader {
        variable,
        block_size,
        assignment,
        number,
        len: len + 1,
    })
}

/// A frame's samples, a channel after another, wide enough for a side
/// channel of 32-bit samples.
#[derive(Default)]
pub(crate) struct Samples {
    samples: Vec<i64>,
    block: usize,
}

impl Samples {
    /// Channel `channel`'s samples.
    pub(crate) fn channel(&self, channel: usize) -> &[i64] {
        &self.samples[channel * self.block..(channel + 1) * self.block]
    }

    fn reset(&mut self, block: usize, channels: usize) -> Result<&mut [i64], FrameError> {
        let total = block * channels;
        if let Some(more) = total
            .checked_sub(self.samples.len())
            .filter(|&more| more > 0)
        {
            if !tairix_util::fallible::reserve(&mut self.samples, more) {
                return Err(FrameError::OutOfMemory);
            }
            self.samples.resize(total, 0);
        }
        self.block = block;
        Ok(&mut self.samples[..total])
    }
}

/// Decode the frame `bytes` opens with into `out`, answering its header and
/// the bytes it took.
///
/// # Errors
///
/// [`FrameError::Truncated`] when the frame runs past `bytes`, which the
/// caller tells apart from a frame too large by what `bytes` covered.
pub(crate) fn decode(
    bytes: &[u8],
    stream: &Stream,
    out: &mut Samples,
) -> Result<(FrameHeader, usize), FrameError> {
    let header = header(bytes, stream)?;
    let block = header.block_size as usize;
    let channels = usize::from(header.assignment.channels());
    let samples = out.reset(block, channels)?;
    let body = bytes.get(header.len..).ok_or(FrameError::Truncated)?;
    let mut reader = BitReader::new(body);
    for (channel, samples) in samples.chunks_exact_mut(block).enumerate() {
        let width = u32::from(stream.bits) + u32::from(header.assignment.is_side(channel));
        subframe(&mut reader, width, samples)?;
    }
    reader.align();
    let len = header.len + reader.bytes_consumed();
    let crc = reader.read(16)?;
    if u32::from(crc16(0, &bytes[..len])) != crc {
        return Err(FrameError::FrameCrc);
    }
    decorrelate(header.assignment, stream.bits, samples, block)?;
    Ok((header, len + FOOTER_LEN))
}

/// Undo the stereo decorrelation, so both channels hold samples, each of
/// which must fit the stream's width.
fn decorrelate(
    assignment: Assignment,
    bits: u8,
    samples: &mut [i64],
    block: usize,
) -> Result<(), FrameError> {
    let width = u32::from(bits);
    let (first, second) = samples.split_at_mut(block);
    match assignment {
        Assignment::Independent(_) => {}
        Assignment::LeftSide => {
            for (left, side) in first.iter().zip(second.iter_mut()) {
                *side = within(width, *left - *side)?;
            }
        }
        Assignment::SideRight => {
            for (side, right) in first.iter_mut().zip(second.iter()) {
                *side = within(width, *side + *right)?;
            }
        }
        Assignment::MidSide => {
            for (mid, side) in first.iter_mut().zip(second.iter_mut()) {
                let doubled = (*mid << 1) | (*side & 1);
                let left = (doubled + *side) >> 1;
                let right = (doubled - *side) >> 1;
                *mid = within(width, left)?;
                *side = within(width, right)?;
            }
        }
    }
    Ok(())
}

/// Decode one subframe of `width`-bit samples into `out`.
fn subframe(reader: &mut BitReader<'_>, width: u32, out: &mut [i64]) -> Result<(), FrameError> {
    if reader.read(1)? != 0 {
        return Err(FrameError::Reserved);
    }
    let kind = reader.read(6)?;
    let wasted = if reader.read(1)? == 1 {
        reader.unary()?.saturating_add(1)
    } else {
        0
    };
    if wasted >= width {
        return Err(FrameError::Invalid);
    }
    let width = width - wasted;
    match kind {
        0 => {
            let value = reader.read_signed_wide(width)?;
            out.fill(value);
        }
        1 => {
            for sample in out.iter_mut() {
                *sample = reader.read_signed_wide(width)?;
            }
        }
        8..=12 => fixed(reader, width, (kind - 8) as usize, out)?,
        32..=63 => lpc(reader, width, (kind - 31) as usize, out)?,
        _ => return Err(FrameError::Reserved),
    }
    if wasted > 0 {
        for sample in out.iter_mut() {
            *sample <<= wasted;
        }
    }
    Ok(())
}

/// The samples a predictor of `order` starts from, read verbatim.
fn warm_up(
    reader: &mut BitReader<'_>,
    width: u32,
    order: usize,
    out: &mut [i64],
) -> Result<(), FrameError> {
    let warm = out.get_mut(..order).ok_or(FrameError::Invalid)?;
    for sample in warm {
        *sample = reader.read_signed_wide(width)?;
    }
    Ok(())
}

/// The fixed predictor of `order`, at most four, over the `order` samples
/// before the one predicted, oldest first.
pub(crate) fn fixed_prediction(order: usize, past: &[i64]) -> i64 {
    match (order, past) {
        (1, [a]) => *a,
        (2, [a, b]) => 2 * b - a,
        (3, [a, b, c]) => 3 * c - 3 * b + a,
        (4, [a, b, c, d]) => 4 * d - 6 * c + 4 * b - a,
        _ => 0,
    }
}

/// A linear predictor's prediction over the samples before the one predicted,
/// oldest first: the first coefficient weighs the newest.
pub(crate) fn lpc_prediction(coefficients: &[i64], shift: u32, past: &[i64]) -> i64 {
    let sum: i64 = coefficients
        .iter()
        .zip(past.iter().rev())
        .map(|(&coefficient, &sample)| coefficient * sample)
        .sum();
    sum >> shift
}

/// A residual folded to the unsigned form Rice coding takes.
#[cfg(any(test, feature = "encode"))]
pub(crate) const fn fold(residual: i64) -> u64 {
    if residual >= 0 {
        residual.unsigned_abs() << 1
    } else {
        ((residual + 1).unsigned_abs() << 1) | 1
    }
}

/// A folded residual unfolded: an even value is the residual doubled, an odd
/// one a negative residual's magnitude doubled less one.
const fn unfold(folded: u64) -> i64 {
    let magnitude = (folded >> 1).cast_signed();
    if folded & 1 == 0 {
        magnitude
    } else {
        -magnitude - 1
    }
}

/// The largest folded residual the format admits: 32 signed bits short of the
/// most negative.
pub(crate) const MAX_FOLDED: u64 = u32::MAX as u64 - 1;

fn fixed(
    reader: &mut BitReader<'_>,
    width: u32,
    order: usize,
    out: &mut [i64],
) -> Result<(), FrameError> {
    warm_up(reader, width, order, out)?;
    residual(reader, order, out)?;
    for n in order..out.len() {
        let prediction = fixed_prediction(order, &out[n - order..n]);
        out[n] = within(width, out[n] + prediction)?;
    }
    Ok(())
}

fn lpc(
    reader: &mut BitReader<'_>,
    width: u32,
    order: usize,
    out: &mut [i64],
) -> Result<(), FrameError> {
    warm_up(reader, width, order, out)?;
    let precision = reader.read(4)?;
    if precision == 0xF {
        return Err(FrameError::Reserved);
    }
    let shift = u32::try_from(reader.read_signed(5)?).map_err(|_| FrameError::Reserved)?;
    let mut coefficients = [0i64; MAX_LPC_ORDER];
    for coefficient in &mut coefficients[..order] {
        *coefficient = i64::from(reader.read_signed(precision + 1)?);
    }
    residual(reader, order, out)?;
    let coefficients = &coefficients[..order];
    for n in order..out.len() {
        let prediction = lpc_prediction(coefficients, shift, &out[n - order..n]);
        out[n] = within(width, out[n] + prediction)?;
    }
    Ok(())
}

/// `value`, refused if it does not fit `width` signed bits.
pub(crate) fn within(width: u32, value: i64) -> Result<i64, FrameError> {
    let limit = 1i64 << (width - 1);
    if (-limit..limit).contains(&value) {
        Ok(value)
    } else {
        Err(FrameError::Invalid)
    }
}

/// Read the residual of a predictor of `order` into `out[order..]`.
fn residual(reader: &mut BitReader<'_>, order: usize, out: &mut [i64]) -> Result<(), FrameError> {
    let parameter_bits = match reader.read(2)? {
        0 => 4,
        1 => 5,
        _ => return Err(FrameError::Reserved),
    };
    let escape = (1u32 << parameter_bits) - 1;
    let partitions = 1usize << reader.read(4)?;
    let block = out.len();
    if !block.is_multiple_of(partitions) || block / partitions < order {
        return Err(FrameError::Invalid);
    }
    let per_partition = block / partitions;
    let mut at = order;
    for partition in 1..=partitions {
        let end = partition * per_partition;
        let samples = &mut out[at..end];
        let parameter = reader.read(parameter_bits)?;
        if parameter == escape {
            let raw = reader.read(5)?;
            for sample in samples {
                *sample = i64::from(reader.read_signed(raw)?);
            }
        } else {
            for sample in samples {
                *sample = rice(reader, parameter)?;
            }
        }
        at = end;
    }
    Ok(())
}

/// One Rice-coded residual, refused past [`MAX_FOLDED`].
fn rice(reader: &mut BitReader<'_>, parameter: u32) -> Result<i64, FrameError> {
    let quotient = u64::from(reader.unary()?);
    let folded = quotient << parameter | u64::from(reader.read(parameter)?);
    if folded > MAX_FOLDED {
        return Err(FrameError::Invalid);
    }
    Ok(unfold(folded))
}

#[cfg(test)]
#[path = "flac_frame_tests.rs"]
mod tests;

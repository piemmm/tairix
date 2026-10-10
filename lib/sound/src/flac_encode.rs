//! Writing FLAC: a core that emits exactly the constructs it is given, and a
//! chooser that picks constructs which encode well.
//!
//! The core is what a test or a fuzz generator drives, to make a verbatim
//! subframe, an escaped partition or wasted bits that a chooser would never
//! pick; the chooser is what an asset build drives. Both share the decoder's
//! format model — its predictors, its residual fold, its CRCs and digest —
//! so the two halves cannot drift apart by construction. Off by default: no
//! binary that runs on the machine carries it.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_util::mathf;

use crate::bits::BitWriter;
use crate::crc::{crc16, crc8};
use crate::flac::{BLOCK_HEADER_LEN, MARKER, STREAMINFO_LEN};
use crate::flac_frame::{
    common_block, fixed_prediction, fold, lpc_prediction, Assignment, Stream, MAX_BITS, MAX_BLOCK,
    MAX_CHANNELS, MAX_FOLDED, RATE_CODES, WIDTH_CODES,
};
use crate::md5::Md5;

/// Why a stream or a frame could not be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// A rate, width, channel count or block size the format cannot carry.
    Unsupported,
    /// Samples that disagree with the stream: another channel count, values
    /// past its width, a block length its blocking does not allow.
    BadSamples,
    /// A construct that cannot code these samples: wasted bits that are not
    /// zero, a residual past its partition's coding, an order past the block.
    Unrepresentable,
    /// Constructs that would code the frame larger than twice its samples
    /// stored verbatim: past the bound the decoder holds a frame to.
    Oversized,
}

/// What a stream holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Params {
    /// Samples a second.
    pub rate: u32,
    /// Channels, one to eight.
    pub channels: u8,
    /// Bits a sample holds, four to 32.
    pub bits: u8,
}

/// How a stream's frames are sized.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Blocking {
    /// Every frame but the last this many samples, frames numbered in order.
    Fixed(u32),
    /// Frames between these sizes, the last no smaller than one sample,
    /// numbered by their first sample.
    Variable {
        /// The least a frame but the last holds.
        min: u32,
        /// The most a frame holds.
        max: u32,
    },
}

/// How a frame's two channels are coded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stereo {
    /// Each channel on its own.
    Independent,
    /// Left, then left minus right.
    LeftSide,
    /// Left minus right, then right.
    SideRight,
    /// The mean, then left minus right.
    MidSide,
}

impl Stereo {
    /// The channel assignment a frame of `channels` coded so states.
    const fn assignment(self, channels: u8) -> Assignment {
        match self {
            Self::Independent => Assignment::Independent(channels),
            Self::LeftSide => Assignment::LeftSide,
            Self::SideRight => Assignment::SideRight,
            Self::MidSide => Assignment::MidSide,
        }
    }
}

/// A subframe's predictor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Predictor {
    /// One value for the whole block.
    Constant,
    /// Every sample as it is.
    Verbatim,
    /// The fixed polynomial predictor of this order, zero to four.
    Fixed(u8),
    /// A linear predictor.
    Lpc {
        /// One to 32 coefficients, the first weighing the newest sample.
        coefficients: Vec<i32>,
        /// Bits each coefficient takes, one to 15.
        precision: u8,
        /// The prediction's right shift, zero to 15.
        shift: u8,
    },
}

/// How one residual partition is coded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Partition {
    /// Rice coded with this parameter.
    Rice(u8),
    /// Each residual in this many bits, uncoded.
    Escape(u8),
}

/// How a subframe's residual is coded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Residual {
    /// Rice parameters take five bits rather than four.
    pub wide: bool,
    /// The block splits into `2^order` partitions.
    pub order: u8,
    /// Each partition's coding, in order.
    pub partitions: Vec<Partition>,
}

/// One channel's subframe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subframe {
    /// How its samples are predicted.
    pub predictor: Predictor,
    /// Low zero bits every sample shares, left out.
    pub wasted: u8,
    /// How the residual is coded; unread for a constant or verbatim one.
    pub residual: Residual,
}

/// How a frame header states its rate.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum RateCode {
    /// The table's code where the rate has one, else the shortest field.
    #[default]
    Compact,
    /// A byte of kilohertz.
    Kilohertz,
    /// Sixteen bits of hertz.
    Hertz,
    /// Sixteen bits of tens of hertz.
    TensOfHertz,
    /// Refer to `STREAMINFO`.
    StreamInfo,
}

/// How a frame header states its block size.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum BlockCode {
    /// The table's code where the size has one, else the shortest field.
    #[default]
    Compact,
    /// A byte holding the size less one.
    Byte,
    /// Sixteen bits holding the size less one.
    Word,
}

/// The codes a frame header is written with.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Codes {
    /// How the rate is stated.
    pub rate: RateCode,
    /// Refer the sample width to `STREAMINFO` rather than state it.
    pub width_from_streaminfo: bool,
    /// How the block size is stated.
    pub block: BlockCode,
}

/// One frame's constructs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// How two channels are coded.
    pub stereo: Stereo,
    /// A subframe a channel.
    pub subframes: Vec<Subframe>,
    /// How the header states its fields.
    pub codes: Codes,
}

/// A frame written, by its first sample and its place in the frames.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Written {
    pub(crate) first: u64,
    pub(crate) block: u32,
    pub(crate) offset: usize,
    pub(crate) len: usize,
}

/// A stream being written.
pub struct Writer {
    params: Params,
    blocking: Blocking,
    frames: Vec<u8>,
    index: Vec<Written>,
    md5: Md5,
    total: u64,
    vendor: Option<String>,
    fields: Vec<String>,
    blocks: Vec<(u8, Vec<u8>)>,
    seek_every: Option<u64>,
    seek_placeholders: u32,
    padding: Option<u32>,
    state_digest: bool,
}

/// The most a `STREAMINFO` rate field holds.
const MAX_RATE: u32 = (1 << 20) - 1;

/// The most samples `STREAMINFO` counts.
const MAX_TOTAL: u64 = (1 << 36) - 1;

impl Writer {
    /// A stream of `params`, its frames sized by `blocking`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Unsupported`] for parameters the format cannot carry.
    pub fn new(params: Params, blocking: Blocking) -> Result<Self, EncodeError> {
        let (min, max) = match blocking {
            Blocking::Fixed(block) => (block, block),
            Blocking::Variable { min, max } => (min, max),
        };
        let supported = (1..=MAX_RATE).contains(&params.rate)
            && (1..=MAX_CHANNELS).contains(&params.channels)
            && (4..=MAX_BITS).contains(&params.bits)
            && 16 <= min
            && min <= max
            && max <= MAX_BLOCK;
        if !supported {
            return Err(EncodeError::Unsupported);
        }
        Ok(Self {
            params,
            blocking,
            frames: Vec::new(),
            index: Vec::new(),
            md5: Md5::new(),
            total: 0,
            vendor: None,
            fields: Vec::new(),
            blocks: Vec::new(),
            seek_every: None,
            seek_placeholders: 0,
            padding: None,
            state_digest: true,
        })
    }

    /// Carry a Vorbis comment block: `vendor`, then `NAME=value` fields.
    pub fn comments(&mut self, vendor: &str, fields: &[&str]) {
        self.vendor = Some(vendor.into());
        self.fields = fields.iter().map(|&field| field.into()).collect();
    }

    /// Carry a seek table with a point at the first frame of every `every`
    /// samples, then `placeholders` placeholder points.
    pub fn seek_points(&mut self, every: u64, placeholders: u32) {
        self.seek_every = Some(every.max(1));
        self.seek_placeholders = placeholders;
    }

    /// Carry a padding block of `bytes`.
    pub fn padding(&mut self, bytes: u32) {
        self.padding = Some(bytes);
    }

    /// Carry a metadata block of `kind` holding `data` as it is.
    pub fn block(&mut self, kind: u8, data: &[u8]) {
        self.blocks.push((kind, data.to_vec()));
    }

    /// State no digest, as an encoder that could not compute one does.
    pub fn without_digest(&mut self) {
        self.state_digest = false;
    }

    /// Samples a channel the frames hold so far.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.total
    }

    /// Write a frame of `channels`, one slice a channel, coded as `frame`
    /// says.
    ///
    /// # Errors
    ///
    /// [`EncodeError::BadSamples`] for samples the stream cannot hold,
    /// [`EncodeError::Unrepresentable`] for constructs that cannot code
    /// them, or [`EncodeError::Oversized`] for constructs that would code
    /// them past the bound a decoder holds a frame to; nothing is written in
    /// any case.
    pub fn frame(&mut self, channels: &[&[i32]], frame: &Frame) -> Result<(), EncodeError> {
        self.frame_within(channels, frame, true)
    }

    /// [`Self::frame`] with no bound on the frame's size, so a test can make
    /// the frame a decoder refuses.
    #[cfg(test)]
    pub(crate) fn frame_unbounded(
        &mut self,
        channels: &[&[i32]],
        frame: &Frame,
    ) -> Result<(), EncodeError> {
        self.frame_within(channels, frame, false)
    }

    fn frame_within(
        &mut self,
        channels: &[&[i32]],
        frame: &Frame,
        bounded: bool,
    ) -> Result<(), EncodeError> {
        let params = self.params;
        let block = channels.first().map_or(0, |channel| channel.len());
        let limit = 1i64 << (params.bits - 1);
        let fits = channels.len() == usize::from(params.channels)
            && channels.iter().all(|channel| {
                channel.len() == block
                    && channel
                        .iter()
                        .all(|&sample| (-limit..limit).contains(&i64::from(sample)))
            });
        let block = u32::try_from(block).map_err(|_| EncodeError::BadSamples)?;
        if !fits || block == 0 || !self.admits(block) {
            return Err(EncodeError::BadSamples);
        }
        let stereo = frame.stereo != Stereo::Independent;
        if (stereo && params.channels != 2) || frame.subframes.len() != channels.len() {
            return Err(EncodeError::Unrepresentable);
        }
        let mut writer = BitWriter::default();
        let number = match self.blocking {
            Blocking::Fixed(_) => self.index.len() as u64,
            Blocking::Variable { .. } => self.total,
        };
        self.header(&mut writer, frame, block, number)?;
        let stream = Stream {
            rate: params.rate,
            bits: params.bits,
            channels: params.channels,
            max_block: block,
        };
        let header_len = writer.bytes().len();
        let limit = if bounded {
            8 * stream.frame_bound(block, header_len) as u64 - 16
        } else {
            u64::MAX
        };
        let signals = decorrelate(frame.stereo, channels);
        let assignment = frame.stereo.assignment(params.channels);
        for (channel, (signal, subframe)) in signals.iter().zip(&frame.subframes).enumerate() {
            let width = u32::from(params.bits) + u32::from(assignment.is_side(channel));
            write_subframe(&mut writer, signal, width, subframe, limit)?;
        }
        writer.align();
        if writer.bits() > limit {
            return Err(EncodeError::Oversized);
        }
        let crc = crc16(0, writer.bytes());
        writer.write(u64::from(crc), 16);
        let bytes = writer.into_bytes();
        self.index.push(Written {
            first: self.total,
            block,
            offset: self.frames.len(),
            len: bytes.len(),
        });
        self.frames.extend_from_slice(&bytes);
        self.total += u64::from(block);
        self.digest(channels);
        Ok(())
    }

    /// Whether a frame of `block` may follow those written: a frame that
    /// breaks the blocking may only be the last, and only by being shorter.
    fn admits(&self, block: u32) -> bool {
        let (min, max) = match self.blocking {
            Blocking::Fixed(block) => (block, block),
            Blocking::Variable { min, max } => (min, max),
        };
        let last_short = self.index.last().is_some_and(|last| last.block < min);
        block <= max && !last_short && self.total + u64::from(block) <= MAX_TOTAL
    }

    fn header(
        &self,
        writer: &mut BitWriter,
        frame: &Frame,
        block: u32,
        number: u64,
    ) -> Result<(), EncodeError> {
        let variable = matches!(self.blocking, Blocking::Variable { .. });
        writer.write(0x7FFC, 15);
        writer.write(u64::from(variable), 1);
        let (block_code, block_field) = block_code(block, frame.codes.block)?;
        let (rate_code, rate_field) = rate_code(self.params.rate, frame.codes.rate)?;
        writer.write(u64::from(block_code), 4);
        writer.write(u64::from(rate_code), 4);
        let assignment = frame.stereo.assignment(self.params.channels);
        writer.write(u64::from(assignment.code()), 4);
        let width = WIDTH_CODES
            .iter()
            .position(|&width| width == Some(self.params.bits))
            .filter(|_| !frame.codes.width_from_streaminfo)
            .unwrap_or(0);
        writer.write(width as u64, 3);
        writer.write(0, 1);
        coded_number(writer, number);
        if let Some((value, bits)) = block_field {
            writer.write(u64::from(value), bits);
        }
        if let Some((value, bits)) = rate_field {
            writer.write(u64::from(value), bits);
        }
        let crc = crc8(writer.bytes());
        writer.write(u64::from(crc), 8);
        Ok(())
    }

    fn digest(&mut self, channels: &[&[i32]]) {
        let width = usize::from(self.params.bits).div_ceil(8);
        let mut bytes = Vec::with_capacity(channels[0].len() * channels.len() * width);
        for index in 0..channels[0].len() {
            for channel in channels {
                bytes.extend_from_slice(&channel[index].to_le_bytes()[..width]);
            }
        }
        self.md5.update(&bytes);
    }

    /// The metadata blocks, `STREAMINFO` first, as kind and data.
    pub(crate) fn metadata(&self) -> Vec<(u8, Vec<u8>)> {
        let mut blocks = vec![(0, self.streaminfo().to_vec())];
        if let Some(every) = self.seek_every {
            blocks.push((3, self.seek_table(every)));
        }
        if let Some(vendor) = &self.vendor {
            let length = |len: usize| u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes();
            let mut data = Vec::new();
            data.extend_from_slice(&length(vendor.len()));
            data.extend_from_slice(vendor.as_bytes());
            data.extend_from_slice(&length(self.fields.len()));
            for text in &self.fields {
                data.extend_from_slice(&length(text.len()));
                data.extend_from_slice(text.as_bytes());
            }
            blocks.push((4, data));
        }
        blocks.extend(self.blocks.iter().cloned());
        if let Some(padding) = self.padding {
            blocks.push((1, vec![0; usize::try_from(padding).unwrap_or(0)]));
        }
        blocks
    }

    fn streaminfo(&self) -> [u8; STREAMINFO_LEN] {
        let (min_block, max_block) = match self.blocking {
            Blocking::Fixed(block) => (block, block),
            Blocking::Variable { min, max } => (min, max),
        };
        let lens = self.index.iter().map(|frame| frame.len);
        let min_frame = lens.clone().min().unwrap_or(0);
        let max_frame = lens.max().unwrap_or(0);
        let mut info = [0u8; STREAMINFO_LEN];
        info[0..2].copy_from_slice(&min_block.to_be_bytes()[2..]);
        info[2..4].copy_from_slice(&max_block.to_be_bytes()[2..]);
        let u24 = |len: usize| u32::try_from(len).unwrap_or(0).to_be_bytes();
        info[4..7].copy_from_slice(&u24(min_frame)[1..]);
        info[7..10].copy_from_slice(&u24(max_frame)[1..]);
        let packed = u64::from(self.params.rate) << 44
            | u64::from(self.params.channels - 1) << 41
            | u64::from(self.params.bits - 1) << 36
            | self.total;
        info[10..18].copy_from_slice(&packed.to_be_bytes());
        if self.state_digest {
            info[18..].copy_from_slice(&self.md5.clone().finish());
        }
        info
    }

    fn seek_table(&self, every: u64) -> Vec<u8> {
        let mut table = Vec::new();
        let mut next = 0;
        for frame in &self.index {
            if frame.first >= next {
                table.extend_from_slice(&frame.first.to_be_bytes());
                table.extend_from_slice(&(frame.offset as u64).to_be_bytes());
                table.extend_from_slice(&frame.block.to_be_bytes()[2..]);
                next = (frame.first / every + 1) * every;
            }
        }
        for _ in 0..self.seek_placeholders {
            table.extend_from_slice(&u64::MAX.to_be_bytes());
            table.extend_from_slice(&[0; 10]);
        }
        table
    }

    /// The stream in Ogg as the FLAC mapping lays it, as the logical stream
    /// `serial`: the mapping's packet alone on the first page, a metadata
    /// block a packet, then a frame a packet, ending a page once it holds
    /// `per_page` bytes.
    #[must_use]
    pub fn finish_ogg(self, serial: u32, per_page: usize) -> Vec<u8> {
        let mut blocks = self.metadata();
        let streaminfo = blocks.remove(0).1;
        let headers = u16::try_from(blocks.len()).unwrap_or(0);
        let mut pages = crate::ogg::PageWriter::new(serial);
        pages.packet(
            &crate::ogg::flac_header(&streaminfo, headers, blocks.is_empty()),
            0,
        );
        pages.flush(false);
        let count = blocks.len();
        for (at, (kind, data)) in blocks.into_iter().enumerate() {
            let mut packet = block_header(kind, data.len(), at + 1 == count).to_vec();
            packet.extend_from_slice(&data);
            pages.packet(&packet, 0);
        }
        if count > 0 {
            pages.flush(false);
        }
        let frames = self.index.len();
        for (at, frame) in self.index.iter().enumerate() {
            let bytes = &self.frames[frame.offset..frame.offset + frame.len];
            pages.packet(bytes, frame.first + u64::from(frame.block));
            if at + 1 == frames || pages.filled() >= per_page {
                pages.flush(at + 1 == frames);
            }
        }
        if frames == 0 {
            pages.flush(true);
        }
        pages.into_bytes()
    }

    /// The native stream: the marker, the metadata blocks, the frames.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let mut out = MARKER.to_vec();
        let blocks = self.metadata();
        let count = blocks.len();
        for (at, (kind, data)) in blocks.into_iter().enumerate() {
            out.extend_from_slice(&block_header(kind, data.len(), at + 1 == count));
            out.extend_from_slice(&data);
        }
        out.extend_from_slice(&self.frames);
        out
    }
}

/// A metadata block's header.
pub(crate) fn block_header(kind: u8, len: usize, last: bool) -> [u8; BLOCK_HEADER_LEN] {
    let len = u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes();
    [kind | if last { 0x80 } else { 0 }, len[1], len[2], len[3]]
}

/// The block-size code for `block`, and the field after the header it needs.
fn block_code(block: u32, code: BlockCode) -> Result<(u8, Option<(u32, u32)>), EncodeError> {
    let common = (1..16u8).find(|&code| common_block(u32::from(code)) == Some(block));
    let byte = (block <= 256).then_some((6, Some((block - 1, 8))));
    let word = (7, Some((block - 1, 16)));
    match code {
        BlockCode::Compact => Ok(common.map_or(byte.unwrap_or(word), |common| (common, None))),
        BlockCode::Byte => byte.ok_or(EncodeError::Unrepresentable),
        BlockCode::Word => Ok(word),
    }
}

/// The rate code for `rate`, and the field after the header it needs.
fn rate_code(rate: u32, code: RateCode) -> Result<(u8, Option<(u32, u32)>), EncodeError> {
    let table = (1..=11u8).find(|&code| RATE_CODES[usize::from(code) - 1] == rate);
    let kilohertz = (rate.is_multiple_of(1_000) && rate / 1_000 <= 255).then_some(rate / 1_000);
    let hertz = (rate <= 65_535).then_some(rate);
    let tens = (rate.is_multiple_of(10) && rate / 10 <= 65_535).then_some(rate / 10);
    Ok(match code {
        RateCode::StreamInfo => (0, None),
        RateCode::Kilohertz => (
            12,
            Some((kilohertz.ok_or(EncodeError::Unrepresentable)?, 8)),
        ),
        RateCode::Hertz => (13, Some((hertz.ok_or(EncodeError::Unrepresentable)?, 16))),
        RateCode::TensOfHertz => (14, Some((tens.ok_or(EncodeError::Unrepresentable)?, 16))),
        RateCode::Compact => match (table, kilohertz, hertz, tens) {
            (Some(code), ..) => (code, None),
            (_, Some(khz), ..) => (12, Some((khz, 8))),
            (_, _, Some(hz), _) => (13, Some((hz, 16))),
            (_, _, _, Some(tens)) => (14, Some((tens, 16))),
            _ => (0, None),
        },
    })
}

/// The UTF-8-style coded number a header carries.
fn coded_number(writer: &mut BitWriter, number: u64) {
    if number < 0x80 {
        writer.write(number, 8);
        return;
    }
    let mut extra = 1;
    while extra < 6 && number >> (6 * extra + 6 - extra) != 0 {
        extra += 1;
    }
    let lead_bits = 6 - extra;
    let ones = (0xFF00u64 >> (extra + 1)) & 0xFF;
    writer.write(ones | (number >> (6 * extra)) & ((1 << lead_bits) - 1), 8);
    for at in (0..extra).rev() {
        writer.write(0x80 | ((number >> (6 * at)) & 0x3F), 8);
    }
}

/// The signals a frame's subframes code, a channel's samples widened.
fn decorrelate(stereo: Stereo, channels: &[&[i32]]) -> Vec<Vec<i64>> {
    let wide = |channel: &[i32]| {
        channel
            .iter()
            .map(|&sample| i64::from(sample))
            .collect::<Vec<_>>()
    };
    let pair = || {
        let left = wide(channels[0]);
        let right = wide(channels[1]);
        let side: Vec<i64> = left.iter().zip(&right).map(|(l, r)| l - r).collect();
        (left, right, side)
    };
    match stereo {
        Stereo::Independent => channels.iter().map(|channel| wide(channel)).collect(),
        Stereo::LeftSide => {
            let (left, _, side) = pair();
            vec![left, side]
        }
        Stereo::SideRight => {
            let (_, right, side) = pair();
            vec![side, right]
        }
        Stereo::MidSide => {
            let (left, right, side) = pair();
            let mid = left.iter().zip(&right).map(|(l, r)| (l + r) >> 1).collect();
            vec![mid, side]
        }
    }
}

/// The residual `predictor` leaves of `signal`, from its warm-up on, or
/// `None` for a predictor that cannot code it.
fn residuals(signal: &[i64], predictor: &Predictor) -> Option<Vec<i64>> {
    match predictor {
        Predictor::Constant | Predictor::Verbatim => Some(Vec::new()),
        Predictor::Fixed(order) => {
            let order = usize::from(*order);
            (order <= signal.len()).then(|| {
                (order..signal.len())
                    .map(|n| signal[n] - fixed_prediction(order, &signal[n - order..n]))
                    .collect()
            })
        }
        Predictor::Lpc {
            coefficients,
            shift,
            ..
        } => {
            let wide: Vec<i64> = coefficients.iter().map(|&c| i64::from(c)).collect();
            let shift = u32::from(*shift);
            let order = wide.len();
            (order <= signal.len()).then(|| {
                (order..signal.len())
                    .map(|n| signal[n] - lpc_prediction(&wide, shift, &signal[n - order..n]))
                    .collect()
            })
        }
    }
}

/// Write one subframe, refusing to grow `writer` past `limit` bits.
fn write_subframe(
    writer: &mut BitWriter,
    signal: &[i64],
    width: u32,
    subframe: &Subframe,
    limit: u64,
) -> Result<(), EncodeError> {
    let wasted = u32::from(subframe.wasted);
    let low = (1i64 << wasted) - 1;
    if wasted >= width || signal.iter().any(|&sample| sample & low != 0) {
        return Err(EncodeError::Unrepresentable);
    }
    let shifted: Vec<i64> = signal.iter().map(|&sample| sample >> wasted).collect();
    let width = width - wasted;
    let kind: u64 = match &subframe.predictor {
        Predictor::Constant => 0,
        Predictor::Verbatim => 1,
        Predictor::Fixed(order) if *order <= 4 => 8 + u64::from(*order),
        Predictor::Lpc { coefficients, .. } if (1..=32).contains(&coefficients.len()) => {
            31 + coefficients.len() as u64
        }
        _ => return Err(EncodeError::Unrepresentable),
    };
    writer.write(0, 1);
    writer.write(kind, 6);
    if wasted > 0 {
        writer.write(1, 1);
        writer.unary(u64::from(wasted - 1));
    } else {
        writer.write(0, 1);
    }
    match &subframe.predictor {
        Predictor::Constant => {
            if shifted.iter().any(|&sample| sample != shifted[0]) {
                return Err(EncodeError::Unrepresentable);
            }
            writer.write_signed(shifted[0], width);
            Ok(())
        }
        Predictor::Verbatim => {
            for &sample in &shifted {
                writer.write_signed(sample, width);
            }
            Ok(())
        }
        Predictor::Fixed(order) => {
            let order = usize::from(*order);
            let residual =
                residuals(&shifted, &subframe.predictor).ok_or(EncodeError::Unrepresentable)?;
            for &sample in &shifted[..order] {
                writer.write_signed(sample, width);
            }
            write_residual(
                writer,
                &residual,
                order,
                shifted.len(),
                &subframe.residual,
                limit,
            )
        }
        Predictor::Lpc {
            coefficients,
            precision,
            shift,
        } => {
            let order = coefficients.len();
            let precision = u32::from(*precision);
            let range = 1i64 << precision.saturating_sub(1);
            let fits = coefficients
                .iter()
                .all(|&coefficient| (-range..range).contains(&i64::from(coefficient)));
            if !(1..=15).contains(&precision) || *shift > 15 || !fits {
                return Err(EncodeError::Unrepresentable);
            }
            let residual =
                residuals(&shifted, &subframe.predictor).ok_or(EncodeError::Unrepresentable)?;
            for &sample in &shifted[..order] {
                writer.write_signed(sample, width);
            }
            writer.write(u64::from(precision - 1), 4);
            writer.write(u64::from(*shift), 5);
            for &coefficient in coefficients {
                writer.write_signed(i64::from(coefficient), precision);
            }
            write_residual(
                writer,
                &residual,
                order,
                shifted.len(),
                &subframe.residual,
                limit,
            )
        }
    }
}

/// Write a residual as `coding` partitions it, each partition's size known
/// before it is written so `writer` never grows past `limit` bits.
fn write_residual(
    writer: &mut BitWriter,
    residual: &[i64],
    order: usize,
    block: usize,
    coding: &Residual,
    limit: u64,
) -> Result<(), EncodeError> {
    let partitions = 1usize << coding.order;
    let parameter_bits = if coding.wide { 5 } else { 4 };
    let escape = (1u8 << parameter_bits) - 1;
    if coding.order > 15
        || coding.partitions.len() != partitions
        || !block.is_multiple_of(partitions)
        || block / partitions < order
    {
        return Err(EncodeError::Unrepresentable);
    }
    writer.write(u64::from(coding.wide), 2);
    writer.write(u64::from(coding.order), 4);
    let per = block / partitions;
    let mut at = 0;
    for (index, partition) in coding.partitions.iter().enumerate() {
        let count = if index == 0 { per - order } else { per };
        let values = &residual[at..at + count];
        match *partition {
            Partition::Rice(parameter) if parameter < escape => {
                if values.iter().any(|&value| {
                    !(-(1i64 << 31)..1i64 << 31).contains(&value) || fold(value) > MAX_FOLDED
                }) {
                    return Err(EncodeError::Unrepresentable);
                }
                let size = values
                    .iter()
                    .map(|&value| (fold(value) >> parameter) + 1 + u64::from(parameter))
                    .sum::<u64>();
                if writer.bits() + u64::from(parameter_bits) + size > limit {
                    return Err(EncodeError::Oversized);
                }
                writer.write(u64::from(parameter), parameter_bits);
                for &value in values {
                    let folded = fold(value);
                    writer.unary(folded >> parameter);
                    writer.write(folded & ((1u64 << parameter) - 1), u32::from(parameter));
                }
            }
            Partition::Escape(bits) if bits <= 31 => {
                let range = if bits == 0 { 0 } else { 1i64 << (bits - 1) };
                let fits = values.iter().all(|&value| {
                    if bits == 0 {
                        value == 0
                    } else {
                        (-range..range).contains(&value)
                    }
                });
                if !fits {
                    return Err(EncodeError::Unrepresentable);
                }
                let size = u64::from(parameter_bits) + 5 + values.len() as u64 * u64::from(bits);
                if writer.bits() + size > limit {
                    return Err(EncodeError::Oversized);
                }
                writer.write(u64::from(escape), parameter_bits);
                writer.write(u64::from(bits), 5);
                for &value in values {
                    writer.write_signed(value, u32::from(bits));
                }
            }
            _ => return Err(EncodeError::Unrepresentable),
        }
        at += count;
    }
    Ok(())
}

/// How [`encode`] chooses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Options {
    /// Samples a frame.
    pub block: u32,
    /// The longest linear predictor tried; zero tries fixed predictors only.
    pub max_lpc_order: u8,
    /// The most partitions a residual is split into, as a power of two.
    pub max_partition_order: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            block: 4096,
            max_lpc_order: 12,
            max_partition_order: 8,
        }
    }
}

/// Encode `interleaved`, a frame's worth of samples a channel after
/// another, choosing for each frame the constructs that code it smallest.
///
/// # Errors
///
/// [`EncodeError::Unsupported`] for parameters the format cannot carry, or
/// [`EncodeError::BadSamples`] for samples past the width or not whole
/// frames.
pub fn encode(
    params: Params,
    interleaved: &[i32],
    options: Options,
) -> Result<Writer, EncodeError> {
    let channels = usize::from(params.channels);
    if !interleaved.len().is_multiple_of(channels.max(1)) {
        return Err(EncodeError::BadSamples);
    }
    let mut writer = Writer::new(params, Blocking::Fixed(options.block))?;
    let block = usize::try_from(options.block).map_err(|_| EncodeError::Unsupported)?;
    for chunk in interleaved.chunks(block * channels) {
        let split: Vec<Vec<i32>> = (0..channels)
            .map(|channel| {
                chunk
                    .iter()
                    .skip(channel)
                    .step_by(channels)
                    .copied()
                    .collect()
            })
            .collect();
        let slices: Vec<&[i32]> = split.iter().map(Vec::as_slice).collect();
        let frame = choose(params, &slices, options);
        writer.frame(&slices, &frame)?;
    }
    Ok(writer)
}

/// The frame that codes `channels`, one slice a channel, smallest: the
/// chooser [`encode`] runs on every frame, for a caller driving
/// [`Writer::frame`] itself.
#[must_use]
pub fn choose(params: Params, channels: &[&[i32]], options: Options) -> Frame {
    let candidates: &[Stereo] = if channels.len() == 2 {
        &[
            Stereo::Independent,
            Stereo::LeftSide,
            Stereo::SideRight,
            Stereo::MidSide,
        ]
    } else {
        &[Stereo::Independent]
    };
    let mut best: Option<(u64, Frame)> = None;
    for &stereo in candidates {
        let signals = decorrelate(stereo, channels);
        let mut bits = 0;
        let mut subframes = Vec::new();
        let assignment = stereo.assignment(params.channels);
        for (channel, signal) in signals.iter().enumerate() {
            let width = u32::from(params.bits) + u32::from(assignment.is_side(channel));
            let (cost, subframe) = best_subframe(signal, width, options);
            bits += cost;
            subframes.push(subframe);
        }
        if best.as_ref().is_none_or(|(cost, _)| bits < *cost) {
            best = Some((
                bits,
                Frame {
                    stereo,
                    subframes,
                    codes: Codes::default(),
                },
            ));
        }
    }
    best.map_or_else(
        || Frame {
            stereo: Stereo::Independent,
            subframes: Vec::new(),
            codes: Codes::default(),
        },
        |(_, frame)| frame,
    )
}

/// The subframe that codes `signal` smallest, and its size in bits.
fn best_subframe(signal: &[i64], width: u32, options: Options) -> (u64, Subframe) {
    let shared = signal.iter().fold(0i64, |shared, &sample| shared | sample);
    let wasted = if shared == 0 {
        0
    } else {
        shared.trailing_zeros().min(width - 1)
    };
    let shifted: Vec<i64> = signal.iter().map(|&sample| sample >> wasted).collect();
    let narrow = width - wasted;
    let header = 8 + if wasted > 0 { u64::from(wasted) } else { 0 };
    let verbatim_residual = Residual {
        wide: false,
        order: 0,
        partitions: vec![Partition::Rice(0)],
    };
    let wasted_bits = u8::try_from(wasted).unwrap_or(0);
    if shifted.iter().all(|&sample| sample == shifted[0]) {
        return (
            header + u64::from(narrow),
            Subframe {
                predictor: Predictor::Constant,
                wasted: wasted_bits,
                residual: verbatim_residual,
            },
        );
    }
    let mut best = (
        header + shifted.len() as u64 * u64::from(narrow),
        Subframe {
            predictor: Predictor::Verbatim,
            wasted: wasted_bits,
            residual: verbatim_residual,
        },
    );
    let mut consider = |predictor: Predictor, extra: u64| {
        let order = match &predictor {
            Predictor::Fixed(order) => usize::from(*order),
            Predictor::Lpc { coefficients, .. } => coefficients.len(),
            _ => 0,
        };
        let Some(residual) = residuals(&shifted, &predictor) else {
            return;
        };
        let Some((cost, coding)) = best_coding(&residual, order, shifted.len(), options) else {
            return;
        };
        let total = header + order as u64 * u64::from(narrow) + extra + cost;
        if total < best.0 {
            best = (
                total,
                Subframe {
                    predictor,
                    wasted: wasted_bits,
                    residual: coding,
                },
            );
        }
    };
    for order in 0..=4u8.min(u8::try_from(shifted.len() - 1).unwrap_or(4)) {
        consider(Predictor::Fixed(order), 0);
    }
    let max_order = usize::from(options.max_lpc_order).min(shifted.len() - 1);
    let precision = precision(narrow, shifted.len());
    for (coefficients, shift) in lpc_predictors(&shifted, max_order, precision) {
        let extra = 4 + 5 + coefficients.len() as u64 * u64::from(precision);
        consider(
            Predictor::Lpc {
                coefficients,
                precision: u8::try_from(precision).unwrap_or(15),
                shift,
            },
            extra,
        );
    }
    best
}

/// The coefficient precision for a signal of `width` bits in blocks of
/// `block`, as the reference encoder chooses it.
fn precision(width: u32, block: usize) -> u32 {
    if width < 16 {
        (2 + width / 2).max(5)
    } else if width == 16 {
        match block {
            0..=192 => 7,
            193..=384 => 8,
            385..=576 => 9,
            577..=1152 => 10,
            1153..=2304 => 11,
            2305..=4608 => 12,
            _ => 13,
        }
    } else if block <= 384 {
        13
    } else if block <= 1152 {
        14
    } else {
        15
    }
}

/// Quantised linear predictors of every order to `max_order` for `signal`:
/// Levinson-Durbin over its Tukey-windowed autocorrelation.
#[allow(
    clippy::cast_precision_loss,
    reason = "samples of at most 33 bits and block offsets under 2^16 are exact in f64"
)]
fn lpc_predictors(signal: &[i64], max_order: usize, precision: u32) -> Vec<(Vec<i32>, u8)> {
    let n = signal.len();
    if max_order == 0 || n < 2 {
        return Vec::new();
    }
    let taper = (n / 4).max(1);
    let window = |index: usize| -> f64 {
        let edge = index.min(n - 1 - index);
        if edge >= taper {
            1.0
        } else {
            0.5 - 0.5 * mathf::cos(core::f64::consts::PI * edge as f64 / taper as f64)
        }
    };
    let windowed: Vec<f64> = signal
        .iter()
        .enumerate()
        .map(|(index, &sample)| sample as f64 * window(index))
        .collect();
    let mut autocorrelation = vec![0.0f64; max_order + 1];
    for (lag, value) in autocorrelation.iter_mut().enumerate() {
        *value = windowed[lag..]
            .iter()
            .zip(&windowed)
            .map(|(a, b)| a * b)
            .sum();
    }
    if autocorrelation[0] <= 0.0 {
        return Vec::new();
    }
    let mut lpc = vec![0.0f64; max_order];
    let mut error = autocorrelation[0];
    let mut predictors = Vec::new();
    for order in 1..=max_order {
        let mut reflection = -autocorrelation[order];
        for j in 0..order - 1 {
            reflection -= lpc[j] * autocorrelation[order - 1 - j];
        }
        reflection /= error;
        let previous = lpc.clone();
        lpc[order - 1] = reflection;
        for j in 0..order - 1 {
            lpc[j] = previous[j] + reflection * previous[order - 2 - j];
        }
        error *= 1.0 - reflection * reflection;
        let predictor: Vec<f64> = lpc[..order]
            .iter()
            .map(|&coefficient| -coefficient)
            .collect();
        if let Some(quantised) = quantise(&predictor, precision) {
            predictors.push(quantised);
        }
        if error <= 0.0 {
            break;
        }
    }
    predictors
}

/// `coefficients` as integers of `precision` bits and the shift that scales
/// them, rounding with the error carried forward.
#[allow(
    clippy::cast_possible_truncation,
    reason = "each value is rounded and clamped to the coefficient range first"
)]
fn quantise(coefficients: &[f64], precision: u32) -> Option<(Vec<i32>, u8)> {
    let largest = coefficients
        .iter()
        .fold(0.0f64, |largest, &c| largest.max(mathf::fabs(c)));
    if largest <= 0.0 || !largest.is_finite() {
        return None;
    }
    let mut exponent = 0i32;
    let mut scaled = largest;
    while scaled >= 1.0 {
        scaled /= 2.0;
        exponent += 1;
    }
    while scaled < 0.5 {
        scaled *= 2.0;
        exponent -= 1;
    }
    let shift = (i32::try_from(precision).ok()? - 1 - exponent).clamp(0, 15);
    let limit = (1i32 << (precision - 1)) - 1;
    let factor = f64::from(1u32 << shift);
    let mut carried = 0.0f64;
    let mut quantised = Vec::with_capacity(coefficients.len());
    for &coefficient in coefficients {
        carried += coefficient * factor;
        let rounded = mathf::round(carried).clamp(f64::from(-limit - 1), f64::from(limit));
        carried -= rounded;
        quantised.push(rounded as i32);
    }
    Some((quantised, u8::try_from(shift).ok()?))
}

/// The partitioning that codes `residual` smallest, and its size in bits.
fn best_coding(
    residual: &[i64],
    order: usize,
    block: usize,
    options: Options,
) -> Option<(u64, Residual)> {
    let folded: Option<Vec<u64>> = residual
        .iter()
        .map(|&value| Some(fold(value)).filter(|&folded| folded <= MAX_FOLDED))
        .collect();
    let folded = folded?;
    let mut best: Option<(u64, Residual)> = None;
    for partition_order in 0..=options.max_partition_order.min(15) {
        let partitions = 1usize << partition_order;
        if !block.is_multiple_of(partitions) || block / partitions < order {
            break;
        }
        let per = block / partitions;
        let mut cost = 2 + 4;
        let mut codings = Vec::with_capacity(partitions);
        let mut wide = false;
        let mut at = 0;
        for index in 0..partitions {
            let count = if index == 0 { per - order } else { per };
            let (bits, coding, needs_wide) =
                best_partition(&folded[at..at + count], &residual[at..at + count]);
            cost += bits;
            wide |= needs_wide;
            codings.push(coding);
            at += count;
        }
        cost += partitions as u64 * if wide { 5 } else { 4 };
        if best.as_ref().is_none_or(|(best, _)| cost < *best) {
            best = Some((
                cost,
                Residual {
                    wide,
                    order: partition_order,
                    partitions: codings,
                },
            ));
        }
    }
    best
}

/// The coding of one partition: the best Rice parameter or an escape, its
/// bits less the parameter field, and whether the parameter needs five bits.
fn best_partition(folded: &[u64], residual: &[i64]) -> (u64, Partition, bool) {
    let count = folded.len() as u64;
    let rice_cost = |parameter: u32| -> u64 {
        count * u64::from(parameter + 1)
            + folded.iter().map(|&value| value >> parameter).sum::<u64>()
    };
    let sum: u64 = folded.iter().sum();
    let guess = sum
        .checked_div(count)
        .and_then(u64::checked_ilog2)
        .unwrap_or(0);
    let mut best = (u64::MAX, 0u32);
    for parameter in guess.saturating_sub(1)..=(guess + 1).min(30) {
        let cost = rice_cost(parameter);
        if cost < best.0 {
            best = (cost, parameter);
        }
    }
    let widest = residual
        .iter()
        .map(|&value| match value.cmp(&0) {
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 65 - value.leading_zeros(),
            core::cmp::Ordering::Less => 65 - (!value).leading_zeros(),
        })
        .max()
        .unwrap_or(0);
    let escape = 5 + count * u64::from(widest);
    if let Some(bits) = u8::try_from(widest)
        .ok()
        .filter(|&bits| bits <= 31 && escape < best.0)
    {
        return (escape, Partition::Escape(bits), false);
    }
    let parameter = u8::try_from(best.1).unwrap_or(30);
    (best.0, Partition::Rice(parameter), parameter >= 15)
}

#[cfg(test)]
#[path = "flac_encode_tests.rs"]
mod tests;

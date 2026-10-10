//! `codec-v1`: the audio codec seam (`plans/SOUND.md` SND8, `plans/SUPPLIERS.md`
//! SL1's `Codec` role).
//!
//! A codec is a device of its own, driven by its own driver, while the audio
//! channel is the digital audio interface's: the interface's driver serves the
//! mixer and calls its codec's endpoint ([`CODEC_ENDPOINTS`]), naming the
//! [`LinkRequest`] discovery gave it, for what the codec accepts and for the
//! gain it owns.

use crate::driver::audio::{
    GainRange, Rate, RateSupport, GAIN_RANGE_WIRE_LEN, RATE_SUPPORT_WIRE_LEN,
};
use crate::hwlink::{LinkRequest, LinkRole};
use crate::hwtree::{HwResource, NodeEndpointBlock};
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::{DriverError, Errno};

/// The endpoints codecs serve, indexed by the codec's node id. Binding one
/// takes the node's codec [`LinkDuty`](crate::hwlink::LinkDuty).
pub const CODEC_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"CD");

/// How a digital audio interface frames samples on its data line.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DaiFormat {
    /// Philips I2S: the frame clock low for the left channel, data one bit
    /// clock after each edge.
    I2s = 1,
    /// Left justified: data from each frame-clock edge, the left channel high.
    LeftJustified = 2,
    /// Right justified: data ending at each frame-clock edge.
    RightJustified = 3,
    /// DSP mode A: a one-bit frame pulse, data one bit clock after it.
    DspA = 4,
    /// DSP mode B: a one-bit frame pulse, data with it.
    DspB = 5,
}

impl DaiFormat {
    /// The format a `simple-audio-card` `format` string names.
    #[must_use]
    pub fn from_binding(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"i2s" => Self::I2s,
            b"left_j" => Self::LeftJustified,
            b"right_j" => Self::RightJustified,
            b"dsp_a" => Self::DspA,
            b"dsp_b" => Self::DspB,
            _ => return None,
        })
    }

    const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::I2s,
            2 => Self::LeftJustified,
            3 => Self::RightJustified,
            4 => Self::DspA,
            5 => Self::DspB,
            _ => return None,
        })
    }
}

/// Which of a link's clocks run inverted, as a `simple-audio-card`'s
/// `bitclock-inversion` and `frame-inversion` state.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClockInversion {
    /// Neither: data is sampled on the bit clock's rising edge, and the frame
    /// clock has the format's own sense.
    Normal,
    /// The bit clock: data is sampled on its falling edge.
    BitClock,
    /// The frame clock, against the format's sense.
    FrameClock,
    /// Both.
    Both,
}

impl ClockInversion {
    /// The inversion of the clocks that are inverted.
    #[must_use]
    pub const fn of(bit_clock: bool, frame_clock: bool) -> Self {
        match (bit_clock, frame_clock) {
            (false, false) => Self::Normal,
            (true, false) => Self::BitClock,
            (false, true) => Self::FrameClock,
            (true, true) => Self::Both,
        }
    }

    /// Whether the bit clock is inverted.
    #[must_use]
    pub const fn bit_clock(self) -> bool {
        matches!(self, Self::BitClock | Self::Both)
    }

    /// Whether the frame clock is inverted.
    #[must_use]
    pub const fn frame_clock(self) -> bool {
        matches!(self, Self::FrameClock | Self::Both)
    }
}

/// One link between a CPU's digital audio interface and a codec's: the
/// selector a codec [`LinkRequest`] carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DaiLink {
    /// The framing both sides use.
    pub format: DaiFormat,
    /// The codec drives the bit clock; otherwise the CPU side does.
    pub codec_drives_bit_clock: bool,
    /// The codec drives the frame clock; otherwise the CPU side does.
    pub codec_drives_frame_clock: bool,
    /// The clocks running inverted.
    pub inversion: ClockInversion,
    /// Which of the CPU node's interfaces the link is on.
    pub cpu_dai: u8,
    /// Which of the codec node's interfaces the link is on.
    pub codec_dai: u32,
}

/// Selector cell 0: the format, the clock sides and inversions, and the CPU
/// interface.
mod cell0 {
    pub const FORMAT: u32 = 0xFF;
    pub const CODEC_BIT_CLOCK: u32 = 1 << 8;
    pub const CODEC_FRAME_CLOCK: u32 = 1 << 9;
    pub const BIT_CLOCK_INVERTED: u32 = 1 << 10;
    pub const FRAME_CLOCK_INVERTED: u32 = 1 << 11;
    pub const CPU_DAI_SHIFT: u32 = 16;
    pub const DEFINED: u32 = FORMAT
        | CODEC_BIT_CLOCK
        | CODEC_FRAME_CLOCK
        | BIT_CLOCK_INVERTED
        | FRAME_CLOCK_INVERTED
        | (0xFF << CPU_DAI_SHIFT);
}

impl DaiLink {
    /// The two selector cells that carry the link.
    #[must_use]
    pub const fn to_cells(&self) -> [u32; 2] {
        let mut first = self.format as u32 | ((self.cpu_dai as u32) << cell0::CPU_DAI_SHIFT);
        if self.codec_drives_bit_clock {
            first |= cell0::CODEC_BIT_CLOCK;
        }
        if self.codec_drives_frame_clock {
            first |= cell0::CODEC_FRAME_CLOCK;
        }
        if self.inversion.bit_clock() {
            first |= cell0::BIT_CLOCK_INVERTED;
        }
        if self.inversion.frame_clock() {
            first |= cell0::FRAME_CLOCK_INVERTED;
        }
        [first, self.codec_dai]
    }

    /// The link `cells` carry.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for anything but two cells, an unknown format, or
    /// a set bit no field defines.
    pub fn from_cells(cells: &[u32]) -> Result<Self, Errno> {
        let &[first, codec_dai] = cells else {
            return Err(Errno::BadMagic);
        };
        if first & !cell0::DEFINED != 0 {
            return Err(Errno::BadMagic);
        }
        let format = DaiFormat::from_u8((first & cell0::FORMAT) as u8).ok_or(Errno::BadMagic)?;
        Ok(Self {
            format,
            codec_drives_bit_clock: first & cell0::CODEC_BIT_CLOCK != 0,
            codec_drives_frame_clock: first & cell0::CODEC_FRAME_CLOCK != 0,
            inversion: ClockInversion::of(
                first & cell0::BIT_CLOCK_INVERTED != 0,
                first & cell0::FRAME_CLOCK_INVERTED != 0,
            ),
            cpu_dai: u8::try_from(first >> cell0::CPU_DAI_SHIFT).map_err(|_| Errno::BadMagic)?,
            codec_dai,
        })
    }
}

/// Magic opening every request frame (`"CDCR"`).
pub const CODEC_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"CDCR");

/// The `codec-v1` protocol version.
pub const CODEC_VERSION_V1: u16 = 1;

/// A request's header: magic, version, operation and a reserved byte.
const HEADER_LEN: usize = 8;

/// A reply's header: the status word and a reserved word.
const REPLY_HEADER_LEN: usize = 8;

/// A configure or gain request's body after the link record.
const ARGUMENT_LEN: usize = 8;

/// A facts reply's body: the rates, then the widths, the framings and the
/// flags, a reserved byte, and the gain range.
const FACTS_LEN: usize = RATE_SUPPORT_WIRE_LEN + 4 + GAIN_RANGE_WIRE_LEN;

/// Largest request frame: a configure or a gain.
pub const CODEC_MAX_REQUEST: usize = HEADER_LEN + HwResource::WIRE_LEN + ARGUMENT_LEN;

/// Largest reply frame: a facts report.
pub const CODEC_MAX_REPLY: usize = REPLY_HEADER_LEN + FACTS_LEN;

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CodecOp {
    /// Report what the codec accepts.
    Describe = 1,
    /// Set the codec's interface up for a rate and a sample width, in the
    /// framing and clock sides the link states.
    Configure = 2,
    /// Set the codec's gain and mute.
    Gain = 3,
    /// Bring the codec's output up.
    Start = 4,
    /// Take the codec's output down.
    Stop = 5,
}

impl CodecOp {
    const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Describe,
            2 => Self::Configure,
            3 => Self::Gain,
            4 => Self::Start,
            5 => Self::Stop,
            _ => return None,
        })
    }

    const fn request_len(self) -> usize {
        match self {
            Self::Configure | Self::Gain => CODEC_MAX_REQUEST,
            Self::Describe | Self::Start | Self::Stop => HEADER_LEN + HwResource::WIRE_LEN,
        }
    }
}

/// The sample widths a codec takes in its slot.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SampleWidths(u8);

impl SampleWidths {
    /// Every width a slot carries: 16, 20, 24 and 32 bits.
    pub const ALL: [u8; 4] = [16, 20, 24, 32];

    /// The empty set.
    pub const EMPTY: Self = Self(0);

    const DEFINED: u8 = 0b1111;

    const fn bit(width: u8) -> Option<u8> {
        Some(match width {
            16 => 1,
            20 => 1 << 1,
            24 => 1 << 2,
            32 => 1 << 3,
            _ => return None,
        })
    }

    /// The set of `widths`; [`None`] when one is a width no slot carries.
    #[must_use]
    pub const fn of(widths: &[u8]) -> Option<Self> {
        let mut set = Self::EMPTY;
        let mut index = 0;
        while index < widths.len() {
            match set.with(widths[index]) {
                Some(next) => set = next,
                None => return None,
            }
            index += 1;
        }
        Some(set)
    }

    /// `self` with `width` added; [`None`] for a width no slot carries.
    #[must_use]
    pub const fn with(self, width: u8) -> Option<Self> {
        match Self::bit(width) {
            Some(bit) => Some(Self(self.0 | bit)),
            None => None,
        }
    }

    /// Whether `width` is in the set.
    #[must_use]
    pub const fn contains(self, width: u8) -> bool {
        match Self::bit(width) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

/// The framings a codec takes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DaiFormats(u8);

impl DaiFormats {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// One bit per framing, at its discriminant.
    const DEFINED: u8 = 0b11_1110;

    /// `self` with `format` added.
    #[must_use]
    pub const fn with(self, format: DaiFormat) -> Self {
        Self(self.0 | 1 << format as u8)
    }

    /// Whether `format` is in the set.
    #[must_use]
    pub const fn contains(self, format: DaiFormat) -> bool {
        self.0 & 1 << format as u8 != 0
    }
}

/// What a codec accepts, as [`CodecOp::Describe`] reports it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CodecFacts {
    /// The frame rates it converts.
    pub rates: RateSupport,
    /// The sample widths it takes in its slot.
    pub widths: SampleWidths,
    /// The framings it takes.
    pub formats: DaiFormats,
    /// It can drive the bit and frame clocks itself.
    pub drives_clocks: bool,
    /// Its gain control, where it has one.
    pub gain: Option<GainRange>,
}

/// [`CodecFacts`]'s one flag.
const DRIVES_CLOCKS: u8 = 1;

/// A request to a codec's endpoint, naming the caller's codec link, which
/// the codec believes only once the kernel confirms the caller holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CodecRequest {
    /// Report what the codec accepts.
    Describe(LinkRequest),
    /// Set the interface up for `rate` and `width`-bit samples, in the
    /// framing, clock sides and inversions the link's [`DaiLink`] states.
    Configure {
        /// The caller's codec link.
        link: LinkRequest,
        /// The frame rate.
        rate: Rate,
        /// The width of a sample and of its slot, in bits: a frame is two
        /// such slots.
        width: u8,
    },
    /// Set the gain, which the codec rounds to the step at or above
    /// `millibel`, and the mute.
    Gain {
        /// The caller's codec link.
        link: LinkRequest,
        /// The gain asked for, in hundredths of a decibel.
        millibel: i32,
        /// Silence the output.
        mute: bool,
    },
    /// Bring the output up.
    Start(LinkRequest),
    /// Take the output down.
    Stop(LinkRequest),
}

impl CodecRequest {
    /// The operation the request carries.
    #[must_use]
    pub const fn op(&self) -> CodecOp {
        match self {
            Self::Describe(_) => CodecOp::Describe,
            Self::Configure { .. } => CodecOp::Configure,
            Self::Gain { .. } => CodecOp::Gain,
            Self::Start(_) => CodecOp::Start,
            Self::Stop(_) => CodecOp::Stop,
        }
    }

    /// The codec link the request names.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        match self {
            Self::Describe(link)
            | Self::Start(link)
            | Self::Stop(link)
            | Self::Configure { link, .. }
            | Self::Gain { link, .. } => link,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let op = self.op();
        let len = op.request_len();
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, CODEC_REQUEST_MAGIC);
        put_u16(frame, 4, CODEC_VERSION_V1);
        frame[6] = op as u8;
        frame[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN]
            .copy_from_slice(&HwResource::request(self.link()).to_le_bytes());
        let argument = HEADER_LEN + HwResource::WIRE_LEN;
        match *self {
            Self::Configure { rate, width, .. } => {
                put_u32(frame, argument, rate.hz());
                frame[argument + 4] = width;
            }
            Self::Gain { millibel, mute, .. } => {
                frame[argument..argument + 4].copy_from_slice(&millibel.to_le_bytes());
                frame[argument + 4] = u8::from(mute);
            }
            Self::Describe(_) | Self::Start(_) | Self::Stop(_) => {}
        }
        Ok(len)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly its operation's length.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte, a quoted
    ///   record that is not a canonical codec link, or an argument outside
    ///   its field: a width no slot carries, a rate outside the vocabulary, a
    ///   mute byte that is neither.
    /// * [`Errno::AbiVersionUnsupported`] — not [`CODEC_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..HEADER_LEN).ok_or(Errno::LengthOutOfRange)?;
        if read_u32(header, 0) != CODEC_REQUEST_MAGIC || header[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(header, 4) != CODEC_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let op = CodecOp::from_u8(header[6]).ok_or(Errno::OutOfRange)?;
        if bytes.len() != op.request_len() {
            return Err(Errno::LengthOutOfRange);
        }
        let record = HwResource::from_bytes(&bytes[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN])
            .map_err(|_| Errno::BadMagic)?;
        let link = record.link_request().map_err(|_| Errno::BadMagic)?;
        if link.role() != LinkRole::Codec {
            return Err(Errno::BadMagic);
        }
        let argument = &bytes[HEADER_LEN + HwResource::WIRE_LEN..];
        Ok(match op {
            CodecOp::Describe => Self::Describe(link),
            CodecOp::Start => Self::Start(link),
            CodecOp::Stop => Self::Stop(link),
            CodecOp::Configure => {
                let width = argument[4];
                if argument[5..] != [0; 3] || SampleWidths::EMPTY.with(width).is_none() {
                    return Err(Errno::BadMagic);
                }
                let rate = Rate::new(read_u32(argument, 0)).map_err(|_| Errno::BadMagic)?;
                Self::Configure { link, rate, width }
            }
            CodecOp::Gain => {
                let mute = match argument[4] {
                    0 => false,
                    1 => true,
                    _ => return Err(Errno::BadMagic),
                };
                if argument[5..] != [0; 3] {
                    return Err(Errno::BadMagic);
                }
                let mut millibel = [0u8; 4];
                millibel.copy_from_slice(&argument[..4]);
                Self::Gain {
                    link,
                    millibel: i32::from_le_bytes(millibel),
                    mute,
                }
            }
        })
    }
}

/// The reason a codec driver's refusal travels as, keeping apart a framing
/// it does not take, a codec another driver holds and a gain it lacks.
#[must_use]
pub const fn refusal_reason(err: DriverError) -> Errno {
    match err {
        DriverError::Unsupported => Errno::NotSupported,
        DriverError::Busy => Errno::Busy,
        other => other.as_errno(),
    }
}

/// The refusal a reply's reason stands for, as [`refusal_reason`] sent it;
/// any reason it never sends is the codec failing.
#[must_use]
pub const fn refusal(reason: Errno) -> DriverError {
    match reason {
        Errno::NotSupported => DriverError::Unsupported,
        Errno::Busy => DriverError::Busy,
        Errno::NotImplemented => DriverError::NotImplemented,
        other => DriverError::from_errno(other),
    }
}

/// Frame a refusal of a request.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the status word.
pub fn encode_error_reply(out: &mut [u8], err: Errno) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..STATUS_REPLY_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame.copy_from_slice(&encode_status_reply(Err(err)));
    Ok(STATUS_REPLY_LEN)
}

/// A success frame with a `body_len`-byte body, zeroed, and its length.
fn success_frame(out: &mut [u8], body_len: usize) -> Result<(&mut [u8], usize), Errno> {
    let len = REPLY_HEADER_LEN + body_len;
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(Ok(())));
    Ok((&mut frame[REPLY_HEADER_LEN..], len))
}

/// The `body_len`-byte body of a success reply, or the refusal it carries.
fn success_body(bytes: &[u8], body_len: usize) -> Result<&[u8], Errno> {
    decode_status_reply(bytes)?;
    if bytes.len() != REPLY_HEADER_LEN + body_len || read_u32(bytes, STATUS_REPLY_LEN) != 0 {
        return Err(Errno::BadMagic);
    }
    Ok(&bytes[REPLY_HEADER_LEN..])
}

/// Frame the reply to [`CodecOp::Describe`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_describe_reply(out: &mut [u8], facts: &CodecFacts) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, FACTS_LEN)?;
    body[..RATE_SUPPORT_WIRE_LEN].copy_from_slice(&facts.rates.to_wire());
    let flags = RATE_SUPPORT_WIRE_LEN;
    body[flags] = facts.widths.0;
    body[flags + 1] = facts.formats.0;
    body[flags + 2] = if facts.drives_clocks {
        DRIVES_CLOCKS
    } else {
        0
    };
    body[flags + 4..].copy_from_slice(&GainRange::to_wire(facts.gain));
    Ok(len)
}

/// Decode the reply to [`CodecOp::Describe`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length, a set bit no field defines, an empty width or framing set,
/// or a rate or gain range the vocabulary refuses.
pub fn decode_describe_reply(bytes: &[u8]) -> Result<CodecFacts, Errno> {
    let body = success_body(bytes, FACTS_LEN)?;
    let rates =
        RateSupport::from_wire(&body[..RATE_SUPPORT_WIRE_LEN]).map_err(|_| Errno::BadMagic)?;
    let flags = RATE_SUPPORT_WIRE_LEN;
    let (widths, formats, drives) = (body[flags], body[flags + 1], body[flags + 2]);
    if widths & !SampleWidths::DEFINED != 0
        || widths == 0
        || formats & !DaiFormats::DEFINED != 0
        || formats == 0
        || drives & !DRIVES_CLOCKS != 0
        || body[flags + 3] != 0
    {
        return Err(Errno::BadMagic);
    }
    Ok(CodecFacts {
        rates,
        widths: SampleWidths(widths),
        formats: DaiFormats(formats),
        drives_clocks: drives != 0,
        gain: GainRange::from_wire(&body[flags + 4..]).map_err(|_| Errno::BadMagic)?,
    })
}

/// Frame the bare acknowledgement of a configure, a start or a stop.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_done_reply(out: &mut [u8]) -> Result<usize, Errno> {
    success_frame(out, 0).map(|(_, len)| len)
}

/// Decode the bare acknowledgement of a configure, a start or a stop.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_done_reply(bytes: &[u8]) -> Result<(), Errno> {
    success_body(bytes, 0).map(|_| ())
}

/// Frame the reply to [`CodecOp::Gain`]: the gain the codec applied.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_gain_reply(out: &mut [u8], millibel: i32) -> Result<usize, Errno> {
    let (body, len) = success_frame(out, ARGUMENT_LEN)?;
    body[..4].copy_from_slice(&millibel.to_le_bytes());
    Ok(len)
}

/// Decode the reply to [`CodecOp::Gain`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length or a dirty reserved word.
pub fn decode_gain_reply(bytes: &[u8]) -> Result<i32, Errno> {
    let body = success_body(bytes, ARGUMENT_LEN)?;
    if read_u32(body, 4) != 0 {
        return Err(Errno::BadMagic);
    }
    let mut millibel = [0u8; 4];
    millibel.copy_from_slice(&body[..4]);
    Ok(i32::from_le_bytes(millibel))
}

/// An audio codec: what its driver implements and the codec server serves.
pub trait Codec {
    /// What the codec accepts.
    fn facts(&self) -> CodecFacts;

    /// Set the interface up for `rate` and `width`-bit samples, each in a
    /// slot as wide and two slots a frame, in the framing, clock sides and
    /// inversions `link` states.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] for a framing, a clock side or a width
    ///   the codec does not take, or a rate it does not convert.
    /// * [`DriverError::DeviceFault`] if the part refused its programming.
    fn configure(&mut self, link: &DaiLink, rate: Rate, width: u8) -> Result<(), DriverError>;

    /// Set the gain at or above `millibel`, within the codec's range, and the
    /// mute, answering the gain set.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotImplemented`] for a codec with no gain, which is
    ///   how its caller learns to apply the gain itself.
    /// * [`DriverError::DeviceFault`] if the part refused.
    fn set_gain(&mut self, millibel: i32, mute: bool) -> Result<i32, DriverError>;

    /// Bring the output up.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if the part refused.
    fn start(&mut self) -> Result<(), DriverError>;

    /// Take the output down.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if the part refused.
    fn stop(&mut self) -> Result<(), DriverError>;
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;

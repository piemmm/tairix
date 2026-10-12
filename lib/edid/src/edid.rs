//! An EDID: its base block, the extensions it is read with, and what the
//! whole states about the sink.

use tairix_inline::{ArrayString, ArrayVec};

use crate::audio::Audio;
use crate::tables::{DMT, ESTABLISHED, ESTABLISHED_III};
use crate::timing::{detailed, Aspect, Size, DETAILED_LEN};
use crate::{cta, displayid};
use tairix_abi::driver::display::Timing;

/// Bytes of an EDID block.
pub const BLOCK_LEN: usize = 128;

/// The most blocks a description is read from: the base block and fifteen
/// extensions, a defence against a sink declaring 255.
pub const MAX_BLOCKS: usize = 16;

/// The most distinct timings a description is read with.
pub const MAX_MODES: usize = 128;

/// The most characters a sink's name descriptor holds.
pub const NAME_LEN: usize = 13;

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Where the four 18-byte descriptors of the base block start.
const DESCRIPTORS: usize = 54;

/// A display descriptor's tags, in its fourth byte.
const TAG_NAME: u8 = 0xFC;
const TAG_RANGE_LIMITS: u8 = 0xFD;
const TAG_STANDARD_TIMINGS: u8 = 0xFA;
const TAG_ESTABLISHED_III: u8 = 0xF7;

/// Why a description was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EdidError {
    /// Not a whole number of blocks, or none.
    Length,
    /// No EDID 1 header.
    NotEdid,
    /// The base block's checksum is wrong.
    Checksum,
}

/// Who made a sink, and when.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Product {
    /// The PNP manufacturer id, three capital letters.
    pub manufacturer: [u8; 3],
    /// The manufacturer's product code.
    pub code: u16,
    /// The serial number; zero when unstated.
    pub serial: u32,
    /// The week of manufacture, 1 to 54; 0 when unstated, `0xFF` when
    /// `year` is the model year.
    pub week: u8,
    /// The year of manufacture.
    pub year: u16,
}

/// The code a source names a timing by in its `InfoFrames`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VideoCode {
    /// A CTA-861 video identification code, and the aspect ratio it carries.
    Cta {
        /// The code.
        vic: u8,
        /// The picture aspect ratio.
        aspect: Aspect,
    },
    /// An HDMI 1.4 video code.
    Hdmi(u8),
}

/// A timing a sink takes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Mode {
    /// The timing.
    pub timing: Timing,
    /// The code the sink listed it by, when it listed it by one.
    pub code: Option<VideoCode>,
    /// Whether the sink prefers it.
    pub preferred: bool,
}

/// What an HDMI Forum data block adds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HdmiForum {
    /// The fastest TMDS character rate the sink takes, in kHz, when above
    /// 340 MHz.
    pub max_tmds_khz: Option<u32>,
    /// Whether it has the status and control data channel.
    pub scdc: bool,
    /// Whether it requests reads over that channel.
    pub read_request: bool,
    /// Whether it descrambles at TMDS rates of 340 MHz and below too.
    pub scrambles_below_340: bool,
}

/// What a sink's HDMI vendor-specific data blocks state: it is an HDMI sink,
/// not a DVI one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hdmi {
    /// Its CEC physical address, `a.b.c.d` a nibble each.
    pub physical_address: u16,
    /// The fastest TMDS clock it takes, in kHz, when it says.
    pub max_tmds_khz: Option<u32>,
    /// The HDMI Forum's additions, when it has them.
    pub forum: Option<HdmiForum>,
}

/// What a sink's description states.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edid {
    /// Who made it.
    pub product: Product,
    /// The EDID 1 revision.
    pub revision: u8,
    /// Whether its input is digital.
    pub digital: bool,
    /// The picture's physical size, when stated.
    pub size: Option<Size>,
    /// Its name, when its name descriptor holds one.
    pub name: Option<ArrayString<NAME_LEN>>,
    /// The fastest pixel clock it takes, in kHz, when its range limits say.
    pub max_clock_khz: Option<u32>,
    /// The distinct timings it takes, as it listed them.
    pub modes: ArrayVec<Mode, MAX_MODES>,
    /// The audio it plays.
    pub audio: Audio,
    /// Its HDMI capabilities: `None` for a DVI sink.
    pub hdmi: Option<Hdmi>,
    /// Whether a source may state an RGB quantization range in its AVI
    /// `InfoFrames`.
    pub rgb_quantization_selectable: bool,
}

/// Whether `block`'s bytes sum to zero, its checksum good.
pub(crate) fn sums_to_zero(block: &[u8; BLOCK_LEN]) -> bool {
    block.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

/// The base block of `blocks`, checked.
fn base(blocks: &[[u8; BLOCK_LEN]]) -> Result<&[u8; BLOCK_LEN], EdidError> {
    let base = blocks.first().ok_or(EdidError::Length)?;
    if base[..HEADER.len()] != HEADER || base[18] != 1 {
        return Err(EdidError::NotEdid);
    }
    if !sums_to_zero(base) {
        return Err(EdidError::Checksum);
    }
    Ok(base)
}

/// How many blocks a sink's description holds, as its first blocks `read`
/// state, at most [`MAX_BLOCKS`]: the base block's extension count, unless an
/// HDMI Forum override in the first extension states another.
///
/// # Errors
///
/// As [`Edid::parse`] refuses the base block.
pub fn block_count(read: &[u8]) -> Result<usize, EdidError> {
    let (blocks, _) = read.as_chunks::<BLOCK_LEN>();
    Ok(count(base(blocks)?, blocks))
}

/// The blocks `base`, the checked base block of `blocks`, says there are.
fn count(base: &[u8; BLOCK_LEN], blocks: &[[u8; BLOCK_LEN]]) -> usize {
    let declared = usize::from(base[126]);
    let overridden = (declared > 0)
        .then(|| blocks.get(1))
        .flatten()
        .filter(|first| sums_to_zero(first))
        .and_then(cta::override_count);
    (1 + overridden.unwrap_or(declared)).min(MAX_BLOCKS)
}

impl Edid {
    /// Read the description `bytes` hold: the base block and as many
    /// extensions as it declares and `bytes` carries, at most
    /// [`MAX_BLOCKS`] in all. An extension whose checksum is wrong, or of a
    /// kind not read, is skipped.
    ///
    /// # Errors
    ///
    /// * [`EdidError::Length`] — not a whole number of blocks, or none.
    /// * [`EdidError::NotEdid`] — the base block is no EDID 1 block.
    /// * [`EdidError::Checksum`] — its checksum is wrong.
    pub fn parse(bytes: &[u8]) -> Result<Self, EdidError> {
        let (blocks, rest) = bytes.as_chunks::<BLOCK_LEN>();
        if !rest.is_empty() {
            return Err(EdidError::Length);
        }
        let base = base(blocks)?;
        let mut edid = Self::from_base(base);
        for block in blocks.iter().take(count(base, blocks)).skip(1) {
            if !sums_to_zero(block) {
                continue;
            }
            match block[0] {
                cta::TAG => cta::read(block, &mut edid),
                displayid::TAG => displayid::read(block, &mut edid),
                _ => {}
            }
        }
        Ok(edid)
    }

    fn from_base(base: &[u8; BLOCK_LEN]) -> Self {
        let id = u16::from_be_bytes([base[8], base[9]]);
        let letter = |shift: u16| b'@' + u8::try_from((id >> shift) & 0x1F).unwrap_or(0);
        let revision = base[19];
        let mut edid = Self {
            product: Product {
                manufacturer: [letter(10), letter(5), letter(0)],
                code: u16::from_le_bytes([base[10], base[11]]),
                serial: u32::from_le_bytes([base[12], base[13], base[14], base[15]]),
                week: base[16],
                year: 1990 + u16::from(base[17]),
            },
            revision,
            digital: base[20] & 0x80 != 0,
            size: (base[21] != 0 && base[22] != 0).then(|| Size {
                width_mm: u16::from(base[21]) * 10,
                height_mm: u16::from(base[22]) * 10,
            }),
            name: None,
            max_clock_khz: None,
            modes: ArrayVec::new(),
            audio: Audio::default(),
            hdmi: None,
            rgb_quantization_selectable: false,
        };
        // From revision 4 the first detailed timing is always the preferred
        // one; before, a feature bit says whether it is.
        let first_preferred = revision >= 4 || base[24] & 0x02 != 0;
        let (descriptors, _) =
            base[DESCRIPTORS..DESCRIPTORS + 4 * DETAILED_LEN].as_chunks::<DETAILED_LEN>();
        for (index, descriptor) in descriptors.iter().enumerate() {
            match detailed(descriptor) {
                Some((timing, size)) => {
                    let preferred = index == 0 && first_preferred;
                    if preferred && size.width_mm != 0 && size.height_mm != 0 {
                        edid.size = Some(size);
                    }
                    edid.add(Mode {
                        timing,
                        code: None,
                        preferred,
                    });
                }
                None => edid.read_display_descriptor(descriptor),
            }
        }
        let established =
            u32::from(base[0x23]) | u32::from(base[0x24]) << 8 | u32::from(base[0x25] & 0x80) << 9;
        for (bit, timing) in ESTABLISHED.iter().enumerate() {
            if established & (1 << bit) != 0 {
                edid.add(Mode {
                    timing: *timing,
                    code: None,
                    preferred: false,
                });
            }
        }
        let (standard, _) = base[0x26..0x36].as_chunks::<2>();
        edid.add_standard(standard);
        edid
    }

    fn read_display_descriptor(&mut self, descriptor: &[u8; DETAILED_LEN]) {
        if descriptor[0..3] != [0, 0, 0] {
            return;
        }
        match descriptor[3] {
            TAG_NAME => self.name = name(&descriptor[5..]),
            TAG_RANGE_LIMITS => {
                self.max_clock_khz =
                    (descriptor[9] != 0).then(|| u32::from(descriptor[9]) * 10_000);
            }
            TAG_STANDARD_TIMINGS => {
                let (standard, _) = descriptor[5..17].as_chunks::<2>();
                self.add_standard(standard);
            }
            TAG_ESTABLISHED_III => {
                for (bit, &(width, height, refresh, reduced)) in ESTABLISHED_III.iter().enumerate()
                {
                    if descriptor[6 + bit / 8] & (0x80 >> (bit % 8)) != 0 {
                        self.add_dmt(width, height, refresh, reduced);
                    }
                }
            }
            _ => {}
        }
    }

    /// Add each standard timing a DMT timing states; a code naming none
    /// would be synthesised by a formula the sink's own descriptors make
    /// unnecessary.
    fn add_standard(&mut self, codes: &[[u8; 2]]) {
        for &[size, shape] in codes {
            if size == 0 || (size, shape) == (1, 1) {
                continue;
            }
            let width = (u16::from(size) + 31) * 8;
            let height = match shape >> 6 {
                0 if self.revision < 3 => width,
                0 => width * 10 / 16,
                1 => width * 3 / 4,
                2 => width * 4 / 5,
                _ => width * 9 / 16,
            };
            let refresh = (shape & 0x3F) + 60;
            // A width of 1366 cannot be spelled in eighths: such a panel names
            // the nearest the code can.
            let (width, height) = match (width, height, refresh) {
                (1360, 765, 60) | (1368, 769, 60) => (1366, 768),
                other => (other.0, other.1),
            };
            self.add_dmt(width, height, refresh, false);
        }
    }

    fn add_dmt(&mut self, width: u16, height: u16, refresh: u8, reduced: bool) {
        let found = DMT.iter().find(|dmt| {
            dmt.timing.horizontal.active == width
                && dmt.timing.vertical.active == height
                && dmt.refresh == refresh
                && dmt.reduced == reduced
        });
        if let Some(dmt) = found {
            self.add(Mode {
                timing: dmt.timing,
                code: None,
                preferred: false,
            });
        }
    }

    /// Add `mode`, merged into an earlier listing of its timing: the code
    /// one listing names and either's preference kept. A timing past
    /// [`MAX_MODES`] distinct ones is dropped.
    pub(crate) fn add(&mut self, mode: Mode) {
        if let Some(listed) = self
            .modes
            .iter_mut()
            .find(|listed| listed.timing == mode.timing)
        {
            listed.preferred |= mode.preferred;
            if listed.code.is_none() {
                listed.code = mode.code;
            }
            return;
        }
        // Past the bound a timing is dropped, never an earlier one.
        let _ = self.modes.try_push(mode);
    }
}

/// The name a name descriptor's thirteen bytes hold: printable ASCII ended
/// by a line feed or the field's end, trailing spaces trimmed. Anything else
/// is no name.
fn name(field: &[u8]) -> Option<ArrayString<NAME_LEN>> {
    let field = field.get(..NAME_LEN)?;
    let text = field
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(field, |end| &field[..end]);
    if !text.iter().all(|byte| (0x20..0x7F).contains(byte)) {
        return None;
    }
    let text = core::str::from_utf8(text).ok()?.trim_end();
    (!text.is_empty()).then(|| ArrayString::from_str_truncating(text))
}

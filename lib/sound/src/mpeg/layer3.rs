//! Layer III (ISO/IEC 11172-3 §2.4.1.7, §2.4.2.7 and §2.4.3.4; ISO/IEC
//! 13818-3 for the lower rates): side information, the bit reservoir,
//! scalefactors, the Huffman-coded spectrum, requantisation, stereo
//! processing, and the hybrid filterbank that turns each granule into
//! subband samples.

use alloc::vec::Vec;

use tairix_util::fallible::{filled, reserve};
use tairix_util::mathf::{exp, ln, sin, tan};

use super::frame::{Header, Mode, CRC_LEN, HEADER_LEN};
use super::huffman::{count1_signs, Lookups};
use super::layer12::Slots;
use super::synth::SUBBANDS;
use super::tables::{Bands, BANDS, LSF_PARTITIONS, MIXED_SHORT_START, PRETAB, SLEN};
use crate::bits::{BitReader, Exhausted};
use crate::crc::crc16;
use crate::single;
use crate::transform::Mdct;
use crate::DecodeError;

/// Spectral lines a granule holds per channel.
const LINES: usize = 576;

/// Time slots a granule makes per subband.
const SLOTS: usize = 18;

/// The largest value a big-values escape reaches: 15 and 13 linbits.
const MAX_MAGNITUDE: usize = 15 + (1 << 13) - 1;

/// Bytes of past main data a frame may reach back into: MPEG-1's nine-bit
/// `main_data_begin`.
pub(super) const RESERVOIR: usize = 511;

/// How a granule's samples are windowed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Block {
    /// One long block: normal (0), start (1) or stop (3), a mixed block's
    /// long part under the normal window.
    Long { kind: u8, mixed: bool },
    /// Three short blocks, the lowest subbands long where mixed.
    Short { mixed: bool },
}

/// One channel's side information for one granule.
#[derive(Copy, Clone, Debug)]
struct Granule {
    part2_3_length: u32,
    big_values: u32,
    global_gain: i32,
    scalefac_compress: u32,
    block: Block,
    table_select: [u8; 3],
    subblock_gain: [i32; 3],
    /// Lines where regions 1 and 2 of the big values start.
    regions: [usize; 2],
    preflag: bool,
    scalefac_scale: bool,
    count1_table: bool,
}

const EMPTY: Granule = Granule {
    part2_3_length: 0,
    big_values: 0,
    global_gain: 0,
    scalefac_compress: 0,
    block: Block::Long {
        kind: 0,
        mixed: false,
    },
    table_select: [0; 3],
    subblock_gain: [0; 3],
    regions: [0; 2],
    preflag: false,
    scalefac_scale: false,
    count1_table: false,
};

/// A frame's side information.
struct SideInfo {
    main_data_begin: usize,
    /// MPEG-1's scalefactor reuse, by band group, per channel.
    scfsi: [[bool; 4]; 2],
    /// By granule, then channel.
    granules: [[Granule; 2]; 2],
}

/// The scalefactors of one channel's granule.
#[derive(Copy, Clone)]
struct Scalefactors {
    long: [u8; 22],
    short: [[u8; 3]; 13],
    /// Each long and short value's largest code, where it is an intensity
    /// position the lower rates read: the code that position cannot take.
    long_limit: [u8; 22],
    short_limit: [[u8; 3]; 13],
}

/// The most bands a granule codes: 22 long, or 13 short in each window.
const MOST_BANDS: usize = 3 * 13;

/// One band in coded order: where its lines lie, the right channel's
/// intensity position for it and the position it cannot take, and the
/// scales it is given where it is intensity-coded.
#[derive(Copy, Clone, Default)]
struct CodedBand {
    start: usize,
    width: usize,
    position: u8,
    limit: u8,
    scale: Option<[f32; 2]>,
}

const NO_SCALEFACTORS: Scalefactors = Scalefactors {
    long: [0; 22],
    short: [[0; 3]; 13],
    long_limit: [u8::MAX; 22],
    short_limit: [[u8::MAX; 3]; 13],
};

/// Where a granule's band layout starts and ends.
struct Layout<'b> {
    bands: &'b Bands,
    /// Lines below which a mixed block is long.
    long_lines: usize,
}

impl Layout<'_> {
    /// Short band `band`'s first line within one window.
    fn short_start(&self, band: usize) -> usize {
        self.bands.short[..band]
            .iter()
            .map(|&w| usize::from(w))
            .sum()
    }

    /// Long band `band`'s first line.
    fn long_start(&self, band: usize) -> usize {
        self.bands.long[..band]
            .iter()
            .map(|&w| usize::from(w))
            .sum()
    }
}

/// The constants every granule's decode reads.
struct Tables {
    /// `|x|^(4/3)` for every magnitude a code reaches.
    pow43: Vec<f32>,
    /// Long windows by block type; type 2's slot is unused.
    windows: [[f32; 36]; 4],
    short_window: [f32; 12],
    /// The aliasing butterflies' coefficients.
    cs: [f32; 8],
    ca: [f32; 8],
    /// MPEG-1's intensity scales, left and right, by position.
    ratios: [[f32; 2]; 7],
}

/// `f` at `0, 1, …` into `out`.
fn tabulate(out: &mut [f32], f: impl Fn(u32) -> f32) {
    for (slot, at) in out.iter_mut().zip(0u32..) {
        *slot = f(at);
    }
}

impl Tables {
    fn build() -> Result<Self, DecodeError> {
        use core::f64::consts::PI;
        const CI: [f64; 8] = [
            -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
        ];
        let mut pow43 = filled(MAX_MAGNITUDE + 1, 0f32).ok_or(DecodeError::OutOfMemory)?;
        tabulate(&mut pow43, |magnitude| match magnitude {
            0 => 0.0,
            _ => single(exp(ln(f64::from(magnitude)) * 4.0 / 3.0)),
        });
        let normal = |i: u32| single(sin(PI / 36.0 * (f64::from(i) + 0.5)));
        let narrow = |i: u32| single(sin(PI / 12.0 * (f64::from(i) + 0.5)));
        let mut windows = [[0f32; 36]; 4];
        tabulate(&mut windows[0], normal);
        tabulate(&mut windows[1], |i| match i {
            0..18 => normal(i),
            18..24 => 1.0,
            24..30 => narrow(i - 18),
            _ => 0.0,
        });
        tabulate(&mut windows[3], |i| match i {
            0..6 => 0.0,
            6..12 => narrow(i - 6),
            12..18 => 1.0,
            _ => normal(i),
        });
        let mut short_window = [0f32; 12];
        tabulate(&mut short_window, narrow);
        let root = |c: f64| tairix_util::mathf::sqrt(1.0 + c * c);
        let mut ratios = [[1.0, 0.0]; 7];
        for (scale, position) in ratios.iter_mut().zip(0u32..6) {
            let ratio = tan(f64::from(position) * PI / 12.0);
            *scale = [single(ratio / (1.0 + ratio)), single(1.0 / (1.0 + ratio))];
        }
        Ok(Self {
            pow43,
            windows,
            short_window,
            cs: CI.map(|c| single(1.0 / root(c))),
            ca: CI.map(|c| single(c / root(c))),
            ratios,
        })
    }
}

/// `2^(quarters / 4)`, zero where it falls below a normal `f32`.
fn quarter_power(quarters: i32) -> f32 {
    const QUARTERS: [f32; 4] = [1.0, 1.189_207_1, core::f32::consts::SQRT_2, 1.681_792_8];
    let whole = quarters.div_euclid(4);
    let fraction = QUARTERS[quarters.rem_euclid(4) as usize];
    match whole {
        -126..=127 => f32::from_bits((whole + 127).cast_unsigned() << 23) * fraction,
        128.. => f32::MAX,
        _ => 0.0,
    }
}

/// The hybrid filterbank's IMDCTs: 18 lines into a long block's 36 samples,
/// 6 into each short block's 12.
struct Imdcts {
    long: Mdct,
    short: Mdct,
}

impl Imdcts {
    fn new() -> Result<Self, DecodeError> {
        Ok(Self {
            long: Mdct::new(2 * SLOTS).ok_or(DecodeError::OutOfMemory)?,
            short: Mdct::new(12).ok_or(DecodeError::OutOfMemory)?,
        })
    }
}

/// Layer III's decoding state: the reservoir, each channel's overlap, and
/// the constants every granule uses.
pub(super) struct Layer3 {
    reservoir: Vec<u8>,
    /// The second half of each subband's last IMDCT, per channel.
    overlap: [[[f32; SLOTS]; SUBBANDS]; 2],
    lookups: Lookups,
    tables: Tables,
    imdcts: Imdcts,
}

impl Layer3 {
    /// A decoder that has heard nothing.
    ///
    /// # Errors
    ///
    /// [`DecodeError::OutOfMemory`] when its constants cannot be allocated.
    pub(super) fn new() -> Result<Self, DecodeError> {
        let mut reservoir = Vec::new();
        if !reserve(&mut reservoir, RESERVOIR + super::frame::MAX_FRAME_LEN) {
            return Err(DecodeError::OutOfMemory);
        }
        Ok(Self {
            reservoir,
            overlap: [[[0.0; SLOTS]; SUBBANDS]; 2],
            lookups: Lookups::build()?,
            tables: Tables::build()?,
            imdcts: Imdcts::new()?,
        })
    }

    /// Forget the reservoir and the overlap, as at a stream's start.
    pub(super) fn reset(&mut self) {
        self.reservoir.clear();
        self.overlap = [[[0.0; SLOTS]; SUBBANDS]; 2];
    }

    /// Decode `frame` into `out` for a stream of `outputs` channels,
    /// answering the slots it filled: none for a frame whose main data
    /// reaches back past what the reservoir holds, which only a stream
    /// entered part-way through has. A mono frame in a stereo stream drives
    /// both channels' filterbanks with its one channel.
    ///
    /// # Errors
    ///
    /// [`DecodeError::MpegFrameCrc`] or [`DecodeError::MpegFrameInvalid`].
    pub(super) fn frame(
        &mut self,
        header: Header,
        frame: &[u8],
        outputs: usize,
        out: &mut Slots,
    ) -> Result<usize, DecodeError> {
        let side_start = HEADER_LEN + if header.protected { CRC_LEN } else { 0 };
        let data_start = header.data_start();
        let side_bytes = frame
            .get(side_start..data_start)
            .ok_or(DecodeError::MpegFrameInvalid)?;
        if header.protected {
            let stated = u16::from_be_bytes([frame[HEADER_LEN], frame[HEADER_LEN + 1]]);
            if crc16(crc16(0xFFFF, &frame[2..HEADER_LEN]), side_bytes) != stated {
                return Err(DecodeError::MpegFrameCrc);
            }
        }
        let layout = layout(header);
        let side = side_info(header, side_bytes, &layout)?;
        let held = self.reservoir.len();
        let main = &frame[data_start..];
        if !reserve(&mut self.reservoir, main.len()) {
            return Err(DecodeError::OutOfMemory);
        }
        self.reservoir.extend_from_slice(main);
        let filled_slots = if side.main_data_begin > held {
            0
        } else {
            let start = held - side.main_data_begin;
            let Self {
                reservoir,
                overlap,
                lookups,
                tables,
                imdcts,
            } = self;
            let decoder = Granules {
                header,
                layout: &layout,
                side: &side,
                lookups,
                tables,
            };
            decoder.decode(&reservoir[start..], overlap, imdcts, outputs, out)?
        };
        let excess = self.reservoir.len().saturating_sub(RESERVOIR);
        self.reservoir.drain(..excess);
        Ok(filled_slots)
    }
}

/// The band layout `header`'s rate uses.
fn layout(header: Header) -> Layout<'static> {
    let family = match header.version {
        super::frame::Version::Mpeg1 => 0,
        super::frame::Version::Mpeg2 => 1,
        super::frame::Version::Mpeg25 => 2,
    };
    let bands = &BANDS[family][usize::from(header.rate_index)];
    let long_lines = bands.long[..usize::from(bands.mixed_long)]
        .iter()
        .map(|&w| usize::from(w))
        .sum();
    Layout { bands, long_lines }
}

fn short(_: Exhausted) -> DecodeError {
    DecodeError::MpegFrameInvalid
}

/// A field of at most eight bits.
fn field(bits: &mut BitReader<'_>, width: u32) -> Result<u8, DecodeError> {
    let value = bits.read(width).map_err(short)?;
    u8::try_from(value).map_err(|_| DecodeError::MpegFrameInvalid)
}

/// Read a frame's side information.
fn side_info(header: Header, bytes: &[u8], layout: &Layout<'_>) -> Result<SideInfo, DecodeError> {
    let mut bits = BitReader::new(bytes);
    let channels = usize::from(header.channels());
    let lsf = header.lsf();
    let mut side = SideInfo {
        main_data_begin: 0,
        scfsi: [[false; 4]; 2],
        granules: [[EMPTY; 2]; 2],
    };
    side.main_data_begin = bits.read(if lsf { 8 } else { 9 }).map_err(short)? as usize;
    let private = match (lsf, channels) {
        (false, 1) => 5,
        (false, _) => 3,
        (true, 1) => 1,
        (true, _) => 2,
    };
    bits.read(private).map_err(short)?;
    if !lsf {
        for scfsi in side.scfsi.iter_mut().take(channels) {
            for group in scfsi.iter_mut() {
                *group = bits.read(1).map_err(short)? == 1;
            }
        }
    }
    let granules = if lsf { 1 } else { 2 };
    for granule in side.granules.iter_mut().take(granules) {
        for channel in granule.iter_mut().take(channels) {
            *channel = granule_info(&mut bits, lsf, layout)?;
        }
    }
    Ok(side)
}

fn granule_info(
    bits: &mut BitReader<'_>,
    lsf: bool,
    layout: &Layout<'_>,
) -> Result<Granule, DecodeError> {
    let part2_3_length = bits.read(12).map_err(short)?;
    let big_values = bits.read(9).map_err(short)?;
    if big_values as usize > LINES / 2 {
        return Err(DecodeError::MpegFrameInvalid);
    }
    let global_gain = i32::from(field(bits, 8)?);
    let scalefac_compress = bits.read(if lsf { 9 } else { 4 }).map_err(short)?;
    let mut table_select = [0u8; 3];
    let mut subblock_gain = [0i32; 3];
    let (block, regions) = if field(bits, 1)? == 1 {
        let block = match (field(bits, 2)?, field(bits, 1)? == 1) {
            (1, mixed) => Block::Long { kind: 1, mixed },
            (2, mixed) => Block::Short { mixed },
            (3, mixed) => Block::Long { kind: 3, mixed },
            _ => return Err(DecodeError::MpegFrameInvalid),
        };
        for select in table_select.iter_mut().take(2) {
            *select = field(bits, 5)?;
        }
        for gain in &mut subblock_gain {
            *gain = i32::from(field(bits, 3)?);
        }
        let first = if matches!(block, Block::Short { .. }) {
            3 * layout.short_start(MIXED_SHORT_START)
        } else {
            layout.long_start(8)
        };
        (block, [first, LINES])
    } else {
        for select in &mut table_select {
            *select = field(bits, 5)?;
        }
        let first = usize::from(field(bits, 4)?) + 1;
        let second = first + usize::from(field(bits, 3)?) + 1;
        let block = Block::Long {
            kind: 0,
            mixed: false,
        };
        (
            block,
            [
                layout.long_start(first.min(22)),
                layout.long_start(second.min(22)),
            ],
        )
    };
    if table_select
        .iter()
        .any(|&select| select == 4 || select == 14)
    {
        return Err(DecodeError::MpegFrameInvalid);
    }
    let preflag = if lsf {
        scalefac_compress >= 500
    } else {
        field(bits, 1)? == 1
    };
    Ok(Granule {
        part2_3_length,
        big_values,
        global_gain,
        scalefac_compress,
        block,
        table_select,
        subblock_gain,
        regions,
        preflag,
        scalefac_scale: field(bits, 1)? == 1,
        count1_table: field(bits, 1)? == 1,
    })
}

/// One frame's granules being decoded.
struct Granules<'d> {
    header: Header,
    layout: &'d Layout<'static>,
    side: &'d SideInfo,
    lookups: &'d Lookups,
    tables: &'d Tables,
}

impl Granules<'_> {
    /// Decode every granule from `main`, the frame's main data and what
    /// follows, for `outputs` channels, answering the slots filled.
    fn decode(
        &self,
        main: &[u8],
        overlap: &mut [[[f32; SLOTS]; SUBBANDS]; 2],
        imdcts: &mut Imdcts,
        outputs: usize,
        out: &mut Slots,
    ) -> Result<usize, DecodeError> {
        let channels = usize::from(self.header.channels());
        let granules = if self.header.lsf() { 1 } else { 2 };
        let mut offset = 0usize;
        let mut scalefactors = [NO_SCALEFACTORS; 2];
        let mut spectra = [[0f32; LINES]; 2];
        for granule in 0..granules {
            let mut nonzero = [0usize; 2];
            for channel in 0..channels {
                let info = &self.side.granules[granule][channel];
                let length = info.part2_3_length as usize;
                if offset + length > main.len() * 8 {
                    return Err(DecodeError::MpegFrameInvalid);
                }
                // A reader of its own, so an overrun in one granule's bits
                // cannot move where the next one starts.
                let mut bits = BitReader::new(&main[offset / 8..]);
                let lead = u32::try_from(offset % 8).map_err(|_| DecodeError::MpegFrameInvalid)?;
                bits.skip(lead).map_err(short)?;
                let end = offset % 8 + length;
                offset += length;
                let previous = scalefactors[channel];
                scalefactors[channel] = if self.header.lsf() {
                    self.lsf_scalefactors(&mut bits, info, channel)?
                } else {
                    self.scalefactors(&mut bits, info, channel, granule, &previous)?
                };
                if bits.bits_consumed() > end {
                    return Err(DecodeError::MpegFrameInvalid);
                }
                let mut quantised = [0i32; LINES];
                nonzero[channel] = self.spectrum(&mut bits, info, end, &mut quantised)?;
                self.requantise(
                    info,
                    &scalefactors[channel],
                    &quantised,
                    &mut spectra[channel],
                );
            }
            if self.header.mode == Mode::JointStereo && channels == 2 {
                self.stereo(granule, &scalefactors[1], &mut spectra);
            }
            // The second output's IMDCT then follows the one channel it carries.
            if outputs > channels {
                spectra[1] = spectra[0];
                nonzero[1] = nonzero[0];
            }
            for channel in 0..outputs {
                let info = &self.side.granules[granule][channel.min(channels - 1)];
                let lines = if self.header.mode == Mode::JointStereo {
                    nonzero[0].max(nonzero[1])
                } else {
                    nonzero[channel]
                };
                self.hybrid(
                    info,
                    &mut spectra[channel],
                    lines,
                    &mut overlap[channel],
                    imdcts,
                    &mut out[channel][granule * SLOTS..(granule + 1) * SLOTS],
                );
            }
        }
        Ok(granules * SLOTS)
    }

    /// MPEG-1's scalefactors (§2.4.2.7), reusing granule 0's where `scfsi`
    /// says so.
    fn scalefactors(
        &self,
        bits: &mut BitReader<'_>,
        info: &Granule,
        channel: usize,
        granule: usize,
        previous: &Scalefactors,
    ) -> Result<Scalefactors, DecodeError> {
        let compress = info.scalefac_compress as usize;
        let (slen1, slen2) = (u32::from(SLEN[0][compress]), u32::from(SLEN[1][compress]));
        let mut out = NO_SCALEFACTORS;
        let mut read = |width: u32| field(bits, width);
        match info.block {
            Block::Short { mixed } => {
                let first_short = if mixed {
                    for band in 0..8 {
                        out.long[band] = read(slen1)?;
                    }
                    MIXED_SHORT_START
                } else {
                    0
                };
                for band in first_short..12 {
                    let width = if band < 6 { slen1 } else { slen2 };
                    for window in 0..3 {
                        out.short[band][window] = read(width)?;
                    }
                }
            }
            Block::Long { .. } => {
                const GROUPS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];
                for (group, &(from, to)) in GROUPS.iter().enumerate() {
                    let width = if group < 2 { slen1 } else { slen2 };
                    if granule == 1 && self.side.scfsi[channel][group] {
                        out.long[from..to].copy_from_slice(&previous.long[from..to]);
                    } else {
                        for band in from..to {
                            out.long[band] = read(width)?;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The lower rates' scalefactors (13818-3 §2.4.3.2), in the partitions
    /// `scalefac_compress` picks.
    fn lsf_scalefactors(
        &self,
        bits: &mut BitReader<'_>,
        info: &Granule,
        channel: usize,
    ) -> Result<Scalefactors, DecodeError> {
        let intensity = self.header.mode == Mode::JointStereo
            && self.header.mode_extension & 1 == 1
            && channel == 1;
        let compress = info.scalefac_compress;
        let (slen, partition) = if intensity {
            let compress = compress >> 1;
            match compress {
                0..180 => ([compress / 36, compress % 36 / 6, compress % 36 % 6, 0], 3),
                180..244 => {
                    let rest = compress - 180;
                    ([(rest % 64) >> 4, (rest % 16) >> 2, rest % 4, 0], 4)
                }
                _ => {
                    let rest = compress - 244;
                    ([rest / 3, rest % 3, 0, 0], 5)
                }
            }
        } else {
            match compress {
                0..400 => (
                    [
                        (compress >> 4) / 5,
                        (compress >> 4) % 5,
                        (compress % 16) >> 2,
                        compress % 4,
                    ],
                    0,
                ),
                400..500 => {
                    let rest = compress - 400;
                    ([(rest >> 2) / 5, (rest >> 2) % 5, rest % 4, 0], 1)
                }
                _ => {
                    let rest = compress - 500;
                    ([rest / 3, rest % 3, 0, 0], 2)
                }
            }
        };
        let kind = match info.block {
            Block::Long { .. } => 0,
            Block::Short { mixed: false } => 1,
            Block::Short { mixed: true } => 2,
        };
        let counts = LSF_PARTITIONS[partition][kind];
        let mut values = [(0u8, 0u8); 39];
        let mut at = 0;
        for (&count, &width) in counts.iter().zip(&slen) {
            // A position of no bits is zero, which is legal; one of `width`
            // bits cannot take its largest value.
            let limit = match width {
                0 => u8::MAX,
                _ => {
                    u8::try_from((1u32 << width) - 1).map_err(|_| DecodeError::MpegFrameInvalid)?
                }
            };
            for _ in 0..count {
                values[at] = (field(bits, width)?, limit);
                at += 1;
            }
        }
        let mut out = NO_SCALEFACTORS;
        let mut next = values[..at].iter().copied();
        let mut take = || next.next().unwrap_or((0, 0));
        match info.block {
            Block::Long { .. } => {
                for band in 0..21 {
                    (out.long[band], out.long_limit[band]) = take();
                }
            }
            Block::Short { mixed } => {
                let first_short = if mixed {
                    for band in 0..6 {
                        (out.long[band], out.long_limit[band]) = take();
                    }
                    MIXED_SHORT_START
                } else {
                    0
                };
                for band in first_short..12 {
                    for window in 0..3 {
                        (out.short[band][window], out.short_limit[band][window]) = take();
                    }
                }
            }
        }
        Ok(out)
    }

    /// Decode the Huffman-coded spectrum up to `end` into `quantised`,
    /// answering the lines below which any value is non-zero.
    fn spectrum(
        &self,
        bits: &mut BitReader<'_>,
        info: &Granule,
        end: usize,
        quantised: &mut [i32; LINES],
    ) -> Result<usize, DecodeError> {
        let big = (info.big_values as usize * 2).min(LINES);
        let mut line = 0;
        while line < big {
            let region = if line < info.regions[0] {
                0
            } else if line < info.regions[1] {
                1
            } else {
                2
            };
            if let Some((x, y)) = self
                .lookups
                .pair(bits, info.table_select[region])
                .map_err(short)?
            {
                quantised[line] = x;
                quantised[line + 1] = y;
            }
            line += 2;
        }
        if bits.bits_consumed() > end {
            return Err(DecodeError::MpegFrameInvalid);
        }
        while line + 4 <= LINES && bits.bits_consumed() < end {
            // A last code that overran its granule's bits is not one the
            // encoder wrote; its signs may run on, as the reference decoder
            // reads them.
            let Ok(code) = self.lookups.count1(bits, info.count1_table) else {
                break;
            };
            if bits.bits_consumed() > end {
                break;
            }
            quantised[line..line + 4].copy_from_slice(&count1_signs(bits, code));
            line += 4;
        }
        Ok(quantised
            .iter()
            .rposition(|&value| value != 0)
            .map_or(0, |last| last + 1))
    }

    /// Requantise `quantised` into `spectrum` (§2.4.3.4.7.1), in the order
    /// it was coded: long bands, then short bands window by window.
    fn requantise(
        &self,
        info: &Granule,
        factors: &Scalefactors,
        quantised: &[i32; LINES],
        spectrum: &mut [f32; LINES],
    ) {
        let shift = if info.scalefac_scale { 4 } else { 2 };
        let gain = info.global_gain - 210;
        let mut put = |at: usize, quarters: i32| {
            let value = quantised[at];
            let magnitude = self.tables.pow43[value.unsigned_abs() as usize];
            let scaled = magnitude * quarter_power(quarters);
            spectrum[at] = if value < 0 { -scaled } else { scaled };
        };
        let bands = self.layout.bands;
        let mut line = 0;
        let long_bands = match info.block {
            Block::Long { .. } => 22,
            Block::Short { mixed: true } => usize::from(bands.mixed_long),
            Block::Short { mixed: false } => 0,
        };
        let long = bands.long.iter().zip(&factors.long).zip(&PRETAB);
        for ((&width, &factor), &pre) in long.take(long_bands) {
            let pre = if info.preflag { pre } else { 0 };
            let quarters = gain - shift * i32::from(factor + pre);
            let width = usize::from(width);
            for at in line..line + width {
                put(at, quarters);
            }
            line += width;
        }
        if long_bands == 22 {
            return;
        }
        let first_short = if long_bands > 0 { MIXED_SHORT_START } else { 0 };
        for band in first_short..13 {
            let width = usize::from(bands.short[band]);
            for window in 0..3 {
                let quarters = gain
                    - 8 * info.subblock_gain[window]
                    - shift * i32::from(factors.short[band][window]);
                for at in line..line + width {
                    put(at, quarters);
                }
                line += width;
            }
        }
    }

    /// Joint stereo (§2.4.3.4.9; 13818-3 §2.4.3.4.9.3): mid/side below the
    /// intensity bound, intensity above it.
    fn stereo(&self, granule: usize, right: &Scalefactors, spectra: &mut [[f32; LINES]; 2]) {
        use core::f32::consts::FRAC_1_SQRT_2;
        let info = &self.side.granules[granule][1];
        let mid_side = self.header.mode_extension & 2 != 0;
        let mut bands = [CodedBand::default(); MOST_BANDS];
        let bands = self.coded_bands(info, right, &mut bands);
        if self.header.mode_extension & 1 != 0 {
            self.intensity_scales(info, &spectra[1], bands);
        }
        let [left, right_lines] = spectra;
        for band in &*bands {
            let lines = band.start..band.start + band.width;
            let pairs = left[lines.clone()].iter_mut().zip(&mut right_lines[lines]);
            match band.scale {
                Some([left_scale, right_scale]) => {
                    for (left, right) in pairs {
                        *right = *left * right_scale;
                        *left *= left_scale;
                    }
                }
                None if mid_side => {
                    for (left, right) in pairs {
                        let (mid, side) = (*left, *right);
                        *left = (mid + side) * FRAC_1_SQRT_2;
                        *right = (mid - side) * FRAC_1_SQRT_2;
                    }
                }
                None => {}
            }
        }
    }

    /// The granule's bands in coded order, each with the right channel's
    /// value for it: long bands, then short bands window by window.
    fn coded_bands<'b>(
        &self,
        info: &Granule,
        right: &Scalefactors,
        bands: &'b mut [CodedBand; MOST_BANDS],
    ) -> &'b mut [CodedBand] {
        let widths = self.layout.bands;
        let long_bands = match info.block {
            Block::Long { .. } => 22,
            Block::Short { mixed: true } => usize::from(widths.mixed_long),
            Block::Short { mixed: false } => 0,
        };
        let long = widths.long.iter().zip(&right.long).zip(&right.long_limit);
        let long = long
            .take(long_bands)
            .map(|((&width, &position), &limit)| (width, position, limit));
        let shorts = match long_bands {
            22 => 0..0,
            0 => 0..13,
            _ => MIXED_SHORT_START..13,
        };
        let short = widths.short[shorts.clone()].iter();
        let short = short
            .zip(&right.short[shorts.clone()])
            .zip(&right.short_limit[shorts]);
        let short = short.flat_map(|((&width, positions), limits)| {
            positions
                .iter()
                .zip(limits)
                .map(move |(&position, &limit)| (width, position, limit))
        });
        let mut count = 0;
        let mut start = 0;
        for (band, (width, position, limit)) in bands.iter_mut().zip(long.chain(short)) {
            let width = usize::from(width);
            *band = CodedBand {
                start,
                width,
                position,
                limit,
                scale: None,
            };
            start += width;
            count += 1;
        }
        &mut bands[..count]
    }

    /// Give every intensity-coded band its scales: each band above the right
    /// channel's highest with a non-zero line — per window for short blocks,
    /// across the granule otherwise — whose position its band can take.
    ///
    /// The top band carries no position of its own: it takes the one below
    /// it, or the centre where that band is not intensity-coded — the
    /// reading the compliance streams' reference output holds.
    fn intensity_scales(
        &self,
        info: &Granule,
        right_lines: &[f32; LINES],
        bands: &mut [CodedBand],
    ) {
        let windows = if matches!(info.block, Block::Short { .. }) {
            3
        } else {
            1
        };
        let mut highest: [Option<usize>; 3] = [None; 3];
        for (at, band) in bands.iter().enumerate() {
            if right_lines[band.start..band.start + band.width]
                .iter()
                .any(|&v| v != 0.0)
            {
                highest[at % 3] = Some(at);
            }
        }
        if !matches!(info.block, Block::Short { mixed: false }) {
            highest = [highest.iter().copied().max().flatten(); 3];
        }
        let centre = if self.header.lsf() { 0 } else { 3 };
        let count = bands.len();
        for (class, &top_highest) in highest.iter().enumerate().take(windows) {
            let top = count - windows + class;
            let below = top - windows;
            let (position, limit) = if top_highest.is_some_and(|at| at >= below) {
                (centre, u8::MAX)
            } else {
                (bands[below].position, bands[below].limit)
            };
            bands[top].position = position;
            bands[top].limit = limit;
        }
        for (at, band) in bands.iter_mut().enumerate() {
            if highest[at % 3].is_none_or(|highest| at > highest) {
                band.scale = self.intensity_scale(band.position, band.limit, info);
            }
        }
    }

    /// The left and right scales intensity position `position` stands for,
    /// or `None` for a position its band cannot take — MPEG-1's 7 and above,
    /// the lower rates' `limit` — which leaves the band to mid/side.
    fn intensity_scale(&self, position: u8, limit: u8, info: &Granule) -> Option<[f32; 2]> {
        if !self.header.lsf() {
            return self.tables.ratios.get(usize::from(position)).copied();
        }
        if position == limit {
            return None;
        }
        let quarters = if info.scalefac_compress & 1 == 0 {
            1
        } else {
            2
        };
        let step = |count: i32| quarter_power(-quarters * count);
        let position = i32::from(position);
        Some(match position {
            0 => [1.0, 1.0],
            odd if odd % 2 == 1 => [step((odd + 1) / 2), 1.0],
            even => [1.0, step(even / 2)],
        })
    }

    /// Reorder, antialias and run the hybrid filterbank over one channel's
    /// granule, writing its 18 slots of subband samples to `out`.
    fn hybrid(
        &self,
        info: &Granule,
        spectrum: &mut [f32; LINES],
        nonzero: usize,
        overlap: &mut [[f32; SLOTS]; SUBBANDS],
        imdcts: &mut Imdcts,
        out: &mut [[f32; SUBBANDS]],
    ) {
        let mixed_long = self.layout.long_lines / SLOTS;
        let long_subbands = match info.block {
            Block::Long { .. } => SUBBANDS,
            Block::Short { mixed: true } => mixed_long,
            Block::Short { mixed: false } => 0,
        };
        if long_subbands < SUBBANDS {
            self.reorder(spectrum, long_subbands);
        }
        // Subbands holding a non-zero line; the butterflies across the
        // boundary above them still reach the first silent one.
        let used = nonzero.div_ceil(SLOTS);
        let boundaries = if long_subbands == SUBBANDS {
            used.min(SUBBANDS - 1)
        } else {
            long_subbands.saturating_sub(1)
        };
        for subband in 0..boundaries {
            for i in 0..8 {
                let low = subband * SLOTS + 17 - i;
                let high = (subband + 1) * SLOTS + i;
                let (a, b) = (spectrum[low], spectrum[high]);
                spectrum[low] = a * self.tables.cs[i] - b * self.tables.ca[i];
                spectrum[high] = b * self.tables.cs[i] + a * self.tables.ca[i];
            }
        }
        let live = if long_subbands < SUBBANDS {
            SUBBANDS
        } else {
            (used + 1).min(SUBBANDS)
        };
        for subband in 0..SUBBANDS {
            let mut z = [0f32; 36];
            if subband < live {
                let lines = &spectrum[subband * SLOTS..(subband + 1) * SLOTS];
                if subband < long_subbands {
                    let kind = match info.block {
                        Block::Long { mixed: true, .. } if subband < mixed_long => 0,
                        Block::Long { kind, .. } => usize::from(kind),
                        Block::Short { .. } => 0,
                    };
                    long_block(&mut imdcts.long, lines, &self.tables.windows[kind], &mut z);
                } else {
                    self.short_blocks(&mut imdcts.short, lines, &mut z);
                }
            }
            for slot in 0..SLOTS {
                let mut sample = z[slot] + overlap[subband][slot];
                if subband % 2 == 1 && slot % 2 == 1 {
                    sample = -sample;
                }
                out[slot][subband] = sample;
            }
            overlap[subband].copy_from_slice(&z[SLOTS..]);
        }
    }

    /// Put short blocks' lines, coded band by band and window by window, in
    /// the order the filterbank reads: window-interleaved within each band.
    fn reorder(&self, spectrum: &mut [f32; LINES], long_subbands: usize) {
        let first_line = long_subbands * SLOTS;
        let first_short = if long_subbands > 0 {
            MIXED_SHORT_START
        } else {
            0
        };
        let mut ordered = [0f32; LINES];
        let mut at = first_line;
        for band in first_short..13 {
            let width = usize::from(self.layout.bands.short[band]);
            let start = self.layout.short_start(band);
            for window in 0..3 {
                for freq in 0..width {
                    let to = 3 * (start + freq) + window;
                    if let (Some(slot), Some(&value)) = (ordered.get_mut(to), spectrum.get(at)) {
                        *slot = value;
                    }
                    at += 1;
                }
            }
        }
        spectrum[first_line..].copy_from_slice(&ordered[first_line..]);
    }

    /// Each window's six interleaved lines into its twelve samples,
    /// windowed and laid six apart across the long block's middle.
    fn short_blocks(&self, imdct: &mut Mdct, lines: &[f32], z: &mut [f32; 36]) {
        let mut coefficients = [0f32; 6];
        let mut samples = [0f32; 12];
        for window in 0..3 {
            for (k, coefficient) in coefficients.iter_mut().enumerate() {
                *coefficient = lines[3 * k + window];
            }
            imdct.inverse(&coefficients, &mut samples);
            for (i, sample) in samples.iter().enumerate() {
                z[6 + 6 * window + i] += sample * self.tables.short_window[i];
            }
        }
    }
}

/// A long block's eighteen lines into its 36 samples, windowed.
fn long_block(imdct: &mut Mdct, lines: &[f32], window: &[f32; 36], z: &mut [f32; 36]) {
    imdct.inverse(lines, z);
    for (sample, weight) in z.iter_mut().zip(window) {
        *sample *= weight;
    }
}

#[cfg(test)]
#[path = "layer3_tests.rs"]
mod tests;

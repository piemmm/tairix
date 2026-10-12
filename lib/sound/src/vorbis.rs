//! Vorbis I (the Xiph.Org Vorbis I specification): the three headers, and
//! audio packets decoded to interleaved 32-bit floats.
//!
//! The codec knows packets and nothing of the container they arrive in: the
//! Ogg mapping hands it the headers and then each audio packet, and decides
//! which of each packet's samples the stream keeps.
//!
//! A packet's samples follow the specification's framing exactly: the first
//! packet after a reset returns none, and each one after it the span from the
//! centre of the block before to the centre of its own. The windows are
//! chosen by the blocks' actual sizes, as libvorbis chooses them, so a packet
//! whose window flags misstate its neighbours still overlaps them correctly.
//!
//! A setup is held to stated bounds — its codebooks' entries and their
//! expanded vectors — that admit many times what any encoder writes, so a
//! hostile setup costs a bounded amount of memory. A packet is held to the
//! most bits its decode can consume.

mod bits;
mod codebook;
mod floor;
mod residue;
#[cfg(test)]
pub(crate) mod writer;

use alloc::vec::Vec;

use tairix_abi::driver::audio::ChannelMap;
use tairix_util::fallible::filled;
use tairix_util::mathf::sin;

use self::bits::{ilog, Bits};
use self::codebook::Codebook;
use self::floor::{Decoded, Floor};
use self::residue::{Bundle, Residue};
use crate::transform::Mdct;
use crate::{DecodeError, DecodeLimits};

/// The identification header's length.
pub(crate) const IDENTIFICATION_LEN: usize = 30;

/// A header packet's type byte and the signature after it.
pub(crate) const HEADER_PREFIX_LEN: usize = 7;

const SIGNATURE: &[u8; 6] = b"vorbis";
const IDENTIFICATION: u8 = 1;
const COMMENT: u8 = 3;
const SETUP: u8 = 5;

/// The exponents of the smallest and largest block sizes the format admits:
/// 64 and 8192 samples.
const SMALLEST_BLOCK: u32 = 6;
const LARGEST_BLOCK: u32 = 13;

/// The most bytes a setup header may take. libvorbis writes about 4 KiB at
/// any quality and 11 KiB for 5.1; older encoders not much more.
pub(crate) const MAX_SETUP_LEN: usize = 256 * 1024;

/// The most codebook entries a setup may declare in all: libvorbis's 5.1
/// setups declare about twenty thousand.
const MAX_ENTRIES: u32 = 1 << 18;

/// The most vector values a setup's codebooks may expand to in all:
/// libvorbis's 5.1 setups expand to under ninety thousand.
const MAX_VALUES: usize = 1 << 19;

/// The most bits one spectral line's residue can consume: a 32-bit codeword
/// for each of eight passes, and a classword's share.
const MAX_LINE_BYTES: usize = 36;

/// The most bytes one channel's floor can consume: 31 partitions of a
/// classword and eight values, each a 32-bit codeword.
const MAX_FLOOR_BYTES: usize = 1152;

/// Whether `packet` is a Vorbis header of `kind`.
fn is_header(packet: &[u8], kind: u8) -> bool {
    packet.first() == Some(&kind) && packet.get(1..HEADER_PREFIX_LEN) == Some(SIGNATURE)
}

/// Whether `packet` opens a Vorbis stream.
pub(crate) fn is_identification(packet: &[u8]) -> bool {
    is_header(packet, IDENTIFICATION)
}

/// Whether `prefix` opens a comment header.
pub(crate) fn is_comment(prefix: &[u8]) -> bool {
    is_header(prefix, COMMENT)
}

/// Whether the byte after a comment header's fields carries its framing bit.
pub(crate) const fn is_framed(byte: u8) -> bool {
    byte & 1 == 1
}

/// What the identification header states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identification {
    pub(crate) channels: u8,
    pub(crate) rate: u32,
    blocksizes: [usize; 2],
}

impl Identification {
    /// Read the identification header `packet`.
    pub(crate) fn parse(packet: &[u8]) -> Result<Self, DecodeError> {
        let bad = DecodeError::VorbisBadIdentification;
        let header = packet
            .first_chunk::<IDENTIFICATION_LEN>()
            .filter(|_| is_identification(packet))
            .ok_or(bad)?;
        let le32 = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let (small, large) = (u32::from(header[28] & 0x0F), u32::from(header[28] >> 4));
        let valid = le32(7) == 0
            && header[11] != 0
            && le32(12) != 0
            && (SMALLEST_BLOCK..=LARGEST_BLOCK).contains(&small)
            && (small..=LARGEST_BLOCK).contains(&large)
            && is_framed(header[29]);
        if !valid {
            return Err(bad);
        }
        Ok(Self {
            channels: header[11],
            rate: le32(12),
            blocksizes: [1 << small, 1 << large],
        })
    }

    /// The most bytes an audio packet of block `size` may usefully hold.
    pub(crate) fn max_packet_len(&self, size: usize) -> usize {
        max_packet_len(size, usize::from(self.channels))
    }

    /// The most bytes an audio packet of the stream's long block may hold.
    pub(crate) fn max_long_packet_len(&self) -> usize {
        self.max_packet_len(self.blocksizes[1])
    }

    /// Samples a channel's long block spans.
    pub(crate) const fn long_block(&self) -> usize {
        self.blocksizes[1]
    }
}

/// The channel map of `count` channels in the order Vorbis I lays them out
/// (section 4.3.9), within `limits`. Past eight the order is the encoder's
/// own, so it is refused by name.
pub(crate) fn channel_map(count: u8, limits: &DecodeLimits) -> Result<ChannelMap, DecodeError> {
    use tairix_abi::driver::audio::ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, Mono, RearCentre, RearLeft, RearRight,
        SideLeft, SideRight,
    };
    if count == 0 {
        return Err(DecodeError::NoChannels);
    }
    if count > limits.max_channels() {
        return Err(DecodeError::ChannelsExceedLimit);
    }
    let positions: &[_] = match count {
        1 => &[Mono],
        2 => &[FrontLeft, FrontRight],
        3 => &[FrontLeft, FrontCentre, FrontRight],
        4 => &[FrontLeft, FrontRight, RearLeft, RearRight],
        5 => &[FrontLeft, FrontCentre, FrontRight, RearLeft, RearRight],
        6 => &[
            FrontLeft,
            FrontCentre,
            FrontRight,
            RearLeft,
            RearRight,
            LowFrequency,
        ],
        7 => &[
            FrontLeft,
            FrontCentre,
            FrontRight,
            SideLeft,
            SideRight,
            RearCentre,
            LowFrequency,
        ],
        8 => &[
            FrontLeft,
            FrontCentre,
            FrontRight,
            SideLeft,
            SideRight,
            RearLeft,
            RearRight,
            LowFrequency,
        ],
        _ => return Err(DecodeError::ChannelLayoutUnsupported),
    };
    ChannelMap::new(positions).map_err(|_| DecodeError::ChannelLayoutUnsupported)
}

/// The most bytes an audio packet of block `size` across `channels` channels
/// can consume.
const fn max_packet_len(size: usize, channels: usize) -> usize {
    size / 2 * channels * MAX_LINE_BYTES + channels * MAX_FLOOR_BYTES + 8
}

/// The most bytes of packets a stream of `channels` channels reads to return
/// `frames` frames: a packet returns a quarter of each of two blocks, so at
/// least half the shortest, and a read can begin with one that returns none
/// beside the one it ends in.
pub(crate) const fn max_working_set(channels: usize, frames: u64) -> u64 {
    let short = 1 << SMALLEST_BLOCK;
    let per_block = max_packet_len(short, channels) as u64;
    frames.div_ceil(short as u64 / 2) * per_block
        + 2 * max_packet_len(1 << LARGEST_BLOCK, channels) as u64
}

/// What a setup has charged against its bounds so far.
#[derive(Default)]
pub(super) struct Budget {
    entries: u32,
    values: usize,
}

impl Budget {
    fn charge_entries(&mut self, entries: u32) -> Result<(), DecodeError> {
        self.entries = self
            .entries
            .checked_add(entries)
            .filter(|&total| total <= MAX_ENTRIES)
            .ok_or(DecodeError::VorbisSetupTooLarge)?;
        Ok(())
    }

    fn charge_values(&mut self, values: usize) -> Result<(), DecodeError> {
        self.values = self
            .values
            .checked_add(values)
            .filter(|&total| total <= MAX_VALUES)
            .ok_or(DecodeError::VorbisSetupTooLarge)?;
        Ok(())
    }
}

/// One submap of a mapping: its floor and residue, and its channels in
/// order.
struct Submap {
    floor: u8,
    residue: u8,
    channels: Vec<u8>,
}

struct Mapping {
    /// Magnitude and angle channel of each coupling step.
    coupling: Vec<(u8, u8)>,
    /// Each channel's submap.
    mux: Vec<u8>,
    submaps: Vec<Submap>,
}

#[derive(Copy, Clone)]
struct Mode {
    long: bool,
    mapping: u8,
}

struct Setup {
    books: Vec<Codebook>,
    floors: Vec<Floor>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<Mode>,
    mode_bits: u32,
}

/// A count field: `width` bits, plus one.
fn count(bits: &mut Bits<'_>, width: u32) -> Result<usize, DecodeError> {
    Ok(bits.read(width).map_err(|_| DecodeError::VorbisBadSetup)? as usize + 1)
}

/// A vector of `len` reserved up front, refusing the allocation by name.
fn reserved<T>(len: usize) -> Result<Vec<T>, DecodeError> {
    let mut items = Vec::new();
    if items.try_reserve_exact(len).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    Ok(items)
}

/// Read the setup header `packet` for the stream `identification` states.
fn read_setup(packet: &[u8], identification: &Identification) -> Result<Setup, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    if packet.len() > MAX_SETUP_LEN {
        return Err(DecodeError::VorbisSetupTooLarge);
    }
    if !is_header(packet, SETUP) {
        return Err(DecodeError::VorbisBadSetup);
    }
    let mut bits = Bits::new(&packet[HEADER_PREFIX_LEN..]);
    let mut budget = Budget::default();
    let book_count = count(&mut bits, 8)?;
    let mut books = reserved(book_count)?;
    for _ in 0..book_count {
        books.push(codebook::read(&mut bits, &mut budget)?);
    }
    for _ in 0..count(&mut bits, 6)? {
        if bits.read(16).map_err(bad)? != 0 {
            return Err(DecodeError::VorbisBadSetup);
        }
    }
    let floor_count = count(&mut bits, 6)?;
    let mut floors = reserved(floor_count)?;
    for _ in 0..floor_count {
        floors.push(floor::read(&mut bits, &books, identification.blocksizes)?);
    }
    let residue_count = count(&mut bits, 6)?;
    let mut residues = reserved(residue_count)?;
    for _ in 0..residue_count {
        residues.push(residue::read(&mut bits, &books)?);
    }
    let mapping_count = count(&mut bits, 6)?;
    let mut mappings = reserved(mapping_count)?;
    for _ in 0..mapping_count {
        mappings.push(read_mapping(
            &mut bits,
            identification.channels,
            floor_count,
            residue_count,
        )?);
    }
    let mode_count = count(&mut bits, 6)?;
    let mut modes = reserved(mode_count)?;
    for _ in 0..mode_count {
        let long = bits.flag().map_err(bad)?;
        let window = bits.read(16).map_err(bad)?;
        let transform = bits.read(16).map_err(bad)?;
        let mapping = bits.read(8).map_err(bad)?;
        if window != 0 || transform != 0 || mapping as usize >= mapping_count {
            return Err(DecodeError::VorbisBadSetup);
        }
        let mapping = u8::try_from(mapping).map_err(|_| DecodeError::VorbisBadSetup)?;
        modes.push(Mode { long, mapping });
    }
    if !bits.flag().map_err(bad)? {
        return Err(DecodeError::VorbisBadSetup);
    }
    let mode_bits = ilog(u32::try_from(mode_count - 1).map_err(|_| DecodeError::VorbisBadSetup)?);
    Ok(Setup {
        books,
        floors,
        residues,
        mappings,
        modes,
        mode_bits,
    })
}

fn read_mapping(
    bits: &mut Bits<'_>,
    channels: u8,
    floors: usize,
    residues: usize,
) -> Result<Mapping, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    if bits.read(16).map_err(bad)? != 0 {
        return Err(DecodeError::VorbisBadSetup);
    }
    let submap_count = if bits.flag().map_err(bad)? {
        count(bits, 4)?
    } else {
        1
    };
    let mut coupling = Vec::new();
    if bits.flag().map_err(bad)? {
        let steps = count(bits, 8)?;
        coupling = reserved(steps)?;
        let width = ilog(u32::from(channels) - 1);
        for _ in 0..steps {
            let magnitude = bits.read(width).map_err(bad)?;
            let angle = bits.read(width).map_err(bad)?;
            if magnitude == angle
                || magnitude >= u32::from(channels)
                || angle >= u32::from(channels)
            {
                return Err(DecodeError::VorbisBadSetup);
            }
            let to_u8 = |value: u32| u8::try_from(value).map_err(|_| DecodeError::VorbisBadSetup);
            coupling.push((to_u8(magnitude)?, to_u8(angle)?));
        }
    }
    if bits.read(2).map_err(bad)? != 0 {
        return Err(DecodeError::VorbisBadSetup);
    }
    let mut mux = filled(usize::from(channels), 0u8).ok_or(DecodeError::OutOfMemory)?;
    if submap_count > 1 {
        for slot in &mut mux {
            let submap = bits.read(4).map_err(bad)?;
            if submap as usize >= submap_count {
                return Err(DecodeError::VorbisBadSetup);
            }
            *slot = u8::try_from(submap).map_err(|_| DecodeError::VorbisBadSetup)?;
        }
    }
    let mut submaps = reserved(submap_count)?;
    for index in 0..submap_count {
        bits.read(8).map_err(bad)?;
        let floor = bits.read(8).map_err(bad)?;
        let residue = bits.read(8).map_err(bad)?;
        if floor as usize >= floors || residue as usize >= residues {
            return Err(DecodeError::VorbisBadSetup);
        }
        let mut members = reserved(usize::from(channels))?;
        members.extend(
            mux.iter()
                .zip(0u8..)
                .filter(|&(&submap, _)| usize::from(submap) == index)
                .map(|(_, channel)| channel),
        );
        submaps.push(Submap {
            floor: u8::try_from(floor).map_err(|_| DecodeError::VorbisBadSetup)?,
            residue: u8::try_from(residue).map_err(|_| DecodeError::VorbisBadSetup)?,
            channels: members,
        });
    }
    Ok(Mapping {
        coupling,
        mux,
        submaps,
    })
}

/// The rising half of the Vorbis window over `len` samples:
/// `sin(π/2 · sin²((i + ½)/len · π/2))`.
fn slope(len: usize) -> Option<Vec<f32>> {
    let mut values = filled(len, 0.0f32)?;
    let span = f64::from(u32::try_from(len).ok()?);
    let quarter = core::f64::consts::FRAC_PI_2;
    for (value, i) in values.iter_mut().zip(0u32..) {
        let inner = sin((f64::from(i) + 0.5) / span * quarter);
        *value = crate::single(sin(quarter * inner * inner));
    }
    Some(values)
}

/// Undo one coupling step on a pair of lines (Vorbis I, section 4.3.5).
fn uncouple(magnitude: f32, angle: f32) -> (f32, f32) {
    match (magnitude > 0.0, angle > 0.0) {
        (true, true) => (magnitude, magnitude - angle),
        (true, false) => (magnitude + angle, magnitude),
        (false, true) => (magnitude, magnitude + angle),
        (false, false) => (magnitude - angle, magnitude),
    }
}

/// A Vorbis stream's decoder, past its headers.
pub(crate) struct Decoder {
    channels: usize,
    blocksizes: [usize; 2],
    setup: Setup,
    transforms: [Mdct; 2],
    slopes: [Vec<f32>; 2],
    floors: Vec<Decoded>,
    /// Which channels' residues the packet codes.
    coded: Vec<bool>,
    /// The packet's spectra, a block's half a channel, channel after channel.
    spectrum: Vec<f32>,
    /// One channel's inverse transform.
    time: Vec<f32>,
    /// Each channel's last block's second half, unwindowed: the overlap the
    /// next block adds to.
    saved: Vec<f32>,
    /// The last block's size, while one is held.
    previous: Option<usize>,
    classes: Vec<u8>,
    /// The last packet's samples, interleaved.
    pcm: Vec<f32>,
    held: usize,
}

impl Decoder {
    /// A decoder for the stream `identification` states, configured by the
    /// setup header `setup`.
    pub(crate) fn new(identification: &Identification, setup: &[u8]) -> Result<Self, DecodeError> {
        let setup = read_setup(setup, identification)?;
        let channels = usize::from(identification.channels);
        let blocksizes = identification.blocksizes;
        let half = blocksizes[1] / 2;
        let transform = |size: usize| Mdct::new(size).ok_or(DecodeError::OutOfMemory);
        let slopes = [
            slope(blocksizes[0] / 2).ok_or(DecodeError::OutOfMemory)?,
            slope(half).ok_or(DecodeError::OutOfMemory)?,
        ];
        let classes = setup
            .mappings
            .iter()
            .flat_map(|mapping| &mapping.submaps)
            .map(|submap| {
                setup.residues[usize::from(submap.residue)].classes_needed(
                    half,
                    submap.channels.len(),
                    &setup.books,
                )
            })
            .max()
            .unwrap_or(0);
        let mut floors = reserved(channels)?;
        floors.resize_with(channels, Decoded::default);
        let buffer = |len: usize| filled(len, 0.0f32).ok_or(DecodeError::OutOfMemory);
        Ok(Self {
            channels,
            blocksizes,
            transforms: [transform(blocksizes[0])?, transform(blocksizes[1])?],
            slopes,
            floors,
            coded: filled(channels, false).ok_or(DecodeError::OutOfMemory)?,
            spectrum: buffer(channels * half)?,
            time: buffer(blocksizes[1])?,
            saved: buffer(channels * half)?,
            previous: None,
            classes: filled(classes, 0u8).ok_or(DecodeError::OutOfMemory)?,
            pcm: buffer(channels * half)?,
            held: 0,
            setup,
        })
    }

    /// The mode an audio packet opens with, and its block's size index;
    /// `None` for a packet that is not audio, or that ends before its window
    /// is known, which the stream discards.
    fn block(&self, bits: &mut Bits<'_>) -> Option<(Mode, usize)> {
        if bits.read(1).ok()? != 0 {
            return None;
        }
        let mode = *self
            .setup
            .modes
            .get(bits.read(self.setup.mode_bits).ok()? as usize)?;
        if mode.long {
            bits.read(2).ok()?;
        }
        Some((mode, usize::from(mode.long)))
    }

    /// The samples a channel's block of `packet` spans, or `None` for a packet
    /// the stream discards.
    pub(crate) fn blocksize(&self, packet: &[u8]) -> Option<usize> {
        self.block(&mut Bits::new(packet))
            .map(|(_, size)| self.blocksizes[size])
    }

    /// Forget the block held, so the next packet decoded returns nothing and
    /// only primes the overlap: where decoding resumes after a seek.
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.held = 0;
    }

    /// The last packet's samples, interleaved.
    pub(crate) fn pcm(&self) -> &[f32] {
        &self.pcm[..self.held * self.channels]
    }

    /// Decode `packet`, answering the frames it returns, or `None` for a
    /// packet the stream discards.
    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<Option<usize>, DecodeError> {
        let mut bits = Bits::new(packet);
        let Some((mode, size)) = self.block(&mut bits) else {
            return Ok(None);
        };
        let half = self.blocksizes[size] / 2;
        let channels = self.channels;
        let setup = &self.setup;
        let mapping = &setup.mappings[usize::from(mode.mapping)];
        for (channel, decoded) in self.floors.iter_mut().enumerate() {
            let submap = &mapping.submaps[usize::from(mapping.mux[channel])];
            setup.floors[usize::from(submap.floor)].decode(&mut bits, &setup.books, decoded);
        }
        for (coded, decoded) in self.coded.iter_mut().zip(&self.floors) {
            *coded = decoded.used();
        }
        for &(magnitude, angle) in &mapping.coupling {
            let (magnitude, angle) = (usize::from(magnitude), usize::from(angle));
            if self.coded[magnitude] || self.coded[angle] {
                self.coded[magnitude] = true;
                self.coded[angle] = true;
            }
        }
        let spectrum = &mut self.spectrum[..channels * half];
        spectrum.fill(0.0);
        for submap in &mapping.submaps {
            let mut bundle = Bundle {
                spectrum: &mut *spectrum,
                half,
                channels: &submap.channels,
                decode: &self.coded,
            };
            setup.residues[usize::from(submap.residue)].decode(
                &mut bits,
                &setup.books,
                &mut bundle,
                &mut self.classes,
            );
        }
        for &(magnitude, angle) in mapping.coupling.iter().rev() {
            let (magnitude, angle) = (usize::from(magnitude), usize::from(angle));
            let (low, high) = (magnitude.min(angle), magnitude.max(angle));
            let (first, second) = spectrum.split_at_mut(high * half);
            let (low_lines, high_lines) = (&mut first[low * half..][..half], &mut second[..half]);
            let (magnitudes, angles) = if magnitude < angle {
                (low_lines, high_lines)
            } else {
                (high_lines, low_lines)
            };
            for (m, a) in magnitudes.iter_mut().zip(angles.iter_mut()) {
                (*m, *a) = uncouple(*m, *a);
            }
        }
        for (channel, decoded) in self.floors.iter().enumerate() {
            let submap = &mapping.submaps[usize::from(mapping.mux[channel])];
            let lines = &mut spectrum[channel * half..][..half];
            setup.floors[usize::from(submap.floor)].apply(decoded, size, lines);
        }
        self.synthesise(size)
    }

    /// Transform each channel's spectrum, overlap it with the block before
    /// and keep its second half for the block after.
    fn synthesise(&mut self, size: usize) -> Result<Option<usize>, DecodeError> {
        let half = self.blocksizes[size] / 2;
        let stride = self.blocksizes[1] / 2;
        let channels = self.channels;
        let frames = self
            .previous
            .map_or(0, |previous| self.blocksizes[previous] / 4 + half / 2);
        for channel in 0..channels {
            let time = &mut self.time[..2 * half];
            if self.floors[channel].used() {
                self.transforms[size].inverse(&self.spectrum[channel * half..][..half], time);
            } else {
                time.fill(0.0);
            }
            let saved = &mut self.saved[channel * stride..][..stride];
            if let Some(previous) = self.previous {
                let before = self.blocksizes[previous] / 2;
                let overlap = before.min(half);
                let window = &self.slopes[previous.min(size)];
                let lead = before / 2 - overlap / 2;
                let start = half / 2 - overlap / 2;
                let rising = time[start..start + overlap].iter().zip(window);
                let falling = saved[lead..lead + overlap].iter().zip(window.iter().rev());
                let samples = saved[..lead]
                    .iter()
                    .copied()
                    .chain(
                        rising
                            .zip(falling)
                            .map(|((now, up), (then, down))| now * up + then * down),
                    )
                    .chain(time[half / 2 + overlap / 2..half].iter().copied());
                for (slot, sample) in self.pcm[channel..]
                    .iter_mut()
                    .step_by(channels)
                    .zip(samples)
                {
                    *slot = sample;
                }
            }
            saved[..half].copy_from_slice(&time[half..2 * half]);
        }
        self.previous = Some(size);
        self.held = frames;
        if self.pcm().iter().any(|sample| !sample.is_finite()) {
            self.reset();
            return Err(DecodeError::VorbisPacketInvalid);
        }
        Ok(Some(frames))
    }
}

#[cfg(test)]
#[path = "vorbis_tests.rs"]
mod tests;

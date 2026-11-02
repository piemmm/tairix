//! Where a member's bytes are and how they are read (`plans/ARCHIVES.md`
//! §3): in the archive, stored or compressed, or written since and waiting in
//! the staging file.
//!
//! A compressed member is read through a decoder that runs from its start,
//! and a cursor keeps one decoding where the last read stopped, so a member
//! read through in order is decoded once. A read before a cursor's place
//! starts a fresh one. Each member's check — its CRC — is held to the bytes
//! once the decoder reaches its end, and a member whose bytes do not match is
//! refused rather than served.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::volume::{ARCHIVE_REGION, ARCHIVE_STAGING};
use tairix_abi::DriverError;
use tairix_compress::{arc, bzip2};
use tairix_fsserve::VolumeAccess;

use crate::region::{ByteInput, Input, Regions};

/// How a member's bytes are kept in the archive.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Coding {
    /// As they are.
    Stored,
    /// DEFLATE (RFC 1951), `packed` bytes of it.
    Deflate {
        /// The compressed length.
        packed: u64,
    },
    /// One bzip2 stream, `packed` bytes of it.
    Bzip2 {
        /// The compressed length.
        packed: u64,
    },
    /// One of the ARC family's methods, `packed` bytes of it, the stream
    /// proper after the first `skip` of them: a Spark or `ArcFS` member.
    Arc {
        /// The coded length, `skip` included.
        packed: u64,
        /// The bytes of the member's own ahead of its stream: Spark's code
        /// width.
        skip: u8,
        /// How the stream is coded.
        method: arc::Method,
    },
    /// At `at` of the decoded stream the whole archive is: a member of a
    /// compressed tar, or a lone compressed file.
    Stream,
    /// RAR entry `entry`, read through the archive's RAR reader.
    Rar {
        /// Its index among the archive's entries.
        entry: u32,
    },
    /// Not readable, for the reason given — an encryption, or a method this
    /// driver does not decode — its `packed` coded bytes kept as they are.
    Refused {
        /// Why its bytes are not read.
        why: DriverError,
        /// The coded length.
        packed: u64,
    },
}

/// What a member's bytes are checked against once read whole.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Check {
    /// Nothing.
    None,
    /// Their CRC-32 (IEEE).
    Crc32(u32),
    /// Their CRC-16/ARC.
    Crc16(u16),
}

/// A member's check, taken over its bytes as they are decoded.
enum Sum {
    None,
    Crc32(tairix_crc32::Crc32),
    Crc16(u16),
}

impl Sum {
    const fn of(check: Check) -> Self {
        match check {
            Check::None => Self::None,
            Check::Crc32(_) => Self::Crc32(tairix_crc32::Crc32::new()),
            Check::Crc16(_) => Self::Crc16(0),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::None => {}
            Self::Crc32(crc) => crc.update(bytes),
            Self::Crc16(crc) => *crc = tairix_crc16::update(*crc, bytes),
        }
    }

    /// Whether the bytes taken match `check`.
    fn holds(&self, check: Check) -> bool {
        match (self, check) {
            (Self::None, Check::None) => true,
            (Self::Crc32(crc), Check::Crc32(expected)) => crc.finish() == expected,
            (Self::Crc16(crc), Check::Crc16(expected)) => *crc == expected,
            _ => false,
        }
    }
}

/// What stands in the archive ahead of a member's bytes, where its record
/// does not say where they start.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Lead {
    /// Nothing: `at` is where they start.
    None,
    /// A zip local header at `at`, whose name and extra field they follow.
    ZipLocal,
}

/// A member's bytes in the archive.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Archived {
    /// Where its stored or compressed bytes start, or what leads them does.
    pub(crate) at: u64,
    pub(crate) lead: Lead,
    /// How many bytes it holds, decoded.
    pub(crate) len: u64,
    pub(crate) coding: Coding,
    pub(crate) check: Check,
}

impl Archived {
    /// How many bytes it takes in the archive.
    pub(crate) const fn packed(&self) -> u64 {
        match self.coding {
            Coding::Stored | Coding::Stream | Coding::Rar { .. } => self.len,
            Coding::Deflate { packed }
            | Coding::Bzip2 { packed }
            | Coding::Arc { packed, .. }
            | Coding::Refused { packed, .. } => packed,
        }
    }
}

/// One run of a staged file: `len` of its bytes, from the file offset it is
/// keyed by, kept at `at` of the staging file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Run {
    pub(crate) at: u64,
    pub(crate) len: u64,
}

/// A file written since the archive was read: its length, and the runs of
/// it the staging file holds; what no run covers reads as zeros.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Staged {
    pub(crate) len: u64,
    pub(crate) runs: BTreeMap<u64, Run>,
}

impl Staged {
    /// Note that bytes `offset..offset + len` of the file now lie at `at` of
    /// the staging file, which shadows whatever run held them before.
    pub(crate) fn wrote(&mut self, offset: u64, len: u64, at: u64) -> Result<(), DriverError> {
        let end = offset
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if len == 0 {
            return Ok(());
        }
        self.cut(offset, end);
        // A write that carries on from the run before it, in the file and in
        // the staging file alike, lengthens that run.
        let merged = self
            .runs
            .range_mut(..offset)
            .next_back()
            .filter(|(&start, run)| start + run.len == offset && run.at + run.len == at)
            .map(|(_, run)| run.len += len)
            .is_some();
        if !merged {
            self.runs.insert(offset, Run { at, len });
        }
        self.len = self.len.max(end);
        Ok(())
    }

    /// Take `start..end` of the file out of every run, splitting a run that
    /// spans it.
    fn cut(&mut self, start: u64, end: u64) {
        if let Some((&before, run)) = self.runs.range(..start).next_back() {
            let run = *run;
            let run_end = before + run.len;
            if run_end > start {
                self.runs.insert(
                    before,
                    Run {
                        at: run.at,
                        len: start - before,
                    },
                );
                if run_end > end {
                    self.runs.insert(
                        end,
                        Run {
                            at: run.at + (end - before),
                            len: run_end - end,
                        },
                    );
                }
            }
        }
        let inside: Vec<u64> = self.runs.range(start..end).map(|(&at, _)| at).collect();
        for key in inside {
            if let Some(run) = self.runs.remove(&key) {
                let run_end = key + run.len;
                if run_end > end {
                    self.runs.insert(
                        end,
                        Run {
                            at: run.at + (end - key),
                            len: run_end - end,
                        },
                    );
                }
            }
        }
    }

    /// Cut the file to `len` bytes, or extend it with zeros.
    pub(crate) fn truncate(&mut self, len: u64) {
        if len < self.len {
            self.cut(len, u64::MAX);
        }
        self.len = len;
    }

    /// Read up to `buf.len()` bytes at `offset`, answering how many.
    pub(crate) fn read<A: VolumeAccess>(
        &self,
        regions: &Regions<A>,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let want = usize::try_from(self.len.saturating_sub(offset))
            .map_or(buf.len(), |left| left.min(buf.len()));
        let buf = &mut buf[..want];
        buf.fill(0);
        let end = offset + want as u64;
        let first = self
            .runs
            .range(..=offset)
            .next_back()
            .map_or(offset, |(&start, _)| start);
        for (&start, run) in self.runs.range(first..end) {
            let run_end = start + run.len;
            if run_end <= offset {
                continue;
            }
            let from = start.max(offset);
            let to = run_end.min(end);
            let span =
                |at: u64| usize::try_from(at - offset).map_err(|_| DriverError::LengthOutOfRange);
            let into = &mut buf[span(from)?..span(to)?];
            let got = regions.read(ARCHIVE_STAGING, run.at + (from - start), into)?;
            if got != into.len() {
                return Err(DriverError::DeviceFault);
            }
        }
        Ok(want)
    }
}

/// Where a member's bytes are.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Data {
    /// In the archive.
    Archived(Archived),
    /// In the staging file.
    Staged(Staged),
}

impl Data {
    /// A file holding nothing.
    pub(crate) fn empty() -> Self {
        Self::Staged(Staged::default())
    }

    /// How many bytes it holds.
    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::Archived(archived) => archived.len,
            Self::Staged(staged) => staged.len,
        }
    }
}

/// One value on the heap, placed there fallibly: a decoder's state is tens
/// of kilobytes.
pub(crate) struct Heap<T>(Vec<T>);

impl<T> Heap<T> {
    /// `value`, on the heap.
    pub(crate) fn new(value: T) -> Result<Self, DriverError> {
        let mut slot = Vec::new();
        slot.try_reserve_exact(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        slot.push(value);
        Ok(Self(slot))
    }

    /// The value.
    pub(crate) fn get(&mut self) -> Result<&mut T, DriverError> {
        self.0.first_mut().ok_or(DriverError::DeviceFault)
    }

    /// The value, to read.
    pub(crate) fn get_ref(&self) -> Result<&T, DriverError> {
        self.0.first().ok_or(DriverError::DeviceFault)
    }
}

/// A DEFLATE stream's decoder.
struct Inflate(Heap<tairix_compress::inflate::Inflater>);

impl Inflate {
    fn new() -> Result<Self, DriverError> {
        Heap::new(tairix_compress::inflate::Inflater::new()).map(Self)
    }

    /// Decode into `out` from what `input` holds, answering the bytes
    /// produced; zero once the stream has ended.
    fn decode<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        input: &mut Input,
        out: &mut [u8],
    ) -> Result<usize, DriverError> {
        let inflater = self.0.get()?;
        loop {
            if inflater.is_finished() {
                return Ok(0);
            }
            let src = input.fill(regions)?;
            if src.is_empty() {
                // The member's bytes ran out before its stream ended.
                return Err(DriverError::MediumError);
            }
            let progress = inflater
                .inflate(src, out)
                .map_err(|_| DriverError::MediumError)?;
            input.consume(progress.consumed);
            if progress.produced > 0 || progress.finished {
                return Ok(progress.produced);
            }
        }
    }
}

/// A bzip2 stream's decoder, its workspace sized to the stream's level or,
/// for a member shorter than a block, to the member.
struct Bunzip {
    decoder: Heap<bzip2::Decoder<Vec<u32>>>,
    in_block: bool,
}

impl Bunzip {
    /// A decoder standing past the header of the stream `input` opens, which
    /// decodes to `len` bytes.
    fn new<A: VolumeAccess>(
        regions: &Regions<A>,
        input: &mut Input,
        len: u64,
    ) -> Result<Self, DriverError> {
        let level = match *input.fill(regions)? {
            [b'B', b'Z', b'h', digit @ b'1'..=b'9', ..] => digit - b'0',
            _ => return Err(DriverError::MediumError),
        };
        let symbols = usize::try_from(bzip2::block_symbols(len)).unwrap_or(usize::MAX);
        let work = tairix_util::fallible::filled(bzip2::decoder_work(level).min(symbols), 0u32)
            .ok_or(DriverError::OutOfMemory)?;
        let mut decoder = Heap::new(bzip2::Decoder::new(work))?;
        let mut bytes = ByteInput::new(input, regions);
        decoder
            .get()?
            .begin(&mut bytes)
            .map_err(|_| bytes.fault_or(DriverError::MediumError))?;
        Ok(Self {
            decoder,
            in_block: false,
        })
    }

    /// Decode into `out`, answering the bytes produced; zero once the
    /// stream has ended.
    fn decode<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        input: &mut Input,
        out: &mut [u8],
    ) -> Result<usize, DriverError> {
        let decoder = self.decoder.get()?;
        loop {
            if self.in_block {
                let got = decoder.read(out).map_err(|_| DriverError::MediumError)?;
                if got > 0 {
                    return Ok(got);
                }
                self.in_block = false;
            }
            if decoder.ended() {
                return Ok(0);
            }
            let mut bytes = ByteInput::new(input, regions);
            match decoder.next_block(&mut bytes) {
                Ok(bzip2::Next::Block { .. }) => self.in_block = true,
                Ok(bzip2::Next::End) => return Ok(0),
                Err(_) => return Err(bytes.fault_or(DriverError::MediumError)),
            }
        }
    }
}

/// How a cursor decodes.
enum Decoding {
    Inflate(Inflate),
    Bunzip(Bunzip),
    Arc(Heap<arc::Decoder<Vec<u32>, Vec<u8>>>),
}

/// One member being decoded in order.
struct Cursor {
    node: u64,
    content_gen: u64,
    /// Bytes decoded so far: where the next read can carry on from.
    produced: u64,
    decoding: Decoding,
    input: Input,
    sum: Sum,
    /// When it was last read, for choosing one to drop.
    used: u64,
}

/// The most members decoded at once: one each for a few readers reading
/// different members in order.
const CURSORS: usize = 4;

/// Bytes skipped through a scratch buffer this long when a read starts past
/// a cursor's place.
const SKIP_LEN: usize = 16 * 1024;

/// A stored member being read in order, its check taken over the bytes as
/// they are read.
struct Verify {
    node: u64,
    content_gen: u64,
    /// Bytes checked so far: where the next read in order starts.
    upto: u64,
    sum: Sum,
    used: u64,
}

/// The members being decoded, the stored ones being checked, and the
/// archive's one decoded stream, or its RAR reader, where it has one.
#[derive(Default)]
pub(crate) struct Cursors {
    slots: Vec<Cursor>,
    verifying: Vec<Verify>,
    clock: u64,
    stream: Option<crate::stream::Stream>,
    rar: Option<crate::rar::Reader>,
}

impl Cursors {
    /// Forget every cursor of `node`.
    pub(crate) fn forget(&mut self, node: u64) {
        self.slots.retain(|cursor| cursor.node != node);
        self.verifying.retain(|verify| verify.node != node);
    }

    /// Forget every cursor: the archive they read is gone. A compressed
    /// archive's next stream is read through `stream`.
    pub(crate) fn clear(&mut self, stream: Option<crate::stream::Stream>) {
        self.slots.clear();
        self.verifying.clear();
        self.stream = stream;
    }

    /// Read a RAR archive's members through `reader`.
    pub(crate) fn set_rar(&mut self, reader: Option<crate::rar::Reader>) {
        self.rar = reader;
    }

    /// Take the stored bytes `read` at `offset` of `node`'s `archived` bytes
    /// into its check, where they carry on a read from its start: a read
    /// through a member in order is held to its check as it reaches the end,
    /// as a decoded member's is.
    fn verify(
        &mut self,
        (node, content_gen): (u64, u64),
        archived: &Archived,
        offset: u64,
        read: &[u8],
    ) -> Result<(), DriverError> {
        if archived.check == Check::None {
            return Ok(());
        }
        self.clock += 1;
        let found = self.verifying.iter().position(|verify| {
            verify.node == node && verify.content_gen == content_gen && verify.upto == offset
        });
        let at = match found {
            Some(at) => at,
            None if offset == 0 => {
                let fresh = Verify {
                    node,
                    content_gen,
                    upto: 0,
                    sum: Sum::of(archived.check),
                    used: 0,
                };
                if self.verifying.len() < CURSORS {
                    self.verifying
                        .try_reserve(1)
                        .map_err(|_| DriverError::OutOfMemory)?;
                    self.verifying.push(fresh);
                    self.verifying.len() - 1
                } else {
                    let oldest = self
                        .verifying
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, verify)| verify.used)
                        .map_or(0, |(at, _)| at);
                    self.verifying[oldest] = fresh;
                    oldest
                }
            }
            // A read out of order checks nothing.
            None => return Ok(()),
        };
        let verify = &mut self.verifying[at];
        verify.sum.update(read);
        verify.upto += read.len() as u64;
        verify.used = self.clock;
        if verify.upto < archived.len {
            return Ok(());
        }
        let holds = verify.sum.holds(archived.check);
        self.verifying.swap_remove(at);
        if holds {
            Ok(())
        } else {
            Err(DriverError::MediumError)
        }
    }

    /// The archive's decoded stream, where it is one.
    pub(crate) fn stream_mut(&mut self) -> Option<&mut crate::stream::Stream> {
        self.stream.as_mut()
    }

    /// Read up to `buf.len()` bytes at `offset` of `node`'s `archived` bytes,
    /// of content generation `content_gen`, answering how many.
    pub(crate) fn read<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        (node, content_gen): (u64, u64),
        archived: &Archived,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let want = usize::try_from(archived.len.saturating_sub(offset))
            .map_or(buf.len(), |left| left.min(buf.len()));
        if want == 0 {
            return Ok(0);
        }
        let buf = &mut buf[..want];
        if archived.lead != Lead::None {
            return Err(DriverError::DeviceFault);
        }
        match archived.coding {
            Coding::Refused { why, .. } => return Err(why),
            Coding::Stored => {
                let at = archived
                    .at
                    .checked_add(offset)
                    .ok_or(DriverError::MediumError)?;
                if regions.read(ARCHIVE_REGION, at, buf)? != want {
                    return Err(DriverError::MediumError);
                }
                self.verify((node, content_gen), archived, offset, buf)?;
                return Ok(want);
            }
            Coding::Rar { entry } => {
                let reader = self.rar.as_mut().ok_or(DriverError::DeviceFault)?;
                let entry = usize::try_from(entry).map_err(|_| DriverError::DeviceFault)?;
                return reader.read(regions, entry, offset, buf);
            }
            Coding::Stream => {
                let stream = self.stream.as_mut().ok_or(DriverError::DeviceFault)?;
                let at = archived
                    .at
                    .checked_add(offset)
                    .ok_or(DriverError::MediumError)?;
                return if stream.read(regions, at, buf)? == want {
                    Ok(want)
                } else {
                    Err(DriverError::MediumError)
                };
            }
            Coding::Deflate { .. } | Coding::Bzip2 { .. } | Coding::Arc { .. } => {}
        }
        self.clock += 1;
        let clock = self.clock;
        let at = self.cursor(regions, (node, content_gen), archived, offset)?;
        let cursor = &mut self.slots[at];
        cursor.used = clock;
        let mut skip = [0u8; SKIP_LEN];
        while cursor.produced < offset {
            let gap =
                usize::try_from(offset - cursor.produced).map_or(SKIP_LEN, |gap| gap.min(SKIP_LEN));
            let got = cursor.step(regions, archived, &mut skip[..gap])?;
            if got == 0 {
                return Err(DriverError::MediumError);
            }
        }
        let mut filled = 0;
        while filled < want {
            let got = cursor.step(regions, archived, &mut buf[filled..])?;
            if got == 0 {
                return Err(DriverError::MediumError);
            }
            filled += got;
        }
        Ok(filled)
    }

    /// The slot of a cursor of `node` at or before `offset`, made afresh at
    /// the member's start where none is.
    fn cursor<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        (node, content_gen): (u64, u64),
        archived: &Archived,
        offset: u64,
    ) -> Result<usize, DriverError> {
        if let Some(at) = self.slots.iter().position(|cursor| {
            cursor.node == node && cursor.content_gen == content_gen && cursor.produced <= offset
        }) {
            return Ok(at);
        }
        let (decoding, input) = match archived.coding {
            Coding::Deflate { packed } => (
                Decoding::Inflate(Inflate::new()?),
                Input::new(archived.at, packed)?,
            ),
            Coding::Bzip2 { packed } => {
                let mut input = Input::new(archived.at, packed)?;
                let decoding = Bunzip::new(regions, &mut input, archived.len)?;
                (Decoding::Bunzip(decoding), input)
            }
            Coding::Arc {
                packed,
                skip,
                method,
            } => {
                let skip = u64::from(skip);
                let stream = packed.checked_sub(skip).ok_or(DriverError::BadMagic)?;
                let at = archived.at.checked_add(skip).ok_or(DriverError::BadMagic)?;
                (arc_decoding(method)?, Input::new(at, stream)?)
            }
            Coding::Stored | Coding::Refused { .. } | Coding::Stream | Coding::Rar { .. } => {
                return Err(DriverError::Unsupported)
            }
        };
        let cursor = Cursor {
            node,
            content_gen,
            produced: 0,
            decoding,
            input,
            sum: Sum::of(archived.check),
            used: 0,
        };
        if self.slots.len() < CURSORS {
            self.slots
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.slots.push(cursor);
            return Ok(self.slots.len() - 1);
        }
        let oldest = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, cursor)| cursor.used)
            .map_or(0, |(at, _)| at);
        self.slots[oldest] = cursor;
        Ok(oldest)
    }
}

/// A decoder of an ARC-family member coded as `method`, with workspace for
/// its tables where it codes with some.
fn arc_decoding(method: arc::Method) -> Result<Decoding, DriverError> {
    let (table, stack) = match method {
        arc::Method::Packed => (0, 0),
        arc::Method::Squeezed => (arc::SQUEEZE_TABLE, 0),
        arc::Method::Crunched { .. } | arc::Method::Lzw { .. } => {
            (arc::DECODER_TABLE, arc::DECODER_STACK)
        }
    };
    let table = tairix_util::fallible::filled(table, 0u32).ok_or(DriverError::OutOfMemory)?;
    let stack = tairix_util::fallible::filled(stack, 0u8).ok_or(DriverError::OutOfMemory)?;
    let decoder = arc::Decoder::new(table, stack, method).map_err(|_| DriverError::BadMagic)?;
    Ok(Decoding::Arc(Heap::new(decoder)?))
}

impl Cursor {
    /// Decode the next bytes into `out`, answering how many; zero only past
    /// the member's end. The member's check is held to its bytes as the
    /// decoder ends, and the decoder must end exactly where the member does.
    fn step<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        archived: &Archived,
        out: &mut [u8],
    ) -> Result<usize, DriverError> {
        let left = archived.len - self.produced;
        if left == 0 {
            return Ok(0);
        }
        let room = usize::try_from(left).map_or(out.len(), |left| left.min(out.len()));
        let got = match &mut self.decoding {
            Decoding::Inflate(inflate) => {
                inflate.decode(regions, &mut self.input, &mut out[..room])?
            }
            Decoding::Bunzip(bunzip) => {
                bunzip.decode(regions, &mut self.input, &mut out[..room])?
            }
            Decoding::Arc(decoder) => {
                let mut bytes = ByteInput::new(&mut self.input, regions);
                decoder
                    .get()?
                    .read(&mut bytes, &mut out[..room])
                    .map_err(|_| bytes.fault_or(DriverError::MediumError))?
            }
        };
        if got == 0 {
            return Err(DriverError::MediumError);
        }
        self.sum.update(&out[..got]);
        self.produced += got as u64;
        if self.produced == archived.len && !self.sum.holds(archived.check) {
            return Err(DriverError::MediumError);
        }
        Ok(got)
    }
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;

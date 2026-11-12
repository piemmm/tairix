//! An archive that is one compressed stream — a gzip'd or bzip2'd tar, or a
//! lone compressed file (`plans/ARCHIVES.md` AR4): its decoded bytes, read by
//! offset, and the stream of the next archive, written.
//!
//! The opening pass decodes the stream once from its start, noting where a
//! reader can resume: every bzip2 block, which decodes alone, and for gzip
//! every member and, every so many bytes, a block start with its window, as
//! zlib's `zran` does. A read decodes from the nearest place before it, so a
//! member deep in a large archive costs a stretch of decoding, never the
//! whole stream. The places are bounded in number, the stretch between them
//! widening as the stream grows. A writer notes its own places as it
//! compresses — a gzip place is a sync flush — so the archive it writes is
//! read the same way with no second pass.
//!
//! A gzip member decoded from its start is held to its CRC and length, as
//! the opening pass does every member; a bzip2 block is held to its CRC every
//! time it is decoded.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_compress::bzip2;
use tairix_compress::deflate::{Deflate, Flush};
use tairix_compress::gzip::{self, Header as GzipHeader, HEADER_MAX as GZIP_HEADER_MAX};
use tairix_compress::inflate::{Checkpoint, WINDOW_SIZE};
use tairix_fsserve::VolumeAccess;

use crate::data::Heap;
use crate::format::{Content, Out};
use crate::region::{ByteInput, Input, Regions, Sink, SinkOutput};

/// The most gzip places kept: each holds a window, so a few megabytes
/// whatever the stream's size.
const GZIP_POINTS: usize = 128;

/// The most bzip2 places kept: a block's place is a few words.
const BZIP2_POINTS: usize = 4096;

/// The decoded stretch between gzip places a stream starts with.
const GZIP_SPAN: u64 = 1 << 22;

/// How much plaintext a writer compresses at once.
const CHUNK: usize = 64 * 1024;

/// How the archive's one stream is compressed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Compression {
    /// gzip (RFC 1952), one member or several.
    Gzip,
    /// bzip2, one stream or several.
    Bzip2,
}

impl Compression {
    const fn max_points(self) -> usize {
        match self {
            Self::Gzip => GZIP_POINTS,
            Self::Bzip2 => BZIP2_POINTS,
        }
    }

    /// The stretch between places a stream starts with: none for bzip2,
    /// whose every block is one.
    const fn first_span(self) -> u64 {
        match self {
            Self::Gzip => GZIP_SPAN,
            Self::Bzip2 => 0,
        }
    }
}

/// What a stream is written as: its compression, and what its header keeps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Packing {
    /// gzip, its header naming `name` — ISO 8859-1, holding no NUL — and
    /// stamped `mtime`, seconds since the epoch or zero for none.
    Gzip { name: Option<Vec<u8>>, mtime: u32 },
    /// bzip2, in blocks of `level` hundred thousand bytes.
    Bzip2 { level: u8 },
}

impl Packing {
    /// How a stream it writes is compressed.
    pub(crate) const fn compression(&self) -> Compression {
        match self {
            Self::Gzip { .. } => Compression::Gzip,
            Self::Bzip2 { .. } => Compression::Bzip2,
        }
    }

    /// How the stream `regions` holds was written, read from its first
    /// header.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for an archive that opens no such stream;
    /// the kernel's refusal; [`DriverError::OutOfMemory`].
    pub(crate) fn of<A: VolumeAccess>(
        compression: Compression,
        regions: &Regions<A>,
    ) -> Result<Self, DriverError> {
        match compression {
            Compression::Gzip => {
                let (header, head) = member_header(regions, 0)?.ok_or(DriverError::BadMagic)?;
                let name = match header.name {
                    Some(range) => {
                        let field = head.get(range).ok_or(DriverError::BadMagic)?;
                        let mut name = Vec::new();
                        name.try_reserve_exact(field.len())
                            .map_err(|_| DriverError::OutOfMemory)?;
                        name.extend_from_slice(field);
                        Some(name)
                    }
                    None => None,
                };
                Ok(Self::Gzip {
                    name,
                    mtime: header.mtime,
                })
            }
            Compression::Bzip2 => match regions.archive_bytes(0, 4)?.as_slice() {
                [b'B', b'Z', b'h', digit @ b'1'..=b'9'] => Ok(Self::Bzip2 {
                    level: digit - b'0',
                }),
                _ => Err(DriverError::BadMagic),
            },
        }
    }
}

/// A place a reader resumes the decoded stream from.
enum Point {
    /// A gzip member's or a bzip2 stream's header, at `at` of the archive.
    Head { decoded: u64, at: u64 },
    /// A block start inside a gzip member, its deflate resuming at `at` from
    /// `checkpoint`.
    Inside {
        decoded: u64,
        at: u64,
        checkpoint: Heap<Checkpoint>,
    },
    /// A bzip2 block, its magic at bit `bit` of the archive, in a stream of
    /// `level`.
    Block { decoded: u64, bit: u64, level: u8 },
}

impl Point {
    const fn decoded(&self) -> u64 {
        match self {
            Self::Head { decoded, .. }
            | Self::Inside { decoded, .. }
            | Self::Block { decoded, .. } => *decoded,
        }
    }
}

/// Where a gzip reader stands in its member.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    /// Before a member's header, or past the last.
    Header,
    /// In a member's deflate stream.
    Body,
}

/// A gzip reader's decoding: the member's body, held to its trailer where
/// it was decoded from its start.
struct Gzip {
    member: Heap<gzip::Member>,
    stage: Stage,
}

/// A bzip2 reader's decoding.
struct Bzip2 {
    decoder: Heap<bzip2::Decoder<Vec<u32>>>,
    /// The archive bit the decoder's own count is counted from.
    origin_bit: u64,
    /// The stream's level, zero before its header is read.
    level: u8,
    /// The decoder stands inside a block.
    in_block: bool,
}

enum Decoding {
    Gzip(Gzip),
    Bzip2(Bzip2),
}

/// A reader of the decoded stream, where it stands.
struct Cursor {
    decoded: u64,
    input: Input,
    decoding: Decoding,
    ended: bool,
}

/// The index and the reader of an archive's one compressed stream.
pub(crate) struct Stream {
    compression: Compression,
    points: Vec<Point>,
    span: u64,
    /// The decoded stream's length, once the opening pass has found it.
    len: Option<u64>,
    cursor: Option<Cursor>,
}

impl Stream {
    /// The stream of an archive compressed as `compression` says, to be read
    /// from its start: the opening pass, noting its places as it goes.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    pub(crate) fn new(compression: Compression) -> Result<Self, DriverError> {
        let mut points = Vec::new();
        points
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        points.push(Point::Head { decoded: 0, at: 0 });
        Ok(Self {
            compression,
            points,
            span: compression.first_span(),
            len: None,
            cursor: None,
        })
    }

    /// Note a place: in the order a reader passes them, no nearer than the
    /// stretch to the last but where a header lies, and held to the most a
    /// stream keeps by letting every other place go, the stretch growing to
    /// what then lies between them.
    fn push(&mut self, point: Point) -> Result<(), DriverError> {
        let Some(last) = self.points.last() else {
            return Err(DriverError::DeviceFault);
        };
        let gap = point.decoded().saturating_sub(last.decoded());
        if gap == 0 || (gap < self.span && !matches!(point, Point::Head { .. })) {
            return Ok(());
        }
        if self.points.len() >= self.compression.max_points() {
            let mut keep = false;
            self.points.retain(|_| {
                keep = !keep;
                keep
            });
            let reach = self.points.last().map_or(0, Point::decoded);
            let between = reach / self.points.len().max(1) as u64;
            self.span = self.span.saturating_mul(2).max(between);
        }
        self.points
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.points.push(point);
        Ok(())
    }

    /// Read up to `buf.len()` bytes at `at` of the decoded stream, answering
    /// how many: fewer only at its end.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a stream that does not decode or a
    /// member or block whose check fails; [`DriverError::BadMagic`] for bytes
    /// that open no member or stream where one must start; the kernel's
    /// refusal; [`DriverError::OutOfMemory`].
    pub(crate) fn read<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        at: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let nearest = self.points.partition_point(|point| point.decoded() <= at);
        let point = nearest
            .checked_sub(1)
            .and_then(|index| self.points.get(index))
            .ok_or(DriverError::DeviceFault)?;
        // The opening pass keeps its one cursor whatever it skips, so every
        // member is checked and every place noted; a later read keeps it
        // while it stands no further back than the nearest place.
        let noting = self.len.is_none();
        let reuse = self.cursor.as_ref().is_some_and(|cursor| {
            cursor.decoded <= at && (noting || cursor.decoded >= point.decoded())
        });
        if !reuse {
            self.cursor = Some(resume(self.compression, regions, point)?);
        }
        let mut skip = [0u8; 16 * 1024];
        while let Some(cursor) = self.cursor.as_ref() {
            if cursor.decoded >= at || cursor.ended {
                break;
            }
            let gap =
                usize::try_from(at - cursor.decoded).map_or(skip.len(), |gap| gap.min(skip.len()));
            if self.step(regions, &mut skip[..gap])? == 0 {
                break;
            }
        }
        let mut filled = 0;
        while filled < buf.len() {
            let got = self.step(regions, &mut buf[filled..])?;
            if got == 0 {
                break;
            }
            filled += got;
        }
        Ok(filled)
    }

    /// Decode the next bytes into `out`, answering how many; zero at the
    /// stream's end. The opening pass notes the places it passes.
    fn step<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        out: &mut [u8],
    ) -> Result<usize, DriverError> {
        let noting = self.len.is_none();
        let last_noted = self.points.last().map_or(0, Point::decoded);
        let wants_point = noting && self.compression == Compression::Gzip;
        let span = self.span;
        let Some(cursor) = self.cursor.as_mut() else {
            return Err(DriverError::DeviceFault);
        };
        if cursor.ended || out.is_empty() {
            return Ok(0);
        }
        let mut noted = None;
        let decoded = cursor.decoded;
        let got = match &mut cursor.decoding {
            Decoding::Gzip(gzip) => {
                let wants_point = wants_point && decoded >= last_noted.saturating_add(span);
                gzip_step(
                    gzip,
                    &mut cursor.input,
                    regions,
                    out,
                    wants_point,
                    decoded,
                    &mut noted,
                )?
            }
            Decoding::Bzip2(bz) => {
                bzip2_step(bz, &mut cursor.input, regions, out, decoded, &mut noted)?
            }
        };
        cursor.decoded += got as u64;
        if got == 0 {
            cursor.ended = true;
            if noting {
                self.len = Some(cursor.decoded);
            }
        }
        if let (true, Some(point)) = (noting, noted) {
            self.push(point)?;
        }
        Ok(got)
    }

    /// Decode the rest of the stream to its end, noting its places and its
    /// length, every gzip member checked: the end of the opening pass.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub(crate) fn measure<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
    ) -> Result<u64, DriverError> {
        if let Some(len) = self.len {
            return Ok(len);
        }
        if self.cursor.is_none() {
            self.read(regions, 0, &mut [])?;
        }
        let mut buf = tairix_util::fallible::filled(CHUNK, 0u8).ok_or(DriverError::OutOfMemory)?;
        loop {
            if self.step(regions, &mut buf)? == 0 {
                return self.len.ok_or(DriverError::DeviceFault);
            }
        }
    }
}

/// A reader standing at `point` of a stream compressed as `compression`.
fn resume<A: VolumeAccess>(
    compression: Compression,
    regions: &Regions<A>,
    point: &Point,
) -> Result<Cursor, DriverError> {
    let (decoding, at) = match (compression, point) {
        (Compression::Gzip, Point::Head { at, .. }) => (
            Decoding::Gzip(Gzip {
                member: Heap::new(gzip::Member::new())?,
                stage: Stage::Header,
            }),
            *at,
        ),
        (Compression::Gzip, Point::Inside { at, checkpoint, .. }) => {
            let mut member = Heap::new(gzip::Member::new())?;
            member.get()?.resume(checkpoint.get_ref()?);
            let gzip = Gzip {
                member,
                stage: Stage::Body,
            };
            (Decoding::Gzip(gzip), *at)
        }
        (Compression::Bzip2, Point::Head { at, .. }) => (
            Decoding::Bzip2(Bzip2 {
                decoder: bzip2_decoder()?,
                origin_bit: at.checked_mul(8).ok_or(DriverError::DeviceFault)?,
                level: 0,
                in_block: false,
            }),
            *at,
        ),
        (Compression::Bzip2, Point::Block { bit, level, .. }) => {
            let mut decoder = bzip2_decoder()?;
            let skip = u8::try_from(bit % 8).map_err(|_| DriverError::DeviceFault)?;
            decoder
                .get()?
                .resume(*level, skip)
                .map_err(|_| DriverError::DeviceFault)?;
            // A resumed decoder counts its bits from the block's magic.
            let bz = Bzip2 {
                decoder,
                origin_bit: *bit,
                level: *level,
                in_block: false,
            };
            (Decoding::Bzip2(bz), bit / 8)
        }
        _ => return Err(DriverError::DeviceFault),
    };
    Ok(Cursor {
        decoded: point.decoded(),
        input: Input::new(at, regions.len().saturating_sub(at))?,
        decoding,
        ended: false,
    })
}

/// A bzip2 decoder with workspace for the largest block.
fn bzip2_decoder() -> Result<Heap<bzip2::Decoder<Vec<u32>>>, DriverError> {
    let work =
        tairix_util::fallible::filled(bzip2::MAX_BLOCK, 0u32).ok_or(DriverError::OutOfMemory)?;
    Heap::new(bzip2::Decoder::new(work))
}

/// The gzip member header at `at` and the bytes it was read from, or `None`
/// where the archive holds only zeros from there to its end — padding a
/// writer left — a few hundred bytes read first, the whole bound only for a
/// header that runs past them.
fn member_header<A: VolumeAccess>(
    regions: &Regions<A>,
    at: u64,
) -> Result<Option<(GzipHeader, Vec<u8>)>, DriverError> {
    const FIRST: usize = 512;
    let left = regions.len().saturating_sub(at);
    let first = usize::try_from(left).map_or(FIRST, |left| left.min(FIRST));
    let head = regions.archive_bytes(at, first)?;
    if let Some(header) = GzipHeader::parse(&head) {
        return Ok(Some((header, head)));
    }
    if zeros_to_end(regions, at, &head)? {
        return Ok(None);
    }
    let whole = usize::try_from(left).map_or(GZIP_HEADER_MAX, |left| left.min(GZIP_HEADER_MAX));
    if whole > first {
        let head = regions.archive_bytes(at, whole)?;
        if let Some(header) = GzipHeader::parse(&head) {
            return Ok(Some((header, head)));
        }
    }
    Err(DriverError::BadMagic)
}

/// Whether the archive holds only zeros from `at` to its end, `head` being
/// the bytes it holds first there.
fn zeros_to_end<A: VolumeAccess>(
    regions: &Regions<A>,
    at: u64,
    head: &[u8],
) -> Result<bool, DriverError> {
    if head.iter().any(|&byte| byte != 0) {
        return Ok(false);
    }
    let mut next = at + head.len() as u64;
    let mut buf = [0u8; 4096];
    while next < regions.len() {
        let want =
            usize::try_from(regions.len() - next).map_or(buf.len(), |left| left.min(buf.len()));
        regions.read_archive(next, &mut buf[..want])?;
        if buf[..want].iter().any(|&byte| byte != 0) {
            return Ok(false);
        }
        next += want as u64;
    }
    Ok(true)
}

/// One step of a gzip reader: through headers and trailers as they come,
/// decoding into `out`; a checkpoint is taken at the first block start it
/// passes once `wants_point`.
fn gzip_step<A: VolumeAccess>(
    gzip: &mut Gzip,
    input: &mut Input,
    regions: &Regions<A>,
    out: &mut [u8],
    wants_point: bool,
    decoded: u64,
    noted: &mut Option<Point>,
) -> Result<usize, DriverError> {
    loop {
        if gzip.stage == Stage::Header {
            let at = input.position();
            if at >= regions.len() {
                return Ok(0);
            }
            let Some((header, _)) = member_header(regions, at)? else {
                input.seek(regions.len());
                return Ok(0);
            };
            input.seek(at + header.len as u64);
            gzip.member.get()?.begin();
            gzip.stage = Stage::Body;
            *noted = Some(Point::Head { decoded, at });
            continue;
        }
        let member = gzip.member.get()?;
        if member.is_finished() {
            let mut trailer = [0u8; gzip::TRAILER_LEN];
            let mut got = 0;
            while got < trailer.len() {
                let src = input.fill(regions)?;
                if src.is_empty() {
                    return Err(DriverError::MediumError);
                }
                let take = src.len().min(trailer.len() - got);
                trailer[got..got + take].copy_from_slice(&src[..take]);
                input.consume(take);
                got += take;
            }
            if !member.verify(&trailer) {
                return Err(DriverError::MediumError);
            }
            gzip.stage = Stage::Header;
            continue;
        }
        if wants_point && noted.is_none() && member.at_block_start() {
            let mut checkpoint = Heap::new(Checkpoint::default())?;
            if member.checkpoint(checkpoint.get()?) {
                *noted = Some(Point::Inside {
                    decoded,
                    at: input.position(),
                    checkpoint,
                });
            }
        }
        let src = input.fill(regions)?;
        if src.is_empty() {
            return Err(DriverError::MediumError);
        }
        let progress = if wants_point {
            member.inflate_by_block(src, out)
        } else {
            member.inflate(src, out)
        }
        .map_err(|_| DriverError::MediumError)?;
        input.consume(progress.consumed);
        if progress.produced > 0 {
            return Ok(progress.produced);
        }
    }
}

/// One step of a bzip2 reader: through stream headers, blocks and end
/// markers as they come, decoding into `out`, each block start noted.
fn bzip2_step<A: VolumeAccess>(
    bz: &mut Bzip2,
    input: &mut Input,
    regions: &Regions<A>,
    out: &mut [u8],
    decoded: u64,
    noted: &mut Option<Point>,
) -> Result<usize, DriverError> {
    let coder = bz.decoder.get()?;
    loop {
        if bz.in_block {
            let got = coder.read(out).map_err(|_| DriverError::MediumError)?;
            if got > 0 {
                return Ok(got);
            }
            bz.in_block = false;
        }
        if bz.level == 0 || coder.ended() {
            // Another stream may follow, as a parallel compressor writes
            // them; zeros to the archive's end are padding.
            let at = input.position();
            let rest = input.fill(regions)?;
            if rest.is_empty() || zeros_to_end(regions, at, rest)? {
                input.seek(regions.len());
                return Ok(0);
            }
            let mut bytes = ByteInput::new(&mut *input, regions);
            bz.level = coder
                .begin(&mut bytes)
                .map_err(|_| bytes.fault_or(DriverError::BadMagic))?;
        }
        let mut bytes = ByteInput::new(&mut *input, regions);
        match coder.next_block(&mut bytes) {
            Ok(bzip2::Next::Block { at }) => {
                *noted = Some(Point::Block {
                    decoded,
                    bit: bz.origin_bit + at,
                    level: bz.level,
                });
                bz.in_block = true;
            }
            Ok(bzip2::Next::End) => {}
            Err(_) => return Err(bytes.fault_or(DriverError::MediumError)),
        }
    }
}

/// The next archive's one stream, compressed as a writer puts its
/// plaintext, its places noted as it goes for the reader of the archive it
/// becomes.
pub(crate) struct Writer<'s, 'r, A> {
    sink: &'s mut Sink<'r, A>,
    index: Stream,
    plain: u64,
    coder: Coder,
    /// What a copy is read through.
    scratch: Vec<u8>,
}

enum Coder {
    Gzip(GzipCoder),
    Bzip2(Bzip2Coder),
}

/// A gzip writer's compressing.
struct GzipCoder {
    deflate: Heap<Deflate>,
    crc: tairix_crc32::Crc32,
    /// The plaintext's last window, compressed already, then what is not
    /// yet: the history a place's checkpoint holds.
    text: Vec<u8>,
    held: usize,
    out: Vec<u8>,
}

/// A bzip2 writer's compressing.
struct Bzip2Coder {
    encoder: Heap<bzip2::Encoder<Vec<u32>, Vec<u8>>>,
    level: u8,
}

impl<'s, 'r, A: VolumeAccess> Writer<'s, 'r, A> {
    /// The next archive's stream, written as `packing` says into `sink`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`]; the kernel's refusal to write the
    /// header.
    pub(crate) fn new(packing: &Packing, sink: &'s mut Sink<'r, A>) -> Result<Self, DriverError> {
        let coder = match packing {
            Packing::Gzip { name, mtime } => {
                const FNAME: u8 = 0x08;
                // No extra field or comment; the method deflate, the system
                // unknown.
                let mut header = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
                header[4..8].copy_from_slice(&mtime.to_le_bytes());
                if let Some(name) = name {
                    if name.is_empty() || name.contains(&0) {
                        return Err(DriverError::OutOfRange);
                    }
                    header[3] = FNAME;
                }
                sink.put(&header)?;
                if let Some(name) = name {
                    sink.put(name)?;
                    sink.put(&[0])?;
                }
                let text_room = WINDOW_SIZE + CHUNK;
                let mut text = Vec::new();
                text.try_reserve_exact(text_room)
                    .map_err(|_| DriverError::OutOfMemory)?;
                Coder::Gzip(GzipCoder {
                    deflate: Heap::new(Deflate::new())?,
                    crc: tairix_crc32::Crc32::new(),
                    text,
                    held: 0,
                    out: Vec::new(),
                })
            }
            Packing::Bzip2 { level } => {
                let work = tairix_util::fallible::filled(bzip2::encoder_work(*level), 0u32)
                    .ok_or(DriverError::OutOfMemory)?;
                let block = tairix_util::fallible::filled(bzip2::encoder_block(*level), 0u8)
                    .ok_or(DriverError::OutOfMemory)?;
                let encoder = bzip2::Encoder::new(work, block, *level)
                    .map_err(|_| DriverError::OutOfRange)?;
                Coder::Bzip2(Bzip2Coder {
                    encoder: Heap::new(encoder)?,
                    level: *level,
                })
            }
        };
        Ok(Self {
            sink,
            index: Stream::new(packing.compression())?,
            plain: 0,
            coder,
            scratch: tairix_util::fallible::filled(CHUNK, 0u8).ok_or(DriverError::OutOfMemory)?,
        })
    }

    /// Compress `bytes` onto the stream.
    fn write(&mut self, mut bytes: &[u8]) -> Result<(), DriverError> {
        match &mut self.coder {
            Coder::Gzip(gzip) => {
                while !bytes.is_empty() {
                    let room = CHUNK - (gzip.text.len() - gzip.held);
                    let (now, later) = bytes.split_at(room.min(bytes.len()));
                    gzip.text.extend_from_slice(now);
                    gzip.crc.update(now);
                    self.plain += now.len() as u64;
                    bytes = later;
                    if gzip.text.len() - gzip.held == CHUNK {
                        gzip.compress(self.sink, Flush::None)?;
                    }
                }
                let last = self.index.points.last().map_or(0, Point::decoded);
                if self.plain >= last.saturating_add(self.index.span) {
                    // A sync flush leaves the stream byte-aligned at a block's
                    // start, which a reader resumes from with the window.
                    gzip.compress(self.sink, Flush::Sync)?;
                    let mut checkpoint = Heap::new(Checkpoint::default())?;
                    checkpoint.get()?.after_sync(&gzip.text);
                    self.index.push(Point::Inside {
                        decoded: self.plain,
                        at: self.sink.position(),
                        checkpoint,
                    })?;
                }
                Ok(())
            }
            Coder::Bzip2(bz) => {
                let mut out = SinkOutput::new(&mut *self.sink);
                let mut places = Places {
                    index: &mut self.index,
                    level: bz.level,
                    fault: None,
                };
                let wrote = bz.encoder.get()?.write(bytes, &mut out, &mut places);
                settle(wrote, out.fault(), places.fault)?;
                self.plain += bytes.len() as u64;
                Ok(())
            }
        }
    }

    /// End the stream, answering the index the archive it ends is read
    /// through.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to write the stream's end.
    pub(crate) fn finish(mut self) -> Result<Stream, DriverError> {
        match &mut self.coder {
            Coder::Gzip(gzip) => {
                gzip.compress(self.sink, Flush::Finish)?;
                let sum = core::mem::take(&mut gzip.crc).finish();
                self.sink.put(&sum.to_le_bytes())?;
                // The length modulo 2^32.
                self.sink.put(&self.plain.to_le_bytes()[..4])?;
            }
            Coder::Bzip2(bz) => {
                let mut out = SinkOutput::new(&mut *self.sink);
                let mut places = Places {
                    index: &mut self.index,
                    level: bz.level,
                    fault: None,
                };
                let finished = bz.encoder.get()?.finish(&mut out, &mut places);
                settle(finished, out.fault(), places.fault)?;
            }
        }
        self.index.len = Some(self.plain);
        Ok(self.index)
    }
}

impl GzipCoder {
    /// Compress what is held and not yet compressed, then keep only the
    /// plaintext's last window.
    fn compress<A: VolumeAccess>(
        &mut self,
        sink: &mut Sink<'_, A>,
        flush: Flush,
    ) -> Result<(), DriverError> {
        let deflate = self.deflate.get()?;
        let pending = &self.text[self.held..];
        let need = deflate.bound(pending.len());
        if self.out.len() < need {
            self.out
                .try_reserve(need - self.out.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            self.out.resize(need, 0);
        }
        let made = deflate
            .deflate(pending, &mut self.out[..need], flush)
            .map_err(|_| DriverError::DeviceFault)?;
        sink.put(&self.out[..made])?;
        let keep = self.text.len().min(WINDOW_SIZE);
        let from = self.text.len() - keep;
        self.text.copy_within(from.., 0);
        self.text.truncate(keep);
        self.held = keep;
        Ok(())
    }
}

/// A bzip2 encoder's blocks, noted as a stream's places.
struct Places<'i> {
    index: &'i mut Stream,
    level: u8,
    fault: Option<DriverError>,
}

impl bzip2::BlockIndex for Places<'_> {
    fn block(&mut self, plain: u64, bit: u64) {
        let point = Point::Block {
            decoded: plain,
            bit,
            level: self.level,
        };
        if let Err(err) = self.index.push(point) {
            self.fault.get_or_insert(err);
        }
    }
}

/// The outcome of a bzip2 encoder's call, a refusal its output or index met
/// standing first.
fn settle(
    outcome: Result<(), bzip2::Error>,
    output: Option<DriverError>,
    index: Option<DriverError>,
) -> Result<(), DriverError> {
    if let Some(err) = output.or(index) {
        return Err(err);
    }
    outcome.map_err(|_| DriverError::DeviceFault)
}

impl<A: VolumeAccess> Out<A> for Writer<'_, '_, A> {
    fn position(&self) -> u64 {
        self.plain
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), DriverError> {
        self.write(bytes)
    }

    fn copy(
        &mut self,
        content: &mut Content<'_, A>,
        region: u8,
        at: u64,
        len: u64,
    ) -> Result<(), DriverError> {
        let mut scratch = core::mem::take(&mut self.scratch);
        let copied = (|| {
            let mut done = 0;
            while done < len {
                let want = usize::try_from(len - done)
                    .map_or(scratch.len(), |left| left.min(scratch.len()));
                let from = at.checked_add(done).ok_or(DriverError::LengthOutOfRange)?;
                if content.read_region(region, from, &mut scratch[..want])? != want {
                    return Err(DriverError::MediumError);
                }
                self.write(&scratch[..want])?;
                done += want as u64;
            }
            Ok(())
        })();
        self.scratch = scratch;
        copied
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;

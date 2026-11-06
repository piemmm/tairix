//! A VP8 stream decoded a frame at a time (RFC 6386 §9.7, §9.8): keyframes
//! and inter frames into pictures, with the last, golden and alternate frames
//! inter frames predict from.
//!
//! Pictures are held in a pool, and a reference names one by its slot, so a
//! refresh or a copy moves a slot and never a plane, and a stream holds at
//! most one picture more than its three references. A frame decodes into a
//! picture no reference holds and becomes the stream's — its references,
//! segment map and carried probabilities with it — only once it has decoded
//! whole, so a refused frame leaves the stream as it was.
//!
//! A frame is reconstructed a macroblock row at a time against the unfiltered
//! rows above it, and the row above is filtered once it is: the loop filter
//! acting on the frame as a whole, so a reference holds its frame filtered.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::fallible;

use super::inter::{read_row, MbInfo, Prediction};
use super::motion::{predict_inter, Interpolation, RefPlane};
use super::{
    dimensions, filter_level, filter_row, read_header, segment_factors, split_partitions, Bool,
    Buffer, BufferUpdate, Dequant, Edges, Header, InterHeader, IntraContexts, Macroblock, Planes,
    ResidualCoding, Rgba8, StreamState, Tag, Tokens, Vp8Error, BLOCK_COEFFS, COLUMN_CONTEXT_BYTES,
    MAX_DIMENSION, MAX_PARTITIONS, MB_BLOCKS, SEGMENTS, TAG_BYTES, UNCOMPRESSED_HEADER,
};

/// Pictures a stream holds at most: one for each reference, and one a frame
/// decodes into while the references keep theirs.
const POOL: usize = 4;

/// A VP8 stream's decoder: it decodes each frame against what the frames
/// before it left, holding the pictures inter frames predict from.
///
/// A stream opens with a keyframe, which sets its size; a later keyframe may
/// change it. [`stream_peak_bytes`] of the size is all the decoder holds, and
/// a keyframe changing the size holds no more than the new size's beyond
/// what the stream held, until it decodes whole. The decoder takes no limits:
/// a caller bounds a stream by [probing](super::probe) each keyframe's size
/// before it is decoded.
pub struct Decoder {
    /// What a stream of the current size carries, `None` until a keyframe
    /// decodes.
    stream: Option<Stream>,
}

impl Decoder {
    /// A decoder no frame has reached yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { stream: None }
    }

    /// Decode `frame`, the stream's next, answering the picture it made.
    ///
    /// # Errors
    ///
    /// [`Vp8Error::Interframe`] for an inter frame before any keyframe, and
    /// otherwise why the frame is refused. A refused frame leaves the stream
    /// as it was, so the next decodes as though the refused one never came.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Decoded<'_>, Vp8Error> {
        let tag = Tag::read(frame)?;
        let interpolation =
            Interpolation::of_version(tag.version).ok_or(Vp8Error::UnsupportedProfile)?;
        let geometry = if tag.keyframe {
            let (width, height) = dimensions(frame)?;
            Geometry::new(width, height)
        } else {
            self.stream
                .as_ref()
                .ok_or(Vp8Error::Interframe)?
                .walk
                .geometry
        };
        let resumes = self
            .stream
            .as_ref()
            .is_some_and(|stream| stream.walk.geometry == geometry);
        if !resumes {
            return self.restart(frame, (tag, interpolation), geometry);
        }
        match &mut self.stream {
            Some(stream) => stream.decode(frame, (tag, interpolation)),
            None => Err(Vp8Error::Interframe),
        }
    }

    /// Open the stream afresh at `geometry` with the keyframe `frame`,
    /// keeping the stream it replaces whole until the keyframe decodes.
    fn restart(
        &mut self,
        frame: &[u8],
        (tag, interpolation): (Tag, Interpolation),
        geometry: Geometry,
    ) -> Result<Decoded<'_>, Vp8Error> {
        if let Some(replaced) = &mut self.stream {
            replaced.release_free();
        }
        let mut state = StreamState::new();
        let mut parsed = Parsed::read(frame, (tag, interpolation), geometry, &mut state)?;
        let mut walk = Walk::new(geometry)?;
        let mut picture = geometry.picture()?;
        walk.reconstruct(&mut parsed, None, &mut picture)?;
        let stream = self.stream.insert(Stream {
            state: StreamState::new(),
            pool: [const { None }; POOL],
            refs: References::all(0),
            walk,
        });
        Ok(stream.commit(0, picture, state, &parsed.header, tag))
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// A frame [`Decoder::decode`] decoded, lending the picture it made until the
/// decoder is next asked for a frame.
pub struct Decoded<'a> {
    pub(super) picture: &'a Planes,
    width: u32,
    height: u32,
    shown: bool,
    keyframe: bool,
}

impl Decoded<'_> {
    /// Whether the stream shows the frame: one it does not is decoded only
    /// for later frames to predict from.
    #[must_use]
    pub const fn shown(&self) -> bool {
        self.shown
    }

    /// Whether the frame is a keyframe.
    #[must_use]
    pub const fn keyframe(&self) -> bool {
        self.keyframe
    }

    /// The picture's width, as the stream's keyframe declares it.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The picture's height, as the stream's keyframe declares it.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Convert the picture's rows `rows` into opaque RGBA8 in `out`, the
    /// first at its start and each `stride` bytes after the one above.
    ///
    /// # Errors
    ///
    /// [`Vp8Error::InvalidGeometry`] for rows the picture does not have, a
    /// stride narrower than a row, or an `out` too short for the rows asked
    /// for; nothing is written then.
    pub fn rgba_rows(
        &self,
        rows: Range<usize>,
        out: &mut [u8],
        stride: usize,
    ) -> Result<(), Vp8Error> {
        let width = usize::try_from(self.width).map_err(|_| Vp8Error::DimensionsOverflow)?;
        let height = usize::try_from(self.height).map_err(|_| Vp8Error::DimensionsOverflow)?;
        let line = width
            .checked_mul(size_of::<Rgba8>())
            .ok_or(Vp8Error::DimensionsOverflow)?;
        let needed = match rows.len() {
            0 => Some(0),
            count => (count - 1)
                .checked_mul(stride)
                .and_then(|start| start.checked_add(line)),
        };
        let fits = needed.is_some_and(|needed| needed <= out.len());
        if rows.start > rows.end || rows.end > height || stride < line.max(1) || !fits {
            return Err(Vp8Error::InvalidGeometry);
        }
        for (y, bytes) in rows.zip(out.chunks_mut(stride)) {
            let pixels = bytes.get_mut(..line).unwrap_or_default();
            self.picture
                .rgba_row(y, y / 2, pixels.as_chunks_mut::<4>().0);
        }
        Ok(())
    }
}

/// Every byte a [`Decoder`] of a `width`×`height` stream holds at its peak: a
/// picture for each reference and one being decoded into, the contexts of two
/// macroblock rows and the segment maps, and the readers of a frame's
/// partitions; `None` for a size no keyframe can declare.
#[must_use]
pub fn stream_peak_bytes(width: u32, height: u32) -> Option<u64> {
    let geometry = Geometry::declared(width, height)?;
    let (luma_width, luma_height) = geometry.luma();
    let pictures = Planes::len(luma_width, luma_height)?.checked_mul(POOL)?;
    let per_column = COLUMN_CONTEXT_BYTES + 2 * size_of::<Edges>() + 2 * size_of::<MbInfo>();
    let maps = geometry
        .columns
        .checked_mul(geometry.rows)?
        .checked_mul(2)?;
    let walk = geometry
        .columns
        .checked_mul(per_column)?
        .checked_add(maps)?;
    let partitions = MAX_PARTITIONS * size_of::<Bool<'static>>();
    let total = pictures.checked_add(walk)?.checked_add(partitions)?;
    u64::try_from(total).ok()
}

/// A stream's size: its pictures' declared size, and the whole macroblocks
/// they are decoded at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Geometry {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) columns: usize,
    pub(super) rows: usize,
}

impl Geometry {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            columns: usize::try_from(width.div_ceil(16)).unwrap_or(0),
            rows: usize::try_from(height.div_ceil(16)).unwrap_or(0),
        }
    }

    /// The size of a `width`×`height` stream, or `None` for one no keyframe
    /// can declare.
    pub(super) fn declared(width: u32, height: u32) -> Option<Self> {
        let declared = 1..=MAX_DIMENSION;
        let held = declared.contains(&width) && declared.contains(&height);
        held.then(|| Self::new(width, height))
    }

    /// The luma samples across and down its whole macroblocks.
    pub(super) fn luma(self) -> (usize, usize) {
        (self.columns * 16, self.rows * 16)
    }

    /// A picture of this size, bordered as intra prediction reads it.
    pub(super) fn picture(self) -> Result<Planes, Vp8Error> {
        let (width, height) = self.luma();
        Planes::new(width, height)
    }
}

/// Everything a stream of one size carries from frame to frame.
struct Stream {
    /// What its headers carry.
    state: StreamState,
    /// Its pictures, by slot.
    pool: [Option<Planes>; POOL],
    /// The slots of the pictures its inter frames predict from.
    refs: References,
    walk: Walk,
}

impl Stream {
    /// Decode `frame`, of this stream's size, into a picture no reference
    /// holds, and make it the stream's once it decodes whole.
    fn decode(
        &mut self,
        frame: &[u8],
        (tag, interpolation): (Tag, Interpolation),
    ) -> Result<Decoded<'_>, Vp8Error> {
        let mut state = self.state.clone();
        let geometry = self.walk.geometry;
        let mut parsed = Parsed::read(frame, (tag, interpolation), geometry, &mut state)?;
        let slot = self.refs.free();
        let mut picture = match self.pool[slot].take() {
            Some(picture) => picture,
            None => geometry.picture()?,
        };
        let decoded = if tag.keyframe {
            self.walk.reconstruct(&mut parsed, None, &mut picture)
        } else {
            views(&self.pool, self.refs, geometry).and_then(|views| {
                self.walk
                    .reconstruct(&mut parsed, Some(&views), &mut picture)
            })
        };
        if let Err(error) = decoded {
            self.pool[slot] = Some(picture);
            return Err(error);
        }
        Ok(self.commit(slot, picture, state, &parsed.header, tag))
    }

    /// Make the frame decoded whole into `picture` the stream's, in `slot`:
    /// the state its header leaves, its segment map, and the references it
    /// updates.
    fn commit(
        &mut self,
        slot: usize,
        picture: Planes,
        state: StreamState,
        header: &Header,
        tag: Tag,
    ) -> Decoded<'_> {
        self.state = state;
        self.refs = self.refs.updated(slot, header.inter.as_ref());
        self.walk.commit();
        let Geometry { width, height, .. } = self.walk.geometry;
        Decoded {
            picture: self.pool[slot].insert(picture),
            width,
            height,
            shown: tag.shown,
            keyframe: tag.keyframe,
        }
    }

    /// Let go of every picture no reference holds.
    fn release_free(&mut self) {
        let refs = self.refs;
        for (slot, picture) in self.pool.iter_mut().enumerate() {
            if !refs.holds(slot) {
                *picture = None;
            }
        }
    }
}

/// The pool slots holding the last, golden and alternate frames.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct References {
    last: usize,
    golden: usize,
    alternate: usize,
}

impl References {
    /// All three in `slot`, as a keyframe leaves them.
    const fn all(slot: usize) -> Self {
        Self {
            last: slot,
            golden: slot,
            alternate: slot,
        }
    }

    fn holds(self, slot: usize) -> bool {
        self.last == slot || self.golden == slot || self.alternate == slot
    }

    /// The lowest slot none of them holds: three leave one of four free.
    fn free(self) -> usize {
        let mut slot = 0;
        while self.holds(slot) {
            slot += 1;
        }
        slot
    }

    /// The slot holding `buffer`.
    fn slot(self, buffer: Buffer) -> usize {
        match buffer {
            Buffer::Last => self.last,
            Buffer::Golden => self.golden,
            Buffer::Alternate => self.alternate,
        }
    }

    /// These references once a frame decoded into `slot` has updated them as
    /// its inter header says, or a keyframe, which has none, has taken all
    /// three. The alternate's copy comes first, then the golden's, reading
    /// the alternate as just copied, then the refreshes and the last: the
    /// reference decoder's order.
    fn updated(self, slot: usize, inter: Option<&InterHeader>) -> Self {
        let Some(inter) = inter else {
            return Self::all(slot);
        };
        let mut next = self;
        if let BufferUpdate::CopyOf(from) = inter.buffers.alternate {
            next.alternate = next.slot(from);
        }
        if let BufferUpdate::CopyOf(from) = inter.buffers.golden {
            next.golden = next.slot(from);
        }
        if inter.buffers.golden == BufferUpdate::Refresh {
            next.golden = slot;
        }
        if inter.buffers.alternate == BufferUpdate::Refresh {
            next.alternate = slot;
        }
        if inter.refresh_last {
            next.last = slot;
        }
        next
    }
}

/// A frame's header, read against what its stream carries, and the readers of
/// its partitions.
struct Parsed<'a> {
    header: Header,
    modes: Bool<'a>,
    tokens: Vec<Bool<'a>>,
    interpolation: Interpolation,
}

impl<'a> Parsed<'a> {
    /// Read `frame`'s header against `state`, which takes what it updates,
    /// and split its partitions.
    fn read(
        frame: &'a [u8],
        (tag, interpolation): (Tag, Interpolation),
        geometry: Geometry,
        state: &mut StreamState,
    ) -> Result<Self, Vp8Error> {
        let start = if tag.keyframe {
            UNCOMPRESSED_HEADER
        } else {
            TAG_BYTES
        };
        let rest = frame.get(start..).ok_or(Vp8Error::Truncated)?;
        let head = rest.get(..tag.first_partition).ok_or(Vp8Error::Truncated)?;
        let body = rest.get(tag.first_partition..).ok_or(Vp8Error::Truncated)?;
        let mut modes = Bool::new(head);
        let header = read_header(
            &mut modes,
            tag.keyframe,
            geometry.width,
            geometry.height,
            state,
        )?;
        let tokens = split_partitions(body, header.partitions)?;
        Ok(Self {
            header,
            modes,
            tokens,
            interpolation,
        })
    }
}

/// The three references' planes as inter prediction reads them.
pub(super) struct Views<'p> {
    pub(super) last: [RefPlane<'p>; 3],
    golden: [RefPlane<'p>; 3],
    alternate: [RefPlane<'p>; 3],
}

impl<'p> Views<'p> {
    /// The views of a stream whose macroblocks predict from the last frame
    /// alone, `picture` at `width`×`height` luma samples: each reference
    /// reads it, as none but the last is ever read.
    #[cfg(any(test, feature = "encode"))]
    pub(super) fn only(
        picture: &'p Planes,
        (width, height): (usize, usize),
    ) -> Result<Self, Vp8Error> {
        let last = picture
            .view(width, height)
            .ok_or(Vp8Error::DimensionsOverflow)?;
        Ok(Self {
            last,
            golden: last,
            alternate: last,
        })
    }

    fn of(&self, buffer: Buffer) -> &[RefPlane<'p>; 3] {
        match buffer {
            Buffer::Last => &self.last,
            Buffer::Golden => &self.golden,
            Buffer::Alternate => &self.alternate,
        }
    }
}

/// The planes of the pictures `refs` names in `pool`, at `geometry`'s whole
/// macroblocks.
fn views(
    pool: &[Option<Planes>; POOL],
    refs: References,
    geometry: Geometry,
) -> Result<Views<'_>, Vp8Error> {
    let (width, height) = geometry.luma();
    let view = |buffer: Buffer| {
        pool.get(refs.slot(buffer))
            .and_then(Option::as_ref)
            .and_then(|picture| picture.view(width, height))
            .ok_or(Vp8Error::DimensionsOverflow)
    };
    Ok(Views {
        last: view(Buffer::Last)?,
        golden: view(Buffer::Golden)?,
        alternate: view(Buffer::Alternate)?,
    })
}

/// What every macroblock of a frame is reconstructed against.
pub(super) struct Against<'a, 'p> {
    pub(super) header: &'a Header,
    pub(super) factors: [Dequant; SEGMENTS],
    pub(super) interpolation: Interpolation,
    pub(super) references: Option<&'a Views<'p>>,
}

impl Against<'_, '_> {
    /// Reconstruct `decided`, the macroblock at `column` of macroblock row
    /// `row`, into `target` from its dequantised `coeffs`, its luma's top at
    /// sample row `top` of `target`, whose residual an inter macroblock adds
    /// only where `coded` says its tokens went past their first positions;
    /// answers what the loop filter is to do at it.
    pub(super) fn reconstruct(
        &self,
        decided: &MbInfo,
        coded: bool,
        ((column, row), top): ((usize, usize), isize),
        coeffs: &mut [[i16; BLOCK_COEFFS]; MB_BLOCKS],
        target: &mut Planes,
    ) -> Result<Edges, Vp8Error> {
        let has_y2 = decided.has_y2();
        let origin = (isize::try_from(column * 16).unwrap_or(0), top);
        match decided.prediction {
            Prediction::Intra {
                ymode,
                uvmode,
                bmodes,
            } => {
                let modes = (ymode, &bmodes, uvmode);
                target.reconstruct_intra(modes, origin, row > 0, coeffs);
            }
            Prediction::Inter {
                reference, motion, ..
            } => {
                let views = self.references.ok_or(Vp8Error::Interframe)?;
                let planes = [
                    &mut target.luma,
                    &mut target.blue_chroma,
                    &mut target.red_chroma,
                ];
                predict_inter(
                    views.of(reference),
                    &motion,
                    self.interpolation,
                    (column, row),
                    planes,
                    top,
                );
                if coded {
                    target.add_macroblock_residual(origin, has_y2, coeffs);
                }
            }
        }
        let level = filter_level(self.header, decided.segment, decided.filter_deltas());
        Ok(Edges::new(level, coded || !has_y2))
    }
}

/// What a stream's frames are decoded with besides their pictures: the
/// segment map the last frame left and the one the frame being decoded makes,
/// and the contexts a frame's walk carries.
struct Walk {
    geometry: Geometry,
    /// Each macroblock's segment, as the last frame decoded left it.
    segments: Vec<u8>,
    /// Each macroblock's segment in the frame being decoded.
    next_segments: Vec<u8>,
    /// The coding decisions of the macroblock row above and of this one.
    above: Vec<MbInfo>,
    row: Vec<MbInfo>,
    /// What the loop filter does at each macroblock of the row above and of
    /// this one.
    above_edges: Vec<Edges>,
    edges: Vec<Edges>,
    tokens: Tokens,
    intra: IntraContexts,
}

/// A macroblock row's entries before a frame reads them.
const UNREAD: MbInfo = MbInfo {
    segment: 0,
    skip: true,
    prediction: Prediction::Intra {
        ymode: 0,
        uvmode: 0,
        bmodes: [0; BLOCK_COEFFS],
    },
};

impl Walk {
    fn new(geometry: Geometry) -> Result<Self, Vp8Error> {
        let Geometry { columns, rows, .. } = geometry;
        let map = || fallible::filled(columns * rows, 0u8).ok_or(Vp8Error::OutOfMemory);
        let decisions = || fallible::filled(columns, UNREAD).ok_or(Vp8Error::OutOfMemory);
        let filters = || fallible::filled(columns, Edges::default()).ok_or(Vp8Error::OutOfMemory);
        Ok(Self {
            geometry,
            segments: map()?,
            next_segments: map()?,
            above: decisions()?,
            row: decisions()?,
            above_edges: filters()?,
            edges: filters()?,
            tokens: Tokens::new(columns)?,
            intra: IntraContexts::new(columns)?,
        })
    }

    /// Make the segment map the frame just decoded left the stream's.
    fn commit(&mut self) {
        core::mem::swap(&mut self.segments, &mut self.next_segments);
    }

    /// Decode `parsed` into `target`, its inter macroblocks predicted from
    /// `references`, a macroblock row at a time: each row reconstructed
    /// against the unfiltered rows before it, then the row above it filtered.
    /// Only `target` and the walk's own contexts change.
    fn reconstruct(
        &mut self,
        parsed: &mut Parsed<'_>,
        references: Option<&Views<'_>>,
        target: &mut Planes,
    ) -> Result<(), Vp8Error> {
        let frame = Against {
            header: &parsed.header,
            factors: segment_factors(&parsed.header),
            interpolation: parsed.interpolation,
            references,
        };
        self.tokens.start_frame();
        self.intra.start_frame();
        let Geometry { columns, rows, .. } = self.geometry;
        for row in 0..rows {
            self.read_modes(frame.header, &mut parsed.modes, row);
            self.tokens.start_row();
            let partition = row % parsed.tokens.len().max(1);
            let reader = parsed
                .tokens
                .get_mut(partition)
                .ok_or(Vp8Error::InvalidPartitions)?;
            for column in 0..columns {
                self.macroblock(&frame, reader, (column, row), target)?;
            }
            finish_row(target, frame.header, &self.above_edges, row);
            core::mem::swap(&mut self.above, &mut self.row);
            core::mem::swap(&mut self.above_edges, &mut self.edges);
        }
        finish_frame(target, frame.header, &self.above_edges, rows);
        if parsed.modes.exhausted() {
            return Err(Vp8Error::Truncated);
        }
        Ok(())
    }

    /// Read the coding decisions of macroblock row `row` into the walk's row:
    /// a keyframe's against the subblock modes around them, an inter frame's
    /// against the row above, each macroblock keeping the segment the last
    /// frame gave it where this one leaves the map be.
    fn read_modes(&mut self, header: &Header, modes: &mut Bool<'_>, row: usize) {
        let Some(inter) = &header.inter else {
            self.intra.start_row();
            for (column, decided) in self.row.iter_mut().enumerate() {
                *decided = MbInfo::from(self.intra.read(header, modes, column));
            }
            return;
        };
        let Geometry { columns, rows, .. } = self.geometry;
        let previous = self.segments.get(row * columns..).unwrap_or_default();
        for (decided, &segment) in self.row.iter_mut().zip(previous) {
            decided.segment = segment;
        }
        let above: &[MbInfo] = if row == 0 { &[] } else { &self.above };
        read_row(modes, header, inter, above, &mut self.row, (row, rows));
    }

    /// Decode the macroblock at `column` of macroblock row `row` into
    /// `target`: its residual, its prediction, and what the loop filter is to
    /// do at it.
    fn macroblock(
        &mut self,
        frame: &Against<'_, '_>,
        reader: &mut Bool<'_>,
        (column, row): (usize, usize),
        target: &mut Planes,
    ) -> Result<(), Vp8Error> {
        let Some(&decided) = self.row.get(column) else {
            return Ok(());
        };
        let has_y2 = decided.has_y2();
        let coding = ResidualCoding {
            skip: decided.skip,
            has_y2,
            factor: frame
                .factors
                .get(usize::from(decided.segment))
                .copied()
                .unwrap_or_default(),
        };
        let mut coeffs = [[0i16; BLOCK_COEFFS]; MB_BLOCKS];
        let probs = &frame.header.probs.coeff;
        let coded = self
            .tokens
            .macroblock(reader, probs, column, coding, &mut coeffs)?;
        let at = ((column, row), sample_rows(row).0);
        let edges = frame.reconstruct(&decided, coded, at, &mut coeffs, target)?;
        if let Some(slot) = self.edges.get_mut(column) {
            *slot = edges;
        }
        let at = row * self.geometry.columns + column;
        if let Some(segment) = self.next_segments.get_mut(at) {
            *segment = decided.segment;
        }
        Ok(())
    }
}

impl From<Macroblock> for MbInfo {
    fn from(block: Macroblock) -> Self {
        Self {
            segment: block.segment,
            skip: block.skip,
            prediction: Prediction::Intra {
                ymode: block.ymode,
                uvmode: block.uvmode,
                bmodes: block.bmodes,
            },
        }
    }
}

/// Settle macroblock row `row` of `target` once it is reconstructed: carry
/// its bottom luma row into the spare columns the row below predicts from,
/// then filter the row above it, which `above_edges` describes, now that no
/// prediction reads it unfiltered.
pub(super) fn finish_row(target: &mut Planes, header: &Header, above_edges: &[Edges], row: usize) {
    target.luma.extend_row(sample_rows(row).0 + 15);
    if let Some(above) = row.checked_sub(1) {
        filter_row(target, header, above_edges, sample_rows(above), above > 0);
    }
}

/// Filter the last of a frame's `rows` macroblock rows, which `edges`
/// describes, once every row is reconstructed.
pub(super) fn finish_frame(target: &mut Planes, header: &Header, edges: &[Edges], rows: usize) {
    if let Some(last) = rows.checked_sub(1) {
        filter_row(target, header, edges, sample_rows(last), last > 0);
    }
}

/// Where macroblock row `row` starts in luma and in chroma samples.
fn sample_rows(row: usize) -> (isize, isize) {
    (
        isize::try_from(row * 16).unwrap_or(0),
        isize::try_from(row * 8).unwrap_or(0),
    )
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;

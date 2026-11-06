//! The VP8 encoder a screen recording is coded with: keyframes, and inter
//! frames predicting from the last frame alone. Each macroblock takes the
//! prediction that costs least — the squared error of its reconstruction and
//! its bits, at a worth the quantiser sets — and is reconstructed by the
//! decoder's own predictors, inverse transforms and loop filter, so the
//! reference a frame is coded against is, to the bit, every decoder's.
//!
//! A frame is coded in two passes: the first decides and reconstructs each
//! macroblock and counts the token branches it takes; the second writes the
//! header, with each token probability the counts make cheaper replaced, then
//! the modes and the tokens. A first partition longer than a tag can name is
//! coded again without subblock prediction, then at the cheapest modes, which
//! fit any picture the format carries.
//!
//! Both passes spread across a runner's cores and code the same bytes on any
//! of them. The first is a wavefront: a lane codes each macroblock row in its
//! own band, two macroblocks behind the row above, from what that row
//! publishes as it goes — its unfiltered bottom samples and its decisions,
//! which the macroblocks below predict from, then its filtered bottom samples,
//! which their top edges read — and filters the row a macroblock behind. The
//! second writes a large frame's tokens as eight partitions at once, beside
//! its modes.
//!
//! A macroblock no changed region touches is not examined: it is skipped with
//! no motion from the last frame, which an inter frame's mode delta filters at
//! level nought, so static content is copied exactly and never smears; a row
//! of nothing else is the last frame's, copied whole.
//!
//! Every buffer is made with the encoder; a frame grows only the caller's
//! output, and a frame refused that room leaves the encoder as it was.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::{Range, RangeInclusive};

use tairix_inline::ArrayVec;
use tairix_parallel::{for_each, for_each_ordered, JobRunner, Published};
use tairix_util::fallible;

use super::colour;
use super::forward::{forward_dct, forward_walsh, Step};
use super::inter::{lent, quarters, write_row, InterMode, Lent, MbInfo, Prediction, ZERO};
use super::motion::{luma_sad, predict_inter, Interpolation, Motion, Mv, EDGE_SCRATCH};
use super::stream::{Against, Geometry, Views};
use super::writer::{
    codable, code_block, cost, write_block, write_header, write_mv, Cost, Counted, PlaneCounts,
    Priced, Writer, BMODE_PATHS, DEFAULT_TOKEN_COSTS, KF_YMODE_PATHS, MV_REF_PATHS, UV_MODE_PATHS,
    YMODE_PATHS,
};
use super::{
    add_residual, dequantise, filter_macroblock, implied_bmodes, keyframe_header, predict_block,
    segment_factors, subblock_origin, walk_blocks, Buffer, BufferUpdate, Buffers, CoeffProbs,
    Dequant, Edges, FilterDeltas, Header, InterHeader, IntraContexts, Macroblock, Nonzero, Plane,
    Planes, Probabilities, Quant, Rgba8, Segmentation, StreamState, Tag, Tokens, Vp8Error,
    ABOVE_ABSENT, BANDS, BLOCK_COEFFS, BLUE_BLOCKS, B_DC_PRED, B_HU_PRED, B_PRED,
    COEFF_UPDATE_PROBS, COLUMN_CONTEXT_BYTES, CONTEXTS, DC_PRED, DEFAULT_COEFF_PROBS,
    DEFAULT_MV_PROBS, EDGE_REACH, KF_BMODE_PROBS, KF_UV_MODE_PROBS, KF_YMODE_PROBS, LEFT_ABSENT,
    LUMA_BLOCKS, MAX_FILTER_LEVEL, MAX_PARTITIONS, MB_BLOCKS, NODES, PLANES, RED_BLOCKS,
    SPARE_COLUMNS, TAG_BYTES, TM_PRED, UNCOMPRESSED_HEADER, UV_MODE_PROBS, Y2_BLOCK, YMODE_PROBS,
    ZIGZAG,
};

/// A region of the picture that changed since the picture last encoded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Changed {
    /// The column of its left edge.
    pub x: u32,
    /// The row of its top edge.
    pub y: u32,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
}

/// How finely a recording is quantised: what it trades between size and
/// fidelity.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Quality {
    /// The smallest recording that keeps text legible.
    Compact,
    /// Text and flat colour kept clean, at a fraction of Fine's size.
    #[default]
    Standard,
    /// Anti-aliased edges and pictures kept as well.
    Fine,
}

impl Quality {
    /// The quantiser index the quality codes at, by the step it gives an AC
    /// coefficient, eight to a sample level at the transform's scale: Fine's
    /// 16 is two levels, which keeps an anti-aliased edge's gradations;
    /// Standard's 32 is four, which keeps text and flat colour clean; and
    /// Compact's 52, six and a half, keeps text legible.
    const fn index(self) -> i32 {
        match self {
            Self::Compact => 48,
            Self::Standard => 28,
            Self::Fine => 12,
        }
    }
}

/// What one encode produced.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Encoded {
    /// Whether the frame is a keyframe, which a demuxer's seek lands on.
    pub keyframe: bool,
}

/// A VP8 encoder for a stream of opaque RGBA8 pictures of one size.
pub struct Encoder {
    geometry: Geometry,
    quality: Quality,
    /// The last frame as every decoder of the stream holds it.
    reference: Planes,
    /// The frame being coded, which becomes the reference once it is coded
    /// whole.
    target: Planes,
    /// What the stream's headers carry, as its decoders hold it.
    state: StreamState,
    /// Whether a frame has been coded, so the next need not be a keyframe.
    started: bool,
    coding: Coding,
}

impl Encoder {
    /// An encoder for `width` × `height` pictures, coded on at most `lanes`
    /// cores at once: a runner wider than that codes on `lanes` of its own.
    ///
    /// # Errors
    ///
    /// [`Vp8Error::InvalidGeometry`] for a size the format cannot carry
    /// (zero, or past 16383 either way); [`Vp8Error::OutOfMemory`] when its
    /// planes cannot be had.
    pub fn new(width: u32, height: u32, quality: Quality, lanes: usize) -> Result<Self, Vp8Error> {
        let geometry = Geometry::declared(width, height).ok_or(Vp8Error::InvalidGeometry)?;
        Ok(Self {
            geometry,
            quality,
            reference: geometry.picture()?,
            target: geometry.picture()?,
            state: StreamState::new(),
            started: false,
            coding: Coding::new(geometry, lanes.max(1))?,
        })
    }

    /// The bytes an encoder of that size, made for `lanes` lanes, holds at
    /// most, every buffer included; nought for a size no encoder is made of.
    #[must_use]
    pub fn peak_bytes(width: u32, height: u32, lanes: usize) -> u64 {
        let Some(geometry) = Geometry::declared(width, height) else {
            return 0;
        };
        Coding::peak_bytes(geometry, lanes.max(1))
            .and_then(|coding| {
                let (luma_width, luma_height) = geometry.luma();
                Planes::len(luma_width, luma_height)?
                    .checked_mul(2)?
                    .checked_add(coding)
            })
            .and_then(|total| u64::try_from(total).ok())
            .unwrap_or(u64::MAX)
    }

    /// Encode `rgba` — `height` rows of `width` opaque RGBA8 pixels, `stride`
    /// bytes apart — into `out` across `runner`, `out` cleared and then
    /// holding one complete `VP8 ` frame: the same bytes whichever runner.
    ///
    /// `changed` names where the picture differs from the one last encoded.
    /// A macroblock no region touches is coded as skip with zero motion from
    /// the last frame and filtered not at all, so a static screen costs
    /// almost nothing and never smears; the first frame, and a keyframe, read
    /// the whole picture whatever `changed` says. `key` asks for a keyframe;
    /// the first frame is one regardless.
    ///
    /// # Errors
    ///
    /// [`Vp8Error::InvalidGeometry`] when `rgba` and `stride` do not hold the
    /// picture; [`Vp8Error::OutOfMemory`] when `out` cannot hold the frame. A
    /// refused encode changes nothing: the next frame is coded against the
    /// same reference.
    pub fn encode(
        &mut self,
        rgba: &[u8],
        stride: usize,
        changed: &[Changed],
        key: bool,
        out: &mut Vec<u8>,
        runner: &dyn JobRunner,
    ) -> Result<Encoded, Vp8Error> {
        let picture = Picture::new(rgba, stride, self.geometry)?;
        let keyframe = key || !self.started;
        if !keyframe {
            self.coding.touch(changed, self.geometry);
        }
        for modes in [Modes::Chosen, Modes::Whole, Modes::Cheapest] {
            if self.code(picture, keyframe, modes, out, runner)? {
                return Ok(Encoded { keyframe });
            }
        }
        // The cheapest modes fit any picture the format carries.
        Err(Vp8Error::InvalidGeometry)
    }

    /// Code `picture` as the stream's next frame into `out`, its macroblocks
    /// decided between `modes`; answers whether the frame's first partition
    /// fit what its tag names, the frame becoming the stream's only where it
    /// did.
    fn code(
        &mut self,
        picture: Picture<'_>,
        keyframe: bool,
        modes: Modes,
        out: &mut Vec<u8>,
        runner: &dyn JobRunner,
    ) -> Result<bool, Vp8Error> {
        let mut header = self.header(keyframe, modes);
        self.analyse(picture, &header, modes, runner)?;
        self.coding.choose(&mut header);
        let mut written = self.write(&header, out, runner)?;
        if written == Written::Unsized {
            header.partitions = 1;
            written = self.write(&header, out, runner)?;
        }
        if written != Written::Whole {
            return Ok(false);
        }
        core::mem::swap(&mut self.reference, &mut self.target);
        self.state = carried(&header);
        self.started = true;
        Ok(true)
    }

    /// The header a frame is coded under until its probabilities are chosen:
    /// the quality's quantiser and the loop filter derived from it, and an
    /// inter frame's deltas.
    fn header(&self, keyframe: bool, modes: Modes) -> Header {
        let index = self.quality.index();
        let inter = InterHeader {
            buffers: Buffers {
                golden: BufferUpdate::Keep,
                alternate: BufferUpdate::Keep,
                sign_bias_golden: false,
                sign_bias_alternate: false,
            },
            refresh_last: true,
            prob_intra: EVEN,
            prob_last: FROM_LAST,
            prob_golden: EVEN,
        };
        Header {
            width: self.geometry.width,
            height: self.geometry.height,
            simple_filter: false,
            filter_level: loop_filter_level(index),
            sharpness: 0,
            segmentation: Segmentation::none(),
            deltas: if keyframe {
                FilterDeltas::none()
            } else {
                STILL_UNFILTERED
            },
            quant: Quant {
                index,
                y_dc: 0,
                y2_dc: 0,
                y2_ac: 0,
                uv_dc: 0,
                uv_ac: 0,
            },
            partitions: 1,
            probs: Probabilities::DEFAULT,
            skip_probability: (modes != Modes::Cheapest).then_some(EVEN),
            refresh_entropy: false,
            inter: (!keyframe).then_some(inter),
        }
    }

    /// The first pass: decide, quantise and reconstruct every macroblock of
    /// `picture` into the target as `header` heads the frame, counting the
    /// token branches it takes, its rows spread across `runner`.
    fn analyse(
        &mut self,
        picture: Picture<'_>,
        header: &Header,
        modes: Modes,
        runner: &dyn JobRunner,
    ) -> Result<(), Vp8Error> {
        let inter = header.inter.is_some();
        let views = match header.inter {
            Some(_) => Some(Views::only(&self.reference, self.geometry.luma())?),
            None => None,
        };
        let factors = segment_factors(header);
        let lambda = bit_worth(factors[0].y_ac);
        let analysis = Analysis {
            picture,
            frame: Against {
                header,
                factors,
                interpolation: Interpolation::SixTap,
                references: views.as_ref(),
            },
            factor: factors[0],
            steps: Steps::of(&factors[0]),
            lambda,
            motion_lambda: lambda.isqrt().max(1),
            intra_floor: intra_floor(lambda),
            modes,
            rows: self.coding.rows,
        };
        let used = self.coding.start(runner);
        let Coding {
            columns,
            rows,
            decisions,
            levels,
            shapes,
            touched,
            still,
            contexts,
            borders,
            tiles,
            lanes,
            ..
        } = &mut self.coding;
        let front = Front {
            columns: *columns,
            rows: *rows,
            touched,
            still: inter
                .then_some(&**still)
                .filter(|_| modes != Modes::Cheapest),
            last: &self.reference,
            borders,
            tiles,
        };
        let work = RowWorks::new(
            &mut self.target,
            (decisions, levels, shapes, contexts),
            *rows,
        );
        let lanes = lanes.get_mut(..used).unwrap_or_default();
        for_each_ordered(runner, lanes, work, &|lane, row, work| {
            analysis.code_row(lane, row, work, &front);
        });
        self.coding.gather(used)
    }

    /// The second pass: write the frame `header` heads into `out` — its tag,
    /// a keyframe's start code and size, its first partition and its token
    /// partitions, those written at once across `runner` — answering whether
    /// the frame fit what its tag and partition sizes name, `out` left empty
    /// where it did not.
    fn write(
        &mut self,
        header: &Header,
        out: &mut Vec<u8>,
        runner: &dyn JobRunner,
    ) -> Result<Written, Vp8Error> {
        let keyframe = header.inter.is_none();
        let lead = if keyframe {
            UNCOMPRESSED_HEADER
        } else {
            TAG_BYTES
        };
        out.clear();
        if !fallible::grow_to(out, lead, 0) {
            return Err(Vp8Error::OutOfMemory);
        }
        let Coding {
            columns,
            rows,
            decisions,
            levels,
            shapes,
            contexts,
            intra,
            partitions,
            ..
        } = &mut self.coding;
        let tokens = TokenRows {
            columns: *columns,
            rows: *rows,
            levels,
            shapes,
            contexts,
            probs: &header.probs.coeff,
            partitions: header.partitions,
        };
        let mut jobs: ArrayVec<WriteJob<'_>, { MAX_PARTITIONS + 1 }> = ArrayVec::new();
        let modes = WriteJob::Modes {
            out: core::mem::take(out),
            decisions,
            intra,
            closed: None,
        };
        let _ = jobs.try_push(modes);
        for (index, partition) in partitions.iter_mut().take(header.partitions).enumerate() {
            partition.overflowed = false;
            let _ = jobs.try_push(WriteJob::Tokens { index, partition });
        }
        let state = &self.state;
        for_each(runner, &mut jobs, &|job| job.write(header, state, &tokens));
        let closed = match jobs.first_mut() {
            Some(WriteJob::Modes { closed, .. }) => closed.take(),
            _ => None,
        };
        kept(closed.unwrap_or(Err(Vec::new())), out)?;
        let tag = Tag {
            keyframe,
            version: 0,
            shown: true,
            first_partition: out.len() - lead,
        };
        let (Some(bytes), Some(head)) = (tag.bytes(), out.first_chunk_mut()) else {
            out.clear();
            return Ok(Written::Unfit);
        };
        *head = bytes;
        if keyframe {
            let size = out
                .get_mut(TAG_BYTES..lead)
                .and_then(<[u8]>::first_chunk_mut);
            if let Some(size) = size {
                *size = keyframe_header(header.width, header.height);
            }
        }
        let partitions = jobs.iter_mut().filter_map(|job| match job {
            WriteJob::Tokens { index, partition } => Some((*index, &mut **partition)),
            WriteJob::Modes { .. } => None,
        });
        tokens.append(partitions, out)
    }
}

/// How a frame's second pass came out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Written {
    /// Every part fit what the format names it by.
    Whole,
    /// The first partition is longer than a tag names.
    Unfit,
    /// A token partition is longer than its size names, which one partition
    /// alone, needing none, would not be.
    Unsized,
}

/// Luma rows a lane's band keeps above the macroblock row it codes: twice the
/// rows a top edge reads, so chroma keeps those it reads at half luma's, as a
/// site's chroma origin is half its luma's everywhere.
const BAND_TOP: usize = 2 * EDGE_REACH;

/// Luma rows of a lane's band.
const BAND_ROWS: usize = BAND_TOP + 16;

/// Rows above a top edge its filter moves, luma's and chroma's alike: what a
/// row leaves the row above it to settle.
const EDGE_MOVES: usize = EDGE_REACH - 1;

/// Coded macroblocks past which a frame's tokens are written as
/// [`MAX_PARTITIONS`] partitions rather than one: fewer code too few bytes for
/// the others' sizes and flushes to pay for themselves.
const PARTITIONED_MACROBLOCKS: u32 = 1024;

/// The bytes a token partition is made with: an eighth of the picture's raw
/// size, so the eight hold what a recording's frame comes to at any quality.
/// One that outgrows it is written again into the frame itself.
fn partition_room(geometry: Geometry) -> usize {
    let (width, height) = geometry.luma();
    width.saturating_mul(height).saturating_mul(3) / 2 / MAX_PARTITIONS
}

/// The rows of a plane whose macroblocks are `size` samples high that coding
/// macroblock row `row` of `rows` settles: from the rows of the row above
/// that its top edges move, to its own but those the row below's will.
fn settled(row: usize, rows: usize, size: usize) -> Range<usize> {
    let start = match row {
        0 => 0,
        _ => row * size - EDGE_MOVES,
    };
    let end = if row + 1 == rows {
        (row + 1) * size
    } else {
        (row + 1) * size - EDGE_MOVES
    };
    start..end
}

/// Copy what of `from` fits into `into`.
fn copy_into(into: &mut [u8], from: &[u8]) {
    let len = into.len().min(from.len());
    if let (Some(into), Some(from)) = (into.get_mut(..len), from.get(..len)) {
        into.copy_from_slice(from);
    }
}

/// The `N` samples of `plane` from (`x`, `y`) along their row; nought where
/// they would leave it.
fn samples<const N: usize>(plane: &Plane, (x, y): (usize, usize)) -> [u8; N] {
    let at = plane.at(signed(x), signed(y));
    plane
        .samples
        .get(at..at + N)
        .and_then(|run| run.try_into().ok())
        .unwrap_or([0; N])
}

/// Write `run` into `plane` from (`x`, `y`) along its row.
fn lay(plane: &mut Plane, (x, y): (usize, usize), run: &[u8]) {
    copy_into(plane.row_mut(signed(x), signed(y), run.len()), run);
}

/// `value` as a plane coordinate.
fn signed(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

/// What a coded macroblock leaves the row below before the loop filter moves
/// any of it: its bottom samples, which the macroblocks below predict from,
/// its decisions, which lend them their vectors, and the contexts its tokens
/// and subblock modes leave beneath it.
#[derive(Copy, Clone)]
struct Border {
    luma: [u8; 16],
    blue: [u8; 8],
    red: [u8; 8],
    decided: MbInfo,
    tokens: Nonzero,
    bmodes: [u8; 4],
}

impl Border {
    /// What macroblock `column` of a row the last frame lends whole leaves
    /// below it: skipped, unmoved and unfiltered, so its samples are the
    /// last frame's.
    fn lent(last: &Planes, row: usize, column: usize) -> Self {
        Self {
            luma: samples(&last.luma, (column * 16, row * 16 + 15)),
            blue: samples(&last.blue_chroma, (column * 8, row * 8 + 7)),
            red: samples(&last.red_chroma, (column * 8, row * 8 + 7)),
            decided: STILL,
            tokens: Nonzero::default(),
            bmodes: [B_DC_PRED; 4],
        }
    }
}

/// A filtered macroblock's bottom samples, which the top edges below it read
/// and move.
#[derive(Copy, Clone)]
struct Tile {
    luma: [[u8; 16]; EDGE_REACH],
    blue: [[u8; 8]; EDGE_REACH],
    red: [[u8; 8]; EDGE_REACH],
}

impl Tile {
    /// The bottom samples of macroblock `column` of `planes`, whose row of
    /// macroblocks begins at luma row `top` and chroma row half that.
    fn of(planes: &Planes, top: usize, column: usize) -> Self {
        let (luma, chroma) = (top + 16 - EDGE_REACH, top / 2 + 8 - EDGE_REACH);
        Self {
            luma: core::array::from_fn(|y| samples(&planes.luma, (column * 16, luma + y))),
            blue: core::array::from_fn(|y| samples(&planes.blue_chroma, (column * 8, chroma + y))),
            red: core::array::from_fn(|y| samples(&planes.red_chroma, (column * 8, chroma + y))),
        }
    }
}

/// Whether a macroblock skips its residual, and whether it codes a luma DC
/// block: what its tokens are laid out by.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Shape {
    skip: bool,
    has_y2: bool,
}

/// A participant's share of the first pass: the band it codes a macroblock
/// row in and the contexts it carries along the row, and what it counts
/// across the rows it codes.
struct Lane {
    /// The row coded, from luma row [`BAND_TOP`] and chroma row half that.
    /// Above it are the row above's bottom samples: its last row unfiltered
    /// for the macroblocks to predict from, each column turned to the
    /// filtered samples the row above leaves its top edges once nothing
    /// predicts from it.
    band: Planes,
    tokens: Tokens,
    intra: IntraContexts,
    /// The row above's decisions, as far as its macroblocks are read.
    above: Vec<MbInfo>,
    /// What the loop filter does at each macroblock of the row.
    edges: Vec<Edges>,
    counts: Box<Counts>,
    tally: Tally,
    /// What a macroblock of a row this lane coded refused, if one did.
    failed: Option<Vp8Error>,
}

impl Lane {
    fn new(geometry: Geometry) -> Result<Self, Vp8Error> {
        let (luma, _) = geometry.luma();
        let columns = geometry.columns;
        let refused = Vp8Error::OutOfMemory;
        Ok(Self {
            band: Planes::new(luma, BAND_ROWS)?,
            tokens: Tokens::new(columns)?,
            intra: IntraContexts::new(columns)?,
            above: fallible::filled(columns, STILL).ok_or(refused)?,
            edges: fallible::filled(columns, Edges::default()).ok_or(refused)?,
            counts: fallible::boxed_array(NO_COUNTS).ok_or(refused)?,
            tally: Tally::default(),
            failed: None,
        })
    }

    /// The bytes a lane for `geometry` holds.
    fn peak_bytes(geometry: Geometry) -> Option<usize> {
        let (luma, _) = geometry.luma();
        let per_column = COLUMN_CONTEXT_BYTES + size_of::<MbInfo>() + size_of::<Edges>();
        Planes::len(luma, BAND_ROWS)?
            .checked_add(geometry.columns.checked_mul(per_column)?)?
            .checked_add(size_of::<Counts>())
    }

    /// Begin a frame's pass.
    fn start(&mut self) {
        flat_counts(&mut self.counts).fill([0; 2]);
        self.tally = Tally::default();
        self.failed = None;
    }

    /// Begin coding row `row`: nothing to the left of its first macroblock,
    /// and above the top row what the format reads where no row lies.
    fn begin(&mut self, row: usize) {
        self.tokens.start_row();
        self.intra.start_row();
        if row == 0 {
            self.tokens.start_frame();
            self.intra.start_frame();
        }
        let planes = [
            (&mut self.band.luma, BAND_TOP - 1),
            (&mut self.band.blue_chroma, BAND_TOP / 2 - 1),
            (&mut self.band.red_chroma, BAND_TOP / 2 - 1),
        ];
        for (plane, border) in planes {
            match row {
                0 => plane.rows_mut(border..border + 1).fill(ABOVE_ABSENT),
                _ => plane.set(-1, signed(border), LEFT_ABSENT),
            }
        }
    }

    /// Lay what macroblock `column` of the row above left, `border`, along
    /// the band's row above, its last sample carried into the spare columns
    /// where it is the row's `last`.
    fn lay_border(&mut self, column: usize, border: &Border, last: bool) {
        let (luma, chroma) = (BAND_TOP - 1, BAND_TOP / 2 - 1);
        lay(&mut self.band.luma, (column * 16, luma), &border.luma);
        lay(
            &mut self.band.blue_chroma,
            (column * 8, chroma),
            &border.blue,
        );
        lay(&mut self.band.red_chroma, (column * 8, chroma), &border.red);
        if last {
            let spare = [border.luma[15]; SPARE_COLUMNS];
            lay(&mut self.band.luma, ((column + 1) * 16, luma), &spare);
        }
    }

    /// Take the contexts and decisions macroblock `column` of the row above
    /// left, `border`, as the macroblock below it is coded against.
    fn lend(&mut self, column: usize, border: &Border) {
        if let Some(slot) = self.tokens.above.get_mut(column) {
            *slot = border.tokens;
        }
        if let Some(slot) = self.intra.above.get_mut(column * 4..column * 4 + 4) {
            slot.copy_from_slice(&border.bmodes);
        }
        if let Some(slot) = self.above.get_mut(column) {
            *slot = border.decided;
        }
    }

    /// Lay `tile`, macroblock `column` of the row above as its filter left
    /// it, above the band's row, for the top edge below it.
    fn lay_tile(&mut self, column: usize, tile: &Tile) {
        let (luma, chroma) = (BAND_TOP - EDGE_REACH, BAND_TOP / 2 - EDGE_REACH);
        for (y, line) in tile.luma.iter().enumerate() {
            lay(&mut self.band.luma, (column * 16, luma + y), line);
        }
        for (y, (blue, red)) in tile.blue.iter().zip(&tile.red).enumerate() {
            lay(&mut self.band.blue_chroma, (column * 8, chroma + y), blue);
            lay(&mut self.band.red_chroma, (column * 8, chroma + y), red);
        }
    }

    /// What macroblock `column` of the band, just coded as `decided`, leaves
    /// the row below.
    fn border(&self, column: usize, decided: MbInfo) -> Border {
        let (luma, chroma) = (BAND_TOP + 15, BAND_TOP / 2 + 7);
        let mut bmodes = [B_DC_PRED; 4];
        if let Some(kept) = self.intra.above.get(column * 4..column * 4 + 4) {
            bmodes.copy_from_slice(kept);
        }
        Border {
            luma: samples(&self.band.luma, (column * 16, luma)),
            blue: samples(&self.band.blue_chroma, (column * 8, chroma)),
            red: samples(&self.band.red_chroma, (column * 8, chroma)),
            decided,
            tokens: self.tokens.above.get(column).copied().unwrap_or_default(),
            bmodes,
        }
    }

    /// Hand row `row` of `rows` to the frame's `work`: the rows above it its
    /// top edges settled and its own but those the row below's will move,
    /// and the token contexts it leaves below it.
    fn settle(&self, row: usize, rows: usize, work: &mut RowWork<'_>) {
        let planes = [
            (&self.band.luma, &mut *work.luma, 16),
            (&self.band.blue_chroma, &mut *work.blue, 8),
            (&self.band.red_chroma, &mut *work.red, 8),
        ];
        for (band, frame, size) in planes {
            let settled = settled(row, rows, size);
            let top = BAND_TOP * size / 16;
            let from = (settled.start + top).saturating_sub(row * size);
            copy_into(frame, band.rows(from..from + settled.len()));
        }
        for (slot, &context) in work.contexts.iter_mut().zip(&self.tokens.above) {
            *slot = context;
        }
    }
}

/// No branch counted.
const NO_COUNTS: PlaneCounts = [[[[0; 2]; NODES]; CONTEXTS]; BANDS];

/// Every branch count of `counts`, in one run.
fn flat_counts(counts: &mut Counts) -> &mut [[u32; 2]] {
    counts
        .as_flattened_mut()
        .as_flattened_mut()
        .as_flattened_mut()
}

/// What is left of a slice once the rows before have taken theirs.
struct Rest<'f, T>(&'f mut [T]);

impl<'f, T> Rest<'f, T> {
    /// The next `len`, which the slice keeps no longer.
    fn take(&mut self, len: usize) -> &'f mut [T] {
        let all = core::mem::take(&mut self.0);
        let (head, tail) = all.split_at_mut(len.min(all.len()));
        self.0 = tail;
        head
    }
}

/// What one macroblock row of a frame's first pass writes and nothing else
/// does: the frame rows it settles, its macroblocks' decisions, levels and
/// shapes, and the token contexts it leaves below it.
struct RowWork<'f> {
    luma: &'f mut [u8],
    blue: &'f mut [u8],
    red: &'f mut [u8],
    decisions: &'f mut [MbInfo],
    levels: &'f mut [Levels],
    shapes: &'f mut [Shape],
    contexts: &'f mut [Nonzero],
}

/// A frame cut into its rows' work, the rows in order.
struct RowWorks<'f> {
    row: usize,
    rows: usize,
    columns: usize,
    strides: (usize, usize),
    luma: Rest<'f, u8>,
    blue: Rest<'f, u8>,
    red: Rest<'f, u8>,
    decisions: Rest<'f, MbInfo>,
    levels: Rest<'f, Levels>,
    shapes: Rest<'f, Shape>,
    contexts: Rest<'f, Nonzero>,
}

impl<'f> RowWorks<'f> {
    /// The rows of `target`, and of a frame's per-macroblock records, that
    /// each of its `rows` rows writes.
    fn new(
        target: &'f mut Planes,
        (decisions, levels, shapes, contexts): (
            &'f mut [MbInfo],
            &'f mut [Levels],
            &'f mut [Shape],
            &'f mut [Nonzero],
        ),
        rows: usize,
    ) -> Self {
        let strides = (target.luma.stride, target.blue_chroma.stride);
        let pictured = |plane: &'f mut Plane| {
            let stride = plane.stride;
            plane.samples.get_mut(stride..).unwrap_or_default()
        };
        Self {
            row: 0,
            rows,
            columns: decisions.len() / rows.max(1),
            strides,
            luma: Rest(pictured(&mut target.luma)),
            blue: Rest(pictured(&mut target.blue_chroma)),
            red: Rest(pictured(&mut target.red_chroma)),
            decisions: Rest(decisions),
            levels: Rest(levels),
            shapes: Rest(shapes),
            contexts: Rest(contexts),
        }
    }
}

impl<'f> Iterator for RowWorks<'f> {
    type Item = RowWork<'f>;

    fn next(&mut self) -> Option<RowWork<'f>> {
        let (row, rows, columns) = (self.row, self.rows, self.columns);
        if row >= rows {
            return None;
        }
        self.row += 1;
        let luma = settled(row, rows, 16).len() * self.strides.0;
        let chroma = settled(row, rows, 8).len() * self.strides.1;
        Some(RowWork {
            luma: self.luma.take(luma),
            blue: self.blue.take(chroma),
            red: self.red.take(chroma),
            decisions: self.decisions.take(columns),
            levels: self.levels.take(columns),
            shapes: self.shapes.take(columns),
            contexts: self.contexts.take(columns),
        })
    }
}

/// What every row of a frame's first pass reads and none writes: the changed
/// macroblocks, the last frame, and what each row publishes.
struct Front<'a> {
    columns: usize,
    rows: usize,
    touched: &'a [bool],
    /// Each row an inter frame's change leaves wholly alone, where such a row
    /// is the last frame's.
    still: Option<&'a [bool]>,
    last: &'a Planes,
    borders: &'a [Published<Border>],
    tiles: &'a [Published<Tile>],
}

impl Front<'_> {
    /// Whether row `row` is the last frame's, copied whole.
    fn lends(&self, row: usize) -> bool {
        self.still
            .is_some_and(|still| still.get(row).copied().unwrap_or(false))
    }

    /// What macroblock `column` of row `row` leaves the row below, once it
    /// has.
    fn border(&self, row: usize, column: usize) -> Border {
        if self.lends(row) {
            return Border::lent(self.last, row, column);
        }
        self.borders
            .get(row * self.columns + column)
            .map_or_else(|| Border::lent(self.last, row, column), |slot| *slot.wait())
    }

    /// Macroblock `column` of row `row` as its filter left it for the top
    /// edge below it, once it has.
    fn tile(&self, row: usize, column: usize) -> Tile {
        let lent = || Tile::of(self.last, row * 16, column);
        if self.lends(row) {
            return lent();
        }
        self.tiles
            .get(row * self.columns + column)
            .map_or_else(lent, |slot| *slot.wait())
    }

    /// Publish what macroblock `column` of row `row` leaves the row below.
    fn publish_border(&self, row: usize, column: usize, border: Border) {
        if let Some(slot) = self.borders.get(row * self.columns + column) {
            let _ = slot.publish(border);
        }
    }

    /// Publish macroblock `column` of row `row` as its filter left it.
    fn publish_tile(&self, row: usize, column: usize, tile: Tile) {
        if let Some(slot) = self.tiles.get(row * self.columns + column) {
            let _ = slot.publish(tile);
        }
    }
}

/// What a macroblock is decided against besides its pictures: the token
/// contexts its residual is priced beside, the subblock modes around it, and
/// the decisions of the row above and of its own row so far.
struct Around<'a> {
    tokens: &'a Tokens,
    intra: &'a IntraContexts,
    above: &'a [MbInfo],
    row: &'a [MbInfo],
}

impl Around<'_> {
    /// What the macroblock at `site`, of a frame of `rows` rows, is coded
    /// against when it moves: the vectors its neighbours lend it.
    fn lending(&self, site: Site, rows: usize, buffers: &Buffers) -> Lent {
        lent(self.above, self.row, site.column, (site.row, rows), buffers)
    }
}

/// One of the token partitions a large frame is written in: its bytes, held
/// to the room it was made with, and the contexts it walks its rows with.
struct Partition {
    bytes: Vec<u8>,
    tokens: Tokens,
    /// Its tokens outgrew its room, so the frame holds them written anew.
    overflowed: bool,
}

impl Partition {
    fn new(columns: usize, room: usize) -> Result<Self, Vp8Error> {
        let mut bytes = Vec::new();
        if !fallible::reserve(&mut bytes, room) {
            return Err(Vp8Error::OutOfMemory);
        }
        Ok(Self {
            bytes,
            tokens: Tokens::new(columns)?,
            overflowed: false,
        })
    }
}

/// What the token partitions are written from.
struct TokenRows<'a> {
    columns: usize,
    rows: usize,
    levels: &'a [Levels],
    shapes: &'a [Shape],
    contexts: &'a [Nonzero],
    probs: &'a CoeffProbs,
    partitions: usize,
}

impl TokenRows<'_> {
    /// Write the tokens of partition `index` — each row whose number leaves
    /// it over the partitions — with `tokens` as their contexts.
    fn write(&self, writer: &mut Writer, tokens: &mut Tokens, index: usize) {
        let columns = self.columns;
        for row in (index..self.rows).step_by(self.partitions.max(1)) {
            let above = row
                .checked_sub(1)
                .and_then(|above| self.contexts.get(above * columns..row * columns));
            match above {
                Some(above) => copy_contexts(&mut tokens.above, above),
                None => tokens.start_frame(),
            }
            tokens.start_row();
            let shapes = self.shapes.get(row * columns..).unwrap_or_default();
            let levels = self.levels.get(row * columns..).unwrap_or_default();
            for (column, (shape, levels)) in shapes.iter().zip(levels).take(columns).enumerate() {
                if shape.skip {
                    tokens.skip(column, shape.has_y2);
                    continue;
                }
                tokens.each_block(column, shape.has_y2, |at| {
                    match (self.probs.get(at.plane), levels.get(at.index)) {
                        (Some(plane), Some(block)) => {
                            write_block(writer, plane, at.context, at.first, block)
                        }
                        _ => false,
                    }
                });
            }
        }
    }

    /// Append the frame's token partitions to `out`, its first partition
    /// written: the sizes of all but the last, then each, `written` holding
    /// each in its room or, where it outgrew that, to be written anew.
    fn append<'p>(
        &self,
        written: impl Iterator<Item = (usize, &'p mut Partition)>,
        out: &mut Vec<u8>,
    ) -> Result<Written, Vp8Error> {
        let sizes = out.len();
        let table = 3 * self.partitions.saturating_sub(1);
        if !fallible::grow_to(out, sizes + table, 0) {
            out.clear();
            return Err(Vp8Error::OutOfMemory);
        }
        for (index, partition) in written {
            let start = out.len();
            if partition.overflowed {
                let mut writer = Writer::onto(core::mem::take(out));
                self.write(&mut writer, &mut partition.tokens, index);
                kept(writer.close(), out)?;
            } else if !fallible::reserve(out, partition.bytes.len()) {
                out.clear();
                return Err(Vp8Error::OutOfMemory);
            } else {
                out.extend_from_slice(&partition.bytes);
            }
            if index + 1 == self.partitions {
                continue;
            }
            let size = u32::try_from(out.len() - start)
                .ok()
                .filter(|&size| size < 1 << 24);
            let entry = out
                .get_mut(sizes + 3 * index..)
                .and_then(<[u8]>::first_chunk_mut::<3>);
            let (Some(size), Some(entry)) = (size, entry) else {
                out.clear();
                return Ok(Written::Unsized);
            };
            let [low, middle, high, _] = size.to_le_bytes();
            *entry = [low, middle, high];
        }
        Ok(Written::Whole)
    }
}

/// Copy the token contexts `from` into `into`, column for column.
fn copy_contexts(into: &mut [Nonzero], from: &[Nonzero]) {
    for (slot, &context) in into.iter_mut().zip(from) {
        *slot = context;
    }
}

/// One of a frame's second-pass jobs: its first partition, or one of its
/// token partitions.
enum WriteJob<'a> {
    Modes {
        out: Vec<u8>,
        decisions: &'a mut [MbInfo],
        intra: &'a mut IntraContexts,
        closed: Option<Result<Vec<u8>, Vec<u8>>>,
    },
    Tokens {
        index: usize,
        partition: &'a mut Partition,
    },
}

impl WriteJob<'_> {
    fn write(&mut self, header: &Header, state: &StreamState, tokens: &TokenRows<'_>) {
        match self {
            Self::Modes {
                out,
                decisions,
                intra,
                closed,
            } => {
                let mut writer = Writer::onto(core::mem::take(out));
                write_header(&mut writer, header, state);
                write_modes(&mut writer, header, decisions, intra, tokens.columns);
                *closed = Some(writer.close());
            }
            Self::Tokens { index, partition } => {
                partition.bytes.clear();
                let mut writer = Writer::within(core::mem::take(&mut partition.bytes));
                tokens.write(&mut writer, &mut partition.tokens, *index);
                match writer.close() {
                    Ok(bytes) => partition.bytes = bytes,
                    Err(bytes) => {
                        partition.bytes = bytes;
                        partition.overflowed = true;
                    }
                }
            }
        }
    }
}

/// Write every macroblock's modes, `decisions` in rows of `columns`, as the
/// first pass decided them: a keyframe's against the subblock modes around
/// them, which `intra` carries.
fn write_modes(
    writer: &mut Writer,
    header: &Header,
    decisions: &mut [MbInfo],
    intra: &mut IntraContexts,
    columns: usize,
) {
    let rows = decisions.len() / columns.max(1);
    let Some(inter) = &header.inter else {
        intra.start_frame();
        for row in decisions.chunks(columns.max(1)) {
            intra.start_row();
            for (column, decided) in row.iter().enumerate() {
                if let Prediction::Intra {
                    ymode,
                    uvmode,
                    bmodes,
                } = decided.prediction
                {
                    let block = Macroblock {
                        segment: decided.segment,
                        skip: decided.skip,
                        ymode,
                        uvmode,
                        bmodes,
                    };
                    intra.write(header, writer, column, &block);
                }
            }
        }
        return;
    };
    for index in 0..rows {
        let (done, rest) = decisions.split_at_mut(index * columns);
        let above = done
            .get(done.len().saturating_sub(columns)..)
            .unwrap_or_default();
        let row = rest.get_mut(..columns).unwrap_or_default();
        write_row(writer, header, inter, above, row, (index, rows));
    }
}

/// A macroblock's quantised coefficients, each block's in scan order: the
/// sixteen luma blocks in raster order, the four blue and the four red chroma
/// blocks, then the luma DC block. Dequantised into raster order, the same
/// shape holds what the inverse transforms read.
type Levels = [[i16; BLOCK_COEFFS]; MB_BLOCKS];

/// The coefficient tree's branches a frame takes, by plane.
type Counts = [PlaneCounts; PLANES];

/// No residual at all.
const NO_LEVELS: Levels = [[0; BLOCK_COEFFS]; MB_BLOCKS];

/// Even odds: the first pass's guess at what a frame's counts set, and the
/// golden frame's, which no macroblock reads.
const EVEN: u8 = 128;

/// The odds an inter macroblock predicts from the last frame, which every
/// one does.
const FROM_LAST: u8 = u8::MAX;

/// The eight bits a header's update of a token probability carries, in
/// 256ths of a bit.
const UPDATE_BITS: u32 = 8 * 256;

/// A whole luma pixel, in a vector's eighths.
const EIGHTHS: i16 = 8;

/// The whole-pixel steps a motion search moves by, coarsest first, and how
/// often it may move at each before it settles.
const SEARCH_STEPS: [i16; 3] = [4, 2, 1];
const SEARCH_MOVES: usize = 4;

/// A macroblock no region touches: skipped, with no motion from the last
/// frame.
const STILL: MbInfo = MbInfo {
    segment: 0,
    skip: true,
    prediction: Prediction::Inter {
        reference: Buffer::Last,
        mode: InterMode::Zero,
        motion: Motion::Whole(ZERO),
    },
};

/// An inter frame's loop-filter deltas: no motion's takes any level to
/// nought, so a macroblock the last frame lends unmoved is copied exactly;
/// the rest are nought.
const STILL_UNFILTERED: FilterDeltas = FilterDeltas {
    enabled: true,
    reference: [0; 4],
    mode: [0, -MAX_FILTER_LEVEL, 0, 0],
};

/// The least a keyframe's subblock mode costs, under the neighbours that
/// make it cheapest.
const CHEAPEST_KEY_SUBBLOCK: u32 = {
    let mut least = u32::MAX;
    let mut above = 0;
    while above < KF_BMODE_PROBS.len() {
        let mut left = 0;
        while left < KF_BMODE_PROBS[above].len() {
            let spent = cheapest_subblock(&KF_BMODE_PROBS[above][left]);
            if spent < least {
                least = spent;
            }
            left += 1;
        }
        above += 1;
    }
    least
};

/// The least any subblock mode costs coded with `probs`.
const fn cheapest_subblock(probs: &[u8; 9]) -> u32 {
    let mut least = u32::MAX;
    let mut mode = 0;
    while mode < BMODE_PATHS.len() {
        let spent = BMODE_PATHS[mode].cost(probs);
        if spent < least {
            least = spent;
        }
        mode += 1;
    }
    least
}

/// The loop-filter level of a frame quantised at `index`: half the index, so
/// the steps it smooths track the step the residual was coded at while a text
/// edge's contrast stays past its limits.
const fn loop_filter_level(index: i32) -> i32 {
    index / 2
}

/// What a bit is worth against squared error when coefficients step by
/// `step`: about the slope of rate against distortion that quantising at
/// that step sets.
fn bit_worth(step: i32) -> u64 {
    (u64::from(step.unsigned_abs()).pow(2) / 24).max(1)
}

/// A choice's cost: what it misses the picture by, `missed`, and its `bits`,
/// in 256ths of a bit, at `lambda`, what a bit is worth.
fn weighed(missed: u64, bits: u32, lambda: u64) -> u64 {
    missed
        .saturating_mul(256)
        .saturating_add(lambda.saturating_mul(u64::from(bits)))
}

/// The mode of `modes` whose `price` is least, the first of any tied, and
/// that price.
fn cheapest(modes: RangeInclusive<u8>, mut price: impl FnMut(u8) -> u64) -> (u8, u64) {
    let first = *modes.start();
    modes
        .map(|mode| (mode, price(mode)))
        .min_by_key(|&(_, spent)| spent)
        .unwrap_or((first, u64::MAX))
}

/// The least an intra macroblock of an inter frame can cost at `lambda`: its
/// cheapest whole-block modes, missing nothing.
fn intra_floor(lambda: u64) -> u64 {
    let costs = ModeCosts::Inter;
    let whole = (DC_PRED..=TM_PRED).map(|mode| costs.ymode(mode)).min();
    let chroma = (DC_PRED..=TM_PRED).map(|mode| costs.uvmode(mode)).min();
    let luma = whole.map_or(u64::MAX, u64::from);
    lambda.saturating_mul(luma.saturating_add(chroma.map_or(0, u64::from)))
}

/// Which modes a frame's macroblocks are decided between. A frame's first
/// partition must fit what its tag names, which for every picture the format
/// carries only the cheapest modes are sure to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Modes {
    /// Whichever costs least.
    Chosen,
    /// Whichever costs least but prediction a subblock at a time, whose
    /// sixteen modes are most of what a macroblock writes there.
    Whole,
    /// Averaging intra prediction in a keyframe and no motion in an inter
    /// frame, and no skip flag.
    Cheapest,
}

/// Which of a macroblock's blocks a step reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Blocks {
    /// The luma blocks and the luma DC block.
    Luma,
    /// The chroma blocks.
    Chroma,
    /// Every block.
    All,
}

impl Blocks {
    /// Whether the block at `index` among a macroblock's is one of these.
    fn holds(self, index: usize) -> bool {
        let chroma = (BLUE_BLOCKS..Y2_BLOCK).contains(&index);
        match self {
            Self::Luma => !chroma,
            Self::Chroma => chroma,
            Self::All => true,
        }
    }
}

/// Where a macroblock lies: its column and row, where its luma begins in the
/// frame, which its motion is searched from, where its luma and its chroma
/// begin in the band it is coded in, and whether a macroblock row lies above
/// it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Site {
    column: usize,
    row: usize,
    frame: (isize, isize),
    luma: (isize, isize),
    chroma: (isize, isize),
    has_above: bool,
}

impl Site {
    /// The macroblock in `column` of `row`.
    fn of(column: usize, row: usize) -> Self {
        let x = signed(column * 16);
        let y = signed(BAND_TOP);
        Self {
            column,
            row,
            frame: (x, signed(row * 16)),
            luma: (x, y),
            chroma: (x / 2, y / 2),
            has_above: row > 0,
        }
    }
}

/// A picture `encode` was handed, checked to hold its stream's size.
#[derive(Copy, Clone)]
struct Picture<'a> {
    rgba: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> Picture<'a> {
    /// `rgba` as `geometry`'s picture, its rows `stride` bytes apart.
    fn new(rgba: &'a [u8], stride: usize, geometry: Geometry) -> Result<Self, Vp8Error> {
        let width = usize::try_from(geometry.width).map_err(|_| Vp8Error::InvalidGeometry)?;
        let height = usize::try_from(geometry.height).map_err(|_| Vp8Error::InvalidGeometry)?;
        let line = width
            .checked_mul(size_of::<Rgba8>())
            .ok_or(Vp8Error::InvalidGeometry)?;
        let needed = (height - 1)
            .checked_mul(stride)
            .and_then(|rows| rows.checked_add(line));
        if stride < line || needed.is_none_or(|needed| needed > rgba.len()) {
            return Err(Vp8Error::InvalidGeometry);
        }
        Ok(Self {
            rgba,
            stride,
            width,
            height,
        })
    }

    /// Row `y` of the picture, a row past its bottom edge its last.
    fn row(&self, y: usize) -> &[u8] {
        let at = y.min(self.height - 1) * self.stride;
        self.rgba
            .get(at..at + self.width * size_of::<Rgba8>())
            .unwrap_or_default()
    }

    /// The red, green and blue of pixel `x` of `row`, a pixel past the
    /// picture's right edge its last.
    fn pixel(&self, row: &[u8], x: usize) -> [u8; 3] {
        let at = x.min(self.width - 1) * size_of::<Rgba8>();
        match row.get(at..at + 3) {
            Some(&[red, green, blue]) => [red, green, blue],
            _ => [0; 3],
        }
    }

    /// The macroblock at `site` as luma and chroma samples, each chroma
    /// sample the four pixels it covers. Each pixel is read once.
    fn macroblock(&self, site: Site) -> Source {
        let (x0, y0) = (site.column * 16, site.row * 16);
        let mut source = Source {
            luma: [0; 256],
            blue: [0; 64],
            red: [0; 64],
        };
        let mut sums = [[0u32; 3]; 64];
        for (y, line) in source.luma.as_chunks_mut::<16>().0.iter_mut().enumerate() {
            let row = self.row(y0 + y);
            for (x, sample) in line.iter_mut().enumerate() {
                let pixel = self.pixel(row, x0 + x);
                *sample = colour::luma(pixel);
                if let Some(sum) = sums.get_mut((y / 2) * 8 + x / 2) {
                    for (sum, channel) in sum.iter_mut().zip(pixel) {
                        *sum += u32::from(channel);
                    }
                }
            }
        }
        let chroma = source.blue.iter_mut().zip(source.red.iter_mut());
        for ((blue, red), &sums) in chroma.zip(&sums) {
            [*blue, *red] = colour::chroma(sums);
        }
        source
    }
}

/// One macroblock of the picture being coded: its luma's sixteen rows of
/// sixteen samples, and each chroma plane's eight of eight.
struct Source {
    luma: [u8; 256],
    blue: [u8; 64],
    red: [u8; 64],
}

/// A prediction a macroblock is weighed by: how it predicts, its quantised
/// residual, and what it costs.
struct Candidate {
    prediction: Prediction,
    levels: Levels,
    cost: u64,
}

/// How many of a frame's macroblocks skip their residual and how many code
/// one, and how many predict from their own frame and how many from the last.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Tally {
    skipped: u32,
    coded: u32,
    intra: u32,
    inter: u32,
}

impl Tally {
    /// Count `other`'s counts in.
    fn join(&mut self, other: &Self) {
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.coded = self.coded.saturating_add(other.coded);
        self.intra = self.intra.saturating_add(other.intra);
        self.inter = self.inter.saturating_add(other.inter);
    }

    /// Count `decided` in.
    fn add(&mut self, decided: &MbInfo) {
        let skip = if decided.skip {
            &mut self.skipped
        } else {
            &mut self.coded
        };
        *skip = skip.saturating_add(1);
        let source = match decided.prediction {
            Prediction::Intra { .. } => &mut self.intra,
            Prediction::Inter { .. } => &mut self.inter,
        };
        *source = source.saturating_add(1);
    }
}

/// What a frame's two passes carry from macroblock to macroblock besides its
/// pictures.
struct Coding {
    columns: usize,
    rows: usize,
    /// Each macroblock's decisions, quantised residual and token layout, kept
    /// for the second pass to write.
    decisions: Vec<MbInfo>,
    levels: Vec<Levels>,
    shapes: Vec<Shape>,
    /// Whether a changed region touches each macroblock.
    touched: Vec<bool>,
    /// Whether each macroblock row holds no macroblock a change touches.
    still: Vec<bool>,
    /// The token contexts each macroblock leaves the one below it, which the
    /// second pass begins each row against.
    contexts: Vec<Nonzero>,
    /// What each macroblock leaves the row below, unfiltered and filtered.
    borders: Vec<Published<Border>>,
    tiles: Vec<Published<Tile>>,
    lanes: Vec<Lane>,
    /// The subblock-mode contexts a keyframe's modes are written against.
    intra: IntraContexts,
    partitions: Vec<Partition>,
    counts: Box<Counts>,
    tally: Tally,
}

impl Coding {
    /// What the passes over a frame of `geometry`'s macroblocks work in, on
    /// at most `lanes` lanes.
    fn new(geometry: Geometry, lanes: usize) -> Result<Self, Vp8Error> {
        let Geometry { columns, rows, .. } = geometry;
        let macroblocks = columns * rows;
        let refused = Vp8Error::OutOfMemory;
        let room = partition_room(geometry);
        Ok(Self {
            columns,
            rows,
            decisions: fallible::filled(macroblocks, STILL).ok_or(refused)?,
            levels: fallible::filled(macroblocks, NO_LEVELS).ok_or(refused)?,
            shapes: fallible::filled(macroblocks, Shape::default()).ok_or(refused)?,
            touched: fallible::filled(macroblocks, false).ok_or(refused)?,
            still: fallible::filled(rows, false).ok_or(refused)?,
            contexts: fallible::filled(macroblocks, Nonzero::default()).ok_or(refused)?,
            borders: slots(macroblocks).ok_or(refused)?,
            tiles: slots(macroblocks).ok_or(refused)?,
            lanes: made(lanes, || Lane::new(geometry))?,
            intra: IntraContexts::new(columns)?,
            partitions: made(MAX_PARTITIONS, || Partition::new(columns, room))?,
            counts: fallible::boxed_array(NO_COUNTS).ok_or(refused)?,
            tally: Tally::default(),
        })
    }

    /// The bytes [`Coding::new`] holds for `geometry` and `lanes`.
    fn peak_bytes(geometry: Geometry, lanes: usize) -> Option<usize> {
        let Geometry { columns, rows, .. } = geometry;
        let macroblocks = columns.checked_mul(rows)?;
        let per_macroblock = size_of::<MbInfo>()
            + size_of::<Levels>()
            + size_of::<Shape>()
            + size_of::<bool>()
            + size_of::<Nonzero>()
            + size_of::<Published<Border>>()
            + size_of::<Published<Tile>>();
        let lane = size_of::<Lane>().checked_add(Lane::peak_bytes(geometry)?)?;
        let partition = size_of::<Partition>()
            .checked_add(partition_room(geometry))?
            .checked_add(columns.checked_mul(size_of::<Nonzero>())?)?;
        [
            macroblocks.checked_mul(per_macroblock),
            rows.checked_mul(size_of::<bool>()),
            lanes.checked_mul(lane),
            columns.checked_mul(4),
            MAX_PARTITIONS.checked_mul(partition),
            Some(size_of::<Counts>()),
        ]
        .into_iter()
        .try_fold(0usize, |total, part| total.checked_add(part?))
    }

    /// Begin a frame's first pass on `runner`, answering how many lanes it
    /// codes on.
    fn start(&mut self, runner: &dyn JobRunner) -> usize {
        for slot in &mut self.borders {
            slot.reset();
        }
        for slot in &mut self.tiles {
            slot.reset();
        }
        let used = runner.width().clamp(1, self.lanes.len().max(1));
        for lane in self.lanes.iter_mut().take(used) {
            lane.start();
        }
        used
    }

    /// Mark the macroblocks `changed` touches, each region held to
    /// `geometry`'s picture, and the rows it leaves wholly alone.
    fn touch(&mut self, changed: &[Changed], geometry: Geometry) {
        self.touched.fill(false);
        let index = |at: u32| usize::try_from(at).unwrap_or(usize::MAX);
        for region in changed {
            let right = region.x.saturating_add(region.width).min(geometry.width);
            let bottom = region.y.saturating_add(region.height).min(geometry.height);
            if region.x >= right || region.y >= bottom {
                continue;
            }
            let (left, after) = (index(region.x / 16), index(right.div_ceil(16)));
            for row in index(region.y / 16)..index(bottom.div_ceil(16)) {
                let start = row * self.columns;
                if let Some(line) = self.touched.get_mut(start + left..start + after) {
                    line.fill(true);
                }
            }
        }
        let rows = self.touched.chunks(self.columns.max(1));
        for (still, row) in self.still.iter_mut().zip(rows) {
            *still = !row.contains(&true);
        }
    }

    /// Join what the first `used` lanes counted, answering what a macroblock
    /// they coded refused, if one did.
    fn gather(&mut self, used: usize) -> Result<(), Vp8Error> {
        let counts = flat_counts(&mut self.counts);
        counts.fill([0; 2]);
        let mut tally = Tally::default();
        let mut failed = None;
        for lane in self.lanes.iter_mut().take(used) {
            for (sum, &[zeros, ones]) in counts.iter_mut().zip(&*flat_counts(&mut lane.counts)) {
                sum[0] = sum[0].saturating_add(zeros);
                sum[1] = sum[1].saturating_add(ones);
            }
            tally.join(&lane.tally);
            failed = failed.or(lane.failed);
        }
        self.tally = tally;
        failed.map_or(Ok(()), Err)
    }

    /// Set `header`'s probabilities for the frame the first pass counted:
    /// each token probability the counts make cheaper to replace than keep,
    /// and the odds of skipping and of intra prediction as counted; and the
    /// token partitions the frame's coded macroblocks are worth.
    fn choose(&self, header: &mut Header) {
        header.probs.coeff = updated(&self.counts);
        let Tally {
            skipped,
            coded,
            intra,
            inter,
        } = self.tally;
        if let Some(probability) = &mut header.skip_probability {
            *probability = odds(coded, skipped);
        }
        if let Some(fields) = &mut header.inter {
            fields.prob_intra = odds(intra, inter);
        }
        let partitioned = coded >= PARTITIONED_MACROBLOCKS && self.rows >= MAX_PARTITIONS;
        header.partitions = if partitioned { MAX_PARTITIONS } else { 1 };
    }
}

/// `count` empty slots.
fn slots<T>(count: usize) -> Option<Vec<Published<T>>> {
    fallible::collected(count, (0..count).map(|_| Published::new()))
}

/// `count` of what `make` makes, or the first refusal.
fn made<T>(
    count: usize,
    mut make: impl FnMut() -> Result<T, Vp8Error>,
) -> Result<Vec<T>, Vp8Error> {
    let mut all = Vec::new();
    if !fallible::reserve(&mut all, count) {
        return Err(Vp8Error::OutOfMemory);
    }
    for _ in 0..count {
        all.push(make()?);
    }
    Ok(all)
}

impl Analysis<'_> {
    /// Code macroblock row `row` in `lane`'s band into `work`, two
    /// macroblocks behind the row above and filtering one behind itself,
    /// publishing what each macroblock leaves the row below as it goes.
    fn code_row(&self, lane: &mut Lane, row: usize, mut work: RowWork<'_>, front: &Front<'_>) {
        if front.lends(row) {
            Self::lend_row(lane, row, &mut work, front);
            return;
        }
        let columns = front.columns;
        lane.begin(row);
        let above = row.checked_sub(1);
        if let Some(above) = above {
            lane.lay_border(0, &front.border(above, 0), columns == 1);
        }
        for step in 0..=columns {
            if step < columns {
                if let Some(above) = above {
                    let next = step + 1;
                    if next < columns {
                        lane.lay_border(next, &front.border(above, next), next + 1 == columns);
                    }
                    lane.lend(step, &front.border(above, step));
                }
                let site = Site::of(step, row);
                let touched = front.touched.get(row * columns + step).copied();
                match self.macroblock(lane, &mut work, touched.unwrap_or(false), site) {
                    Ok(decided) => front.publish_border(row, step, lane.border(step, decided)),
                    Err(refused) => {
                        lane.failed.get_or_insert(refused);
                        front.publish_border(row, step, lane.border(step, STILL));
                    }
                }
            }
            // The macroblock behind is filtered once the one beside it is
            // coded, which predicts from its unfiltered right-hand column, and
            // the row above's filter has left the samples above it.
            if let Some(column) = step.checked_sub(1) {
                if let Some(above) = above {
                    lane.lay_tile(column, &front.tile(above, column));
                }
                let edge = lane.edges.get(column).copied().unwrap_or_default();
                let rows = (signed(BAND_TOP), signed(BAND_TOP / 2));
                filter_macroblock(
                    &mut lane.band,
                    self.frame.header,
                    edge,
                    (column, rows),
                    above.is_some(),
                );
                if let Some(settled) = column.checked_sub(1) {
                    front.publish_tile(row, settled, Tile::of(&lane.band, BAND_TOP, settled));
                }
                if column + 1 == columns {
                    front.publish_tile(row, column, Tile::of(&lane.band, BAND_TOP, column));
                }
            }
        }
        lane.settle(row, front.rows, &mut work);
    }

    /// Code row `row`, which no change touches, as the last frame lends it:
    /// each macroblock skipped and unmoved, and its samples the last frame's
    /// but those the row above's filter left above its top edges.
    fn lend_row(lane: &mut Lane, row: usize, work: &mut RowWork<'_>, front: &Front<'_>) {
        let (columns, rows) = (front.columns, front.rows);
        let planes = [
            (&front.last.luma, &mut *work.luma, 16),
            (&front.last.blue_chroma, &mut *work.blue, 8),
            (&front.last.red_chroma, &mut *work.red, 8),
        ];
        for (last, frame, size) in planes {
            let settled = settled(row, rows, size);
            let own = settled.start.max(row * size);
            let stride = last.stride;
            let below = frame
                .get_mut((own - settled.start) * stride..)
                .unwrap_or_default();
            copy_into(below, last.rows(own..settled.end));
        }
        if let Some(above) = row.checked_sub(1) {
            let moved = EDGE_REACH - EDGE_MOVES..EDGE_REACH;
            let strides = (front.last.luma.stride, front.last.blue_chroma.stride);
            for column in 0..columns {
                let tile = front.tile(above, column);
                let luma = tile.luma.get(moved.clone()).unwrap_or_default();
                let blue = tile.blue.get(moved.clone()).unwrap_or_default();
                let red = tile.red.get(moved.clone()).unwrap_or_default();
                lay_rows(work.luma, luma, strides.0, column);
                lay_rows(work.blue, blue, strides.1, column);
                lay_rows(work.red, red, strides.1, column);
            }
        }
        work.decisions.fill(STILL);
        work.shapes.fill(Shape {
            skip: true,
            has_y2: STILL.has_y2(),
        });
        work.contexts.fill(Nonzero::default());
        for _ in 0..columns {
            lane.tally.add(&STILL);
        }
    }

    /// Decide, quantise and reconstruct the macroblock at `site` in `lane`'s
    /// band, `touched` saying whether a change reached it, counting its
    /// tokens; answers what it was decided as.
    fn macroblock(
        &self,
        lane: &mut Lane,
        work: &mut RowWork<'_>,
        touched: bool,
        site: Site,
    ) -> Result<MbInfo, Vp8Error> {
        let column = site.column;
        let keyframe = self.frame.header.inter.is_none();
        let Lane {
            band,
            tokens,
            intra,
            above,
            edges,
            counts,
            tally,
            ..
        } = lane;
        let (prediction, empty) = if keyframe || touched {
            let source = self.picture.macroblock(site);
            let around = Around {
                tokens,
                intra,
                above: if site.has_above { above } else { &[] },
                row: work.decisions,
            };
            let chosen = self.decide(&around, band, site, &source);
            let empty = chosen.levels.iter().flatten().all(|&level| level == 0);
            if let Some(slot) = work.levels.get_mut(column) {
                *slot = chosen.levels;
            }
            (chosen.prediction, empty)
        } else {
            // Only a frame with no skip flag codes an unexamined macroblock's
            // levels, and it codes none.
            let unskipped = self.modes == Modes::Cheapest;
            if let Some(slot) = work.levels.get_mut(column).filter(|_| unskipped) {
                *slot = NO_LEVELS;
            }
            (STILL.prediction, true)
        };
        let decided = MbInfo {
            segment: 0,
            skip: empty && self.modes != Modes::Cheapest,
            prediction,
        };
        let has_y2 = decided.has_y2();
        let levels = work.levels.get(column);
        let coded = if decided.skip {
            tokens.skip(column, has_y2);
            false
        } else {
            count(tokens, counts, levels, column, has_y2)
        };
        let mut coeffs = match levels {
            Some(levels) if coded => dequantised(levels, Blocks::All, has_y2, &self.factor),
            _ => NO_LEVELS,
        };
        let at = ((column, site.row), site.luma.1);
        let filtered = self
            .frame
            .reconstruct(&decided, coded, at, &mut coeffs, band)?;
        if let Some(slot) = edges.get_mut(column) {
            *slot = filtered;
        }
        tally.add(&decided);
        if let (true, Prediction::Intra { bmodes, .. }) = (keyframe, &decided.prediction) {
            intra.keep(column, bmodes);
        }
        if let Some(slot) = work.decisions.get_mut(column) {
            *slot = decided;
        }
        if let Some(slot) = work.shapes.get_mut(column) {
            *slot = Shape {
                skip: decided.skip,
                has_y2,
            };
        }
        Ok(decided)
    }
}

/// Lay `lines`, rows of macroblock `column` `N` samples wide, along the
/// first rows of `frame`, whose rows are `stride` bytes apart and lead with
/// the border column.
fn lay_rows<const N: usize>(frame: &mut [u8], lines: &[[u8; N]], stride: usize, column: usize) {
    for (y, line) in lines.iter().enumerate() {
        let at = y * stride + 1 + column * N;
        copy_into(frame.get_mut(at..).unwrap_or_default(), line);
    }
}

/// Count the tokens `levels` codes for the macroblock in `column` into
/// `counts`, carrying `tokens` on; answers whether any went past its first
/// position.
fn count(
    tokens: &mut Tokens,
    counts: &mut Counts,
    levels: Option<&Levels>,
    column: usize,
    has_y2: bool,
) -> bool {
    let Some(levels) = levels else {
        return false;
    };
    tokens.each_block(column, has_y2, |at| match counts.get_mut(at.plane) {
        Some(plane) => levels
            .get(at.index)
            .is_some_and(|block| code_block(&mut Counted(plane), at.context, at.first, block)),
        None => false,
    })
}

/// What each macroblock of a frame's first pass is decided against.
struct Analysis<'a> {
    picture: Picture<'a>,
    frame: Against<'a, 'a>,
    /// The frame's dequantisation factors, its one segment's, and the
    /// quantiser steps they are.
    factor: Dequant,
    steps: Steps,
    /// What a bit is worth against squared error, and against a motion
    /// search's absolute error.
    lambda: u64,
    motion_lambda: u64,
    /// The least an intra macroblock of an inter frame can cost.
    intra_floor: u64,
    modes: Modes,
    /// The frame's macroblock rows, which bound the vectors a row is lent.
    rows: usize,
}

impl Analysis<'_> {
    /// The prediction the macroblock at `site` costs least by, its residual
    /// quantised.
    fn decide(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
    ) -> Candidate {
        let Some(inter) = &self.frame.header.inter else {
            let costs = ModeCosts::Keyframe {
                contexts: around.intra,
                column: site.column,
            };
            return self.intra(around, target, site, source, &costs);
        };
        let lent = around.lending(site, self.rows, &inter.buffers);
        let moved = self.moved(around, target, site, source, &lent);
        // Intra prediction could only spend bits on error the quantiser
        // already discards where the motion leaves no residual.
        let exact = moved.levels.iter().flatten().all(|&level| level == 0);
        if self.modes == Modes::Cheapest || exact || moved.cost <= self.intra_floor {
            return moved;
        }
        let intra = self.intra(around, target, site, source, &ModeCosts::Inter);
        if intra.cost < moved.cost {
            intra
        } else {
            moved
        }
    }

    /// The intra prediction of the macroblock at `site` that costs least,
    /// `costs` pricing its modes, its residual quantised.
    fn intra(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        costs: &ModeCosts<'_>,
    ) -> Candidate {
        let modes = if self.modes == Modes::Cheapest {
            DC_PRED..=DC_PRED
        } else {
            DC_PRED..=TM_PRED
        };
        let mut levels = NO_LEVELS;
        let named = (costs, modes.clone());
        let (whole, whole_cost) = self.whole(around, target, site, source, named, &mut levels);
        let mut split = NO_LEVELS;
        let subblocks = costs
            .subblocks()
            .filter(|_| {
                let floor = self.lambda.saturating_mul(LEAST_KEY_SUBBLOCK_BITS);
                self.modes == Modes::Chosen && whole_cost > floor
            })
            .map(|subblocks| self.subblocks(around, target, site, source, &subblocks, &mut split))
            .filter(|&(_, spent)| spent < whole_cost);
        let (ymode, bmodes, luma_cost) = match subblocks {
            Some((bmodes, spent)) => {
                levels = split;
                (B_PRED, bmodes, spent)
            }
            None => (whole, costs.implied(whole), whole_cost),
        };
        let named = (costs, modes);
        let (uvmode, chroma_cost) = self.chroma(around, target, site, source, named, &mut levels);
        Candidate {
            prediction: Prediction::Intra {
                ymode,
                uvmode,
                bmodes,
            },
            levels,
            cost: luma_cost.saturating_add(chroma_cost),
        }
    }

    /// The whole-macroblock luma mode of `modes` that predicts `site` best
    /// for what naming it costs, its prediction reconstructed in `target` and
    /// its residual quantised into `levels`; and what it costs.
    fn whole(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        (costs, modes): (&ModeCosts<'_>, RangeInclusive<u8>),
        levels: &mut Levels,
    ) -> (u8, u64) {
        let (x0, y0) = site.luma;
        let (mode, _) = cheapest(modes, |mode| {
            predict_block(&mut target.luma, x0, y0, 16, mode, site.has_above);
            let missed = luma_error(&target.luma, site, source);
            weighed(missed, costs.ymode(mode), self.lambda)
        });
        predict_block(&mut target.luma, x0, y0, 16, mode, site.has_above);
        luma_levels(&target.luma, site.luma, &source.luma, &self.steps, levels);
        let mut coeffs = dequantised(levels, Blocks::Luma, true, &self.factor);
        target.add_macroblock_residual(site.luma, true, &mut coeffs);
        let tokens = rate(around.tokens, site.column, true, levels, Blocks::Luma);
        let missed = luma_error(&target.luma, site, source);
        (
            mode,
            weighed(missed, costs.ymode(mode) + tokens, self.lambda),
        )
    }

    /// Predict the macroblock at `site` a subblock at a time, each by the
    /// mode that predicts it best for what naming it costs and reconstructed
    /// in place before the next predicts from it, its luma's residual
    /// quantised into `levels`; its modes, and what they cost.
    fn subblocks(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        costs: &Subblocks<'_>,
        levels: &mut Levels,
    ) -> ([u8; BLOCK_COEFFS], u64) {
        let edge = target.above_right(site.luma);
        let factors = (self.factor.y_dc, self.factor.y_ac);
        let steps = (self.steps.y_dc, self.steps.y_ac);
        let mut bmodes = [B_DC_PRED; BLOCK_COEFFS];
        let mut bits = KEY_SUBBLOCK_YMODE;
        for (index, block) in levels.iter_mut().enumerate().take(LUMA_BLOCKS) {
            let at = subblock_origin(site.luma, index);
            let wanted = (&source.luma[..], subblock_start(index), 16);
            let (mode, _) = cheapest(B_DC_PRED..=B_HU_PRED, |mode| {
                target.predict_luma_subblock(site.luma, index, mode, edge);
                let missed = error(&target.luma, at, 4, wanted);
                weighed(missed, costs.bmode(&bmodes, index, mode), self.lambda)
            });
            bmodes[index] = mode;
            bits += costs.bmode(&bmodes, index, mode);
            target.predict_luma_subblock(site.luma, index, mode, edge);
            quantised(
                &forward_dct(&residual(&target.luma, at, wanted)),
                steps,
                0,
                block,
            );
            let coded = dequantised_block(block, 0, factors);
            add_residual(&mut target.luma, at.0, at.1, &coded);
        }
        bits += rate(around.tokens, site.column, false, levels, Blocks::Luma);
        let missed = luma_error(&target.luma, site, source);
        (bmodes, weighed(missed, bits, self.lambda))
    }

    /// The chroma mode of `modes` that predicts `site` best for what naming
    /// it costs, its prediction reconstructed in `target` and its residual
    /// quantised into `levels`; and what it costs.
    fn chroma(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        (costs, modes): (&ModeCosts<'_>, RangeInclusive<u8>),
        levels: &mut Levels,
    ) -> (u8, u64) {
        let (mode, _) = cheapest(modes, |mode| {
            predict_chroma(target, site, mode);
            let missed = chroma_error(target, site, source);
            weighed(missed, costs.uvmode(mode), self.lambda)
        });
        predict_chroma(target, site, mode);
        let named = weighed(0, costs.uvmode(mode), self.lambda);
        let residual = self.chroma_residual(around, target, site, source, levels);
        (mode, named.saturating_add(residual))
    }

    /// Quantise the chroma residual of `site`, as `target` predicts it, into
    /// `levels` and reconstruct it in place; what it costs.
    fn chroma_residual(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        levels: &mut Levels,
    ) -> u64 {
        chroma_levels(target, site.chroma, source, &self.steps, levels);
        let coeffs = dequantised(levels, Blocks::Chroma, false, &self.factor);
        target.add_chroma_residual(site.chroma, &coeffs);
        let tokens = rate(around.tokens, site.column, false, levels, Blocks::Chroma);
        weighed(chroma_error(target, site, source), tokens, self.lambda)
    }

    /// The prediction of the macroblock at `site` from the last frame, by the
    /// vector the search finds, its residual quantised.
    fn moved(
        &self,
        around: &Around<'_>,
        target: &mut Planes,
        site: Site,
        source: &Source,
        lent: &Lent,
    ) -> Candidate {
        let (vector, mode) = self.search(site, source, lent);
        self.predict_moved(target, site, vector);
        let mut levels = NO_LEVELS;
        luma_levels(
            &target.luma,
            site.luma,
            &source.luma,
            &self.steps,
            &mut levels,
        );
        let mut coeffs = dequantised(&levels, Blocks::Luma, true, &self.factor);
        target.add_macroblock_residual(site.luma, true, &mut coeffs);
        let tokens = rate(around.tokens, site.column, true, &levels, Blocks::Luma);
        let missed = luma_error(&target.luma, site, source);
        let luma = weighed(missed, inter_bits(mode, vector, lent) + tokens, self.lambda);
        let chroma = self.chroma_residual(around, target, site, source, &mut levels);
        Candidate {
            prediction: Prediction::Inter {
                reference: Buffer::Last,
                mode,
                motion: Motion::Whole(vector),
            },
            levels,
            cost: luma.saturating_add(chroma),
        }
    }

    /// The whole-pixel vector into the last frame that predicts `site`'s luma
    /// best for what coding it costs, searched from no motion and from the
    /// vectors its neighbours lend a step at a time; and the mode that names
    /// it.
    fn search(&self, site: Site, source: &Source, lent: &Lent) -> (Mv, InterMode) {
        let references = self
            .frame
            .references
            .filter(|_| self.modes != Modes::Cheapest);
        let Some(views) = references else {
            return (ZERO, InterMode::Zero);
        };
        let (x0, y0) = site.frame;
        let mut edge = EDGE_SCRATCH;
        let mut price = |vector: Mv| {
            let at = (
                x0 + isize::from(vector.x >> 3),
                y0 + isize::from(vector.y >> 3),
            );
            let missed = luma_sad(&views.last[0], at, &source.luma, &mut edge);
            let bits = inter_bits(mode_of(vector, lent), vector, lent);
            (weighed(u64::from(missed), bits, self.motion_lambda), missed)
        };
        let named = |vector: Mv| [ZERO, lent.nearest, lent.near].contains(&vector);
        let allowed = |vector: Mv| {
            lent.reach.holds(vector) && (named(vector) || coded_anew(vector, lent.best))
        };
        let (spent, missed) = price(ZERO);
        let mut best = (ZERO, spent, missed);
        for start in [lent.nearest, lent.near, lent.best] {
            if allowed(start) {
                let (spent, missed) = price(start);
                if spent < best.1 {
                    best = (start, spent, missed);
                }
            }
        }
        // A start that matches exactly is not improved on by moving further,
        // which only codes a longer vector.
        'search: for step in SEARCH_STEPS {
            for _ in 0..SEARCH_MOVES {
                if best.2 == 0 {
                    break 'search;
                }
                let centre = best.0;
                for (across, down) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                    let moved = moved_by(centre, across, down).filter(|&vector| allowed(vector));
                    let Some(vector) = moved else {
                        continue;
                    };
                    let (spent, missed) = price(vector);
                    if spent < best.1 {
                        best = (vector, spent, missed);
                    }
                }
                if best.0 == centre {
                    break;
                }
            }
        }
        (best.0, mode_of(best.0, lent))
    }

    /// Predict the macroblock at `site` of `target` from the last frame,
    /// moved by `vector`.
    fn predict_moved(&self, target: &mut Planes, site: Site, vector: Mv) {
        let Some(views) = self.frame.references else {
            return;
        };
        let planes = [
            &mut target.luma,
            &mut target.blue_chroma,
            &mut target.red_chroma,
        ];
        let motion = Motion::Whole(vector);
        let at = (site.column, site.row);
        predict_inter(
            &views.last,
            &motion,
            self.frame.interpolation,
            at,
            planes,
            site.luma.1,
        );
    }
}

/// What naming an intra macroblock's modes costs: a keyframe's, coded against
/// its neighbours' subblock modes, or an inter frame's, coded with the
/// frame's fixed probabilities after the decision that it is intra at all.
/// Only a keyframe's macroblock is predicted a subblock at a time: an inter
/// frame weighs motion against the whole-block modes, which keeps most of
/// what the subblock search would save at a fraction of its cost, as a
/// recording coded as the screen changes must.
enum ModeCosts<'a> {
    Keyframe {
        contexts: &'a IntraContexts,
        column: usize,
    },
    Inter,
}

impl ModeCosts<'_> {
    /// A whole-macroblock luma mode's, subblock prediction among them.
    fn ymode(&self, mode: u8) -> u32 {
        let index = usize::from(mode);
        match self {
            Self::Keyframe { .. } => KF_YMODE_PATHS
                .get(index)
                .map_or(0, |path| path.cost(&KF_YMODE_PROBS)),
            Self::Inter => {
                let path = YMODE_PATHS.get(index);
                cost(EVEN, false) + path.map_or(0, |path| path.cost(&YMODE_PROBS))
            }
        }
    }

    /// A chroma mode's.
    fn uvmode(&self, mode: u8) -> u32 {
        let probs = match self {
            Self::Keyframe { .. } => &KF_UV_MODE_PROBS,
            Self::Inter => &UV_MODE_PROBS,
        };
        UV_MODE_PATHS
            .get(usize::from(mode))
            .map_or(0, |path| path.cost(probs))
    }

    /// What predicting a subblock at a time costs, where it is weighed.
    fn subblocks(&self) -> Option<Subblocks<'_>> {
        match self {
            Self::Keyframe { contexts, column } => Some(Subblocks {
                contexts,
                column: *column,
            }),
            Self::Inter => None,
        }
    }

    /// The subblock modes a whole-macroblock luma mode leaves, as a reader
    /// of the frame holds them.
    fn implied(&self, mode: u8) -> [u8; BLOCK_COEFFS] {
        match self {
            Self::Keyframe { .. } => implied_bmodes(mode),
            Self::Inter => [B_DC_PRED; BLOCK_COEFFS],
        }
    }
}

/// What naming a keyframe macroblock's subblock modes costs, coded against
/// its neighbours'.
struct Subblocks<'a> {
    contexts: &'a IntraContexts,
    column: usize,
}

impl Subblocks<'_> {
    /// Subblock `index`'s mode `mode`, the subblocks before it holding
    /// `bmodes`.
    fn bmode(&self, bmodes: &[u8; BLOCK_COEFFS], index: usize, mode: u8) -> u32 {
        let probs = self.contexts.bmode_probs(self.column, bmodes, index);
        BMODE_PATHS
            .get(usize::from(mode))
            .map_or(0, |path| path.cost(probs))
    }
}

/// The luma mode that says a keyframe macroblock is predicted a subblock at a
/// time.
const KEY_SUBBLOCK_YMODE: u32 = KF_YMODE_PATHS[B_PRED as usize].cost(&KF_YMODE_PROBS);

/// The least a keyframe macroblock predicted a subblock at a time can cost,
/// in 256ths of a bit: its luma mode and sixteen subblock modes, each at its
/// cheapest.
const LEAST_KEY_SUBBLOCK_BITS: u64 =
    KEY_SUBBLOCK_YMODE as u64 + LUMA_BLOCKS as u64 * CHEAPEST_KEY_SUBBLOCK as u64;

/// The mode that names an inter macroblock's `vector` against what its
/// neighbours `lent` it: no motion, the nearest or near vector they lend, or
/// a new one.
fn mode_of(vector: Mv, lent: &Lent) -> InterMode {
    if vector == ZERO {
        InterMode::Zero
    } else if vector == lent.nearest {
        InterMode::Nearest
    } else if vector == lent.near {
        InterMode::Near
    } else {
        InterMode::New
    }
}

/// Whether `vector` is one a new vector codes against `best`: each
/// component's difference within what the bitstream carries.
fn coded_anew(vector: Mv, best: Mv) -> bool {
    codable(quarters(vector.x, best.x)) && codable(quarters(vector.y, best.y))
}

/// `centre` moved `across` and `down` whole pixels, or `None` past what a
/// vector holds.
fn moved_by(centre: Mv, across: i16, down: i16) -> Option<Mv> {
    Some(Mv {
        x: centre.x.checked_add(across.checked_mul(EIGHTHS)?)?,
        y: centre.y.checked_add(down.checked_mul(EIGHTHS)?)?,
    })
}

/// What naming an inter macroblock costs — that it is inter and from the
/// last frame, its `mode`, and a new `vector` — against what its neighbours
/// `lent` it, in 256ths of a bit.
fn inter_bits(mode: InterMode, vector: Mv, lent: &Lent) -> u32 {
    let tree = MV_REF_PATHS
        .get(usize::from(mode.code()))
        .map_or(0, |path| path.cost(&lent.probs));
    let mut spent = Cost(cost(EVEN, true) + cost(FROM_LAST, false) + tree);
    if mode == InterMode::New {
        let delta = (
            quarters(vector.y, lent.best.y),
            quarters(vector.x, lent.best.x),
        );
        write_mv(&mut spent, &DEFAULT_MV_PROBS, delta);
    }
    spent.0
}

/// The squared difference between `plane`'s `side`×`side` samples from `at`
/// and `source`'s from `start`, rows `stride` apart.
fn error(
    plane: &Plane,
    at: (isize, isize),
    side: usize,
    (source, start, stride): (&[u8], usize, usize),
) -> u64 {
    let made = plane
        .samples
        .get(plane.at(at.0, at.1)..)
        .unwrap_or_default();
    let wanted = source.get(start..).unwrap_or_default();
    let rows = made
        .chunks(plane.stride)
        .zip(wanted.chunks(stride))
        .take(side);
    rows.map(|(made, wanted)| {
        let pairs = made.iter().zip(wanted).take(side);
        // A row of sixteen squared differences of samples fits in 32 bits.
        let row: u32 = pairs
            .map(|(&made, &wanted)| u32::from(made.abs_diff(wanted)).pow(2))
            .sum();
        u64::from(row)
    })
    .sum()
}

/// What the luma of the macroblock at `site` of `luma` misses `source` by.
fn luma_error(luma: &Plane, site: Site, source: &Source) -> u64 {
    error(luma, site.luma, 16, (&source.luma[..], 0, 16))
}

/// What both chroma planes of the macroblock at `site` of `target` miss
/// `source` by.
fn chroma_error(target: &Planes, site: Site, source: &Source) -> u64 {
    let blue = error(
        &target.blue_chroma,
        site.chroma,
        8,
        (&source.blue[..], 0, 8),
    );
    blue + error(&target.red_chroma, site.chroma, 8, (&source.red[..], 0, 8))
}

/// Predict both chroma planes of the macroblock at `site` of `target` by
/// `mode`.
fn predict_chroma(target: &mut Planes, site: Site, mode: u8) {
    let (x, y) = site.chroma;
    predict_block(&mut target.blue_chroma, x, y, 8, mode, site.has_above);
    predict_block(&mut target.red_chroma, x, y, 8, mode, site.has_above);
}

/// Where luma subblock `index` of a macroblock begins among its source's
/// samples, sixteen to a row.
const fn subblock_start(index: usize) -> usize {
    (index / 4) * 4 * 16 + (index % 4) * 4
}

/// The residual of the 4×4 block of `plane` from `at`, as predicted there,
/// against `source`'s samples from `start`, rows `stride` apart.
fn residual(
    plane: &Plane,
    at: (isize, isize),
    (source, start, stride): (&[u8], usize, usize),
) -> [i32; BLOCK_COEFFS] {
    let mut out = [0; BLOCK_COEFFS];
    let made = plane
        .samples
        .get(plane.at(at.0, at.1)..)
        .unwrap_or_default();
    let wanted = source.get(start..).unwrap_or_default();
    let rows = made.chunks(plane.stride).zip(wanted.chunks(stride));
    for (line, (made, wanted)) in out.as_chunks_mut::<4>().0.iter_mut().zip(rows) {
        for ((value, &made), &wanted) in line.iter_mut().zip(made).zip(wanted) {
            *value = i32::from(wanted) - i32::from(made);
        }
    }
    out
}

/// The quantiser steps of a frame's dequantisation factors.
struct Steps {
    y_dc: Step,
    y_ac: Step,
    y2_dc: Step,
    y2_ac: Step,
    uv_dc: Step,
    uv_ac: Step,
}

impl Steps {
    /// The steps of `factor`.
    const fn of(factor: &Dequant) -> Self {
        Self {
            y_dc: Step::new(factor.y_dc),
            y_ac: Step::new(factor.y_ac),
            y2_dc: Step::new(factor.y2_dc),
            y2_ac: Step::new(factor.y2_ac),
            uv_dc: Step::new(factor.uv_dc),
            uv_ac: Step::new(factor.uv_ac),
        }
    }
}

/// `coefficients`, in raster order, quantised into `levels` in scan order
/// from position `first`: the first position at `dc`, the rest at `ac`.
fn quantised(
    coefficients: &[i32; BLOCK_COEFFS],
    (dc, ac): (Step, Step),
    first: usize,
    levels: &mut [i16; BLOCK_COEFFS],
) {
    for (scan, (level, &position)) in levels.iter_mut().zip(&ZIGZAG).enumerate() {
        let coefficient = coefficients
            .get(usize::from(position))
            .copied()
            .unwrap_or(0);
        *level = if scan < first {
            0
        } else if scan == 0 {
            dc.level(coefficient, true)
        } else {
            ac.level(coefficient, false)
        };
    }
}

/// Quantise the luma residual of the macroblock whose luma starts at
/// `origin` of `luma`, as predicted there, into `levels`: each block's DC
/// through the luma DC block.
fn luma_levels(
    luma: &Plane,
    origin: (isize, isize),
    source: &[u8; 256],
    steps: &Steps,
    levels: &mut Levels,
) {
    let mut dc = [0; BLOCK_COEFFS];
    for (index, (block, slot)) in levels.iter_mut().zip(&mut dc).enumerate() {
        let wanted = (&source[..], subblock_start(index), 16);
        let coefficients = forward_dct(&residual(luma, subblock_origin(origin, index), wanted));
        *slot = coefficients[0];
        quantised(&coefficients, (steps.y_dc, steps.y_ac), 1, block);
    }
    let walsh = forward_walsh(&dc);
    if let Some(block) = levels.get_mut(Y2_BLOCK) {
        quantised(&walsh, (steps.y2_dc, steps.y2_ac), 0, block);
    }
}

/// Quantise both chroma residuals of the macroblock whose chroma starts at
/// `origin` of `target`, as predicted there, into `levels`.
fn chroma_levels(
    target: &Planes,
    origin: (isize, isize),
    source: &Source,
    steps: &Steps,
    levels: &mut Levels,
) {
    let planes = [
        (&target.blue_chroma, &source.blue, BLUE_BLOCKS),
        (&target.red_chroma, &source.red, RED_BLOCKS),
    ];
    for (plane, samples, blocks) in planes {
        for index in 0..4 {
            let (x, y) = ((index % 2) * 4, (index / 2) * 4);
            let x0 = origin.0 + isize::try_from(x).unwrap_or(0);
            let at = (x0, origin.1 + isize::try_from(y).unwrap_or(0));
            let coefficients = forward_dct(&residual(plane, at, (&samples[..], y * 8 + x, 8)));
            if let Some(block) = levels.get_mut(blocks + index) {
                quantised(&coefficients, (steps.uv_dc, steps.uv_ac), 0, block);
            }
        }
    }
}

/// The coefficients a decoder holds for `blocks` of `levels`, each
/// dequantised and put in place as its token reader leaves them; nought for
/// the rest.
fn dequantised(levels: &Levels, blocks: Blocks, has_y2: bool, factor: &Dequant) -> Levels {
    core::array::from_fn(|index| {
        let carried = blocks.holds(index) && (index != Y2_BLOCK || has_y2);
        match levels.get(index) {
            Some(block) if carried => {
                let first = usize::from(has_y2 && index < LUMA_BLOCKS);
                dequantised_block(block, first, factor.of_block(index))
            }
            _ => [0; BLOCK_COEFFS],
        }
    })
}

/// One block's `levels`, in scan order from position `first`, dequantised at
/// `dc` and `ac` into raster order, as the token reader leaves them.
fn dequantised_block(
    levels: &[i16; BLOCK_COEFFS],
    first: usize,
    (dc, ac): (i32, i32),
) -> [i16; BLOCK_COEFFS] {
    let mut coeffs = [0; BLOCK_COEFFS];
    for (scan, (&level, &position)) in levels.iter().zip(&ZIGZAG).enumerate().skip(first) {
        let factor = if scan == 0 { dc } else { ac };
        if let Some(slot) = coeffs.get_mut(usize::from(position)) {
            *slot = dequantise(i32::from(level), factor);
        }
    }
    coeffs
}

/// What coding `blocks` of `levels` costs, in 256ths of a bit, beside the
/// token contexts the macroblock in `column` is coded against; nothing where
/// they code nothing past their first positions, as a skipped macroblock
/// does.
fn rate(tokens: &Tokens, column: usize, has_y2: bool, levels: &Levels, blocks: Blocks) -> u32 {
    let mut spent = 0u32;
    let (_, _, coded) = walk_blocks(tokens.around(column), has_y2, |at| {
        let found = (DEFAULT_TOKEN_COSTS.get(at.plane), levels.get(at.index));
        match found {
            (Some(costs), Some(block)) if blocks.holds(at.index) => {
                let mut priced = Priced { costs, spent: 0 };
                let coded = code_block(&mut priced, at.context, at.first, block);
                spent = spent.saturating_add(priced.spent);
                coded
            }
            _ => false,
        }
    });
    if coded {
        spent
    } else {
        0
    }
}

/// The token probabilities a frame whose branches were counted `counts` codes
/// with: each default replaced where coding the frame's branches with the
/// probability they imply, and the update that carries it, costs less.
fn updated(counts: &Counts) -> CoeffProbs {
    let mut probs = DEFAULT_COEFF_PROBS;
    let counted = counts.as_flattened().as_flattened().as_flattened();
    let updates = COEFF_UPDATE_PROBS
        .as_flattened()
        .as_flattened()
        .as_flattened();
    let flat = probs
        .as_flattened_mut()
        .as_flattened_mut()
        .as_flattened_mut();
    for ((probability, &[zeros, ones]), &update) in flat.iter_mut().zip(counted).zip(updates) {
        let implied = odds(zeros, ones);
        let keeping = coded_cost(*probability, zeros, ones) + u64::from(cost(update, false));
        let replacing =
            coded_cost(implied, zeros, ones) + u64::from(cost(update, true) + UPDATE_BITS);
        if replacing < keeping {
            *probability = implied;
        }
    }
    probs
}

/// What coding `zeros` falses and `ones` trues with `probability` costs, in
/// 256ths of a bit.
fn coded_cost(probability: u8, zeros: u32, ones: u32) -> u64 {
    let falses = u64::from(zeros) * u64::from(cost(probability, false));
    falses + u64::from(ones) * u64::from(cost(probability, true))
}

/// The probability of the first of two outcomes counted `first` and `second`
/// times, held to what a header carries; even where neither was counted.
fn odds(first: u32, second: u32) -> u8 {
    let total = u64::from(first) + u64::from(second);
    if total == 0 {
        return EVEN;
    }
    let scaled = (u64::from(first) * 256 + total / 2) / total;
    u8::try_from(scaled.clamp(1, 255)).unwrap_or(EVEN)
}

/// What every decoder of the stream carries once it has read `header`: its
/// segmentation and deltas, and the default probabilities, which no frame
/// refreshes.
fn carried(header: &Header) -> StreamState {
    StreamState {
        probs: Probabilities::DEFAULT,
        segmentation: header.segmentation.clone(),
        deltas: header.deltas.clone(),
    }
}

/// Keep the partition `closed` answers in `out`; or give `out` back its
/// buffer, emptied, where a byte of the partition found no room.
fn kept(closed: Result<Vec<u8>, Vec<u8>>, out: &mut Vec<u8>) -> Result<(), Vp8Error> {
    match closed {
        Ok(bytes) => {
            *out = bytes;
            Ok(())
        }
        Err(mut bytes) => {
            bytes.clear();
            *out = bytes;
            Err(Vp8Error::OutOfMemory)
        }
    }
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;

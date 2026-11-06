//! An inter frame's macroblock syntax (RFC 6386 §16, §17 and §19.3): each
//! macroblock's segment and skip flag, then either its intra modes — read with
//! the frame's probabilities and none of the context a keyframe gives them —
//! or the reference frame it predicts from and its motion, coded against its
//! neighbours' vectors.
//!
//! A vector a neighbour lends is held to a macroblock past the frame's edges
//! before it is used or a new one is coded against it, and components add with
//! the reference decoder's 16-bit wrap.
//!
//! A writer beside the reader codes a row so that the reader reads it back:
//! the encoder writes its frames with it, and the tests their streams.

use super::motion::{Motion, Mv};
#[cfg(any(test, feature = "fixture", feature = "encode"))]
use super::writer::{write_mv, write_path, write_tree, Writer, MV_REF_PATHS};
use super::{
    Bool, Buffer, Buffers, DeltaIndex, Header, InterHeader, Probabilities, BLOCK_COEFFS,
    BMODE_PROBS, BMODE_TREE, B_DC_PRED, B_PRED, MODE_CONTEXTS, MV_IS_SHORT, MV_LONG, MV_LONG_BITS,
    MV_NEAR, MV_NEAREST, MV_NEW, MV_PROBS, MV_REF_TREE, MV_SHORT, MV_SIGN, MV_SPLIT, SMALL_MV_TREE,
    SPLITS, SPLIT_PROBS, SPLIT_TREE, SUB_MV_ABOVE, SUB_MV_LEFT, SUB_MV_NEW, SUB_MV_REF_PROBS,
    SUB_MV_REF_TREE, UV_MODE_TREE, YMODE_TREE,
};
#[cfg(any(test, feature = "fixture", feature = "encode"))]
use super::{MV_ZERO, SUB_MV_ZERO};

/// No motion.
pub(super) const ZERO: Mv = Mv { x: 0, y: 0 };

/// A macroblock's extent in [`Mv`]'s units: sixteen pixels in eighths.
const MACROBLOCK_SPAN: i32 = 128;

/// The subblock whose vector a split macroblock lends its neighbours as its
/// whole macroblock's.
const LAST_SUBBLOCK: usize = 15;

/// One macroblock of an inter frame, as its header in the first partition
/// codes it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct MbInfo {
    /// The segment its quantiser and loop-filter level come from.
    pub(super) segment: u8,
    /// Whether it codes no residual.
    pub(super) skip: bool,
    pub(super) prediction: Prediction,
}

/// How a macroblock is predicted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Prediction {
    /// From its own frame, as a keyframe's macroblocks are.
    Intra {
        /// The whole-macroblock luma mode, `B_PRED` where the subblocks carry
        /// their own.
        ymode: u8,
        uvmode: u8,
        /// Each subblock's mode where `ymode` is `B_PRED`.
        bmodes: [u8; BLOCK_COEFFS],
    },
    /// From a reference frame, moved by motion vectors.
    Inter {
        reference: Buffer,
        /// How its motion is coded.
        mode: InterMode,
        motion: Motion,
    },
}

/// How an inter macroblock's motion is coded (RFC 6386 §16.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum InterMode {
    /// The nearest of its neighbours' vectors.
    Nearest,
    /// The next nearest.
    Near,
    /// No motion.
    Zero,
    /// A vector coded against the best of its neighbours'.
    New,
    /// A vector for each part of a partitioning.
    Split(Partitioning),
}

/// How a split macroblock's subblocks share their vectors (RFC 6386 §16.4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Partitioning {
    /// The top and bottom halves, 16×8.
    TopBottom,
    /// The left and right halves, 8×16.
    LeftRight,
    /// The four quarters, 8×8.
    Quarters,
    /// Every subblock its own, 4×4.
    Each,
}

/// Each partitioning, by the code the split tree names it by.
const PARTITIONINGS: [Partitioning; 4] = [
    Partitioning::TopBottom,
    Partitioning::LeftRight,
    Partitioning::Quarters,
    Partitioning::Each,
];

impl Partitioning {
    /// The partitioning the split tree's `code` names.
    pub(super) fn of_code(code: u8) -> Self {
        PARTITIONINGS
            .get(usize::from(code))
            .copied()
            .unwrap_or(Self::Each)
    }

    /// The code the split tree names this partitioning by.
    #[cfg(any(test, feature = "fixture", feature = "encode"))]
    fn code(self) -> u8 {
        let index = PARTITIONINGS.iter().position(|&each| each == self);
        index
            .and_then(|index| u8::try_from(index).ok())
            .unwrap_or(3)
    }
}

#[cfg(any(test, feature = "fixture", feature = "encode"))]
impl InterMode {
    /// The code the mode tree names this mode by.
    pub(super) fn code(self) -> u8 {
        match self {
            Self::Nearest => MV_NEAREST,
            Self::Near => MV_NEAR,
            Self::Zero => MV_ZERO,
            Self::New => MV_NEW,
            Self::Split(_) => MV_SPLIT,
        }
    }
}

impl MbInfo {
    /// The reference and vector this macroblock lends a neighbour's motion
    /// search, `None` for an intra one.
    fn motion(&self) -> Option<(Buffer, Mv)> {
        let Prediction::Inter { reference, .. } = &self.prediction else {
            return None;
        };
        Some((*reference, self.block_mv(LAST_SUBBLOCK)))
    }

    /// The vector subblock `block` moves by, none for an intra macroblock.
    fn block_mv(&self, block: usize) -> Mv {
        let Prediction::Inter { motion, .. } = &self.prediction else {
            return ZERO;
        };
        match motion {
            Motion::Whole(mv) => *mv,
            Motion::Split(mvs) => mvs.get(block).copied().unwrap_or(ZERO),
        }
    }

    /// Whether this macroblock splits its motion.
    fn is_split(&self) -> bool {
        matches!(
            self.prediction,
            Prediction::Inter {
                motion: Motion::Split(_),
                ..
            }
        )
    }

    /// Whether it codes a luma DC block, which every macroblock does but one
    /// predicted a subblock at a time: by subblock intra modes, or split
    /// motion.
    pub(super) fn has_y2(&self) -> bool {
        match self.prediction {
            Prediction::Intra { ymode, .. } => ymode != B_PRED,
            Prediction::Inter { mode, .. } => !matches!(mode, InterMode::Split(_)),
        }
    }

    /// Where its loop-filter level reads a stream's deltas.
    pub(super) fn filter_deltas(&self) -> DeltaIndex {
        match self.prediction {
            Prediction::Intra { ymode, .. } => DeltaIndex::intra(ymode),
            Prediction::Inter {
                reference, mode, ..
            } => DeltaIndex {
                reference: match reference {
                    Buffer::Last => 1,
                    Buffer::Golden => 2,
                    Buffer::Alternate => 3,
                },
                mode: Some(match mode {
                    InterMode::Zero => 1,
                    InterMode::Nearest | InterMode::Near | InterMode::New => 2,
                    InterMode::Split(_) => 3,
                }),
            },
        }
    }
}

impl Mv {
    /// This vector plus `other`, wrapping as the reference decoder's 16-bit
    /// components do.
    fn plus(self, other: Self) -> Self {
        Self {
            x: self.x.wrapping_add(other.x),
            y: self.y.wrapping_add(other.y),
        }
    }

    /// This vector pointing the other way.
    fn reversed(self) -> Self {
        Self {
            x: self.x.wrapping_neg(),
            y: self.y.wrapping_neg(),
        }
    }

    /// This vector held within `reach`.
    fn clamped(self, reach: Reach) -> Self {
        Self {
            x: within(self.x, reach.left, reach.right),
            y: within(self.y, reach.top, reach.bottom),
        }
    }
}

#[cfg(any(test, feature = "fixture", feature = "encode"))]
impl Motion {
    /// The one vector this motion asks a whole macroblock to move by.
    fn whole(self) -> Mv {
        match self {
            Self::Whole(mv) => mv,
            Self::Split(mvs) => mvs[LAST_SUBBLOCK],
        }
    }

    /// The vector this motion asks each subblock to move by.
    fn each(self) -> [Mv; BLOCK_COEFFS] {
        match self {
            Self::Whole(mv) => [mv; BLOCK_COEFFS],
            Self::Split(mvs) => mvs,
        }
    }
}

/// `value` held between `low` and `high`, which lie either side of nought, so
/// the result never leaves the component's range.
fn within(value: i16, low: i32, high: i32) -> i16 {
    i16::try_from(i32::from(value).max(low).min(high)).unwrap_or(value)
}

/// How far a macroblock's vectors may point past each edge of the frame, in
/// [`Mv`]'s units: to the edge, and a macroblock beyond it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Reach {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Reach {
    /// The reach of the macroblock in `column` of `columns` and `row` of
    /// `rows`.
    fn new(column: usize, columns: usize, row: usize, rows: usize) -> Self {
        let span = |macroblocks: usize| {
            i32::try_from(macroblocks).map_or(i32::MAX, |macroblocks| {
                macroblocks.saturating_mul(MACROBLOCK_SPAN)
            })
        };
        Self {
            left: -span(column.saturating_add(1)),
            right: span(columns.saturating_sub(column)),
            top: -span(row.saturating_add(1)),
            bottom: span(rows.saturating_sub(row)),
        }
    }

    /// Whether `mv` points no further past the frame than this reach.
    #[cfg(any(test, feature = "encode"))]
    pub(super) fn holds(self, mv: Mv) -> bool {
        (self.left..=self.right).contains(&i32::from(mv.x))
            && (self.top..=self.bottom).contains(&i32::from(mv.y))
    }
}

/// The macroblocks a macroblock's motion is coded against, `None` where the
/// frame has none.
#[derive(Copy, Clone)]
struct Neighbours<'a> {
    above: Option<&'a MbInfo>,
    left: Option<&'a MbInfo>,
    above_left: Option<&'a MbInfo>,
}

/// What a macroblock's neighbours make of its motion (RFC 6386 §16.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Proposed {
    /// The vector a new one is coded against.
    best: Mv,
    nearest: Mv,
    near: Mv,
    /// How strongly the neighbours weigh for each step of the mode tree: no
    /// motion, the nearest vector, the near one, and a split.
    counts: [usize; 4],
}

impl Proposed {
    /// The probabilities the mode tree is read with.
    fn mode_probs(&self) -> [u8; 4] {
        core::array::from_fn(|step| {
            MODE_CONTEXTS
                .get(self.counts[step])
                .map_or(128, |probs| probs[step])
        })
    }
}

/// Search the neighbours' vectors for those a macroblock predicting from
/// `reference` is coded against: the one above and the one to the left weigh
/// two, the one above-left one, and a neighbour whose reference points the
/// other way lends its vector reversed.
fn find_near(neighbours: Neighbours<'_>, reference: Buffer, buffers: &Buffers) -> Proposed {
    let weighted = [
        (neighbours.above, 2),
        (neighbours.left, 2),
        (neighbours.above_left, 1),
    ];
    let mut vectors = [ZERO; 4];
    let mut counts = [0usize; 4];
    let mut last = 0;
    for (neighbour, weight) in weighted {
        let Some((theirs, mv)) = neighbour.and_then(MbInfo::motion) else {
            continue;
        };
        if mv == ZERO {
            counts[0] += weight;
            continue;
        }
        let mv = if buffers.sign_bias(theirs) == buffers.sign_bias(reference) {
            mv
        } else {
            mv.reversed()
        };
        if mv != vectors[last] {
            last += 1;
            vectors[last] = mv;
        }
        counts[last] += weight;
    }
    // A third distinct vector agreeing with the first strengthens it.
    if counts[3] > 0 && vectors[3] == vectors[1] {
        counts[1] += 1;
    }
    counts[3] = weighted
        .iter()
        .filter(|(neighbour, _)| neighbour.is_some_and(MbInfo::is_split))
        .map(|(_, weight)| weight)
        .sum();
    if counts[2] > counts[1] {
        counts.swap(1, 2);
        vectors.swap(1, 2);
    }
    if counts[1] >= counts[0] {
        vectors[0] = vectors[1];
    }
    Proposed {
        best: vectors[0],
        nearest: vectors[1],
        near: vectors[2],
        counts,
    }
}

/// Read one macroblock row of an inter frame's first partition: the row at
/// `index` of the frame's `rows`. `above` is the row above as this frame
/// decoded it, empty for the top row, and `row` holds, on entry, the segment
/// each macroblock had in the frame before, which it keeps where this frame
/// leaves the map be, and on return this frame's decisions.
pub(super) fn read_row(
    reader: &mut Bool<'_>,
    header: &Header,
    inter: &InterHeader,
    above: &[MbInfo],
    row: &mut [MbInfo],
    place: (usize, usize),
) {
    each_macroblock(above, row, place, |neighbours, reach, current| {
        *current = read_macroblock(reader, header, inter, neighbours, reach, current.segment);
    });
}

/// Write one macroblock row of an inter frame's first partition so that
/// [`read_row`] reads it back as `row`, against the same `above` and
/// `place`. A nearest or near vector, and a split part asking for its left or
/// above neighbour's vector or none, are what the neighbours lend, so `row`
/// takes on return the motion they make.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
pub(super) fn write_row(
    writer: &mut Writer,
    header: &Header,
    inter: &InterHeader,
    above: &[MbInfo],
    row: &mut [MbInfo],
    place: (usize, usize),
) {
    each_macroblock(above, row, place, |neighbours, reach, current| {
        write_macroblock(writer, header, inter, neighbours, reach, current);
    });
}

/// Visit each macroblock of `row`, the row at `index` of the frame's `rows`
/// below `above`, with the neighbours its motion is coded against and its
/// reach.
fn each_macroblock(
    above: &[MbInfo],
    row: &mut [MbInfo],
    (index, rows): (usize, usize),
    mut visit: impl FnMut(Neighbours<'_>, Reach, &mut MbInfo),
) {
    let columns = row.len();
    for column in 0..columns {
        let (done, rest) = row.split_at_mut(column);
        let Some(current) = rest.first_mut() else {
            return;
        };
        let reach = Reach::new(column, columns, index, rows);
        visit(neighbours(above, done, column), reach, current);
    }
}

/// The macroblocks the one in `column` codes its motion against: those of
/// `above`, the row above, and the last of `done`, its row's macroblocks
/// before it.
fn neighbours<'a>(above: &'a [MbInfo], done: &'a [MbInfo], column: usize) -> Neighbours<'a> {
    Neighbours {
        above: above.get(column),
        left: done.last(),
        above_left: column.checked_sub(1).and_then(|left| above.get(left)),
    }
}

/// What a macroblock predicting from the last frame is coded against: the
/// vectors its neighbours lend it, held to its reach as a reader holds them,
/// that reach, and the probabilities its mode is coded with.
#[cfg(any(test, feature = "encode"))]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Lent {
    /// The vector a new one is coded against.
    pub(super) best: Mv,
    pub(super) nearest: Mv,
    pub(super) near: Mv,
    pub(super) reach: Reach,
    /// The probability of each step of the mode tree.
    pub(super) probs: [u8; 4],
}

/// What its neighbours lend the macroblock in `column` of `row`, the row at
/// `index` of the frame's `rows` below `above`, predicting from the last
/// frame: `row`'s macroblocks before it as the frame has decided them.
#[cfg(any(test, feature = "encode"))]
pub(super) fn lent(
    above: &[MbInfo],
    row: &[MbInfo],
    column: usize,
    (index, rows): (usize, usize),
    buffers: &Buffers,
) -> Lent {
    let reach = Reach::new(column, row.len(), index, rows);
    let done = row.get(..column).unwrap_or_default();
    let near = find_near(neighbours(above, done, column), Buffer::Last, buffers);
    Lent {
        best: near.best.clamped(reach),
        nearest: near.nearest.clamped(reach),
        near: near.near.clamped(reach),
        reach,
        probs: near.mode_probs(),
    }
}

/// Read one macroblock's header (RFC 6386 §19.3), which keeps `previous`, its
/// segment in the frame before, where this frame leaves the map be.
fn read_macroblock(
    reader: &mut Bool<'_>,
    header: &Header,
    inter: &InterHeader,
    neighbours: Neighbours<'_>,
    reach: Reach,
    previous: u8,
) -> MbInfo {
    let segment = header.segmentation.read_id(reader).unwrap_or(previous);
    let skip = header.read_skip(reader);
    let prediction = if reader.bit(inter.prob_intra) == 0 {
        read_intra(reader, &header.probs)
    } else {
        read_inter(reader, &header.probs.mv, inter, neighbours, reach)
    };
    MbInfo {
        segment,
        skip,
        prediction,
    }
}

/// Read an intra macroblock's modes with the frame's probabilities, and none
/// of the context a keyframe's take from their neighbours (RFC 6386 §16.1).
fn read_intra(reader: &mut Bool<'_>, probs: &Probabilities) -> Prediction {
    let ymode = reader.tree(&YMODE_TREE, &probs.ymode, 0);
    let mut bmodes = [B_DC_PRED; BLOCK_COEFFS];
    if ymode == B_PRED {
        for mode in &mut bmodes {
            *mode = reader.tree(&BMODE_TREE, &BMODE_PROBS, 0);
        }
    }
    let uvmode = reader.tree(&UV_MODE_TREE, &probs.uvmode, 0);
    Prediction::Intra {
        ymode,
        uvmode,
        bmodes,
    }
}

/// Read an inter macroblock's reference frame and motion (RFC 6386 §16.2).
fn read_inter(
    reader: &mut Bool<'_>,
    probs: &[[u8; MV_PROBS]; 2],
    inter: &InterHeader,
    neighbours: Neighbours<'_>,
    reach: Reach,
) -> Prediction {
    let reference = if reader.bit(inter.prob_last) == 0 {
        Buffer::Last
    } else if reader.bit(inter.prob_golden) == 0 {
        Buffer::Golden
    } else {
        Buffer::Alternate
    };
    let near = find_near(neighbours, reference, &inter.buffers);
    let (mode, motion) = match reader.tree(&MV_REF_TREE, &near.mode_probs(), 0) {
        MV_NEAREST => (
            InterMode::Nearest,
            Motion::Whole(near.nearest.clamped(reach)),
        ),
        MV_NEAR => (InterMode::Near, Motion::Whole(near.near.clamped(reach))),
        MV_NEW => {
            let mv = read_mv(reader, probs).plus(near.best.clamped(reach));
            (InterMode::New, Motion::Whole(mv))
        }
        MV_SPLIT => {
            let best = near.best.clamped(reach);
            let (partitioning, mvs) = read_split(reader, probs, neighbours, best);
            (InterMode::Split(partitioning), Motion::Split(mvs))
        }
        _ => (InterMode::Zero, Motion::Whole(ZERO)),
    };
    Prediction::Inter {
        reference,
        mode,
        motion,
    }
}

/// Read a split macroblock's partitioning and each part's vector: its left
/// or above neighbour's, none, or a new one coded against `best`
/// (RFC 6386 §16.4).
fn read_split(
    reader: &mut Bool<'_>,
    probs: &[[u8; MV_PROBS]; 2],
    neighbours: Neighbours<'_>,
    best: Mv,
) -> (Partitioning, [Mv; BLOCK_COEFFS]) {
    let code = reader.tree(&SPLIT_TREE, &SPLIT_PROBS, 0);
    let mvs = each_part(neighbours, code, |_, left, above| {
        match reader.tree(&SUB_MV_REF_TREE, sub_mv_probs(left, above), 0) {
            SUB_MV_LEFT => left,
            SUB_MV_ABOVE => above,
            SUB_MV_NEW => read_mv(reader, probs).plus(best),
            _ => ZERO,
        }
    });
    (Partitioning::of_code(code), mvs)
}

/// Every subblock's vector in the partitioning the split tree's `code`
/// names, its parts taken in order: `vector` answers a part's from its first
/// subblock and the vectors left of and above that subblock — the
/// macroblock's own parts' where they lie inside it, its neighbours' where
/// they lie outside.
fn each_part(
    neighbours: Neighbours<'_>,
    code: u8,
    mut vector: impl FnMut(usize, Mv, Mv) -> Mv,
) -> [Mv; BLOCK_COEFFS] {
    let owners = SPLITS.get(usize::from(code)).unwrap_or(&SPLITS[3]);
    let parts = owners.iter().max().map_or(0, |&last| usize::from(last) + 1);
    let mut mvs = [ZERO; BLOCK_COEFFS];
    for part in 0..parts {
        let Some(first) = owners.iter().position(|&owner| usize::from(owner) == part) else {
            continue;
        };
        let left = if first.is_multiple_of(4) {
            neighbours.left.map_or(ZERO, |mb| mb.block_mv(first + 3))
        } else {
            mvs.get(first - 1).copied().unwrap_or(ZERO)
        };
        let above = if first < 4 {
            neighbours.above.map_or(ZERO, |mb| mb.block_mv(first + 12))
        } else {
            mvs.get(first - 4).copied().unwrap_or(ZERO)
        };
        let mv = vector(first, left, above);
        for (slot, &owner) in mvs.iter_mut().zip(owners) {
            if usize::from(owner) == part {
                *slot = mv;
            }
        }
    }
    mvs
}

/// The part-vector probabilities for a part whose left and above neighbours
/// move by `left` and `above`.
fn sub_mv_probs(left: Mv, above: Mv) -> &'static [u8; 3] {
    let context = match (left == above, above == ZERO) {
        (true, true) => 4,
        (true, false) => 3,
        (false, true) => 2,
        (false, false) => usize::from(left == ZERO),
    };
    SUB_MV_REF_PROBS
        .get(context)
        .unwrap_or(&SUB_MV_REF_PROBS[0])
}

/// One vector, its row first (RFC 6386 §17.1).
fn read_mv(reader: &mut Bool<'_>, probs: &[[u8; MV_PROBS]; 2]) -> Mv {
    let y = read_component(reader, &probs[0]);
    let x = read_component(reader, &probs[1]);
    Mv { x, y }
}

/// One vector component, doubled into [`Mv`]'s units: a magnitude under
/// eight from its tree or a larger one bit by bit, then its sign.
fn read_component(reader: &mut Bool<'_>, probs: &[u8; MV_PROBS]) -> i16 {
    let prob = |index: usize| probs.get(index).copied().unwrap_or(128);
    let magnitude = if reader.bit(prob(MV_IS_SHORT)) == 0 {
        u16::from(reader.tree(&SMALL_MV_TREE, &probs[MV_SHORT..MV_LONG], 0))
    } else {
        let mut value = 0u16;
        for bit in (0..3).chain((4..MV_LONG_BITS).rev()) {
            value |= u16::from(reader.bit(prob(MV_LONG + bit)) != 0) << bit;
        }
        // A long magnitude is at least eight, so bit three is coded only where
        // a higher bit is set and implied where none is.
        if value >> 4 == 0 || reader.bit(prob(MV_LONG + 3)) != 0 {
            value |= 8;
        }
        value
    };
    let value = i32::from(magnitude) * 2;
    let value = if magnitude != 0 && reader.bit(prob(MV_SIGN)) != 0 {
        -value
    } else {
        value
    };
    i16::try_from(value).unwrap_or(0)
}

/// Write one macroblock's header so that [`read_macroblock`] reads it back as
/// `mb`, which takes the motion its neighbours make of it.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
fn write_macroblock(
    writer: &mut Writer,
    header: &Header,
    inter: &InterHeader,
    neighbours: Neighbours<'_>,
    reach: Reach,
    mb: &mut MbInfo,
) {
    header.segmentation.write_id(writer, mb.segment);
    if let Some(probability) = header.skip_probability {
        writer.bit(probability, mb.skip);
    }
    match &mut mb.prediction {
        Prediction::Intra {
            ymode,
            uvmode,
            bmodes,
        } => {
            writer.bit(inter.prob_intra, false);
            write_tree(
                writer,
                &YMODE_TREE,
                &header.probs.ymode,
                usize::from(*ymode),
            );
            if *ymode == B_PRED {
                for &mode in &*bmodes {
                    write_tree(writer, &BMODE_TREE, &BMODE_PROBS, usize::from(mode));
                }
            }
            write_tree(
                writer,
                &UV_MODE_TREE,
                &header.probs.uvmode,
                usize::from(*uvmode),
            );
        }
        Prediction::Inter {
            reference,
            mode,
            motion,
        } => {
            writer.bit(inter.prob_intra, true);
            writer.bit(inter.prob_last, *reference != Buffer::Last);
            if *reference != Buffer::Last {
                writer.bit(inter.prob_golden, *reference == Buffer::Alternate);
            }
            let near = find_near(neighbours, *reference, &inter.buffers);
            if let Some(path) = MV_REF_PATHS.get(usize::from(mode.code())) {
                write_path(writer, path, &near.mode_probs());
            }
            let best = near.best.clamped(reach);
            let probs = &header.probs.mv;
            *motion = match *mode {
                InterMode::Nearest => Motion::Whole(near.nearest.clamped(reach)),
                InterMode::Near => Motion::Whole(near.near.clamped(reach)),
                InterMode::Zero => Motion::Whole(ZERO),
                InterMode::New => {
                    let wanted = motion.whole();
                    write_delta(writer, probs, wanted, best);
                    Motion::Whole(wanted)
                }
                InterMode::Split(partitioning) => {
                    let wanted = motion.each();
                    let mvs = write_split(writer, probs, neighbours, best, (partitioning, &wanted));
                    Motion::Split(mvs)
                }
            };
        }
    }
}

/// Write `wanted` as a new vector coded against `best`.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
fn write_delta(writer: &mut Writer, probs: &[[u8; MV_PROBS]; 2], wanted: Mv, best: Mv) {
    write_mv(
        writer,
        probs,
        (quarters(wanted.y, best.y), quarters(wanted.x, best.x)),
    );
}

/// The quarter pixels a new vector's component codes to reach `to` from
/// `from`, both in [`Mv`]'s units: half their difference. A luma vector is
/// even in those units, a reader doubling what it reads, so the half is exact.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
pub(super) fn quarters(to: i16, from: i16) -> i16 {
    i16::try_from((i32::from(to) - i32::from(from)) / 2).unwrap_or(0)
}

/// Write a split macroblock's partitioning and each part's vector so that
/// [`read_split`] reads back `wanted`, coded against `best`: a part takes its
/// left or above neighbour's vector, or none, where that is the one asked for.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
fn write_split(
    writer: &mut Writer,
    probs: &[[u8; MV_PROBS]; 2],
    neighbours: Neighbours<'_>,
    best: Mv,
    (partitioning, wanted): (Partitioning, &[Mv; BLOCK_COEFFS]),
) -> [Mv; BLOCK_COEFFS] {
    let code = partitioning.code();
    write_tree(writer, &SPLIT_TREE, &SPLIT_PROBS, usize::from(code));
    each_part(neighbours, code, |first, left, above| {
        let target = wanted.get(first).copied().unwrap_or(ZERO);
        let choice = if target == left {
            SUB_MV_LEFT
        } else if target == above {
            SUB_MV_ABOVE
        } else if target == ZERO {
            SUB_MV_ZERO
        } else {
            SUB_MV_NEW
        };
        write_tree(
            writer,
            &SUB_MV_REF_TREE,
            sub_mv_probs(left, above),
            usize::from(choice),
        );
        if choice == SUB_MV_NEW {
            write_delta(writer, probs, target, best);
        }
        target
    })
}

#[cfg(test)]
#[path = "inter_tests.rs"]
mod tests;

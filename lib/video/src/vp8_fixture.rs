//! Bitstreams the VP8 tests and the container's tests are written from.
//!
//! They are written with the encoder's own writer (the `writer` module): the
//! boolean encoder from the format's specification rather than the inverse
//! of the decoder beside it, so a round trip tests the decoder against the
//! format.
//!
//! An inter frame's modes and vectors are coded by the writer the `inter`
//! module keeps beside its reader, since a vector is coded against the near
//! search its neighbours make; its token contexts are worked out here.

use alloc::vec::Vec;

use super::inter::{write_row, InterMode, MbInfo, Partitioning, Prediction};
use super::motion::{Motion, Mv};
use super::writer::{write_block, write_header, write_tree, Writer};
use super::{
    keyframe_header, Buffer, BufferUpdate, Buffers, FilterDeltas, Header, InterHeader,
    Probabilities, Quant, Segmentation, StreamState, Tag, BMODE_TREE, B_DC_PRED, COEFF_BANDS,
    COEFF_TREE, COEFF_UPDATE_PROBS, DEFAULT_COEFF_PROBS, KF_BMODE_PROBS, KF_UV_MODE_PROBS,
    KF_YMODE_PROBS, KF_YMODE_TREE, SPLITS, UV_MODE_TREE,
};

impl Writer {
    /// The partition written, as a fixture's frame carries it.
    pub(super) fn finish(self) -> Vec<u8> {
        self.close().expect("room for a fixture's partition")
    }
}

/// The token value that ends a block, as the tree writer takes it.
const DCT_EOB: usize = super::DCT_EOB as usize;

/// The whole-macroblock luma mode that gives every subblock its own.
pub(crate) const B_PRED: usize = super::B_PRED as usize;

/// What one macroblock is coded as.
#[derive(Copy, Clone)]
pub struct Block {
    pub(crate) ymode: usize,
    pub(crate) uvmode: usize,
    /// The mode every subblock takes when the luma mode is [`B_PRED`].
    pub(crate) bmode: usize,
    /// The luma DC block's first coefficient, as a small token value.
    pub(crate) y2_dc: usize,
}

impl Block {
    /// A macroblock predicted whole: luma mode `ymode`, chroma mode `uvmode`.
    #[must_use]
    pub const fn flat(ymode: usize, uvmode: usize) -> Self {
        Self {
            ymode,
            uvmode,
            bmode: 0,
            y2_dc: 0,
        }
    }

    /// A macroblock whose sixteen subblocks each take `bmode`.
    #[must_use]
    pub const fn subblocks(bmode: usize) -> Self {
        Self {
            ymode: B_PRED,
            uvmode: 0,
            bmode,
            y2_dc: 0,
        }
    }
}

/// Write a whole one-macroblock keyframe.
#[must_use]
pub fn keyframe(width: u32, height: u32, block: Block) -> Vec<u8> {
    let mut head = Writer::new();
    head.flag(false);
    head.flag(false);
    head.flag(false);
    head.flag(false);
    head.literal(0, 6);
    head.literal(0, 3);
    head.flag(false);
    head.literal(0, 2);
    head.literal(0, 7);
    for _ in 0..5 {
        head.flag(false);
    }
    head.flag(true);
    for plane in &COEFF_UPDATE_PROBS {
        for band in plane {
            for context in band {
                for &update in context {
                    head.bit(update, false);
                }
            }
        }
    }
    head.flag(false);
    write_tree(&mut head, &KF_YMODE_TREE, &KF_YMODE_PROBS, block.ymode);
    if block.ymode == B_PRED {
        // A subblock mode is read against the modes of the subblocks above
        // and to its left, which outside the macroblock are the averaging
        // mode: so the first row's above context and the first column's
        // left context are that, and the rest is the mode being written.
        let mut above = [0usize; 4];
        for _ in 0..4 {
            let mut left = 0usize;
            for column in 0..4 {
                let table = &KF_BMODE_PROBS[above[column]][left];
                write_tree(&mut head, &BMODE_TREE, table, block.bmode);
                left = block.bmode;
                above[column] = block.bmode;
            }
        }
    }
    write_tree(&mut head, &UV_MODE_TREE, &KF_UV_MODE_PROBS, block.uvmode);
    let first_part = head.finish();

    let mut tokens = Writer::new();
    let probs = &DEFAULT_COEFF_PROBS;
    let band_at = |scan: usize| usize::from(COEFF_BANDS[scan]);
    // A subblock-predicted macroblock carries no luma DC block, so its luma
    // blocks start at coefficient zero and use their own plane.
    if block.ymode == B_PRED {
        for _ in 0..16 {
            write_tree(&mut tokens, &COEFF_TREE, &probs[3][band_at(0)][0], DCT_EOB);
        }
    } else if block.y2_dc == 0 {
        write_tree(&mut tokens, &COEFF_TREE, &probs[1][band_at(0)][0], DCT_EOB);
    } else {
        write_tree(
            &mut tokens,
            &COEFF_TREE,
            &probs[1][band_at(0)][0],
            block.y2_dc,
        );
        tokens.flag(false);
        let context = if block.y2_dc == 1 { 1 } else { 2 };
        write_tree(
            &mut tokens,
            &COEFF_TREE,
            &probs[1][band_at(1)][context],
            DCT_EOB,
        );
    }
    if block.ymode != B_PRED {
        for _ in 0..16 {
            write_tree(&mut tokens, &COEFF_TREE, &probs[0][band_at(1)][0], DCT_EOB);
        }
    }
    for _ in 0..8 {
        write_tree(&mut tokens, &COEFF_TREE, &probs[2][band_at(0)][0], DCT_EOB);
    }
    let residual = tokens.finish();
    assemble(KEYFRAME, Some((width, height)), &first_part, &[residual])
}

/// How a drawn keyframe's loop filter is set.
#[derive(Copy, Clone, Debug)]
pub struct Filter {
    /// The simple filter rather than the normal one.
    pub simple: bool,
    /// The filter level, 0 to 63.
    pub level: u32,
    /// The sharpness, 0 to 7.
    pub sharpness: u32,
}

/// A `width`×`height` keyframe with its loop filter set as `filter` says,
/// coded over `partitions` residual partitions (one, two, four or eight), and
/// whose modes and residuals are drawn from `word`, a source of 64-bit words.
///
/// Not a picture anyone encoded, but a bitstream any decoder of the format
/// reads the same way: every partition carries far more than its macroblocks
/// read, so none runs out.
#[must_use]
pub fn drawn_keyframe(
    width: u32,
    height: u32,
    filter: Filter,
    partitions: usize,
    word: &mut impl FnMut() -> u64,
) -> Vec<u8> {
    let macroblocks = (width.div_ceil(16) * height.div_ceil(16)) as usize;
    let mut head = Writer::new();
    head.flag(false);
    head.flag(false);
    head.flag(false);
    head.flag(filter.simple);
    head.literal(filter.level, 6);
    head.literal(filter.sharpness, 3);
    head.flag(false);
    head.literal(partitions.trailing_zeros(), 2);
    head.literal(u32::from(draw_byte(word) & 0x7F), 7);
    for _ in 0..5 {
        head.flag(false);
    }
    head.flag(true);
    for plane in &COEFF_UPDATE_PROBS {
        for band in plane {
            for context in band {
                for &update in context {
                    head.bit(update, false);
                }
            }
        }
    }
    let skipping = draw_byte(word) & 1 == 1;
    head.flag(skipping);
    if skipping {
        head.literal(u32::from(draw_byte(word)), 8);
    }
    draw_bits(&mut head, macroblocks * 512, word);
    let first_part = head.finish();
    let bodies = drawn_partitions(macroblocks, partitions, word);
    assemble(KEYFRAME, Some((width, height)), &first_part, &bodies)
}

/// A byte drawn as one word's top byte, the way the shared test generator
/// draws one, so a fixture pinned to its stream stays that stream.
fn draw_byte(word: &mut impl FnMut() -> u64) -> u8 {
    word().to_be_bytes()[0]
}

/// `buf` filled a word's little-endian bytes at a time, likewise.
fn draw_fill(word: &mut impl FnMut() -> u64, buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        chunk.copy_from_slice(&word().to_le_bytes()[..chunk.len()]);
    }
}

/// `count` bits drawn into `writer`, each under a probability drawn too.
fn draw_bits(writer: &mut Writer, count: usize, word: &mut impl FnMut() -> u64) {
    for _ in 0..count {
        writer.bit(draw_byte(word).max(1), draw_byte(word) & 1 == 1);
    }
}

/// `partitions` residual partitions of drawn bytes, each far longer than
/// `macroblocks` read.
fn drawn_partitions(
    macroblocks: usize,
    partitions: usize,
    word: &mut impl FnMut() -> u64,
) -> Vec<Vec<u8>> {
    (0..partitions)
        .map(|_| {
            let mut body = alloc::vec![0u8; macroblocks * 4096];
            draw_fill(word, &mut body);
            body
        })
        .collect()
}

/// A frame's bytes: its tag — whether it is a keyframe and is shown, and its
/// first partition's size — then a keyframe's start code and `size`, its
/// first partition, and its residual `partitions`, all but the last's length
/// before them.
fn assemble(
    (keyframe, shown): (bool, bool),
    size: Option<(u32, u32)>,
    first: &[u8],
    partitions: &[Vec<u8>],
) -> Vec<u8> {
    let tag = Tag {
        keyframe,
        version: 0,
        shown,
        first_partition: first.len(),
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&tag.bytes().expect("a small partition"));
    if let Some((width, height)) = size {
        bytes.extend_from_slice(&keyframe_header(width, height));
    }
    bytes.extend_from_slice(first);
    let (last, sized) = partitions.split_last().expect("a residual partition");
    for body in sized {
        let length = u32::try_from(body.len()).expect("a small partition");
        bytes.extend_from_slice(&length.to_le_bytes()[..3]);
    }
    for body in sized {
        bytes.extend_from_slice(body);
    }
    bytes.extend_from_slice(last);
    bytes
}

/// The keyframes written here: keyframes, and not shown, which the WEBP
/// container never asks.
const KEYFRAME: (bool, bool) = (true, false);

/// Even odds, for the probabilities a written inter frame sets itself.
const EVEN: u8 = 128;

/// A reference frame a written inter macroblock predicts from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reference {
    /// The frame decoded last.
    Last,
    /// The golden frame.
    Golden,
    /// The alternate frame.
    Alternate,
}

impl Reference {
    const fn buffer(self) -> Buffer {
        match self {
            Self::Last => Buffer::Last,
            Self::Golden => Buffer::Golden,
            Self::Alternate => Buffer::Alternate,
        }
    }
}

/// What a written inter frame leaves in the golden or the alternate frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Update {
    /// What it held.
    Kept,
    /// This frame.
    Refreshed,
    /// A copy of the last frame.
    CopiedFromLast,
    /// A copy of the other of the two.
    CopiedFromOther,
}

impl Update {
    /// The update this names for a buffer whose other is `other`.
    const fn of(self, other: Buffer) -> BufferUpdate {
        match self {
            Self::Kept => BufferUpdate::Keep,
            Self::Refreshed => BufferUpdate::Refresh,
            Self::CopiedFromLast => BufferUpdate::CopyOf(Buffer::Last),
            Self::CopiedFromOther => BufferUpdate::CopyOf(other),
        }
    }
}

/// How a written inter macroblock moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Movement {
    /// Not at all.
    Still,
    /// By the nearest of the vectors its neighbours lend.
    Nearest,
    /// By the next nearest.
    Near,
    /// By `(x, y)` quarter pixels, coded against the best its neighbours lend.
    New(i16, i16),
    /// Each part of a partitioning by its own vector.
    Split {
        /// The partitioning, as the split tree names it: nought the top and
        /// bottom halves, one the left and right, two the quarters, three
        /// every subblock alone.
        partitioning: u8,
        /// Each part's vector, `(x, y)` quarter pixels, the parts in raster
        /// order and as many used as the partitioning has.
        parts: [(i16, i16); 16],
    },
}

impl Movement {
    /// The mode it is coded with, and the vectors it asks for in the units a
    /// decoder holds them in.
    fn asked(self) -> (InterMode, Motion) {
        let vector = |(x, y): (i16, i16)| Mv { x: x * 2, y: y * 2 };
        let none = Motion::Whole(Mv::default());
        match self {
            Self::Still => (InterMode::Zero, none),
            Self::Nearest => (InterMode::Nearest, none),
            Self::Near => (InterMode::Near, none),
            Self::New(x, y) => (InterMode::New, Motion::Whole(vector((x, y)))),
            Self::Split {
                partitioning,
                parts,
            } => {
                let owners = SPLITS
                    .get(usize::from(partitioning))
                    .expect("a partitioning");
                let mvs = core::array::from_fn(|block| vector(parts[usize::from(owners[block])]));
                let split = InterMode::Split(Partitioning::of_code(partitioning));
                (split, Motion::Split(mvs))
            }
        }
    }
}

/// How a written macroblock is predicted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Predicted {
    /// From its own frame.
    Intra {
        /// The luma mode.
        ymode: u8,
        /// Every subblock's mode, where the luma mode is the subblock mode.
        bmode: u8,
        /// The chroma mode.
        uvmode: u8,
    },
    /// From a reference frame.
    Inter {
        /// The frame it predicts from.
        reference: Reference,
        /// How it moves.
        motion: Movement,
    },
}

/// One macroblock of a written inter frame.
#[derive(Copy, Clone, Debug)]
pub struct Written {
    /// How it is predicted.
    pub predicted: Predicted,
    /// Its quantised coefficients, each block's in scan order — the sixteen
    /// luma blocks in raster order, the four blue and the four red chroma
    /// blocks, then the luma DC block — or `None` to skip it. The luma blocks
    /// of a macroblock with a luma DC block code no first coefficient.
    pub residual: Option<[[i16; 16]; 25]>,
}

impl Written {
    /// What a decoder is to read this macroblock as, its vectors as asked.
    fn asked(&self) -> MbInfo {
        let prediction = match self.predicted {
            Predicted::Intra {
                ymode,
                bmode,
                uvmode,
            } => {
                let bmode = if ymode == super::B_PRED {
                    bmode
                } else {
                    B_DC_PRED
                };
                Prediction::Intra {
                    ymode,
                    uvmode,
                    bmodes: [bmode; 16],
                }
            }
            Predicted::Inter { reference, motion } => {
                let (mode, motion) = motion.asked();
                Prediction::Inter {
                    reference: reference.buffer(),
                    mode,
                    motion,
                }
            }
        };
        MbInfo {
            segment: 0,
            skip: self.residual.is_none(),
            prediction,
        }
    }
}

/// How a written inter frame is set.
#[derive(Copy, Clone, Debug)]
pub struct InterFrame {
    /// Its loop filter.
    pub filter: Filter,
    /// Its quantiser index, nought to 127.
    pub quantiser: u32,
    /// What it leaves in the golden frame.
    pub golden: Update,
    /// What it leaves in the alternate frame.
    pub alternate: Update,
    /// Whether vectors into the golden and into the alternate frame point
    /// opposite to vectors into the last.
    pub sign_bias: [bool; 2],
    /// Whether it becomes the last frame.
    pub refresh_last: bool,
    /// Whether the stream shows it.
    pub shown: bool,
}

impl Default for InterFrame {
    /// A shown frame becoming the last, leaving the golden and alternate
    /// frames as they were, unfiltered at quantiser nought.
    fn default() -> Self {
        Self {
            filter: Filter {
                simple: false,
                level: 0,
                sharpness: 0,
            },
            quantiser: 0,
            golden: Update::Kept,
            alternate: Update::Kept,
            sign_bias: [false; 2],
            refresh_last: true,
            shown: true,
        }
    }
}

impl InterFrame {
    /// The header the frame is written with, over `partitions` residual
    /// partitions: what a keyframe leaves, but for its buffers, filter and
    /// quantiser.
    fn header(&self, (width, height): (u32, u32), partitions: usize) -> Header {
        Header {
            width,
            height,
            simple_filter: self.filter.simple,
            filter_level: i32::try_from(self.filter.level).expect("a filter level"),
            sharpness: self.filter.sharpness,
            segmentation: Segmentation::none(),
            deltas: FilterDeltas::none(),
            quant: Quant {
                index: i32::try_from(self.quantiser).expect("a quantiser index"),
                y_dc: 0,
                y2_dc: 0,
                y2_ac: 0,
                uv_dc: 0,
                uv_ac: 0,
            },
            partitions,
            probs: Probabilities::DEFAULT,
            skip_probability: Some(EVEN),
            refresh_entropy: true,
            inter: Some(InterHeader {
                buffers: Buffers {
                    golden: self.golden.of(Buffer::Alternate),
                    alternate: self.alternate.of(Buffer::Golden),
                    sign_bias_golden: self.sign_bias[0],
                    sign_bias_alternate: self.sign_bias[1],
                },
                refresh_last: self.refresh_last,
                prob_intra: EVEN,
                prob_last: EVEN,
                prob_golden: EVEN,
            }),
        }
    }
}

/// Write an inter frame of a `width`×`height` stream, set as `frame` says,
/// whose macroblocks are `blocks` in raster order: each vector coded against
/// those its neighbours lend and each residual against its neighbours' token
/// contexts, as the format codes them.
///
/// # Panics
///
/// Where `blocks` is not one per macroblock.
#[must_use]
pub fn inter_frame(width: u32, height: u32, frame: &InterFrame, blocks: &[Written]) -> Vec<u8> {
    let columns = usize::try_from(width.div_ceil(16)).expect("a column count");
    let rows = usize::try_from(height.div_ceil(16)).expect("a row count");
    assert_eq!(blocks.len(), columns * rows, "a block for each macroblock");
    let header = frame.header((width, height), 1);
    let inter = header.inter.clone().expect("an inter frame's header");
    let mut head = Writer::new();
    write_header(&mut head, &header, &StreamState::new());
    let mut decided: Vec<MbInfo> = blocks.iter().map(Written::asked).collect();
    for index in 0..rows {
        let (done, rest) = decided.split_at_mut(index * columns);
        let above = done
            .get(done.len().saturating_sub(columns)..)
            .unwrap_or_default();
        let row = rest.get_mut(..columns).unwrap_or_default();
        write_row(&mut head, &header, &inter, above, row, (index, rows));
    }
    let mut tokens = Writer::new();
    let mut contexts = TokenContexts::new(columns);
    for (index, (block, decision)) in blocks.iter().zip(&decided).enumerate() {
        let column = index % columns;
        contexts.write(
            &mut tokens,
            column,
            decision.has_y2(),
            block.residual.as_ref(),
        );
    }
    assemble(
        (false, frame.shown),
        None,
        &head.finish(),
        &[tokens.finish()],
    )
}

/// An inter frame of a `width`×`height` stream, set as `frame` says, coded
/// over `partitions` residual partitions (one, two, four or eight), whose
/// probabilities, macroblocks and residuals are drawn from `word` as
/// [`drawn_keyframe`]'s are: not a frame anyone encoded, but one any decoder
/// of the format reads the same way, every partition carrying far more than
/// its macroblocks read.
#[must_use]
pub fn drawn_inter_frame(
    width: u32,
    height: u32,
    frame: &InterFrame,
    partitions: usize,
    word: &mut impl FnMut() -> u64,
) -> Vec<u8> {
    let macroblocks =
        usize::try_from(width.div_ceil(16) * height.div_ceil(16)).expect("a macroblock count");
    let mut header = frame.header((width, height), partitions);
    header.skip_probability = (draw_byte(word) & 1 == 1).then(|| draw_byte(word));
    if let Some(inter) = &mut header.inter {
        inter.prob_intra = draw_byte(word);
        inter.prob_last = draw_byte(word);
        inter.prob_golden = draw_byte(word);
    }
    let mut head = Writer::new();
    write_header(&mut head, &header, &StreamState::new());
    draw_bits(&mut head, macroblocks * 2048, word);
    let bodies = drawn_partitions(macroblocks, partitions, word);
    assemble((false, frame.shown), None, &head.finish(), &bodies)
}

/// The token contexts a writer keeps as a decoder does: whether each block
/// bordering the next coded past its first position — four luma blocks, two
/// blue, two red, then the luma DC block.
struct TokenContexts {
    above: Vec<[bool; 9]>,
    left: [bool; 9],
}

/// Where the blue, the red and the luma DC blocks' contexts sit among one
/// macroblock edge's nine.
const BLUE: usize = 4;
const RED: usize = 6;
const LUMA_DC: usize = 8;

impl TokenContexts {
    fn new(columns: usize) -> Self {
        Self {
            above: alloc::vec![[false; 9]; columns],
            left: [false; 9],
        }
    }

    /// Write the residual of the macroblock in `column`, or, for one skipped,
    /// leave the contexts a decoder leaves.
    fn write(
        &mut self,
        writer: &mut Writer,
        column: usize,
        has_y2: bool,
        residual: Option<&[[i16; 16]; 25]>,
    ) {
        if column == 0 {
            self.left = [false; 9];
        }
        let above = self.above.get_mut(column).expect("a column of the frame");
        let left = &mut self.left;
        let Some(blocks) = residual else {
            // A skipped macroblock codes nothing, but one coding no luma DC
            // block passes that context by.
            for edge in [&mut *above, &mut *left] {
                let kept = !has_y2 && edge[LUMA_DC];
                *edge = [false; 9];
                edge[LUMA_DC] = kept;
            }
            return;
        };
        let probs = &DEFAULT_COEFF_PROBS;
        let mut coded = [false; 25];
        if has_y2 {
            let context = usize::from(left[LUMA_DC]) + usize::from(above[LUMA_DC]);
            coded[24] = write_block(writer, &probs[1], context, 0, &blocks[24]);
        }
        let (plane, first) = if has_y2 { (0, 1) } else { (3, 0) };
        for index in 0..16 {
            let (row, col) = (index / 4, index % 4);
            let from_left = if col > 0 { coded[index - 1] } else { left[row] };
            let from_above = if row > 0 {
                coded[index - 4]
            } else {
                above[col]
            };
            let context = usize::from(from_left) + usize::from(from_above);
            coded[index] = write_block(writer, &probs[plane], context, first, &blocks[index]);
        }
        for (base, edge) in [(16, BLUE), (20, RED)] {
            for index in 0..4 {
                let (row, col) = (index / 2, index % 2);
                let at = base + index;
                let from_left = if col > 0 {
                    coded[at - 1]
                } else {
                    left[edge + row]
                };
                let from_above = if row > 0 {
                    coded[at - 2]
                } else {
                    above[edge + col]
                };
                let context = usize::from(from_left) + usize::from(from_above);
                coded[at] = write_block(writer, &probs[2], context, 0, &blocks[at]);
            }
        }
        let luma_dc = |kept: bool| if has_y2 { coded[24] } else { kept };
        *left = [
            coded[3],
            coded[7],
            coded[11],
            coded[15],
            coded[17],
            coded[19],
            coded[21],
            coded[23],
            luma_dc(left[LUMA_DC]),
        ];
        *above = [
            coded[12],
            coded[13],
            coded[14],
            coded[15],
            coded[18],
            coded[19],
            coded[22],
            coded[23],
            luma_dc(above[LUMA_DC]),
        ];
    }
}

//! A complete, fail-closed VP8 decoder (RFC 6386).
//!
//! The WEBP container carries exactly one kind of VP8 frame — a keyframe —
//! and [`decode_rows`] decodes exactly that, a macroblock row at a time,
//! refusing a bitstream that declares itself an interframe rather than
//! half-reading it. Within that, the format is complete: the boolean entropy
//! decoder, the segmentation, loop-filter and quantiser headers with their
//! per-segment and per-mode deltas, the token probability updates, the four
//! whole-macroblock luma modes and four chroma modes, all ten subblock modes,
//! the Walsh-Hadamard and DCT reconstructions, both the normal and the simple
//! loop filter at macroblock and subblock edges, and the conversion to RGB.
//!
//! [`Decoder`] decodes a stream of keyframes and inter frames, holding the
//! frames inter frames predict from (the `stream` module). An inter frame's
//! header is read against what a stream carries from frame to frame — its
//! probabilities, segment features and loop-filter deltas, which a keyframe
//! resets — and its macroblocks' modes and motion vectors by the `inter`
//! module. Inter prediction, a macroblock predicted from a reference frame
//! through its motion vectors, is the `motion` module's.
//!
//! Behind the `encode` feature, `Encoder` codes a recording's pictures as
//! such a stream (the `encode` module), reconstructing each frame with this
//! module's predictors, inverse transforms and loop filters, so its reference
//! is, to the bit, every decoder's. Both sides write through one boolean
//! encoder and syntax writer (the `writer` module), as the test fixture does.
//!
//! # Two readings of the frame header
//!
//! **A reserved colour space is refused.** The header's one colour-space bit
//! selects ITU-R BT.601 at zero and is reserved at one, and a decoder that
//! guessed BT.601 for the reserved value would silently produce the wrong
//! colours rather than say it could not.
//!
//! **The clamping-type bit is read and not acted on.** It says whether
//! reconstruction *needs* clamping; clamping regardless is correct either
//! way, and the alternative is trusting a file's word that its arithmetic
//! stays in range.
//!
//! The horizontal and vertical scale fields are likewise read and not acted
//! on: they ask a *display* to stretch the picture, and the decoded picture
//! is the size the header declares.
//!
//! # Where the arithmetic must match exactly
//!
//! Both transforms, the six intra-prediction averaging rules, and both loop
//! filters are specified as integer code with exact rounding, and a decoder
//! that rounds differently drifts visibly rather than approximately. Each is
//! transcribed from the reference decoder's own arithmetic.
//!
//! One erratum is worth naming: the reference code for the horizontal-down
//! subblock mode reads `svg2p` where the mode's own diagonal pattern, and
//! every other implementation, require `avg2p`.
//!
//! Where the specification's prose and its reference decoder part, the
//! reference decoder is followed, since every stream is encoded against it,
//! and the place says so.

mod colour;
#[cfg(any(test, feature = "encode"))]
mod encode;
#[cfg(any(test, feature = "encode"))]
mod forward;
mod inter;
mod motion;
mod stream;
#[cfg(any(test, feature = "fixture", feature = "encode"))]
mod writer;

use alloc::vec::Vec;

use tairix_util::fallible;

use motion::{Interpolation, RefPlane};
#[cfg(any(test, feature = "fixture", feature = "encode"))]
use writer::Writer;
#[cfg(any(test, feature = "encode"))]
use writer::{write_path, BMODE_PATHS, KF_YMODE_PATHS, UV_MODE_PATHS};

#[cfg(any(test, feature = "encode"))]
pub use encode::{Changed, Encoded, Encoder, Quality};
pub use stream::{stream_peak_bytes, Decoded, Decoder};

/// One pixel of a decoded row: opaque, straight-alpha RGBA8.
pub type Rgba8 = [u8; 4];

/// Why a `VP8 ` frame could not be decoded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Vp8Error {
    /// The frame ends before what it declares.
    Truncated,
    /// An inter frame where only a keyframe can be decoded.
    Interframe,
    /// A version the format does not define.
    UnsupportedProfile,
    /// A keyframe without its start code.
    BadStartCode,
    /// A width or height of nought.
    InvalidGeometry,
    /// The colour space the format reserves.
    ReservedColourSpace,
    /// A reference-buffer copy the format reserves.
    ReservedBufferCopy,
    /// Token partitions that do not fit the frame.
    InvalidPartitions,
    /// Dimensions whose sizes overflow.
    DimensionsOverflow,
    /// Memory the decode needs could not be had.
    OutOfMemory,
}

/// The three bytes a keyframe's uncompressed header carries after its frame
/// tag (RFC 6386 §9.1).
const START_CODE: [u8; 3] = [0x9D, 0x01, 0x2A];

/// Bytes a keyframe's uncompressed header occupies: the frame tag, the start
/// code, and the two dimensions.
const UNCOMPRESSED_HEADER: usize = 10;

/// Bytes a frame's tag occupies, which is the whole of an inter frame's
/// uncompressed header.
const TAG_BYTES: usize = 3;

/// The largest width or height a keyframe declares: fourteen bits.
const MAX_DIMENSION: u32 = (1 << 14) - 1;

/// The highest loop-filter level, which six bits carry.
const MAX_FILTER_LEVEL: i32 = 63;

/// Segments a frame may divide its macroblocks into.
const SEGMENTS: usize = 4;

/// Coefficient plane types the token probabilities are indexed by: luma
/// after a Y2 block, the Y2 block, chroma, and luma with no Y2 block.
const PLANES: usize = 4;

/// Coefficient bands one plane's probabilities are grouped into.
const BANDS: usize = 8;

/// Previous-coefficient contexts a band's probabilities are chosen by.
const CONTEXTS: usize = 3;

/// Probabilities one context holds, one per interior node of the token tree.
const NODES: usize = 11;

/// Residual partitions a frame may split its tokens across.
const MAX_PARTITIONS: usize = 8;

/// Coefficients one transform block holds.
const BLOCK_COEFFS: usize = 16;

/// Transform blocks one macroblock holds: sixteen luma, four each of two
/// chroma planes, and the luma DC block.
const MB_BLOCKS: usize = 25;

/// A macroblock's luma blocks, which come first among its blocks, then where
/// its four blue and its four red chroma blocks begin.
const LUMA_BLOCKS: usize = 16;
const BLUE_BLOCKS: usize = LUMA_BLOCKS;
const RED_BLOCKS: usize = BLUE_BLOCKS + 4;

/// The luma DC block's position among a macroblock's blocks.
const Y2_BLOCK: usize = 24;

/// Whole-macroblock luma and chroma modes, in the order the trees name them.
/// The decoder's arms fall through to the first, which only the encoder
/// names.
#[cfg(any(test, feature = "encode"))]
const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const H_PRED: u8 = 2;
const TM_PRED: u8 = 3;
const B_PRED: u8 = 4;

/// Subblock modes, in the order the tree names them.
const B_DC_PRED: u8 = 0;
const B_TM_PRED: u8 = 1;
const B_VE_PRED: u8 = 2;
const B_HE_PRED: u8 = 3;
const B_LD_PRED: u8 = 4;
const B_RD_PRED: u8 = 5;
const B_VR_PRED: u8 = 6;
const B_VL_PRED: u8 = 7;
const B_HD_PRED: u8 = 8;
const B_HU_PRED: u8 = 9;

/// The subblock mode a whole-macroblock mode implies for a neighbour's
/// context (RFC 6386 §11.3).
const MODE_TO_BMODE: [u8; 4] = [B_DC_PRED, B_VE_PRED, B_HE_PRED, B_TM_PRED];

/// The value an absent above row predicts from.
const ABOVE_ABSENT: u8 = 127;

/// The value an absent left column predicts from.
const LEFT_ABSENT: u8 = 129;

/// Token values the coefficient tree names.
const DCT_0: u8 = 0;
const DCT_CAT1: u8 = 5;
const DCT_EOB: u8 = 11;

/// The node the coefficient tree's end-of-block branch sits above, which a
/// read following a zero coefficient starts below.
const TREE_AFTER_EOB: usize = 2;

/// Extra-bit probabilities for each large-coefficient category, most
/// significant bit first (RFC 6386 §13.2).
const CATEGORY_PROBS: [&[u8]; 6] = [
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];

/// The smallest absolute value each large-coefficient category names.
const CATEGORY_BASES: [i32; 6] = [5, 7, 11, 19, 35, 67];

/// The two 16-bit fixed-point multipliers the inverse DCT uses.
const COS_PI8_SQRT2_MINUS1: i32 = 20091;
const SIN_PI8_SQRT2: i32 = 35468;

/// The DC dequantisation factor each quantiser index names
/// (RFC 6386 §14.1).
const DC_QLOOKUP: [i32; 128] = [
    4, 5, 6, 7, 8, 9, 10, 10, 11, 12, 13, 14, 15, 16, 17, 17, 18, 19, 20, 20, 21, 21, 22, 22, 23,
    23, 24, 25, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 37, 38, 39, 40, 41, 42, 43, 44,
    45, 46, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67,
    68, 69, 70, 71, 72, 73, 74, 75, 76, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 91,
    93, 95, 96, 98, 100, 101, 102, 104, 106, 108, 110, 112, 114, 116, 118, 122, 124, 126, 128, 130,
    132, 134, 136, 138, 140, 143, 145, 148, 151, 154, 157,
];

/// The AC dequantisation factor each quantiser index names.
const AC_QLOOKUP: [i32; 128] = [
    4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52,
    53, 54, 55, 56, 57, 58, 60, 62, 64, 66, 68, 70, 72, 74, 76, 78, 80, 82, 84, 86, 88, 90, 92, 94,
    96, 98, 100, 102, 104, 106, 108, 110, 112, 114, 116, 119, 122, 125, 128, 131, 134, 137, 140,
    143, 146, 149, 152, 155, 158, 161, 164, 167, 170, 173, 177, 181, 185, 189, 193, 197, 201, 205,
    209, 213, 217, 221, 225, 229, 234, 239, 245, 249, 254, 259, 264, 269, 274, 279, 284,
];

/// The position in a transform block each scan index writes to.
const ZIGZAG: [u8; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// The probability band each scan index draws its token
/// probabilities from.
const COEFF_BANDS: [u8; 16] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7];

/// The coefficient token tree: a positive entry is the next node, a
/// non-positive one is the negated token it decodes to.
const COEFF_TREE: [i8; 22] = [
    -11, 2, 0, 4, -1, 6, 8, 12, -2, 10, -3, -4, 14, 16, -5, -6, 18, 20, -7, -8, -9, -10,
];

/// The subblock-mode tree.
const BMODE_TREE: [i8; 18] = [
    0, 2, -1, 4, -2, 6, 8, 12, -3, 10, -5, -6, -4, 14, -7, 16, -8, -9,
];

/// The whole-macroblock luma-mode tree a keyframe uses.
const KF_YMODE_TREE: [i8; 8] = [-4, 2, 4, 6, 0, -1, -2, -3];

/// The chroma-mode tree.
const UV_MODE_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];

/// The probabilities the keyframe luma-mode tree is read with.
const KF_YMODE_PROBS: [u8; 4] = [145, 156, 163, 128];

/// The probabilities the chroma-mode tree is read with.
const KF_UV_MODE_PROBS: [u8; 3] = [142, 114, 183];

/// The probabilities a subblock mode is read with, chosen by the modes
/// of the subblocks above and to the left (RFC 6386 §11.4).
const KF_BMODE_PROBS: [[[u8; 9]; 10]; 10] = [
    [
        [231, 120, 48, 89, 115, 113, 120, 152, 112],
        [152, 179, 64, 126, 170, 118, 46, 70, 95],
        [175, 69, 143, 80, 85, 82, 72, 155, 103],
        [56, 58, 10, 171, 218, 189, 17, 13, 152],
        [144, 71, 10, 38, 171, 213, 144, 34, 26],
        [114, 26, 17, 163, 44, 195, 21, 10, 173],
        [121, 24, 80, 195, 26, 62, 44, 64, 85],
        [170, 46, 55, 19, 136, 160, 33, 206, 71],
        [63, 20, 8, 114, 114, 208, 12, 9, 226],
        [81, 40, 11, 96, 182, 84, 29, 16, 36],
    ],
    [
        [134, 183, 89, 137, 98, 101, 106, 165, 148],
        [72, 187, 100, 130, 157, 111, 32, 75, 80],
        [66, 102, 167, 99, 74, 62, 40, 234, 128],
        [41, 53, 9, 178, 241, 141, 26, 8, 107],
        [104, 79, 12, 27, 217, 255, 87, 17, 7],
        [74, 43, 26, 146, 73, 166, 49, 23, 157],
        [65, 38, 105, 160, 51, 52, 31, 115, 128],
        [87, 68, 71, 44, 114, 51, 15, 186, 23],
        [47, 41, 14, 110, 182, 183, 21, 17, 194],
        [66, 45, 25, 102, 197, 189, 23, 18, 22],
    ],
    [
        [88, 88, 147, 150, 42, 46, 45, 196, 205],
        [43, 97, 183, 117, 85, 38, 35, 179, 61],
        [39, 53, 200, 87, 26, 21, 43, 232, 171],
        [56, 34, 51, 104, 114, 102, 29, 93, 77],
        [107, 54, 32, 26, 51, 1, 81, 43, 31],
        [39, 28, 85, 171, 58, 165, 90, 98, 64],
        [34, 22, 116, 206, 23, 34, 43, 166, 73],
        [68, 25, 106, 22, 64, 171, 36, 225, 114],
        [34, 19, 21, 102, 132, 188, 16, 76, 124],
        [62, 18, 78, 95, 85, 57, 50, 48, 51],
    ],
    [
        [193, 101, 35, 159, 215, 111, 89, 46, 111],
        [60, 148, 31, 172, 219, 228, 21, 18, 111],
        [112, 113, 77, 85, 179, 255, 38, 120, 114],
        [40, 42, 1, 196, 245, 209, 10, 25, 109],
        [100, 80, 8, 43, 154, 1, 51, 26, 71],
        [88, 43, 29, 140, 166, 213, 37, 43, 154],
        [61, 63, 30, 155, 67, 45, 68, 1, 209],
        [142, 78, 78, 16, 255, 128, 34, 197, 171],
        [41, 40, 5, 102, 211, 183, 4, 1, 221],
        [51, 50, 17, 168, 209, 192, 23, 25, 82],
    ],
    [
        [125, 98, 42, 88, 104, 85, 117, 175, 82],
        [95, 84, 53, 89, 128, 100, 113, 101, 45],
        [75, 79, 123, 47, 51, 128, 81, 171, 1],
        [57, 17, 5, 71, 102, 57, 53, 41, 49],
        [115, 21, 2, 10, 102, 255, 166, 23, 6],
        [38, 33, 13, 121, 57, 73, 26, 1, 85],
        [41, 10, 67, 138, 77, 110, 90, 47, 114],
        [101, 29, 16, 10, 85, 128, 101, 196, 26],
        [57, 18, 10, 102, 102, 213, 34, 20, 43],
        [117, 20, 15, 36, 163, 128, 68, 1, 26],
    ],
    [
        [138, 31, 36, 171, 27, 166, 38, 44, 229],
        [67, 87, 58, 169, 82, 115, 26, 59, 179],
        [63, 59, 90, 180, 59, 166, 93, 73, 154],
        [40, 40, 21, 116, 143, 209, 34, 39, 175],
        [57, 46, 22, 24, 128, 1, 54, 17, 37],
        [47, 15, 16, 183, 34, 223, 49, 45, 183],
        [46, 17, 33, 183, 6, 98, 15, 32, 183],
        [65, 32, 73, 115, 28, 128, 23, 128, 205],
        [40, 3, 9, 115, 51, 192, 18, 6, 223],
        [87, 37, 9, 115, 59, 77, 64, 21, 47],
    ],
    [
        [104, 55, 44, 218, 9, 54, 53, 130, 226],
        [64, 90, 70, 205, 40, 41, 23, 26, 57],
        [54, 57, 112, 184, 5, 41, 38, 166, 213],
        [30, 34, 26, 133, 152, 116, 10, 32, 134],
        [75, 32, 12, 51, 192, 255, 160, 43, 51],
        [39, 19, 53, 221, 26, 114, 32, 73, 255],
        [31, 9, 65, 234, 2, 15, 1, 118, 73],
        [88, 31, 35, 67, 102, 85, 55, 186, 85],
        [56, 21, 23, 111, 59, 205, 45, 37, 192],
        [55, 38, 70, 124, 73, 102, 1, 34, 98],
    ],
    [
        [102, 61, 71, 37, 34, 53, 31, 243, 192],
        [69, 60, 71, 38, 73, 119, 28, 222, 37],
        [68, 45, 128, 34, 1, 47, 11, 245, 171],
        [62, 17, 19, 70, 146, 85, 55, 62, 70],
        [75, 15, 9, 9, 64, 255, 184, 119, 16],
        [37, 43, 37, 154, 100, 163, 85, 160, 1],
        [63, 9, 92, 136, 28, 64, 32, 201, 85],
        [86, 6, 28, 5, 64, 255, 25, 248, 1],
        [56, 8, 17, 132, 137, 255, 55, 116, 128],
        [58, 15, 20, 82, 135, 57, 26, 121, 40],
    ],
    [
        [164, 50, 31, 137, 154, 133, 25, 35, 218],
        [51, 103, 44, 131, 131, 123, 31, 6, 158],
        [86, 40, 64, 135, 148, 224, 45, 183, 128],
        [22, 26, 17, 131, 240, 154, 14, 1, 209],
        [83, 12, 13, 54, 192, 255, 68, 47, 28],
        [45, 16, 21, 91, 64, 222, 7, 1, 197],
        [56, 21, 39, 155, 60, 138, 23, 102, 213],
        [85, 26, 85, 85, 128, 128, 32, 146, 171],
        [18, 11, 7, 63, 144, 171, 4, 4, 246],
        [35, 27, 10, 146, 174, 171, 12, 26, 128],
    ],
    [
        [190, 80, 35, 99, 180, 80, 126, 54, 45],
        [85, 126, 47, 87, 176, 51, 41, 20, 32],
        [101, 75, 128, 139, 118, 146, 116, 128, 85],
        [56, 41, 15, 176, 236, 85, 37, 9, 62],
        [146, 36, 19, 30, 171, 255, 97, 27, 20],
        [71, 30, 17, 119, 118, 255, 17, 18, 138],
        [101, 38, 60, 138, 55, 70, 43, 26, 142],
        [138, 45, 61, 62, 219, 1, 81, 188, 64],
        [32, 41, 20, 117, 151, 142, 20, 21, 163],
        [112, 19, 12, 61, 195, 128, 48, 4, 24],
    ],
];

/// The whole-macroblock luma-mode tree an inter frame's intra macroblocks use.
const YMODE_TREE: [i8; 8] = [0, 2, 4, 6, -1, -2, -3, -4];

/// The probabilities an inter frame's luma-mode tree starts from
/// (RFC 6386 §16.1).
const YMODE_PROBS: [u8; 4] = [112, 86, 140, 37];

/// The probabilities an inter frame's chroma-mode tree starts from.
const UV_MODE_PROBS: [u8; 3] = [162, 101, 204];

/// The probabilities an inter frame's subblock modes are read with, which no
/// header updates.
const BMODE_PROBS: [u8; 9] = [120, 90, 79, 133, 87, 85, 80, 111, 151];

/// Codes the mode tree names an inter macroblock's motion by; nought is no
/// motion.
const MV_NEAREST: u8 = 1;
const MV_NEAR: u8 = 2;
const MV_NEW: u8 = 3;
const MV_SPLIT: u8 = 4;

/// The mode tree's code for no motion, which only a writer names: a reader
/// takes it as the one code left.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
const MV_ZERO: u8 = 0;

/// The tree an inter macroblock's mode is read with.
const MV_REF_TREE: [i8; 8] = [0, 2, -1, 4, -2, 6, -3, -4];

/// The probability of each step of the mode tree, by how strongly the
/// neighbours' vectors weigh for that step (RFC 6386 §16.3).
const MODE_CONTEXTS: [[u8; 4]; 6] = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
];

/// The tree a split macroblock's partitioning is read with: nought is the top
/// and bottom halves, one the left and right halves, two the quarters, and
/// three every subblock alone (RFC 6386 §16.4).
const SPLIT_TREE: [i8; 6] = [-3, 2, -2, 4, 0, -1];

/// The probabilities the partitioning tree is read with.
const SPLIT_PROBS: [u8; 3] = [110, 111, 150];

/// The part of each partitioning every subblock belongs to, in raster order.
const SPLITS: [[u8; 16]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];

/// Codes the part-vector tree names a part's vector by; two is no motion.
const SUB_MV_LEFT: u8 = 0;
const SUB_MV_ABOVE: u8 = 1;
const SUB_MV_NEW: u8 = 3;

/// The part-vector tree's code for no motion, which only a writer names.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
const SUB_MV_ZERO: u8 = 2;

/// The tree a part's vector is read with: its left neighbour's, its above
/// neighbour's, none, or a new one.
const SUB_MV_REF_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];

/// The probabilities the part-vector tree is read with, by whether the left
/// and above neighbours' vectors are zero and agree: neither, the left zero,
/// the above zero, both equal, both zero.
const SUB_MV_REF_PROBS: [[u8; 3]; 5] = [
    [147, 136, 18],
    [106, 145, 1],
    [179, 121, 1],
    [223, 1, 34],
    [208, 1, 1],
];

/// Probabilities one motion-vector component is read with: whether it is
/// short, its sign, the short tree's seven, then each long bit's
/// (RFC 6386 §17.2).
const MV_PROBS: usize = 19;

/// Where each part of a component's coding reads its probabilities from.
const MV_IS_SHORT: usize = 0;
const MV_SIGN: usize = 1;
const MV_SHORT: usize = 2;
const MV_LONG: usize = 9;

/// Bits a long component carries.
const MV_LONG_BITS: usize = 10;

/// The tree a short component's magnitude, nought to seven, is read with.
const SMALL_MV_TREE: [i8; 14] = [2, 8, 4, 6, 0, -1, -2, -3, 10, 12, -4, -5, -6, -7];

/// The vector probabilities a keyframe resets a stream to: the row's, then
/// the column's.
const DEFAULT_MV_PROBS: [[u8; MV_PROBS]; 2] = [
    [
        162, 128, 225, 146, 172, 147, 214, 39, 156, 128, 129, 132, 75, 145, 178, 206, 239, 254, 254,
    ],
    [
        164, 128, 204, 170, 119, 235, 140, 230, 228, 128, 130, 130, 74, 148, 180, 203, 236, 254,
        254,
    ],
];

/// The probability that each vector probability is replaced by one the
/// header carries.
const MV_UPDATE_PROBS: [[u8; MV_PROBS]; 2] = [
    [
        237, 246, 253, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 250, 250, 252, 254,
        254,
    ],
    [
        231, 243, 245, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 251, 251, 254, 254,
        254,
    ],
];

/// The token probabilities a keyframe starts from (RFC 6386 §13.5).
///
/// Plane zero's first band is unused — a luma block following a Y2 block
/// starts at scan index one, whose band is one — so its placeholder
/// entries are never read.
const DEFAULT_COEFF_PROBS: [[[[u8; 11]; 3]; 8]; 4] = [
    [
        [
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [253, 136, 254, 255, 228, 219, 128, 128, 128, 128, 128],
            [189, 129, 242, 255, 227, 213, 255, 219, 128, 128, 128],
            [106, 126, 227, 252, 214, 209, 255, 255, 128, 128, 128],
        ],
        [
            [1, 98, 248, 255, 236, 226, 255, 255, 128, 128, 128],
            [181, 133, 238, 254, 221, 234, 255, 154, 128, 128, 128],
            [78, 134, 202, 247, 198, 180, 255, 219, 128, 128, 128],
        ],
        [
            [1, 185, 249, 255, 243, 255, 128, 128, 128, 128, 128],
            [184, 150, 247, 255, 236, 224, 128, 128, 128, 128, 128],
            [77, 110, 216, 255, 236, 230, 128, 128, 128, 128, 128],
        ],
        [
            [1, 101, 251, 255, 241, 255, 128, 128, 128, 128, 128],
            [170, 139, 241, 252, 236, 209, 255, 255, 128, 128, 128],
            [37, 116, 196, 243, 228, 255, 255, 255, 128, 128, 128],
        ],
        [
            [1, 204, 254, 255, 245, 255, 128, 128, 128, 128, 128],
            [207, 160, 250, 255, 238, 128, 128, 128, 128, 128, 128],
            [102, 103, 231, 255, 211, 171, 128, 128, 128, 128, 128],
        ],
        [
            [1, 152, 252, 255, 240, 255, 128, 128, 128, 128, 128],
            [177, 135, 243, 255, 234, 225, 128, 128, 128, 128, 128],
            [80, 129, 211, 255, 194, 224, 128, 128, 128, 128, 128],
        ],
        [
            [1, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [246, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [255, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [198, 35, 237, 223, 193, 187, 162, 160, 145, 155, 62],
            [131, 45, 198, 221, 172, 176, 220, 157, 252, 221, 1],
            [68, 47, 146, 208, 149, 167, 221, 162, 255, 223, 128],
        ],
        [
            [1, 149, 241, 255, 221, 224, 255, 255, 128, 128, 128],
            [184, 141, 234, 253, 222, 220, 255, 199, 128, 128, 128],
            [81, 99, 181, 242, 176, 190, 249, 202, 255, 255, 128],
        ],
        [
            [1, 129, 232, 253, 214, 197, 242, 196, 255, 255, 128],
            [99, 121, 210, 250, 201, 198, 255, 202, 128, 128, 128],
            [23, 91, 163, 242, 170, 187, 247, 210, 255, 255, 128],
        ],
        [
            [1, 200, 246, 255, 234, 255, 128, 128, 128, 128, 128],
            [109, 178, 241, 255, 231, 245, 255, 255, 128, 128, 128],
            [44, 130, 201, 253, 205, 192, 255, 255, 128, 128, 128],
        ],
        [
            [1, 132, 239, 251, 219, 209, 255, 165, 128, 128, 128],
            [94, 136, 225, 251, 218, 190, 255, 255, 128, 128, 128],
            [22, 100, 174, 245, 186, 161, 255, 199, 128, 128, 128],
        ],
        [
            [1, 182, 249, 255, 232, 235, 128, 128, 128, 128, 128],
            [124, 143, 241, 255, 227, 234, 128, 128, 128, 128, 128],
            [35, 77, 181, 251, 193, 211, 255, 205, 128, 128, 128],
        ],
        [
            [1, 157, 247, 255, 236, 231, 255, 255, 128, 128, 128],
            [121, 141, 235, 255, 225, 227, 255, 255, 128, 128, 128],
            [45, 99, 188, 251, 195, 217, 255, 224, 128, 128, 128],
        ],
        [
            [1, 1, 251, 255, 213, 255, 128, 128, 128, 128, 128],
            [203, 1, 248, 255, 255, 128, 128, 128, 128, 128, 128],
            [137, 1, 177, 255, 224, 255, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [253, 9, 248, 251, 207, 208, 255, 192, 128, 128, 128],
            [175, 13, 224, 243, 193, 185, 249, 198, 255, 255, 128],
            [73, 17, 171, 221, 161, 179, 236, 167, 255, 234, 128],
        ],
        [
            [1, 95, 247, 253, 212, 183, 255, 255, 128, 128, 128],
            [239, 90, 244, 250, 211, 209, 255, 255, 128, 128, 128],
            [155, 77, 195, 248, 188, 195, 255, 255, 128, 128, 128],
        ],
        [
            [1, 24, 239, 251, 218, 219, 255, 205, 128, 128, 128],
            [201, 51, 219, 255, 196, 186, 128, 128, 128, 128, 128],
            [69, 46, 190, 239, 201, 218, 255, 228, 128, 128, 128],
        ],
        [
            [1, 191, 251, 255, 255, 128, 128, 128, 128, 128, 128],
            [223, 165, 249, 255, 213, 255, 128, 128, 128, 128, 128],
            [141, 124, 248, 255, 255, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 16, 248, 255, 255, 128, 128, 128, 128, 128, 128],
            [190, 36, 230, 255, 236, 255, 128, 128, 128, 128, 128],
            [149, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 226, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [247, 192, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [240, 128, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [1, 134, 252, 255, 255, 128, 128, 128, 128, 128, 128],
            [213, 62, 250, 255, 255, 128, 128, 128, 128, 128, 128],
            [55, 93, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
        [
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
    [
        [
            [202, 24, 213, 235, 186, 191, 220, 160, 240, 175, 255],
            [126, 38, 182, 232, 169, 184, 228, 174, 255, 187, 128],
            [61, 46, 138, 219, 151, 178, 240, 170, 255, 216, 128],
        ],
        [
            [1, 112, 230, 250, 199, 191, 247, 159, 255, 255, 128],
            [166, 109, 228, 252, 211, 215, 255, 174, 128, 128, 128],
            [39, 77, 162, 232, 172, 180, 245, 178, 255, 255, 128],
        ],
        [
            [1, 52, 220, 246, 198, 199, 249, 220, 255, 255, 128],
            [124, 74, 191, 243, 183, 193, 250, 221, 255, 255, 128],
            [24, 71, 130, 219, 154, 170, 243, 182, 255, 255, 128],
        ],
        [
            [1, 182, 225, 249, 219, 240, 255, 224, 128, 128, 128],
            [149, 150, 226, 252, 216, 205, 255, 171, 128, 128, 128],
            [28, 108, 170, 242, 183, 194, 254, 223, 255, 255, 128],
        ],
        [
            [1, 81, 230, 252, 204, 203, 255, 192, 128, 128, 128],
            [123, 102, 209, 247, 188, 196, 255, 233, 128, 128, 128],
            [20, 95, 153, 243, 164, 173, 255, 203, 128, 128, 128],
        ],
        [
            [1, 222, 248, 255, 216, 213, 128, 128, 128, 128, 128],
            [168, 175, 246, 252, 235, 205, 255, 255, 128, 128, 128],
            [47, 116, 215, 255, 211, 212, 255, 255, 128, 128, 128],
        ],
        [
            [1, 121, 236, 253, 212, 214, 255, 255, 128, 128, 128],
            [141, 84, 213, 252, 201, 202, 255, 219, 128, 128, 128],
            [42, 80, 160, 240, 162, 185, 255, 205, 128, 128, 128],
        ],
        [
            [1, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [244, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
            [238, 1, 255, 128, 128, 128, 128, 128, 128, 128, 128],
        ],
    ],
];

/// The probability that each token probability is replaced by one the
/// frame header carries (RFC 6386 §13.4).
const COEFF_UPDATE_PROBS: [[[[u8; 11]; 3]; 8]; 4] = [
    [
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [176, 246, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [223, 241, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 244, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [234, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 246, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [239, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 253, 255, 254, 255, 255, 255, 255, 255, 255],
            [250, 255, 254, 255, 254, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [217, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [225, 252, 241, 253, 255, 255, 254, 255, 255, 255, 255],
            [234, 250, 241, 250, 253, 255, 253, 254, 255, 255, 255],
        ],
        [
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [223, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [238, 253, 254, 254, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 248, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [247, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [186, 251, 250, 255, 255, 255, 255, 255, 255, 255, 255],
            [234, 251, 244, 254, 255, 255, 255, 255, 255, 255, 255],
            [251, 251, 243, 253, 254, 255, 254, 255, 255, 255, 255],
        ],
        [
            [255, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [236, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [251, 253, 253, 254, 254, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 254, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
    [
        [
            [248, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 254, 252, 254, 255, 255, 255, 255, 255, 255, 255],
            [248, 254, 249, 253, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [246, 253, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 254, 251, 254, 254, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 254, 252, 255, 255, 255, 255, 255, 255, 255, 255],
            [248, 254, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 255, 254, 254, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [245, 251, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [253, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 251, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [252, 253, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 254, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 252, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [249, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 254, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 253, 255, 255, 255, 255, 255, 255, 255, 255],
            [250, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
        [
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [254, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
            [255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        ],
    ],
];

/// A boolean entropy decoder (RFC 6386 §7.3).
///
/// A read past the end of the partition yields zero bytes and records that
/// it happened, rather than refusing on the spot: the decoder's two-byte
/// lookahead legitimately reaches past the last meaningful byte, so only a
/// frame that *finished* short is malformed. [`Self::exhausted`] is checked
/// once the frame is decoded, and no pixels are handed out when it is set.
struct Bool<'a> {
    bytes: &'a [u8],
    next: usize,
    value: u32,
    range: u32,
    count: u32,
    exhausted: bool,
}

impl<'a> Bool<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let mut decoder = Self {
            bytes,
            next: 0,
            value: 0,
            range: 255,
            count: 0,
            exhausted: false,
        };
        decoder.value = (u32::from(decoder.byte()) << 8) | u32::from(decoder.byte());
        decoder
    }

    fn byte(&mut self) -> u8 {
        let Some(&value) = self.bytes.get(self.next) else {
            self.exhausted = true;
            return 0;
        };
        self.next += 1;
        value
    }

    fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn bit(&mut self, probability: u8) -> u32 {
        let split = 1 + (((self.range - 1) * u32::from(probability)) >> 8);
        let big_split = split << 8;
        let taken = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            1
        } else {
            self.range = split;
            0
        };
        // Back to 128 or more in one shift, reading the byte the eighth shift
        // since the last one reaches: the same bits, a step at a time or not.
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.value <<= shift;
        self.count += shift;
        if self.count >= 8 {
            self.count -= 8;
            self.value |= u32::from(self.byte()) << self.count;
        }
        taken
    }

    fn flag(&mut self) -> bool {
        self.bit(128) != 0
    }

    /// An unsigned literal, most significant bit first.
    fn literal(&mut self, bits: u32) -> u32 {
        let mut value = 0;
        for _ in 0..bits {
            value = (value << 1) | self.bit(128);
        }
        value
    }

    /// An eight-bit probability.
    fn probability(&mut self) -> u8 {
        u8::try_from(self.literal(8)).unwrap_or(u8::MAX)
    }

    /// A magnitude-then-sign literal, which is how every header delta is
    /// coded.
    fn signed(&mut self, bits: u32) -> i32 {
        let magnitude = i32::try_from(self.literal(bits)).unwrap_or(0);
        if self.flag() {
            -magnitude
        } else {
            magnitude
        }
    }

    /// A literal that is present only when a flag says so.
    fn optional_signed(&mut self, bits: u32) -> i32 {
        if self.flag() {
            self.signed(bits)
        } else {
            0
        }
    }

    /// Walk `tree` from node `start`, answering the leaf value reached.
    fn tree(&mut self, tree: &[i8], probs: &[u8], start: usize) -> u8 {
        let mut node = start;
        for _ in 0..tree.len() {
            let probability = probs.get(node >> 1).copied().unwrap_or(128);
            let branch = node + usize::try_from(self.bit(probability)).unwrap_or(0);
            match tree.get(branch).copied().unwrap_or(0) {
                next if next > 0 => node = usize::try_from(next).unwrap_or(0),
                leaf => return u8::try_from(-i32::from(leaf)).unwrap_or(0),
            }
        }
        0
    }
}

/// Per-segment overrides of the quantiser index and loop-filter level, which
/// persist from frame to frame until a header replaces them.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Segmentation {
    enabled: bool,
    update_map: bool,
    absolute: bool,
    quant: [i32; SEGMENTS],
    filter: [i32; SEGMENTS],
    probs: [u8; 3],
}

impl Segmentation {
    const fn none() -> Self {
        Self {
            enabled: false,
            update_map: false,
            absolute: false,
            quant: [0; SEGMENTS],
            filter: [0; SEGMENTS],
            probs: [255; 3],
        }
    }

    /// Read whether segmentation applies to this frame, and the features and
    /// map probabilities it replaces.
    fn read(&mut self, reader: &mut Bool<'_>) {
        self.enabled = reader.flag();
        self.update_map = self.enabled && reader.flag();
        if self.enabled && reader.flag() {
            self.absolute = reader.flag();
            for value in &mut self.quant {
                *value = reader.optional_signed(7);
            }
            for value in &mut self.filter {
                *value = reader.optional_signed(6);
            }
        }
        if self.update_map {
            for probability in &mut self.probs {
                *probability = if reader.flag() {
                    reader.probability()
                } else {
                    255
                };
            }
        }
    }

    /// The segment a macroblock header names, or `None` where this frame
    /// keeps the segment map it has.
    fn read_id(&self, reader: &mut Bool<'_>) -> Option<u8> {
        if !self.update_map {
            return None;
        }
        Some(if reader.bit(self.probs[0]) != 0 {
            2 + u8::from(reader.bit(self.probs[2]) != 0)
        } else {
            u8::from(reader.bit(self.probs[1]) != 0)
        })
    }

    /// Write `segment` as a macroblock header names it, where this frame
    /// updates the map, so that [`Self::read_id`] reads it back.
    #[cfg(any(test, feature = "fixture", feature = "encode"))]
    fn write_id(&self, writer: &mut Writer, segment: u8) {
        if !self.update_map {
            return;
        }
        let [first, low, high] = self.probs;
        writer.bit(first, segment >= 2);
        if segment >= 2 {
            writer.bit(high, segment == 3);
        } else {
            writer.bit(low, segment == 1);
        }
    }

    /// The quantiser index a segment uses, from the frame's own index.
    fn quant_index(&self, base: i32, segment: usize) -> i32 {
        if !self.enabled {
            return base;
        }
        let delta = self.quant.get(segment).copied().unwrap_or(0);
        let value = if self.absolute { delta } else { base + delta };
        value.clamp(0, 127)
    }

    /// The loop-filter level a segment uses, from the frame's own level, held
    /// to the filter's range before any delta adjusts it.
    fn filter_level(&self, base: i32, segment: usize) -> i32 {
        if !self.enabled {
            return base;
        }
        let delta = self.filter.get(segment).copied().unwrap_or(0);
        let level = if self.absolute { delta } else { base + delta };
        level.clamp(0, MAX_FILTER_LEVEL)
    }
}

/// The loop-filter level adjustments a stream declares, which persist from
/// frame to frame until a header replaces one.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FilterDeltas {
    enabled: bool,
    /// By the frame a macroblock predicts from: its own, then the last, the
    /// golden and the alternate.
    reference: [i32; 4],
    /// By how it is predicted: subblock modes, then no motion, one vector for
    /// the whole macroblock, and split motion.
    mode: [i32; 4],
}

impl FilterDeltas {
    const fn none() -> Self {
        Self {
            enabled: false,
            reference: [0; 4],
            mode: [0; 4],
        }
    }

    /// Read whether the deltas apply to this frame, and each one it replaces.
    fn read(&mut self, reader: &mut Bool<'_>) {
        self.enabled = reader.flag();
        if !self.enabled || !reader.flag() {
            return;
        }
        for value in self.reference.iter_mut().chain(&mut self.mode) {
            if reader.flag() {
                *value = reader.signed(6);
            }
        }
    }
}

/// The dequantisation factors one segment's quantiser index yields.
#[derive(Copy, Clone, Default)]
struct Dequant {
    y_dc: i32,
    y_ac: i32,
    y2_dc: i32,
    y2_ac: i32,
    uv_dc: i32,
    uv_ac: i32,
}

impl Dequant {
    /// The DC and AC factors of the block at `index` among a macroblock's.
    fn of_block(&self, index: usize) -> (i32, i32) {
        if index == Y2_BLOCK {
            (self.y2_dc, self.y2_ac)
        } else if (BLUE_BLOCKS..Y2_BLOCK).contains(&index) {
            (self.uv_dc, self.uv_ac)
        } else {
            (self.y_dc, self.y_ac)
        }
    }
}

/// The quantiser index and its per-plane deltas.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Quant {
    index: i32,
    y_dc: i32,
    y2_dc: i32,
    y2_ac: i32,
    uv_dc: i32,
    uv_ac: i32,
}

impl Quant {
    fn read(reader: &mut Bool<'_>) -> Self {
        Self {
            index: i32::try_from(reader.literal(7)).unwrap_or(0),
            y_dc: reader.optional_signed(4),
            y2_dc: reader.optional_signed(4),
            y2_ac: reader.optional_signed(4),
            uv_dc: reader.optional_signed(4),
            uv_ac: reader.optional_signed(4),
        }
    }

    /// The factors an index yields, with the scaling and clamping the format
    /// applies to the luma DC and chroma DC terms.
    fn factors(&self, index: i32) -> Dequant {
        let dc = |at: i32| {
            DC_QLOOKUP
                .get(usize::try_from(at.clamp(0, 127)).unwrap_or(0))
                .copied()
                .unwrap_or(4)
        };
        let ac = |at: i32| {
            AC_QLOOKUP
                .get(usize::try_from(at.clamp(0, 127)).unwrap_or(0))
                .copied()
                .unwrap_or(4)
        };
        Dequant {
            y_dc: dc(index + self.y_dc),
            y_ac: ac(index),
            y2_dc: dc(index + self.y2_dc) * 2,
            y2_ac: (ac(index + self.y2_ac) * 155 / 100).max(8),
            uv_dc: dc(index + self.uv_dc).min(132),
            uv_ac: ac(index + self.uv_ac),
        }
    }
}

/// One macroblock's coding decisions.
#[derive(Clone)]
struct Macroblock {
    segment: u8,
    skip: bool,
    /// The whole-macroblock luma mode, [`B_PRED`] when the subblocks carry
    /// their own.
    ymode: u8,
    uvmode: u8,
    bmodes: [u8; BLOCK_COEFFS],
}

/// Whether each of a macroblock's blocks decoded a non-zero coefficient,
/// which is the context its neighbours' token reads use.
#[derive(Copy, Clone, Default)]
struct Nonzero {
    y: [bool; 4],
    u: [bool; 2],
    v: [bool; 2],
    y2: bool,
}

/// One image plane, held with the one-pixel border prediction reads from and
/// four spare columns for the above-right samples a subblock mode needs.
struct Plane {
    samples: Vec<u8>,
    stride: usize,
    width: usize,
}

/// Spare columns each plane row carries past its width, so the rightmost
/// macroblock's above-right samples have somewhere to be.
const SPARE_COLUMNS: usize = 4;

impl Plane {
    /// The bytes a plane of `width`×`height` samples holds: one bordering
    /// row above, and one bordering and the spare columns to each row.
    fn len(width: usize, height: usize) -> Option<usize> {
        width
            .checked_add(1 + SPARE_COLUMNS)?
            .checked_mul(height.checked_add(1)?)
    }

    fn new(width: usize, height: usize) -> Result<Self, Vp8Error> {
        let stride = width
            .checked_add(1 + SPARE_COLUMNS)
            .ok_or(Vp8Error::DimensionsOverflow)?;
        let len = Self::len(width, height).ok_or(Vp8Error::DimensionsOverflow)?;
        let mut samples = fallible::filled(len, 0u8).ok_or(Vp8Error::OutOfMemory)?;
        // The row above the picture reads 127 and the column to its left
        // reads 129, which is what the format predicts from where a
        // neighbour does not exist. Their shared corner belongs to the row.
        if let Some(row) = samples.get_mut(..stride) {
            row.fill(ABOVE_ABSENT);
        }
        for y in 0..height {
            if let Some(sample) = samples.get_mut((y + 1) * stride) {
                *sample = LEFT_ABSENT;
            }
        }
        Ok(Self {
            samples,
            stride,
            width,
        })
    }

    /// The index of sample (`x`, `y`), where `-1` is the border.
    fn at(&self, x: isize, y: isize) -> usize {
        let column = usize::try_from(x + 1).unwrap_or(0);
        let row = usize::try_from(y + 1).unwrap_or(0);
        row * self.stride + column
    }

    fn get(&self, x: isize, y: isize) -> u8 {
        self.samples.get(self.at(x, y)).copied().unwrap_or(0)
    }

    fn set(&mut self, x: isize, y: isize, value: u8) {
        let index = self.at(x, y);
        if let Some(sample) = self.samples.get_mut(index) {
            *sample = value;
        }
    }

    /// The `len` samples from (`x`, `y`) along their row, writable; none
    /// where they would leave the plane.
    fn row_mut(&mut self, x: isize, y: isize, len: usize) -> &mut [u8] {
        let at = self.at(x, y);
        self.samples.get_mut(at..at + len).unwrap_or_default()
    }

    /// Make row `last` the bordering row above, so the next macroblock row
    /// predicts from it as it was reconstructed.
    fn carry(&mut self, last: usize) {
        let from = (last + 1) * self.stride;
        if from + self.stride <= self.samples.len() {
            self.samples.copy_within(from..from + self.stride, 0);
        }
    }

    /// The samples of rows `rows`, border column and spare columns included.
    fn rows(&self, rows: core::ops::Range<usize>) -> &[u8] {
        self.samples
            .get((rows.start + 1) * self.stride..(rows.end + 1) * self.stride)
            .unwrap_or(&[])
    }

    /// The samples of rows `rows`, writable.
    fn rows_mut(&mut self, rows: core::ops::Range<usize>) -> &mut [u8] {
        let stride = self.stride;
        self.samples
            .get_mut((rows.start + 1) * stride..(rows.end + 1) * stride)
            .unwrap_or(&mut [])
    }

    /// Copy the rightmost sample of row `y` into the spare columns, so the
    /// rightmost macroblock of the row below predicts from it.
    fn extend_row(&mut self, y: isize) {
        let last = self.get(isize::try_from(self.width).unwrap_or(0) - 1, y);
        for spare in 0..SPARE_COLUMNS {
            let x = isize::try_from(self.width + spare).unwrap_or(0);
            self.set(x, y, last);
        }
    }

    /// Its `width`×`height` samples, border left out, as a reference frame is
    /// read.
    fn view(&self, width: usize, height: usize) -> Option<RefPlane<'_>> {
        RefPlane::new(
            self.samples.get(self.at(0, 0)..)?,
            self.stride,
            width,
            height,
        )
    }
}

/// The token probabilities, by plane, band, context and tree node.
type CoeffProbs = [[[[u8; NODES]; CONTEXTS]; BANDS]; PLANES];

/// The probabilities a frame decodes with, which a stream carries from frame
/// to frame.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Probabilities {
    coeff: CoeffProbs,
    /// The luma-mode tree's, for an inter frame's intra macroblocks.
    ymode: [u8; 4],
    /// The chroma-mode tree's, likewise.
    uvmode: [u8; 3],
    /// Each vector component's: the row's, then the column's.
    mv: [[u8; MV_PROBS]; 2],
}

impl Probabilities {
    /// What a keyframe resets a stream's probabilities to.
    const DEFAULT: Self = Self {
        coeff: DEFAULT_COEFF_PROBS,
        ymode: YMODE_PROBS,
        uvmode: UV_MODE_PROBS,
        mv: DEFAULT_MV_PROBS,
    };

    /// Replace each token probability the header updates (RFC 6386 §13.4).
    fn read_coeff_updates(&mut self, reader: &mut Bool<'_>) {
        for (plane, updates) in self.coeff.iter_mut().zip(&COEFF_UPDATE_PROBS) {
            for (band, updates) in plane.iter_mut().zip(updates) {
                for (context, updates) in band.iter_mut().zip(updates) {
                    for (probability, &update) in context.iter_mut().zip(updates) {
                        if reader.bit(update) != 0 {
                            *probability = reader.probability();
                        }
                    }
                }
            }
        }
    }

    /// Replace the intra-mode and vector probabilities an inter frame's
    /// header updates (RFC 6386 §9.10, §17.2).
    fn read_inter_updates(&mut self, reader: &mut Bool<'_>) {
        if reader.flag() {
            for probability in &mut self.ymode {
                *probability = reader.probability();
            }
        }
        if reader.flag() {
            for probability in &mut self.uvmode {
                *probability = reader.probability();
            }
        }
        for (component, updates) in self.mv.iter_mut().zip(&MV_UPDATE_PROBS) {
            for (probability, &update) in component.iter_mut().zip(updates) {
                if reader.bit(update) != 0 {
                    // Seven bits doubled, nought standing for one: a vector
                    // probability is never nought.
                    let value = reader.literal(7) << 1;
                    *probability = u8::try_from(value).map_or(u8::MAX, |value| value.max(1));
                }
            }
        }
    }
}

/// What a stream's headers carry from one frame to the next, which a
/// keyframe resets.
#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamState {
    /// The probabilities the next frame starts from.
    probs: Probabilities,
    segmentation: Segmentation,
    deltas: FilterDeltas,
}

impl StreamState {
    /// What a keyframe resets a stream to.
    const fn new() -> Self {
        Self {
            probs: Probabilities::DEFAULT,
            segmentation: Segmentation::none(),
            deltas: FilterDeltas::none(),
        }
    }
}

/// A reference frame an inter macroblock predicts from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Buffer {
    /// The frame decoded last.
    Last,
    /// The golden frame.
    Golden,
    /// The alternate reference frame.
    Alternate,
}

/// What a frame leaves in the golden or alternate buffer once it is decoded
/// (RFC 6386 §9.7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BufferUpdate {
    /// What it held.
    Keep,
    /// This frame.
    Refresh,
    /// A copy of another buffer.
    CopyOf(Buffer),
}

impl BufferUpdate {
    /// Read what a buffer takes: this frame where `refreshed`, else the copy
    /// field, which names the last frame or `other`.
    fn read(reader: &mut Bool<'_>, refreshed: bool, other: Buffer) -> Result<Self, Vp8Error> {
        if refreshed {
            return Ok(Self::Refresh);
        }
        match reader.literal(2) {
            0 => Ok(Self::Keep),
            1 => Ok(Self::CopyOf(Buffer::Last)),
            2 => Ok(Self::CopyOf(other)),
            _ => Err(Vp8Error::ReservedBufferCopy),
        }
    }
}

/// What an inter frame does with the golden and alternate buffers, and which
/// way vectors into them point.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Buffers {
    golden: BufferUpdate,
    alternate: BufferUpdate,
    /// Whether vectors into the golden frame point opposite to vectors into
    /// the last, so a neighbour predicting from the one lends its vector to a
    /// macroblock predicting from the other reversed.
    sign_bias_golden: bool,
    /// Likewise for the alternate frame.
    sign_bias_alternate: bool,
}

impl Buffers {
    /// Read the buffer fields an inter frame's own part of the header opens
    /// with.
    fn read(reader: &mut Bool<'_>) -> Result<Self, Vp8Error> {
        let refresh_golden = reader.flag();
        let refresh_alternate = reader.flag();
        let golden = BufferUpdate::read(reader, refresh_golden, Buffer::Alternate)?;
        let alternate = BufferUpdate::read(reader, refresh_alternate, Buffer::Golden)?;
        let sign_bias_golden = reader.flag();
        let sign_bias_alternate = reader.flag();
        Ok(Self {
            golden,
            alternate,
            sign_bias_golden,
            sign_bias_alternate,
        })
    }

    /// Whether vectors into `buffer` point opposite to vectors into the last
    /// frame.
    fn sign_bias(&self, buffer: Buffer) -> bool {
        match buffer {
            Buffer::Last => false,
            Buffer::Golden => self.sign_bias_golden,
            Buffer::Alternate => self.sign_bias_alternate,
        }
    }
}

/// What only an inter frame's header declares (RFC 6386 §9.7–§9.10).
#[derive(Clone, Debug, Eq, PartialEq)]
struct InterHeader {
    buffers: Buffers,
    /// Whether this frame becomes the last frame.
    refresh_last: bool,
    /// The probability a macroblock is predicted from its own frame.
    prob_intra: u8,
    /// The probability one predicted from another frame uses the last.
    prob_last: u8,
    /// The probability one using neither its own frame nor the last uses the
    /// golden frame rather than the alternate.
    prob_golden: u8,
}

impl InterHeader {
    /// Read the fields an inter frame's header closes with, after the skip
    /// probability, replacing the probabilities they update in `probs`; `None`
    /// for a keyframe, which read no `buffers`.
    fn read(
        reader: &mut Bool<'_>,
        buffers: Option<Buffers>,
        refresh_last: bool,
        probs: &mut Probabilities,
    ) -> Option<Self> {
        let buffers = buffers?;
        let prob_intra = reader.probability();
        let prob_last = reader.probability();
        let prob_golden = reader.probability();
        probs.read_inter_updates(reader);
        Some(Self {
            buffers,
            refresh_last,
            prob_intra,
            prob_last,
            prob_golden,
        })
    }
}

/// Everything a frame's header declares.
#[derive(Debug, Eq, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    simple_filter: bool,
    filter_level: i32,
    sharpness: u32,
    segmentation: Segmentation,
    deltas: FilterDeltas,
    quant: Quant,
    partitions: usize,
    /// The probabilities this frame decodes with.
    probs: Probabilities,
    skip_probability: Option<u8>,
    /// Whether the probabilities this frame decodes with carry on to the next.
    refresh_entropy: bool,
    /// What only an inter frame declares, `None` for a keyframe.
    inter: Option<InterHeader>,
}

impl Header {
    /// The skip flag a macroblock header carries where the frame lets
    /// macroblocks skip their residuals.
    fn read_skip(&self, reader: &mut Bool<'_>) -> bool {
        self.skip_probability
            .is_some_and(|probability| reader.bit(probability) != 0)
    }
}

/// What a frame's tag declares (RFC 6386 §9.1).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Tag {
    keyframe: bool,
    /// The version, which names how inter prediction interpolates.
    version: u32,
    /// Whether the stream shows the frame once it is decoded.
    shown: bool,
    /// The bytes the first partition occupies.
    first_partition: usize,
}

impl Tag {
    fn read(bytes: &[u8]) -> Result<Self, Vp8Error> {
        let head = bytes.get(..TAG_BYTES).ok_or(Vp8Error::Truncated)?;
        let tag = u32::from(head[0]) | (u32::from(head[1]) << 8) | (u32::from(head[2]) << 16);
        Ok(Self {
            keyframe: tag & 1 == 0,
            version: (tag >> 1) & 0x7,
            shown: tag & 0x10 != 0,
            first_partition: usize::try_from(tag >> 5).unwrap_or(0),
        })
    }

    /// The three bytes that read as this tag, or `None` for a first partition
    /// longer than its nineteen bits name.
    #[cfg(any(test, feature = "fixture", feature = "encode"))]
    fn bytes(&self) -> Option<[u8; TAG_BYTES]> {
        let size = u32::try_from(self.first_partition).ok();
        let size = size.filter(|&size| size < 1 << 19)?;
        let flags = u32::from(!self.keyframe) | (self.version << 1) | (u32::from(self.shown) << 4);
        let [low, middle, high, _] = (flags | (size << 5)).to_le_bytes();
        Some([low, middle, high])
    }
}

/// What a keyframe's uncompressed header carries after its tag: the start
/// code, then its width and height at no scale.
#[cfg(any(test, feature = "fixture", feature = "encode"))]
fn keyframe_header(width: u32, height: u32) -> [u8; UNCOMPRESSED_HEADER - TAG_BYTES] {
    let [code, start, end] = START_CODE;
    let [width_low, width_high, ..] = width.to_le_bytes();
    let [height_low, height_high, ..] = height.to_le_bytes();
    [
        code,
        start,
        end,
        width_low,
        width_high,
        height_low,
        height_high,
    ]
}

/// Read the uncompressed part of a keyframe header.
fn dimensions(bytes: &[u8]) -> Result<(u32, u32), Vp8Error> {
    let head = bytes
        .get(..UNCOMPRESSED_HEADER)
        .ok_or(Vp8Error::Truncated)?;
    let tag = Tag::read(head)?;
    if !tag.keyframe {
        return Err(Vp8Error::Interframe);
    }
    if Interpolation::of_version(tag.version).is_none() {
        return Err(Vp8Error::UnsupportedProfile);
    }
    if head[3..6] != START_CODE {
        return Err(Vp8Error::BadStartCode);
    }
    // The two scale fields above each dimension ask a display to stretch
    // the picture; the decoded picture is the size declared.
    let width = u32::from(u16::from_le_bytes([head[6], head[7]])) & MAX_DIMENSION;
    let height = u32::from(u16::from_le_bytes([head[8], head[9]])) & MAX_DIMENSION;
    if width == 0 || height == 0 {
        return Err(Vp8Error::InvalidGeometry);
    }
    Ok((width, height))
}

/// Read the geometry a keyframe declares, decoding no pixels.
///
/// # Errors
///
/// Why the frame's header is refused.
pub fn probe(bytes: &[u8]) -> Result<(u32, u32), Vp8Error> {
    dimensions(bytes)
}

/// An upper bound of the bytes decoding a keyframe `width` wide holds,
/// whatever its height: its partitions, the macroblock row it reconstructs
/// and the rows its loop filter and colours still need, per-macroblock state
/// for one row, and the colour row it hands over.
pub fn frame_peak_bytes(width: u32) -> u64 {
    use core::mem::size_of;
    let columns = usize::try_from(width.div_ceil(16)).unwrap_or(usize::MAX);
    let plane = |width: Option<usize>, rows: usize| {
        width
            .and_then(|width| Plane::len(width, rows))
            .map_or(u64::MAX, |len| len as u64)
    };
    let (luma, chroma) = (columns.checked_mul(16), columns.checked_mul(8));
    let planes = [
        plane(luma, 16),
        plane(chroma, 8),
        plane(chroma, 8),
        plane(luma, LUMA_HELD + 16),
        plane(chroma, CHROMA_HELD + 8),
        plane(chroma, CHROMA_HELD + 8),
    ];
    let per_column = size_of::<Edges>() + COLUMN_CONTEXT_BYTES;
    let state = [
        (columns as u64).saturating_mul(per_column as u64),
        (MAX_PARTITIONS * size_of::<Bool<'static>>()) as u64,
        u64::from(width) * size_of::<Rgba8>() as u64,
    ];
    planes.into_iter().chain(state).fold(0, u64::saturating_add)
}

/// Decode a `VP8 ` keyframe, handing its opaque RGBA8 rows to `row` top
/// first.
///
/// A macroblock row is reconstructed against the unfiltered bottom row of the
/// one above — what the format predicts from — and filtered once it is whole,
/// one row behind, so neither the picture nor a whole plane is ever held: only
/// the row being built and the rows the filter and the colours still need.
/// That is at most [`frame_peak_bytes`] of the width [`probe`] reads, so a
/// caller bounds a decode by checking the probed geometry first.
///
/// # Errors
///
/// Why the frame is refused, converted into the caller's error, or what `row`
/// answered; nothing is handed past a refusal.
pub fn decode_rows<E: From<Vp8Error>>(
    bytes: &[u8],
    mut row: impl FnMut(&[Rgba8]) -> Result<(), E>,
) -> Result<(), E> {
    let (width, height) = dimensions(bytes)?;
    let size = Tag::read(bytes)?.first_partition;
    let rest = bytes
        .get(UNCOMPRESSED_HEADER..)
        .ok_or(Vp8Error::Truncated)?;
    let head = rest.get(..size).ok_or(Vp8Error::Truncated)?;
    let mut reader = Bool::new(head);
    let header = read_header(&mut reader, true, width, height, &mut StreamState::new())?;
    let mut tokens = split_partitions(
        rest.get(size..).ok_or(Vp8Error::Truncated)?,
        header.partitions,
    )?;
    let mut frame = Frame::new(&header)?;
    frame.decode(&header, &mut reader, &mut tokens, &mut row)?;
    if reader.exhausted() {
        return Err(Vp8Error::Truncated.into());
    }
    Ok(())
}

/// Read a frame's compressed header against what the stream carries, which a
/// keyframe resets first. `state` takes the header's updates only once it is
/// read whole, and its probability updates only where it says they carry on.
fn read_header(
    reader: &mut Bool<'_>,
    keyframe: bool,
    width: u32,
    height: u32,
    state: &mut StreamState,
) -> Result<Header, Vp8Error> {
    let mut next = if keyframe {
        if reader.flag() {
            return Err(Vp8Error::ReservedColourSpace);
        }
        // Whether reconstruction needs clamping. Clamping regardless is correct
        // either way, so this is read and not acted on.
        let _clamping = reader.flag();
        StreamState::new()
    } else {
        state.clone()
    };
    next.segmentation.read(reader);
    let simple_filter = reader.flag();
    let filter_level = i32::try_from(reader.literal(6)).unwrap_or(0);
    let sharpness = reader.literal(3);
    next.deltas.read(reader);
    let partitions = 1usize << reader.literal(2).min(3);
    let quant = Quant::read(reader);
    let buffers = if keyframe {
        None
    } else {
        Some(Buffers::read(reader)?)
    };
    let refresh_entropy = reader.flag();
    let refresh_last = buffers.is_some() && reader.flag();
    let mut probs = next.probs.clone();
    probs.read_coeff_updates(reader);
    let skip_probability = reader.flag().then(|| reader.probability());
    let inter = InterHeader::read(reader, buffers, refresh_last, &mut probs);
    let header = Header {
        width,
        height,
        simple_filter,
        filter_level,
        sharpness,
        segmentation: next.segmentation.clone(),
        deltas: next.deltas.clone(),
        quant,
        partitions,
        probs,
        skip_probability,
        refresh_entropy,
        inter,
    };
    if header.refresh_entropy {
        next.probs.clone_from(&header.probs);
    }
    *state = next;
    Ok(header)
}

/// Split the residual partitions from their declared sizes.
fn split_partitions(bytes: &[u8], count: usize) -> Result<Vec<Bool<'_>>, Vp8Error> {
    let count = count.min(MAX_PARTITIONS);
    let table = (count - 1)
        .checked_mul(3)
        .ok_or(Vp8Error::InvalidPartitions)?;
    let sizes = bytes.get(..table).ok_or(Vp8Error::InvalidPartitions)?;
    let mut body = bytes.get(table..).ok_or(Vp8Error::InvalidPartitions)?;
    let mut partitions = Vec::new();
    if !fallible::reserve(&mut partitions, count) {
        return Err(Vp8Error::OutOfMemory);
    }
    for entry in sizes.as_chunks::<3>().0 {
        let size = usize::try_from(
            u32::from(entry[0]) | (u32::from(entry[1]) << 8) | (u32::from(entry[2]) << 16),
        )
        .unwrap_or(usize::MAX);
        let (partition, remainder) = body
            .split_at_checked(size)
            .ok_or(Vp8Error::InvalidPartitions)?;
        partitions.push(Bool::new(partition));
        body = remainder;
    }
    partitions.push(Bool::new(body));
    Ok(partitions)
}

/// Rows above a macroblock row its top edges read: the three they move, and
/// one beyond.
const EDGE_REACH: usize = 4;

/// Luma rows the filter window keeps above the macroblock row it filters:
/// those the row's top edge reads, widened by the two whose colours still
/// wait on chroma, which settles three of its rows behind and covers two of
/// luma's each.
const LUMA_HELD: usize = EDGE_REACH + 2;

/// Chroma rows the filter window keeps above the macroblock row it filters:
/// those the row's top edge reads.
const CHROMA_HELD: usize = EDGE_REACH;

/// The planes a keyframe reconstructs into, a macroblock row at a time, and
/// the contexts its walk carries.
struct Frame {
    /// The macroblock row being reconstructed, unfiltered. Its bordering row
    /// is the unfiltered bottom row of the row above, which is what the
    /// format predicts from.
    current: Planes,
    window: Window,
    columns: usize,
    rows: usize,
    /// What the loop filter does at each of the current row's macroblocks,
    /// which it reads once the row is whole.
    edges: Vec<Edges>,
    tokens: Tokens,
    modes: IntraContexts,
}

/// The rows the loop filter and the colour conversion still need: the
/// filtered tail of the macroblock row above, then the row being filtered.
struct Window {
    planes: Planes,
}

impl Frame {
    fn new(header: &Header) -> Result<Self, Vp8Error> {
        let columns = usize::try_from(header.width.div_ceil(16)).unwrap_or(0);
        let rows = usize::try_from(header.height.div_ceil(16)).unwrap_or(0);
        let luma = columns
            .checked_mul(16)
            .ok_or(Vp8Error::DimensionsOverflow)?;
        let chroma = luma / 2;
        Ok(Self {
            current: Planes::new(luma, 16)?,
            window: Window {
                planes: Planes {
                    luma: Plane::new(luma, LUMA_HELD + 16)?,
                    blue_chroma: Plane::new(chroma, CHROMA_HELD + 8)?,
                    red_chroma: Plane::new(chroma, CHROMA_HELD + 8)?,
                },
            },
            columns,
            rows,
            edges: fallible::filled(columns, Edges::default()).ok_or(Vp8Error::OutOfMemory)?,
            tokens: Tokens::new(columns)?,
            modes: IntraContexts::new(columns)?,
        })
    }

    /// Read every macroblock's modes and residuals, reconstruct and filter it
    /// a row at a time, and hand each picture row to `out` once it is
    /// settled.
    fn decode<E: From<Vp8Error>>(
        &mut self,
        header: &Header,
        modes: &mut Bool<'_>,
        tokens: &mut [Bool<'_>],
        out: &mut impl FnMut(&[Rgba8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let factors = segment_factors(header);
        let height = usize::try_from(header.height).unwrap_or(0);
        let width = usize::try_from(header.width).unwrap_or(0);
        let mut line = fallible::filled(width, [0u8; 4]).ok_or(Vp8Error::OutOfMemory)?;
        let mut handed = 0;
        let held = (
            isize::try_from(LUMA_HELD).unwrap_or(0),
            isize::try_from(CHROMA_HELD).unwrap_or(0),
        );
        for row in 0..self.rows {
            self.tokens.start_row();
            self.modes.start_row();
            let partition = row % tokens.len().max(1);
            for column in 0..self.columns {
                let block = self.modes.read(header, modes, column);
                let reader = tokens
                    .get_mut(partition)
                    .ok_or(Vp8Error::InvalidPartitions)?;
                let has_y2 = block.ymode != B_PRED;
                let coding = ResidualCoding {
                    skip: block.skip,
                    has_y2,
                    factor: factors
                        .get(usize::from(block.segment))
                        .copied()
                        .unwrap_or_default(),
                };
                let mut coeffs = [[0i16; BLOCK_COEFFS]; MB_BLOCKS];
                let coded = self.tokens.macroblock(
                    reader,
                    &header.probs.coeff,
                    column,
                    coding,
                    &mut coeffs,
                )?;
                let x0 = isize::try_from(column * 16).unwrap_or(0);
                let intra = (block.ymode, &block.bmodes, block.uvmode);
                self.current
                    .reconstruct_intra(intra, (x0, 0), row > 0, &mut coeffs);
                let level = filter_level(header, block.segment, DeltaIndex::intra(block.ymode));
                if let Some(slot) = self.edges.get_mut(column) {
                    *slot = Edges::new(level, coded || !has_y2);
                }
            }
            self.current.luma.extend_row(15);
            self.current.blue_chroma.extend_row(7);
            self.current.red_chroma.extend_row(7);
            self.window.take(&self.current);
            self.current.luma.carry(15);
            self.current.blue_chroma.carry(7);
            self.current.red_chroma.carry(7);
            filter_row(&mut self.window.planes, header, &self.edges, held, row > 0);
            // Filtering the next row moves this one's last three luma rows and
            // last three chroma rows, so its colours are settled to its ninth
            // luma row; the last row settles whole.
            let settled = if row + 1 == self.rows {
                height
            } else {
                (row * 16 + 10).min(height)
            };
            while handed < settled {
                let luma_row = (handed + LUMA_HELD).checked_sub(row * 16);
                let chroma_row = (handed / 2 + CHROMA_HELD).checked_sub(row * 8);
                let (Some(luma_row), Some(chroma_row)) = (luma_row, chroma_row) else {
                    return Err(Vp8Error::DimensionsOverflow.into());
                };
                self.window.planes.rgba_row(luma_row, chroma_row, &mut line);
                out(&line)?;
                handed += 1;
            }
            self.window.advance();
        }
        Ok(())
    }
}

/// The bytes per macroblock column the contexts a frame's walk carries hold:
/// the token contexts and the subblock modes of the row above.
const COLUMN_CONTEXT_BYTES: usize = size_of::<Nonzero>() + 4 * size_of::<u8>();

/// How a macroblock's residual is coded: whether it is skipped, whether a
/// luma DC block carries its luma blocks' DC terms, and its segment's
/// dequantisation factors.
#[derive(Copy, Clone)]
struct ResidualCoding {
    skip: bool,
    has_y2: bool,
    factor: Dequant,
}

/// The token contexts a frame's walk carries: whether each block bordering
/// the next to be read coded past its first position.
struct Tokens {
    /// The bottom blocks of each macroblock of the row above.
    above: Vec<Nonzero>,
    /// The right-hand blocks of the macroblock to the left.
    left: Nonzero,
}

impl Tokens {
    fn new(columns: usize) -> Result<Self, Vp8Error> {
        Ok(Self {
            above: fallible::filled(columns, Nonzero::default()).ok_or(Vp8Error::OutOfMemory)?,
            left: Nonzero::default(),
        })
    }

    /// Begin a frame, above whose top row no block lies.
    fn start_frame(&mut self) {
        self.above.fill(Nonzero::default());
    }

    /// Begin a macroblock row, left of whose first macroblock no block lies.
    fn start_row(&mut self) {
        self.left = Nonzero::default();
    }

    /// Read the residual of the macroblock in `column` into `coeffs`, or clear
    /// the contexts a skipped one leaves, answering whether any block coded
    /// past its first position. A partition that runs out before the
    /// macroblock is read whole is refused.
    fn macroblock(
        &mut self,
        reader: &mut Bool<'_>,
        probs: &CoeffProbs,
        column: usize,
        coding: ResidualCoding,
        coeffs: &mut [[i16; BLOCK_COEFFS]; MB_BLOCKS],
    ) -> Result<bool, Vp8Error> {
        let coded = if coding.skip {
            self.skip(column, coding.has_y2);
            false
        } else {
            self.read(reader, probs, column, coding, coeffs)
        };
        if reader.exhausted() {
            return Err(Vp8Error::Truncated);
        }
        Ok(coded)
    }

    /// Clear the token contexts a skipped macroblock leaves behind.
    ///
    /// A skipped macroblock coded no coefficients, so every block of it
    /// reads as zero to its neighbours — except the luma DC block of a
    /// macroblock that codes none, so that context passes through untouched.
    fn skip(&mut self, column: usize, has_y2: bool) {
        self.left = Nonzero {
            y2: !has_y2 && self.left.y2,
            ..Nonzero::default()
        };
        if let Some(above) = self.above.get_mut(column) {
            *above = Nonzero {
                y2: !has_y2 && above.y2,
                ..Nonzero::default()
            };
        }
    }

    /// Read one macroblock's residual tokens, answering whether any block
    /// coded past its first position.
    fn read(
        &mut self,
        reader: &mut Bool<'_>,
        probs: &CoeffProbs,
        column: usize,
        coding: ResidualCoding,
        coeffs: &mut [[i16; BLOCK_COEFFS]; MB_BLOCKS],
    ) -> bool {
        let ResidualCoding { has_y2, factor, .. } = coding;
        self.each_block(column, has_y2, |at| match coeffs.get_mut(at.index) {
            Some(block) => {
                let factors = factor.of_block(at.index);
                read_block(
                    reader, probs, at.plane, at.context, at.first, factors, block,
                )
            }
            None => false,
        })
    }

    /// The contexts the macroblock in `column` is coded beside: the
    /// right-hand blocks of the one to its left and the bottom blocks of the
    /// one above.
    fn around(&self, column: usize) -> (Nonzero, Nonzero) {
        let above = self.above.get(column).copied().unwrap_or_default();
        (self.left, above)
    }

    /// Visit each block of the macroblock in `column` as [`walk_blocks`]
    /// does, and carry the contexts it leaves into those its right and lower
    /// neighbours read; answers whether any block coded past its first
    /// position.
    fn each_block(
        &mut self,
        column: usize,
        has_y2: bool,
        code: impl FnMut(BlockAt) -> bool,
    ) -> bool {
        let (left, above, coded) = walk_blocks(self.around(column), has_y2, code);
        self.left = left;
        if let Some(slot) = self.above.get_mut(column) {
            *slot = above;
        }
        coded
    }
}

/// One block of a macroblock as its tokens are coded: its index among the
/// macroblock's blocks, the plane its probabilities are drawn from, the
/// context its first token is coded under, and the position its tokens begin
/// at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct BlockAt {
    index: usize,
    plane: usize,
    context: usize,
    first: usize,
}

/// Visit each block of a macroblock in the order its tokens are coded — the
/// luma DC block where `has_y2` says there is one, the luma blocks, then the
/// blue and the red — beside the contexts its left and above neighbours
/// leave, `code` coding each and answering whether its tokens went past its
/// first position. Answers the contexts the macroblock leaves to its right
/// and below, and whether any block's tokens went past its first position.
fn walk_blocks(
    (from_left, from_above): (Nonzero, Nonzero),
    has_y2: bool,
    mut code: impl FnMut(BlockAt) -> bool,
) -> (Nonzero, Nonzero, bool) {
    let mut coded = [false; MB_BLOCKS];
    if has_y2 {
        coded[Y2_BLOCK] = code(BlockAt {
            index: Y2_BLOCK,
            plane: 1,
            context: usize::from(from_left.y2) + usize::from(from_above.y2),
            first: 0,
        });
    }
    let (plane, first) = if has_y2 { (0, 1) } else { (3, 0) };
    for index in 0..LUMA_BLOCKS {
        let (row, column) = (index / 4, index % 4);
        let left = if column > 0 {
            coded[index - 1]
        } else {
            from_left.y[row]
        };
        let above = if row > 0 {
            coded[index - 4]
        } else {
            from_above.y[column]
        };
        coded[index] = code(BlockAt {
            index,
            plane,
            context: usize::from(left) + usize::from(above),
            first,
        });
    }
    let chroma = [
        (BLUE_BLOCKS, from_left.u, from_above.u),
        (RED_BLOCKS, from_left.v, from_above.v),
    ];
    for (base, beside, over) in chroma {
        for offset in 0..4 {
            let (row, column) = (offset / 2, offset % 2);
            let index = base + offset;
            let left = if column > 0 {
                coded[index - 1]
            } else {
                beside[row]
            };
            let above = if row > 0 {
                coded[index - 2]
            } else {
                over[column]
            };
            coded[index] = code(BlockAt {
                index,
                plane: 2,
                context: usize::from(left) + usize::from(above),
                first: 0,
            });
        }
    }
    let y2 = |kept: bool| if has_y2 { coded[Y2_BLOCK] } else { kept };
    let left = Nonzero {
        y: [coded[3], coded[7], coded[11], coded[15]],
        u: [coded[17], coded[19]],
        v: [coded[21], coded[23]],
        y2: y2(from_left.y2),
    };
    let above = Nonzero {
        y: [coded[12], coded[13], coded[14], coded[15]],
        u: [coded[18], coded[19]],
        v: [coded[22], coded[23]],
        y2: y2(from_above.y2),
    };
    (left, above, coded.contains(&true))
}

/// The subblock-mode contexts a keyframe's walk carries (RFC 6386 §11.3).
struct IntraContexts {
    /// The bottom row of subblock modes of the macroblock row above, one
    /// entry per luma subblock column.
    above: Vec<u8>,
    /// The right column of the macroblock to the left.
    left: [u8; 4],
}

impl IntraContexts {
    fn new(columns: usize) -> Result<Self, Vp8Error> {
        Ok(Self {
            above: fallible::filled(columns * 4, B_DC_PRED).ok_or(Vp8Error::OutOfMemory)?,
            left: [B_DC_PRED; 4],
        })
    }

    /// Begin a keyframe, above whose top row every subblock reads as
    /// averaging.
    fn start_frame(&mut self) {
        self.above.fill(B_DC_PRED);
    }

    /// Begin a macroblock row, left of which every subblock reads as
    /// averaging.
    fn start_row(&mut self) {
        self.left = [B_DC_PRED; 4];
    }

    /// The probabilities the mode of subblock `index` of the keyframe
    /// macroblock in `column` is coded with, the subblocks before it holding
    /// `bmodes`: those its above and left neighbours' modes select.
    fn bmode_probs(
        &self,
        column: usize,
        bmodes: &[u8; BLOCK_COEFFS],
        index: usize,
    ) -> &'static [u8; 9] {
        let table: &'static [[[u8; 9]; 10]; 10] = &KF_BMODE_PROBS;
        let (row, column_in_mb) = (index / 4, index % 4);
        let above = match index.checked_sub(4) {
            Some(up) => bmodes.get(up),
            None => self.above.get(column * 4 + column_in_mb),
        };
        let left = match column_in_mb {
            0 => self.left.get(row),
            _ => bmodes.get(index - 1),
        };
        let mode = |found: Option<&u8>| usize::from(found.copied().unwrap_or(B_DC_PRED));
        table
            .get(mode(above))
            .and_then(|by_left| by_left.get(mode(left)))
            .unwrap_or(&table[0][0])
    }

    /// Carry the subblock modes of the keyframe macroblock in `column` into
    /// the contexts its right and lower neighbours read.
    fn keep(&mut self, column: usize, bmodes: &[u8; BLOCK_COEFFS]) {
        self.left = [bmodes[3], bmodes[7], bmodes[11], bmodes[15]];
        if let Some(above) = self.above.get_mut(column * 4..column * 4 + 4) {
            above.copy_from_slice(&bmodes[12..]);
        }
    }

    /// Read the segment, skip flag, and intra modes of the keyframe
    /// macroblock in `column`.
    fn read(&mut self, header: &Header, reader: &mut Bool<'_>, column: usize) -> Macroblock {
        let segment = header.segmentation.read_id(reader).unwrap_or(0);
        let skip = header.read_skip(reader);
        let ymode = reader.tree(&KF_YMODE_TREE, &KF_YMODE_PROBS, 0);
        let bmodes = if ymode == B_PRED {
            let mut bmodes = [B_DC_PRED; BLOCK_COEFFS];
            for index in 0..LUMA_BLOCKS {
                let probs = self.bmode_probs(column, &bmodes, index);
                bmodes[index] = reader.tree(&BMODE_TREE, probs, 0);
            }
            bmodes
        } else {
            implied_bmodes(ymode)
        };
        self.keep(column, &bmodes);
        let uvmode = reader.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS, 0);
        Macroblock {
            segment,
            skip,
            ymode,
            uvmode,
            bmodes,
        }
    }

    /// Write the keyframe macroblock `block` in `column` so that
    /// [`Self::read`] reads it back.
    #[cfg(any(test, feature = "encode"))]
    fn write(&mut self, header: &Header, writer: &mut Writer, column: usize, block: &Macroblock) {
        header.segmentation.write_id(writer, block.segment);
        if let Some(probability) = header.skip_probability {
            writer.bit(probability, block.skip);
        }
        if let Some(path) = KF_YMODE_PATHS.get(usize::from(block.ymode)) {
            write_path(writer, path, &KF_YMODE_PROBS);
        }
        let bmodes = if block.ymode == B_PRED {
            for (index, &mode) in block.bmodes.iter().enumerate() {
                let probs = self.bmode_probs(column, &block.bmodes, index);
                if let Some(path) = BMODE_PATHS.get(usize::from(mode)) {
                    write_path(writer, path, probs);
                }
            }
            block.bmodes
        } else {
            implied_bmodes(block.ymode)
        };
        self.keep(column, &bmodes);
        if let Some(path) = UV_MODE_PATHS.get(usize::from(block.uvmode)) {
            write_path(writer, path, &KF_UV_MODE_PROBS);
        }
    }
}

/// The subblock modes a keyframe macroblock predicted whole by `ymode` lends
/// its neighbours' contexts.
fn implied_bmodes(ymode: u8) -> [u8; BLOCK_COEFFS] {
    let implied = MODE_TO_BMODE.get(usize::from(ymode)).copied();
    [implied.unwrap_or(B_DC_PRED); BLOCK_COEFFS]
}

/// Read one transform block's tokens, dequantise them into `coeffs`, and
/// answer whether they went past the block's first position, which is what
/// its neighbours' contexts and the loop filter ask. That is the reference
/// decoder's test: the specification's prose asks for a non-zero coefficient,
/// which a block coding only zeros to its end does not have.
fn read_block(
    reader: &mut Bool<'_>,
    probs: &CoeffProbs,
    plane: usize,
    context: usize,
    first: usize,
    (dc, ac): (i32, i32),
    coeffs: &mut [i16; BLOCK_COEFFS],
) -> bool {
    let mut context = context;
    let mut previous_zero = false;
    for scan in first..BLOCK_COEFFS {
        let band = COEFF_BANDS.get(scan).copied().unwrap_or(0);
        let table = probs
            .get(plane)
            .and_then(|plane| plane.get(usize::from(band)))
            .and_then(|band| band.get(context))
            .unwrap_or(&probs[0][0][0]);
        let start = if previous_zero { TREE_AFTER_EOB } else { 0 };
        let token = reader.tree(&COEFF_TREE, table, start);
        if token == DCT_EOB {
            return scan > first;
        }
        if token == DCT_0 {
            context = 0;
            previous_zero = true;
            continue;
        }
        let magnitude = if token < DCT_CAT1 {
            i32::from(token)
        } else {
            let category = usize::from(token - DCT_CAT1);
            let extra = CATEGORY_PROBS
                .get(category)
                .map_or(0, |bits| extra_bits(reader, bits));
            CATEGORY_BASES.get(category).copied().unwrap_or(0) + extra
        };
        context = if magnitude == 1 { 1 } else { 2 };
        previous_zero = false;
        let signed = if reader.flag() { -magnitude } else { magnitude };
        let factor = if scan == 0 { dc } else { ac };
        let position = ZIGZAG.get(scan).copied().unwrap_or(0);
        if let Some(slot) = coeffs.get_mut(usize::from(position)) {
            *slot = dequantise(signed, factor);
        }
    }
    true
}

/// A coefficient's `level` dequantised at `factor`, in the sixteen bits the
/// format stores it in: a conforming stream's ranges stay inside them, and a
/// product that does not saturates rather than wrapping a large positive into
/// a large negative.
fn dequantise(level: i32, factor: i32) -> i16 {
    let product = level.saturating_mul(factor);
    i16::try_from(product).unwrap_or(if product < 0 { i16::MIN } else { i16::MAX })
}

/// Read a large coefficient's extra bits, most significant first.
fn extra_bits(reader: &mut Bool<'_>, probs: &[u8]) -> i32 {
    let mut value = 0i32;
    for &probability in probs {
        value = 2 * value + i32::try_from(reader.bit(probability)).unwrap_or(0);
    }
    value
}

/// Clamp a filter temporary into signed eight bits, as the filters do.
fn clamp_s8(value: i32) -> i32 {
    value.clamp(-128, 127)
}

/// Clamp a reconstructed sample into pixel range.
fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

/// The weighted average of three adjacent samples, centred on the second.
fn avg3(x: u8, y: u8, z: u8) -> u8 {
    let sum = i32::from(x) + 2 * i32::from(y) + i32::from(z) + 2;
    clamp_u8(sum >> 2)
}

/// The average of two adjacent samples.
fn avg2(x: u8, y: u8) -> u8 {
    clamp_u8((i32::from(x) + i32::from(y) + 1) >> 1)
}

/// The inverse Walsh-Hadamard transform of a macroblock's luma DC block,
/// whose outputs become the sixteen luma blocks' DC coefficients.
fn inverse_walsh(input: &[i16; BLOCK_COEFFS]) -> [i16; BLOCK_COEFFS] {
    let mut middle = [0i32; BLOCK_COEFFS];
    for column in 0..4 {
        let at = |row: usize| i32::from(input[row * 4 + column]);
        let a = at(0) + at(3);
        let b = at(1) + at(2);
        let c = at(1) - at(2);
        let d = at(0) - at(3);
        middle[column] = a + b;
        middle[4 + column] = c + d;
        middle[8 + column] = a - b;
        middle[12 + column] = d - c;
    }
    let mut output = [0i16; BLOCK_COEFFS];
    for row in 0..4 {
        let at = |column: usize| middle[row * 4 + column];
        let a = at(0) + at(3);
        let b = at(1) + at(2);
        let c = at(1) - at(2);
        let d = at(0) - at(3);
        let write = |value: i32| i16::try_from((value + 3) >> 3).unwrap_or(0);
        output[row * 4] = write(a + b);
        output[row * 4 + 1] = write(c + d);
        output[row * 4 + 2] = write(a - b);
        output[row * 4 + 3] = write(d - c);
    }
    output
}

/// The inverse DCT of one transform block, answering its residual samples.
///
/// The intermediates are 64-bit because the second pass multiplies values
/// the first pass already widened: a conformant stream's coefficients keep
/// those products inside 32 bits, but a corrupt one's need not, and the
/// transform must answer a bounded residual rather than trap. Narrowing
/// saturates for the same reason. A conformant stream is unaffected — its
/// values are orders of magnitude inside the bound.
fn inverse_dct(input: &[i16; BLOCK_COEFFS]) -> [i32; BLOCK_COEFFS] {
    const SIN: i64 = SIN_PI8_SQRT2 as i64;
    const COS: i64 = COS_PI8_SQRT2_MINUS1 as i64;

    let mut middle = [0i64; BLOCK_COEFFS];
    for column in 0..4 {
        let at = |row: usize| i64::from(input[row * 4 + column]);
        let a = at(0) + at(2);
        let b = at(0) - at(2);
        let c = ((at(1) * SIN) >> 16) - (at(3) + ((at(3) * COS) >> 16));
        let d = (at(1) + ((at(1) * COS) >> 16)) + ((at(3) * SIN) >> 16);
        middle[column] = a + d;
        middle[12 + column] = a - d;
        middle[4 + column] = b + c;
        middle[8 + column] = b - c;
    }
    let mut output = [0i32; BLOCK_COEFFS];
    for row in 0..4 {
        let at = |column: usize| middle[row * 4 + column];
        let a = at(0) + at(2);
        let b = at(0) - at(2);
        let c = ((at(1) * SIN) >> 16) - (at(3) + ((at(3) * COS) >> 16));
        let d = (at(1) + ((at(1) * COS) >> 16)) + ((at(3) * SIN) >> 16);
        let write = |value: i64| narrow_residual((value + 4) >> 3);
        output[row * 4] = write(a + d);
        output[row * 4 + 3] = write(a - d);
        output[row * 4 + 1] = write(b + c);
        output[row * 4 + 2] = write(b - c);
    }
    output
}

/// Narrow a transform intermediate to a residual, saturating at a bound
/// low enough that adding it to a sample cannot overflow either.
fn narrow_residual(value: i64) -> i32 {
    const BOUND: i64 = (i32::MAX / 2) as i64;
    i32::try_from(value.clamp(-BOUND, BOUND)).unwrap_or(0)
}

/// Add one transform block's residual to the samples predicted for it.
///
/// A block whose only non-zero coefficient is its first has a constant
/// residual, which the full transform would reach the long way round.
fn add_residual(plane: &mut Plane, x0: isize, y0: isize, coeffs: &[i16; BLOCK_COEFFS]) {
    let ac_present = coeffs.iter().skip(1).any(|&value| value != 0);
    if !ac_present {
        if coeffs[0] == 0 {
            return;
        }
        let flat = (i32::from(coeffs[0]) + 4) >> 3;
        for y in y0..y0 + 4 {
            for sample in plane.row_mut(x0, y, 4) {
                *sample = clamp_u8(i32::from(*sample) + flat);
            }
        }
        return;
    }
    let residual = inverse_dct(coeffs);
    for (y, offsets) in (y0..).zip(residual.as_chunks::<4>().0) {
        for (sample, &offset) in plane.row_mut(x0, y, 4).iter_mut().zip(offsets) {
            *sample = clamp_u8(i32::from(*sample) + offset);
        }
    }
}

/// Where luma subblock `index` of the macroblock whose luma starts at
/// `origin` begins.
fn subblock_origin((x0, y0): (isize, isize), index: usize) -> (isize, isize) {
    let (column, row) = (index % 4, index / 4);
    (
        x0 + isize::try_from(column * 4).unwrap_or(0),
        y0 + isize::try_from(row * 4).unwrap_or(0),
    )
}

/// Predict a whole 16x16 or 8x8 block from its above row and left column.
/// `has_above` says whether a macroblock row lies above it, which the plane
/// cannot: it holds one macroblock row.
fn predict_block(plane: &mut Plane, x0: isize, y0: isize, size: isize, mode: u8, has_above: bool) {
    let has_left = x0 > 0;
    let len = usize::try_from(size).unwrap_or(0).min(MAX_PREDICTED);
    let mut above = [0u8; MAX_PREDICTED];
    let mut left = [0u8; MAX_PREDICTED];
    for ((up, side), offset) in above.iter_mut().zip(&mut left).zip(0..size).take(len) {
        *up = plane.get(x0 + offset, y0 - 1);
        *side = plane.get(x0 - 1, y0 + offset);
    }
    let (above, left) = (&above[..len], &left[..len]);
    let corner = i32::from(plane.get(x0 - 1, y0 - 1));
    // Every mode but V, H and TM averages what is available, and a block
    // with neither neighbour has nothing to average.
    let average = || {
        let mut sum = 0i32;
        let mut counted = 0i32;
        for (shown, samples) in [(has_above, above), (has_left, left)] {
            if shown {
                sum += samples.iter().map(|&sample| i32::from(sample)).sum::<i32>();
                counted += size_i32(len);
            }
        }
        if counted == 0 {
            128
        } else {
            let shift = counted.trailing_zeros();
            clamp_u8((sum + (1 << (shift - 1))) >> shift)
        }
    };
    let flat = match mode {
        V_PRED | H_PRED | TM_PRED => 0,
        _ => average(),
    };
    for (y, &side) in (y0..).zip(left) {
        let line = plane.row_mut(x0, y, len);
        match mode {
            V_PRED => {
                for (sample, &up) in line.iter_mut().zip(above) {
                    *sample = up;
                }
            }
            H_PRED => line.fill(side),
            TM_PRED => {
                for (sample, &up) in line.iter_mut().zip(above) {
                    *sample = clamp_u8(i32::from(side) + i32::from(up) - corner);
                }
            }
            _ => line.fill(flat),
        }
    }
}

/// The widest block a whole-block mode predicts: a macroblock's luma.
const MAX_PREDICTED: usize = 16;

/// `len` samples as the sum a block's average divides by.
fn size_i32(len: usize) -> i32 {
    i32::try_from(len).unwrap_or(0)
}

/// Predict one 4x4 luma subblock.
///
/// `above` holds the eight samples above the subblock — its own four
/// followed by the four above and to the right — and `corner` the sample
/// above and to the left of both.
fn predict_subblock(plane: &mut Plane, x0: isize, y0: isize, mode: u8, above: [u8; 8], corner: u8) {
    let left = [
        plane.get(x0 - 1, y0),
        plane.get(x0 - 1, y0 + 1),
        plane.get(x0 - 1, y0 + 2),
        plane.get(x0 - 1, y0 + 3),
    ];
    // The nine already-reconstructed edge samples the diagonal modes run
    // along, from the bottom of the left column round to the right of the
    // above row.
    let edge = [
        left[3], left[2], left[1], left[0], corner, above[0], above[1], above[2], above[3],
    ];
    let mut block = [[0u8; 4]; 4];
    let smooth3_above = |at: usize| avg3(above[at - 1], above[at], above[at + 1]);
    match mode {
        B_TM_PRED => {
            for row in 0..4 {
                for column in 0..4 {
                    block[row][column] = clamp_u8(
                        i32::from(left[row]) + i32::from(above[column]) - i32::from(corner),
                    );
                }
            }
        }
        B_VE_PRED => {
            let smoothed = [
                avg3(corner, above[0], above[1]),
                smooth3_above(1),
                smooth3_above(2),
                smooth3_above(3),
            ];
            block.fill(smoothed);
        }
        B_HE_PRED => {
            let smoothed = [
                avg3(corner, left[0], left[1]),
                avg3(left[0], left[1], left[2]),
                avg3(left[1], left[2], left[3]),
                avg3(left[2], left[3], left[3]),
            ];
            for (row, value) in block.iter_mut().zip(smoothed) {
                *row = [value; 4];
            }
        }
        // The six modes that subdivide the block into diagonal lines are
        // the bulk of the work and have no full-block analogue.
        B_LD_PRED | B_RD_PRED | B_VR_PRED | B_VL_PRED | B_HD_PRED | B_HU_PRED => {
            predict_diagonal(&mut block, mode, above, &edge, left);
        }
        // The remaining mode averages the eight samples around the
        // subblock's upper-left corner.
        _ => {
            let mut sum = 4i32;
            for (&up, &side) in above.iter().take(4).zip(&left) {
                sum += i32::from(up) + i32::from(side);
            }
            let value = clamp_u8(sum >> 3);
            block = [[value; 4]; 4];
        }
    }
    for (y, samples) in (y0..).zip(&block) {
        for (sample, &value) in plane.row_mut(x0, y, 4).iter_mut().zip(samples) {
            *sample = value;
        }
    }
}

/// Predict one 4x4 luma subblock under a diagonal mode.
///
/// Each mode assigns every pixel of a diagonal line the same value: a
/// smoothed or half-step version of an already-reconstructed edge sample
/// lying on that line. `edge` holds those samples from the bottom of the
/// left column round to the right of the above row.
fn predict_diagonal(
    block: &mut [[u8; 4]; 4],
    mode: u8,
    above: [u8; 8],
    edge: &[u8; 9],
    left: [u8; 4],
) {
    let smooth3 =
        |at: usize, samples: &[u8; 9]| avg3(samples[at - 1], samples[at], samples[at + 1]);
    let smooth3_above = |at: usize| avg3(above[at - 1], above[at], above[at + 1]);
    match mode {
        B_LD_PRED => {
            block[0][0] = smooth3_above(1);
            block[0][1] = smooth3_above(2);
            block[1][0] = block[0][1];
            block[0][2] = smooth3_above(3);
            block[1][1] = block[0][2];
            block[2][0] = block[0][2];
            block[0][3] = smooth3_above(4);
            block[1][2] = block[0][3];
            block[2][1] = block[0][3];
            block[3][0] = block[0][3];
            block[1][3] = smooth3_above(5);
            block[2][2] = block[1][3];
            block[3][1] = block[1][3];
            block[2][3] = smooth3_above(6);
            block[3][2] = block[2][3];
            block[3][3] = avg3(above[6], above[7], above[7]);
        }
        B_RD_PRED => {
            block[3][0] = smooth3(1, edge);
            block[3][1] = smooth3(2, edge);
            block[2][0] = block[3][1];
            block[3][2] = smooth3(3, edge);
            block[2][1] = block[3][2];
            block[1][0] = block[3][2];
            block[3][3] = smooth3(4, edge);
            block[2][2] = block[3][3];
            block[1][1] = block[3][3];
            block[0][0] = block[3][3];
            block[2][3] = smooth3(5, edge);
            block[1][2] = block[2][3];
            block[0][1] = block[2][3];
            block[1][3] = smooth3(6, edge);
            block[0][2] = block[1][3];
            block[0][3] = smooth3(7, edge);
        }
        // The remaining four use lines of slope two and one half, which
        // often need a sample synthesised midway between two real ones.
        _ => predict_shallow_diagonal(block, mode, above, edge, left),
    }
}

/// Predict one 4x4 luma subblock under a shallow diagonal mode, whose lines
/// run at roughly 27 degrees from an axis.
fn predict_shallow_diagonal(
    block: &mut [[u8; 4]; 4],
    mode: u8,
    above: [u8; 8],
    edge: &[u8; 9],
    left: [u8; 4],
) {
    let smooth3 =
        |at: usize, samples: &[u8; 9]| avg3(samples[at - 1], samples[at], samples[at + 1]);
    let smooth3_above = |at: usize| avg3(above[at - 1], above[at], above[at + 1]);
    let mean2 = |at: usize, samples: &[u8; 9]| avg2(samples[at], samples[at + 1]);
    match mode {
        B_VR_PRED => {
            block[3][0] = smooth3(2, edge);
            block[2][0] = smooth3(3, edge);
            block[3][1] = smooth3(4, edge);
            block[1][0] = block[3][1];
            block[2][1] = mean2(4, edge);
            block[0][0] = block[2][1];
            block[3][2] = smooth3(5, edge);
            block[1][1] = block[3][2];
            block[2][2] = mean2(5, edge);
            block[0][1] = block[2][2];
            block[3][3] = smooth3(6, edge);
            block[1][2] = block[3][3];
            block[2][3] = mean2(6, edge);
            block[0][2] = block[2][3];
            block[1][3] = smooth3(7, edge);
            block[0][3] = mean2(7, edge);
        }
        B_VL_PRED => {
            block[0][0] = avg2(above[0], above[1]);
            block[1][0] = smooth3_above(1);
            block[2][0] = avg2(above[1], above[2]);
            block[0][1] = block[2][0];
            block[1][1] = smooth3_above(2);
            block[3][0] = block[1][1];
            block[2][1] = avg2(above[2], above[3]);
            block[0][2] = block[2][1];
            block[3][1] = smooth3_above(3);
            block[1][2] = block[3][1];
            block[2][2] = avg2(above[3], above[4]);
            block[0][3] = block[2][2];
            block[3][2] = smooth3_above(4);
            block[1][3] = block[3][2];
            // The last two do not continue the pattern: no reconstructed
            // sample lies on their diagonals.
            block[2][3] = smooth3_above(5);
            block[3][3] = smooth3_above(6);
        }
        B_HD_PRED => {
            block[3][0] = avg2(edge[0], edge[1]);
            block[3][1] = smooth3(1, edge);
            block[2][0] = mean2(1, edge);
            block[3][2] = block[2][0];
            block[2][1] = smooth3(2, edge);
            block[3][3] = block[2][1];
            block[2][2] = mean2(2, edge);
            block[1][0] = block[2][2];
            block[2][3] = smooth3(3, edge);
            block[1][1] = block[2][3];
            block[1][2] = mean2(3, edge);
            block[0][0] = block[1][2];
            block[1][3] = smooth3(4, edge);
            block[0][1] = block[1][3];
            block[0][2] = smooth3(5, edge);
            block[0][3] = smooth3(6, edge);
        }
        B_HU_PRED => {
            block[0][0] = avg2(left[0], left[1]);
            block[0][1] = avg3(left[0], left[1], left[2]);
            block[0][2] = avg2(left[1], left[2]);
            block[1][0] = block[0][2];
            block[0][3] = avg3(left[1], left[2], left[3]);
            block[1][1] = block[0][3];
            block[1][2] = avg2(left[2], left[3]);
            block[2][0] = block[1][2];
            block[1][3] = avg3(left[2], left[3], left[3]);
            block[2][1] = block[1][3];
            block[2][2] = left[3];
            block[2][3] = left[3];
            block[3] = [left[3]; 4];
        }
        _ => {}
    }
}

/// A picture's luma plane and its two chroma planes at half its size.
struct Planes {
    luma: Plane,
    blue_chroma: Plane,
    red_chroma: Plane,
}

impl Planes {
    /// Planes of `width`×`height` luma samples, bordered as intra prediction
    /// reads them.
    fn new(width: usize, height: usize) -> Result<Self, Vp8Error> {
        Ok(Self {
            luma: Plane::new(width, height)?,
            blue_chroma: Plane::new(width / 2, height / 2)?,
            red_chroma: Plane::new(width / 2, height / 2)?,
        })
    }

    /// The bytes planes of `width`×`height` luma samples hold.
    fn len(width: usize, height: usize) -> Option<usize> {
        let chroma = Plane::len(width / 2, height / 2)?;
        Plane::len(width, height)?.checked_add(chroma.checked_mul(2)?)
    }

    /// The planes as a reference frame is read: `width`×`height` luma
    /// samples, and half that of each chroma plane.
    fn view(&self, width: usize, height: usize) -> Option<[RefPlane<'_>; 3]> {
        Some([
            self.luma.view(width, height)?,
            self.blue_chroma.view(width / 2, height / 2)?,
            self.red_chroma.view(width / 2, height / 2)?,
        ])
    }

    /// Predict the intra macroblock whose luma starts at (`x0`, `y0`) by its
    /// luma mode, its subblocks' where that is [`B_PRED`], and its chroma
    /// mode, and add its residual. `has_above` says whether a macroblock row
    /// lies above it.
    fn reconstruct_intra(
        &mut self,
        (ymode, bmodes, uvmode): (u8, &[u8; BLOCK_COEFFS], u8),
        (x0, y0): (isize, isize),
        has_above: bool,
        coeffs: &mut [[i16; BLOCK_COEFFS]; MB_BLOCKS],
    ) {
        let (cx, cy) = (x0 / 2, y0 / 2);
        predict_block(&mut self.blue_chroma, cx, cy, 8, uvmode, has_above);
        predict_block(&mut self.red_chroma, cx, cy, 8, uvmode, has_above);
        if ymode != B_PRED {
            predict_block(&mut self.luma, x0, y0, 16, ymode, has_above);
            self.add_macroblock_residual((x0, y0), true, coeffs);
            return;
        }
        let edge = self.above_right((x0, y0));
        for (index, residual) in coeffs.iter().enumerate().take(LUMA_BLOCKS) {
            self.predict_luma_subblock((x0, y0), index, bmodes[index], edge);
            let (x, y) = subblock_origin((x0, y0), index);
            add_residual(&mut self.luma, x, y, residual);
        }
        self.add_chroma_residual((cx, cy), coeffs);
    }

    /// The four samples above and to the right of the macroblock whose luma
    /// starts at (`x0`, `y0`), which its rightmost subblocks predict from:
    /// their own upper-right neighbours are not reconstructed yet.
    fn above_right(&self, (x0, y0): (isize, isize)) -> [u8; 4] {
        core::array::from_fn(|offset| {
            let x = x0 + 16 + isize::try_from(offset).unwrap_or(0);
            self.luma.get(x, y0 - 1)
        })
    }

    /// Predict luma subblock `index` of the macroblock whose luma starts at
    /// `origin` by subblock mode `mode`, `edge` being the samples above and to
    /// the right of the macroblock.
    fn predict_luma_subblock(
        &mut self,
        origin: (isize, isize),
        index: usize,
        mode: u8,
        edge: [u8; 4],
    ) {
        let (x, y) = subblock_origin(origin, index);
        let mut above: [u8; 8] = core::array::from_fn(|offset| {
            self.luma
                .get(x + isize::try_from(offset).unwrap_or(0), y - 1)
        });
        if index % 4 == 3 {
            above[4..].copy_from_slice(&edge);
        }
        let corner = self.luma.get(x - 1, y - 1);
        predict_subblock(&mut self.luma, x, y, mode, above, corner);
    }

    /// Add the residual of the macroblock whose luma starts at (`x0`, `y0`)
    /// to the samples predicted for it, its luma DC block's inverse transform
    /// first giving each luma block its DC term where `has_y2` says it has
    /// one.
    fn add_macroblock_residual(
        &mut self,
        (x0, y0): (isize, isize),
        has_y2: bool,
        coeffs: &mut [[i16; BLOCK_COEFFS]; MB_BLOCKS],
    ) {
        if has_y2 {
            let dc = inverse_walsh(&coeffs[Y2_BLOCK]);
            for (block, value) in coeffs.iter_mut().zip(dc) {
                block[0] = value;
            }
        }
        for (index, residual) in coeffs.iter().enumerate().take(LUMA_BLOCKS) {
            let (x, y) = subblock_origin((x0, y0), index);
            add_residual(&mut self.luma, x, y, residual);
        }
        self.add_chroma_residual((x0 / 2, y0 / 2), coeffs);
    }

    /// Add a macroblock's chroma residuals to the samples predicted for its
    /// chroma, which starts at (`cx`, `cy`).
    fn add_chroma_residual(
        &mut self,
        (cx, cy): (isize, isize),
        coeffs: &[[i16; BLOCK_COEFFS]; MB_BLOCKS],
    ) {
        for index in 0..4 {
            let x = cx + isize::try_from((index % 2) * 4).unwrap_or(0);
            let y = cy + isize::try_from((index / 2) * 4).unwrap_or(0);
            add_residual(&mut self.blue_chroma, x, y, &coeffs[16 + index]);
            add_residual(&mut self.red_chroma, x, y, &coeffs[20 + index]);
        }
    }

    /// The simple filter over the macroblock whose luma starts at (`x0`,
    /// `y0`), which covers luma edges only: its left and top edges where
    /// `left` and `top` say a macroblock lies beyond them, and its inner edges
    /// where `strength` says.
    fn filter_simple(
        &mut self,
        (x0, y0): (isize, isize),
        (left, top): (bool, bool),
        strength: &Strength,
    ) {
        if left {
            simple_edge(&mut self.luma, x0, y0, 16, true, strength.edge);
        }
        if strength.inner {
            for offset in [4, 8, 12] {
                simple_edge(&mut self.luma, x0 + offset, y0, 16, true, strength.inside);
            }
        }
        if top {
            simple_edge(&mut self.luma, x0, y0, 16, false, strength.edge);
        }
        if strength.inner {
            for offset in [4, 8, 12] {
                simple_edge(&mut self.luma, x0, y0 + offset, 16, false, strength.inside);
            }
        }
    }

    /// The normal filter over the macroblock whose luma starts at (`x0`,
    /// `y0`) and chroma at (`cx`, `cy`), which covers luma and both chroma
    /// planes.
    fn filter_normal(
        &mut self,
        (x0, y0): (isize, isize),
        (cx, cy): (isize, isize),
        (left, top): (bool, bool),
        strength: &Strength,
    ) {
        if left {
            macroblock_edge(&mut self.luma, x0, y0, 16, true, strength);
            macroblock_edge(&mut self.blue_chroma, cx, cy, 8, true, strength);
            macroblock_edge(&mut self.red_chroma, cx, cy, 8, true, strength);
        }
        if strength.inner {
            for offset in [4, 8, 12] {
                subblock_edge(&mut self.luma, x0 + offset, y0, 16, true, strength);
            }
            subblock_edge(&mut self.blue_chroma, cx + 4, cy, 8, true, strength);
            subblock_edge(&mut self.red_chroma, cx + 4, cy, 8, true, strength);
        }
        if top {
            macroblock_edge(&mut self.luma, x0, y0, 16, false, strength);
            macroblock_edge(&mut self.blue_chroma, cx, cy, 8, false, strength);
            macroblock_edge(&mut self.red_chroma, cx, cy, 8, false, strength);
        }
        if strength.inner {
            for offset in [4, 8, 12] {
                subblock_edge(&mut self.luma, x0, y0 + offset, 16, false, strength);
            }
            subblock_edge(&mut self.blue_chroma, cx, cy + 4, 8, false, strength);
            subblock_edge(&mut self.red_chroma, cx, cy + 4, 8, false, strength);
        }
    }

    /// Convert luma row `luma_row`, over chroma row `chroma_row`, into
    /// `line`'s opaque RGBA8 pixels.
    fn rgba_row(&self, luma_row: usize, chroma_row: usize, line: &mut [Rgba8]) {
        let y = isize::try_from(luma_row).unwrap_or(0);
        let cy = isize::try_from(chroma_row).unwrap_or(0);
        for (x, pixel) in (0..).zip(line) {
            let (blue, red) = (
                self.blue_chroma.get(x / 2, cy),
                self.red_chroma.get(x / 2, cy),
            );
            *pixel = colour::rgba(self.luma.get(x, y), blue, red);
        }
    }
}

impl Window {
    /// Take the macroblock row `row` holds, just reconstructed, below the
    /// rows held.
    fn take(&mut self, row: &Planes) {
        self.planes
            .luma
            .rows_mut(LUMA_HELD..LUMA_HELD + 16)
            .copy_from_slice(row.luma.rows(0..16));
        self.planes
            .blue_chroma
            .rows_mut(CHROMA_HELD..CHROMA_HELD + 8)
            .copy_from_slice(row.blue_chroma.rows(0..8));
        self.planes
            .red_chroma
            .rows_mut(CHROMA_HELD..CHROMA_HELD + 8)
            .copy_from_slice(row.red_chroma.rows(0..8));
    }

    /// Keep the filtered rows the next macroblock row still needs, moving
    /// them to the top.
    fn advance(&mut self) {
        let planes = &mut self.planes;
        for (plane, rows, held) in [
            (&mut planes.luma, 16, LUMA_HELD),
            (&mut planes.blue_chroma, 8, CHROMA_HELD),
            (&mut planes.red_chroma, 8, CHROMA_HELD),
        ] {
            let stride = plane.stride;
            let from = (rows + 1) * stride;
            plane
                .samples
                .copy_within(from..from + held * stride, stride);
        }
    }
}

/// The thresholds one macroblock's edges are filtered under.
struct Strength {
    edge: i32,
    inside: i32,
    interior: i32,
    hev: i32,
    inner: bool,
}

impl Strength {
    /// The thresholds of a macroblock at filter `level` in a frame of
    /// `sharpness`, a keyframe where `keyframe` says, its inner edges filtered
    /// where `inner` says.
    fn new(level: i32, sharpness: u32, keyframe: bool, inner: bool) -> Self {
        let mut interior = level;
        if sharpness > 0 {
            interior >>= if sharpness > 4 { 2 } else { 1 };
            interior = interior.min(9 - i32::try_from(sharpness).unwrap_or(0));
        }
        let interior = interior.max(1);
        // An inter frame counts fewer edges as sharp features, which the
        // normal filter leaves to its narrowest adjustment.
        let hev = if keyframe {
            i32::from(level >= 40) + i32::from(level >= 15)
        } else {
            i32::from(level >= 40) + i32::from(level >= 20) + i32::from(level >= 15)
        };
        Self {
            edge: 2 * (level + 2) + interior,
            inside: 2 * level + interior,
            interior,
            hev,
            inner,
        }
    }
}

/// Where a macroblock's loop-filter level reads a stream's deltas: its
/// reference delta's index — its own frame, the last, the golden or the
/// alternate — and its mode delta's, where it takes one: subblocks, no
/// motion, one vector or split motion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct DeltaIndex {
    reference: usize,
    mode: Option<usize>,
}

impl DeltaIndex {
    /// An intra macroblock's, luma mode `ymode`: only one predicted a
    /// subblock at a time takes a mode delta.
    fn intra(ymode: u8) -> Self {
        Self {
            reference: 0,
            mode: (ymode == B_PRED).then_some(0),
        }
    }
}

/// The loop-filter level of a macroblock of `segment` whose deltas `deltas`
/// indexes: the segment's level, held to range, then the stream's deltas,
/// held to range again.
fn filter_level(header: &Header, segment: u8, deltas: DeltaIndex) -> i32 {
    let mut level = header
        .segmentation
        .filter_level(header.filter_level, usize::from(segment));
    if header.deltas.enabled {
        level += header
            .deltas
            .reference
            .get(deltas.reference)
            .copied()
            .unwrap_or(0);
        if let Some(mode) = deltas.mode {
            level += header.deltas.mode.get(mode).copied().unwrap_or(0);
        }
    }
    level.clamp(0, MAX_FILTER_LEVEL)
}

/// What the loop filter does at one macroblock: its level, nought for
/// nothing, and whether its inner edges are filtered too.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Edges {
    level: u8,
    inner: bool,
}

impl Edges {
    fn new(level: i32, inner: bool) -> Self {
        Self {
            level: u8::try_from(level.clamp(0, MAX_FILTER_LEVEL)).unwrap_or(0),
            inner,
        }
    }
}

/// Filter one macroblock row of `planes`, whose luma starts at sample row
/// `luma_row` and chroma at `chroma_row`, each macroblock as its `edges` say.
/// `top` says whether a macroblock row lies above, whose bottom samples the
/// row's top edges reach into.
fn filter_row(
    planes: &mut Planes,
    header: &Header,
    edges: &[Edges],
    rows: (isize, isize),
    top: bool,
) {
    for (column, edge) in edges.iter().enumerate() {
        filter_macroblock(planes, header, *edge, (column, rows), top);
    }
}

/// Filter the macroblock in `column` of the macroblock row of `planes` whose
/// luma starts at sample row `luma_row` and chroma at `chroma_row`, as `edge`
/// says; `top` as [`filter_row`]'s. The filter is defined in raster order, so
/// a macroblock is filtered once its left neighbour is and, where `top`, once
/// the row above is to its right.
fn filter_macroblock(
    planes: &mut Planes,
    header: &Header,
    edge: Edges,
    (column, (luma_row, chroma_row)): (usize, (isize, isize)),
    top: bool,
) {
    if header.filter_level == 0 || edge.level == 0 {
        return;
    }
    let keyframe = header.inter.is_none();
    let level = i32::from(edge.level);
    let strength = Strength::new(level, header.sharpness, keyframe, edge.inner);
    let x0 = isize::try_from(column * 16).unwrap_or(0);
    let sides = (column > 0, top);
    if header.simple_filter {
        planes.filter_simple((x0, luma_row), sides, &strength);
    } else {
        planes.filter_normal((x0, luma_row), (x0 / 2, chroma_row), sides, &strength);
    }
}

/// Each segment's dequantisation factors.
fn segment_factors(header: &Header) -> [Dequant; SEGMENTS] {
    core::array::from_fn(|segment| {
        header
            .quant
            .factors(header.segmentation.quant_index(header.quant.index, segment))
    })
}

/// The eight samples straddling one edge, from four before it to four after,
/// `step` apart: the window of the plane one filter segment works in.
struct Straddle<'p> {
    samples: &'p mut [u8],
    step: usize,
}

impl<'p> Straddle<'p> {
    /// The segment of `plane` crossing the edge at (`x`, `y`), across a
    /// vertical edge where `vertical`; `None` where it would leave the plane,
    /// which no edge the filter reaches does.
    fn at(plane: &'p mut Plane, x: isize, y: isize, vertical: bool) -> Option<Self> {
        let step = if vertical { 1 } else { plane.stride };
        let first = plane.at(x, y).checked_sub(4 * step)?;
        let samples = plane.samples.get_mut(first..=first + 7 * step)?;
        Some(Self { samples, step })
    }

    /// The eight samples, converted to signed.
    fn values(&self) -> [i32; 8] {
        core::array::from_fn(|at| {
            let sample = self.samples.get(at * self.step).copied().unwrap_or(128);
            i32::from(sample) - 128
        })
    }

    /// Store signed sample `at` back into pixel range.
    fn set(&mut self, at: usize, value: i32) {
        if let Some(sample) = self.samples.get_mut(at * self.step) {
            *sample = clamp_u8(clamp_s8(value) + 128);
        }
    }
}

/// Whether an edge's four straddling samples are close enough to filter and
/// its interior differences small enough (RFC 6386 §15.3).
fn filterable(values: &[i32; 8], interior: i32, edge: i32) -> bool {
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *values;
    (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= edge
        && (p3 - p2).abs() <= interior
        && (p2 - p1).abs() <= interior
        && (p1 - p0).abs() <= interior
        && (q3 - q2).abs() <= interior
        && (q2 - q1).abs() <= interior
        && (q1 - q0).abs() <= interior
}

/// Whether the samples either side of the edge differ sharply enough that
/// the wider filters would blur a real feature.
fn high_variance(values: &[i32; 8], threshold: i32) -> bool {
    (values[2] - values[3]).abs() > threshold || (values[5] - values[4]).abs() > threshold
}

/// The adjustment both filters share, applied to the two edge samples.
fn common_adjust(segment: &mut Straddle<'_>, values: &[i32; 8], outer_taps: bool) -> i32 {
    let (p1, p0, q0, q1) = (values[2], values[3], values[4], values[5]);
    let base = clamp_s8(if outer_taps { clamp_s8(p1 - q1) } else { 0 } + 3 * (q0 - p0));
    let toward = clamp_s8(base + 3) >> 3;
    let away = clamp_s8(base + 4) >> 3;
    segment.set(4, q0 - away);
    segment.set(3, p0 + toward);
    away
}

/// The simple filter over one edge's segments.
fn simple_edge(plane: &mut Plane, x: isize, y: isize, span: isize, vertical: bool, limit: i32) {
    for along in 0..span {
        let (sx, sy) = if vertical {
            (x, y + along)
        } else {
            (x + along, y)
        };
        let Some(mut segment) = Straddle::at(plane, sx, sy, vertical) else {
            continue;
        };
        let values = segment.values();
        let (p1, p0, q0, q1) = (values[2], values[3], values[4], values[5]);
        if (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= limit {
            common_adjust(&mut segment, &values, true);
        }
    }
}

/// The normal filter over one subblock edge's segments.
fn subblock_edge(
    plane: &mut Plane,
    x: isize,
    y: isize,
    span: isize,
    vertical: bool,
    strength: &Strength,
) {
    for along in 0..span {
        let (sx, sy) = if vertical {
            (x, y + along)
        } else {
            (x + along, y)
        };
        let Some(mut segment) = Straddle::at(plane, sx, sy, vertical) else {
            continue;
        };
        let values = segment.values();
        if !filterable(&values, strength.interior, strength.inside) {
            continue;
        }
        let sharp = high_variance(&values, strength.hev);
        let adjust = (common_adjust(&mut segment, &values, sharp) + 1) >> 1;
        if !sharp {
            segment.set(5, values[5] - adjust);
            segment.set(2, values[2] + adjust);
        }
    }
}

/// The normal filter over one macroblock edge's segments, which reaches
/// three samples either side.
fn macroblock_edge(
    plane: &mut Plane,
    x: isize,
    y: isize,
    span: isize,
    vertical: bool,
    strength: &Strength,
) {
    for along in 0..span {
        let (sx, sy) = if vertical {
            (x, y + along)
        } else {
            (x + along, y)
        };
        let Some(mut segment) = Straddle::at(plane, sx, sy, vertical) else {
            continue;
        };
        let values = segment.values();
        if !filterable(&values, strength.interior, strength.edge) {
            continue;
        }
        if high_variance(&values, strength.hev) {
            common_adjust(&mut segment, &values, true);
            continue;
        }
        let (p2, p1, p0, q0, q1, q2) = (
            values[1], values[2], values[3], values[4], values[5], values[6],
        );
        let width = clamp_s8(clamp_s8(p1 - q1) + 3 * (q0 - p0));
        let near = clamp_s8((27 * width + 63) >> 7);
        segment.set(4, q0 - near);
        segment.set(3, p0 + near);
        let middle = clamp_s8((18 * width + 63) >> 7);
        segment.set(5, q1 - middle);
        segment.set(2, p1 + middle);
        let far = clamp_s8((9 * width + 63) >> 7);
        segment.set(6, q2 - far);
        segment.set(1, p2 + far);
    }
}

#[cfg(any(test, feature = "fixture"))]
#[path = "vp8_fixture.rs"]
pub mod fixture;

#[cfg(test)]
#[path = "vp8_tests.rs"]
mod tests;
